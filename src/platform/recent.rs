//! 系统最近记录: 资源管理器 Recent 目录和各程序跳转列表里的文件和文件夹, UserAssist 里的程序.
//! 都是用户打开时系统记下的, 在当前用户范围内, 不要管理员权限. Recent 记不到程序自带最近列表里打开的文件, 由跳转列表补上;
//! UserAssist 只记从开始菜单, 任务栏, 桌面, 资源管理器启动的程序, 命令行或别的程序拉起的不记.

use std::collections::HashMap;
use std::fs;
use std::io::{Cursor, Read};
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use windows::Win32::Foundation::{ERROR_NO_MORE_ITEMS, ERROR_SUCCESS};
use windows::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoTaskMemFree, CoUninitialize,
};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, RRF_RT_REG_DWORD, RegCloseKey, RegEnumValueW, RegGetValueW, RegOpenKeyExW,
};
use windows::Win32::UI::Shell::{
    FOLDERID_Desktop, FOLDERID_Downloads, FOLDERID_Recent, KF_FLAG_DEFAULT, SHGetKnownFolderPath,
};
use windows::core::{GUID, PCWSTR, PWSTR, w};

use super::{shell, wide};

/// UserAssist 下两类记录: 直接运行的 exe 和通过快捷方式启动的.
const USER_ASSIST: [&str; 2] = [
    "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\UserAssist\\{CEBFF5CD-ACE2-4F4F-9178-9926F41749EA}\\Count",
    "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\UserAssist\\{F4E57C4B-2036-45F0-A9AB-443BCFE33D9F}\\Count",
];
/// 两个系统开关所在的键; 值不存在时系统按开启处理.
const ADVANCED: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Advanced");
/// UserAssist 值数据的长度和其中最后运行时间 (FILETIME) 的偏移.
const USER_ASSIST_LEN: usize = 72;
const LAST_RUN_OFFSET: usize = 60;
/// FILETIME (1601 年起的 100 纳秒) 与 Unix 毫秒的换算.
const FILETIME_UNIX_EPOCH: u64 = 116_444_736_000_000_000;
/// 跳转列表所在的 Recent 子目录和其中记录打开历史的流.
const JUMP_LISTS: &str = "AutomaticDestinations";
const DEST_LIST: &str = "DestList";
/// DestList 的格式 (版本 3 及以上): 32 字节表头 (版本, 条数...), 之后每条在偏移 100 处是最后打开时间 (FILETIME),
/// 128 处是路径长度 (UTF-16 字符数), 紧跟路径, 再跟 4 字节.
const DEST_LIST_MIN_VERSION: u32 = 3;
const DEST_LIST_HEADER: usize = 32;
const DEST_LAST_OPENED: usize = 100;
const DEST_PATH_LEN: usize = 128;
const DEST_ENTRY_TAIL: usize = 4;

pub struct Recent {
    pub files: Vec<RecentEntry>,
    pub folders: Vec<RecentEntry>,
    pub programs: Vec<RecentEntry>,
    /// 下载文件夹和桌面上最近出现的文件, at 是修改时间; 还没打开过, 只当候选.
    pub new_files: Vec<RecentEntry>,
    /// 系统设置里 "显示最近打开的项目" 是否开着; 关着时 Recent 目录不再更新.
    pub tracks_documents: bool,
    /// 系统设置里 "允许 Windows 跟踪应用启动" 是否开着; 关着时 UserAssist 不再更新.
    pub tracks_programs: bool,
}

/// 一条最近记录, at 是最后一次打开的时间 (Unix 毫秒). 各列表按 at 从新到旧排.
pub struct RecentEntry {
    pub name: String,
    pub target: PathBuf,
    pub at: i64,
}

