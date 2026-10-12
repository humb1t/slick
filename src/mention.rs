//! The `@` and `#` autocomplete in the message box, like Slack's: people or
//! channels filtered as you type, put in as pills that Slack gets as
//! `<@U1>` and `<#C1>`.

use crate::app::{App, ImgRef};
use crate::editor::{is_token, Token};
use crate::i18n::tr;
use crate::slack::Kind;
use crate::text::search;
use crate::ui::{dim, draw_image, truncate};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use ratatui::Frame;
use std::collections::HashMap;
use unicode_width::UnicodeWidthStr;

const ONLINE: &str = "\u{f111}";
const OFFLINE: &str = "\u{f10c}";
const LOCK: &str = "\u{f023}";
/// Rows in the popup.
const ROWS: usize = 8;
/// Past this many characters after the `@` it is just text.
const MAX_QUERY: usize = 30;
/// The broadcasts Slack offers in channels, after the people.
const SPECIAL: [&str; 3] = ["here", "channel", "everyone"];

/// The popup's state, like the `:` one's.
#[derive(Default)]
pub struct Mention {
    pub sel: usize,
    /// The fragment it was last shown for; a new one starts at the top.
    last: String,
    /// Esc hides it until the fragment changes.
    dismissed: Option<String>,
    /// Where the last draw put it and its rows, for the mouse.
    rect: Rect,
    rows: Vec<(Rect, usize)>,
}

#[derive(Clone)]
enum Icon {
    User(String),
    Channel { private: bool },
    Special,
}

/// A row of the popup and the pill it puts in.
#[derive(Clone)]
pub struct Hit {
    icon: Icon,
    /// In bold.
    name: String,
    /// Dimmed after the name: the full name, or what a broadcast does.
    detail: String,
    token: Token,
}

/// The `@query` or `#query` right before the cursor, like Slack's: the sign
/// starts a word, `#` names have no spaces, and a space at the end (or two
/// in a row) means the name is over. The sign's index (chars), the sign and
/// the query.
fn fragment(text: &str, cursor: usize) -> Option<(usize, char, String)> {
    let before: Vec<char> = text.chars().take(cursor).collect();
    let mut i = before.len();
    while i > 0 && !matches!(before[i - 1], '@' | '#') {
        let c = before[i - 1];
        if c == '\n' || is_token(c) || before.len() - i >= MAX_QUERY {
            return None;
        }
        i -= 1;
    }
    if i == 0 || (i >= 2 && !before[i - 2].is_whitespace()) {
        return None;
    }
    let sign = before[i - 1];
    let query: String = before[i..].iter().collect();
    let spaced = query.starts_with(' ') || query.ends_with(' ') || query.contains("  ") || query.matches(' ').count() > 2;
    let ok = if sign == '#' { !query.contains(char::is_whitespace) } else { !spaced };
    ok.then_some((i - 1, sign, query))
}

impl App {
    /// The popup's rows, while an `@name` or `#channel` is being typed.
    pub fn mentions(&mut self) -> Option<Vec<Hit>> {
        let ed = self.typing_editor()?;
        let (_, sign, query) = fragment(&ed.text, ed.cursor)?;
        let frag = format!("{sign}{query}");
        if self.mention.dismissed.as_deref() == Some(frag.as_str()) {
            return None;
        }
        self.mention.dismissed = None;
        if self.mention.last != frag {
            self.mention.sel = 0;
            self.mention.last = frag;
        }
        let hits = if sign == '@' { self.people(&query) } else { self.channels(&query) };
        self.mention.sel = self.mention.sel.min(hits.len().saturating_sub(1));
        (!hits.is_empty()).then_some(hits)
    }

    /// How recently each conversation was opened: 0 is the latest.
    fn recency(&self) -> HashMap<&str, usize> {
        self.recents.iter().enumerate().map(|(i, id)| (id.as_str(), i)).collect()
    }

