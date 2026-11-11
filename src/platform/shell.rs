//! Windows Shell: 提取文件图标, 读取和创建快捷方式, 列出已安装应用, 启动, 打开方式, 打开所在位置, 拖到别的程序, 复制文件.

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use gpui_kit::RenderImage;
use image::{Frame, RgbaImage};
use windows::Win32::Foundation::{COLORREF, HWND, MAX_PATH, POINT, SIZE};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DeleteObject, GetDC, GetDIBits, GetObjectW, HGDIOBJ,
    ReleaseDC,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoCreateInstance, CoInitializeEx,
    CoTaskMemFree, CoUninitialize, IDataObject, IPersistFile, STGM_READ,
};
use windows::Win32::System::Ole::{
    DROPEFFECT_COPY, DROPEFFECT_LINK, DROPEFFECT_NONE, IDropSource, OleFlushClipboard, OleSetClipboard,
};
use windows::Win32::UI::Shell::{
    BHID_DataObject, BHID_EnumItems, CLSID_DragDropHelper, DSH_ALLOWDROPDESCRIPTIONTEXT, IDragSourceHelper2,
    IEnumShellItems, IShellItem, IShellItemImageFactory, IShellLinkW, SHCreateItemFromParsingName, SHDRAGIMAGE,
    SHDoDragDrop, SIGDN, SIGDN_NORMALDISPLAY, SIGDN_PARENTRELATIVEPARSING, SIIGBF_BIGGERSIZEOK, SIIGBF_ICONONLY,
    ShellExecuteW, ShellLink,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{Interface, PCWSTR, w};

use super::wide;
use crate::params::PARAMS;

/// 快捷方式参数的最大长度 (IShellLink 文档里的 INFOTIPSIZE).
const LINK_ARGS_LEN: usize = 1024;

/// 快捷方式指向的本地文件和它带的启动参数.
pub struct Shortcut {
    pub target: PathBuf,
    pub args: String,
}

pub fn is_link(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("lnk"))
}

/// 读出 .lnk 的目标和参数. 目标不是存在的本地文件 (应用商店应用, MSI 广告快捷方式, 网址等) 时返回 None.
/// 要在已初始化 COM 的线程上调用 (GPUI 主线程已初始化).
pub fn read_shortcut(link: &Path) -> Option<Shortcut> {
    let link = wide(link);
    let mut target = [0u16; MAX_PATH as usize];
    let mut args = [0u16; LINK_ARGS_LEN];
    unsafe {
        let shell_link: IShellLinkW =
            CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).expect("create ShellLink");
        shell_link
            .cast::<IPersistFile>()
            .expect("ShellLink as IPersistFile")
            .Load(PCWSTR(link.as_ptr()), STGM_READ)
            .ok()?;
        shell_link.GetPath(&mut target, std::ptr::null_mut(), 0).ok()?;
        shell_link.GetArguments(&mut args).expect("read shortcut arguments");
    }
    let target = PathBuf::from(OsString::from_wide(until_nul(&target)));
    target.exists().then(|| Shortcut { target, args: String::from_utf16_lossy(until_nul(&args)) })
}

fn until_nul(buf: &[u16]) -> &[u16] {
    &buf[..buf.iter().position(|&c| c == 0).unwrap_or(buf.len())]
}

/// 系统"所有应用"里的一项; target 是 `shell:AppsFolder\<应用 ID>`, 可直接启动和取图标.
pub struct InstalledApp {
    pub name: String,
    pub target: PathBuf,
}

/// 列出 shell:AppsFolder 里的应用 (含应用商店应用), 按名称排序.
/// 要在已初始化 COM 的线程上调用 (GPUI 主线程已初始化).
pub fn installed_apps() -> Vec<InstalledApp> {
    let mut apps = Vec::new();
    unsafe {
        let folder: IShellItem = SHCreateItemFromParsingName(w!("shell:AppsFolder"), None).expect("open AppsFolder");
        let items: IEnumShellItems = folder.BindToHandler(None, &BHID_EnumItems).expect("enumerate AppsFolder");
        loop {
            let mut item = [None];
            let mut fetched = 0;
            let _ = items.Next(&mut item, Some(&mut fetched));
            let [Some(item)] = item else {
                break;
            };
            apps.push(InstalledApp {
                name: display_name(&item, SIGDN_NORMALDISPLAY),
                // 商店应用是 AUMID, 普通程序是已知文件夹 GUID 开头的路径, 拼上 shell:AppsFolder 都能启动
                target: PathBuf::from(format!(
                    "shell:AppsFolder\\{}",
                    display_name(&item, SIGDN_PARENTRELATIVEPARSING)
                )),
            });
        }
    }
    apps.sort_by_key(|app| app.name.to_lowercase());
    apps
}

