//! Slack Web API: a blocking client plus the few shapes the app needs.

use crate::i18n::tr;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

pub type Res<T> = Result<T, String>;

#[derive(Clone, Default, PartialEq)]
pub struct Creds {
    pub token: String,
    /// The `d` cookie, only for browser session tokens (xoxc).
    pub cookie: Option<String>,
}

/// Clones share the credentials, so a new login reaches every thread.
#[derive(Clone)]
pub struct Slack {
    agent: ureq::Agent,
    creds: Arc<RwLock<Creds>>,
    /// The people whose presence this workspace's socket reports (`rtm`).
    wanted: Arc<Mutex<Vec<String>>>,
    /// Away set by hand: the socket stops saying someone is there (`rtm`).
    away: Arc<std::sync::atomic::AtomicBool>,
    /// The last `client.userBoot` answer, the token it was for and when it came
    /// (`user_boot`): start-up asks for it up to three times at once.
    boot_cache: Arc<Mutex<Option<(String, Instant, Value)>>>,
    /// The edge API's org id (`org`) and the token it was for: it doesn't change in a session.
    org_cache: Arc<Mutex<Option<(String, String)>>>,
}

/// How long a `client.userBoot` answer is used again: start-up's calls, the
/// last of them `REST_AFTER` after the first, but not a later refresh.
const BOOT_FOR: Duration = Duration::from_secs(30);

#[derive(Clone, Debug)]
pub struct AuthInfo {
    pub user_id: String,
    pub user: String,
    pub team: String,
    pub team_id: String,
    /// The workspace's address, the `acme` in acme.slack.com.
    pub domain: String,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Users {
    /// id → the name people see (display name, else real name, else handle).
    pub names: HashMap<String, String>,
    /// handle → id, to name the people in group DMs.
    pub handles: HashMap<String, String>,
    /// id → full name, shown beside the display name in the `@` autocomplete.
    #[serde(default)]
    pub real: HashMap<String, String>,
    /// Apps and Slackbot: their DMs stay out of "Mensagens diretas".
    #[serde(default)]
    pub bots: HashSet<String>,
    /// Deactivated accounts: out of the sidebar, found by searching (last,
    /// dim, photo in gray); their DMs open as read-only archives.
    #[serde(default)]
    pub deleted: HashSet<String>,
    /// id → 72px profile picture.
    #[serde(default)]
    pub avatars: HashMap<String, String>,
    /// id → (emoji shortcode, text) of a status that had not expired when read;
    /// `status()` leaves out the ones that expired since.
    #[serde(default)]
    pub statuses: HashMap<String, (String, String)>,
    /// id → when that status clears itself (unix seconds), for those that do.
    #[serde(default)]
    pub status_ends: HashMap<String, i64>,
    /// id → the status emoji as Slack draws it, when Slack's name for it
    /// isn't the usual one (`knife_fork_plate` is 🍽).
    #[serde(default)]
    pub status_unicode: HashMap<String, String>,
    /// id → the picture Slack sent with a custom status emoji, for names the
    /// workspace's list doesn't have (aliases, another org's emoji).
    #[serde(default)]
    pub status_image: HashMap<String, String>,
    /// bot id → (name, icon), for app messages that came without `bot_profile`.
    #[serde(default)]
    pub apps: HashMap<String, (String, String)>,
}

/// The "picture address" of Slack's logo, drawn from the bundled file.
pub const SLACK_LOGO: &str = "slick:slack-logo";

impl Users {
    /// "Slack" (USLACK, its system notices) is no one users.list names: an
    /// app, its picture Slack's logo.
    pub fn with_system(mut self) -> Self {
        self.bots.insert("USLACK".into());
        self.avatars.insert("USLACK".into(), SLACK_LOGO.into());
        self
    }

    /// Someone's status, unless it has expired meanwhile.
    pub fn status(&self, uid: &str) -> Option<&(String, String)> {
        let s = self.statuses.get(uid)?;
        self.status_ends.get(uid).is_none_or(|end| *end > unix_now()).then_some(s)
    }

    /// Someone's status emoji, drawn: by its name, or by the code points
    /// Slack sent along with it.
    pub fn status_glyph(&self, uid: &str) -> Option<String> {
        let (emoji, _) = self.status(uid)?;
        crate::emoji::standard(emoji.trim_matches(':')).or_else(|| self.status_unicode.get(uid).cloned())
    }

    /// Takes in what a user object says (from `users.list`, `users.info` or
    /// the websocket's `user_change`): name, photo, status, whether it's
    /// deactivated. A field it doesn't carry stays as it was.
    pub fn apply(&mut self, u: &Value) {
        let Some(id) = u["id"].as_str().filter(|s| !s.is_empty()).map(String::from) else { return };
        let p = &u["profile"];
        if let Some(name) = [&p["display_name"], &p["real_name"], &u["name"]].iter().filter_map(|v| v.as_str()).find(|s| !s.is_empty()) {
            self.names.insert(id.clone(), name.to_string());
        }
        if let Some(handle) = u["name"].as_str().filter(|s| !s.is_empty()) {
            self.handles.insert(handle.to_string(), id.clone());
        }
        if let Some(real) = p["real_name"].as_str().filter(|r| !r.is_empty()) {
            self.real.insert(id.clone(), real.to_string());
        }
        if u["is_bot"].as_bool() == Some(true) || id == "USLACKBOT" {
            self.bots.insert(id.clone());
        }
        match u["deleted"].as_bool() {
            Some(true) => self.deleted.insert(id.clone()),
            Some(false) => self.deleted.remove(&id),
            None => false,
        };
        if let Some(img) = p["image_72"].as_str().filter(|_| id != "USLACK") {
            self.avatars.insert(id.clone(), img.to_string());
        }
        // A profile without the status fields says nothing about it.
        if p["status_emoji"].is_string() || p["status_text"].is_string() {
            let (emoji, text) = (str_of(&p["status_emoji"]), str_of(&p["status_text"]));
            let end = p["status_expiration"].as_i64().unwrap_or(0);
            if (!emoji.is_empty() || !text.is_empty()) && (end == 0 || end > unix_now()) {
                self.statuses.insert(id.clone(), (emoji, text));
            } else {
                self.statuses.remove(&id);
            }
            let info = &p["status_emoji_display_info"][0];
            match info["unicode"].as_str().and_then(crate::text::emoji) {
                Some(u) => self.status_unicode.insert(id.clone(), u),
                None => self.status_unicode.remove(&id),
            };
            match info["display_url"].as_str().filter(|u| u.starts_with("http") && info["unicode"].is_null()) {
                Some(url) => self.status_image.insert(id.clone(), url.to_string()),
                None => self.status_image.remove(&id),
            };
            if end > 0 {
                self.status_ends.insert(id, end);
            } else {
                self.status_ends.remove(&id);
            }
        }
    }
}

pub(crate) fn unix_now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Kind {
    Channel,
    Private,
    Direct,
    Group,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Conversation {
    pub id: String,
    pub kind: Kind,
    pub name: String,
    /// The other person, for direct messages.
    pub user: Option<String>,
    /// Shared with another organization: Slack lists it under "Slack Connect".
    #[serde(default)]
    pub ext_shared: bool,
    /// An archived channel: out of the sidebar, found by searching, read-only.
    #[serde(default)]
    pub archived: bool,
    /// The workspace's default channel (#general), opened when nothing else is.
    #[serde(default)]
    pub general: bool,
    /// A public channel the user is not in, opened from the search to read:
    /// never in the sidebar nor the cache, gone once another is opened.
    #[serde(skip)]
    pub guest: bool,
    /// When Slack last changed it, in Unix milliseconds: the search's
    /// order for one whose last message is unknown.
    #[serde(default)]
    pub updated: i64,
    /// Slack flagged it dormant (30 days without a message): "Active only"
    /// leaves it out of the sidebar.
    #[serde(default)]
    pub dormant: bool,
}

/// The workspace's public channels, the user's or not, for the search to
/// find those the user is not in, as Slack's does. Kept apart from the
/// sidebar's cache: it can run to thousands.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Directory {
    /// When it was fetched, in Unix seconds; 0 while there is none.
    pub at: i64,
    /// In name order, but for channels added as messages named them (`add`).
    pub chans: Vec<DirChan>,
    /// Each channel's place in `chans`, by id (`prepare`).
    #[serde(skip)]
    index: HashMap<String, usize>,
}

/// Only what the search and the preview need of a channel.
#[derive(Clone, Serialize, Deserialize)]
pub struct DirChan {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub members: u32,
    /// When it last changed (else was created), in Unix milliseconds: the
    /// search orders by it, having no last message for these.
    #[serde(default)]
    pub updated: i64,
    /// The name made ready for the search (`Directory::prepare`).
    #[serde(skip)]
    pub folded: crate::text::Folded,
}

impl Directory {
    /// The channel as a conversation to open, flagged as one the user is not in.
    pub fn conv(&self, i: usize) -> Conversation {
        let d = &self.chans[i];
        Conversation {
            id: d.id.clone(),
            kind: Kind::Channel,
            name: d.name.clone(),
            user: None,
            ext_shared: false,
            archived: d.archived,
            general: false,
            guest: true,
            updated: d.updated,
            dormant: false,
        }
    }

