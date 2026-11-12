//! 推荐页的候选和学习: 每次打开 QRun 记下情境, 拆成标签 (`tags`), 给分组里的项目, 系统最近记录, 最近碰过的目标
//! 按 `model::brain` 打分排好; 从 QRun 启动时按这次摆出的候选学一次对错, 并给标签账记一笔; 呼出后从别处打开的也记账.
//! 给 Jev 拼请求 (附上本地账里的习惯); Jev 的归类标签并进这次的情境, 用户打开了什么就按它在 Jev 排序里的名次考核 Jev.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use gpui_kit::Context;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::activity::{self, Recent};
use super::tags::{self, StateInput};
use super::{Item, Library, NEW_FILE, now_ms, week_minute};
use crate::model::brain::{self, Candidate, Entry, FEATURES, Features, Ledger, SKIPPED, Sample, Snapshot, Weights};
use crate::model::jev::{self, Answer};
use crate::model::predict::{self, Model};
use crate::params::PARAMS;
use crate::storage;

const MINUTE: i64 = 60_000;
const HOUR: i64 = 3_600_000;
const DAY: i64 = 86_400_000;
const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
/// Jev 归类得到的情境标签的前缀.
const JEV_TAG: &str = "jev:";

/// 一次打开时 Jev 的应答, 存在 opens.jev 里, 回放用: 归类标签, 候选按 Jev 给的概率从高到低 (考核用).
#[derive(Serialize, Deserialize)]
pub(super) struct JevRecord {
    pub(super) tags: Vec<String>,
    pub(super) ranking: Vec<String>,
}

/// 打开 QRun 那一刻的情境, 由界面从系统读好传进来; 拿不到的项为 None 或空. 标题已脱敏.
#[derive(Clone, Default)]
pub struct Situation {
    /// 打开 QRun 前的前台程序.
    pub foreground: Option<PathBuf>,
    pub title: Option<String>,
    pub clipboard: Option<String>,
    /// 开着的窗口, 按 Z 序从上到下.
    pub windows: Vec<Window>,
}

/// 一个开着的窗口; 程序读不到时为 None.
#[derive(Clone, Serialize, serde::Deserialize)]
pub struct Window {
    pub program: Option<PathBuf>,
    pub title: String,
}

impl Situation {
    /// 有窗口开着的程序, 去重.
    pub fn running(&self) -> BTreeSet<&Path> {
        self.windows.iter().filter_map(|w| w.program.as_deref()).collect()
    }
}

/// 从 QRun 的哪里打开的.
#[derive(Clone, Copy)]
pub enum Via {
    /// 推荐页"猜你要打开"的第几个 (从 0 起).
    Predicted(usize),
    /// 推荐页的其他几块 (最常用, 最近文件等).
    SmartList,
    Group,
    Search,
}

impl Via {
    pub(super) fn as_db(self) -> &'static str {
        match self {
            Self::Predicted(_) => "predicted",
            Self::SmartList => "smart list",
            Self::Group => "group",
            Self::Search => "search",
        }
    }

    pub(super) fn rank(self) -> Option<i64> {
        match self {
            Self::Predicted(rank) => Some(rank as i64),
            _ => None,
        }
    }
}

/// 排好的一个候选. item 是分组里同一目标的项目, 有它时按项目启动.
#[derive(Clone)]
pub struct Ranked {
    pub key: String,
    pub name: String,
    pub target: PathBuf,
    pub item: Option<Item>,
    /// item / file / folder / program / new file.
    pub kind: &'static str,
    /// 系统记录的最后打开时间, 新文件是出现的时间.
    at: Option<i64>,
    states: Vec<&'static str>,
    features: Features,
    jev: Option<f64>,
    score: f64,
}

/// 一次打开 QRun.
pub(super) struct Open {
    pub(super) id: i64,
    at: i64,
    situation: Situation,
    /// 距上一次打开多少分钟; 这次打开之后再问 Jev 也要用它, 不能重新查 (最近一次就是这次).
    gap: Option<i64>,
    /// 情境标签和各候选的状态, 记账用.
    snapshot: Snapshot,
    candidates: Vec<Ranked>,
    /// 这次打开里已经点过的目标 (比较键); 面板没收起时会接着点下一个, 它们不算被跳过.
    launched: HashSet<String>,
    /// 呼出后从别处打开的已经记过账 (只记第一个).
    outside_learned: bool,
    /// "猜你要打开"摆出了前几个.
    shown: usize,
    /// 这次有没有用上 Jev 的结果排序.
    with_jev: bool,
    /// Jev 对这次打开的排序; 结果晚到没用上时也有, 用户打开了什么都拿它考核 Jev.
    jev_ranking: Option<Vec<String>>,
    /// 候选是什么时候排的; 沿用上次排序时也沿用它, 否则一直不换窗口就永远不重排.
    ranked_at: i64,
}

