//! Model defaults and provider/profile inventory; actual profile routing
//! remains in the application's dedicated Profiles editor.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{Sizable as _, h_flex};
use gpui_kit::*;

use super::layout::{or_dash, row, section};
use super::snapshot::Snapshot;
use super::{SettingsEvent, SettingsView};
use crate::shared::ui;

pub(super) fn models_section(s: &Snapshot) -> Div {
    section(IconName::Cpu, "Model defaults")
        .child(row(
            "Default model",
            or_dash(&s.default_model, "first signed-in model"),
        ))
        .child(row(
            "Default effort",
            or_dash(&s.default_thinking, "medium"),
        ))
        .child(row(
            "Scoped models",
            if s.enabled_models.is_empty() {
                "all".into()
            } else {
                s.enabled_models.join(", ")
            },
        ))
}

pub(super) fn providers_section(s: &Snapshot, cx: &mut Context<SettingsView>) -> Div {
    section(IconName::Bot, "Profiles & providers")
        .child(row("Active profile", or_dash(&s.active_profile, "none")))
        .child(row(
            "Profiles",
            if s.profiles.is_empty() {
                "—".into()
            } else {
                s.profiles.join(", ")
            },
        ))
        .child(row(
            "Sign-ins",
            if s.sign_ins.is_empty() {
                "none".into()
            } else {
                s.sign_ins
                    .iter()
                    .map(|(p, t)| format!("{p} ({t})"))
                    .collect::<Vec<_>>()
                    .join(", ")
            },
        ))
        .child(row(
            "Custom providers",
            if s.custom_providers.is_empty() {
                "—".into()
            } else {
                s.custom_providers.join(", ")
            },
        ))
        .child(
            div().pt_2().text_xs().text_color(ui::faint()).child(
                "Model and sign-in details are read-only here. Use Profiles to edit agent routing and defaults.",
            ),
        )
        .child(
            h_flex().pt_3().child(
                Button::new("settings-manage-profiles")
                    .ghost()
                    .small()
                    .label("Manage profiles")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::OpenProfiles))),
            ),
        )
}
