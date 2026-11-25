//! Onboarding: the first-run screen (and "Setup & requirements" any time).
//! It shows what this machine has for Gentle Shell (`crate::features::setup` checks),
//! installs what can be installed unattended with a live log, hands anything
//! that needs a password or an interactive login to a terminal tab, and sends
//! provider sign-in to the Providers screen. The other way in is connecting
//! to a Gentle Shell server (`gentle-shell serve share`'s code): agents then
//! run there, nothing is installed on this computer and only the server is
//! checked.

use std::collections::VecDeque;

use gentle_core::backend::Backend;
use gpui_kit::*;

use crate::features::settings::{ConnectEvent, ServerConnect};
use crate::features::setup::{self, Check, CheckId};

mod checks;
mod install;
mod sequence;
mod view;

use checks::{install_plan, required_ok, usable, with_registry};

/// Where the checklist is shown.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// The onboarding screen: hero, checklist, footer, in a card.
    Screen,
    /// Inside Configuration: the checklist and its actions, without the
    /// screen's hero, footer and frame.
    Embedded,
    /// The welcome screen's install sequence: numbered steps on a progress
    /// rail; the welcome draws the heading and footer.
    Sequence,
}

/// One look at the machine: what the screen shows and whether it must open.
pub struct Probe {
    pub platform: String,
    pub first_run: bool,
    pub checks: Vec<Check>,
}

impl Probe {
    /// BLOCKING: runs every check; call it on the background executor.
    pub fn run() -> Self {
        // The checks run where agents do: the badge says so.
        let platform = match crate::features::backend::saved() {
            gentle_core::backend::Backend::Local => setup::platform_label(),
            choice => format!("Agents on {}", choice.label()),
        };
        Self {
            platform,
            first_run: setup::first_run(),
            checks: setup::run_checks(),
        }
    }

    /// Onboarding opens by itself on a first run or when something required
    /// is missing.
    pub fn needs_onboarding(&self) -> bool {
        self.first_run || !required_ok(&self.checks)
    }
}

#[derive(Clone, Debug)]
pub enum OnboardingEvent {
    /// Continue (after recording completion) or Skip.
    Close,
    /// An install finished; tabs whose backend failed may start now.
    Installed,
    /// Open the Providers screen.
    SignIn,
    /// Open a terminal tab with this command typed (needs the user: a
    /// password, an interactive login).
    Terminal(String),
    /// Where agents run changed here: a Gentle Shell server was paired
    /// (nothing needs installing on this computer then), or this computer
    /// was chosen again.
    RunsOnChanged,
}

pub struct OnboardingView {
    mode: Mode,
    /// The sequence's steps start coming in after this (the welcome's
    /// intro plays first).
    entrance: std::time::Duration,
    platform: Option<String>,
    first_run: bool,
    /// `None` until the first look finishes.
    checks: Option<Vec<Check>>,
    checking: bool,
    installing: Option<CheckId>,
    /// "Install everything": what still runs after the current install.
    queue: VecDeque<CheckId>,
    /// Output of the last install, and the check it belongs to.
    log: Vec<String>,
    log_for: Option<CheckId>,
    failure: Option<(CheckId, String)>,
    /// Emit [`OnboardingEvent::Installed`] once the checks after an install land.
    announce: bool,
    /// The provider check as the active agent tab's Gentle Shell answered
    /// it (`get_auth_status`), applied over every look at the machine.
    registry_sign_in: Option<Result<String, String>>,
    /// Connecting to a Gentle Shell server instead of setting up this
    /// computer (the first-run paths; Configuration has its own).
    connect: Option<(Entity<ServerConnect>, Subscription)>,
    _probe: Task<()>,
    _job: Task<()>,
}

impl EventEmitter<OnboardingEvent> for OnboardingView {}

impl OnboardingView {
    /// `probe` is a look already taken (the startup check); `None` looks now.
    /// Offers connecting to a Gentle Shell server as the other way in.
    pub fn new(probe: Option<Probe>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut this = Self::build(probe, cx);
        let url = match crate::features::backend::saved() {
            Backend::Server { url, .. } => url,
            _ => String::new(),
        };
        let connect = cx.new(|cx| ServerConnect::new(url, window, cx));
        let paired = cx.subscribe(&connect, |this, _, event: &ConnectEvent, cx| {
            let ConnectEvent::Paired(choice, _) = event;
            this.choose_runs_on(choice.clone(), cx);
        });
        this.connect = Some((connect, paired));
        this
    }

