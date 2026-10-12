//! Slash commands in the message box, like Slack's: a `/` at the start of the
//! box lists the workspace's commands, Slack's own and the apps', filtered as
//! you type. Sending `/command text` runs it instead of posting the text:
//! the few Slack runs in its own client run here, the rest go to Slack
//! (`chat.command`), whose answers only you see arrive over the websocket.
//! The list (`commands.list`) and `chat.command` are the web client's, so
//! only session tokens (xoxc) get them.

use crate::app::{App, Event, ImgRef};
use crate::editor::{is_token, Lookup};
use crate::i18n::tr;
use crate::slack::{escape, Kind, Message, Res, Slack};
use crate::text::search;
use crate::ui::{dim, draw_image, mark_selected, truncate};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use ratatui::Frame;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use unicode_width::UnicodeWidthStr;

/// Commands in view at once; each takes two rows, and the list scrolls.
const SHOWN: usize = 5;
/// For commands without a picture: Slack's own, or with no graphics here.
const PROMPT: &str = "\u{f120}";
/// The subtype a command's answer only you see gets here: Slack never lists
/// them, so they live in memory alone and are never cached.
pub const EPHEMERAL: &str = "ephemeral";

/// A command as `commands.list` describes it.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Command {
    /// With its slash: `/giphy`.
    pub name: String,
    /// What goes after the name, as Slack hints it: `[text]`.
    #[serde(default)]
    pub usage: String,
    #[serde(default)]
    pub desc: String,
    /// The app's name; empty for Slack's own commands.
    #[serde(default)]
    pub app: String,
    #[serde(default)]
    pub app_id: String,
    /// The app's picture, small.
    #[serde(default)]
    pub icon: Option<String>,
    /// Another name of a command (`/dm` of `/msg`): it runs, but stays off the list.
    #[serde(default)]
    pub alias_of: Option<String>,
    /// Offered in a thread's box too.
    #[serde(default)]
    pub in_thread: bool,
}

/// The commands and the popup's state, like the `@` one's.
#[derive(Default)]
pub struct Slash {
    pub commands: Vec<Command>,
    pub sel: usize,
    /// The first command in view, so the selected one stays shown.
    top: usize,
    /// The fragment it was last shown for; a new one starts at the top.
    last: String,
    /// Esc hides it until the fragment changes.
    dismissed: Option<String>,
    /// Where the last draw put it and each command's two rows, for the mouse.
    rect: Rect,
    rows: Vec<(Rect, usize)>,
}

impl Slash {
    pub fn new(commands: Vec<Command>) -> Self {
        Self { commands, ..Default::default() }
    }
}

pub enum Ev {
    List(Res<Vec<Command>>),
    /// A command Slack ran, or refused: the box it came from (`thread`) and
    /// its line, given back on failure; on success what Slack said, if anything.
    Ran { chan: String, thread: Option<String>, line: String, res: Res<Option<String>> },
}

