//! A sidebar section's name and emoji: Slack's "Rename this section", and
//! the same dialog for a new one ("Create new section"), which can take the
//! conversation it was asked for along. And its "Section settings" → Filter.

use crate::app::{App, Event};
use crate::compose::tail;
use crate::emoji::{self, Glyph};
use crate::i18n::tr;
use crate::slack::{Conversation, Res, Section};
use serde_json::{json, Value};
use crate::ui::{dim, draw_image, emoji_row, truncate};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Margin, Position, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

/// Slack's limit for a section's name.
const NAME_MAX: usize = 80;

pub enum Target {
    /// The section's id.
    Rename(String),
    /// A new section: the conversation to move into it, and the section it
    /// goes right before.
    New { chan: Option<String>, next: String },
}

pub struct Dialog {
    target: Target,
    name: String,
    /// A shortcode without colons; empty for none.
    emoji: String,
    /// 0: the emoji button, 1: the name.
    field: usize,
    /// The emoji picker's query and row, while it is open.
    picker: Option<(String, usize)>,
    saving: bool,
}

/// The sidebar's "Show" once a section has a filter of its own: "Custom,
/// depending on the section".
const CUSTOM: &str = "hide_read_channels_unless_starred";

/// A section's filters as Slack names them: Active only, Unreads, Mentions, All.
pub const FILTERS: [&str; 4] = ["active", "hid", "mentions", "all"];

/// How long Slack takes to call a conversation without messages dormant.
const DORMANT_SECS: f64 = 30.0 * 86400.0;

pub fn filter_name(f: &str) -> &'static str {
    match f {
        "active" => tr("Active only", "Somente ativas"),
        "hid" => tr("Unreads", "Não lidas"),
        "mentions" => tr("Mentions", "Menções"),
        _ => tr("All", "Todas"),
    }
}

/// The filters a section offers: Mentions isn't one for Direct messages.
pub fn filters_for(kind: &str) -> Vec<&'static str> {
    FILTERS.into_iter().filter(|f| kind != "direct_messages" || *f != "mentions").collect()
}

/// A section's settings in the `channel_sections` pref, made an object if
/// it wasn't one.
fn entry<'a>(prefs: &'a mut serde_json::Map<String, Value>, id: &str) -> &'a mut serde_json::Map<String, Value> {
    let e = prefs.entry(id).or_insert_with(|| json!({}));
    if !e.is_object() {
        *e = json!({});
    }
    match e {
        Value::Object(o) => o,
        _ => unreachable!(),
    }
}

pub enum Ev {
    /// A filter saved, or not: then the sidebar's prefs go back to `old`.
    Filtered { old: (String, serde_json::Map<String, Value>), res: Res<()> },
    Renamed { id: String, name: String, emoji: String, res: Res<()> },
    Created { name: String, emoji: String, chan: Option<String>, next: String, res: Res<String> },
}

#[derive(Default)]
pub struct Areas {
    dialog: Rect,
    emoji: Rect,
    text: Rect,
    save: Rect,
    cancel: Rect,
    picker: Rect,
    picker_rows: Vec<(Rect, usize)>,
}

impl App {
    /// A paid team, or one whose plan isn't known yet: only those set the
    /// sidebar per section.
    fn paid_team(&self) -> bool {
        self.boot.as_ref().and_then(|b| b.plan.as_deref()).is_none_or(|p| !p.is_empty())
    }

    fn sidebar_behavior(&self) -> &str {
        self.boot.as_ref().map_or("", |b| b.sidebar_behavior.as_str())
    }

