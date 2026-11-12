//! 隐私: 推荐收集哪些信息, 活动记录保留多久, 清除活动记录和重置推荐.

use gpui_kit::component::IconName;
use gpui_kit::component::Sizable;
use gpui_kit::component::button::Button;
use gpui_kit::component::setting::{SettingField, SettingGroup, SettingItem, SettingPage};
use gpui_kit::component::slider::SliderState;
use gpui_kit::*;

use super::{SettingsView, handle, slider, slider_field};
use crate::model::library::Library;
use crate::model::settings::{Collect, Settings};

/// 保留天数的范围.
const MIN_DAYS: f32 = 7.;
const MAX_DAYS: f32 = 180.;

pub fn retention_slider(settings: &Entity<Settings>, cx: &mut Context<SettingsView>) -> Entity<SliderState> {
    slider(
        settings,
        (MIN_DAYS, MAX_DAYS, 1.),
        |s| s.smart.retention_days as f32,
        |s, v| s.smart.retention_days = v as u32,
        false,
        cx,
    )
}

pub fn page(
    settings: &Entity<Settings>,
    library: &Entity<Library>,
    retention: &Entity<SliderState>,
    cx: &App,
) -> SettingPage {
    // 关掉推荐时什么都不收集, 各项变灰
    let off = !settings.read(cx).smart.enabled;
    let collect = |title, description, get: fn(&Collect) -> bool, set: fn(&mut Collect, bool)| {
        let (read, write) = handle(settings);
        let field = SettingField::switch(
            move |cx| get(&read.read(cx).smart.collect),
            move |value, cx| write.update(cx, |s, cx| s.change(cx, |s| set(&mut s.smart.collect, value))),
        );
        SettingItem::new(title, field).description(description).disabled(off)
    };
    let days = retention.read(cx).value().end();
    SettingPage::new("隐私")
        .icon(IconName::Eye)
        .group(
            SettingGroup::new()
                .title("推荐收集的信息")
                .description(
                    "所有数据只保存在这台电脑上, 不会上传到任何服务器或云端. 唯一的例外是你自己填写了 Jev 密钥: \
                     这时只把脱敏后的摘要发给 Jev, 用于推荐; 不填就不会发出任何数据.",
                )
                .item(collect("窗口", "前台窗口的程序和标题, 开着的窗口.", |c| c.window, |c, v| c.window = v))
                .item(collect(
                    "剪贴板",
                    "复制内容的开头, 已脱敏. 关闭后只记类型 (文字, 图片, 文件).",
                    |c| c.clipboard,
                    |c, v| c.clipboard = v,
                ))
                .item(collect("程序启停", "程序的启动, 退出和监听的端口.", |c| c.processes, |c, v| c.processes = v))
                .item(collect(
                    "文件改动",
                    "用户目录和本地硬盘上改动过的文件路径, 不读取内容.",
                    |c| c.files,
                    |c, v| c.files = v,
                ))
                .item(collect("新文件", "下载文件夹和桌面上新出现的文件.", |c| c.new_files, |c, v| c.new_files = v))
                .item(collect(
                    "系统最近记录",
                    "Windows 记下的最近打开的文件和各程序的最近列表. 关闭后推荐页不显示最近文件, 文件夹和程序.",
                    |c| c.system_recent,
                    |c, v| c.system_recent = v,
                )),
        )
        .group(
            SettingGroup::new()
                .title("记录")
                .item(
                    SettingItem::new("保留天数", slider_field(retention, off))
                        .description(format!("活动记录保留 {days} 天, 学到的习惯不受影响.")),
                )
                .item(clear_item(
                    library,
                    Clear {
                        id: "clear-activity",
                        title: "清除活动记录",
                        description: "删除窗口, 剪贴板, 文件等活动记录, 学到的习惯保留.",
                        message: "清除全部活动记录?",
                        detail: "学到的习惯保留, 推荐不受影响.",
                        action: Library::clear_activity,
                    },
                ))
                .item(clear_item(
                    library,
                    Clear {
                        id: "reset-recommendation",
                        title: "重置推荐",
                        description: "删除活动记录和学到的习惯, 推荐从头学起. 分组的点击次数保留.",
                        message: "重置推荐?",
                        detail: "活动记录和学到的习惯都会删除, 推荐从头学起.",
                        action: Library::reset_recommendation,
                    },
                )),
        )
}

/// 一个要确认才执行的清除按钮.
struct Clear {
    id: &'static str,
    title: &'static str,
    description: &'static str,
    message: &'static str,
    detail: &'static str,
    action: fn(&mut Library),
}

fn clear_item(library: &Entity<Library>, clear: Clear) -> SettingItem {
    let Clear { id, title, description, message, detail, action } = clear;
    let library = library.clone();
    let field = SettingField::render(move |_, _, _| {
        let library = library.clone();
        Button::new(id).outline().small().label(title).on_click(move |_, window, cx| {
            let answer = window.prompt(PromptLevel::Warning, message, Some(detail), &[title, "取消"], cx);
            let library = library.clone();
            cx.spawn(async move |cx| {
                if let Ok(0) = answer.await {
                    cx.update(|cx| {
                        library.update(cx, |lib, cx| {
                            action(lib);
                            cx.notify();
                        })
                    });
                }
            })
            .detach();
        })
    });
    SettingItem::new(title, field).description(description)
}
