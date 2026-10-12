# Product

<!-- impeccable:product-schema 1 -->

## Platform

web (landing) — the product itself is a terminal (TUI) app

## Stack

Landing: static HTML/CSS/JS, no build step. Product: Rust (ratatui + crossterm, ureq).

## Users

Developers and terminal-first people who live in Slack all day and are tired of a heavy Electron client: they keep a terminal open anyway, navigate by keyboard, and want Slack to open instantly and stay out of the way.

## Product Purpose

slick is a Slack client that runs in the terminal: a single Rust binary, no webview. Success is someone installing it, opening it, and having their workspace on screen in an instant.

## Positioning

A native terminal binary instead of a browser in a box. It opens from a local cache with everything already on screen and refreshes in the background; network requests run on threads so the UI never blocks.

## Operating Context

Runs inside the user's terminal (kitty, ghostty, foot, alacritty, etc.). Auth via `~/.config/slick/config` with an internal Slack app token (`xoxp-`) or a browser session (`xoxc-` + `xoxd-` cookie), or `SLACK_TOKEN` / `SLACK_COOKIE`. Build: `cargo build --release`.

## Capabilities and Constraints

Confirmed (shipping today):
- Channels, private channels, DMs, group DMs; send messages; multiline input (Shift+Enter).
- Slack sidebar sections/folders (collapsible, reorderable with Shift+J/K, synced to Slack), starred, unread state, mention badges.
- Presence dots for DMs, avatars and inline images (kitty/sixel graphics via ratatui-image), click to enlarge.
- Fuzzy-ish conversation filter (`/` or Ctrl+K), vim-style j/k navigation, mouse support.
- Local cache (`~/.cache/slick/cache.json`) so it opens with everything on screen; open conversation polls every 3 s; marks as read.
- Block Kit and attachments rendered as text; buttons shown as `[label]`, not clickable.
- Uses the terminal's own palette (ANSI colors), so it follows the terminal theme.
- Release binary is ~4.6 MB (LTO, stripped).

Roadmap (must be labeled "coming"/"roadmap", never presented as shipping):
- Customization: themes, keybindings, layout config.
- Extensibility: plugins / scripting hooks.

Undecided: license, public repo URL, prebuilt binaries/package managers, threads/reactions support.

## Brand Commitments

Name: slick (lowercase). Not affiliated with Slack Technologies; must not use Slack's logo or brand marks.

## Evidence on Hand

Source code and README in this repo. No benchmarks, users, testimonials, or stars exist — do not fabricate them. Startup-time or memory numbers must not be invented; only the binary size is measured.

## Product Principles

- Instant beats clever: anything that delays first paint is wrong.
- The keyboard is the primary interface; the mouse is a courtesy.
- Respect the terminal: its theme, its fonts, its conventions.
- Honest about scope: what ships is stated plainly, what's planned is labeled as planned.

## Accessibility & Inclusion

Landing must be bilingual: English default, Brazilian Portuguese when the browser locale is pt.
