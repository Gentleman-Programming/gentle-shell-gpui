//! Configuration, Profiles and Usage while agents run on a Gentle Shell
//! server (`gentle-shell serve`): the app is a thin client there. Nothing
//! on these screens reads or writes this computer's Gentle Shell home, runs
//! its `gentle-shell` or starts its login helper; what the server's Gentle
//! Shell serves over Desktop RPC is read (and written) through a session on
//! the server — signing in, API keys and packages included
//! ([`super::server_sign_in`], [`super::server_packages`]) — upgrading it
//! goes to the server itself ([`super::server_upgrade`]), and anything else
//! (or a server too old for those) shows "Do this on <server>: run
//! <command>".
//!
//! # The control session
//!
//! The Desktop RPC methods these screens use (`get_config`/`set_config`,
//! `get_status`, `get_auth_status`, `get_update_status`,
//! `list_available_models`, …) are asked of an agent session or, on a
//! server, of the server itself:
//!
//! - The active agent tab's, when it runs on that same server: reused as is,
//!   and left running when the screen closes (it is the user's tab).
//! - Otherwise a dedicated one ([`ControlSession::open`]): a [`SessionView`]
//!   entity the screen owns and never shows. Reusing `SessionView` keeps one
//!   Desktop RPC client (its capability probe, request queue, timeouts and
//!   `config_changed` handling) instead of a second copy of that plumbing.
//!   - On a server that offers it (capability `desktop`), the view asks the
//!     server's session-less Desktop RPC (`POST /api/desktop`, its events
//!     on `GET /api/desktop/events`; `PiProcess::serve_desktop`): no session
//!     is started for the screen. The server answers from a control session
//!     of its own, which it keeps hidden and stops once nobody uses it;
//!     closing the screen only stops following it. The models come from
//!     `list_available_models`, since there is no Pi RPC.
//!   - On an older server, a hidden session started in the server's home
//!     folder ([`server::home`]; the screen's folder when an older server
//!     doesn't say). It lives exactly as long as its screen: when the screen
//!     is released (closed, or replaced by another screen), the session's
//!     backend is shut down the way closing a tab does (`shutdown_backend`
//!     → `PiProcess::shutdown` → `DELETE /api/sessions/:id`), and quitting
//!     the app does the same, where a tab's server session is instead left
//!     running for the next launch to reattach to. A session still being
//!     started when the screen goes is dropped with its entity:
//!     `PiProcess`'s own `Drop` stops it on the server once the start
//!     returns, so none is left behind.

use std::path::{Path, PathBuf};

use gentle_core::backend::Backend;
use gpui_kit::assets::IconName;
use gpui_kit::component::{Icon, Sizable as _, h_flex};
use gpui_kit::*;

use crate::features::backend::server;
use crate::features::session::SessionView;
use crate::shared::ui;

/// The Gentle Shell server a screen works against, and its name as shown.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ServerPlace {
    pub(crate) backend: Backend,
    pub(crate) name: String,
}

impl ServerPlace {
    /// `choice` when it is a server.
    pub(crate) fn of(choice: &Backend) -> Option<Self> {
        let Backend::Server { url, name, .. } = choice else {
            return None;
        };
        let name = if name.trim().is_empty() {
            url.split_once("://")
                .map_or(url.as_str(), |(_, rest)| rest)
                .to_string()
        } else {
            name.trim().to_string()
        };
        Some(Self {
            backend: choice.clone(),
            name,
        })
    }

    /// "Do this on <server>: run <command>".
    pub(crate) fn run(&self, command: &str) -> String {
        run_on(&self.name, command)
    }

    /// "Do this on <server>: <action>", for what no command does.
    pub(crate) fn act(&self, action: &str) -> String {
        do_on(&self.name, action)
    }
}

/// What a screen does there instead of acting on this computer.
pub(crate) fn run_on(server: &str, command: &str) -> String {
    do_on(server, &format!("run {command}"))
}

/// What a screen asks the user to do on the server, in words.
pub(crate) fn do_on(server: &str, action: &str) -> String {
    format!("Do this on {server}: {action}")
}

