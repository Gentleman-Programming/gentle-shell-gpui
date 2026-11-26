//! "Run setup on <server>": a paired device with the `admin` scope, on a
//! server that offers `setup`, runs Gentle Shell's own companion setup for
//! the server's home there (`POST /api/setup`) and follows its output
//! (`GET /api/setup/:job/stream`); then Setup checks the server again.
//! Nothing restarts, and nothing runs on this computer.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gentle_core::backend::Backend;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use pi_rpc::serve::UpgradeEvent;

use super::OnboardingView;
use crate::features::backend::server;
use crate::shared::ui;

/// Output lines kept.
const MAX_LINES: usize = 200;
/// Output lines shown.
const SHOWN_LINES: usize = 10;

/// What the setup's follower reports.
#[derive(Debug, Clone, PartialEq, Eq)]
enum News {
    Line(String),
    Finished { ok: bool, error: Option<String> },
    Failed(String),
}

/// Where the setup is.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Phase {
    Running,
    Done,
    Failed(String),
}

/// The setup as the screen shows it.
#[derive(Debug)]
struct SetupJob {
    phase: Phase,
    lines: Vec<String>,
}

impl SetupJob {
    fn new() -> Self {
        Self {
            phase: Phase::Running,
            lines: Vec::new(),
        }
    }

    fn running(&self) -> bool {
        self.phase == Phase::Running
    }

    /// Whether it just ended.
    fn apply(&mut self, news: News) -> bool {
        if !self.running() {
            return false;
        }
        match news {
            News::Line(line) => {
                if self.lines.len() >= MAX_LINES {
                    self.lines.remove(0);
                }
                self.lines.push(line);
                return false;
            }
            News::Finished { ok: true, .. } => self.phase = Phase::Done,
            News::Finished { ok: false, error } => {
                self.phase = Phase::Failed(error.unwrap_or_else(|| "Setup failed.".into()))
            }
            News::Failed(error) => self.phase = Phase::Failed(error),
        }
        true
    }
}

/// Whether this device may run setup on the server, and the one running.
#[derive(Default)]
pub(super) struct ServerSetup {
    /// The server offers `setup` and this device has `admin`.
    allowed: bool,
    job: Option<SetupJob>,
    _ask: Option<Task<()>>,
    _follow: Option<Task<()>>,
    stop: Option<Arc<AtomicBool>>,
}

impl ServerSetup {
    pub(super) fn running(&self) -> bool {
        self.job.as_ref().is_some_and(SetupJob::running)
    }
}

impl Drop for ServerSetup {
    fn drop(&mut self) {
        if let Some(stop) = &self.stop {
            stop.store(true, Ordering::Release);
        }
    }
}

/// Run setup on `choice` (a server) on its own thread, reporting to `news`.
/// Blocking.
fn follow(choice: &Backend, news: &async_channel::Sender<News>, stop: &AtomicBool) {
    let say = |item: News| news.send_blocking(item).is_ok();
    let client = match server::client(choice) {
        Ok(client) => client,
        Err(error) => {
            say(News::Failed(error.to_string()));
            return;
        }
    };
    let job = match client.setup() {
        Ok(job) => job,
        Err(error) => {
            say(News::Failed(match error.error.as_deref() {
                Some("setup_running") => "Setup is already running on the server.".into(),
                Some("upgrade_running") => {
                    "Gentle Shell is being upgraded on the server; try again when it's done.".into()
                }
                _ => error.message,
            }));
            return;
        }
    };
    let mut stream = match client.setup_stream(&job) {
        Ok(stream) => stream,
        Err(error) => {
            say(News::Failed(format!("Setup's output: {}", error.message)));
            return;
        }
    };
    while let Some(event) = stream.next(stop) {
        let item = match event {
            UpgradeEvent::Line(line) => News::Line(line),
            UpgradeEvent::Finished { ok, error, .. } => {
                say(News::Finished { ok, error });
                return;
            }
        };
        if !say(item) {
            return;
        }
    }
    if !stop.load(Ordering::Acquire) {
        say(News::Failed(
            "Setup's output stopped before it said how it ended. Check again in a moment.".into(),
        ));
    }
}

impl OnboardingView {
    /// Ask whether this device may run setup on the server agents run on
    /// (with every look: another pairing may answer differently).
    pub(super) fn ask_server_setup(&mut self, cx: &mut Context<Self>) {
        let choice = self.machine();
        if !choice.is_server() || self.server_setup.running() {
            self.server_setup.allowed = false;
            return;
        }
        self.server_setup._ask = Some(cx.spawn(async move |this, cx| {
            let allowed = cx
                .background_executor()
                .spawn(async move {
                    let offers = server::known_info(&choice).is_some_and(|info| info.has("setup"));
                    offers
                        && server::client(&choice)
                            .ok()
                            .and_then(|client| client.scopes().ok())
                            .is_some_and(|scopes| scopes.iter().any(|scope| scope == "admin"))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.server_setup.allowed = allowed;
                cx.notify();
            });
        }));
    }

