//! A channel's details, opened from its name in the header (or Alt+I), as in
//! Slack: the star and the notification level on top, then "About",
//! "Members" and "Settings". Changes go straight to Slack; what it refuses
//! (no permission, a restricted workspace) shows in the status line.

use crate::app::{App, Event, Focus};
use crate::compose::{normalize, tail, NewMessage};
use crate::editor::Editor;
use crate::i18n::{self, tr};
use crate::slack::{Kind, Res, Slack};
use crate::ui::{dim, draw_avatar, draw_presence, presence_of, presence_span, truncate, wrap};
use chrono::{Local, TimeZone};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Margin, Position, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use ratatui::Frame;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::time::Instant;
use unicode_width::UnicodeWidthStr;

const LOCK: &str = "\u{f023}";
const STAR_ON: &str = "\u{f005}";
const STAR_OFF: &str = "\u{f006}";
/// The dropdown's button and the highlighted level: a quiet bar, as in
/// the sidebar, rather than inverted colors.
pub(crate) const BUTTON_BG: Color = Color::Indexed(236);
pub(crate) const HOVER_BG: Color = Color::Indexed(238);
/// nf-fa-archive, beside "Archive" and "Unarchive".
const ARCHIVE: &str = "\u{f187}";
/// nf-fa-trash, beside "Delete this channel".
const TRASH: &str = "\u{f1f8}";
// Nerd Font squares, as the thread panel's checkbox.
const BOX_OFF: &str = "\u{f096}";
const BOX_ON: &str = "\u{f14a}";
/// The notification levels' dropdown.
const LEVELS: [Notify; 3] = [Notify::All, Notify::Mentions, Notify::Mute];

#[derive(Clone, Copy, PartialEq)]
pub enum Tab {
    About,
    Members,
    Settings,
}

/// "Notify you about…": Slack's three levels for a channel.
#[derive(Clone, Copy, PartialEq)]
pub enum Notify {
    All,
    Mentions,
    Mute,
}

impl Notify {
    fn label(self) -> &'static str {
        match self {
            Notify::All => tr("All new posts", "Todas as novas publicações"),
            Notify::Mentions => tr("Just mentions", "Somente menções"),
            Notify::Mute => tr("Mute and hide", "Silenciar e ocultar"),
        }
    }

    /// Slack's bells (Nerd Font, Material outlines): ringing, plain, slashed.
    fn icon(self) -> &'static str {
        match self {
            Notify::All => "\u{f009f}",
            Notify::Mentions => "\u{f009c}",
            Notify::Mute => "\u{f10d1}",
        }
    }

    fn about(self) -> &'static str {
        match self {
            Notify::All => tr("Messages and threads you follow", "Mensagens e conversas que você segue"),
            Notify::Mentions => tr("@you, @channel, @here", "@você, @channel, @here"),
            Notify::Mute => tr("Only badge the channel when someone @mentions you", "Só destaca o canal quando alguém menciona você"),
        }
    }
}

/// What a row of a tab does when picked.
#[derive(Clone, Copy, PartialEq)]
enum Item {
    Name,
    Topic,
    Purpose,
    Leave,
    CopyId,
    Add,
    /// An index into the members shown.
    Member(usize),
    CopyNames,
    CopyEmails,
    Private,
    Archive,
    Unarchive,
    Delete,
}

/// What `conversations.info` says about the channel.
#[derive(Clone, Default)]
pub struct Info {
    name: String,
    topic: String,
    purpose: String,
    creator: String,
    created: i64,
    private: bool,
    general: bool,
    members: Option<u64>,
    /// The global desktop level, which a channel without its own follows.
    global: Option<String>,
}

pub struct Details {
    pub chan: String,
    tab: Tab,
    info: Option<Info>,
    members: Option<Vec<String>>,
    sel: usize,
    /// The members tab's search.
    filter: String,
    /// The name, topic or description being edited.
    edit: Option<(Item, Editor)>,
    /// The notification dropdown, open, with its highlighted level.
    notify: Option<usize>,
    /// Leaving, archiving or going private was picked once; again does it.
    confirm: Option<Item>,
    /// "Add people": the people picked and the search, as in a new message.
    adding: Option<NewMessage>,
    /// "Delete channel?", open over the details.
    delete: Option<Delete>,
    busy: bool,
}

/// The delete confirmation: the box that enables the button, and what
/// Slack said when it refused.
#[derive(Default)]
struct Delete {
    checked: bool,
    error: Option<String>,
}

/// A change sent to Slack.
#[derive(Clone)]
pub enum Job {
    Rename(String),
    Topic(String),
    Purpose(String),
    Leave,
    Archive,
    Delete,
    Private,
    Invite(Vec<String>),
    Notify(Notify),
}

pub enum Ev {
    Info { chan: String, res: Res<Info> },
    Members { chan: String, res: Res<Vec<String>> },
    Done { chan: String, job: Job, res: Res<()> },
    /// The members' addresses, for "Copy member email addresses".
    Emails(Res<Vec<String>>),
}

/// Where the last draw put things, for the mouse.
#[derive(Default)]
pub struct Areas {
    modal: Rect,
    close: Rect,
    star: Rect,
    notify: Rect,
    dropdown: Rect,
    levels: Vec<(Rect, usize)>,
    tabs: Vec<(Rect, Tab)>,
    rows: Vec<(Rect, Item)>,
    edits: Vec<(Rect, Item)>,
    /// "Add people": the people found and those picked.
    picks: Vec<(Rect, usize)>,
    chips: Vec<(Rect, usize)>,
    invite: Rect,
    /// "Delete channel?": the dialog, its ✕, the box, the link and the buttons.
    del: Rect,
    del_close: Rect,
    del_check: Rect,
    del_archive: Rect,
    del_cancel: Rect,
    del_ok: Rect,
}

// ---- Slack -----------------------------------------------------------------

fn info(s: &Slack, chan: &str) -> Res<Info> {
    let v = s.call("conversations.info", &[("channel", chan), ("include_num_members", "true")])?;
    let c = &v["channel"];
    let text = |v: &Value| v.as_str().unwrap_or("").to_string();
    let global = s.call("users.prefs.get", &[]).ok().and_then(|p| {
        let all: Value = serde_json::from_str(p["prefs"]["all_notifications_prefs"].as_str()?).ok()?;
        all["global"]["global_desktop"].as_str().map(String::from)
    });
    Ok(Info {
        name: text(&c["name"]),
        topic: text(&c["topic"]["value"]),
        purpose: text(&c["purpose"]["value"]),
        creator: text(&c["creator"]),
        created: c["created"].as_i64().unwrap_or(0),
        private: c["is_private"] == true,
        general: c["is_general"] == true,
        members: c["num_members"].as_u64(),
        global,
    })
}

fn members(s: &Slack, chan: &str) -> Res<Vec<String>> {
    let list = s.paginate("conversations.members", &[("channel", chan), ("limit", "200")], "members")?;
    Ok(list.iter().filter_map(|m| m.as_str().map(String::from)).collect())
}

/// The addresses of these people, in their order; only those Slack shows
/// (it takes `users:read.email`, and some workspaces hide them).
fn emails(s: &Slack, uids: &[String]) -> Res<Vec<String>> {
    let list = s.paginate("users.list", &[("limit", "200")], "members")?;
    let by_id: std::collections::HashMap<&str, &str> = list
        .iter()
        .filter_map(|u| Some((u["id"].as_str()?, u["profile"]["email"].as_str().filter(|e| !e.is_empty())?)))
        .collect();
    Ok(uids.iter().filter_map(|u| by_id.get(u.as_str()).map(|e| e.to_string())).collect())
}

/// The channel's own level in `all_notifications_prefs`, which the app reads
/// back (`events::prefs`); the older `muted_channels` list follows, where the
/// account still has it.
fn set_notify(s: &Slack, chan: &str, n: Notify) -> Res<()> {
    let v = s.call("users.prefs.get", &[])?;
    let prefs = &v["prefs"];
    let mut all: Value = serde_json::from_str(prefs["all_notifications_prefs"].as_str().unwrap_or("{}")).unwrap_or_else(|_| json!({}));
    if !all.is_object() {
        all = json!({});
    }
    if !all["channels"].is_object() {
        all["channels"] = json!({});
    }
    let c = &mut all["channels"][chan];
    if !c.is_object() {
        *c = json!({});
    }
    match n {
        Notify::All | Notify::Mentions => {
            let level = if n == Notify::All { "everything" } else { "mentions" };
            c["desktop"] = json!(level);
            c["mobile"] = json!(level);
            c["muted"] = json!(false);
        }
        Notify::Mute => c["muted"] = json!(true),
    }
    s.call("users.prefs.set", &[("name", "all_notifications_prefs"), ("value", &all.to_string())])?;
    if let Some(list) = prefs["muted_channels"].as_str() {
        let mut muted: Vec<&str> = list.split(',').filter(|c| !c.is_empty() && *c != chan).collect();
        if n == Notify::Mute {
            muted.push(chan);
        }
        let _ = s.call("users.prefs.set", &[("name", "muted_channels"), ("value", &muted.join(","))]);
    }
    Ok(())
}

