//! The mouse wheel, as in omaslack's WheelScroll: a notch goes its own
//! distance (per pane), and every notch adds to what is left to go, which
//! a few frames cover, so notches in a row (or a touchpad's stream) add up
//! into one motion that eases into place instead of jumping or crawling.

use std::time::{Duration, Instant};

use crossterm::event::{MouseEvent, MouseEventKind};

use crate::app::App;

/// What the wheel is scrolling.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pane {
    Messages,
    Thread,
    Sidebar,
    /// The folded sidebar: a step is a photo.
    Rail,
    Activity,
    Threads,
    Pins,
    Search,
    Help,
}

impl Pane {
    /// Lines (rows) a notch goes.
    fn step(self) -> i32 {
        match self {
            Pane::Messages | Pane::Thread => 6,
            Pane::Sidebar => 4,
            Pane::Rail => 1,
            Pane::Activity | Pane::Threads | Pane::Pins | Pane::Search | Pane::Help => 4,
        }
    }
}

/// Events this close together came from one notch (terminals that send
/// several per notch) or from a backlog: a line each.
const SAME: Duration = Duration::from_millis(5);
/// Closer than this, a stream (a touchpad, a free-spinning wheel): a short
/// step each, the stream adding up.
const STREAM: Duration = Duration::from_millis(25);
/// Notches closer than this are a spin: each goes a bit further, up to twice.
const SPIN: Duration = Duration::from_millis(150);
/// Between the frames of a glide.
const FRAME: Duration = Duration::from_millis(16);

#[derive(Default)]
pub struct Wheel {
    pane: Option<Pane>,
    up: bool,
    /// Lines still to go: up is positive.
    left: i32,
    /// The last notch, and how many came in a spin before it.
    last: Option<Instant>,
    spin: i32,
    due: Option<Instant>,
}

impl Wheel {
    /// When the next frame of a glide is due.
    pub fn due(&self) -> Option<Instant> {
        self.due
    }

    /// Lets go of a glide (a key moved the view, say).
    pub fn stop(&mut self) {
        (self.left, self.due) = (0, None);
    }

    /// A notch up or down over `pane`.
    pub fn notch(&mut self, pane: Pane, up: bool) {
        let now = Instant::now();
        let gap = self.last.map(|t| now - t);
        self.last = Some(now);
        // Another pane, or turned the other way: starts over from here.
        let same = self.pane == Some(pane) && self.up == up;
        if !same {
            (self.pane, self.up, self.left, self.spin) = (Some(pane), up, 0, 0);
        }
        let step = pane.step();
        let n = match gap {
            Some(g) if same && g < SAME => 1,
            Some(g) if same && g < STREAM => (step / 3).max(1),
            Some(g) if same && g < SPIN => {
                self.spin = (self.spin + 1).min(4);
                step + step * self.spin / 4
            }
            _ => {
                self.spin = 0;
                step
            }
        };
        self.left += if up { n } else { -n };
        self.due.get_or_insert(now);
    }

    /// The share of what is left that a frame goes: half, at least a line.
    fn frame(&mut self, now: Instant) -> Option<(Pane, isize)> {
        if self.due.is_none_or(|t| t > now) {
            return None;
        }
        let pane = self.pane?;
        let n = (self.left.abs() + 1) / 2 * self.left.signum();
        self.left -= n;
        self.due = (self.left != 0).then(|| now + FRAME);
        (n != 0).then_some((pane, n as isize))
    }
}

/// PgUp/PgDn's distance in a view `height` lines tall: a page, less three
/// lines kept on screen to read on from (ten before the first draw).
pub fn page(height: usize) -> usize {
    if height == 0 { 10 } else { height.saturating_sub(3).max(3) }
}

/// Which way a wheel event goes, when it is one.
fn up(m: &MouseEvent) -> Option<bool> {
    match m.kind {
        MouseEventKind::ScrollUp => Some(true),
        MouseEventKind::ScrollDown => Some(false),
        _ => None,
    }
}

impl App {
    /// A wheel event over `pane`: its glide starts with the next frame.
    pub(crate) fn wheel(&mut self, m: &MouseEvent, pane: Pane) {
        if let Some(up) = up(m) {
            self.wheel.notch(pane, up);
        }
    }

    /// The glide's frame, when due: moves the pane and asks for a redraw.
    pub fn wheel_frame(&mut self) {
        let Some((pane, n)) = self.wheel.frame(Instant::now()) else { return };
        // Up is toward older messages, the top of a list.
        match pane {
            Pane::Messages if n > 0 => self.scroll_by(n as usize),
            Pane::Messages => self.scroll_down(n.unsigned_abs()),
            Pane::Thread => self.scroll_thread(n),
            Pane::Sidebar | Pane::Rail => {
                self.sidebar_wheel = true;
                let max = match pane {
                    Pane::Sidebar => self.rows().len().saturating_sub(self.areas.sidebar.height as usize),
                    _ => self.areas.rail_max,
                };
                let off = self.list.offset().saturating_add_signed(-n).min(max);
                *self.list.offset_mut() = off;
            }
            Pane::Activity => self.wheel_activity(-n),
            Pane::Threads => self.scroll_threads(-n),
            Pane::Pins => self.scroll_pins(-n),
            Pane::Search => self.scroll_search(-n),
            Pane::Help => self.scroll_help(-n),
        }
        self.dirty = true;
    }
}
