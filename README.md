<p align="center">
  <img src="assets/slick.svg" alt="" width="96" height="96">
</p>
<h1 align="center">
  <img src="assets/readme/wordmark.svg" alt="slick" width="360">
</h1>

<p align="center">A fast, lightweight Slack client for the terminal, written in Rust.</p>

One small binary, no browser inside: it opens with your workspace already on
screen (from a local cache) and refreshes in the background.

<!-- screenshot -->

> slick is an unofficial client. It is not affiliated with, endorsed by or
> supported by Slack Technologies or Salesforce. "Slack" is a trademark of
> Slack Technologies, LLC. slick talks to Slack using your own session, the
> same way the web client does; use it at your own risk and within your
> workspace's policies and Slack's terms. The interface is currently in
> Brazilian Portuguese.

## Features

- Channels, DMs and group DMs, with Slack's sidebar sections, starred
  conversations and unread counts; drag to reorder, collapse sections.
- Fuzzy search over conversations by several words, with the matches highlighted.
- Threads in a side panel, with "also send to channel".
- Reactions and a reaction picker; `:` autocomplete in the message box.
- Custom workspace emoji, animated ones included (played at the GIF's own pace).
- Inline images (kitty graphics, sixel, iTerm2, or half-blocks as a fallback),
  an image viewer, and opening or downloading shared files.
- Block Kit and attachments rendered the way Slack draws them, with clickable buttons.
- Message actions: edit, delete, forward, save for later, remind me, mark as
  unread, copy link or text, follow/unfollow a thread.
- Pinned messages, in their own tab per conversation.
- New DM or channel from the "new" button.
- Your account: presence (active/away), pausing notifications, custom status,
  profile editing; other people's presence, status and profile cards.
- Mouse support: select messages by dragging and copy them, right-click menus,
  resizable sidebar.
- Follows your terminal's color theme.
- Auto-update from GitHub Releases (see below).

## Trust me bro benchmark

Slick and the Slack desktop app, same workspace, running side by side on my
machine. Not a lab benchmark: these are just the numbers the OS reports for
each app (resident memory, processes and threads from `/proc`, and the size of
each install and cache directory on disk).

|               | Slick | <a href="https://aur.archlinux.org/packages/slack-desktop" target="_blank">Slack App</a> |
| ------------- | ----: | -----: |
| memory        | 24 MB | 650 MB |
| processes     |     1 |     10 |
| threads       |     5 |    131 |
| install       |  8 MB | 295 MB |
| cache on disk | 13 MB | 800 MB |

## Requirements

- Linux (x86_64 or aarch64) or Windows 10/11 (x86_64).
- A terminal with an image protocol is recommended: kitty, ghostty, WezTerm,
  foot or any sixel terminal. Others fall back to half-block images.
- A [Nerd Font](https://www.nerdfonts.com/) for the icons.
- Linux: GTK 3 and WebKitGTK 4.1 at runtime, for the sign-in window
  (`webkit2gtk-4.1` on Arch, `libwebkit2gtk-4.1-0` on Debian/Ubuntu,
  `webkit2gtk4.1` on Fedora).
- Windows: [Windows Terminal](https://aka.ms/terminal) (or another terminal
  with true color and, ideally, an image protocol, like WezTerm); the old
  console host is not supported. The sign-in window uses WebView2, which comes
  with Windows 10/11. There is no tray icon on Windows yet.

## Install

```sh
curl -fsSL https://slick.leonavas.com/install.sh | sh
```

The script ([install.sh](install.sh)) downloads the latest
[release](https://github.com/leonavas/slick/releases) for your architecture
(built on Ubuntu 24.04, so glibc 2.39 or newer), checks its SHA-256 and puts
`slick` in `~/.local/bin`, without sudo. It warns when that folder is not on
your `PATH` or WebKitGTK is missing. `SLICK_INSTALL_DIR` changes the folder and
`SLICK_VERSION=v0.2.0` picks a version. Installed this way, slick updates itself
(see below).

### Windows

In PowerShell:

```powershell
irm https://slick.leonavas.com/install.ps1 | iex
```

The script ([install.ps1](install.ps1)) downloads the latest release, checks its
SHA-256, puts `slick.exe` in `%LOCALAPPDATA%\Programs\slick` and adds that folder
to your user `PATH`, without admin rights (`$env:SLICK_VERSION = 'v0.2.0'` picks
a version). Open a new terminal and run `slick`. Only x86_64 for now.

### From source

Install Rust and the GTK/WebKitGTK development packages first:

```sh
# Debian/Ubuntu
sudo apt install build-essential pkg-config libgtk-3-dev libwebkit2gtk-4.1-dev
# Arch
sudo pacman -S base-devel gtk3 webkit2gtk-4.1
# Fedora
sudo dnf install gcc pkgconf-pkg-config gtk3-devel webkit2gtk4.1-devel
```

Then either:

```sh
cargo install --locked --git https://github.com/leonavas/slick
```

or, from a clone:

```sh
cargo build --release
./target/release/slick
```

## Sign in

Just run `slick`: with no session yet it opens the sign-in screen by itself.
You can also open it with `Ctrl+L` inside the app, or directly with:

```sh
slick login [workspace]
```

It opens Slack's own sign-in page in a small window
(run by the same binary, in a process of its own); `workspace` (`acme` or `acme.slack.com`) goes straight to that
workspace's email-and-password form. Once you are in, the window closes and the
session (the browser token `xoxc-…` and its `d` cookie `xoxd-…`) is saved
locally in `~/.config/slick/config`, readable only by you, and is only ever sent
to Slack.

The window keeps its own browser profile in `~/.local/share/slick/webview`
(`%LOCALAPPDATA%\slick\webview` on Windows), so
signing out of Slack in your browser does not sign slick out.

Other ways: paste the token and cookie on the sign-in screen, set
`SLACK_TOKEN` / `SLACK_COOKIE` in the environment, or use a user token
(`xoxp-…`) from your own Slack app; `slack-app-manifest.yml` is a manifest
for creating that app with the scopes slick uses.

## Usage

| Key | Action |
|---|---|
| `↑` `↓` / `j` `k` | move in the sidebar |
| `Enter` | open a conversation / expand or collapse a section |
| `/` or `Ctrl+K` | search conversations |
| `Tab` | sidebar → message box → thread |
| `Shift+J` / `Shift+K` | move a section up or down |
| `m` or right click | menu for a conversation or message |
| `PgUp` `PgDn` / mouse wheel | scroll (older messages load at the top) |
| `Enter` / `Shift+Enter` | send / new line |
| `:` | emoji autocomplete |
| `Ctrl+U` / `Ctrl+W` | clear the message box / delete a word |
| `↑` in an empty message box | pick a message |
| `E` `R` `T` `F` `A` `U` `L` `Del` | on a picked message: edit, react, reply in thread, forward, save, mark unread, copy link, delete |
| `Ctrl+C` | copy the picked or selected messages; otherwise quit |
| `Ctrl+B` | in a thread, also send the reply to the channel |
| `Ctrl+N` | new message or channel |
| `Ctrl+P` | your account, status and profile |
| `Ctrl+R` | reload channels and users |
| `Ctrl+L` | sign in again |
| click on an image | enlarge it (`o` opens it in the system viewer, `d` downloads it) |
| `q` / `Ctrl+C` | quit |

## Config

`~/.config/slick/config` (`$XDG_CONFIG_HOME` is honored; `%APPDATA%\slick\config`
on Windows), `key = value` per line:

| Key | Meaning |
|---|---|
| `token`, `cookie` | the session, written by `slick login` |
| `workspace` | the last workspace signed in to |
| `sidebar_width`, `sidebar_avatars` | the sidebar's width and whether it shows photos, saved from the app |
| `auto_update` | `false` turns off the update check |
| `tray` | `false` hides the system tray icon (Linux; lit with unread DMs and mentions, a click focuses the window on Hyprland) |

The cache lives in `~/.cache/slick/` (`%LOCALAPPDATA%\slick` on Windows):
conversations, users, recent messages and images; images unseen for a week are
deleted.

## Auto-update

Every time it starts, and every 6 hours while open, slick asks GitHub for the
latest release in the background (errors are ignored). When there is a newer
one, a small green "↑ X.Y.Z" button shows at the right of your name in the
account button, at the bottom of the sidebar. Nothing is downloaded until you
click it:

- If slick was installed from a release into a folder you can write to (like
  `~/.local/bin` or `%LOCALAPPDATA%\Programs\slick`), the click downloads the
  archive for your system, checks its SHA-256 and replaces `slick` in place (on
  Windows the running `slick.exe` steps aside as `slick.old.exe`, deleted at the
  next start). The button
  reads "atualizando…" meanwhile, then "↻ reiniciar": click it (or press
  `Ctrl+O`) to restart into the new version. On failure the button comes back
  and the error shows in the status line.
- If the binary belongs to something else (`~/.cargo/bin`, `/usr`, `/nix`,
  Homebrew) or the folder is read-only, the click only tells in the status line
  how to update (for cargo, the `cargo install` command above).

Development builds (`target/`) never check, unless `SLICK_UPDATE_DEV=1` is
set: then the button shows (with a made-up version when there is no release)
and a click only says it is a development build.

`slick update` checks and installs right away, printing what it does.
Set `auto_update = false` in the config to turn the check off.

## License

Licensed under the [Apache License 2.0](LICENSE). See [NOTICE](NOTICE).
