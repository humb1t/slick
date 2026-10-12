//! Turns Slack's message payloads (mrkdwn, Block Kit, attachments) into plain text.

use crate::slack::Message;
use serde_json::Value;
use std::collections::HashMap;

pub struct Names<'a> {
    pub users: &'a HashMap<String, String>,
    pub chans: &'a HashMap<String, String>,
    /// The workspace's custom emoji: name → image URL.
    pub emoji: &'a HashMap<String, String>,
}

/// The flat text, for copying and previews; the conversation itself
/// is drawn by `blocks`.
pub fn body(m: &Message, n: &Names) -> String {
    if m.subtype.as_deref() == Some("huddle_thread") {
        return crate::i18n::tr("📞 started a huddle", "📞 iniciou um huddle").into();
    }
    let mut out = match m.blocks.as_array() {
        Some(b) if !b.is_empty() => blocks(b, n),
        _ => String::new(),
    };
    if out.trim().is_empty() {
        out = mrkdwn(&m.text, n);
    }
    if let Some(atts) = m.attachments.as_array() {
        for a in atts {
            let t = attachment(a, n);
            if !t.is_empty() {
                push_line(&mut out, &t.lines().map(|l| format!("▎ {l}")).collect::<Vec<_>>().join("\n"));
            }
        }
    }
    // Files are drawn by the UI: images inline, the rest as cards.
    shortcodes(&out)
}

