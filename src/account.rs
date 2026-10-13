//! The account button at the bottom of the sidebar and its menu, as in
//! Slack: your status, away or active, pausing notifications, your profile,
//! Slack's pages, slick's own settings (language, clock, message spacing, tray icon) and signing out.

use crate::app::{App, Event};
use crate::blocks::{CAP_L, CAP_R};
use crate::compose::tail;
use crate::editor::Editor;
use crate::emoji::{self, Glyph};
use crate::files;
use crate::i18n::{self, tr, Lang};
use crate::slack::{Profile, Res, Slack};
use crate::update::{Note, State};
use crate::icons::Presence;
use crate::ui::{dim, draw_avatar, draw_image, draw_presence, emoji_row, presence_span, truncate};
use chrono::{Datelike, Days, Local, TimeZone};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Margin, Position, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;
use serde_json::Value;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Nerd Font bell with a slash: notifications paused.
const PAUSED: &str = "\u{f1f6}";
const MENU_W: u16 = 46;
/// Slack cuts a status at this many characters.
const STATUS_MAX: usize = 100;

/// Slack's suggestions: emoji, text in English and Portuguese, and when it
/// clears (an index into `CLEAR_EN`).
const PRESETS: [(&str, &str, &str, usize); 5] = [
    ("date", "In a meeting", "Em reunião", 2),
    ("bus", "Commuting", "Em trânsito", 1),
    ("face_with_thermometer", "Out sick", "Doente", 4),
    ("palm_tree", "Vacationing", "De férias", 0),
    ("house_with_garden", "Working remotely", "Trabalhando remotamente", 4),
];
const CLEAR_EN: [&str; 6] = ["Don't clear", "30 minutes", "1 hour", "4 hours", "Today", "This week"];
const CLEAR_PT: [&str; 6] = ["Não limpar", "30 minutos", "1 hora", "4 horas", "Hoje", "Esta semana"];

fn clear_label(i: usize) -> &'static str {
    if i18n::is_pt() { CLEAR_PT[i] } else { CLEAR_EN[i] }
}

/// The status dialog's stops: emoji, text, the presets, "Clear after".
const FIELDS: usize = 3 + PRESETS.len();
const AFTER: usize = FIELDS - 1;
/// The profile dialog's last stop: the photo button, then its path.
const PHOTO: usize = 3;
/// Where slick's code lives, shown in "About slick".
const REPO: &str = "github.com/leonavas/slick";
const SITE: &str = "slick.leonavas.com";

