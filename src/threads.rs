//! Threads you follow, as Slack's "Threads" view: the ones you started,
//! replied to, were mentioned in or followed. A row on top of the sidebar
//! opens them in place of the conversation, one card each; a reply in one
//! counts as unread there and is announced like a new message (`events`).
//! Opening a thread in the panel, reading it there as replies come, or
//! seeing its new replies on a card of the view marks it read in Slack.

use crate::app::{App, Event, Focus};
use crate::i18n::{self, tr};
use crate::slack::{parse_message, Kind, Message, Res, Slack};
use crate::ui::{dim, packed_lines, place, presence_of, presence_span, truncate, Rendered, MAX_INPUT_ROWS};
use chrono::{Datelike, Local, TimeZone};
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Constraint, Layout, Margin, Position, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, ListItem, Paragraph};
use ratatui::Frame;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthStr;

/// nf-oct-comment_discussion: two balloons in one cell, like Slack's Threads icon.
pub const ICON: &str = "\u{f442}";
const LOCK: &str = "\u{f023}";
// Nerd Font squares, as the thread panel's checkbox.
const BOX_OFF: &str = "\u{f096}";
const BOX_ON: &str = "\u{f14a}";
/// Pages of threads read each time; Slack sends at most 10 a page.
const PAGES: usize = 3;
/// How long a burst of nudges waits before the list is read again.
const SOON: Duration = Duration::from_millis(700);
/// Unread replies a thread keeps in the snapshot.
const PREVIEW: usize = 5;
/// How long the card the pill jumped to stays lit.
const FLASH: Duration = Duration::from_millis(1800);
/// After a thread is read here, how long Slack's counts may still be from before it.
const SETTLE: Duration = Duration::from_secs(15);

/// A thread you follow, as the view lists it.
#[derive(Clone)]
pub struct Sub {
    pub chan: String,
    pub root: Message,
    /// The newest replies Slack sent along (the unread ones among them), oldest first.
    pub replies: Vec<Message>,
    /// Up to where you read it; what came after is unread.
    pub last_read: String,
    /// The newest reply.
    pub latest: String,
    /// How many people replied, also those `root.reply_users` leaves out.
    people: usize,
}

impl Sub {
    pub fn unread(&self) -> bool {
        !self.latest.is_empty() && self.latest > self.last_read
    }

    /// The replies after where you read, which the card shows under "New".
    /// How many replies are new: those after where you read among the ones
    /// Slack sent, at least one when it is unread.
    pub fn new_replies(&self) -> usize {
        if self.unread() { self.unread_replies().count().max(1) } else { 0 }
    }

    fn unread_replies(&self) -> impl Iterator<Item = &Message> {
        self.replies.iter().filter(move |m| m.ts > self.last_read)
    }

    /// When something last happened in it, for the order.
    fn activity(&self) -> &str {
        if self.latest.is_empty() { &self.root.ts } else { &self.latest }
    }
}

fn key(chan: &str, ts: &str) -> String {
    format!("{chan}:{ts}")
}

/// Unread first (and those read while the view showed them, which keep
/// their place), then the newest activity first, as Slack lists them.
fn order(list: &mut [Sub], kept: &HashMap<String, String>) {
    let fresh = |s: &Sub| s.unread() || kept.contains_key(&key(&s.chan, &s.root.ts));
    list.sort_by(|a, b| fresh(b).cmp(&fresh(a)).then_with(|| b.activity().cmp(a.activity())));
}

pub enum Ev {
    /// `client.counts`' threads: any unread, and how many replies mention you.
    Counts((bool, u64)),
    /// The list, and Slack's count of the unread replies in all of it.
    View(Res<(Vec<Sub>, u64)>),
    /// A thread marked read in Slack.
    Marked(Res<()>),
    /// All of a thread's replies, for a card expanded in place.
    Expanded { chan: String, ts: String, res: Res<Vec<Message>> },
}

#[derive(Default)]
pub struct Threads {
    pub list: Vec<Sub>,
    pub loaded: bool,
    pub error: Option<String>,
    fetching: bool,
    /// Asked for again while a fetch was on its way.
    again: bool,
    /// When to read the list, a moment after the first nudge.
    soon: Option<Instant>,
    /// Slack's own counts: a thread with unread replies, and the replies that mention you.
    pub unread: bool,
    pub mentions: u64,
    /// Unread replies in the threads you follow: Slack's total when it says
    /// more than the list shows, less what was read here since.
    pub new: u64,
    /// The view is in the main pane, in place of a conversation.
    pub open: bool,
    /// The selected card.
    pub sel: usize,
    /// The first line of the list on screen.
    pub top: usize,
    /// Set by the last draw: how far `top` goes, and how many lines show.
    max_top: usize,
    page: usize,
    /// The next draw brings the selected card into view.
    reveal: bool,
    /// The newest reply already accounted for, per thread: newer ones are news.
    seen: HashMap<String, String>,
    /// The first reading of the list: replies from before it were waiting at
    /// start-up, not news.
    since: Option<String>,
    /// Read here up to this reply, until Slack's list says so too.
    marked: HashMap<String, String>,
    /// Cards whose "N more replies" was opened: all their replies, oldest first.
    expanded: HashMap<String, Vec<Message>>,
    /// Those being fetched.
    expanding: HashSet<String>,
    /// Unread cards the "↑ N unread threads" pill was closed on, by thread
    /// and newest reply: a new reply above brings it back.
    dismissed: HashSet<String>,
    /// The card the pill jumped to, lit for a moment.
    flash: Option<(String, Instant)>,
    /// Cards read while the view was open, and up to where they had been
    /// read before: their "New" line and their place stay until it opens again.
    kept: HashMap<String, String>,
    /// When a thread was last read here: Slack's counts may lag for a moment.
    read_at: Option<Instant>,
    /// Set by the last draw: unread cards whose new replies are on screen.
    in_view: Vec<(String, String, String)>,
}

impl Threads {
    /// Unread, or read while the view showed it: it keeps its "New" line.
    fn fresh(&self, s: &Sub) -> bool {
        s.unread() || self.kept.contains_key(&key(&s.chan, &s.root.ts))
    }

    /// A thread was read here a moment ago, and Slack may not count it yet.
    fn settling(&self) -> bool {
        self.read_at.is_some_and(|t| t.elapsed() < SETTLE)
    }

