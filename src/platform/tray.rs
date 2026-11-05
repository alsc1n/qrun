//! 系统托盘图标: 左键单击发出"显示面板", 右键菜单可选显示面板, 设置或退出; 点气泡通知打开设置.
//! 托盘消息发到一个隐藏的窗口, 由 GPUI 主线程的消息循环分发, 结果放进 COMMAND 供轮询读取.
//! 再次启动的 QRun 也经这个窗口 (按类名找到) 通知正在运行的这个.

use std::sync::atomic::{AtomicU8, Ordering};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE, NIM_MODIFY, NIN_BALLOONUSERCLICK,
    NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, FindWindowW,
    GetCursorPos, GetSystemMetrics, GetWindowThreadProcessId, HICON, IMAGE_ICON, LR_DEFAULTCOLOR, LoadImageW,
    MF_STRING, PostMessageW, RegisterClassW, SM_CXSMICON, SM_CYSMICON, SetForegroundWindow, TPM_RETURNCMD,
    TPM_RIGHTBUTTON, TrackPopupMenu, WINDOW_STYLE, WM_APP, WM_LBUTTONUP, WM_RBUTTONUP, WNDCLASSW, WS_EX_TOOLWINDOW,
};
use windows::core::{PCWSTR, w};

const WM_TRAY: u32 = WM_APP + 1;
/// 又启动了一个 QRun, 由那个进程发来 (见 `notify_running`).
const WM_LAUNCHED_AGAIN: u32 = WM_APP + 2;
const CLASS: PCWSTR = w!("QRunTray");
const NONE: u8 = 0;
const SHOW: u8 = 1;
const QUIT: u8 = 2;
const SETTINGS: u8 = 3;
/// 点了系统通知 (目前只有新版本提醒).
const NOTIFICATION: u8 = 4;
const LAUNCHED_AGAIN: u8 = 5;

static COMMAND: AtomicU8 = AtomicU8::new(NONE);

pub enum Command {
    Show,
    Settings,
    Quit,
    Notification,
    /// 已在运行时用户又启动了一次 QRun.
    LaunchedAgain,
}

pub struct Tray {
    hwnd: HWND,
}

impl Tray {
    pub fn new() -> Self {
        unsafe {
            let instance = GetModuleHandleW(None).expect("module handle");
            let class = WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                hInstance: instance.into(),
                lpszClassName: CLASS,
                ..Default::default()
            };
            RegisterClassW(&class);
            // 不显示的普通顶层窗口; 弹出菜单需要它能成为前台窗口, 所以不用 message-only 窗口
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW,
                CLASS,
                None,
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                None,
                None,
                Some(instance.into()),
                None,
            )
            .expect("create tray window");

            // 取 exe 里资源 ID 为 1 的图标 (见 build.rs), 按小图标尺寸加载, 托盘里才清晰
            let icon = LoadImageW(
                Some(instance.into()),
                // MAKEINTRESOURCE(1): 资源 ID 当指针传
                PCWSTR(std::ptr::without_provenance(1)),
                IMAGE_ICON,
                GetSystemMetrics(SM_CXSMICON),
                GetSystemMetrics(SM_CYSMICON),
                LR_DEFAULTCOLOR,
            )
            .expect("load tray icon");
            let mut data = notify_data(hwnd);
            data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
            data.uCallbackMessage = WM_TRAY;
            data.hIcon = HICON(icon.0);
            for (dst, src) in data.szTip.iter_mut().zip("QRun".encode_utf16()) {
                *dst = src;
            }
            Shell_NotifyIconW(NIM_ADD, &data).ok().expect("add tray icon");
            Self { hwnd }
        }
    }

    /// 退出前移除, 否则图标会残留到鼠标划过才消失.
    pub fn remove(&self) {
        let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &notify_data(self.hwnd)) };
    }

    /// 弹出系统通知; 用户点击通知时发出 `Command::Notification`.
    pub fn notify(&self, title: &str, text: &str) {
        let mut data = notify_data(self.hwnd);
        data.uFlags = NIF_INFO;
        data.dwInfoFlags = NIIF_INFO;
        for (dst, src) in data.szInfoTitle.iter_mut().zip(title.encode_utf16()) {
            *dst = src;
        }
        for (dst, src) in data.szInfo.iter_mut().zip(text.encode_utf16()) {
            *dst = src;
        }
        let _ = unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) };
    }
}

/// 请求退出, 由轮询照托盘菜单"退出"同样处理 (先移除托盘图标再退出).
pub fn request_quit() {
    COMMAND.store(QUIT, Ordering::Relaxed);
}

/// 取出最近一次托盘操作.
pub fn take_command() -> Option<Command> {
    match COMMAND.swap(NONE, Ordering::Relaxed) {
        SHOW => Some(Command::Show),
        SETTINGS => Some(Command::Settings),
        QUIT => Some(Command::Quit),
        NOTIFICATION => Some(Command::Notification),
        LAUNCHED_AGAIN => Some(Command::LaunchedAgain),
        _ => None,
    }
}

fn notify_data(hwnd: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW { cbSize: size_of::<NOTIFYICONDATAW>() as u32, hWnd: hwnd, uID: 1, ..Default::default() }
}

/// 已有 QRun 在运行时调用: 告诉它用户又启动了一次, 由它把面板显示出来. 它的托盘窗口还没建好 (刚启动) 时什么也不做.
pub fn notify_running() {
    unsafe {
        if let Ok(hwnd) = FindWindowW(CLASS, PCWSTR::null()) {
            // 刚被用户启动的进程才有资格把窗口提到前台, 把这个资格让给正在运行的那个, 它的面板才能激活
            let mut pid = 0;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            let _ = AllowSetForegroundWindow(pid);
            let _ = PostMessageW(Some(hwnd), WM_LAUNCHED_AGAIN, WPARAM(0), LPARAM(0));
        }
    }
}

unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_LAUNCHED_AGAIN {
        COMMAND.store(LAUNCHED_AGAIN, Ordering::Relaxed);
        return LRESULT(0);
    }
    if msg != WM_TRAY {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }
    // lparam 低 16 位是托盘图标上发生的鼠标消息
    match lparam.0 as u32 & 0xFFFF {
        WM_LBUTTONUP => COMMAND.store(SHOW, Ordering::Relaxed),
        NIN_BALLOONUSERCLICK => COMMAND.store(NOTIFICATION, Ordering::Relaxed),
        WM_RBUTTONUP => {
            let picked = unsafe { show_menu(hwnd) };
            COMMAND.store(picked, Ordering::Relaxed);
        }
        _ => {}
    }
    LRESULT(0)
}

unsafe fn show_menu(hwnd: HWND) -> u8 {
    unsafe {
        let menu = CreatePopupMenu().expect("create menu");
        AppendMenuW(menu, MF_STRING, SHOW as usize, w!("显示面板")).expect("append menu");
        AppendMenuW(menu, MF_STRING, SETTINGS as usize, w!("设置")).expect("append menu");
        AppendMenuW(menu, MF_STRING, QUIT as usize, w!("退出")).expect("append menu");
        let mut point = POINT::default();
        GetCursorPos(&mut point).expect("cursor");
        // 托盘菜单的固定写法: 先让所属窗口成为前台, 点菜单外面时菜单才会关闭
        let _ = SetForegroundWindow(hwnd);
        let picked = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, point.x, point.y, None, hwnd, None);
        DestroyMenu(menu).expect("destroy menu");
        picked.0 as u8
    }
}
