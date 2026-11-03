//! Emoji: the workspace's custom ones, the `:` autocomplete in the message
//! box, the reaction picker and the reactions themselves.

use crate::app::{App, Event, ImgRef};
use crate::editor::Lookup;
use crate::i18n::tr;
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

/// What the picker offers before anything is typed, and ranks first after,
/// until Slack tells which ones you use; it also tops a short list up.
const FREQUENT: [&str; 6] = ["+1", "heart", "joy", "tada", "eyes", "white_check_mark"];
/// Slack's one-click reactions out of the box.
const QUICK: [&str; 3] = ["white_check_mark", "eyes", "raised_hands"];
/// How many of your most used emoji are kept.
const MOST_USED: usize = 36;
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
    /// Your uses of each emoji as Slack counts them, by its `frecency` id
    /// ("E1f44d", "Eparrot"). Only read: reacting here doesn't move them.
    #[serde(default)]
    pub uses: HashMap<String, u64>,
    #[serde(default)]
    pub quick: Quick,
}

/// Slack's one-click reactions setting (Preferences → Messages & media).
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Quick {
    /// Turned off.
    pub hidden: bool,
    /// Your three most used, rather than the three chosen.
    pub frequent: bool,
    /// The three chosen; empty ones are Slack's.
    pub own: Vec<String>,
    /// Your skin tone, 2 to 6 (0: none).
    pub tone: u8,
}

/// A search result: the name, how it's drawn and the stretches of the name
/// (byte ranges) that matched.
pub struct Found {
    pub name: String,
    pub glyph: Glyph,
    pub hl: Vec<(usize, usize)>,
}

