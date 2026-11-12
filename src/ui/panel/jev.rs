//! 打开面板时记一次打开, 以及 Jev 的调用时机: 前台窗口或它的标题停稳 prefetch_delay_ms 后预取, 都没变时每 refresh_minutes
//! 刷新; 两次预取至少隔 prefetch_min_interval_seconds, 人离开 (idle_minutes 没有输入) 时不预取.
//! 打开面板时预取结果是同一窗口同一标题且不过旧就直接用, 否则补调. 补调结果在打开后 reorder_window_ms 内且鼠标还没进推荐页
//! 才用上, 晚到的这次打开不用, 排序和学习都按本地分, 同一情境不会时而按 Jev 时而不按. 搜索框停止输入 search_debounce_ms 后按搜索词调一次.
//! 每次调用的结果记进 Library, 设置页显示最近一次.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui_kit::*;
use windows::Win32::Foundation::HWND;

use super::Panel;
use crate::model::jev;
use crate::model::library::{self, NEW_FILE, Observed, Situation};
use crate::model::redact::redact;
use crate::params::PARAMS;
use crate::platform::{self, context, http, recent};

/// Jev 给每个候选 (比较键) 的概率.
type Probabilities = HashMap<String, f64>;

/// 前台窗口和它当时的标题; 浏览器换标签页, 编辑器换文件时窗口不变, 标题变了也算换了情境.
#[derive(Clone, PartialEq)]
struct Watched {
    hwnd: HWND,
    title: String,
}

struct Prefetched {
    watched: Watched,
    /// 结果到的时间.
    at: Instant,
    /// 失败为 None, 到刷新时间再试.
    probabilities: Option<Probabilities>,
}

pub(super) struct JevState {
    /// 最近一个不是 QRun 的前台窗口和它 (连同标题) 从什么时候起没变; 打开 QRun 时它就是打开前在用的窗口.
    external: Option<(Watched, Instant)>,
    prefetched: Option<Prefetched>,
    /// 正在预取, 完成前不再发.
    prefetch: Option<Task<()>>,
    /// 补调这次打开的结果; 换成新任务时旧的随之取消.
    open_fetch: Task<()>,
    opened_at: Option<Instant>,
    /// 上一次打开 QRun 时在用的窗口.
    opened_on: Option<Watched>,
    /// 这次打开后鼠标进过推荐页, 之后到的 Jev 结果不再重排.
    pub(super) smart_entered: bool,
    /// 上一轮面板是否摆在屏幕上.
    presented: bool,
    /// 搜索词和 Jev 对它的结果.
    search: Option<(String, Probabilities)>,
    search_fetch: Task<()>,
}

impl JevState {
    pub(super) fn new() -> Self {
        Self {
            external: None,
            prefetched: None,
            prefetch: None,
            open_fetch: Task::ready(()),
            opened_at: None,
            opened_on: None,
            smart_entered: false,
            presented: false,
            search: None,
            search_fetch: Task::ready(()),
        }
    }
}

fn minutes(n: u64) -> Duration {
    Duration::from_secs(n * 60)
}

impl Panel {
    /// app::poll 每轮调用. presented 是面板现在是否摆在屏幕上 (显示着且没缩在边外), 从没摆着变成摆着算打开一次.
    pub fn tick(&mut self, presented: bool, cx: &mut Context<Self>) {
        if presented && !self.jev.presented {
            self.opened(cx);
        }
        self.jev.presented = presented;
        self.watch_foreground(cx);
        if !cx.has_active_drag() {
            self.drag_out = None;
        }
    }

