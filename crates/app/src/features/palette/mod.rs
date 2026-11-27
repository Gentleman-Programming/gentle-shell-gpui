//! The switcher (⌘K), after Tern's: one search field over segmented
//! categories — the app's screens and pane commands plus every Gentle Shell
//! slash command the backend reports, the window's workspaces (switch,
//! rename, delete, create), files under the workspace root and git branches.

use std::path::PathBuf;

use gpui_kit::assets::IconName;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::*;

use gentle_core::commands::Screen;
use gentle_core::fuzzy::{FileEntry, Needle};

use crate::features::workspaces::TabLayout;

mod commands;
mod keyboard;
mod rows;
mod sources;
mod view;

#[cfg(test)]
mod tests;

use commands::{apply_binding_hints, command_row, screen_rows};
use sources::list_branches;
pub use sources::switch_branch;
pub(crate) use sources::walk_files;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaletteAction {
    Providers,
    Profiles,
    Sessions,
    Usage,
    Memory,
    Docs,
    History,
    Devices,
    Settings,
    /// Create (if missing) and open the user's `keymap.json`.
    OpenKeymap,
    /// Re-read `keymap.json` and rebind every shortcut.
    ReloadKeymap,
    Shortcuts,
    /// The onboarding screen: dependency checks and installs.
    Setup,
    /// The first-run welcome screen, from its intro.
    Welcome,
    /// Put the `gsd` command on the user's PATH.
    InstallGsd,
    /// The folder explorer: pick a folder and open Gentle Shell there.
    OpenExplorer,
    Agents,
    NewSession,
    /// A new tab with an agent pane.
    NewTab,
    NewTerminalTab,
    NewCanvasTab,
    /// A new pane of the focused kind, tiled Hyprland-style.
    TilePane,
    SplitRight,
    SplitDown,
    /// Split the focused pane with an agent / terminal beside it.
    NewAgentPane,
    NewTerminalPane,
    ClosePane,
    ReopenClosed,
    ToggleSidebar,
    FocusComposer,
    FocusFiles,
    FocusChanges,
    NextTab,
    PrevTab,
    FocusNextPane,
    FocusPrevPane,
    FocusPaneLeft,
    FocusPaneRight,
    FocusPaneUp,
    FocusPaneDown,
    ResizePaneLeft,
    ResizePaneRight,
    ResizePaneUp,
    ResizePaneDown,
    SplitLeft,
    SplitUp,
    TogglePaneMode,
    ActivateTab(usize),
    SetTabLayout(TabLayout),
    /// Send `/name` as a prompt in the active agent pane.
    Command(String),
    /// A Gentle Shell command whose UI is terminal-only, opened natively.
    Screen(Screen),
    GoToTab,
    ActivateGlobalTab(usize),
    OpenWorkspaces,
    OpenDeleteWorkspace,
    CreateWorkspaceFromFolder,
    SwitchWorkspace(usize),
    RenameWorkspace {
        index: usize,
        name: String,
    },
    DeleteWorkspace(usize),
    /// A new workspace named `name`, rooted at the current folder.
    CreateWorkspace {
        name: String,
    },
    OpenFile(PathBuf),
    /// Asks for confirmation before running `git switch`.
    SwitchBranch(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaletteEvent {
    Run(PaletteAction),
    Close,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    All,
    Commands,
    Workspaces,
    Tabs,
    Files,
    Branches,
}

const CATEGORIES: [(Category, &str); 6] = [
    (Category::All, "All"),
    (Category::Commands, "Commands"),
    (Category::Workspaces, "Workspaces"),
    (Category::Tabs, "Tabs"),
    (Category::Files, "Files"),
    (Category::Branches, "Branches"),
];

impl Category {
    fn placeholder(self) -> &'static str {
        match self {
            Category::All => "Search commands, workspaces, files…",
            Category::Commands => "Type a command or screen…",
            Category::Workspaces => "Find or create a workspace…",
            Category::Tabs => "Find a tab by title or workspace…",
            Category::Files => "Find a file…",
            Category::Branches => "Find a branch…",
        }
    }
}

/// A workspace as the switcher lists it.
#[derive(Clone, Debug)]
pub struct WorkspaceRow {
    /// Index in the window's workspace list.
    pub index: usize,
    pub name: String,
    pub tabs: usize,
    pub current: bool,
    pub activity: Activity,
}

/// What a workspace's agents are doing: how many work and how many wait on
/// you (an agent that waits isn't counted as working).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Activity {
    pub working: usize,
    pub needs_you: usize,
}

pub struct TabRow {
    pub id: usize,
    pub title: String,
    pub workspace: String,
    pub icon: IconName,
    pub current: bool,
}

pub struct PaletteInput {
    pub tabs: Vec<TabRow>,
    /// Slash commands of the active agent pane.
    pub commands: Vec<gentle_core::commands::SlashCommand>,
    pub workspaces: Vec<WorkspaceRow>,
    /// The current workspace's root, for files and branches.
    pub root: PathBuf,
    pub category: Category,
    /// Current window tab placement; determines the available view command.
    pub tab_layout: TabLayout,
}

