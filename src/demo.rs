//! A pretend workspace for trying slick without signing in and for the
//! screenshots (`SLICK_DEMO=1`, only in builds with `--features demo`).
//! The Web API calls are answered from here, writes are kept in memory, the
//! pictures are embedded (`assets/demo`), and nothing reaches Slack. Everyone
//! and everything in it is made up; the data follows omaslack's demo.
//!
//! `SLICK_DEMO_SCRIPT` drives the app once it's up, for the screenshots:
//! whitespace-separated steps, run in order:
//!
//! - `sleep:800` waits that many ms
//! - `ctrl+k`, `alt+down`, `shift+enter`, `enter`, `esc`, `tab`, `up`, `j`… a key
//! - `type:text` types it (`_` is a space)
//! - `click:col,row`, `rclick:col,row`, `move:col,row`, `wheel-up:col,row`, `wheel-down:col,row`
//! - `ready` writes `$SLICK_DEMO_DIR/ready`, for whoever takes the screenshot
//! - `live:message` / `live:reaction` play something happening in the workspace

use crate::app::Event;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::Sender;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const ME: &str = "U0ME";
/// Where the pictures are "downloaded" from; `download` answers them.
const HOST: &str = "https://demo.slick.invalid/";

pub fn on() -> bool {
    std::env::var("SLICK_DEMO").is_ok_and(|v| v == "1")
}

/// `SLICK_DEMO_GRID=1`: the workspaces behave like an Enterprise Grid org
/// that restricts `users.conversations` and `stars.list`.
fn grid() -> bool {
    std::env::var("SLICK_DEMO_GRID").is_ok_and(|v| v == "1")
}

/// Its own config, cache and data folders, fresh at every start, so the
/// real ones are never read or written: `$SLICK_DEMO_DIR`, else a folder in /tmp.
pub fn setup() {
    let dir = dir();
    let _ = std::fs::remove_dir_all(&dir);
    for sub in ["config", "cache", "data"] {
        let _ = std::fs::create_dir_all(dir.join(sub));
    }
    // Before any thread starts: nothing reads the environment concurrently yet.
    std::env::set_var("XDG_CONFIG_HOME", dir.join("config"));
    std::env::set_var("XDG_CACHE_HOME", dir.join("cache"));
    std::env::set_var("XDG_DATA_HOME", dir.join("data"));
    std::env::remove_var("SLACK_TOKEN");
    let _ = std::fs::create_dir_all(dir.join("config/slick"));
    let _ = std::fs::write(dir.join("config/slick/config"), "tray = false\nnotifications = false\nauto_update = false\n");
}

fn dir() -> std::path::PathBuf {
    std::env::var_os("SLICK_DEMO_DIR").map(Into::into).unwrap_or_else(|| std::env::temp_dir().join("slick-demo"))
}

/// The pretend workspaces, as the config would list them.
pub fn workspaces() -> Vec<crate::config::WsConf> {
    [("T0DEMO", "acme"), ("T0SIDE", "sideprojects")]
        .into_iter()
        .map(|(id, domain)| crate::config::WsConf {
            team_id: Some(id.into()),
            domain: Some(domain.into()),
            creds: crate::slack::Creds { token: format!("xoxp-demo-{id}"), cookie: None },
        })
        .collect()
}

// ------------------------------------------------------------------ pictures

