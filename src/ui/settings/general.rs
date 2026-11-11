//! 常规: 开机自启, 单击或双击启动, 悬停切换分组, 置于顶层, 启动项目后收起面板, 按名称去重, 拖出程序时给什么.

use gpui_kit::component::IconName;
use gpui_kit::component::setting::{SettingField, SettingGroup, SettingItem, SettingPage};
use gpui_kit::*;

use super::handle;
use crate::model::settings::{DragProgram, LaunchClick, Settings};
use crate::platform::autostart::{self, Autostart};

pub fn page(settings: &Entity<Settings>) -> SettingPage {
    let autostart_description = match autostart::state() {
        Autostart::Moved => "启动项已失效, 请重新开启.",
        _ => "登录 Windows 时启动 QRun.",
    };
    let (read, write) = handle(settings);
    let (click_read, click_write) = handle(settings);
    let (hover_read, hover_write) = handle(settings);
    let (top_read, top_write) = handle(settings);
    let (dedupe_read, dedupe_write) = handle(settings);
    let (drag_read, drag_write) = handle(settings);

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
                .description("鼠标悬停在分组标签上时切换分组."),
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
                .description("面板始终显示在其他窗口之上."),
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
                .description("贴边时除外."),
            )
            .item(
                SettingItem::new(
                    "按名称去重",
                    SettingField::switch(
                        move |cx| dedupe_read.read(cx).general.dedupe_names,
                        move |value, cx| {
                            dedupe_write.update(cx, |s, cx| s.change(cx, |s| s.general.dedupe_names = value))
                        },
                    ),
                )
                .description("分组中已有同名项目时不再添加. 目标相同的项目始终不重复添加."),
            )
            .item(
                SettingItem::new(
                    "拖出程序",
                    SettingField::dropdown(
                        vec![("shortcut".into(), "快捷方式".into()), ("file".into(), "程序本身".into())],
                        move |cx| match drag_read.read(cx).general.drag_program {
                            DragProgram::Shortcut => "shortcut".into(),
                            DragProgram::File => "file".into(),
                        },
                        move |value, cx| {
                            let drag = if value == "file" { DragProgram::File } else { DragProgram::Shortcut };
                            drag_write.update(cx, |s, cx| s.change(cx, |s| s.general.drag_program = drag));
                        },
                    ),
                )
                .description("把程序拖到其他位置时生成的内容. 快捷方式拖出后仍是快捷方式."),
            ),
    )
}
