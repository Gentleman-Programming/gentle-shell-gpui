//! Vim-style list navigation setting: a global boolean saved in the app's
//! `settings.json` (`gentle_core::vim_keys::SETTINGS_KEY`).

use gentle_core::vim_keys::{SETTINGS_KEY, VimKeys};
use gpui_kit::{App, Context, Global, Subscription};

struct VimKeysChoice(VimKeys);

impl Global for VimKeysChoice {}

/// Read the saved choice into a global.
pub fn init(cx: &mut App) {
    let saved = crate::shared::themes::store_path()
        .and_then(|path| crate::shared::themes::read_key(&path, SETTINGS_KEY))
        .map(|value| VimKeys::from_json(&value))
        .unwrap_or_default();
    cx.set_global(VimKeysChoice(saved));
}

pub fn enabled(cx: &App) -> bool {
    cx.try_global::<VimKeysChoice>()
        .is_some_and(|choice| choice.0.enabled)
}

/// Redraw `T` when the setting changes.
pub fn follow<T: 'static>(cx: &mut Context<T>) -> Subscription {
    cx.observe_global::<VimKeysChoice>(|_, cx| cx.notify())
}

/// Toggle and persist the choice.
pub fn set_enabled(enabled: bool, cx: &mut App) {
    cx.set_global(VimKeysChoice(VimKeys { enabled }));
    if let Some(path) = crate::shared::themes::store_path()
        && let Err(err) =
            crate::shared::themes::write_key(&path, SETTINGS_KEY, VimKeys { enabled }.to_json())
    {
        eprintln!("gentle-shell-gpui: could not save vim keys setting: {err}");
    }
}
