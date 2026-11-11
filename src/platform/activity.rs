//! 活动时间线的传感器: 进程, 监听端口, 文件改动. 只读元数据 (路径, 端口, 改动类型), 不读文件内容.
//! 读不到的 (别的会话的进程, 打不开的目录) 就当没有, 不报错.

use std::collections::HashMap;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, ERROR_INSUFFICIENT_BUFFER, HANDLE, NO_ERROR};
use windows::Win32::NetworkManagement::IpHelper::{
    GetExtendedTcpTable, MIB_TCP6TABLE_OWNER_PID, MIB_TCPTABLE_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ACTION_ADDED, FILE_ACTION_MODIFIED, FILE_ACTION_REMOVED, FILE_ACTION_RENAMED_NEW_NAME,
    FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_SYSTEM, FILE_FLAG_BACKUP_SEMANTICS, FILE_LIST_DIRECTORY,
    FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_INFORMATION,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, GetDriveTypeW, GetFileAttributesW, GetLogicalDrives,
    INVALID_FILE_ATTRIBUTES, OPEN_EXISTING, ReadDirectoryChangesW,
};
use windows::Win32::System::IO::CancelSynchronousIo;
use windows::Win32::System::ProcessStatus::EnumProcesses;
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::UI::Shell::FOLDERID_Profile;
use windows::core::PCWSTR;

use super::{context, recent, wide};

/// 地址族编号 (ws2def.h), 只为这两个不必引入整个 WinSock 模块.
const AF_INET: u32 = 2;
const AF_INET6: u32 = 23;
/// GetDriveTypeW 的本地硬盘 (WinBase.h).
const DRIVE_FIXED: u32 = 3;
/// 每个监听线程缓存多少个目录的隐藏判断, 满了清空重来.
const HIDDEN_CACHE: usize = 10_000;

/// 当前用户的用户目录 (C:\Users\<名字>).
pub fn profile() -> PathBuf {
    recent::known_folder(&FOLDERID_Profile)
}

/// 本地硬盘的根目录 (C:\, D:\ ...), 不含 U 盘, 光驱和网络盘.
pub fn fixed_drives() -> Vec<PathBuf> {
    let mask = unsafe { GetLogicalDrives() };
    (0..26u8)
        .filter(|i| mask & (1 << i) != 0)
        .map(|i| PathBuf::from(format!("{}:\\", (b'A' + i) as char)))
        .filter(|root| unsafe { GetDriveTypeW(PCWSTR(wide(root).as_ptr())) } == DRIVE_FIXED)
        .collect()
}

/// 正在运行的进程 ID.
pub fn process_ids() -> Vec<u32> {
    let mut ids = vec![0u32; 4096];
    loop {
        let bytes = (ids.len() * size_of::<u32>()) as u32;
        let mut needed = 0;
        unsafe { EnumProcesses(ids.as_mut_ptr(), bytes, &mut needed) }.expect("EnumProcesses");
        // 写满了说明可能没放下, 加大再取
        if needed < bytes {
            ids.truncate(needed as usize / size_of::<u32>());
            return ids;
        }
        ids.resize(ids.len() * 2, 0);
    }
}

/// 和 QRun 在同一个登录会话里的进程的 exe 路径; 别的会话 (服务, 其他用户) 或没有权限的为 None.
pub fn session_program(pid: u32) -> Option<PathBuf> {
    let session = |pid| {
        let mut id = 0;
        unsafe { ProcessIdToSessionId(pid, &mut id) }.ok().map(|_| id)
    };
    if session(pid)? != session(std::process::id())? {
        return None;
    }
    context::program(pid)
}

