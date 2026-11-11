//! 预测下一个要打开的东西, 只从启动记录学: 记录越多越贴近个人习惯.
//!
//! 候选 i 在当前情况下的分数 (朴素贝叶斯 + 前后衔接):
//!   ln 常用度(i) + ln P(钟点 | i) + ln P(工作日/周末 | i) + ln [P(i | 上一个) / P(i)]
//! - 常用度: 每条记录按 2^(-距今 / 半衰期) 衰减后计数, 不用的东西自然往下掉.
//! - 钟点: 一次启动按正态核摊到前后钟点上, 8:55 和 9:05 算同一个习惯.
//! - 前后衔接: 上一次启动在 session_gap 内时, 统计"上一个之后接着开 i"; 超过就算"刚开始干活".
//! - 钟点, 工作日/周末, 前后衔接都向整体分布平滑, 记录少时不会因一两次巧合排到前面.
//!
//! 半衰期和核宽不写死: `tune` 回放最近的历史, 每次只用之前的记录预测, 选命中率最高的一组.
//!
//! 上面四项之外还有打开 QRun 那一刻的情境 (前台程序, 距上次打开多久, 剪贴板类型, 同样按 lift 算), 以及最近被跳过几次,
//! 名称是否出现在前台窗口标题里, 是否正在运行, 是不是刚出现的新文件, 活动时间线里的近况 (刚被改过, 在前台停了多久,
//! 刚启动, 和最近活动的路径有多近, 出现最近这些事之后常开它的程度), 常开的文件类型, Jev 给的概率. 各项合成一组特征,
//! 最终分数是特征的加权和; 权重 (`Weights`) 从每次打开时的对错里学: 被点的是对, 摆在它前面却没被点的是错.

use std::collections::HashMap;
use std::path::Path;

use crate::params::PARAMS;

/// 特征名, 与 `Features` 的下标一一对应; 也是库里 weights 表的键.
pub const FEATURES: [&str; 18] = [
    "frequency",
    "hour",
    "day",
    "after_previous",
    "foreground",
    "gap",
    "clipboard",
    "skipped",
    "in_title",
    "running",
    "fresh",
    "modified",
    "focused",
    "started",
    "nearby",
    "after_events",
    "extension",
    "jev",
];
pub const SKIPPED: usize = 7;
pub const IN_TITLE: usize = 8;
pub const RUNNING: usize = 9;
const FRESH: usize = 10;
const MODIFIED: usize = 11;
const FOCUSED: usize = 12;
const STARTED: usize = 13;
const NEARBY: usize = 14;
const AFTER_EVENTS: usize = 15;
const EXTENSION: usize = 16;
const JEV: usize = 17;
/// 前四项是朴素贝叶斯的四项, 初始权重为 1, 起步就是只按记录打分.
const BASE_TERMS: usize = 4;

pub type Features = [f64; FEATURES.len()];

/// 打开 QRun 那一刻能从历史里学"这种情况下常开什么"的情境, 拿不到的为 None.
#[derive(Clone, Default)]
pub struct Facets {
    /// 打开 QRun 前的前台程序的比较键.
    pub foreground: Option<String>,
    /// 距上次打开 QRun 的区间, 见 `gap_bucket`.
    pub gap: Option<String>,
    /// 剪贴板类型.
    pub clipboard: Option<String>,
}

impl Facets {
    fn values(&self) -> [Option<&String>; 3] {
        [self.foreground.as_ref(), self.gap.as_ref(), self.clipboard.as_ref()]
    }
}

/// 距上次打开分几段学: 刚用完又呼出多半是接着干同一件事, 隔了很久多半是新的开始.
pub fn gap_bucket(minutes: i64) -> String {
    match minutes {
        ..2 => "under 2 minutes",
        2..30 => "2 to 30 minutes",
        30..240 => "30 minutes to 4 hours",
        _ => "over 4 hours",
    }
    .into()
}

