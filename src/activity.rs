//! Activity, as Slack's: what happened that concerns you, from the web
//! client's own `activity.feed` (mentions, thread replies, reactions to your
//! messages, DMs, invitations), newest first and by day, with All /
//! Mentions / Threads / Reactions tabs. Recent channels is ours, not the
//! feed's: the channels you're in and haven't muted, by their latest
//! message. A row below "Threads" opens it in place of the conversation;
//! Enter opens an item, `m` its menu (mark read, copy the link).

use crate::app::{Action, App, Event, Focus, Menu};
use crate::i18n::{self, tr};
use crate::slack::{parse_message, Kind, Message, Res, Slack};
use crate::ui::{dim, truncate};
use chrono::{Datelike, Local, TimeZone};
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Constraint, Layout, Margin, Position, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, ListItem, Paragraph};
use ratatui::Frame;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthStr;

/// nf-fa-bell, like Slack's Activity icon.
pub const ICON: &str = "\u{f0f3}";
const LOCK: &str = "\u{f023}";
const THREAD: &str = "\u{f442}";
const BOX_OFF: &str = "\u{f096}";
const BOX_ON: &str = "\u{f14a}";
/// How often Slack's Activity counts are read, for the dots and the badge.
const COUNTS_EVERY: Duration = Duration::from_secs(60);
/// How often the first page is read again while the view shows.
const FEED_EVERY: Duration = Duration::from_secs(30);
/// Messages fetched per request batch.
const BATCH: usize = 5;
/// Pages read by reaching the list's end, at most.
const AUTO_PAGES: usize = 10;
/// Recent channels listed at most.
const RECENT: usize = 50;

const MENTIONS: &str = "at_user,at_user_group,at_channel,at_everyone,keyword,unjoined_channel_mention";

/// The view's tabs.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum Tab {
    #[default]
    All,
    Mentions,
    Threads,
    Reactions,
    Recent,
}

const TABS: [Tab; 5] = [Tab::All, Tab::Mentions, Tab::Threads, Tab::Reactions, Tab::Recent];

impl Tab {
    fn label(self) -> &'static str {
        match self {
            Tab::All => tr("All", "Tudo"),
            Tab::Mentions => tr("Mentions", "Menções"),
            Tab::Threads => tr("Threads", "Conversas"),
            Tab::Reactions => tr("Reactions", "Reações"),
            Tab::Recent => tr("Recent channels", "Canais recentes"),
        }
    }

    /// What the feed is asked for, as the web client does.
    fn types(self) -> String {
        match self {
            Tab::All => format!("{MENTIONS},thread_v2,message_reaction,bot_dm_bundle,dm,internal_channel_invite,external_channel_invite"),
            Tab::Mentions => MENTIONS.into(),
            Tab::Threads => "thread_v2".into(),
            Tab::Reactions => "message_reaction".into(),
            Tab::Recent => String::new(),
        }
    }
}

/// One entry of the feed: where its message is and what it says about it.
#[derive(Clone, Debug)]
pub struct Entry {
    pub key: String,
    pub ty: String,
    /// When it happened (`feed_ts`).
    pub when: String,
    pub unread: bool,
    pub chan: String,
    pub ts: String,
    pub thread: Option<String>,
    /// Who wrote it, or who invited you.
    pub by: String,
    /// A reaction to your message: who and which emoji.
    pub reaction: Option<(String, String)>,
    /// New messages it stands for: a thread's, an app's.
    pub count: u64,
}

fn s(v: &Value) -> String {
    v.as_str().unwrap_or("").to_string()
}

/// A feed item, as `activity.feed` sends it; none when it points nowhere.
fn entry(it: &Value) -> Option<Entry> {
    let i = &it["item"];
    let p = &i["bundle_info"]["payload"];
    let m = &i["message"];
    let ty = s(&i["type"]);
    let mut e = Entry {
        key: s(&it["key"]),
        when: s(&it["feed_ts"]),
        unread: it["is_unread"].as_bool().unwrap_or(false),
        chan: s(&m["channel"]),
        ts: s(&m["ts"]),
        thread: m["thread_ts"].as_str().filter(|t| !t.is_empty()).map(String::from),
        by: s(&m["author_user_id"]),
        reaction: i["reaction"].is_object().then(|| (s(&i["reaction"]["user"]), s(&i["reaction"]["name"]))),
        count: 0,
        ty,
    };
    match e.ty.as_str() {
        // Its newest message from the other side.
        "dm" => {
            e.chan = s(&p["dm_entry"]["latest_message"]["channel"]);
            e.ts = e.when.clone();
        }
        "thread_v2" => {
            let t = &p["thread_entry"];
            e.chan = s(&t["channel_id"]);
            e.ts = s(&t["latest_ts"]);
            e.thread = t["thread_ts"].as_str().map(String::from);
            e.count = t["unread_msg_count"].as_u64().unwrap_or(0);
        }
        "bot_dm_bundle" => {
            e.chan = s(&p["message"]["channel"]);
            e.ts = s(&p["message"]["ts"]);
            e.count = i["bundle_info"]["unread_count"].as_u64().unwrap_or(0);
        }
        _ => {}
    }
    (!e.chan.is_empty() && !e.ts.is_empty()).then_some(e)
}