/// "slick v0.1.0 · abc1234", the commit when built from git.
pub(crate) fn version() -> String {
    let commit = env!("GIT_COMMIT");
    let v = format!("{} v{}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
    if commit.is_empty() {
        v
    } else {
        format!("{v} · {commit}")
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Item {
    Sep,
    Status,
    Away,
    Notifications,
    /// "Invite people to <workspace>", in a section of its own.
    Invite,
    Profile,
    Preferences,
    Settings,
    Downloads,
    /// The keyboard shortcuts guide, as Ctrl+/.
    Shortcuts,
    CheckUpdate,
    About,
    Logout,
    // The notifications submenu.
    Resume,
    /// Minutes; 0 is "until tomorrow".
    Pause(i64),
    /// Until Monday, 9am.
    PauseWeek,
    /// The "Pause Notifications" dialog, for any day and time.
    PauseCustom,
    NotifyPrefs,
}

const MAIN: [Item; 15] = [
    Item::Status,
    Item::Away,
    Item::Notifications,
    Item::Sep,
    Item::Invite,
    Item::Sep,
    Item::Profile,
    Item::Preferences,
    Item::Settings,
    Item::Downloads,
    Item::Sep,
    Item::Shortcuts,
    Item::CheckUpdate,
    Item::About,
    Item::Logout,
];

impl Item {
    fn in_sub(self) -> bool {
        matches!(self, Item::Resume | Item::Pause(_) | Item::PauseWeek | Item::PauseCustom | Item::NotifyPrefs)
    }

    /// Opens a submenu beside the menu.
    fn has_sub(self) -> bool {
        self == Item::Notifications
    }
}

/// Notifications paused until `until`, by a snooze or by the schedule.
#[derive(Clone, Copy)]
pub struct Paused {
    pub until: i64,
    pub snooze: bool,
}

pub struct Popup {
    sel: Item,
    /// The notifications submenu, open, with its selected row.
    sub: Option<Item>,
    /// "Sign out" was chosen once; choosing it again signs out.
    confirm: bool,
}

#[derive(Default)]
struct Picker {
    query: String,
    sel: usize,
}

pub struct StatusDialog {
    /// A shortcode without colons; empty for none.
    emoji: String,
    text: String,
    /// An index into `CLEAR`.
    clear: usize,
    field: usize,
    picker: Option<Picker>,
}

#[derive(Default)]
pub struct Account {
    pub online: bool,
    /// Away set by hand, not just idle.
    pub away: bool,
    pub paused: Option<Paused>,
    /// Whether the other person of a DM paused their notifications.
    pub others_paused: std::collections::HashMap<String, bool>,
    /// Slack's desktop notification level: everything, mentions, vips or nothing.
    pub level: Option<String>,
    pub menu: Option<Popup>,
    pub dialog: Option<StatusDialog>,
    /// "Pause Notifications", from "Custom…".
    pub pause: Option<crate::pause::PauseDialog>,
    /// "Edit your profile".
    pub profile: Option<ProfileDialog>,
    /// "About slick", open.
    pub about: bool,
    /// The settings dialog, open, with its selected row.
    pub settings: Option<usize>,
    /// The tray icon, on; main starts and stops it to match.
    pub tray: bool,
}

/// The settings dialog's rows.
#[derive(Clone, Copy, PartialEq)]
enum Opt {
    Lang(Lang),
    /// The clock: 12-hour when true, else 24-hour.
    Clock(bool),
    /// The space between messages: compact (back to back) when true.
    Spacing(bool),
    /// Photos instead of presence dots next to DMs in the sidebar.
    Photos,
    #[cfg(target_os = "linux")]
    Tray,
    /// Slack's own account settings, in the browser.
    Slack,
}

fn opts() -> Vec<Opt> {
    let mut v: Vec<Opt> = Lang::ALL.into_iter().map(Opt::Lang).collect();
    v.extend([Opt::Clock(false), Opt::Clock(true)]);
    v.extend([Opt::Spacing(false), Opt::Spacing(true)]);
    v.push(Opt::Photos);
    #[cfg(target_os = "linux")]
    v.push(Opt::Tray);
    v.push(Opt::Slack);
    v
}

pub struct ProfileDialog {
    uid: String,
    /// Full name, display name, title and the photo's path.
    fields: [Editor; 4],
    /// The path field shows once "Upload photo" is chosen.
    photo: bool,
    field: usize,
    /// Only shown: Slack sets it elsewhere.
    tz: String,
    saving: bool,
}

pub enum Ev {
    /// Online, and away set by hand.
    Presence(Res<(bool, bool)>),
    Dnd(Res<Option<Paused>>),
    /// After pausing or resuming here.
    DndSaved(Res<Option<Paused>>),
    /// The custom pause dialog's.
    Pause(crate::pause::Ev),
    Level(Res<String>),
    /// Someone else's pause; on failure the notice just does not show.
    UserDnd { uid: String, res: Res<bool> },
    /// Someone's user object, read again on opening their DM.
    User(Res<serde_json::Value>),
    /// A change made here reached Slack, or not and is undone.
    Saved { res: Res<()>, undo: Undo },
    /// The profile as Slack has it after editing, and whether the photo changed.
    ProfileSaved { uid: String, photo: bool, res: Res<Profile> },
}

pub enum Undo {
    /// The status before, and when it was to clear itself (0: never).
    Status(Option<(String, String)>, i64),
    Away(bool),
}

/// Where the last draw put things, for the mouse.
#[derive(Default)]
pub struct Areas {
    pub button: Rect,
    menu: Rect,
    sub: Rect,
    rows: Vec<(Rect, Item)>,
    /// The ✕ that clears the status.
    clear: Rect,
    dialog: Rect,
    emoji: Rect,
    text: Rect,
    presets: Vec<(Rect, usize)>,
    after: Rect,
    save: Rect,
    cancel: Rect,
    picker: Rect,
    picker_rows: Vec<(Rect, usize)>,
    /// The profile dialog's fields: where, which, and the first char shown.
    fields: Vec<(Rect, usize, usize)>,
    photo: Rect,
    about: Rect,
    /// The site's and the repository's links in "About slick".
    about_site: Rect,
    about_link: Rect,
    settings: Rect,
    settings_rows: Vec<(Rect, usize)>,
    /// The update button by your name.
    update: Rect,
}

// ---- Slack -----------------------------------------------------------------

/// Without a user, `users.getPresence` is about yourself and says whether
/// "away" was set by hand.
fn presence(s: &Slack) -> Res<(bool, bool)> {
    let v = s.call("users.getPresence", &[])?;
    Ok((v["presence"] == "active", v["manual_away"].as_bool().unwrap_or(false)))
}

/// A snooze, or the schedule ("Não perturbe" from 20h to 8h) right now.
pub(crate) fn dnd(s: &Slack) -> Res<Option<Paused>> {
    dnd_of(s, &[])
}

/// Someone else's pause, for the notice over the message box of a DM.
fn user_dnd(s: &Slack, uid: &str) -> Res<bool> {
    dnd_of(s, &[("user", uid)]).map(|p| p.is_some())
}

fn dnd_of(s: &Slack, params: &[(&str, &str)]) -> Res<Option<Paused>> {
    let v = s.call("dnd.info", params)?;
    let now = Local::now().timestamp();
    let (start, end) = (v["next_dnd_start_ts"].as_i64().unwrap_or(0), v["next_dnd_end_ts"].as_i64().unwrap_or(0));
    Ok(if v["snooze_enabled"].as_bool() == Some(true) {
        Some(Paused { until: v["snooze_endtime"].as_i64().unwrap_or(0), snooze: true })
    } else if v["dnd_enabled"].as_bool() == Some(true) && start <= now && now < end {
        Some(Paused { until: end, snooze: false })
    } else {
        None
    })
}

/// `num_minutes` must be a valid number: Slack takes anything else as
/// "no snooze" and ends the current one.
pub(crate) fn pause(s: &Slack, minutes: i64) -> Res<Option<Paused>> {
    s.call("dnd.setSnooze", &[("num_minutes", &minutes.max(1).to_string())])?;
    dnd(s)
}

pub(crate) fn resume(s: &Slack, snooze: bool) -> Res<Option<Paused>> {
    s.call(if snooze { "dnd.endSnooze" } else { "dnd.endDnd" }, &[])?;
    dnd(s)
}

/// The desktop level. `global_desktop` says everything, mentions or nothing;
/// with VIPs, "nothing" still lets theirs through.
fn level(s: &Slack) -> Res<String> {
    let v = s.call("users.prefs.get", &[])?;
    let prefs = &v["prefs"];
    let all: Value = serde_json::from_str(prefs["all_notifications_prefs"].as_str().unwrap_or("{}")).unwrap_or_default();
    let vips = !prefs["vip_users"].as_str().unwrap_or("").is_empty();
    let key = match all["global"]["global_desktop"].as_str().unwrap_or("") {
        "everything" => "everything",
        "mentions" | "mentions_dms" => "mentions",
        "nothing" if vips => "vips",
        "nothing" => "nothing",
        other => return Err(trf!("unknown notification level: {other}", "nível de notificação desconhecido: {other}")),
    };
    Ok(key.to_string())
}

/// The level in Slack's words.
fn level_label(key: &str) -> &'static str {
    match key {
        "everything" => tr("All new messages", "Todas as novas mensagens"),
        "mentions" => tr("Mentions and direct messages", "Menções e mensagens diretas"),
        "vips" => tr("Only VIPs", "Somente VIPs"),
        _ => tr("Nothing", "Nada"),
    }
}

fn set_status(s: &Slack, emoji: &str, text: &str, expiration: i64) -> Res<()> {
    let profile = serde_json::json!({ "status_text": text, "status_emoji": emoji, "status_expiration": expiration });
    s.call("users.profile.set", &[("profile", &profile.to_string())]).map(|_| ())
}

/// The photo first, as the likelier to fail, then the rest; the profile anew.
fn save_profile(s: &Slack, uid: &str, fields: [String; 3], photo: Option<std::path::PathBuf>) -> Res<Profile> {
    if let Some(path) = photo {
        s.set_photo(&path)?;
    }
    let [real_name, display_name, title] = fields;
    let profile = serde_json::json!({ "real_name": real_name, "display_name": display_name, "title": title });
    s.call("users.profile.set", &[("profile", &profile.to_string())])?;
    s.profile(uid)
}

/// A path as typed or dropped on the terminal: quotes, `file://`, `~` and
/// escaped spaces undone.
fn photo_path(s: &str) -> std::path::PathBuf {
    let s = s.trim().trim_matches(['"', '\'']).trim_start_matches("file://").replace("\\ ", " ");
    match s.strip_prefix("~/").or_else(|| s.strip_prefix("~\\")) {
        Some(rest) => crate::config::home().join(rest),
        None => s.into(),
    }
}

/// The part of a one-line field that fits `w` cells with the cursor in view:
/// its first char, the text and the cursor's column.
pub(crate) fn window(e: &Editor, w: usize) -> (usize, String, usize) {
    let chars: Vec<char> = e.text.chars().collect();
    let cur = e.cursor.min(chars.len());
    let width = |s: &[char]| s.iter().map(|c| c.width().unwrap_or(0)).sum::<usize>();
    let mut start = 0;
    while start < cur && width(&chars[start..cur]) >= w {
        start += 1;
    }
    let mut shown = String::new();
    let mut used = 0;
    for &c in &chars[start..] {
        used += c.width().unwrap_or(0);
        if used > w {
            break;
        }
        shown.push(c);
    }
    (start, shown, width(&chars[start..cur]))
}

/// The char under column `dx` of a field showing from `start`.
fn cursor_at(e: &Editor, start: usize, dx: u16) -> usize {
    let mut x = 0;
    let mut n = start;
    for c in e.text.chars().skip(start) {
        x += c.width().unwrap_or(0);
        if x > dx as usize {
            break;
        }
        n += 1;
    }
    n
}

fn set_away(s: &Slack, away: bool) -> Res<()> {
    s.call("users.setPresence", &[("presence", if away { "away" } else { "auto" })]).map(|_| ())
}

// ---- time ------------------------------------------------------------------

/// The end of the day `days` from today, local time.
fn end_of_day(days: u64) -> i64 {
    let day = Local::now().date_naive() + Days::new(days);
    day.and_hms_opt(23, 59, 59).and_then(|t| Local.from_local_datetime(&t).single()).map_or(0, |t| t.timestamp())
}

/// When a status chosen with "Clear after" goes away; 0 never.
fn expiration(clear: usize) -> i64 {
    let now = Local::now();
    match clear {
        1 => now.timestamp() + 30 * 60,
        2 => now.timestamp() + 3600,
        3 => now.timestamp() + 4 * 3600,
        4 => end_of_day(0),
        5 => end_of_day(6 - now.weekday().num_days_from_monday() as u64),
        _ => 0,
    }
}

/// Minutes until 9am tomorrow, Slack's "until tomorrow".
fn until_tomorrow() -> i64 {
    until_nine(1)
}

/// Minutes until 9am next Monday (a week from a Monday), Slack's "until next week".
fn until_next_week() -> i64 {
    until_nine(7 - Local::now().weekday().num_days_from_monday() as u64)
}

/// Minutes until 9am `days` from today, local time.
fn until_nine(days: u64) -> i64 {
    let now = Local::now();
    let at = (now.date_naive() + Days::new(days)).and_hms_opt(9, 0, 0).and_then(|t| Local.from_local_datetime(&t).single());
    at.map_or(12 * 60, |t| (t.timestamp() - now.timestamp()) / 60)
}

/// "until 14:30", "until tomorrow, 8:00", "until Oct 17, 8:00";
/// "até 14:30", "até amanhã, 8:00", "até 17/10, 8:00" (or "2:30 PM"…).
pub(crate) fn until(ts: i64) -> String {
    let Some(t) = Local.timestamp_opt(ts, 0).single() else { return String::new() };
    let today = Local::now().date_naive();
    let hhmm = i18n::clock(&t);
    match t.date_naive() {
        d if d == today => trf!("until {hhmm}", "até {hhmm}"),
        d if today.succ_opt() == Some(d) => trf!("until tomorrow, {hhmm}", "até amanhã, {hhmm}"),
        _ => {
            let (month, day, dm) = (&i18n::month(&t)[..3], t.day(), t.format("%d/%m"));
            trf!("until {month} {day}, {hhmm}", "até {dm}, {hhmm}")
        }
    }
}

// ---- state -----------------------------------------------------------------

fn step(items: &[Item], cur: Item, delta: isize) -> Item {
    let n = items.len() as isize;
    let mut i = items.iter().position(|x| *x == cur).unwrap_or(0) as isize;
    for _ in 0..n {
        i = (i + delta).rem_euclid(n);
        if items[i as usize] != Item::Sep {
            break;
        }
    }
    items[i as usize]
}

/// The notifications submenu's rows; those without an item are only text.
fn sub_rows(app: &App, _parent: Item) -> Vec<(Option<Item>, String, String)> {
    let mut rows = Vec::new();
    // Paused: until when, and a way back, before pausing anew.
    if let Some(p) = app.account.paused {
        rows.push((None, trf!("{PAUSED} Paused {}", "{PAUSED} Pausadas {}", until(p.until)), String::new()));
        rows.push((Some(Item::Sep), String::new(), String::new()));
        rows.push((Some(Item::Resume), tr("Resume notifications", "Retomar notificações").into(), String::new()));
        rows.push((Some(Item::Sep), String::new(), String::new()));
    }
    rows.push((None, tr("Pause notifications for…", "Pausar notificações por…").into(), String::new()));
    let pauses = [
        (Item::Pause(30), tr("30 minutes", "30 minutos")),
        (Item::Pause(60), tr("1 hour", "1 hora")),
        (Item::Pause(120), tr("2 hours", "2 horas")),
        (Item::Pause(0), tr("Until tomorrow", "Até amanhã")),
        (Item::PauseWeek, tr("Until next week", "Até a próxima semana")),
        (Item::PauseCustom, tr("Custom…", "Personalizado…")),
    ];
    for (item, label) in pauses {
        rows.push((Some(item), label.into(), String::new()));
    }
    rows.push((Some(Item::Sep), String::new(), String::new()));
    let level = app.account.level.as_deref().map_or("…", level_label);
    rows.push((None, trf!("Notify me about: {level}", "Notificar sobre: {level}"), String::new()));
    rows.push((Some(Item::NotifyPrefs), tr("Notification preferences", "Preferências de notificação").into(), "↗".into()));
    rows
}

fn sub_items(app: &App, parent: Item) -> Vec<Item> {
    sub_rows(app, parent).into_iter().filter_map(|(i, ..)| i).collect()
}

/// The row a submenu opens on: its first item.
fn first_sub(app: &App, parent: Item) -> Option<Item> {
    sub_items(app, parent).into_iter().find(|i| *i != Item::Sep)
}

/// Your status emoji: text, or a picture for custom ones; a status with
/// only text gets a speech balloon, as Slack shows it.
fn glyph(app: &App, emoji: &str, text: &str) -> Option<Glyph> {
    let name = emoji.trim_matches(':');
    if name.is_empty() {
        return (!text.is_empty()).then(|| Glyph::Text("💬".into()));
    }
    app.emoji.glyph(name)
}

/// Text to print for a glyph: the emoji, or two cells for its picture.
fn shown(g: &Glyph) -> String {
    match g {
        Glyph::Text(e) => e.clone(),
        Glyph::Image(_) => "  ".into(),
    }
}

/// Columns of a two-row photo, square on screen; none without graphics.
fn avatar_cols(app: &App) -> Option<u16> {
    let s = app.picker.as_ref()?.font_size();
    Some(((2.0 * s.height as f64 / s.width.max(1) as f64).round() as u16).clamp(3, 6))
}

impl App {
    fn my_id(&self) -> Option<String> {
        self.me.as_ref().map(|m| m.user_id.clone())
    }

    fn my_status(&self) -> (String, String) {
        self.my_id().and_then(|me| self.users.status(&me).cloned()).unwrap_or_default()
    }

    /// Presence and pause, and the notification level when asked.
    pub(crate) fn refresh_account(&self, with_level: bool) {
        self.spawn(|s| Event::Account(Ev::Presence(presence(s))));
        self.spawn(|s| Event::Account(Ev::Dnd(dnd(s))));
        if with_level {
            self.spawn(|s| Event::Account(Ev::Level(level(s))));
        }
    }

    /// Your pause alone: it ends, or gets set elsewhere, without telling us.
    pub(crate) fn refresh_dnd(&self) {
        self.spawn(|s| Event::Account(Ev::Dnd(dnd(s))));
    }

    /// Pauses notifications, as from the menu, for `minutes`: in every
    /// workspace, which `Workspaces` takes from here.
    pub(crate) fn pause_notifications(&mut self, minutes: i64) {
        self.dnd_all = Some(crate::pause::DndAll::Pause { minutes, vips: None });
    }

    /// Ends the pause in every workspace (`Workspaces`).
    pub(crate) fn resume_everywhere(&mut self) {
        self.dnd_all = Some(crate::pause::DndAll::Resume);
    }

    /// Ends the pause in this workspace.
    pub(crate) fn resume_notifications(&self) {
        let snooze = self.account.paused.is_none_or(|p| p.snooze);
        self.spawn(move |s| Event::Account(Ev::DndSaved(resume(s, snooze))));
    }

    /// Once per opening of a DM: is the other person paused?
    pub(crate) fn refresh_user_dnd(&self, uid: String) {
        self.spawn(move |s| {
            let res = user_dnd(s, &uid);
            Event::Account(Ev::UserDnd { uid, res })
        });
    }

    /// Someone's name, photo and status as Slack has them now.
    pub(crate) fn refresh_user(&self, uid: String) {
        self.spawn(move |s| Event::Account(Ev::User(s.user(&uid))));
    }

    /// A user object, from the websocket (`user_change`) or `users.info`:
    /// the sidebar, messages, the composer and an open profile card all
    /// read from `users`, so they show the change on the next draw.
    pub(crate) fn on_user(&mut self, u: &serde_json::Value) {
        let Some(uid) = u["id"].as_str().map(String::from) else { return };
        let (name, avatar) = (self.users.names.get(&uid).cloned(), self.users.avatars.get(&uid).cloned());
        self.users.apply(u);
        if self.users.avatars.get(&uid) != avatar.as_ref() && avatar.is_some() {
            self.forget_photo(&uid);
        }
        let status = self.users.status(&uid).map(|(e, t)| [e.as_str(), t.as_str()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" "));
        if let Some(p) = self.profile.as_mut().filter(|(id, _)| *id == uid).and_then(|(_, p)| p.as_mut()) {
            p.status = status.unwrap_or_default();
            if let Some(n) = self.users.names.get(&uid) {
                p.name = n.clone();
            }
        }
        // A new name moves their DM in the sidebar.
        if self.users.names.get(&uid) != name.as_ref() {
            self.resort();
        }
    }

    pub(crate) fn on_account(&mut self, ev: Ev) {
        match ev {
            Ev::Presence(Ok((online, away))) => {
                self.account.online = online;
                self.account.away = away;
                if let Some(me) = self.my_id() {
                    self.on_presence(vec![(me, online)]);
                }
            }
            Ev::Dnd(Ok(p)) | Ev::DndSaved(Ok(p)) => self.account.paused = p,
            Ev::Pause(e) => self.on_pause(e),
            Ev::Level(Ok(l)) => self.account.level = Some(l),
            Ev::UserDnd { uid, res: Ok(p) } => {
                self.account.others_paused.insert(uid, p);
            }
            Ev::UserDnd { uid, res: Err(_) } => {
                self.account.others_paused.remove(&uid);
            }
            Ev::User(Ok(u)) => self.on_user(&u),
            // What was read before stays.
            Ev::User(Err(_)) => {}
            // What was read before stays.
            Ev::Presence(Err(_)) | Ev::Dnd(Err(_)) | Ev::Level(Err(_)) => {}
            Ev::DndSaved(Err(e)) => self.error = Some(e),
            Ev::Saved { res: Ok(()), .. } => {}
            Ev::Saved { res: Err(e), undo } => {
                self.error = Some(e);
                match undo {
                    Undo::Status(prev, end) => {
                        self.put_status(prev, end);
                    }
                    Undo::Away(away) => self.show_away(away),
                }
            }
            Ev::ProfileSaved { uid, photo, res: Ok(p) } => {
                // The name everywhere: sidebar, messages, the menus.
                self.users.names.insert(uid.clone(), p.name.clone());
                if photo {
                    self.forget_photo(&uid);
                }
                if !p.thumb.is_empty() {
                    self.users.avatars.insert(uid.clone(), p.thumb.clone());
                }
                if let Some(card) = self.profile.as_mut().filter(|(u, _)| *u == uid) {
                    card.1 = Some(p);
                }
                self.account.profile = None;
                self.status = tr("profile updated", "perfil atualizado").into();
                self.resort();
            }
            Ev::ProfileSaved { res: Err(e), .. } => {
                if let Some(d) = self.account.profile.as_mut() {
                    d.saving = false;
                }
                self.status = String::new();
                self.error = Some(e);
            }
        }
    }

    pub(crate) fn on_update(&mut self, note: Note) {
        match note {
            Note::Found { ver, assets } => self.update = Some(State::found(self.update.take(), ver, assets)),
            Note::Installed(Ok(ver)) => {
                self.status = trf!("updated to {ver} · click restart or Ctrl+O", "atualizado para {ver} · clique em reiniciar ou Ctrl+O");
                self.update = Some(State::Installed(ver));
            }
            Note::Checked(Ok(Some((ver, assets)))) => {
                self.status = trf!("slick {ver} is available", "slick {ver} disponível");
                self.update = Some(State::found(self.update.take(), ver, assets));
            }
            Note::Checked(Ok(None)) => {
                let v = crate::update::current();
                self.brief(trf!("slick is up to date ({v})", "slick está atualizado ({v})"), 10);
            }
            Note::Checked(Err(e)) => {
                self.status = String::new();
                self.error = Some(e);
            }
            Note::Installed(Err(e)) => {
                self.error = Some(trf!("didn't update: {e}", "não atualizou: {e}"));
                if let Some(State::Installing { ver, assets }) = self.update.take() {
                    self.update = Some(State::Available { ver, assets });
                }
            }
        }
    }

    /// "Check for updates": looks now; with one installed already, restarts.
    fn check_updates(&mut self) {
        if matches!(self.update, Some(State::Installed(_))) {
            self.restart = true;
            self.quit = true;
            return;
        }
        self.error = None;
        if crate::update::check_disabled() {
            self.status = tr("development build: updates are disabled", "build de desenvolvimento: atualizações desligadas").into();
            return;
        }
        self.status = tr("Checking for updates…", "Procurando atualizações…").into();
        self.spawn(|_| crate::update::check_now());
    }

    /// The update button: install, or restart once installed.
    fn update_click(&mut self) {
        match self.update.take() {
            Some(State::Available { ver, assets }) => {
                if let Some(how) = crate::update::how() {
                    self.status = how;
                    self.update = Some(State::Available { ver, assets });
                } else {
                    self.status = trf!("downloading slick {ver}…", "baixando slick {ver}…");
                    self.update = Some(State::Installing { ver: ver.clone(), assets: assets.clone() });
                    self.spawn(move |_| crate::update::install_now(ver, assets));
                }
            }
            Some(State::Installed(ver)) => {
                self.update = Some(State::Installed(ver));
                self.restart = true;
                self.quit = true;
            }
            other => self.update = other,
        }
        self.dirty = true;
    }

    /// Drops the old photo in every size, here and on disk, so the new one loads.
    fn forget_photo(&mut self, uid: &str) {
        let prefixes = ["avatar-", "avatar-big-", "avatar-512-", "photo-"].map(|p| format!("{p}{uid}"));
        let stale = |k: &str| prefixes.iter().any(|p| k.strip_prefix(p.as_str()).is_some_and(|r| r.is_empty() || r.starts_with(['+', '.'])));
        let keys: Vec<String> = self.images.keys().filter(|k| stale(k)).cloned().collect();
        for k in keys {
            let _ = std::fs::remove_file(crate::cache::image_path(&k));
            self.images.remove(&k);
        }
        self.protocols.retain(|(k, ..), _| !stale(k));
    }

    pub(crate) fn open_profile_editor(&mut self, p: &Profile) {
        let Some(uid) = self.my_id() else { return };
        let mut fields: [Editor; 4] = Default::default();
        for (e, text) in fields.iter_mut().zip([&p.real_name, &p.display_name, &p.title]) {
            e.set(text.clone());
        }
        self.account.menu = None;
        self.account.profile = Some(ProfileDialog { uid, fields, photo: false, field: 0, tz: p.tz_label.clone(), saving: false });
        self.dirty = true;
    }

    fn submit_profile(&mut self) {
        let Some(d) = self.account.profile.as_mut() else { return };
        if d.saving {
            return;
        }
        let [real, display, title, path] = d.fields.each_ref().map(|e| e.text.trim().to_string());
        let photo = Some(path).filter(|p| d.photo && !p.is_empty()).map(|p| photo_path(&p));
        let (uid, changed) = (d.uid.clone(), photo.is_some());
        d.saving = true;
        self.status = tr("saving profile…", "salvando perfil…").into();
        self.dirty = true;
        self.spawn(move |s| {
            let res = save_profile(s, &uid, [real, display, title], photo);
            Event::Account(Ev::ProfileSaved { uid, photo: changed, res })
        });
    }

    pub(crate) fn profile_paste(&mut self, s: &str) {
        let line = s.lines().next().unwrap_or("").trim();
        let Some(d) = self.account.profile.as_mut() else { return };
        if d.field == PHOTO {
            d.photo = true;
        }
        d.fields[d.field].insert(line);
    }

    pub(crate) fn profile_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        self.dirty = true;
        let Some(d) = self.account.profile.as_mut() else { return };
        let button = d.field == PHOTO && !d.photo;
        if (ctrl && matches!(k.code, KeyCode::Enter | KeyCode::Char('j'))) || (k.code == KeyCode::Enter && !button) {
            return self.submit_profile();
        }
        match k.code {
            KeyCode::Esc => return self.account.profile = None,
            KeyCode::Tab | KeyCode::Down => return d.field = (d.field + 1) % 4,
            KeyCode::BackTab | KeyCode::Up => return d.field = (d.field + 3) % 4,
            KeyCode::Enter | KeyCode::Char(' ') if button => return d.photo = true,
            _ if button => return,
            _ => {}
        }
        let e = &mut d.fields[d.field];
        let word = ctrl || k.modifiers.contains(KeyModifiers::ALT);
        match k.code {
            KeyCode::Left if ctrl => e.word_left(),
            KeyCode::Right if ctrl => e.word_right(),
            KeyCode::Left => e.left(),
            KeyCode::Right => e.right(),
            KeyCode::Home => e.home(),
            KeyCode::Char('a') if ctrl => e.home(),
            KeyCode::End => e.end(),
            KeyCode::Char('e') if ctrl => e.end(),
            KeyCode::Backspace if word => e.delete_word(),
            KeyCode::Char('w') if ctrl => e.delete_word(),
            KeyCode::Backspace => e.backspace(),
            KeyCode::Delete if ctrl => e.delete_word_forward(),
            KeyCode::Delete => e.delete(),
            KeyCode::Char('u') if ctrl => e.set(String::new()),
            KeyCode::Char(c) if !ctrl => e.insert(c.encode_utf8(&mut [0; 4])),
            _ => {}
        }
    }

    fn profile_mouse(&mut self, m: MouseEvent, at: Position) {
        if m.kind != MouseEventKind::Down(MouseButton::Left) {
            return;
        }
        let a = &self.areas.account;
        let hit = a.fields.iter().find(|(r, ..)| r.contains(at)).map(|(r, i, start)| (*i, *start, at.x - r.x));
        let (save, cancel, photo) = (a.save.contains(at), a.cancel.contains(at), a.photo.contains(at));
        let Some(d) = self.account.profile.as_mut() else { return };
        if cancel {
            self.account.profile = None;
        } else if save {
            self.submit_profile();
        } else if photo {
            (d.photo, d.field) = (true, PHOTO);
        } else if let Some((i, start, dx)) = hit {
            d.field = i;
            d.fields[i].cursor = cursor_at(&d.fields[i], start, dx);
        }
    }

    /// Your status here, with when it clears itself (0: never); the one it replaced.
    fn put_status(&mut self, status: Option<(String, String)>, end: i64) -> (Option<(String, String)>, i64) {
        let Some(me) = self.my_id() else { return (None, 0) };
        let prev_end = match end {
            0 => self.users.status_ends.remove(&me),
            end => self.users.status_ends.insert(me.clone(), end),
        };
        // What Slack sent for the old emoji would stand in for the new one.
        self.users.status_unicode.remove(&me);
        self.users.status_image.remove(&me);
        let prev = match status {
            Some(s) => self.users.statuses.insert(me, s),
            None => self.users.statuses.remove(&me),
        };
        (prev, prev_end.unwrap_or(0))
    }

    fn show_away(&mut self, away: bool) {
        self.account.away = away;
        self.account.online = !away;
        if let Some(me) = self.my_id() {
            self.on_presence(vec![(me, !away)]);
        }
    }

    /// Here at once, then in Slack; `emoji` with its colons.
    pub(crate) fn save_status(&mut self, emoji: String, text: String, expiration: i64) {
        let status = (!emoji.is_empty() || !text.is_empty()).then(|| (emoji.clone(), text.clone()));
        let (prev, prev_end) = self.put_status(status, expiration);
        self.spawn(move |s| Event::Account(Ev::Saved { res: set_status(s, &emoji, &text, expiration), undo: Undo::Status(prev, prev_end) }));
    }

    pub(crate) fn clear_status(&mut self) {
        self.save_status(String::new(), String::new(), 0);
    }

    pub(crate) fn toggle_away(&mut self) {
        let was = self.account.away;
        self.show_away(!was);
        self.spawn(move |s| Event::Account(Ev::Saved { res: set_away(s, !was), undo: Undo::Away(was) }));
    }

    pub(crate) fn open_account_menu(&mut self) {
        if self.me.is_none() {
            return;
        }
        self.menu = None;
        self.account.menu = Some(Popup { sel: Item::Status, sub: None, confirm: false });
        self.refresh_account(false);
        self.dirty = true;
    }

    pub(crate) fn open_status_dialog(&mut self) {
        let (emoji, text) = self.my_status();
        let emoji = emoji.trim_matches(':').to_string();
        self.account.dialog = Some(StatusDialog { emoji, text, clear: 0, field: 1, picker: None });
    }

    fn choose(&mut self, item: Item) {
        let page = |app: &Self, path: &str| app.team_url().map(|u| format!("{u}/{path}"));
        let mut close = true;
        match item {
            Item::Sep => close = false,
            Item::Status => self.open_status_dialog(),
            Item::Away => self.toggle_away(),
            Item::Notifications => {
                close = false;
                let first = first_sub(self, item);
                if let Some(p) = self.account.menu.as_mut() {
                    (p.sel, p.sub) = (item, first);
                }
                self.spawn(|s| Event::Account(Ev::Level(level(s))));
            }
            Item::Invite => self.open_invite(),
            Item::Profile => {
                if let Some(me) = self.my_id() {
                    self.open_profile(me);
                }
            }
            Item::Preferences => page(self, "account/preferences").iter().for_each(|u| self.open_url(u)),
            Item::Settings => self.open_settings(),
            Item::NotifyPrefs => page(self, "account/notifications").iter().for_each(|u| self.open_url(u)),
            Item::Downloads => {
                let dir = files::downloads_dir();
                let _ = std::fs::create_dir_all(&dir);
                self.open_url(&dir.to_string_lossy());
            }
            Item::Shortcuts => self.open_help(),
            Item::CheckUpdate => self.check_updates(),
            Item::About => self.account.about = true,
            Item::Logout => {
                let confirmed = self.account.menu.as_ref().is_some_and(|p| p.confirm);
                if confirmed {
                    self.account.menu = None;
                    self.logout();
                } else if let Some(p) = self.account.menu.as_mut() {
                    p.confirm = true;
                    close = false;
                }
            }
            Item::Resume => self.resume_everywhere(),
            Item::Pause(min) => self.pause_notifications(if min == 0 { until_tomorrow() } else { min }),
            Item::PauseWeek => self.pause_notifications(until_next_week()),
            Item::PauseCustom => self.open_pause(),
        }
        if close {
            self.account.menu = None;
        }
        self.dirty = true;
    }

    /// Switches the interface at once and remembers it in the config.
    fn set_language(&mut self, l: Lang) {
        i18n::set(l);
        if let Err(e) = crate::config::set(&[("language", l.code().to_string())]) {
            self.error = Some(trf!("didn't save the language: {e}", "não salvou o idioma: {e}"));
        }
    }

    /// 12- or 24-hour times at once, remembered in the config.
    fn set_clock(&mut self, h12: bool) {
        i18n::set_h12(h12);
        if let Err(e) = crate::config::set(&[("clock", if h12 { "12h" } else { "24h" }.to_string())]) {
            self.error = Some(trf!("didn't save the setting: {e}", "não salvou a configuração: {e}"));
        }
    }

    pub(crate) fn open_settings(&mut self) {
        self.account.menu = None;
        let first = opts().iter().position(|o| *o == Opt::Lang(i18n::lang())).unwrap_or(0);
        self.account.settings = Some(first);
        self.dirty = true;
    }

    /// Picks or flips a row; the dialog stays open.
    fn choose_opt(&mut self, o: Opt) {
        match o {
            Opt::Lang(l) => self.set_language(l),
            Opt::Clock(h12) => self.set_clock(h12),
            Opt::Spacing(compact) => {
                self.compact = compact;
                let v = if compact { "compact" } else { "comfortable" };
                if let Err(e) = crate::config::set(&[("message_spacing", v.to_string())]) {
                    self.error = Some(trf!("didn't save the setting: {e}", "não salvou a configuração: {e}"));
                }
            }
            Opt::Photos => {
                self.sidebar_avatars = !self.sidebar_avatars;
                if let Err(e) = crate::config::set(&[("sidebar_avatars", self.sidebar_avatars.to_string())]) {
                    self.error = Some(trf!("didn't save the setting: {e}", "não salvou a configuração: {e}"));
                }
            }
            #[cfg(target_os = "linux")]
            Opt::Tray => {
                self.account.tray = !self.account.tray;
                if let Err(e) = crate::config::set(&[("tray", self.account.tray.to_string())]) {
                    self.error = Some(trf!("didn't save the setting: {e}", "não salvou a configuração: {e}"));
                }
            }
            Opt::Slack => {
                if let Some(u) = self.team_url() {
                    self.open_url(&format!("{u}/account/settings"));
                }
            }
        }
        self.dirty = true;
    }

    pub(crate) fn settings_key(&mut self, k: KeyEvent) {
        let Some(sel) = self.account.settings else { return };
        let all = opts();
        let n = all.len();
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') => self.account.settings = None,
            KeyCode::Up | KeyCode::Char('k') | KeyCode::BackTab => self.account.settings = Some((sel + n - 1) % n),
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => self.account.settings = Some((sel + 1) % n),
            KeyCode::Enter | KeyCode::Char(' ') => self.choose_opt(all[sel]),
            _ => {}
        }
        self.dirty = true;
    }

    pub(crate) fn about_key(&mut self, k: KeyEvent) {
        if matches!(k.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q')) {
            self.account.about = false;
        }
        self.dirty = true;
    }

    pub(crate) fn account_key(&mut self, k: KeyEvent) {
        let Some(parent) = self.account.menu.as_ref().map(|p| p.sel) else { return };
        let sub = sub_items(self, parent);
        let Some(p) = self.account.menu.as_mut() else { return };
        let go = matches!(k.code, KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') | KeyCode::Char(' '));
        match p.sub {
            Some(s) => match k.code {
                KeyCode::Up | KeyCode::Char('k') => p.sub = Some(step(&sub, s, -1)),
                KeyCode::Down | KeyCode::Char('j') => p.sub = Some(step(&sub, s, 1)),
                KeyCode::Esc | KeyCode::Left | KeyCode::Char('h') => p.sub = None,
                _ if go => self.choose(s),
                _ => {}
            },
            None => match k.code {
                KeyCode::Up | KeyCode::Char('k') => (p.sel, p.confirm) = (step(&MAIN, p.sel, -1), false),
                KeyCode::Down | KeyCode::Char('j') => (p.sel, p.confirm) = (step(&MAIN, p.sel, 1), false),
                KeyCode::Esc => self.account.menu = None,
                KeyCode::Backspace | KeyCode::Delete if p.sel == Item::Status => self.clear_status(),
                _ if go => {
                    let sel = p.sel;
                    self.choose(sel);
                }
                _ => {}
            },
        }
        self.dirty = true;
    }

    fn apply_preset(d: &mut StatusDialog, i: usize) {
        let (emoji, en, pt, clear) = PRESETS[i];
        (d.emoji, d.text, d.clear, d.field) = (emoji.into(), tr(en, pt).into(), clear, 1);
    }

    fn pick_emoji(&mut self, i: usize) {
        let Some(d) = self.account.dialog.as_mut() else { return };
        let Some(p) = d.picker.take() else { return };
        if let Some((name, _)) = self.emoji.search(&p.query, emoji::ROWS).into_iter().nth(i) {
            d.emoji = name;
        }
        d.field = 1;
    }

    fn submit_status(&mut self) {
        let Some(d) = self.account.dialog.take() else { return };
        let text: String = d.text.trim().chars().take(STATUS_MAX).collect();
        let emoji = if d.emoji.is_empty() { String::new() } else { format!(":{}:", d.emoji) };
        let exp = if text.is_empty() && emoji.is_empty() { 0 } else { expiration(d.clear) };
        self.save_status(emoji, text, exp);
    }

    pub(crate) fn status_paste(&mut self, s: &str) {
        let line = s.lines().next().unwrap_or("").trim();
        let Some(d) = self.account.dialog.as_mut() else { return };
        match d.picker.as_mut() {
            Some(p) => p.query.push_str(line),
            None => {
                d.text.push_str(line);
                d.text = d.text.chars().take(STATUS_MAX).collect();
                d.field = 1;
            }
        }
    }

    pub(crate) fn status_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        self.dirty = true;
        let Some(d) = self.account.dialog.as_mut() else { return };
        if let Some(p) = d.picker.as_mut() {
            let n = self.emoji.search(&p.query, emoji::ROWS).len().max(1);
            match k.code {
                KeyCode::Esc => d.picker = None,
                KeyCode::Up | KeyCode::BackTab => p.sel = (p.sel + n - 1) % n,
                KeyCode::Down | KeyCode::Tab => p.sel = (p.sel + 1) % n,
                KeyCode::Enter => {
                    let sel = p.sel;
                    self.pick_emoji(sel);
                }
                KeyCode::Backspace => (_, p.sel) = (p.query.pop(), 0),
                KeyCode::Char('u') if ctrl => (p.query, p.sel) = (String::new(), 0),
                KeyCode::Char(c) if !ctrl && c != ':' => {
                    p.query.push(c);
                    p.sel = 0;
                }
                _ => {}
            }
            return;
        }
        let save = (ctrl && matches!(k.code, KeyCode::Enter | KeyCode::Char('j'))) || (k.code == KeyCode::Enter && matches!(d.field, 1 | AFTER));
        if save {
            return self.submit_status();
        }
        match k.code {
            KeyCode::Esc => self.account.dialog = None,
            KeyCode::Tab | KeyCode::Down => d.field = (d.field + 1) % FIELDS,
            KeyCode::BackTab | KeyCode::Up => d.field = (d.field + FIELDS - 1) % FIELDS,
            KeyCode::Enter if d.field == 0 => d.picker = Some(Picker::default()),
            KeyCode::Enter => Self::apply_preset(d, d.field - 2),
            KeyCode::Left if d.field == AFTER => d.clear = (d.clear + CLEAR_EN.len() - 1) % CLEAR_EN.len(),
            KeyCode::Right | KeyCode::Char(' ') if d.field == AFTER => d.clear = (d.clear + 1) % CLEAR_EN.len(),
            KeyCode::Backspace | KeyCode::Delete if d.field == 0 => d.emoji.clear(),
            KeyCode::Backspace => {
                d.text.pop();
                d.field = 1;
            }
            KeyCode::Char('u') if ctrl => d.text.clear(),
            KeyCode::Char(c) if !ctrl => {
                if d.text.chars().count() < STATUS_MAX {
                    d.text.push(c);
                }
                d.field = 1;
            }
            _ => {}
        }
    }

    /// The button, the menu and the status dialog; true when the event was theirs.
    pub(crate) fn account_mouse(&mut self, m: MouseEvent) -> bool {
        let at = Position::new(m.column, m.row);
        let left = m.kind == MouseEventKind::Down(MouseButton::Left);
        // Modal: clicks outside do not throw the edits away.
        if self.account.profile.is_some() {
            self.profile_mouse(m, at);
            self.dirty = true;
            return true;
        }
        if self.account.dialog.is_some() {
            self.status_mouse(m, at);
            self.dirty = true;
            return true;
        }
        let a = &self.areas.account;
        if self.account.settings.is_some() {
            let hit = a.settings_rows.iter().find(|(r, _)| r.contains(at)).map(|(_, i)| *i);
            let outside = !a.settings.contains(at);
            match m.kind {
                MouseEventKind::Moved => {
                    if let Some(i) = hit {
                        self.account.settings = Some(i);
                    }
                }
                _ if left && hit.is_some() => {
                    let i = hit.unwrap_or(0);
                    self.account.settings = Some(i);
                    self.choose_opt(opts()[i]);
                }
                MouseEventKind::Down(_) if outside => self.account.settings = None,
                _ => return true,
            }
            self.dirty = true;
            return true;
        }
        if self.account.about {
            if left && a.about_site.contains(at) {
                self.open_url(&format!("https://{SITE}"));
            } else if left && a.about_link.contains(at) {
                self.open_url(&format!("https://{REPO}"));
            } else if matches!(m.kind, MouseEventKind::Down(_)) && !a.about.contains(at) {
                self.account.about = false;
                self.dirty = true;
            }
            return true;
        }
        if self.account.menu.is_some() {
            let hit = a.rows.iter().find(|(r, _)| r.contains(at)).map(|(_, i)| *i);
            let first_sub = hit.filter(|i| i.has_sub()).and_then(|i| first_sub(self, i));
            let (clear, inside, button) = (a.clear.contains(at), a.menu.contains(at) || a.sub.contains(at), a.button.contains(at));
            let Some(p) = self.account.menu.as_mut() else { return true };
            match m.kind {
                MouseEventKind::Moved | MouseEventKind::Drag(_) => match hit {
                    Some(i) if i.in_sub() => p.sub = Some(i),
                    // Like Slack, the submenu opens under the pointer.
                    Some(i) if i.has_sub() && (p.sub.is_none() || p.sel != i) => {
                        (p.sel, p.sub, p.confirm) = (i, first_sub, false);
                    }
                    Some(i) if i != p.sel => (p.sel, p.sub, p.confirm) = (i, None, false),
                    _ => return true,
                },
                _ if left && clear => self.clear_status(),
                _ if left && hit.is_some() => self.choose(hit.unwrap_or(Item::Sep)),
                MouseEventKind::Down(_) if button || !inside => self.account.menu = None,
                _ => return true,
            }
            self.dirty = true;
            return true;
        }
        // Its own button inside the account's: never opens the menu.
        if left && a.update.contains(at) {
            self.update_click();
            return true;
        }
        if left && a.button.contains(at) {
            self.open_account_menu();
            return true;
        }
        false
    }

    fn status_mouse(&mut self, m: MouseEvent, at: Position) {
        let a = &self.areas.account;
        let row = |rows: &[(Rect, usize)]| rows.iter().find(|(r, _)| r.contains(at)).map(|(_, i)| *i);
        let (pick, preset) = (row(&a.picker_rows), row(&a.presets));
        let (in_picker, in_dialog) = (a.picker.contains(at), a.dialog.contains(at));
        let (emoji, text, after, save, cancel) =
            (a.emoji.contains(at), a.text.contains(at), a.after.contains(at), a.save.contains(at), a.cancel.contains(at));
        let left = m.kind == MouseEventKind::Down(MouseButton::Left);
        let Some(d) = self.account.dialog.as_mut() else { return };
        if let Some(p) = d.picker.as_mut() {
            match m.kind {
                MouseEventKind::Moved => p.sel = pick.unwrap_or(p.sel),
                _ if left && pick.is_some() => self.pick_emoji(pick.unwrap_or(0)),
                MouseEventKind::Down(_) if !in_picker => d.picker = None,
                _ => {}
            }
            return;
        }
        if !left {
            return;
        }
        if !in_dialog || cancel {
            self.account.dialog = None;
        } else if save {
            self.submit_status();
        } else if emoji {
            (d.field, d.picker) = (0, Some(Picker::default()));
        } else if text {
            d.field = 1;
        } else if let Some(i) = preset {
            Self::apply_preset(d, i);
        } else if after {
            (d.field, d.clear) = (AFTER, (d.clear + 1) % CLEAR_EN.len());
        }
    }
}