macro_rules! pics {
    ($($name:literal),* $(,)?) => {
        &[$(($name, include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/demo/", $name)) as &[u8])),*]
    };
}

static PICS: &[(&str, &[u8])] = pics![
    "face-alex.png", "face-priya.png", "face-diego.png", "face-sam.png", "face-lena.png", "face-tomas.png",
    "face-hana.png", "face-noah.png", "face-kai.png", "face-rosa.png", "face-ivy.png",
    "mark-acme.png", "mark-side.png", "mark-shipyard.png", "mark-watchtower.png", "mark-slackbot.png",
    "dashboard.png", "sunset.png", "emoji-lgtm.png", "emoji-party-blob.gif",
];

/// A picture by its address; the demo has no others.
pub fn download(url: &str) -> Result<Vec<u8>, String> {
    let name = url.strip_prefix(HOST).unwrap_or(url);
    PICS.iter().find(|(n, _)| *n == name).map(|(_, b)| b.to_vec()).ok_or_else(|| format!("demo: no {name}"))
}

fn pic(name: &str) -> String {
    format!("{HOST}{name}")
}

// ------------------------------------------------------------------ the data

fn now() -> i64 {
    chrono::Local::now().timestamp()
}

struct Clock(i64);

impl Clock {
    /// A message's ts, `ago` seconds back.
    fn ts(&self, ago: i64) -> String {
        format!("{}.000100", self.0 - ago)
    }
}

fn person(id: &str, handle: &str, name: &str, title: &str, extra: Value) -> Value {
    let face = pic(&format!("face-{handle}.png"));
    let mut u = json!({
        "id": id, "name": handle, "tz": "Europe/Lisbon", "tz_label": "Western European Time", "tz_offset": 0,
        "profile": { "display_name": name, "real_name": name, "title": title,
                     "image_72": face, "image_192": face, "image_512": face,
                     "email": format!("{handle}@acme.example") }
    });
    if let Some(p) = extra.get("profile").and_then(Value::as_object) {
        for (k, v) in p {
            u["profile"][k] = v.clone();
        }
    }
    u
}

fn app_user(id: &str, name: &str, mark: &str) -> Value {
    let icon = pic(&format!("mark-{mark}.png"));
    json!({ "id": id, "name": name.to_lowercase(), "is_bot": true,
            "profile": { "display_name": name, "real_name": name, "image_72": icon, "image_192": icon, "image_512": icon,
                         "bot_id": format!("B{}", &id[1..]) } })
}

fn slackbot() -> Value {
    let icon = pic("mark-slackbot.png");
    json!({ "id": "USLACKBOT", "name": "slackbot", "is_bot": true,
            "profile": { "display_name": "Slackbot", "real_name": "Slackbot", "image_72": icon, "image_192": icon } })
}

fn image(id: &str, name: &str, user: &str, file: &str) -> Value {
    let url = pic(file);
    json!({ "id": id, "name": name, "title": name, "mimetype": "image/png", "filetype": "png", "pretty_type": "PNG",
            "size": 482113, "user": user, "original_w": 960, "original_h": 600,
            "thumb_720": url, "thumb_720_w": 960, "thumb_720_h": 600, "thumb_360": url, "thumb_360_w": 480, "thumb_360_h": 300,
            "thumb_480": url, "thumb_480_w": 960, "thumb_480_h": 600,
            "url_private": url, "url_private_download": url,
            "permalink": format!("https://acme.slack.com/files/{user}/{id}/{name}") })
}

fn rt(parts: Value) -> Value {
    json!([{ "type": "rich_text", "elements": parts }])
}

fn sec(els: Value) -> Value {
    json!({ "type": "rich_text_section", "elements": els })
}

fn tx(t: &str) -> Value {
    json!({ "type": "text", "text": t })
}

fn styled(t: &str, style: Value) -> Value {
    json!({ "type": "text", "text": t, "style": style })
}

/// One workspace: everything its calls answer from, kept as JSON.
struct Space {
    id: &'static str,
    name: &'static str,
    domain: &'static str,
    icon: &'static str,
    users: Vec<Value>,
    convs: Vec<Value>,
    open: Vec<&'static str>,
    sections: Value,
    history: HashMap<String, Vec<Value>>,
    replies: HashMap<String, Vec<Value>>,
    /// conversation → (unread, mentions, last read)
    counts: HashMap<String, (bool, u64, String)>,
    custom: Value,
    stars: Vec<&'static str>,
    saved: Vec<(String, String)>,
    pins: HashMap<&'static str, Vec<usize>>,
    directory: Vec<Value>,
    snoozed: Vec<&'static str>,
    active: Vec<&'static str>,
    threads: Vec<(String, String)>,
    vips: Vec<&'static str>,
    /// What happens live (`live:` steps): reactions to a message of yours, and a DM.
    react: Option<(String, String, Vec<(&'static str, &'static str)>)>,
    message: Option<(&'static str, &'static str, &'static str)>,
    commands: Value,
}

fn acme(c: &Clock) -> Space {
    let t = |ago| c.ts(ago);
    let users = vec![
        person(ME, "alex", "Alex Morgan", "Staff Engineer", json!({ "profile": { "status_emoji": ":house_with_garden:", "status_text": "Working remotely" } })),
        person("U0PRIYA", "priya", "Priya Raman", "Product Designer", json!({ "profile": { "status_emoji": ":palm_tree:", "status_text": "Off Friday" } })),
        person("U0DIEGO", "diego", "Diego Alvarez", "Backend Engineer", json!({})),
        person("U0SAM", "sam", "Sam Okafor", "Engineering Manager", json!({ "profile": { "status_emoji": ":date:", "status_text": "In a meeting" } })),
        person("U0LENA", "lena", "Lena Novak", "Data Scientist", json!({})),
        person("U0TOMAS", "tomas", "Tomás Ribeiro", "Site Reliability Engineer", json!({ "profile": { "status_emoji": ":pager:", "status_text": "On call" } })),
        person("U0HANA", "hana", "Hana Sato", "QA Engineer", json!({})),
        person("U0NOAH", "noah", "Noah Fischer", "Frontend Engineer", json!({})),
        app_user("U0SHIP", "Shipyard", "shipyard"),
        app_user("U0WATCH", "Watchtower", "watchtower"),
        slackbot(),
    ];
    let ch = |id: &str, name: &str, members: u64, topic: &str| {
        json!({ "id": id, "name": name, "is_channel": true, "is_member": true, "num_members": members, "topic": { "value": topic }, "created": c.0 - 86400 * 300, "creator": "U0SAM" })
    };
    let mut general = ch("C0GENERAL", "general", 48, "Company-wide announcements and work-based matters");
    general["is_general"] = json!(true);
    general["purpose"] = json!({ "value": "This channel is for workspace-wide communication and announcements." });
    let mut eng = ch("C0ENG", "engineering", 18, "Deploys on Tuesdays and Thursdays · <https://status.acme.example|status page>");
    eng["purpose"] = json!({ "value": "Where the platform team builds, reviews and ships." });
    let mut leads = ch("G0LEADS", "leadership", 5, "");
    leads["is_private"] = json!(true);
    leads["is_group"] = json!(true);
    let mut old = ch("C0OLD", "old-website", 7, "");
    old["is_archived"] = json!(true);
    let im = |id: &str, user: &str| json!({ "id": id, "is_im": true, "user": user });
    let convs = vec![
        general,
        ch("C0RANDOM", "random", 45, "Non-work banter and water cooler conversation"),
        eng,
        ch("C0DESIGN", "design", 9, "Critiques on Wednesdays"),
        ch("C0LAUNCH", "launch-q4", 14, "Countdown to the Q4 launch :rocket:"),
        ch("C0RELEASES", "releases", 30, "What shipped, and when"),
        leads,
        ch("C0ALERTS", "alerts-prod", 12, "Production alerts from Watchtower"),
        ch("C0INCIDENTS", "incidents", 21, "One thread per incident"),
        old,
        im("D0PRIYA", "U0PRIYA"),
        im("D0DIEGO", "U0DIEGO"),
        im("D0SAM", "U0SAM"),
        im("D0LENA", "U0LENA"),
        im("D0TOMAS", "U0TOMAS"),
        im("D0ME", ME),
        im("D0SHIP", "U0SHIP"),
        json!({ "id": "G0MPIM", "name": "mpdm-priya--diego--alex-1", "is_mpim": true, "is_private": true }),
    ];
    let sections = json!([
        { "channel_section_id": "L0STARS", "type": "stars", "name": "", "channel_ids_page": { "channel_ids": ["C0ENG", "D0ME"] } },
        { "channel_section_id": "L0PROJ", "type": "standard", "name": "Projects", "emoji": "rocket", "channel_ids_page": { "channel_ids": ["C0LAUNCH", "C0RELEASES"] } },
        { "channel_section_id": "L0OPS", "type": "standard", "name": "On call", "emoji": "rotating_light", "channel_ids_page": { "channel_ids": ["C0ALERTS", "C0INCIDENTS"] } },
        { "channel_section_id": "L0CHAN", "type": "channels", "name": "", "channel_ids_page": { "channel_ids": [] } },
        { "channel_section_id": "L0DMS", "type": "direct_messages", "name": "", "channel_ids_page": { "channel_ids": [] } },
        { "channel_section_id": "L0APPS", "type": "recent_apps", "name": "", "channel_ids_page": { "channel_ids": [] } }
    ]);

    let mut history: HashMap<String, Vec<Value>> = HashMap::new();
    let mut h = |id: &str, msgs: Vec<Value>| {
        history.insert(id.into(), msgs);
    };
    let brand = t(86400 + 500);
    h("C0GENERAL", vec![
        json!({ "user": "U0SAM", "ts": t(86400 * 2 + 3000), "text": "Morning everyone :wave: Reminder: the all-hands is tomorrow at 3pm in the atrium. Snacks are on us." }),
        json!({ "user": "U0LENA", "subtype": "channel_join", "ts": t(86400 * 2 + 2000), "text": "<@U0LENA> has joined the channel" }),
        json!({ "user": "U0HANA", "subtype": "channel_join", "ts": t(86400 * 2 + 1900), "text": "<@U0HANA> has joined the channel" }),
        json!({ "user": "U0PRIYA", "ts": brand, "text": "The new brand guidelines are live! Have a look and leave your comments :art:",
                "reactions": [{ "name": "heart", "users": ["U0DIEGO", "U0SAM", ME], "count": 3 }, { "name": "tada", "users": ["U0LENA"], "count": 1 }],
                "reply_count": 4, "reply_users": ["U0DIEGO", "U0SAM", ME], "reply_users_count": 3, "latest_reply": t(3600 * 5), "thread_ts": brand }),
        json!({ "user": "U0DIEGO", "ts": t(7200), "text": "Is the office open on Friday?" }),
        json!({ "user": "U0SAM", "ts": t(7100), "text": "Nope :sweat_smile: enjoy the long weekend" }),
        json!({ "user": "U0SAM", "ts": t(7050), "text": "and remember to set your *status* before you go", "edited": { "user": "U0SAM", "ts": t(7000) } }),
        json!({ "user": "U0PRIYA", "ts": t(1800), "text": "<!here> fresh coffee beans just landed in the kitchen :coffee:",
                "reactions": [{ "name": "coffee", "users": ["U0DIEGO", "U0TOMAS", "U0HANA", "U0LENA", "U0NOAH", "U0SAM", ME], "count": 7 }, { "name": "+1::skin-tone-3", "users": [ME], "count": 1 }] }),
    ]);
    let review = t(9000);
    h("C0ENG", vec![
        json!({ "user": "U0TOMAS", "ts": t(86400 + 4000), "text": "Yesterday's deploy of the API went out without a hitch :rocket:" }),
        json!({ "user": "U0PRIYA", "ts": t(9600), "text": "Here's the new latency dashboard for on-call. Thoughts? :eyes:",
                "files": [image("F0DASH", "latency-dashboard.png", "U0PRIYA", "dashboard.png")],
                "reactions": [{ "name": "heart_eyes", "users": ["U0NOAH", "U0SAM", "U0TOMAS", ME], "count": 4 }, { "name": "fire", "users": ["U0DIEGO", "U0LENA"], "count": 2 }] }),
        json!({ "user": "U0DIEGO", "ts": review, "text": "Can someone review <https://git.acme.example/platform/api/pull/482|platform/api#482>? It fixes the session cache TTL :pray:",
                "reply_count": 4, "reply_users": ["U0LENA", "U0TOMAS", ME], "reply_users_count": 3, "latest_reply": t(5200), "thread_ts": review, "subscribed": true }),
        json!({ "user": "U0LENA", "ts": t(8400), "text": "Found it. The ttl was in seconds, not milliseconds",
                "blocks": rt(json!([
                    sec(json!([tx("Found it. The "), styled("ttl", json!({ "code": true })), tx(" was in "), styled("seconds", json!({ "bold": true })), tx(", not milliseconds:")])),
                    { "type": "rich_text_preformatted", "elements": [tx("const ttl = config.session.ttl * 1000\ncache.set(key, value, { ttl })")] },
                    sec(json!([tx("I'll write it up in "), { "type": "channel", "channel_id": "C0RELEASES" }, tx(" once it ships.")]))
                ])) }),
        json!({ "user": ME, "ts": t(8100), "text": "Nice catch! Approving now :white_check_mark:",
                "reactions": [{ "name": "raised_hands", "users": ["U0DIEGO", "U0LENA"], "count": 2 }, { "name": "party-blob", "users": ["U0TOMAS", "U0NOAH", "U0HANA"], "count": 3 }] }),
        json!({ "user": "U0SHIP", "bot_id": "B0SHIP", "ts": t(5000), "text": "",
                "attachments": [{ "color": "#2eb886", "pretext": "Deployed to production", "title": "api v2.14.0 · Fix session cache TTL", "title_link": "https://ci.acme.example/deploys/2140",
                                  "text": "Session entries now expire after the configured TTL.",
                                  "fields": [{ "title": "Duration", "value": "3m 12s", "short": true }, { "title": "Checks", "value": "128 passed", "short": true }],
                                  "footer": "Shipyard", "ts": c.0 - 5000 }] }),
        json!({ "user": "U0NOAH", "ts": t(2400), "text": "p99 on the gateway is creeping up again :chart_with_upwards_trend: anyone else seeing it?" }),
        json!({ "user": "U0SAM", "ts": t(600), "text": "<@U0ME> can you look at the latency alert in <#C0ALERTS>? Probably related to this morning's deploy.",
                "reactions": [{ "name": "eyes", "users": [ME], "count": 1 }] }),
    ]);
    h("C0RANDOM", vec![
        json!({ "user": "U0LENA", "ts": t(86400 + 9000), "text": "Best lunch spot around here? :thinking:" }),
        json!({ "user": "U0HANA", "ts": t(86400 + 8000), "text": "the ramen place on the corner is :100:", "reactions": [{ "name": "100", "users": ["U0LENA", "U0PRIYA"], "count": 2 }] }),
        json!({ "user": "U0PRIYA", "ts": t(3300), "text": ":tada::tada::tada:" }),
        json!({ "user": "U0DIEGO", "ts": t(2900), "text": "Sunset from yesterday's trail run :sunrise_over_mountains:",
                "files": [image("F0SUNSET", "trail-sunset.png", "U0DIEGO", "sunset.png")],
                "reactions": [{ "name": "star_struck", "users": ["U0PRIYA", "U0HANA", ME], "count": 3 }, { "name": "party-blob", "users": ["U0NOAH"], "count": 1 }] }),
    ]);
    h("C0DESIGN", vec![
        json!({ "user": "U0PRIYA", "ts": t(86400 * 5), "text": "Moodboard for the new landing page:",
                "files": [{ "id": "F0MOOD", "name": "landing-moodboard.pdf", "title": "landing-moodboard.pdf", "mimetype": "application/pdf", "filetype": "pdf", "pretty_type": "PDF", "size": 2400000,
                            "url_private": pic("landing-moodboard.pdf"), "permalink": "https://acme.slack.com/files/U0PRIYA/F0MOOD" }] }),
        json!({ "user": "U0NOAH", "ts": t(86400 * 5 - 600), "text": "Love the type pairing. Can we try the darker variant for the hero?" }),
    ]);
    h("C0LAUNCH", vec![
        json!({ "user": "U0SAM", "ts": t(86400 * 2), "text": "The launch checklist is pinned. Owners, please update your items by Friday :pray:" }),
        json!({ "user": "U0NOAH", "ts": t(4000), "text": "12 days to go :rocket: The new onboarding flow is behind the `onboarding_v2` flag in staging." }),
        json!({ "user": "U0SAM", "ts": t(3000), "text": "Freeze starts next Monday: last deploy window is Thursday afternoon." }),
    ]);
    h("C0RELEASES", vec![json!({ "user": "U0TOMAS", "ts": t(86400 * 6), "text": "Release 4.12.0", "blocks": [
        { "type": "header", "text": { "type": "plain_text", "text": "Release 4.12.0" } },
        { "type": "section", "text": { "type": "mrkdwn", "text": "*Highlights*\n• Faster search\n• Dark mode for reports\n• Bug fixes" } },
        { "type": "divider" },
        { "type": "context", "elements": [{ "type": "mrkdwn", "text": "Shipped by <@U0TOMAS> · <https://acme.example/changelog|changelog>" }] },
        { "type": "actions", "elements": [
            { "type": "button", "text": { "type": "plain_text", "text": "View changelog" }, "url": "https://acme.example/changelog", "style": "primary" },
            { "type": "button", "text": { "type": "plain_text", "text": "Report a bug" }, "url": "https://acme.example/bugs" }] }
    ] })]);
    h("G0LEADS", vec![
        json!({ "user": "U0SAM", "ts": t(86400 + 200), "text": "Agenda for Thursday:\n1. Q4 planning\n2. Hiring\n3. Offsite" }),
        json!({ "user": ME, "ts": t(86400 + 100), "text": "> 2. Hiring\nI have two great backend candidates lined up." }),
    ]);
    h("C0ALERTS", vec![
        json!({ "user": "U0WATCH", "bot_id": "B0WATCH", "ts": t(700), "text": "", "attachments": [{ "color": "danger", "title": "[FIRING:1] HighLatency api-gateway", "text": "p99 latency above 800ms for 10 minutes", "footer": "Watchtower" }] }),
        json!({ "user": "U0WATCH", "bot_id": "B0WATCH", "ts": t(300), "text": "", "attachments": [{ "color": "good", "title": "[RESOLVED] HighLatency api-gateway", "text": "p99 latency back to normal", "footer": "Watchtower" }] }),
    ]);
    h("C0INCIDENTS", vec![
        json!({ "user": "U0TOMAS", "ts": t(86400 * 9), "text": ":white_check_mark: INC-212 resolved: queue backlog after the Redis failover. Postmortem in the thread." }),
        json!({ "user": "U0DIEGO", "ts": t(86400 * 3), "text": "Rolling back the worker deploy, the new pool is leaking connections :rotating_light:" }),
        json!({ "user": "U0HANA", "ts": t(86400 * 3 - 1800), "text": "Smoke tests are green on the rollback. Holding deploys until the fix lands." }),
    ]);
    h("D0PRIYA", vec![
        json!({ "user": "U0PRIYA", "ts": t(86400 + 300), "text": "Hey! Did you get a chance to look at the prototype? :eyes:" }),
        json!({ "user": ME, "ts": t(86400 + 200), "text": "I did, it looks great! I'd only bump the contrast on the buttons" }),
        json!({ "user": "U0PRIYA", "ts": t(900), "text": "Done! Have a look when you can :pray:" }),
        json!({ "user": "U0PRIYA", "ts": t(880), "text": "and ping me if you need anything before my day off :palm_tree:" }),
    ]);
    h("D0DIEGO", vec![
        json!({ "user": "U0DIEGO", "ts": t(86400 * 2), "text": "thanks for the review yesterday!" }),
        json!({ "user": ME, "ts": t(86400 * 2 - 100), "text": "Anytime :muscle:" }),
    ]);
    h("D0SAM", vec![json!({ "user": "U0SAM", "ts": t(400), "text": "Got 5 minutes for a quick call about the incident?" })]);
    h("D0LENA", vec![
        json!({ "user": "U0LENA", "ts": t(86400 * 3), "text": "The churn model is retrained, numbers look much better :chart_with_upwards_trend:" }),
        json!({ "user": ME, "ts": t(86400 * 3 - 300), "text": "Amazing, let's demo it on Thursday" }),
    ]);
    h("D0TOMAS", vec![json!({ "user": "U0TOMAS", "ts": t(86400 * 4), "text": "Swapping on-call with you next week works for me :+1:" })]);
    h("D0ME", vec![json!({ "user": ME, "ts": t(86400 * 8), "text": "Reminder: renew the TLS certificate by Friday" })]);
    h("D0SHIP", vec![json!({ "user": "U0SHIP", "bot_id": "B0SHIP", "ts": t(86400 * 4), "text": "You have 2 review requests waiting." })]);
    h("G0MPIM", vec![
        json!({ "user": "U0PRIYA", "ts": t(86400 * 3), "text": "Team dinner on Friday?" }),
        json!({ "user": "U0DIEGO", "ts": t(86400 * 3 - 60), "text": "I'm in :ramen:" }),
    ]);

    let mut replies = HashMap::new();
    replies.insert(format!("C0GENERAL/{brand}"), vec![
        json!({ "user": "U0DIEGO", "ts": t(86000), "thread_ts": brand, "text": "Looks beautiful! :star_struck:" }),
        json!({ "user": "U0SAM", "ts": t(80000), "thread_ts": brand, "text": "Great work, Priya. Let's show it at the all-hands." }),
        json!({ "user": ME, "ts": t(20000), "thread_ts": brand, "text": "Love it! Is there a PDF version too?" }),
        json!({ "user": "U0PRIYA", "ts": t(3600 * 5), "thread_ts": brand, "text": "<@U0ME> just uploaded it to the shared folder :+1:" }),
    ]);
    replies.insert(format!("C0ENG/{review}"), vec![
        json!({ "user": "U0LENA", "ts": t(8800), "thread_ts": review, "text": "Looking now :eyes:" }),
        json!({ "user": "U0TOMAS", "ts": t(7400), "thread_ts": review, "text": "Left a couple of comments on the retry logic, otherwise LGTM :party-blob:" }),
        json!({ "user": ME, "ts": t(6200), "thread_ts": review, "text": "Approved :white_check_mark: ship it :rocket:", "reactions": [{ "name": "rocket", "users": ["U0DIEGO"], "count": 1 }] }),
        json!({ "user": "U0DIEGO", "ts": t(5200), "thread_ts": review, "text": "Merged and deployed, thanks all! :tada:" }),
    ]);

    let mut counts = HashMap::new();
    for (id, unread, mentions, read) in [
        ("C0GENERAL", true, 0, t(7000)),
        ("C0ENG", true, 1, t(5100)),
        ("C0RANDOM", true, 0, t(86400 + 8000)),
        ("C0LAUNCH", true, 0, t(86400)),
        ("C0ALERTS", true, 0, t(800)),
        ("D0PRIYA", true, 2, t(86400 + 200)),
        ("D0SAM", true, 1, t(500)),
    ] {
        counts.insert(id.to_string(), (unread, mentions, read));
    }
    Space {
        id: "T0DEMO",
        name: "Acme",
        domain: "acme",
        icon: "mark-acme.png",
        users,
        convs,
        open: vec!["D0PRIYA", "D0DIEGO", "D0SAM", "D0LENA", "D0TOMAS", "D0ME", "D0SHIP", "G0MPIM"],
        sections,
        history,
        replies,
        counts,
        custom: json!({ "party-blob": pic("emoji-party-blob.gif"), "lgtm": pic("emoji-lgtm.png"), "shipit": "alias:chipmunk" }),
        stars: vec!["C0ENG", "D0ME"],
        saved: vec![("C0ENG".into(), t(9600))],
        pins: HashMap::from([("C0GENERAL", vec![0]), ("C0LAUNCH", vec![0]), ("C0ENG", vec![1])]),
        directory: vec![
            json!({ "id": "C0BOOK", "name": "book-club", "num_members": 7, "purpose": { "value": "One book a month, no spoilers" } }),
            json!({ "id": "C0MUSIC", "name": "music", "num_members": 15, "purpose": { "value": "What's on your headphones" } }),
        ],
        snoozed: vec!["U0PRIYA"],
        active: vec!["U0PRIYA", "U0SAM", "U0DIEGO", "U0NOAH", "U0TOMAS", ME],
        threads: vec![("C0ENG".into(), review), ("C0GENERAL".into(), brand)],
        vips: vec!["U0SAM"],
        react: Some(("C0ENG".into(), t(8100), vec![("U0PRIYA", "party-blob"), ("U0SAM", "tada"), ("U0DIEGO", "heart"), ("U0NOAH", "rocket"), ("U0HANA", "fire")])),
        message: Some(("D0PRIYA", "U0PRIYA", "Could you take a look at the final mockups in <#C0DESIGN> before standup? :sparkles:")),
        commands: json!([
            { "name": "/remind", "type": "core", "desc": "Set a reminder", "usage": "[@someone or #channel] [what] [when]" },
            { "name": "/poll", "type": "app", "desc": "Start a quick poll", "usage": "\"question\" \"option\" \"option\"", "app_name": "Polls" },
            { "name": "/deploy", "type": "app", "desc": "Deploy a service with Shipyard", "usage": "[service] [env]", "app_name": "Shipyard" }
        ]),
    }
}

fn side(c: &Clock) -> Space {
    let t = |ago| c.ts(ago);
    let users = vec![
        person(ME, "alex", "Alex Morgan", "Maker", json!({})),
        person("U0KAI", "kai", "Kai Jensen", "Hardware tinkerer", json!({})),
        person("U0ROSA", "rosa", "Rosa Delgado", "Game developer", json!({})),
        person("U0IVY", "ivy", "Ivy Chen", "Illustrator", json!({})),
        slackbot(),
    ];
    let ch = |id: &str, name: &str, members: u64| json!({ "id": id, "name": name, "is_channel": true, "is_member": true, "num_members": members });
    let mut general = ch("C1GENERAL", "general", 23);
    general["is_general"] = json!(true);
    let convs = vec![
        general,
        ch("C1IDEAS", "ideas", 19),
        ch("C1KEYBOARDS", "keyboards", 11),
        json!({ "id": "D1KAI", "is_im": true, "user": "U0KAI" }),
        json!({ "id": "D1ROSA", "is_im": true, "user": "U0ROSA" }),
    ];
    let history = HashMap::from([
        ("C1GENERAL".to_string(), vec![json!({ "user": "U0ROSA", "ts": t(86400 * 2), "text": "Game jam this weekend, who's in? :video_game:" })]),
        ("C1IDEAS".to_string(), vec![json!({ "user": "U0IVY", "ts": t(5000), "text": "What if the plant watering bot sent a photo every morning? :seedling:" })]),
        ("C1KEYBOARDS".to_string(), vec![json!({ "user": "U0KAI", "ts": t(86400 * 4), "text": "Lubed the switches, it sounds like rain now :keyboard:" })]),
        ("D1KAI".to_string(), vec![json!({ "user": "U0KAI", "ts": t(2400), "text": "<@U0ME> the new PCB arrived! Soldering tonight?" })]),
        ("D1ROSA".to_string(), vec![
            json!({ "user": "U0ROSA", "ts": t(86400 * 3), "text": "Thanks for the shader tips!" }),
            json!({ "user": ME, "ts": t(86400 * 3 - 100), "text": "Anytime :sparkles:" }),
        ]),
    ]);
    Space {
        id: "T0SIDE",
        name: "Side Projects",
        domain: "sideprojects",
        icon: "mark-side.png",
        users,
        convs,
        open: vec!["D1KAI", "D1ROSA"],
        sections: json!([
            { "channel_section_id": "L1CHAN", "type": "channels", "name": "", "channel_ids_page": { "channel_ids": [] } },
            { "channel_section_id": "L1DMS", "type": "direct_messages", "name": "", "channel_ids_page": { "channel_ids": [] } }
        ]),
        history,
        replies: HashMap::new(),
        counts: HashMap::from([("C1IDEAS".to_string(), (true, 0, t(6000))), ("D1KAI".to_string(), (true, 1, t(3000)))]),
        custom: json!({}),
        stars: vec![],
        saved: vec![],
        pins: HashMap::new(),
        directory: vec![],
        snoozed: vec![],
        active: vec!["U0KAI", ME],
        threads: vec![],
        vips: vec![],
        react: None,
        message: None,
        commands: json!([]),
    }
}

static SPACES: OnceLock<Mutex<HashMap<String, Space>>> = OnceLock::new();

fn with<T>(token: &str, f: impl FnOnce(&mut Space) -> T) -> T {
    let spaces = SPACES.get_or_init(|| {
        let c = Clock(now());
        Mutex::new(HashMap::from([("T0DEMO".to_string(), acme(&c)), ("T0SIDE".to_string(), side(&c))]))
    });
    let mut g = spaces.lock().unwrap_or_else(|e| e.into_inner());
    let team = if token.ends_with("T0SIDE") { "T0SIDE" } else { "T0DEMO" };
    f(g.get_mut(team).expect("demo space"))
}

// ------------------------------------------------------------------ answers

fn latest(d: &Space, id: &str) -> String {
    d.history.get(id).and_then(|h| h.last()).and_then(|m| m["ts"].as_str()).unwrap_or("").to_string()
}

fn ts_f(v: &str) -> f64 {
    v.parse().unwrap_or(0.0)
}

/// `text` with every word of the query between Slack's highlight marks.
fn highlight(text: &str, words: &[String]) -> String {
    let lower = text.to_ascii_lowercase();
    let mut lit = vec![false; text.len()];
    for w in words {
        let mut from = 0;
        while let Some(i) = lower[from..].find(w.as_str()) {
            let at = from + i;
            lit[at..at + w.len()].iter_mut().for_each(|b| *b = true);
            from = at + w.len();
        }
    }
    let mut out = String::new();
    let mut on = false;
    for (i, ch) in text.char_indices() {
        if lit[i] != on {
            out.push(if lit[i] { '\u{E000}' } else { '\u{E001}' });
            on = lit[i];
        }
        out.push(ch);
    }
    if on {
        out.push('\u{E001}');
    }
    out
}

/// The answer to a Web API call, as Slack would give it.
pub fn call(token: &str, method: &str, params: &[(&str, &str)]) -> Result<Value, String> {
    let v = with(token, |d| answer(d, method, params));
    if v["ok"].as_bool() == Some(true) {
        Ok(v)
    } else {
        Err(format!("{method}: {}", v["error"].as_str().unwrap_or("demo")))
    }
}

/// A parameter of the call, "" when not given.
fn arg<'a>(params: &[(&str, &'a str)], k: &str) -> &'a str {
    params.iter().find(|(n, _)| *n == k).map(|(_, v)| *v).unwrap_or("")
}

