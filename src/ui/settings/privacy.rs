//! 隐私: 推荐收集哪些信息, 不推荐的目录和扩展名, 活动记录保留多久, 清除活动记录和重置推荐.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::setting::{SettingField, SettingGroup, SettingItem, SettingPage};
use gpui_kit::component::slider::SliderState;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{IconName, Sizable, h_flex, v_flex};
use gpui_kit::*;

use super::{SettingsView, handle, slider, slider_field};
use crate::model::library::Library;
use crate::model::settings::{Collect, Ignore, Settings};

/// 不推荐的目录和扩展名: 已有的一项一个标签 (点 × 删掉), 下面的输入框打字回车 (或离开输入框) 加成新标签.
/// 输入框由视图持有, 跨帧保留打了一半的字.
pub struct IgnoreInputs {
    dirs: Entity<InputState>,
    extensions: Entity<InputState>,
}

/// 一类清单: 读, 改, 扩展名要去掉开头的点.
#[derive(Clone, Copy)]
struct IgnoreList {
    id: &'static str,
    get: fn(&Ignore) -> &Vec<String>,
    get_mut: fn(&mut Ignore) -> &mut Vec<String>,
    extensions: bool,
}

const DIRS: IgnoreList = IgnoreList { id: "ignore-dir", get: |i| &i.dirs, get_mut: |i| &mut i.dirs, extensions: false };
const EXTENSIONS: IgnoreList =
    IgnoreList { id: "ignore-ext", get: |i| &i.extensions, get_mut: |i| &mut i.extensions, extensions: true };

pub fn ignore_inputs(settings: &Entity<Settings>, window: &mut Window, cx: &mut Context<SettingsView>) -> IgnoreInputs {
    IgnoreInputs {
        dirs: ignore_input(settings, DIRS, "添加目录名或完整路径, 回车确认", window, cx),
        extensions: ignore_input(settings, EXTENSIONS, "添加扩展名, 回车确认", window, cx),
    }
}

fn ignore_input(
    settings: &Entity<Settings>,
    list: IgnoreList,
    placeholder: &'static str,
    window: &mut Window,
    cx: &mut Context<SettingsView>,
) -> Entity<InputState> {
    let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
    let settings = settings.clone();
    cx.subscribe_in(&input, window, move |_, input, event: &InputEvent, window, cx| {
        if !matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
            return;
        }
        let added = parse_list(&input.read(cx).value(), list.extensions);
        if added.is_empty() {
            return;
        }
        input.update(cx, |state, cx| state.set_value("", window, cx));
        settings.update(cx, |s, cx| {
            let current = (list.get)(&s.smart.ignore);
            let added: Vec<String> = added.into_iter().filter(|item| !current.contains(item)).collect();
            if !added.is_empty() {
                s.change(cx, |s| (list.get_mut)(&mut s.smart.ignore).extend(added));
            }
        });
    })
    .detach();
    input
}

/// 一次可以加好几个: 逗号 (中英文都行), 分号或空白分开; 转小写, 去重. 扩展名去掉开头的点 (目录名不去, .git 就叫 .git).
/// 完整路径里可能有空格 (Program Files), 含路径分隔符的整段当一项, 不按空白拆.
fn parse_list(text: &str, extensions: bool) -> Vec<String> {
    let mut items: Vec<String> = Vec::new();
    for part in text.split([',', '，', ';', '；', '\n']) {
        let part = part.trim();
        let pieces: Vec<&str> = match part.contains(['\\', '/', ':']) {
            true => vec![part],
            false => part.split_whitespace().collect(),
        };
        for piece in pieces {
            let item = if extensions { piece.trim_start_matches('.') } else { piece }.to_lowercase();
            if !item.is_empty() && !items.contains(&item) {
                items.push(item);
            }
        }
    }
    items
}

/// 已有的标签, 每个带 × 删除; 下面是添加用的输入框.
fn tags_field(settings: &Entity<Settings>, input: &Entity<InputState>, list: IgnoreList) -> SettingField<SharedString> {
    let (settings, input) = (settings.clone(), input.clone());
    SettingField::render(move |_, _, cx| {
        let items = (list.get)(&settings.read(cx).smart.ignore).clone();
        let tags = items.into_iter().enumerate().map(|(ix, item)| {
            let settings = settings.clone();
            let remove = item.clone();
            let delete = Button::new((list.id, ix)).ghost().xsmall().icon(IconName::Close).on_click(move |_, _, cx| {
                let remove = remove.clone();
                settings
                    .update(cx, |s, cx| s.change(cx, |s| (list.get_mut)(&mut s.smart.ignore).retain(|x| *x != remove)));
            });
            Tag::secondary().small().child(h_flex().gap_0p5().child(item).child(delete))
        });
        v_flex().w_full().gap_2().child(h_flex().flex_wrap().gap_1().children(tags)).child(Input::new(&input).small())
    })
}

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
    ignore: &IgnoreInputs,
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
                .title("不推荐的文件")
                .item(
                    SettingItem::new("目录", tags_field(settings, &ignore.dirs, DIRS))
                        .layout(Axis::Vertical)
                        .disabled(off)
                        .description(
                            "写名称 (如 temp) 排除所有叫这个名字的目录, 写完整路径 (如 D:\\logs) 只排除这一个.",
                        ),
                )
                .item(
                    SettingItem::new("扩展名", tags_field(settings, &ignore.extensions, EXTENSIONS))
                        .layout(Axis::Vertical)
                        .disabled(off)
                        .description("隐藏文件和 Office 打开文档时生成的锁文件始终不推荐."),
                ),
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