    /// What the list says: any thread unread, and the unread replies that mention you.
    fn local_counts(&self, me: &str) -> (bool, u64) {
        let unread = self.list.iter().any(Sub::unread);
        let mentions = self.list.iter().map(|s| s.unread_replies().filter(|m| crate::events::mentions(m, me)).count() as u64).sum();
        (unread, mentions)
    }
}

/// Where the last draw put the cards, for the mouse.
#[derive(Default)]
pub struct Areas {
    pub list: Rect,
    /// Each card on screen and its place in the list.
    pub cards: Vec<(Rect, usize)>,
    /// Each card's reply box.
    pub replies: Vec<(Rect, usize)>,
    /// "Also send to #channel" under the reply being typed.
    pub broadcast: Rect,
    /// "N more replies", which shows them in the card.
    pub more: Vec<(Rect, usize)>,
    /// "↑ N unread threads" over the list's top, and its ✕.
    pub pill: Rect,
    pub pill_close: Rect,
    /// The unread cards above the view, which ✕ dismisses.
    pub above: Vec<String>,
}

impl Slack {
    /// The threads you follow, newest activity first, as Slack's Threads view
    /// lists them. Undocumented, like `counts`: session tokens only.
    pub fn thread_view(&self) -> Res<(Vec<Sub>, u64)> {
        let mut out: Vec<Sub> = Vec::new();
        let mut total = None;
        let mut known = HashSet::new();
        let mut current = String::new();
        for _ in 0..PAGES {
            let mut p = vec![("limit", "10")];
            if !current.is_empty() {
                p.push(("current_ts", current.as_str()));
            }
            let v = self.call("subscriptions.thread.getView", &p)?;
            // Every thread's, not only this page's.
            total = total.or(v["total_unread_replies"].as_u64());
            for t in v["threads"].as_array().into_iter().flatten() {
                let r = &t["root_msg"];
                let Some(chan) = r["channel"].as_str().filter(|c| !c.is_empty()).map(String::from) else { continue };
                let root = parse_message(r);
                if !known.insert(key(&chan, &root.ts)) {
                    continue;
                }
                let mut replies: Vec<Message> = t["latest_replies"].as_array().into_iter().flatten().map(parse_message).collect();
                replies.sort_by(|a, b| a.ts.cmp(&b.ts));
                let latest = r["latest_reply"].as_str().map(String::from).or_else(|| replies.last().map(|m| m.ts.clone())).unwrap_or_default();
                let last_read = r["last_read"].as_str().unwrap_or("").to_string();
                let people = r["reply_users_count"].as_u64().unwrap_or(0) as usize;
                out.push(Sub { chan, root, replies, last_read, latest, people });
            }
            // The next page: what was active before the last thread of this one.
            let last = out.last().map(|s| s.activity().to_string()).unwrap_or_default();
            if v["has_more"] != true || last.is_empty() || last == current {
                break;
            }
            current = last;
        }
        Ok((out, total.unwrap_or(0)))
    }

    /// Marks a thread read up to the reply `ts`, as reading it in Slack does.
    pub fn mark_thread(&self, chan: &str, thread_ts: &str, ts: &str) -> Res<()> {
        self.call("subscriptions.thread.mark", &[("channel", chan), ("thread_ts", thread_ts), ("ts", ts), ("read", "1")]).map(|_| ())
    }
}

impl App {
    /// Reads the list again in a moment: a burst of nudges costs one request.
    pub(crate) fn threads_soon(&mut self) {
        let at = Instant::now() + SOON;
        self.threads.soon = Some(self.threads.soon.map_or(at, |t| t.min(at)));
    }

    /// From `tick`.
    pub(crate) fn poll_threads(&mut self) {
        if self.threads.soon.is_some_and(|t| t <= Instant::now()) {
            self.threads.soon = None;
            self.fetch_threads();
        }
        self.read_in_view();
    }

    /// Cards whose new replies the last draw showed are read, as Slack's view
    /// does: only with the window focused.
    fn read_in_view(&mut self) {
        let seen = std::mem::take(&mut self.threads.in_view);
        if !self.threads.open || !self.focused {
            return;
        }
        for (chan, ts, latest) in seen {
            self.read_thread(&chan, &ts, &latest, true);
        }
    }

    fn fetch_threads(&mut self) {
        if self.threads.fetching {
            self.threads.again = true;
            return;
        }
        self.threads.fetching = true;
        self.spawn(|s| Event::Threads(Ev::View(s.thread_view())));
    }

    pub(crate) fn on_threads(&mut self, ev: Ev) {
        match ev {
            Ev::Counts((mut unread, mut mentions)) => {
                // Right after a read here, Slack's counts may be from before it:
                // they don't bring back what the list says was read.
                if self.threads.settling() {
                    let me = self.me.as_ref().map(|m| m.user_id.clone()).unwrap_or_default();
                    let (any, local) = self.threads.local_counts(&me);
                    unread &= any;
                    mentions = mentions.min(local);
                }
                let changed = (unread, mentions) != (self.threads.unread, self.threads.mentions);
                (self.threads.unread, self.threads.mentions) = (unread, mentions);
                // The first counts, a change, or (with the websocket down,
                // which would have said) unread ones that may have grown.
                if !self.threads.loaded || changed || (unread && !self.live) {
                    self.threads_soon();
                }
            }
            Ev::View(res) => self.on_view(res),
            Ev::Expanded { chan, ts, res } => {
                let k = key(&chan, &ts);
                self.threads.expanding.remove(&k);
                match res {
                    // The root comes first in the answer; the card has it already.
                    Ok(msgs) => {
                        self.threads.expanded.insert(k, msgs.into_iter().filter(|m| m.ts != ts).collect());
                    }
                    Err(e) => self.error = Some(e),
                }
                self.dirty = true;
            }
            Ev::Marked(res) => {
                if let Err(e) = res {
                    self.error = Some(e);
                }
                self.counts_soon();
            }
        }
    }

