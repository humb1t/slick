//! The "new" button at the top of the sidebar, as in Slack: a message to one
//! or more people, or a new channel, each in a dialog over everything else.

use crate::app::{Action, App, Event, ImgRef, Menu};
use crate::i18n::tr;
use crate::slack::{Conversation, Count, Kind, Res};
use crate::ui::{dim, draw_image, truncate};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use ratatui::Frame;
use std::collections::HashMap;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Nerd Font pencil-square, Slack's "new" icon.
pub const NEW: &str = "\u{f044}";
const ONLINE: &str = "\u{f111}";
const OFFLINE: &str = "\u{f10c}";
const LOCK: &str = "\u{f023}";
const TOGGLE_ON: &str = "\u{f205}";
const TOGGLE_OFF: &str = "\u{f204}";
/// `conversations.open` takes at most this many people besides yourself.
const MAX_PEOPLE: usize = 8;
const NAME_MAX: usize = 80;
/// Label column of the channel dialog.
const LABEL_W: usize = 13;

pub enum Dialog {
    Message(NewMessage),
    Channel(NewChannel),
}

#[derive(Default)]
pub struct NewMessage {
    pub query: String,
    /// The people picked so far, in order.
    pub to: Vec<String>,
    pub sel: usize,
    /// Moved through the results with the arrows: Enter picks instead of opening.
    browsing: bool,
    pub busy: bool,
    pub error: Option<String>,
}

#[derive(Default)]
pub struct NewChannel {
    pub name: String,
    pub purpose: String,
    pub private: bool,
    /// 0 = name, 1 = private, 2 = description.
    pub field: usize,
    pub busy: bool,
    pub error: Option<String>,
}

impl NewChannel {
    fn text(&mut self) -> Option<&mut String> {
        match self.field {
            0 => Some(&mut self.name),
            2 => Some(&mut self.purpose),
            _ => None,
        }
    }
}

/// Where the last draw put the button and the dialog's parts, for the mouse.
#[derive(Default)]
pub struct Areas {
    pub button: Rect,
    pub dialog: Rect,
    /// Result rows, by index into `App::candidates`.
    pub results: Vec<(Rect, usize)>,
    /// Picked people, by index into `NewMessage::to`; a click removes one.
    pub chips: Vec<(Rect, usize)>,
    /// Channel dialog fields, by `NewChannel::field`.
    pub fields: Vec<(Rect, usize)>,
    pub confirm: Rect,
}

/// Like Slack: lowercase, spaces become hyphens, at most 80 characters.
pub fn normalize(name: &str) -> String {
    name.trim()
        .chars()
        .flat_map(char::to_lowercase)
        .map(|c| if c.is_whitespace() { '-' } else { c })
        .filter(|c| c.is_alphanumeric() || matches!(c, '-' | '_'))
        .take(NAME_MAX)
        .collect()
}

/// Slack's error codes in words; anything else as it came.
fn friendly(e: &str) -> String {
    let code = e.rsplit(": ").next().unwrap_or(e);
    let text = match code {
        "name_taken" => tr("A channel with that name already exists.", "Já existe um canal com esse nome."),
        "invalid_name" | "invalid_name_specials" | "invalid_name_punctuation" => tr(
            "Use only lowercase letters, numbers, hyphens and underscores.",
            "Use só letras minúsculas, números, hífens e sublinhados.",
        ),
        "invalid_name_maxlength" => tr("The name can have at most 80 characters.", "O nome pode ter no máximo 80 caracteres."),
        "invalid_name_required" => tr("Give the channel a name.", "Dê um nome ao canal."),
        "restricted_action" | "missing_scope" | "not_allowed_token_type" => {
            tr("The workspace doesn't let you do that.", "O workspace não deixa você fazer isso.")
        }
        "user_not_found" | "user_disabled" => {
            tr("One of the people no longer exists or was deactivated.", "Uma das pessoas não existe mais ou foi desativada.")
        }
        "too_many_users" => tr("A group conversation has at most 8 other people.", "Uma conversa em grupo tem no máximo 8 outras pessoas."),
        "not_enough_users" => tr("Pick at least one person.", "Escolha pelo menos uma pessoa."),
        "cannot_dm_bot" => tr("You can't open a conversation with this app.", "Não dá para abrir conversa com esse app."),
        _ => return e.to_string(),
    };
    text.to_string()
}

