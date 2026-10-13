//! Block Kit, attachments and mrkdwn laid out the way Slack draws them:
//! styled text, fields in two columns, dividers, images and buttons that
//! can be clicked. `text::body` stays the flat version for everything else.

use crate::app::ImgRef;
use crate::i18n::tr;
use crate::slack::Message;
use crate::text::{self, Names};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;
use std::hash::{Hash, Hasher};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Nerd Font half circles: the round ends of a button.
pub(crate) const CAP_L: &str = "\u{e0b6}";
pub(crate) const CAP_R: &str = "\u{e0b4}";
/// Narrower than this, the accessory goes under the text instead of beside it.
const MIN_TEXT: usize = 24;

fn dim() -> Style {
    Style::new().fg(Color::DarkGray)
}

fn link_style() -> Style {
    Style::new().fg(Color::Blue).underlined()
}

/// Slack's mention pills: light blue on a dim blue; yours, and `@here`
/// and the like, in its yellow highlight.
const PILL_FG: Color = Color::Rgb(0x6C, 0xC4, 0xF0);
const PILL_BG: Color = Color::Rgb(0x15, 0x34, 0x4B);
const PILL_MINE_FG: Color = Color::Rgb(0xF2, 0xC7, 0x44);
const PILL_MINE_BG: Color = Color::Rgb(0x4A, 0x3E, 0x12);
const LOCK: &str = "\u{f023}";

fn code() -> Style {
    Style::new().fg(crate::ui::CODE_FG).bg(crate::ui::CODE_BG)
}

/// Inline code in a thin box: a cell of fill each side, with an
/// eighth-block line on its outer edge.
fn code_pill(seg: Seg, out: &mut Vec<Seg>) {
    // The wires sit on the cell edges next to the text: no padding.
    let edge = Style::new().fg(CODE_BORDER);
    out.push(Seg::new("\u{2595}", edge));
    out.push(seg);
    out.push(Seg::new("\u{258f}", edge));
}

fn code_block() -> Style {
    Style::new().fg(Color::Gray)
}

/// A code block's frame, subtle on its fill, and the copy button in it.
const CODE_BORDER: Color = Color::Indexed(240);
const COPY: &str = "\u{f0c5}";

/// A run of text in one style; links keep where they go.
#[derive(Clone, Default)]
pub struct Seg {
    pub text: String,
    pub style: Style,
    pub url: Option<String>,
    /// A custom emoji drawn over the two cells of `text`.
    pub emoji: Option<ImgRef>,
    /// Where a click on a mention goes.
    pub target: Option<Target>,
}

