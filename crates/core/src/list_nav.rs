//! Pure list selection helpers for keyboard navigation.
//!
//! All helpers clamp at the ends of the list (vim-style) and only land on
//! rows marked as selectable.

/// Index of the first selectable row, if any.
pub fn first(selectable: &[bool]) -> Option<usize> {
    selectable.iter().position(|&ok| ok)
}

/// Index of the last selectable row, if any.
pub fn last(selectable: &[bool]) -> Option<usize> {
    selectable.iter().rposition(|&ok| ok)
}

/// Move one selectable row down from `current`, clamping at the last one.
/// `current` is `None` when no row is selected yet.
pub fn next(current: Option<usize>, selectable: &[bool]) -> Option<usize> {
    if selectable.is_empty() {
        return None;
    }
    let start = current.map(|ix| ix + 1).unwrap_or(0);
    selectable
        .iter()
        .enumerate()
        .skip(start)
        .find_map(|(ix, ok)| (*ok).then_some(ix))
        .or_else(|| last(selectable))
}

/// Move one selectable row up from `current`, clamping at the first one.
pub fn prev(current: Option<usize>, selectable: &[bool]) -> Option<usize> {
    if selectable.is_empty() {
        return None;
    }
    let start = current.unwrap_or(selectable.len());
    let start = start.saturating_sub(1);
    selectable
        .iter()
        .enumerate()
        .take(start + 1)
        .rfind(|&(_, ok)| *ok)
        .map(|(ix, _)| ix)
        .or_else(|| first(selectable))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_list_has_no_selection() {
        assert_eq!(first(&[]), None);
        assert_eq!(last(&[]), None);
        assert_eq!(next(None, &[]), None);
        assert_eq!(prev(None, &[]), None);
    }

    #[test]
    fn first_and_last_skip_non_selectable_rows() {
        assert_eq!(first(&[false, true, false]), Some(1));
        assert_eq!(last(&[false, true, false]), Some(1));
    }

    #[test]
    fn next_from_none_selects_first() {
        assert_eq!(next(None, &[false, true, true]), Some(1));
    }

    #[test]
    fn prev_from_none_selects_last() {
        assert_eq!(prev(None, &[true, true, false]), Some(1));
    }

    #[test]
    fn next_clamps_at_last_selectable() {
        let rows = &[true, true, true];
        assert_eq!(next(Some(0), rows), Some(1));
        assert_eq!(next(Some(1), rows), Some(2));
        assert_eq!(next(Some(2), rows), Some(2));
    }

    #[test]
    fn prev_clamps_at_first_selectable() {
        let rows = &[true, true, true];
        assert_eq!(prev(Some(2), rows), Some(1));
        assert_eq!(prev(Some(1), rows), Some(0));
        assert_eq!(prev(Some(0), rows), Some(0));
    }

    #[test]
    fn next_skips_non_selectable_rows() {
        let rows = &[true, false, true, false, true];
        assert_eq!(next(Some(0), rows), Some(2));
        assert_eq!(next(Some(2), rows), Some(4));
        assert_eq!(next(Some(4), rows), Some(4));
    }

    #[test]
    fn prev_skips_non_selectable_rows() {
        let rows = &[true, false, true, false, true];
        assert_eq!(prev(Some(4), rows), Some(2));
        assert_eq!(prev(Some(2), rows), Some(0));
        assert_eq!(prev(Some(0), rows), Some(0));
    }
}
