//! Configuration: everything the user's Gentle Shell is already set up with,
//! read from the same homes the launcher uses, plus package management
//! (`gentle-shell install|remove`) and the Claude bridge's plan.

use std::path::PathBuf;

use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::slider::{SliderEvent, SliderState};
use gpui_kit::*;

use crate::{features::app_update, shared::themes};

mod agent_ui;
mod app;
mod background;
mod claude;
mod devices;
mod events;
mod hydration;
mod layout;
mod maestro;
mod models;
mod music;
mod navigation;
mod notifications;
mod packages;
mod pages;
mod services;
mod snapshot;
mod theme;
mod upgrade;
mod view;

pub use claude::forget_claude_auth;
pub use events::SettingsEvent;
pub use snapshot::npm_name;

use navigation::SettingsPage;
use snapshot::Snapshot;

pub struct SettingsView {
    cwd: PathBuf,
    page: SettingsPage,
    body_scroll: ScrollHandle,
    settings_search: Entity<InputState>,
    _settings_search: Subscription,
    compact_categories_open: bool,
    snapshot: Option<Snapshot>,
    /// Package input, progress, output and event subscription.
    package: packages::PackageUiState,
    _load: Task<()>,
    /// Existing shared lifecycle for package and Gentle Shell upgrade commands.
    _job: Task<()>,
    /// Window transparency, 0–100 %.
    transparency: Entity<SliderState>,
    _transparency: Subscription,
    /// Pending background-helper policy command and its feedback.
    background: background::BackgroundUiState,
    /// Gentle Shell upgrade status and its feedback.
    shell_update: upgrade::ShellUpdateUiState,
    /// Re-render as this app's own update moves along.
    _app_update: Subscription,
    /// Notification configuration write feedback.
    notifications: notifications::NotificationUiState,
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
    /// Active agent tab, if available, for the new Desktop RPC update status.
    session: Option<Entity<crate::features::session::SessionView>>,
    _session_sub: Option<Subscription>,
    /// RPC status and its in-flight ticket; separate from upgrade UI state.
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
        let package = packages::PackageUiState::new(window, cx);
        let settings_search =
            cx.new(|cx| InputState::new(window, cx).placeholder("Find a setting…"));
        let settings_search_subscription = cx.subscribe(
            &settings_search,
            |this, input, event: &InputEvent, cx| match event {
                InputEvent::Change => cx.notify(),
                InputEvent::PressEnter { .. } => {
                    let query = input.read(cx).value().to_string();
                    if !query.trim().is_empty()
                        && let Some(page) = SettingsPage::ALL
                            .into_iter()
                            .find(|page| page.matches_query(&query))
                    {
                        this.navigate_to(page, cx);
                    }
                }
                _ => {}
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
            page: SettingsPage::default(),
            body_scroll: ScrollHandle::new(),
            settings_search,
            _settings_search: settings_search_subscription,
            compact_categories_open: false,
            snapshot: None,
            package,
            _load: Task::ready(()),
            _job: Task::ready(()),
            transparency,
            _transparency: transparency_sub,
            background: background::BackgroundUiState::default(),
            shell_update: upgrade::ShellUpdateUiState::default(),
            _app_update: cx.observe_global::<app_update::AppUpdate>(|_, cx| cx.notify()),
            notifications: notifications::NotificationUiState::default(),
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

    /// Reuse a single navigation path for the sidebar, compact picker and search.
    fn navigate_to(&mut self, page: SettingsPage, cx: &mut Context<Self>) {
        self.compact_categories_open = false;
        if self.page != page {
            self.page = page;
            self.body_scroll.scroll_to_item(0);
        }
        cx.notify();
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        self.reload_with(false, cx);
    }

    /// Prefer update information from the active agent's Desktop RPC.
    /// Fall back to the existing launcher/registry path when unavailable.
    fn reload_with(&mut self, refresh_updates: bool, cx: &mut Context<Self>) {
        let shell_updates = self.ask_update_status(refresh_updates, cx);
        if !shell_updates {
            self.shell_updates.status = None;
        }
        let cwd = self.cwd.clone();
        self._load = cx.spawn(async move |this, cx| {
            let snapshot = cx
                .background_executor()
                .spawn(async move { services::snapshot::load(&cwd, shell_updates) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.snapshot = Some(snapshot);
                cx.notify();
            });
        });
    }
}