/// `:green_heart:` → 💚 for the shortcodes that have a Unicode emoji.
pub fn shortcodes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find(':') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        match after.find(':') {
            Some(j) if j > 0 && after[..j].bytes().all(|b| b.is_ascii_alphanumeric() || b"_+-".contains(&b)) => {
                match crate::emoji::standard(&after[..j]) {
                    Some(e) => {
                        rest = &after[j + 1..];
                        // Slack writes a skin tone as a second shortcode: `:+1::skin-tone-2:`.
                        let tone = rest.get(..13).filter(|t| t.starts_with(":skin-tone-") && t.ends_with(':'));
                        match tone.and_then(|t| crate::emoji::standard(&format!("{}:{}", &after[..j], &t[..12]))) {
                            Some(toned) => {
                                out.push_str(&toned);
                                rest = &rest[13..];
                            }
                            None => out.push_str(&e),
                        }
                    }
                    None => {
                        out.push(':');
                        rest = after;
                    }
                }
            }
            _ => {
                out.push(':');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

fn push_line(out: &mut String, line: &str) {
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(line);
}

fn attachment(a: &Value, n: &Names) -> String {
    let mut out = String::new();
    if let Some(b) = a["blocks"].as_array() {
        out = blocks(b, n);
    }
    // Shared messages carry the same content as blocks and as text.
    let keys: &[&str] = if out.is_empty() { &["pretext", "title", "text"] } else { &["pretext", "title"] };
    for key in keys {
        if let Some(s) = a[key].as_str().filter(|s| !s.is_empty()) {
            push_line(&mut out, &mrkdwn(s, n));
        }
    }
    if let Some(fields) = a["fields"].as_array() {
        for f in fields {
            let (t, v) = (f["title"].as_str().unwrap_or(""), f["value"].as_str().unwrap_or(""));
            push_line(&mut out, &format!("{t}: {}", mrkdwn(v, n)));
        }
    }
    if let Some(author) = a["author_name"].as_str().filter(|s| !s.is_empty()) {
        out.insert_str(0, &format!("{author}\n"));
    }
    if out.is_empty() {
        out = mrkdwn(a["fallback"].as_str().unwrap_or(""), n);
    }
    out
}

/// Slack's mrkdwn: `<@U1|x>`, `<#C1|name>`, `<!here>`, `<url|label>` and HTML escapes.
pub fn mrkdwn(s: &str, n: &Names) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('<') {
        out.push_str(&unescape(&rest[..i]));
        let after = &rest[i + 1..];
        match after.find('>') {
            Some(j) => {
                out.push_str(&link(&after[..j], n));
                rest = &after[j + 1..];
            }
            None => {
                out.push_str(&unescape(&rest[i..]));
                rest = "";
            }
        }
    }
    out.push_str(&unescape(rest));
    out
}

pub fn link(inner: &str, n: &Names) -> String {
    let (target, label) = match inner.split_once('|') {
        Some((t, l)) => (t, unescape(l)),
        None => (inner, String::new()),
    };
    if let Some(id) = target.strip_prefix('@') {
        return format!("@{}", n.users.get(id).cloned().unwrap_or(if label.is_empty() { id.to_string() } else { label }));
    }
    if let Some(id) = target.strip_prefix('#') {
        return format!("#{}", n.chans.get(id).cloned().unwrap_or(if label.is_empty() { id.to_string() } else { label }));
    }
    if let Some(special) = target.strip_prefix('!') {
        return match special {
            "here" | "channel" | "everyone" => format!("@{special}"),
            _ if !label.is_empty() => label,
            _ => format!("@{}", special.split('^').next().unwrap_or(special)),
        };
    }
    let url = target.strip_prefix("mailto:").unwrap_or(target);
    if label.is_empty() || label == url {
        url.to_string()
    } else {
        format!("{label} ({url})")
    }
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
}

fn blocks(bs: &[Value], n: &Names) -> String {
    let mut out = String::new();
    for b in bs {
        let line = match b["type"].as_str().unwrap_or("") {
            "rich_text" => {
                let mut s = String::new();
                for el in b["elements"].as_array().into_iter().flatten() {
                    rich_block(el, n, &mut s);
                }
                s.trim_end().to_string()
            }
            "section" => {
                let mut s = text_obj(&b["text"], n);
                for f in b["fields"].as_array().into_iter().flatten() {
                    push_line(&mut s, &text_obj(f, n));
                }
                if let Some(label) = element_label(&b["accessory"], n) {
                    push_line(&mut s, &label);
                }
                s
            }
            "header" => text_obj(&b["text"], n).to_uppercase(),
            "context" => b["elements"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|e| if e["type"] == "image" { String::new() } else { text_obj(e, n) })
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" · "),
            "actions" => b["elements"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|e| element_label(e, n))
                .collect::<Vec<_>>()
                .join(" "),
            "divider" => "────────".into(),
            "image" => format!("{} {}", crate::i18n::tr("[image]", "[imagem]"), b["title"]["text"].as_str().or(b["alt_text"].as_str()).unwrap_or("")),
            "video" => format!("{} {}", crate::i18n::tr("[video]", "[vídeo]"), b["title"]["text"].as_str().unwrap_or("")),
            "file" => crate::i18n::tr("[file]", "[arquivo]").into(),
            _ => String::new(),
        };
        if !line.is_empty() {
            push_line(&mut out, &line);
        }
    }
    out
}

/// Buttons and menus, shown as `[label]` — they cannot be clicked from here.
fn element_label(e: &Value, n: &Names) -> Option<String> {
    match e["type"].as_str()? {
        "button" => Some(format!("[{}]", text_obj(&e["text"], n))),
        t if t.ends_with("_select") || t == "overflow" => {
            let label = text_obj(&e["placeholder"], n);
            Some(format!("[{} ▾]", if label.is_empty() { "…" } else { &label }))
        }
        _ => None,
    }
}

fn text_obj(t: &Value, n: &Names) -> String {
    let s = t["text"].as_str().unwrap_or("");
    if t["type"] == "mrkdwn" {
        mrkdwn(s, n)
    } else {
        s.to_string()
    }
}

