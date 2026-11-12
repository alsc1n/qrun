//! 脱敏: 剪贴板, 窗口标题, 搜索词等进时间线和库, 发给 Jev 之前先过这里, 密钥, 密码, 个人信息换成只描述形状的占位符,
//! 如 `<随机串 前缀sk- 51位>`, 推荐和 Jev 仍能看出"这是一个密钥", 但拿不到它本身.
//!
//! 按顺序处理, 前面换掉的后面不再看: 结构 (私钥块, JWT, 网址里的账号密码和参数值) → 上下文 (password= 这类字样后的值)
//! → 个人信息 (邮箱, 手机号, 身份证号, 银行卡号, 其余数字串) → 兜底 (又长又乱的串).

use std::sync::LazyLock;

use regex::{Captures, Regex};

use crate::params::PARAMS;

/// 脱敏结果. shapes 是换掉的东西的形状 (如 "JWT", "手机号"), 去重, 按出现先后; 推荐把它们当标签用.
pub struct Redacted {
    pub text: String,
    pub shapes: Vec<String>,
    /// 一半以上的字符被换掉了: 基本就是一段密钥, 外发只发形状, 不发拼剩的残句.
    pub mostly_secret: bool,
}

struct Rules {
    private_key: Regex,
    jwt: Regex,
    userinfo: Regex,
    query_value: Regex,
    keyword_value: Regex,
    bearer: Regex,
    email: Regex,
    digits: Regex,
    token: Regex,
    vendor_prefix: Regex,
}

