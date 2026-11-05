//! 会被截断的文字: 放不下时末尾省略, 鼠标停留提示全称. 单行 (`truncate()`) 和多行 (`line_clamp(n).text_ellipsis()`) 都适用.

use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::*;

/// 截断样式由外层元素给; 这里只负责提示.
/// 排版后显示出来的文字和原文不同才说明被截断了, 只有这时才弹提示, 放得下的文字不打扰.
pub fn truncated_text(id: impl Into<ElementId>, text: impl Into<SharedString>) -> InteractiveText {
    let text: SharedString = text.into();
    let styled = StyledText::new(text.clone());
    let layout = styled.layout().clone();
    InteractiveText::new(id, styled).tooltip(move |_, window, cx| {
        (layout.text() != text.as_ref()).then(|| Tooltip::new(text.clone()).build(window, cx))
    })
}
