//! The message box: multi-line text with a cursor, wrapped to the box width.
//! Emoji, mentions and channels sit in the text as one character each, a
//! token, so the cursor, Backspace and wrapping take them whole; Slack gets
//! them as `:name:`, `<@U1>` and `<#C1>`.

use crate::app::{App, Focus};
use crate::emoji::{Emoji, Glyph};
use crate::markup;
use crate::slack::Users;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use crate::ui::{CODE_BG, CODE_FG};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use std::collections::HashMap;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Tokens are characters of the last private-use plane, which nothing types.
const BASE: u32 = 0x10_0000;
const LAST: u32 = 0x10_FFFD;

pub fn is_token(c: char) -> bool {
    c as u32 >= BASE
}

/// How a char looks in the box, from its `markup::scan` flags.
fn typed_style(look: u16) -> Style {
    let mut s = Style::new();
    if look & markup::PRE != 0 {
        s = s.fg(Color::Gray).bg(CODE_BG);
    }
    for (bit, m) in [(markup::BOLD, Modifier::BOLD), (markup::ITALIC, Modifier::ITALIC), (markup::STRIKE, Modifier::CROSSED_OUT)] {
        if look & bit != 0 {
            s = s.add_modifier(m);
        }
    }
    if look & markup::CODE != 0 {
        s = s.fg(CODE_FG).bg(CODE_BG);
    }
    if look & markup::MARK != 0 {
        s = s.fg(Color::DarkGray).remove_modifier(Modifier::CROSSED_OUT);
    }
    s
}

/// Mentions and channels in the box: Slack's blue highlight.
fn pill() -> Style {
    Style::new().fg(Color::Rgb(0xE3, 0xF5, 0xFC)).bg(Color::Rgb(0x16, 0x4C, 0x6C))
}

#[derive(Clone)]
pub enum Token {
    /// Drawn as the emoji, two cells; sent as `:name:`.
    Emoji { name: String, glyph: Glyph },
    /// A mention or a channel: drawn as a pill with `label`, sent as `raw` (`<@U1>`).
    Ref { label: String, raw: String },
}

impl Token {
    fn width(&self) -> usize {
        match self {
            Token::Emoji { glyph: Glyph::Text(s), .. } => s.width().max(2),
            Token::Emoji { .. } => 2,
            Token::Ref { label, .. } => label.width(),
        }
    }

    fn wire(&self) -> String {
        match self {
            Token::Emoji { name, .. } => format!(":{name}:"),
            Token::Ref { raw, .. } => raw.clone(),
        }
    }
}

/// What turns `<@U1>`, `<#C1>` and `:name:` into tokens. Built from the
/// app's fields, not `&App`, so it can sit beside a `&mut` to the box.
pub struct Lookup<'a> {
    pub users: &'a Users,
    pub chans: &'a HashMap<String, String>,
    pub emoji: &'a Emoji,
    /// The terminal shows pictures; without them custom emoji stay `:name:`.
    pub pictures: bool,
}

impl Lookup<'_> {
    pub fn emoji(&self, name: &str) -> Option<Token> {
        let glyph = match self.emoji.glyph(name)? {
            Glyph::Image(_) if !self.pictures => Glyph::Text(format!(":{name}:")),
            g => g,
        };
        Some(Token::Emoji { name: name.to_string(), glyph })
    }

    /// `inner` is what Slack writes between `<` and `>`: `@U1`, `#C1|general`, `!here`.
    pub fn reference(&self, inner: &str) -> Option<Token> {
        crate::slack::slack_ref(&format!("<{inner}>"))?;
        let (target, label) = inner.split_once('|').unwrap_or((inner, ""));
        let label = unescape(label);
        let or_label = |id: &str| if label.is_empty() { id.to_string() } else { label.trim_start_matches(['@', '#']).to_string() };
        let shown = if let Some(id) = target.strip_prefix('@') {
            format!("@{}", self.users.names.get(id).cloned().unwrap_or_else(|| or_label(id)))
        } else if let Some(id) = target.strip_prefix('#') {
            format!("#{}", self.chans.get(id).cloned().unwrap_or_else(|| or_label(id)))
        } else {
            let special = target.strip_prefix('!')?;
            match special {
                "here" | "channel" | "everyone" => format!("@{special}"),
                _ if !label.is_empty() => label.clone(),
                _ => format!("@{}", special.split('^').next().unwrap_or(special)),
            }
        };
        Some(Token::Ref { label: shown, raw: format!("<{inner}>") })
    }
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
}

fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || "_+-'".contains(c)
}