    /// People whose display name, full name or handle matches: best match
    /// first, then the ones talked to lately, apps last. In a channel the
    /// broadcasts follow when the query starts one.
    fn people(&self, q: &str) -> Vec<Hit> {
        let recency = self.recency();
        let talked: HashMap<&str, usize> = self
            .convs
            .iter()
            .filter(|c| c.kind == Kind::Direct)
            .filter_map(|c| Some((c.user.as_deref()?, *recency.get(c.id.as_str())?)))
            .collect();
        let handles: HashMap<&str, &str> = self.users.handles.iter().map(|(h, id)| (id.as_str(), h.as_str())).collect();
        let q = q.trim();
        let mut found: Vec<_> = self
            .users
            .names
            .iter()
            .filter(|(id, _)| !self.users.deleted.contains(*id))
            .filter_map(|(id, name)| {
                let real = self.users.real.get(id).map(String::as_str).unwrap_or("");
                let handle = handles.get(id.as_str()).copied().unwrap_or("");
                let rank = [name.as_str(), real, handle].iter().filter_map(|s| search(s, q)).map(|(r, _)| r).min()?;
                let bot = self.users.bots.contains(id);
                Some(((rank, bot, talked.get(id.as_str()).copied().unwrap_or(usize::MAX), name.to_lowercase()), id, name, real, handle))
            })
            .collect();
        found.sort_by(|a, b| a.0.cmp(&b.0));
        let in_channel = self.current.as_ref().and_then(|id| self.convs.iter().find(|c| &c.id == id)).is_some_and(|c| matches!(c.kind, Kind::Channel | Kind::Private));
        let lower = q.to_lowercase();
        let specials: Vec<Hit> = SPECIAL
            .iter()
            .filter(|s| in_channel && !lower.is_empty() && s.starts_with(&lower))
            .map(|s| Hit {
                icon: Icon::Special,
                name: format!("@{s}"),
                detail: match *s {
                    "here" => tr("Notify everyone online in this channel", "Avisa quem está online no canal"),
                    "channel" => tr("Notify everyone in this channel", "Avisa todos do canal"),
                    _ => tr("Notify everyone in the workspace", "Avisa todos do workspace"),
                }
                .to_string(),
                token: Token::Ref { label: format!("@{s}"), raw: format!("<!{s}>") },
            })
            .collect();
        let mut hits: Vec<Hit> = found
            .into_iter()
            .take(ROWS - specials.len())
            .map(|(_, id, name, real, handle)| {
                let detail = if !real.is_empty() && real != name { real } else { handle };
                Hit {
                    icon: Icon::User(id.clone()),
                    name: name.clone(),
                    detail: detail.to_string(),
                    token: Token::Ref { label: format!("@{name}"), raw: format!("<@{id}>") },
                }
            })
            .collect();
        hits.extend(specials);
        hits
    }

    /// Channels you are in whose name matches, best match then latest first.
    fn channels(&self, q: &str) -> Vec<Hit> {
        let recency = self.recency();
        let mut found: Vec<_> = self
            .convs
            .iter()
            .filter(|c| matches!(c.kind, Kind::Channel | Kind::Private))
            .filter_map(|c| {
                let (rank, _) = search(&c.name, q)?;
                Some(((rank, recency.get(c.id.as_str()).copied().unwrap_or(usize::MAX), c.name.clone()), c))
            })
            .collect();
        found.sort_by(|a, b| a.0.cmp(&b.0));
        found
            .into_iter()
            .take(ROWS)
            .map(|(_, c)| Hit {
                icon: Icon::Channel { private: c.kind == Kind::Private },
                name: c.name.clone(),
                detail: String::new(),
                token: Token::Ref { label: format!("#{}", c.name), raw: format!("<#{}>", c.id) },
            })
            .collect()
    }

    /// Replaces the `@fragment` with the pill and a space.
    fn mention_with(&mut self, hit: Hit) {
        let Some(ed) = self.typing_editor_mut() else { return };
        let Some((start, ..)) = fragment(&ed.text, ed.cursor) else { return };
        ed.complete(start, hit.token);
    }

