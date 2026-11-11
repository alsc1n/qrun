//! 推荐页: 猜你要打开, 最常用, 最近文件, 最近文件夹, 最近程序. 点开即启动; 可拖到别的程序, 右键可复制,
//! 不在分组里的可加进分组; 不能在面板里排序, 编辑或删除.

use std::collections::HashMap;
use std::path::PathBuf;

use gpui_kit::component::menu::{ContextMenuExt, PopupMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, v_flex};
use gpui_kit::*;

use super::Panel;
use super::drag_out::DraggedTile;
use super::grid::{ITEM_PADDING, columns};
use crate::model::library::{Id, Item, Library, Via};
use crate::model::predict;
use crate::platform::recent::RecentEntry;
use crate::platform::shell;

/// "猜你要打开"一块的 id; 从这块打开的按名次学对错.
const PREDICT: &str = "predict";
const DOCUMENTS_OFF: &str = "系统没有记录: 设置 > 个性化 > 开始里 \"显示最近打开的项目\" 已关闭.";
const PROGRAMS_OFF: &str = "系统没有记录: 设置 > 隐私和安全性 > 常规里 \"允许 Windows 跟踪应用启动\" 已关闭.";

/// 推荐页上的一格. item 是分组里同一目标的项目, 有它时按项目启动 (带参数, 管理员, 计点击次数).
#[derive(Clone)]
struct Tile {
    name: String,
    target: PathBuf,
    item: Option<Item>,
}

impl Tile {
    fn from_item(item: &Item) -> Self {
        Self { name: item.name.clone(), target: item.target.clone(), item: Some(item.clone()) }
    }
}

/// 一块: 标题, 内容, 内容为空时的说明.
struct Section {
    id: &'static str,
    title: &'static str,
    tiles: Vec<Tile>,
    empty: &'static str,
}

impl Panel {
    pub(super) fn render_smart(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let smart = &self.settings.read(cx).smart;
        let counts = [smart.predict, smart.frequent, smart.recent_files, smart.recent_folders, smart.recent_programs]
            .map(|n| n as usize);
        let items: HashMap<String, Item> = self
            .library
            .read(cx)
            .groups()
            .iter()
            .flat_map(|g| &g.items)
            .map(|i| (predict::key(&i.target), i.clone()))
            .collect();
        let to_tiles = |entries: &[RecentEntry], n: usize| -> Vec<Tile> {
            entries
                .iter()
                .take(n)
                .map(|e| match items.get(&predict::key(&e.target)) {
                    Some(item) => Tile::from_item(item),
                    None => Tile { name: e.name.clone(), target: e.target.clone(), item: None },
                })
                .collect()
        };
        let reading = "读取中...";
        let (files, folders, programs) = match &self.recent {
            None => (Vec::new(), Vec::new(), Vec::new()),
            Some(recent) => (
                to_tiles(&recent.files, counts[2]),
                to_tiles(&recent.folders, counts[3]),
                to_tiles(&recent.programs, counts[4]),
            ),
        };
        let documents_empty = match &self.recent {
            None => reading,
            Some(recent) if !recent.tracks_documents => DOCUMENTS_OFF,
            Some(_) => "暂无.",
        };
        let programs_empty = match &self.recent {
            None => reading,
            Some(recent) if !recent.tracks_programs => PROGRAMS_OFF,
            Some(_) => "暂无.",
        };
        let sections = [
            Section {
                id: PREDICT,
                title: "猜你要打开",
                tiles: self
                    .library
                    .read(cx)
                    .predicted()
                    .iter()
                    .take(counts[0])
                    .map(|r| Tile { name: r.name.clone(), target: r.target.clone(), item: r.item.clone() })
                    .collect(),
                empty: "分组里还没有项目, 系统也没有最近记录.",
            },
            Section {
                id: "frequent",
                title: "最常用",
                tiles: self.library.read(cx).most_clicked(counts[1]).iter().map(Tile::from_item).collect(),
                empty: "从 QRun 打开过的项目会按次数排在这里.",
            },
            Section { id: "files", title: "最近文件", tiles: files, empty: documents_empty },
            Section { id: "folders", title: "最近文件夹", tiles: folders, empty: documents_empty },
            Section { id: "programs", title: "最近程序", tiles: programs, empty: programs_empty },
        ];

        let item_width = px(self.settings.read(cx).appearance.icon_size) + ITEM_PADDING;
        let columns = columns(self.grid_width, item_width);
        let panel = cx.entity().downgrade();
        let content = v_flex()
            .relative()
            .gap_4()
            .children(
                sections
                    .into_iter()
                    .zip(counts)
                    .filter(|(_, count)| *count > 0)
                    .map(|(section, _)| self.render_section(section, columns, cx)),
            )
            // 和图标网格一样量宽度算列数; 推荐页是默认标签, 网格可能还没画过
            .child(
                canvas(
                    move |bounds, _, cx| {
                        panel.update(cx, |this, cx| this.set_grid_width(bounds.size.width, item_width, cx)).ok();
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            );
        div()
            .id("smart")
            .flex_1()
            .h_full()
            .min_w_0()
            .overflow_y_scroll()
            .px_4()
            .pb_4()
            // 鼠标进来后用户可能正要点, 之后到的 Jev 结果不再挪动顺序
            .on_hover(cx.listener(|this, hovered: &bool, _, _| {
                if *hovered {
                    this.jev.smart_entered = true;
                }
            }))
            .child(content)
    }

    fn render_section(&mut self, section: Section, columns: u16, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let muted_fg = cx.theme().muted_foreground;
        let body = if section.tiles.is_empty() {
            div().text_sm().text_color(muted_fg).child(section.empty).into_any_element()
        } else {
            div()
                .grid()
                .grid_cols(columns)
                .gap(super::grid::GAP)
                .children(section.tiles.into_iter().enumerate().map(|(ix, tile)| {
                    let via = if section.id == PREDICT { Via::Predicted(ix) } else { Via::SmartList };
                    let drag = DraggedTile {
                        name: tile.name.clone().into(),
                        target: tile.target.clone(),
                        item: tile.item.as_ref().map(|item| item.id),
                        via,
                    };
                    let (panel, library) = (cx.entity().downgrade(), self.library.clone());
                    let menu_tile = tile.clone();
                    let element = self
                        .tile((section.id, ix), &tile.name, &tile.target, cx)
                        .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                            if this.launches_on(event, cx) {
                                this.launch_tile(&tile, false, via, cx);
                            }
                        }))
                        .on_drag(drag, |drag, _, _, cx| cx.new(|_| drag.clone()))
                        .context_menu(move |menu, window, cx| {
                            tile_menu(menu, &menu_tile, via, &library, &panel, window, cx)
                        });
                    // 格子占满一列, 项目在里面居中
                    div().flex().justify_center().child(element)
                }))
                .into_any_element()
        };
        v_flex().gap_2().child(div().text_sm().text_color(muted_fg).child(section.title)).child(body)
    }

    /// 打开推荐页上的一格: 分组里有的按项目启动 (带参数, 计点击次数).
    fn launch_tile(&self, tile: &Tile, as_admin: bool, via: Via, cx: &mut Context<Self>) {
        match &tile.item {
            Some(item) => self.launch(item, as_admin, via, cx),
            None => self.launch_target(&tile.target, as_admin, via, cx),
        }
    }
}

