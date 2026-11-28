//! `Render` for the workspace window.

use super::*;

/// The title bar's height; overlay screens open below it.
pub(super) const TITLE_BAR_HEIGHT: f32 = 36.;

/// Paint and clip the content inside the width allocated by the row.
fn sidebar_surface(side: Side, content: impl IntoElement) -> Div {
    div()
        .size_full()
        .min_w_0()
        .debug_selector(|| match side {
            Side::Tabs => "tabs-slide-surface".into(),
            Side::Files => "files-slide-surface".into(),
        })
        .rounded_xl()
        .bg(ui::panel())
        .border_1()
        .border_color(gpui_kit::transparent_black())
        .overflow_hidden()
        .child(content)
}

fn drag_saved_width(
    action: Option<gentle_core::workspaces::SidebarDrag>,
    pointer: f32,
    window: f32,
    saved: f32,
) -> Option<f32> {
    use gentle_core::workspaces::shown_sidebar_width;
    let Some(gentle_core::workspaces::SidebarDrag::Width(w)) = action else {
        return None;
    };
    // The window-share cap can render a sidebar narrower than its saved width.
    // A drag that still sits at or beyond that rendered edge is not asking to
    // shrink the saved width, so keep it; anything inside the edge resizes.
    let pinned_at_cap = saved > w && w == shown_sidebar_width(saved, window) && pointer >= w;
    (!pinned_at_cap).then_some(w)
}

