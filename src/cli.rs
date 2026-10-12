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
        "mark-read" => {
            let (pos, team) = take_team(rest);
            match pos.first() {
                Some(conv) => Next::Exit(send_running(json!({ "cmd": "mark-read", "conv": conv, "team": team }))),
                None => usage_error(),
            }
        }
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

/// `--team <T0AAA|acme>` taken out of the arguments: the rest, and the
/// workspace it names (an id as it is, an address as `acme`).
fn take_team(args: &[String]) -> (Vec<String>, Option<String>) {
    let (mut rest, mut team) = (Vec::new(), None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--team" => team = it.next().and_then(|t| team_name(t)),
            _ => rest.push(a.clone()),
        }
    }
    (rest, team)
}

/// A workspace as `--team` takes it: an id (`T0AAA`) as it is, else its address.
fn team_name(t: &str) -> Option<String> {
    let id = t.starts_with('T') && t.len() > 1 && t.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit());
    if id { Some(t.to_string()) } else { crate::config::workspace_name(t) }
}

/// A Slack link to a message, `https://acme.slack.com/archives/C0123/p1760000000000100`
/// (`?thread_ts=…` for a reply), or to a conversation in the web client,
/// `https://app.slack.com/client/T0AAA/C0123`: (workspace, conversation, ts, thread).
fn permalink(url: &str) -> Option<(Option<String>, String, Option<String>, Option<String>)> {
    let rest = url.trim().strip_prefix("https://").or_else(|| url.trim().strip_prefix("http://"))?;
    let (host, path) = rest.split_once('/')?;
    let (path, query) = path.split_once('?').unwrap_or((path, ""));
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    let conv_ok = |c: &str| !c.is_empty() && c.chars().all(|c| c.is_ascii_alphanumeric());
    if host == "app.slack.com" {
        return match parts.as_slice() {
            ["client", team, conv, ..] if conv_ok(conv) => Some((Some(team.to_string()), conv.to_string(), None, None)),
            _ => None,
        };
    }
    let domain = host.strip_suffix(".slack.com")?;
    let (conv, p) = match parts.as_slice() {
        ["archives", conv, p, ..] => (*conv, Some(*p)),
        ["archives", conv] => (*conv, None),
        _ => return None,
    };
    if !conv_ok(conv) {
        return None;
    }
    // p1760000000000100 → 1760000000.000100
    let ts = p.and_then(|p| p.strip_prefix('p')).filter(|d| d.len() > 6 && d.chars().all(|c| c.is_ascii_digit())).map(|d| {
        let (secs, micros) = d.split_at(d.len() - 6);
        format!("{secs}.{micros}")
    });
    let thread = query.split('&').find_map(|kv| kv.strip_prefix("thread_ts=")).filter(|t| !t.is_empty()).map(String::from);
    // The thread's own first message is not a reply in it.
    let thread = thread.filter(|t| ts.as_ref() != Some(t));
    Some((crate::config::workspace_name(domain), conv.to_string(), ts, thread))
}

