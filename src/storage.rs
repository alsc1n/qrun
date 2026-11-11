//! 数据文件 (JSON 文件, SQLite 库, 日志) 放在哪, 以及它们的读写.
//! 安装版, 绿色版, 开发构建都默认放 %LOCALAPPDATA%\QRun, 可在设置里改到别处 (记在注册表), 所以同一用户的各个 QRun 用同一份数据.
//! 每个 JSON 文件顶层带 version, 库在 user_version 里记版本, 都是数据结构的版本. 有破坏性变更时按版本逐级迁移旧结构.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use rusqlite::{Connection, Transaction};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use crate::platform::{self, install};

pub const DB_FILE: &str = "qrun.db";
/// 数据文件名的开头: 设置, 库, 0.1.0 兼容文件, 日志, 连同它们的 .bak / .old / .tmp. 搬家时按它挑文件.
const DATA_FILES: [&str; 4] = ["settings.json", DB_FILE, "library.json", "qrun.log"];
/// 注册表值: 自选的数据目录; 更改后还没搬的旧目录 (重启时搬).
const DATA_DIR_VALUE: &str = "DataDir";
const MOVE_FROM_VALUE: &str = "MoveDataFrom";

static DIR: OnceLock<PathBuf> = OnceLock::new();

/// 启动时最先调用 (日志之前): 定下数据目录. 更改过位置还没搬的, 把旧目录的数据搬过来 (新目录里已有的先删掉);
/// 以前放在 exe 旁的数据, 数据目录里还没有数据时搬过来一次.
pub fn prepare() -> Result<(), String> {
    let exe_dir = std::env::current_exe().expect("locate exe").parent().expect("exe dir").to_path_buf();
    let dir = install::value(DATA_DIR_VALUE).map_or_else(default_dir, PathBuf::from);
    match install::value(MOVE_FROM_VALUE) {
        Some(from) => {
            move_data(Path::new(&from), &dir)?;
            install::delete_value(MOVE_FROM_VALUE);
        }
        None if !has_data(&dir) && has_data(&exe_dir) => move_data(&exe_dir, &dir)?,
        None => {}
    }
    fs::create_dir_all(&dir).map_err(|e| format!("创建数据目录 {} 失败: {e}", dir.display()))?;
    DIR.set(dir).expect("storage::prepare 只调一次");
    Ok(())
}

/// 数据目录.
pub fn dir() -> &'static Path {
    DIR.get().expect("先调 storage::prepare")
}

/// 目录里有没有 QRun 的数据 (设置或库).
pub fn has_data(dir: &Path) -> bool {
    dir.join("settings.json").exists() || dir.join(DB_FILE).exists()
}

/// 两个路径是不是同一个目录 (不分大小写, 不管结尾的分隔符).
pub fn same_dir(a: &Path, b: &Path) -> bool {
    let key = |p: &Path| p.to_string_lossy().trim_end_matches('\\').to_lowercase();
    key(a) == key(b)
}

/// 更改数据目录, 记进注册表, 重启后 `prepare` 照办. keep_existing: 直接用新目录里已有的数据, 当前的留在原处;
/// 否则重启时把当前数据搬过去. 选回默认目录时删掉自选值.
pub fn relocate(to: &Path, keep_existing: bool) -> Result<(), String> {
    if !to.is_absolute() {
        return Err(format!("{} 不是完整路径, 要从盘符写起", to.display()));
    }
    // 目录不存在就建; 写不进去的 (没有权限, 只读盘) 现在就报, 不留到重启后启动不了
    let probe = to.join("qrun.write-test");
    fs::create_dir_all(to)
        .and_then(|_| fs::write(&probe, b""))
        .and_then(|_| fs::remove_file(&probe))
        .map_err(|e| format!("不能写入 {}: {e}", to.display()))?;
    if same_dir(to, &default_dir()) {
        install::delete_value(DATA_DIR_VALUE);
    } else {
        install::set_value(DATA_DIR_VALUE, &to.to_string_lossy())?;
    }
    if keep_existing {
        install::delete_value(MOVE_FROM_VALUE);
        Ok(())
    } else {
        install::set_value(MOVE_FROM_VALUE, &dir().to_string_lossy())
    }
}

fn default_dir() -> PathBuf {
    install::local_app_data().join("QRun")
}

/// 把 from 里的数据文件搬到 to. to 里已有的数据文件先删掉, 不然留下的 library.json 之类会和搬来的库对不上.
/// from 里没有数据 (如旧目录已被删) 时什么都不动, 不能把 to 里的数据删了.
fn move_data(from: &Path, to: &Path) -> Result<(), String> {
    if !has_data(from) {
        return Ok(());
    }
    let fail = |e: std::io::Error| format!("把数据从 {} 搬到 {} 失败: {e}", from.display(), to.display());
    fs::create_dir_all(to).map_err(fail)?;
    for file in data_files(to).map_err(fail)? {
        fs::remove_file(file).map_err(fail)?;
    }
    for file in data_files(from).map_err(fail)? {
        let target = to.join(file.file_name().expect("data file name"));
        // 不在同一个盘时改名不了, 复制后删掉
        if fs::rename(&file, &target).is_err() {
            fs::copy(&file, &target).and_then(|_| fs::remove_file(&file)).map_err(fail)?;
        }
    }
    Ok(())
}

