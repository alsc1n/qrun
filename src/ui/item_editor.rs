//! 编辑项目对话框: 名称, 目标, 参数, 始终以管理员权限启动.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{Sizable, WindowExt, h_flex, v_flex};
use gpui_kit::*;

use crate::model::library::{Id, ItemEdit, Library};

pub fn open(library: &Entity<Library>, id: Id, window: &mut Window, cx: &mut App) {
    let Some(item) = library.read(cx).item(id).cloned() else {
        return;
    };
    let name = cx.new(|cx| InputState::new(window, cx).default_value(item.name));
    let target = cx.new(|cx| InputState::new(window, cx).default_value(item.target.display().to_string()));
    let args = cx.new(|cx| InputState::new(window, cx).default_value(item.args));
    let admin = Rc::new(Cell::new(item.admin));
    let library = library.clone();

    window.open_dialog(cx, move |dialog, _, _| {
        let browse_target = target.clone();
        let toggle_admin = admin.clone();
        let (name, target, args, admin, library) =
            (name.clone(), target.clone(), args.clone(), admin.clone(), library.clone());
        dialog
            .title("编辑")
            .w(px(420.))
            .child(
                v_flex()
                    .gap_4()
                    .child(field("名称", Input::new(&name)))
                    .child(
                        v_flex()
                            .gap_1p5()
                            .child(
                                h_flex().justify_between().child("目标").child(
                                    Button::new("browse")
                                        .small()
                                        .outline()
                                        .label("浏览")
                                        .on_click(move |_, window, cx| browse(browse_target.clone(), window, cx)),
                                ),
                            )
                            .child(Input::new(&target)),
                    )
                    .child(field("参数", Input::new(&args)))
                    .child(Checkbox::new("admin").label("始终以管理员权限启动").checked(admin.get()).on_click(
                        move |checked, window, _| {
                            toggle_admin.set(*checked);
                            window.refresh();
                        },
                    )),
            )
            .footer(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(Button::new("ok").primary().label("确定").on_click(move |_, window, cx| {
                        let edit = ItemEdit {
                            name: name.read(cx).value().trim().to_string(),
                            target: PathBuf::from(target.read(cx).value().trim()),
                            args: args.read(cx).value().trim().to_string(),
                            admin: admin.get(),
                        };
                        library.update(cx, |lib, cx| lib.update_item(id, edit, cx));
                        window.close_dialog(cx);
                    }))
                    .child(Button::new("cancel").label("取消").on_click(|_, window, cx| window.close_dialog(cx))),
            )
    });
}

fn field(label: &'static str, input: Input) -> impl IntoElement {
    v_flex().gap_1p5().child(label).child(input)
}

/// 选择文件, 填入目标输入框.
fn browse(target: Entity<InputState>, window: &mut Window, cx: &mut App) {
    let picked =
        cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: false, prompt: None });
    window
        .spawn(cx, async move |cx| {
            if let Ok(Ok(Some(paths))) = picked.await {
                cx.update(|window, cx| {
                    target.update(cx, |state, cx| state.set_value(paths[0].display().to_string(), window, cx))
                })
                .ok();
            }
        })
        .detach();
}