// ---- drawing -----------------------------------------------------------------

/// Rows the button takes at the bottom of the sidebar, its top line included.
pub fn button_height(app: &App) -> u16 {
    match (&app.me, avatar_cols(app)) {
        (None, _) => 0,
        (_, Some(_)) => 3,
        _ => 2,
    }
}

/// Your presence, from the account's own state, and its word.
fn my_presence(app: &App) -> (Presence, &'static str) {
    let p = Presence { active: app.account.online && !app.account.away, paused: app.account.paused.is_some() };
    (p, if p.active { tr("Active", "Ativo") } else { tr("Away", "Ausente") })
}

/// Your photo with presence and status, and your name beside it.
pub fn button(f: &mut Frame, app: &mut App, area: Rect) {
    (app.areas.account.button, app.areas.account.update) = Default::default();
    let Some(me) = app.me.clone() else { return };
    let block = Block::new().borders(Borders::TOP).border_style(dim());
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height == 0 || inner.width < 4 {
        return;
    }
    app.areas.account.button = inner;
    let uid = me.user_id;
    let name = app.users.names.get(&uid).cloned().unwrap_or(me.user);
    let (emoji, text) = app.my_status();
    let g = glyph(app, &emoji, &text);
    let name_style = if app.account.menu.is_some() { Style::new().fg(Color::Cyan).bold() } else { Style::new().bold() };
    // A pause shows as the "z" in the dot, as in Slack.
    let (pres, word) = my_presence(app);
    let dot = presence_span(app, pres);
    let dot_w = dot.width();
    // A newer version: a small button at the right of the name's line.
    let up = update_pill(app);
    let up_w = up.as_ref().map_or(0, |l| l.width() as u16);
    let up_room = if up.is_some() { up_w as usize + 2 } else { 0 };
    let up = up.map(|line| (line, Rect::new(inner.right().saturating_sub(up_w + 1).max(inner.x), inner.y, up_w, 1).intersection(inner)));
    app.areas.account.update = up.as_ref().map(|(_, r)| *r).unwrap_or_default();
    // Drawn over the name's row, after it.
    let draw_up = |f: &mut Frame| {
        if let Some((line, r)) = up {
            f.render_widget(Paragraph::new(line), r);
        }
    };

    let photo = avatar_cols(app).zip(app.users.avatars.get(&uid).cloned());
    let Some((cols, url)) = photo.filter(|_| inner.height >= 2) else {
        // No photo: presence, name and status emoji on one line.
        let status = g.as_ref().map(|g| format!(" {}", shown(g))).unwrap_or_default();
        let room = (inner.width as usize).saturating_sub(2 + dot_w + status.width() + up_room);
        let line = Line::from(vec![
            Span::raw(" "),
            dot,
            Span::raw(" "),
            Span::styled(truncate(&name, room), name_style),
            Span::raw(status),
        ]);
        f.render_widget(Paragraph::new(line), inner);
        draw_up(f);
        draw_presence(f, app, pres, inner.x + 1, inner.y);
        return;
    };
    let rect = Rect::new(inner.x + 1, inner.y, cols, 2);
    draw_avatar(f, app, &uid, &url, pres, rect);
    let x = rect.right() + 1;
    let w = inner.right().saturating_sub(x) as usize;
    let first = Line::from(Span::styled(truncate(&name, w.saturating_sub(up_room)), name_style));
    // Under the name, the status: the dot on the photo already shows presence.
    let mut second = vec![];
    let mut used = 0;
    if let Some(g) = &g {
        let e = format!("{} ", shown(g));
        used += e.width();
        second.push(Span::raw(e));
    }
    let about = if text.is_empty() { word.to_string() } else { crate::text::shortcodes(&text) };
    second.push(Span::styled(truncate(&about, w.saturating_sub(used)), dim()));
    f.render_widget(Paragraph::new(vec![first, Line::from(second)]), Rect::new(x, inner.y, w as u16, 2));
    draw_up(f);
    if let Some(Glyph::Image(img)) = g {
        draw_image(f, app, &img, Rect::new(x, inner.y + 1, 2, 1));
    }
}

