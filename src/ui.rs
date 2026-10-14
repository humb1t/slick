//! Drawing. Only the terminal's own palette (ANSI colors and the default
//! foreground/background) is used, so the app follows the terminal theme;
//! the sign-in screen is the exception and wears Slack's own colors.

use crate::app::{Action, App, Focus, Img, ImgRef, ReadOnly, Row};
use crate::emoji;
use crate::blocks;
use crate::files;
use crate::i18n::{self, tr};
use crate::slack::{Conversation, File, Kind, Message};
use crate::text::Names;
use chrono::{Datelike, Local, TimeZone};
use std::time::{Duration, Instant};
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, Paragraph};
use ratatui::widgets::ListState;
use ratatui::Frame;
use crate::icons::Presence;
use ratatui_image::picker::ProtocolType;
use ratatui_image::{Image, Resize};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Nerd Font lock (Ghostty and Kitty ship these glyphs): one cell, unlike 🔒.
pub(crate) const LOCK: &str = "\u{f023}";
// Nerd Font circles: bigger than ● and ○, and the same size as each other.
const ONLINE: &str = "\u{f111}";
const OFFLINE: &str = "\u{f10c}";
const STAR_ON: &str = "\u{f005}";
const STAR_OFF: &str = "\u{f006}";
/// nf-fa-archive: Slack's mark on a deactivated account's archives and archived channels.
pub(crate) const ARCHIVE: &str = "\u{f187}";
pub(crate) const PROFILE: u16 = 44;
pub(crate) const MAX_INPUT_ROWS: usize = 8;

pub(crate) fn dim() -> Style {
    Style::new().fg(Color::DarkGray)
}

/// The bar under a selected row, the sidebar's: quieter than inverting it.
pub(crate) const SELECTED: Color = Color::Indexed(238);
/// A surface a little lighter than the background, for pills and bars.
pub(crate) const SURFACE: Color = Color::Indexed(236);
/// Inline code's text: the theme's warning color, as for `warning` attachments.
pub(crate) const CODE_FG: Color = Color::Yellow;
/// Code's fill: lighter than `SURFACE`, which the picked message is painted
/// with, so code still stands out on it.
pub(crate) const CODE_BG: Color = Color::Indexed(238);

/// A text input: a rounded frame, dim or in the focus accent, with the
/// text a column in from each side. Returns where the text went.
pub(crate) fn rounded_input<'a>(f: &mut Frame, area: Rect, focused: bool, text: impl Into<ratatui::text::Text<'a>>) -> Rect {
    let border = if focused { Style::new().fg(Color::Cyan) } else { dim() };
    let block = Block::bordered().border_type(BorderType::Rounded).border_style(border);
    let inner = block.inner(area).inner(ratatui::layout::Margin { horizontal: 1, vertical: 0 });
    f.render_widget(block, area);
    f.render_widget(Paragraph::new(text), inner);
    inner
}

/// Marks the selected row of a popup list, already padded to its width:
/// the bar under all of it, a thin accent where it starts with a space,
/// and the dim parts lifted a little so they still read on the bar.
pub(crate) fn mark_selected(line: &mut Line) {
    if let Some(first) = line.spans.first_mut().filter(|s| s.content.starts_with(' ')) {
        first.content = first.content[1..].to_string().into();
        line.spans.insert(0, Span::styled("▎", Style::new().fg(Color::Cyan)));
    }
    for s in line.spans.iter_mut().filter(|s| s.style.fg == Some(Color::DarkGray)) {
        s.style.fg = Some(Color::Indexed(246));
    }
    line.style = line.style.bg(SELECTED);
}

pub fn draw(f: &mut Frame, app: &mut App) {
    app.next_frame = None;
    let [main, status] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(f.area());
    let [side, right] = Layout::horizontal([Constraint::Length(app.sidebar_width), Constraint::Min(20)]).areas(main);
    // The profile or thread panel takes the right side, leaving the conversation room to read.
    let panel_w = if app.profile.is_some() {
        PROFILE.min(right.width.saturating_sub(40))
    } else if app.thread.as_ref().is_some_and(|t| !t.inline) {
        crate::thread::width(right.width)
    } else {
        0
    };
    let [right, panel] = Layout::horizontal([Constraint::Min(20), Constraint::Length(panel_w)]).areas(right);

    let input_width = right.width.saturating_sub(4) as usize;
    app.input_width = input_width;
    let input_rows = if let Some(why) = app.read_only() {
        read_only_lines(app, why, input_width).len() as u16
    } else {
        app.editor.rows(input_width).len().clamp(1, MAX_INPUT_ROWS) as u16 + crate::attach::rows(app, false)
    };
    let paused = paused_peer(app);
    // The header: the name, a blank line, the tabs and a border.
    let [header, msgs, notice, _, input] = Layout::vertical([
        Constraint::Length(4),
        Constraint::Min(1),
        // A blank line above the notice.
        Constraint::Length(2 * paused.is_some() as u16),
        // A blank row keeps the last message off the message box.
        Constraint::Length(1),
        Constraint::Length(input_rows + 2),
    ])
    .areas(right);

    sidebar(f, app, side);
    // The Threads view takes the conversation's place, header and box included.
    if app.threads.open {
        crate::threads::draw(f, app, right);
    } else {
        conversation_header(f, app, header);
        messages(f, app, msgs);
        if let Some(name) = paused {
            paused_notice(f, app, &name, notice);
        }
        input_box(f, app, input);
    }
    status_line(f, app, status);
    profile(f, app, panel);
    crate::thread::draw(f, app, panel);
    reaction_card(f, app);
    // After the thread panel: they may sit over its reply box.
    emoji_popup(f, app);
    crate::mention::draw(f, app);
    crate::slash::draw(f, app);
    team_popup(f, app);
    menu(f, app);
    crate::account::draw(f, app);
    crate::details::draw(f, app);
    crate::invite::draw(f, app);
    react_picker(f, app);
    crate::edit::draw(f, app);
    viewer(f, app);
    crate::compose::draw(f, app);
    crate::search::draw(f, app);
    crate::help::draw(f, app);
    login(f, app);
}

/// Shows the start and end of a secret, enough to tell it was pasted right.
fn masked(s: &str) -> String {
    let n = s.chars().count();
    if n <= 16 {
        return s.to_string();
    }
    let head: String = s.chars().take(10).collect();
    let tail: String = s.chars().skip(n - 4).collect();
    trf!("{head}…{tail}  ({n} characters)", "{head}…{tail}  ({n} caracteres)")
}

/// slick's own four colors, the bars of the mark: left, top, bottom, right.
const MARK_BLUE: Color = Color::Rgb(0x46, 0x79, 0x96);
const MARK_GREEN: Color = Color::Rgb(0x59, 0x82, 0x66);
const MARK_RED: Color = Color::Rgb(0xB6, 0x5F, 0x4E);
const MARK_YELLOW: Color = Color::Rgb(0xE7, 0xBA, 0x79);
const SLACK_AUBERGINE: Color = Color::Rgb(0x4A, 0x15, 0x4B);

