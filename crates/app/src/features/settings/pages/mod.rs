//! Page composition for Configuration.
//!
//! Navigation selects a category; each page owns its content composition.
//! Interactive state, subscriptions and persistence continue to be handled by
//! the existing feature modules until their ownership can be migrated safely.

mod agents;
mod appearance;
mod devices;
mod extensions;
mod general;
mod integrations;
mod models;
mod notifications;
mod updates;

use super::{SettingsView, navigation::SettingsPage, snapshot::Snapshot};
use crate::shared::ui;
use gpui_kit::*;

impl SettingsView {
    pub(super) fn render_page(
        &self,
        page: SettingsPage,
        snapshot: &Snapshot,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // Exhaustive dispatch keeps the category and its page in sync.
        // Only the selected page renders, so existing async actions remain
        // scoped to the selected configuration section.
        let sections: Div = match page {
            SettingsPage::General => general::render(self, snapshot),
            SettingsPage::Appearance => appearance::render(self, snapshot, cx),
            SettingsPage::Agents => agents::render(self, snapshot, cx),
            SettingsPage::Models => models::render(self, snapshot, cx),
            SettingsPage::Devices => devices::render(self, cx),
            SettingsPage::Integrations => integrations::render(self, snapshot, cx),
            SettingsPage::Notifications => notifications::render(self, cx),
            SettingsPage::Updates => updates::render(self, snapshot, cx),
            SettingsPage::Extensions => extensions::render(self, snapshot, cx),
        };
        ui::appear("settings-content", sections.p_4()).into_any_element()
    }
}
