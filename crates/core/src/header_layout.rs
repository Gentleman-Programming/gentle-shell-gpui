//! Space allocation for adaptive headers, in logical units at 100% UI size.

/// Allocate optional content only after reserving essential actions, padding
/// and a usable primary label. Each caller supplies its own measured costs.
pub struct HeaderBudget {
    remaining: f32,
}

impl HeaderBudget {
    pub fn new(width: f32, reserved: f32) -> Self {
        Self {
            remaining: (width - reserved).max(0.),
        }
    }

    /// Claim an optional item and its gap, in descending priority order.
    /// A rejected item does not consume space.
    pub fn take(&mut self, cost: f32) -> bool {
        if cost.is_finite() && cost >= 0. && cost <= self.remaining {
            self.remaining -= cost;
            true
        } else {
            false
        }
    }
}

/// Whether a title and its fixed action group can share a row. Otherwise
/// the actions need their own row, not a clipped or scaled-down button.
pub fn needs_action_row(width: f32, reserved: f32, minimum_title: f32) -> bool {
    width < reserved + minimum_title
}

#[cfg(test)]
mod tests {
    use super::{HeaderBudget, needs_action_row};

    #[test]
    fn optional_content_never_uses_the_reserved_space() {
        let mut budget = HeaderBudget::new(320., 220.);
        assert!(!budget.take(120.));
        assert!(budget.take(60.));
        assert!(budget.take(40.));
        assert!(!budget.take(1.));
    }

    #[test]
    fn narrow_headers_omit_optional_items_and_stack_actions() {
        let mut budget = HeaderBudget::new(150., 180.);
        assert!(!budget.take(1.));
        assert!(needs_action_row(150., 180., 48.));
        assert!(needs_action_row(227., 180., 48.));
        assert!(!needs_action_row(228., 180., 48.));
        assert!(!needs_action_row(320., 180., 48.));
    }

    #[test]
    fn a_wider_header_restores_optional_content_in_priority_order() {
        let visible = |width| {
            let mut budget = HeaderBudget::new(width, 200.);
            [budget.take(80.), budget.take(40.), budget.take(30.)]
        };
        assert_eq!(visible(200.), [false, false, false]);
        assert_eq!(visible(280.), [true, false, false]);
        assert_eq!(visible(350.), [true, true, true]);
    }
}
