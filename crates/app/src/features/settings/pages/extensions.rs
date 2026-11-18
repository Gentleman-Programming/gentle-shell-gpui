//! Installed packages and extensions, using the existing package controller.

use gpui_kit::component::v_flex;
use gpui_kit::*;

use super::super::{SettingsView, snapshot::Snapshot};

pub(super) fn render(
    view: &SettingsView,
    snapshot: &Snapshot,
    cx: &mut Context<SettingsView>,
) -> Div {
    v_flex().gap_3().child(view.packages_section(snapshot, cx))
}