/// 正在监听的 TCP 端口和监听它的进程 ID (IPv4 和 IPv6), 去重.
pub fn listening_ports() -> Vec<(u16, u32)> {
    let mut ports = Vec::new();
    let v4 = tcp_table(AF_INET);
    if !v4.is_empty() {
        let table = unsafe { &*v4.as_ptr().cast::<MIB_TCPTABLE_OWNER_PID>() };
        let rows = unsafe { std::slice::from_raw_parts(table.table.as_ptr(), table.dwNumEntries as usize) };
        ports.extend(rows.iter().map(|row| (local_port(row.dwLocalPort), row.dwOwningPid)));
    }
    let v6 = tcp_table(AF_INET6);
    if !v6.is_empty() {
        let table = unsafe { &*v6.as_ptr().cast::<MIB_TCP6TABLE_OWNER_PID>() };
        let rows = unsafe { std::slice::from_raw_parts(table.table.as_ptr(), table.dwNumEntries as usize) };
        ports.extend(rows.iter().map(|row| (local_port(row.dwLocalPort), row.dwOwningPid)));
    }
    ports.sort_unstable();
    ports.dedup();
    ports
}

/// 一个地址族的监听表; 按 u32 分配, 表里的结构才对齐. 取不到时为空, 写日志.
fn tcp_table(family: u32) -> Vec<u32> {
    let mut size = 0;
    loop {
        let mut buffer = vec![0u32; (size as usize).div_ceil(size_of::<u32>())];
        let result = unsafe {
            GetExtendedTcpTable(
                Some(buffer.as_mut_ptr().cast()),
                &mut size,
                false,
                family,
                TCP_TABLE_OWNER_PID_LISTENER,
                0,
            )
        };
        // 第一次 size 为 0 必然不够, 拿到要的大小再取; 两次之间新开了端口也会再来一轮
        if result == NO_ERROR.0 {
            return buffer;
        }
        if result != ERROR_INSUFFICIENT_BUFFER.0 {
            log::warn!("读取监听端口失败: 错误码 {result}");
            return Vec::new();
        }
    }
}

/// 端口号在低 16 位, 网络字节序.
fn local_port(raw: u32) -> u16 {
    u16::from_be(raw as u16)
}

#[derive(Clone, Copy)]
pub enum FileChange {
    Created,
    Modified,
    Removed,
    Renamed,
}

impl FileChange {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Modified => "modified",
            Self::Removed => "removed",
            Self::Renamed => "renamed",
        }
    }
}

/// 一个监听线程: 停止标志和线程本身.
struct Watch {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

/// 递归监听几个目录的文件改动, 每个目录一个后台线程; drop 时停止. 隐藏和系统目录 (AppData, .git, 回收站) 里的改动不报.
pub struct FileWatcher {
    roots: Vec<PathBuf>,
    watches: Vec<Watch>,
    changes: Receiver<(usize, FileChange, PathBuf)>,
}

impl FileWatcher {
    /// 打不开的目录跳过, 写日志.
    pub fn new(roots: Vec<PathBuf>) -> Self {
        let (sender, changes) = mpsc::channel();
        let mut kept = Vec::new();
        let mut watches = Vec::new();
        for root in roots {
            let opened = unsafe {
                CreateFileW(
                    PCWSTR(wide(&root).as_ptr()),
                    FILE_LIST_DIRECTORY.0,
                    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                    None,
                    OPEN_EXISTING,
                    FILE_FLAG_BACKUP_SEMANTICS,
                    None,
                )
            };
            let handle = match opened {
                Ok(handle) => handle,
                Err(e) => {
                    log::warn!("监听 {} 失败: {e}", root.display());
                    continue;
                }
            };
            let stop = Arc::new(AtomicBool::new(false));
            // HANDLE 不能跨线程传, 传数值
            let (raw, ix, dir, flag, sender) =
                (handle.0 as isize, kept.len(), root.clone(), stop.clone(), sender.clone());
            let thread = thread::spawn(move || {
                let handle = HANDLE(raw as *mut _);
                read_changes(handle, &dir, ix, &flag, &sender);
                unsafe { CloseHandle(handle) }.expect("close watched directory");
            });
            kept.push(root);
            watches.push(Watch { stop, thread });
        }
        Self { roots: kept, watches, changes }
    }

