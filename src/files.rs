//! Shared files: the card a message shows for them and saving them to the
//! downloads folder.

use crate::app::ImgRef;
use crate::cache;
use crate::i18n::tr;
use crate::slack::{File, Res, Slack};
use crate::ui::truncate;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthStr;

/// A download, keyed by `Job::key`.
pub enum State {
    Saving(Arc<Progress>),
    Saved(PathBuf),
}

/// How far a download got: the bytes so far and its size (0 while not known).
#[derive(Default)]
pub struct Progress {
    pub done: AtomicU64,
    pub total: AtomicU64,
}

impl Progress {
    /// From 0 to 1, when the size is known.
    pub fn fraction(&self) -> Option<f64> {
        let (done, total) = (self.done.load(Ordering::Relaxed), self.total.load(Ordering::Relaxed));
        (total > 0).then(|| (done as f64 / total as f64).min(1.0))
    }

    /// `42%`, else the bytes so far.
    pub fn label(&self) -> String {
        match self.fraction() {
            Some(f) => format!("{}%", (f * 100.0).floor() as u64),
            None => size_label(self.done.load(Ordering::Relaxed)),
        }
    }
}

/// Wakes the screen this often at most while bytes arrive.
const PROGRESS_EVERY: Duration = Duration::from_millis(200);

/// Counts what goes through into `Progress`, calling `tick` now and then.
struct Counting<'a> {
    out: &'a mut std::fs::File,
    progress: &'a Progress,
    tick: &'a dyn Fn(),
    last: Instant,
}

impl Write for Counting<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.out.write(buf)?;
        self.progress.done.fetch_add(n as u64, Ordering::Relaxed);
        if self.last.elapsed() >= PROGRESS_EVERY {
            self.last = Instant::now();
            (self.tick)();
        }
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.out.flush()
    }
}

/// Something to save: a shared file or the image in the viewer.
#[derive(Clone)]
pub struct Job {
    pub key: String,
    pub url: String,
    pub name: String,
    /// Only an HTML file may come back as HTML; otherwise it is Slack's sign-in page.
    pub html_ok: bool,
    /// A copy already on disk, saved without downloading it again.
    pub cached: Option<PathBuf>,
}

impl Job {
    pub fn file(f: &File) -> Option<Self> {
        Some(Self {
            key: f.id.clone(),
            url: f.download.clone()?,
            name: f.name.clone(),
            html_ok: f.mimetype == "text/html",
            cached: None,
        })
    }

    /// The viewer shows the original, so the cached copy is the file itself.
    pub fn image(r: &ImgRef) -> Self {
        let ext = r.key.rsplit_once('.').map(|(_, e)| e).filter(|e| e.len() <= 5);
        let name = match ext {
            Some(e) if !r.name.contains('.') => format!("{}.{e}", if r.name.is_empty() { tr("image", "imagem") } else { &r.name }),
            _ => r.name.clone(),
        };
        // `Slack::download` stops at 20 MB: a copy that size may be cut short.
        let cached = Some(r.file()).filter(|p| p.metadata().is_ok_and(|m| m.len() < 20 << 20));
        Self { key: r.key.clone(), url: r.url.clone(), name, html_ok: false, cached }
    }
}

/// What a click on a card does.
#[derive(Clone)]
pub enum Click {
    Save(Job),
    /// Downloads to the cache and hands it to the system's default app.
    OpenWith(Job),
    Open(PathBuf),
    Reveal(PathBuf),
    /// The card itself: a menu with what can be done with the file.
    Menu(Option<Job>, Option<PathBuf>),
}

/// A clickable stretch of a card line, in message-list coordinates.
pub struct Hit {
    pub line: usize,
    pub x: u16,
    pub w: u16,
    pub click: Click,
}

