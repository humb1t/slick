//! "Custom…" in the notifications submenu: Slack's "Pause Notifications"
//! dialog. A day (with a small calendar beside it), a time in half hours
//! and, when Slack has the pref, whether VIPs still get through. Saving
//! turns the moment into minutes for `dnd.setSnooze`, like the other pauses.

use crate::account::Paused;
use crate::app::{App, Event};
use crate::blocks::{CAP_L, CAP_R};
use crate::i18n::{self, tr};
use crate::slack::{Res, Slack};
use crate::ui::{dim, rounded_input, truncate, SELECTED};
use chrono::{Datelike, Days, Local, Months, NaiveDate, NaiveTime, TimeZone, Timelike};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Margin, Position, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

/// Half hours in a day: the time dropdown's rows.
const SLOTS: u32 = 48;
/// Rows the time dropdown shows at once.
const VISIBLE: usize = 8;
/// The fields' column, and the calendar's (7 days of 3 cells, less the last gap).
const FIELD_W: u16 = 30;
const CAL_W: u16 = 20;
/// Slack's pref: VIPs still notify during a pause.
const VIP_PREF: &str = "priority_dnd_override";

#[derive(Clone, Copy, PartialEq)]
enum Field {
    Date,
    Time,
    For,
    Save,
}

/// Where the last draw put things, for the mouse.
#[derive(Default)]
struct Areas {
    modal: Rect,
    close: Rect,
    date: Rect,
    prev_day: Rect,
    next_day: Rect,
    time: Rect,
    vips: Rect,
    save: Rect,
    prev_month: Rect,
    next_month: Rect,
    days: Vec<(Rect, NaiveDate)>,
    list: Rect,
    rows: Vec<(Rect, u32)>,
}

pub struct PauseDialog {
    day: NaiveDate,
    /// Half hours since midnight.
    slot: u32,
    /// Everyone except VIPs; `None` when Slack doesn't have the pref.
    vips: Option<bool>,
    /// As Slack had it, to set it only when changed.
    vips_was: Option<bool>,
    vips_loaded: bool,
    field: Field,
    /// The open dropdown, time or VIPs, and its highlighted row.
    open: Option<u32>,
    error: Option<String>,
    saving: bool,
    areas: Areas,
}

pub enum Ev {
    Vips(Res<Option<bool>>),
    Saved(Res<Option<Paused>>),
}

// ---- Slack -----------------------------------------------------------------

fn vip_pref(s: &Slack) -> Res<Option<bool>> {
    let v = s.call("users.prefs.get", &[])?;
    let p = &v["prefs"][VIP_PREF];
    Ok(p.as_bool().or_else(|| p.as_str().map(|s| s == "true" || s == "1")))
}

/// A pause set or ended here, for every workspace (`Workspaces`); the VIP
/// pref changed with it goes to all of them too.
#[derive(Clone, Copy)]
pub(crate) enum DndAll {
    Pause { minutes: i64, vips: Option<bool> },
    Resume,
}

/// The VIP pref first, when changed: a failure there pauses nothing.
fn save(s: &Slack, minutes: i64, vips: Option<bool>) -> Res<Option<Paused>> {
    if let Some(on) = vips {
        s.call("users.prefs.set", &[("name", VIP_PREF), ("value", if on { "true" } else { "false" })])?;
    }
    crate::account::pause(s, minutes)
}

// ---- time ------------------------------------------------------------------

fn time_of(slot: u32) -> NaiveTime {
    NaiveTime::from_hms_opt(slot / 2, slot % 2 * 30, 0).unwrap_or_default()
}

/// "12:00 PM", or "12h00".
fn slot_label(slot: u32) -> String {
    i18n::clock(&time_of(slot))
}

/// "October 12th, 2026", or "12 de outubro de 2026".
fn date_label(d: NaiveDate) -> String {
    let (month, year) = (i18n::month(&d), d.year());
    if i18n::is_pt() {
        format!("{} de {month} de {year}", d.day())
    } else {
        format!("{month} {}, {year}", i18n::ordinal(d.day()))
    }
}

