//! Screens drawn over the workspace: palette, settings, providers, sessions,
//! usage, memory, profiles, onboarding; and the surge. Floating web and
//! device cards live in `stages.rs`.

use super::*;
use crate::features::explorer::Purpose;

impl Workspace {
    // ── Screens ─────────────────────────────────────────────────────────────

    /// Open the switcher on `category`; the same shortcut again closes it, the
    /// other one changes category.
    pub(super) fn open_palette(
        &mut self,
        category: Category,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some((Overlay::Palette(view), _)) = &self.overlay {
            if view.read(cx).category() == category {
                self.close_overlay(window, cx);
            } else {
                view.update(cx, |view, cx| view.set_category(category, window, cx));
            }
            return;
        }
        self.start_active_session(window, cx);
        let commands = self
            .active_session()
            .map(|t| t.read(cx).commands())
            .unwrap_or_default();
        let workspaces = self.workspace_rows();
        let input = PaletteInput {
            commands,
            workspaces,
            root: self.space().root.clone(),
            category,
            tab_layout: self.tab_layout,
        };
        let view = cx.new(|cx| PaletteView::new(input, window, cx));
        let subscription = cx.subscribe_in(
            &view,
            window,
            |this, _, event: &PaletteEvent, window, cx| match event {
                PaletteEvent::Close => this.close_overlay(window, cx),
                PaletteEvent::Run(PaletteAction::RenameWorkspace { index, name }) => {
                    // Renaming happens in place; the switcher stays open.
                    this.rename_workspace(*index, name.clone(), cx);
                }
                PaletteEvent::Run(action) => {
                    this.close_overlay(window, cx);
                    this.run_palette_action(action.clone(), window, cx);
                }
            },
        );
        self.overlay = Some((Overlay::Palette(view), subscription));
        cx.notify();
    }

    /// Every workspace as the switcher lists it, with what its agents do.
    pub(super) fn workspace_rows(&self) -> Vec<WorkspaceRow> {
        self.spaces
            .iter()
            .enumerate()
            .map(|(index, space)| WorkspaceRow {
                index,
                name: space.name.clone(),
                tabs: space.tabs.len(),
                current: index == self.current,
                activity: self.space_activity(space),
            })
            .collect()
    }

    pub(super) fn run_palette_action(
        &mut self,
        action: PaletteAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let needs_agent = matches!(
            action,
            PaletteAction::Agents
                | PaletteAction::NewSession
                | PaletteAction::Command(_)
                | PaletteAction::Screen(_)
        );
        if needs_agent {
            self.ensure_agent_built(window, cx);
        }
        let tab = self.active_session();
        match action {
            PaletteAction::Providers => self.open_providers(window, cx),
            PaletteAction::Sessions => self.open_sessions(window, cx),
            PaletteAction::Profiles => self.open_profiles(window, cx),
            PaletteAction::Usage => self.open_usage(window, cx),
            PaletteAction::Memory => {
                let cwd = self.active_cwd(cx);
                self.open_memory(cwd, Default::default(), window, cx)
            }
            PaletteAction::Docs => self.open_docs(window, cx),
            PaletteAction::History => {
                let cwd = self.active_cwd(cx);
                self.open_history(cwd, window, cx)
            }
            PaletteAction::Devices => self.devices.update(cx, |panel, cx| panel.show(cx)),
            PaletteAction::Settings => self.open_settings(window, cx),
            PaletteAction::Setup => self.open_onboarding(None, window, cx),
            PaletteAction::Welcome => self.open_welcome(None, window, cx),
            PaletteAction::InstallGsd => self.install_gsd(window, cx),
            PaletteAction::OpenExplorer => self.open_explorer(window, cx),
            PaletteAction::NewTab => self.on_new_tab(&NewTab, window, cx),
            PaletteAction::NewTerminalTab => self.on_new_terminal_tab(&NewTerminalTab, window, cx),
            PaletteAction::NewCanvasTab => self.on_new_canvas_tab(&NewCanvasTab, window, cx),
            PaletteAction::TilePane => self.tile(None, window, cx),
            PaletteAction::SplitRight => self.split(SplitAxis::Horizontal, None, window, cx),
            PaletteAction::SplitDown => self.split(SplitAxis::Vertical, None, window, cx),
            PaletteAction::NewAgentPane => self.split_with_kind(PaneKind::Agent, window, cx),
            PaletteAction::NewTerminalPane => self.split_with_kind(PaneKind::Terminal, window, cx),
            PaletteAction::ClosePane => self.on_close_pane(&ClosePane, window, cx),
            PaletteAction::ReopenClosed => self.reopen_closed(window, cx),
            PaletteAction::ToggleSidebar => self.toggle_sidebar(cx),
            PaletteAction::SetTabLayout(layout) => self.set_tab_layout(layout, cx),
            PaletteAction::Agents => {
                if let Some(tab) = tab {
                    tab.update(cx, |s, cx| s.open_agents_panel(AgentsTab::Agents, cx));
                }
            }
            PaletteAction::NewSession => {
                if let Some(tab) = tab {
                    tab.update(cx, |s, cx| s.new_session(window, cx));
                }
            }
            PaletteAction::Command(name) => {
                if let Some(tab) = tab {
                    tab.update(cx, |s, cx| s.run_prompt(format!("/{name}"), cx));
                }
            }
            PaletteAction::Screen(screen) => self.open_screen(screen, tab, window, cx),
            PaletteAction::SwitchWorkspace(index) => self.switch_workspace(index, window, cx),
            PaletteAction::RenameWorkspace { index, name } => {
                self.rename_workspace(index, name, cx)
            }
            PaletteAction::DeleteWorkspace(index) => {
                self.confirm_delete_workspace(index, window, cx)
            }
            PaletteAction::CreateWorkspace {
                name,
                pick_folder: false,
            } => {
                // The current folder, on the shown workspace's machine (the
                // focused agent's, else the workspace's folder there).
                let root = self.new_agent_cwd(cx);
                let machine = self.space().machine();
                self.create_workspace(Some(name), root, &machine, window, cx);
            }
            PaletteAction::CreateWorkspace {
                name,
                pick_folder: true,
            } => self.pick_workspace_folder(Some(name), window, cx),
            PaletteAction::ShowServerFolder(root, server) => {
                self.show_folder(root, &server, window, cx)
            }
            PaletteAction::OpenFolderOn(server) => self.open_folder_on(server, window, cx),
            PaletteAction::OpenFile(path) => self.open_file(path, window, cx),
            PaletteAction::SwitchBranch(branch) => self.confirm_switch_branch(branch, window, cx),
        }
    }

