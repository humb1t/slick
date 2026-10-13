//! Emoji: the workspace's custom ones, the `:` autocomplete in the message
//! box, the reaction picker and the reactions themselves.

use crate::app::{App, Event, ImgRef};
use crate::editor::Lookup;
use crate::slack::{Res, Slack};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use emojis::SkinTone;
use ratatui::backend::{Backend, ClearType, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Rect, Size};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;
use unicode_width::UnicodeWidthChar;

/// What the picker offers before anything is typed, and ranks first after.
const FREQUENT: [&str; 6] = ["+1", "heart", "joy", "tada", "eyes", "white_check_mark"];
/// Rows in the autocomplete popup and in the picker.
pub const ROWS: usize = 8;

/// The workspace's custom emoji, aliases already resolved.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Emoji {
    /// name → image URL.
    pub custom: HashMap<String, String>,
    /// Custom names that alias a standard emoji (`white_square` → ⬜).
    #[serde(default)]
    pub unicode: HashMap<String, String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Reaction {
    /// Skin tones come attached: `+1::skin-tone-2`.
    pub name: String,
    /// Slack may list fewer people than `count`.
    #[serde(default)]
    pub users: Vec<String>,
    #[serde(default)]
    pub count: u64,
}

/// How an emoji is drawn: a character, or a picture for custom ones.
#[derive(Clone)]
pub enum Glyph {
    Text(String),
    Image(ImgRef),
}

pub enum Ev {
    List(Res<Emoji>),
    /// A reaction toggled here reached Slack, or not.
    Reacted { chan: String, ts: String, res: Res<()> },
    /// A message's reactions as Slack has them, fetched after a failed toggle.
    Reactions { chan: String, ts: String, res: Res<Vec<Reaction>> },
}

/// The `:` popup over the message box.
#[derive(Default)]
pub struct Complete {
    pub sel: usize,
    /// The fragment it was last shown for; a new one starts at the top.
    last: String,
    /// Esc hides it until the fragment changes.
    dismissed: Option<String>,
}

/// The reaction picker, opened from a message.
pub struct ReactPicker {
    pub chan: String,
    pub ts: String,
    pub query: String,
    pub sel: usize,
    pub x: u16,
    pub y: u16,
}

pub fn fetch(s: &Slack) -> Res<Emoji> {
    let v = s.call("emoji.list", &[])?;
    let raw: HashMap<&str, &str> =
        v["emoji"].as_object().into_iter().flatten().filter_map(|(k, v)| Some((k.as_str(), v.as_str()?))).collect();
    let mut out = Emoji::default();
    for (&name, &val) in &raw {
        // Aliases point at another custom emoji, or at a standard one.
        let mut val = val;
        for _ in 0..4 {
            let Some(target) = val.strip_prefix("alias:") else { break };
            match raw.get(target) {
                Some(next) => val = next,
                None => {
                    if let Some(s) = standard(target) {
                        out.unicode.insert(name.to_string(), s);
                    }
                    break;
                }
            }
        }
        if val.starts_with("http") {
            out.custom.insert(name.to_string(), val.to_string());
        }
    }
    Ok(out)
}

fn react(s: &Slack, chan: &str, ts: &str, name: &str, on: bool) -> Res<()> {
    let method = if on { "reactions.add" } else { "reactions.remove" };
    s.call(method, &[("channel", chan), ("timestamp", ts), ("name", name)]).map(|_| ())
}

fn reactions(s: &Slack, chan: &str, ts: &str) -> Res<Vec<Reaction>> {
    let v = s.call("reactions.get", &[("channel", chan), ("timestamp", ts), ("full", "true")])?;
    Ok(serde_json::from_value(v["message"]["reactions"].clone()).unwrap_or_default())
}

