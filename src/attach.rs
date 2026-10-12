//! Pictures pasted into a message box, sent with the message like Slack's
//! attachments. Terminals don't forward pasted images, so the clipboard is
//! read directly: wl-paste or xclip on Linux, arboard elsewhere.

use crate::app::{App, Event, Focus, Img, ImgRef};
use crate::editor::Editor;
use crate::i18n::tr;
use crate::slack::{escape, Res, Slack};
use crate::ui::{dim, draw_image};
use image::DynamicImage;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Block, BorderType, Paragraph};
use ratatui::Frame;
use std::sync::Arc;
use std::time::Duration;
use unicode_width::UnicodeWidthStr;

/// The card: a few rows of picture in a rounded border.
pub const CARD_ROWS: u16 = 6;
const PIC_ROWS: u16 = CARD_ROWS - 2;
/// Slack takes up to 10 files per message.
const MAX: usize = 10;

/// Where a box sends: the conversation, or a thread in it.
#[derive(Clone, PartialEq)]
pub struct Target {
    pub chan: String,
    pub thread: Option<String>,
}

pub struct Attachment {
    /// Also its key in `App::images`, where the preview lives.
    pub key: String,
    pub name: String,
    pub bytes: Arc<Vec<u8>>,
    pub w: u32,
    pub h: u32,
    /// On its way with the message: no ✕, and kept until Slack has it.
    pub sending: bool,
}

/// Each box keeps its own pictures, so they wait in their conversation or
/// thread like a draft.
#[derive(Default)]
pub struct Attach {
    items: Vec<(Target, Attachment)>,
    /// Each card's ✕ on screen and the picture it removes.
    pub close: Vec<(Rect, String)>,
    reading: bool,
    next: u64,
    /// The status line set while uploading, cleared when it ends.
    status: Option<String>,
}

impl Attach {
    fn of<'a>(&'a self, to: &'a Target) -> impl Iterator<Item = &'a Attachment> + 'a {
        self.items.iter().filter(move |(t, _)| t == to).map(|(_, a)| a)
    }
}

pub enum Clip {
    Image { bytes: Vec<u8>, ext: &'static str, thumb: DynamicImage, w: u32, h: u32 },
    Text(String),
    Empty,
}

pub enum Ev {
    Clip { to: Target, key: String, res: Res<Clip> },
    Sent { to: Target, keys: Vec<String>, text: String, res: Res<()> },
}

impl Slack {
    /// Slack's upload flow: an address per file, the bytes there, then one
    /// message holding all of them with the text as its comment.
    pub fn upload(&self, chan: &str, thread: Option<&str>, text: &str, files: &[(String, Arc<Vec<u8>>)]) -> Res<()> {
        // The upload address is presigned: no credentials, but room for big files.
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(20))
            .timeout_read(Duration::from_secs(60))
            .timeout_write(Duration::from_secs(120))
            .user_agent(concat!("slick/", env!("CARGO_PKG_VERSION")))
            .build();
        let mut done = Vec::new();
        for (name, bytes) in files {
            let len = bytes.len().to_string();
            let v = self.call("files.getUploadURLExternal", &[("filename", name.as_str()), ("length", len.as_str())])?;
            let (Some(url), Some(id)) = (v["upload_url"].as_str(), v["file_id"].as_str()) else {
                return Err("files.getUploadURLExternal: no upload_url".into());
            };
            agent
                .post(url)
                .set("Content-Type", "application/octet-stream")
                .send_bytes(bytes)
                .map_err(|e| trf!("upload {name}: {e}", "envio de {name}: {e}"))?;
            done.push(serde_json::json!({ "id": id, "title": name }));
        }
        let files = serde_json::Value::Array(done).to_string();
        let comment = escape(text);
        let mut p = vec![("files", files.as_str()), ("channel_id", chan)];
        if !comment.is_empty() {
            p.push(("initial_comment", comment.as_str()));
        }
        if let Some(ts) = thread {
            p.push(("thread_ts", ts));
        }
        self.call("files.completeUploadExternal", &p).map(|_| ())
    }
}

impl App {
    /// What the conversation's box (or the thread's) sends to.
    pub(crate) fn target(&self, thread: bool) -> Option<Target> {
        if thread {
            let t = self.thread.as_ref()?;
            Some(Target { chan: t.chan.clone(), thread: Some(t.ts.clone()) })
        } else {
            Some(Target { chan: self.current.clone()?, thread: None })
        }
    }

