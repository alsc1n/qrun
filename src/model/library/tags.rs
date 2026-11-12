//! 把一次打开时的情况拆成标签 (`model::brain` 按标签记账): 情境标签整次打开一组, 状态标签每个候选一组.
//! 只做通用的拆分 (剪贴板的形状和开头的词, 前台程序, 窗口标题的段, 最近发生的事), 不按场景写规则.
//! 输入都已脱敏 (时间线和标题在记下时就脱敏了).

use std::collections::HashSet;
use std::path::Path;

use super::NEW_FILE;
use super::activity::{self, Recent};
use super::smart::Situation;
use super::timeline::{Timeline, What};
use crate::model::predict;
use crate::params::PARAMS;

const MINUTE: i64 = 60_000;

/// 情境标签: 剪贴板, 前台程序, 窗口标题, 最近发生的事, 距上次打开多久; 去重, 最多 max_context_tags 个.
pub(super) fn context(situation: &Situation, timeline: &Timeline, gap: Option<i64>, now: i64) -> Vec<String> {
    let mut tags: Vec<String> = Vec::new();
    clipboard(timeline, now, &mut tags);
    if let Some(program) = &situation.foreground {
        tags.push(format!("前台:{}", stem(program)));
    }
    if let Some(title) = &situation.title {
        title_tags(title, situation.foreground.as_deref(), &mut tags);
    }
    tags.extend(activity::tokens(timeline, now).into_iter().map(|token| format!("事:{token}")));
    if let Some(gap) = gap {
        tags.push(format!("距上次:{}", predict::gap_bucket(gap)));
    }
    let mut seen = HashSet::new();
    tags.retain(|tag| seen.insert(tag.clone()));
    tags.truncate(PARAMS.tags.max_context_tags);
    tags
}

/// 候选此刻的状态. name 是显示名, title 是前台窗口标题 (小写).
pub(super) struct StateInput<'a> {
    pub running: &'a HashSet<String>,
    pub foreground: Option<&'a str>,
    pub title: &'a str,
    pub recent: &'a Recent,
}

pub(super) fn states(key: &str, name: &str, kind: &str, input: &StateInput) -> Vec<&'static str> {
    let thresholds = &PARAMS.tags;
    let activity = input.recent.activity(key);
    let name = name.to_lowercase();
    [
        ("自己:运行中", input.running.contains(key)),
        ("自己:前台", input.foreground == Some(key)),
        ("自己:刚改过", activity.modified >= thresholds.modified),
        ("自己:前台很久", activity.focused_minutes >= thresholds.focused_minutes),
        ("自己:刚启动", activity.started >= thresholds.started),
        // 一个字的名称在标题里到处都是, 不算
        ("自己:在标题里", name.chars().count() >= 2 && input.title.contains(&name)),
        ("自己:附近", input.recent.nearby(key) >= thresholds.nearby),
        ("自己:新文件", kind == NEW_FILE),
    ]
    .into_iter()
    .filter_map(|(state, on)| on.then_some(state))
    .collect()
}

/// 最近一次复制 (clip_minutes 内的): 类型, 来源, 多久前, 文字的形状和开头的词, 文件扩展名.
fn clipboard(timeline: &Timeline, now: i64, tags: &mut Vec<String>) {
    let p = &PARAMS.tags;
    let Some(event) = timeline.events.iter().rev().find(|e| matches!(e.what, What::Clipboard { .. })) else {
        return;
    };
    let age = now - event.end;
    if age > p.clip_minutes * MINUTE {
        return;
    }
    let What::Clipboard { content, from, text, shapes, files, protected } = &event.what else {
        unreachable!("刚按类型找到的");
    };
    tags.push(format!("clip:{content}"));
    tags.push(format!(
        "clip:新:{}",
        match age {
            ..MINUTE => "刚刚",
            _ if age < 10 * MINUTE => "几分钟前",
            _ => "较早",
        }
    ));
    if !from.is_empty() {
        tags.push(format!("clip:来自:{}", stem(Path::new(from))));
    }
    if *protected {
        tags.push("clip:受保护".into());
    }
    tags.extend(shapes.iter().map(|shape| format!("clip:形状:{shape}")));
    tags.extend(files.iter().map(|ext| format!("clip:扩展名:{ext}")));
    let Some(text) = text else {
        return;
    };
    let lines = text.lines().filter(|l| !l.trim().is_empty()).count();
    tags.push(format!(
        "clip:行:{}",
        match lines {
            ..=1 => "单行",
            2..=10 => "多行",
            _ => "很多行",
        }
    ));
    let chars = text.chars().count();
    tags.push(format!(
        "clip:长:{}",
        match chars {
            ..20 => "短",
            20..100 => "中",
            _ => "长",
        }
    ));
    tags.push(format!("clip:字符:{}", composition(text)));
    let words = words(text);
    if let Some(first) = words.first() {
        tags.push(format!("clip:开头:{first}"));
    }
    tags.extend(words.into_iter().take(p.clip_words).map(|w| format!("clip:词:{w}")));
}

