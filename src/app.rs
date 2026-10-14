//! App state, input handling and the background requests that feed it.

use crate::cache::{self, Cache};
use crate::editor::Editor;
use crate::emoji;
use crate::config;
use crate::files;
use crate::i18n::tr;
use crate::slack::{AuthInfo, Boot, Conversation, Count, Creds, Directory, File, Kind, Message, Res, Profile, Section, Slack, Users, HISTORY_LIMIT};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use image::DynamicImage;
use ratatui::layout::{Position, Rect};
use ratatui::widgets::ListState;
use ratatui_image::picker::Picker;
use ratatui_image::protocol::Protocol;
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

pub(crate) const POLL_EVERY: Duration = Duration::from_secs(3);
/// Slack repeats "typing" every few seconds while someone types; this long
/// after the last one, they stopped.
const TYPING: Duration = Duration::from_secs(6);
/// How long "copied to clipboard" stays beside what was copied.
const COPIED_FOR: Duration = Duration::from_secs(2);
/// With the websocket up, polls only catch what it might have missed.
const LIVE_FALLBACK: Duration = Duration::from_secs(60);
/// Presence polling, for when the websocket can't report it.
const PRESENCE_EVERY: Duration = Duration::from_secs(60);
/// Everyone's names, photos and statuses read again: the websocket of a
/// browser session never says when someone changes them. Two pages or so.
const USERS_EVERY: Duration = Duration::from_secs(2 * 60);
/// The same for a workspace behind another, and its pause.
const USERS_EVERY_BACKGROUND: Duration = Duration::from_secs(15 * 60);
const DND_EVERY_BACKGROUND: Duration = Duration::from_secs(5 * 60);
/// A background workspace's start: the rest of `refresh` this long after the light part.
const REST_AFTER: Duration = Duration::from_secs(10);
/// How soon a change in the sidebar's DMs reaches the presence subscription.
const WATCH_EVERY: Duration = Duration::from_secs(5);
const COUNTS_EVERY: Duration = Duration::from_secs(15);
/// The workspace's public channels (`Directory`) are read again this often.
const DIRECTORY_EVERY: Duration = Duration::from_secs(6 * 3600);
/// How long after start they're asked for: the sidebar's own requests go first.
const DIRECTORY_AFTER: Duration = Duration::from_secs(5);
/// Channels you are not in shown by a search, at most.
const OUTSIDE_HITS: usize = 50;
const MAX_RECENTS: usize = 8;
/// Conversations Alt+← can go back through.
const MAX_HISTORY: usize = 100;
/// How long a message opened from "Fixados" stays highlighted.
const FLASH: Duration = Duration::from_secs(3);
/// How long a status line or an error stays under the message box.
const NOTICE_FOR: Duration = Duration::from_secs(10);
/// Without Slack's list of open DMs: how many the section shows, most recent first.
const DM_LIMIT: usize = 15;
/// The key of the section slick adds last for the channels muted in Slack.
/// Not one of Slack's: it can't be moved, nor have conversations moved in.
pub const SILENCED: &str = "slick:silenced";
pub const SIDEBAR_DEFAULT: u16 = 32;
pub const SIDEBAR_MIN: u16 = 18;

pub enum Event {
    Auth(Res<AuthInfo>),
    Users(Res<Users>),
    Convs(Res<Vec<Conversation>>),
    Sections(Res<Vec<Section>>),
    Stars(Res<Vec<String>>),
    Vips(Res<HashSet<String>>),
    VipSaved(Res<()>),
    Counts(Res<HashMap<String, Count>>),
    History { chan: String, fetch: Fetch, res: Res<Vec<Message>> },
    /// The history around `ts`; whether it reaches the first and the newest message.
    Around { chan: String, ts: String, res: Res<(Vec<Message>, bool, bool)> },
    Sent { chan: String, text: String, res: Res<Message> },
    /// `anim` holds every frame when the picture is an animated emoji.
    Image { key: String, res: Res<DynamicImage>, anim: Option<emoji::Anim> },
    Presence(HashMap<String, bool>),
    Profile { uid: String, res: Res<Profile> },
    SectionMoved(Res<()>),
    Boot(Res<Boot>),
    TeamIcon(Res<String>),
    /// A conversation moved, starred or closed; on failure the sidebar reloads.
    SidebarSaved(Res<()>),
    /// The result of checking credentials typed into the login screen.
    Login(Res<(AuthInfo, Creds)>),
    /// A DM opened or a channel created from the "new" button; `warning`
    /// when the channel exists but its description was not saved.
    Composed { res: Res<Conversation>, warning: Option<String> },
    /// A thread's messages, parent first.
    Replies { chan: String, ts: String, res: Res<Vec<Message>> },
    /// A reply sent from the thread panel.
    Replied { chan: String, ts: String, text: String, res: Res<Message> },
    Emoji(emoji::Ev),
    /// A file fetched into the cache to open with the system's app.
    Opened(Res<std::path::PathBuf>),
    /// Edits, pins, saves and the rest of the message menu.
    Edit(crate::edit::Ev),
    /// A file saved to the downloads folder, keyed by `files::Job::key`.
    Downloaded { key: String, res: Res<std::path::PathBuf> },
    /// A conversation's pinned messages.
    Pins { chan: String, res: Res<Vec<crate::pins::Pin>> },
    Account(crate::account::Ev),
    /// A newer release, installed or only announced.
    Update(crate::update::Note),
    /// Quitting from outside the terminal, like the tray's menu.
    Quit,
    /// A command from another process, through the socket (`ipc`).
    Remote(crate::ipc::Cmd),
    /// Slack's websocket: something changed (`rtm`).
    Live(crate::rtm::Ev),
    /// A conversation that appeared after the list was loaded (`events::on_counts`).
    Joined(Res<Conversation>),
    /// A conversation's unread messages, for the preview other programs see (`events`).
    Preview { chan: String, latest: String, res: Res<Vec<Message>> },
    /// Each channel's notification preference: muted, and its desktop level.
    NotifyPrefs(Res<HashMap<String, crate::events::Pref>>),
    /// A pasted picture read from the clipboard, or pictures sent (`attach`).
    Attach(crate::attach::Ev),
    /// A person or channel a message mentioned: the name, and whether it's private.
    Named { id: String, res: Res<(String, bool)> },
    /// A channel a message mentioned, looked up (`request_names`).
    ChanNamed { id: String, res: Res<Conversation> },
    /// The DM opened by clicking a mention.
    Dm(Res<Conversation>),
    /// Slash commands: the list, and one run (`slash`).
    Slash(crate::slash::Ev),
    /// The channel details popup: its data and the changes made from it.
    Details(crate::details::Ev),
    /// An archived channel brought back (`App::unarchive`).
    Unarchived { id: String, res: Res<()> },
    /// The workspace's public channels, for the search (`Directory`).
    Directory(Res<Directory>),
    /// A channel joined from its preview (`App::join`).
    Entered { id: String, res: Res<Conversation> },
    /// The search window's answers: messages found, previews (`search`).
    Search(crate::search::Ev),
    /// The threads you follow: their counts, the list, one marked read (`threads`).
    Threads(crate::threads::Ev),
    /// "Invite people": your address's domain and the invitations sent (`invite`).
    Invite(crate::invite::Ev),
}

/// Why the open conversation has a notice instead of a message box.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReadOnly {
    /// A DM with a deactivated account: only its archives.
    Deactivated,
    /// An archived channel, until someone unarchives it.
    Archived,
    /// A public channel the user is not in, opened from the search: it
    /// takes messages once joined.
    Outside,
    /// An archived channel the user is not in: only its members could unarchive it.
    OutsideArchived,
}

/// The login screen: the two values copied from a browser session.
pub struct Login {
    /// Typed at the prompt: the workspace whose email-and-password form opens.
    pub workspace: String,
    pub token: String,
    pub cookie: String,
    /// 0 = token, 1 = cookie.
    pub field: usize,
    pub message: Option<String>,
    pub checking: bool,
    /// When the screen opened, for the name printing in.
    pub shown: Instant,
}

impl Default for Login {
    fn default() -> Self {
        Self {
            workspace: String::new(),
            token: String::new(),
            cookie: String::new(),
            field: 0,
            message: None,
            checking: false,
            shown: Instant::now(),
        }
    }
}

/// Slack's answers that mean the credentials are no longer good.
fn is_auth_error(e: &str) -> bool {
    ["invalid_auth", "not_authed", "token_revoked", "token_expired", "account_inactive"].iter().any(|k| e.ends_with(k))
}

/// An image to fetch and show: a thumbnail, an original or an avatar.
#[derive(Clone)]
pub struct ImgRef {
    /// Cache key, also the file name on disk.
    pub key: String,
    pub url: String,
    pub name: String,
    /// Where "ver no navegador" goes.
    pub link: Option<String>,
}

impl ImgRef {
    pub fn thumb(f: &File) -> Option<Self> {
        Some(Self { key: f.id.clone(), url: f.thumb.clone()?, name: f.name.clone(), link: f.permalink.clone() })
    }

    /// The original, keeping its extension so other apps can open the file.
    pub fn full(f: &File) -> Option<Self> {
        let ext = f.name.rsplit_once('.').map(|(_, e)| format!(".{e}")).unwrap_or_default();
        Some(Self { key: format!("full-{}{ext}", f.id), url: f.full.clone()?, name: f.name.clone(), link: f.permalink.clone() })
    }

    pub fn avatar(uid: &str, url: &str) -> Self {
        Self { key: format!("avatar-{uid}"), url: url.to_string(), name: String::new(), link: None }
    }

    /// The photo sharp enough for `px` pixels: Slack's 72px one, or 192px past that.
    pub fn avatar_px(uid: &str, url: &str, px: u32) -> Self {
        if px > 72 { Self::avatar_big(uid, url) } else { Self::avatar(uid, url) }
    }

    /// A sharper copy for the large avatar at the top of a conversation.
    pub fn avatar_big(uid: &str, url: &str) -> Self {
        let url = url.replace("_72.", "_192.").replace("s=72", "s=192");
        Self { key: format!("avatar-big-{uid}"), url, name: String::new(), link: None }
    }

    /// The profile panel's photo.
    pub fn profile(uid: &str, url: &str) -> Self {
        Self { key: format!("avatar-512-{uid}"), url: url.to_string(), name: String::new(), link: None }
    }