type Glyph = [&'static str; 6];
const S: Glyph = ["███████╗", "██╔════╝", "███████╗", "╚════██║", "███████║", "╚══════╝"];
const L: Glyph = ["██╗     ", "██║     ", "██║     ", "██║     ", "███████╗", "╚══════╝"];
const I: Glyph = ["██╗", "██║", "██║", "██║", "██║", "╚═╝"];
const C: Glyph = [" ██████╗", "██╔════╝", "██║     ", "██║     ", "╚██████╗", " ╚═════╝"];
const K: Glyph = ["██╗  ██╗", "██║ ██╔╝", "█████╔╝ ", "██╔═██╗ ", "██║  ██╗", "╚═╝  ╚═╝"];

/// The name, letter by letter in the mark's four colors.
fn slick() -> Vec<(Glyph, Style)> {
    [(S, MARK_BLUE), (L, MARK_GREEN), (I, MARK_RED), (C, MARK_YELLOW), (K, MARK_BLUE)]
        .map(|(g, c)| (g, Style::new().fg(c)))
        .to_vec()
}

/// The mark as a picture, for terminals that show real ones.
const MARK_PNG: &[u8] = include_bytes!("../assets/slick-256.png");
/// Its rows when the logo is drawn on screen.
pub(crate) const MARK_ROWS: u16 = 8;

/// The mark in text, for the other terminals: a # leaning forward, its four
/// bars woven over and under, one pixel per letter. Two pixels share a cell
/// as half blocks.
const MARK: [&str; 20] = [
    ".........bbb...yyy..",
    ".........bbb...yyy..",
    ".........bbb...yyy..",
    ".........bbb...yyy..",
    "........bbb...yyy...",
    "........bbb.........",
    ".gggggg.bbb.ggggggg.",
    ".gggggg.bbb.ggggggg.",
    ".ggggg.bbb.gggggggg.",
    ".......bbb..........",
    ".............yyy....",
    ".rrrrrrrrrrr.yyy.rr.",
    ".rrrrrrrrrr.yyy.rrr.",
    ".rrrrrrrrrr.yyy.rrr.",
    "............yyy.....",
    "......bbb...yyy.....",
    ".....bbb...yyy......",
    ".....bbb...yyy......",
    ".....bbb...yyy......",
    ".....bbb...yyy......",
];

pub(crate) fn mark() -> Vec<Line<'static>> {
    let color = |c: u8| match c {
        b'b' => Some(MARK_BLUE),
        b'g' => Some(MARK_GREEN),
        b'r' => Some(MARK_RED),
        b'y' => Some(MARK_YELLOW),
        _ => None,
    };
    MARK.chunks(2)
        .map(|pair| {
            let (top, bottom) = (pair[0].as_bytes(), pair[1].as_bytes());
            Line::from(
                top.iter()
                    .zip(bottom)
                    .map(|(&t, &b)| match (color(t), color(b)) {
                        (Some(t), Some(b)) if t == b => Span::styled("█", Style::new().fg(t)),
                        (Some(t), Some(b)) => Span::styled("▀", Style::new().fg(t).bg(b)),
                        (Some(t), None) => Span::styled("▀", Style::new().fg(t)),
                        (None, Some(b)) => Span::styled("▄", Style::new().fg(b)),
                        (None, None) => Span::raw(" "),
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
}

/// The mark as a picture, encoded once for its size.
pub(crate) fn draw_mark(f: &mut Frame, app: &mut App, rect: Rect) {
    let key = ("slick:mark".to_string(), rect.width, rect.height);
    if !app.protocols.contains_key(&key) {
        let Some(picker) = app.picker.as_ref() else { return };
        let Ok(img) = image::load_from_memory(MARK_PNG) else { return };
        let Ok(p) = picker.new_protocol(img.clone(), Rect::new(0, 0, rect.width, rect.height).into(), fit(&img)) else { return };
        app.protocols.insert(key.clone(), p);
    }
    f.render_widget(Image::new(&app.protocols[&key]), rect);
}

/// Big letters, six rows tall, only their first `cols` columns showing:
/// the rest stay as blanks so the centered name doesn't move as it prints.
fn banner(glyphs: &[(Glyph, Style)], cols: usize) -> Vec<Line<'static>> {
    (0..6)
        .map(|row| {
            let mut left = cols;
            Line::from(
                glyphs
                    .iter()
                    .map(|(g, st)| {
                        let text: String = g[row].chars().enumerate().map(|(i, c)| if i < left { c } else { ' ' }).collect();
                        left = left.saturating_sub(g[row].chars().count());
                        Span::styled(text, *st)
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
}

/// The name prints left to right when the login screen opens, column by
/// column like the site's logo, then stays still.
const PRINT: Duration = Duration::from_millis(600);

fn login(f: &mut Frame, app: &mut App) {
    let Some(l) = &app.login else { return };
    let area = f.area();
    f.render_widget(Clear, area);

    let mut lines = Vec::new();
    let mut print_next = None;
    let title: Vec<Line> = if area.width >= 37 {
        let glyphs = slick();
        let total: usize = glyphs.iter().map(|(g, _)| g[0].chars().count()).sum();
        let elapsed = l.shown.elapsed();
        let cols = if elapsed >= PRINT { total } else { (elapsed.as_micros() * total as u128 / PRINT.as_micros()) as usize };
        if cols < total {
            print_next = Some(l.shown + PRINT * (cols as u32 + 1) / total as u32);
        }
        banner(&glyphs, cols)
    } else {
        vec![Line::from(
            slick().into_iter().zip("slick".chars()).map(|((_, st), ch)| Span::styled(ch.to_string(), st.bold())).collect::<Vec<_>>(),
        )]
    };
    let manual = !l.token.is_empty() || !l.cookie.is_empty();
    // The mark only when there is room for it above everything else: a
    // picture where the terminal shows one, else drawn in half blocks.
    let cell = dot_cell(app);
    let mark_at = (area.height >= 36 && area.width >= 37).then(|| {
        let at = lines.len();
        match cell {
            Some(_) => lines.extend((0..MARK_ROWS).map(|_| Line::from(""))),
            None => lines.extend(mark()),
        }
        lines.push(Line::from(""));
        at
    });
    lines.extend(title);
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("Ultrafast, slim and beautiful Slack TUI", dim().italic())));
    lines.push(Line::from(""));
    lines.push(Line::from(""));

    let workspace = l.workspace.trim();
    let typing = !manual && !l.checking;
    let address = if workspace.is_empty() {
        Span::styled(tr("your-workspace", "seu-workspace"), dim())
    } else {
        Span::styled(workspace.to_string(), Style::new().bold())
    };
    lines.push(Line::from(vec![
        Span::styled("Workspace  ", dim()),
        address,
        Span::styled(if typing { "▏" } else { " " }, Style::new().fg(MARK_BLUE)),
        Span::styled(".slack.com", dim()),
    ]));
    lines.push(Line::from(""));

    let label = if l.checking { tr("  Waiting for sign-in…  ", "  Aguardando o login…  ") } else { tr("  Sign in to slick  ", "  Entrar no slick  ") };
    let button_row = lines.len();
    lines.push(Line::from(Span::styled(label, Style::new().fg(Color::White).bg(SLACK_AUBERGINE).bold())));
    let hint = if workspace.is_empty() {
        tr("Enter or click · opens Slack's sign-in in a window", "Enter ou clique · abre o login do Slack numa janela").to_string()
    } else {
        tr("Enter or click · sign in with email and password", "Enter ou clique · entrar com e-mail e senha").to_string()
    };
    lines.push(Line::from(Span::styled(hint, dim())));
    lines.push(Line::from(""));

    let field = |label: &str, value: &str, placeholder: &str, active: bool| {
        let marker = if active { Span::styled("› ", Style::new().fg(Color::Cyan).bold()) } else { Span::raw("  ") };
        let value = if value.is_empty() {
            Span::styled(placeholder.to_string(), dim())
        } else {
            Span::styled(masked(value), Style::new().bold())
        };
        Line::from(vec![marker, Span::styled(format!("{label:<9}"), Style::new().bold()), value])
    };
    // Pasting a session by hand still works; its fields only show once used.
    if manual {
        lines.push(field("Token", &l.token, "xoxc-…", l.field == 0));
        lines.push(field("Cookie d", &l.cookie, "xoxd-…", l.field == 1));
        lines.push(Line::from(""));
    }
    if let Some(m) = &l.message {
        let style = if l.checking { dim() } else { Style::new().fg(Color::Yellow) };
        lines.extend(m.lines().map(|m| Line::from(Span::styled(m.to_string(), style))));
    }

    let [body, footer] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(area);
    let top = body.y + (body.height.saturating_sub(lines.len() as u16)) / 2;
    let button_w = label.width() as u16;
    if let Some(next) = print_next {
        app.next_frame = Some(app.next_frame.map_or(next, |t| t.min(next)));
    }
    app.areas.login_button =
        Rect::new(body.x + body.width.saturating_sub(button_w) / 2, top + button_row as u16, button_w, 1).intersection(body);
    let content = Rect::new(body.x, top, body.width, body.height.saturating_sub(top - body.y));
    f.render_widget(Paragraph::new(lines).centered(), content);
    if let (Some(at), Some((fw, fh))) = (mark_at, cell) {
        let w = ((MARK_ROWS as u32 * fh) / fw.max(1)) as u16;
        let rect = Rect::new(body.x + body.width.saturating_sub(w) / 2, top + at as u16, w, MARK_ROWS).intersection(body);
        draw_mark(f, app, rect);
    }

    let back = if app.me.is_some() || !app.convs.is_empty() { tr("Esc back · ", "Esc volta · ") } else { "" };
    f.render_widget(Paragraph::new(format!("{back}{}", tr("Ctrl+C quits", "Ctrl+C sai"))).style(dim()).centered(), footer);
}

// ---- sidebar -----------------------------------------------------------

fn sidebar(f: &mut Frame, app: &mut App, area: Rect) {
    // The typing dots move: come back for their next step.
    if !app.typing.is_empty() {
        let at = Instant::now() + TYPING_STEP;
        app.next_frame = Some(app.next_frame.map_or(at, |t| t.min(at)));
    }
    let block = Block::new().borders(Borders::RIGHT).border_style(dim());
    let inner = block.inner(area);
    f.render_widget(block, area);
    app.areas.sidebar_border = Rect::new(area.right().saturating_sub(1), area.y, 1, area.height);
    // A bigger workspace icon takes two rows, the name beside it.
    let icon = app.team_icon.clone().filter(|_| app.picker.is_some());
    let head_h = if icon.is_some() { 2 } else { 1 };
    // A blank row under the name, then the search box: three rows tall,
    // its text on the middle one.
    let [team, _, search, _, list_area, account] = Layout::vertical([
        Constraint::Length(head_h),
        Constraint::Length(1),
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(crate::account::button_height(app)),
    ])
    .areas(inner);
    crate::account::button(f, app, account);

    let team_name = app.me.as_ref().map(|m| m.team.clone()).unwrap_or_else(|| "slick".into());
    let indent = if icon.is_some() { 6 } else { 1 };
    // Other workspaces: a stacked icon, or "⧉" after the name without one,
    // and what they wait with beside the arrow.
    let multi = app.workspaces.len() > 1;
    let others = crate::switcher::others(&app.workspaces);
    let stack = if multi && icon.is_none() { " ⧉" } else { "" };
    let mut badge = crate::switcher::badge_spans(others);
    if !badge.is_empty() {
        badge.insert(0, Span::raw(" "));
    }
    let badge_w: usize = badge.iter().map(|s| s.content.width()).sum();
    // Room on the right for the "new" button.
    let name = truncate(&team_name, (team.width as usize).saturating_sub(indent + 5 + stack.width() + badge_w));
    let arrow = if app.team_popup { " ▴" } else { " ▾" };
    let mut first = vec![Span::raw(" ".repeat(indent)), Span::raw(name.clone()).bold(), Span::styled(stack, dim()), Span::styled(arrow, dim())];
    first.extend(badge);
    let mut lines = vec![Line::from(first)];
    if icon.is_some() {
        let who = app.me.as_ref().map(|m| format!("@{}", m.user)).unwrap_or_default();
        lines.push(Line::from(vec![Span::raw(" ".repeat(indent)), Span::styled(truncate(&who, (team.width as usize).saturating_sub(indent)), dim())]));
    }
    f.render_widget(Paragraph::new(lines), team);
    app.areas.team_name = Rect::new(team.x, team.y, (indent + name.width() + stack.width() + 2 + badge_w) as u16, team.height).intersection(team);
    let new = Rect::new(team.right().saturating_sub(3), team.y, 3.min(team.width), 1);
    let new_style = if app.compose_hover { Style::new().fg(Color::Cyan).bold() } else { dim() };
    f.render_widget(Paragraph::new(format!(" {} ", crate::compose::NEW)).style(new_style), new);
    app.areas.compose.button = new;
    if let Some(icon) = icon {
        let domain = app.me.as_ref().map(|m| m.domain.clone()).unwrap_or_default();
        let plain = ImgRef::avatar(&format!("team-{domain}"), &icon);
        let rect = Rect::new(team.x + 1, team.y, 4, 2);
        let dot = matches!(others, crate::switcher::Badge::Count(_));
        match multi.then(|| stacked_icon(app, &plain.key, &domain, dot)).flatten() {
            Some(key) => {
                let r = ImgRef { key, url: String::new(), name: String::new(), link: None };
                draw_image(f, app, &r, Rect { height: 3, ..rect });
            }
            _ => draw_image(f, app, &plain, rect),
        }
    }

    // A rounded input a column in from each side, with "^K" at its right end.
    let search = Rect::new(search.x + 1, search.y, search.width.saturating_sub(2), search.height);
    let filtering = app.focus == Focus::Filter;
    // Text room: the frame and a column of padding on each side.
    let tw = (search.width as usize).saturating_sub(4);
    let (fill, hint) = if filtering || !app.filter.is_empty() {
        let room = tw.saturating_sub(4);
        (vec![Span::styled("🔍 ", dim()), Span::raw(truncate(&app.filter, room)), Span::raw("▏")], "")
    } else {
        let text = tr("Search", "Buscar");
        (vec![Span::styled("🔍 ", dim()), Span::styled(truncate(text, tw.saturating_sub(3)), dim())], "^K")
    };
    let used: usize = fill.iter().map(|s| s.content.width()).sum();
    let mut middle = fill;
    // "^K" only where it fits beside the placeholder.
    if used + hint.width() + 1 <= tw {
        middle.push(Span::raw(" ".repeat(tw - used - hint.width())));
        middle.push(Span::styled(hint, dim()));
    }
    rounded_input(f, search, filtering, Line::from(middle));
    app.areas.search = search;
    app.areas.sidebar = list_area;

    let rows = app.rows();
    app.fix_selection(&rows);
    let width = list_area.width as usize;
    let photos = app.sidebar_avatars && app.picker.is_some();
    let drag = app.drag.filter(|d| d.over.is_some());
    let target = drag.and_then(|d| App::section_of(&rows, d.over?));
    let mut pics = Vec::new();
    let items: Vec<ListItem> = rows
        .iter()
        .enumerate()
        .map(|(n, r)| {
            let (item, pic) = sidebar_row(app, r, width, photos);
            pics.extend(pic.map(|p| (n, p)));
            match drag {
                Some(d) if n == d.from => item.style(dim()),
                // The section the row would land in.
                _ if target.is_some() && App::section_of(&rows, n) == target => item.style(Style::new().bg(Color::Indexed(236))),
                _ => item,
            }
        })
        .collect();
    let focused = matches!(app.focus, Focus::Sidebar | Focus::Filter);
    // A quiet bar, like Slack's, instead of inverting the row's own colors.
    let list = List::new(items).highlight_style(if focused {
        Style::new().bg(SELECTED)
    } else {
        Style::new()
    });
    // ratatui scrolls a list back to its selected row on every draw, which
    // undid wheel scrolling past it; draw it unselected while scrolled away.
    let (off, h) = (app.list.offset(), list_area.height as usize);
    let away = app.list.selected().is_some_and(|s| s < off || s >= off + h);
    if app.sidebar_wheel && away {
        let mut state = ListState::default().with_offset(off);
        f.render_stateful_widget(list, list_area, &mut state);
    } else {
        f.render_stateful_widget(list, list_area, &mut app.list);
    }

    // Like Slack: a pill over the list's edge for each side with a mention
    // scrolled out of sight, or else a bold row ("More unreads"). The rows
    // under it skip their photo and dot.
    app.areas.mention_pills.clear();
    let (up, down) = offscreen_mentions(app, &rows, app.list.offset(), h);
    let (more_up, more_down) = offscreen_unreads(app, &rows, app.list.offset(), h);
    let up = up.map(|n| (n, true)).or(more_up.map(|n| (n, false)));
    let down = down.map(|n| (n, true)).or(more_down.map(|n| (n, false)));
    let under = |n: usize, offset: usize| (up.is_some() && n == offset) || (down.is_some() && n + 1 == offset + h);

    // Photos go over the blank cells the rows left for them.
    if photos {
        let offset = app.list.offset();
        for (n, row) in rows.iter().enumerate().skip(offset).take(list_area.height as usize) {
            let Row::Conv(i) = row else { continue };
            if under(n, offset) {
                continue;
            }
            let c = &app.convs[*i];
            // "•••" takes the photo's place while someone types (`sidebar_row`).
            if !app.typing_in(&c.id).is_empty() {
                continue;
            }
            let rect = Rect::new(list_area.x + 3, list_area.y + (n - offset) as u16, 2, 1);
            if c.kind == Kind::Group {
                // Drawn as a picture so the number sits in the middle of its two cells.
                let others = app.label(c).matches(", ").count() + 1;
                let (fw, fh) = app.picker.as_ref().map(|p| p.font_size()).map(|s| (s.width as u32, s.height as u32)).unwrap_or((8, 16));
                let key = format!("badge-{others}-{fw}x{fh}");
                app.images.entry(key.clone()).or_insert_with(|| Img::Ready(crate::icons::group_badge(others, 2 * fw, fh)));
                draw_image(f, app, &ImgRef { key, url: String::new(), name: String::new(), link: Default::default() }, rect);
                continue;
            }
            if c.kind != Kind::Direct {
                continue;
            }
            let Some(uid) = c.user.clone() else { continue };
            let Some(url) = app.users.avatars.get(&uid).cloned() else { continue };
            let px = app.picker.as_ref().map_or(0, |p| p.font_size().height as u32);
            draw_image(f, app, &ImgRef::avatar_px(&uid, &url, px), rect);
        }
    }
    // Custom status emoji go over the two cells their row left blank.
    let offset = app.list.offset();
    for (n, (x, img)) in pics {
        if n < offset || n >= offset + h || under(n, offset) || x as usize + 2 > width {
            continue;
        }
        draw_image(f, app, &img, Rect::new(list_area.x + x, list_area.y + (n - offset) as u16, 2, 1));
    }
    // Presence dots go over the cell each DM row left blank for them.
    if dot_cell(app).is_some() {
        let offset = app.list.offset();
        let x = list_area.x + if photos { 6 } else { 3 };
        for (n, row) in rows.iter().enumerate().skip(offset).take(list_area.height as usize) {
            let Row::Conv(i) = row else { continue };
            if under(n, offset) {
                continue;
            }
            let c = &app.convs[*i];
            let Some(uid) = c.user.clone().filter(|u| c.kind == Kind::Direct && !app.users.bots.contains(u)) else { continue };
            if !app.typing_in(&c.id).is_empty() {
                continue;
            }
            let p = presence_of(app, &uid);
            draw_presence(f, app, p, x, list_area.y + (n - offset) as u16);
        }
    }
    if let Some((n, mention)) = up {
        mentions_pill(f, app, list_area, true, mention, n);
    }
    if let Some((n, mention)) = down {
        mentions_pill(f, app, list_area, false, mention, n);
    }
}

/// The nearest rows with a mention badge above and below what the list
/// shows. Rows in collapsed sections are not in the list, and their header
/// shows no badge, so they don't count; neither does a search.
fn offscreen_mentions(app: &App, rows: &[Row], offset: usize, height: usize) -> (Option<usize>, Option<usize>) {
    if !app.filter.is_empty() || height < 2 {
        return (None, None);
    }
    let mention = |n: &usize| row_attention(app, &rows[*n]).1;
    let up = (0..offset.min(rows.len())).rev().find(mention);
    let down = (offset + height..rows.len()).find(mention);
    (up, down)
}

/// Whether a sidebar row is bold (unread, not muted) and whether it has a
/// mention badge: what the edge pills look for. Rows other than
/// conversations that can be unread (the "Threads" row) answer here too.
pub(crate) fn row_attention(app: &App, row: &Row) -> (bool, bool) {
    match row {
        Row::Conv(i) => {
            let c = &app.convs[*i];
            let (unread, mentions) = app.unread(&c.id);
            (unread && !app.muted(c), mentions > 0)
        }
        Row::Threads => (app.threads.unread, app.threads.mentions > 0),
        // A collapsed section's header isn't a row to scroll to.
        _ => (false, false),
    }
}

/// The nearest bold rows (unread, not muted) above and below what the list
/// shows, for "More unreads"; same rules as `offscreen_mentions`.
fn offscreen_unreads(app: &App, rows: &[Row], offset: usize, height: usize) -> (Option<usize>, Option<usize>) {
    if !app.filter.is_empty() || height < 2 {
        return (None, None);
    }
    let bold = |n: &usize| row_attention(app, &rows[*n]).0;
    let up = (0..offset.min(rows.len())).rev().find(bold);
    let down = (offset + height..rows.len()).find(bold);
    (up, down)
}

/// Slack's "↑ Unread mentions": a red pill, like the mention badge, over
/// the list's top (or bottom) row; a click brings row `n` into view. Without
/// a `mention` it's "↑ More unreads", in the reactions' quieter gray.
fn mentions_pill(f: &mut Frame, app: &mut App, area: Rect, up: bool, mention: bool, n: usize) {
    let text = match (up, mention) {
        (true, true) => tr(" ↑ Unread mentions ", " ↑ Menções não lidas "),
        (false, true) => tr(" ↓ Unread mentions ", " ↓ Menções não lidas "),
        (true, false) => tr(" ↑ More unreads ", " ↑ Mais não lidas "),
        (false, false) => tr(" ↓ More unreads ", " ↓ Mais não lidas "),
    };
    let (fg, bg) = if mention { (Color::White, Color::Red) } else { (Color::Indexed(252), Color::Indexed(238)) };
    let w = text.width() as u16 + 2;
    if area.width < w {
        return;
    }
    let y = if up { area.y } else { area.bottom() - 1 };
    let rect = Rect::new(area.x + (area.width - w) / 2, y, w, 1);
    let pill = Line::from(vec![
        Span::styled(blocks::CAP_L, Style::new().fg(bg)),
        Span::styled(text, Style::new().fg(fg).bg(bg).bold()),
        Span::styled(blocks::CAP_R, Style::new().fg(bg)),
    ]);
    f.render_widget(Clear, rect);
    f.render_widget(Paragraph::new(pill), rect);
    app.areas.mention_pills.push((rect, n));
}

/// The emoji of someone's status, as Slack shows it next to their name;
/// a status with only text gets a speech balloon. A custom emoji comes as
/// two blank cells and the picture to draw over them.
fn status_emoji(app: &App, uid: &str) -> Option<(String, Option<ImgRef>)> {
    let (emoji, text) = app.users.status(uid)?;
    let name = emoji.trim_matches(':');
    if name.is_empty() {
        return (!text.is_empty()).then(|| ("💬".into(), None));
    }
    Some(named_status_emoji(app, uid, name))
}

/// A status emoji by name: what Slack sent with the status when it is
/// theirs, the workspace's emoji (aliases resolved), else the picture Slack
/// sent. Without graphics a picture stays `:name:`.
fn named_status_emoji(app: &App, uid: &str, name: &str) -> (String, Option<ImgRef>) {
    let theirs = app.users.status(uid).is_some_and(|(e, _)| e.trim_matches(':') == name);
    if let Some(e) = app.users.status_glyph(uid).filter(|_| theirs) {
        return (e, None);
    }
    let sent = || app.users.status_image.get(uid).filter(|_| theirs).map(|url| emoji::Glyph::Image(emoji::img(name, url)));
    match app.emoji.glyph(name).or_else(sent) {
        Some(emoji::Glyph::Text(e)) => (e, None),
        Some(emoji::Glyph::Image(img)) if app.picker.is_some() => ("  ".into(), Some(img)),
        _ => (format!(":{name}:"), None),
    }
}

/// A sidebar row, and the picture of a custom status emoji with the column
/// it goes over.
fn sidebar_row<'a>(app: &App, row: &Row, width: usize, photos: bool) -> (ListItem<'a>, Option<(u16, ImgRef)>) {
    match row {
        Row::Gap => (ListItem::new(""), None),
        Row::Threads => (crate::threads::sidebar_row(app, width), None),
        Row::Header { title, collapsed, unread, .. } => {
            let arrow = if *collapsed { "▸" } else { "▾" };
            let style = if *unread { Style::new().bold() } else { dim().bold() };
            (ListItem::new(Line::from(format!(" {arrow} {title}")).style(style)), None)
        }
        Row::Conv(i) => conv_row(app, &app.convs[*i], width, photos),
        // A channel you are not in, found by searching: drawn as the one its preview opens.
        Row::Other(j) => conv_row(app, &app.directory.conv(*j), width, photos),
    }
}

/// A conversation's row in the sidebar (`sidebar_row`).
fn conv_row<'a>(app: &App, c: &Conversation, width: usize, photos: bool) -> (ListItem<'a>, Option<(u16, ImgRef)>) {
    let (unread, mentions) = app.unread(&c.id);
    // Like Slack: a muted channel stays dim; only a mention badges it.
    let unread = unread && !app.muted(c);
    let uid = c.user.as_deref().unwrap_or("");
    let bot = app.users.bots.contains(uid);
    let dot = presence_span(app, presence_of(app, uid));
    // Like Slack: a DM or group where someone is typing shows "•••"
    // where the photo (or presence) goes.
    let typing = matches!(c.kind, Kind::Direct | Kind::Group) && !app.typing_in(&c.id).is_empty();
    let icon = match c.kind {
        // Sized here, drawn below (`typing_dots`).
        _ if typing => Span::raw(if photos { "•••" } else { "•" }),
        // Archived (found by searching): Slack's box in place of # or the lock.
        _ if c.archived => Span::styled(ARCHIVE, dim()),
        Kind::Channel => Span::styled("#", dim()),
        Kind::Private => Span::styled(LOCK, dim()),
        // The photo is drawn over the first two cells; presence goes after a gap.
        // Apps have no presence: the name comes right after the photo.
        Kind::Direct if bot => Span::raw(if photos { "  " } else { " " }),
        Kind::Direct if photos => Span::styled(format!("   {}", dot.content), dot.style),
        Kind::Direct => dot,
        // Like Slack: how many other people are in the group, drawn over these cells.
        Kind::Group if photos => Span::raw("  "),
        Kind::Group => Span::styled("◉", dim()),
    };
    // Photo mode gives people five cells (photo, gap, presence, gap); channels
    // keep their compact look either way.
    let channel = matches!(c.kind, Kind::Channel | Kind::Private);
    let cells = if bot { 3 } else { 5 };
    let pad = if photos && !channel { " ".repeat(cells - icon.content.width().min(cells)) } else { " ".into() };
    let icon_w = icon.content.width() + pad.width();
    let badge = (mentions > 0).then(|| format!(" {mentions} "));
    let (status, pic) = match status_emoji(app, uid).filter(|_| c.kind == Kind::Direct) {
        Some((e, pic)) => (Some(format!(" {e}")), pic),
        None => (None, None),
    };
    let status_w = status.as_ref().map_or(0, |s| s.width());
    let archived = app.gone_suffix(c);
    let archived_w = archived.map_or(0, |a| a.width());
    // The badge is a pill: its text plus a rounded cap on each side.
    let room = width.saturating_sub(3 + icon_w + status_w + archived_w + badge.as_ref().map_or(0, |b| b.width() + 3));
    let label = truncate(&app.label(c), room);
    let is_current = app.current.as_deref() == Some(c.id.as_str());
    let style = if is_current {
        Style::new().fg(Color::Cyan).bold()
    } else if app.users.deleted.contains(uid) || c.archived || c.guest {
        // A deactivated account, an archived channel or one you are not in (found by searching).
        dim()
    } else if unread {
        Style::new().fg(Color::White).bold()
    } else {
        Style::new().fg(Color::Gray)
    };
    let mut spans = vec![Span::raw("   ")];
    if typing {
        spans.extend(typing_dots(app, photos));
    } else {
        spans.push(icon);
    }
    spans.push(Span::raw(pad));
    // The own DM's " (you)" is quieter than the name, even on the open row.
    let name_len = app.label(c).chars().count() - app.you_suffix(c).map_or(0, |y| y.chars().count());
    // Like Slack: what the search matched, in bold.
    // Ranges come from the whole name; a cut name keeps its first chars.
    match crate::text::find(&app.label(c), &app.filter).filter(|_| !app.filter.is_empty()) {
        Some((_, ranges)) => {
            let hl = style.fg(if is_current { Color::Cyan } else { Color::White }).bold();
            let chars: Vec<char> = label.chars().collect();
            let mut bold = vec![false; chars.len()];
            for (a, b) in ranges {
                bold[a.min(chars.len())..b.min(chars.len())].iter_mut().for_each(|x| *x = true);
            }
            let mut start = 0;
            for i in 1..=chars.len() {
                if i == chars.len() || i == name_len || bold[i] != bold[start] {
                    let s: String = chars[start..i].iter().collect();
                    let st = if start >= name_len { dim() } else if bold[start] { hl } else { style };
                    spans.push(Span::styled(s, st));
                    start = i;
                }
            }
        }
        None => {
            let cut = label.char_indices().nth(name_len).map_or(label.len(), |(b, _)| b);
            spans.push(Span::styled(label[..cut].to_string(), style));
            if cut < label.len() {
                spans.push(Span::styled(label[cut..].to_string(), dim()));
            }
        }
    }
    spans.extend(archived.map(|a| Span::styled(a, dim())));
    spans.extend(status.map(Span::raw));
    // After the name and the space before the emoji's blank cells.
    let pic = pic.map(|img| ((3 + icon_w + label.width() + archived_w + 1) as u16, img));
    if let Some(b) = badge {
        let used = 3 + icon_w + label.width() + archived_w + status_w;
        spans.push(Span::raw(" ".repeat(width.saturating_sub(used + b.width() + 2))));
        spans.push(Span::styled(blocks::CAP_L, Style::new().fg(Color::Red)));
        spans.push(Span::styled(b, Style::new().fg(Color::White).bg(Color::Red).bold()));
        spans.push(Span::styled(blocks::CAP_R, Style::new().fg(Color::Red)));
    }
    (ListItem::new(Line::from(spans)), pic)
}

// ---- conversation --------------------------------------------------------

fn conversation_header(f: &mut Frame, app: &mut App, area: Rect) {
    app.areas.header_star = Rect::default();
    app.areas.header_user = None;
    app.areas.header_name = Rect::default();
    app.areas.pins.tabs.clear();
    let block = Block::new().borders(Borders::BOTTOM).border_style(dim());
    let inner = block.inner(area);
    f.render_widget(block, area);
    let Some(c) = app.current.as_ref().and_then(|id| app.convs.iter().find(|c| &c.id == id)) else {
        return;
    };
    let uid = c.user.clone().unwrap_or_default();
    let icon = match c.kind {
        _ if c.archived => Span::styled(format!("{ARCHIVE} "), dim()),
        Kind::Channel => Span::styled("# ", dim()),
        Kind::Private => Span::styled(format!("{LOCK} "), dim()),
        Kind::Direct if app.users.bots.contains(&uid) => Span::raw(""),
        Kind::Direct => {
            let dot = presence_span(app, presence_of(app, &uid));
            Span::styled(format!("{} ", dot.content), dot.style)
        }
        Kind::Group => Span::styled("◉ ", dim()),
    };
    // Like Slack: a star to click, filled when the conversation is a favourite;
    // none for a channel you are not in.
    let star = if c.guest {
        Span::raw(" ")
    } else if app.stars.contains(&c.id) {
        Span::styled(STAR_ON, Style::new().fg(Color::Yellow))
    } else {
        Span::styled(STAR_OFF, dim())
    };
    let (status, pic) = match status_emoji(app, &uid).filter(|_| c.kind == Kind::Direct) {
        Some((e, pic)) => (Some(format!(" {e}")), pic),
        None => (None, None),
    };
    // The own DM's " (you)" is quieter than the name.
    let (label, you) = (app.label(c), app.you_suffix(c).unwrap_or_default());
    let name = label.strip_suffix(you).unwrap_or(&label);
    // Same for an archived channel's " (archived)", a deactivated account's " (deactivated)".
    let you = app.gone_suffix(c).unwrap_or(you);
    let line = Line::from(vec![
        Span::raw(" "),
        star,
        Span::raw("  "),
        icon,
        Span::styled(name.to_string(), Style::new().bold()),
        Span::styled(you, dim()),
        Span::raw(status.unwrap_or_default()),
    ]);
    let name_x = inner.x + 4 + line.spans[3].width() as u16;
    let name_w = (line.spans[4].width() + line.spans[5].width()) as u16;
    let icon_w = line.spans[3].width() as u16;
    let pic_x = inner.x + line.spans[..6].iter().map(|s| s.width() as u16).sum::<u16>() + 1;
    f.render_widget(Paragraph::new(line), inner);
    if !c.guest {
        app.areas.header_star = Rect::new(inner.x, inner.y, 3, 1);
    }
    let person = (c.kind == Kind::Direct && !app.users.bots.contains(&uid)).then(|| uid.clone());
    if c.kind == Kind::Direct {
        app.areas.header_user = Some((Rect::new(name_x, inner.y, name_w, 1), uid));
    } else {
        // The icon too: Slack's whole name button opens the details.
        app.areas.header_name = Rect::new(name_x - icon_w, inner.y, name_w + icon_w, 1);
    }
    if let Some(uid) = person {
        let p = presence_of(app, &uid);
        draw_presence(f, app, p, inner.x + 4, inner.y);
    }
    if let Some(img) = pic.filter(|_| pic_x + 2 <= inner.right()) {
        draw_image(f, app, &img, Rect::new(pic_x, inner.y, 2, 1));
    }
    crate::pins::tabs(f, app, area);
}

/// A reserved block of blank lines where an image goes.
pub(crate) struct Slot {
    line: usize,
    x: u16,
    cols: u16,
    rows: u16,
    img: ImgRef,
    /// What a click enlarges; avatars have none.
    full: Option<ImgRef>,
    /// Whose avatar it is; a click opens their profile.
    user: Option<String>,
}

/// A line that opens a thread when clicked, by the parent's ts.
pub(crate) struct Link {
    line: usize,
    x: u16,
    w: u16,
    ts: String,
}

/// Messages ready to show: the lines, the images over them and the thread links.
pub(crate) struct Rendered {
    pub lines: Vec<Line<'static>>,
    slots: Vec<Slot>,
    links: Vec<Link>,
    /// Names that open a profile: (line, column, width, user id).
    people: Vec<(usize, u16, u16, String)>,
    /// Reaction pills and message rows.
    hits: Vec<Hit>,
    /// Block Kit buttons and links.
    buttons: Vec<blocks::Hit>,
    /// File cards' actions.
    files: Vec<files::Hit>,
    /// Joins and leaves folded into one line: its lines, once per message,
    /// to scroll to them; nothing to pick or react to there.
    quiet: Vec<(std::ops::Range<usize>, String)>,
}

impl Rendered {
    /// Drops the first `n` lines and what sits on them, as a pane that shows
    /// only part of what `message_lines` laid out (the Threads view's cards).
    pub(crate) fn drop_front(&mut self, n: usize) {
        let n = n.min(self.lines.len());
        self.lines.drain(..n);
        self.slots.retain(|s| s.line >= n);
        self.slots.iter_mut().for_each(|s| s.line -= n);
        self.links.retain(|l| l.line >= n);
        self.links.iter_mut().for_each(|l| l.line -= n);
        self.people.retain(|p| p.0 >= n);
        self.people.iter_mut().for_each(|p| p.0 -= n);
        self.hits.retain_mut(|h| match h {
            Hit::Pill { line, .. } if *line >= n => {
                *line -= n;
                true
            }
            Hit::Message { lines, .. } if lines.end > n => {
                *lines = lines.start.saturating_sub(n)..lines.end - n;
                true
            }
            _ => false,
        });
        self.buttons.retain(|b| b.line >= n);
        self.buttons.iter_mut().for_each(|b| b.line -= n);
        self.files.retain(|h| h.line >= n);
        self.files.iter_mut().for_each(|h| h.line -= n);
        self.quiet.retain(|q| q.0.start >= n);
        self.quiet.iter_mut().for_each(|q| q.0 = q.0.start - n..q.0.end - n);
    }

    /// Each message header's time, reworded by `f` from the message's ts
    /// ("2 minutes ago" in the Threads view).
    pub(crate) fn retime(&mut self, f: impl Fn(&str) -> Option<String>) {
        for h in &self.hits {
            let Hit::Message { lines, ts } = h else { continue };
            let (Some(new), old) = (f(ts), format!("  {}", hhmm(ts))) else { continue };
            let header = self.lines[lines.clone()].iter_mut().flat_map(|l| l.spans.iter_mut()).find(|s| s.content == old);
            if let Some(span) = header {
                span.content = format!("  {new}").into();
            }
        }
    }

    /// Only drawn: no thread links, reaction pills nor messages to pick.
    pub(crate) fn inert(&mut self) {
        self.links.clear();
        self.hits.clear();
    }

    /// The lines the message `ts` is drawn on, folded into a line of joins or not.
    fn span_of(&self, ts: &str) -> Option<std::ops::Range<usize>> {
        let hit = self.hits.iter().find_map(|h| match h {
            Hit::Message { lines, ts: t } if t == ts => Some(lines.clone()),
            _ => None,
        });
        hit.or_else(|| self.quiet.iter().find(|q| q.1 == ts).map(|q| q.0.clone()))
    }

    /// How far up to scroll so the message `ts` starts at the top of `height` rows.
    pub fn scroll_to(&self, ts: &str, height: usize) -> Option<usize> {
        self.span_of(ts).map(|lines| self.lines.len().saturating_sub(lines.start + height))
    }

    /// The scroll that keeps the end of the message `ts` `over` lines above the bottom.
    pub fn keep(&self, ts: &str, over: usize) -> Option<usize> {
        self.span_of(ts).map(|lines| self.lines.len() - lines.end + over)
    }

    /// Puts `other` below these lines.

    pub fn append(&mut self, other: Rendered) {
        let n = self.lines.len();
        self.lines.extend(other.lines);
        self.slots.extend(other.slots.into_iter().map(|s| Slot { line: s.line + n, ..s }));
        self.links.extend(other.links.into_iter().map(|l| Link { line: l.line + n, ..l }));
        self.people.extend(other.people.into_iter().map(|(line, x, w, uid)| (line + n, x, w, uid)));
        self.hits.extend(other.hits.into_iter().map(|h| match h {
            Hit::Pill { line, x, w, ts, name } => Hit::Pill { line: line + n, x, w, ts, name },
            Hit::Message { lines, ts } => Hit::Message { lines: lines.start + n..lines.end + n, ts },
        }));
        self.buttons.extend(other.buttons.into_iter().map(|b| blocks::Hit { line: b.line + n, ..b }));
        self.files.extend(other.files.into_iter().map(|h| files::Hit { line: h.line + n, ..h }));
        self.quiet.extend(other.quiet.into_iter().map(|(l, ts)| (l.start + n..l.end + n, ts)));
    }
}

fn messages(f: &mut Frame, app: &mut App, area: Rect) {
    app.areas.messages = area;
    app.areas.images.clear();
    app.areas.avatars.clear();
    app.areas.thread_links.clear();
    app.areas.reactions.clear();
    app.areas.message_rows.clear();
    app.areas.buttons.clear();
    app.areas.file_buttons.clear();
    app.areas.latest = Rect::default();
    let inner = area.inner(ratatui::layout::Margin { horizontal: 1, vertical: 0 });
    let Some(chan) = app.current.clone() else {
        let help = vec![
            Line::from(""),
            Line::from(tr(
                "  ↑↓ j k    navigate            Enter / click   open or collapse a section",
                "  ↑↓ j k    navegar             Enter / clique  abrir ou recolher pasta",
            )),
            Line::from(tr(
                "  Ctrl+K /  find a conversation Shift+J/K       move a section (all over Slack)",
                "  Ctrl+K /  procurar conversa   Shift+J/K       mover pasta (vale no Slack todo)",
            )),
            Line::from(tr(
                "  PgUp PgDn scroll (or wheel)   Shift+Enter     new line",
                "  PgUp PgDn rolar (ou a roda)   Shift+Enter     nova linha",
            )),
            Line::from(tr(
                "  Ctrl+B    bold                Ctrl+I          italic",
                "  Ctrl+B    negrito             Ctrl+I          itálico",
            )),
            Line::from(tr(
                "  Alt+X     strikethrough       Alt+C           code",
                "  Alt+X     riscado             Alt+C           código",
            )),
            Line::from(tr(
                "  Tab       list ↔ message      click an image  enlarge",
                "  Tab       lista ↔ mensagem    clique na imagem  ampliar",
            )),
            Line::from(tr(
                "  Ctrl+R    reload              q / Ctrl+C      quit",
                "  Ctrl+R    recarregar          q / Ctrl+C      sair",
            )),
            Line::from(tr(
                "  Ctrl+P    account and status  Ctrl+N          new message or channel",
                "  Ctrl+P    sua conta e status  Ctrl+N          nova mensagem ou canal",
            )),
            Line::from(tr("  Ctrl+/    all shortcuts", "  Ctrl+/    todos os atalhos")),
        ];
        f.render_widget(Paragraph::new(help).style(dim()), inner);
        return;
    };
    if app.pins.tab == crate::pins::Tab::Pinned {
        return crate::pins::draw(f, app, inner, &chan);
    }
    let Some(msgs) = app.messages.get(&chan) else {
        f.render_widget(Paragraph::new(tr("loading…", "carregando…")).style(dim()), inner);
        return;
    };

    let intro = if app.no_older.contains(&chan) { intro(app, &chan) } else { None };
    let names = app.names();
    let font = app.picker.as_ref().map(|p| p.font_size()).map(|s| (s.width, s.height));
    let new_at = app.new_line.as_ref().filter(|n| n.chan == chan).and_then(|n| n.at.clone());
    let mut rendered = lines_with(msgs, intro, &names, app, inner.width as usize, font, new_at.as_deref(), !app.compact);
    // Opened with unread messages above the view: up to the "New" line, near
    // the top, as Slack opens at the first unread.
    if let Some(n) = app.new_line.as_mut().filter(|n| n.jump && n.chan == chan) {
        n.jump = false;
        let first = msgs.iter().find(|m| new_at.as_deref().is_some_and(|t| m.ts.as_str() >= t));
        if let Some(s) = first.and_then(|m| rendered.scroll_to(&m.ts, (inner.height as usize).saturating_sub(3))) {
            app.scroll = app.scroll.max(s);
        }
    }
    // Coming from "Fixados": show the message picked there, a third down.
    if let Some(s) = app.pins.jump.take().and_then(|ts| rendered.scroll_to(&ts, inner.height as usize * 2 / 3)) {
        app.scroll = s;
    }
    // Newer messages loaded below: what was on screen stays put.
    if let Some(s) = app.stay.take().and_then(|(ts, over)| rendered.keep(&ts, over)) {
        app.scroll = s;
    }
    app.scroll = select(app, &mut rendered, false, inner, app.scroll);
    flash(app, &mut rendered, inner);
    shade(app, &mut rendered, false, inner);
    app.max_scroll = place(f, app, inner, rendered, app.scroll, true);
    app.scroll = app.scroll.min(app.max_scroll);
    if app.scroll > inner.height as usize / 2 || app.detached.contains(&chan) {
        latest_button(f, app, inner);
    }
}

/// Slack's "↓ Latest messages": a pill over the bottom of the conversation.
fn latest_button(f: &mut Frame, app: &mut App, area: Rect) {
    let blue = Color::Rgb(0x12, 0x64, 0xA3);
    let text = tr(" ↓ Latest messages ", " ↓ Últimas mensagens ");
    let w = text.width() as u16 + 2;
    if area.height < 2 || area.width < w {
        return;
    }
    let rect = Rect::new(area.x + (area.width - w) / 2, area.bottom() - 1, w, 1);
    let pill = Line::from(vec![
        Span::styled("\u{e0b6}", Style::new().fg(blue)),
        Span::styled(text, Style::new().fg(Color::White).bg(blue).bold()),
        Span::styled("\u{e0b4}", Style::new().fg(blue)),
    ]);
    f.render_widget(ratatui::widgets::Clear, rect);
    f.render_widget(Paragraph::new(pill), rect);
    app.areas.latest = rect;
}

/// Paints the message just jumped to, for a moment after the jump.
pub(crate) fn flash(app: &App, r: &mut Rendered, area: Rect) {
    let Some((ts, _)) = app.flash.as_ref() else { return };
    let Some(lines) = r.span_of(ts) else { return };
    let start = (lines.start..lines.end).find(|&i| r.lines[i].width() > 0).unwrap_or(lines.start);
    let bg = Color::Rgb(0x4a, 0x41, 0x1c);
    for l in &mut r.lines[start..lines.end] {
        let pad = (area.width as usize).saturating_sub(l.width());
        l.spans.push(Span::raw(" ".repeat(pad)));
        for s in l.spans.iter_mut().filter(|s| s.style.bg.is_none()) {
            s.style = s.style.bg(bg);
        }
    }
}

/// Paints the picked message of a pane and, right after it was picked,
/// returns the scroll that brings it into view.
pub(crate) fn select(app: &mut App, r: &mut Rendered, thread: bool, area: Rect, scroll: usize) -> usize {
    let Some(sel) = app.edit.selected.as_ref().filter(|s| s.thread == thread) else { return scroll };
    let Some(lines) = r.hits.iter().find_map(|h| match h {
        Hit::Message { lines, ts } if *ts == sel.ts => Some(lines.clone()),
        _ => None,
    }) else {
        return scroll;
    };
    // The blank line above a new author is spacing, not the message.
    let start = (lines.start..lines.end).find(|&i| r.lines[i].width() > 0).unwrap_or(lines.start);
    let bg = Color::Indexed(236);
    for l in &mut r.lines[start..lines.end] {
        let pad = (area.width as usize).saturating_sub(l.width());
        l.spans.push(Span::raw(" ".repeat(pad)));
        // A bar in the gutter, where the avatar leaves room for it.
        if let Some(first) = l.spans.first_mut().filter(|s| s.content.starts_with(' ')) {
            first.content = first.content[1..].to_string().into();
            l.spans.insert(0, Span::styled("▌", Style::new().fg(Color::Cyan)));
        }
        for s in l.spans.iter_mut().filter(|s| s.style.bg.is_none()) {
            s.style = s.style.bg(bg);
        }
    }
    if !std::mem::take(&mut app.edit.reveal) {
        return scroll;
    }
    crate::edit::reveal(scroll, r.lines.len(), area.height as usize, start..lines.end)
}

/// Shades the messages dragged over in a pane, leaving the avatar column alone.
pub(crate) fn shade(app: &App, r: &mut Rendered, thread: bool, area: Rect) {
    let Some(sel) = app.selection.as_ref().filter(|s| s.thread == thread && s.moved) else { return };
    let msgs: Vec<_> = r
        .hits
        .iter()
        .filter_map(|h| match h {
            Hit::Message { lines, ts } => Some((lines.clone(), ts.clone())),
            _ => None,
        })
        .collect();
    let at = |ts: &str| msgs.iter().position(|(_, t)| t == ts);
    let (Some(a), Some(b)) = (at(&sel.anchor), at(&sel.head)) else { return };
    let bg = Color::Indexed(236);
    let indent = |l: &Line| l.spans.first().filter(|s| !s.content.is_empty() && s.content.trim().is_empty()).map(|s| s.width());
    for (lines, _) in &msgs[a.min(b)..=a.max(b)] {
        let start = lines.clone().find(|&i| r.lines[i].width() > 0).unwrap_or(lines.start);
        let gutter = r.lines[start..lines.end].iter().find_map(indent).unwrap_or(0);
        for l in &mut r.lines[start..lines.end] {
            if l.width() == 0 {
                l.spans = vec![Span::raw(" ".repeat(gutter))];
            }
            let pad = (area.width as usize).saturating_sub(l.width());
            l.spans.push(Span::raw(" ".repeat(pad)));
            let skip = indent(l).is_some() as usize;
            for s in l.spans.iter_mut().skip(skip).filter(|s| s.style.bg.is_none()) {
                s.style = s.style.bg(bg);
            }
        }
    }
}

/// Shows rendered messages `scroll` lines up from the end, draws their images
/// and registers what can be clicked. Returns how far `scroll` can go.
pub(crate) fn place(f: &mut Frame, app: &mut App, inner: Rect, r: Rendered, scroll: usize, bottom: bool) -> usize {
    let Rendered { lines, slots, links, people, hits, buttons, files, .. } = r;
    let height = inner.height as usize;
    let max_scroll = lines.len().saturating_sub(height);
    let end = lines.len() - scroll.min(max_scroll);
    let start = end.saturating_sub(height);
    // Few messages: sit them at the bottom, like a chat.
    let pad = if bottom { height - (end - start) } else { 0 };
    let mut visible = vec![Line::from(""); pad];
    visible.extend_from_slice(&lines[start..end]);
    f.render_widget(Paragraph::new(visible), inner);

    for s in slots {
        if s.line < start || s.line + s.rows as usize > end {
            continue;
        }
        let rect = Rect::new(inner.x + s.x, inner.y + (pad + s.line - start) as u16, s.cols, s.rows);
        draw_image(f, app, &s.img, rect);
        if let Some(full) = s.full {
            app.areas.images.push((rect, full));
        }
        if let Some(uid) = s.user {
            app.areas.avatars.push((rect, uid));
        }
    }
    // Names open the profile too, like the photos.
    for (line, x, w, uid) in people.into_iter().filter(|p| (start..end).contains(&p.0)) {
        app.areas.avatars.push((Rect::new(inner.x + x, inner.y + (pad + line - start) as u16, w, 1).intersection(inner), uid));
    }
    for l in links.into_iter().filter(|l| (start..end).contains(&l.line)) {
        let rect = Rect::new(inner.x + l.x, inner.y + (pad + l.line - start) as u16, l.w, 1).intersection(inner);
        app.areas.thread_links.push((rect, l.ts));
    }
    let row = |line: usize| inner.y + (pad + line - start) as u16;
    let pane = app.areas.thread.contains(Position::new(inner.x, inner.y));
    let hover = app.hover_msg.clone().filter(|(t, _)| *t == pane).map(|(_, ts)| ts);
    for h in hits {
        match h {
            Hit::Pill { line, x, w, ts, name } if line >= start && line < end => {
                app.areas.reactions.push((Rect::new(inner.x + x, row(line), w, 1).intersection(inner), ts, name));
            }
            Hit::Message { lines, ts } => {
                let (a, b) = (lines.start.max(start), lines.end.min(end));
                if a < b {
                    // Under the mouse: Slack's add-reaction button, top right.
                    if hover.as_deref() == Some(ts.as_str()) {
                        let (fg, bg) = PILL_ADD;
                        let label = format!(" {ADD_REACTION} ");
                        let w = label.width() as u16 + 2;
                        let rect = Rect::new(inner.right().saturating_sub(w + 1), row(a), w, 1).intersection(inner);
                        let line = Line::from(vec![
                            Span::styled(blocks::CAP_L, Style::new().fg(bg)),
                            Span::styled(label, Style::new().fg(fg).bg(bg)),
                            Span::styled(blocks::CAP_R, Style::new().fg(bg)),
                        ]);
                        f.render_widget(Clear, rect);
                        f.render_widget(Paragraph::new(line), rect);
                        app.areas.reactions.push((rect, ts.clone(), None));
                    }
                    app.areas.message_rows.push((Rect::new(inner.x, row(a), inner.width, (b - a) as u16), ts));
                }
            }
            _ => {}
        }
    }
    for h in buttons.into_iter().filter(|h| h.line >= start && h.line < end) {
        let named = match &h.target {
            blocks::Target::User(id) => app.users.names.contains_key(id) || app.others.contains_key(id),
            blocks::Target::Channel(id) => app.chans.contains_key(id) || app.others.contains_key(id),
            _ => true,
        };
        if !named {
            app.unnamed.push(h.target.clone());
        }
        let rect = Rect::new(inner.x + h.x, inner.y + (pad + h.line - start) as u16, h.w, 1).intersection(inner);
        app.areas.buttons.push((rect, h.target));
    }
    for h in files.into_iter().filter(|h| h.line >= start && h.line < end) {
        let rect = Rect::new(inner.x + h.x, inner.y + (pad + h.line - start) as u16, h.w, 1).intersection(inner);
        app.areas.file_buttons.push((rect, h.click));
    }
    max_scroll
}

/// The other person of the open DM, when they paused their notifications.
fn paused_peer(app: &App) -> Option<String> {
    let c = app.current.as_ref().and_then(|id| app.convs.iter().find(|c| &c.id == id))?;
    let uid = c.user.as_deref().filter(|u| c.kind == Kind::Direct && !app.users.deleted.contains(*u))?;
    (app.account.others_paused.get(uid) == Some(&true)).then(|| app.label(c))
}

/// Slack's quiet line over the message box: "Jane has paused their notifications."
fn paused_notice(f: &mut Frame, app: &App, name: &str, area: Rect) {
    // Lined up with the messages' text: their margin plus the avatar's gutter.
    let font = app.picker.as_ref().map(|p| p.font_size()).map(|s| (s.width, s.height));
    let gutter = font.map_or(2, |(fw, fh)| ((2.0 * fh as f64 / fw as f64).round() as usize).clamp(3, 6) + 1);
    let pad = " ".repeat(1 + gutter);
    let text = trf!("{pad}{name} has paused their notifications.", "{pad}{name} pausou as próprias notificações.");
    let line = Line::from(Span::styled(text, dim().italic()));
    f.render_widget(Paragraph::new(line), Rect { y: area.bottom().saturating_sub(1), height: 1, ..area }.intersection(area));
}

/// What Slack shows above the first message of a conversation.
pub(crate) struct Intro {
    title: String,
    /// The own DM's " (you)" (or " (archived)", " (deactivated)") after the title, drawn dimmer.
    you: &'static str,
    /// The status emoji beside a DM's name, and a custom one's picture.
    status: Option<(String, Option<ImgRef>)>,
    text: String,
    /// The `@name` in the text, highlighted on its own.
    mention: Option<String>,
    avatar: Option<(String, ImgRef)>,
    /// A DM's "View profile" button opens this user.
    profile: Option<String>,
}

fn intro(app: &App, chan: &str) -> Option<Intro> {
    let c = app.convs.iter().find(|c| c.id == chan)?;
    let label = app.label(c);
    // No-break spaces keep the mention on one line and in one highlight.
    let tag = format!("@{}", label.replace(' ', "\u{a0}"));
    let (title, text) = match c.kind {
        Kind::Channel | Kind::Private => {
            let icon = if c.archived {
                ARCHIVE
            } else if c.kind == Kind::Channel {
                "#"
            } else {
                "🔒"
            };
            let name = &c.name;
            (
                format!("{icon} {name}"),
                trf!("This is the very beginning of the #{name} channel.", "Este é o início do canal #{name}."),
            )
        }
        Kind::Direct if c.user.as_deref().is_some_and(|u| app.is_me(u)) => (
            label.strip_suffix(app.you_suffix(c).unwrap_or_default()).unwrap_or(&label).to_string(),
            tr(
                "This is your space. Draft messages, list your to-dos or keep links and files handy.",
                "Este é o seu espaço. Rascunhe mensagens, anote tarefas ou guarde links e arquivos à mão.",
            )
            .into(),
        ),
        Kind::Direct => {
            let text = trf!(
                "This conversation is just between you and {tag}. Check out their profile to learn more about them.",
                "Esta conversa é apenas entre você e {tag}. Clique no perfil para descobrir mais sobre essa pessoa."
            );
            (label, text)
        }
        Kind::Group => {
            let people = match label.rsplit_once(", ") {
                Some((rest, last)) => trf!("{rest} and {last}", "{rest} e {last}"),
                None => label.clone(),
            };
            (label, trf!("This conversation is just between you, {people}.", "Esta conversa é apenas entre você, {people}."))
        }
    };
    let avatar = match c.kind {
        Kind::Direct => c
            .user
            .as_deref()
            .and_then(|u| app.users.avatars.get(u).map(|url| (u.to_string(), ImgRef::avatar_big(u, url)))),
        _ => None,
    };
    let peer = c.user.clone().filter(|u| c.kind == Kind::Direct && !app.is_me(u));
    let status = peer.as_deref().and_then(|u| status_emoji(app, u));
    let mention = peer.as_ref().map(|_| tag);
    let you = app.gone_suffix(c).or(app.you_suffix(c)).unwrap_or_default();
    Some(Intro { title, you, status, text, mention, avatar, profile: peer })
}

/// A line of the intro, with the mention highlighted but not what follows it.
fn intro_line(l: String, mention: Option<&str>) -> Line<'static> {
    let Some((before, after)) = mention.and_then(|m| l.split_once(m).map(|(b, a)| (b.to_string(), a.to_string()))) else {
        return styled_line(l, "", false);
    };
    let m = mention.unwrap_or_default().to_string();
    Line::from(vec![
        Span::raw(before),
        Span::styled(m, Style::new().fg(Color::Yellow).bold().bg(Color::Indexed(237))),
        Span::raw(after),
    ])
}

/// Shrinking with a smooth filter (the default, nearest, is jagged); a
/// cheaper one for big pictures, so encoding them doesn't stall a frame.
fn fit(img: &image::DynamicImage) -> Resize {
    let big = img.width() as u64 * img.height() as u64 > 1_000_000;
    Resize::Fit(Some(if big { ratatui_image::FilterType::Triangle } else { ratatui_image::FilterType::CatmullRom }))
}

/// Draws a fetched image into `rect`, or a placeholder while it loads.
pub(crate) fn draw_image(f: &mut Frame, app: &mut App, r: &ImgRef, rect: Rect) {
    let label = |state: &str| Paragraph::new(format!("🖼  {}{state}", r.name)).style(dim());
    match app.images.get(&r.key) {
        Some(Img::Ready(img)) => {
            // Animated emoji: this moment's frame, each encoded once under its own key.
            let (key, img) = match app.frame(&r.key) {
                Some((i, next)) => {
                    app.next_frame = Some(app.next_frame.map_or(next, |t| t.min(next)));
                    ((format!("{}#{i}", r.key), rect.width, rect.height), &app.anims[&r.key].frames[i])
                }
                None => ((r.key.clone(), rect.width, rect.height), img),
            };
            if !app.protocols.contains_key(&key) {
                let Some(picker) = app.picker.as_ref() else { return };
                let size = Rect::new(0, 0, rect.width, rect.height).into();
                match picker.new_protocol(img.clone(), size, fit(img)) {
                    Ok(p) => {
                        app.protocols.insert(key.clone(), p);
                    }
                    Err(_) => return,
                }
            }
            f.render_widget(Image::new(&app.protocols[&key]), rect);
        }
        Some(Img::Failed) if !r.name.is_empty() => f.render_widget(label(tr(" (didn't load)", " (não carregou)")), rect),
        Some(Img::Loading) if !r.name.is_empty() => f.render_widget(label("…"), rect),
        Some(_) => {}
        None => {
            if !r.name.is_empty() {
                f.render_widget(label("…"), rect);
            }
            app.wanted.push(r.clone());
        }
    }
}

/// One cell's size in pixels when the terminal shows real pictures; a dot in
/// half blocks is a smudge, so those terminals get the glyphs instead.
pub(crate) fn dot_cell(app: &App) -> Option<(u32, u32)> {
    let p = app.picker.as_ref().filter(|p| p.protocol_type() != ProtocolType::Halfblocks)?;
    let s = p.font_size();
    Some((s.width as u32, s.height as u32))
}

/// Someone's presence; paused notifications are only known for yourself.
pub(crate) fn presence_of(app: &App, uid: &str) -> Presence {
    let paused = app.is_me(uid) && app.account.paused.is_some();
    Presence { active: app.presence.get(uid) == Some(&true), paused }
}

/// The presence dot as text: a blank cell for the picture `draw_presence`
/// puts over it, or Slack's look in glyphs without graphics.
pub(crate) fn presence_span(app: &App, p: Presence) -> Span<'static> {
    if dot_cell(app).is_some() {
        return Span::raw(" ");
    }
    let z = if p.paused { "ᶻ" } else { "" };
    if p.active {
        Span::styled(format!("{ONLINE}{z}"), Style::new().fg(Color::Green))
    } else {
        Span::styled(format!("{OFFLINE}{z}"), dim())
    }
}

/// Slack's presence dot drawn as a picture in the cell at (x, y).
pub(crate) fn draw_presence(f: &mut Frame, app: &mut App, p: Presence, x: u16, y: u16) {
    let Some((fw, fh)) = dot_cell(app) else { return };
    let key = format!("presence-{}-{fw}x{fh}", p.key());
    app.images.entry(key.clone()).or_insert_with(|| Img::Ready(crate::icons::presence(p, fw, fh)));
    draw_image(f, app, &ImgRef { key, url: String::new(), name: String::new(), link: None }, Rect::new(x, y, 1, 1));
}

/// Someone's photo with the presence dot over its corner, as Slack shows it.
pub(crate) fn draw_avatar(f: &mut Frame, app: &mut App, uid: &str, url: &str, p: Presence, rect: Rect) {
    let plain = ImgRef::avatar(uid, url);
    let key = format!("{}+{}", plain.key, p.key());
    if !app.images.contains_key(&key) {
        if let Some(Img::Ready(img)) = app.images.get(&plain.key) {
            let img = crate::icons::with_presence(img, p);
            app.images.insert(key.clone(), Img::Ready(img));
        }
    }
    if app.images.contains_key(&key) {
        draw_image(f, app, &ImgRef { key, url: String::new(), name: String::new(), link: None }, rect);
    } else {
        draw_image(f, app, &plain, rect);
    }
}

/// The enlarged image in a popup the size of the image, when it fits.
fn viewer(f: &mut Frame, app: &mut App) {
    app.areas.viewer = Rect::default();
    app.areas.viewer_link = Rect::default();
    app.areas.viewer_save = Rect::default();
    let Some(r) = app.viewer.clone() else { return };
    let area = f.area();
    let (fw, fh) = app.picker.as_ref().map(|p| p.font_size()).map(|s| (s.width.max(1), s.height.max(1))).unwrap_or((8, 16));
    // Room for the border and the line of links under the image.
    let (max_w, max_h) = (area.width.saturating_sub(4).max(10), area.height.saturating_sub(5).max(4));
    let (cols, rows) = match app.images.get(&r.key) {
        Some(Img::Ready(img)) => {
            let (w, h) = (img.width() as f64 / fw as f64, img.height() as f64 / fh as f64);
            let scale = (max_w as f64 / w).min(max_h as f64 / h).min(1.0);
            ((w * scale).round().max(1.0) as u16, (h * scale).round().max(1.0) as u16)
        }
        _ => (max_w.min(60), max_h.min(20)),
    };
    let link = if r.link.is_some() { tr(" ↗ Open in browser ", " ↗ Ver no navegador ") } else { "" };
    let save = match app.downloads.get(&r.key) {
        Some(files::State::Saving) => tr(" downloading… ", " baixando… "),
        Some(files::State::Saved(_)) => tr(" ✓ Show in folder ", " ✓ Mostrar na pasta "),
        None => tr(" ⤓ Download (d) ", " ⤓ Baixar (d) "),
    };
    let hint = tr(" o open in viewer · Esc close ", " o abrir no visualizador · Esc fechar ");
    let inner_w = cols.max((link.width() + save.width() + hint.width()) as u16).min(max_w);
    let rect = Rect::new(area.x + (area.width - inner_w - 2) / 2, area.y + (area.height - rows - 3) / 2, inner_w + 2, rows + 3);
    f.render_widget(Clear, rect);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(dim())
        .title(Line::from(format!(" {} ", truncate(&r.name, inner_w.saturating_sub(4) as usize))).bold());
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    app.areas.viewer = rect;
    let [img_area, links] = Layout::vertical([Constraint::Length(rows), Constraint::Length(1)]).areas(inner);
    let img_rect = Rect::new(img_area.x + (img_area.width.saturating_sub(cols)) / 2, img_area.y, cols.min(img_area.width), rows);
    draw_image(f, app, &r, img_rect);
    if !link.is_empty() {
        app.areas.viewer_link = Rect::new(links.x, links.y, link.width() as u16, 1);
    }
    app.areas.viewer_save = Rect::new(links.x + link.width() as u16, links.y, save.width() as u16, 1).intersection(links);
    let gap = (links.width as usize).saturating_sub(link.width() + save.width() + hint.width());
    let line = Line::from(vec![
        Span::styled(link, Style::new().fg(Color::Cyan).underlined()),
        Span::styled(save, Style::new().fg(Color::Cyan)),
        Span::raw(" ".repeat(gap)),
        Span::styled(hint, dim()),
    ]);
    f.render_widget(Paragraph::new(line), links);
}

/// The workspace icon over a grey card, made once from the plain icon when
/// it's loaded; the key it's kept under. Sized for 4×3 cells.
fn stacked_icon(app: &mut App, plain: &str, domain: &str, dot: bool) -> Option<String> {
    let s = app.picker.as_ref()?.font_size();
    let (w, h) = (4 * s.width as u32, 3 * s.height as u32);
    let key = format!("team-stack-{domain}-{w}x{h}{}", if dot { "-dot" } else { "" });
    if !app.images.contains_key(&key) {
        let Some(Img::Ready(img)) = app.images.get(plain) else { return None };
        let img = crate::icons::stacked(img, crate::icons::grey(), dot, w, h);
        app.images.insert(key.clone(), Img::Ready(img));
    }
    Some(key)
}

/// The workspace's address and ID, under its name, then every signed-in
/// workspace to switch to and "Add a workspace", as in Slack's dropdown.
fn team_popup(f: &mut Frame, app: &mut App) {
    app.areas.team_popup = Rect::default();
    app.areas.team_url = Rect::default();
    app.areas.team_id = Rect::default();
    app.areas.switcher.rows.clear();
    app.areas.switcher.add = Rect::default();
    if !app.team_popup {
        app.areas.switcher.open = false;
        return;
    }
    let Some(me) = app.me.clone() else { return };
    let list = crate::switcher::list(app);
    let n = list.len();
    // Just opened: start on the one on screen.
    if !app.areas.switcher.open {
        app.areas.switcher.open = true;
        app.areas.switcher.sel = list.iter().position(|w| w.active).unwrap_or(0);
    }
    let sel = app.areas.switcher.sel.min(n);
    let url = app.team_url().unwrap_or_default();
    let shown = url.trim_start_matches("https://").to_string();
    let id = format!("ID {}", me.team_id);
    let (copy, copy_style) = copy_hint(app, "team");
    let info = vec![
        Line::from(me.team.clone()).bold(),
        Line::from(vec![Span::styled(shown.clone(), Style::new().fg(Color::Cyan).italic().underlined()), Span::styled("  ↗", dim())]),
        Line::from(vec![Span::styled(id.clone(), dim()), Span::styled(copy, copy_style)]),
    ];
    let version = crate::account::version();
    let add = tr("Add a workspace", "Adicionar um workspace");
    let expired = tr("Session expired · sign in again", "Sessão expirada · entre de novo");
    let check = " ✓";
    let ctrl = |i: usize| if i < 9 { format!("Ctrl {}", i + 1) } else { String::new() };
    let address = |w: &crate::switcher::WsInfo| format!("{}.slack.com", w.domain);
    // The right side of a workspace's first line: its badge and shortcut.
    let right = |i: usize, w: &crate::switcher::WsInfo| {
        let mut spans = crate::switcher::badge_spans(w.badge);
        let key = ctrl(i);
        if !key.is_empty() {
            spans.push(Span::raw(if spans.is_empty() { "" } else { "  " }));
            spans.push(Span::styled(key, dim()));
        }
        spans
    };
    let width = |spans: &[Span]| spans.iter().map(|s| s.content.width()).sum::<usize>();
    // The icon takes four columns and a gap.
    let rows_w = list.iter().enumerate().map(|(i, w)| {
        let top = w.name.width() + if w.active { check.width() } else { 0 } + 2 + width(&right(i, w));
        let bottom = if w.expired { expired.width() } else { address(w).width() };
        5 + top.max(bottom)
    });
    let content = info.iter().map(|l| l.width()).chain(rows_w).chain([5 + add.width(), version.width()]).max().unwrap_or(10).max(30);
    let area = f.area();
    let w = (content as u16 + 4).min(area.width);
    // The address and ID, a divider, three rows per workspace (the last a gap), "Add", a divider and the version.
    let h = 3 + 1 + 3 * n as u16 + 1 + 1 + 1 + 2;
    let below = app.areas.team_name;
    let rect = Rect::new(below.x + 1, below.bottom(), w, h).intersection(area);
    f.render_widget(Clear, rect);
    let block = Block::bordered().border_type(BorderType::Rounded).border_style(dim());
    let full = block.inner(rect);
    let inner = full.inner(ratatui::layout::Margin { horizontal: 1, vertical: 0 });
    f.render_widget(block, rect);
    app.areas.team_popup = rect;
    if inner.width < 10 || inner.height < 3 {
        return;
    }
    f.render_widget(Paragraph::new(info), inner);
    app.areas.team_url = Rect::new(inner.x, inner.y + 1, shown.width() as u16 + 3, 1).intersection(inner);
    app.areas.team_id = Rect::new(inner.x, inner.y + 2, (id.width() + copy.width()) as u16, 1).intersection(inner);

    // A divider joined to the frame.
    let divider = |f: &mut Frame, y: u16| {
        if y < full.bottom() {
            let line = format!("├{}┤", "─".repeat(full.width as usize));
            f.render_widget(Paragraph::new(line).style(dim()), Rect::new(rect.x, y, rect.width, 1));
        }
    };
    // The highlighted row: the selection bar with a thin accent where it starts.
    let highlight = |f: &mut Frame, r: Rect| {
        f.buffer_mut().set_style(r, Style::new().bg(SELECTED));
        for y in r.y..r.bottom() {
            f.render_widget(Paragraph::new("▎").style(Style::new().fg(Color::Cyan)), Rect::new(r.x, y, 1, 1));
        }
    };
    divider(f, inner.y + 3);
    let text_x = inner.x + 5;
    let text_w = inner.width.saturating_sub(5) as usize;
    let mut y = inner.y + 4;
    for (i, ws) in list.iter().enumerate() {
        if y + 1 >= full.bottom() {
            break;
        }
        let row = Rect::new(full.x, y, full.width, 2);
        if i == sel {
            highlight(f, row);
        }
        let icon = Rect::new(inner.x, y, 4, 2);
        match ws.icon_url.as_deref().filter(|_| app.picker.is_some()) {
            Some(u) => draw_image(f, app, &ImgRef::avatar(&format!("team-{}", ws.domain), u), icon),
            None => {
                // The name's initial on a tile, without pictures.
                let initial: String = ws.name.chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_default();
                let tile = Paragraph::new(vec![Line::from(format!(" {initial}")).bold(), Line::from("")]).style(Style::new().bg(SURFACE));
                f.render_widget(tile, icon);
            }
        }
        let right = right(i, ws);
        let right_w = width(&right);
        let mark = if ws.active { check } else { "" };
        let name = truncate(&ws.name, text_w.saturating_sub(right_w + mark.width() + 1));
        let gap = text_w.saturating_sub(name.width() + mark.width() + right_w);
        let mut top = vec![Span::raw(name).bold(), Span::styled(mark, Style::new().fg(Color::Green).bold()), Span::raw(" ".repeat(gap))];
        top.extend(right);
        let bottom = if ws.expired {
            Span::styled(truncate(expired, text_w), Style::new().fg(Color::Yellow))
        } else {
            Span::styled(truncate(&address(ws), text_w), dim())
        };
        f.render_widget(Paragraph::new(vec![Line::from(top), Line::from(bottom)]), Rect::new(text_x, y, text_w as u16, 2));
        app.areas.switcher.rows.push((row, i));
        y += 3;
    }
    if y < full.bottom() {
        let row = Rect::new(full.x, y, full.width, 1);
        if sel == n {
            highlight(f, row);
        }
        f.render_widget(Paragraph::new(" +").bold(), Rect::new(inner.x, y, 4, 1));
        f.render_widget(Paragraph::new(truncate(add, text_w)), Rect::new(text_x, y, text_w as u16, 1));
        app.areas.switcher.add = row;
    }
    divider(f, y + 1);
    if y + 2 < full.bottom() {
        f.render_widget(Paragraph::new(version).style(dim()), Rect::new(inner.x, y + 2, inner.width, 1));
    }
}

/// The right-click menu, kept on screen.
fn menu(f: &mut Frame, app: &mut App) {
    app.areas.menu_items.clear();
    app.areas.menu = Rect::default();
    let Some(m) = &app.menu else { return };
    let area = f.area();
    // "label\tshortcut": the shortcut goes on the right, dim.
    let split = |l: &str| l.split_once('\t').map_or((l.to_string(), String::new()), |(a, b)| (a.to_string(), b.to_string()));
    let w = m.items.iter().map(|(l, _)| split(l)).map(|(a, b)| a.width() + if b.is_empty() { 0 } else { b.width() + 3 }).max().unwrap_or(10) as u16 + 4;
    let h = m.items.len() as u16 + 2;
    let x = m.x.min(area.right().saturating_sub(w));
    let y = m.y.min(area.bottom().saturating_sub(h));
    let rect = Rect::new(x, y, w.min(area.width), h.min(area.height));
    f.render_widget(Clear, rect);
    let block = Block::bordered().border_type(BorderType::Rounded).border_style(dim());
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    let mut items = Vec::new();
    for (n, (label, action)) in m.items.iter().enumerate() {
        let y = inner.y + n as u16;
        if y >= inner.bottom() {
            break;
        }
        let row = Rect::new(inner.x, y, inner.width, 1);
        let line = if matches!(action, Action::Separator) {
            Line::from("─".repeat(inner.width as usize)).style(dim())
        } else {
            items.push((row, n));
            let style = match action {
                _ if n == m.sel => Style::new().add_modifier(Modifier::REVERSED),
                Action::Close(_) => Style::new().fg(Color::Red),
                Action::Message(it) if it.danger() => Style::new().fg(Color::Red),
                _ => Style::new(),
            };
            let (left, right) = split(label);
            let room = (inner.width as usize).saturating_sub(2 + right.width());
            let key = if n == m.sel { Style::new() } else { dim() };
            Line::from(vec![Span::raw(format!(" {left:<room$}")), Span::styled(format!("{right} "), key)]).style(style)
        };
        f.render_widget(Paragraph::new(line), row);
    }
    app.areas.menu_items = items;
    app.areas.menu = rect;
}

/// The profile panel on the right, like Slack's.
fn profile(f: &mut Frame, app: &mut App, area: Rect) {
    app.areas.profile_close = Rect::default();
    app.areas.profile_browser = Rect::default();
    app.areas.profile_photo = Rect::default();
    app.areas.profile_star = Rect::default();
    app.areas.profile_vip = Rect::default();
    app.areas.profile_id = Rect::default();
    app.areas.profile_email = Rect::default();
    app.areas.profile_edit = Rect::default();
    let Some((uid, data)) = app.profile.clone() else { return };
    let block = Block::new().borders(Borders::LEFT).border_style(dim());
    let inner = block.inner(area).inner(ratatui::layout::Margin { horizontal: 2, vertical: 0 });
    f.render_widget(block, area);
    if inner.width < 10 || inner.height < 4 {
        return;
    }

    let close = Rect::new(inner.x + inner.width - 1, inner.y, 1, 1);
    app.areas.profile_close = close;
    f.render_widget(Paragraph::new(Line::from(tr(" Profile", " Perfil")).bold()), Rect::new(inner.x, inner.y, inner.width - 2, 1));
    f.render_widget(Paragraph::new("✕").style(dim()), close);
    f.render_widget(
        Block::new().borders(Borders::BOTTOM).border_style(dim()),
        Rect::new(area.x + 1, inner.y, area.width - 1, 2),
    );
    let mut y = inner.y + 3;
    let bottom = inner.y + inner.height;
    let Some(p) = data else {
        f.render_widget(Paragraph::new(tr("loading…", "carregando…")).style(dim()), Rect::new(inner.x, y, inner.width, 1));
        return;
    };

    // A deactivated account: Slack's dim "Deactivated account" band at the
    // photo's foot, and nothing about someone who is no longer there.
    let gone = app.users.deleted.contains(&uid);
    let band = |width: u16| {
        let text = format!("{ARCHIVE} {}", tr("Deactivated account", "Conta desativada"));
        let text = truncate(&text, width as usize);
        Paragraph::new(text).centered().style(Style::new().fg(Color::Gray).bg(Color::Indexed(237)))
    };
    // The photo: a centred square as wide as the panel allows, up to half its height.
    let font = app.picker.as_ref().map(|p| p.font_size()).map(|s| (s.width, s.height));
    let mut banded = false;
    if let (Some((fw, fh)), false) = (font, p.avatar.is_empty()) {
        let max_rows = (inner.height / 2).max(4);
        let mut cols = inner.width.saturating_sub(4);
        let mut rows = (cols as f64 * fw as f64 / fh as f64).round() as u16;
        if rows > max_rows {
            rows = max_rows;
            cols = (rows as f64 * fh as f64 / fw as f64).round() as u16;
        }
        let photo = Rect::new(inner.x + (inner.width - cols) / 2, y, cols, rows);
        draw_image(f, app, &ImgRef::profile(&uid, &p.avatar), photo);
        app.areas.profile_photo = photo;
        y += rows;
        // Its own row under the photo: text over a terminal picture isn't
        // reliably drawn (kitty puts the picture on top, sixel paints over it).
        if gone && y < bottom {
            f.render_widget(band(cols), Rect::new(photo.x, y, cols, 1));
            banded = true;
            y += 1;
        }
        y += 1;
    }

    let w = inner.width as usize;
    // Your own profile gets "Edit" on the right of the name, as in Slack.
    let mine = app.is_me(&uid) && w >= 16;
    let edit = tr("Edit", "Editar");
    let mut lines = if mine {
        let name = truncate(&p.name, w - edit.width() - 2);
        let gap = " ".repeat(w - name.width() - edit.width());
        app.areas.profile_edit = Rect::new(inner.right() - edit.width() as u16, y, edit.width() as u16, 1).intersection(inner);
        vec![Line::from(vec![Span::styled(name, Style::new().bold()), Span::raw(gap), Span::styled(edit, Style::new().fg(Color::Blue))])]
    } else {
        vec![Line::from(Span::styled(truncate(&p.name, w), Style::new().bold()))]
    };
    if !p.real_name.is_empty() && p.real_name != p.name {
        lines.push(Line::from(truncate(&p.real_name, w)).style(dim()));
    }
    if !p.title.is_empty() {
        lines.push(Line::from(truncate(&p.title, w)));
    }
    // Without a photo the band goes under the name.
    let band_row = (gone && !banded).then(|| {
        lines.push(Line::from(""));
        lines.len() as u16 - 1
    });
    lines.push(Line::from(""));
    // The status emoji as beside their name: theirs as `users` has it, or a
    // workspace emoji's picture; a picture goes over two blank cells.
    let lead = app.users.status(&uid).map(|(e, _)| e.clone()).filter(|e| !e.is_empty() && p.status.starts_with(e.as_str())).or_else(|| {
        let name = p.status.strip_prefix(':')?.split(':').next()?;
        matches!(app.emoji.glyph(name), Some(emoji::Glyph::Image(_))).then(|| format!(":{name}:"))
    });
    let (status, pic) = match lead {
        Some(e) => {
            let (shown, pic) = named_status_emoji(app, &uid, e.trim_matches(':'));
            (format!("{shown}{}", crate::text::shortcodes(&p.status[e.len()..])), pic)
        }
        None => (crate::text::shortcodes(&p.status), None),
    };
    let mut dot_row = None;
    // The status line and where its picture goes.
    let mut pic_at = None;
    match app.presence.get(&uid) {
        _ if gone => {}
        Some(_) => {
            let pres = presence_of(app, &uid);
            let word = if pres.active { tr("Active", "Ativo") } else { tr("Away", "Ausente") };
            let rest = if status.is_empty() { word.to_string() } else { format!("{word} · {status}") };
            let dot = presence_span(app, pres);
            let dot = Span::styled(format!("{} ", dot.content), dot.style);
            let room = w.saturating_sub(dot.width());
            dot_row = Some((lines.len() as u16, pres));
            pic_at = Some((lines.len() as u16, (dot.width() + word.width() + 3) as u16));
            lines.push(Line::from(vec![dot, Span::raw(truncate(&rest, room))]));
        }
        None if !status.is_empty() => {
            pic_at = Some((lines.len() as u16, 0));
            lines.push(Line::from(truncate(&status, w)));
        }
        None => {}
    }
    if let Some(tz) = chrono::FixedOffset::east_opt(p.tz_offset).filter(|_| !gone) {
        let local = i18n::clock(&chrono::Utc::now().with_timezone(&tz));
        lines.push(Line::from(trf!("🕐 {local} local time", "🕐 {local} hora local")));
    }
    // The member ID, click to copy (as in Slack's "Copy member ID").
    lines.push(Line::from(""));
    let id_row = lines.len() as u16;
    let id_text = format!("ID {uid}");
    let (copy, copy_style) = copy_hint(app, "id");
    lines.push(Line::from(vec![Span::styled(id_text.clone(), dim()), Span::styled(copy, copy_style)]));
    let n = lines.len() as u16;
    if y + id_row < bottom {
        app.areas.profile_id = Rect::new(inner.x, y + id_row, (id_text.width() + copy.width()) as u16, 1).intersection(inner);
    }
    f.render_widget(Paragraph::new(lines), Rect::new(inner.x, y, inner.width, n.min(bottom.saturating_sub(y))));
    if let Some((row, pres)) = dot_row.filter(|(row, _)| y + row < bottom) {
        draw_presence(f, app, pres, inner.x, y + row);
    }
    if let (Some(img), Some((row, x))) = (pic, pic_at.filter(|(row, x)| y + row < bottom && *x as usize + 2 <= w)) {
        draw_image(f, app, &img, Rect::new(inner.x + x, y + row, 2, 1));
    }
    if let Some(row) = band_row.filter(|row| y + row < bottom) {
        let bw = (tr("Deactivated account", "Conta desativada").width() as u16 + 4).min(inner.width);
        f.render_widget(band(bw), Rect::new(inner.x, y + row, bw, 1));
    }
    y += n + 1;

    // Bordered buttons, as in Slack, wrapping to a new row when full.
    let dm = app.dm_with(&uid);
    let starred = dm.as_ref().is_some_and(|id| app.stars.contains(id));
    let vip = app.vips.contains(&uid);
    let mut buttons: Vec<(Line, u8)> = Vec::new();
    if dm.is_some() {
        let (icon, style, text) =
            if starred { (STAR_ON, Style::new().fg(Color::Yellow), tr(" Starred", " Favorito")) } else { (STAR_OFF, Style::new(), tr(" Star", " Favoritar")) };
        buttons.push((Line::from(vec![Span::raw(" "), Span::styled(icon, style), Span::raw(format!("{text} "))]), 0));
    }
    if !app.is_me(&uid) && !app.users.bots.contains(&uid) && !gone {
        let line = if vip {
            Line::from(vec![Span::styled(" ✓", Style::new().fg(Color::Green)), Span::raw(" VIP ")])
        } else {
            Line::from(" + VIP ")
        };
        buttons.push((line, 1));
    }
    buttons.push((Line::from(tr(" Open in browser ", " Abrir no navegador ")), 2));
    let mut x = inner.x;
    let mut row_used = false;
    for (line, kind) in buttons {
        let bw = (line.width() as u16 + 2).min(inner.width);
        if row_used && x + bw > inner.x + inner.width {
            x = inner.x;
            y += 3;
        }
        if y + 3 > bottom {
            break;
        }
        let button = Rect::new(x, y, bw, 3);
        f.render_widget(Paragraph::new(line).block(Block::bordered().border_type(BorderType::Rounded).border_style(dim())), button);
        match kind {
            0 => app.areas.profile_star = button,
            1 => app.areas.profile_vip = button,
            _ => app.areas.profile_browser = button,
        }
        x += bw + 1;
        row_used = true;
    }
    y += 4;

    let mut contact = Vec::new();
    let mut email_at = None;
    if !p.email.is_empty() {
        contact.push(Line::from(tr("Email address", "Endereço de e-mail")).style(dim()));
        // Click to copy; its row is known once the header lines are in.
        let done = app.copied.is_some_and(|(what, _)| what == "email");
        let (note, note_style) = copy_hint(app, "email");
        let email = truncate(&p.email, if done { w.saturating_sub(note.width()) } else { w });
        email_at = Some((contact.len() as u16, email.width() as u16));
        let mut spans = vec![Span::styled(email, Style::new().fg(Color::Blue).underlined())];
        if done {
            spans.push(Span::styled(note, note_style));
        }
        contact.push(Line::from(spans));
    }
    if !p.phone.is_empty() {
        contact.push(Line::from(tr("Phone", "Telefone")).style(dim()));
        contact.push(Line::from(truncate(&p.phone, w)));
    }
    if !contact.is_empty() && y + 3 <= bottom {
        f.render_widget(
            Block::new().borders(Borders::TOP).border_style(dim()),
            Rect::new(area.x + 1, y, area.width - 1, 1),
        );
        contact.insert(0, Line::from(""));
        contact.insert(1, Line::from(tr("Contact information", "Informações de contato")).bold());
        contact.insert(2, Line::from(""));
        if let Some((row, ew)) = email_at.filter(|(row, _)| y + 3 + row < bottom) {
            app.areas.profile_email = Rect::new(inner.x, y + 3 + row, ew, 1).intersection(inner);
        }
        f.render_widget(Paragraph::new(contact), Rect::new(inner.x, y, inner.width, bottom - y));
    }
}

fn day_label(t: chrono::DateTime<Local>) -> String {
    let today = Local::now().date_naive();
    let d = t.date_naive();
    if d == today {
        return tr("Today", "Hoje").into();
    }
    if today.pred_opt() == Some(d) {
        return tr("Yesterday", "Ontem").into();
    }
    // "Monday, September 28th", "segunda-feira, 28 de setembro".
    let (wd, mo) = (i18n::weekday(&t), i18n::month(&t));
    let mut s = if i18n::is_pt() { format!("{wd}, {} de {mo}", t.day()) } else { format!("{wd}, {mo} {}", i18n::ordinal(t.day())) };
    if t.year() != today.year() {
        s.push_str(&trf!(", {}", " de {}", t.year()));
    }
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or(s)
}

pub(crate) fn message_lines(
    msgs: &[Message],
    intro: Option<Intro>,
    names: &Names,
    app: &App,
    width: usize,
    font: Option<(u16, u16)>,
) -> Rendered {
    lines_with(msgs, intro, names, app, width, font, None, !app.compact)
}

/// `message_lines` back to back, whatever the spacing setting: the Threads
/// view's cards keep their own layout.
pub(crate) fn packed_lines(msgs: &[Message], names: &Names, app: &App, width: usize, font: Option<(u16, u16)>) -> Rendered {
    lines_with(msgs, None, names, app, width, font, None, false)
}

/// `message_lines`, with Slack's red "New" line over the first message from
/// `new_at` on: only the open conversation has one. With `gap`, a blank row
/// between a message and the next from the same author too, as a new
/// author's header already has; it belongs to neither message.
#[allow(clippy::too_many_arguments)]
fn lines_with(
    msgs: &[Message],
    intro: Option<Intro>,
    names: &Names,
    app: &App,
    width: usize,
    font: Option<(u16, u16)>,
    mut new_at: Option<&str>,
    gap: bool,
) -> Rendered {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut slots = Vec::new();
    let mut links = Vec::new();
    // Clickable names: (line, column, width, user id).
    let mut people = Vec::new();
    // Block Kit buttons and links.
    let mut buttons = Vec::new();
    // File cards' actions.
    let mut file_hits = Vec::new();
    let mut hits = Vec::new();
    let mut quiet = Vec::new();
    let mut prev: Option<&Message> = None;
    let mut prev_day = None;
    // Messages already drawn in a line of joins and leaves.
    let mut folded = 0;
    // Avatar: a square of two rows; the text starts after it.
    let avatar_cols = font.map(|(fw, fh)| ((2.0 * fh as f64 / fw as f64).round() as u16).clamp(3, 6));
    let gutter = avatar_cols.map_or(2, |c| c as usize + 1);
    let indent = " ".repeat(gutter);
    let body_width = width.saturating_sub(gutter).max(10);
    if let Some(i) = intro {
        lines.push(Line::from(""));
        let title_w = i.title.width() as u16;
        let title = Span::styled(i.title, Style::new().bold());
        let you = Span::styled(i.you, dim());
        let (status, pic) = i.status.unzip();
        let mut pic = pic.flatten();
        let status = Span::raw(status.map(|e| format!(" {e}")).unwrap_or_default());
        let pic_x = title_w + you.width() as u16 + 1;
        match (avatar_cols, i.avatar) {
            (Some(cols), Some((uid, img))) => {
                // Twice the message avatar, with the name beside its middle.
                let (cols, rows) = (cols * 2, 4);
                people.push((lines.len() + (rows / 2 - 1) as usize, cols + 2, title_w, uid.clone()));
                slots.push(Slot { line: lines.len(), x: 0, cols, rows, img, full: None, user: Some(uid) });
                for r in 0..rows {
                    if r == rows / 2 - 1 {
                        let x = cols + 2 + pic_x;
                        if let Some(img) = pic.take().filter(|_| x as usize + 2 <= width) {
                            slots.push(Slot { line: lines.len(), x, cols: 2, rows: 1, img, full: None, user: None });
                        }
                        lines.push(Line::from(vec![Span::raw(" ".repeat(cols as usize + 2)), title.clone(), you.clone(), status.clone()]));
                    } else {
                        lines.push(Line::from(""));
                    }
                }
            }
            _ => {
                if let Some(img) = pic.filter(|_| pic_x as usize + 2 <= width) {
                    slots.push(Slot { line: lines.len(), x: pic_x, cols: 2, rows: 1, img, full: None, user: None });
                }
                lines.push(Line::from(vec![title, you, status]));
            }
        }
        lines.push(Line::from(""));
        for l in wrap(&i.text, width) {
            lines.push(intro_line(l, i.mention.as_deref()));
        }
        // A rounded button, like Block Kit's, that opens the profile.
        if let Some(uid) = i.profile {
            let (fg, bg) = (Color::White, Color::DarkGray);
            let label = tr(" View profile ", " Ver perfil ");
            lines.push(Line::from(""));
            people.push((lines.len(), 0, label.width() as u16 + 2, uid));
            lines.push(Line::from(vec![
                Span::styled(blocks::CAP_L, Style::new().fg(bg)),
                Span::styled(label, Style::new().fg(fg).bg(bg).bold()),
                Span::styled(blocks::CAP_R, Style::new().fg(bg)),
            ]));
        }
    }
    for (i, m) in msgs.iter().enumerate() {
        if folded > 0 {
            folded -= 1;
            continue;
        }
        let time = Local.timestamp_opt(secs(&m.ts), 0).single();
        let day = time.map(|t| t.date_naive());
        // Slack's "Igor and 6 others left #x. Also, Ana rejoined.": the joins
        // and leaves in a row, that day, as one quiet line with no avatar.
        let same_day = |n: &&Message| Local.timestamp_opt(secs(&n.ts), 0).single().map(|t| t.date_naive()) == day;
        let run: Vec<&Message> = msgs[i..].iter().take_while(|n| crate::text::membership(n).is_some() && same_day(n)).collect();
        let mut spaced = false;
        if day != prev_day {
            if let Some(t) = time {
                lines.push(Line::from(""));
                let label = format!(" {} ", day_label(t));
                let side = width.saturating_sub(label.width()) / 2;
                lines.push(Line::from(vec![
                    Span::styled("─".repeat(side), dim()),
                    Span::styled(label, Style::new().bold()),
                    Span::styled("─".repeat(side), dim()),
                ]));
                spaced = true;
            }
            prev_day = day;
            prev = None;
        }
        // Slack's "New": a red rule across, the word at its right end, right
        // under the day's line when one is there. Nothing to pick on it.
        if new_at.is_some_and(|t| m.ts.as_str() >= t || run.last().is_some_and(|r| r.ts.as_str() >= t)) {
            new_at = None;
            if !spaced {
                lines.push(Line::from(""));
            }
            let label = tr(" New", " Novas");
            let red = Style::new().fg(Color::Red);
            lines.push(Line::from(vec![
                Span::styled("─".repeat(width.saturating_sub(label.width())), red),
                Span::styled(label, red.bold()),
            ]));
            prev = None;
        }
        if !run.is_empty() {
            folded = run.len() - 1;
            let first = lines.len();
            lines.push(Line::from(""));
            let mut segs = membership_segs(&run, app, names);
            let clock = time.map(|t| i18n::clock(&t)).unwrap_or_default();
            segs.push(blocks::Seg { text: format!("  {}", clock.replace(' ', "\u{a0}")), ..Default::default() });
            let para = blocks::Para { kind: blocks::ParaKind::Plain, segs };
            let rich = blocks::render(&[blocks::Piece::Text(vec![para], None)], body_width, None, dim());
            let top = lines.len();
            buttons.extend(rich.hits.into_iter().map(|h| blocks::Hit { line: top + h.line, x: gutter as u16 + h.x, ..h }));
            // The person icon sits in the middle of where the avatars go.
            let at = (gutter - 1) / 2;
            for (k, mut l) in rich.lines.into_iter().enumerate() {
                let lead = if k == 0 { format!("{}{PERSON}{}", " ".repeat(at), " ".repeat(gutter - at - 1)) } else { indent.clone() };
                l.spans.insert(0, Span::styled(lead, dim()));
                lines.push(l);
            }
            quiet.extend(run.iter().map(|n| (first..lines.len(), n.ts.clone())));
            prev = None;
            continue;
        }
        let grouped = grouped(app, prev, m);
        // Spacing between messages: a new author's header brings its own
        // blank row, and so do the day's line, "New" and the joins' line.
        if gap && grouped {
            lines.push(Line::from(""));
        }
        let first = lines.len();
        let author = author(m, names);
        // A pinned message gets its own header, with "Fixada" over it.
        let pinned = app.current.as_ref().filter(|c| m.pinned_to.contains(c));
        let mut header_line = None;
        if !grouped {
            lines.push(Line::from(""));
            if let Some(chan) = pinned {
                lines.push(Line::from(vec![Span::raw(indent.clone()), Span::styled(crate::pins::label(app, chan, &m.ts), dim())]));
            }
            header_line = Some(lines.len());
            let hhmm = time.map(|t| i18n::clock(&t)).unwrap_or_default();
            let bot = if m.user.is_none() { " APP" } else { "" };
            let (status, pic) = m.user.as_deref().and_then(|u| status_emoji(app, u)).unzip();
            let x = (gutter + author.width() + 1) as u16;
            if let Some(img) = pic.flatten().filter(|_| x as usize + 2 <= width) {
                slots.push(Slot { line: lines.len(), x, cols: 2, rows: 1, img, full: None, user: None });
            }
            lines.push(Line::from(vec![
                Span::raw(indent.clone()),
                Span::styled(author.clone(), Style::new().bold()),
                Span::raw(status.map(|e| format!(" {e}")).unwrap_or_default()),
                Span::styled(bot, dim()),
                Span::styled(format!("  {hhmm}"), dim()),
            ]));
            if let Some(uid) = &m.user {
                people.push((lines.len() - 1, gutter as u16, author.width() as u16, uid.clone()));
            }
            if let (Some(cols), Some(url)) = (avatar_cols, m.user.as_ref().and_then(|u| app.users.avatars.get(u))) {
                let uid = m.user.as_deref().unwrap_or("");
                let user = Some(uid.to_string());
                slots.push(Slot { line: lines.len() - 1, x: 0, cols, rows: 2, img: ImgRef::avatar_px(uid, url, 2 * font.map_or(0, |f| f.1 as u32)), full: None, user });
            }
        }
        prev = Some(m);

        // A reply also sent to the channel: the way into its thread.
        if let (Some("thread_broadcast"), Some(ts)) = (m.subtype.as_deref(), &m.thread_ts) {
            let label = tr("replied to a thread", "respondeu a uma conversa");
            links.push(Link { line: lines.len(), x: gutter as u16, w: label.width() as u16, ts: ts.clone() });
            lines.push(Line::from(vec![Span::raw(indent.clone()), Span::styled(label, dim().italic())]));
        }
        // A slash command's answer, as Slack shows it: dim, for you alone.
        let ephemeral = crate::slash::is_ephemeral(m);
        if ephemeral {
            lines.push(Line::from(vec![Span::raw(indent.clone()), Span::styled(tr("Only visible to you", "Visível só para você"), dim().italic())]));
        }
        let muted = ephemeral
            || matches!(
                m.subtype.as_deref(),
                Some("channel_join" | "channel_leave" | "channel_topic" | "channel_purpose" | "huddle_thread")
            )
            || crate::text::system(m).is_some_and(|_| m.subtype.as_deref() != Some("pinned_item"));
        // Block Kit and mrkdwn laid out at this width, then moved past the avatar.
        let mut pieces = blocks::parse(m, names);
        blocks::drop_author(&mut pieces, &author);
        let mut rich = blocks::render(&pieces, body_width, font, if muted { dim().italic() } else { Style::new() });
        if app.copied.is_some_and(|(what, _)| what == "code") {
            blocks::mark_copied(&mut rich, &app.copied_code);
        }
        let (top, dx) = (lines.len(), gutter as u16);
        for p in rich.pics {
            slots.push(Slot { line: top + p.line, x: dx + p.x, cols: p.cols, rows: p.rows, full: Some(p.img.clone()), img: p.img, user: None });
        }
        buttons.extend(rich.hits.into_iter().map(|h| blocks::Hit { line: top + h.line, x: dx + h.x, ..h }));
        lines.extend(rich.lines.into_iter().map(|mut l| {
            l.spans.insert(0, Span::raw(indent.clone()));
            l
        }));
        if m.edited.is_some() {
            edited_mark(&mut lines, top, width, &indent);
        }
        for file in m.files.iter().filter(|f| f.thumb.is_some()) {
            match (font, ImgRef::thumb(file)) {
                (Some((fw, fh)), Some(img)) if file.w > 0 && file.h > 0 => {
                    let (cols, rows) = image_cells(file, fw, fh, body_width);
                    slots.push(Slot { line: lines.len(), x: gutter as u16, cols, rows, img, full: ImgRef::full(file), user: None });
                    lines.extend(std::iter::repeat_n(Line::from(""), rows as usize));
                }
                // No graphics here: a card, to save it at least.
                _ => {
                    let (card, h) = files::card(file, app.downloads.get(&file.id), &indent, body_width, lines.len());
                    lines.extend(card);
                    file_hits.extend(h);
                }
            }
        }
        for file in m.files.iter().filter(|f| f.thumb.is_none()) {
            let (card, h) = files::card(file, app.downloads.get(&file.id), &indent, body_width, lines.len());
            lines.extend(card);
            file_hits.extend(h);
        }
        if !m.reactions.is_empty() {
            reaction_lines(app, m, gutter, body_width, &mut lines, &mut slots, &mut hits);
        }
        if m.reply_count > 0 {
            // Like Slack: the faces of who replied, the count and when, all one link.
            let mut spans = vec![Span::raw(indent.clone())];
            let mut x = gutter as u16;
            if let Some(cols) = avatar_cols {
                let cols = (cols / 2).max(1);
                for uid in m.reply_users.iter().take(3) {
                    let Some(url) = app.users.avatars.get(uid) else { continue };
                    let img = ImgRef::avatar_px(uid, url, font.map_or(0, |f| f.1 as u32));
                    slots.push(Slot { line: lines.len(), x, cols, rows: 1, img, full: None, user: Some(uid.clone()) });
                    spans.push(Span::raw(" ".repeat(cols as usize + 1)));
                    x += cols + 1;
                }
            }
            let n = m.reply_count;
            let count = if n == 1 { trf!("{n} reply", "{n} resposta") } else { trf!("{n} replies", "{n} respostas") };
            let last = m.latest_reply.as_deref().and_then(when).map(|w| trf!(" · last reply {w}", " · última resposta {w}")).unwrap_or_default();
            let w = x - gutter as u16 + (count.width() + last.width()) as u16;
            links.push(Link { line: lines.len(), x: gutter as u16, w, ts: m.ts.clone() });
            spans.push(Span::styled(count, Style::new().fg(Color::Blue).bold()));
            spans.push(Span::styled(last, Style::new().fg(Color::Blue)));
            lines.push(Line::from(spans));
        }
        // The avatar is two rows tall; a one-line message still needs room for it.
        if let Some(h) = header_line {
            while lines.len() < h + 2 {
                lines.push(Line::from(""));
            }
        }
        hits.push(Hit::Message { lines: first..lines.len(), ts: m.ts.clone() });
    }
    Rendered { lines, slots, links, people, hits, buttons, files: file_hits, quiet }
}

/// nf-fa-user: the mark of a line of joins and leaves, where avatars go.
const PERSON: &str = "\u{f007}";

/// What a run of joins and leaves amounts to, in Slack's words: each person
/// once, by how they ended up ("left then joined" is "rejoined"), the
/// largest group first and with the channel, the others after "Also,".
fn membership_segs(run: &[&Message], app: &App, names: &Names) -> Vec<blocks::Seg> {
    #[derive(Clone, Copy, PartialEq)]
    enum Cat {
        Joined,
        Left,
        Rejoined,
    }
    // Each person: the first and last thing they did and who added them.
    let mut people: Vec<(String, bool, bool, Option<String>)> = Vec::new();
    for m in run {
        let (Some(uid), Some(join)) = (m.user.clone(), crate::text::membership(m)) else { continue };
        let by = m.system.as_ref().and_then(|s| s.inviter.clone()).filter(|b| !b.trim().is_empty() && *b != uid);
        match people.iter_mut().find(|p| p.0 == uid) {
            Some(p) => {
                p.2 = join;
                p.3 = if join { by } else { None };
            }
            None => people.push((uid, join, join, by)),
        }
    }
    // The groups, in the order their first person appears.
    let mut groups: Vec<(Cat, Vec<(String, Option<String>)>)> = Vec::new();
    for (uid, first, last, by) in people {
        let cat = match (first, last) {
            (_, false) => Cat::Left,
            (false, true) => Cat::Rejoined,
            (true, true) => Cat::Joined,
        };
        match groups.iter_mut().find(|g| g.0 == cat) {
            Some(g) => g.1.push((uid, by)),
            None => groups.push((cat, vec![(uid, by)])),
        }
    }
    let Some(main) = (0..groups.len()).fold(None, |best: Option<usize>, i| match best {
        Some(b) if groups[b].1.len() >= groups[i].1.len() => Some(b),
        _ => Some(i),
    }) else {
        return Vec::new();
    };
    let txt = |t: String| blocks::Seg { text: t, ..Default::default() };
    let name_style = Style::new().fg(Color::Gray);
    let person = |uid: &str| blocks::Seg {
        text: names.user(uid).unwrap_or(uid).to_string(),
        style: name_style,
        target: Some(blocks::Target::User(uid.to_string())),
        ..Default::default()
    };
    // "Ana", "Ana and Bia", "Ana and 6 others".
    let who = |g: &[(String, Option<String>)], out: &mut Vec<blocks::Seg>| {
        out.push(person(&g[0].0));
        match g.len() {
            1 => {}
            2 => {
                out.push(txt(tr(" and ", " e ").into()));
                out.push(person(&g[1].0));
            }
            n => {
                let n = n - 1;
                out.push(txt(trf!(" and {n} others", " e mais {n} pessoas")));
            }
        }
    };
    let verb = |cat: Cat, many: bool| -> &'static str {
        match (cat, many) {
            (Cat::Joined, false) => tr("joined", "entrou"),
            (Cat::Joined, true) => tr("joined", "entraram"),
            (Cat::Left, false) => tr("left", "saiu"),
            (Cat::Left, true) => tr("left", "saíram"),
            (Cat::Rejoined, false) => tr("rejoined", "voltou"),
            (Cat::Rejoined, true) => tr("rejoined", "voltaram"),
        }
    };
    let chan = app.current.as_deref().and_then(|c| {
        let sign = if names.private.contains(c) { LOCK } else { "#" };
        names.chan(c).map(|n| blocks::Seg { text: format!("{sign}{n}"), style: name_style, ..Default::default() })
    });
    let (cat, group) = &groups[main];
    let many = group.len() > 1;
    let mut out = Vec::new();
    who(group, &mut out);
    // All added by the same person: "were added to #x by @Ana".
    let by = group[0].1.clone().filter(|b| *cat == Cat::Joined && group.iter().all(|p| p.1.as_ref() == Some(b)));
    let added = by.is_some();
    let lead = match (added, many) {
        (true, false) => tr(" was added to ", " entrou em "),
        (true, true) => tr(" were added to ", " entraram em "),
        _ => "",
    };
    match (chan, added) {
        (Some(c), true) => {
            out.push(txt(lead.into()));
            out.push(c);
        }
        (None, true) => out.push(txt(lead.replace(" to ", " to the channel").replace(" em ", " no canal"))),
        (Some(c), false) => {
            let prep = match *cat {
                _ if !i18n::is_pt() => " ",
                Cat::Joined => " em ",
                Cat::Left => " de ",
                Cat::Rejoined => " para ",
            };
            out.push(txt(format!(" {}{prep}", verb(*cat, many))));
            out.push(c);
        }
        (None, false) => {
            let place = match *cat {
                Cat::Joined => tr("the channel", "no canal"),
                Cat::Left => tr("the channel", "do canal"),
                Cat::Rejoined => tr("the channel", "para o canal"),
            };
            out.push(txt(format!(" {} {place}", verb(*cat, many))));
        }
    }
    if let Some(by) = by {
        out.push(txt(tr(" by ", " a convite de ").into()));
        out.extend(blocks::mrkdwn(&format!("<@{by}>"), names).into_iter().flat_map(|p| p.segs));
    }
    out.push(txt(".".into()));
    // "Also, Bia rejoined and Caio joined."
    let rest: Vec<_> = groups.iter().enumerate().filter(|(i, _)| *i != main).map(|(_, g)| g).collect();
    for (k, (cat, group)) in rest.iter().enumerate() {
        out.push(txt(if k == 0 { tr(" Also, ", " Além disso, ") } else { tr(" and ", " e ") }.into()));
        who(group, &mut out);
        out.push(txt(format!(" {}", verb(*cat, group.len() > 1))));
    }
    if !rest.is_empty() {
        out.push(txt(".".into()));
    }
    out
}

/// Seconds since the epoch of a Slack ts.
fn secs(ts: &str) -> i64 {
    ts.split('.').next().and_then(|s| s.parse().ok()).unwrap_or(0)
}

/// Who wrote it, as its header shows.
pub(crate) fn author(m: &Message, names: &Names) -> String {
    m.user
        .as_deref()
        .map(|u| names.users.get(u).cloned().unwrap_or_else(|| u.to_string()))
        .or_else(|| m.bot_name.clone())
        .unwrap_or_else(|| "bot".into())
}

/// Whether `m` goes under `prev`'s header: same author and day, within five
/// minutes, and not pinned (a pinned message gets its own header).
pub(crate) fn grouped(app: &App, prev: Option<&Message>, m: &Message) -> bool {
    let Some(p) = prev else { return false };
    let key = |m: &Message| m.user.clone().or_else(|| m.bot_name.clone()).unwrap_or_default();
    let day = |m: &Message| Local.timestamp_opt(secs(&m.ts), 0).single().map(|t| t.date_naive());
    let pinned = app.current.as_ref().is_some_and(|c| m.pinned_to.contains(c));
    !pinned && key(p) == key(m) && day(p) == day(m) && secs(&m.ts) - secs(&p.ts) < 300
}

/// A message's time as its header shows it, "21:29" or "9:29 PM".
pub(crate) fn hhmm(ts: &str) -> String {
    Local.timestamp_opt(secs(ts), 0).single().map(|t| i18n::clock(&t)).unwrap_or_default()
}

/// Slack's "(edited)": after the text when it fits, else on a line of its own.
fn edited_mark(lines: &mut Vec<Line<'static>>, top: usize, width: usize, indent: &str) {
    let mark = Span::styled(tr("(edited)", "(editado)"), dim());
    let fits = lines.len() > top && lines.last().is_some_and(|l| l.width() + 10 <= width);
    match lines.last_mut() {
        Some(last) if fits => last.spans.extend([Span::raw(" "), mark]),
        _ => lines.push(Line::from(vec![Span::raw(indent.to_string()), mark])),
    }
}

/// When something happened, the way Slack words it: "today at 14:03",
/// "yesterday at 9:10", "on October 3rd" ("hoje às 14:03", "em 3 de outubro").
fn when(ts: &str) -> Option<String> {
    let t = Local.timestamp_opt(ts.split('.').next()?.parse().ok()?, 0).single()?;
    let today = Local::now().date_naive();
    let hhmm = i18n::clock(&t);
    let (day, month, year) = (t.day(), i18n::month(&t), t.year());
    let nth = i18n::ordinal(day);
    Some(match t.date_naive() {
        d if d == today => trf!("today at {hhmm}", "hoje às {hhmm}"),
        d if today.pred_opt() == Some(d) => trf!("yesterday at {hhmm}", "ontem às {hhmm}"),
        d if d.year() == today.year() => trf!("on {month} {nth}", "em {day} de {month}"),
        _ => trf!("on {month} {nth}, {year}", "em {day} de {month} de {year}"),
    })
}

/// Fits the image in at most 50 columns by 16 rows, keeping its shape.
fn image_cells(file: &File, fw: u16, fh: u16, body_width: usize) -> (u16, u16) {
    let max_cols = body_width.min(50) as f64;
    let max_rows = 16.0;
    let (w, h) = (file.w as f64 / fw as f64, file.h as f64 / fh as f64);
    let scale = (max_cols / w).min(max_rows / h).min(1.0);
    ((w * scale).ceil().max(1.0) as u16, (h * scale).ceil().max(1.0) as u16)
}

/// Links, mentions and quote bars get their own look.
fn styled_line(l: String, indent: &str, muted: bool) -> Line<'static> {
    let base = if muted { dim().italic() } else { Style::new() };
    if let Some(rest) = l.strip_prefix("▎ ").or_else(|| l.strip_prefix("> ")) {
        return Line::from(vec![Span::raw(indent.to_string()), Span::styled("▎ ", dim()), Span::styled(rest.to_string(), base)]);
    }
    let mut spans = vec![Span::raw(indent.to_string())];
    for (i, word) in l.split(' ').enumerate() {
        if i > 0 {
            spans.push(Span::styled(" ", base));
        }
        let style = if word.starts_with("http://") || word.starts_with("https://") || word.starts_with("(http") {
            Style::new().fg(Color::Blue).underlined()
        } else if word.starts_with('@') && word.len() > 1 {
            Style::new().fg(Color::Yellow).bold()
        } else if word.len() > 2 && word.starts_with(':') && word.ends_with(':') {
            // Shortcodes still here are custom emoji (or unknown ones).
            dim()
        } else {
            base
        };
        spans.push(Span::styled(word.to_string(), style));
    }
    Line::from(spans)
}

/// Greedy word wrap by display width; words longer than a line are split.
pub(crate) fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    for para in text.split('\n') {
        let mut line = String::new();
        let mut w = 0;
        for word in para.split(' ') {
            let ww = word.width();
            if w > 0 && w + 1 + ww > width {
                out.push(std::mem::take(&mut line));
                w = 0;
            }
            if w > 0 {
                line.push(' ');
                w += 1;
            }
            for ch in word.chars() {
                let cw = ch.width().unwrap_or(0);
                if w + cw > width {
                    out.push(std::mem::take(&mut line));
                    w = 0;
                }
                line.push(ch);
                w += cw;
            }
        }
        out.push(line);
    }
    out
}