impl Workspace {
    fn sidebar_handle(&self, side: Side, cx: &mut Context<Self>) -> impl IntoElement {
        let (id, revision) = match side {
            Side::Tabs => ("tabs-resize-handle", self.tabs_sidebar_motion.revision),
            Side::Files => ("files-resize-handle", self.files_sidebar_motion.revision),
        };
        let workspace = cx.entity().downgrade();
        ui::resize_handle(
            id,
            self.sidebar_drag_active(side, cx),
            !cx.has_active_drag(),
        )
        .absolute()
        .top_0()
        .bottom_0()
        .when(matches!(side, Side::Tabs), |d| d.right(rems(-6. / 16.)))
        // Keep the right handle inside its panel's bounds. Cached dock
        // views and adjacent panes must not mask an out-of-bounds hit area.
        .when(matches!(side, Side::Files), |d| {
            d.left_0().w(rems(10. / 16.)).occlude()
        })
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_drag(
            SidebarDrag::new(side, cx.entity_id(), revision),
            move |drag, _, _, cx| {
                let _ = workspace.update(cx, |this, cx| {
                    this.sidebar_motion_mut(side).active_drag =
                        std::rc::Rc::downgrade(&drag.revision);
                    cx.notify();
                });
                cx.new(|_| Invisible)
            },
        )
        .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
            if event.click_count() == 2 {
                let width = match side {
                    Side::Tabs => &mut this.tabs_sidebar_width,
                    Side::Files => &mut this.files_sidebar_width,
                };
                *width = gentle_core::workspaces::SIDEBAR_DEFAULT_WIDTH;
                this.schedule_save(cx);
                cx.notify();
            }
        }))
        .tooltip(|window, cx| {
            Tooltip::new("Drag to resize · double-click to reset").build(window, cx)
        })
    }

    fn resize_sidebar(
        &mut self,
        event: &DragMoveEvent<SidebarDrag>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        // The row survives the handle unmounting. A missed mouse-up can
        // leave GPUI's active payload in place even when left is no longer held.
        let drag = event.drag(cx);
        if drag.owner != cx.entity_id() {
            return;
        }
        let scale = f32::from(window.rem_size()) / 16.;
        let x = event.event.position.x;
        let left_rail = self.layout_profiles.current().has(ToolPlacement::Left);
        let right_rail = self.layout_profiles.current().has(ToolPlacement::Right);
        let (distance, open, revision) = match drag.side {
            Side::Tabs if self.sidebar_compact => return,
            Side::Tabs => (
                x - event.bounds.left(),
                self.navigation_visible(),
                self.tabs_sidebar_motion.revision,
            ),
            Side::Files if self.right_sidebar_view().is_some() => (
                event.bounds.right() - x,
                self.sidebar_visible,
                self.files_sidebar_motion.revision,
            ),
            Side::Files => return,
        };
        // Manual toggles take precedence over any in-flight gesture.
        if drag.revision.get() != revision {
            return;
        }
        // The row's horizontal padding is 6 px at 100% interface scale.
        let rail = if (matches!(drag.side, Side::Files) && right_rail)
            || (matches!(drag.side, Side::Tabs) && left_rail)
        {
            right_sidebar::ACTIVITY_BAR_WIDTH + 6.
        } else {
            0.
        };
        let pointer = f32::from(distance) / scale - 6. - rail;
        let window_width = f32::from(window.viewport_size().width) / scale;
        let effective = self.responsive_geometry(
            window_width,
            scale,
            std::time::Instant::now(),
            cx,
        );
        let side = drag.side;
        let rendered_width = match side {
            Side::Tabs if effective.compact => return,
            Side::Tabs => effective.tabs.0,
            Side::Files => effective.files.0,
        };
        let held = event.event.pressed_button == Some(MouseButton::Left);
        let action = drag
            .gesture
            .borrow_mut()
            .update(pointer, window_width, open, held);
        let drag_revision = drag.revision.clone();
        let saved = match side {
            Side::Tabs => self.tabs_sidebar_width,
            Side::Files => self.files_sidebar_width,
        };
        if rendered_width > 0.
            && !(saved > rendered_width && pointer >= rendered_width)
            && let Some(w) = drag_saved_width(action, pointer, window_width, saved)
        {
            match side {
                Side::Tabs => self.resize_tab_sidebar(w, cx),
                Side::Files if saved != w => {
                    self.files_sidebar_width = w;
                    self.schedule_save(cx);
                    cx.notify();
                }
                Side::Files => {}
            }
        }
        if !held {
            self.sidebar_motion_mut(side).active_drag = Default::default();
            cx.notify();
        }
        drag_revision.set(match side {
            Side::Tabs => self.tabs_sidebar_motion.revision,
            Side::Files => self.files_sidebar_motion.revision,
        });
    }
}

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
        let has_files_sidebar = self.sidebar.is_some();
        let has_right_panel = self.right_sidebar_view().is_some();
        let profile = self.layout_profiles.current();
        let vertical_tabs = profile.vertical_tabs();
        let horizontal_tabs = profile.horizontal_tabs();
        let left_rail = profile.has(ToolPlacement::Left);
        let right_rail = profile.has(ToolPlacement::Right);
        let title_utilities = profile.title_utilities;
        let original_controls = profile.original_controls;
        let profile_has_title = profile.has(ToolPlacement::Title);
        let navigation_visible = self.tabs_sidebar_visible();
        // Agents is owned by a session but uses the same workspace modal layer.
        let agents_session = if self.overlay.is_none() {
            self.agents_overlay_session(cx)
        } else {
            None
        };
        // First-run screens keep their original full-window presentation.
        let workspace_modal = agents_session.is_some()
            || self.overlay.as_ref().is_some_and(|(overlay, _)| {
                !matches!(overlay, Overlay::Onboarding(_) | Overlay::Welcome(_))
            });
        let scale = f32::from(window.rem_size()) / 16.;
        let window_px_at_100 = f32::from(window.viewport_size().width) / scale;
        let now = std::time::Instant::now();
        let tabs_moving = self.tabs_sidebar_motion.advance(now, cx.reduce_motion());
        let files_moving = self.files_sidebar_motion.advance(now, cx.reduce_motion());
        if tabs_moving || (files_moving && has_right_panel) {
            ui::request_animation_frame(window);
        }
        let plan = self.responsive_plan(window_px_at_100, scale, now, cx);
        let geometry = plan.layout;
        let (tabs_width, tabs_gap) = geometry.tabs;
        let (files_width, files_gap) = geometry.files;
        let right_overlay =
            self.sidebar_visible && plan.overlay == Some(responsive::Auxiliary::Right);
        let active_overlay =
            if self.sidebar_visible || plan.overlay != Some(responsive::Auxiliary::Right) {
                plan.overlay
            } else {
                None
            };
        self.files_sidebar_motion.suspended =
            self.sidebar_visible && files_width == 0. && !right_overlay;
        self.apply_responsive_docks(plan, cx);
        let automatic_compact = geometry.compact && !self.sidebar_compact;
        let tabs_resizing = self.sidebar_drag_active(Side::Tabs, cx);
        let files_resizing = self.sidebar_drag_active(Side::Files, cx);
        let files_sidebar = self.right_sidebar_view().filter(|_| files_width > 0.);
        let floating_sidebar = self.right_sidebar_view().filter(|_| right_overlay);
        let rail_width = responsive::ROW_GAP + right_sidebar::ACTIVITY_BAR_WIDTH;
        let overlay_width = self.files_sidebar_width.min(
            (window_px_at_100
                - responsive::ROW_PADDING
                - rail_width * (usize::from(left_rail) + usize::from(right_rail)) as f32
                - tabs_width
                - tabs_gap)
                .max(0.),
        );
        // Native web views would cover a screen or a dialog opened over
        // them, and swallow the pointer while a pane is dragged over them;
        // device cards stop capturing while covered.
        let covered = self.overlay.is_some()
            || agents_session.is_some()
            || window.has_active_dialog(cx)
            || window.has_active_sheet(cx)
            || cx.has_active_drag()
            || crate::features::browser::popovers_open(cx);
        self.set_stages_covered(covered, active_overlay, cx);
        self.sync_web_views(covered || active_overlay.is_some(), cx);
        let (devices_docked, devices_floating) = {
            let panel = self.devices.read(cx);
            let docked = panel.mode() == crate::shared::floating::Mode::Docked;
            let visible = panel.visible() && !panel.responsive_suspended();
            (visible && docked, visible && !docked)
        };
        // Browser cards: floating over the window, or docked in a column at
        // its right, whose divider may be being dragged.
        let floating_stages: Vec<AnyElement> = self
            .stages
            .iter()
            .filter_map(|stage| stage.element(cx))
            .collect();
        let browser_dock = self.render_browser_dock(plan.browser_width * scale, cx);
        let browser_dock_drag = if plan.browser_width > 0. {
            self.browser_dock_drag_layer(cx)
        } else {
            None
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
            // Only an empty workspace needs the root to own the keyboard: a
            // pane's or an overlay's own focusable elements take it in every
            // other state, and an unconditional grab here would steal focus
            // from a pane when the pointer hits blank chrome.
            .when(self.tab().is_none(), |root| root.track_focus(&self.focus))
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
                                    .when(vertical_tabs, |row| {
                                        row.child(self.new_tab_button("title-new-tab", None, cx))
                                    })
                                    .when(horizontal_tabs, |row| {
                                        row.child(
                                            div()
                                                .flex_none()
                                                .w(px(1.))
                                                .h(rems(14. / 16.))
                                                .bg(ui::border()),
                                        )
                                        .child(self.render_tabs(cx))
                                    }),
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
                                    .when(title_utilities && self.tab().is_some(), |d| {
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
                                    .when(title_utilities, |d| {
                                        d.children(crate::features::music::title_button(
                                            &self.music,
                                            cx,
                                        ))
                                        .when(original_controls, |d| {
                                            d.children(self.render_devices_button(cx))
                                        })
                                        .children(self.render_tray(cx))
                                    })
                                    .when(title_utilities, |d| {
                                        d.child({
                                            let weak = cx.entity().downgrade();
                                            div()
                                                .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                                    cx.stop_propagation()
                                                })
                                                .child(notices::bell(
                                                    &self.notices,
                                                    Rc::new({
                                                        let weak = weak.clone();
                                                        move |pane, window, cx| {
                                                            let _ = weak.update(cx, |this, cx| {
                                                                this.reveal_pane(pane, window, cx)
                                                            });
                                                        }
                                                    }),
                                                    Rc::new(move |origin, cx| {
                                                        weak.upgrade().map_or(
                                                            notices::Reach::Open,
                                                            |this| {
                                                                this.read(cx).notice_reach(origin)
                                                            },
                                                        )
                                                    }),
                                                    cx,
                                                ))
                                        })
                                    })
                                    .when(vertical_tabs && !automatic_compact, |d| {
                                        d.child(
                                            Button::new("toggle-tab-sidebar")
                                                .ghost()
                                                .xsmall()
                                                .icon(Icon::new(if self.sidebar_compact {
                                                    IconName::PanelLeftOpen
                                                } else {
                                                    IconName::PanelLeftClose
                                                }))
                                                .tooltip(if self.sidebar_compact {
                                                    "Expand workspace sidebar"
                                                } else {
                                                    "Compact workspace sidebar"
                                                })
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.toggle_sidebar_compact(cx)
                                                })),
                                        )
                                    })
                                    .when(profile_has_title, |d| {
                                        d.child(self.render_title_shortcuts(cx))
                                    })
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
                                    .when(original_controls, |d| {
                                        d.child(self.render_screens_menu(cx))
                                    })
                                    .when(original_controls && has_files_sidebar, |d| {
                                        d.child(
                                            Button::new("toggle-sidebar")
                                                .ghost()
                                                .xsmall()
                                                .icon(Icon::new(if self.sidebar_visible
                                                    && !self.files_sidebar_motion.suspended
                                                {
                                                    IconName::PanelRightClose
                                                } else {
                                                    IconName::PanelRightOpen
                                                }))
                                                .tooltip(format!(
                                                    "{} Files sidebar ({})",
                                                    if self.sidebar_visible
                                                        && !self.files_sidebar_motion.suspended
                                                    {
                                                        "Hide"
                                                    } else {
                                                        "Show"
                                                    },
                                                    ui::shortcut("B")
                                                ))
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.request_sidebar(cx)
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
                    .on_drag_move(cx.listener(
                        |this, event: &DragMoveEvent<SidebarDrag>, window, cx| {
                            this.resize_sidebar(event, window, cx);
                        },
                    ))
                    .when(left_rail, |row| {
                        if workspace_modal {
                            row.child(
                                div()
                                    .w(rems(right_sidebar::ACTIVITY_BAR_WIDTH / 16.))
                                    .flex_none(),
                            )
                        } else {
                            row.child(self.render_activity_bar(ToolPlacement::Left, cx))
                        }
                    })
                    // The vertical tab navigator remains independent of the Files panel.
                    .when(vertical_tabs && tabs_width > 0., |row| {
                        row.child(
                            div()
                                .id("vertical-tab-panel")
                                .debug_selector(|| "tabs-slide-row".into())
                                .w(rems(tabs_width / 16.))
                                .max_w(rems(tabs_width / 16.))
                                .min_w_0()
                                .mr(rems((tabs_gap - 6.) / 16.))
                                .flex_none()
                                .h_full()
                                .relative()
                                .on_mouse_down(
                                    MouseButton::Right,
                                    cx.listener(|this, event: &MouseDownEvent, window, cx| {
                                        cx.stop_propagation();
                                        this.open_empty_area_menu(event.position, window, cx);
                                    }),
                                )
                                .child(sidebar_surface(
                                    Side::Tabs,
                                    self.render_tab_sidebar(geometry.compact, cx),
                                ))
                                .when(
                                    !geometry.compact
                                        && (tabs_resizing || (navigation_visible && !tabs_moving)),
                                    |column| column.child(self.sidebar_handle(Side::Tabs, cx)),
                                ),
                        )
                    })
                    .child(
                        div()
                            .relative()
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .debug_selector(|| "sidebar-adjacent-body".into())
                            .child(self.render_body(window, cx))
                            .child(self.measure_conversation()),
                    )
                    .children(browser_dock)
                    .when(devices_docked, |row| row.child(self.devices.clone()))
                    // Ctrl/Cmd+B toggles the tool content; the rail stays visible.
                    .when_some(files_sidebar, |row, view| {
                        row.child(
                            div()
                                .id("files-sidebar-panel")
                                .debug_selector(|| "files-sidebar-panel".into())
                                .w(rems(files_width / 16.))
                                .max_w(rems(files_width / 16.))
                                .min_w_0()
                                .ml(rems((files_gap - 6.) / 16.))
                                .flex_none()
                                .h_full()
                                .relative()
                                .child(sidebar_surface(
                                    Side::Files,
                                    view.cached(StyleRefinement::default().size_full()),
                                ))
                                .when(
                                    files_resizing || (self.sidebar_visible && !files_moving),
                                    |column| column.child(self.sidebar_handle(Side::Files, cx)),
                                ),
                        )
                    })
                    .when(right_rail, |row| {
                        if workspace_modal {
                            row.child(
                                div()
                                    .w(rems(right_sidebar::ACTIVITY_BAR_WIDTH / 16.))
                                    .flex_none(),
                            )
                        } else {
                            row.child(self.render_activity_bar(ToolPlacement::Right, cx))
                        }
                    }),
            )
            .children(floating_stages)
            .when(devices_floating, |root| root.child(self.devices.clone()))
            .when_some(floating_sidebar, |root, view| {
                root.child(
                    div()
                        .id("responsive-tool-overlay")
                        .absolute()
                        .top(rems((TITLE_BAR_HEIGHT + responsive::ROW_GAP) / 16.))
                        .bottom(rems(responsive::ROW_GAP / 16.))
                        .right(rems((responsive::ROW_GAP
                            + if right_rail { rail_width } else { 0. }) / 16.))
                        .w(rems(overlay_width / 16.))
                        .min_w_0()
                        .occlude()
                        .shadow(crate::shared::floating::shadow(false))
                        .child(sidebar_surface(
                            Side::Files,
                            v_flex()
                                .size_full()
                                .child(
                                    h_flex()
                                        .flex_none()
                                        .justify_end()
                                        .px_1()
                                        .child(
                                            Button::new("close-responsive-tool")
                                                .ghost()
                                                .xsmall()
                                                .icon(Icon::new(IconName::X))
                                                .tooltip("Hide tool")
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.toggle_sidebar(cx);
                                                })),
                                        ),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_h_0()
                                        .min_w_0()
                                        .child(view.cached(StyleRefinement::default().size_full())),
                                ),
                        )),
                )
            })
            .children(self.render_badge_picker(window, cx))
            .children(self.render_context_menu(cx))
            .children(browser_dock_drag)
            // Dim the entire workspace body once. Insetting the scrim along
            // with the modal creates a visible rectangular edge in the gaps
            // beside rounded panels. The rails are painted above this layer.
            .when(workspace_modal, |root| {
                root.child(
                    div()
                        .absolute()
                        .top(rems(TITLE_BAR_HEIGHT / 16.))
                        .left_0()
                        .right_0()
                        .bottom_0()
                        .child(ui::scrim("workspace-modal-scrim")),
                )
            })
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
                // Only modal content and its hit-testing area are inset.
                // The full-width scrim is painted separately above the body.
                div()
                    .absolute()
                    .top(rems(TITLE_BAR_HEIGHT / 16.))
                    .left(rems(if left_rail {
                        (right_sidebar::ACTIVITY_BAR_WIDTH + 6.) / 16.
                    } else {
                        0.
                    }))
                    .right(rems(if right_rail {
                        (right_sidebar::ACTIVITY_BAR_WIDTH + 6.) / 16.
                    } else {
                        0.
                    }))
                    .bottom_0()
                    .child(view)
                    .into_any_element()
            }))
            .children(agents_session.and_then(|session| {
                let view = SessionView::agents_overlay(&session, cx)?;
                Some(
                    div()
                        .absolute()
                        .top(rems(TITLE_BAR_HEIGHT / 16.))
                        .left(rems(if left_rail {
                            (right_sidebar::ACTIVITY_BAR_WIDTH + 6.) / 16.
                        } else {
                            0.
                        }))
                        .right(rems(if right_rail {
                            (right_sidebar::ACTIVITY_BAR_WIDTH + 6.) / 16.
                        } else {
                            0.
                        }))
                        .bottom_0()
                        .child(view),
                )
            }))
            // Keep the same rail bounds as the row, above the continuous
            // scrim. No duplicated rail is mounted in the row in this state.
            .when(workspace_modal && left_rail, |root| {
                root.child(
                    div()
                        .absolute()
                        .top(rems(TITLE_BAR_HEIGHT / 16.))
                        .bottom(rems(6. / 16.))
                        .left(rems(6. / 16.))
                        .w(rems(right_sidebar::ACTIVITY_BAR_WIDTH / 16.))
                        .child(self.render_activity_bar(ToolPlacement::Left, cx)),
                )
            })
            .when(workspace_modal && right_rail, |root| {
                root.child(
                    div()
                        .absolute()
                        .top(rems(TITLE_BAR_HEIGHT / 16.))
                        .bottom(rems(6. / 16.))
                        .right(rems(6. / 16.))
                        .w(rems(right_sidebar::ACTIVITY_BAR_WIDTH / 16.))
                        .child(self.render_activity_bar(ToolPlacement::Right, cx)),
                )
            })
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
        let panel = self.devices.read(cx);
        let open = panel.visible() && !panel.responsive_suspended();
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

#[cfg(test)]
mod tests {
    use super::drag_saved_width;
    use gentle_core::workspaces::{SidebarGesture, shown_sidebar_width};

    #[test]
    fn drag_at_window_cap_preserves_saved_width() {
        // Tabs and Files both measure the pointer as a width from their outer
        // edge, so one rule covers both sidebars.
        let mut gesture = SidebarGesture::default();
        let saved = 420.;
        assert_eq!(shown_sidebar_width(saved, 800.), 320.);
        for pointer in [320., 370.] {
            let action = gesture.update(pointer, 800., true, true);
            assert_eq!(drag_saved_width(action, pointer, 800., saved), None);
        }
        let action = gesture.update(300., 800., true, true);
        assert_eq!(drag_saved_width(action, 300., 800., saved), Some(300.));
    }
}