    /// The photo at full size, keeping its extension so other apps can open it.
    pub fn photo(uid: &str, url: &str, name: &str, link: String) -> Self {
        let ext = url.rsplit_once('.').map(|(_, e)| format!(".{e}")).filter(|e| e.len() <= 5).unwrap_or_default();
        Self { key: format!("photo-{uid}{ext}"), url: url.to_string(), name: name.to_string(), link: Some(link) }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum Fetch {
    Initial,
    Poll,
    Older,
    /// After the last message of a history away from the present.
    Newer,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Focus {
    Sidebar,
    Filter,
    Input,
    /// The reply box of the thread panel.
    Thread,
}

pub enum Row {
    /// "Threads", on top: the threads you follow (`threads`).
    Threads,
    /// A blank line between sections.
    Gap,
    Header { key: String, title: String, collapsed: bool, unread: bool },
    Conv(usize),
    /// A search result among the channels the user is not in, an index in `directory`.
    Other(usize),
}

/// What a context-menu entry does.
#[derive(Clone)]
pub enum Action {
    /// A file card's menu entry.
    File(files::Click),
    Separator,
    MarkRead(String),
    Star(String, bool),
    /// Opens the list of sections to move the conversation to.
    MoveMenu(String),
    MoveTo(String, String),
    CopyName(String),
    CopyLink(String),
    Browser(String),
    Close(String),
    SectionUp(String),
    SectionDown(String),
    Collapse(String),
    NewMessage,
    NewChannel,
    /// The message menu's entries.
    Message(crate::edit::Item),
}

/// The right-click menu; a submenu keeps the menu it came from.
pub struct Menu {
    pub x: u16,
    pub y: u16,
    pub items: Vec<(String, Action)>,
    pub sel: usize,
    pub parent: Option<Box<Menu>>,
}

impl Menu {
    pub(crate) fn new(x: u16, y: u16, items: Vec<(String, Action)>) -> Self {
        let sel = items.iter().position(|(_, a)| !matches!(a, Action::Separator)).unwrap_or(0);
        Self { x, y, items, sel, parent: None }
    }

    fn step(&mut self, delta: isize) {
        let n = self.items.len() as isize;
        let mut i = self.sel as isize;
        for _ in 0..n {
            i = (i + delta).rem_euclid(n);
            if !matches!(self.items[i as usize].1, Action::Separator) {
                break;
            }
        }
        self.sel = i as usize;
    }
}

/// A sidebar row being dragged: where it started and where it would land.
#[derive(Clone, Copy)]
pub struct Drag {
    pub from: usize,
    pub over: Option<usize>,
}

pub enum Img {
    Loading,
    Ready(DynamicImage),
    Failed,
}

/// Where the last draw put things, for mouse clicks.
#[derive(Default)]
pub struct Areas {
    pub search: Rect,
    pub sidebar: Rect,
    pub messages: Rect,
    pub input: Rect,
    /// "Unarchive" in an archived channel's notice, where the message box goes.
    pub unarchive: Rect,
    /// Message images on screen, clickable to enlarge.
    pub images: Vec<(Rect, ImgRef)>,
    /// Avatars on screen, clickable to open the person's profile.
    pub avatars: Vec<(Rect, String)>,
    /// Links and Block Kit buttons in messages.
    pub buttons: Vec<(Rect, crate::blocks::Target)>,
    /// The profile panel's buttons and photo.
    pub profile_close: Rect,
    pub profile_browser: Rect,
    pub profile_photo: Rect,
    pub profile_star: Rect,
    pub profile_vip: Rect,
    pub profile_id: Rect,
    /// The email address, click to copy.
    pub profile_email: Rect,
    /// "Edit", on your own profile.
    pub profile_edit: Rect,
    /// The workspace name (shows the address) and the address itself.
    pub team_name: Rect,
    pub team_url: Rect,
    pub team_id: Rect,
    pub team_popup: Rect,
    /// The star before the conversation's name.
    pub header_star: Rect,
    /// A DM's name in the header, opening the person's profile.
    pub header_user: Option<(Rect, String)>,
    /// A channel's or group DM's name in the header, opening its details.
    pub header_name: Rect,
    pub login_button: Rect,
    /// The column between the sidebar and the conversation, draggable.
    pub sidebar_border: Rect,
    pub menu_items: Vec<(Rect, usize)>,
    pub menu: Rect,
    pub viewer: Rect,
    pub viewer_link: Rect,
    /// The "new" button and its dialogs.
    pub compose: crate::compose::Areas,
    /// "N respostas" and "respondeu a uma conversa": click to open the thread.
    pub thread_links: Vec<(Rect, String)>,
    /// The thread panel, its parts and its buttons.
    pub thread: Rect,
    pub thread_close: Rect,
    pub thread_input: Rect,
    pub thread_broadcast: Rect,
    /// The `:` autocomplete rows over the message box.
    pub emoji_rows: Vec<(Rect, usize)>,
    /// The reaction picker and its rows.
    pub picker: Rect,
    pub picker_rows: Vec<(Rect, usize)>,
    /// Reaction pills: the message's ts and the emoji (`None`: the "+" pill).
    pub reactions: Vec<(Rect, String, Option<String>)>,
    /// Each message's lines on screen, for the right click that reacts.
    pub message_rows: Vec<(Rect, String)>,
    pub viewer_save: Rect,
    /// File cards' actions: Baixar, Abrir, Mostrar na pasta.
    pub file_buttons: Vec<(Rect, files::Click)>,
    /// The header's "Mensagens" and "Fixados" tabs.
    pub pins: crate::pins::Areas,
    /// "↓ Últimas mensagens", over the bottom of the conversation.
    pub latest: Rect,
    /// "↑ Unread mentions" (or "↑ More unreads") over the sidebar's edges, and the sidebar row each leads to.
    pub mention_pills: Vec<(Rect, usize)>,
    /// The account button, its menu and the status dialog.
    pub account: crate::account::Areas,
    /// The channel details popup.
    pub details: crate::details::Areas,
    /// The Threads view's cards.
    pub threads: crate::threads::Areas,
    /// The invite modal.
    pub invite: crate::invite::Areas,
    /// The workspace switcher's rows and "Add a workspace".
    #[allow(dead_code)]
    pub switcher: crate::switcher::Areas,
}

/// Slack's red "New" line: over the first message someone else sent after
/// where the conversation was read up to on opening. Found once, it stays
/// put while the conversation is open; messages arriving meanwhile get none.
pub struct NewLine {
    pub chan: String,
    /// Read up to here on opening.
    after: String,
    /// The message the line goes over; `None` with nothing unread.
    pub at: Option<String>,
    /// The open's fetch landed: what it found stays.
    fetched: bool,
    /// The next draw scrolls up to it, if it is above the view.
    pub jump: bool,
}

impl NewLine {
    /// Right over the message `ts`: "Mark unread" from it.
    pub fn over(chan: String, ts: String) -> Self {
        NewLine { chan, after: String::new(), at: Some(ts), fetched: true, jump: false }
    }
}

pub struct App {
    slack: Slack,
    tx: Sender<Event>,
    pub me: Option<AuthInfo>,
    pub users: Users,
    pub chans: HashMap<String, String>,
    /// Private channels, which messages link with a lock.
    pub private: HashSet<String>,
    /// Names of people and channels outside `users` and `convs`, looked up
    /// when a message on screen mentions them.
    pub others: HashMap<String, String>,
    /// Mentions the last draw could not name; asked for once each.
    pub unnamed: Vec<crate::blocks::Target>,
    asked: HashSet<String>,
    pub convs: Vec<Conversation>,
    /// The workspace's public channels, for the search to find those you
    /// are not in; read in the background (`fetch_directory`).
    pub directory: Directory,
    /// The workspace `directory` was loaded from disk for.
    dir_team: Option<String>,
    dir_loading: bool,
    /// Ctrl+R: read it again however fresh.
    dir_force: bool,
    /// Bumped at each new `directory`, so `dir_hits` knows it is stale.
    pub(crate) dir_gen: u64,
    /// The last search over `directory`: (query, `dir_gen`, (tier, index) best first).
    dir_hits: std::cell::RefCell<(String, u64, Vec<(u8, usize)>)>,
    /// The search window (Ctrl+K): conversations and messages (`search`).
    pub search: crate::search::Search,
    pub sections: Vec<Section>,
    pub stars: Vec<String>,
    pub vips: HashSet<String>,
    pub team_icon: Option<String>,
    /// The popup with the workspace's address and ID, opened from its name.
    pub team_popup: bool,
    pub counts: HashMap<String, Count>,
    pub collapsed: HashSet<String>,
    /// Silenced starts folded each time; its fold is kept here, not in the cache.
    pub silenced_open: bool,
    pub recents: Vec<String>,
    /// Conversations in the order they were opened, for Alt+← and Alt+→,
    /// and where in it the one on screen is.
    history: Vec<String>,
    history_at: usize,
    pub messages: HashMap<String, Vec<Message>>,
    pub current: Option<String>,
    pub focus: Focus,
    pub list: ListState,
    /// The selected row as of the last draw (index, what it showed), so the
    /// cursor follows its row when the sidebar reorders under it.
    sel_row: Option<(usize, String)>,
    /// Set by the mouse wheel: the list stays where it was scrolled instead of
    /// jumping back to the selected row; moving the selection clears it.
    pub sidebar_wheel: bool,
    pub filter: String,
    pub editor: Editor,
    /// Set by the last draw: the text width of the message box.
    pub input_width: usize,
    /// Lines scrolled up from the newest message.
    pub scroll: usize,
    /// Set by the last draw: how far `scroll` can go.
    pub max_scroll: usize,
    /// Messages loaded below the view: the next draw keeps this message where
    /// it was, `scroll` lines over its end.
    pub stay: Option<(String, usize)>,
    /// A message just jumped to, highlighted for a moment.
    pub flash: Option<(String, Instant)>,
    /// Slack's red "New" line in the open conversation.
    pub new_line: Option<NewLine>,
    /// Conversations whose loaded history is a stretch that does not reach
    /// the newest message; polls leave it alone until they meet it.
    pub detached: HashSet<String>,
    pub areas: Areas,
    pub picker: Option<Picker>,
    /// Keyed by `ImgRef::key`.
    pub images: HashMap<String, Img>,
    /// Encoded for the terminal, per (key, columns, rows).
    pub protocols: HashMap<(String, u16, u16), Protocol>,
    /// Animated emoji frames, keyed like `images`; frame `i` is encoded under `key#i`.
    pub anims: HashMap<String, emoji::Anim>,
    /// Animations all run on this clock.
    pub clock: Instant,
    /// Set by the last draw: when a visible animation shows its next frame.
    pub next_frame: Option<Instant>,
    /// Images the last draw wanted but did not have.
    pub wanted: Vec<ImgRef>,
    /// The enlarged image, drawn over everything.
    pub viewer: Option<ImgRef>,
    /// Files being saved or saved this session, keyed by `files::Job::key`.
    pub downloads: HashMap<String, files::State>,
    /// The profile panel on the right: whose, and the data once it arrives.
    pub profile: Option<(String, Option<Profile>)>,
    /// The thread panel, in the same slot as the profile: one or the other.
    pub thread: Option<crate::thread::Thread>,
    /// The "Fixados" tab and each conversation's pins.
    pub pins: crate::pins::Pins,
    pub presence: HashMap<String, bool>,
    last_presence: Instant,
    last_users: Instant,
    pub boot: Option<Boot>,
    pub sidebar_width: u16,
    /// Photos instead of presence dots next to DMs in the sidebar.
    pub sidebar_avatars: bool,
    /// Messages back to back, without the blank row between them.
    pub compact: bool,
    pub menu: Option<Menu>,
    pub emoji: emoji::Emoji,
    pub complete: emoji::Complete,
    /// The `@` and `#` autocomplete over the message box.
    pub mention: crate::mention::Mention,
    /// The workspace's slash commands and the `/` autocomplete.
    pub slash: crate::slash::Slash,
    pub react: Option<emoji::ReactPicker>,
    /// The message under the mouse (thread panel?, ts): it shows the add-reaction button.
    pub hover_msg: Option<(bool, String)>,
    /// The mouse, while over a reaction pill: the card of who reacted.
    pub hover_pill: Option<Position>,
    /// The picked message, the edit and the forward dialog.
    pub edit: crate::edit::Edit,
    /// Messages dragged over with the mouse, copied on release.
    pub selection: Option<crate::selection::Selection>,
    pub drag: Option<Drag>,
    resizing: bool,
    pub login: Option<Login>,
    /// The new-message or new-channel dialog, over everything else.
    pub dialog: Option<crate::compose::Dialog>,
    pub compose_hover: bool,
    /// Your presence, pause and status, and the menu over the account button.
    pub account: crate::account::Account,
    /// Pictures pasted into the message boxes, waiting to be sent.
    pub attach: crate::attach::Attach,
    /// The channel details popup, opened from the header's name.
    pub details: Option<crate::details::Details>,
    /// The threads you follow, and their view in the main pane.
    pub threads: crate::threads::Threads,
    /// "Invite people to …", opened from the account menu.
    pub invite: Option<crate::invite::Invite>,
    /// The keyboard shortcuts guide (Ctrl+/), over everything but sign-in.
    pub help: Option<crate::help::Help>,
    /// Whose data the cache holds.
    owner: String,
    pub status: String,
    /// A status that clears itself: the text and when it goes.
    pub status_until: Option<(String, Instant)>,
    /// The status line and the error as last seen, and since when: they go after `NOTICE_FOR`.
    notice_seen: (String, Option<String>, Instant),
    /// What was just copied from a card (`id`, `email`, `team`), shown
    /// beside it for a moment since the status line sits under the popup.
    pub copied: Option<(&'static str, Instant)>,
    /// The code block behind `copied` = `code`.
    pub copied_code: String,
    /// A newer version, shown as the button by your name.
    pub update: Option<crate::update::State>,
    /// Quitting to run the updated binary.
    pub restart: bool,
    pub error: Option<String>,
    loading: HashSet<String>,
    /// Conversations whose history is loaded back to the first message.
    pub no_older: HashSet<String>,
    polling: bool,
    last_poll: Instant,
    last_counts: Instant,
    /// The terminal has the keyboard. While it doesn't, the open
    /// conversation is not marked read and counts as unread like any other.
    /// Off until the terminal says so or a key or click arrives: started
    /// hidden (at login, in a scratchpad) nothing gets read unseen.
    pub focused: bool,
    /// What other programs are told: the snapshot and events (`events`).
    pub events: crate::events::State,
    /// Slack's websocket is up: polling only covers for it, slowly.
    pub(crate) live: bool,
    /// The websocket reported presence since it came up: no need to poll it.
    presence_live: bool,
    last_watch: Instant,
    /// Who is typing where (conversation → person → when last heard), from the websocket.
    pub typing: HashMap<String, HashMap<String, Instant>>,
    rtm_started: bool,
    /// The counts asked for by the websocket, a moment after its first nudge,
    /// so a burst of messages costs one request.
    counts_at: Option<Instant>,
    /// Another workspace is on screen: this one keeps only what feeds its badge.
    pub background: bool,
    /// Every signed-in workspace, for the switcher; filled by `Workspaces` before each draw.
    pub workspaces: Vec<crate::switcher::WsInfo>,
    /// A switch asked for by a key or click, carried out by `Workspaces`.
    pub switch_req: Option<crate::switcher::SwitchReq>,
    /// Started by "Add a workspace": its sign-in is a new workspace, which
    /// `Workspaces` keeps (or merges into one already there).
    pub adding: bool,
    /// That sign-in, checked, for `Workspaces` to decide on.
    pub added: Option<(AuthInfo, Creds)>,
    /// Esc on the "add" sign-in: `Workspaces` drops this one.
    pub discard: bool,
    /// Signed out with other workspaces left: `Workspaces` drops this one.
    pub signed_out: bool,
    /// A pause set or ended here, which `Workspaces` applies in every workspace.
    pub dnd_all: Option<crate::pause::DndAll>,
    /// A config from before multi-workspace: its session, saved again with
    /// the workspace's id (`config::team`) at the first `auth.test`.
    pub legacy: Option<Creds>,
    /// A workspace opened behind another: when it starts loading, then the rest.
    start_at: Option<Instant>,
    rest_at: Option<Instant>,
    /// The conversation it was left on, opened once it comes on screen.
    reopen: Option<String>,
    pub dirty: bool,
    pub quit: bool,
}

impl App {
    fn build(slack: Slack, tx: Sender<Event>, cache: Cache, picker: Option<Picker>) -> Self {
        Self {
            slack,
            tx,
            me: None,
            users: cache.users,
            chans: HashMap::new(),
            private: HashSet::new(),
            others: HashMap::new(),
            unnamed: Vec::new(),
            asked: HashSet::new(),
            convs: cache.convs,
            directory: Directory::default(),
            dir_team: None,
            dir_loading: false,
            dir_force: false,
            dir_gen: 0,
            dir_hits: Default::default(),
            search: Default::default(),
            sections: cache.sections,
            stars: cache.stars,
            vips: HashSet::new(),
            counts: HashMap::new(),
            collapsed: cache.collapsed,
            silenced_open: false,
            recents: cache.recents,
            history: Vec::new(),
            history_at: 0,
            messages: cache.messages,
            current: None,
            focus: Focus::Sidebar,
            list: ListState::default(),
            sel_row: None,
            sidebar_wheel: false,
            filter: String::new(),
            editor: Editor::default(),
            input_width: 40,
            scroll: 0,
            max_scroll: 0,
            stay: None,
            flash: None,
            new_line: None,
            detached: HashSet::new(),
            areas: Areas::default(),
            picker,
            images: HashMap::new(),
            protocols: HashMap::new(),
            anims: HashMap::new(),
            clock: Instant::now(),
            next_frame: None,
            wanted: Vec::new(),
            viewer: None,
            downloads: HashMap::new(),
            profile: None,
            thread: None,
            pins: Default::default(),
            presence: HashMap::new(),
            last_presence: Instant::now() - PRESENCE_EVERY + Duration::from_secs(4),
            last_users: Instant::now(),
            team_icon: None,
            team_popup: false,
            sidebar_avatars: config::sidebar_avatars().or(cache.boot.as_ref().map(|b| b.sidebar_avatars)).unwrap_or(true),
            compact: config::compact_messages(),
            boot: cache.boot,
            sidebar_width: config::sidebar_width().unwrap_or(SIDEBAR_DEFAULT).max(SIDEBAR_MIN),
            menu: None,
            emoji: cache.emoji,
            complete: emoji::Complete::default(),
            mention: Default::default(),
            slash: crate::slash::Slash::new(cache.commands),
            react: None,
            hover_msg: None,
            hover_pill: None,
            edit: Default::default(),
            selection: None,
            drag: None,
            resizing: false,
            login: None,
            dialog: None,
            compose_hover: false,
            account: Default::default(),
            attach: Default::default(),
            details: None,
            threads: Default::default(),
            invite: None,
            help: None,
            owner: cache.owner,
            status: tr("connecting…", "conectando…").into(),
            update: None,
            restart: false,
            status_until: None,
            notice_seen: (String::new(), None, Instant::now()),
            copied: None,
            copied_code: String::new(),
            error: None,
            loading: HashSet::new(),
            no_older: HashSet::new(),
            polling: false,
            last_poll: Instant::now(),
            last_counts: Instant::now(),
            focused: false,
            events: Default::default(),
            live: false,
            presence_live: false,
            last_watch: Instant::now(),
            typing: HashMap::new(),
            rtm_started: false,
            counts_at: None,
            background: false,
            workspaces: Vec::new(),
            switch_req: None,
            adding: false,
            added: None,
            discard: false,
            signed_out: false,
            dnd_all: None,
            legacy: None,
            start_at: None,
            rest_at: None,
            reopen: None,
            dirty: true,
            quit: false,
        }
    }

    pub fn new(slack: Slack, tx: Sender<Event>, cache: Cache, picker: Option<Picker>, logged_in: bool) -> Self {
        let current = cache.current.clone();
        let mut app = Self::build(slack, tx, cache, picker);
        app.resort();
        if logged_in {
            app.refresh();
            // Back where it was left; a `slick open` arriving next replaces it.
            if let Some(id) = current.filter(|id| app.convs.iter().any(|c| &c.id == id)) {
                app.open(id);
            }
        } else {
            app.open_login(None);
        }
        app
    }

    /// A signed-in workspace behind the one on screen: it starts loading
    /// after `delay`, the little its badge needs first.
    pub fn new_background(slack: Slack, tx: Sender<Event>, cache: Cache, picker: Option<Picker>, delay: Duration) -> Self {
        let current = cache.current.clone();
        let mut app = Self::build(slack, tx, cache, picker);
        app.resort();
        app.background = true;
        app.start_at = Some(Instant::now() + delay);
        app.reopen = current.filter(|id| app.convs.iter().any(|c| &c.id == id));
        app
    }

    /// It comes on screen: what the background skipped is fetched now.
    pub fn activated(&mut self) {
        self.background = false;
        self.dirty = true;
        if self.login.is_some() {
            return;
        }
        // Never started: all of it at once.
        if self.start_at.take().is_some() {
            self.rest_at = None;
            self.refresh();
        } else if self.rest_at.take().is_some() {
            self.refresh_rest();
        }
        let now = Instant::now();
        self.last_presence = now.checked_sub(PRESENCE_EVERY).unwrap_or(now);
        if let Some(id) = self.reopen.take() {
            return self.open(id);
        }
        // What came in while it was behind.
        self.last_poll = now.checked_sub(LIVE_FALLBACK).unwrap_or(now);
        self.refresh_thread();
        self.open_default();
    }

    /// Nothing open yet (a workspace just signed in to, or never left on a
    /// conversation): its default channel, as Slack does, or else the
    /// sidebar's first conversation.
    pub(crate) fn open_default(&mut self) {
        if self.current.is_some() || self.threads.open || self.background || self.login.is_some() || self.reopen.is_some() {
            return;
        }
        let rows = self.rows();
        let general = self.convs.iter().find(|c| !c.archived && (c.general || (c.kind == Kind::Channel && c.name == "general")));
        let first = || rows.iter().find_map(|r| match r {
            Row::Conv(i) => Some(&self.convs[*i]),
            _ => None,
        });
        if let Some(id) = general.or_else(first).map(|c| c.id.clone()) {
            self.open(id);
        }
    }

    pub fn to_cache(&self) -> Cache {
        let messages = self
            .recents
            .iter()
            .filter_map(|id| {
                let msgs = self.messages.get(id)?;
                // Answers only you saw were for this session.
                let msgs: Vec<Message> = msgs.iter().filter(|m| !crate::slash::is_ephemeral(m)).cloned().collect();
                Some((id.clone(), msgs[msgs.len().saturating_sub(50)..].to_vec()))
            })
            .collect();
        Cache {
            owner: self.me.as_ref().map(|m| m.user_id.clone()).unwrap_or_else(|| self.owner.clone()),
            users: self.users.clone(),
            convs: self.convs.iter().filter(|c| !c.guest).cloned().collect(),
            sections: self.sections.clone(),
            stars: self.stars.clone(),
            collapsed: self.collapsed.clone(),
            recents: self.recents.clone(),
            current: self.current.clone(),
            messages,
            boot: self.boot.clone(),
            emoji: self.emoji.clone(),
            commands: self.slash.commands.clone(),
        }
    }

    pub(crate) fn spawn(&self, job: impl FnOnce(&Slack) -> Event + Send + 'static) {
        let (slack, tx) = (self.slack.clone(), self.tx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(job(&slack));
        });
    }

    /// This workspace's client; clones share its session and presence list.
    pub(crate) fn slack(&self) -> &Slack {
        &self.slack
    }

    fn spawn_quiet(&self, job: impl FnOnce(&Slack) + Send + 'static) {
        let slack = self.slack.clone();
        std::thread::spawn(move || job(&slack));
    }

    fn refresh(&mut self) {
        self.spawn(|s| Event::Auth(s.auth()));
        self.spawn(|s| Event::Users(s.users()));
        self.spawn(|s| Event::Convs(s.conversations()));
        self.spawn(|s| Event::Sections(s.sections()));
        self.spawn(|s| Event::Stars(s.stars()));
        self.spawn(|s| Event::Vips(s.vips()));
        self.fetch_counts();
        self.spawn(|s| Event::Boot(s.boot()));
        self.spawn(|s| Event::TeamIcon(s.team_icon()));
        self.spawn(|s| Event::Emoji(emoji::Ev::List(emoji::fetch(s))));
        self.spawn(|s| Event::Slash(crate::slash::Ev::List(crate::slash::fetch(s))));
        self.refresh_account(true);
        self.refresh_prefs();
        self.spawn(|s| Event::Edit(crate::edit::Ev::Saved(s.saved())));
    }

    /// A background workspace's start: what its badge, previews and pause need.
    fn refresh_light(&self) {
        self.spawn(|s| Event::Auth(s.auth()));
        self.spawn(|s| Event::Convs(s.conversations()));
        self.fetch_counts();
        self.refresh_prefs();
        self.spawn(|s| Event::TeamIcon(s.team_icon()));
        self.refresh_dnd();
    }

    /// The rest of `refresh`, a while after `refresh_light`.
    fn refresh_rest(&self) {
        self.spawn(|s| Event::Users(s.users()));
        self.spawn(|s| Event::Sections(s.sections()));
        self.spawn(|s| Event::Stars(s.stars()));
        self.spawn(|s| Event::Vips(s.vips()));
        self.spawn(|s| Event::Boot(s.boot()));
        self.spawn(|s| Event::Emoji(emoji::Ev::List(emoji::fetch(s))));
        self.spawn(|s| Event::Slash(crate::slash::Ev::List(crate::slash::fetch(s))));
        self.refresh_account(true);
        self.spawn(|s| Event::Edit(crate::edit::Ev::Saved(s.saved())));
    }

    /// https://acme.slack.com
    pub fn team_url(&self) -> Option<String> {
        self.me.as_ref().map(|m| m.domain.as_str()).filter(|d| !d.is_empty()).map(|d| format!("https://{d}.slack.com"))
    }

    // ---- conversations -------------------------------------------------

    pub fn names(&self) -> crate::text::Names<'_> {
        crate::text::Names {
            users: &self.users.names,
            chans: &self.chans,
            emoji: &self.emoji.custom,
            others: &self.others,
            private: &self.private,
            directory: &self.directory,
            me: self.me.as_ref().map(|m| m.user_id.as_str()).unwrap_or(""),
        }
    }

    pub fn is_me(&self, uid: &str) -> bool {
        self.me.as_ref().is_some_and(|m| m.user_id == uid)
    }

    /// Whether the open conversation takes no new messages, and why: like
    /// Slack, a deactivated account's DM or an archived channel shows its
    /// history with a notice where the message box goes.
    pub(crate) fn read_only(&self) -> Option<ReadOnly> {
        let c = self.current.as_ref().and_then(|id| self.convs.iter().find(|c| &c.id == id))?;
        if c.kind == Kind::Direct && c.user.as_ref().is_some_and(|u| self.users.deleted.contains(u)) {
            Some(ReadOnly::Deactivated)
        } else if c.guest {
            Some(if c.archived { ReadOnly::OutsideArchived } else { ReadOnly::Outside })
        } else if c.archived {
            Some(ReadOnly::Archived)
        } else {
            None
        }
    }

    /// Unarchives a channel; on success it is a channel like any other again
    /// (sidebar, message box), on failure Slack's error goes to the status line.
    pub fn unarchive(&mut self, id: String) {
        self.status = tr("unarchiving…", "desarquivando…").into();
        self.spawn(move |s| {
            let res = s.unarchive(&id);
            Event::Unarchived { id, res }
        });
    }

    fn unarchived(&mut self, id: String, res: Res<()>) {
        match res {
            Ok(()) => {
                let Some(c) = self.convs.iter_mut().find(|c| c.id == id) else { return };
                c.archived = false;
                let name = c.name.clone();
                self.brief(trf!("#{name} unarchived", "#{name} desarquivado"), 4);
            }
            Err(e) => self.status = e,
        }
    }

    /// Joins the public channel open as a preview; on success it is in the
    /// sidebar and takes messages like any other.
    pub fn join(&mut self, id: String) {
        self.status = tr("joining…", "entrando…").into();
        self.spawn(move |s| {
            let res = s.join(&id);
            Event::Entered { id, res }
        });
    }

    fn entered(&mut self, id: String, res: Res<Conversation>) {
        match res {
            Ok(mut c) => {
                // Some answers lack the id; it is the one asked for.
                if c.id.is_empty() {
                    c.id = id.clone();
                }
                self.convs.retain(|x| x.id != id);
                let name = c.name.clone();
                self.convs.push(c);
                self.resort();
                self.recents.retain(|r| r != &id);
                self.recents.insert(0, id);
                self.recents.truncate(MAX_RECENTS);
                self.status.clear();
                self.brief(trf!("You joined #{name}", "Você entrou em #{name}"), 4);
                self.fetch_counts();
            }
            Err(e) => self.status = e,
        }
    }

    /// The workspace's public channels read again when stale, after a
    /// moment at start; Ctrl+R asks now. A failure waits as long as a success.
    fn fetch_directory(&mut self) {
        let Some(team) = self.me.as_ref().map(|m| m.team_id.clone()) else { return };
        let fresh = crate::slack::unix_now() - self.directory.at < DIRECTORY_EVERY.as_secs() as i64;
        if self.dir_loading || (fresh && !self.dir_force) {
            return;
        }
        let force = std::mem::take(&mut self.dir_force);
        self.dir_loading = true;
        self.spawn(move |s| {
            if !force {
                std::thread::sleep(DIRECTORY_AFTER);
            }
            let res = s.directory().map(|chans| {
                let d = Directory::new(crate::slack::unix_now(), chans);
                cache::save_directory(&team, &d);
                d
            });
            Event::Directory(res)
        });
    }

    fn on_directory(&mut self, res: Res<Directory>) {
        self.dir_loading = false;
        match res {
            Ok(mut d) => {
                d.prepare();
                self.directory = d;
                self.dir_gen += 1;
            }
            Err(_) => self.directory.at = crate::slack::unix_now(),
        }
    }

    /// The channels you are not in that the search matches, best first;
    /// worked out once per query (`dir_hits`), the list can be long.
    fn directory_hits(&self, query: &str, q: &crate::text::Query) -> std::cell::Ref<'_, Vec<(u8, usize)>> {
        let stale = {
            let memo = self.dir_hits.borrow();
            memo.0 != query || memo.1 != self.dir_gen
        };
        if stale {
            let chans = &self.directory.chans;
            let mut ranges = Vec::new();
            let mut hits: Vec<(u8, usize)> =
                chans.iter().enumerate().filter_map(|(j, d)| Some((crate::text::rank(q, &d.folded, &mut ranges)?, j))).collect();
            hits.sort_by_key(|&(t, j)| crate::text::place(t, chans[j].archived, false, chans[j].updated, chans[j].members, chans[j].folded.len()));
            *self.dir_hits.borrow_mut() = (query.to_string(), self.dir_gen, hits);
        }
        std::cell::Ref::map(self.dir_hits.borrow(), |m| &m.2)
    }

    /// When the conversation's last message was, in Unix milliseconds, for
    /// the search's order: from the counts, else the messages loaded, else
    /// when Slack last changed it; 0 when nothing says.
    pub(crate) fn last_message(&self, c: &Conversation) -> i64 {
        let counted = self.counts.get(&c.id).map_or(0, |n| crate::text::ts_ms(&n.latest));
        let loaded = self.messages.get(&c.id).and_then(|m| m.last()).map_or(0, |m| crate::text::ts_ms(&m.ts));
        [counted, loaded].into_iter().max().filter(|t| *t > 0).unwrap_or(c.updated)
    }

    /// The channels opened from the search that you are not in, but `keep`:
    /// a visit only, gone once another conversation opens.
    fn drop_guests(&mut self, keep: &str) {
        let gone: Vec<String> = self.convs.iter().filter(|c| c.guest && c.id != keep).map(|c| c.id.clone()).collect();
        if gone.is_empty() {
            return;
        }
        self.convs.retain(|c| !gone.contains(&c.id));
        for id in &gone {
            self.messages.remove(id);
        }
        self.resort();
    }

    /// " (you)" closing the label of the user's own DM; drawn dimmer than the name.
    pub(crate) fn you_suffix(&self, c: &Conversation) -> Option<&'static str> {
        (c.kind == Kind::Direct && c.user.as_deref().is_some_and(|u| self.is_me(u))).then(|| tr(" (you)", " (você)"))
    }

