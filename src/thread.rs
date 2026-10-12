//! Threads: a message's replies in the panel on the right, as in Slack.

use crate::app::{App, Event, Focus, POLL_EVERY};
use crate::editor::Editor;
use crate::slack::{escape, parse_message, Kind, Message, Res, Slack};
use crate::text::Names;
use crate::ui::{dim, flash, message_lines, place, select, shade, truncate, MAX_INPUT_ROWS, PROFILE};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Constraint, Layout, Margin, Position, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use ratatui::Frame;
use std::time::Instant;
use unicode_width::UnicodeWidthStr;

// Nerd Font squares, one cell each like the other glyphs in the UI.
const BOX_OFF: &str = "\u{f096}";
const BOX_ON: &str = "\u{f14a}";

pub struct Thread {
    pub chan: String,
    /// The parent's ts, which is what names a thread.
    pub ts: String,
    /// The parent first, then the replies, oldest first.
    pub msgs: Vec<Message>,
    pub loaded: bool,
    pub editor: Editor,
    /// "Também enviar para #canal".
    pub broadcast: bool,
    /// Lines scrolled up from the newest reply.
    pub scroll: usize,
    /// Set by the last draw: how far `scroll` can go.
    pub max_scroll: usize,
    /// A reply to scroll to once the replies are loaded.
    pub jump: Option<String>,
    /// Set by the last draw: the text width of the reply box.
    pub input_width: usize,
    polling: bool,
    last_poll: Instant,
}

/// Slack's thread pane takes about 40% of the width; the conversation keeps 40 columns.
pub fn width(total: u16) -> u16 {
    (total * 2 / 5).max(PROFILE).min(total.saturating_sub(40))
}

impl Slack {
    /// The parent and all its replies, oldest first. Slack pages from the newest
    /// reply backwards and repeats the parent on every page, so pages are put
    /// back in order.
    pub fn replies(&self, chan: &str, ts: &str) -> Res<Vec<Message>> {
        let list = self.paginate("conversations.replies", &[("channel", chan), ("ts", ts), ("limit", "200")], "messages")?;
        let mut msgs: Vec<Message> = list.iter().map(parse_message).collect();
        msgs.sort_by(|a, b| a.ts.cmp(&b.ts));
        msgs.dedup_by(|a, b| a.ts == b.ts);
        Ok(msgs)
    }

    /// A reply in the thread `ts`; `broadcast` also shows it in the channel.
    pub fn reply(&self, chan: &str, ts: &str, text: &str, broadcast: bool) -> Res<Message> {
        let text = escape(text);
        let mut p = vec![("channel", chan), ("thread_ts", ts), ("text", text.as_str())];
        if broadcast {
            p.push(("reply_broadcast", "true"));
        }
        let v = self.call("chat.postMessage", &p)?;
        let mut m = parse_message(&v["message"]);
        if m.ts.is_empty() {
            m.ts = v["ts"].as_str().unwrap_or("").to_string();
        }
        Ok(m)
    }
}

/// Inserts by ts, skipping one already there: a poll and a send land in either order.
fn insert(list: &mut Vec<Message>, m: Message) {
    if let Err(pos) = list.binary_search_by(|x| x.ts.cmp(&m.ts)) {
        list.insert(pos, m);
    }
}

impl App {
    /// Opens a thread of the current conversation, in place of the profile.
    pub(crate) fn open_thread(&mut self, ts: String) {
        let Some(chan) = self.current.clone() else { return };
        // The parent shows at once when it is loaded; the fetch brings the rest.
        let parent = self.messages.get(&chan).and_then(|l| l.iter().find(|m| m.ts == ts)).cloned();
        self.profile = None;
        self.edit.forget_thread();
        self.thread = Some(Thread {
            chan,
            ts,
            msgs: parent.into_iter().collect(),
            loaded: false,
            editor: Editor::default(),
            broadcast: false,
            scroll: 0,
            max_scroll: 0,
            jump: None,
            input_width: 40,
            polling: false,
            last_poll: Instant::now(),
        });
        self.focus = Focus::Thread;
        self.fetch_thread();
    }

    pub(crate) fn close_thread(&mut self) {
        self.thread = None;
        self.edit.forget_thread();
        if self.focus == Focus::Thread {
            self.focus = Focus::Input;
        }
        self.dirty = true;
    }

