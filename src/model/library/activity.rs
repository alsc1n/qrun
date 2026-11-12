//! 时间线的用法: 从最近活动算各候选的近况, 列出时间线里碰过的目标当候选, 拼给 Jev 的活动摘要;
//! 以及"出现某件事之后常开什么"的长期统计 (cooccur 表). 都不按目录名或文件类型写规则, 权重由学习决定.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, params};
use serde_json::{Value, json};

use super::timeline::{Event, Timeline, What};
use crate::model::predict::key;
use crate::params::PARAMS;

const MINUTE: i64 = 60_000;
const DAY: i64 = 86_400_000;
/// 衰减到这么小的统计在读库时删掉, 表不会一直变大.
const STALE: f64 = 0.01;

/// 一个目标在最近活动里的近况, 按距今衰减 (activity_half_life_minutes).
/// 改动和启动只看最近一次有多近 (0 到 1), 不累加次数: 后台程序每隔几秒写一次日志, 次数成百上千,
/// 累加会压过所有别的信号, 而改得勤不说明用户想打开它.
#[derive(Default, Clone, Copy)]
pub(super) struct Activity {
    /// 自己, 或 (文件夹) 里面的文件最近一次被改有多近.
    pub modified: f64,
    /// 在前台停了多少分钟 (各段衰减后相加, 打分时再取对数).
    pub focused_minutes: f64,
    /// 最近一次启动有多近.
    pub started: f64,
}

/// 此刻的近况, 每次排候选时从时间线算一遍.
pub(super) struct Recent {
    activity: HashMap<String, Activity>,
    /// 最近活动过的路径 (比较键拆成的各级) 和最近一次有多近, 只留最近的 nearby_paths 个.
    paths: Vec<(Vec<String>, f64)>,
    /// 时间线里用户碰过且还在的目标: 从 QRun 打开过的, 在前台用过的程序; 越近越靠前.
    /// 只是启动过的程序不算 (后台程序启停很多), 只给它们的 started 加分; 改过的文件也不算, 只给 modified 加分.
    /// 用户打开过的文件和文件夹从系统最近记录和跳转列表来 (`platform::recent`).
    pub(super) targets: Vec<PathBuf>,
}

impl Recent {
    pub(super) fn of(timeline: &Timeline, now: i64) -> Self {
        let half_life = (PARAMS.timeline.activity_half_life_minutes * MINUTE) as f64;
        let mut activity: HashMap<String, Activity> = HashMap::new();
        let mut paths: HashMap<String, f64> = HashMap::new();
        let mut touched: HashMap<String, (PathBuf, f64)> = HashMap::new();
        let mut touch = |path: &Path, weight: f64| {
            raise(&mut touched.entry(key(path)).or_insert_with(|| (path.to_path_buf(), 0.)).1, weight);
        };
        for event in &timeline.events {
            let weight = (-((now - event.end) as f64) / half_life).exp2();
            match &event.what {
                // 文件监听不知道是谁改的, 程序自己写的日志, 数据库, 缓存也在里面: 改动只给已经是候选的
                // (用户打开过的) 加分, 不让任何东西成为候选
                What::File { change, path } if change != "removed" => {
                    let path = Path::new(path);
                    raise(&mut activity.entry(key(path)).or_default().modified, weight);
                    raise(paths.entry(key(path)).or_default(), weight);
                    if let Some(parent) = path.parent() {
                        raise(&mut activity.entry(key(parent)).or_default().modified, weight);
                    }
                }
                What::Files { dir, .. } => {
                    raise(&mut activity.entry(key(Path::new(dir))).or_default().modified, weight);
                    raise(paths.entry(key(Path::new(dir))).or_default(), weight);
                }
                What::Focus { program, .. } if !program.is_empty() => {
                    let minutes = (event.end - event.start) as f64 / MINUTE as f64;
                    activity.entry(key(Path::new(program))).or_default().focused_minutes += minutes * weight;
                    touch(Path::new(program), weight);
                }
                What::Started { program } => {
                    raise(&mut activity.entry(key(Path::new(program))).or_default().started, weight)
                }
                What::Launch { target } => {
                    raise(paths.entry(key(Path::new(target))).or_default(), weight);
                    touch(Path::new(target), weight);
                }
                _ => {}
            }
        }
        let mut targets: Vec<(PathBuf, f64)> = touched.into_values().collect();
        targets.sort_by(|a, b| b.1.total_cmp(&a.1));
        let targets =
            targets.into_iter().map(|(path, _)| path).filter(|path| path.exists()).take(PARAMS.timeline.candidates);
        let mut paths: Vec<(String, f64)> = paths.into_iter().collect();
        paths.sort_by(|a, b| b.1.total_cmp(&a.1));
        paths.truncate(PARAMS.timeline.nearby_paths);
        Self {
            activity,
            paths: paths.into_iter().map(|(path, weight)| (parts(&path), weight)).collect(),
            targets: targets.collect(),
        }
    }

    pub(super) fn activity(&self, key: &str) -> Activity {
        self.activity.get(key).copied().unwrap_or_default()
    }

    /// 和最近活动过的路径有多近: 共同的前几级占较长一方级数的比例, 四次方后让只同在用户目录下这类远亲几乎不算,
    /// 再乘那条路径最近一次活动有多近, 取最大 (0 到 1).
    pub(super) fn nearby(&self, key: &str) -> f64 {
        let parts = parts(key);
        self.paths
            .iter()
            .map(|(path, weight)| {
                let common = parts.iter().zip(path).take_while(|(a, b)| a == b).count();
                weight * (common as f64 / parts.len().max(path.len()) as f64).powi(4)
            })
            .fold(0., f64::max)
    }
}

