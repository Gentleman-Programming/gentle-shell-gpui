//! The command rows: the app's screens and pane commands, and the rows of
//! the backend's slash commands.

use gpui_kit::assets::IconName;
use gpui_kit::{Action, App, KeyContext, Window};

use gentle_core::commands::{self, SlashCommand};
use gentle_core::keyboard::format_binding;
use gentle_core::shortcuts::{self as shortcuts_core, Binding};

use crate::features::workspace::{
    ActivateTab1, ActivateTab2, ActivateTab3, ActivateTab4, ActivateTab5, ActivateTab6,
    ActivateTab7, ActivateTab8, ActivateTab9, ClosePane, CreateWorkspace, DeleteWorkspace,
    FocusChanges, FocusComposer, FocusFiles, FocusNextPane, FocusPrevPane, NewCanvasTab, NewTab,
    NewTerminalTab, NextTab, PrevTab, ReopenClosed, SplitDown, TilePane, ToggleSidebar,
};
use crate::features::workspace::{
    FocusPaneDown, FocusPaneLeft, FocusPaneRight, FocusPaneUp, ResizePaneDown, ResizePaneLeft,
    ResizePaneRight, ResizePaneUp, SplitLeft, SplitUp, TogglePaneMode,
};

use crate::features::session;
use crate::shared::ui;

use super::{PaletteAction, Row, TabLayout};

