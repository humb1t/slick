//! Credentials: `SLACK_TOKEN`/`SLACK_COOKIE`, else `~/.config/slick/config`
//! (`%APPDATA%\slick\config` on Windows), which also remembers the workspace
//! last signed in to.

use crate::slack::Creds;
use std::io::Write;
use std::path::PathBuf;

/// The home directory: `$HOME`, else `%USERPROFILE%` (Windows).
pub fn home() -> PathBuf {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from).unwrap_or_default()
}

/// The folder in `$var` (`$XDG_*_HOME`, `%APPDATA%`…), else this under the home directory.
fn xdg(var: &str, fallback: &str) -> PathBuf {
    std::env::var_os(var).filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| home().join(fallback))
}

/// slick's config folder; `%APPDATA%\slick` on Windows.
pub fn config_dir() -> PathBuf {
    if cfg!(windows) { xdg("APPDATA", "AppData/Roaming") } else { xdg("XDG_CONFIG_HOME", ".config") }.join("slick")
}

/// slick's cache folder; `%LOCALAPPDATA%\slick` on Windows.
pub fn cache_dir() -> PathBuf {
    if cfg!(windows) { xdg("LOCALAPPDATA", "AppData/Local") } else { xdg("XDG_CACHE_HOME", ".cache") }.join("slick")
}

/// slick's data folder (the sign-in window's profile); `%LOCALAPPDATA%\slick` on Windows.
pub fn data_dir() -> PathBuf {
    if cfg!(windows) { xdg("LOCALAPPDATA", "AppData/Local") } else { xdg("XDG_DATA_HOME", ".local/share") }.join("slick")
}

pub fn path() -> PathBuf {
    config_dir().join("config")
}

/// The app used to be called fastslack: its config, cache and sign-in window
/// profile move to the new name once, so nobody has to sign in again.
#[cfg(unix)]
pub fn migrate() {
    for (var, fallback) in [("XDG_CONFIG_HOME", ".config"), ("XDG_CACHE_HOME", ".cache"), ("XDG_DATA_HOME", ".local/share")] {
        let base = xdg(var, fallback);
        let (old, new) = (base.join("fastslack"), base.join("slick"));
        if new.exists() || !old.is_dir() {
            continue;
        }
        // Another filesystem: copy, then drop the old one.
        if std::fs::rename(&old, &new).is_err() && copy_dir(&old, &new).is_ok() {
            let _ = std::fs::remove_dir_all(&old);
        }
    }
}

#[cfg(unix)]
fn copy_dir(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let target = to.join(e.file_name());
        let kind = e.file_type()?;
        if kind.is_dir() {
            copy_dir(&e.path(), &target)?;
        } else if kind.is_symlink() {
            std::os::unix::fs::symlink(std::fs::read_link(e.path())?, &target)?;
        } else {
            std::fs::copy(e.path(), &target)?;
        }
    }
    Ok(())
}

fn value(key: &str) -> Option<String> {
    let text = std::fs::read_to_string(path()).ok()?;
    text.lines().find_map(|line| {
        let (k, v) = line.split_once('=')?;
        (k.trim() == key).then(|| v.trim().trim_matches('"').to_string()).filter(|v| !v.is_empty())
    })
}