/// 读一遍系统最近记录; new_since 之后 (Unix 毫秒) 修改的下载和桌面文件算新文件.
/// system 为 false 时不读最近记录和跳转列表, new_since 为 None 时不扫新文件 (收集里关掉了), 对应几项为空.
/// 要解析几百个快捷方式, 在后台线程调用; 自己初始化 COM.
pub fn read(system: bool, new_since: Option<i64>) -> Recent {
    // 后台线程池的线程会复用: 已按别的模式初始化过时返回 RPC_E_CHANGED_MODE, COM 照样可用, 只是不归这里反初始化
    let initialized = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) }.is_ok();
    let (files, folders) = if system { documents() } else { (Vec::new(), Vec::new()) };
    let recent = Recent {
        files,
        folders,
        programs: if system { programs() } else { Vec::new() },
        new_files: new_since.map(new_files).unwrap_or_default(),
        tracks_documents: switch_on(w!("Start_TrackDocs")),
        tracks_programs: switch_on(w!("Start_TrackProgs")),
    };
    if initialized {
        unsafe { CoUninitialize() };
    }
    recent
}

/// 用户打开过的文件和文件夹: Recent 目录加上各程序的跳转列表, 同一目标留最近一次. 目标已不存在的跳过.
fn documents() -> (Vec<RecentEntry>, Vec<RecentEntry>) {
    let mut opened: HashMap<String, (PathBuf, i64)> = HashMap::new();
    for (target, at) in recent_links().into_iter().chain(jump_lists()) {
        let key = target.to_string_lossy().to_lowercase();
        if opened.get(&key).is_none_or(|&(_, seen)| seen < at) {
            opened.insert(key, (target, at));
        }
    }
    let (mut files, mut folders) = (Vec::new(), Vec::new());
    for (target, at) in opened.into_values() {
        let name = target.file_name().unwrap_or(target.as_os_str()).to_string_lossy().into_owned();
        let folder = target.is_dir();
        let entry = RecentEntry { name, target, at };
        if folder { folders.push(entry) } else { files.push(entry) }
    }
    files.sort_by_key(|e| std::cmp::Reverse(e.at));
    folders.sort_by_key(|e| std::cmp::Reverse(e.at));
    (files, folders)
}

/// Recent 目录里每个快捷方式对应一个打开过的文件或文件夹, 快捷方式的修改时间就是最后打开的时间.
/// 资源管理器里双击打开的, 和从程序的打开/保存对话框打开的会记在这里.
fn recent_links() -> Vec<(PathBuf, i64)> {
    let Ok(entries) = fs::read_dir(known_folder(&FOLDERID_Recent)) else {
        return Vec::new();
    };
    let mut links = Vec::new();
    for entry in entries.flatten() {
        let link = entry.path();
        if !shell::is_link(&link) {
            continue;
        }
        let (Some(shortcut), Ok(modified)) = (shell::read_shortcut(&link), entry.metadata().and_then(|m| m.modified()))
        else {
            continue;
        };
        links.push((
            shortcut.target,
            modified.duration_since(UNIX_EPOCH).expect("file time after 1970").as_millis() as i64,
        ));
    }
    links
}

/// 跳转列表 (任务栏图标右键里的"最近"): 每个程序一份 `Recent\AutomaticDestinations\*.automaticDestinations-ms`.
/// 程序在自己的最近列表, 标签页里打开的文件 (Office, Notepad++, 编辑器等) 记在这里, Recent 目录里没有.
fn jump_lists() -> Vec<(PathBuf, i64)> {
    let Ok(entries) = fs::read_dir(known_folder(&FOLDERID_Recent).join(JUMP_LISTS)) else {
        return Vec::new();
    };
    entries.flatten().filter_map(|entry| fs::read(entry.path()).ok()).flat_map(dest_list).collect()
}

