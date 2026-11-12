//! 推荐: 开关, 推荐页各块显示几个, Jev 的接口地址和密钥.

use gpui_kit::component::IconName;
use gpui_kit::component::Sizable;
use gpui_kit::component::button::Button;
use gpui_kit::component::setting::{SettingField, SettingGroup, SettingItem, SettingPage};
use gpui_kit::component::slider::SliderState;
use gpui_kit::*;

use super::{SettingsView, handle, slider, slider_field};
use crate::model::library::Library;
use crate::model::settings::Settings;
use crate::params::PARAMS;
use crate::platform::shell;
use crate::{logger, storage};

/// 每块数量的上限.
const MAX_COUNT: f32 = 30.;

/// 推荐页五块各一个滑动条.
pub struct SmartSliders {
    predict: Entity<SliderState>,
    frequent: Entity<SliderState>,
    recent_files: Entity<SliderState>,
    recent_folders: Entity<SliderState>,
    recent_programs: Entity<SliderState>,
}

/// 推荐页就在面板上, 拖动中面板跟着变.
pub fn sliders(settings: &Entity<Settings>, cx: &mut Context<SettingsView>) -> SmartSliders {
    let range = (0., MAX_COUNT, 1.);
    SmartSliders {
        predict: slider(settings, range, |s| s.smart.predict as f32, |s, v| s.smart.predict = v as u32, true, cx),
        frequent: slider(settings, range, |s| s.smart.frequent as f32, |s, v| s.smart.frequent = v as u32, true, cx),
        recent_files: slider(
            settings,
            range,
            |s| s.smart.recent_files as f32,
            |s, v| s.smart.recent_files = v as u32,
            true,
            cx,
        ),
        recent_folders: slider(
            settings,
            range,
            |s| s.smart.recent_folders as f32,
            |s, v| s.smart.recent_folders = v as u32,
            true,
            cx,
        ),
        recent_programs: slider(
            settings,
            range,
            |s| s.smart.recent_programs as f32,
            |s, v| s.smart.recent_programs = v as u32,
            true,
            cx,
        ),
    }
}

pub fn page(settings: &Entity<Settings>, library: &Entity<Library>, sliders: &SmartSliders, cx: &App) -> SettingPage {
    // 关掉推荐时下面各项变灰
    let off = !settings.read(cx).smart.enabled;
    let count = |state: &Entity<SliderState>| {
        let count = state.read(cx).value().end();
        if count == 0. { "不显示".to_string() } else { format!("最多 {count} 个") }
    };
    let item = |title, state: &Entity<SliderState>| {
        SettingItem::new(title, slider_field(state, off)).description(count(state))
    };
    // 改了算法能直接看出变好还是变差
    let predict_description = match library.read(cx).hit_rate() {
        None => count(&sliders.predict),
        Some(hit) => format!(
            "{} · 近 {} 天命中率: 首位 {:.0}%, 前 {} 位 {:.0}%",
            count(&sliders.predict),
            PARAMS.learn.hit_rate_days,
            hit.top1 * 100.,
            PARAMS.predict.replay_top_k,
            hit.top_k * 100.
        ),
    };
    let (get, set) = handle(settings);
    let enabled = SettingField::switch(
        move |cx| get.read(cx).smart.enabled,
        move |value, cx| set.update(cx, |s, cx| s.change(cx, |s| s.smart.enabled = value)),
    );
    let (get, set) = handle(settings);
    let jev_base_url = SettingField::input(
        move |cx| get.read(cx).smart.jev_base_url.clone().into(),
        move |value: SharedString, cx| {
            set.update(cx, |s, cx| s.change(cx, |s| s.smart.jev_base_url = value.trim().into()))
        },
    );
    let (get, set) = handle(settings);
    let jev_key = SettingField::input(
        move |cx| get.read(cx).smart.jev_key.clone().into(),
        move |value: SharedString, cx| set.update(cx, |s, cx| s.change(cx, |s| s.smart.jev_key = value.trim().into())),
    );
    SettingPage::new("推荐")
        .icon(IconName::Bot)
        .group(
            SettingGroup::new()
                .item(SettingItem::new("推荐", enabled).description("关闭后不再记录使用情况, 搜索仅按名称匹配."))
                .item(
                    SettingItem::new("猜你要打开", slider_field(&sliders.predict, off))
                        .description(predict_description),
                )
                .item(item("最常用", &sliders.frequent))
                .item(item("最近文件", &sliders.recent_files))
                .item(item("最近文件夹", &sliders.recent_folders))
                .item(item("最近程序", &sliders.recent_programs)),
        )
        .group(
            SettingGroup::new()
                .title("Jev 决策模型")
                .item(
                    SettingItem::new("接口地址", jev_base_url)
                        .layout(Axis::Vertical)
                        .disabled(off)
                        .description("通常无需修改."),
                )
                .item(SettingItem::new("密钥", jev_key).layout(Axis::Vertical).disabled(off).description(
                    "填写后, 当前窗口和最近使用记录将发送至 Jev 用于推荐. 发送前已在本机脱敏, \
                             密钥, 密码, 手机号, 证件号等不会发出.",
                ))
                .item(jev_log(library, cx)),
        )
}

/// 最近一次调用 Jev 的结果和它的考核得分; 每次失败都写进日志, 按钮打开日志文件.
fn jev_log(library: &Entity<Library>, cx: &App) -> SettingItem {
    let library = library.read(cx);
    // at 形如 2026-11-05 14:03:21.123, 只显示时分
    let mut description = match library.jev_last() {
        None => "尚未调用.".to_string(),
        Some((at, Ok(()))) => format!("上次调用 {} 成功.", &at[11..16]),
        Some((at, Err(e))) => format!("上次调用 {} 失败: {e}", &at[11..16]),
    };
    // 打开的在 Jev 排序里第一得 1 分, 每后一名减半; 得分越高推荐越听它的
    if let Some((score, count)) = library.jev_score() {
        description.push_str(&format!(" 排序得分 {score:.2} (近 {count:.0} 次), 得分越高推荐越参考 Jev."));
    }
    let open = SettingField::render(|_, _, _| {
        Button::new("jev-log")
            .outline()
            .small()
            .label("打开日志")
            .on_click(|_, _, _| shell::run(&storage::path(logger::FILE), "", false))
    });
    SettingItem::new("调用记录", open).description(description)
}