/// 候选在这次打开时本地统计以外的信号.
#[derive(Default)]
pub struct Signals {
    /// 最近被摆出来却没被点的次数 (按天衰减).
    pub skipped: f64,
    pub in_title: bool,
    pub running: bool,
    /// 下载文件夹或桌面上的新文件出现了多少小时.
    pub fresh_hours: Option<f64>,
    /// 以下来自活动时间线 (`library::timeline`), 都按距今衰减: 自己或里面的文件最近一次被改有多近 (0 到 1),
    /// 在前台停了多少分钟, 最近一次启动有多近 (0 到 1), 和最近活动过的路径有多近 (0 到 1),
    /// 出现最近这些事之后打开它的比例 (0 到 1).
    pub modified: f64,
    pub focused_minutes: f64,
    pub started: f64,
    pub nearby: f64,
    pub after_events: f64,
}

const MINUTE: i64 = 60_000;
const DAY: i64 = 86_400_000;
const MINUTES_PER_DAY: u16 = 1440;
/// 一周里周六 0 点的分钟数 (周一 0 点为 0), 之后是周末.
const WEEKEND_START: u16 = 5 * MINUTES_PER_DAY;
/// 工作日和周末天数的占比, 是没有记录时的先验.
const DAY_SHARE: [f64; 2] = [5. / 7., 2. / 7.];
/// 钟点核向两边各摊几小时.
const KERNEL_REACH: i32 = 2;
/// "刚开始干活"这个上一个状态, 不与任何候选的下标冲突.
const SESSION_START: usize = usize::MAX;
/// 记录权重放大存储的指数上限; 超过就整体换基准时间, f64 不会溢出.
const MAX_EXPONENT: f64 = 500.;

/// 一次启动. key 是目标的比较键 (见 `key`); week_minute 是启动时本地时间在一周里的分钟数, 周一 0 点为 0.
/// facets 是从 QRun 打开时那次打开的情境, 系统来源的启动没有.
#[derive(Clone)]
pub struct Launch {
    pub key: String,
    pub at: i64,
    pub week_minute: u16,
    pub facets: Option<Facets>,
}

/// 同一目标在不同来源里大小写可能不同, 统一成小写比较.
pub fn key(target: &Path) -> String {
    target.to_string_lossy().to_lowercase()
}

/// 比较键的扩展名 (已是小写); 文件夹和没有扩展名的为空.
fn extension(key: &str) -> String {
    Path::new(key).extension().map_or(String::new(), |ext| ext.to_string_lossy().into_owned())
}

#[derive(Clone, Copy)]
pub struct Tuning {
    pub half_life_days: f64,
    pub hour_sigma: f64,
}

impl Tuning {
    /// params.json 里两组候选两两组合.
    fn candidates() -> Vec<Self> {
        let predict = &PARAMS.predict;
        predict
            .half_life_days
            .iter()
            .flat_map(|&half_life_days| {
                predict.hour_sigma.iter().map(move |&hour_sigma| Self { half_life_days, hour_sigma })
            })
            .collect()
    }

    /// 还没回放过时先用第一组.
    pub fn initial() -> Self {
        Self::candidates()[0]
    }
}

#[derive(Default, Clone)]
struct Stats {
    count: f64,
    hours: [f64; 24],
    days: [f64; 2],
}

/// 从启动记录累积出的统计. 记录权重按 2^((at - base) / 半衰期) 放大后累加, 查询时统一乘回 2^(-(now - base) / 半衰期),
/// 这样新增一条记录不用把所有旧记录重新衰减一遍.
pub struct Model {
    tuning: Tuning,
    kernel: [f64; (2 * KERNEL_REACH + 1) as usize],
    half_life_ms: f64,
    base: i64,
    index: HashMap<String, usize>,
    targets: Vec<Stats>,
    all: Stats,
    /// (上一个, 接着开的) -> 次数; 上一个可以是 SESSION_START.
    transitions: HashMap<(usize, usize), f64>,
    /// 上一个 -> 它之后接着开的总次数.
    followed: HashMap<usize, f64>,
    /// (情境下标, 情境值) -> 这种情境下的总次数和各目标的次数; 下标按 `Facets::values` 的顺序.
    facets: HashMap<(usize, String), (f64, HashMap<usize, f64>)>,
    /// 各文件类型 (扩展名, 没有的为空) 被打开的次数; 没打开过的东西也能按常开的类型排.
    extensions: HashMap<String, f64>,
    last: Option<(usize, i64)>,
}

