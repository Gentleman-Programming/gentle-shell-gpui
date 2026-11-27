//! Keybinding action handlers.

use super::*;

impl Workspace {
    /// A file pane's preview followed a link to another Markdown file: show
    /// it in that pane, or beside it while the pane has unsaved edits.
    pub(super) fn on_show_in_pane(
        &mut self,
        action: &ShowInPane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self
            .panes
            .iter()
            .find(|(_, pane)| pane.item.view.entity_id() == action.pane)
            .map(|(id, _)| *id)
        else {
            return;
        };
        let unsaved = self.panes[&id]
            .item
            .unsaved
            .as_ref()
            .is_some_and(|probe| probe(cx));
        let Some(factory) = PaneFactories::get(cx).file.filter(|_| !unsaved) else {
            self.open_file_beside(action.path.clone(), window, cx);
            return;
        };
        let item = factory(action.path.clone(), window, cx);
        if let Some(pane) = self.panes.get_mut(&id) {
            pane.item = item;
            pane.cwd = action
                .path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_default();
            pane.path = Some(action.path.clone());
            pane.sync_chrome(cx);
        }
        self.focus_pane(id, window, cx);
        self.schedule_save(cx);
        cx.notify();
    }

    pub(super) fn on_new_tab(&mut self, _: &NewTab, window: &mut Window, cx: &mut Context<Self>) {
        let cwd = self.active_cwd(cx);
        self.open_tab(PaneKind::Agent, cwd, None, None, window, cx);
    }

    pub(super) fn on_new_terminal_tab(
        &mut self,
        _: &NewTerminalTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cwd = self.active_cwd(cx);
        self.open_tab(PaneKind::Terminal, cwd, None, None, window, cx);
    }

    pub(super) fn on_new_canvas_tab(
        &mut self,
        _: &NewCanvasTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cwd = self.active_cwd(cx);
        self.open_tab(PaneKind::Canvas, cwd, None, None, window, cx);
    }

    pub(super) fn on_close_tab(
        &mut self,
        _: &CloseTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let active = self.space().active;
        self.close_tab(active, window, cx);
    }

    pub(super) fn on_next_tab(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        let space = self.space();
        let next = (space.active + 1) % space.tabs.len().max(1);
        self.activate(next, window, cx);
    }

    pub(super) fn on_prev_tab(&mut self, _: &PrevTab, window: &mut Window, cx: &mut Context<Self>) {
        let space = self.space();
        let len = space.tabs.len().max(1);
        let prev = (space.active + len - 1) % len;
        self.activate(prev, window, cx);
    }

    pub(super) fn on_split_right(
        &mut self,
        _: &SplitRight,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.split(SplitAxis::Horizontal, None, window, cx);
    }

    /// ⌘D: a new pane tiled Hyprland-style (dwindle), splitting the focused
    /// pane along its longer side.
    pub(super) fn on_tile_pane(
        &mut self,
        _: &TilePane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.tile(None, window, cx);
    }

    pub(super) fn on_split_down(
        &mut self,
        _: &SplitDown,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.split(SplitAxis::Vertical, None, window, cx);
    }

    pub(super) fn on_new_agent_pane(
        &mut self,
        _: &NewAgentPane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.split_with_kind(PaneKind::Agent, window, cx);
    }

    pub(super) fn on_new_terminal_pane(
        &mut self,
        _: &NewTerminalPane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.split_with_kind(PaneKind::Terminal, window, cx);
    }

    pub(super) fn on_close_pane(
        &mut self,
        _: &ClosePane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(pane) = self.focused_pane() {
            self.close_pane(pane, window, cx);
        }
    }

    pub(super) fn on_reopen_closed(
        &mut self,
        _: &ReopenClosed,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.reopen_closed(window, cx);
    }

    pub(super) fn on_focus_next_pane(
        &mut self,
        _: &FocusNextPane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(next) = self
            .tab()
            .and_then(|t| t.built_tree()?.cycle(&t.focused()?, true))
        {
            self.focus_pane(next, window, cx);
        }
    }

    pub(super) fn on_focus_prev_pane(
        &mut self,
        _: &FocusPrevPane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(prev) = self
            .tab()
            .and_then(|t| t.built_tree()?.cycle(&t.focused()?, false))
        {
            self.focus_pane(prev, window, cx);
        }
    }

    pub(super) fn on_toggle_sidebar(
        &mut self,
        _: &ToggleSidebar,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_sidebar(cx);
    }