fn run(s: &Slack, chan: &str, job: &Job) -> Res<()> {
    let call = |method: &str, extra: &[(&str, &str)]| {
        let mut p = vec![("channel", chan)];
        p.extend_from_slice(extra);
        s.call(method, &p).map(|_| ())
    };
    match job {
        Job::Rename(name) => call("conversations.rename", &[("name", name)]),
        Job::Topic(t) => call("conversations.setTopic", &[("topic", t)]),
        Job::Purpose(p) => s.set_purpose(chan, p),
        Job::Leave => call("conversations.leave", &[]),
        Job::Archive => call("conversations.archive", &[]),
        // The web client's own call (session tokens); `admin.conversations.delete`
        // is for Enterprise org admins only.
        Job::Delete => call("conversations.delete", &[]),
        Job::Private => call("conversations.convertToPrivate", &[]),
        Job::Invite(users) => call("conversations.invite", &[("users", &users.join(","))]),
        Job::Notify(n) => set_notify(s, chan, *n),
    }
}

/// Slack's error codes in words; anything else as it came.
fn friendly(e: &str) -> String {
    let code = e.rsplit(": ").next().unwrap_or(e);
    let text = match code {
        "not_authorized" | "restricted_action" | "user_is_restricted" | "missing_scope" | "not_allowed_token_type" | "access_denied" => {
            tr("You don't have permission to do that in this channel.", "Você não tem permissão para fazer isso neste canal.")
        }
        "name_taken" => tr("A channel with that name already exists.", "Já existe um canal com esse nome."),
        "invalid_name" | "invalid_name_specials" | "invalid_name_punctuation" | "invalid_name_required" => {
            tr("Use only lowercase letters, numbers, hyphens and underscores.", "Use só letras minúsculas, números, hífens e sublinhados.")
        }
        "invalid_name_maxlength" => tr("The name can have at most 80 characters.", "O nome pode ter no máximo 80 caracteres."),
        "too_long" => tr("That's too long for Slack.", "Ficou longo demais para o Slack."),
        "cant_leave_general" => tr("No one can leave the workspace's general channel.", "Ninguém pode sair do canal geral do workspace."),
        "cant_delete_general" => tr("The general channel can't be deleted.", "O canal geral não pode ser apagado."),
        "channel_not_found" => tr("Slack couldn't find this channel: it may be gone already.", "O Slack não encontrou este canal: talvez já tenha sido apagado."),
        "method_not_supported_for_channel_type" => tr("Slack doesn't allow that for this kind of conversation.", "O Slack não permite isso para este tipo de conversa."),
        "cant_archive_general" => tr("The general channel can't be archived.", "O canal geral não pode ser arquivado."),
        "last_member" => tr("You're the last member: archive the channel instead.", "Você é o último membro: arquive o canal."),
        "already_in_channel" => tr("They're already in the channel.", "Já estão no canal."),
        "unknown_method" | "method_deprecated" => tr("Slack doesn't offer that here.", "O Slack não oferece isso aqui."),
        _ => return e.to_string(),
    };
    text.to_string()
}

/// "March 3rd, 2021", or "3 de março de 2021".
fn date(secs: i64) -> String {
    let Some(t) = Local.timestamp_opt(secs, 0).single() else { return String::new() };
    use chrono::Datelike;
    if i18n::is_pt() {
        format!("{} de {} de {}", t.day(), i18n::month(&t), t.year())
    } else {
        format!("{} {}, {}", i18n::month(&t), i18n::ordinal(t.day()), t.year())
    }
}

/// The tabs a conversation has: a group DM only lists its people.
fn tabs(kind: Kind) -> Vec<Tab> {
    if kind == Kind::Group { vec![Tab::Members] } else { vec![Tab::About, Tab::Members, Tab::Settings] }
}

impl App {
    fn details_kind(&self) -> Option<Kind> {
        let d = self.details.as_ref()?;
        self.convs.iter().find(|c| c.id == d.chan).map(|c| c.kind)
    }

    /// The channel in the details is archived: nothing to change but that.
    fn details_archived(&self) -> bool {
        let Some(d) = self.details.as_ref() else { return false };
        self.convs.iter().find(|c| c.id == d.chan).is_some_and(|c| c.archived)
    }

    /// The conversation's details; for a DM, the person's profile, as a
    /// click on its name does.
    pub(crate) fn open_details(&mut self) {
        let Some(c) = self.current.as_ref().and_then(|id| self.convs.iter().find(|c| &c.id == id)).cloned() else { return };
        if c.kind == Kind::Direct {
            if let Some(uid) = c.user {
                self.open_profile(uid);
            }
            return;
        }
        self.menu = None;
        self.details = Some(Details {
            chan: c.id.clone(),
            tab: tabs(c.kind)[0],
            info: None,
            members: None,
            sel: 0,
            filter: String::new(),
            edit: None,
            notify: None,
            confirm: None,
            adding: None,
            delete: None,
            busy: false,
        });
        self.dirty = true;
        let chan = c.id.clone();
        self.spawn(move |s| Event::Details(Ev::Info { res: info(s, &chan), chan }));
        self.refresh_members(c.id);
    }

    fn refresh_members(&self, chan: String) {
        self.spawn(move |s| Event::Details(Ev::Members { res: members(s, &chan), chan }));
    }

    pub(crate) fn on_details(&mut self, ev: Ev) {
        self.dirty = true;
        match ev {
            Ev::Info { chan, res } => match res {
                Ok(i) => {
                    if let Some(d) = self.details.as_mut().filter(|d| d.chan == chan) {
                        d.info = Some(i);
                    }
                }
                Err(e) => self.error = Some(friendly(&e)),
            },
            Ev::Members { chan, res } => match res {
                Ok(m) => {
                    if let Some(d) = self.details.as_mut().filter(|d| d.chan == chan) {
                        if let Some(i) = d.info.as_mut() {
                            i.members = Some(m.len() as u64);
                        }
                        d.members = Some(m);
                    }
                }
                Err(e) => self.error = Some(friendly(&e)),
            },
            Ev::Emails(res) => match res {
                Ok(list) if list.is_empty() => {
                    self.error = Some(tr("Slack didn't show their email addresses.", "O Slack não mostrou os e-mails dessas pessoas.").into())
                }
                Ok(list) => {
                    self.copy(&list.join(", "));
                    self.brief_copied(trf!("{} email addresses copied", "{} e-mails copiados", list.len()));
                }
                Err(e) => self.error = Some(friendly(&e)),
            },
            Ev::Done { chan, job, res } => self.details_done(chan, job, res),
        }
    }

    fn details_done(&mut self, chan: String, job: Job, res: Res<()>) {
        if let Some(d) = self.details.as_mut().filter(|d| d.chan == chan) {
            d.busy = false;
            d.confirm = None;
        }
        if let Err(e) = res {
            // A refused delete reads in its dialog.
            match self.details.as_mut().filter(|d| d.chan == chan).and_then(|d| d.delete.as_mut()) {
                Some(del) if matches!(job, Job::Delete) => {
                    del.error = Some(friendly(&e));
                    self.status.clear();
                }
                _ => self.error = Some(friendly(&e)),
            }
            if matches!(job, Job::Notify(_)) {
                self.refresh_prefs();
            }
            return;
        }
        self.error = None;
        let info = self.details.as_mut().filter(|d| d.chan == chan).and_then(|d| d.info.as_mut());
        match job {
            Job::Rename(name) => {
                if let Some(i) = info {
                    i.name = name.clone();
                }
                if let Some(c) = self.convs.iter_mut().find(|c| c.id == chan) {
                    c.name = name;
                }
                self.resort();
                self.brief(tr("Channel renamed", "Canal renomeado").into(), 3);
            }
            Job::Topic(t) => {
                if let Some(i) = info {
                    i.topic = crate::slack::escape(&t);
                }
                self.brief(tr("Topic saved", "Tópico salvo").into(), 3);
            }
            Job::Purpose(p) => {
                if let Some(i) = info {
                    i.purpose = crate::slack::escape(&p);
                }
                self.brief(tr("Description saved", "Descrição salva").into(), 3);
            }
            Job::Private => {
                if let Some(i) = info {
                    i.private = true;
                }
                if let Some(c) = self.convs.iter_mut().find(|c| c.id == chan) {
                    c.kind = Kind::Private;
                }
                self.private.insert(chan);
                self.resort();
                self.brief(tr("The channel is now private", "O canal agora é privado").into(), 3);
            }
            Job::Invite(users) => {
                if let Some(d) = self.details.as_mut().filter(|d| d.chan == chan) {
                    d.adding = None;
                }
                self.brief(trf!("{} added to the channel", "{} adicionado(s) ao canal", users.len()), 3);
                self.refresh_members(chan);
            }
            // Already in place: the sidebar moved when it was picked.
            Job::Notify(_) => {}
            // Archived, it stays here (read-only, out of the sidebar) and
            // its settings offer "Unarchive".
            Job::Archive => {
                if let Some(c) = self.convs.iter_mut().find(|c| c.id == chan) {
                    c.archived = true;
                }
                if let Some(d) = self.details.as_mut() {
                    d.sel = 0;
                }
                self.brief(tr("Channel archived", "Canal arquivado").into(), 3);
            }
            Job::Delete => {
                self.details = None;
                self.forget_channel(&chan);
                self.brief(tr("Channel deleted", "Canal apagado").into(), 3);
            }
            Job::Leave => {
                self.details = None;
                self.convs.retain(|c| c.id != chan);
                self.recents.retain(|r| *r != chan);
                self.resort();
                if self.current.as_deref() == Some(chan.as_str()) {
                    self.current = None;
                    self.thread = None;
                    self.focus = Focus::Sidebar;
                }
                self.brief(tr("You left the channel", "Você saiu do canal").into(), 3);
            }
        }
    }