/// A standard emoji by its Slack name, skin tone included (`+1::skin-tone-2`).
pub fn standard(name: &str) -> Option<String> {
    let (base, tone) = name.split_once("::").unwrap_or((name, ""));
    let e = emojis::get_by_shortcode(base)?;
    let tone = match tone {
        "skin-tone-2" => SkinTone::Light,
        "skin-tone-3" => SkinTone::MediumLight,
        "skin-tone-4" => SkinTone::Medium,
        "skin-tone-5" => SkinTone::MediumDark,
        "skin-tone-6" => SkinTone::Dark,
        _ => return Some(e.as_str().to_string()),
    };
    Some(e.with_skin_tone(tone).unwrap_or(e).as_str().to_string())
}

/// The terminal, with a skin tone on a text-style base (✌ ☝ ✍) sent as
/// `✌ FE0F 🏾`. Without the FE0F the terminal draws the plain glyph and drops
/// the tone; with it in the buffer, ratatui would count four columns where
/// the terminal draws two. So the cell keeps `✌🏾` and gets it on the way out.
pub struct Toned<B>(pub B);

impl<B: Backend> Backend for Toned<B> {
    type Error = B::Error;

    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        let fixed: Vec<(u16, u16, Cell)> = content
            .map(|(x, y, c)| {
                let mut chars = c.symbol().chars();
                let (base, tone) = (chars.next(), chars.next());
                let mut c = c.clone();
                if let (Some(b), Some(t @ '\u{1f3fb}'..='\u{1f3ff}')) = (base, tone) {
                    if b.width() == Some(1) {
                        c.set_symbol(&format!("{b}\u{fe0f}{t}{}", chars.as_str()));
                    }
                }
                (x, y, c)
            })
            .collect();
        self.0.draw(fixed.iter().map(|(x, y, c)| (*x, *y, c)))
    }
    fn append_lines(&mut self, n: u16) -> Result<(), Self::Error> {
        self.0.append_lines(n)
    }
    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        self.0.hide_cursor()
    }
    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        self.0.show_cursor()
    }
    fn get_cursor_position(&mut self) -> Result<Position, Self::Error> {
        self.0.get_cursor_position()
    }
    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> Result<(), Self::Error> {
        self.0.set_cursor_position(position)
    }
    fn clear(&mut self) -> Result<(), Self::Error> {
        self.0.clear()
    }
    fn clear_region(&mut self, clear_type: ClearType) -> Result<(), Self::Error> {
        self.0.clear_region(clear_type)
    }
    fn size(&self) -> Result<Size, Self::Error> {
        self.0.size()
    }
    fn window_size(&mut self) -> Result<WindowSize, Self::Error> {
        self.0.window_size()
    }
    fn flush(&mut self) -> Result<(), Self::Error> {
        self.0.flush()
    }
}

/// A custom emoji's picture. The hash in its URL names the file on disk, so
/// an emoji redrawn under the same name is downloaded again.
pub fn img(name: &str, url: &str) -> ImgRef {
    let hash = url.rsplit('/').next().unwrap_or("").split('.').next().unwrap_or("");
    ImgRef { key: format!("emoji-{name}-{hash}"), url: url.to_string(), name: String::new(), link: None }
}

/// Frames kept per animated emoji, and their largest side in pixels: enough
/// for the small emoji and a bigger one, without holding whole GIFs in memory.
const MAX_FRAMES: usize = 60;
const FRAME_PX: u32 = 64;

/// An animated custom emoji: each frame already composed, with how long it shows.
pub struct Anim {
    pub frames: Vec<image::DynamicImage>,
    /// Where each frame ends, from the start of the loop.
    ends: Vec<Duration>,
}

