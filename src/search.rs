//! The search window, Ctrl+K (Ctrl+T, or a click on the sidebar's box): a
//! large box over everything, as in Slack's and other web apps' search.
//! The query finds channels and people here, with the sidebar's matcher
//! (`App::conv_matches`, `text::rank`), and messages on Slack's side
//! (`search.messages`), under four tabs: All, Messages, Channels, People.
//! With nothing typed it is a quick switcher: where you've been, then
//! what's unread, then what moved last. On the right, a preview of the
//! result picked: the messages around a found one, a conversation's
//! latest, or someone's profile. Enter opens it, a message at its place.
//! Ctrl+F opens it on the open conversation (`in:#eng `): there Slack's
//! prefix search over its recent messages (`search.inline`) comes first.

use crate::app::{App, Event, Row};
use crate::editor::Editor;
use crate::i18n::{self, tr};
use crate::slack::{Conversation, Kind, Message, Profile, Res, Slack};
use crate::ui::{dim, rounded_input, truncate, ARCHIVE, LOCK};
use chrono::{Datelike, Local, TimeZone};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Margin, Position, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use ratatui::Frame;
use serde_json::Value;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::time::{Duration, Instant};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// How long the typing must stop before Slack is asked.
const PAUSE: Duration = Duration::from_millis(300);
/// How long the selection must rest before its preview is asked.
const PREVIEW_PAUSE: Duration = Duration::from_millis(150);
/// Messages a page of Slack's search brings.
const PAGE: &str = "20";
/// Queries whose messages are kept; past this, the oldest go.
const KEEP: usize = 64;
/// Previews kept, the least recently seen going first.
const PREVIEWS: usize = 48;
/// Channels and people "All" shows before the messages; their tabs show more.
const ALL_CHANS: usize = 5;
const ALL_PEOPLE: usize = 3;
/// Results a Channels or People tab lists at most; past them, a note.
const TAB_MAX: usize = 150;
/// The quick switcher's conversations: at most, and filled up to with
/// those that moved last.
const RECENT_MAX: usize = 20;
const RECENT_FILL: usize = 12;
/// Messages around a found one in its preview, on each side.
const AROUND: usize = 5;
/// A conversation's latest messages in its preview.
const LATEST: usize = 8;
/// How near the end of the loaded messages the selection gets before the next page is asked.
const NEAR: usize = 5;
/// Narrower than this, the window has no preview.
const NARROW: u16 = 100;
/// A found message's text, in lines at most.
const CARD_LINES: usize = 3;
/// Slack's marks around what matched, with `highlight=true`.
const MARK_ON: char = '\u{E000}';
const MARK_OFF: char = '\u{E001}';

/// What matched, the same everywhere in the window.
fn lit() -> Style {
    Style::new().fg(Color::Yellow).bold()
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Tab {
    #[default]
    All,
    Msgs,
    Chans,
    People,
}

const TABS: [Tab; 4] = [Tab::All, Tab::Msgs, Tab::Chans, Tab::People];

/// The messages' order: newest first, or Slack's relevance (Ctrl+S).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Sort {
    #[default]
    Date,
    Score,
}

/// One message the search found.
#[derive(Clone, Debug)]
pub struct Hit {
    pub chan: String,
    /// The channel's name as the search sent it, for one not in `convs`.
    chan_name: String,
    kind: Kind,
    user: Option<String>,
    /// A bot's or an integration's name, when there is no user.
    username: String,
    pub ts: String,
    /// As Slack sent it: mrkdwn, with its marks around what matched.
    raw: String,
    /// The thread it replies in, when it is a reply.
    pub thread: Option<String>,
}

/// A page of Slack's answer.
pub struct Page {
    hits: Vec<Hit>,
    pages: usize,
    total: usize,
}

/// A query's messages, as far as they were paged.
#[derive(Default)]
struct Found {
    hits: Vec<Hit>,
    /// The query Slack was asked: as typed, or with a `*` when that found nothing.
    asked: String,
    /// Pages loaded, and how many there are.
    page: usize,
    pages: usize,
    total: usize,
    loading: bool,
    /// The `*` was tried already.
    retried: bool,
    error: Option<String>,
}

/// What a preview shows: the messages around a found one, or a conversation.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    Msg { chan: String, ts: String, thread: Option<String> },
    Conv(String),
    Person(String),
}

#[derive(Default)]
pub struct Preview {
    msgs: Vec<Message>,
    /// A thread: replies were left out between the parent and those shown.
    gap: bool,
    topic: String,
    purpose: String,
    members: Option<u32>,
    /// Someone's profile, for a person's preview.
    profile: Option<Profile>,
    error: Option<String>,
}

/// A conversation's recent messages that match, as `search.inline` finds
/// them: every word a prefix ("gab" finds Gabriel), accents aside.
#[derive(Default)]
struct Recent {
    hits: Vec<Hit>,
    loading: bool,
    error: Option<String>,
}

/// A query with one `in:` and plain words: the conversation it names,
/// those words, and the heading over its recent messages.
struct Scope {
    conv: String,
    words: String,
    title: String,
}

/// Answers from Slack, as `Event::Search`.
pub enum Ev {
    Found { query: String, sort: Sort, page: usize, res: Res<Page> },
    Recent { key: (String, String), res: Res<Vec<Hit>> },
    Preview { key: Key, preview: Preview },
}

/// A conversation in the results: ours, an index in `convs`, or one of
/// the workspace's we are not in, an index in `directory`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Conv {
    Ours(usize),
    Other(usize),
}

/// Someone in the People list: a person (a DM opens, made if need be), or
/// a conversation that goes with people (a group DM, an app's DM).
#[derive(Clone, Debug, PartialEq, Eq)]
enum Who {
    Person(String),
    Conv(Conv),
}

/// What the query finds here, worked out once per query and state of the
/// lists: with no query, the quick switcher's conversations.
#[derive(Default)]
struct Matches {
    /// All's list with no query.
    recent: Vec<Conv>,
    chans: Vec<Conv>,
    people: Vec<Who>,
    /// All: People go first, their best being the better match.
    people_first: bool,
}

/// A result the selection can stop on: a channel, someone (an index in
/// `Matches::people`), a message, or "+N more", which shows its tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Item {
    Conv(Conv),
    Who(usize),
    Msg(usize),
    More(Tab),
}

/// A heading's part of the list: its results and what it leaves to its tab.
struct Part {
    title: String,
    count: Option<usize>,
    items: Vec<Item>,
    more: Option<(usize, Tab)>,
    /// Said when there are no results.
    empty: Option<&'static str>,
    /// Said under the results: more are there than shown.
    narrow: bool,
}

/// The window's state, kept between openings for its caches.
#[derive(Default)]
pub struct Search {
    pub open: bool,
    input: Editor,
    tab: Tab,
    sort: Sort,
    /// The result picked, among those the tab lists (`App::search_items`).
    sel: usize,
    /// The list's first row shown; the wheel moves it apart from the selection.
    scroll: usize,
    wheel: bool,
    /// The query as last seen, and since when: Slack is asked after `PAUSE`.
    seen: (String, Option<Instant>),
    /// Each query's messages, by query and order; `order` evicts the oldest.
    found: HashMap<(String, Sort), Found>,
    order: VecDeque<(String, Sort)>,
    /// Each conversation's recent messages, by conversation and words.
    recent: HashMap<(String, String), Recent>,
    recent_order: VecDeque<(String, String)>,
    /// What the last query matched here: worked out once per query and
    /// state of the lists, not at each frame; emptied at each opening.
    local: RefCell<Option<(String, (u64, usize, usize), Rc<Matches>)>>,
    /// The preview wanted, and since when: asked after `PREVIEW_PAUSE`.
    want: Option<(Key, Instant)>,
    previews: HashMap<Key, Preview>,
    lru: VecDeque<Key>,
    asking: HashSet<Key>,
    /// From the last draw: the window, each result's rows, the tabs, the
    /// furthest scroll and the list's height.
    rect: Rect,
    spots: Vec<(Rect, usize)>,
    tabs: Vec<(Rect, Tab)>,
    sort_rect: Rect,
    max_scroll: usize,
    height: usize,
}

/// Whether a query is worth asking Slack: two characters at least.
fn worth(q: &str) -> bool {
    q.chars().filter(|c| !c.is_whitespace()).count() >= 2
}

/// The query again with a `*` on its last word, for a second try when it
/// found nothing: "deplo" finds "deployed" then.
fn starred(q: &str) -> Option<String> {
    let last = q.split_whitespace().last()?;
    (last.chars().count() >= 2 && !last.ends_with('*') && !last.contains(':')).then(|| format!("{q}*"))
}

/// The thread a permalink points into (`?thread_ts=…`).
fn thread_of(permalink: &str) -> Option<String> {
    let (_, rest) = permalink.split_once("thread_ts=")?;
    Some(rest.split('&').next()?.to_string()).filter(|t| !t.is_empty())
}

/// A page of the messages that match, as Slack's search finds them.
fn fetch(s: &Slack, query: &str, sort: Sort, page: usize) -> Res<Page> {
    let page = page.to_string();
    let sort = if sort == Sort::Score { "score" } else { "timestamp" };
    let v = s.call(
        "search.messages",
        &[("query", query), ("sort", sort), ("sort_dir", "desc"), ("count", PAGE), ("page", &page), ("highlight", "true")],
    )?;
    let m = &v["messages"];
    let s = |v: &Value| v.as_str().unwrap_or("").to_string();
    let hits = m["matches"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| m["channel"]["id"].is_string() && m["ts"].is_string())
        .map(|m| {
            let c = &m["channel"];
            let flag = |k: &str| c[k].as_bool() == Some(true);
            let kind = if flag("is_im") {
                Kind::Direct
            } else if flag("is_mpim") {
                Kind::Group
            } else if flag("is_private") {
                Kind::Private
            } else {
                Kind::Channel
            };
            let ts = s(&m["ts"]);
            // A file or an attachment only: what it is called.
            let mut raw = s(&m["text"]);
            if raw.trim().is_empty() {
                let files = m["files"].as_array().into_iter().flatten().filter_map(|f| f["name"].as_str());
                raw = files.map(|n| format!("📎 {n}")).collect::<Vec<_>>().join(" ");
            }
            if raw.trim().is_empty() {
                raw = s(&m["attachments"][0]["fallback"]);
            }
            Hit {
                chan: s(&c["id"]),
                chan_name: s(&c["name"]),
                kind,
                user: m["user"].as_str().filter(|u| !u.is_empty()).map(String::from),
                username: s(&m["username"]),
                thread: thread_of(m["permalink"].as_str().unwrap_or("")).filter(|t| *t != ts),
                ts,
                raw,
            }
        })
        .collect();
    let paging = &m["paging"];
    let n = |v: &Value| v.as_u64().unwrap_or(0) as usize;
    Ok(Page { hits, pages: n(&paging["pages"]).max(1), total: n(&m["total"]).max(n(&paging["total"])) })
}

