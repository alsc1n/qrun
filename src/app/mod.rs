//! 应用级: 打开窗口 (面板, 设置), 分发托盘命令和应用动作, 驱动贴边和定时检查更新.

mod dock;
mod sensors;
pub mod update;

use std::borrow::Cow;
use std::fs;
use std::time::{Duration, Instant};

use gpui_kit::component::Theme;
use gpui_kit::*;
use windows::Win32::Foundation::HWND;

use crate::actions::{OpenSettings, OpenUpdate};
use crate::model::library::Library;
use crate::model::settings::Settings;
use crate::params::PARAMS;
use crate::platform::shell;
use crate::platform::tray::{self, Tray};
use crate::platform::window as native;
use crate::ui::panel::Panel;
use crate::ui::settings::SettingsView;
use crate::ui::update::UpdateView;
use dock::Dock;
use sensors::Sensors;
use update::Updater;

/// 轮询托盘命令和鼠标位置的间隔.
const POLL: Duration = Duration::from_millis(50);

/// 面板最小尺寸: 标题栏放得下 240 宽的搜索框和两个按钮, 下面至少一行分组标签和一行图标.
const PANEL_MIN_SIZE: Size<Pixels> = size(px(420.), px(300.));
/// 设置窗口默认和最小尺寸: 最小时左栏 180, 内容区约 420, 设置项仍能左右排开.
const SETTINGS_SIZE: Size<Pixels> = size(px(720.), px(520.));
const SETTINGS_MIN_SIZE: Size<Pixels> = size(px(600.), px(420.));
/// 窗口打开时最多占主屏可用区域 (不含任务栏) 的比例.
const MAX_SCREEN_RATIO: f32 = 0.9;

/// 全局共享的数据, 各窗口都从这里拿同一份 Entity.
struct Shared {
    settings: Entity<Settings>,
    /// 设置页的推荐命中率从这里读.
    library: Entity<Library>,
    updater: Entity<Updater>,
    /// 已打开的设置窗口; 再次打开时激活它而不是新开一个.
    settings_window: Option<AnyWindowHandle>,
    /// 已打开的更新窗口, 同上.
    update_window: Option<AnyWindowHandle>,
}

impl Global for Shared {}

pub fn run(settings: Settings, library: Library, cx: &mut App) {
    let saved = size(px(settings.window.width), px(settings.window.height));
    let settings = cx.new(|_| settings);
    let library = cx.new(|cx| {
        let mut library = library;
        library.start_tuning(cx);
        library
    });
    let updater = cx.new(|_| Updater::new());
    cx.set_global(Shared {
        settings: settings.clone(),
        library: library.clone(),
        updater: updater.clone(),
        settings_window: None,
        update_window: None,
    });
    cx.on_action(|_: &OpenSettings, cx| open_settings(cx));
    cx.on_action(|_: &OpenUpdate, cx| open_update(cx));
    load_fonts(cx);
    follow_font(&settings, cx);

    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, fit_screen(saved, PANEL_MIN_SIZE, cx), cx))),
        window_min_size: Some(PANEL_MIN_SIZE),
        titlebar: Some(TitlebarOptions { title: Some("QRun".into()), appears_transparent: true, ..Default::default() }),
        // 先隐藏创建, 改完任务栏样式再显示
        show: false,
        ..Default::default()
    };
    let panel_settings = settings.clone();
    let panel_library = library.clone();
    let (window, panel) = gpui_kit::open_window(options, cx, |window, cx| {
        let hwnd = native::hwnd(window);
        native::hide_from_taskbar(hwnd);
        native::remove_caption_buttons(hwnd);
        native::widen_resize_border(hwnd);
        // 首次显示会用居中的初始位置, 隐身到 poll 里挪回贴边位置后再现身, 否则屏幕中间会闪一下
        native::set_cloaked(hwnd, true);
        window.activate_window();
        cx.new(|cx| Panel::new(panel_library, panel_settings, window, cx))
    })
    .expect("open panel window");

    let hwnd = panel.read(cx).hwnd;
    poll(window, hwnd, panel, settings, library, updater, cx);
}

/// 注册 exe 旁 fonts 目录里随程序发布的字体 (默认的 HarmonyOS Sans SC), 只对 QRun 生效, 不装进系统.
/// 目录不在 (比如只拷走了 exe) 时不注册, 界面按 apply_font 的规则用系统字体.
fn load_fonts(cx: &mut App) {
    let dir = std::env::current_exe().expect("locate exe").with_file_name("fonts");
    let Ok(entries) = fs::read_dir(&dir) else {
        return;
    };
    let fonts = entries
        .map(|entry| entry.expect("list fonts").path())
        .filter(|path| path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("ttf")))
        .map(|path| fs::read(&path).map(Cow::Owned).expect("read font"))
        .collect();
    cx.text_system().add_fonts(fonts).expect("register fonts");
}