    fn editor_for(&mut self, to: &Target) -> Option<&mut Editor> {
        match &to.thread {
            None => (self.current.as_deref() == Some(to.chan.as_str())).then_some(&mut self.editor),
            Some(ts) => self.thread.as_mut().filter(|t| t.chan == to.chan && t.ts == *ts).map(|t| &mut t.editor),
        }
    }

    /// Ctrl+V, or a paste with nothing in it: a picture on the clipboard is
    /// attached, text is typed in. Read off the UI thread.
    pub(crate) fn paste_clipboard(&mut self, thread: bool) {
        let Some(to) = self.target(thread) else { return };
        if std::mem::replace(&mut self.attach.reading, true) {
            return;
        }
        self.attach.next += 1;
        let key = format!("paste-{}-{}", std::process::id(), self.attach.next);
        self.spawn(move |_| Event::Attach(Ev::Clip { to, key, res: read_clipboard() }));
    }

    /// Backspace in an empty box: the last picture goes. True when one did.
    pub(crate) fn drop_last(&mut self, thread: bool) -> bool {
        let Some(to) = self.target(thread) else { return false };
        let Some(key) = self.attach.of(&to).filter(|a| !a.sending).last().map(|a| a.key.clone()) else { return false };
        self.forget(&key);
        true
    }

    fn forget(&mut self, key: &str) {
        self.attach.items.retain(|(_, a)| a.key != key);
        self.images.remove(key);
        self.protocols.retain(|(k, _, _), _| k != key);
    }

    /// Enter with pictures attached: they go up with the text as their
    /// comment. False when there are none, for the plain send.
    pub(crate) fn send_files(&mut self, thread: bool) -> bool {
        let Some(to) = self.target(thread) else { return false };
        let (mut keys, mut files) = (Vec::new(), Vec::new());
        for (_, a) in self.attach.items.iter_mut().filter(|(t, a)| *t == to && !a.sending) {
            a.sending = true;
            keys.push(a.key.clone());
            files.push((a.name.clone(), a.bytes.clone()));
        }
        if files.is_empty() {
            // Still going: Enter waits for them instead of sending the text alone.
            return self.attach.of(&to).next().is_some();
        }
        let text = self.editor_for(&to).map(|e| e.take().trim().to_string()).unwrap_or_default();
        match self.thread.as_mut().filter(|_| thread) {
            Some(t) => t.scroll = 0,
            None => self.to_present(),
        }
        let status = match files.len() {
            1 => tr("Sending the image…", "Enviando a imagem…").to_string(),
            n => trf!("Sending {n} images…", "Enviando {n} imagens…"),
        };
        self.status = status.clone();
        self.attach.status = Some(status);
        self.spawn(move |s| {
            let res = s.upload(&to.chan, to.thread.as_deref(), &text, &files);
            Event::Attach(Ev::Sent { to, keys, text, res })
        });
        true
    }

    pub(crate) fn on_attach(&mut self, ev: Ev) {
        match ev {
            Ev::Clip { to, key, res } => {
                self.attach.reading = false;
                match res {
                    Ok(Clip::Empty) => self.brief(tr("Nothing to paste", "Nada para colar").into(), 3),
                    Ok(Clip::Text(s)) => {
                        if let Some(e) = self.editor_for(&to) {
                            e.insert(&s);
                        }
                    }
                    Ok(Clip::Image { .. }) if self.attach.of(&to).count() >= MAX => {
                        self.error = Some(trf!("Up to {MAX} images per message", "No máximo {MAX} imagens por mensagem"));
                    }
                    Ok(Clip::Image { bytes, ext, thumb, w, h }) => {
                        self.images.insert(key.clone(), Img::Ready(thumb));
                        let name = format!("image.{ext}");
                        self.attach.items.push((to, Attachment { key, name, bytes: Arc::new(bytes), w, h, sending: false }));
                    }
                    Err(e) => self.error = Some(e),
                }
            }
            Ev::Sent { to, keys, text, res } => {
                if self.attach.status.take().is_some_and(|s| s == self.status) {
                    self.status.clear();
                }
                match res {
                    Ok(()) => {
                        keys.iter().for_each(|k| self.forget(k));
                        // The message shows once Slack has it; don't wait for the next poll.
                        match &to.thread {
                            Some(_) => self.refresh_thread(),
                            None if self.current.as_deref() == Some(to.chan.as_str()) => {
                                self.fetch(to.chan.clone(), crate::app::Fetch::Poll)
                            }
                            None => {}
                        }
                    }
                    Err(e) => {
                        for (_, a) in self.attach.items.iter_mut().filter(|(_, a)| keys.contains(&a.key)) {
                            a.sending = false;
                        }
                        self.error = Some(e);
                        let look = crate::editor::Lookup { users: &self.users, chans: &self.chans, emoji: &self.emoji, pictures: self.picker.is_some() };
                        let ed = match &to.thread {
                            None => (self.current.as_deref() == Some(to.chan.as_str())).then_some(&mut self.editor),
                            Some(ts) => self.thread.as_mut().filter(|t| t.chan == to.chan && t.ts == *ts).map(|t| &mut t.editor),
                        };
                        if let Some(ed) = ed.filter(|e| e.is_empty()) {
                            ed.load(&text, false, &look);
                        }
                    }
                }
            }
        }
    }

