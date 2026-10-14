//! The sidebar folded to a rail: dragged past its narrowest, it keeps only
//! each conversation's picture, with mentions as a badge over it.

use crate::app::{App, Focus, Img, ImgRef, Row};
use crate::icons::Presence;
use crate::slack::{Conversation, Kind};
use crate::ui::{dim, draw_image, draw_presence, dot_cell, presence_of, presence_span, row_attention, scrollbar, LOCK, SELECTED, SURFACE};
use crossterm::event::{MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

/// The rail's width, its border included: a gap, the picture, the scrollbar.
pub const WIDTH: u16 = 7;
/// A picture's cells, a square in most fonts.
const PIC: (u16, u16) = (4, 2);
/// Rows per conversation: the picture and a blank row.
const ENTRY: u16 = 3;
/// Behind people without a photo, picked by their id so each keeps one.
const TONES: [Color; 6] = [
    Color::Rgb(0x5B, 0x6E, 0xAE),
    Color::Rgb(0x3E, 0x8E, 0x7E),
    Color::Rgb(0xA0, 0x6A, 0x3C),
    Color::Rgb(0x8E, 0x4F, 0x8E),
    Color::Rgb(0x4F, 0x7F, 0xA8),
    Color::Rgb(0x9A, 0x5B, 0x5B),
];
const CHANNEL_TONE: Color = Color::Indexed(238);

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let block = Block::new().borders(Borders::RIGHT).border_style(dim());
    let inner = block.inner(area);
    f.render_widget(block, area);
    app.areas.sidebar_border = Rect::new(area.right().saturating_sub(1), area.y, 1, area.height);
    app.areas.compose.button = Rect::default();
    app.areas.mention_pills.clear();
    app.areas.rail.clear();
    if inner.height < 10 {
        return;
    }
    let x = inner.x + 1;
    let photos = app.sidebar_avatars && app.picker.is_some();

    // The workspace's icon, then the search button.
    let team = Rect::new(x, inner.y, PIC.0, PIC.1);
    match app.team_icon.clone().filter(|_| app.picker.is_some()) {
        Some(icon) => {
            let domain = app.me.as_ref().map(|m| m.domain.clone()).unwrap_or_default();
            draw_image(f, app, &ImgRef::avatar(&format!("team-{domain}"), &icon), team);
        }
        None => {
            let name = app.me.as_ref().map(|m| m.team.clone()).unwrap_or_else(|| "slick".into());
            tile(f, Rect::new(x, inner.y, PIC.0, 3), &initials(&name), Color::Indexed(240), None);
        }
    }
    app.areas.team_name = team;
    let search = Rect::new(inner.x, inner.y + 3, inner.width, 1);
    f.render_widget(Paragraph::new(Line::from(vec![Span::raw("  "), Span::styled("🔍", dim())])), search);
    app.areas.search = search;

    // Your photo at the bottom, under a line.
    let foot = Rect::new(inner.x, inner.bottom() - 3, inner.width, 3);
    f.render_widget(Paragraph::new("─".repeat(inner.width as usize)).style(dim()), Rect { height: 1, ..foot });
    let me = Rect::new(x, foot.y + 1, PIC.0, PIC.1);
    // A newer version: a green arrow beside it, its menu has the update.
    if app.update.is_some() {
        let up = Rect::new(me.right(), me.y, 1, 1).intersection(inner);
        f.render_widget(Paragraph::new(Span::styled("↑", Style::new().fg(Color::Green).add_modifier(Modifier::BOLD))), up);
    }
    app.areas.account.button = Rect { width: me.width + 1, ..me }.intersection(inner);
    if let Some(uid) = app.me.as_ref().map(|m| m.user_id.clone()) {
        match app.users.avatars.get(&uid).cloned().filter(|_| photos) {
            Some(url) => {
                let p = presence_of(app, &uid);
                picture(f, app, avatar(app, &uid, &url), Some(p), 0, me);
            }
            None => {
                let name = app.users.names.get(&uid).cloned().unwrap_or_default();
                tile(f, Rect::new(x, foot.y, PIC.0, 3), &initials(&name), tone(&uid), None);
                tile_presence(f, app, &uid, x, foot.y);
            }
        }
    }

    // The conversations between them.
    let list = Rect::new(inner.x, inner.y + 5, inner.width, foot.y.saturating_sub(inner.y + 5));
    app.areas.sidebar = list;
    let rows = app.rows();
    app.fix_selection(&rows);
    let heights: Vec<u16> = rows.iter().map(height).collect();
    // The furthest the list scrolls: its last rows fill it.
    let mut max = rows.len();
    while max > 0 && heights[max - 1..].iter().sum::<u16>() <= list.height {
        max -= 1;
    }
    app.areas.rail_max = max;
    // From the list's offset; moved so the selected row shows whole, unless the wheel moved it.
    let sel = app.list.selected();
    let mut start = app.list.offset().min(max);
    if let Some(s) = sel.filter(|_| !app.sidebar_wheel) {
        start = start.min(s);
        while start < s && heights[start..=s].iter().sum::<u16>() > list.height {
            start += 1;
        }
    }
    *app.list.offset_mut() = start;
    let focused = matches!(app.focus, Focus::Sidebar | Focus::Filter);
    let mut y = list.y;
    let mut end = start;
    for (n, row) in rows.iter().enumerate().skip(start) {
        let h = heights[n];
        if y + h > list.bottom() {
            break;
        }
        end = n + 1;
        let rect = Rect::new(list.x, y, list.width.saturating_sub(1), h);
        let picked = focused && sel == Some(n);
        match row {
            Row::Gap => {}
            Row::Header { .. } => {
                // A short line where the section's name was.
                f.render_widget(Paragraph::new(format!(" {}", "─".repeat(PIC.0 as usize + 2))).style(dim()), rect);
            }
            Row::Threads => {
                let t = &app.threads;
                let current = t.open;
                shade(f, rect, picked || current);
                let badge = if t.mentions > 0 { Some((t.mentions, Color::Red)) } else { (t.new > 0).then_some((t.new, Color::Rgb(0x12, 0x64, 0xA3))) };
                tile(f, Rect::new(x, y, PIC.0, 3), crate::threads::ICON, CHANNEL_TONE, badge);
            }
            Row::Conv(i) => {
                let c = app.convs[*i].clone();
                conv(f, app, &c, rect, x, picked, photos);
            }
            Row::Other(j) => {
                let c = app.directory.conv(*j);
                conv(f, app, &c, rect, x, picked, photos);
            }
        }
        if h > 0 && !matches!(row, Row::Header { .. }) {
            app.areas.rail.push((rect, n));
        }
        y += h;
    }
    let total: usize = heights.iter().map(|h| *h as usize).sum();
    let top: usize = heights[..start].iter().map(|h| *h as usize).sum();
    let bar = Rect::new(list.right().saturating_sub(1), list.y, 1, list.height);
    scrollbar(f, bar, total, list.height as usize, top);

    // What's scrolled out of sight: an arrow with how many, red for mentions,
    // over the row above the list and the line below it.
    let above = (0..start.min(rows.len())).rev();
    arrow(f, app, &rows, above.collect(), Rect::new(x - 1, list.y - 1, PIC.0 + 2, 1), "↑");
    arrow(f, app, &rows, (end..rows.len()).collect(), Rect::new(x - 1, foot.y, PIC.0 + 2, 1), "↓");
}