pub enum Ev {
    /// A page of the feed and the cursor to the next; `serial` tells a stale one.
    Feed { serial: u64, more: bool, res: Res<(Vec<Entry>, String)> },
    /// `client.counts`' unread Activity entries by type.
    Counts(Res<HashMap<String, u64>>),
    /// Messages the rows point at: none when gone.
    Msgs(Vec<(String, Option<Message>)>),
    /// An entry marked read or unread in Slack.
    Marked(Res<()>),
}

/// A menu entry of a row.
#[derive(Clone)]
pub enum Act {
    Open(String),
    Mark(String, bool),
}

/// A message a row on screen needs: its key, where it is, and whether it
/// must be that very message (a recent channel takes its newest).
#[derive(Clone)]
struct Want {
    key: String,
    chan: String,
    ts: String,
    thread: Option<String>,
    exact: bool,
}

#[derive(Default)]
pub struct Activity {
    /// The view is in the main pane, in place of a conversation.
    pub open: bool,
    tab: Tab,
    /// Only what's unread.
    unreads: bool,
    entries: Vec<Entry>,
    cursor: String,
    pages: usize,
    loaded: bool,
    fetching: bool,
    error: Option<String>,
    serial: u64,
    /// Entries marked read (false) or unread (true) here, until the feed is read again.
    marks: HashMap<String, bool>,
    /// Slack's unread entries by type.
    counts: HashMap<String, u64>,
    counts_at: Option<Instant>,
    feed_at: Option<Instant>,
    /// Messages fetched for the rows, by `chan/ts`: none when gone.
    msgs: HashMap<String, Option<Message>>,
    asked: HashSet<String>,
    /// Set by the last draw: messages the rows on screen lack.
    want: Vec<Want>,
    /// Set by the last draw: the list's end shows, so the next page comes.
    at_end: bool,
    pub sel: usize,
    top: usize,
    max_top: usize,
    page: usize,
    reveal: bool,
}

impl Activity {
    fn unread(&self, e: &Entry) -> bool {
        self.marks.get(&e.key).copied().unwrap_or(e.unread)
    }

    /// Slack's unread entries of these types.
    fn count(&self, types: &str) -> u64 {
        types.split(',').filter_map(|t| self.counts.get(t)).sum()
    }
}

/// Where the last draw put things, for the mouse.
#[derive(Default)]
pub struct Areas {
    pub list: Rect,
    pub tabs: Vec<(Rect, Tab)>,
    pub unreads: Rect,
    /// Each row on screen and its place in the list.
    pub rows: Vec<(Rect, usize)>,
}

impl Slack {
    /// A page of Slack's Activity, newest first. Undocumented, like `counts`:
    /// session tokens only.
    pub fn activity_feed(&self, types: &str, cursor: &str, unread: bool) -> Res<(Vec<Entry>, String)> {
        let unread = if unread { "true" } else { "false" };
        let mut p = vec![
            ("types", types),
            ("mode", "chrono_v1"),
            ("limit", "20"),
            ("archive_only", "false"),
            ("unread_only", unread),
            ("priority_only", "false"),
            ("is_activity_inbox", "true"),
        ];
        if !cursor.is_empty() {
            p.push(("cursor", cursor));
        }
        let v = self.call("activity.feed", &p)?;
        let list = v["items"].as_array().into_iter().flatten().filter_map(entry).collect();
        Ok((list, s(&v["response_metadata"]["next_cursor"])))
    }

    /// The unread Activity entries by type, from `client.counts`.
    pub fn activity_counts(&self) -> Res<HashMap<String, u64>> {
        let v = self.call("client.counts", &[])?;
        let map = v["activity_v2"].as_object().into_iter().flatten().filter_map(|(k, n)| Some((k.clone(), n.as_u64()?))).collect();
        Ok(map)
    }

    /// Marks an entry read or unread, as the web client does.
    pub fn activity_mark(&self, ty: &str, key: &str, when: &str, read: bool) -> Res<()> {
        if read {
            self.call("activity.markRead", &[("type", ty), ("key", key), ("ts", when), ("feed_ts", when)]).map(|_| ())
        } else {
            let entries = json!([{ "type": ty, "key": key, "ts": when, "feed_ts": when }]).to_string();
            self.call("activity.markUnread", &[("entries", &entries)]).map(|_| ())
        }
    }

    /// One message: the reply in its thread, else the one at `ts` in the
    /// conversation (or, not `exact`, its newest up to there).
    fn activity_message(&self, chan: &str, ts: &str, thread: Option<&str>, exact: bool) -> Res<Option<Message>> {
        let in_thread = |parent: &str| -> Res<Option<Message>> {
            let p = [("channel", chan), ("ts", parent), ("oldest", ts), ("latest", ts), ("inclusive", "true"), ("limit", "2")];
            let v = self.call("conversations.replies", &p)?;
            Ok(v["messages"].as_array().into_iter().flatten().map(parse_message).find(|m| m.ts == ts))
        };
        if let Some(parent) = thread.filter(|t| *t != ts) {
            return in_thread(parent);
        }
        let v = self.call("conversations.history", &[("channel", chan), ("latest", ts), ("inclusive", "true"), ("limit", "1")])?;
        match v["messages"].as_array().and_then(|a| a.first()).map(parse_message) {
            Some(m) if !exact || m.ts == ts => Ok(Some(m)),
            _ if exact => in_thread(ts),
            _ => Ok(None),
        }
    }
}

