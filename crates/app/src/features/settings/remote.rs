//! Configuration, Profiles and Usage while agents run on a Gentle Shell
//! server (`gentle-shell serve`): the app is a thin client there. Nothing
//! on these screens reads or writes this computer's Gentle Shell home, runs
//! its `gentle-shell` or starts its login helper; what the server's Gentle
//! Shell serves over Desktop RPC is read (and written) through a session on
//! the server, and anything else shows "Do this on <server>: run <command>".
//!
//! # The control session
//!
//! The Desktop RPC methods these screens use (`get_config`/`set_config`,
//! `get_status`, `get_auth_status`, `get_update_status`, …) are asked of an
//! agent session, so a screen needs one on the server:
//!
//! - The active agent tab's, when it runs on that same server: reused as is,
//!   and left running when the screen closes (it is the user's tab).
//! - Otherwise a dedicated one ([`ControlSession::open`]): a [`SessionView`]
//!   entity the screen owns and never shows, started in the server's home
//!   folder ([`server::home`]; the screen's folder when an older server
//!   doesn't say). Reusing `SessionView` keeps one Desktop RPC client (its
//!   capability probe, request queue, timeouts and `config_changed`
//!   handling) instead of a second copy of that plumbing.
//!
//! A dedicated session lives exactly as long as its screen. When the screen
//! is released (closed, or replaced by another screen), the session's
//! backend is shut down the way closing a tab does (`shutdown_backend` →
//! `PiProcess::shutdown` → `DELETE /api/sessions/:id`), and quitting the app
//! does the same, where a tab's server session is instead left running for
//! the next launch to reattach to. A session still being started when the
//! screen goes is dropped with its entity: `PiProcess`'s own `Drop` stops it
//! on the server once the start returns, so none is left behind.

use std::path::{Path, PathBuf};

use gentle_core::backend::Backend;
use gpui_kit::assets::IconName;
use gpui_kit::component::{Icon, Sizable as _, h_flex};
use gpui_kit::*;

use crate::features::backend::{self, server};
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
    /// Agents run on this computer (or WSL, or a command): the active tab,
    /// as before; the files are the fallback.
    Local,
    /// The active tab runs on the screen's server: ask it.
    Reuse,
    /// The screen's server, with no tab there to ask: a dedicated session.
    Hidden,
}

/// Decide from where the screen's agents run (`screen`) and where the
/// active tab's runs (`tab`).
pub(crate) fn control_for(screen: &Backend, tab: Option<&Backend>) -> Control {
    let Backend::Server { url, .. } = screen else {
        return Control::Local;
    };
    match tab {
        Some(Backend::Server { url: there, .. }) if there == url => Control::Reuse,
        _ => Control::Hidden,
    }
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
    /// The session for a screen opened on `cwd`, whose window's active agent
    /// tab is `active`. A dedicated session is stopped when the screen
    /// (`cx`'s entity) is released, or when the app quits.
    pub(crate) fn open<V: 'static>(
        active: Option<Entity<SessionView>>,
        cwd: &Path,
        window: &mut Window,
        cx: &mut Context<V>,
    ) -> Self {
        let screen = backend::for_tab(cwd);
        let tab = active.as_ref().map(|session| session.read(cx).runs_on());
        let server = ServerPlace::of(&screen);
        match control_for(&screen, tab.as_ref()) {
            Control::Local | Control::Reuse => Self {
                session: active,
                server,
                hidden: false,
                _start: Task::ready(()),
                _quit: None,
            },
            Control::Hidden => Self::start_hidden(screen, server, cwd, window, cx),
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
        let session = cx.new(|cx| SessionView::new(folder, None, window, cx, start_now));
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

    use super::{Control, ServerPlace, commands, control_for, do_on, run_on};

    fn server(url: &str, name: &str) -> Backend {
        Backend::Server {
            url: url.into(),
            device: "dev_1".into(),
            name: name.into(),
        }
    }

    #[test]
    fn a_server_screen_reuses_only_a_tab_on_that_server() {
        let box_a = server("http://a:7420", "studio");
        let box_b = server("http://b:7420", "lab");
        assert_eq!(control_for(&Backend::Local, None), Control::Local);
        assert_eq!(
            control_for(&Backend::Local, Some(&box_a)),
            Control::Local,
            "a local screen keeps the active tab, as before"
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
