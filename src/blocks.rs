//! Block Kit, attachments and mrkdwn laid out the way Slack draws them:
//! styled text, fields in two columns, dividers, images and buttons that
//! can be clicked. `text::body` stays the flat version for everything else.

use crate::app::ImgRef;
use crate::i18n::tr;
use crate::slack::{File, Kind, Message};
use chrono::{Datelike, Local, TimeZone};
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
    /// A shared or forwarded message, as its own card.
    Share(Box<Share>),
    /// An app's card for a link (Slack's work object).
    Card(Box<Card>),
}

/// An app's card for a link (Slack's work object, such as a Jira issue): the
/// app's name above, the item's icon, title and kind, its fields, when its
/// data is from and the app's buttons.
#[derive(Clone)]
pub struct Card {
    pub header: String,
    pub title: String,
    pub subtitle: String,
    pub icon: Option<ImgRef>,
    /// Each field's label and values.
    pub fields: Vec<(String, Vec<Tag>)>,
    /// What `chat.refreshUnfurl` asks for; empty: no Refresh.
    pub entity: String,
    /// When the app says it last changed (Unix seconds); 0 when it doesn't.
    pub updated: i64,
    /// The message it belongs to.
    pub ts: String,
    /// Where a click on it goes, and the link the message had.
    pub url: String,
    pub link: String,
    pub actions: Vec<Button>,
    /// The app's other actions, in the menu by its name.
    pub more: Vec<Button>,
    /// Set by the UI before drawing it.
    pub refresh: Refresh,
}

/// A field's value: its text, Slack's color name for it and a picture (a person's).
#[derive(Clone)]
pub struct Tag {
    pub text: String,
    pub color: String,
    pub icon: Option<ImgRef>,
}

/// Where a card's Refresh is: the spinner's frame while the app answers.
#[derive(Clone, Copy, Default, PartialEq)]
pub enum Refresh {
    #[default]
    Idle,
    Busy(usize),
    Failed,
}


/// A shared message as Slack draws it: the author's photo and name, its day,
/// where it was posted ("Thread in 🔒 name"), its text and its files. A click
/// anywhere else on it opens the original (`open`).
pub struct Share {
    pub author: String,
    /// The author's id, whose profile the photo opens.
    pub user: Option<String>,
    /// The photo: whose (a user id, or a key of its own) and its URL.
    pub photo: Option<(String, String)>,
    pub when: String,
    /// "Posted in #name", or Slack's own footer for a place we don't know.
    pub place: Vec<Seg>,
    pub body: Vec<Piece>,
    pub files: Vec<File>,
    pub open: Option<Target>,
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
    /// A shared message's original: its conversation at it, with its
    /// thread open when it is a reply.
    Open { chan: String, ts: String, thread: Option<String> },
    /// A card's app name: a menu with its other actions and the link's.
    CardMenu { url: String, link: String, more: Vec<Button> },
    CopyLink(String),
    /// A card's Refresh (`chat.refreshUnfurl`).
    Refresh { ts: String, entity: String },
    /// An emoji: its name, for the tooltip, and what a click copies.
    Emoji(String, String),
}

/// Where an image goes, in cells from the top left of the laid-out text.
pub struct Pic {
    pub line: usize,
    pub x: u16,
    pub cols: u16,
    pub rows: u16,
    pub img: ImgRef,
    /// What a click enlarges instead of the picture itself.
    pub full: Option<ImgRef>,
    /// A person's photo: a click opens their profile, nothing to enlarge.
    pub user: Option<String>,
}