impl Model {
    fn new(tuning: Tuning) -> Self {
        let mut kernel = [0.; (2 * KERNEL_REACH + 1) as usize];
        for (ix, weight) in kernel.iter_mut().enumerate() {
            let offset = ix as f64 - KERNEL_REACH as f64;
            *weight = (-offset * offset / (2. * tuning.hour_sigma * tuning.hour_sigma)).exp();
        }
        let sum: f64 = kernel.iter().sum();
        kernel.iter_mut().for_each(|w| *w /= sum);
        Self {
            tuning,
            kernel,
            half_life_ms: tuning.half_life_days * DAY as f64,
            base: 0,
            index: HashMap::new(),
            targets: Vec::new(),
            all: Stats::default(),
            transitions: HashMap::new(),
            followed: HashMap::new(),
            facets: HashMap::new(),
            extensions: HashMap::new(),
            last: None,
        }
    }

    /// launches 要按时间从早到晚排好.
    pub fn build(launches: &[Launch], tuning: Tuning) -> Self {
        let mut model = Self::new(tuning);
        for launch in launches {
            model.add(launch);
        }
        model
    }

    pub fn tuning(&self) -> Tuning {
        self.tuning
    }

    /// 记一次启动; 时间不能早于已记的最后一次 (更早的记录要重新 build).
    pub fn add(&mut self, launch: &Launch) {
        if self.targets.is_empty() {
            self.base = launch.at;
        }
        let exponent = (launch.at - self.base) as f64 / self.half_life_ms;
        if exponent > MAX_EXPONENT {
            self.rebase(launch.at);
        }
        let weight = ((launch.at - self.base) as f64 / self.half_life_ms).exp2();
        let next = self.targets.len();
        let ix = *self.index.entry(launch.key.clone()).or_insert(next);
        if ix == next {
            self.targets.push(Stats::default());
        }
        let hour = (launch.week_minute % MINUTES_PER_DAY / 60) as i32;
        let day = (launch.week_minute >= WEEKEND_START) as usize;
        for stats in [&mut self.targets[ix], &mut self.all] {
            stats.count += weight;
            stats.days[day] += weight;
            for (offset, k) in (-KERNEL_REACH..=KERNEL_REACH).zip(self.kernel) {
                stats.hours[(hour + offset).rem_euclid(24) as usize] += weight * k;
            }
        }
        *self.extensions.entry(extension(&launch.key)).or_default() += weight;
        let prev = self.prev(launch.at);
        *self.transitions.entry((prev, ix)).or_default() += weight;
        *self.followed.entry(prev).or_default() += weight;
        if let Some(facets) = &launch.facets {
            for (facet, value) in facets.values().into_iter().enumerate() {
                if let Some(value) = value {
                    let (total, targets) = self.facets.entry((facet, value.clone())).or_default();
                    *total += weight;
                    *targets.entry(ix).or_default() += weight;
                }
            }
        }
        self.last = Some((ix, launch.at));
    }

