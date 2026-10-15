//! Turns Slack's message payloads (mrkdwn, Block Kit, attachments) into plain text.

use crate::slack::Message;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

pub struct Names<'a> {
    pub users: &'a HashMap<String, String>,
    /// bot id → (name, icon) of the apps that post here.
    pub apps: &'a HashMap<String, (String, String)>,
    pub chans: &'a HashMap<String, String>,
    /// The workspace's custom emoji: name → image URL.
    pub emoji: &'a HashMap<String, String>,
    /// People and channels outside those lists (other orgs, channels we're
    /// not in), named once a message mentioned them.
    pub others: &'a HashMap<String, String>,
    /// Private channels, linked with a lock instead of `#`.
    pub private: &'a HashSet<String>,
    /// The workspace's public channels, those we're not in too.
    pub directory: &'a crate::slack::Directory,
    /// Your user id: mentions of you stand out.
    pub me: &'a str,
}

impl Names<'_> {
    pub fn user(&self, id: &str) -> Option<&str> {
        self.users.get(id).or_else(|| self.others.get(id)).map(String::as_str)
    }

    /// A channel's name: ours, the workspace's, else one looked up (`App::request_names`).
    pub fn chan(&self, id: &str) -> Option<&str> {
        self.chans.get(id).map(String::as_str).or_else(|| self.directory.name(id)).or_else(|| self.others.get(id).map(String::as_str))
    }

    /// What a channel mention shows, without its sign: the message's own
    /// label, else the channel's name, else "…" while it is looked up. A
    /// failed lookup leaves the id (`App::request_names`).
    pub fn chan_shown(&self, id: &str, label: &str) -> String {
        if !label.is_empty() {
            return label.to_string();
        }
        self.chan(id).unwrap_or("…").to_string()
    }
}

/// The flat text, for copying and previews; the conversation itself
/// is drawn by `blocks`.
pub fn body(m: &Message, n: &Names) -> String {
    body_as(m, n, false)
}

/// `body` as HTML for other programs to show: everything escaped, with only
/// the bold, italic and strikethrough the message itself has as tags.
pub fn body_html(m: &Message, n: &Names) -> String {
    body_as(m, n, true)
}

fn body_as(m: &Message, n: &Names, html: bool) -> String {
    let esc = |t: String| if html { escape(&t) } else { t };
    if m.subtype.as_deref() == Some("huddle_thread") {
        return esc(crate::i18n::tr("📞 started a huddle", "📞 iniciou um huddle").into());
    }
    let sys = system(m);
    let mut out = match m.blocks.as_array() {
        Some(b) if !b.is_empty() && sys.is_none() => blocks(b, n, html),
        _ => String::new(),
    };
    if out.trim().is_empty() {
        out = esc(mrkdwn(sys.as_deref().unwrap_or(&m.text), n));
    }
    if let Some(atts) = m.attachments.as_array() {
        for a in atts {
            let t = esc(attachment(a, n));
            if !t.is_empty() {
                push_line(&mut out, &t.lines().map(|l| format!("▎ {l}")).collect::<Vec<_>>().join("\n"));
            }
        }
    }
    // Files are drawn by the UI: images inline, the rest as cards.
    shortcodes(&out)
}

