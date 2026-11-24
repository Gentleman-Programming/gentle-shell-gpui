//! Tab placement and the tab operations the vertical sidebar shares with the
//! horizontal strip. It shares the window's existing tabs, panes and
//! persisted workspaces; no second tab model is created. The sidebar itself
//! is drawn in `sidebars/`.

use super::*;

fn platform_select_modifier(event: &ClickEvent, cx: &App) -> bool {
    event.modifiers().secondary(cx)
}

impl Workspace {
    pub(super) fn toggle_tab_layout(&mut self, cx: &mut Context<Self>) {
        let next = match self.tab_layout {
            TabLayout::Horizontal => TabLayout::Vertical,
            TabLayout::Vertical => TabLayout::Horizontal,
        };
        self.set_tab_layout(next, cx);
    }

    /// All presentation controls share one idempotent, persisted state change.
    pub(super) fn set_tab_layout(&mut self, layout: TabLayout, cx: &mut Context<Self>) {
        if self.tab_layout == layout {
            return;
        }
        self.cancel_workspace_rename_state(cx);
        self.tab_layout = layout;
        // File browsing is a separate right sidebar; switching tab placement
        // must neither open nor hide that independent panel.
        self.schedule_save(cx);
        cx.notify();
    }

    /// The horizontal strip and the sidebar use the same placement menu.
    pub(super) fn tab_placement_menu(
        menu: PopupMenu,
        layout: TabLayout,
        workspace: WeakEntity<Self>,
    ) -> PopupMenu {
        let horizontal = workspace.clone();
        menu.item(
            PopupMenuItem::new("Horizontal tabs")
                .icon(Icon::new(IconName::List))
                .checked(layout == TabLayout::Horizontal)
                .on_click(move |_, _, cx| {
                    let _ = horizontal.update(cx, |this, cx| {
                        this.set_tab_layout(TabLayout::Horizontal, cx)
                    });
                }),
        )
        .item(
            PopupMenuItem::new("Vertical tabs")
                .icon(Icon::new(IconName::PanelLeft))
                .checked(layout == TabLayout::Vertical)
                .on_click(move |_, _, cx| {
                    let _ = workspace
                        .update(cx, |this, cx| this.set_tab_layout(TabLayout::Vertical, cx));
                }),
        )
    }

    /// Keep the selected tab stable when rows move, and pins ahead of other
    /// tabs. Works on any workspace's tabs, built or not.
    fn with_reordered_tabs(
        &mut self,
        space: usize,
        change: impl FnOnce(&mut Vec<Tab>),
        cx: &mut Context<Self>,
    ) {
        let Some(space) = self.spaces.get_mut(space) else {
            return;
        };
        reorder_keeping_active(space, change);
        self.schedule_save(cx);
        cx.notify();
    }

    fn toggle_tab_pin(&mut self, space: usize, id: usize, cx: &mut Context<Self>) {
        self.with_reordered_tabs(space, |tabs| toggle_pin(tabs, id), cx);
    }

    fn move_tab_by(&mut self, space: usize, id: usize, direction: isize, cx: &mut Context<Self>) {
        self.with_reordered_tabs(space, |tabs| move_by(tabs, id, direction), cx);
    }

    /// One of the sidebar footer's two new-tab tiles: a `surface` button the
    /// height of a field with its icon and label, lifting to `fg` on hover;
    /// the tooltip names the tab and its shortcut.
    pub(super) fn new_tab_tile(
        id: &'static str,
        icon: IconName,
        label: &'static str,
        keys: &'static str,
        tooltip: &'static str,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> impl IntoElement {
        let tooltip = SharedString::from(format!("{tooltip} ({keys})"));
        h_flex()
            .id(id)
            .flex_1()
            .min_w_0()
            .h(rems(34. / 16.))
            .px_2()
            .gap_2()
            .justify_center()
            .rounded_lg()
            .bg(ui::surface())
            .cursor_pointer()
            .text_size(rems(12.5 / 16.))
            .text_color(ui::muted())
            .hover(|s| s.bg(ui::surface_hover()).text_color(ui::fg()))
            .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
            .on_click(on_click)
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .child(Icon::new(icon).small())
            .child(div().min_w_0().truncate().child(label))
    }

