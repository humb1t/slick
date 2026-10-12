//! Slack's real-time websocket (`rtm.connect`), the way the official app
//! hears about a message the moment it is sent. It only says *that*
//! something changed: the app then fetches what it would have fetched on its
//! next poll, just now instead of seconds later. Polling stays, slower, as
//! the fallback for when the socket is down. Reactions and presence come
//! with their details, which polling can't tell. With a browser session it
//! is the official client's socket (see `Slack::rtm_url`), the only one told
//! when someone else's status or profile changes.

use crate::app::Event;
use crate::slack::Slack;
use serde_json::Value;
use std::net::TcpStream;
use std::sync::mpsc::Sender;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::Message as Frame;

/// What the socket told us, reduced to what the app does about it.
pub enum Ev {
    /// Connected; everything may have moved while it was down.
    Up,
    Down,
    /// A message in a conversation (new, edited, deleted), the thread it
    /// belongs to and who sent it.
    Message { chan: String, thread: Option<String>, user: Option<String> },
    /// `user` put (or took back) the emoji `name` on the message `ts`, which
    /// `item_user` wrote.
    Reaction { added: bool, chan: String, ts: String, name: String, user: String, item_user: Option<String> },
    /// People subscribed to (`presence_sub`) became active (true) or away.
    Presence(Vec<(String, bool)>),
    /// You set yourself away, or back, somewhere.
    MyPresence,
    /// Someone is typing in a conversation (not in a thread).
    Typing { chan: String, user: String },
    /// Something was read, here or elsewhere.
    Counts,
    /// A thread you follow changed: read, followed, unfollowed, or its
    /// unread state (`threads`).
    Threads,
    /// Your pause was set or ended.
    Dnd,
    /// Your notification preferences changed: a channel muted, or set to mentions only.
    Prefs,
    /// You were added to a conversation or one was created.
    Convs,
    /// Someone's account changed: name, photo, status, deactivated (or
    /// back). The user object as Slack sent it, for `Users::apply`.
    User(Value),
    /// Someone's account changed, without the details: what the official
    /// client's socket says (`user_invalidated`); fetch them again.
    UserStale(String),
    /// A channel was archived (or unarchived), here or elsewhere.
    Archived { chan: String, archived: bool },
    /// A message only you see, like a slash command's answer: Slack never
    /// lists it, so it comes whole.
    Ephemeral { chan: String, msg: crate::slack::Message },
}

/// Slack expects a word from the client now and then; quiet sockets get dropped.
const PING_EVERY: Duration = Duration::from_secs(30);

/// The people whose presence the socket should report: the app sets it,
/// each connection subscribes to it (again) after `hello`.
fn wanted() -> &'static Mutex<Vec<String>> {
    static WANTED: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
    WANTED.get_or_init(Default::default)
}

/// Whose presence to hear about. Sent within seconds, and only when it changed.
pub fn subscribe(mut ids: Vec<String>) {
    ids.sort();
    ids.dedup();
    if let Ok(mut w) = wanted().lock() {
        *w = ids;
    }
}

/// Runs for the life of the process, reconnecting with a growing pause.
pub fn start(slack: Slack, tx: Sender<Event>) {
    std::thread::spawn(move || {
        let mut wait = Duration::from_secs(2);
        loop {
            let started = Instant::now();
            let up = run(&slack, &tx);
            if tx.send(Event::Live(Ev::Down)).is_err() {
                return;
            }
            // A connection that lasted resets the pause; failing at once doubles it.
            wait = if up && started.elapsed() > Duration::from_secs(60) { Duration::from_secs(2) } else { (wait * 2).min(Duration::from_secs(120)) };
            std::thread::sleep(wait);
        }
    });
}

