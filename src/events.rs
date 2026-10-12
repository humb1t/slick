//! What slick tells other programs: a snapshot of what is unread, in
//! `state.json` next to the socket (see `ipc`), and events, as JSON lines,
//! to the socket's subscribers and to the hooks in
//! `~/.config/slick/hooks/<event>.d/`. The Omarchy widget reads the
//! snapshot; scripts can follow `slick events`. docs/events.md has the format.

use crate::app::{App, Event};
use crate::slack::{Count, Kind, Message, Res};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::sync::mpsc::{self, Sender};
use std::sync::{Mutex, OnceLock};

/// Bumped when a field changes meaning or goes away; new fields don't bump it.
const VERSION: u32 = 1;
/// Unread messages kept per conversation for the preview.
const PREVIEW: usize = 5;
/// Unread channels without a mention that get a preview, the most recent first.
const QUIET_CHANNELS: usize = 8;

#[derive(Default)]
pub struct State {
    /// Off in a second slick (`--new-instance`): the first one speaks for slick.
    pub on: bool,
    /// The unread messages of a conversation, and the `latest` they were fetched at.
    previews: HashMap<String, (String, Vec<Message>)>,
    fetching: HashSet<String>,
    /// Conversations the counts mention that the list didn't have, asked for once.
    discovered: HashSet<String>,
    /// The newest message already accounted for, per conversation: newer ones
    /// are news. Filled from the first counts, so what was waiting at start-up
    /// is not announced.
    seen: HashMap<String, String>,
    primed: bool,
    /// The last snapshot, without its time, to write only what changed.
    last: Option<Value>,
    /// Whose presence is followed (the sidebar's DMs, and you): only their
    /// changes are announced.
    watched: HashSet<String>,
    /// Reactions other people put on your messages, newest first.
    reactions: Vec<Reacted>,
    /// Each conversation's own notification preference, as Slack keeps it.
    prefs: HashMap<String, Pref>,
    /// The first answer about them came (or failed): until then every channel
    /// would look like it notifies, and the badges would count them all.
    prefs_loaded: bool,
}

/// A conversation's notification preference: muted, and the desktop level
/// set for it (`everything`, `mentions_dms`, `nothing`), `None` following
/// the global one.
#[derive(Clone, Default, PartialEq)]
pub struct Pref {
    pub muted: bool,
    pub desktop: Option<String>,
}

impl Pref {
    /// Muted, or set to notify of mentions only (or nothing): such a channel
    /// waits quietly, out of the badges and the preview's rows.
    fn quiet(&self) -> bool {
        self.muted || matches!(self.desktop.as_deref(), Some("mentions" | "mentions_dms" | "nothing"))
    }
}

/// Every conversation's own preference, from `all_notifications_prefs`.
fn prefs(s: &crate::slack::Slack) -> Res<HashMap<String, Pref>> {
    let v = s.call("users.prefs.get", &[])?;
    let all: Value = serde_json::from_str(v["prefs"]["all_notifications_prefs"].as_str().unwrap_or("{}")).unwrap_or_default();
    let mut out = HashMap::new();
    for (id, p) in all["channels"].as_object().into_iter().flatten() {
        let desktop = p["desktop"].as_str().filter(|d| !d.is_empty() && *d != "default").map(String::from);
        out.insert(id.clone(), Pref { muted: p["muted"] == true, desktop });
    }
    Ok(out)
}

/// Reactions kept for the snapshot, and for how long.
const REACTIONS: usize = 20;
const REACTION_SECS: i64 = 24 * 60 * 60;

/// A reaction to one of your messages, as the snapshot lists it.
struct Reacted {
    chan: String,
    ts: String,
    name: String,
    user: String,
    at: i64,
    item: Value,
}

/// Events and snapshots go out from a thread of their own: a slow hook or
/// subscriber never holds up the interface.
enum Job {
    Event(String, String),
    State(String),
}

