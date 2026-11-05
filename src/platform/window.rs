//! 窗口操作: 句柄, 任务栏样式, 显示隐藏, 置顶, 位置. 坐标都是物理像素.

use std::ffi::c_void;

use gpui_kit::{Hsla, Rgba, Window};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWMWA_CLOAK, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute, DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromRect};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GWL_STYLE, GetWindowLongPtrW, GetWindowRect, HTBOTTOM, HTBOTTOMLEFT, HTBOTTOMRIGHT, HTLEFT, HTNOWHERE,
    HTRIGHT, HTTOP, HTTOPLEFT, HTTOPRIGHT, HWND_NOTOPMOST, HWND_TOPMOST, IsWindowVisible, IsZoomed, SW_HIDE, SW_SHOW,
    SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SetWindowLongPtrW, SetWindowPos,
    ShowWindow, WINDOW_EX_STYLE, WINDOW_STYLE, WM_NCHITTEST, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW, WS_MAXIMIZEBOX,
    WS_MINIMIZEBOX, WS_SYSMENU,
};
use windows::core::BOOL;

pub fn hwnd(window: &Window) -> HWND {
    // Window 自己也有 window_handle 方法, 要显式调用 raw-window-handle 的版本
    let RawWindowHandle::Win32(handle) = HasWindowHandle::window_handle(window).expect("window handle").as_raw() else {
        unreachable!("QRun only runs on Windows");
    };
    HWND(handle.hwnd.get() as *mut _)
}

/// 不在任务栏显示: 去掉 APPWINDOW, 加上 TOOLWINDOW. 要在窗口第一次显示前调用.
pub fn hide_from_taskbar(hwnd: HWND) {
    unsafe {
        let style = WINDOW_EX_STYLE(GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32);
        let style = (style & !WS_EX_APPWINDOW) | WS_EX_TOOLWINDOW;
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style.0 as isize);
    }
}

/// 去掉系统菜单和最小化, 最大化按钮的样式 (保留可拖边改大小的 THICKFRAME). 要在窗口第一次显示前调用.
/// GPUI 建窗口时会带上这些样式, 系统就在窗口框上画关闭按钮; 面板用自绘标题栏, 这个框被面板盖住,
/// 但快速拖动窗口时两者错开, 会露出透明的系统框和它右上角的叉号.
pub fn remove_caption_buttons(hwnd: HWND) {
    unsafe {
        let style = WINDOW_STYLE(GetWindowLongPtrW(hwnd, GWL_STYLE) as u32);
        let style = style & !(WS_SYSMENU | WS_MINIMIZEBOX | WS_MAXIMIZEBOX);
        SetWindowLongPtrW(hwnd, GWL_STYLE, style.0 as isize);
        // 改了样式要通知系统重算窗口框
        SetWindowPos(
            hwnd,
            None,
            0,
            0,
            0,
            0,
            SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        )
        .expect("apply window style");
    }
}

/// 窗口底色跟着界面背景色: 颜色变了 (首次, 换主题) 才重新设置, last 记着上次设的颜色. 在视图的 render 里调用.
/// GPUI 的画面总比窗口位置, 大小的变化晚一帧, 这一帧里新露出的区域没有内容 (透明), 拖动窗口时像多了一层.
/// 让系统先用界面背景色填满窗口, 露出来的也是同一个颜色, 就看不出来.
pub fn sync_backdrop(last: &mut Option<Hsla>, window: &Window, color: Hsla) {
    if *last != Some(color) {
        set_backdrop_color(hwnd(window), color.into());
        *last = Some(color);
    }
}

/// 让系统用纯色画窗口底层 (ACCENT_ENABLE_GRADIENT), 随窗口位置和大小同步, 不等 GPUI 下一帧.
/// 与 GPUI 设毛玻璃一样用未公开的 SetWindowCompositionAttribute.
fn set_backdrop_color(hwnd: HWND, color: Rgba) {
    #[repr(C)]
    struct AccentPolicy {
        state: u32,
        flags: u32,
        gradient_color: u32,
        animation_id: u32,
    }
    #[repr(C)]
    struct CompositionData {
        attribute: u32,
        data: *mut c_void,
        size: usize,
    }
    windows::core::link!("user32.dll" "system" fn SetWindowCompositionAttribute(hwnd: HWND, data: *mut CompositionData) -> BOOL);

    let channel = |value: f32| (value.clamp(0., 1.) * 255.).round() as u32;
    let accent = AccentPolicy {
        // ACCENT_ENABLE_GRADIENT: 纯色, 不模糊
        state: 1,
        flags: 0,
        // 颜色是 ABGR
        gradient_color: channel(color.r) | channel(color.g) << 8 | channel(color.b) << 16 | channel(color.a) << 24,
        animation_id: 0,
    };
    let mut data = CompositionData {
        // WCA_ACCENT_POLICY
        attribute: 0x13,
        data: &accent as *const AccentPolicy as *mut c_void,
        size: size_of::<AccentPolicy>(),
    };
    let _ = unsafe { SetWindowCompositionAttribute(hwnd, &mut data) };
}

pub fn show(hwnd: HWND) {
    let _ = unsafe { ShowWindow(hwnd, SW_SHOW) };
}