pub(crate) fn truncate(s: &str, width: usize) -> String {
    if s.width() <= width {
        return s.to_string();
    }
    let mut out = String::new();
    let mut w = 0;
    for ch in s.chars() {
        let cw = ch.width().unwrap_or(0);
        if w + cw + 1 > width {
            break;
        }
        out.push(ch);
        w += cw;
    }
    out.push('…');
    out
}

// ---- input and status --------------------------------------------------

/// How often the typing dots take a step.
const TYPING_STEP: Duration = Duration::from_millis(250);

/// The typing dots at this moment. Only their brightness moves, as a wave:
/// the lit dot, the one it just left fading, the other dark. Without photos
/// the one dot in the presence cell pulses instead.
fn typing_dots(app: &App, photos: bool) -> Vec<Span<'static>> {
    const LEVELS: [Color; 3] = [Color::DarkGray, Color::Gray, Color::White];
    let step = (app.clock.elapsed().as_millis() / TYPING_STEP.as_millis()) as usize;
    if !photos {
        // Dark, mid, lit, mid: a pulse.
        let level = [0, 1, 2, 1][step % 4];
        return vec![Span::styled("•", Style::new().fg(LEVELS[level]).bold())];
    }
    let lit = step % 3;
    (0..3)
        .map(|i| {
            let level = if i == lit { 2 } else if (i + 1) % 3 == lit { 1 } else { 0 };
            Span::styled("•", Style::new().fg(LEVELS[level]).bold())
        })
        .collect()
}