#[derive(Clone, Default)]
pub struct Editor {
    pub text: String,
    /// In chars.
    pub cursor: usize,
    /// What each token character stands for, by its distance from `BASE`.
    tokens: Vec<Token>,
}

/// One visual row: where it starts in `text` (chars) and what it shows.
pub struct Row {
    pub start: usize,
    pub chars: Vec<char>,
}

impl Editor {
    fn byte(&self, c: usize) -> usize {
        self.text.char_indices().nth(c).map(|(i, _)| i).unwrap_or(self.text.len())
    }

    fn len(&self) -> usize {
        self.text.chars().count()
    }

    pub fn token(&self, c: char) -> Option<&Token> {
        self.tokens.get((c as u32).checked_sub(BASE)? as usize)
    }

    /// Cells a character takes: a token its emoji's or pill's.
    fn cells(&self, c: char) -> usize {
        match self.token(c) {
            Some(t) => t.width(),
            None => c.width().unwrap_or(0),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty()
    }

    /// The text as Slack should get it, tokens written out. Not escaped:
    /// `slack::escape` leaves the mentions alone.
    pub fn wire(&self) -> String {
        self.text.chars().map(|c| self.token(c).map_or_else(|| c.to_string(), Token::wire)).collect()
    }

    /// Empties the box, giving back its text as Slack should get it.
    pub fn take(&mut self) -> String {
        let s = self.wire();
        self.cursor = 0;
        self.text.clear();
        self.tokens.clear();
        s
    }

    pub fn set(&mut self, text: String) {
        self.tokens.clear();
        self.text = text.chars().filter(|c| !is_token(*c)).collect();
        self.cursor = self.len();
    }

    /// Text with mentions and emoji in it: what was sent (`escaped` false)
    /// or a message as Slack has it (`escaped`, links shown as their URL).
    pub fn load(&mut self, s: &str, escaped: bool, look: &Lookup) {
        self.set(String::new());
        let mut rest = s;
        while let Some(i) = rest.find('<') {
            self.plain(&rest[..i], escaped, look);
            let after = &rest[i + 1..];
            let Some(j) = after.find('>') else {
                rest = &rest[i..];
                break;
            };
            let inner = &after[..j];
            match look.reference(inner) {
                Some(t) => self.insert_token(t),
                None if escaped => {
                    let target = inner.split('|').next().unwrap_or(inner);
                    self.insert(&unescape(target.strip_prefix("mailto:").unwrap_or(target)));
                }
                None => self.insert(&format!("<{inner}>")),
            }
            rest = &after[j + 1..];
        }
        self.plain(rest, escaped, look);
    }

    /// A stretch without links, its known `:name:`s as emoji.
    fn plain(&mut self, s: &str, escaped: bool, look: &Lookup) {
        let s = if escaped { unescape(s) } else { s.to_string() };
        let mut rest = s.as_str();
        while let Some(i) = rest.find(':') {
            self.insert(&rest[..i]);
            let after = &rest[i + 1..];
            let name_len = after.find(':').filter(|&j| j > 0 && after[..j].chars().all(is_name_char));
            // Slack writes a skin tone as a second shortcode: `:+1::skin-tone-2:`.
            let toned = name_len.and_then(|j| {
                after[j..].get(..14).filter(|t| t.starts_with("::skin-tone-") && t.ends_with(':'))?;
                Some(j + 13)
            });
            let hit = [toned, name_len].into_iter().flatten().find_map(|j| Some((j, look.emoji(&after[..j])?)));
            match hit {
                Some((j, t)) => {
                    self.insert_token(t);
                    rest = &after[j + 1..];
                }
                None => {
                    self.insert(":");
                    rest = after;
                }
            }
        }
        self.insert(rest);
    }

    pub fn insert(&mut self, s: &str) {
        let s: String = s.replace("\r\n", "\n").replace('\r', "\n").chars().filter(|c| !is_token(*c)).collect();
        let i = self.byte(self.cursor);
        self.text.insert_str(i, &s);
        self.cursor += s.chars().count();
    }

    pub fn insert_token(&mut self, t: Token) {
        let Some(c) = char::from_u32(BASE + self.tokens.len() as u32).filter(|c| *c as u32 <= LAST) else { return };
        self.tokens.push(t);
        let i = self.byte(self.cursor);
        self.text.insert(i, c);
        self.cursor += 1;
    }

    /// Replaces what was typed from `start` to the cursor (an autocomplete's
    /// `:sm` or `@an`) with the token and a space, like Slack.
    pub fn complete(&mut self, start: usize, t: Token) {
        let (a, b) = (self.byte(start.min(self.cursor)), self.byte(self.cursor));
        self.text.replace_range(a..b, "");
        self.cursor = start.min(self.cursor);
        self.insert_token(t);
        self.insert(" ");
    }

    /// The `:name:` just closed before the cursor becomes its emoji, when there is one.
    pub fn shortcode(&mut self, look: &Lookup) {
        let before: Vec<char> = self.text.chars().take(self.cursor).collect();
        let Some((&':', head)) = before.split_last() else { return };
        let mut i = head.len();
        while i > 0 && is_name_char(head[i - 1]) {
            i -= 1;
        }
        if i == 0 || i == head.len() || head[i - 1] != ':' {
            return;
        }
        let opens = i - 1;
        if opens > 0 && !head[opens - 1].is_whitespace() && !is_token(head[opens - 1]) {
            return;
        }
        let name: String = head[i..].iter().collect();
        if let Some(t) = look.emoji(&name) {
            let (a, b) = (self.byte(opens), self.byte(self.cursor));
            self.text.replace_range(a..b, "");
            self.cursor = opens;
            self.insert_token(t);
        }
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            let i = self.byte(self.cursor);
            self.text.remove(i);
        }
    }