impl Anim {
    /// Decodes an animated GIF or WebP; `None` for still pictures.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        use image::AnimationDecoder;
        let cur = std::io::Cursor::new(bytes);
        let frames: Vec<image::Frame> = if bytes.starts_with(b"GIF8") {
            image::codecs::gif::GifDecoder::new(cur).ok()?.into_frames().take(MAX_FRAMES).collect::<Result<_, _>>().ok()?
        } else if bytes.len() > 12 && &bytes[8..12] == b"WEBP" {
            let d = image::codecs::webp::WebPDecoder::new(cur).ok()?;
            if !d.has_animation() {
                return None;
            }
            d.into_frames().take(MAX_FRAMES).collect::<Result<_, _>>().ok()?
        } else {
            return None;
        };
        if frames.len() < 2 {
            return None;
        }
        let mut out = Self { frames: Vec::new(), ends: Vec::new() };
        let mut t = Duration::ZERO;
        for f in frames {
            let (n, d) = f.delay().numer_denom_ms();
            let ms = n / d.max(1);
            // Like browsers: too-short delays mean "as fast as allowed", read as 100ms.
            t += Duration::from_millis(if ms < 20 { 100 } else { ms as u64 });
            out.ends.push(t);
            let img = image::DynamicImage::ImageRgba8(f.into_buffer());
            let big = img.width().max(img.height()) > FRAME_PX;
            out.frames.push(if big { img.thumbnail(FRAME_PX, FRAME_PX) } else { img });
        }
        Some(out)
    }

    /// The frame showing `t` after the clock started, and how long until the next one.
    pub fn frame_at(&self, t: Duration) -> (usize, Duration) {
        let total = *self.ends.last().unwrap();
        let t = Duration::from_nanos((t.as_nanos() % total.as_nanos()) as u64);
        let i = self.ends.iter().position(|e| *e > t).unwrap_or(0);
        (i, self.ends[i].saturating_sub(t))
    }
}

impl Emoji {
    pub fn glyph(&self, name: &str) -> Option<Glyph> {
        if let Some(s) = standard(name).or_else(|| self.unicode.get(name).cloned()) {
            return Some(Glyph::Text(s));
        }
        self.custom.get(name).map(|url| Glyph::Image(img(name, url)))
    }

    /// Standard and custom emoji whose name has `query`: prefix matches
    /// first, then the frequent ones, then the shortest names.
    pub fn search(&self, query: &str, limit: usize) -> Vec<(String, Glyph)> {
        enum Src<'a> {
            Std(&'static emojis::Emoji),
            Custom(&'a str),
            Alias(&'a str),
        }
        let q = query.to_lowercase();
        let rank = |name: &str| {
            let prefix = name.starts_with(&q);
            (prefix || name.contains(&q)).then(|| (!prefix, !FREQUENT.contains(&name), name.len()))
        };
        let mut hits = Vec::new();
        // One row per standard emoji, under its best-matching shortcode.
        for e in emojis::iter() {
            if let Some((r, name)) = e.shortcodes().filter_map(|n| Some((rank(n)?, n))).min() {
                hits.push((r, name, Src::Std(e)));
            }
        }
        for (name, url) in &self.custom {
            if let Some(r) = rank(name) {
                hits.push((r, name.as_str(), Src::Custom(url)));
            }
        }
        for (name, s) in &self.unicode {
            if let Some(r) = rank(name) {
                hits.push((r, name.as_str(), Src::Alias(s)));
            }
        }
        hits.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
        hits.into_iter()
            .take(limit)
            .map(|(_, name, src)| {
                let glyph = match src {
                    Src::Std(e) => Glyph::Text(e.as_str().to_string()),
                    Src::Custom(url) => Glyph::Image(img(name, url)),
                    Src::Alias(s) => Glyph::Text(s.to_string()),
                };
                (name.to_string(), glyph)
            })
            .collect()
    }
}

/// Adds or takes back `me`'s reaction, here only; true when it was added.
pub fn toggle(list: &mut Vec<Reaction>, name: &str, me: &str) -> bool {
    match list.iter().position(|r| r.name == name) {
        Some(i) if list[i].users.iter().any(|u| u == me) => {
            let r = &mut list[i];
            r.users.retain(|u| u != me);
            r.count = r.count.saturating_sub(1);
            if r.count == 0 {
                list.remove(i);
            }
            false
        }
        Some(i) => {
            list[i].users.push(me.to_string());
            list[i].count += 1;
            true
        }
        None => {
            list.push(Reaction { name: name.to_string(), users: vec![me.to_string()], count: 1 });
            true
        }
    }
}

fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || "_+-'".contains(c)
}

/// The `:fragment` typed right before the cursor, like Slack: after a space
/// or at the start, at least two characters. Its colon's index (chars) and
/// the text after it.
fn fragment(text: &str, cursor: usize) -> Option<(usize, String)> {
    let before: Vec<char> = text.chars().take(cursor).collect();
    let mut i = before.len();
    while i > 0 && is_name_char(before[i - 1]) {
        i -= 1;
    }
    if i == 0 || before[i - 1] != ':' || (i >= 2 && !before[i - 2].is_whitespace()) {
        return None;
    }
    let frag: String = before[i..].iter().collect();
    (frag.chars().count() >= 2).then_some((i - 1, frag))
}

impl App {
    /// The autocomplete popup's rows, while a `:fragment` is being typed.
    pub fn completions(&mut self) -> Option<Vec<(String, Glyph)>> {
        let ed = self.typing_editor()?;
        let (_, frag) = fragment(&ed.text, ed.cursor)?;
        if self.complete.dismissed.as_deref() == Some(frag.as_str()) {
            return None;
        }
        self.complete.dismissed = None;
        if self.complete.last != frag {
            self.complete.sel = 0;
            self.complete.last = frag.clone();
        }
        let hits = self.emoji.search(&frag, ROWS);
        self.complete.sel = self.complete.sel.min(hits.len().saturating_sub(1));
        (!hits.is_empty()).then_some(hits)
    }