    /// A channel gone for good (deleted, here or by someone else): out of
    /// the sidebar and the state; if it was open, the default one opens.
    pub(crate) fn forget_channel(&mut self, chan: &str) {
        if self.details.as_ref().is_some_and(|d| d.chan == chan) {
            self.details = None;
        }
        self.convs.retain(|c| c.id != chan);
        self.recents.retain(|r| r != chan);
        self.messages.remove(chan);
        self.counts.remove(chan);
        self.stars.retain(|s| s != chan);
        self.resort();
        if self.current.as_deref() == Some(chan) {
            self.current = None;
            self.thread = None;
            self.focus = Focus::Sidebar;
            self.open_default();
        }
        self.dirty = true;
    }

    fn details_job(&mut self, job: Job) {
        let Some(d) = self.details.as_mut() else { return };
        if d.busy {
            return;
        }
        d.busy = true;
        let chan = d.chan.clone();
        self.status = tr("saving…", "salvando…").into();
        self.spawn(move |s| {
            let res = run(s, &chan, &job);
            Event::Details(Ev::Done { chan, job, res })
        });
    }

    /// The level shown checked: the channel's own, else the global one.
    fn notify_level(&self, chan: &str, global: Option<&str>) -> Notify {
        let p = self.pref(chan);
        if p.muted {
            return Notify::Mute;
        }
        match p.desktop.as_deref().or(global) {
            Some("mentions" | "mentions_dms" | "nothing") => Notify::Mentions,
            _ => Notify::All,
        }
    }

    /// Takes effect here at once (the sidebar files a muted channel under
    /// Silenced), then in Slack; on failure the preferences reload.
    fn set_notify(&mut self, n: Notify) {
        let Some(chan) = self.details.as_ref().map(|d| d.chan.clone()) else { return };
        let mut p = self.pref(&chan);
        match n {
            Notify::All => (p.muted, p.desktop) = (false, Some("everything".into())),
            Notify::Mentions => (p.muted, p.desktop) = (false, Some("mentions".into())),
            Notify::Mute => p.muted = true,
        }
        self.set_pref(chan.clone(), p);
        self.spawn(move |s| {
            let job = Job::Notify(n);
            let res = run(s, &chan, &job);
            Event::Details(Ev::Done { chan, job, res })
        });
    }

    fn toggle_details_star(&mut self) {
        let Some(chan) = self.details.as_ref().map(|d| d.chan.clone()) else { return };
        let on = !self.stars.contains(&chan);
        self.set_star(chan, on);
    }

    /// The members as listed: by name, or the best match to the search;
    /// deactivated accounts left out.
    fn shown_members(&self, d: &Details) -> Vec<String> {
        let Some(all) = &d.members else { return Vec::new() };
        let name = |u: &String| self.users.names.get(u).cloned().or_else(|| self.others.get(u).cloned()).unwrap_or_else(|| u.clone());
        let q = d.filter.trim();
        let mut hits: Vec<(u8, String, &String)> = all
            .iter()
            .filter(|u| !self.users.deleted.contains(*u))
            .filter_map(|u| {
                let n = name(u);
                let rank = if q.is_empty() {
                    0
                } else {
                    let real = self.users.real.get(u).map(String::as_str).unwrap_or("");
                    [n.as_str(), real].iter().filter_map(|s| crate::text::search(s, q)).map(|(r, _)| r).min()?
                };
                Some((rank, n.to_lowercase(), u))
            })
            .collect();
        hits.sort();
        hits.into_iter().map(|(_, _, u)| u.clone()).collect()
    }

    fn details_items(&self, d: &Details) -> Vec<Item> {
        let kind = self.details_kind().unwrap_or(Kind::Channel);
        let info = d.info.clone().unwrap_or_default();
        let archived = self.details_archived();
        match d.tab {
            Tab::About => {
                let mut v = vec![Item::Name, Item::Topic, Item::Purpose];
                if !info.general && !archived {
                    v.push(Item::Leave);
                }
                v.push(Item::CopyId);
                v
            }
            Tab::Members => {
                let mut v = Vec::new();
                if kind != Kind::Group && !archived {
                    v.push(Item::Add);
                }
                v.extend((0..self.shown_members(d).len()).map(Item::Member));
                v
            }
            Tab::Settings => {
                let mut v = vec![Item::CopyNames, Item::CopyEmails];
                // Going public again is for admins only: there's no call for it.
                if kind == Kind::Channel && !info.private && !info.general {
                    v.push(Item::Private);
                }
                if archived {
                    v.push(Item::Unarchive);
                } else if !info.general {
                    v.push(Item::Archive);
                }
                // Slack doesn't say who may delete (admins, by default):
                // offered to all, its refusal explains.
                if !info.general && matches!(kind, Kind::Channel | Kind::Private) {
                    v.push(Item::Delete);
                }
                v
            }
        }
    }

    fn set_details_tab(&mut self, tab: Tab) {
        let kind = self.details_kind().unwrap_or(Kind::Channel);
        let Some(d) = self.details.as_mut() else { return };
        if tabs(kind).contains(&tab) {
            (d.tab, d.sel, d.confirm, d.edit, d.adding) = (tab, 0, None, None, None);
        }
    }

    fn step_details_tab(&mut self, delta: isize) {
        let kind = self.details_kind().unwrap_or(Kind::Channel);
        let Some(d) = self.details.as_ref() else { return };
        let all = tabs(kind);
        let i = all.iter().position(|t| *t == d.tab).unwrap_or(0) as isize;
        let tab = all[(i + delta).rem_euclid(all.len() as isize) as usize];
        self.set_details_tab(tab);
    }

    fn activate_detail(&mut self, item: Item) {
        let names = self.names();
        let Some(d) = self.details.as_ref() else { return };
        let info = d.info.clone().unwrap_or_default();
        let shown = self.shown_members(d);
        let confirmed = d.confirm == Some(item);
        let chan = d.chan.clone();
        let edit = |text: String| {
            let mut e = Editor::default();
            e.set(text);
            e
        };
        let start = match item {
            _ if self.details_archived() => None,
            Item::Name => Some(edit(info.name.clone())),
            Item::Topic => Some(edit(crate::text::mrkdwn(&info.topic, &names))),
            Item::Purpose => Some(edit(crate::text::mrkdwn(&info.purpose, &names))),
            _ => None,
        };
        let Some(d) = self.details.as_mut() else { return };
        if let Some(e) = start {
            // Nothing to edit before Slack says what's there.
            if d.info.is_some() {
                (d.edit, d.confirm) = (Some((item, e)), None);
            }
            return;
        }
        match item {
            Item::Leave | Item::Archive | Item::Private if !confirmed => d.confirm = Some(item),
            Item::Leave => self.details_job(Job::Leave),
            Item::Archive => self.details_job(Job::Archive),
            Item::Private => self.details_job(Job::Private),
            Item::Unarchive => self.unarchive(chan),
            Item::Delete => (d.delete, d.confirm) = (Some(Delete::default()), None),
            Item::CopyId => {
                self.copy(&chan);
                self.brief_copied(trf!("ID {chan} copied", "ID {chan} copiado"));
                self.copied = Some(("chan", Instant::now()));
            }
            Item::Add => d.adding = Some(NewMessage::default()),
            Item::Member(i) => {
                if let Some(uid) = shown.get(i).cloned() {
                    self.details = None;
                    self.open_profile(uid);
                }
            }
            Item::CopyNames => {
                let Some(all) = d.members.clone() else { return };
                let names: Vec<String> = all
                    .iter()
                    .filter(|u| !self.users.deleted.contains(*u))
                    .map(|u| self.users.names.get(u).cloned().unwrap_or_else(|| u.clone()))
                    .collect();
                self.copy(&names.join(", "));
                self.brief_copied(trf!("{} names copied", "{} nomes copiados", names.len()));
            }
            Item::CopyEmails => {
                let Some(all) = d.members.clone() else { return };
                self.status = tr("looking up email addresses…", "buscando os e-mails…").into();
                self.spawn(move |s| Event::Details(Ev::Emails(emails(s, &all))));
            }
            Item::Name | Item::Topic | Item::Purpose => {}
        }
    }

