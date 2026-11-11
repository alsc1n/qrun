//! 图标网格: 显示项目, 单击或双击启动, 右键菜单 (空白处添加应用), 拖入添加 (快捷方式可选解析为目标程序), 拖动排序.

use std::path::Path;

use gpui_kit::component::ActiveTheme;
use gpui_kit::component::menu::{ContextMenuExt, PopupMenu, PopupMenuItem};
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use super::{DraggedItem, LinkDrop, Panel};
use crate::model::library::{GroupSort, Id, Item, Library, Via};
use crate::model::settings::LaunchClick;
use crate::platform::shell;
use crate::ui::truncated::truncated_text;
use crate::ui::{app_picker, item_editor};

/// 项目格子之间的间距 (行列相同).
pub(super) const GAP: Pixels = px(4.);
/// 项目宽度 = 图标尺寸 + 这个余量 (给名称和内边距).
pub(super) const ITEM_PADDING: Pixels = px(44.);

/// 这么宽的网格一行放几个项目, 至少一个.
pub(super) fn columns(width: Pixels, item_width: Pixels) -> u16 {
    ((width + GAP) / (item_width + GAP)).floor().max(1.) as u16
}

impl Panel {
    /// 记下网格宽度; 只有列数变了才重画, 拖窗口改大小时不多画一遍.
    pub(super) fn set_grid_width(&mut self, width: Pixels, item_width: Pixels, cx: &mut Context<Self>) {
        let changed = columns(width, item_width) != columns(self.grid_width, item_width);
        self.grid_width = width;
        if changed {
            cx.notify();
        }
    }

