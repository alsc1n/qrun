//! 启动板数据: 分组, 项目和启动记录, 保存在 qrun.db.
//! 分组和项目都用稳定 ID 定位, 增删不会让别处持有的引用指错对象.
//! 兼容期 (临时, 旧版不再需要时删除): 分组和项目同时按 0.1.0 的结构写一份 library.json, 装回旧版数据还在.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui_kit::{Context, Task};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};

use super::brain::{Ledger, Weights};
use super::predict::{self, Launch, Model, Tuning};
use super::redact::redact;
use super::rolan;
use crate::params::PARAMS;
use crate::storage;

mod activity;
mod replay;
mod smart;
mod tags;
mod timeline;

pub use replay::report;
pub use smart::{Ranked, Situation, Via, Window};
pub use timeline::What;

/// 库结构版本; 破坏性变更时加一, 并在 migrate 里加上从旧版本迁移的语句.
const DB_VERSION: u32 = 14;
const LEGACY_FILE: &str = "library.json";
const LEGACY_VERSION: u32 = 1;

/// 启动记录永久保留: 预测靠它越用越准, 很久以前的记录在打分时按时间衰减, 不用删.
/// source 区分从 QRun 启动 (qrun) 和从系统记录得知的启动.
const SCHEMA_V1: &str = "
    CREATE TABLE groups (
        id INTEGER PRIMARY KEY,
        name TEXT NOT NULL,
        sort TEXT NOT NULL,
        position INTEGER NOT NULL
    );
    CREATE TABLE items (
        id INTEGER PRIMARY KEY,
        group_id INTEGER NOT NULL REFERENCES groups (id),
        name TEXT NOT NULL,
        target TEXT NOT NULL,
        args TEXT NOT NULL,
        admin INTEGER NOT NULL,
        position INTEGER NOT NULL
    );
    CREATE TABLE launches (
        id INTEGER PRIMARY KEY,
        item_id INTEGER REFERENCES items (id) ON DELETE CASCADE,
        target TEXT NOT NULL,
        at INTEGER NOT NULL,
        source TEXT NOT NULL
    );
    CREATE INDEX launches_at ON launches (at);
";

/// 2: 启动记录加上本地时间在一周里的分钟数 (预测按钟点和工作日/周末算); system_seen 记上次看到的系统最近记录,
/// 下次对比出新的打开.
const SCHEMA_V2: &str = "
    ALTER TABLE launches ADD COLUMN week_minute INTEGER NOT NULL DEFAULT 0;
    CREATE TABLE system_seen (
        target TEXT PRIMARY KEY,
        at INTEGER NOT NULL
    );
";

/// 3: 从对错里学. opens 记每次打开 QRun 时的情境, shown 记这次推荐页摆出了什么, 启动记录加上来自哪次打开,
/// 从哪打开 (smart / group / search) 和推荐页名次; skips 记被摆出来却没被点的目标; weights 是学到的特征权重.
const SCHEMA_V3: &str = "
    CREATE TABLE opens (
        id INTEGER PRIMARY KEY,
        at INTEGER NOT NULL,
        week_minute INTEGER NOT NULL,
        foreground TEXT,
        title TEXT,
        gap_minutes INTEGER,
        clipboard TEXT
    );
    CREATE TABLE shown (
        open_id INTEGER NOT NULL REFERENCES opens (id),
        rank INTEGER NOT NULL,
        target TEXT NOT NULL,
        score REAL NOT NULL,
        jev REAL,
        PRIMARY KEY (open_id, rank)
    );
    ALTER TABLE launches ADD COLUMN open_id INTEGER REFERENCES opens (id);
    ALTER TABLE launches ADD COLUMN via TEXT;
    ALTER TABLE launches ADD COLUMN rank INTEGER;
    CREATE TABLE skips (
        target TEXT NOT NULL,
        at INTEGER NOT NULL
    );
    CREATE TABLE weights (
        name TEXT PRIMARY KEY,
        value REAL NOT NULL
    );
";

/// 4: 学习改成 AdaGrad 加回拉, weights 加上各特征的梯度平方和; 之前按旧规则学偏的权重清掉, 从起步值重学.
/// QRun 自己目录下的文件 (库, 设置, 日志) 不再算系统来源的启动, 已记的在迁移时删掉 (见 migrate).
const SCHEMA_V4: &str = "
    DELETE FROM weights;
    ALTER TABLE weights ADD COLUMN grad_sq REAL NOT NULL DEFAULT 0;
";

/// 5: 从 Rolan 导入时带过来的点击次数, 和从 QRun 启动的次数相加, 只用于按点击次数排序和"最常用";
/// Rolan 没记时间, 不进启动记录, 不影响预测.
const SCHEMA_V5: &str = "
    ALTER TABLE items ADD COLUMN imported_clicks INTEGER NOT NULL DEFAULT 0;
";

