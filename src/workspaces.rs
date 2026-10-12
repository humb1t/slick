//! Every signed-in workspace, one `App` each, and which one is on screen.
//! The main loop drains every workspace's events; keys, clicks and drawing
//! go to the active one.

use crate::app::{App, Event};
use crate::cache::{self, Cache};
use crate::config;
use crate::i18n::tr;
use crate::ipc::Cmd;
use crate::pause::DndAll;
use crate::slack::{Creds, Slack};
use crate::switcher::{Badge, SwitchReq, WsInfo};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui_image::picker::Picker;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

/// How far apart the workspaces behind the first one start loading.
const STAGGER: Duration = Duration::from_millis(1500);

/// One workspace: its app and the events its threads send it.
pub struct Ws {
    pub app: App,
    pub rx: Receiver<Event>,
    /// `None` until its first `auth.test` (a legacy config, or one being added).
    pub team_id: Option<String>,
    /// The `acme` in acme.slack.com, from the config, until `auth.test` says.
    domain: Option<String>,
}

impl Ws {
    /// Its id or its domain, as `slick open --team` takes them.
    fn is(&self, team: &str) -> bool {
        let me = self.app.me.as_ref();
        self.team_id.as_deref() == Some(team)
            || me.is_some_and(|m| m.team_id == team || m.domain.eq_ignore_ascii_case(team))
            || self.domain.as_deref().is_some_and(|d| d.eq_ignore_ascii_case(team))
    }
}

pub struct Workspaces {
    pub list: Vec<Ws>,
    /// Index of the one on screen in `list`.
    pub active: usize,
    /// Where "Add a workspace" came from: Esc goes back there.
    prev: usize,
    /// The slick that owns the socket speaks for all (`events`).
    owner: bool,
    /// `SLACK_TOKEN` in the environment: one workspace, kept out of the config.
    env: bool,
}

impl Workspaces {
    /// One app per workspace in the config, the last one on screen in front;
    /// none signed in means the sign-in screen.
    pub fn new(picker: Option<Picker>, owner: bool) -> Self {
        let confs = config::workspaces();
        let env = std::env::var("SLACK_TOKEN").is_ok_and(|t| !t.is_empty());
        let last = config::active();
        let active = confs.iter().position(|c| c.team_id.is_some() && c.team_id == last).unwrap_or(0);
        let mut list = Vec::new();
        if confs.is_empty() {
            let (tx, rx) = mpsc::channel();
            let app = App::new(Slack::new(Creds::default()), tx, cache::load(), picker.clone(), false);
            list.push(Ws { app, rx, team_id: None, domain: None });
        }
        let mut behind = 0;
        for (i, c) in confs.into_iter().enumerate() {
            let (tx, rx) = mpsc::channel();
            let cache = cache::load_for(c.team_id.as_deref());
            let slack = Slack::new(c.creds.clone());
            let mut app = if i == active {
                App::new(slack, tx, cache, picker.clone(), true)
            } else {
                behind += 1;
                App::new_background(slack, tx, cache, picker.clone(), STAGGER * behind)
            };
            // Its `team` is written at the first `auth.test`.
            if c.team_id.is_none() && !env {
                app.legacy = Some(c.creds);
            }
            list.push(Ws { app, rx, team_id: c.team_id, domain: c.domain });
        }
        for w in &mut list {
            w.app.events.on = owner;
            #[cfg(target_os = "linux")]
            {
                w.app.account.tray = config::tray();
            }
        }
        list[active].app.starting();
        Self { list, active, prev: active, owner, env }
    }

    pub fn active(&self) -> &App {
        &self.list[self.active].app
    }

    pub fn active_mut(&mut self) -> &mut App {
        &mut self.list[self.active].app
    }

    pub fn quit(&self) -> bool {
        self.list.iter().any(|w| w.app.quit)
    }

    pub fn restart(&self) -> bool {
        self.list.iter().any(|w| w.app.restart)
    }

    pub fn dirty(&self) -> bool {
        self.list.iter().any(|w| w.app.dirty)
    }

    /// Mentions and DMs waiting in every signed-in workspace, for the tray.
    pub fn attention(&self) -> (u64, usize) {
        self.list.iter().filter(|w| w.app.login.is_none()).map(|w| w.app.attention()).fold((0, 0), |a, b| (a.0 + b.0, a.1 + b.1))
    }

    /// Each workspace's events to its own app.
    pub fn drain(&mut self) {
        for w in &mut self.list {
            while let Ok(ev) = w.rx.try_recv() {
                w.app.handle(ev);
            }
        }
    }

    pub fn tick(&mut self) {
        for w in &mut self.list {
            w.app.tick();
        }
    }

    /// An event for slick as a whole: quitting, another process, an update.
    pub fn global(&mut self, ev: Event) {
        match ev {
            Event::Remote(cmd) => self.remote(cmd),
            Event::Quit => self.list.iter_mut().for_each(|w| w.app.quit = true),
            ev => self.active_mut().handle(ev),
        }
    }