    /// Pin, rename, move and close one tab of the shown workspace. Sidebar
    /// rows keep Move up / Move down; horizontal tabs use `compact` to omit
    /// them.
    pub(super) fn tab_actions_menu(
        menu: PopupMenu,
        weak: WeakEntity<Self>,
        tab: TabMenu,
    ) -> PopupMenu {
        let TabMenu {
            space,
            id,
            can_rename,
            pinned,
            can_up,
            can_down,
            compact,
        } = tab;
        let (pin, rename, up, down, close) =
            (weak.clone(), weak.clone(), weak.clone(), weak.clone(), weak);
        let mut menu = menu
            .item(
                PopupMenuItem::new(if pinned { "Unpin tab" } else { "Pin tab" })
                    .icon(Icon::new(if pinned {
                        IconName::PinOff
                    } else {
                        IconName::Pin
                    }))
                    .on_click(move |_, _, cx| {
                        let _ = pin.update(cx, |this, cx| this.toggle_tab_pin(space, id, cx));
                    }),
            )
            .item(
                PopupMenuItem::new("Rename")
                    .icon(Icon::new(IconName::Pencil))
                    // Renaming needs a live session, which an unbuilt tab
                    // doesn't have (and building it just to rename isn't worth it).
                    .disabled(!can_rename)
                    .on_click(move |_, window, cx| {
                        let _ = rename.update(cx, |this, cx| {
                            if let Some(i) = this.space().tabs.iter().position(|t| t.id == id) {
                                this.start_tab_rename(i, window, cx);
                            }
                        });
                    }),
            );
        if !compact {
            menu = menu
                .separator()
                .item(
                    PopupMenuItem::new("Move up")
                        .icon(Icon::new(IconName::ArrowUp))
                        .disabled(!can_up)
                        .on_click(move |_, _, cx| {
                            let _ = up.update(cx, |this, cx| this.move_tab_by(space, id, -1, cx));
                        }),
                )
                .item(
                    PopupMenuItem::new("Move down")
                        .icon(Icon::new(IconName::ArrowDown))
                        .disabled(!can_down)
                        .on_click(move |_, _, cx| {
                            let _ = down.update(cx, |this, cx| this.move_tab_by(space, id, 1, cx));
                        }),
                );
        }
        menu.separator().item(
            PopupMenuItem::new("Close tab")
                .icon(Icon::new(IconName::X))
                .on_click(move |_, window, cx| {
                    let _ = close.update(cx, |this, cx| {
                        let index = this
                            .spaces
                            .get(space)
                            .and_then(|s| s.tabs.iter().position(|t| t.id == id));
                        if let Some(i) = index {
                            this.close_tab_in_space(space, i, window, cx);
                        }
                    });
                }),
        )
    }

    /// Right-click on empty space of the tab area: create a workspace or
    /// switch between horizontal and vertical tabs.
    pub(super) fn empty_area_menu(
        menu: PopupMenu,
        weak: WeakEntity<Self>,
        layout: TabLayout,
    ) -> PopupMenu {
        let new_space = weak.clone();
        let menu = menu
            .item(
                PopupMenuItem::new("New workspace")
                    .icon(Icon::new(IconName::FolderPlus))
                    .on_click(move |_, window, cx| {
                        let _ = new_space
                            .update(cx, |this, cx| this.pick_workspace_folder(None, window, cx));
                    }),
            )
            .separator();
        Self::tab_placement_menu(menu, layout, weak)
    }

    // ── Tab multi-selection ───────────────────────────────────────────────

    pub(super) fn is_tab_selected(&self, space: usize, id: usize) -> bool {
        self.tab_selection
            .as_ref()
            .filter(|s| s.space == space)
            .is_some_and(|s| s.selected.contains(&id))
    }

    pub(super) fn selected_tab_ids(&self, space: usize) -> Vec<usize> {
        self.tab_selection
            .as_ref()
            .filter(|s| s.space == space)
            .map(|s| s.selected.iter().copied().collect())
            .unwrap_or_default()
    }

    pub(super) fn clear_tab_selection(&mut self, cx: &mut Context<Self>) {
        if self.tab_selection.take().is_some() {
            cx.notify();
        }
    }

    /// Drop selected ids that no longer exist in their workspace; a closed
    /// anchor falls back to the shown tab. Call after anything that may have
    /// removed tabs.
    pub(super) fn prune_tab_selection(&mut self, cx: &mut Context<Self>) {
        let Some(sel) = self.tab_selection.as_ref() else {
            return;
        };
        let pruned = self.spaces.get(sel.space).and_then(|space| {
            let existing: Vec<usize> = space.tabs.iter().map(|t| t.id).collect();
            let active = space.tabs.get(space.active).map(|t| t.id);
            pruned_selection(sel, &existing, active)
        });
        self.tab_selection = pruned;
        cx.notify();
    }

