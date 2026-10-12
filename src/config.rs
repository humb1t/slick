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
#[cfg(not(target_os = "linux"))]
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

pub fn load() -> Option<Creds> {
    let mut token = std::env::var("SLACK_TOKEN").ok();
    let mut cookie = std::env::var("SLACK_COOKIE").ok();
    if token.is_none() {
        token = value("token");
        cookie = value("cookie");
    }
    Some(Creds { token: token.filter(|t| !t.is_empty())?, cookie: cookie.filter(|c| !c.is_empty()) })
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

pub fn save(creds: &Creds, workspace: &str) -> std::io::Result<()> {
    let mut v = vec![("token", creds.token.clone()), ("cookie", creds.cookie.clone().unwrap_or_default())];
    if !workspace.is_empty() {
        v.push(("workspace", workspace.to_string()));
    }
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
