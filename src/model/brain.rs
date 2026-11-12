//! 推荐的打分和学习: 把一次打开 QRun 时的情况拆成标签, 按 "标签 → 打开了什么" 记账.
//!
//! - 情境标签 (整次打开一组, 如 `clip:开头:alter`, `前台:cindy`): 记 "出现这个标签时开了什么",
//!   打分时看 "出现这个标签时开它比平时高几倍".
//! - 状态标签 (每个候选一组, 如 `自己:运行中`): 按候选分开记 "处于这个状态时被摆出几次, 被开几次",
//!   同一状态对不同程序可以一个加分一个扣分 (开着也常点的程序, 运行中对它是加分). 某程序数据少时退回所有程序的平均.
//! - 次数少时向 "和平时一样" 平滑, 见过一两次几乎不动, 多次才明显起作用; 旧账按半衰期变淡.
//!
//! 最终分数是几项的加权和: 程序本身的四项 (`predict::Model`), 情境, 状态, 最近被跳过, Jev. 权重从每次点击的对错里学.
//! 这里不碰库, 时钟和系统: 线上和回放调同一套函数, 回放量出的就是线上的效果.

use std::collections::HashMap;

use crate::model::predict::Model;
use crate::params::PARAMS;

const DAY: f64 = 86_400_000.;
/// 每个候选都带的状态, 用来算 "平时被摆出来时开它的比例".
const CANDIDATE: &str = "自己:候选";

/// 特征名, 与 `Features` 的下标一一对应; 也是库里 weights 表的键.
pub const FEATURES: [&str; 8] = ["frequency", "hour", "day", "after_previous", "context", "state", "skipped", "jev"];
const CONTEXT: usize = 4;
const STATE: usize = 5;
pub const SKIPPED: usize = 6;
const JEV: usize = 7;
/// 前四项是程序本身的统计 (`Model::base_terms`).
const BASE_TERMS: usize = 4;

pub type Features = [f64; FEATURES.len()];

/// 一次打开时的一个候选: 比较键, 此刻所处的状态, 最近被跳过的次数 (按天衰减).
pub struct Candidate {
    pub key: String,
    pub states: Vec<&'static str>,
    pub skipped: f64,
}

/// 一次打开 QRun 时的一切, 已脱敏. tags 是情境标签.
pub struct Snapshot {
    pub at: i64,
    pub week_minute: u16,
    pub tags: Vec<String>,
    pub candidates: Vec<Candidate>,
}

/// 各候选的特征; Jev 一项为 0, 有 Jev 结果时用 `set_jev` 填上.
pub fn features(model: &Model, ledger: &Ledger, snapshot: &Snapshot) -> Vec<Features> {
    let base = model.base_terms(snapshot.candidates.iter().map(|c| c.key.as_str()), snapshot.at, snapshot.week_minute);
    snapshot
        .candidates
        .iter()
        .zip(base)
        .map(|(c, (terms, p_target))| {
            let mut x = [0.; FEATURES.len()];
            x[..BASE_TERMS].copy_from_slice(&terms);
            x[CONTEXT] = snapshot.tags.iter().map(|tag| ledger.context_lift(tag, &c.key, p_target, snapshot.at)).sum();
            x[STATE] = ledger.state_lift(&c.states, &c.key, snapshot.at);
            x[SKIPPED] = c.skipped;
            x
        })
        .collect()
}

/// Jev 给的概率填进特征: ln(p + jev_smoothing). 加上平滑, Jev 给 0 的候选扣分有限, 本地记录很强时仍能排上来.
pub fn set_jev(x: &mut Features, probability: f64) {
    x[JEV] = (probability + PARAMS.learn.jev_smoothing).ln();
}

/// 按半衰期衰减的次数: 存次数和记下它的时间, 用时换算到那一刻.
#[derive(Clone, Copy)]
struct Decayed {
    count: f64,
    at: i64,
}

impl Decayed {
    const ZERO: Self = Self { count: 0., at: 0 };

    fn value(self, now: i64) -> f64 {
        self.count * (-((now - self.at) as f64) / (PARAMS.ledger.half_life_days * DAY)).exp2()
    }

    fn add(&mut self, weight: f64, now: i64) {
        *self = Self { count: self.value(now) + weight, at: now };
    }
}

/// 一个标签 (或状态) 的总次数和各目标的次数.
#[derive(Clone, Default)]
struct Counts {
    total: Option<Decayed>,
    targets: HashMap<String, Decayed>,
}

impl Counts {
    fn total(&self, now: i64) -> f64 {
        self.total.map_or(0., |d| d.value(now))
    }

    fn of(&self, key: &str, now: i64) -> f64 {
        self.targets.get(key).map_or(0., |d| d.value(now))
    }

    fn add(&mut self, key: &str, weight: f64, now: i64) {
        self.total.get_or_insert(Decayed::ZERO).add(weight, now);
        self.targets.entry(key.to_string()).or_insert(Decayed::ZERO).add(weight, now);
    }
}

/// 标签账. 全部在内存里, 由回放从库里的历史建出来 (启动时和定时), 之后每次打开实时加.
#[derive(Clone, Default)]
pub struct Ledger {
    /// 情境标签 -> 出现它的打开里开了什么.
    context: HashMap<String, Counts>,
    /// 状态 -> 处于这个状态的候选被摆出的次数, 以及其中被打开的次数.
    shown: HashMap<&'static str, Counts>,
    opened: HashMap<&'static str, Counts>,
}