/// 推荐页格子的右键菜单. 和分组项目的菜单一样有打开和复制; 不在分组里的可以加进某个分组, 没有编辑和删除.
fn tile_menu(
    menu: PopupMenu,
    tile: &Tile,
    via: Via,
    library: &Entity<Library>,
    panel: &WeakEntity<Panel>,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let (admin_tile, copy_tile, panel_admin, panel_copy) = (tile.clone(), tile.clone(), panel.clone(), panel.clone());
    let (with_target, reveal_target, path_target) = (tile.target.clone(), tile.target.clone(), tile.target.clone());
    let menu = menu
        .item(PopupMenuItem::new("以管理员身份运行").on_click(move |_, _, cx| {
            panel_admin.update(cx, |this, cx| this.launch_tile(&admin_tile, true, via, cx)).ok();
        }))
        .item(PopupMenuItem::new("打开方式").on_click(move |_, _, _| shell::open_with(&with_target)))
        .item(PopupMenuItem::new("打开所在位置").on_click(move |_, _, _| shell::reveal(&reveal_target)))
        .item(PopupMenuItem::new("复制路径").on_click(move |_, _, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(path_target.display().to_string()))
        }))
        .item(PopupMenuItem::new("复制文件").on_click(move |_, _, cx| {
            let item = copy_tile.item.as_ref().map(|item| item.id);
            panel_copy.update(cx, |this, cx| this.copy_file(item, &copy_tile.target, via, cx)).ok();
        }));
    if tile.item.is_some() {
        return menu;
    }
    let groups: Vec<(Id, String)> = library.read(cx).groups().iter().map(|g| (g.id, g.name.clone())).collect();
    let (panel, target) = (panel.clone(), tile.target.clone());
    menu.separator().submenu("添加到分组", window, cx, move |menu, _, _| {
        groups.iter().fold(menu, |menu, (id, name)| {
            let (panel, target, id) = (panel.clone(), target.clone(), *id);
            menu.item(PopupMenuItem::new(name.clone()).on_click(move |_, _, cx| {
                panel.update(cx, |this, cx| this.add_paths(id, std::slice::from_ref(&target), cx)).ok();
            }))
        })
    })
}