    /// A left click on a tab of the shown workspace. Modifiers decide before
    /// the click count, so a Cmd/Ctrl/Shift double-click never renames.
    /// `visible` are the tab ids shown (the sidebar's search filter), for
    /// range selection; `None` means every tab of the workspace.
    pub(super) fn on_tab_click(
        &mut self,
        index: usize,
        event: &ClickEvent,
        visible: Option<Vec<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gentle_core::workspaces::TabClick;
        let platform = platform_select_modifier(event, cx);
        let shift = event.modifiers().shift;
        match gentle_core::workspaces::tab_click_action(event.click_count(), platform, shift) {
            TabClick::Rename => self.start_tab_rename(index, window, cx),
            TabClick::Ignore => {}
            TabClick::Select | TabClick::Activate => {
                self.apply_tab_click(index, platform, shift, visible.as_deref(), window, cx)
            }
        }
    }

    /// Apply a click on tab `index` of the shown workspace to the selection
    /// (and show the tab on an ordinary click).
    pub(super) fn apply_tab_click(
        &mut self,
        index: usize,
        platform: bool,
        shift: bool,
        visible: Option<&[usize]>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let space = self.current;
        let Some(tab_id) = self.space().tabs.get(index).map(|t| t.id) else {
            return;
        };
        let tabs: Vec<usize> = match visible {
            Some(visible) => visible.to_vec(),
            None => self.space().tabs.iter().map(|t| t.id).collect(),
        };
        let (selected, anchor) = self
            .tab_selection
            .as_ref()
            .filter(|s| s.space == space)
            .map(|s| {
                (
                    s.selected.iter().copied().collect::<Vec<_>>(),
                    Some(s.anchor),
                )
            })
            .unwrap_or_default();
        let update =
            gentle_core::workspaces::select_tab(&tabs, &selected, anchor, tab_id, platform, shift);
        // An ordinary click empties the selection but keeps the clicked tab as
        // the anchor, so a later Shift-click selects a range from it.
        self.tab_selection = Some(TabSelection {
            space,
            anchor: update.anchor.unwrap_or(tab_id),
            selected: update.selected.iter().copied().collect(),
        });
        if update.activate {
            self.activate(index, window, cx);
        }
        cx.notify();
    }

    /// Pin or unpin every selected tab of `space`. The decision is
    /// deterministic: if any selected tab is unpinned, pin all of them;
    /// otherwise unpin all of them.
    pub(super) fn toggle_selected_tab_pins(
        &mut self,
        space: usize,
        ids: Vec<usize>,
        cx: &mut Context<Self>,
    ) {
        self.with_reordered_tabs(space, |tabs| pin_tabs(tabs, &ids), cx);
    }

    /// Close every selected tab of `space`. Built tabs follow the existing
    /// confirmation rules; unbuilt tabs close without confirmation and are
    /// never built.
    pub(super) fn close_selected_tabs(
        &mut self,
        space: usize,
        ids: Vec<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(space_ref) = self.spaces.get(space) else {
            return;
        };
        let mut built_leaves: Vec<PaneId> = Vec::new();
        let mut to_close: Vec<usize> = Vec::new();
        for tab in &space_ref.tabs {
            if ids.contains(&tab.id) {
                to_close.push(tab.id);
                if tab.is_built() {
                    built_leaves.extend(tab.leaves());
                }
            }
        }
        if to_close.is_empty() {
            return;
        }

        let unsaved = self.unsaved_titles(built_leaves.clone(), cx);
        let busy = self.busy_titles(built_leaves);

        if unsaved.is_empty() && busy.is_empty() {
            self.close_tab_ids_now(space, &to_close, window, cx);
        } else {
            self.confirm_close(unsaved, busy, window, cx, move |this, window, cx| {
                // Recompute which selected tabs still exist; the dialog may
                // have stayed open across other changes.
                let mut remaining: Vec<usize> = Vec::new();
                if let Some(s) = this.spaces.get(space) {
                    for tab in &s.tabs {
                        if ids.contains(&tab.id) {
                            remaining.push(tab.id);
                        }
                    }
                }
                this.close_tab_ids_now(space, &remaining, window, cx);
            });
        }
    }