/// "Ana is typing", "Ana and Bruno are typing", "Several people are typing".
fn typing_text(who: &[String]) -> Option<String> {
    Some(match who {
        [] => return None,
        [a] => trf!("{a} is typing", "{a} está digitando"),
        [a, b] => trf!("{a} and {b} are typing", "{a} e {b} estão digitando"),
        _ => tr("Several people are typing", "Várias pessoas estão digitando").into(),
    })
}

/// The notice's lines, `width` cells wide: one when it fits, else the
/// archived channel's "Unarchive…" goes on a second line.
fn read_only_lines(app: &App, why: ReadOnly, width: usize) -> Vec<Line<'static>> {
    let lead = Span::styled(format!("{ARCHIVE}  "), dim());
    if why == ReadOnly::Deactivated {
        let text = tr("You are viewing the archives of a deactivated account", "Você está vendo os arquivos de uma conta desativada");
        return vec![Line::from(vec![lead, Span::styled(text, dim().italic())])];
    }
    let name = app.current.as_ref().and_then(|id| app.convs.iter().find(|c| &c.id == id)).map(|c| c.name.clone()).unwrap_or_default();
    if why == ReadOnly::OutsideArchived {
        let text = tr(", an archived channel you are not in. Only its members can unarchive it.", ", um canal arquivado do qual você não participa. Só os membros podem desarquivá-lo.");
        return vec![Line::from(vec![
            lead,
            Span::styled(tr("You are viewing ", "Você está vendo "), dim()),
            Span::styled(format!("#{name}"), Style::new().bold()),
            Span::styled(text, dim().italic()),
        ])];
    }
    if why == ReadOnly::Outside {
        let first = vec![
            Span::styled("#  ", dim()),
            Span::styled(tr("You are viewing ", "Você está vendo "), dim()),
            Span::styled(format!("#{name}"), Style::new().bold()),
            Span::styled(tr(", a channel you are not in. ", ", um canal do qual você não participa. "), dim()),
        ];
        let second = vec![
            Span::styled(join_word(), Style::new().fg(Color::Cyan).underlined()),
            Span::styled(tr(" to send messages.", " para mandar mensagens."), dim()),
        ];
        return split_notice(first, second, width);
    }
    let first = vec![
        lead,
        Span::styled(tr("You are viewing ", "Você está vendo "), dim()),
        Span::styled(format!("#{name}"), Style::new().bold()),
        Span::styled(tr(", an archived channel. ", ", um canal arquivado. "), dim()),
    ];
    // The word to click, like Slack's link.
    let second = vec![
        Span::styled(tr("Unarchive", "Desarquive"), Style::new().fg(Color::Cyan).underlined()),
        Span::styled(tr(" it to continue the discussion.", "-o para continuar a conversa."), dim()),
    ];
    split_notice(first, second, width)
}

