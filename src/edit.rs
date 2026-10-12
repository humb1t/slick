//! A message picked with ↑ or a right click, and what can be done to it:
//! Slack's message menu, editing in the message box, pins, saving for later,
//! reminders, forwarding and deleting.

use crate::app::{Action, App, Event, Focus, Menu};
use crate::editor::Editor;
use crate::slack::{escape, parse_message, Edited, Kind, Message, Res, Slack};
use crate::text::{self, Names};
use crate::ui::{dim, truncate};
use chrono::{Datelike, Local, TimeZone};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use ratatui::Frame;
use serde_json::Value;
use std::collections::HashSet;
use std::ops::Range;

// Nerd Font glyphs, one cell each, for the menu.
const SMILE: &str = "\u{f118}";
const COMMENTS: &str = "\u{f086}";
const SHARE: &str = "\u{f064}";
const BOOKMARK: &str = "\u{f097}";
const BOOKMARK_ON: &str = "\u{f02e}";
const PENCIL: &str = "\u{f040}";
const ENVELOPE: &str = "\u{f0e0}";
const CLOCK: &str = "\u{f017}";
const BELL: &str = "\u{f0f3}";
const BELL_OFF: &str = "\u{f1f6}";
const PIN: &str = "\u{f08d}";
const LINK: &str = "\u{f0c1}";
const COPY: &str = "\u{f0c5}";
const TRASH: &str = "\u{f1f8}";
/// Conversations the forward dialog lists.
const ROWS: usize = 8;

/// A picked message: in the conversation, or in the thread panel.
#[derive(Clone)]
pub struct Sel {
    pub thread: bool,
    pub ts: String,
}

/// A message being edited in its pane's box.
pub struct Editing {
    pub chan: String,
    pub ts: String,
    pub thread: bool,
    /// Mentions and channels as typed and as Slack writes them, put back on save.
    refs: Vec<(String, String)>,
    /// What the box held before, given back when the edit ends.
    draft: String,
}

/// The dialog that sends a message's link to another conversation.
pub struct Forward {
    chan: String,
    ts: String,
    query: String,
    sel: usize,
}

#[derive(Default)]
pub struct Edit {
    pub selected: Option<Sel>,
    pub editing: Option<Editing>,
    pub forward: Option<Forward>,
    /// Messages saved for later, as (channel, ts).
    pub saved: HashSet<(String, String)>,
    /// The next draw scrolls the selection into view.
    pub reveal: bool,
    /// Where the forward dialog and its rows were drawn.
    dialog: Rect,
    rows: Vec<(Rect, usize)>,
}

impl Edit {
    /// The thread panel closed or changed: what pointed into it goes.
    pub fn forget_thread(&mut self) {
        if self.selected.as_ref().is_some_and(|s| s.thread) {
            self.selected = None;
        }
        if self.editing.as_ref().is_some_and(|e| e.thread) {
            self.editing = None;
        }
    }
}

#[derive(Clone)]
pub enum Act {
    React,
    Thread,
    Forward,
    Save(bool),
    Edit,
    Unread,
    /// Opens the list of times.
    Remind,
    /// A unix time.
    RemindAt(i64),
    Subscribe(bool),
    Pin(bool),
    CopyLink,
    CopyText,
    /// Asks first.
    Delete,
    DeleteNow,
    Cancel,
}

/// A message-menu entry: what to do, and to which message.
#[derive(Clone)]
pub struct Item {
    pub chan: String,
    pub ts: String,
    pub thread: bool,
    pub act: Act,
}

impl Item {
    pub fn danger(&self) -> bool {
        matches!(self.act, Act::Delete | Act::DeleteNow)
    }
}

pub enum Ev {
    Saved(Res<HashSet<(String, String)>>),
    Updated { chan: String, old: Message, res: Res<Message> },
    Deleted { chan: String, old: Message, res: Res<()> },
    Pinned { chan: String, ts: String, on: bool, res: Res<()> },
    SavedOne { chan: String, ts: String, on: bool, res: Res<()> },
    Subscribed { chan: String, ts: String, on: bool, res: Res<()> },
    Unread(Res<()>),
    /// Nothing to undo: the status says how it went.
    Done { ok: String, res: Res<()> },
}

impl Slack {
    /// New text for a message. Without `blocks=[]` Slack keeps the old rich-text
    /// blocks, which the official clients draw instead of the text.
    fn update(&self, chan: &str, ts: &str, text: &str) -> Res<Message> {
        let v = self.call("chat.update", &[("channel", chan), ("ts", ts), ("text", text), ("blocks", "[]")])?;
        Ok(parse_message(&v["message"]))
    }

