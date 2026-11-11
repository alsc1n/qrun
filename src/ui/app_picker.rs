//! 添加应用对话框: 列出系统"所有应用" (含应用商店应用), 搜索后点一下加到分组.

use std::collections::HashMap;
use std::sync::Arc;

use gpui_kit::component::list::{List, ListDelegate, ListItem, ListState};
use gpui_kit::component::{IndexPath, WindowExt, h_flex};
use gpui_kit::*;

use crate::model::library::{Id, Library};
use crate::platform::shell::{self, InstalledApp};
use crate::ui::truncated::truncated_text;

/// 列表行里的图标尺寸.
const ICON_SIZE: f32 = 20.;

/// by_name 是设置"按名称去重", 加到分组时用.
pub fn open(library: &Entity<Library>, group: Id, by_name: bool, window: &mut Window, cx: &mut App) {
    let delegate = AppList {
        apps: shell::installed_apps(),
        matched: Vec::new(),
        selected: None,
        icons: HashMap::new(),
        library: library.clone(),
        group,
        by_name,
    };
    let list = cx.new(|cx| {
        let mut list = ListState::new(delegate, window, cx).searchable(true);
        list.delegate_mut().matched = (0..list.delegate().apps.len()).collect();
        list
    });
    list.update(cx, |list, cx| list.focus(window, cx));

    window.open_dialog(cx, move |dialog, _, _| {
        dialog.title("添加应用").w(px(420.)).child(List::new(&list).search_placeholder("搜索应用").h(px(320.)))
    });
}

struct AppList {
    apps: Vec<InstalledApp>,
    /// 匹配搜索的应用在 apps 里的下标.
    matched: Vec<usize>,
    selected: Option<IndexPath>,
    /// 按 apps 下标缓存的图标; 列表只渲染看得到的行, 滚到了才取.
    icons: HashMap<usize, Option<Arc<RenderImage>>>,
    library: Entity<Library>,
    group: Id,
    by_name: bool,
}

impl ListDelegate for AppList {
    type Item = ListItem;

    fn perform_search(&mut self, query: &str, _: &mut Window, _: &mut Context<ListState<Self>>) -> Task<()> {
        let query = query.trim().to_lowercase();
        self.matched = (0..self.apps.len()).filter(|&ix| self.apps[ix].name.to_lowercase().contains(&query)).collect();
        Task::ready(())
    }

    fn items_count(&self, _: usize, _: &App) -> usize {
        self.matched.len()
    }

    fn render_item(&mut self, ix: IndexPath, _: &mut Window, _: &mut Context<ListState<Self>>) -> Option<ListItem> {
        let app_ix = self.matched[ix.row];
        let app = &self.apps[app_ix];
        // 按 2 倍尺寸提取, 高分屏下也清晰
        let icon = self.icons.entry(app_ix).or_insert_with(|| shell::icon(&app.target, (ICON_SIZE * 2.) as i32));
        Some(
            ListItem::new(ix.row).child(
                h_flex()
                    .gap_2()
                    .text_sm()
                    .child(div().size(px(ICON_SIZE)).children(icon.clone().map(|image| img(image).size_full())))
                    .child(div().flex_1().min_w_0().truncate().child(truncated_text("name", app.name.clone()))),
            ),
        )
    }

    fn set_selected_index(&mut self, ix: Option<IndexPath>, _: &mut Window, _: &mut Context<ListState<Self>>) {
        self.selected = ix;
    }

    fn confirm(&mut self, _: bool, window: &mut Window, cx: &mut Context<ListState<Self>>) {
        let Some(ix) = self.selected else {
            return;
        };
        let app = &self.apps[self.matched[ix.row]];
        let (name, target, group, by_name) = (app.name.clone(), app.target.clone(), self.group, self.by_name);
        self.library.update(cx, |lib, cx| lib.add_app(group, name, target, by_name, cx));
        window.close_dialog(cx);
    }
}
