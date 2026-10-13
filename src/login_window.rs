//! The sign-in window: Slack's own sign-in page. Runs in a process of its
//! own (`… __login-window [workspace]`, started by `login::window_known`), so
//! tao's event loop never meets the TUI and the process can just exit. On
//! Linux that process is the `slick-login` helper (`login/`), so slick itself
//! doesn't link WebKitGTK; elsewhere it's slick itself.
//! Once the person is in, prints the session it created as one JSON line on
//! stdout, `{"token":"xoxc-…","cookie":"xoxd-…","team":"T0AAA","teams":[…]}`
//! (`teams`: every workspace the browser is signed in to, `{id,name,domain,token}`),
//! and exits. Closing the window without signing in exits with status 1.
//!
//! With a workspace (`acme`) it opens on that workspace's email-and-password
//! form instead of Slack's default sign-in.
//!
//! Adding a workspace: `SLICK_KNOWN_TEAMS` (`T0AAA,T0BBB`) lists the ones
//! slick already has. The window then opens on Slack's sign-in page and only
//! takes a workspace outside the list.
//!
//! The browser profile lives in ~/.local/share/slick/webview
//! (%LOCALAPPDATA%\slick\webview on Windows), apart from any other browser,
//! so signing out elsewhere does not touch this session. Its HTTP cache is
//! dropped once the window closes (`login::purge_cache`); cookies and
//! storage stay, so signing in again doesn't ask for the password.
//!
//! Linux draws it with WebKitGTK, Windows with WebView2 (part of Windows 10/11).

use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::window::{Icon, WindowBuilder};
use wry::{NewWindowResponse, WebContext, WebViewBuilder};

/// What the page and the webview's handlers hand the event loop.
enum Msg {
    /// From the page (`window.ipc`): the session found, as JSON.
    Found(String),
    /// A page to open in this same window instead.
    Go(String),
}

/// Where a link the webview won't follow should go instead: `slack://`
/// (the desktop app: `slack://open?team=T0AAA…`) becomes that workspace in
/// the web client; web pages stay as they are.
fn web_url(url: &str) -> Option<String> {
    if url.starts_with("https://") || url.starts_with("http://") {
        return Some(url.to_string());
    }
    let rest = url.strip_prefix("slack://")?;
    let team = rest
        .split(['?', '&'])
        .find_map(|kv| kv.strip_prefix("team="))
        .map(|t| t.chars().take_while(|c| c.is_ascii_alphanumeric()).collect::<String>())
        .filter(|t| t.starts_with('T'));
    Some(match team {
        Some(t) => format!("https://app.slack.com/client/{t}"),
        None => "https://app.slack.com/client".to_string(),
    })
}

/// The slick mark, 64×64 RGBA.
const ICON: &[u8] = include_bytes!("../assets/icon-64.rgba");

/// Runs on every page; on the Slack client it hands over the team's token.
/// After signing in Slack lands on /ssb/redirect, which wants to launch the
/// desktop app; this goes on to the web client instead.
const GRAB_TOKEN: &str = r#"
(function () {
  // Links meant for a new tab ("Open" on the workspace list) open here: the window has no tabs.
  document.addEventListener('click', (e) => {
    const a = e.target && e.target.closest && e.target.closest('a[target]');
    if (a && a.target !== '_self') a.target = '_self';
  }, true);
  if (location.pathname.startsWith('/ssb/redirect')) {
    const go = () => {
      const a = document.querySelector('a[href*="app.slack.com/client"]');
      location.replace(a ? a.href : 'https://app.slack.com/client');
    };
    if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', go); else go();
    return;
  }
  if (location.hostname !== 'app.slack.com') return;
  const known = __KNOWN__;
  function grab() {
    try {
      const cfg = JSON.parse(localStorage.getItem('localConfig_v2') || 'null');
      if (!cfg || !cfg.teams) return false;
      const m = location.pathname.match(/\/client\/(T[A-Z0-9]+)/);
      // Only workspaces slick doesn't have yet: the browser may be signed in to those too.
      const fresh = Object.values(cfg.teams).filter((t) => t && t.id && !known.includes(t.id));
      const named = m && cfg.teams[m[1]];
      const team = (named && !known.includes(named.id) ? named : null) || (fresh.length === 1 ? fresh[0] : null);
      if (!team || !team.token) return false;
      const teams = Object.values(cfg.teams)
        .filter((t) => t && t.id && t.token)
        .map((t) => ({ id: t.id, name: t.name || '', domain: t.domain || '', token: t.token }));
      window.ipc.postMessage(JSON.stringify({ token: team.token, team: team.id, teams: teams }));
      return true;
    } catch (e) {
      return false;
    }
  }
  if (!grab()) {
    const timer = setInterval(() => { if (grab()) clearInterval(timer); }, 1000);
  }
})();
"#;