/// `$XDG_DOWNLOAD_DIR`, as `xdg-user-dirs` records it, else `~/Downloads`.
pub fn downloads_dir() -> PathBuf {
    let home = crate::config::home().to_string_lossy().into_owned();
    let config = std::env::var("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|_| Path::new(&home).join(".config"));
    let from_file = std::fs::read_to_string(config.join("user-dirs.dirs")).ok().and_then(|text| {
        text.lines().find_map(|l| {
            let v = l.trim().strip_prefix("XDG_DOWNLOAD_DIR=")?.trim_matches('"');
            Some(v.replace("$HOME", &home))
        })
    });
    std::env::var("XDG_DOWNLOAD_DIR")
        .ok()
        .or(from_file)
        .filter(|d| !d.is_empty() && d.trim_end_matches('/') != home.trim_end_matches('/'))
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(&home).join("Downloads"))
}

/// `~/Downloads/a.pdf`, for the status line.
pub fn pretty(p: &Path) -> String {
    let home = crate::config::home();
    match p.strip_prefix(&home) {
        Ok(rest) if !home.as_os_str().is_empty() => format!("~{}{}", std::path::MAIN_SEPARATOR, rest.display()),
        _ => p.display().to_string(),
    }
}

/// The name Slack gave it, minus anything that would leave the folder.
fn safe_name(name: &str) -> String {
    // Windows also refuses \ : * ? " < > | in names.
    let bad = |c: char| c == '/' || c.is_control() || (cfg!(windows) && "\\:*?\"<>|".contains(c));
    let n: String = name.chars().map(|c| if bad(c) { '_' } else { c }).collect();
    let n = n.trim();
    if n.is_empty() || n == "." || n == ".." { tr("file", "arquivo").into() } else { n.to_string() }
}

/// `a.pdf`, else `a (1).pdf`, `a (2).pdf`…: the first name neither saved nor
/// being saved, claimed by creating its `.part` file.
fn claim(dir: &Path, name: &str) -> Res<(PathBuf, PathBuf, std::fs::File)> {
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s, format!(".{e}")),
        _ => (name, String::new()),
    };
    for n in 0.. {
        let path = dir.join(if n == 0 { name.to_string() } else { format!("{stem} ({n}){ext}") });
        let mut part = path.clone().into_os_string();
        part.push(".part");
        let part = PathBuf::from(part);
        if path.exists() {
            continue;
        }
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&part) {
            Ok(f) => return Ok((path, part, f)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
    unreachable!()
}

/// Saves into the downloads folder without overwriting anything; the file
/// only gets its name once complete. Each try writes a `.part` of its own,
/// so a retry never lands after what an earlier one left. `tick` is
/// called as `progress` moves.
pub fn save(s: &Slack, job: &Job, progress: &Progress, tick: &dyn Fn()) -> Res<PathBuf> {
    let dir = downloads_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let (path, part, mut out) = claim(&dir, &safe_name(&job.name))?;
    let mut counting = Counting { out: &mut out, progress, tick, last: Instant::now() };
    let res = match &job.cached {
        Some(c) => std::fs::File::open(c)
            .and_then(|mut f| {
                progress.total.store(f.metadata()?.len(), Ordering::Relaxed);
                std::io::copy(&mut f, &mut counting)
            })
            .map_err(|e| e.to_string()),
        None => s.download_to(&job.url, &mut counting, job.html_ok, &|n| progress.total.store(n, Ordering::Relaxed)),
    };
    drop(out);
    match res.and_then(|_| std::fs::rename(&part, &path).map_err(|e| e.to_string())) {
        Ok(()) => Ok(path),
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            Err(e)
        }
    }
}

/// A copy in the cache, for opening with another app; downloaded once.
pub fn cached(s: &Slack, job: &Job) -> Res<PathBuf> {
    let path = cache::dir().join("files").join(&job.key).join(safe_name(&job.name));
    if path.exists() {
        return Ok(path);
    }
    let dir = path.parent().unwrap();
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    // One `.part` per try: two at once never write into the same one.
    static TRY: AtomicU64 = AtomicU64::new(0);
    let part = dir.join(format!(".part{}-{}", std::process::id(), TRY.fetch_add(1, Ordering::Relaxed)));
    let mut out = std::fs::File::create(&part).map_err(|e| e.to_string())?;
    let res = s.download_to(&job.url, &mut out, job.html_ok, &|_| {});
    drop(out);
    match res.and_then(|_| std::fs::rename(&part, &path).map_err(|e| e.to_string())) {
        Ok(()) => Ok(path),
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            Err(e)
        }
    }
}

