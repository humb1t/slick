//! Slack Web API: a blocking client plus the few shapes the app needs.

use crate::i18n::tr;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::sync::{Arc, RwLock};
use std::time::Duration;

pub type Res<T> = Result<T, String>;

#[derive(Clone, Default)]
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
}

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
    /// Deactivated accounts, hidden everywhere; their DMs open as read-only archives.
    #[serde(default)]
    pub deleted: HashSet<String>,
    /// id → 72px profile picture.
    #[serde(default)]
    pub avatars: HashMap<String, String>,
    /// id → (emoji shortcode, text) of a status that has not expired.
    #[serde(default)]
    pub statuses: HashMap<String, (String, String)>,
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
}

/// Messages per history page; a shorter page means the start was reached.
pub const HISTORY_LIMIT: usize = 60;

impl Slack {
    pub fn new(creds: Creds) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(20))
            .user_agent(concat!("slick/", env!("CARGO_PKG_VERSION")))
            .build();
        Self { agent, creds: Arc::new(RwLock::new(creds)) }
    }

    fn creds(&self) -> Creds {
        self.creds.read().map(|c| c.clone()).unwrap_or_default()
    }

    /// The real-time websocket's address, and the session cookie it also wants.
    pub fn rtm_url(&self) -> Res<(String, Option<String>)> {
        let v = self.call("rtm.connect", &[])?;
        let url = v["url"].as_str().filter(|u| !u.is_empty()).ok_or("rtm.connect: no url")?;
        Ok((url.to_string(), self.creds().cookie))
    }

    pub fn set_creds(&self, creds: Creds) {
        if let Ok(mut c) = self.creds.write() {
            *c = creds;
        }
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

    pub(crate) fn paginate(&self, method: &str, params: &[(&str, &str)], key: &str) -> Res<Vec<Value>> {
        let mut out = Vec::new();
        let mut cursor = String::new();
        loop {
            let v = {
                let mut p = params.to_vec();
                if !cursor.is_empty() {
                    p.push(("cursor", &cursor));
                }
                self.call(method, &p)?
            };
            if let Some(items) = v[key].as_array() {
                out.extend(items.iter().cloned());
            }
            cursor = v["response_metadata"]["next_cursor"].as_str().unwrap_or("").to_string();
            if cursor.is_empty() {
                return Ok(out);
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
        let status = [str_of(&p["status_emoji"]), str_of(&p["status_text"])]
            .into_iter()
            .filter(|s| !s.is_empty())
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

    /// Your own photo. `users.setPhoto` only takes multipart, which ureq 2
    /// lacks, so the body is put together by hand.
    pub fn set_photo(&self, path: &std::path::Path) -> Res<()> {
        let data = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
        let mime = match ext.as_str() {
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            _ => return Err(tr("the photo must be a JPG, PNG or GIF", "a foto precisa ser JPG, PNG ou GIF").into()),
        };
        let file = path.file_name().map(|n| n.to_string_lossy().replace('"', "")).unwrap_or_else(|| "photo".into());
        let boundary = format!("slick{:x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
        let creds = self.creds();
        let mut body = Vec::new();
        if creds.cookie.is_some() {
            write!(body, "--{boundary}\r\nContent-Disposition: form-data; name=\"token\"\r\n\r\n{}\r\n", creds.token).map_err(|e| e.to_string())?;
        }
        write!(body, "--{boundary}\r\nContent-Disposition: form-data; name=\"image\"; filename=\"{file}\"\r\nContent-Type: {mime}\r\n\r\n")
            .map_err(|e| e.to_string())?;
        body.extend_from_slice(&data);
        write!(body, "\r\n--{boundary}--\r\n").map_err(|e| e.to_string())?;
        let req = self.authed(self.agent.post("https://slack.com/api/users.setPhoto"));
        let resp = req
            .set("Content-Type", &format!("multipart/form-data; boundary={boundary}"))
            .send_bytes(&body)
            .map_err(|e| format!("users.setPhoto: {e}"))?;
        let v: Value = resp.into_json().map_err(|e| e.to_string())?;
        match v["ok"].as_bool() {
            Some(true) => Ok(()),
            _ => Err(format!("users.setPhoto: {}", v["error"].as_str().unwrap_or(tr("unknown error", "erro desconhecido")))),
        }
    }

    pub fn users(&self) -> Res<Users> {
        let list = self.paginate("users.list", &[("limit", "200")], "members")?;
        let mut users = Users::default();
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
        for u in &list {
            let p = &u["profile"];
            let name = [&p["display_name"], &p["real_name"], &u["name"]]
                .iter()
                .filter_map(|v| v.as_str())
                .find(|s| !s.is_empty())
                .unwrap_or("?")
                .to_string();
            let id = str_of(&u["id"]);
            if u["is_bot"].as_bool() == Some(true) || id == "USLACKBOT" {
                users.bots.insert(id.clone());
            }
            if u["deleted"].as_bool() == Some(true) {
                users.deleted.insert(id.clone());
            }
            if let Some(img) = p["image_72"].as_str() {
                users.avatars.insert(id.clone(), img.to_string());
            }
            let (emoji, text) = (str_of(&p["status_emoji"]), str_of(&p["status_text"]));
            let expires = p["status_expiration"].as_i64().unwrap_or(0);
            if (!emoji.is_empty() || !text.is_empty()) && (expires == 0 || expires > now) {
                users.statuses.insert(id.clone(), (emoji, text));
            }
            if let Some(real) = p["real_name"].as_str().filter(|r| !r.is_empty()) {
                users.real.insert(id.clone(), real.to_string());
            }
            users.handles.insert(str_of(&u["name"]), id.clone());
            users.names.insert(id, name);
        }
        Ok(users)
    }

    /// Only the conversations the user is in.
    pub fn conversations(&self) -> Res<Vec<Conversation>> {
        let list = self.paginate(
            "users.conversations",
            &[
                ("types", "public_channel,private_channel,mpim,im"),
                ("exclude_archived", "true"),
                ("limit", "200"),
            ],
            "channels",
        )?;
        Ok(list
            .iter()
            .filter(|c| !c["is_user_deleted"].as_bool().unwrap_or(false))
            .map(parse_conversation)
            .collect())
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
            })
            .collect())
    }

    /// Undocumented, like `sections`: the start-up call of Slack's web client.
    pub fn boot(&self) -> Res<Boot> {
        let v = self.call("client.userBoot", &[])?;
        Ok(Boot {
            open: v["is_open"].as_array().into_iter().flatten().filter_map(|i| i.as_str().map(String::from)).collect(),
            sidebar_avatars: v["prefs"]["show_sidebar_avatars"].as_bool().unwrap_or(true),
        })
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
        let items = self.paginate("stars.list", &[("limit", "200")], "items")?;
        Ok(items
            .iter()
            .filter(|i| matches!(i["type"].as_str(), Some("im" | "mpim" | "channel" | "group")))
            .filter_map(|i| i["channel"].as_str().map(String::from))
            .collect())
    }

    /// Unread state and last activity for every conversation, in one call.
    /// Undocumented, like `sections`.
    pub fn counts(&self) -> Res<HashMap<String, Count>> {
        let v = self.call("client.counts", &[])?;
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
        Ok(out)
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
        let v = self.call("chat.postMessage", &[("channel", chan), ("text", &escape(text))])?;
        let mut m = parse_message(&v["message"]);
        if m.ts.is_empty() {
            m.ts = str_of(&v["ts"]);
        }
        Ok(m)
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

    /// Moves the read cursor so the official clients stop flagging it unread.
    pub fn mark(&self, chan: &str, ts: &str) -> Res<()> {
        self.call("conversations.mark", &[("channel", chan), ("ts", ts)]).map(|_| ())
    }

    /// Files on files.slack.com need the same credentials as the API; anything
    /// else (avatars on gravatar, slack-edge) must never see them.
    pub fn download(&self, url: &str) -> Res<Vec<u8>> {
        let req = self.agent.get(url);
        let req = if is_slack(url) { self.authed(req) } else { req };
        let resp = req.call().map_err(|e| e.to_string())?;
        let mut buf = Vec::new();
        resp.into_reader()
            .take(20 << 20)
            .read_to_end(&mut buf)
            .map_err(|e| e.to_string())?;
        Ok(buf)
    }

    /// Streams a file into `out`, for saving it: no size cap and only connect
    /// and read timeouts, so files of hundreds of MB get through.
    pub fn download_to(&self, url: &str, out: &mut dyn Write, html_ok: bool) -> Res<u64> {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(20))
            .timeout_read(Duration::from_secs(60))
            .user_agent(concat!("slick/", env!("CARGO_PKG_VERSION")))
            .build();
        let req = agent.get(url);
        let req = if is_slack(url) { self.authed(req) } else { req };
        let resp = req.call().map_err(|e| e.to_string())?;
        // A session Slack no longer accepts gets its sign-in page, not the file.
        if !html_ok && resp.content_type() == "text/html" {
            return Err(tr("Slack didn't hand over the file (session expired?)", "o Slack não entregou o arquivo (sessão expirada?)").into());
        }
        std::io::copy(&mut resp.into_reader(), out).map_err(|e| e.to_string())
    }
}

/// Only slack.com hosts get the credentials.
fn is_slack(url: &str) -> bool {
    let host = url.split("://").nth(1).and_then(|r| r.split('/').next()).unwrap_or("");
    host == "slack.com" || host.ends_with(".slack.com")
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
    }
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
    }
}

fn parse_file(f: &Value) -> File {
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