    /// Slack's "Salvos" still to do, as (channel, ts).
    pub fn saved(&self) -> Res<HashSet<(String, String)>> {
        let items = self.paginate("saved.list", &[("limit", "100")], "saved_items")?;
        Ok(items
            .iter()
            .filter(|i| i["item_type"] == "message" && i["state"] == "in_progress")
            .filter_map(|i| Some((i["item_id"].as_str()?.to_string(), i["ts"].as_str()?.to_string())))
            .collect())
    }
}

/// Slack's text back to what was typed: `<@U1>` → `@Ana`, `<#C1|geral>` →
/// `#geral`, `<url|label>` → `url`, entities decoded. Also the mentions and
/// channels as (typed, Slack's), to put back on save.
pub fn typed(s: &str, n: &Names) -> (String, Vec<(String, String)>) {
    let mut out = String::new();
    let mut refs = Vec::new();
    let mut rest = s;
    while let Some(i) = rest.find('<') {
        let Some(j) = rest[i..].find('>') else { break };
        out.push_str(&text::mrkdwn(&rest[..i], n));
        let inner = &rest[i + 1..i + j];
        let target = inner.split('|').next().unwrap_or(inner);
        if target.starts_with(['@', '#', '!']) {
            let shown = text::link(inner, n);
            refs.push((shown.clone(), format!("<{inner}>")));
            out.push_str(&shown);
        } else {
            out.push_str(&text::link(target, n));
        }
        rest = &rest[i + j + 1..];
    }
    out.push_str(&text::mrkdwn(rest, n));
    (out, refs)
}

/// The typed text as Slack wants it: escaped like a new message, with the
/// mentions and channels it had put back. Longest first, so `@Ana Paula`
/// is not taken for `@Ana`.
fn slack_text(typed: &str, refs: &[(String, String)]) -> String {
    let mut refs = refs.to_vec();
    refs.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then(a.cmp(b)));
    refs.dedup();
    let mut out = escape(typed);
    for (shown, raw) in refs {
        out = out.replace(&escape(&shown), &raw);
    }
    out
}

/// The ts right before `ts`, for "marcar como não lida".
fn before(ts: &str) -> String {
    let (secs, frac) = ts.split_once('.').unwrap_or((ts, "0"));
    let (secs, frac) = (secs.parse::<i64>().unwrap_or(0), frac.parse::<i64>().unwrap_or(0));
    let (secs, frac) = if frac > 0 { (secs, frac - 1) } else { (secs - 1, 999_999) };
    format!("{secs}.{frac:06}")
}

/// A menu row: icon, label and the shortcut, which the menu puts on the right.
fn entry(icon: &str, label: &str, key: &str) -> String {
    format!("{icon}  {label}\t{key}")
}

/// Reminder times as Slack offers them; "amanhã" and "próxima semana" at 9h.
fn remind_times() -> Vec<(&'static str, i64)> {
    let now = Local::now();
    let at9 = |days: i64| {
        let d = now.date_naive() + chrono::Duration::days(days);
        Local.from_local_datetime(&d.and_hms_opt(9, 0, 0).unwrap_or_default()).earliest().map_or(0, |t| t.timestamp())
    };
    let monday = 7 - now.weekday().num_days_from_monday() as i64;
    let t = now.timestamp();
    vec![
        ("Em 20 minutos", t + 20 * 60),
        ("Em 1 hora", t + 3600),
        ("Em 3 horas", t + 3 * 3600),
        ("Amanhã", at9(1)),
        ("Na próxima semana", at9(monday)),
    ]
}

/// Inserts by ts, skipping one already there.
fn insert(list: &mut Vec<Message>, m: Message) {
    if let Err(pos) = list.binary_search_by(|x| x.ts.cmp(&m.ts)) {
        list.insert(pos, m);
    }
}

impl App {
    /// The conversation and the messages a pane shows.
    pub(crate) fn pane(&self, thread: bool) -> Option<(&str, &[Message])> {
        if thread {
            let t = self.thread.as_ref()?;
            Some((&t.chan, &t.msgs))
        } else {
            let c = self.current.as_deref()?;
            Some((c, self.messages.get(c)?))
        }
    }

    fn pane_editor(&mut self, thread: bool) -> Option<&mut Editor> {
        if thread {
            self.thread.as_mut().map(|t| &mut t.editor)
        } else {
            Some(&mut self.editor)
        }
    }

    /// The focused pane: false for the conversation, true for the thread.
    fn focused_pane(&self) -> Option<bool> {
        match self.focus {
            Focus::Input => Some(false),
            Focus::Thread if self.thread.is_some() => Some(true),
            _ => None,
        }
    }