/// The next half or whole hour after now: today, or past midnight tomorrow.
fn next_slot() -> (NaiveDate, u32) {
    let now = Local::now();
    let slot = (now.hour() * 60 + now.minute()) / 30 + 1;
    let today = now.date_naive();
    if slot >= SLOTS {
        (today + Days::new(1), 0)
    } else {
        (today, slot)
    }
}

/// Minutes from now until the moment, rounded up; an error when it's gone.
fn minutes_until(day: NaiveDate, slot: u32) -> Result<i64, String> {
    let at = Local.from_local_datetime(&day.and_time(time_of(slot))).earliest();
    let Some(at) = at else { return Err(tr("That time doesn't exist here (clock change).", "Esse horário não existe aqui (mudança de horário).").into()) };
    let secs = at.timestamp() - Local::now().timestamp();
    if secs <= 0 {
        return Err(tr("Pick a date and time in the future.", "Escolha uma data e hora no futuro.").into());
    }
    Ok((secs + 59) / 60)
}

fn days_in_month(d: NaiveDate) -> u32 {
    let first = d.with_day(1).unwrap_or(d);
    first.checked_add_months(Months::new(1)).map_or(31, |n| (n - first).num_days() as u32)
}

// ---- state -----------------------------------------------------------------

impl PauseDialog {
    fn fields(&self) -> Vec<Field> {
        let mut v = vec![Field::Date, Field::Time];
        if self.vips.is_some() {
            v.push(Field::For);
        }
        v.push(Field::Save);
        v
    }

    fn step_field(&mut self, delta: isize) {
        let all = self.fields();
        let n = all.len() as isize;
        let i = all.iter().position(|f| *f == self.field).unwrap_or(0) as isize;
        self.field = all[(i + delta).rem_euclid(n) as usize];
        self.open = None;
    }

    /// Never before today.
    fn set_day(&mut self, d: NaiveDate) {
        self.day = d.max(Local::now().date_naive());
        self.error = None;
    }

    fn shift_days(&mut self, n: i64) {
        let d = if n < 0 { self.day.checked_sub_days(Days::new(n.unsigned_abs())) } else { self.day.checked_add_days(Days::new(n as u64)) };
        if let Some(d) = d {
            self.set_day(d);
        }
    }

    fn shift_months(&mut self, n: i32) {
        let d = if n < 0 { self.day.checked_sub_months(Months::new(n.unsigned_abs())) } else { self.day.checked_add_months(Months::new(n as u32)) };
        if let Some(d) = d {
            self.set_day(d);
        }
    }

    fn shift_slot(&mut self, delta: i32) {
        self.slot = (self.slot as i32 + delta).clamp(0, SLOTS as i32 - 1) as u32;
        self.error = None;
    }

    /// The open dropdown's rows.
    fn rows(&self) -> u32 {
        if self.field == Field::Time { SLOTS } else { 2 }
    }

    fn toggle_list(&mut self) {
        self.open = match (self.open, self.field) {
            (Some(_), _) => None,
            (None, Field::Time) => Some(self.slot),
            (None, Field::For) => Some(u32::from(self.vips == Some(true))),
            _ => None,
        };
    }

    fn pick(&mut self, row: u32) {
        match self.field {
            Field::Time => self.slot = row.min(SLOTS - 1),
            Field::For => self.vips = Some(row == 1),
            _ => {}
        }
        self.open = None;
        self.error = None;
    }
}

impl App {
    pub(crate) fn open_pause(&mut self) {
        let (day, slot) = next_slot();
        self.account.menu = None;
        self.account.pause = Some(PauseDialog {
            day,
            slot,
            vips: None,
            vips_was: None,
            vips_loaded: false,
            field: Field::Date,
            open: None,
            error: None,
            saving: false,
            areas: Areas::default(),
        });
        self.spawn(|s| Event::Account(crate::account::Ev::Pause(Ev::Vips(vip_pref(s)))));
        self.dirty = true;
    }

    pub(crate) fn on_pause(&mut self, ev: Ev) {
        match ev {
            Ev::Vips(res) => {
                let Some(d) = self.account.pause.as_mut() else { return };
                // Without it the field stays hidden.
                let v = res.ok().flatten();
                (d.vips, d.vips_was, d.vips_loaded) = (v, v, true);
            }
            Ev::Saved(Ok(p)) => {
                self.account.paused = p;
                self.account.pause = None;
                if let Some(p) = p {
                    self.brief(trf!("Notifications paused {}", "Notificações pausadas {}", crate::account::until(p.until)), 4);
                }
            }
            Ev::Saved(Err(e)) => {
                if let Some(d) = self.account.pause.as_mut() {
                    (d.saving, d.error) = (false, Some(e));
                }
            }
        }
        self.dirty = true;
    }

