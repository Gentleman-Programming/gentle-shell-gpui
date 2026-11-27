//! The leader guide. It started as the composer's own selection, because the
//! draft lost focus to GPUI's timed pending input; now every focus opens it
//! (a terminal pane, Files, Changes, the tab list, the canvas) and it owns the
//! sequence itself, so a lone ⌃Space waits for a choice wherever it is pressed.

use super::*;
use crate::features::shortcuts::{keystroke_string, live_bindings};
use gentle_core::{
    keyboard::format_binding,
    shortcuts::{self, same_keys},
};

/// Holds the leader's keystrokes and the key context they were started in,
/// independently of GPUI's timed pending input.
pub(super) struct LeaderSelection {
    input: Vec<Keystroke>,
    context: Vec<KeyContext>,
}

enum Choice {
    Keep,
    Cancel,
    Dispatch(Box<dyn Action>),
}

/// The window's keystroke interceptor: a leader pressed with no overlay open
/// and outside pane mode starts the guide, whatever has focus.
pub(super) fn watch_leader_keys(weak: WeakEntity<Workspace>, cx: &mut App) -> Subscription {
    cx.intercept_keystrokes(move |event, window, cx| {
        let _ = weak.update(cx, |this, cx| this.leader_key(&event.keystroke, window, cx));
    })
}

impl LeaderSelection {
    fn start(
        key: &Keystroke,
        leader: &str,
        context: Vec<KeyContext>,
        map: &Keymap,
    ) -> Option<Self> {
        if !same_keys(&keystroke_string(key), leader) {
            return None;
        }
        let input = vec![key.clone()];
        let (exact, pending) = map.bindings_for_input(&input, &context);
        (exact.is_empty() && pending).then_some(Self { input, context })
    }

    fn choose(&mut self, key: &Keystroke, map: &Keymap) -> Choice {
        if keystroke_string(key) == "escape" {
            return Choice::Cancel;
        }
        self.input.push(key.clone());
        let (bindings, pending) = map.bindings_for_input(&self.input, &self.context);
        if pending {
            return Choice::Keep;
        }
        if let Some(binding) = bindings.first() {
            return Choice::Dispatch(binding.action().boxed_clone());
        }
        self.input.pop();
        Choice::Keep
    }

    pub(super) fn hint(&self, cx: &App) -> Option<WhichKey> {
        let macos = cfg!(target_os = "macos");
        let input: Vec<String> = self.input.iter().map(keystroke_string).collect();
        let entries: Vec<_> = shortcuts::hints(&live_bindings(cx), &input)
            .into_iter()
            .map(|hint| (format_binding(&hint.keys[0], macos), hint.label))
            .collect();
        (!entries.is_empty()).then(|| WhichKey {
            title: format_binding(&input.join(" "), macos),
            entries,
            footer: "Esc cancels",
        })
    }
}

impl Workspace {
    /// Ends the guide. Esc and a choice both call this: `origin` (where focus
    /// was when the leader was pressed) takes it back, with no other change to
    /// that view, or the active pane when nothing was focused.
    pub(super) fn cancel_leader(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((origin, _)) = self.leader.take() {
            if self.pane_mode_focus.is_focused(window) {
                match origin {
                    Some(origin) => origin.focus(window, cx),
                    None => self.focus_active(window, cx),
                }
            }
            cx.notify();
        }
    }

    pub(super) fn leader_key(
        &mut self,
        key: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pane_mode_focus.is_focused(window)
            && let Some((_, selection)) = self.leader.as_mut()
        {
            let choice = selection.choose(key, &cx.key_bindings().borrow());
            match choice {
                Choice::Keep => cx.notify(),
                Choice::Cancel => self.cancel_leader(window, cx),
                Choice::Dispatch(action) => {
                    // Restore the original action scope before dispatch; an action may move focus elsewhere.
                    self.cancel_leader(window, cx);
                    window.dispatch_action(action, cx);
                }
            }
            cx.stop_propagation();
            return;
        }
        if self.pane_mode || self.overlay.is_some() || window.has_pending_keystrokes() {
            return;
        }
        let Some(selection) = LeaderSelection::start(
            key,
            &crate::app::leader(cx),
            window.context_stack(),
            &cx.key_bindings().borrow(),
        ) else {
            return;
        };
        self.leader = Some((window.focused(cx), selection));
        self.pane_mode_focus.focus(window, cx);
        cx.notify();
        cx.stop_propagation();
    }
}