    fn message(&self, chan: &str, ts: &str) -> Option<Message> {
        let in_thread = self.thread.as_ref().filter(|t| t.chan == chan).and_then(|t| t.msgs.iter().find(|m| m.ts == ts));
        in_thread.or_else(|| self.messages.get(chan)?.iter().find(|m| m.ts == ts)).cloned()
    }

    /// Every copy of a message: the conversation's and the thread panel's.
    fn each_copy(&mut self, chan: &str, ts: &str, mut f: impl FnMut(&mut Message)) {
        if let Some(m) = self.messages.get_mut(chan).and_then(|l| l.iter_mut().find(|m| m.ts == ts)) {
            f(m);
        }
        if let Some(t) = self.thread.as_mut().filter(|t| t.chan == chan) {
            if let Some(m) = t.msgs.iter_mut().find(|m| m.ts == ts) {
                f(m);
            }
        }
    }

    /// Yours and plain text: bots' and join notices can't be edited.
    pub fn is_mine(&self, m: &Message) -> bool {
        m.user.as_deref().is_some_and(|u| self.is_me(u))
            && matches!(m.subtype.as_deref(), None | Some("thread_broadcast" | "file_share"))
    }

    /// A reply's link opens its thread.
    fn permalink(&self, chan: &str, m: &Message) -> String {
        let url = self.message_url(chan, &m.ts);
        match &m.thread_ts {
            Some(t) if *t != m.ts => format!("{url}?thread_ts={t}&cid={chan}"),
            _ => url,
        }
    }

    /// Where a popup for the message goes: under the menu, or on the message.
    fn anchor(&self, it: &Item) -> (u16, u16) {
        if let Some(m) = &self.menu {
            return (m.x, m.y);
        }
        let in_pane = |r: &Rect| it.thread == self.areas.thread.contains(Position::new(r.x, r.y));
        self.areas
            .message_rows
            .iter()
            .find(|(r, ts)| *ts == it.ts && in_pane(r))
            .map(|(r, _)| (r.x + 4, r.y + 1))
            .unwrap_or((self.areas.input.x + 2, self.areas.input.y))
    }

    /// A conversation switched: the pick and the edit end.
    pub(crate) fn drop_edit(&mut self) {
        self.cancel_edit();
        self.edit.selected = None;
    }

    /// Ctrl+C with a message picked copies it; true when it did.
    pub fn copy_selected(&mut self) -> bool {
        let Some(thread) = self.focused_pane() else { return false };
        let Some(sel) = self.edit.selected.clone().filter(|s| s.thread == thread) else { return false };
        let Some(chan) = self.pane(thread).map(|(c, _)| c.to_string()) else { return false };
        self.message_action(Item { chan, ts: sel.ts, thread, act: Act::CopyText });
        true
    }

    /// Moves the pick (older is -1); past the newest it clears. With none, ↑ picks the newest.
    fn step(&mut self, thread: bool, delta: isize) -> bool {
        let Some((_, msgs)) = self.pane(thread) else { return false };
        if msgs.is_empty() {
            return false;
        }
        let at = self.edit.selected.as_ref().filter(|s| s.thread == thread).and_then(|s| msgs.iter().position(|m| m.ts == s.ts));
        let next = match at {
            None => Some(msgs.len() - 1),
            Some(i) if delta < 0 => Some(i.saturating_sub(1)),
            Some(i) => Some(i + 1).filter(|&j| j < msgs.len()),
        };
        self.edit.selected = next.map(|i| Sel { thread, ts: msgs[i].ts.clone() });
        self.edit.reveal = true;
        true
    }

