//! Model defaults and provider/profile inventory; actual profile routing
//! remains in the application's dedicated Profiles editor. On a Gentle
//! Shell server the model defaults are its Gentle Shell's `pi_settings`
//! store (Pi's merged view), read and changed over Desktop RPC; a server too old for it keeps
//! saying "Do this on <server>".

use gentle_core::model_defaults::ModelDefaults;
use gentle_core::profiles::THINKING_LEVELS;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{Disableable as _, Sizable as _, h_flex};
use gpui_kit::*;
use pi_rpc::desktop_rpc::{ConfigStatus, ConfigStore};

use super::layout::{control, or_dash, row, section};
use super::remote::{ServerPlace, commands, do_on, note, run_on};
use super::snapshot::Snapshot;
use super::{SettingsEvent, SettingsView};
use crate::shared::ui;

/// What the effort menu offers for "no default" (Pi's own).
const PI_EFFORT: &str = "Pi's default (medium)";

pub(super) fn models_section(
    view: &SettingsView,
    s: &Snapshot,
    cx: &mut Context<SettingsView>,
) -> Div {
    if let Some(server) = view.server() {
        return view.server_model_defaults(server, cx);
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

impl SettingsView {
    /// The server's model defaults, changed there: a pick patches the keys
    /// it changes over the revision read (another change meanwhile is
    /// refused and shown afresh).
    fn server_model_defaults(&self, server: &ServerPlace, cx: &mut Context<Self>) -> Div {
        let card = section(IconName::Cpu, "Model defaults");
        let store = ConfigStore::PiSettings;
        if self.server_lacks(store, cx) {
            return card.child(note(run_on(&server.name, commands::SETTINGS)));
        }
        let Some(state) = self.shell_config.state(store) else {
            return card.child(
                h_flex()
                    .gap_2()
                    .text_xs()
                    .text_color(ui::muted())
                    .child(Spinner::new().xsmall().color(ui::accent()))
                    .child(format!("Asking {} for its model defaults…", server.name)),
            );
        };
        if state.status == ConfigStatus::Invalid {
            return card.child(note(do_on(
                &server.name,
                "fix settings.json in its Gentle Shell agent folder (it isn't a JSON object)",
            )));
        }
        let defaults = ModelDefaults::from_value(state.value.as_ref());
        let busy = self.shell_config.writing(store);
        let choices = self
            .session
            .as_ref()
            .map(|session| session.read(cx).model_choices())
            .unwrap_or_default();
        let weak = cx.entity().downgrade();
        let pick = move |defaults: ModelDefaults| {
            let weak = weak.clone();
            move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                let _ = weak.update(cx, |this, cx| this.set_model_defaults(&defaults, cx));
            }
        };
        let model = Button::new("server-default-model")
            .ghost()
            .xsmall()
            .label(
                defaults
                    .model_label()
                    .unwrap_or_else(|| "First signed-in model".into()),
            )
            .dropdown_caret(true)
            .disabled(busy)
            .dropdown_menu({
                let (defaults, pick) = (defaults.clone(), pick.clone());
                move |menu, _, _| {
                    let current = defaults.model_label();
                    let menu = menu.item(
                        PopupMenuItem::new("First signed-in model")
                            .checked(current.is_none())
                            .on_click(pick(defaults.with_model(None))),
                    );
                    choices.iter().fold(menu, |menu, (provider, id, name, _)| {
                        let label = format!("{provider}/{id}");
                        menu.item(
                            PopupMenuItem::new(format!("{name} · {provider}"))
                                .checked(current.as_deref() == Some(label.as_str()))
                                .on_click(pick(defaults.with_model(Some((provider, id))))),
                        )
                    })
                }
            });
        let effort = Button::new("server-default-effort")
            .ghost()
            .xsmall()
            .label(
                defaults
                    .thinking
                    .clone()
                    .unwrap_or_else(|| PI_EFFORT.into()),
            )
            .dropdown_caret(true)
            .disabled(busy)
            .dropdown_menu({
                let (defaults, pick) = (defaults.clone(), pick.clone());
                move |menu, _, _| {
                    let menu = menu.item(
                        PopupMenuItem::new(PI_EFFORT)
                            .checked(defaults.thinking.is_none())
                            .on_click(pick(defaults.with_thinking(None))),
                    );
                    THINKING_LEVELS.into_iter().fold(menu, |menu, level| {
                        menu.item(
                            PopupMenuItem::new(level)
                                .checked(defaults.thinking.as_deref() == Some(level))
                                .on_click(pick(defaults.with_thinking(Some(level)))),
                        )
                    })
                }
            });
        let scoped = if defaults.enabled.is_empty() {
            "all".to_string()
        } else {
            defaults.enabled.join(", ")
        };
        card.child(control("Default model", busy, model))
            .child(control("Default effort", busy, effort))
            .child(
                control(
                    "Scoped models",
                    busy,
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(ui::code_rems(12.))
                        .font_family(ui::mono())
                        .text_color(ui::fg())
                        .child(scoped),
                )
                .children((!defaults.enabled.is_empty()).then(|| {
                    Button::new("server-all-models")
                        .ghost()
                        .xsmall()
                        .label("Use all models")
                        .disabled(busy)
                        .on_click(pick(defaults.with_all_models()))
                })),
            )
            .child(
                div()
                    .pt_2()
                    .text_xs()
                    .text_color(ui::faint())
                    .child(format!(
                        "Saved in settings.json on {}; its new agents start with these.",
                        server.name
                    )),
            )
            .children(self.shell_config.note(store).map(|text| {
                div()
                    .pt_1()
                    .text_xs()
                    .text_color(ui::danger())
                    .child(text.to_string())
            }))
    }
}

/// The inventory; on a server whose Shell signs in itself
/// (`signs_in_there`), Sign-ins below does it instead of the "Do this on"
/// note, and its custom providers are the Custom models card's.
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
        .children(server.is_none().then(|| {
            row(
                "Custom providers",
                if s.custom_providers.is_empty() {
                    "—".into()
                } else {
                    s.custom_providers.join(", ")
                },
            )
        }))
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
