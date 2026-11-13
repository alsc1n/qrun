//! 设置窗口: 左侧分类, 右侧对应页面. 每个分类一个文件, 只读写 model::settings.

mod about;
mod appearance;
mod data;
mod dock;
mod general;
mod privacy;
mod smart;

use gpui_kit::component::ActiveTheme;
use gpui_kit::component::input::InputState;
use gpui_kit::component::setting::{SettingField, Settings as SettingsLayout};
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState};
use gpui_kit::*;

use crate::app::update::Updater;
use crate::model::library::Library;
use crate::model::settings::Settings;
use crate::platform::window;

pub struct SettingsView {
    settings: Entity<Settings>,
    /// 推荐页显示命中率.
    library: Entity<Library>,
    updater: Entity<Updater>,
    /// 字体下拉要跨帧保留搜索词和展开状态, 由视图持有.
    font_select: appearance::FontSelect,
    /// 滑动条同样要跨帧保留拖动状态.
    icon_size: Entity<SliderState>,
    dock_sliders: dock::DockSliders,
    smart_sliders: smart::SmartSliders,
    retention: Entity<SliderState>,
    ignore_inputs: privacy::IgnoreInputs,
    rolan_import: data::Status,
    data_location: data::Status,
    data_dir: Entity<InputState>,
    /// 系统给窗口画的底色, 跟着界面背景色; 见 `window::sync_backdrop`.
    backdrop: window::Backdrop,
    _subscriptions: Vec<Subscription>,
}

impl SettingsView {
    pub fn new(
        settings: Entity<Settings>,
        library: Entity<Library>,
        updater: Entity<Updater>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let font_select = appearance::font_select(&settings, window, cx);
        let icon_size = appearance::icon_size_slider(&settings, cx);
        let dock_sliders = dock::sliders(&settings, cx);
        let smart_sliders = smart::sliders(&settings, cx);
        let retention = privacy::retention_slider(&settings, cx);
        let ignore_inputs = privacy::ignore_inputs(&settings, window, cx);
        let rolan_import = cx.new(|_| None);
        let data_location = cx.new(|_| None);
        let data_dir = data::location_input(&data_location, window, cx);
        let _subscriptions = vec![
            cx.observe(&settings, |_, _, cx| cx.notify()),
            cx.observe(&updater, |_, _, cx| cx.notify()),
            // 新的启动会改变推荐命中率
            cx.observe(&library, |_, _, cx| cx.notify()),
            cx.observe(&rolan_import, |_, _, cx| cx.notify()),
            cx.observe(&data_location, |_, _, cx| cx.notify()),
        ];
        Self {
            settings,
            library,
            updater,
            font_select,
            icon_size,
            dock_sliders,
            smart_sliders,
            retention,
            ignore_inputs,
            rolan_import,
            data_location,
            data_dir,
            backdrop: None,
            _subscriptions,
        }
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window::sync_backdrop(&mut self.backdrop, window, cx.theme().background);
        SettingsLayout::new("settings").sidebar_width(px(180.)).pages([
            general::page(&self.settings),
            appearance::page(&self.settings, &self.font_select, &self.icon_size, cx),
            smart::page(&self.settings, &self.library, &self.smart_sliders, cx),
            privacy::page(&self.settings, &self.library, &self.retention, &self.ignore_inputs, cx),
            dock::page(&self.settings, &self.dock_sliders, cx),
            data::page(&self.library, &self.rolan_import, &self.data_dir, &self.data_location, cx),
            about::page(&self.settings, &self.updater, cx),
        ])
    }
}

/// 设置项的读写闭包都要各自持有一份 Entity.
fn handle(settings: &Entity<Settings>) -> (Entity<Settings>, Entity<Settings>) {
    (settings.clone(), settings.clone())
}

/// 设置页滑动条的状态, 由视图持有以跨帧保留拖动.
/// 拖动中 live 为 true 时只改内存让面板跟着变 (`Settings::preview`), 否则只重画设置页让说明显示当前值;
/// 松手才保存, 拖动时不反复写文件.
fn slider(
    settings: &Entity<Settings>,
    (min, max, step): (f32, f32, f32),
    get: fn(&Settings) -> f32,
    set: fn(&mut Settings, f32),
    live: bool,
    cx: &mut Context<SettingsView>,
) -> Entity<SliderState> {
    let value = get(settings.read(cx));
    let state = cx.new(|_| SliderState::new().min(min).max(max).step(step).default_value(value));
    let settings = settings.clone();
    // 鼠标一动就发 Change, 还停在同一格时也发; 跳到新的一格才通知, 免得面板和设置页白白重画
    let mut shown = value;
    cx.subscribe(&state, move |_, _, event: &SliderEvent, cx| match event {
        SliderEvent::Change(value) => {
            let value = value.end();
            if value == shown {
                return;
            }
            shown = value;
            if live {
                settings.update(cx, |s, cx| s.preview(cx, |s| set(s, value)));
            } else {
                cx.notify();
            }
        }
        // 松手总要保存: live 时设置里已是预览值, 和它比较会以为没变而漏存
        SliderEvent::Release(value) => {
            let value = value.end();
            settings.update(cx, |s, cx| s.change(cx, |s| set(s, value)));
        }
    })
    .detach();
    state
}

/// 设置项右侧的滑动条, 宽度与其他宽控件一致.
fn slider_field(state: &Entity<SliderState>, disabled: bool) -> SettingField<SharedString> {
    let state = state.clone();
    SettingField::render(move |_, _, _| Slider::new(&state).w(px(180.)).disabled(disabled))
}