    fn submit_detail_edit(&mut self) {
        let Some(d) = self.details.as_mut() else { return };
        let Some((item, e)) = d.edit.take() else { return };
        let text = e.text.trim().to_string();
        let job = match item {
            Item::Name => {
                let name = normalize(&text);
                if name.is_empty() {
                    d.edit = Some((item, e));
                    self.error = Some(tr("Give the channel a name.", "Dê um nome ao canal.").into());
                    return;
                }
                if d.info.as_ref().is_some_and(|i| i.name == name) {
                    return;
                }
                Job::Rename(name)
            }
            Item::Topic => Job::Topic(text),
            _ => Job::Purpose(text),
        };
        self.details_job(job);
    }

    fn submit_invite(&mut self) {
        let Some(users) = self.details.as_ref().and_then(|d| d.adding.as_ref()).map(|a| a.to.clone()) else { return };
        if users.is_empty() {
            self.error = Some(tr("Pick at least one person.", "Escolha pelo menos uma pessoa.").into());
            return;
        }
        self.details_job(Job::Invite(users));
    }

    /// People to add: anyone not in the channel yet.
    fn invite_candidates(&self) -> Vec<String> {
        let Some(d) = self.details.as_ref() else { return Vec::new() };
        let Some(a) = &d.adding else { return Vec::new() };
        let mut search = NewMessage::default();
        (search.query, search.to) = (a.query.clone(), a.to.clone());
        let inside: HashSet<&String> = d.members.iter().flatten().collect();
        self.candidates(&search).into_iter().filter(|u| !inside.contains(u)).collect()
    }

    pub(crate) fn details_paste(&mut self, s: &str) {
        let line = s.lines().next().unwrap_or("").trim();
        let Some(d) = self.details.as_mut() else { return };
        if let Some((_, e)) = d.edit.as_mut() {
            e.insert(line);
        } else if let Some(a) = d.adding.as_mut() {
            a.query.push_str(line);
            a.sel = 0;
        } else if d.tab == Tab::Members {
            d.filter.push_str(line);
            d.sel = 0;
        }
    }

    pub(crate) fn details_key(&mut self, k: KeyEvent) {
        self.dirty = true;
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let (group, archived) = (self.details_kind() == Some(Kind::Group), self.details_archived());
        let Some(d) = self.details.as_mut() else { return };
        if let Some(del) = d.delete.as_mut() {
            match k.code {
                _ if d.busy => {}
                KeyCode::Esc | KeyCode::Char('q') => d.delete = None,
                KeyCode::Char(' ') => (del.checked, del.error) = (!del.checked, None),
                KeyCode::Char('a') if !archived => self.archive_instead(),
                KeyCode::Enter if del.checked => self.details_job(Job::Delete),
                _ => {}
            }
            return;
        }
        if let Some(i) = d.notify {
            match k.code {
                KeyCode::Up | KeyCode::Char('k') => d.notify = Some(i.saturating_sub(1)),
                KeyCode::Down | KeyCode::Char('j') => d.notify = Some((i + 1).min(LEVELS.len() - 1)),
                KeyCode::Enter | KeyCode::Char(' ') => {
                    d.notify = None;
                    self.set_notify(LEVELS[i]);
                }
                KeyCode::Esc | KeyCode::Char('q') => d.notify = None,
                _ => {}
            }
            return;
        }
        if d.edit.is_some() {
            return self.detail_edit_key(k, ctrl);
        }
        if d.adding.is_some() {
            return self.invite_key(k, ctrl);
        }
        if alt {
            match k.code {
                KeyCode::Char('s') => self.toggle_details_star(),
                KeyCode::Char('n') if !group && !archived => {
                    let chan = d.chan.clone();
                    let global = d.info.as_ref().and_then(|i| i.global.clone());
                    let level = self.notify_level(&chan, global.as_deref());
                    if let Some(d) = self.details.as_mut() {
                        d.notify = LEVELS.iter().position(|l| *l == level);
                    }
                }
                KeyCode::Char('1') => self.set_details_tab(Tab::About),
                KeyCode::Char('2') => self.set_details_tab(Tab::Members),
                KeyCode::Char('3') => self.set_details_tab(Tab::Settings),
                _ => {}
            }
            return;
        }
        let Some(d) = self.details.as_ref() else { return };
        let items = self.details_items(d);
        let (tab, sel, filtering) = (d.tab, d.sel.min(items.len().saturating_sub(1)), !d.filter.is_empty());
        let Some(d) = self.details.as_mut() else { return };
        let all = items.len() as isize;
        let delta = match k.code {
            KeyCode::Up => Some(-1),
            KeyCode::Down => Some(1),
            KeyCode::PageUp => Some(-10),
            KeyCode::PageDown => Some(10),
            KeyCode::Home if tab != Tab::Members => Some(-all),
            KeyCode::End if tab != Tab::Members => Some(all),
            KeyCode::Char('k') if tab != Tab::Members => Some(-1),
            KeyCode::Char('j') if tab != Tab::Members => Some(1),
            _ => None,
        };
        if let Some(delta) = delta {
            d.sel = sel.saturating_add_signed(delta).min(items.len().saturating_sub(1));
            d.confirm = None;
            return;
        }
        match k.code {
            KeyCode::Esc if d.confirm.is_some() => d.confirm = None,
            KeyCode::Esc if filtering && tab == Tab::Members => (d.filter, d.sel) = (String::new(), 0),
            KeyCode::Esc => self.details = None,
            KeyCode::Tab | KeyCode::Right => self.step_details_tab(1),
            KeyCode::BackTab | KeyCode::Left => self.step_details_tab(-1),
            KeyCode::Enter => {
                if let Some(&item) = items.get(sel) {
                    self.activate_detail(item);
                }
            }
            // In Members, typing searches; the first match gets the cursor.
            KeyCode::Backspace if tab == Tab::Members => {
                d.filter.pop();
                d.sel = 0;
            }
            KeyCode::Char('u') if ctrl && tab == Tab::Members => (d.filter, d.sel) = (String::new(), 0),
            KeyCode::Char(c) if tab == Tab::Members && !ctrl => {
                d.filter.push(c);
                d.sel = usize::from(items.first() == Some(&Item::Add));
            }
            KeyCode::Char('q') => self.details = None,
            _ => {}
        }
    }

    /// The dialog's "archive a channel instead": closes it and starts the
    /// archive confirmation in its place.
    fn archive_instead(&mut self) {
        let Some(d) = self.details.as_ref() else { return };
        let at = self.details_items(d).iter().position(|i| *i == Item::Archive);
        let Some(d) = self.details.as_mut() else { return };
        d.delete = None;
        if let Some(i) = at {
            (d.sel, d.confirm) = (i, Some(Item::Archive));
        }
    }