/// Two reserved lines where a file's card goes (`files::card`), drawn by
/// the UI, which knows whether it was downloaded.
pub struct FileCard {
    pub line: usize,
    pub x: u16,
    pub w: u16,
    pub file: File,
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
    pub cards: Vec<FileCard>,
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
    // Its blocks are only the fallback for clients without cards.
    if let Some(c) = card(a, n, ts) {
        return vec![Piece::Card(Box::new(c))];
    }
    let get = |k: &str| a[k].as_str().filter(|s| !s.is_empty());
    let line = |segs: Vec<Seg>| Piece::Text(vec![Para::plain(segs)], None);
    let mut out = Vec::new();
    // As in Slack, the pretext sits above the bar.
    if let Some(p) = get("pretext") {
        out.push(Piece::Text(mrkdwn(p, n), None));
    }
    if a["is_share"] == true || a["is_msg_unfurl"] == true {
        out.push(Piece::Share(Box::new(share(a, n))));
        return out;
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

/// A shared message's card from its attachment: who, when, where, its rich
/// text (the original's blocks, else its mrkdwn) and its files.
fn share(a: &Value, n: &Names) -> Share {
    let get = |k: &str| a[k].as_str().filter(|s| !s.is_empty());
    let (chan, ts, reply, open) = origin(a);
    let (chan, ts) = (chan.as_str(), ts.as_str());
    let user = get("author_id").map(String::from);
    let author = user.as_deref().and_then(|u| n.user(u)).or(get("author_name")).or(get("author_subname")).unwrap_or("").to_string();
    let photo = match (&user, user.as_ref().and_then(|u| n.avatars.get(u)).map(String::as_str).or(get("author_icon"))) {
        (Some(u), Some(url)) => Some((u.clone(), url.to_string())),
        (None, Some(url)) => {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            url.hash(&mut h);
            Some((format!("share-{:016x}", h.finish()), url.to_string()))
        }
        _ => None,
    };
    let footer = get("footer").unwrap_or("");
    let place = match n.place(chan) {
        Some((kind, name)) => {
            let lead = if reply { tr("Thread in ", "Conversa em ") } else { tr("Posted in ", "Publicada em ") };
            let sign = match kind {
                Kind::Channel => "#".to_string(),
                Kind::Private => format!("{LOCK} "),
                Kind::Direct => tr("direct message with ", "mensagem direta com ").to_string(),
                Kind::Group => String::new(),
            };
            vec![Seg::new(format!("{lead}{sign}{name}"), dim())]
        }
        None => {
            let mut paras = mrkdwn(footer, n);
            restyle(&mut paras, |s| if s.fg.is_none() { s.fg(Color::DarkGray) } else { s });
            paras.into_iter().flat_map(|p| p.segs).collect()
        }
    };
    let mut body = a["blocks"].as_array().map(|b| blocks(b, n, ts)).unwrap_or_default();
    if body.is_empty() {
        for mb in a["message_blocks"].as_array().into_iter().flatten() {
            body.extend(mb["message"]["blocks"].as_array().map(|b| blocks(b, n, ts)).unwrap_or_default());
        }
    }
    if body.is_empty() {
        let paras = mrkdwn(get("text").or(get("fallback")).unwrap_or(""), n);
        if !blank(&paras) {
            body.push(Piece::Text(paras, None));
        }
    }
    let files = a["files"].as_array().into_iter().flatten().filter(|f| f["mode"] != "tombstone").map(crate::slack::parse_file).collect();
    Share { author, user, photo, when: stamp(ts), place, body, files, open }
}

/// A shared message's original, from its attachment: its conversation and
/// ts, whether it is a reply, and the card's click (`Target::Open`, with the
/// thread's ts for a reply) when both are known.
fn origin(a: &Value) -> (String, String, bool, Option<Target>) {
    let get = |k: &str| a[k].as_str().filter(|s| !s.is_empty());
    // Else from its link: …/archives/C0123/p1760000000000100?thread_ts=….
    let link = get("from_url").or(get("original_url")).and_then(permalink);
    let chan = get("channel_id").map(String::from).or(link.as_ref().map(|l| l.chan.clone())).unwrap_or_default();
    let ts = get("ts").map(String::from).or(link.as_ref().and_then(|l| l.ts.clone())).unwrap_or_default();
    // A reply: Slack's footer says "Thread in …"; its link has the thread's ts.
    let thread_ts = get("thread_ts").or(link.as_ref().and_then(|l| l.thread.as_deref()));
    let lower = get("footer").unwrap_or("").to_lowercase();
    let reply = a["is_reply_unfurl"] == true || lower.starts_with("thread") || lower.starts_with("conversa") || thread_ts.is_some_and(|t| t != ts);
    let open = (!chan.is_empty() && !ts.is_empty()).then(|| {
        let thread = reply.then(|| thread_ts.unwrap_or(&ts).to_string());
        Target::Open { chan: chan.clone(), ts: ts.clone(), thread }
    });
    (chan, ts, reply, open)
}

/// The original of the first message a message shares (Enter on it, as a
/// click on its card).
pub fn shared_original(m: &Message) -> Option<Target> {
    let shares = m.attachments.as_array().into_iter().flatten().filter(|a| a["is_share"] == true || a["is_msg_unfurl"] == true);
    shares.filter_map(|a| origin(a).3).next()
}

/// Where a Slack permalink points: the conversation, the message and its
/// thread. …/archives/C0123/p1760000000000100?thread_ts=1760000000.000050,
/// or the web app's …/client/T0123/C0123 (/thread/C0123-1760000000.000050).
pub struct Permalink {
    pub chan: String,
    pub ts: Option<String>,
    pub thread: Option<String>,
}

pub fn permalink(url: &str) -> Option<Permalink> {
    let (path, query) = url.split_once('?').unwrap_or((url, ""));
    let (host, path) = path.split_once("://")?.1.split_once('/')?;
    if host != "slack.com" && !host.ends_with(".slack.com") {
        return None;
    }
    let rest = match path.split_once('/')? {
        ("archives", rest) => rest,
        // client/T0123/C0123/…
        ("client", rest) => rest.split_once('/')?.1,
        _ => return None,
    };
    let mut parts = rest.split('/').filter(|p| !p.is_empty());
    let chan = parts.next().filter(|c| c.len() > 1 && c.bytes().all(|b| b.is_ascii_alphanumeric()))?.to_string();
    let stamp = |d: &str| (d.len() > 6 && d.bytes().all(|b| b.is_ascii_digit())).then(|| format!("{}.{}", &d[..d.len() - 6], &d[d.len() - 6..]));
    let digits = |t: &&str| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit() || b == b'.');
    let (mut ts, mut thread) = (None, None);
    match parts.next() {
        Some(p) if p.starts_with('p') => ts = stamp(&p[1..]),
        Some("thread") => thread = parts.next().and_then(|t| t.rsplit_once('-')).map(|(_, t)| t).filter(digits).map(String::from),
        _ => {}
    }
    let query = query.split('#').next().unwrap_or("");
    thread = thread.or_else(|| query.split('&').find_map(|kv| kv.strip_prefix("thread_ts=")).filter(digits).map(String::from));
    // A thread's own link is at its first message.
    let ts = ts.or_else(|| thread.clone());
    Some(Permalink { chan, ts, thread })
}

/// When a shared message was posted: "Today at 15:13", "October 22nd at
/// 15:13" ("22 de outubro às 15:13"), with the year when not this one.
fn stamp(ts: &str) -> String {
    let secs = ts.split('.').next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let Some(t) = Local.timestamp_opt(secs, 0).single().filter(|_| secs > 0) else { return String::new() };
    let today = Local::now().date_naive();
    let clock = crate::i18n::clock(&t);
    let (day, month, year, nth) = (t.day(), crate::i18n::month(&t), t.year(), crate::i18n::ordinal(t.day()));
    match t.date_naive() {
        d if d == today => trf!("Today at {clock}", "Hoje às {clock}"),
        d if today.pred_opt() == Some(d) => trf!("Yesterday at {clock}", "Ontem às {clock}"),
        _ if year == today.year() => trf!("{month} {nth} at {clock}", "{day} de {month} às {clock}"),
        _ => trf!("{month} {nth}, {year} at {clock}", "{day} de {month} de {year} às {clock}"),
    }
}

/// The text of a value that may be a string, a number or a text object.
fn any_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(x) => x.to_string(),
        _ => v["text"].as_str().unwrap_or("").to_string(),
    }
}

