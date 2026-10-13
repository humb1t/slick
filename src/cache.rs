//! What survives a restart, so the app opens with everything already on screen.

use crate::slack::{Boot, Conversation, Directory, Message, Section, Users};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

#[derive(Default, Serialize, Deserialize)]
pub struct Cache {
    /// The user it belongs to; another account's login discards it.
    #[serde(default)]
    pub owner: String,
    pub users: Users,
    pub convs: Vec<Conversation>,
    #[serde(default)]
    pub sections: Vec<Section>,
    #[serde(default)]
    pub stars: Vec<String>,
    #[serde(default)]
    pub collapsed: HashSet<String>,
    pub recents: Vec<String>,
    /// The conversation open at quit, reopened at start like it was left.
    #[serde(default)]
    pub current: Option<String>,
    pub messages: HashMap<String, Vec<Message>>,
    #[serde(default)]
    pub boot: Option<Boot>,
    #[serde(default)]
    pub emoji: crate::emoji::Emoji,
    /// The workspace's slash commands, for the `/` autocomplete.
    #[serde(default)]
    pub commands: Vec<crate::slash::Command>,
}

pub fn dir() -> PathBuf {
    crate::config::cache_dir()
}

/// A workspace's own cache, `cache-<team_id>.json`; `cache.json` while its id is unknown.
fn path(team: Option<&str>) -> PathBuf {
    dir().join(team.map_or_else(|| "cache.json".to_string(), |t| format!("cache-{t}.json")))
}

pub fn image_path(file_id: &str) -> PathBuf {
    dir().join("img").join(file_id)
}

/// Images unseen for this long are deleted at start-up.
const IMAGE_TTL: std::time::Duration = std::time::Duration::from_secs(7 * 86400);

/// Marks an image as just seen: its modification time is its last use, so
/// what keeps being looked at never expires.
pub fn touch(path: &std::path::Path) {
    if let Ok(f) = std::fs::File::options().write(true).open(path) {
        let _ = f.set_modified(std::time::SystemTime::now());
    }
}

/// Deletes the images nobody has looked at for `IMAGE_TTL`.
pub fn prune_images() {
    let Ok(entries) = std::fs::read_dir(dir().join("img")) else { return };
    let now = std::time::SystemTime::now();
    for e in entries.flatten() {
        let old = e.metadata().and_then(|m| m.modified()).is_ok_and(|t| now.duration_since(t).unwrap_or_default() > IMAGE_TTL);
        if old {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

pub fn load() -> Cache {
    load_for(None)
}

/// A workspace's cache. The legacy `cache.json` becomes the first
/// workspace's (`config::team`) the first time it is asked for by id; the
/// app still drops it at `auth.test` if its owner is someone else.
pub fn load_for(team: Option<&str>) -> Cache {
    let p = path(team);
    if let Some(t) = team {
        let legacy = path(None);
        let first = crate::config::team().as_deref() == Some(t);
        if !p.exists() && legacy.exists() && first {
            let _ = std::fs::rename(&legacy, &p);
        }
    }
    std::fs::read(p)
        .ok()
        .and_then(|b| serde_json::from_slice::<Cache>(&b).ok())
        .unwrap_or_default()
}

/// The workspace's public channels (`slack::Directory`), `channels-<team_id>.json`:
/// apart from the cache, which is written far more often.
fn directory_path(team: &str) -> PathBuf {
    dir().join(format!("channels-{team}.json"))
}

pub fn load_directory(team: &str) -> Directory {
    let mut d = std::fs::read(directory_path(team))
        .ok()
        .and_then(|b| serde_json::from_slice::<Directory>(&b).ok())
        .unwrap_or_default();
    d.prepare();
    d
}

pub fn save_directory(team: &str, d: &Directory) {
    let p = directory_path(team);
    let _ = std::fs::create_dir_all(dir());
    if let Ok(json) = serde_json::to_vec(d) {
        let tmp = p.with_extension("tmp");
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(tmp, p);
        }
    }
}

pub fn save_for(team: Option<&str>, cache: Cache) {
    let p = path(team);
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_vec(&cache) {
        let tmp = p.with_extension("tmp");
        if std::fs::write(&tmp, json).is_ok() && std::fs::rename(tmp, p).is_ok() {
            // Saved under its id: the legacy file would only go stale.
            if team.is_some_and(|t| crate::config::team().as_deref() == Some(t)) {
                let _ = std::fs::remove_file(path(None));
            }
        }
    }
}
