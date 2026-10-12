//! "Invite people to <workspace>", from the account menu, as in Slack's
//! modal: the addresses, an optional message and channels to join.

use crate::app::{App, Event};
use crate::blocks::{CAP_L, CAP_R};
use crate::editor::Editor;
use crate::i18n::tr;
use crate::slack::{Kind, Res, Slack};
use crate::ui::{dim, mark_selected, rounded_input, truncate, wrap, SELECTED};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Margin, Position, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use ratatui::Frame;
use serde_json::{json, Value};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Nerd Font lock: a private channel.
const LOCK: &str = "\u{f023}";
/// Text rows of the message box.
const MSG_ROWS: u16 = 3;
/// Search hits shown under the field.
const HITS: usize = 5;
/// Channels offered before searching.
const SUGGESTED: usize = 3;
/// The pills' surface.
const SHADE: Color = crate::ui::SURFACE;

/// The stops Tab goes through, in order.
#[derive(Clone, Copy, PartialEq)]
enum Field {
    Emails,
    Message,
    Suggested,
    Search,
    Send,
}

const ORDER: [Field; 5] = [Field::Emails, Field::Message, Field::Suggested, Field::Search, Field::Send];

pub struct Invite {
    emails: Editor,
    message: Editor,
    search: Editor,
    field: Field,
    /// Channels picked, by id, in the order they were added.
    chosen: Vec<String>,
    /// Channels with recent activity, offered as "+ #name".
    suggested: Vec<String>,
    /// The selected suggestion, among those not chosen yet.
    pick: usize,
    /// The selected search hit.
    hit: usize,
    /// Your own address's domain, for the placeholder.
    domain: Option<String>,
    sending: bool,
    /// What Slack refused, per address or for the whole request.
    errors: Vec<String>,
}

pub enum Ev {
    /// Your address's domain, for the placeholder.
    Domain(Option<String>),
    /// Each address and Slack's error for it, if any.
    Sent(Res<Vec<(String, Option<String>)>>),
}

/// Where the last draw put things, for the mouse.
#[derive(Default)]
pub struct Areas {
    dialog: Rect,
    close: Rect,
    /// The addresses' input, framed.
    emails: Input,
    message: Input,
    /// The message box's first row shown and its width.
    message_top: usize,
    message_w: usize,
    suggested: Vec<(Rect, String)>,
    chosen: Vec<(Rect, String)>,
    search: Input,
    hits: Vec<(Rect, usize)>,
    send: Rect,
}

/// A framed input: the whole box (what a click hits), where its text
/// starts and, for one line, the first char shown.
#[derive(Default, Clone, Copy)]
struct Input {
    frame: Rect,
    text: Rect,
    start: usize,
}

impl Input {
    fn contains(&self, at: Position) -> bool {
        self.frame.contains(at)
    }

    /// The column and row in the text under `at`, kept inside it.
    fn offset(&self, at: Position) -> (u16, u16) {
        let dx = at.x.saturating_sub(self.text.x).min(self.text.width.saturating_sub(1));
        let dy = at.y.saturating_sub(self.text.y).min(self.text.height.saturating_sub(1));
        (dx, dy)
    }
}

// ---- Slack -----------------------------------------------------------------

/// The web client's call: `users.admin.inviteBulk`, which takes the
/// browser's session; older workspaces get one `users.admin.invite` each.
fn send(s: &Slack, emails: &[String], message: &str, channels: &[String]) -> Res<Vec<(String, Option<String>)>> {
    let invites = Value::Array(emails.iter().map(|e| json!({ "email": e, "type": "regular", "mode": "manual" })).collect()).to_string();
    let chans = channels.join(",");
    let mut p = vec![
        ("invites", invites.as_str()),
        ("source", "invite_modal"),
        ("mode", "manual"),
        ("restricted", "false"),
        ("ultra_restricted", "false"),
        ("email_password_policy_enabled", "false"),
    ];
    if !chans.is_empty() {
        p.extend([("channels", chans.as_str()), ("channel_ids", chans.as_str())]);
    }
    if !message.is_empty() {
        p.push(("custom_message", message));
    }
    match s.call("users.admin.inviteBulk", &p) {
        Ok(v) => Ok(results(&v, emails)),
        Err(e) if code(&e) == "unknown_method" => Ok(emails
            .iter()
            .map(|email| {
                let mut p = vec![("email", email.as_str())];
                if !chans.is_empty() {
                    p.push(("channels", chans.as_str()));
                }
                if !message.is_empty() {
                    p.push(("custom_message", message));
                }
                let err = s.call("users.admin.invite", &p).err().map(|e| code(&e).to_string());
                (email.clone(), err)
            })
            .collect()),
        Err(e) => Err(e),
    }
}

