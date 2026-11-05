//! 主面板: 标题栏 (header), 分组标签 (tabs), 图标网格 (grid).

mod grid;
mod header;
mod tabs;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::{ActiveTheme, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use windows::Win32::Foundation::HWND;

use crate::model::library::{Id, Item, Library};
use crate::model::settings::{Settings, TabLayout};
use crate::platform::{shell, window as native};

pub struct Panel {
    pub hwnd: HWND,
    library: Entity<Library>,
    settings: Entity<Settings>,
    active_group: Id,
    search: Entity<InputState>,
    /// 正在重命名的分组和它的输入框.
    renaming: Option<Id>,
    rename: Entity<InputState>,
    /// 重命名期间挂在面板根上, 点击面板任意处把焦点收回来, 输入框失焦即提交.
    focus: FocusHandle,
    /// 拖动排序时当前的插入位置, 插入线和松手后的位置都以它为准; None 时松手不动.
    drop_hint: Option<DropHint>,
    /// 插到末尾的位置, 由最后一个元素在拖动中更新.
    drop_tail: Option<DropHint>,
    /// 面板根在窗口里的原点; 插入线的位置是窗口坐标, 画的时候换算.
    drop_origin: Point<Pixels>,
    /// 上一帧量到的网格宽度, 用来算一行放几列.
    grid_width: Pixels,
    /// 系统给窗口画的底色, 跟着界面背景色; 见 `window::sync_backdrop`.
    backdrop: Option<Hsla>,
    /// 从外部拖入的单个快捷方式, 有值时内容区盖上左右分区; 拖动面板内元素时清掉.
    link_drop: Option<LinkDrop>,
    /// 按目标路径缓存的图标; None 表示取不到 (目标已不存在).
    icons: HashMap<PathBuf, Option<Arc<RenderImage>>>,
    /// 等停手后保存窗口大小; 换成新任务时旧的随之取消.
    save_size: Task<()>,
    /// 等图标尺寸停止变化后按新尺寸重取图标; 换成新任务时旧的随之取消.
    refresh_icons: Task<()>,
    /// 悬停切换分组时等着切过去的分组; 移开时丢掉任务即取消.
    hover_switch: Option<(Id, Task<()>)>,
    _subscriptions: Vec<Subscription>,
}

/// 拖边改大小停手多久后才保存, 拖动中不反复写文件.
const SAVE_SIZE_DELAY: Duration = Duration::from_millis(500);
/// 图标尺寸停止变化多久后才按新尺寸重取图标.
const ICON_REFRESH_DELAY: Duration = Duration::from_millis(300);

impl Panel {
    pub fn new(
        library: Entity<Library>,
        settings: Entity<Settings>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("搜索"));
        let rename = cx.new(|cx| InputState::new(window, cx));
        let _subscriptions = vec![
            cx.subscribe_in(&search, window, Self::on_search_event),
            cx.subscribe_in(&rename, window, Self::on_rename_event),
            // 点到别的程序时窗口失活, 输入框不会收到失焦, 在这里提交
            cx.observe_window_activation(window, |this, window, cx| {
                if !window.is_window_active() {
                    this.commit_rename(cx);
                }
            }),
            cx.observe_window_bounds(window, Self::remember_size),
            cx.observe(&library, |_, _, cx| cx.notify()),
            // 贴边位置, 窗口大小等其他设置也会触发, 只有图标尺寸变了才重取图标.
            // 拖滑块时尺寸连续在变, 取图标慢, 每格都重取会卡: 先把已有图标缩放显示, 停手后再按新尺寸重取, 换回清晰的
            cx.observe(&settings, {
                let mut icon_size = settings.read(cx).appearance.icon_size;
                move |this, settings, cx| {
                    let size = settings.read(cx).appearance.icon_size;
                    if size != icon_size {
                        icon_size = size;
                        this.refresh_icons = cx.spawn(async move |this, cx| {
                            cx.background_executor().timer(ICON_REFRESH_DELAY).await;
                            this.update(cx, |this, cx| {
                                this.icons.clear();
                                cx.notify();
                            })
                            .ok();
                        });
                    }
                    cx.notify();
                }
            }),
        ];
        let active_group = library.read(cx).groups()[0].id;
        Self {
            hwnd: native::hwnd(window),
            library,
            settings,
            active_group,
            search,
            renaming: None,
            rename,
            focus: cx.focus_handle(),
            drop_hint: None,
            drop_tail: None,
            drop_origin: Point::default(),
            grid_width: Pixels::ZERO,
            backdrop: None,
            link_drop: None,
            icons: HashMap::new(),
            save_size: Task::ready(()),
            refresh_icons: Task::ready(()),
            hover_switch: None,
            _subscriptions,
        }
    }