fn answer(d: &mut Space, method: &str, params: &[(&str, &str)]) -> Value {
    let p = |k: &str| arg(params, k);
    let stamp = || format!("{}.{:06}", now(), rand_us());
    match method {
        "auth.test" => json!({ "ok": true, "url": format!("https://{}.slack.com/", d.domain), "team": d.name, "user": "alex", "team_id": d.id, "user_id": ME }),
        "team.info" => json!({ "ok": true, "team": { "id": d.id, "name": d.name, "domain": d.domain, "icon": { "image_132": pic(d.icon), "image_88": pic(d.icon) } } }),
        "users.list" => json!({ "ok": true, "members": d.users }),
        "users.info" => {
            let u = d.users.iter().find(|u| u["id"] == p("user")).unwrap_or(&d.users[0]);
            json!({ "ok": true, "user": u })
        }
        // `SLICK_DEMO_GRID=1`: an Enterprise Grid org, whose org-level session is refused some workspace-level methods.
        "users.conversations" if grid() => json!({ "ok": false, "error": "enterprise_is_restricted" }),
        "users.conversations" => json!({ "ok": true, "channels": d.convs }),
        "users.channelSections.list" => json!({ "ok": true, "channel_sections": d.sections }),
        "stars.list" if grid() => json!({ "ok": false, "error": "enterprise_is_restricted" }),
        "stars.list" => {
            let items: Vec<Value> = d
                .stars
                .iter()
                .map(|c| json!({ "type": if c.starts_with('D') { "im" } else { "channel" }, "channel": c }))
                .collect();
            json!({ "ok": true, "items": items })
        }
        "stars.add" | "stars.remove" => {
            let c = p("channel");
            d.stars.retain(|s| *s != c);
            if method == "stars.add" {
                if let Some(id) = d.convs.iter().filter_map(|x| x["id"].as_str()).find(|id| *id == c) {
                    d.stars.push(Box::leak(id.to_string().into_boxed_str()));
                }
            }
            json!({ "ok": true })
        }
        "bots.info" => {
            let bot = d.users.iter().find(|u| u["is_bot"] == true && u["profile"]["bot_id"] == p("bot"));
            match bot {
                Some(b) => json!({ "ok": true, "bot": { "id": p("bot"), "name": b["profile"]["real_name"], "user_id": b["id"],
                                                        "icons": { "image_72": b["profile"]["image_72"] } } }),
                None => json!({ "ok": false, "error": "bot_not_found" }),
            }
        }
        "client.userBoot" => {
            let mut v = json!({ "ok": true, "is_open": d.open, "prefs": { "show_sidebar_avatars": true }, "team": { "plan": "std" } });
            if grid() {
                // Where `users.conversations` is restricted, the conversations come from here.
                let (ims, channels): (Vec<&Value>, Vec<&Value>) = d.convs.iter().partition(|c| c["is_im"] == true);
                v["channels"] = json!(channels);
                v["ims"] = json!(ims);
                v["starred"] = json!(d.stars);
            }
            v
        }
        "client.counts" => {
            let mut out = json!({ "ok": true, "channels": [], "ims": [], "mpims": [],
                                  "threads": { "has_unreads": !d.threads.is_empty(), "mention_count": if d.threads.is_empty() { 0 } else { 1 } } });
            let (mut dms, mut mentions) = (0, 0);
            for c in &d.convs {
                let id = c["id"].as_str().unwrap_or("");
                let (unread, m, read) = d.counts.get(id).cloned().unwrap_or((false, 0, latest(d, id)));
                let row = json!({ "id": id, "latest": latest(d, id), "last_read": read, "has_unreads": unread, "mention_count": m });
                let key = if c["is_im"] == true { "ims" } else if c["is_mpim"] == true { "mpims" } else { "channels" };
                out[key].as_array_mut().unwrap().push(row);
                if c["is_im"] == true {
                    dms += unread as u64;
                } else {
                    mentions += m;
                }
            }
            out["activity_v2"] = json!({ "dm": dms, "at_user": mentions, "thread_v2": 1 });
            out
        }
        "conversations.mark" => {
            let c = p("channel");
            let to = if p("ts").is_empty() { latest(d, c) } else { p("ts").to_string() };
            if let Some(k) = d.counts.get_mut(c) {
                *k = (false, 0, to);
            }
            json!({ "ok": true })
        }
        "users.prefs.get" => json!({ "ok": true, "prefs": {
            "all_notifications_prefs": json!({ "channels": { "C0OLD": { "muted": true } }, "global": { "global_desktop": "mentions_dms" } }).to_string(),
            "frecency": json!({
                "E2705": { "id": "E2705", "count": 40 }, "E1f440": { "id": "E1f440", "count": 32 }, "E1f389": { "id": "E1f389", "count": 25 },
                "Eparty-blob": { "id": "Eparty-blob", "count": 22 }, "E1f525": { "id": "E1f525", "count": 15 }, "E1f64f": { "id": "E1f64f", "count": 12 }
            }).to_string()
        } }),
        "users.getPresence" => {
            let who = if p("user").is_empty() { ME } else { p("user") };
            json!({ "ok": true, "presence": if d.active.contains(&who) { "active" } else { "away" }, "manual_away": false })
        }
        "dnd.info" => json!({ "ok": true, "snooze_enabled": false, "dnd_enabled": false }),
        "dnd.teamInfo" => {
            let mut users = serde_json::Map::new();
            for u in &d.snoozed {
                users.insert(u.to_string(), json!({ "snooze_enabled": true, "snooze_endtime": now() + 3600, "dnd_enabled": false }));
            }
            json!({ "ok": true, "users": users })
        }
        "emoji.list" => json!({ "ok": true, "emoji": d.custom }),
        "commands.list" => json!({ "ok": true, "commands": d.commands }),
        "saved.list" => {
            let items: Vec<Value> =
                d.saved.iter().map(|(c, ts)| json!({ "item_type": "message", "item_id": c, "ts": ts, "state": "in_progress", "date_created": now() - 3600 })).collect();
            json!({ "ok": true, "saved_items": items })
        }
        "saved.add" | "saved.delete" => {
            let (c, ts) = (p("item_id").to_string(), p("ts").to_string());
            d.saved.retain(|s| *s != (c.clone(), ts.clone()));
            if method == "saved.add" {
                d.saved.push((c, ts));
            }
            json!({ "ok": true })
        }
        "users.priority.list" => json!({ "ok": true, "users": { "vip": d.vips } }),
        "subscriptions.thread.getView" => {
            let views: Vec<Value> = d
                .threads
                .iter()
                .enumerate()
                .filter_map(|(i, (chan, ts))| {
                    let root = d.history.get(chan)?.iter().find(|m| m["ts"] == ts.as_str())?;
                    let rs = d.replies.get(&format!("{chan}/{ts}")).cloned().unwrap_or_default();
                    // The first one has a reply you haven't read.
                    let read = rs.get(rs.len().saturating_sub(if i == 0 { 2 } else { 1 })).map(|r| r["ts"].clone()).unwrap_or(root["ts"].clone());
                    let mut rm = root.clone();
                    rm["channel"] = json!(chan);
                    rm["last_read"] = read;
                    let tail = rs[rs.len().saturating_sub(3)..].to_vec();
                    Some(json!({ "root_msg": rm, "latest_replies": tail, "unread_replies": if i == 0 { json!([rs.last()]) } else { json!([]) } }))
                })
                .collect();
            json!({ "ok": true, "total_unread_replies": if views.is_empty() { 0 } else { 1 }, "has_more": false, "threads": views })
        }
        "activity.feed" => activity(d, p("types"), p("unread_only") == "true"),
        "conversations.history" => {
            let mut h = d.history.get(p("channel")).cloned().unwrap_or_default();
            let incl = p("inclusive") == "true";
            if !p("latest").is_empty() {
                let l = ts_f(p("latest"));
                h.retain(|m| {
                    let t = ts_f(m["ts"].as_str().unwrap_or(""));
                    t < l || (incl && t == l)
                });
            }
            if !p("oldest").is_empty() {
                let o = ts_f(p("oldest"));
                h.retain(|m| {
                    let t = ts_f(m["ts"].as_str().unwrap_or(""));
                    t > o || (incl && t == o)
                });
            }
            h.reverse();
            h.truncate(p("limit").parse().unwrap_or(60));
            json!({ "ok": true, "messages": h, "has_more": false })
        }
        "conversations.replies" => {
            let (chan, ts) = (p("channel"), p("ts"));
            let parent = d.history.get(chan).and_then(|h| h.iter().find(|m| m["ts"] == ts)).cloned();
            let mut out: Vec<Value> = parent.into_iter().collect();
            out.extend(d.replies.get(&format!("{chan}/{ts}")).cloned().unwrap_or_default());
            if !p("oldest").is_empty() && p("oldest") == p("latest") {
                out.retain(|m| m["ts"] == p("oldest"));
            }
            json!({ "ok": true, "messages": out, "has_more": false })
        }
        "conversations.info" => match d.convs.iter().find(|c| c["id"] == p("channel")) {
            Some(c) => json!({ "ok": true, "channel": c }),
            None => json!({ "ok": false, "error": "channel_not_found" }),
        },
        "conversations.members" => {
            let ids: Vec<&Value> = d.users.iter().filter(|u| u["is_bot"] != true).map(|u| &u["id"]).collect();
            json!({ "ok": true, "members": ids })
        }
        "conversations.list" => {
            let mut list: Vec<Value> = d.convs.iter().filter(|c| c["is_channel"] == true && c["is_private"] != true).cloned().collect();
            list.extend(d.directory.iter().cloned());
            json!({ "ok": true, "channels": list })
        }
        "pins.list" => {
            let chan = p("channel");
            let items: Vec<Value> = d
                .pins
                .get(chan)
                .into_iter()
                .flatten()
                .filter_map(|i| d.history.get(chan)?.get(*i))
                .map(|m| json!({ "type": "message", "created": now() - 3000, "created_by": m["user"], "message": m, "channel": chan }))
                .collect();
            json!({ "ok": true, "items": items })
        }
        "search.messages" | "search.inline" => {
            let q = p("query").to_ascii_lowercase().replace('*', "");
            let words: Vec<String> = q.split_whitespace().filter(|w| !w.contains(':')).map(String::from).collect();
            let only = (method == "search.inline").then(|| p("channel"));
            let mut hits = Vec::new();
            for c in &d.convs {
                let id = c["id"].as_str().unwrap_or("");
                if only.is_some_and(|o| o != id) {
                    continue;
                }
                for m in d.history.get(id).into_iter().flatten() {
                    let text = m["text"].as_str().unwrap_or("");
                    let lower = text.to_ascii_lowercase();
                    if words.is_empty() || m["subtype"].is_string() || !words.iter().all(|w| lower.contains(w.as_str())) {
                        continue;
                    }
                    let ts = m["ts"].as_str().unwrap_or("");
                    hits.push(json!({
                        "ts": ts, "user": m["user"], "text": highlight(text, &words), "files": m["files"],
                        "channel": { "id": id, "name": c["name"].as_str().unwrap_or(""), "is_im": c["is_im"] == true, "is_mpim": c["is_mpim"] == true, "is_private": c["is_private"] == true },
                        "permalink": format!("https://{}.slack.com/archives/{id}/p{}", d.domain, ts.replace('.', ""))
                    }));
                }
            }
            hits.sort_by(|a, b| ts_f(b["ts"].as_str().unwrap_or("")).total_cmp(&ts_f(a["ts"].as_str().unwrap_or(""))));
            let n = hits.len();
            if method == "search.inline" {
                json!({ "ok": true, "items": hits })
            } else {
                json!({ "ok": true, "messages": { "total": n, "paging": { "count": 20, "page": 1, "pages": 1, "total": n }, "pagination": { "page_count": 1, "total_count": n }, "matches": hits } })
            }
        }
        "reactions.get" => {
            let m = find(d, p("channel"), p("timestamp")).cloned().unwrap_or(json!({}));
            json!({ "ok": true, "message": m })
        }
        "reactions.add" | "reactions.remove" => {
            let (chan, ts, name) = (p("channel").to_string(), p("timestamp").to_string(), p("name").to_string());
            if let Some(m) = find_mut(d, &chan, &ts) {
                react(m, &name, ME, method == "reactions.add");
            }
            json!({ "ok": true })
        }
        "chat.postMessage" | "chat.meMessage" => {
            let ts = stamp();
            let mut m = json!({ "user": ME, "ts": ts, "text": p("text") });
            if let Ok(b) = serde_json::from_str::<Value>(p("blocks")) {
                m["blocks"] = b;
            }
            let (chan, thread) = (p("channel").to_string(), p("thread_ts").to_string());
            if !thread.is_empty() {
                m["thread_ts"] = json!(thread);
                d.replies.entry(format!("{chan}/{thread}")).or_default().push(m.clone());
                if let Some(root) = find_mut(d, &chan, &thread) {
                    root["reply_count"] = json!(root["reply_count"].as_u64().unwrap_or(0) + 1);
                    root["latest_reply"] = json!(ts);
                    root["thread_ts"] = json!(thread);
                }
            }
            if thread.is_empty() || p("reply_broadcast") == "true" {
                d.history.entry(chan.clone()).or_default().push(m.clone());
            }
            json!({ "ok": true, "ts": ts, "channel": chan, "message": m })
        }
        "chat.update" => {
            let (chan, ts) = (p("channel").to_string(), p("ts").to_string());
            let text = p("text").to_string();
            if let Some(m) = find_mut(d, &chan, &ts) {
                m["text"] = json!(text);
                m["edited"] = json!({ "user": ME, "ts": stamp() });
                if let Ok(b) = serde_json::from_str::<Value>(p("blocks")) {
                    m["blocks"] = b;
                }
            }
            json!({ "ok": true, "ts": ts, "channel": chan })
        }
        "chat.delete" => {
            let (chan, ts) = (p("channel"), p("ts"));
            if let Some(h) = d.history.get_mut(chan) {
                h.retain(|m| m["ts"] != ts);
            }
            json!({ "ok": true })
        }
        "conversations.open" => {
            let who: Vec<&str> = p("users").split(',').filter(|s| !s.is_empty()).collect();
            let found = (who.len() == 1).then(|| d.convs.iter().find(|c| c["is_im"] == true && c["user"] == who[0]).cloned()).flatten();
            let c = found.unwrap_or_else(|| {
                let c = json!({ "id": format!("D0{}", who.join("").trim_start_matches('U')), "is_im": who.len() == 1, "is_mpim": who.len() > 1, "user": who.first() });
                d.convs.push(c.clone());
                c
            });
            json!({ "ok": true, "channel": c })
        }
        "conversations.create" => {
            let c = json!({ "id": format!("C0NEW{}", rand_us()), "name": p("name"), "is_channel": true, "is_private": p("is_private") == "true",
                            "is_member": true, "created": now(), "creator": ME, "num_members": 1 });
            d.convs.push(c.clone());
            json!({ "ok": true, "channel": c })
        }
        "conversations.join" => {
            let id = p("channel");
            let mut c = d.directory.iter().find(|c| c["id"] == id).cloned().unwrap_or(json!({ "id": id }));
            c["is_channel"] = json!(true);
            c["is_member"] = json!(true);
            d.convs.push(c.clone());
            json!({ "ok": true, "channel": c })
        }
        "users.profile.set" => {
            if let Ok(prof) = serde_json::from_str::<Value>(p("profile")) {
                if let (Some(me), Some(obj)) = (d.users.iter_mut().find(|u| u["id"] == ME), prof.as_object()) {
                    for (k, v) in obj {
                        me["profile"][k] = v.clone();
                    }
                }
            }
            json!({ "ok": true })
        }
        // No socket: `rtm` pretends one is up.
        "client.getWebSocketURL" | "rtm.connect" => json!({ "ok": false, "error": "demo" }),
        "admin.roles.entity.listAssignments" => json!({ "ok": true, "role_assignments": [{ "role_id": "Rl0A", "users": ["U0SAM"] }] }),
        "apps.profile.get" => json!({ "ok": false, "error": "demo" }),
        _ => json!({ "ok": true }),
    }
}

