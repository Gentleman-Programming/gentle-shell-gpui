//! Keyboard shortcuts, discoverable: the which-key hint shown while a key
//! sequence is pending (⌃Space …) or pane mode is on, and the cheat-sheet
//! screen. Both read the live keymap, so a rebound key shows up as is; what
//! to show (prefix filtering, grouping, search) is `gentle_core::shortcuts`.

mod hint;
mod view;

use gentle_core::shortcuts::Binding;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::*;

pub use hint::{WhichKey, pane_mode_hint, panel, pending_hint};

pub enum ShortcutsEvent {
    Close,
}

pub struct ShortcutsView {
    query: Entity<InputState>,
    _subscription: Subscription,
}

impl EventEmitter<ShortcutsEvent> for ShortcutsView {}

impl ShortcutsView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Search by action or key, e.g. split or ⌃Space")
        });
        query.update(cx, |state, cx| state.focus(window, cx));
        let subscription =
            cx.subscribe_in(&query, window, |_, _, _: &InputEvent, _, cx| cx.notify());
        Self {
            query,
            _subscription: subscription,
        }
    }
}

/// A keystroke as the binding string the keymap is written in.
pub fn keystroke_string(stroke: &Keystroke) -> String {
    let m = &stroke.modifiers;
    let mut parts = Vec::new();
    if m.control {
        parts.push("ctrl");
    }
    if m.platform {
        parts.push(if cfg!(target_os = "macos") {
            "cmd"
        } else {
            "super"
        });
    }
    if m.alt {
        parts.push("alt");
    }
    if m.shift {
        parts.push("shift");
    }
    parts.push(&stroke.key);
    parts.join("-")
}

/// The app's live keymap, as `gentle_core` reads it.
pub fn live_bindings(cx: &App) -> Vec<Binding> {
    let mut live: Vec<Binding> = Vec::new();
    for binding in cx.key_bindings().borrow().bindings() {
        let keys: Vec<String> = binding
            .keystrokes()
            .iter()
            .map(|k| keystroke_string(k.inner()))
            .collect();
        let context = binding.predicate().map(|p| p.to_string());
        // A `keymap.json` entry with `"action": null` unbinds the same keys in
        // the same context: the keys no longer do what was bound before.
        if gpui_kit::is_no_action(binding.action()) {
            live.retain(|b| b.keys != keys || b.context != context);
            continue;
        }
        live.push(Binding {
            keys,
            action: binding.action().name().to_string(),
            context,
        });
    }
    live
}
