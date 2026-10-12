//! One slick per session. The first one holds a lock in the runtime dir and
//! listens next to it; a later `slick` finds the lock taken, hands the
//! running one what it was asked and exits. The channel speaks JSON lines
//! (`{"cmd":"open","conv":"C123"}`), so scripts and the Omarchy widget can
//! use it too; `{"cmd":"subscribe"}` turns a connection into a feed of
//! events (see `events`).
//!
//! A Unix socket on Linux. Windows has none in std, so there it is a
//! loopback TCP port, written with a random token to a file only the user
//! can read; a connection that doesn't start with the token is dropped.

use crate::app::Event;
use std::fs::{File, TryLockError};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::Mutex;
use std::time::Duration;

#[cfg(unix)]
type Stream = std::os::unix::net::UnixStream;
#[cfg(unix)]
type Listener = std::os::unix::net::UnixListener;
#[cfg(windows)]
type Stream = std::net::TcpStream;
#[cfg(windows)]
type Listener = std::net::TcpListener;

/// What another process asks the running slick to do.
#[allow(dead_code)]
pub enum Cmd {
    /// Bring the window to the front.
    Focus,
    /// A conversation, scrolled to a message, in its thread when it has one.
    /// `team`: the workspace (id or domain) it belongs to; `None` looks in each.
    Open { conv: String, ts: Option<String>, thread: Option<String>, focus: bool, team: Option<String> },
    MarkRead { conv: String, team: Option<String> },
    /// Pause notifications for this many minutes, or resume them.
    Pause(i64),
    Resume,
    Quit,
}

impl Cmd {
    fn parse(v: &serde_json::Value) -> Option<Cmd> {
        let s = |k: &str| v[k].as_str().filter(|s| !s.is_empty()).map(String::from);
        Some(match v["cmd"].as_str()? {
            "focus" => Cmd::Focus,
            "open" => Cmd::Open { conv: s("conv")?, ts: s("ts"), thread: s("thread_ts"), focus: v["focus"].as_bool() != Some(false), team: s("team") },
            "mark-read" => Cmd::MarkRead { conv: s("conv")?, team: s("team") },
            "dnd.pause" => Cmd::Pause(v["minutes"].as_i64().filter(|m| *m > 0)?),
            "dnd.resume" => Cmd::Resume,
            "quit" => Cmd::Quit,
            _ => return None,
        })
    }
}

/// `$XDG_RUNTIME_DIR/slick`, a private tmpfs that goes away with the
/// session; the cache folder when there is none, as on Windows.
pub fn dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|v| !v.is_empty() && cfg!(unix))
        .map(|d| PathBuf::from(d).join("slick"))
        .unwrap_or_else(|| crate::config::cache_dir().join("run"))
}

/// Where the channel is: the socket, or the file with the port and token.
fn address() -> PathBuf {
    dir().join(if cfg!(unix) { "slick.sock" } else { "slick.port" })
}

pub enum Claim {
    /// We are the one; `serve` starts answering the others.
    Owner(Server),
    /// Another slick holds the lock.
    Running,
}

pub struct Server {
    /// Held for as long as the process lives; the system lets go of it on exit or exec.
    lock: File,
    listener: Listener,
    #[cfg(windows)]
    token: String,
}

fn make_dir() -> std::io::Result<PathBuf> {
    let dir = dir();
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
    b.create(&dir)?;
    Ok(dir)
}

/// Whether some slick holds the lock right now.
pub fn running() -> bool {
    let Ok(lock) = File::options().create(true).truncate(false).write(true).open(dir().join("slick.lock")) else { return false };
    matches!(lock.try_lock(), Err(TryLockError::WouldBlock))
}

/// Takes the lock, or says someone has it. Anything else that goes wrong
/// (no runtime dir, no socket) is `None`: slick runs, just without the rest.
pub fn claim() -> Option<Claim> {
    let dir = make_dir().ok()?;
    let lock = File::options().create(true).truncate(false).write(true).open(dir.join("slick.lock")).ok()?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Some(Claim::Running),
        Err(_) => return None,
    }
    // Whatever is there belonged to a slick that is gone: the lock says so.
    let path = address();
    let _ = std::fs::remove_file(&path);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let listener = Listener::bind(&path).ok()?;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        Some(Claim::Owner(Server { lock, listener }))
    }
    #[cfg(windows)]
    {
        let listener = Listener::bind("127.0.0.1:0").ok()?;
        let port = listener.local_addr().ok()?.port();
        let token = token();
        std::fs::write(&path, format!("{port} {token}")).ok()?;
        Some(Claim::Owner(Server { lock, listener, token }))
    }
}