/// The words of a query its modifiers left out (`in:`, `from:`…): what
/// the found messages mark.
fn bare_words(q: &str) -> String {
    q.split_whitespace().filter(|w| !w.contains(':')).collect::<Vec<_>>().join(" ")
}

/// A ts Slack sent as a string, or now and then as a number.
fn ts_of(v: &Value) -> Option<String> {
    v.as_str().map(String::from).or_else(|| v.as_number().map(|n| n.to_string())).filter(|t| !t.is_empty())
}

/// A message as a found one: its text, else its files' names.
fn hit_of(m: &Message, chan: &str, chan_name: &str, kind: Kind, thread: Option<String>) -> Hit {
    let mut raw = m.text.clone();
    if raw.trim().is_empty() {
        raw = m.files.iter().map(|f| format!("📎 {}", f.name)).collect::<Vec<_>>().join(" ");
    }
    Hit {
        chan: chan.to_string(),
        chan_name: chan_name.to_string(),
        kind,
        user: m.user.clone(),
        username: m.bot_name.clone().unwrap_or_default(),
        ts: m.ts.clone(),
        raw,
        thread,
    }
}

/// The conversation's recent messages that match, newest first, as the
/// web client's "Recent messages with …" (`search.inline`). Slack answers
/// with their ts only: the messages come from those `loaded`, else one
/// history over the span of the rest, else one by one.
fn fetch_recent(s: &Slack, chan: &str, words: &str, chan_name: &str, kind: Kind, loaded: Vec<Message>) -> Res<Vec<Hit>> {
    let v = s.call("search.inline", &[("query", words), ("channel", chan), ("count", PAGE), ("page", "1")])?;
    let refs: Vec<(String, Option<String>)> = v["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|it| {
            let ts = ts_of(&it["ts"])?;
            let thread = thread_of(it["permalink"].as_str().unwrap_or("")).filter(|t| *t != ts);
            Some((ts, thread))
        })
        .collect();
    let mut got: HashMap<String, Message> = HashMap::new();
    for m in loaded {
        if refs.iter().any(|(ts, _)| *ts == m.ts) {
            got.insert(m.ts.clone(), m);
        }
    }
    let mut top: Vec<&str> = refs.iter().filter(|(ts, t)| t.is_none() && !got.contains_key(ts)).map(|(ts, _)| ts.as_str()).collect();
    top.sort();
    if let [first, .., last] = top[..] {
        if let Ok(msgs) = history(s, chan, &[("oldest", first), ("latest", last), ("inclusive", "true"), ("limit", "200")]) {
            for m in msgs.into_iter().filter(|m| top.contains(&m.ts.as_str())) {
                got.insert(m.ts.clone(), m);
            }
        }
    }
    for (ts, thread) in &refs {
        if got.contains_key(ts) {
            continue;
        }
        let one = match thread {
            Some(parent) => s
                .call("conversations.replies", &[("channel", chan), ("ts", parent), ("oldest", ts), ("latest", ts), ("inclusive", "true"), ("limit", "1")])
                .map(|v| v["messages"].as_array().into_iter().flatten().map(crate::slack::parse_message).collect::<Vec<_>>()),
            None => history(s, chan, &[("latest", ts), ("inclusive", "true"), ("limit", "1")]),
        };
        if let Some(m) = one.ok().and_then(|msgs| msgs.into_iter().find(|m| m.ts == *ts)) {
            got.insert(ts.clone(), m);
        }
    }
    Ok(refs.iter().filter_map(|(ts, thread)| got.get(ts).map(|m| hit_of(m, chan, chan_name, kind, thread.clone()))).collect())
}

/// History in chronological order: up to `limit` before `latest`, it
/// included, or the closest after `oldest`.
fn history(s: &Slack, chan: &str, params: &[(&str, &str)]) -> Res<Vec<Message>> {
    let mut p = vec![("channel", chan)];
    p.extend_from_slice(params);
    let v = s.call("conversations.history", &p)?;
    let mut msgs: Vec<Message> = v["messages"].as_array().into_iter().flatten().map(crate::slack::parse_message).collect();
    msgs.sort_by(|a, b| a.ts.cmp(&b.ts));
    Ok(msgs)
}

/// The messages around a found one: its neighbours in the channel, or in
/// its thread the parent and the replies near it.
fn fetch_around(s: &Slack, chan: &str, ts: &str, thread: Option<&str>) -> Preview {
    let around = AROUND.to_string();
    let res = match thread {
        Some(parent) => s.call("conversations.replies", &[("channel", chan), ("ts", parent), ("limit", "200")]).map(|v| {
            let all: Vec<Message> = v["messages"].as_array().into_iter().flatten().map(crate::slack::parse_message).collect();
            let at = all.iter().position(|m| m.ts == ts).unwrap_or(1).max(1);
            let from = at.saturating_sub(AROUND).max(1);
            let to = (at + AROUND + 1).min(all.len());
            let mut msgs: Vec<Message> = all.first().cloned().into_iter().collect();
            msgs.extend(all.get(from..to).unwrap_or_default().iter().cloned());
            (msgs, from > 1)
        }),
        None => {
            let before = (AROUND + 1).to_string();
            history(s, chan, &[("latest", ts), ("inclusive", "true"), ("limit", &before)]).and_then(|mut msgs| {
                // With only `oldest`, Slack gives the messages closest to it.
                let after = history(s, chan, &[("oldest", ts), ("limit", &around)])?;
                msgs.extend(after.into_iter().filter(|m| m.ts.as_str() > ts).take(AROUND));
                Ok((msgs, false))
            })
        }
    };
    match res {
        Ok((msgs, gap)) => Preview { msgs, gap, ..Default::default() },
        Err(e) => Preview { error: Some(e), ..Default::default() },
    }
}

/// A conversation's topic, purpose and members (channels only) and, when
/// none are loaded, its latest messages.
fn fetch_conv(s: &Slack, id: &str, channel: bool, history_too: bool) -> Preview {
    let mut p = Preview::default();
    if channel {
        match s.call("conversations.info", &[("channel", id), ("include_num_members", "true")]) {
            Ok(v) => {
                let c = &v["channel"];
                p.topic = c["topic"]["value"].as_str().unwrap_or("").to_string();
                p.purpose = c["purpose"]["value"].as_str().unwrap_or("").to_string();
                p.members = c["num_members"].as_u64().map(|n| n as u32);
            }
            Err(e) => p.error = Some(e),
        }
    }
    if history_too {
        let limit = LATEST.to_string();
        match history(s, id, &[("limit", &limit)]) {
            Ok(m) => p.msgs = m,
            Err(e) => p.error = Some(e),
        }
    }
    p
}

impl Search {
    /// The window with the messages' order kept from the last run.
    pub fn with_sort(score: bool) -> Self {
        Search { sort: if score { Sort::Score } else { Sort::Date }, ..Default::default() }
    }

    /// Whether the messages go by relevance, for the cache to keep.
    pub fn by_score(&self) -> bool {
        self.sort == Sort::Score
    }

    fn query(&self) -> String {
        self.input.text.trim().to_string()
    }

    fn found_now(&self) -> Option<&Found> {
        self.found.get(&(self.query(), self.sort))
    }

    fn recent_now(&self, scope: Option<&Scope>) -> Option<&Recent> {
        let sc = scope.filter(|_| worth(&self.query()))?;
        self.recent.get(&(sc.conv.clone(), sc.words.clone()))
    }

    /// A new query, or tab, or order: back to the first result.
    fn reset(&mut self) {
        (self.sel, self.scroll, self.wheel) = (0, 0, false);
    }

    /// Kept for `PREVIEWS` previews, the least recently seen going first.
    fn keep_preview(&mut self, key: Key, p: Preview) {
        self.lru.retain(|k| k != &key);
        self.lru.push_back(key.clone());
        self.previews.insert(key, p);
        while self.lru.len() > PREVIEWS {
            if let Some(old) = self.lru.pop_front() {
                self.previews.remove(&old);
            }
        }
    }
}

impl App {
    /// Ctrl+K, Ctrl+T, or a click on the sidebar's box: a fresh search.
    pub(crate) fn open_search(&mut self) {
        (self.menu, self.account.menu) = (None, None);
        let s = &mut self.search;
        s.open = true;
        s.input = Editor::default();
        s.tab = Tab::All;
        s.want = None;
        // The quick switcher's lists change between openings.
        *s.local.borrow_mut() = None;
        s.reset();
        self.dirty = true;
    }

    /// Ctrl+F: the search on the open conversation (`in:#eng `, `in:@ana `),
    /// its messages only, as Slack does.
    pub(crate) fn search_here(&mut self) {
        let Some(id) = self.current.clone() else { return };
        let conv = self.convs.iter().find(|c| c.id == id).cloned().or_else(|| self.directory.find(&id).map(|j| self.directory.conv(j)));
        let Some(c) = conv else { return };
        let scope = match (c.kind, c.user.as_deref()) {
            (Kind::Direct, Some(uid)) => match self.users.handles.iter().find(|(_, v)| v.as_str() == uid) {
                Some((h, _)) => format!("@{h}"),
                None => format!("<@{uid}>"),
            },
            _ => format!("#{}", c.name),
        };
        self.open_search();
        self.search.input.insert(&format!("in:{scope} "));
        self.search.tab = Tab::Msgs;
    }

    /// The conversation a query with one `in:` and only plain words
    /// searches in (`in:#eng deplo`, `in:@ana`, `in:<#C1>`), for its recent
    /// messages; none with other modifiers.
    fn search_scope(&self) -> Option<Scope> {
        let q = self.search.query();
        let (ins, rest): (Vec<&str>, Vec<&str>) = q.split_whitespace().partition(|w| w.len() >= 3 && w.is_char_boundary(3) && w[..3].eq_ignore_ascii_case("in:"));
        if ins.len() != 1 || rest.iter().any(|w| w.contains(':') || w.starts_with('-')) {
            return None;
        }
        let words = rest.join(" ").replace(['*', '"'], " ").split_whitespace().collect::<Vec<_>>().join(" ");
        if !words.chars().any(char::is_alphanumeric) {
            return None;
        }
        let v = &ins[0][3..];
        let inner = |p: &str| v.strip_prefix(p).map(|r| r.trim_end_matches('>').split('|').next().unwrap_or("").to_string());
        let dm_with = |uid: &str| self.convs.iter().find(|c| c.kind == Kind::Direct && c.user.as_deref() == Some(uid)).map(|c| c.id.clone());
        let id = if let Some(id) = inner("<#") {
            Some(id)
        } else if let Some(uid) = inner("<@") {
            dm_with(&uid)
        } else if let Some(h) = v.strip_prefix('@') {
            self.users.handles.get(h).and_then(|uid| dm_with(uid))
        } else {
            let name = v.trim_start_matches('#');
            let ours = self.convs.iter().find(|c| c.kind != Kind::Direct && c.name.eq_ignore_ascii_case(name)).map(|c| c.id.clone());
            ours.or_else(|| self.directory.chans.iter().find(|d| d.name.eq_ignore_ascii_case(name)).map(|d| d.id.clone()))
        }?;
        let conv = self.convs.iter().find(|c| c.id == id).cloned().or_else(|| self.directory.find(&id).map(|j| self.directory.conv(j)))?;
        let label = self.label(&conv);
        let title = match conv.kind {
            Kind::Direct | Kind::Group => trf!("Recent messages with {label}", "Mensagens recentes com {label}"),
            _ => trf!("Recent messages in #{label}", "Mensagens recentes em #{label}"),
        };
        Some(Scope { conv: id, words, title })
    }

