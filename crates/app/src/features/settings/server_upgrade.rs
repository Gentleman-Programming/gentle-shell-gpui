//! Updates › "Update Gentle Shell on <server>": a paired device with the
//! `admin` scope, on a server that offers `upgrade`, runs `gentle-shell
//! upgrade` there (`POST /api/upgrade`) and follows its output
//! (`GET /api/upgrade/:job/stream`). On success the server restarts itself
//! on the new version: its sessions keep running and this app's streams
//! reconnect; the screen waits for `GET /api/server` to answer again and
//! shows the version it reports. Without the scope or the route the screen
//! keeps saying "Do this on <server>".

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gentle_core::backend::Backend;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use pi_rpc::serve::UpgradeEvent;

use super::SettingsView;
use crate::features::backend::server;
use crate::shared::ui;

/// Output lines kept.
const MAX_LINES: usize = 200;
/// How long the server may take to answer again after the upgrade.
const COME_BACK: Duration = Duration::from_secs(180);
/// How often it is asked meanwhile.
const POLL: Duration = Duration::from_secs(2);

/// What the upgrade's follower reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum News {
    Started,
    Line(String),
    Finished {
        ok: bool,
        error: Option<String>,
        version: Option<String>,
    },
    /// The stream ended (a restarting server ends it).
    Ended,
    /// The server answers again, at this version.
    Back(Option<String>),
    Failed(String),
}

/// Where the upgrade is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Phase {
    Starting,
    Running,
    /// It finished (or its stream ended) and the server restarts: waiting
    /// for it to answer.
    Restarting {
        finished: bool,
    },
    Done(Option<String>),
    Failed(String),
}

/// The upgrade as the screen shows it.
#[derive(Debug)]
pub(super) struct ServerUpgrade {
    pub(super) phase: Phase,
    pub(super) lines: Vec<String>,
    /// The version the server ran before.
    before: Option<String>,
}

impl ServerUpgrade {
    pub(super) fn new(before: Option<String>) -> Self {
        Self {
            phase: Phase::Starting,
            lines: Vec::new(),
            before,
        }
    }

    pub(super) fn running(&self) -> bool {
        !matches!(self.phase, Phase::Done(_) | Phase::Failed(_))
    }

    pub(super) fn apply(&mut self, news: News) {
        if !self.running() {
            return;
        }
        match news {
            News::Started => self.phase = Phase::Running,
            News::Line(line) => {
                if self.lines.len() >= MAX_LINES {
                    self.lines.remove(0);
                }
                self.lines.push(line);
            }
            News::Finished { ok: true, .. } => self.phase = Phase::Restarting { finished: true },
            News::Finished {
                ok: false, error, ..
            } => self.phase = Phase::Failed(error.unwrap_or_else(|| "The upgrade failed.".into())),
            // Without `finished`, whether it worked shows in the version the
            // server answers with.
            News::Ended if self.phase == Phase::Running => {
                self.phase = Phase::Restarting { finished: false }
            }
            News::Ended => {}
            News::Back(version) => {
                let finished = matches!(self.phase, Phase::Restarting { finished: true });
                self.phase = if finished || (version.is_some() && version != self.before) {
                    Phase::Done(version)
                } else {
                    Phase::Failed(
                        "The upgrade's output stopped before it said how it ended; the server \
                         still runs the same version."
                            .into(),
                    )
                };
            }
            News::Failed(error) => self.phase = Phase::Failed(error),
        }
    }
}

/// Whether this device may upgrade the server, as asked once per screen.
#[derive(Default)]
pub(super) struct ServerUpgradeUi {
    /// The server offers `upgrade` and this device has `admin`.
    pub(super) allowed: Option<bool>,
    /// The version the server reports.
    version: Option<String>,
    pub(super) upgrade: Option<ServerUpgrade>,
    _ask: Option<Task<()>>,
    _follow: Option<Task<()>>,
    stop: Option<Arc<AtomicBool>>,
}

impl Drop for ServerUpgradeUi {
    fn drop(&mut self) {
        if let Some(stop) = &self.stop {
            stop.store(true, Ordering::Release);
        }
    }
}