/// The end of a field that no longer fits, so the cursor stays in view.
pub(crate) fn tail(s: &str, width: usize) -> String {
    if s.width() <= width {
        return s.to_string();
    }
    let mut out: Vec<char> = Vec::new();
    let mut w = 1;
    for ch in s.chars().rev() {
        let cw = ch.width().unwrap_or(0);
        if w + cw > width {
            break;
        }
        out.push(ch);
        w += cw;
    }
    std::iter::once('…').chain(out.into_iter().rev()).collect()
}

impl App {
    /// People to pick: no apps, deactivated accounts, yourself or those already
    /// picked; names (or handles) starting with the query first.
    pub fn candidates(&self, d: &NewMessage) -> Vec<String> {
        let q = d.query.trim();
        let handles = self.handles();
        let mut hits: Vec<(u8, String, &String)> = self
            .users
            .names
            .iter()
            .filter(|(id, _)| {
                !self.users.bots.contains(*id) && !self.users.deleted.contains(*id) && !self.is_me(id) && !d.to.contains(id)
            })
            .filter_map(|(id, name)| {
                let handle = handles.get(id.as_str()).copied().unwrap_or("");
                let rank = [name.as_str(), handle].iter().filter_map(|s| crate::text::search(s, q)).map(|(r, _)| r).min()?;
                Some((rank, name.to_lowercase(), id))
            })
            .collect();
        hits.sort();
        hits.into_iter().map(|(_, _, id)| id.clone()).collect()
    }

    /// id → handle, the reverse of `Users::handles`.
    fn handles(&self) -> HashMap<&str, &str> {
        self.users.handles.iter().map(|(h, id)| (id.as_str(), h.as_str())).collect()
    }

    /// The small menu under the button.
    pub fn open_compose_menu(&mut self) {
        let b = self.areas.compose.button;
        let items = vec![
            (tr("New message", "Nova mensagem").to_string(), Action::NewMessage),
            (tr("Create channel", "Criar canal").to_string(), Action::NewChannel),
        ];
        self.menu = Some(Menu { x: b.right().saturating_sub(17), y: b.bottom(), items, sel: 0, parent: None });
        self.dirty = true;
    }

    fn is_compose_menu(&self) -> bool {
        self.menu.as_ref().is_some_and(|m| matches!(m.items.first(), Some((_, Action::NewMessage))))
    }

    pub fn dialog_paste(&mut self, s: &str) {
        let line = s.lines().next().unwrap_or("").trim();
        match self.dialog.as_mut() {
            Some(Dialog::Message(m)) if !m.busy => {
                m.query.push_str(line);
                m.sel = 0;
            }
            Some(Dialog::Channel(c)) if !c.busy => {
                if let Some(t) = c.text() {
                    t.push_str(line);
                }
            }
            _ => {}
        }
    }

