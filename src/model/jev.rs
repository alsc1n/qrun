//! Jev (TypeSafe System One API, `POST /v1/systemone`) 的请求和应答: 把这一刻的情境, 最近的启动和候选装进 state, 问一个 choice 问题
//! "下一个要打开哪个", 读回每个候选的概率. 只拼 JSON, 联网由调用方经 platform::http 做.

use std::collections::{HashMap, HashSet};

use serde_json::{Map, Value, json};

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
pub struct Draft {
    pub context: Map<String, Value>,
    pub candidates: Vec<Candidate>,
    pub query: Option<String>,
}

/// 候选多时要序列化好几次, 在后台线程调用.
/// 候选按名称起选项名 (重名的加序号), state 里按选项名放各自的信息, 选项说明里再写一遍名称和类型, 不靠下标对应.
/// 整个请求超过 max_state_bytes 时从排名靠后的候选开始去掉, 守在 Jev 的默认模式里.
pub fn request(draft: Draft) -> Request {
    let Draft { mut context, candidates, query } = draft;
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
        context.insert("search_query".into(), json!(query));
    }
    let instructions = match query {
        Some(_) => "The user typed search_query into the launcher's search box. Which candidate do they want to open?",
        None => {
            "The user just opened their app launcher. Which candidate will they open next? Use the situation \
             (time, foreground program and window title, clipboard), their recent launches and habits, and each \
             candidate's history."
        }
    };
    // 逐个试要序列化上百次; 体积随候选数单调增加, 二分找能放下的最多个数, 至少留一个
    let counts: Vec<usize> = (2..=options.len()).collect();
    let fits =
        |n: &usize| body(&context, &candidates, &options[..*n], instructions).len() <= PARAMS.jev.max_state_bytes;
    let count = options.len().min(1) + counts.partition_point(fits);
    options.truncate(count);
    Request { body: body(&context, &candidates, &options, instructions), options }
}

fn body(
    context: &Map<String, Value>,
    candidates: &[Candidate],
    options: &[(String, String)],
    instructions: &str,
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
    let request = json!({
        "model": PARAMS.jev.model,
        "state": state,
        "questions": {
            "next": { "type": "choice", "instructions": instructions, "criteria": criteria }
        }
    });
    serde_json::to_vec(&request).expect("serialize Jev request")
}

/// 读出每个候选的概率, 键是候选的比较键.
pub fn parse(body: &[u8], options: &[(String, String)]) -> Result<HashMap<String, f64>, String> {
    let value: Value = serde_json::from_slice(body).map_err(|e| format!("Jev 应答不是 JSON: {e}"))?;
    let probabilities = value
        .pointer("/answers/next/probabilities")
        .and_then(Value::as_object)
        .ok_or_else(|| format!("Jev 应答里没有 answers.next.probabilities: {value}"))?;
    Ok(options.iter().filter_map(|(option, key)| Some((key.clone(), probabilities.get(option)?.as_f64()?))).collect())
}