    /// Keys for the picked message and for the edit; true when taken.
    pub fn edit_key(&mut self, k: KeyEvent) -> bool {
        let Some(thread) = self.focused_pane() else { return false };
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if self.edit.editing.as_ref().is_some_and(|e| e.thread == thread) {
            let newline = k.modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::ALT);
            match k.code {
                KeyCode::Esc => self.cancel_edit(),
                KeyCode::Enter if !newline => self.save_edit(),
                _ => return false,
            }
            return true;
        }
        let Some(sel) = self.edit.selected.clone().filter(|s| s.thread == thread) else {
            let empty = self.pane_editor(thread).is_some_and(|e| e.text.is_empty());
            return k.code == KeyCode::Up && empty && k.modifiers.is_empty() && self.step(thread, -1);
        };
        let Some(chan) = self.pane(thread).map(|(c, _)| c.to_string()) else {
            self.edit.selected = None;
            return false;
        };
        let saved = self.edit.saved.contains(&(chan.clone(), sel.ts.clone()));
        let act = match k.code {
            KeyCode::Up => return self.step(thread, -1),
            KeyCode::Down => return self.step(thread, 1),
            KeyCode::Esc => {
                self.edit.selected = None;
                return true;
            }
            KeyCode::PageUp | KeyCode::PageDown => return false,
            KeyCode::Delete => Act::Delete,
            KeyCode::Menu => {
                let (x, y) = self.anchor(&Item { chan: chan.clone(), ts: sel.ts.clone(), thread, act: Act::Cancel });
                self.message_menu(thread, sel.ts, x, y);
                return true;
            }
            KeyCode::Char(c) if !ctrl => match c.to_ascii_lowercase() {
                'r' => Act::React,
                't' if !thread => Act::Thread,
                'f' => Act::Forward,
                'a' => Act::Save(!saved),
                'e' => Act::Edit,
                'u' if !thread => Act::Unread,
                'l' => Act::CopyLink,
                'm' => {
                    let (x, y) = self.anchor(&Item { chan: chan.clone(), ts: sel.ts.clone(), thread, act: Act::Cancel });
                    self.message_menu(thread, sel.ts, x, y);
                    return true;
                }
                // Anything else is typing.
                _ => {
                    self.edit.selected = None;
                    return false;
                }
            },
            _ => {
                self.edit.selected = None;
                return false;
            }
        };
        self.message_action(Item { chan, ts: sel.ts, thread, act });
        true
    }

    /// Slack's right-click menu for a message, which also picks it.
    pub fn message_menu(&mut self, thread: bool, ts: String, x: u16, y: u16) {
        let Some((chan, msgs)) = self.pane(thread) else { return };
        let Some(m) = msgs.iter().find(|m| m.ts == ts).cloned() else { return };
        let chan = chan.to_string();
        let it = |act| Action::Message(Item { chan: chan.clone(), ts: ts.clone(), thread, act });
        let sep = || (String::new(), Action::Separator);
        let mine = self.is_mine(&m);
        let saved = self.edit.saved.contains(&(chan.clone(), ts.clone()));
        let pinned = m.pinned_to.contains(&chan);

        let mut items = vec![(entry(SMILE, "Adicionar reação…", "R"), it(Act::React))];
        if !thread {
            items.push((entry(COMMENTS, "Responder na conversa", "T"), it(Act::Thread)));
        }
        items.push((entry(SHARE, "Encaminhar mensagem…", "F"), it(Act::Forward)));
        items.push(if saved {
            (entry(BOOKMARK_ON, "Remover dos itens salvos", "A"), it(Act::Save(false)))
        } else {
            (entry(BOOKMARK, "Salvar para depois", "A"), it(Act::Save(true)))
        });
        if mine {
            items.extend([sep(), (entry(PENCIL, "Editar mensagem", "E"), it(Act::Edit))]);
        }
        items.push(sep());
        if !thread {
            items.push((entry(ENVELOPE, "Marcar como não lida", "U"), it(Act::Unread)));
        }
        items.push((entry(CLOCK, "Lembrar-me", "›"), it(Act::Remind)));
        if let Some(parent) = &m.thread_ts {
            let on = msgs.iter().chain(self.messages.get(&chan).into_iter().flatten()).find(|p| p.ts == *parent).is_some_and(|p| p.subscribed);
            items.push(if on {
                (entry(BELL_OFF, "Desativar notificações de respostas", ""), it(Act::Subscribe(false)))
            } else {
                (entry(BELL, "Ativar notificações de respostas", ""), it(Act::Subscribe(true)))
            });
        }
        items.extend([
            sep(),
            (entry(PIN, if pinned { "Desafixar da conversa" } else { "Fixar na conversa" }, ""), it(Act::Pin(!pinned))),
            (entry(LINK, "Copiar link", "L"), it(Act::CopyLink)),
            (entry(COPY, "Copiar mensagem", "Ctrl+C"), it(Act::CopyText)),
        ]);
        if mine {
            items.extend([sep(), (entry(TRASH, "Excluir mensagem…", "Del"), it(Act::Delete))]);
        }
        self.edit.selected = Some(Sel { thread, ts });
        self.focus = if thread { Focus::Thread } else { Focus::Input };
        self.menu = Some(Menu::new(x, y, items));
        self.dirty = true;
    }

    /// Replaces the menu with a submenu beside it (or opens one on the message).
    fn submenu(&mut self, it: &Item, items: Vec<(String, Action)>) {
        let parent = self.menu.take();
        let (x, y) = match &parent {
            Some(p) => (self.areas.menu.right(), self.areas.menu.y + p.sel as u16),
            None => self.anchor(it),
        };
        let mut sub = Menu::new(x, y, items);
        sub.parent = parent.map(Box::new);
        self.menu = Some(sub);
    }

    /// Runs a message-menu entry, from the menu or a key. False when a
    /// submenu took the menu's place, so it stays open.
    pub(crate) fn message_action(&mut self, it: Item) -> bool {
        self.dirty = true;
        let Some(m) = self.message(&it.chan, &it.ts) else { return true };
        let item = |act| Action::Message(Item { act, ..it.clone() });
        match it.act.clone() {
            Act::Cancel => {}
            Act::React => {
                let (x, y) = self.anchor(&it);
                self.open_picker(it.ts, x, y);
            }
            Act::Thread => {
                self.edit.selected = None;
                self.open_thread(m.thread_ts.unwrap_or(it.ts));
            }
            Act::Forward => {
                self.edit.selected = None;
                self.edit.forward = Some(Forward { chan: it.chan, ts: it.ts, query: String::new(), sel: 0 });
            }
            Act::Save(on) => self.set_saved(it.chan, it.ts, on),
            Act::Edit => self.start_edit(it, &m),
            Act::Unread => {
                let (chan, ts) = (it.chan, before(&it.ts));
                self.spawn(move |s| Event::Edit(Ev::Unread(s.mark(&chan, &ts))));
            }
            Act::Remind => {
                let items = remind_times().into_iter().map(|(label, t)| (format!("{label}  "), item(Act::RemindAt(t)))).collect();
                self.submenu(&it, items);
                return false;
            }
            Act::RemindAt(when) => {
                let text = self.permalink(&it.chan, &m);
                self.spawn(move |s| {
                    let res = s.call("reminders.add", &[("text", &text), ("time", &when.to_string())]).map(|_| ());
                    Event::Edit(Ev::Done { ok: "lembrete criado".into(), res })
                });
            }
            Act::Subscribe(on) => {
                let Some(parent) = m.thread_ts.clone() else { return true };
                self.set_subscribed(it.chan, parent, on);
            }
            Act::Pin(on) => self.set_pin(it.chan, it.ts, on),
            Act::CopyLink => {
                let url = self.permalink(&it.chan, &m);
                self.copy(&url);
                self.status = "link copiado".into();
            }
            Act::CopyText => {
                let names = Names { users: &self.users.names, chans: &self.chans, emoji: &self.emoji.custom };
                let (text, _) = typed(&m.text, &names);
                self.copy(&text);
                self.status = "mensagem copiada".into();
            }
            Act::Delete => {
                if !self.is_mine(&m) {
                    self.status = "só dá para excluir as suas mensagens".into();
                    return true;
                }
                let items = vec![
                    (entry(TRASH, "Excluir? Enter confirma", "Enter"), item(Act::DeleteNow)),
                    (entry(" ", "Cancelar", "Esc"), item(Act::Cancel)),
                ];
                self.submenu(&it, items);
                return false;
            }
            Act::DeleteNow => self.delete(it.chan, m),
        }
        true
    }

    fn start_edit(&mut self, it: Item, m: &Message) {
        if !self.is_mine(m) {
            self.status = "só dá para editar as suas mensagens".into();
            return;
        }
        self.cancel_edit();
        let names = Names { users: &self.users.names, chans: &self.chans, emoji: &self.emoji.custom };
        let (text, refs) = typed(&m.text, &names);
        let Some(e) = self.pane_editor(it.thread) else { return };
        let draft = e.take();
        e.set(text);
        self.edit.editing = Some(Editing { chan: it.chan, ts: it.ts, thread: it.thread, refs, draft });
        self.edit.selected = None;
        self.focus = if it.thread { Focus::Thread } else { Focus::Input };
    }

    /// The box gets back what it had before the edit.
    fn cancel_edit(&mut self) {
        let Some(e) = self.edit.editing.take() else { return };
        if let Some(ed) = self.pane_editor(e.thread) {
            ed.set(e.draft);
        }
    }

    /// Saves the edit: here at once, then in Slack; undone if Slack refuses.
    fn save_edit(&mut self) {
        let Some(thread) = self.edit.editing.as_ref().map(|e| e.thread) else { return };
        let typed = self.pane_editor(thread).map(|ed| ed.text.trim().to_string()).unwrap_or_default();
        let Some(e) = self.edit.editing.take() else { return };
        // Like Slack: saving it empty deletes the message.
        if typed.is_empty() {
            if let Some(ed) = self.pane_editor(thread) {
                ed.set(e.draft);
            }
            if let Some(old) = self.message(&e.chan, &e.ts) {
                self.delete(e.chan, old);
            }
            return;
        }
        let text = slack_text(&typed, &e.refs);
        if let Some(ed) = self.pane_editor(thread) {
            ed.set(e.draft);
        }
        let Some(old) = self.message(&e.chan, &e.ts).filter(|m| m.text != text) else { return };
        let user = self.me.as_ref().map(|m| m.user_id.clone()).unwrap_or_default();
        let edited = Edited { user, ts: format!("{}.000000", Local::now().timestamp()) };
        self.each_copy(&e.chan, &e.ts, |m| {
            m.text = text.clone();
            m.blocks = Value::Null;
            m.edited = Some(edited.clone());
        });
        let (chan, ts) = (e.chan, e.ts);
        self.spawn(move |s| {
            let res = s.update(&chan, &ts, &text);
            Event::Edit(Ev::Updated { chan, old, res })
        });
    }

    fn delete(&mut self, chan: String, old: Message) {
        let ts = old.ts.clone();
        if let Some(l) = self.messages.get_mut(&chan) {
            l.retain(|m| m.ts != ts);
        }
        if self.thread.as_ref().is_some_and(|t| t.chan == chan && t.ts == ts) {
            self.close_thread();
        } else if let Some(t) = self.thread.as_mut().filter(|t| t.chan == chan) {
            t.msgs.retain(|m| m.ts != ts);
        }
        self.edit.selected = None;
        self.spawn(move |s| {
            let res = s.call("chat.delete", &[("channel", &chan), ("ts", &ts)]).map(|_| ());
            Event::Edit(Ev::Deleted { chan, old, res })
        });
    }

    /// Pins or unpins a message: here at once, then in Slack.
    pub fn set_pin(&mut self, chan: String, ts: String, on: bool) {
        self.each_copy(&chan, &ts, |m| {
            m.pinned_to.retain(|c| *c != chan);
            if on {
                m.pinned_to.push(chan.clone());
            }
        });
        self.spawn(move |s| {
            let method = if on { "pins.add" } else { "pins.remove" };
            let res = s.call(method, &[("channel", &chan), ("timestamp", &ts)]).map(|_| ());
            Event::Edit(Ev::Pinned { chan, ts, on, res })
        });
    }

    fn set_saved(&mut self, chan: String, ts: String, on: bool) {
        let key = (chan.clone(), ts.clone());
        if on {
            self.edit.saved.insert(key);
        } else {
            self.edit.saved.remove(&key);
        }
        self.status = if on { "salva para depois" } else { "removida dos itens salvos" }.into();
        self.spawn(move |s| {
            let method = if on { "saved.add" } else { "saved.delete" };
            let res = s.call(method, &[("item_type", "message"), ("item_id", &chan), ("ts", &ts)]).map(|_| ());
            Event::Edit(Ev::SavedOne { chan, ts, on, res })
        });
    }

    /// Notifications for a thread's replies; `ts` is the parent's.
    fn set_subscribed(&mut self, chan: String, ts: String, on: bool) {
        self.each_copy(&chan, &ts, |m| m.subscribed = on);
        self.spawn(move |s| {
            let method = if on { "subscriptions.thread.add" } else { "subscriptions.thread.remove" };
            let res = s.call(method, &[("channel", &chan), ("thread_ts", &ts)]).map(|_| ());
            Event::Edit(Ev::Subscribed { chan, ts, on, res })
        });
    }

    pub fn on_edit(&mut self, ev: Ev) {
        match ev {
            Ev::Saved(Ok(s)) => self.edit.saved = s,
            Ev::Saved(Err(_)) => {}
            Ev::Updated { chan, old, res } => match res {
                Ok(new) => self.each_copy(&chan, &old.ts, |m| {
                    if !new.ts.is_empty() {
                        m.text = new.text.clone();
                        m.blocks = new.blocks.clone();
                        m.edited = new.edited.clone().or(m.edited.take());
                    }
                }),
                Err(e) => {
                    self.error = Some(e);
                    self.each_copy(&chan, &old.ts, |m| {
                        m.text = old.text.clone();
                        m.blocks = old.blocks.clone();
                        m.edited = old.edited.clone();
                    });
                }
            },
            Ev::Deleted { res: Ok(()), .. } => self.status = "mensagem excluída".into(),
            Ev::Deleted { chan, old, res: Err(e) } => {
                self.error = Some(e);
                if let Some(t) = self.thread.as_mut().filter(|t| t.chan == chan && old.thread_ts.as_ref() == Some(&t.ts)) {
                    insert(&mut t.msgs, old.clone());
                }
                if old.thread_ts.as_ref().is_none_or(|t| *t == old.ts) || old.subtype.as_deref() == Some("thread_broadcast") {
                    if let Some(l) = self.messages.get_mut(&chan) {
                        insert(l, old);
                    }
                }
            }
            Ev::Pinned { res: Ok(()), on, chan, .. } => {
                self.status = if on { "fixada na conversa" } else { "desafixada" }.into();
                // The "Fixados" tab counts what Slack lists.
                self.refresh_pins(&chan);
            }
            Ev::Pinned { chan, ts, on, res: Err(e) } => {
                self.error = Some(e);
                self.each_copy(&chan, &ts, |m| {
                    m.pinned_to.retain(|c| *c != chan);
                    if !on {
                        m.pinned_to.push(chan.clone());
                    }
                });
            }
            Ev::SavedOne { res: Ok(()), .. } => {}
            Ev::SavedOne { chan, ts, on, res: Err(e) } => {
                self.error = Some(e);
                if on {
                    self.edit.saved.remove(&(chan, ts));
                } else {
                    self.edit.saved.insert((chan, ts));
                }
            }
            Ev::Subscribed { res: Ok(()), on, .. } => {
                self.status = if on { "notificações de respostas ativadas" } else { "notificações de respostas desativadas" }.into()
            }
            Ev::Subscribed { chan, ts, on, res: Err(e) } => {
                self.error = Some(e);
                self.each_copy(&chan, &ts, |m| m.subscribed = !on);
            }
            Ev::Unread(Ok(())) => {
                self.status = "marcada como não lida".into();
                self.spawn(|s| Event::Counts(s.counts()));
            }
            Ev::Done { ok, res: Ok(()) } => self.status = ok,
            Ev::Unread(Err(e)) | Ev::Done { res: Err(e), .. } => self.error = Some(e),
        }
    }

    // ---- forward -------------------------------------------------------

    /// Conversations matching the query, names starting with it first;
    /// with none typed, the recent ones.
    fn targets(&self, query: &str) -> Vec<usize> {
        let q = query.to_lowercase();
        if q.is_empty() {
            return self.recents.iter().filter_map(|id| self.convs.iter().position(|c| c.id == *id)).take(ROWS).collect();
        }
        let mut hits: Vec<(bool, usize)> = (0..self.convs.len())
            .filter_map(|i| {
                let l = self.label(&self.convs[i]).to_lowercase();
                l.contains(&q).then(|| (!l.starts_with(&q), i))
            })
            .collect();
        hits.sort();
        hits.into_iter().map(|(_, i)| i).take(ROWS).collect()
    }

    fn conv_name(&self, i: usize) -> String {
        let c = &self.convs[i];
        match c.kind {
            Kind::Channel | Kind::Private => format!("#{}", c.name),
            _ => self.label(c),
        }
    }

    /// Posts the message's link in the chosen conversation; Slack shows it as a quote.
    fn send_forward(&mut self) {
        let Some(fw) = self.edit.forward.take() else { return };
        let Some(&i) = self.targets(&fw.query).get(fw.sel) else { return };
        let (to, name) = (self.convs[i].id.clone(), self.conv_name(i));
        let url = match self.message(&fw.chan, &fw.ts) {
            Some(m) => self.permalink(&fw.chan, &m),
            None => self.message_url(&fw.chan, &fw.ts),
        };
        self.spawn(move |s| {
            let res = s.post(&to, &url).map(|_| ());
            Event::Edit(Ev::Done { ok: format!("encaminhada para {name}"), res })
        });
    }

    pub fn forward_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let Some(query) = self.edit.forward.as_ref().map(|f| f.query.clone()) else { return };
        let n = self.targets(&query).len().max(1);
        let Some(fw) = self.edit.forward.as_mut() else { return };
        match k.code {
            KeyCode::Esc => self.edit.forward = None,
            KeyCode::Up | KeyCode::BackTab => fw.sel = (fw.sel + n - 1) % n,
            KeyCode::Down | KeyCode::Tab => fw.sel = (fw.sel + 1) % n,
            KeyCode::Enter => self.send_forward(),
            KeyCode::Backspace => {
                fw.query.pop();
                fw.sel = 0;
            }
            KeyCode::Char('u') if ctrl => {
                fw.query.clear();
                fw.sel = 0;
            }
            KeyCode::Char(c) if !ctrl => {
                fw.query.push(c);
                fw.sel = 0;
            }
            _ => {}
        }
        self.dirty = true;
    }

    /// The forward dialog takes every click; elsewhere a left click drops the pick.
    pub fn edit_mouse(&mut self, m: MouseEvent) -> bool {
        let at = Position::new(m.column, m.row);
        let left = m.kind == MouseEventKind::Down(MouseButton::Left);
        if self.edit.forward.is_some() {
            if left {
                match self.edit.rows.iter().find(|(r, _)| r.contains(at)).map(|(_, i)| *i) {
                    Some(i) => {
                        self.edit.forward.iter_mut().for_each(|f| f.sel = i);
                        self.send_forward();
                    }
                    None if !self.edit.dialog.contains(at) => self.edit.forward = None,
                    None => {}
                }
                self.dirty = true;
            }
            return true;
        }
        if left && self.edit.selected.take().is_some() {
            self.dirty = true;
        }
        false
    }
}

