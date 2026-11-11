//! 拖到别的程序和复制文件. 面板里照常用 GPUI 拖动 (排序, 换分组); 光标拖出面板窗口时, 由 app::poll 收掉面板里的
//! 拖动预览, 交给系统拖放 (`shell::drag_out`), 才能放进微信, 资源管理器等. GPUI 在 Windows 上没有实现拖到窗口外.
//! 程序 (exe) 按设置拖出为快捷方式: 在临时目录按项目名称和参数写一个, 拖它出去; 快捷方式本身原样拖出.
//! 拖出去放下了, 或复制了文件, 都算用了一次, 和点开一样记进启动记录, 推荐从中学习.

use std::fs;
use std::path::{Path, PathBuf};

use gpui_kit::*;

use super::{DraggedItem, Panel, drag_preview};
use crate::model::library::{Id, Via};
use crate::model::settings::DragProgram;
use crate::platform::shell;

/// 文件名里不能出现的字符, 项目名称里有的换成下划线.
const INVALID_NAME_CHARS: &str = r#"\/:*?"<>|"#;

/// 正在拖的目标, 拖出窗口时交给系统; name 是生成快捷方式时的文件名, item 和 via 用来记这次使用.
pub(super) struct DragOut {
    name: SharedString,
    target: PathBuf,
    item: Option<Id>,
    via: Via,
    /// 已交给系统拖放; 这期间面板收到的外部拖入就是它自己, 见 `own_drop`.
    system: bool,
    /// 在面板自己身上松了手, 不算用了一次.
    returned: bool,
}

impl DragOut {
    fn new(name: SharedString, target: PathBuf, item: Option<Id>, via: Via) -> Self {
        Self { name, target, item, via, system: false, returned: false }
    }
}

/// 推荐页上拖动的格子, 同时是跟随鼠标的预览. 面板里没有地方接它, 只能拖出面板.
#[derive(Clone)]
pub(super) struct DraggedTile {
    pub name: SharedString,
    pub target: PathBuf,
    pub item: Option<Id>,
    pub via: Via,
}

impl Render for DraggedTile {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        drag_preview(self.name.clone(), cx)
    }
}

impl Panel {
    /// 面板根上: 拖着分组里的项目时记下它; 光标出了窗口就收不到移动事件了, 所以每次移动都记.
    pub(super) fn track_drag_out_item(
        cx: &Context<Self>,
    ) -> impl Fn(&DragMoveEvent<DraggedItem>, &mut Window, &mut App) + 'static {
        cx.listener(|this, event: &DragMoveEvent<DraggedItem>, _, cx| {
            let drag = event.drag(cx);
            let (name, target, item) = (drag.name.clone(), drag.target.clone(), Some(drag.id));
            this.drag_out = Some(DragOut::new(name, target, item, this.grid_via(cx)));
        })
    }

    pub(super) fn track_drag_out_tile(
        cx: &Context<Self>,
    ) -> impl Fn(&DragMoveEvent<DraggedTile>, &mut Window, &mut App) + 'static {
        cx.listener(|this, event: &DragMoveEvent<DraggedTile>, _, cx| {
            let drag = event.drag(cx);
            this.drag_out = Some(DragOut::new(drag.name.clone(), drag.target.clone(), drag.item, drag.via));
        })
    }

    /// 正在拖分组里的项目或推荐页的格子. app::poll 看到它拖出了窗口就交给系统.
    pub fn dragging(&self, cx: &App) -> bool {
        self.drag_out.is_some() && cx.has_active_drag()
    }

    /// 交给系统拖出去的文件: 程序按设置换成快捷方式 (名称和参数取项目的), 其余原样. 快捷方式写不了时为 None.
    pub fn start_system_drag(&mut self, cx: &App) -> Option<PathBuf> {
        let drag = self.drag_out.as_mut()?;
        drag.system = true;
        if !shell::is_program(&drag.target) || self.settings.read(cx).general.drag_program == DragProgram::File {
            return Some(drag.target.clone());
        }
        let args = drag.item.and_then(|id| self.library.read(cx).item(id)).map(|item| item.args.clone());
        let dir = std::env::temp_dir().join("QRun").join("drag");
        // 上次拖出的快捷方式早被放置目标复制走了, 每次清空重写, 目录里不会越积越多
        let _ = fs::remove_dir_all(&dir);
        if let Err(e) = fs::create_dir_all(&dir) {
            log::warn!("创建 {} 失败: {e}", dir.display());
            return None;
        }
        let name: String = drag.name.chars().map(|c| if INVALID_NAME_CHARS.contains(c) { '_' } else { c }).collect();
        let link = dir.join(format!("{name}.lnk"));
        shell::create_shortcut(&link, &drag.target, args.as_deref().unwrap_or_default()).then_some(link)
    }

    /// 外部拖入的松手处调用: 是自己交给系统的拖放又放回了面板时返回 true, 调用方不添加 (拖出窗口又拖回来松手,
    /// 不该多出一个项目), 这次也不算用了一次.
    pub(super) fn own_drop(&mut self) -> bool {
        let Some(drag) = self.drag_out.as_mut().filter(|drag| drag.system) else {
            return false;
        };
        drag.returned = true;
        true
    }

    /// 自己的项目正由系统拖着; 这时经过面板不显示快捷方式分区.
    pub(super) fn in_system_drag(&self) -> bool {
        self.drag_out.as_ref().is_some_and(|drag| drag.system)
    }

    /// 系统拖放结束 (松手): 放到了别处就记一次使用, 记的是项目本身, 不是临时快捷方式.
    pub fn dragged_out(&mut self, dropped: bool, cx: &mut Context<Self>) {
        if let Some(drag) = self.drag_out.take()
            && dropped
            && !drag.returned
        {
            self.used(drag.item, &drag.target, drag.via, cx);
        }
    }

    /// 把文件放进剪贴板, 成功就记一次使用.
    pub(super) fn copy_file(&self, item: Option<Id>, target: &Path, via: Via, cx: &mut Context<Self>) {
        if shell::copy_file(target) {
            self.used(item, target, via, cx);
        }
    }

    /// 拖出或复制: 和点开一样记进启动记录 (分组里的项目计点击次数), 但不启动.
    fn used(&self, item: Option<Id>, target: &Path, via: Via, cx: &mut Context<Self>) {
        self.library.update(cx, |lib, cx| lib.record_launch(item, target, via, cx));
    }
}