/// 6: 活动时间线. timeline 是内存里最近一段时间线的副本 (每条一行 JSON, 定期整表覆盖, 重启读回);
/// opens 加上打开时开着的窗口和那时最近的时间线, shown 加上各候选的特征, 以后换特征时可回放.
const SCHEMA_V6: &str = "
    CREATE TABLE timeline (event TEXT NOT NULL);
    ALTER TABLE opens ADD COLUMN windows TEXT;
    ALTER TABLE opens ADD COLUMN recent TEXT;
    ALTER TABLE shown ADD COLUMN features TEXT;
";

/// 7: "出现某件事之后打开了什么"的统计 (activity::Cooccur): 标记, 目标, 衰减次数和记下的时间.
const SCHEMA_V7: &str = "
    CREATE TABLE cooccur (
        token TEXT NOT NULL,
        target TEXT NOT NULL,
        count REAL NOT NULL,
        at INTEGER NOT NULL,
        PRIMARY KEY (token, target)
    );
";

/// 8: 时间线的几项近况从"累加次数"改为"最近一次有多近" (0 到 1), 旧值动辄几百上千, 学到的这几项权重和
/// 步长都偏了, 删掉让它们从 params.json 的起步值重新学; 其余权重保留.
const SCHEMA_V8: &str = "
    DELETE FROM weights WHERE name IN ('modified', 'focused', 'started', 'nearby');
";

/// 11: 归档的活动时间线 (已脱敏), 内存里只留 2 小时, 这里留 retention_days 天, 回放用. event 是 What 的 JSON.
const SCHEMA_V11: &str = "
    CREATE TABLE history (start INTEGER NOT NULL, end INTEGER NOT NULL, event TEXT NOT NULL);
    CREATE INDEX history_end ON history (end);
";

/// 12: 推荐改为按标签记账 (`model::brain`), 账由回放从历史建出, 不存库; "出现某件事之后打开了什么"并入标签账,
/// 删掉 cooccur 表. 特征换了, 旧权重清掉.
const SCHEMA_V12: &str = "
    DROP TABLE cooccur;
    DELETE FROM weights;
";

/// 13: 每次打开时 Jev 的应答 (归类标签, 它认为最可能的目标), 回放时还原标签和考核 Jev 用.
const SCHEMA_V13: &str = "ALTER TABLE opens ADD COLUMN jev TEXT;";

/// 14: 标签账存库一直累积 (之前只在内存里, 每次启动由回放重建, 记录过了保留天数经验就丢了).
/// 表是空的时 (就是这次升级) 启动后由回放从历史建一次. 以后改了算法旧账不能用时, 迁移里清空这张表即可重建.
const SCHEMA_V14: &str = "
    CREATE TABLE ledger (
        book TEXT NOT NULL,
        tag TEXT NOT NULL,
        target TEXT NOT NULL,
        count REAL NOT NULL,
        at INTEGER NOT NULL,
        PRIMARY KEY (book, tag, target)
    ) WITHOUT ROWID;
";

const INSERT_LAUNCH: &str = "INSERT INTO launches (item_id, target, at, source, week_minute, open_id, via, rank)
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)";

pub type Id = u64;

pub struct Library {
    groups: Vec<Group>,
    /// 下一个可用 ID, 加载时按已有最大 ID 推算.
    next_id: Id,
    /// 各项目从 QRun 启动的次数, 按点击次数排序的分组和"最常用"用.
    clicks: HashMap<Id, u32>,
    /// 全部启动记录 (含系统来源), 按时间排好, 预测从这里学.
    launches: Vec<Launch>,
    model: Model,
    /// 标签账, 从库里读出, 每次打开实时加并存库.
    ledger: Ledger,
    weights: Weights,
    /// 上次读到的系统最近记录和新文件, 推荐页的候选.
    system: Vec<Observed>,
    /// 当前这次打开 QRun 的情境和排好的候选; 启动时据此学对错.
    open: Option<smart::Open>,
    /// 上一次打开; 呼出后从别处打开的东西常在下次呼出时才从系统最近记录读到, 那时要记到这一次上.
    previous_open: Option<smart::Open>,
    /// 最近一次调用 Jev 的时间和结果 (失败带原因), 设置页显示; 这次运行还没调过为 None.
    jev_last: Option<(String, Result<(), String>)>,
    /// 最近一段活动时间线.
    timeline: timeline::Timeline,
    db: Connection,
    /// 定时调程序本身统计的参数 (库里还没有账时先回放建账) 的后台任务.
    tuning: Task<()>,
}

/// 系统最近记录里的一条: 打开过的目标和最后一次打开的时间 (Unix 毫秒).
/// kind 是 file / folder / program, 或 new file (下载文件夹和桌面上的新文件, 还没打开过, 只当候选).
pub struct Observed {
    pub name: String,
    pub target: PathBuf,
    pub at: i64,
    pub kind: &'static str,
}

pub const NEW_FILE: &str = "new file";

/// 一次导入的结果: 加了几个项目, 新建了几个分组, 跳过了几个已有的.
#[derive(Default)]
pub struct Imported {
    pub items: usize,
    pub groups: usize,
    pub skipped: usize,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Group {
    pub id: Id,
    pub name: String,
    pub items: Vec<Item>,
    /// 只存在库里; library.json 保持 0.1.0 的结构, 旧版才读得了.
    #[serde(skip)]
    pub sort: GroupSort,
}

#[derive(Clone, Copy, PartialEq, Default)]
pub enum GroupSort {
    /// 按拖拽排好的顺序.
    #[default]
    Manual,
    /// 按从 QRun 启动的次数, 次数相同的保持手动顺序.
    Clicks,
}

impl GroupSort {
    fn as_db(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Clicks => "clicks",
        }
    }