/// Seconds, milliseconds or a date, as Unix seconds; 0 when it's none.
fn when(v: &Value) -> i64 {
    let secs = |v: f64| if v < 1e12 { v as i64 } else { (v / 1000.0) as i64 };
    match v {
        Value::Number(x) => x.as_f64().map(secs).unwrap_or(0),
        Value::String(s) => s
            .parse::<f64>()
            .map(secs)
            .or_else(|_| chrono::DateTime::parse_from_rfc3339(s).map(|d| d.timestamp()))
            .unwrap_or(0),
        _ => 0,
    }
}

/// An attachment's work object, from its expanded layout; None without a title.
fn card(a: &Value, n: &Names, ts: &str) -> Option<Card> {
    let e = &a["work_object_entity"];
    let l = &e["layouts"]["expanded"];
    let title = text::shortcodes(&any_text(&l["title"]));
    if title.is_empty() {
        return None;
    }
    let pic = |v: &Value| v.as_str().filter(|u| !u.is_empty()).map(|u| block_img(u, ""));
    let fields = elements(&l["fields"])
        .filter_map(|f| {
            let mut tags: Vec<Tag> = f["tag"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|g| Tag { text: any_text(&g["text"]), color: str_of(&g["color"]), icon: pic(&g["icon_url"]) })
                .filter(|t| !t.text.is_empty())
                .collect();
            if tags.is_empty() {
                let t = [any_text(&f["text"]), any_text(&f["value"])].into_iter().find(|t| !t.is_empty())?;
                tags.push(Tag { text: t, color: String::new(), icon: None });
            }
            Some((any_text(&f["label"]), tags))
        })
        .collect();
    // A button that is also among the fallback blocks answers as that one.
    let ctl = |x: &Value| {
        let mut b = control(x, n, x["block_id"].as_str().unwrap_or(""), ts)?;
        for blk in a["blocks"].as_array().into_iter().flatten().filter(|b| b["type"] == "actions") {
            if let Some(y) = elements(blk).find(|y| y["type"] == x["type"] && y["action_id"] == x["action_id"]) {
                b.block_id = str_of(&blk["block_id"]);
                b.value = str_of(&y["value"]);
            }
        }
        Some(b)
    };
    let acts = &l["actions"];
    let first = |vs: [&Value; 3]| vs.iter().find_map(|v| v.as_str().filter(|s| !s.is_empty())).unwrap_or("").to_string();
    let header = [any_text(&l["header_title"]), str_of(&e["app_name"]), str_of(&e["product_name"])].into_iter().find(|s| !s.is_empty());
    Some(Card {
        header: header.unwrap_or_default(),
        title,
        subtitle: text::shortcodes(&any_text(&l["subtitle"])),
        icon: pic(&l["product_icon"]["image_url"]).or_else(|| pic(&e["app_icons"]["image_36"])),
        fields,
        entity: str_of(&e["entity_id"]),
        updated: when(&l["updated_at"]),
        ts: ts.to_string(),
        url: first([&e["external_url"], &a["app_unfurl_url"], &a["from_url"]]),
        link: first([&a["from_url"], &a["app_unfurl_url"], &e["external_url"]]),
        actions: acts["primary_actions"].as_array().into_iter().flatten().filter_map(ctl).collect(),
        more: acts["overflow_actions"]["app_actions"].as_array().into_iter().flatten().filter_map(ctl).collect(),
        refresh: Refresh::Idle,
    })
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
    let target = Some(Target::Emoji(name.to_string(), format!(":{name}:")));
    Seg { text: "\u{3000}".into(), style, emoji: Some(crate::emoji::img(name, url)), target, url: None }
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
                    standard_emoji(&std::mem::take(&mut plain), style, out);
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
        standard_emoji(&plain, style, out);
    }
}

