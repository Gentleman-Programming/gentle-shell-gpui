//! Keyboard section for Configuration → Appearance.

use gpui_kit::assets::IconName;
use gpui_kit::component::h_flex;
use gpui_kit::*;

use super::SettingsView;
use super::layout::section;
use crate::shared::ui;
use crate::shared::vim_keys;

impl SettingsView {
    pub(super) fn keyboard_section(&self, cx: &mut Context<Self>) -> Div {
        let enabled = vim_keys::enabled(cx);
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
                         o opens in Files, s/u stage/unstage and d opens the diff in Changes. \
                         Arrows keep working.",
            ))
    }
}
