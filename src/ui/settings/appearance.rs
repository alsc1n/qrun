//! 外观: 界面字体, 分组标签排列, 图标大小, 背景不透明度.

use gpui_kit::component::IconName;
use gpui_kit::component::IndexPath;
use gpui_kit::component::searchable_list::{SearchableListItem, SearchableVec};
use gpui_kit::component::select::{Select, SelectEvent, SelectState};
use gpui_kit::component::setting::{SettingField, SettingGroup, SettingItem, SettingPage};
use gpui_kit::component::slider::SliderState;
use gpui_kit::*;

use super::{SettingsView, handle, slider, slider_field};
use crate::model::settings::{Settings, TabLayout};

/// 字体下拉的一项; name 为空表示系统默认.
#[derive(Clone)]
pub struct FontItem {
    name: SharedString,
    label: SharedString,
}

impl SearchableListItem for FontItem {
    type Value = SharedString;

    fn title(&self) -> SharedString {
        self.label.clone()
    }

    fn value(&self) -> &SharedString {
        &self.name
    }
}

pub type FontSelect = Entity<SelectState<SearchableVec<FontItem>>>;

/// 字体下拉: 第一项系统默认, 后面按名字排列本机字体和 fonts 目录带的字体, 可搜索. 选中即保存.
pub fn font_select(settings: &Entity<Settings>, window: &mut Window, cx: &mut Context<SettingsView>) -> FontSelect {
    // 系统字体和 fonts 目录的字体分属两个集合, 合在一起排序去重
    let mut names = cx.text_system().all_font_names();
    names.sort_unstable();
    names.dedup();
    let items: Vec<FontItem> =
        std::iter::once(FontItem { name: SharedString::default(), label: "系统默认".into() })
            .chain(names.into_iter().map(|name| {
                let name = SharedString::from(name);
                FontItem { name: name.clone(), label: name }
            }))
            .collect();
    let current = settings.read(cx).appearance.font_family.as_str();
    let selected = items.iter().position(|item| item.name == current).map(IndexPath::new);
    let select = cx.new(|cx| SelectState::new(SearchableVec::new(items), selected, window, cx).searchable(true));

    let settings = settings.clone();
    cx.subscribe(&select, move |_, _, event: &SelectEvent<SearchableVec<FontItem>>, cx| {
        let SelectEvent::Confirm(Some(name)) = event else {
            return;
        };
        let name = name.to_string();
        settings.update(cx, |s, cx| s.change(cx, |s| s.appearance.font_family = name));
    })
    .detach();
    select
}

/// 图标大小拖动中面板就跟着缩放, 边拖边看.
pub fn icon_size_slider(settings: &Entity<Settings>, cx: &mut Context<SettingsView>) -> Entity<SliderState> {
    slider(settings, (24., 96., 4.), |s| s.appearance.icon_size, |s, v| s.appearance.icon_size = v, true, cx)
}

pub fn page(
    settings: &Entity<Settings>,
    font_select: &FontSelect,
    icon_size: &Entity<SliderState>,
    cx: &App,
) -> SettingPage {
    let (layout_read, layout_write) = handle(settings);
    let font_select = font_select.clone();
    let icon_size_value = icon_size.read(cx).value().end();

    SettingPage::new("外观").icon(IconName::Palette).group(
        SettingGroup::new()
            .item(SettingItem::new(
                "界面字体",
                SettingField::render(move |_, _, _| {
                    // 字体有几百个, 限高滚动, 可输入名字搜索
                    Select::new(&font_select)
                        .w(px(180.))
                        .menu_max_h(px(320.))
                        .placeholder("系统默认")
                        .search_placeholder("搜索字体")
                }),
            ))
            .item(SettingItem::new(
                "分组标签",
                SettingField::dropdown(
                    vec![
                        ("horizontal".into(), "横排 (图标上方)".into()),
                        ("vertical".into(), "竖排 (图标左侧)".into()),
                    ],
                    move |cx| match layout_read.read(cx).appearance.tab_layout {
                        TabLayout::Horizontal => "horizontal".into(),
                        TabLayout::Vertical => "vertical".into(),
                    },
                    move |value, cx| {
                        let layout = if value == "vertical" { TabLayout::Vertical } else { TabLayout::Horizontal };
                        layout_write.update(cx, |s, cx| s.change(cx, |s| s.appearance.tab_layout = layout));
                    },
                ),
            ))
            .item(
                SettingItem::new("图标大小", slider_field(icon_size, false))
                    .description(format!("{icon_size_value} 像素")),
            ),
    )
}