    fn from_db(value: &str) -> Self {
        match value {
            "manual" => Self::Manual,
            "clicks" => Self::Clicks,
            other => unreachable!("qrun.db 里有未知的排序方式 {other}"),
        }
    }
}

/// library.json 的结构, 与 0.1.0 一致.
#[derive(Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct LegacyLibrary {
    groups: Vec<Group>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Item {
    pub id: Id,
    pub name: String,
    pub target: PathBuf,
    /// 启动参数, 大多数项目不填.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub args: String,
    /// 始终以管理员权限启动.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub admin: bool,
}

/// 编辑对话框提交的内容.
pub struct ItemEdit {
    pub name: String,
    pub target: PathBuf,
    pub args: String,
    pub admin: bool,
}

impl Library {
    pub fn load() -> Result<Self, String> {
        let db = storage::open_db(DB_VERSION, migrate)?;
        let fail = |e: rusqlite::Error| format!("读取 {} 失败: {e}", storage::path(storage::DB_FILE).display());
        let saved = read_groups(&db).map_err(fail)?;
        // 兼容期: 两份不一致说明装回旧版后在旧版里改过, 以 library.json 为准, 排序方式按分组 ID 沿用库里的.
        // 从 0.1.0 升级上来时库是空的, 同样走这里导入. 没有 library.json 时以库为准, 随后补写一份
        let legacy_exists = storage::path(LEGACY_FILE).exists();
        let legacy = if legacy_exists {
            // library.json 只有版本 1, 没有要迁移的旧结构
            let legacy: LegacyLibrary = storage::load(LEGACY_FILE, LEGACY_VERSION, |from, _| {
                unreachable!("library.json 没有版本 {from} 的迁移")
            })?;
            let same = serde_json::to_value(&legacy.groups).expect("serialize groups")
                == serde_json::to_value(&saved).expect("serialize groups");
            (!same).then_some(legacy)
        } else {
            None
        };
        let imported = legacy.is_some();
        let groups = match legacy {
            None => saved,
            Some(legacy) => {
                let sorts: HashMap<Id, GroupSort> = saved.iter().map(|g| (g.id, g.sort)).collect();
                let mut groups = legacy.groups;
                for group in &mut groups {
                    group.sort = sorts.get(&group.id).copied().unwrap_or_default();
                }
                groups
            }
        };
        let clicks = read_clicks(&db).map_err(fail)?;
        let launches = read_launches(&db).map_err(fail)?;
        let weights = smart::read_weights(&db).map_err(fail)?;
        let ledger = smart::read_ledger(&db).map_err(fail)?;
        let timeline = timeline::Timeline::read(&db).map_err(fail)?;
        let max_id = groups.iter().flat_map(|g| std::iter::once(g.id).chain(g.items.iter().map(|i| i.id))).max();
        let mut library = Self {
            groups,
            next_id: max_id.map_or(1, |id| id + 1),
            clicks,
            model: Model::build(&launches, Tuning::initial()),
            launches,
            ledger,
            weights,
            system: Vec::new(),
            open: None,
            previous_open: None,
            jev_last: None,
            timeline,
            db,
            tuning: Task::ready(()),
        };
        // 首次运行给一个默认分组
        let first_run = library.groups.is_empty();
        if first_run {
            let id = library.allocate_id();
            library.groups.push(Group { id, name: "常用".into(), items: Vec::new(), sort: GroupSort::Manual });
        }
        if first_run || imported || !legacy_exists {
            library.save();
        }
        Ok(library)
    }

    pub fn groups(&self) -> &[Group] {
        &self.groups
    }

    pub fn group(&self, id: Id) -> Option<&Group> {
        self.groups.iter().find(|g| g.id == id)
    }

    /// 分组里的项目, 按分组的排序方式排好.
    pub fn sorted_items(&self, group: Id) -> Vec<Item> {
        let Some(group) = self.group(group) else {
            return Vec::new();
        };
        let mut items = group.items.clone();
        if group.sort == GroupSort::Clicks {
            // 稳定排序, 次数相同的保持手动顺序; 没启动过的算 0 次
            items.sort_by_key(|i| Reverse(self.clicks.get(&i.id).copied().unwrap_or(0)));
        }
        items
    }

    pub fn item(&self, id: Id) -> Option<&Item> {
        self.groups.iter().flat_map(|g| &g.items).find(|i| i.id == id)
    }

    /// 从 QRun 启动次数最多的 n 个项目, 没启动过的不算.
    pub fn most_clicked(&self, n: usize) -> Vec<Item> {
        let mut items: Vec<(&Item, u32)> = self
            .groups
            .iter()
            .flat_map(|g| &g.items)
            .filter_map(|i| self.clicks.get(&i.id).map(|&clicks| (i, clicks)))
            .collect();
        items.sort_by_key(|&(_, clicks)| Reverse(clicks));
        items.into_iter().take(n).map(|(i, _)| i.clone()).collect()
    }