    pub(crate) fn close_search(&mut self) {
        self.search.open = false;
        self.search.want = None;
        self.dirty = true;
    }

    /// What the query finds here (`Matches`), once per query and state of the lists.
    fn search_local(&self) -> Rc<Matches> {
        let q = self.search.query();
        let state = (self.dir_gen, self.convs.len(), self.users.names.len());
        if let Some((mq, ms, m)) = self.search.local.borrow().as_ref() {
            if *mq == q && *ms == state {
                return m.clone();
            }
        }
        let m = Rc::new(if q.is_empty() { self.quick_switch() } else { self.local_matches(&q) });
        *self.search.local.borrow_mut() = Some((q, state, m.clone()));
        m
    }

    /// With nothing typed: where you've been, then what's unread, then what
    /// moved last; for People, the DMs and group DMs that moved last.
    fn quick_switch(&self) -> Matches {
        let here = self.current.as_deref();
        let ok = |c: &Conversation| !c.guest && here != Some(c.id.as_str()) && !(c.kind == Kind::Direct && c.user.is_none());
        let mut seen = HashSet::new();
        let mut recent: Vec<usize> = Vec::new();
        for id in &self.recents {
            if let Some(i) = self.convs.iter().position(|c| &c.id == id && ok(c)) {
                if seen.insert(i) {
                    recent.push(i);
                }
            }
        }
        let mut rest: Vec<(bool, std::cmp::Reverse<i64>, usize)> = self
            .convs
            .iter()
            .enumerate()
            .filter(|(i, c)| ok(c) && !c.archived && !seen.contains(i))
            .map(|(i, c)| {
                let (unread, mentions) = self.unread(&c.id);
                (!(mentions > 0 || (unread && !self.muted(c))), std::cmp::Reverse(self.last_message(c)), i)
            })
            .collect();
        rest.sort();
        for (read, _, i) in rest {
            if read && recent.len() >= RECENT_FILL {
                break;
            }
            recent.push(i);
        }
        recent.truncate(RECENT_MAX);
        let recent: Vec<Conv> = recent.into_iter().map(Conv::Ours).collect();
        let chans = recent.iter().copied().filter(|c| matches!(c, Conv::Ours(i) if matches!(self.convs[*i].kind, Kind::Channel | Kind::Private))).collect();
        let users = &self.users;
        let mut dms: Vec<(std::cmp::Reverse<i64>, usize)> = self
            .convs
            .iter()
            .enumerate()
            .filter(|(_, c)| match (c.kind, c.user.as_deref()) {
                (Kind::Group, _) => !c.archived,
                (Kind::Direct, Some(u)) => !users.bots.contains(u) && !users.deleted.contains(u),
                _ => false,
            })
            .map(|(i, c)| (std::cmp::Reverse(self.last_message(c)), i))
            .collect();
        dms.sort();
        let people = dms
            .into_iter()
            .take(RECENT_MAX)
            .map(|(_, i)| match (self.convs[i].kind, &self.convs[i].user) {
                (Kind::Direct, Some(u)) => Who::Person(u.clone()),
                _ => Who::Conv(Conv::Ours(i)),
            })
            .collect();
        Matches { recent, chans, people, people_first: true }
    }

    /// The channels the query matches (ours and the workspace's), and the
    /// people: anyone by any of their names (display, full, handle), a DM
    /// or not, with group DMs and apps' DMs among them. The better match
    /// leads, then the last message; you go behind the others.
    fn local_matches(&self, query: &str) -> Matches {
        use crate::text::{place, rank, Folded, Query};
        let chans: Vec<Conv> = self
            .conv_matches(query)
            .into_iter()
            .filter_map(|r| match r {
                Row::Conv(i) if matches!(self.convs[i].kind, Kind::Channel | Kind::Private) => Some(Conv::Ours(i)),
                Row::Other(j) => Some(Conv::Other(j)),
                _ => None,
            })
            .collect();
        let q = Query::new(query);
        let me = self.me.as_ref().map(|m| m.user_id.as_str());
        let users = &self.users;
        let handle_of: HashMap<&str, &str> = users.handles.iter().map(|(h, u)| (u.as_str(), h.as_str())).collect();
        let dm_of: HashMap<&str, usize> =
            self.convs.iter().enumerate().filter(|(_, c)| c.kind == Kind::Direct).filter_map(|(i, c)| Some((c.user.as_deref()?, i))).collect();
        let mut ranges = Vec::new();
        // (archived, inexact, you, place): fields compare in order.
        let mut hits: Vec<((bool, bool, bool, crate::text::Place), Who)> = Vec::new();
        let mut covered: HashSet<&str> = HashSet::new();
        for (uid, name) in &users.names {
            if users.deleted.contains(uid) || users.bots.contains(uid) {
                continue;
            }
            let names = [Some(name.as_str()), users.real.get(uid).map(String::as_str), handle_of.get(uid.as_str()).copied()];
            let Some(tier) = names.into_iter().flatten().filter(|n| !n.is_empty()).filter_map(|n| rank(&q, &Folded::new(n), &mut ranges)).min() else {
                continue;
            };
            covered.insert(uid);
            let you = me == Some(uid.as_str());
            let latest = if you { 0 } else { dm_of.get(uid.as_str()).map_or(0, |&i| self.last_message(&self.convs[i])) };
            hits.push(((false, tier > 0, you, place(tier, false, true, latest, 0, name.chars().count())), Who::Person(uid.clone())));
        }
        for (i, c) in self.convs.iter().enumerate() {
            let group = c.kind == Kind::Group;
            if !group && !(c.kind == Kind::Direct && c.user.as_deref().is_none_or(|u| !covered.contains(u))) {
                continue;
            }
            let name = Folded::new(&self.label(c));
            let Some(tier) = rank(&q, &name, &mut ranges) else { continue };
            let at = place(tier, c.archived, true, self.last_message(c), 0, name.len()).group(group);
            hits.push(((c.archived, tier > 0, false, at), Who::Conv(Conv::Ours(i))));
        }
        hits.sort_by(|a, b| a.0.cmp(&b.0));
        // All: the part whose first answer is the better match goes first;
        // People on a tie, unless what leads it is you.
        let lead_c = chans.first().and_then(|c| self.conv_of(*c)).map_or((true, true), |c| {
            let exact = crate::text::find(&self.label(&c), query).is_some_and(|(t, _)| t == 0);
            (c.archived, !exact)
        });
        let lead_p = hits.first().map_or((true, true), |h| (h.0 .0, h.0 .1));
        let you_lead = hits.first().is_some_and(|h| h.0 .2);
        let people_first = lead_p < lead_c || (lead_p == lead_c && !you_lead);
        Matches { recent: Vec::new(), chans, people: hits.into_iter().map(|(_, w)| w).collect(), people_first }
    }

    /// Someone in the People list, by their place in it.
    fn who(&self, k: usize) -> Option<Who> {
        self.search_local().people.get(k).cloned()
    }

    /// The tab's local parts, in order: each heading's results and what
    /// it leaves to its own tab.
    fn search_parts(&self) -> Vec<Part> {
        let tab = self.search.tab;
        if tab == Tab::Msgs {
            return Vec::new();
        }
        let m = self.search_local();
        let recent = |items: Vec<Item>, empty| Part { title: tr("Recent", "Recentes").into(), count: None, items, more: None, empty: Some(empty), narrow: false };
        let part = |title: &str, total: usize, items: Vec<Item>, to: Tab, empty| {
            let left = total - items.len();
            let more = (tab == Tab::All && left > 0).then_some((left, to));
            Part { title: title.into(), count: Some(total), items, more, empty: Some(empty), narrow: tab != Tab::All && left > 0 }
        };
        let room = |n: usize| if tab == Tab::All { n } else { TAB_MAX };
        let chans = || {
            let items = m.chans.iter().take(room(ALL_CHANS)).copied().map(Item::Conv).collect();
            part(tr("Channels", "Canais"), m.chans.len(), items, Tab::Chans, tr("No channels match", "Nenhum canal encontrado"))
        };
        let people = || {
            let items = (0..m.people.len().min(room(ALL_PEOPLE))).map(Item::Who).collect();
            part(tr("People", "Pessoas"), m.people.len(), items, Tab::People, tr("No one matches", "Ninguém encontrado"))
        };
        let typed = !self.search.query().is_empty();
        match tab {
            Tab::All if !typed => vec![recent(m.recent.iter().copied().map(Item::Conv).collect(), tr("Type to find a conversation", "Digite para achar uma conversa"))],
            Tab::Chans if !typed => vec![recent(m.chans.iter().copied().map(Item::Conv).collect(), tr("Type to find a channel", "Digite para achar um canal"))],
            Tab::People if !typed => vec![recent((0..m.people.len()).map(Item::Who).collect(), tr("Type to find someone", "Digite para achar alguém"))],
            Tab::Chans => vec![chans()],
            Tab::People => vec![people()],
            _ => {
                let (c, p) = (chans(), people());
                let both = if m.people_first { [p, c] } else { [c, p] };
                both.into_iter().filter(|p| !p.items.is_empty()).collect()
            }
        }
    }

    /// What the tab lists, in order: the results the selection stops on.
    fn search_items(&self) -> Vec<Item> {
        let mut out: Vec<Item> = Vec::new();
        for p in self.search_parts() {
            out.extend(p.items);
            out.extend(p.more.map(|(_, tab)| Item::More(tab)));
        }
        if matches!(self.search.tab, Tab::All | Tab::Msgs) {
            out.extend((0..self.search_msgs().len()).map(Item::Msg));
        }
        out
    }

    /// The messages listed, in order: the conversation's recent ones that
    /// match (with `in:`), then Slack's search without those.
    fn search_msgs(&self) -> Vec<&Hit> {
        let scope = self.search_scope();
        let mut out: Vec<&Hit> = self.search.recent_now(scope.as_ref()).map(|r| r.hits.iter().collect()).unwrap_or_default();
        let seen: HashSet<(&str, &str)> = out.iter().map(|h| (h.chan.as_str(), h.ts.as_str())).collect();
        if let Some(f) = self.search.found_now() {
            out.extend(f.hits.iter().filter(|h| !seen.contains(&(h.chan.as_str(), h.ts.as_str()))));
        }
        out
    }