    fn fetch_thread(&mut self) {
        let Some(t) = self.thread.as_mut() else { return };
        t.polling = true;
        t.last_poll = Instant::now();
        let (chan, ts) = (t.chan.clone(), t.ts.clone());
        self.spawn(move |s| {
            let res = s.replies(&chan, &ts);
            Event::Replies { chan, ts, res }
        });
    }

    /// From `tick`: the open thread refreshes as often as the conversation.
    pub(crate) fn poll_thread(&mut self) {
        if self.thread.as_ref().is_some_and(|t| !t.polling && t.last_poll.elapsed() >= POLL_EVERY) {
            self.fetch_thread();
        }
    }

    /// The parent in the conversation's loaded history.
    fn parent_mut(&mut self, chan: &str, ts: &str) -> Option<&mut Message> {
        let list = self.messages.get_mut(chan)?;
        let i = list.binary_search_by(|m| m.ts.as_str().cmp(ts)).ok()?;
        list.get_mut(i)
    }

    fn open_thread_mut(&mut self, chan: &str, ts: &str) -> Option<&mut Thread> {
        self.thread.as_mut().filter(|t| t.chan == chan && t.ts == ts)
    }

    pub(crate) fn on_replies(&mut self, chan: String, ts: String, res: Res<Vec<Message>>) {
        if let Some(t) = self.open_thread_mut(&chan, &ts) {
            t.polling = false;
        }
        let new = match res {
            Ok(new) => new,
            Err(e) => return self.error = Some(e),
        };
        // The conversation's "N respostas" follows the thread.
        if let Some(fresh) = new.first().filter(|m| m.ts == ts).cloned() {
            if let Some(p) = self.parent_mut(&chan, &ts) {
                p.reply_count = fresh.reply_count;
                p.latest_reply = fresh.latest_reply;
                p.reply_users = fresh.reply_users;
            }
        }
        let Some(t) = self.open_thread_mut(&chan, &ts) else { return };
        // A reply sent while this was on its way stays until a fetch has it.
        let last = new.last().map(|m| m.ts.clone()).unwrap_or_default();
        let sent: Vec<Message> = t.msgs.drain(..).filter(|m| m.ts > last).collect();
        t.msgs = new;
        t.msgs.extend(sent);
        t.loaded = true;
    }

    pub(crate) fn on_replied(&mut self, chan: String, ts: String, text: String, res: Res<Message>) {
        let m = match res {
            Ok(m) => m,
            Err(e) => {
                self.error = Some(e);
                if let Some(t) = self.open_thread_mut(&chan, &ts).filter(|t| t.editor.is_empty()) {
                    t.editor.set(text);
                }
                return;
            }
        };
        if let Some(p) = self.parent_mut(&chan, &ts).filter(|p| p.latest_reply.as_ref().is_none_or(|l| *l < m.ts)) {
            p.reply_count += 1;
            p.latest_reply = Some(m.ts.clone());
            if let Some(u) = m.user.clone().filter(|u| !p.reply_users.contains(u)) {
                p.reply_users.push(u);
            }
        }
        if m.subtype.as_deref() == Some("thread_broadcast") {
            if let Some(list) = self.messages.get_mut(&chan) {
                insert(list, m.clone());
            }
        }
        if let Some(t) = self.open_thread_mut(&chan, &ts) {
            insert(&mut t.msgs, m);
        }
    }

    fn send_reply(&mut self) {
        let Some(t) = self.thread.as_mut() else { return };
        if t.editor.is_empty() {
            return;
        }
        let text = t.editor.take().trim().to_string();
        let (chan, ts, broadcast) = (t.chan.clone(), t.ts.clone(), t.broadcast);
        // Like Slack, "também enviar" is for one reply.
        t.broadcast = false;
        t.scroll = 0;
        self.spawn(move |s| {
            let res = s.reply(&chan, &ts, &text, broadcast);
            Event::Replied { chan, ts, text, res }
        });
    }

    pub(crate) fn scroll_thread(&mut self, delta: isize) {
        if let Some(t) = self.thread.as_mut() {
            t.scroll = t.scroll.saturating_add_signed(delta).min(t.max_scroll);
        }
    }

