//! Slack's markup in the message box: `*bold*`, `_italic_`, `~strike~`,
//! `` `code` ``, ```` ``` ```` blocks, `> ` quotes and `- ` / `1. ` lists.
//! One scan styles the box while typing and builds the `rich_text` blocks a
//! message is sent with, as the official clients send it; editing a message
//! turns its blocks back into this markup.

use crate::slack::slack_ref;
use serde_json::{json, Map, Value};

/// Per-character looks from `scan`.
pub const MARK: u16 = 1;
pub const BOLD: u16 = 2;
pub const ITALIC: u16 = 4;
pub const STRIKE: u16 = 8;
pub const CODE: u16 = 16;
/// Inside a ``` block, fences included.
pub const PRE: u16 = 32;
/// The `>` of a quote line (and its space).
pub const QUOTE: u16 = 64;
/// The `-` or `*` of a list item, and its indent.
pub const BULLET: u16 = 128;
/// The `1.` of a numbered item.
pub const NUMBER: u16 = 256;
const INLINE: u16 = BOLD | ITALIC | STRIKE | CODE;

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Quote,
    Bullet(u8),
    Ordered(u8, u64),
}

/// A line's quote or list prefix: what it is and how many chars it takes.
fn prefix(line: &[char]) -> Option<(Kind, usize)> {
    if line.first() == Some(&'>') {
        return Some((Kind::Quote, if line.get(1) == Some(&' ') { 2 } else { 1 }));
    }
    let sp = line.iter().take_while(|c| **c == ' ').count();
    let indent = (sp / 2).min(8) as u8;
    let rest = &line[sp..];
    if matches!(rest.first(), Some('-' | '*' | '•')) && rest.get(1) == Some(&' ') {
        return Some((Kind::Bullet(indent), sp + 2));
    }
    let digits = rest.iter().take_while(|c| c.is_ascii_digit()).count();
    if (1..=3).contains(&digits) && rest.get(digits) == Some(&'.') && rest.get(digits + 1) == Some(&' ') {
        let n = rest[..digits].iter().collect::<String>().parse().unwrap_or(1);
        return Some((Kind::Ordered(indent, n), sp + digits + 2));
    }
    None
}

