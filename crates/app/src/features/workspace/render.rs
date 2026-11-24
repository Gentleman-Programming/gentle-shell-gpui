//! `Render` for the workspace window.

use super::*;

/// The title bar's height; overlay screens open below it.
pub(super) const TITLE_BAR_HEIGHT: f32 = 36.;

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::shared::diagnostics::render("Workspace");
        // Follow keyboard focus into panes (a terminal focused by its own click).
        if let Some(tab) = self.tab()
            && let Some(inside) = tab.leaves().into_iter().find(|id| {
                self.panes
                    .get(id)
                    .is_some_and(|p| p.item.focus.contains_focused(window, cx))
            })
            && Some(inside) != tab.focused()
            && let Some(tab) = self.tab_mut()
        {
            tab.state.set_focused(inside);
        }
        let files_sidebar = self.sidebar.clone().filter(|_| self.sidebar_visible);
        let has_files_sidebar = self.sidebar.is_some();
        let vertical_tabs = self.tab_layout == TabLayout::Vertical;
        // Native web views would cover a screen or a dialog opened over
        // them, and swallow the pointer while a pane is dragged over them;
        // device cards stop capturing while covered.
        let covered = self.overlay.is_some()
            || window.has_active_dialog(cx)
            || window.has_active_sheet(cx)
            || cx.has_active_drag()
            || crate::features::browser::popovers_open(cx);
        self.set_stages_covered(covered, cx);
        self.sync_web_views(covered, cx);
        let (devices_docked, devices_floating) = {
            let panel = self.devices.read(cx);
            let docked = panel.mode() == crate::features::devices::Mode::Docked;
            (panel.visible() && docked, panel.visible() && !docked)
        };
        // While the surge plays, the window shakes before the rose hatches:
        // the root moves in place and asks for the next frame, so the element
        // tree (and every element's state) stays the same.
        let (shake_x, shake_y) = match self.surge {
            Some((_, started, level)) => {
                ui::request_animation_frame(window);
                crate::shared::surge::shake_at(started.elapsed(), level)
            }
            None => (0., 0.),
        };
        v_flex()
            .size_full()
            .text_color(ui::fg())
            // A click in a web page (a device, the browser card) takes the
            // keyboard away from GPUI; any click that reaches GPUI takes it back.
            .capture_any_mouse_down(|_, window, _| {
                crate::features::browser::reclaim_keyboard(window)
            })
            .on_action(cx.listener(Self::on_new_tab))
            .on_action(cx.listener(Self::on_show_in_pane))
            .on_action(cx.listener(Self::on_close_viewer))
            .on_action(cx.listener(Self::on_open_commit_diff))
            .on_action(cx.listener(Self::on_ask_agent))
            .on_action(cx.listener(Self::on_open_terminal_at))
            .on_action(cx.listener(Self::on_new_terminal_tab))
            .on_action(cx.listener(Self::on_new_canvas_tab))
            .on_action(cx.listener(Self::on_close_tab))
            .on_action(cx.listener(Self::on_next_tab))
            .on_action(cx.listener(Self::on_prev_tab))
            .on_action(cx.listener(Self::on_split_right))
            .on_action(cx.listener(Self::on_tile_pane))
            .on_action(cx.listener(Self::on_split_down))
            .on_action(cx.listener(Self::on_new_agent_pane))
            .on_action(cx.listener(Self::on_new_terminal_pane))
            .on_action(cx.listener(Self::on_close_pane))
            .on_action(cx.listener(Self::on_reopen_closed))
            .on_action(cx.listener(Self::on_focus_next_pane))
            .on_action(cx.listener(Self::on_focus_prev_pane))
            .on_action(cx.listener(Self::on_toggle_sidebar))
            .on_action(cx.listener(Self::on_open_providers))
            .on_action(cx.listener(Self::on_open_sessions))
            .on_action(cx.listener(Self::on_open_palette))
            .on_action(cx.listener(Self::on_open_settings))
            .on_action(cx.listener(Self::on_open_workspaces))
            .on_action(cx.listener(Self::on_open_explorer))
            .on_action(cx.listener(Self::on_close_overlay))
            .key_context("GentleWorkspace")
            .relative()
            .left(px(shake_x))
            .top(px(shake_y))
            // The theme's backdrop, first so everything draws over it.
            .children(crate::shared::backdrop::layer(window, cx))
            .child(
                TitleBar::new()
                    .h(rems(TITLE_BAR_HEIGHT / 16.))
                    .bg(gpui_kit::transparent_black())
                    .child(
                        h_flex()
                            // Fill gpui-component's bar without counting toward
                            // its width: `w_full` there resolves against the
                            // content and pushed the buttons out of the window.
                            .flex_1()
                            .w_0()
                            .min_w_0()
                            .pr_2()
                            .gap_3()
                            .justify_between()
                            .child(
                                h_flex()
                                    .id("title-bar-chip-strip")
                                    .gap_2()
                                    .flex_1()
                                    // Width 0: the tabs take what the buttons leave.
                                    .w_0()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .on_mouse_down(
                                        MouseButton::Right,
                                        cx.listener(|this, event: &MouseDownEvent, window, cx| {
                                            cx.stop_propagation();
                                            this.open_empty_area_menu(event.position, window, cx);
                                        }),
                                    )
                                    .child(self.render_workspace_chip(cx))
                                    .child(
                                        div()
                                            .flex_none()
                                            .w(px(1.))
                                            .h(rems(14. / 16.))
                                            .bg(ui::border()),
                                    )
                                    .when(!vertical_tabs, |row| row.child(self.render_tabs(cx))),
                            )
                            .child(
                                h_flex()
                                    .flex_none()
                                    .gap_1()
                                    // Without this, Windows answers clicks on these
                                    // buttons as title-bar drags (HTCAPTION).
                                    .occlude()
                                    .on_mouse_down(MouseButton::Right, |_, _, cx| {
                                        cx.stop_propagation()
                                    })
                                    // Tern keeps pane layout in the title bar, so
                                    // panes carry no header: one menu acts on the
                                    // focused pane, each entry with its shortcut.
                                    .when(self.tab().is_some(), |d| {
                                        d.child(self.render_layout_menu(cx)).child(
                                            div()
                                                .w(px(1.))
                                                .h(rems(14. / 16.))
                                                .mx_1()
                                                .bg(ui::border()),
                                        )
                                    })
                                    .when_some(
                                        crate::features::app_update::badge(cx),
                                        |d, (label, tip)| {
                                            d.child(
                                                div()
                                                    .id("app-update-pill")
                                                    .cursor_pointer()
                                                    .hover(|s| s.opacity(0.8))
                                                    .tooltip(move |window, cx| {
                                                        Tooltip::new(tip.clone()).build(window, cx)
                                                    })
                                                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                                        cx.stop_propagation()
                                                    })
                                                    .on_click(cx.listener(|this, _, window, cx| {
                                                        this.open_app_updates(window, cx)
                                                    }))
                                                    .child(ui::chip(label, ui::accent())),
                                            )
                                        },
                                    )
                                    .children(crate::features::music::title_button(&self.music, cx))
                                    .children(self.render_devices_button(cx))
                                    .children(self.render_tray(cx))
                                    .child({
                                        let weak = cx.entity().downgrade();
                                        div()
                                            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                                cx.stop_propagation()
                                            })
                                            .child(notices::bell(
                                                &self.notices,
                                                Rc::new(move |pane, window, cx| {
                                                    let _ = weak.update(cx, |this, cx| {
                                                        this.reveal_pane(pane, window, cx)
                                                    });
                                                }),
                                                cx,
                                            ))
                                    })
                                    .child(
                                        Button::new("toggle-tab-layout")
                                            .ghost()
                                            .xsmall()
                                            .icon(Icon::new(if vertical_tabs {
                                                IconName::PanelLeft
                                            } else {
                                                IconName::List
                                            }))
                                            .tooltip(if vertical_tabs {
                                                "Use horizontal tabs"
                                            } else {
                                                "Show tabs in the sidebar"
                                            })
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.toggle_tab_layout(cx)
                                            })),
                                    )
                                    .child(
                                        Button::new("open-palette")
                                            .ghost()
                                            .xsmall()
                                            .icon(Icon::new(IconName::Command))
                                            .tooltip(format!("Commands ({})", ui::shortcut("K")))
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.on_open_palette(&OpenPalette, window, cx)
                                            })),
                                    )
                                    // The screens behind one menu; it's lit while
                                    // one of them is open.
                                    .child(self.render_screens_menu(cx))
                                    .when(has_files_sidebar, |d| {
                                        d.child(
                                            Button::new("toggle-sidebar")
                                                .ghost()
                                                .xsmall()
                                                .icon(Icon::new(if self.sidebar_visible {
                                                    IconName::PanelRightClose
                                                } else {
                                                    IconName::PanelRightOpen
                                                }))
                                                .tooltip(format!(
                                                    "{} Files sidebar ({})",
                                                    if self.sidebar_visible {
                                                        "Hide"
                                                    } else {
                                                        "Show"
                                                    },
                                                    ui::shortcut("B")
                                                ))
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.toggle_sidebar(cx)
                                                })),
                                        )
                                    }),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .px_1p5()
                    .pb_1p5()
                    .gap_1p5()
                    // The vertical tab navigator remains independent of the Files panel.
                    .when(vertical_tabs, |row| {
                        row.child(
                            div()
                                .id("vertical-tab-panel")
                                .w(rems(SIDEBAR_WIDTH / 16.))
                                .flex_none()
                                .h_full()
                                .on_mouse_down(
                                    MouseButton::Right,
                                    cx.listener(|this, event: &MouseDownEvent, window, cx| {
                                        cx.stop_propagation();
                                        this.open_empty_area_menu(event.position, window, cx);
                                    }),
                                )
                                .child(ui::appear(
                                    "tabs-sidebar-in",
                                    div()
                                        .size_full()
                                        .rounded_xl()
                                        .bg(ui::panel())
                                        .border_1()
                                        .border_color(gpui_kit::transparent_black())
                                        .overflow_hidden()
                                        .child(self.render_tab_sidebar(cx)),
                                )),
                        )
                    })
                    .child(
                        div()
                            .relative()
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .child(self.render_body(window, cx)),
                    )
                    .when(devices_docked, |row| row.child(self.devices.clone()))
                    // Ctrl/Cmd+B toggles only the right Files/Changes/Branches sidebar.
                    .when_some(files_sidebar, |row, view| {
                        row.child(ui::appear(
                            "files-sidebar-in",
                            div()
                                .w(rems(SIDEBAR_WIDTH / 16.))
                                .flex_none()
                                .h_full()
                                .rounded_xl()
                                .bg(ui::panel())
                                .border_1()
                                .border_color(gpui_kit::transparent_black())
                                .overflow_hidden()
                                .child(
                                    view.cached(StyleRefinement::default().size_full())
                                        .into_any_element(),
                                ),
                        ))
                    }),
            )
            .children(self.stages.iter().filter_map(Stage::element))
            .children(self.render_badge_picker(window, cx))
            .children(self.render_context_menu(cx))
            .when(devices_floating, |root| root.child(self.devices.clone()))
            .children(self.overlay.as_ref().map(|(overlay, _)| {
                let (view, full_window) = match overlay {
                    Overlay::Providers(view) => (view.clone().into_any_element(), false),
                    Overlay::Sessions(view) => (view.clone().into_any_element(), false),
                    Overlay::Palette(view) => (view.clone().into_any_element(), false),
                    Overlay::Explorer(view) => (view.clone().into_any_element(), false),
                    Overlay::Usage(view) => (view.clone().into_any_element(), false),
                    Overlay::Memory(view) => (view.clone().into_any_element(), false),
                    Overlay::Docs(view) => (view.clone().into_any_element(), false),
                    Overlay::Settings(view) => (view.clone().into_any_element(), false),
                    Overlay::Profiles(view) => (view.clone().into_any_element(), false),
                    Overlay::Review(view) => (view.clone().into_any_element(), false),
                    // First-run screens still cover the whole window.
                    Overlay::Onboarding(view) => (view.clone().into_any_element(), true),
                    Overlay::Welcome(view) => (view.clone().into_any_element(), true),
                };
                if full_window {
                    return view;
                }
                // Below the title bar, so its buttons can switch screens.
                div()
                    .absolute()
                    .top(rems(TITLE_BAR_HEIGHT / 16.))
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .child(view)
                    .into_any_element()
            }))
            .when_some(self.surge, |root, (run, _, level)| {
                root.child(crate::shared::surge::overlay(run, level))
            })
            .when_some(crate::features::hydration::playing(cx), |root, run| {
                root.child(crate::features::hydration::overlay(run, cx))
            })
    }
}

impl Workspace {
    /// The title bar's device panel button, while "Use devices" is on.
    fn render_devices_button(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !crate::features::devices::hub(cx).read(cx).prefs().enabled {
            return None;
        }
        let open = self.devices.read(cx).visible();
        Some(
            div()
                // Clicks here are not title-bar drags.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    Button::new("title-devices")
                        .ghost()
                        .xsmall()
                        .icon(Icon::new(IconName::Smartphone))
                        .when(open, |b| b.text_color(ui::petal()))
                        .tooltip("Devices: simulators and emulators")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.devices.update(cx, |panel, cx| panel.toggle(cx))
                        })),
                )
                .into_any_element(),
        )
    }
}