    /// " (archived)" after an archived channel, " (not a member)" after a
    /// channel you are not in, " (deactivated)" after a deactivated
    /// account's DM, as in Slack's lists; drawn dimmer than the name.
    pub(crate) fn gone_suffix(&self, c: &Conversation) -> Option<&'static str> {
        if c.archived {
            Some(tr(" (archived)", " (arquivado)"))
        } else if c.guest {
            Some(tr(" (not a member)", " (não participa)"))
        } else if c.kind == Kind::Direct && c.user.as_ref().is_some_and(|u| self.users.deleted.contains(u)) {
            Some(tr(" (deactivated)", " (desativada)"))
        } else {
            None
        }
    }

    pub fn label(&self, c: &Conversation) -> String {
        match c.kind {
            Kind::Channel | Kind::Private => c.name.clone(),
            Kind::Direct => {
                let uid = c.user.as_deref().unwrap_or("");
                let name = self.users.names.get(uid).map(String::as_str).unwrap_or(uid);
                format!("{name}{}", self.you_suffix(c).unwrap_or_default())
            }
            Kind::Group => {
                // mpdm-ana--bruno.silva--leonavas-1 → Ana, Bruno Silva
                let s = c.name.trim_start_matches("mpdm-");
                let s = s.rsplit_once('-').map(|(a, _)| a).unwrap_or(s);
                let mine = self.me.as_ref().map(|m| m.user.as_str());
                s.split("--")
                    .filter(|h| Some(*h) != mine)
                    .map(|h| {
                        self.users
                            .handles
                            .get(h)
                            .and_then(|id| self.users.names.get(id))
                            .cloned()
                            .unwrap_or_else(|| h.to_string())
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        }
    }

    /// Keeps `convs` in label order, so sorting indices sorts by name.
    pub(crate) fn resort(&mut self) {
        let mut keyed: Vec<(String, Conversation)> = std::mem::take(&mut self.convs)
            .into_iter()
            .map(|c| (String::new(), c))
            .collect();
        for k in &mut keyed {
            k.0 = self.label(&k.1).to_lowercase();
        }
        keyed.sort_by(|a, b| a.0.cmp(&b.0));
        self.convs = keyed.into_iter().map(|k| k.1).collect();
        self.chans = self.convs.iter().map(|c| (c.id.clone(), c.name.clone())).collect();
        self.private.extend(self.convs.iter().filter(|c| c.kind == Kind::Private).map(|c| c.id.clone()));
    }

    pub fn unread(&self, id: &str) -> (bool, u64) {
        if self.focused && self.current.as_deref() == Some(id) {
            return (false, 0);
        }
        self.counts.get(id).map(|c| (c.unread, c.mentions)).unwrap_or((false, 0))
    }

    /// What the tray lights up for: mentions anywhere and DMs with anything
    /// unread, as the sidebar counts them. Channels without a mention don't count.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn attention(&self) -> (u64, usize) {
        let (mut mentions, mut dms) = (0, 0);
        for c in &self.convs {
            let (unread, m) = self.unread(&c.id);
            mentions += m;
            if unread && matches!(c.kind, Kind::Direct | Kind::Group) {
                dms += 1;
            }
        }
        // Replies in threads you follow that mention you, as Slack's badge counts them.
        mentions += self.threads.mentions;
        (mentions, dms)
    }

    pub fn rows(&self) -> Vec<Row> {
        self.sidebar_rows(false)
    }

    /// The sidebar's rows; `unfold` lists folded sections' conversations too
    /// and ignores the search, for Alt+Shift+↑/↓, which visit them as Slack does.
    fn sidebar_rows(&self, unfold: bool) -> Vec<Row> {
        if !self.filter.is_empty() && !unfold {
            return self.conv_matches(&self.filter);
        }

        let dm_user = |i: usize| self.convs[i].user.as_deref().unwrap_or("");
        let index: HashMap<&str, usize> = self
            .convs
            .iter()
            .enumerate()
            // Deactivated accounts' DMs, archived channels and those we are not in: only the search finds them.
            .filter(|(_, c)| !c.archived && !c.guest && !c.user.as_ref().is_some_and(|u| self.users.deleted.contains(u)))
            .map(|(i, c)| (c.id.as_str(), i))
            .collect();
        let mut placed = HashSet::new();
        let mut explicit: HashMap<String, Vec<usize>> = HashMap::new();
        let mut members_of = |ids: &[String], self_first: bool| {
            let mut m: Vec<usize> = ids.iter().filter_map(|id| index.get(id.as_str()).copied()).collect();
            m.sort_by_key(|&i| (!(self_first && self.convs[i].user.as_deref().is_some_and(|u| self.is_me(u))), i));
            placed.extend(m.iter().copied());
            m
        };
        for s in &self.sections {
            match s.kind.as_str() {
                // Slack's client shows a user group's channels under Canais, not as a section.
                "direct_messages" | "channels" | "user_group" => {}
                "stars" => {
                    explicit.insert(s.id.clone(), members_of(&self.stars, true));
                }
                // Slack fills this one itself; the API lists no channels in it.
                "slack_connect" => {
                    let ids: Vec<String> = self
                        .convs
                        .iter()
                        .filter(|c| c.ext_shared && matches!(c.kind, Kind::Channel | Kind::Private))
                        .map(|c| c.id.clone())
                        .collect();
                    explicit.insert(s.id.clone(), members_of(&ids, false));
                }
                _ => {
                    explicit.insert(s.id.clone(), members_of(&s.ids, false));
                }
            }
        }
        // Without Slack's sections (e.g. an app token): favourites, DMs, channels.
        let fallback: Vec<Section> = if self.sections.is_empty() {
            explicit.insert("stars".into(), members_of(&self.stars, true));
            [("stars", ""), ("direct_messages", ""), ("channels", "")]
                .iter()
                .map(|(k, n)| Section { id: k.to_string(), kind: k.to_string(), name: n.to_string(), emoji: String::new(), ids: Vec::new() })
                .collect()
        } else {
            Vec::new()
        };
        let order: &[Section] = if self.sections.is_empty() { &fallback } else { &self.sections };

        let latest = |i: &usize| self.counts.get(&self.convs[*i].id).map(|c| c.latest.clone()).unwrap_or_default();
        // Slack's list of DMs open in the sidebar, when we have it.
        let open = self.boot.as_ref().map(|b| &b.open).filter(|o| !o.is_empty());
        let mut dms = Vec::new();
        let mut apps = Vec::new();
        let mut channels = Vec::new();
        for i in 0..self.convs.len() {
            if placed.contains(&i) || !index.contains_key(self.convs[i].id.as_str()) {
                continue;
            }
            // Apps, and people from other orgs we cannot name, stay out of the
            // DM section (the search still finds them).
            let uid = dm_user(i);
            let unknown = !self.users.names.is_empty() && !uid.is_empty() && !self.users.names.contains_key(uid);
            if self.users.bots.contains(uid) {
                apps.push(i);
                continue;
            }
            if unknown {
                continue;
            }
            match self.convs[i].kind {
                Kind::Direct | Kind::Group => dms.push(i),
                _ => channels.push(i),
            }
        }
        let mut dms: Vec<usize> = match open {
            Some(open) => dms
                .into_iter()
                .filter(|&i| {
                    let id = &self.convs[i].id;
                    open.contains(id) || self.unread(id).0 || self.current.as_ref() == Some(id)
                })
                .collect(),
            None => {
                dms.sort_by_key(|i| std::cmp::Reverse(latest(i)));
                dms.iter()
                    .enumerate()
                    .filter(|(n, i)| *n < DM_LIMIT || self.unread(&self.convs[**i].id).0)
                    .map(|(_, i)| *i)
                    .collect()
            }
        };
        dms.sort();
        // Like Slack: unread DMs on top, most recent first; the rest by name.
        dms.sort_by_key(|i| {
            let unread = self.unread(&self.convs[*i].id).0;
            (!unread, std::cmp::Reverse(if unread { latest(i) } else { String::new() }))
        });
        // Slack's "Apps" (its recent_apps section): the apps whose DM is open.
        apps.retain(|&i| {
            let id = &self.convs[i].id;
            open.is_none_or(|o| o.contains(id)) || self.unread(id).0 || self.current.as_ref() == Some(id)
        });
        apps.sort_by_key(|&i| self.label(&self.convs[i]).to_lowercase());
        // Without it among Slack's sections, Apps goes last.
        let apps_section =
            Section { id: "apps".into(), kind: "recent_apps".into(), name: String::new(), emoji: String::new(), ids: Vec::new() };
        let mut order: Vec<&Section> = order.iter().collect();
        if !order.iter().any(|s| s.kind == "recent_apps") {
            order.push(&apps_section);
        }
        // Like Slack's muting, but out of the way: channels muted there leave
        // their sections for Silenced, last, by name. Before the preferences
        // arrive nothing is muted, and the sidebar is as Slack has it.
        let muted = |i: &usize| self.muted(&self.convs[*i]);
        let silenced: Vec<usize> = (0..self.convs.len()).filter(|i| index.contains_key(self.convs[*i].id.as_str()) && muted(i)).collect();
        let silenced_section =
            Section { id: SILENCED.into(), kind: SILENCED.into(), name: String::new(), emoji: String::new(), ids: Vec::new() };
        order.push(&silenced_section);
        // Like Slack: Threads first, above every section.
        let mut rows = vec![Row::Threads];
        for s in order {
            let all: &[usize] = match s.kind.as_str() {
                "direct_messages" => &dms,
                "recent_apps" => &apps,
                "channels" => &channels,
                SILENCED => &silenced,
                _ => explicit.get(&s.id).map(Vec::as_slice).unwrap_or(&[]),
            };
            let is_silenced = s.kind == SILENCED;
            let members: Vec<usize> =
                all.iter().copied().filter(|i| is_silenced || !muted(i)).collect();
            if members.is_empty() {
                continue;
            }
            let title = match s.kind.as_str() {
                "stars" => tr("Starred", "Favoritos").to_string(),
                "direct_messages" => tr("Direct messages", "Mensagens diretas").to_string(),
                "channels" => tr("Channels", "Canais").to_string(),
                "recent_apps" => "Apps".to_string(),
                SILENCED => tr("Silenced", "Silenciados").to_string(),
                _ => s.name.clone(),
            };
            let emoji = emojis::get_by_shortcode(&s.emoji).map(|e| format!("{} ", e.as_str())).unwrap_or_default();
            let collapsed = if is_silenced { !self.silenced_open } else { self.collapsed.contains(&s.id) };
            // Muted unread doesn't light a folded Silenced; its mentions show anyway.
            let unread = collapsed && !is_silenced && members.iter().any(|i| self.unread(&self.convs[*i].id).0);
            if !rows.is_empty() {
                rows.push(Row::Gap);
            }
            rows.push(Row::Header { key: s.id.clone(), title: format!("{emoji}{title}"), collapsed, unread });
            if !collapsed || unfold {
                rows.extend(members.iter().map(|i| Row::Conv(*i)));
            } else if is_silenced {
                // Folded, it still lists what mentions you and the open one,
                // as Slack does: a mention in a muted channel still counts.
                let keep = |i: &&usize| {
                    let id = &self.convs[**i].id;
                    self.unread(id).1 > 0 || self.current.as_ref() == Some(id)
                };
                rows.extend(members.iter().filter(keep).map(|i| Row::Conv(*i)));
            }
        }
        rows
    }

    /// The conversations a search matches: every one we are in, also those
    /// hidden from the sidebar, and the workspace's public channels we are
    /// not in, in the order `text::Place` gives. The sidebar's "/" and the
    /// search window (`search`) share it.
    pub(crate) fn conv_matches(&self, query: &str) -> Vec<Row> {
        let q = crate::text::Query::new(query);
        if q.is_empty() {
            return Vec::new();
        }
        let mut ranges = Vec::new();
        let mut hits: Vec<(crate::text::Place, Row)> = Vec::new();
        for (i, c) in self.convs.iter().enumerate() {
            let label = self.label(c);
            let name = crate::text::Folded::new(&label);
            let Some(tier) = crate::text::rank(&q, &name, &mut ranges) else { continue };
            hits.push((crate::text::place(tier, c.archived, !c.guest, self.last_message(c), 0, name.len()), Row::Conv(i)));
        }
        let ours: HashSet<&str> = self.convs.iter().map(|c| c.id.as_str()).collect();
        let chans = &self.directory.chans;
        let others = self.directory_hits(query, &q);
        hits.extend(others.iter().filter(|(_, j)| !ours.contains(chans[*j].id.as_str())).take(OUTSIDE_HITS).map(|&(tier, j)| {
            let d = &chans[j];
            (crate::text::place(tier, d.archived, false, d.updated, d.members, d.folded.len()), Row::Other(j))
        }));
        // Stable: equal places keep the sidebar's name order.
        hits.sort_by(|a, b| a.0.cmp(&b.0));
        hits.into_iter().map(|(_, r)| r).collect()
    }

    pub fn fix_selection(&mut self, rows: &[Row]) {
        // An empty search highlights nothing until something is typed.
        if self.focus == Focus::Filter && self.filter.is_empty() {
            self.sel_row = None;
            return self.list.select(None);
        }
        let key = |r: &Row| match r {
            Row::Conv(i) => Some(self.convs[*i].id.clone()),
            Row::Other(j) => Some(self.directory.chans[*j].id.clone()),
            Row::Header { key, .. } => Some(format!("section:{key}")),
            Row::Threads => Some("threads".into()),
            Row::Gap => None,
        };
        // Same index as last draw but another row there: the list reordered
        // (e.g. a DM got unread), so follow the row. Search keeps its own order.
        if let (Some((n, was)), Some(sel), true) = (&self.sel_row, self.list.selected(), self.filter.is_empty()) {
            if *n == sel && rows.get(sel).and_then(key).as_ref() != Some(was) {
                if let Some(pos) = rows.iter().position(|r| key(r).as_ref() == Some(was)) {
                    self.list.select(Some(pos));
                }
            }
        }
        let sel = self.list.selected().unwrap_or(0).min(rows.len().saturating_sub(1));
        self.list.select((!rows.is_empty()).then_some(sel));
        // A list that shrank must not stay scrolled past its top rows.
        let offset = self.list.offset().min(rows.len().saturating_sub(1));
        *self.list.offset_mut() = offset;
        self.sel_row = match (self.filter.is_empty(), self.list.selected()) {
            (true, Some(n)) => rows.get(n).and_then(key).map(|k| (n, k)),
            _ => None,
        };
    }

    fn move_selection(&mut self, delta: isize) {
        self.sidebar_wheel = false;
        let rows = self.rows();
        let n = rows.len() as isize;
        let from = self.list.selected().unwrap_or(0) as isize;
        let mut i = (from + delta).clamp(0, (n - 1).max(0));
        // Gaps are not stops.
        while matches!(rows.get(i as usize), Some(Row::Gap)) {
            i += delta.signum();
            if !(0..n).contains(&i) {
                i = from;
                break;
            }
        }
        self.list.select(Some(i as usize));
    }

    fn activate(&mut self, row: usize) {
        let rows = self.rows();
        match rows.get(row) {
            Some(Row::Conv(i)) => {
                let id = self.convs[*i].id.clone();
                self.open(id);
            }
            Some(Row::Other(j)) => {
                let id = self.directory.chans[*j].id.clone();
                self.open(id);
            }
            Some(Row::Header { key, .. }) => {
                let key = key.clone();
                self.toggle_section(key);
                self.list.select(Some(row));
            }
            Some(Row::Threads) => {
                self.open_threads();
                self.list.select(Some(row));
            }
            _ => {}
        }
    }

    /// Folds or unfolds a sidebar section.
    fn toggle_section(&mut self, key: String) {
        if key == SILENCED {
            self.silenced_open = !self.silenced_open;
        } else if !self.collapsed.remove(&key) {
            self.collapsed.insert(key);
        }
    }

    /// Where Alt+↑/↓ start in `rows`: the open conversation, else the row
    /// selected in the sidebar.
    fn start_row(&self, rows: &[Row]) -> Option<usize> {
        let open = self.current.as_ref().and_then(|id| rows.iter().position(|r| matches!(r, Row::Conv(i) if self.convs[*i].id == *id)));
        open.or_else(|| {
            let shown = self.rows();
            let sel = shown.get(self.list.selected()?)?;
            rows.iter().position(|r| match (r, sel) {
                (Row::Conv(a), Row::Conv(b)) => a == b,
                (Row::Header { key: a, .. }, Row::Header { key: b, .. }) => a == b,
                (Row::Threads, Row::Threads) => true,
                _ => false,
            })
        })
    }

    /// The conversation `delta` steps away in `rows` (down is 1) that `want`
    /// takes, going round past the ends when `wrap`.
    fn next_conv(&self, rows: &[Row], delta: isize, wrap: bool, want: impl Fn(&Row) -> bool) -> Option<String> {
        let n = rows.len() as isize;
        let start = self.start_row(rows).map_or(if delta < 0 { n } else { -1 }, |i| i as isize);
        let at = |k: isize| {
            let i = start + delta * k;
            if wrap { Some(i.rem_euclid(n.max(1))) } else { (0..n).contains(&i).then_some(i) }
        };
        (1..=n).map_while(at).find_map(|i| match rows[i as usize] {
            Row::Conv(c) if want(&rows[i as usize]) => Some(self.convs[c].id.clone()),
            _ => None,
        })
    }

    /// Alt+↑/↓: the conversation above or below the open one in the sidebar.
    fn open_next(&mut self, delta: isize) {
        let rows = self.rows();
        if let Some(id) = self.next_conv(&rows, delta, false, |_| true) {
            self.open(id);
        }
    }

    /// Alt+Shift+↑/↓: the previous or next conversation with something unread
    /// (bold, or with a mention badge, as the sidebar shows them), folded
    /// sections included, going round past the ends.
    fn open_unread(&mut self, delta: isize) {
        let rows = self.sidebar_rows(true);
        let unread = |r: &Row| {
            let (bold, mention) = crate::ui::row_attention(self, r);
            bold || mention
        };
        match self.next_conv(&rows, delta, true, unread) {
            Some(id) => self.open(id),
            None => self.brief(tr("No unread conversations", "Nenhuma conversa não lida").into(), 2),
        }
    }

    /// Alt+← and Alt+→: the conversation opened before (or after) this one.
    /// Away from it (the Threads view), back is the last one opened.
    fn go_history(&mut self, delta: isize) {
        let here = self.current.is_some() && self.current.as_ref() == self.history.get(self.history_at);
        let mut i = self.history_at as isize + if here || delta > 0 { delta } else { 0 };
        // Conversations left since are passed over.
        loop {
            let Some(id) = usize::try_from(i).ok().and_then(|n| self.history.get(n)).cloned() else { return };
            if self.convs.iter().any(|c| c.id == id) || self.directory.find(&id).is_some() {
                self.history_at = i as usize;
                return self.open(id);
            }
            i += delta;
        }
    }

    /// Shift+Esc: every conversation marked read, as Slack's "mark all as read".
    fn mark_all_read(&mut self) {
        let ids: Vec<String> = self.counts.iter().filter(|(_, c)| c.unread || c.mentions > 0).map(|(id, _)| id.clone()).collect();
        if ids.is_empty() {
            return;
        }
        let n = ids.len();
        for id in ids {
            self.run_action(Action::MarkRead(id));
        }
        self.brief(trf!("{n} conversations marked as read", "{n} conversas marcadas como lidas"), 3);
    }

    /// Ctrl+.: the panel on the right (thread or profile) closes.
    fn close_panel(&mut self) {
        if self.thread.is_some() {
            self.close_thread();
        } else {
            self.profile = None;
        }
    }

    pub(crate) fn open(&mut self, id: String) {
        // A channel you are not in comes for its visit only, as a preview.
        self.drop_guests(&id);
        if !self.convs.iter().any(|c| c.id == id) {
            if let Some(j) = self.directory.find(&id) {
                self.convs.push(self.directory.conv(j));
                self.resort();
            }
        }
        if !self.convs.iter().any(|c| c.id == id && c.guest) {
            self.recents.retain(|r| r != &id);
            self.recents.insert(0, id.clone());
            self.recents.truncate(MAX_RECENTS);
        }
        // Back and forward (`go_history`) land on their entry and push nothing;
        // anything else opened drops what was ahead.
        if self.history.get(self.history_at) != Some(&id) {
            self.history.truncate(self.history_at + 1);
            self.history.push(id.clone());
            if self.history.len() > MAX_HISTORY {
                self.history.remove(0);
            }
            self.history_at = self.history.len() - 1;
        }
        // Slack's "New" line, from where it was read up to before this open
        // reads it; opened again from itself, it stays where it was.
        if self.current.as_deref() != Some(id.as_str()) {
            let from = self.counts.get(&id).filter(|c| (c.unread || c.mentions > 0) && !c.last_read.is_empty());
            self.new_line = from.map(|c| NewLine { chan: id.clone(), after: c.last_read.clone(), at: None, fetched: false, jump: false });
            self.find_new_line(&id, false);
        }
        // Out of focus (opened from outside) it stays unread until the window
        // gets the keyboard, which then marks it read (`set_focused`).
        if let Some(c) = self.counts.get_mut(&id).filter(|_| self.focused) {
            c.unread = false;
            c.mentions = 0;
        }
        self.drop_edit();
        self.selection = None;
        self.current = Some(id.clone());
        self.scroll = 0;
        self.stay = None;
        self.thread = None;
        self.threads.open = false;
        self.pins.tab = crate::pins::Tab::Messages;
        self.focus = Focus::Input;
        self.filter.clear();
        // Cached messages show at once; the fetch replaces them, a stretch
        // left from an earlier jump too (`merge`).
        self.detached.remove(&id);
        self.fetch(id.clone(), Fetch::Initial);
        // A DM tells when the other person paused their notifications.
        if let Some(uid) = self.convs.iter().find(|c| c.id == id && c.kind == Kind::Direct).and_then(|c| c.user.clone()) {
            if !self.is_me(&uid) && !self.users.deleted.contains(&uid) {
                // And their status as it is now, in case the websocket missed it.
                self.refresh_user(uid.clone());
                self.refresh_user_dnd(uid);
            }
        }
        let rows = self.rows();
        let pos = rows.iter().position(|r| matches!(r, Row::Conv(i) if self.convs[*i].id == id));
        if pos.is_some() {
            self.list.select(pos);
        }
    }

    /// Puts the "New" line over the first message after where it was read
    /// up to that someone else sent. The cache may not reach that far: then
    /// the open's own fetch (`fetched`) settles it, line or none.
    fn find_new_line(&mut self, chan: &str, fetched: bool) {
        let Some(n) = self.new_line.as_ref().filter(|n| n.chan == chan && !n.fetched) else { return };
        let msgs = self.messages.get(chan).map(Vec::as_slice).unwrap_or_default();
        let first = msgs.iter().find(|m| m.ts > n.after && !m.user.as_deref().is_some_and(|u| self.is_me(u)));
        let at = first.map(|m| m.ts.clone()).or_else(|| n.at.clone());
        let Some(n) = self.new_line.as_mut() else { return };
        // Found in the cache, the fetch only asks to bring it into view again.
        n.jump |= at.is_some() && (fetched || n.at.is_none());
        n.at = at;
        n.fetched = fetched;
    }

    pub(crate) fn fetch(&mut self, chan: String, fetch: Fetch) {
        let key = format!("{chan}:{}", fetch as u8);
        if !self.loading.insert(key) {
            return;
        }
        if fetch == Fetch::Initial {
            self.refresh_pins(&chan);
        }
        let msgs = self.messages.get(&chan);
        // Polls fetch the newest page whole, not just what came after: edits,
        // deletions and reactions made elsewhere show up too.
        let oldest = match fetch {
            Fetch::Newer => msgs.and_then(|m| m.last()).map(|m| m.ts.clone()),
            _ => None,
        };
        let latest = match fetch {
            Fetch::Older => msgs.and_then(|m| m.first()).map(|m| m.ts.clone()),
            _ => None,
        };
        self.spawn(move |s| {
            let res = s.history(&chan, oldest.as_deref(), latest.as_deref());
            Event::History { chan, fetch, res }
        });
    }

    pub(crate) fn open_profile(&mut self, uid: String) {
        self.close_thread();
        self.profile = Some((uid.clone(), None));
        self.dirty = true;
        self.spawn(move |s| {
            let res = s.profile(&uid);
            Event::Profile { uid, res }
        });
    }

    fn mark_read(&self, chan: &str) {
        // Slack keeps no place in a channel for who isn't in it.
        if self.convs.iter().any(|c| c.id == chan && c.guest) {
            return;
        }
        if let Some(ts) = self.messages.get(chan).and_then(|m| m.last()).map(|m| m.ts.clone()) {
            let chan = chan.to_string();
            self.spawn_quiet(move |s| {
                let _ = s.mark(&chan, &ts);
            });
        }
    }

    /// The frame an animated image shows now, and when to redraw for the next;
    /// `None` for still pictures.
    pub fn frame(&self, key: &str) -> Option<(usize, Instant)> {
        let now = Instant::now();
        let (i, left) = self.anims.get(key)?.frame_at(now - self.clock);
        // The GIF's own delay rules, but not sooner than 20ms (50fps) to spare the terminal.
        Some((i, now + left.max(Duration::from_millis(20))))
    }

    /// Names who and what the last draw mentioned without knowing; with the
    /// lists still loading, everyone would look unknown.
    pub fn request_names(&mut self) {
        use crate::blocks::Target;
        for t in std::mem::take(&mut self.unnamed) {
            let (id, person) = match t {
                Target::User(id) if !self.users.names.is_empty() => (id, true),
                Target::Channel(id) if !self.convs.is_empty() => (id, false),
                _ => continue,
            };
            let known = !person && (self.chans.contains_key(&id) || self.directory.find(&id).is_some());
            if known || self.others.contains_key(&id) || !self.asked.insert(id.clone()) {
                continue;
            }
            self.spawn(move |s| {
                if person {
                    let res = s.profile(&id).map(|p| (p.name, false));
                    Event::Named { id, res }
                } else {
                    let res = s.conversation(&id);
                    Event::ChanNamed { id, res }
                }
            });
        }
    }

    /// A mentioned channel looked up: named from now on, and a public one
    /// joins the search's list of channels, kept on disk with it.
    fn chan_named(&mut self, id: String, c: Conversation) {
        if c.kind == Kind::Private {
            self.private.insert(id.clone());
        }
        self.others.insert(id, c.name.clone());
        if c.kind == Kind::Channel && self.directory.find(&c.id).is_none() {
            self.directory.add(&c);
            self.dir_gen += 1;
            if let Some(team) = self.me.as_ref().map(|m| m.team_id.clone()) {
                let d = self.directory.clone();
                std::thread::spawn(move || cache::save_directory(&team, &d));
            }
        }
    }

    /// Asks for the names of the channels found messages mention, which
    /// their rows show as "#…" until then (`request_names`).
    pub(crate) fn name_mentions(&mut self, text: &str) {
        let mut rest = text;
        while let Some(i) = rest.find("<#") {
            rest = &rest[i + 2..];
            let id = rest.split(['|', '>']).next().unwrap_or("");
            if !id.is_empty() && self.names().chan(id).is_none() {
                self.unnamed.push(crate::blocks::Target::Channel(id.to_string()));
            }
        }
    }

    /// A click on a mention: the person's DM, opened in Slack when there's none yet.
    pub(crate) fn open_dm(&mut self, uid: String) {
        match self.dm_with(&uid) {
            Some(id) => self.open(id),
            None => self.spawn(move |s| Event::Dm(s.open_conversation(&[uid]))),
        }
    }

    /// After the cell's pixel size changed: encodings and the pictures drawn
    /// for one cell size go, and are made again at the next draw.
    pub fn font_changed(&mut self) {
        self.protocols.clear();
        self.images.retain(|k, _| !["presence-", "badge-", "team-stack-"].iter().any(|p| k.starts_with(p)));
        self.dirty = true;
    }

    /// Downloads what the last draw asked for; files are kept on disk.
    pub fn request_images(&mut self) {
        for r in std::mem::take(&mut self.wanted) {
            if self.images.contains_key(&r.key) {
                continue;
            }
            self.images.insert(r.key.clone(), Img::Loading);
            self.spawn(move |s| {
                let path = cache::image_path(&r.key);
                let bytes = std::fs::read(&path).inspect(|_| cache::touch(&path)).or_else(|_| {
                    let b = s.download(&r.url)?;
                    let _ = std::fs::create_dir_all(path.parent().unwrap());
                    let _ = std::fs::write(&path, &b);
                    Ok::<_, String>(b)
                });
                let anim = bytes.as_ref().ok().filter(|_| r.key.starts_with("emoji-")).and_then(|b| emoji::Anim::decode(b));
                let mut res = bytes.and_then(|b| image::load_from_memory(&b).map_err(|e| e.to_string()));
                if r.key.starts_with("avatar-512-") {
                    res = res.map(|i| crate::icons::rounded_by(i, crate::icons::PROFILE_RADIUS));
                } else if r.key.starts_with("avatar-") {
                    res = res.map(crate::icons::rounded);
                }
                Event::Image { key: r.key, res, anim }
            });
        }
    }

    /// The people in the sidebar's DMs, whose presence is followed.
    fn presence_uids(&self) -> Vec<String> {
        self.rows()
            .iter()
            .filter_map(|r| match r {
                Row::Conv(i) if self.convs[*i].kind == Kind::Direct => self.convs[*i].user.clone(),
                _ => None,
            })
            .filter(|u| !self.users.bots.contains(u))
            .take(40)
            .collect()
    }

    /// People in the sidebar's DMs, checked one by one to stay under the rate
    /// limit. Not while the websocket reports their presence itself.
    fn refresh_presence(&mut self) {
        let uids = self.presence_uids();
        self.watch_presence(uids.clone());
        if uids.is_empty() || (self.live && self.presence_live) {
            return;
        }
        self.spawn(move |s| {
            let mut out = HashMap::new();
            for u in uids {
                if let Ok(on) = s.online(&u) {
                    out.insert(u, on);
                }
                std::thread::sleep(Duration::from_millis(300));
            }
            Event::Presence(out)
        });
    }

    /// Moves a sidebar section past its visible neighbour, here and in Slack.
    fn move_section(&mut self, key: &str, delta: isize) {
        if self.sections.is_empty() {
            return;
        }
        let visible: Vec<String> = self
            .rows()
            .iter()
            .filter_map(|r| match r {
                // Silenced is slick's, pinned last: neither moved nor passed.
                Row::Header { key, .. } if key != SILENCED => Some(key.clone()),
                _ => None,
            })
            .collect();
        let Some(pos) = visible.iter().position(|k| k == key) else { return };
        let target = pos as isize + delta;
        if target < 0 || target >= visible.len() as isize {
            return;
        }
        // Slack takes "the section that comes right after"; hidden empty
        // sections sit in between, so aim past the visible neighbour.
        let next = if delta < 0 {
            visible[pos - 1].clone()
        } else {
            let after = self.sections.iter().position(|s| s.id == visible[pos + 1]).unwrap_or(0);
            self.sections.get(after + 1).map(|s| s.id.clone()).unwrap_or_default()
        };
        self.place_section(key, next);
    }

    /// Puts a section right before `next` (empty: last), here and in Slack.
    fn place_section(&mut self, key: &str, next: String) {
        let Some(idx) = self.sections.iter().position(|s| s.id == key) else { return };
        let section = self.sections.remove(idx);
        let at = self.sections.iter().position(|s| s.id == next).unwrap_or(self.sections.len());
        self.sections.insert(at, section);
        let row = self.rows().iter().position(|r| matches!(r, Row::Header { key: k, .. } if k == key));
        self.list.select(row);
        let id = key.to_string();
        self.spawn(move |s| Event::SectionMoved(s.move_section(&id, &next)));
    }

    /// The section a sidebar row sits in: the header above it.
    pub fn section_of(rows: &[Row], row: usize) -> Option<String> {
        rows[..=row.min(rows.len().saturating_sub(1))].iter().rev().find_map(|r| match r {
            Row::Header { key, .. } => Some(key.clone()),
            _ => None,
        })
    }

    fn section_kind(&self, key: &str) -> String {
        self.sections.iter().find(|s| s.id == key).map(|s| s.kind.clone()).unwrap_or_else(|| key.to_string())
    }

    fn is_default_section(kind: &str) -> bool {
        matches!(kind, "stars" | "direct_messages" | "channels")
    }

    /// Sections a conversation can be moved to, in sidebar order.
    fn section_choices(&self) -> Vec<(String, String)> {
        let names = |s: &Section| match s.kind.as_str() {
            "stars" => Some(tr("Starred", "Favoritos").to_string()),
            "direct_messages" => Some(tr("Direct messages", "Mensagens diretas").to_string()),
            "channels" => Some(tr("Channels", "Canais").to_string()),
            "standard" => Some(s.name.clone()),
            _ => None,
        };
        self.sections.iter().filter_map(|s| Some((s.id.clone(), names(s)?))).collect()
    }

    /// Favourite or not: into "Favoritos", or back to its default section.
    pub fn set_star(&mut self, id: String, on: bool) {
        let to = if on {
            self.sections.iter().find(|s| s.kind == "stars").map(|s| s.id.clone()).unwrap_or_else(|| "stars".into())
        } else {
            let kind = match self.convs.iter().find(|c| c.id == id).map(|c| c.kind) {
                Some(Kind::Direct | Kind::Group) => "direct_messages",
                _ => "channels",
            };
            self.sections.iter().find(|s| s.kind == kind).map(|s| s.id.clone()).unwrap_or_else(|| kind.into())
        };
        self.move_conv(id, to);
    }

    fn set_vip(&mut self, uid: String, on: bool) {
        if on {
            self.vips.insert(uid.clone());
        } else {
            self.vips.remove(&uid);
        }
        self.spawn(move |s| Event::VipSaved(s.vip(&uid, on)));
    }

    /// The DM with someone, if there is one.
    pub fn dm_with(&self, uid: &str) -> Option<String> {
        self.convs.iter().find(|c| c.kind == Kind::Direct && c.user.as_deref() == Some(uid)).map(|c| c.id.clone())
    }

    /// Drag and drop: a conversation onto another section, here and in Slack.
    fn move_conv(&mut self, id: String, to: String) {
        let kind = self.section_kind(&to);
        // Apps lists app DMs on its own, Silenced the muted channels; nothing
        // is moved into them.
        if kind == "recent_apps" || to == SILENCED {
            return;
        }
        let from = self
            .sections
            .iter()
            .find(|s| !Self::is_default_section(&s.kind) && s.ids.contains(&id))
            .map(|s| s.id.clone());
        let starred = self.stars.contains(&id);
        let unchanged = match kind.as_str() {
            "stars" => starred,
            "direct_messages" | "channels" => !starred && from.is_none(),
            _ => !starred && from.as_deref() == Some(to.as_str()),
        };
        if unchanged {
            return;
        }
        for s in &mut self.sections {
            s.ids.retain(|c| c != &id);
        }
        self.stars.retain(|c| c != &id);
        let (star, insert) = match kind.as_str() {
            "stars" => {
                self.stars.push(id.clone());
                (Some(true), None)
            }
            "direct_messages" | "channels" => (starred.then_some(false), None),
            _ => {
                if let Some(s) = self.sections.iter_mut().find(|s| s.id == to) {
                    s.ids.push(id.clone());
                }
                (starred.then_some(false), Some(to))
            }
        };
        self.spawn(move |s| {
            let res = star.map_or(Ok(()), |on| s.star(&id, on)).and_then(|_| s.move_to_section(&id, from.as_deref(), insert.as_deref()));
            Event::SidebarSaved(res)
        });
    }

    /// Drag and drop of a section header onto another section.
    fn drop_section(&mut self, key: &str, onto: &str) {
        let visible: Vec<String> = self
            .rows()
            .iter()
            .filter_map(|r| match r {
                Row::Header { key, .. } if key != SILENCED => Some(key.clone()),
                _ => None,
            })
            .collect();
        let (Some(a), Some(b)) = (visible.iter().position(|k| k == key), visible.iter().position(|k| k == onto)) else { return };
        if a == b {
            return;
        }
        let next = if b < a {
            onto.to_string()
        } else {
            let after = self.sections.iter().position(|s| s.id == onto).unwrap_or(0);
            self.sections.get(after + 1).map(|s| s.id.clone()).unwrap_or_default()
        };
        self.place_section(key, next);
    }

    fn drop_row(&mut self, from: usize, over: usize) {
        let rows = self.rows();
        let Some(onto) = Self::section_of(&rows, over) else { return };
        match rows.get(from) {
            Some(Row::Conv(i)) => {
                let id = self.convs[*i].id.clone();
                self.move_conv(id, onto);
            }
            Some(Row::Header { key, .. }) => {
                let key = key.clone();
                self.drop_section(&key, &onto);
            }
            _ => {}
        }
    }

    fn conv_url(&self, id: &str) -> String {
        match self.me.as_ref().map(|m| m.domain.as_str()).filter(|d| !d.is_empty()) {
            Some(d) => format!("https://{d}.slack.com/archives/{id}"),
            None => format!("https://app.slack.com/client/{id}"),
        }
    }

    /// A message's permalink: interactive buttons open it in the browser
    /// until the app can answer them itself.
    pub(crate) fn message_url(&self, chan: &str, ts: &str) -> String {
        format!("{}/p{}", self.conv_url(chan), ts.replace('.', ""))
    }

    /// Copies with wl-copy or xclip (clip.exe on Windows), or else through the
    /// terminal (OSC 52), which works over SSH too.
    pub(crate) fn copy(&mut self, text: &str) {
        use std::io::Write;
        let local = if cfg!(windows) {
            // clip.exe reads UTF-16 with a BOM; UTF-8 would garble accents.
            let utf16: Vec<u8> = [0xFF, 0xFE].into_iter().chain(text.encode_utf16().flat_map(u16::to_le_bytes)).collect();
            pipe("clip", &[], &utf16)
        } else {
            let tools: [(&str, &[&str], &str); 2] =
                [("wl-copy", &[], "WAYLAND_DISPLAY"), ("xclip", &["-selection", "clipboard"], "DISPLAY")];
            tools.iter().any(|(cmd, args, env)| std::env::var_os(env).is_some() && pipe(cmd, args, text.as_bytes()))
        };
        if !local {
            let mut out = std::io::stdout();
            let _ = write!(out, "\x1b]52;c;{}\x07", base64(text.as_bytes()));
            let _ = out.flush();
        }
        self.brief_copied(tr("copied", "copiado").into());
    }

    /// The right-click menu for a sidebar row, at a screen position.
    fn open_menu(&mut self, row: usize, x: u16, y: u16) {
        let rows = self.rows();
        let items: Vec<(String, Action)> = match rows.get(row) {
            Some(Row::Conv(i)) if !self.convs[*i].guest => {
                let c = &self.convs[*i];
                let id = c.id.clone();
                let starred = self.stars.contains(&id);
                let mut items = vec![
                    (tr("Mark as read", "Marcar como lida").to_string(), Action::MarkRead(id.clone())),
                    (
                        if starred { tr("Remove from starred", "Remover dos favoritos") } else { tr("Star conversation", "Adicionar aos favoritos") }
                            .to_string(),
                        Action::Star(id.clone(), !starred),
                    ),
                ];
                if !self.sections.is_empty() {
                    items.push((tr("Move conversation  ›", "Mover conversa  ›").to_string(), Action::MoveMenu(id.clone())));
                }
                items.extend([
                    ("".to_string(), Action::Separator),
                    (tr("Copy name", "Copiar nome").to_string(), Action::CopyName(self.label(c))),
                    (tr("Copy link", "Copiar link").to_string(), Action::CopyLink(self.conv_url(&id))),
                    (tr("Open in browser", "Abrir no navegador").to_string(), Action::Browser(self.conv_url(&id))),
                ]);
                if matches!(c.kind, Kind::Direct | Kind::Group) {
                    items.push(("".to_string(), Action::Separator));
                    items.push((tr("Close conversation", "Fechar conversa").to_string(), Action::Close(id)));
                }
                items
            }
            Some(Row::Header { key, collapsed, .. }) if key == SILENCED => {
                vec![((if *collapsed { tr("Expand", "Expandir") } else { tr("Collapse", "Recolher") }).to_string(), Action::Collapse(key.clone()))]
            }
            Some(Row::Header { key, collapsed, .. }) => vec![
                (tr("Move section up", "Mover seção para cima").to_string(), Action::SectionUp(key.clone())),
                (tr("Move section down", "Mover seção para baixo").to_string(), Action::SectionDown(key.clone())),
                ((if *collapsed { tr("Expand", "Expandir") } else { tr("Collapse", "Recolher") }).to_string(), Action::Collapse(key.clone())),
            ],
            _ => return,
        };
        self.list.select(Some(row));
        self.menu = Some(Menu::new(x, y, items));
    }

    fn run_action(&mut self, action: Action) {
        let mut close = true;
        match action {
            Action::Separator => close = false,
            Action::MarkRead(id) => {
                let ts = self.counts.get(&id).map(|c| c.latest.clone()).unwrap_or_default();
                if let Some(c) = self.counts.get_mut(&id) {
                    c.unread = false;
                    c.mentions = 0;
                }
                if !ts.is_empty() {
                    self.spawn_quiet(move |s| {
                        let _ = s.mark(&id, &ts);
                    });
                }
            }
            Action::Star(id, on) => self.set_star(id, on),
            Action::MoveMenu(id) => {
                close = false;
                if let Some(parent) = self.menu.take() {
                    let items = self.section_choices().into_iter().map(|(sid, name)| (name, Action::MoveTo(id.clone(), sid))).collect();
                    let x = self.areas.menu.right();
                    let y = self.areas.menu.y + parent.sel as u16;
                    let mut sub = Menu::new(x, y, items);
                    sub.parent = Some(Box::new(parent));
                    self.menu = Some(sub);
                }
            }
            Action::MoveTo(id, to) => self.move_conv(id, to),
            Action::CopyName(t) | Action::CopyLink(t) => self.copy(&t),
            Action::Browser(url) => self.open_url(&url),
            Action::File(click) => self.file_click(click, 0, 0),
            Action::Close(id) => {
                if let Some(b) = self.boot.as_mut() {
                    b.open.remove(&id);
                }
                self.spawn(move |s| Event::SidebarSaved(s.close(&id)));
            }
            Action::SectionUp(key) => self.move_section(&key, -1),
            Action::SectionDown(key) => self.move_section(&key, 1),
            Action::Collapse(key) => self.toggle_section(key),
            Action::NewMessage => self.dialog = Some(crate::compose::Dialog::Message(Default::default())),
            Action::NewChannel => self.dialog = Some(crate::compose::Dialog::Channel(Default::default())),
            Action::Message(it) => close = self.message_action(it),
        }
        if close {
            self.menu = None;
        }
        self.dirty = true;
    }

    fn menu_key(&mut self, k: KeyEvent) {
        let Some(m) = self.menu.as_mut() else { return };
        match k.code {
            KeyCode::Up | KeyCode::Char('k') => m.step(-1),
            KeyCode::Down | KeyCode::Char('j') => m.step(1),
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('h') => self.menu = self.menu.take().and_then(|m| m.parent.map(|p| *p)),
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') | KeyCode::Char(' ') => {
                let action = m.items[m.sel].1.clone();
                self.run_action(action);
            }
            _ => {}
        }
    }

    fn set_sidebar_width(&mut self, w: u16) {
        let screen = crossterm::terminal::size().map(|s| s.0).unwrap_or(120);
        self.sidebar_width = w.clamp(SIDEBAR_MIN, (screen / 2).max(SIDEBAR_MIN));
    }

    fn profile_url(&self, uid: &str) -> String {
        match self.me.as_ref().map(|m| m.domain.as_str()).filter(|d| !d.is_empty()) {
            Some(d) => format!("https://{d}.slack.com/team/{uid}"),
            None => format!("https://app.slack.com/team/{uid}"),
        }
    }

    pub(crate) fn open_url(&self, url: &str) {
        let _ = std::process::Command::new(OPENER)
            .arg(url)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }

    fn open_external(&self, r: &ImgRef) {
        let path = cache::image_path(&r.key);
        if path.exists() {
            cache::touch(&path);
            let _ = std::process::Command::new(OPENER)
                .arg(path)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
        }
    }

    /// Saves a file to the downloads folder in the background.
    fn save_file(&mut self, job: files::Job) {
        if matches!(self.downloads.get(&job.key), Some(files::State::Saving)) {
            return;
        }
        self.downloads.insert(job.key.clone(), files::State::Saving);
        self.status = trf!("downloading {}…", "baixando {}…", job.name);
        self.spawn(move |s| Event::Downloaded { res: files::save(s, &job), key: job.key });
    }

    fn file_click(&mut self, click: files::Click, x: u16, y: u16) {
        match click {
            files::Click::Save(job) => self.save_file(job),
            files::Click::OpenWith(job) => {
                self.status = trf!("opening {}…", "abrindo {}…", job.name);
                self.spawn(move |s| Event::Opened(files::cached(s, &job)));
            }
            files::Click::Menu(job, saved) => {
                let mut items = Vec::new();
                if let Some(p) = &saved {
                    items.push((tr("Open", "Abrir").to_string(), Action::File(files::Click::Open(p.clone()))));
                    items.push((tr("Show in folder", "Mostrar na pasta").to_string(), Action::File(files::Click::Reveal(p.clone()))));
                } else if let Some(job) = &job {
                    items.push((tr("Open with system app", "Abrir no sistema").to_string(), Action::File(files::Click::OpenWith(job.clone()))));
                }
                if let Some(job) = job {
                    items.push((tr("Download", "Baixar").to_string(), Action::File(files::Click::Save(job))));
                }
                if !items.is_empty() {
                    self.menu = Some(Menu::new(x, y, items));
                }
            }
            files::Click::Open(path) => self.open_url(&path.to_string_lossy()),
            files::Click::Reveal(path) => {
                if let Some(dir) = path.parent() {
                    self.open_url(&dir.to_string_lossy());
                }
            }
        }
        self.dirty = true;
    }

    // ---- events --------------------------------------------------------


    /// A status line that goes away after `secs`.
    pub fn brief(&mut self, text: String, secs: u64) {
        self.status_until = Some((text.clone(), Instant::now() + std::time::Duration::from_secs(secs)));
        self.status = text;
    }

    /// A "… copied" status line, gone with the "✓ copied" beside a card.
    pub fn brief_copied(&mut self, text: String) {
        self.status_until = Some((text.clone(), Instant::now() + COPIED_FOR));
        self.status = text;
    }

    pub fn tick(&mut self) {
        if self.login.is_some() {
            return;
        }
        let now = Instant::now();
        if self.start_at.is_some_and(|t| t <= now) {
            self.start_at = None;
            self.rest_at = Some(now + REST_AFTER);
            self.refresh_light();
        }
        if self.rest_at.is_some_and(|t| t <= now) {
            self.rest_at = None;
            self.refresh_rest();
        }
        if self.start_at.is_some() {
            return;
        }
        // Behind another workspace: no presence polling, only its pause now and then.
        if self.background {
            if self.last_presence.elapsed() >= DND_EVERY_BACKGROUND {
                self.last_presence = now;
                self.refresh_dnd();
            }
        } else if self.last_presence.elapsed() >= PRESENCE_EVERY && !self.convs.is_empty() {
            self.last_presence = Instant::now();
            self.refresh_presence();
            self.refresh_dnd();
        }
        // Only the workspace on screen: a search is typed there.
        if !self.background {
            self.fetch_directory();
            self.tick_search();
        }
        let users_every = if self.background { USERS_EVERY_BACKGROUND } else { USERS_EVERY };
        if self.last_users.elapsed() >= users_every {
            self.last_users = Instant::now();
            // Quietly: a failure here waits for the next round.
            let (slack, tx) = (self.slack.clone(), self.tx.clone());
            std::thread::spawn(move || {
                if let Ok(users) = slack.users() {
                    let _ = tx.send(Event::Users(Ok(users)));
                }
            });
        }
        // A DM that joined or left the sidebar joins or leaves the subscription.
        if self.live && self.last_watch.elapsed() >= WATCH_EVERY && !self.convs.is_empty() {
            self.last_watch = Instant::now();
            let uids = self.presence_uids();
            self.watch_presence(uids);
        }
        let counts_every = if self.live { LIVE_FALLBACK } else { COUNTS_EVERY };
        if self.counts_at.is_some_and(|t| t <= Instant::now()) || self.last_counts.elapsed() >= counts_every {
            self.counts_at = None;
            self.last_counts = Instant::now();
            self.fetch_counts();
        }
        if !self.background {
            self.poll_thread();
        }
        self.poll_threads();
        if self.copied.is_some_and(|(_, at)| at.elapsed() >= COPIED_FOR) {
            self.copied = None;
            self.dirty = true;
        }
        // Only if nothing replaced it in the meantime.
        if let Some((text, at)) = &self.status_until {
            if Instant::now() >= *at {
                if self.status == *text {
                    self.status.clear();
                    self.dirty = true;
                }
                self.status_until = None;
            }
        }
        // Anything else there goes too after a while, but what's still
        // going on ("downloading…").
        if (&self.status, &self.error) != (&self.notice_seen.0, &self.notice_seen.1) {
            self.notice_seen = (self.status.clone(), self.error.clone(), Instant::now());
        } else if self.notice_seen.2.elapsed() >= NOTICE_FOR {
            if !self.status.is_empty() && !self.status.ends_with('…') {
                self.status.clear();
                self.dirty = true;
            }
            if self.error.take().is_some() {
                self.dirty = true;
            }
        }
        let before = self.typing.values().map(HashMap::len).sum::<usize>();
        self.typing.retain(|_, t| {
            t.retain(|_, at| at.elapsed() < TYPING);
            !t.is_empty()
        });
        if self.typing.values().map(HashMap::len).sum::<usize>() != before {
            self.dirty = true;
        }
        if self.flash.as_ref().is_some_and(|(_, at)| at.elapsed() >= FLASH) {
            self.flash = None;
            self.dirty = true;
        }
        let Some(chan) = self.current.clone().filter(|_| !self.background) else { return };
        if self.polling || self.last_poll.elapsed() < self.poll_every() || !self.messages.contains_key(&chan) {
            return;
        }
        if self.loading.contains(&format!("{chan}:{}", Fetch::Initial as u8)) {
            return;
        }
        self.polling = true;
        self.last_poll = Instant::now();
        self.fetch(chan, Fetch::Poll);
    }

    pub fn handle(&mut self, ev: Event) {
        self.dirty = true;
        let ev = match ev {
            Event::Auth(Err(e)) | Event::Users(Err(e)) | Event::Convs(Err(e)) | Event::Stars(Err(e))
                if is_auth_error(&e) =>
            {
                return self.open_login(Some(tr("The session expired. Sign in again.", "A sessão expirou. Entre de novo.").into()));
            }
            Event::History { res: Err(e), .. } | Event::Sent { res: Err(e), .. } if is_auth_error(&e) => {
                self.loading.clear();
                self.polling = false;
                return self.open_login(Some(tr("The session expired. Sign in again.", "A sessão expirou. Entre de novo.").into()));
            }
            ev => ev,
        };
        match ev {
            Event::Auth(Ok(me)) => {
                if !self.rtm_started {
                    self.rtm_started = true;
                    crate::rtm::start(self.slack.clone(), self.tx.clone());
                }
                // Another account: nothing cached belongs to it.
                if !self.owner.is_empty() && self.owner != me.user_id {
                    self.convs.clear();
                    self.sections.clear();
                    self.stars.clear();
                    self.messages.clear();
                    self.recents.clear();
                    self.users = Users::default();
                    self.emoji = emoji::Emoji::default();
                    self.slash.commands.clear();
                    // The conversation reopened from the cache was the other account's.
                    self.current = None;
                    self.thread = None;
                    self.threads = Default::default();
                    self.pins = Default::default();
                    self.focus = Focus::Sidebar;
                }
                self.owner = me.user_id.clone();
                if let Some(creds) = self.legacy.take() {
                    let _ = config::save_workspace(&me, &creds);
                }
                self.status.clear();
                // The search's channel list as last read, until it is read again.
                if self.dir_team.as_deref() != Some(me.team_id.as_str()) {
                    self.dir_team = Some(me.team_id.clone());
                    self.directory = cache::load_directory(&me.team_id);
                    self.dir_gen += 1;
                }
                self.me = Some(me);
                self.resort();
            }
            Event::Login(res) => self.finish_login(res),
            Event::Composed { res, warning } => self.composed(res, warning),
            Event::Replies { chan, ts, res } => self.on_replies(chan, ts, res),
            Event::Replied { chan, ts, text, res } => self.on_replied(chan, ts, text, res),
            Event::Pins { chan, res } => self.on_pins(chan, res),
            Event::Around { chan, ts, res } => self.on_around(chan, ts, res),
            Event::Users(Ok(users)) => {
                self.users = users;
                self.resort();
            }
            Event::Convs(Ok(convs)) => {
                // The channel being previewed stays until it is left.
                let guest = self.convs.iter().find(|c| c.guest && self.current.as_ref() == Some(&c.id)).cloned();
                self.convs = convs;
                self.convs.extend(guest.filter(|g| !self.convs.iter().any(|c| c.id == g.id)));
                self.resort();
                self.open_default();
            }
            Event::Sections(Ok(s)) => self.sections = s,
            Event::Stars(Ok(s)) => self.stars = s,
            Event::Vips(Ok(v)) => self.vips = v,
            Event::Vips(Err(_)) | Event::VipSaved(Ok(())) => {}
            Event::VipSaved(Err(e)) => {
                self.error = Some(e);
                self.spawn(|s| Event::Vips(s.vips()));
            }
            Event::Counts(Ok(c)) => self.on_counts(c),
            Event::Quit => self.quit = true,
            Event::Remote(cmd) => self.on_remote(cmd),
            Event::Preview { chan, latest, res } => self.on_preview(chan, latest, res),
            Event::NotifyPrefs(res) => self.on_prefs(res),
            Event::Attach(e) => self.on_attach(e),
            Event::Joined(Ok(c)) => {
                if !self.convs.iter().any(|x| x.id == c.id) {
                    self.convs.push(c);
                    self.resort();
                }
            }
            Event::Joined(Err(_)) => {}
            Event::Named { id, res: Ok((name, private)) } => {
                if private {
                    self.private.insert(id.clone());
                }
                self.others.insert(id, name);
            }
            // Nothing better to show than the id, and no asking again.
            Event::Named { id, res: Err(_) } | Event::ChanNamed { id, res: Err(_) } => {
                self.others.insert(id.clone(), id);
            }
            Event::ChanNamed { id, res: Ok(c) } => self.chan_named(id, c),
            Event::Dm(res) => self.dm_opened(res),
            Event::Slash(e) => self.on_slash(e),
            Event::Details(e) => self.on_details(e),
            Event::Invite(e) => self.on_invite(e),
            Event::Unarchived { id, res } => self.unarchived(id, res),
            Event::Directory(res) => self.on_directory(res),
            Event::Entered { id, res } => self.entered(id, res),
            Event::Search(e) => self.on_search(e),
            Event::Threads(e) => self.on_threads(e),
            Event::Live(ev) => self.on_live(ev),
            // Sections and counts are session-token only; without them the
            // sidebar falls back to favourites, DMs and channels.
            Event::Sections(Err(_)) | Event::Counts(Err(_)) => {}
            Event::Auth(Err(e)) | Event::Users(Err(e)) | Event::Convs(Err(e)) | Event::Stars(Err(e)) => self.error = Some(e),
            Event::History { chan, fetch, res } => {
                self.loading.remove(&format!("{chan}:{}", fetch as u8));
                if fetch == Fetch::Poll {
                    self.polling = false;
                }
                match res {
                    Ok(new) if fetch == Fetch::Poll => self.poll_page(&chan, new),
                    Ok(new) => self.merge(&chan, fetch, new),
                    Err(e) => self.error = Some(e),
                }
            }
            Event::Sent { chan, text, res } => match res {
                Ok(m) => self.merge(&chan, Fetch::Poll, vec![m]),
                Err(e) => {
                    self.error = Some(e);
                    if self.editor.is_empty() && self.current.as_deref() == Some(chan.as_str()) {
                        self.editor.load(&text, false, &crate::editor::Lookup { users: &self.users, chans: &self.chans, emoji: &self.emoji, pictures: self.picker.is_some() });
                    }
                }
            },
            Event::Image { key, res, anim } => {
                // The workspace icon gets Slack's rounded corners.
                let res = if key.starts_with("avatar-team-") { res.map(round_corners) } else { res };
                if let Some(a) = anim {
                    self.anims.insert(key.clone(), a);
                }
                self.images.insert(key, res.map(Img::Ready).unwrap_or(Img::Failed));
            }
            Event::Presence(p) => self.on_presence(p.into_iter().collect()),
            Event::Profile { uid, res } => match res {
                Ok(p) => {
                    if let Some(card) = self.profile.as_mut().filter(|(u, _)| *u == uid) {
                        card.1 = Some(p);
                    }
                }
                Err(e) => {
                    self.profile = None;
                    self.error = Some(e);
                }
            },
            Event::Boot(Ok(b)) => {
                if config::sidebar_avatars().is_none() {
                    self.sidebar_avatars = b.sidebar_avatars;
                }
                self.boot = Some(b);
            }
            Event::Boot(Err(_)) | Event::TeamIcon(Err(_)) => {}
            Event::TeamIcon(Ok(url)) => self.team_icon = Some(url),
            Event::SidebarSaved(Ok(())) => {}
            Event::SidebarSaved(Err(e)) => {
                self.error = Some(e);
                self.spawn(|s| Event::Sections(s.sections()));
                self.spawn(|s| Event::Stars(s.stars()));
                self.spawn(|s| Event::Boot(s.boot()));
            }
            Event::Emoji(e) => self.on_emoji(e),
            Event::Opened(res) => match res {
                Ok(path) => {
                    self.status = String::new();
                    self.open_url(&path.to_string_lossy());
                }
                Err(e) => self.error = Some(trf!("open file: {e}", "abrir arquivo: {e}")),
            },
            Event::Account(e) => self.on_account(e),
            Event::Update(note) => self.on_update(note),
            Event::Edit(e) => self.on_edit(e),
            Event::Downloaded { key, res } => match res {
                Ok(path) => {
                    self.status = trf!("✓ saved to {}", "✓ salvo em {}", files::pretty(&path));
                    self.downloads.insert(key, files::State::Saved(path));
                }
                Err(e) => {
                    self.downloads.remove(&key);
                    self.error = Some(format!("download: {e}"));
                }
            },
            // Slack keeps sections as a linked list; reload it so the next move
            // starts from what Slack has, hidden sections included.
            Event::SectionMoved(res) => {
                if let Err(e) = res {
                    self.error = Some(e);
                }
                self.spawn(|s| Event::Sections(s.sections()));
            }
        }
    }

    /// How often the open conversation and thread are fetched: often without
    /// the websocket, rarely with it, since it says when there is news.
    pub(crate) fn poll_every(&self) -> Duration {
        if self.live { LIVE_FALLBACK } else { POLL_EVERY }
    }

    /// Who is typing in a conversation, by name, most recent last.
    pub fn typing_in(&self, chan: &str) -> Vec<String> {
        let Some(t) = self.typing.get(chan) else { return Vec::new() };
        let mut who: Vec<(&String, &Instant)> = t.iter().filter(|(_, at)| at.elapsed() < TYPING).collect();
        who.sort_by_key(|(_, at)| **at);
        who.into_iter().map(|(u, _)| self.users.names.get(u).cloned().unwrap_or_else(|| u.clone())).collect()
    }

    /// `client.counts`: the conversations' and, from the same answer, the threads'.
    fn fetch_counts(&self) {
        let tx = self.tx.clone();
        self.spawn(move |s| match s.all_counts() {
            Ok((convs, threads)) => {
                let _ = tx.send(Event::Threads(crate::threads::Ev::Counts(threads)));
                Event::Counts(Ok(convs))
            }
            Err(e) => Event::Counts(Err(e)),
        });
    }

    /// Fetches the counts in a moment, sooner than the next poll.
    pub(crate) fn counts_soon(&mut self) {
        let at = Instant::now() + Duration::from_millis(250);
        self.counts_at = Some(self.counts_at.map_or(at, |t| t.min(at)));
    }

    /// The websocket says something changed: fetch it now.
    fn on_live(&mut self, ev: crate::rtm::Ev) {
        use crate::rtm::Ev;
        match ev {
            Ev::Up => {
                self.live = true;
                // Whatever happened while it was down.
                self.counts_soon();
                self.refresh_prefs();
                self.last_poll = Instant::now() - POLL_EVERY;
            }
            Ev::Down => {
                // Polling takes presence back on its next round.
                self.live = false;
                self.presence_live = false;
            }
            Ev::Reaction { added, chan, ts, name, user, item_user } => {
                self.on_reaction(added, &chan, &ts, &name, &user, item_user.as_deref());
                // The conversation shows the new count as before.
                self.on_live(Ev::Message { chan, thread: None, user: None });
            }
            Ev::Presence(p) => {
                self.presence_live = true;
                self.on_presence(p);
            }
            // Away set by hand somewhere else: the account says which.
            Ev::MyPresence => self.refresh_account(false),
            Ev::Typing { chan, user } => {
                if !self.is_me(&user) {
                    self.typing.entry(chan).or_default().insert(user, Instant::now());
                }
            }
            Ev::Message { chan, thread, user } => {
                self.thread_message(&chan, thread.as_deref());
                // The message they were typing arrived.
                if let (Some(u), Some(t)) = (user, self.typing.get_mut(&chan)) {
                    t.remove(&u);
                }
                self.counts_soon();
                // Behind another workspace, the open conversation waits until it's on screen.
                if self.background {
                    return;
                }
                if self.current.as_deref() == Some(chan.as_str()) && !self.polling && self.messages.contains_key(&chan) {
                    self.polling = true;
                    self.last_poll = Instant::now();
                    self.fetch(chan.clone(), Fetch::Poll);
                }
                let open = self.thread.as_ref().is_some_and(|t| t.chan == chan && (thread.is_none() || thread.as_deref() == Some(t.ts.as_str())));
                if open {
                    self.refresh_thread();
                }
            }
            Ev::Counts => self.counts_soon(),
            Ev::Threads => {
                self.counts_soon();
                self.threads_soon();
            }
            Ev::Dnd => self.refresh_dnd(),
            Ev::Prefs => self.refresh_prefs(),
            Ev::Convs => self.spawn(|s| Event::Convs(s.conversations())),
            Ev::Ephemeral { chan, msg } => self.on_ephemeral(chan, msg),
            Ev::User(user) => self.on_user(&user),
            Ev::UserStale(uid) => self.refresh_user(uid),
            Ev::Archived { chan, archived } => {
                if let Some(c) = self.convs.iter_mut().find(|c| c.id == chan) {
                    c.archived = archived;
                }
            }
            // Deleted by someone else (ours is already gone: a no-op).
            Ev::Deleted(chan) => {
                let open = self.current.as_deref() == Some(chan.as_str());
                if self.convs.iter().any(|c| c.id == chan) {
                    self.forget_channel(&chan);
                    if open {
                        self.brief(tr("This channel was deleted", "Este canal foi apagado").into(), 3);
                    }
                }
            }
        }
    }

    /// The terminal got or lost the keyboard. Coming back reads what arrived
    /// in the open conversation meanwhile.
    pub fn set_focused(&mut self, focused: bool) {
        if self.focused == focused {
            return;
        }
        self.focused = focused;
        self.dirty = true;
        // The thread open in the panel too.
        self.read_open_thread();
        let Some(chan) = self.current.clone().filter(|_| focused) else { return };
        if let Some(c) = self.counts.get_mut(&chan).filter(|c| c.unread || c.mentions > 0) {
            c.unread = false;
            c.mentions = 0;
            self.mark_read(&chan);
        }
    }

    /// A command from another process (`ipc`).
    fn on_remote(&mut self, cmd: crate::ipc::Cmd) {
        use crate::ipc::Cmd;
        match cmd {
            Cmd::Focus => focus_window(),
            Cmd::Open { conv, ts, thread, focus, .. } => {
                if focus {
                    focus_window();
                }
                if self.login.is_some() {
                    return;
                }
                self.menu = None;
                self.viewer = None;
                self.dialog = None;
                self.open(conv.clone());
                // A thread alone (`--thread <root>`) opens it at its root.
                let Some(ts) = ts.or_else(|| thread.clone()) else { return };
                self.flash = Some((ts.clone(), Instant::now()));
                match thread {
                    Some(parent) => {
                        self.open_thread(parent.clone());
                        if let Some(t) = self.thread.as_mut() {
                            t.jump = (ts != parent).then_some(ts);
                        }
                        self.show_message(conv, parent);
                    }
                    None => self.show_message(conv, ts),
                }
            }
            Cmd::MarkRead { conv, .. } => self.run_action(Action::MarkRead(conv)),
            Cmd::Pause(minutes) => self.pause_notifications(minutes),
            Cmd::Resume => self.resume_everywhere(),
            Cmd::Quit => self.quit = true,
        }
    }

    /// Scrolls the conversation to a message, fetching the history around it
    /// when it is not loaded.
    pub(crate) fn show_message(&mut self, chan: String, ts: String) {
        if self.messages.get(&chan).is_some_and(|l| l.binary_search_by(|m| m.ts.cmp(&ts)).is_ok()) {
            self.pins.jump = Some(ts);
            return;
        }
        self.status = tr("loading the message…", "carregando a mensagem…").into();
        self.spawn(move |s| {
            let res = s.around(&chan, &ts);
            Event::Around { chan, ts, res }
        });
    }

    fn on_around(&mut self, chan: String, ts: String, res: Res<(Vec<Message>, bool, bool)>) {
        self.status.clear();
        let (msgs, first, present) = match res {
            Ok(r) => r,
            Err(e) => return self.error = Some(e),
        };
        if first {
            self.no_older.insert(chan.clone());
        } else {
            self.no_older.remove(&chan);
        }
        if present {
            self.detached.remove(&chan);
        } else {
            self.detached.insert(chan.clone());
        }
        self.messages.insert(chan.clone(), msgs);
        if self.current.as_deref() == Some(chan.as_str()) {
            self.stay = None;
            self.pins.jump = Some(ts);
            if let Some(f) = self.flash.as_mut() {
                f.1 = Instant::now();
            }
        }
    }

    /// Back to the newest messages; a stretch away from the present is dropped
    /// for the newest page.
    pub(crate) fn to_present(&mut self) {
        let Some(chan) = self.current.clone() else { return };
        self.scroll = 0;
        self.stay = None;
        self.pins.jump = None;
        if self.detached.remove(&chan) {
            self.messages.remove(&chan);
            self.fetch(chan, Fetch::Initial);
        }
    }

    /// A poll's newest page. A stretch away from the present only takes it
    /// once the two meet, keeping on screen what was there.
    fn poll_page(&mut self, chan: &str, page: Vec<Message>) {
        if self.detached.contains(chan) {
            let last = self.messages.get(chan).and_then(|l| l.last()).map(|m| m.ts.clone());
            let meets = page.len() < HISTORY_LIMIT || page.first().zip(last.as_ref()).is_some_and(|(f, l)| f.ts <= *l);
            if !meets {
                return;
            }
            self.detached.remove(chan);
            if self.current.as_deref() == Some(chan) {
                self.stay = last.map(|l| (l, self.scroll));
            }
        }
        self.merge_page(chan, page);
    }

    /// The newest page replaces what it covers, so messages gone from it were
    /// deleted; older history stays, and so do sends newer than the page.
    fn merge_page(&mut self, chan: &str, page: Vec<Message>) {
        let old = std::mem::take(self.messages.entry(chan.to_string()).or_default());
        let last = old.last().map(|m| m.ts.clone());
        let start = if page.len() >= HISTORY_LIMIT { page.first().map(|m| m.ts.clone()) } else { None };
        let end = page.last().map(|m| m.ts.clone());
        let mut merged: Vec<Message> = old.iter().filter(|m| start.as_ref().is_some_and(|s| m.ts < *s)).cloned().collect();
        // Answers only you see are never in a page: they stay where they were.
        let ephemeral: Vec<Message> = old
            .iter()
            .filter(|m| crate::slash::is_ephemeral(m) && start.as_ref().is_none_or(|s| m.ts >= *s) && end.as_ref().is_none_or(|e| m.ts <= *e))
            .cloned()
            .collect();
        merged.extend(page);
        // A send can land before the poll that missed it.
        merged.extend(old.into_iter().filter(|m| end.as_ref().is_some_and(|e| m.ts > *e)));
        if !ephemeral.is_empty() {
            merged.extend(ephemeral);
            merged.sort_by(|a, b| a.ts.cmp(&b.ts));
        }
        let grew = merged.last().map(|m| &m.ts) != last.as_ref();
        self.messages.insert(chan.to_string(), merged);
        if grew && self.focused && self.current.as_deref() == Some(chan) {
            self.mark_read(chan);
        }
    }

    fn merge(&mut self, chan: &str, fetch: Fetch, new: Vec<Message>) {
        // A jump's stretch of history (`on_around`) landed after the open
        // asked for the newest page: it stays, the way back is `to_present`.
        if fetch == Fetch::Initial && self.detached.contains(chan) {
            return;
        }
        let is_current = self.current.as_deref() == Some(chan);
        let list = self.messages.entry(chan.to_string()).or_default();
        let grew = match fetch {
            Fetch::Initial => {
                self.detached.remove(chan);
                if new.len() < HISTORY_LIMIT {
                    self.no_older.insert(chan.to_string());
                } else {
                    self.no_older.remove(chan);
                }
                *list = new;
                true
            }
            Fetch::Poll => {
                // A poll and a send can land in either order: insert by ts, skip dupes.
                let before = list.len();
                for m in new {
                    if let Err(pos) = list.binary_search_by(|x| x.ts.cmp(&m.ts)) {
                        list.insert(pos, m);
                    }
                }
                list.len() > before
            }
            Fetch::Older => {
                if new.len() < HISTORY_LIMIT {
                    self.no_older.insert(chan.to_string());
                }
                let first = list.first().map(|m| m.ts.clone()).unwrap_or_default();
                let mut older: Vec<Message> = new.into_iter().filter(|m| first.is_empty() || m.ts < first).collect();
                older.append(list);
                *list = older;
                false
            }
            Fetch::Newer => {
                let last = list.last().map(|m| m.ts.clone()).unwrap_or_default();
                let present = new.len() < HISTORY_LIMIT;
                list.extend(new.into_iter().filter(|m| m.ts > last));
                if is_current {
                    self.stay = Some((last, self.scroll));
                }
                if present {
                    self.detached.remove(chan);
                }
                present
            }
        };
        if fetch == Fetch::Initial {
            self.find_new_line(chan, true);
        }
        if grew && is_current && self.focused {
            self.mark_read(chan);
        }
    }

    // ---- login ---------------------------------------------------------

    pub fn open_login(&mut self, message: Option<String>) {
        if self.login.is_none() {
            let workspace = config::workspace().unwrap_or_default();
            self.login = Some(Login { message, workspace, ..Login::default() });
        } else if let Some(l) = self.login.as_mut() {
            if message.is_some() && !l.checking {
                l.message = message;
            }
        }
        self.error = None;
        self.dirty = true;
    }

    /// Slack's own sign-in page in a window; the session comes back by itself.
    fn login_window(&mut self) {
        let Some(l) = self.login.as_mut() else { return };
        let workspace = if l.workspace.trim().is_empty() {
            None
        } else {
            match config::workspace_name(&l.workspace) {
                Some(w) => Some(w),
                None => {
                    let msg = tr("Invalid workspace address (e.g. mycompany).", "Endereço do workspace inválido (ex.: minhaempresa).");
                    return l.message = Some(msg.into());
                }
            }
        };
        l.checking = true;
        l.message = Some(tr("Sign in through the window that opened…", "Entre na janela de login que abriu…").into());
        let tx = self.tx.clone();
        // The other workspaces, which the window leaves alone: all of them on
        // "Add a workspace", the rest when signing in to this one again.
        let known: Vec<String> = self.workspaces.iter().filter(|w| !w.active && !w.id.is_empty()).map(|w| w.id.clone()).collect();
        std::thread::spawn(move || {
            let res = crate::login::sign_in(workspace.as_deref(), &known);
            let _ = tx.send(Event::Login(res));
        });
    }

    fn submit_login(&mut self) {
        let Some(l) = self.login.as_mut() else { return };
        let token = l.token.trim().to_string();
        let cookie = l.cookie.trim().to_string();
        if !token.starts_with("xox") {
            l.message = Some(tr("The token starts with xoxc- (or xoxp-).", "O token começa com xoxc- (ou xoxp-).").into());
            return;
        }
        if token.starts_with("xoxc-") && cookie.is_empty() {
            l.message = Some(tr("An xoxc- token needs the d cookie (xoxd-…).", "Token xoxc- precisa do cookie d (xoxd-…).").into());
            l.field = 1;
            return;
        }
        l.checking = true;
        l.message = Some(tr("Checking…", "Verificando…").into());
        let creds = Creds { token, cookie: (!cookie.is_empty()).then_some(cookie) };
        std::thread::spawn({
            let tx = self.tx.clone();
            move || {
                let res = Slack::new(creds.clone()).auth().map(|me| (me, creds));
                let _ = tx.send(Event::Login(res));
            }
        });
    }

    fn finish_login(&mut self, res: Res<(AuthInfo, Creds)>) {
        match res {
            // A new workspace: `Workspaces` keeps it, or updates the one it already has.
            Ok((me, creds)) if self.adding => self.added = Some((me, creds)),
            Ok((me, creds)) => {
                let old = self.me.as_ref().map(|m| m.team_id.clone()).filter(|t| *t != me.team_id);
                // Next to others, another workspace comes in through "Add a workspace".
                if old.is_some() && self.workspaces.len() > 1 {
                    if let Some(l) = self.login.as_mut() {
                        l.checking = false;
                        l.message = Some(tr("That's another workspace: add it from the workspace menu.", "Esse é outro workspace: adicione pelo menu do workspace.").into());
                    }
                    return;
                }
                // Alone, it replaces the one signed in before.
                if let Some(old) = old {
                    let _ = config::remove_workspace(&old);
                }
                // A legacy session never confirmed: the new one takes its keys.
                if self.legacy.take().is_some() {
                    let _ = config::logout();
                }
                let saved = crate::workspaces::save_session(&me, &creds);
                self.sign_in(creds);
                if let Err(e) = saved {
                    self.error = Some(trf!("didn't save the config: {e}", "não salvou o config: {e}"));
                }
            }
            Err(e) => {
                if let Some(l) = self.login.as_mut() {
                    l.checking = false;
                    l.message = Some(if is_auth_error(&e) {
                        tr("Slack refused: check the token and the cookie.", "O Slack recusou: confira o token e o cookie.").into()
                    } else {
                        e
                    });
                }
            }
        }
    }

    /// Takes a checked session: the sign-in screen closes and everything loads.
    pub(crate) fn sign_in(&mut self, creds: Creds) {
        self.slack.set_creds(creds);
        self.login = None;
        self.error = None;
        self.loading.clear();
        self.polling = false;
        self.refresh();
        if let Some(chan) = self.current.clone() {
            self.fetch(chan, Fetch::Initial);
        }
    }

    /// "Sign out": the session is forgotten with everything of the account,
    /// as when another account signs in, and the login screen opens. With
    /// other workspaces signed in, only this one goes and another takes the screen.
    pub(crate) fn logout(&mut self) {
        let team = self.me.as_ref().map(|m| m.team_id.clone());
        let listed = team.as_ref().is_some_and(|t| config::workspaces().iter().any(|w| w.team_id.as_ref() == Some(t)));
        let res = match team.filter(|_| listed) {
            Some(t) => config::remove_workspace(&t),
            None => config::logout(),
        };
        if let Err(e) = res {
            self.error = Some(trf!("didn't save the config: {e}", "não salvou o config: {e}"));
        }
        self.slack.set_creds(Creds::default());
        if self.workspaces.len() > 1 {
            self.signed_out = true;
            return;
        }
        self.me = None;
        self.users = Users::default();
        self.emoji = emoji::Emoji::default();
        self.convs.clear();
        self.chans.clear();
        self.private.clear();
        self.others.clear();
        self.asked.clear();
        self.sections.clear();
        self.stars.clear();
        self.vips.clear();
        self.counts.clear();
        self.messages.clear();
        self.recents.clear();
        self.presence.clear();
        self.no_older.clear();
        self.loading.clear();
        self.polling = false;
        (self.boot, self.team_icon, self.current, self.profile, self.thread, self.viewer) = (None, None, None, None, None, None);
        self.threads = Default::default();
        (self.menu, self.react, self.dialog) = (None, None, None);
        self.account = Default::default();
        self.editor = Editor::default();
        self.filter.clear();
        self.focus = Focus::Sidebar;
        self.list.select(None);
        self.open_login(Some(tr("You signed out of slick.", "Você saiu do slick.").into()));
    }

    /// Pasted text lands in the field it looks like, whatever is focused.
    fn login_paste(&mut self, s: &str) {
        let Some(l) = self.login.as_mut() else { return };
        let s = s.trim();
        if s.starts_with("xoxc-") || s.starts_with("xoxp-") {
            l.token = s.to_string();
            l.field = 1;
        } else if s.starts_with("xoxd-") {
            l.cookie = s.to_string();
            l.field = 0;
        } else if l.token.is_empty() && l.cookie.is_empty() {
            l.workspace = s.to_string();
        } else if l.field == 0 {
            l.token.push_str(s);
        } else {
            l.cookie.push_str(s);
        }
        l.message = None;
    }

    fn login_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let Some(l) = self.login.as_mut() else { return };
        if l.checking {
            return;
        }
        let cookie_empty = l.cookie.is_empty();
        let token_empty = l.token.is_empty();
        // Keys type the workspace, unless a session is being pasted by hand.
        let field = if token_empty && cookie_empty {
            &mut l.workspace
        } else if l.field == 0 {
            &mut l.token
        } else {
            &mut l.cookie
        };
        match k.code {
            // "Add a workspace" given up: back to the one before.
            KeyCode::Esc if self.adding => self.discard = true,
            KeyCode::Esc if self.me.is_some() || !self.convs.is_empty() => self.login = None,
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Up | KeyCode::Down => l.field = 1 - l.field,
            KeyCode::Enter if token_empty && cookie_empty => self.login_window(),
            KeyCode::Enter if l.field == 0 && cookie_empty => l.field = 1,
            KeyCode::Enter => self.submit_login(),
            KeyCode::Backspace => {
                field.pop();
            }
            KeyCode::Char('u') if ctrl => field.clear(),
            KeyCode::Char(c) if !ctrl => field.push(c),
            _ => {}
        }
    }

    // ---- keys and mouse ------------------------------------------------

    pub fn on_paste(&mut self, s: &str) {
        self.dirty = true;
        if self.login.is_some() {
            return self.login_paste(s);
        }
        if self.help.is_some() {
            return self.help_paste(s);
        }
        if self.search.open {
            return self.search_paste(s);
        }
        if self.dialog.is_some() {
            return self.dialog_paste(s);
        }
        if self.account.dialog.is_some() {
            return self.status_paste(s);
        }
        // No text to type in the pause dialog.
        if self.account.pause.is_some() {
            return;
        }
        if self.account.profile.is_some() {
            return self.profile_paste(s);
        }
        if self.details.is_some() {
            return self.details_paste(s);
        }
        if self.invite.is_some() {
            return self.people_paste(s);
        }
        match self.focus {
            // No message box in a read-only conversation.
            Focus::Input if self.read_only().is_some() => {}
            // A pasted picture arrives empty: the terminal can't carry it.
            Focus::Input | Focus::Thread if s.is_empty() => self.paste_clipboard(self.focus == Focus::Thread),
            Focus::Input => self.editor.insert(s),
            Focus::Filter => self.filter.push_str(s.lines().next().unwrap_or("")),
            Focus::Thread => self.thread.iter_mut().for_each(|t| t.editor.insert(s)),
            Focus::Sidebar => {}
        }
    }

    pub fn on_key(&mut self, k: KeyEvent) {
        self.dirty = true;
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        // Ctrl+Shift+C, where the terminal lets it through, is code in the box.
        let code = k.modifiers.contains(KeyModifiers::SHIFT) && self.typing_editor().is_some();
        if ctrl && k.code == KeyCode::Char('c') && !code {
            // With a message picked, Ctrl+C copies it, as in Slack.
            if self.login.is_none() && self.menu.is_none() && self.edit.forward.is_none() && self.copy_selected() {
                return;
            }
            return self.quit = true;
        }
        if self.login.is_some() {
            return self.login_key(k);
        }
        // The shortcuts guide sits over everything and takes every key;
        // Ctrl+/ opens it from anywhere, the message box included.
        if self.help.is_some() {
            return self.help_key(k);
        }
        if crate::help::is_toggle(&k) {
            return self.toggle_help();
        }
        // The search window, over the rest: every key but those above.
        if self.search.open {
            return self.search_key(k);
        }
        if ctrl && k.code == KeyCode::Char('l') {
            return self.open_login(None);
        }
        if ctrl && k.code == KeyCode::Char('o') && matches!(self.update, Some(crate::update::State::Installed(_))) {
            self.restart = true;
            return self.quit = true;
        }
        if self.dialog.is_some() {
            return self.dialog_key(k);
        }
        if self.account.dialog.is_some() {
            return self.status_key(k);
        }
        if self.account.pause.is_some() {
            return self.pause_key(k);
        }
        if self.account.profile.is_some() {
            return self.profile_key(k);
        }
        if self.account.about {
            return self.about_key(k);
        }
        if self.account.settings.is_some() {
            return self.settings_key(k);
        }
        if self.details.is_some() {
            return self.details_key(k);
        }
        if self.invite.is_some() {
            return self.people_key(k);
        }
        let (alt, shift) = (k.modifiers.contains(KeyModifiers::ALT), k.modifiers.contains(KeyModifiers::SHIFT));
        // The conversation's details, or a DM's profile, as a click on its name;
        // Ctrl+Shift+I as in Slack.
        if (alt && k.code == KeyCode::Char('i')) || (ctrl && shift && matches!(k.code, KeyCode::Char('i' | 'I'))) {
            return self.open_details();
        }
        if ctrl && k.code == KeyCode::Char('n') {
            self.account.menu = None;
            return self.open_compose_menu();
        }
        if ctrl && k.code == KeyCode::Char('p') {
            return match self.account.menu.take() {
                Some(_) => {}
                None => self.open_account_menu(),
            };
        }
        if self.account.menu.is_some() {
            return self.account_key(k);
        }
        if self.menu.is_some() {
            return self.menu_key(k);
        }
        if self.react.is_some() {
            return self.react_key(k);
        }
        if self.edit.forward.is_some() {
            return self.forward_key(k);
        }
        if self.team_popup && self.switcher_key(k) {
            return;
        }
        if let Some(r) = self.viewer.clone() {
            match k.code {
                KeyCode::Char('o') => self.open_external(&r),
                KeyCode::Char('d') => self.save_file(files::Job::image(&r)),
                KeyCode::Char('v') | KeyCode::Char('b') => {
                    if let Some(link) = &r.link {
                        self.open_url(link);
                    }
                }
                KeyCode::Char('c') if ctrl => self.quit = true,
                _ => self.viewer = None,
            }
            return;
        }
        if self.global_key(k, ctrl, alt, shift) {
            return;
        }
        if k.code == KeyCode::Esc && self.selection.take().is_some_and(|s| s.moved) {
            return;
        }
        if self.format_key(k) {
            return;
        }
        if self.emoji_key(k) || self.mention_key(k) || self.slash_key(k) {
            return;
        }
        if self.edit_key(k) {
            return;
        }
        if self.profile.is_some() && k.code == KeyCode::Esc {
            self.profile = None;
            return;
        }
        if self.thread.is_some() && k.code == KeyCode::Esc && self.focus != Focus::Thread {
            return self.close_thread();
        }
        if self.threads_key(k, ctrl) || self.pins_key(k) {
            return;
        }
        match (k.code, ctrl) {
            (KeyCode::Char('c'), true) => return self.quit = true,
            // Ctrl+T too, as in Slack.
            (KeyCode::Char('k' | 't'), true) => return self.open_search(),
            (KeyCode::Char('r'), true) => {
                self.error = None;
                self.dir_force = true;
                return self.refresh();
            }
            (KeyCode::PageUp, _) if self.focus == Focus::Thread => return self.scroll_thread(10),
            (KeyCode::PageDown, _) if self.focus == Focus::Thread => return self.scroll_thread(-10),
            (KeyCode::PageUp, _) => return self.scroll_by(10),
            (KeyCode::PageDown, _) => return self.scroll_down(10),
            (KeyCode::Tab, _) => {
                self.focus = match self.focus {
                    Focus::Input if self.thread.is_some() => Focus::Thread,
                    Focus::Input | Focus::Thread => Focus::Sidebar,
                    _ => Focus::Input,
                };
                return;
            }
            _ => {}
        }
        match self.focus {
            Focus::Sidebar => match k.code {
                KeyCode::Char('q') => self.quit = true,
                KeyCode::Char('/') => {
                    self.focus = Focus::Filter;
                    self.list.select(None);
                }
                // Alt+↑/↓ on a header does it too (`global_key`).
                KeyCode::Char('K') | KeyCode::Char('J') => {
                    self.move_selected_section(if k.code == KeyCode::Char('K') { -1 } else { 1 });
                }
                KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
                KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
                KeyCode::Home | KeyCode::Char('g') => self.list.select(Some(0)),
                KeyCode::End | KeyCode::Char('G') => self.move_selection(isize::MAX / 2),
                KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') | KeyCode::Char(' ') => {
                    self.activate(self.list.selected().unwrap_or(0))
                }
                KeyCode::Char('m') | KeyCode::Menu => {
                    let row = self.list.selected().unwrap_or(0);
                    let y = self.areas.sidebar.y + row.saturating_sub(self.list.offset()) as u16;
                    self.open_menu(row, self.areas.sidebar.x + 4, y + 1);
                }
                KeyCode::Esc => self.error = None,
                _ => {}
            },
            Focus::Filter => match k.code {
                KeyCode::Esc => {
                    self.filter.clear();
                    self.focus = Focus::Sidebar;
                }
                KeyCode::Enter => {
                    if let Some(row) = self.list.selected() {
                        self.activate(row);
                    }
                }
                KeyCode::Up => self.move_selection(-1),
                KeyCode::Down => self.move_selection(1),
                KeyCode::Backspace => {
                    self.filter.pop();
                    self.sidebar_wheel = false;
                    self.list.select(Some(0));
                }
                KeyCode::Char(c) if !ctrl => {
                    self.filter.push(c);
                    self.sidebar_wheel = false;
                    self.list.select(Some(0));
                }
                _ => {}
            },
            Focus::Input => self.on_input_key(k, ctrl),
            Focus::Thread => self.on_thread_key(k, ctrl),
        }
    }

    /// Slack's keys that work wherever the focus is, once no dialog or popup
    /// has the key. True when handled.
    fn global_key(&mut self, k: KeyEvent, ctrl: bool, alt: bool, shift: bool) -> bool {
        let delta = if matches!(k.code, KeyCode::Up | KeyCode::Left) { -1 } else { 1 };
        match k.code {
            KeyCode::Up | KeyCode::Down if alt && shift && !ctrl => self.open_unread(delta),
            // The search's own list keeps its arrows.
            KeyCode::Up | KeyCode::Down if alt && !ctrl && self.focus != Focus::Filter => {
                // On a section's header in the sidebar it moves the section instead.
                if !(self.focus == Focus::Sidebar && self.move_selected_section(delta)) {
                    self.open_next(delta);
                }
            }
            KeyCode::Left | KeyCode::Right if alt && !ctrl && !shift => self.go_history(delta),
            KeyCode::Esc if shift && !ctrl && !alt => self.mark_all_read(),
            KeyCode::Char('t' | 'T') if ctrl && shift => {
                self.open_threads();
                let rows = self.rows();
                if let Some(pos) = rows.iter().position(|r| matches!(r, Row::Threads)) {
                    self.list.select(Some(pos));
                }
            }
            KeyCode::Char('y' | 'Y') if ctrl && shift && self.me.is_some() => self.open_status_dialog(),
            KeyCode::Char(',') if ctrl => self.open_settings(),
            KeyCode::Char('.') if ctrl => self.close_panel(),
            // Ctrl+Shift+\: a reaction to the conversation's last message.
            KeyCode::Char('\\') if ctrl && shift => self.react_last(),
            KeyCode::Char('|') if ctrl => self.react_last(),
            _ => return false,
        }
        true
    }

    /// Moves the section whose header is selected in the sidebar; false
    /// when the selection is not a header.
    fn move_selected_section(&mut self, delta: isize) -> bool {
        let rows = self.rows();
        let Some(Row::Header { key, .. }) = self.list.selected().and_then(|i| rows.get(i)) else { return false };
        let key = key.clone();
        self.move_section(&key, delta);
        true
    }

    fn scroll_by(&mut self, n: usize) {
        self.scroll = (self.scroll + n).min(self.max_scroll);
        if self.scroll == self.max_scroll {
            if let Some(chan) = self.current.clone() {
                if !self.no_older.contains(&chan) {
                    self.fetch(chan, Fetch::Older);
                }
            }
        }
    }

    /// Toward the newest messages; at the end of a stretch away from the
    /// present, loads what came after it.
    fn scroll_down(&mut self, n: usize) {
        self.scroll = self.scroll.saturating_sub(n);
        if self.scroll == 0 {
            if let Some(chan) = self.current.clone().filter(|c| self.detached.contains(c)) {
                self.fetch(chan, Fetch::Newer);
            }
        }
    }

    fn on_input_key(&mut self, k: KeyEvent, ctrl: bool) {
        // Alt with the arrows is the sidebar's and the history's (`global_key`).
        if k.modifiers.contains(KeyModifiers::ALT) && matches!(k.code, KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right) {
            return;
        }
        // A deactivated account's archives, an archived channel or one you
        // are not in: reading only, nothing to type in; `u` unarchives the
        // channel, Enter or `j` joins it.
        if let Some(why) = self.read_only() {
            match k.code {
                KeyCode::Esc => self.focus = Focus::Sidebar,
                KeyCode::Char('u') if why == ReadOnly::Archived && !ctrl => {
                    if let Some(id) = self.current.clone() {
                        self.unarchive(id);
                    }
                }
                KeyCode::Enter | KeyCode::Char('j') if why == ReadOnly::Outside && !ctrl => {
                    if let Some(id) = self.current.clone() {
                        self.join(id);
                    }
                }
                KeyCode::End => self.to_present(),
                KeyCode::Up => self.scroll_by(1),
                KeyCode::Down => self.scroll_down(1),
                _ => {}
            }
            return;
        }
        let newline = k.modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::ALT);
        match k.code {
            KeyCode::Esc => self.focus = Focus::Sidebar,
            KeyCode::Enter if newline || self.editor.in_code() => self.editor.newline(),
            KeyCode::Char('j') if ctrl => self.editor.newline(),
            // Clears the whole box, handy when editing a message.
            KeyCode::Char('d') if ctrl => self.editor.set(String::new()),
            KeyCode::Char('v') if ctrl => self.paste_clipboard(false),
            KeyCode::Enter => self.send(),
            KeyCode::Left if ctrl => self.editor.word_left(),
            KeyCode::Right if ctrl => self.editor.word_right(),
            KeyCode::Left => self.editor.left(),
            KeyCode::Right => self.editor.right(),
            KeyCode::Home => self.editor.home(),
            // Like the "↓ Últimas mensagens" button.
            KeyCode::End if ctrl || self.editor.is_empty() => self.to_present(),
            KeyCode::End => self.editor.end(),
            KeyCode::Up => {
                if !self.editor.vertical(self.input_width, -1) {
                    self.scroll_by(1);
                }
            }
            KeyCode::Down => {
                if !self.editor.vertical(self.input_width, 1) {
                    self.scroll_down(1);
                }
            }
            KeyCode::Backspace if self.editor.text.is_empty() && self.drop_last(false) => {}
            KeyCode::Backspace if ctrl => self.editor.delete_word(),
            KeyCode::Backspace => self.editor.backspace(),
            KeyCode::Delete if ctrl => self.editor.delete_word_forward(),
            KeyCode::Delete => self.editor.delete(),
            KeyCode::Char('u') if ctrl => {
                self.editor.take();
            }
            KeyCode::Char('w') if ctrl => self.editor.delete_word(),
            KeyCode::Char(c) if !ctrl => {
                let mut b = [0; 4];
                self.editor.insert(c.encode_utf8(&mut b));
                if c == ':' {
                    self.editor.shortcode(&crate::editor::Lookup { users: &self.users, chans: &self.chans, emoji: &self.emoji, pictures: self.picker.is_some() });
                }
            }
            _ => {}
        }
    }

    pub fn on_mouse(&mut self, m: MouseEvent) {
        let at = Position::new(m.column, m.row);
        if let Some(l) = &self.login {
            if m.kind == MouseEventKind::Down(MouseButton::Left) && !l.checking && self.areas.login_button.contains(at) {
                self.login_window();
                self.dirty = true;
            }
            return;
        }
        if self.help.is_some() {
            return self.help_mouse(m);
        }
        if self.search.open {
            return self.search_mouse(m);
        }
        if self.details.is_some() {
            return self.details_mouse(m);
        }
        if self.account.pause.is_some() {
            return self.pause_mouse(m);
        }
        if self.invite.is_some() {
            return self.people_mouse(m);
        }
        // The workspace dropdown sits over the sidebar: it sees the mouse first.
        if self.team_popup && self.switcher_mouse(m) {
            return;
        }
        let covered = self.menu.is_some() || self.viewer.is_some() || self.react.is_some() || self.dialog.is_some() || self.account.profile.is_some() || self.account.about || self.account.settings.is_some();
        if m.kind == MouseEventKind::Down(MouseButton::Left) && !covered && self.areas.latest.contains(at) {
            self.to_present();
            self.dirty = true;
            return;
        }
        if m.kind == MouseEventKind::Down(MouseButton::Left) && !covered && self.areas.unarchive.contains(at) {
            match (self.current.clone(), self.read_only()) {
                (Some(id), Some(ReadOnly::Archived)) => self.unarchive(id),
                (Some(id), Some(ReadOnly::Outside)) => self.join(id),
                _ => {}
            }
            self.dirty = true;
            return;
        }
        if !covered && self.account.menu.is_none() && self.edit.forward.is_none() && self.attach_mouse(m) {
            return;
        }
        if self.selection_mouse(m) {
            return;
        }
        if self.compose_mouse(m) || self.account_mouse(m) {
            return;
        }
        if self.edit_mouse(m) {
            return;
        }
        if let Some(menu) = self.menu.as_mut() {
            let hit = self.areas.menu_items.iter().find(|(r, _)| r.contains(at)).map(|(_, i)| *i);
            match m.kind {
                MouseEventKind::Moved | MouseEventKind::Drag(_) => {
                    if let Some(i) = hit.filter(|&i| i != menu.sel) {
                        menu.sel = i;
                        self.dirty = true;
                    }
                }
                MouseEventKind::Down(MouseButton::Left) if hit.is_some() => {
                    let action = menu.items[hit.unwrap()].1.clone();
                    self.run_action(action);
                }
                MouseEventKind::Down(_) if !self.areas.menu.contains(at) => {
                    self.menu = None;
                    self.dirty = true;
                    // A right click elsewhere in the sidebar opens that row's menu instead.
                    if m.kind != MouseEventKind::Down(MouseButton::Right) {
                        return;
                    }
                }
                _ => return,
            }
            if self.menu.is_some() {
                return;
            }
        }
        if let Some(r) = self.viewer.clone() {
            if let MouseEventKind::Down(_) = m.kind {
                if self.areas.viewer_link.contains(at) {
                    if let Some(link) = &r.link {
                        self.open_url(link);
                    }
                } else if self.areas.viewer_save.contains(at) {
                    let click = match self.downloads.get(&r.key) {
                        Some(files::State::Saved(p)) => files::Click::Reveal(p.clone()),
                        _ => files::Click::Save(files::Job::image(&r)),
                    };
                    self.file_click(click, 0, 0);
                } else if !self.areas.viewer.contains(at) {
                    self.viewer = None;
                    self.dirty = true;
                }
            }
            return;
        }
        if self.emoji_mouse(m) || self.mention_mouse(m) || self.slash_mouse(m) {
            return;
        }
        // Dragging the border resizes the sidebar.
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) if self.areas.sidebar_border.contains(at) => {
                self.resizing = true;
                return;
            }
            MouseEventKind::Drag(MouseButton::Left) if self.resizing => {
                self.set_sidebar_width(m.column + 1);
                self.protocols.clear();
                self.dirty = true;
                return;
            }
            MouseEventKind::Up(_) if self.resizing => {
                self.resizing = false;
                let _ = config::set(&[("sidebar_width", self.sidebar_width.to_string())]);
                return;
            }
            _ => {}
        }
        // "↑ Unread mentions" / "↑ More unreads": scrolls to that row and selects it, without opening it.
        if m.kind == MouseEventKind::Down(MouseButton::Left) {
            if let Some(&(_, n)) = self.areas.mention_pills.iter().find(|(r, _)| r.contains(at)) {
                if n < self.rows().len() {
                    self.sidebar_wheel = false;
                    self.focus = Focus::Sidebar;
                    self.list.select(Some(n));
                }
                self.dirty = true;
                return;
            }
        }
        // Sidebar rows: a click opens, a drag moves, a right click shows the menu.
        let in_side = self.areas.sidebar.contains(at);
        let row_at = |app: &Self| app.list.offset() + (m.row.saturating_sub(app.areas.sidebar.y)) as usize;
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) if in_side => {
                let row = row_at(self);
                if row < self.rows().len() {
                    self.drag = Some(Drag { from: row, over: None });
                }
                return;
            }
            MouseEventKind::Drag(MouseButton::Left) if self.drag.is_some() => {
                let over = in_side.then(|| row_at(self)).filter(|&r| r < self.rows().len());
                if let Some(d) = self.drag.as_mut() {
                    let over = over.filter(|&r| r != d.from || d.over.is_some());
                    if d.over != over {
                        d.over = over;
                        self.dirty = true;
                    }
                }
                return;
            }
            MouseEventKind::Up(MouseButton::Left) if self.drag.is_some() => {
                let d = self.drag.take().unwrap();
                match d.over {
                    Some(over) if over != d.from => self.drop_row(d.from, over),
                    Some(_) => {}
                    None if in_side && row_at(self) == d.from => self.activate(d.from),
                    None => {}
                }
                self.dirty = true;
                return;
            }
            MouseEventKind::Down(MouseButton::Right) if in_side => {
                let row = row_at(self);
                self.open_menu(row, m.column, m.row + 1);
                self.dirty = true;
                return;
            }
            _ => {}
        }
        if let MouseEventKind::Down(button) = m.kind {
            if button == MouseButton::Left && self.areas.team_name.contains(at) {
                self.team_popup = !self.team_popup;
                self.dirty = true;
                return;
            }
        }
        if let (MouseEventKind::Down(MouseButton::Left), Some((uid, data))) = (m.kind, self.profile.clone()) {
            if self.areas.profile_close.contains(at) {
                self.profile = None;
                self.dirty = true;
                return;
            }
            if self.areas.profile_browser.contains(at) {
                return self.open_url(&self.profile_url(&uid));
            }
            if self.areas.profile_star.contains(at) {
                if let Some(id) = self.dm_with(&uid) {
                    let on = !self.stars.contains(&id);
                    self.set_star(id, on);
                }
                self.dirty = true;
                return;
            }
            if let Some(p) = data.as_ref().filter(|_| self.areas.profile_edit.contains(at)) {
                return self.open_profile_editor(p);
            }
            if self.areas.profile_id.contains(at) {
                self.copy(&uid);
                self.brief_copied(trf!("ID {uid} copied", "ID {uid} copiado"));
                self.copied = Some(("id", Instant::now()));
                self.dirty = true;
                return;
            }
            if let Some(p) = data.as_ref().filter(|p| !p.email.is_empty() && self.areas.profile_email.contains(at)) {
                self.copy(&p.email);
                self.brief_copied(tr("Email copied", "E-mail copiado").into());
                self.copied = Some(("email", Instant::now()));
                self.dirty = true;
                return;
            }
            if self.areas.profile_vip.contains(at) {
                let on = !self.vips.contains(&uid);
                self.set_vip(uid, on);
                self.dirty = true;
                return;
            }
            if let Some(p) = data.filter(|p| !p.photo.is_empty() && self.areas.profile_photo.contains(at)) {
                self.viewer = Some(ImgRef::photo(&uid, &p.photo, &p.name, self.profile_url(&uid)));
                self.dirty = true;
                return;
            }
        }
        if let MouseEventKind::Down(MouseButton::Left) = m.kind {
            if let Some((_, c)) = self.areas.file_buttons.iter().find(|(rect, _)| rect.contains(at)) {
                return self.file_click(c.clone(), m.column, m.row);
            }
            if let Some((_, t)) = self.areas.buttons.iter().find(|(rect, _)| rect.contains(at)) {
                let url = match t {
                    crate::blocks::Target::Link(u) => u.clone(),
                    crate::blocks::Target::Button(b) => match &b.url {
                        Some(u) => u.clone(),
                        None => self.message_url(self.current.as_deref().unwrap_or(""), &b.ts),
                    },
                    // A channel we're not in opens all the same: its history reads fine.
                    crate::blocks::Target::Channel(id) => return self.open(id.clone()),
                    crate::blocks::Target::User(uid) => return self.open_dm(uid.clone()),
                    crate::blocks::Target::Copy(text) => {
                        let text = text.clone();
                        self.copy(&text);
                        self.brief_copied(tr("code copied", "código copiado").into());
                        self.copied = Some(("code", Instant::now()));
                        self.copied_code = text;
                        self.dirty = true;
                        return;
                    }
                };
                self.open_url(&url);
                return;
            }
            if let Some((_, r)) = self.areas.images.iter().find(|(rect, _)| rect.contains(at)) {
                self.viewer = Some(r.clone());
                self.dirty = true;
                return;
            }
            if self.areas.header_star.contains(at) {
                if let Some(id) = self.current.clone() {
                    let on = !self.stars.contains(&id);
                    self.set_star(id, on);
                }
                self.dirty = true;
                return;
            }
            let header_user = self.areas.header_user.iter().find(|(rect, _)| rect.contains(at));
            if let Some((_, uid)) = header_user.or_else(|| self.areas.avatars.iter().find(|(rect, _)| rect.contains(at))) {
                let uid = uid.clone();
                self.open_profile(uid);
                return;
            }
            if self.areas.header_name.contains(at) {
                self.open_details();
                self.dirty = true;
                return;
            }
        }
        if self.pins_mouse(m, at) || self.thread_mouse(m, at) || self.threads_mouse(m, at) {
            self.dirty = true;
            return;
        }
        let in_side = self.areas.sidebar.contains(at);
        let in_msgs = self.areas.messages.contains(at);
        match m.kind {
            MouseEventKind::ScrollUp if in_msgs => self.scroll_by(3),
            MouseEventKind::ScrollDown if in_msgs => self.scroll_down(3),
            MouseEventKind::ScrollUp if in_side => {
                self.sidebar_wheel = true;
                *self.list.offset_mut() = self.list.offset().saturating_sub(3);
            }
            MouseEventKind::ScrollDown if in_side => {
                self.sidebar_wheel = true;
                let max = self.rows().len().saturating_sub(self.areas.sidebar.height as usize);
                *self.list.offset_mut() = (self.list.offset() + 3).min(max);
            }
            // The box opens the search window; "/" keeps the sidebar's own filter.
            MouseEventKind::Down(MouseButton::Left) if self.areas.search.contains(at) => self.open_search(),
            MouseEventKind::Down(MouseButton::Left) if self.areas.input.contains(at) => self.focus = Focus::Input,
            MouseEventKind::Down(MouseButton::Left) if in_msgs => self.focus = Focus::Input,
            _ => return,
        }
        self.dirty = true;
    }

    fn send(&mut self) {
        let Some(chan) = self.current.clone().filter(|_| self.read_only().is_none()) else { return };
        if self.send_files(false) {
            return;
        }
        if self.editor.is_empty() || self.run_slash(false) {
            return;
        }
        let text = self.editor.take().trim().to_string();
        // What is sent shows with the newest messages.
        self.to_present();
        self.spawn(move |s| {
            let res = s.post(&chan, &text);
            Event::Sent { chan, text, res }
        });
    }
}

