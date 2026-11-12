//! 回放: 按时间顺序把库里的历史 (启动记录, 每次打开时的情境, 归档的活动时间线, 跳过记录) 重新喂给一个空的模型,
//! 每次打开 QRun 时用它排序, 看实际打开的目标排第几, 然后照常学习. 和线上走同一套打分和记账 (`model::brain`),
//! 量出的就是线上效果; 跑完得到的账和权重就是用全部历史从零练出来的, 后台定时用它换掉在线学到的.
//!
//! 答案: 这次打开里从 QRun 点开的第一个; 没有时, 呼出后 outside_minutes 内 (且在下次呼出前) 从别处打开的第一个
//! (系统最近记录里的文件, 文件夹, 用户自己开的程序). 对比的 "当时" 是那次实际摆出的名次 (shown 表).
//! 与线上的差别: 候选里的分组项目用现在的分组; 系统最近记录用之前打开过的目标近似; 下载和桌面的新文件没有.
//! 归档 (history) 之前的打开只有当时存下的最近 summary_events 条活动, 能还原多少算多少.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::path::{Path, PathBuf};

use rusqlite::Connection;

use super::activity::{self, Recent};
use super::smart::{self, Situation, Window};
use super::tags::{self, StateInput};
use super::timeline::{Event, Timeline, What};
use super::{Group, Library, read_groups, read_launches};
use crate::model::brain::{self, Candidate, Features, Ledger, Sample, Snapshot, Weights};
use crate::model::predict::{self, Launch, Model, Tuning};
use crate::params::PARAMS;
use crate::storage;

const MINUTE: i64 = 60_000;
const DAY: i64 = 86_400_000;

/// 分组里的一个项目, 回放时当候选.
pub(super) struct ReplayItem {
    key: String,
    name: String,
    kind: &'static str,
}

/// 现在分组里的项目, 回放的候选用.
pub(super) fn items(library: &Library) -> Vec<ReplayItem> {
    replay_items(&library.groups)
}

fn replay_items(groups: &[Group]) -> Vec<ReplayItem> {
    groups
        .iter()
        .flat_map(|g| &g.items)
        .map(|i| ReplayItem { key: predict::key(&i.target), name: i.name.clone(), kind: smart::kind(&i.target) })
        .collect()
}

/// 回放结果: 从 QRun 打开时新算法和当时实际的名次 (按当时有没有用上 Jev 分开, 回放不带 Jev, 没用 Jev 的那部分
/// 才是同等条件的对比), 呼出后从别处打开时新算法的名次.
#[derive(Default)]
pub struct Report {
    opens: usize,
    without_jev: Hits,
    with_jev: Hits,
    /// 从 QRun 打开的目标根本不在候选里 (排不上, 怎么改打分也猜不中).
    missing: usize,
    outside: Hits,
}

/// 一组打开里答案排第一, 进前 k 的次数; online 是当时实际摆出的.
#[derive(Default)]
struct Hits {
    count: usize,
    top1: usize,
    top_k: usize,
    online_top1: usize,
    online_top_k: usize,
}

impl Hits {
    fn add(&mut self, rank: Option<usize>, online: Option<usize>) {
        let k = PARAMS.predict.replay_top_k;
        self.count += 1;
        self.top1 += (rank == Some(0)) as usize;
        self.top_k += rank.is_some_and(|r| r < k) as usize;
        self.online_top1 += (online == Some(0)) as usize;
        self.online_top_k += online.is_some_and(|r| r < k) as usize;
    }
}

impl fmt::Display for Hits {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let k = PARAMS.predict.replay_top_k;
        let pct = |n: usize| if self.count == 0 { 0. } else { n as f64 * 100. / self.count as f64 };
        write!(
            f,
            "{} 次, 排第一 {:.0}% (当时 {:.0}%), 前 {k} {:.0}% (当时 {:.0}%)",
            self.count,
            pct(self.top1),
            pct(self.online_top1),
            pct(self.top_k),
            pct(self.online_top_k)
        )
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "{} 次打开. 从 QRun 打开, 当时没用 Jev: {}; 当时用了 Jev: {}; 不在候选里 {}. 呼出后从别处打开: {}.",
            self.opens, self.without_jev, self.with_jev, self.missing, self.outside
        )
    }
}

/// 开发用 (`qrun.exe --replay`): 只读打开库, 调参后回放一遍, 返回报告.
pub fn report() -> Result<String, String> {
    let fail = |e: rusqlite::Error| format!("回放失败: {e}");
    let db = storage::open_db_read_only().map_err(fail)?;
    let items = replay_items(&read_groups(&db).map_err(fail)?);
    let tuning = predict::tune(&read_launches(&db).map_err(fail)?, super::now_ms());
    let (_, _, report) = run(&db, &items, tuning).map_err(fail)?;
    Ok(report.to_string())
}

struct OpenRow {
    id: i64,
    at: i64,
    week_minute: u16,
    situation: Situation,
    gap: Option<i64>,
    /// 归档之前的打开存下的最近活动.
    recent: Option<String>,
}