    fn detail_edit_key(&mut self, k: KeyEvent, ctrl: bool) {
        let word = ctrl || k.modifiers.contains(KeyModifiers::ALT);
        let Some(d) = self.details.as_mut() else { return };
        if k.code == KeyCode::Enter || (ctrl && k.code == KeyCode::Char('j')) {
            return self.submit_detail_edit();
        }
        let Some((_, e)) = d.edit.as_mut() else { return };
        match k.code {
            KeyCode::Esc => d.edit = None,
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

    /// "Add people", like the new message dialog: Enter picks, Enter on an
    /// empty search (or Ctrl+Enter) adds them.
    fn invite_key(&mut self, k: KeyEvent, ctrl: bool) {
        let picks = self.invite_candidates();
        let Some(a) = self.details.as_mut().and_then(|d| d.adding.as_mut()) else { return };
        a.sel = a.sel.min(picks.len().saturating_sub(1));
        let confirm = (ctrl && matches!(k.code, KeyCode::Enter | KeyCode::Char('j'))) || (k.code == KeyCode::Enter && a.query.is_empty());
        if confirm {
            if let Some(uid) = picks.get(a.sel).filter(|_| !a.query.is_empty()) {
                a.to.push(uid.clone());
            }
            return self.submit_invite();
        }
        match k.code {
            KeyCode::Esc => {
                if let Some(d) = self.details.as_mut() {
                    d.adding = None;
                }
            }
            KeyCode::Enter => {
                if let Some(uid) = picks.get(a.sel) {
                    a.to.push(uid.clone());
                    (a.query, a.sel) = (String::new(), 0);
                }
            }
            KeyCode::Up => a.sel = a.sel.saturating_sub(1),
            KeyCode::Down => a.sel = (a.sel + 1).min(picks.len().saturating_sub(1)),
            KeyCode::Backspace if a.query.is_empty() => {
                a.to.pop();
            }
            KeyCode::Backspace => {
                a.query.pop();
                a.sel = 0;
            }
            KeyCode::Char('u') if ctrl => a.query.clear(),
            KeyCode::Char(c) if !ctrl => {
                a.query.push(c);
                a.sel = 0;
            }
            _ => {}
        }
    }

    /// Modal: every click lands here while it's open; outside closes it.
    pub(crate) fn details_mouse(&mut self, m: MouseEvent) {
        let at = Position::new(m.column, m.row);
        let left = m.kind == MouseEventKind::Down(MouseButton::Left);
        let a = &self.areas.details;
        let find = |v: &[(Rect, Item)]| v.iter().find(|(r, _)| r.contains(at)).map(|(_, i)| *i);
        let (row, edit) = (find(&a.rows), find(&a.edits));
        let level = a.levels.iter().find(|(r, _)| r.contains(at)).map(|(_, i)| *i);
        let tab = a.tabs.iter().find(|(r, _)| r.contains(at)).map(|(_, t)| *t);
        let pick = a.picks.iter().find(|(r, _)| r.contains(at)).map(|(_, i)| *i);
        let chip = a.chips.iter().find(|(r, _)| r.contains(at)).map(|(_, i)| *i);
        let (close, star, notify, invite) = (a.close.contains(at), a.star.contains(at), a.notify.contains(at), a.invite.contains(at));
        let (inside, in_dropdown) = (a.modal.contains(at), a.dropdown.contains(at));
        let (in_del, del_close, del_check) = (a.del.contains(at), a.del_close.contains(at), a.del_check.contains(at));
        let (del_archive, del_cancel, del_ok) = (a.del_archive.contains(at), a.del_cancel.contains(at), a.del_ok.contains(at));
        let picks = self.invite_candidates();
        let Some(d) = self.details.as_mut() else { return };
        self.dirty = true;
        // "Delete channel?" takes every click; outside closes it.
        if let Some(del) = d.delete.as_mut() {
            if !left || d.busy {
                return;
            }
            if !in_del || del_close || del_cancel {
                d.delete = None;
            } else if del_check {
                (del.checked, del.error) = (!del.checked, None);
            } else if del_archive {
                self.archive_instead();
            } else if del_ok && del.checked {
                self.details_job(Job::Delete);
            }
            return;
        }
        if d.notify.is_some() {
            match m.kind {
                MouseEventKind::Moved if level.is_some() => d.notify = level,
                _ if left && level.is_some() => {
                    d.notify = None;
                    self.set_notify(LEVELS[level.unwrap_or(0)]);
                }
                MouseEventKind::Down(_) if !in_dropdown => d.notify = None,
                _ => {}
            }
            return;
        }
        let items_len = {
            let Some(d) = self.details.as_ref() else { return };
            self.details_items(d).len()
        };
        let Some(d) = self.details.as_mut() else { return };
        match m.kind {
            MouseEventKind::Down(_) if !inside => self.details = None,
            MouseEventKind::ScrollUp if d.adding.is_some() => {
                if let Some(a) = d.adding.as_mut() {
                    a.sel = a.sel.saturating_sub(3);
                }
            }
            MouseEventKind::ScrollDown if d.adding.is_some() => {
                if let Some(a) = d.adding.as_mut() {
                    a.sel = (a.sel + 3).min(picks.len().saturating_sub(1));
                }
            }
            MouseEventKind::ScrollUp => d.sel = d.sel.saturating_sub(3),
            MouseEventKind::ScrollDown => d.sel = (d.sel + 3).min(items_len.saturating_sub(1)),
            MouseEventKind::Moved => {
                if let (Some(i), Some(a)) = (pick, d.adding.as_mut()) {
                    a.sel = i;
                }
            }
            _ if !left => {}
            _ if close => self.details = None,
            _ if star => self.toggle_details_star(),
            _ if notify => {
                let (chan, global) = (d.chan.clone(), d.info.as_ref().and_then(|i| i.global.clone()));
                let level = self.notify_level(&chan, global.as_deref());
                if let Some(d) = self.details.as_mut() {
                    d.notify = LEVELS.iter().position(|l| *l == level);
                }
            }
            _ if tab.is_some() => self.set_details_tab(tab.unwrap_or(Tab::About)),
            _ if d.adding.is_some() => {
                if invite {
                    self.submit_invite();
                } else if let (Some(uid), Some(a)) = (pick.and_then(|i| picks.get(i)), d.adding.as_mut()) {
                    a.to.push(uid.clone());
                    (a.query, a.sel) = (String::new(), 0);
                } else if let (Some(i), Some(a)) = (chip, d.adding.as_mut()) {
                    a.to.remove(i);
                }
            }
            _ if d.edit.is_some() => {}
            _ if edit.is_some() => self.activate_detail(edit.unwrap_or(Item::Name)),
            _ => {
                let Some(item) = row else { return };
                let Some(d) = self.details.as_ref() else { return };
                if let Some(i) = self.details_items(d).iter().position(|x| *x == item) {
                    if let Some(d) = self.details.as_mut() {
                        if d.confirm != Some(item) {
                            d.confirm = None;
                        }
                        d.sel = i;
                    }
                }
                // Name, topic and description edit from their "Edit".
                if !matches!(item, Item::Name | Item::Topic | Item::Purpose) {
                    self.activate_detail(item);
                }
            }
        }
    }
}

// ---- drawing -------------------------------------------------------------

/// A row of a tab: what it does when picked, and whether it starts the item
/// (where the marker and "Edit" go).
struct Row {
    item: Option<Item>,
    head: bool,
    line: Line<'static>,
    /// An editing field in place of the line.
    field: bool,
}

fn row(item: Option<Item>, head: bool, line: Line<'static>) -> Row {
    Row { item, head, line, field: false }
}

pub fn draw(f: &mut Frame, app: &mut App) {
    app.areas.details = Areas::default();
    let Some(d) = app.details.as_ref() else { return };
    let Some(c) = app.convs.iter().find(|c| c.id == d.chan).cloned() else { return };
    let info = d.info.clone();
    let (tab, notify, busy) = (d.tab, d.notify, d.busy);
    let area = f.area();
    let w = 68.min(area.width.saturating_sub(4));
    let h = 32.min(area.height.saturating_sub(2));
    let rect = Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h);
    f.render_widget(Clear, rect);
    let icon = match c.kind {
        Kind::Private => format!("{LOCK} "),
        Kind::Group => "◉ ".into(),
        _ => "# ".into(),
    };
    let name = info.as_ref().map(|i| i.name.clone()).filter(|n| !n.is_empty() && c.kind != Kind::Group).unwrap_or_else(|| app.label(&c));
    let title = Line::from(vec![Span::raw(" "), Span::styled(icon, dim()), Span::raw(truncate(&name, w.saturating_sub(10) as usize)).bold(), Span::raw(" ")]);
    let block = Block::bordered().border_type(BorderType::Rounded).border_style(dim()).title(title);
    let inner = block.inner(rect).inner(Margin { horizontal: 1, vertical: 0 });
    f.render_widget(block, rect);
    app.areas.details.modal = rect;
    let close = Rect::new(rect.right().saturating_sub(5), rect.y, 3, 1);
    f.render_widget(Paragraph::new(" ✕ "), close);
    app.areas.details.close = close;
    if inner.width < 30 || inner.height < 12 {
        return;
    }

    // The star and the notification level.
    let starred = app.stars.contains(&c.id);
    let star = if starred { Span::styled(format!(" {STAR_ON} "), Style::new().fg(Color::Yellow)) } else { Span::styled(format!(" {STAR_OFF} "), dim()) };
    let star_r = Rect::new(inner.x, inner.y, 3, 1);
    f.render_widget(Paragraph::new(star), star_r);
    app.areas.details.star = star_r;
    let level = app.notify_level(&c.id, info.as_ref().and_then(|i| i.global.as_deref()));
    // An archived channel notifies of nothing: no level to pick.
    if c.kind != Kind::Group && !c.archived {
        // A rounded button, like "Cancel" in the profile dialog; lit while open.
        let (bg, fg) = if notify.is_some() { (HOVER_BG, Color::Cyan) } else { (BUTTON_BG, Color::Reset) };
        let face = Style::new().bg(bg).fg(fg);
        let label = format!(" {}  {} ▾ ", level.icon(), level.label());
        let button = Line::from(vec![
            Span::styled(crate::blocks::CAP_L, Style::new().fg(bg)),
            Span::styled(label, face.bold()),
            Span::styled(crate::blocks::CAP_R, Style::new().fg(bg)),
        ]);
        let r = Rect::new(inner.x + 5, inner.y, (button.width() as u16).min(inner.width.saturating_sub(5)), 1);
        f.render_widget(Paragraph::new(button), r);
        app.areas.details.notify = r;
    }

    // The tabs, the active one underlined on the line below.
    let n = info.as_ref().and_then(|i| i.members).or(d.members.as_ref().map(|m| m.len() as u64));
    let (row_y, line_y) = (inner.y + 2, inner.y + 3);
    f.render_widget(Paragraph::new("─".repeat(inner.width as usize)).style(dim()), Rect::new(inner.x, line_y, inner.width, 1));
    let mut x = inner.x;
    for t in tabs(c.kind) {
        let label = match t {
            Tab::About => tr("About", "Sobre").to_string(),
            Tab::Members => match n {
                Some(n) => trf!("Members {n}", "Membros {n}"),
                None => tr("Members", "Membros").to_string(),
            },
            Tab::Settings => tr("Settings", "Configurações").to_string(),
        };
        let tw = label.width() as u16 + 2;
        let r = Rect::new(x, row_y, tw, 1).intersection(inner);
        let active = t == tab;
        f.render_widget(Paragraph::new(format!(" {label} ")).style(if active { Style::new().bold() } else { dim() }), r);
        if active {
            f.render_widget(Paragraph::new("━".repeat(tw as usize)), Rect::new(x, line_y, tw, 1).intersection(inner));
        }
        app.areas.details.tabs.push((r, t));
        x += tw + 1;
    }

    let body = Rect::new(inner.x, line_y + 2, inner.width, inner.bottom().saturating_sub(line_y + 4));
    match tab {
        Tab::About => about(f, app, body),
        Tab::Members => members_tab(f, app, body),
        Tab::Settings => settings(f, app, body),
    }

    let hint = if busy {
        tr("saving…", "salvando…")
    } else if notify.is_some() {
        tr("↑↓ move · Enter picks · Esc closes", "↑↓ move · Enter escolhe · Esc fecha")
    } else if app.details.as_ref().is_some_and(|d| d.edit.is_some()) {
        tr("Enter saves · Esc cancels", "Enter salva · Esc cancela")
    } else if app.details.as_ref().is_some_and(|d| d.adding.is_some()) {
        tr("Enter picks · Enter on empty adds · Esc back", "Enter escolhe · Enter vazio adiciona · Esc volta")
    } else {
        tr("Tab tabs · ↑↓ move · Enter picks · Alt+S star · Alt+N notifications · Esc closes", "Tab abas · ↑↓ move · Enter escolhe · Alt+S favorito · Alt+N notificações · Esc fecha")
    };
    f.render_widget(Paragraph::new(truncate(hint, inner.width as usize)).style(dim()), Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1));