/// The update button: its words and colours by where the update stands.
fn update_pill(app: &App) -> Option<Line<'static>> {
    let (label, fg, bg) = match app.update.as_ref()? {
        State::Available { ver, .. } => (format!("↑ {ver}"), Color::Black, Color::Green),
        // Faded while it works: there is nothing to click.
        State::Installing { .. } => (tr("updating…", "atualizando…").into(), Color::DarkGray, Color::Indexed(236)),
        State::Installed(_) => (tr("↻ restart", "↻ reiniciar").into(), Color::Black, Color::Green),
    };
    Some(pill(&label, fg, bg))
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let a = &mut app.areas.account;
    (a.menu, a.sub, a.clear) = Default::default();
    (a.dialog, a.emoji, a.text, a.after, a.save, a.cancel, a.picker) = Default::default();
    a.rows.clear();
    a.presets.clear();
    a.picker_rows.clear();
    a.fields.clear();
    (a.photo, a.about, a.about_site, a.about_link, a.settings) = Default::default();
    a.settings_rows.clear();
    if app.account.menu.is_some() {
        draw_menu(f, app);
    }
    if app.account.dialog.is_some() {
        draw_dialog(f, app);
    }
    if app.account.profile.is_some() {
        draw_profile_dialog(f, app);
    }
    if app.account.about {
        draw_about(f, app);
    }
    if app.account.settings.is_some() {
        draw_settings(f, app);
    }
    crate::pause::draw(f, app);
}