    /// A click on a card's ✕. True when handled.
    pub(crate) fn attach_mouse(&mut self, m: crossterm::event::MouseEvent) -> bool {
        use crossterm::event::{MouseButton, MouseEventKind};
        if m.kind != MouseEventKind::Down(MouseButton::Left) {
            return false;
        }
        let at = ratatui::layout::Position::new(m.column, m.row);
        let Some(key) = self.attach.close.iter().find(|(r, _)| r.contains(at)).map(|(_, k)| k.clone()) else { return false };
        self.forget(&key);
        self.dirty = true;
        true
    }
}

/// The rows a box grows by for its cards.
pub fn rows(app: &App, thread: bool) -> u16 {
    let any = app.target(thread).is_some_and(|to| app.attach.of(&to).next().is_some());
    if any { CARD_ROWS } else { 0 }
}

/// A box's inside: the text on top and the cards under it, as in Slack.
pub fn split(app: &App, inner: Rect, thread: bool) -> (Rect, Rect) {
    let h = rows(app, thread);
    if h == 0 || inner.height <= h {
        return (inner, Rect::default());
    }
    (Rect { height: inner.height - h, ..inner }, Rect::new(inner.x, inner.bottom() - h, inner.width, h))
}

/// The status line's keys while a box holds pictures.
pub fn hint(app: &App) -> Option<String> {
    let thread = match app.focus {
        Focus::Input => false,
        Focus::Thread => true,
        _ => return None,
    };
    (rows(app, thread) > 0).then(|| {
        tr(
            "Enter send · Backspace remove last image · Ctrl+V paste another · Esc list",
            "Enter enviar · Backspace remover última imagem · Ctrl+V colar outra · Esc lista",
        )
        .to_string()
    })
}

/// The cards in a row, each a thumbnail with a ✕ on its corner. The
/// conversation's box draws first and starts the frame's ✕ list.
pub fn draw(f: &mut Frame, app: &mut App, area: Rect, thread: bool) {
    if !thread {
        app.attach.close.clear();
    }
    let Some(to) = app.target(thread) else { return };
    if area.height < CARD_ROWS {
        return;
    }
    let cards: Vec<(String, String, u32, u32, bool)> =
        app.attach.of(&to).map(|a| (a.key.clone(), a.name.clone(), a.w, a.h, a.sending)).collect();
    let (fw, fh) = app.picker.as_ref().map(|p| p.font_size()).map(|s| (s.width as u64, s.height as u64)).unwrap_or((1, 2));
    let mut x = area.x;
    for (i, (key, name, w, h, sending)) in cards.iter().enumerate() {
        let cols = (*w as u64 * fh * PIC_ROWS as u64 / ((*h).max(1) as u64 * fw.max(1))).clamp(6, 28) as u16;
        let card = Rect::new(x, area.y, cols + 2, CARD_ROWS);
        if card.right() > area.right() {
            let more = format!("+{}", cards.len() - i);
            if x + more.width() as u16 <= area.right() {
                f.render_widget(Paragraph::new(more).style(dim()), Rect::new(x, area.y + CARD_ROWS / 2, area.right() - x, 1));
            }
            break;
        }
        let mut block = Block::bordered().border_type(BorderType::Rounded).border_style(dim());
        if *sending {
            block = block.title_bottom(Line::from(tr(" sending… ", " enviando… ")).style(dim().italic()));
        }
        let pic = block.inner(card);
        f.render_widget(block, card);
        if app.picker.is_some() {
            draw_image(f, app, &ImgRef { key: key.clone(), url: String::new(), name: String::new(), link: None }, pic);
        } else {
            f.render_widget(Paragraph::new(format!("🖼 {name}")).style(dim()), pic);
        }
        if !*sending {
            let close = Rect::new(card.right() - 4, card.y, 3, 1);
            f.render_widget(Paragraph::new(" ✕ ").style(Style::new().bold()), close);
            app.attach.close.push((close, key.clone()));
        }
        x = card.right() + 1;
    }
}

