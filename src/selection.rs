//! Messages picked by dragging the mouse over a pane, copied as Slack copies
//! them: `Nome  [21h29]` over each header's text, `[21h30]texto` below it.

use crate::app::App;
use crate::slack::Message;
use crate::text::{self, Names};
use crate::ui;
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Position;

/// The messages from the one pressed on to the one under the pointer.
pub struct Selection {
    /// In the thread panel rather than the conversation.
    pub thread: bool,
    pub anchor: String,
    pub head: String,
    /// The row pressed on; leaving it makes it a drag, not a click.
    row: u16,
    /// Dragged: shaded and copied on release.
    pub moved: bool,
    /// The button is still down.
    held: bool,
}

impl App {
    /// A press over a message starts a selection (the click still does what it
    /// does), a drag stretches it and the release copies it. True when taken.
    pub(crate) fn selection_mouse(&mut self, m: MouseEvent) -> bool {
        let at = Position::new(m.column, m.row);
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.dirty |= self.selection.take().is_some_and(|s| s.moved);
                let covered = self.menu.is_some()
                    || self.viewer.is_some()
                    || self.react.is_some()
                    || self.dialog.is_some()
                    || self.edit.forward.is_some()
                    || self.account.menu.is_some()
                    || self.account.dialog.is_some()
                    || self.account.profile.is_some()
                    || self.account.about;
                if let Some((thread, ts)) = self.message_at(at).filter(|_| !covered) {
                    self.selection = Some(Selection { thread, anchor: ts.clone(), head: ts, row: m.row, moved: false, held: true });
                }
                false
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                let Some(s) = self.selection.as_ref().filter(|s| s.held) else { return false };
                // A click that opened something over the messages is no drag.
                if self.viewer.is_some() || self.menu.is_some() || self.react.is_some() {
                    self.selection = None;
                    return false;
                }
                if !s.moved && m.row == s.row {
                    return true;
                }
                let thread = s.thread;
                let head = self.message_at(at).filter(|(t, _)| *t == thread).map(|(_, ts)| ts);
                if let Some(s) = self.selection.as_mut() {
                    s.moved = true;
                    s.head = head.unwrap_or_else(|| s.head.clone());
                }
                self.dirty = true;
                true
            }
            MouseEventKind::Up(MouseButton::Left) => {
                let Some(s) = self.selection.as_mut().filter(|s| s.held) else { return false };
                s.held = false;
                if !s.moved {
                    self.selection = None;
                    return false;
                }
                self.copy_selection();
                self.dirty = true;
                true
            }
            _ => false,
        }
    }

    /// The message under a screen position, and whether it is in the thread panel.
    fn message_at(&self, at: Position) -> Option<(bool, String)> {
        let (_, ts) = self.areas.message_rows.iter().find(|(r, _)| r.contains(at))?;
        Some((self.areas.thread.contains(at), ts.clone()))
    }

    fn copy_selection(&mut self) {
        let Some(s) = self.selection.as_ref() else { return };
        let Some((_, msgs)) = self.pane(s.thread) else { return };
        let at = |ts: &str| msgs.iter().position(|m| m.ts == ts);
        let (Some(a), Some(b)) = (at(&s.anchor), at(&s.head)) else { return };
        let (a, b) = (a.min(b), a.max(b));
        let names = Names { users: &self.users.names, chans: &self.chans, emoji: &self.emoji.custom };
        let mut out = String::new();
        let mut prev: Option<&Message> = None;
        for (i, m) in msgs.iter().enumerate().take(b + 1).skip(a) {
            // The panel draws the parent apart, so the first reply has its own header.
            if s.thread && i == 1 {
                prev = None;
            }
            let hhmm = ui::hhmm(&m.ts);
            if ui::grouped(self, prev, m) {
                out.push_str(&format!("[{hhmm}]"));
            } else {
                out.push_str(&format!("{}  [{hhmm}]\n", ui::author(m, &names)));
            }
            let mut body = text::body(m, &names);
            for f in &m.files {
                if !body.is_empty() {
                    body.push('\n');
                }
                body.push_str(&f.name);
            }
            out.push_str(&body);
            out.push('\n');
            prev = Some(m);
        }
        let n = b - a + 1;
        let text = out.trim_end().to_string();
        self.copy(&text);
        self.status = if n == 1 { "mensagem copiada".into() } else { format!("{n} mensagens copiadas") };
    }
}
