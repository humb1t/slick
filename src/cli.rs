//! The commands for scripts and the Omarchy widget: they talk to the
//! running slick (`ipc`), or do without it where they can.

use crate::ipc::{self, Cmd};
use serde_json::{json, Value};
use std::io::IsTerminal;

/// `println!` that doesn't panic when the reader went away (`slick status | head`).
macro_rules! out {
    ($($t:tt)*) => {{
        use std::io::Write;
        let _ = writeln!(std::io::stdout(), $($t)*);
    }};
}

pub enum Next {
    Exit(i32),
    /// Nothing running and a terminal here: open slick, then do this.
    Start(Cmd),
}

/// `None` when the first argument is none of these commands.
pub fn run(args: &[String]) -> Option<Next> {
    let rest = &args[1.min(args.len())..];
    Some(match args.first()?.as_str() {
        "open" => open(rest),
        "quit" => Next::Exit(if ipc::running() { send(json!({ "cmd": "quit" })) } else { 0 }),
        "mark-read" => match rest.first() {
            Some(conv) => Next::Exit(send_running(json!({ "cmd": "mark-read", "conv": conv }))),
            None => usage_error(),
        },
        "dnd" => dnd(rest),
        "status" => Next::Exit(status(rest.iter().any(|a| a == "--json"))),
        "events" => Next::Exit(ipc::follow()),
        _ => return None,
    })
}

fn usage_error() -> Next {
    eprint!("{}", crate::usage());
    Next::Exit(2)
}

fn send(cmd: Value) -> i32 {
    match ipc::send(&cmd) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{}", trf!("slick: didn't reach the running slick: {e}", "slick: não falou com o slick aberto: {e}"));
            1
        }
    }
}

fn send_running(cmd: Value) -> i32 {
    if ipc::running() {
        send(cmd)
    } else {
        eprintln!("{}", crate::i18n::tr("slick isn't running", "o slick não está aberto"));
        1
    }
}

/// `slick open <conversation> [ts] [--thread <ts>] [--background]`.
fn open(args: &[String]) -> Next {
    let (mut pos, mut thread, mut focus) = (Vec::new(), None, true);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--thread" => thread = it.next().cloned(),
            "--background" => focus = false,
            _ => pos.push(a.clone()),
        }
    }
    let Some(conv) = pos.first().cloned() else { return usage_error() };
    let ts = pos.get(1).cloned();
    if ipc::running() {
        return Next::Exit(send(json!({ "cmd": "open", "conv": conv, "ts": ts, "thread_ts": thread, "focus": focus })));
    }
    if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
        return Next::Start(Cmd::Open { conv, ts, thread, focus });
    }
    Next::Exit(launch(args))
}