    pub(super) fn render_grid(&mut self, searching: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let (items, extras) = self.visible(cx);
        // 按点击次数排序的分组顺序不由拖拽决定, 不显示插入线, 项目只能拖到标签上换分组
        let sortable =
            !searching && self.library.read(cx).group(self.active_group).is_some_and(|g| g.sort == GroupSort::Manual);
        // 列数按上一帧量到的网格宽度算; 放不下下一个项目前列数不变, 多出的宽度平分给各列, 项目在格子里居中
        let item_width = px(self.settings.read(cx).appearance.icon_size) + ITEM_PADDING;
        let columns = columns(self.grid_width, item_width);
        let panel = cx.entity().downgrade();
        let theme = cx.theme();
        let (muted_fg, accent) = (theme.muted_foreground, theme.accent);

        let body = if items.is_empty() && extras.is_empty() {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(muted_fg)
                .child(if searching {
                    "没有匹配的项目"
                } else {
                    "把文件、文件夹或快捷方式拖到这里, 也可以右键添加应用"
                })
                .into_any_element()
        } else {
            div()
                .relative()
                .grid()
                .grid_cols(columns)
                .gap(GAP)
                .children(items.iter().enumerate().map(|(ix, item)| {
                    let prev = ix.checked_sub(1).map(|ix| items[ix].id);
                    let next = items.get(ix + 1).map(|item| item.id);
                    self.render_item(item, prev, next, searching, sortable, cx)
                }))
                // Jev 按搜索词认为很可能的其他候选, 只读, 点开即启动
                .children(extras.into_iter().enumerate().map(|(ix, ranked)| {
                    let tile = self.tile(("extra", ix), &ranked.name, &ranked.target, cx).on_click(cx.listener(
                        move |this, event: &ClickEvent, _, cx| {
                            if this.launches_on(event, cx) {
                                this.launch_ranked(&ranked, Via::Search, cx);
                            }
                        },
                    ));
                    div().flex().justify_center().child(tile)
                }))
                // 量网格宽度, 变了就重排
                .child(
                    canvas(
                        move |bounds, _, cx| {
                            panel.update(cx, |this, cx| this.set_grid_width(bounds.size.width, item_width, cx)).ok();
                        },
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .inset_0(),
                )
                .into_any_element()
        };

        let grid = div()
            .id("grid")
            .flex_1()
            .overflow_y_scroll()
            .px_4()
            .pb_4()
            .rounded_lg()
            .drag_over::<ExternalPaths>(move |s, _, _, _| s.bg(accent.opacity(0.25)))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                if this.own_drop() {
                    return;
                }
                this.add_paths(this.active_group, paths.paths(), cx);
            }))
            // 光标在网格空白处时插到末尾 (最后一个项目后侧亮线); 在项目上时由项目改成精确位置
            .when(sortable, |el| el.on_drag_move(Self::track_drop_tail::<DraggedItem>(cx)))
            // 松手按插入线的位置放; 没有插入线 (位置不变) 时不动
            .on_drop(cx.listener(|this, drag: &DraggedItem, _, cx| {
                let Some(hint) = this.drop_hint.take() else {
                    return;
                };
                let group = this.active_group;
                let ids: Vec<Id> = this.visible_items(cx).iter().map(|item| item.id).collect();
                let before = hint.before(&ids);
                this.library.update(cx, |lib, cx| lib.move_item(drag.id, group, before, cx));
            }))
            .child(body)
            // 项目自己的右键菜单先处理并拦住, 这里只在空白处弹出
            .context_menu({
                let (library, panel) = (self.library.clone(), cx.entity().downgrade());
                move |menu, _, _| {
                    let (library, panel) = (library.clone(), panel.clone());
                    menu.item(PopupMenuItem::new("添加应用…").on_click(move |_, window, cx| {
                        if let Ok((group, by_name)) =
                            panel.read_with(cx, |this, cx| (this.active_group, this.dedupe_names(cx)))
                        {
                            app_picker::open(&library, group, by_name, window, cx);
                        }
                    }))
                }
            });

        // 拖入快捷方式时只盖住网格; 分区放在不滚动的外层, 不随网格内容滚走
        v_flex().relative().flex_1().h_full().min_w_0().min_h_0().child(grid).children(self.render_link_zones(cx))
    }

    /// 外部拖入的是单个 .lnk 时记下它并解析一次, 换了文件才重新解析; 其他情况不分区.
    pub(super) fn track_link_drop(
        &mut self,
        event: &DragMoveEvent<ExternalPaths>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let link = match event.drag(cx).paths() {
            [path] if shell::is_link(path) && !self.in_system_drag() => Some(path.clone()),
            _ => None,
        };
        if self.link_drop.as_ref().map(|drop| &drop.link) != link.as_ref() {
            self.link_drop = link.map(|link| LinkDrop { shortcut: shell::read_shortcut(&link), link });
            cx.notify();
        }
    }

    /// 快捷方式分区: 左边浅蓝保留快捷方式, 右边浅绿解析为目标程序, 各占一半; 解析不出目标时只有左边一块占满.
    /// 只盖在网格上面并挡住网格的拖放, 松手只由分区处理; 标签区不受影响.
    pub(super) fn render_link_zones(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let drop = self.link_drop.as_ref().filter(|_| cx.has_active_drag())?;
        let theme = cx.theme();
        let (blue, green, muted_fg) = (theme.blue, theme.green, theme.muted_foreground);
        // GPUI 没有元素级背景模糊; 用 95% 不透明的 muted 色 (比背景深一档) 压住网格, 近似磨砂遮罩
        let mask = theme.muted.opacity(0.95);

        let keep = link_zone("keep-link", "保留快捷方式", file_name(&drop.link), blue, muted_fg).on_drop(cx.listener(
            |this, paths: &ExternalPaths, _, cx| {
                this.link_drop = None;
                this.add_paths(this.active_group, paths.paths(), cx);
            },
        ));
        let resolve = drop.shortcut.as_ref().map(|shortcut| {
            let (link, target, args) = (drop.link.clone(), shortcut.target.clone(), shortcut.args.clone());
            link_zone("resolve-link", "解析为目标程序", file_name(&target), green, muted_fg).on_drop(cx.listener(
                move |this, _: &ExternalPaths, _, cx| {
                    this.link_drop = None;
                    let (group, by_name) = (this.active_group, this.dedupe_names(cx));
                    let (target, args) = (target.clone(), args.clone());
                    this.library.update(cx, |lib, cx| lib.add_link_target(group, &link, target, args, by_name, cx));
                },
            ))
        });

        Some(
            h_flex()
                .id("link-zones")
                .absolute()
                .inset_0()
                .p_3()
                .gap_3()
                .rounded_lg()
                .bg(mask)
                .occlude()
                .child(keep)
                .children(resolve),
        )
    }

    /// prev / next 是前后相邻的项目. 搜索结果混着多个分组, 排序没有意义, 搜索时不能拖动.
    /// sortable 为 false 时能拖但不显示插入线, 只能拖到标签上换分组.
    fn render_item(
        &mut self,
        item: &Item,
        prev: Option<Id>,
        next: Option<Id>,
        searching: bool,
        sortable: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let id = item.id;
        let drag = DraggedItem { id, name: item.name.clone().into(), target: item.target.clone() };
        // 拖动判定挂在格子上: 格子间距固定为 GAP, 插入线落在两个格子正中, 也就是两个项目正中
        let track = Self::track_drop::<DraggedItem>(id, prev, next, false, GAP, cx);
        let launch_item = item.clone();
        let menu_item = item.clone();
        let library = self.library.clone();
        let panel = cx.entity().downgrade();
        let tile = self
            .tile(("item", id), &item.name, &item.target, cx)
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                if this.launches_on(event, cx) {
                    let via = this.grid_via(cx);
                    this.launch(&launch_item, false, via, cx);
                }
            }))
            .when(!searching, |el| el.on_drag(drag, |drag, _, _, cx| cx.new(|_| drag.clone())))
            .context_menu(move |menu, _, _| item_menu(menu, &menu_item, &library, &panel));

        // 格子占满一列, 项目在里面居中
        div().flex().justify_center().when(sortable, |el| el.on_drag_move(track)).child(tile)
    }

    /// 一个项目格: 图标加名称, 悬停和按下的样式; 点击, 拖动, 右键由调用方加. 网格和推荐页共用.
    pub(super) fn tile(
        &mut self,
        id: impl Into<ElementId>,
        name: &str,
        target: &Path,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let icon_size = px(self.settings.read(cx).appearance.icon_size);
        let icon = self.icon(target, cx);
        let theme = cx.theme();

        let icon = match icon {
            Some(image) => img(image).size(icon_size).into_any_element(),
            // 目标已不存在时用首字代替
            None => div()
                .size(icon_size)
                .rounded_lg()
                .bg(theme.muted)
                .flex()
                .items_center()
                .justify_center()
                .text_color(theme.muted_foreground)
                .child(name.chars().next().unwrap_or(' ').to_string())
                .into_any_element(),
        };

        v_flex()
            .id(id)
            .w(icon_size + ITEM_PADDING)
            .py_2()
            .px_1()
            .gap_1p5()
            .items_center()
            .rounded_lg()
            .cursor_pointer()
            .hover(|s| s.bg(theme.list_hover))
            .active(|s| s.opacity(0.7))
            .child(icon)
            // 名称先换行, 最多两行, 还放不下才省略, 省略了鼠标停留提示全称
            .child(
                div()
                    .w_full()
                    .text_xs()
                    .text_center()
                    .line_clamp(2)
                    .text_ellipsis()
                    .child(truncated_text("name", name.to_string())),
            )
    }

    /// 按设置里的单击或双击启动, 这次点击算不算启动.
    pub(super) fn launches_on(&self, event: &ClickEvent, cx: &App) -> bool {
        let clicks = match self.settings.read(cx).general.launch_click {
            LaunchClick::Single => 1,
            LaunchClick::Double => 2,
        };
        event.click_count() == clicks
    }
}

