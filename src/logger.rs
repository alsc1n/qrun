//! 运行日志: exe 旁的 qrun.log, 记 warn / error (含 GPUI 内部经 `log` 报的错) 和崩溃信息.
//! 每次启动把上次的日志改名为 qrun.log.old 再新开, 崩溃后重启一次仍能看到崩溃那次的日志, 文件也不会越积越大.

use std::fs::{self, File};
use std::io::Write;
use std::sync::Mutex;

use log::{LevelFilter, Log, Metadata, Record};

use crate::{platform, storage};

const FILE: &str = "qrun.log";

/// 不带缓冲直接写文件: 发布版 panic 后进程立即结束, 缓冲里的内容来不及写出.
struct FileLogger(Mutex<File>);

impl Log for FileLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= LevelFilter::Warn
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = format!("{} {} [{}] {}\n", platform::local_time(), record.level(), record.target(), record.args());
        // 日志写不进去不影响使用, 也不能为此再报错 (报错又会写日志)
        let _ = self.0.lock().expect("log file lock").write_all(line.as_bytes());
    }

    fn flush(&self) {}
}

pub fn init() {
    let path = storage::path(FILE);
    if path.exists() {
        fs::rename(&path, path.with_extension("log.old")).expect("keep previous log");
    }
    let file = File::create(&path).expect("create log file");
    log::set_boxed_logger(Box::new(FileLogger(Mutex::new(file)))).expect("install logger");
    log::set_max_level(LevelFilter::Warn);
}