fn height(r: &Row) -> u16 {
    match r {
        Row::Gap => 0,
        Row::Header { .. } => 1,
        _ => ENTRY,
    }
}

/// A pill for the rows in `hidden`, nearest first, that wait with mentions
/// or else unread; a click goes to the nearest.
fn arrow(f: &mut Frame, app: &mut App, rows: &[Row], hidden: Vec<usize>, rect: Rect, glyph: &str) {
    let attention: Vec<(usize, (bool, bool))> = hidden.into_iter().map(|n| (n, row_attention(app, &rows[n]))).collect();
    let mentions: Vec<usize> = attention.iter().filter(|(_, a)| a.1).map(|(n, _)| *n).collect();
    let unread: Vec<usize> = attention.iter().filter(|(_, a)| a.0).map(|(n, _)| *n).collect();
    let (to, count, bg) = match (mentions.first(), unread.first()) {
        (Some(&n), _) => (n, mentions.len(), Color::Red),
        (None, Some(&n)) => (n, unread.len(), Color::Indexed(240)),
        _ => return,
    };
    let text = format!("{glyph}{}", count.min(99));
    let w = text.width() as u16 + 2;
    let rect = Rect { x: rect.x + rect.width.saturating_sub(w) / 2, width: w.min(rect.width), ..rect };
    let line = Line::from(vec![
        Span::styled(crate::blocks::CAP_L, Style::new().fg(bg)),
        Span::styled(text, Style::new().fg(Color::White).bg(bg).add_modifier(Modifier::BOLD)),
        Span::styled(crate::blocks::CAP_R, Style::new().fg(bg)),
    ]);
    f.render_widget(Paragraph::new(line), rect);
    app.areas.mention_pills.push((rect, to));
}