    /// Remember every close of a batch (`(index, built)` per tab) so reopen
    /// can undo it, lowest index last pushed so it reopens first.
    pub(super) fn remember_closes(&mut self, space: usize, removals: &[(usize, bool)], cx: &App) {
        let order = gentle_core::workspaces::close_stack_order(
            &removals.iter().map(|(i, _)| *i).collect::<Vec<_>>(),
        );
        for idx in &order {
            let Some(is_built) = removals.iter().find(|(i, _)| i == idx).map(|(_, b)| *b) else {
                continue;
            };
            let Some(tab) = self.spaces.get(space).and_then(|s| s.tabs.get(*idx)) else {
                continue;
            };
            if is_built {
                let leaves = tab.leaves();
                self.remember_close(space, *idx, &leaves, cx);
            } else if let Some(saved) = tab.state.unbuilt().cloned() {
                let title = saved
                    .layout
                    .leaves()
                    .get(
                        saved
                            .focused
                            .min(saved.layout.leaves().len().saturating_sub(1)),
                    )
                    .map(super::sidebars::saved_title)
                    .unwrap_or_default();
                if let Some(closed) =
                    closed::Closed::unbuilt(space, tab.id, *idx, tab.pinned, saved, title)
                {
                    if self.closed.len() == closed::KEEP {
                        self.closed.remove(0);
                    }
                    self.closed.push(closed);
                }
            }
        }
    }

    /// Close a set of tabs by id, built or unbuilt, in one batch.
    fn close_tab_ids_now(
        &mut self,
        space: usize,
        ids: &[usize],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(space_ref) = self.spaces.get(space) else {
            return;
        };
        let mut removals: Vec<(usize, bool)> = Vec::new();
        for (i, tab) in space_ref.tabs.iter().enumerate() {
            if ids.contains(&tab.id) {
                removals.push((i, tab.is_built()));
            }
        }
        if removals.is_empty() {
            return;
        }

        let original_len = space_ref.tabs.len();
        let original_active = space_ref.active;

        self.remember_closes(space, &removals, cx);

        // Remove from highest index to lowest so indices stay valid.
        let indices: Vec<usize> = removals.iter().map(|(i, _)| *i).collect();
        for idx in gentle_core::workspaces::close_stack_order(&indices) {
            let tab = self.spaces[space].tabs.remove(idx);
            if tab.is_built() {
                for pane in tab.leaves() {
                    self.panes.remove(&pane);
                }
            }
        }

        self.finish_multi_tab_close(space, original_len, original_active, &removals, window, cx);
    }

    fn finish_multi_tab_close(
        &mut self,
        space: usize,
        original_len: usize,
        original_active: usize,
        removals: &[(usize, bool)],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let removed: Vec<usize> = removals.iter().map(|(i, _)| *i).collect();
        self.spaces[space].active =
            gentle_core::workspaces::active_after_removals(original_len, original_active, &removed);
        self.prune_tab_selection(cx);
        self.announce_close(window, cx);
        if space == self.current {
            if self.space().tabs.is_empty() {
                let root = self.space().root.clone();
                self.open_tab(PaneKind::Agent, root, None, None, window, cx);
                self.save_now(cx);
            } else {
                self.activate(self.space().active, window, cx);
            }
        } else {
            self.schedule_save(cx);
            cx.notify();
        }
    }

    /// Build the multi-tab context menu for a selection of 2+ tabs.
    pub(super) fn multi_tab_actions_menu(
        menu: PopupMenu,
        weak: WeakEntity<Self>,
        selection: MultiTabMenu,
    ) -> PopupMenu {
        let MultiTabMenu {
            space,
            ids,
            any_unpinned,
            count,
        } = selection;
        let pin_ids = ids.clone();
        let close_ids = ids;
        let pin = weak.clone();
        let close = weak;
        let pin_label = if any_unpinned {
            format!("Pin {count} tabs")
        } else {
            format!("Unpin {count} tabs")
        };
        menu.item(
            PopupMenuItem::new(pin_label.clone())
                .icon(Icon::new(if any_unpinned {
                    IconName::Pin
                } else {
                    IconName::PinOff
                }))
                .on_click(move |_, _, cx| {
                    let _ = pin.update(cx, |this, cx| {
                        this.toggle_selected_tab_pins(space, pin_ids.clone(), cx)
                    });
                }),
        )
        .separator()
        .item(
            PopupMenuItem::new(format!("Close {count} tabs"))
                .icon(Icon::new(IconName::X))
                .on_click(move |_, window, cx| {
                    let _ = close.update(cx, |this, cx| {
                        this.close_selected_tabs(space, close_ids.clone(), window, cx)
                    });
                }),
        )
    }
}