    pub fn delete(&mut self) {
        if self.cursor < self.len() {
            let i = self.byte(self.cursor);
            self.text.remove(i);
        }
    }

    pub fn delete_word(&mut self) {
        let end = self.byte(self.cursor);
        let head = self.text[..end].trim_end_matches([' ', '\n']);
        let start = head.rfind([' ', '\n']).map(|i| i + 1).unwrap_or(0);
        self.text.replace_range(start..end, "");
        self.cursor = self.text[..start].chars().count();
    }

    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.len());
    }

    /// Start and end of the logical line (between newlines) holding the cursor.
    pub fn home(&mut self) {
        let chars: Vec<char> = self.text.chars().collect();
        while self.cursor > 0 && chars[self.cursor - 1] != '\n' {
            self.cursor -= 1;
        }
    }

    pub fn end(&mut self) {
        let chars: Vec<char> = self.text.chars().collect();
        while self.cursor < chars.len() && chars[self.cursor] != '\n' {
            self.cursor += 1;
        }
    }

    /// The logical line holding the cursor: its start and end (chars) and the text.
    fn line(&self) -> (usize, usize, Vec<char>) {
        let chars: Vec<char> = self.text.chars().collect();
        let start = chars[..self.cursor].iter().rposition(|&c| c == '\n').map_or(0, |p| p + 1);
        let end = chars[self.cursor..].iter().position(|&c| c == '\n').map_or(chars.len(), |p| self.cursor + p);
        (start, end, chars)
    }

    fn remove(&mut self, start: usize, end: usize) {
        let (a, b) = (self.byte(start), self.byte(end));
        self.text.replace_range(a..b, "");
        self.cursor = if self.cursor >= end { self.cursor - (end - start) } else { self.cursor.min(start) };
    }

    /// The cursor is in a ``` block: Enter breaks the line instead of sending.
    pub fn in_code(&self) -> bool {
        let before: Vec<char> = self.text.chars().take(self.cursor).collect();
        crate::markup::in_block(&before)
    }

    /// Shift+Enter: a new line that stays in the quote or list (the next
    /// number); on an empty item the prefix goes instead, ending the list.
    pub fn newline(&mut self) {
        if self.in_code() {
            return self.insert("\n");
        }
        let (start, end, chars) = self.line();
        let Some((next, n)) = crate::markup::continued(&chars[start..end]) else { return self.insert("\n") };
        if chars[start + n..end].iter().all(|c| c.is_whitespace()) {
            return self.remove(start, start + n);
        }
        self.insert(&format!("\n{next}"));
    }

    /// Ctrl+B and the like: a pair of `mark` with the cursor between them.
    /// Right before a closing mark it steps over it; on an empty pair it
    /// takes the pair away.
    pub fn wrap(&mut self, mark: &str) {
        let m: Vec<char> = mark.chars().collect();
        let (start, _, chars) = self.line();
        let after = chars[self.cursor..].starts_with(&m);
        let before = self.cursor >= start + m.len() && chars[self.cursor - m.len()..self.cursor] == m[..];
        if after && before {
            return self.remove(self.cursor - m.len(), self.cursor + m.len());
        }
        let opened = chars[start..self.cursor].windows(m.len()).filter(|w| *w == &m[..]).count() % 2 == 1;
        if after && opened {
            self.cursor += m.len();
            return;
        }
        // A mark glued to a word doesn't count.
        if self.cursor > start && chars[self.cursor - 1].is_alphanumeric() {
            self.insert(" ");
        }
        self.insert(&mark.repeat(2));
        self.cursor -= m.len();
    }

    /// Ctrl+Shift+9: the line becomes a quote, or stops being one.
    pub fn quote(&mut self) {
        let (start, _, chars) = self.line();
        match chars.get(start) {
            Some('>') if chars.get(start + 1) == Some(&' ') => self.remove(start, start + 2),
            Some('>') => self.remove(start, start + 1),
            _ => {
                let at = self.cursor;
                self.cursor = start;
                self.insert("> ");
                self.cursor = at + 2;
            }
        }
    }

    /// Ctrl+Shift+8 and Ctrl+Shift+7: the line becomes a bulleted (or
    /// numbered) item, or stops being one; a list of the other kind switches.
    /// A quote's `> ` stays in front.
    pub fn list(&mut self, numbered: bool) {
        let (start, end, chars) = self.line();
        let q = match (chars.get(start), chars.get(start + 1)) {
            (Some('>'), Some(' ')) => 2,
            (Some('>'), _) => 1,
            _ => 0,
        };
        let line = &chars[(start + q).min(end)..end];
        let sp = line.iter().take_while(|c| **c == ' ').count();
        let at = start + q + sp;
        if let Some((_, n)) = crate::markup::continued(line).filter(|_| line.first() != Some(&'>')) {
            let was_numbered = line.get(sp).is_some_and(|c| c.is_ascii_digit());
            self.remove(at, start + q + n);
            if was_numbered == numbered {
                return;
            }
        }
        let mark = if numbered { "1. " } else { "- " };
        let cursor = self.cursor;
        self.cursor = at;
        self.insert(mark);
        self.cursor = if cursor >= at { cursor + mark.len() } else { cursor };
    }

    pub fn rows(&self, width: usize) -> Vec<Row> {
        let width = width.max(2);
        let mut rows = vec![Row { start: 0, chars: Vec::new() }];
        let mut w = 0;
        for (i, ch) in self.text.chars().enumerate() {
            if ch == '\n' {
                rows.push(Row { start: i + 1, chars: Vec::new() });
                w = 0;
                continue;
            }
            let cw = self.cells(ch);
            // A pill wider than the box gets a row of its own, cut at the edge.
            if w + cw > width && w > 0 {
                rows.push(Row { start: i, chars: Vec::new() });
                w = 0;
            }
            rows.last_mut().unwrap().chars.push(ch);
            w += cw;
        }
        rows
    }

    /// Cursor as (row, column in cells).
    pub fn position(&self, rows: &[Row]) -> (usize, usize) {
        let r = rows.iter().rposition(|r| r.start <= self.cursor).unwrap_or(0);
        let n = (self.cursor - rows[r].start).min(rows[r].chars.len());
        (r, rows[r].chars[..n].iter().map(|&c| self.cells(c)).sum())
    }

    /// Moves a visual row up or down, keeping the column. False at the edge.
    pub fn vertical(&mut self, width: usize, delta: isize) -> bool {
        let rows = self.rows(width);
        let (r, col) = self.position(&rows);
        let target = r as isize + delta;
        if target < 0 || target >= rows.len() as isize {
            return false;
        }
        let row = &rows[target as usize];
        let mut w = 0;
        let mut k = 0;
        for &ch in &row.chars {
            let cw = self.cells(ch);
            if w + cw > col {
                break;
            }
            w += cw;
            k += 1;
        }
        self.cursor = row.start + k;
        true
    }
}