    /// 面板打开了 (激活或滑出): 记一次打开, 排好"猜你要打开", 重新读系统最近记录.
    /// 贴边滑出和激活窗口常紧挨着发生, 相隔不到 open_merge_seconds 算同一次.
    /// 刚启动系统最近记录还没读完时先不记, 读完再记, 候选里才有系统记录. 关了推荐时什么都不记.
    pub(super) fn opened(&mut self, cx: &mut Context<Self>) {
        let merge = Duration::from_secs(PARAMS.learn.open_merge_seconds);
        if !self.settings.read(cx).smart.enabled
            || self.recent.is_none()
            || self.jev.opened_at.is_some_and(|at| at.elapsed() < merge)
        {
            return;
        }
        self.jev.opened_at = Some(Instant::now());
        self.jev.smart_entered = false;
        let appeared = self.scan_new_files(cx);
        self.refresh_recent(cx);

        let external = self.jev.external.as_ref().map(|(watched, _)| watched.clone());
        // 情境没变时接着用上次的顺序, 不重算也不问 Jev, 免得每次划出来都换位
        let keep_order = !appeared
            && external.is_some()
            && self.jev.opened_on == external
            && self.library.read(cx).order_still_fresh();
        self.jev.opened_on = external.clone();
        let fresh = self
            .jev
            .prefetched
            .as_ref()
            .filter(|p| Some(&p.watched) == external.as_ref() && p.at.elapsed() < minutes(PARAMS.jev.stale_minutes))
            .and_then(|p| p.probabilities.clone());
        let situation = self.situation(cx);
        let shown = self.settings.read(cx).smart.predict as usize;
        self.library.update(cx, |lib, cx| {
            lib.begin_open(situation, shown, fresh.as_ref().filter(|_| !keep_order), keep_order, cx)
        });
        if keep_order || fresh.is_some() || !self.settings.read(cx).smart.jev_on() {
            return;
        }
        let library = self.library.read(cx);
        let Some((open_id, request)) = library.open_id().zip(library.open_jev_request(None)) else {
            return;
        };
        let ask = self.ask_jev(request, cx);
        self.jev.open_fetch = cx.spawn(async move |this, cx| {
            let result = ask.await;
            this.update(cx, |this, cx| {
                let Some(probabilities) = this.jev_answered(result, cx) else {
                    return;
                };
                let window = Duration::from_millis(PARAMS.jev.reorder_window_ms);
                if !this.jev.smart_entered && this.jev.opened_at.is_some_and(|at| at.elapsed() < window) {
                    this.library.update(cx, |lib, cx| lib.apply_jev(open_id, &probabilities, cx));
                }
                if let Some(watched) = external {
                    this.jev.prefetched =
                        Some(Prefetched { watched, at: Instant::now(), probabilities: Some(probabilities) });
                }
            })
            .ok();
        });
    }

    /// 跟踪前台窗口和标题 (QRun 自己的窗口不算), 停稳后预取, 都没变时到点刷新.
    fn watch_foreground(&mut self, cx: &mut Context<Self>) {
        let Some(hwnd) = context::foreground() else {
            return;
        };
        if context::process_id(hwnd) == std::process::id() {
            return;
        }
        let watched = Watched { hwnd, title: context::title(hwnd) };
        let since = match &self.jev.external {
            Some((external, since)) if *external == watched => *since,
            _ => {
                let now = Instant::now();
                self.jev.external = Some((watched.clone(), now));
                now
            }
        };
        if self.jev.prefetch.is_some() || !self.settings.read(cx).smart.jev_on() {
            return;
        }
        let stable = since.elapsed() >= Duration::from_millis(PARAMS.jev.prefetch_delay_ms);
        let away = context::idle() >= minutes(PARAMS.jev.idle_minutes);
        let due = self.jev.prefetched.as_ref().is_none_or(|p| {
            let spaced = p.at.elapsed() >= Duration::from_secs(PARAMS.jev.prefetch_min_interval_seconds);
            spaced && (p.watched != watched || p.at.elapsed() >= minutes(PARAMS.jev.refresh_minutes))
        });
        if !stable || away || !due {
            return;
        }
        self.scan_new_files(cx);
        let request = self.library.read(cx).prefetch_jev_request(&self.situation(cx));
        let ask = self.ask_jev(request, cx);
        self.jev.prefetch = Some(cx.spawn(async move |this, cx| {
            let result = ask.await;
            this.update(cx, |this, cx| {
                let probabilities = this.jev_answered(result, cx);
                this.jev.prefetch = None;
                this.jev.prefetched = Some(Prefetched { watched, at: Instant::now(), probabilities });
            })
            .ok();
        }));
    }