/// slick's settings: the language, the clock, the message spacing, the sidebar's photos, the tray icon and a way to Slack's own.
fn draw_settings(f: &mut Frame, app: &mut App) {
    let Some(sel) = app.account.settings else { return };
    let all = opts();
    // A heading over each group, a blank line between groups.
    let mut lines: Vec<(Option<usize>, Line)> = vec![(None, Line::from(""))];
    let heading = |s: &str| Line::from(format!(" {s}")).style(dim().bold());
    let mut last: Option<u8> = None;
    for (i, o) in all.iter().enumerate() {
        let group = match o {
            Opt::Lang(_) => 0,
            Opt::Clock(_) => 1,
            Opt::Spacing(_) => 2,
            Opt::Photos => 3,
            #[cfg(target_os = "linux")]
            Opt::Tray => 4,
            Opt::Slack => 5,
        };
        if last != Some(group) {
            if last.is_some() {
                lines.push((None, Line::from("")));
            }
            match group {
                0 => lines.push((None, heading(tr("Language", "Idioma")))),
                1 => lines.push((None, heading(tr("Clock", "Relógio")))),
                2 => lines.push((None, heading(tr("Message spacing", "Espaçamento das mensagens")))),
                3 => lines.push((None, heading(tr("Sidebar", "Barra lateral")))),
                4 => lines.push((None, heading(tr("System tray", "Bandeja do sistema")))),
                _ => lines.push((None, heading("Slack"))),
            }
            last = Some(group);
        }
        let text = match o {
            Opt::Lang(l) => format!("{} {}", if *l == i18n::lang() { "(•)" } else { "( )" }, l.label()),
            Opt::Clock(h12) => {
                let label = if *h12 { tr("12-hour (9:29 PM)", "12 horas (9:29 PM)") } else { tr("24-hour (21:29)", "24 horas (21:29)") };
                format!("{} {label}", if *h12 == i18n::h12() { "(•)" } else { "( )" })
            }
            Opt::Spacing(compact) => {
                let label = if *compact { tr("Compact", "Compacto") } else { tr("Comfortable", "Confortável") };
                format!("{} {label}", if *compact == app.compact { "(•)" } else { "( )" })
            }
            Opt::Photos => format!("{} {}", if app.sidebar_avatars { "[✓]" } else { "[ ]" }, tr("Show photos in the list", "Mostrar fotos na lista")),
            #[cfg(target_os = "linux")]
            Opt::Tray => format!("{} {}", if app.account.tray { "[✓]" } else { "[ ]" }, tr("Show the tray icon", "Mostrar o ícone na bandeja")),
            Opt::Slack => format!("{} ↗", tr("Account settings", "Configurações da conta")),
        };
        lines.push((Some(i), Line::from(text)));
    }
    lines.push((None, Line::from("")));
    let hint = tr("↑↓ move · Enter picks · Esc closes", "↑↓ move · Enter escolhe · Esc fecha");
    lines.push((None, Line::from(format!(" {hint}")).style(dim())));

    let area = f.area();
    let content_w = lines.iter().map(|(_, l)| l.width()).max().unwrap_or(0) as u16;
    let (w, h) = ((content_w + 8).max(44).min(area.width), (lines.len() as u16 + 3).min(area.height));
    let rect = Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h);
    f.render_widget(Clear, rect);
    let title = Line::from(tr(" Settings ", " Configurações ")).bold();
    let block = Block::bordered().border_type(BorderType::Rounded).border_style(dim()).title(title);
    let inner = block.inner(rect).inner(Margin { horizontal: 1, vertical: 0 });
    f.render_widget(block, rect);
    app.areas.account.settings = rect;

    for (n, (opt, line)) in lines.into_iter().enumerate() {
        let row = Rect::new(inner.x, inner.y + n as u16, inner.width, 1);
        if row.y >= inner.bottom() {
            break;
        }
        let Some(i) = opt else {
            f.render_widget(Paragraph::new(line), row);
            continue;
        };
        let mark = if i == sel { Span::styled("› ", Style::new().fg(Color::Cyan).bold()) } else { Span::raw("  ") };
        let style = if i == sel { Style::new().add_modifier(Modifier::REVERSED) } else { Style::new() };
        let mut spans = vec![mark];
        spans.extend(line.spans.into_iter().map(|s| s.patch_style(style)));
        f.render_widget(Paragraph::new(Line::from(spans)), row);
        app.areas.account.settings_rows.push((row, i));
    }
}

