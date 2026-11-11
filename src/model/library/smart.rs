//! 推荐页的候选和学习: 每次打开 QRun 记下情境, 给分组里的项目和系统最近记录算特征和分数排好;
//! 从 QRun 启动时按这次摆出的候选学一次对错; 给 Jev 拼请求.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use gpui_kit::Context;
use rusqlite::{Connection, params};
use serde::Serialize;
use serde_json::{Map, Value, json};

use super::activity::{self, Recent};
use super::{Item, Library, NEW_FILE, now_ms, week_minute};
use crate::model::jev;
use crate::model::predict::{self, FEATURES, Facets, Features, IN_TITLE, RUNNING, SKIPPED, Signals, Weights};
use crate::params::PARAMS;
use crate::storage;

const MINUTE: i64 = 60_000;
const HOUR: i64 = 3_600_000;
const DAY: i64 = 86_400_000;
const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

/// 打开 QRun 那一刻的情境, 由界面从系统读好传进来; 拿不到的项为 None 或空.
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
#[derive(Clone, Serialize)]
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
    features: Features,
    jev: Option<f64>,
    score: f64,
}

/// 一次打开 QRun.
pub(super) struct Open {
    pub(super) id: i64,
    situation: Situation,
    /// 距上一次打开多少分钟; 这次打开之后再问 Jev 也要用它, 不能重新查 (最近一次就是这次).
    gap: Option<i64>,
    pub(super) facets: Facets,
    candidates: Vec<Ranked>,
    /// 这次打开里已经点过的目标 (比较键); 面板没收起时会接着点下一个, 它们不算被跳过.
    launched: HashSet<String>,
    /// "猜你要打开"摆出了前几个.
    shown: usize,
    /// 这次有没有用上 Jev 的结果.
    with_jev: bool,
    /// 候选是什么时候排的; 沿用上次排序时也沿用它, 否则一直不换窗口就永远不重排.
    ranked_at: i64,
    /// 打开前发生了哪些事 (`activity::tokens`), 点了什么就记进"出现这些事之后打开了什么".
    pub(super) tokens: Vec<String>,
}

impl Library {
    /// 打开了 QRun: 记下情境, 排好候选, 记下摆出的前 shown 个. jev 是预取好的概率 (调用方确认前台程序没变且不过旧),
    /// 没有时只按本地排, 结果到了再 `apply_jev`. keep_order 时沿用上次排好的候选 (调用方先用 `order_still_fresh` 确认).
    pub fn begin_open(
        &mut self,
        situation: Situation,
        shown: usize,
        jev: Option<&HashMap<String, f64>>,
        keep_order: bool,
        cx: &mut Context<Self>,
    ) {
        let now = now_ms();
        let week_minute = week_minute(&self.db, now);
        let gap = self.minutes_since_last_open(now);
        let facets = facets(&situation, gap);
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
        let kept = self
            .open
            .as_ref()
            .filter(|_| keep_order)
            .map(|open| (open.candidates.clone(), open.with_jev, open.ranked_at));
        let (candidates, with_jev, ranked_at) =
            kept.unwrap_or_else(|| (self.candidates(&situation, &facets, now, week_minute), false, now));
        let mut open = Open {
            id: self.db.last_insert_rowid(),
            situation,
            gap,
            facets,
            candidates,
            launched: HashSet::new(),
            shown,
            with_jev,
            ranked_at,
            tokens: activity::tokens(&self.timeline, now),
        };
        if let Some(probabilities) = jev {
            open.apply_jev(probabilities, &self.weights);
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
        cx.notify();
    }

    pub fn open_id(&self) -> Option<i64> {
        self.open.as_ref().map(|open| open.id)
    }

    /// 这次打开补调的 Jev 结果及时到了, 按它重排.
    pub fn apply_jev(&mut self, open_id: i64, probabilities: &HashMap<String, f64>, cx: &mut Context<Self>) {
        let Some(open) = self.open.as_mut().filter(|open| open.id == open_id) else {
            return;
        };
        open.apply_jev(probabilities, &self.weights);
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
        // 搜索是这次打开里的事, 情境 (标题, 正在运行等) 和推荐页一样按打开那一刻算, 学到的权重才对得上
        let (situation, facets) = match &self.open {
            Some(open) => (open.situation.clone(), open.facets.clone()),
            None => (Situation::default(), Facets::default()),
        };
        let mut candidates = self.candidates(&situation, &facets, now, week_minute(&self.db, now));
        if let Some(probabilities) = jev {
            rescore(&mut candidates, probabilities, &self.weights);
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
        let candidates = self.candidates(situation, &facets(situation, gap), now, week_minute);
        self.jev_request(situation, gap, &candidates, None, now, week_minute)
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
        Some(self.jev_request(&open.situation, open.gap, &candidates, query, now, week_minute(&self.db, now)))
    }

    /// 给 Jev 的请求: 情境, 最近的启动, 习惯摘要, 候选 (已排好).
    fn jev_request(
        &self,
        situation: &Situation,
        gap: Option<i64>,
        candidates: &[Ranked],
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
                details.insert("running".into(), json!(c.features[RUNNING] > 0.));
                details.insert("name_in_window_title".into(), json!(c.features[IN_TITLE] > 0.));
                jev::Candidate { key: c.key.clone(), name: c.name.clone(), details: Value::Object(details) }
            })
            .collect();
        jev::Draft { context, candidates, query: query.map(str::to_string) }
    }