impl App {
    /// The box being typed in: the conversation's or the thread's.
    pub fn typing_editor(&self) -> Option<&Editor> {
        match self.focus {
            Focus::Input => Some(&self.editor),
            Focus::Thread => self.thread.as_ref().map(|t| &t.editor),
            _ => None,
        }
    }

    pub fn typing_editor_mut(&mut self) -> Option<&mut Editor> {
        match self.focus {
            Focus::Input => Some(&mut self.editor),
            Focus::Thread => self.thread.as_mut().map(|t| &mut t.editor),
            _ => None,
        }
    }

    /// Formatting shortcuts in the box being typed in, as in Slack: Ctrl+B
    /// bold (Alt+B too, as the thread's Ctrl+B also sends to the channel),
    /// Ctrl+I italic (the terminal must tell it from Tab), Ctrl+Shift+X
    /// strike, Ctrl+Shift+C code (Alt+X and Alt+C where the terminal keeps
    /// those), Ctrl+Alt+Shift+C a code block, Ctrl+Shift+9 quote and
    /// Ctrl+Shift+8 / Ctrl+Shift+7 bulleted / numbered list. True when handled.
    pub fn format_key(&mut self, k: KeyEvent) -> bool {
        let m = k.modifiers;
        let (ctrl, shift, alt) = (m.contains(KeyModifiers::CONTROL), m.contains(KeyModifiers::SHIFT), m.contains(KeyModifiers::ALT));
        let KeyCode::Char(c) = k.code else { return false };
        let thread = self.focus == Focus::Thread;
        let mark = match c.to_ascii_lowercase() {
            'b' if ctrl && !shift && !alt && !thread => "*",
            'b' if alt && !ctrl => "*",
            'i' if ctrl && !shift && !alt => "_",
            'c' if ctrl && shift && alt => "```",
            'x' | 'c' if (ctrl && shift && !alt) || (alt && !ctrl) => if c.eq_ignore_ascii_case(&'x') { "~" } else { "`" },
            '9' | '(' if ctrl && shift && !alt => ">",
            '8' | '*' if ctrl && shift && !alt => "- ",
            '7' | '&' if ctrl && shift && !alt => "1. ",
            _ => return false,
        };
        if self.focus == Focus::Input && self.read_only().is_some() {
            return false;
        }
        let Some(e) = self.typing_editor_mut() else { return false };
        match mark {
            ">" => e.quote(),
            "- " | "1. " => e.list(mark != "- "),
            _ => e.wrap(mark),
        }
        true
    }