/// "About slick": the mark, the name, the version and where the code is.
fn draw_about(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let commit = env!("GIT_COMMIT");
    let version = format!("{} {}", tr("version", "versão"), env!("CARGO_PKG_VERSION"));
    let version = if commit.is_empty() { version } else { format!("{version} · {commit}") };
    // The mark as on the login screen: a picture where the terminal shows
    // one, else drawn in half blocks.
    let cell = crate::ui::dot_cell(app);
    let blocks = crate::ui::mark();
    let (mark_w, mark_h) = match cell {
        Some((fw, fh)) => (((crate::ui::MARK_ROWS as u32 * fh) / fw.max(1)) as u16, crate::ui::MARK_ROWS),
        None => (blocks.iter().map(|l| l.width()).max().unwrap_or(0) as u16, blocks.len() as u16),
    };
    let text_w = version.width().max(REPO.width()) as u16;
    // Borders and a padding of three columns, one row; the mark only fits sometimes.
    let (w, h) = (text_w.max(mark_w) + 8, mark_h + 9);
    let logo = w <= area.width && h <= area.height;
    let (w, h) = if logo { (w, h) } else { (text_w + 8, 8) };
    let (w, h) = (w.min(area.width), h.min(area.height));
    let rect = Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h);
    f.render_widget(Clear, rect);
    let block = Block::bordered().border_type(BorderType::Rounded).border_style(dim());
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    app.areas.account.about = rect;

    let mut lines = vec![Line::from("")];
    if logo {
        match cell {
            Some(_) => lines.extend((0..mark_h).map(|_| Line::from(""))),
            None => lines.extend(blocks),
        }
        lines.push(Line::from(""));
    }
    lines.push(Line::from("slick").bold());
    lines.push(Line::from(version).style(dim()));
    let site_y = inner.y + lines.len() as u16;
    lines.push(Line::from(SITE).style(dim().underlined()));
    lines.push(Line::from(REPO).style(dim().underlined()));
    f.render_widget(Paragraph::new(lines).centered(), inner);
    // Where a centered line lands.
    let at = |text: &str, y: u16| {
        let lw = text.width() as u16;
        Rect::new(inner.x + (inner.width / 2).saturating_sub(lw / 2), y, lw, 1).intersection(inner)
    };
    app.areas.account.about_site = at(SITE, site_y);
    app.areas.account.about_link = at(REPO, site_y + 1);
    if logo && cell.is_some() {
        let mark = Rect::new(inner.x + inner.width.saturating_sub(mark_w) / 2, inner.y + 1, mark_w, mark_h).intersection(inner);
        crate::ui::draw_mark(f, app, mark);
    }
}

/// A rounded button, like Block Kit's.
fn pill(label: &str, fg: Color, bg: Color) -> Line<'static> {
    Line::from(vec![
        Span::styled(CAP_L, Style::new().fg(bg)),
        Span::styled(label.to_string(), Style::new().fg(fg).bg(bg).bold()),
        Span::styled(CAP_R, Style::new().fg(bg)),
    ])
}