/// A row as the view shows it, from a feed entry or a recent channel.
struct Item {
    key: String,
    /// The entry it comes from; none for a recent channel.
    entry: Option<Entry>,
    chan: String,
    ts: String,
    thread: Option<String>,
    when: String,
    unread: bool,
    /// The mentions or new messages it stands for.
    news: u64,
    icon: &'static str,
    place: String,
    who: String,
    /// What it did, after the name: "reacted 👍", "invited you".
    did: String,
    text: String,
    want: Option<Want>,
}

/// The conversation as a row names it, and its icon.
fn place(app: &App, chan: &str) -> (&'static str, String) {
    match app.convs.iter().find(|c| c.id == chan) {
        Some(c) => match c.kind {
            Kind::Channel => ("#", c.name.clone()),
            Kind::Private => (LOCK, c.name.clone()),
            Kind::Direct | Kind::Group => ("\u{f4ad}", app.label(c)),
        },
        None => ("#", app.chans.get(chan).or_else(|| app.others.get(chan)).cloned().unwrap_or_else(|| chan.to_string())),
    }
}

fn name(app: &App, uid: &str) -> String {
    if uid.is_empty() {
        return String::new();
    }
    if app.is_me(uid) {
        return tr("You", "Você").into();
    }
    app.users.names.get(uid).cloned().unwrap_or_else(|| uid.to_string())
}

/// The message's text on one line.
fn flat(app: &App, m: &Message) -> String {
    let text = crate::text::body(m, &app.names());
    text.lines().map(|l| l.trim_start_matches('▎').trim()).filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" ")
}

/// A message the app has: the conversation's, else one fetched here.
/// `Some(None)`: gone.
fn known(app: &App, chan: &str, ts: &str) -> Option<Option<Message>> {
    if let Some(m) = app.messages.get(chan).and_then(|l| l.binary_search_by(|m| m.ts.as_str().cmp(ts)).ok().map(|i| l[i].clone())) {
        return Some(Some(m));
    }
    app.activity.msgs.get(&format!("{chan}/{ts}")).cloned()
}

/// The channels you're in and haven't muted, the latest message first.
fn recent(app: &App) -> Vec<Item> {
    let mut list: Vec<(String, String, bool, u64)> = app
        .convs
        .iter()
        .filter(|c| matches!(c.kind, Kind::Channel | Kind::Private) && !c.archived && !c.guest && !app.muted(c))
        .filter_map(|c| {
            let latest = app.counts.get(&c.id).map(|k| k.latest.clone()).filter(|l| !l.is_empty())?;
            let (unread, mentions) = app.unread(&c.id);
            Some((c.id.clone(), latest, unread || mentions > 0, mentions))
        })
        .filter(|r| !app.activity.unreads || r.2)
        .collect();
    list.sort_by(|a, b| b.1.cmp(&a.1));
    list.truncate(RECENT);
    list.into_iter()
        .map(|(chan, latest, unread, mentions)| {
            let (icon, label) = place(app, &chan);
            let key = format!("{chan}/{latest}");
            let loaded = app.messages.get(&chan).and_then(|l| l.last()).filter(|m| m.ts >= latest).cloned();
            let msg = loaded.or_else(|| app.activity.msgs.get(&key).cloned().flatten());
            let fetched = app.activity.msgs.contains_key(&key);
            let want = (msg.is_none() && !fetched).then(|| Want { key: key.clone(), chan: chan.clone(), ts: latest.clone(), thread: None, exact: false });
            let who = msg.as_ref().map(|m| crate::ui::author(m, &app.names())).unwrap_or_else(|| format!("{icon}{label}"));
            Item {
                key: format!("rc/{chan}"),
                entry: None,
                ts: msg.as_ref().map(|m| m.ts.clone()).unwrap_or_default(),
                text: msg.as_ref().map(|m| flat(app, m)).unwrap_or_default(),
                chan,
                thread: None,
                when: latest,
                unread,
                news: mentions,
                icon,
                place: label,
                who,
                did: String::new(),
                want,
            }
        })
        .collect()
}