    fn submit_pause(&mut self) {
        let Some(d) = self.account.pause.as_mut() else { return };
        if d.saving {
            return;
        }
        d.open = None;
        let minutes = match minutes_until(d.day, d.slot) {
            Ok(m) => m,
            Err(e) => return d.error = Some(e),
        };
        let vips = d.vips.filter(|v| d.vips_was != Some(*v));
        (d.saving, d.error) = (true, None);
        self.dnd_all = Some(DndAll::Pause { minutes, vips });
    }

    /// A pause asked for in any workspace, applied in this one. The one
    /// whose dialog is saving hears back through it.
    pub(crate) fn apply_dnd(&mut self, req: DndAll) {
        match req {
            DndAll::Pause { minutes, vips } => {
                let dialog = self.account.pause.as_ref().is_some_and(|d| d.saving);
                self.spawn(move |s| {
                    let res = save(s, minutes, vips);
                    Event::Account(if dialog { crate::account::Ev::Pause(Ev::Saved(res)) } else { crate::account::Ev::DndSaved(res) })
                });
            }
            DndAll::Resume => self.resume_notifications(),
        }
    }

    pub(crate) fn pause_key(&mut self, k: KeyEvent) {
        self.dirty = true;
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let Some(d) = self.account.pause.as_mut() else { return };
        if ctrl && matches!(k.code, KeyCode::Enter | KeyCode::Char('j') | KeyCode::Char('s')) {
            return self.submit_pause();
        }
        if let Some(hl) = d.open {
            let n = d.rows();
            match k.code {
                KeyCode::Up | KeyCode::Char('k') => d.open = Some(hl.saturating_sub(1)),
                KeyCode::Down | KeyCode::Char('j') => d.open = Some((hl + 1).min(n - 1)),
                KeyCode::PageUp => d.open = Some(hl.saturating_sub(VISIBLE as u32)),
                KeyCode::PageDown => d.open = Some((hl + VISIBLE as u32).min(n - 1)),
                KeyCode::Enter | KeyCode::Char(' ') => d.pick(hl),
                KeyCode::Esc => d.open = None,
                KeyCode::Tab => d.step_field(1),
                KeyCode::BackTab => d.step_field(-1),
                _ => {}
            }
            return;
        }
        match (k.code, d.field) {
            (KeyCode::Esc, _) => self.account.pause = None,
            (KeyCode::Tab, _) => d.step_field(1),
            (KeyCode::BackTab, _) => d.step_field(-1),
            (KeyCode::Left | KeyCode::Up, Field::Date) => d.shift_days(-1),
            (KeyCode::Right | KeyCode::Down, Field::Date) => d.shift_days(1),
            (KeyCode::PageUp, Field::Date) => d.shift_months(-1),
            (KeyCode::PageDown, Field::Date) => d.shift_months(1),
            (KeyCode::Home, Field::Date) => d.set_day(Local::now().date_naive()),
            (KeyCode::Up, Field::Time) => d.shift_slot(-1),
            (KeyCode::Down, Field::Time) => d.shift_slot(1),
            (KeyCode::Up | KeyCode::Down, Field::For) => d.vips = d.vips.map(|v| !v),
            (KeyCode::Enter | KeyCode::Char(' '), Field::Time | Field::For) => d.toggle_list(),
            (KeyCode::Enter, _) | (KeyCode::Char(' '), Field::Save) => self.submit_pause(),
            _ => {}
        }
    }