    /// 按这次打开时摆出的候选学一次对错, 只把多半被看到了的算作被跳过:
    /// 被点的摆在"猜你要打开"里 (不论从哪点的): 它上面最近的 skip_window 个, 离得越远分量越小;
    /// 没摆出来的 (在分组或搜索里另点了别的): "猜你要打开"的前 elsewhere_skipped 个; 推荐里没有才去别处点, 这是推荐失算, 要学.
    /// 这次打开里之前点过的不算被跳过.
    pub(super) fn learn(&mut self, key: &str, target: &Path, via: Via, at: i64) {
        let Some(open) = &mut self.open else {
            return;
        };
        open.launched.insert(key.to_string());
        let open = &*open;
        let shown = &open.candidates[..open.shown.min(open.candidates.len())];
        // 打开后在设置里调大了"猜你要打开"的数量时, 点的名次可能超出当时记下的范围, 按没摆出来算
        let position = match via {
            Via::Predicted(rank) if rank < shown.len() => Some(rank),
            _ => shown.iter().position(|c| c.key == key),
        };
        let learn = &PARAMS.learn;
        let (skipped, shares, chosen_features): (&[Ranked], Vec<f64>, Features) = match position {
            Some(rank) => {
                let skipped = &shown[rank.saturating_sub(learn.skip_window)..rank];
                let shares = (0..skipped.len()).map(|ix| learn.skip_decay.powi((skipped.len() - 1 - ix) as i32));
                (skipped, shares.collect(), shown[rank].features)
            }
            None => {
                let features = match open.candidates.iter().find(|c| c.key == key) {
                    Some(c) => c.features,
                    None => {
                        self.model.features(at, week_minute(&self.db, at), &open.facets, [(key, &Signals::default())])
                            [0]
                    }
                };
                let skipped = &shown[..learn.elsewhere_skipped.min(shown.len())];
                (skipped, vec![1.; skipped.len()], features)
            }
        };
        let (skipped, mut shares): (Vec<&Ranked>, Vec<f64>) =
            skipped.iter().zip(shares).filter(|(c, _)| !open.launched.contains(&c.key)).unzip();
        if skipped.is_empty() {
            return;
        }
        let mut rows: Vec<Features> = skipped.iter().map(|c| c.features).collect();
        rows.push(chosen_features);
        shares.push(1.);
        let chosen = rows.len() - 1;
        if open.with_jev {
            // 被点的不在 Jev 的候选里时它的概率按 0 算, 与 apply_jev 一致
            let jev = open.candidates.iter().find(|c| c.key == key).and_then(|c| c.jev).unwrap_or(0.);
            predict::set_jev(&mut rows[chosen], jev);
        }
        self.weights.learn(&rows, &shares, chosen, open.with_jev);
        let skipped: Vec<String> = skipped.iter().map(|c| c.target.to_string_lossy().into_owned()).collect();
        if let Err(e) = self.write_feedback(&skipped, at) {
            storage::db_save_failed(&e);
        }
        log::debug!("learned from {} for {}", via.as_db(), target.display());
    }