    /// 上次取过之后的改动: (所在的监听目录, 改动, 路径).
    pub fn take(&self) -> Vec<(PathBuf, FileChange, PathBuf)> {
        self.changes.try_iter().map(|(ix, change, path)| (self.roots[ix].clone(), change, path)).collect()
    }
}

impl Drop for FileWatcher {
    /// 线程阻塞在 ReadDirectoryChangesW 里, 要取消它的同步读取才会醒; 取消时它可能正好不在读 (刚处理完一批),
    /// 所以先立停止标志, 再反复取消直到线程结束.
    fn drop(&mut self) {
        for watch in self.watches.drain(..) {
            watch.stop.store(true, Ordering::Relaxed);
            while !watch.thread.is_finished() {
                let _ = unsafe { CancelSynchronousIo(HANDLE(watch.thread.as_raw_handle())) };
                thread::sleep(Duration::from_millis(1));
            }
            watch.thread.join().expect("join file watcher");
        }
    }
}

/// 监听线程: 读一批改动, 去掉隐藏和系统目录里的, 发给主线程; 停止或目录没了时返回.
fn read_changes(
    handle: HANDLE,
    root: &Path,
    ix: usize,
    stop: &AtomicBool,
    sender: &Sender<(usize, FileChange, PathBuf)>,
) {
    // 64 KB, 按 u32 分配以对齐 FILE_NOTIFY_INFORMATION
    let mut buffer = vec![0u32; 16 * 1024];
    let mut hidden: HashMap<PathBuf, bool> = HashMap::new();
    while !stop.load(Ordering::Relaxed) {
        let mut bytes = 0;
        let read = unsafe {
            ReadDirectoryChangesW(
                handle,
                buffer.as_mut_ptr().cast(),
                (buffer.len() * size_of::<u32>()) as u32,
                true,
                FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_DIR_NAME | FILE_NOTIFY_CHANGE_LAST_WRITE,
                Some(&mut bytes),
                None,
                None,
            )
        };
        if read.is_err() {
            return;
        }
        if bytes == 0 {
            log::warn!("{} 改动太多, 缓冲区放不下, 这一批没记", root.display());
            continue;
        }
        if hidden.len() > HIDDEN_CACHE {
            hidden.clear();
        }
        let mut offset = 0;
        loop {
            let info = unsafe { &*buffer.as_ptr().cast::<u8>().add(offset).cast::<FILE_NOTIFY_INFORMATION>() };
            let name = unsafe {
                std::slice::from_raw_parts(info.FileName.as_ptr(), info.FileNameLength as usize / size_of::<u16>())
            };
            let change = match info.Action {
                FILE_ACTION_ADDED => Some(FileChange::Created),
                FILE_ACTION_MODIFIED => Some(FileChange::Modified),
                FILE_ACTION_REMOVED => Some(FileChange::Removed),
                FILE_ACTION_RENAMED_NEW_NAME => Some(FileChange::Renamed),
                // 改名的旧名字不报, 新名字已说明了
                _ => None,
            };
            let path = root.join(String::from_utf16_lossy(name));
            if let Some(change) = change
                && !in_hidden(root, &path, &mut hidden)
                && sender.send((ix, change, path)).is_err()
            {
                return;
            }
            if info.NextEntryOffset == 0 {
                break;
            }
            offset += info.NextEntryOffset as usize;
        }
    }
}

/// 路径本身或它在 root 之下的某一级目录是隐藏或系统的. 目录的结果缓存起来, 同一目录频繁改动时不反复查.
fn in_hidden(root: &Path, path: &Path, cache: &mut HashMap<PathBuf, bool>) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };
    let mut dir = root.to_path_buf();
    let mut parts = relative.components().peekable();
    while let Some(part) = parts.next() {
        dir.push(part);
        // 最后一级是文件 (或刚删掉的), 不缓存
        if parts.peek().is_none() {
            return is_hidden(&dir);
        }
        if *cache.entry(dir.clone()).or_insert_with(|| is_hidden(&dir)) {
            return true;
        }
    }
    false
}

/// 隐藏或系统属性; 已不存在的算不是.
fn is_hidden(path: &Path) -> bool {
    let attributes = unsafe { GetFileAttributesW(PCWSTR(wide(path).as_ptr())) };
    attributes != INVALID_FILE_ATTRIBUTES && attributes & (FILE_ATTRIBUTE_HIDDEN.0 | FILE_ATTRIBUTE_SYSTEM.0) != 0
}