    fn search_hit(&self, k: usize) -> Option<&Hit> {
        self.search_msgs().get(k).copied()
    }

    /// The conversation a result is, as it would open.
    fn conv_of(&self, c: Conv) -> Option<Conversation> {
        match c {
            Conv::Ours(i) => self.convs.get(i).cloned(),
            Conv::Other(j) => (j < self.directory.chans.len()).then(|| self.directory.conv(j)),
        }
    }

    /// The preview a result wants.
    fn preview_key(&self, item: Item) -> Option<Key> {
        match item {
            Item::Conv(c) => self.conv_of(c).map(|c| Key::Conv(c.id)),
            Item::Who(k) => match self.who(k)? {
                Who::Person(uid) => Some(Key::Person(uid)),
                Who::Conv(c) => self.conv_of(c).map(|c| Key::Conv(c.id)),
            },
            Item::Msg(k) => self.search_hit(k).map(|h| Key::Msg { chan: h.chan.clone(), ts: h.ts.clone(), thread: h.thread.clone() }),
            Item::More(_) => None,
        }
    }

    /// From `tick`, while the window is open: Slack is asked for the
    /// messages once the typing stopped for `PAUSE`, for the next page as
    /// the selection nears the end, and for the preview once it rested.
    pub(crate) fn tick_search(&mut self) {
        if !self.search.open {
            return;
        }
        let q = self.search.query();
        if q != self.search.seen.0 {
            self.search.seen = (q, Some(Instant::now()));
        } else if self.search.seen.1.is_some_and(|t| t.elapsed() >= PAUSE) && worth(&q) {
            let key = (q.clone(), self.search.sort);
            if !self.search.found.contains_key(&key) {
                self.search.order.push_back(key.clone());
                while self.search.order.len() > KEEP {
                    if let Some(old) = self.search.order.pop_front() {
                        self.search.found.remove(&old);
                    }
                }
                self.search.found.insert(key, Found { asked: q.clone(), loading: true, ..Default::default() });
                self.ask_found(q.clone(), self.search.sort, q, 1);
            }
            self.ask_recent();
        }
        let items = self.search_items();
        let sel = items.get(self.search.sel).copied();
        // The next page, as the selection nears the end of those loaded, or
        // when all those loaded were among the recent ones.
        let shown = self.search_msgs().len();
        let recent = self.search.recent_now(self.search_scope().as_ref()).map_or(0, |r| r.hits.len());
        let near = matches!(sel, Some(Item::Msg(k)) if k + NEAR >= shown);
        if near || (recent > 0 && shown == recent) {
            let key = (self.search.query(), self.search.sort);
            if let Some(f) = self.search.found.get_mut(&key) {
                if f.page > 0 && f.page < f.pages && !f.loading && f.error.is_none() {
                    f.loading = true;
                    let (asked, page) = (f.asked.clone(), f.page + 1);
                    self.ask_found(key.0, key.1, asked, page);
                }
            }
        }
        // The preview of the result picked, once the selection rests.
        let Some(key) = sel.and_then(|i| self.preview_key(i)) else { return };
        match &self.search.want {
            Some((k, _)) if *k == key => {}
            _ => self.search.want = Some((key.clone(), Instant::now())),
        }
        let rested = self.search.want.as_ref().is_some_and(|(_, t)| t.elapsed() >= PREVIEW_PAUSE);
        if !rested || self.search.previews.contains_key(&key) || !self.search.asking.insert(key.clone()) {
            return;
        }
        match key.clone() {
            Key::Msg { chan, ts, thread } => self.spawn(move |s| {
                let preview = fetch_around(s, &chan, &ts, thread.as_deref());
                Event::Search(Ev::Preview { key, preview })
            }),
            Key::Conv(id) => {
                let conv = self.convs.iter().find(|c| c.id == id).cloned().or_else(|| self.directory.find(&id).map(|j| self.directory.conv(j)));
                let channel = conv.is_some_and(|c| matches!(c.kind, Kind::Channel | Kind::Private));
                let history_too = self.messages.get(&id).map_or(true, |m| m.is_empty());
                self.spawn(move |s| {
                    let preview = fetch_conv(s, &id, channel, history_too);
                    Event::Search(Ev::Preview { key, preview })
                });
            }
            Key::Person(uid) => {
                // Their DM's latest too, when it has none loaded.
                let dm = self.dm_with(&uid).filter(|id| self.messages.get(id).is_none_or(|m| m.is_empty()));
                self.spawn(move |s| {
                    let mut preview = dm.map(|id| fetch_conv(s, &id, false, true)).unwrap_or_default();
                    match s.profile(&uid) {
                        Ok(p) => preview.profile = Some(p),
                        Err(e) => preview.error = Some(e),
                    }
                    Event::Search(Ev::Preview { key, preview })
                });
            }
        }
    }

    /// With `in:`, the conversation's recent messages, once per conversation and words.
    fn ask_recent(&mut self) {
        let Some(sc) = self.search_scope() else { return };
        let key = (sc.conv.clone(), sc.words.clone());
        if self.search.recent.get(&key).is_some_and(|r| r.error.is_none()) {
            return;
        }
        self.search.recent_order.retain(|k| k != &key);
        self.search.recent_order.push_back(key.clone());
        while self.search.recent_order.len() > KEEP {
            if let Some(old) = self.search.recent_order.pop_front() {
                self.search.recent.remove(&old);
            }
        }
        self.search.recent.insert(key.clone(), Recent { loading: true, ..Default::default() });
        let conv = self.convs.iter().find(|c| c.id == sc.conv).cloned().or_else(|| self.directory.find(&sc.conv).map(|j| self.directory.conv(j)));
        let (name, kind) = conv.map_or((String::new(), Kind::Channel), |c| (c.name, c.kind));
        let loaded = self.messages.get(&sc.conv).cloned().unwrap_or_default();
        self.dirty = true;
        self.spawn(move |s| {
            let res = fetch_recent(s, &key.0, &key.1, &name, kind, loaded);
            Event::Search(Ev::Recent { key, res })
        });
    }

    fn ask_found(&mut self, query: String, sort: Sort, asked: String, page: usize) {
        self.dirty = true;
        self.spawn(move |s| {
            let res = fetch(s, &asked, sort, page);
            Event::Search(Ev::Found { query, sort, page, res })
        });
    }

    pub(crate) fn on_search(&mut self, e: Ev) {
        match e {
            Ev::Found { query, sort, page, res } => {
                let key = (query, sort);
                // Gone from the cache since: nothing to fill.
                let Some(f) = self.search.found.get_mut(&key) else { return };
                f.loading = false;
                let p = match res {
                    Ok(p) => p,
                    Err(e) => return f.error = Some(e),
                };
                // Nothing as typed: once more with a `*` on the last word.
                if page == 1 && p.hits.is_empty() && !f.retried {
                    if let Some(again) = starred(&f.asked) {
                        (f.retried, f.loading, f.asked) = (true, true, again.clone());
                        return self.ask_found(key.0, key.1, again, 1);
                    }
                }
                // A page already there, or out of turn.
                if page != f.page + 1 {
                    return;
                }
                let texts: Vec<String> = p.hits.iter().map(|h| h.raw.clone()).collect();
                f.hits.extend(p.hits);
                (f.page, f.pages, f.total, f.error) = (page, p.pages, p.total, None);
                for t in texts {
                    self.name_mentions(&t);
                }
            }
            Ev::Recent { key, res } => {
                let Some(r) = self.search.recent.get_mut(&key) else { return };
                r.loading = false;
                match res {
                    Ok(hits) => {
                        let texts: Vec<String> = hits.iter().map(|h| h.raw.clone()).collect();
                        (r.hits, r.error) = (hits, None);
                        for t in texts {
                            self.name_mentions(&t);
                        }
                    }
                    Err(e) => r.error = Some(e),
                }
            }
            Ev::Preview { key, preview } => {
                self.search.asking.remove(&key);
                let texts: Vec<String> = preview.msgs.iter().map(|m| m.text.clone()).collect();
                self.search.keep_preview(key, preview);
                for t in texts {
                    self.name_mentions(&t);
                }
            }
        }
    }

    fn search_move(&mut self, delta: isize) {
        let n = self.search_items().len() as isize;
        let s = &mut self.search;
        s.sel = (s.sel as isize + delta).clamp(0, (n - 1).max(0)) as usize;
        s.wheel = false;
    }

    fn search_tab(&mut self, tab: Tab) {
        if self.search.tab != tab {
            self.search.tab = tab;
            self.search.reset();
        }
    }

    /// Typing goes to the field; the arrows and the page keys move the
    /// selection, Tab changes the tab, Enter opens.
    pub(crate) fn search_key(&mut self, k: KeyEvent) {
        self.dirty = true;
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        let page = self.search.height.max(4) as isize / 3;
        let at = TABS.iter().position(|t| *t == self.search.tab).unwrap_or(0);
        let (next, prev) = (TABS[(at + 1) % TABS.len()], TABS[(at + TABS.len() - 1) % TABS.len()]);
        match k.code {
            KeyCode::Char('k' | 't' | 'g') if ctrl && !shift => return self.close_search(),
            KeyCode::Esc if !self.search.input.text.is_empty() => {
                self.search.input = Editor::default();
                return self.search.reset();
            }
            KeyCode::Esc => return self.close_search(),
            KeyCode::Enter => return self.open_selected(),
            KeyCode::Up => return self.search_move(-1),
            KeyCode::Down => return self.search_move(1),
            KeyCode::PageUp => return self.search_move(-page),
            KeyCode::PageDown => return self.search_move(page),
            KeyCode::Home if !ctrl => return self.search_move(isize::MIN / 2),
            KeyCode::End if !ctrl => return self.search_move(isize::MAX / 2),
            KeyCode::Tab if shift => return self.search_tab(prev),
            KeyCode::BackTab => return self.search_tab(prev),
            KeyCode::Tab => return self.search_tab(next),
            KeyCode::Char(c @ '1'..='4') if alt => return self.search_tab(TABS[c as usize - '1' as usize]),
            KeyCode::Char('s') if ctrl => return self.search_sort(),
            _ => {}
        }
        let e = &mut self.search.input;
        let before = e.text.clone();
        match k.code {
            KeyCode::Left if ctrl => e.word_left(),
            KeyCode::Right if ctrl => e.word_right(),
            KeyCode::Left => e.left(),
            KeyCode::Right => e.right(),
            KeyCode::Char('a') if ctrl => e.home(),
            KeyCode::Char('e') if ctrl => e.end(),
            KeyCode::Backspace if ctrl || alt => e.delete_word(),
            KeyCode::Char('w') if ctrl => e.delete_word(),
            KeyCode::Backspace => e.backspace(),
            KeyCode::Delete if ctrl => e.delete_word_forward(),
            KeyCode::Delete => e.delete(),
            KeyCode::Char('u') if ctrl => *e = Editor::default(),
            KeyCode::Char(c) if !ctrl && !alt => e.insert(c.encode_utf8(&mut [0; 4])),
            _ => {}
        }
        if self.search.input.text != before {
            self.search.reset();
        }
    }