/// What the profile dialog stacks, top to bottom.
enum Part {
    Gap,
    Label(&'static str),
    Field(usize),
    Hint(String),
    Text(String),
    Photo,
}

fn draw_profile_dialog(f: &mut Frame, app: &mut App) {
    let Some(d) = &app.account.profile else { return };
    let (field, photo, saving, tz) = (d.field, d.photo, d.saving, d.tz.clone());
    let fields: Vec<Editor> = d.fields.to_vec();
    let team = app.me.as_ref().map(|m| m.team.clone()).unwrap_or_else(|| "Slack".into());
    let area = f.area();
    let w = 64.min(area.width.saturating_sub(4));
    let iw = w.saturating_sub(4) as usize;
    let hint = |s: &str| crate::ui::wrap(s, iw.saturating_sub(2)).into_iter().map(Part::Hint).collect::<Vec<_>>();
    let mut parts = vec![
        Part::Gap,
        Part::Label(tr("Full name", "Nome completo")),
        Part::Field(0),
        Part::Gap,
        Part::Label(tr("Display name", "Nome exibido")),
        Part::Field(1),
    ];
    parts.extend(hint(tr(
        "This could be your first name or a nickname, however you'd like people to refer to you in Slack.",
        "Pode ser seu nome ou um apelido, pense em como você quiser que as pessoas chamem você no Slack.",
    )));
    parts.extend([Part::Gap, Part::Label(tr("Title", "Cargo")), Part::Field(2)]);
    parts.extend(hint(&trf!("Let people know what you do at {team}.", "Deixe as pessoas verem o que você faz em {team}.")));
    if !tz.is_empty() {
        parts.extend([Part::Gap, Part::Label(tr("Time zone", "Fuso horário")), Part::Text(tz)]);
    }
    parts.extend([Part::Gap, Part::Label(tr("Profile photo", "Foto do perfil")), Part::Photo]);
    if photo {
        parts.push(Part::Field(PHOTO));
        parts.extend(hint(tr(
            "JPG, PNG or GIF; drag the file here or type its path.",
            "JPG, PNG ou GIF; arraste o arquivo para cá ou digite o caminho.",
        )));
    }
    let h = (parts.len() as u16 + 4).min(area.height.saturating_sub(2));
    let rect = Rect::new(area.x + (area.width - w) / 2, area.y + (area.height.saturating_sub(h)) / 2, w, h);
    f.render_widget(Clear, rect);
    let block = Block::bordered().border_type(BorderType::Rounded).border_style(dim()).title(Line::from(tr(" Edit your profile ", " Editar seu perfil ")).bold());
    let inner = block.inner(rect).inner(Margin { horizontal: 1, vertical: 0 });
    f.render_widget(block, rect);
    app.areas.account.dialog = rect;
    if inner.width < 20 || inner.height < 4 {
        return;
    }
    let mark = |n: usize| if n == field { Span::styled("› ", Style::new().fg(Color::Cyan).bold()) } else { Span::raw("  ") };
    // The buttons' row stays at the bottom; the rest is cut if it does not fit.
    let last = inner.bottom().saturating_sub(2);
    for (n, part) in parts.iter().enumerate() {
        let y = inner.y + n as u16;
        if y >= last {
            break;
        }
        let row = Rect::new(inner.x, y, inner.width, 1);
        let line = match part {
            Part::Gap => continue,
            Part::Label(s) => Line::from(Span::styled(*s, Style::new().bold())),
            Part::Hint(s) | Part::Text(s) => {
                let style = if matches!(part, Part::Hint(_)) { dim() } else { Style::new() };
                Line::from(vec![Span::raw("  "), Span::styled(s.clone(), style)])
            }
            Part::Field(i) => {
                // A shaded box as wide as the dialog, the cursor in it.
                let fw = inner.width.saturating_sub(2);
                let (start, shown, cx) = window(&fields[*i], fw.saturating_sub(2) as usize);
                let pad = (fw as usize).saturating_sub(shown.width() + 1);
                let bg = Style::new().bg(Color::Indexed(236));
                let line = Line::from(vec![mark(*i), Span::styled(format!(" {shown}{}", " ".repeat(pad)), bg)]);
                let boxed = Rect::new(inner.x + 2, y, fw, 1);
                app.areas.account.fields.push((Rect::new(boxed.x + 1, y, fw.saturating_sub(1), 1), *i, start));
                if *i == field && !saving {
                    f.set_cursor_position(((boxed.x + 1 + cx as u16).min(boxed.right().saturating_sub(1)), y));
                }
                line
            }
            Part::Photo => {
                let label = tr(" Upload photo ", " Fazer upload de foto ");
                let mut line = pill(label, Color::White, Color::DarkGray);
                line.spans.insert(0, mark(PHOTO));
                app.areas.account.photo = Rect::new(inner.x + 2, y, label.width() as u16 + 2, 1);
                line
            }
        };
        f.render_widget(Paragraph::new(line), row);
    }

    // Hint, then Cancel and Save changes on the right.
    let by = inner.bottom().saturating_sub(1);
    let save = if saving { tr(" Saving… ", " Salvando… ") } else { tr(" Save changes ", " Salvar alterações ") };
    let cancel = tr(" Cancel ", " Cancelar ");
    let save_r = Rect::new(inner.right().saturating_sub(save.width() as u16 + 2), by, save.width() as u16 + 2, 1);
    let cancel_r = Rect::new(save_r.x.saturating_sub(cancel.width() as u16 + 3), by, cancel.width() as u16 + 2, 1);
    f.render_widget(Paragraph::new(pill(save, Color::Black, Color::Green)), save_r);
    f.render_widget(Paragraph::new(pill(cancel, Color::Reset, Color::Indexed(236))), cancel_r);
    let hint_w = cancel_r.x.saturating_sub(inner.x + 1);
    let hint = tr("Tab fields · Enter saves · Esc cancels", "Tab campos · Enter salva · Esc cancela");
    f.render_widget(Paragraph::new(truncate(hint, hint_w as usize)).style(dim()), Rect::new(inner.x, by, hint_w, 1));
    (app.areas.account.save, app.areas.account.cancel) = (save_r, cancel_r);
}

/// ` left      right `, filling `w` cells.
fn padded(left: &str, right: &str, w: usize) -> String {
    let left = truncate(left, w.saturating_sub(right.width() + 3));
    let gap = w.saturating_sub(left.width() + right.width() + 2);
    format!(" {left}{}{right} ", " ".repeat(gap))
}

fn draw_menu(f: &mut Frame, app: &mut App) {
    let Some(p) = &app.account.menu else { return };
    let (sel, sub, confirm) = (p.sel, p.sub, p.confirm);
    let Some(me) = app.me.clone() else { return };
    let area = f.area();
    let b = app.areas.account.button;
    // Photo and name, a gap and the items.
    let (w, h) = (MENU_W.min(area.width), (MAIN.len() as u16 + 5).min(area.height));
    // Over the button's top line, as if it grew out of it.
    let rect = Rect::new(b.x, b.y.saturating_sub(h).max(area.y), w, h).intersection(area);
    f.render_widget(Clear, rect);
    let block = Block::bordered().border_type(BorderType::Rounded).border_style(dim());
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    app.areas.account.menu = rect;
    if inner.height < 3 {
        return;
    }

    // Photo, name in bold and presence.
    let uid = me.user_id;
    let name = app.users.names.get(&uid).cloned().unwrap_or(me.user);
    let mut x = inner.x + 1;
    let (pres, word) = my_presence(app);
    let photo = avatar_cols(app).zip(app.users.avatars.get(&uid).cloned());
    if let Some((cols, url)) = &photo {
        draw_avatar(f, app, &uid, url, pres, Rect::new(x, inner.y, *cols, 2));
        x += cols + 1;
    }
    let tw = inner.right().saturating_sub(x);
    // With a photo its dot shows presence; without one, the dot goes before the word.
    let state = match photo {
        Some(_) => Line::from(word.to_string()),
        None => Line::from(vec![presence_span(app, pres), Span::raw(format!(" {word}"))]),
    };
    let head = vec![Line::from(truncate(&name, tw as usize)).bold(), state];
    f.render_widget(Paragraph::new(head), Rect::new(x, inner.y, tw, 2));
    if photo.is_none() {
        draw_presence(f, app, pres, x, inner.y + 1);
    }

    let (emoji, text) = app.my_status();
    let iw = inner.width as usize;
    let mut sub_y = inner.y;
    for (n, item) in MAIN.iter().enumerate() {
        let row = Rect::new(inner.x, inner.y + 3 + n as u16, inner.width, 1);
        if row.y >= inner.bottom() {
            break;
        }
        if *item == Item::Sep {
            f.render_widget(Paragraph::new("─".repeat(iw)).style(dim()), row);
            continue;
        }
        let mut style = if *item == sel { Style::new().add_modifier(Modifier::REVERSED) } else { Style::new() };
        let label = match item {
            Item::Status => {
                // A field showing the status, with a ✕ that clears it.
                let set = !emoji.is_empty() || !text.is_empty();
                let g = glyph(app, &emoji, &text).unwrap_or(Glyph::Text("🙂".into()));
                let words = if set { crate::text::shortcodes(&text) } else { tr("Update your status", "Atualizar seu status").into() };
                if *item != sel {
                    style = if set { Style::new().bg(Color::Indexed(236)) } else { dim().bg(Color::Indexed(236)) };
                }
                if set {
                    app.areas.account.clear = Rect::new(row.right().saturating_sub(3), row.y, 3, 1);
                }
                padded(&format!("{} {words}", shown(&g)), if set { "✕" } else { "" }, iw)
            }
            Item::Away if app.account.away => padded(tr("Set yourself as active", "Defina seu status como ativo"), "", iw),
            Item::Away => padded(tr("Set yourself as away", "Defina seu status como ausente"), "", iw),
            Item::Notifications => {
                if *item == sel {
                    sub_y = row.y;
                    if sub.is_some() {
                        style = Style::new().bg(Color::Indexed(236));
                    }
                }
                let pause = if app.account.paused.is_some() { format!("{PAUSED} ") } else { String::new() };
                let level = truncate(app.account.level.as_deref().map_or("", level_label), iw.saturating_sub(20));
                padded(tr("Notifications", "Notificações"), &format!("{pause}{level} ›"), iw)
            }
            Item::Invite => padded(&trf!("Invite people to {}", "Convidar pessoas para {}", me.team), "", iw),
            Item::Profile => padded(tr("Profile", "Perfil"), "", iw),
            Item::Preferences => padded(tr("Preferences", "Preferências"), "↗", iw),
            Item::Settings => padded(tr("Settings", "Configurações"), "", iw),
            Item::Downloads => padded("Downloads", "", iw),
            Item::About if app.update.is_some() => {
                // The note faded, after the words.
                let note = tr("· update available", "· atualização disponível");
                let s = padded(tr("About slick", "Sobre o slick"), note, iw);
                let cut = s.len().saturating_sub(note.len() + 1);
                let line = Line::from(vec![Span::styled(s[..cut].to_string(), style), Span::styled(s[cut..].to_string(), style.patch(dim()))]);
                f.render_widget(Paragraph::new(line), row);
                app.areas.account.rows.push((row, *item));
                continue;
            }
            Item::Shortcuts => {
                // The key faded, on the right, as a hint.
                let key = "Ctrl+/";
                let s = padded(tr("Keyboard shortcuts", "Atalhos de teclado"), key, iw);
                let cut = s.len().saturating_sub(key.len() + 1);
                let line = Line::from(vec![Span::styled(s[..cut].to_string(), style), Span::styled(s[cut..].to_string(), style.patch(dim()))]);
                f.render_widget(Paragraph::new(line), row);
                app.areas.account.rows.push((row, *item));
                continue;
            }
            Item::CheckUpdate if matches!(app.update, Some(State::Installed(_))) => {
                padded(tr("Restart to update", "Reinicie para atualizar"), "↻", iw)
            }
            Item::CheckUpdate => padded(tr("Check for updates", "Procurar atualizações"), "", iw),
            Item::About => padded(tr("About slick", "Sobre o slick"), "", iw),
            Item::Logout if confirm => {
                style = style.fg(Color::Yellow).bold();
                padded(tr("Sign out? Enter confirms", "Sair? Enter confirma"), "", iw)
            }
            _ => padded(&trf!("Sign out of {}", "Sair de {}", me.team), "", iw),
        };
        // The picture of a custom emoji goes over its blank cells, after the text.
        f.render_widget(Paragraph::new(Line::from(label).style(style)), row);
        if let (Item::Status, Some(Glyph::Image(img))) = (item, glyph(app, &emoji, &text)) {
            draw_image(f, app, &img, Rect::new(row.x + 1, row.y, 2, 1));
        }
        app.areas.account.rows.push((row, *item));
    }
    if let Some(s) = sub {
        draw_sub(f, app, rect, sub_y, sel, s);
    }
}

/// The notifications submenu, beside the menu.
fn draw_sub(f: &mut Frame, app: &mut App, main: Rect, y: u16, parent: Item, sel: Item) {
    let rows = sub_rows(app, parent);
    let area = f.area();
    let w = (rows.iter().map(|(_, l, r)| l.width() + r.width() + 5).max().unwrap_or(20) as u16).max(30).min(area.width);
    let h = (rows.len() as u16 + 2).min(area.height);
    let x = main.right().min(area.right().saturating_sub(w));
    let y = y.saturating_sub(1).min(area.bottom().saturating_sub(h));
    let rect = Rect::new(x, y, w, h);
    f.render_widget(Clear, rect);
    let block = Block::bordered().border_type(BorderType::Rounded).border_style(dim());
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    app.areas.account.sub = rect;
    let iw = inner.width as usize;
    for (n, (item, left, right)) in rows.iter().enumerate() {
        let row = Rect::new(inner.x, inner.y + n as u16, inner.width, 1);
        if row.y >= inner.bottom() {
            break;
        }
        let line = match item {
            Some(Item::Sep) => Line::from("─".repeat(iw)).style(dim()),
            None => {
                let paused = parent == Item::Notifications && app.account.paused.is_some() && n == 0;
                let style = if paused { Style::new().fg(Color::Yellow) } else { dim() };
                Line::from(padded(left, right, iw)).style(style)
            }
            Some(i) => {
                app.areas.account.rows.push((row, *i));
                let style = if *i == sel { Style::new().add_modifier(Modifier::REVERSED) } else { Style::new() };
                Line::from(padded(left, right, iw)).style(style)
            }
        };
        f.render_widget(Paragraph::new(line), row);
    }
}

fn draw_dialog(f: &mut Frame, app: &mut App) {
    let Some(d) = &app.account.dialog else { return };
    let (emoji, text, clear, field) = (d.emoji.clone(), d.text.clone(), d.clear, d.field);
    let picker = d.picker.as_ref().map(|p| (p.query.clone(), p.sel));
    let area = f.area();
    let (w, h) = (56.min(area.width.saturating_sub(4)), 18.min(area.height.saturating_sub(2)));
    let rect = Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h);
    f.render_widget(Clear, rect);
    let block = Block::bordered().border_type(BorderType::Rounded).border_style(dim()).title(Line::from(tr(" Set a status ", " Definir um status ")).bold());
    let inner = block.inner(rect).inner(Margin { horizontal: 1, vertical: 0 });
    f.render_widget(block, rect);
    app.areas.account.dialog = rect;
    if inner.width < 30 || inner.height < 15 {
        return;
    }
    let iw = inner.width as usize;
    let marker = |n: usize| if n == field { Span::styled("› ", Style::new().fg(Color::Cyan).bold()) } else { Span::raw("  ") };
    let selected = |n: usize| if n == field { Style::new().add_modifier(Modifier::REVERSED) } else { Style::new() };

