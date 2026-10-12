//! What survives a restart, so the app opens with everything already on screen.

use crate::slack::{Boot, Conversation, Message, Section, Users};
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
}

pub fn dir() -> PathBuf {
    crate::config::cache_dir()
}

fn path() -> PathBuf {
    dir().join("cache.json")
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
    std::fs::read(path())
        .ok()
        .and_then(|b| serde_json::from_slice::<Cache>(&b).ok())
        .unwrap_or_default()
}

pub fn save(cache: Cache) {
    let p = path();
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_vec(&cache) {
        let tmp = p.with_extension("tmp");
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(tmp, p);
        }
    }
}