/// Each address with its error: Slack answers `ok` with a list that says
/// which ones failed; an address it does not mention went out.
fn results(v: &Value, emails: &[String]) -> Vec<(String, Option<String>)> {
    let listed: Vec<&Value> = ["invites", "errors", "failures"].iter().filter_map(|k| v[*k].as_array()).flatten().collect();
    emails
        .iter()
        .map(|e| {
            let err = listed
                .iter()
                .filter(|i| i["email"].as_str().is_some_and(|m| m.eq_ignore_ascii_case(e)))
                .find_map(|i| match i["error"].as_str() {
                    Some(err) => Some(err.to_string()),
                    None if i["ok"] == false => Some("unknown_error".into()),
                    None => None,
                });
            (e.clone(), err)
        })
        .collect()
}

/// `users.admin.inviteBulk: not_allowed` → `not_allowed`.
fn code(e: &str) -> &str {
    e.rsplit(": ").next().unwrap_or(e).trim()
}

/// Slack's refusals that mean you may not invite at all.
fn forbidden(code: &str) -> bool {
    matches!(
        code,
        "not_allowed" | "not_authorized" | "no_perms" | "missing_scope" | "restricted_action" | "user_is_restricted" | "ura_not_allowed"
            | "not_allowed_token_type" | "team_invites_disabled" | "invites_disabled" | "admin_approval_required"
    )
}

/// What an address's error means, after the address.
fn per_email(code: &str) -> String {
    match code {
        "already_in_team" => tr("is already in this workspace", "já está neste workspace").into(),
        "already_invited" | "already_in_team_invited_user" | "already_pending" => tr("was already invited", "já foi convidado").into(),
        "invalid_email" | "bad_email" => tr("isn't a valid email address", "não é um e-mail válido").into(),
        "sent_recently" => tr("was invited recently", "foi convidado há pouco").into(),
        "user_disabled" | "account_disabled" => tr("belongs to a deactivated account", "é de uma conta desativada").into(),
        "domain_not_allowed" | "email_domain_not_allowed" => tr("is from a domain this workspace doesn't allow", "é de um domínio que o workspace não permite").into(),
        "invite_limit_reached" => tr("hit the invitation limit", "atingiu o limite de convites").into(),
        c if forbidden(c) => tr("can't be invited: you don't have permission", "não pode ser convidado: você não tem permissão").into(),
        c => c.replace('_', " "),
    }
}

/// A refusal of the whole request, readable.
fn whole(e: &str, team: &str) -> String {
    let c = code(e);
    match c {
        _ if forbidden(c) => trf!(
            "You don't have permission to invite people to {team}. Ask a workspace admin. ({c})",
            "Você não tem permissão para convidar pessoas para {team}. Peça a um admin do workspace. ({c})"
        ),
        "invite_limit_reached" | "ratelimited" => trf!("Too many invitations for now; try again later. ({c})", "Convites demais por agora; tente mais tarde. ({c})"),
        _ => trf!("Slack didn't send the invitations ({c}).", "O Slack não enviou os convites ({c})."),
    }
}

/// The addresses typed, split at commas, semicolons and spaces, once each.
fn addresses(s: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for a in s.split(|c: char| c == ',' || c == ';' || c.is_whitespace()).map(str::trim).filter(|a| !a.is_empty()) {
        let a = a.trim_matches(|c| c == '<' || c == '>').to_string();
        if !out.iter().any(|o| o.eq_ignore_ascii_case(&a)) {
            out.push(a);
        }
    }
    out
}

/// Enough to catch a typo before asking Slack.
fn plausible(a: &str) -> bool {
    let Some((user, host)) = a.split_once('@') else { return false };
    !user.is_empty() && host.contains('.') && !host.starts_with('.') && !host.ends_with('.') && !host.contains('@')
}

/// The char under column `dx` of a one-line field showing from `start`.
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