pub(super) fn run(
    db: &Connection,
    items: &[ReplayItem],
    tuning: Tuning,
) -> rusqlite::Result<(Ledger, Weights, Report)> {
    let launches: Vec<(Launch, String, Option<i64>)> = db
        .prepare("SELECT target, at, week_minute, source, open_id FROM launches ORDER BY at, id")?
        .query_map([], |row| {
            let launch = Launch {
                key: predict::key(Path::new(&row.get::<_, String>(0)?)),
                at: row.get(1)?,
                week_minute: row.get(2)?,
            };
            Ok((launch, row.get(3)?, row.get(4)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let opens: Vec<OpenRow> = db
        .prepare("SELECT id, at, week_minute, foreground, title, gap_minutes, clipboard, windows, recent FROM opens ORDER BY at")?
        .query_map([], |row| {
            let windows: Option<String> = row.get(7)?;
            Ok(OpenRow {
                id: row.get(0)?,
                at: row.get(1)?,
                week_minute: row.get(2)?,
                situation: Situation {
                    foreground: row.get::<_, Option<String>>(3)?.map(PathBuf::from),
                    title: row.get(4)?,
                    clipboard: row.get(6)?,
                    windows: windows
                        .map(|json| serde_json::from_str::<Vec<Window>>(&json).expect("opens.windows 是窗口列表"))
                        .unwrap_or_default(),
                },
                gap: row.get(5)?,
                recent: row.get(8)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let history: Vec<Event> = db
        .prepare("SELECT start, end, event FROM history ORDER BY end")?
        .query_map([], |row| {
            let what: What = serde_json::from_str(&row.get::<_, String>(2)?).expect("history.event 是事件");
            Ok(Event { start: row.get(0)?, end: row.get(1)?, what })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let skips: Vec<(String, i64)> = db
        .prepare("SELECT target, at FROM skips ORDER BY at")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut shown: HashMap<i64, Vec<String>> = HashMap::new();
    let mut with_jev: HashSet<i64> = HashSet::new();
    let mut rows = db.prepare("SELECT open_id, target, jev FROM shown ORDER BY open_id, rank")?;
    let rows =
        rows.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, Option<f64>>(2)?)))?;
    for row in rows {
        let (open, target, jev) = row?;
        shown.entry(open).or_default().push(predict::key(Path::new(&target)));
        if jev.is_some() {
            with_jev.insert(open);
        }
    }

    // 每次打开里从 QRun 点开的, 按先后
    let mut clicks: HashMap<i64, Vec<&str>> = HashMap::new();
    for (launch, source, open) in &launches {
        if let (Some(open), "qrun") = (open, source.as_str()) {
            clicks.entry(*open).or_default().push(&launch.key);
        }
    }
    // 别处打开的: 系统最近记录里的, 用户自己开的程序
    let mut outside: Vec<(String, i64)> = launches
        .iter()
        .filter(|(_, source, _)| source == "system")
        .map(|(launch, _, _)| (launch.key.clone(), launch.at))
        .chain(activity::user_started(&history))
        .collect();
    outside.sort_by_key(|(_, at)| *at);

    let window = PARAMS.sample.outside_minutes * MINUTE;
    let keep = PARAMS.timeline.keep_minutes * MINUTE;
    let skip_days = PARAMS.learn.skip_days * DAY;
    let mut model = Model::new(tuning);
    let mut ledger = Ledger::default();
    let mut weights = Weights::initial();
    let mut report = Report::default();
    let mut timeline = Timeline::of_events(VecDeque::new());
    let mut recent_skips: VecDeque<&(String, i64)> = VecDeque::new();
    // 之前打开过的目标和最后一次的时间, 当系统最近记录用
    let mut opened: HashMap<&str, i64> = HashMap::new();
    let (mut next_launch, mut next_event, mut next_skip) = (0, 0, 0);

    for (ix, open) in opens.iter().enumerate() {
        let until = opens.get(ix + 1).map_or(i64::MAX, |next| next.at);
        while let Some((launch, _, _)) = launches.get(next_launch).filter(|(l, _, _)| l.at < open.at) {
            model.add(launch);
            opened.insert(&launch.key, launch.at);
            next_launch += 1;
        }
        while let Some(event) = history.get(next_event).filter(|e| e.end <= open.at) {
            timeline.events.push_back(event.clone());
            next_event += 1;
        }
        while timeline.events.front().is_some_and(|e| e.end < open.at - keep) {
            timeline.events.pop_front();
        }
        while let Some(skip) = skips.get(next_skip).filter(|(_, at)| *at < open.at) {
            recent_skips.push_back(skip);
            next_skip += 1;
        }
        while recent_skips.front().is_some_and(|(_, at)| *at < open.at - skip_days) {
            recent_skips.pop_front();
        }

        let saved;
        let at_open = if timeline.events.is_empty()
            && let Some(recent) = &open.recent
        {
            saved = Timeline::of_events(serde_json::from_str(recent).expect("opens.recent 是事件列表"));
            &saved
        } else {
            &timeline
        };
        let snapshot = snapshot(items, &opened, at_open, open, &recent_skips);
        let features = brain::features(&model, &ledger, &snapshot);
        let mut order: Vec<usize> = (0..features.len()).collect();
        order.sort_by(|&a, &b| weights.score(&features[b], false).total_cmp(&weights.score(&features[a], false)));
        let rank_of = |key: &str| order.iter().position(|&c| snapshot.candidates[c].key == key);

        report.opens += 1;
        let clicked = clicks.get(&open.id).map(Vec::as_slice).unwrap_or_default();
        if let Some(&first) = clicked.first() {
            let rank = rank_of(first);
            let online = shown.get(&open.id).and_then(|list| list.iter().position(|k| k == first));
            report.missing += rank.is_none() as usize;
            match with_jev.contains(&open.id) {
                true => report.with_jev.add(rank, online),
                false => report.without_jev.add(rank, online),
            }

            ledger.learn(&snapshot, first, 1., Sample::First);
            for key in &clicked[1..] {
                ledger.learn(&snapshot, key, 1., Sample::More);
            }
            let shown_count = shown.get(&open.id).map_or(0, Vec::len);
            let position = rank.filter(|&r| r < shown_count);
            let skipped: Vec<(Features, f64)> = smart::skipped_shares(shown_count, position)
                .into_iter()
                .filter(|(ix, _)| !clicked.contains(&snapshot.candidates[order[*ix]].key.as_str()))
                .map(|(ix, share)| (features[order[ix]], share))
                .collect();
            if !skipped.is_empty() {
                let chosen = match rank {
                    Some(r) => features[order[r]],
                    None => single_features(&model, &ledger, &snapshot, first),
                };
                let (mut rows, mut shares): (Vec<Features>, Vec<f64>) = skipped.into_iter().unzip();
                rows.push(chosen);
                shares.push(1.);
                weights.learn(&rows, &shares, rows.len() - 1, false);
            }
        } else if let Some((key, _)) =
            outside.iter().find(|(_, at)| *at > open.at && at - open.at <= window && *at < until)
        {
            report.outside.add(rank_of(key), None);
            ledger.learn(&snapshot, key, 1., Sample::First);
        }
    }
    Ok((ledger, weights, report))
}

/// 这次打开时的候选 (分组项目, 之前打开过的最近 replay_candidates 个目标, 时间线里碰过的) 和它们的状态, 情境标签.
fn snapshot(
    items: &[ReplayItem],
    opened: &HashMap<&str, i64>,
    timeline: &Timeline,
    open: &OpenRow,
    skips: &VecDeque<&(String, i64)>,
) -> Snapshot {
    let recent = Recent::of(timeline, open.at);
    let mut seen = HashSet::new();
    let mut list: Vec<(String, String, &'static str)> = Vec::new();
    for item in items {
        if seen.insert(item.key.clone()) {
            list.push((item.key.clone(), item.name.clone(), item.kind));
        }
    }
    let mut before: Vec<(&str, i64)> = opened.iter().map(|(&k, &at)| (k, at)).collect();
    before.sort_by_key(|&(_, at)| std::cmp::Reverse(at));
    let paths = before.into_iter().take(PARAMS.replay.candidates).map(|(key, _)| PathBuf::from(key));
    for path in paths.chain(recent.targets.iter().cloned()) {
        let key = predict::key(&path);
        if seen.insert(key.clone()) {
            list.push((key, smart::display_name(&path), smart::kind(&path)));
        }
    }

    let situation = &open.situation;
    let title = situation.title.as_deref().unwrap_or("").to_lowercase();
    let running: HashSet<String> = situation.running().into_iter().map(predict::key).collect();
    let foreground = situation.foreground.as_deref().map(predict::key);
    let input = StateInput { running: &running, foreground: foreground.as_deref(), title: &title, recent: &recent };
    let skipped = smart::skip_counts(skips.iter().map(|(target, at)| (target.as_str(), *at)), open.at);
    Snapshot {
        at: open.at,
        week_minute: open.week_minute,
        tags: tags::context(situation, timeline, open.gap, open.at),
        candidates: list
            .into_iter()
            .map(|(key, name, kind)| Candidate {
                states: tags::states(&key, &name, kind, &input),
                skipped: skipped.get(&key).copied().unwrap_or(0.),
                key,
            })
            .collect(),
    }
}

/// 不在候选里的目标在这次情境下的特征: 没有状态, 没被跳过.
fn single_features(model: &Model, ledger: &Ledger, snapshot: &Snapshot, key: &str) -> Features {
    let single = Snapshot {
        at: snapshot.at,
        week_minute: snapshot.week_minute,
        tags: snapshot.tags.clone(),
        candidates: vec![Candidate { key: key.to_string(), states: Vec::new(), skipped: 0. }],
    };
    brain::features(model, ledger, &single)[0]
}