    /// 窗口移动和改大小都会触发, 大小真的变了才记; 最大化 (双击标题栏) 的大小不记, 免得下次启动铺满屏幕.
    fn remember_size(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let size = window.bounds().size;
        let (width, height) = (f32::from(size.width), f32::from(size.height));
        let saved = &self.settings.read(cx).window;
        if window.is_maximized() || (saved.width, saved.height) == (width, height) {
            return;
        }
        let settings = self.settings.clone();
        self.save_size = cx.spawn(async move |_, cx| {
            cx.background_executor().timer(SAVE_SIZE_DELAY).await;
            settings.update(cx, |s, cx| {
                s.change(cx, |s| {
                    s.window.width = width;
                    s.window.height = height;
                })
            });
        });
    }

    fn query(&self, cx: &App) -> String {
        self.search.read(cx).value().trim().to_string()
    }

    /// 搜索时跨所有分组匹配, 否则显示当前分组.
    fn visible_items(&self, cx: &App) -> Vec<Item> {
        let library = self.library.read(cx);
        let query = self.query(cx);
        if query.is_empty() {
            library.group(self.active_group).map_or_else(Vec::new, |g| g.items.clone())
        } else {
            library.search(&query).into_iter().cloned().collect()
        }
    }

    fn icon(&mut self, target: &Path, cx: &App) -> Option<Arc<RenderImage>> {
        // 按 2 倍尺寸提取, 高分屏下也清晰
        let size = (self.settings.read(cx).appearance.icon_size * 2.0) as i32;
        self.icons.entry(target.to_path_buf()).or_insert_with(|| shell::icon(target, size)).clone()
    }

    /// 启动项目; 设置了"启动后收起"时收进托盘. 贴边状态下不收, 鼠标离开后它会自己缩回.
    fn launch(&self, item: &Item, as_admin: bool, cx: &App) {
        shell::run(&item.target, &item.args, as_admin || item.admin);
        let settings = self.settings.read(cx);
        if settings.general.hide_after_launch && settings.window.docked.is_none() {
            native::hide(self.hwnd);
        }
    }

    fn on_search_event(&mut self, _: &Entity<InputState>, event: &InputEvent, _: &mut Window, cx: &mut Context<Self>) {
        match event {
            InputEvent::Change => cx.notify(),
            InputEvent::PressEnter { .. } => {
                if let Some(item) = self.visible_items(cx).first() {
                    self.launch(item, false, cx);
                }
            }
            _ => {}
        }
    }
}

/// 拖动中的项目, 同时是跟随鼠标的预览.
#[derive(Clone)]
struct DraggedItem {
    id: Id,
    name: SharedString,
}

/// 拖动中的分组, 同时是跟随鼠标的预览.
#[derive(Clone)]
struct DraggedGroup {
    id: Id,
    name: SharedString,
}

impl Render for DraggedItem {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        drag_preview(self.name.clone(), cx)
    }
}

impl Render for DraggedGroup {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        drag_preview(self.name.clone(), cx)
    }
}

/// 拖入的快捷方式和解析结果; 解析不出本地目标时 shortcut 为 None, 只给"保留快捷方式"一块.
struct LinkDrop {
    link: PathBuf,
    shortcut: Option<shell::Shortcut>,
}

