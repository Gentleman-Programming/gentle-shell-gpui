//! How full a conversation's context window is, in the bands the context
//! line over the composer colors: calm while there's room, filling once
//! answers start to degrade with a long context, full when it's time to
//! compact.

/// A band of context use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextFill {
    /// Under half: nothing to watch.
    Calm,
    /// Half to three quarters: worth keeping an eye on.
    Filling,
    /// Three quarters on: compact soon.
    Full,
}

impl ContextFill {
    /// The band `percent` (0..=100) of the window falls in.
    pub fn of(percent: f64) -> Self {
        if percent >= 75. {
            Self::Full
        } else if percent >= 50. {
            Self::Filling
        } else {
            Self::Calm
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ContextFill;

    #[test]
    fn bands_start_at_half_and_three_quarters() {
        assert_eq!(ContextFill::of(0.), ContextFill::Calm);
        assert_eq!(ContextFill::of(49.9), ContextFill::Calm);
        assert_eq!(ContextFill::of(50.), ContextFill::Filling);
        assert_eq!(ContextFill::of(74.9), ContextFill::Filling);
        assert_eq!(ContextFill::of(75.), ContextFill::Full);
        assert_eq!(ContextFill::of(100.), ContextFill::Full);
    }
}