/// Commands the screens point at on the server.
pub(crate) mod commands {
    pub(crate) const UPGRADE: &str = "gentle-shell upgrade";
    pub(crate) const UPGRADE_MAIN: &str = "gentle-shell upgrade --channel main";
    pub(crate) const UPDATE_PI: &str = "gentle-shell update";
    pub(crate) const UPDATE_PACKAGES: &str = "gentle-shell update --extensions";
    pub(crate) const INSTALL: &str = "gentle-shell install <source>";
    pub(crate) const LOGIN: &str = "gentle-shell, then /login";
    pub(crate) const SETTINGS: &str = "gentle-shell, then /settings";
    pub(crate) const CUSTOMIZE: &str = "gentle-shell, then /gentle:customize";
    pub(crate) const PROFILES: &str = "gentle-shell, then /gentle:profiles";
    pub(crate) const USAGE: &str = "gentle-shell, then /gentle:usage";
    pub(crate) const STATS: &str = "gentle-shell, then /gentle:stats";

    /// `gentle-shell remove <source>`.
    pub(crate) fn remove(source: &str) -> String {
        format!("gentle-shell remove {source}")
    }
}

/// One "Do this on" line, as the screens show it.
pub(crate) fn note(text: impl Into<SharedString>) -> Div {
    h_flex()
        .pt_2()
        .gap_2()
        .items_start()
        .child(
            Icon::new(IconName::SquareTerminal)
                .xsmall()
                .text_color(ui::accent())
                .mt_0p5(),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_xs()
                .text_color(ui::muted())
                .child(text.into()),
        )
}

/// Which session a screen asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Control {
    /// The screen is on this computer (or WSL, or a command) and so is the
    /// active tab: ask it; the files are the fallback.
    Local,
    /// The screen is on this computer, the active tab elsewhere (a server,
    /// another environment): the files only, never another machine's
    /// answers.
    Files,
    /// The active tab runs on the screen's server: ask it.
    Reuse,
    /// The screen's server, with no tab there to ask: a dedicated session.
    Hidden,
}

/// Decide from the machine the screen is for (`screen`) and where the
/// active tab's agent runs (`tab`).
pub(crate) fn control_for(screen: &Backend, tab: Option<&Backend>) -> Control {
    let Backend::Server { url, .. } = screen else {
        return match tab {
            Some(tab) if !gentle_core::machines::same(tab, screen) => Control::Files,
            _ => Control::Local,
        };
    };
    match tab {
        Some(Backend::Server { url: there, .. }) if there == url => Control::Reuse,
        _ => Control::Hidden,
    }
}

/// Which of the window's agent tabs a screen about `screen` asks (its
/// models, config, usage): `tabs` are where each runs, the active tab
/// first; the first on `screen` is taken, so This Mac's list comes from a
/// tab here even while the active one runs on a server. `None`: no tab
/// runs there ([`control_for`] then reads the files here, or a server's
/// own Desktop RPC or a hidden session there).
pub(crate) fn ask_tab(screen: &Backend, tabs: &[Backend]) -> Option<usize> {
    tabs.iter()
        .position(|tab| gentle_core::machines::same(tab, screen))
}

/// Whether a view that asked for it (`requested`, Configuration's control
/// session) takes the server's session-less Desktop RPC: only when the
/// server says it offers it (`info`, capability `desktop`). An older
/// server, or one that didn't answer, gets a hidden session as before.
pub(crate) fn uses_server_desktop(
    requested: bool,
    info: Option<&pi_rpc::serve::ServerInfo>,
) -> bool {
    requested && info.is_some_and(|info| info.has("desktop"))
}

/// The session a screen reads Gentle Shell's configuration through, and the
/// server it is on (see the module docs).
pub(crate) struct ControlSession {
    session: Option<Entity<SessionView>>,
    server: Option<ServerPlace>,
    hidden: bool,
    /// Asks the server for its home folder, then starts the hidden session.
    _start: Task<()>,
    _quit: Option<Subscription>,
}

