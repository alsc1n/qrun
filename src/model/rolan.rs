//! 读 Rolan 2 的分组和项目, 给"从 Rolan 导入"用.
//! Rolan 2 (2.6) 把分组和项目存在 `%APPDATA%\Rolan\Data`: raw deflate (无 zlib 头) 压缩的 UTF-8 JSON, 顶层是分组数组.
//! 只取导入要用的字段, 其余 (图标位置, 分组类型等) 忽略.

use std::path::PathBuf;

use serde::Deserialize;

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Group {
    pub title: String,
    #[serde(rename = "Data")]
    pub items: Vec<Item>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Item {
    pub name: String,
    pub target_path: PathBuf,
    pub arguments: String,
    /// 以管理员身份运行.
    pub admin: bool,
    /// 在 Rolan 里点过几次.
    pub count: u32,
}

/// Rolan 2 数据文件的默认位置.
pub fn default_path() -> PathBuf {
    PathBuf::from(std::env::var_os("APPDATA").expect("APPDATA")).join("Rolan").join("Data")
}

/// 解开数据文件内容, 读出分组.
pub fn parse(bytes: &[u8]) -> Result<Vec<Group>, String> {
    let json = miniz_oxide::inflate::decompress_to_vec(bytes).map_err(|e| format!("不是 Rolan 2 的数据文件 ({e})"))?;
    serde_json::from_slice(&json).map_err(|e| format!("Rolan 数据格式不认识: {e}"))
}