/// 界面字体跟随设置: 启动时套用一次, 之后设置里换了字体再套用.
fn follow_font(settings: &Entity<Settings>, cx: &mut App) {
    let system = Theme::global(cx).font_family.clone();
    let mut applied = settings.read(cx).appearance.font_family.clone();
    apply_font(&applied, &system, cx);
    cx.observe(settings, move |settings, cx| {
        let font = settings.read(cx).appearance.font_family.clone();
        if font != applied {
            apply_font(&font, &system, cx);
            applied = font;
        }
    })
    .detach();
}

/// 本机装了或 fonts 目录里带了这个字体才用它, 否则用系统默认: 主题会原样使用设置的字体名, 不存在的字体会让排版出错.
fn apply_font(name: &str, system: &SharedString, cx: &mut App) {
    let installed = cx.text_system().all_font_names().iter().any(|font| font == name);
    Theme::global_mut(cx).font_family = if installed { name.to_string().into() } else { system.clone() };
    cx.refresh_windows();
}

/// 打开窗口时的大小: 不超过主屏可用区域的 MAX_SCREEN_RATIO, 也不小于 min.
/// 在大屏上调大保存后换到小屏 (或调高缩放比例) 启动, 窗口不会超出屏幕. 只在打开时限制一次, 之后拖边框改大小由系统管.
fn fit_screen(wanted: Size<Pixels>, min: Size<Pixels>, cx: &App) -> Size<Pixels> {
    let screen = cx.primary_display().expect("primary display").visible_bounds().size;
    wanted.min(&size(screen.width * MAX_SCREEN_RATIO, screen.height * MAX_SCREEN_RATIO)).max(&min)
}

/// 设置窗口只开一个, 已开着就激活.
fn open_settings(cx: &mut App) {
    if let Some(handle) = cx.global::<Shared>().settings_window
        && handle.update(cx, |_, window, _| window.activate_window()).is_ok()
    {
        return;
    }
    let shared = cx.global::<Shared>();
    let (settings, library, updater) = (shared.settings.clone(), shared.library.clone(), shared.updater.clone());
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            fit_screen(SETTINGS_SIZE, SETTINGS_MIN_SIZE, cx),
            cx,
        ))),
        window_min_size: Some(SETTINGS_MIN_SIZE),
        titlebar: Some(TitlebarOptions { title: Some("设置".into()), ..Default::default() }),
        ..Default::default()
    };
    let (handle, _) = gpui_kit::open_window(options, cx, |window, cx| {
        cx.new(|cx| SettingsView::new(settings, library, updater, window, cx))
    })
    .expect("open settings window");
    cx.global_mut::<Shared>().settings_window = Some(handle);
}

/// 更新窗口同样只开一个. QRun 平时收在托盘或贴边藏着, 提醒放在面板里常常看不到, 所以单独一个窗口.
fn open_update(cx: &mut App) {
    if let Some(handle) = cx.global::<Shared>().update_window
        && handle.update(cx, |_, window, _| window.activate_window()).is_ok()
    {
        return;
    }
    let shared = cx.global::<Shared>();
    let (settings, updater) = (shared.settings.clone(), shared.updater.clone());
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(440.), px(440.)), cx))),
        titlebar: Some(TitlebarOptions { title: Some("更新".into()), ..Default::default() }),
        is_resizable: false,
        ..Default::default()
    };
    let (handle, _) =
        gpui_kit::open_window(options, cx, |window, cx| cx.new(|cx| UpdateView::new(settings, updater, window, cx)))
            .expect("open update window");
    cx.global_mut::<Shared>().update_window = Some(handle);
}

