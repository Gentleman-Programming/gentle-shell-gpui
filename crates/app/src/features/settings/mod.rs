//! Configuration: everything the user's Gentle Shell is already set up with,
//! read from the same homes the launcher uses, plus package management
//! (`gentle-shell install|remove`) and the Claude bridge's plan.

use std::path::PathBuf;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::slider::{SliderEvent, SliderState};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::{features::app_update, shared::themes, shared::ui};

mod agent_ui;
mod app;
mod background;
mod claude;
mod devices;
mod hydration;
mod layout;
mod maestro;
mod models;
mod music;
mod notifications;
mod packages;
mod snapshot;
mod theme;
mod upgrade;

pub use claude::forget_claude_auth;
pub use snapshot::npm_name;

use layout::{row, section};
use snapshot::{Snapshot, probe, read_snapshot};

pub enum SettingsEvent {
    Close,
    /// Packages or sign-ins changed: tabs should reload their backend.
    Changed,
    /// Run this Gentle Shell slash command in the active agent pane and
    /// report back through [`SettingsView::shell_done`].
    ShellCommand(String),
    /// `gentle-shell upgrade` finished: agent tabs restart on the new version.
    BackendUpdated,
    /// gentle-stream was installed or removed: the title bar's ♪ follows.
    MusicChanged,
    /// Configuration → Devices' switches changed.
    DevicesChanged(crate::features::setup::devices::Prefs),
    /// An action from the Setup & requirements checklist (sign in, a
    /// terminal command, an install that finished) for the window to carry out.
    Setup(crate::features::onboarding::OnboardingEvent),
}

pub struct SettingsView {
    cwd: PathBuf,
    snapshot: Option<Snapshot>,
    package_input: Entity<InputState>,
    running: Option<String>,
    log: Vec<String>,
    _load: Task<()>,
    _job: Task<()>,
    _subscription: Subscription,
    /// Window transparency, 0–100 %.
    transparency: Entity<SliderState>,
    _transparency: Subscription,
    /// A `/gentle:background-subagents` change sent to the shell, not yet answered.
    background_pending: bool,
    /// The shell's answer to the last change: `(text, failed)`.
    background_note: Option<(String, bool)>,
    /// `gentle-shell upgrade` running.
    upgrading: bool,
    /// How the last `gentle-shell upgrade` ended, with its last output lines.
    upgrade: Option<upgrade::UpgradeResult>,
    /// "Skip this version" couldn't be saved.
    skip_error: Option<String>,
    /// Re-render as this app's own update moves along.
    _app_update: Subscription,
    /// A failed notifications.json write.
    notifications_note: Option<String>,
    /// Setup & requirements: the onboarding checklist, embedded.
    setup: Entity<crate::features::onboarding::OnboardingView>,
    _setup: Subscription,
    /// UI testing (Maestro): what was found, the job running and its log.
    maestro: maestro::Maestro,
    /// Music (gentle-stream): what was found, the job running and its log.
    music: music::Music,
    /// Devices: the switches, what this machine has, an install running.
    devices: devices::Devices,
    /// Engram's own update.
    engram: upgrade::EngramUpdate,
    /// Theme: importing and sharing themes.
    theme_share: theme::ThemeShare,
    /// The window's active agent tab: its Gentle Shell reports update status
    /// when it offers `get_update_status`.
    session: Option<Entity<crate::features::session::SessionView>>,
    _session_sub: Option<Subscription>,
    /// Gentle Shell's own update status, when it reports one.
    shell_updates: upgrade::ShellUpdates,
}

impl EventEmitter<SettingsEvent> for SettingsView {}