    /// The section's own filter, when the sidebar is set per section.
    fn section_own_show(&self, id: &str) -> Option<&'static str> {
        if self.sidebar_behavior() != CUSTOM || !self.paid_team() {
            return None;
        }
        let own = self.boot.as_ref()?.section_prefs.get(id)?["sidebar"].as_str()?;
        FILTERS.into_iter().find(|f| *f == own)
    }

    /// What the global "Show" gives a section of this kind (Slack's
    /// getChannelSectionSidebarBehavior).
    fn section_default_show(&self, kind: &str) -> &'static str {
        match self.sidebar_behavior() {
            "hide_inactive_channels" => "active",
            "hide_read_channels" => "hid",
            "show_mentions_only" if kind == "direct_messages" => "hid",
            "show_mentions_only" => "mentions",
            CUSTOM if self.paid_team() => match kind {
                "channels" | "direct_messages" | "org_channels" | "recent_apps" | "shared_channels" | "slack_connect" => "hid",
                _ => "active",
            },
            _ => "all",
        }
    }

    /// What a section shows now: one of `FILTERS`.
    pub(crate) fn section_show(&self, id: &str, kind: &str) -> &'static str {
        match self.section_own_show(id) {
            Some("mentions") if kind == "direct_messages" => "hid",
            Some(own) => own,
            None => self.section_default_show(kind),
        }
    }

    /// Whether a section's filter keeps a conversation: what's open, unread
    /// or mentions you always stays (the open one is also the only one with
    /// a draft); "Active only" also keeps what was opened here lately and
    /// what isn't dormant.
    pub(crate) fn section_keeps(&self, c: &Conversation, show: &str) -> bool {
        if show == "all" || self.current.as_deref() == Some(c.id.as_str()) {
            return true;
        }
        let (unread, mentions) = self.unread(&c.id);
        match show {
            _ if mentions > 0 => true,
            "mentions" => false,
            _ if unread => true,
            "hid" => false,
            _ => self.recents.contains(&c.id) || !self.dormant(c),
        }
    }

    /// Flagged dormant, and no message since Slack flagged it (the flag is
    /// as old as the list of conversations).
    fn dormant(&self, c: &Conversation) -> bool {
        if !c.dormant {
            return false;
        }
        let latest = self.counts.get(&c.id).and_then(|k| k.latest.parse::<f64>().ok()).unwrap_or(0.0);
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
        now - latest >= DORMANT_SECS
    }

    /// A section's conversations its filter keeps. Starred keeps what
    /// "Active only" would take unless the section itself asked for it: the
    /// user put them there.
    pub(crate) fn section_filtered(&self, id: &str, kind: &str, members: &[usize]) -> Vec<usize> {
        let show = self.section_show(id, kind);
        if kind == "stars" && show == "active" && self.section_own_show(id).is_none() {
            return members.to_vec();
        }
        members.iter().copied().filter(|&i| self.section_keeps(&self.convs[i], show)).collect()
    }

    /// The section's menu offers its Filter: a paid team's, for one of the
    /// sections Slack keeps.
    pub(crate) fn can_filter(&self, id: &str) -> bool {
        self.boot.is_some() && self.paid_team() && self.sections.iter().any(|s| s.id == id && s.kind != "priority")
    }

    /// Slack's setChannelSectionPrefByApi: the filter goes into the
    /// `channel_sections` pref; once a section's differs from what the
    /// global "Show" gives it, the sidebar turns "Custom, depending on the
    /// section", every section keeping what it showed. Here first, Slack's after.
    pub(crate) fn set_section_filter(&mut self, id: String, value: &'static str) {
        let Some(boot) = self.boot.as_ref() else { return };
        let old = (boot.sidebar_behavior.clone(), boot.section_prefs.clone());
        let mut prefs = old.1.clone();
        entry(&mut prefs, &id).insert("sidebar".into(), json!(value));
        let kind_of = |k: &str| self.sections.iter().find(|s| s.id == k).map(|s| s.kind.as_str());
        let differs = prefs.iter().any(|(k, p)| {
            kind_of(k).is_some_and(|kind| p["sidebar"].as_str().is_some_and(|v| v != self.section_default_show(kind)))
        });
        let to_custom = differs && old.0 != CUSTOM;
        if to_custom {
            for s in &self.sections {
                entry(&mut prefs, &s.id).entry("sidebar").or_insert_with(|| json!(self.section_default_show(&s.kind)));
            }
        }
        // Slack keeps only the known sections' non-empty entries.
        prefs.retain(|k, p| kind_of(k).is_some() && p.as_object().is_some_and(|o| !o.is_empty()));
        let text = Value::Object(prefs.clone()).to_string();
        if let Some(b) = self.boot.as_mut() {
            b.section_prefs = prefs;
            if to_custom {
                b.sidebar_behavior = CUSTOM.into();
            }
        }
        self.dirty = true;
        self.spawn(move |s| {
            let res = if to_custom { s.set_pref("sidebar_behavior", CUSTOM) } else { Ok(()) };
            let res = res.and_then(|_| s.set_pref("channel_sections", &text));
            Event::Section(Ev::Filtered { old, res })
        });
    }

    pub(crate) fn rename_section(&mut self, id: String) {
        let Some(s) = self.sections.iter().find(|s| s.id == id) else { return };
        let (name, emoji) = (s.name.clone(), s.emoji.trim_matches(':').to_string());
        self.section = Some(Dialog { target: Target::Rename(id), name, emoji, field: 1, picker: None, saving: false });
    }

    /// Like Slack, the new section goes right above `before` (the one whose
    /// menu made it), else above Channels.
    pub(crate) fn new_section(&mut self, chan: Option<String>, before: Option<String>) {
        let next = before
            .filter(|b| self.sections.iter().any(|s| &s.id == b))
            .or_else(|| self.sections.iter().find(|s| s.kind == "channels").map(|s| s.id.clone()))
            .unwrap_or_default();
        let target = Target::New { chan, next };
        self.section = Some(Dialog { target, name: String::new(), emoji: String::new(), field: 1, picker: None, saving: false });
    }

    /// The custom section a conversation is in, as the header's button shows it.
    pub(crate) fn section_of_conv(&self, chan: &str) -> Option<&Section> {
        self.sections.iter().find(|s| s.kind == "standard" && s.ids.iter().any(|c| c == chan))
    }

    fn save_section(&mut self) {
        let Some(d) = self.section.as_mut().filter(|d| !d.saving) else { return };
        let name = d.name.trim().to_string();
        if name.is_empty() {
            return;
        }
        d.saving = true;
        let emoji = d.emoji.clone();
        match &d.target {
            Target::Rename(id) => {
                let id = id.clone();
                let at = self.sections.iter().position(|s| s.id == id);
                let next = at.and_then(|i| self.sections.get(i + 1)).map(|s| s.id.clone()).unwrap_or_default();
                self.spawn(move |s| {
                    let res = s.rename_section(&id, &name, &emoji, &next);
                    Event::Section(Ev::Renamed { id, name, emoji, res })
                });
            }
            Target::New { chan, next } => {
                let (chan, next) = (chan.clone(), next.clone());
                self.spawn(move |s| {
                    let res = s.create_section(&name, &emoji, &next);
                    Event::Section(Ev::Created { name, emoji, chan, next, res })
                });
            }
        }
    }

    pub(crate) fn on_section(&mut self, ev: Ev) {
        match ev {
            Ev::Filtered { res: Ok(()), .. } => {}
            Ev::Filtered { old, res: Err(e) } => {
                if let Some(b) = self.boot.as_mut() {
                    (b.sidebar_behavior, b.section_prefs) = old;
                }
                self.error = Some(e);
            }
            Ev::Renamed { id, name, emoji, res: Ok(()) } => {
                if let Some(s) = self.sections.iter_mut().find(|s| s.id == id) {
                    (s.name, s.emoji) = (name, emoji);
                }
                self.section = None;
            }
            Ev::Created { name, emoji, chan, next, res: Ok(id) } => {
                let at = self.sections.iter().position(|s| s.id == next).unwrap_or(self.sections.len());
                self.sections.insert(at, Section { id: id.clone(), kind: "standard".into(), name, emoji, ids: Vec::new(), hidden: false });
                self.section = None;
                if let Some(chan) = chan {
                    self.move_conv(chan, id);
                }
            }
            Ev::Renamed { res: Err(e), .. } | Ev::Created { res: Err(e), .. } => {
                if let Some(d) = self.section.as_mut() {
                    d.saving = false;
                }
                self.error = Some(e);
            }
        }
        self.dirty = true;
    }

    fn pick_section_emoji(&mut self, i: usize) {
        let Some(d) = self.section.as_mut() else { return };
        let Some((query, _)) = d.picker.take() else { return };
        if let Some(emoji::Found { name, .. }) = self.emoji.search(&query, emoji::ROWS).into_iter().nth(i) {
            d.emoji = name;
        }
        d.field = 1;
    }

    pub(crate) fn section_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        self.dirty = true;
        let Some(d) = self.section.as_mut() else { return };
        if let Some((query, sel)) = d.picker.as_mut() {
            let n = self.emoji.search(query, emoji::ROWS).len().max(1);
            match k.code {
                KeyCode::Esc => d.picker = None,
                KeyCode::Up | KeyCode::BackTab => *sel = (*sel + n - 1) % n,
                KeyCode::Down | KeyCode::Tab => *sel = (*sel + 1) % n,
                KeyCode::Enter => {
                    let sel = *sel;
                    self.pick_section_emoji(sel);
                }
                KeyCode::Backspace => (_, *sel) = (query.pop(), 0),
                KeyCode::Char('u') if ctrl => (*query, *sel) = (String::new(), 0),
                KeyCode::Char(c) if !ctrl && c != ':' => {
                    query.push(c);
                    *sel = 0;
                }
                _ => {}
            }
            return;
        }
        match k.code {
            KeyCode::Esc => self.section = None,
            KeyCode::Enter if d.field == 0 => d.picker = Some((String::new(), 0)),
            KeyCode::Enter => self.save_section(),
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Up | KeyCode::Down => d.field = 1 - d.field,
            KeyCode::Backspace | KeyCode::Delete if d.field == 0 => d.emoji.clear(),
            KeyCode::Backspace => {
                d.name.pop();
            }
            KeyCode::Char('u') if ctrl => d.name.clear(),
            KeyCode::Char(c) if !ctrl => {
                if d.name.chars().count() < NAME_MAX {
                    d.name.push(c);
                }
                d.field = 1;
            }
            _ => {}
        }
    }

    pub(crate) fn section_paste(&mut self, s: &str) {
        let line = s.lines().next().unwrap_or("").trim();
        let Some(d) = self.section.as_mut() else { return };
        match d.picker.as_mut() {
            Some((query, _)) => query.push_str(line),
            None => {
                d.name.push_str(line);
                d.name = d.name.chars().take(NAME_MAX).collect();
                d.field = 1;
            }
        }
        self.dirty = true;
    }

    /// Modal: clicks outside close it, like Cancel.
    pub(crate) fn section_mouse(&mut self, m: MouseEvent) {
        let at = Position::new(m.column, m.row);
        let a = &self.areas.section;
        let pick = a.picker_rows.iter().find(|(r, _)| r.contains(at)).map(|(_, i)| *i);
        let (in_picker, in_dialog) = (a.picker.contains(at), a.dialog.contains(at));
        let (emoji, text, save, cancel) = (a.emoji.contains(at), a.text.contains(at), a.save.contains(at), a.cancel.contains(at));
        let left = m.kind == MouseEventKind::Down(MouseButton::Left);
        self.dirty = true;
        let Some(d) = self.section.as_mut() else { return };
        if let Some((_, sel)) = d.picker.as_mut() {
            match m.kind {
                MouseEventKind::Moved => *sel = pick.unwrap_or(*sel),
                _ if left && pick.is_some() => self.pick_section_emoji(pick.unwrap_or(0)),
                MouseEventKind::Down(_) if !in_picker => d.picker = None,
                _ => {}
            }
            return;
        }
        if !left {
            return;
        }
        if !in_dialog || cancel {
            self.section = None;
        } else if save {
            self.save_section();
        } else if emoji {
            (d.field, d.picker) = (0, Some((String::new(), 0)));
        } else if text {
            d.field = 1;
        }
    }
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let a = &mut app.areas.section;
    (a.dialog, a.emoji, a.text, a.save, a.cancel, a.picker) = Default::default();
    a.picker_rows.clear();
    let Some(d) = &app.section else { return };
    let (name, emoji, field, saving) = (d.name.clone(), d.emoji.clone(), d.field, d.saving);
    let picker = d.picker.clone();
    let title = match d.target {
        Target::Rename(_) => tr(" Rename this section ", " Renomear esta seção "),
        Target::New { .. } => tr(" Create a section ", " Criar uma seção "),
    };
    let area = f.area();
    let (w, h) = (56.min(area.width.saturating_sub(4)), 7.min(area.height.saturating_sub(2)));
    let rect = Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h);
    f.render_widget(Clear, rect);
    let block = Block::bordered().border_type(BorderType::Rounded).border_style(dim()).title(Line::from(title).bold());
    let inner = block.inner(rect).inner(Margin { horizontal: 1, vertical: 0 });
    f.render_widget(block, rect);
    app.areas.section.dialog = rect;
    if inner.width < 30 || inner.height < 5 {
        return;
    }
    let iw = inner.width as usize;

    // The emoji button and the name, on one line like Slack's field.
    let y = inner.y + 1;
    let g = if emoji.is_empty() { None } else { app.emoji.glyph(&emoji) };
    let face = match &g {
        Some(Glyph::Text(e)) => e.clone(),
        Some(Glyph::Image(_)) => "  ".into(),
        None => "🙂".into(),
    };
    let button = format!(" {face}{} ", " ".repeat(2usize.saturating_sub(face.width())));
    let button_style = match () {
        _ if field == 0 => Style::new().reversed(),
        _ if g.is_none() => dim().bg(Color::Indexed(236)),
        _ => Style::new().bg(Color::Indexed(236)),
    };
    let room = iw.saturating_sub(4 + 1 + 1);
    let value = if name.is_empty() { Span::styled(tr("Section name", "Nome da seção"), dim()) } else { Span::raw(tail(&name, room)) };
    f.render_widget(Paragraph::new(Line::from(vec![Span::styled(button, button_style), Span::raw(" "), value])), Rect::new(inner.x, y, inner.width, 1));
    let emoji_rect = Rect::new(inner.x, y, 4, 1);
    app.areas.section.emoji = emoji_rect;
    app.areas.section.text = Rect::new(inner.x + 5, y, inner.width.saturating_sub(5), 1);
    if let Some(Glyph::Image(img)) = &g {
        draw_image(f, app, img, Rect::new(inner.x + 1, y, 2, 1));
    }
    if field == 1 && picker.is_none() {
        let cx = inner.x + 5 + if name.is_empty() { 0 } else { tail(&name, room).width() as u16 };
        f.set_cursor_position((cx.min(inner.right().saturating_sub(1)), y));
    }

    // Hint, then Cancel and Save on the right.
    let by = inner.bottom().saturating_sub(1);
    let save = if saving { tr(" Saving… ", " Salvando… ") } else { tr(" Save ", " Salvar ") };
    let cancel = tr(" Cancel ", " Cancelar ");
    let save_r = Rect::new(inner.right().saturating_sub(save.width() as u16), by, save.width() as u16, 1);
    let cancel_r = Rect::new(save_r.x.saturating_sub(cancel.width() as u16 + 1), by, cancel.width() as u16, 1);
    let save_style = if name.trim().is_empty() { dim().bg(Color::Indexed(236)) } else { Style::new().fg(Color::Black).bg(Color::Green).bold() };
    f.render_widget(Paragraph::new(Span::styled(save, save_style)), save_r);
    f.render_widget(Paragraph::new(Span::styled(cancel, Style::new().bg(Color::Indexed(236)))), cancel_r);
    let hint_w = cancel_r.x.saturating_sub(inner.x + 1);
    let hint = tr("Tab emoji · Enter saves · Esc cancels", "Tab emoji · Enter salva · Esc cancela");
    f.render_widget(Paragraph::new(truncate(hint, hint_w as usize)).style(dim()), Rect::new(inner.x, by, hint_w, 1));
    (app.areas.section.save, app.areas.section.cancel) = (save_r, cancel_r);

    if let Some((query, sel)) = picker {
        draw_picker(f, app, &query, sel, emoji_rect);
    }
}