/// 一次打开里开了什么, 怎么记.
#[derive(Clone, Copy, PartialEq)]
pub enum Sample {
    /// 这次打开里第一次开东西: 摆出的候选的状态也记一笔.
    First,
    /// 同一次打开里接着又开了别的: 只记开了什么.
    More,
}

impl Ledger {
    /// 在 snapshot 这次打开里开了 key, 记 weight 笔.
    pub fn learn(&mut self, snapshot: &Snapshot, key: &str, weight: f64, sample: Sample) {
        let now = snapshot.at;
        for tag in &snapshot.tags {
            self.context.entry(tag.clone()).or_default().add(key, weight, now);
        }
        if sample == Sample::First {
            for c in &snapshot.candidates {
                for &state in c.states.iter().chain([&CANDIDATE]) {
                    self.shown.entry(state).or_default().add(&c.key, weight, now);
                }
            }
        }
        if let Some(chosen) = snapshot.candidates.iter().find(|c| c.key == key) {
            for &state in chosen.states.iter().chain([&CANDIDATE]) {
                self.opened.entry(state).or_default().add(key, weight, now);
            }
        }
    }

    /// ln (出现这个标签时开 key 的比例 / 平时开它的比例), 向平时平滑; 没见过的标签为 0.
    fn context_lift(&self, tag: &str, key: &str, p_target: f64, now: i64) -> f64 {
        let Some(counts) = self.context.get(tag) else {
            return 0.;
        };
        let smoothing = PARAMS.ledger.smoothing;
        let p = (counts.of(key, now) + smoothing * p_target) / (counts.total(now) + smoothing);
        (p / p_target).ln()
    }

    /// Σ ln (处于这个状态时开 key 的比例 / key 被摆出来时开它的比例). 两个比例都向所有程序的平均平滑,
    /// 没数据的程序按平均算; 某状态各程序都没数据时按 params.json 的起步倍数 (state_priors).
    fn state_lift(&self, states: &[&'static str], key: &str, now: i64) -> f64 {
        let smoothing = PARAMS.ledger.state_smoothing;
        let rate = |state: &'static str, all: f64| {
            let opened = self.opened.get(state);
            let shown = self.shown.get(state);
            let (o, s) = (opened.map_or(0., |c| c.of(key, now)), shown.map_or(0., |c| c.of(key, now)));
            (o + smoothing * all) / (s + smoothing)
        };
        let average = |state: &'static str, base: f64| {
            let prior = PARAMS.ledger.state_priors.get(state).copied().unwrap_or(1.);
            let opened = self.opened.get(state).map_or(0., |c| c.total(now));
            let shown = self.shown.get(state).map_or(0., |c| c.total(now));
            (opened + smoothing * base * prior) / (shown + smoothing)
        };
        let overall = {
            let opened = self.opened.get(CANDIDATE).map_or(0., |c| c.total(now));
            let shown = self.shown.get(CANDIDATE).map_or(0., |c| c.total(now));
            (opened + 1.) / (shown + 1.)
        };
        let usual = rate(CANDIDATE, average(CANDIDATE, overall));
        states.iter().map(|&state| (rate(state, average(state, overall)) / usual).ln()).sum()
    }
}

/// 各特征的权重, 一套权重同时用于开和不开 Jev: 没有 Jev 结果时 jev 一项不参与打分, 也不参与这次学习, 权重原样保留.
/// grad_sq 是各特征历次梯度的平方和 (AdaGrad), 学得越多步子越小, 量级大的特征也不会被拽得更多.
#[derive(Clone)]
pub struct Weights {
    pub value: Features,
    pub grad_sq: Features,
}

impl Weights {
    /// 起步按 params.json 的 learn.initial, 没给的为 0; Jev 一项起步就高. 学习时也往这里回拉.
    pub fn initial() -> Self {
        Self { value: initial_values(), grad_sq: [0.; FEATURES.len()] }
    }

    pub fn score(&self, x: &Features, with_jev: bool) -> f64 {
        self.value.iter().zip(x).enumerate().filter(|(ix, _)| with_jev || *ix != JEV).map(|(_, (w, v))| w * v).sum()
    }

    /// 一次打开的反馈: rows 是这次参与比较的候选, chosen 是被点的下标, shares 是各行算作反例的分量
    /// (被点的那行为 1; 离被点的越远, 越可能没被看到, 分量越小).
    /// 按加权 softmax 交叉熵走一步 (AdaGrad 步长), 往起步值回拉的 l2 也算在这一步的梯度里: 样本少时不被个别点击带偏;
    /// 放进梯度后两者一起缩小, 权重停在数据支持的位置. Jev 一项不低于 jev_floor, 它始终是比重高的一环.
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
        // 跳过之前都是 "记录显示现在该开它" 的证据, 权重可以压到不看, 但变负就成了越匹配越靠后
        for weight in &mut self.value[..SKIPPED] {
            *weight = weight.max(learn.evidence_floor);
        }
    }
}

fn initial_values() -> Features {
    let mut w = [0.; FEATURES.len()];
    w[JEV] = PARAMS.learn.jev_initial;
    for (name, &value) in &PARAMS.learn.initial {
        let ix = FEATURES.iter().position(|f| f == name).expect("params.json learn.initial 里是已知特征");
        w[ix] = value;
    }
    w
}