pub fn hide(hwnd: HWND) {
    let _ = unsafe { ShowWindow(hwnd, SW_HIDE) };
}

/// 隐身: 窗口照常显示和绘制, 只是 DWM 不把它合成到屏幕上. 用来在用户看到之前先把窗口挪到位.
pub fn set_cloaked(hwnd: HWND, cloaked: bool) {
    let value = BOOL::from(cloaked);
    unsafe { DwmSetWindowAttribute(hwnd, DWMWA_CLOAK, &value as *const BOOL as *const _, size_of::<BOOL>() as u32) }
        .expect("set window cloak");
}

pub fn is_visible(hwnd: HWND) -> bool {
    unsafe { IsWindowVisible(hwnd) }.as_bool()
}

pub fn is_maximized(hwnd: HWND) -> bool {
    unsafe { IsZoomed(hwnd) }.as_bool()
}

/// 把逻辑像素换算成这个窗口所在显示器的物理像素.
pub fn to_physical(hwnd: HWND, logical: i32) -> i32 {
    logical * unsafe { GetDpiForWindow(hwnd) } as i32 / 96
}

/// 窗口内侧边缘可以拖动改大小的宽度 (逻辑像素).
const RESIZE_BORDER: i32 = 6;

/// 让窗口可见区域内侧边缘也能拖动改大小. 自绘标题栏的窗口只有可见区域外一圈几像素宽, 看不见的系统边框能改大小,
/// 很难找准. 这里截获命中测试, 在内侧边缘告诉系统 "这是边框", 光标和拖动都由系统处理, 不经过 GPUI.
/// GPUI 没提供这个: `start_window_resize` 在 Windows 上是空实现.
pub fn widen_resize_border(hwnd: HWND) {
    unsafe { SetWindowSubclass(hwnd, Some(resize_border_proc), 1, 0) }.ok().expect("subclass window");
}

unsafe extern "system" fn resize_border_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _: usize,
    _: usize,
) -> LRESULT {
    if msg == WM_NCHITTEST && !is_maximized(hwnd) {
        // lparam 是屏幕坐标, 高低 16 位各是带符号的 y, x (副屏在主屏左边或上边时为负)
        let (x, y) = (lparam.0 as i16 as i32, (lparam.0 >> 16) as i16 as i32);
        let rect = visible_rect(hwnd);
        let monitor = monitor_rect(&rect);
        let width = to_physical(hwnd, RESIZE_BORDER);
        if (rect.left..rect.right).contains(&x) && (rect.top..rect.bottom).contains(&y) {
            // 贴着屏幕边的那条边不给调整: 贴边时往屏幕外拉没有意义, 窗口离开那条边还会被当成拖走而解除吸附
            let left = x < rect.left + width && rect.left > monitor.left;
            let right = x >= rect.right - width && rect.right < monitor.right;
            let top = y < rect.top + width && rect.top > monitor.top;
            let bottom = y >= rect.bottom - width && rect.bottom < monitor.bottom;
            let hit = match (top, bottom, left, right) {
                (true, _, true, _) => HTTOPLEFT,
                (true, _, _, true) => HTTOPRIGHT,
                (_, true, true, _) => HTBOTTOMLEFT,
                (_, true, _, true) => HTBOTTOMRIGHT,
                (true, ..) => HTTOP,
                (_, true, ..) => HTBOTTOM,
                (_, _, true, _) => HTLEFT,
                (_, _, _, true) => HTRIGHT,
                _ => HTNOWHERE,
            };
            if hit != HTNOWHERE {
                return LRESULT(hit as isize);
            }
        }
    }
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

/// 窗口可见区域. GetWindowRect 含有看不见的缩放边框, 要以 DWM 的范围为准.
pub fn visible_rect(hwnd: HWND) -> RECT {
    let mut rect = RECT::default();
    unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut rect as *mut RECT as *mut _,
            size_of::<RECT>() as u32,
        )
    }
    .expect("window visible bounds");
    rect
}

/// 窗口整体范围, 含可见区域外一圈看不见的缩放边框 (四个角的缩放区几乎都在这圈里).
pub fn outer_rect(hwnd: HWND) -> RECT {
    let mut rect = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut rect) }.expect("window rect");
    rect
}

/// 让窗口可见区域的左上角移到 (x, y).
pub fn move_visible(hwnd: HWND, x: i32, y: i32) {
    let visible = visible_rect(hwnd);
    let outer = outer_rect(hwnd);
    let (dx, dy) = (visible.left - outer.left, visible.top - outer.top);
    unsafe { SetWindowPos(hwnd, None, x - dx, y - dy, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE) }
        .expect("move window");
}

pub fn set_topmost(hwnd: HWND, topmost: bool) {
    let after = if topmost { HWND_TOPMOST } else { HWND_NOTOPMOST };
    unsafe { SetWindowPos(hwnd, Some(after), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE) }
        .expect("set topmost");
}

/// 和这个矩形重叠最多的显示器的范围.
pub fn monitor_rect(rect: &RECT) -> RECT {
    let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
    let _ = unsafe { GetMonitorInfoW(MonitorFromRect(rect, MONITOR_DEFAULTTONEAREST), &mut info) };
    info.rcMonitor
}