fn worker() -> &'static Sender<Job> {
    static TX: OnceLock<Sender<Job>> = OnceLock::new();
    TX.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Job>();
        std::thread::spawn(move || {
            for job in rx {
                match job {
                    Job::Event(name, line) => deliver(&name, &line),
                    Job::State(text) => write_state(&text),
                }
            }
        });
        tx
    })
}

fn envelope(name: &str, data: Value) -> String {
    json!({ "v": VERSION, "event": name, "at": chrono::Local::now().timestamp(), "data": data }).to_string()
}

pub fn emit(name: &str, data: Value) {
    let _ = worker().send(Job::Event(name.to_string(), envelope(name, data)));
}

fn deliver(name: &str, line: &str) {
    crate::ipc::broadcast(line);
    run_hooks(name, line);
}

/// Every executable in `hooks/<event>.d/`, with the event on stdin and its
/// name in `SLICK_EVENT`. Started and left to run: nobody waits for them.
fn run_hooks(name: &str, line: &str) {
    let dir = crate::config::config_dir().join("hooks").join(format!("{name}.d"));
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
    let mut hooks: Vec<_> = entries.flatten().map(|e| e.path()).filter(|p| runnable(p)).collect();
    hooks.sort();
    for hook in hooks {
        let child = std::process::Command::new(&hook)
            .env("SLICK_EVENT", name)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        if let Ok(mut c) = child {
            if let Some(mut stdin) = c.stdin.take() {
                let _ = writeln!(stdin, "{line}");
            }
            // Reaped in the background, so no zombie is left behind.
            std::thread::spawn(move || c.wait());
        }
    }
}

#[cfg(unix)]
fn runnable(p: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(windows)]
fn runnable(p: &std::path::Path) -> bool {
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    p.is_file() && matches!(ext.as_str(), "exe" | "bat" | "cmd")
}

pub fn state_path() -> std::path::PathBuf {
    crate::ipc::dir().join("state.json")
}

/// Written beside and renamed over, so a reader never sees half of it.
fn write_state(text: &str) {
    let path = state_path();
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

fn kind(k: &Kind) -> &'static str {
    match k {
        Kind::Direct => "dm",
        Kind::Group => "group",
        Kind::Channel => "channel",
        Kind::Private => "private",
    }
}

/// Mentions you by name, or the whole channel.
fn mentions(m: &Message, me: &str) -> bool {
    (!me.is_empty() && m.text.contains(&format!("<@{me}>"))) || ["<!here", "<!channel", "<!everyone"].iter().any(|t| m.text.contains(t))
}

/// Custom emoji saved for the widgets: name → whether its file is there
/// (false while it downloads, or when that failed).
fn emoji_files() -> &'static Mutex<HashMap<String, bool>> {
    static FILES: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();
    FILES.get_or_init(Default::default)
}

/// A custom emoji's picture in `emoji/<name>.<ext>` next to the snapshot,
/// when it is there. The first ask downloads it in the background: the
/// snapshot picks it up once it lands.
fn emoji_file(name: &str, url: &str) -> Option<String> {
    let safe: String = name.chars().map(|c| if c.is_alphanumeric() || "-_+".contains(c) { c } else { '_' }).collect();
    let ext = url.rsplit('/').next().and_then(|f| f.rsplit_once('.')).map(|(_, e)| e).filter(|e| e.len() <= 4).unwrap_or("png");
    let path = crate::ipc::dir().join("emoji").join(format!("{safe}.{ext}"));
    let shown = path.to_string_lossy().into_owned();
    let mut files = emoji_files().lock().ok()?;
    match files.get(name) {
        Some(true) => return Some(shown),
        Some(false) => return None,
        None => {}
    }
    // Left by an earlier run in the same session.
    if path.is_file() {
        files.insert(name.to_string(), true);
        return Some(shown);
    }
    files.insert(name.to_string(), false);
    let (name, url) = (name.to_string(), url.to_string());
    std::thread::spawn(move || {
        let saved = (|| -> Option<()> {
            let resp = ureq::get(&url).call().ok()?;
            let mut bytes = Vec::new();
            std::io::Read::read_to_end(&mut std::io::Read::take(resp.into_reader(), 5 << 20), &mut bytes).ok()?;
            std::fs::create_dir_all(path.parent()?).ok()?;
            // Written beside and renamed over, so a reader never sees half of it.
            let tmp = path.with_extension("part");
            std::fs::write(&tmp, &bytes).ok()?;
            std::fs::rename(&tmp, &path).ok()
        })();
        if saved.is_some() {
            if let Ok(mut files) = emoji_files().lock() {
                files.insert(name, true);
            }
        }
    });
    None
}