    /// The folder explorer (⌘O / Ctrl+O, Open folder…), on the shown
    /// workspace's machine from where a new agent would start; a row at its
    /// top switches to this computer or another paired server.
    pub(super) fn open_explorer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((Overlay::Explorer(_), _)) = &self.overlay {
            self.close_overlay(window, cx);
            return;
        }
        let machine = self.space().machine();
        self.open_folder_on(machine, window, cx);
    }

    /// The folder explorer at `dir` on `machine` (one of `machines`, which
    /// it can switch between) for `purpose`: a folder chosen for
    /// [`Purpose::Choose`] goes to `chosen` with the machine browsed. A
    /// folder opened from it opens on that machine
    /// ([`Self::show_folder`]).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn show_explorer(
        &mut self,
        machine: gentle_core::backend::Backend,
        machines: Vec<gentle_core::backend::Backend>,
        dir: PathBuf,
        purpose: Purpose,
        chosen: impl Fn(
            &mut Self,
            PathBuf,
            gentle_core::backend::Backend,
            &mut Window,
            &mut Context<Self>,
        ) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = cx.new(|cx| ExplorerView::new(machine, machines, dir, purpose, window, cx));
        let subscription = cx.subscribe_in(
            &view,
            window,
            move |this, view, event: &ExplorerEvent, window, cx| {
                let machine = view.read(cx).machine().clone();
                this.close_overlay(window, cx);
                match event {
                    // Open Gentle Shell here: an agent in that folder, on
                    // the machine browsed.
                    ExplorerEvent::OpenAgent(folder) => {
                        this.show_folder(folder.clone(), &machine, window, cx);
                        this.open_tab(PaneKind::Agent, folder.clone(), None, None, window, cx);
                    }
                    ExplorerEvent::OpenTerminal(folder) => {
                        this.show_folder(folder.clone(), &machine, window, cx);
                        this.open_tab(PaneKind::Terminal, folder.clone(), None, None, window, cx);
                    }
                    ExplorerEvent::Choose(folder) => {
                        chosen(this, folder.clone(), machine, window, cx)
                    }
                    ExplorerEvent::OpenFile(path) => {
                        // A file of another machine than the shown
                        // workspace's opens in that machine's workspace.
                        if !gentle_core::machines::same(&this.space().machine(), &machine)
                            && let Some(parent) = path.parent()
                        {
                            this.show_folder(parent.to_path_buf(), &machine, window, cx);
                        }
                        this.open_file(path.clone(), window, cx)
                    }
                    ExplorerEvent::Close => {}
                }
            },
        );
        self.overlay = Some((Overlay::Explorer(view), subscription));
        cx.notify();
    }

    /// Show the workspace rooted at `folder` on `machine`, creating it (a
    /// new workspace opens with an agent tab). `gsd <folder>` and the
    /// explorer both land here. A Gentle Shell server's folder is as the
    /// server names it, never resolved on this computer.
    pub(super) fn show_folder(
        &mut self,
        folder: PathBuf,
        machine: &gentle_core::backend::Backend,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let index = self.space_for(folder, machine);
        self.switch_workspace(index, window, cx);
    }

    /// Palette → Install `gsd` command; the result arrives as a notification.
    fn install_gsd(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |_, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { crate::platform::gsd::install() })
                .await;
            let _ = cx.update(|window, cx| {
                let note = match result {
                    Ok(message) => Notification::success(message),
                    Err(message) => Notification::error(message),
                };
                window.push_notification(note, cx);
            });
        })
        .detach();
    }

    /// Play an effort level's surge over the window; another change
    /// restarts it at the new level.
    pub(super) fn play_surge(
        &mut self,
        level: crate::shared::surge::Level,
        cx: &mut Context<Self>,
    ) {
        self.surge_runs += 1;
        let run = self.surge_runs;
        self.surge = Some((run, std::time::Instant::now(), level));
        self._surge_end = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(level.duration()).await;
            let _ = this.update(cx, |this, cx| {
                if this.surge.is_some_and(|(playing, _, _)| playing == run) {
                    this.surge = None;
                    cx.notify();
                }
            });
        }));
        cx.notify();
    }

    /// The Agents window of `session` (else the active one), on `tab`.
    fn open_agents_window(
        &mut self,
        tab: AgentsTab,
        session: Option<Entity<SessionView>>,
        cx: &mut Context<Self>,
    ) {
        if let Some(session) = session.or_else(|| self.active_session()) {
            session.update(cx, |s, cx| s.open_agents_panel(tab, cx));
        }
    }

    /// The native screen of a terminal-only Gentle Shell command; `session`
    /// is the agent it concerns (the one that ran it, else the active one).
    pub(super) fn open_screen(
        &mut self,
        screen: Screen,
        session: Option<Entity<SessionView>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match screen {
            Screen::Usage => self.open_usage(window, cx),
            Screen::Stats => {
                self.open_usage(window, cx);
                if let Some((Overlay::Usage(view), _)) = &self.overlay {
                    view.update(cx, |view, cx| view.show_stats(cx));
                }
            }
            Screen::Agents => self.open_agents_window(AgentsTab::Agents, session, cx),
            // Background jobs are the Jobs tab of the Agents window.
            Screen::Jobs => self.open_agents_window(AgentsTab::Jobs, session, cx),
            Screen::Profiles => self.open_profiles(window, cx),
            Screen::Palette => {
                self.close_overlay(window, cx);
                self.open_palette(Category::Commands, window, cx);
            }
            Screen::Changes => {
                let cwd = match &session {
                    Some(session) => session.read(cx).cwd().clone(),
                    None => self.active_cwd(cx),
                };
                self.close_overlay(window, cx);
                self.open_review(cwd, window, cx);
            }
            Screen::Tower => {
                self.close_overlay(window, cx);
                self.open_tower(window, cx);
            }
        }
    }

    /// Gentle Tower in a tab of its own, so it spans the whole window: the
    /// one already open, wherever it is, else a new tab.
    pub(super) fn open_tower(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let open = self
            .panes
            .iter()
            .find(|(_, pane)| pane.item.kind == PaneKind::Tower)
            .map(|(id, _)| *id);
        match open {
            Some(id) => self.reveal_pane(id, window, cx),
            None => {
                let cwd = self.active_cwd(cx);
                self.open_tab(PaneKind::Tower, cwd, None, None, window, cx);
            }
        }
    }

    /// The review screen of the repository `cwd` is in, as a modal over the
    /// window (Esc closes it).
    pub(super) fn open_review(
        &mut self,
        cwd: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = cx.new(|cx| ReviewView::new(cwd, window, cx));
        let subscription = cx.subscribe_in(
            &view,
            window,
            |this, _, event: &ReviewEvent, window, cx| match event {
                ReviewEvent::Close => this.close_overlay(window, cx),
                // Opened like the Files sidebar opens it, once the review
                // is out of the way.
                ReviewEvent::OpenFile(path) => {
                    this.close_overlay(window, cx);
                    this.open_file(path.clone(), window, cx);
                }
            },
        );
        // Focused, so Esc reaches the workspace's CloseOverlay through it.
        view.focus_handle(cx).focus(window, cx);
        self.overlay = Some((Overlay::Review(view), subscription));
        cx.notify();
    }

    /// The window's active agent tab (`active_session`) is the one the
    /// sessions screen asks to list and switch, when its Gentle Shell offers
    /// it; its backend is started (probe included) first.
    pub(super) fn open_sessions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.start_active_session(window, cx);
        let cwd = self.active_cwd(cx);
        let session = self.active_session();
        let current = session.as_ref().and_then(|t| t.read(cx).session_file());
        let view = cx.new(|cx| SessionsView::new(cwd, current, session, window, cx));
        let subscription = cx.subscribe_in(
            &view,
            window,
            |this, _, event: &SessionsEvent, window, cx| match event {
                SessionsEvent::Close => this.close_overlay(window, cx),
                SessionsEvent::NewSession => {
                    this.close_overlay(window, cx);
                    if let Some(tab) = this.active_session() {
                        tab.update(cx, |s, cx| s.new_session(window, cx));
                    }
                }
                SessionsEvent::Open { path, cwd } => {
                    this.close_overlay(window, cx);
                    let path = path.clone();
                    if *cwd == this.active_cwd(cx) {
                        if let Some(tab) = this.active_session() {
                            tab.update(cx, |s, cx| s.open_session(path, cx));
                        }
                    } else {
                        this.open_tab(PaneKind::Agent, cwd.clone(), None, Some(path), window, cx);
                    }
                }
            },
        );
        self.overlay = Some((Overlay::Sessions(view), subscription));
        cx.notify();
    }

    pub(super) fn open_profiles(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_profiles_on(None, window, cx);
    }

    /// Which machine a screen about one machine (Profiles, Usage) shows:
    /// `machine` (Configuration's "Settings for", the screen's own picker),
    /// else the shown workspace's; and the machines it can switch to.
    fn shown_machine(
        &self,
        machine: Option<gentle_core::backend::Backend>,
    ) -> gentle_core::machines::Shown {
        gentle_core::machines::Shown::new(machine, self.space().machine(), self.machines())
    }

    /// The agent tab a screen about `machine` asks (`remote::ask_tab`): the
    /// active one when it runs there, else another tab there (started if it
    /// wasn't), else the active one, which the screen's control session
    /// only asks on its own machine.
    fn session_on(
        &mut self,
        machine: &gentle_core::backend::Backend,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<crate::features::session::SessionView>> {
        self.start_active_session(window, cx);
        let active = self.active_session();
        let tabs: Vec<_> = active.iter().cloned().chain(self.sessions()).collect();
        let runs_on: Vec<_> = tabs.iter().map(|tab| tab.read(cx).runs_on()).collect();
        let Some(index) = crate::features::settings::remote::ask_tab(machine, &runs_on) else {
            return active;
        };
        let tab = tabs[index].clone();
        tab.update(cx, |s, cx| s.ensure_started(window, cx));
        Some(tab)
    }

    /// Profiles of `machine` (Configuration's "Settings for", the screen's
    /// picker; `None`: the shown workspace's).
    fn open_profiles_on(
        &mut self,
        machine: Option<gentle_core::backend::Backend>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let shown = self.shown_machine(machine);
        let cwd = self.folder_on(&shown.shows, cx);
        let session = self.session_on(&shown.shows, window, cx);
        let view = cx.new(|cx| ProfilesView::new(shown, cwd, session, window, cx));
        let subscription = cx.subscribe_in(&view, window, |this, _, event: &ProfilesEvent, window, cx| {
            let tab = this.active_session();
            match event {
                ProfilesEvent::Close => this.close_overlay(window, cx),
                ProfilesEvent::Changed => {}
                ProfilesEvent::MachineChanged(machine) => {
                    this.open_profiles_on(Some(machine.clone()), window, cx)
                }
                ProfilesEvent::RunCommand(text) => {
                    if let Some(tab) = tab {
                        let text = text.clone();
                        tab.update(cx, |s, cx| s.run_prompt(text, cx));
                    }
                }
                ProfilesEvent::SwitchOrchestrator { model, thinking } => {
                    if let Some(tab) = tab {
                        let ((provider, id), thinking) = (model.clone(), thinking.clone());
                        tab.update(cx, |s, cx| {
                            s.send_command(serde_json::json!({"type": "set_model", "provider": provider, "modelId": id}), cx);
                            if let Some(level) = thinking {
                                s.send_command(serde_json::json!({"type": "set_thinking_level", "level": level}), cx);
                            }
                        });
                    }
                }
                ProfilesEvent::UseInSession { profile, model, thinking } => {
                    if let Some(tab) = tab {
                        let (profile, model, thinking) = (profile.clone(), model.clone(), thinking.clone());
                        let note = format!("Profile \u{201c}{profile}\u{201d} applied to this session's orchestrator; helpers keep the routing this session started with.");
                        tab.update(cx, |s, cx| {
                            s.note(note, cx);
                            s.set_session_profile(Some(profile), cx);
                            if let Some((provider, id)) = model {
                                s.send_command(serde_json::json!({"type": "set_model", "provider": provider, "modelId": id}), cx);
                            }
                            if let Some(level) = thinking {
                                s.send_command(serde_json::json!({"type": "set_thinking_level", "level": level}), cx);
                            }
                        });
                    }
                }
            }
        });
        self.overlay = Some((Overlay::Profiles(view), subscription));
        cx.notify();
    }

    /// Configuration on its Updates page, for the title-bar update pill.
    pub(super) fn open_app_updates(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_settings(window, cx);
        if let Some((Overlay::Settings(view), _)) = &self.overlay {
            view.update(cx, |view, cx| view.show_updates(cx));
        }
    }

    /// Configuration about the shown workspace's machine; its update status
    /// is asked of the window's active agent tab when it runs there and its
    /// Gentle Shell reports it.
    pub(super) fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_settings_on(None, window, cx);
    }

    /// Configuration about `machine` ("Settings for"; `None`: the shown
    /// workspace's), every page read afresh.
    fn open_settings_on(
        &mut self,
        machine: Option<gentle_core::backend::Backend>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.start_active_session(window, cx);
        let active = self.space().machine();
        let machines = self.machines();
        let machine = machine
            .unwrap_or_else(|| machines[gentle_core::machines::pick(&active, &machines)].clone());
        let cwd = self.folder_on(&machine, cx);
        let session = self.active_session();
        let tab_layout = self.tab_layout;
        let view =
            cx.new(|cx| SettingsView::new(machine, machines, cwd, tab_layout, session, window, cx));
        let subscription = cx.subscribe_in(
            &view,
            window,
            |this, view, event: &SettingsEvent, window, cx| match event {
                SettingsEvent::Close => {
                    this.close_overlay(window, cx);
                    // Signed in on a server from the welcome screen (see
                    // `open_providers`): back to its sequence.
                    if let Some(view) = this.resume_welcome.take() {
                        this.attach_welcome(view, window, cx);
                    }
                }
                SettingsEvent::TabLayoutChanged(layout) => this.set_tab_layout(*layout, cx),
                SettingsEvent::OpenProfiles => {
                    let machine = view.read(cx).machine().clone();
                    this.open_profiles_on(Some(machine), window, cx)
                }
                SettingsEvent::MachineChanged(machine) => {
                    let previous = view.clone();
                    this.open_settings_on(Some(machine.clone()), window, cx);
                    if let Some((Overlay::Settings(next), _)) = &this.overlay {
                        next.update(cx, |next, cx| next.keep_page(&previous, cx));
                    }
                }
                SettingsEvent::Paired(server) => this.server_paired(server, window, cx),
                SettingsEvent::OpenTower(machine) => {
                    this.close_overlay(window, cx);
                    // In a workspace on that machine: the tab reaches its
                    // Tower from there.
                    if !gentle_core::machines::same(&this.space().machine(), machine) {
                        let folder = this.folder_on(machine, cx);
                        this.show_folder(folder, machine, window, cx);
                    }
                    this.open_tower(window, cx);
                }
                SettingsEvent::Changed => {
                    for tab in this.sessions() {
                        tab.update(cx, |session, cx| session.reload_models(window, cx));
                    }
                }
                SettingsEvent::CredentialsChanged => {
                    // Only a server's tabs share its credentials.
                    for tab in this.sessions() {
                        if tab.read(cx).runs_on().is_server() {
                            tab.update(cx, |session, cx| session.reload_credentials(window, cx));
                        }
                    }
                }
                SettingsEvent::DevicesChanged(prefs) => {
                    let prefs = *prefs;
                    crate::features::devices::hub(cx)
                        .update(cx, |hub, cx| hub.set_prefs(prefs, cx));
                    pi_rpc::set_system_prompt(crate::features::devices::agent_instructions(prefs));
                    if !prefs.enabled {
                        this.devices.update(cx, |panel, cx| panel.hide(cx));
                    }
                    cx.notify();
                }
                SettingsEvent::MusicChanged => {
                    this.music.update(cx, |music, cx| music.refresh(cx));
                }
                SettingsEvent::BackendUpdated => {
                    for tab in this.sessions() {
                        tab.update(cx, |session, cx| session.restart_for_update(window, cx));
                    }
                }
                SettingsEvent::Setup(event) => {
                    let usable = view.read(cx).setup_usable(cx);
                    this.on_setup_event(event, usable, window, cx);
                }
                SettingsEvent::ServerUnpaired {
                    server,
                    addresses,
                    name,
                    revoked,
                } => this.server_unpaired(server, addresses, name, *revoked, cx),
                SettingsEvent::ShellCommand(text) => {
                    // The session Configuration reads from: the active tab,
                    // or on a server, its control session there.
                    let session = view.read(cx).control_session();
                    let view = view.downgrade();
                    let report = move |result: Result<(), String>, cx: &mut App| {
                        let _ = view.update(cx, |view, cx| view.shell_done(result, cx));
                    };
                    match session {
                        Some(session) => session.update(cx, |session, cx| {
                            session.run_extension_command(text.clone(), cx, report)
                        }),
                        None => report(
                            Err(
                                "Open an agent tab first: Gentle Shell applies this setting."
                                    .into(),
                            ),
                            cx,
                        ),
                    }
                }
            },
        );
        self.overlay = Some((Overlay::Settings(view), subscription));
        cx.notify();
    }

    /// Where an open agent tab found the docs skill, if any did.
    pub(super) fn docs_skill_dir(&self, cx: &App) -> Option<PathBuf> {
        self.active_session()
            .into_iter()
            .chain(self.sessions())
            .find_map(|s| s.read(cx).skill_dir(crate::features::docs::SKILL))
    }

    pub(super) fn open_docs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let root = self.space().root.clone();
        let skill = self.docs_skill_dir(cx);
        let view = cx.new(|cx| DocsView::new(root.clone(), skill, window, cx));
        let subscription = cx.subscribe_in(
            &view,
            window,
            move |this, _, event: &DocsEvent, window, cx| match event {
                DocsEvent::Close => this.close_overlay(window, cx),
                DocsEvent::Run(prompt) => {
                    this.close_overlay(window, cx);
                    this.open_tab(
                        PaneKind::Agent,
                        root.clone(),
                        Some(prompt.clone()),
                        None,
                        window,
                        cx,
                    );
                }
                DocsEvent::Open(path) => {
                    this.close_overlay(window, cx);
                    this.open_file(path.clone(), window, cx);
                }
            },
        );
        self.overlay = Some((Overlay::Docs(view), subscription));
        cx.notify();
    }

    /// The Memory screen for `cwd`'s project, or on `target`'s project and
    /// memory (a session's Engram status).
    pub(super) fn open_memory(
        &mut self,
        cwd: PathBuf,
        target: gentle_core::engram::MemoryTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = cx.new(|cx| MemoryView::new(cwd, target, window, cx));
        let subscription = cx.subscribe_in(
            &view,
            window,
            |this, _, event: &MemoryEvent, window, cx| match event {
                MemoryEvent::Close => this.close_overlay(window, cx),
            },
        );
        self.overlay = Some((Overlay::Memory(view), subscription));
        cx.notify();
    }

    pub(super) fn open_usage(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_usage_on(None, window, cx);
    }

    /// Usage of `machine` (the screen's picker; `None`: the shown
    /// workspace's).
    fn open_usage_on(
        &mut self,
        machine: Option<gentle_core::backend::Backend>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The Subscription tab reads through an agent's RPC slice on the
        // machine shown, so its backend must be started (probe included)
        // before it is opened. On a Gentle Shell server without a tab there,
        // the screen starts its own control session there (see
        // `settings::remote`).
        let shown = self.shown_machine(machine);
        let cwd = self.folder_on(&shown.shows, cx);
        let session = self.session_on(&shown.shows, window, cx);
        let view = cx.new(|cx| UsageView::new(shown, &cwd, window, cx, session));
        let subscription = cx.subscribe_in(
            &view,
            window,
            |this, view, event: &UsageEvent, window, cx| match event {
                UsageEvent::Close => this.close_overlay(window, cx),
                UsageEvent::MachineChanged(machine) => {
                    let previous = view.clone();
                    this.open_usage_on(Some(machine.clone()), window, cx);
                    if let Some((Overlay::Usage(next), _)) = &this.overlay {
                        next.update(cx, |next, cx| next.keep_tab(&previous, cx));
                    }
                }
            },
        );
        self.overlay = Some((Overlay::Usage(view), subscription));
        cx.notify();
    }

    /// Providers: on a Gentle Shell server, its sign-ins are Configuration's
    /// (Models › Sign-ins, over Desktop RPC), so that opens instead.
    pub(super) fn open_providers(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if crate::features::backend::on_server(&self.active_cwd(cx)) {
            self.open_settings(window, cx);
            if let Some((Overlay::Settings(view), _)) = &self.overlay {
                view.update(cx, |view, cx| view.show_models(cx));
            }
            return;
        }
        let cwd = self.active_cwd(cx);
        let view = cx.new(|cx| ProvidersView::new(cwd, window, cx));
        let subscription = cx.subscribe_in(
            &view,
            window,
            |this, _, event: &ProvidersEvent, window, cx| match event {
                ProvidersEvent::Close => {
                    this.close_overlay(window, cx);
                    // Signed in from the welcome screen: back to its sequence.
                    if let Some(view) = this.resume_welcome.take() {
                        this.attach_welcome(view, window, cx);
                    }
                }
                ProvidersEvent::Changed => {
                    for tab in this.sessions() {
                        tab.update(cx, |session, cx| session.reload_credentials(window, cx));
                    }
                }
                ProvidersEvent::Terminal(cwd, command) => {
                    this.close_overlay(window, cx);
                    this.open_setup_terminal(cwd.clone(), command.clone(), window, cx);
                }
            },
        );
        self.overlay = Some((Overlay::Providers(view), subscription));
        cx.notify();
    }

    /// The onboarding screen; `probe` is a look at the machine already taken.
    pub(super) fn open_onboarding(
        &mut self,
        probe: Option<Probe>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = cx.new(|cx| OnboardingView::new(probe, window, cx));
        let subscription = cx.subscribe_in(
            &view,
            window,
            |this, view, event: &OnboardingEvent, window, cx| {
                let usable = view.read(cx).usable();
                this.on_setup_event(event, usable, window, cx);
            },
        );
        self.overlay = Some((Overlay::Onboarding(view), subscription));
        cx.notify();
    }

    /// The first-run welcome screen; `probe` is a look at the machine already
    /// taken. ⌘K → "Welcome screen" shows it again.
    pub(super) fn open_welcome(
        &mut self,
        probe: Option<Probe>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.resume_welcome = None;
        let view = cx.new(|cx| WelcomeView::new(probe, window, cx));
        self.attach_welcome(view, window, cx);
    }

    /// Show `view` (new, or back from Providers) and route its events.
    pub(super) fn attach_welcome(
        &mut self,
        view: Entity<WelcomeView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let subscription = cx.subscribe_in(
            &view,
            window,
            |this, view, event: &WelcomeEvent, window, cx| match event {
                WelcomeEvent::Close => this.close_overlay(window, cx),
                // Providers takes the window; closing it returns to the welcome.
                WelcomeEvent::Setup(OnboardingEvent::SignIn) => {
                    this.resume_welcome = Some(view.clone());
                    this.close_overlay(window, cx);
                    this.open_providers(window, cx);
                }
                WelcomeEvent::Setup(event) => {
                    let usable = view.read(cx).usable(cx);
                    this.on_setup_event(event, usable, window, cx);
                }
            },
        );
        view.read(cx).focus_handle().clone().focus(window, cx);
        self.overlay = Some((Overlay::Welcome(view), subscription));
        cx.notify();
    }

    /// An action from the setup checklist, on the onboarding screen or in
    /// Configuration (which is closed first, like the screen, when the action
    /// moves elsewhere). `usable`: gentle-shell can start as of its last look.
    pub(super) fn on_setup_event(
        &mut self,
        event: &OnboardingEvent,
        usable: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            OnboardingEvent::Close => self.close_overlay(window, cx),
            OnboardingEvent::Installed => {
                if usable {
                    self.retry_failed_sessions(window, cx);
                }
            }
            OnboardingEvent::SignIn => {
                self.close_overlay(window, cx);
                self.open_providers(window, cx);
            }
            OnboardingEvent::Terminal(command) => {
                self.close_overlay(window, cx);
                let home = dirs::home_dir().unwrap_or_else(|| self.active_cwd(cx));
                self.open_setup_terminal(home, command.clone(), window, cx);
            }
            OnboardingEvent::RunsOnChanged => self.runs_on_changed(window, cx),
        }
    }

    /// Onboarding changed where new workspaces go. A Gentle Shell server
    /// just paired: the tabs and terminals on it that this device's earlier
    /// pairing reached follow their sessions through the new one, and its
    /// home folder opens as a workspace of its own (named after that
    /// folder, with an agent tab when new), beside this computer's, which
    /// stay where they are. This computer chosen: the agent tabs that
    /// never came up start again where they are.
    fn runs_on_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let choice = crate::features::backend::default_machine();
        let gentle_core::backend::Backend::Server { url, .. } = &choice else {
            return self.retry_failed_sessions(window, cx);
        };
        if let Some(server) = crate::features::backend::server::key(&choice) {
            self.follow_server(&server, None, window, cx);
        }
        let Some(home) = crate::features::backend::server::known_home(url) else {
            return self.retry_failed_sessions(window, cx);
        };
        let home = PathBuf::from(home);
        self.show_folder(home.clone(), &choice, window, cx);
        if self.space().tabs.is_empty() {
            self.open_tab(PaneKind::Agent, home, None, None, window, cx);
        }
        cx.notify();
    }

    /// Where a screen about `machine` opens: the shown workspace's folder
    /// when it is that machine's, else the folder of a workspace on it,
    /// else its home.
    pub(super) fn folder_on(&self, machine: &gentle_core::backend::Backend, cx: &App) -> PathBuf {
        if gentle_core::machines::same(&self.space().machine(), machine) {
            return self.active_cwd(cx);
        }
        self.spaces
            .iter()
            .find(|space| gentle_core::machines::same(&space.machine(), machine))
            .map(|space| space.root.clone())
            .unwrap_or_else(|| self.browse_from(machine, cx))
    }

    /// Configuration paired this device with `server`: it can hold
    /// workspaces now, and the tabs and terminals that let go of it (a
    /// server paired again) follow it.
    fn server_paired(
        &mut self,
        server: &gentle_core::backend::Backend,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(key) = crate::features::backend::server::key(server) {
            self.follow_server(&key, None, window, cx);
        }
        cx.notify();
    }

    /// A terminal tab in `cwd` (where its agents run) with `command` typed at
    /// the prompt for the user to run (it may ask for a password); the
    /// clipboard holds it when the terminal can't take it.
    pub(super) fn open_setup_terminal(
        &mut self,
        cwd: PathBuf,
        command: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(pane) = self.new_pane(PaneKind::Terminal, cwd, None, None, window, cx) else {
            cx.write_to_clipboard(ClipboardItem::new_string(command));
            window.push_notification(
                Notification::info("The setup command is on your clipboard: run it in a terminal."),
                cx,
            );
            return;
        };
        self.push_pane_tab(pane, window, cx);
        let terminal = self.panes.get(&pane).and_then(|p| {
            p.item
                .view
                .clone()
                .downcast::<crate::features::terminal::TerminalView>()
                .ok()
        });
        let note = match terminal {
            Some(terminal) => {
                terminal.update(cx, |terminal, cx| terminal.type_when_ready(command, cx));
                "The command is typed in this terminal: press Enter to run it. Reopen Setup & requirements from the command palette when it's done."
            }
            None => {
                cx.write_to_clipboard(ClipboardItem::new_string(command));
                "The setup command is on your clipboard: paste it here and press Enter. Reopen Setup & requirements from the command palette when it's done."
            }
        };
        window.push_notification(Notification::info(note), cx);
    }

    /// Start again every agent tab whose backend could not start (onboarding
    /// just made gentle-shell available).
    pub(super) fn retry_failed_sessions(&self, window: &mut Window, cx: &mut Context<Self>) {
        for tab in self.sessions() {
            tab.update(cx, |session, cx| session.retry_start(window, cx));
        }
    }

    /// A terminal pane (`except`) paired its server again: the other
    /// agents and terminals on that server follow it too.
    pub(super) fn follow_pairing(
        &mut self,
        except: Option<PaneId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let server = except
            .and_then(|id| self.terminal_view(id))
            .and_then(|terminal| terminal.read(cx).server_key());
        if let Some(server) = server {
            self.follow_server(&server, except, window, cx);
        }
    }

    /// The Gentle Shell server `server` names was paired again: the agents
    /// and terminals on it that this device's earlier pairing reached
    /// follow their sessions through the new one (`except`: a terminal
    /// pane that did that itself).
    fn follow_server(
        &mut self,
        server: &str,
        except: Option<PaneId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Its workspaces reach it at its new pairing from now on.
        self.publish_machines();
        let (agents, _) = self.agents_and_terminals(cx);
        for agent in agents {
            agent.update(cx, |agent, cx| agent.follow_pairing(server, window, cx));
        }
        let terminals: Vec<PaneId> = self.panes.keys().copied().collect();
        for id in terminals.into_iter().filter(|id| Some(*id) != except) {
            if let Some(terminal) = self.terminal_view(id) {
                terminal.update(cx, |terminal, cx| {
                    terminal.follow_pairing(server, window, cx)
                });
            }
        }
    }

    /// Every agent open in this window (tabs and canvases' agents), and
    /// every terminal.
    fn agents_and_terminals(
        &self,
        cx: &App,
    ) -> (
        Vec<Entity<SessionView>>,
        Vec<Entity<crate::features::terminal::TerminalView>>,
    ) {
        let mut agents = Vec::new();
        let mut terminals = Vec::new();
        for (id, pane) in &self.panes {
            let agent = pane
                .session
                .clone()
                .or_else(|| self.canvas_view(*id)?.read(cx).agent_session());
            agents.extend(agent);
            terminals.extend(self.terminal_view(*id));
        }
        (agents, terminals)
    }

    /// This device was unpaired from `server` (named `name`, paired or
    /// reached at `addresses`; `revoked`: the server removed it): every
    /// agent and terminal on it lets go of it, its token no longer used.
    /// Its sessions keep running there.
    fn server_unpaired(
        &mut self,
        server: &str,
        addresses: &[String],
        name: &str,
        revoked: bool,
        cx: &mut Context<Self>,
    ) {
        let (agents, terminals) = self.agents_and_terminals(cx);
        for agent in agents {
            agent.update(cx, |agent, cx| {
                agent.server_unpaired(server, addresses, name, cx)
            });
        }
        for terminal in terminals {
            terminal.update(cx, |terminal, cx| {
                terminal.server_unpaired(name, revoked, cx)
            });
        }
        self.schedule_save(cx);
        cx.notify();
    }
}