impl Library {
    /// 打开了 QRun: 记下情境, 排好候选, 记下摆出的前 shown 个. jev 是预取好的应答 (调用方确认前台程序没变且不过旧),
    /// 没有时只按本地排, 结果到了再 `apply_jev`. keep_order 时沿用上次排好的候选 (调用方先用 `order_still_fresh` 确认).
    pub fn begin_open(
        &mut self,
        situation: Situation,
        shown: usize,
        jev: Option<&Answer>,
        keep_order: bool,
        cx: &mut Context<Self>,
    ) {
        let now = now_ms();
        let week_minute = week_minute(&self.db, now);
        let gap = self.minutes_since_last_open(now);
        let windows = serde_json::to_string(&situation.windows).expect("serialize windows");
        let recent =
            serde_json::to_string(&self.timeline.recent(PARAMS.timeline.summary_events)).expect("serialize timeline");
        let inserted = self.db.execute(
            "INSERT INTO opens (at, week_minute, foreground, title, gap_minutes, clipboard, windows, recent)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                now,
                week_minute,
                situation.foreground.as_ref().map(|p| p.to_string_lossy().into_owned()),
                situation.title,
                gap,
                situation.clipboard,
                windows,
                recent
            ],
        );
        if let Err(e) = inserted {
            storage::db_save_failed(&e);
            return;
        }
        let previous = self.open.take();
        let kept = previous.as_ref().filter(|_| keep_order).map(|open| {
            let snapshot = clone_snapshot(&open.snapshot);
            (open.candidates.clone(), open.with_jev, open.jev_ranking.clone(), open.ranked_at, snapshot)
        });
        let (candidates, with_jev, jev_ranking, ranked_at, snapshot) = kept.unwrap_or_else(|| {
            let (candidates, snapshot) = self.candidates(&situation, gap, now, week_minute);
            (candidates, false, None, now, snapshot)
        });
        self.previous_open = previous;
        let mut open = Open {
            id: self.db.last_insert_rowid(),
            at: now,
            situation,
            gap,
            snapshot,
            candidates,
            launched: HashSet::new(),
            outside_learned: false,
            shown,
            with_jev,
            jev_ranking,
            ranked_at,
        };
        if let Some(answer) = jev {
            open.take_answer(answer, true, &self.model, &self.ledger, &self.weights);
        }
        self.write_shown(&open);
        self.open = Some(open);
        cx.notify();
    }

    /// 上次的排序还能接着用: 这期间没点过东西, 排好还不到 keep_order_minutes.
    pub fn order_still_fresh(&self) -> bool {
        self.open.as_ref().is_some_and(|open| {
            open.launched.is_empty() && now_ms() - open.ranked_at < PARAMS.learn.keep_order_minutes * MINUTE
        })
    }

    /// 关掉推荐: 丢掉这次打开, 之后的启动不再学习, 也不挂情境.
    pub fn end_open(&mut self, cx: &mut Context<Self>) {
        self.open = None;
        self.previous_open = None;
        cx.notify();
    }

    pub fn open_id(&self) -> Option<i64> {
        self.open.as_ref().map(|open| open.id)
    }

    /// 这次打开补调的 Jev 结果到了: 归类标签并进情境, 记下它的排序待考核; reorder (及时到了) 时按它重排.
    pub fn apply_jev(&mut self, open_id: i64, answer: &Answer, reorder: bool, cx: &mut Context<Self>) {
        let Some(open) = self.open.as_mut().filter(|open| open.id == open_id) else {
            return;
        };
        open.take_answer(answer, reorder, &self.model, &self.ledger, &self.weights);
        let open = self.open.as_ref().expect("just updated");
        self.write_shown(open);
        cx.notify();
    }

    /// 记下这次调用 Jev 的时间 (本地时间文本) 和结果, 失败同时写日志.
    pub fn record_jev(&mut self, at: String, result: Result<(), String>, cx: &mut Context<Self>) {
        if let Err(e) = &result {
            log::warn!("Jev 调用失败: {e}");
        }
        self.jev_last = Some((at, result));
        cx.notify();
    }

    /// 最近一次调用 Jev 的时间和结果.
    pub fn jev_last(&self) -> Option<&(String, Result<(), String>)> {
        self.jev_last.as_ref()
    }

    /// 这次打开排好的候选, 推荐页"猜你要打开"取前面的.
    pub fn predicted(&self) -> &[Ranked] {
        self.open.as_ref().map_or(&[], |open| &open.candidates)
    }

    /// 跨所有分组按名称匹配的项目, 按现在被打开的可能性排好; 有 Jev 结果时再加上名称不匹配但 Jev 认为很可能的候选
    /// (如搜"刚下载的 pdf"). 回车打开项目里的第一个.
    pub fn search(&self, query: &str, jev: Option<&HashMap<String, f64>>) -> (Vec<Item>, Vec<Ranked>) {
        let query = query.to_lowercase();
        let now = now_ms();
        // 搜索是这次打开里的事, 情境 (标题, 正在运行等) 和推荐页一样按打开那一刻算, 学到的账才对得上
        let (situation, gap) = match &self.open {
            Some(open) => (open.situation.clone(), open.gap),
            None => (Situation::default(), None),
        };
        let (mut candidates, _) = self.candidates(&situation, gap, now, week_minute(&self.db, now));
        if let Some(probabilities) = jev {
            rescore(&mut candidates, probabilities, &self.weights, self.ledger.jev_trust(now));
            sort(&mut candidates);
        }
        let (matched, rest): (Vec<Ranked>, Vec<Ranked>) =
            candidates.into_iter().partition(|c| c.item.is_some() && c.name.to_lowercase().contains(&query));
        let extra =
            rest.into_iter().filter(|c| c.jev.is_some_and(|p| p >= PARAMS.jev.search_min_probability)).collect();
        (matched.into_iter().filter_map(|c| c.item).collect(), extra)
    }

    /// 预取用的 Jev 请求: 假如现在打开 QRun, 按此刻的情境排好候选.
    pub fn prefetch_jev_request(&self, situation: &Situation) -> jev::Draft {
        let now = now_ms();
        let week_minute = week_minute(&self.db, now);
        let gap = self.minutes_since_last_open(now);
        let (candidates, snapshot) = self.candidates(situation, gap, now, week_minute);
        self.jev_request(situation, gap, &candidates, &snapshot.tags, None, now, week_minute)
    }

    /// 这次打开 QRun 时的 Jev 请求 (补调或搜索): 用打开那一刻的情境和摆出的候选, 搜索时名称匹配的排前面.
    /// 还没有这次打开 (系统最近记录没读完, 或关了推荐) 时为 None.
    pub fn open_jev_request(&self, query: Option<&str>) -> Option<jev::Draft> {
        let open = self.open.as_ref()?;
        let now = now_ms();
        let mut candidates = open.candidates.clone();
        if let Some(query) = query {
            let query = query.to_lowercase();
            candidates.sort_by_key(|c| !c.name.to_lowercase().contains(&query));
        }
        let tags = &open.snapshot.tags;
        Some(self.jev_request(&open.situation, open.gap, &candidates, tags, query, now, week_minute(&self.db, now)))
    }

    /// 给 Jev 的请求: 情境, 最近的启动, 习惯摘要, 本地排前 max_candidates 的候选 (各附这次情境标签下的习惯).
    /// 不是搜索时同时问归类; 最近复制过且不是受保护的内容时才问剪贴板是什么.
    #[allow(clippy::too_many_arguments)]
    fn jev_request(
        &self,
        situation: &Situation,
        gap: Option<i64>,
        candidates: &[Ranked],
        tags: &[String],
        query: Option<&str>,
        now: i64,
        week_minute: u16,
    ) -> jev::Draft {
        let names: HashMap<&str, &str> = candidates.iter().map(|c| (c.key.as_str(), c.name.as_str())).collect();

        let mut context = Map::new();
        context.insert("now".into(), json!(format!("{} (local time)", describe(week_minute))));
        if let Some(program) = &situation.foreground {
            context.insert("foreground_program".into(), json!(program.to_string_lossy()));
        }
        if let Some(title) = situation.title.as_ref().filter(|t| !t.is_empty()) {
            context.insert("foreground_window_title".into(), json!(title));
        }
        if let Some(gap) = gap {
            context.insert("minutes_since_launcher_last_opened".into(), json!(gap));
        }
        if let Some(clipboard) = &situation.clipboard {
            context.insert("clipboard_content_type".into(), json!(clipboard));
        }
        let running = situation.running();
        if !running.is_empty() {
            let running: Vec<String> = running.into_iter().map(display_name).collect();
            context.insert("running_programs".into(), json!(running));
        }
        context.insert("recent_launches".into(), json!(self.recent_launches(now)));
        context.insert("recent_activity_on_this_computer".into(), json!(activity::summary(&self.timeline, now)));
        let usual_next: Vec<&str> =
            self.model.usual_next(now, 5).into_iter().map(|key| names.get(key).copied().unwrap_or(key)).collect();
        context.insert("usually_opened_next_after_the_last_launch".into(), json!(usual_next));

        let mut history: HashMap<&str, (usize, i64)> = HashMap::new();
        for launch in &self.launches {
            let entry = history.entry(launch.key.as_str()).or_insert((0, launch.at));
            entry.0 += 1;
            entry.1 = launch.at;
        }
        // 新文件没打开过, 本地分低, 排在后面会被 max_state_bytes 截掉, Jev 就看不到它; 搜索时仍是名称匹配的优先
        let mut order: Vec<(usize, &Ranked)> = candidates.iter().enumerate().collect();
        if query.is_none() {
            order.sort_by_key(|(_, c)| c.kind != NEW_FILE);
        }
        let candidates: Vec<jev::Candidate> = order
            .into_iter()
            .take(PARAMS.jev.max_candidates)
            .map(|(rank, c)| {
                let mut details = Map::new();
                details.insert("type".into(), json!(c.kind));
                details.insert("path".into(), json!(c.target.to_string_lossy()));
                details.insert("local_rank".into(), json!(rank + 1));
                if let Some(&(count, last)) = history.get(c.key.as_str()) {
                    details.insert("times_opened".into(), json!(count));
                    details.insert("last_opened".into(), json!(ago(now - last)));
                    details.insert("usual_hours".into(), json!(self.model.usual_hours(&c.key)));
                }
                if c.kind == NEW_FILE {
                    details.insert("appeared".into(), json!(ago(now - c.at.expect("新文件有出现时间"))));
                }
                if c.features[SKIPPED] > 0. {
                    details.insert("times_skipped_recently".into(), json!((c.features[SKIPPED] * 10.).round() / 10.));
                }
                details.insert("running".into(), json!(c.states.contains(&"自己:运行中")));
                details.insert("name_in_window_title".into(), json!(c.states.contains(&"自己:在标题里")));
                let habits: Vec<Value> = self
                    .ledger
                    .evidence(tags, &c.key, now)
                    .into_iter()
                    .map(|(tag, opened, seen)| json!({ "signal": tag, "opened_it": tenths(opened), "signal_seen": tenths(seen) }))
                    .collect();
                if !habits.is_empty() {
                    details.insert("habits_in_this_situation".into(), json!(habits));
                }
                jev::Candidate { key: c.key.clone(), name: c.name.clone(), details: Value::Object(details) }
            })
            .collect();
        let classify = query.is_none();
        let ask_clip =
            classify && tags.iter().any(|t| t.starts_with("clip:")) && !tags.iter().any(|t| t == "clip:受保护");
        jev::Draft { context, candidates, query: query.map(str::to_string), classify, ask_clip }
    }

    /// 从 QRun 打开了 key: 给标签账记一笔 (这次打开里第一次开东西时, 摆出的候选的状态也记); 再按摆出的候选学一次对错,
    /// 只把多半被看到了的算作被跳过 (见 `skipped_shares`). 这次打开里之前点过的不算被跳过.
    pub(super) fn learn(&mut self, key: &str, target: &Path, via: Via, at: i64) {
        let Some(open) = self.open.as_mut() else {
            return;
        };
        let sample = if open.launched.is_empty() && !open.outside_learned { Sample::First } else { Sample::More };
        open.launched.insert(key.to_string());
        let open = self.open.as_ref().expect("just updated");
        let trust = self.ledger.jev_trust(open.at);
        self.ledger.learn(&open.snapshot, key, 1., sample);
        if let Some(ranking) = open.jev_ranking.as_ref().filter(|_| sample == Sample::First) {
            self.ledger.judge_jev(ranking, key, at);
        }
        self.save_ledger();
        let open = self.open.as_ref().expect("just updated");

        let shown = open.shown.min(open.candidates.len());
        // 打开后在设置里调大了"猜你要打开"的数量时, 点的名次可能超出当时记下的范围, 按没摆出来算
        let position = match via {
            Via::Predicted(rank) if rank < shown => Some(rank),
            _ => open.candidates[..shown].iter().position(|c| c.key == key),
        };
        let (skipped, mut shares): (Vec<&Ranked>, Vec<f64>) = skipped_shares(shown, position)
            .into_iter()
            .map(|(ix, share)| (&open.candidates[ix], share))
            .filter(|(c, _)| !open.launched.contains(&c.key))
            .unzip();
        if skipped.is_empty() {
            return;
        }
        let chosen_features = match open.candidates.iter().find(|c| c.key == key) {
            Some(c) => c.features,
            None => {
                let mut x = self.single_features(&open.snapshot, key, at);
                if open.with_jev {
                    // 不在候选里的 Jev 没见过, 概率按 0 算, 与 rescore 一致
                    brain::set_jev(&mut x, 0., trust);
                }
                x
            }
        };
        let mut rows: Vec<Features> = skipped.iter().map(|c| c.features).collect();
        rows.push(chosen_features);
        shares.push(1.);
        self.weights.learn(&rows, &shares, rows.len() - 1, open.with_jev);
        let skipped: Vec<String> = skipped.iter().map(|c| c.target.to_string_lossy().into_owned()).collect();
        if let Err(e) = self.write_feedback(&skipped, at) {
            storage::db_save_failed(&e);
        }
        log::debug!("learned from {} for {}", via.as_db(), target.display());
    }

    /// 呼出 QRun 后 outside_minutes 内, 没从 QRun 打开东西, 而是从任务栏, 资源管理器等别处打开了 key: 这也是这次情境下
    /// 想开的东西, 给标签账记一笔 (每次打开只记第一个). 不学权重: 不知道用户看到了推荐里的哪些.
    /// 系统最近记录是下次呼出时才读到的, 那时这次已是上一次, 所以两次都看.
    pub(super) fn learn_outside(&mut self, key: &str, at: i64) {
        let window = PARAMS.sample.outside_minutes * MINUTE;
        let open = [self.open.as_mut(), self.previous_open.as_mut()]
            .into_iter()
            .flatten()
            .find(|open| open.at < at && at - open.at <= window);
        let Some(open) = open.filter(|open| open.launched.is_empty() && !open.outside_learned) else {
            return;
        };
        open.outside_learned = true;
        self.ledger.learn(&open.snapshot, key, 1., Sample::First);
        if let Some(ranking) = &open.jev_ranking {
            self.ledger.judge_jev(ranking, key, at);
        }
        self.save_ledger();
        log::debug!("learned outside open of {key}");
    }

    /// 重置推荐: 清除活动记录, 学到的账和权重, 从头学. 启动记录留着 (分组按点击次数排序和最常用要用).
    /// 活动记录一起清掉, 否则账空了会被回放从历史再建出来.
    pub fn reset_recommendation(&mut self) {
        self.clear_activity();
        if let Err(e) = self.db.execute_batch("DELETE FROM ledger; DELETE FROM weights;") {
            storage::db_save_failed(&e);
            return;
        }
        self.ledger = Ledger::default();
        self.weights = Weights::initial();
    }

    /// Jev 的平均考核得分和考核次数 (按半衰期衰减); 还没考核过为 None.
    pub fn jev_score(&self) -> Option<(f64, f64)> {
        self.ledger.jev_score(now_ms())
    }

    /// 账里变了的各笔写进库.
    pub(super) fn save_ledger(&mut self) {
        let entries = self.ledger.take_changed();
        let result = (|| -> rusqlite::Result<()> {
            let tx = self.db.unchecked_transaction()?;
            {
                let mut upsert = tx.prepare(
                    "INSERT INTO ledger (book, tag, target, count, at) VALUES (?1, ?2, ?3, ?4, ?5)
                     ON CONFLICT (book, tag, target) DO UPDATE SET count = excluded.count, at = excluded.at",
                )?;
                for e in &entries {
                    upsert.execute(params![e.book, e.tag, e.target, e.count, e.at])?;
                }
            }
            tx.commit()
        })();
        if let Err(e) = result {
            storage::db_save_failed(&e);
        }
    }

    /// 不在候选里的目标 (如在分组里另点的, 时间线还没碰过的) 在这次情境下的特征: 没有状态, 没被跳过.
    fn single_features(&self, snapshot: &Snapshot, key: &str, at: i64) -> Features {
        let single = Snapshot {
            at,
            week_minute: snapshot.week_minute,
            tags: snapshot.tags.clone(),
            candidates: vec![Candidate { key: key.to_string(), states: Vec::new(), skipped: 0. }],
        };
        brain::features(&self.model, &self.ledger, &single)[0]
    }

    /// 分组里的项目, 系统最近记录, 时间线里最近碰过的目标 (同一目标只留一个, 依次优先), 按本地分数排好;
    /// 连同记账用的快照 (情境标签和各候选的状态).
    fn candidates(
        &self,
        situation: &Situation,
        gap: Option<i64>,
        now: i64,
        week_minute: u16,
    ) -> (Vec<Ranked>, Snapshot) {
        let recent = Recent::of(&self.timeline, now);
        let mut seen = HashSet::new();
        let mut list: Vec<Ranked> = Vec::new();
        for item in self.groups.iter().flat_map(|g| &g.items) {
            let key = predict::key(&item.target);
            if seen.insert(key.clone()) {
                list.push(ranked(
                    key,
                    item.name.clone(),
                    item.target.clone(),
                    Some(item.clone()),
                    kind(&item.target),
                    None,
                ));
            }
        }
        for entry in &self.system {
            let key = predict::key(&entry.target);
            if seen.insert(key.clone()) {
                list.push(ranked(key, entry.name.clone(), entry.target.clone(), None, entry.kind, Some(entry.at)));
            }
        }
        for target in &recent.targets {
            let key = predict::key(target);
            if seen.insert(key.clone()) {
                list.push(ranked(key, display_name(target), target.clone(), None, kind(target), None));
            }
        }

        let skipped = self.skipped(now);
        let title = situation.title.as_deref().unwrap_or("").to_lowercase();
        let running: HashSet<String> = situation.running().into_iter().map(predict::key).collect();
        let foreground = situation.foreground.as_deref().map(predict::key);
        let input = StateInput { running: &running, foreground: foreground.as_deref(), title: &title, recent: &recent };
        for c in &mut list {
            c.states = tags::states(&c.key, &c.name, c.kind, &input);
        }
        let snapshot = Snapshot {
            at: now,
            week_minute,
            tags: tags::context(situation, &self.timeline, gap, now),
            candidates: list
                .iter()
                .map(|c| Candidate {
                    key: c.key.clone(),
                    states: c.states.clone(),
                    skipped: skipped.get(&c.key).copied().unwrap_or(0.),
                })
                .collect(),
        };
        let features = brain::features(&self.model, &self.ledger, &snapshot);
        for (c, x) in list.iter_mut().zip(features) {
            c.score = self.weights.score(&x, false);
            c.features = x;
        }
        sort(&mut list);
        (list, snapshot)
    }

    fn minutes_since_last_open(&self, now: i64) -> Option<i64> {
        let last: Option<i64> = self.db.query_row("SELECT max(at) FROM opens", [], |row| row.get(0)).unwrap_or(None);
        last.map(|last| (now - last) / MINUTE)
    }

    /// 最近 skip_days 天里各目标被跳过的次数, 越早的分量越小 (每天减到约三分之一).
    fn skipped(&self, now: i64) -> HashMap<String, f64> {
        let since = now - PARAMS.learn.skip_days * DAY;
        let rows = self.db.prepare_cached("SELECT target, at FROM skips WHERE at >= ?1").and_then(|mut stmt| {
            stmt.query_map([since], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()
        });
        match rows {
            Ok(rows) => skip_counts(rows.iter().map(|(target, at)| (target.as_str(), *at)), now),
            Err(e) => {
                log::warn!("读取跳过记录失败: {e}");
                HashMap::new()
            }
        }
    }

    /// 最近的启动, 给 Jev 看行为序列.
    fn recent_launches(&self, now: i64) -> Vec<Value> {
        let rows = self
            .db
            .prepare_cached("SELECT target, at, source, via FROM launches ORDER BY at DESC LIMIT ?1")
            .and_then(|mut stmt| {
                stmt.query_map([PARAMS.jev.history as i64], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()
            });
        match rows {
            Ok(rows) => rows
                .into_iter()
                .map(|(target, at, source, via)| {
                    let from = if source == "system" { "outside the launcher".to_string() } else { format!("launcher, {}", via.unwrap_or_default()) };
                    json!({ "name": display_name(Path::new(&target)), "path": target, "when": ago(now - at), "opened_from": from })
                })
                .collect(),
            Err(e) => {
                log::warn!("读取最近启动失败: {e}");
                Vec::new()
            }
        }
    }

    fn write_shown(&self, open: &Open) {
        let result = (|| -> rusqlite::Result<()> {
            let tx = self.db.unchecked_transaction()?;
            tx.execute("DELETE FROM shown WHERE open_id = ?1", [open.id])?;
            {
                let mut insert = tx.prepare(
                    "INSERT INTO shown (open_id, rank, target, score, jev, features) VALUES (?1, ?2, ?3, ?4, ?5, NULL)",
                )?;
                for (rank, c) in open.candidates.iter().take(open.shown).enumerate() {
                    insert.execute(params![open.id, rank, c.target.to_string_lossy(), c.score, c.jev])?;
                }
            }
            if let Some(ranking) = &open.jev_ranking {
                let tags = open.snapshot.tags.iter().filter(|t| t.starts_with(JEV_TAG)).cloned().collect();
                let record = JevRecord { tags, ranking: ranking.clone() };
                let record = serde_json::to_string(&record).expect("serialize Jev record");
                tx.execute("UPDATE opens SET jev = ?1 WHERE id = ?2", params![record, open.id])?;
            }
            tx.commit()
        })();
        if let Err(e) = result {
            storage::db_save_failed(&e);
        }
    }

    fn write_feedback(&self, skipped: &[String], at: i64) -> rusqlite::Result<()> {
        let tx = self.db.unchecked_transaction()?;
        {
            let mut skip = tx.prepare("INSERT INTO skips (target, at) VALUES (?1, ?2)")?;
            for target in skipped {
                skip.execute(params![target, at])?;
            }
        }
        write_weights(&tx, &self.weights)?;
        tx.commit()
    }

    /// 最近 hit_rate_days 天从 QRun 打开东西时, 它在当时"猜你要打开"里排第一和进前 replay_top_k 的比例.
    /// 没有记录时为 None.
    pub fn hit_rate(&self) -> Option<HitRate> {
        let since = now_ms() - PARAMS.learn.hit_rate_days * DAY;
        let (launches, top1, top_k): (i64, f64, f64) = self
            .db
            .query_row(
                "SELECT count(*), total(r = 0), total(r < ?2) FROM (
                     SELECT (SELECT s.rank FROM shown s WHERE s.open_id = l.open_id AND s.target = l.target COLLATE NOCASE) AS r
                     FROM launches l WHERE l.source = 'qrun' AND l.open_id IS NOT NULL AND l.at >= ?1)",
                params![since, PARAMS.predict.replay_top_k],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(|e| log::warn!("统计命中率失败: {e}"))
            .ok()?;
        (launches > 0).then(|| HitRate { top1: top1 / launches as f64, top_k: top_k / launches as f64 })
    }
}

/// 推荐命中率, 设置页显示.
pub struct HitRate {
    pub top1: f64,
    pub top_k: f64,
}

impl Open {
    /// Jev 的应答到了: 归类标签并进情境 (记账时一起记, 情境一项随之重算), 记下它的排序 (之后用户打开了什么就考核它).
    /// reorder 时填上 Jev 的概率 (乘信任) 重新打分排序, 不在 Jev 候选里的按 0 算; 否则已经摆出来了, 顺序不动.
    fn take_answer(&mut self, answer: &Answer, reorder: bool, model: &Model, ledger: &Ledger, weights: &Weights) {
        for tag in &answer.tags {
            if !self.snapshot.tags.contains(tag) {
                self.snapshot.tags.push(tag.clone());
            }
        }
        self.jev_ranking = Some(answer.ranking());
        let keys = self.snapshot.candidates.iter().map(|c| c.key.as_str());
        let features: HashMap<&str, Features> = keys.zip(brain::features(model, ledger, &self.snapshot)).collect();
        for c in &mut self.candidates {
            c.features = features[c.key.as_str()];
        }
        if reorder {
            rescore(&mut self.candidates, &answer.probabilities, weights, ledger.jev_trust(self.at));
            self.with_jev = true;
            sort(&mut self.candidates);
        }
    }
}

/// 被点的目标前面哪些算被跳过, 各算多少分量 (下标指排好的候选). shown 是 "猜你要打开" 摆出了几个, position 是
/// 被点的在其中的名次: 在里面时它上面最近的 skip_window 个, 离得越远分量越小; 没摆出来 (在分组或搜索里另点了别的)
/// 时前 elsewhere_skipped 个, 推荐里没有才去别处点, 这是推荐失算, 要学.
pub(super) fn skipped_shares(shown: usize, position: Option<usize>) -> Vec<(usize, f64)> {
    let learn = &PARAMS.learn;
    match position {
        Some(rank) => {
            let start = rank.saturating_sub(learn.skip_window);
            (start..rank).map(|ix| (ix, learn.skip_decay.powi((rank - 1 - ix) as i32))).collect()
        }
        None => (0..learn.elsewhere_skipped.min(shown)).map(|ix| (ix, 1.)).collect(),
    }
}

/// 各目标被跳过的次数, 越早的分量越小 (每天减到约三分之一); 比较键为键.
pub(super) fn skip_counts<'a>(rows: impl IntoIterator<Item = (&'a str, i64)>, now: i64) -> HashMap<String, f64> {
    let mut counts = HashMap::new();
    for (target, at) in rows {
        *counts.entry(predict::key(Path::new(target))).or_insert(0.) += (-((now - at) as f64) / DAY as f64).exp();
    }
    counts
}