    /// Folds the names for the search, once for all its keys, and indexes the ids.
    pub fn prepare(&mut self) {
        for c in &mut self.chans {
            c.folded = crate::text::Folded::new(&c.name);
        }
        self.index = self.chans.iter().enumerate().map(|(i, c)| (c.id.clone(), i)).collect();
    }

    /// Fetched at `at`; ready for the search once `prepare`d.
    pub fn new(at: i64, chans: Vec<DirChan>) -> Self {
        Self { at, chans, index: HashMap::new() }
    }

    pub fn find(&self, id: &str) -> Option<usize> {
        self.index.get(id).copied()
    }

    pub fn name(&self, id: &str) -> Option<&str> {
        self.find(id).map(|i| self.chans[i].name.as_str())
    }

    /// A public channel learnt of another way (a message mentioned it).
    pub fn add(&mut self, c: &Conversation) {
        if self.find(&c.id).is_some() || c.kind != Kind::Channel {
            return;
        }
        self.index.insert(c.id.clone(), self.chans.len());
        self.chans.push(DirChan {
            id: c.id.clone(),
            name: c.name.clone(),
            archived: c.archived,
            members: 0,
            updated: c.updated,
            folded: crate::text::Folded::new(&c.name),
        });
    }
}

/// A sidebar section as the user arranged it in Slack.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Section {
    pub id: String,
    /// standard, user_group, stars, direct_messages, channels, …
    pub kind: String,
    pub name: String,
    pub emoji: String,
    pub ids: Vec<String>,
    /// Hidden in Slack (a user group's section the user put away).
    #[serde(default)]
    pub hidden: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Count {
    pub latest: String,
    /// Up to where it was read; what comes after is unread.
    #[serde(default)]
    pub last_read: String,
    pub unread: bool,
    pub mentions: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct File {
    pub id: String,
    pub name: String,
    /// A thumbnail to download, for images only.
    pub thumb: Option<String>,
    /// The original, for the enlarged view.
    #[serde(default)]
    pub full: Option<String>,
    pub w: u32,
    pub h: u32,
    /// The file's page on Slack, for "ver no navegador".
    #[serde(default)]
    pub permalink: Option<String>,
    /// The original to save; none for external (Google Drive…) and deleted files.
    #[serde(default)]
    pub download: Option<String>,
    #[serde(default)]
    pub size: u64,
    /// `pdf`, `xlsx`, `python`… and Slack's own label for it, already translated.
    #[serde(default)]
    pub filetype: String,
    #[serde(default)]
    pub pretty_type: String,
    #[serde(default)]
    pub mimetype: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    pub ts: String,
    pub user: Option<String>,
    pub bot_name: Option<String>,
    /// An app's message: whose, to name it when `bot_name` is missing.
    #[serde(default)]
    pub bot_id: Option<String>,
    /// The app's picture: the one the message set, else its profile's.
    #[serde(default)]
    pub bot_icon: Option<String>,
    pub subtype: Option<String>,
    pub text: String,
    #[serde(default)]
    pub blocks: Value,
    #[serde(default)]
    pub attachments: Value,
    #[serde(default)]
    pub files: Vec<File>,
    #[serde(default)]
    pub reply_count: u64,
    /// The thread it belongs to: its own ts for a parent, the parent's for a reply.
    #[serde(default)]
    pub thread_ts: Option<String>,
    #[serde(default)]
    pub latest_reply: Option<String>,
    /// Who replied, for the faces next to "N respostas".
    #[serde(default)]
    pub reply_users: Vec<String>,
    #[serde(default)]
    pub reactions: Vec<crate::emoji::Reaction>,
    /// Set once the text was changed: who and when.
    #[serde(default)]
    pub edited: Option<Edited>,
    /// The conversations it is pinned in.
    #[serde(default)]
    pub pinned_to: Vec<String>,
    /// A thread parent: whether its replies notify you.
    #[serde(default)]
    pub subscribed: bool,
    /// What a system message (topic, rename, join…) is about, to write it in
    /// the app's language. None in caches from before it: then `text` is used.
    #[serde(default)]
    pub system: Option<System>,
}

/// The fields of a system message that Slack's own (localized) text is made of.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct System {
    pub topic: Option<String>,
    pub purpose: Option<String>,
    pub name: Option<String>,
    pub old_name: Option<String>,
    pub inviter: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Edited {
    pub user: String,
    pub ts: String,
}

/// What the profile card shows about someone.
#[derive(Clone, Debug, Default)]
pub struct Profile {
    pub name: String,
    pub real_name: String,
    /// As typed, possibly empty; `name` falls back to the real name.
    pub display_name: String,
    pub title: String,
    pub status: String,
    pub email: String,
    pub phone: String,
    /// Seconds east of UTC.
    pub tz_offset: i32,
    /// "Horário Padrão de Brasília", or the zone's name.
    pub tz_label: String,
    pub avatar: String,
    /// The 72px picture the rest of the app uses.
    pub thumb: String,
    /// The largest photo there is, for the enlarged view.
    pub photo: String,
}

/// What the official client knows at start-up and the API does not say
/// elsewhere: which DMs are open in the sidebar and how the sidebar behaves.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Boot {
    /// DMs and group DMs open in the sidebar ("Fechar conversa" closes one).
    pub open: HashSet<String>,
    pub sidebar_avatars: bool,
    /// The sidebar's "Show" (`sidebar_behavior`): "" for all,
    /// `hide_inactive_channels`, `hide_read_channels`, `show_mentions_only`,
    /// or `hide_read_channels_unless_starred` for "Custom, depending on the section".
    #[serde(default)]
    pub sidebar_behavior: String,
    /// The `channel_sections` pref: each section's settings, its filter
    /// under `sidebar`, as the official client writes them.
    #[serde(default)]
    pub section_prefs: serde_json::Map<String, Value>,
    /// The team's plan, "" when free; `None` until known.
    #[serde(default)]
    pub plan: Option<String>,
    /// The new Slackbot has its own link above Threads, in its DM's place.
    #[serde(default)]
    pub slackbot_link: bool,
}

/// Messages per history page; a shorter page means the start was reached.
pub const HISTORY_LIMIT: usize = 60;

impl Slack {
    pub fn new(creds: Creds) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(20))
            .user_agent(concat!("slick/", env!("CARGO_PKG_VERSION")))
            .build();
        Self {
            agent,
            creds: Arc::new(RwLock::new(creds)),
            wanted: Default::default(),
            away: Default::default(),
            boot_cache: Default::default(),
            org_cache: Default::default(),
        }
    }

    /// The session in use (`config::save_workspace` after `auth.test`, the cookie check).
    pub(crate) fn creds(&self) -> Creds {
        self.creds.read().map(|c| c.clone()).unwrap_or_default()
    }

    /// Whose presence to hear about. The socket sends it within seconds, and only when it changed.
    pub fn subscribe(&self, mut ids: Vec<String>) {
        ids.sort();
        ids.dedup();
        if let Ok(mut w) = self.wanted.lock() {
            *w = ids;
        }
    }

    /// The presence list as last set; each connection subscribes to it (again) after `hello`.
    pub fn wanted(&self) -> Vec<String> {
        self.wanted.lock().map(|w| w.clone()).unwrap_or_default()
    }

    /// Whether you set yourself away by hand, as last heard.
    pub fn set_away(&self, away: bool) {
        self.away.store(away, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn away(&self) -> bool {
        self.away.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// The real-time websocket's address, and the session cookie it also wants.
    /// A browser session connects the way the official client does
    /// (`client.getWebSocketURL` plus its flannel parameters): only that
    /// socket hears when someone else's status or profile changes
    /// (`user_invalidated`). `rtm.connect` hands out a legacy bot socket
    /// that never does; it stays as the fallback, and for other tokens.
    pub fn rtm_url(&self) -> Res<(String, Option<String>)> {
        let creds = self.creds();
        if creds.cookie.is_some() {
            if let Ok(url) = self.client_ws_url(&creds.token) {
                return Ok((url, creds.cookie));
            }
        }
        let v = self.call("rtm.connect", &[])?;
        let url = v["url"].as_str().filter(|u| !u.is_empty()).ok_or("rtm.connect: no url")?;
        Ok((url.to_string(), creds.cookie))
    }

    /// The official client's socket address: the gateway from
    /// `client.getWebSocketURL`, and the token and flags it adds itself.
    fn client_ws_url(&self, token: &str) -> Res<String> {
        let v = self.call("client.getWebSocketURL", &[])?;
        let base = v["primary_websocket_url"].as_str().filter(|u| u.starts_with("wss://")).ok_or("client.getWebSocketURL: no url")?;
        let start_args = "?agent=client&org_wide_aware=true&eac_cache_ts=true&cache_ts=0&name_tagging=true&only_self_subteams=true&connect_only=true&ms_latest=true";
        let mut q = vec![
            ("token", token),
            ("sync_desync", "1"),
            ("slack_client", "desktop"),
            ("start_args", start_args),
            ("no_query_on_subscribe", "1"),
            ("flannel", "3"),
            ("lazy_channels", "1"),
        ];
        if let Some(g) = v["routing_context"].as_str() {
            q.push(("gateway_server", g));
        }
        q.push(("batch_presence_aware", "1"));
        let q: Vec<String> = q.iter().map(|(k, v)| format!("{k}={}", form_encode(v))).collect();
        Ok(format!("{}/?{}", base.trim_end_matches('/'), q.join("&")))
    }

    pub fn set_creds(&self, creds: Creds) {
        if let Ok(mut c) = self.creds.write() {
            *c = creds;
        }
        // Another session: nothing the old one was told is its.
        *self.boot_cache.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *self.org_cache.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    fn authed(&self, req: ureq::Request) -> ureq::Request {
        let creds = self.creds();
        let req = req.set("Authorization", &format!("Bearer {}", creds.token));
        match &creds.cookie {
            Some(c) => req.set("Cookie", &format!("d={c}")),
            None => req,
        }
    }

    pub fn call(&self, method: &str, params: &[(&str, &str)]) -> Res<Value> {
        #[cfg(feature = "demo")]
        if crate::demo::on() {
            return crate::demo::call(&self.creds().token, method, params);
        }
        let url = format!("https://slack.com/api/{method}");
        let creds = self.creds();
        let mut form: Vec<(&str, &str)> = params.to_vec();
        if creds.cookie.is_some() {
            form.push(("token", &creds.token));
        }
        for _ in 0..8 {
            match self.authed(self.agent.post(&url)).send_form(&form) {
                Ok(resp) => {
                    let v: Value = resp.into_json().map_err(|e| e.to_string())?;
                    if v["ok"].as_bool() == Some(true) {
                        return Ok(v);
                    }
                    return Err(format!("{method}: {}", v["error"].as_str().unwrap_or(tr("unknown error", "erro desconhecido"))));
                }
                Err(ureq::Error::Status(429, resp)) => {
                    let secs = resp
                        .header("Retry-After")
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(2u64);
                    std::thread::sleep(Duration::from_secs(secs.min(30)));
                }
                Err(e) => return Err(format!("{method}: {e}")),
            }
        }
        Err(format!("{method}: rate limited"))
    }

    /// A message sent with its markup as `rich_text` blocks, like the official
    /// clients; `text` stays as the notifications' fallback. Should Slack
    /// refuse the blocks, it goes again as text alone.
    pub(crate) fn call_rich(&self, method: &str, params: &[(&str, &str)], typed: &str) -> Res<Value> {
        let blocks = crate::markup::blocks(typed).to_string();
        let mut p = params.to_vec();
        p.push(("blocks", &blocks));
        match self.call(method, &p) {
            Err(e) if ["invalid_blocks", "invalid_blocks_format", "invalid_arguments"].iter().any(|c| e.ends_with(c)) => self.call(method, params),
            res => res,
        }
    }

    pub(crate) fn paginate(&self, method: &str, params: &[(&str, &str)], key: &str) -> Res<Vec<Value>> {
        let mut out = Vec::new();
        self.paginate_each(method, params, key, |v| out.push(v.clone()))?;
        Ok(out)
    }

    /// `paginate`, each item handed to `each` as its page arrives instead of
    /// all of them kept: for lists too long to hold whole.
    pub(crate) fn paginate_each(&self, method: &str, params: &[(&str, &str)], key: &str, mut each: impl FnMut(&Value)) -> Res<()> {
        let mut cursor = String::new();
        loop {
            let v = {
                let mut p = params.to_vec();
                if !cursor.is_empty() {
                    p.push(("cursor", &cursor));
                }
                self.call(method, &p)?
            };
            v[key].as_array().into_iter().flatten().for_each(&mut each);
            cursor = v["response_metadata"]["next_cursor"].as_str().unwrap_or("").to_string();
            if cursor.is_empty() {
                return Ok(());
            }
        }
    }

    pub fn auth(&self) -> Res<AuthInfo> {
        let v = self.call("auth.test", &[])?;
        Ok(AuthInfo {
            user_id: str_of(&v["user_id"]),
            user: str_of(&v["user"]),
            team: str_of(&v["team"]),
            team_id: str_of(&v["team_id"]),
            domain: crate::config::workspace_name(&str_of(&v["url"])).unwrap_or_default(),
        })
    }

    /// The workspace's icon, for the corner of the sidebar.
    pub fn team_icon(&self) -> Res<String> {
        let v = self.call("team.info", &[])?;
        let icon = &v["team"]["icon"];
        ["image_132", "image_88", "image_68"]
            .iter()
            .find_map(|k| icon[*k].as_str().map(String::from))
            .ok_or_else(|| tr("no icon", "sem ícone").to_string())
    }

    pub fn profile(&self, uid: &str) -> Res<Profile> {
        let v = self.call("users.info", &[("user", uid)])?;
        let (u, p) = (&v["user"], &v["user"]["profile"]);
        let real_name = str_of(&p["real_name"]);
        let name = [&p["display_name"], &p["real_name"], &u["name"]]
            .iter()
            .filter_map(|v| v.as_str())
            .find(|s| !s.is_empty())
            .unwrap_or("?")
            .to_string();
        // Slack may still return a status for a while after it expired.
        let end = p["status_expiration"].as_i64().unwrap_or(0);
        let status = [str_of(&p["status_emoji"]), str_of(&p["status_text"])]
            .into_iter()
            .filter(|s| !s.is_empty() && (end == 0 || end > unix_now()))
            .collect::<Vec<_>>()
            .join(" ");
        Ok(Profile {
            name,
            real_name,
            display_name: str_of(&p["display_name"]),
            title: str_of(&p["title"]),
            status,
            email: str_of(&p["email"]),
            phone: str_of(&p["phone"]),
            tz_offset: u["tz_offset"].as_i64().unwrap_or(0) as i32,
            tz_label: [str_of(&u["tz_label"]), str_of(&u["tz"])].into_iter().find(|s| !s.is_empty()).unwrap_or_default(),
            thumb: str_of(&p["image_72"]),
            avatar: [&p["image_512"], &p["image_192"], &p["image_72"]].iter().filter_map(|v| v.as_str()).next().unwrap_or("").to_string(),
            photo: [&p["image_original"], &p["image_1024"], &p["image_512"], &p["image_192"]]
                .iter()
                .filter_map(|v| v.as_str())
                .next()
                .unwrap_or("")
                .to_string(),
        })
    }

    /// Your own photo.
    pub fn set_photo(&self, path: &std::path::Path) -> Res<()> {
        let mime = image_mime(path).ok_or_else(|| tr("the photo must be a JPG, PNG or GIF", "a foto precisa ser JPG, PNG ou GIF").to_string())?;
        self.multipart("users.setPhoto", &[], "image", path, mime).map(|_| ())
    }

    /// A call carrying a file, such as `users.setPhoto` and `emoji.add`:
    /// they only take multipart, which ureq 2 lacks, so the body is put
    /// together by hand.
    pub fn multipart(&self, method: &str, params: &[(&str, &str)], field: &str, path: &std::path::Path, mime: &str) -> Res<Value> {
        #[cfg(feature = "demo")]
        if crate::demo::on() {
            return crate::demo::call(&self.creds().token, method, params);
        }
        let data = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let file = path.file_name().map(|n| n.to_string_lossy().replace('"', "")).unwrap_or_else(|| "image".into());
        let boundary = format!("slick{:x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
        let creds = self.creds();
        let mut body = Vec::new();
        let mut fields: Vec<(&str, &str)> = params.to_vec();
        if creds.cookie.is_some() {
            fields.push(("token", &creds.token));
        }
        for (k, v) in fields {
            write!(body, "--{boundary}\r\nContent-Disposition: form-data; name=\"{k}\"\r\n\r\n{v}\r\n").map_err(|e| e.to_string())?;
        }
        write!(body, "--{boundary}\r\nContent-Disposition: form-data; name=\"{field}\"; filename=\"{file}\"\r\nContent-Type: {mime}\r\n\r\n")
            .map_err(|e| e.to_string())?;
        body.extend_from_slice(&data);
        write!(body, "\r\n--{boundary}--\r\n").map_err(|e| e.to_string())?;
        let req = self.authed(self.agent.post(&format!("https://slack.com/api/{method}")));
        let resp = req
            .set("Content-Type", &format!("multipart/form-data; boundary={boundary}"))
            .send_bytes(&body)
            .map_err(|e| format!("{method}: {e}"))?;
        let v: Value = resp.into_json().map_err(|e| e.to_string())?;
        match v["ok"].as_bool() {
            Some(true) => Ok(v),
            _ => Err(format!("{method}: {}", v["error"].as_str().unwrap_or(tr("unknown error", "erro desconhecido")))),
        }
    }

    pub fn users(&self) -> Res<Users> {
        let list = self.paginate("users.list", &[("limit", "200")], "members")?;
        let mut users = Users::default();
        for u in &list {
            users.apply(u);
            // Someone without any name still gets a row.
            if let Some(id) = u["id"].as_str().filter(|id| !users.names.contains_key(*id)) {
                users.names.insert(id.to_string(), "?".into());
            }
        }
        Ok(users.with_system())
    }

    /// An app's name and picture, for its messages that don't carry them.
    pub fn bot(&self, id: &str) -> Res<(String, String)> {
        let v = self.call("bots.info", &[("bot", id)])?;
        let b = &v["bot"];
        Ok((str_of(&b["name"]), icon_of(&b["icons"]).unwrap_or_default()))
    }

    /// The app behind a bot, as Slack's "App" panel shows it: name,
    /// description, icon (`apps.profile.get`, undocumented, the web client's),
    /// and its bot user, empty for those without one (webhooks).
    pub fn app_profile(&self, bot: &str) -> Res<(String, Profile)> {
        let v = self.call("bots.info", &[("bot", bot)])?;
        let b = &v["bot"];
        let app = str_of(&b["app_id"]);
        let user = str_of(&b["user_id"]);
        let a = if app.is_empty() { Value::Null } else { self.call("apps.profile.get", &[("app", &app)]).map(|v| v["app_profile"].clone()).unwrap_or_default() };
        let user = a["bot_user"]["id"].as_str().filter(|u| !u.is_empty()).map(String::from).unwrap_or(user);
        let pick = |icons: &Value, keys: &[&str]| keys.iter().find_map(|k| icons[*k].as_str().filter(|u| u.starts_with("http")).map(String::from));
        let (icons, own) = (&a["icons"], &b["icons"]);
        let thumb = pick(icons, &["image_72", "image_64", "image_48"]).or_else(|| icon_of(own)).unwrap_or_default();
        Ok((user, Profile {
            name: a["name"].as_str().filter(|n| !n.is_empty()).map(String::from).unwrap_or_else(|| str_of(&b["name"])),
            title: str_of(&a["desc"]),
            avatar: pick(icons, &["image_512", "image_192", "image_128"]).unwrap_or_else(|| thumb.clone()),
            photo: pick(icons, &["image_1024", "image_original", "image_512"]).unwrap_or_else(|| thumb.clone()),
            thumb,
            ..Default::default()
        }))
    }

    /// One person's user object as it is now, for `Users::apply`.
    pub fn user(&self, uid: &str) -> Res<Value> {
        let v = self.call("users.info", &[("user", uid)])?;
        Ok(v["user"].clone())
    }

    /// Only the conversations the user is in, archived channels included:
    /// Slack keeps their members, so the same call lists them, a page or so more.
    pub fn conversations(&self) -> Res<Vec<Conversation>> {
        let list = match self.paginate(
            "users.conversations",
            &[
                ("types", "public_channel,private_channel,mpim,im"),
                ("exclude_archived", "false"),
                ("limit", "200"),
            ],
            "channels",
        ) {
            Ok(list) => list,
            // An Enterprise Grid org can forbid the method; the official client's start-up call still lists them.
            Err(e) if e.ends_with("enterprise_is_restricted") => return self.boot_conversations().map_err(|_| e),
            Err(e) => return Err(e),
        };
        Ok(list
            .iter()
            .filter(|c| !c["is_user_deleted"].as_bool().unwrap_or(false))
            .map(parse_conversation)
            .collect())
    }

    /// The user's conversations from `client.userBoot`, for where `users.conversations`
    /// is restricted: its `channels` (group DMs among them), `mpims` should it ever
    /// have them, and `ims`. An open one it leaves out (`has_more_mpdms`) is asked
    /// for on its own. Session tokens only.
    fn boot_conversations(&self) -> Res<Vec<Conversation>> {
        let v = self.user_boot()?;
        let mut seen = HashSet::new();
        let mut out: Vec<Conversation> = ["channels", "mpims", "ims"]
            .iter()
            .flat_map(|k| v[*k].as_array().into_iter().flatten())
            .filter(|c| !c["is_user_deleted"].as_bool().unwrap_or(false))
            .map(parse_conversation)
            .filter(|c| !c.id.is_empty() && seen.insert(c.id.clone()))
            .collect();
        let missing: Vec<String> =
            v["is_open"].as_array().into_iter().flatten().filter_map(|i| i.as_str()).filter(|i| !seen.contains(*i)).map(String::from).collect();
        out.extend(missing.iter().filter_map(|id| self.conversation(id).ok()));
        Ok(out)
    }

    /// Every public channel of the workspace, archived ones too (`Directory`).
    pub fn directory(&self) -> Res<Vec<DirChan>> {
        let mut out = Vec::new();
        let params = [("types", "public_channel"), ("exclude_archived", "false"), ("limit", "1000")];
        let dir = |c: &Value| DirChan {
            id: str_of(&c["id"]),
            name: str_of(&c["name"]),
            archived: c["is_archived"].as_bool() == Some(true),
            members: c["num_members"].as_u64().unwrap_or(0) as u32,
            updated: updated(c),
            folded: Default::default(),
        };
        match self.paginate_each("conversations.list", &params, "channels", |c| out.push(dir(c))) {
            Ok(()) => {}
            // Restricted on some Enterprise Grid orgs: the channel browser's own search
            // lists them there, private ones it can see too, without member counts.
            Err(e) if e.ends_with("enterprise_is_restricted") => {
                out.clear();
                let all = self.edge_pages("channels/search", json!({ "query": "", "count": 1000 })).map_err(|_| e)?;
                out.extend(all.iter().filter(|c| c["is_private"].as_bool() != Some(true)).map(dir));
            }
            Err(e) => return Err(e),
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// A channel's members from the edge API, for where `conversations.members`
    /// is restricted (an Enterprise Grid org); session tokens only.
    pub fn edge_members(&self, chan: &str) -> Res<Vec<String>> {
        let all = self.edge_pages("users/list", json!({ "channels": [chan], "filter": "everyone", "count": 100 }))?;
        Ok(all.iter().filter_map(|u| u["id"].as_str().map(String::from)).collect())
    }

    /// Every page of an edge API list (`results`, `next_marker`), up to 50.
    fn edge_pages(&self, path: &str, body: Value) -> Res<Vec<Value>> {
        let org = self.org()?;
        let mut out = Vec::new();
        let mut body = body;
        for _ in 0..50 {
            let v = self.edge(&org, path, &body)?;
            out.extend(v["results"].as_array().into_iter().flatten().cloned());
            match v["next_marker"].as_str().filter(|m| !m.is_empty()) {
                Some(m) => body["marker"] = json!(m),
                None => break,
            }
        }
        Ok(out)
    }

    /// The id the edge API is addressed by: the org's on Enterprise Grid, else the workspace's.
    fn org(&self) -> Res<String> {
        let token = self.creds().token;
        let mut cache = self.org_cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, org)) = cache.as_ref().filter(|(t, _)| *t == token) {
            return Ok(org.clone());
        }
        let v = self.call("auth.test", &[])?;
        let org = [&v["enterprise_id"], &v["team_id"]].iter().find_map(|i| i.as_str().filter(|s| !s.is_empty())).unwrap_or_default().to_string();
        *cache = Some((token, org.clone()));
        Ok(org)
    }

    /// `client.userBoot`, its answer shared for `BOOT_FOR` by `boot` and the
    /// fallbacks of `conversations` and `stars`, which start-up runs together:
    /// the first one asks, the others wait for it and take its answer.
    fn user_boot(&self) -> Res<Value> {
        let token = self.creds().token;
        let mut cache = self.boot_cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, _, v)) = cache.as_ref().filter(|(t, at, _)| *t == token && at.elapsed() < BOOT_FOR) {
            return Ok(v.clone());
        }
        let v = self.call("client.userBoot", &[])?;
        *cache = Some((token, Instant::now(), v.clone()));
        Ok(v)
    }

    /// Slack's edge API (`edgeapi.slack.com/cache/<org>/<path>`), which the web
    /// client asks for some lists. Undocumented: only session tokens get it.
    fn edge(&self, org: &str, path: &str, body: &Value) -> Res<Value> {
        #[cfg(feature = "demo")]
        if crate::demo::on() {
            return crate::demo::call(&self.creds().token, &format!("edge/{path}"), &[("channel", body["channels"][0].as_str().unwrap_or(""))]);
        }
        let mut body = body.clone();
        body["token"] = json!(self.creds().token);
        let url = format!("https://edgeapi.slack.com/cache/{org}/{path}");
        // Rate limits as in `call`.
        for _ in 0..8 {
            match self.authed(self.agent.post(&url)).send_json(&body) {
                Ok(resp) => {
                    let v: Value = resp.into_json().map_err(|e| e.to_string())?;
                    if v["ok"].as_bool() == Some(true) {
                        return Ok(v);
                    }
                    return Err(format!("edge {path}: {}", v["error"].as_str().unwrap_or(tr("unknown error", "erro desconhecido"))));
                }
                Err(ureq::Error::Status(429, resp)) => {
                    let secs = resp.header("Retry-After").and_then(|s| s.parse().ok()).unwrap_or(2u64);
                    std::thread::sleep(Duration::from_secs(secs.min(30)));
                }
                Err(e) => return Err(format!("edge {path}: {e}")),
            }
        }
        Err(format!("edge {path}: rate limited"))
    }

    /// Joins a public channel; Slack answers with it as the user now sees it.
    pub fn join(&self, chan: &str) -> Res<Conversation> {
        let v = self.call("conversations.join", &[("channel", chan)])?;
        Ok(parse_conversation(&v["channel"]))
    }

    /// The DM or group DM with these people, created if there is none yet.
    pub fn open_conversation(&self, users: &[String]) -> Res<Conversation> {
        let v = self.call("conversations.open", &[("users", &users.join(",")), ("return_im", "true")])?;
        let c = &v["channel"];
        // Some answers carry only the id; ask for the rest.
        if c["is_im"].is_null() && c["is_mpim"].is_null() {
            return self.conversation(&str_of(&c["id"]));
        }
        Ok(parse_conversation(c))
    }

    pub fn conversation(&self, id: &str) -> Res<Conversation> {
        let v = self.call("conversations.info", &[("channel", id)])?;
        Ok(parse_conversation(&v["channel"]))
    }

    pub fn create_channel(&self, name: &str, private: bool) -> Res<Conversation> {
        let v = self.call("conversations.create", &[("name", name), ("is_private", if private { "true" } else { "false" })])?;
        Ok(parse_conversation(&v["channel"]))
    }

    /// Brings an archived channel back; it takes messages again.
    pub fn unarchive(&self, chan: &str) -> Res<()> {
        self.call("conversations.unarchive", &[("channel", chan)]).map(|_| ())
    }

    /// The channel's description, as Slack calls it in the UI.
    pub fn set_purpose(&self, chan: &str, purpose: &str) -> Res<()> {
        self.call("conversations.setPurpose", &[("channel", chan), ("purpose", purpose)]).map(|_| ())
    }

    /// The user's sidebar sections. Undocumented: only session tokens get it.
    pub fn sections(&self) -> Res<Vec<Section>> {
        let v = self.call("users.channelSections.list", &[])?;
        Ok(v["channel_sections"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|s| Section {
                id: str_of(&s["channel_section_id"]),
                kind: str_of(&s["type"]),
                name: str_of(&s["name"]),
                emoji: str_of(&s["emoji"]),
                ids: s["channel_ids_page"]["channel_ids"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|i| i.as_str().map(String::from))
                    .collect(),
                hidden: s["is_hidden"].as_bool() == Some(true),
            })
            .collect())
    }

    /// Undocumented, like `sections`: the start-up call of Slack's web client.
    pub fn boot(&self) -> Res<Boot> {
        let v = self.user_boot()?;
        Ok(Boot {
            open: v["is_open"].as_array().into_iter().flatten().filter_map(|i| i.as_str().map(String::from)).collect(),
            sidebar_avatars: v["prefs"]["show_sidebar_avatars"].as_bool().unwrap_or(true),
            sidebar_behavior: str_of(&v["prefs"]["sidebar_behavior"]),
            section_prefs: section_prefs(&v["prefs"]["channel_sections"]),
            plan: v["team"]["plan"].as_str().map(String::from),
            // Slack asks an edge API; its link's "New" tag seen, or big mode had, tells it here.
            slackbot_link: v["prefs"]["enable_slackbot_view"].as_bool() != Some(false)
                && (v["prefs"]["has_seen_slackbot_ai_new_tag"].as_bool() == Some(true)
                    || v["prefs"]["had_slackbot_big_mode_access"].as_bool() == Some(true)),
        })
    }

    /// One of the user's preferences (`users.prefs.set`), its value as text.
    pub fn set_pref(&self, name: &str, value: &str) -> Res<()> {
        self.call("users.prefs.set", &[("name", name), ("value", value)]).map(|_| ())
    }

    /// Takes a conversation out of one custom section and/or into another.
    /// Leaving both default sections out puts it back in DMs or Canais.
    pub fn move_to_section(&self, chan: &str, from: Option<&str>, to: Option<&str>) -> Res<()> {
        let part = |section: &str| serde_json::json!([{ "channel_section_id": section, "channel_ids": [chan] }]).to_string();
        let mut p = Vec::new();
        let (insert, remove) = (to.map(part), from.map(part));
        if let Some(i) = &insert {
            p.push(("insert", i.as_str()));
        }
        if let Some(r) = &remove {
            p.push(("remove", r.as_str()));
        }
        if p.is_empty() {
            return Ok(());
        }
        self.call("users.channelSections.channels.bulkUpdate", &p).map(|_| ())
    }

    pub fn star(&self, chan: &str, on: bool) -> Res<()> {
        self.call(if on { "stars.add" } else { "stars.remove" }, &[("channel", chan)]).map(|_| ())
    }

    /// Slack's VIP list: people whose messages always notify.
    pub fn vips(&self) -> Res<HashSet<String>> {
        let v = self.call("users.priority.list", &[])?;
        let mut out = HashSet::new();
        if let Some(groups) = v["users"].as_object() {
            for ids in groups.values().filter_map(Value::as_array) {
                out.extend(ids.iter().filter_map(|i| i.as_str().map(String::from)));
            }
        }
        Ok(out)
    }

    pub fn vip(&self, user: &str, on: bool) -> Res<()> {
        self.call(if on { "users.priority.add" } else { "users.priority.remove" }, &[("user", user)]).map(|_| ())
    }

    /// Takes a DM out of the sidebar, like "Fechar conversa".
    pub fn close(&self, chan: &str) -> Res<()> {
        self.call("conversations.close", &[("channel", chan)]).map(|_| ())
    }

    /// Starred conversations: Slack's "Favoritos".
    pub fn stars(&self) -> Res<Vec<String>> {
        let items = match self.paginate("stars.list", &[("limit", "200")], "items") {
            Ok(items) => items,
            // Restricted like `users.conversations`: `client.userBoot` has the starred ones too.
            Err(e) if e.ends_with("enterprise_is_restricted") => {
                let v = self.user_boot().map_err(|_| e)?;
                return Ok(v["starred"].as_array().into_iter().flatten().filter_map(|i| i.as_str().map(String::from)).collect());
            }
            Err(e) => return Err(e),
        };
        Ok(items
            .iter()
            .filter(|i| matches!(i["type"].as_str(), Some("im" | "mpim" | "channel" | "group")))
            .filter_map(|i| i["channel"].as_str().map(String::from))
            .collect())
    }

    /// Unread state and last activity for every conversation, in one call.
    /// Undocumented, like `sections`.
    pub fn counts(&self) -> Res<HashMap<String, Count>> {
        self.all_counts().map(|c| c.0)
    }

    /// `counts`, and the threads you follow: whether any has unread replies,
    /// and how many of those mention you.
    pub fn all_counts(&self) -> Res<(HashMap<String, Count>, (bool, u64))> {
        let v = self.call("client.counts", &[])?;
        let threads = (v["threads"]["has_unreads"].as_bool().unwrap_or(false), v["threads"]["mention_count"].as_u64().unwrap_or(0));
        let mut out = HashMap::new();
        for key in ["channels", "ims", "mpims"] {
            for c in v[key].as_array().into_iter().flatten() {
                out.insert(
                    str_of(&c["id"]),
                    Count {
                        latest: str_of(&c["latest"]),
                        last_read: str_of(&c["last_read"]),
                        unread: c["has_unreads"].as_bool().unwrap_or(false),
                        mentions: c["mention_count"].as_u64().unwrap_or(0),
                    },
                );
            }
        }
        Ok((out, threads))
    }

    /// Messages in chronological order. `oldest` fetches what came after it,
    /// `latest` what came before it.
    pub fn history(&self, chan: &str, oldest: Option<&str>, latest: Option<&str>) -> Res<Vec<Message>> {
        let limit = HISTORY_LIMIT.to_string();
        let mut p = vec![("channel", chan), ("limit", limit.as_str())];
        if let Some(o) = oldest {
            p.push(("oldest", o));
        }
        if let Some(l) = latest {
            p.push(("latest", l));
        }
        let v = self.call("conversations.history", &p)?;
        let mut msgs: Vec<Message> = v["messages"]
            .as_array()
            .map(|a| a.iter().map(parse_message).collect())
            .unwrap_or_default();
        msgs.reverse();
        Ok(msgs)
    }

    /// The messages around `ts`, it included: a page up to it and a page after
    /// it, in chronological order. Also whether the first page reached the
    /// conversation's first message and the second its newest one.
    pub fn around(&self, chan: &str, ts: &str) -> Res<(Vec<Message>, bool, bool)> {
        let limit = HISTORY_LIMIT.to_string();
        let page = |p: &[(&str, &str)]| -> Res<Vec<Message>> {
            let v = self.call("conversations.history", p)?;
            Ok(v["messages"].as_array().map(|a| a.iter().map(parse_message).collect()).unwrap_or_default())
        };
        let mut msgs = page(&[("channel", chan), ("limit", &limit), ("latest", ts), ("inclusive", "true")])?;
        let first = msgs.len() < HISTORY_LIMIT;
        msgs.reverse();
        // With only `oldest`, Slack returns the messages closest to it.
        let after = page(&[("channel", chan), ("limit", &limit), ("oldest", ts)])?;
        let present = after.len() < HISTORY_LIMIT;
        let mut after: Vec<Message> = after.into_iter().filter(|m| m.ts.as_str() > ts).collect();
        after.sort_by(|a, b| a.ts.cmp(&b.ts));
        msgs.extend(after);
        Ok((msgs, first, present))
    }

    pub fn post(&self, chan: &str, text: &str) -> Res<Message> {
        let v = self.call_rich("chat.postMessage", &[("channel", chan), ("text", &escape(text))], text)?;
        let mut m = parse_message(&v["message"]);
        if m.ts.is_empty() {
            m.ts = str_of(&v["ts"]);
        }
        Ok(m)
    }

    /// Shares a file into a conversation, as Slack's "Share this file".
    pub fn share_file(&self, file: &str, chan: &str) -> Res<()> {
        self.call("files.share", &[("file", file), ("channel", chan)]).map(|_| ())
    }

    pub fn online(&self, user: &str) -> Res<bool> {
        let v = self.call("users.getPresence", &[("user", user)])?;
        Ok(v["presence"] == "active")
    }

    /// Puts a sidebar section right before `next` (empty: at the end). Saved
    /// on Slack's side, so every client shows the new order.
    pub fn move_section(&self, id: &str, next: &str) -> Res<()> {
        self.call("users.channelSections.set", &[("channel_section_id", id), ("next_channel_section_id", next)])
            .map(|_| ())
    }

    /// A new custom section, empty, right before `next`; its id. Undocumented,
    /// like `sections`: the calls are the web client's.
    pub fn create_section(&self, name: &str, emoji: &str, next: &str) -> Res<String> {
        let v = self.call("users.channelSections.create", &[("name", name), ("emoji", emoji), ("next_channel_section_id", next)])?;
        Ok(str_of(&v["channel_section_id"]))
    }

    /// A custom section's new name and emoji (a shortcode without colons;
    /// empty: none). `set` also places it, so `next` keeps it where it is.
    pub fn rename_section(&self, id: &str, name: &str, emoji: &str, next: &str) -> Res<()> {
        let p = [("channel_section_id", id), ("name", name), ("emoji", emoji), ("next_channel_section_id", next)];
        self.call("users.channelSections.set", &p).map(|_| ())
    }

    /// Asks the app behind a card (a work object) for its latest; it answers
    /// by changing the message.
    pub fn refresh_unfurl(&self, chan: &str, ts: &str, entity: &str) -> Res<()> {
        self.call("chat.refreshUnfurl", &[("channel", chan), ("ts", ts), ("entity_id", entity)]).map(|_| ())
    }

    /// Moves the read cursor so the official clients stop flagging it unread.
    pub fn mark(&self, chan: &str, ts: &str) -> Res<()> {
        self.call("conversations.mark", &[("channel", chan), ("ts", ts)]).map(|_| ())
    }

    /// Files on files.slack.com need the same credentials as the API; anything
    /// else (avatars on gravatar, slack-edge) must never see them.
    pub fn download(&self, url: &str) -> Res<Vec<u8>> {
        #[cfg(feature = "demo")]
        if crate::demo::on() {
            return crate::demo::download(url);
        }
        let req = self.agent.get(url);
        let req = if is_slack(&req) { self.authed(req) } else { req };
        let resp = req.call().map_err(|e| e.to_string())?;
        let mut buf = Vec::new();
        resp.into_reader()
            .take(20 << 20)
            .read_to_end(&mut buf)
            .map_err(|e| e.to_string())?;
        Ok(buf)
    }

    /// Streams a file into `out`, for saving it: no size cap and only connect
    /// and read timeouts, so files of hundreds of MB get through. `size` hears
    /// the file's size, when the server says it.
    pub fn download_to(&self, url: &str, out: &mut dyn Write, html_ok: bool, size: &dyn Fn(u64)) -> Res<u64> {
        #[cfg(feature = "demo")]
        if crate::demo::on() {
            let b = crate::demo::download(url)?;
            size(b.len() as u64);
            return out.write_all(&b).map(|_| b.len() as u64).map_err(|e| e.to_string());
        }
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(20))
            .timeout_read(Duration::from_secs(60))
            .user_agent(concat!("slick/", env!("CARGO_PKG_VERSION")))
            .build();
        let req = agent.get(url);
        let req = if is_slack(&req) { self.authed(req) } else { req };
        let resp = req.call().map_err(|e| e.to_string())?;
        // A session Slack no longer accepts gets its sign-in page, not the file.
        if !html_ok && resp.content_type() == "text/html" {
            return Err(tr("Slack didn't hand over the file (session expired?)", "o Slack não entregou o arquivo (sessão expirada?)").into());
        }
        if let Some(n) = resp.header("Content-Length").and_then(|v| v.trim().parse().ok()) {
            size(n);
        }
        std::io::copy(&mut resp.into_reader(), out).map_err(|e| e.to_string())
    }
}

/// A picture's type by its extension: JPG, PNG or GIF, what Slack takes for
/// photos and emoji.
pub fn image_mime(path: &std::path::Path) -> Option<&'static str> {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        _ => None,
    }
}