/// Typing in a field; Enter is the caller's.
fn edit(e: &mut Editor, k: KeyEvent) {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let word = ctrl || k.modifiers.contains(KeyModifiers::ALT);
    match k.code {
        KeyCode::Left => e.left(),
        KeyCode::Right => e.right(),
        KeyCode::Home => e.home(),
        KeyCode::Char('a') if ctrl => e.home(),
        KeyCode::End => e.end(),
        KeyCode::Char('e') if ctrl => e.end(),
        KeyCode::Backspace if word => e.delete_word(),
        KeyCode::Char('w') if ctrl => e.delete_word(),
        KeyCode::Backspace => e.backspace(),
        KeyCode::Delete => e.delete(),
        KeyCode::Char('u') if ctrl => e.set(String::new()),
        KeyCode::Char(c) if !ctrl => e.insert(c.encode_utf8(&mut [0; 4])),
        _ => {}
    }
}

impl App {
    /// "#name" or the lock and the name; None for a channel no longer listed.
    fn people_chan(&self, id: &str) -> Option<(&'static str, String)> {
        let c = self.convs.iter().find(|c| c.id == id)?;
        Some((if c.kind == Kind::Private { LOCK } else { "#" }, c.name.clone()))
    }

    /// The channels you can add: yours, not archived, not picked yet.
    fn people_hits(&self) -> Vec<String> {
        let Some(d) = &self.invite else { return Vec::new() };
        let q = d.search.text.trim().trim_start_matches('#').to_lowercase();
        if q.is_empty() {
            return Vec::new();
        }
        let latest = |id: &str| self.counts.get(id).map(|c| c.latest.clone()).unwrap_or_default();
        let mut hits: Vec<(bool, std::cmp::Reverse<String>, String, String)> = self
            .convs
            .iter()
            .filter(|c| matches!(c.kind, Kind::Channel | Kind::Private) && !c.archived && !d.chosen.contains(&c.id))
            .filter_map(|c| {
                let name = c.name.to_lowercase();
                name.contains(&q).then(|| (!name.starts_with(&q), std::cmp::Reverse(latest(&c.id)), name, c.id.clone()))
            })
            .collect();
        hits.sort();
        hits.into_iter().take(HITS).map(|h| h.3).collect()
    }

    /// The suggestions still offered: those not picked yet.
    fn people_offered(&self) -> Vec<String> {
        let Some(d) = &self.invite else { return Vec::new() };
        d.suggested.iter().filter(|id| !d.chosen.contains(id)).cloned().collect()
    }

    /// Channels you opened lately, then those with the newest messages.
    fn recent_channels(&self) -> Vec<String> {
        let ok = |id: &str| self.convs.iter().any(|c| c.id == id && matches!(c.kind, Kind::Channel | Kind::Private) && !c.archived);
        let mut out: Vec<String> = self.recents.iter().filter(|id| ok(id)).take(SUGGESTED).cloned().collect();
        if out.len() < SUGGESTED {
            let mut rest: Vec<(&str, &str)> = self
                .convs
                .iter()
                .filter(|c| ok(&c.id) && !out.contains(&c.id))
                .map(|c| (c.id.as_str(), self.counts.get(&c.id).map_or("", |n| n.latest.as_str())))
                .collect();
            rest.sort_by(|a, b| b.1.cmp(a.1));
            out.extend(rest.into_iter().take(SUGGESTED - out.len()).map(|(id, _)| id.to_string()));
        }
        out
    }

    pub(crate) fn open_invite(&mut self) {
        let Some(me) = self.me.as_ref().map(|m| m.user_id.clone()) else { return };
        self.account.menu = None;
        self.invite = Some(Invite {
            emails: Editor::default(),
            message: Editor::default(),
            search: Editor::default(),
            field: Field::Emails,
            chosen: Vec::new(),
            suggested: self.recent_channels(),
            pick: 0,
            hit: 0,
            domain: None,
            sending: false,
            errors: Vec::new(),
        });
        // The placeholder shows addresses at your own domain, as Slack's does.
        self.spawn(move |s| {
            let domain = s.profile(&me).ok().and_then(|p| p.email.split_once('@').map(|(_, d)| d.to_string())).filter(|d| !d.is_empty());
            Event::Invite(Ev::Domain(domain))
        });
        self.dirty = true;
    }

