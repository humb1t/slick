#[macro_use]
mod i18n;
mod account;
mod app;
mod attach;
mod blocks;
mod cache;
mod cli;
mod compose;
mod config;
mod edit;
mod editor;
mod icons;
mod ipc;
mod emoji;
mod events;
mod files;
mod login;
mod mention;
#[cfg(not(target_os = "linux"))]
mod login_window;
mod pins;
mod rtm;
mod selection;
mod slack;
mod text;
mod thread;
#[cfg(target_os = "linux")]
mod tray;
mod ui;
mod update;

use crossterm::event::{
    self, DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste, EnableFocusChange,
    EnableMouseCapture, Event as TermEvent,
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

fn usage() -> &'static str {
    i18n::tr(
        "\
usage:
  slick                    open slick, or bring the open one to the front
  slick --new-instance     open another slick alongside the running one
  slick login [workspace]  sign in to Slack through the sign-in window
  slick update             look for and install a new version
  slick --version          show the version

talking to the running slick (see docs/events.md):
  slick open <conversation> [ts] [--thread <ts>] [--background]
  slick mark-read <conversation>
  slick dnd pause <minutes> | resume | status
  slick quit
  slick status [--json]    what is unread, as JSON for scripts
  slick events             follow new messages, counts and pauses (JSON lines)
",
        "\
uso:
  slick                    abre o slick, ou traz pra frente o que já está aberto
  slick --new-instance     abre outro slick além do que já está rodando
  slick login [workspace]  entra no Slack pela janela de login
  slick update             procura e instala a versão nova
  slick --version          mostra a versão

falando com o slick aberto (veja docs/events.md):
  slick open <conversa> [ts] [--thread <ts>] [--background]
  slick mark-read <conversa>
  slick dnd pause <minutos> | resume | status
  slick quit
  slick status [--json]    o que não foi lido, em JSON para scripts
  slick events             acompanha mensagens novas, contagens e pausas (linhas JSON)
",
    )
}

/// "slick 0.1.0 (abc1234)", the commit when built from git.
fn version() -> String {
    let v = concat!("slick ", env!("CARGO_PKG_VERSION"));
    match env!("GIT_COMMIT") {
        "" => v.to_string(),
        c => format!("{v} ({c})"),
    }
}

