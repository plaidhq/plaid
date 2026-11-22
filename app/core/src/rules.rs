//! Local text rules applied to the ASR final before the paste (and the format subset after the LLM).
//! Every rule is off unless its setting is "1"; every rule is idempotent and deliberately narrow:
//! a missed filler costs less than a mangled sentence.

use crate::text::is_ideographic;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rules {
    /// Drop standalone 嗯/呃/那个 clauses and stray 嗯/呃.
    pub fillers: bool,
    /// Collapse immediate restarts: 明天明天 → 明天, 我们，我们 → 我们.
    pub repeats: bool,
    /// Explicit self-corrections: 三点，不对，四点 → 四点.
    pub corrections: bool,
    /// Strip the sentence-final 。/. (questions and exclamations keep theirs).
    pub no_period: bool,
    /// A space between CJK and Latin letters or digits.
    pub spacing: bool,
    /// Chinese numerals ≥ 10, digit strings and percentages to Arabic.
    pub numbers: bool,
}

pub const KEYS: [&str; 6] = [
    "ruleFillers",
    "ruleRepeats",
    "ruleCorrections",
    "ruleNoPeriod",
    "ruleSpacing",
    "ruleNumbers",
];

impl Rules {
    pub fn load(mut setting: impl FnMut(&str) -> Option<String>) -> Rules {
        let mut on = |k: &str| setting(k).as_deref() == Some("1");
        Rules {
            fillers: on(KEYS[0]),
            repeats: on(KEYS[1]),
            corrections: on(KEYS[2]),
            no_period: on(KEYS[3]),
            spacing: on(KEYS[4]),
            numbers: on(KEYS[5]),
        }
    }

    /// All enabled rules. Returns the input unchanged if the rules would leave nothing.
    pub fn apply(&self, s: &str) -> String {
        let mut t = s.to_string();
        if self.fillers {
            t = fillers(&t);
        }
        if self.repeats {
            t = repeats(&t);
        }
        if self.corrections {
            t = corrections(&t);
        }
        t = self.format(&t);
        if t.trim().is_empty() {
            s.to_string()
        } else {
            t
        }
    }

    /// Formatting only — safe on LLM output, which already handled fillers and restarts.
    pub fn format(&self, s: &str) -> String {
        let mut t = s.to_string();
        if self.numbers {
            t = numbers(&t);
        }
        if self.spacing {
            t = spacing(&t);
        }
        if self.no_period {
            t = no_period(&t);
        }
        t
    }
}

// Clauses ------------------------------------------------------------------------------------------

const SEPS: &[char] = &[
    '，', ',', '、', '。', '！', '？', '!', '?', '；', ';', '：', ':', '\n',
];
const TERMINAL: &[char] = &['。', '！', '？', '!', '?', '\n'];

/// `(clause, separator)` pairs; the separator is None only for an unterminated tail.
fn clauses(s: &str) -> Vec<(String, Option<char>)> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in s.chars() {
        if SEPS.contains(&c) {
            out.push((std::mem::take(&mut cur), Some(c)));
        } else {
            cur.push(c);
        }
    }
    if !cur.is_empty() {
        out.push((cur, None));
    }
    out
}

fn join(parts: &[(String, Option<char>)]) -> String {
    let mut s = String::new();
    for (t, sep) in parts {
        s.push_str(t);
        if let Some(c) = sep {
            s.push(*c);
        }
    }
    s
}

/// Removes clause `i`. A terminal separator survives by replacing the previous clause's comma.
fn drop_clause(parts: &mut Vec<(String, Option<char>)>, i: usize) {
    let (_, sep) = parts.remove(i);
    if let Some(c) = sep.filter(|c| TERMINAL.contains(c))
        && i > 0
        && !parts[i - 1].1.is_some_and(|p| TERMINAL.contains(&p))
    {
        parts[i - 1].1 = Some(c);
    }
}

// Fillers ------------------------------------------------------------------------------------------

const FILLER_CLAUSES: &[&str] = &["嗯", "呃", "额", "唔", "啊", "那个", "就是说"];

fn is_filler_clause(t: &str) -> bool {
    let t = t.trim();
    if t.is_empty() {
        return false;
    }
    FILLER_CLAUSES.iter().any(|f| {
        let mut rest = t;
        while let Some(r) = rest.strip_prefix(f) {
            rest = r;
        }
        rest.is_empty()
    })
}

