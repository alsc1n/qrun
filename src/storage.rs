//! 读写 exe 旁的 JSON 数据文件, 整个目录可以直接拷走.
//! 每个文件顶层带 version, 记录文件结构的版本. 以后有破坏性变更时, 在 load 里按版本逐级迁移旧结构.

use std::fs;
use std::path::PathBuf;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use crate::platform;

/// 写盘时把 version 放在最前面, 其余字段照原样展开.
#[derive(Serialize)]
struct Versioned<'a, T> {
    version: u32,
    #[serde(flatten)]
    data: &'a T,
}

/// exe 旁的文件; 数据文件和日志都放在这里.
pub fn path(file: &str) -> PathBuf {
    std::env::current_exe().expect("locate exe").with_file_name(file)
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

/// 先写临时文件再替换, 写到一半崩溃或断电也不会留下半截文件.
pub fn save<T: Serialize>(file: &str, version: u32, data: &T) {
    let path = path(file);
    let temp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(&Versioned { version, data }).expect("serialize data");
    if let Err(e) = fs::write(&temp, text).and_then(|_| fs::rename(&temp, &path)) {
        platform::error_box(&format!("保存 {} 失败: {e}", path.display()));
    }
}