pub(super) fn screen_rows(tab_layout: TabLayout) -> Vec<Row> {
    let row = |icon, title: &str, detail: &str, action| Row {
        icon,
        title: title.into(),
        meta: String::new(),
        detail: detail.into(),
        group: "Gentle Shell",
        action,
        workspace: None,
        current: false,
    };
    let mut rows = vec![
        row(
            IconName::Users,
            "Agents",
            "Helpers working for this session",
            PaletteAction::Agents,
        ),
        row(
            IconName::Route,
            "Profiles",
            "Model and effort per agent; profiles as tabs",
            PaletteAction::Profiles,
        ),
        row(
            IconName::RotateCcwClock,
            "Sessions",
            "Resume a previous conversation",
            PaletteAction::Sessions,
        ),
        row(
            IconName::Brain,
            "Memory",
            "Browse and search Engram, the persistent memory",
            PaletteAction::Memory,
        ),
        row(
            IconName::BookOpen,
            "Architecture docs",
            "ADRs, C4, data, states and flows, written and kept current by the agent",
            PaletteAction::Docs,
        ),
        row(
            IconName::GitGraph,
            "Git",
            "Commits as a graph, branches, tags, pull, push, cherry-pick, revert and reset",
            PaletteAction::History,
        ),
        row(
            IconName::Smartphone,
            "Devices",
            "Launch an Android emulator or iOS simulator and watch it",
            PaletteAction::Devices,
        ),
        row(
            IconName::Gauge,
            "Usage & stats",
            "Subscription windows and local token/cost stats",
            PaletteAction::Usage,
        ),
        row(
            IconName::KeyRound,
            "Providers",
            "Sign in with a subscription or an API key",
            PaletteAction::Providers,
        ),
        row(
            IconName::Settings,
            "Settings",
            "Gentle Shell configuration",
            PaletteAction::Settings,
        ),
        row(
            IconName::Settings,
            "Open keymap file",
            "Your shortcuts and the leader key (keymap.json)",
            PaletteAction::OpenKeymap,
        ),
        row(
            IconName::RotateCcwClock,
            "Reload keymap",
            "Apply keymap.json now, without restarting",
            PaletteAction::ReloadKeymap,
        ),
        row(
            IconName::Keyboard,
            "Keyboard shortcuts",
            "Every key, by section, searchable; the leader and pane mode included",
            PaletteAction::Shortcuts,
        ),
        row(
            IconName::CircleCheck,
            "Setup & requirements",
            "Check Node, Gentle Shell, providers and extras; install what's missing",
            PaletteAction::Setup,
        ),
        row(
            IconName::Sparkles,
            "Welcome screen",
            "The first-run screen: the intro, then the install sequence",
            PaletteAction::Welcome,
        ),
        row(
            IconName::Folder,
            "Browse folders",
            &format!(
                "Open Gentle Shell in any folder, oil-style ({})",
                ui::shortcut("O")
            ),
            PaletteAction::OpenExplorer,
        ),
        row(
            IconName::Terminal,
            "Install `gsd` command",
            "Type gsd in any terminal folder to open it here as a workspace",
            PaletteAction::InstallGsd,
        ),
        row(
            IconName::Plus,
            "New session",
            "Start a fresh conversation in this pane",
            PaletteAction::NewSession,
        ),
        row(
            IconName::Bot,
            "New agent tab",
            "Another agent in the same folder",
            PaletteAction::NewTab,
        ),
        row(
            IconName::SquareTerminal,
            "New terminal tab",
            "A shell in the same folder",
            PaletteAction::NewTerminalTab,
        ),
        row(
            IconName::PenTool,
            "New canvas tab",
            "A whiteboard for ink, shapes, notes and images",
            PaletteAction::NewCanvasTab,
        ),
        row(
            IconName::Layers,
            "Go to tab…",
            "Find a tab in any workspace",
            PaletteAction::GoToTab,
        ),
        row(
            IconName::Layers,
            "Switch workspace…",
            "Find a workspace by name",
            PaletteAction::OpenWorkspaces,
        ),
        row(
            IconName::FolderOpen,
            "Create workspace",
            "Choose a folder; the new workspace is named after it",
            PaletteAction::CreateWorkspaceFromFolder,
        ),
        row(
            IconName::Trash,
            "Delete workspace…",
            "Choose a workspace to delete",
            PaletteAction::OpenDeleteWorkspace,
        ),
        row(
            IconName::Building2,
            "Gentle Tower",
            "Every session as a floor of its project's building: tasks, talk, artifacts, cost",
            PaletteAction::Screen(gentle_core::commands::Screen::Tower),
        ),
        row(
            IconName::LayoutDashboard,
            "New pane (tiled)",
            "Same kind of pane, tiled beside the focused one",
            PaletteAction::TilePane,
        ),
        row(
            IconName::Columns2,
            "Split right",
            "Same kind of pane to the right of this one",
            PaletteAction::SplitRight,
        ),
        row(
            IconName::Rows2,
            "Split down",
            "Same kind of pane below this one",
            PaletteAction::SplitDown,
        ),
        row(
            IconName::Bot,
            "New agent pane",
            "Split with an agent beside the focused pane",
            PaletteAction::NewAgentPane,
        ),
        row(
            IconName::SquareTerminal,
            "New terminal pane",
            "Split with a terminal beside the focused pane",
            PaletteAction::NewTerminalPane,
        ),
        row(
            IconName::X,
            "Close pane",
            "The last pane closes its tab",
            PaletteAction::ClosePane,
        ),
        row(
            IconName::RotateCcwClock,
            "Reopen closed pane",
            "The last pane or tab closed, where it was",
            PaletteAction::ReopenClosed,
        ),
        match tab_layout {
            TabLayout::Horizontal => row(
                IconName::PanelLeft,
                "View: Show tabs vertically",
                "Move tabs from the top bar into the left sidebar",
                PaletteAction::SetTabLayout(TabLayout::Vertical),
            ),
            TabLayout::Vertical => row(
                IconName::List,
                "View: Show tabs horizontally",
                "Move tabs from the sidebar back into the top bar",
                PaletteAction::SetTabLayout(TabLayout::Horizontal),
            ),
        },
        row(
            IconName::PanelRight,
            "Toggle Files sidebar",
            "Show or hide Files / Changes / Branches on the right",
            PaletteAction::ToggleSidebar,
        ),
        row(
            IconName::MessageSquare,
            "Focus composer",
            "Focus the active agent pane's composer",
            PaletteAction::FocusComposer,
        ),
        row(
            IconName::Files,
            "Focus Files",
            "Show the Files sidebar and focus the file tree",
            PaletteAction::FocusFiles,
        ),
        row(
            IconName::Diff,
            "Focus Git changes",
            "Show the Files sidebar and focus the changes list",
            PaletteAction::FocusChanges,
        ),
        row(
            IconName::ArrowRight,
            "Next tab",
            "Switch to the next tab in this workspace",
            PaletteAction::NextTab,
        ),
        row(
            IconName::ArrowLeft,
            "Previous tab",
            "Switch to the previous tab in this workspace",
            PaletteAction::PrevTab,
        ),
        row(
            IconName::ArrowDown,
            "Focus next pane",
            "Move focus to the next pane in this tab",
            PaletteAction::FocusNextPane,
        ),
        row(
            IconName::ArrowUp,
            "Focus previous pane",
            "Move focus to the previous pane in this tab",
            PaletteAction::FocusPrevPane,
        ),
        row(
            IconName::ArrowLeft,
            "Focus pane left",
            "Move focus to the pane on the left",
            PaletteAction::FocusPaneLeft,
        ),
        row(
            IconName::ArrowRight,
            "Focus pane right",
            "Move focus to the pane on the right",
            PaletteAction::FocusPaneRight,
        ),
        row(
            IconName::ArrowUp,
            "Focus pane up",
            "Move focus to the pane above",
            PaletteAction::FocusPaneUp,
        ),
        row(
            IconName::ArrowDown,
            "Focus pane down",
            "Move focus to the pane below",
            PaletteAction::FocusPaneDown,
        ),
        row(
            IconName::Columns2,
            "Split left",
            "Same kind of pane to the left of this one",
            PaletteAction::SplitLeft,
        ),
        row(
            IconName::Rows2,
            "Split up",
            "Same kind of pane above this one",
            PaletteAction::SplitUp,
        ),
        row(
            IconName::MoveHorizontal,
            "Resize pane left",
            "Move the focused pane's left edge",
            PaletteAction::ResizePaneLeft,
        ),
        row(
            IconName::MoveHorizontal,
            "Resize pane right",
            "Move the focused pane's right edge",
            PaletteAction::ResizePaneRight,
        ),
        row(
            IconName::MoveVertical,
            "Resize pane up",
            "Move the focused pane's top edge",
            PaletteAction::ResizePaneUp,
        ),
        row(
            IconName::MoveVertical,
            "Resize pane down",
            "Move the focused pane's bottom edge",
            PaletteAction::ResizePaneDown,
        ),
        row(
            IconName::LayoutDashboard,
            "Pane mode",
            "Sticky keys: hjkl move, HJKL resize, v s split, x close, Esc exits",
            PaletteAction::TogglePaneMode,
        ),
    ];
    for n in 1..=9 {
        rows.push(row(
            IconName::Hash,
            &format!("Go to tab {n}"),
            "Switch to a tab by number; 9 is the last tab",
            PaletteAction::ActivateTab(n),
        ));
    }
    rows
}

