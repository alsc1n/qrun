//! 活动时间线: 传感器 (app::sensors) 报来的前台窗口, 离开, 程序启停, 剪贴板, 文件改动, 监听端口, 以及从 QRun 打开的目标,
//! 合并降噪后放在内存里, 只留最近 keep_minutes, 最多 max_events 条; 定期和退出时整表覆盖写进库, 重启后读回.
//! 每次打开 QRun 时最近 summary_events 条随那次打开存下 (opens.recent), 以后换特征可回放.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use super::{Library, is_own, now_ms, own_dirs};
use crate::model::predict;
use crate::params::PARAMS;
use crate::storage;

const SECOND: i64 = 1000;
const MINUTE: i64 = 60_000;

/// 时间线上的一条; start..end 是 Unix 毫秒, 瞬间发生的两者相等.
#[derive(Clone, Serialize, Deserialize)]
pub struct Event {
    pub start: i64,
    pub end: i64,
    #[serde(flatten)]
    pub what: What,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum What {
    /// 前台窗口; 程序读不到时为空.
    Focus {
        program: String,
        title: String,
    },
    /// 人离开 (没有键盘鼠标输入).
    Away,
    Started {
        program: String,
    },
    Exited {
        program: String,
    },
    /// 剪贴板换了内容, 只记类型.
    Clipboard {
        content: String,
    },
    /// change: created / modified / removed / renamed.
    File {
        change: String,
        path: String,
    },
    /// 短时间大量改动 (编译, 解压, 同步) 合并成一条: 这些改动的共同上级目录和次数.
    Files {
        dir: String,
        count: u32,
    },
    Listen {
        port: u16,
        program: String,
    },
    Unlisten {
        port: u16,
        program: String,
    },
    /// 从 QRun 打开的目标.
    Launch {
        target: String,
    },
}

pub(super) struct Timeline {
    /// 早的在前; 前台窗口会被后续心跳延长, 所以只是大致按结束时间排.
    pub(super) events: VecDeque<Event>,
    /// 各目录 (监听目录下第一级) 最近 burst_window_seconds 内的文件改动时间, 最多留 burst_events + 1 个.
    bursts: HashMap<PathBuf, VecDeque<i64>>,
    /// QRun 自己的目录 (数据目录, exe 所在目录), 里面的改动不记.
    own_dirs: Vec<String>,
    /// 上次写库之后有没有变.
    dirty: bool,
}

impl Timeline {
    /// 库里存的时间线, 去掉已超过 keep_minutes 的.
    pub(super) fn read(db: &Connection) -> rusqlite::Result<Self> {
        let since = now_ms() - PARAMS.timeline.keep_minutes * MINUTE;
        let texts = db
            .prepare("SELECT event FROM timeline ORDER BY rowid")?
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let events = texts
            .iter()
            .filter_map(|text| {
                serde_json::from_str::<Event>(text).inspect_err(|e| log::warn!("时间线里有一条读不了, 跳过: {e}")).ok()
            })
            .filter(|event| event.end >= since)
            .collect();
        Ok(Self { events, bursts: HashMap::new(), own_dirs: own_dirs(), dirty: false })
    }

    /// 最近 n 条, 早的在前.
    pub(super) fn recent(&self, n: usize) -> Vec<&Event> {
        self.events.iter().skip(self.events.len().saturating_sub(n)).collect()
    }

    fn push(&mut self, what: What, start: i64, end: i64) {
        self.events.push_back(Event { start, end, what });
        self.dirty = true;
        let since = end - PARAMS.timeline.keep_minutes * MINUTE;
        while self.events.len() > PARAMS.timeline.max_events || self.events.front().is_some_and(|e| e.end < since) {
            self.events.pop_front();
        }
    }

    fn last_focus(&self) -> Option<usize> {
        self.events.iter().rposition(|e| matches!(e.what, What::Focus { .. }))
    }

    /// 前台窗口心跳 (每轮轮询一次): 和上一个前台窗口相同且接得上就延长它, 否则另起一条.
    /// 停留不到 focus_min_seconds 的 (切换途中经过的) 在换走时删掉; 删掉后前后是同一个窗口的接回去.
    fn focus(&mut self, what: What, now: i64) {
        let merge = PARAMS.timeline.merge_seconds * SECOND;
        let continues = |event: &Event| event.what == what && now - event.end <= merge;
        if let Some(ix) = self.last_focus() {
            if continues(&self.events[ix]) {
                self.events[ix].end = now;
                self.dirty = true;
                return;
            }
            if self.events[ix].end - self.events[ix].start < PARAMS.timeline.focus_min_seconds * SECOND {
                self.events.remove(ix);
                if let Some(ix) = self.last_focus()
                    && continues(&self.events[ix])
                {
                    self.events[ix].end = now;
                    self.dirty = true;
                    return;
                }
            }
        }
        self.push(what, now, now);
    }

    /// 连着复制几次只留最后一次的类型.
    fn clipboard(&mut self, what: What, now: i64) {
        if let Some(last) = self.events.back_mut()
            && matches!(last.what, What::Clipboard { .. })
            && now - last.end <= PARAMS.timeline.merge_seconds * SECOND
        {
            last.what = what;
            last.end = now;
            self.dirty = true;
            return;
        }
        self.push(what, now, now);
    }