/// The rows of the open tab.
fn items(app: &App) -> Vec<Item> {
    let a = &app.activity;
    if a.tab == Tab::Recent {
        return recent(app);
    }
    let mut out = Vec::new();
    for e in &a.entries {
        let unread = a.unread(e);
        if a.unreads && !unread {
            continue;
        }
        let msg = known(app, &e.chan, &e.ts);
        // Deleted, or out of reach.
        if matches!(msg, Some(None)) {
            continue;
        }
        let m = msg.flatten();
        let want = m.is_none().then(|| Want { key: format!("{}/{}", e.chan, e.ts), chan: e.chan.clone(), ts: e.ts.clone(), thread: e.thread.clone(), exact: true });
        let invite = e.ty.ends_with("_channel_invite");
        let thread = e.thread.clone().or_else(|| m.as_ref().and_then(|m| m.thread_ts.clone())).filter(|t| *t != e.ts);
        let (mut icon, label) = place(app, &e.chan);
        let mut place = label.clone();
        if e.ty == "thread_v2" || thread.is_some() {
            let hash = if icon == "#" || icon == LOCK { format!("{icon}{label}") } else { label.clone() };
            place = trf!("Thread in {}", "Conversa em {}", hash);
            icon = THREAD;
        } else if e.ty == "bot_dm_bundle" {
            place = tr("App", "App").into();
        } else if invite {
            place = tr("Invitation", "Convite").into();
        }
        if e.count > 1 {
            place += &trf!("  ·  {} new", "  ·  {} novas", e.count);
        }
        let (who, did) = match &e.reaction {
            Some((u, emoji)) if e.ty == "message_reaction" => {
                let glyph = match app.emoji.glyph(emoji) {
                    Some(crate::emoji::Glyph::Text(t)) => t,
                    _ => format!(":{emoji}:"),
                };
                (name(app, u), trf!("reacted {}", "reagiu {}", glyph))
            }
            _ if invite => (name(app, &e.by), trf!("invited you to #{}", "convidou você para #{}", label)),
            _ => match &m {
                Some(m) => (crate::ui::author(m, &app.names()), String::new()),
                None => (name(app, &e.by), String::new()),
            },
        };
        // A DM stands for its conversation's unread messages.
        let news = if e.ty == "dm" {
            let (u, n) = app.unread(&e.chan);
            n.max(u as u64)
        } else {
            e.count
        };
        out.push(Item {
            key: e.key.clone(),
            entry: Some(e.clone()),
            chan: e.chan.clone(),
            ts: e.ts.clone(),
            thread,
            when: e.when.clone(),
            unread,
            news,
            icon,
            place,
            who: if who.is_empty() { "…".into() } else { who },
            did,
            text: if invite { String::new() } else { m.as_ref().map(|m| flat(app, m)).unwrap_or_default() },
            want,
        });
    }
    out
}

/// Whether anything is unread in the sidebar row's eyes, and the mentions.
pub(crate) fn attention(app: &App) -> (bool, u64) {
    let a = &app.activity;
    let any = a.count(&Tab::All.types()) > 0 || a.entries.iter().any(|e| a.unread(e));
    (any, a.count(MENTIONS))
}

/// Something unread under a tab, for its dot.
fn tab_unread(app: &App, t: Tab) -> bool {
    let a = &app.activity;
    match t {
        Tab::Recent => app.convs.iter().any(|c| {
            matches!(c.kind, Kind::Channel | Kind::Private) && !c.archived && !c.guest && !app.muted(c) && {
                let (u, m) = app.unread(&c.id);
                u || m > 0
            }
        }),
        _ => a.count(&t.types()) > 0 || (t == a.tab && a.entries.iter().any(|e| a.unread(e))),
    }
}

impl App {
    /// Opens the view in the main pane, on the tab it was left on.
    pub(crate) fn open_activity(&mut self) {
        self.drop_edit();
        self.selection = None;
        self.current = None;
        self.thread = None;
        self.profile = None;
        self.edit.forget_thread();
        self.filter.clear();
        self.threads.open = false;
        self.activity.open = true;
        (self.activity.sel, self.activity.top) = (0, 0);
        self.focus = Focus::Input;
        self.load_activity(false);
        self.fetch_activity_counts();
    }

    fn fetch_activity_counts(&mut self) {
        self.activity.counts_at = Some(Instant::now());
        self.spawn(|s| Event::Activity(Ev::Counts(s.activity_counts())));
    }

    /// The first page again, or (`more`) the next one.
    fn load_activity(&mut self, more: bool) {
        let a = &mut self.activity;
        a.feed_at = Some(Instant::now());
        if a.tab == Tab::Recent {
            (a.loaded, a.error, a.fetching) = (true, None, false);
            return;
        }
        if more && (a.cursor.is_empty() || a.fetching) {
            return;
        }
        if !more {
            a.serial += 1;
        }
        a.fetching = true;
        let (serial, types, cursor, unread) = (a.serial, a.tab.types(), if more { a.cursor.clone() } else { String::new() }, a.unreads);
        self.spawn(move |s| Event::Activity(Ev::Feed { serial, more, res: s.activity_feed(&types, &cursor, unread) }));
    }

    /// Another tab, or Unreads switched: the list from its start.
    fn reset_activity(&mut self) {
        let a = &mut self.activity;
        (a.entries, a.cursor, a.pages, a.loaded, a.error, a.sel, a.top) = (Vec::new(), String::new(), 0, false, None, 0, 0);
        a.marks.clear();
        self.load_activity(false);
    }

    fn activity_tab(&mut self, t: Tab) {
        if self.activity.tab == t {
            return self.load_activity(false);
        }
        self.activity.tab = t;
        self.reset_activity();
    }

    fn toggle_unreads(&mut self) {
        self.activity.unreads = !self.activity.unreads;
        self.reset_activity();
    }