    /// Where the box being typed in is, for the popups over it.
    pub fn typing_area(&self) -> Rect {
        if self.focus == Focus::Thread { self.areas.thread_input } else { self.areas.input }
    }
}

/// The box's text in `area`, scrolled to the cursor: tokens as emoji (custom
/// ones as their picture) and pills. The cursor goes there when `focused`.
pub fn draw(f: &mut Frame, app: &mut App, thread: bool, area: Rect, focused: bool) {
    let ed = if thread { app.thread.as_ref().map(|t| &t.editor) } else { Some(&app.editor) };
    let Some(ed) = ed else { return };
    let rows = ed.rows(area.width as usize);
    let (cr, cc) = ed.position(&rows);
    let h = area.height as usize;
    let top = (cr + 1).saturating_sub(h);
    // Slack's markup, styled as it is typed; the markers stay, dimmed.
    let looks = markup::scan(&ed.text.chars().collect::<Vec<_>>(), true);
    let pre = |i: usize| looks.get(i).is_some_and(|l| l & markup::PRE != 0);
    let mut lines = Vec::new();
    let mut pictures = Vec::new();
    for (y, row) in rows[top..].iter().take(h).enumerate() {
        let (mut spans, mut run, mut x) = (Vec::new(), String::new(), 0);
        let mut style = Style::new();
        for (k, &ch) in row.chars.iter().enumerate() {
            let look = looks.get(row.start + k).copied().unwrap_or(0);
            let Some(t) = ed.token(ch) else {
                let s = typed_style(look);
                if s != style && !run.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut run), style));
                }
                style = s;
                run.push(match ch {
                    '>' if look & markup::QUOTE != 0 => '▎',
                    '-' | '*' if look & markup::BULLET != 0 => '•',
                    _ => ch,
                });
                x += ch.width().unwrap_or(0);
                continue;
            };
            if !run.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut run), style));
            }
            spans.push(match t {
                Token::Emoji { glyph: Glyph::Text(s), .. } => Span::raw(format!("{s}{}", " ".repeat(2usize.saturating_sub(s.width())))),
                Token::Emoji { glyph: Glyph::Image(img), .. } => {
                    pictures.push((img.clone(), x, y));
                    Span::raw("  ")
                }
                Token::Ref { label, .. } => Span::styled(label.clone(), pill()),
            });
            x += t.width();
        }
        if !run.is_empty() {
            spans.push(Span::styled(run, style));
        }
        // A code block's rows are filled to the edge, like a block.
        let in_pre = match row.chars.len() {
            0 => row.start > 0 && pre(row.start - 1),
            n => pre(row.start + n - 1),
        };
        if in_pre && x < area.width as usize {
            spans.push(Span::styled(" ".repeat(area.width as usize - x), Style::new().bg(CODE_BG)));
        }
        lines.push(Line::from(spans));
    }
    f.render_widget(Paragraph::new(lines), area);
    for (img, x, y) in pictures {
        if x + 2 <= area.width as usize {
            crate::ui::draw_image(f, app, &img, Rect::new(area.x + x as u16, area.y + y as u16, 2, 1));
        }
    }
    if focused {
        f.set_cursor_position((area.x + cc.min(area.width as usize) as u16, area.y + (cr - top) as u16));
    }
}