/// The emoji picker under the emoji button, the same as the status dialog's.
fn draw_picker(f: &mut Frame, app: &mut App, query: &str, sel: usize, below: Rect) {
    let hits = app.emoji.search(query, emoji::ROWS);
    let area = f.area();
    let (w, h) = (34.min(area.width), (hits.len().max(1) as u16 + 4).min(area.height));
    let rect = Rect::new(below.x.min(area.right().saturating_sub(w)), (below.y + 1).min(area.bottom().saturating_sub(h)), w, h);
    f.render_widget(Clear, rect);
    let block = Block::bordered().border_type(BorderType::Rounded).border_style(dim()).title(Line::from(" Emoji ").bold());
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    app.areas.section.picker = rect;
    let search = if query.is_empty() {
        Line::from(vec![Span::raw(" 🔍 "), Span::styled(tr("Search emoji", "Procurar emoji"), dim())])
    } else {
        Line::from(vec![Span::raw(" 🔍 "), Span::raw(query.to_string()), Span::raw("▏")])
    };
    f.render_widget(Paragraph::new(search), Rect::new(inner.x, inner.y, inner.width, 1));
    f.render_widget(Paragraph::new("─".repeat(inner.width as usize)).style(dim()), Rect::new(inner.x, inner.y + 1, inner.width, 1));
    if hits.is_empty() {
        f.render_widget(Paragraph::new(tr(" no emoji", " nenhum emoji")).style(dim()), Rect::new(inner.x, inner.y + 2, inner.width, 1));
    }
    for (n, emoji::Found { name, glyph, hl }) in hits.iter().enumerate() {
        let row = Rect::new(inner.x, inner.y + 2 + n as u16, inner.width, 1);
        if row.y >= inner.bottom() {
            break;
        }
        emoji_row(f, app, row, name, glyph, hl, n == sel);
        app.areas.section.picker_rows.push((row, n));
    }
}