    /// 各候选的特征; jev 一项为 0, 有 Jev 结果时由调用方用 `set_jev` 填上.
    pub fn features<'a>(
        &self,
        now: i64,
        week_minute: u16,
        facets: &Facets,
        candidates: impl IntoIterator<Item = (&'a str, &'a Signals)>,
    ) -> Vec<Features> {
        let context = self.context(now, week_minute);
        candidates
            .into_iter()
            .map(|(key, signals)| {
                let ix = self.index.get(key).copied();
                let mut x = [0.; FEATURES.len()];
                let (base, p_target) = self.base_terms(ix, &context);
                x[..BASE_TERMS].copy_from_slice(&base);
                for (facet, value) in facets.values().into_iter().enumerate() {
                    if let Some(value) = value {
                        x[BASE_TERMS + facet] = self.facet_lift(facet, value, ix, p_target, context.decay);
                    }
                }
                x[SKIPPED] = signals.skipped;
                x[IN_TITLE] = signals.in_title as u8 as f64;
                x[RUNNING] = signals.running as u8 as f64;
                // 新文件刚出现时最可能马上要开, 一天后影响减到约三分之一
                x[FRESH] = signals.fresh_hours.map_or(0., |hours| (-hours / 24.).exp());
                x[MODIFIED] = signals.modified;
                // 停了几秒和停了一小时差别大, 一小时和两小时差别小
                x[FOCUSED] = signals.focused_minutes.ln_1p();
                x[STARTED] = signals.started;
                x[NEARBY] = signals.nearby;
                x[AFTER_EVENTS] = signals.after_events;
                x[EXTENSION] = self.extension_share(key, &context);
                x
            })
            .collect()
    }

    /// 记录里这个目标最常被打开的几个钟点, 从多到少; 给 Jev 看的习惯摘要.
    pub fn usual_hours(&self, key: &str) -> Vec<usize> {
        let Some(&ix) = self.index.get(key) else {
            return Vec::new();
        };
        let hours = self.targets[ix].hours;
        let mut order: Vec<usize> = (0..24).collect();
        order.sort_by(|&a, &b| hours[b].total_cmp(&hours[a]));
        order.truncate(3);
        order
    }

    /// 在 prev 之后最常接着开的几个目标的比较键, 从多到少.
    pub fn usual_next(&self, now: i64, n: usize) -> Vec<&str> {
        let prev = self.prev(now);
        let mut next: Vec<(usize, f64)> =
            self.transitions.iter().filter(|((p, _), _)| *p == prev).map(|((_, ix), &v)| (*ix, v)).collect();
        next.sort_by(|a, b| b.1.total_cmp(&a.1));
        let keys: HashMap<usize, &str> = self.index.iter().map(|(k, &ix)| (ix, k.as_str())).collect();
        next.into_iter().take(n).map(|(ix, _)| keys[&ix]).collect()
    }

    /// ln 这类文件 (同扩展名) 占所有打开的比例, 向均匀分布平滑.
    fn extension_share(&self, key: &str, context: &Context) -> f64 {
        let smoothing = PARAMS.predict.smoothing;
        let count = self.extensions.get(&extension(key)).map_or(0., |v| v * context.decay);
        let total = self.all.count * context.decay;
        ((count + smoothing / (self.extensions.len() + 1) as f64) / (total + smoothing)).ln()
    }

    fn facet_lift(&self, facet: usize, value: &str, ix: Option<usize>, p_target: f64, decay: f64) -> f64 {
        let smoothing = PARAMS.predict.smoothing;
        let (total, count) = self.facets.get(&(facet, value.to_string())).map_or((0., 0.), |(total, targets)| {
            (*total * decay, ix.and_then(|ix| targets.get(&ix)).map_or(0., |v| v * decay))
        });
        let p = (count + smoothing * p_target) / (total + smoothing);
        (p / p_target).ln()
    }

    fn rebase(&mut self, base: i64) {
        let factor = (-((base - self.base) as f64) / self.half_life_ms).exp2();
        for stats in self.targets.iter_mut().chain(std::iter::once(&mut self.all)) {
            stats.count *= factor;
            stats.days.iter_mut().chain(stats.hours.iter_mut()).for_each(|v| *v *= factor);
        }
        self.transitions
            .values_mut()
            .chain(self.followed.values_mut())
            .chain(self.extensions.values_mut())
            .for_each(|v| *v *= factor);
        for (total, targets) in self.facets.values_mut() {
            *total *= factor;
            targets.values_mut().for_each(|v| *v *= factor);
        }
        self.base = base;
    }

    fn prev(&self, at: i64) -> usize {
        match self.last {
            Some((ix, last)) if at - last <= PARAMS.predict.session_gap_minutes * MINUTE => ix,
            _ => SESSION_START,
        }
    }

    /// 与候选无关, 每次打分只算一遍的量.
    fn context(&self, now: i64, week_minute: u16) -> Context {
        let decay = (-((now - self.base) as f64) / self.half_life_ms).exp2();
        let total = self.all.count * decay;
        let hour = (week_minute % MINUTES_PER_DAY / 60) as usize;
        let day = (week_minute >= WEEKEND_START) as usize;
        let prev = self.prev(now);
        Context {
            decay,
            hour,
            day,
            prev,
            // 整体分布再向均匀分布平滑一次, 刚开始没什么记录时也有值
            all_hour: (self.all.hours[hour] * decay + 1. / 24.) / (total + 1.),
            all_day: (self.all.days[day] * decay + DAY_SHARE[day]) / (total + 1.),
            prior_total: total + PARAMS.predict.prior_count * (self.targets.len() + 1) as f64,
            followed: self.followed.get(&prev).copied().unwrap_or(0.) * decay,
        }
    }

    /// 朴素贝叶斯的四项和候选的整体占比 P(i).
    fn base_terms(&self, ix: Option<usize>, context: &Context) -> ([f64; BASE_TERMS], f64) {
        let smoothing = PARAMS.predict.smoothing;
        let stats = ix.map(|ix| &self.targets[ix]);
        let count = stats.map_or(0., |s| s.count * context.decay);
        let in_hour = stats.map_or(0., |s| s.hours[context.hour] * context.decay);
        let in_day = stats.map_or(0., |s| s.days[context.day] * context.decay);
        let after_prev = ix.and_then(|ix| self.transitions.get(&(context.prev, ix))).map_or(0., |v| v * context.decay);

        let frequency = count + PARAMS.predict.prior_count;
        let p_hour = (in_hour + smoothing * context.all_hour) / (count + smoothing);
        let p_day = (in_day + smoothing * context.all_day) / (count + smoothing);
        let p_target = frequency / context.prior_total;
        let p_after = (after_prev + smoothing * p_target) / (context.followed + smoothing);
        ([frequency.ln(), p_hour.ln(), p_day.ln(), (p_after / p_target).ln()], p_target)
    }

    /// 回放调参只比较记录统计本身, 四项等权相加.
    fn score(&self, ix: Option<usize>, context: &Context) -> f64 {
        self.base_terms(ix, context).0.iter().sum()
    }
}