impl Seg {
    fn new(text: impl Into<String>, style: Style) -> Self {
        Self { text: text.into(), style, ..Default::default() }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum ParaKind {
    Plain,
    Quote,
    Code,
}

/// A paragraph; it may still hold `\n`, which breaks the line.
#[derive(Clone)]
pub struct Para {
    pub kind: ParaKind,
    pub segs: Vec<Seg>,
}

impl Para {
    fn plain(segs: Vec<Seg>) -> Self {
        Self { kind: ParaKind::Plain, segs }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum Look {
    Default,
    Primary,
    Danger,
    /// Selects, overflow menus and date pickers.
    Menu,
}

/// A button or menu, keeping what Slack needs to answer a click
/// (`blocks.actions`) for when the app learns to do it.
#[derive(Clone)]
#[allow(dead_code)]
pub struct Button {
    pub label: String,
    pub look: Look,
    pub url: Option<String>,
    pub action_id: String,
    pub block_id: String,
    pub value: String,
    /// The message it belongs to.
    pub ts: String,
}

pub enum Accessory {
    Button(Button),
    Image(ImgRef),
}

pub enum Piece {
    /// Paragraphs, with a button or image on their right.
    Text(Vec<Para>, Option<Accessory>),
    /// A section's fields, two per row.
    Fields(Vec<Vec<Para>>),
    Divider,
    Controls(Vec<Button>),
    Image { img: ImgRef, w: u32, h: u32, title: String },
    /// A legacy attachment: its own pieces behind a colored bar.
    Bar(Color, Vec<Piece>),
}

/// What a click on a message does.
#[derive(Clone)]
pub enum Target {
    Link(String),
    Button(Button),
    /// A mentioned person: opens the DM.
    User(String),
    Channel(String),
    /// A code block's copy button: the text it copies.
    Copy(String),
}

/// Where an image goes, in cells from the top left of the laid-out text.
pub struct Pic {
    pub line: usize,
    pub x: u16,
    pub cols: u16,
    pub rows: u16,
    pub img: ImgRef,
}

/// A clickable stretch of one line.
pub struct Hit {
    pub line: usize,
    pub x: u16,
    pub w: u16,
    pub target: Target,
}

#[derive(Default)]
pub struct Out {
    pub lines: Vec<Line<'static>>,
    pub pics: Vec<Pic>,
    pub hits: Vec<Hit>,
}

// ---- parsing ---------------------------------------------------------------

pub fn parse(m: &Message, n: &Names) -> Vec<Piece> {
    if m.subtype.as_deref() == Some("huddle_thread") {
        return vec![Piece::Text(vec![Para::plain(vec![Seg::new(tr("📞 started a huddle", "📞 iniciou um huddle"), Style::new())])], None)];
    }
    // System messages are written here, in the app's language, not Slack's.
    let sys = text::system(m);
    let mut out = if sys.is_some() { Vec::new() } else { m.blocks.as_array().map(|b| blocks(b, n, &m.ts)).unwrap_or_default() };
    if out.is_empty() {
        let paras = mrkdwn(sys.as_deref().unwrap_or(&m.text), n);
        if !blank(&paras) {
            out.push(Piece::Text(paras, None));
        }
    }
    for a in m.attachments.as_array().into_iter().flatten() {
        out.extend(attachment(a, n, &m.ts));
    }
    // Images are drawn by the UI; other files get a line.
    out
}

/// "Ana aceitou o convite" under Ana's name: Slack drops the repeat.
pub fn drop_author(pieces: &mut [Piece], author: &str) {
    let Some(Piece::Text(paras, _)) = pieces.first_mut() else { return };
    let Some(seg) = paras.first_mut().and_then(|p| p.segs.first_mut()) else { return };
    if let Some(rest) = seg.text.strip_prefix(author).and_then(|r| r.strip_prefix(' ')) {
        if rest.starts_with(char::is_lowercase) {
            seg.text = rest.to_string();
        }
    }
}

/// No text and no custom emoji (whose two cells are a space).
fn blank(paras: &[Para]) -> bool {
    paras.iter().all(|p| p.segs.iter().all(|s| s.emoji.is_none() && s.text.trim().is_empty()))
}

fn elements(v: &Value) -> impl Iterator<Item = &Value> {
    v["elements"].as_array().into_iter().flatten()
}

fn str_of(v: &Value) -> String {
    v.as_str().unwrap_or("").to_string()
}

fn blocks(bs: &[Value], n: &Names, ts: &str) -> Vec<Piece> {
    let mut out = Vec::new();
    for b in bs {
        let block_id = b["block_id"].as_str().unwrap_or("");
        match b["type"].as_str().unwrap_or("") {
            "rich_text" => {
                let paras = rich_text(b, n);
                if !blank(&paras) {
                    out.push(Piece::Text(paras, None));
                }
            }
            "section" => {
                let paras = text_obj(&b["text"], n);
                let acc = accessory(&b["accessory"], n, block_id, ts);
                if !blank(&paras) || acc.is_some() {
                    out.push(Piece::Text(paras, acc));
                }
                let fields: Vec<_> = b["fields"].as_array().into_iter().flatten().map(|f| text_obj(f, n)).collect();
                if !fields.is_empty() {
                    out.push(Piece::Fields(fields));
                }
            }
            "header" => {
                let mut paras = text_obj(&b["text"], n);
                restyle(&mut paras, |s| s.bold());
                out.push(Piece::Text(paras, None));
            }
            "context" => {
                let mut segs = Vec::new();
                for e in elements(b).filter(|e| e["type"] != "image") {
                    if !segs.is_empty() {
                        segs.push(Seg::new("  ", Style::new()));
                    }
                    for (i, p) in text_obj(e, n).into_iter().enumerate() {
                        if i > 0 {
                            segs.push(Seg::new(" ", Style::new()));
                        }
                        segs.extend(p.segs);
                    }
                }
                let mut paras = vec![Para::plain(segs)];
                if !blank(&paras) {
                    restyle(&mut paras, |s| if s.fg.is_none() { s.fg(Color::DarkGray) } else { s });
                    out.push(Piece::Text(paras, None));
                }
            }
            "actions" => {
                let bs: Vec<_> = elements(b).filter_map(|e| control(e, n, block_id, ts)).collect();
                if !bs.is_empty() {
                    out.push(Piece::Controls(bs));
                }
            }
            "divider" => out.push(Piece::Divider),
            "image" => {
                if let Some(url) = image_url(b) {
                    let title = plain(&b["title"], n);
                    let name = if title.is_empty() { b["alt_text"].as_str().unwrap_or(tr("image", "imagem")).to_string() } else { title.clone() };
                    let size = |k: &str| b[k].as_u64().unwrap_or(0) as u32;
                    out.push(Piece::Image { img: block_img(&url, &name), w: size("image_width"), h: size("image_height"), title });
                }
            }
            "video" => {
                let url = b["title_url"].as_str().or(b["video_url"].as_str()).map(String::from);
                let seg = Seg { text: format!("▶ {}", plain(&b["title"], n)), style: link_style(), url, ..Default::default() };
                out.push(Piece::Text(vec![Para::plain(vec![seg])], None));
            }
            "file" => out.push(Piece::Text(vec![Para::plain(vec![Seg::new(tr("📎 file", "📎 arquivo"), dim())])], None)),
            _ => {}
        }
    }
    out
}

fn restyle(paras: &mut [Para], f: impl Fn(Style) -> Style) {
    for s in paras.iter_mut().flat_map(|p| p.segs.iter_mut()) {
        s.style = f(s.style);
    }
}

fn text_obj(t: &Value, n: &Names) -> Vec<Para> {
    let s = t["text"].as_str().unwrap_or("");
    if t["type"] == "mrkdwn" {
        mrkdwn(s, n)
    } else {
        s.split('\n').map(|l| Para::plain(vec![Seg::new(text::shortcodes(l), Style::new())])).collect()
    }
}

/// The flat text of a text object; legacy attachments give a bare string.
fn plain(t: &Value, n: &Names) -> String {
    if let Some(s) = t.as_str() {
        return s.to_string();
    }
    let s = t["text"].as_str().unwrap_or("");
    text::shortcodes(&if t["type"] == "mrkdwn" { text::mrkdwn(s, n) } else { s.to_string() })
}

fn image_url(e: &Value) -> Option<String> {
    e["image_url"].as_str().or(e["slack_file"]["url"].as_str()).map(String::from)
}

/// Block images have no file id; the URL's hash names them on disk.
fn block_img(url: &str, name: &str) -> ImgRef {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut h);
    let ext = url
        .split(['?', '#'])
        .next()
        .and_then(|p| p.rsplit_once('.'))
        .map(|(_, e)| e)
        .filter(|e| e.len() <= 4 && e.chars().all(|c| c.is_ascii_alphanumeric()))
        .map(|e| format!(".{e}"))
        .unwrap_or_default();
    ImgRef { key: format!("blk-{:016x}{ext}", h.finish()), url: url.to_string(), name: name.to_string(), link: Some(url.to_string()) }
}

fn accessory(e: &Value, n: &Names, block_id: &str, ts: &str) -> Option<Accessory> {
    if e["type"] == "image" {
        let alt = e["alt_text"].as_str().filter(|s| !s.is_empty()).unwrap_or(tr("image", "imagem"));
        return image_url(e).map(|u| Accessory::Image(block_img(&u, alt)));
    }
    control(e, n, block_id, ts).map(Accessory::Button)
}

/// Buttons and menus; the menus show their current value or placeholder.
fn control(e: &Value, n: &Names, block_id: &str, ts: &str) -> Option<Button> {
    let or = |s: String, default: &str| if s.is_empty() { default.to_string() } else { s };
    let placeholder = |default: &str| or(plain(&e["placeholder"], n), default);
    let (label, look) = match e["type"].as_str()? {
        "button" => {
            let look = match e["style"].as_str() {
                Some("primary") => Look::Primary,
                Some("danger") => Look::Danger,
                _ => Look::Default,
            };
            (or(plain(&e["text"], n), "…"), look)
        }
        "overflow" => ("⋯".into(), Look::Menu),
        "datepicker" => (or(str_of(&e["initial_date"]), &placeholder(tr("Pick a date", "Escolher data"))), Look::Menu),
        "timepicker" => (or(str_of(&e["initial_time"]), &placeholder(tr("Pick a time", "Escolher hora"))), Look::Menu),
        t if t.ends_with("_select") => (or(plain(&e["initial_option"]["text"], n), &placeholder(tr("Select", "Escolher"))), Look::Menu),
        _ => return None,
    };
    let action_id = e["action_id"].as_str().or(e["name"].as_str()).unwrap_or("").to_string();
    Some(Button {
        label,
        look,
        url: e["url"].as_str().filter(|u| !u.is_empty()).map(String::from),
        action_id,
        block_id: block_id.to_string(),
        value: str_of(&e["value"]),
        ts: ts.to_string(),
    })
}

fn attachment(a: &Value, n: &Names, ts: &str) -> Vec<Piece> {
    let get = |k: &str| a[k].as_str().filter(|s| !s.is_empty());
    let line = |segs: Vec<Seg>| Piece::Text(vec![Para::plain(segs)], None);
    let mut out = Vec::new();
    // As in Slack, the pretext sits above the bar.
    if let Some(p) = get("pretext") {
        out.push(Piece::Text(mrkdwn(p, n), None));
    }
    let mut inner = Vec::new();
    if let Some(name) = get("author_name") {
        inner.push(line(vec![Seg::new(name, Style::new().bold())]));
    }
    if let Some(t) = get("title") {
        let seg = match get("title_link") {
            Some(u) => Seg { text: t.to_string(), style: link_style().bold(), url: Some(u.to_string()), ..Default::default() },
            None => Seg::new(t, Style::new().bold()),
        };
        inner.push(line(vec![seg]));
    }
    let bs = a["blocks"].as_array().map(|b| blocks(b, n, ts)).unwrap_or_default();
    // Shared messages carry the same content as blocks and as text.
    if bs.is_empty() {
        if let Some(t) = get("text") {
            inner.push(Piece::Text(mrkdwn(t, n), None));
        }
    }
    inner.extend(bs);
    let mut short = Vec::new();
    for f in a["fields"].as_array().into_iter().flatten() {
        let mut paras = Vec::new();
        if let Some(t) = f["title"].as_str().filter(|s| !s.is_empty()) {
            paras.push(Para::plain(vec![Seg::new(t, Style::new().bold())]));
        }
        paras.extend(mrkdwn(f["value"].as_str().unwrap_or(""), n));
        if f["short"] == true {
            short.push(paras);
        } else {
            if !short.is_empty() {
                inner.push(Piece::Fields(std::mem::take(&mut short)));
            }
            inner.push(Piece::Text(paras, None));
        }
    }
    if !short.is_empty() {
        inner.push(Piece::Fields(short));
    }
    if let Some(u) = get("image_url") {
        let size = |k: &str| a[k].as_u64().unwrap_or(0) as u32;
        inner.push(Piece::Image { img: block_img(u, get("title").unwrap_or(tr("image", "imagem"))), w: size("image_width"), h: size("image_height"), title: String::new() });
    }
    let callback = a["callback_id"].as_str().unwrap_or("");
    let bs: Vec<_> = a["actions"].as_array().into_iter().flatten().filter_map(|e| control(e, n, callback, ts)).collect();
    if !bs.is_empty() {
        inner.push(Piece::Controls(bs));
    }
    if let Some(f) = get("footer") {
        let mut paras = mrkdwn(f, n);
        restyle(&mut paras, |s| if s.fg.is_none() { s.fg(Color::DarkGray) } else { s });
        inner.push(Piece::Text(paras, None));
    }
    if inner.is_empty() {
        if let Some(f) = get("fallback") {
            inner.push(Piece::Text(mrkdwn(f, n), None));
        }
    }
    if !inner.is_empty() {
        out.push(Piece::Bar(color(a), inner));
    }
    out
}

fn color(a: &Value) -> Color {
    match a["color"].as_str().unwrap_or("") {
        "good" => Color::Green,
        "warning" => Color::Yellow,
        "danger" => Color::Red,
        c => match u32::from_str_radix(c.trim_start_matches('#'), 16) {
            Ok(v) if c.trim_start_matches('#').len() == 6 => Color::Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8),
            _ => Color::DarkGray,
        },
    }
}

/// Slack's mrkdwn with its looks: `*bold*`, `_italic_`, `~strike~`,
/// `` `code` ``, ```` ``` ```` blocks, `>` quotes, links and mentions.
pub fn mrkdwn(s: &str, n: &Names) -> Vec<Para> {
    let mut out = Vec::new();
    let parts: Vec<&str> = s.split("```").collect();
    let last = parts.len() - 1;
    for (i, part) in parts.iter().enumerate() {
        // An odd part is code, unless its fence was never closed.
        let fenced = i % 2 == 1;
        if fenced && (i < last || last % 2 == 0) {
            let body = part.strip_prefix('\n').unwrap_or(part);
            let body = body.strip_suffix('\n').unwrap_or(body);
            // One paragraph per block, so two blocks side by side stay two boxes.
            out.push(Para { kind: ParaKind::Code, segs: vec![Seg::new(unescape(body), code_block())] });
            continue;
        }
        let owned = if fenced { format!("```{part}") } else { part.to_string() };
        let mut t = owned.as_str();
        if i > 0 {
            t = t.strip_prefix('\n').unwrap_or(t);
        }
        if i < last {
            t = t.strip_suffix('\n').unwrap_or(t);
        }
        if t.is_empty() && last > 0 {
            continue;
        }
        for line in t.split('\n') {
            let (kind, line) = match line.strip_prefix("&gt;").or_else(|| line.strip_prefix('>')) {
                Some(rest) => (ParaKind::Quote, rest.strip_prefix(' ').unwrap_or(rest)),
                None => (ParaKind::Plain, line),
            };
            let mut segs = Vec::new();
            inline(line, Style::new(), n, &mut segs);
            out.push(Para { kind, segs });
        }
    }
    out
}

/// One line of mrkdwn; a marker only counts at a word's edge, so
/// `snake_case` and `2*3*4` stay as they are.
fn inline(s: &str, style: Style, n: &Names, out: &mut Vec<Seg>) {
    let mut buf = String::new();
    let flush = |buf: &mut String, out: &mut Vec<Seg>| {
        if !buf.is_empty() {
            emojify(&text::shortcodes(&unescape(buf)), style, n, out);
            buf.clear();
        }
    };
    let mut prev: Option<char> = None;
    let mut i = 0;
    while let Some(c) = s[i..].chars().next() {
        let rest = &s[i + c.len_utf8()..];
        let found = match c {
            '<' => rest.find('>').map(|j| {
                flush(&mut buf, out);
                push(out, link(&rest[..j], style, n));
                j
            }),
            '`' => rest.find('`').filter(|&j| j > 0).map(|j| {
                flush(&mut buf, out);
                code_pill(Seg::new(unescape(&rest[..j]), style.patch(code())), out);
                j
            }),
            '*' | '_' | '~' if prev.is_none_or(|p| !p.is_alphanumeric()) && rest.starts_with(|a: char| !a.is_whitespace()) => {
                closing(rest, c).map(|j| {
                    flush(&mut buf, out);
                    let m = match c {
                        '*' => Modifier::BOLD,
                        '_' => Modifier::ITALIC,
                        _ => Modifier::CROSSED_OUT,
                    };
                    inline(&rest[..j], style.add_modifier(m), n, out);
                    j
                })
            }
            _ => None,
        };
        match found {
            // The opening mark, the inside and the closing mark.
            Some(j) => i += c.len_utf8() + j + 1,
            None => {
                buf.push(c);
                i += c.len_utf8();
            }
        }
        prev = Some(c);
    }
    flush(&mut buf, out);
}

/// Two cells the custom emoji's picture is drawn over; an ideographic space
/// is one "word" two cells wide, so wrapping keeps it whole.
fn custom_emoji(name: &str, url: &str, style: Style) -> Seg {
    Seg { text: "\u{3000}".into(), style, emoji: Some(crate::emoji::img(name, url)), ..Default::default() }
}

/// Text with the workspace's `:name:` emoji split out as pictures.
fn emojify(s: &str, style: Style, n: &Names, out: &mut Vec<Seg>) {
    let mut rest = s;
    let mut plain = String::new();
    while let Some(a) = rest.find(':') {
        let after = &rest[a + 1..];
        let found = after.find(':').map(|b| &after[..b]).filter(|name| {
            !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || "-_+'".contains(c)) && n.emoji.contains_key(*name)
        });
        match found {
            Some(name) => {
                plain.push_str(&rest[..a]);
                if !plain.is_empty() {
                    out.push(Seg::new(std::mem::take(&mut plain), style));
                }
                out.push(custom_emoji(name, &n.emoji[name], style));
                rest = &after[name.len() + 1..];
            }
            None => {
                plain.push_str(&rest[..=a]);
                rest = after;
            }
        }
    }
    plain.push_str(rest);
    if !plain.is_empty() {
        out.push(Seg::new(plain, style));
    }
}

/// Where the closing `mark` is: after a non-space, before a non-word.
fn closing(s: &str, mark: char) -> Option<usize> {
    let mut prev: Option<char> = None;
    let mut in_link = false;
    for (j, ch) in s.char_indices() {
        match ch {
            '<' => in_link = true,
            '>' => in_link = false,
            _ if ch == mark && !in_link && j > 0 => {
                let next = s[j + 1..].chars().next();
                if prev.is_some_and(|p| !p.is_whitespace()) && next.is_none_or(|a| !a.is_alphanumeric()) {
                    return Some(j);
                }
            }
            _ => {}
        }
        prev = Some(ch);
    }
    None
}

fn link(inner: &str, style: Style, n: &Names) -> Seg {
    let (target, label) = inner.split_once('|').unwrap_or((inner, ""));
    if let Some(id) = target.strip_prefix('@') {
        return user(id, &unescape(label), style, n);
    }
    if let Some(id) = target.strip_prefix('#') {
        return channel(id, &unescape(label), style, n);
    }
    if let Some(special) = target.strip_prefix('!') {
        let shown = text::link(inner, n);
        return match special.split('^').next().unwrap_or("") {
            "here" | "channel" | "everyone" => mention(shown, style, true, None),
            "subteam" => mention(shown, style, false, None),
            // Dates and the like: just their text.
            _ => Seg::new(shown, style),
        };
    }
    let shown = if label.is_empty() { target.strip_prefix("mailto:").unwrap_or(target).to_string() } else { unescape(label) };
    Seg { text: text::shortcodes(&shown), style: style.patch(link_style()), url: Some(unescape(target)), ..Default::default() }
}

/// A mention as Slack draws it; no-break spaces keep it one word, so it
/// wraps whole and stays in one highlight.
fn mention(shown: String, style: Style, mine: bool, target: Option<Target>) -> Seg {
    let (fg, bg) = if mine { (PILL_MINE_FG, PILL_MINE_BG) } else { (PILL_FG, PILL_BG) };
    Seg { text: shown.replace(' ', "\u{a0}"), style: style.fg(fg).bg(bg), target, ..Default::default() }
}

/// `label` is what the message itself calls them, for people we can't name.
fn user(id: &str, label: &str, style: Style, n: &Names) -> Seg {
    let name = n.user(id).or(Some(label).filter(|l| !l.is_empty())).unwrap_or(id);
    mention(format!("@{}", name.trim_start_matches('@')), style, id == n.me, Some(Target::User(id.into())))
}

fn channel(id: &str, label: &str, style: Style, n: &Names) -> Seg {
    let name = n.chan_shown(id, label);
    let sign = if n.private.contains(id) { LOCK } else { "#" };
    mention(format!("{sign}{name}"), style, false, Some(Target::Channel(id.into())))
}

fn is_pill(s: &Seg) -> bool {
    s.style.bg.is_some_and(|b| b == PILL_BG || b == PILL_MINE_BG)
}

/// Mentions side by side get a space between them, to read as two.
fn push(out: &mut Vec<Seg>, seg: Seg) {
    if is_pill(&seg) && out.last().is_some_and(is_pill) {
        out.push(Seg::new(" ", Style::new()));
    }
    out.push(seg);
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
}

fn rich_text(b: &Value, n: &Names) -> Vec<Para> {
    let segs = |el: &Value, base: Style| {
        let mut v = Vec::new();
        for e in elements(el) {
            rich_inline(e, base, n, &mut v);
        }
        v
    };
    let mut out = Vec::new();
    for el in elements(b) {
        match el["type"].as_str().unwrap_or("") {
            "rich_text_list" => {
                let ordered = el["style"] == "ordered";
                let indent = "  ".repeat(el["indent"].as_u64().unwrap_or(0) as usize);
                let offset = el["offset"].as_u64().unwrap_or(0) as usize;
                for (i, item) in elements(el).enumerate() {
                    let bullet = if ordered { format!("{indent}{}. ", offset + i + 1) } else { format!("{indent}• ") };
                    let mut s = vec![Seg::new(bullet, Style::new())];
                    s.extend(trim(segs(item, Style::new())));
                    out.push(Para::plain(s));
                }
            }
            "rich_text_quote" => out.push(Para { kind: ParaKind::Quote, segs: trim(segs(el, Style::new())) }),
            "rich_text_preformatted" => out.push(Para { kind: ParaKind::Code, segs: trim(segs(el, code_block())) }),
            _ => out.push(Para::plain(trim(segs(el, Style::new())))),
        }
    }
    out
}

/// A section ending in `\n` would leave an empty line under it.
fn trim(mut segs: Vec<Seg>) -> Vec<Seg> {
    while let Some(last) = segs.last_mut() {
        let t = last.text.trim_end_matches('\n').len();
        last.text.truncate(t);
        if !last.text.is_empty() {
            break;
        }
        segs.pop();
    }
    segs
}

fn rich_inline(e: &Value, base: Style, n: &Names, out: &mut Vec<Seg>) {
    let s = |k: &str| e[k].as_str().unwrap_or("");
    let st = &e["style"];
    let mut style = base;
    for (k, m) in [("bold", Modifier::BOLD), ("italic", Modifier::ITALIC), ("strike", Modifier::CROSSED_OUT)] {
        if st[k] == true {
            style = style.add_modifier(m);
        }
    }
    let is_code = st["code"] == true;
    if is_code {
        style = style.patch(code());
    }
    let seg = match s("type") {
        "text" => Seg::new(s("text"), style),
        "link" => Seg {
            text: if s("text").is_empty() { s("url") } else { s("text") }.to_string(),
            style: style.patch(link_style()),
            url: Some(s("url").to_string()),
            ..Default::default()
        },
        "user" => user(s("user_id"), "", style, n),
        "channel" => channel(s("channel_id"), "", style, n),
        "usergroup" => mention(tr("@group", "@grupo").into(), style, false, None),
        "broadcast" => mention(format!("@{}", s("range")), style, true, None),
        "emoji" => match (text::emoji(s("unicode")), n.emoji.get(s("name"))) {
            (Some(u), _) => Seg::new(u, style),
            (None, Some(url)) => custom_emoji(s("name"), url, style),
            (None, None) => Seg::new(text::shortcodes(&format!(":{}:", s("name"))), style),
        },
        "date" => Seg::new(s("fallback"), style),
        "color" => Seg::new(s("value"), style),
        _ => return,
    };
    if is_code && s("type") == "text" {
        return code_pill(seg, out);
    }
    push(out, seg);
}

// ---- layout ----------------------------------------------------------------

/// One laid-out line under construction, with its clickable stretches.
#[derive(Default)]
struct Row {
    spans: Vec<Span<'static>>,
    w: usize,
    hits: Vec<(usize, usize, Target)>,
    /// Custom emoji: the column and the picture.
    pics: Vec<(usize, ImgRef)>,
}

impl Row {
    fn push(&mut self, s: String, style: Style, target: Option<Target>) {
        let w = s.width();
        if let Some(t) = target {
            self.hits.push((self.w, w, t));
        }
        self.spans.push(Span::styled(s, style));
        self.w += w;
    }