    /// 一次文件改动. group 是它所在的监听目录下第一级目录, 按它判断是不是一阵大量改动.
    /// 同一文件连着改 (相邻两次相隔不到 file_merge_minutes) 并成一条, 延长结束时间 (新建后接着写入仍算新建;
    /// 新建后又删掉的是临时文件, 整条去掉).
    fn file(&mut self, group: PathBuf, change: &str, path: &Path, now: i64) {
        let t = &PARAMS.timeline;
        let window = t.burst_window_seconds * SECOND;
        let times = self.bursts.entry(group.clone()).or_default();
        while times.front().is_some_and(|&at| now - at > window) {
            times.pop_front();
        }
        times.push_back(now);
        if times.len() > t.burst_events {
            times.pop_front();
            self.burst(&group, path.parent().unwrap_or(path), now);
            return;
        }

        let text = path.to_string_lossy();
        let merge = t.file_merge_minutes * MINUTE;
        let same = self
            .events
            .iter()
            .rposition(|e| now - e.end <= merge && matches!(&e.what, What::File { path: p, .. } if *p == text));
        let Some(ix) = same else {
            self.push(What::File { change: change.to_string(), path: text.into_owned() }, now, now);
            return;
        };
        self.dirty = true;
        let created = matches!(&self.events[ix].what, What::File { change: c, .. } if c == "created");
        if created && change == "removed" {
            self.events.remove(ix);
            return;
        }
        // 挪到最后: 时间线按结束时间排, 一直在改的文件不会被当成旧事件从前面丢掉
        let mut event = self.events.remove(ix).expect("index from rposition");
        event.end = now;
        if !created && let What::File { change: before, .. } = &mut event.what {
            *before = change.to_string();
        }
        self.events.push_back(event);
    }

    /// 一阵大量改动里的一次: 并进这个目录正在累计的一条 (还在 burst_window_seconds 内的), 没有就新起一条.
    fn burst(&mut self, group: &Path, dir: &Path, now: i64) {
        let window = PARAMS.timeline.burst_window_seconds * SECOND;
        self.dirty = true;
        let open = self
            .events
            .iter_mut()
            .rev()
            .take_while(|e| now - e.end <= window)
            .find(|e| matches!(&e.what, What::Files { dir, .. } if Path::new(dir).starts_with(group)));
        let Some(event) = open else {
            self.push(What::Files { dir: dir.to_string_lossy().into_owned(), count: 1 }, now, now);
            return;
        };
        event.end = now;
        if let What::Files { dir: common, count } = &mut event.what {
            *common = common_dir(Path::new(common), dir).to_string_lossy().into_owned();
            *count += 1;
        }
    }
}

/// 两个目录的共同上级.
fn common_dir(a: &Path, b: &Path) -> PathBuf {
    a.components().zip(b.components()).take_while(|(x, y)| x == y).map(|(x, _)| x).collect()
}

impl Library {
    /// 记一条时间线: 前台窗口按心跳延长, 剪贴板连着复制合并, 其余照记.
    pub fn record(&mut self, what: What) {
        let now = now_ms();
        match what {
            What::Focus { .. } => self.timeline.focus(what, now),
            What::Clipboard { .. } => self.timeline.clipboard(what, now),
            _ => self.timeline.push(what, now, now),
        }
    }

    /// 监听目录 root 下的一次文件改动. QRun 自己目录里的 (库, 日志) 不记.
    pub fn record_file(&mut self, root: &Path, change: &str, path: &Path) {
        if is_own(&predict::key(path), &self.timeline.own_dirs) {
            return;
        }
        let group = match path.strip_prefix(root).ok().and_then(|rest| rest.components().next()) {
            Some(first) => root.join(first),
            None => root.to_path_buf(),
        };
        self.timeline.file(group, change, path, now_ms());
    }

    /// 人离开了 (已有 idle 没有输入): 前台窗口只算到最后一次输入. 返回离开的起点, 回来时交给 `record_back`.
    pub fn record_away(&mut self, idle: Duration) -> i64 {
        let since = now_ms() - idle.as_millis() as i64;
        if let Some(ix) = self.timeline.last_focus() {
            let event = &mut self.timeline.events[ix];
            event.end = event.end.min(since).max(event.start);
            self.timeline.dirty = true;
        }
        since
    }

    /// 人回来了: 记一条从 since 到现在的离开.
    pub fn record_back(&mut self, since: i64) {
        self.timeline.push(What::Away, since, now_ms());
    }

    /// 时间线有变化时整表覆盖写进库. 定期和退出时调用.
    pub fn save_timeline(&mut self) {
        let window = PARAMS.timeline.burst_window_seconds * SECOND;
        let now = now_ms();
        self.timeline.bursts.retain(|_, times| times.back().is_some_and(|&at| now - at <= window));
        if !self.timeline.dirty {
            return;
        }
        let result = (|| -> rusqlite::Result<()> {
            let tx = self.db.unchecked_transaction()?;
            tx.execute("DELETE FROM timeline", [])?;
            {
                let mut insert = tx.prepare("INSERT INTO timeline (event) VALUES (?1)")?;
                for event in &self.timeline.events {
                    insert.execute([serde_json::to_string(event).expect("serialize event")])?;
                }
            }
            tx.commit()
        })();
        match result {
            Ok(()) => self.timeline.dirty = false,
            Err(e) => storage::db_save_failed(&e),
        }
    }
}