pub fn fetch(s: &Slack) -> Res<Vec<Command>> {
    let v = s.call("commands.list", &[])?;
    let list: Vec<&Value> = match &v["commands"] {
        Value::Object(m) => m.values().collect(),
        Value::Array(a) => a.iter().collect(),
        _ => Vec::new(),
    };
    let text = |v: &Value| v.as_str().unwrap_or("").trim().to_string();
    let mut out: Vec<Command> = list
        .into_iter()
        .filter_map(|c| {
            let name = c["name"].as_str().filter(|n| n.starts_with('/'))?.to_lowercase();
            let core = c["type"] == "core";
            Some(Command {
                name,
                usage: text(&c["usage"]),
                desc: text(&c["desc"]),
                app: if core { String::new() } else { [text(&c["app_name"]), text(&c["service_name"])].into_iter().find(|s| !s.is_empty()).unwrap_or_default() },
                app_id: text(&c["app"]),
                icon: ["image_32", "image_48", "image_64"].iter().find_map(|k| c["icons"][*k].as_str().map(String::from)),
                alias_of: c["alias_of"].as_str().filter(|a| !a.is_empty()).map(String::from),
                in_thread: core || c["is_runnable_from_thread"].as_bool() == Some(true),
            })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Runs a command as Slack's web client does; `disp` is how it was typed.
fn run(s: &Slack, chan: &str, command: &str, text: &str, thread: Option<&str>) -> Res<Option<String>> {
    let text = escape(text);
    let mut p = vec![("channel", chan), ("command", command), ("disp", command), ("text", text.as_str())];
    if let Some(t) = thread {
        p.push(("thread_ts", t));
    }
    let v = s.call("chat.command", &p)?;
    Ok(v["response"].as_str().map(|r| r.trim().to_string()).filter(|r| !r.is_empty()))
}

/// The `/name` typed at the very start of the box, the cursor at its end: the name.
fn fragment(text: &str, cursor: usize) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    let q: String = chars.get(..cursor)?.strip_prefix(&['/'])?.iter().collect();
    let word_ends = chars.get(cursor).is_none_or(|c| c.is_whitespace());
    let ok = word_ends && !q.contains(|c: char| c.is_whitespace() || c == '/' || is_token(c));
    ok.then_some(q)
}

/// `/dnd 30 minutes`, `/dnd 2h`, `/dnd for 1 hour`: how many minutes.
fn minutes(s: &str) -> Option<i64> {
    let s = s.trim().trim_start_matches("for ").trim_start_matches("por ").trim();
    let digits: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
    let n: i64 = digits.parse().ok()?;
    let unit = s[digits.len()..].trim().to_lowercase();
    match unit.as_str() {
        "" | "m" | "min" | "mins" | "minute" | "minutes" | "minuto" | "minutos" => Some(n),
        "h" | "hr" | "hrs" | "hour" | "hours" | "hora" | "horas" => Some(n * 60),
        "d" | "day" | "days" | "dia" | "dias" => Some(n * 24 * 60),
        _ => None,
    }
}

/// Inserts by ts, skipping one already there.
fn insert(list: &mut Vec<Message>, m: Message) {
    if let Err(pos) = list.binary_search_by(|x| x.ts.cmp(&m.ts)) {
        list.insert(pos, m);
    }
}

pub fn is_ephemeral(m: &Message) -> bool {
    m.subtype.as_deref() == Some(EPHEMERAL)
}

impl App {
    /// The popup's commands, while a `/name` starts the box.
    pub fn slash_hits(&mut self) -> Option<Vec<Command>> {
        let ed = self.typing_editor()?;
        let q = fragment(&ed.text, ed.cursor)?;
        if self.slash.dismissed.as_deref() == Some(q.as_str()) {
            return None;
        }
        self.slash.dismissed = None;
        if self.slash.last != q {
            self.slash.sel = 0;
            self.slash.top = 0;
            self.slash.last = q.clone();
        }
        let thread = self.focus == crate::app::Focus::Thread;
        let mut found: Vec<_> = self
            .slash
            .commands
            .iter()
            .filter(|c| c.alias_of.is_none() && (!thread || c.in_thread))
            .filter_map(|c| Some(((search(&c.name[1..], &q)?.0, c.name.as_str()), c)))
            .collect();
        found.sort_by(|a, b| a.0.cmp(&b.0));
        let hits: Vec<Command> = found.into_iter().map(|(_, c)| c.clone()).collect();
        self.slash.sel = self.slash.sel.min(hits.len().saturating_sub(1));
        (!hits.is_empty()).then_some(hits)
    }

    /// Replaces the `/fragment` with the command and a space, like Slack.
    fn slash_with(&mut self, name: &str) {
        let Some(ed) = self.typing_editor_mut() else { return };
        let end = ed.text.chars().position(char::is_whitespace).unwrap_or(ed.text.chars().count());
        let spaced = ed.text.chars().nth(end).is_some_and(|c| c == ' ');
        ed.cursor = end;
        for _ in 0..end {
            ed.backspace();
        }
        ed.insert(name);
        if spaced {
            ed.right();
        } else {
            ed.insert(" ");
        }
    }

    /// Keys for the popup; true when it took the key.
    pub fn slash_key(&mut self, k: KeyEvent) -> bool {
        let Some(hits) = self.slash_hits() else { return false };
        let n = hits.len();
        let plain = !k.modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::ALT | KeyModifiers::CONTROL);
        let sel = self.slash.sel.min(n - 1);
        match k.code {
            KeyCode::Up => self.slash.sel = (sel + n - 1) % n,
            KeyCode::Down => self.slash.sel = (sel + 1) % n,
            KeyCode::Enter | KeyCode::Tab if plain => self.slash_with(&hits[sel].name),
            KeyCode::Esc => self.slash.dismissed = Some(self.slash.last.clone()),
            _ => return false,
        }
        true
    }

    /// Hover, the wheel and clicks on the popup; true when taken.
    pub fn slash_mouse(&mut self, m: MouseEvent) -> bool {
        let at = Position::new(m.column, m.row);
        if !self.slash.rect.contains(at) {
            return false;
        }
        let Some(hits) = self.slash_hits() else { return false };
        let n = hits.len();
        let row = self.slash.rows.iter().find(|(r, _)| r.contains(at)).map(|(_, i)| *i);
        match m.kind {
            MouseEventKind::Moved => match row {
                Some(i) if i != self.slash.sel => self.slash.sel = i,
                _ => return true,
            },
            MouseEventKind::ScrollUp => self.slash.sel = self.slash.sel.saturating_sub(1),
            MouseEventKind::ScrollDown => self.slash.sel = (self.slash.sel + 1).min(n - 1),
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(c) = row.and_then(|i| hits.get(i)) {
                    self.slash_with(&c.name.clone());
                }
            }
            _ => {}
        }
        self.dirty = true;
        true
    }

    /// Sending: a box starting with `/name` runs the command instead. True
    /// when it was one; an unknown one stays in the box, as in Slack.
    pub(crate) fn run_slash(&mut self, thread: bool) -> bool {
        let ed = if thread { self.thread.as_ref().map(|t| &t.editor) } else { Some(&self.editor) };
        let Some(line) = ed.map(|e| e.wire().trim().to_string()) else { return false };
        // "/ text", with a space, is a message.
        if !line.starts_with('/') || line[1..].starts_with(char::is_whitespace) || line.len() == 1 {
            return false;
        }
        let (typed, args) = line.split_once(char::is_whitespace).map_or((line.as_str(), ""), |(n, a)| (n, a.trim()));
        let typed = typed.to_lowercase();
        let known = self.slash.commands.iter().find(|c| c.name == typed);
        // Without the list (not loaded, or a token that can't get it) Slack decides.
        if known.is_none() && !self.slash.commands.is_empty() {
            self.error = Some(trf!(
                "{typed} is not a valid command. To send a message starting with \"/\", put a space before it.",
                "{typed} não é um comando válido. Para enviar uma mensagem que começa com \"/\", ponha um espaço antes."
            ));
            return true;
        }
        let canonical = known.and_then(|c| c.alias_of.clone()).unwrap_or_else(|| typed.clone());
        let target = if thread {
            self.thread.as_ref().map(|t| (t.chan.clone(), Some(t.ts.clone())))
        } else {
            self.current.clone().map(|c| (c, None))
        };
        let Some((chan, ts)) = target else { return true };
        let args = args.to_string();
        let ed = if thread { self.thread.as_mut().map(|t| &mut t.editor) } else { Some(&mut self.editor) };
        if let Some(ed) = ed {
            ed.take();
        }
        if !self.local_command(&canonical, &args, &chan, ts.as_deref()) {
            let line = line.clone();
            self.spawn(move |s| {
                let res = run(s, &chan, &typed, &args, ts.as_deref());
                Event::Slash(Ev::Ran { chan, thread: ts, line, res })
            });
        }
        if !thread {
            self.to_present();
        }
        true
    }

    /// What Slack's own client does by itself, done with what the app
    /// already has; false leaves it to Slack.
    fn local_command(&mut self, name: &str, args: &str, chan: &str, thread: Option<&str>) -> bool {
        let chan = chan.to_string();
        let line = format!("{name} {args}").trim().to_string();
        match name {
            "/shrug" => {
                let text = format!("{args} ¯\\_(ツ)_/¯").trim_start().to_string();
                match thread.map(String::from) {
                    Some(ts) => self.spawn(move |s| {
                        let res = s.reply(&chan, &ts, &text, false);
                        Event::Replied { chan, ts, text, res }
                    }),
                    None => self.spawn(move |s| {
                        let res = s.post(&chan, &text);
                        Event::Sent { chan, text, res }
                    }),
                }
            }
            // `chat.meMessage` has no threads: there Slack decides.
            "/me" if thread.is_none() => {
                let text = escape(args);
                self.spawn(move |s| {
                    let res = s.call("chat.meMessage", &[("channel", &chan), ("text", &text)]).map(|_| None);
                    Event::Slash(Ev::Ran { chan, thread: None, line, res })
                });
            }
            "/away" => {
                self.toggle_away();
                let note = if self.account.away { tr("You're set to away", "Você está ausente") } else { tr("You're set to active", "Você está ativo") };
                self.brief(note.to_string(), 4);
            }
            "/active" => {
                if self.account.away {
                    self.toggle_away();
                }
                self.brief(tr("You're set to active", "Você está ativo").to_string(), 4);
            }
            "/status" if args.is_empty() => self.open_status_dialog(),
            "/status" if args.eq_ignore_ascii_case("clear") => {
                self.clear_status();
                self.brief(tr("Status cleared", "Status limpo").to_string(), 4);
            }
            "/status" => {
                // `:emoji: text`, the emoji optional.
                let (emoji, text) = match args.strip_prefix(':').and_then(|r| r.split_once(':')) {
                    Some((e, rest)) if !e.is_empty() && !e.contains(char::is_whitespace) => (format!(":{e}:"), rest.trim().to_string()),
                    _ => (String::new(), args.to_string()),
                };
                self.save_status(emoji, text, 0);
                self.brief(tr("Status set", "Status definido").to_string(), 4);
            }
            "/dnd" => {
                let off = matches!(args.to_lowercase().as_str(), "off" | "end" | "stop" | "desligar" | "parar");
                if off || (args.is_empty() && self.account.paused.is_some()) {
                    self.resume_everywhere();
                    self.brief(tr("Notifications resumed", "Notificações retomadas").to_string(), 4);
                } else if let Some(m) = minutes(args).filter(|m| *m > 0) {
                    self.pause_notifications(m);
                    self.brief(trf!("Notifications paused for {m} min", "Notificações pausadas por {m} min"), 4);
                } else {
                    self.error = Some(tr("/dnd: say for how long, like \"/dnd 30 minutes\" or \"/dnd 2 hours\"", "/dnd: diga por quanto tempo, como \"/dnd 30 minutos\" ou \"/dnd 2 horas\"").into());
                }
            }
            "/topic" if !args.is_empty() => {
                let topic = args.to_string();
                self.spawn(move |s| {
                    let res = s.call("conversations.setTopic", &[("channel", &chan), ("topic", &topic)]).map(|_| None);
                    Event::Slash(Ev::Ran { chan, thread: None, line, res })
                });
            }
            "/open" | "/join" => {
                // `<#C1>` from the `#` autocomplete, or a name typed out.
                let arg = args.trim_start_matches('#');
                let id = arg.strip_prefix("<#").and_then(|r| r.split(['>', '|']).next()).map(String::from).or_else(|| {
                    self.convs.iter().find(|c| matches!(c.kind, Kind::Channel | Kind::Private) && c.name == arg).map(|c| c.id.clone())
                });
                match id.filter(|id| self.convs.iter().any(|c| &c.id == id)) {
                    Some(id) => self.open(id),
                    None => return false,
                }
            }
            "/msg" => {
                // `<@U1>` from the `@` autocomplete, or `@handle` typed out.
                let (who, text) = args.split_once(char::is_whitespace).map_or((args, ""), |(w, t)| (w, t.trim()));
                let uid = who.strip_prefix("<@").and_then(|r| r.split(['>', '|']).next()).map(String::from).or_else(|| self.users.handles.get(who.trim_start_matches('@')).cloned());
                let Some(uid) = uid else { return false };
                if text.is_empty() {
                    self.open_dm(uid);
                    return true;
                }
                let name = self.users.names.get(&uid).cloned().unwrap_or_else(|| uid.clone());
                let text = text.to_string();
                self.spawn(move |s| {
                    let res = s.open_conversation(&[uid]).and_then(|c| s.post(&c.id, &text)).map(|_| Some(trf!("✓ Sent to {name}", "✓ Enviada para {name}")));
                    Event::Slash(Ev::Ran { chan, thread: None, line, res })
                });
            }
            _ => return false,
        }
        true
    }

    pub(crate) fn on_slash(&mut self, ev: Ev) {
        match ev {
            Ev::List(Ok(c)) => self.slash.commands = c,
            // The cached list stays; without one Slack decides what a `/` is.
            Ev::List(Err(_)) => {}
            Ev::Ran { res: Ok(said), .. } => {
                if let Some(said) = said {
                    let first = said.lines().next().unwrap_or("").to_string();
                    self.brief(first, 8);
                }
            }
            Ev::Ran { chan, thread, line, res: Err(e) } => {
                self.error = Some(e);
                // Back in its box, to fix and send again.
                let look = Lookup { users: &self.users, chans: &self.chans, emoji: &self.emoji, pictures: self.picker.is_some() };
                let ed = match &thread {
                    Some(ts) => self.thread.as_mut().filter(|t| t.chan == chan && t.ts == *ts).map(|t| &mut t.editor),
                    None => Some(&mut self.editor).filter(|_| self.current.as_deref() == Some(chan.as_str())),
                };
                if let Some(ed) = ed.filter(|e| e.is_empty()) {
                    ed.load(&line, false, &look);
                }
            }
        }
    }

    /// An answer only you see, from the websocket: shown where it belongs
    /// until the app quits.
    pub(crate) fn on_ephemeral(&mut self, chan: String, mut m: Message) {
        m.subtype = Some(EPHEMERAL.into());
        if m.ts.is_empty() {
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
            m.ts = format!("{}.{:06}", now.as_secs(), now.subsec_micros());
        }
        match m.thread_ts.clone().filter(|t| *t != m.ts) {
            Some(parent) => {
                if let Some(t) = self.thread.as_mut().filter(|t| t.chan == chan && t.ts == parent) {
                    insert(&mut t.msgs, m);
                }
            }
            None => {
                if let Some(list) = self.messages.get_mut(&chan) {
                    insert(list, m);
                }
            }
        }
        self.dirty = true;
    }
}