    /// Ctrl+S, or a click on the order: newest first or most relevant,
    /// kept for the next run.
    fn search_sort(&mut self) {
        let s = &mut self.search;
        s.sort = if s.sort == Sort::Date { Sort::Score } else { Sort::Date };
        s.reset();
    }

    pub(crate) fn search_paste(&mut self, s: &str) {
        let line = s.lines().next().unwrap_or("").to_string();
        self.search.input.insert(&line);
        self.search.reset();
    }

    /// The wheel's move on the list, `delta` lines down.
    pub(crate) fn scroll_search(&mut self, delta: isize) {
        let s = &mut self.search;
        (s.scroll, s.wheel) = (s.scroll.saturating_add_signed(delta).min(s.max_scroll), true);
    }

    /// The wheel scrolls the list; a click picks a result, a second one
    /// opens it; one on a tab shows it; one outside closes.
    pub(crate) fn search_mouse(&mut self, m: MouseEvent) {
        let at = Position::new(m.column, m.row);
        let s = &mut self.search;
        match m.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => self.wheel(&m, crate::wheel::Pane::Search),
            MouseEventKind::Down(_) if !s.rect.contains(at) => return self.close_search(),
            MouseEventKind::Down(MouseButton::Left) => {
                let spot = s.spots.iter().find(|(r, _)| r.contains(at)).map(|(_, i)| *i);
                if let Some(tab) = s.tabs.iter().find(|(r, _)| r.contains(at)).map(|(_, t)| *t) {
                    self.search_tab(tab);
                } else if s.sort_rect.contains(at) {
                    self.search_sort();
                } else if let Some(Item::More(tab)) = spot.and_then(|i| self.search_items().get(i).copied()) {
                    self.search_tab(tab);
                } else if let Some(i) = spot {
                    let s = &mut self.search;
                    if s.sel == i {
                        return self.open_selected();
                    }
                    (s.sel, s.wheel) = (i, true);
                }
            }
            _ => return,
        }
        self.dirty = true;
    }

    /// Enter: a conversation opens (a channel you are not in as a
    /// preview to join), someone's DM (made if need be), a message at its
    /// place, a reply in its thread; "+N more" shows its tab. With nothing
    /// to open, Slack is asked at once rather than after the pause.
    fn open_selected(&mut self) {
        let Some(item) = self.search_items().get(self.search.sel).copied() else {
            self.search.seen.1 = Instant::now().checked_sub(PAUSE);
            return;
        };
        match item {
            Item::Conv(c) => {
                let Some(conv) = self.conv_of(c) else { return };
                self.close_search();
                self.open(conv.id);
            }
            Item::Who(k) => {
                let Some(who) = self.who(k) else { return };
                self.close_search();
                match who {
                    Who::Person(uid) => self.open_dm(uid),
                    Who::Conv(c) => {
                        if let Some(conv) = self.conv_of(c) {
                            self.open(conv.id);
                        }
                    }
                }
            }
            Item::More(tab) => self.search_tab(tab),
            Item::Msg(k) => {
                let Some(h) = self.search_hit(k).cloned() else { return };
                self.close_search();
                self.open_hit(h);
            }
        }
    }

    /// A found message's conversation, opened at it, as a permalink would.
    fn open_hit(&mut self, h: Hit) {
        // A public channel neither ours nor listed: opened as one you are not in.
        if h.kind == Kind::Channel && !self.convs.iter().any(|c| c.id == h.chan) && self.directory.find(&h.chan).is_none() {
            self.convs.push(Conversation {
                id: h.chan.clone(),
                kind: Kind::Channel,
                name: h.chan_name.clone(),
                user: None,
                ext_shared: false,
                archived: false,
                general: false,
                guest: true,
                updated: 0,
                dormant: false,
            });
            self.resort();
        }
        self.open(h.chan.clone());
        self.flash = Some((h.ts.clone(), Instant::now()));
        match h.thread {
            Some(parent) => {
                self.open_thread(parent.clone());
                if let Some(t) = self.thread.as_mut() {
                    t.jump = Some(h.ts);
                }
                self.show_message(h.chan, parent);
            }
            None => self.show_message(h.chan, h.ts),
        }
    }
}

// ---- text ------------------------------------------------------------------

/// "Today", "Yesterday", the weekday within a week, else the date.
fn day(ts: &str) -> String {
    let Some(t) = ts.split('.').next().and_then(|s| s.parse().ok()).and_then(|s| Local.timestamp_opt(s, 0).single()) else {
        return String::new();
    };
    let today = Local::now().date_naive();
    let d = t.date_naive();
    let days = (today - d).num_days();
    let month: String = i18n::month(&t).chars().take(3).collect();
    match days {
        0 => tr("Today", "Hoje").into(),
        1 => tr("Yesterday", "Ontem").into(),
        2..=6 => {
            let w = i18n::weekday(&t);
            let mut c = w.chars();
            c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
        }
        _ if d.year() == today.year() => {
            if i18n::is_pt() { format!("{} de {month}", t.day()) } else { format!("{month} {}", t.day()) }
        }
        _ => {
            if i18n::is_pt() { format!("{} de {month} de {}", t.day(), t.year()) } else { format!("{month} {}, {}", t.day(), t.year()) }
        }
    }
}

/// Where a message is: "Direct message with Ana", "#geral", the lock and a
/// private channel's name, or a group's people.
fn context(app: &App, h: &Hit) -> String {
    let conv = app.convs.iter().find(|c| c.id == h.chan);
    match (h.kind, conv) {
        (Kind::Direct, Some(c)) => {
            let who = app.label(c);
            trf!("Direct message with {who}", "Mensagem direta com {who}")
        }
        (Kind::Direct, None) => tr("Direct message", "Mensagem direta").into(),
        (Kind::Group, Some(c)) => app.label(c),
        (Kind::Private, _) => format!("{LOCK} {}", conv.map_or(h.chan_name.as_str(), |c| c.name.as_str())),
        (_, _) => format!("#{}", conv.map_or(h.chan_name.as_str(), |c| c.name.as_str())),
    }
}

/// Links with a label show the label alone, as on Slack; the marks are
/// taken out of what isn't text (ids, URLs) so mentions still resolve.
fn plain_links(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(i) = rest.find('<') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let Some(j) = after.find('>') else {
            out.push_str(&rest[i..]);
            return out;
        };
        let inner = &after[..j];
        let (target, label) = inner.split_once('|').unwrap_or((inner, ""));
        let target: String = target.chars().filter(|c| !matches!(*c, MARK_ON | MARK_OFF)).collect();
        let url = target.starts_with("http") || target.starts_with("mailto:");
        match () {
            _ if url && !label.is_empty() => out.push_str(label),
            _ if label.is_empty() => out.push_str(&format!("<{target}>")),
            _ => out.push_str(&format!("<{target}|{label}>")),
        }
        rest = &after[j + 1..];
    }
    out.push_str(rest);
    out
}

/// A found message's text on one line, readable (mentions named, links as
/// their words, emoji drawn), and which chars matched: Slack's marks and
/// the query's words where they start one of the text's.
fn hit_text(app: &App, h: &Hit, query: &str) -> (Vec<char>, Vec<bool>) {
    let names = app.names();
    let s = crate::text::shortcodes(&crate::text::mrkdwn(&plain_links(&h.raw), &names));
    let (mut chars, mut mask) = (Vec::new(), Vec::new());
    let mut on = false;
    for c in s.chars() {
        match c {
            MARK_ON => on = true,
            MARK_OFF => on = false,
            c if c.is_whitespace() => {
                if chars.last().is_some_and(|l| *l != ' ') {
                    chars.push(' ');
                    mask.push(false);
                }
            }
            c => {
                chars.push(c);
                mask.push(on);
            }
        }
    }
    while chars.last() == Some(&' ') {
        chars.pop();
        mask.pop();
    }
    // Slack's marks miss some forms it matched ("cha" found "chá" unmarked):
    // the query's words, accents and case aside, are marked too.
    let text: String = chars.iter().collect();
    for (a, b) in crate::text::marks(&text, &bare_words(query)) {
        mask[a.min(chars.len())..b.min(chars.len())].iter_mut().for_each(|x| *x = true);
    }
    (chars, mask)
}

/// The words that matched in a found message, for its preview to mark.
fn matched_words(chars: &[char], mask: &[bool]) -> String {
    let mut out = String::new();
    for (i, c) in chars.iter().enumerate() {
        if mask[i] {
            out.push(*c);
        } else if i > 0 && mask[i - 1] {
            out.push(' ');
        }
    }
    out
}

/// Lines of at most `width` cells, broken at spaces where it can, as char
/// ranges; newlines break too.
fn wrap_at(chars: &[char], width: usize) -> Vec<(usize, usize)> {
    let width = width.max(1);
    let mut out = Vec::new();
    let (mut start, mut w, mut space, mut i) = (0, 0, None::<usize>, 0);
    while i < chars.len() {
        let c = chars[i];
        if c == '\n' {
            out.push((start, i));
            (start, w, space) = (i + 1, 0, None);
            i += 1;
            continue;
        }
        let cw = c.width().unwrap_or(0);
        if w + cw > width && i > start {
            match space.filter(|&s| s > start) {
                Some(s) => {
                    out.push((start, s));
                    start = s + 1;
                }
                None => {
                    out.push((start, i));
                    start = i;
                }
            }
            w = chars[start..i].iter().map(|c| c.width().unwrap_or(0)).sum();
            space = None;
            continue;
        }
        if c == ' ' {
            space = Some(i);
        }
        w += cw;
        i += 1;
    }
    out.push((start, chars.len()));
    out
}

/// Spans of `chars`, what `mask` marks in the match's style.
fn spans_of(chars: &[char], mask: &[bool], base: Style) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut start = 0;
    for i in 1..=chars.len() {
        if i == chars.len() || mask[i] != mask[start] {
            let st = if mask[start] { base.patch(lit()) } else { base };
            spans.push(Span::styled(chars[start..i].iter().collect::<String>(), st));
            start = i;
        }
    }
    spans
}