/// The scroll (lines up from the end) that shows `lines`, moving as little as
/// possible; a message taller than the view shows its end.
pub fn reveal(scroll: usize, total: usize, height: usize, lines: Range<usize>) -> usize {
    let end = total - scroll.min(total);
    let start = end.saturating_sub(height);
    if lines.end > end {
        total - lines.end
    } else if lines.start < start {
        (total - lines.start).saturating_sub(height)
    } else {
        scroll
    }
}

/// "Editando mensagem" on the border of the box doing an edit.
pub fn title(app: &App, thread: bool) -> Option<Line<'static>> {
    app.edit.editing.as_ref().filter(|e| e.thread == thread)?;
    Some(Line::from(vec![
        Span::styled(" Editando mensagem ", Style::new().fg(Color::Yellow).bold()),
        Span::styled("Esc cancela · Enter salva ", dim()),
    ]))
}

/// The status line's keys while a message is picked or edited.
pub fn hint(app: &App) -> Option<String> {
    let thread = app.focused_pane()?;
    if app.edit.editing.as_ref().is_some_and(|e| e.thread == thread) {
        return Some("Enter salvar · Esc cancelar · Shift+Enter nova linha".into());
    }
    let sel = app.edit.selected.as_ref().filter(|s| s.thread == thread)?;
    let mine = app.pane(thread)?.1.iter().find(|m| m.ts == sel.ts).is_some_and(|m| app.is_mine(m));
    let mut keys = Vec::new();
    if mine {
        keys.push("E editar");
    }
    keys.push("R reagir");
    if !thread {
        keys.extend(["T responder", "U não lida"]);
    }
    keys.extend(["F encaminhar", "A salvar", "L link"]);
    if mine {
        keys.push("Del excluir");
    }
    keys.extend(["M menu", "Esc cancelar"]);
    Some(keys.join(" · "))
}

