//! Jev (TypeSafe System One API, `POST /v1/systemone`) 的请求和应答: 把这一刻的情境, 最近的启动和候选 (附本地记下的习惯)
//! 装进 state, 同一次请求问几个 choice 问题: "下一个要打开哪个", 以及归类 "剪贴板里是什么", "用户在做什么";
//! 读回每个候选的概率和归类标签. 只拼 JSON, 联网由调用方经 platform::http 做.

use std::collections::{HashMap, HashSet};

use serde_json::{Map, Value, json};

use crate::model::redact::redact;
use crate::params::PARAMS;

/// 准备发出的请求; options 是选项名到候选比较键的对应, 读应答时用.
pub struct Request {
    pub body: Vec<u8>,
    pub options: Vec<(String, String)>,
}

/// 一个候选: key 是比较键, name 是显示名, details 是给 Jev 看的各项信息.
pub struct Candidate {
    pub key: String,
    pub name: String,
    pub details: Value,
}

/// 组一次请求要的材料: context 是情境和习惯摘要 (对象), candidates 已按本地排名从前到后排好.
/// classify 时问 "用户在做什么", 再 ask_clip 时问 "剪贴板里是什么" (最近复制过且不是受保护的内容).
pub struct Draft {
    pub context: Map<String, Value>,
    pub candidates: Vec<Candidate>,
    pub query: Option<String>,
    pub classify: bool,
    pub ask_clip: bool,
}

/// Jev 的应答: 每个候选 (比较键) 的概率, 归类得到的情境标签.
#[derive(Clone)]
pub struct Answer {
    pub probabilities: HashMap<String, f64>,
    pub tags: Vec<String>,
}

impl Answer {
    /// 候选按 Jev 给的概率从高到低.
    pub fn ranking(&self) -> Vec<String> {
        let mut ranking: Vec<(&String, f64)> = self.probabilities.iter().map(|(k, &p)| (k, p)).collect();
        ranking.sort_by(|a, b| b.1.total_cmp(&a.1));
        ranking.into_iter().map(|(k, _)| k.clone()).collect()
    }
}

/// 归类问题: 问题名, 选项, 记成标签时的前缀, 问法.
struct Classify {
    name: &'static str,
    options: &'static [String],
    prefix: &'static str,
    instructions: &'static str,
}

fn classify_questions(ask_clip: bool) -> Vec<Classify> {
    let p = &PARAMS.jev;
    let mut questions = vec![Classify {
        name: "doing",
        options: &p.doings,
        prefix: "jev:在做",
        instructions: "Judging from the situation and recent activity, what is the user doing right now?",
    }];
    if ask_clip {
        questions.push(Classify {
            name: "clip_kind",
            options: &p.clip_kinds,
            prefix: "jev:剪贴板",
            instructions: "What kind of content did the user most recently copy to the clipboard \
                           (the latest clipboard event in recent_activity_on_this_computer)?",
        });
    }
    questions
}

/// 候选多时要序列化好几次, 在后台线程调用.
/// 候选按名称起选项名 (重名的加序号), state 里按选项名放各自的信息, 选项说明里再写一遍名称和类型, 不靠下标对应.
/// 整个请求超过 max_state_bytes 时从排名靠后的候选开始去掉, 守在 Jev 的默认模式里.
pub fn request(draft: Draft) -> Request {
    let Draft { mut context, candidates, query, classify, ask_clip } = draft;
    let mut questions = Map::new();
    if classify {
        for q in classify_questions(ask_clip) {
            let criteria: Map<String, Value> = q.options.iter().map(|o| (o.clone(), json!(o))).collect();
            let question = json!({ "type": "choice", "instructions": q.instructions, "criteria": criteria });
            questions.insert(q.name.into(), question);
        }
    }
    let mut used = HashSet::new();
    let mut options: Vec<(String, String)> = Vec::new();
    for candidate in candidates.iter().take(PARAMS.jev.max_candidates) {
        let mut option = candidate.name.clone();
        let mut n = 2;
        while !used.insert(option.clone()) {
            option = format!("{} ({n})", candidate.name);
            n += 1;
        }
        options.push((option, candidate.key.clone()));
    }
    if let Some(query) = &query {
        context.insert("search_query".into(), json!(redact(query).text));
    }
    let instructions = match query {
        Some(_) => "The user typed search_query into the launcher's search box. Which candidate do they want to open?",
        None => {
            "The user just opened their app launcher. Which candidate will they open next? Each candidate's \
             habits_in_this_situation records how often THIS user opened it before when the same signal was present \
             (signal, opened_it, signal_seen); the user's own habits matter most. Also use the situation (time, \
             foreground program and window title, clipboard), recent activity and launches."
        }
    };
    // 逐个试要序列化很多次; 体积随候选数单调增加, 二分找能放下的最多个数, 至少留一个
    let counts: Vec<usize> = (2..=options.len()).collect();
    let fits = |n: &usize| {
        body(&context, &candidates, &options[..*n], instructions, &questions).len() <= PARAMS.jev.max_state_bytes
    };
    let count = options.len().min(1) + counts.partition_point(fits);
    options.truncate(count);
    Request { body: body(&context, &candidates, &options, instructions, &questions), options }
}

fn body(
    context: &Map<String, Value>,
    candidates: &[Candidate],
    options: &[(String, String)],
    instructions: &str,
    classify: &Map<String, Value>,
) -> Vec<u8> {
    let mut state = context.clone();
    let mut listed = Map::new();
    let mut criteria = Map::new();
    for ((option, _), candidate) in options.iter().zip(candidates) {
        listed.insert(option.clone(), candidate.details.clone());
        let kind = candidate.details.get("type").and_then(Value::as_str).unwrap_or("item");
        criteria
            .insert(option.clone(), json!(format!("Open \"{}\" ({kind}), see candidates.{option}", candidate.name)));
    }
    state.insert("candidates".into(), Value::Object(listed));
    let mut questions = classify.clone();
    questions.insert("next".into(), json!({ "type": "choice", "instructions": instructions, "criteria": criteria }));
    let request = json!({ "model": PARAMS.jev.model, "state": state, "questions": questions });
    serde_json::to_vec(&request).expect("serialize Jev request")
}

/// 读出每个候选的概率 (键是候选的比较键), 以及归类问题里概率最高且不低于 tag_min_probability 的答案 (成为情境标签).
pub fn parse(body: &[u8], options: &[(String, String)]) -> Result<Answer, String> {
    let value: Value = serde_json::from_slice(body).map_err(|e| format!("Jev 应答不是 JSON: {e}"))?;
    let probabilities = value
        .pointer("/answers/next/probabilities")
        .and_then(Value::as_object)
        .ok_or_else(|| format!("Jev 应答里没有 answers.next.probabilities: {value}"))?;
    let probabilities =
        options.iter().filter_map(|(option, key)| Some((key.clone(), probabilities.get(option)?.as_f64()?))).collect();
    // 没问的问题应答里没有, 按问过的全集找
    let tags = classify_questions(true)
        .into_iter()
        .filter_map(|q| {
            let answer = value.pointer(&format!("/answers/{}/probabilities", q.name))?.as_object()?;
            let (label, p) =
                answer.iter().filter_map(|(label, p)| Some((label, p.as_f64()?))).max_by(|a, b| a.1.total_cmp(&b.1))?;
            (p >= PARAMS.jev.tag_min_probability).then(|| format!("{}:{label}", q.prefix))
        })
        .collect();
    Ok(Answer { probabilities, tags })
}