fn rich_block(el: &Value, n: &Names, out: &mut String) {
    let inline = |out: &mut String| {
        for e in el["elements"].as_array().into_iter().flatten() {
            rich_inline(e, n, out);
        }
    };
    match el["type"].as_str().unwrap_or("") {
        "rich_text_list" => {
            let ordered = el["style"] == "ordered";
            let indent = "  ".repeat(el["indent"].as_u64().unwrap_or(0) as usize);
            for (i, item) in el["elements"].as_array().into_iter().flatten().enumerate() {
                let bullet = if ordered { format!("{}.", i + 1) } else { "•".into() };
                let mut s = String::new();
                for e in item["elements"].as_array().into_iter().flatten() {
                    rich_inline(e, n, &mut s);
                }
                out.push_str(&format!("{indent}{bullet} {s}\n"));
            }
        }
        "rich_text_quote" => {
            let mut s = String::new();
            inline(&mut s);
            for l in s.lines() {
                out.push_str(&format!("> {l}\n"));
            }
        }
        _ => {
            inline(out);
            if el["type"] == "rich_text_preformatted" && !out.ends_with('\n') {
                out.push('\n');
            }
        }
    }
}

fn rich_inline(e: &Value, n: &Names, out: &mut String) {
    let s = |k: &str| e[k].as_str().unwrap_or("");
    match s("type") {
        "text" => out.push_str(s("text")),
        "link" => out.push_str(if s("text").is_empty() { s("url") } else { s("text") }),
        "user" => {
            out.push('@');
            out.push_str(n.users.get(s("user_id")).map(String::as_str).unwrap_or(s("user_id")));
        }
        "channel" => {
            out.push('#');
            out.push_str(n.chans.get(s("channel_id")).map(String::as_str).unwrap_or(s("channel_id")));
        }
        "usergroup" => out.push_str(crate::i18n::tr("@group", "@grupo")),
        "broadcast" => {
            out.push('@');
            out.push_str(s("range"));
        }
        "emoji" => match emoji(s("unicode")) {
            Some(c) => out.push_str(&c),
            None => out.push_str(&format!(":{}:", s("name"))),
        },
        "date" => out.push_str(s("fallback")),
        "color" => out.push_str(s("value")),
        _ => {}
    }
}

/// Block Kit sends emoji as hex code points joined by `-`.
pub fn emoji(hex: &str) -> Option<String> {
    if hex.is_empty() {
        return None;
    }
    hex.split('-')
        .map(|h| u32::from_str_radix(h, 16).ok().and_then(char::from_u32))
        .collect()
}

// ---- search ------------------------------------------------------------

/// Lowercase without accents (João → joao), one char for one char so
/// positions in the folded name are positions in the name.
fn fold(c: char) -> char {
    let c = c.to_lowercase().next().unwrap_or(c);
    match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => 'a',
        'è' | 'é' | 'ê' | 'ë' => 'e',
        'ì' | 'í' | 'î' | 'ï' => 'i',
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' => 'o',
        'ù' | 'ú' | 'û' | 'ü' => 'u',
        'ç' => 'c',
        'ñ' => 'n',
        'ý' | 'ÿ' => 'y',
        _ => c,
    }
}

/// How `name` matches a search, like Slack's: every word of the query must
/// appear in it, at the start of a word when possible. Rank 0 when the name
/// starts with the query, 1 when every word starts a word, 2 otherwise; with
/// the matched char ranges, to draw in bold.
pub fn search(name: &str, query: &str) -> Option<(u8, Vec<(usize, usize)>)> {
    let name: Vec<char> = name.chars().map(fold).collect();
    let words: Vec<Vec<char>> = query.split_whitespace().map(|w| w.chars().map(fold).collect()).collect();
    let starts_word = |i: usize| i == 0 || !name[i - 1].is_alphanumeric();
    let mut ranges = Vec::new();
    let mut all_prefix = true;
    for w in &words {
        let at: Vec<usize> = (0..(name.len() + 1).saturating_sub(w.len())).filter(|&i| name[i..].starts_with(w)).collect();
        let i = match at.iter().find(|&&i| starts_word(i)) {
            Some(&i) => i,
            None => {
                all_prefix = false;
                *at.first()?
            }
        };
        ranges.push((i, i + w.len()));
    }
    let whole: Vec<char> = words.join(&' ');
    let rank = if name.starts_with(&whole) {
        ranges = vec![(0, whole.len())];
        0
    } else if all_prefix {
        1
    } else {
        2
    };
    Some((rank, ranges))
}
