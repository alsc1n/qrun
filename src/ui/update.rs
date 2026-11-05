//! 更新窗口: 新版本号, 更新说明 (Markdown, 限高滚动), 稍后 / 跳过此版本 / 立即更新; 下载时换成进度条和取消.
//! 和设置页的关于读写同一个 Updater, 两边显示的状态始终一致.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::progress::Progress;
use gpui_kit::component::text::TextView;
use gpui_kit::component::{ActiveTheme, StyledExt, h_flex, v_flex};
use gpui_kit::*;

use crate::app::update::{Status, Updater};
use crate::model::settings::Settings;
use crate::platform::window as native;

pub struct UpdateView {
    settings: Entity<Settings>,
    updater: Entity<Updater>,
    /// 系统给窗口画的底色, 跟着界面背景色; 见 `window::sync_backdrop`.
    backdrop: Option<Hsla>,
    _subscriptions: Vec<Subscription>,
}

impl UpdateView {
    pub fn new(settings: Entity<Settings>, updater: Entity<Updater>, _: &mut Window, cx: &mut Context<Self>) -> Self {
        let _subscriptions = vec![cx.observe(&updater, |_, _, cx| cx.notify())];
        Self { settings, updater, backdrop: None, _subscriptions }
    }

    /// 底部一行: 下载中是进度条和取消, 校验和安装时只有一句状态, 其余时候是三个按钮.
    fn render_footer(&self, version: &str, cx: &mut Context<Self>) -> AnyElement {
        let updater = self.updater.read(cx);
        let theme = cx.theme();
        match &updater.status {
            Status::Downloading { received, total } => v_flex()
                .gap_2()
                .child(Progress::new("download").value(percent(*received, *total)))
                .child(
                    h_flex()
                        .justify_between()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(progress_text(*received, *total))
                        .child(Button::new("cancel").outline().label("取消").on_click(
                            cx.listener(|this, _, _, cx| this.updater.update(cx, |updater, _| updater.cancel())),
                        )),
                )
                .into_any_element(),
            Status::Verifying => status_line("正在校验安装包...", theme.muted_foreground),
            Status::Installing => status_line("正在安装, QRun 即将重启...", theme.muted_foreground),
            Status::Checking => status_line("正在检查...", theme.muted_foreground),
            status => {
                let failed = matches!(status, Status::Failed(_));
                let label = match (updater.installed, failed) {
                    (false, _) => "打开下载页",
                    (true, false) => "立即更新",
                    (true, true) => "重试",
                };
                let installed = updater.installed;
                let skip_version = version.to_string();
                v_flex()
                    .gap_3()
                    .children(match status {
                        Status::Failed(error) => Some(div().text_sm().text_color(theme.danger).child(error.clone())),
                        _ => None,
                    })
                    .child(
                        h_flex()
                            .gap_2()
                            .child(Button::new("skip").ghost().label("跳过此版本").on_click(cx.listener(
                                move |this, _, window, cx| {
                                    let version = skip_version.clone();
                                    this.settings
                                        .update(cx, |s, cx| s.change(cx, |s| s.general.skipped_version = version));
                                    window.remove_window();
                                },
                            )))
                            .child(div().flex_1())
                            .child(
                                Button::new("later")
                                    .outline()
                                    .label("稍后")
                                    .on_click(|_, window, _| window.remove_window()),
                            )
                            .child(Button::new("install").primary().label(label).on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.updater.update(cx, |updater, cx| updater.install(cx));
                                    // 绿色版只是打开下载页, 窗口没有后续要显示的了
                                    if !installed {
                                        window.remove_window();
                                    }
                                },
                            ))),
                    )
                    .into_any_element()
            }
        }
    }
}

impl Render for UpdateView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (bg, fg, muted_fg, border) = (theme.background, theme.foreground, theme.muted_foreground, theme.border);
        native::sync_backdrop(&mut self.backdrop, window, bg);
        let root = v_flex().size_full().p_5().gap_3().bg(bg).text_color(fg);

        // 检查后发现已经没有更新的版本 (比如别处又检查了一次)
        let Some(release) = self.updater.read(cx).release.clone() else {
            return root
                .items_center()
                .justify_center()
                .text_sm()
                .child("已是最新版本.")
                .child(Button::new("close").outline().label("关闭").on_click(|_, window, _| window.remove_window()));
        };
        let hint = if self.updater.read(cx).installed {
            "更新时会先退出 QRun, 装完自动重新打开."
        } else {
            "点击后在浏览器打开下载页."
        };
        let notes = if release.notes.is_empty() {
            div().text_sm().text_color(muted_fg).child("此版本没有附带更新说明.").into_any_element()
        } else {
            TextView::markdown("notes", release.notes.clone()).text_sm().into_any_element()
        };

        root.child(div().text_lg().font_semibold().child(format!("QRun v{} 可更新", release.version)))
            .child(
                div().text_sm().text_color(muted_fg).child(format!("当前版本 v{}. {hint}", env!("CARGO_PKG_VERSION"))),
            )
            // 说明长度不定: 占满中间剩余高度, 超出滚动, 不把窗口撑大
            .child(
                div()
                    .id("notes-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_3()
                    .rounded_lg()
                    .border_1()
                    .border_color(border)
                    .child(notes),
            )
            .child(self.render_footer(&release.version, cx))
    }
}

fn status_line(text: &'static str, color: Hsla) -> AnyElement {
    div().text_sm().text_color(color).child(text).into_any_element()
}

/// 进度条的百分比; 不知道总大小时停在 0.
pub fn percent(received: u64, total: Option<u64>) -> f32 {
    total.map_or(0., |total| received as f32 / total as f32 * 100.)
}

/// "12.3 / 18.0 MB"; 不知道总大小时只写已下载多少.
pub fn progress_text(received: u64, total: Option<u64>) -> String {
    let mb = |bytes: u64| bytes as f64 / (1024. * 1024.);
    match total {
        Some(total) => format!("{:.1} / {:.1} MB", mb(received), mb(total)),
        None => format!("已下载 {:.1} MB", mb(received)),
    }
}
