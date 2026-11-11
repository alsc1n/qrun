//! 贴边悬停: 窗口拖到屏幕上/左/右边附近或拖出边外, 松开后吸附, 鼠标离开一段时间后完全滑出屏幕,
//! 鼠标碰到该边时滑回. 坐标都是物理像素. 置顶不在这里管, 由 app::poll 按吸附状态和"置于顶层"设置统一设.

use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Dwm::DwmFlush;
use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONULL, MonitorFromPoint};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

use crate::model::settings::{DockSettings, Docked, Edge};
use crate::platform::window;

/// 鼠标离开窗口 (含缩放边框) 外这么远 (逻辑像素) 才开始计时缩回.
const LEAVE_MARGIN: i32 = 16;

pub struct Dock {
    hwnd: HWND,
    settings: DockSettings,
    docked: Option<Docked>,
    hidden: bool,
    /// 鼠标离开已弹出窗口的时刻.
    left_at: Option<Instant>,
}

impl Dock {
    pub fn new(hwnd: HWND, settings: DockSettings, docked: Option<Docked>) -> Self {
        Self { hwnd, settings, docked, hidden: false, left_at: None }
    }

    /// 有保存的吸附状态时, 把窗口放回原来那条边并直接收起, 鼠标碰边再滑出. 要在窗口首次显示之后调用, 否则会被初始位置覆盖.
    pub fn restore(&mut self) {
        if let Some(docked) = self.docked {
            window::move_visible(self.hwnd, docked.x, docked.y);
            let rect = window::visible_rect(self.hwnd);
            let (x, y) = hidden_origin(docked.edge, &rect, &window::monitor_rect(&rect));
            window::move_visible(self.hwnd, x, y);
            self.hidden = true;
        }
    }

    /// 从托盘打开面板时, 缩在屏幕外的窗口要滑出来.
    pub fn reveal(&mut self) {
        if let (Some(docked), true) = (self.docked, self.hidden) {
            let rect = window::visible_rect(self.hwnd);
            self.slide((rect.left, rect.top), self.shown(docked.edge));
            self.hidden = false;
        }
    }

    /// 已在运行时用户又启动了一次: 吸附着的窗口 (藏着或弹出) 滑到离边刚好超出吸附距离的位置并解除吸附,
    /// 停在吸附距离内的话松手判断会马上又吸回去. 要贴边由用户自己再拖过去. 解除了吸附时返回 true, 由调用方保存.
    pub fn undock(&mut self) -> bool {
        let Some(docked) = self.docked else {
            return false;
        };
        let rect = window::visible_rect(self.hwnd);
        let monitor = window::monitor_rect(&rect);
        let (x, y) = shown_origin(docked.edge, &rect, &monitor);
        let gap = window::to_physical(self.hwnd, self.settings.snap_distance) + 1;
        let to = match docked.edge {
            Edge::Top => (x, y + gap),
            Edge::Left => (x + gap, y),
            Edge::Right => (x - gap, y),
        };
        self.slide((rect.left, rect.top), to);
        self.docked = None;
        self.hidden = false;
        self.left_at = None;
        true
    }

    /// 按窗口当前大小和所在屏幕算出贴紧边的位置; 保存的坐标可能因窗口尺寸或缩放变化而过时.
    fn shown(&self, edge: Edge) -> (i32, i32) {
        let rect = window::visible_rect(self.hwnd);
        shown_origin(edge, &rect, &window::monitor_rect(&rect))
    }

    /// 缩在屏幕边外.
    pub fn is_hidden(&self) -> bool {
        self.hidden
    }

    /// 设置页改了参数后立即生效.
    pub fn set_settings(&mut self, settings: DockSettings) {
        self.settings = settings;
    }

    /// 推进一次状态. 吸附状态变化时返回 Some(新状态), 由调用方保存.
    pub fn tick(&mut self) -> Option<Option<Docked>> {
        // 收在托盘里时不处理
        if !window::is_visible(self.hwnd) {
            return None;
        }
        // 关掉贴边时, 已吸附的窗口滑回来并解除吸附
        if !self.settings.enabled {
            self.docked?;
            self.reveal();
            self.docked = None;
            self.left_at = None;
            return Some(None);
        }
        // 锁屏时取不到光标位置, 这一轮跳过
        let cursor = cursor()?;
        let rect = window::visible_rect(self.hwnd);
        let monitor = window::monitor_rect(&rect);

        let Some(docked) = self.docked else {
            // 拖动结束才判断吸附; 最大化的窗口天然贴边, 不算吸附
            if mouse_down() || window::is_maximized(self.hwnd) {
                return None;
            }
            let edge = self.edge_near(&rect, &monitor)?;
            let (x, y) = shown_origin(edge, &rect, &monitor);
            self.slide((rect.left, rect.top), (x, y));
            let docked = Docked { edge, x, y };
            self.docked = Some(docked);
            return Some(Some(docked));
        };

        if self.hidden {
            if touches_edge(docked.edge, cursor, &rect, &monitor) {
                self.slide((rect.left, rect.top), shown_origin(docked.edge, &rect, &monitor));
                self.hidden = false;
            }
            return None;
        }

        // 弹出状态下离开了那条边, 说明用户把窗口拖走了, 解除吸附
        let shown = shown_origin(docked.edge, &rect, &monitor);
        if (rect.left, rect.top) != shown {
            self.docked = None;
            self.left_at = None;
            return Some(None);
        }
        // 沿边挪动或改了尺寸, 松手后更新保存的位置; 拖动中每轮都存会反复写文件, 拖着改大小很卡
        if shown != (docked.x, docked.y) && !mouse_down() {
            let docked = Docked { edge: docked.edge, x: shown.0, y: shown.1 };
            self.docked = Some(docked);
            return Some(Some(docked));
        }

        // 按含缩放边框的整体范围再外扩一圈算: 边框和四个角在可见区域外, 去抓它们或在窗口边上找位置时
        // 鼠标难免多挪出去一点, 范围卡太紧就被当成离开, 窗口立刻收走
        let margin = window::to_physical(self.hwnd, LEAVE_MARGIN);
        let mut area = window::outer_rect(self.hwnd);
        (area.left, area.top, area.right, area.bottom) =
            (area.left - margin, area.top - margin, area.right + margin, area.bottom + margin);
        if contains(&area, cursor) || mouse_down() {
            self.left_at = None;
            return None;
        }
        let left_at = *self.left_at.get_or_insert_with(Instant::now);
        if left_at.elapsed() >= Duration::from_millis(self.settings.hide_delay_ms) {
            let (x, y) = hidden_origin(docked.edge, &rect, &monitor);
            self.slide((rect.left, rect.top), (x, y));
            self.hidden = true;
            self.left_at = None;
        }
        None
    }