    /// 扫一遍下载和桌面 (系统最近记录在后台读, 赶不上这次打开), 返回有没有新出现的文件.
    /// 有的话之前问 Jev 的结果里没有它, 按概率 0 算会被埋掉, 作废重问.
    fn scan_new_files(&mut self, cx: &mut Context<Self>) -> bool {
        let found = match self.settings.read(cx).smart.collect.new_files {
            true => recent::new_files(super::new_file_since()),
            false => Vec::new(),
        };
        let found =
            found.into_iter().map(|e| Observed { name: e.name, target: e.target, at: e.at, kind: NEW_FILE }).collect();
        let appeared = self.library.update(cx, |lib, _| lib.sync_new_files(found));
        if appeared {
            self.jev.prefetch = None;
            self.jev.prefetched = None;
        }
        appeared
    }

    /// 搜索词变了: 停手 search_debounce_ms 后让 Jev 按搜索词排一次.
    pub(super) fn search_changed(&mut self, cx: &mut Context<Self>) {
        let query = self.query(cx);
        if query.is_empty() || !self.settings.read(cx).smart.jev_on() {
            self.jev.search_fetch = Task::ready(());
            return;
        }
        self.jev.search_fetch = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(PARAMS.jev.search_debounce_ms)).await;
            let Ok(Some(ask)) = this.update(cx, |this, cx| {
                let request = this.library.read(cx).open_jev_request(Some(&query))?;
                Some(this.ask_jev(request, cx))
            }) else {
                return;
            };
            let result = ask.await;
            this.update(cx, |this, cx| {
                if let Some(probabilities) = this.jev_answered(result, cx) {
                    this.jev.search = Some((query, probabilities));
                    this.refresh_search(cx);
                }
            })
            .ok();
        });
    }

    /// Jev 对这个搜索词的结果; 还没到或是别的搜索词的为 None.
    pub(super) fn search_probabilities(&self, query: &str) -> Option<&Probabilities> {
        self.jev.search.as_ref().filter(|(q, _)| q == query).map(|(_, probabilities)| probabilities)
    }

    /// 打开 QRun 前在用的窗口, 剪贴板类型, 开着的窗口; 前台窗口已关掉时它的几项为空. 标题已脱敏.
    /// 收集里关掉了窗口时窗口几项都为空.
    fn situation(&self, cx: &App) -> Situation {
        if !self.settings.read(cx).smart.collect.window {
            return Situation { clipboard: context::clipboard_kind().map(str::to_string), ..Situation::default() };
        }
        let window = self.jev.external.as_ref().map(|(watched, _)| context::window_info(watched.hwnd));
        Situation {
            foreground: window.as_ref().and_then(|w| w.program.clone()),
            title: window.map(|w| redact(&w.title).text).filter(|title| !title.is_empty()),
            clipboard: context::clipboard_kind().map(str::to_string),
            windows: context::windows()
                .into_iter()
                .map(|w| library::Window { program: w.program, title: redact(&w.title).text })
                .collect(),
        }
    }

    /// 后台组请求并调 Jev.
    fn ask_jev(&self, draft: jev::Draft, cx: &App) -> Task<Result<Probabilities, String>> {
        let smart = &self.settings.read(cx).smart;
        let (url, key) = (smart.jev_url(), smart.jev_key.clone());
        cx.background_executor().spawn(async move {
            let request = jev::request(draft);
            http::post_json(&url, &key, &request.body, PARAMS.jev.timeout_secs)
                .and_then(|body| jev::parse(&body, &request.options))
        })
    }

    /// 记下 Jev 这次的结果 (设置页显示, 失败写日志), 成功时返回概率.
    fn jev_answered(&self, result: Result<Probabilities, String>, cx: &mut Context<Self>) -> Option<Probabilities> {
        let status = result.as_ref().map(|_| ()).map_err(String::clone);
        self.library.update(cx, |lib, cx| lib.record_jev(platform::local_time(), status, cx));
        result.ok()
    }
}