// ---- the clipboard ---------------------------------------------------------

/// Image types Slack shows inline and we can preview, the usual first.
const TYPES: [(&str, &str); 4] = [("image/png", "png"), ("image/jpeg", "jpg"), ("image/gif", "gif"), ("image/webp", "webp")];

fn read_clipboard() -> Res<Clip> {
    let clip = raw_clipboard()?;
    let Clip::Image { bytes, ext, .. } = clip else { return Ok(clip) };
    let img = image::load_from_memory(&bytes).map_err(|e| trf!("pasted image: {e}", "imagem colada: {e}"))?;
    let (w, h) = (img.width(), img.height());
    // The card is a few rows tall; a 4K screenshot would only cost memory.
    let thumb = img.thumbnail(480, 480);
    Ok(Clip::Image { bytes, ext, thumb, w, h })
}

/// wl-paste on Wayland, else xclip: shelling out keeps X11 and Wayland
/// libraries out of the build.
#[cfg(target_os = "linux")]
fn raw_clipboard() -> Res<Clip> {
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
    let (tool, package) = if wayland { ("wl-paste", "wl-clipboard") } else { ("xclip", "xclip") };
    let get = |args: &[&str]| run(tool, package, args);
    let types = if wayland { get(&["--list-types"])? } else { get(&["-selection", "clipboard", "-t", "TARGETS", "-o"])? };
    // Nothing copied: both tools just fail.
    let Some(types) = types else { return Ok(Clip::Empty) };
    let types = String::from_utf8_lossy(&types);
    let has = |t: &str| types.lines().any(|l| l.trim() == t);
    if let Some((mime, ext)) = TYPES.iter().copied().find(|(m, _)| has(m)) {
        let args: &[&str] = if wayland { &["--type", mime] } else { &["-selection", "clipboard", "-t", mime, "-o"] };
        return match get(args)? {
            Some(bytes) if !bytes.is_empty() => Ok(Clip::Image { bytes, ext, thumb: DynamicImage::new_rgba8(0, 0), w: 0, h: 0 }),
            _ => Ok(Clip::Empty),
        };
    }
    let text = types.lines().any(|l| l.starts_with("text/") || matches!(l.trim(), "UTF8_STRING" | "STRING" | "TEXT"));
    if !text {
        return Ok(Clip::Empty);
    }
    let args: &[&str] = if wayland { &["--no-newline", "--type", "text"] } else { &["-selection", "clipboard", "-t", "UTF8_STRING", "-o"] };
    Ok(get(args)?.map(|b| String::from_utf8_lossy(&b).into_owned()).filter(|s| !s.is_empty()).map_or(Clip::Empty, Clip::Text))
}

/// What the tool printed, `None` when it failed (an empty clipboard). A tool
/// that hangs, waiting on an app that owns the clipboard, is given up on.
#[cfg(target_os = "linux")]
fn run(cmd: &str, package: &str, args: &[&str]) -> Res<Option<Vec<u8>>> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let spawned = Command::new(cmd).args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn();
    let mut child = match spawned {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(trf!("pasting images needs {cmd}: install {package}", "colar imagens precisa do {cmd}: instale o {package}"));
        }
        Err(e) => return Err(format!("{cmd}: {e}")),
    };
    let mut out = child.stdout.take().ok_or_else(|| format!("{cmd}: no output"))?;
    let reader = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = out.read_to_end(&mut b);
        b
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status.success().then(|| reader.join().unwrap_or_default())),
            Ok(None) if std::time::Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(trf!("{cmd} didn't answer", "o {cmd} não respondeu"));
            }
        }
    }
}

/// arboard hands over raw pixels: they go up as a PNG.
#[cfg(not(target_os = "linux"))]
fn raw_clipboard() -> Res<Clip> {
    let mut cb = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    if let Ok(img) = cb.get_image() {
        let rgba = image::RgbaImage::from_raw(img.width as u32, img.height as u32, img.bytes.into_owned())
            .ok_or_else(|| tr("pasted image: unreadable", "imagem colada: ilegível").to_string())?;
        let mut bytes = Vec::new();
        DynamicImage::ImageRgba8(rgba)
            .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
            .map_err(|e| e.to_string())?;
        return Ok(Clip::Image { bytes, ext: TYPES[0].1, thumb: DynamicImage::new_rgba8(0, 0), w: 0, h: 0 });
    }
    Ok(cb.get_text().ok().filter(|s| !s.is_empty()).map_or(Clip::Empty, Clip::Text))
}
