//! 跨窗口的应用级动作. 界面只负责发出, 由 app 统一处理, 界面不必知道窗口怎么打开.

use gpui_kit::actions;

actions!(qrun, [OpenSettings, OpenUpdate]);