/// 可拖动排序的东西.
trait Dragged: 'static {
    fn id(&self) -> Id;
}

impl Dragged for DraggedItem {
    fn id(&self) -> Id {
        self.id
    }
}

impl Dragged for DraggedGroup {
    fn id(&self) -> Id {
        self.id
    }
}

/// 拖动排序的插入位置: 插在 target 的前面或后面, line 是插入线在窗口里的位置.
/// 项目和分组的 ID 互不重复, 共用这一个结构.
#[derive(Clone, Copy, PartialEq)]
struct DropHint {
    target: Id,
    after: bool,
    line: Bounds<Pixels>,
}

/// 插入线粗细.
const LINE_WIDTH: Pixels = px(2.);

impl DropHint {
    /// 插入线放在元素前侧或后侧空隙的正中, 只按坐标算: 前一个的后侧和后一个的前侧算出同一位置, 光标在两者间移动时线不动.
    fn new(target: Id, after: bool, bounds: Bounds<Pixels>, vertical: bool, gap: Pixels) -> Self {
        let line = if vertical {
            let y = if after { bounds.bottom() + gap / 2. } else { bounds.top() - gap / 2. };
            let inset = bounds.size.width * 0.1;
            Bounds::new(
                point(bounds.left() + inset, y - LINE_WIDTH / 2.),
                size(bounds.size.width - inset * 2., LINE_WIDTH),
            )
        } else {
            let x = if after { bounds.right() + gap / 2. } else { bounds.left() - gap / 2. };
            let inset = bounds.size.height * 0.2;
            Bounds::new(
                point(x - LINE_WIDTH / 2., bounds.top() + inset),
                size(LINE_WIDTH, bounds.size.height - inset * 2.),
            )
        };
        Self { target, after, line }
    }

    /// 换成"插到哪个元素前面"; ids 是容器里元素的顺序, None 表示末尾.
    fn before(self, ids: &[Id]) -> Option<Id> {
        if !self.after {
            return Some(self.target);
        }
        let ix = ids.iter().position(|&id| id == self.target)?;
        ids.get(ix + 1).copied()
    }
}

