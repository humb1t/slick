//! The search window, Ctrl+K (Ctrl+T, or a click on the sidebar's box): a
//! large box over everything, as in Slack's and other web apps' search.
//! The query finds conversations here, with the sidebar's matcher
//! (`App::conv_matches`), and messages on Slack's side (`search.messages`),
//! under three tabs: All, Conversations, Messages. On the right, a preview
//! of the result picked: the messages around a found one, or a
//! conversation's latest. Enter opens it, a message at its place.

use crate::app::{App, Event, Row};
use crate::editor::Editor;
use crate::i18n::{self, tr};
use crate::slack::{Conversation, Kind, Message, Res, Slack};
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
/// Conversations "All" shows before the messages; the tab shows them all.
const ALL_CONVS: usize = 6;
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
    Convs,
    Msgs,
}

const TABS: [Tab; 3] = [Tab::All, Tab::Convs, Tab::Msgs];

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
}

#[derive(Default)]
pub struct Preview {
    msgs: Vec<Message>,
    /// A thread: replies were left out between the parent and those shown.
    gap: bool,
    topic: String,
    purpose: String,
    members: Option<u32>,
    error: Option<String>,
}

/// Answers from Slack, as `Event::Search`.
pub enum Ev {
    Found { query: String, sort: Sort, page: usize, res: Res<Page> },
    Preview { key: Key, preview: Preview },
}

/// A conversation in the results: ours, an index in `convs`, or one of
/// the workspace's we are not in, an index in `directory`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Conv {
    Ours(usize),
    Other(usize),
}

