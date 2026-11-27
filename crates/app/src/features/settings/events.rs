//! Actions emitted by Configuration for the workspace to handle.
//!
//! This is the public integration contract. Keep it separate from how a page
//! renders or persists preferences.

use crate::features::workspaces::TabLayout;

pub enum SettingsEvent {
    Close,
    /// Switch the current window's persisted tab placement.
    TabLayoutChanged(TabLayout),
    /// Toggle the left navigator's narrow icon rail.
    SidebarCompactChanged(bool),
    /// Show or hide the left navigator (vertical tabs only).
    TabsSidebarHidden(bool),
    /// Show or hide the right Files sidebar.
    FilesSidebarShown(bool),
    /// Open the application's full profile-routing editor.
    OpenProfiles,
    OpenKeymap,
    ReloadKeymap,
    /// Packages or sign-ins changed: tabs should reload their backend.
    Changed,
    /// Run this Gentle Shell slash command in the active agent pane and
    /// report back through [`super::SettingsView::shell_done`].
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