    pub(crate) fn on_invite(&mut self, ev: Ev) {
        let team = self.me.as_ref().map(|m| m.team.clone()).unwrap_or_default();
        match ev {
            Ev::Domain(domain) => {
                if let Some(d) = self.invite.as_mut() {
                    d.domain = domain;
                }
            }
            Ev::Sent(res) => {
                let sent = res.as_ref().map(|r| r.iter().filter(|(_, e)| e.is_none()).count()).unwrap_or(0);
                if sent > 0 {
                    let text = match sent {
                        1 => trf!("✓ invitation sent to {team}", "✓ convite enviado para {team}"),
                        n => trf!("✓ {n} invitations sent to {team}", "✓ {n} convites enviados para {team}"),
                    };
                    self.brief(text, 5);
                }
                let Some(d) = self.invite.as_mut() else { return self.dirty = true };
                d.sending = false;
                match res {
                    Err(e) => d.errors = vec![whole(&e, &team)],
                    Ok(r) => {
                        let failed: Vec<(String, String)> = r.into_iter().filter_map(|(m, e)| e.map(|e| (m, e))).collect();
                        if failed.is_empty() {
                            self.invite = None;
                        } else if failed.iter().all(|(_, e)| forbidden(e)) {
                            d.errors = vec![whole(&failed[0].1, &team)];
                        } else {
                            // Only the refused addresses stay, to fix or drop.
                            d.errors = failed.iter().map(|(m, e)| format!("{m} {}", per_email(e))).collect();
                            d.emails.set(failed.iter().map(|(m, _)| m.as_str()).collect::<Vec<_>>().join(", "));
                            d.field = Field::Emails;
                        }
                    }
                }
            }
        }
        self.dirty = true;
    }

    fn send_invites(&mut self) {
        let Some(d) = self.invite.as_mut() else { return };
        if d.sending {
            return;
        }
        let emails = addresses(&d.emails.text);
        if emails.is_empty() {
            d.errors = vec![tr("Type at least one email address.", "Digite ao menos um e-mail.").into()];
            d.field = Field::Emails;
            return;
        }
        let bad: Vec<String> = emails.iter().filter(|e| !plausible(e)).map(|e| format!("{e} {}", per_email("invalid_email"))).collect();
        if !bad.is_empty() {
            (d.errors, d.field) = (bad, Field::Emails);
            return;
        }
        let message = d.message.text.trim().to_string();
        let channels = d.chosen.clone();
        (d.sending, d.errors) = (true, Vec::new());
        self.spawn(move |s| Event::Invite(Ev::Sent(send(s, &emails, &message, &channels))));
    }

    fn people_add(&mut self, id: String) {
        let Some(d) = self.invite.as_mut() else { return };
        if !d.chosen.contains(&id) {
            d.chosen.push(id);
        }
        d.search.set(String::new());
        d.hit = 0;
        let left = self.people_offered().len();
        if let Some(d) = self.invite.as_mut() {
            d.pick = d.pick.min(left.saturating_sub(1));
        }
    }

    /// The next stop from `from`, skipping the suggestions when none are left.
    fn people_step(&self, from: Field, delta: isize) -> Field {
        let empty = self.people_offered().is_empty();
        let n = ORDER.len() as isize;
        let mut i = ORDER.iter().position(|f| *f == from).unwrap_or(0) as isize;
        loop {
            i = (i + delta).rem_euclid(n);
            if !(empty && ORDER[i as usize] == Field::Suggested) {
                return ORDER[i as usize];
            }
        }
    }

    pub(crate) fn people_paste(&mut self, s: &str) {
        let Some(d) = self.invite.as_mut() else { return };
        match d.field {
            // A pasted list, one per line, becomes one per comma.
            Field::Emails => d.emails.insert(&s.lines().map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>().join(", ")),
            Field::Message => d.message.insert(s),
            Field::Search => {
                d.search.insert(s.lines().next().unwrap_or("").trim());
                d.hit = 0;
            }
            _ => {}
        }
    }