/// 跳转列表是复合文档, 其中 DestList 流按条记着打开过的路径和最后打开时间 (格式没有官方文档, 按 Windows 10 起的
/// 版本 3 及以上读). 读不了, 版本更早或数据不完整的跳过; 不是本地路径的 (应用内部 ID, ms-settings: 等) 和已不存在的不要.
fn dest_list(bytes: Vec<u8>) -> Vec<(PathBuf, i64)> {
    let mut data = Vec::new();
    let read = cfb::CompoundFile::open(Cursor::new(bytes))
        .and_then(|mut file| file.open_stream(DEST_LIST)?.read_to_end(&mut data));
    if read.is_err() {
        return Vec::new();
    }
    let bytes = |at: usize, len: usize| data.get(at..at + len);
    let u32_at = |at: usize| bytes(at, 4).map(|b| u32::from_le_bytes(b.try_into().expect("4 bytes")));
    let (Some(version), Some(count)) = (u32_at(0), u32_at(4)) else {
        return Vec::new();
    };
    if version < DEST_LIST_MIN_VERSION {
        return Vec::new();
    }
    let mut list = Vec::new();
    let mut at = DEST_LIST_HEADER;
    for _ in 0..count {
        let filetime = bytes(at + DEST_LAST_OPENED, 8).map(|b| u64::from_le_bytes(b.try_into().expect("8 bytes")));
        let len = bytes(at + DEST_PATH_LEN, 2).map(|b| u16::from_le_bytes(b.try_into().expect("2 bytes")) as usize);
        let (Some(filetime), Some(len)) = (filetime, len) else {
            break;
        };
        let start = at + DEST_PATH_LEN + 2;
        let Some(raw) = bytes(start, len * 2) else {
            break;
        };
        let units: Vec<u16> = raw.as_chunks::<2>().0.iter().map(|&c| u16::from_le_bytes(c)).collect();
        let path = PathBuf::from(String::from_utf16_lossy(&units));
        if path.is_absolute() && filetime > FILETIME_UNIX_EPOCH && path.exists() {
            list.push((path, unix_ms(filetime)));
        }
        at = start + len * 2 + DEST_ENTRY_TAIL;
    }
    list
}

fn unix_ms(filetime: u64) -> i64 {
    ((filetime - FILETIME_UNIX_EPOCH) / 10_000) as i64
}

/// 下载文件夹和桌面上 since 之后修改的文件 (不进子目录). 快捷方式, 系统文件, 还没下完的临时文件不算;
/// 隐藏文件和 . 或 ~ 开头的文件也不算: 文档打开时办公软件旁边生成的锁文件 (LibreOffice / WPS 的 .~lock.xxx#,
/// Office 的 ~$xxx) 都是这样.
pub fn new_files(since: i64) -> Vec<RecentEntry> {
    const SKIP: [&str; 6] = ["lnk", "ini", "tmp", "crdownload", "part", "download"];
    const HIDDEN_OR_SYSTEM: u32 = 0x2 | 0x4;
    let mut files = Vec::new();
    for folder in [&FOLDERID_Downloads, &FOLDERID_Desktop] {
        let Ok(entries) = fs::read_dir(known_folder(folder)) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            let skipped =
                path.extension().is_some_and(|ext| SKIP.contains(&ext.to_string_lossy().to_lowercase().as_str()));
            let temporary = entry.file_name().to_string_lossy().starts_with(['.', '~'])
                || metadata.file_attributes() & HIDDEN_OR_SYSTEM != 0;
            if !metadata.is_file() || skipped || temporary {
                continue;
            }
            let Ok(modified) = metadata.modified() else {
                continue;
            };
            let at = modified.duration_since(UNIX_EPOCH).expect("file time after 1970").as_millis() as i64;
            if at >= since {
                let name = path.file_name().unwrap_or(path.as_os_str()).to_string_lossy().into_owned();
                files.push(RecentEntry { name, target: path, at });
            }
        }
    }
    files.sort_by_key(|e| std::cmp::Reverse(e.at));
    files
}