/// 留下更近的一次.
fn raise(slot: &mut f64, weight: f64) {
    *slot = slot.max(weight);
}

fn parts(key: &str) -> Vec<String> {
    key.split('\\').filter(|part| !part.is_empty()).map(str::to_string).collect()
}

/// 最近 token_minutes 内发生了哪些事, 每件事一个标记 (动作 + 对象的比较键), 去重. 用来统计"出现这些事之后常开什么".
/// 文件改动按所在文件夹记, 同一文件夹里换个文件也算同一件事.
pub(super) fn tokens(timeline: &Timeline, now: i64) -> Vec<String> {
    let since = now - PARAMS.timeline.token_minutes * MINUTE;
    let mut tokens: Vec<String> = Vec::new();
    for event in timeline.events.iter().rev().filter(|event| event.end >= since) {
        let path = |text: &str| key(Path::new(text));
        let token = match &event.what {
            What::Focus { program, .. } if !program.is_empty() => format!("focus {}", path(program)),
            What::File { change, path: file } if change != "removed" => {
                format!("file in {}", Path::new(file).parent().map_or_else(|| path(file), key))
            }
            What::Files { dir, .. } => format!("files in {}", path(dir)),
            What::Started { program } => format!("started {}", path(program)),
            What::Exited { program } => format!("exited {}", path(program)),
            What::Listen { program, .. } => format!("listen {}", path(program)),
            What::Clipboard { content, .. } => format!("clipboard {content}"),
            What::Launch { target } => format!("launch {}", path(target)),
            What::Away => "back from away".to_string(),
            _ => continue,
        };
        if !tokens.contains(&token) {
            tokens.push(token);
        }
    }
    tokens
}

/// 给 Jev 的最近活动, 最新的在前, 最多 jev_events 条; 每条是事件本身加上多少分钟前, 有时长的带时长.
pub(super) fn summary(timeline: &Timeline, now: i64) -> Vec<Value> {
    timeline
        .events
        .iter()
        .rev()
        .take(PARAMS.timeline.jev_events)
        .map(|event: &Event| {
            let mut value = serde_json::to_value(&event.what).expect("serialize event");
            value["minutes_ago"] = json!((now - event.end) / MINUTE);
            if event.end - event.start >= MINUTE {
                value["lasted_minutes"] = json!((event.end - event.start) / MINUTE);
            }
            value
        })
        .collect()
}

/// 按 cooccur_half_life_days 衰减的次数: 存次数和记下它的时间, 用时换算到现在.
#[derive(Clone, Copy)]
struct Decayed {
    count: f64,
    at: i64,
}

impl Decayed {
    fn value(self, now: i64) -> f64 {
        let half_life = (PARAMS.timeline.cooccur_half_life_days * DAY) as f64;
        self.count * (-((now - self.at) as f64) / half_life).exp2()
    }

    fn add(&mut self, now: i64) {
        *self = Self { count: self.value(now) + 1., at: now };
    }
}

/// "出现某件事之后打开了什么"的统计: 标记 -> (出现后打开过东西的次数, 各目标的次数). 只记从 QRun 打开的,
/// 用那次打开时的标记 (`tokens`). 全部读进内存, 改动随时写库.
#[derive(Default)]
pub(super) struct Cooccur {
    tokens: HashMap<String, (Decayed, HashMap<String, Decayed>)>,
}

impl Cooccur {
    /// 读库, 顺带删掉衰减得差不多的.
    pub(super) fn read(db: &Connection) -> rusqlite::Result<Self> {
        let now = super::now_ms();
        let rows = db
            .prepare("SELECT token, target, count, at FROM cooccur")?
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    Decayed { count: row.get(2)?, at: row.get(3)? },
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut cooccur = Self::default();
        let tx = db.unchecked_transaction()?;
        for (token, target, decayed) in rows {
            let value = decayed.value(now);
            if value < STALE {
                tx.execute("DELETE FROM cooccur WHERE token = ?1 AND target = ?2", params![token, target])?;
                continue;
            }
            let (total, targets) =
                cooccur.tokens.entry(token).or_insert_with(|| (Decayed { count: 0., at: now }, HashMap::new()));
            total.count += value;
            targets.insert(target, Decayed { count: value, at: now });
        }
        tx.commit()?;
        Ok(cooccur)
    }

    /// 出现这些事之后打开 key 的比例, 取其中最高的一件; 次数少时向 0 平滑.
    pub(super) fn after_events(&self, tokens: &[String], key: &str, now: i64) -> f64 {
        tokens
            .iter()
            .filter_map(|token| self.tokens.get(token))
            .map(|(total, targets)| {
                let count = targets.get(key).map_or(0., |c| c.value(now));
                count / (total.value(now) + PARAMS.predict.smoothing)
            })
            .fold(0., f64::max)
    }

    /// 出现这些事之后打开了 key.
    pub(super) fn learn(&mut self, db: &Connection, tokens: &[String], key: &str, now: i64) -> rusqlite::Result<()> {
        let tx = db.unchecked_transaction()?;
        for token in tokens {
            let (total, targets) =
                self.tokens.entry(token.clone()).or_insert_with(|| (Decayed { count: 0., at: now }, HashMap::new()));
            total.add(now);
            let count = targets.entry(key.to_string()).or_insert(Decayed { count: 0., at: now });
            count.add(now);
            tx.execute(
                "INSERT INTO cooccur (token, target, count, at) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (token, target) DO UPDATE SET count = excluded.count, at = excluded.at",
                params![token, key, count.count, count.at],
            )?;
        }
        tx.commit()
    }
}