    fn on_view(&mut self, res: Res<(Vec<Sub>, u64)>) {
        self.threads.fetching = false;
        if std::mem::take(&mut self.threads.again) {
            self.threads_soon();
        }
        let (mut list, total) = match res {
            Ok(l) => l,
            Err(e) => return self.threads.error = Some(e),
        };
        self.threads.error = None;
        // Read here, and Slack's list not there yet: it stays read, and its
        // replies leave Slack's total too. Once Slack has it, Slack decides.
        let mut gone = 0u64;
        for s in &mut list {
            let k = key(&s.chan, &s.root.ts);
            match self.threads.marked.get(&k).cloned() {
                Some(m) if m > s.last_read => {
                    let before = s.new_replies();
                    s.last_read = m;
                    gone += (before - s.new_replies()) as u64;
                }
                Some(_) => {
                    self.threads.marked.remove(&k);
                }
                None => {}
            }
        }
        self.announce(&list);
        // An expanded card with replies newer than it shows reads them again.
        let stale: Vec<(String, String)> = list
            .iter()
            .filter(|s| self.threads.expanded.get(&key(&s.chan, &s.root.ts)).is_some_and(|r| r.last().is_none_or(|m| m.ts < s.latest)))
            .map(|s| (s.chan.clone(), s.root.ts.clone()))
            .collect();
        for (chan, ts) in stale {
            self.expand(chan, ts);
        }
        let sel = self.threads.list.get(self.threads.sel).map(|s| key(&s.chan, &s.root.ts));
        order(&mut list, &self.threads.kept);
        self.threads.list = list;
        self.threads.loaded = true;
        let sum: u64 = self.threads.list.iter().map(|s| s.new_replies() as u64).sum();
        let slack = total.saturating_sub(gone);
        // Right after a read here, a total from before it doesn't grow the
        // badge back; new replies in the list still do.
        self.threads.new = if self.threads.settling() { slack.min(self.threads.new.max(sum)) } else { slack }.max(sum);
        // Replies the list has are unread even if counts held back by `settling` said otherwise.
        let me = self.me.as_ref().map(|m| m.user_id.clone()).unwrap_or_default();
        let (any, mentions) = self.threads.local_counts(&me);
        self.threads.unread |= any;
        self.threads.mentions = self.threads.mentions.max(mentions);
        self.follow_selection(sel);
        // A thread open in the panel that got replies is read at once.
        self.read_open_thread();
        self.dirty = true;
    }

    /// The selection stays on its thread when the list reorders.
    fn follow_selection(&mut self, sel: Option<String>) {
        let t = &mut self.threads;
        if let Some(i) = sel.and_then(|k| t.list.iter().position(|s| key(&s.chan, &s.root.ts) == k)) {
            t.sel = i;
        }
        t.sel = t.sel.min(t.list.len().saturating_sub(1));
    }

    /// Replies other people sent to threads you follow since the last
    /// reading: `message.new`, like a message in a conversation. Not for a
    /// muted channel (unless it mentions you), nor for the thread being read.
    fn announce(&mut self, list: &[Sub]) {
        let me = self.me.as_ref().map(|m| m.user_id.clone()).unwrap_or_default();
        let first = self.threads.since.is_none();
        let since = self.threads.since.get_or_insert_with(|| format!("{}.000000", chrono::Local::now().timestamp())).clone();
        let reading = self.thread.as_ref().filter(|t| !t.inline && self.focused).map(|t| key(&t.chan, &t.ts));
        for s in list {
            let k = key(&s.chan, &s.root.ts);
            let seen = self.threads.seen.get(&k).cloned().unwrap_or_default().max(since.clone());
            if !first && self.speaking() && reading.as_ref() != Some(&k) {
                let muted = self.convs.iter().find(|c| c.id == s.chan).is_some_and(|c| self.muted(c));
                let conv = self.conv_json(&s.chan);
                for m in s.replies.iter().filter(|m| m.ts > seen && m.ts > s.last_read && m.user.as_deref() != Some(me.as_str())) {
                    let mention = crate::events::mentions(m, &me);
                    if muted && !mention {
                        continue;
                    }
                    let data = json!({ "conversation": conv.clone(), "message": self.message_json(m), "mention": mention, "thread": true });
                    self.emit("message.new", data);
                }
            }
            let newest = s.replies.last().map(|m| m.ts.clone()).unwrap_or_default().max(s.latest.clone());
            let seen = self.threads.seen.entry(k).or_default();
            if newest > *seen {
                *seen = newest;
            }
        }
    }

    /// Marks a thread read up to the reply `upto`, here and in Slack, once per
    /// reply. One the list doesn't have is marked only when it's followed.
    pub(crate) fn read_thread(&mut self, chan: &str, ts: &str, upto: &str, followed: bool) {
        let k = key(chan, ts);
        if upto.is_empty() || self.threads.marked.get(&k).is_some_and(|m| m.as_str() >= upto) {
            return;
        }
        match self.threads.list.iter_mut().find(|s| s.chan == chan && s.root.ts == ts) {
            Some(s) if s.last_read.as_str() >= upto => return,
            Some(s) => {
                // Read in front of you: its "New" line and place stay for now.
                if self.threads.open {
                    self.threads.kept.entry(k.clone()).or_insert_with(|| s.last_read.clone());
                }
                let before = s.new_replies();
                s.last_read = upto.to_string();
                let read = before - s.new_replies();
                self.threads.new = self.threads.new.saturating_sub(read as u64);
            }
            None if !followed => return,
            None => {}
        }
        self.threads.marked.insert(k, upto.to_string());
        // The row and the badge follow at once; Slack's counts confirm them.
        let me = self.me.as_ref().map(|m| m.user_id.clone()).unwrap_or_default();
        let t = &mut self.threads;
        (t.unread, t.mentions) = t.local_counts(&me);
        t.read_at = Some(Instant::now());
        let sel = t.list.get(t.sel).map(|s| key(&s.chan, &s.root.ts));
        order(&mut t.list, &t.kept);
        self.follow_selection(sel);
        let (chan, ts, upto) = (chan.to_string(), ts.to_string(), upto.to_string());
        self.spawn(move |s| Event::Threads(Ev::Marked(s.mark_thread(&chan, &ts, &upto))));
        self.dirty = true;
    }

    /// The thread open in the panel, with the window focused, is read as it
    /// opens and as its replies arrive, like an open conversation.
    pub(crate) fn read_open_thread(&mut self) {
        if !self.focused {
            return;
        }
        let Some(t) = self.thread.as_ref().filter(|t| !t.inline) else { return };
        let (chan, ts) = (t.chan.clone(), t.ts.clone());
        let followed = t.msgs.first().is_some_and(|m| m.ts == ts && m.subscribed);
        let loaded = t.msgs.iter().filter(|m| m.ts != ts).map(|m| m.ts.clone()).max().unwrap_or_default();
        let listed = self.threads.list.iter().find(|s| s.chan == chan && s.root.ts == ts).map(|s| s.latest.clone()).unwrap_or_default();
        self.read_thread(&chan, &ts, &loaded.max(listed), followed);
    }