/// `slick open <conversation|link> [ts] [--thread <ts>] [--team <workspace>] [--background]`.
fn open(args: &[String]) -> Next {
    let (args_left, mut team) = take_team(args);
    let (mut pos, mut thread, mut focus) = (Vec::new(), None, true);
    let mut it = args_left.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--thread" => thread = it.next().cloned(),
            "--background" => focus = false,
            _ => pos.push(a.clone()),
        }
    }
    let Some(first) = pos.first().cloned() else { return usage_error() };
    let (conv, mut ts) = (first.clone(), pos.get(1).cloned());
    let conv = match permalink(&first) {
        Some((t, c, link_ts, link_thread)) => {
            team = team.or(t);
            ts = ts.or(link_ts);
            thread = thread.or(link_thread);
            c
        }
        None => conv,
    };
    if ipc::running() {
        return Next::Exit(send(json!({ "cmd": "open", "conv": conv, "ts": ts, "thread_ts": thread, "focus": focus, "team": team })));
    }
    if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
        return Next::Start(Cmd::Open { conv, ts, thread, focus, team });
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
            if cmd == "pause" && !min.is_some_and(|m| m > 0) {
                return usage_error();
            }
            // Every workspace signed in to: a pause is for all of them.
            // `status` asks the last one on screen.
            let mut all = crate::config::workspaces();
            if all.is_empty() {
                eprintln!("{}", crate::i18n::tr("slick: not signed in", "slick: não entrou no Slack"));
                return Next::Exit(1);
            }
            if cmd == "status" {
                let active = crate::config::active();
                let i = all.iter().position(|w| w.team_id.is_some() && w.team_id == active).unwrap_or(0);
                all = vec![all.swap_remove(i)];
            }
            let (mut shown, mut failed) = (false, false);
            for ws in all {
                let s = crate::slack::Slack::new(ws.creds);
                let res = match (cmd, min) {
                    ("pause", Some(m)) => crate::account::pause(&s, m),
                    ("resume", _) => crate::account::dnd(&s).and_then(|p| crate::account::resume(&s, p.is_none_or(|p| p.snooze))),
                    _ => crate::account::dnd(&s),
                };
                match res {
                    Ok(p) if !shown => {
                        shown = true;
                        out!("{}", describe(p.map(|p| p.until)));
                    }
                    Ok(_) => {}
                    Err(e) => {
                        failed = true;
                        match ws.domain.or(ws.team_id) {
                            Some(w) => eprintln!("slick: {w}: {e}"),
                            None => eprintln!("slick: {e}"),
                        }
                    }
                }
            }
            failed as i32
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

/// " · 3 waiting (1 mention)", or " · nothing waiting".
fn waiting(n: u64, mentions: u64) -> String {
    match (n, mentions) {
        (0, _) => crate::i18n::tr(" · nothing waiting", " · nada esperando").to_string(),
        (n, 0) => trf!(" · {n} waiting", " · {n} esperando"),
        (n, 1) => trf!(" · {n} waiting (1 mention)", " · {n} esperando (1 menção)"),
        (n, m) => trf!(" · {n} waiting ({m} mentions)", " · {n} esperando ({m} menções)"),
    }
}

/// The unread conversations, one line each, indented by `pad`.
fn conversations<'a>(list: impl Iterator<Item = &'a Value>, pad: &str) {
    for c in list {
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
        out!("{pad}{name}{at}{}", line.unwrap_or_default());
    }
}

/// `slick status [--json]`: the unread conversations, as the widget sees
/// them; with more than one workspace, under each one's name.
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
    let count = |v: &Value| (v["attention"].as_u64().unwrap_or(0), v["totals"]["mentions"].as_u64().unwrap_or(0));
    let workspaces: Vec<&Value> = s["workspaces"].as_array().into_iter().flatten().collect();
    let convs = s["conversations"].as_array().into_iter().flatten();
    if workspaces.len() <= 1 {
        let (n, mentions) = count(&s);
        out!("slick · {}{}", s["team"].as_str().unwrap_or(""), waiting(n, mentions));
        if s["dnd"]["paused"] == true {
            out!("{}", describe(s["dnd"]["until"].as_i64()));
        }
        conversations(convs, "  ");
        return 0;
    }
    let (n, mentions) = count(&s);
    out!("slick{}", waiting(n, mentions));
    for w in workspaces {
        let (n, mentions) = count(w);
        let on = if w["active"] == true { " *" } else { "" };
        out!("  {}{on}{}", w["team"].as_str().unwrap_or(""), waiting(n, mentions));
        if w["dnd"]["paused"] == true {
            out!("    {}", describe(w["dnd"]["until"].as_i64()));
        }
        conversations(convs.clone().filter(|c| c["team"]["id"] == w["id"]), "    ");
    }
    0
}