/// Text with its standard emoji split out, each knowing its name: hovered
/// it shows, clicked the emoji is copied.
fn standard_emoji(s: &str, style: Style, out: &mut Vec<Seg>) {
    let chars: Vec<(usize, char)> = s.char_indices().collect();
    let (mut from, mut i) = (0, 0);
    while i < chars.len() {
        let (at, c) = chars[i];
        let flag = ('\u{1f1e6}'..='\u{1f1ff}').contains(&c);
        let starts = flag
            || (!c.is_ascii() && (emojis::get(c.encode_utf8(&mut [0; 4])).is_some() || emojis::get(&format!("{c}\u{fe0f}")).is_some()));
        if !starts {
            i += 1;
            continue;
        }
        // The sequence: modifiers, joined emoji, a flag's second letter.
        let mut j = i + 1;
        while j < chars.len() {
            match chars[j].1 {
                '\u{fe0f}' | '\u{20e3}' | '\u{1f3fb}'..='\u{1f3ff}' | '\u{e0020}'..='\u{e007f}' => j += 1,
                '\u{200d}' if j + 1 < chars.len() => j += 2,
                '\u{1f1e6}'..='\u{1f1ff}' if flag && j == i + 1 => j += 1,
                _ => break,
            }
        }
        let end = chars.get(j).map_or(s.len(), |(k, _)| *k);
        let run = &s[at..end];
        let tone = run.chars().find_map(|c| match c {
            '\u{1f3fb}'..='\u{1f3ff}' => Some(c as u32 - 0x1f3fb + 2),
            _ => None,
        });
        let plain: String = run.chars().filter(|c| !('\u{1f3fb}'..='\u{1f3ff}').contains(c)).collect();
        let found = emojis::get(&plain).or_else(|| emojis::get(&plain.replace('\u{fe0f}', ""))).and_then(|e| e.shortcode());
        if let Some(code) = found {
            if from < at {
                out.push(Seg::new(&s[from..at], style));
            }
            let name = match tone {
                Some(t) => format!("{code}::skin-tone-{t}"),
                None => code.to_string(),
            };
            out.push(Seg { target: Some(Target::Emoji(name, run.to_string())), ..Seg::new(run, style) });
            from = end;
        }
        i = j;
    }
    if from < s.len() {
        out.push(Seg::new(&s[from..], style));
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
            (Some(u), _) => {
                let name = match e["skin_tone"].as_u64() {
                    Some(t) => format!("{}::skin-tone-{t}", s("name")),
                    None => s("name").to_string(),
                };
                Seg { target: Some(Target::Emoji(name, u.clone())), ..Seg::new(u, style) }
            }
            (None, Some(url)) => custom_emoji(s("name"), url, style),
            (None, None) => Seg::new(text::shortcodes(&format!(":{}:", s("name"))), style),
        },
        // A link an app turned into a card: its icon and title on the
        // mentions' tint, as in Slack.
        "attachment_mention" => {
            let url = Some(s("url").to_string()).filter(|u| !u.is_empty());
            let shown = if s("text").is_empty() { s("url") } else { s("text") };
            let style = style.fg(PILL_FG).bg(PILL_BG);
            let icon = e["icon_url"].as_str().filter(|u| !u.is_empty());
            let lead = Seg { text: "\u{a0}".into(), style, url: url.clone(), ..Default::default() };
            push(out, lead);
            if let Some(u) = icon {
                out.push(Seg { text: "\u{3000}".into(), style, url: url.clone(), emoji: Some(block_img(u, "")), ..Default::default() });
                out.push(Seg { text: "\u{a0}".into(), style, url: url.clone(), ..Default::default() });
            }
            out.push(Seg { text: format!("{}\u{a0}", text::shortcodes(shown).replace(' ', "\u{a0}")), style, url, ..Default::default() });
            return;
        }
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
    out.pics.extend(row.pics.into_iter().map(|(x, img)| Pic { line, x: x as u16, cols: 2, rows: 1, img, full: None, user: None }));
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
                image(&mut out, img, None, *w, *h, width, font);
            }
            Piece::Bar(color, inner) => {
                let sub = render(inner, width.saturating_sub(2), font, base);
                nest(&mut out, sub, Span::styled("▎ ", Style::new().fg(*color)));
            }
            Piece::Share(s) => share_card(&mut out, s, width, font, base),
            Piece::Card(c) => card_box(&mut out, c, width, base),
        }
    }
    out
}

