//! The workspace switcher: the dropdown under the workspace's name, its
//! badges and what a pick asks `Workspaces` to do.

use crate::app::App;
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::Span;

/// What a workspace in the list waits with.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Badge {
    #[default]
    None,
    /// Unread messages only in channels that aren't muted.
    Dot,
    /// Mentions plus DMs with something unread.
    Count(u32),
}

/// One signed-in workspace, as the switcher shows it.
#[derive(Clone, Debug, Default)]
pub struct WsInfo {
    #[allow(dead_code)]
    pub id: String,
    pub name: String,
    /// The `acme` in acme.slack.com.
    pub domain: String,
    pub icon_url: Option<String>,
    pub badge: Badge,
    /// The one on screen.
    pub active: bool,
    /// Its session was rejected: picking it signs in again.
    pub expired: bool,
}

/// A switch asked for by a key or a click; `Workspaces` carries it out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwitchReq {
    /// To the workspace at this index (Ctrl+1..9 and the dropdown's rows).
    To(usize),
    /// "Add a workspace": a new one, starting at the sign-in screen.
    Add,
}

/// Where the last draw put the dropdown's rows, for mouse clicks, and the
/// keyboard's place in it.
#[derive(Default)]
pub struct Areas {
    /// Each workspace's row and its index.
    pub rows: Vec<(Rect, usize)>,
    pub add: Rect,
    /// The highlighted row: a workspace's index, or the list's length for "Add".
    pub sel: usize,
    /// Drawn open last time; the first draw after opening puts `sel` on the active one.
    pub open: bool,
}

/// The workspaces to list: the manager's, or just this one before it fills them.
pub fn list(app: &App) -> Vec<WsInfo> {
    if !app.workspaces.is_empty() {
        return app.workspaces.clone();
    }
    let Some(me) = &app.me else { return Vec::new() };
    vec![WsInfo {
        id: me.team_id.clone(),
        name: me.team.clone(),
        domain: me.domain.clone(),
        icon_url: app.team_icon.clone(),
        badge: app.badge(),
        active: true,
        expired: false,
    }]
}

/// What the workspaces not on screen wait with, together.
pub fn others(list: &[WsInfo]) -> Badge {
    let mut n = 0;
    let mut dot = false;
    for w in list.iter().filter(|w| !w.active) {
        match w.badge {
            Badge::Count(c) => n += c,
            Badge::Dot => dot = true,
            Badge::None => {}
        }
    }
    if n > 0 {
        Badge::Count(n)
    } else if dot {
        Badge::Dot
    } else {
        Badge::None
    }
}

/// A badge as text: the sidebar's red pill with the number, or a dot.
pub fn badge_spans(b: Badge) -> Vec<Span<'static>> {
    match b {
        Badge::None => Vec::new(),
        Badge::Dot => vec![Span::styled("●", Style::new().bold())],
        Badge::Count(n) => {
            let n = if n > 99 { "99+".to_string() } else { n.to_string() };
            vec![
                Span::styled(crate::blocks::CAP_L, Style::new().fg(Color::Red)),
                Span::styled(n, Style::new().fg(Color::White).bg(Color::Red).bold()),
                Span::styled(crate::blocks::CAP_R, Style::new().fg(Color::Red)),
            ]
        }
    }
}

impl App {
    /// Keys while the dropdown is open; false lets the rest of the app have it.
    pub fn switcher_key(&mut self, k: KeyEvent) -> bool {
        let n = list(self).len();
        let sel = &mut self.areas.switcher.sel;
        match k.code {
            KeyCode::Esc => self.team_popup = false,
            KeyCode::Up => *sel = sel.saturating_sub(1),
            KeyCode::Down => *sel = (*sel + 1).min(n),
            KeyCode::Enter => {
                let i = *sel;
                self.switcher_pick(i, n);
            }
            _ => return false,
        }
        self.dirty = true;
        true
    }

    /// The mouse while the dropdown is open: hover, picks, the address and
    /// the ID, and a click outside closes it. False leaves the click on the
    /// workspace's name to toggle it.
    pub fn switcher_mouse(&mut self, m: MouseEvent) -> bool {
        let at = Position::new(m.column, m.row);
        let inside = self.areas.team_popup.contains(at);
        let a = &self.areas.switcher;
        let hit = a.rows.iter().find(|(r, _)| r.contains(at)).map(|&(_, i)| i);
        let n = list(self).len();
        let hit = hit.or_else(|| a.add.contains(at).then_some(n));
        match m.kind {
            MouseEventKind::Moved => {
                if let Some(i) = hit.filter(|&i| i != self.areas.switcher.sel) {
                    self.areas.switcher.sel = i;
                    self.dirty = true;
                }
                inside
            }
            MouseEventKind::Down(MouseButton::Left) if self.areas.team_name.contains(at) => false,
            MouseEventKind::Down(button) => {
                if button == MouseButton::Left {
                    if let Some(i) = hit {
                        self.switcher_pick(i, n);
                    } else if self.areas.team_url.contains(at) {
                        if let Some(url) = self.team_url() {
                            self.open_url(&url);
                        }
                    } else if self.areas.team_id.contains(at) {
                        let id = self.me.as_ref().map(|m| m.team_id.clone()).unwrap_or_default();
                        self.copy(&id);
                        self.brief_copied(trf!("ID {id} copied", "ID {id} copiado"));
                        self.copied = Some(("team", std::time::Instant::now()));
                    }
                }
                if !inside {
                    self.team_popup = false;
                }
                self.dirty = true;
                true
            }
            _ => inside,
        }
    }

    /// Row `i` picked: a workspace, or "Add" past the last one.
    fn switcher_pick(&mut self, i: usize, n: usize) {
        self.team_popup = false;
        let list = list(self);
        self.switch_req = match list.get(i) {
            // The one on screen: nothing to do, unless it must sign in again.
            Some(w) if w.active && !w.expired => None,
            Some(_) => Some(SwitchReq::To(i)),
            None if i == n => Some(SwitchReq::Add),
            None => None,
        };
    }

    /// This workspace's badge: mentions and unread DMs as a number, else a
    /// dot for unread channels that aren't muted or set to mentions only.
    pub fn badge(&self) -> Badge {
        let (mentions, dms) = self.attention();
        let n = mentions + dms as u64;
        if n > 0 {
            return Badge::Count(n.min(u32::MAX as u64) as u32);
        }
        let unread = self.convs.iter().any(|c| !c.archived && self.unread(&c.id).0 && !self.quiet(c));
        if unread { Badge::Dot } else { Badge::None }
    }
}