/// Up to `n` lines of a found message's text, around the first match:
/// cut from a little before it, with "…", when it would fall past them.
fn window(chars: &[char], mask: &[bool], width: usize, n: usize) -> Vec<(Vec<char>, Vec<bool>)> {
    let mut lines = wrap_at(chars, width);
    let first = mask.iter().position(|m| *m);
    let line_of = |lines: &[(usize, usize)], f: usize| lines.iter().position(|&(a, b)| f >= a && f < b.max(a + 1)).unwrap_or(0);
    let (mut chars, mut mask) = (chars.to_vec(), mask.to_vec());
    if let Some(f) = first.filter(|&f| line_of(&lines, f) >= n) {
        let mut s = f.saturating_sub(width / 3);
        if let Some(p) = chars[s..f].iter().position(|c| *c == ' ') {
            s += p + 1;
        }
        chars = std::iter::once('…').chain(chars[s..].iter().copied()).collect();
        mask = std::iter::once(false).chain(mask[s..].iter().copied()).collect();
        lines = wrap_at(&chars, width);
    }
    let more = lines.len() > n;
    let mut out: Vec<(Vec<char>, Vec<bool>)> = lines.into_iter().take(n).map(|(a, b)| (chars[a..b].to_vec(), mask[a..b].to_vec())).collect();
    if more {
        if let Some((c, m)) = out.last_mut() {
            while c.iter().map(|c| c.width().unwrap_or(0)).sum::<usize>() + 1 > width && !c.is_empty() {
                c.pop();
                m.pop();
            }
            c.push('…');
            m.push(false);
        }
    }
    out
}

/// `text` wrapped to `width`, the matched words marked: for the preview.
fn marked_lines(text: &str, terms: &str, width: usize, base: Style) -> Vec<Vec<Span<'static>>> {
    let chars: Vec<char> = text.chars().collect();
    let mut mask = vec![false; chars.len()];
    if !terms.trim().is_empty() {
        for (a, b) in crate::text::marks(text, terms) {
            mask[a.min(chars.len())..b.min(chars.len())].iter_mut().for_each(|x| *x = true);
        }
    }
    wrap_at(&chars, width).into_iter().map(|(a, b)| spans_of(&chars[a..b], &mask[a..b], base)).collect()
}

// ---- drawing ---------------------------------------------------------------

/// A row of the list: a result's own rows carry its place among the results.
struct Entry {
    item: Option<usize>,
    lines: Vec<Line<'static>>,
}

impl Entry {
    fn plain(line: Line<'static>) -> Self {
        Entry { item: None, lines: vec![line] }
    }
}

fn note(text: &str) -> Entry {
    Entry::plain(Line::from(vec![Span::raw("  "), Span::styled(text.to_string(), dim().italic())]))
}

fn heading(title: &str, count: Option<String>) -> Entry {
    let mut spans = vec![Span::styled(format!(" {title}"), dim().bold())];
    spans.extend(count.map(|c| Span::styled(format!("  {c}"), dim())));
    Entry::plain(Line::from(spans))
}

/// Left and right parts on one line, `width` wide, the left cut to fit.
fn spread(mut left: Vec<Span<'static>>, right: Vec<Span<'static>>, width: usize) -> Line<'static> {
    let rw: usize = right.iter().map(|s| s.width()).sum();
    let room = width.saturating_sub(rw + 1);
    let mut used = 0;
    for s in left.iter_mut() {
        if used >= room {
            s.content = "".into();
            continue;
        }
        if used + s.width() > room {
            s.content = truncate(&s.content, room - used).into();
        }
        used += s.width();
    }
    left.push(Span::raw(" ".repeat(width.saturating_sub(used + rw))));
    left.extend(right);
    Line::from(left)
}

/// A conversation's row: its sign, the name with what matched, why it's
/// set apart, its members, and its last message's day at the right.
fn conv_line(app: &App, c: &Conversation, members: u32, query: &str, width: usize) -> Line<'static> {
    let uid = c.user.as_deref().unwrap_or("");
    let icon = match c.kind {
        _ if c.archived => Span::styled(ARCHIVE, dim()),
        Kind::Channel => Span::styled("#", dim()),
        Kind::Private => Span::styled(LOCK, dim()),
        Kind::Direct if app.users.bots.contains(uid) => Span::raw(" "),
        Kind::Direct => crate::ui::presence_span(app, crate::ui::presence_of(app, uid)),
        Kind::Group => Span::styled("◉", dim()),
    };
    let gone = c.archived || c.guest;
    // Unread, as the sidebar bolds it.
    let (unread, mentions) = app.unread(&c.id);
    let base = match () {
        _ if gone => Style::new().fg(Color::Gray),
        _ if mentions > 0 || (unread && !app.muted(c)) => Style::new().fg(Color::White).bold(),
        _ => Style::new().fg(Color::White),
    };
    let mut left = vec![Span::raw("  "), icon, Span::raw(" ")];
    left.extend(lit_name(&app.label(c), query, base));
    left.extend(app.gone_suffix(c).map(|s| Span::styled(s, dim())));
    if members > 0 {
        let n = if members == 1 { tr(" · 1 member", " · 1 membro").to_string() } else { trf!(" · {members} members", " · {members} membros") };
        left.push(Span::styled(n, dim()));
    }
    let latest = app.last_message(c);
    let when = if latest > 0 { day(&(latest / 1000).to_string()) } else { String::new() };
    spread(left, vec![Span::styled(when, dim()), Span::raw(" ")], width)
}

/// A name with what the query matched lit.
fn lit_name(label: &str, query: &str, base: Style) -> Vec<Span<'static>> {
    let chars: Vec<char> = label.chars().collect();
    let mut mask = vec![false; chars.len()];
    if let Some((_, ranges)) = crate::text::find(label, query).filter(|_| !query.is_empty()) {
        for (a, b) in ranges {
            mask[a.min(chars.len())..b.min(chars.len())].iter_mut().for_each(|x| *x = true);
        }
    }
    spans_of(&chars, &mask, base)
}

/// Someone's handle, when the list has it.
fn handle_of<'a>(app: &'a App, uid: &str) -> Option<&'a str> {
    app.users.handles.iter().find(|(_, v)| v.as_str() == uid).map(|(h, _)| h.as_str())
}

/// Someone's row: presence, the name with what matched, "(you)", their
/// status emoji, their full name (or handle) dim, and their DM's last day.
fn person_line(app: &App, uid: &str, query: &str, width: usize) -> Line<'static> {
    let name = app.users.names.get(uid).cloned().unwrap_or_else(|| uid.to_string());
    let dot = crate::ui::presence_span(app, crate::ui::presence_of(app, uid));
    let mut left = vec![Span::raw("  "), dot, Span::raw(" ")];
    left.extend(lit_name(&name, query, Style::new().fg(Color::White)));
    if app.me.as_ref().is_some_and(|m| m.user_id == uid) {
        left.push(Span::styled(tr(" (you)", " (você)"), dim()));
    }
    if let Some(g) = app.users.status_glyph(uid) {
        left.push(Span::raw(format!(" {g}")));
    }
    let real = app.users.real.get(uid).filter(|r| !r.is_empty() && **r != name).cloned();
    if let Some(aside) = real.or_else(|| handle_of(app, uid).map(|h| format!("@{h}"))) {
        left.push(Span::raw("  "));
        left.extend(lit_name(&aside, query, dim()));
    }
    let latest = app.dm_with(uid).and_then(|id| app.convs.iter().find(|c| c.id == id)).map_or(0, |c| app.last_message(c));
    let when = if latest > 0 { day(&(latest / 1000).to_string()) } else { String::new() };
    spread(left, vec![Span::styled(when, dim()), Span::raw(" ")], width)
}

/// A conversation's members, when the workspace's list says.
fn members_of(app: &App, id: &str) -> u32 {
    app.directory.find(id).map_or(0, |j| app.directory.chans[j].members)
}

/// A found message's card: who, where, the day; then its text around the match.
fn hit_lines(app: &App, h: &Hit, query: &str, width: usize) -> Vec<Line<'static>> {
    let who = h.user.as_ref().and_then(|u| app.users.names.get(u)).cloned().unwrap_or_else(|| if h.username.is_empty() { "?".into() } else { h.username.clone() });
    let who = truncate(&who, width / 2);
    let mut left = vec![Span::raw("  "), Span::styled(who, Style::new().fg(Color::White).bold()), Span::raw(" "), Span::styled(context(app, h), dim())];
    if h.thread.is_some() {
        left.push(Span::styled(tr(" · in a thread", " · numa thread"), dim()));
    }
    let mut lines = vec![spread(left, vec![Span::styled(day(&h.ts), dim()), Span::raw(" ")], width)];
    let (chars, mask) = hit_text(app, h, query);
    for (c, m) in window(&chars, &mask, width.saturating_sub(4), CARD_LINES) {
        let mut spans = vec![Span::raw("  ")];
        spans.extend(spans_of(&c, &m, Style::new().fg(Color::Gray)));
        lines.push(Line::from(spans));
    }
    lines
}

/// The list's entries for the tab, in order, each result numbered as `search_items`.
fn entries(app: &App, width: usize) -> Vec<Entry> {
    let s = &app.search;
    let query = s.query();
    let mut out = Vec::new();
    let mut n = 0;
    let local = app.search_local();
    let conv_entry = |c: Conv, n: usize| {
        let conv = app.conv_of(c)?;
        let members = match c {
            Conv::Other(j) => app.directory.chans[j].members,
            Conv::Ours(_) => members_of(app, &conv.id),
        };
        Some(Entry { item: Some(n), lines: vec![conv_line(app, &conv, members, &query, width)] })
    };
    for p in app.search_parts() {
        out.push(heading(&p.title, p.count.map(|c| c.to_string())));
        if p.items.is_empty() {
            out.extend(p.empty.map(note));
        }
        for it in &p.items {
            let e = match *it {
                Item::Conv(c) => conv_entry(c, n),
                Item::Who(k) => match local.people.get(k) {
                    Some(Who::Person(uid)) => Some(Entry { item: Some(n), lines: vec![person_line(app, uid, &query, width)] }),
                    Some(Who::Conv(c)) => conv_entry(*c, n),
                    None => None,
                },
                _ => None,
            };
            out.extend(e);
            n += 1;
        }
        if let Some((more, tab)) = p.more {
            let k = TABS.iter().position(|t| *t == tab).unwrap_or(0) + 1;
            let text = trf!("+{more} more · Alt+{k}", "+{more} outros · Alt+{k}");
            out.push(Entry { item: Some(n), lines: vec![Line::from(vec![Span::raw("  "), Span::styled(text, Style::new().fg(Color::Cyan))])] });
            n += 1;
        }
        if p.narrow {
            out.push(note(tr("Keep typing to narrow it down", "Continue digitando para filtrar")));
        }
        if s.tab == Tab::All {
            out.push(Entry::plain(Line::from("")));
        }
    }
    if matches!(s.tab, Tab::Chans | Tab::People) {
        return out;
    }
    if !worth(&query) {
        if s.tab == Tab::Msgs || !query.is_empty() {
            out.push(note(tr("Type two letters or more to search messages", "Digite duas letras ou mais para buscar mensagens")));
        }
        return out;
    }
    // The conversation's recent messages first; Slack's search after, without them.
    let scope = app.search_scope();
    let recent = s.recent_now(scope.as_ref()).map_or(0, |r| r.hits.len());
    let msgs = app.search_msgs();
    if let (Some(sc), true) = (&scope, recent > 0) {
        out.push(heading(&sc.title, Some(recent.to_string())));
        for h in &msgs[..recent] {
            out.push(Entry { item: Some(n), lines: hit_lines(app, h, &query, width) });
            out.push(Entry::plain(Line::from("")));
            n += 1;
        }
    }
    let others = &msgs[recent..];
    let found = s.found_now();
    let count = found.filter(|f| f.page > 0).map(|f| f.total.to_string());
    out.push(heading(tr("Messages", "Mensagens"), count));
    match found {
        None => out.push(note(tr("Searching…", "Buscando…"))),
        Some(f) if others.is_empty() && f.loading && recent > 0 => out.push(note(tr("Loading more…", "Carregando mais…"))),
        Some(f) if others.is_empty() && f.loading => out.push(note(tr("Searching…", "Buscando…"))),
        Some(f) if others.is_empty() => match &f.error {
            Some(e) => out.push(note(e)),
            None if recent > 0 => out.push(note(tr("Nothing else matches the whole words", "Nada mais tem as palavras inteiras"))),
            None => out.push(note(tr("No messages match", "Nenhuma mensagem encontrada"))),
        },
        Some(f) => {
            for h in others {
                out.push(Entry { item: Some(n), lines: hit_lines(app, h, &query, width) });
                out.push(Entry::plain(Line::from("")));
                n += 1;
            }
            if f.loading {
                out.push(note(tr("Loading more…", "Carregando mais…")));
            } else if let Some(e) = &f.error {
                out.push(note(e));
            }
        }
    }
    out
}