static RULES: LazyLock<Rules> = LazyLock::new(|| {
    let keywords: Vec<String> = PARAMS.redact.keywords.iter().map(|k| regex::escape(k)).collect();
    let rule = |pattern: &str| Regex::new(pattern).expect("脱敏规则");
    Rules {
        private_key: rule(r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?(?:-----END [A-Z ]*PRIVATE KEY-----|$)"),
        jwt: rule(r"eyJ[A-Za-z0-9_-]{4,}\.[A-Za-z0-9_-]{4,}\.[A-Za-z0-9_-]*"),
        userinfo: rule(r"([A-Za-z][A-Za-z0-9+.-]*://)[^/\s:@]+:[^/\s@]+@"),
        query_value: rule(r"([?&][^=\s&#?]+=)[^&\s#]+"),
        // 关键词后跟 = 或 : 才算; 只用空格隔开的 (the key point) 多半是普通文字
        keyword_value: rule(&format!(r#"(?i)((?:{})["']?\s*[:=：]\s*["']?)([^\s"',;]+)"#, keywords.join("|"))),
        bearer: rule(r"(?i)(bearer\s+)([A-Za-z0-9._~+/=-]+)"),
        email: rule(r"[A-Za-z0-9._%+-]+@([A-Za-z0-9.-]+\.[A-Za-z]{2,})"),
        digits: rule(r"\d+[Xx]?"),
        token: rule(r"[A-Za-z0-9+/=_-]+"),
        vendor_prefix: rule(r"^(?:[A-Za-z]{1,8}[-_]){1,2}"),
    }
});

pub fn redact(text: &str) -> Redacted {
    let rules = &*RULES;
    let total = text.chars().count().max(1);
    let mut shapes: Vec<String> = Vec::new();
    let mut replaced = 0usize;
    let mut note = |shape: &str, original: &str, shapes: &mut Vec<String>| {
        replaced += original.chars().count();
        if !shapes.iter().any(|s| s == shape) {
            shapes.push(shape.to_string());
        }
    };

    let text = rules.private_key.replace_all(text, |caps: &Captures| {
        note("私钥", &caps[0], &mut shapes);
        "<私钥>".to_string()
    });
    let text = rules.jwt.replace_all(&text, |caps: &Captures| {
        note("JWT", &caps[0], &mut shapes);
        "<JWT>".to_string()
    });
    let text = rules.userinfo.replace_all(&text, |caps: &Captures| {
        note("账号密码", &caps[0][caps[1].len()..], &mut shapes);
        format!("{}<账号密码>@", &caps[1])
    });
    let text = rules.query_value.replace_all(&text, |caps: &Captures| {
        note("参数值", &caps[0][caps[1].len()..], &mut shapes);
        format!("{}<值>", &caps[1])
    });
    let text = rules.keyword_value.replace_all(&text, |caps: &Captures| {
        if caps[2].starts_with('<') {
            return caps[0].to_string();
        }
        note("密码", &caps[2], &mut shapes);
        format!("{}<值 {}位>", &caps[1], caps[2].chars().count())
    });
    let text = rules.bearer.replace_all(&text, |caps: &Captures| {
        note("令牌", &caps[2], &mut shapes);
        format!("{}<令牌>", &caps[1])
    });
    let text = rules.email.replace_all(&text, |caps: &Captures| {
        note("邮箱", &caps[0], &mut shapes);
        format!("<邮箱 @{}>", &caps[1])
    });
    let current = text.clone();
    let text = rules.digits.replace_all(&text, |caps: &Captures| {
        let found = caps.get(0).expect("whole match");
        // 夹在字母数字中间的 (v20261112, abc123) 是标识符的一部分, 不是独立的号码
        let before = current[..found.start()].chars().next_back();
        let after = current[found.end()..].chars().next();
        if before.is_some_and(|c| c.is_ascii_alphanumeric()) || after.is_some_and(|c| c.is_ascii_alphanumeric()) {
            return found.as_str().to_string();
        }
        match number_shape(found.as_str()) {
            Some(shape) => {
                note(shape.trim_matches(['<', '>']), found.as_str(), &mut shapes);
                shape
            }
            None => found.as_str().to_string(),
        }
    });
    let text = rules.token.replace_all(&text, |caps: &Captures| {
        let token = &caps[0];
        if !looks_random(token) {
            return token.to_string();
        }
        note("随机串", token, &mut shapes);
        match rules.vendor_prefix.find(token) {
            // 前缀后面还剩够长才算厂商标识 (sk-, ghp_), 否则是普通的连字符词
            Some(prefix) if token.len() - prefix.end() >= PARAMS.redact.min_secret_len => {
                format!("<随机串 前缀{} {}位>", prefix.as_str(), token.chars().count())
            }
            _ => format!("<随机串 {}位>", token.chars().count()),
        }
    });

    let mostly_secret = replaced * 2 > total;
    Redacted { text: text.into_owned(), shapes, mostly_secret }
}

/// 独立的一串数字是什么: 手机号, 身份证号, 银行卡号, 或 4 到 8 位的数字 (验证码, 端口, 年份); 其余不换.
fn number_shape(digits: &str) -> Option<String> {
    let len = digits.len();
    let bytes = digits.as_bytes();
    if len == 11 && bytes[0] == b'1' && (b'3'..=b'9').contains(&bytes[1]) {
        return Some("<手机号>".into());
    }
    if len == 18 && id_checksum_ok(digits) {
        return Some("<身份证号>".into());
    }
    if digits.bytes().all(|b| b.is_ascii_digit()) {
        if (13..=19).contains(&len) && luhn_ok(digits) {
            return Some("<银行卡号>".into());
        }
        if (4..=8).contains(&len) {
            return Some(format!("<{len}位数字>"));
        }
    }
    None
}

/// 18 位身份证号的最后一位校验码.
fn id_checksum_ok(id: &str) -> bool {
    const WEIGHTS: [u32; 17] = [7, 9, 10, 5, 8, 4, 2, 1, 6, 3, 7, 9, 10, 5, 8, 4, 2];
    const CHECK: &[u8; 11] = b"10X98765432";
    let bytes = id.as_bytes();
    if !bytes[..17].iter().all(u8::is_ascii_digit) {
        return false;
    }
    let sum: u32 = bytes[..17].iter().zip(WEIGHTS).map(|(b, w)| (b - b'0') as u32 * w).sum();
    CHECK[(sum % 11) as usize] == bytes[17].to_ascii_uppercase()
}

/// 银行卡号的 Luhn 校验.
fn luhn_ok(digits: &str) -> bool {
    let sum: u32 = digits
        .bytes()
        .rev()
        .enumerate()
        .map(|(ix, b)| {
            let d = (b - b'0') as u32;
            if ix % 2 == 1 { if d * 2 > 9 { d * 2 - 9 } else { d * 2 } } else { d }
        })
        .sum();
    sum.is_multiple_of(10)
}

/// 又长又乱的串: 够长, 字符分布接近随机 (香农熵接近这么长的串能达到的上限), 字母和数字交替好几次.
/// 交替次数把驼峰标识符 (getUserById2) 和单词连写排除在外, 它们熵也不低.
/// 厂商前缀 (sk_, ghp_) 不随机, 只看后面的部分; 后面全是十六进制时按 16 个字符算上限, 否则 sk_ 加 32 位十六进制这种
/// 会因为多了几个字母被按 64 个字符的上限衡量, 显得不够乱.
fn looks_random(token: &str) -> bool {
    let redact = &PARAMS.redact;
    let body = &token[RULES.vendor_prefix.find(token).map_or(0, |prefix| prefix.end())..];
    let len = body.len();
    if len < redact.min_secret_len {
        return false;
    }
    let switches = body
        .as_bytes()
        .windows(2)
        .filter(|pair| {
            pair[0].is_ascii_digit() != pair[1].is_ascii_digit() && pair.iter().all(u8::is_ascii_alphanumeric)
        })
        .count();
    if switches < redact.min_digit_switches {
        return false;
    }
    let hex = body.bytes().all(|b| b.is_ascii_hexdigit());
    let alphabet: f64 = if hex { 16. } else { 64. };
    let mut counts = [0usize; 256];
    for b in body.bytes() {
        counts[b as usize] += 1;
    }
    let entropy: f64 = counts
        .iter()
        .filter(|&&n| n > 0)
        .map(|&n| {
            let p = n as f64 / len as f64;
            -p * p.log2()
        })
        .sum();
    entropy >= redact.entropy_ratio * (len as f64).min(alphabet).log2()
}
