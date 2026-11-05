//! 开机自启: 当前用户注册表 Run 项. 只在用户打开开关时写入, 开关状态直接读注册表,
//! 这样用户在任务管理器里关掉, 或被安全软件删掉, QRun 都能如实显示.

use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
};
use windows::core::{PCWSTR, w};

use super::{error_box, wide};

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const VALUE_NAME: PCWSTR = w!("QRun");

#[derive(Clone, Copy, PartialEq)]
pub enum Autostart {
    Off,
    On,
    /// 登记的是别处的 exe, 一般是整个目录被挪走了, 原来的自启已经失效.
    Moved,
}

pub fn state() -> Autostart {
    let mut buffer = [0u16; 1024];
    let mut size = size_of_val(&buffer) as u32;
    let result = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            VALUE_NAME,
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr() as *mut _),
            Some(&mut size),
        )
    };
    if result != ERROR_SUCCESS {
        return Autostart::Off;
    }
    // size 是字节数, 含结尾的 0
    let value = String::from_utf16_lossy(&buffer[..size as usize / 2 - 1]);
    if value == command() { Autostart::On } else { Autostart::Moved }
}

pub fn set(enabled: bool) {
    let result = unsafe {
        if enabled {
            let command = wide(command());
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                RUN_KEY,
                VALUE_NAME,
                REG_SZ.0,
                Some(command.as_ptr() as *const _),
                (command.len() * 2) as u32,
            )
        } else {
            RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, VALUE_NAME)
        }
    };
    if result != ERROR_SUCCESS {
        error_box(&format!("设置开机自启失败 ({}), 可能被安全软件拦截.", result.0));
    }
}

/// 带引号的 exe 路径, 路径里有空格也能正确启动.
fn command() -> String {
    format!("\"{}\"", std::env::current_exe().expect("locate exe").display())
}