    /// A message from the websocket: a reply in a thread you follow brings the list up to date.
    pub(crate) fn thread_message(&mut self, chan: &str, thread: Option<&str>) {
        let Some(ts) = thread else { return };
        let listed = self.threads.list.iter().any(|s| s.chan == chan && s.root.ts == ts);
        let followed = self.messages.get(chan).and_then(|l| l.iter().find(|m| m.ts == ts)).is_some_and(|m| m.subscribed);
        if listed || followed {
            self.threads_soon();
        }
    }

    /// The Threads row: the list in the main pane, read again.
    pub(crate) fn open_threads(&mut self) {
        self.drop_edit();
        self.selection = None;
        self.current = None;
        self.thread = None;
        self.profile = None;
        self.edit.forget_thread();
        self.filter.clear();
        self.threads.open = true;
        // Cards read last time take their place among the read ones.
        self.threads.kept.clear();
        let t = &mut self.threads;
        order(&mut t.list, &t.kept);
        (self.threads.sel, self.threads.top) = (0, 0);
        self.focus = Focus::Input;
        self.fetch_threads();
    }

    /// "N more replies": all of the thread's replies, in the card itself.
    fn expand(&mut self, chan: String, ts: String) {
        if !self.threads.expanding.insert(key(&chan, &ts)) {
            return;
        }
        self.spawn(move |s| {
            let res = s.replies(&chan, &ts);
            Event::Threads(Ev::Expanded { chan, ts, res })
        });
    }

    /// Expands a card that leaves replies out; true when it did.
    fn expand_card(&mut self, i: usize) -> bool {
        let Some(s) = self.threads.list.get(i) else { return false };
        let k = key(&s.chan, &s.root.ts);
        if self.threads.expanded.contains_key(&k) || (s.root.reply_count as usize) <= s.replies.len() {
            return false;
        }
        let (chan, ts) = (s.chan.clone(), s.root.ts.clone());
        self.threads.sel = i;
        self.expand(chan, ts);
        true
    }

    /// Opens a card's thread in the panel, which reads it.
    fn open_card(&mut self, i: usize) {
        let Some(s) = self.threads.list.get(i).cloned() else { return };
        self.threads.sel = i;
        self.threads.reveal = true;
        self.open_thread_in(s.chan, s.root.ts.clone(), Some(s.root), false);
    }

    /// Starts a reply in a card's own box.
    fn reply_inline(&mut self, i: usize) {
        let Some(s) = self.threads.list.get(i) else { return };
        self.threads.sel = i;
        self.threads.reveal = true;
        if self.thread.as_ref().is_some_and(|t| t.inline && t.chan == s.chan && t.ts == s.root.ts) {
            self.focus = Focus::Thread;
            return;
        }
        let (chan, ts) = (s.chan.clone(), s.root.ts.clone());
        self.open_thread_in(chan, ts, None, true);
    }

    /// The next or previous card; a reply box being typed in follows, while still empty.
    fn threads_step(&mut self, delta: isize) {
        let n = self.threads.list.len();
        if n == 0 {
            return;
        }
        let i = (self.threads.sel as isize + delta).clamp(0, n as isize - 1) as usize;
        match self.thread.as_ref().filter(|t| t.inline && self.focus == Focus::Thread) {
            Some(t) if !t.editor.is_empty() => {}
            Some(_) => self.reply_inline(i),
            None => {
                self.threads.sel = i;
                self.threads.reveal = true;
            }
        }
    }

    fn scroll_threads(&mut self, delta: isize) {
        let t = &mut self.threads;
        t.top = t.top.saturating_add_signed(delta).min(t.max_top);
    }

    /// Keys for the view: moving between cards, opening one, replying in it.
    /// True when handled.
    pub(crate) fn threads_key(&mut self, k: KeyEvent, ctrl: bool) -> bool {
        if !self.threads.open {
            return false;
        }
        let page = self.threads.page.max(2) as isize - 1;
        let inline = self.thread.as_ref().is_some_and(|t| t.inline);
        match self.focus {
            // Typing a reply in a card: the arrows leave the box at its edges.
            Focus::Thread if inline => {
                let empty = self.thread.as_ref().is_some_and(|t| t.editor.is_empty());
                match k.code {
                    KeyCode::PageUp => self.scroll_threads(-page),
                    KeyCode::PageDown => self.scroll_threads(page),
                    KeyCode::Up | KeyCode::Down if k.modifiers.is_empty() => {
                        let delta = if k.code == KeyCode::Up { -1 } else { 1 };
                        let moved = self.thread.as_mut().is_some_and(|t| t.editor.vertical(t.input_width, delta));
                        if !moved {
                            self.threads_step(delta);
                        }
                    }
                    KeyCode::Enter if k.modifiers.is_empty() && empty => self.open_card(self.threads.sel),
                    KeyCode::Esc => self.focus = Focus::Sidebar,
                    _ => return false,
                }
                true
            }
            Focus::Input if !ctrl && k.code != KeyCode::Tab => {
                match k.code {
                    KeyCode::Up | KeyCode::Char('k') => self.threads_step(-1),
                    KeyCode::Down | KeyCode::Char('j') => self.threads_step(1),
                    KeyCode::Home | KeyCode::Char('g') => self.threads_step(isize::MIN / 2),
                    KeyCode::End | KeyCode::Char('G') => self.threads_step(isize::MAX / 2),
                    KeyCode::PageUp => self.scroll_threads(-page),
                    KeyCode::PageDown => self.scroll_threads(page),
                    KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => self.open_card(self.threads.sel),
                    KeyCode::Char('r') | KeyCode::Char('i') => self.reply_inline(self.threads.sel),
                    KeyCode::Char('e') | KeyCode::Char(' ') => {
                        self.expand_card(self.threads.sel);
                    }
                    KeyCode::Esc => self.focus = Focus::Sidebar,
                    _ => {}
                }
                true
            }
            _ => false,
        }
    }