/// The popup, just above the box being typed in: each command with its
/// usage, and under it who provides it and what it does.
pub fn draw(f: &mut Frame, app: &mut App) {
    app.slash.rect = Rect::default();
    app.slash.rows.clear();
    let Some(hits) = app.slash_hits() else { return };
    let input = app.typing_area();
    let widest = hits.iter().map(|c| (c.name.width() + c.usage.width() + 2).max(c.app.width() + c.desc.width() + 8)).max().unwrap_or(0) as u16;
    let w = (widest + 6).clamp(40, 96).min(input.width.saturating_sub(2));
    let shown = SHOWN.min(hits.len()).min((input.y.saturating_sub(2) / 2) as usize);
    if shown == 0 || w < 20 {
        return;
    }
    let h = shown as u16 * 2 + 2;
    let rect = Rect::new(input.x + 1, input.y - h, w, h);
    f.render_widget(Clear, rect);
    let block = Block::bordered().border_type(BorderType::Rounded).border_style(dim());
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    app.slash.rect = rect;
    // Scrolled so the selected command is in view.
    let sel = app.slash.sel;
    let top = app.slash.top.min(hits.len() - shown);
    let top = if sel < top { sel } else if sel >= top + shown { sel + 1 - shown } else { top };
    app.slash.top = top;
    let pictures = app.picker.is_some();
    let width = inner.width as usize;
    for (n, c) in hits.iter().enumerate().skip(top).take(shown) {
        let y = inner.y + (n - top) as u16 * 2;
        let photo = c.icon.as_ref().filter(|_| pictures && !c.app_id.is_empty()).map(|url| ImgRef {
            key: format!("app-{}", c.app_id),
            url: url.clone(),
            name: String::new(),
            link: None,
        });
        let icon = if photo.is_some() { Span::raw("  ") } else { Span::styled(format!("{PROMPT} "), dim()) };
        let name = truncate(&c.name, width.saturating_sub(5));
        let room = width.saturating_sub(4 + name.width());
        let usage = if c.usage.is_empty() { String::new() } else { truncate(&format!(" {}", c.usage), room) };
        let mut first = Line::from(vec![Span::raw(" "), icon, Span::raw(" "), Span::styled(name, Style::new().bold()), Span::styled(usage, dim())]);
        let who = if c.app.is_empty() { "Slack".to_string() } else { format!("{} · {}", tr("App", "App"), c.app) };
        let about = if c.desc.is_empty() { who } else { format!("{who} · {}", c.desc) };
        let mut second = Line::from(vec![Span::raw("    "), Span::styled(truncate(&about, width.saturating_sub(5)), dim())]);
        for line in [&mut first, &mut second] {
            let pad = width.saturating_sub(line.width());
            line.spans.push(Span::raw(" ".repeat(pad)));
            if n == sel {
                mark_selected(line);
            }
        }
        let area = Rect::new(inner.x, y, inner.width, 2);
        f.render_widget(Paragraph::new(vec![first, second]), area);
        app.slash.rows.push((area, n));
        if let Some(img) = photo {
            draw_image(f, app, &img, Rect::new(area.x + 1, area.y, 2, 1));
        }
    }
}