    pub fn dialog_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if k.code == KeyCode::Esc {
            self.dialog = None;
            return;
        }
        match self.dialog {
            Some(Dialog::Message(_)) => self.message_key(k, ctrl),
            Some(Dialog::Channel(_)) => self.channel_key(k, ctrl),
            None => {}
        }
    }

    fn message_key(&mut self, k: KeyEvent, ctrl: bool) {
        let picks = match &self.dialog {
            Some(Dialog::Message(m)) => self.candidates(m),
            _ => return,
        };
        let Some(Dialog::Message(m)) = self.dialog.as_mut() else { return };
        if m.busy {
            return;
        }
        m.sel = m.sel.min(picks.len().saturating_sub(1));
        // Ctrl+Enter (Ctrl+J where the terminal cannot tell them apart), or
        // Enter once the people are picked.
        let confirm = (ctrl && matches!(k.code, KeyCode::Enter | KeyCode::Char('j')))
            || (k.code == KeyCode::Enter && m.query.is_empty() && !m.to.is_empty() && !m.browsing);
        if confirm {
            if !m.query.is_empty() {
                if let Some(id) = picks.get(m.sel) {
                    pick(m, id);
                }
            }
            return self.submit_message();
        }
        match k.code {
            KeyCode::Enter => {
                if let Some(id) = picks.get(m.sel) {
                    pick(m, id);
                }
            }
            KeyCode::Up => {
                m.sel = m.sel.saturating_sub(1);
                m.browsing = true;
            }
            KeyCode::Down => {
                m.sel = (m.sel + 1).min(picks.len().saturating_sub(1));
                m.browsing = true;
            }
            KeyCode::Backspace if m.query.is_empty() => {
                m.to.pop();
                m.error = None;
            }
            KeyCode::Backspace => {
                m.query.pop();
                m.sel = 0;
            }
            KeyCode::Char('u') if ctrl => m.query.clear(),
            KeyCode::Char(c) if !ctrl => {
                m.query.push(c);
                m.sel = 0;
                m.browsing = false;
            }
            _ => {}
        }
    }

    fn channel_key(&mut self, k: KeyEvent, ctrl: bool) {
        let Some(Dialog::Channel(c)) = self.dialog.as_mut() else { return };
        if c.busy {
            return;
        }
        match k.code {
            KeyCode::Enter => return self.submit_channel(),
            KeyCode::Char('j') if ctrl => return self.submit_channel(),
            KeyCode::Tab | KeyCode::Down => c.field = (c.field + 1) % 3,
            KeyCode::BackTab | KeyCode::Up => c.field = (c.field + 2) % 3,
            KeyCode::Char(' ') | KeyCode::Left | KeyCode::Right if c.field == 1 => c.private = !c.private,
            KeyCode::Backspace => {
                if let Some(t) = c.text() {
                    t.pop();
                }
            }
            KeyCode::Char('u') if ctrl => {
                if let Some(t) = c.text() {
                    t.clear();
                }
            }
            KeyCode::Char(ch) if !ctrl => {
                if c.field == 0 && normalize(&c.name).chars().count() >= NAME_MAX {
                    return;
                }
                if let Some(t) = c.text() {
                    t.push(ch);
                }
            }
            _ => return,
        }
        c.error = None;
    }

    fn submit_message(&mut self) {
        let Some(Dialog::Message(m)) = self.dialog.as_mut() else { return };
        if m.to.is_empty() {
            m.error = Some(tr("Pick at least one person.", "Escolha pelo menos uma pessoa.").into());
            return;
        }
        m.busy = true;
        m.error = None;
        let users = m.to.clone();
        self.spawn(move |s| Event::Composed { res: s.open_conversation(&users), warning: None });
    }

    fn submit_channel(&mut self) {
        let Some(Dialog::Channel(c)) = self.dialog.as_mut() else { return };
        let name = normalize(&c.name);
        if name.is_empty() {
            c.error = Some(tr("Give the channel a name.", "Dê um nome ao canal.").into());
            c.field = 0;
            return;
        }
        c.busy = true;
        c.error = None;
        let (private, purpose) = (c.private, c.purpose.trim().to_string());
        self.spawn(move |s| {
            let res = s.create_channel(&name, private);
            let warning = match &res {
                Ok(c) if !purpose.is_empty() => s.set_purpose(&c.id, &purpose).err(),
                _ => None,
            };
            Event::Composed { res, warning }
        });
    }

    /// The conversation came back: into the sidebar and open, unless the
    /// dialog was closed while waiting.
    pub fn composed(&mut self, res: Res<Conversation>, warning: Option<String>) {
        let c = match res {
            Ok(c) => c,
            Err(e) => {
                let text = friendly(&e);
                match self.dialog.as_mut() {
                    Some(Dialog::Message(m)) => (m.busy, m.error) = (false, Some(text)),
                    Some(Dialog::Channel(c)) => (c.busy, c.error) = (false, Some(text)),
                    None => self.error = Some(text),
                }
                return;
            }
        };
        let id = c.id.clone();
        // conversations.open also opens it in Slack's sidebar.
        if matches!(c.kind, Kind::Direct | Kind::Group) {
            if let Some(b) = self.boot.as_mut() {
                b.open.insert(id.clone());
            }
        }
        // Without activity "hide inactive" would hide it once left.
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        self.counts.entry(id.clone()).or_insert_with(|| Count { latest: format!("{now}.000000"), ..Count::default() });
        if !self.convs.iter().any(|x| x.id == id) {
            self.convs.push(c);
            self.resort();
        }
        if let Some(w) = warning {
            self.error = Some(trf!("channel created, but the description wasn't saved: {w}", "canal criado, mas a descrição não foi salva: {w}"));
        }
        if self.dialog.take().is_some() {
            self.open(id);
        }
        self.dirty = true;
    }

    /// Hover and clicks on the button and the dialogs; true when handled.
    pub fn compose_mouse(&mut self, m: MouseEvent) -> bool {
        let at = Position::new(m.column, m.row);
        let a = &self.areas.compose;
        let hover = self.dialog.is_none() && a.button.contains(at);
        let outside = !a.dialog.contains(at);
        let result = a.results.iter().find(|(r, _)| r.contains(at)).map(|(_, i)| *i);
        let chip = a.chips.iter().find(|(r, _)| r.contains(at)).map(|(_, i)| *i);
        let field = a.fields.iter().find(|(r, _)| r.contains(at)).map(|(_, i)| *i);
        let confirm = a.confirm.contains(at);
        if hover != self.compose_hover {
            self.compose_hover = hover;
            self.dirty = true;
        }
        let left = m.kind == MouseEventKind::Down(MouseButton::Left);
        if self.dialog.is_none() {
            if left && hover {
                if self.is_compose_menu() {
                    self.menu = None;
                } else {
                    self.open_compose_menu();
                }
                self.dirty = true;
                return true;
            }
            return false;
        }
        let picks = match &self.dialog {
            Some(Dialog::Message(d)) => self.candidates(d),
            _ => Vec::new(),
        };
        match (m.kind, self.dialog.as_mut()) {
            (MouseEventKind::Down(_), _) if outside => self.dialog = None,
            (MouseEventKind::Moved, Some(Dialog::Message(d))) => match result {
                Some(i) if i != d.sel => d.sel = i,
                _ => return true,
            },
            (MouseEventKind::ScrollUp, Some(Dialog::Message(d))) => d.sel = d.sel.saturating_sub(3),
            (MouseEventKind::ScrollDown, Some(Dialog::Message(d))) => d.sel = (d.sel + 3).min(picks.len().saturating_sub(1)),
            (MouseEventKind::Down(MouseButton::Left), Some(Dialog::Message(d))) if !d.busy => {
                if confirm {
                    self.submit_message();
                } else if let Some(id) = result.and_then(|i| picks.get(i)) {
                    pick(d, id);
                } else if let Some(i) = chip {
                    d.to.remove(i);
                    d.error = None;
                }
            }
            (MouseEventKind::Down(MouseButton::Left), Some(Dialog::Channel(c))) if !c.busy => {
                if confirm {
                    self.submit_channel();
                } else if let Some(f) = field {
                    c.field = f;
                    if f == 1 {
                        c.private = !c.private;
                    }
                }
            }
            _ => return true,
        }
        self.dirty = true;
        true
    }
}

