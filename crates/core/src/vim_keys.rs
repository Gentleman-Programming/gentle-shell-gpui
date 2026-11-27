//! Vim-style key navigation in lists: a user setting, off by default.

use serde_json::Value;

/// Key of the app's `settings.json`.
pub const SETTINGS_KEY: &str = "vimKeys";

/// Whether vim-style navigation keys (j/k/gg/G, s/u/d) are active in the
/// file tree and changes list.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VimKeys {
    /// `true` when the user opted in; arrows and the rest of the app keep
    /// working unchanged.
    pub enabled: bool,
}

impl VimKeys {
    /// Off by default; any non-boolean saved value falls back to off.
    pub fn from_json(value: &Value) -> Self {
        Self {
            enabled: value.as_bool().unwrap_or(false),
        }
    }

    pub fn to_json(self) -> Value {
        Value::Bool(self.enabled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_off() {
        assert!(!VimKeys::default().enabled);
    }

    #[test]
    fn explicit_values_round_trip() {
        assert!(VimKeys::from_json(&Value::Bool(true)).enabled);
        assert!(!VimKeys::from_json(&Value::Bool(false)).enabled);
        assert_eq!(
            VimKeys::from_json(&Value::Bool(true)).to_json(),
            Value::Bool(true)
        );
    }

    #[test]
    fn unknown_or_missing_values_fall_back_to_off() {
        assert!(!VimKeys::from_json(&Value::String("yes".into())).enabled);
        assert!(!VimKeys::from_json(&Value::Null).enabled);
        assert!(!VimKeys::from_json(&Value::Number(1.into())).enabled);
    }
}
