//! The cheat-sheet screen: every shortcut of the live keymap by section,
//! with a search field.

use gentle_core::keyboard::format_binding;
use gentle_core::shortcuts::{self, Group, Row, Section};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::Input;
use gpui_kit::component::{Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::{ShortcutsEvent, ShortcutsView};
use crate::shared::ui;

const MACOS: bool = cfg!(target_os = "macos");

/// A line under a section's title saying how its keys work.
fn note(section: Section, leader: &str) -> Option<String> {
    let leader = format_binding(leader, MACOS);
    match section {
        Section::Leader => Some(format!(
            "Press {leader}, then one key. It waits about a second."
        )),
        Section::PaneMode => Some(format!(
            "{leader} Space turns it on; it stays on until Esc or Enter."
        )),
        Section::Files => Some(
            "Arrows and Enter always work. Letter keys need Configuration › Appearance › Keyboard › Vim keys."
                .into(),
        ),
        Section::Direct | Section::Session => None,
    }
}

/// A row's keys: one key cap per keystroke, alternatives apart.
fn keys(row: &Row) -> Div {
    h_flex()
        .gap_2()
        .flex_shrink_0()
        .children(row.keys.iter().map(|sequence| {
            h_flex().gap_1().children(
                sequence
                    .split_whitespace()
                    .map(|k| ui::kbd(format_binding(k, MACOS))),
            )
        }))
}

fn render_group(group: &Group, leader: &str) -> Div {
    v_flex()
        .gap_1()
        .child(ui::section_label(group.section.title()))
        .when_some(note(group.section, leader), |d, note| {
            d.child(div().text_xs().text_color(ui::faint()).child(note))
        })
        .child(
            v_flex()
                .gap_0p5()
                .mt_1()
                .children(group.rows.iter().map(|row| {
                    h_flex()
                        .px_2()
                        .py_1()
                        .gap_3()
                        .rounded_md()
                        .hover(|d| d.bg(ui::surface_hover()))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_sm()
                                .text_color(ui::fg())
                                .child(row.label),
                        )
                        .when(row.vim, |d| d.child(ui::chip("Vim keys", ui::muted())))
                        .child(keys(row))
                })),
        )
}

impl Render for ShortcutsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let query = self.query.read(cx).value().to_string();
        // Read the live keymap on every render, so a reload while the screen
        // is open shows the new bindings.
        let leader = crate::app::leader(cx);
        let all = shortcuts::groups(&super::live_bindings(cx), &leader);
        let groups = shortcuts::search(&all, &query, MACOS);
        let (backdrop, panel) = ui::screen(Some(760.));
        backdrop.occlude().child(ui::pop_in(
            "shortcuts-screen",
            panel
                .child(
                    ui::screen_header()
                        .child(
                            Icon::new(IconName::Keyboard)
                                .small()
                                .text_color(ui::muted()),
                        )
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_base()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(ui::fg())
                                .child("Keyboard shortcuts"),
                        )
                        .child(div().flex_1())
                        .child(
                            Button::new("shortcuts-close")
                                .ghost()
                                .xsmall()
                                .icon(Icon::new(IconName::X))
                                .on_click(
                                    cx.listener(|_, _, _, cx| cx.emit(ShortcutsEvent::Close)),
                                ),
                        ),
                )
                .child(
                    div().px_4().py_3().child(
                        ui::field()
                            .child(Icon::new(IconName::Search).small().text_color(ui::faint()))
                            .child(Input::new(&self.query).appearance(false).small()),
                    ),
                )
                .child(
                    v_flex()
                        .id("shortcuts-list")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .px_4()
                        .pb_4()
                        .gap_5()
                        .children(groups.iter().map(|group| render_group(group, &leader)))
                        .when(groups.is_empty(), |d| {
                            d.child(
                                div()
                                    .py_8()
                                    .text_center()
                                    .text_sm()
                                    .text_color(ui::muted())
                                    .child("No shortcut matches."),
                            )
                        }),
                ),
        ))
    }
}
