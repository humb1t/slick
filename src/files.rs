//! Shared files: the card a message shows for them and saving them to the
//! downloads folder.

use crate::app::ImgRef;
use crate::cache;
use crate::slack::{File, Res, Slack};
use crate::ui::truncate;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use std::path::{Path, PathBuf};
use unicode_width::UnicodeWidthStr;

/// A download, keyed by `Job::key`.
pub enum State {
    Saving,
    Saved(PathBuf),
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
            Some(e) if !r.name.contains('.') => format!("{}.{e}", if r.name.is_empty() { "imagem" } else { &r.name }),
            _ => r.name.clone(),
        };
        // `Slack::download` stops at 20 MB: a copy that size may be cut short.
        let cached = Some(cache::image_path(&r.key)).filter(|p| p.metadata().is_ok_and(|m| m.len() < 20 << 20));
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
    if n.is_empty() || n == "." || n == ".." { "arquivo".into() } else { n.to_string() }
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
/// only gets its name once complete.
pub fn save(s: &Slack, job: &Job) -> Res<PathBuf> {
    let dir = downloads_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let (path, part, mut out) = claim(&dir, &safe_name(&job.name))?;
    let res = match &job.cached {
        Some(c) => std::fs::File::open(c).and_then(|mut f| std::io::copy(&mut f, &mut out)).map_err(|e| e.to_string()),
        None => s.download_to(&job.url, &mut out, job.html_ok),
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
    let part = dir.join(".part");
    let mut out = std::fs::File::create(&part).map_err(|e| e.to_string())?;
    let res = s.download_to(&job.url, &mut out, job.html_ok);
    drop(out);
    match res.and_then(|_| std::fs::rename(&part, &path).map_err(|e| e.to_string())) {
        Ok(()) => Ok(path),
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            Err(e)
        }
    }
}

/// `1,2 MB`, the way Slack writes sizes in Portuguese.
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
        format!("{v:.1} {}", units[u]).replace('.', ",")
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
        (Some(State::Saving), _) => action(&mut spans, "   ", "baixando…", dim.italic(), None),
        (Some(State::Saved(p)), _) => {
            action(&mut spans, "   ", "✓ salvo", Style::new().fg(Color::Green), None);
            action(&mut spans, "  ", "Abrir", link, Some(Click::Open(p.clone())));
            action(&mut spans, " · ", "Mostrar na pasta", link, Some(Click::Reveal(p.clone())));
        }
        (None, Some(job)) => action(&mut spans, "   ", "⤓ Baixar", link, Some(Click::Save(job))),
        (None, None) => {}
    }
    (vec![name, Line::from(spans)], hits)
}