    /// From `tick`: the counts now and then, the feed while it shows, the
    /// next page at its end, and the messages the rows on screen lack.
    pub(crate) fn poll_activity(&mut self) {
        if self.me.is_none() {
            return;
        }
        if self.activity.counts_at.is_none_or(|t| t.elapsed() >= COUNTS_EVERY) {
            self.fetch_activity_counts();
        }
        if !self.activity.open {
            return;
        }
        let a = &self.activity;
        if !a.fetching && a.pages <= 1 && a.feed_at.is_some_and(|t| t.elapsed() >= FEED_EVERY) {
            self.load_activity(false);
        }
        // A short list (Unreads) doesn't page through the whole feed by itself.
        if std::mem::take(&mut self.activity.at_end) && self.activity.loaded && self.activity.pages < AUTO_PAGES {
            self.load_activity(true);
        }
        let want: Vec<Want> = std::mem::take(&mut self.activity.want).into_iter().filter(|w| self.activity.asked.insert(w.key.clone())).collect();
        let mut want = want.into_iter().peekable();
        while want.peek().is_some() {
            let batch: Vec<Want> = want.by_ref().take(BATCH).collect();
            self.spawn(move |s| {
                let got = batch
                    .into_iter()
                    .filter_map(|w| s.activity_message(&w.chan, &w.ts, w.thread.as_deref(), w.exact).ok().map(|m| (w.key, m)))
                    .collect();
                Event::Activity(Ev::Msgs(got))
            });
        }
    }

    pub(crate) fn on_activity(&mut self, ev: Ev) {
        let a = &mut self.activity;
        match ev {
            Ev::Feed { serial, more, res } => {
                if serial != a.serial {
                    return;
                }
                a.fetching = false;
                a.loaded = true;
                match res {
                    Ok((list, cursor)) => {
                        a.error = None;
                        if more {
                            let have: HashSet<String> = a.entries.iter().map(|e| e.key.clone()).collect();
                            a.entries.extend(list.into_iter().filter(|e| !have.contains(&e.key)));
                            a.pages += 1;
                        } else {
                            let sel = a.entries.get(a.sel).map(|e| e.key.clone());
                            a.entries = list;
                            a.marks.clear();
                            a.pages = 1;
                            // The selection stays on its entry.
                            if let Some(i) = sel.and_then(|k| a.entries.iter().position(|e| e.key == k)) {
                                a.sel = i;
                            }
                        }
                        a.cursor = cursor;
                    }
                    Err(e) => a.error = Some(e),
                }
            }
            Ev::Counts(Ok(c)) => a.counts = c,
            Ev::Counts(Err(_)) => {}
            Ev::Msgs(got) => {
                // Not to grow without end.
                if a.msgs.len() > 1000 {
                    a.msgs.clear();
                    a.asked.clear();
                }
                a.msgs.extend(got);
            }
            Ev::Marked(res) => {
                if let Err(e) = res {
                    self.error = Some(e);
                }
                // Slack's counts again in a moment.
                self.activity.counts_at = Instant::now().checked_sub(COUNTS_EVERY - Duration::from_secs(3));
            }
        }
        self.dirty = true;
    }

    /// An entry read or unread, here at once and in Slack.
    fn mark_activity(&mut self, key: &str, read: bool) {
        let Some(e) = self.activity.entries.iter().find(|e| e.key == key).cloned() else { return };
        let a = &mut self.activity;
        if a.unread(&e) == !read {
            return;
        }
        a.marks.insert(e.key.clone(), !read);
        let n = a.counts.entry(e.ty.clone()).or_default();
        *n = if read { n.saturating_sub(1) } else { *n + 1 };
        self.spawn(move |s| Event::Activity(Ev::Marked(s.activity_mark(&e.ty, &e.key, &e.when, read))));
    }

    /// Opens a row: a recent channel where you stopped reading, an entry's
    /// thread at its reply or its conversation at the message.
    fn open_activity_item(&mut self, i: usize) {
        let Some(it) = items(self).into_iter().nth(i) else { return };
        self.activity.sel = i;
        if let Some(e) = &it.entry {
            if it.unread {
                self.mark_activity(&e.key.clone(), true);
            }
        }
        self.open(it.chan.clone());
        if it.entry.is_none() || it.ts.is_empty() {
            return;
        }
        self.flash = Some((it.ts.clone(), Instant::now()));
        match it.thread {
            Some(parent) => {
                self.open_thread(parent.clone());
                if let Some(t) = self.thread.as_mut() {
                    t.jump = Some(it.ts);
                }
                self.show_message(it.chan, parent);
            }
            None => self.show_message(it.chan, it.ts),
        }
    }

    /// A row's menu: open, mark read (or unread), copy the message's link.
    fn activity_menu(&mut self, i: usize, x: u16, y: u16) {
        let Some(it) = items(self).into_iter().nth(i) else { return };
        self.activity.sel = i;
        let mut menu = vec![(tr("Open", "Abrir").to_string(), Action::Activity(Act::Open(it.key.clone())))];
        match (&it.entry, it.unread) {
            (None, true) => menu.push((tr("Mark as read", "Marcar como lida").into(), Action::MarkRead(it.chan.clone()))),
            (None, false) => {}
            (Some(_), true) => menu.push((tr("Mark as read", "Marcar como lida").into(), Action::Activity(Act::Mark(it.key.clone(), true)))),
            (Some(_), false) => menu.push((tr("Mark as unread", "Marcar como não lida").into(), Action::Activity(Act::Mark(it.key.clone(), false)))),
        }
        if !it.ts.is_empty() {
            let url = self.message_url(&it.chan, &it.ts);
            let url = match &it.thread {
                Some(t) => format!("{url}?thread_ts={t}&cid={}", it.chan),
                None => url,
            };
            menu.push((String::new(), Action::Separator));
            menu.push((tr("Copy link", "Copiar link").into(), Action::CopyLink(url.clone())));
            menu.push((tr("Open in browser", "Abrir no navegador").into(), Action::Browser(url)));
        }
        self.menu = Some(Menu::new(x, y, menu));
    }

