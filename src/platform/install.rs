//! 安装方式: 区分安装包装的和绿色版, 以及静默运行新版安装包.

use std::path::Path;
use std::process::Command;

use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_SZ, RegGetValueW};
use windows::core::{PCWSTR, w};

use super::wide;

/// 当前 exe 是不是安装包装的: Inno Setup 在卸载信息里记了安装目录, 和 exe 所在目录一致才算.
/// 绿色版, 开发构建, 或从安装目录拷走的 exe 都不算.
pub fn installed(app_id: &str) -> bool {
    let key = wide(format!("Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\{app_id}_is1"));
    let mut buffer = [0u16; 1024];
    let mut size = size_of_val(&buffer) as u32;
    let result = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(key.as_ptr()),
            w!("InstallLocation"),
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr() as *mut _),
            Some(&mut size),
        )
    };
    if result != ERROR_SUCCESS {
        return false;
    }
    // size 是字节数, 含结尾的 0; Inno 记录的目录末尾带反斜杠
    let location = String::from_utf16_lossy(&buffer[..size as usize / 2 - 1]);
    let exe = std::env::current_exe().expect("locate exe");
    let dir = exe.parent().expect("exe dir").to_string_lossy().into_owned();
    location.trim_end_matches('\\').eq_ignore_ascii_case(dir.trim_end_matches('\\'))
}

/// 静默运行安装包: 只显示进度条, 装回原目录 (Inno 按 AppId 沿用上次的目录), 装完由安装包重新启动 QRun.
pub fn run_installer(setup: &Path) -> Result<(), String> {
    Command::new(setup)
        .args(["/SILENT", "/SUPPRESSMSGBOXES", "/NORESTART"])
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("启动安装包失败: {e}"))
}