/// The window, centered: the field, the tabs, the results and their
/// preview side by side, and a hint.
pub fn draw(f: &mut Frame, app: &mut App) {
    if !app.search.open {
        return;
    }
    let area = f.area();
    let rect = if area.width < 70 || area.height < 20 {
        area
    } else {
        let w = (area.width as u32 * 85 / 100) as u16;
        let h = (area.height as u32 * 85 / 100) as u16;
        let (w, h) = (w.max(70).min(area.width), h.max(20).min(area.height));
        Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h)
    };
    f.render_widget(Clear, rect);
    let title = Line::from(tr(" Search ", " Buscar ")).bold();
    let block = Block::bordered().border_type(BorderType::Rounded).border_style(dim()).title(title);
    let inner = block.inner(rect).inner(Margin { horizontal: 1, vertical: 0 });
    f.render_widget(block, rect);
    app.search.rect = rect;
    if inner.height < 9 || inner.width < 24 {
        return;
    }

    field(f, app, Rect::new(inner.x, inner.y, inner.width, 3));
    tabs(f, app, Rect::new(inner.x, inner.y + 3, inner.width, 1));
    f.render_widget(Paragraph::new("─".repeat(inner.width as usize)).style(dim()), Rect::new(inner.x, inner.y + 4, inner.width, 1));
    let body = Rect::new(inner.x, inner.y + 5, inner.width, inner.height - 6);
    if rect.width >= NARROW {
        let lw = body.width * 45 / 100;
        let list = Rect { width: lw, ..body };
        let side = Rect::new(body.x + lw + 2, body.y, body.width.saturating_sub(lw + 2), body.height);
        let bar = vec![Line::from("│"); body.height as usize];
        f.render_widget(Paragraph::new(bar).style(dim()), Rect::new(body.x + lw, body.y, 1, body.height));
        results(f, app, list);
        preview(f, app, side);
    } else {
        results(f, app, body);
    }
    hint(f, app, Rect::new(inner.x, inner.bottom() - 1, inner.width, 1));
}

/// The query, always focused, scrolled to keep the cursor in view.
fn field(f: &mut Frame, app: &App, area: Rect) {
    let e = &app.search.input;
    let room = (area.width as usize).saturating_sub(4 + 3);
    let chars: Vec<char> = e.text.chars().collect();
    let cursor = e.cursor.min(chars.len());
    let w = |c: &[char]| c.iter().map(|c| c.width().unwrap_or(0)).sum::<usize>();
    let mut start = 0;
    while start < cursor && w(&chars[start..cursor]) >= room {
        start += 1;
    }
    let shown: String = chars[start..].iter().collect();
    let text = if chars.is_empty() {
        Line::from(vec![Span::styled("🔍 ", dim()), Span::styled(truncate(tr("Search messages, channels and people", "Buscar mensagens, canais e pessoas"), room), dim())])
    } else {
        Line::from(vec![Span::styled("🔍 ", dim()), Span::raw(truncate(&shown, room + 1))])
    };
    let at = rounded_input(f, area, true, text);
    let x = at.x + 3 + w(&chars[start..cursor]).min(room) as u16;
    f.set_cursor_position((x, at.y));
}

/// "All · Messages 143 · Channels 12 · People 3", the one shown lit; the
/// messages' order at the right, a click changing it.
fn tabs(f: &mut Frame, app: &mut App, area: Rect) {
    let q = app.search.query();
    let local = (!q.is_empty()).then(|| app.search_local());
    let (chans, people) = (local.as_ref().map(|m| m.chans.len()), local.as_ref().map(|m| m.people.len()));
    let msgs = app.search.found_now().filter(|f| f.page > 0).map(|f| f.total);
    let label = |t: Tab| match t {
        Tab::All => tr("All", "Tudo").to_string(),
        Tab::Msgs => msgs.map_or_else(|| tr("Messages", "Mensagens").to_string(), |n| trf!("Messages {n}", "Mensagens {n}")),
        Tab::Chans => chans.map_or_else(|| tr("Channels", "Canais").to_string(), |n| trf!("Channels {n}", "Canais {n}")),
        Tab::People => people.map_or_else(|| tr("People", "Pessoas").to_string(), |n| trf!("People {n}", "Pessoas {n}")),
    };
    let mut spans = vec![Span::raw(" ")];
    let mut x = area.x + 1;
    let mut rects = Vec::new();
    for (i, t) in TABS.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ", dim()));
            x += 3;
        }
        let text = label(*t);
        let w = text.width() as u16;
        let style = if *t == app.search.tab { Style::new().fg(Color::Cyan).bold().underlined() } else { Style::new().fg(Color::Gray) };
        spans.push(Span::styled(text, style));
        rects.push((Rect::new(x, area.y, w.min(area.right().saturating_sub(x)), 1), *t));
        x += w;
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
    app.search.tabs = rects;
    app.search.sort_rect = Rect::default();
    if matches!(app.search.tab, Tab::All | Tab::Msgs) {
        let text = if app.search.sort == Sort::Score { tr("Most relevant ▾", "Mais relevantes ▾") } else { tr("Most recent ▾", "Mais recentes ▾") };
        let w = text.width() as u16;
        if x + w + 4 <= area.right() {
            let r = Rect::new(area.right() - w - 1, area.y, w, 1);
            f.render_widget(Paragraph::new(text).style(Style::new().fg(Color::Gray)), r);
            app.search.sort_rect = r;
        }
    }
}

/// The results, the selection kept in view unless the wheel moved the list.
fn results(f: &mut Frame, app: &mut App, area: Rect) {
    let width = area.width as usize;
    let list = entries(app, width);
    let items = list.iter().filter(|e| e.item.is_some()).count();
    let sel = app.search.sel.min(items.saturating_sub(1));
    // Each row: its line and the result it belongs to.
    let mut rows: Vec<(Line<'static>, Option<usize>)> = Vec::new();
    let mut span = (0, 0);
    for e in list {
        if e.item == Some(sel) {
            span = (rows.len(), rows.len() + e.lines.len());
        }
        for mut l in e.lines {
            if e.item == Some(sel) {
                let w = l.width();
                l.spans.push(Span::raw(" ".repeat(width.saturating_sub(w))));
                crate::ui::mark_selected(&mut l);
            }
            rows.push((l, e.item));
        }
    }
    let height = area.height as usize;
    let max = rows.len().saturating_sub(height);
    let s = &mut app.search;
    s.sel = sel;
    let mut scroll = s.scroll.min(max);
    if !s.wheel && items > 0 {
        // The selection in view; the heading over the first result too.
        if span.0 < scroll || sel == 0 {
            scroll = if sel == 0 { 0 } else { span.0 };
        } else if span.1 > scroll + height {
            scroll = (span.1 - height).min(max);
        }
    }
    (s.scroll, s.max_scroll, s.height) = (scroll, max, height);
    s.spots.clear();
    for (n, (line, item)) in rows.into_iter().skip(scroll).take(height).enumerate() {
        let r = Rect::new(area.x, area.y + n as u16, area.width, 1);
        if let Some(i) = item {
            s.spots.push((r, i));
        }
        f.render_widget(Paragraph::new(line), r);
    }
}

/// The right side: the messages around the found one picked, or the
/// conversation picked, as far as they are loaded.
fn preview(f: &mut Frame, app: &mut App, area: Rect) {
    let Some(item) = app.search_items().get(app.search.sel).copied() else {
        let text = tr("Nothing to preview", "Nada para mostrar");
        f.render_widget(Paragraph::new(text).style(dim().italic()), area);
        return;
    };
    let Some(key) = app.preview_key(item) else { return };
    // Seen: the last to go from the cache.
    if app.search.previews.contains_key(&key) {
        app.search.lru.retain(|k| k != &key);
        app.search.lru.push_back(key.clone());
    }
    let width = area.width as usize;
    let lines = match item {
        Item::Msg(k) => match app.search_hit(k) {
            Some(h) => msg_preview(app, h, &key, width, area.height as usize),
            None => Vec::new(),
        },
        Item::Conv(c) => match app.conv_of(c) {
            Some(conv) => conv_preview(app, &conv, &key, width, area.height as usize),
            None => Vec::new(),
        },
        Item::Who(k) => match (app.who(k), &key) {
            (Some(Who::Person(uid)), _) => person_preview(app, &uid, &key, width, area.height as usize),
            (Some(Who::Conv(c)), _) => app.conv_of(c).map(|conv| conv_preview(app, &conv, &key, width, area.height as usize)).unwrap_or_default(),
            (None, _) => Vec::new(),
        },
        Item::More(_) => Vec::new(),
    };
    f.render_widget(Paragraph::new(lines), area);
}

/// A message's lines in a preview: who and when, then its text.
fn message_lines(app: &App, m: &Message, width: usize, hit: Option<&str>, day_too: bool, max: usize) -> Vec<Line<'static>> {
    let names = app.names();
    let mut when = crate::ui::hhmm(&m.ts);
    if day_too {
        when = format!("{} {when}", day(&m.ts));
    }
    let mut out = vec![Line::from(vec![
        Span::styled(crate::ui::author(m, &names), Style::new().fg(Color::White).bold()),
        Span::raw("  "),
        Span::styled(when, dim()),
    ])];
    let mut text = crate::text::body(m, &names);
    for file in &m.files {
        text.push_str(&format!("\n📎 {}", file.name));
    }
    let text = text.trim().to_string();
    let base = Style::new().fg(Color::Gray);
    let mut body = marked_lines(&text, hit.unwrap_or(""), width.saturating_sub(2), base);
    if body.len() > max {
        body.truncate(max);
        if let Some(last) = body.last_mut() {
            last.push(Span::styled(" …", dim()));
        }
    }
    out.extend(body.into_iter().map(Line::from));
    // The found one: a bar down its side, as the conversation flashes it.
    let bar = if hit.is_some() { Span::styled("▎ ", Style::new().fg(Color::Cyan)) } else { Span::raw("  ") };
    out.into_iter()
        .map(|mut l| {
            l.spans.insert(0, bar.clone());
            l
        })
        .collect()
}

