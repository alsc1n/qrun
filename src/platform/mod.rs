//! Windows 原生能力的薄封装, 不涉及 QRun 的业务数据.

pub mod autostart;
pub mod env;
pub mod http;
pub mod install;
pub mod shell;
pub mod tray;
pub mod window;

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
use windows::core::{PCWSTR, w};

/// 转成 Win32 要的以 0 结尾的 UTF-16.
pub fn wide(s: impl AsRef<OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(Some(0)).collect()
}

/// 本用户会话里是不是第一个 QRun: 占住一个命名互斥量, 已被别的 QRun 占着就返回 false.
/// 互斥量句柄不关, 进程退出时系统释放, 之后再启动的 QRun 又能占到.
pub fn first_instance() -> bool {
    unsafe {
        CreateMutexW(None, false, w!("Local\\QRun")).expect("create instance mutex");
        GetLastError() != ERROR_ALREADY_EXISTS
    }
}

/// 本地时间, 形如 `2026-11-05 14:03:21.123`.
pub fn local_time() -> String {
    let t = unsafe { GetLocalTime() };
    format!(
        "{}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
    )
}

/// 程序没有控制台, 出错时用系统消息框告诉用户.
pub fn error_box(message: &str) {
    let message = wide(message);
    unsafe { MessageBoxW(None, PCWSTR(message.as_ptr()), w!("QRun"), MB_ICONERROR | MB_OK) };
}
