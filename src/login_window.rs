//! The sign-in window: Slack's own sign-in page. Runs in a process of its
//! own (`slick __login-window [workspace]`, started by `login::window`), so
//! tao's event loop never meets the TUI and the process can just exit.
//! Once the person is in, prints the session it created as one JSON line on
//! stdout, `{"token":"xoxc-…","cookie":"xoxd-…"}`, and exits. Closing the
//! window without signing in exits with status 1.
//!
//! With a workspace (`acme`) it opens on that workspace's email-and-password
//! form instead of Slack's default sign-in.
//!
//! The browser profile lives in ~/.local/share/slick/webview
//! (%LOCALAPPDATA%\slick\webview on Windows), apart from any other browser,
//! so signing out elsewhere does not touch this session.
//!
//! Linux draws it with WebKitGTK, Windows with WebView2 (part of Windows 10/11).

use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::window::{Icon, WindowBuilder};
use wry::{WebContext, WebViewBuilder};

/// The hidden argument that runs this window instead of the app.
pub const ARG: &str = "__login-window";

/// The slick mark, 64×64 RGBA.
const ICON: &[u8] = include_bytes!("../assets/icon-64.rgba");

/// Runs on every page; on the Slack client it hands over the team's token.
/// After signing in Slack lands on /ssb/redirect, which wants to launch the
/// desktop app; this goes on to the web client instead.
const GRAB_TOKEN: &str = r#"
(function () {
  if (location.pathname.startsWith('/ssb/redirect')) {
    const go = () => {
      const a = document.querySelector('a[href*="app.slack.com/client"]');
      location.replace(a ? a.href : 'https://app.slack.com/client');
    };
    if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', go); else go();
    return;
  }
  if (location.hostname !== 'app.slack.com') return;
  function grab() {
    try {
      const cfg = JSON.parse(localStorage.getItem('localConfig_v2') || 'null');
      if (!cfg || !cfg.teams) return false;
      const m = location.pathname.match(/\/client\/(T[A-Z0-9]+)/);
      const teams = Object.values(cfg.teams);
      const team = (m && cfg.teams[m[1]]) || (teams.length === 1 ? teams[0] : null);
      if (!team || !team.token) return false;
      window.ipc.postMessage(team.token);
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

/// Opens the window; never returns.
pub fn run() -> ! {
    // WebKitGTK's DMA-BUF renderer dies on some Wayland setups (hybrid GPUs)
    // with "Error 71 (Protocol error)" and the window closes at once.
    #[cfg(target_os = "linux")]
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }
    let event_loop = EventLoopBuilder::<String>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let window = WindowBuilder::new()
        .with_title(crate::i18n::tr("Sign in to slick", "Entrar no slick"))
        .with_window_icon(Icon::from_rgba(ICON.to_vec(), 64, 64).ok())
        .with_inner_size(tao::dpi::LogicalSize::new(520.0, 760.0))
        .build(&event_loop)
        .expect("window");

    let ws = workspace();
    let mut context = WebContext::new(Some(data_dir()));
    let builder = WebViewBuilder::new_with_web_context(&mut context)
        .with_url(match &ws {
            Some(w) => format!("https://{w}.slack.com/sign_in_with_password"),
            None => "https://app.slack.com/client".into(),
        })
        .with_initialization_script(GRAB_TOKEN)
        .with_initialization_script(ws.as_deref().map(|w| PASSWORD_FIRST.replace("__WORKSPACE__", w)).unwrap_or_default())
        // slack:// would try to launch the desktop app.
        .with_navigation_handler(|url| url.starts_with("https://") || url.starts_with("http://") || url.starts_with("about:"))
        .with_ipc_handler(move |req| {
            let _ = proxy.send_event(req.into_body());
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
            Event::UserEvent(token) if token.starts_with("xoxc-") => {
                // The `d` cookie is HttpOnly: the page cannot read it, the webview can.
                let cookie = webview
                    .cookies_for_url("https://app.slack.com/")
                    .unwrap_or_default()
                    .into_iter()
                    .find(|c| c.name() == "d")
                    .map(|c| c.value().to_string());
                if let Some(cookie) = cookie {
                    println!("{}", serde_json::json!({ "token": token, "cookie": cookie }));
                    std::process::exit(0);
                }
            }
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => std::process::exit(1),
            _ => {}
        }
    });
}
