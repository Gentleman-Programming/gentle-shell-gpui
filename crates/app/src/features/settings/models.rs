//! Model defaults and provider/profile inventory; actual profile routing
//! remains in the application's dedicated Profiles editor.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{Sizable as _, h_flex};
use gpui_kit::*;

use super::layout::{or_dash, row, section};
use super::remote::{commands, do_on, note, run_on};
use super::snapshot::Snapshot;
use super::{SettingsEvent, SettingsView};
use crate::shared::ui;

pub(super) fn models_section(s: &Snapshot) -> Div {
    // Pi's settings.json has no Desktop RPC store: change it there.
    if let Some(server) = &s.server {
        return section(IconName::Cpu, "Model defaults")
            .child(note(run_on(&server.name, commands::SETTINGS)));
    }
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

/// The inventory; on a server whose Shell signs in itself
/// (`signs_in_there`), Sign-ins below does it instead of the "Do this on"
/// note.
pub(super) fn providers_section(
    s: &Snapshot,
    signs_in_there: bool,
    cx: &mut Context<SettingsView>,
) -> Div {
    let server = s.server.as_ref().map(|server| server.name.as_str());
    // On a server, nothing here is a file read: an empty list means its
    // Gentle Shell didn't report one.
    let unknown = |fallback: &'static str| match server {
        Some(name) => format!("not reported by {name}"),
        None => fallback.into(),
    };
    section(IconName::Bot, "Profiles & providers")
        .child(row(
            "Active profile",
            s.active_profile.clone().unwrap_or_else(|| unknown("none")),
        ))
        .child(row(
            "Profiles",
            if s.profiles.is_empty() {
                unknown("—")
            } else {
                s.profiles.join(", ")
            },
        ))
        .child(row(
            "Sign-ins",
            if s.sign_ins.is_empty() {
                unknown("none")
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
            match server {
                // Pi's models.json has no Desktop RPC store.
                Some(name) => do_on(name, "edit models.json in its Gentle Shell agent folder"),
                None if s.custom_providers.is_empty() => "—".into(),
                None => s.custom_providers.join(", "),
            },
        ))
        .children(
            server
                .filter(|_| !signs_in_there)
                .map(|name| note(run_on(name, commands::LOGIN))),
        )
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