/// The workspace to sign in to: `acme`, from what someone typed
/// (`acme`, `acme.slack.com`, `https://acme.slack.com/`).
pub fn workspace_name(s: &str) -> Option<String> {
    let s = s.trim().trim_start_matches("https://").trim_start_matches("http://");
    let s = s.split('/').next().unwrap_or("").to_lowercase();
    let s = s.strip_suffix(".slack.com").unwrap_or(&s);
    (!s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')).then(|| s.to_string())
}

pub fn workspace() -> Option<String> {
    value("workspace")
}

/// Sidebar width and whether it shows photos; `None` when never changed.
pub fn sidebar_width() -> Option<u16> {
    value("sidebar_width")?.parse().ok()
}

/// Looking for new versions at start-up and every 6h; `auto_update = false` turns it off.
pub fn auto_update() -> bool {
    value("auto_update").is_none_or(|v| v != "false")
}

/// The system tray icon; `tray = false` turns it off.
#[cfg(target_os = "linux")]
pub fn tray() -> bool {
    value("tray").is_none_or(|v| v != "false")
}

/// The interface language: `language = "pt-BR"` or `"en-US"`.
pub fn language() -> Option<crate::i18n::Lang> {
    crate::i18n::Lang::parse(&value("language")?)
}

pub fn sidebar_avatars() -> Option<bool> {
    value("sidebar_avatars")?.parse().ok()
}

/// One signed-in workspace, as the config has it.
#[allow(dead_code)]
#[derive(Clone)]
pub struct WsConf {
    /// `None` until the first `auth.test` writes it (a config from before multi-workspace).
    pub team_id: Option<String>,
    /// The `acme` in acme.slack.com.
    pub domain: Option<String>,
    pub creds: Creds,
}

/// The legacy workspace's id (`team`): its session lives in the plain
/// `token`/`cookie`/`workspace` keys, the others' in `token.<id>`…
#[allow(dead_code)]
pub fn team() -> Option<String> {
    value("team")
}

/// The last workspace on screen, reopened at start.
#[allow(dead_code)]
pub fn active() -> Option<String> {
    value("active")
}

#[allow(dead_code)]
pub fn set_active(id: &str) -> std::io::Result<()> {
    set(&[("active", id.to_string())])
}

/// The `workspaces` list, in the order they were added.
fn ids() -> Vec<String> {
    value("workspaces").map(|v| v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()).unwrap_or_default()
}

/// Every workspace signed in to, in the order they were added (Ctrl+1..9).
/// `SLACK_TOKEN` in the environment means that one alone; a config without
/// `workspaces` is the single legacy one.
#[allow(dead_code)]
pub fn workspaces() -> Vec<WsConf> {
    if let Some(token) = std::env::var("SLACK_TOKEN").ok().filter(|t| !t.is_empty()) {
        let cookie = std::env::var("SLACK_COOKIE").ok().filter(|c| !c.is_empty());
        return vec![WsConf { team_id: None, domain: workspace(), creds: Creds { token, cookie } }];
    }
    let (legacy, shared) = (team(), value("cookie"));
    let mut ids = ids();
    // The legacy one comes first unless the list puts it elsewhere.
    if value("token").is_some() && !legacy.as_ref().is_some_and(|t| ids.contains(t)) {
        ids.insert(0, legacy.clone().unwrap_or_default());
    }
    ids.into_iter()
        .filter_map(|id| {
            let is_legacy = id.is_empty() || legacy.as_deref() == Some(id.as_str());
            let key = |k: &str| if is_legacy { k.to_string() } else { format!("{k}.{id}") };
            let token = value(&key("token"))?;
            let cookie = value(&key("cookie")).or_else(|| shared.clone());
            let team_id = (!id.is_empty()).then_some(id.clone());
            Some(WsConf { team_id, domain: value(&key("workspace")), creds: Creds { token, cookie } })
        })
        .collect()
}

/// Saves a workspace's session after `auth.test`: the legacy one (or the
/// first ever) in the plain keys, any other in `token.<id>`… and the list.
/// Its cookie is only written apart when it isn't the shared one.
#[allow(dead_code)]
pub fn save_workspace(me: &crate::slack::AuthInfo, creds: &Creds) -> std::io::Result<()> {
    let id = me.team_id.as_str();
    let legacy = team();
    let legacy_token = value("token");
    // Without `team` yet (a config from before), the plain keys are this
    // workspace's when they hold its token or its address: a new session
    // for the same workspace replaces them instead of adding it twice.
    let is_legacy = match &legacy {
        Some(t) => t == id,
        None => {
            legacy_token.is_none() || legacy_token.as_deref() == Some(creds.token.as_str()) || workspace().as_deref() == Some(me.domain.as_str())
        }
    };
    let mut ids = ids();
    let mut v = Vec::new();
    if is_legacy {
        v.push(("token".to_string(), creds.token.clone()));
        v.push(("cookie".to_string(), creds.cookie.clone().unwrap_or_default()));
        v.push(("workspace".to_string(), me.domain.clone()));
        v.push(("team".to_string(), id.to_string()));
    } else {
        let own = creds.cookie.clone().filter(|c| value("cookie").as_ref() != Some(c)).unwrap_or_default();
        v.push((format!("token.{id}"), creds.token.clone()));
        v.push((format!("cookie.{id}"), own));
        v.push((format!("workspace.{id}"), me.domain.clone()));
        // The first other one starts the list, with the legacy one ahead of it.
        if ids.is_empty() {
            ids.extend(legacy.filter(|_| legacy_token.is_some()));
        }
    }
    if !ids.is_empty() || !is_legacy {
        if !ids.iter().any(|i| i == id) {
            ids.push(id.to_string());
        }
        v.push(("workspaces".to_string(), ids.join(", ")));
    }
    let v: Vec<(&str, String)> = v.iter().map(|(k, val)| (k.as_str(), val.clone())).collect();
    set(&v)
}

/// A workspace keeps this cookie as its own (`cookie.<id>`): the shared
/// one changed with a new sign-in and Slack refused it here. The legacy
/// workspace has no key of its own; it always follows the shared one.
#[allow(dead_code)]
pub fn pin_cookie(id: &str, cookie: &str) -> std::io::Result<()> {
    if team().as_deref() == Some(id) {
        return Ok(());
    }
    set(&[(format!("cookie.{id}").as_str(), cookie.to_string())])
}

/// Signing out of one workspace: its session goes, the others stay. The
/// shared cookie and the domain go with the last one only, like `logout`.
#[allow(dead_code)]
pub fn remove_workspace(id: &str) -> std::io::Result<()> {
    let ids: Vec<String> = ids().into_iter().filter(|i| i != id).collect();
    let mut v = vec![("workspaces".to_string(), ids.join(", "))];
    if team().as_deref() == Some(id) {
        v.push(("token".to_string(), String::new()));
        v.push(("team".to_string(), String::new()));
        if ids.is_empty() {
            v.push(("cookie".to_string(), String::new()));
        }
    } else {
        for k in ["token", "cookie", "workspace"] {
            v.push((format!("{k}.{id}"), String::new()));
        }
    }
    if active().as_deref() == Some(id) {
        v.push(("active".to_string(), String::new()));
    }
    let v: Vec<(&str, String)> = v.iter().map(|(k, val)| (k.as_str(), val.clone())).collect();
    set(&v)
}

/// Signing out: the session goes, the workspace and the sidebar's look stay.
pub fn logout() -> std::io::Result<()> {
    set(&[("token", String::new()), ("cookie", String::new())])
}

/// Replaces these keys and keeps the rest. Written readable by the owner only.
pub fn set(values: &[(&str, String)]) -> std::io::Result<()> {
    let p = path();
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let old = std::fs::read_to_string(&p).unwrap_or_default();
    let mut text: String = old
        .lines()
        .filter(|l| !l.split_once('=').is_some_and(|(k, _)| values.iter().any(|(key, _)| k.trim() == *key)))
        .map(|l| format!("{l}\n"))
        .collect();
    for (k, v) in values {
        if !v.is_empty() {
            text.push_str(&format!("{k} = {v}\n"));
        }
    }
    let tmp = p.with_extension("tmp");
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    // Windows: %APPDATA% is the user's own already.
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    let mut f = opts.open(&tmp)?;
    f.write_all(text.as_bytes())?;
    std::fs::rename(tmp, p)
}
