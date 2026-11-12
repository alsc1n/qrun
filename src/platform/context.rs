//! 当前情境: 前台窗口 (程序和标题), 开着的窗口, 剪贴板 (类型, 来源程序, 文字开头, 文件扩展名), 人是否离开.
//! 预测用, 都在当前用户范围内; 读到的文字由调用方马上脱敏 (model::redact).
//! 读不到的 (管理员权限的进程, 没有标题的窗口) 就当没有, 不报错.

use std::path::PathBuf;
use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, HGLOBAL, HWND, LPARAM};
use windows::Win32::System::DataExchange::{
    CloseClipboard, GetClipboardData, GetClipboardOwner, GetClipboardSequenceNumber, IsClipboardFormatAvailable,
    OpenClipboard, RegisterClipboardFormatW,
};
use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GW_OWNER, GetForegroundWindow, GetWindow, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible,
};
use windows::core::{BOOL, PWSTR, w};

/// 剪贴板标准格式的编号 (WinUser.h), 只为这三个不必引入整个 Ole 模块.
const CF_UNICODETEXT: u32 = 13;
const CF_DIB: u32 = 8;
const CF_HDROP: u32 = 15;

/// 一个顶层窗口所属的程序和标题. 程序路径读不到 (如管理员权限的进程) 时为 None.
pub struct WindowInfo {
    pub program: Option<PathBuf>,
    pub title: String,
}

/// 前台窗口; 没有 (如锁屏时) 为 None. 只取句柄, 定时轮询用.
pub fn foreground() -> Option<HWND> {
    let hwnd = unsafe { GetForegroundWindow() };
    (!hwnd.is_invalid()).then_some(hwnd)
}

/// 窗口所属的程序和现在的标题.
pub fn window_info(hwnd: HWND) -> WindowInfo {
    WindowInfo { program: program(process_id(hwnd)), title: title(hwnd) }
}

pub fn process_id(hwnd: HWND) -> u32 {
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    pid
}

/// 可见的主窗口 (有标题, 不从属于别的窗口), 按 Z 序从上到下.
pub fn windows() -> Vec<WindowInfo> {
    let mut windows: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(collect), LPARAM(&mut windows as *mut Vec<HWND> as isize));
    }
    windows
        .into_iter()
        .filter(|&hwnd| unsafe { IsWindowVisible(hwnd) }.as_bool())
        .filter(|&hwnd| unsafe { GetWindow(hwnd, GW_OWNER) }.is_err())
        .map(window_info)
        .filter(|window| !window.title.is_empty())
        .collect()
}

unsafe extern "system" fn collect(hwnd: HWND, windows: LPARAM) -> BOOL {
    unsafe { (*(windows.0 as *mut Vec<HWND>)).push(hwnd) };
    true.into()
}

/// 剪贴板里有什么: 文件, 图片, 网页内容 (从浏览器或 Office 复制的带格式内容), 文字; 只看格式, 不读内容. 空时为 None.
pub fn clipboard_kind() -> Option<&'static str> {
    let has = |format: u32| unsafe { IsClipboardFormatAvailable(format) }.is_ok();
    let html = unsafe { RegisterClipboardFormatW(w!("HTML Format")) };
    if has(CF_HDROP) {
        Some("files")
    } else if has(CF_DIB) {
        Some("image")
    } else if html != 0 && has(html) {
        Some("rich text")
    } else if has(CF_UNICODETEXT) {
        Some("text")
    } else {
        None
    }
}

/// 一次复制: 类型, 从哪个程序复制的, 文字的开头 (原文, 调用方马上脱敏), 文件的扩展名.
/// 受保护的 (密码管理器复制的密码) 只有类型和来源, 不读内容.
pub struct Clip {
    pub kind: &'static str,
    pub owner: Option<PathBuf>,
    pub protected: bool,
    pub text: Option<String>,
    pub extensions: Vec<String>,
}

/// 剪贴板正被别的程序占着, 稍后再读.
pub struct ClipboardBusy;

/// 读剪贴板; 空时为 None. 文字只读开头 max_chars 个字符.
pub fn read_clipboard(max_chars: usize) -> Result<Option<Clip>, ClipboardBusy> {
    let Some(kind) = clipboard_kind() else {
        return Ok(None);
    };
    let owner = unsafe { GetClipboardOwner() }.ok().and_then(|hwnd| program(process_id(hwnd)));
    unsafe { OpenClipboard(None) }.map_err(|_| ClipboardBusy)?;
    let protected = protected();
    let text = (!protected && matches!(kind, "text" | "rich text")).then(|| text(max_chars)).flatten();
    let extensions = if kind == "files" && !protected { extensions() } else { Vec::new() };
    let _ = unsafe { CloseClipboard() };
    Ok(Some(Clip { kind, owner, protected, text, extensions }))
}