fn name_of(app: &App, uid: &str) -> String {
    app.users.names.get(uid).cloned().unwrap_or_else(|| uid.to_string())
}

fn pick(m: &mut NewMessage, id: &str) {
    if m.to.len() >= MAX_PEOPLE {
        m.error = Some(trf!("At most {MAX_PEOPLE} people in a conversation.", "No máximo {MAX_PEOPLE} pessoas numa conversa."));
        return;
    }
    m.to.push(id.to_string());
    m.query.clear();
    m.sel = 0;
    m.browsing = false;
    m.error = None;
}

// ---- drawing -------------------------------------------------------------

pub fn draw(f: &mut Frame, app: &mut App) {
    let a = &mut app.areas.compose;
    a.dialog = Rect::default();
    a.confirm = Rect::default();
    a.results.clear();
    a.chips.clear();
    a.fields.clear();
    match app.dialog {
        Some(Dialog::Message(_)) => draw_message(f, app),
        Some(Dialog::Channel(_)) => draw_channel(f, app),
        None => {}
    }
}

/// A centred, bordered box like the image viewer's; returns its inside.
fn frame(f: &mut Frame, app: &mut App, title: &str, w: u16, h: u16) -> Rect {
    let area = f.area();
    let (w, h) = (w.min(area.width), h.min(area.height));
    let rect = Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h);
    f.render_widget(Clear, rect);
    let block = Block::bordered().border_type(BorderType::Rounded).border_style(dim()).title(Line::from(format!(" {title} ")).bold());
    let inner = block.inner(rect).inner(ratatui::layout::Margin { horizontal: 1, vertical: 0 });
    f.render_widget(block, rect);
    app.areas.compose.dialog = rect;
    inner
}