/// Follow an upgrade on `choice` (a server, running `before`) on its own
/// thread, reporting to `news`. Blocking.
fn follow(
    choice: &Backend,
    channel: Option<&str>,
    before: Option<&str>,
    news: &async_channel::Sender<News>,
    stop: &AtomicBool,
) {
    let say = |item: News| news.send_blocking(item).is_ok();
    let client = match server::client(choice) {
        Ok(client) => client,
        Err(error) => {
            say(News::Failed(error.to_string()));
            return;
        }
    };
    let job = match client.upgrade(channel) {
        Ok(job) => job,
        Err(error) => {
            say(News::Failed(
                if error.error.as_deref() == Some("upgrade_running") {
                    "Gentle Shell is already being upgraded on the server.".into()
                } else {
                    error.message
                },
            ));
            return;
        }
    };
    if !say(News::Started) {
        return;
    }
    match client.upgrade_stream(&job) {
        Ok(mut stream) => {
            while let Some(event) = stream.next(stop) {
                let item = match event {
                    UpgradeEvent::Line(line) => News::Line(line),
                    UpgradeEvent::Finished { ok, error, version } => {
                        // A failed upgrade restarts nothing.
                        let done = !ok;
                        if !say(News::Finished { ok, error, version }) || done {
                            return;
                        }
                        continue;
                    }
                };
                if !say(item) {
                    return;
                }
            }
        }
        Err(error) => {
            say(News::Line(format!(
                "The upgrade's output: {}",
                error.message
            )));
        }
    }
    if !say(News::Ended) {
        return;
    }
    // The server restarts: wait for it to answer again. Until it goes down
    // it still answers as before, so it counts as back once it was away,
    // answers with another version, or kept answering for a while (a
    // restart faster than a poll).
    let start = Instant::now();
    let mut away = false;
    let mut answered = 0;
    while start.elapsed() < COME_BACK && !stop.load(Ordering::Acquire) {
        std::thread::sleep(POLL);
        server::forget_info(choice);
        match server::info(choice) {
            Ok(info) => {
                answered += 1;
                let moved = info.version.is_some() && info.version.as_deref() != before;
                if away || moved || answered >= 5 {
                    say(News::Back(info.version));
                    return;
                }
            }
            Err(_) => away = true,
        }
    }
    if !stop.load(Ordering::Acquire) {
        say(News::Failed(
            "The server didn't answer after the upgrade. It may still be restarting.".into(),
        ));
    }
}

impl SettingsView {
    /// Ask whether this device may upgrade the server (once per screen).
    pub(super) fn ask_server_upgrade(&mut self, cx: &mut Context<Self>) {
        let Some(place) = self.server().cloned() else {
            return;
        };
        if self.server_upgrade.allowed.is_some() || self.server_upgrade._ask.is_some() {
            return;
        }
        let choice = place.backend.clone();
        self.server_upgrade._ask = Some(cx.spawn(async move |this, cx| {
            let answer = cx
                .background_executor()
                .spawn(async move {
                    let info = server::known_info(&choice)?;
                    let admin = server::client(&choice)
                        .ok()?
                        .scopes()
                        .ok()?
                        .iter()
                        .any(|scope| scope == "admin");
                    Some((info.has("upgrade") && admin, info.version))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                let (allowed, version) = answer.unwrap_or((false, None));
                this.server_upgrade.allowed = Some(allowed);
                this.server_upgrade.version = version;
                cx.notify();
            });
        }));
    }

    /// The server may be upgraded from here.
    pub(super) fn server_upgrade_allowed(&self) -> bool {
        self.server().is_some() && self.server_upgrade.allowed == Some(true)
    }

    /// Upgrade the server's Gentle Shell (on `channel`, else its own).
    pub(super) fn run_server_upgrade(
        &mut self,
        channel: Option<&'static str>,
        cx: &mut Context<Self>,
    ) {
        let Some(place) = self.server().cloned() else {
            return;
        };
        if !self.server_upgrade_allowed()
            || self
                .server_upgrade
                .upgrade
                .as_ref()
                .is_some_and(ServerUpgrade::running)
        {
            return;
        }
        if let Some(stop) = self.server_upgrade.stop.take() {
            stop.store(true, Ordering::Release);
        }
        let stop = Arc::new(AtomicBool::new(false));
        self.server_upgrade.stop = Some(stop.clone());
        self.server_upgrade.upgrade = Some(ServerUpgrade::new(self.server_upgrade.version.clone()));
        let (tx, rx) = async_channel::unbounded::<News>();
        let choice = place.backend.clone();
        let before = self.server_upgrade.version.clone();
        let spawned = std::thread::Builder::new()
            .name("gentle-server-upgrade".into())
            .spawn(move || follow(&choice, channel, before.as_deref(), &tx, &stop));
        if let Err(error) = spawned
            && let Some(upgrade) = self.server_upgrade.upgrade.as_mut()
        {
            upgrade.apply(News::Failed(error.to_string()));
        }
        self.server_upgrade._follow = Some(cx.spawn(async move |this, cx| {
            while let Ok(news) = rx.recv().await {
                let back = matches!(news, News::Back(_));
                let alive = this.update(cx, |this, cx| {
                    if let News::Back(version) = &news {
                        this.server_upgrade.version = version.clone();
                    }
                    if let Some(upgrade) = this.server_upgrade.upgrade.as_mut() {
                        upgrade.apply(news);
                    }
                    if back {
                        // What the server offers now, and its update status.
                        this.reload_with(true, cx);
                    }
                    cx.notify();
                });
                if alive.is_err() {
                    return;
                }
            }
        }));
        cx.notify();
    }