/// The GPUI action that backs a palette row, if the row maps to a bindable
/// action, and the key context its binding lives in. Used to show live
/// shortcut hints from the current keymap.
pub(super) fn palette_action_to_gpui(action: &PaletteAction) -> Option<Box<dyn Action>> {
    match action {
        PaletteAction::Shortcuts => Some(Box::new(crate::features::workspace::OpenShortcuts)),
        PaletteAction::Agents => Some(Box::new(session::ToggleAgents)),
        PaletteAction::NewSession => Some(Box::new(session::NewSession)),
        PaletteAction::NewTab => Some(Box::new(NewTab)),
        PaletteAction::NewTerminalTab => Some(Box::new(NewTerminalTab)),
        PaletteAction::NewCanvasTab => Some(Box::new(NewCanvasTab)),
        PaletteAction::GoToTab => Some(Box::new(crate::features::workspace::GoToTab)),
        PaletteAction::OpenWorkspaces => Some(Box::new(crate::features::workspace::OpenWorkspaces)),
        PaletteAction::OpenDeleteWorkspace => Some(Box::new(DeleteWorkspace)),
        PaletteAction::CreateWorkspaceFromFolder => Some(Box::new(CreateWorkspace)),
        PaletteAction::ClosePane => Some(Box::new(ClosePane)),
        PaletteAction::ReopenClosed => Some(Box::new(ReopenClosed)),
        PaletteAction::NextTab => Some(Box::new(NextTab)),
        PaletteAction::PrevTab => Some(Box::new(PrevTab)),
        PaletteAction::TilePane => Some(Box::new(TilePane)),
        PaletteAction::SplitDown => Some(Box::new(SplitDown)),
        PaletteAction::FocusNextPane => Some(Box::new(FocusNextPane)),
        PaletteAction::FocusPrevPane => Some(Box::new(FocusPrevPane)),
        PaletteAction::ToggleSidebar => Some(Box::new(ToggleSidebar)),
        PaletteAction::FocusPaneLeft => Some(Box::new(FocusPaneLeft)),
        PaletteAction::FocusPaneRight => Some(Box::new(FocusPaneRight)),
        PaletteAction::FocusPaneUp => Some(Box::new(FocusPaneUp)),
        PaletteAction::FocusPaneDown => Some(Box::new(FocusPaneDown)),
        PaletteAction::SplitLeft => Some(Box::new(SplitLeft)),
        PaletteAction::SplitUp => Some(Box::new(SplitUp)),
        PaletteAction::ResizePaneLeft => Some(Box::new(ResizePaneLeft)),
        PaletteAction::ResizePaneRight => Some(Box::new(ResizePaneRight)),
        PaletteAction::ResizePaneUp => Some(Box::new(ResizePaneUp)),
        PaletteAction::ResizePaneDown => Some(Box::new(ResizePaneDown)),
        PaletteAction::TogglePaneMode => Some(Box::new(TogglePaneMode)),
        PaletteAction::FocusComposer => Some(Box::new(FocusComposer)),
        PaletteAction::FocusFiles => Some(Box::new(FocusFiles)),
        PaletteAction::FocusChanges => Some(Box::new(FocusChanges)),
        PaletteAction::ActivateTab(1) => Some(Box::new(ActivateTab1)),
        PaletteAction::ActivateTab(2) => Some(Box::new(ActivateTab2)),
        PaletteAction::ActivateTab(3) => Some(Box::new(ActivateTab3)),
        PaletteAction::ActivateTab(4) => Some(Box::new(ActivateTab4)),
        PaletteAction::ActivateTab(5) => Some(Box::new(ActivateTab5)),
        PaletteAction::ActivateTab(6) => Some(Box::new(ActivateTab6)),
        PaletteAction::ActivateTab(7) => Some(Box::new(ActivateTab7)),
        PaletteAction::ActivateTab(8) => Some(Box::new(ActivateTab8)),
        PaletteAction::ActivateTab(9) => Some(Box::new(ActivateTab9)),
        _ => None,
    }
}

