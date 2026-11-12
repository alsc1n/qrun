//! 程序本身的统计, 只从启动记录学: 记录越多越贴近个人习惯. 是推荐打分 (`model::brain`) 的前四项.
//!
//! 候选 i 在当前时刻的四项 (朴素贝叶斯 + 前后衔接):
//!   ln 常用度(i), ln P(钟点 | i), ln P(工作日/周末 | i), ln [P(i | 上一个) / P(i)]
//! - 常用度: 每条记录按 2^(-距今 / 半衰期) 衰减后计数, 不用的东西自然往下掉.
//! - 钟点: 一次启动按正态核摊到前后钟点上, 8:55 和 9:05 算同一个习惯.
//! - 前后衔接: 上一次启动在 session_gap 内时, 统计"上一个之后接着开 i"; 超过就算"刚开始干活".
//! - 钟点, 工作日/周末, 前后衔接都向整体分布平滑, 记录少时不会因一两次巧合排到前面.
//!
//! 半衰期和核宽不写死: `tune` 回放最近的历史, 每次只用之前的记录预测, 选命中率最高的一组.

use std::collections::HashMap;
use std::path::Path;

use crate::params::PARAMS;

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
#[derive(Clone)]
pub struct Launch {
    pub key: String,
    pub at: i64,
    pub week_minute: u16,
}

/// 同一目标在不同来源里大小写可能不同, 统一成小写比较.
pub fn key(target: &Path) -> String {
    target.to_string_lossy().to_lowercase()
}

/// 距上次打开分几段: 刚用完又呼出多半是接着干同一件事, 隔了很久多半是新的开始.
pub fn gap_bucket(minutes: i64) -> &'static str {
    match minutes {
        ..2 => "under 2 minutes",
        2..30 => "2 to 30 minutes",
        30..240 => "30 minutes to 4 hours",
        _ => "over 4 hours",
    }
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
#[derive(Clone)]
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
    last: Option<(usize, i64)>,
}

impl Model {
    pub fn new(tuning: Tuning) -> Self {
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
        let prev = self.prev(launch.at);
        *self.transitions.entry((prev, ix)).or_default() += weight;
        *self.followed.entry(prev).or_default() += weight;
        self.last = Some((ix, launch.at));
    }

    /// 各候选的四项和它在所有打开里的占比 P(i).
    pub fn base_terms<'a>(
        &self,
        keys: impl IntoIterator<Item = &'a str>,
        now: i64,
        week_minute: u16,
    ) -> Vec<([f64; 4], f64)> {
        let context = self.context(now, week_minute);
        keys.into_iter().map(|key| self.terms(self.index.get(key).copied(), &context)).collect()
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

    fn rebase(&mut self, base: i64) {
        let factor = (-((base - self.base) as f64) / self.half_life_ms).exp2();
        for stats in self.targets.iter_mut().chain(std::iter::once(&mut self.all)) {
            stats.count *= factor;
            stats.days.iter_mut().chain(stats.hours.iter_mut()).for_each(|v| *v *= factor);
        }
        self.transitions.values_mut().chain(self.followed.values_mut()).for_each(|v| *v *= factor);
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
    fn terms(&self, ix: Option<usize>, context: &Context) -> ([f64; 4], f64) {
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
        self.terms(ix, context).0.iter().sum()
    }
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
