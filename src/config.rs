//! Credentials: `SLACK_TOKEN`/`SLACK_COOKIE`, else the system keyring
//! (`keyring`), else `~/.config/slick/config` (`%APPDATA%\slick\config` on
//! Windows). The config lists the workspaces and remembers the one last
//! signed in to; it keeps their sessions itself only where there's no keyring.

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
    // An Enterprise Grid org's own address is `acme.enterprise`; no other dot is one.
    let (name, org) = s.strip_suffix(".enterprise").map_or((s, ""), |n| (n, ".enterprise"));
    (!name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')).then(|| format!("{name}{org}"))
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

/// Desktop notifications for DMs, mentions and replies; `notifications = false` turns them off.
#[cfg(target_os = "linux")]
pub fn notifications() -> bool {
    value("notifications").is_none_or(|v| v != "false")
}

/// The sound a notification plays: a sound theme name (`message-new-instant`), none by default.
#[cfg(target_os = "linux")]
pub fn notification_sound() -> Option<String> {
    value("notification_sound").filter(|v| v != "none")
}

/// The interface language: `language = "pt-BR"` or `"en-US"`.
pub fn language() -> Option<crate::i18n::Lang> {
    crate::i18n::Lang::parse(&value("language")?)
}

/// The clock: `clock = "24h"` (the default) or `"12h"`; `None` when never set.
pub fn clock_h12() -> Option<bool> {
    match value("clock")?.as_str() {
        "12h" => Some(true),
        "24h" => Some(false),
        _ => None,
    }
}

/// Messages back to back: `message_spacing = "compact"`; `"comfortable"`
/// (the default) leaves a blank row between them.
pub fn compact_messages() -> bool {
    value("message_spacing").is_some_and(|v| v == "compact")
}

/// How unread conversations stand out in the sidebar, besides bold: a bar on
/// the left (`unread_bar = false` turns it off) and a tinted row
/// (`unread_background = true` turns it on).
pub fn unread_bar() -> bool {
    value("unread_bar").is_none_or(|v| v != "false")
}

pub fn unread_background() -> bool {
    value("unread_background").is_some_and(|v| v == "true")
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
    list("workspaces")
}

/// The workspaces whose session is in the system keyring (`keyring`), not here.
fn vaulted() -> Vec<String> {
    list("keyring")
}

fn list(key: &str) -> Vec<String> {
    value(key).map(|v| v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()).unwrap_or_default()
}

/// The legacy workspace has a session: in the plain keys or in the keyring.
fn has_legacy() -> bool {
    value("token").is_some() || team().is_some_and(|t| vaulted().contains(&t))
}

/// A workspace's entry in the keyring is named after its address.
fn label(domain: Option<&str>, id: &str) -> String {
    format!("slick: {}", domain.filter(|d| !d.is_empty()).unwrap_or(id))
}

/// Whether a session left in the file still follows the shared `cookie`:
/// the legacy one, or another without a cookie of its own. `moved` are
/// those going to the keyring now.
fn shared_cookie_used(moved: &[String]) -> bool {
    let (legacy, vaulted) = (team(), vaulted());
    let gone = |id: &String| moved.contains(id) || vaulted.contains(id);
    let legacy_here = value("token").is_some() && !legacy.as_ref().is_some_and(gone);
    legacy_here
        || ids().iter().any(|id| {
            legacy.as_ref() != Some(id) && !gone(id) && value(&format!("token.{id}")).is_some() && value(&format!("cookie.{id}")).is_none()
        })
}

/// Every workspace signed in to, in the order they were added (Ctrl+1..9).
/// `SLACK_TOKEN` in the environment means that one alone; a config without
/// `workspaces` is the single legacy one.
#[allow(dead_code)]
pub fn workspaces() -> Vec<WsConf> {
    #[cfg(feature = "demo")]
    if crate::demo::on() {
        return crate::demo::workspaces();
    }
    if let Some(token) = std::env::var("SLACK_TOKEN").ok().filter(|t| !t.is_empty()) {
        let cookie = std::env::var("SLACK_COOKIE").ok().filter(|c| !c.is_empty());
        return vec![WsConf { team_id: None, domain: workspace(), creds: Creds { token, cookie } }];
    }
    let (legacy, shared, vaulted) = (team(), value("cookie"), vaulted());
    let mut ids = ids();
    // The legacy one comes first unless the list puts it elsewhere.
    if has_legacy() && !legacy.as_ref().is_some_and(|t| ids.contains(t)) {
        ids.insert(0, legacy.clone().unwrap_or_default());
    }
    ids.into_iter()
        .filter_map(|id| {
            let is_legacy = id.is_empty() || legacy.as_deref() == Some(id.as_str());
            let key = |k: &str| if is_legacy { k.to_string() } else { format!("{k}.{id}") };
            let creds = if !id.is_empty() && vaulted.contains(&id) {
                // Unreadable (the keyring locked and its prompt dismissed, the
                // entry deleted): an empty session asks to sign in again.
                crate::keyring::lookup(&id).unwrap_or_default()
            } else {
                let token = value(&key("token"))?;
                Creds { token, cookie: value(&key("cookie")).or_else(|| shared.clone()) }
            };
            let team_id = (!id.is_empty()).then_some(id.clone());
            Some(WsConf { team_id, domain: value(&key("workspace")), creds })
        })
        .collect()
}

/// Saves a workspace's session after `auth.test`: in the keyring when
/// there's one, else the legacy one (or the first ever) in the plain keys,
/// any other in `token.<id>`… Its address and the list go in the config
/// either way. Its cookie is only written apart when it isn't the shared one.
#[allow(dead_code)]
pub fn save_workspace(me: &crate::slack::AuthInfo, creds: &Creds) -> std::io::Result<()> {
    let id = me.team_id.as_str();
    let legacy = team();
    let legacy_token = value("token");
    let legacy_here = has_legacy();
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
    let mut vaulted = vaulted();
    // A new shared cookie: the others in the keyring that followed the old
    // one take it, as those in the file do (`workspaces::save_session`).
    if is_legacy {
        follow_shared(id, creds);
    }
    let stored = crate::keyring::store(id, &label(Some(&me.domain), id), creds).is_ok();
    let mut v = Vec::new();
    if is_legacy {
        // The file keeps the shared cookie while a session there follows it.
        let shared = if stored && !shared_cookie_used(&[id.to_string()]) { String::new() } else { creds.cookie.clone().unwrap_or_default() };
        v.push(("token".to_string(), if stored { String::new() } else { creds.token.clone() }));
        v.push(("cookie".to_string(), shared));
        v.push(("workspace".to_string(), me.domain.clone()));
        v.push(("team".to_string(), id.to_string()));
    } else {
        let own = creds.cookie.clone().filter(|c| !stored && value("cookie").as_ref() != Some(c)).unwrap_or_default();
        v.push((format!("token.{id}"), if stored { String::new() } else { creds.token.clone() }));
        v.push((format!("cookie.{id}"), own));
        v.push((format!("workspace.{id}"), me.domain.clone()));
        // The first other one starts the list, with the legacy one ahead of it.
        if ids.is_empty() {
            ids.extend(legacy.filter(|_| legacy_here));
        }
    }
    if !stored {
        vaulted.retain(|v| v != id);
    } else if !vaulted.iter().any(|v| v == id) {
        vaulted.push(id.to_string());
    }
    v.push(("keyring".to_string(), vaulted.join(", ")));
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
    if vaulted().iter().any(|v| v == id) {
        let Some(mut creds) = crate::keyring::lookup(id) else { return Ok(()) };
        creds.cookie = Some(cookie.to_string());
        return crate::keyring::store(id, &label(value(&format!("workspace.{id}")).as_deref(), id), &creds).map_err(std::io::Error::other);
    }
    set(&[(format!("cookie.{id}").as_str(), cookie.to_string())])
}

/// The legacy workspace `id` signs in again with `creds`: the others in the
/// keyring still on its old cookie (the shared one) get the new one.
fn follow_shared(id: &str, creds: &Creds) {
    let vaulted = vaulted();
    let old = if vaulted.iter().any(|v| v == id) { crate::keyring::lookup(id).and_then(|c| c.cookie) } else { value("cookie") };
    let Some(old) = old.filter(|o| creds.cookie.as_ref() != Some(o)) else { return };
    for other in vaulted.iter().filter(|v| *v != id) {
        if let Some(mut c) = crate::keyring::lookup(other).filter(|c| c.cookie.as_ref() == Some(&old)) {
            c.cookie = creds.cookie.clone();
            let _ = crate::keyring::store(other, &label(value(&format!("workspace.{other}")).as_deref(), other), &c);
        }
    }
}

/// The first start with a keyring moves the sessions in the file there.
/// The config is rewritten only for those the keyring confirmed; without
/// one, nothing changes.
pub fn move_to_keyring() {
    if std::env::var("SLACK_TOKEN").is_ok_and(|t| !t.is_empty()) || !crate::keyring::available() {
        return;
    }
    let (legacy, vaulted) = (team(), vaulted());
    // The legacy one without its id yet moves after its first `auth.test`.
    let pending: Vec<WsConf> = workspaces()
        .into_iter()
        .filter(|w| w.team_id.as_ref().is_some_and(|id| !vaulted.contains(id)) && !w.creds.token.is_empty())
        .collect();
    let (mut moved, mut v) = (Vec::new(), Vec::new());
    for w in pending {
        let id = w.team_id.unwrap_or_default();
        if crate::keyring::store(&id, &label(w.domain.as_deref(), &id), &w.creds).is_err() {
            // No keyring after all: the rest stays in the file too.
            break;
        }
        if legacy.as_deref() == Some(id.as_str()) {
            v.push(("token".to_string(), String::new()));
        } else {
            v.push((format!("token.{id}"), String::new()));
            v.push((format!("cookie.{id}"), String::new()));
        }
        moved.push(id);
    }
    if moved.is_empty() {
        return;
    }
    if !shared_cookie_used(&moved) {
        v.push(("cookie".to_string(), String::new()));
    }
    v.push(("keyring".to_string(), vaulted.into_iter().chain(moved).collect::<Vec<_>>().join(", ")));
    let v: Vec<(&str, String)> = v.iter().map(|(k, val)| (k.as_str(), val.clone())).collect();
    let _ = set(&v);
}

/// Signing out of one workspace: its session goes, the others stay. The
/// shared cookie and the domain go with the last one only, like `logout`.
#[allow(dead_code)]
pub fn remove_workspace(id: &str) -> std::io::Result<()> {
    let ids: Vec<String> = ids().into_iter().filter(|i| i != id).collect();
    let mut v = vec![("workspaces".to_string(), ids.join(", "))];
    let vaulted = vaulted();
    if vaulted.iter().any(|v| v == id) {
        crate::keyring::clear(id);
        v.push(("keyring".to_string(), vaulted.iter().filter(|v| *v != id).cloned().collect::<Vec<_>>().join(", ")));
    }
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
    let mut v = vec![("token", String::new()), ("cookie", String::new())];
    let vaulted = vaulted();
    if let Some(t) = team().filter(|t| vaulted.contains(t)) {
        crate::keyring::clear(&t);
        v.push(("keyring", vaulted.iter().filter(|v| **v != t).cloned().collect::<Vec<_>>().join(", ")));
    }
    set(&v)
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