    /// 启动时和之后每隔 replay_interval_hours 在后台调程序本身统计的参数 (半衰期, 钟点宽度; 从永久保留的启动记录算).
    /// 库里还没有标签账时 (升级上来, 或迁移清掉了), 这一轮先用回放从历史建账和权重, 存进库; 之后账一直累积, 不再重建.
    /// 建账期间点的不会记进账 (建好的账整个换上), 只有升级后第一次启动的几秒.
    pub fn start_tuning(&mut self, cx: &mut Context<Self>) {
        let interval = Duration::from_secs(PARAMS.predict.replay_interval_hours * 3600);
        self.tuning = cx.spawn(async move |this, cx| {
            loop {
                let Ok((launches, items)) = this.read_with(cx, |lib, _| {
                    let build = lib.ledger.is_empty().then(|| replay::items(lib));
                    (lib.launches.clone(), build)
                }) else {
                    return;
                };
                let tuned = cx
                    .background_executor()
                    .spawn(async move {
                        let tuning = predict::tune(&launches, now_ms());
                        let built = items
                            .map(|items| storage::open_db_read_only().and_then(|db| replay::run(&db, &items, tuning)));
                        (tuning, built)
                    })
                    .await;
                let updated = this.update(cx, |lib, _| {
                    let (tuning, built) = tuned;
                    lib.model = Model::build(&lib.launches, tuning);
                    match built {
                        Some(Ok((ledger, weights, report))) => {
                            log::info!("从历史建账: {report}");
                            lib.ledger = ledger;
                            lib.weights = weights;
                            lib.save_ledger();
                            if let Err(e) = smart::write_weights(&lib.db, &lib.weights) {
                                storage::db_save_failed(&e);
                            }
                        }
                        Some(Err(e)) => log::warn!("从历史建账失败: {e}"),
                        None => {}
                    }
                });
                if updated.is_err() {
                    return;
                }
                cx.background_executor().timer(interval).await;
            }
        });
    }

    pub fn add_group(&mut self, name: &str, cx: &mut Context<Self>) -> Id {
        let id = self.allocate_id();
        self.groups.push(Group { id, name: name.into(), items: Vec::new(), sort: GroupSort::Manual });
        self.changed(cx);
        id
    }

    pub fn set_group_sort(&mut self, id: Id, sort: GroupSort, cx: &mut Context<Self>) {
        if let Some(group) = self.groups.iter_mut().find(|g| g.id == id) {
            group.sort = sort;
            self.changed(cx);
        }
    }

    pub fn rename_group(&mut self, id: Id, name: &str, cx: &mut Context<Self>) {
        if let Some(group) = self.groups.iter_mut().find(|g| g.id == id) {
            group.name = name.into();
            self.changed(cx);
        }
    }

    /// 至少保留一个分组.
    pub fn remove_group(&mut self, id: Id, cx: &mut Context<Self>) {
        if self.groups.len() > 1 {
            if let Some(group) = self.groups.iter().find(|g| g.id == id) {
                for item in &group.items {
                    self.clicks.remove(&item.id);
                }
            }
            self.groups.retain(|g| g.id != id);
            // 写库时级联删掉了这些项目的启动记录, 预测也跟着去掉
            self.save();
            self.reload_launches();
            cx.notify();
        }
    }

    /// 拖进来的文件或文件夹加到分组末尾, 名称取文件名 (文件去掉扩展名). 去重见 `add_items`.
    pub fn add_paths(&mut self, group: Id, paths: &[PathBuf], by_name: bool, cx: &mut Context<Self>) {
        let new = paths.iter().map(|path| (display_name(path), path.clone(), String::new())).collect();
        self.add_items(group, new, by_name, cx);
    }

    /// 快捷方式解析出的程序加到分组末尾, 名称取快捷方式名, 参数沿用快捷方式里的. 去重见 `add_items`.
    pub fn add_link_target(
        &mut self,
        group: Id,
        link: &Path,
        target: PathBuf,
        args: String,
        by_name: bool,
        cx: &mut Context<Self>,
    ) {
        self.add_items(group, vec![(display_name(link), target, args)], by_name, cx);
    }

    /// 从"添加应用"列表选中的应用加到分组末尾. 去重见 `add_items`.
    pub fn add_app(&mut self, group: Id, name: String, target: PathBuf, by_name: bool, cx: &mut Context<Self>) {
        self.add_items(group, vec![(name, target, String::new())], by_name, cx);
    }

