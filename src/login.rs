//! Signing in through the window with Slack's own page (`login_window`), in
//! a child process: on Linux the `slick-login` helper this binary carries
//! (see build.rs), elsewhere this same binary.

use crate::i18n::tr;
use crate::slack::{Creds, Res};
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// The hidden argument that runs the window instead of the app.
pub const ARG: &str = "__login-window";

/// The `slick-login` helper, built from login/ by build.rs.
#[cfg(target_os = "linux")]
const HELPER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/slick-login"));

/// The helper, written to the cache once per build (named by its hash), with
/// older copies removed.
#[cfg(target_os = "linux")]
fn program() -> Res<PathBuf> {
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::PermissionsExt;
    let hash: String = Sha256::digest(HELPER).iter().take(6).map(|b| format!("{b:02x}")).collect();
    let dir = crate::config::cache_dir();
    let path = dir.join(format!("slick-login-{hash}"));
    if path.exists() {
        return Ok(path);
    }
    let write = || -> std::io::Result<()> {
        std::fs::create_dir_all(&dir)?;
        for e in std::fs::read_dir(&dir)?.flatten() {
            if e.file_name().to_string_lossy().starts_with("slick-login-") {
                let _ = std::fs::remove_file(e.path());
            }
        }
        let tmp = dir.join(format!(".slick-login-{hash}.tmp"));
        std::fs::write(&tmp, HELPER)?;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
        std::fs::rename(&tmp, &path)
    };
    write().map_err(|e| trf!("couldn't set up the sign-in window: {e}", "não preparou a janela de login: {e}"))?;
    Ok(path)
}

#[cfg(not(target_os = "linux"))]
fn program() -> Res<PathBuf> {
    Ok(crate::update::exe().ok_or(tr("couldn't find slick's executable", "não achou o executável do slick"))?)
}

/// Opens the window and waits for the person to sign in. With a workspace
/// it opens on that workspace's email-and-password form.
pub fn window(workspace: Option<&str>) -> Res<Creds> {
    let out = Command::new(program()?)
        .arg(ARG)
        .args(workspace)
        .env("SLICK_LANG", crate::i18n::lang().code())
        .stdin(Stdio::null())
        .output()
        .map_err(|e| trf!("couldn't open the sign-in window: {e}", "não abriu a janela de login: {e}"))?;
    if !out.status.success() {
        // The loader couldn't find WebKitGTK (or GTK): the window never opened.
        if String::from_utf8_lossy(&out.stderr).contains("error while loading shared libraries") {
            return Err(tr(
                "The sign-in window needs WebKitGTK 4.1. Install it and try again:\n\
                 webkit2gtk-4.1 (Arch) · libwebkit2gtk-4.1-0 (Debian/Ubuntu) · webkit2gtk4.1 (Fedora)\n\
                 Or paste a token (xoxc-…) here instead.",
                "A janela de login precisa do WebKitGTK 4.1. Instale e tente de novo:\n\
                 webkit2gtk-4.1 (Arch) · libwebkit2gtk-4.1-0 (Debian/Ubuntu) · webkit2gtk4.1 (Fedora)\n\
                 Ou cole um token (xoxc-…) aqui.",
            )
            .into());
        }
        return Err(tr("Window closed without signing in.", "Janela fechada sem entrar.").into());
    }
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).map_err(|_| tr("the sign-in window didn't return the session", "a janela de login não devolveu a sessão").to_string())?;
    let token = v["token"].as_str().unwrap_or("").to_string();
    let cookie = v["cookie"].as_str().filter(|c| !c.is_empty()).map(String::from);
    if token.is_empty() || cookie.is_none() {
        return Err(tr("the sign-in window didn't return the session", "a janela de login não devolveu a sessão").into());
    }
    Ok(Creds { token, cookie })
}