    fn append(&mut self, other: Row) {
        self.hits.extend(other.hits.into_iter().map(|(x, w, t)| (x + self.w, w, t)));
        self.pics.extend(other.pics.into_iter().map(|(x, img)| (x + self.w, img)));
        self.spans.extend(other.spans);
        self.w += other.w;
    }

    fn pad_to(&mut self, x: usize) {
        if self.w < x {
            self.spans.push(Span::raw(" ".repeat(x - self.w)));
            self.w = x;
        }
    }
}

fn push_row(out: &mut Out, row: Row) {
    let line = out.lines.len();
    out.hits.extend(row.hits.into_iter().map(|(x, w, target)| Hit { line, x: x as u16, w: w as u16, target }));
    out.pics.extend(row.pics.into_iter().map(|(x, img)| Pic { line, x: x as u16, cols: 2, rows: 1, img }));
    out.lines.push(Line::from(row.spans));
}

/// Lays the pieces out `width` cells wide; `base` goes under every text style
/// (system messages are dim and italic). `font` is the cell size in pixels,
/// when images can be drawn.
pub fn render(pieces: &[Piece], width: usize, font: Option<(u16, u16)>, base: Style) -> Out {
    let width = width.max(4);
    if let ([Piece::Text(paras, None)], Some(font)) = (pieces, font) {
        if let Some(items) = jumbo_items(paras) {
            return jumbo(&items, width, font, base);
        }
    }
    let mut out = Out::default();
    for p in pieces {
        match p {
            Piece::Text(paras, acc) => text_piece(&mut out, paras, acc.as_ref(), width, font, base),
            Piece::Fields(fs) => fields(&mut out, fs, width, base),
            Piece::Divider => out.lines.push(Line::from(Span::styled("─".repeat(width), dim()))),
            Piece::Controls(bs) => controls(&mut out, bs, width),
            Piece::Image { img, w, h, title } => {
                if !title.is_empty() {
                    for r in wrap(&[Seg::new(title.clone(), dim())], width, base) {
                        push_row(&mut out, r);
                    }
                }
                image(&mut out, img, *w, *h, width, font);
            }
            Piece::Bar(color, inner) => {
                let sub = render(inner, width.saturating_sub(2), font, base);
                nest(&mut out, sub, Span::styled("▎ ", Style::new().fg(*color)));
            }
        }
    }
    out
}

/// Slack draws a message of nothing but emoji (up to this many) bigger.
const JUMBO_MAX: usize = 23;

enum Big {
    Pic(ImgRef),
    Text(String),
    Break,
}

/// The emoji of a message that is only emoji, when one of them is custom:
/// unicode ones cannot grow in a terminal, so they alone stay as they are.
fn jumbo_items(paras: &[Para]) -> Option<Vec<Big>> {
    if !paras.iter().any(|p| p.segs.iter().any(|s| s.emoji.is_some())) {
        return None;
    }
    let (mut items, mut n) = (Vec::new(), 0);
    for (k, p) in paras.iter().enumerate() {
        if p.kind != ParaKind::Plain {
            return None;
        }
        if k > 0 {
            items.push(Big::Break);
        }
        for s in &p.segs {
            if let Some(img) = &s.emoji {
                items.push(Big::Pic(img.clone()));
                n += 1;
                continue;
            }
            if s.url.is_some() || s.target.is_some() {
                return None;
            }
            for (j, line) in s.text.split('\n').enumerate() {
                if j > 0 {
                    items.push(Big::Break);
                }
                for word in line.split_whitespace() {
                    n += emoji_run(word)?;
                    items.push(Big::Text(word.to_string()));
                }
            }
        }
    }
    (n <= JUMBO_MAX).then_some(items)
}

/// How many emoji `s` is made of, or None when it holds anything else.
fn emoji_run(s: &str) -> Option<usize> {
    let (mut n, mut prev, mut flag) = (0, ' ', false);
    for c in s.chars() {
        let joined = prev == '\u{200d}';
        prev = c;
        match c {
            // Joiners, variation selectors, skin tones and tags ride on the emoji before.
            '\u{200d}' | '\u{fe0f}' | '\u{20e3}' | '\u{1f3fb}'..='\u{1f3ff}' | '\u{e0020}'..='\u{e007f}' => {}
            // A flag is two regional indicators.
            '\u{1f1e6}'..='\u{1f1ff}' => {
                n += usize::from(!flag);
                flag = !flag;
            }
            _ if !c.is_ascii()
                && (emojis::get(c.encode_utf8(&mut [0; 4])).is_some() || emojis::get(&format!("{c}\u{fe0f}")).is_some()) =>
            {
                n += usize::from(!joined);
            }
            _ => return None,
        }
    }
    Some(n)
}

/// Custom emoji two rows tall and as wide as keeps them square; unicode ones
/// go on the top row, as big as the terminal draws them.
fn jumbo(items: &[Big], width: usize, (fw, fh): (u16, u16), base: Style) -> Out {
    let rows = 2u16;
    let cols = ((rows as f64 * fh.max(1) as f64 / fw.max(1) as f64).round() as u16).max(1);
    let end = |out: &mut Out, row: Row| {
        let line = out.lines.len();
        out.pics.extend(row.pics.into_iter().map(|(x, img)| Pic { line, x: x as u16, cols, rows, img }));
        out.lines.push(Line::from(row.spans));
        out.lines.extend(std::iter::repeat_n(Line::default(), rows as usize - 1));
    };
    let mut out = Out::default();
    let mut row = Row::default();
    for item in items {
        let w = match item {
            Big::Break => {
                end(&mut out, std::mem::take(&mut row));
                continue;
            }
            Big::Pic(_) => cols as usize,
            Big::Text(t) => t.width(),
        };
        if row.w > 0 && row.w + 1 + w > width {
            end(&mut out, std::mem::take(&mut row));
        }
        if row.w > 0 {
            row.push(" ".into(), base, None);
        }
        match item {
            Big::Pic(img) => {
                row.pics.push((row.w, img.clone()));
                row.push(" ".repeat(w), base, None);
            }
            Big::Text(t) => row.push(t.clone(), base, None),
            Big::Break => {}
        }
    }
    end(&mut out, row);
    out
}

fn nest(out: &mut Out, sub: Out, prefix: Span<'static>) {
    let (top, dx) = (out.lines.len(), prefix.width() as u16);
    out.lines.extend(sub.lines.into_iter().map(|mut l| {
        l.spans.insert(0, prefix.clone());
        l
    }));
    out.pics.extend(sub.pics.into_iter().map(|p| Pic { line: top + p.line, x: dx + p.x, ..p }));
    out.hits.extend(sub.hits.into_iter().map(|h| Hit { line: top + h.line, x: dx + h.x, ..h }));
}

fn rows_of(paras: &[Para], width: usize, base: Style) -> Vec<Row> {
    paras.iter().flat_map(|p| para_rows(p, width, base)).collect()
}

fn text_piece(out: &mut Out, paras: &[Para], acc: Option<&Accessory>, width: usize, font: Option<(u16, u16)>, base: Style) {
    match (acc, font) {
        // Like Slack, the accessory sits at the top right of the text.
        (Some(Accessory::Button(b)), _) => {
            let p = pill(b, width);
            if width >= p.w + 2 + MIN_TEXT {
                let x = width - p.w;
                let mut rows = rows_of(paras, x - 2, base).into_iter();
                let mut first = rows.next().unwrap_or_default();
                first.pad_to(x);
                first.append(p);
                push_row(out, first);
                rows.for_each(|r| push_row(out, r));
            } else {
                rows_of(paras, width, base).into_iter().for_each(|r| push_row(out, r));
                push_row(out, p);
            }
        }
        (Some(Accessory::Image(img)), Some((fw, fh))) => {
            let rows = 4u16;
            let cols = ((rows as f64 * fh.max(1) as f64 / fw.max(1) as f64).round() as u16).max(1);
            let top = out.lines.len();
            if width >= cols as usize + 2 + MIN_TEXT {
                rows_of(paras, width - cols as usize - 2, base).into_iter().for_each(|r| push_row(out, r));
                out.pics.push(Pic { line: top, x: (width - cols as usize) as u16, cols, rows, img: img.clone() });
                while out.lines.len() < top + rows as usize {
                    out.lines.push(Line::default());
                }
            } else {
                rows_of(paras, width, base).into_iter().for_each(|r| push_row(out, r));
                out.pics.push(Pic { line: out.lines.len(), x: 0, cols, rows, img: img.clone() });
                out.lines.extend(std::iter::repeat_n(Line::default(), rows as usize));
            }
        }
        (Some(Accessory::Image(img)), None) => {
            rows_of(paras, width, base).into_iter().for_each(|r| push_row(out, r));
            out.lines.push(Line::from(Span::styled(format!("🖼  {}", img.name), dim())));
        }
        (None, _) => rows_of(paras, width, base).into_iter().for_each(|r| push_row(out, r)),
    }
}

/// Two columns, as in Slack; a narrow screen gets one.
fn fields(out: &mut Out, fs: &[Vec<Para>], width: usize, base: Style) {
    if width < 40 {
        fs.iter().flat_map(|f| rows_of(f, width, base)).for_each(|r| push_row(out, r));
        return;
    }
    let col = (width - 2) / 2;
    for pair in fs.chunks(2) {
        let left = rows_of(&pair[0], col, base);
        let right = pair.get(1).map(|f| rows_of(f, col, base)).unwrap_or_default();
        let n = left.len().max(right.len());
        let (mut l, mut r) = (left.into_iter(), right.into_iter());
        for _ in 0..n {
            let mut row = l.next().unwrap_or_default();
            if let Some(rr) = r.next() {
                row.pad_to(col + 2);
                row.append(rr);
            }
            push_row(out, row);
        }
    }
}

/// A row of buttons, wrapping when it does not fit.
fn controls(out: &mut Out, bs: &[Button], width: usize) {
    let mut row = Row::default();
    for b in bs {
        let p = pill(b, width);
        if row.w > 0 && row.w + 1 + p.w > width {
            push_row(out, std::mem::take(&mut row));
        }
        if row.w > 0 {
            row.push(" ".into(), Style::new(), None);
        }
        row.append(p);
    }
    push_row(out, row);
}

/// A rounded, filled button; menus are quieter and get a ▾.
fn pill(b: &Button, max: usize) -> Row {
    let (fg, bg) = match b.look {
        Look::Primary => (Color::Black, Color::Green),
        Look::Danger => (Color::White, Color::Red),
        Look::Default => (Color::White, Color::DarkGray),
        Look::Menu => (Color::Gray, Color::Indexed(236)),
    };
    let face = if b.look == Look::Menu { Style::new().fg(fg).bg(bg) } else { Style::new().fg(fg).bg(bg).bold() };
    let arrow = if b.look == Look::Menu && b.label != "⋯" { " ▾" } else { "" };
    let label = truncate(&b.label, max.saturating_sub(4 + arrow.width()));
    let mut r = Row::default();
    r.push(CAP_L.into(), Style::new().fg(bg), None);
    r.push(format!(" {label}{arrow} "), face, None);
    r.push(CAP_R.into(), Style::new().fg(bg), None);
    r.hits.push((0, r.w, Target::Button(b.clone())));
    r
}

/// An image at most 50 columns by 16 rows, keeping its shape when known.
fn image(out: &mut Out, img: &ImgRef, w: u32, h: u32, width: usize, font: Option<(u16, u16)>) {
    let Some((fw, fh)) = font else {
        out.lines.push(Line::from(Span::styled(format!("🖼  {}", img.name), dim())));
        return;
    };
    let max_cols = width.min(50) as f64;
    let (cols, rows) = if w > 0 && h > 0 {
        let (w, h) = (w as f64 / fw.max(1) as f64, h as f64 / fh.max(1) as f64);
        let scale = (max_cols / w).min(16.0 / h).min(1.0);
        ((w * scale).ceil().max(1.0) as u16, (h * scale).ceil().max(1.0) as u16)
    } else {
        (width.min(40) as u16, 12)
    };
    out.pics.push(Pic { line: out.lines.len(), x: 0, cols, rows, img: img.clone() });
    out.lines.extend(std::iter::repeat_n(Line::default(), rows as usize));
}

fn para_rows(p: &Para, width: usize, base: Style) -> Vec<Row> {
    let prefix = match p.kind {
        ParaKind::Plain => return wrap(&p.segs, width, base),
        ParaKind::Quote => "▎ ",
        ParaKind::Code => return code_box(&p.segs, width, base),
    };
    wrap(&p.segs, width.saturating_sub(2), base)
        .into_iter()
        .map(|r| {
            let mut row = Row::default();
            row.push(prefix.into(), dim(), None);
            row.append(r);
            row
        })
        .collect()
}

/// A code block as Slack draws it: a filled, rounded box the full width,
/// a column of padding, the text verbatim (long lines broken at the edge)
/// and a copy button in the top right corner.
fn code_box(segs: &[Seg], width: usize, base: Style) -> Vec<Row> {
    let width = width.max(5);
    let inner = width - 4;
    // Only the inside is filled; the frame sits on the message's background.
    let fill = Style::new().bg(crate::ui::CODE_BG);
    let border = Style::new().fg(CODE_BORDER);
    let text: String = segs.iter().map(|s| s.text.as_str()).collect();
    let mut top = Row::default();
    if width >= 8 {
        top.push(format!("╭{}", "─".repeat(width - 6)), border, None);
        top.push(format!(" {COPY} "), Style::new().fg(Color::Gray), Some(Target::Copy(text)));
        top.push("─╮".into(), border, None);
    } else {
        top.push(format!("╭{}╮", "─".repeat(width - 2)), border, None);
    }
    let mut rows = vec![top];
    for r in code_lines(segs, inner, base.bg(crate::ui::CODE_BG)) {
        let mut row = Row::default();
        row.push("│".into(), border, None);
        row.push(" ".into(), fill, None);
        let pad = inner.saturating_sub(r.w);
        row.append(r);
        row.push(" ".repeat(pad + 1), fill, None);
        row.push("│".into(), border, None);
        rows.push(row);
    }
    let mut bottom = Row::default();
    bottom.push(format!("╰{}╯", "─".repeat(width - 2)), border, None);
    rows.push(bottom);
    rows
}

/// Code broken only at `\n` and at the box's edge: spaces, indentation and
/// empty lines all stay; tabs become four spaces.
fn code_lines(segs: &[Seg], width: usize, base: Style) -> Vec<Row> {
    let target = |s: &Seg| s.target.clone().or_else(|| s.url.clone().map(Target::Link));
    let mut rows = Vec::new();
    let mut row = Row::default();
    for s in segs {
        let style = base.patch(s.style);
        let flush = |row: &mut Row, chunk: &mut String| {
            if !chunk.is_empty() {
                if let Some(img) = &s.emoji {
                    row.pics.push((row.w, img.clone()));
                }
                row.push(std::mem::take(chunk), style, target(s));
            }
        };
        for (k, line) in s.text.split('\n').enumerate() {
            if k > 0 {
                rows.push(std::mem::take(&mut row));
            }
            let mut chunk = String::new();
            let mut cw = 0;
            for ch in line.replace('\t', "    ").chars() {
                let w = ch.width().unwrap_or(0);
                if row.w + cw + w > width && row.w + cw > 0 {
                    flush(&mut row, &mut chunk);
                    rows.push(std::mem::take(&mut row));
                    cw = 0;
                }
                chunk.push(ch);
                cw += w;
            }
            flush(&mut row, &mut chunk);
        }
    }
    rows.push(row);
    rows
}

/// Shows "✓ copied" in place of the copy button of the block holding `text`.
pub fn mark_copied(out: &mut Out, text: &str) {
    let label = format!(" ✓ {} ", tr("copied", "copiado"));
    for h in out.hits.iter_mut().filter(|h| matches!(&h.target, Target::Copy(t) if t == text)) {
        let Some(line) = out.lines.get_mut(h.line) else { continue };
        // The button's span, found by its column.
        let mut x = 0;
        let Some(i) = line.spans.iter().position(|s| {
            let at = x;
            x += s.width();
            at == h.x as usize
        }) else {
            continue;
        };
        // The dashes before it make room for the longer label.
        let grow = label.width().saturating_sub(line.spans[i].width());
        let Some(dashes) = i.checked_sub(1).map(|j| &mut line.spans[j]) else { continue };
        let n = dashes.content.chars().count();
        if n <= grow + 1 {
            continue;
        }
        dashes.content = dashes.content.chars().take(n - grow).collect::<String>().into();
        let style = line.spans[i].style.fg(Color::Green);
        line.spans[i] = Span::styled(label.clone(), style);
        h.x -= grow as u16;
        h.w = label.width() as u16;
    }
}

/// Greedy word wrap over styled runs: a word may span runs (`*bold*,`),
/// words longer than a line are split, and `\n` breaks the line.
fn wrap(segs: &[Seg], width: usize, base: Style) -> Vec<Row> {
    enum Tok<'a> {
        Word(Vec<(&'a str, &'a Seg)>),
        Space(&'a Seg),
        Break,
    }
    let mut toks = Vec::new();
    for seg in segs {
        for (k, line) in seg.text.split('\n').enumerate() {
            if k > 0 {
                toks.push(Tok::Break);
            }
            for (j, word) in line.split(' ').enumerate() {
                if j > 0 {
                    toks.push(Tok::Space(seg));
                }
                if word.is_empty() {
                    continue;
                }
                match toks.last_mut() {
                    Some(Tok::Word(parts)) => parts.push((word, seg)),
                    _ => toks.push(Tok::Word(vec![(word, seg)])),
                }
            }
        }
    }
    let width = width.max(1);
    let style = |s: &Seg| base.patch(s.style);
    let target = |s: &Seg| s.target.clone().or_else(|| s.url.clone().map(Target::Link));
    let mut rows = Vec::new();
    let mut row = Row::default();
    let mut spaces: Vec<&Seg> = Vec::new();
    for tok in toks {
        match tok {
            Tok::Break => {
                rows.push(std::mem::take(&mut row));
                spaces.clear();
            }
            Tok::Space(s) => spaces.push(s),
            Tok::Word(parts) => {
                let ww: usize = parts.iter().map(|(w, _)| w.width()).sum();
                // Spaces at a wrap are dropped; leading ones (code indentation) stay.
                if row.w > 0 && row.w + spaces.len() + ww > width {
                    rows.push(std::mem::take(&mut row));
                    spaces.clear();
                }
                for s in spaces.drain(..) {
                    row.push(" ".into(), style(s), target(s));
                }
                for (word, s) in parts {
                    if row.w + word.width() <= width {
                        if let Some(img) = &s.emoji {
                            row.pics.push((row.w, img.clone()));
                        }
                        row.push(word.into(), style(s), target(s));
                        continue;
                    }
                    let (mut chunk, mut cw) = (String::new(), 0);
                    for ch in word.chars() {
                        let w = ch.width().unwrap_or(0);
                        if row.w + cw + w > width && (row.w > 0 || cw > 0) {
                            if !chunk.is_empty() {
                                row.push(std::mem::take(&mut chunk), style(s), target(s));
                            }
                            rows.push(std::mem::take(&mut row));
                            cw = 0;
                        }
                        chunk.push(ch);
                        cw += w;
                    }
                    if !chunk.is_empty() {
                        row.push(chunk, style(s), target(s));
                    }
                }
            }
        }
    }
    rows.push(row);
    rows
}

fn truncate(s: &str, width: usize) -> String {
    if s.width() <= width {
        return s.to_string();
    }
    let mut out = String::new();
    let mut w = 0;
    for ch in s.chars() {
        let cw = ch.width().unwrap_or(0);
        if w + cw + 1 > width {
            break;
        }
        out.push(ch);
        w += cw;
    }
    out.push('…');
    out
}