/// The rows the rail lists: channels only while they wait (or are open),
/// and a section's line only when something is left under it.
pub fn rows(app: &App, rows: Vec<Row>) -> Vec<Row> {
    let keep = |r: &Row| match r {
        Row::Gap => false,
        Row::Conv(i) => {
            let c = &app.convs[*i];
            let (unread, mentions) = app.unread(&c.id);
            !matches!(c.kind, Kind::Channel | Kind::Private) || (unread && !app.muted(c)) || mentions > 0 || app.current.as_deref() == Some(c.id.as_str())
        }
        _ => true,
    };
    let mut rows: Vec<Row> = rows.into_iter().filter(keep).collect();
    let empty: Vec<bool> = (0..rows.len())
        .map(|n| matches!(rows[n], Row::Header { .. }) && !matches!(rows.get(n + 1), Some(Row::Conv(_) | Row::Other(_))))
        .collect();
    let mut n = 0;
    rows.retain(|_| {
        n += 1;
        !empty[n - 1]
    });
    rows
}

/// The name of the entry under the mouse, beside the rail.
pub fn tooltip(f: &mut Frame, app: &mut App) {
    let Some(n) = app.rail_hover else { return };
    let Some(&(rect, _)) = app.areas.rail.iter().find(|(_, i)| *i == n) else { return };
    let label = match app.rows().get(n) {
        Some(Row::Threads) => crate::i18n::tr("Threads", "Conversas").to_string(),
        Some(Row::Conv(i)) => app.label(&app.convs[*i]),
        Some(Row::Other(j)) => app.label(&app.directory.conv(*j)),
        _ => return,
    };
    let area = f.area();
    let x = app.areas.sidebar_border.right();
    let w = (label.width() as u16 + 2).min(area.width.saturating_sub(x));
    let tip = Rect::new(x, rect.y + PIC.1 / 2, w, 1).intersection(area);
    f.render_widget(Clear, tip);
    f.render_widget(Paragraph::new(format!(" {label} ")).style(Style::new().fg(Color::White).bg(SURFACE)), tip);
}

impl App {
    /// Remembers which folded sidebar entry the mouse is over, for `tooltip`.
    pub fn rail_mouse(&mut self, m: MouseEvent) {
        let at = Position::new(m.column, m.row);
        let hover = match m.kind {
            MouseEventKind::Moved => self.areas.rail.iter().find(|(r, _)| r.contains(at)).map(|(_, n)| *n),
            _ => None,
        };
        if hover != self.rail_hover {
            self.rail_hover = hover;
            self.dirty = true;
        }
    }
}

/// A conversation: its photo, the group's count, or a tile with its initials.
fn conv(f: &mut Frame, app: &mut App, c: &Conversation, rect: Rect, x: u16, picked: bool, photos: bool) {
    let (_, mentions) = app.unread(&c.id);
    let current = app.current.as_deref() == Some(c.id.as_str());
    shade(f, rect, picked || current);
    let uid = c.user.clone().unwrap_or_default();
    let pic = Rect::new(x, rect.y, PIC.0, PIC.1);
    match c.kind {
        Kind::Direct if photos && app.users.avatars.contains_key(&uid) => {
            let url = app.users.avatars[&uid].clone();
            let p = (!app.users.bots.contains(&uid)).then(|| presence_of(app, &uid));
            picture(f, app, avatar(app, &uid, &url), p, mentions, pic);
        }
        Kind::Group if photos => {
            let others = app.label(c).matches(", ").count() + 1;
            let (fw, fh) = font(app);
            let key = format!("badge-{others}-{fw}x{fh}");
            app.images.entry(key.clone()).or_insert_with(|| Img::Ready(crate::icons::group_badge(others, PIC.0 as u32 * fw, PIC.1 as u32 * fh)));
            picture(f, app, ImgRef { key, url: String::new(), name: String::new(), link: None }, None, mentions, pic);
        }
        Kind::Channel | Kind::Private => {
            let mark = if c.kind == Kind::Private { LOCK } else { "#" };
            let text = format!("{mark}{}", initials(&c.name).to_lowercase());
            tile(f, Rect::new(x, rect.y, PIC.0, 3), &text, CHANNEL_TONE, (mentions > 0).then_some((mentions, Color::Red)));
        }
        _ => {
            let text = initials(&app.label(c));
            tile(f, Rect::new(x, rect.y, PIC.0, 3), &text, tone(&uid), (mentions > 0).then_some((mentions, Color::Red)));
            if c.kind == Kind::Direct && !app.users.bots.contains(&uid) {
                tile_presence(f, app, &uid, x, rect.y);
            }
        }
    }
}