fn fillers(s: &str) -> String {
    let mut parts = clauses(s);
    let mut i = 0;
    while i < parts.len() {
        if is_filler_clause(&parts[i].0) {
            drop_clause(&mut parts, i);
        } else {
            i += 1;
        }
    }
    // 嗯 and 呃 are never part of a written word.
    let t: String = join(&parts)
        .chars()
        .filter(|c| !matches!(c, '嗯' | '呃'))
        .collect();
    t.trim_start().to_string()
}

// Repeats ------------------------------------------------------------------------------------------

/// Legitimate ABAB reduplication and emphasis that look like restarts.
const KEEP_DOUBLE: &[&str] = &[
    "非常", "特别", "真的", "超级", "好多", "讨论", "研究", "考虑", "休息", "学习", "商量", "整理",
    "锻炼", "收拾", "打扫", "了解", "尝试", "介绍", "活动", "准备", "检查", "看看", "试试", "想想",
    "聊聊", "走走", "哈哈", "呵呵", "嘿嘿", "咋地", "谢谢",
];
const AFFIRM: &[&str] = &[
    "对", "好", "是", "行", "嗯", "好的", "对的", "是的", "没错", "可以", "谢谢",
];

fn repeats(s: &str) -> String {
    let c: Vec<char> = s.chars().collect();
    let mut out: Vec<char> = Vec::with_capacity(c.len());
    let mut i = 0;
    'outer: while i < c.len() {
        // XX with X = 2..=6 CJK chars, longest first.
        for n in (2..=6).rev() {
            if i + 2 * n > c.len() || !c[i..i + n].iter().all(|&x| is_ideographic(x)) {
                continue;
            }
            if c[i..i + n] != c[i + n..i + 2 * n] {
                continue;
            }
            let x: String = c[i..i + n].iter().collect();
            if c[i] == '一' || KEEP_DOUBLE.contains(&x.as_str()) {
                continue;
            }
            out.extend_from_slice(&c[i..i + n]);
            i += 2 * n;
            continue 'outer;
        }
        out.push(c[i]);
        i += 1;
    }
    let t: String = out.into_iter().collect();
    // X，X... → X...: a restart across a comma.
    let mut parts = clauses(&t);
    let mut i = 0;
    while i + 1 < parts.len() {
        let a = parts[i].0.trim();
        let n = a.chars().count();
        let comma = matches!(parts[i].1, Some('，' | ','));
        if comma
            && (2..=6).contains(&n)
            && !AFFIRM.contains(&a)
            && a.chars().all(is_ideographic)
            && parts[i + 1].0.trim_start().starts_with(a)
        {
            parts.remove(i);
        } else {
            i += 1;
        }
    }
    join(&parts)
}

// Corrections --------------------------------------------------------------------------------------

const MARKERS: &[&str] = &["不对", "不是", "我是说", "我的意思是", "说错了", "应该是"];
const UNITS: &[char] = &[
    '点', '号', '日', '月', '年', '个', '块', '元', '岁', '次', '分', '天', '周', '人', '楼', '层',
    '页', '章', '倍', '%', '万', '亿',
];
const PREFIXES: &[&str] = &["周", "星期", "礼拜"];
const NUMERALS: &str = "零一二三四五六七八九十百千万亿两半";

fn is_num(c: char) -> bool {
    c.is_ascii_digit() || NUMERALS.contains(c)
}

/// `prefix? number unit?` at the start of `s`: (prefix, unit, byte length).
fn quantity_head(s: &str) -> Option<(&'static str, Option<char>, usize)> {
    let pre = PREFIXES
        .iter()
        .find(|p| s.starts_with(**p))
        .copied()
        .unwrap_or("");
    let rest = &s[pre.len()..];
    let n: usize = rest
        .chars()
        .take_while(|&c| is_num(c))
        .map(char::len_utf8)
        .sum();
    if n == 0 {
        return None;
    }
    let sp = rest[n..].len() - rest[n..].trim_start_matches(' ').len();
    let unit = rest[n + sp..]
        .chars()
        .next()
        .filter(|&c| UNITS.contains(&c) && !is_num(c));
    Some((
        pre,
        unit,
        pre.len() + n + unit.map_or(0, |u| sp + u.len_utf8()),
    ))
}

