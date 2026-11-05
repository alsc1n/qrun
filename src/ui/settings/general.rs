//! 常规: 开机自启, 单击或双击启动, 悬停切换分组, 置于顶层, 启动项目后收起面板.

use gpui_kit::component::IconName;
use gpui_kit::component::setting::{SettingField, SettingGroup, SettingItem, SettingPage};
use gpui_kit::*;

use super::handle;
use crate::model::settings::{LaunchClick, Settings};
use crate::platform::autostart::{self, Autostart};

pub fn page(settings: &Entity<Settings>) -> SettingPage {
    let autostart_description = match autostart::state() {
        Autostart::Moved => "位置已移动, 请重新开启.",
        _ => "登录 Windows 后自动运行 QRun.",
    };
    let (read, write) = handle(settings);
    let (click_read, click_write) = handle(settings);
    let (hover_read, hover_write) = handle(settings);
    let (top_read, top_write) = handle(settings);

    SettingPage::new("常规").icon(IconName::Settings2).default_open(true).group(
        SettingGroup::new()
            .item(
                SettingItem::new(
                    "开机自启",
                    SettingField::switch(
                        |_| autostart::state() == Autostart::On,
                        |enabled, cx| {
                            autostart::set(enabled);
                            // 状态在注册表里, 没有 Entity 变化可观察, 手动刷新
                            cx.refresh_windows();
                        },
                    ),
                )
                .description(autostart_description),
            )
            .item(SettingItem::new(
                "启动项目",
                SettingField::dropdown(
                    vec![("single".into(), "单击".into()), ("double".into(), "双击".into())],
                    move |cx| match click_read.read(cx).general.launch_click {
                        LaunchClick::Single => "single".into(),
                        LaunchClick::Double => "double".into(),
                    },
                    move |value, cx| {
                        let click = if value == "double" { LaunchClick::Double } else { LaunchClick::Single };
                        click_write.update(cx, |s, cx| s.change(cx, |s| s.general.launch_click = click));
                    },
                ),
            ))
            .item(
                SettingItem::new(
                    "鼠标悬停切换分组",
                    SettingField::switch(
                        move |cx| hover_read.read(cx).general.hover_switch_group,
                        move |value, cx| {
                            hover_write.update(cx, |s, cx| s.change(cx, |s| s.general.hover_switch_group = value))
                        },
                    ),
                )
                .description("鼠标停在分组标签上就切过去, 不用点击."),
            )
            .item(
                SettingItem::new(
                    "置于顶层",
                    SettingField::switch(
                        move |cx| top_read.read(cx).general.always_on_top,
                        move |value, cx| {
                            top_write.update(cx, |s, cx| s.change(cx, |s| s.general.always_on_top = value))
                        },
                    ),
                )
                .description("面板始终在其他窗口上方; 贴边时总是置顶."),
            )
            .item(
                SettingItem::new(
                    "启动项目后收起面板",
                    SettingField::switch(
                        move |cx| read.read(cx).general.hide_after_launch,
                        move |value, cx| {
                            write.update(cx, |s, cx| s.change(cx, |s| s.general.hide_after_launch = value))
                        },
                    ),
                )
                .description("贴边状态下不收起, 鼠标离开后它会自己缩回."),
            ),
    )
}