impl App {
    pub(crate) fn refresh_prefs(&self) {
        self.spawn(|s| Event::NotifyPrefs(prefs(s)));
    }

    /// New preferences: channels that stopped being quiet get their preview.
    pub(crate) fn on_prefs(&mut self, res: Res<HashMap<String, Pref>>) {
        let first = !self.events.prefs_loaded;
        self.events.prefs_loaded = true;
        let Ok(p) = res else {
            self.dirty |= first;
            return;
        };
        if first || p != self.events.prefs {
            self.events.prefs = p;
            self.on_counts(self.counts.clone());
            self.dirty = true;
        }
    }

    /// A channel muted or set to mentions only. DMs never are: they always count.
    fn quiet(&self, c: &crate::slack::Conversation) -> bool {
        !matches!(c.kind, Kind::Direct | Kind::Group) && self.events.prefs.get(&c.id).is_some_and(Pref::quiet)
    }

    /// A channel muted in Slack (not "mentions only"): the sidebar files it
    /// under Silenced and never bolds it. DMs stay where they are.
    pub(crate) fn muted(&self, c: &crate::slack::Conversation) -> bool {
        !matches!(c.kind, Kind::Direct | Kind::Group) && self.events.prefs.get(&c.id).is_some_and(|p| p.muted)
    }

    /// The new counts: conversations whose newest message moved get their
    /// unread messages fetched for the preview.
    pub(crate) fn on_counts(&mut self, counts: HashMap<String, Count>) {
        self.counts = counts;
        // The list is loaded at start-up; a channel or group created since only
        // shows up here, in the counts. Fetched once, it joins the sidebar. Only
        // the unread ones: the counts also carry old DMs the list leaves out on
        // purpose (deactivated people), and those would cost a call each.
        let known: HashSet<&str> = self.convs.iter().map(|c| c.id.as_str()).collect();
        let new: Vec<String> = self
            .counts
            .iter()
            .filter(|(id, c)| (c.unread || c.mentions > 0) && !known.contains(id.as_str()))
            .map(|(id, _)| id.clone())
            .collect();
        for id in new {
            if self.events.discovered.insert(id.clone()) {
                self.spawn(move |s| Event::Joined(s.conversation(&id)));
            }
        }
        if !self.events.on {
            return;
        }
        let ev = &mut self.events;
        if !ev.primed {
            ev.primed = true;
            ev.seen = self.counts.iter().map(|(id, c)| (id.clone(), c.latest.clone())).collect();
        }
        // Read ones move the mark along, so reading them elsewhere and then
        // getting one new message announces just that one.
        for (id, c) in &self.counts {
            if !c.unread {
                ev.previews.remove(id);
                let seen = ev.seen.entry(id.clone()).or_default();
                if c.latest > *seen {
                    *seen = c.latest.clone();
                }
            }
        }
        let mut quiet: Vec<(&String, &Count)> = Vec::new();
        let mut want: Vec<(String, String)> = Vec::new();
        for c in &self.convs {
            let Some(n) = self.counts.get(&c.id) else { continue };
            if !self.unread(&c.id).0 {
                continue;
            }
            if matches!(c.kind, Kind::Direct | Kind::Group) || n.mentions > 0 || !self.quiet(c) {
                want.push((c.id.clone(), n.last_read.clone()));
            } else {
                quiet.push((&c.id, n));
            }
        }
        quiet.sort_by(|a, b| b.1.latest.cmp(&a.1.latest));
        want.extend(quiet.into_iter().take(QUIET_CHANNELS).map(|(id, n)| (id.clone(), n.last_read.clone())));
        for (chan, last_read) in want {
            let latest = self.counts[&chan].latest.clone();
            let ev = &mut self.events;
            if ev.previews.get(&chan).is_some_and(|p| p.0 == latest) || !ev.fetching.insert(chan.clone()) {
                continue;
            }
            self.spawn(move |s| {
                let res = s.history(&chan, Some(last_read.as_str()).filter(|t| !t.is_empty()), None);
                Event::Preview { chan, latest, res }
            });
        }
    }

