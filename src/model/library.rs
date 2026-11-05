//! 启动板数据: 分组和项目, 保存在 library.json.
//! 分组和项目都用稳定 ID 定位, 增删不会让别处持有的引用指错对象.

use std::path::{Path, PathBuf};

use gpui_kit::Context;
use serde::{Deserialize, Serialize};

use crate::storage;

const FILE: &str = "library.json";
/// 文件结构版本; 破坏性变更时加一, 并在 storage 读取前把旧结构迁移过来.
const VERSION: u32 = 1;

pub type Id = u64;

#[derive(Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Library {
    groups: Vec<Group>,
    /// 下一个可用 ID, 加载时按已有最大 ID 推算.
    #[serde(skip)]
    next_id: Id,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Group {
    pub id: Id,
    pub name: String,
    pub items: Vec<Item>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Item {
    pub id: Id,
    pub name: String,
    pub target: PathBuf,
    /// 启动参数, 大多数项目不填.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub args: String,
    /// 始终以管理员权限启动.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub admin: bool,
}

/// 编辑对话框提交的内容.
pub struct ItemEdit {
    pub name: String,
    pub target: PathBuf,
    pub args: String,
    pub admin: bool,
}

impl Library {
    pub fn load() -> Result<Self, String> {
        // 只有版本 1, 还没有要迁移的旧结构
        let mut library: Self =
            storage::load(FILE, VERSION, |from, _| unreachable!("library.json 没有版本 {from} 的迁移"))?;
        let max_id =
            library.groups.iter().flat_map(|g| std::iter::once(g.id).chain(g.items.iter().map(|i| i.id))).max();
        library.next_id = max_id.map_or(1, |id| id + 1);
        // 首次运行给一个默认分组
        if library.groups.is_empty() {
            let id = library.allocate_id();
            library.groups.push(Group { id, name: "常用".into(), items: Vec::new() });
            library.save();
        }
        Ok(library)
    }

    pub fn groups(&self) -> &[Group] {
        &self.groups
    }

    pub fn group(&self, id: Id) -> Option<&Group> {
        self.groups.iter().find(|g| g.id == id)
    }

    pub fn item(&self, id: Id) -> Option<&Item> {
        self.groups.iter().flat_map(|g| &g.items).find(|i| i.id == id)
    }

    /// 跨所有分组按名称匹配, 不区分大小写.
    pub fn search(&self, query: &str) -> Vec<&Item> {
        let query = query.to_lowercase();
        self.groups.iter().flat_map(|g| &g.items).filter(|i| i.name.to_lowercase().contains(&query)).collect()
    }

    pub fn add_group(&mut self, name: &str, cx: &mut Context<Self>) -> Id {
        let id = self.allocate_id();
        self.groups.push(Group { id, name: name.into(), items: Vec::new() });
        self.changed(cx);
        id
    }

    pub fn rename_group(&mut self, id: Id, name: &str, cx: &mut Context<Self>) {
        if let Some(group) = self.groups.iter_mut().find(|g| g.id == id) {
            group.name = name.into();
            self.changed(cx);
        }
    }

    /// 至少保留一个分组.
    pub fn remove_group(&mut self, id: Id, cx: &mut Context<Self>) {
        if self.groups.len() > 1 {
            self.groups.retain(|g| g.id != id);
            self.changed(cx);
        }
    }

    /// 拖进来的文件或文件夹加到分组末尾, 名称取文件名 (文件去掉扩展名).
    pub fn add_paths(&mut self, group: Id, paths: &[PathBuf], cx: &mut Context<Self>) {
        let items: Vec<Item> = paths
            .iter()
            .map(|path| Item {
                id: self.allocate_id(),
                name: display_name(path),
                target: path.clone(),
                args: String::new(),
                admin: false,
            })
            .collect();
        if let Some(group) = self.groups.iter_mut().find(|g| g.id == group) {
            group.items.extend(items);
            self.changed(cx);
        }
    }

    /// 快捷方式解析出的程序加到分组末尾, 名称取快捷方式名, 参数沿用快捷方式里的.
    pub fn add_link_target(&mut self, group: Id, link: &Path, target: PathBuf, args: String, cx: &mut Context<Self>) {
        let item = Item { id: self.allocate_id(), name: display_name(link), target, args, admin: false };
        if let Some(group) = self.groups.iter_mut().find(|g| g.id == group) {
            group.items.push(item);
            self.changed(cx);
        }
    }

    /// 从"添加应用"列表选中的应用加到分组末尾.
    pub fn add_app(&mut self, group: Id, name: String, target: PathBuf, cx: &mut Context<Self>) {
        let item = Item { id: self.allocate_id(), name, target, args: String::new(), admin: false };
        if let Some(group) = self.groups.iter_mut().find(|g| g.id == group) {
            group.items.push(item);
            self.changed(cx);
        }
    }

    /// 把分组挪到 before 前面, before 为 None 时挪到最后.
    pub fn move_group(&mut self, id: Id, before: Option<Id>, cx: &mut Context<Self>) {
        if before == Some(id) {
            return;
        }
        let Some(from) = self.groups.iter().position(|g| g.id == id) else {
            return;
        };
        let group = self.groups.remove(from);
        let to = before.and_then(|b| self.groups.iter().position(|g| g.id == b)).unwrap_or(self.groups.len());
        self.groups.insert(to, group);
        self.changed(cx);
    }

    /// 把项目挪到 group 分组里 before 前面, before 为 None 时挪到末尾; 同组即排序, 跨组即移动.
    pub fn move_item(&mut self, id: Id, group: Id, before: Option<Id>, cx: &mut Context<Self>) {
        if before == Some(id) {
            return;
        }
        let Some(item) = self.groups.iter_mut().find_map(|g| {
            let ix = g.items.iter().position(|i| i.id == id)?;
            Some(g.items.remove(ix))
        }) else {
            return;
        };
        let items = &mut self.groups.iter_mut().find(|g| g.id == group).expect("target group exists").items;
        let to = before.and_then(|b| items.iter().position(|i| i.id == b)).unwrap_or(items.len());
        items.insert(to, item);
        self.changed(cx);
    }

    pub fn update_item(&mut self, id: Id, edit: ItemEdit, cx: &mut Context<Self>) {
        if let Some(item) = self.groups.iter_mut().flat_map(|g| &mut g.items).find(|i| i.id == id) {
            item.name = edit.name;
            item.target = edit.target;
            item.args = edit.args;
            item.admin = edit.admin;
            self.changed(cx);
        }
    }

    pub fn remove_item(&mut self, id: Id, cx: &mut Context<Self>) {
        for group in &mut self.groups {
            group.items.retain(|i| i.id != id);
        }
        self.changed(cx);
    }

    fn allocate_id(&mut self) -> Id {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn save(&self) {
        storage::save(FILE, VERSION, self);
    }

    fn changed(&self, cx: &mut Context<Self>) {
        self.save();
        cx.notify();
    }
}

fn display_name(path: &Path) -> String {
    if path.is_dir() { path.file_name() } else { path.file_stem() }
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}
