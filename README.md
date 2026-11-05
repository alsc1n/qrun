<div align="center">

<img src="assets/qrun.png" width="128" alt="QRun">

# QRun

<p>English | <a href="README.zh-CN.md">简体中文</a></p>

<p><strong>A lightweight launcher for Windows, written in Rust with GPUI.</strong></p>

</div>

> QRun keeps the apps, files and folders you use every day in grouped tabs on one panel, and opens any of them
> with a click.

## ✨ Features

- **Grouped panel**: organize items into tabs; right-click a tab to rename or delete it.
- **Drag and drop**: drop files, folders or shortcuts onto the panel; their system icons are picked up automatically.
- **Click to launch**: files, folders, shortcuts and URLs open with their default handler.
- **Search**: filter by name across all groups; press Enter to open the first match.
- **Item menu**: run as administrator, open with, open file location, copy path, edit (name, target, arguments,
  always run as administrator) and delete.
- **Edge docking**: drag the panel to the top, left or right edge (or past it) to dock; it slides off-screen when the
  mouse leaves and slides back when the mouse touches that edge.
- **System tray**: the panel stays out of the taskbar; click the tray icon to show it, right-click for the menu.
- **Settings window**: open it from the gear button or the tray menu. Pages: General (start with Windows, single or
  double click to launch, switch groups on hover, always on top, hide after launch), Appearance (font, horizontal or vertical tabs, icon size, opacity), Docking and About (version, updates).
- **Start with Windows**: registered under the current user's `Run` registry key, only when you turn it on.
- **Updates**: checks for new releases at startup and every 24 hours (GitHub first, then the
  [CNB mirror](https://cnb.cool/alsc1n/qrun) for users in China). The installed version downloads the installer,
  verifies its SHA-256 and updates itself; the portable version opens the download page.
- **Portable data**: `settings.json` and `library.json` live next to the executable, so the whole folder can be moved.

Planned: a global hotkey.

## 🛠 Build from source

Requires Windows 10/11 and the latest stable Rust (MSVC toolchain).

```sh
git clone https://github.com/alsc1n/qrun.git
cd qrun
cargo run            # development
cargo build --release
```

The release executable is `target/release/qrun.exe`.

## ⚙️ Configuration

Two files are created next to the executable on first run. `settings.json` holds the preferences, which are normally
changed from the settings window:

| Field | Meaning |
| --- | --- |
| `general.launch_click` | `single` (launch items with a single click) or `double` (double click) |
| `general.hover_switch_group` | Switch groups by resting the mouse on a group tab |
| `general.always_on_top` | Keep the panel above other windows when not docked (it is always on top while docked) |
| `general.hide_after_launch` | Hide the panel after launching an item (not while docked) |
| `appearance.tab_layout` | `horizontal` (tabs above the icons) or `vertical` (tabs on the left) |
| `appearance.font_family` | UI font family, `HarmonyOS Sans SC` by default (shipped in the `fonts` folder next to the executable). Empty means the system font, Segoe UI with Microsoft YaHei for Chinese; a font that is neither installed nor in `fonts` also falls back to it |
| `appearance.icon_size` / `appearance.opacity` | Icon display size, and panel background opacity from 0 to 1 |
| `dock.enabled` | Whether edge docking is on |
| `dock.lock_position` | Lock the docked panel: it cannot be dragged away, and launching QRun again does not undock it |
| `dock.snap_distance` | How close to an edge (logical pixels) the panel must be dropped to dock |
| `dock.hide_delay_ms` / `dock.animation_ms` | Delay before a docked panel hides, and the slide duration |
| `window.width` / `window.height` | Panel size in logical pixels |

Start with Windows is not stored here; the registry is the single source of truth.

Both files carry a top-level `version` for their structure. Missing settings fall back to their defaults, while
unknown field names are reported as errors. A file written by a newer QRun is left untouched and QRun asks you to
upgrade.

`library.json` holds the groups and their items. Each item has an `id`, `name` and `target`, plus optional `args` and
`admin`. If either file cannot be read, QRun shows the reason and exits instead of starting with broken data.

## 🤝 Contributing

Issues and pull requests are welcome. Before committing, make sure these pass:

```sh
cargo fmt --check
cargo clippy -- -D warnings
```

Commit messages follow [Conventional Commits](https://www.conventionalcommits.org/), e.g.
`feat(search): match pinyin initials`.

## 📄 License

[Apache License 2.0](LICENSE). Third-party notices are in [NOTICE](NOTICE).

## 🙏 Acknowledgements

- [GPUI](https://github.com/zed-industries/zed) and [GPUI Kit](https://github.com/longbridge/gpui-kit) for the UI
  framework and components.
- [Lucide](https://lucide.dev) for the interface icons.
- [HarmonyOS Sans](https://developer.huawei.com/consumer/cn/design/resource/) for the interface font, used under the
  HarmonyOS Sans Fonts License Agreement.