/// A multi-selection of tabs shown in the context menu.
pub(super) struct MultiTabMenu {
    pub space: usize,
    pub ids: Vec<usize>,
    pub any_unpinned: bool,
    pub count: usize,
}

/// The tab a [`Workspace::tab_actions_menu`] acts on, and which moves it allows.
#[derive(Clone, Copy)]
pub(super) struct TabMenu {
    /// The workspace the tab belongs to (not always the shown one).
    pub space: usize,
    pub id: usize,
    /// Rename needs a live agent session: the tab is built, an agent, and in
    /// the shown workspace.
    pub can_rename: bool,
    pub pinned: bool,
    pub can_up: bool,
    pub can_down: bool,
    /// Horizontal tabs omit Move up / Move down.
    pub compact: bool,
}

/// Apply `change` to the space's tabs; the active tab stays the same tab.
fn reorder_keeping_active(space: &mut Space, change: impl FnOnce(&mut Vec<Tab>)) {
    let active = space.tabs.get(space.active).map(|tab| tab.id);
    change(&mut space.tabs);
    if let Some(active) = active {
        space.active = space
            .tabs
            .iter()
            .position(|tab| tab.id == active)
            .unwrap_or(0);
    }
}

/// Flip tab `id`'s pin; it goes to the end of the pinned group, or, when
/// unpinned, to the start of the rest.
fn toggle_pin(tabs: &mut Vec<Tab>, id: usize) {
    let Some(index) = tabs.iter().position(|tab| tab.id == id) else {
        return;
    };
    let mut tab = tabs.remove(index);
    tab.pinned = !tab.pinned;
    let boundary = tabs.iter().take_while(|t| t.pinned).count();
    tabs.insert(boundary, tab);
}

/// Swap tab `id` with its neighbour in `direction`, never across the pinned
/// boundary.
fn move_by(tabs: &mut [Tab], id: usize, direction: isize) {
    let Some(index) = tabs.iter().position(|tab| tab.id == id) else {
        return;
    };
    let Some(target) = index.checked_add_signed(direction) else {
        return;
    };
    if target < tabs.len() && tabs[index].pinned == tabs[target].pinned {
        tabs.swap(index, target);
    }
}

/// Pin or unpin `ids` together: if any is unpinned they all get pinned,
/// otherwise they all get unpinned. Pinned tabs stay ahead, each group in
/// its previous order.
fn pin_tabs(tabs: &mut Vec<Tab>, ids: &[usize]) {
    let Some(pinned) = gentle_core::workspaces::pin_state_for_selection(
        |id| tabs.iter().any(|t| t.id == id && t.pinned),
        ids,
    ) else {
        return;
    };
    for tab in tabs.iter_mut().filter(|t| ids.contains(&t.id)) {
        tab.pinned = pinned;
    }
    let (mut ahead, rest): (Vec<Tab>, Vec<Tab>) = tabs.drain(..).partition(|t| t.pinned);
    ahead.extend(rest);
    *tabs = ahead;
}

/// The selection with its workspace index changed to `space`; none when the
/// workspace is gone.
pub(super) fn remap_selection_space(
    selection: Option<TabSelection>,
    space: Option<usize>,
) -> Option<TabSelection> {
    let mut selection = selection?;
    selection.space = space?;
    Some(selection)
}

/// The selection restricted to the tabs that still exist (`existing`, in
/// order). A closed anchor falls back to the shown tab (`active`).
fn pruned_selection(
    selection: &TabSelection,
    existing: &[usize],
    active: Option<usize>,
) -> Option<TabSelection> {
    let anchor = if existing.contains(&selection.anchor) {
        selection.anchor
    } else {
        active?
    };
    Some(TabSelection {
        space: selection.space,
        anchor,
        selected: selection
            .selected
            .iter()
            .copied()
            .filter(|id| existing.contains(id))
            .collect(),
    })
}

/// Keep the selected tab after removal, or select its neighbour if it closed.
pub(super) fn after_removal(space: &mut Space, index: usize) {
    let active = space.active - usize::from(index < space.active);
    space.active = active.min(space.tabs.len().saturating_sub(1));
}

#[cfg(test)]
mod tests {
    use crate::features::workspaces::fresh_id;
    use std::path::PathBuf;