impl SettingsView {
    pub fn new(
        cwd: PathBuf,
        session: Option<Entity<crate::features::session::SessionView>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let package_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("npm:package-name, git:github.com/…, or a local path")
        });
        let subscription = cx.subscribe_in(
            &package_input,
            window,
            |this, input, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    let source = input.read(cx).value().trim().to_string();
                    if !source.is_empty() {
                        input.update(cx, |s, cx| s.set_value("", window, cx));
                        this.run_package("install", source, cx);
                    }
                }
            },
        );
        let percent = (cx.global::<themes::Themes>().transparency * 100.).round();
        let transparency = cx.new(|_| {
            SliderState::new()
                .min(0.)
                .max(100.)
                .step(1.)
                .default_value(percent)
        });
        // Drag re-tints live; the file is written once, on release.
        let transparency_sub = cx.subscribe(&transparency, |_, _, event: &SliderEvent, cx| {
            let (value, persist) = match event {
                SliderEvent::Change(value) => (value.start(), false),
                SliderEvent::Release(value) => (value.start(), true),
            };
            themes::set_transparency(value / 100., persist, cx);
        });
        let setup = cx.new(crate::features::onboarding::OnboardingView::embedded);
        let setup_sub = cx.subscribe(&setup, |_, _, event, cx| {
            cx.emit(SettingsEvent::Setup(event.clone()));
        });
        let mut this = Self {
            cwd,
            snapshot: None,
            package_input,
            running: None,
            log: Vec::new(),
            _load: Task::ready(()),
            _job: Task::ready(()),
            _subscription: subscription,
            transparency,
            _transparency: transparency_sub,
            background_pending: false,
            background_note: None,
            upgrading: false,
            upgrade: None,
            skip_error: None,
            _app_update: cx.observe_global::<app_update::AppUpdate>(|_, cx| cx.notify()),
            notifications_note: None,
            setup,
            _setup: setup_sub,
            maestro: maestro::Maestro::default(),
            music: music::Music::default(),
            devices: devices::Devices::default(),
            engram: upgrade::EngramUpdate::default(),
            theme_share: theme::ThemeShare::new(window, cx),
            _session_sub: session
                .as_ref()
                .map(|session| cx.observe(session, |this, _, cx| this.update_status_changed(cx))),
            session,
            shell_updates: upgrade::ShellUpdates::default(),
        };
        this.reload(cx);
        this.refresh_maestro(cx);
        this.refresh_music(cx);
        this.refresh_devices(cx);
        this
    }

    /// gentle-shell can start, as of the Setup & requirements checklist.
    pub fn setup_usable(&self, cx: &App) -> bool {
        self.setup.read(cx).usable()
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        self.reload_with(false, cx);
    }

    /// Re-read the configuration. Update status comes from the active agent
    /// tab's Gentle Shell when it reports it (`refresh_updates` looks it up,
    /// as a manual check); otherwise the npm path below runs, as before.
    fn reload_with(&mut self, refresh_updates: bool, cx: &mut Context<Self>) {
        let shell_updates = self.ask_update_status(refresh_updates, cx);
        if !shell_updates {
            self.shell_updates.status = None;
        }
        let cwd = self.cwd.clone();
        self._load = cx.spawn(async move |this, cx| {
            let snapshot = cx
                .background_executor()
                .spawn(async move {
                    let homes = pi_rpc::resolve_homes();
                    let mut snapshot = read_snapshot(&homes.agent, &homes.config, &cwd);
                    let spec = pi_rpc::resolve_backend(&cwd).ok();
                    if let Some(spec) = &spec {
                        probe(spec, &mut snapshot);
                    }
                    // gentle-ai updates its managed install, and its
                    // launcher config, itself.
                    snapshot.managed = spec
                        .as_ref()
                        .is_some_and(pi_rpc::LaunchSpec::managed_by_gentle_ai);
                    snapshot.skipped_release = crate::features::updates::skipped();
                    // Cached for a day in the launcher config, so reopening
                    // Configuration stays off the network.
                    if !snapshot.managed && !shell_updates {
                        snapshot.update =
                            crate::features::updates::current_from_versions(&snapshot.versions)
                                .and_then(crate::features::updates::check);
                    }
                    snapshot.engram_update = crate::features::setup::engram_version()
                        .and_then(|v| crate::features::updates::engram::check(&v));
                    snapshot
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.snapshot = Some(snapshot);
                cx.notify();
            });
        });
    }
}