    /// 把 (名称, 目标, 参数) 加到分组末尾. 这个分组里已有同一目标的不加; by_name (设置"按名称去重") 时
    /// 已有同名的也不加, 否则同名照加, 名称不改, 由用户自己改. 都不区分大小写; 一次加的几项之间同样去重.
    fn add_items(&mut self, group: Id, new: Vec<(String, PathBuf, String)>, by_name: bool, cx: &mut Context<Self>) {
        let Some(ix) = self.groups.iter().position(|g| g.id == group) else {
            return;
        };
        let mut added = false;
        for (name, target, args) in new {
            let (key, lower_name) = (predict::key(&target), name.to_lowercase());
            let duplicate = self.groups[ix]
                .items
                .iter()
                .any(|i| predict::key(&i.target) == key || by_name && i.name.to_lowercase() == lower_name);
            if duplicate {
                continue;
            }
            let id = self.allocate_id();
            self.groups[ix].items.push(Item { id, name, target, args, admin: false });
            added = true;
        }
        if added {
            self.changed(cx);
        }
    }

    /// 把分组挪到 before 前面, before 为 None 时挪到最后.
    pub fn move_group(&mut self, id: Id, before: Option<Id>, cx: &mut Context<Self>) {
        if before == Some(id) {
            return;
        }
        let Some(from) = self.groups.iter().position(|g| g.id == id) else {
            return;
        };
        let group = self.groups.remove(from);
        let to = before.and_then(|b| self.groups.iter().position(|g| g.id == b)).unwrap_or(self.groups.len());
        self.groups.insert(to, group);
        self.changed(cx);
    }

    /// 把项目挪到 group 分组里 before 前面, before 为 None 时挪到末尾; 同组即排序, 跨组即移动.
    pub fn move_item(&mut self, id: Id, group: Id, before: Option<Id>, cx: &mut Context<Self>) {
        if before == Some(id) {
            return;
        }
        let Some(item) = self.groups.iter_mut().find_map(|g| {
            let ix = g.items.iter().position(|i| i.id == id)?;
            Some(g.items.remove(ix))
        }) else {
            return;
        };
        let items = &mut self.groups.iter_mut().find(|g| g.id == group).expect("target group exists").items;
        let to = before.and_then(|b| items.iter().position(|i| i.id == b)).unwrap_or(items.len());
        items.insert(to, item);
        self.changed(cx);
    }

    pub fn update_item(&mut self, id: Id, edit: ItemEdit, cx: &mut Context<Self>) {
        if let Some(item) = self.groups.iter_mut().flat_map(|g| &mut g.items).find(|i| i.id == id) {
            item.name = edit.name;
            item.target = edit.target;
            item.args = edit.args;
            item.admin = edit.admin;
            self.changed(cx);
        }
    }

    pub fn remove_item(&mut self, id: Id, cx: &mut Context<Self>) {
        for group in &mut self.groups {
            group.items.retain(|i| i.id != id);
        }
        self.clicks.remove(&id);
        // 写库时级联删掉了它的启动记录, 预测也跟着去掉
        self.save();
        self.reload_launches();
        cx.notify();
    }

    /// 从 QRun 启动了目标: 记一条启动记录, 并按这次打开时摆出的候选学一次对错.
    /// item 是对应的项目, 从推荐页打开的系统最近项没有项目, 不计点击次数.
    pub fn record_launch(&mut self, item: Option<Id>, target: &Path, via: Via, cx: &mut Context<Self>) {
        let at = now_ms();
        let launch = Launch { key: predict::key(target), at, week_minute: week_minute(&self.db, at) };
        let result = self.db.execute(
            INSERT_LAUNCH,
            params![
                item,
                target.to_string_lossy(),
                at,
                "qrun",
                launch.week_minute,
                self.open_id(),
                via.as_db(),
                via.rank()
            ],
        );
        if let Err(e) = result {
            storage::db_save_failed(&e);
        }
        self.learn(&launch.key, target, via, at);
        self.record(What::Launch { target: target.to_string_lossy().into_owned() });
        self.model.add(&launch);
        self.launches.push(launch);
        if let Some(id) = item {
            *self.clicks.entry(id).or_default() += 1;
        }
        cx.notify();
    }

    /// 系统最近记录里比上次看到的更新的打开, 各记一条系统来源的启动 (只用于预测, 不计点击次数).
    /// 第一次看到的按它最后打开的时间记一条, 刚用就有东西可学. 和 QRun 自己记的同一次启动 (从 QRun 打开文件,
    /// 资源管理器也会记一笔) 相差不到 dedupe_seconds 的不重复记. 新文件还没打开过, 只留作推荐页的候选.
    /// QRun 自己目录下的文件既不记也不当候选.
    pub fn sync_system(&mut self, mut observed: Vec<Observed>, cx: &mut Context<Self>) {
        let dirs = own_dirs();
        observed.retain(|entry| !is_own(&predict::key(&entry.target), &dirs));
        match self.write_observed(&observed) {
            Ok(added) if !added.is_empty() => {
                // 系统记录的时间可能早于已有记录, 不能接着往后加, 重新读一遍按时间排好
                self.reload_launches();
                for (key, at) in added {
                    self.learn_outside(&key, at);
                }
            }
            Ok(_) => {}
            Err(e) => storage::db_save_failed(&e),
        }
        self.system = observed;
        cx.notify();
    }

