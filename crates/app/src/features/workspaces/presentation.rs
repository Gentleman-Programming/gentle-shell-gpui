//! Independent, serializable presentation presets. They never own panes,
//! sessions, or workspace data.
use gpui_kit::assets::IconName;
use serde::{Deserialize, Serialize};

use super::{SavedState, TabLayout};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayoutTool {
    Files,
    Git,
    History,
    Engram,
    Agents,
    Docs,
    Devices,
    Usage,
    Tower,
    Profiles,
    Sessions,
    Providers,
    Configuration,
}

impl LayoutTool {
    pub const ALL: [Self; 13] = [
        Self::Files,
        Self::Git,
        Self::History,
        Self::Engram,
        Self::Agents,
        Self::Docs,
        Self::Devices,
        Self::Usage,
        Self::Tower,
        Self::Profiles,
        Self::Sessions,
        Self::Providers,
        Self::Configuration,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Files => "Files",
            Self::Git => "Git",
            Self::History => "Git History",
            Self::Engram => "Engram",
            Self::Agents => "Agents",
            Self::Docs => "Architecture Docs",
            Self::Devices => "Devices",
            Self::Usage => "Usage & Stats",
            Self::Tower => "Gentle Tower",
            Self::Profiles => "Profiles",
            Self::Sessions => "Sessions",
            Self::Providers => "Providers",
            Self::Configuration => "Configuration",
        }
    }
    pub fn icon(self) -> IconName {
        match self {
            Self::Files => IconName::Files,
            Self::Git => IconName::GitBranch,
            Self::History => IconName::GitGraph,
            Self::Engram => IconName::Brain,
            Self::Agents => IconName::Users,
            Self::Docs => IconName::BookOpen,
            Self::Devices => IconName::Smartphone,
            Self::Usage => IconName::Gauge,
            Self::Tower => IconName::Building2,
            Self::Profiles => IconName::Route,
            Self::Sessions => IconName::RotateCcwClock,
            Self::Providers => IconName::KeyRound,
            Self::Configuration => IconName::Settings,
        }
    }
    pub fn docked(self) -> bool {
        matches!(self, Self::Files | Self::Git | Self::History | Self::Engram)
    }

    /// Configuration tools stay at the bottom of vertical activity rails,
    /// independently of the saved order of the working tools.
    pub fn rail_footer(self) -> bool {
        matches!(self, Self::Profiles | Self::Providers | Self::Configuration)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolPlacement {
    Title,
    Left,
    Right,
    Hidden,
}

impl ToolPlacement {
    pub const ALL: [Self; 4] = [Self::Title, Self::Left, Self::Right, Self::Hidden];
    pub fn label(self) -> &'static str {
        match self {
            Self::Title => "Title bar",
            Self::Left => "Left rail",
            Self::Right => "Right rail",
            Self::Hidden => "Commands only",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayoutPreset {
    Minimal,
    Expanded,
}

impl LayoutPreset {
    pub const ALL: [Self; 2] = [Self::Minimal, Self::Expanded];
    pub fn label(self) -> &'static str {
        match self {
            Self::Minimal => "Minimal",
            Self::Expanded => "Expanded",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolShortcut {
    pub tool: LayoutTool,
    pub placement: ToolPlacement,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LayoutProfile {
    pub shortcuts: Vec<ToolShortcut>,
    /// Legacy field retained for older snapshots; normalized to true.
    pub tabs_visible: bool,
    #[serde(default)]
    pub title_utilities: bool,
    #[serde(default)]
    pub original_controls: bool,
    pub tab_layout: TabLayout,
    /// Legacy name: expanded (true) vs compact (false) vertical navigator.
    pub workspace_visible: bool,
    /// Legacy field retained for older snapshots; normalized to false.
    #[serde(default)]
    pub tabs_sidebar_hidden: bool,
    pub sidebar_visible: bool,
    pub dock: LayoutTool,
    pub tabs_width: f32,
    pub files_width: f32,
}

impl LayoutProfile {
    pub fn preset(preset: LayoutPreset) -> Self {
        let expanded = preset == LayoutPreset::Expanded;
        let width = gentle_core::workspaces::SIDEBAR_DEFAULT_WIDTH;
        Self {
            shortcuts: LayoutTool::ALL
                .into_iter()
                .map(|tool| ToolShortcut {
                    tool,
                    placement: if expanded {
                        ToolPlacement::Right
                    } else {
                        ToolPlacement::Hidden
                    },
                })
                .collect(),
            tabs_visible: true,
            title_utilities: true,
            original_controls: !expanded,
            tab_layout: if expanded {
                TabLayout::Vertical
            } else {
                TabLayout::Horizontal
            },
            workspace_visible: expanded,
            tabs_sidebar_hidden: false,
            sidebar_visible: true,
            dock: LayoutTool::Files,
            tabs_width: width,
            files_width: width,
        }
    }
    /// Tab rows have exactly one placement, independently of the navigator.
    pub fn horizontal_tabs(&self) -> bool {
        self.tab_layout == TabLayout::Horizontal
    }

    pub fn vertical_tabs(&self) -> bool {
        self.tab_layout == TabLayout::Vertical
    }

    pub fn navigation_visible(&self) -> bool {
        self.vertical_tabs()
    }

    pub fn navigation_compact(&self) -> bool {
        self.vertical_tabs() && !self.workspace_visible
    }

    pub fn has(&self, placement: ToolPlacement) -> bool {
        self.shortcuts.iter().any(|s| s.placement == placement)
    }
    /// Partition a vertical rail without changing stored order or placement.
    pub fn rail_tools(
        &self,
        placement: ToolPlacement,
        footer: bool,
    ) -> impl Iterator<Item = LayoutTool> + '_ {
        self.shortcuts
            .iter()
            .filter(move |s| s.placement == placement && s.tool.rail_footer() == footer)
            .map(|s| s.tool)
    }

    pub fn place(&mut self, tool: LayoutTool, placement: ToolPlacement) {
        if let Some(s) = self.shortcuts.iter_mut().find(|s| s.tool == tool) {
            s.placement = placement;
        }
    }
    /// Reorder without introducing duplicate shortcuts.
    pub fn move_before(&mut self, tool: LayoutTool, target: LayoutTool) {
        if tool == target {
            return;
        }
        let Some(from) = self.shortcuts.iter().position(|s| s.tool == tool) else {
            return;
        };
        if !self.shortcuts.iter().any(|s| s.tool == target) {
            return;
        }
        let item = self.shortcuts.remove(from);
        if let Some(to) = self.shortcuts.iter().position(|s| s.tool == target) {
            self.shortcuts.insert(to, item);
        }
    }
    pub fn normalize(&mut self, preset: LayoutPreset) {
        // Older layouts could hide both tab presentations. Restore their
        // selected orientation without changing sessions or saved widths.
        self.tabs_visible = true;
        self.tabs_sidebar_hidden = false;
        if self.horizontal_tabs() {
            self.workspace_visible = false;
        }
        let mut seen = Vec::new();
        self.shortcuts.retain(|s| {
            if seen.contains(&s.tool) {
                false
            } else {
                seen.push(s.tool);
                true
            }
        });
        for s in Self::preset(preset).shortcuts {
            if !seen.contains(&s.tool) {
                self.shortcuts.push(s);
            }
        }
        if !self.dock.docked() {
            self.dock = LayoutTool::Files;
        }
        self.tabs_width = gentle_core::workspaces::saved_sidebar_width(self.tabs_width);
        self.files_width = gentle_core::workspaces::saved_sidebar_width(self.files_width);
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LayoutProfiles {
    pub active: LayoutPreset,
    pub minimal: LayoutProfile,
    pub expanded: LayoutProfile,
}

impl Default for LayoutProfiles {
    fn default() -> Self {
        Self {
            active: LayoutPreset::Minimal,
            minimal: LayoutProfile::preset(LayoutPreset::Minimal),
            expanded: LayoutProfile::preset(LayoutPreset::Expanded),
        }
    }
}

impl LayoutProfiles {
    pub fn profile(&self, preset: LayoutPreset) -> &LayoutProfile {
        match preset {
            LayoutPreset::Minimal => &self.minimal,
            LayoutPreset::Expanded => &self.expanded,
        }
    }

    pub fn current(&self) -> &LayoutProfile {
        self.profile(self.active)
    }
    pub fn current_mut(&mut self) -> &mut LayoutProfile {
        match self.active {
            LayoutPreset::Minimal => &mut self.minimal,
            LayoutPreset::Expanded => &mut self.expanded,
        }
    }
    pub fn reset(&mut self) {
        let preset = self.active;
        *self.current_mut() = LayoutProfile::preset(preset);
    }
    pub fn normalize(&mut self) {
        self.minimal.normalize(LayoutPreset::Minimal);
        self.expanded.normalize(LayoutPreset::Expanded);
    }
    /// Existing installations retain their tab placement, widths and panel
    /// visibility. Only a new installation starts with the Minimal preset.
    pub fn from_saved(saved: &SavedState) -> Self {
        if let Some(mut profiles) = saved.presentation.clone() {
            profiles.normalize();
            return profiles;
        }
        let mut profiles = Self::default();
        if saved.version != 0 || !saved.workspaces.is_empty() {
            profiles.active = LayoutPreset::Minimal;
            let p = profiles.current_mut();
            p.tab_layout = saved.tab_layout;
            p.tabs_sidebar_hidden = saved.tabs_sidebar_hidden;
            p.workspace_visible = saved.tab_layout == TabLayout::Vertical && !saved.sidebar_compact;
            p.sidebar_visible = saved.sidebar;
            p.tabs_width = saved.tabs_sidebar_width;
            p.files_width = saved.files_sidebar_width;
        }
        profiles.normalize();
        profiles
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_placement_is_exclusive_in_every_navigator_state() {
        for preset in LayoutPreset::ALL {
            let mut p = LayoutProfile::preset(preset);
            for layout in [TabLayout::Horizontal, TabLayout::Vertical] {
                p.tab_layout = layout;
                for tabs_visible in [false, true] {
                    p.tabs_visible = tabs_visible;
                    for workspace_visible in [false, true] {
                        p.workspace_visible = workspace_visible;
                        for hidden in [false, true] {
                            p.tabs_sidebar_hidden = hidden;
                            let horizontal = p.horizontal_tabs();
                            let vertical = p.vertical_tabs();
                            assert_ne!(horizontal, vertical);
                            assert_eq!(horizontal, layout == TabLayout::Horizontal);
                            assert_eq!(vertical, layout == TabLayout::Vertical);
                            assert_eq!(p.navigation_visible(), vertical);
                            assert_eq!(p.navigation_compact(), vertical && !workspace_visible);
                            let mut normalized = p.clone();
                            normalized.normalize(preset);
                            assert!(normalized.tabs_visible);
                            assert!(!normalized.tabs_sidebar_hidden);
                            assert_eq!(normalized.tab_layout, layout);
                            assert_eq!(normalized.navigation_visible(), vertical);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn saved_hidden_presets_restore_their_selected_orientation() {
        let mut profiles = LayoutProfiles::default();
        for p in [&mut profiles.minimal, &mut profiles.expanded] {
            p.tabs_visible = false;
            p.tabs_sidebar_hidden = true;
            p.tabs_width = 345.;
        }
        let saved = SavedState {
            presentation: Some(profiles),
            ..SavedState::default()
        };
        let restored = LayoutProfiles::from_saved(&saved);
        assert!(restored.minimal.horizontal_tabs());
        assert!(!restored.minimal.navigation_visible());
        assert!(restored.expanded.vertical_tabs());
        assert!(restored.expanded.navigation_visible());
        assert_eq!(restored.expanded.tabs_width, 345.);
        assert!(!restored.expanded.tabs_sidebar_hidden);
    }

    #[test]
    fn minimal_matches_original_window_defaults() {
        let p = LayoutProfiles::from_saved(&SavedState::default());
        assert_eq!(p.active, LayoutPreset::Minimal);
        let current = p.current();
        assert!(current.tabs_visible && current.title_utilities && current.original_controls);
        assert_eq!(current.tab_layout, TabLayout::Horizontal);
        assert!(!current.workspace_visible);
        assert_eq!(current.sidebar_visible, SavedState::default().sidebar);
        assert!(
            current
                .shortcuts
                .iter()
                .all(|s| s.placement == ToolPlacement::Hidden)
        );
        assert!(!p.expanded.original_controls);
    }

    #[test]
    fn reopening_keeps_the_selected_preset_instead_of_resetting_it() {
        let mut profiles = LayoutProfiles {
            active: LayoutPreset::Expanded,
            ..LayoutProfiles::default()
        };
        profiles
            .expanded
            .place(LayoutTool::Git, ToolPlacement::Title);
        let saved = SavedState {
            presentation: Some(profiles.clone()),
            ..SavedState::default()
        };
        assert_eq!(LayoutProfiles::from_saved(&saved), profiles);
    }

    #[test]
    fn presets_keep_independent_changes_and_round_trip() {
        let mut p = LayoutProfiles::default();
        p.current_mut()
            .place(LayoutTool::Engram, ToolPlacement::Left);
        p.current_mut()
            .move_before(LayoutTool::Engram, LayoutTool::Files);
        p.active = LayoutPreset::Expanded;
        assert_eq!(p.current().shortcuts[0].tool, LayoutTool::Files);
        p.current_mut().sidebar_visible = false;
        p.active = LayoutPreset::Minimal;
        assert_eq!(p.current().shortcuts[0].tool, LayoutTool::Engram);
        assert_eq!(
            serde_json::from_str::<LayoutProfiles>(&serde_json::to_string(&p).unwrap()).unwrap(),
            p
        );
        p.reset();
        assert_eq!(p.current().shortcuts[0].tool, LayoutTool::Files);
        assert!(!p.expanded.sidebar_visible);
    }

    #[test]
    fn configuration_tools_stay_in_the_footer_of_their_assigned_rail() {
        let mut p = LayoutProfile::preset(LayoutPreset::Expanded);
        p.move_before(LayoutTool::Configuration, LayoutTool::Files);
        p.place(LayoutTool::Providers, ToolPlacement::Left);
        p.place(LayoutTool::Profiles, ToolPlacement::Hidden);

        let top = p.rail_tools(ToolPlacement::Right, false).collect::<Vec<_>>();
        assert_eq!(top.first(), Some(&LayoutTool::Files));
        assert!(top.iter().all(|tool| !tool.rail_footer()));
        assert_eq!(
            p.rail_tools(ToolPlacement::Right, true).collect::<Vec<_>>(),
            vec![LayoutTool::Configuration]
        );
        assert_eq!(
            p.rail_tools(ToolPlacement::Left, true).collect::<Vec<_>>(),
            vec![LayoutTool::Providers]
        );
        assert!(!p.rail_tools(ToolPlacement::Right, true).any(|tool| tool == LayoutTool::Profiles));
    }

    #[test]
    fn normalization_removes_duplicates_and_restores_missing_tools() {
        let mut p = LayoutProfile::preset(LayoutPreset::Minimal);
        p.shortcuts.push(p.shortcuts[0]);
        p.shortcuts.remove(1);
        p.dock = LayoutTool::Devices;
        p.normalize(LayoutPreset::Minimal);
        assert_eq!(p.shortcuts.len(), LayoutTool::ALL.len());
        assert_eq!(p.dock, LayoutTool::Files);
    }

    #[test]
    fn legacy_layout_preserves_visibility_and_width() {
        let saved = SavedState {
            version: 1,
            tab_layout: TabLayout::Vertical,
            sidebar_compact: true,
            sidebar: false,
            files_sidebar_width: 420.,
            ..SavedState::default()
        };
        let p = LayoutProfiles::from_saved(&saved);
        assert_eq!(p.active, LayoutPreset::Minimal);
        assert_eq!(p.current().tab_layout, TabLayout::Vertical);
        assert!(!p.current().workspace_visible && !p.current().sidebar_visible);
        assert_eq!(p.current().files_width, 420.);
    }
}