fn main() {
    i18n::init();
    match std::env::args().nth(1).as_deref() {
        #[cfg(not(target_os = "linux"))]
        Some(login::ARG) => login_window::run(),
        Some("-V" | "--version") => return println!("{}", version()),
        Some("-h" | "--help") => return print!("{}", usage()),
        Some("--new-instance") => {}
        Some(a) if a.starts_with('-') => {
            eprint!("{}{}", trf!("slick: unknown option {a}\n", "slick: opção desconhecida {a}\n"), usage());
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
    let args: Vec<String> = std::env::args().skip(1).collect();
    let start = match cli::run(&args) {
        Some(cli::Next::Exit(code)) => std::process::exit(code),
        Some(cli::Next::Start(cmd)) => Some(cmd),
        None => None,
    };
    // A slick already running is brought to the front instead of starting
    // another; this one doesn't even need a terminal for that.
    let new_instance = args.first().map(String::as_str) == Some("--new-instance");
    let server = match if new_instance { None } else { ipc::claim() } {
        Some(ipc::Claim::Owner(s)) => Some(s),
        Some(ipc::Claim::Running) => {
            if let Err(e) = ipc::send(&serde_json::json!({ "cmd": "focus" })) {
                eprintln!("{}", trf!("slick is already running, but didn't answer: {e}", "o slick já está rodando, mas não respondeu: {e}"));
                std::process::exit(1);
            }
            // Only Hyprland lets us bring the other window up; elsewhere, say where it is.
            if !cfg!(target_os = "linux") || std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_none() {
                eprintln!("{}", i18n::tr("slick is already open in another window", "o slick já está aberto em outra janela"));
            }
            return;
        }
        None => None,
    };
    // The one that owns the socket speaks for slick; a second one keeps quiet.
    let owner = server.is_some();
    {
        use std::io::IsTerminal;
        if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
            eprintln!("{}", i18n::tr("slick: needs a terminal", "slick: precisa de um terminal"));
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
    // Whether the window has the keyboard: the open conversation only counts as read then.
    let _ = crossterm::execute!(std::io::stdout(), EnableFocusChange);

    update::background(tx.clone());
    if let Some(s) = server {
        s.serve(tx.clone());
    }
    if let Some(cmd) = start {
        let _ = tx.send(app::Event::Remote(cmd));
    }
    #[cfg(target_os = "linux")]
    let (tray_tx, mut tray) = (tx.clone(), None::<tray::Tray>);
    let logged_in = creds.is_some();
    let mut app = app::App::new(slack::Slack::new(creds.unwrap_or_default()), tx, cache::load(), picker, logged_in);
    app.events.on = owner;
    #[cfg(target_os = "linux")]
    {
        app.account.tray = config::tray();
    }
    while !app.quit {
        if app.dirty {
            app.dirty = false;
            // Turned on or off in the settings: dropping it takes it off the bar.
            #[cfg(target_os = "linux")]
            if app.account.tray != tray.is_some() {
                tray = app.account.tray.then(|| tray::Tray::start(tray_tx.clone()));
            }
            #[cfg(target_os = "linux")]
            if let Some(t) = &mut tray {
                // A pause you set dims it, like the widget; the night schedule doesn't.
                let snoozed = app.account.paused.is_some_and(|p| p.snooze);
                t.set(if snoozed { (0, 0) } else { app.attention() });
            }
            app.publish();
            if terminal.draw(|f| ui::draw(f, &mut app)).is_err() {
                break;
            }
            app.request_images();
            app.request_names();
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
            let ev = event::read();
            // Typing or clicking means the window is seen, also in terminals
            // that never report focus.
            if matches!(ev, Ok(TermEvent::Key(_) | TermEvent::Mouse(_) | TermEvent::Paste(_))) {
                app.set_focused(true);
            }
            match ev {
                Ok(TermEvent::Key(k)) if k.kind != KeyEventKind::Release => app.on_key(k),
                Ok(TermEvent::Mouse(m)) => app.on_mouse(m),
                Ok(TermEvent::Paste(s)) => app.on_paste(&s),
                Ok(TermEvent::FocusGained) => app.set_focused(true),
                Ok(TermEvent::FocusLost) => app.set_focused(false),
                Ok(TermEvent::Resize(..)) => {
                    app.protocols.clear();
                    app.dirty = true;
                }
                _ => {}
            }
        }
    }
    let _ = crossterm::execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
    let _ = crossterm::execute!(std::io::stdout(), DisableMouseCapture, DisableBracketedPaste, DisableFocusChange);
    ratatui::restore();
    app.unpublish();
    cache::save(app.to_cache());
    if app.restart {
        eprintln!("{}", trf!("didn't restart: {}", "não reiniciou: {}", update::restart()));
    }
}

/// `slick login`: the sign-in window, then the session saved to the config.
fn login_cli() -> i32 {
    let workspace = std::env::args().nth(2).and_then(|w| config::workspace_name(&w)).or_else(config::workspace);
    eprintln!("{}", i18n::tr("Opening the Slack sign-in window…", "Abrindo a janela de login do Slack…"));
    let checked = login::window(workspace.as_deref()).and_then(|creds| slack::Slack::new(creds.clone()).auth().map(|me| (me, creds)));
    match checked {
        Ok((me, creds)) => match config::save(&creds, &me.domain) {
            Ok(()) => {
                eprintln!("{}", trf!("Signed in: {} · {}", "Entrou: {} · {}", me.team, me.user));
                0
            }
            Err(e) => {
                let p = config::path();
                eprintln!("{}", trf!("Signed in, but didn't save {}: {e}", "Entrou, mas não salvou {}: {e}", p.display()));
                1
            }
        },
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}