/// Opens slick in a new terminal window, with these arguments to `slick open`.
#[cfg(target_os = "linux")]
fn launch(args: &[String]) -> i32 {
    let class = std::env::var("SLICK_WINDOW_CLASS").ok().filter(|c| !c.is_empty()).unwrap_or_else(|| "org.omarchy.slick".into());
    let exe = std::env::current_exe().unwrap_or_else(|_| "slick".into());
    let spawned = std::process::Command::new("xdg-terminal-exec")
        .arg(format!("--app-id={class}"))
        .arg("-e")
        .arg(exe)
        .arg("open")
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    match spawned {
        Ok(_) => 0,
        Err(_) => {
            eprintln!("{}", crate::i18n::tr("slick isn't running", "o slick não está aberto"));
            1
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn launch(_args: &[String]) -> i32 {
    eprintln!("{}", crate::i18n::tr("slick isn't running", "o slick não está aberto"));
    1
}

/// `slick dnd pause <minutes> | resume | status`. Without a running slick
/// it asks Slack itself, with the saved session.
fn dnd(args: &[String]) -> Next {
    let running = ipc::running();
    let code = match (args.first().map(String::as_str), args.get(1).and_then(|m| m.parse::<i64>().ok())) {
        (Some("pause"), Some(min)) if min > 0 && running => send(json!({ "cmd": "dnd.pause", "minutes": min })),
        (Some("resume"), _) if running => send(json!({ "cmd": "dnd.resume" })),
        (Some("status"), _) if running => {
            let dnd = &snapshot()["dnd"];
            out!("{}", describe(dnd["until"].as_i64().filter(|_| dnd["paused"] == true)));
            0
        }
        (Some(cmd @ ("pause" | "resume" | "status")), min) => {
            let Some(creds) = crate::config::load() else {
                eprintln!("{}", crate::i18n::tr("slick: not signed in", "slick: não entrou no Slack"));
                return Next::Exit(1);
            };
            let s = crate::slack::Slack::new(creds);
            let res = match (cmd, min) {
                ("pause", Some(m)) if m > 0 => crate::account::pause(&s, m),
                ("pause", _) => return usage_error(),
                ("resume", _) => crate::account::dnd(&s).and_then(|p| crate::account::resume(&s, p.is_none_or(|p| p.snooze))),
                _ => crate::account::dnd(&s),
            };
            match res {
                Ok(p) => {
                    out!("{}", describe(p.map(|p| p.until)));
                    0
                }
                Err(e) => {
                    eprintln!("slick: {e}");
                    1
                }
            }
        }
        _ => return usage_error(),
    };
    Next::Exit(code)
}

fn describe(until: Option<i64>) -> String {
    match until {
        Some(t) => trf!("notifications paused {}", "notificações pausadas {}", crate::account::until(t)),
        None => crate::i18n::tr("notifications on", "notificações ligadas").into(),
    }
}

/// What the running slick last wrote; `{"running": false}` when none is.
fn snapshot() -> Value {
    let stopped = json!({ "v": 1, "running": false });
    if !ipc::running() {
        return stopped;
    }
    std::fs::read_to_string(crate::events::state_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(stopped)
}

/// `slick status [--json]`: the unread conversations, as the widget sees them.
fn status(as_json: bool) -> i32 {
    let s = snapshot();
    if as_json {
        out!("{s}");
        return 0;
    }
    if s["running"] != true {
        out!("{}", crate::i18n::tr("slick isn't running", "o slick não está aberto"));
        return 0;
    }
    let (n, mentions) = (s["attention"].as_u64().unwrap_or(0), s["totals"]["mentions"].as_u64().unwrap_or(0));
    let mut head = format!("slick · {}", s["team"].as_str().unwrap_or(""));
    head += &match (n, mentions) {
        (0, _) => crate::i18n::tr(" · nothing waiting", " · nada esperando").to_string(),
        (n, 0) => trf!(" · {n} waiting", " · {n} esperando"),
        (n, 1) => trf!(" · {n} waiting (1 mention)", " · {n} esperando (1 menção)"),
        (n, m) => trf!(" · {n} waiting ({m} mentions)", " · {n} esperando ({m} menções)"),
    };
    out!("{head}");
    if s["dnd"]["paused"] == true {
        out!("{}", describe(s["dnd"]["until"].as_i64()));
    }
    for c in s["conversations"].as_array().into_iter().flatten() {
        let name = c["name"].as_str().unwrap_or("");
        let name = if matches!(c["kind"].as_str(), Some("channel" | "private")) { format!("#{name}") } else { name.to_string() };
        let last = c["messages"].as_array().and_then(|m| m.last());
        let line = last.map(|m| {
            let text: String = m["text"].as_str().unwrap_or("").split_whitespace().collect::<Vec<_>>().join(" ").chars().take(80).collect();
            format!(" · {}: {text}", m["author"].as_str().unwrap_or(""))
        });
        let at = match c["mentions"].as_u64() {
            Some(m) if m > 0 => format!(" (@{m})"),
            _ => String::new(),
        };
        out!("  {name}{at}{}", line.unwrap_or_default());
    }
    0
}