/// A card is at most this wide, as Slack's.
const CARD_MAX: usize = 64;
const CARD_BORDER: Color = Color::Indexed(240);
/// Nerd Font: an arrow out of a box, and a link.
const OPEN: &str = "\u{f08e}";
const LINK: &str = "\u{f0c1}";
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// A field's value on a tint of its color, as in Slack.
fn tag_bg(color: &str) -> Option<Color> {
    Some(match color {
        "green" => Color::Rgb(0x1E, 0x3B, 0x2A),
        "blue" => Color::Rgb(0x15, 0x34, 0x4B),
        "red" => Color::Rgb(0x4B, 0x1C, 0x22),
        "yellow" => Color::Rgb(0x4A, 0x3E, 0x12),
        "orange" => Color::Rgb(0x4D, 0x2E, 0x14),
        "purple" => Color::Rgb(0x36, 0x24, 0x4B),
        "" => return None,
        _ => Color::Indexed(237),
    })
}

/// When the card's data is from: "Updated 5 min ago" once the app says
/// when it changed, else the message's time, "As of today at 15:58".
pub fn card_stamp(c: &Card) -> String {
    use chrono::{Local, TimeZone};
    let now = Local::now();
    let date = |t: &chrono::DateTime<Local>| {
        let (d, mo) = (chrono::Datelike::day(t), crate::i18n::month(t));
        let year = chrono::Datelike::year(t);
        let other = year != chrono::Datelike::year(&now);
        match (crate::i18n::is_pt(), other) {
            (true, false) => format!("{d} de {mo}"),
            (true, true) => format!("{d} de {mo} de {year}"),
            (false, false) => format!("{mo} {}", crate::i18n::ordinal(d)),
            (false, true) => format!("{mo} {}, {year}", crate::i18n::ordinal(d)),
        }
    };
    if c.updated > 0 {
        let Some(t) = Local.timestamp_opt(c.updated, 0).single() else { return String::new() };
        let secs = (now - t).num_seconds().max(0);
        let (mins, hours) = (secs / 60, secs / 3600);
        let days = (now.date_naive() - t.date_naive()).num_days();
        return match () {
            _ if secs < 60 => tr("Updated just now", "Atualizado agora").into(),
            _ if mins < 60 => trf!("Updated {mins} min ago", "Atualizado há {mins} min"),
            _ if hours == 1 => tr("Updated 1 hour ago", "Atualizado há 1 hora").into(),
            _ if hours < 24 => trf!("Updated {hours} hours ago", "Atualizado há {hours} horas"),
            _ if days == 1 => tr("Updated yesterday", "Atualizado ontem").into(),
            _ if days <= 7 => trf!("Updated on {}", "Atualizado {}", crate::i18n::weekday(&t)),
            _ => trf!("Updated on {}", "Atualizado em {}", date(&t)),
        };
    }
    let Some(t) = Local.timestamp_opt(text::ts_ms(&c.ts) / 1000, 0).single() else { return String::new() };
    let clock = crate::i18n::clock(&t);
    match (now.date_naive() - t.date_naive()).num_days() {
        0 => trf!("As of today at {clock}", "Atualizado hoje às {clock}"),
        1 => trf!("As of yesterday at {clock}", "Atualizado ontem às {clock}"),
        2..=7 => trf!("As of {} at {clock}", "Atualizado {} às {clock}", crate::i18n::weekday(&t)),
        _ => trf!("As of {}", "Atualizado em {}", date(&t)),
    }
}