    pub(crate) fn on_thread_key(&mut self, k: KeyEvent, ctrl: bool) {
        let newline = k.modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::ALT);
        let Some(t) = self.thread.as_mut() else { return self.focus = Focus::Input };
        let e = &mut t.editor;
        match k.code {
            KeyCode::Esc => self.focus = Focus::Input,
            KeyCode::Enter if newline => e.insert("\n"),
            KeyCode::Char('j') if ctrl => e.insert("\n"),
            KeyCode::Char('d') if ctrl => e.set(String::new()),
            KeyCode::Enter => self.send_reply(),
            KeyCode::Char('b') if ctrl => t.broadcast = !t.broadcast,
            KeyCode::Left => e.left(),
            KeyCode::Right => e.right(),
            KeyCode::Home => e.home(),
            KeyCode::End => e.end(),
            KeyCode::Up => {
                if !e.vertical(t.input_width, -1) {
                    t.scroll = (t.scroll + 1).min(t.max_scroll);
                }
            }
            KeyCode::Down => {
                if !e.vertical(t.input_width, 1) {
                    t.scroll = t.scroll.saturating_sub(1);
                }
            }
            KeyCode::Backspace if ctrl => e.delete_word(),
            KeyCode::Backspace => e.backspace(),
            KeyCode::Delete => e.delete(),
            KeyCode::Char('u') if ctrl => {
                e.take();
            }
            KeyCode::Char('w') if ctrl => e.delete_word(),
            KeyCode::Char(c) if !ctrl => {
                let mut b = [0; 4];
                e.insert(c.encode_utf8(&mut b));
            }
            _ => {}
        }
    }

    /// Thread links anywhere, and clicks and the wheel on the panel. True when handled.
    pub(crate) fn thread_mouse(&mut self, m: MouseEvent, at: Position) -> bool {
        let left = m.kind == MouseEventKind::Down(MouseButton::Left);
        if let Some((_, ts)) = self.areas.thread_links.iter().find(|(r, _)| left && r.contains(at)) {
            let ts = ts.clone();
            self.open_thread(ts);
            return true;
        }
        if self.thread.is_none() || !self.areas.thread.contains(at) {
            return false;
        }
        let close = self.areas.thread_close.contains(at);
        let broadcast = self.areas.thread_broadcast.contains(at);
        match m.kind {
            MouseEventKind::ScrollUp => self.scroll_thread(3),
            MouseEventKind::ScrollDown => self.scroll_thread(-3),
            _ if !left => return false,
            _ if close => self.close_thread(),
            _ if broadcast => self.thread.iter_mut().for_each(|t| t.broadcast = !t.broadcast),
            _ => self.focus = Focus::Thread,
        }
        true
    }
}