    pub(crate) fn people_key(&mut self, k: KeyEvent) {
        self.dirty = true;
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let Some(field) = self.invite.as_ref().map(|d| d.field) else { return };
        if (ctrl && matches!(k.code, KeyCode::Enter | KeyCode::Char('s'))) || (field == Field::Send && matches!(k.code, KeyCode::Enter | KeyCode::Char(' '))) {
            return self.send_invites();
        }
        match k.code {
            KeyCode::Esc => return self.invite = None,
            KeyCode::Tab | KeyCode::BackTab => {
                let next = self.people_step(field, if k.code == KeyCode::Tab { 1 } else { -1 });
                if let Some(d) = self.invite.as_mut() {
                    d.field = next;
                }
                return;
            }
            _ => {}
        }
        let offered = self.people_offered();
        let hits = self.people_hits();
        let msg_w = self.areas.invite.message_w;
        let Some(d) = self.invite.as_mut() else { return };
        match field {
            Field::Emails => match k.code {
                KeyCode::Enter | KeyCode::Down => d.field = Field::Message,
                _ => edit(&mut d.emails, k),
            },
            Field::Message => match k.code {
                KeyCode::Enter => d.message.insert("\n"),
                KeyCode::Up => {
                    if !d.message.vertical(msg_w, -1) {
                        d.field = Field::Emails;
                    }
                }
                KeyCode::Down => {
                    d.message.vertical(msg_w, 1);
                }
                _ => edit(&mut d.message, k),
            },
            Field::Suggested => match k.code {
                KeyCode::Left | KeyCode::Char('h') => d.pick = d.pick.saturating_sub(1),
                KeyCode::Right | KeyCode::Char('l') => d.pick = (d.pick + 1).min(offered.len().saturating_sub(1)),
                KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Char('+') => {
                    if let Some(id) = offered.get(d.pick).cloned() {
                        self.people_add(id);
                    }
                }
                _ => {}
            },
            Field::Search => match k.code {
                KeyCode::Up => d.hit = d.hit.saturating_sub(1),
                KeyCode::Down => d.hit = (d.hit + 1).min(hits.len().saturating_sub(1)),
                KeyCode::Enter => {
                    if let Some(id) = hits.get(d.hit).cloned() {
                        self.people_add(id);
                    }
                }
                // Like a pill box: Backspace on an empty field takes the last channel off.
                KeyCode::Backspace if d.search.text.is_empty() => {
                    d.chosen.pop();
                }
                _ => {
                    edit(&mut d.search, k);
                    d.hit = 0;
                }
            },
            Field::Send => {}
        }
    }

    pub(crate) fn people_mouse(&mut self, m: MouseEvent) {
        let at = Position::new(m.column, m.row);
        let a = &self.areas.invite;
        let hit = a.hits.iter().find(|(r, _)| r.contains(at)).map(|(_, i)| *i);
        if m.kind == MouseEventKind::Moved {
            if let (Some(i), Some(d)) = (hit, self.invite.as_mut()) {
                if d.hit != i {
                    d.hit = i;
                    self.dirty = true;
                }
            }
            return;
        }
        if m.kind != MouseEventKind::Down(MouseButton::Left) {
            return;
        }
        self.dirty = true;
        let find = |v: &[(Rect, String)]| v.iter().find(|(r, _)| r.contains(at)).map(|(_, id)| id.clone());
        let (suggested, chosen) = (find(&a.suggested), find(&a.chosen));
        let (close, send) = (a.close.contains(at), a.send.contains(at));
        let (er, sr) = (a.emails, a.search);
        let (mr, mtop, mw) = (a.message, a.message_top, a.message_w);
        let hits = self.people_hits();
        // Modal: a click outside does not throw the addresses away.
        if close {
            self.invite = None;
        } else if send {
            self.send_invites();
        } else if let Some(id) = suggested {
            self.people_add(id);
        } else if let Some(id) = chosen {
            if let Some(d) = self.invite.as_mut() {
                d.chosen.retain(|c| *c != id);
            }
        } else if let Some(id) = hit.and_then(|i| hits.get(i).cloned()) {
            self.people_add(id);
            if let Some(d) = self.invite.as_mut() {
                d.field = Field::Search;
            }
        } else if let Some(d) = self.invite.as_mut() {
            if er.contains(at) {
                d.field = Field::Emails;
                d.emails.cursor = cursor_at(&d.emails, er.start, er.offset(at).0);
            } else if sr.contains(at) {
                d.field = Field::Search;
                d.search.cursor = cursor_at(&d.search, sr.start, sr.offset(at).0);
            } else if mr.contains(at) {
                d.field = Field::Message;
                // The row clicked, then the column in it.
                let rows = d.message.rows(mw);
                let (dx, dy) = mr.offset(at);
                if let Some(row) = rows.get(mtop + dy as usize) {
                    let (dx, mut w, mut k) = (dx as usize, 0, 0);
                    for c in &row.chars {
                        w += c.width().unwrap_or(0);
                        if w > dx {
                            break;
                        }
                        k += 1;
                    }
                    d.message.cursor = row.start + k;
                } else {
                    d.message.cursor = d.message.text.chars().count();
                }
            }
        }
    }
}

