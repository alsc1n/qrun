#![windows_subsystem = "windows"]

mod actions;
mod app;
mod logger;
mod model;
mod params;
mod platform;
mod storage;
mod ui;

use model::library::Library;
use model::settings::Settings;

fn main() {
    // 没有控制台, 发布版 panic 后直接结束进程; 先写日志再弹框写明出错位置和原因, 用户才能反馈.
    // 先装钩子再开日志: 日志文件建不了时也要弹框, 不能悄无声息地退出
    std::panic::set_hook(Box::new(|info| {
        log::error!("{info}");
        platform::error_box(&format!("QRun 出错, 即将退出. 请把下面的内容反馈给开发者:\n\n{info}"))
    }));
    // 开发用: 只读回放历史, 在启动它的终端里打印新算法和当时的命中率, 不开界面 (可与正在运行的 QRun 同时用)
    if std::env::args().any(|arg| arg == "--replay") {
        platform::attach_console();
        storage::locate();
        match model::library::report() {
            Ok(report) => println!("{report}"),
            Err(message) => eprintln!("{message}"),
        }
        return;
    }
    // 只开一个: 已在运行就让它显示面板, 自己退出. 要在开日志前判断, 否则会去改名正在运行的那个开着的 qrun.log.
    // 重启 (如更改数据目录后) 时旧的正在退出, 等它退完
    let restarting = std::env::args().any(|arg| arg == platform::install::RESTART_ARG);
    if !(platform::first_instance() || restarting && platform::wait_first_instance()) {
        platform::tray::notify_running();
        return;
    }
    // 定下数据目录 (安装版可能要先搬数据), 日志也写在那里
    if let Err(message) = storage::prepare() {
        platform::error_box(&message);
        return;
    }
    logger::init();
    platform::env::reset_to_user_default();

    // 配置读不了时说明原因并退出, 不带着坏数据启动
    let (settings, library) = match (Settings::load(), Library::load()) {
        (Ok(settings), Ok(library)) => (settings, library),
        (Err(message), _) | (_, Err(message)) => {
            platform::error_box(&message);
            return;
        }
    };

    gpui_kit::application().with_assets(gpui_kit::assets::Assets).run(move |cx| {
        gpui_kit::init(cx);
        app::run(settings, library, cx);
    });
}