/// The forward dialog, over everything.
pub fn draw(f: &mut Frame, app: &mut App) {
    app.edit.rows.clear();
    app.edit.dialog = Rect::default();
    let Some(fw) = &app.edit.forward else { return };
    let (query, sel) = (fw.query.clone(), fw.sel);
    let targets = app.targets(&query);
    let area = f.area();
    let (w, h) = (56.min(area.width), (ROWS as u16 + 6).min(area.height));
    let rect = Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 3, w, h);
    f.render_widget(Clear, rect);
    let block = Block::bordered().border_type(BorderType::Rounded).border_style(dim()).title(Line::from(" Encaminhar mensagem ").bold());
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    app.edit.dialog = rect;
    if inner.height < 4 {
        return;
    }
    let width = inner.width as usize;
    let to = Line::from(vec![Span::styled(" Para: ", dim()), Span::raw(truncate(&query, width.saturating_sub(8)))]);
    f.render_widget(Paragraph::new(to), Rect::new(inner.x, inner.y, inner.width, 1));
    f.set_cursor_position((inner.x + 7 + query.chars().count().min(width.saturating_sub(8)) as u16, inner.y));
    for (n, &i) in targets.iter().enumerate() {
        let y = inner.y + 2 + n as u16;
        if y >= inner.bottom().saturating_sub(1) {
            break;
        }
        let row = Rect::new(inner.x, y, inner.width, 1);
        let name = truncate(&app.conv_name(i), width.saturating_sub(2));
        let style = if n == sel { Style::new().add_modifier(Modifier::REVERSED) } else { Style::new() };
        f.render_widget(Paragraph::new(Line::from(format!(" {name:<w$}", w = width.saturating_sub(1))).style(style)), row);
        app.edit.rows.push((row, n));
    }
    if targets.is_empty() {
        f.render_widget(Paragraph::new(" nenhuma conversa").style(dim()), Rect::new(inner.x, inner.y + 2, inner.width, 1));
    }
    let foot = Rect::new(inner.x, inner.bottom() - 1, inner.width, 1);
    f.render_widget(Paragraph::new(" Enter envia o link · Esc cancela").style(dim()), foot);
}