/// The bottom two rows: the error, then the hint and the button on the right.
fn footer(f: &mut Frame, app: &mut App, inner: Rect, error: Option<&str>, hint: &str, button: &str, ready: bool) {
    let y = inner.bottom().saturating_sub(2);
    if let Some(e) = error {
        f.render_widget(Paragraph::new(truncate(e, inner.width as usize)).style(Style::new().fg(Color::Yellow)), Rect::new(inner.x, y, inner.width, 1));
    }
    let bw = button.width() as u16;
    let style = if ready { Style::new().fg(Color::Black).bg(Color::Green).bold() } else { Style::new().fg(Color::Black).bg(Color::DarkGray) };
    let b = Rect::new(inner.right().saturating_sub(bw), y + 1, bw.min(inner.width), 1);
    f.render_widget(Paragraph::new(Span::styled(button, style)), b);
    app.areas.compose.confirm = b;
    let room = inner.width.saturating_sub(bw + 1) as usize;
    f.render_widget(Paragraph::new(truncate(hint, room)).style(dim()), Rect::new(inner.x, y + 1, room as u16, 1));
}

fn draw_message(f: &mut Frame, app: &mut App) {
    let Some(Dialog::Message(d)) = &app.dialog else { return };
    let picks = app.candidates(d);
    let (query, to, busy, error) = (d.query.clone(), d.to.clone(), d.busy, d.error.clone());
    let sel = d.sel.min(picks.len().saturating_sub(1));
    let area = f.area();
    let inner = frame(f, app, tr("New message", "Nova mensagem"), 64.min(area.width.saturating_sub(4)), 22.min(area.height.saturating_sub(2)));
    if inner.width < 24 || inner.height < 8 {
        return;
    }

    // "Para:" and the people picked, wrapping, then what is being typed.
    let width = inner.width as usize;
    const LEAD: usize = 6;
    let mut lines = Vec::new();
    let mut spans = vec![Span::styled(tr("To: ", "Para: "), dim())];
    let (mut x, mut row) = (LEAD, 0u16);
    for (i, uid) in to.iter().enumerate() {
        let label = format!(" {} × ", truncate(&name_of(app, uid), width.saturating_sub(LEAD + 4)));
        let lw = label.width();
        if x + lw > width && x > LEAD {
            lines.push(Line::from(std::mem::replace(&mut spans, vec![Span::raw(" ".repeat(LEAD))])));
            (x, row) = (LEAD, row + 1);
        }
        app.areas.compose.chips.push((Rect::new(inner.x + x as u16, inner.y + row, lw as u16, 1), i));
        spans.push(Span::styled(label, Style::new().fg(Color::Black).bg(Color::Cyan)));
        spans.push(Span::raw(" "));
        x += lw + 1;
    }
    let shown = tail(&query, width.saturating_sub(LEAD + 1));
    if x + shown.width() + 1 > width && x > LEAD {
        lines.push(Line::from(std::mem::replace(&mut spans, vec![Span::raw(" ".repeat(LEAD))])));
        (x, row) = (LEAD, row + 1);
    }
    let cursor = (inner.x + (x + shown.width()) as u16, inner.y + row);
    if query.is_empty() && to.is_empty() {
        spans.push(Span::styled(tr("type a name", "digite um nome"), dim()));
    } else {
        spans.push(Span::raw(shown));
    }
    lines.push(Line::from(spans));
    let head = (lines.len() as u16).min(inner.height.saturating_sub(5));
    f.render_widget(Paragraph::new(lines), Rect::new(inner.x, inner.y, inner.width, head));
    if !busy && cursor.1 < inner.y + head {
        f.set_cursor_position(cursor);
    }
    f.render_widget(Paragraph::new("─".repeat(width)).style(dim()), Rect::new(inner.x, inner.y + head, inner.width, 1));

    // The people found, the highlighted one kept in view.
    let top = inner.y + head + 1;
    let list_h = inner.bottom().saturating_sub(top + 3) as usize;
    let photos = app.sidebar_avatars && app.picker.is_some();
    if picks.is_empty() {
        let text = if app.users.names.is_empty() { tr("loading people…", "carregando pessoas…") } else { tr("No one found", "Ninguém encontrado") };
        f.render_widget(Paragraph::new(format!(" {text}")).style(dim()), Rect::new(inner.x, top, inner.width, 1));
    }
    let handles: HashMap<String, String> = app.handles().into_iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    let offset = (sel + 1).saturating_sub(list_h);
    for (n, uid) in picks.iter().enumerate().skip(offset).take(list_h) {
        let y = top + (n - offset) as u16;
        let rect = Rect::new(inner.x, y, inner.width, 1);
        let icon = if photos {
            Span::raw("  ")
        } else if app.presence.get(uid) == Some(&true) {
            Span::styled(ONLINE, Style::new().fg(Color::Green))
        } else {
            Span::styled(OFFLINE, dim())
        };
        let name = name_of(app, uid);
        let handle = handles.get(uid).map(|h| format!("  @{h}")).unwrap_or_default();
        let room = width.saturating_sub(4 + name.width().min(width));
        let mut line = Line::from(vec![
            Span::raw(" "),
            icon,
            Span::raw(" "),
            Span::styled(truncate(&name, width.saturating_sub(4)), Style::new().bold()),
            Span::styled(truncate(&handle, room), dim()),
        ]);
        if n == sel {
            line = line.style(Style::new().add_modifier(Modifier::REVERSED));
        }
        f.render_widget(Paragraph::new(line), rect);
        app.areas.compose.results.push((rect, n));
        if let Some(url) = app.users.avatars.get(uid).cloned().filter(|_| photos) {
            draw_image(f, app, &ImgRef::avatar(uid, &url), Rect::new(inner.x + 1, y, 2, 1));
        }
    }

    let hint = if to.is_empty() {
        tr("Enter adds · Esc closes", "Enter adiciona · Esc fecha")
    } else {
        tr("Enter adds · Ctrl+Enter opens · Esc closes", "Enter adiciona · Ctrl+Enter abre · Esc fecha")
    };
    let button = if busy { tr(" Opening… ", " Abrindo… ") } else { tr(" Open conversation ", " Abrir conversa ") };
    footer(f, app, inner, error.as_deref(), hint, button, !to.is_empty() && !busy);
}