/// With a workspace given: fills it in on Slack's "Sign in to your workspace"
/// page and, on the workspace's own page, follows the sign-in-with-password
/// link when there is one. Each step runs once per tab, so going back works.
const PASSWORD_FIRST: &str = r#"
(function () {
  const ws = '__WORKSPACE__';
  function once(key, fn) {
    try { if (sessionStorage.getItem(key)) return; } catch (e) {}
    if (fn()) { try { sessionStorage.setItem(key, '1'); } catch (e) {} return true; }
    return false;
  }
  function fillDomain() {
    const input = document.querySelector('input[data-qa="signin_domain_input"], input[name="domain"], input[type="text"]');
    if (!input) return false;
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(input, ws);
    input.dispatchEvent(new Event('input', { bubbles: true }));
    setTimeout(() => {
      const button = (input.form && input.form.querySelector('button[type="submit"]')) || document.querySelector('button[type="submit"]');
      if (button) button.click(); else if (input.form) input.form.requestSubmit();
    }, 300);
    return true;
  }
  function passwordLink() {
    const a = document.querySelector('a[href*="sign_in_with_password"]');
    if (!a) return false;
    location.href = a.href;
    return true;
  }
  function step() {
    if (location.hostname === 'slack.com' && location.pathname.startsWith('/workspace-signin')) {
      return once('slick_domain', fillDomain);
    }
    if (location.hostname === ws + '.slack.com' && !location.pathname.startsWith('/sign_in_with_password')) {
      return once('slick_password', passwordLink);
    }
    return true;
  }
  let tries = 0;
  const timer = setInterval(() => { if (step() || ++tries > 20) clearInterval(timer); }, 500);
})();
"#;

fn data_dir() -> std::path::PathBuf {
    crate::config::data_dir().join("webview")
}

/// The workspace after the hidden argument, if it looks like one.
fn workspace() -> Option<String> {
    std::env::args().nth(2).filter(|w| !w.is_empty() && w.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
}

/// The workspaces slick already has (`SLICK_KNOWN_TEAMS`, `T0AAA,T0BBB`):
/// adding one, these are not taken.
fn known() -> Vec<String> {
    std::env::var("SLICK_KNOWN_TEAMS")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty() && t.chars().all(|c| c.is_ascii_alphanumeric()))
        .map(String::from)
        .collect()
}

/// Opens the window; never returns.
pub fn run() -> ! {
    // WebKitGTK's DMA-BUF renderer dies on some Wayland setups (hybrid GPUs)
    // with "Error 71 (Protocol error)" and the window closes at once.
    #[cfg(target_os = "linux")]
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }
    let event_loop = EventLoopBuilder::<Msg>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let (nav_proxy, window_proxy) = (proxy.clone(), proxy.clone());
    let window = WindowBuilder::new()
        .with_title(crate::i18n::tr("Sign in to slick", "Entrar no slick"))
        .with_window_icon(Icon::from_rgba(ICON.to_vec(), 64, 64).ok())
        .with_inner_size(tao::dpi::LogicalSize::new(520.0, 760.0))
        .build(&event_loop)
        .expect("window");

    let ws = workspace();
    let known = known();
    let mut context = WebContext::new(Some(data_dir()));
    let builder = WebViewBuilder::new_with_web_context(&mut context)
        .with_url(match &ws {
            Some(w) => format!("https://{w}.slack.com/sign_in_with_password"),
            // Adding one: the client would just open a workspace slick has.
            None if !known.is_empty() => "https://slack.com/signin".into(),
            None => "https://app.slack.com/client".into(),
        })
        .with_initialization_script(GRAB_TOKEN.replace("__KNOWN__", &serde_json::json!(known).to_string()))
        .with_initialization_script(ws.as_deref().map(|w| PASSWORD_FIRST.replace("__WORKSPACE__", w)).unwrap_or_default())
        // slack:// would try to launch the desktop app: that workspace opens
        // in the web client here instead, where its session is taken.
        .with_navigation_handler(move |url| {
            if url.starts_with("https://") || url.starts_with("http://") || url.starts_with("about:") {
                return true;
            }
            if let Some(go) = web_url(&url) {
                let _ = nav_proxy.send_event(Msg::Go(go));
            }
            false
        })
        // A new window (window.open, target=_blank): the same page in this one.
        .with_new_window_req_handler(move |url, _| {
            if let Some(go) = web_url(&url) {
                let _ = window_proxy.send_event(Msg::Go(go));
            }
            NewWindowResponse::Deny
        })
        .with_ipc_handler(move |req| {
            let _ = proxy.send_event(Msg::Found(req.into_body()));
        });
    #[cfg(target_os = "linux")]
    let webview = {
        use tao::platform::unix::WindowExtUnix;
        use wry::WebViewBuilderExtUnix;
        builder.build_gtk(window.default_vbox().expect("vbox")).expect("webview")
    };
    #[cfg(not(target_os = "linux"))]
    let webview = builder.build(&window).expect("webview");

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::UserEvent(Msg::Go(url)) => {
                let _ = webview.load_url(&url);
            }
            Event::UserEvent(Msg::Found(found)) => {
                let Ok(mut found) = serde_json::from_str::<serde_json::Value>(&found) else { return };
                if !found["token"].as_str().is_some_and(|t| t.starts_with("xoxc-")) {
                    return;
                }
                // The `d` cookie is HttpOnly: the page cannot read it, the webview can.
                let cookie = webview
                    .cookies_for_url("https://app.slack.com/")
                    .unwrap_or_default()
                    .into_iter()
                    .find(|c| c.name() == "d")
                    .map(|c| c.value().to_string());
                if let Some(cookie) = cookie {
                    found["cookie"] = serde_json::json!(cookie);
                    println!("{found}");
                    std::process::exit(0);
                }
            }
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => std::process::exit(1),
            _ => {}
        }
    });
}
