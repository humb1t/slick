mod account;
mod app;
mod blocks;
mod cache;
mod compose;
mod config;
mod edit;
mod editor;
mod icons;
mod emoji;
mod files;
mod login;
mod login_window;
mod pins;
mod selection;
mod slack;
mod text;
mod thread;
#[cfg(target_os = "linux")]
mod tray;
mod ui;
mod update;

use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event as TermEvent,
    KeyEventKind, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Asks the terminal for the given ANSI palette entries (OSC 4), with a
/// device attributes query after them so we know when the replies are over.
/// Gives up after a short wait; raw mode must be on.
fn query_palette(idx: &[u8]) -> Vec<Option<[u8; 3]>> {
    use std::io::{Read, Write};
    let none = vec![None; idx.len()];
    // The console hands replies to crossterm's reader, not to stdin: no asking.
    if cfg!(windows) {
        return none;
    }
    let mut q: String = idx.iter().map(|i| format!("\x1b]4;{i};?\x1b\\")).collect();
    q.push_str("\x1b[c");
    let mut out = std::io::stdout();
    if out.write_all(q.as_bytes()).and_then(|_| out.flush()).is_err() {
        return none;
    }
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 256];
        // Read until the device attributes reply, "ESC [ ? … c".
        while !buf.windows(3).position(|w| w == b"\x1b[?").is_some_and(|i| buf[i..].contains(&b'c')) {
            match std::io::stdin().read(&mut chunk) {
                Ok(n) if n > 0 => buf.extend_from_slice(&chunk[..n]),
                _ => break,
            }
        }
        let _ = tx.send(buf);
    });
    let Ok(buf) = rx.recv_timeout(Duration::from_millis(100)) else { return none };
    let text = String::from_utf8_lossy(&buf);
    idx.iter().map(|i| parse_osc4(&text, *i)).collect()
}

/// The colour in an "ESC ] 4 ; N ; rgb:RRRR/GGGG/BBBB" reply, in 8 bits.
fn parse_osc4(text: &str, i: u8) -> Option<[u8; 3]> {
    let tag = format!("]4;{i};rgb:");
    let body = &text[text.find(&tag)? + tag.len()..];
    let end = body.find(['\x1b', '\x07']).unwrap_or(body.len());
    let mut parts = body[..end].split('/');
    let mut rgb = [0u8; 3];
    for c in &mut rgb {
        let hex = parts.next()?;
        let max = 16u32.checked_pow(hex.len() as u32)?.checked_sub(1).filter(|m| *m > 0)?;
        *c = (u32::from_str_radix(hex, 16).ok()? * 255 / max) as u8;
    }
    Some(rgb)
}

const USAGE: &str = "\
uso:
  slick                    abre o slick
  slick login [workspace]  entra no Slack pela janela de login
  slick update             procura e instala a versão nova
  slick --version          mostra a versão
";

/// "slick 0.1.0 (abc1234)", the commit when built from git.
fn version() -> String {
    let v = concat!("slick ", env!("CARGO_PKG_VERSION"));
    match env!("GIT_COMMIT") {
        "" => v.to_string(),
        c => format!("{v} ({c})"),
    }
}

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some(login_window::ARG) => login_window::run(),
        Some("-V" | "--version") => return println!("{}", version()),
        Some("-h" | "--help") => return print!("{USAGE}"),
        Some(a) if a.starts_with('-') => {
            eprint!("slick: opção desconhecida {a}\n{USAGE}");
            std::process::exit(2);
        }
        _ => {}
    }
    #[cfg(unix)]
    config::migrate();
    update::cleanup();
    if std::env::args().nth(1).as_deref() == Some("login") {
        std::process::exit(login_cli());
    }
    if std::env::args().nth(1).as_deref() == Some("update") {
        std::process::exit(update::cli());
    }
    {
        use std::io::IsTerminal;
        if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
            eprintln!("slick: precisa de um terminal");
            std::process::exit(1);
        }
    }
    let creds = config::load();
    std::thread::spawn(cache::prune_images);
    let (tx, rx) = mpsc::channel();

    let mut terminal = ratatui::init();
    // Asks the terminal what graphics it speaks (kitty, sixel…), so it must
    // run before we start reading input ourselves.
    let picker = ratatui_image::picker::Picker::from_query_stdio().ok();
    // The presence dots are pictures; paint them in the theme's own colours.
    let palette = query_palette(&[2, 8]);
    icons::set_palette(palette[0], palette[1]);
    // Kitty keyboard protocol tells Shift+Enter apart from Enter. Apart from
    // the rest: the Windows console refuses it, and that would stop the others.
    let _ = crossterm::execute!(
        std::io::stdout(),
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    );
    let _ = crossterm::execute!(std::io::stdout(), EnableMouseCapture, EnableBracketedPaste);

    update::background(tx.clone());
    #[cfg(target_os = "linux")]
    let mut tray = config::tray().then(|| tray::Tray::start(tx.clone()));
    let logged_in = creds.is_some();
    let mut app = app::App::new(slack::Slack::new(creds.unwrap_or_default()), tx, cache::load(), picker, logged_in);
    while !app.quit {
        if app.dirty {
            app.dirty = false;
            #[cfg(target_os = "linux")]
            if let Some(t) = &mut tray {
                t.set(app.attention());
            }
            if terminal.draw(|f| ui::draw(f, &mut app)).is_err() {
                break;
            }
            app.request_images();
        }
        while let Ok(ev) = rx.try_recv() {
            app.handle(ev);
        }
        app.tick();
        // A visible animated emoji asks for a redraw when its frame changes,
        // at most every 20ms: wait just until then, so it isn't late by up to 50ms.
        // With none on screen nothing changes here.
        let now = Instant::now();
        let wait = match app.next_frame {
            Some(t) if t <= now => {
                app.next_frame = None;
                app.dirty = true;
                Duration::ZERO
            }
            Some(t) => (t - now).min(Duration::from_millis(50)),
            None => Duration::from_millis(50),
        };
        if event::poll(wait).unwrap_or(false) {
            match event::read() {
                Ok(TermEvent::Key(k)) if k.kind != KeyEventKind::Release => app.on_key(k),
                Ok(TermEvent::Mouse(m)) => app.on_mouse(m),
                Ok(TermEvent::Paste(s)) => app.on_paste(&s),
                Ok(TermEvent::Resize(..)) => {
                    app.protocols.clear();
                    app.dirty = true;
                }
                _ => {}
            }
        }
    }
    let _ = crossterm::execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
    let _ = crossterm::execute!(std::io::stdout(), DisableMouseCapture, DisableBracketedPaste);
    ratatui::restore();
    cache::save(app.to_cache());
    if app.restart {
        eprintln!("não reiniciou: {}", update::restart());
    }
}

/// `slick login`: the sign-in window, then the session saved to the config.
fn login_cli() -> i32 {
    let workspace = std::env::args().nth(2).and_then(|w| config::workspace_name(&w)).or_else(config::workspace);
    eprintln!("Abrindo a janela de login do Slack…");
    let checked = login::window(workspace.as_deref()).and_then(|creds| slack::Slack::new(creds.clone()).auth().map(|me| (me, creds)));
    match checked {
        Ok((me, creds)) => match config::save(&creds, &me.domain) {
            Ok(()) => {
                eprintln!("Entrou: {} · {}", me.team, me.user);
                0
            }
            Err(e) => {
                eprintln!("Entrou, mas não salvou {}: {e}", config::path().display());
                1
            }
        },
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}