    /// 换上刚扫到的新文件 (其余系统记录不动), 返回有没有之前没见过的.
    pub fn sync_new_files(&mut self, mut found: Vec<Observed>) -> bool {
        let dirs = own_dirs();
        found.retain(|entry| !is_own(&predict::key(&entry.target), &dirs));
        let known: HashSet<String> =
            self.system.iter().filter(|e| e.kind == NEW_FILE).map(|e| predict::key(&e.target)).collect();
        let appeared = found.iter().any(|e| !known.contains(&predict::key(&e.target)));
        self.system.retain(|e| e.kind != NEW_FILE);
        self.system.extend(found);
        appeared
    }

    /// 返回新记的启动 (比较键, 时间). 打开文件时系统会把它所在的文件夹也记一笔, 和里面某个文件相差不到
    /// folder_merge_seconds 的文件夹不算一次启动 (仍在最近文件夹里显示, 也仍是候选).
    fn write_observed(&self, observed: &[Observed]) -> rusqlite::Result<Vec<(String, i64)>> {
        let dedupe = PARAMS.predict.dedupe_seconds * 1000;
        let merge = PARAMS.predict.folder_merge_seconds * 1000;
        let files: Vec<(String, i64)> = observed
            .iter()
            .filter(|e| e.kind == "file")
            .filter_map(|e| Some((predict::key(e.target.parent()?), e.at)))
            .collect();
        let with_file = |e: &Observed| {
            files.iter().any(|(folder, at)| *folder == predict::key(&e.target) && (at - e.at).abs() <= merge)
        };
        let tx = self.db.unchecked_transaction()?;
        let mut added = Vec::new();
        {
            let mut last_seen = tx.prepare("SELECT at FROM system_seen WHERE target = ?1")?;
            let mut seen = tx.prepare(
                "INSERT INTO system_seen (target, at) VALUES (?1, ?2) ON CONFLICT (target) DO UPDATE SET at = excluded.at",
            )?;
            let mut duplicate = tx.prepare(
                "SELECT EXISTS (SELECT 1 FROM launches
                 WHERE source = 'qrun' AND target = ?1 COLLATE NOCASE AND abs(at - ?2) <= ?3)",
            )?;
            let mut insert = tx.prepare(INSERT_LAUNCH)?;
            for entry in observed.iter().filter(|entry| entry.kind != NEW_FILE) {
                let key = predict::key(&entry.target);
                let last: Option<i64> = last_seen.query_row([&key], |row| row.get(0)).optional()?;
                if last.is_some_and(|last| last >= entry.at) {
                    continue;
                }
                seen.execute(params![key, entry.at])?;
                let target = entry.target.to_string_lossy();
                let folder_of_file = entry.kind == "folder" && with_file(entry);
                if !folder_of_file && !duplicate.query_row(params![target, entry.at, dedupe], |row| row.get(0))? {
                    insert.execute(params![
                        None::<Id>,
                        target,
                        entry.at,
                        "system",
                        week_minute(&tx, entry.at),
                        None::<i64>,
                        None::<&str>,
                        None::<i64>
                    ])?;
                    added.push((key, entry.at));
                }
            }
        }
        tx.commit()?;
        Ok(added)
    }

    /// 按库里的启动记录重建预测统计, 调参结果沿用.
    fn reload_launches(&mut self) {
        match read_launches(&self.db) {
            Ok(launches) => {
                self.model = Model::build(&launches, self.model.tuning());
                self.launches = launches;
            }
            Err(e) => storage::db_save_failed(&e),
        }
    }

    fn allocate_id(&mut self) -> Id {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// 导入 Rolan 的分组和项目: 同名分组并进去, 没有的新建; 任何分组里已有同一目标的不再加, 重复导入不会出重复.
    /// Rolan 里的点击次数记成导入的点击次数.
    pub fn import_rolan(&mut self, groups: Vec<rolan::Group>, cx: &mut Context<Self>) -> Imported {
        let mut imported = Imported::default();
        let mut targets: HashSet<String> =
            self.groups.iter().flat_map(|g| &g.items).map(|i| predict::key(&i.target)).collect();
        let mut clicks: Vec<(Id, u32)> = Vec::new();
        for source in groups {
            let mut items = Vec::new();
            for item in source.items {
                if !targets.insert(predict::key(&item.target_path)) {
                    imported.skipped += 1;
                    continue;
                }
                let id = self.allocate_id();
                if item.count > 0 {
                    clicks.push((id, item.count));
                }
                items.push(Item {
                    id,
                    name: item.name,
                    target: item.target_path,
                    args: item.arguments,
                    admin: item.admin,
                });
            }
            if items.is_empty() {
                continue;
            }
            imported.items += items.len();
            match self.groups.iter_mut().find(|g| g.name == source.title) {
                Some(group) => group.items.extend(items),
                None => {
                    let id = self.allocate_id();
                    self.groups.push(Group { id, name: source.title, items, sort: GroupSort::Manual });
                    imported.groups += 1;
                }
            }
        }
        self.save();
        if let Err(e) = self.write_imported_clicks(&clicks) {
            storage::db_save_failed(&e);
        }
        for (id, count) in clicks {
            *self.clicks.entry(id).or_default() += count;
        }
        cx.notify();
        imported
    }

    fn write_imported_clicks(&self, clicks: &[(Id, u32)]) -> rusqlite::Result<()> {
        let tx = self.db.unchecked_transaction()?;
        {
            let mut update = tx.prepare("UPDATE items SET imported_clicks = ?2 WHERE id = ?1")?;
            for (id, count) in clicks {
                update.execute(params![id, count])?;
            }
        }
        tx.commit()
    }

    fn save(&self) {
        if let Err(e) = self.write_db() {
            storage::db_save_failed(&e);
        }
        // 兼容期: 0.1.0 只认 library.json
        storage::save(LEGACY_FILE, LEGACY_VERSION, &serde_json::json!({ "groups": self.groups }));
    }

    /// 把库里的分组和项目改成与内存一致: 有的更新, 新的插入, 没了的删掉 (项目的启动记录随之删除).
    /// 不整表清空重写, 否则级联会把所有启动记录一起删掉.
    fn write_db(&self) -> rusqlite::Result<()> {
        let tx = self.db.unchecked_transaction()?;
        {
            let mut upsert_group = tx.prepare(
                "INSERT INTO groups (id, name, sort, position) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (id) DO UPDATE SET name = excluded.name, sort = excluded.sort, position = excluded.position",
            )?;
            let mut upsert_item = tx.prepare(
                "INSERT INTO items (id, group_id, name, target, args, admin, position) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT (id) DO UPDATE SET group_id = excluded.group_id, name = excluded.name, target = excluded.target,
                     args = excluded.args, admin = excluded.admin, position = excluded.position",
            )?;
            for (group_pos, group) in self.groups.iter().enumerate() {
                upsert_group.execute(params![group.id, group.name, group.sort.as_db(), group_pos])?;
                for (item_pos, item) in group.items.iter().enumerate() {
                    upsert_item.execute(params![
                        item.id,
                        group.id,
                        item.name,
                        item.target.to_string_lossy(),
                        item.args,
                        item.admin,
                        item_pos
                    ])?;
                }
            }
        }
        // 先删项目再删分组, 被删分组里的项目已经不在内存里, 外键不会挡住
        let group_ids: Vec<Id> = self.groups.iter().map(|g| g.id).collect();
        let item_ids: Vec<Id> = self.groups.iter().flat_map(|g| &g.items).map(|i| i.id).collect();
        let ids = |ids: &[Id]| serde_json::to_string(ids).expect("serialize ids");
        tx.execute("DELETE FROM items WHERE id NOT IN (SELECT value FROM json_each(?1))", [ids(&item_ids)])?;
        tx.execute("DELETE FROM groups WHERE id NOT IN (SELECT value FROM json_each(?1))", [ids(&group_ids)])?;
        tx.commit()
    }

    fn changed(&self, cx: &mut Context<Self>) {
        self.save();
        cx.notify();
    }
}

