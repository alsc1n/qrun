//! 自动更新: 读发行版附带的 latest.json 判断有没有新版本. 安装版下载安装包 (带进度, 可取消), 校验 SHA-256 后
//! 静默安装并退出, 由安装包装完重新启动; 绿色版只打开下载页. 发行版地址按 params.json 的顺序尝试, GitHub 取不到就用国内镜像.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use gpui_kit::*;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::params::PARAMS;
use crate::platform::{http, install, tray};

/// 下载中刷新进度的间隔.
const PROGRESS_REFRESH: Duration = Duration::from_millis(100);

#[derive(Deserialize, Clone)]
pub struct Asset {
    pub name: String,
    pub sha256: String,
}

/// latest.json 里用得到的部分, 由 release.yml 生成; 绿色版附件只在下载页上给用户下.
#[derive(Deserialize, Clone)]
pub struct Release {
    pub version: String,
    pub setup: Asset,
    /// CHANGELOG.md 里该版本的一节 (Markdown); 0.2.1 及更早的 latest.json 没有.
    #[serde(default)]
    pub notes: String,
}

pub enum Status {
    Idle,
    Checking,
    UpToDate,
    /// 有新版本 (见 `Updater::release`), 等用户决定.
    Available,
    Downloading {
        received: u64,
        total: Option<u64>,
    },
    Verifying,
    Installing,
    /// 检查或下载失败; 下载失败时 `Updater::release` 仍在, 可以重试.
    Failed(String),
}

pub struct Updater {
    pub status: Status,
    /// 比当前版本新的发行版; 检查成功后更新, 下载失败或取消时保留.
    pub release: Option<Release>,
    /// 是安装包装的 (能静默更新), 还是绿色版 (只能打开下载页).
    pub installed: bool,
    /// 刚检查到新版本, 等 app 决定要不要弹窗提醒; true 表示是用户手动检查的, 跳过的版本也要提醒.
    pub offer: Option<bool>,
    /// 取到 latest.json 的发行版地址, 下载页用它.
    source: String,
    /// 下载循环每读一块看一次, 置位即中止.
    cancel: Arc<AtomicBool>,
}

/// 下载线程写, 界面定时读.
#[derive(Default)]
struct Progress {
    received: AtomicU64,
    /// 0 表示服务器没给总大小.
    total: AtomicU64,
}

impl Updater {
    pub fn new() -> Self {
        Self {
            status: Status::Idle,
            release: None,
            installed: install::installed(&PARAMS.installer_app_id),
            offer: None,
            source: PARAMS.update.sources[0].clone(),
            cancel: Arc::default(),
        }
    }

    pub fn busy(&self) -> bool {
        matches!(self.status, Status::Checking | Status::Downloading { .. } | Status::Verifying | Status::Installing)
    }