    /// The upgrade button for an offer card on the server: `None` when this
    /// device can't upgrade it (the card then says what to run there).
    pub(super) fn server_upgrade_button(
        &self,
        id: &'static str,
        channel: Option<&'static str>,
        cx: &mut Context<Self>,
    ) -> Option<Button> {
        let place = self.server()?;
        if !self.server_upgrade_allowed() {
            return None;
        }
        let running = self
            .server_upgrade
            .upgrade
            .as_ref()
            .is_some_and(ServerUpgrade::running);
        Some(
            Button::new(id)
                .primary()
                .small()
                .icon(Icon::new(IconName::ArrowUp))
                .label(format!("Update Gentle Shell on {}", place.name))
                .loading(running)
                .disabled(running)
                .on_click(cx.listener(move |this, _, _, cx| this.run_server_upgrade(channel, cx))),
        )
    }

    /// The server upgrade running or just ended, and, when no offer card
    /// shows one, its button.
    pub(super) fn render_server_upgrade(
        &self,
        offered: bool,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let place = self.server()?;
        let upgrade = self.server_upgrade.upgrade.as_ref();
        let button = (!offered && upgrade.is_none_or(|upgrade| !upgrade.running()))
            .then(|| self.server_upgrade_button("server-upgrade", None, cx))
            .flatten();
        if upgrade.is_none() && button.is_none() {
            return None;
        }
        let name = &place.name;
        Some(
            v_flex()
                .pt_2()
                .gap_2()
                .children(button.map(|button| h_flex().child(button)))
                .children(upgrade.map(|upgrade| {
                    let busy = |text: String| {
                        h_flex()
                            .gap_2()
                            .text_xs()
                            .text_color(ui::muted())
                            .child(Spinner::new().xsmall().color(ui::accent()))
                            .child(text)
                    };
                    let headline = match &upgrade.phase {
                        Phase::Starting => busy(format!("Starting the upgrade on {name}…")),
                        Phase::Running => busy(format!("Upgrading Gentle Shell on {name}…")),
                        Phase::Restarting { .. } => {
                            busy(format!("{name} restarts on the new version; reconnecting…"))
                        }
                        Phase::Done(version) => h_flex()
                            .text_xs()
                            .text_color(ui::success())
                            .child(match version {
                                Some(version) => format!(
                                    "Gentle Shell {version} runs on {name}. Its sessions kept running."
                                ),
                                None => format!("{name} is back on the new version."),
                            }),
                        Phase::Failed(error) => h_flex()
                            .text_xs()
                            .text_color(ui::danger())
                            .child(error.clone()),
                    };
                    v_flex().gap_2().child(headline).when(!upgrade.lines.is_empty(), |d| {
                        d.child(
                            ui::well_box().px_3().py_2().gap_0p5().children(
                                upgrade
                                    .lines
                                    .iter()
                                    .rev()
                                    .take(10)
                                    .rev()
                                    .map(|line| div().text_color(ui::muted()).child(line.clone())),
                            ),
                        )
                    })
                })),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{News, Phase, ServerUpgrade};

    #[test]
    fn a_finished_upgrade_waits_for_the_server_and_shows_its_version() {
        let mut upgrade = ServerUpgrade::new(Some("5.0.0".into()));
        upgrade.apply(News::Started);
        upgrade.apply(News::Line("Installing 5.1.0…".into()));
        upgrade.apply(News::Finished {
            ok: true,
            error: None,
            version: Some("5.1.0".into()),
        });
        assert_eq!(upgrade.phase, Phase::Restarting { finished: true });
        upgrade.apply(News::Ended);
        assert!(upgrade.running());
        upgrade.apply(News::Back(Some("5.1.0".into())));
        assert_eq!(upgrade.phase, Phase::Done(Some("5.1.0".into())));
        assert_eq!(upgrade.lines, ["Installing 5.1.0…"]);
        // Nothing moves it once it ended.
        upgrade.apply(News::Failed("late".into()));
        assert_eq!(upgrade.phase, Phase::Done(Some("5.1.0".into())));
    }

    #[test]
    fn a_failed_or_unknown_ending_says_so() {
        let mut failed = ServerUpgrade::new(Some("5.0.0".into()));
        failed.apply(News::Started);
        failed.apply(News::Finished {
            ok: false,
            error: Some("npm ERR! EACCES".into()),
            version: None,
        });
        assert_eq!(failed.phase, Phase::Failed("npm ERR! EACCES".into()));

        // The stream ended without `finished`: the version decides.
        let mut moved = ServerUpgrade::new(Some("5.0.0".into()));
        moved.apply(News::Started);
        moved.apply(News::Ended);
        assert_eq!(moved.phase, Phase::Restarting { finished: false });
        moved.apply(News::Back(Some("5.1.0".into())));
        assert_eq!(moved.phase, Phase::Done(Some("5.1.0".into())));

        let mut same = ServerUpgrade::new(Some("5.0.0".into()));
        same.apply(News::Started);
        same.apply(News::Ended);
        same.apply(News::Back(Some("5.0.0".into())));
        assert!(matches!(same.phase, Phase::Failed(_)));

        let mut refused = ServerUpgrade::new(None);
        refused.apply(News::Failed("already being upgraded".into()));
        assert!(!refused.running());
    }
}