/// Slack's grey bar under the picked or open entry.
fn shade(f: &mut Frame, rect: Rect, on: bool) {
    if on {
        f.render_widget(Block::new().style(Style::new().bg(SELECTED)), Rect { height: rect.height.min(PIC.1 + 1), ..rect });
    }
}

/// A picture with presence and a mention count laid over it, made once per
/// combination.
fn picture(f: &mut Frame, app: &mut App, base: ImgRef, p: Option<Presence>, n: u64, rect: Rect) {
    let key = format!("rail-{}-{}-{n}", base.key, p.map_or("", |p| p.key()));
    if !app.images.contains_key(&key) {
        let Some(Img::Ready(img)) = app.images.get(&base.key) else {
            return draw_image(f, app, &base, rect);
        };
        let mut img = img.clone();
        if let Some(p) = p {
            img = crate::icons::with_presence_by(&img, p, 0.15);
        }
        if n > 0 {
            img = crate::icons::with_badge(&img, n);
        }
        app.images.insert(key.clone(), Img::Ready(img));
    }
    draw_image(f, app, &ImgRef { key, url: String::new(), name: String::new(), link: None }, rect);
}

/// The presence dot over the bottom-right corner of a tile drawn at (x, y).
fn tile_presence(f: &mut Frame, app: &mut App, uid: &str, x: u16, y: u16) {
    let p = presence_of(app, uid);
    let (cx, cy) = (x + PIC.0 - 1, y + 2);
    if dot_cell(app).is_some() {
        return draw_presence(f, app, p, cx, cy);
    }
    f.render_widget(Paragraph::new(Line::from(presence_span(app, p))), Rect::new(cx, cy, 1, 1));
}

/// Someone's photo, sharp enough for the picture's cells.
fn avatar(app: &App, uid: &str, url: &str) -> ImgRef {
    ImgRef::avatar_px(uid, url, PIC.1 as u32 * font(app).1)
}

fn font(app: &App) -> (u32, u32) {
    app.picker.as_ref().map(|p| p.font_size()).map(|s| (s.width as u32, s.height as u32)).unwrap_or((8, 16))
}

/// A square of colour three rows tall, its edges half blocks, with `text`
/// on the middle row and the count in red over its top-right corner.
fn tile(f: &mut Frame, rect: Rect, text: &str, bg: Color, badge: Option<(u64, Color)>) {
    let w = rect.width as usize;
    let edge = |s: &str| Span::styled(s.repeat(w), Style::new().fg(bg));
    let pad = w.saturating_sub(text.width());
    let middle = Span::styled(format!("{}{text}{}", " ".repeat(pad / 2), " ".repeat(pad - pad / 2)), Style::new().fg(Color::White).bg(bg).add_modifier(Modifier::BOLD));
    let mut top = vec![edge("▄")];
    if let Some((n, color)) = badge {
        let b = n.min(99).to_string();
        let keep = w.saturating_sub(1);
        top = vec![Span::styled("▄".repeat(keep), Style::new().fg(bg)), Span::styled(b, Style::new().fg(Color::White).bg(color).add_modifier(Modifier::BOLD))];
    }
    let lines = vec![Line::from(top), Line::from(middle), Line::from(edge("▀"))];
    // One column wider, for a two-digit count past the corner.
    f.render_widget(Paragraph::new(lines), Rect { width: rect.width + 1, ..rect });
}

/// Up to two letters: the first of the first two words, or of the first
/// two parts of a channel's name.
fn initials(name: &str) -> String {
    let words: Vec<&str> = name.split(|c: char| c.is_whitespace() || c == '-' || c == '_' || c == '.').filter(|w| !w.is_empty()).collect();
    let s: String = match words.as_slice() {
        [] => String::new(),
        [one] => one.chars().take(2).collect(),
        [a, b, ..] => a.chars().take(1).chain(b.chars().take(1)).collect(),
    };
    s.to_uppercase()
}

fn tone(id: &str) -> Color {
    let h = id.bytes().fold(0usize, |h, b| h.wrapping_mul(31).wrapping_add(b as usize));
    TONES[h % TONES.len()]
}