/// Jev 给的概率填进特征: ln(p + jev_smoothing). 加上平滑, Jev 给 0 的候选扣分有限, 本地记录很强时仍能排上来,
/// 不是一票否决.
pub fn set_jev(x: &mut Features, probability: f64) {
    x[JEV] = (probability + PARAMS.learn.jev_smoothing).ln();
}

/// 各特征的权重, 一套权重同时用于开和不开 Jev: 没有 Jev 结果时 jev 一项不参与打分, 也不参与这次学习, 权重原样保留.
/// grad_sq 是各特征历次梯度的平方和 (AdaGrad), 学得越多步子越小, 量级大的特征也不会被拽得更多.
#[derive(Clone)]
pub struct Weights {
    pub value: Features,
    pub grad_sq: Features,
}

impl Weights {
    /// 起步以记录打分为主 (前四项为 1), Jev 一项起步就高; params.json 的 initial 里给了的按它, 其余从 0 开始学.
    /// 学习时也往这里回拉.
    pub fn initial() -> Self {
        Self { value: initial_values(), grad_sq: [0.; FEATURES.len()] }
    }

    pub fn score(&self, x: &Features, with_jev: bool) -> f64 {
        self.value.iter().zip(x).enumerate().filter(|(ix, _)| with_jev || *ix != JEV).map(|(_, (w, v))| w * v).sum()
    }