    pub(crate) fn pause_mouse(&mut self, m: MouseEvent) {
        let at = Position::new(m.column, m.row);
        let Some(d) = self.account.pause.as_mut() else { return };
        let a = &d.areas;
        let row = a.rows.iter().find(|(r, _)| r.contains(at)).map(|(_, i)| *i);
        let day = a.days.iter().find(|(r, _)| r.contains(at)).map(|(_, d)| *d);
        let in_list = a.list.contains(at);
        let (on_date, on_time, on_vips) = (a.date.contains(at), a.time.contains(at), a.vips.contains(at));
        let on_cal = day.is_some() || a.prev_month.contains(at) || a.next_month.contains(at);
        let (prev_day, next_day, prev_month, next_month) = (a.prev_day.contains(at), a.next_day.contains(at), a.prev_month.contains(at), a.next_month.contains(at));
        let (close, save) = (a.close.contains(at), a.save.contains(at));
        self.dirty = true;
        match m.kind {
            MouseEventKind::Moved => {
                if let (Some(_), Some(r)) = (d.open, row) {
                    d.open = Some(r);
                }
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let up = m.kind == MouseEventKind::ScrollUp;
                if let Some(hl) = d.open.filter(|_| in_list) {
                    d.open = Some(if up { hl.saturating_sub(1) } else { (hl + 1).min(d.rows() - 1) });
                } else if on_date || on_cal {
                    d.shift_days(if up { -1 } else { 1 });
                } else if on_time {
                    d.shift_slot(if up { -1 } else { 1 });
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(r) = row.filter(|_| d.open.is_some()) {
                    return d.pick(r);
                }
                // A click elsewhere closes an open dropdown, and still counts.
                let was_open = d.open.take().is_some();
                if close {
                    self.account.pause = None;
                } else if save {
                    self.submit_pause();
                } else if prev_day || next_day {
                    d.field = Field::Date;
                    d.shift_days(if prev_day { -1 } else { 1 });
                } else if prev_month || next_month {
                    d.field = Field::Date;
                    d.shift_months(if prev_month { -1 } else { 1 });
                } else if let Some(day) = day {
                    d.field = Field::Date;
                    d.set_day(day);
                } else if on_date {
                    d.field = Field::Date;
                } else if on_time || (on_vips && d.vips.is_some()) {
                    let f = if on_time { Field::Time } else { Field::For };
                    let reopen = !(was_open && d.field == f);
                    d.field = f;
                    if reopen {
                        d.toggle_list();
                    }
                }
            }
            _ => {}
        }
    }
}

// ---- drawing ---------------------------------------------------------------

/// A rounded button, like the other dialogs'.
fn pill(label: &str, fg: Color, bg: Color) -> Line<'static> {
    Line::from(vec![
        Span::styled(CAP_L, Style::new().fg(bg)),
        Span::styled(label.to_string(), Style::new().fg(fg).bg(bg).bold()),
        Span::styled(CAP_R, Style::new().fg(bg)),
    ])
}