    /// `slick open`/`mark-read` go to the workspace that has the
    /// conversation; a pause is for all of them.
    fn remote(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Open { conv, ts, thread, focus, team } => {
                let i = self.pick(team.as_deref(), &conv);
                self.switch_to(i);
                self.list[i].app.handle(Event::Remote(Cmd::Open { conv, ts, thread, focus, team }));
            }
            Cmd::MarkRead { conv, team } => {
                let i = self.pick(team.as_deref(), &conv);
                self.list[i].app.handle(Event::Remote(Cmd::MarkRead { conv, team }));
            }
            Cmd::Pause(minutes) => self.dnd(DndAll::Pause { minutes, vips: None }),
            Cmd::Resume => self.dnd(DndAll::Resume),
            cmd => self.active_mut().handle(Event::Remote(cmd)),
        }
    }

    /// Pausing notifications pauses them in every signed-in workspace.
    fn dnd(&mut self, req: DndAll) {
        for w in self.list.iter_mut().filter(|w| w.app.login.is_none()) {
            w.app.apply_dnd(req);
        }
    }

    /// The workspace named, else the one on screen if it has the
    /// conversation, else the first that has it, else the one on screen.
    fn pick(&self, team: Option<&str>, conv: &str) -> usize {
        if let Some(i) = team.and_then(|t| self.list.iter().position(|w| w.is(t))) {
            return i;
        }
        let has = |w: &Ws| w.app.convs.iter().any(|c| c.id == conv);
        if has(&self.list[self.active]) {
            return self.active;
        }
        self.list.iter().position(has).unwrap_or(self.active)
    }

    /// Ctrl+1..9, before the app sees the key. Only with more than one.
    pub fn key(&mut self, k: KeyEvent) -> bool {
        if self.list.len() < 2 || !k.modifiers.contains(KeyModifiers::CONTROL) {
            return false;
        }
        let KeyCode::Char(c @ '1'..='9') = k.code else { return false };
        self.switch_to(c as usize - '1' as usize);
        true
    }

    /// Puts the workspace at `i` on screen; the one before goes to the background.
    pub fn switch_to(&mut self, i: usize) {
        if i == self.active || i >= self.list.len() {
            return;
        }
        let from = self.active;
        let old = &mut self.list[from].app;
        let focused = old.focused;
        old.set_focused(false);
        old.background = true;
        // An "add" left half done goes away.
        old.discard |= old.adding;
        let carry = (old.update.take(), old.sidebar_width, old.sidebar_avatars, old.account.tray, old.picker.clone());
        let new = &mut self.list[i].app;
        (new.update, new.sidebar_width, new.sidebar_avatars, new.account.tray, new.picker) = carry;
        new.protocols.clear();
        new.activated();
        new.set_focused(focused);
        new.dirty = true;
        self.active = i;
        if let Some(id) = self.list[i].team_id.as_deref().filter(|_| !self.env) {
            let _ = config::set_active(id);
        }
    }

    /// Starts a new workspace at the sign-in screen and puts it on screen.
    pub fn add(&mut self) {
        let (tx, rx) = mpsc::channel();
        let picker = self.active().picker.clone();
        let mut app = App::new(Slack::new(Creds::default()), tx, Cache::default(), picker, false);
        app.adding = true;
        app.events.on = self.owner;
        // Not the address of the one signed in already.
        if let Some(l) = app.login.as_mut() {
            l.workspace.clear();
        }
        self.list.push(Ws { app, rx, team_id: None, domain: None });
        self.prev = self.active;
        self.switch_to(self.list.len() - 1);
    }

    /// After events and input: ids learned, sign-ins from "add", workspaces
    /// dropped, and the switch a key or click asked for.
    pub fn settle(&mut self) {
        for i in 0..self.list.len() {
            self.learn_team(i);
        }
        // From the menu, the pause dialog or `/dnd`, in any of them.
        if let Some(req) = self.list.iter_mut().find_map(|w| w.app.dnd_all.take()) {
            self.dnd(req);
        }
        if let Some(i) = self.list.iter().position(|w| w.app.added.is_some()) {
            self.on_added(i);
        }
        while let Some(i) = self.list.iter().position(|w| w.app.discard || w.app.signed_out) {
            if self.list.len() == 1 {
                let app = &mut self.list[0].app;
                (app.discard, app.adding) = (false, false);
                if std::mem::take(&mut app.signed_out) {
                    app.open_login(None);
                }
                break;
            }
            if i == self.active {
                let to = if self.prev != i && self.prev < self.list.len() { self.prev } else if i == 0 { 1 } else { 0 };
                self.switch_to(to);
            }
            self.list.remove(i);
            if self.active > i {
                self.active -= 1;
            }
            if self.prev > i {
                self.prev -= 1;
            } else if self.prev == i {
                self.prev = self.active;
            }
        }
        if let Some(req) = self.active_mut().switch_req.take() {
            match req {
                SwitchReq::To(i) => self.switch_to(i),
                SwitchReq::Add => self.add(),
            }
        }
    }

    /// `auth.test` named the workspace: its cache and the config go by that id.
    fn learn_team(&mut self, i: usize) {
        let w = &mut self.list[i];
        let Some(me) = w.app.me.as_ref().filter(|_| !self.env && !w.app.adding) else { return };
        if w.team_id.as_deref() == Some(me.team_id.as_str()) {
            return;
        }
        w.team_id = Some(me.team_id.clone());
        w.domain = Some(me.domain.clone());
        if i == self.active {
            let _ = config::set_active(&me.team_id);
        }
    }

    /// "Add a workspace" signed in: one already here only gets the new
    /// session; any other joins the list.
    fn on_added(&mut self, i: usize) {
        let Some((me, creds)) = self.list[i].app.added.take() else { return };
        let saved = save_session(&me, &creds);
        let same = |w: &Ws| w.team_id.as_deref() == Some(me.team_id.as_str()) || w.app.me.as_ref().is_some_and(|m| m.team_id == me.team_id);
        match (0..self.list.len()).find(|&j| j != i && same(&self.list[j])) {
            Some(j) => {
                let app = &mut self.list[j].app;
                if app.login.is_some() {
                    app.sign_in(creds);
                } else {
                    app.slack().set_creds(creds);
                }
                self.list[i].app.discard = true;
                self.switch_to(j);
            }
            None => {
                let w = &mut self.list[i];
                w.team_id = Some(me.team_id.clone());
                w.domain = Some(me.domain.clone());
                w.app.adding = false;
                w.app.sign_in(creds);
                let _ = config::set_active(&me.team_id);
            }
        }
        if let Err(e) = saved {
            self.active_mut().error = Some(trf!("didn't save the config: {e}", "não salvou o config: {e}"));
        }
    }

    /// The switcher's view of every workspace, for the one on screen; a
    /// change in it redraws.
    pub fn fill_infos(&mut self) {
        let infos: Vec<WsInfo> = self
            .list
            .iter()
            .enumerate()
            .map(|(i, w)| {
                let me = w.app.me.as_ref();
                let domain = me.map(|m| m.domain.clone()).or_else(|| w.domain.clone()).unwrap_or_default();
                let name = match me {
                    Some(m) => m.team.clone(),
                    None if w.app.adding => tr("New workspace", "Novo workspace").into(),
                    None => domain.clone(),
                };
                WsInfo {
                    id: me.map(|m| m.team_id.clone()).or_else(|| w.team_id.clone()).unwrap_or_default(),
                    name,
                    domain,
                    icon_url: w.app.team_icon.clone(),
                    badge: if w.app.login.is_some() { Badge::None } else { w.app.badge() },
                    active: i == self.active,
                    expired: w.app.login.is_some() && !w.app.adding,
                }
            })
            .collect();
        let app = self.active_mut();
        if !same_infos(&app.workspaces, &infos) {
            app.workspaces = infos;
            app.dirty = true;
        }
    }

    /// The snapshot and events other programs see, from every workspace.
    pub fn publish(&mut self) {
        if !self.owner {
            return;
        }
        // `Null` keeps the indices lined up with `active`.
        let parts: Vec<serde_json::Value> = self.list.iter_mut().map(|w| w.app.snapshot_part().unwrap_or(serde_json::Value::Null)).collect();
        crate::events::publish_all(&parts, self.active);
    }

    pub fn unpublish(&mut self) {
        if self.owner {
            crate::events::unpublish_all();
        }
    }

    /// Every workspace's cache, under its id; one being added has none.
    pub fn save(&self) {
        for w in self.list.iter().filter(|w| !w.app.adding) {
            cache::save_for(w.team_id.as_deref(), w.app.to_cache());
        }
    }
}

fn same_infos(a: &[WsInfo], b: &[WsInfo]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| {
            (&x.id, &x.name, &x.domain, &x.icon_url, x.badge, x.active, x.expired) == (&y.id, &y.name, &y.domain, &y.icon_url, y.badge, y.active, y.expired)
        })
}

/// Saves a checked session. When that changes the shared `d` cookie, each
/// workspace that followed it is tried with the new one; those Slack refuses
/// keep the old one as their own.
pub fn save_session(me: &crate::slack::AuthInfo, creds: &Creds) -> std::io::Result<()> {
    let before = config::workspaces();
    config::save_workspace(me, creds)?;
    let after = config::workspaces();
    for w in after {
        let Some(id) = w.team_id.clone().filter(|id| *id != me.team_id) else { continue };
        let old = before.iter().find(|b| b.team_id.as_ref() == Some(&id)).and_then(|b| b.creds.cookie.clone());
        let Some(old) = old.filter(|o| w.creds.cookie.as_ref() != Some(o)) else { continue };
        std::thread::spawn(move || {
            if Slack::new(w.creds).auth().is_err() {
                let _ = config::pin_cookie(&id, &old);
            }
        });
    }
    Ok(())
}