    pub fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.files_sidebar_motion
            .retarget(self.sidebar_visible, cx.reduce_motion());
        self.sidebar_visible = !self.sidebar_visible;
        cx.emit(WorkspaceEvent::FilesVisibilityChanged(
            self.files_sidebar_visible(),
        ));
        self.schedule_save(cx);
        cx.notify();
    }

    /// The right Files sidebar polls only while it is visible, regardless
    /// of whether tabs are arranged horizontally or vertically.
    pub fn files_sidebar_visible(&self) -> bool {
        self.sidebar_visible
    }

    pub fn show_sidebar(&mut self, cx: &mut Context<Self>) {
        if !self.sidebar_visible {
            self.sidebar_visible = true;
            cx.emit(WorkspaceEvent::FilesVisibilityChanged(
                self.files_sidebar_visible(),
            ));
            self.schedule_save(cx);
        }
        cx.notify();
    }

    pub(super) fn on_activate_tab1(
        &mut self,
        _: &ActivateTab1,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.activate(0, window, cx);
    }

    pub(super) fn on_activate_tab2(
        &mut self,
        _: &ActivateTab2,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.activate(1, window, cx);
    }

    pub(super) fn on_activate_tab3(
        &mut self,
        _: &ActivateTab3,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.activate(2, window, cx);
    }

    pub(super) fn on_activate_tab4(
        &mut self,
        _: &ActivateTab4,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.activate(3, window, cx);
    }

    pub(super) fn on_activate_tab5(
        &mut self,
        _: &ActivateTab5,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.activate(4, window, cx);
    }

    pub(super) fn on_activate_tab6(
        &mut self,
        _: &ActivateTab6,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.activate(5, window, cx);
    }

    pub(super) fn on_activate_tab7(
        &mut self,
        _: &ActivateTab7,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.activate(6, window, cx);
    }

    pub(super) fn on_activate_tab8(
        &mut self,
        _: &ActivateTab8,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.activate(7, window, cx);
    }

    pub(super) fn on_activate_tab9(
        &mut self,
        _: &ActivateTab9,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let last = self.space().tabs.len().saturating_sub(1);
        self.activate(last.min(8), window, cx);
    }

    pub(super) fn on_focus_composer(
        &mut self,
        _: &FocusComposer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The agent pane's focus handle is its composer; for other pane kinds
        // this still focuses the active pane.
        self.focus_active(window, cx);
    }

    pub(super) fn on_focus_files(
        &mut self,
        _: &FocusFiles,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_sidebar(cx);
        cx.emit(WorkspaceEvent::FocusSidebar(SidebarFocus::Files));
    }

    pub(super) fn on_focus_changes(
        &mut self,
        _: &FocusChanges,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_sidebar(cx);
        cx.emit(WorkspaceEvent::FocusSidebar(SidebarFocus::Changes));
    }

    pub(super) fn on_focus_pane_left(
        &mut self,
        _: &FocusPaneLeft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_neighbour(PaneDirection::Left, window, cx);
    }

    pub(super) fn on_focus_pane_right(
        &mut self,
        _: &FocusPaneRight,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_neighbour(PaneDirection::Right, window, cx);
    }

    pub(super) fn on_focus_pane_up(
        &mut self,
        _: &FocusPaneUp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_neighbour(PaneDirection::Up, window, cx);
    }

    pub(super) fn on_focus_pane_down(
        &mut self,
        _: &FocusPaneDown,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_neighbour(PaneDirection::Down, window, cx);
    }

    pub(super) fn on_resize_pane_left(
        &mut self,
        _: &ResizePaneLeft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.resize_pane(PaneDirection::Left, window, cx);
    }

    pub(super) fn on_resize_pane_right(
        &mut self,
        _: &ResizePaneRight,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.resize_pane(PaneDirection::Right, window, cx);
    }

    pub(super) fn on_resize_pane_up(
        &mut self,
        _: &ResizePaneUp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.resize_pane(PaneDirection::Up, window, cx);
    }

    pub(super) fn on_resize_pane_down(
        &mut self,
        _: &ResizePaneDown,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.resize_pane(PaneDirection::Down, window, cx);
    }

    pub(super) fn on_split_left(
        &mut self,
        _: &SplitLeft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.split_before(SplitAxis::Horizontal, None, window, cx);
    }

    pub(super) fn on_split_up(&mut self, _: &SplitUp, window: &mut Window, cx: &mut Context<Self>) {
        self.split_before(SplitAxis::Vertical, None, window, cx);
    }

    pub(super) fn on_toggle_pane_mode(
        &mut self,
        _: &TogglePaneMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pane_mode = !self.pane_mode;
        if self.pane_mode {
            self.pane_mode_focus.focus(window, cx);
        } else {
            self.focus_active(window, cx);
        }
        cx.notify();
    }

    pub(super) fn on_exit_pane_mode(
        &mut self,
        _: &ExitPaneMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pane_mode {
            self.pane_mode = false;
            self.focus_active(window, cx);
            cx.notify();
        }
    }

    pub(super) fn on_open_providers(
        &mut self,
        _: &OpenProviders,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_providers(window, cx);
    }

    /// The file view's header ✕ asks to close the pane showing it.
    pub(super) fn on_close_viewer(
        &mut self,
        action: &CloseViewer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self
            .panes
            .iter()
            .find(|(_, pane)| pane.item.view.entity_id() == action.pane)
            .map(|(id, _)| *id)
        else {
            return;
        };
        self.close_pane(id, window, cx);
    }

    /// The Git pane's file row: what a commit changed in that file, placed
    /// like a preview.
    pub(super) fn on_open_commit_diff(
        &mut self,
        action: &OpenCommitDiff,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(factory) = PaneFactories::get(cx).commit_diff else {
            return;
        };
        let item = factory(action, window, cx);
        let id = self.file_pane(item, action.root.join(&action.path), cx);
        self.place_preview(id, window, cx);
    }

    /// A new agent tab in `cwd` that starts with `prompt` (Explain with the
    /// agent, in the Git pane).
    pub(super) fn on_ask_agent(
        &mut self,
        action: &AskAgent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_tab(
            PaneKind::Agent,
            action.cwd.clone(),
            Some(action.prompt.clone()),
            None,
            window,
            cx,
        );
    }

    /// A terminal beside the focused pane, in `cwd`.
    pub(super) fn on_open_terminal_at(
        &mut self,
        action: &OpenTerminalAt,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.tile(Some((PaneKind::Terminal, action.cwd.clone())), window, cx);
    }

    pub(super) fn on_close_overlay(
        &mut self,
        _: &CloseOverlay,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match &self.overlay {
            // An install is running: the screen stays until it ends.
            Some((Overlay::Onboarding(view), _)) if view.read(cx).busy() => {}
            Some((Overlay::Welcome(view), _)) if view.read(cx).busy(cx) => {}
            Some(_) => self.close_overlay(window, cx),
            None => cx.propagate(),
        }
    }

    pub(super) fn close_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let restore = match &self.overlay {
            Some((Overlay::Palette(view), _)) if view.read(cx).category() == Category::Tabs => {
                let previous = &view.read(cx).previous_focus;
                // Opening the chooser exits pane mode on render. Its former
                // focus handle is no longer mounted, so return to the pane.
                if !self.pane_mode && previous.as_ref() == Some(&self.pane_mode_focus) {
                    None
                } else {
                    Some(previous.clone())
                }
            }
            _ => None,
        };
        let retry = match &self.overlay {
            Some((Overlay::Onboarding(view), _)) => view.read(cx).usable(),
            Some((Overlay::Welcome(view), _)) => view.read(cx).usable(cx),
            _ => false,
        };
        self.overlay = None;
        if retry {
            self.retry_failed_sessions(window, cx);
        }
        match restore {
            Some(Some(focus)) => focus.focus(window, cx),
            Some(None) => window.blur(cx),
            None => self.focus_active(window, cx),
        }
        cx.notify();
    }

    pub(super) fn on_go_to_tab(
        &mut self,
        _: &GoToTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_palette(Category::Tabs, window, cx);
    }

    pub(super) fn on_focus_tab_list(
        &mut self,
        _: &FocusTabList,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_tab_list(window, cx);
    }

    /// Leader `a`, palette and menus: choose the folder in the app's explorer
    /// (the same ⌘O one), whose "Open Gentle Shell here" names the workspace
    /// after that folder and shows it, creating it when it is new.
    pub(super) fn on_create_workspace(
        &mut self,
        _: &CreateWorkspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_explorer(window, cx);
    }

    pub(super) fn on_delete_workspace(
        &mut self,
        _: &DeleteWorkspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_workspace_deleter(window, cx);
    }

    pub(super) fn on_open_sessions(
        &mut self,
        _: &OpenSessions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_sessions(window, cx);
    }

    pub(super) fn on_open_palette(
        &mut self,
        _: &OpenPalette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_palette(Category::All, window, cx);
    }

    /// ⌘, (Ctrl+, elsewhere): Configuration, or closes it when it's open.
    pub(super) fn on_open_settings(
        &mut self,
        _: &OpenSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(self.overlay, Some((Overlay::Settings(_), _))) {
            self.close_overlay(window, cx);
        } else {
            self.open_settings(window, cx);
        }
    }

    /// ⌘/ (Ctrl+Shift+/ elsewhere), ⌃Space ?: the shortcuts screen, or closes
    /// it when it's open.
    pub(super) fn on_open_shortcuts(
        &mut self,
        _: &OpenShortcuts,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(self.overlay, Some((Overlay::Shortcuts(_), _))) {
            self.close_overlay(window, cx);
        } else {
            self.open_shortcuts(window, cx);
        }
    }

    pub(super) fn on_open_explorer(
        &mut self,
        _: &OpenExplorer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_explorer(window, cx);
    }

    pub(super) fn on_open_workspaces(
        &mut self,
        _: &OpenWorkspaces,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_palette(Category::Workspaces, window, cx);
    }
}
