//! The composer: context line, attachments, text area and its bottom bar.

use std::time::{Duration, Instant};

/// The focus pulse's length.
const PULSE: Duration = Duration::from_millis(460);
/// A typed kiss's flight, the least time between two, and how many fly.
const KISS_LIFE: Duration = Duration::from_millis(900);
const KISS_GAP: Duration = Duration::from_millis(90);
const KISSES_MAX: usize = 10;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{InputToken, Textarea};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::activity::chip_popover;
use super::dialogs::checkbox_box;
use super::{NARROW, TINY, image_tile, number_badge};
use crate::features::session::SessionView;
use crate::features::session::attachments::PastedText;
use crate::shared::kisses::{HEART, LIPS, noise, paint_sprite};
use crate::shared::ui;
use gentle_core::transcript::format_tokens;

impl SessionView {
    /// The thin context gauge under the status row: only when the window is
    /// more than 0% used, so at rest the row alone is the status line.
    fn render_context_bar(&self) -> Option<Div> {
        let percent = self.usage.context_percent.unwrap_or(0.0).clamp(0.0, 100.0) as f32;
        (percent > 0.0).then(move || {
            div().w_full().px_1().pt(px(2.)).child(
                div()
                    .w_full()
                    .h(px(2.))
                    .rounded_full()
                    .bg(ui::border())
                    .child(
                        div()
                            .h_full()
                            .w(relative(percent / 100.0))
                            .rounded_full()
                            .bg(ui::petal()),
                    ),
            )
        })
    }

    /// The context percentage as a chip: compact now, toggle auto-compaction.
    pub(super) fn render_context_chip(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let high = self
            .usage
            .context_percent
            .is_some_and(|p| p >= crate::features::alerts::context_threshold(cx));
        let compacting = self.usage.compacting;
        let auto = self.usage.auto_compaction;
        let ready = self.backend.is_ready();
        let used = match self.usage.context_percent {
            Some(p) => format!("{p:.0}%"),
            None => "–".into(),
        };
        let window = self
            .models
            .current
            .as_ref()
            .map(|m| m.context_window)
            .filter(|w| *w > 0)
            .map(format_tokens);
        let label = if compacting {
            "compacting…".to_string()
        } else {
            used.clone()
        };
        let count = match &window {
            Some(window) => format!("{used} of {window}"),
            None => used,
        };
        let color = if high { ui::warning() } else { ui::faint() };
        let weak = cx.entity().downgrade();
        chip_popover(
            "context",
            IconName::Gauge,
            color,
            label,
            "Context",
            count,
            move || {
                let compact = weak.clone();
                let toggle = weak.clone();
                vec![
                    div()
                        .text_size(rems(12. / 16.))
                        .text_color(ui::muted())
                        .whitespace_normal()
                        .child(if high {
                            "Context is nearly full. Compacting summarizes the conversation so far to free room."
                        } else {
                            "Compacting summarizes the conversation so far to free room. Add what to keep with /compact <instructions>."
                        })
                        .into_any_element(),
                    h_flex()
                        .pt_1()
                        .child(
                            Button::new("context-compact")
                                .small()
                                .primary()
                                .icon(Icon::new(IconName::ChevronsDownUp))
                                .label(if compacting { "Compacting…" } else { "Compact now" })
                                .loading(compacting)
                                .disabled(compacting || !ready)
                                .on_click(move |_, _, cx| {
                                    let _ = compact.update(cx, |this, cx| this.compact(None, cx));
                                }),
                        )
                        .into_any_element(),
                    h_flex()
                        .id("context-auto-compaction")
                        .gap_2()
                        .pt_1()
                        .cursor_pointer()
                        .text_size(rems(12. / 16.))
                        .text_color(ui::fg())
                        .when(auto.is_none() || !ready, |d| d.opacity(0.5))
                        .child(checkbox_box(auto.unwrap_or(false)))
                        .child("Compact automatically when nearly full")
                        .when_some(auto.filter(|_| ready), |d, enabled| {
                            d.on_click(move |_, _, cx| {
                                let _ = toggle.update(cx, |this, cx| {
                                    this.set_auto_compaction(!enabled, cx)
                                });
                            })
                        })
                        .into_any_element(),
                ]
            },
        )
    }

