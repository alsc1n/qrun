//! 主面板: 标题栏 (header), 分组标签 (tabs), 图标网格 (grid).

mod drag_out;
mod grid;
mod header;
mod jev;
mod smart;
mod tabs;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::{ActiveTheme, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use windows::Win32::Foundation::HWND;

use crate::model::library::{Id, Item, Library, NEW_FILE, Observed, Ranked, Via};
use crate::model::settings::{Settings, TabLayout};
use crate::params::PARAMS;
use crate::platform::recent::{self, Recent};
use crate::platform::{shell, window as native};

pub struct Panel {
    pub hwnd: HWND,
    library: Entity<Library>,
    settings: Entity<Settings>,
    /// 当前是推荐页 (固定在分组标签最前面); 为 false 时显示 active_group.
    smart_page: bool,
    active_group: Id,
    /// 上次读到的系统最近记录, 推荐页显示用; 还没读完时为 None.
    recent: Option<Recent>,
    /// 后台读系统最近记录; 换成新任务时旧的随之取消.
    read_recent: Task<()>,
    search: Entity<InputState>,
    /// 搜索结果: 网格里的项目, 以及 Jev 认为很可能但名称不匹配的候选. 排序要给全部候选打分, 不在每次重画时算,
    /// 只在搜索词, 库, 推荐开关或 Jev 结果变了时重算 (`refresh_search`); 没在搜索时为空.
    searched: (Vec<Item>, Vec<Ranked>),
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
    backdrop: native::Backdrop,
    /// 从外部拖入的单个快捷方式, 有值时内容区盖上左右分区; 拖动面板内元素时清掉.
    link_drop: Option<LinkDrop>,
    /// 正在拖的项目或推荐页格子, 拖出窗口时交给系统 (见 `drag_out`); 不在拖时由 `tick` 清掉.
    drag_out: Option<drag_out::DragOut>,
    /// 按目标路径缓存的图标; None 表示取不到 (目标已不存在).
    icons: HashMap<PathBuf, Option<Arc<RenderImage>>>,
    /// 等停手后保存窗口大小; 换成新任务时旧的随之取消.
    save_size: Task<()>,
    /// 等图标尺寸停止变化后按新尺寸重取图标; 换成新任务时旧的随之取消.
    refresh_icons: Task<()>,
    /// 悬停切换时等着切过去的标签 (None 是推荐页); 移开时丢掉任务即取消.
    hover_switch: Option<(Option<Id>, Task<()>)>,
    jev: jev::JevState,
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
            // 点到别的程序时窗口失活, 输入框不会收到失焦, 在这里提交; 回到面板算打开一次
            cx.observe_window_activation(window, |this, window, cx| {
                if !window.is_window_active() {
                    this.commit_rename(cx);
                } else {
                    this.opened(cx);
                }
            }),
            cx.observe_window_bounds(window, Self::remember_size),
            cx.observe(&library, |this, _, cx| {
                this.refresh_search(cx);
                cx.notify();
            }),
            // 贴边位置, 窗口大小等其他设置也会触发, 只有图标尺寸变了才重取图标.
            // 拖滑块时尺寸连续在变, 取图标慢, 每格都重取会卡: 先把已有图标缩放显示, 停手后再按新尺寸重取, 换回清晰的
            cx.observe(&settings, {
                let mut icon_size = settings.read(cx).appearance.icon_size;
                let mut enabled = settings.read(cx).smart.enabled;
                move |this, settings, cx| {
                    if settings.read(cx).smart.enabled != enabled {
                        enabled = !enabled;
                        this.set_recommend(enabled, cx);
                        this.refresh_search(cx);
                    }
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
        let recommend = settings.read(cx).smart.enabled;
        let mut panel = Self {
            hwnd: native::hwnd(window),
            library,
            settings,
            smart_page: recommend,
            active_group,
            recent: None,
            read_recent: Task::ready(()),
            search,
            searched: (Vec::new(), Vec::new()),
            renaming: None,
            rename,
            focus: cx.focus_handle(),
            drop_hint: None,
            drop_tail: None,
            drop_origin: Point::default(),
            grid_width: Pixels::ZERO,
            backdrop: None,
            link_drop: None,
            drag_out: None,
            icons: HashMap::new(),
            save_size: Task::ready(()),
            refresh_icons: Task::ready(()),
            hover_switch: None,
            jev: jev::JevState::new(),
            _subscriptions,
        };
        if recommend {
            panel.refresh_recent(cx);
        }
        panel
    }

    /// 设置里开关推荐. 关掉: 回到分组, 丢掉这次打开, 清掉系统最近记录;
    /// 打开: 像刚启动一样重新读系统最近记录, 读完记一次打开.
    fn set_recommend(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if enabled {
            self.refresh_recent(cx);
        } else {
            self.smart_page = false;
            self.recent = None;
            self.read_recent = Task::ready(());
            self.library.update(cx, |lib, cx| lib.end_open(cx));
        }
    }

    /// 当前标签: None 是推荐页, 否则是分组.
    fn current_tab(&self) -> Option<Id> {
        (!self.smart_page).then_some(self.active_group)
    }

    fn select_tab(&mut self, tab: Option<Id>, cx: &mut Context<Self>) {
        match tab {
            None => {
                self.smart_page = true;
                self.refresh_recent(cx);
            }
            Some(id) => {
                self.smart_page = false;
                self.active_group = id;
            }
        }
        cx.notify();
    }

    /// 后台读系统最近记录, 读完显示到推荐页, 并交给 library 当候选, 新的打开记进启动记录供预测学习.
    /// 第一次读完时补记启动时那次打开.
    fn refresh_recent(&mut self, cx: &mut Context<Self>) {
        let collect = self.settings.read(cx).smart.collect;
        let new_since = collect.new_files.then(new_file_since);
        self.read_recent = cx.spawn(async move |this, cx| {
            let recent =
                cx.background_executor().spawn(async move { recent::read(collect.system_recent, new_since) }).await;
            this.update(cx, |this, cx| {
                let lists = [
                    (&recent.files, "file"),
                    (&recent.folders, "folder"),
                    (&recent.programs, "program"),
                    (&recent.new_files, NEW_FILE),
                ];
                let observed: Vec<Observed> = lists
                    .into_iter()
                    .flat_map(|(entries, kind)| {
                        entries.iter().map(move |e| Observed {
                            name: e.name.clone(),
                            target: e.target.clone(),
                            at: e.at,
                            kind,
                        })
                    })
                    .collect();
                this.library.update(cx, |lib, cx| lib.sync_system(observed, cx));
                let first = this.recent.is_none();
                this.recent = Some(recent);
                if first {
                    this.opened(cx);
                }
                cx.notify();
            })
            .ok();
        });
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

    /// 搜索时跨所有分组匹配, 否则按分组的排序方式显示当前分组.
    fn visible_items(&self, cx: &App) -> Vec<Item> {
        self.visible(cx).0
    }

    /// 网格里的项目, 以及搜索时 Jev 认为很可能但名称不匹配的候选 (排在项目后面).
    fn visible(&self, cx: &App) -> (Vec<Item>, Vec<Ranked>) {
        if self.query(cx).is_empty() {
            (self.library.read(cx).sorted_items(self.active_group), Vec::new())
        } else {
            self.searched.clone()
        }
    }

    /// 按当前搜索词重算搜索结果.
    fn refresh_search(&mut self, cx: &App) {
        let library = self.library.read(cx);
        let query = self.query(cx);
        self.searched = if query.is_empty() {
            (Vec::new(), Vec::new())
        } else if self.settings.read(cx).smart.enabled {
            library.search(&query, self.search_probabilities(&query))
        } else {
            // 关了推荐只按名称匹配, 按分组和组内顺序排
            let query = query.to_lowercase();
            let items = library.groups().iter().flat_map(|g| &g.items);
            (items.filter(|item| item.name.to_lowercase().contains(&query)).cloned().collect(), Vec::new())
        };
    }

    /// 从网格启动的来源: 搜索结果或分组.
    fn grid_via(&self, cx: &App) -> Via {
        if self.query(cx).is_empty() { Via::Group } else { Via::Search }
    }

    /// 设置"按名称去重": 添加项目时分组里已有同名的就不加.
    fn dedupe_names(&self, cx: &App) -> bool {
        self.settings.read(cx).general.dedupe_names
    }

    /// 把文件或文件夹加到分组末尾, 按设置去重.
    fn add_paths(&self, group: Id, paths: &[PathBuf], cx: &mut Context<Self>) {
        let by_name = self.dedupe_names(cx);
        self.library.update(cx, |lib, cx| lib.add_paths(group, paths, by_name, cx));
    }

    fn icon(&mut self, target: &Path, cx: &App) -> Option<Arc<RenderImage>> {
        // 按 2 倍尺寸提取, 高分屏下也清晰
        let size = (self.settings.read(cx).appearance.icon_size * 2.0) as i32;
        self.icons.entry(target.to_path_buf()).or_insert_with(|| shell::icon(target, size)).clone()
    }

    /// 启动项目并记一次启动; 设置了"启动后收起"时收进托盘. 贴边状态下不收, 鼠标离开后它会自己缩回.
    fn launch(&self, item: &Item, as_admin: bool, via: Via, cx: &mut Context<Self>) {
        shell::run(&item.target, &item.args, as_admin || item.admin);
        self.library.update(cx, |lib, cx| lib.record_launch(Some(item.id), &item.target, via, cx));
        self.after_launch(cx);
    }

    /// 启动不在分组中的系统最近项或新文件.
    fn launch_target(&self, target: &Path, as_admin: bool, via: Via, cx: &mut Context<Self>) {
        shell::run(target, "", as_admin);
        self.library.update(cx, |lib, cx| lib.record_launch(None, target, via, cx));
        self.after_launch(cx);
    }

    /// 启动推荐页或搜索结果里的一个候选: 分组里有的按项目启动.
    fn launch_ranked(&self, ranked: &Ranked, via: Via, cx: &mut Context<Self>) {
        match &ranked.item {
            Some(item) => self.launch(item, false, via, cx),
            None => self.launch_target(&ranked.target, false, via, cx),
        }
    }

    fn after_launch(&self, cx: &App) {
        let settings = self.settings.read(cx);
        if settings.general.hide_after_launch && settings.window.docked.is_none() {
            native::hide(self.hwnd);
        }
    }

    /// 内容区: 搜索结果和分组用图标网格, 推荐页单独排版.
    fn render_content(&mut self, searching: bool, cx: &mut Context<Self>) -> AnyElement {
        if self.smart_page && !searching {
            self.render_smart(cx).into_any_element()
        } else {
            self.render_grid(searching, cx).into_any_element()
        }
    }

    fn on_search_event(&mut self, _: &Entity<InputState>, event: &InputEvent, _: &mut Window, cx: &mut Context<Self>) {
        match event {
            InputEvent::Change => {
                self.refresh_search(cx);
                self.search_changed(cx);
                cx.notify();
            }
            InputEvent::PressEnter { .. } => {
                if let Some(item) = self.visible_items(cx).first() {
                    self.launch(item, false, Via::Search, cx);
                }
            }
            _ => {}
        }
    }
}

/// 这个时间 (Unix 毫秒) 之后修改的下载和桌面文件算新文件.
fn new_file_since() -> i64 {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).expect("clock after 1970").as_millis() as i64;
    now - PARAMS.learn.new_file_days * 86_400_000
}

/// 拖动中的项目, 同时是跟随鼠标的预览. target 用于拖出面板.
#[derive(Clone)]
struct DraggedItem {
    id: Id,
    name: SharedString,
    target: PathBuf,
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
                .child(self.render_content(searching, cx))
        } else {
            v_flex()
                .flex_1()
                .min_h_0()
                .when(!searching, |el| el.child(self.render_tabs(false, cx)))
                .child(self.render_content(searching, cx))
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
            .on_drag_move(Self::track_drag_out_item(cx))
            .on_drag_move(Self::track_drag_out_tile(cx))
            .child(self.render_header(cx))
            .child(body)
            .children(self.render_insert_line(cx))
    }
}