/// Only HTTPS Slack hosts get credentials, using the HTTP client's URL parser.
fn is_slack(req: &ureq::Request) -> bool {
    req.request_url().is_ok_and(|url| {
        url.scheme() == "https" && (url.host() == "slack.com" || url.host().ends_with(".slack.com"))
    })
}

fn parse_conversation(c: &Value) -> Conversation {
    let kind = if c["is_im"].as_bool() == Some(true) {
        Kind::Direct
    } else if c["is_mpim"].as_bool() == Some(true) {
        Kind::Group
    } else if c["is_private"].as_bool() == Some(true) {
        Kind::Private
    } else {
        Kind::Channel
    };
    Conversation {
        id: str_of(&c["id"]),
        kind,
        name: str_of(&c["name"]),
        user: c["user"].as_str().map(String::from),
        ext_shared: c["is_ext_shared"].as_bool() == Some(true),
        archived: c["is_archived"].as_bool() == Some(true),
        general: c["is_general"].as_bool() == Some(true),
        guest: false,
        updated: updated(c),
        dormant: c["properties"]["is_dormant"].as_bool() == Some(true),
    }
}

/// The `channel_sections` pref, a JSON object in a string.
pub fn section_prefs(v: &Value) -> serde_json::Map<String, Value> {
    match v {
        Value::String(t) => serde_json::from_str::<Value>(t).ok(),
        o => Some(o.clone()),
    }
    .and_then(|v| v.as_object().cloned())
    .unwrap_or_default()
}