/// The clickable words of a preview's notice, like Slack's button.
fn join_word() -> &'static str {
    tr("Join channel", "Entre no canal")
}

/// A notice on one line when it fits, else its second part under the
/// first, lined up with the text past the icon.
fn split_notice(first: Vec<Span<'static>>, second: Vec<Span<'static>>, width: usize) -> Vec<Line<'static>> {
    let w = |spans: &[Span]| spans.iter().map(|s| s.width()).sum::<usize>();
    if w(&first) + w(&second) <= width {
        return vec![Line::from([first, second].concat())];
    }
    let indent = Span::raw(" ".repeat(first[0].width()));
    vec![Line::from(first), Line::from([vec![indent], second].concat())]
}

/// Slack's bar in place of the message box of a read-only conversation:
/// a deactivated account's DM, an archived channel, whose "Unarchive" is
/// clickable (`areas.unarchive`), or a channel you are not in, whose
/// "Join channel" is, in the same place.
fn read_only_notice(f: &mut Frame, app: &mut App, why: ReadOnly, area: Rect) {
    let block = Block::bordered().border_type(BorderType::Rounded).border_style(dim());
    let inner = block.inner(area).inner(ratatui::layout::Margin { horizontal: 1, vertical: 0 });
    f.render_widget(block, area);
    let lines = read_only_lines(app, why, inner.width as usize);
    let word = match why {
        ReadOnly::Archived => Some(tr("Unarchive", "Desarquive")),
        ReadOnly::Outside => Some(join_word()),
        _ => None,
    };
    if let Some(word) = word {
        // The clickable word: wherever it landed, on the first line or the second.
        let found = lines.iter().enumerate().find_map(|(row, l)| {
            let mut x = 0;
            for s in &l.spans {
                if s.content == word {
                    return Some((row, x));
                }
                x += s.width();
            }
            None
        });
        if let Some((row, x)) = found {
            let rect = Rect::new(inner.x + x as u16, inner.y + row as u16, word.width() as u16, 1);
            app.areas.unarchive = rect.intersection(inner);
        }
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn input_box(f: &mut Frame, app: &mut App, area: Rect) {
    app.areas.input = area;
    app.areas.unarchive = Rect::default();
    if let Some(why) = app.read_only() {
        return read_only_notice(f, app, why, area);
    }
    let focused = app.focus == Focus::Input;
    let border = if focused { Style::new().fg(Color::Cyan) } else { dim() };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(border);
    let block = match crate::edit::title(app, false) {
        Some(t) => block.title_top(t).border_style(Style::new().fg(Color::Yellow)),
        None => block,
    };
    let inner = block.inner(area);
    f.render_widget(block, area);
    let inner = inner.inner(ratatui::layout::Margin { horizontal: 1, vertical: 0 });
    let (inner, cards) = crate::attach::split(app, inner, false);
    crate::attach::draw(f, app, cards, false);
    let Some(c) = app.current.as_ref().and_then(|id| app.convs.iter().find(|c| &c.id == id)) else {
        return;
    };

    if app.editor.text.is_empty() {
        let target = match c.kind {
            Kind::Channel | Kind::Private => format!("#{}", c.name),
            _ => app.label(c),
        };
        // A DM adds the other person's status, as Slack does: "Message Ana  🌴 On vacation".
        // A custom emoji leaves two blank cells for its picture; the balloon
        // of a status with only text isn't needed beside the text.
        let mut pic = None;
        let status = match c.kind {
            Kind::Direct => c.user.as_deref().and_then(|u| Some((u, app.users.status(u)?))).map(|(u, (emoji, text))| {
                let shown = match status_emoji(app, u).filter(|_| !emoji.is_empty()) {
                    Some((e, img)) => {
                        pic = img;
                        e
                    }
                    None => String::new(),
                };
                let text = crate::text::shortcodes(text);
                format!("  {}", [shown.as_str(), text.as_str()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" "))
            }).filter(|s| !s.trim().is_empty()),
            _ => None,
        };
        // Your own DM is a notepad, and says so like Slack's.
        let (placeholder, pic) = match app.you_suffix(c) {
            Some(_) => (tr("Jot something down", "Anote alguma coisa").to_string(), None),
            None => {
                let message = trf!("Message {target}", "Mensagem para {target}");
                let pic = pic.map(|img| ((message.width() + 2) as u16, img));
                (message + status.as_deref().unwrap_or_default(), pic)
            }
        };
        f.render_widget(Paragraph::new(placeholder).style(dim()), inner);
        if let Some((x, img)) = pic.filter(|(x, _)| inner.x + x + 2 <= inner.right()) {
            draw_image(f, app, &img, Rect::new(inner.x + x, inner.y, 2, 1));
        }
        if focused {
            f.set_cursor_position((inner.x, inner.y));
        }
        return;
    }
    crate::editor::draw(f, app, false, inner, focused);
}

fn status_line(f: &mut Frame, app: &App, area: Rect) {
    let hints = crate::edit::hint(app).or_else(|| crate::attach::hint(app)).unwrap_or_else(|| {
        match app.focus {
            Focus::Sidebar => tr(
                "Enter open · m or right-click menu · drag to move · / search · q quit",
                "Enter abrir · m ou botão direito menu · arraste para mover · / procurar · q sair",
            ),
            Focus::Filter => tr("type to filter · Enter open · Esc cancel", "digite para filtrar · Enter abrir · Esc cancelar"),
            Focus::Input if app.read_only() == Some(ReadOnly::Archived) => tr(
                "u unarchive · ↑↓ PgUp/PgDn scroll · Esc list · Ctrl+K search",
                "u desarquivar · ↑↓ PgUp/PgDn rolar · Esc lista · Ctrl+K procurar",
            ),
            Focus::Input if app.read_only() == Some(ReadOnly::Outside) => tr(
                "Enter or j join · ↑↓ PgUp/PgDn scroll · Esc list · Ctrl+K search",
                "Enter ou j entrar · ↑↓ PgUp/PgDn rolar · Esc lista · Ctrl+K procurar",
            ),
            Focus::Input if app.read_only() == Some(ReadOnly::OutsideArchived) => tr(
                "↑↓ PgUp/PgDn scroll · Esc list · Ctrl+K search",
                "↑↓ PgUp/PgDn rolar · Esc lista · Ctrl+K procurar",
            ),
            Focus::Input if app.threads.open => tr(
                "↑↓ j k pick a thread · Enter open · r reply · e more replies · Esc list",
                "↑↓ j k escolher conversa · Enter abrir · r responder · e mais respostas · Esc lista",
            ),
            Focus::Thread if app.thread.as_ref().is_some_and(|t| t.inline) => tr(
                "Enter send · ↑↓ other thread (empty box) · Enter open (empty box) · Ctrl+B also send · Esc list",
                "Enter enviar · ↑↓ outra conversa (caixa vazia) · Enter abrir (caixa vazia) · Ctrl+B também enviar · Esc lista",
            ),
            Focus::Input => tr(
                "↑ pick a message · Esc list · PgUp/PgDn scroll · Ctrl+K search",
                "↑ escolher mensagem · Esc lista · PgUp/PgDn rolar · Ctrl+K procurar",
            ),
            Focus::Thread => tr(
                "↑ pick a message · Esc leave the thread · Ctrl+B also send to channel",
                "↑ escolher mensagem · Esc sair da conversa · Ctrl+B também enviar no canal",
            ),
        }
        .to_string()
    });
    let left = match &app.error {
        Some(e) => Span::styled(format!(" ✗ {e}"), Style::new().fg(Color::Red)),
        None => Span::styled(format!(" {}", app.status), dim()),
    };
    // Like Slack, under the message box: "Ana is typing", lined up with the box's text.
    let typing = app.current.as_deref().and_then(|c| typing_text(&app.typing_in(c)));
    let mut spans = vec![left];
    if let Some(t) = typing {
        let at = (app.areas.input.x + 2).saturating_sub(area.x) as usize;
        let gap = at.saturating_sub(spans[0].content.width()).max(1);
        spans.push(Span::raw(" ".repeat(gap)));
        spans.push(Span::styled(t, dim().italic()));
    }
    let used: usize = spans.iter().map(|s| s.content.width()).sum();
    let room = (area.width as usize).saturating_sub(used + hints.width() + 1);
    spans.push(Span::raw(" ".repeat(room)));
    spans.push(Span::styled(hints, dim()));
    let line = Line::from(spans);
    f.render_widget(Paragraph::new(line), area);
}

// ---- emoji -------------------------------------------------------------

/// Clickable spots in the message list besides images and avatars.
enum Hit {
    /// A reaction pill; `None` is the "+" that adds one.
    Pill { line: usize, x: u16, w: u16, ts: String, name: Option<String> },
    /// The lines a message spans, for the right click that reacts to it.
    Message { lines: std::ops::Range<usize>, ts: String },
}

/// Reaction pills: Slack's blue for the ones I'm in, dark gray for the rest.
const PILL_MINE: (Color, Color) = (Color::Rgb(0xE3, 0xF5, 0xFC), Color::Rgb(0x16, 0x4C, 0x6C));
const PILL: (Color, Color) = (Color::Indexed(252), Color::Indexed(236));
const PILL_ADD: (Color, Color) = (Color::Indexed(246), Color::Indexed(236));
/// nf-md-emoticon_plus_outline.
const ADD_REACTION: &str = "\u{f0c70}";

/// Slack's reaction pills under a message, rounded at the ends and wrapping
/// when the row is full, with a smiley "+" at the end to add another.
fn reaction_lines(
    app: &App,
    m: &Message,
    gutter: usize,
    width: usize,
    lines: &mut Vec<Line<'static>>,
    slots: &mut Vec<Slot>,
    hits: &mut Vec<Hit>,
) {
    let me = app.me.as_ref().map(|m| m.user_id.as_str()).unwrap_or("");
    let mut pills: Vec<(String, Style, Color, Option<ImgRef>, Option<String>)> = m
        .reactions
        .iter()
        .map(|r| {
            // A skin tone goes as Slack's picture; as text, unless it didn't load.
            let toned = emoji::toned_img(&r.name)
                .filter(|i| app.picker.is_some() && !matches!(app.images.get(&i.key), Some(Img::Failed)));
            let (label, img) = match toned.map(emoji::Glyph::Image).or_else(|| app.emoji.glyph(&r.name)) {
                Some(emoji::Glyph::Text(e)) => (format!("{e} {}", r.count), None),
                // Two blank cells for the picture.
                Some(emoji::Glyph::Image(img)) if app.picker.is_some() => (format!("   {}", r.count), Some(img)),
                _ => (format!(":{}: {}", r.name, r.count), None),
            };
            let mine = r.users.iter().any(|u| u == me);
            let (fg, bg) = if mine { PILL_MINE } else { PILL };
            let style = if mine { Style::new().fg(fg).bg(bg).bold() } else { Style::new().fg(fg).bg(bg) };
            (label, style, bg, img, Some(r.name.clone()))
        })
        .collect();
    let (fg, bg) = PILL_ADD;
    pills.push((format!(" {ADD_REACTION} "), Style::new().fg(fg).bg(bg), bg, None, None));

    let indent = Span::raw(" ".repeat(gutter));
    let mut row = vec![indent.clone()];
    let mut x = 0;
    for (label, style, bg, img, name) in pills {
        // The label plus a half moon at each end.
        let w = label.width() + 2;
        if x > 0 && x + w > width {
            lines.push(Line::from(std::mem::replace(&mut row, vec![indent.clone()])));
            x = 0;
        }
        let line = lines.len();
        if let Some(img) = img {
            slots.push(Slot { line, x: (gutter + x + 1) as u16, cols: 2, rows: 1, img, full: None, user: None });
        }
        hits.push(Hit::Pill { line, x: (gutter + x) as u16, w: w as u16, ts: m.ts.clone(), name });
        row.push(Span::styled(blocks::CAP_L, Style::new().fg(bg)));
        row.push(Span::styled(label, style));
        row.push(Span::styled(blocks::CAP_R, Style::new().fg(bg)));
        row.push(Span::raw(" "));
        x += w + 1;
    }
    lines.push(Line::from(row));
}

/// Slack's card over a hovered reaction: the emoji big, and who reacted with it.
fn reaction_card(f: &mut Frame, app: &mut App) {
    let Some(at) = app.hover_pill.filter(|_| app.react.is_none()) else { return };
    let Some((pill, ts, Some(name))) = app.areas.reactions.iter().find(|(r, _, n)| n.is_some() && r.contains(at)).cloned() else {
        return;
    };
    let thread = app.areas.thread.contains(at);
    let found = app.pane(thread).and_then(|(_, l)| l.iter().find(|m| m.ts == ts));
    let Some(r) = found.and_then(|m| m.reactions.iter().find(|r| r.name == name)).cloned() else { return };

    // "You, Ana, and 3 others reacted with :heart:", you first like Slack.
    let me = app.me.as_ref().map(|m| m.user_id.clone()).unwrap_or_default();
    let names = app.names();
    let mut who: Vec<String> = Vec::new();
    if r.users.contains(&me) {
        who.push(tr("You", "Você").into());
    }
    who.extend(r.users.iter().filter(|u| **u != me).map(|u| names.user(u).unwrap_or(u).to_string()));
    let others = (r.count as usize).saturating_sub(r.users.len());
    if others > 0 {
        who.push(if who.is_empty() { trf!("{others} people", "{others} pessoas") } else { trf!("{others} others", "mais {others} pessoas") });
    }
    let n = who.len();
    let bold = Style::new().fg(Color::White).bold();
    let seg = |text: String, style: Style| blocks::Seg { text, style, ..Default::default() };
    let mut segs = Vec::new();
    for (i, w) in who.into_iter().enumerate() {
        if i > 0 {
            let sep = match (i + 1 == n, i18n::is_pt()) {
                (false, _) => ", ",
                (true, true) => " e ",
                (true, false) if n > 2 => ", and ",
                (true, false) => " and ",
            };
            segs.push(seg(sep.into(), dim()));
        }
        segs.push(seg(w, bold));
    }
    let verb = if n > 1 || others > 1 { tr("reacted with", "reagiram com") } else { tr("reacted with", "reagiu com") };
    segs.push(seg(format!(" {verb} :{name}:"), dim()));
    let width = 52.min(f.area().width.saturating_sub(6) as usize);
    let para = blocks::Para { kind: blocks::ParaKind::Plain, segs };
    let lines: Vec<Line> = blocks::render(&[blocks::Piece::Text(vec![para], None)], width, None, dim()).lines;

    // The emoji on a light tile of three rows, Slack's picture for a standard one.
    let glyph = app.emoji.glyph(&name);
    let pic = match &glyph {
        Some(emoji::Glyph::Image(i)) => Some(i.clone()),
        Some(emoji::Glyph::Text(_)) => emoji::standard_img(&name),
        None => None,
    };
    let pic = pic.filter(|i| app.picker.is_some() && !matches!(app.images.get(&i.key), Some(Img::Failed)));
    let (fw, fh) = app.picker.as_ref().map(|p| p.font_size()).map(|s| (s.width.max(1), s.height.max(1))).unwrap_or((8, 16));
    let (cols, rows) = match pic {
        Some(_) => (((3.0 * fh as f64 / fw as f64).round() as u16).max(3), 3),
        None => (2, 1),
    };

    let inner_w = lines.iter().map(|l| l.width()).max().unwrap_or(0).max(cols as usize) as u16;
    let (w, h) = (inner_w + 4, rows + lines.len() as u16 + 2);
    let area = f.area();
    let x = (pill.x + pill.width / 2).saturating_sub(w / 2).min(area.right().saturating_sub(w));
    // Above the pill, like Slack; under it when there's no room.
    let y = if pill.y >= area.y + h { pill.y - h } else { pill.y + 1 };
    let rect = Rect::new(x, y, w, h).intersection(area);
    f.render_widget(Clear, rect);
    let block = Block::bordered().border_type(BorderType::Rounded).border_style(dim());
    let inner = block.inner(rect).inner(ratatui::layout::Margin { horizontal: 1, vertical: 0 });
    f.render_widget(block, rect);
    let top = Rect::new(inner.x + inner.width.saturating_sub(cols) / 2, inner.y, cols, rows).intersection(inner);
    match (pic, glyph) {
        (Some(img), _) => {
            f.render_widget(Block::new().style(Style::new().bg(EMOJI_TILE)), top);
            match card_image(app, &img, (top.width as u32 * fw as u32, top.height as u32 * fh as u32)) {
                Some(card) => draw_image(f, app, &card, top),
                // Not here yet: asked for, the tile empty meanwhile.
                None if !app.images.contains_key(&img.key) => app.wanted.push(img),
                None => {}
            }
        }
        (None, Some(emoji::Glyph::Text(e))) => f.render_widget(Paragraph::new(e), top),
        _ => {}
    }
    let text = Rect::new(inner.x, inner.y + rows, inner.width, inner.height.saturating_sub(rows));
    f.render_widget(Paragraph::new(lines.into_iter().map(|l| l.centered()).collect::<Vec<_>>()), text);
}

/// Slack's light square behind the emoji of a reaction's card.
const EMOJI_TILE: Color = Color::Rgb(0xe8, 0xe8, 0xe8);

/// The emoji already sized to the tile's pixels, with Slack's margin around
/// it: drawn as is, not stretched from its 64px by the terminal.
fn card_image(app: &mut App, img: &ImgRef, (w, h): (u32, u32)) -> Option<ImgRef> {
    let key = format!("card-{}-{w}x{h}", img.key);
    if !app.images.contains_key(&key) {
        let Some(Img::Ready(src)) = app.images.get(&img.key) else { return None };
        let side = (w.min(h) as f64 * 0.78) as u32;
        let emoji = src.resize(side, side, image::imageops::FilterType::CatmullRom);
        let mut canvas = image::RgbaImage::new(w, h);
        let (x, y) = ((w - emoji.width()) / 2, (h - emoji.height()) / 2);
        image::imageops::overlay(&mut canvas, &emoji.to_rgba8(), x as i64, y as i64);
        app.images.insert(key.clone(), Img::Ready(image::DynamicImage::ImageRgba8(canvas)));
    }
    Some(ImgRef { key, ..img.clone() })
}

/// One row of the autocomplete or the picker: the emoji, then `:name:`.
/// The "copy" link beside an ID or e-mail, or the confirmation for a
/// moment after a click.
pub(crate) fn copy_hint(app: &App, what: &str) -> (&'static str, Style) {
    match app.copied {
        Some((w, _)) if w == what => (tr("  ✓ copied to clipboard", "  ✓ copiado"), Style::new().fg(Color::Green)),
        _ => (tr("  ⧉ copy", "  ⧉ copiar"), Style::new().fg(Color::Cyan)),
    }
}

pub(crate) fn emoji_row(f: &mut Frame, app: &mut App, rect: Rect, name: &str, glyph: &emoji::Glyph, selected: bool) {
    let shown = match glyph {
        emoji::Glyph::Text(e) => format!("{e}{}", " ".repeat(2usize.saturating_sub(e.width()))),
        emoji::Glyph::Image(_) => "  ".into(),
    };
    let label = truncate(&format!(":{name}:"), (rect.width as usize).saturating_sub(5));
    let mut line = Line::from(format!(" {shown} {label:<w$}", w = (rect.width as usize).saturating_sub(4)));
    if selected {
        mark_selected(&mut line);
    }
    f.render_widget(Paragraph::new(line), rect);
    if let emoji::Glyph::Image(img) = glyph {
        draw_image(f, app, img, Rect::new(rect.x + 1, rect.y, 2, 1));
    }
}

/// The `:` autocomplete, just above the message box.
fn emoji_popup(f: &mut Frame, app: &mut App) {
    app.areas.emoji_rows.clear();
    let Some(hits) = app.completions() else { return };
    let input = app.typing_area();
    let w = (hits.iter().map(|(n, _)| n.width()).max().unwrap_or(0) as u16 + 9).max(24).min(input.width.saturating_sub(2));
    let h = hits.len() as u16 + 2;
    if input.y < h || w < 10 {
        return;
    }
    let rect = Rect::new(input.x + 1, input.y - h, w, h);
    f.render_widget(Clear, rect);
    let block = Block::bordered().border_type(BorderType::Rounded).border_style(dim());
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    let sel = app.complete.sel;
    for (n, (name, glyph)) in hits.iter().enumerate() {
        let row = Rect::new(inner.x, inner.y + n as u16, inner.width, 1);
        emoji_row(f, app, row, name, glyph, n == sel);
        app.areas.emoji_rows.push((row, n));
    }
}

/// The reaction picker: a search field over the same list as the autocomplete.
fn react_picker(f: &mut Frame, app: &mut App) {
    app.areas.picker = Rect::default();
    app.areas.picker_rows.clear();
    let Some(p) = &app.react else { return };
    let (query, sel, x, y) = (p.query.clone(), p.sel, p.x, p.y);
    let hits = app.emoji.search(&query, emoji::ROWS);
    let area = f.area();
    let (w, h) = (34.min(area.width), (hits.len().max(1) as u16 + 4).min(area.height));
    let rect = Rect::new(x.min(area.right().saturating_sub(w)), y.min(area.bottom().saturating_sub(h)), w, h);
    f.render_widget(Clear, rect);
    let block = Block::bordered().border_type(BorderType::Rounded).border_style(dim()).title(Line::from(tr(" React ", " Reagir ")).bold());
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    app.areas.picker = rect;
    let search = if query.is_empty() {
        Line::from(vec![Span::raw(" 🔍 "), Span::styled(tr("Search emoji", "Procurar emoji"), dim())])
    } else {
        Line::from(vec![Span::raw(" 🔍 "), Span::raw(query), Span::raw("▏")])
    };
    f.render_widget(Paragraph::new(search), Rect::new(inner.x, inner.y, inner.width, 1));
    f.render_widget(Paragraph::new("─".repeat(inner.width as usize)).style(dim()), Rect::new(inner.x, inner.y + 1, inner.width, 1));
    if hits.is_empty() {
        f.render_widget(Paragraph::new(tr(" no emoji", " nenhum emoji")).style(dim()), Rect::new(inner.x, inner.y + 2, inner.width, 1));
    }
    for (n, (name, glyph)) in hits.iter().enumerate() {
        let row = Rect::new(inner.x, inner.y + 2 + n as u16, inner.width, 1);
        if row.y >= inner.bottom() {
            break;
        }
        emoji_row(f, app, row, name, glyph, n == sel);
        app.areas.picker_rows.push((row, n));
    }
}