    /// asked: 用户手动点的检查, 有新版本时一定提醒.
    pub fn check(&mut self, asked: bool, cx: &mut Context<Self>) {
        if self.busy() {
            return;
        }
        self.status = Status::Checking;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async { latest() }).await;
            this.update(cx, |this, cx| {
                this.status = match result {
                    Ok((source, release)) if version(&release.version) > version(env!("CARGO_PKG_VERSION")) => {
                        this.source = source;
                        this.release = Some(release);
                        this.offer = Some(asked);
                        Status::Available
                    }
                    Ok(_) => {
                        this.release = None;
                        Status::UpToDate
                    }
                    Err(e) => Status::Failed(e),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 安装版: 下载, 校验, 静默安装, 然后退出. 绿色版: 打开该版本的下载页.
    pub fn install(&mut self, cx: &mut Context<Self>) {
        let Some(release) = self.release.clone() else {
            return;
        };
        if self.busy() {
            return;
        }
        if !self.installed {
            cx.open_url(&format!("{}/tag/v{}", self.source, release.version));
            return;
        }
        self.cancel.store(false, Ordering::Relaxed);
        self.status = Status::Downloading { received: 0, total: None };
        cx.notify();

        let progress = Arc::new(Progress::default());
        self.refresh_progress(progress.clone(), cx);
        let cancel = self.cancel.clone();
        cx.spawn(async move |this, cx| {
            let setup = release.setup.clone();
            let body = cx.background_executor().spawn(async move { download(&release, &progress, &cancel) }).await;
            let result = match body {
                Ok(body) => {
                    this.update(cx, |this, cx| this.set_status(Status::Verifying, cx)).ok();
                    cx.background_executor().spawn(async move { save(&setup, body) }).await
                }
                Err(e) => Err(e),
            };
            match result.and_then(|path| install::run_installer(&path)) {
                Ok(()) => {
                    this.update(cx, |this, cx| this.set_status(Status::Installing, cx)).ok();
                    tray::request_quit();
                }
                Err(e) => {
                    let status = if e == http::CANCELED { Status::Available } else { Status::Failed(e) };
                    this.update(cx, |this, cx| this.set_status(status, cx)).ok();
                }
            }
        })
        .detach();
    }

    /// 中止正在进行的下载; 下载线程读到下一块时停下, 状态回到"有新版本".
    pub fn cancel(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    fn set_status(&mut self, status: Status, cx: &mut Context<Self>) {
        self.status = status;
        cx.notify();
    }

    /// 下载期间定时把下载线程的进度抄进状态, 界面跟着重画; 不再是下载中就停.
    fn refresh_progress(&self, progress: Arc<Progress>, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(PROGRESS_REFRESH).await;
                let downloading = this.update(cx, |this, cx| {
                    let Status::Downloading { received, total } = &mut this.status else {
                        return false;
                    };
                    *received = progress.received.load(Ordering::Relaxed);
                    *total = Some(progress.total.load(Ordering::Relaxed)).filter(|&t| t > 0);
                    cx.notify();
                    true
                });
                if !matches!(downloading, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
    }
}

/// 依次向各发行版地址请求 path, 返回第一个成功的地址和内容; 全部失败时汇总各自的原因.
/// progress 中止时直接返回取消, 不再试下一个地址.
fn fetch(path: &str, mut progress: impl FnMut(u64, Option<u64>) -> bool) -> Result<(String, Vec<u8>), String> {
    let mut errors = Vec::new();
    for source in &PARAMS.update.sources {
        match http::download(&format!("{source}/{path}"), PARAMS.update.timeout_secs, &mut progress) {
            Ok(body) => return Ok((source.clone(), body)),
            Err(e) if e == http::CANCELED => return Err(e),
            Err(e) => errors.push(format!("{source}: {e}")),
        }
    }
    Err(errors.join("\n"))
}

fn latest() -> Result<(String, Release), String> {
    let (source, body) = fetch("latest/download/latest.json", |_, _| true)?;
    let release: Release = serde_json::from_slice(&body).map_err(|e| format!("latest.json 格式有误: {e}"))?;
    if release.version.split('.').any(|n| n.parse::<u64>().is_err()) {
        return Err(format!("latest.json 的版本号有误: {}", release.version));
    }
    Ok((source, release))
}

fn download(release: &Release, progress: &Progress, cancel: &AtomicBool) -> Result<Vec<u8>, String> {
    let path = format!("download/v{}/{}", release.version, release.setup.name);
    let (_, body) = fetch(&path, |received, total| {
        progress.received.store(received, Ordering::Relaxed);
        progress.total.store(total.unwrap_or(0), Ordering::Relaxed);
        !cancel.load(Ordering::Relaxed)
    })?;
    Ok(body)
}

/// 摘要和 latest.json 对不上就不用它; 对得上存到临时目录.
fn save(setup: &Asset, body: Vec<u8>) -> Result<PathBuf, String> {
    let digest = format!("{:x}", Sha256::digest(&body));
    if digest != setup.sha256 {
        return Err(format!("安装包校验失败: 应为 {}, 实际 {digest}", setup.sha256));
    }
    let dir = std::env::temp_dir().join("qrun-update");
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建临时目录失败: {e}"))?;
    let path = dir.join(&setup.name);
    std::fs::write(&path, body).map_err(|e| format!("保存安装包失败: {e}"))?;
    Ok(path)
}

/// x.y.z 拆成数字, 按段比较大小. 远端的版本号已在 latest() 里检查过.
fn version(text: &str) -> Vec<u64> {
    text.split('.').map(|n| n.parse().expect("x.y.z version")).collect()
}