    if let Some(sel) = notify {
        dropdown(f, app, level, sel);
    }
    if app.details.as_ref().is_some_and(|d| d.delete.is_some()) {
        delete_dialog(f, app);
    }
}

/// A rounded button, like the other dialogs'.
fn pill(label: &str, style: Style) -> Line<'static> {
    let cap = Style::new().fg(style.bg.unwrap_or(BUTTON_BG));
    Line::from(vec![Span::styled(crate::blocks::CAP_L, cap), Span::styled(label.to_string(), style), Span::styled(crate::blocks::CAP_R, cap)])
}

/// "Delete channel?", as Slack asks it: what goes, what stays, and a box
/// to check before the red button wakes up.
fn delete_dialog(f: &mut Frame, app: &mut App) {
    let Some(d) = app.details.as_ref() else { return };
    let Some(del) = d.delete.as_ref() else { return };
    let (checked, error, busy) = (del.checked, del.error.clone(), d.busy);
    let archived = app.details_archived();
    let area = f.area();
    let w = 74.min(area.width.saturating_sub(4));
    // Inside the frame and two columns of margin on each side.
    let tw = w.saturating_sub(6) as usize;

    // The warning, its last sentence in bold.
    let plain = tr(
        "When you delete a channel, all messages from this channel will be removed from Slack immediately.",
        "Ao apagar um canal, todas as mensagens dele serão removidas do Slack imediatamente.",
    );
    let strong = tr("This can't be undone.", "Isso não pode ser desfeito.");
    let lead = plain.split(' ').count();
    let mut lines: Vec<Line> = Vec::new();
    let mut word = 0;
    for l in wrap(&format!("{plain} {strong}"), tw) {
        let words: Vec<&str> = l.split(' ').collect();
        let spans: Vec<Span> = words
            .iter()
            .enumerate()
            .map(|(i, wd)| {
                let text = if i == 0 { wd.to_string() } else { format!(" {wd}") };
                Span::styled(text, if word + i >= lead { Style::new().bold() } else { Style::new() })
            })
            .collect();
        word += words.len();
        lines.push(Line::from(spans));
    }
    lines.push(Line::default());
    lines.push(Line::from(tr("Keep in mind that:", "Tenha em mente que:")));
    let files = tr("Any files uploaded to this channel won't be removed", "Os arquivos enviados para este canal não serão removidos");
    for (i, l) in wrap(files, tw.saturating_sub(3)).into_iter().enumerate() {
        lines.push(Line::from(format!("{}{l}", if i == 0 { " • " } else { "   " })));
    }
    // "archive a channel instead", a link to the archive confirmation.
    let mut link = None;
    if !archived {
        let (before, label, after) = if i18n::is_pt() {
            ("Você pode ", "arquivar o canal", " em vez disso, sem remover as mensagens")
        } else {
            ("You can ", "archive a channel instead", " without removing its messages")
        };
        link = Some((lines.len(), 3 + before.width() as u16, label.width() as u16));
        lines.push(Line::from(vec![
            Span::raw(format!(" • {before}")),
            Span::styled(label, Style::new().fg(Color::Cyan).underlined()),
            Span::raw(after),
        ]));
    }
    if let Some(e) = &error {
        lines.push(Line::default());
        lines.extend(wrap(e, tw).into_iter().map(|l| Line::from(Span::styled(l, Style::new().fg(Color::Red)))));
    }

    // The text, a blank, the box and the buttons, a blank, the hint.
    let n = lines.len() as u16;
    let h = (n + 7).min(area.height);
    let rect = Rect::new(area.x + (area.width - w) / 2, area.y + area.height.saturating_sub(h) / 2, w, h);
    f.render_widget(Clear, rect);
    let title = Line::from(vec![Span::raw(" "), Span::raw(tr("Delete channel?", "Apagar canal?")).bold(), Span::raw(" ")]);
    let block = Block::bordered().border_type(BorderType::Rounded).border_style(dim()).title(title);
    let inner = block.inner(rect).inner(Margin { horizontal: 2, vertical: 0 });
    f.render_widget(block, rect);
    let close = Rect::new(rect.right().saturating_sub(5), rect.y, 3, 1);
    f.render_widget(Paragraph::new(" ✕ "), close);
    let a = &mut app.areas.details;
    (a.del, a.del_close) = (rect, close);
    if inner.height < n + 5 {
        return;
    }
    let text = Rect::new(inner.x, inner.y + 1, inner.width, n);
    f.render_widget(Paragraph::new(lines), text);
    if let Some((row, x, lw)) = link {
        a.del_archive = Rect::new(text.x + x, text.y + row as u16, lw, 1).intersection(text);
    }

    // "Cancel" and "Delete Channel" on the right, the box on the left.
    let y = text.bottom() + 1;
    let ok_label = if busy { tr(" Deleting… ", " Apagando… ") } else { tr(" Delete Channel ", " Apagar canal ") };
    let ok_style = if checked && !busy { Style::new().fg(Color::White).bg(Color::Red).bold() } else { Style::new().fg(Color::DarkGray).bg(BUTTON_BG) };
    let ok = pill(ok_label, ok_style);
    let cancel = pill(tr(" Cancel ", " Cancelar "), Style::new().bg(BUTTON_BG).bold());
    let (ow, cw) = (ok.width() as u16, cancel.width() as u16);
    let ok_r = Rect::new(inner.right().saturating_sub(ow), y, ow, 1).intersection(inner);
    let cancel_r = Rect::new(ok_r.x.saturating_sub(cw + 1), y, cw, 1).intersection(inner);
    f.render_widget(Paragraph::new(ok), ok_r);
    f.render_widget(Paragraph::new(cancel), cancel_r);
    (a.del_ok, a.del_cancel) = (ok_r, cancel_r);
    let (icon, style) = if checked { (BOX_ON, Style::new().fg(Color::Cyan)) } else { (BOX_OFF, dim()) };
    let room = cancel_r.x.saturating_sub(inner.x + 3) as usize;
    let label = truncate(tr("Yes, permanently delete the channel", "Sim, apagar o canal para sempre"), room);
    let check = Line::from(vec![Span::styled(icon, style), Span::raw(" "), Span::raw(label)]);
    let check_r = Rect::new(inner.x, y, check.width() as u16, 1).intersection(inner);
    f.render_widget(Paragraph::new(check), check_r);
    a.del_check = check_r;

    let hint = if busy {
        tr("deleting…", "apagando…")
    } else if archived {
        tr("Space checks · Enter deletes · Esc cancels", "Espaço marca · Enter apaga · Esc cancela")
    } else {
        tr("Space checks · Enter deletes · A archives instead · Esc cancels", "Espaço marca · Enter apaga · A arquiva · Esc cancela")
    };
    f.render_widget(Paragraph::new(truncate(hint, inner.width as usize)).style(dim()), Rect::new(inner.x, y + 2, inner.width, 1).intersection(inner));
}