    /// Clicks and the wheel on the view. True when handled.
    pub(crate) fn threads_mouse(&mut self, m: MouseEvent, at: Position) -> bool {
        if !self.threads.open || !self.areas.threads.list.contains(at) {
            return false;
        }
        match m.kind {
            MouseEventKind::ScrollUp => self.scroll_threads(-3),
            MouseEventKind::ScrollDown => self.scroll_threads(3),
            MouseEventKind::Down(MouseButton::Left) => {
                let a = &self.areas.threads;
                if a.pill_close.contains(at) {
                    let above = a.above.clone();
                    self.threads.dismissed.extend(above);
                } else if a.pill.contains(at) {
                    // Those threads were pointed at: the pill doesn't come back for them.
                    let above = a.above.clone();
                    self.threads.dismissed.extend(above);
                    // The topmost unread card (unread ones come first), lit for a
                    // moment and read, now that it is in front of you.
                    if let Some(s) = self.threads.list.iter().find(|s| s.unread()).cloned() {
                        let k = key(&s.chan, &s.root.ts);
                        self.threads.sel = self.threads.list.iter().position(|x| key(&x.chan, &x.root.ts) == k).unwrap_or(0);
                        self.threads.reveal = true;
                        self.threads.flash = Some((k, Instant::now()));
                        self.focus = Focus::Input;
                        self.read_thread(&s.chan, &s.root.ts, &s.latest, true);
                    }
                } else if a.broadcast.contains(at) {
                    self.thread.iter_mut().for_each(|t| t.broadcast = !t.broadcast);
                } else if let Some(&(_, i)) = a.more.iter().find(|(r, _)| r.contains(at)) {
                    self.focus = Focus::Input;
                    self.expand_card(i);
                } else if let Some(&(_, i)) = a.replies.iter().find(|(r, _)| r.contains(at)) {
                    self.reply_inline(i);
                } else if let Some(&(_, i)) = a.cards.iter().find(|(r, _)| r.contains(at)) {
                    self.focus = Focus::Input;
                    self.open_card(i);
                } else {
                    self.focus = Focus::Input;
                }
            }
            _ => return false,
        }
        true
    }

    /// The snapshot's `threads`, and how many of its threads ask for
    /// attention: those that mention you, or in a DM.
    pub(crate) fn threads_json(&self) -> (Value, u64) {
        let me = self.me.as_ref().map(|m| m.user_id.clone()).unwrap_or_default();
        let mut attention = 0;
        let list: Vec<Value> = self
            .threads
            .list
            .iter()
            .filter(|s| s.unread())
            .map(|s| {
                let unread: Vec<&Message> = s.unread_replies().collect();
                let mention = unread.iter().any(|m| crate::events::mentions(m, &me));
                let direct = self.convs.iter().find(|c| c.id == s.chan).is_some_and(|c| matches!(c.kind, Kind::Direct | Kind::Group));
                attention += (mention || direct) as u64;
                let messages: Vec<Value> = unread[unread.len().saturating_sub(PREVIEW)..].iter().map(|m| self.message_json(m)).collect();
                json!({
                    "conversation": self.conv_json(&s.chan),
                    "ts": s.root.ts,
                    "root": self.message_json(&s.root),
                    "latest": s.latest,
                    "unread": unread.len().max(1),
                    "new": s.new_replies(),
                    "mention": mention,
                    "messages": messages,
                })
            })
            .collect();
        let unread = self.threads.unread || !list.is_empty();
        (json!({ "unread": unread, "new": self.threads.new, "mentions": self.threads.mentions, "list": list }), attention)
    }

    /// Threads that left the snapshot's unread ones since the last: `thread.read`.
    pub(crate) fn threads_read(&self, old: &Value, new: &Value) {
        let keys = |v: &Value| -> Vec<(String, String)> {
            v["threads"]["list"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|t| Some((t["conversation"]["id"].as_str()?.to_string(), t["ts"].as_str()?.to_string())))
                .collect()
        };
        let now = keys(new);
        for (chan, ts) in keys(old).into_iter().filter(|k| !now.contains(k)) {
            self.emit("thread.read", json!({ "conversation": self.conv_json(&chan), "ts": ts }));
        }
    }
}

/// When a message was sent, as Slack's Threads view says it: "2 minutes ago",
/// "Wednesday at 9:10", "Aug 14th at 11:22".
fn ago(ts: &str) -> Option<String> {
    let secs: i64 = ts.split('.').next()?.parse().ok()?;
    let t = Local.timestamp_opt(secs, 0).single()?;
    let now = Local::now();
    let mins = (now.timestamp() - secs) / 60;
    let clock = i18n::clock(&t);
    let (today, d) = (now.date_naive(), t.date_naive());
    let (day, month, year) = (t.day(), i18n::month(&t), t.year());
    let short: String = month.chars().take(3).collect();
    let nth = i18n::ordinal(day);
    Some(match mins {
        m if m < 1 => tr("just now", "agora mesmo").to_string(),
        1 => tr("1 minute ago", "há 1 minuto").to_string(),
        m if m < 60 => trf!("{m} minutes ago", "há {m} minutos"),
        _ if d == today => trf!("Today at {clock}", "Hoje às {clock}"),
        _ if today.pred_opt() == Some(d) => trf!("Yesterday at {clock}", "Ontem às {clock}"),
        _ if (today - d).num_days() < 7 => {
            let wd = i18n::weekday(&t);
            trf!("{wd} at {clock}", "{wd} às {clock}")
        }
        _ if year == today.year() => trf!("{short} {nth} at {clock}", "{day} de {month} às {clock}"),
        _ => trf!("{short} {nth}, {year} at {clock}", "{day} de {month} de {year} às {clock}"),
    })
}

/// Who is in a thread, as the card says it under the channel: "Ana and you",
/// "Ana, Bruno, and 16 others".
fn people(app: &App, s: &Sub) -> String {
    let mut ids: Vec<&str> = Vec::new();
    for u in s.root.user.iter().chain(&s.root.reply_users) {
        if !ids.contains(&u.as_str()) {
            ids.push(u);
        }
    }
    let you = ids.iter().any(|u| app.is_me(u));
    let others: Vec<String> =
        ids.iter().filter(|u| !app.is_me(u)).map(|u| app.users.names.get(*u).cloned().unwrap_or_else(|| u.to_string())).collect();
    // Slack names a few repliers and counts the rest.
    let unnamed = s.people.saturating_sub(s.root.reply_users.len());
    let n = others.len() + unnamed;
    match (n, others.as_slice()) {
        (0, _) => tr("You", "Você").to_string(),
        (1, [a]) if you => trf!("{a} and you", "{a} e você"),
        (1, [a]) => a.clone(),
        (2, [a, b]) if you => trf!("{a}, {b}, and you", "{a}, {b} e você"),
        (2, [a, b]) => trf!("{a} and {b}", "{a} e {b}"),
        (_, [a, b, ..]) => {
            let rest = n - 2 + you as usize;
            trf!("{a}, {b}, and {rest} others", "{a}, {b} e mais {rest}")
        }
        (_, [a]) => {
            let rest = n - 1 + you as usize;
            trf!("{a} and {rest} others", "{a} e mais {rest}")
        }
        _ => trf!("{n} people", "{n} pessoas"),
    }
}