/// 库里存的标签账.
pub(super) fn read_ledger(db: &Connection) -> rusqlite::Result<Ledger> {
    let entries = db
        .prepare("SELECT book, tag, target, count, at FROM ledger")?
        .query_map([], |row| {
            Ok(Entry { book: row.get(0)?, tag: row.get(1)?, target: row.get(2)?, count: row.get(3)?, at: row.get(4)? })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(Ledger::from_entries(entries))
}

/// 库里学到的权重; 没学过的特征用初始值.
pub(super) fn read_weights(db: &Connection) -> rusqlite::Result<Weights> {
    let mut weights = Weights::initial();
    let rows = db
        .prepare("SELECT name, value, grad_sq FROM weights")?
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?, row.get::<_, f64>(2)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (name, value, grad_sq) in rows {
        if let Some(ix) = FEATURES.iter().position(|f| *f == name) {
            weights.value[ix] = value;
            weights.grad_sq[ix] = grad_sq;
        }
    }
    Ok(weights)
}

pub(super) fn write_weights(db: &Connection, weights: &Weights) -> rusqlite::Result<()> {
    let mut weight = db.prepare(
        "INSERT INTO weights (name, value, grad_sq) VALUES (?1, ?2, ?3)
         ON CONFLICT (name) DO UPDATE SET value = excluded.value, grad_sq = excluded.grad_sq",
    )?;
    for (ix, name) in FEATURES.iter().enumerate() {
        weight.execute(params![name, weights.value[ix], weights.grad_sq[ix]])?;
    }
    Ok(())
}

fn clone_snapshot(snapshot: &Snapshot) -> Snapshot {
    Snapshot {
        at: snapshot.at,
        week_minute: snapshot.week_minute,
        tags: snapshot.tags.clone(),
        candidates: snapshot
            .candidates
            .iter()
            .map(|c| Candidate { key: c.key.clone(), states: c.states.clone(), skipped: c.skipped })
            .collect(),
    }
}

fn rescore(candidates: &mut [Ranked], probabilities: &HashMap<String, f64>, weights: &Weights, trust: f64) {
    for c in candidates {
        let p = probabilities.get(&c.key).copied().unwrap_or(0.);
        c.jev = Some(p);
        brain::set_jev(&mut c.features, p, trust);
        c.score = weights.score(&c.features, true);
    }
}

/// 保留一位小数, 给 Jev 看的次数.
fn tenths(x: f64) -> f64 {
    (x * 10.).round() / 10.
}

fn sort(candidates: &mut [Ranked]) {
    candidates.sort_by(|a, b| b.score.total_cmp(&a.score));
}

fn ranked(
    key: String,
    name: String,
    target: PathBuf,
    item: Option<Item>,
    kind: &'static str,
    at: Option<i64>,
) -> Ranked {
    Ranked {
        key,
        name,
        target,
        item,
        kind,
        at,
        states: Vec::new(),
        features: [0.; FEATURES.len()],
        jev: None,
        score: 0.,
    }
}

/// 分组项目的类型, 给 Jev 看.
pub(super) fn kind(target: &Path) -> &'static str {
    let text = target.to_string_lossy().to_lowercase();
    if text.starts_with("shell:appsfolder") || text.ends_with(".exe") || text.ends_with(".lnk") {
        "program"
    } else if target.is_dir() {
        "folder"
    } else if text.starts_with("http://") || text.starts_with("https://") {
        "website"
    } else {
        "file"
    }
}

pub(super) fn display_name(path: &Path) -> String {
    path.file_stem().unwrap_or(path.as_os_str()).to_string_lossy().into_owned()
}

/// 一周里的分钟数换成 "Thu 21:05".
fn describe(week_minute: u16) -> String {
    let day = (week_minute / 1440) as usize;
    let minute = week_minute % 1440;
    format!("{} {:02}:{:02}", WEEKDAYS[day], minute / 60, minute % 60)
}

fn ago(ms: i64) -> String {
    match ms {
        ..HOUR => format!("{} minutes ago", ms / MINUTE),
        HOUR..DAY => format!("{} hours ago", ms / HOUR),
        _ => format!("{} days ago", ms / DAY),
    }
}