/// A progress bar `cells` wide, in eighths of a cell.
pub fn bar(f: f64, cells: usize) -> String {
    const PARTS: [&str; 8] = [" ", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];
    let eighths = (f.clamp(0.0, 1.0) * (cells * 8) as f64).round() as usize;
    let mut s = "█".repeat(eighths / 8);
    if eighths / 8 < cells {
        s.push_str(PARTS[eighths % 8]);
        s.push_str(&"░".repeat(cells - eighths / 8 - 1));
    }
    s
}

/// The file manager with the file picked in it (`ShowItems` on Linux's
/// session bus), else its folder.
pub fn reveal(path: &Path) {
    let quiet = |c: &mut std::process::Command| {
        c.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    };
    let path = path.to_path_buf();
    // dbus-send waits for the file manager to start: not on the UI's thread.
    std::thread::spawn(move || {
        let shown = if cfg!(target_os = "macos") {
            let mut c = std::process::Command::new("open");
            c.arg("-R").arg(&path);
            quiet(&mut c);
            c.status().is_ok_and(|s| s.success())
        } else if cfg!(windows) {
            let mut arg = std::ffi::OsString::from("/select,");
            arg.push(&path);
            let mut c = std::process::Command::new("explorer");
            c.arg(arg);
            quiet(&mut c);
            // Explorer's exit code says nothing: it showed it.
            c.status().is_ok()
        } else {
            let mut c = std::process::Command::new("dbus-send");
            c.args([
                "--session",
                "--print-reply",
                "--dest=org.freedesktop.FileManager1",
                "--type=method_call",
                "/org/freedesktop/FileManager1",
                "org.freedesktop.FileManager1.ShowItems",
            ]);
            c.arg(format!("array:string:{}", file_uri(&path))).arg("string:");
            quiet(&mut c);
            c.status().is_ok_and(|s| s.success())
        };
        if !shown {
            if let Some(dir) = path.parent() {
                let opener = if cfg!(windows) { "explorer" } else if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
                let mut c = std::process::Command::new(opener);
                c.arg(dir);
                quiet(&mut c);
                let _ = c.spawn();
            }
        }
    });
}