    /// Keys for the popup; true when it took the key.
    pub fn mention_key(&mut self, k: KeyEvent) -> bool {
        let Some(hits) = self.mentions() else { return false };
        let n = hits.len();
        let plain = !k.modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::ALT | KeyModifiers::CONTROL);
        let sel = self.mention.sel.min(n - 1);
        match k.code {
            KeyCode::Up => self.mention.sel = (sel + n - 1) % n,
            KeyCode::Down => self.mention.sel = (sel + 1) % n,
            KeyCode::Enter | KeyCode::Tab if plain => self.mention_with(hits[sel].clone()),
            KeyCode::Esc => self.mention.dismissed = Some(self.mention.last.clone()),
            _ => return false,
        }
        true
    }

    /// Hover, the wheel and clicks on the popup; true when taken.
    pub fn mention_mouse(&mut self, m: MouseEvent) -> bool {
        let at = Position::new(m.column, m.row);
        if !self.mention.rect.contains(at) {
            return false;
        }
        let Some(hits) = self.mentions() else { return false };
        let n = hits.len();
        let row = self.mention.rows.iter().find(|(r, _)| r.contains(at)).map(|(_, i)| *i);
        match m.kind {
            MouseEventKind::Moved => match row {
                Some(i) if i != self.mention.sel => self.mention.sel = i,
                _ => return true,
            },
            MouseEventKind::ScrollUp => self.mention.sel = self.mention.sel.saturating_sub(1),
            MouseEventKind::ScrollDown => self.mention.sel = (self.mention.sel + 1).min(n - 1),
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(hit) = row.and_then(|i| hits.get(i).cloned()) {
                    self.mention_with(hit);
                }
            }
            _ => {}
        }
        self.dirty = true;
        true
    }
}

/// The popup, just above the box being typed in.
pub fn draw(f: &mut Frame, app: &mut App) {
    app.mention.rect = Rect::default();
    app.mention.rows.clear();
    let Some(hits) = app.mentions() else { return };
    let input = app.typing_area();
    let widest = hits.iter().map(|h| h.name.width() + h.detail.width() + 2).max().unwrap_or(0) as u16;
    let w = (widest + 8).clamp(30, 64).min(input.width.saturating_sub(2));
    let h = hits.len() as u16 + 2;
    if input.y < h || w < 16 {
        return;
    }
    let rect = Rect::new(input.x + 1, input.y - h, w, h);
    f.render_widget(Clear, rect);
    let block = Block::bordered().border_type(BorderType::Rounded).border_style(dim());
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    app.mention.rect = rect;
    let photos = app.sidebar_avatars && app.picker.is_some();
    let width = inner.width as usize;
    let sel = app.mention.sel;
    for (n, hit) in hits.iter().enumerate() {
        let row = Rect::new(inner.x, inner.y + n as u16, inner.width, 1);
        let mut photo = None;
        let icon = match &hit.icon {
            Icon::User(uid) => match app.users.avatars.get(uid).filter(|_| photos) {
                Some(url) => {
                    photo = Some(ImgRef::avatar(uid, url));
                    Span::raw("  ")
                }
                None => match app.presence.get(uid) {
                    Some(true) => Span::styled(format!("{ONLINE} "), Style::new().fg(Color::Green)),
                    Some(false) => Span::styled(format!("{OFFLINE} "), dim()),
                    None => Span::raw("  "),
                },
            },
            Icon::Channel { private: true } => Span::styled(format!("{LOCK} "), dim()),
            Icon::Channel { private: false } => Span::styled("# ", dim()),
            Icon::Special => Span::styled("@ ", dim()),
        };
        // With a photo, the presence goes after the name, like Slack's dot.
        let dot = match &hit.icon {
            Icon::User(uid) if photo.is_some() => match app.presence.get(uid) {
                Some(true) => Span::styled(format!(" {ONLINE}"), Style::new().fg(Color::Green)),
                Some(false) => Span::styled(format!(" {OFFLINE}"), dim()),
                None => Span::raw(""),
            },
            _ => Span::raw(""),
        };
        let name = truncate(&hit.name, width.saturating_sub(5));
        let room = width.saturating_sub(4 + name.width() + dot.content.width());
        let detail = if hit.detail.is_empty() { String::new() } else { truncate(&format!("  {}", hit.detail), room) };
        let mut line = Line::from(vec![Span::raw(" "), icon, Span::raw(" "), Span::styled(name, Style::new().bold()), dot, Span::styled(detail, dim())]);
        let pad = width.saturating_sub(line.width());
        line.spans.push(Span::raw(" ".repeat(pad)));
        if n == sel {
            line = line.style(Style::new().add_modifier(Modifier::REVERSED));
        }
        f.render_widget(Paragraph::new(line), row);
        app.mention.rows.push((row, n));
        if let Some(img) = photo {
            draw_image(f, app, &img, Rect::new(row.x + 1, row.y, 2, 1));
        }
    }
}