/// A conversation's `updated` (milliseconds), else its `created` (seconds), as Unix milliseconds.
fn updated(c: &Value) -> i64 {
    c["updated"].as_i64().filter(|t| *t > 0).or_else(|| c["created"].as_i64().filter(|t| *t > 0).map(|t| t * 1000)).unwrap_or(0)
}

pub(crate) fn parse_message(m: &Value) -> Message {
    let bot_name = m["user_profile"]["display_name"]
        .as_str()
        .filter(|s| !s.is_empty() && m["user"].is_null())
        .or_else(|| m["username"].as_str())
        .or_else(|| m["bot_profile"]["name"].as_str())
        .map(String::from);
    Message {
        ts: str_of(&m["ts"]),
        user: m["user"].as_str().map(String::from),
        bot_name,
        bot_id: m["bot_id"].as_str().filter(|s| !s.is_empty()).map(String::from),
        bot_icon: [&m["icons"], &m["bot_profile"]["icons"]].iter().find_map(|i| icon_of(i)),
        subtype: m["subtype"].as_str().map(String::from),
        text: str_of(&m["text"]),
        blocks: m["blocks"].clone(),
        attachments: m["attachments"].clone(),
        files: m["files"].as_array().into_iter().flatten().map(parse_file).collect(),
        reply_count: m["reply_count"].as_u64().unwrap_or(0),
        thread_ts: m["thread_ts"].as_str().map(String::from),
        latest_reply: m["latest_reply"].as_str().map(String::from),
        reply_users: m["reply_users"].as_array().into_iter().flatten().filter_map(|u| u.as_str().map(String::from)).collect(),
        reactions: serde_json::from_value(m["reactions"].clone()).unwrap_or_default(),
        edited: serde_json::from_value(m["edited"].clone()).ok(),
        pinned_to: m["pinned_to"].as_array().into_iter().flatten().filter_map(|c| c.as_str().map(String::from)).collect(),
        subscribed: m["subscribed"].as_bool().unwrap_or(false),
        system: m["subtype"].is_string().then(|| {
            let s = |k: &str| m[k].as_str().map(String::from);
            System { topic: s("topic"), purpose: s("purpose"), name: s("name"), old_name: s("old_name"), inviter: s("inviter") }
        }),
    }
}

