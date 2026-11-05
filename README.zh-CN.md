<div align="center">

<img src="assets/qrun.png" width="128" alt="QRun">

# QRun

<p><a href="README.md">English</a> | 简体中文</p>

<p><strong>轻量的 Windows 快速启动器，用 Rust 和 GPUI 编写。</strong></p>

</div>

> QRun 把常用的应用、文件和文件夹按分组摆在一个面板里，点一下就打开。

## ✨ 功能

- **分组面板**：用标签页整理项目，右键分组可以重命名或删除。
- **拖拽添加**：把文件、文件夹或快捷方式拖进面板，自动读取系统图标。
- **点击启动**：用系统默认方式打开文件、文件夹、快捷方式和网址。
- **搜索**：跨分组按名称过滤，回车打开第一个结果。
- **项目右键菜单**：以管理员身份运行、打开方式、打开所在位置、复制路径、编辑（名称、目标、参数、始终以管理员权限启动）、删除。
- **贴边悬停**：把面板拖到屏幕上、左、右边附近或拖出边外即可吸附；鼠标离开后滑出屏幕，鼠标碰到该边时滑回。
- **系统托盘**：面板不占任务栏，单击托盘图标打开，右键弹出菜单。
- **设置窗口**：从标题栏齿轮按钮或托盘菜单打开，分常规（开机自启、单击或双击启动、悬停切换分组、置于顶层、启动后收起）、外观（界面字体、标签横排/竖排、图标大小、不透明度）、贴边、关于（版本、检查更新）四页。
- **开机自启**：写入当前用户注册表的 `Run` 项，只在你打开开关时写入。
- **自动更新**：启动时和之后每 24 小时检查新版本，先查 GitHub，查不到再用国内镜像 [CNB](https://cnb.cool/alsc1n/qrun)。安装版下载安装包、校验 SHA-256 后自动更新；绿色版打开下载页。
- **便携数据**：`settings.json` 和 `library.json` 都在 exe 旁边，整个目录可以直接拷走。

规划中：全局热键呼出。

## 🛠 从源码构建

需要 Windows 10/11 和最新稳定版 Rust（MSVC 工具链）。

```sh
git clone https://github.com/alsc1n/qrun.git
cd qrun
cargo run            # 开发运行
cargo build --release
```

发布版 exe 在 `target/release/qrun.exe`。

## ⚙️ 配置

首次运行会在 exe 同目录生成两个文件。`settings.json` 是设置，一般在设置窗口里改：

| 字段 | 说明 |
| --- | --- |
| `general.launch_click` | `single`（单击启动项目）或 `double`（双击启动） |
| `general.hover_switch_group` | 鼠标停在分组标签上就切换分组 |
| `general.always_on_top` | 未贴边时面板也保持在最上层（贴边时总是置顶） |
| `general.hide_after_launch` | 启动项目后收起面板（贴边时不收） |
| `appearance.tab_layout` | `horizontal`（标签在图标上方）或 `vertical`（标签在左侧） |
| `appearance.font_family` | 界面字体的字体族名，默认 `HarmonyOS Sans SC`（随程序放在 exe 旁的 `fonts` 目录）；空表示系统默认（Segoe UI，中文用微软雅黑）；本机没装、`fonts` 里也没有的字体同样用系统默认 |
| `appearance.icon_size` / `appearance.opacity` | 图标显示尺寸，面板背景不透明度（0 到 1） |
| `dock.enabled` | 是否启用贴边 |
| `dock.lock_position` | 锁定贴边位置：吸附后窗口拖不动，再次启动 QRun 也不解除吸附 |
| `dock.snap_distance` | 松手时离屏幕边多近（逻辑像素）就吸附 |
| `dock.hide_delay_ms` / `dock.animation_ms` | 吸附后鼠标离开多久缩回，以及滑动动画时长 |
| `window.width` / `window.height` | 面板大小（逻辑像素） |

开机自启不存在这里，以注册表为准。

两个文件顶层都有 `version`，记录文件结构的版本。缺少的设置项取默认值，写错的字段名会报错。文件来自更新版本的 QRun 时，原样不动并提示升级。

`library.json` 是分组和项目，每个项目有 `id`、`name`、`target`，以及可选的 `args`（参数）和 `admin`（管理员启动）。
任一文件读不了时，QRun 会弹窗说明原因并退出，不会带着坏数据启动。

## 🤝 参与贡献

欢迎提 Issue 和 PR。提交前请确认：

```sh
cargo fmt --check
cargo clippy -- -D warnings
```

提交信息使用 [Conventional Commits](https://www.conventionalcommits.org/)，例如 `feat(search): match pinyin initials`。

## 📄 许可证

[Apache License 2.0](LICENSE)。第三方声明见 [NOTICE](NOTICE)。

## 🙏 致谢

- [GPUI](https://github.com/zed-industries/zed) 和 [GPUI Kit](https://github.com/longbridge/gpui-kit)：界面框架与组件。
- [Lucide](https://lucide.dev)：界面图标。
- [HarmonyOS Sans](https://developer.huawei.com/consumer/cn/design/resource/)：界面字体，按 HarmonyOS Sans 字体许可协议使用。