// ---- drawing -----------------------------------------------------------------

/// What the modal stacks, top to bottom.
enum Part {
    Gap,
    Label(Line<'static>),
    Emails,
    Message,
    /// A row of pills: label, style and the channel each one stands for.
    Pills(Vec<(String, Style, String)>, bool),
    Search,
    Hit(usize),
    Error(String),
}

impl Part {
    /// Rows it takes, the message box showing `msg` rows of text.
    fn height(&self, msg: u16) -> u16 {
        match self {
            Part::Emails | Part::Search => 3,
            Part::Message => msg + 2,
            _ => 1,
        }
    }
}

/// Rows all the parts take.
fn total(parts: &[Part], msg: u16) -> u16 {
    parts.iter().map(|p| p.height(msg)).sum()
}

/// Pills laid out in rows of `w` cells, after a lead on the first.
fn pill_rows(lead: &str, pills: Vec<(String, Style, String)>, w: usize, suggested: bool) -> Vec<Part> {
    let mut rows: Vec<Vec<(String, Style, String)>> = vec![Vec::new()];
    let mut used = lead.width();
    for p in pills {
        let pw = p.0.width() + 3;
        if used + pw > w && !rows.last().is_some_and(|r| r.is_empty()) {
            rows.push(Vec::new());
            used = lead.width();
        }
        used += pw;
        rows.last_mut().unwrap().push(p);
    }
    rows.into_iter().map(|r| Part::Pills(r, suggested)).collect()
}

/// A rounded button or pill: the text on `bg`, a round cap on each side.
fn capped(text: String, style: Style) -> Line<'static> {
    let cap = Style::new().fg(style.bg.unwrap_or(SHADE));
    Line::from(vec![Span::styled(CAP_L, cap), Span::styled(text, style), Span::styled(CAP_R, cap)])
}

