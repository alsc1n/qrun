//! 项目参数, 来自仓库根目录的 params.json, 编译时嵌入 exe.

use std::sync::LazyLock;

use serde::Deserialize;

#[derive(Deserialize)]
pub struct Params {
    pub update: Update,
    pub panel: Panel,
    /// 与 installer/qrun.iss 的 AppId 一致, 用来找安装包写入的卸载信息.
    pub installer_app_id: String,
}

#[derive(Deserialize)]
pub struct Update {
    /// 发行版地址, 按顺序尝试: 前面的取不到再用后面的.
    pub sources: Vec<String>,
    pub check_interval_hours: u64,
    pub timeout_secs: u32,
}

#[derive(Deserialize)]
pub struct Panel {
    /// 开了悬停切换分组时, 鼠标在标签上停多久才切, 划过标签栏时不会连着切.
    pub hover_switch_delay_ms: u64,
}

pub static PARAMS: LazyLock<Params> =
    LazyLock::new(|| serde_json::from_str(include_str!("../params.json")).expect("params.json"));
