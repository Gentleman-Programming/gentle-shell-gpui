//! Model defaults, profile and provider inventory, signing in on a Gentle
//! Shell server, and an installed Claude bridge.

use gpui_kit::component::v_flex;
use gpui_kit::*;

use super::super::{SettingsView, models, snapshot::Snapshot};

pub(super) fn render(
    view: &SettingsView,
    snapshot: &Snapshot,
    cx: &mut Context<SettingsView>,
) -> Div {
    let sign_in = view.server_sign_in_section(cx);
    v_flex()
        .gap_3()
        .child(models::models_section(view, snapshot, cx))
        .child(models::providers_section(snapshot, sign_in.is_some(), cx))
        .children(sign_in)
        .children(
            view.server()
                .map(|server| view.server_custom_models(server, cx)),
        )
        .children(view.bridge_section(snapshot, cx))
}