/// One connection, until it drops. Whether it got as far as connecting.
fn run(slack: &Slack, tx: &Sender<Event>) -> bool {
    let Ok((url, cookie)) = slack.rtm_url() else { return false };
    let Ok(mut req) = tungstenite::client::IntoClientRequest::into_client_request(url.as_str()) else { return false };
    if let Some(c) = cookie.and_then(|c| format!("d={c}").parse().ok()) {
        req.headers_mut().insert("Cookie", c);
    }
    if let Ok(o) = "https://app.slack.com".parse() {
        req.headers_mut().insert("Origin", o);
    }
    let Ok((mut ws, _)) = tungstenite::connect(req) else { return false };
    // Reads wake up on their own, so the pings go out on time.
    if let Some(s) = tcp(ws.get_mut()) {
        let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
    }
    if tx.send(Event::Live(Ev::Up)).is_err() {
        return true;
    }
    let (mut last_ping, mut id) = (Instant::now(), 0u64);
    // What this connection is subscribed to; `None` until `hello`.
    let mut subscribed: Option<Vec<String>> = None;
    loop {
        let want = wanted().lock().map(|w| w.clone()).unwrap_or_default();
        if subscribed.as_ref().is_some_and(|s| *s != want) {
            // Each presence_sub replaces the last one.
            let sub = serde_json::json!({ "type": "presence_sub", "ids": want }).to_string();
            if ws.send(Frame::text(sub)).is_err() {
                return true;
            }
            subscribed = Some(want);
        }
        if last_ping.elapsed() >= PING_EVERY {
            id += 1;
            last_ping = Instant::now();
            if ws.send(Frame::text(format!(r#"{{"id":{id},"type":"ping"}}"#))).is_err() {
                return true;
            }
        }
        match ws.read() {
            Ok(Frame::Text(t)) => {
                // SLICK_RTM_LOG=<file>: every frame, as it came, for debugging.
                if let Some(path) = std::env::var_os("SLICK_RTM_LOG") {
                    use std::io::Write;
                    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
                        let _ = writeln!(f, "{t}");
                    }
                }
                let Ok(v) = serde_json::from_str::<Value>(&t) else { continue };
                if v["type"] == "goodbye" {
                    return true;
                }
                // Ready for commands: the presence subscription goes out next round.
                if v["type"] == "hello" && subscribed.is_none() {
                    subscribed = Some(Vec::new());
                }
                if let Some(ev) = classify(&v) {
                    if tx.send(Event::Live(ev)).is_err() {
                        return true;
                    }
                }
            }
            Ok(Frame::Close(_)) => return true,
            Ok(_) => {}
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(_) => return true,
        }
    }
}

fn tcp(s: &mut MaybeTlsStream<TcpStream>) -> Option<&mut TcpStream> {
    match s {
        MaybeTlsStream::Plain(s) => Some(s),
        MaybeTlsStream::Rustls(s) => Some(s.get_mut()),
        _ => None,
    }
}

fn classify(v: &Value) -> Option<Ev> {
    let s = |k: &str| v[k].as_str().map(String::from);
    Some(match v["type"].as_str()? {
        "message" if v["is_ephemeral"].as_bool() == Some(true) => Ev::Ephemeral { chan: s("channel")?, msg: crate::slack::parse_message(v) },
        "message" => {
            // Edits and deletions carry the thread in the message they change.
            let inner = if v["message"].is_object() { &v["message"] } else if v["previous_message"].is_object() { &v["previous_message"] } else { v };
            Ev::Message { chan: s("channel")?, thread: inner["thread_ts"].as_str().map(String::from), user: s("user") }
        }
        "reaction_added" | "reaction_removed" => Ev::Reaction {
            added: v["type"] == "reaction_added",
            chan: v["item"]["channel"].as_str()?.to_string(),
            ts: v["item"]["ts"].as_str()?.to_string(),
            name: s("reaction")?,
            user: s("user")?,
            item_user: s("item_user"),
        },
        // One person (`user`), or several at once (`users`).
        "presence_change" => {
            let active = v["presence"] == "active";
            let users: Vec<String> = match v["users"].as_array() {
                Some(us) => us.iter().filter_map(|u| u.as_str().map(String::from)).collect(),
                None => vec![s("user")?],
            };
            Ev::Presence(users.into_iter().map(|u| (u, active)).collect())
        }
        "manual_presence_change" => Ev::MyPresence,
        "user_typing" if v["thread_ts"].is_null() => Ev::Typing { chan: s("channel")?, user: s("user")? },
        "channel_marked" | "group_marked" | "im_marked" | "mpim_marked" | "badge_counts_updated" => Ev::Counts,
        "thread_marked" | "thread_subscribed" | "thread_unsubscribed" | "update_thread_state" => Ev::Threads,
        "dnd_updated" => Ev::Dnd,
        "pref_change" if matches!(v["name"].as_str(), Some("all_notifications_prefs" | "muted_channels")) => Ev::Prefs,
        "channel_joined" | "group_joined" | "im_created" | "mpim_joined" | "mpim_open" | "im_open" | "group_open" | "channel_created" => Ev::Convs,
        "channel_archive" | "group_archive" => Ev::Archived { chan: s("channel")?, archived: true },
        "channel_unarchive" | "group_unarchive" => Ev::Archived { chan: s("channel")?, archived: false },
        // The whole user object, profile and status included; the last two
        // are what Slack sends when only those changed.
        "user_change" | "user_status_changed" | "user_profile_changed" if v["user"]["id"].is_string() => Ev::User(v["user"].clone()),
        "user_invalidated" => Ev::UserStale(v["user"]["id"].as_str()?.to_string()),
        _ => return None,
    })
}
