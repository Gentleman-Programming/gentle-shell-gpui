//! Keystroke display formatting: turn GPUI key binding strings such as
//! `"cmd-shift-t"` into platform-appropriate shortcut hints.
//!
//! The rules match the style already used by the app's palette hints:
//! symbols on macOS (`⌘⇧T`), words elsewhere (`Ctrl+Shift+T`). Multi-
//! keystroke chords are separated by a space.

/// Format a GPUI key binding string for display.
///
/// `binding` is a single keystroke or a space-separated sequence, e.g.
/// `"cmd-shift-t"` or `"ctrl-space h"`. `macos` selects the macOS symbol
/// style; otherwise title-cased modifier words with `"+"` separators are
/// used.
///
/// # Examples
///
/// ```
/// use gentle_core::keyboard::format_binding;
/// assert_eq!(format_binding("cmd-shift-t", true), "⌘⇧T");
/// assert_eq!(format_binding("cmd-alt-t", true), "⌥⌘T");
/// assert_eq!(format_binding("ctrl-shift-t", false), "Ctrl+Shift+T");
/// ```
pub fn format_binding(binding: &str, macos: bool) -> String {
    let parts: Vec<&str> = binding.split_whitespace().collect();
    if parts.is_empty() {
        return String::new();
    }
    let formatted: Vec<String> = parts.iter().map(|p| format_keystroke(p, macos)).collect();
    formatted.join(" ")
}

fn format_keystroke(keystroke: &str, macos: bool) -> String {
    let mut mods = KeystrokeMods::default();
    let mut key = None;
    for part in keystroke.split('-') {
        match part {
            "cmd" | "command" | "super" => mods.cmd = true,
            "ctrl" | "control" => mods.ctrl = true,
            "alt" | "option" => mods.alt = true,
            "shift" => mods.shift = true,
            other => key = Some(other),
        }
    }
    // A trailing `-` represents the minus key itself (e.g. `cmd--`).
    if keystroke.ends_with('-') {
        key = Some("-");
    }
    let key = key.unwrap_or(keystroke);
    if macos {
        format_macos(&mods, key)
    } else {
        format_other(&mods, key)
    }
}

#[derive(Default)]
struct KeystrokeMods {
    cmd: bool,
    ctrl: bool,
    alt: bool,
    shift: bool,
}

fn format_macos(mods: &KeystrokeMods, key: &str) -> String {
    let mut out = String::new();
    // Modifier order used by the existing hardcoded hints: option first,
    // then command, then control, then shift (e.g. ⌥⌘T, ⌘⇧E, ⌃⇧T).
    if mods.alt {
        out.push('⌥');
    }
    if mods.cmd {
        out.push('⌘');
    }
    if mods.ctrl {
        out.push('⌃');
    }
    if mods.shift {
        out.push('⇧');
    }
    out.push_str(&format_key(key, true));
    out
}

fn format_other(mods: &KeystrokeMods, key: &str) -> String {
    let mut parts = Vec::new();
    if mods.ctrl {
        parts.push("Ctrl".to_string());
    }
    if mods.alt {
        parts.push("Alt".to_string());
    }
    if mods.shift {
        parts.push("Shift".to_string());
    }
    parts.push(format_key(key, false));
    parts.join("+")
}

fn format_key(key: &str, macos: bool) -> String {
    if key.len() == 1 {
        let ch = key.chars().next().unwrap();
        if ch.is_ascii_alphabetic() {
            return ch.to_ascii_uppercase().to_string();
        }
        return key.to_string();
    }
    match key.to_ascii_lowercase().as_str() {
        "space" => "Space".to_string(),
        "enter" | "return" => "↵".to_string(),
        "tab" => "⇥".to_string(),
        "escape" | "esc" => {
            if macos {
                "⎋".to_string()
            } else {
                "Esc".to_string()
            }
        }
        "backspace" => "⌫".to_string(),
        "delete" | "del" => {
            if macos {
                "⌦".to_string()
            } else {
                "Delete".to_string()
            }
        }
        "up" => {
            if macos {
                "↑".to_string()
            } else {
                "Up".to_string()
            }
        }
        "down" => {
            if macos {
                "↓".to_string()
            } else {
                "Down".to_string()
            }
        }
        "left" => {
            if macos {
                "←".to_string()
            } else {
                "Left".to_string()
            }
        }
        "right" => {
            if macos {
                "→".to_string()
            } else {
                "Right".to_string()
            }
        }
        _ => key.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macos_modifiers() {
        assert_eq!(format_binding("cmd-t", true), "⌘T");
        assert_eq!(format_binding("cmd-shift-t", true), "⌘⇧T");
        assert_eq!(format_binding("cmd-alt-t", true), "⌥⌘T");
        assert_eq!(format_binding("ctrl-shift-t", true), "⌃⇧T");
        assert_eq!(format_binding("cmd-alt-shift-t", true), "⌥⌘⇧T");
    }

    #[test]
    fn other_modifiers() {
        assert_eq!(format_binding("ctrl-t", false), "Ctrl+T");
        assert_eq!(format_binding("ctrl-shift-t", false), "Ctrl+Shift+T");
        assert_eq!(
            format_binding("ctrl-alt-shift-t", false),
            "Ctrl+Alt+Shift+T"
        );
        assert_eq!(format_binding("alt-t", false), "Alt+T");
    }

    #[test]
    fn multi_keystroke_sequence() {
        assert_eq!(format_binding("ctrl-space h", false), "Ctrl+Space H");
        assert_eq!(format_binding("ctrl-space h", true), "⌃Space H");
        assert_eq!(format_binding("cmd-k cmd-s", true), "⌘K ⌘S");
    }

    #[test]
    fn special_keys() {
        assert_eq!(format_binding("enter", true), "↵");
        assert_eq!(format_binding("enter", false), "↵");
        assert_eq!(format_binding("space", true), "Space");
        assert_eq!(format_binding("space", false), "Space");
        assert_eq!(format_binding("escape", true), "⎋");
        assert_eq!(format_binding("escape", false), "Esc");
        assert_eq!(format_binding("up", true), "↑");
        assert_eq!(format_binding("up", false), "Up");
    }

    #[test]
    fn digits_and_symbols() {
        assert_eq!(format_binding("cmd-1", true), "⌘1");
        assert_eq!(format_binding("ctrl-1", false), "Ctrl+1");
        assert_eq!(format_binding("cmd--", true), "⌘-");
    }
}