/// Append live shortcut hints from the current keymap to row descriptions.
/// Rows whose action has no binding keep their description unchanged.
pub(super) fn apply_binding_hints(rows: &mut [Row], window: &mut Window, cx: &App) {
    let macos = cfg!(target_os = "macos");
    let live: Vec<Binding> = crate::features::shortcuts::live_bindings(cx);
    for row in rows {
        let Some(action) = palette_action_to_gpui(&row.action) else {
            continue;
        };
        // Look bindings up in a fixed context, not wherever focus was when
        // the switcher opened: session bindings only match inside an agent pane.
        let context = match row.action {
            PaletteAction::Agents | PaletteAction::NewSession => session::CONTEXT,
            _ => "GentleWorkspace",
        };
        let Ok(context) = KeyContext::parse(context) else {
            continue;
        };
        let Some(binding) =
            window.highest_precedence_binding_for_action_in_context(action.as_ref(), context)
        else {
            continue;
        };
        let keystrokes: Vec<String> = binding
            .keystrokes()
            .iter()
            .map(|k| keystroke_binding_string(k, macos))
            .collect();
        if keystrokes.is_empty() {
            continue;
        }
        let raw = keystrokes.join(" ");
        let display =
            shortcuts_core::preferred_display_sequence(action.name(), &raw, &live).unwrap_or(raw);
        let hint = format_binding(&display, macos);
        row.detail = format!("{} ({})", row.detail, hint);
    }
}

fn keystroke_binding_string(keystroke: &gpui_kit::KeybindingKeystroke, macos: bool) -> String {
    let mut parts = Vec::new();
    let modifiers = keystroke.modifiers();
    if modifiers.control {
        parts.push("ctrl");
    }
    if modifiers.platform {
        parts.push(if macos { "cmd" } else { "super" });
    }
    if modifiers.alt {
        parts.push("alt");
    }
    if modifiers.shift {
        parts.push("shift");
    }
    parts.push(keystroke.key());
    parts.join("-")
}

/// The row of a slash command: `None` for commands that do nothing over RPC;
/// terminal-only Gentle Shell ones open their native screen instead of
/// running, and Pi built-ins that only work in the terminal say so.
pub(super) fn command_row(command: SlashCommand) -> Option<Row> {
    let SlashCommand {
        name,
        description: detail,
        terminal,
    } = command;
    if commands::hidden(&name) {
        return None;
    }
    let (group, icon) = if name.starts_with("skill:") {
        ("Skills", IconName::Zap)
    } else if name.starts_with("gentle:") {
        ("Gentle commands", IconName::Command)
    } else {
        ("Commands", IconName::Command)
    };
    let action = match commands::route(&name) {
        Some(screen) => PaletteAction::Screen(screen),
        None => PaletteAction::Command(name.clone()),
    };
    Some(Row {
        icon,
        title: format!("/{name}"),
        meta: if terminal {
            "terminal".to_string()
        } else {
            String::new()
        },
        detail,
        group,
        action,
        workspace: None,
        current: false,
    })
}

#[cfg(test)]
mod tests {
    use gentle_core::commands::{Screen, SlashCommand};

    use super::super::{PaletteAction, TabLayout};
    use super::{command_row, screen_rows};

    fn command(name: &str, detail: &str, terminal: bool) -> SlashCommand {
        SlashCommand {
            name: name.into(),
            description: detail.into(),
            terminal,
        }
    }

