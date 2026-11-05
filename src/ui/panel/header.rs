//! 标题栏: 图标和拖动区, 搜索框, 设置按钮, 收起按钮.

use std::sync::{Arc, LazyLock};

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::Input;
use gpui_kit::component::{Icon, IconName, h_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use super::Panel;
use crate::actions::OpenSettings;
use crate::platform::window as native;

/// 标题栏左上角的程序图标; 从 1024px 的 qrun.png 缩成 64px 单独存一份, 免得把大图编进 exe.
static LOGO: LazyLock<Arc<Image>> = LazyLock::new(|| {
    Arc::new(Image::from_bytes(ImageFormat::Png, include_bytes!("../../../assets/logo-64.png").to_vec()))
});

impl Panel {
    pub(super) fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        // 锁定贴边位置: 吸附着时标题栏不再是拖动区, 窗口拖不离屏幕边
        let settings = self.settings.read(cx);
        let locked = settings.dock.lock_position && settings.window.docked.is_some();
        h_flex()
            .h(px(56.))
            .pl_4()
            .pr_3()
            .gap_2()
            .child(
                h_flex()
                    .id("drag-area")
                    .flex_1()
                    .h_full()
                    .when(!locked, |el| el.window_control_area(WindowControlArea::Drag))
                    .child(img(LOGO.clone()).size(px(26.))),
            )
            .child(Input::new(&self.search).w(px(240.)).prefix(Icon::new(IconName::Search)).cleanable(true))
            .child(
                Button::new("settings")
                    .ghost()
                    .icon(IconName::Settings)
                    .on_click(|_, window, cx| window.dispatch_action(Box::new(OpenSettings), cx)),
            )
            .child(
                // 收进托盘, 程序继续在后台运行
                Button::new("close")
                    .ghost()
                    .icon(IconName::Close)
                    .on_click(cx.listener(|this, _, _, _| native::hide(this.hwnd))),
            )
    }
}