fn rand_us() -> u32 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_micros()).unwrap_or(0)
}

fn find<'a>(d: &'a Space, chan: &str, ts: &str) -> Option<&'a Value> {
    d.history
        .get(chan)
        .and_then(|h| h.iter().find(|m| m["ts"] == ts))
        .or_else(|| d.replies.iter().filter(|(k, _)| k.starts_with(chan)).flat_map(|(_, v)| v).find(|m| m["ts"] == ts))
}

fn find_mut<'a>(d: &'a mut Space, chan: &str, ts: &str) -> Option<&'a mut Value> {
    if let Some(i) = d.history.get(chan).and_then(|h| h.iter().position(|m| m["ts"] == ts)) {
        return d.history.get_mut(chan).map(|h| &mut h[i]);
    }
    d.replies.iter_mut().filter(|(k, _)| k.starts_with(chan)).flat_map(|(_, v)| v.iter_mut()).find(|m| m["ts"] == ts)
}

fn react(m: &mut Value, name: &str, user: &str, on: bool) {
    if !m["reactions"].is_array() {
        m["reactions"] = json!([]);
    }
    let list = m["reactions"].as_array_mut().unwrap();
    match list.iter_mut().find(|r| r["name"] == name) {
        Some(r) => {
            let mut users: Vec<Value> = r["users"].as_array().cloned().unwrap_or_default();
            let had = users.iter().any(|u| u == user);
            if on && !had {
                users.push(json!(user));
            } else if !on && had {
                users.retain(|u| u != user);
            }
            r["count"] = json!(users.len());
            r["users"] = json!(users);
        }
        None if on => list.push(json!({ "name": name, "users": [user], "count": 1 })),
        None => {}
    }
    list.retain(|r| r["count"].as_u64().unwrap_or(0) > 0);
}