unsafe fn display_name(item: &IShellItem, kind: SIGDN) -> String {
    unsafe {
        let name = item.GetDisplayName(kind).expect("read shell item name");
        let text = name.to_string().expect("shell item name is UTF-16");
        CoTaskMemFree(Some(name.0 as *const _));
        text
    }
}

/// 取目标的 Shell 图标, 转成 GPUI 可直接绘制的图像. 目标不存在时返回 None.
pub fn icon(path: &Path, size: i32) -> Option<Arc<RenderImage>> {
    let path = wide(path);
    unsafe {
        let factory: IShellItemImageFactory = SHCreateItemFromParsingName(PCWSTR(path.as_ptr()), None).ok()?;
        let hbitmap = factory.GetImage(SIZE { cx: size, cy: size }, SIIGBF_ICONONLY | SIIGBF_BIGGERSIZEOK).ok()?;

        let mut bitmap = BITMAP::default();
        GetObjectW(HGDIOBJ(hbitmap.0), size_of::<BITMAP>() as i32, Some(&mut bitmap as *mut _ as *mut _));
        let (width, height) = (bitmap.bmWidth, bitmap.bmHeight);

        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                // 负高度表示自上而下的行序
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        let hdc = GetDC(None);
        GetDIBits(hdc, hbitmap, 0, height as u32, Some(pixels.as_mut_ptr() as *mut _), &mut info, DIB_RGB_COLORS);
        ReleaseDC(None, hdc);
        let _ = DeleteObject(HGDIOBJ(hbitmap.0));

        // Shell 给的是预乘 alpha 的 BGRA, GPUI 要非预乘的 BGRA.
        for px in pixels.as_chunks_mut::<4>().0 {
            let a = px[3] as u32;
            if a > 0 && a < 255 {
                for c in &mut px[..3] {
                    *c = ((*c as u32 * 255) / a).min(255) as u8;
                }
            }
        }

        let buffer = RgbaImage::from_raw(width as u32, height as u32, pixels)?;
        Some(Arc::new(RenderImage::new([Frame::new(buffer)])))
    }
}

/// 用系统默认方式打开文件, 文件夹, 快捷方式或网址; as_admin 时以管理员身份运行.
pub fn run(target: &Path, args: &str, as_admin: bool) {
    execute(if as_admin { "runas" } else { "open" }, target, args);
}

/// 弹出系统的"选择打开方式"对话框.
pub fn open_with(target: &Path) {
    execute("openas", target, "");
}

/// 在资源管理器中打开所在文件夹并选中它.
pub fn reveal(target: &Path) {
    // explorer 只认 /select,"路径" 这种写法, 不能让 Rust 再整体加一层引号
    let _ = Command::new("explorer").raw_arg(format!("/select,\"{}\"", target.display())).spawn();
}

pub fn is_program(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
}

/// 在 link 写一个指向 target 的快捷方式, 带上启动参数, 工作目录取程序所在目录. 写不了时写日志, 返回 false.
pub fn create_shortcut(link: &Path, target: &Path, args: &str) -> bool {
    let (link_w, target_w, args_w) = (wide(link), wide(target), wide(args));
    let dir = target.parent().map(wide);
    let result: windows::core::Result<()> = unsafe {
        let shell_link: IShellLinkW =
            CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).expect("create ShellLink");
        shell_link.SetPath(PCWSTR(target_w.as_ptr())).expect("set shortcut target");
        shell_link.SetArguments(PCWSTR(args_w.as_ptr())).expect("set shortcut arguments");
        if let Some(dir) = &dir {
            shell_link.SetWorkingDirectory(PCWSTR(dir.as_ptr())).expect("set shortcut working directory");
        }
        shell_link.cast::<IPersistFile>().expect("ShellLink as IPersistFile").Save(PCWSTR(link_w.as_ptr()), true)
    };
    result.inspect_err(|e| log::warn!("创建快捷方式 {} 失败: {e}", link.display())).is_ok()
}

