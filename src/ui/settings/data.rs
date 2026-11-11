//! 数据: 数据目录 (查看, 打开, 安装版可更改), 从 Rolan 导入.

use std::path::PathBuf;

use gpui_kit::component::button::Button;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::setting::{SettingField, SettingGroup, SettingItem, SettingPage};
use gpui_kit::component::{IconName, Sizable, h_flex};
use gpui_kit::*;

use super::SettingsView;
use crate::model::library::Library;
use crate::model::rolan;
use crate::platform::{install, shell};
use crate::storage;

/// 上次操作的结果 (导入了多少, 更改目录失败的原因), 显示在设置项说明里; 这次打开设置后还没做过为 None.
pub type Status = Entity<Option<String>>;

pub fn page(
    library: &Entity<Library>,
    import_status: &Status,
    location: &Entity<InputState>,
    location_status: &Status,
    cx: &App,
) -> SettingPage {
    SettingPage::new("数据")
        .icon(IconName::HardDrive)
        .group(SettingGroup::new().item(location_item(location, location_status, cx)))
        .group(SettingGroup::new().item(rolan_import(library, import_status, cx)))
}

/// 数据目录的输入框, 初始是当前目录; 回车按输入的路径更改.
pub fn location_input(status: &Status, window: &mut Window, cx: &mut Context<SettingsView>) -> Entity<InputState> {
    let input = cx.new(|cx| InputState::new(window, cx).default_value(storage::dir().display().to_string()));
    let status = status.clone();
    cx.subscribe_in(&input, window, move |_, input, event: &InputEvent, window, cx| {
        if let InputEvent::PressEnter { .. } = event {
            let to = PathBuf::from(input.read(cx).value().trim());
            change_location(to, &status, input, window, cx);
        }
    })
    .detach();
    input
}

/// 数据目录: 设置, 库和日志所在. 输入框改路径回车, 或点"选择"挑目录.
fn location_item(input: &Entity<InputState>, status: &Status, cx: &App) -> SettingItem {
    let description = status
        .read(cx)
        .clone()
        .unwrap_or_else(|| "存放设置, 使用记录和日志. 更改后数据将迁移至新位置并重启 QRun.".into());
    let (input, status) = (input.clone(), status.clone());
    let field = SettingField::render(move |_, _, _| {
        let (browse_input, status) = (input.clone(), status.clone());
        let browse = Button::new("browse-data")
            .outline()
            .small()
            .label("选择")
            .on_click(move |_, window, cx| browse(&browse_input, &status, window, cx));
        let open = Button::new("open-data").outline().small().label("打开").on_click(|_, _, _| {
            shell::run(storage::dir(), "", false);
        });
        h_flex().w_full().gap_2().child(Input::new(&input).small().flex_1()).child(browse).child(open)
    });
    SettingItem::new("数据目录", field).layout(Axis::Vertical).description(description)
}

/// 挑一个目录, 填进输入框并按它更改.
fn browse(input: &Entity<InputState>, status: &Status, window: &mut Window, cx: &mut App) {
    let picked =
        cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: None });
    let (input, status) = (input.clone(), status.clone());
    window
        .spawn(cx, async move |cx| {
            let Ok(Ok(Some(paths))) = picked.await else {
                return;
            };
            cx.update(|window, cx| {
                let to = paths[0].clone();
                input.update(cx, |state, cx| state.set_value(to.display().to_string(), window, cx));
                change_location(to, &status, &input, window, cx);
            })
            .ok();
        })
        .detach();
}

/// 把数据目录改到 to: 那里已有 QRun 的数据时问用哪份, 没有时确认后搬过去; 定了就记下并重启 QRun.
/// 取消时输入框改回当前目录; 失败的原因显示在设置项说明里.
fn change_location(to: PathBuf, status: &Status, input: &Entity<InputState>, window: &mut Window, cx: &mut App) {
    if storage::same_dir(&to, storage::dir()) {
        return;
    }
    let existing = storage::has_data(&to);
    let (message, answers): (String, &[&str]) = if existing {
        (format!("{} 里已有 QRun 的数据", to.display()), &["用这里的数据", "用当前数据覆盖", "取消"])
    } else {
        (format!("把数据搬到 {} 并重启 QRun?", to.display()), &["搬过去并重启", "取消"])
    };
    let detail = existing.then_some("用这里的数据: 当前数据留在原处不动. 用当前数据覆盖: 这里已有的会被删掉.");
    let answer = window.prompt(PromptLevel::Info, &message, detail, answers, cx);
    let (status, input) = (status.clone(), input.clone());
    window
        .spawn(cx, async move |cx| {
            let keep_existing = match (existing, answer.await) {
                (true, Ok(0)) => true,
                (true, Ok(1)) | (false, Ok(0)) => false,
                _ => {
                    cx.update(|window, cx| {
                        let current = storage::dir().display().to_string();
                        input.update(cx, |state, cx| state.set_value(current, window, cx));
                    })
                    .ok();
                    return;
                }
            };
            if let Err(e) = storage::relocate(&to, keep_existing).and_then(|_| install::restart()) {
                cx.update(|_, cx| {
                    status.update(cx, |status, cx| {
                        *status = Some(format!("更改失败: {e}"));
                        cx.notify();
                    })
                })
                .ok();
            }
        })
        .detach();
}

fn rolan_import(library: &Entity<Library>, status: &Status, cx: &App) -> SettingItem {
    let description = status.read(cx).clone().unwrap_or_else(|| "导入 Rolan 2 的分组和项目, 同名分组将合并.".into());
    let (library, status) = (library.clone(), status.clone());
    let import = SettingField::render(move |_, _, _| {
        let (library, status) = (library.clone(), status.clone());
        Button::new("rolan").outline().small().label("导入").on_click(move |_, _, cx| import(&library, &status, cx))
    });
    SettingItem::new("从 Rolan 导入", import).description(description)
}

/// 默认位置有 Rolan 的数据就直接导入, 没有 (如绿色版放在别处) 让用户选文件.
fn import(library: &Entity<Library>, status: &Status, cx: &mut App) {
    let path = rolan::default_path();
    if path.exists() {
        import_file(path, library, status, cx);
        return;
    }
    let picked =
        cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: false, prompt: None });
    let (library, status) = (library.clone(), status.clone());
    cx.spawn(async move |cx| {
        if let Ok(Ok(Some(paths))) = picked.await {
            cx.update(|cx| import_file(paths[0].clone(), &library, &status, cx));
        }
    })
    .detach();
}

fn import_file(path: PathBuf, library: &Entity<Library>, status: &Status, cx: &mut App) {
    let result = std::fs::read(&path)
        .map_err(|e| format!("读取 {} 失败: {e}", path.display()))
        .and_then(|bytes| rolan::parse(&bytes));
    let message = match result {
        Ok(groups) => {
            let imported = library.update(cx, |lib, cx| lib.import_rolan(groups, cx));
            format!(
                "已导入 {} 个项目, 新建 {} 个分组, 跳过 {} 个重复项.",
                imported.items, imported.groups, imported.skipped
            )
        }
        Err(e) => format!("导入失败: {e}"),
    };
    status.update(cx, |status, cx| {
        *status = Some(message);
        cx.notify();
    });
}
