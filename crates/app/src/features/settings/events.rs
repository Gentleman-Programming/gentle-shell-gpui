//! Actions emitted by Configuration for the workspace to handle.
//!
//! This is the public integration contract. Keep it separate from how a page
//! renders or persists preferences.

use crate::features::workspaces::TabLayout;

pub enum SettingsEvent {
    Close,
    /// Switch the current window's persisted tab placement.
    TabLayoutChanged(TabLayout),
    /// Open the application's full profile-routing editor.
    OpenProfiles,
    /// Packages or sign-ins changed: tabs should reload their backend.
    Changed,
    /// Credentials changed on a Gentle Shell server (a sign-in, a key, a
    /// removal): tabs pick them up, in place where their Shell reloads them.
    CredentialsChanged,
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
    /// This device was unpaired from a Gentle Shell server (what names it,
    /// every address it was paired or reached at, its name, and whether
    /// the server removed this device, closing its terminals): open tabs
    /// and terminals on it let go of it, leaving its sessions running there.
    ServerUnpaired {
        server: String,
        addresses: Vec<String>,
        name: String,
        revoked: bool,
    },
}
