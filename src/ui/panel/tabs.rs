//! 分组标签: 切换, 新建, 重命名, 删除, 拖动排序, 接收拖来的项目.

use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, IconName, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use super::{DraggedGroup, DraggedItem, Panel};
use crate::model::library::Id;
use crate::params::PARAMS;
use crate::ui::truncated::truncated_text;

impl Panel {
    /// vertical 为 true 时竖排在图标左侧, 否则横排在图标上方并自动换行.
    pub(super) fn render_tabs(&self, vertical: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let entity = cx.entity().downgrade();
        let groups = self.library.read(cx).groups();
        let can_remove = groups.len() > 1;
        let hover_switch = self.settings.read(cx).general.hover_switch_group;
        let drop_target = theme.tokens.drop_target;
        // 标签间距: 竖排 4px (gap_1), 横排 8px (gap_2)
        let gap = if vertical { px(4.) } else { px(8.) };

        let container = if vertical {
            v_flex().id("tabs").w(px(140.)).h_full().overflow_y_scroll().px_3().pb_4().gap_1()
        } else {
            h_flex().id("tabs").px_4().pb_3().gap_2().flex_wrap()
        };
        // 标签, 重命名输入框, 新建按钮都是 32px 高 (控件默认 medium), 一排对齐
        container
            .children(groups.iter().enumerate().map(|(ix, group)| {
                let id = group.id;
                if self.renaming == Some(id) {
                    return Input::new(&self.rename).when(!vertical, |el| el.w(px(120.))).into_any_element();
                }
                let (rename_entity, remove_entity) = (entity.clone(), entity.clone());
                let prev = ix.checked_sub(1).map(|ix| groups[ix].id);
                let next = groups.get(ix + 1).map(|group| group.id);
                div()
                    .id(("group", id))
                    .px_4()
                    .py_1p5()
                    .map(|el| if vertical { el.w_full().rounded_md() } else { el.rounded_full() })
                    .text_sm()
                    .cursor_pointer()
                    .map(|el| {
                        if id == self.active_group {
                            el.bg(theme.secondary).text_color(theme.foreground)
                        } else {
                            el.text_color(theme.muted_foreground).hover(|s| s.bg(theme.list_hover))
                        }
                    })
                    .map(|el| if vertical { el.truncate() } else { el })
                    .child(truncated_text("name", group.name.clone()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.active_group = id;
                        cx.notify();
                    }))
                    .when(hover_switch, |el| {
                        el.on_hover(cx.listener(move |this, hovered: &bool, _, cx| this.hover_group(id, *hovered, cx)))
                    })
                    .on_drag(DraggedGroup { id, name: group.name.clone().into() }, |drag, _, _, cx| {
                        cx.new(|_| drag.clone())
                    })
                    .on_drag_move(Self::track_drop::<DraggedGroup>(id, prev, next, vertical, gap, cx))
                    // 项目拖到标签上立即切到该分组, 可以接着拖进网格放到想要的位置
                    .on_drag_move(cx.listener(move |this, event: &DragMoveEvent<DraggedItem>, _, cx| {
                        if this.active_group != id && event.bounds.contains(&event.event.position) {
                            this.active_group = id;
                            cx.notify();
                        }
                    }))
                    // 直接在标签上松手, 移到该分组末尾
                    .drag_over::<DraggedItem>(move |s, _, _, _| s.bg(drop_target))
                    .on_drop(cx.listener(move |this, drag: &DraggedItem, _, cx| {
                        this.library.update(cx, |lib, cx| lib.move_item(drag.id, id, None, cx));
                    }))
                    .context_menu(move |menu, _, _| {
                        let (rename_entity, remove_entity) = (rename_entity.clone(), remove_entity.clone());
                        menu.item(PopupMenuItem::new("重命名").on_click(move |_, window, cx| {
                            rename_entity.update(cx, |this, cx| this.start_rename(id, window, cx)).ok();
                        }))
                        .when(can_remove, |menu| {
                            menu.item(PopupMenuItem::new("删除分组").on_click(move |_, _, cx| {
                                remove_entity
                                    .update(cx, |this, cx| this.library.update(cx, |lib, cx| lib.remove_group(id, cx)))
                                    .ok();
                            }))
                        })
                    })
                    .into_any_element()
            }))
            .child(Button::new("add-group").ghost().icon(IconName::Plus).on_click(cx.listener(
                |this, _, window, cx| {
                    let id = this.library.update(cx, |lib, cx| lib.add_group("新分组", cx));
                    this.active_group = id;
                    this.start_rename(id, window, cx);
                },
            )))
            // 光标在标签栏空白处时挪到最后 (最后一个标签后侧亮线); 在标签上时由标签改成精确位置
            .on_drag_move(Self::track_drop_tail::<DraggedGroup>(cx))
            // 松手按插入线的位置放; 没有插入线 (位置不变) 时不动
            .on_drop(cx.listener(|this, drag: &DraggedGroup, _, cx| {
                let Some(hint) = this.drop_hint.take() else {
                    return;
                };
                let ids: Vec<Id> = this.library.read(cx).groups().iter().map(|group| group.id).collect();
                let before = hint.before(&ids);
                this.library.update(cx, |lib, cx| lib.move_group(drag.id, before, cx));
            }))
    }

    /// 鼠标在标签上停够 hover_switch_delay_ms 才切过去; 中途移开就取消, 划过标签栏不会连着切.
    /// 进入下一个标签和离开上一个的事件先后不定, 所以离开时只取消自己那次.
    fn hover_group(&mut self, id: Id, hovered: bool, cx: &mut Context<Self>) {
        if !hovered {
            if self.hover_switch.as_ref().is_some_and(|(pending, _)| *pending == id) {
                self.hover_switch = None;
            }
            return;
        }
        if id == self.active_group {
            return;
        }
        let delay = Duration::from_millis(PARAMS.panel.hover_switch_delay_ms);
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            this.update(cx, |this, cx| {
                this.active_group = id;
                this.hover_switch = None;
                cx.notify();
            })
            .ok();
        });
        self.hover_switch = Some((id, task));
    }

    fn start_rename(&mut self, id: Id, window: &mut Window, cx: &mut Context<Self>) {
        let Some(group) = self.library.read(cx).group(id) else {
            return;
        };
        let name = group.name.clone();
        self.rename.update(cx, |state, cx| {
            state.set_value(name, window, cx);
            state.focus(window, cx);
        });
        self.renaming = Some(id);
        cx.notify();
    }

    /// 回车或失去焦点时提交.
    pub(super) fn on_rename_event(
        &mut self,
        _: &Entity<InputState>,
        event: &InputEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
            self.commit_rename(cx);
        }
    }

    /// 结束重命名并保存; 空名称不保存.
    pub(super) fn commit_rename(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.renaming.take() else {
            return;
        };
        let name = self.rename.read(cx).value().trim().to_string();
        if !name.is_empty() {
            self.library.update(cx, |lib, cx| lib.rename_group(id, &name, cx));
        }
        cx.notify();
    }
}