/// The ``` blocks as (start, end, closed): from the opening fence to past the
/// closing one. An unclosed fence is a block to the end only when `open`
/// (the box, while typing); Slack shows it as text.
fn regions(c: &[char], open: bool) -> Vec<(usize, usize, bool)> {
    let mut fences = Vec::new();
    let mut i = 0;
    while i + 3 <= c.len() {
        if c[i..i + 3] == ['`'; 3] {
            fences.push(i);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut out: Vec<_> = fences.chunks(2).filter(|p| p.len() == 2).map(|p| (p[0], p[1] + 3, true)).collect();
    if open && fences.len() % 2 == 1 {
        out.push((fences[fences.len() - 1], c.len(), false));
    }
    out
}

/// A mention, channel or link written out (`<@U1>`), or a bare URL: taken
/// whole, so no marker counts inside it.
fn atom(c: &[char], i: usize, b: usize) -> Option<usize> {
    if c[i] == '<' {
        let end = c[i..b].iter().position(|&x| x == '>')?;
        let s: String = c[i..=i + end].iter().collect();
        return slack_ref(&s).map(|_| end + 1);
    }
    if i > 0 && c[i - 1].is_alphanumeric() {
        return None;
    }
    let starts = |p: &str| p.chars().enumerate().all(|(k, ch)| c.get(i + k) == Some(&ch) && i + k < b);
    if !(starts("http://") || starts("https://")) {
        return None;
    }
    let mut n = c[i..b].iter().position(|x| x.is_whitespace() || matches!(x, '<' | '>')).unwrap_or(b - i);
    let opens = c[i..i + n].contains(&'(');
    while n > 0 && (".,;:!?*_~'\"`".contains(c[i + n - 1]) || (c[i + n - 1] == ')' && !opens)) {
        n -= 1;
    }
    Some(n)
}

/// Looks for each char of `c`: markers, inline styles, code blocks and the
/// prefixes of quote and list lines.
pub fn scan(c: &[char], open: bool) -> Vec<u16> {
    let mut f = vec![0u16; c.len()];
    let mut i = 0;
    for (a, b, closed) in regions(c, open) {
        lines(c, i, a, &mut f);
        for k in a..b {
            f[k] = PRE;
        }
        for k in a..a + 3 {
            f[k] |= MARK;
        }
        if closed {
            for k in b - 3..b {
                f[k] |= MARK;
            }
        }
        i = b;
    }
    lines(c, i, c.len(), &mut f);
    f
}

/// The lines between `a` and `b`; a prefix counts only at a line's start.
fn lines(c: &[char], a: usize, b: usize, f: &mut [u16]) {
    let mut s = a;
    loop {
        let e = c[s..b].iter().position(|&x| x == '\n').map_or(b, |p| s + p);
        let mut from = s;
        if s == 0 || c[s - 1] == '\n' {
            if let Some((kind, n)) = prefix(&c[s..e]) {
                let look = match kind {
                    Kind::Quote => QUOTE | MARK,
                    Kind::Bullet(_) => BULLET,
                    Kind::Ordered(..) => NUMBER,
                };
                f[s..s + n].fill(look);
                from = s + n;
            }
        }
        inline(c, from, e, 0, f);
        if e >= b {
            break;
        }
        s = e + 1;
    }
}

/// One line's inline marks; like Slack, a marker only counts at a word's
/// edge, so `snake_case` and `2*3*4` stay as they are.
fn inline(c: &[char], a: usize, b: usize, style: u16, f: &mut [u16]) {
    let mut prev = a.checked_sub(1).map(|p| c[p]);
    let mut i = a;
    while i < b {
        if let Some(n) = atom(c, i, b).filter(|n| *n > 0) {
            f[i..i + n].fill(style);
            i += n;
            prev = Some(c[i - 1]);
            continue;
        }
        let ch = c[i];
        let edge = prev.is_none_or(|p| !p.is_alphanumeric());
        let found = match ch {
            '`' => c[i + 1..b].iter().position(|&x| x == '`').map(|p| i + 1 + p).filter(|&j| j > i + 1),
            '*' | '_' | '~' if edge && i + 1 < b && !c[i + 1].is_whitespace() => closing(c, i + 1, b, ch),
            _ => None,
        };
        match found {
            Some(j) => {
                f[i] = style | MARK;
                f[j] = style | MARK;
                match ch {
                    '`' => f[i + 1..j].fill(style | CODE),
                    '*' => inline(c, i + 1, j, style | BOLD, f),
                    '_' => inline(c, i + 1, j, style | ITALIC, f),
                    _ => inline(c, i + 1, j, style | STRIKE, f),
                }
                i = j + 1;
            }
            None => {
                f[i] = style;
                i += 1;
            }
        }
        prev = Some(ch);
    }
}

/// Where the closing `mark` is: after a non-space, before a non-word.
fn closing(c: &[char], start: usize, b: usize, mark: char) -> Option<usize> {
    let mut prev = None;
    let mut j = start;
    while j < b {
        if let Some(n) = atom(c, j, b).filter(|n| *n > 0) {
            j += n;
            prev = Some(c[j - 1]);
            continue;
        }
        let ch = c[j];
        if ch == mark && j > start && prev.is_some_and(|p: char| !p.is_whitespace()) && c.get(j + 1).is_none_or(|x| !x.is_alphanumeric()) {
            return Some(j);
        }
        prev = Some(ch);
        j += 1;
    }
    None
}

// ---- sending -----------------------------------------------------------------

/// What the box holds (tokens written out, not escaped) as the `blocks` of
/// `chat.postMessage` / `chat.update`: one `rich_text`, like Slack's own.
pub fn blocks(text: &str) -> Value {
    let c: Vec<char> = text.chars().collect();
    let f = scan(&c, false);
    let mut els = Vec::new();
    let mut i = 0;
    for (a, b, _) in regions(&c, false) {
        outside(&c, &f, i, a, &mut els);
        let (mut s, mut e) = (a + 3, b - 3);
        if s < e && c[s] == '\n' {
            s += 1;
        }
        if s < e && c[e - 1] == '\n' {
            e -= 1;
        }
        let items = items(&c, &f, s, e, true);
        if !items.is_empty() {
            els.push(json!({ "type": "rich_text_preformatted", "elements": items, "border": 0 }));
        }
        i = b;
    }
    outside(&c, &f, i, c.len(), &mut els);
    json!([{ "type": "rich_text", "elements": els }])
}

/// A stretch between code blocks: sections, quotes and lists, line by line.
fn outside(c: &[char], f: &[u16], a: usize, b: usize, els: &mut Vec<Value>) {
    let (mut a, mut b) = (a, b);
    if a > 0 && a < b && c[a] == '\n' {
        a += 1;
    }
    if b < c.len() && b > a && c[b - 1] == '\n' {
        b -= 1;
    }
    if a >= b {
        return;
    }
    // The group being built: plain lines, a quote or a list.
    let mut group: Option<(Option<Kind>, Vec<Value>)> = None;
    let flush = |group: &mut Option<(Option<Kind>, Vec<Value>)>, els: &mut Vec<Value>| {
        let Some((kind, items)) = group.take() else { return };
        match kind {
            // Slack refuses empty sections, quotes and lists.
            _ if items.is_empty() => {}
            None => els.push(json!({ "type": "rich_text_section", "elements": items })),
            Some(Kind::Quote) => els.push(json!({ "type": "rich_text_quote", "elements": items })),
            Some(Kind::Bullet(indent)) => {
                els.push(json!({ "type": "rich_text_list", "style": "bullet", "indent": indent, "border": 0, "elements": items }))
            }
            Some(Kind::Ordered(indent, first)) => {
                let mut l = json!({ "type": "rich_text_list", "style": "ordered", "indent": indent, "border": 0, "elements": items });
                if first > 1 {
                    l["offset"] = json!(first - 1);
                }
                els.push(l);
            }
        }
    };
    let mut s = a;
    loop {
        let e = c[s..b].iter().position(|&x| x == '\n').map_or(b, |p| s + p);
        let found = (s == 0 || c[s - 1] == '\n').then(|| prefix(&c[s..e])).flatten();
        let (kind, from) = match found {
            Some((k, n)) => (Some(k), s + n),
            None => (None, s),
        };
        let line = items(c, f, from, e, false);
        let same = match (&group, kind) {
            (Some((None, _)), None) | (Some((Some(Kind::Quote), _)), Some(Kind::Quote)) => true,
            (Some((Some(Kind::Bullet(x)), _)), Some(Kind::Bullet(y))) => *x == y,
            (Some((Some(Kind::Ordered(x, _)), _)), Some(Kind::Ordered(y, _))) => *x == y,
            _ => false,
        };
        match kind {
            // A list's items are sections of their own.
            Some(Kind::Bullet(_) | Kind::Ordered(..)) => {
                if !same {
                    flush(&mut group, els);
                    group = Some((kind, Vec::new()));
                }
                if let Some((_, list)) = group.as_mut().filter(|_| !line.is_empty()) {
                    list.push(json!({ "type": "rich_text_section", "elements": line }));
                }
            }
            _ => {
                if same {
                    if let Some((_, items)) = group.as_mut() {
                        push_text(items, "\n", 0);
                        join(items, line);
                    }
                } else {
                    flush(&mut group, els);
                    group = Some((kind, line));
                }
            }
        }
        if e >= b {
            break;
        }
        s = e + 1;
    }
    flush(&mut group, els);
}

/// Appends elements, a text merging into the text before it in the same style.
fn join(items: &mut Vec<Value>, more: Vec<Value>) {
    for v in more {
        match (v["type"].as_str(), v["text"].as_str()) {
            (Some("text"), Some(t)) => push_text(items, t, bits(&v["style"])),
            _ => items.push(v),
        }
    }
}

fn bits(style: &Value) -> u16 {
    [("bold", BOLD), ("italic", ITALIC), ("strike", STRIKE), ("code", CODE)]
        .iter()
        .filter(|(k, _)| style[*k] == true)
        .fold(0, |acc, (_, b)| acc | b)
}

fn style_of(bits: u16) -> Option<Value> {
    let mut m = Map::new();
    for (k, b) in [("bold", BOLD), ("italic", ITALIC), ("strike", STRIKE), ("code", CODE)] {
        if bits & b != 0 {
            m.insert(k.into(), Value::Bool(true));
        }
    }
    (!m.is_empty()).then_some(Value::Object(m))
}

fn with_style(mut v: Value, bits: u16) -> Value {
    if let Some(s) = style_of(bits) {
        v["style"] = s;
    }
    v
}

fn push_text(items: &mut Vec<Value>, t: &str, look: u16) {
    if t.is_empty() {
        return;
    }
    if let Some(last) = items.last_mut().filter(|l| l["type"] == "text" && bits(&l["style"]) == look) {
        let joined = format!("{}{t}", last["text"].as_str().unwrap_or(""));
        last["text"] = Value::String(joined);
        return;
    }
    items.push(with_style(json!({ "type": "text", "text": t }), look));
}

/// The elements of `a..b`, markers left out; in a code block (`literal`)
/// only mentions and links are picked out.
fn items(c: &[char], f: &[u16], a: usize, b: usize, literal: bool) -> Vec<Value> {
    let mut out = Vec::new();
    let mut run = String::new();
    let mut style = 0;
    for k in a..b {
        if f[k] & MARK != 0 {
            continue;
        }
        let s = if literal { 0 } else { f[k] & INLINE };
        if s != style && !run.is_empty() {
            emit(&mut out, &std::mem::take(&mut run), style, literal);
        }
        style = s;
        run.push(c[k]);
    }
    emit(&mut out, &run, style, literal);
    out
}

fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || "_+-'".contains(c)
}

/// A run in one style: its mentions, channels, links and `:emoji:` become
/// their own elements.
fn emit(out: &mut Vec<Value>, s: &str, style: u16, literal: bool) {
    let c: Vec<char> = s.chars().collect();
    let mut plain = String::new();
    let mut i = 0;
    while i < c.len() {
        let prev = i.checked_sub(1).map(|p| c[p]);
        if let Some(n) = atom(&c, i, c.len()).filter(|n| *n > 0) {
            push_text(out, &std::mem::take(&mut plain), style);
            let raw: String = c[i..i + n].iter().collect();
            out.push(match raw.strip_prefix('<').and_then(|r| r.strip_suffix('>')) {
                Some(inner) => reference(inner, style),
                None => with_style(json!({ "type": "link", "url": raw }), style),
            });
            i += n;
            continue;
        }
        let emoji = (!literal && style & CODE == 0 && c[i] == ':' && prev.is_none_or(|p| !p.is_alphanumeric()))
            .then(|| shortcode(&c[i + 1..]))
            .flatten();
        if let Some((name, tone, n)) = emoji {
            push_text(out, &std::mem::take(&mut plain), style);
            let mut e = json!({ "type": "emoji", "name": name });
            if let Some(t) = tone {
                e["skin_tone"] = json!(t);
            }
            out.push(with_style(e, style));
            i += 1 + n;
            continue;
        }
        plain.push(c[i]);
        i += 1;
    }
    push_text(out, &plain, style);
}

/// `name:` (or `name::skin-tone-N:`) after a `:`: the name, the tone and the
/// chars taken.
fn shortcode(c: &[char]) -> Option<(String, Option<u64>, usize)> {
    let n = c.iter().take_while(|x| is_name_char(**x)).count();
    if n == 0 || c.get(n) != Some(&':') {
        return None;
    }
    let name: String = c[..n].iter().collect();
    let tail: String = c[n..].iter().take(14).collect();
    if let Some(t) = tail.strip_prefix("::skin-tone-").and_then(|t| t.strip_suffix(':')).and_then(|t| t.parse().ok()) {
        return Some((name, Some(t), n + 14));
    }
    Some((name, None, n + 1))
}

/// What is between `<` and `>`: a person, a channel, `@here` and the like.
fn reference(inner: &str, style: u16) -> Value {
    let (target, label) = inner.split_once('|').unwrap_or((inner, ""));
    let v = if let Some(id) = target.strip_prefix('@') {
        json!({ "type": "user", "user_id": id })
    } else if let Some(id) = target.strip_prefix('#') {
        json!({ "type": "channel", "channel_id": id })
    } else {
        let special = target.trim_start_matches('!');
        match special.split_once('^') {
            Some(("subteam", id)) => json!({ "type": "usergroup", "usergroup_id": id }),
            Some(_) => json!({ "type": "text", "text": if label.is_empty() { special } else { label } }),
            None => json!({ "type": "broadcast", "range": special }),
        }
    };
    with_style(v, style)
}

// ---- editing -------------------------------------------------------------------

/// A message's `rich_text` blocks as this markup, Slack-escaped like a
/// message's text (`<@U1>`, `&lt;`), for the box to load. None when the
/// blocks are something else.
pub fn markup(blocks: &Value) -> Option<String> {
    let bs = blocks.as_array().filter(|b| !b.is_empty())?;
    if bs.iter().any(|b| b["type"] != "rich_text") {
        return None;
    }
    let mut parts: Vec<String> = Vec::new();
    for el in bs.iter().flat_map(|b| b["elements"].as_array().into_iter().flatten()) {
        let elements = &el["elements"];
        match el["type"].as_str().unwrap_or("") {
            "rich_text_quote" => {
                let t = written(elements, false);
                let t = t.strip_suffix('\n').unwrap_or(&t);
                parts.push(t.split('\n').map(|l| format!("> {l}")).collect::<Vec<_>>().join("\n"));
            }
            "rich_text_preformatted" => {
                let t = written(elements, true);
                parts.push(if t.contains('\n') { format!("```\n{t}\n```") } else { format!("```{t}```") });
            }
            "rich_text_list" => {
                let indent = "  ".repeat(el["indent"].as_u64().unwrap_or(0) as usize);
                let offset = el["offset"].as_u64().unwrap_or(0);
                let ordered = el["style"] == "ordered";
                for (k, item) in elements.as_array().into_iter().flatten().enumerate() {
                    let mark = if ordered { format!("{}. ", offset + k as u64 + 1) } else { "- ".into() };
                    let t = written(&item["elements"], false);
                    parts.push(format!("{indent}{mark}{}", t.trim_end_matches('\n')));
                }
            }
            _ => {
                let t = written(elements, false);
                parts.push(t.strip_suffix('\n').unwrap_or(&t).to_string());
            }
        }
    }
    Some(parts.join("\n"))
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Inline elements written out, each run of one style inside its markers.
fn written(elements: &Value, literal: bool) -> String {
    let mut runs: Vec<(String, u16)> = Vec::new();
    for e in elements.as_array().into_iter().flatten() {
        let s = |k: &str| e[k].as_str().unwrap_or("").to_string();
        let text = match e["type"].as_str().unwrap_or("") {
            "text" => esc(&s("text")),
            "link" if s("text").is_empty() || s("text") == s("url") => format!("<{}>", s("url")),
            "link" => format!("<{}|{}>", s("url"), esc(&s("text"))),
            "user" => format!("<@{}>", s("user_id")),
            "channel" => format!("<#{}>", s("channel_id")),
            "usergroup" => format!("<!subteam^{}>", s("usergroup_id")),
            "broadcast" => format!("<!{}>", s("range")),
            "emoji" => match e["skin_tone"].as_u64() {
                Some(t) => format!(":{}::skin-tone-{t}:", s("name")),
                None => format!(":{}:", s("name")),
            },
            "date" => esc(&s("fallback")),
            "color" => s("value"),
            _ => continue,
        };
        let style = if literal { 0 } else { bits(&e["style"]) };
        match runs.last_mut() {
            Some((t, st)) if *st == style => t.push_str(&text),
            _ => runs.push((text, style)),
        }
    }
    let mut out = String::new();
    for (t, style) in runs {
        let marks: String = [(BOLD, '*'), (ITALIC, '_'), (STRIKE, '~'), (CODE, '`')].iter().filter(|(b, _)| style & b != 0).map(|(_, m)| *m).collect();
        let core = t.trim();
        if marks.is_empty() || core.is_empty() {
            out.push_str(&t);
            continue;
        }
        let lead = &t[..t.len() - t.trim_start().len()];
        let trail = &t[t.trim_end().len()..];
        let close: String = marks.chars().rev().collect();
        out.push_str(&format!("{lead}{marks}{core}{close}{trail}"));
    }
    out
}

// ---- the box -------------------------------------------------------------------

/// The text before the cursor leaves a ``` block open: Enter breaks the line.
pub fn in_block(before: &[char]) -> bool {
    regions(before, true).last().is_some_and(|r| !r.2)
}

/// What a new line under `line` starts with to stay in its quote or list
/// (the next number), and how long `line`'s own prefix is.
pub fn continued(line: &[char]) -> Option<(String, usize)> {
    let (kind, n) = prefix(line)?;
    let next = match kind {
        Kind::Quote => "> ".to_string(),
        Kind::Bullet(_) => line[..n].iter().collect(),
        Kind::Ordered(_, num) => {
            let sp = line.iter().take_while(|c| **c == ' ').count();
            format!("{}{}. ", " ".repeat(sp), num + 1)
        }
    };
    Some((next, n))
}