    /// 分组里的项目, 系统最近记录, 时间线里最近碰过的目标 (同一目标只留一个, 依次优先), 按本地分数排好.
    fn candidates(&self, situation: &Situation, facets: &Facets, now: i64, week_minute: u16) -> Vec<Ranked> {
        let recent = Recent::of(&self.timeline, now);
        let tokens = activity::tokens(&self.timeline, now);
        let mut seen = HashSet::new();
        let mut list: Vec<Ranked> = Vec::new();
        let items = self.groups.iter().flat_map(|g| &g.items);
        for item in items {
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
        let signals: Vec<Signals> = list
            .iter()
            .map(|c| {
                let name = c.name.to_lowercase();
                let activity = recent.activity(&c.key);
                Signals {
                    modified: activity.modified,
                    focused_minutes: activity.focused_minutes,
                    started: activity.started,
                    nearby: recent.nearby(&c.key),
                    after_events: self.cooccur.after_events(&tokens, &c.key, now),
                    skipped: skipped.get(&c.key).copied().unwrap_or(0.),
                    // 一个字的名称在标题里到处都是, 不算
                    in_title: name.chars().count() >= 2 && title.contains(&name),
                    running: running.contains(&c.key),
                    fresh_hours: (c.kind == NEW_FILE)
                        .then(|| (now - c.at.expect("新文件有出现时间")) as f64 / HOUR as f64),
                }
            })
            .collect();
        let features =
            self.model.features(now, week_minute, facets, list.iter().map(|c| c.key.as_str()).zip(signals.iter()));
        for (c, x) in list.iter_mut().zip(features) {
            c.score = self.weights.score(&x, false);
            c.features = x;
        }
        sort(&mut list);
        list
    }

    fn minutes_since_last_open(&self, now: i64) -> Option<i64> {
        let last: Option<i64> = self.db.query_row("SELECT max(at) FROM opens", [], |row| row.get(0)).unwrap_or(None);
        last.map(|last| (now - last) / MINUTE)
    }

    /// 最近 skip_days 天里各目标被跳过的次数, 越早的分量越小 (每天减到约三分之一).
    fn skipped(&self, now: i64) -> HashMap<String, f64> {
        let since = now - PARAMS.learn.skip_days * DAY;
        let mut counts = HashMap::new();
        let rows = self.db.prepare_cached("SELECT target, at FROM skips WHERE at >= ?1").and_then(|mut stmt| {
            stmt.query_map([since], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()
        });
        match rows {
            Ok(rows) => {
                for (target, at) in rows {
                    *counts.entry(predict::key(Path::new(&target))).or_insert(0.) +=
                        (-((now - at) as f64) / DAY as f64).exp();
                }
            }
            Err(e) => log::warn!("读取跳过记录失败: {e}"),
        }
        counts
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
                    "INSERT INTO shown (open_id, rank, target, score, jev, features) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                )?;
                for (rank, c) in open.candidates.iter().take(open.shown).enumerate() {
                    let features = serde_json::to_string(&c.features).expect("serialize features");
                    insert.execute(params![open.id, rank, c.target.to_string_lossy(), c.score, c.jev, features])?;
                }
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
            let mut weight = tx.prepare(
                "INSERT INTO weights (name, value, grad_sq) VALUES (?1, ?2, ?3)
                 ON CONFLICT (name) DO UPDATE SET value = excluded.value, grad_sq = excluded.grad_sq",
            )?;
            for (ix, name) in FEATURES.iter().enumerate() {
                weight.execute(params![name, self.weights.value[ix], self.weights.grad_sq[ix]])?;
            }
        }
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
    /// 填上 Jev 的概率重新打分; 不在 Jev 候选里的 (请求超长时去掉的排名靠后的) 按 0 算.
    fn apply_jev(&mut self, probabilities: &HashMap<String, f64>, weights: &Weights) {
        rescore(&mut self.candidates, probabilities, weights);
        self.with_jev = true;
        sort(&mut self.candidates);
    }
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

fn facets(situation: &Situation, gap: Option<i64>) -> Facets {
    Facets {
        foreground: situation.foreground.as_deref().map(predict::key),
        gap: gap.map(predict::gap_bucket),
        clipboard: situation.clipboard.clone(),
    }
}

fn rescore(candidates: &mut [Ranked], probabilities: &HashMap<String, f64>, weights: &Weights) {
    for c in candidates {
        let p = probabilities.get(&c.key).copied().unwrap_or(0.);
        c.jev = Some(p);
        predict::set_jev(&mut c.features, p);
        c.score = weights.score(&c.features, true);
    }
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
    Ranked { key, name, target, item, kind, at, features: [0.; FEATURES.len()], jev: None, score: 0. }
}

/// 分组项目的类型, 给 Jev 看.
fn kind(target: &Path) -> &'static str {
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

fn display_name(path: &Path) -> String {
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
