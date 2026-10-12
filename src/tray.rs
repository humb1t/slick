//! The system tray icon (StatusNotifierItem over D-Bus): lit while DMs or
//! mentions wait, a click focuses the window, the menu opens or quits.
//! Without a session bus or a tray host it quietly does nothing.

use crate::app::Event;
use crate::i18n::tr;
use ksni::blocking::TrayMethods;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Sender};

/// Mentions anywhere and DMs with something unread.
type State = (u64, usize);

/// The app's side: hands the tray thread a new state only when it changes.
pub struct Tray {
    tx: Sender<State>,
    /// With the language, so switching it redraws the tooltip and menu.
    last: Option<(State, crate::i18n::Lang)>,
}

impl Tray {
    /// Connects in its own thread, so a slow or missing bus never holds up the TUI.
    pub fn start(events: Sender<Event>) -> Tray {
        let (tx, rx) = mpsc::channel::<State>();
        std::thread::spawn(move || {
            let item = Item { events, state: (0, 0) };
            // A bar started later still finds us; no bus at all ends here.
            let Ok(handle) = item.assume_sni_available(true).spawn() else { return };
            for s in rx {
                if handle.update(|i| i.state = s).is_none() {
                    break;
                }
            }
        });
        Tray { tx, last: None }
    }

    pub fn set(&mut self, s: State) {
        let now = (s, crate::i18n::lang());
        if self.last != Some(now) {
            self.last = Some(now);
            let _ = self.tx.send(s);
        }
    }
}

struct Item {
    events: Sender<Event>,
    state: State,
}

impl Item {
    fn label(&self) -> String {
        match self.state {
            (1, _) => tr("slick · 1 mention", "slick · 1 menção").into(),
            (0, 0) => "slick".into(),
            (0, 1) => tr("slick · 1 unread", "slick · 1 não lida").into(),
            (0, n) => trf!("slick · {n} unread", "slick · {n} não lidas"),
            (n, _) => trf!("slick · {n} mentions", "slick · {n} menções"),
        }
    }
}

impl ksni::Tray for Item {
    fn id(&self) -> String {
        "slick".into()
    }

    fn title(&self) -> String {
        self.label()
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::Communications
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        let lit = self.state != (0, 0);
        let set: [(i32, &[u8]); 3] = if lit {
            [
                (22, include_bytes!("../assets/tray/unread-22.argb")),
                (32, include_bytes!("../assets/tray/unread-32.argb")),
                (64, include_bytes!("../assets/tray/unread-64.argb")),
            ]
        } else {
            [
                (22, include_bytes!("../assets/tray/normal-22.argb")),
                (32, include_bytes!("../assets/tray/normal-32.argb")),
                (64, include_bytes!("../assets/tray/normal-64.argb")),
            ]
        };
        set.iter().map(|(n, data)| ksni::Icon { width: *n, height: *n, data: data.to_vec() }).collect()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip { title: self.label(), ..Default::default() }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        focus();
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::StandardItem;
        vec![
            StandardItem { label: tr("Open", "Abrir").into(), activate: Box::new(|_: &mut Self| focus()), ..Default::default() }.into(),
            StandardItem {
                label: tr("Quit", "Sair").into(),
                activate: Box::new(|i: &mut Self| {
                    let _ = i.events.send(Event::Quit);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

/// Brings the terminal running slick to the front. Hyprland only.
fn focus() {
    if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_none() {
        return;
    }
    let class = std::env::var("SLICK_WINDOW_CLASS").ok().filter(|c| !c.is_empty()).unwrap_or_else(|| "org.omarchy.slick".into());
    std::thread::spawn(move || {
        // Lua configs take `eval`; the classic dispatcher is the fallback.
        let lua = format!("hl.dispatch(hl.dsp.focus({{ window = \"class:{class}\" }}))");
        if !hyprctl(&["eval", &lua]) {
            hyprctl(&["dispatch", "focuswindow", &format!("class:{class}")]);
        }
    });
}

/// Whether hyprctl ran and didn't answer with an error.
fn hyprctl(args: &[&str]) -> bool {
    Command::new("hyprctl")
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .is_ok_and(|o| o.status.success() && !String::from_utf8_lossy(&o.stdout).to_lowercase().contains("error"))
}