/// A field's text, with its marker (› or ▾) at the right end of `w` cells.
fn field_line(text: &str, right: &str, w: u16) -> Line<'static> {
    let w = w as usize;
    let text = truncate(text, w.saturating_sub(right.width() + 1));
    let gap = w.saturating_sub(text.width() + right.width());
    Line::from(vec![Span::raw(format!("{text}{}", " ".repeat(gap))), Span::styled(right.to_string(), Style::new().fg(Color::Gray))])
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let Some(d) = app.account.pause.as_mut() else { return };
    d.areas = Areas::default();
    let area = f.area();
    // The calendar goes beside the fields when there's room.
    let wide = area.width >= FIELD_W + CAL_W + 3 + 4 + 4;
    let iw = if wide { FIELD_W + 3 + CAL_W } else { FIELD_W };
    // Each field is a label over a 3-row input; a blank row between the
    // parts while the terminal is tall enough.
    let n = if !d.vips_loaded || d.vips.is_some() { 3 } else { 2 };
    let need = |g: u16| g + n * 4 + (n - 1) * g + g + 1 + g + 1 + 2;
    let g = if need(1) + 2 <= area.height { 1 } else { 0 };
    let (w, h) = ((iw + 4).min(area.width.saturating_sub(2)), need(g).min(area.height.saturating_sub(2)));
    let rect = Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h);
    f.render_widget(Clear, rect);
    let title = Line::from(tr(" Pause Notifications ", " Pausar notificações ")).bold();
    let block = Block::bordered().border_type(BorderType::Rounded).border_style(dim()).title(title);
    let inner = block.inner(rect).inner(Margin { horizontal: 1, vertical: 0 });
    f.render_widget(block, rect);
    d.areas.modal = rect;
    d.areas.close = Rect::new(rect.right().saturating_sub(5), rect.y, 3, 1);
    f.render_widget(Paragraph::new(" ✕ "), d.areas.close);
    if inner.width < 20 || h < need(g) {
        return;
    }
    let fw = if wide { FIELD_W } else { inner.width };
    // Text room inside an input: its frame and a column of padding each side.
    let tw = fw.saturating_sub(4);
    let x = inner.x;
    let top = inner.y + g;
    let mut y = top;
    let label = |f: &mut Frame, text: &str, y: u16| f.render_widget(Paragraph::new(Line::from(text.to_string()).bold()), Rect::new(x, y, fw, 1));

    // The day, ‹ › around it for the arrows.
    label(f, tr("Pause notifications until…", "Pausar notificações até…"), y);
    let date = Rect::new(x, y + 1, fw, 3);
    rounded_input(f, date, d.field == Field::Date, field_line(&format!("‹ {}", date_label(d.day)), "›", tw));
    d.areas.date = date;
    d.areas.prev_day = Rect::new(x, date.y, 4, 3);
    d.areas.next_day = Rect::new(date.right().saturating_sub(4), date.y, 4, 3);
    y += 4 + g;

    // The time, in half hours.
    label(f, tr("Time", "Hora"), y);
    let time = Rect::new(x, y + 1, fw, 3);
    rounded_input(f, time, d.field == Field::Time, field_line(&slot_label(d.slot), "▾", tw));
    d.areas.time = time;
    y += 4 + g;

    // Everyone, or everyone but VIPs: only when Slack has the pref.
    if n == 3 {
        label(f, tr("Pause notifications for", "Pausar notificações de"), y);
        let r = Rect::new(x, y + 1, fw, 3);
        let text = match d.vips {
            Some(v) => vip_label(v).to_string(),
            None => "…".into(),
        };
        rounded_input(f, r, d.field == Field::For, field_line(&text, "▾", tw));
        d.areas.vips = r;
        y += 4 + g;
    }

    if wide {
        calendar(f, d, Rect::new(x + FIELD_W + 3, top, CAL_W, 8));
    }

    // An error over the buttons.
    if let Some(e) = &d.error {
        f.render_widget(Paragraph::new(truncate(e, inner.width as usize)).style(Style::new().fg(Color::Red)), Rect::new(x, y, inner.width, 1));
    }

    // Hint, then Save on the right.
    let by = inner.bottom().saturating_sub(1);
    let label = if d.saving { tr(" Saving… ", " Salvando… ") } else { tr(" Save ", " Salvar ") };
    let save = Rect::new(inner.right().saturating_sub(label.width() as u16 + 2), by, label.width() as u16 + 2, 1);
    let (fg, bg) = if d.field == Field::Save { (Color::Black, Color::Cyan) } else { (Color::Black, Color::Green) };
    f.render_widget(Paragraph::new(pill(label, fg, bg)), save);
    d.areas.save = save;
    let hint = match (d.open.is_some(), d.field) {
        (true, _) => tr("↑↓ move · Enter picks · Esc closes", "↑↓ move · Enter escolhe · Esc fecha"),
        (_, Field::Date) => tr("←→ day · PgUp/PgDn month · Tab next · Esc", "←→ dia · PgUp/PgDn mês · Tab próximo · Esc"),
        _ => tr("Tab fields · Enter picks · Esc closes", "Tab campos · Enter escolhe · Esc fecha"),
    };
    let hint_w = save.x.saturating_sub(x + 1);
    f.render_widget(Paragraph::new(truncate(hint, hint_w as usize)).style(dim()), Rect::new(x, by, hint_w, 1));

    if let Some(hl) = d.open {
        let below = if d.field == Field::Time { time } else { d.areas.vips };
        dropdown(f, d, below, hl);
    }
}

fn vip_label(except: bool) -> &'static str {
    if except { tr("Everyone except VIPs", "Todos, exceto VIPs") } else { tr("Everyone", "Todos") }
}