/// 把文件或文件夹拖到别的程序 (微信, 资源管理器等), 在主线程调用: 系统接管拖放直到松手才返回, 返回是否放下了.
/// 只允许复制和创建快捷方式, 不允许移动, 拖到资源管理器里不会把原文件挪走. 拿不到目标 (如已删除) 时写日志, 返回 false.
/// 跟着光标的是目标的系统图标 (图片是缩略图), 放到资源管理器等上方时显示"复制到 xxx", 和资源管理器里拖文件一样.
pub fn drag_out(hwnd: HWND, target: &Path) -> bool {
    let Some(data) = data_object(target) else {
        return false;
    };
    set_drag_image(&data, target);
    match unsafe { SHDoDragDrop(Some(hwnd), &data, None::<&IDropSource>, DROPEFFECT_COPY | DROPEFFECT_LINK) } {
        Ok(effect) => effect != DROPEFFECT_NONE,
        Err(e) => {
            log::warn!("拖出 {} 失败: {e}", target.display());
            false
        }
    }
}

/// 把文件或文件夹放进剪贴板, 在资源管理器, 微信里粘贴就是这个文件. 放好后交给系统保管, QRun 退出也还在.
pub fn copy_file(target: &Path) -> bool {
    let Some(data) = data_object(target) else {
        return false;
    };
    unsafe { OleSetClipboard(&data).and_then(|_| OleFlushClipboard()) }
        .inspect_err(|e| log::warn!("复制 {} 失败: {e}", target.display()))
        .is_ok()
}

/// 给拖放数据挂上拖动图像; 取不到图标时写日志, 由系统用默认样子拖.
fn set_drag_image(data: &IDataObject, target: &Path) {
    let size = PARAMS.panel.drag_image_size;
    let path = wide(target);
    let result: windows::core::Result<()> = unsafe {
        let helper: IDragSourceHelper2 =
            CoCreateInstance(&CLSID_DragDropHelper, None, CLSCTX_INPROC_SERVER).expect("create DragDropHelper");
        // 允许放置目标在图标下方显示"复制到 xxx"这类说明
        helper.SetFlags(DSH_ALLOWDROPDESCRIPTIONTEXT.0 as u32).expect("set drag helper flags");
        SHCreateItemFromParsingName::<_, _, IShellItemImageFactory>(PCWSTR(path.as_ptr()), None)
            .and_then(|factory| factory.GetImage(SIZE { cx: size, cy: size }, SIIGBF_BIGGERSIZEOK))
            .and_then(|bitmap| {
                let mut info = BITMAP::default();
                GetObjectW(HGDIOBJ(bitmap.0), size_of::<BITMAP>() as i32, Some(&mut info as *mut _ as *mut _));
                let image = SHDRAGIMAGE {
                    sizeDragImage: SIZE { cx: info.bmWidth, cy: info.bmHeight },
                    // 光标在图标中间
                    ptOffset: POINT { x: info.bmWidth / 2, y: info.bmHeight / 2 },
                    hbmpDragImage: bitmap,
                    // CLR_NONE: 不按颜色抠图, 用位图自带的透明度
                    crColorKey: COLORREF(u32::MAX),
                };
                // 成功后位图归系统, 失败要自己释放
                helper.InitializeFromBitmap(&image, data).inspect_err(|_| {
                    let _ = DeleteObject(HGDIOBJ(bitmap.0));
                })
            })
    };
    if let Err(e) = result {
        log::warn!("取 {} 的拖动图像失败: {e}", target.display());
    }
}

/// 资源管理器给文件用的数据对象 (带文件列表等格式), 拖放和剪贴板都认.
fn data_object(target: &Path) -> Option<IDataObject> {
    let path = wide(target);
    let result: windows::core::Result<IDataObject> = unsafe {
        SHCreateItemFromParsingName::<_, _, IShellItem>(PCWSTR(path.as_ptr()), None)
            .and_then(|item| item.BindToHandler(None, &BHID_DataObject))
    };
    result.inspect_err(|e| log::warn!("读取 {} 失败: {e}", target.display())).ok()
}

/// ShellExecute 解析快捷方式和拉起进程可能要一两秒, 放到后台线程, 界面不跟着卡.
fn execute(verb: &str, target: &Path, args: &str) {
    super::env::reset_to_user_default();
    let verb = wide(verb);
    let file = wide(target);
    let args = wide(args);
    // shell:AppsFolder\... 这类目标的上一级不是真实目录, 不能当工作目录
    let dir = target.parent().filter(|dir| dir.is_dir()).map(wide);
    std::thread::spawn(move || unsafe {
        // ShellExecute 要求调用线程已初始化 COM
        CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE).ok().expect("init COM");
        ShellExecuteW(
            None,
            PCWSTR(verb.as_ptr()),
            PCWSTR(file.as_ptr()),
            PCWSTR(args.as_ptr()),
            dir.as_ref().map_or(PCWSTR::null(), |d| PCWSTR(d.as_ptr())),
            SW_SHOWNORMAL,
        );
        CoUninitialize();
    });
}