    // The emoji button and the text, on one line like Slack's field.
    let y = inner.y + 1;
    let g = if emoji.is_empty() { None } else { app.emoji.glyph(&emoji) };
    let face = g.as_ref().map(shown).unwrap_or_else(|| "🙂".into());
    let button = format!(" {face}{} ", " ".repeat(2usize.saturating_sub(face.width())));
    let button_style = if field == 0 { selected(0) } else if g.is_none() { dim().bg(Color::Indexed(236)) } else { Style::new().bg(Color::Indexed(236)) };
    let room = iw.saturating_sub(2 + 4 + 1 + 1);
    let value = if text.is_empty() { Span::styled(tr("What's your status?", "O que você está fazendo?"), dim()) } else { Span::raw(tail(&text, room)) };
    let mark = if field <= 1 { Span::styled("› ", Style::new().fg(Color::Cyan).bold()) } else { Span::raw("  ") };
    f.render_widget(Paragraph::new(Line::from(vec![mark, Span::styled(button, button_style), Span::raw(" "), value])), Rect::new(inner.x, y, inner.width, 1));
    let emoji_rect = Rect::new(inner.x + 2, y, 4, 1);
    app.areas.account.emoji = emoji_rect;
    app.areas.account.text = Rect::new(inner.x + 7, y, inner.width.saturating_sub(7), 1);
    if let Some(Glyph::Image(img)) = &g {
        draw_image(f, app, img, Rect::new(inner.x + 3, y, 2, 1));
    }
    if field == 1 && picker.is_none() {
        let cx = inner.x + 7 + if text.is_empty() { 0 } else { tail(&text, room).width() as u16 };
        f.set_cursor_position((cx.min(inner.right().saturating_sub(1)), y));
    }

    // Slack's suggestions.
    f.render_widget(Paragraph::new(Line::from(tr(" Suggestions", " Sugestões")).style(dim())), Rect::new(inner.x, y + 2, inner.width, 1));
    for (i, (code, en, pt, after)) in PRESETS.iter().enumerate() {
        let label = tr(en, pt);
        let row = Rect::new(inner.x, y + 3 + i as u16, inner.width, 1);
        let e = emoji::standard(code).unwrap_or_default();
        let line = Line::from(vec![
            marker(2 + i),
            Span::raw(format!("{e} ")),
            Span::styled(label.to_string(), selected(2 + i)),
            Span::styled(format!(" — {}", clear_label(*after).to_lowercase()), dim()),
        ]);
        f.render_widget(Paragraph::new(line), row);
        app.areas.account.presets.push((row, i));
    }

    // When it clears.
    let ay = y + 4 + PRESETS.len() as u16;
    let choice = format!("‹ {} ›", clear_label(clear));
    let after = tr("Clear after  ", "Limpar após  ");
    let line = Line::from(vec![marker(AFTER), Span::styled(after, Style::new().bold()), Span::styled(choice.clone(), selected(AFTER).fg(Color::Cyan))]);
    f.render_widget(Paragraph::new(line), Rect::new(inner.x, ay, inner.width, 1));
    app.areas.account.after = Rect::new(inner.x, ay, (2 + after.width() + choice.width()) as u16, 1).intersection(inner);

    // Hint, then Cancel and Save on the right.
    let by = inner.bottom().saturating_sub(1);
    let (save, cancel) = (tr(" Save ", " Salvar "), tr(" Cancel ", " Cancelar "));
    let save_r = Rect::new(inner.right().saturating_sub(save.width() as u16), by, save.width() as u16, 1);
    let cancel_r = Rect::new(save_r.x.saturating_sub(cancel.width() as u16 + 1), by, cancel.width() as u16, 1);
    f.render_widget(Paragraph::new(Span::styled(save, Style::new().fg(Color::Black).bg(Color::Green).bold())), save_r);
    f.render_widget(Paragraph::new(Span::styled(cancel, Style::new().bg(Color::Indexed(236)))), cancel_r);
    let hint_w = cancel_r.x.saturating_sub(inner.x + 1);
    let hint = tr("Tab fields · Enter saves · Esc cancels", "Tab campos · Enter salva · Esc cancela");
    f.render_widget(Paragraph::new(truncate(hint, hint_w as usize)).style(dim()), Rect::new(inner.x, by, hint_w, 1));
    (app.areas.account.save, app.areas.account.cancel) = (save_r, cancel_r);

    if let Some((query, sel)) = picker {
        draw_picker(f, app, &query, sel, emoji_rect);
    }
}

/// The emoji picker under the emoji button, the same as for reactions.
fn draw_picker(f: &mut Frame, app: &mut App, query: &str, sel: usize, below: Rect) {
    let hits = app.emoji.search(query, emoji::ROWS);
    let area = f.area();
    let (w, h) = (34.min(area.width), (hits.len().max(1) as u16 + 4).min(area.height));
    let rect = Rect::new(below.x.min(area.right().saturating_sub(w)), (below.y + 1).min(area.bottom().saturating_sub(h)), w, h);
    f.render_widget(Clear, rect);
    let block = Block::bordered().border_type(BorderType::Rounded).border_style(dim()).title(Line::from(" Emoji ").bold());
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    app.areas.account.picker = rect;
    let search = if query.is_empty() {
        Line::from(vec![Span::raw(" 🔍 "), Span::styled(tr("Search emoji", "Procurar emoji"), dim())])
    } else {
        Line::from(vec![Span::raw(" 🔍 "), Span::raw(query.to_string()), Span::raw("▏")])
    };
    f.render_widget(Paragraph::new(search), Rect::new(inner.x, inner.y, inner.width, 1));
    f.render_widget(Paragraph::new("─".repeat(inner.width as usize)).style(dim()), Rect::new(inner.x, inner.y + 1, inner.width, 1));
    if hits.is_empty() {
        f.render_widget(Paragraph::new(tr(" no emoji", " nenhum emoji")).style(dim()), Rect::new(inner.x, inner.y + 2, inner.width, 1));
    }
    for (n, (name, glyph)) in hits.iter().enumerate() {
        let row = Rect::new(inner.x, inner.y + 2 + n as u16, inner.width, 1);
        if row.y >= inner.bottom() {
            break;
        }
        emoji_row(f, app, row, name, glyph, n == sel);
        app.areas.account.picker_rows.push((row, n));
    }
}