/// "Notify you about…", under its button, as Slack lays it out: a check
/// column, the level's bell, its name and, under the name, what it means.
/// The current level in the accent color, the highlighted one on a bar.
fn dropdown(f: &mut Frame, app: &mut App, level: Notify, sel: usize) {
    // " ✓ 󰂟  " before the name; the two spaces after the icon leave room
    // for fonts that draw Material glyphs a bit wider than a cell.
    const INDENT: usize = 6;
    let below = app.areas.details.notify;
    let area = f.area();
    let header = tr("Notify you about…", "Notificar você sobre…");
    let text_w = LEVELS.iter().map(|l| l.about().width().max(l.label().width())).max().unwrap_or(30).max(header.width());
    let w = (text_w + INDENT + 4) as u16;
    // Header, a gap, then two lines per level with a blank between them.
    let h = 2 + 2 + LEVELS.len() as u16 * 3 - 1;
    let (w, h) = (w.min(area.width), h.min(area.height));
    let x = below.x.min(area.right().saturating_sub(w));
    let y = below.bottom().min(area.bottom().saturating_sub(h));
    let rect = Rect::new(x, y, w, h);
    f.render_widget(Clear, rect);
    let block = Block::bordered().border_type(BorderType::Rounded).border_style(dim());
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    app.areas.details.dropdown = rect;
    if inner.height == 0 {
        return;
    }
    f.render_widget(Paragraph::new(format!(" {header}")).style(dim()), Rect::new(inner.x, inner.y, inner.width, 1));
    let room = (inner.width as usize).saturating_sub(INDENT + 1);
    for (i, l) in LEVELS.iter().enumerate() {
        let r = Rect::new(inner.x, inner.y + 2 + i as u16 * 3, inner.width, 2).intersection(inner);
        if r.is_empty() {
            break;
        }
        let current = *l == level;
        let accent = Style::new().fg(Color::Cyan);
        let (title, about) = if current { (accent.bold(), accent) } else { (Style::new().bold(), dim()) };
        // DarkGray barely shows on the bar: a lighter gray there.
        let about = if i == sel && !current { Style::new().fg(Color::Gray) } else { about };
        let check = if current { Span::styled("✓", accent.bold()) } else { Span::raw(" ") };
        let lines = vec![
            Line::from(vec![
                Span::raw(" "),
                check,
                Span::raw(" "),
                Span::styled(l.icon(), if current { accent } else { Style::new() }),
                Span::raw("  "),
                Span::styled(truncate(l.label(), room), title),
            ]),
            Line::from(vec![Span::raw(" ".repeat(INDENT)), Span::styled(truncate(l.about(), room), about)]),
        ];
        let bar = if i == sel { Style::new().bg(HOVER_BG) } else { Style::new() };
        f.render_widget(Paragraph::new(lines).style(bar), r);
        app.areas.details.levels.push((r, i));
    }
}

/// Lines a tab lists, the selected item marked, clickable rows recorded.
fn render_rows(f: &mut Frame, app: &mut App, area: Rect, rows: Vec<Row>, offset: usize) {
    let Some(d) = app.details.as_ref() else { return };
    let items = app.details_items(d);
    let sel = items.get(d.sel.min(items.len().saturating_sub(1))).copied();
    let editing = d.edit.as_ref().map(|(i, e)| (*i, e.clone()));
    let archived = app.details_archived();
    let editable = |i: Option<Item>| !archived && matches!(i, Some(Item::Name | Item::Topic | Item::Purpose));
    for (n, r) in rows.into_iter().skip(offset).enumerate() {
        let y = area.y + n as u16;
        if y >= area.bottom() {
            break;
        }
        let rect = Rect::new(area.x, y, area.width, 1);
        let picked = r.item.is_some() && r.item == sel && editing.is_none();
        let mark = if picked && r.head { Span::styled("› ", Style::new().fg(Color::Cyan).bold()) } else { Span::raw("  ") };
        if r.field {
            let Some((_, e)) = &editing else { continue };
            let fw = area.width.saturating_sub(2);
            let (_, shown, cx) = crate::account::window(e, fw.saturating_sub(2) as usize);
            let pad = (fw as usize).saturating_sub(shown.width() + 1);
            let line = Line::from(vec![Span::raw("  "), Span::styled(format!(" {shown}{}", " ".repeat(pad)), Style::new().bg(Color::Indexed(236)))]);
            f.render_widget(Paragraph::new(line), rect);
            f.set_cursor_position(((area.x + 3 + cx as u16).min(area.right().saturating_sub(1)), y));
            continue;
        }
        let mut spans = vec![mark];
        spans.extend(r.line.spans);
        f.render_widget(Paragraph::new(Line::from(spans)), rect);
        if let Some(item) = r.item {
            app.areas.details.rows.push((rect, item));
        }
        if r.head && editable(r.item) && editing.is_none() {
            let label = tr("Edit", "Editar");
            let er = Rect::new(area.right().saturating_sub(label.width() as u16), y, label.width() as u16, 1);
            f.render_widget(Paragraph::new(label).style(Style::new().fg(Color::Cyan)), er);
            if let Some(i) = r.item {
                app.areas.details.edits.push((er, i));
            }
        }
    }
}

fn about(f: &mut Frame, app: &mut App, area: Rect) {
    let Some(d) = app.details.as_ref() else { return };
    let Some(info) = d.info.clone() else {
        return f.render_widget(Paragraph::new(tr("  loading…", "  carregando…")).style(dim()), area);
    };
    let (chan, confirm) = (d.chan.clone(), d.confirm);
    let editing = d.edit.as_ref().map(|(i, _)| *i);
    let names = app.names();
    let width = area.width.saturating_sub(4) as usize;
    let mut rows = Vec::new();
    let bold = |s: &str| Span::styled(s.to_string(), Style::new().bold());
    // A label, then its value (or the editing field) at most three lines.
    let field = |rows: &mut Vec<Row>, item: Item, label: &str, value: String, empty: &str| {
        rows.push(row(Some(item), true, Line::from(bold(label))));
        if editing == Some(item) {
            rows.push(Row { item: Some(item), head: false, line: Line::default(), field: true });
        } else if value.trim().is_empty() {
            rows.push(row(Some(item), false, Line::from(Span::styled(empty.to_string(), dim()))));
        } else {
            let lines = wrap(&value, width);
            let more = lines.len() > 3;
            for (n, l) in lines.into_iter().take(3).enumerate() {
                let l = if more && n == 2 { format!("{}…", truncate(&l, width.saturating_sub(1))) } else { l };
                rows.push(row(Some(item), false, Line::from(l)));
            }
        }
        rows.push(row(None, false, Line::default()));
    };
    let sign = if info.private { format!("{LOCK} ") } else { "# ".into() };
    field(&mut rows, Item::Name, tr("Channel name", "Nome do canal"), format!("{sign}{}", info.name), "");
    field(&mut rows, Item::Topic, tr("Topic", "Tópico"), crate::text::mrkdwn(&info.topic, &names), tr("Add a topic", "Adicionar um tópico"));
    field(&mut rows, Item::Purpose, tr("Description", "Descrição"), crate::text::mrkdwn(&info.purpose, &names), tr("Add a description", "Adicionar uma descrição"));
    if !info.creator.is_empty() {
        let who = app.users.names.get(&info.creator).cloned().unwrap_or_else(|| info.creator.clone());
        rows.push(row(None, false, Line::from(bold(tr("Created by", "Criado por")))));
        rows.push(row(None, false, Line::from(trf!("{who} on {}", "{who} em {}", date(info.created)))));
        rows.push(row(None, false, Line::default()));
    }
    if !info.general && !app.details_archived() {
        let text = if confirm == Some(Item::Leave) {
            if info.private {
                tr("Leave? You'll need an invite to come back. Enter again", "Sair? Só volta com convite. Enter de novo")
            } else {
                tr("Leave the channel? Enter again to confirm", "Sair do canal? Enter de novo para confirmar")
            }
        } else {
            tr("Leave channel", "Sair do canal")
        };
        rows.push(row(Some(Item::Leave), true, Line::from(Span::styled(text, Style::new().fg(Color::Red).bold()))));
        rows.push(row(None, false, Line::default()));
    }
    let (copy, copy_style) = crate::ui::copy_hint(app, "chan");
    rows.push(row(Some(Item::CopyId), true, Line::from(vec![Span::styled(trf!("Channel ID: {chan}", "ID do canal: {chan}"), dim()), Span::styled(copy, copy_style)])));
    // The selected item kept in view.
    let items = app.details.as_ref().map(|d| app.details_items(d)).unwrap_or_default();
    let sel = app.details.as_ref().and_then(|d| items.get(d.sel.min(items.len().saturating_sub(1))).copied());
    let last = rows.iter().rposition(|r| r.item.is_some() && r.item == sel).unwrap_or(0);
    let offset = (last + 1).saturating_sub(area.height as usize);
    render_rows(f, app, area, rows, offset);
}