/// The month of the chosen day: today underlined, past days faded.
fn calendar(f: &mut Frame, d: &mut PauseDialog, r: Rect) {
    let today = Local::now().date_naive();
    let focused = d.field == Field::Date;
    let head = format!("{} {}", i18n::month(&d.day), d.day.year());
    let head = truncate(&head, r.width.saturating_sub(4) as usize);
    let pad = (r.width as usize).saturating_sub(head.width() + 4);
    let line = Line::from(vec![
        Span::styled("‹", Style::new().fg(Color::Gray)),
        Span::raw(" ".repeat(pad / 2 + 1)),
        Span::styled(head, Style::new().bold()),
        Span::raw(" ".repeat(pad - pad / 2 + 1)),
        Span::styled("›", Style::new().fg(Color::Gray)),
    ]);
    f.render_widget(Paragraph::new(line), Rect::new(r.x, r.y, r.width, 1));
    d.areas.prev_month = Rect::new(r.x, r.y, 2, 1);
    d.areas.next_month = Rect::new(r.right().saturating_sub(2), r.y, 2, 1);
    let names = tr("Su Mo Tu We Th Fr Sa", "Do Se Te Qu Qu Se Sá");
    f.render_widget(Paragraph::new(names).style(dim()), Rect::new(r.x, r.y + 1, r.width, 1));
    let Some(first) = d.day.with_day(1) else { return };
    let offset = first.weekday().num_days_from_sunday();
    for n in 1..=days_in_month(d.day) {
        let Some(day) = first.with_day(n) else { continue };
        let i = offset + n - 1;
        let cell = Rect::new(r.x + (i % 7) as u16 * 3, r.y + 2 + (i / 7) as u16, 2, 1);
        if cell.bottom() > r.bottom() {
            break;
        }
        let mut style = if day < today { dim() } else { Style::new() };
        if day == today {
            style = style.add_modifier(Modifier::UNDERLINED);
        }
        if day == d.day {
            style = if focused { Style::new().fg(Color::Black).bg(Color::Cyan).bold() } else { Style::new().bg(SELECTED).fg(Color::Cyan).bold() };
        }
        f.render_widget(Paragraph::new(Span::styled(format!("{n:>2}"), style)), cell);
        if day >= today {
            d.areas.days.push((cell, day));
        }
    }
}

/// The open dropdown under its field: the chosen row checked, the
/// highlighted one on a bar; the time scrolls around the highlight.
fn dropdown(f: &mut Frame, d: &mut PauseDialog, below: Rect, hl: u32) {
    let area = f.area();
    let n = d.rows() as usize;
    let shown = n.min(VISIBLE);
    let h = shown as u16 + 2;
    let w = below.width;
    let y = if below.bottom() + h <= area.bottom() { below.bottom() } else { below.y.saturating_sub(h) };
    let rect = Rect::new(below.x, y, w, h).intersection(area);
    f.render_widget(Clear, rect);
    let block = Block::bordered().border_type(BorderType::Rounded).border_style(dim());
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    d.areas.list = rect;
    let start = (hl as usize).saturating_sub(shown / 2).min(n - shown);
    let today = Local::now().date_naive();
    let now = Local::now().time();
    for (k, i) in (start..start + shown).enumerate() {
        let row = Rect::new(inner.x, inner.y + k as u16, inner.width, 1);
        if row.y >= inner.bottom() {
            break;
        }
        let i = i as u32;
        let (text, chosen, past) = match d.field {
            Field::Time => (slot_label(i), i == d.slot, d.day == today && time_of(i) <= now),
            _ => (vip_label(i == 1).to_string(), d.vips == Some(i == 1), false),
        };
        let accent = Style::new().fg(Color::Cyan);
        let check = if chosen { Span::styled(" ✓ ", accent.bold()) } else { Span::raw("   ") };
        let face = if chosen { accent.bold() } else if past { dim() } else { Style::new() };
        let mut line = Line::from(vec![check, Span::styled(truncate(&text, inner.width.saturating_sub(4) as usize), face)]);
        if i == hl {
            line = line.style(Style::new().bg(SELECTED));
            let pad = (inner.width as usize).saturating_sub(line.width());
            line.spans.push(Span::raw(" ".repeat(pad)));
        }
        f.render_widget(Paragraph::new(line), row);
        d.areas.rows.push((row, i));
    }
}