fn data_files(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        let name = path.file_name().expect("entry name").to_string_lossy().into_owned();
        if path.is_file() && DATA_FILES.iter().any(|prefix| name.starts_with(prefix)) {
            files.push(path);
        }
    }
    Ok(files)
}

/// 写盘时把 version 放在最前面, 其余字段照原样展开.
#[derive(Serialize)]
struct Versioned<'a, T> {
    version: u32,
    #[serde(flatten)]
    data: &'a T,
}

/// 数据目录里的文件; 数据文件和日志都放在这里.
pub fn path(file: &str) -> PathBuf {
    dir().join(file)
}

/// 读取数据文件; 文件不存在时写入默认值. 读不了, 格式有误或版本不对时返回给用户看的说明.
/// 文件比本程序新时同样返回错误, 程序随即退出, 不会用旧结构覆盖掉新版本写入的内容.
/// 文件比本程序旧时先备份成 `<文件>.v<旧版本>.bak`, 再用 migrate 逐级迁移 (每次把 from 版本的结构改成 from + 1), 迁移完写回.
pub fn load<T: Default + Serialize + DeserializeOwned>(
    file: &str,
    version: u32,
    migrate: fn(u32, &mut Map<String, Value>),
) -> Result<T, String> {
    let path = path(file);
    if !path.exists() {
        let value = T::default();
        save(file, version, &value);
        return Ok(value);
    }
    let invalid = |e: &dyn std::fmt::Display| {
        format!("{} 格式有误: {e}\n\n请修正该文件, 或删除它恢复默认, 然后重新启动.", path.display())
    };
    let text = fs::read_to_string(&path).map_err(|e| format!("读取 {} 失败: {e}", path.display()))?;
    let mut value: Value = serde_json::from_str(&text).map_err(|e| invalid(&e))?;
    let object = value.as_object_mut().ok_or_else(|| invalid(&"顶层不是对象"))?;
    // v0.1.0 写的文件还没有 version 字段, 结构就是版本 1
    let found = match object.remove("version") {
        None => 1,
        Some(v) => v.as_u64().ok_or_else(|| invalid(&"version 不是整数"))?,
    };
    if found > version as u64 {
        return Err(format!(
            "{} 来自更新版本的 QRun (文件版本 {found}, 本程序支持 {version}).\n\n请升级 QRun 后再打开; 本程序不会改动该文件.",
            path.display()
        ));
    }
    if found == version as u64 {
        return serde_json::from_value(value).map_err(|e| invalid(&e));
    }
    let backup = path.with_extension(format!("json.v{found}.bak"));
    fs::copy(&path, &backup).map_err(|e| format!("备份 {} 失败: {e}", path.display()))?;
    for from in found as u32..version {
        migrate(from, object);
    }
    let data: T = serde_json::from_value(value).map_err(|e| invalid(&e))?;
    save(file, version, &data);
    Ok(data)
}

/// 打开数据目录里的库, 不存在就新建. 不开 WAL, 库始终只有一个文件, 拷走目录不会漏掉没合并的改动.
/// 版本规则同 JSON 文件: 库比本程序新时返回错误; 比本程序旧时先备份成 `qrun.db.v<旧版本>.bak`, 再用 migrate 逐级迁移.
/// 新建的库版本是 0, 从 0 迁移就是建表.
pub fn open_db(version: u32, migrate: fn(u32, &Transaction) -> rusqlite::Result<()>) -> Result<Connection, String> {
    let path = path(DB_FILE);
    let fail = |e: rusqlite::Error| format!("打开 {} 失败: {e}", path.display());
    let mut db = Connection::open(&path).map_err(fail)?;
    db.pragma_update(None, "foreign_keys", true).map_err(fail)?;
    let found: u32 = db.pragma_query_value(None, "user_version", |row| row.get(0)).map_err(fail)?;
    if found > version {
        return Err(format!(
            "{} 来自更新版本的 QRun (库版本 {found}, 本程序支持 {version}).\n\n请升级 QRun 后再打开; 本程序不会改动该文件.",
            path.display()
        ));
    }
    if found < version {
        if found > 0 {
            let backup = path.with_extension(format!("db.v{found}.bak"));
            fs::copy(&path, &backup).map_err(|e| format!("备份 {} 失败: {e}", path.display()))?;
        }
        let tx = db.transaction().map_err(fail)?;
        for from in found..version {
            migrate(from, &tx).map_err(fail)?;
        }
        tx.pragma_update(None, "user_version", version).map_err(fail)?;
        tx.commit().map_err(fail)?;
    }
    Ok(db)
}

/// 先写临时文件再替换, 写到一半崩溃或断电也不会留下半截文件.
pub fn save<T: Serialize>(file: &str, version: u32, data: &T) {
    let path = path(file);
    let temp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(&Versioned { version, data }).expect("serialize data");
    if let Err(e) = fs::write(&temp, text).and_then(|_| fs::rename(&temp, &path)) {
        platform::error_box(&format!("保存 {} 失败: {e}", path.display()));
    }
}

/// 写库失败时弹框说明; 和 JSON 文件一样不中断程序, 内存里的数据照常可用.
pub fn db_save_failed(e: &rusqlite::Error) {
    platform::error_box(&format!("保存 {} 失败: {e}", path(DB_FILE).display()));
}