    /// Replaces the `:fragment` with the emoji and a space.
    fn complete_with(&mut self, name: &str) {
        let look = Lookup { users: &self.users, chans: &self.chans, emoji: &self.emoji, pictures: self.picker.is_some() };
        let Some(t) = look.emoji(name) else { return };
        let Some(ed) = self.typing_editor_mut() else { return };
        let Some((start, _)) = fragment(&ed.text, ed.cursor) else { return };
        ed.complete(start, t);
    }

    /// Keys for the `:` popup; true when it took the key.
    pub fn emoji_key(&mut self, k: KeyEvent) -> bool {
        let Some(hits) = self.completions() else { return false };
        let n = hits.len();
        let plain = !k.modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::ALT | KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Up => self.complete.sel = (self.complete.sel + n - 1) % n,
            KeyCode::Down => self.complete.sel = (self.complete.sel + 1) % n,
            KeyCode::Enter | KeyCode::Tab if plain => self.complete_with(&hits[self.complete.sel].0),
            KeyCode::Esc => self.complete.dismissed = Some(self.complete.last.clone()),
            _ => return false,
        }
        true
    }

    /// Reacts with `name`, or takes the reaction back: here at once, then in Slack.
    pub fn toggle_reaction(&mut self, chan: String, ts: String, name: String) {
        let Some(me) = self.me.as_ref().map(|m| m.user_id.clone()) else { return };
        // Both copies, the conversation's and the thread panel's: a reply lives
        // only in the panel. One we don't hold (not loaded) gets the reaction.
        let mut on = None;
        self.each_copy(&chan, &ts, |m| on = Some(toggle(&mut m.reactions, &name, &me)));
        let on = on.unwrap_or(true);
        self.dirty = true;
        self.spawn(move |s| {
            let res = react(s, &chan, &ts, &name, on);
            Event::Emoji(Ev::Reacted { chan, ts, res })
        });
    }

    pub fn on_emoji(&mut self, ev: Ev) {
        match ev {
            Ev::List(Ok(e)) => self.emoji = e,
            // The cached list stays; without it only standard emoji show.
            Ev::List(Err(_)) => {}
            Ev::Reacted { res: Ok(()), .. } => {}
            Ev::Reacted { chan, ts, res: Err(e) } => {
                self.error = Some(e);
                self.spawn(move |s| {
                    let res = reactions(s, &chan, &ts);
                    Event::Emoji(Ev::Reactions { chan, ts, res })
                });
            }
            Ev::Reactions { chan, ts, res: Ok(r) } => self.each_copy(&chan, &ts, |m| m.reactions = r.clone()),
            Ev::Reactions { res: Err(_), .. } => {}
        }
    }

    pub(crate) fn open_picker(&mut self, chan: String, ts: String, x: u16, y: u16) {
        self.react = Some(ReactPicker { chan, ts, query: String::new(), sel: 0, x, y });
        self.dirty = true;
    }