/// 一块虚线分区: 标题用分区颜色, 下面一行是松手后添加的文件名; 拖到上面时底色加深.
fn link_zone(id: &'static str, title: &'static str, detail: String, color: Hsla, muted_fg: Hsla) -> Stateful<Div> {
    v_flex()
        .id(id)
        .flex_1()
        .h_full()
        .min_w_0()
        .items_center()
        .justify_center()
        .gap_1()
        .rounded_lg()
        // GPUI 虚线的线段和间隔固定为边框宽的 2 倍和 1 倍, 只能靠加粗边框让虚线变疏; 1px 时太密
        .border_2()
        .border_dashed()
        .border_color(color)
        .bg(color.opacity(0.22))
        .shadow_sm()
        .drag_over::<ExternalPaths>(move |s, _, _, _| s.bg(color.opacity(0.36)))
        .child(div().text_sm().text_color(color).child(title))
        .child(
            div().max_w_full().px_4().text_xs().text_color(muted_fg).truncate().child(truncated_text("detail", detail)),
        )
}

fn file_name(path: &Path) -> String {
    path.file_name().unwrap_or(path.as_os_str()).to_string_lossy().into_owned()
}

/// 项目右键菜单. 编辑和删除按项目 ID 操作, 菜单打开期间别处增删项目也不会指错.
fn item_menu(menu: PopupMenu, item: &Item, library: &Entity<Library>, panel: &WeakEntity<Panel>) -> PopupMenu {
    let id = item.id;
    let (admin_item, with_target, reveal_target, copy_target, file_target) =
        (item.clone(), item.target.clone(), item.target.clone(), item.target.clone(), item.target.clone());
    let (edit_library, remove_library, panel, copy_panel) =
        (library.clone(), library.clone(), panel.clone(), panel.clone());
    menu.item(PopupMenuItem::new("以管理员身份运行").on_click(move |_, _, cx| {
        panel
            .update(cx, |this, cx| {
                let via = this.grid_via(cx);
                this.launch(&admin_item, true, via, cx)
            })
            .ok();
    }))
    .item(PopupMenuItem::new("打开方式").on_click(move |_, _, _| shell::open_with(&with_target)))
    .item(PopupMenuItem::new("打开所在位置").on_click(move |_, _, _| shell::reveal(&reveal_target)))
    .item(
        PopupMenuItem::new("复制路径").on_click(move |_, _, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(copy_target.display().to_string()))
        }),
    )
    .item(PopupMenuItem::new("复制文件").on_click(move |_, _, cx| {
        copy_panel
            .update(cx, |this, cx| {
                let via = this.grid_via(cx);
                this.copy_file(Some(id), &file_target, via, cx)
            })
            .ok();
    }))
    .separator()
    .item(PopupMenuItem::new("编辑").on_click(move |_, window, cx| item_editor::open(&edit_library, id, window, cx)))
    .item(PopupMenuItem::new("删除").on_click(move |_, _, cx| {
        remove_library.update(cx, |lib, cx| lib.remove_item(id, cx));
    }))
}