    use super::{
        Space, Tab, TabState, after_removal, move_by, pin_tabs, pruned_selection,
        remap_selection_space, reorder_keeping_active, toggle_pin,
    };
    use crate::features::workspaces::{SavedPane, SavedTab};
    use crate::shared::pane::{PaneKind, PaneTree};

    fn unbuilt(id: usize, pinned: bool) -> Tab {
        Tab {
            id,
            pinned,
            state: TabState::Unbuilt(SavedTab {
                layout: PaneTree::Leaf(SavedPane {
                    kind: PaneKind::Terminal,
                    cwd: PathBuf::from("/x"),
                    path: None,
                    session: None,
                }),
                pinned,
                focused: 0,
            }),
        }
    }

    fn ids(tabs: &[Tab]) -> Vec<usize> {
        tabs.iter().map(|t| t.id).collect()
    }

    #[test]
    fn closing_above_the_active_tab_keeps_it_selected() {
        let mut space = Space::new(fresh_id([]), "w".into(), PathBuf::from("/w"));
        space.tabs = vec![unbuilt(1, false), unbuilt(2, false), unbuilt(3, false)];
        space.active = 1;
        space.tabs.remove(0);
        after_removal(&mut space, 0);
        assert_eq!(ids(&space.tabs), [2, 3]);
        assert_eq!(space.tabs[space.active].id, 2);
    }

    #[test]
    fn closing_the_last_active_tab_clamps_active() {
        let mut space = Space::new(fresh_id([]), "w".into(), PathBuf::from("/w"));
        space.tabs = vec![unbuilt(1, false), unbuilt(2, false)];
        space.active = 1;
        space.tabs.remove(1);
        after_removal(&mut space, 1);
        assert_eq!(ids(&space.tabs), [1]);
        assert_eq!(space.active, 0);
    }

    /// Pinning and unpinning work on tabs that were never built, keeping
    /// pins ahead of the rest.
    #[test]
    fn pin_moves_an_unbuilt_tab_across_the_pinned_group() {
        let mut tabs = vec![unbuilt(1, true), unbuilt(2, false), unbuilt(3, false)];
        toggle_pin(&mut tabs, 3);
        assert_eq!(ids(&tabs), [1, 3, 2]);
        assert!(tabs[1].pinned);
        toggle_pin(&mut tabs, 1);
        assert_eq!(ids(&tabs), [3, 1, 2]);
        assert!(!tabs[1].pinned);
        toggle_pin(&mut tabs, 99);
        assert_eq!(ids(&tabs), [3, 1, 2]);
    }

    /// Moving swaps neighbours of the same group only, and stops at the ends.
    #[test]
    fn move_swaps_within_a_group_and_stops_at_edges() {
        let mut tabs = vec![unbuilt(1, true), unbuilt(2, false), unbuilt(3, false)];
        move_by(&mut tabs, 2, -1);
        assert_eq!(ids(&tabs), [1, 2, 3], "no crossing the pinned boundary");
        move_by(&mut tabs, 2, 1);
        assert_eq!(ids(&tabs), [1, 3, 2]);
        move_by(&mut tabs, 2, 1);
        assert_eq!(ids(&tabs), [1, 3, 2], "no move past the end");
        move_by(&mut tabs, 1, -1);
        assert_eq!(ids(&tabs), [1, 3, 2], "no move before the start");
    }

    /// Reordering a workspace's tabs never changes which tab is the active one.
    #[test]
    fn reordering_keeps_the_active_tab() {
        let mut space = Space::new(fresh_id([]), "w".into(), PathBuf::from("/w"));
        space.tabs = vec![unbuilt(1, false), unbuilt(2, false), unbuilt(3, false)];
        space.active = 2;
        reorder_keeping_active(&mut space, |tabs| move_by(tabs, 3, -1));
        assert_eq!(ids(&space.tabs), [1, 3, 2]);
        assert_eq!(space.tabs[space.active].id, 3);
        reorder_keeping_active(&mut space, |tabs| toggle_pin(tabs, 2));
        assert_eq!(space.tabs[space.active].id, 3);
    }