/// The 72px picture of an `icons` object, or the nearest size it has.
fn icon_of(icons: &Value) -> Option<String> {
    ["image_72", "image_64", "image_48", "image_36"].iter().find_map(|k| icons[*k].as_str().filter(|u| u.starts_with("http")).map(String::from))
}

pub(crate) fn parse_file(f: &Value) -> File {
    let is_image = f["mimetype"].as_str().unwrap_or("").starts_with("image/");
    let thumb = is_image
        .then(|| {
            ["thumb_720", "thumb_480", "thumb_360", "url_private"]
                .iter()
                .find_map(|k| f[*k].as_str().map(String::from))
        })
        .flatten();
    let (w, h) = ["thumb_720", "thumb_480", "thumb_360"]
        .iter()
        .find(|k| f[**k].is_string())
        .map(|k| (f[format!("{k}_w")].as_u64(), f[format!("{k}_h")].as_u64()))
        .unwrap_or((f["original_w"].as_u64(), f["original_h"].as_u64()));
    File {
        id: str_of(&f["id"]),
        name: f["name"].as_str().or(f["title"].as_str()).unwrap_or(tr("file", "arquivo")).to_string(),
        thumb,
        full: f["url_private"].as_str().map(String::from),
        w: w.unwrap_or(0) as u32,
        h: h.unwrap_or(0) as u32,
        permalink: f["permalink"].as_str().map(String::from),
        download: (f["is_external"].as_bool() != Some(true))
            .then(|| f["url_private_download"].as_str().or(f["url_private"].as_str()).map(String::from))
            .flatten(),
        size: f["size"].as_u64().unwrap_or(0),
        filetype: str_of(&f["filetype"]),
        pretty_type: str_of(&f["pretty_type"]),
        mimetype: str_of(&f["mimetype"]),
    }
}