/// Slack's card for an app's link: its name (a menu) above a rounded box
/// with Open and Copy link on its top edge; the icon and title, the kind,
/// the fields as label over value; under a line, when its data is from with
/// Refresh, and the app's buttons on the right.
fn card_box(out: &mut Out, c: &Card, width: usize, base: Style) {
    let cw = width.min(CARD_MAX).max(12);
    let inner = cw - 4;
    let border = Style::new().fg(CARD_BORDER);
    let side = |out: &mut Out, row: Row| {
        let mut r = Row::default();
        r.push("│ ".into(), border, None);
        r.append(row);
        r.pad_to(cw - 2);
        r.push(" │".into(), border, None);
        push_row(out, r);
    };
    if !c.header.is_empty() {
        let mut r = Row::default();
        let menu = Target::CardMenu { url: c.url.clone(), link: c.link.clone(), more: c.more.clone() };
        r.push(format!("{} ▾", truncate(&c.header, cw - 2)), dim(), Some(menu));
        push_row(out, r);
    }
    // The bar Slack shows over a hovered card, on the top edge here.
    let mut tools = Row::default();
    if !c.url.is_empty() {
        let blue = Style::new().fg(Color::Blue);
        tools.push(format!(" {OPEN} {} ", tr("Open", "Abrir")), blue, Some(Target::Link(c.url.clone())));
        tools.push("·".into(), border, None);
        tools.push(format!(" {LINK} {} ", tr("Copy link", "Copiar link")), blue, Some(Target::CopyLink(c.link.clone())));
    }
    let mut top = Row::default();
    top.push("╭".into(), border, None);
    if tools.w > 0 && tools.w + 4 <= cw {
        top.push("─".repeat(cw - 3 - tools.w), border, None);
        top.append(tools);
        top.push("─".into(), border, None);
    } else {
        top.push("─".repeat(cw - 2), border, None);
    }
    top.push("╮".into(), border, None);
    push_row(out, top);

    // The icon, then the title beside it.
    let text_w = inner.saturating_sub(3).max(4);
    let title = Seg { text: c.title.clone(), style: Style::new().bold(), url: Some(c.url.clone()).filter(|u| !u.is_empty()), ..Default::default() };
    for (k, row) in wrap(&[title], text_w, base).into_iter().enumerate() {
        let mut r = Row::default();
        match (&c.icon, k) {
            (Some(img), 0) => {
                r.pics.push((0, img.clone()));
                r.push("   ".into(), Style::new(), None);
            }
            (None, 0) => r.push(format!("{LINK}  "), dim(), None),
            _ => r.push("   ".into(), Style::new(), None),
        }
        r.append(row);
        side(out, r);
    }
    if !c.subtitle.is_empty() {
        let mut r = Row::default();
        r.push(format!("   {}", truncate(&c.subtitle, text_w)), base.patch(dim()), None);
        side(out, r);
    }
    // Fields side by side while they fit, each its label over its values.
    if !c.fields.is_empty() {
        side(out, Row::default());
        let (mut labels, mut values) = (Row::default(), Row::default());
        for (label, tags) in &c.fields {
            let mut v = Row::default();
            for (i, t) in tags.iter().enumerate() {
                if i > 0 {
                    v.push(" ".into(), Style::new(), None);
                }
                if let Some(img) = &t.icon {
                    v.pics.push((v.w, img.clone()));
                    v.push("  ".into(), Style::new(), None);
                    v.push(" ".into(), Style::new(), None);
                }
                match tag_bg(&t.color) {
                    Some(bg) => v.push(format!(" {} ", truncate(&t.text, text_w.saturating_sub(4))), base.bg(bg), None),
                    None => v.push(truncate(&t.text, text_w.saturating_sub(v.w)), base, None),
                }
            }
            let l = truncate(label, text_w);
            let w = l.width().max(v.w);
            let mut x = if labels.w == 0 && values.w == 0 { 0 } else { labels.w.max(values.w) + 3 };
            if x > 0 && x + w > text_w {
                for r in [std::mem::take(&mut labels), std::mem::take(&mut values)] {
                    let mut row = Row::default();
                    row.push("   ".into(), Style::new(), None);
                    row.append(r);
                    side(out, row);
                }
                x = 0;
            }
            labels.pad_to(x);
            values.pad_to(x);
            labels.push(l, base.patch(dim()), None);
            values.append(v);
        }
        for r in [labels, values] {
            let mut row = Row::default();
            row.push("   ".into(), Style::new(), None);
            row.append(r);
            side(out, row);
        }
    }
    let mut mid = Row::default();
    mid.push(format!("├{}┤", "─".repeat(cw - 2)), border, None);
    push_row(out, mid);

    // When it's from, Refresh, and the app's buttons on the right.
    let mut foot = Row::default();
    match c.refresh {
        Refresh::Busy(f) => foot.push(format!("{} {}", SPINNER[f % SPINNER.len()], tr("Refreshing…", "Atualizando…")), dim(), None),
        Refresh::Failed => foot.push(tr("Failed to refresh", "Falha ao atualizar").into(), Style::new().fg(Color::Red), None),
        Refresh::Idle => {
            foot.push(truncate(&card_stamp(c), inner), dim(), None);
            if !c.entity.is_empty() {
                let label = tr("Refresh", "Atualizar");
                if foot.w + 3 + label.width() <= inner {
                    foot.push(" · ".into(), dim(), None);
                    foot.push(label.into(), Style::new().fg(Color::Blue), Some(Target::Refresh { ts: c.ts.clone(), entity: c.entity.clone() }));
                }
            }
        }
    }
    let mut buttons = Row::default();
    for b in &c.actions {
        if buttons.w > 0 {
            buttons.push(" ".into(), Style::new(), None);
        }
        let mut p = pill(b, inner);
        // Slack dims them while the app is asked.
        if c.refresh != Refresh::Idle {
            p.hits.clear();
            p.spans.iter_mut().for_each(|s| s.style = s.style.add_modifier(Modifier::DIM));
        }
        buttons.append(p);
    }
    if buttons.w > 0 && foot.w + 2 + buttons.w <= inner {
        foot.pad_to(inner - buttons.w);
        foot.append(buttons);
        side(out, foot);
    } else {
        side(out, foot);
        if buttons.w > 0 {
            let mut rows = Out::default();
            controls(&mut rows, &c.actions, inner);
            for (i, line) in rows.lines.into_iter().enumerate() {
                let mut r = Row::default();
                r.spans = line.spans;
                r.w = r.spans.iter().map(|s| s.width()).sum();
                r.hits = rows.hits.iter().filter(|h| h.line == i && c.refresh == Refresh::Idle).map(|h| (h.x as usize, h.w as usize, h.target.clone())).collect();
                side(out, r);
            }
        }
    }
    let mut bottom = Row::default();
    bottom.push(format!("╰{}╯", "─".repeat(cw - 2)), border, None);
    push_row(out, bottom);
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
            if s.url.is_some() || s.target.as_ref().is_some_and(|t| !matches!(t, Target::Emoji(..))) {
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
        out.pics.extend(row.pics.into_iter().map(|(x, img)| Pic { line, x: x as u16, cols, rows, img, full: None, user: None }));
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
    out.cards.extend(sub.cards.into_iter().map(|c| FileCard { line: top + c.line, x: dx + c.x, ..c }));
}

/// A shared message's card behind a quiet bar: the photo two rows tall with
/// the name and day beside it, where it was posted under them, then its text
/// and files. Every line of it opens the original; what answers a click
/// itself (a link, a picture, a file, the photo) is found first.
fn share_card(out: &mut Out, s: &Share, width: usize, font: Option<(u16, u16)>, base: Style) {
    let w = width.saturating_sub(2).max(4);
    let mut sub = Out::default();
    let photo = font.zip(s.photo.as_ref()).map(|((fw, fh), (id, url))| {
        let cols = ((2.0 * fh as f64 / fw.max(1) as f64).round() as u16).clamp(3, 6);
        let img = ImgRef::avatar_px(id, url, 2 * fh as u32);
        (cols, img)
    });
    let pad = photo.as_ref().map_or(0, |(cols, _)| *cols as usize + 1);
    if let Some((cols, img)) = photo {
        sub.pics.push(Pic { line: 0, x: 0, cols, rows: 2, img, full: None, user: s.user.clone() });
    }
    let mut head = Row::default();
    head.pad_to(pad);
    head.push(truncate(&s.author, w.saturating_sub(pad + 2)), base.bold(), None);
    if !s.when.is_empty() && head.w + 2 + s.when.width() <= w {
        head.push(format!("  {}", s.when), dim(), None);
    }
    push_row(&mut sub, head);
    let mut place = Row::default();
    place.pad_to(pad);
    for r in wrap(&s.place, w.saturating_sub(pad), base).into_iter().take(1) {
        place.append(r);
    }
    push_row(&mut sub, place);
    let body = render(&s.body, w, font, base);
    let top = sub.lines.len();
    sub.lines.extend(body.lines);
    sub.pics.extend(body.pics.into_iter().map(|p| Pic { line: top + p.line, ..p }));
    sub.hits.extend(body.hits.into_iter().map(|h| Hit { line: top + h.line, ..h }));
    sub.cards.extend(body.cards.into_iter().map(|c| FileCard { line: top + c.line, ..c }));
    // Pictures first, then the other files' cards, as under a message.
    for f in s.files.iter().filter(|f| f.thumb.is_some()) {
        if let Some(img) = ImgRef::thumb(f) {
            image(&mut sub, &img, ImgRef::full(f), f.w, f.h, w, font);
        }
    }
    for f in s.files.iter().filter(|f| f.thumb.is_none()) {
        sub.cards.push(FileCard { line: sub.lines.len(), x: 0, w: w as u16, file: f.clone() });
        sub.lines.extend([Line::default(), Line::default()]);
    }
    let first = out.lines.len();
    nest(out, sub, Span::styled("▎ ", dim()));
    if let Some(t) = &s.open {
        out.hits.extend((first..out.lines.len()).map(|line| Hit { line, x: 0, w: width as u16, target: t.clone() }));
    }
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
                out.pics.push(Pic { line: top, x: (width - cols as usize) as u16, cols, rows, img: img.clone(), full: None, user: None });
                while out.lines.len() < top + rows as usize {
                    out.lines.push(Line::default());
                }
            } else {
                rows_of(paras, width, base).into_iter().for_each(|r| push_row(out, r));
                out.pics.push(Pic { line: out.lines.len(), x: 0, cols, rows, img: img.clone(), full: None, user: None });
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
fn image(out: &mut Out, img: &ImgRef, full: Option<ImgRef>, w: u32, h: u32, width: usize, font: Option<(u16, u16)>) {
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
    out.pics.push(Pic { line: out.lines.len(), x: 0, cols, rows, img: img.clone(), full, user: None });
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permalink_reads_archives_link_with_thread_ts() {
        let p = permalink("https://acme.slack.com/archives/C0123/p1760000000000100?thread_ts=1760000000.000050&cid=C0123").unwrap();
        assert_eq!(p.chan, "C0123");
        assert_eq!(p.ts.as_deref(), Some("1760000000.000100"));
        assert_eq!(p.thread.as_deref(), Some("1760000000.000050"));
    }

    #[test]
    fn permalink_reads_web_client_thread_and_rejects_other_hosts() {
        let p = permalink("https://app.slack.com/client/T0AAA/C0123/thread/C0123-1760000000.000050").unwrap();
        assert_eq!(p.chan, "C0123");
        // A thread's own link is at its first message.
        assert_eq!(p.ts.as_deref(), Some("1760000000.000050"));
        assert_eq!(p.thread.as_deref(), Some("1760000000.000050"));
        let p = permalink("https://acme.slack.com/archives/C0123").unwrap();
        assert_eq!((p.ts, p.thread), (None, None));
        assert!(permalink("https://example.com/archives/C0123/p1760000000000100").is_none());
        assert!(permalink("https://notslack.com/archives/C0123").is_none());
    }
}