    /// A conversation's unread messages arrived: kept for the preview, and the
    /// ones not seen before announced.
    pub(crate) fn on_preview(&mut self, chan: String, latest: String, res: Res<Vec<Message>>) {
        self.events.fetching.remove(&chan);
        let Ok(msgs) = res else { return };
        let me = self.me.as_ref().map(|m| m.user_id.clone()).unwrap_or_default();
        let seen = self.events.seen.get(&chan).cloned().unwrap_or_default();
        let fresh: Vec<&Message> = msgs.iter().filter(|m| m.ts > seen && m.user.as_deref() != Some(me.as_str())).collect();
        if !fresh.is_empty() {
            let conv = self.conv_json(&chan);
            for m in fresh {
                let mut data = json!({ "conversation": conv.clone(), "message": self.message_json(m) });
                data["mention"] = json!(mentions(m, &me));
                emit("message.new", data);
            }
        }
        if let Some(last) = msgs.last() {
            let seen = self.events.seen.entry(chan.clone()).or_default();
            if last.ts > *seen {
                *seen = last.ts.clone();
            }
        }
        let keep = msgs[msgs.len().saturating_sub(PREVIEW)..].to_vec();
        self.events.previews.insert(chan, (latest, keep));
        self.dirty = true;
    }

    fn conv_json(&self, id: &str) -> Value {
        match self.convs.iter().find(|c| c.id == id) {
            Some(c) => json!({ "id": c.id, "kind": kind(&c.kind), "name": self.label(c), "user": c.user }),
            None => json!({ "id": id, "kind": Value::Null, "name": id, "user": Value::Null }),
        }
    }

    fn message_json(&self, m: &Message) -> Value {
        let names = self.names();
        // Without the bars that mark attachments in the conversation.
        let mut text = crate::text::body(m, &names).lines().map(|l| l.trim_start_matches('▎').trim_start()).collect::<Vec<_>>().join("\n");
        if text.trim().is_empty() && !m.files.is_empty() {
            text = m.files.iter().map(|f| format!("📎 {}", f.name)).collect::<Vec<_>>().join("\n");
        }
        if text.chars().count() > 500 {
            text = text.chars().take(500).collect::<String>() + "…";
        }
        json!({
            "ts": m.ts,
            "user": m.user,
            "author": crate::ui::author(m, &names),
            "text": text,
            "html": self.message_html(m, &names),
            "thread_ts": m.thread_ts.as_ref().filter(|t| **t != m.ts),
        })
    }

    /// `text` as HTML (`text::body_html`): escaped, with the message's own
    /// bold, italic and strikethrough, and the workspace's emoji as `<img>`
    /// of their saved pictures (`:name:` until one is downloaded). Cut like
    /// `text`, never inside a tag or an entity.
    fn message_html(&self, m: &Message, names: &crate::text::Names) -> String {
        let mut html = crate::text::body_html(m, names).lines().map(|l| l.trim_start_matches('▎').trim_start()).collect::<Vec<_>>().join("\n");
        if html.trim().is_empty() && !m.files.is_empty() {
            html = m.files.iter().map(|f| format!("📎 {}", crate::text::escape(&f.name))).collect::<Vec<_>>().join("\n");
        }
        let html = self.emoji_images(&html);
        let mut html: String = html;
        if html.chars().count() > 1500 {
            html = html.chars().take(1500).collect();
            if let Some(i) = html.rfind(['<', '&']).filter(|&i| !html[i..].contains(['>', ';'])) {
                html.truncate(i);
            }
            html.push('…');
        }
        html
    }