fn draw_channel(f: &mut Frame, app: &mut App) {
    let Some(Dialog::Channel(c)) = &app.dialog else { return };
    let (name, purpose, private, field, busy, error) = (c.name.clone(), c.purpose.clone(), c.private, c.field, c.busy, c.error.clone());
    let area = f.area();
    let inner = frame(f, app, tr("Create channel", "Criar canal"), 60.min(area.width.saturating_sub(4)), 15.min(area.height.saturating_sub(2)));
    if inner.width < 30 || inner.height < 11 {
        return;
    }
    let width = inner.width as usize;
    let room = width.saturating_sub(LABEL_W + 3);
    let marker = |f: usize| if f == field { Span::styled("› ", Style::new().fg(Color::Cyan).bold()) } else { Span::raw("  ") };
    let label = |s: &str| Span::styled(format!("{s:<w$}", w = LABEL_W), Style::new().bold());
    let value = |s: &str, placeholder: &str| {
        if s.is_empty() { Span::styled(placeholder.to_string(), dim()) } else { Span::raw(tail(s, room)) }
    };

    let normalized = normalize(&name);
    let preview = if normalized.is_empty() {
        Span::raw("")
    } else {
        let sign = if private { format!("{LOCK} ") } else { "#".into() };
        Span::styled(truncate(&format!("{sign}{normalized}"), room), Style::new().fg(Color::Cyan))
    };
    let (toggle, toggle_style) = if private { (TOGGLE_ON, Style::new().fg(Color::Green)) } else { (TOGGLE_OFF, dim()) };
    let about = if private {
        tr("only people who are invited can see the channel", "só quem for convidado vê o canal")
    } else {
        tr("anyone in the workspace can join", "qualquer pessoa do workspace pode entrar")
    };
    let pad = Span::raw(" ".repeat(LABEL_W + 2));
    let lines = vec![
        Line::from(""),
        Line::from(vec![marker(0), label(tr("Name", "Nome")), value(&name, tr("e.g. new-project", "ex.: projeto-novo"))]),
        Line::from(vec![pad.clone(), preview]),
        Line::from(""),
        Line::from(vec![marker(1), label(tr("Private", "Privado")), Span::styled(toggle, toggle_style), Span::raw(" "), Span::styled(truncate(about, room.saturating_sub(2)), dim())]),
        Line::from(""),
        Line::from(vec![marker(2), label(tr("Description", "Descrição")), value(&purpose, tr("(optional)", "(opcional)"))]),
    ];
    f.render_widget(Paragraph::new(lines), Rect::new(inner.x, inner.y, inner.width, 7));
    for (f_idx, dy) in [(0, 1), (1, 4), (2, 6)] {
        app.areas.compose.fields.push((Rect::new(inner.x, inner.y + dy, inner.width, 1), f_idx));
    }
    let text = match field {
        0 => Some((&name, 1)),
        2 => Some((&purpose, 6)),
        _ => None,
    };
    if let (Some((s, dy)), false) = (text, busy) {
        f.set_cursor_position((inner.x + (LABEL_W + 2 + tail(s, room).width()) as u16, inner.y + dy));
    }

    let hint = tr("Tab fields · Space toggles · Enter creates · Esc closes", "Tab campos · Espaço alterna · Enter cria · Esc fecha");
    let button = if busy { tr(" Creating… ", " Criando… ") } else { tr(" Create ", " Criar ") };
    footer(f, app, inner, error.as_deref(), hint, button, !normalized.is_empty() && !busy);
}