/// 窗口标题按常见分隔符切成段 (浏览器, 编辑器标题都是 "页面 - 站点 - 程序" 这样), 去掉末尾的程序名,
/// 每段截 title_segment_chars 个字, 最多 title_tags 段.
fn title_tags(title: &str, foreground: Option<&Path>, tags: &mut Vec<String>) {
    let p = &PARAMS.tags;
    let program = foreground.map(stem);
    let mut segments: Vec<String> = Vec::new();
    // " - " 要带空格切, 文件名和网址里的连字符 (my-project) 不是分隔符
    for part in title.split(" - ") {
        for piece in part.split(['—', '–', '·', '|']) {
            let piece = piece.trim().to_lowercase();
            if !piece.is_empty() {
                segments.push(piece.chars().take(p.title_segment_chars).collect());
            }
        }
    }
    if segments.len() > 1
        && program.as_deref().is_some_and(|program| segments.last().is_some_and(|s| s.contains(program)))
    {
        segments.pop();
    }
    tags.extend(segments.into_iter().take(p.title_tags).map(|s| format!("标题:{s}")));
}

/// 字符组成: 纯数字, 含中文, 字母为主, 混合.
fn composition(text: &str) -> &'static str {
    let (mut digits, mut letters, mut cjk, mut total) = (0, 0, 0, 0);
    for c in text.chars().filter(|c| !c.is_whitespace()) {
        total += 1;
        if c.is_ascii_digit() {
            digits += 1;
        } else if c.is_ascii_alphabetic() {
            letters += 1;
        } else if is_cjk(c) {
            cjk += 1;
        }
    }
    if total > 0 && digits == total {
        "纯数字"
    } else if cjk * 5 >= total {
        "含中文"
    } else if letters * 2 >= total {
        "字母为主"
    } else {
        "混合"
    }
}

/// 有意义的词, 去重, 按出现先后: 拉丁按非字母数字切 (转小写, 至少 2 个字符), 中文按标点切段 (每段只取前
/// cjk_word_chars 个字, 不做词典分词, 语义靠 Jev 归类补);
/// 脱敏占位符 (<...>) 已作为形状标签记下, 不再拆. 停用词跳过.
fn words(text: &str) -> Vec<String> {
    let p = &PARAMS.tags;
    let mut clean = String::new();
    let mut depth = 0;
    for c in text.chars() {
        match c {
            '<' => depth += 1,
            '>' if depth > 0 => depth -= 1,
            _ if depth == 0 => clean.push(c),
            _ => {}
        }
    }
    let mut words: Vec<String> = Vec::new();
    let mut current = String::new();
    let flush = |current: &mut String, words: &mut Vec<String>| {
        let word = std::mem::take(current).to_lowercase();
        let long_enough = word.chars().count() >= 2;
        if long_enough && !p.stopwords.contains(&word) && !words.contains(&word) {
            words.push(word);
        }
    };
    let mut cjk_run = false;
    for c in clean.chars() {
        let cjk = is_cjk(c);
        let word_char = c.is_ascii_alphanumeric() || c == '_' || cjk;
        if !word_char || (cjk != cjk_run && !current.is_empty()) {
            flush(&mut current, &mut words);
        }
        if word_char && !(cjk && current.chars().count() >= p.cjk_word_chars) {
            current.push(c);
        }
        cjk_run = cjk;
    }
    flush(&mut current, &mut words);
    // 纯数字的词 (行号, 金额) 不算
    words.retain(|w| !w.chars().all(|c| c.is_ascii_digit()));
    words
}

fn is_cjk(c: char) -> bool {
    ('\u{4e00}'..='\u{9fff}').contains(&c)
}

/// 程序或文件的名称 (不带扩展名, 小写).
fn stem(path: &Path) -> String {
    path.file_stem().unwrap_or(path.as_os_str()).to_string_lossy().to_lowercase()
}