    /// A menu entry chosen.
    pub(crate) fn activity_action(&mut self, act: Act) {
        let at = |app: &App, key: &str| items(app).iter().position(|it| it.key == key);
        match act {
            Act::Open(key) => {
                if let Some(i) = at(self, &key) {
                    self.open_activity_item(i);
                }
            }
            Act::Mark(key, read) => self.mark_activity(&key, read),
        }
    }

    fn activity_step(&mut self, delta: isize) {
        let n = items(self).len();
        if n == 0 {
            return;
        }
        let a = &mut self.activity;
        a.sel = (a.sel as isize).saturating_add(delta).clamp(0, n as isize - 1) as usize;
        a.reveal = true;
        if a.sel + 1 == n {
            a.at_end = true;
        }
    }

    fn scroll_activity(&mut self, delta: isize) {
        let a = &mut self.activity;
        a.top = a.top.saturating_add_signed(delta).min(a.max_top);
    }

    /// The wheel: down to the end of the list asks for more of it.
    pub(crate) fn wheel_activity(&mut self, delta: isize) {
        self.scroll_activity(delta);
        if delta > 0 && self.activity.top >= self.activity.max_top {
            self.activity.at_end = true;
        }
    }

    /// Keys for the view. True when handled.
    pub(crate) fn activity_key(&mut self, k: KeyEvent, ctrl: bool) -> bool {
        if !self.activity.open || self.focus != Focus::Input || ctrl || k.code == KeyCode::Tab {
            return false;
        }
        let page = (self.activity.page / 4).max(1) as isize;
        let tab = TABS.iter().position(|t| *t == self.activity.tab).unwrap_or(0);
        match k.code {
            KeyCode::Up | KeyCode::Char('k') => self.activity_step(-1),
            KeyCode::Down | KeyCode::Char('j') => self.activity_step(1),
            KeyCode::Home | KeyCode::Char('g') => self.activity_step(isize::MIN / 2),
            KeyCode::End | KeyCode::Char('G') => self.activity_step(isize::MAX / 2),
            KeyCode::PageUp => self.activity_step(-page),
            KeyCode::PageDown => self.activity_step(page),
            KeyCode::Left | KeyCode::Char('h') => self.activity_tab(TABS[(tab + TABS.len() - 1) % TABS.len()]),
            KeyCode::Right | KeyCode::Char('l') => self.activity_tab(TABS[(tab + 1) % TABS.len()]),
            KeyCode::Char(c @ '1'..='5') => self.activity_tab(TABS[c as usize - '1' as usize]),
            KeyCode::Char('u') => self.toggle_unreads(),
            KeyCode::Enter => self.open_activity_item(self.activity.sel),
            KeyCode::Char('m') | KeyCode::Menu => {
                let sel = self.activity.sel;
                let r = self.areas.activity.rows.iter().find(|(_, i)| *i == sel).map(|(r, _)| *r).unwrap_or(self.areas.activity.list);
                self.activity_menu(sel, r.x + 4, r.y + 1);
            }
            KeyCode::Esc => self.focus = Focus::Sidebar,
            _ => {}
        }
        true
    }

    /// Clicks and the wheel on the view. True when handled.
    pub(crate) fn activity_mouse(&mut self, m: MouseEvent, at: Position) -> bool {
        if !self.activity.open {
            return false;
        }
        let a = &self.areas.activity;
        if let MouseEventKind::Down(MouseButton::Left) = m.kind {
            if let Some(&(_, t)) = a.tabs.iter().find(|(r, _)| r.contains(at)) {
                self.focus = Focus::Input;
                self.activity_tab(t);
                return true;
            }
            if a.unreads.contains(at) {
                self.focus = Focus::Input;
                self.toggle_unreads();
                return true;
            }
        }
        if !a.list.contains(at) {
            return false;
        }
        let row = a.rows.iter().find(|(r, _)| r.contains(at)).map(|(_, i)| *i);
        match m.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => self.wheel(&m, crate::wheel::Pane::Activity),
            MouseEventKind::Down(MouseButton::Left) => {
                self.focus = Focus::Input;
                if let Some(i) = row {
                    self.open_activity_item(i);
                }
            }
            MouseEventKind::Down(MouseButton::Right) => {
                self.focus = Focus::Input;
                if let Some(i) = row {
                    self.activity_menu(i, m.column, m.row);
                }
            }
            _ => return false,
        }
        true
    }
}

/// "just now", "5 min", then the time: the day is over the rows.
fn ago(ts: &str) -> String {
    let secs: i64 = ts.split('.').next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let mins = (Local::now().timestamp() - secs) / 60;
    match mins {
        m if m < 1 => tr("just now", "agora").into(),
        m if m < 60 => format!("{m} min"),
        _ => crate::ui::hhmm(ts),
    }
}