/// Slack's Activity feed, made of references the way Slack's is: the
/// messages are fetched apart.
fn activity(d: &Space, types: &str, unread_only: bool) -> Value {
    let want: Vec<&str> = types.split(',').collect();
    let mut feed: Vec<Value> = Vec::new();
    let mut add = |ty: &str, unread: bool, when: &str, key: String, mut item: Value| {
        if want.contains(&ty) || types.is_empty() {
            item["type"] = json!(ty);
            feed.push(json!({ "is_unread": unread, "feed_ts": when, "key": key, "item": item }));
        }
    };
    for c in &d.convs {
        let id = c["id"].as_str().unwrap_or("");
        let h = d.history.get(id).cloned().unwrap_or_default();
        let k = d.counts.get(id);
        let read = k.map(|k| ts_f(&k.2)).unwrap_or(f64::MAX);
        let dm = c["is_im"] == true || c["is_mpim"] == true;
        if let Some(last) = h.iter().rev().find(|m| m["user"] != ME).filter(|_| dm) {
            let when = last["ts"].as_str().unwrap_or("");
            add("dm", k.is_some_and(|k| k.0), when, format!("dm-{id}"),
                json!({ "bundle_info": { "payload": { "dm_entry": { "latest_message": { "ts": latest(d, id), "channel": id } } } } }));
        }
        for m in &h {
            let ts = m["ts"].as_str().unwrap_or("");
            if !dm && m["text"].as_str().unwrap_or("").contains(&format!("<@{ME}>")) {
                add("at_user", ts_f(ts) > read, ts, format!("at_user-{id}-{ts}"),
                    json!({ "message": { "ts": ts, "channel": id, "author_user_id": m["user"] } }));
            }
            if m["user"] == ME {
                for r in m["reactions"].as_array().into_iter().flatten() {
                    let name = r["name"].as_str().unwrap_or("");
                    if let Some(by) = r["users"].as_array().into_iter().flatten().find(|u| *u != ME) {
                        add("message_reaction", false, ts, format!("message_reaction-{id}-{ts}-{name}"),
                            json!({ "message": { "ts": ts, "channel": id }, "reaction": { "user": by, "name": name } }));
                    }
                }
            }
        }
    }
    for (key, rs) in &d.replies {
        let (chan, root) = key.split_once('/').unwrap_or_default();
        let Some(last) = rs.last() else { continue };
        let when = last["ts"].as_str().unwrap_or("");
        add("thread_v2", chan == "C0ENG", when, format!("thread_v2-{chan}-{root}"),
            json!({ "bundle_info": { "payload": { "thread_entry": { "channel_id": chan, "latest_ts": when, "thread_ts": root, "unread_msg_count": if chan == "C0ENG" { 1 } else { 0 } } } } }));
    }
    if unread_only {
        feed.retain(|i| i["is_unread"] == true);
    }
    feed.sort_by(|a, b| ts_f(b["feed_ts"].as_str().unwrap_or("")).total_cmp(&ts_f(a["feed_ts"].as_str().unwrap_or(""))));
    feed.truncate(40);
    json!({ "ok": true, "items": feed })
}