/// A system message ("set the channel topic: x") in the app's language, as
/// mrkdwn: Slack's `text` comes in the workspace's. None for anything else.
pub fn system(m: &Message) -> Option<String> {
    use crate::i18n::tr;
    let s = m.system.as_ref()?;
    let val = |v: &Option<String>| v.clone().filter(|v| !v.trim().is_empty());
    Some(match m.subtype.as_deref()? {
        "channel_topic" | "group_topic" => match val(&s.topic) {
            Some(t) => format!("{} {t}", tr("set the channel topic:", "definiu o assunto do canal:")),
            None => tr("cleared the channel topic", "limpou o assunto do canal").into(),
        },
        "channel_purpose" | "group_purpose" => match val(&s.purpose) {
            Some(t) => format!("{} {t}", tr("set the channel description:", "definiu a descrição do canal:")),
            None => tr("cleared the channel description", "limpou a descrição do canal").into(),
        },
        "channel_name" | "group_name" => {
            let new = val(&s.name)?;
            match val(&s.old_name) {
                Some(old) => trf!("renamed the channel from #{old} to #{new}", "renomeou o canal de #{old} para #{new}"),
                None => trf!("renamed the channel to #{new}", "renomeou o canal para #{new}"),
            }
        }
        "channel_join" | "group_join" => match val(&s.inviter) {
            Some(by) => trf!("was added by <@{by}>", "foi adicionado por <@{by}>"),
            None => tr("joined the channel", "entrou no canal").into(),
        },
        "channel_leave" | "group_leave" => tr("left the channel", "saiu do canal").into(),
        "channel_archive" | "group_archive" => tr("archived the channel", "arquivou o canal").into(),
        "channel_unarchive" | "group_unarchive" => tr("unarchived the channel", "desarquivou o canal").into(),
        "channel_convert_to_private" => tr("made the channel private", "tornou o canal privado").into(),
        "channel_convert_to_public" => tr("made the channel public", "tornou o canal público").into(),
        // The pinned message itself stays as its attachment.
        "pinned_item" => tr("pinned a message to this channel", "fixou uma mensagem neste canal").into(),
        _ => return None,
    })
}