/// The line over a day's rows: none for today's.
fn day(ts: &str) -> Option<String> {
    let secs: i64 = ts.split('.').next()?.parse().ok()?;
    let t = Local.timestamp_opt(secs, 0).single()?;
    let (today, d) = (Local::now().date_naive(), t.date_naive());
    let (n, month) = (t.day(), i18n::month(&t));
    Some(match () {
        _ if d == today => return None,
        _ if today.pred_opt() == Some(d) => tr("Yesterday", "Ontem").to_string(),
        _ if (today - d).num_days() < 7 => i18n::weekday(&t).to_string(),
        _ if t.year() == today.year() => trf!("{month} {n}", "{n} de {month}"),
        _ => trf!("{month} {n}, {}", "{n} de {month} de {}", t.year()),
    })
}

/// The sidebar's row under "Threads": bold with unread activity, the red
/// badge of the mentions.
pub(crate) fn sidebar_row<'a>(app: &App, width: usize) -> ListItem<'a> {
    let (unread, mentions) = attention(app);
    let open = app.activity.open;
    let label = tr("Activity", "Atividade");
    let style = if open {
        Style::new().fg(Color::Cyan).bold()
    } else if unread {
        Style::new().fg(Color::White).bold()
    } else {
        Style::new().fg(Color::Gray)
    };
    let mark = unread && !open;
    let mut spans = vec![crate::ui::unread_lead(app, mark), Span::styled(ICON, if unread || open { style } else { dim() }), Span::raw(" "), Span::styled(label, style)];
    if mentions > 0 {
        let b = format!(" {mentions} ");
        let used = 5 + label.width();
        spans.push(Span::raw(" ".repeat(width.saturating_sub(used + b.width() + 2))));
        spans.push(Span::styled(crate::blocks::CAP_L, Style::new().fg(Color::Red)));
        spans.push(Span::styled(b, Style::new().fg(Color::White).bg(Color::Red).bold()));
        spans.push(Span::styled(crate::blocks::CAP_R, Style::new().fg(Color::Red)));
    }
    crate::ui::unread_fill(app, ListItem::new(Line::from(spans)), mark)
}

