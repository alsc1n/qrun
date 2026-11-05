//! 进程环境变量: 换成当前用户的标准环境.

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Security::TOKEN_QUERY;
use windows::Win32::System::Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// 用当前用户的标准环境 (系统变量 + 用户变量) 替换本进程继承来的环境.
/// QRun 拉起的程序会继承 QRun 的环境, 不替换的话, 启动 QRun 的终端或工具临时设的变量
/// (如指向已关闭端口的 HTTPS_PROXY) 会一路传给用户启动的每个程序.
/// 启动时和每次拉起程序前各调一次, 用户在系统设置里改的环境变量不用重启 QRun 就生效.
pub fn reset_to_user_default() {
    let mut block = std::ptr::null_mut();
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).expect("open process token");
        CreateEnvironmentBlock(&mut block, Some(token), false).expect("create environment block");
        let _ = CloseHandle(token);
    }

    // 环境块是一串 "名=值\0", 以空串结尾
    let mut vars = Vec::new();
    let mut entry = block as *const u16;
    unsafe {
        loop {
            let len = (0..).take_while(|&i| *entry.add(i) != 0).count();
            if len == 0 {
                break;
            }
            let text = std::slice::from_raw_parts(entry, len);
            // 跳过首字符再找 '=', 兼容 "=C:=C:\" 这类以等号开头的名字
            let eq = 1 + text[1..].iter().position(|&c| c == '=' as u16).expect("environment entry has '='");
            vars.push((OsString::from_wide(&text[..eq]), OsString::from_wide(&text[eq + 1..])));
            entry = entry.add(len + 1);
        }
        let _ = DestroyEnvironmentBlock(block);
    }

    // Windows 上 std 经 SetEnvironmentVariableW 改环境, 系统在进程环境块上加锁, 多线程下也安全
    unsafe {
        for (key, _) in std::env::vars_os() {
            std::env::remove_var(key);
        }
        for (key, value) in vars {
            std::env::set_var(key, value);
        }
    }
}