/// The thread panel: the parent, the replies and a box to reply.
pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let a = &mut app.areas;
    (a.thread, a.thread_close, a.thread_input, a.thread_broadcast) = Default::default();
    let Some(t) = app.thread.as_ref() else { return };
    let block = Block::new().borders(Borders::LEFT).border_style(dim());
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width < 20 || inner.height < 8 {
        return;
    }

    let label = match app.convs.iter().find(|c| c.id == t.chan) {
        Some(c) if matches!(c.kind, Kind::Channel | Kind::Private) => format!("#{}", c.name),
        Some(c) => app.label(c),
        None => String::new(),
    };
    let input_w = inner.width.saturating_sub(6) as usize;
    let rows = t.editor.rows(input_w);
    let cursor = t.editor.position(&rows);
    let empty = t.editor.text.is_empty();
    let (broadcast, scroll, loaded) = (t.broadcast, t.scroll, t.loaded);
    let mut list = t.msgs.clone();
    let input_rows = rows.len().clamp(1, MAX_INPUT_ROWS) as u16;
    let [header, msgs, input, toggle] =
        Layout::vertical([Constraint::Length(2), Constraint::Min(1), Constraint::Length(input_rows + 2), Constraint::Length(1)])
            .areas(inner);
    app.areas.thread = area;

    // "Conversa #canal" and a close button, like Slack's header.
    f.render_widget(Block::new().borders(Borders::BOTTOM).border_style(dim()), header);
    let room = (header.width as usize).saturating_sub(16);
    let title = Line::from(vec![
        Span::raw(" "),
        Span::styled("Conversa", Style::new().bold()),
        Span::styled(format!("  {}", truncate(&label, room)), dim()),
    ]);
    f.render_widget(Paragraph::new(title), Rect::new(header.x, header.y, header.width.saturating_sub(4), 1));
    let close = Rect::new(header.right().saturating_sub(4), header.y, 3, 1);
    f.render_widget(Paragraph::new(" ✕").style(dim()), close);
    app.areas.thread_close = close;

    // The parent, "N respostas" and the replies, through the same rendering as
    // the conversation. The panel shows its own count and no links into itself.
    let replies = if loaded { list.len().saturating_sub(1) } else { list.first().map_or(0, |p| p.reply_count as usize) };
    for m in &mut list {
        m.reply_count = 0;
        if m.subtype.as_deref() == Some("thread_broadcast") {
            m.subtype = None;
        }
    }
    let body = msgs.inner(Margin { horizontal: 1, vertical: 0 });
    let width = body.width as usize;
    let names = Names { users: &app.users.names, chans: &app.chans, emoji: &app.emoji.custom };
    let font = app.picker.as_ref().map(|p| p.font_size()).map(|s| (s.width, s.height));
    let mut r = message_lines(&list[..list.len().min(1)], None, &names, app, width, font);
    if replies > 0 {
        let count = format!("{replies} {} ", if replies == 1 { "resposta" } else { "respostas" });
        r.lines.push(Line::from(""));
        r.lines.push(Line::from(vec![
            Span::styled(count.clone(), dim()),
            Span::styled("─".repeat(width.saturating_sub(count.width())), dim()),
        ]));
    }
    if loaded {
        r.append(message_lines(&list[1.min(list.len())..], None, &names, app, width, font));
    } else {
        r.lines.push(Line::from(""));
        r.lines.push(Line::from("carregando…").style(dim()));
    }
    // Opened from "Fixados": shows the reply picked there.
    let jump = if loaded { app.thread.as_mut().and_then(|t| t.jump.take()) } else { None };
    let scroll = jump.and_then(|ts| r.scroll_to(&ts, body.height as usize * 2 / 3)).unwrap_or(scroll);
    let scroll = select(app, &mut r, true, body, scroll);
    flash(app, &mut r, body);
    shade(app, &mut r, true, body);
    let max = place(f, app, body, r, scroll, false);
    if let Some(t) = app.thread.as_mut() {
        t.max_scroll = max;
        t.scroll = scroll.min(max);
        t.input_width = input_w;
    }

    // The reply box, like the conversation's.
    let focused = app.focus == Focus::Thread;
    let boxed = input.inner(Margin { horizontal: 1, vertical: 0 });
    app.areas.thread_input = boxed;
    let block = Block::bordered().border_type(BorderType::Rounded).border_style(if focused {
        Style::new().fg(Color::Cyan)
    } else {
        dim()
    });
    let block = match crate::edit::title(app, true) {
        Some(t) => block.title_top(t).border_style(Style::new().fg(Color::Yellow)),
        None => block,
    };
    let text_area = block.inner(boxed).inner(Margin { horizontal: 1, vertical: 0 });
    f.render_widget(block, boxed);
    if empty {
        f.render_widget(Paragraph::new("Responder…").style(dim()), text_area);
        if focused {
            f.set_cursor_position((text_area.x, text_area.y));
        }
    } else {
        let (cr, cc) = cursor;
        let h = text_area.height as usize;
        let top = (cr + 1).saturating_sub(h);
        let lines: Vec<Line> = rows[top..].iter().take(h).map(|r| Line::from(r.chars.iter().collect::<String>())).collect();
        f.render_widget(Paragraph::new(lines), text_area);
        if focused {
            f.set_cursor_position((text_area.x + cc as u16, text_area.y + (cr - top) as u16));
        }
    }

    // "Também enviar para #canal", a checkbox to click or Ctrl+B.
    let (icon, style) = if broadcast { (BOX_ON, Style::new().fg(Color::Cyan)) } else { (BOX_OFF, dim()) };
    let text = truncate(&format!(" Também enviar para {label}"), (toggle.width as usize).saturating_sub(9));
    let line = Line::from(vec![Span::raw("  "), Span::styled(icon, style), Span::raw(text), Span::styled("  ^B", dim())]);
    app.areas.thread_broadcast = Rect::new(toggle.x, toggle.y, line.width() as u16, 1).intersection(toggle);
    f.render_widget(Paragraph::new(line), toggle);
}