/// The card's title: the conversation as the sidebar shows it.
fn title(app: &App, chan: &str, bold: bool) -> Line<'static> {
    let style = if bold { Style::new().fg(Color::White).bold() } else { Style::new().bold() };
    let Some(c) = app.convs.iter().find(|c| c.id == chan) else {
        let name = app.chans.get(chan).or_else(|| app.others.get(chan)).cloned().unwrap_or_else(|| chan.to_string());
        return Line::from(vec![Span::styled("# ", dim()), Span::styled(name, style)]);
    };
    let icon = match c.kind {
        Kind::Channel => Span::styled("# ", dim()),
        Kind::Private => Span::styled(format!("{LOCK} "), dim()),
        Kind::Direct if c.user.as_ref().is_some_and(|u| app.users.bots.contains(u)) => Span::raw(""),
        Kind::Direct => {
            let dot = presence_span(app, presence_of(app, c.user.as_deref().unwrap_or("")));
            Span::styled(format!("{} ", dot.content), dot.style)
        }
        Kind::Group => Span::styled("◉ ", dim()),
    };
    Line::from(vec![icon, Span::styled(app.label(c), style)])
}

/// A card's messages: the root, how many replies it leaves out, the replies
/// already read and, under "New", the unread ones. Through the conversation's
/// rendering, but without its day lines and links into threads.
/// Also the line of "N more replies", when there is one. `full` is every
/// reply, once the card was expanded.
/// `read_to` is where the "New" line goes; also the line it is on, when there is one.
fn card_lines(app: &App, s: &Sub, read_to: &str, full: Option<&[Message]>, loading: bool, width: usize, font: Option<(u16, u16)>) -> (Rendered, Option<usize>, Option<usize>) {
    let names = app.names();
    let plain = |m: &Message| {
        let mut m = m.clone();
        m.reply_count = 0;
        if m.subtype.as_deref() == Some("thread_broadcast") {
            m.subtype = None;
        }
        m
    };
    // `packed_lines` opens with a blank line and the day's: not in a card.
    let mut r = packed_lines(&[plain(&s.root)], &names, app, width, font);
    r.drop_front(3);
    let gutter = font.map_or(2, |(fw, fh)| ((2.0 * fh as f64 / fw as f64).round() as usize).clamp(3, 6) + 1);
    let replies = full.unwrap_or(&s.replies);
    let hidden = (s.root.reply_count as usize).saturating_sub(replies.len());
    let mut more_at = None;
    if hidden > 0 && full.is_none() {
        // A link, like Slack's: a click (or `e`) shows them here.
        let more = match hidden {
            _ if loading => tr("loading…", "carregando…").to_string(),
            1 => tr("Show 1 more reply", "Mostrar mais 1 resposta").to_string(),
            _ => trf!("Show {hidden} more replies", "Mostrar mais {hidden} respostas"),
        };
        r.lines.push(Line::from(""));
        more_at = Some(r.lines.len());
        r.lines.push(Line::from(vec![Span::raw(" ".repeat(gutter)), Span::styled(more, Style::new().fg(Color::Cyan).bold())]));
    }
    let (read, unread): (Vec<Message>, Vec<Message>) = replies.iter().map(plain).partition(|m| m.ts.as_str() <= read_to);
    if !read.is_empty() {
        let mut more = packed_lines(&read, &names, app, width, font);
        more.drop_front(2);
        r.append(more);
    }
    let mut new_at = None;
    if !unread.is_empty() {
        // Slack's "New" line before the first unread reply.
        let label = tr(" New", " Novas");
        r.lines.push(Line::from(""));
        new_at = Some(r.lines.len());
        r.lines.push(Line::from(vec![
            Span::styled("─".repeat(width.saturating_sub(label.width())), Style::new().fg(Color::Red)),
            Span::styled(label, Style::new().fg(Color::Red).bold()),
        ]));
        let mut more = packed_lines(&unread, &names, app, width, font);
        more.drop_front(3);
        r.append(more);
    }
    r.retime(ago);
    r.inert();
    (r, more_at, new_at)
}

/// The sidebar's top row: bold with unread replies, a blue badge with how
/// many, and the red one of those that mention you.
pub(crate) fn sidebar_row<'a>(app: &App, width: usize) -> ListItem<'a> {
    let t = &app.threads;
    let label = tr("Threads", "Conversas");
    let style = if t.open {
        Style::new().fg(Color::Cyan).bold()
    } else if t.unread {
        Style::new().fg(Color::White).bold()
    } else {
        Style::new().fg(Color::Gray)
    };
    let mark = t.unread && !t.open;
    let mut spans = vec![crate::ui::unread_lead(app, mark), Span::styled(ICON, if t.unread || t.open { style } else { dim() }), Span::raw(" "), Span::styled(label, style)];
    // New replies in Slack's blue, mentions in the red of the other rows.
    let badges: Vec<(String, Color)> = [(t.new, Color::Rgb(0x12, 0x64, 0xA3)), (t.mentions, Color::Red)]
        .into_iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, c)| (format!(" {n} "), c))
        .collect();
    if !badges.is_empty() {
        let used = 5 + label.width();
        let w: usize = badges.iter().map(|(b, _)| b.width() + 2).sum::<usize>() + badges.len() - 1;
        spans.push(Span::raw(" ".repeat(width.saturating_sub(used + w))));
        for (n, (b, c)) in badges.into_iter().enumerate() {
            if n > 0 {
                spans.push(Span::raw(" "));
            }
            spans.push(Span::styled(crate::blocks::CAP_L, Style::new().fg(c)));
            spans.push(Span::styled(b, Style::new().fg(Color::White).bg(c).bold()));
            spans.push(Span::styled(crate::blocks::CAP_R, Style::new().fg(c)));
        }
    }
    crate::ui::unread_fill(app, ListItem::new(Line::from(spans)), mark)
}

/// A reply box being typed in, as laid out in the list.
struct Input {
    /// Its first text row, in list lines.
    y: usize,
    rows: usize,
    /// "Also send to …", on the line under the box.
    toggle: Line<'static>,
}