    /// Run Gentle Shell's setup on the server, then look again.
    fn run_server_setup(&mut self, cx: &mut Context<Self>) {
        let choice = self.machine();
        if !choice.is_server() || !self.server_setup.allowed || self.server_setup.running() {
            return;
        }
        if let Some(stop) = self.server_setup.stop.take() {
            stop.store(true, Ordering::Release);
        }
        let stop = Arc::new(AtomicBool::new(false));
        self.server_setup.stop = Some(stop.clone());
        self.server_setup.job = Some(SetupJob::new());
        let (tx, rx) = async_channel::unbounded::<News>();
        let spawned = std::thread::Builder::new()
            .name("gentle-server-setup".into())
            .spawn(move || follow(&choice, &tx, &stop));
        if let Err(error) = spawned
            && let Some(job) = self.server_setup.job.as_mut()
        {
            job.apply(News::Failed(error.to_string()));
        }
        self.server_setup._follow = Some(cx.spawn(async move |this, cx| {
            while let Ok(news) = rx.recv().await {
                let alive = this.update(cx, |this, cx| {
                    let ended = this
                        .server_setup
                        .job
                        .as_mut()
                        .is_some_and(|job| job.apply(news));
                    if ended {
                        this.refresh(cx);
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

    /// The button (when this device may run it) and the setup running or
    /// just ended; `None` off a server or when there is nothing to show.
    pub(super) fn render_server_setup(&self, cx: &mut Context<Self>) -> Option<Div> {
        let name = self.server()?;
        let job = self.server_setup.job.as_ref();
        if !self.server_setup.allowed && job.is_none() {
            return None;
        }
        let running = self.server_setup.running();
        let button = self.server_setup.allowed.then(|| {
            Button::new("server-setup-run")
                .small()
                .ghost()
                .icon(Icon::new(IconName::Wrench))
                .label(format!("Run setup on {name}"))
                .loading(running)
                .disabled(running || self.checking || self.busy())
                .on_click(cx.listener(|this, _, _, cx| this.run_server_setup(cx)))
        });
        Some(
            v_flex()
                .gap_2()
                .child(
                    div()
                        .text_xs()
                        .text_color(ui::muted())
                        .whitespace_normal()
                        .child(format!(
                            "Gentle Shell's setup installs its companion packages for the home \
                             {name} runs, as gentle-shell setup does there."
                        )),
                )
                .children(button.map(|button| h_flex().child(button)))
                .children(job.map(|job| {
                    let headline = match &job.phase {
                        Phase::Running => h_flex()
                            .gap_2()
                            .text_xs()
                            .text_color(ui::muted())
                            .child(Spinner::new().xsmall().color(ui::accent()))
                            .child(format!("Running setup on {name}…")),
                        Phase::Done => h_flex()
                            .text_xs()
                            .text_color(ui::success())
                            .child(format!("Setup finished on {name}.")),
                        Phase::Failed(error) => h_flex()
                            .text_xs()
                            .text_color(ui::danger())
                            .child(error.clone()),
                    };
                    v_flex()
                        .gap_2()
                        .child(headline)
                        .when(!job.lines.is_empty(), |d| {
                            d.child(
                                ui::well_box().px_3().py_2().gap_0p5().children(
                                    job.lines.iter().rev().take(SHOWN_LINES).rev().map(|line| {
                                        div().text_color(ui::muted()).child(line.clone())
                                    }),
                                ),
                            )
                        })
                })),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{News, Phase, SetupJob};

    #[test]
    fn setup_keeps_its_output_and_ends_once() {
        let mut job = SetupJob::new();
        assert!(!job.apply(News::Line("Installing gentle-pi…".into())));
        assert!(job.apply(News::Finished {
            ok: true,
            error: None
        }));
        assert_eq!(job.phase, Phase::Done);
        assert_eq!(job.lines, ["Installing gentle-pi…"]);
        // Nothing moves it once it ended.
        assert!(!job.apply(News::Failed("late".into())));
        assert_eq!(job.phase, Phase::Done);
    }

    #[test]
    fn a_failed_or_refused_setup_says_why() {
        let mut failed = SetupJob::new();
        failed.apply(News::Finished {
            ok: false,
            error: Some("npm ERR! EACCES".into()),
        });
        assert_eq!(failed.phase, Phase::Failed("npm ERR! EACCES".into()));
        let mut unnamed = SetupJob::new();
        unnamed.apply(News::Finished {
            ok: false,
            error: None,
        });
        assert_eq!(unnamed.phase, Phase::Failed("Setup failed.".into()));
        let mut refused = SetupJob::new();
        refused.apply(News::Failed(
            "Setup is already running on the server.".into(),
        ));
        assert!(!refused.running());
    }
}