    /// 一次打开的反馈: rows 是这次参与比较的候选, chosen 是被点的下标, shares 是各行算作反例的分量
    /// (被点的那行为 1; 离被点的越远, 越可能没被看到, 分量越小).
    /// 按加权 softmax 交叉熵走一步 (AdaGrad 步长), 往起步值回拉的 l2 也算在这一步的梯度里: 样本少时不被个别点击带偏.
    /// 回拉若在步长之外每次按固定比例拉, 步长随学习次数变小后回拉占上风, 学到的会一点点退回起步值;
    /// 放进梯度后两者一起缩小, 权重停在数据支持的位置, 越用越准. Jev 一项不低于 jev_floor, 它始终是比重高的一环.
    pub fn learn(&mut self, rows: &[Features], shares: &[f64], chosen: usize, with_jev: bool) {
        let scores: Vec<f64> = rows.iter().map(|x| self.score(x, with_jev)).collect();
        let max = scores.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let exp: Vec<f64> = scores.iter().zip(shares).map(|(s, share)| share * (s - max).exp()).collect();
        let sum: f64 = exp.iter().sum();
        let initial = initial_values();
        let learn = &PARAMS.learn;
        for feature in 0..FEATURES.len() {
            if feature == JEV && !with_jev {
                continue;
            }
            let expected: f64 = rows.iter().zip(&exp).map(|(x, e)| x[feature] * e / sum).sum();
            let gradient = rows[chosen][feature] - expected - learn.l2 * (self.value[feature] - initial[feature]);
            self.grad_sq[feature] += gradient * gradient;
            if self.grad_sq[feature] > 0. {
                self.value[feature] += learn.rate * gradient / self.grad_sq[feature].sqrt();
            }
        }
        self.value[JEV] = self.value[JEV].max(learn.jev_floor);
        // SKIPPED 之前都是"记录显示现在该开它"的证据, 被点的目标常因记录少而这几项偏低, 权重可以压到不看, 但变负就成了越匹配越靠后
        for weight in &mut self.value[..SKIPPED] {
            *weight = weight.max(learn.evidence_floor);
        }
    }
}

fn initial_values() -> Features {
    let mut w = [0.; FEATURES.len()];
    w[..BASE_TERMS].fill(1.);
    w[JEV] = PARAMS.learn.jev_initial;
    for (name, &value) in &PARAMS.learn.initial {
        let ix = FEATURES.iter().position(|f| f == name).expect("params.json learn.initial 里是已知特征");
        w[ix] = value;
    }
    w
}

struct Context {
    decay: f64,
    hour: usize,
    day: usize,
    prev: usize,
    all_hour: f64,
    all_day: f64,
    prior_total: f64,
    followed: f64,
}

/// 每组候选参数各回放一次, 返回命中率最高的一组; 命中率相同时取靠前的, 最近没有可回放的启动时就是第一组.
/// launches 按时间从早到晚排好. 在后台线程调用, 记录多时要算一会儿.
pub fn tune(launches: &[Launch], now: i64) -> Tuning {
    let mut best = (Tuning::initial(), -1.);
    for tuning in Tuning::candidates() {
        let hit_rate = replay(launches, tuning, now).unwrap_or(0.);
        if hit_rate > best.1 {
            best = (tuning, hit_rate);
        }
    }
    best.0
}

/// 回放最近 replay_days 天的每次启动: 只用它之前的记录, 在当时见过的所有目标里排名, 进前 replay_top_k 算命中.
/// 第一次出现的目标没法预测, 算没命中.
fn replay(launches: &[Launch], tuning: Tuning, now: i64) -> Option<f64> {
    let start = now - PARAMS.predict.replay_days * DAY;
    let top_k = PARAMS.predict.replay_top_k;
    let mut model = Model::new(tuning);
    let (mut hits, mut tests) = (0usize, 0usize);
    for launch in launches {
        if launch.at >= start {
            tests += 1;
            if let Some(&ix) = model.index.get(&launch.key) {
                let context = model.context(launch.at, launch.week_minute);
                let score = model.score(Some(ix), &context);
                let better =
                    (0..model.targets.len()).filter(|&j| j != ix && model.score(Some(j), &context) > score).count();
                if better < top_k {
                    hits += 1;
                }
            }
        }
        model.add(launch);
    }
    (tests > 0).then(|| hits as f64 / tests as f64)
}