fn str_of(v: &Value) -> String {
    v.as_str().unwrap_or("").to_string()
}

/// Slack wants &, < and > escaped in outgoing text. Mentions and channel
/// links the message box put in (`<@U1>`, `<#C1>`, `<!here>`) go as they
/// are, or they would arrive as plain text.
pub(crate) fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find(['&', '<', '>']) {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        if let Some(n) = slack_ref(tail) {
            out.push_str(&tail[..n]);
            rest = &tail[n..];
            continue;
        }
        out.push_str(match tail.as_bytes()[0] {
            b'&' => "&amp;",
            b'<' => "&lt;",
            _ => "&gt;",
        });
        rest = &tail[1..];
    }
    out.push_str(rest);
    out
}

/// The length of a `<@U1>`, `<#C1|general>` or `<!here>` starting `s`.
pub(crate) fn slack_ref(s: &str) -> Option<usize> {
    let body = s.strip_prefix('<')?;
    let end = body.find(['>', '<', '\n'])?;
    let inner = &body[..end];
    if !body[end..].starts_with('>') {
        return None;
    }
    let id = inner.split('|').next().unwrap_or(inner);
    let ok = match id.split_at_checked(1) {
        // Slack's ids: U…, W…, C…, G…, in capitals.
        Some(("@" | "#", rest)) => !rest.is_empty() && rest.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()),
        Some(("!", rest)) => !rest.is_empty() && rest.chars().all(|c| c.is_ascii_alphanumeric() || "^_-.".contains(c)),
        _ => false,
    };
    ok.then_some(end + 2)
}

/// `s` as a URL query value: everything but the unreserved characters escaped.
fn form_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}
