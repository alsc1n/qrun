//! 活动时间线的传感器: 每轮轮询看前台窗口, 人是否离开, 剪贴板; 隔几秒看程序启停和监听端口; 收文件改动.
//! 窗口标题和剪贴板文字先脱敏 (model::redact) 再交出去, 内存和库里都没有明文.
//! 读到的交给 Library 合并记下, 定期写库. 关了推荐时什么都不读, 文件监听也停掉.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui_kit::*;

use crate::model::library::{Library, What};
use crate::model::redact::redact;
use crate::model::settings::{Collect, Smart};
use crate::params::PARAMS;
use crate::platform::activity::{self, FileWatcher};
use crate::platform::context::{self, Clip, ClipboardBusy};

pub struct Sensors {
    /// 推荐开着时才有.
    watcher: Option<FileWatcher>,
    /// 离开的起点 (Unix 毫秒), 没离开为 None.
    away: Option<i64>,
    clipboard: u32,
    /// 进程 ID 对应的程序; 别的会话或读不到的为 None. 只查新出现的进程.
    programs: HashMap<u32, Option<PathBuf>>,
    /// 在运行的程序; 第一次看之前为 None (第一次只记下现状, 不算启动). 端口同理.
    running: Option<HashSet<PathBuf>>,
    ports: Option<HashSet<(u16, PathBuf)>>,
    next_processes: Instant,
    next_ports: Instant,
    next_save: Instant,
}

impl Sensors {
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            watcher: None,
            away: None,
            clipboard: context::clipboard_sequence(),
            programs: HashMap::new(),
            running: None,
            ports: None,
            next_processes: now,
            next_ports: now,
            next_save: now + minutes(PARAMS.timeline.save_minutes),
        }
    }

    /// app::poll 每轮调用. 推荐关着时什么都不读; collect 里关掉的那类不读也不记.
    pub fn tick(&mut self, library: &Entity<Library>, smart: &Smart, cx: &mut App) {
        if !smart.enabled {
            *self = Self::new();
            return;
        }
        let collect = smart.collect;
        let files = match collect.files {
            true => self.watcher.get_or_insert_with(|| FileWatcher::new(roots())).take(),
            false => {
                self.watcher = None;
                Vec::new()
            }
        };
        library.update(cx, |library, _| {
            for (root, change, path) in files {
                library.record_file(&root, change.as_str(), &path);
            }
            self.sense(library, collect, smart.retention_days);
        });
    }

    fn sense(&mut self, library: &mut Library, collect: Collect, retention_days: u32) {
        let away = context::idle() >= minutes(PARAMS.jev.idle_minutes);
        match self.away {
            None if away => self.away = Some(library.record_away(context::idle())),
            Some(since) if !away => {
                library.record_back(since);
                self.away = None;
            }
            _ => {}
        }
        // QRun 自己的窗口不算, 打开 QRun 前在用的窗口停在打开那一刻
        if !away
            && collect.window
            && let Some(hwnd) = context::foreground()
            && context::process_id(hwnd) != std::process::id()
        {
            let window = context::window_info(hwnd);
            let program = window.program.map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
            library.record(What::Focus { program, title: redact(&window.title).text });
        }

        let clipboard = context::clipboard_sequence();
        if clipboard != self.clipboard {
            if !collect.clipboard {
                self.clipboard = clipboard;
                if let Some(content) = context::clipboard_kind() {
                    library.record(What::clipboard_kind(content));
                }
            } else {
                // 别的程序正占着剪贴板时不更新版本号, 下一轮再读
                match context::read_clipboard(PARAMS.redact.clip_chars) {
                    Err(ClipboardBusy) => {}
                    Ok(clip) => {
                        self.clipboard = clipboard;
                        if let Some(clip) = clip {
                            library.record(clipboard_event(clip));
                        }
                    }
                }
            }
        }

        let now = Instant::now();
        if !collect.processes {
            // 重新打开时第一次只记下现状, 不把关着期间开的程序都当成刚启动
            self.running = None;
            self.ports = None;
        } else {
            if now >= self.next_processes {
                self.next_processes = now + Duration::from_secs(PARAMS.timeline.process_interval_seconds);
                self.processes(library);
            }
            if now >= self.next_ports {
                self.next_ports = now + Duration::from_secs(PARAMS.timeline.port_interval_seconds);
                self.ports(library);
            }
        }
        if now >= self.next_save {
            self.next_save = now + minutes(PARAMS.timeline.save_minutes);
            library.save_timeline();
            library.prune(retention_days);
        }
    }

    /// 程序启停: 同一程序的进程数从 0 变多算启动, 变回 0 算退出 (浏览器开关子进程不算).
    fn processes(&mut self, library: &mut Library) {
        let ids = activity::process_ids();
        let alive: HashSet<u32> = ids.iter().copied().collect();
        self.programs.retain(|pid, _| alive.contains(pid));
        for pid in ids {
            self.programs.entry(pid).or_insert_with(|| activity::session_program(pid));
        }
        let running: HashSet<PathBuf> = self.programs.values().flatten().cloned().collect();
        if let Some(before) = &self.running {
            for program in running.difference(before) {
                library.record(What::Started { program: program.to_string_lossy().into_owned() });
            }
            for program in before.difference(&running) {
                library.record(What::Exited { program: program.to_string_lossy().into_owned() });
            }
        }
        self.running = Some(running);
    }

    /// 本会话里的程序开始或停止监听某个端口 (开发服务器, 本地工具); 系统服务的不看.
    fn ports(&mut self, library: &mut Library) {
        let ports: HashSet<(u16, PathBuf)> = activity::listening_ports()
            .into_iter()
            .filter_map(|(port, pid)| Some((port, self.programs.get(&pid)?.clone()?)))
            .collect();
        if let Some(before) = &self.ports {
            for (port, program) in ports.difference(before) {
                library.record(What::Listen { port: *port, program: program.to_string_lossy().into_owned() });
            }
            for (port, program) in before.difference(&ports) {
                library.record(What::Unlisten { port: *port, program: program.to_string_lossy().into_owned() });
            }
        }
        self.ports = Some(ports);
    }
}

/// 一次复制记进时间线前先脱敏; 大半是密钥时文字不留, 只留形状.
fn clipboard_event(clip: Clip) -> What {
    let redacted = clip.text.as_deref().map(redact);
    What::Clipboard {
        content: clip.kind.to_string(),
        from: clip.owner.map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(),
        text: redacted.as_ref().filter(|r| !r.mostly_secret).map(|r| r.text.clone()),
        shapes: redacted.map(|r| r.shapes).unwrap_or_default(),
        files: clip.extensions,
        protected: clip.protected,
    }
}

/// 监听用户目录和其他本地硬盘; 系统盘只看用户目录, 系统和程序目录里的改动不是用户在做的.
fn roots() -> Vec<PathBuf> {
    let profile = activity::profile();
    let mut roots: Vec<PathBuf> =
        activity::fixed_drives().into_iter().filter(|drive| !profile.starts_with(drive)).collect();
    roots.insert(0, profile);
    roots
}

fn minutes(n: u64) -> Duration {
    Duration::from_secs(n * 60)
}