/// Byte offset where `a` ends with a quantity of the same shape.
fn quantity_tail(a: &str, pre: &str, unit: Option<char>) -> Option<usize> {
    let mut end = a.len();
    if let Some(u) = unit {
        end = a.strip_suffix(u)?.trim_end_matches(' ').len();
    } else if a
        .chars()
        .last()
        .is_some_and(|c| UNITS.contains(&c) && !is_num(c))
    {
        return None;
    }
    let n: usize = a[..end]
        .chars()
        .rev()
        .take_while(|&c| is_num(c))
        .map(char::len_utf8)
        .sum();
    if n == 0 {
        return None;
    }
    let start = end - n;
    if pre.is_empty() {
        return Some(start);
    }
    a[..start].strip_suffix(pre).map(|s| s.len())
}

fn corrections(s: &str) -> String {
    let mut parts = clauses(s);
    let mut i = 1;
    while i + 1 < parts.len() {
        let comma = |p: &(String, Option<char>)| matches!(p.1, Some('，' | ','));
        if comma(&parts[i - 1]) && comma(&parts[i]) && MARKERS.contains(&parts[i].0.trim()) {
            let b = parts[i + 1].0.trim_start().to_string();
            let a = parts[i - 1].0.clone();
            if let Some((pre, unit, _)) = quantity_head(&b)
                && let Some(cut) = quantity_tail(&a, pre, unit)
            {
                parts[i + 1].0 = format!("{}{}", &a[..cut], b);
                parts.drain(i - 1..=i);
                continue;
            }
        }
        i += 1;
    }
    join(&parts)
}

// Numbers ------------------------------------------------------------------------------------------

fn digit(c: char) -> Option<u64> {
    "零一二三四五六七八九"
        .find(c)
        .map(|b| (b / 3) as u64)
        .or((c == '两').then_some(2))
}

fn magnitude(c: char) -> Option<u64> {
    match c {
        '十' => Some(10),
        '百' => Some(100),
        '千' => Some(1000),
        _ => None,
    }
}

/// A section below 万: 三千零五, 二十, 十二, 三百五 (= 350).
fn section(c: &[char]) -> Option<u64> {
    if c.is_empty() {
        return Some(0);
    }
    let (mut total, mut pending, mut last_mag, mut zero) = (0u64, None::<u64>, 10_000u64, false);
    for (i, &ch) in c.iter().enumerate() {
        if let Some(d) = digit(ch) {
            if d == 0 {
                if pending.is_some() {
                    return None;
                }
                zero = true;
                continue;
            }
            if pending.is_some() {
                return None;
            }
            pending = Some(d);
        } else {
            let m = magnitude(ch)?;
            if m >= last_mag {
                return None;
            }
            let d = match pending.take() {
                Some(d) => d,
                None if m == 10 && i == 0 => 1,
                None => return None,
            };
            total += d * m;
            last_mag = m;
            zero = false;
        }
    }
    if let Some(d) = pending {
        // 三百五 = 350, but 三百零五 = 305.
        let scale = if last_mag < 10_000 && last_mag > 10 && !zero {
            last_mag / 10
        } else {
            1
        };
        total += d * scale;
    }
    Some(total)
}

fn value(run: &[char]) -> Option<u64> {
    let mut total = 0u64;
    let mut rest = run;
    for (unit, scale) in [('亿', 100_000_000u64), ('万', 10_000)] {
        if let Some(p) = rest.iter().position(|&c| c == unit) {
            let s = section_or_value(&rest[..p])?;
            if s == 0 {
                return None;
            }
            total += s * scale;
            rest = &rest[p + 1..];
            // 一万五 = 15000
            if let [d] = rest
                && let Some(d) = digit(*d).filter(|d| *d > 0)
            {
                return Some(total + d * scale / 10);
            }
        }
    }
    Some(total + section(rest)?)
}

fn section_or_value(c: &[char]) -> Option<u64> {
    if c.contains(&'万') {
        value(c)
    } else {
        section(c)
    }
}

fn render(v: u64) -> String {
    if v >= 100_000_000 && v.is_multiple_of(100_000_000) {
        format!("{}亿", v / 100_000_000)
    } else if v >= 10_000 && v.is_multiple_of(10_000) {
        format!("{}万", v / 10_000)
    } else {
        v.to_string()
    }
}