/// A result the selection can stop on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Item {
    Conv(Conv),
    Msg(usize),
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
    /// The conversations the last query matched: worked out once per
    /// query and state of the lists, not at each frame.
    convs: RefCell<(String, (u64, usize, usize), Vec<Conv>)>,
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
    fn query(&self) -> String {
        self.input.text.trim().to_string()
    }

    fn found_now(&self) -> Option<&Found> {
        self.found.get(&(self.query(), self.sort))
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
        s.reset();
        self.dirty = true;
    }

    pub(crate) fn close_search(&mut self) {
        self.search.open = false;
        self.search.want = None;
        self.dirty = true;
    }

    /// The conversations the query matches, best first; with no query,
    /// the ones opened lately. Worked out once per query (`Search::convs`).
    fn search_convs(&self) -> Vec<Conv> {
        let q = self.search.query();
        if q.is_empty() {
            return self.recents.iter().filter_map(|id| self.convs.iter().position(|c| &c.id == id)).map(Conv::Ours).collect();
        }
        let state = (self.dir_gen, self.convs.len(), self.users.names.len());
        {
            let memo = self.search.convs.borrow();
            if memo.0 == q && memo.1 == state {
                return memo.2.clone();
            }
        }
        let list: Vec<Conv> = self
            .conv_matches(&q)
            .into_iter()
            .filter_map(|r| match r {
                Row::Conv(i) => Some(Conv::Ours(i)),
                Row::Other(j) => Some(Conv::Other(j)),
                _ => None,
            })
            .collect();
        *self.search.convs.borrow_mut() = (q, state, list.clone());
        list
    }

    /// What the tab lists, in order: the results the selection stops on.
    fn search_items(&self) -> Vec<Item> {
        let tab = self.search.tab;
        let mut out: Vec<Item> = Vec::new();
        if tab != Tab::Msgs {
            let convs = self.search_convs();
            let n = if tab == Tab::All { ALL_CONVS } else { convs.len() };
            out.extend(convs.into_iter().take(n).map(Item::Conv));
        }
        if tab != Tab::Convs {
            let n = self.search.found_now().map_or(0, |f| f.hits.len());
            out.extend((0..n).map(Item::Msg));
        }
        out
    }

    fn search_hit(&self, k: usize) -> Option<&Hit> {
        self.search.found_now()?.hits.get(k)
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
            Item::Msg(k) => self.search_hit(k).map(|h| Key::Msg { chan: h.chan.clone(), ts: h.ts.clone(), thread: h.thread.clone() }),
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
        }
        let items = self.search_items();
        let sel = items.get(self.search.sel).copied();
        // The next page, as the selection nears the end of those loaded.
        if let Some(Item::Msg(k)) = sel {
            let key = (self.search.query(), self.search.sort);
            if let Some(f) = self.search.found.get_mut(&key) {
                if k + NEAR >= f.hits.len() && f.page < f.pages && !f.loading && f.error.is_none() {
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
        }
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
            KeyCode::Char('k' | 't') if ctrl && !shift => return self.close_search(),
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
            KeyCode::Char(c @ '1'..='3') if alt => return self.search_tab(TABS[c as usize - '1' as usize]),
            KeyCode::Char('s') if ctrl => {
                let s = &mut self.search;
                s.sort = if s.sort == Sort::Date { Sort::Score } else { Sort::Date };
                return s.reset();
            }
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

    pub(crate) fn search_paste(&mut self, s: &str) {
        let line = s.lines().next().unwrap_or("").to_string();
        self.search.input.insert(&line);
        self.search.reset();
    }

    /// The wheel scrolls the list; a click picks a result, a second one
    /// opens it; one on a tab shows it; one outside closes.
    pub(crate) fn search_mouse(&mut self, m: MouseEvent) {
        let at = Position::new(m.column, m.row);
        let s = &mut self.search;
        match m.kind {
            MouseEventKind::ScrollUp => (s.scroll, s.wheel) = (s.scroll.saturating_sub(3), true),
            MouseEventKind::ScrollDown => (s.scroll, s.wheel) = ((s.scroll + 3).min(s.max_scroll), true),
            MouseEventKind::Down(_) if !s.rect.contains(at) => return self.close_search(),
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(tab) = s.tabs.iter().find(|(r, _)| r.contains(at)).map(|(_, t)| *t) {
                    self.search_tab(tab);
                } else if let Some(i) = s.spots.iter().find(|(r, _)| r.contains(at)).map(|(_, i)| *i) {
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
    /// preview to join), a message at its place, a reply in its thread.
    fn open_selected(&mut self) {
        let Some(item) = self.search_items().get(self.search.sel).copied() else { return };
        match item {
            Item::Conv(c) => {
                let Some(conv) = self.conv_of(c) else { return };
                self.close_search();
                self.open(conv.id);
            }
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
    for (a, b) in crate::text::marks(&text, query) {
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
    let label = app.label(c);
    let chars: Vec<char> = label.chars().collect();
    let mut mask = vec![false; chars.len()];
    if let Some((_, ranges)) = crate::text::find(&label, query).filter(|_| !query.is_empty()) {
        for (a, b) in ranges {
            mask[a.min(chars.len())..b.min(chars.len())].iter_mut().for_each(|x| *x = true);
        }
    }
    let gone = c.archived || c.guest;
    let base = if gone { Style::new().fg(Color::Gray) } else { Style::new().fg(Color::White) };
    let mut left = vec![Span::raw("  "), icon, Span::raw(" ")];
    left.extend(spans_of(&chars, &mask, base));
    left.extend(app.gone_suffix(c).map(|s| Span::styled(s, dim())));
    if members > 0 {
        let n = if members == 1 { tr(" · 1 member", " · 1 membro").to_string() } else { trf!(" · {members} members", " · {members} membros") };
        left.push(Span::styled(n, dim()));
    }
    let latest = app.last_message(c);
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
    if s.tab != Tab::Msgs {
        let convs = app.search_convs();
        let shown = if s.tab == Tab::All { ALL_CONVS.min(convs.len()) } else { convs.len() };
        if query.is_empty() {
            out.push(heading(tr("Recent", "Recentes"), None));
        } else if !convs.is_empty() || s.tab == Tab::Convs {
            out.push(heading(tr("Conversations", "Conversas"), Some(convs.len().to_string())));
        }
        for c in &convs[..shown] {
            let Some(conv) = app.conv_of(*c) else { continue };
            let members = match c {
                Conv::Other(j) => app.directory.chans[*j].members,
                Conv::Ours(_) => members_of(app, &conv.id),
            };
            out.push(Entry { item: Some(n), lines: vec![conv_line(app, &conv, members, &query, width)] });
            n += 1;
        }
        if convs.len() > shown {
            let more = convs.len() - shown;
            out.push(note(&trf!("+{more} more · Alt+2", "+{more} outras · Alt+2")));
        }
        if convs.is_empty() && s.tab == Tab::Convs {
            out.push(note(if query.is_empty() { tr("Type to find a conversation", "Digite para achar uma conversa") } else { tr("No conversations match", "Nenhuma conversa encontrada") }));
        }
        if s.tab == Tab::All && !out.is_empty() {
            out.push(Entry::plain(Line::from("")));
        }
    }
    if s.tab == Tab::Convs {
        return out;
    }
    if !worth(&query) {
        if s.tab == Tab::Msgs || !query.is_empty() {
            out.push(note(tr("Type two letters or more to search messages", "Digite duas letras ou mais para buscar mensagens")));
        }
        return out;
    }
    let found = s.found_now();
    let count = found.filter(|f| f.page > 0).map(|f| f.total.to_string());
    out.push(heading(tr("Messages", "Mensagens"), count));
    match found {
        None => out.push(note(tr("Searching…", "Buscando…"))),
        Some(f) if f.hits.is_empty() && f.loading => out.push(note(tr("Searching…", "Buscando…"))),
        Some(f) if f.hits.is_empty() => match &f.error {
            Some(e) => out.push(note(e)),
            None => out.push(note(tr("No messages match", "Nenhuma mensagem encontrada"))),
        },
        Some(f) => {
            for h in &f.hits {
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
        Line::from(vec![Span::styled("🔍 ", dim()), Span::styled(truncate(tr("Search conversations and messages", "Buscar conversas e mensagens"), room), dim())])
    } else {
        Line::from(vec![Span::styled("🔍 ", dim()), Span::raw(truncate(&shown, room + 1))])
    };
    let at = rounded_input(f, area, true, text);
    let x = at.x + 3 + w(&chars[start..cursor]).min(room) as u16;
    f.set_cursor_position((x, at.y));
}

/// "All · Conversations 12 · Messages 143", the one shown lit.
fn tabs(f: &mut Frame, app: &mut App, area: Rect) {
    let q = app.search.query();
    let convs = if q.is_empty() { None } else { Some(app.search_convs().len()) };
    let msgs = app.search.found_now().filter(|f| f.page > 0).map(|f| f.total);
    let label = |t: Tab| match t {
        Tab::All => tr("All", "Tudo").to_string(),
        Tab::Convs => convs.map_or_else(|| tr("Conversations", "Conversas").to_string(), |n| trf!("Conversations {n}", "Conversas {n}")),
        Tab::Msgs => msgs.map_or_else(|| tr("Messages", "Mensagens").to_string(), |n| trf!("Messages {n}", "Mensagens {n}")),
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

/// The keys on the left; what Slack is doing, or failed at, on the right.
fn hint(f: &mut Frame, app: &App, area: Rect) {
    let sort = if app.search.sort == Sort::Date { tr("Ctrl+S: by relevance", "Ctrl+S: por relevância") } else { tr("Ctrl+S: by date", "Ctrl+S: por data") };
    let keys = format!(
        " {} · {sort} · {}",
        tr("↑↓ move · Enter open · Tab section", "↑↓ mover · Enter abrir · Tab seção"),
        tr("Esc close", "Esc fechar")
    );
    let found = app.search.found_now();
    let status = match found {
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