    fn action(name: &str) -> Option<PaletteAction> {
        command_row(command(name, "detail", false)).map(|row| row.action)
    }

    #[test]
    fn tab_placement_command_matches_current_layout() {
        for (current, expected_title, target, absent) in [
            (
                TabLayout::Horizontal,
                "View: Show tabs vertically",
                TabLayout::Vertical,
                "View: Show tabs horizontally",
            ),
            (
                TabLayout::Vertical,
                "View: Show tabs horizontally",
                TabLayout::Horizontal,
                "View: Show tabs vertically",
            ),
        ] {
            let rows = screen_rows(current);
            assert!(rows.iter().any(|row| {
                row.title == expected_title && row.action == PaletteAction::SetTabLayout(target)
            }));
            assert!(!rows.iter().any(|row| row.title == absent));
        }
    }

    #[test]
    fn backend_commands_skip_dead_ones_and_open_native_screens() {
        assert_eq!(action("gentle:guide"), None);
        assert_eq!(action("history"), None);
        assert_eq!(action("gentle:panel"), None);
        assert_eq!(
            action("gentle:usage"),
            Some(PaletteAction::Screen(Screen::Usage))
        );
        assert_eq!(
            action("gentle:profiles"),
            Some(PaletteAction::Screen(Screen::Profiles))
        );
        assert_eq!(
            action("gentle:doctor"),
            Some(PaletteAction::Command("gentle:doctor".into()))
        );
        let row = command_row(command("gentle:stats", "Local stats", false)).unwrap();
        assert_eq!(
            (row.title.as_str(), row.detail.as_str(), row.meta.as_str()),
            ("/gentle:stats", "Local stats", "")
        );
    }

    #[test]
    fn terminal_only_pi_built_ins_are_marked() {
        let row = command_row(command("model", "Select model", true)).unwrap();
        assert_eq!(row.meta, "terminal");
        assert_eq!(row.action, PaletteAction::Command("model".into()));
    }

    #[test]
    fn new_focus_and_navigation_commands_are_present() {
        let rows = screen_rows(TabLayout::Horizontal);
        assert!(rows.iter().any(|r| r.title == "Focus composer"));
        assert!(rows.iter().any(|r| r.title == "Focus Files"));
        assert!(rows.iter().any(|r| r.title == "Focus Git changes"));
        assert!(rows.iter().any(|r| r.title == "Next tab"));
        assert!(rows.iter().any(|r| r.title == "Previous tab"));
        assert!(rows.iter().any(|r| r.title == "Focus next pane"));
        assert!(rows.iter().any(|r| r.title == "Focus previous pane"));
        for n in 1..=9 {
            assert!(rows.iter().any(|r| r.title == format!("Go to tab {n}")));
        }
    }

    #[test]
    fn palette_actions_map_to_gpui_actions() {
        use gpui_kit::Action as _;

        use super::palette_action_to_gpui;
        use crate::features::workspace::{
            ActivateTab1, FocusComposer, FocusFiles, NewTab, NextTab,
        };

        assert!(
            palette_action_to_gpui(&PaletteAction::NewTab)
                .is_some_and(|a| a.name() == NewTab.name())
        );
        assert!(
            palette_action_to_gpui(&PaletteAction::NextTab)
                .is_some_and(|a| a.name() == NextTab.name())
        );
        assert!(
            palette_action_to_gpui(&PaletteAction::FocusComposer)
                .is_some_and(|a| a.name() == FocusComposer.name())
        );
        assert!(
            palette_action_to_gpui(&PaletteAction::FocusFiles)
                .is_some_and(|a| a.name() == FocusFiles.name())
        );
        assert!(
            palette_action_to_gpui(&PaletteAction::ActivateTab(1))
                .is_some_and(|a| a.name() == ActivateTab1.name())
        );
        assert!(
            palette_action_to_gpui(&PaletteAction::FocusPaneLeft)
                .is_some_and(|a| a.name() == crate::features::workspace::FocusPaneLeft.name())
        );
        assert!(
            palette_action_to_gpui(&PaletteAction::TogglePaneMode)
                .is_some_and(|a| a.name() == crate::features::workspace::TogglePaneMode.name())
        );
        assert!(palette_action_to_gpui(&PaletteAction::ActivateTab(10)).is_none());
        assert!(palette_action_to_gpui(&PaletteAction::Sessions).is_none());
        assert!(
            palette_action_to_gpui(&PaletteAction::Agents)
                .is_some_and(|a| a.name() == crate::features::session::ToggleAgents.name())
        );
    }
}