#[cfg(test)]
mod tests {
    use super::{Choice, LeaderSelection, OpenPalette, OpenShortcuts};
    use gpui_kit::{KeyBinding, Keymap, Keystroke, TestAppContext};

    #[gpui::test]
    fn composer_leader_ignores_elapsed_timeout_and_resolves_configured_choice(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        let map = Keymap::new(vec![KeyBinding::new(
            "ctrl-a f",
            crate::features::workspace::FocusFiles,
            None,
        )]);
        let prefix = Keystroke::parse("ctrl-a").unwrap();
        assert!(LeaderSelection::start(&prefix, "ctrl-space", vec![], &map).is_none());
        let mut selection = LeaderSelection::start(&prefix, "ctrl-a", vec![], &map).unwrap();
        cx.executor()
            .advance_clock(std::time::Duration::from_secs(3));
        assert!(matches!(
            selection.choose(&Keystroke::parse("z").unwrap(), &map),
            Choice::Keep
        ));
        let Choice::Dispatch(action) = selection.choose(&Keystroke::parse("f").unwrap(), &map)
        else {
            panic!("delayed choice must dispatch");
        };
        assert!(
            action
                .as_any()
                .is::<crate::features::workspace::FocusFiles>()
        );
        let mut selection = LeaderSelection::start(&prefix, "ctrl-a", vec![], &map).unwrap();
        assert!(matches!(
            selection.choose(&Keystroke::parse("escape").unwrap(), &map),
            Choice::Cancel
        ));
    }

    #[gpui::test]
    fn composer_hint_matches_pending_hint_and_prefers_glyphs(cx: &mut TestAppContext) {
        let prefix = Keystroke::parse("ctrl-space").unwrap();
        let (composer_hint, normal) = cx.update(|cx| {
            gpui_kit::init(cx);
            cx.bind_keys([
                KeyBinding::new("ctrl-space ?", OpenShortcuts, None),
                KeyBinding::new("ctrl-space shift-/", OpenShortcuts, None),
                KeyBinding::new("ctrl-space :", OpenPalette, None),
                KeyBinding::new("ctrl-space shift-;", OpenPalette, None),
                KeyBinding::new("ctrl-space a", super::CreateWorkspace, None),
            ]);
            let leader = crate::app::leader(cx);
            let bindings = cx.key_bindings();
            let map = bindings.borrow();
            let selection = LeaderSelection::start(&prefix, &leader, vec![], &map).unwrap();
            drop(map);
            drop(bindings);
            let composer_hint = selection.hint(cx).unwrap();
            let live = crate::features::shortcuts::live_bindings(cx);
            let normal = super::shortcuts::hints(&live, &[leader]);
            (composer_hint, normal)
        });
        assert_eq!(composer_hint.entries.len(), normal.len());
        for ((got_key, got_label), hint) in composer_hint.entries.iter().zip(normal.iter()) {
            assert_eq!(*got_label, hint.label);
            assert_eq!(
                got_key.as_str(),
                gentle_core::keyboard::format_binding(&hint.keys[0], cfg!(target_os = "macos"))
            );
        }
        let keys: Vec<_> = composer_hint
            .entries
            .iter()
            .map(|(k, _)| k.as_str())
            .collect();
        assert!(keys.contains(&"?"));
        assert!(keys.contains(&":"));
        assert!(!keys.iter().any(|k| k.contains('/') && k.contains('⇧')));
        assert!(!keys.iter().any(|k| k.contains(';')));
    }
}
