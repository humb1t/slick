//! The system keyring: each workspace's token and cookie in the Secret
//! Service, through `secret-tool` (libsecret), one entry per workspace
//! (`application=slick team=<id>`). The secret only goes through its stdin
//! and stdout, never its arguments. Elsewhere than Linux, or without a
//! keyring, nothing is stored here and the config file keeps them.

use crate::slack::Creds;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// What was read or stored this run, so the keyring is asked once per workspace.
fn known() -> &'static Mutex<HashMap<String, Creds>> {
    static K: OnceLock<Mutex<HashMap<String, Creds>>> = OnceLock::new();
    K.get_or_init(Default::default)
}

/// Only an id like `T0AAA` names an entry.
fn valid(team: &str) -> bool {
    !team.is_empty() && team.chars().all(|c| c.is_ascii_alphanumeric())
}

/// Saves a workspace's session; `Ok` once the keyring confirmed it.
pub fn store(team: &str, label: &str, creds: &Creds) -> Result<(), String> {
    if !valid(team) {
        return Err("bad team".into());
    }
    if known().lock().unwrap().get(team).is_some_and(|c| c == creds) {
        return Ok(());
    }
    let secret = serde_json::json!({ "token": creds.token, "cookie": creds.cookie.clone().unwrap_or_default() }).to_string();
    imp::store(team, label, &secret)?;
    known().lock().unwrap().insert(team.to_string(), creds.clone());
    Ok(())
}

/// A workspace's session, `None` when the keyring doesn't have it (or can't be reached).
pub fn lookup(team: &str) -> Option<Creds> {
    if !valid(team) {
        return None;
    }
    if let Some(c) = known().lock().unwrap().get(team) {
        return Some(c.clone());
    }
    let v: serde_json::Value = serde_json::from_str(&imp::lookup(team)?).ok()?;
    let token = v["token"].as_str().filter(|t| !t.is_empty())?.to_string();
    let cookie = v["cookie"].as_str().filter(|c| !c.is_empty()).map(String::from);
    let creds = Creds { token, cookie };
    known().lock().unwrap().insert(team.to_string(), creds.clone());
    Some(creds)
}

/// Signing out: the workspace's entry goes.
pub fn clear(team: &str) {
    known().lock().unwrap().remove(team);
    if valid(team) {
        imp::clear(team);
    }
}

/// Whether there's a keyring to try: `secret-tool` on the path.
pub fn available() -> bool {
    imp::available()
}

#[cfg(target_os = "linux")]
mod imp {
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    /// A locked keyring asks for its password first.
    const WAIT: Duration = Duration::from_secs(120);

    /// `secret-tool <verb> [extra] application slick team <id>`, with `input`
    /// on its stdin: whether it succeeded, its stdout and its stderr.
    fn run(verb: &str, extra: &[&str], team: &str, input: &str) -> Option<(bool, String, String)> {
        let mut child = Command::new("secret-tool")
            .arg(verb)
            .args(extra)
            .args(["application", "slick", "team", team])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .ok()?;
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(input.as_bytes());
        }
        let (mut out, mut err) = (child.stdout.take()?, child.stderr.take()?);
        let reader = std::thread::spawn(move || {
            let (mut o, mut e) = (String::new(), String::new());
            let _ = out.read_to_string(&mut o);
            let _ = err.read_to_string(&mut e);
            (o, e)
        });
        let start = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break s,
                Ok(None) if start.elapsed() < WAIT => std::thread::sleep(Duration::from_millis(20)),
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
            }
        };
        let (o, e) = reader.join().unwrap_or_default();
        Some((status.success(), o, e.trim().to_string()))
    }

    pub fn store(team: &str, label: &str, secret: &str) -> Result<(), String> {
        let label = format!("--label={label}");
        match run("store", &[&label], team, secret) {
            Some((true, _, _)) => Ok(()),
            Some((false, _, e)) if !e.is_empty() => Err(e),
            _ => Err("secret-tool store failed".into()),
        }
    }

    pub fn lookup(team: &str) -> Option<String> {
        run("lookup", &[], team, "").filter(|(ok, out, _)| *ok && !out.is_empty()).map(|(_, out, _)| out)
    }

    pub fn clear(team: &str) {
        let _ = run("clear", &[], team, "");
    }

    pub fn available() -> bool {
        std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join("secret-tool").is_file()))
    }
}

/// No keyring here: the config file keeps the sessions, as it always did.
#[cfg(not(target_os = "linux"))]
mod imp {
    pub fn store(_: &str, _: &str, _: &str) -> Result<(), String> {
        Err("no keyring".into())
    }

    pub fn lookup(_: &str) -> Option<String> {
        None
    }

    pub fn clear(_: &str) {}

    pub fn available() -> bool {
        false
    }
}