/// The "Add Emoji" dialog: a picture's path and the emoji's name.
#[derive(Default)]
pub struct AddDialog {
    /// The path, then the name.
    pub fields: [crate::editor::Editor; 2],
    pub field: usize,
    /// The name was typed, so the file's no longer sets it.
    named: bool,
    pub saving: bool,
    pub error: Option<String>,
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
    /// Your emoji counts and one-click reactions, from Slack's preferences.
    Uses(Res<(HashMap<String, u64>, Quick)>),
    /// An emoji added from the picker reached Slack, or not.
    Added { name: String, res: Res<()> },
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

/// Slack's one-click reactions preferences.
const QUICK_PREFS: [&str; 6] = [
    "user_customized_quick_reactions_display_feature",
    "user_customized_quick_reactions_use_frequently_used_emoji",
    "user_customized_quick_reactions_has_customized",
    "user_customized_quick_reactions_emoji_1",
    "user_customized_quick_reactions_emoji_2",
    "user_customized_quick_reactions_emoji_3",
];

/// A pref as Slack keeps it: a JSON object, or that object as a string.
fn json_pref(v: &serde_json::Value) -> serde_json::Value {
    match v.as_str() {
        Some(s) => serde_json::from_str(s).unwrap_or_default(),
        None => v.clone(),
    }
}

/// A yes/no pref: true, "true" or a number above 0.
fn pref_on(v: &serde_json::Value) -> bool {
    let num = |n: f64| n > 0.0;
    v.as_bool().unwrap_or(false) || v.as_f64().is_some_and(num) || v.as_str().is_some_and(|s| s == "true" || s.parse().is_ok_and(num))
}

fn pref_set(v: &serde_json::Value) -> bool {
    !v.is_null() && v.as_str() != Some("")
}

/// Your emoji counts, from Slack's `frecency` pref as its web client reads
/// them (each emoji's own entry, "E" + code points or a custom name), and
/// how the one-click reactions are set.
pub fn uses(s: &Slack) -> Res<(HashMap<String, u64>, Quick)> {
    let names = ["frecency", "preferred_skin_tone"].into_iter().chain(QUICK_PREFS).collect::<Vec<_>>().join(",");
    let v = s.call("users.prefs.get", &[("prefs", &names)])?;
    let p = &v["prefs"];
    let mut counts = HashMap::new();
    for (k, it) in json_pref(&p["frecency"]).as_object().into_iter().flatten() {
        if !k.starts_with('E') || it["id"].as_str() != Some(k.as_str()) {
            continue;
        }
        let n = it["count"].as_f64().or_else(|| it["count"].as_str().and_then(|s| s.parse().ok())).unwrap_or(0.0);
        if n > 0.0 {
            counts.insert(k.clone(), n as u64);
        }
    }
    let [shown, frequent, customized, e1, e2, e3] = QUICK_PREFS.map(|k| &p[k]);
    let mine = !pref_set(customized) || pref_on(customized);
    let own = [e1, e2, e3]
        .map(|e| if mine { e.as_str().unwrap_or("").split(',').next().unwrap_or("").trim().trim_matches(':').to_string() } else { String::new() });
    let tone = p["preferred_skin_tone"].as_str().and_then(|t| t.parse().ok()).or(p["preferred_skin_tone"].as_u64().map(|t| t as u8));
    let quick = Quick {
        hidden: pref_set(shown) && !pref_on(shown),
        frequent: pref_on(frequent),
        own: own.to_vec(),
        tone: tone.filter(|t| (2..=6).contains(t)).unwrap_or(0),
    };
    Ok((counts, quick))
}

/// A custom emoji from a picture, as the web client's "Add emoji" does.
fn add(s: &Slack, name: &str, path: &std::path::Path) -> Res<()> {
    let mime = crate::slack::image_mime(path).ok_or_else(|| tr("Use a JPG, PNG or GIF picture.", "Use uma imagem JPG, PNG ou GIF.").to_string())?;
    s.multipart("emoji.add", &[("mode", "data"), ("name", name)], "image", path, mime).map(|_| ())
}

/// What went wrong adding an emoji, in words.
fn add_error(e: &str) -> String {
    let code = e.rsplit(": ").next().unwrap_or(e);
    match code {
        "error_name_taken" | "error_name_taken_i18n" => tr("There's already an emoji with that name.", "Já existe um emoji com esse nome."),
        "error_bad_name_i18n" | "error_bad_name" => tr("Names can only have lowercase letters, numbers, - and _.", "Nomes só podem ter letras minúsculas, números, - e _."),
        "error_missing_name" => tr("Give the emoji a name.", "Dê um nome ao emoji."),
        "error_bad_upload" | "error_bad_format" => tr("Slack couldn't read that picture. Try a PNG, JPG or GIF.", "O Slack não conseguiu ler essa imagem. Tente PNG, JPG ou GIF."),
        "error_too_big" | "resized_but_still_too_large" => tr("That picture is too large, even resized. Try a smaller one.", "Essa imagem é grande demais, mesmo redimensionada. Tente uma menor."),
        "too_many_frames" => tr("That GIF has too many frames.", "Esse GIF tem quadros demais."),
        "not_an_admin" | "no_permission" | "restricted_action" => tr("You don't have permission to add emoji here.", "Você não tem permissão para adicionar emoji aqui."),
        _ => return e.to_string(),
    }
    .to_string()
}

/// An emoji name from a file's: lowercase, spaces as `_`, only `a-z 0-9 _ -`.
fn name_of(s: &str) -> String {
    let mut out = String::new();
    for c in s.to_lowercase().chars() {
        match c {
            'a'..='z' | '0'..='9' | '_' | '-' => out.push(c),
            ':' | ' ' | '.' if !out.ends_with('_') => out.push('_'),
            _ => {}
        }
    }
    out.trim_matches(['_', '-']).to_string()
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

/// Slack's own picture of a standard emoji, the official app's: named by
/// its code points, the tone right after the base (`270c-1f3fe`).
pub fn standard_img(name: &str) -> Option<ImgRef> {
    let code: Vec<String> = standard(name)?.chars().map(|c| format!("{:x}", c as u32)).collect();
    let code = code.join("-");
    let url = format!("https://a.slack-edge.com/production-standard-emoji-assets/15.0/apple-large/{code}.png");
    Some(ImgRef { key: format!("emoji-std-{code}"), url, name: String::new(), link: None })
}

/// A standard emoji with a skin tone, as a picture: as text the tone gets
/// lost on the way (multiplexers, terminals without VS16), leaving the plain hand.
pub fn toned_img(name: &str) -> Option<ImgRef> {
    name.contains("::skin-tone-").then(|| standard_img(name)).flatten()
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

    /// The one name an emoji is counted under ("thumbsup" → "+1", a custom
    /// alias of a standard one → that one); `None` for one we can't draw.
    fn canonical(&self, name: &str) -> Option<String> {
        let base = name.split("::").next().unwrap_or(name);
        if let Some(e) = emojis::get_by_shortcode(base) {
            return Some(e.shortcode().unwrap_or(base).to_string());
        }
        if let Some(s) = self.unicode.get(base) {
            return Some(emojis::get(s).and_then(|e| e.shortcode()).unwrap_or(base).to_string());
        }
        self.custom.contains_key(base).then(|| base.to_string())
    }

    /// A `frecency` id's emoji: "E" + code points ("E1f44d", tones dropped),
    /// or "E" + a custom emoji's name.
    fn frecency_name(&self, id: &str) -> Option<String> {
        let key = id.strip_prefix('E')?;
        let hex = key.to_lowercase();
        let cps: Option<String> = hex
            .split('-')
            .map(|p| if (2..=6).contains(&p.len()) { u32::from_str_radix(p, 16).ok().and_then(char::from_u32) } else { None })
            .collect();
        let found = cps
            .map(|s| s.chars().filter(|c| !('\u{1f3fb}'..='\u{1f3ff}').contains(c)).collect::<String>())
            .and_then(|s| emojis::get(&s).or_else(|| emojis::get(&format!("{s}\u{fe0f}"))))
            .and_then(|e| e.shortcode());
        match found {
            Some(n) => Some(n.to_string()),
            None => self.canonical(key),
        }
    }

    /// Your most used emoji, most first, as Slack ranks them: by its counts,
    /// ties by name; the usual ones when Slack hasn't said, or to top it up.
    pub fn frequent(&self) -> Vec<String> {
        let mut count: HashMap<String, u64> = HashMap::new();
        for (id, n) in &self.uses {
            if let Some(name) = self.frecency_name(id) {
                let c = count.entry(name).or_default();
                *c = (*c).max(*n);
            }
        }
        let mut out: Vec<(String, u64)> = count.into_iter().collect();
        out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let mut out: Vec<String> = out.into_iter().take(MOST_USED).map(|(n, _)| n).collect();
        for f in FREQUENT {
            if out.len() >= FREQUENT.len() {
                break;
            }
            if !out.iter().any(|n| n == f) {
                out.push(f.to_string());
            }
        }
        out
    }

    /// The one-click reactions on a message's hover bar, as Slack picks
    /// them: none when turned off, your three most used, or the three chosen
    /// (else Slack's); standard ones in your skin tone.
    pub fn quick(&self) -> Vec<String> {
        let q = &self.quick;
        if q.hidden {
            return Vec::new();
        }
        let names: Vec<String> = if q.frequent {
            self.frequent().into_iter().take(3).collect()
        } else {
            QUICK
                .iter()
                .enumerate()
                .map(|(i, d)| q.own.get(i).filter(|n| !n.is_empty() && self.glyph(n).is_some()).cloned().unwrap_or_else(|| d.to_string()))
                .collect()
        };
        let toned = |n: String| match emojis::get_by_shortcode(&n) {
            Some(e) if q.tone >= 2 && e.skin_tones().is_some() => format!("{n}::skin-tone-{}", q.tone),
            _ => n,
        };
        names.into_iter().map(toned).collect()
    }

    /// Standard and custom emoji for `query`, best first, as Slack ranks
    /// them: the name itself, its start, a word's start (pepe_brain), anywhere
    /// in it, then a word of the emoji's description and, when those are few,
    /// the letters in order (thmup → thumbsup). Your most used first within
    /// each. Without a query, your most used.
    pub fn search(&self, query: &str, limit: usize) -> Vec<Found> {
        let q = query.to_lowercase();
        let q = q.trim_matches(':');
        let mine = self.frequent();
        if q.is_empty() {
            return mine.into_iter().filter_map(|n| Some(Found { glyph: self.glyph(&n)?, name: n, hl: Vec::new() })).take(limit).collect();
        }
        let usual = |name: &str| match mine.iter().position(|n| n == name) {
            Some(i) => i,
            None => FREQUENT.iter().position(|n| *n == name).map_or(999, |i| 100 + i),
        };
        // tier, name, glyph, stretches
        let mut found: HashMap<String, (u8, Glyph, Vec<(usize, usize)>)> = HashMap::new();
        let mut loose: Vec<(String, Glyph)> = Vec::new();
        let mut consider = |name: &str, glyph: Glyph, words: &str, found: &mut HashMap<String, (u8, Glyph, Vec<(usize, usize)>)>| {
            let r = rank(name, q).or_else(|| (q.len() >= 3 && words.contains(q)).then(|| (4, Vec::new())));
            match r {
                Some((tier, hl)) if found.get(name).is_none_or(|old| old.0 > tier) => {
                    found.insert(name.to_string(), (tier, glyph, hl));
                }
                Some(_) => {}
                None => loose.push((name.to_string(), glyph)),
            }
        };
        // One row per standard emoji, under its best-matching shortcode.
        for e in emojis::iter() {
            let glyph = || Glyph::Text(e.as_str().to_string());
            match e.shortcodes().filter_map(|n| Some((rank(n, q)?.0, n))).min() {
                Some((_, name)) => consider(name, glyph(), "", &mut found),
                None => {
                    let Some(first) = e.shortcode() else { continue };
                    if q.len() >= 3 && e.name().contains(q) {
                        consider(first, glyph(), e.name(), &mut found);
                    } else {
                        for n in e.shortcodes() {
                            consider(n, glyph(), "", &mut found);
                        }
                    }
                }
            }
        }
        for (name, url) in &self.custom {
            consider(name, Glyph::Image(img(name, url)), "", &mut found);
        }
        for (name, s) in &self.unicode {
            consider(name, Glyph::Text(s.clone()), "", &mut found);
        }
        let mut out: Vec<(u8, usize, Found)> =
            found.into_iter().map(|(name, (tier, glyph, hl))| (tier, usual(&name), Found { name, glyph, hl })).collect();
        out.sort_by(|a, b| (a.0, a.1, a.2.name.len(), &a.2.name).cmp(&(b.0, b.1, b.2.name.len(), &b.2.name)));
        out.truncate(limit);
        let mut out: Vec<Found> = out.into_iter().map(|(_, _, f)| f).collect();
        // Few hits: the letters in order, tightest first, one row per glyph.
        if out.len() < limit {
            let key = |g: &Glyph| match g {
                Glyph::Text(s) => s.clone(),
                Glyph::Image(i) => i.key.clone(),
            };
            let mut seen: std::collections::HashSet<String> = out.iter().map(|f| key(&f.glyph)).collect();
            let mut fz: Vec<(usize, Found)> = loose
                .into_iter()
                .filter_map(|(name, glyph)| {
                    let (score, hl) = fuzzy(&name, q)?;
                    Some((score, Found { name, glyph, hl }))
                })
                .collect();
            fz.sort_by(|a, b| (a.0, &a.1.name).cmp(&(b.0, &b.1.name)));
            for (_, f) in fz {
                if out.len() >= limit {
                    break;
                }
                if seen.insert(key(&f.glyph)) {
                    out.push(f);
                }
            }
        }
        out
    }
}

/// Where in `name` the query matched, as a tier: 0 the name itself, 1 its
/// start, 2 a word's start (after `_`, `-` or `+`), 3 anywhere.
fn rank(name: &str, q: &str) -> Option<(u8, Vec<(usize, usize)>)> {
    let whole = vec![(0, q.len())];
    if name == q {
        return Some((0, whole));
    }
    if name.starts_with(q) {
        return Some((1, whole));
    }
    let mut first = None;
    for (i, _) in name.match_indices(q) {
        if i > 0 && name[..i].ends_with(['_', '-', '+']) {
            return Some((2, vec![(i, i + q.len())]));
        }
        first.get_or_insert(i);
    }
    first.map(|i| (3, vec![(i, i + q.len())]))
}

/// The letters of `q` in `name`, in order, from a word's start and in few
/// stretches: a score (lower is tighter) and the stretches.
fn fuzzy(name: &str, q: &str) -> Option<(usize, Vec<(usize, usize)>)> {
    let qs: Vec<char> = q.chars().collect();
    if qs.len() < 2 {
        return None;
    }
    let mut hl: Vec<(usize, usize)> = Vec::new();
    let mut j = 0;
    for (i, c) in name.char_indices() {
        if j == qs.len() {
            break;
        }
        if c != qs[j] {
            continue;
        }
        match hl.last_mut() {
            Some(last) if last.1 == i => last.1 = i + c.len_utf8(),
            _ => hl.push((i, i + c.len_utf8())),
        }
        j += 1;
    }
    if j < qs.len() || hl.len() > 2.max(qs.len().div_ceil(2)) {
        return None;
    }
    let s0 = hl[0].0;
    if s0 > 0 && !name[..s0].ends_with(['_', '-', '+']) {
        return None;
    }
    Some((hl.len() * 100 + s0 * 4 + name.len(), hl))
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
    pub fn completions(&mut self) -> Option<Vec<Found>> {
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
            KeyCode::Enter | KeyCode::Tab if plain => self.complete_with(&hits[self.complete.sel].name),
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
            // Your counts come apart, and stay.
            Ev::List(Ok(e)) => {
                let old = std::mem::take(&mut self.emoji);
                self.emoji = Emoji { uses: old.uses, quick: old.quick, ..e };
            }
            // The cached list stays; without it only standard emoji show.
            Ev::List(Err(_)) => {}
            Ev::Uses(Ok((uses, quick))) => (self.emoji.uses, self.emoji.quick) = (uses, quick),
            // The cached counts stay, or the usual ones.
            Ev::Uses(Err(_)) => {}
            Ev::Added { name, res: Ok(()) } => {
                self.add_emoji = None;
                self.brief_copied(trf!("emoji :{name}: added", "emoji :{name}: adicionado"));
                self.spawn(|s| Event::Emoji(Ev::List(fetch(s))));
            }
            Ev::Added { res: Err(e), .. } => {
                if let Some(d) = self.add_emoji.as_mut() {
                    d.saving = false;
                    d.error = Some(add_error(&e));
                }
            }
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
        if let Some(Found { name, .. }) = self.emoji.search(&p.query, ROWS).into_iter().nth(p.sel) {
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
            KeyCode::Char('e') if ctrl => self.open_add_emoji(),
            KeyCode::Char(c) if !ctrl && c != ':' => {
                p.query.push(c);
                p.sel = 0;
            }
            _ => {}
        }
        self.dirty = true;
    }

    /// The emoji under the mouse in a message, or a one-click reaction's:
    /// where it is, its name and what a click copies.
    pub(crate) fn emoji_at(&self, at: Position) -> Option<(Rect, String, String)> {
        let quick = self.areas.quick.iter().find(|(r, ..)| r.contains(at)).map(|(r, _, n)| (*r, n.clone(), String::new()));
        quick.or_else(|| {
            self.areas.buttons.iter().find_map(|(r, t)| match t {
                crate::blocks::Target::Emoji(name, copy) if r.contains(at) => Some((*r, name.clone(), copy.clone())),
                _ => None,
            })
        })
    }

    /// "Add Emoji", from the picker.
    fn open_add_emoji(&mut self) {
        self.react = None;
        self.add_emoji = Some(AddDialog::default());
        self.dirty = true;
    }

    fn submit_add_emoji(&mut self) {
        let Some(d) = self.add_emoji.as_mut() else { return };
        if d.saving {
            return;
        }
        let path = crate::account::photo_path(&d.fields[0].text);
        let name = name_of(d.fields[1].text.trim().trim_matches(':'));
        d.error = if d.fields[0].text.trim().is_empty() {
            Some(tr("Choose a picture first.", "Escolha uma imagem primeiro.").into())
        } else if crate::slack::image_mime(&path).is_none() {
            Some(tr("Use a JPG, PNG or GIF picture.", "Use uma imagem JPG, PNG ou GIF.").into())
        } else if !path.is_file() {
            Some(tr("There's no file at that path.", "Não há arquivo nesse caminho.").into())
        } else if name.is_empty() {
            Some(tr("Give the emoji a name.", "Dê um nome ao emoji.").into())
        } else if self.emoji.glyph(&name).is_some() {
            Some(tr("There's already an emoji with that name.", "Já existe um emoji com esse nome.").into())
        } else {
            None
        };
        if d.error.is_some() {
            return;
        }
        d.fields[1].set(name.clone());
        d.saving = true;
        self.spawn(move |s| {
            let res = add(s, &name, &path);
            Event::Emoji(Ev::Added { name, res })
        });
    }

    /// The name follows the file's until one is typed.
    fn follow_file_name(d: &mut AddDialog) {
        if d.named {
            return;
        }
        let path = crate::account::photo_path(&d.fields[0].text);
        let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        d.fields[1].set(name_of(&stem));
    }

    pub(crate) fn add_emoji_paste(&mut self, s: &str) {
        let line = s.lines().next().unwrap_or("").trim();
        let Some(d) = self.add_emoji.as_mut() else { return };
        d.fields[d.field].insert(line);
        d.error = None;
        if d.field == 0 {
            Self::follow_file_name(d);
        } else {
            d.named = true;
        }
        self.dirty = true;
    }

    pub(crate) fn add_emoji_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        self.dirty = true;
        let Some(d) = self.add_emoji.as_mut() else { return };
        match k.code {
            KeyCode::Esc => return self.add_emoji = None,
            KeyCode::Enter => return self.submit_add_emoji(),
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Down | KeyCode::Up => return d.field = 1 - d.field,
            _ if d.saving => return,
            _ => {}
        }
        let e = &mut d.fields[d.field];
        let before = e.text.clone();
        let word = ctrl || k.modifiers.contains(KeyModifiers::ALT);
        match k.code {
            KeyCode::Left if ctrl => e.word_left(),
            KeyCode::Right if ctrl => e.word_right(),
            KeyCode::Left => e.left(),
            KeyCode::Right => e.right(),
            KeyCode::Home => e.home(),
            KeyCode::Char('a') if ctrl => e.home(),
            KeyCode::End => e.end(),
            KeyCode::Char('e') if ctrl => e.end(),
            KeyCode::Backspace if word => e.delete_word(),
            KeyCode::Char('w') if ctrl => e.delete_word(),
            KeyCode::Backspace => e.backspace(),
            KeyCode::Delete => e.delete(),
            KeyCode::Char('u') if ctrl => e.set(String::new()),
            KeyCode::Char(c) if !ctrl => e.insert(c.encode_utf8(&mut [0; 4])),
            _ => {}
        }
        if e.text != before {
            d.error = None;
            if d.field == 0 {
                Self::follow_file_name(d);
            } else {
                d.named = !d.fields[1].text.is_empty();
            }
        }
    }

    pub(crate) fn add_emoji_mouse(&mut self, m: MouseEvent) {
        if m.kind != MouseEventKind::Down(MouseButton::Left) {
            return;
        }
        let at = Position::new(m.column, m.row);
        let a = &self.areas;
        let (save, cancel) = (a.add_save.contains(at), a.add_cancel.contains(at));
        let field = a.add_fields.iter().find(|(r, _)| r.contains(at)).map(|(_, i)| *i);
        self.dirty = true;
        if save {
            self.submit_add_emoji();
        } else if cancel {
            self.add_emoji = None;
        } else if let (Some(i), Some(d)) = (field, self.add_emoji.as_mut()) {
            d.field = i;
            d.fields[i].end();
        }
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
                MouseEventKind::Down(MouseButton::Left) if self.areas.picker_add.contains(at) => self.open_add_emoji(),
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
            // A reaction under the mouse: who reacted, over it.
            let pill = self.areas.reactions.iter().any(|(r, _, n)| n.is_some() && r.contains(at)).then_some(at);
            if pill.is_some() || self.hover_pill.is_some() {
                self.hover_pill = pill;
                self.dirty = true;
            }
            // An emoji in a message or a one-click reaction: its :name:.
            let tip = self.emoji_at(at).is_some().then_some(at);
            if tip.is_some() || self.hover_emoji.is_some() {
                self.hover_emoji = tip;
                self.dirty = true;
            }
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
                if let Some(Found { name, .. }) = self.completions().and_then(|h| h.into_iter().nth(i)) {
                    self.complete_with(&name);
                }
                self.dirty = true;
                return true;
            }
            if let Some((_, ts, name)) = self.areas.quick.iter().find(|(r, ..)| r.contains(at)).cloned() {
                let thread = self.areas.thread.contains(at);
                if let Some(chan) = self.pane(thread).map(|(c, _)| c.to_string()) {
                    self.toggle_reaction(chan, ts, name);
                }
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