fn is_numeral(c: char) -> bool {
    digit(c).is_some() || magnitude(c).is_some() || c == '万' || c == '亿'
}

/// Two adjacent bare digits (两三万, 三四十) mean "about", not a number to rewrite.
fn approximate(run: &[char]) -> bool {
    run.windows(2)
        .any(|w| matches!((digit(w[0]), digit(w[1])), (Some(a), Some(b)) if a != 0 && b != 0))
}

fn numbers(s: &str) -> String {
    let c: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < c.len() {
        if !is_numeral(c[i]) {
            out.push(c[i]);
            i += 1;
            continue;
        }
        let start = i;
        while i < c.len() && is_numeral(c[i]) {
            i += 1;
        }
        let run = &c[start..i];
        let raw: String = run.iter().collect();
        let prev = start.checked_sub(1).map(|j| c[j]);
        let next = c.get(i).copied();
        // 百分之二十 → 20%
        if start >= 3
            && c[start - 3..start] == ['百', '分', '之']
            && let Some(v) = (!approximate(run)).then(|| value(run)).flatten()
        {
            out.truncate(out.len() - "百分之".len());
            out.push_str(&format!("{v}%"));
            continue;
        }
        // 百分之二十、三十 → 20%、30%
        if out.ends_with("%、")
            && let Some(v) = (!approximate(run)).then(|| value(run)).flatten()
        {
            out.push_str(&format!("{v}%"));
            continue;
        }
        let has_mag = run.iter().any(|&ch| digit(ch).is_none());
        let converted = if matches!(prev, Some('几' | '数' | '上' | '双' | '成' | '好'))
            || matches!(next, Some('几' | '来'))
            || !(digit(run[0]).is_some_and(|d| d != 0) || run[0] == '十')
        {
            None
        } else if !has_mag {
            // Digit strings: years, phone numbers. Two digits alone are usually "about" (三四个).
            (run.len() >= 3 || (run.len() >= 2 && next == Some('年'))).then(|| {
                run.iter()
                    .map(|&ch| char::from(b'0' + digit(ch).unwrap() as u8))
                    .collect()
            })
        } else if approximate(run)
            || (raw == "十" && next == Some('分') && c.get(i + 1) != Some(&'钟'))
        {
            None
        } else {
            value(run).filter(|v| *v >= 10).map(render)
        };
        out.push_str(converted.as_deref().unwrap_or(&raw));
    }
    out
}

// Spacing and period -------------------------------------------------------------------------------

fn latin(c: char) -> bool {
    c.is_ascii_alphanumeric()
}

fn spacing(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    let mut prev: Option<char> = None;
    for c in s.chars() {
        if let Some(p) = prev
            && ((is_ideographic(p) && latin(c)) || ((latin(p) || p == '%') && is_ideographic(c)))
        {
            out.push(' ');
        }
        out.push(c);
        prev = Some(c);
    }
    out
}