impl Render for SettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body: AnyElement = match &self.snapshot {
            None => h_flex()
                .p_6()
                .gap_2()
                .justify_center()
                .text_sm()
                .text_color(ui::muted())
                .child(Spinner::new().small().color(ui::accent()))
                .child("Reading your Gentle Shell configuration…")
                .into_any_element(),
            Some(s) => ui::appear(
                "settings-content",
                v_flex()
                    .gap_3()
                    .p_4()
                    .child(
                        section(IconName::SquareTerminal, "Gentle Shell")
                            .child(row(
                                "Home",
                                if s.home_mode.is_empty() {
                                    s.agent.display().to_string()
                                } else {
                                    s.home_mode.clone()
                                },
                            ))
                            .child(row("Agent dir", s.agent.display().to_string()))
                            .child(row("Config dir", s.config.display().to_string()))
                            .child(row(
                                "Launcher",
                                s.launcher
                                    .as_ref()
                                    .map(|p| p.display().to_string())
                                    .unwrap_or_else(|| "not found".into()),
                            ))
                            .child(row("Versions", s.versions.join(" · ")))
                            .children(
                                self.shell_updates
                                    .status
                                    .iter()
                                    .flat_map(|status| {
                                        crate::features::updates::status::status_notes(
                                            status, s.managed,
                                        )
                                    })
                                    .map(|(label, text)| row(label, text)),
                            )
                            .when_some(self.update_offer(s), |d, offer| {
                                d.child(self.render_update(offer, cx))
                            })
                            .children(self.upgrade.as_ref().map(upgrade::upgrade_result))
                            .child(self.render_engram_update(s.engram_update.clone(), cx))
                            .child(
                                h_flex()
                                    .pt_2()
                                    .gap_2()
                                    .flex_wrap()
                                    .child({
                                        let dir = s.agent.clone();
                                        Button::new("open-agent-dir")
                                            .ghost()
                                            .small()
                                            .icon(Icon::new(IconName::FolderOpen))
                                            .label("Open agent dir")
                                            .on_click(move |_, _, cx| {
                                                cx.open_url(&format!("file://{}", dir.display()))
                                            })
                                    })
                                    .child({
                                        let dir = s.config.clone();
                                        Button::new("open-config-dir")
                                            .ghost()
                                            .small()
                                            .icon(Icon::new(IconName::FolderOpen))
                                            .label("Open config dir")
                                            .on_click(move |_, _, cx| {
                                                cx.open_url(&format!("file://{}", dir.display()))
                                            })
                                    }),
                            ),
                    )
                    .child(
                        section(IconName::CircleCheck, "Setup & requirements")
                            .child(self.setup.clone()),
                    )
                    .child(self.devices_section(cx))
                    .child(self.maestro_section(cx))
                    .child(self.music_section(cx))
                    .child(self.hydration_section(cx))
                    .child(
                        div()
                            .id("theme-drop")
                            .rounded_xl()
                            .drag_over::<ExternalPaths>(|style, _, _, _| {
                                style.bg(ui::surface_hover())
                            })
                            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                                this.theme_drop(paths, cx)
                            }))
                            .child(self.theme_section(cx)),
                    )
                    .child(models::models_section(s))
                    .child(self.gentle_ai_section(s, cx))
                    .child(self.agent_ui_section(cx))
                    .child(self.notifications_section(cx))
                    .child(self.bridge_section(s, cx))
                    .child(self.packages_section(s, cx)),
            )
            .into_any_element(),
        };
        let (backdrop, panel) = ui::screen(Some(820.));
        backdrop.occlude().child(ui::pop_in(
            "settings-screen",
            panel
                .w_full()
                .max_w(rems(820. / 16.))
                .child(
                    ui::screen_header()
                        .child(
                            Icon::new(IconName::Settings)
                                .small()
                                .text_color(ui::muted()),
                        )
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_base()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(ui::fg())
                                .child("Configuration"),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .text_color(ui::muted())
                                .child("Your Gentle Shell setup, as the terminal sees it"),
                        )
                        .child(
                            Button::new("settings-refresh")
                                .ghost()
                                .xsmall()
                                .icon(Icon::new(IconName::RefreshCw))
                                .tooltip("Reload and check for updates")
                                .on_click(cx.listener(|this, _, _, cx| this.reload_with(true, cx))),
                        )
                        .child(
                            Button::new("settings-close")
                                .ghost()
                                .xsmall()
                                .icon(Icon::new(IconName::X))
                                .on_click(cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::Close))),
                        ),
                )
                .child(
                    div()
                        .id("settings-body")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .child(div().px_4().pt_4().child(app::app_section(cx)))
                        .child(body),
                ),
        ))
    }
}