/// UserAssist 的值名是 ROT13 编码的程序路径或应用 ID, 值数据里有最后运行时间.
/// 路径开头的 `{已知文件夹 GUID}` 换成真实目录; 应用 ID 只留"所有应用"里有的, 名称也从那里取.
fn programs() -> Vec<RecentEntry> {
    let apps: HashMap<String, String> = shell::installed_apps()
        .into_iter()
        .map(|app| (app.target.to_string_lossy().to_lowercase(), app.name))
        .collect();
    let mut seen = HashMap::new();
    for key in USER_ASSIST {
        for (name, data) in values(key) {
            if data.len() < USER_ASSIST_LEN {
                continue;
            }
            let filetime = u64::from_le_bytes(data[LAST_RUN_OFFSET..LAST_RUN_OFFSET + 8].try_into().expect("8 bytes"));
            // 从没运行过的记录时间为 0
            if filetime <= FILETIME_UNIX_EPOCH {
                continue;
            }
            let at = unix_ms(filetime);
            let decoded = rot13(&name);
            let Some((name, target)) = program(&decoded, &apps) else {
                continue;
            };
            // 同一程序可能同时有 exe 和快捷方式两条记录, 只留最近一次
            let key = target.to_string_lossy().to_lowercase();
            let newer = seen.get(&key).is_none_or(|e: &RecentEntry| e.at < at);
            if newer {
                seen.insert(key, RecentEntry { name, target, at });
            }
        }
    }
    let mut programs: Vec<RecentEntry> = seen.into_values().collect();
    programs.sort_by_key(|e| std::cmp::Reverse(e.at));
    programs
}

/// 解码后的值名换成名称和可启动的目标; 已不存在的程序和 UserAssist 的内部记录返回 None.
fn program(decoded: &str, apps: &HashMap<String, String>) -> Option<(String, PathBuf)> {
    if !decoded.contains('\\') {
        let target = format!("shell:AppsFolder\\{decoded}");
        let name = apps.get(&target.to_lowercase())?.clone();
        return Some((name, PathBuf::from(target)));
    }
    let path = match decoded.strip_prefix('{').and_then(|rest| rest.split_once("}\\")) {
        Some((guid, rest)) => known_folder(&GUID::try_from(guid).ok()?).join(rest),
        None => PathBuf::from(decoded),
    };
    let ext = path.extension()?.to_string_lossy().to_lowercase();
    if !(ext == "exe" || ext == "lnk") || !path.exists() {
        return None;
    }
    let name = path.file_stem()?.to_string_lossy().into_owned();
    Some((name, path))
}

fn rot13(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            'a'..='z' => (((c as u8 - b'a' + 13) % 26) + b'a') as char,
            'A'..='Z' => (((c as u8 - b'A' + 13) % 26) + b'A') as char,
            _ => c,
        })
        .collect()
}

/// 键下所有值的名称和数据; 键不存在时为空.
fn values(key: &str) -> Vec<(String, Vec<u8>)> {
    let key = wide(key);
    let mut hkey = HKEY::default();
    let mut values = Vec::new();
    unsafe {
        if RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(key.as_ptr()), None, KEY_READ, &mut hkey) != ERROR_SUCCESS {
            return values;
        }
        let mut name = [0u16; 1024];
        let mut data = [0u8; 1024];
        for index in 0.. {
            let mut name_len = name.len() as u32;
            let mut data_len = data.len() as u32;
            let result = RegEnumValueW(
                hkey,
                index,
                Some(PWSTR(name.as_mut_ptr())),
                &mut name_len,
                None,
                None,
                Some(data.as_mut_ptr()),
                Some(&mut data_len),
            );
            if result == ERROR_NO_MORE_ITEMS {
                break;
            }
            // 名称或数据超长的记录不是程序启动记录, 跳过
            if result != ERROR_SUCCESS {
                continue;
            }
            values.push((String::from_utf16_lossy(&name[..name_len as usize]), data[..data_len as usize].to_vec()));
        }
        let _ = RegCloseKey(hkey);
    }
    values
}

fn switch_on(value: PCWSTR) -> bool {
    let mut data = 0u32;
    let mut size = size_of::<u32>() as u32;
    let result = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            ADVANCED,
            value,
            RRF_RT_REG_DWORD,
            None,
            Some(&mut data as *mut u32 as *mut _),
            Some(&mut size),
        )
    };
    result != ERROR_SUCCESS || data != 0
}

pub(super) fn known_folder(id: &GUID) -> PathBuf {
    unsafe {
        let Ok(path) = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None) else {
            // 本机没有这个已知文件夹 (如 32 位系统的 ProgramFilesX86), 拼出的路径不存在, 随后被当作程序已删除跳过
            return PathBuf::new();
        };
        let text = path.to_string().expect("known folder path is UTF-16");
        CoTaskMemFree(Some(path.0 as *const _));
        Path::new(&text).to_path_buf()
    }
}
