//! Pinned messages: the conversation's "Pins" tab, as in Slack.

use crate::app::{App, Event, Focus};
use crate::i18n::tr;
use crate::slack::{parse_message, Message, Res, Slack};
use crate::text::Names;
use crate::ui::{dim, message_lines, place};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Alignment, Position, Rect};
use ratatui::style::Style;
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use std::collections::HashMap;
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Copy, Default, PartialEq)]
pub enum Tab {
    #[default]
    Messages,
    Pinned,
}

pub struct Pin {
    pub msg: Message,
    /// Who pinned it, for "Fixada por".
    pub by: Option<String>,
    /// When it was pinned: the tab lists the newest pin first.
    pub created: i64,
}

#[derive(Default)]
pub struct Pins {
    /// Per conversation, newest pin first; the tab's count comes from here.
    pub by_chan: HashMap<String, Vec<Pin>>,
    pub tab: Tab,
    /// Lines scrolled down from the top of the list.
    pub scroll: usize,
    /// Set by the last draw: how far `scroll` can go.
    pub max_scroll: usize,
    /// A message picked in the tab: the next draw of the conversation scrolls to it.
    pub jump: Option<String>,
}

/// Where the last draw put the tabs.
#[derive(Default)]
pub struct Areas {
    pub tabs: Vec<(Rect, Tab)>,
}

impl Slack {
    /// The conversation's pinned messages, newest pin first. Items without a
    /// message (files pinned the old way) have nothing to show and are left out.
    pub fn pins(&self, chan: &str) -> Res<Vec<Pin>> {
        let v = self.call("pins.list", &[("channel", chan)])?;
        let mut pins: Vec<Pin> = v["items"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|i| i["message"].is_object())
            .map(|i| Pin {
                msg: parse_message(&i["message"]),
                by: i["created_by"].as_str().map(String::from),
                created: i["created"].as_i64().unwrap_or(0),
            })
            .collect();
        pins.sort_by(|a, b| b.created.cmp(&a.created));
        Ok(pins)
    }
}

/// "📌 Pinned by Jane", over a pinned message.
pub(crate) fn label(app: &App, chan: &str, ts: &str) -> String {
    let by = app.pins.by_chan.get(chan).and_then(|l| l.iter().find(|p| p.msg.ts == ts)).and_then(|p| p.by.as_deref());
    match by {
        Some(u) if app.is_me(u) => tr("📌 Pinned by you", "📌 Fixada por você").into(),
        Some(u) => trf!("📌 Pinned by {}", "📌 Fixada por {}", app.users.names.get(u).map_or(u, String::as_str)),
        None => tr("📌 Pinned", "📌 Fixada").into(),
    }
}

impl App {
    /// Asks Slack for the conversation's pins again: on opening it, on opening
    /// the tab, and after a message is pinned or unpinned here.
    pub fn refresh_pins(&mut self, chan: &str) {
        let chan = chan.to_string();
        self.spawn(move |s| {
            let res = s.pins(&chan);
            Event::Pins { chan, res }
        });
    }

    pub fn pin_count(&self, chan: &str) -> usize {
        self.pins.by_chan.get(chan).map_or(0, Vec::len)
    }

    pub(crate) fn on_pins(&mut self, chan: String, res: Res<Vec<Pin>>) {
        let pins = match res {
            Ok(p) => p,
            // Only worth a word when someone is looking at the tab.
            Err(e) if self.pins.tab == Tab::Pinned && self.current.as_deref() == Some(chan.as_str()) => return self.error = Some(e),
            Err(_) => return,
        };
        // Pins made in another client reach the loaded history too, so its
        // "Fixada" lines agree with the count.
        if let Some(list) = self.messages.get_mut(&chan) {
            for m in list {
                let pinned = pins.iter().any(|p| p.msg.ts == m.ts);
                m.pinned_to.retain(|c| *c != chan);
                if pinned {
                    m.pinned_to.push(chan.clone());
                }
            }
        }
        // The last pin gone: its tab goes too.
        if pins.is_empty() && self.current.as_deref() == Some(chan.as_str()) {
            self.pins.tab = Tab::Messages;
        }
        self.pins.by_chan.insert(chan, pins);
    }

    pub(crate) fn set_tab(&mut self, tab: Tab) {
        let Some(chan) = self.current.clone() else { return };
        if tab == Tab::Pinned {
            // No tab to open while there is nothing pinned.
            if self.pin_count(&chan) == 0 {
                return;
            }
            self.pins.scroll = 0;
            self.refresh_pins(&chan);
        }
        self.pins.tab = tab;
    }

    fn scroll_pins(&mut self, delta: isize) {
        self.pins.scroll = self.pins.scroll.saturating_add_signed(delta).min(self.pins.max_scroll);
    }

    /// Back to the conversation at a pinned message, highlighted for a moment:
    /// scrolled to it, its history fetched when it is not loaded. A reply
    /// opens its thread at it, the conversation at the parent.
    fn jump_to(&mut self, ts: String) {
        let Some(chan) = self.current.clone() else { return };
        let parent = self
            .pins
            .by_chan
            .get(&chan)
            .and_then(|l| l.iter().find(|p| p.msg.ts == ts))
            .and_then(|p| p.msg.thread_ts.clone())
            .filter(|t| *t != ts);
        self.pins.tab = Tab::Messages;
        self.focus = Focus::Input;
        self.flash = Some((ts.clone(), std::time::Instant::now()));
        let at = match parent {
            Some(parent) => {
                self.open_thread(parent.clone());
                if let Some(t) = self.thread.as_mut() {
                    t.jump = Some(ts);
                }
                parent
            }
            None => ts,
        };
        self.show_message(chan, at);
    }