    /// 64px tiles of what waits to be sent: numbered image thumbnails, then
    /// pasted-text cards; each with ×.
    fn render_attachments(&self, cx: &mut Context<Self>) -> Option<Div> {
        if self.attachments.is_empty() && self.pasted.is_empty() {
            return None;
        }
        let images: Vec<_> = self
            .attachments
            .iter()
            .enumerate()
            .map(|(ix, attachment)| {
                image_tile(
                    Some(attachment.image.clone()),
                    rems(64. / 16.),
                    rems(64. / 16.),
                )
                .child(number_badge(ix + 1))
                .child(remove_button(
                    ("attachment-remove", ix),
                    cx.listener(move |this, _, window, cx| this.remove_attachment(ix, window, cx)),
                ))
            })
            .collect();
        let pastes = self.pasted.iter().enumerate().map(|(ix, paste)| {
            pasted_card(ix + 1, paste).child(remove_button(
                ("pasted-remove", ix),
                cx.listener(move |this, _, window, cx| this.remove_pasted(ix, window, cx)),
            ))
        });
        Some(
            h_flex()
                .w_full()
                .flex_wrap()
                .gap_2()
                .children(images)
                .children(pastes),
        )
    }

    /// The text field; a click anywhere in it focuses the text area.
    fn render_text_field(&self, ready: bool, cx: &mut Context<Self>) -> Div {
        let weak = cx.entity().downgrade();
        div()
            .min_w_0()
            .text_size(rems(14. / 16.))
            .cursor_text()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    this.composer
                        .update(cx, |state, cx| state.focus(window, cx));
                }),
            )
            .child(
                Textarea::new(&self.composer)
                    .appearance(false)
                    .disabled(!ready)
                    // Pasted texts' tokens, as file-icon `#N` chips.
                    .token(|token, _, _| {
                        let chip = InputToken::new(token)
                            .icon(IconName::FileText)
                            .text_color(ui::info());
                        if token.is_selected() {
                            chip
                        } else {
                            chip.bg(ui::info().opacity(0.12))
                                .border_color(ui::info().opacity(0.22))
                        }
                    })
                    .on_paste(move |item, window, cx| {
                        weak.update(cx, |this, cx| this.paste_clipboard(item, window, cx))
                            .unwrap_or(false)
                    }),
            )
    }

    /// The text grew (you typed): blow a small kiss from the caret. At most
    /// one every `KISS_GAP` and `KISSES_MAX` alive, so it stays a sprinkle.
    /// None with reduced motion.
    pub(in crate::features::session) fn blow_kiss(&mut self, cx: &mut Context<Self>) {
        let len = self.composer.read(cx).value().len();
        let typed = len > self.composer_len;
        self.composer_len = len;
        if !typed || cx.reduce_motion() {
            return;
        }
        self.kisses.retain(|(at, _, _)| at.elapsed() < KISS_LIFE);
        let recent = self
            .kisses
            .last()
            .is_some_and(|(at, _, _)| at.elapsed() < KISS_GAP);
        if recent || self.kisses.len() >= KISSES_MAX {
            return;
        }
        let Some(caret) = self.composer.read(cx).prepaint_caret_geometry() else {
            return;
        };
        let bounds = caret.cursor_bounds();
        let at = point(bounds.origin.x, bounds.origin.y);
        self.kisses.push((Instant::now(), at, self.kisses_blown));
        self.kisses_blown += 1;
    }

    /// Drop the kisses that landed and end a finished focus pulse; whether
    /// either still plays (the view then asks for the next frame).
    pub(super) fn composer_animating(&mut self) -> bool {
        self.kisses.retain(|(at, _, _)| at.elapsed() < KISS_LIFE);
        if self.composer_pulse.is_some_and(|at| at.elapsed() >= PULSE) {
            self.composer_pulse = None;
        }
        !self.kisses.is_empty() || self.composer_pulse.is_some()
    }

    /// The kisses in the air: each rises from where it was blown, swaying,
    /// pops in small, and fades. Every fourth one is a heart. Painted in
    /// window coordinates, over the composer.
    fn render_kisses(&self) -> Option<impl IntoElement> {
        if self.kisses.is_empty() {
            return None;
        }
        let kisses = self.kisses.clone();
        Some(
            canvas(
                |_, _, _| {},
                move |_, _, window, _| {
                    for (born, from, i) in &kisses {
                        let age = born.elapsed().as_secs_f32() / KISS_LIFE.as_secs_f32();
                        if age >= 1. {
                            continue;
                        }
                        let i = *i;
                        let travel = 1. - (1. - age).powi(3);
                        let rise = 26. + 18. * noise(i, 2);
                        let drift = (noise(i, 1) * 2. - 1.) * 12.;
                        let sway = (age * 9. + i as f32).sin() * 4.;
                        let at = point(
                            from.x + px(drift * travel + sway),
                            from.y - px(rise * travel),
                        );
                        let grow = ui::spring((age * 3.).min(1.));
                        let dot = (1.1 + 0.4 * noise(i, 3)) * (0.5 + 0.5 * grow);
                        let alpha = if age > 0.55 { (1. - age) / 0.45 } else { 1. };
                        let heart = i % 4 == 3;
                        let (body, line) = if heart {
                            (ui::petal(), ui::petal())
                        } else {
                            (
                                hsla(340. / 360., 0.85, 0.62, 1.),
                                hsla(340. / 360., 0.70, 0.30, 1.),
                            )
                        };
                        paint_sprite(
                            window,
                            if heart { &HEART } else { &LIPS },
                            at,
                            dot,
                            body.opacity(alpha),
                            line.opacity(alpha),
                        );
                    }
                },
            )
            .absolute()
            .size_0(),
        )
    }

    /// Play the focus pulse (none with reduced motion): frames are asked
    /// for until it's over ([`Self::composer_animating`]). Timed from
    /// `composer_pulse` rather than `with_animation`, whose progress is
    /// lost when the session view is reused from cache, which would replay
    /// the pulse on every later redraw.
    pub(in crate::features::session) fn start_composer_pulse(&mut self, cx: &mut Context<Self>) {
        if !cx.reduce_motion() {
            self.composer_pulse = Some(Instant::now());
        }
    }

    /// The composer's box (background, border, shadow), drawn as a layer
    /// behind its content. During the focus pulse it swells outward on
    /// every side and settles back along one smooth arc. Only this layer
    /// moves (it's absolute), so the text, the buttons and everything
    /// around stay put, and nothing stays bigger.
    fn composer_shell(&self) -> Div {
        /// How far the pulse reaches past the box, on every side.
        const REACH: f32 = 6.;
        let t = self
            .composer_pulse
            .map(|at| (at.elapsed().as_secs_f32() / PULSE.as_secs_f32()).min(1.))
            .unwrap_or(1.);
        let out = px(-REACH * (std::f32::consts::PI * ease_in_out(t)).sin());
        div()
            .absolute()
            .top(out)
            .bottom(out)
            .left(out)
            .right(out)
            .rounded_2xl()
            .border_1()
            .border_color(if self.composer_focused {
                ui::petal().opacity(0.45)
            } else {
                ui::border()
            })
            .bg(ui::surface())
            .shadow_lg()
    }

    pub(super) fn render_composer(
        &self,
        rem_size: Pixels,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (prefix, leaf) = ui::split_path(&self.cwd);
        let empty = self.composer.read(cx).value().trim().is_empty() && self.attachments.is_empty();
        let busy = self.busy();
        let ready = self.backend.is_ready();

        let action_button = if self.stoppable() && empty {
            Button::new("stop")
                .small()
                .danger()
                .rounded(px(999.))
                .child(div().size(rems(9. / 16.)).rounded_sm().bg(ui::well()))
                .tooltip(format!("Stop ({})", ui::shortcut(".")))
                .on_click(cx.listener(|this, _, _, cx| this.stop(cx)))
        } else {
            Button::new("send")
                .small()
                .primary()
                .rounded(px(999.))
                .icon(Icon::new(IconName::ArrowUp))
                .tooltip(if busy {
                    "Steer the running agent"
                } else {
                    "Send"
                })
                .disabled(empty || !ready)
                .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx)))
        };

        let repository = self.git.repository.clone();
        let git = self.git.snapshot.as_ref().map(|git| {
            h_flex()
                .id("git-chip")
                .flex_shrink_0()
                .gap_1()
                .text_size(rems(12. / 16.))
                .child(
                    Icon::new(IconName::GitBranch)
                        .xsmall()
                        .text_color(ui::champagne()),
                )
                .child(
                    div()
                        .font_family(ui::MONO)
                        .text_color(ui::champagne())
                        .child(git.branch.clone().unwrap_or_else(|| "detached".into())),
                )
                .when(!git.files.is_empty(), |this| {
                    this.child(
                        div()
                            .font_family(ui::MONO)
                            .text_color(ui::muted())
                            .child(format!("±{}", git.files.len())),
                    )
                })
                .when_some(repository, |this, path| {
                    let line = format!("Repository: {path}");
                    this.tooltip(move |window, cx| Tooltip::new(line.clone()).build(window, cx))
                })
        });

        let width = self.pane_width.get();
        let narrow = width < f32::from(rems(NARROW / 16.).to_pixels(rem_size));
        let bottom = h_flex()
            .w_full()
            .min_w_0()
            .gap_1p5()
            .items_center()
            .child(
                Button::new("attach")
                    .ghost()
                    .xsmall()
                    .text_color(ui::muted())
                    .icon(Icon::new(IconName::Paperclip))
                    .tooltip("Attach images (or paste / drop them)")
                    .disabled(!ready)
                    .on_click(cx.listener(|this, _, window, cx| this.pick_images(window, cx))),
            )
            .child(div().flex_1().min_w_0())
            .child(div().flex_shrink_0().child(action_button));

        // Below the box: the folder, branch and cost on the left; the model
        // and effort pickers as quiet text buttons on the right.
        let below = h_flex()
            .w_full()
            .min_w_0()
            .gap_2()
            .items_center()
            .when(!narrow, |this| {
                this.child(
                    h_flex()
                        .id("cwd")
                        .min_w_0()
                        .flex_shrink(1.)
                        .gap_1()
                        .px_1p5()
                        .py_0p5()
                        .rounded_md()
                        .cursor_pointer()
                        .hover(|s| s.bg(ui::surface_hover()))
                        .text_size(rems(12. / 16.))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .tooltip(|window, cx| Tooltip::new("Open another folder").build(window, cx))
                        .on_click(cx.listener(|this, _, window, cx| this.choose_folder(window, cx)))
                        .child(Icon::new(IconName::Folder).xsmall().text_color(ui::faint()))
                        .child(
                            div()
                                .min_w_0()
                                .overflow_hidden()
                                .text_ellipsis_start()
                                .font_family(ui::MONO)
                                .text_color(ui::faint())
                                .child(prefix),
                        )
                        .child(
                            div()
                                .flex_shrink_0()
                                .font_family(ui::MONO)
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(ui::fg())
                                .child(leaf),
                        ),
                )
                .children(git)
                .when(self.usage.cost > 0.0, |this| {
                    this.child(
                        div()
                            .font_family(ui::MONO)
                            .text_size(rems(11. / 16.))
                            .text_color(ui::faint())
                            .child(format!("${:.2}", self.usage.cost)),
                    )
                })
            })
            .child(div().flex_1().min_w_0())
            .when_some(self.render_plan_limits_chip(cx), |this, chip| {
                this.child(chip)
            })
            .child(
                div()
                    .min_w_0()
                    .flex_shrink(1.)
                    .flex()
                    .justify_end()
                    .child(self.render_model_picker(narrow, cx)),
            )
            .when(
                self.models.current.as_ref().is_some_and(|m| m.reasoning),
                |this| this.child(self.render_effort_picker(narrow, cx)),
            );

        let content = v_flex()
            .relative()
            .w_full()
            .min_w_0()
            .overflow_hidden()
            .gap_2()
            .map(|d| if narrow { d.px_3() } else { d.px_4() })
            .pt_4()
            .pb_2p5()
            .children(self.render_attachments(cx))
            .child(self.render_text_field(ready, cx))
            .child(bottom);
        let composer_box = div()
            .relative()
            .w_full()
            .min_w_0()
            .child(self.composer_shell())
            .child(content);

        v_flex()
            .w_full()
            .min_w_0()
            .max_w(rems(720. / 16.))
            .gap_1p5()
            .children(self.render_slash_menu(cx))
            .children(self.render_dialog(cx))
            .children(self.render_review_confirm(cx))
            .child(self.render_activity(rem_size, cx))
            .children(self.render_context_bar())
            .child(composer_box)
            .child(below)
            .children(self.render_kisses())
            .when(width >= f32::from(rems(TINY / 16.).to_pixels(rem_size)), |this| {
                this.child(
                    h_flex()
                        .h(rems(1.))
                        .min_w_0()
                        .justify_center()
                        .overflow_hidden().text_size(rems(10. / 16.))
                        .text_color(ui::faint())
                        .child(div().min_w_0().truncate().child(if busy {
                            format!("↵ steers · {} stops", ui::shortcut("."))
                        } else if narrow {
                            "↵ sends · ⇧↵ newline · / commands".to_string()
                        } else {
                            format!(
                                "↵ sends · ⇧↵ newline · !cmd runs a shell command · / for commands · {} new session",
                                ui::shortcut("N")
                            )
                        })),
                )
            })
    }
}

