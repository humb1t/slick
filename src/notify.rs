//! Desktop notifications (freedesktop's, over D-Bus) for DMs, mentions and
//! replies in threads you follow, as each conversation's level and the
//! pause allow, and the sound they play. Clicking one opens its
//! conversation, read there as any other. Without a notification server it
//! quietly does nothing.

use crate::app::{App, Event};
use crate::i18n::tr;
use serde_json::Value;
use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, OnceLock};

/// The sounds the settings offer: freedesktop sound theme names, with their labels.
pub const SOUNDS: [(&str, &str, &str); 5] = [
    ("message-new-instant", "Instant message", "Mensagem instantânea"),
    ("message", "Message", "Mensagem"),
    ("bell", "Bell", "Sino"),
    ("complete", "Complete", "Concluído"),
    ("dialog-information", "Information", "Informação"),
];

static ON: AtomicBool = AtomicBool::new(true);

fn sound_slot() -> &'static Mutex<Option<String>> {
    static S: OnceLock<Mutex<Option<String>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(crate::config::notification_sound()))
}

/// At start-up, from the config.
pub fn init() {
    ON.store(crate::config::notifications(), Ordering::Relaxed);
}

pub fn enabled() -> bool {
    ON.load(Ordering::Relaxed)
}

pub fn set_enabled(on: bool) {
    ON.store(on, Ordering::Relaxed);
}

/// The sound theme name played with each notification; `None` for silence.
pub fn sound() -> Option<String> {
    sound_slot().lock().unwrap().clone()
}

pub fn set_sound(name: Option<String>) {
    *sound_slot().lock().unwrap() = name;
}

/// Where a notification leads when clicked.
struct Target {
    team: String,
    conv: String,
    ts: String,
    thread: Option<String>,
}

struct Note {
    target: Target,
    title: String,
    body: String,
}

fn worker() -> &'static OnceLock<Sender<Note>> {
    static TX: OnceLock<Sender<Note>> = OnceLock::new();
    &TX
}

/// Shows notifications from a thread of its own, so a slow bus never holds
/// up the interface; a click comes back as `slick open` would.
pub fn start(events: Sender<Event>) {
    let (tx, rx) = mpsc::channel::<Note>();
    if worker().set(tx).is_err() {
        return;
    }
    std::thread::spawn(move || {
        let targets: Arc<Mutex<HashMap<u32, Target>>> = Default::default();
        // Connected at the first notification, and again while it fails.
        let mut proxy = None;
        for note in rx {
            if let Some(s) = sound() {
                play(&s);
            }
            if proxy.is_none() {
                proxy = connect(targets.clone(), events.clone());
            }
            let Some(p) = &proxy else { continue };
            let hints: HashMap<&str, zbus::zvariant::Value> = HashMap::from([("category", "im.received".into())]);
            let actions = vec!["default", tr("Open", "Abrir")];
            let body = ("slick", 0u32, "", note.title.as_str(), note.body.as_str(), actions, hints, -1i32);
            if let Ok(id) = p.call::<_, _, u32>("Notify", &body) {
                let mut t = targets.lock().unwrap();
                // Only the recent ones can still be clicked.
                t.retain(|k, _| k.saturating_add(100) > id);
                t.insert(id, note.target);
            }
        }
    });
}

/// The notification server, and a thread that follows its clicks.
fn connect(targets: Arc<Mutex<HashMap<u32, Target>>>, events: Sender<Event>) -> Option<zbus::blocking::Proxy<'static>> {
    let conn = zbus::blocking::Connection::session().ok()?;
    let proxy = zbus::blocking::Proxy::new(&conn, "org.freedesktop.Notifications", "/org/freedesktop/Notifications", "org.freedesktop.Notifications").ok()?;
    let clicks = proxy.receive_signal("ActionInvoked").ok()?;
    std::thread::spawn(move || {
        for msg in clicks {
            let Ok((id, action)) = msg.body().deserialize::<(u32, String)>() else { continue };
            if action != "default" {
                continue;
            }
            let Some(t) = targets.lock().unwrap().remove(&id) else { continue };
            let cmd = crate::ipc::Cmd::Open { conv: t.conv, ts: Some(t.ts), thread: t.thread, focus: true, team: Some(t.team).filter(|t| !t.is_empty()) };
            if events.send(Event::Remote(cmd)).is_err() {
                return;
            }
        }
    });
    Some(proxy)
}