    /// `:name:` of the workspace's own emoji → `<img>` of its saved picture.
    fn emoji_images(&self, html: &str) -> String {
        let mut out = String::with_capacity(html.len());
        let mut rest = html;
        while let Some(i) = rest.find(':') {
            out.push_str(&rest[..i]);
            let after = &rest[i + 1..];
            let name_len = after.find(|c: char| !(c.is_ascii_alphanumeric() || "_-+'".contains(c))).unwrap_or(after.len());
            let name = &after[..name_len];
            let file = (after[name_len..].starts_with(':') && !name.is_empty())
                .then(|| self.emoji.custom.get(name).and_then(|url| emoji_file(name, url)))
                .flatten();
            match file {
                Some(f) => {
                    let src = crate::text::escape(&format!("file://{f}"));
                    out.push_str(&format!("<img src=\"{src}\" alt=\":{name}:\">"));
                    rest = &after[name_len + 1..];
                }
                None => {
                    out.push(':');
                    rest = after;
                }
            }
        }
        out.push_str(rest);
        out
    }

    /// Someone, by id and name.
    fn person(&self, uid: &str) -> Value {
        json!({ "id": uid, "name": self.users.names.get(uid).map(String::as_str).unwrap_or(uid) })
    }

    /// An emoji as widgets can draw it: the character of a standard one, or
    /// the picture of a custom one, and that picture saved next to the snapshot.
    fn emoji_json(&self, name: &str) -> Value {
        let char = crate::emoji::standard(name).or_else(|| self.emoji.unicode.get(name).cloned());
        let url = if char.is_none() { self.emoji.custom.get(name).cloned() } else { None };
        let file = url.as_deref().and_then(|u| emoji_file(name, u));
        json!({ "name": name, "char": char, "url": url, "file": file })
    }

    /// Events go out once `app.started` has, and only from the first slick.
    fn speaking(&self) -> bool {
        self.events.on && self.events.last.is_some()
    }

    /// Someone reacted to a message, or took the reaction back. Only the
    /// socket says this: polling sees that a conversation moved, not why.
    pub(crate) fn on_reaction(&mut self, added: bool, chan: &str, ts: &str, name: &str, user: &str, item_user: Option<&str>) {
        if !self.speaking() {
            return;
        }
        let me = self.me.as_ref().map(|m| m.user_id.clone()).unwrap_or_default();
        let (mine, by_me) = (!me.is_empty() && item_user == Some(me.as_str()), user == me);
        if !added {
            self.events.reactions.retain(|r| !(r.chan == chan && r.ts == ts && r.name == name && r.user == user));
        }
        // The message's text when it's at hand: loaded, in the open thread, or in a preview.
        let cached = self
            .messages
            .get(chan)
            .and_then(|ms| ms.iter().find(|m| m.ts == ts))
            .or_else(|| self.thread.as_ref().filter(|t| t.chan == chan).and_then(|t| t.msgs.iter().find(|m| m.ts == ts)))
            .or_else(|| self.events.previews.get(chan).and_then(|p| p.1.iter().find(|m| m.ts == ts)));
        let message = cached.map_or_else(|| json!({ "ts": ts }), |m| self.message_json(m));
        let conv = self.conv_json(chan);
        let who = self.person(user);
        let emoji = self.emoji_json(name);
        if added && mine && !by_me {
            let item = json!({ "conversation": conv.clone(), "message": message.clone(), "reaction": name, "emoji": emoji.clone(), "user": who.clone() });
            let r = Reacted { chan: chan.into(), ts: ts.into(), name: name.into(), user: user.into(), at: chrono::Local::now().timestamp(), item };
            self.events.reactions.insert(0, r);
            self.events.reactions.truncate(REACTIONS);
        }
        let data = json!({ "conversation": conv, "message": message, "reaction": name, "emoji": emoji, "user": who, "mine": mine, "by_me": by_me });
        emit(if added { "reaction.added" } else { "reaction.removed" }, data);
    }