/// 复制方要求别的程序不要记录这次内容: 密码管理器 (KeePass, 1Password, Bitwarden) 复制密码时会带, Win+V 历史也认.
/// 有排除标记, 或 "可进剪贴板历史" / "可同步到云剪贴板" 为 0. 剪贴板须已打开.
fn protected() -> bool {
    let format = |name| unsafe { RegisterClipboardFormatW(name) };
    if unsafe { IsClipboardFormatAvailable(format(w!("ExcludeClipboardContentFromMonitorProcessing"))) }.is_ok() {
        return true;
    }
    [w!("CanIncludeInClipboardHistory"), w!("CanUploadToCloudClipboard")]
        .into_iter()
        .any(|name| with_data(format(name), |bytes| bytes.get(..4) == Some(&[0, 0, 0, 0])).unwrap_or(false))
}

/// 文字的开头 max_chars 个字符. 剪贴板须已打开.
fn text(max_chars: usize) -> Option<String> {
    with_data(CF_UNICODETEXT, |bytes| {
        let units: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&pair| u16::from_le_bytes(pair))
            .take_while(|&unit| unit != 0)
            // 复制了整篇文档时不必全部转换; 两个 UTF-16 单元足够放下一个字符
            .take(max_chars * 2)
            .collect();
        String::from_utf16_lossy(&units).chars().take(max_chars).collect()
    })
}

/// 复制的各文件的扩展名 (小写, 文件夹和没有扩展名的为空). 剪贴板须已打开.
fn extensions() -> Vec<String> {
    let Ok(handle) = (unsafe { GetClipboardData(CF_HDROP) }) else {
        return Vec::new();
    };
    let drop = HDROP(handle.0);
    let count = unsafe { DragQueryFileW(drop, u32::MAX, None) };
    (0..count)
        .map(|ix| {
            let mut buffer = vec![0u16; unsafe { DragQueryFileW(drop, ix, None) } as usize + 1];
            let len = unsafe { DragQueryFileW(drop, ix, Some(&mut buffer)) } as usize;
            let path = PathBuf::from(String::from_utf16_lossy(&buffer[..len]));
            path.extension().map_or(String::new(), |ext| ext.to_string_lossy().to_lowercase())
        })
        .collect()
}

/// 按格式取剪贴板里的一块内存交给 read; 没有这种格式时为 None. 剪贴板须已打开.
fn with_data<T>(format: u32, read: impl FnOnce(&[u8]) -> T) -> Option<T> {
    let handle = unsafe { GetClipboardData(format) }.ok()?;
    let memory = HGLOBAL(handle.0);
    let data = unsafe { GlobalLock(memory) };
    if data.is_null() {
        return None;
    }
    let bytes = unsafe { std::slice::from_raw_parts(data as *const u8, GlobalSize(memory)) };
    let result = read(bytes);
    let _ = unsafe { GlobalUnlock(memory) };
    Some(result)
}

/// 剪贴板内容的版本号, 每换一次内容加一; 用来发现复制了新东西, 不读内容.
pub fn clipboard_sequence() -> u32 {
    unsafe { GetClipboardSequenceNumber() }
}

/// 距最后一次键盘鼠标输入多久.
pub fn idle() -> Duration {
    let mut info = LASTINPUTINFO { cbSize: size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
    unsafe { GetLastInputInfo(&mut info) }.expect("GetLastInputInfo");
    // 两个都是开机以来的毫秒数 (32 位, 约 49 天回绕一次), 用回绕减法
    Duration::from_millis(unsafe { GetTickCount() }.wrapping_sub(info.dwTime) as u64)
}

pub fn title(hwnd: HWND) -> String {
    let mut buffer = [0u16; 512];
    let len = unsafe { GetWindowTextW(hwnd, &mut buffer) };
    String::from_utf16_lossy(&buffer[..len.max(0) as usize])
}

/// 进程的 exe 路径; 没有权限打开的进程返回 None.
pub fn program(pid: u32) -> Option<PathBuf> {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buffer = [0u16; 1024];
        let mut size = buffer.len() as u32;
        let result = QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, PWSTR(buffer.as_mut_ptr()), &mut size);
        let _ = CloseHandle(process);
        result.ok()?;
        Some(PathBuf::from(String::from_utf16_lossy(&buffer[..size as usize])))
    }
}