impl ControlSession {
    /// The session for a screen about `screen` (a machine) opened on `cwd`,
    /// whose window's active agent tab is `active`. A dedicated session is
    /// stopped when the screen (`cx`'s entity) is released, or when the app
    /// quits.
    pub(crate) fn open<V: 'static>(
        active: Option<Entity<SessionView>>,
        screen: &Backend,
        cwd: &Path,
        window: &mut Window,
        cx: &mut Context<V>,
    ) -> Self {
        let tab = active.as_ref().map(|session| session.read(cx).runs_on());
        let server = ServerPlace::of(screen);
        let reused = |session| Self {
            session,
            server: server.clone(),
            hidden: false,
            _start: Task::ready(()),
            _quit: None,
        };
        match control_for(screen, tab.as_ref()) {
            Control::Local | Control::Reuse => reused(active),
            Control::Files => reused(None),
            Control::Hidden => Self::start_hidden(screen.clone(), server, cwd, window, cx),
        }
    }

    fn start_hidden<V: 'static>(
        screen: Backend,
        server: Option<ServerPlace>,
        cwd: &Path,
        window: &mut Window,
        cx: &mut Context<V>,
    ) -> Self {
        let known = match &screen {
            Backend::Server { url, .. } => server::known_home(url),
            _ => None,
        };
        let start_now = known.is_some();
        let folder = known.map_or_else(|| cwd.to_path_buf(), PathBuf::from);
        // The server's own Desktop RPC when it has it (`use_server_desktop`);
        // a hidden session on an older server.
        let session = cx.new(|cx| {
            let mut view = SessionView::new(folder, None, window, cx, false);
            view.use_server_desktop();
            if start_now {
                view.ensure_started(window, cx);
            }
            view
        });
        let start = if start_now {
            Task::ready(())
        } else {
            let weak = session.downgrade();
            cx.spawn_in(window, async move |_, cx| {
                let home = cx
                    .background_executor()
                    .spawn(async move { server::home(&screen) })
                    .await;
                let _ = weak.update_in(cx, |session, window, cx| {
                    if let Some(home) = home {
                        session.change_cwd(PathBuf::from(home), window, cx);
                    }
                    session.ensure_started(window, cx);
                });
            })
        };
        let released = session.clone();
        cx.on_release(move |_, cx| {
            released.read(cx).shutdown_backend();
        })
        .detach();
        let quitting = session.clone();
        let quit = cx.on_app_quit(move |_, cx| {
            let signalled = quitting.read(cx).shutdown_backend();
            // The stop is an HTTP request on its own thread: a moment for it
            // to leave before the app exits.
            let settle = signalled.then(|| {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(100))
            });
            async move {
                if let Some(settle) = settle {
                    settle.await;
                }
            }
        });
        Self {
            session: Some(session),
            server,
            hidden: true,
            _start: start,
            _quit: Some(quit),
        }
    }

    /// The session to ask, if any.
    pub(crate) fn session(&self) -> Option<&Entity<SessionView>> {
        self.session.as_ref()
    }

    /// The server the screen's agents run on; `None` off a server.
    pub(crate) fn server(&self) -> Option<&ServerPlace> {
        self.server.as_ref()
    }

    /// The session is the screen's own, not a tab the user sees.
    pub(crate) fn hidden(&self) -> bool {
        self.hidden
    }
}

#[cfg(test)]
mod tests {
    use gentle_core::backend::Backend;

    use super::{
        Control, ServerPlace, ask_tab, commands, control_for, do_on, run_on, uses_server_desktop,
    };

    fn info(capabilities: &[&str]) -> pi_rpc::serve::ServerInfo {
        pi_rpc::serve::ServerInfo {
            server_id: "srv_1".into(),
            version: None,
            home: None,
            capabilities: capabilities.iter().map(|c| c.to_string()).collect(),
            platform: None,
            arch: None,
            path_separator: None,
        }
    }