    /// 窗口靠近或越出了哪条边; gap 为负表示已经越出屏幕.
    /// 边外还有别的显示器时不吸附, 否则藏起来的窗口会出现在那块屏幕上.
    fn edge_near(&self, rect: &RECT, monitor: &RECT) -> Option<Edge> {
        let snap = window::to_physical(self.hwnd, self.settings.snap_distance);
        [
            (Edge::Top, rect.top - monitor.top),
            (Edge::Left, rect.left - monitor.left),
            (Edge::Right, monitor.right - rect.right),
        ]
        .into_iter()
        .find(|&(edge, gap)| gap <= snap && !screen_beyond(edge, rect, monitor))
        .map(|(edge, _)| edge)
    }

    /// 按实际经过的时间算位置, 每帧用 DwmFlush 等到下一次屏幕刷新, 帧率跟随显示器.
    /// 动画很短, 直接在主线程上跑完.
    fn slide(&self, from: (i32, i32), to: (i32, i32)) {
        let duration = Duration::from_millis(self.settings.animation_ms);
        let start = Instant::now();
        loop {
            let progress = (start.elapsed().as_secs_f32() / duration.as_secs_f32()).min(1.0);
            // ease-out cubic: 先快后慢
            let t = 1.0 - (1.0 - progress).powi(3);
            let lerp = |a: i32, b: i32| a + ((b - a) as f32 * t).round() as i32;
            window::move_visible(self.hwnd, lerp(from.0, to.0), lerp(from.1, to.1));
            if progress >= 1.0 {
                return;
            }
            unsafe { DwmFlush() }.expect("wait for composition");
        }
    }
}

fn shown_origin(edge: Edge, rect: &RECT, monitor: &RECT) -> (i32, i32) {
    match edge {
        Edge::Top => (rect.left, monitor.top),
        Edge::Left => (monitor.left, rect.top),
        Edge::Right => (monitor.right - (rect.right - rect.left), rect.top),
    }
}

fn hidden_origin(edge: Edge, rect: &RECT, monitor: &RECT) -> (i32, i32) {
    match edge {
        Edge::Top => (rect.left, monitor.top - (rect.bottom - rect.top)),
        Edge::Left => (monitor.left - (rect.right - rect.left), rect.top),
        Edge::Right => (monitor.right, rect.top),
    }
}

/// 鼠标是否顶到了窗口藏起来的那段屏幕边.
fn touches_edge(edge: Edge, cursor: POINT, rect: &RECT, monitor: &RECT) -> bool {
    match edge {
        Edge::Top => cursor.y <= monitor.top && (rect.left..rect.right).contains(&cursor.x),
        Edge::Left => cursor.x <= monitor.left && (rect.top..rect.bottom).contains(&cursor.y),
        Edge::Right => cursor.x >= monitor.right - 1 && (rect.top..rect.bottom).contains(&cursor.y),
    }
}

fn screen_beyond(edge: Edge, rect: &RECT, monitor: &RECT) -> bool {
    let (mid_x, mid_y) = ((rect.left + rect.right) / 2, (rect.top + rect.bottom) / 2);
    let outside = match edge {
        Edge::Top => POINT { x: mid_x, y: monitor.top - 1 },
        Edge::Left => POINT { x: monitor.left - 1, y: mid_y },
        Edge::Right => POINT { x: monitor.right, y: mid_y },
    };
    !unsafe { MonitorFromPoint(outside, MONITOR_DEFAULTTONULL) }.is_invalid()
}

/// 按着鼠标左键且光标已在窗口 (含缩放边框) 外: 拖着东西出了面板.
pub(super) fn dragged_outside(hwnd: HWND) -> bool {
    mouse_down() && cursor().is_some_and(|point| !contains(&window::outer_rect(hwnd), point))
}

fn contains(rect: &RECT, point: POINT) -> bool {
    (rect.left..rect.right).contains(&point.x) && (rect.top..rect.bottom).contains(&point.y)
}

fn cursor() -> Option<POINT> {
    let mut point = POINT::default();
    unsafe { GetCursorPos(&mut point) }.ok()?;
    Some(point)
}

fn mouse_down() -> bool {
    // 最高位为 1 表示按下
    let state = unsafe { GetAsyncKeyState(VK_LBUTTON.0 as i32) };
    state < 0
}