    fn pick(&mut self) {
        let Some(p) = self.react.take() else { return };
        if let Some((name, _)) = self.emoji.search(&p.query, ROWS).into_iter().nth(p.sel) {
            self.toggle_reaction(p.chan, p.ts, name);
        }
    }

    pub fn react_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let Some(p) = self.react.as_mut() else { return };
        let n = self.emoji.search(&p.query, ROWS).len().max(1);
        match k.code {
            KeyCode::Esc => self.react = None,
            KeyCode::Up | KeyCode::BackTab => p.sel = (p.sel + n - 1) % n,
            KeyCode::Down | KeyCode::Tab => p.sel = (p.sel + 1) % n,
            KeyCode::Enter => self.pick(),
            KeyCode::Backspace => {
                p.query.pop();
                p.sel = 0;
            }
            KeyCode::Char('u') if ctrl => {
                p.query.clear();
                p.sel = 0;
            }
            KeyCode::Char(c) if !ctrl && c != ':' => {
                p.query.push(c);
                p.sel = 0;
            }
            _ => {}
        }
        self.dirty = true;
    }

    /// Clicks on emoji things: the picker, the `:` popup, reaction pills,
    /// and the right click on a message that opens its menu. True when taken.
    pub fn emoji_mouse(&mut self, m: MouseEvent) -> bool {
        let at = Position::new(m.column, m.row);
        let row_at = |rows: &[(Rect, usize)]| rows.iter().find(|(r, _)| r.contains(at)).map(|(_, i)| *i);
        if self.react.is_some() {
            let hit = row_at(&self.areas.picker_rows);
            match m.kind {
                MouseEventKind::Moved => {
                    if let (Some(i), Some(p)) = (hit, self.react.as_mut()) {
                        self.dirty |= p.sel != i;
                        p.sel = i;
                    }
                }
                MouseEventKind::Down(MouseButton::Left) if hit.is_some() => {
                    if let Some(p) = self.react.as_mut() {
                        p.sel = hit.unwrap_or(0);
                    }
                    self.pick();
                }
                MouseEventKind::Down(_) if !self.areas.picker.contains(at) => {
                    self.react = None;
                    self.dirty = true;
                }
                _ => {}
            }
            return true;
        }
        // The message under the mouse shows Slack's add-reaction button.
        if m.kind == MouseEventKind::Moved {
            let thread = self.areas.thread.contains(at);
            let hover = self.areas.message_rows.iter().find(|(r, _)| r.contains(at)).map(|(_, ts)| (thread, ts.clone()));
            let hover = hover.filter(|(t, ts)| self.pane(*t).is_some_and(|(_, l)| l.iter().any(|m| m.ts == *ts)));
            if hover != self.hover_msg {
                self.hover_msg = hover;
                self.dirty = true;
            }
            return false;
        }
        let MouseEventKind::Down(button) = m.kind else { return false };
        if button == MouseButton::Left {
            if let Some(i) = row_at(&self.areas.emoji_rows) {
                if let Some((name, _)) = self.completions().and_then(|h| h.into_iter().nth(i)) {
                    self.complete_with(&name);
                }
                self.dirty = true;
                return true;
            }
            if let Some((rect, ts, name)) = self.areas.reactions.iter().find(|(r, ..)| r.contains(at)).cloned() {
                let thread = self.areas.thread.contains(at);
                let Some(chan) = self.pane(thread).map(|(c, _)| c.to_string()) else { return true };
                match name {
                    Some(name) => self.toggle_reaction(chan, ts, name),
                    // The "+" pill or the hover button: the picker right under it.
                    None => self.open_picker(chan, ts, rect.x, rect.y + 1),
                }
                return true;
            }
        }
        if button == MouseButton::Right {
            if let Some((_, ts)) = self.areas.message_rows.iter().find(|(r, _)| r.contains(at)).cloned() {
                let thread = self.areas.thread.contains(at);
                self.message_menu(thread, ts, m.column, m.row + 1);
                return true;
            }
        }
        false
    }
}