/// Someone joining or leaving: a run of these is drawn as one quiet line.
/// Some(true) for a join, Some(false) for a leave.
pub fn membership(m: &Message) -> Option<bool> {
    match m.subtype.as_deref()? {
        "channel_join" | "group_join" => Some(true),
        "channel_leave" | "group_leave" => Some(false),
        _ => None,
    }
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
        out = blocks(b, n, false);
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
        return format!("@{}", n.user(id).map(String::from).unwrap_or(if label.is_empty() { id.to_string() } else { label }));
    }
    if let Some(id) = target.strip_prefix('#') {
        return format!("#{}", n.chan_shown(id, &label));
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

/// Text made safe to put in HTML.
pub fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// With `html`, escaped, and rich text keeps its bold, italic and strikethrough.
fn blocks(bs: &[Value], n: &Names, html: bool) -> String {
    let mut out = String::new();
    for b in bs {
        let line = match b["type"].as_str().unwrap_or("") {
            "rich_text" => {
                let mut s = String::new();
                for el in b["elements"].as_array().into_iter().flatten() {
                    rich_block(el, n, html, &mut s);
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
        let line = if html && b["type"] != "rich_text" { escape(&line) } else { line };
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

fn rich_block(el: &Value, n: &Names, html: bool, out: &mut String) {
    let inline = |out: &mut String| {
        for e in el["elements"].as_array().into_iter().flatten() {
            rich_inline(e, n, html, out);
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
                    rich_inline(e, n, html, &mut s);
                }
                out.push_str(&format!("{indent}{bullet} {s}\n"));
            }
        }
        "rich_text_quote" => {
            let mut s = String::new();
            inline(&mut s);
            let mark = if html { "&gt;" } else { ">" };
            for l in s.lines() {
                out.push_str(&format!("{mark} {l}\n"));
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

fn rich_inline(e: &Value, n: &Names, html: bool, out: &mut String) {
    if html {
        let mut plain = String::new();
        rich_inline(e, n, false, &mut plain);
        let mut text = escape(&plain);
        // Innermost first, so the tags close in order.
        for (k, tag) in [("strike", "s"), ("italic", "i"), ("bold", "b")] {
            if e["style"][k] == true && !text.trim().is_empty() {
                text = format!("<{tag}>{text}</{tag}>");
            }
        }
        out.push_str(&text);
        return;
    }
    let s = |k: &str| e[k].as_str().unwrap_or("");
    match s("type") {
        "text" => out.push_str(s("text")),
        "link" => out.push_str(if s("text").is_empty() { s("url") } else { s("text") }),
        "user" => {
            out.push('@');
            out.push_str(n.user(s("user_id")).unwrap_or(s("user_id")));
        }
        "channel" => {
            out.push('#');
            out.push_str(&n.chan_shown(s("channel_id"), ""));
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

/// A lowercase char without its accent (á → a).
fn fold(c: char) -> char {
    match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => 'a',
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => 'e',
        'ì' | 'í' | 'î' | 'ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' => 'i',
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => 'o',
        'ù' | 'ú' | 'û' | 'ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => 'u',
        'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => 'c',
        'ñ' | 'ń' | 'ņ' | 'ň' => 'n',
        'ý' | 'ÿ' => 'y',
        'ś' | 'ŝ' | 'ş' | 'š' | 'ș' => 's',
        'ź' | 'ż' | 'ž' => 'z',
        'ğ' | 'ĝ' | 'ġ' | 'ģ' => 'g',
        'ř' | 'ŕ' => 'r',
        'ť' | 'ţ' | 'ț' => 't',
        'ď' | 'đ' => 'd',
        'ł' | 'ĺ' | 'ľ' => 'l',
        _ => c,
    }
}

/// An accent typed apart from its letter ("a\u{301}", as macOS writes á):
/// folded away, it stays with its letter.
fn combining(c: char) -> bool {
    matches!(c, '\u{300}'..='\u{36F}' | '\u{1AB0}'..='\u{1AFF}' | '\u{1DC0}'..='\u{1DFF}' | '\u{20D0}'..='\u{20FF}' | '\u{FE20}'..='\u{FE2F}')
}

/// `s` lowercase without accents (João → joao), each folded char with the
/// position of the char of `s` it comes from: a separate accent folds to
/// nothing, a char may lowercase to two (İ → i̇).
fn fold_str(s: &str) -> (Vec<char>, Vec<u32>) {
    let (mut out, mut from) = (Vec::with_capacity(s.len()), Vec::with_capacity(s.len()));
    for (i, c) in s.chars().enumerate() {
        for l in c.to_lowercase().filter(|l| !combining(*l)) {
            out.push(fold(l));
            from.push(i.min(u32::MAX as usize) as u32);
        }
    }
    (out, from)
}

/// A range of folded chars as the range of the chars of the text they come
/// from, whole: the accents after the last one included.
fn unfold(from: &[u32], len: usize, (a, b): (usize, usize)) -> (usize, usize) {
    let start = from.get(a).map_or(len, |&i| i as usize);
    let end = match b.checked_sub(1).and_then(|l| from.get(l)) {
        Some(&last) if b > a => from[b..].iter().find(|&&i| i > last).map_or(len, |&i| i as usize),
        _ => start,
    };
    (start.min(len), end.min(len).max(start.min(len)))
}

/// How `name` matches a search, like Slack's: every word of the query must
/// appear in it, at the start of a word when possible. Rank 0 when the name
/// starts with the query, 1 when every word starts a word, 2 otherwise; with
/// the matched char ranges, to draw in bold.
pub fn search(name: &str, query: &str) -> Option<(u8, Vec<(usize, usize)>)> {
    let len = name.chars().count();
    let (name, from) = fold_str(name);
    let words: Vec<Vec<char>> = query.split_whitespace().map(|w| fold_str(w).0).collect();
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
    Some((rank, ranges.into_iter().map(|r| unfold(&from, len, r)).collect()))
}

// ---- sidebar search ------------------------------------------------------
//
// Ctrl+K's matcher, closer to Slack's than `search`: the query is cut into
// tokens at spaces and punctuation alike ("cha-do-" → cha, do), accents and
// case don't count, and how well a name matches is one of these tiers, best
// first:
//
//   0  the whole name                      general     → general
//   1  the start of the name               cha-do-     → cha-do-henrique
//   2  each token starts a word, in order  cha do      → chá-de-bebê-do-victor
//   3  each token starts a word, any order ti suporte  → suporte-ti
//   4  initials, a word's start each       cdh         → cha-do-henrique
//   5  the token anywhere                  ique        → cha-do-henrique
//   6  one typo from a word's start        henrqiue    → cha-do-henrique
//
// Initials, substrings and typos are for a single token: two or more must
// each start a word of their own. Spaces and punctuation in the query stand
// for any one of them in the name ("cha do" starts "cha-do-henrique"). A
// leading # or @ is dropped. Group DMs match by their label, the people's names.

/// How long a token must be to be matched with a typo.
const TYPO_MIN: usize = 4;
/// Longer single tokens aren't tried as initials: nobody types those, and it
/// keeps the backtracking small.
const INITIALS_MAX: usize = 12;

/// Splits folded chars into runs of letters and digits, as (start, end).
fn runs(chars: &[char]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = None;
    for i in 0..=chars.len() {
        match (start, chars.get(i).is_some_and(|c| c.is_alphanumeric())) {
            (None, true) => start = Some(i),
            (Some(s), false) => {
                out.push((s, i));
                start = None;
            }
            _ => {}
        }
    }
    out
}

/// A name folded once (`fold`) with its words, so a long list is searched
/// without folding it again at each key.
#[derive(Clone, Debug, Default)]
pub struct Folded {
    chars: Box<[char]>,
    /// For each folded char, the position of the name's char it comes from.
    from: Box<[u32]>,
    /// The name's length in chars.
    orig: usize,
    /// Each run of letters and digits, as (start, end) char positions.
    words: Box<[(u16, u16)]>,
}

impl Folded {
    pub fn new(name: &str) -> Self {
        let (chars, from) = fold_str(name);
        let words = runs(&chars).into_iter().map(|(a, b)| (a.min(u16::MAX as usize) as u16, b.min(u16::MAX as usize) as u16)).collect();
        Self { chars: chars.into(), from: from.into(), orig: name.chars().count(), words }
    }

    /// In folded chars, as many as the name has but for separate accents.
    pub fn len(&self) -> usize {
        self.chars.len()
    }

    /// A range of `rank`'s, in the name's own chars.
    fn unfold(&self, r: (usize, usize)) -> (usize, usize) {
        unfold(&self.from, self.orig, r)
    }

    fn word(&self, w: usize) -> &[char] {
        let (a, b) = self.words[w];
        &self.chars[a as usize..b as usize]
    }

    fn start(&self, w: usize) -> usize {
        self.words[w].0 as usize
    }
}

/// A query cut up once for all the names it is matched against.
pub struct Query {
    whole: Vec<char>,
    /// (start, end) in `whole`.
    tokens: Vec<(usize, usize)>,
}

impl Query {
    pub fn new(query: &str) -> Self {
        let q = query.trim().trim_start_matches(['#', '@']).trim();
        let whole = fold_str(q).0;
        let tokens = runs(&whole);
        Self { whole, tokens }
    }

    pub fn is_empty(&self) -> bool {
        self.whole.is_empty()
    }

    fn token(&self, t: usize) -> &[char] {
        let (a, b) = self.tokens[t];
        &self.whole[a..b]
    }
}

/// A space or punctuation; an emoji is a char like a letter here.
fn separator(c: char) -> bool {
    c.is_whitespace() || c.is_ascii_punctuation() || matches!(c, '–' | '—' | '·' | '…')
}

/// Same char, or any separator for any separator.
fn same(q: char, n: char) -> bool {
    q == n || (separator(q) && separator(n))
}

/// Whether `a` and `b` are at most one typo apart: a letter swapped for
/// another, two neighbours switched, one added or one missing.
fn one_typo(a: &[char], b: &[char]) -> bool {
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    let i = short.iter().zip(long).take_while(|(x, y)| x == y).count();
    match long.len() - short.len() {
        0 if i == short.len() => true,
        0 => {
            short[i + 1..] == long[i + 1..]
                || (i + 1 < short.len() && short[i] == long[i + 1] && short[i + 1] == long[i] && short[i + 2..] == long[i + 2..])
        }
        1 => short[i..] == long[i + 1..],
        _ => false,
    }
}

/// Where `tok` is in `n`: at a word's start if it is at one, else the first.
fn locate(n: &[char], tok: &[char]) -> Option<usize> {
    let mut first = None;
    for i in (0..(n.len() + 1).saturating_sub(tok.len())).filter(|&i| n[i..].starts_with(tok)) {
        if i == 0 || !n[i - 1].is_alphanumeric() {
            return Some(i);
        }
        first.get_or_insert(i);
    }
    first
}

/// How `name` matches `q`: its tier (above) or `None`; the matched ranges,
/// in folded chars (`Folded::unfold` for the name's), go to `ranges`,
/// cleared first. Nothing is
/// allocated but what `ranges` grows by.
pub fn rank(q: &Query, name: &Folded, ranges: &mut Vec<(usize, usize)>) -> Option<u8> {
    ranges.clear();
    let (n, w) = (&name.chars[..], &q.whole[..]);
    if w.is_empty() {
        return None;
    }
    if n.len() >= w.len() && w.iter().zip(n).all(|(a, b)| same(*a, *b)) {
        ranges.push((0, w.len()));
        return Some(if n.len() == w.len() { 0 } else { 1 });
    }
    // Only punctuation or symbols (an emoji): found as typed, or not at all.
    if q.tokens.is_empty() {
        let at = locate(n, w)?;
        ranges.push((at, at + w.len()));
        return Some(5);
    }
    let words = name.words.len().min(64);
    // Tier 2: each token starts the earliest word it can after the last one's.
    let mut after = 0;
    let in_order = (0..q.tokens.len()).all(|t| match (after..words).find(|&wi| name.word(wi).starts_with(q.token(t))) {
        Some(wi) => {
            ranges.push((name.start(wi), name.start(wi) + q.token(t).len()));
            after = wi + 1;
            true
        }
        None => false,
    });
    if in_order {
        return Some(2);
    }
    ranges.clear();
    if assign(q, name, 0, 0, words, ranges) {
        return Some(3);
    }
    ranges.clear();
    let single = q.tokens.len() == 1 && (2..=INITIALS_MAX).contains(&q.token(0).len());
    if single && initials(q.token(0), name, 0, words, 0, ranges) {
        return Some(4);
    }
    ranges.clear();
    // Tiers 5 and 6, a single token only: more than one must each start a
    // word of their own ("cha do" isn't "chamados" nor "Cardoso").
    if q.tokens.len() > 1 {
        return None;
    }
    let tok = q.token(0);
    if let Some(i) = locate(n, tok) {
        ranges.push((i, i + tok.len()));
        return Some(5);
    }
    if tok.len() < TYPO_MIN {
        return None;
    }
    // One typo from a word's start.
    let (wi, l) = (0..words).find_map(|wi| {
        let word = name.word(wi);
        (tok.len() - 1..=tok.len() + 1).filter(|&l| l <= word.len()).find(|&l| one_typo(tok, &word[..l])).map(|l| (wi, l))
    })?;
    ranges.push((name.start(wi), name.start(wi) + l));
    Some(6)
}

/// Tier 3: each token from `t` on starts a word not in `used`, in any order.
fn assign(q: &Query, name: &Folded, t: usize, used: u64, words: usize, ranges: &mut Vec<(usize, usize)>) -> bool {
    if t == q.tokens.len() {
        return true;
    }
    for wi in (0..words).filter(|wi| used & (1 << wi) == 0) {
        if name.word(wi).starts_with(q.token(t)) {
            ranges.push((name.start(wi), name.start(wi) + q.token(t).len()));
            if assign(q, name, t + 1, used | (1 << wi), words, ranges) {
                return true;
            }
            ranges.pop();
        }
    }
    false
}

/// Tier 4: `tok` as the starts of words from `wi` on, in order, two words
/// at least, the first word first ("pe2" → proj-ecommerce-2024, "chado" →
/// cha-do-henrique; not "dev" → chá-de-bebê-do-victor).
fn initials(tok: &[char], name: &Folded, wi: usize, words: usize, pieces: usize, ranges: &mut Vec<(usize, usize)>) -> bool {
    if tok.is_empty() {
        return pieces >= 2;
    }
    let last = if pieces == 0 { words.min(1) } else { words };
    for w in wi..last {
        let common = name.word(w).iter().zip(tok).take_while(|(a, b)| a == b).count();
        // The longest piece first.
        for l in (1..=common).rev() {
            ranges.push((name.start(w), name.start(w) + l));
            if initials(&tok[l..], name, w + 1, words, pieces + 1, ranges) {
                return true;
            }
            ranges.pop();
        }
    }
    false
}

/// Where the query's tokens start words of `text`, accents and case aside,
/// as char ranges: the words to draw in bold in a message the search found.
pub fn marks(text: &str, query: &str) -> Vec<(usize, usize)> {
    let q = Query::new(query);
    let name = Folded::new(text);
    let mut out = Vec::new();
    for t in 0..q.tokens.len() {
        let tok = q.token(t);
        out.extend((0..name.words.len()).filter(|&w| name.word(w).starts_with(tok)).map(|w| name.unfold((name.start(w), name.start(w) + tok.len()))));
    }
    out
}

/// `rank` for one name, folding it on the way: for drawing a result, the
/// ranges in the name's chars.
pub fn find(name: &str, query: &str) -> Option<(u8, Vec<(usize, usize)>)> {
    let mut ranges = Vec::new();
    let name = Folded::new(name);
    let tier = rank(&Query::new(query), &name, &mut ranges)?;
    Some((tier, ranges.into_iter().map(|r| name.unfold(r)).collect()))
}

/// Where a result goes in the search's list; smaller goes first. The rule,
/// as Slack's, for channels and DMs alike: every active conversation
/// before any archived channel; within each, a name typed whole on top,
/// then the conversations named after what was typed before group DMs that
/// only have someone by that name in them (Slack shows the person first),
/// then the last message, newest first, those without a known time last.
/// How well the rest matches only breaks ties (the match is strict enough
/// to be a filter), then yours before channels you are not in, the most
/// members, the shortest name. Fields compare in order.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Place {
    archived: bool,
    inexact: bool,
    group: bool,
    recent: std::cmp::Reverse<i64>,
    tier: u8,
    outside: bool,
    crowd: std::cmp::Reverse<u32>,
    len: usize,
}

/// `latest` is the last message's time in Unix milliseconds, 0 when
/// unknown; `members`, 0 when unknown.
pub fn place(tier: u8, archived: bool, member: bool, latest: i64, members: u32, len: usize) -> Place {
    Place {
        archived,
        inexact: tier > 0,
        group: false,
        recent: std::cmp::Reverse(latest),
        tier,
        outside: !member,
        crowd: std::cmp::Reverse(members),
        len,
    }
}

impl Place {
    /// A group DM's: after the people and channels the query names.
    pub fn group(mut self, group: bool) -> Self {
        self.group = group;
        self
    }
}

/// A Slack ts ("1700000000.000100") in Unix milliseconds; 0 when it isn't one.
pub fn ts_ms(ts: &str) -> i64 {
    let (secs, frac) = ts.split_once('.').unwrap_or((ts, ""));
    let ms: i64 = frac.get(..3).and_then(|f| f.parse().ok()).unwrap_or(0);
    secs.parse::<i64>().map_or(0, |s| s * 1000 + ms)
}
