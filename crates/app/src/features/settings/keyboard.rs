//! Keyboard section for Configuration → Appearance.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{Sizable as _, h_flex};
use gpui_kit::*;

use super::layout::section;
use super::{SettingsEvent, SettingsView};
use crate::shared::{keymap, ui, vim_keys};

impl SettingsView {
    pub(super) fn keyboard_section(&self, cx: &mut Context<Self>) -> Div {
        let enabled = vim_keys::enabled(cx);
        let path = keymap::path()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "No data folder on this system".into());
        section(IconName::Keyboard, "Keyboard")
            .child(
                h_flex()
                    .pt_2()
                    .gap_3()
                    .items_center()
                    .child(
                        div()
                            .w(rems(120. / 16.))
                            .flex_shrink_0()
                            .text_xs()
                            .text_color(ui::muted())
                            .child("Vim keys"),
                    )
                    .child(ui::segmented(
                        "vim-keys",
                        &["Off", "On"],
                        Some(usize::from(enabled)),
                        cx.listener(|_, index: &usize, _, cx| {
                            vim_keys::set_enabled(*index == 1, cx);
                        }),
                    )),
            )
            .child(div().pt_2().text_xs().text_color(ui::faint()).child(
                "j/k navigate lists, h/l collapse and expand, gg/G jump to first/last, \
                         s/u stage/unstage in Changes. Arrows keep working.",
            ))
            .child(
                div()
                    .pt_3()
                    .text_xs()
                    .text_color(ui::muted())
                    .child("Keymap file"),
            )
            .child(div().text_xs().text_color(ui::faint()).child(path))
            .child(
                h_flex()
                    .pt_2()
                    .gap_2()
                    .child(
                        Button::new("settings-open-keymap")
                            .ghost()
                            .small()
                            .label("Open keymap file")
                            .on_click(
                                cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::OpenKeymap)),
                            ),
                    )
                    .child(
                        Button::new("settings-reload-keymap")
                            .ghost()
                            .small()
                            .label("Reload keymap")
                            .on_click(
                                cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::ReloadKeymap)),
                            ),
                    ),
            )
    }
}