    /// The people whose presence is followed: the sidebar's DMs, and you.
    /// The socket is told to report them; whoever left the list forgets their
    /// last presence, so coming back is a first reading again, not a change.
    pub(crate) fn watch_presence(&mut self, uids: Vec<String>) {
        let mut set: HashSet<String> = uids.into_iter().collect();
        set.extend(self.me.as_ref().map(|m| m.user_id.clone()));
        if set == self.events.watched {
            return;
        }
        self.presence.retain(|u, _| set.contains(u));
        crate::rtm::subscribe(set.iter().cloned().collect());
        self.events.watched = set;
    }

    /// Presence readings, from the socket or from polling. A change from the
    /// last reading is announced; a first one, or one for someone no longer
    /// followed, is not.
    pub(crate) fn on_presence(&mut self, readings: Vec<(String, bool)>) {
        let me = self.me.as_ref().map(|m| m.user_id.clone()).unwrap_or_default();
        for (uid, active) in readings {
            let mine = uid == me;
            if !mine && !self.events.watched.contains(&uid) {
                continue;
            }
            if mine {
                self.account.online = active;
            }
            let was = self.presence.insert(uid.clone(), active);
            if was.is_some_and(|w| w != active) && self.speaking() {
                let dm = self.convs.iter().find(|c| matches!(c.kind, Kind::Direct) && c.user.as_deref() == Some(uid.as_str())).map(|c| c.id.clone());
                emit("presence.changed", json!({ "user": self.person(&uid), "active": active, "dm": dm, "me": mine }));
            }
        }
        self.dirty = true;
    }