/// The Threads view, in place of the conversation: a title, then a card per
/// thread, unread first; "You're up to date" before the rest.
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
    a.threads = Areas::default();

    let [head, body] = Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).areas(area);
    f.render_widget(Block::new().borders(Borders::BOTTOM).border_style(dim()), head);
    let heading = Line::from(vec![Span::raw(" "), Span::styled(ICON, dim()), Span::raw(" "), Span::styled(tr("Threads", "Conversas"), Style::new().bold())]);
    f.render_widget(Paragraph::new(heading), Rect::new(head.x, head.y, head.width, 1));
    let body = body.inner(Margin { horizontal: 1, vertical: 0 });
    app.areas.threads.list = body;
    let width = body.width as usize;
    if width < 24 || body.height < 3 {
        return;
    }
    let notice = |text: &str| Paragraph::new(vec![Line::from(""), Line::from(text.to_string()).style(dim())]);
    if app.threads.list.is_empty() {
        let text = match (&app.threads.error, app.threads.loaded) {
            (Some(e), _) => e.clone(),
            (None, false) => tr("loading…", "carregando…").to_string(),
            (None, true) => tr("Threads you start, reply to or follow show up here.", "As conversas que você começa, responde ou segue aparecem aqui.").to_string(),
        };
        f.render_widget(notice(&text), body);
        return;
    }

    // The whole list as lines: borders and titles here, the messages drawn
    // over their room (`pieces`), and the reply box being typed in too.
    let font = app.picker.as_ref().map(|p| p.font_size()).map(|s| (s.width, s.height));
    let inner_w = width.saturating_sub(4);
    let box_w = inner_w.saturating_sub(4);
    let focus_here = matches!(app.focus, Focus::Input | Focus::Thread);
    let typing = app.thread.as_ref().filter(|t| t.inline).map(|t| (t.chan.clone(), t.ts.clone(), t.editor.rows(box_w).len(), t.broadcast));
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut pieces: Vec<(usize, Rendered)> = Vec::new();
    let mut cards: Vec<(usize, usize, usize, usize)> = Vec::new();
    // "N more replies" lines, in list lines.
    let mut more: Vec<(usize, usize)> = Vec::new();
    let mut input: Option<Input> = None;
    // Unread cards: where their new replies start and where the card ends, in list lines.
    let mut news: Vec<(usize, usize, usize)> = Vec::new();
    let mut caught_up = false;
    for (i, s) in app.threads.list.iter().enumerate() {
        if !app.threads.fresh(s) && !caught_up {
            caught_up = true;
            // Slack's "You're up to date" between the unread and the rest.
            let label = tr(" You're up to date ", " Você está em dia ");
            let side = width.saturating_sub(label.width()) / 2;
            lines.push(Line::from(vec![
                Span::styled("─".repeat(side), dim()),
                Span::styled(label, dim().bold()),
                Span::styled("─".repeat(side), dim()),
            ]));
            lines.push(Line::from(""));
        }
        let selected = i == app.threads.sel;
        let lit = app.threads.flash.as_ref().filter(|(k, at)| *k == key(&s.chan, &s.root.ts) && at.elapsed() < FLASH).map(|(_, at)| *at + FLASH);
        if let Some(until) = lit {
            // Back for the draw that turns it off.
            app.next_frame = Some(app.next_frame.map_or(until, |t| t.min(until)));
        }
        let border = match lit {
            Some(_) => Style::new().fg(Color::Yellow),
            None if selected && focus_here => Style::new().fg(Color::Cyan),
            None => dim(),
        };
        let side = |text: Line<'static>| {
            let mut spans = vec![Span::styled("│ ", border)];
            let w = text.width();
            spans.extend(text.spans);
            spans.push(Span::raw(" ".repeat(inner_w.saturating_sub(w))));
            spans.push(Span::styled(" │", border));
            Line::from(spans)
        };
        let y0 = lines.len();
        let mut head = title(app, &s.chan, s.unread());
        let head_w = head.width();
        if head_w > width {
            head = Line::from(truncate(&head.spans.iter().map(|s| s.content.as_ref()).collect::<String>(), width));
        }
        lines.push(head);
        lines.push(Line::from(truncate(&people(app, s), width)).style(dim()));
        lines.push(Line::from(Span::styled(format!("╭{}╮", "─".repeat(width.saturating_sub(2))), border)));
        let k = key(&s.chan, &s.root.ts);
        let full = app.threads.expanded.get(&k).map(Vec::as_slice);
        let read_to = app.threads.kept.get(&k).unwrap_or(&s.last_read);
        let (r, more_at, new_at) = card_lines(app, s, read_to, full, app.threads.expanding.contains(&k), inner_w, font);
        more.extend(more_at.map(|m| (lines.len() + m, i)));
        let new_y = new_at.map(|m| lines.len() + m);
        pieces.push((lines.len(), r));
        let n = pieces.last().map_or(0, |p| p.1.lines.len());
        lines.extend((0..n).map(|_| side(Line::from(""))));
        lines.push(side(Line::from("")));
        // The reply box: live on the card being replied to, a placeholder on the others.
        let replying = typing.as_ref().filter(|t| t.0 == s.chan && t.1 == s.root.ts);
        let reply_y = lines.len();
        let rows = replying.map_or(1, |t| t.2.clamp(1, MAX_INPUT_ROWS));
        let inner_border = if replying.is_some() && app.focus == Focus::Thread { Style::new().fg(Color::Cyan) } else { dim() };
        // The box takes the card's inner width: its borders, a space each side, the text.
        lines.push(side(Line::from(Span::styled(format!("╭{}╮", "─".repeat(box_w + 2)), inner_border))));
        for row in 0..rows {
            let mut text = vec![Span::styled("│ ", inner_border)];
            let mut used = 0;
            if row == 0 && replying.is_none() {
                let hint = tr("Reply…", "Responder…");
                text.push(Span::styled(hint, dim()));
                used += hint.width();
                // The key that starts it, on the selected card.
                if selected && app.focus == Focus::Input {
                    text.push(Span::raw(" ".repeat(box_w.saturating_sub(used + 1))));
                    text.push(Span::styled("r", dim()));
                    used = box_w;
                }
            }
            text.push(Span::raw(" ".repeat(box_w.saturating_sub(used))));
            text.push(Span::styled(" │", inner_border));
            lines.push(side(Line::from(text)));
        }
        lines.push(side(Line::from(Span::styled(format!("╰{}╯", "─".repeat(box_w + 2)), inner_border))));
        if let Some(t) = replying {
            // "Also send to #canal", or as a DM: Ctrl+B, as in the thread panel.
            let to = match app.convs.iter().find(|c| c.id == s.chan) {
                Some(c) if matches!(c.kind, Kind::Direct | Kind::Group) => tr(" Also send as direct message", " Também enviar como mensagem direta").to_string(),
                Some(c) => trf!(" Also send to #{}", " Também enviar para #{}", c.name),
                None => tr(" Also send to the channel", " Também enviar para o canal").to_string(),
            };
            let (icon, style) = if t.3 { (BOX_ON, Style::new().fg(Color::Cyan)) } else { (BOX_OFF, dim()) };
            let toggle = Line::from(vec![Span::styled(icon, style), Span::raw(truncate(&to, inner_w.saturating_sub(6))), Span::styled("  ^B", dim())]);
            input = Some(Input { y: reply_y + 1, rows, toggle: toggle.clone() });
            lines.push(side(toggle));
        }
        lines.push(Line::from(Span::styled(format!("╰{}╯", "─".repeat(width.saturating_sub(2))), border)));
        cards.push((i, y0, lines.len(), reply_y));
        if s.unread() {
            news.push((i, new_y.unwrap_or(y0), lines.len()));
        }
        lines.push(Line::from(""));
    }

    // Scrolled so the selected card shows, when it just changed.
    let page = body.height as usize;
    let t = &mut app.threads;
    t.page = page;
    t.max_top = lines.len().saturating_sub(page);
    if std::mem::take(&mut t.reveal) {
        if let Some(&(_, y0, y1, _)) = cards.iter().find(|c| c.0 == t.sel) {
            if y0 < t.top {
                t.top = y0;
            } else if y1 > t.top + page {
                t.top = y0.min(y1 - page);
            }
        }
    }
    t.top = t.top.min(t.max_top);
    let top = t.top;
    let end = (top + page).min(lines.len());
    f.render_widget(Paragraph::new(lines[top..end].to_vec()), body);
    let row = |y: usize| body.y + (y - top) as u16;
    for (y, r) in pieces {
        let (a, b) = (y.max(top), (y + r.lines.len()).min(end));
        if a < b {
            let scroll = y + r.lines.len() - b;
            place(f, app, Rect::new(body.x + 2, row(a), inner_w as u16, (b - a) as u16), r, scroll, false);
        }
    }
    // Unread cards with their new replies on screen are read next tick (`read_in_view`).
    app.threads.in_view = news
        .iter()
        .filter(|&&(_, a, b)| a < end && b > top)
        .filter_map(|&(i, _, _)| app.threads.list.get(i))
        .map(|s| (s.chan.clone(), s.root.ts.clone(), s.latest.clone()))
        .collect();
    for &(y, i) in more.iter().filter(|(y, _)| (top..end).contains(y)) {
        app.areas.threads.more.push((Rect::new(body.x + 2, row(y), inner_w as u16, 1), i));
    }
    for &(i, y0, y1, reply_y) in &cards {
        let (a, b) = (y0.max(top), y1.min(end));
        if a < b {
            app.areas.threads.cards.push((Rect::new(body.x, row(a), body.width, (b - a) as u16), i));
            // Seen on screen: an unread card no longer counts for the pill.
            if let Some(s) = app.threads.list.get(i).filter(|s| s.unread()) {
                let k = format!("{}:{}", key(&s.chan, &s.root.ts), s.latest);
                app.threads.dismissed.insert(k);
            }
        }
        let (a, b) = (reply_y.max(top), (reply_y + 3).min(end));
        if a < b {
            app.areas.threads.replies.push((Rect::new(body.x + 2, row(a), inner_w as u16, (b - a) as u16), i));
        }
    }
    // Like Slack: unread cards scrolled out above get a pill over the top.
    let above: Vec<String> = cards
        .iter()
        .filter(|c| c.2 <= top)
        .filter_map(|c| app.threads.list.get(c.0).filter(|s| s.unread()))
        .map(|s| format!("{}:{}", key(&s.chan, &s.root.ts), s.latest))
        .collect();
    if above.iter().any(|k| !app.threads.dismissed.contains(k)) {
        unread_pill(f, app, body, above.len());
        app.areas.threads.above = above;
    }
    // The reply being typed, when its box shows whole.
    if let Some(inp) = input.filter(|i| i.y >= top && i.y + i.rows < end) {
        let rect = Rect::new(body.x + 4, row(inp.y), box_w as u16, inp.rows as u16);
        app.areas.thread_input = rect;
        let focused = app.focus == Focus::Thread;
        let empty = app.thread.as_ref().is_none_or(|t| t.editor.text.is_empty());
        if empty {
            f.render_widget(Paragraph::new(tr("Reply…", "Responder…")).style(dim()), rect);
            if focused {
                f.set_cursor_position((rect.x, rect.y));
            }
        } else {
            crate::editor::draw(f, app, true, rect, focused);
        }
        if let Some(t) = app.thread.as_mut() {
            t.input_width = box_w;
        }
        let toggle_y = inp.y + inp.rows + 1;
        if toggle_y < end {
            app.areas.threads.broadcast = Rect::new(body.x + 2, row(toggle_y), inp.toggle.width() as u16, 1).intersection(body);
        }
    }
}