// ------------------------------------------------------------------ live

/// Stands in for Slack's websocket: up at once, and people typing now and then.
pub fn rtm(slack: crate::slack::Slack, tx: Sender<Event>) {
    use crate::rtm::Ev;
    let team = slack.creds().token;
    std::thread::spawn(move || {
        if tx.send(Event::Live(Ev::Up)).is_err() {
            return;
        }
        let people: Vec<(String, bool)> = with(&team, |d| {
            d.users.iter().filter_map(|u| u["id"].as_str()).map(|id| (id.to_string(), d.active.contains(&id))).collect()
        });
        let typing: &[(&str, &str)] = if team.ends_with("T0DEMO") { &[("D0PRIYA", "U0PRIYA"), ("C0ENG", "U0TOMAS")] } else { &[] };
        loop {
            // Presence as the socket tells it, again in case the first came before the people.
            let _ = tx.send(Event::Live(Ev::Presence(people.clone())));
            for (chan, user) in typing {
                if tx.send(Event::Live(Ev::Typing { chan: chan.to_string(), user: user.to_string() })).is_err() {
                    return;
                }
            }
            std::thread::sleep(Duration::from_millis(2500));
        }
    });
}

/// Something happens in the workspace on screen: "reaction", a few people
/// reacting to a message of yours; "message", a new DM.
fn live(kind: &str, token: &str) -> Vec<crate::rtm::Ev> {
    use crate::rtm::Ev;
    with(token, |d| match kind {
        "reaction" => {
            let Some((chan, ts, by)) = d.react.clone() else { return vec![] };
            let mut out = Vec::new();
            for (user, name) in by {
                if let Some(m) = find_mut(d, &chan, &ts) {
                    react(m, name, user, true);
                }
                out.push(Ev::Reaction { added: true, chan: chan.clone(), ts: ts.clone(), name: name.into(), user: user.into(), item_user: Some(ME.into()) });
            }
            out
        }
        "message" => {
            let Some((chan, user, text)) = d.message else { return vec![] };
            let ts = format!("{}.000500", now());
            d.history.entry(chan.into()).or_default().push(json!({ "user": user, "ts": ts, "text": text }));
            let k = d.counts.entry(chan.into()).or_insert((false, 0, "0".into()));
            k.0 = true;
            k.1 += 1;
            vec![Ev::Message { chan: chan.into(), thread: None, user: Some(user.into()) }]
        }
        _ => vec![],
    })
}