/// The Activity view, in place of the conversation: a title, the tabs and
/// Unreads, then the rows by day.
pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    // What the conversation's parts left registered no longer applies.
    let a = &mut app.areas;
    a.images.clear();
    a.avatars.clear();
    a.thread_links.clear();
    a.reactions.clear();
    a.message_rows.clear();
    a.buttons.clear();
    a.file_buttons.clear();
    a.pins.tabs.clear();
    (a.messages, a.input, a.latest, a.unarchive, a.header_star, a.header_name) = Default::default();
    a.header_user = None;
    a.activity = Areas::default();

    let [head, tabs, body] = Layout::vertical([Constraint::Length(2), Constraint::Length(2), Constraint::Min(1)]).areas(area);
    f.render_widget(Block::new().borders(Borders::BOTTOM).border_style(dim()), head);
    let heading = Line::from(vec![Span::raw(" "), Span::styled(ICON, dim()), Span::raw(" "), Span::styled(tr("Activity", "Atividade"), Style::new().bold())]);
    f.render_widget(Paragraph::new(heading), Rect::new(head.x, head.y, head.width, 1));

    // All · Mentions · Threads · Reactions · Recent channels, and Unreads.
    let mut spans = vec![Span::raw(" ")];
    let mut x = tabs.x + 1;
    for t in TABS {
        let active = t == app.activity.tab;
        let label = format!(" {} ", t.label());
        let style = if active { Style::new().fg(Color::Cyan).bold().underlined() } else { Style::new().fg(Color::Gray) };
        let w = label.width() as u16 + 1;
        app.areas.activity.tabs.push((Rect::new(x, tabs.y, w, 1).intersection(tabs), t));
        spans.push(Span::styled(label, style));
        spans.push(if tab_unread(app, t) { Span::styled("•", Style::new().fg(Color::Red).bold()) } else { Span::raw(" ") });
        spans.push(Span::raw(" "));
        x += w + 1;
    }
    let (icon, style) = if app.activity.unreads { (BOX_ON, Style::new().fg(Color::Cyan).bold()) } else { (BOX_OFF, dim()) };
    let chip = format!("{icon} {}", tr("Unreads", "Não lidas"));
    let chip_w = chip.width() as u16 + 3;
    let used: u16 = spans.iter().map(|s| s.content.width() as u16).sum();
    if tabs.width >= used + chip_w + 1 {
        let gap = tabs.width - used - chip_w - 1;
        spans.push(Span::raw(" ".repeat(gap as usize)));
        spans.push(Span::styled(chip, style));
        spans.push(Span::styled(" u", dim()));
        app.areas.activity.unreads = Rect::new(tabs.x + used + gap, tabs.y, chip_w, 1);
    }
    f.render_widget(Paragraph::new(Line::from(spans)), Rect::new(tabs.x, tabs.y, tabs.width, 1));

    let body = body.inner(Margin { horizontal: 1, vertical: 0 });
    app.areas.activity.list = body;
    let width = body.width as usize;
    if width < 24 || body.height < 3 {
        return;
    }
    let list = items(app);
    if list.is_empty() {
        let a = &app.activity;
        let text = match (&a.error, a.loaded) {
            (Some(e), _) => e.clone(),
            (None, false) => tr("loading…", "carregando…").to_string(),
            _ if a.unreads => tr("You're all caught up.", "Você está em dia.").to_string(),
            _ => match a.tab {
                Tab::All => tr("Mentions, thread replies and reactions to your messages show up here.", "Menções, respostas em conversas e reações às suas mensagens aparecem aqui."),
                Tab::Mentions => tr("When someone @mentions you, it shows up here.", "Quando alguém @mencionar você, aparece aqui."),
                Tab::Threads => tr("New replies in threads you follow show up here.", "Novas respostas nas conversas que você segue aparecem aqui."),
                Tab::Reactions => tr("Reactions to your messages show up here.", "Reações às suas mensagens aparecem aqui."),
                Tab::Recent => tr("Channels you haven't muted show up here, the latest message first.", "Canais que você não silenciou aparecem aqui, a mensagem mais recente primeiro."),
            }
            .to_string(),
        };
        f.render_widget(Paragraph::new(vec![Line::from(""), Line::from(text).style(dim())]), body);
        return;
    }

    // The whole list as lines; each row is three, by day.
    let focus_here = app.focus == Focus::Input;
    let sel = app.activity.sel.min(list.len() - 1);
    app.activity.sel = sel;
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut rows: Vec<(usize, usize)> = Vec::new();
    let mut last_day: Option<String> = None;
    for (i, it) in list.iter().enumerate() {
        let d = day(&it.when);
        if d != last_day {
            if let Some(d) = &d {
                lines.push(Line::from(Span::styled(format!(" {d}"), dim().bold())));
                lines.push(Line::from(""));
            }
            last_day = d;
        }
        let bar = if i == sel && focus_here { Span::styled("▌", Style::new().fg(Color::Cyan)) } else { Span::raw(" ") };
        let dot = if it.unread { Span::styled("● ", Style::new().fg(Color::Rgb(0x1D, 0x9B, 0xD1))) } else { Span::raw("  ") };
        let strong = if it.unread { Style::new().fg(Color::White).bold() } else { Style::new().bold() };
        let when = ago(&it.when);
        let place = truncate(&format!("{} {}", it.icon, it.place), width.saturating_sub(when.width() + 5));
        let pad = width.saturating_sub(3 + place.width() + when.width());
        lines.push(Line::from(vec![bar.clone(), dot, Span::styled(place, if it.unread { strong } else { dim() }), Span::raw(" ".repeat(pad)), Span::styled(when, dim())]));
        let mut second = vec![bar.clone(), Span::raw("  "), Span::styled(truncate(&it.who, width.saturating_sub(12)), strong)];
        if !it.did.is_empty() {
            second.push(Span::raw(" "));
            second.push(Span::raw(it.did.clone()));
        }
        if it.news > 0 {
            second.push(Span::raw(" "));
            second.push(Span::styled(crate::blocks::CAP_L, Style::new().fg(Color::Red)));
            second.push(Span::styled(format!(" {} ", it.news), Style::new().fg(Color::White).bg(Color::Red).bold()));
            second.push(Span::styled(crate::blocks::CAP_R, Style::new().fg(Color::Red)));
        }
        let y0 = lines.len();
        lines.push(Line::from(second));
        let text = if it.text.is_empty() && it.want.is_some() { tr("loading…", "carregando…").to_string() } else { it.text.clone() };
        let text_style = if it.unread { Style::new() } else { dim() };
        lines.push(Line::from(vec![bar, Span::raw("  "), Span::styled(truncate(&text, width.saturating_sub(4)), text_style)]));
        rows.push((y0 - 1, lines.len()));
        lines.push(Line::from(""));
    }

    // Scrolled so the selected row shows, when it just changed.
    let page = body.height as usize;
    let a = &mut app.activity;
    a.page = page;
    a.max_top = lines.len().saturating_sub(page);
    if std::mem::take(&mut a.reveal) {
        if let Some(&(y0, y1)) = rows.get(sel) {
            // The day's line over the first row of a day comes along.
            let y0 = y0.saturating_sub(2);
            if y0 < a.top {
                a.top = y0;
            } else if y1 > a.top + page {
                a.top = (y1 - page).min(y0);
            }
        }
    }
    a.top = a.top.min(a.max_top);
    let top = a.top;
    let end = (top + page).min(lines.len());
    if end == lines.len() {
        a.at_end = true;
    }
    f.render_widget(Paragraph::new(lines[top..end].to_vec()), body);
    let mut want = Vec::new();
    for (i, &(y0, y1)) in rows.iter().enumerate() {
        let (from, to) = (y0.max(top), y1.min(end));
        if from < to {
            app.areas.activity.rows.push((Rect::new(body.x, body.y + (from - top) as u16, body.width, (to - from) as u16), i));
        }
        // Rows near the screen get their message.
        if y1 + page >= top && y0 <= end + page {
            want.extend(list[i].want.clone());
        }
    }
    app.activity.want = want;
}