fn no_period(s: &str) -> String {
    let t = s.trim_end();
    if let Some(r) = t.strip_suffix('。') {
        return r.to_string();
    }
    match t.strip_suffix('.') {
        Some(r) if !r.ends_with('.') && !r.ends_with(|c: char| c.is_ascii_digit()) => r.to_string(),
        _ => s.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all() -> Rules {
        Rules {
            fillers: true,
            repeats: true,
            corrections: true,
            no_period: true,
            spacing: true,
            numbers: true,
        }
    }

    #[test]
    fn defaults_change_nothing() {
        let r = Rules::load(|_| None);
        assert_eq!(r, Rules::default());
        let s = "嗯，那个我们明天明天下午三点，不对，四点开会，预算两百万。";
        assert_eq!(r.apply(s), s);
    }

    #[test]
    fn sample() {
        assert_eq!(
            all().apply(
                "嗯，那个，我们明天明天下午三点，不对，四点开会讨论一下Q4的预算，大概要两百万。"
            ),
            "我们明天下午四点开会讨论一下 Q4 的预算，大概要 200 万"
        );
    }

    #[test]
    fn idempotent() {
        let r = all();
        for s in [
            "嗯，那个，我们明天明天下午三点，不对，四点开会讨论一下Q4的预算，大概要两百万。",
            "百分之二十的用户用 Rust 写了三千五百行。",
            "让你让你看看看看这个版本，二零二六年上线。",
        ] {
            let once = r.apply(s);
            assert_eq!(r.apply(&once), once, "{s}");
        }
    }

    #[test]
    fn fillers_narrow() {
        let f = |s| fillers(s);
        assert_eq!(f("嗯，目前没问题。"), "目前没问题。");
        assert_eq!(f("现在，呃，学到了。"), "现在，学到了。");
        assert_eq!(f("给一个嗯横的柱状图"), "给一个横的柱状图");
        assert_eq!(f("好的，嗯。"), "好的。");
        assert_eq!(f("嗯嗯，可以"), "可以");
        assert_eq!(f("你要干嘛？嗯。手镯"), "你要干嘛？手镯");
        // 那个 as a demonstrative, 额 inside words.
        assert_eq!(f("你说的那个数字是多少"), "你说的那个数字是多少");
        assert_eq!(f("是关于那个，神经网络"), "是关于那个，神经网络");
        assert_eq!(f("额度不够，额外再申请"), "额度不够，额外再申请");
        assert_eq!(all().apply("嗯。"), "嗯。");
    }

    #[test]
    fn repeats_narrow() {
        assert_eq!(repeats("我们明天明天下午"), "我们明天下午");
        assert_eq!(repeats("让你让你看"), "让你看");
        assert_eq!(repeats("我们，我们明天去"), "我们明天去");
        for keep in [
            "一闪一闪亮晶晶",
            "非常非常好",
            "介绍介绍你自己",
            "看看这个",
            "谢谢谢谢",
            "好的，好的",
            "对，对的",
            "刚刚到",
            "哈哈哈哈",
        ] {
            assert_eq!(repeats(keep), keep);
        }
    }

    #[test]
    fn corrections_narrow() {
        assert_eq!(corrections("下午三点，不对，四点开会"), "下午四点开会");
        assert_eq!(corrections("约在周三，不对，周四吧"), "约在周四吧");
        assert_eq!(corrections("大概两百万，我是说，三百万"), "大概三百万");
        assert_eq!(corrections("放在 3 楼，不是，5 楼"), "放在 5 楼");
        for keep in [
            "这不是问题，不对，是我的问题",
            "这样不对",
            "下午三点，不对，改到明天",
            "我们三个人，不是，四点到",
        ] {
            assert_eq!(corrections(keep), keep);
        }
    }

    #[test]
    fn numbers_narrow() {
        let n = |s| numbers(s);
        assert_eq!(n("两百万"), "200万");
        assert_eq!(n("三千五百行"), "3500行");
        assert_eq!(n("三百五"), "350");
        assert_eq!(n("一千零五"), "1005");
        assert_eq!(n("二十三个"), "23个");
        assert_eq!(n("十二点"), "12点");
        assert_eq!(n("十分钟"), "10分钟");
        assert_eq!(n("二零二六年"), "2026年");
        assert_eq!(n("百分之二十"), "20%");
        assert_eq!(n("百分之五"), "5%");
        assert_eq!(n("提升百分之二十、三十的部分"), "提升20%、30%的部分");
        assert_eq!(n("一亿"), "1亿");
        assert_eq!(n("一万两千"), "12000");
        for keep in [
            "讨论一下",
            "一起去",
            "万一漏了",
            "千万不要",
            "第一次",
            "三思",
            "十分重要",
            "两三万块",
            "三四十个",
            "十几个",
            "几十个",
            "双十一",
            "三个人",
            "四点开会",
            "一样",
            "上百个",
        ] {
            assert_eq!(n(keep), keep);
        }
    }

    #[test]
    fn spacing_and_period() {
        assert_eq!(spacing("用Rust写GPUI界面"), "用 Rust 写 GPUI 界面");
        assert_eq!(spacing("用 Rust 写"), "用 Rust 写");
        assert_eq!(spacing("20%的用户"), "20% 的用户");
        assert_eq!(spacing("你好，world。"), "你好，world。");
        assert_eq!(no_period("好的。"), "好的");
        assert_eq!(no_period("真的吗？"), "真的吗？");
        assert_eq!(no_period("Ship it."), "Ship it");
        assert_eq!(no_period("版本 v1.2."), "版本 v1.2.");
        assert_eq!(no_period("等等..."), "等等...");
    }
}