    #[test]
    fn the_control_session_uses_the_servers_desktop_rpc_only_when_advertised() {
        let new = info(&["sessions", "desktop-rpc", "desktop"]);
        let old = info(&["sessions", "desktop-rpc"]);
        assert!(uses_server_desktop(true, Some(&new)));
        assert!(
            !uses_server_desktop(true, Some(&old)),
            "an older server keeps the hidden session"
        );
        assert!(
            !uses_server_desktop(true, None),
            "a server that didn't say keeps it too"
        );
        assert!(
            !uses_server_desktop(false, Some(&new)),
            "a tab never does: it is a session"
        );
    }

    fn server(url: &str, name: &str) -> Backend {
        Backend::Server {
            url: url.into(),
            device: "dev_1".into(),
            name: name.into(),
        }
    }

    /// Where a screen's lists (Profiles' models, Usage) come from: the
    /// tab picked by `ask_tab` (else the active one) through `control_for`.
    fn source(screen: &Backend, tabs: &[Backend]) -> (Option<usize>, Control) {
        let tab = ask_tab(screen, tabs).or((!tabs.is_empty()).then_some(0));
        (tab, control_for(screen, tab.map(|i| &tabs[i])))
    }

    #[test]
    fn a_screens_models_come_from_the_machine_shown() {
        let pc = server("http://pc:8787", "pc");
        let other = server("http://other:8787", "other");
        // A server with a tab there, active or not: that tab.
        assert_eq!(
            source(&pc, std::slice::from_ref(&pc)),
            (Some(0), Control::Reuse)
        );
        assert_eq!(
            source(&pc, &[Backend::Local, pc.clone()]),
            (Some(1), Control::Reuse)
        );
        // A server without one: its own Desktop RPC (or a hidden session),
        // never the active tab's models from another machine.
        assert_eq!(source(&pc, &[Backend::Local]).1, Control::Hidden);
        assert_eq!(source(&pc, std::slice::from_ref(&other)).1, Control::Hidden);
        assert_eq!(source(&pc, &[]).1, Control::Hidden);
        // This Mac: a tab here, even while the active one is on a server.
        assert_eq!(
            source(&Backend::Local, &[pc.clone(), Backend::Local]),
            (Some(1), Control::Local)
        );
        // This Mac with every tab on a server: the files here only.
        assert_eq!(source(&Backend::Local, &[pc]).1, Control::Files);
    }

    #[test]
    fn a_server_screen_reuses_only_a_tab_on_that_server() {
        let box_a = server("http://a:7420", "studio");
        let box_b = server("http://b:7420", "lab");
        assert_eq!(control_for(&Backend::Local, None), Control::Local);
        assert_eq!(
            control_for(&Backend::Local, Some(&Backend::Local)),
            Control::Local
        );
        assert_eq!(
            control_for(&Backend::Local, Some(&box_a)),
            Control::Files,
            "a screen about this computer never reads a server tab's answers"
        );
        assert_eq!(control_for(&box_a, Some(&box_a)), Control::Reuse);
        assert_eq!(control_for(&box_a, Some(&box_b)), Control::Hidden);
        assert_eq!(control_for(&box_a, Some(&Backend::Local)), Control::Hidden);
        assert_eq!(control_for(&box_a, None), Control::Hidden);
    }

    #[test]
    fn do_this_on_names_the_server_and_the_command() {
        assert_eq!(
            run_on("studio", commands::UPGRADE),
            "Do this on studio: run gentle-shell upgrade"
        );
        assert_eq!(
            do_on("studio", "edit mcp.json"),
            "Do this on studio: edit mcp.json"
        );
        let place = ServerPlace::of(&server("http://a:7420", "  studio ")).unwrap();
        assert_eq!(
            place.run(&commands::remove("npm:pi-btw")),
            "Do this on studio: run gentle-shell remove npm:pi-btw"
        );
        // A server paired without a name goes by its address.
        let unnamed = ServerPlace::of(&server("http://a:7420", "")).unwrap();
        assert_eq!(unnamed.name, "a:7420");
        assert!(ServerPlace::of(&Backend::Local).is_none());
    }
}