/// "↑ N unread threads", a blue pill over the top of the list, Slack's
/// accent; a click goes to the topmost one, its ✕ closes it.
fn unread_pill(f: &mut Frame, app: &mut App, area: Rect, n: usize) {
    let text = if n == 1 { tr(" ↑ 1 unread thread ", " ↑ 1 conversa não lida ").to_string() } else { trf!(" ↑ {n} unread threads ", " ↑ {n} conversas não lidas ") };
    let (fg, bg) = (Color::White, Color::Rgb(0x12, 0x64, 0xA3));
    let w = text.width() as u16 + 4;
    if area.width < w || area.height < 2 {
        return;
    }
    let rect = Rect::new(area.x + (area.width - w) / 2, area.y, w, 1);
    let pill = Line::from(vec![
        Span::styled(crate::blocks::CAP_L, Style::new().fg(bg)),
        Span::styled(text, Style::new().fg(fg).bg(bg).bold()),
        Span::styled("✕ ", Style::new().fg(fg).bg(bg)),
        Span::styled(crate::blocks::CAP_R, Style::new().fg(bg)),
    ]);
    f.render_widget(Clear, rect);
    f.render_widget(Paragraph::new(pill), rect);
    let close = Rect::new(rect.right() - 3, rect.y, 3, 1);
    app.areas.threads.pill = Rect::new(rect.x, rect.y, w - 3, 1);
    app.areas.threads.pill_close = close;
}
