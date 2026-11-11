//! 当前情境: 前台窗口 (程序和标题), 开着的窗口, 剪贴板里是什么类型, 人是否离开. 预测用, 都在当前用户范围内.
//! 读不到的 (管理员权限的进程, 没有标题的窗口) 就当没有, 不报错.

use std::path::PathBuf;
use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM};
use windows::Win32::System::DataExchange::{
    GetClipboardSequenceNumber, IsClipboardFormatAvailable, RegisterClipboardFormatW,
};
use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
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
