//! 关于: 版本和更新, 开源地址, 更新日志, 许可证.

use gpui_kit::component::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::progress::Progress;
use gpui_kit::component::setting::{SettingField, SettingGroup, SettingItem, SettingPage};
use gpui_kit::component::{Disableable, Sizable, h_flex};
use gpui_kit::*;

use super::handle;
use crate::actions::OpenUpdate;
use crate::app::update::{Status, Updater};
use crate::model::settings::Settings;
use crate::params::PARAMS;
use crate::ui::update::{percent, progress_text};

const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");

pub fn page(settings: &Entity<Settings>, updater: &Entity<Updater>, cx: &App) -> SettingPage {
    let (read, write) = handle(settings);

    SettingPage::new("关于").icon(IconName::Info).group(
        SettingGroup::new()
            .item(update_item(settings, updater, cx))
            .item(
                SettingItem::new(
                    "自动检查更新",
                    SettingField::switch(
                        move |cx| read.read(cx).general.auto_check_updates,
                        move |value, cx| {
                            write.update(cx, |s, cx| s.change(cx, |s| s.general.auto_check_updates = value))
                        },
                    ),
                )
                .description(format!("每 {} 小时检查一次, 发现新版本时通知.", PARAMS.update.check_interval_hours)),
            )
            .item(SettingItem::new(
                "开源地址",
                SettingField::render(|_, _, _| {
                    Button::new("repository").link().label(REPOSITORY).on_click(|_, _, cx| cx.open_url(REPOSITORY))
                }),
            ))
            .item(SettingItem::new(
                "更新日志",
                SettingField::render(|_, _, _| {
                    // 发行版页面只列已发布的版本, 正文取自 CHANGELOG 对应一节; CHANGELOG.md 本身顶部是 "未发布"
                    Button::new("changelog")
                        .link()
                        .label("查看各版本改动")
                        .on_click(|_, _, cx| cx.open_url(&format!("{REPOSITORY}/releases")))
                }),
            ))
            .item(SettingItem::new("许可证", SettingField::render(|_, _, _| env!("CARGO_PKG_LICENSE")))),
    )
}

/// 当前版本和更新状态. 有新版本时直接给 "更新说明" 和 "立即更新" (绿色版为 "打开下载页"), 不用再点检查;
/// 下载中显示进度条和取消, 失败显示原因和重试. 和更新窗口读写同一个 Updater.
fn update_item(settings: &Entity<Settings>, updater: &Entity<Updater>, cx: &App) -> SettingItem {
    let state = updater.read(cx);
    let title = format!("当前版本 v{}", env!("CARGO_PKG_VERSION"));
    let Some(release) = &state.release else {
        let description = match &state.status {
            Status::Checking => "正在检查...".into(),
            Status::UpToDate => "已是最新版本.".into(),
            Status::Failed(e) => format!("检查失败: {e}"),
            _ => String::new(),
        };
        let (busy, updater) = (state.busy(), updater.clone());
        return SettingItem::new(
            title,
            SettingField::render(move |_, _, _| {
                let updater = updater.clone();
                Button::new("check")
                    .outline()
                    .small()
                    .label("检查更新")
                    .disabled(busy)
                    .on_click(move |_, _, cx| updater.update(cx, |updater, cx| updater.check(true, cx)))
            }),
        )
        .description(description);
    };

    let skipped = settings.read(cx).general.skipped_version == release.version;
    let description = match &state.status {
        Status::Downloading { received, total } => {
            format!("正在下载 v{}: {}", release.version, progress_text(*received, *total))
        }
        Status::Verifying => "正在校验安装包...".into(),
        Status::Installing => "正在安装, QRun 即将重启...".into(),
        Status::Failed(e) => format!("更新失败: {e}"),
        _ if skipped => format!("发现新版本 v{} (已跳过).", release.version),
        _ => format!("发现新版本 v{}.", release.version),
    };
    let field = match &state.status {
        Status::Downloading { received, total } => {
            let (value, updater) = (percent(*received, *total), updater.clone());
            SettingField::render(move |_, _, _| {
                let updater = updater.clone();
                h_flex().gap_2().child(Progress::new("download").value(value).w(px(100.))).child(
                    Button::new("cancel")
                        .outline()
                        .small()
                        .label("取消")
                        .on_click(move |_, _, cx| updater.update(cx, |updater, _| updater.cancel())),
                )
            })
        }
        Status::Verifying | Status::Installing => SettingField::render(|_, _, _| div()),
        status => {
            let label = match (state.installed, matches!(status, Status::Failed(_))) {
                (false, _) => "打开下载页",
                (true, false) => "立即更新",
                (true, true) => "重试",
            };
            let updater = updater.clone();
            SettingField::render(move |_, _, _| {
                let updater = updater.clone();
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("notes")
                            .outline()
                            .small()
                            .label("更新说明")
                            .on_click(|_, window, cx| window.dispatch_action(Box::new(OpenUpdate), cx)),
                    )
                    .child(
                        Button::new("install")
                            .primary()
                            .small()
                            .label(label)
                            .on_click(move |_, _, cx| updater.update(cx, |updater, cx| updater.install(cx))),
                    )
            })
        }
    };
    SettingItem::new(title, field).description(description)
}
