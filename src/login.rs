//! Signing in through the window with Slack's own page (`login_window`),
//! run by this same binary in a child process.

use crate::slack::{Creds, Res};
use std::process::{Command, Stdio};

/// Opens the window and waits for the person to sign in. With a workspace
/// it opens on that workspace's email-and-password form.
pub fn window(workspace: Option<&str>) -> Res<Creds> {
    let exe = crate::update::exe().ok_or("não achou o executável do slick")?;
    let out = Command::new(exe)
        .arg(crate::login_window::ARG)
        .args(workspace)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| format!("não abriu a janela de login: {e}"))?;
    if !out.status.success() {
        return Err("Janela fechada sem entrar.".into());
    }
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).map_err(|_| "a janela de login não devolveu a sessão".to_string())?;
    let token = v["token"].as_str().unwrap_or("").to_string();
    let cookie = v["cookie"].as_str().filter(|c| !c.is_empty()).map(String::from);
    if token.is_empty() || cookie.is_none() {
        return Err("a janela de login não devolveu a sessão".into());
    }
    Ok(Creds { token, cookie })
}
