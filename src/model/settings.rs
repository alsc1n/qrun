//! 用户设置, 保存在 settings.json. 结构和设置页的分类一一对应; window 是运行状态, 不在设置页显示.
//! 开机自启不在这里, 它的状态以注册表为准, 见 platform::autostart.

use gpui_kit::Context;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::storage;

const FILE: &str = "settings.json";
/// 文件结构版本; 只有删除, 改名, 挪位置, 改类型这类破坏性变更才加一, 并在 migrate 里补上迁移. 新增设置项不用改版本.
const VERSION: u32 = 2;

/// 把 from 版本的结构改成 from + 1 版本.
fn migrate(from: u32, settings: &mut Map<String, Value>) {
    match from {
        // 2: 窗口改为不透明, 去掉外观里的背景不透明度
        1 => {
            if let Some(Value::Object(appearance)) = settings.get_mut("appearance") {
                appearance.remove("opacity");
            }
        }
        _ => unreachable!("settings.json 没有版本 {from} 的迁移"),
    }
}

// 缺少的字段取默认值, 旧文件读得了新加的设置项; 拼错的字段名报错, 不会被悄悄忽略.
#[derive(Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub general: General,
    pub appearance: Appearance,
    pub dock: DockSettings,
    pub window: WindowState,
}

/// 常规.
#[derive(Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct General {
    /// 单击还是双击项目启动.
    pub launch_click: LaunchClick,
    /// 鼠标停在分组标签上就切过去, 不用点击.
    pub hover_switch_group: bool,
    /// 未贴边时窗口也保持在最上层; 贴边时总是置顶.
    pub always_on_top: bool,
    /// 启动项目后收起面板.
    pub hide_after_launch: bool,
    /// 启动时和之后定时检查新版本.
    pub auto_check_updates: bool,
    /// 用户选了"跳过此版本"的版本号, 自动检查不再提醒它; 空表示没有.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub skipped_version: String,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum LaunchClick {
    Single,
    Double,
}

/// 外观.
#[derive(Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Appearance {
    pub tab_layout: TabLayout,
    /// 图标显示尺寸 (逻辑像素).
    pub icon_size: f32,
    /// 界面字体的字体族名, 空字符串表示系统默认. 本机没装, fonts 目录里也没有这个字体时同样用系统默认.
    pub font_family: String,
}

/// 分组标签排在图标上方 (横排) 还是左侧 (竖排).
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum TabLayout {
    Horizontal,
    Vertical,
}

/// 贴边.
#[derive(Serialize, Deserialize, Clone, Copy)]
#[serde(default, deny_unknown_fields)]
pub struct DockSettings {
    pub enabled: bool,
    /// 吸附后标题栏不能拖动窗口, 不会被拖离屏幕边.
    pub lock_position: bool,
    /// 松开鼠标时窗口离屏幕边多近就吸附 (逻辑像素).
    pub snap_distance: i32,
    /// 鼠标离开窗口多久后缩回.
    pub hide_delay_ms: u64,
    /// 滑出和缩回的动画时长.
    pub animation_ms: u64,
}

/// 窗口运行状态.
#[derive(Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WindowState {
    pub width: f32,
    pub height: f32,
    /// 当前吸附的屏幕边, 未吸附时为空.
    pub docked: Option<Docked>,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Edge {
    Top,
    Left,
    Right,
}

/// 吸附状态; x, y 是窗口弹出时可见区域左上角的屏幕坐标 (物理像素).
#[derive(Serialize, Deserialize, Clone, Copy)]
#[serde(deny_unknown_fields)]
pub struct Docked {
    pub edge: Edge,
    pub x: i32,
    pub y: i32,
}

impl Default for General {
    fn default() -> Self {
        Self {
            launch_click: LaunchClick::Single,
            hover_switch_group: false,
            always_on_top: false,
            hide_after_launch: false,
            auto_check_updates: true,
            skipped_version: String::new(),
        }
    }
}

impl Default for Appearance {
    fn default() -> Self {
        // 默认用随程序带的 HarmonyOS Sans SC (exe 旁 fonts 目录), 中英文一套字形
        Self { tab_layout: TabLayout::Horizontal, icon_size: 40.0, font_family: "HarmonyOS Sans SC".into() }
    }
}

impl Default for DockSettings {
    fn default() -> Self {
        Self { enabled: true, lock_position: false, snap_distance: 24, hide_delay_ms: 100, animation_ms: 150 }
    }
}

impl Default for WindowState {
    fn default() -> Self {
        Self { width: 640.0, height: 440.0, docked: None }
    }
}

impl Settings {
    pub fn load() -> Result<Self, String> {
        storage::load(FILE, VERSION, migrate)
    }

    /// 修改设置: 保存并通知面板, 设置页等观察者刷新.
    pub fn change(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Self)) {
        f(self);
        storage::save(FILE, VERSION, self);
        cx.notify();
    }

    /// 只改内存并通知, 不保存: 滑动条拖动中让面板跟着变, 松手再用 `change` 保存.
    pub fn preview(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Self)) {
        f(self);
        cx.notify();
    }
}
