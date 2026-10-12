# Landing — surface brief (product name on the landing: slick)

Scope: marketing landing for slick, `landing/public/index.html`, served by the Worker `slick-landing` at https://slick.leonavas.com (Cloudflare account Leonavas). Mode: **Persuade**. Build path: code-led.
Visitor: a developer who keeps a terminal open all day and is tired of the Electron Slack. Action: install it (copy the install command). Belief to earn: "this ultra fast Slack TUI opens instantly, stays out of my way, and is honest about what it is".
Proof available: the real product behavior (see PRODUCT.md). No benchmarks, stars, users or testimonials exist — never invent them. Customization (themes, keybindings) and extensibility (plugins) are **roadmap** and must be labeled as such.

Vocabulary: "TUI" and "ultra fast Slack app" (user's choice).

Constraints:
- Static HTML/CSS/JS, single file, no build step. Mono face: Maple Mono (local first, jsDelivr @fontsource fallback).
- English source copy in the HTML; pt-BR applied by JS when `navigator.languages` starts with `pt` (`?lang=en|pt`, EN/PT toggle, `<html lang>` updated).
- Install: `cargo install --git https://github.com/leonavas/slick` → `slick login` (or token in `~/.config/slick/config`) → `slick`.
- No Slack logo. The status-line "demo workspace · not affiliated" text was removed at the user's request.

Seed key: 5e3f89cd (direction, persuade). Three proposals were built (keycaps, live session, bitmap collage); the user chose the live session and the others were deleted (still in commit 16fcb92).

## Direction contract
THESIS: you are already using it — the whole page is a slick session on a character grid; sections are channels and the page scroll moves through them. Refuses the "screenshot in a fake window" hero.
OWN-WORLD: real terminal palettes as the color system (default "Terminal" = Gruvbox palette; Solarized Light, Nord, Catppuccin Latte via `t`), box-drawing borders, ANSI-style badges, Maple Mono at every size. Wordmark = the app's own login banner: ANSI Shadow "FAST SLACK" as SVG, FAST in the theme's bright text, SLACK letter by letter in the app's fixed RGB (#36c5f0 blue, #2eb67d green, #e01e5a red, #ecb22e yellow, blue), shadow strokes in the letter color at 55%. Slack's pixel mark is NOT used on the landing.
STORY: lands on #welcome hero → copies install → scrolls (or j / Alt+↓ / Ctrl+K) through #install, #why-fast, #keys, #features, #roadmap; reaching a channel marks it read in the sidebar.
FIRST VIEWPORT: poster, left-aligned (centered was rejected). Wordmark runs the full width of the right pane (stacks FAST over SLACK under 620px of container); below it, single column inside a 1080px measure centered in the pane: H1 "Ultrafast, slim and beautiful Slack TUI." (PT "Uma TUI de Slack ultrarrápida, enxuta e bonita."), one-line lede, install command with solid yellow Copy key. Nothing else (boot log, sign-in link, nav line removed). Every channel not yet visited starts bold/unread. Message box docked at the bottom, status line pinned.
FORM: live TUI session, grounded #1 (pick), seed 5e3f89cd.
FINISH: unreviewed and undocumented is unfinished; this build ends with the finish review, the verdict, DESIGN.md, and every shipping raster carrying its provenance

## Direct messages
- `● slick`: changelog (0.1.0, 2026-10-12).
- `● feature-requests`: proposal → Turnstile → email asked in the input box → POST `/api/feature-request` (Worker `src/worker.js`: verifies Turnstile, stores a pending row in D1 `slick-feature-requests`, sends a confirmation email from noreply@slick.leonavas.com via Cloudflare Email Sending) → link `/api/confirm?t=…` marks it confirmed and redirects to `/?confirmed=<id>#requests` → "Your proposal was sent." History in localStorage `slick-requests` (id, text, status), pending ones re-checked via `GET /api/feature-request/:id`.
- Wordmark: "SLICK" only, letter by letter in the app's four colors (blue, green, red, yellow, blue). No Slack mark.