    #[test]
    fn pin_tabs_pins_all_when_any_is_unpinned_and_keeps_groups_ordered() {
        let mut tabs = vec![
            unbuilt(1, false),
            unbuilt(2, true),
            unbuilt(3, false),
            unbuilt(4, true),
        ];
        pin_tabs(&mut tabs, &[2, 3]);
        assert_eq!(ids(&tabs), [2, 3, 4, 1]);
        assert!(tabs.iter().take(3).all(|t| t.pinned));
        assert!(!tabs[3].pinned);
        // All selected are pinned now: the same selection unpins together.
        pin_tabs(&mut tabs, &[2, 3]);
        assert_eq!(ids(&tabs), [4, 2, 3, 1]);
        assert!(tabs[0].pinned);
        assert!(tabs.iter().skip(1).all(|t| !t.pinned));
        // Unknown or empty selections change nothing.
        pin_tabs(&mut tabs, &[]);
        assert_eq!(ids(&tabs), [4, 2, 3, 1]);
    }

    #[test]
    fn pin_tabs_through_the_reorder_path_keeps_the_shown_tab() {
        let mut space = Space::new(fresh_id([]), "w".into(), PathBuf::from("/w"));
        space.tabs = vec![unbuilt(1, false), unbuilt(2, false), unbuilt(3, false)];
        space.active = 2;
        reorder_keeping_active(&mut space, |tabs| pin_tabs(tabs, &[2, 3]));
        assert_eq!(ids(&space.tabs), [2, 3, 1]);
        assert_eq!(space.tabs[space.active].id, 3);
    }

    fn selection(space: usize, anchor: usize, selected: &[usize]) -> TabSelection {
        TabSelection {
            space,
            anchor,
            selected: selected.iter().copied().collect(),
        }
    }

    #[test]
    fn selection_follows_its_workspace_or_goes_with_it() {
        let moved = remap_selection_space(Some(selection(2, 7, &[7, 8])), Some(0)).unwrap();
        assert_eq!(moved.space, 0);
        assert_eq!(moved.selected.len(), 2);
        assert!(remap_selection_space(Some(selection(2, 7, &[7])), None).is_none());
        assert!(remap_selection_space(None, Some(1)).is_none());
    }

    #[test]
    fn pruning_keeps_what_exists_and_falls_back_to_the_shown_tab() {
        let sel = selection(0, 1, &[1, 2, 3]);
        let kept = pruned_selection(&sel, &[1, 3, 4], Some(4)).unwrap();
        assert_eq!(kept.anchor, 1);
        assert_eq!(kept.selected, [1, 3].into_iter().collect());
        // The anchor closed: the shown tab takes over; the selection stays.
        let moved = pruned_selection(&sel, &[2, 3, 4], Some(4)).unwrap();
        assert_eq!(moved.anchor, 4);
        assert_eq!(moved.selected, [2, 3].into_iter().collect());
        // Nothing left to anchor on.
        assert!(pruned_selection(&sel, &[], None).is_none());
    }

    // ── Headless: the real workspace operations ───────────────────────────

    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    use gpui_kit::component::input::InputState;
    use gpui_kit::{AppContext as _, Context, Task, TestAppContext, Window, WindowOptions};

    use super::super::{
        DevicePanel, Invisible, Music, Notices, PaneFactories, PaneItem, TabLayout, TabSelection,
        Workspace,
    };

    fn space_with(name: &str, ids: &[usize]) -> Space {
        let mut space = Space::new(fresh_id([]), name.into(), std::env::temp_dir());
        space.tabs = ids.iter().map(|id| unbuilt(*id, false)).collect();
        space
    }