fn migrate(from: u32, db: &Transaction) -> rusqlite::Result<()> {
    match from {
        0 => db.execute_batch(SCHEMA_V1),
        1 => {
            db.execute_batch(SCHEMA_V2)?;
            db.execute(&format!("UPDATE launches SET week_minute = {}", week_minute_sql("at")), [])?;
            Ok(())
        }
        2 => db.execute_batch(SCHEMA_V3),
        3 => {
            db.execute_batch(SCHEMA_V4)?;
            for dir in own_dirs() {
                db.execute(
                    "DELETE FROM launches WHERE source = 'system' AND lower(substr(target || '\\', 1, length(?1))) = ?1",
                    [&dir],
                )?;
            }
            Ok(())
        }
        4 => db.execute_batch(SCHEMA_V5),
        5 => db.execute_batch(SCHEMA_V6),
        6 => db.execute_batch(SCHEMA_V7),
        7 => db.execute_batch(SCHEMA_V8),
        // 10: 9 的规则漏了厂商前缀加十六进制的密钥 (sk_ 加 32 位), 按改好的规则再脱敏一遍, 含剪贴板文字
        8 | 9 => redact_saved(db),
        10 => db.execute_batch(SCHEMA_V11),
        11 => db.execute_batch(SCHEMA_V12),
        12 => db.execute_batch(SCHEMA_V13),
        13 => db.execute_batch(SCHEMA_V14),
        _ => unreachable!("qrun.db 没有版本 {from} 的迁移"),
    }
}

/// 9: 窗口标题改为脱敏后再记, 已存下的打开时的标题, 开着的窗口, 最近活动, 时间线也脱敏重写.
fn redact_saved(db: &Transaction) -> rusqlite::Result<()> {
    type Row = (i64, Option<String>, Option<String>, Option<String>);
    let opens: Vec<Row> = db
        .prepare("SELECT id, title, windows, recent FROM opens")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (id, title, windows, recent) in opens {
        db.execute(
            "UPDATE opens SET title = ?2, windows = ?3, recent = ?4 WHERE id = ?1",
            params![
                id,
                title.map(|t| redact(&t).text),
                windows.map(|json| redact_titles(&json)),
                recent.map(|json| redact_titles(&json))
            ],
        )?;
    }
    let events: Vec<(i64, String)> = db
        .prepare("SELECT rowid, event FROM timeline")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (rowid, event) in events {
        db.execute("UPDATE timeline SET event = ?2 WHERE rowid = ?1", params![rowid, redact_titles(&event)])?;
    }
    Ok(())
}

