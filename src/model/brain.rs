//! 推荐的打分和学习: 把一次打开 QRun 时的情况拆成标签, 按 "标签 → 打开了什么" 记账.
//!
//! - 情境标签 (整次打开一组, 如 `clip:开头:alter`, `前台:cindy`): 记 "出现这个标签时开了什么",
//!   打分时看 "出现这个标签时开它比平时高几倍", 这次的各标签取平均.
//! - 状态标签 (每个候选一组, 如 `自己:运行中`): 按候选分开记 "处于这个状态时被摆出几次, 被开几次",
//!   同一状态对不同程序可以一个加分一个扣分 (开着也常点的程序, 运行中对它是加分). 某程序数据少时退回所有程序的平均.
//! - 次数少时向 "和平时一样" 平滑, 见过一两次几乎不动, 多次才明显起作用; 旧账按半衰期变淡.
//!
//! 最终分数是几项的加权和: 程序本身的四项 (`predict::Model`), 情境, 状态, 最近被跳过, Jev. 权重从每次点击的对错里学.
//! 这里不碰库, 时钟和系统: 线上和回放调同一套函数, 回放量出的就是线上的效果. 账和权重由调用方存进库, 一直累积.

use std::collections::{HashMap, HashSet};

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
            // 取平均不取和: 同一次打开的标签是一起出现的, 一次偶然的打开会在这次的几十个标签上各记一笔, 加起来就把
            // 一次巧合算成几十份证据; 平均后一次打开最多算一份, 反复出现的习惯才高
            let lifts = snapshot.tags.iter().map(|tag| ledger.context_lift(tag, &c.key, p_target, snapshot.at));
            x[CONTEXT] = lifts.sum::<f64>() / snapshot.tags.len().max(1) as f64;
            x[STATE] = ledger.state_lift(&c.states, &c.key, snapshot.at);
            x[SKIPPED] = c.skipped;
            x
        })
        .collect()
}

/// Jev 给的概率填进特征: 信任 × ln(p + jev_smoothing). 加上平滑, Jev 给 0 的候选扣分有限, 本地记录很强时仍能排上来;
/// 乘信任 (`Ledger::jev_trust`), Jev 排得不准时它的意见分量小, 不会把本地排对的挤下去.
pub fn set_jev(x: &mut Features, probability: f64, trust: f64) {
    x[JEV] = trust * (probability + PARAMS.learn.jev_smoothing).ln();
}

/// 考核 Jev 一次的得分: 用户打开的在 Jev 排序 (概率从高到低) 里排第一得 1 分, 每往后一名乘 rank_decay, 不在里面得 0 分.
pub fn jev_credit(ranking: &[String], key: &str) -> f64 {
    ranking.iter().position(|k| k == key).map_or(0., |rank| PARAMS.jev.rank_decay.powi(rank as i32))
}

/// 按半衰期衰减的次数: 存次数和记下它的时间, 用时换算到那一刻.
#[derive(Clone, Copy, Default)]
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

/// 账的几本: 情境标签, 状态被摆出, 状态被打开, Jev 考核. 也是库里 ledger 表 book 列的值.
const CONTEXT_BOOK: &str = "context";
const SHOWN_BOOK: &str = "shown";
const OPENED_BOOK: &str = "opened";
const JEV_BOOK: &str = "jev";
/// 库里一行的目标为空时, 这行是这个标签的总数.
const TOTAL: &str = "";
const JEV_CREDIT: &str = "credit";
const JEV_JUDGED: &str = "judged";

/// 账里的一笔, 存库用: 哪本账, 标签 (或状态), 目标 (为空是总数), 次数和记下它的时间 (次数按半衰期从这时起变淡).
pub struct Entry {
    pub book: String,
    pub tag: String,
    pub target: String,
    pub count: f64,
    pub at: i64,
}