impl Panel {
    /// 拖动中光标在元素范围内时, 以中线为界记下插在它前面还是后面; prev / next 是前后相邻元素.
    /// 范围向四周扩半个间距, 相邻元素正好接上, 光标划过空隙时位置不丢.
    /// 放下去位置不变 (自己身上, 前一个的后侧, 后一个的前侧) 时不给插入位置, 不显示线.
    /// 最后一个元素顺便记下"插到末尾"的位置, 供容器空白处使用.
    fn track_drop<T: Dragged>(
        id: Id,
        prev: Option<Id>,
        next: Option<Id>,
        vertical: bool,
        gap: Pixels,
        cx: &Context<Self>,
    ) -> impl Fn(&DragMoveEvent<T>, &mut Window, &mut App) + 'static {
        cx.listener(move |this, event: &DragMoveEvent<T>, _, cx| {
            if next.is_none() {
                this.drop_tail = Some(DropHint::new(id, true, event.bounds, vertical, gap));
            }
            let position = event.event.position;
            if !event.bounds.dilate(gap / 2.).contains(&position) {
                return;
            }
            let center = event.bounds.center();
            let after = if vertical { position.y > center.y } else { position.x > center.x };
            let dragged = Some(event.drag(cx).id());
            let unchanged = Some(id) == dragged || if after { next == dragged } else { prev == dragged };
            let hint = (!unchanged).then(|| DropHint::new(id, after, event.bounds, vertical, gap));
            this.set_drop_hint(hint, cx);
        })
    }

    /// 面板根: 先清掉插入位置, 并记下自己在窗口里的原点, 插入线按它换算成面板内坐标.
    /// 外部拖入的文件拖出窗口时没有事件, 上次留下的快捷方式分区在这里清掉.
    fn track_drop_root<T: Dragged>(cx: &Context<Self>) -> impl Fn(&DragMoveEvent<T>, &mut Window, &mut App) + 'static {
        cx.listener(|this, event: &DragMoveEvent<T>, _, cx| {
            this.drop_origin = event.bounds.origin;
            this.link_drop = None;
            this.set_drop_hint(None, cx);
        })
    }

    /// 容器空白处: 光标在容器内时插到末尾; 最后一个就是被拖的自己时位置不变, 不给插入位置.
    fn track_drop_tail<T: Dragged>(cx: &Context<Self>) -> impl Fn(&DragMoveEvent<T>, &mut Window, &mut App) + 'static {
        cx.listener(|this, event: &DragMoveEvent<T>, _, cx| {
            if !event.bounds.contains(&event.event.position) {
                return;
            }
            let dragged = event.drag(cx).id();
            let hint = this.drop_tail.filter(|tail| tail.target != dragged);
            this.set_drop_hint(hint, cx);
        })
    }

    fn set_drop_hint(&mut self, hint: Option<DropHint>, cx: &mut Context<Self>) {
        if self.drop_hint != hint {
            self.drop_hint = hint;
            cx.notify();
        }
    }

    /// 当前的插入线: 浅灰圆头细条, 画在面板最上层; 拖动结束后不显示.
    fn render_insert_line(&self, cx: &App) -> Option<Div> {
        let line = self.drop_hint.filter(|_| cx.has_active_drag())?.line;
        Some(
            div()
                .absolute()
                .left(line.left() - self.drop_origin.x)
                .top(line.top() - self.drop_origin.y)
                .w(line.size.width)
                .h(line.size.height)
                .rounded_full()
                .bg(cx.theme().muted_foreground.opacity(0.6)),
        )
    }
}

fn drag_preview(name: SharedString, cx: &App) -> Div {
    let theme = cx.theme();
    div()
        .px_3()
        .py_1()
        .rounded_md()
        .border_1()
        .border_color(theme.border)
        .bg(theme.background)
        .text_color(theme.foreground)
        .text_sm()
        .opacity(0.85)
        .child(name)
}

impl Render for Panel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 当前分组被删掉时回到第一个分组
        if self.library.read(cx).group(self.active_group).is_none() {
            self.active_group = self.library.read(cx).groups()[0].id;
        }
        let searching = !self.query(cx).is_empty();
        let appearance = &self.settings.read(cx).appearance;
        let vertical = appearance.tab_layout == TabLayout::Vertical;
        let theme = cx.theme();
        let (bg, fg) = (theme.background, theme.foreground);
        native::sync_backdrop(&mut self.backdrop, window, bg);

        // 横排时标签在图标上方, 竖排时在左侧; 搜索时不显示标签
        let body = if vertical {
            h_flex()
                .flex_1()
                .min_h_0()
                .items_start()
                .when(!searching, |el| el.child(self.render_tabs(true, cx)))
                .child(self.render_grid(searching, cx))
        } else {
            v_flex()
                .flex_1()
                .min_h_0()
                .when(!searching, |el| el.child(self.render_tabs(false, cx)))
                .child(self.render_grid(searching, cx))
        };

        v_flex()
            .size_full()
            .bg(bg)
            .text_color(fg)
            // 只在重命名时接管焦点, 平时点图标不会把焦点从搜索框抢走
            .when(self.renaming.is_some(), |el| el.track_focus(&self.focus))
            // 拖动时先清掉插入位置, 光标所在的容器和元素随后再设 (父元素先于子元素收到); 离开容器线就消失
            .on_drag_move(Self::track_drop_root::<DraggedItem>(cx))
            .on_drag_move(Self::track_drop_root::<DraggedGroup>(cx))
            .on_drag_move(cx.listener(Self::track_link_drop))
            .child(self.render_header(cx))
            .child(body)
            .children(self.render_insert_line(cx))
    }
}
