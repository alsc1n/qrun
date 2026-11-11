//! 安装方式: 区分安装包装的和绿色版, 静默运行新版安装包, 安装版自己的注册表值 (HKCU\Software\QRun), 重启 QRun.

use std::path::{Path, PathBuf};
use std::process::Command;

use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
};
use windows::Win32::UI::Shell::FOLDERID_LocalAppData;
use windows::core::{PCWSTR, w};

use super::{recent, tray, wide};

/// QRun 自己的注册表键; 卸载时整个删掉 (installer/qrun.iss).
const APP_KEY: PCWSTR = w!("Software\\QRun");
/// 重启时带的参数: 新进程等旧的退出再启动, 不当成"再次启动"去显示旧的面板.
pub const RESTART_ARG: &str = "--restart";

/// 当前用户的本机应用数据目录 (%LOCALAPPDATA%), 不随漫游账户同步.
pub fn local_app_data() -> PathBuf {
    recent::known_folder(&FOLDERID_LocalAppData)
}

/// HKCU\Software\QRun 下的一个字符串值; 没有时为 None.
pub fn value(name: &str) -> Option<String> {
    let name = wide(name);
    let mut buffer = [0u16; 1024];
    let mut size = size_of_val(&buffer) as u32;
    let result = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            APP_KEY,
            PCWSTR(name.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr() as *mut _),
            Some(&mut size),
        )
    };
    // size 是字节数, 含结尾的 0
    (result == ERROR_SUCCESS).then(|| String::from_utf16_lossy(&buffer[..size as usize / 2 - 1]))
}

/// 写 HKCU\Software\QRun 下的字符串值, 键不存在时自动建.
pub fn set_value(name: &str, value: &str) -> Result<(), String> {
    let (name, value) = (wide(name), wide(value));
    let result = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            APP_KEY,
            PCWSTR(name.as_ptr()),
            REG_SZ.0,
            Some(value.as_ptr() as *const _),
            (value.len() * 2) as u32,
        )
    };
    if result == ERROR_SUCCESS { Ok(()) } else { Err(format!("写入注册表失败 ({})", result.0)) }
}

/// 删掉 HKCU\Software\QRun 下的值; 本来就没有也算成功.
pub fn delete_value(name: &str) {
    let name = wide(name);
    let _ = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, APP_KEY, PCWSTR(name.as_ptr())) };
}

/// 重启 QRun: 带 RESTART_ARG 拉起新进程, 自己照托盘"退出"处理 (先存时间线, 移除托盘图标).
pub fn restart() -> Result<(), String> {
    let exe = std::env::current_exe().expect("locate exe");
    Command::new(exe).arg(RESTART_ARG).spawn().map_err(|e| format!("重启 QRun 失败: {e}"))?;
    tray::request_quit();
    Ok(())
}

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