/// 把 JSON 里所有 title 字段 (开着的窗口, 前台窗口事件) 和剪贴板事件的 text 字段脱敏; text 大半是密钥时整个去掉,
/// 与新记的一致.
fn redact_titles(json: &str) -> String {
    fn walk(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Object(map) => {
                if let Some(serde_json::Value::String(text)) = map.get("text") {
                    let redacted = redact(text);
                    if redacted.mostly_secret {
                        map.remove("text");
                    } else {
                        map.insert("text".into(), redacted.text.into());
                    }
                }
                for (key, value) in map.iter_mut() {
                    match value {
                        serde_json::Value::String(text) if key == "title" => *text = redact(text).text,
                        _ => walk(value),
                    }
                }
            }
            serde_json::Value::Array(items) => items.iter_mut().for_each(walk),
            _ => {}
        }
    }
    let mut value: serde_json::Value = serde_json::from_str(json).expect("qrun.db 里存的是 JSON");
    walk(&mut value);
    value.to_string()
}

/// 本地时间在一周里的分钟数 (周一 0 点为 0) 的 SQL 表达式, at 是 Unix 毫秒.
/// 用 SQLite 的 localtime 按系统时区换算, model 不必去调 Win32 取时区.
fn week_minute_sql(at: &str) -> String {
    let part = |format: &str| format!("CAST(strftime('{format}', {at} / 1000, 'unixepoch', 'localtime') AS INTEGER)");
    // %w 周日为 0, 换成周一为 0
    format!("(({} + 6) % 7) * 1440 + {} * 60 + {}", part("%w"), part("%H"), part("%M"))
}

fn week_minute(db: &Connection, at: i64) -> u16 {
    db.query_row(&format!("SELECT {}", week_minute_sql("?1")), [at], |row| row.get(0)).expect("SQLite 换算本地时间")
}

/// QRun 自己的目录 (数据目录和 exe 所在目录, 绿色版是同一个) 的比较键, 带结尾分隔符;
/// 这些目录和里面的文件 (库, 日志, qrun.exe) 是 QRun 自己的, 不算用户打开或改动的东西.
fn own_dirs() -> Vec<String> {
    let exe = std::env::current_exe().expect("locate exe");
    let mut dirs: Vec<String> = [storage::dir(), exe.parent().expect("exe dir")]
        .into_iter()
        .map(|dir| predict::key(dir).trim_end_matches('\\').to_string() + "\\")
        .collect();
    dirs.dedup();
    dirs
}

/// 比较键 key 是不是 QRun 自己的目录或其中的文件.
fn is_own(key: &str, dirs: &[String]) -> bool {
    let key = format!("{key}\\");
    dirs.iter().any(|dir| key.starts_with(dir))
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).expect("system clock after 1970").as_millis() as i64
}

/// 启动记录按时间排好.
fn read_launches(db: &Connection) -> rusqlite::Result<Vec<Launch>> {
    db.prepare("SELECT target, at, week_minute FROM launches ORDER BY at, id")?
        .query_map([], |row| {
            Ok(Launch {
                key: predict::key(Path::new(&row.get::<_, String>(0)?)),
                at: row.get(1)?,
                week_minute: row.get(2)?,
            })
        })?
        .collect()
}

/// 按位置读出分组和各自的项目.
fn read_groups(db: &Connection) -> rusqlite::Result<Vec<Group>> {
    let mut groups = db
        .prepare("SELECT id, name, sort FROM groups ORDER BY position")?
        .query_map([], |row| {
            Ok(Group {
                id: row.get(0)?,
                name: row.get(1)?,
                items: Vec::new(),
                sort: GroupSort::from_db(&row.get::<_, String>(2)?),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut stmt = db.prepare("SELECT group_id, id, name, target, args, admin FROM items ORDER BY position")?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let group: Id = row.get(0)?;
        let item = Item {
            id: row.get(1)?,
            name: row.get(2)?,
            target: PathBuf::from(row.get::<_, String>(3)?),
            args: row.get(4)?,
            admin: row.get(5)?,
        };
        groups.iter_mut().find(|g| g.id == group).expect("外键保证项目的分组存在").items.push(item);
    }
    Ok(groups)
}

/// 各项目的点击次数: 从 QRun 启动的次数加上导入时带过来的; 都为 0 的不列.
fn read_clicks(db: &Connection) -> rusqlite::Result<HashMap<Id, u32>> {
    db.prepare(
        "SELECT id, clicks FROM (
             SELECT i.id, i.imported_clicks
                 + (SELECT COUNT(*) FROM launches l WHERE l.source = 'qrun' AND l.item_id = i.id) AS clicks
             FROM items i)
         WHERE clicks > 0",
    )?
    .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
    .collect()
}

fn display_name(path: &Path) -> String {
    if path.is_dir() { path.file_name() } else { path.file_stem() }
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}