    fn build(probe: Option<Probe>, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            mode: Mode::Screen,
            entrance: std::time::Duration::ZERO,
            platform: None,
            first_run: false,
            checks: None,
            checking: false,
            installing: None,
            queue: VecDeque::new(),
            log: Vec::new(),
            log_for: None,
            failure: None,
            announce: false,
            registry_sign_in: None,
            connect: None,
            _probe: Task::ready(()),
            _job: Task::ready(()),
        };
        match probe {
            Some(probe) => this.apply(probe, cx),
            None => this.refresh(cx),
        }
        this
    }

    /// The checklist for Configuration → Setup & requirements: looks at the
    /// machine now, installs and hands off actions like the screen does.
    /// Where agents run is Configuration's own section there.
    pub fn embedded(cx: &mut Context<Self>) -> Self {
        let mut this = Self::build(None, cx);
        this.mode = Mode::Embedded;
        this
    }

    /// Save where agents run (a server just paired, or this computer
    /// again), then look again: on a server only the server is checked and
    /// nothing is installed here.
    fn choose_runs_on(&mut self, choice: Backend, cx: &mut Context<Self>) {
        if let Err(err) = crate::features::backend::save(&choice) {
            eprintln!("gentle-shell-gpui: could not save where agents run: {err}");
        }
        self.failure = None;
        self.checks = None;
        self.refresh(cx);
        cx.emit(OnboardingEvent::RunsOnChanged);
    }

    /// The Gentle Shell server agents run on, as saved: its name.
    fn server(&self) -> Option<String> {
        match crate::features::backend::saved() {
            Backend::Server { name, .. } => Some(name),
            _ => None,
        }
    }

    /// The install sequence for the welcome screen; its steps come in after
    /// `entrance`.
    pub fn sequence(
        probe: Option<Probe>,
        entrance: std::time::Duration,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = Self::new(probe, window, cx);
        this.mode = Mode::Sequence;
        this.entrance = entrance;
        this
    }

    /// Steps come in right away from now on (the intro already played).
    pub fn skip_entrance(&mut self) {
        self.entrance = std::time::Duration::ZERO;
    }

    /// An install is running: the screen must stay open until it ends.
    pub fn busy(&self) -> bool {
        self.installing.is_some()
    }

    /// gentle-shell can start, as of the last look.
    pub fn usable(&self) -> bool {
        self.checks.as_deref().is_some_and(usable)
    }

    /// Everything required is ready, as of the last look.
    pub fn ready(&self) -> bool {
        self.checks.as_deref().is_some_and(required_ok)
    }

    /// No model provider is signed in yet (it can wait; Continue doesn't).
    pub fn needs_sign_in(&self) -> bool {
        self.checks.as_deref().is_some_and(|checks| {
            checks
                .iter()
                .any(|c| c.fix == setup::Fix::SignIn && !checks::is_ok(c))
        })
    }

    /// The first look at the machine is still running.
    pub fn looking(&self) -> bool {
        self.checks.is_none()
    }

    /// A look is running (the first, or "Check again").
    pub fn checking(&self) -> bool {
        self.checking
    }

    /// "Install everything" has something to run.
    pub fn can_install(&self) -> bool {
        self.checks
            .as_deref()
            .is_some_and(|checks| !install_plan(checks).is_empty())
    }

    /// Run every unattended install that's needed, in order.
    pub fn install_everything(&mut self, cx: &mut Context<Self>) {
        self.install_all(cx);
    }

    /// Look at the machine again.
    pub fn check_again(&mut self, cx: &mut Context<Self>) {
        self.refresh(cx);
    }

    /// Gentle Shell's model registry answered which providers have
    /// credentials (by id): the provider check follows it from now on,
    /// instead of reading `auth.json`, `models.json` and the environment.
    pub fn adopt_signed_in(&mut self, configured: &[&str], cx: &mut Context<Self>) {
        let answer = gentle_core::setup::registry_sign_in(configured.iter().copied());
        if let Some(checks) = self.checks.as_mut() {
            with_registry(checks, &answer);
        }
        self.registry_sign_in = Some(answer);
        cx.notify();
    }
}
