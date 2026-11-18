//! `Render` for the workspace window.

use super::*;
use gpui_kit::component::Selectable as _;

/// The title bar's height; overlay screens open below it. 40 px so the
/// 32 px actions keep 4 px of air at each edge.
pub(super) const TITLE_BAR_HEIGHT: f32 = 40.;

/// A quiet 32×32 title action with a 16 px icon: every right-hand action of
/// the title bar. The ghost variant's default `Medium` size is exactly that
/// (`size_8` and `size_4`, both rems, so they follow the interface size);
/// the hover tint and the 8 px radius come from the variant and the theme.
fn title_action(id: &'static str, icon: IconName) -> Button {
    Button::new(id).ghost().icon(Icon::new(icon))
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Follow keyboard focus into panes (a terminal focused by its own click).
        if let Some(tab) = self.tab()
            && let Some(inside) = tab.tree.leaves().into_iter().find(|id| {
                self.panes
                    .get(id)
                    .is_some_and(|p| p.item.focus.contains_focused(window, cx))
            })
            && inside != tab.focused
            && let Some(tab) = self.tab_mut()
        {
            tab.focused = inside;
        }
        let sidebar = self.sidebar.clone().filter(|_| self.sidebar_visible);
        let has_sidebar = self.sidebar.is_some();
        // Native web views would cover a screen or a dialog opened over
        // them; device cards stop capturing while covered.
        let covered =
            self.overlay.is_some() || window.has_active_dialog(cx) || window.has_active_sheet(cx);
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
                window.request_animation_frame();
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
            .on_action(cx.listener(Self::on_new_terminal_tab))
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
            .on_action(cx.listener(Self::on_open_workspaces))
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
                                    .gap_2()
                                    .flex_1()
                                    // Width 0: the tabs take what the buttons leave.
                                    .w_0()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .child(self.render_workspace_chip(cx))
                                    .child(self.render_tabs(cx))
                                    .children(self.render_conversation_chip(cx)),
                            )
                            .child(
                                h_flex()
                                    .flex_none()
                                    .gap_2()
                                    // Without this, Windows answers clicks on these
                                    // buttons as title-bar drags (HTCAPTION).
                                    .occlude()
                                    // Tern keeps pane layout in the title bar, so
                                    // panes carry no header; these act on the
                                    // focused pane, grouped on a sunken pill.
                                    .when(self.tab().is_some(), |d| {
                                        d.child(
                                            h_flex()
                                                .gap_1()
                                                .p(rems(2. / 16.))
                                                .rounded(rems(8. / 16.))
                                                .bg(ui::well())
                                                .child(
                                                    title_action(
                                                        "title-tile-pane",
                                                        IconName::LayoutDashboard,
                                                    )
                                                    .tooltip(format!(
                                                        "New pane, tiled ({})",
                                                        ui::platform_shortcut("⌘D", "Ctrl+Shift+D")
                                                    ))
                                                    .on_click(cx.listener(|this, _, window, cx| {
                                                        this.on_tile_pane(&TilePane, window, cx)
                                                    })),
                                                )
                                                .child(
                                                    title_action(
                                                        "title-split-down",
                                                        IconName::Rows2,
                                                    )
                                                    .tooltip(format!(
                                                        "Split down ({})",
                                                        ui::platform_shortcut(
                                                            "⌘⇧D",
                                                            "Ctrl+Shift+E"
                                                        )
                                                    ))
                                                    .on_click(cx.listener(|this, _, window, cx| {
                                                        this.on_split_down(&SplitDown, window, cx)
                                                    })),
                                                )
                                                .child({
                                                    // Names the pane it closes; hovering it
                                                    // marks that pane (see `render_pane`).
                                                    let target = self
                                                        .focused_pane()
                                                        .map(|id| self.pane_title(id))
                                                        .unwrap_or_default();
                                                    let hover = cx.entity().downgrade();
                                                    // The hover is the wrapper's: the button's
                                                    // tooltip already takes its own `on_hover`,
                                                    // and GPUI allows one per element.
                                                    div()
                                                        .id("title-close-pane-hover")
                                                        .on_hover(move |hovered, _, cx| {
                                                            let hovered = *hovered;
                                                            let _ = hover.update(cx, |this, cx| {
                                                                if this.close_hover != hovered {
                                                                    this.close_hover = hovered;
                                                                    cx.notify();
                                                                }
                                                            });
                                                        })
                                                        .child(
                                                            title_action(
                                                                "title-close-pane",
                                                                IconName::X,
                                                            )
                                                            .tooltip(format!(
                                                                "Close \u{201c}{}\u{201d} ({})",
                                                                target.trim_end_matches(" •"),
                                                                ui::shortcut("W")
                                                            ))
                                                            .on_click(cx.listener(
                                                                |this, _, window, cx| {
                                                                    this.close_hover = false;
                                                                    this.on_close_pane(
                                                                        &ClosePane, window, cx,
                                                                    )
                                                                },
                                                            )),
                                                        )
                                                }),
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
                                                        this.open_settings(window, cx)
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
                                        title_action("open-palette", IconName::Command)
                                            .tooltip(format!("Commands ({})", ui::shortcut("K")))
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.on_open_palette(&OpenPalette, window, cx)
                                            })),
                                    )
                                    .child({
                                        // The screens live here at every width (no narrow
                                        // fork anymore): one code path for the menu.
                                        let weak = cx.entity().downgrade();
                                        div()
                                            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                                cx.stop_propagation()
                                            })
                                            .child(
                                                title_action("title-overflow", IconName::Ellipsis)
                                                    .dropdown_menu(move |menu, window, _| {
                                                        type Open = fn(
                                                            &mut Workspace,
                                                            &mut Window,
                                                            &mut Context<Workspace>,
                                                        );
                                                        let entry = |icon: IconName,
                                                                     label: &'static str,
                                                                     detail: &'static str,
                                                                     open: Open| {
                                                            let weak = weak.clone();
                                                            PopupMenuItem::element(
                                                                move |_, _| {
                                                                    menu_row(
                                                                        icon,
                                                                        label,
                                                                        detail,
                                                                        String::new(),
                                                                    )
                                                                },
                                                            )
                                                            .on_click(move |_, window, cx| {
                                                                let _ = weak.update(
                                                                    cx,
                                                                    |this, cx| {
                                                                        open(this, window, cx)
                                                                    },
                                                                );
                                                            })
                                                        };
                                                        menu.min_w(
                                                            rems(260. / 16.)
                                                                .to_pixels(window.rem_size()),
                                                        )
                                                        .item(entry(
                                                            IconName::Settings,
                                                            "Configuration",
                                                            "Settings for this folder",
                                                            |this, window, cx| {
                                                                this.open_settings(window, cx)
                                                            },
                                                        ))
                                                        .item(entry(
                                                            IconName::Route,
                                                            "Profiles",
                                                            "Model and effort per agent",
                                                            |this, window, cx| {
                                                                this.open_profiles(window, cx)
                                                            },
                                                        ))
                                                        .item(entry(
                                                            IconName::Brain,
                                                            "Memory",
                                                            "Engram observations",
                                                            |this, window, cx| {
                                                                this.open_memory(window, cx)
                                                            },
                                                        ))
                                                        .item(entry(
                                                            IconName::RotateCcwClock,
                                                            "Sessions",
                                                            "Resume a past session",
                                                            |this, window, cx| {
                                                                this.open_sessions(window, cx)
                                                            },
                                                        ))
                                                        .item(entry(
                                                            IconName::KeyRound,
                                                            "Providers",
                                                            "Sign-in and API keys",
                                                            |this, window, cx| {
                                                                this.open_providers(window, cx)
                                                            },
                                                        ))
                                                    }),
                                            )
                                    })
                                    .child({
                                        // Configuration stays a visible button: the other
                                        // screens moved into the ⋯ menu at every width, but
                                        // a menu entry can't show selected while its screen
                                        // is open (see design-system.md), so this one keeps
                                        // the lit-and-closes behavior.
                                        let open = self.overlay.as_ref().is_some_and(|(o, _)| {
                                            matches!(o, Overlay::Settings(_))
                                        });
                                        title_action("open-settings", IconName::Settings)
                                            .tooltip("Configuration")
                                            .selected(open)
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                if open {
                                                    this.close_overlay(window, cx)
                                                } else {
                                                    this.open_settings(window, cx)
                                                }
                                            }))
                                    })
                                    .when(has_sidebar, |d| {
                                        d.child(
                                            title_action(
                                                "toggle-sidebar",
                                                if self.sidebar_visible {
                                                    IconName::PanelLeftClose
                                                } else {
                                                    IconName::PanelLeftOpen
                                                },
                                            )
                                            .tooltip(format!(
                                                "Toggle sidebar ({})",
                                                ui::shortcut("B")
                                            ))
                                            .on_click(
                                                cx.listener(|this, _, _, cx| {
                                                    this.toggle_sidebar(cx)
                                                }),
                                            ),
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
                    .children(sidebar.map(|view| {
                        ui::appear(
                            "sidebar-in",
                            div()
                                .w(rems(SIDEBAR_WIDTH / 16.))
                                .flex_none()
                                .h_full()
                                .rounded_xl()
                                .bg(ui::panel())
                                .border_1()
                                .border_color(gpui_kit::transparent_black())
                                .overflow_hidden()
                                .child(view.cached(StyleRefinement::default().size_full())),
                        )
                    }))
                    .child(
                        div()
                            .relative()
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .child(self.render_body(window, cx)),
                    )
                    .when(devices_docked, |row| row.child(self.devices.clone())),
            )
            .children(self.stages.iter().filter_map(Stage::element))
            .when(devices_floating, |root| root.child(self.devices.clone()))
            .children(self.overlay.as_ref().map(|(overlay, _)| {
                let (view, full_window) = match overlay {
                    Overlay::Providers(view) => (view.clone().into_any_element(), false),
                    Overlay::Sessions(view) => (view.clone().into_any_element(), false),
                    Overlay::Palette(view) => (view.clone().into_any_element(), false),
                    Overlay::Usage(view) => (view.clone().into_any_element(), false),
                    Overlay::Memory(view) => (view.clone().into_any_element(), false),
                    Overlay::Docs(view) => (view.clone().into_any_element(), false),
                    Overlay::Settings(view) => (view.clone().into_any_element(), false),
                    Overlay::Profiles(view) => (view.clone().into_any_element(), false),
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
                    title_action("title-devices", IconName::Smartphone)
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