    /// Runs `body` on a workspace with `spaces` shown at `current`, without
    /// startup, saving or a real PTY (terminals are inert views).
    fn with_workspace(
        cx: &mut TestAppContext,
        spaces: Vec<Space>,
        current: usize,
        body: impl FnOnce(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static,
    ) {
        cx.update(gpui_kit::init);
        let built = Rc::new(RefCell::new(0usize));
        let counter = built.clone();
        let terminal = move |_cwd, _window: &mut Window, cx: &mut gpui_kit::App| {
            *counter.borrow_mut() += 1;
            PaneItem {
                kind: PaneKind::Terminal,
                view: cx.new(|_| Invisible).into(),
                focus: cx.focus_handle(),
                title: Rc::new(|_| "Terminal".into()),
                unsaved: None,
            }
        };
        let (handle, _) = cx
            .update(|cx| {
                cx.set_global(PaneFactories {
                    terminal: Some(Rc::new(terminal)),
                    ..Default::default()
                });
                gpui_kit::open_window(WindowOptions::default(), cx, |_, cx| cx.new(|_| Invisible))
            })
            .unwrap();
        cx.update(|cx| {
            handle
                .update(cx, |_, window, cx| {
                    let workspace = cx.new(|cx| Workspace {
                        spaces,
                        current,
                        panes: HashMap::new(),
                        next_id: 100,
                        overlay: None,
                        web_shown: Default::default(),
                        sidebar: None,
                        sidebar_visible: false,
                        tab_layout: TabLayout::Horizontal,
                        sidebar_open_spaces: HashMap::new(),
                        badge_picker: None,
                        open_menu: None,
                        workspace_rename: None,
                        tab_selection: None,
                        tab_search: cx.new(|cx| InputState::new(window, cx)),
                        save_task: Some(Task::ready(())),
                        _title_poll: Task::ready(()),
                        _lifecycle: Vec::new(),
                        notices: cx.new(|_| Notices::default()),
                        renaming: None,
                        surge: None,
                        surge_runs: 0,
                        _surge_end: None,
                        stages: Vec::new(),
                        next_stage: 1,
                        resume_welcome: None,
                        drop_hint: None,
                        closed: Vec::new(),
                        music: cx.new(|cx| Music::new(window, cx)),
                        devices: cx.new(DevicePanel::new),
                    });
                    workspace.update(cx, |workspace, cx| body(workspace, window, cx));
                })
                .unwrap();
        });
    }

    fn sorted(ids: Vec<usize>) -> Vec<usize> {
        let mut ids = ids;
        ids.sort_unstable();
        ids
    }

    /// Deleting a workspace below the shown one, then dragging a workspace,
    /// leaves the selection and the shown workspace pointing at the same
    /// workspaces (it used to index out of range).
    #[gpui::test]
    fn deleting_then_moving_workspaces_keeps_the_selection_consistent(cx: &mut TestAppContext) {
        let spaces = vec![
            space_with("A", &[1, 2]),
            space_with("B", &[3, 4]),
            space_with("C", &[5, 6, 7]),
        ];
        with_workspace(cx, spaces, 2, |ws, window, cx| {
            ws.tab_selection = Some(selection(2, 5, &[5, 6]));
            ws.delete_workspace(0, window, cx);
            assert_eq!(ws.current, 1);
            assert_eq!(ws.space().name, "C");
            let sel = ws.tab_selection.as_ref().expect("selection kept");
            assert_eq!(sel.space, 1);
            assert_eq!(sorted(ws.selected_tab_ids(1)), [5, 6]);

            ws.move_workspace(0, 1, window, cx);
            assert_eq!(ws.spaces[0].name, "C");
            assert_eq!(ws.current, 0);
            assert_eq!(ws.tab_selection.as_ref().unwrap().space, 0);
            assert_eq!(sorted(ws.selected_tab_ids(0)), [5, 6]);
        });
    }

    /// An ordinary click keeps the clicked tab as the anchor, so a later
    /// Shift-click selects the range from it.
    #[gpui::test]
    fn shift_click_after_an_ordinary_click_selects_the_range(cx: &mut TestAppContext) {
        with_workspace(
            cx,
            vec![space_with("A", &[1, 2, 3, 4])],
            0,
            |ws, window, cx| {
                ws.apply_tab_click(1, false, false, None, window, cx);
                assert_eq!(ws.space().active, 1);
                assert!(ws.selected_tab_ids(0).is_empty());
                ws.apply_tab_click(3, false, true, None, window, cx);
                assert_eq!(sorted(ws.selected_tab_ids(0)), [2, 3, 4]);
                // The range follows only the tabs shown by a filter.
                ws.apply_tab_click(0, false, false, None, window, cx);
                ws.apply_tab_click(3, false, true, Some(&[1, 4]), window, cx);
                assert_eq!(sorted(ws.selected_tab_ids(0)), [1, 4]);
            },
        );
    }

    /// A batch close is remembered so reopening restores the lowest index
    /// first.
    #[gpui::test]
    fn a_multi_close_reopens_the_lowest_index_first(cx: &mut TestAppContext) {
        with_workspace(
            cx,
            vec![space_with("A", &[1, 2, 3, 4])],
            0,
            |ws, _window, cx| {
                ws.remember_closes(0, &[(0, false), (2, false)], cx);
                let indices: Vec<usize> = ws.closed.iter().map(|c| c.index).collect();
                // The last one pushed is the first one reopened.
                assert_eq!(indices, [2, 0]);
            },
        );
    }
}