/// 标签账: 存在库里一直累积 (见 `library::smart` 的 read_ledger / save_ledger), 每次打开实时加.
/// 库里还没有账时 (升级上来, 或改了算法迁移时清掉) 由回放从历史建一次.
#[derive(Clone, Default)]
pub struct Ledger {
    /// 情境标签 -> 出现它的打开里开了什么.
    context: HashMap<String, Counts>,
    /// 状态 -> 处于这个状态的候选被摆出的次数, 以及其中被打开的次数.
    shown: HashMap<String, Counts>,
    opened: HashMap<String, Counts>,
    /// 考核 Jev: 历次得分之和, 考核次数.
    jev_credit: Decayed,
    jev_judged: Decayed,
    /// 上次存库后变了的 (账, 标签, 目标).
    changed: HashSet<(&'static str, String, String)>,
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
            self.add(CONTEXT_BOOK, tag, key, weight, now);
        }
        if sample == Sample::First {
            for c in &snapshot.candidates {
                for &state in c.states.iter().chain([&CANDIDATE]) {
                    self.add(SHOWN_BOOK, state, &c.key, weight, now);
                }
            }
        }
        if let Some(chosen) = snapshot.candidates.iter().find(|c| c.key == key) {
            for &state in chosen.states.iter().chain([&CANDIDATE]) {
                self.add(OPENED_BOOK, state, key, weight, now);
            }
        }
    }

    fn add(&mut self, book: &'static str, tag: &str, key: &str, weight: f64, now: i64) {
        let counts = match book {
            CONTEXT_BOOK => &mut self.context,
            SHOWN_BOOK => &mut self.shown,
            _ => &mut self.opened,
        };
        counts.entry(tag.to_string()).or_default().add(key, weight, now);
        self.changed.insert((book, tag.to_string(), key.to_string()));
        self.changed.insert((book, tag.to_string(), TOTAL.to_string()));
    }

    /// 一次打开里 Jev 排了序, 用户打开了 key: 按名次记一次得分.
    pub fn judge_jev(&mut self, ranking: &[String], key: &str, now: i64) {
        self.jev_credit.add(jev_credit(ranking, key), now);
        self.jev_judged.add(1., now);
        self.changed.insert((JEV_BOOK, JEV_CREDIT.to_string(), TOTAL.to_string()));
        self.changed.insert((JEV_BOOK, JEV_JUDGED.to_string(), TOTAL.to_string()));
    }

    /// 一笔都没记过.
    pub fn is_empty(&self) -> bool {
        self.context.is_empty() && self.shown.is_empty() && self.jev_judged.count == 0.
    }

    /// 从库里读出的各笔建账.
    pub fn from_entries(entries: impl IntoIterator<Item = Entry>) -> Self {
        let mut ledger = Self::default();
        for e in entries {
            let value = Decayed { count: e.count, at: e.at };
            if e.book == JEV_BOOK {
                match e.tag.as_str() {
                    JEV_CREDIT => ledger.jev_credit = value,
                    _ => ledger.jev_judged = value,
                }
                continue;
            }
            let counts = match e.book.as_str() {
                CONTEXT_BOOK => &mut ledger.context,
                SHOWN_BOOK => &mut ledger.shown,
                _ => &mut ledger.opened,
            };
            let counts = counts.entry(e.tag).or_default();
            match e.target.as_str() {
                TOTAL => counts.total = Some(value),
                _ => {
                    counts.targets.insert(e.target, value);
                }
            }
        }
        ledger
    }

    /// 上次取过之后变了的各笔, 存库用; 回放从零建的账全部算变了.
    pub fn take_changed(&mut self) -> Vec<Entry> {
        let changed = std::mem::take(&mut self.changed);
        changed
            .into_iter()
            .map(|(book, tag, target)| {
                let value = match book {
                    JEV_BOOK if tag == JEV_CREDIT => self.jev_credit,
                    JEV_BOOK => self.jev_judged,
                    _ => {
                        let counts = &match book {
                            CONTEXT_BOOK => &self.context,
                            SHOWN_BOOK => &self.shown,
                            _ => &self.opened,
                        }[&tag];
                        match target.as_str() {
                            TOTAL => counts.total.expect("记过的标签有总数"),
                            _ => counts.targets[&target],
                        }
                    }
                };
                Entry { book: book.to_string(), tag, target, count: value.count, at: value.at }
            })
            .collect()
    }

    /// Jev 的信任: 平均得分, 向 trust_prior 平滑.
    pub fn jev_trust(&self, now: i64) -> f64 {
        let p = &PARAMS.jev;
        (self.jev_credit.value(now) + p.trust_smoothing * p.trust_prior)
            / (self.jev_judged.value(now) + p.trust_smoothing)
    }

    /// Jev 的平均得分 (不平滑) 和考核次数; 还没考核过为 None.
    pub fn jev_score(&self, now: i64) -> Option<(f64, f64)> {
        let judged = self.jev_judged.value(now);
        (judged > 0.).then(|| (self.jev_credit.value(now) / judged, judged))
    }

    /// 给 Jev 看的习惯: 这次的情境标签里, 出现时开过 key 至少 evidence_min 次的, 按次数从多到少最多 evidence_tags 条,
    /// 每条是 (标签, 出现时开它的次数, 出现过的打开次数).
    pub fn evidence<'a>(&self, tags: &'a [String], key: &str, now: i64) -> Vec<(&'a str, f64, f64)> {
        let mut found: Vec<(&str, f64, f64)> = tags
            .iter()
            .filter_map(|tag| {
                let counts = self.context.get(tag)?;
                let opened = counts.of(key, now);
                (opened >= PARAMS.jev.evidence_min).then(|| (tag.as_str(), opened, counts.total(now)))
            })
            .collect();
        found.sort_by(|a, b| b.1.total_cmp(&a.1));
        found.truncate(PARAMS.jev.evidence_tags);
        found
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
    /// 放进梯度后两者一起缩小, 权重停在数据支持的位置.
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
        // 跳过之前都是 "记录显示现在该开它" 的证据, Jev 也是; 权重可以压到不看, 但变负就成了越匹配越靠后
        for feature in (0..SKIPPED).chain([JEV]) {
            self.value[feature] = self.value[feature].max(learn.evidence_floor);
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
