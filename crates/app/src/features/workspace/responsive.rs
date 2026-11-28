//! Effective workspace widths at 100% interface scale.
//! This policy never changes the persisted widths or visibility preferences.

use super::COMPACT_SIDEBAR_WIDTH;
use super::right_sidebar::ACTIVITY_BAR_WIDTH;
use gentle_core::workspaces::SIDEBAR_MIN_WIDTH;

pub(super) const ROW_GAP: f32 = 6.;
pub(super) const ROW_PADDING: f32 = 2. * ROW_GAP;
/// A useful conversation/editor width, where the window can provide it.
pub(super) const WORK_AREA_MIN_WIDTH: f32 = 500.;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Allocation {
    pub(super) tabs: (f32, f32),
    pub(super) files: (f32, f32),
    pub(super) compact: bool,
}

/// The input pairs are animated content widths and gaps, not saved widths.
/// Other docked columns are reserved before distributing the remaining space.
/// Keep the navigation accessible even when the window cannot provide the
/// preferred center minimum; a dock that cannot fit its minimum waits until
/// there is room again. Opening it on demand is a separate presentation policy.
pub(super) fn allocate(
    window: f32,
    rails: usize,
    tabs: (f32, f32),
    files: (f32, f32),
    compact: bool,
    other_docks: f32,
) -> Allocation {
    let room = (window - ROW_PADDING - rails as f32 * (ACTIVITY_BAR_WIDTH + ROW_GAP)
        - other_docks)
        .max(0.);
    let compact_tabs = tabs.0.min(COMPACT_SIDEBAR_WIDTH * tabs.1 / ROW_GAP) + tabs.1;
    let center = WORK_AREA_MIN_WIDTH.min((room - compact_tabs).max(0.));
    let budget = room - center;
    let mut tabs_total = tabs.0 + tabs.1;
    let mut files_total = files.0 + files.1;
    let tabs_min = if compact {
        tabs_total
    } else {
        tabs.0.min(SIDEBAR_MIN_WIDTH * tabs.1 / ROW_GAP) + tabs.1
    };
    let files_min = files.0.min(SIDEBAR_MIN_WIDTH * files.1 / ROW_GAP) + files.1;
    let mut effective_compact = compact;

    // The auxiliary dock yields first. When both panels reach their minimum,
    // compact the navigation without giving the freed width back to the dock.
    let excess = (tabs_total + files_total - budget).max(0.);
    let files_shrink = excess.min((files_total - files_min).max(0.));
    files_total -= files_shrink;
    let tabs_shrink = (excess - files_shrink).min((tabs_total - tabs_min).max(0.));
    tabs_total -= tabs_shrink;

    if tabs_total + files_total > budget + 0.001 && tabs_total > 0. && !compact {
        tabs_total = compact_tabs;
        effective_compact = true;
    }
    if tabs_total + files_total > budget + 0.001 {
        files_total = 0.;
    }
    // Only a window smaller than the compact navigation itself reaches this.
    tabs_total = tabs_total.min(budget);
    let tabs_gap = tabs.1.min(tabs_total);
    let files_gap = files.1.min(files_total);
    Allocation {
        tabs: (tabs_total - tabs_gap, tabs_gap),
        files: (files_total - files_gap, files_gap),
        compact: effective_compact,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(window: f32, rails: usize) -> Allocation {
        allocate(window, rails, (480., ROW_GAP), (480., ROW_GAP), false, 0.)
    }

    fn occupied(a: Allocation) -> f32 {
        a.tabs.0 + a.tabs.1 + a.files.0 + a.files.1
    }

    #[test]
    fn wide_windows_keep_the_requested_widths() {
        let a = layout(1600., 2);
        assert_eq!(a.tabs, (480., ROW_GAP));
        assert_eq!(a.files, (480., ROW_GAP));
        assert!(!a.compact);
    }

    #[test]
    fn the_right_sidebar_shrinks_first_to_preserve_the_chat() {
        let a = layout(1450., 2);
        assert_eq!(a.tabs.0, 480.);
        assert!(a.files.0 < 480. && a.files.0 > SIDEBAR_MIN_WIDTH);
        let a = layout(1100., 2);
        assert_eq!(a.files.0, SIDEBAR_MIN_WIDTH);
        assert!(a.tabs.0 >= SIDEBAR_MIN_WIDTH && a.tabs.0 < 480.);
        assert!(!a.compact);
        assert!(occupied(a) <= 1100. - ROW_PADDING - 96. - WORK_AREA_MIN_WIDTH + 0.01);
    }

    #[test]
    fn right_sidebar_alone_yields_before_the_chat() {
        let a = allocate(900., 0, (0., 0.), (480., ROW_GAP), false, 0.);
        assert_eq!(a.files, (382., ROW_GAP));
        let narrow = allocate(700., 0, (0., 0.), (480., ROW_GAP), false, 0.);
        assert_eq!(narrow.files, (0., 0.));
    }

    #[test]
    fn narrowing_the_window_never_expands_the_right_sidebar() {
        let mut previous = f32::INFINITY;
        for width in (600..=1600).rev() {
            let a = layout(width as f32, 2);
            assert!(a.files.0 <= previous + 0.01);
            previous = a.files.0;
        }
    }

    #[test]
    fn navigation_compacts_before_the_tool_dock_is_suppressed() {
        let a = layout(900., 2);
        assert!(a.compact);
        assert_eq!(a.tabs.0, COMPACT_SIDEBAR_WIDTH);
        assert!(a.files.0 >= SIDEBAR_MIN_WIDTH);
        let narrow = layout(800., 2);
        assert_eq!(narrow.tabs.0, COMPACT_SIDEBAR_WIDTH);
        assert_eq!(narrow.files, (0., 0.));
        let restored = layout(1600., 2);
        assert_eq!(restored.tabs, (480., ROW_GAP));
        assert_eq!(restored.files, (480., ROW_GAP));
        assert!(!restored.compact);
    }

    #[test]
    fn hidden_panels_and_manual_compact_are_respected() {
        let a = allocate(1600., 0, (0., 0.), (260., ROW_GAP), false, 0.);
        assert_eq!(a.tabs, (0., 0.));
        assert_eq!(a.files, (260., ROW_GAP));
        let a = allocate(
            1600.,
            1,
            (COMPACT_SIDEBAR_WIDTH, ROW_GAP),
            (0., 0.),
            true,
            0.,
        );
        assert!(a.compact);
        assert_eq!(a.tabs.0, COMPACT_SIDEBAR_WIDTH);
    }

    #[test]
    fn rails_and_other_docks_reduce_the_shared_budget() {
        let without = layout(1200., 0);
        let with = allocate(1200., 2, (480., ROW_GAP), (480., ROW_GAP), false, 326.);
        assert!(occupied(with) < occupied(without));
    }

    #[test]
    fn animation_and_tiny_windows_never_allocate_negative_widths() {
        for width in [0., 100., 650., 800., 1200.] {
            for fraction in [0., 0.25, 0.5, 1.] {
                let a = allocate(
                    width,
                    2,
                    (480. * fraction, ROW_GAP * fraction),
                    (480. * fraction, ROW_GAP * fraction),
                    false,
                    0.,
                );
                assert!(a.tabs.0 >= 0. && a.files.0 >= 0.);
                assert!(occupied(a) <= (width - ROW_PADDING - 96.).max(0.) + 0.01);
            }
        }
    }
}
