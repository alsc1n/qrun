//! 贴边: 开关, 锁定位置, 吸附距离, 缩回延迟, 动画时长.

use gpui_kit::component::IconName;
use gpui_kit::component::setting::{SettingField, SettingGroup, SettingItem, SettingPage};
use gpui_kit::component::slider::SliderState;
use gpui_kit::*;

use super::{SettingsView, handle, slider, slider_field};
use crate::model::settings::Settings;

/// 贴边页的三个滑动条.
pub struct DockSliders {
    snap_distance: Entity<SliderState>,
    hide_delay: Entity<SliderState>,
    animation: Entity<SliderState>,
}

/// 贴边参数要真的拖到屏幕边才看得出效果, 拖动中不必预览, 只显示当前值.
pub fn sliders(settings: &Entity<Settings>, cx: &mut Context<SettingsView>) -> DockSliders {
    DockSliders {
        snap_distance: slider(
            settings,
            (4., 100., 4.),
            |s| s.dock.snap_distance as f32,
            |s, v| s.dock.snap_distance = v as i32,
            false,
            cx,
        ),
        hide_delay: slider(
            settings,
            (0., 3000., 100.),
            |s| s.dock.hide_delay_ms as f32,
            |s, v| s.dock.hide_delay_ms = v as u64,
            false,
            cx,
        ),
        animation: slider(
            settings,
            (0., 1000., 50.),
            |s| s.dock.animation_ms as f32,
            |s, v| s.dock.animation_ms = v as u64,
            false,
            cx,
        ),
    }
}

pub fn page(settings: &Entity<Settings>, sliders: &DockSliders, cx: &App) -> SettingPage {
    let enabled = settings.read(cx).dock.enabled;
    let (enabled_read, enabled_write) = handle(settings);
    let (lock_read, lock_write) = handle(settings);
    let value = |state: &Entity<SliderState>| state.read(cx).value().end();
    let item =
        |title, state: &Entity<SliderState>| SettingItem::new(title, slider_field(state, !enabled)).disabled(!enabled);

    SettingPage::new("贴边").icon(IconName::PanelLeft).group(
        SettingGroup::new()
            .item(
                SettingItem::new(
                    "启用贴边",
                    SettingField::switch(
                        move |cx| enabled_read.read(cx).dock.enabled,
                        move |value, cx| enabled_write.update(cx, |s, cx| s.change(cx, |s| s.dock.enabled = value)),
                    ),
                )
                .description("将面板拖到屏幕边缘时贴靠, 鼠标移开后自动隐藏."),
            )
            .item(
                SettingItem::new(
                    "锁定贴边位置",
                    SettingField::switch(
                        move |cx| lock_read.read(cx).dock.lock_position,
                        move |value, cx| lock_write.update(cx, |s, cx| s.change(cx, |s| s.dock.lock_position = value)),
                    ),
                )
                .description("贴靠后不可拖动.")
                .disabled(!enabled),
            )
            .item(
                item("吸附距离", &sliders.snap_distance).description(format!("{} 像素", value(&sliders.snap_distance))),
            )
            .item(item("缩回延迟", &sliders.hide_delay).description(format!("{} 毫秒", value(&sliders.hide_delay))))
            .item(item("动画时长", &sliders.animation).description({
                let ms = value(&sliders.animation);
                if ms == 0. { "无动画".to_string() } else { format!("{ms} 毫秒") }
            })),
    )
}