// ------------------------------------------------------------------ script

enum Step {
    Sleep(Duration),
    Key(KeyEvent),
    Mouse(MouseEventKind, u16, u16),
    Ready,
    Live(String),
}

pub struct Script {
    steps: VecDeque<Step>,
    at: Instant,
}

impl Script {
    /// `SLICK_DEMO_SCRIPT`, if there is one; it starts after `delay`.
    pub fn from_env(delay: Duration) -> Option<Self> {
        let src = std::env::var("SLICK_DEMO_SCRIPT").ok()?;
        let mut steps = VecDeque::new();
        for tok in src.split_whitespace() {
            if let Some(ms) = tok.strip_prefix("sleep:") {
                steps.push_back(Step::Sleep(Duration::from_millis(ms.parse().unwrap_or(0))));
            } else if let Some(text) = tok.strip_prefix("type:") {
                for ch in text.chars() {
                    let ch = if ch == '_' { ' ' } else { ch };
                    steps.push_back(Step::Key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE)));
                    steps.push_back(Step::Sleep(Duration::from_millis(25)));
                }
            } else if let Some(kind) = tok.strip_prefix("live:") {
                steps.push_back(Step::Live(kind.into()));
            } else if tok == "ready" {
                steps.push_back(Step::Ready);
            } else if let Some((what, at)) = tok.split_once(':') {
                let (x, y) = at.split_once(',').unwrap_or(("0", "0"));
                let (x, y) = (x.parse().unwrap_or(0), y.parse().unwrap_or(0));
                let kind = match what {
                    "click" => MouseEventKind::Down(MouseButton::Left),
                    "rclick" => MouseEventKind::Down(MouseButton::Right),
                    "wheel-up" => MouseEventKind::ScrollUp,
                    "wheel-down" => MouseEventKind::ScrollDown,
                    _ => MouseEventKind::Moved,
                };
                steps.push_back(Step::Mouse(kind, x, y));
                if what == "click" || what == "rclick" {
                    let b = if what == "click" { MouseButton::Left } else { MouseButton::Right };
                    steps.push_back(Step::Mouse(MouseEventKind::Up(b), x, y));
                }
            } else if let Some(k) = key(tok) {
                steps.push_back(Step::Key(k));
            }
        }
        Some(Self { steps, at: Instant::now() + delay })
    }

    /// Runs the steps that are due on the app on screen.
    pub fn run(&mut self, app: &mut crate::app::App) {
        while Instant::now() >= self.at {
            let Some(step) = self.steps.pop_front() else { return };
            match step {
                Step::Sleep(d) => self.at = Instant::now() + d,
                Step::Key(k) => app.on_key(k),
                Step::Mouse(kind, column, row) => app.on_mouse(MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE }),
                Step::Ready => {
                    let _ = std::fs::write(dir().join("ready"), "");
                }
                Step::Live(kind) => {
                    for ev in live(&kind, &app.slack.creds().token) {
                        app.handle(Event::Live(ev));
                    }
                }
            }
            app.dirty = true;
        }
    }
}

/// `ctrl+k`, `alt+shift+down`, `enter`, `j`…
fn key(tok: &str) -> Option<KeyEvent> {
    let mut mods = KeyModifiers::NONE;
    let mut parts: Vec<&str> = tok.split('+').collect();
    let last = parts.pop()?;
    for m in parts {
        mods |= match m {
            "ctrl" => KeyModifiers::CONTROL,
            "alt" => KeyModifiers::ALT,
            "shift" => KeyModifiers::SHIFT,
            _ => return None,
        };
    }
    let code = match last {
        "enter" => KeyCode::Enter,
        "esc" => KeyCode::Esc,
        "tab" => KeyCode::Tab,
        "backtab" => KeyCode::BackTab,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "space" => KeyCode::Char(' '),
        "backspace" => KeyCode::Backspace,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "delete" => KeyCode::Delete,
        s if s.starts_with('f') && s.len() > 1 => KeyCode::F(s[1..].parse().ok()?),
        s if s.chars().count() == 1 => KeyCode::Char(s.chars().next()?),
        _ => return None,
    };
    Some(KeyEvent::new(code, mods))
}
