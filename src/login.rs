//! Signing in through the window with Slack's own page (`login_window`), in
//! a child process: on Linux the `slick-login` helper this binary carries
//! (see build.rs), elsewhere this same binary.

use crate::i18n::tr;
use crate::slack::{AuthInfo, Creds, Res};
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// The hidden argument that runs the window instead of the app.
pub const ARG: &str = "__login-window";

/// The `slick-login` helper, built from login/ by build.rs.
#[cfg(target_os = "linux")]
const HELPER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/slick-login"));

/// The helper, written to the cache once per build (a folder named by its
/// hash), with older copies removed. The file itself is `slick-login`: GTK
/// takes the window's class from it.
#[cfg(target_os = "linux")]
fn program() -> Res<PathBuf> {
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::PermissionsExt;
    let hash: String = Sha256::digest(HELPER).iter().take(6).map(|b| format!("{b:02x}")).collect();
    let cache = crate::config::cache_dir();
    let dir = cache.join(format!("login-{hash}"));
    let path = dir.join("slick-login");
    if path.exists() {
        return Ok(path);
    }
    let write = || -> std::io::Result<()> {
        std::fs::create_dir_all(&cache)?;
        for e in std::fs::read_dir(&cache)?.flatten() {
            if e.file_name().to_string_lossy().starts_with("login-") {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
        std::fs::create_dir_all(&dir)?;
        let tmp = dir.join(".slick-login.tmp");
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

/// The sign-in window's HTTP cache (Slack's pages and scripts, ~100 MB),
/// useless once it's closed. Cookies and storage stay: they keep the window
/// signed in, so adding a workspace or signing in again skips the password.
/// Folders that aren't there or are still in use are left as they are.
pub fn purge_cache() {
    let dir = crate::config::data_dir().join("webview");
    // WebKitGTK; WebView2 keeps its profile in EBWebView.
    let caches: &[&str] = if cfg!(windows) {
        &[
            "EBWebView/Default/Cache",
            "EBWebView/Default/Code Cache",
            "EBWebView/Default/GPUCache",
            "EBWebView/Default/Service Worker/CacheStorage",
            "EBWebView/ShaderCache",
            "EBWebView/GrShaderCache",
        ]
    } else {
        &["WebKitCache", "CacheStorage"]
    };
    for c in caches {
        let _ = std::fs::remove_dir_all(dir.join(c));
    }
}

/// A workspace the browser is signed in to, from Slack's `localConfig_v2`.
#[allow(dead_code)]
#[derive(Clone, Debug, Default)]
pub struct Team {
    pub id: String,
    pub name: String,
    pub domain: String,
    pub token: String,
}

/// What the window brings back: the session it took, the workspace that
/// session is for, and every workspace the browser is signed in to.
#[allow(dead_code)]
pub struct Signed {
    pub creds: Creds,
    pub team: Option<String>,
    pub teams: Vec<Team>,
}

/// The window, leaving out the workspaces in `known` (their ids): with some,
/// it opens on Slack's sign-in page (unless `workspace` names one) and only
/// takes a workspace that is not among them. Empty is the first sign-in.
pub fn window_known(workspace: Option<&str>, known: &[String]) -> Res<Signed> {
    let out = Command::new(program()?)
        .arg(ARG)
        .args(workspace)
        .env("SLICK_LANG", crate::i18n::lang().code())
        .env("SLICK_KNOWN_TEAMS", known.join(","))
        .stdin(Stdio::null())
        .output()
        .map_err(|e| trf!("couldn't open the sign-in window: {e}", "não abriu a janela de login: {e}"))?;
    // The window is gone, and its web processes with it.
    std::thread::spawn(purge_cache);
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
    let s = |t: &serde_json::Value, k: &str| t[k].as_str().unwrap_or("").to_string();
    let teams = v["teams"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|t| Team { id: s(t, "id"), name: s(t, "name"), domain: s(t, "domain"), token: s(t, "token") })
        .filter(|t| !t.id.is_empty() && !t.token.is_empty())
        .collect();
    let team = v["team"].as_str().filter(|t| !t.is_empty()).map(String::from);
    Ok(Signed { creds: Creds { token, cookie }, team, teams })
}

/// Signing in, checked: the window (leaving out `known`, as `window_known`)
/// and then `auth.test` on the session it brought. Blocks until the window
/// closes, so it runs off the interface's thread. "Add a workspace" passes
/// the ids of the ones slick has; signing in again to one of them passes the
/// others; the first sign-in passes none.
#[allow(dead_code)]
pub fn sign_in(workspace: Option<&str>, known: &[String]) -> Res<(AuthInfo, Creds)> {
    let signed = window_known(workspace, known)?;
    let me = crate::slack::Slack::new(signed.creds.clone()).auth()?;
    Ok((me, signed.creds))
}

/// `slick login [workspace]`: the sign-in window, then the session saved
/// to the config, as a workspace added or one signed in to again.
#[allow(dead_code)]
pub fn cli(workspace: Option<&str>) -> i32 {
    let saved = crate::config::workspaces();
    let workspace = workspace.and_then(crate::config::workspace_name);
    // Signing in again to a workspace slick has: the window may take it; the others it leaves.
    let known: Vec<String> = match &workspace {
        Some(w) => saved.iter().filter(|c| c.domain.as_deref() != Some(w.as_str())).filter_map(|c| c.team_id.clone()).collect(),
        None => Vec::new(),
    };
    // Without one, the workspace last signed in to, as before.
    let workspace = workspace.or_else(crate::config::workspace);
    eprintln!("{}", tr("Opening the Slack sign-in window…", "Abrindo a janela de login do Slack…"));
    match sign_in(workspace.as_deref(), &known) {
        Ok((me, creds)) => match crate::config::save_workspace(&me, &creds) {
            Ok(()) => {
                eprintln!("{}", trf!("Signed in: {} · {}", "Entrou: {} · {}", me.team, me.user));
                0
            }
            Err(e) => {
                let p = crate::config::path();
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