/// `file:///home/a/My%20file.pdf`.
fn file_uri(p: &Path) -> String {
    let mut out = String::from("file://");
    for &b in p.as_os_str().as_encoded_bytes() {
        if b.is_ascii_alphanumeric() || b"/-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `1.2 MB`, or `1,2 MB` the way Slack writes sizes in Portuguese.
pub fn size_label(n: u64) -> String {
    let units = ["KB", "MB", "GB", "TB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut v = n as f64 / 1024.0;
    let mut u = 0;
    while v >= 1024.0 && u < units.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 || v >= 100.0 {
        format!("{} {}", v.round(), units[u])
    } else {
        let s = format!("{v:.1} {}", units[u]);
        if crate::i18n::is_pt() { s.replace('.', ",") } else { s }
    }
}

/// A Nerd Font file glyph (one cell) and its color, by type.
fn icon(f: &File) -> (&'static str, Color) {
    let (t, m) = (f.filetype.as_str(), f.mimetype.as_str());
    match t {
        "pdf" => ("\u{f1c1}", Color::Red),
        "zip" | "gzip" | "gz" | "tar" | "tgz" | "rar" | "7z" | "bz2" | "xz" | "zst" => ("\u{f1c6}", Color::Yellow),
        "doc" | "docx" | "odt" | "rtf" | "gdoc" | "pages" => ("\u{f1c2}", Color::Blue),
        "xls" | "xlsx" | "ods" | "csv" | "tsv" | "gsheet" | "numbers" => ("\u{f1c3}", Color::Green),
        "ppt" | "pptx" | "odp" | "key" | "gpres" => ("\u{f1c4}", Color::LightRed),
        "text" | "markdown" | "post" | "space" => ("\u{f0f6}", Color::Gray),
        _ if m.starts_with("image/") => ("\u{f1c5}", Color::Magenta),
        _ if m.starts_with("audio/") => ("\u{f1c7}", Color::Magenta),
        _ if m.starts_with("video/") => ("\u{f1c8}", Color::Magenta),
        _ if m.starts_with("application/zip") || m.contains("compressed") => ("\u{f1c6}", Color::Yellow),
        _ if m.starts_with("text/") || m.contains("json") || m.contains("xml") || m.contains("javascript") => {
            ("\u{f1c9}", Color::Cyan)
        }
        _ => ("\u{f016}", Color::Gray),
    }
}

/// Like Slack's file card: the icon and name, then type, size and what can be
/// done with it. `line` is where the card starts in the message list.
pub fn card(f: &File, state: Option<&State>, indent: &str, width: usize, line: usize) -> (Vec<Line<'static>>, Vec<Hit>) {
    let (glyph, color) = icon(f);
    let shown = truncate(&f.name, width.saturating_sub(3));
    let saved = match state {
        Some(State::Saved(p)) => Some(p.clone()),
        _ => None,
    };
    let mut hits = vec![Hit {
        line,
        x: indent.width() as u16,
        w: 3 + shown.width() as u16,
        click: Click::Menu(Job::file(f), saved),
    }];
    let name = Line::from(vec![
        Span::raw(indent.to_string()),
        Span::styled(glyph, Style::new().fg(color)),
        Span::raw("  "),
        Span::styled(shown, Style::new().bold()),
    ]);

    let kind = if !f.pretty_type.is_empty() { f.pretty_type.clone() } else { f.filetype.to_uppercase() };
    let meta = [kind, if f.size > 0 { size_label(f.size) } else { String::new() }]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    let dim = Style::new().fg(Color::DarkGray);
    let link = Style::new().fg(Color::Cyan);
    let mut spans = vec![Span::raw(format!("{indent}   ")), Span::styled(meta, dim)];
    let mut x = spans.iter().map(|s| s.content.width()).sum::<usize>();
    let mut action = |spans: &mut Vec<Span<'static>>, gap: &str, label: &str, style: Style, click: Option<Click>| {
        spans.push(Span::styled(gap.to_string(), dim));
        x += gap.width();
        if let Some(click) = click {
            hits.push(Hit { line: line + 1, x: x as u16, w: label.width() as u16, click });
        }
        spans.push(Span::styled(label.to_string(), style));
        x += label.width();
    };
    match (state, Job::file(f)) {
        (Some(State::Saving(p)), _) => {
            action(&mut spans, "   ", tr("Downloading", "Baixando"), dim.italic(), None);
            if let Some(f) = p.fraction() {
                action(&mut spans, " ", &bar(f, 10), Style::new().fg(Color::Cyan), None);
            }
            action(&mut spans, " ", &p.label(), dim, None);
        }
        (Some(State::Saved(p)), _) => {
            action(&mut spans, "   ", tr("✓ Downloaded", "✓ Baixado"), Style::new().fg(Color::Green), None);
            action(&mut spans, "  ", tr("Open", "Abrir"), link, Some(Click::Open(p.clone())));
            action(&mut spans, " · ", tr("Show in folder", "Mostrar na pasta"), link, Some(Click::Reveal(p.clone())));
        }
        (None, Some(job)) => action(&mut spans, "   ", tr("⤓ Download", "⤓ Baixar"), link, Some(Click::Save(job))),
        (None, None) => {}
    }
    (vec![name, Line::from(spans)], hits)
}