/// 托盘消息, 贴边和前台窗口都只能从 Win32 读到, 没有 GPUI 事件可订阅, 所以定时轮询; 定时检查更新和时间线采集也挂在这里.
fn poll(
    window: AnyWindowHandle,
    hwnd: HWND,
    panel: Entity<Panel>,
    settings: Entity<Settings>,
    library: Entity<Library>,
    updater: Entity<Updater>,
    cx: &mut App,
) {
    let tray = Tray::new();
    let mut sensors = Sensors::new();
    let mut dock = Dock::new(hwnd, settings.read(cx).dock, settings.read(cx).window.docked);
    let check_interval = Duration::from_secs(PARAMS.update.check_interval_hours * 3600);
    let mut next_check = Instant::now();
    // 本次运行已提醒过的版本: 点了"稍后"的版本下次启动才再自动提醒
    let mut reminded: Option<String> = None;
    // 当前是否置顶; 只在要变时才调 SetWindowPos
    let mut topmost = false;
    cx.spawn(async move |cx| {
        cx.background_executor().timer(POLL).await;
        dock.restore();
        native::set_cloaked(hwnd, false);
        loop {
            cx.background_executor().timer(POLL).await;
            match tray::take_command() {
                Some(tray::Command::Show) => {
                    native::show(hwnd);
                    dock.reveal();
                    window.update(cx, |_, window, _| window.activate_window()).ok();
                }
                Some(tray::Command::Settings) => cx.update(open_settings),
                Some(tray::Command::Notification) => cx.update(open_update),
                // 再次启动 QRun: 显示面板; 贴着边的滑到边外一点并解除吸附, 不再藏回去.
                // 锁定了贴边位置时不解除, 只滑出来
                Some(tray::Command::LaunchedAgain) => {
                    native::show(hwnd);
                    if settings.read_with(cx, |s, _| s.dock.lock_position) {
                        dock.reveal();
                    } else if dock.undock() {
                        settings.update(cx, |settings, cx| settings.change(cx, |s| s.window.docked = None));
                    }
                    window.update(cx, |_, window, _| window.activate_window()).ok();
                }
                // 退出 (托盘菜单, 自动更新装包前): 先把时间线写进库, 重启后接着用
                Some(tray::Command::Quit) => {
                    library.update(cx, |library, _| library.save_timeline());
                    tray.remove();
                    cx.update(|cx| cx.quit());
                }
                None => {}
            }
            dock.set_settings(settings.read_with(cx, |settings, _| settings.dock));
            if let Some(docked) = dock.tick() {
                settings.update(cx, |settings, cx| settings.change(cx, |s| s.window.docked = docked));
            }
            // 贴边时总是置顶, 否则看"置于顶层"设置
            let wanted = settings.read_with(cx, |s, _| s.window.docked.is_some() || s.general.always_on_top);
            if wanted != topmost {
                native::set_topmost(hwnd, wanted);
                topmost = wanted;
            }
            // 面板从托盘显示或从边外滑出算打开一次; 顺带跟踪前台窗口给 Jev 预取
            let presented = native::is_visible(hwnd) && !dock.is_hidden();
            panel.update(cx, |panel, cx| panel.tick(presented, cx));
            // 拖着项目或推荐页的格子出了面板: 交给系统拖放, 才能放进别的程序; 系统接管到松手才返回.
            // 要在 update 之外调: 拖放期间系统照常给 GPUI 窗口发消息, 处理消息时要借用 App.
            // 先收掉面板里跟着光标的名称框, 拖放期间只显示系统的拖动图像
            if panel.read_with(cx, |panel, cx| panel.dragging(cx)) && dock::dragged_outside(hwnd) {
                window.update(cx, |_, window, cx| cx.stop_active_drag(window)).ok();
                let file = panel.update(cx, |panel, cx| panel.start_system_drag(cx));
                let dropped = file.is_some_and(|file| shell::drag_out(hwnd, &file));
                panel.update(cx, |panel, cx| panel.dragged_out(dropped, cx));
            }
            cx.update(|cx| {
                let smart = settings.read(cx).smart.clone();
                sensors.tick(&library, &smart, cx)
            });

            if settings.read_with(cx, |s, _| s.general.auto_check_updates) && Instant::now() >= next_check {
                next_check = Instant::now() + check_interval;
                updater.update(cx, |updater, cx| updater.check(false, cx));
            }
            // 刚检查到新版本: 手动检查一定提醒; 自动检查跳过用户选了"跳过此版本"的, 以及本次运行已提醒过的
            let offer = updater.update(cx, |updater, _| {
                let asked = updater.offer.take()?;
                Some((asked, updater.release.as_ref()?.version.clone()))
            });
            if let Some((asked, version)) = offer {
                let skipped = settings.read_with(cx, |s, _| s.general.skipped_version == version);
                if asked || (!skipped && reminded.as_ref() != Some(&version)) {
                    if !asked {
                        tray.notify("QRun 有新版本", &format!("v{version} 已发布, 点击查看更新说明."));
                    }
                    reminded = Some(version);
                    cx.update(open_update);
                }
            }
        }
    })
    .detach();
}