    /// Writes the snapshot when something in it changed, and tells the
    /// subscribers what: counts, conversations read, a pause.
    pub fn publish(&mut self) {
        if !self.events.on || self.login.is_some() || self.me.is_none() {
            return;
        }
        // Nothing until the counts and the preferences are in: a snapshot
        // before them would show badges that are not the real ones yet.
        if !self.events.primed || !self.events.prefs_loaded {
            return;
        }
        // Reactions stop being news after a day, or once their conversation
        // is read: open in front of you, or no longer unread (`conversation.read`).
        let was_unread = |id: &str| {
            let last = self.events.last.as_ref();
            last.and_then(|l| l["conversations"].as_array()).is_some_and(|cs| cs.iter().any(|c| c["id"] == id))
        };
        let seen: HashSet<String> = self
            .events
            .reactions
            .iter()
            .map(|r| r.chan.clone())
            .filter(|ch| {
                let (unread, m) = self.unread(ch);
                (self.focused && self.current.as_deref() == Some(ch.as_str())) || (was_unread(ch.as_str()) && !unread && m == 0)
            })
            .collect();
        let now = chrono::Local::now().timestamp();
        self.events.reactions.retain(|r| now - r.at < REACTION_SECS && !seen.contains(&r.chan));
        let reactions: Vec<Value> = self
            .events
            .reactions
            .iter()
            .map(|r| {
                let mut v = r.item.clone();
                // Again each time: a custom emoji's file shows up once downloaded.
                v["emoji"] = self.emoji_json(&r.name);
                v["at"] = json!(r.at);
                v
            })
            .collect();
        let mut convs: Vec<(&String, Value)> = Vec::new();
        let (mut mentions, mut dms, mut groups, mut channels, mut channels_mentioned, mut channels_quiet) = (0u64, 0, 0, 0, 0, 0);
        // Messages rather than conversations: in a DM every unread message counts
        // as a mention, so this is how many wait there; in channels, the mentions.
        let (mut direct_messages, mut channel_mentions) = (0u64, 0u64);
        for c in &self.convs {
            let (unread, m) = self.unread(&c.id);
            if !unread && m == 0 {
                continue;
            }
            mentions += m;
            if matches!(c.kind, Kind::Direct | Kind::Group) {
                direct_messages += m.max(1);
            } else {
                channel_mentions += m;
            }
            match c.kind {
                Kind::Direct => dms += 1,
                Kind::Group => groups += 1,
                _ if m > 0 => channels_mentioned += 1,
                _ => {
                    channels += 1;
                    if self.quiet(c) {
                        channels_quiet += 1;
                    }
                }
            }
            let latest = self.counts.get(&c.id).map(|n| &n.latest);
            let messages: Vec<Value> = self
                .events
                .previews
                .get(&c.id)
                .map(|p| p.1.iter().map(|m| self.message_json(m)).collect())
                .unwrap_or_default();
            let mut v = self.conv_json(&c.id);
            v["unread"] = json!(unread);
            v["mentions"] = json!(m);
            v["latest"] = json!(latest);
            v["messages"] = json!(messages);
            let pref = self.events.prefs.get(&c.id).cloned().unwrap_or_default();
            v["muted"] = json!(pref.muted);
            v["notify"] = json!(pref.desktop.as_deref().unwrap_or("default"));
            v["quiet"] = json!(self.quiet(c));
            convs.push((latest.unwrap_or(&c.id), v));
        }
        convs.sort_by(|a, b| b.0.cmp(a.0));
        let dnd = self.account.paused.map(|p| json!({ "paused": true, "until": p.until, "snooze": p.snooze }));
        let me = self.me.as_ref().expect("checked above");
        let snapshot = json!({
            "v": VERSION,
            "running": true,
            "pid": std::process::id(),
            "version": env!("CARGO_PKG_VERSION"),
            "team": me.team,
            "domain": me.domain,
            "user": me.user,
            "dnd": dnd,
            "attention": dms + groups + channels_mentioned,
            "totals": {
                "mentions": mentions, "dms": dms, "groups": groups, "channels": channels, "channels_mentioned": channels_mentioned, "channels_quiet": channels_quiet,
                "direct_messages": direct_messages, "channel_mentions": channel_mentions,
                "reactions": reactions.len(),
            },
            "conversations": convs.into_iter().map(|c| c.1).collect::<Vec<_>>(),
            "reactions": reactions,
        });
        if self.events.last.as_ref() == Some(&snapshot) {
            return;
        }
        match self.events.last.take() {
            None => emit("app.started", json!({ "pid": std::process::id(), "version": env!("CARGO_PKG_VERSION"), "team": me.team })),
            Some(old) => {
                if old["totals"] != snapshot["totals"] {
                    emit("counts.changed", json!({ "attention": snapshot["attention"], "totals": snapshot["totals"] }));
                }
                let ids = |v: &Value| -> HashSet<String> {
                    v["conversations"].as_array().into_iter().flatten().filter_map(|c| c["id"].as_str().map(String::from)).collect()
                };
                let now = ids(&snapshot);
                for id in ids(&old).difference(&now) {
                    emit("conversation.read", json!({ "conversation": self.conv_json(id) }));
                }
                if old["dnd"] != snapshot["dnd"] {
                    emit("dnd.changed", if snapshot["dnd"].is_null() { json!({ "paused": false }) } else { snapshot["dnd"].clone() });
                }
            }
        }
        let mut text = snapshot.clone();
        text["updated"] = json!(chrono::Local::now().timestamp());
        let _ = worker().send(Job::State(text.to_string()));
        self.events.last = Some(snapshot);
    }

    /// On the way in: a snapshot left by a slick that died without its
    /// goodbye (its terminal closed) would show its old badges until the
    /// first real one.
    pub fn starting(&self) {
        if self.events.on {
            write_state(&json!({ "v": VERSION, "running": false, "pid": std::process::id(), "updated": chrono::Local::now().timestamp() }).to_string());
        }
    }

    /// On the way out: the snapshot says so, and the subscribers hear it.
    /// Done here and now, since the process ends right after.
    pub fn unpublish(&mut self) {
        if !self.events.on || self.events.last.is_none() {
            return;
        }
        write_state(&json!({ "v": VERSION, "running": false, "pid": std::process::id(), "updated": chrono::Local::now().timestamp() }).to_string());
        deliver("app.stopped", &envelope("app.stopped", json!({ "pid": std::process::id() })));
    }
}