/// Standard base64, for OSC 52.
/// Opens a link, file or folder with the system's default app.
const OPENER: &str = if cfg!(windows) {
    "explorer"
} else if cfg!(target_os = "macos") {
    "open"
} else {
    "xdg-open"
};

/// Runs `cmd` with `text` on its stdin; true when it took it.
fn pipe(cmd: &str, args: &[&str], text: &[u8]) -> bool {
    use std::io::Write;
    use std::process::{Command, Stdio};
    // Both stay behind to serve the clipboard: keep them off the terminal.
    let spawned = Command::new(cmd).args(args).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
    let Ok(mut child) = spawned else { return false };
    let wrote = child.stdin.take().is_some_and(|mut s| s.write_all(text).is_ok());
    child.wait().is_ok_and(|s| s.success()) && wrote
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for (i, shift) in [18, 12, 6, 0].iter().enumerate() {
            out.push(if i <= c.len() { T[(n >> shift & 63) as usize] as char } else { '=' });
        }
    }
    out
}

/// Transparent rounded corners, like Slack's workspace icons.
fn round_corners(img: DynamicImage) -> DynamicImage {
    let mut rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    let r = w.min(h) as f32 * 0.22;
    for (x, y, p) in rgba.enumerate_pixels_mut() {
        let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
        let cx = if fx < r { r } else if fx > w as f32 - r { w as f32 - r } else { continue };
        let cy = if fy < r { r } else if fy > h as f32 - r { h as f32 - r } else { continue };
        let d = ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt();
        p[3] = (p[3] as f32 * (r - d + 0.5).clamp(0.0, 1.0)) as u8;
    }
    DynamicImage::ImageRgba8(rgba)
}

/// Brings the terminal running slick to the front, where we know how (Hyprland).
fn focus_window() {
    #[cfg(target_os = "linux")]
    crate::tray::focus();
}