fn msg_preview(app: &App, h: &Hit, key: &Key, width: usize, height: usize) -> Vec<Line<'static>> {
    let query = app.search.query();
    let mut head = vec![Span::styled(context(app, h), Style::new().bold())];
    head.push(Span::styled(format!("  {} {}", day(&h.ts), crate::ui::hhmm(&h.ts)), dim()));
    if h.thread.is_some() {
        head.push(Span::styled(tr(" · in a thread", " · numa thread"), dim()));
    }
    let mut lines = vec![Line::from(head), Line::from("")];
    let top = lines.len();
    let Some(p) = app.search.previews.get(key) else {
        lines.push(Line::from(Span::styled(tr("Loading…", "Carregando…"), dim().italic())));
        return lines;
    };
    if let Some(e) = &p.error {
        lines.push(Line::from(Span::styled(e.clone(), dim())));
        return lines;
    }
    let (chars, mask) = hit_text(app, h, &query);
    let terms = format!("{} {query}", matched_words(&chars, &mask));
    let hit_day = day(&h.ts);
    let mut at = (top, top);
    for (i, m) in p.msgs.iter().enumerate() {
        if i == 1 && p.gap {
            lines.push(Line::from(Span::styled("  ⋯", dim())));
            lines.push(Line::from(""));
        }
        let is_hit = m.ts == h.ts;
        let start = lines.len();
        let max = if is_hit { 20 } else { 6 };
        lines.extend(message_lines(app, m, width, is_hit.then_some(terms.as_str()), day(&m.ts) != hit_day, max));
        if is_hit {
            at = (start, lines.len());
        }
        lines.push(Line::from(""));
    }
    // The found one in view, a third of the way down when it can't be at the top.
    let room = height.saturating_sub(top);
    if lines.len() > height && at.1 > height {
        let skip = at.0.saturating_sub(top + room / 3).min(lines.len().saturating_sub(height));
        let rest: Vec<Line> = lines.split_off(top).into_iter().skip(skip).collect();
        lines.extend(rest);
    }
    lines
}

fn conv_preview(app: &App, c: &Conversation, key: &Key, width: usize, height: usize) -> Vec<Line<'static>> {
    let sign = match c.kind {
        _ if c.archived => format!("{ARCHIVE} "),
        Kind::Channel => "# ".into(),
        Kind::Private => format!("{LOCK} "),
        Kind::Direct | Kind::Group => String::new(),
    };
    let mut lines = vec![Line::from(Span::styled(format!("{sign}{}", app.label(c)), Style::new().bold()))];
    let p = app.search.previews.get(key);
    let mut facts: Vec<String> = Vec::new();
    if c.archived {
        facts.push(tr("Archived", "Arquivado").into());
    }
    if c.guest || (app.directory.find(&c.id).is_some() && !app.convs.iter().any(|x| x.id == c.id)) {
        facts.push(tr("You are not a member", "Você não participa").into());
    }
    let members = p.and_then(|p| p.members).unwrap_or(members_of(app, &c.id));
    if members > 0 {
        facts.push(if members == 1 { tr("1 member", "1 membro").into() } else { trf!("{members} members", "{members} membros") });
    }
    if !facts.is_empty() {
        lines.push(Line::from(Span::styled(facts.join(" · "), dim())));
    }
    let names = app.names();
    for (label, text) in [(tr("Topic", "Tópico"), p.map(|p| p.topic.as_str())), (tr("Description", "Descrição"), p.map(|p| p.purpose.as_str()))] {
        let Some(text) = text.filter(|t| !t.trim().is_empty()) else { continue };
        let text = crate::text::shortcodes(&crate::text::mrkdwn(text, &names));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(label.to_string(), dim().bold())));
        for l in crate::ui::wrap(&text, width).into_iter().take(4) {
            lines.push(Line::from(Span::styled(l, Style::new().fg(Color::Gray))));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(tr("Latest messages", "Mensagens recentes"), dim().bold())));
    let loaded = app.messages.get(&c.id).filter(|m| !m.is_empty());
    let msgs: Vec<&Message> = match (loaded, p) {
        (Some(m), _) => m.iter().rev().take(LATEST).rev().collect(),
        (None, Some(p)) => p.msgs.iter().collect(),
        (None, None) => {
            lines.push(Line::from(Span::styled(tr("Loading…", "Carregando…"), dim().italic())));
            return lines;
        }
    };
    if msgs.is_empty() {
        let why = p.and_then(|p| p.error.clone()).unwrap_or_else(|| tr("No messages yet", "Nenhuma mensagem ainda").into());
        lines.push(Line::from(Span::styled(why, dim())));
        return lines;
    }
    let today = day(&crate::slack::unix_now().to_string());
    let mut body = Vec::new();
    for m in msgs {
        body.extend(message_lines(app, m, width, None, day(&m.ts) != today, 4));
        body.push(Line::from(""));
    }
    // The newest at the bottom: the oldest go when they don't fit.
    let room = height.saturating_sub(lines.len());
    let drop = body.len().saturating_sub(room);
    lines.extend(body.into_iter().skip(drop));
    lines
}

/// Someone picked: their names, title, presence and status, local time,
/// then their DM's latest messages, or how Enter starts one.
fn person_preview(app: &App, uid: &str, key: &Key, width: usize, height: usize) -> Vec<Line<'static>> {
    let name = app.users.names.get(uid).cloned().unwrap_or_else(|| uid.to_string());
    let mut head = vec![Span::styled(name.clone(), Style::new().bold())];
    if app.me.as_ref().is_some_and(|m| m.user_id == uid) {
        head.push(Span::styled(tr(" (you)", " (você)"), dim()));
    }
    let mut lines = vec![Line::from(head)];
    let real = app.users.real.get(uid).filter(|r| !r.is_empty() && **r != name);
    let names: Vec<String> = real.cloned().into_iter().chain(handle_of(app, uid).map(|h| format!("@{h}"))).collect();
    if !names.is_empty() {
        lines.push(Line::from(Span::styled(names.join(" · "), dim())));
    }
    let p = app.search.previews.get(key);
    let profile = p.and_then(|p| p.profile.as_ref());
    if let Some(t) = profile.map(|p| p.title.as_str()).filter(|t| !t.is_empty()) {
        lines.push(Line::from(Span::styled(truncate(t, width), Style::new().fg(Color::Gray))));
    }
    let status = app.users.status(uid).map(|(_, text)| {
        let glyph = app.users.status_glyph(uid).unwrap_or_default();
        format!("{glyph} {}", crate::text::shortcodes(text)).trim().to_string()
    });
    lines.push(Line::from(""));
    if app.presence.contains_key(uid) {
        let pres = crate::ui::presence_of(app, uid);
        let word = if pres.active { tr("Active", "Ativo") } else { tr("Away", "Ausente") };
        let rest = status.filter(|s| !s.is_empty()).map_or(word.to_string(), |s| format!("{word} · {s}"));
        lines.push(Line::from(vec![crate::ui::presence_span(app, pres), Span::raw(" "), Span::raw(truncate(&rest, width.saturating_sub(2)))]));
    } else if let Some(s) = status.filter(|s| !s.is_empty()) {
        lines.push(Line::from(truncate(&s, width)));
    }
    if let Some(tz) = profile.and_then(|p| chrono::FixedOffset::east_opt(p.tz_offset)) {
        let local = i18n::clock(&chrono::Utc::now().with_timezone(&tz));
        lines.push(Line::from(Span::styled(trf!("🕐 {local} local time", "🕐 {local} hora local"), dim())));
    }
    lines.push(Line::from(""));
    let Some(dm) = app.dm_with(uid) else {
        lines.push(Line::from(Span::styled(tr("Enter starts a direct message", "Enter começa uma mensagem direta"), dim().italic())));
        return lines;
    };
    lines.push(Line::from(Span::styled(tr("Latest messages", "Mensagens recentes"), dim().bold())));
    let loaded = app.messages.get(&dm).filter(|m| !m.is_empty());
    let msgs: Vec<&Message> = match (loaded, p) {
        (Some(m), _) => m.iter().rev().take(LATEST).rev().collect(),
        (None, Some(p)) => p.msgs.iter().collect(),
        (None, None) => {
            lines.push(Line::from(Span::styled(tr("Loading…", "Carregando…"), dim().italic())));
            return lines;
        }
    };
    if msgs.is_empty() {
        lines.push(Line::from(Span::styled(tr("No messages yet", "Nenhuma mensagem ainda"), dim())));
        return lines;
    }
    let today = day(&crate::slack::unix_now().to_string());
    let mut body = Vec::new();
    for m in msgs {
        body.extend(message_lines(app, m, width, None, day(&m.ts) != today, 4));
        body.push(Line::from(""));
    }
    let drop = body.len().saturating_sub(height.saturating_sub(lines.len()));
    lines.extend(body.into_iter().skip(drop));
    lines
}

/// The keys on the left; what Slack is doing, or failed at, on the right.
fn hint(f: &mut Frame, app: &App, area: Rect) {
    let sort = match app.search.sort {
        _ if !matches!(app.search.tab, Tab::All | Tab::Msgs) => "",
        Sort::Date => tr(" · Ctrl+S by relevance", " · Ctrl+S por relevância"),
        Sort::Score => tr(" · Ctrl+S by date", " · Ctrl+S por data"),
    };
    let esc = if app.search.input.text.is_empty() { tr("Esc close", "Esc fechar") } else { tr("Esc clear", "Esc limpar") };
    let keys = format!(" {}{sort} · {esc}", tr("↑↓ move · Enter open · Tab section", "↑↓ mover · Enter abrir · Tab seção"));
    let found = app.search.found_now();
    let recent = app.search.recent_now(app.search_scope().as_ref());
    let status = match found {
        _ if recent.is_some_and(|r| r.loading) => tr("Searching…", "Buscando…").to_string(),
        Some(f) if f.loading => tr("Searching…", "Buscando…").to_string(),
        Some(f) => f.error.clone().unwrap_or_default(),
        None if worth(&app.search.query()) => tr("Searching…", "Buscando…").to_string(),
        None => String::new(),
    };
    let sw = (status.width() as u16 + 1).min(area.width / 2);
    let kw = area.width - sw;
    f.render_widget(Paragraph::new(truncate(&keys, kw as usize)).style(dim()), Rect { width: kw, ..area });
    f.render_widget(Paragraph::new(truncate(&status, sw as usize)).style(dim().italic()), Rect::new(area.x + kw, area.y, sw, 1));
}