/// Unguessable enough for a loopback port only this user's files point to.
#[cfg(windows)]
fn token() -> String {
    use sha2::Digest;
    let mut h = sha2::Sha256::new();
    let here = 0u8;
    h.update(format!("{:?} {} {:p}", std::time::SystemTime::now(), std::process::id(), &here));
    h.finalize().iter().take(16).map(|b| format!("{b:02x}")).collect()
}

/// Connections that asked for events.
static SUBSCRIBERS: Mutex<Vec<Stream>> = Mutex::new(Vec::new());

impl Server {
    /// Answers in its own thread; every command becomes an `Event::Remote`.
    pub fn serve(self, events: Sender<Event>) {
        std::thread::spawn(move || {
            let _lock = self.lock;
            for conn in self.listener.incoming().flatten() {
                let events = events.clone();
                #[cfg(windows)]
                let token = self.token.clone();
                std::thread::spawn(move || {
                    let Ok(read) = conn.try_clone() else { return };
                    #[cfg_attr(unix, allow(unused_mut))]
                    let mut lines = BufReader::new(read).lines().map_while(Result::ok);
                    #[cfg(windows)]
                    if lines.next().as_deref() != Some(token.as_str()) {
                        return;
                    }
                    for line in lines {
                        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
                        if v["cmd"] == "subscribe" {
                            let _ = conn.set_write_timeout(Some(Duration::from_secs(2)));
                            if let (Ok(c), Ok(mut subs)) = (conn.try_clone(), SUBSCRIBERS.lock()) {
                                subs.push(c);
                            }
                        } else if let Some(cmd) = Cmd::parse(&v) {
                            if events.send(Event::Remote(cmd)).is_err() {
                                return;
                            }
                        }
                    }
                });
            }
        });
    }
}

/// One line to every subscriber; the ones that went away are dropped.
pub fn broadcast(line: &str) {
    let Ok(mut subs) = SUBSCRIBERS.lock() else { return };
    subs.retain_mut(|s| writeln!(s, "{line}").and_then(|_| s.flush()).is_ok());
}

/// A connection to the running slick. The lock can be taken a moment
/// before the channel is up, so a refused connection gets a few more tries.
pub fn connect() -> std::io::Result<Stream> {
    let mut tries = 0;
    loop {
        match open_channel() {
            Ok(c) => return Ok(c),
            Err(_) if tries < 10 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(e),
        }
    }
}

#[cfg(unix)]
fn open_channel() -> std::io::Result<Stream> {
    Stream::connect(address())
}

#[cfg(windows)]
fn open_channel() -> std::io::Result<Stream> {
    let text = std::fs::read_to_string(address())?;
    let (port, token) = text.trim().split_once(' ').ok_or_else(|| std::io::Error::other("slick.port"))?;
    let port: u16 = port.parse().map_err(std::io::Error::other)?;
    let mut s = Stream::connect(("127.0.0.1", port))?;
    writeln!(s, "{token}")?;
    Ok(s)
}

/// Sends one command to the running slick.
pub fn send(cmd: &serde_json::Value) -> std::io::Result<()> {
    let mut conn = connect()?;
    writeln!(conn, "{cmd}")?;
    conn.flush()
}

/// Prints the running slick's events, one JSON line each, until stdout goes
/// away. Waits for slick when it isn't running, and again after it quits.
pub fn follow() -> i32 {
    let mut out = std::io::stdout();
    loop {
        if let Ok(mut conn) = open_channel() {
            if writeln!(conn, "{}", serde_json::json!({ "cmd": "subscribe" })).is_ok() {
                for line in BufReader::new(conn).lines().map_while(Result::ok) {
                    if writeln!(out, "{line}").and_then(|_| out.flush()).is_err() {
                        return 0;
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}