fn settings(f: &mut Frame, app: &mut App, area: Rect) {
    let Some(d) = app.details.as_ref() else { return };
    let confirm = d.confirm;
    let items = app.details_items(d);
    let name = d.info.as_ref().map(|i| i.name.clone()).unwrap_or_default();
    let loaded = d.members.is_some();
    let red = Style::new().fg(Color::Red).bold();
    let mut rows = Vec::new();
    for item in items {
        let line = match item {
            Item::CopyNames | Item::CopyEmails => {
                let label = if item == Item::CopyNames { tr("Copy member names", "Copiar nomes dos membros") } else { tr("Copy member email addresses", "Copiar e-mails dos membros") };
                Line::from(Span::styled(label, if loaded { Style::new() } else { dim() }))
            }
            Item::Private if confirm == Some(item) => Line::from(Span::styled(
                trf!("Make #{name} private for good? Enter again", "Tornar #{name} privado de vez? Enter de novo"),
                Style::new().fg(Color::Yellow).bold(),
            )),
            Item::Private => Line::from(tr("Change to a private channel", "Mudar para um canal privado")),
            Item::Archive if confirm == Some(item) => {
                Line::from(Span::styled(trf!("Archive #{name} for everyone? Enter again", "Arquivar #{name} para todos? Enter de novo"), red))
            }
            Item::Archive => Line::from(vec![Span::styled(format!("{ARCHIVE} "), red), Span::styled(tr("Archive channel for everyone", "Arquivar canal para todos"), red)]),
            Item::Unarchive => Line::from(vec![Span::raw(format!("{ARCHIVE} ")), Span::raw(tr("Unarchive this channel", "Desarquivar este canal"))]),
            Item::Delete => Line::from(vec![Span::styled(format!("{TRASH} "), red), Span::styled(tr("Delete this channel", "Apagar este canal"), red)]),
            _ => continue,
        };
        rows.push(row(Some(item), true, line));
        rows.push(row(None, false, Line::default()));
    }
    render_rows(f, app, area, rows, 0);
}

fn members_tab(f: &mut Frame, app: &mut App, area: Rect) {
    let Some(d) = app.details.as_ref() else { return };
    if d.adding.is_some() {
        return add_people(f, app, area);
    }
    let filter = d.filter.clone();
    let shown = app.shown_members(d);
    let loading = d.members.is_none();
    let items = app.details_items(d);
    let sel = d.sel.min(items.len().saturating_sub(1));

    // The search box, always typing.
    let fw = area.width.saturating_sub(2) as usize;
    let text = tail(&filter, fw.saturating_sub(4));
    let shown_text = if filter.is_empty() { Span::styled(tr("Find members", "Encontrar membros"), dim()) } else { Span::raw(text.clone()) };
    let pad = fw.saturating_sub(3 + shown_text.width());
    let search = Line::from(vec![
        Span::raw("  "),
        Span::styled(" ⌕ ", Style::new().bg(Color::Indexed(236)).fg(Color::DarkGray)),
        shown_text.patch_style(Style::new().bg(Color::Indexed(236))),
        Span::styled(" ".repeat(pad), Style::new().bg(Color::Indexed(236))),
    ]);
    f.render_widget(Paragraph::new(search), Rect::new(area.x, area.y, area.width, 1));
    f.set_cursor_position((area.x + 5 + text.width() as u16, area.y));

    let list = Rect::new(area.x, area.y + 2, area.width, area.height.saturating_sub(2));
    if loading {
        return f.render_widget(Paragraph::new(tr("  loading…", "  carregando…")).style(dim()), list);
    }
    let photos = app.sidebar_avatars && app.picker.is_some();
    let offset = (sel + 1).saturating_sub(list.height as usize);
    for (n, item) in items.iter().enumerate().skip(offset).take(list.height as usize) {
        let y = list.y + (n - offset) as u16;
        let rect = Rect::new(list.x, y, list.width, 1);
        let reversed = if n == sel { Style::new().add_modifier(Modifier::REVERSED) } else { Style::new() };
        let line = match item {
            Item::Add => Line::from(vec![Span::raw("  "), Span::styled(format!("＋ {}", tr("Add people", "Adicionar pessoas")), Style::new().fg(Color::Cyan).bold())]),
            Item::Member(i) => {
                let uid = &shown[*i];
                let name = app.users.names.get(uid).or_else(|| app.others.get(uid)).cloned().unwrap_or_else(|| {
                    app.unnamed.push(crate::blocks::Target::User(uid.clone()));
                    uid.clone()
                });
                let p = presence_of(app, uid);
                let dot = if photos && app.users.avatars.contains_key(uid) { Span::raw("  ") } else { presence_span(app, p) };
                // " (you)" quieter than the name, as in the sidebar.
                let you = if app.is_me(uid) { tr(" (you)", " (você)") } else { "" };
                let room = (list.width as usize).saturating_sub(6 + you.width());
                Line::from(vec![Span::raw("  "), dot, Span::raw(" "), Span::styled(truncate(&name, room), Style::new().bold()), Span::styled(you, dim())])
            }
            _ => continue,
        };
        f.render_widget(Paragraph::new(line.patch_style(reversed)), rect);
        app.areas.details.rows.push((rect, *item));
        if let Item::Member(i) = item {
            let uid = shown[*i].clone();
            let p = presence_of(app, &uid);
            match app.users.avatars.get(&uid).cloned().filter(|_| photos) {
                Some(url) => draw_avatar(f, app, &uid, &url, p, Rect::new(list.x + 2, y, 2, 1)),
                None => draw_presence(f, app, p, list.x + 2, y),
            }
        }
    }
    if shown.is_empty() && !filter.is_empty() {
        let y = list.y + items.len() as u16;
        if y < list.bottom() {
            f.render_widget(Paragraph::new(tr("  No one found", "  Ninguém encontrado")).style(dim()), Rect::new(list.x, y, list.width, 1));
        }
    }
}

/// "Add people": who is picked and the search, then the people found.
fn add_people(f: &mut Frame, app: &mut App, area: Rect) {
    let picks = app.invite_candidates();
    let Some(a) = app.details.as_ref().and_then(|d| d.adding.as_ref()) else { return };
    let (query, to, busy) = (a.query.clone(), a.to.clone(), app.details.as_ref().is_some_and(|d| d.busy));
    let sel = a.sel.min(picks.len().saturating_sub(1));
    let width = area.width as usize;
    let names = app.users.names.clone();
    let name_of = |uid: &str| names.get(uid).cloned().unwrap_or_else(|| uid.to_string());

    // The people picked as chips, then what is being typed.
    let lead = tr("Add: ", "Adicionar: ");
    let mut spans = vec![Span::styled(lead, dim())];
    let mut x = lead.width();
    let mut chips = Vec::new();
    for (i, uid) in to.iter().enumerate() {
        let label = format!(" {} × ", truncate(&name_of(uid), 24));
        if x + label.width() + 1 > width {
            spans.push(Span::styled("…", dim()));
            break;
        }
        chips.push((Rect::new(area.x + x as u16, area.y, label.width() as u16, 1), i));
        x += label.width() + 1;
        spans.push(Span::styled(label, Style::new().fg(Color::Black).bg(Color::Cyan)));
        spans.push(Span::raw(" "));
    }
    let shown = tail(&query, width.saturating_sub(x + 1));
    if query.is_empty() && to.is_empty() {
        spans.push(Span::styled(tr("type a name", "digite um nome"), dim()));
    } else {
        spans.push(Span::raw(shown.clone()));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), Rect::new(area.x, area.y, area.width, 1));
    if !busy {
        f.set_cursor_position(((area.x + (x + shown.width()) as u16).min(area.right().saturating_sub(1)), area.y));
    }
    app.areas.details.chips = chips;
    f.render_widget(Paragraph::new("─".repeat(width)).style(dim()), Rect::new(area.x, area.y + 1, area.width, 1));

    let list = Rect::new(area.x, area.y + 2, area.width, area.height.saturating_sub(4));
    if picks.is_empty() {
        f.render_widget(Paragraph::new(tr(" No one found", " Ninguém encontrado")).style(dim()), Rect::new(list.x, list.y, list.width, 1));
    }
    let offset = (sel + 1).saturating_sub(list.height as usize);
    for (n, uid) in picks.iter().enumerate().skip(offset).take(list.height as usize) {
        let y = list.y + (n - offset) as u16;
        let rect = Rect::new(list.x, y, list.width, 1);
        let dot = presence_span(app, presence_of(app, uid));
        let mut line = Line::from(vec![Span::raw(" "), dot, Span::raw(" "), Span::styled(truncate(&name_of(uid), width.saturating_sub(4)), Style::new().bold())]);
        if n == sel {
            line = line.style(Style::new().add_modifier(Modifier::REVERSED));
        }
        f.render_widget(Paragraph::new(line), rect);
        app.areas.details.picks.push((rect, n));
        let p = presence_of(app, uid);
        draw_presence(f, app, p, list.x + 1, y);
    }

    let button = if busy { tr(" Adding… ", " Adicionando… ") } else { tr(" Add ", " Adicionar ") };
    let bw = button.width() as u16;
    let ready = !to.is_empty() && !busy;
    let style = if ready { Style::new().fg(Color::Black).bg(Color::Green).bold() } else { Style::new().fg(Color::Black).bg(Color::DarkGray) };
    let b = Rect::new(area.right().saturating_sub(bw), area.bottom().saturating_sub(1), bw.min(area.width), 1);
    f.render_widget(Paragraph::new(Span::styled(button, style)), b);
    app.areas.details.invite = b;
}