/// The × in an attachment's top-right corner.
fn remove_button(
    id: impl Into<ElementId>,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    h_flex()
        .id(id)
        .absolute()
        .top(rems(3. / 16.))
        .right(rems(3. / 16.))
        .size(rems(18. / 16.))
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(ui::well())
        .cursor_pointer()
        .hover(|s| s.bg(ui::danger()))
        .tooltip(|window, cx| Tooltip::new("Remove").build(window, cx))
        .on_click(on_click)
        .child(Icon::new(IconName::X).xsmall().text_color(ui::fg()))
}

/// Pasted text `n` waiting to be sent: a page with its first lines in tiny
/// type, then `#N` and how many lines it has.
fn pasted_card(n: usize, paste: &PastedText) -> Div {
    let lines = match paste.lines {
        1 => "+1 line".to_string(),
        lines => format!("+{lines} lines"),
    };
    h_flex()
        .relative()
        .flex_shrink_0()
        .h(rems(64. / 16.))
        .pl(rems(7. / 16.))
        .pr(rems(30. / 16.))
        .gap_2p5()
        .rounded_lg()
        .border_1()
        .border_color(ui::border_strong())
        .bg(ui::surface())
        .child(
            div()
                .flex_shrink_0()
                .w(rems(38. / 16.))
                .h(rems(48. / 16.))
                .p(rems(4. / 16.))
                .rounded_md()
                .overflow_hidden()
                .border_1()
                .border_color(ui::border())
                .bg(ui::well())
                .font_family(ui::MONO)
                .text_size(rems(5. / 16.))
                .line_height(rems(6. / 16.))
                .text_color(ui::faint())
                .whitespace_nowrap()
                .child(paste.preview.clone()),
        )
        .child(
            v_flex()
                .gap_0p5()
                .child(
                    h_flex()
                        .gap_1()
                        .child(
                            Icon::new(IconName::FileText)
                                .xsmall()
                                .text_color(ui::muted()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(ui::fg())
                                .child(format!("#{n}")),
                        ),
                )
                .child(
                    div()
                        .text_size(rems(11. / 16.))
                        .text_color(ui::muted())
                        .child(lines),
                ),
        )
}