pub fn draw(f: &mut Frame, app: &mut App) {
    app.areas.invite = Areas::default();
    let Some(d) = &app.invite else { return };
    let team = app.me.as_ref().map(|m| m.team.clone()).unwrap_or_else(|| "Slack".into());
    let (field, sending, pick, sel_hit) = (d.field, d.sending, d.pick, d.hit);
    let (emails, message, search) = (d.emails.clone(), d.message.clone(), d.search.clone());
    let errors = d.errors.clone();
    let domain = d.domain.clone().unwrap_or_else(|| "example.com".into());
    let chosen = d.chosen.clone();
    let offered = app.people_offered();
    let hits = app.people_hits();
    // Names looked up now: drawing writes to `app`.
    let names: std::collections::HashMap<String, String> = offered
        .iter()
        .chain(&chosen)
        .chain(&hits)
        .map(|id| (id.clone(), app.people_chan(id).map(|(g, n)| format!("{g} {n}")).unwrap_or_else(|| id.clone())))
        .collect();
    let label = |id: &str| names.get(id).cloned().unwrap_or_default();

    let area = f.area();
    let w = 76.min(area.width.saturating_sub(4));
    // Inside the frame and two columns of margin on each side.
    let iw = w.saturating_sub(6) as usize;
    let mut parts = vec![
        Part::Gap,
        Part::Label(Line::from(Span::styled(tr("Email addresses", "Endereços de e-mail"), Style::new().bold()))),
        Part::Emails,
        Part::Message,
        Part::Gap,
        Part::Label(Line::from(vec![
            Span::styled(tr("Add to channels", "Adicionar aos canais"), Style::new().bold()),
            Span::styled(tr(" (optional)", " (opcional)"), dim()),
        ])),
    ];
    let pill = Style::new().fg(Color::Cyan).bg(SHADE);
    if !offered.is_empty() {
        let lead = tr("Suggested: ", "Sugeridos: ");
        let picked = Style::new().fg(Color::Black).bg(Color::Cyan).bold();
        let pills = offered
            .iter()
            .enumerate()
            .map(|(i, id)| {
                let style = if field == Field::Suggested && i == pick { picked } else { pill };
                (format!("+ {}", label(id)), style, id.clone())
            })
            .collect();
        parts.extend(pill_rows(lead, pills, iw, true));
    }
    if !chosen.is_empty() {
        let pills = chosen.iter().map(|id| (format!("{} ✕", label(id)), Style::new().fg(Color::White).bg(SELECTED), id.clone())).collect();
        parts.extend(pill_rows("", pills, iw, false));
    }
    parts.push(Part::Search);
    if field == Field::Search {
        parts.extend((0..hits.len()).map(Part::Hit));
        if hits.is_empty() && !search.text.trim().is_empty() {
            parts.push(Part::Error(tr("No channel of yours matches.", "Nenhum canal seu corresponde.").into()));
        }
    }
    if !errors.is_empty() {
        parts.push(Part::Gap);
        for e in &errors {
            parts.extend(wrap(e, iw).into_iter().map(Part::Error));
        }
    }
    parts.push(Part::Gap);

    // A short terminal loses the gaps first, then message rows; the frame
    // and the Send row always stay.
    let room = area.height.saturating_sub(2 + 3);
    let mut msg = MSG_ROWS;
    if total(&parts, msg) > room {
        parts.retain(|p| !matches!(p, Part::Gap));
    }
    while msg > 1 && total(&parts, msg) > room {
        msg -= 1;
    }
    let h = (total(&parts, msg) + 3).min(area.height.saturating_sub(2));
    let rect = Rect::new(area.x + (area.width.saturating_sub(w)) / 2, area.y + (area.height.saturating_sub(h)) / 2, w, h);
    f.render_widget(Clear, rect);
    let title = trf!(" Invite people to {team} ", " Convidar pessoas para {team} ");
    let block = Block::bordered().border_type(BorderType::Rounded).border_style(dim()).title(Line::from(title).bold());
    let inner = block.inner(rect).inner(Margin { horizontal: 2, vertical: 0 });
    f.render_widget(block, rect);
    app.areas.invite.dialog = rect;
    let close = Rect::new(rect.right().saturating_sub(5), rect.y, 3, 1);
    f.render_widget(Paragraph::new(" ✕ ").bold(), close);
    app.areas.invite.close = close;
    if inner.width < 24 || inner.height < 4 {
        return;
    }

    // Text room inside an input: its frame and a column of padding each side.
    let tw = inner.width.saturating_sub(4) as usize;
    // A one-line input with its placeholder; the cursor goes in it while focused.
    let one_line = |f: &mut Frame, e: &Editor, on: bool, hint: &str, y: u16| -> Input {
        let frame = Rect::new(inner.x, y, inner.width, 3);
        let (start, shown, cx) = crate::account::window(e, tw);
        let line = if e.text.is_empty() { Line::from(Span::styled(truncate(hint, tw), dim())) } else { Line::from(shown) };
        let text = rounded_input(f, frame, on, line);
        if on && !sending {
            f.set_cursor_position(((text.x + cx as u16).min(text.right().saturating_sub(1)), text.y));
        }
        Input { frame, text, start }
    };

    // The message box's rows, scrolled to keep the cursor in view.
    let rows = message.rows(tw);
    let (crow, ccol) = message.position(&rows);
    let top = crow.saturating_sub(msg as usize - 1);
    app.areas.invite.message_top = top;
    app.areas.invite.message_w = tw;

    // The Send row stays at the bottom; what does not fit above it is cut.
    let last = inner.bottom().saturating_sub(1);
    let mut y = inner.y;
    for (n, part) in parts.iter().enumerate() {
        let ph = part.height(msg);
        if y + ph > last {
            break;
        }
        let row = Rect::new(inner.x, y, inner.width, 1);
        match part {
            Part::Gap => {}
            Part::Label(l) => f.render_widget(Paragraph::new(l.clone()), row),
            Part::Emails => {
                let hint = trf!("Ex. ellis@{domain}, maria@{domain}", "Ex. ellis@{domain}, maria@{domain}");
                app.areas.invite.emails = one_line(f, &emails, field == Field::Emails, &hint, y);
            }
            Part::Message => {
                let frame = Rect::new(inner.x, y, inner.width, ph);
                let lines: Vec<Line> = if message.text.is_empty() {
                    vec![Line::from(Span::styled(tr("Add a personalized message (optional)", "Adicione uma mensagem personalizada (opcional)"), dim()))]
                } else {
                    rows.iter().skip(top).take(msg as usize).map(|r| Line::from(r.chars.iter().collect::<String>())).collect()
                };
                let on = field == Field::Message;
                let text = rounded_input(f, frame, on, lines);
                app.areas.invite.message = Input { frame, text, start: 0 };
                if on && !sending && crow >= top && crow < top + msg as usize {
                    f.set_cursor_position(((text.x + ccol as u16).min(text.right().saturating_sub(1)), text.y + (crow - top) as u16));
                }
            }
            Part::Pills(pills, suggested) => {
                let lead = if *suggested { tr("Suggested: ", "Sugeridos: ") } else { "" };
                let first = n == 0 || !matches!(&parts[n - 1], Part::Pills(_, s) if s == suggested);
                let mut x = inner.x;
                if *suggested {
                    if first {
                        f.render_widget(Paragraph::new(Span::styled(lead, dim())), Rect::new(x, y, lead.width() as u16, 1).intersection(row));
                    }
                    x += lead.width() as u16;
                }
                for (text, style, id) in pills {
                    let pw = text.width() as u16 + 2;
                    let r = Rect::new(x, y, pw, 1).intersection(row);
                    f.render_widget(Paragraph::new(capped(text.clone(), *style)), r);
                    let list = if *suggested { &mut app.areas.invite.suggested } else { &mut app.areas.invite.chosen };
                    list.push((r, id.clone()));
                    x += pw + 1;
                }
            }
            Part::Search => {
                let hint = if chosen.is_empty() { tr("Search channels", "Buscar canais") } else { tr("Search channels · Backspace removes the last", "Buscar canais · Backspace remove o último") };
                app.areas.invite.search = one_line(f, &search, field == Field::Search, hint, y);
            }
            Part::Hit(i) => {
                let Some(id) = hits.get(*i) else { continue };
                // Under the search input, lined up with its text.
                let r = Rect::new(inner.x + 1, y, inner.width.saturating_sub(2), 1);
                let text = truncate(&label(id), (r.width as usize).saturating_sub(2));
                let pad = (r.width as usize).saturating_sub(text.width() + 2);
                let mut line = Line::from(format!(" {text}{} ", " ".repeat(pad)));
                if *i == sel_hit {
                    mark_selected(&mut line);
                }
                f.render_widget(Paragraph::new(line), r);
                app.areas.invite.hits.push((r, *i));
            }
            Part::Error(e) => f.render_widget(Paragraph::new(Span::styled(e.clone(), Style::new().fg(Color::Red))), row),
        }
        y += ph;
    }

    // Hint, then Send on the right: quiet until there is an address to send to.
    let by = inner.bottom().saturating_sub(1);
    let send = if sending { tr(" Sending… ", " Enviando… ") } else { tr("  Send  ", "  Enviar  ") };
    let send_r = Rect::new(inner.right().saturating_sub(send.width() as u16 + 2), by, send.width() as u16 + 2, 1);
    let ready = !addresses(&emails.text).is_empty();
    let (fg, bg) = match (sending, field == Field::Send, ready) {
        (true, _, _) => (Color::DarkGray, SHADE),
        (false, true, _) => (Color::Black, Color::Cyan),
        (false, false, true) => (Color::Black, Color::Green),
        _ => (Color::Gray, SELECTED),
    };
    f.render_widget(Paragraph::new(capped(send.to_string(), Style::new().fg(fg).bg(bg).bold())), send_r);
    app.areas.invite.send = send_r;
    let hint_w = send_r.x.saturating_sub(inner.x + 1);
    let hint = tr("Tab fields · Ctrl+S sends · Esc closes", "Tab campos · Ctrl+S envia · Esc fecha");
    f.render_widget(Paragraph::new(truncate(hint, hint_w as usize)).style(dim()), Rect::new(inner.x, by, hint_w, 1));
}
