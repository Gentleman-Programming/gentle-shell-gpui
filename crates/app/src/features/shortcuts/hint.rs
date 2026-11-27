//! The which-key panel: the keys that can come next, drawn bottom-center.

use gentle_core::keyboard::format_binding;
use gentle_core::shortcuts::{self, Section};
use gpui_kit::component::h_flex;
use gpui_kit::*;

use super::{keystroke_string, live_bindings};
use crate::shared::ui;

/// What the panel shows.
#[derive(Clone, PartialEq, Eq)]
pub struct WhichKey {
    pub title: String,
    /// Key as displayed, and what it does.
    pub entries: Vec<(String, &'static str)>,
    pub footer: &'static str,
}

/// The hint for the input pending in `window`, if some binding continues it.
pub fn pending_hint(window: &Window, cx: &App) -> Option<WhichKey> {
    let macos = cfg!(target_os = "macos");
    let pending: Vec<String> = window
        .pending_input()?
        .keystrokes()
        .iter()
        .map(keystroke_string)
        .collect();
    let entries: Vec<_> = shortcuts::hints(&live_bindings(cx), &pending)
        .into_iter()
        .map(|hint| (format_binding(&hint.keys[0], macos), hint.label))
        .collect();
    (!entries.is_empty()).then(|| WhichKey {
        title: format_binding(&pending.join(" "), macos),
        entries,
        footer: "Esc cancels",
    })
}

/// The hint for pane mode: its keys, from the live keymap.
pub fn pane_mode_hint(cx: &App) -> Option<WhichKey> {
    let macos = cfg!(target_os = "macos");
    let group = shortcuts::groups(&live_bindings(cx), &crate::app::leader(cx))
        .into_iter()
        .find(|g| g.section == Section::PaneMode)?;
    Some(WhichKey {
        title: "Pane mode".into(),
        entries: group
            .rows
            .iter()
            .map(|row| (format_binding(&row.keys[0], macos), row.label))
            .collect(),
        footer: "Esc exits",
    })
}

/// The floating panel, over the bottom edge of the window.
pub fn panel(hint: &WhichKey, cx: &App) -> Div {
    div()
        .absolute()
        .bottom(rems(20. / 16.))
        .left_0()
        .right_0()
        .flex()
        .justify_center()
        .child(
            ui::island(cx)
                .max_w(rems(720. / 16.))
                .px_4()
                .py_3()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    h_flex()
                        .gap_3()
                        .child(ui::section_label(hint.title.clone()))
                        .child(div().text_xs().text_color(ui::faint()).child(hint.footer)),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .flex_wrap()
                        .gap_x_4()
                        .gap_y_1()
                        .children(hint.entries.iter().map(|(key, label)| {
                            h_flex()
                                .w(rems(180. / 16.))
                                .gap_2()
                                .child(ui::kbd(key.clone()))
                                .child(
                                    div()
                                        .truncate()
                                        .text_xs()
                                        .text_color(ui::muted())
                                        .child(*label),
                                )
                        })),
                ),
        )
}