    /// Alt+1 and Alt+2 switch tabs; in "Fixados", the scroll keys move the
    /// list and sending goes back to the conversation. True when handled.
    pub(crate) fn pins_key(&mut self, k: KeyEvent) -> bool {
        if k.modifiers.contains(KeyModifiers::ALT) && matches!(k.code, KeyCode::Char('1' | '2')) {
            self.set_tab(if k.code == KeyCode::Char('1') { Tab::Messages } else { Tab::Pinned });
            return true;
        }
        if self.pins.tab != Tab::Pinned || self.focus != Focus::Input {
            return false;
        }
        match k.code {
            KeyCode::PageUp => self.scroll_pins(-10),
            KeyCode::PageDown => self.scroll_pins(10),
            KeyCode::Up | KeyCode::Down => {
                let delta = if k.code == KeyCode::Up { -1 } else { 1 };
                if !self.editor.vertical(self.input_width, delta) {
                    self.scroll_pins(delta);
                }
            }
            KeyCode::Esc => self.pins.tab = Tab::Messages,
            // The message is sent as usual and shows up in the conversation.
            KeyCode::Enter if k.modifiers.is_empty() && !self.editor.is_empty() => {
                self.pins.tab = Tab::Messages;
                return false;
            }
            _ => return false,
        }
        true
    }

    /// Clicks on the tabs, and the wheel and clicks on the pinned list. True when handled.
    pub(crate) fn pins_mouse(&mut self, m: MouseEvent, at: Position) -> bool {
        let left = m.kind == MouseEventKind::Down(MouseButton::Left);
        if let Some(&(_, tab)) = self.areas.pins.tabs.iter().find(|(r, _)| left && r.contains(at)) {
            self.set_tab(tab);
            return true;
        }
        if self.pins.tab != Tab::Pinned || !self.areas.messages.contains(at) {
            return false;
        }
        match m.kind {
            MouseEventKind::ScrollUp => self.scroll_pins(-3),
            MouseEventKind::ScrollDown => self.scroll_pins(3),
            _ if !left => return false,
            // "N respostas" opens the thread, as in the conversation.
            _ if self.areas.thread_links.iter().any(|(r, _)| r.contains(at)) => return false,
            _ => match self.areas.message_rows.iter().find(|(r, _)| r.contains(at)) {
                Some((_, ts)) => {
                    let ts = ts.clone();
                    self.jump_to(ts);
                }
                None => self.focus = Focus::Input,
            },
        }
        true
    }
}

/// The tab row under the conversation's name, the active tab underlined on
/// the header's bottom border. `area` is the whole header.
pub fn tabs(f: &mut Frame, app: &mut App, area: Rect) {
    let n = app.current.as_deref().map_or(0, |c| app.pin_count(c));
    let (row, border) = (area.bottom().saturating_sub(2), area.bottom().saturating_sub(1));
    let mut x = area.x + 1;
    // "Fixados" only shows once the conversation is known to have pins.
    let mut tabs = vec![(Tab::Messages, tr("Messages", "Mensagens").to_string())];
    if n > 0 {
        tabs.push((Tab::Pinned, trf!("Pins {n}", "Fixados {n}")));
    }
    for (tab, name) in tabs {
        let w = name.width() as u16 + 2;
        let rect = Rect::new(x, row, w, 1).intersection(area);
        let active = app.pins.tab == tab;
        f.render_widget(Paragraph::new(format!(" {name} ")).style(if active { Style::new().bold() } else { dim() }), rect);
        if active {
            f.render_widget(Paragraph::new("━".repeat(w as usize)), Rect::new(x, border, w, 1).intersection(area));
        }
        app.areas.pins.tabs.push((rect, tab));
        x += w + 1;
    }
}

/// The "Fixados" tab in place of the messages: each pin on its own, with its
/// day, through the same rendering as the conversation.
pub fn draw(f: &mut Frame, app: &mut App, inner: Rect, chan: &str) {
    let r = {
        let Some(pins) = app.pins.by_chan.get(chan) else {
            return f.render_widget(Paragraph::new(tr("loading…", "carregando…")).style(dim()), inner);
        };
        if pins.is_empty() {
            let middle = Rect::new(inner.x, inner.y + inner.height / 2, inner.width, 1.min(inner.height));
            let empty = Paragraph::new(tr("No pinned messages in this conversation.", "Nenhuma mensagem fixada nesta conversa.")).style(dim()).alignment(Alignment::Center);
            return f.render_widget(empty, middle);
        }
        let names = Names { users: &app.users.names, chans: &app.chans, emoji: &app.emoji.custom };
        let font = app.picker.as_ref().map(|p| p.font_size()).map(|s| (s.width, s.height));
        let mut each = pins.iter().map(|p| message_lines(std::slice::from_ref(&p.msg), None, &names, app, inner.width as usize, font));
        let mut r = each.next().expect("not empty");
        each.for_each(|one| r.append(one));
        r
    };
    // `place` counts from the bottom; the list reads from the top.
    let max = r.lines.len().saturating_sub(inner.height as usize);
    app.pins.max_scroll = max;
    app.pins.scroll = app.pins.scroll.min(max);
    place(f, app, inner, r, max - app.pins.scroll, false);
}