#[derive(Clone)]
struct Row {
    icon: IconName,
    title: String,
    meta: String,
    detail: String,
    group: &'static str,
    action: PaletteAction,
    /// Workspace rows get rename/delete buttons.
    workspace: Option<usize>,
    current: bool,
}

enum Line {
    Header(&'static str),
    Row(usize),
}

pub(crate) const MAX_FILES: usize = 20_000;

pub struct PaletteView {
    pub previous_focus: Option<FocusHandle>,
    tabs: Vec<TabRow>,
    query: Entity<InputState>,
    category: Category,
    selected: usize,
    commands: Vec<Row>,
    workspaces: Vec<WorkspaceRow>,
    root: PathBuf,
    files: Option<Vec<FileEntry>>,
    branches: Option<Vec<(String, bool)>>,
    renaming: Option<(usize, Entity<InputState>)>,
    /// Choosing a workspace to delete: Enter confirms instead of switching.
    deleting: bool,
    /// The query as last matched.
    needle: Needle,
    /// Matched rows and their display lines, recomputed by
    /// [`Self::refresh_rows`] when the query, category or sources change.
    rows: Vec<Row>,
    lines: Vec<Line>,
    scroll: ScrollHandle,
    _subscriptions: Vec<Subscription>,
    _tasks: Vec<Task<()>>,
}

impl EventEmitter<PaletteEvent> for PaletteView {}

impl PaletteView {
    pub fn new(input: PaletteInput, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let previous_focus = window.focused(cx);
        let query =
            cx.new(|cx| InputState::new(window, cx).placeholder(input.category.placeholder()));
        query.update(cx, |state, cx| state.focus(window, cx));
        let subscription = cx.subscribe(&query, |this, _, event: &InputEvent, cx| {
            if let InputEvent::Change = event {
                this.selected = 0;
                this.scroll.scroll_to_item(0);
                this.refresh_rows(cx);
                cx.notify();
            }
        });
        let mut commands = screen_rows(input.tab_layout);
        apply_binding_hints(&mut commands, window, cx);
        commands.extend(input.commands.into_iter().filter_map(command_row));

        let root = input.root.clone();
        let files = cx.spawn(async move |this, cx| {
            let files = cx
                .background_executor()
                .spawn(async move {
                    walk_files(&root, MAX_FILES)
                        .into_iter()
                        .map(FileEntry::new)
                        .collect::<Vec<_>>()
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.files = Some(files);
                this.refresh_rows(cx);
                cx.notify();
            });
        });
        let root = input.root.clone();
        let branches = cx.spawn(async move |this, cx| {
            let branches = cx
                .background_executor()
                .spawn(async move { list_branches(&root) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.branches = Some(branches);
                this.refresh_rows(cx);
                cx.notify();
            });
        });

        let mut this = Self {
            previous_focus,
            tabs: input.tabs,
            query,
            category: input.category,
            selected: 0,
            commands,
            workspaces: input.workspaces,
            root: input.root,
            files: None,
            branches: None,
            renaming: None,
            deleting: false,
            needle: Needle::default(),
            rows: Vec::new(),
            lines: Vec::new(),
            scroll: ScrollHandle::new(),
            _subscriptions: vec![subscription],
            _tasks: vec![files, branches],
        };
        this.refresh_rows(cx);
        this
    }

    pub fn category(&self) -> Category {
        self.category
    }

    pub fn set_category(
        &mut self,
        category: Category,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.category = category;
        if category != Category::Workspaces {
            self.deleting = false;
        }
        self.selected = 0;
        self.renaming = None;
        self.scroll.scroll_to_item(0);
        self.query.update(cx, |state, cx| {
            if category == Category::Tabs {
                state.set_value("", window, cx);
            }
            state.set_placeholder(category.placeholder(), window, cx);
            state.focus(window, cx);
        });
        self.refresh_rows(cx);
        cx.notify();
    }

    /// Enter the workspace picker from its command row, not from a category shortcut.
    pub fn show_workspace_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.deleting = false;
        self.query
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.set_category(Category::Workspaces, window, cx);
    }

    /// Enter the workspace deleter: each row deletes instead of switching.
    pub fn show_workspace_deleter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.deleting = true;
        self.query
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.set_category(Category::Workspaces, window, cx);
        self.query.update(cx, |state, cx| {
            state.set_placeholder("Choose a workspace to delete…", window, cx)
        });
    }

    /// The workspaces changed while the switcher is open (an agent started,
    /// finished or asked something): re-list them in place.
    pub fn set_workspaces(&mut self, workspaces: Vec<WorkspaceRow>, cx: &mut Context<Self>) {
        self.workspaces = workspaces;
        self.refresh_rows(cx);
        cx.notify();
    }
}