/// A sound theme's sound, by name (`message-new-instant`), through
/// PulseAudio's or PipeWire's player, whichever is there. Started and left to run.
pub fn play(name: &str) {
    let Some(file) = sound_file(name) else { return };
    std::thread::spawn(move || {
        for player in ["paplay", "pw-play"] {
            let child = Command::new(player).arg(&file).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
            if let Ok(mut c) = child {
                let _ = c.wait();
                return;
            }
        }
    });
}

/// `sounds/freedesktop/stereo/<name>.oga` in the data folders.
fn sound_file(name: &str) -> Option<std::path::PathBuf> {
    let home = std::env::var_os("XDG_DATA_HOME").map(std::path::PathBuf::from).unwrap_or_else(|| crate::config::home().join(".local/share"));
    let dirs = std::env::var("XDG_DATA_DIRS").ok().filter(|d| !d.is_empty()).unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    let roots = std::iter::once(home).chain(dirs.split(':').map(std::path::PathBuf::from));
    roots
        .flat_map(|r| ["oga", "ogg", "wav"].map(|ext| r.join("sounds/freedesktop/stereo").join(format!("{name}.{ext}"))))
        .find(|p| p.is_file())
}

/// Text as a notification body shows it: servers read a little markup there.
fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

impl App {
    /// A `message.new` as a desktop notification, unless it shouldn't make
    /// a sound: notifications paused, the conversation in front of you, or
    /// its level (Slack's, the conversation's own first) leaves it out.
    pub(crate) fn notify_desktop(&self, data: &Value) {
        if !enabled() || self.account.paused.is_some() || worker().get().is_none() {
            return;
        }
        let (c, m) = (&data["conversation"], &data["message"]);
        let Some(conv) = c["id"].as_str() else { return };
        let (mention, thread) = (data["mention"] == true, data["thread"] == true);
        let direct = matches!(c["kind"].as_str(), Some("dm" | "group"));
        if !thread && self.focused && self.current.as_deref() == Some(conv) {
            return;
        }
        let pref = self.pref(conv);
        if pref.muted && !direct && !mention {
            return;
        }
        let level = pref.desktop.as_deref().or(self.account.level.as_deref()).unwrap_or("mentions");
        let wanted = match level {
            "everything" => true,
            "nothing" => false,
            "vips" => m["user"].as_str().is_some_and(|u| self.vips.contains(u)),
            _ => direct || mention || thread,
        };
        if !wanted {
            return;
        }
        let author = m["author"].as_str().unwrap_or("");
        let name = c["name"].as_str().unwrap_or(conv);
        let place = if matches!(c["kind"].as_str(), Some("channel" | "private")) { format!("#{name}") } else { name.to_string() };
        let title = match (thread, c["kind"].as_str()) {
            (true, _) => trf!("{author} replied in {place}", "{author} respondeu em {place}"),
            (false, Some("dm")) => author.to_string(),
            _ => trf!("{author} in {place}", "{author} em {place}"),
        };
        let target = Target {
            team: self.me.as_ref().map(|me| me.team_id.clone()).unwrap_or_default(),
            conv: conv.to_string(),
            ts: m["ts"].as_str().unwrap_or_default().to_string(),
            thread: m["thread_ts"].as_str().map(String::from),
        };
        let note = Note { target, title, body: escape(m["text"].as_str().unwrap_or("")) };
        if let Some(tx) = worker().get() {
            let _ = tx.send(note);
        }
    }
}
