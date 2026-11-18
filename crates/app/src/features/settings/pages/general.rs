//! Environment details and the existing onboarding checklist.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::*;

use super::super::{
    SettingsView,
    layout::{row, section},
    snapshot::Snapshot,
};

pub(super) fn render(view: &SettingsView, snapshot: &Snapshot) -> Div {
    v_flex()
        .gap_3()
        .child(gentle_shell_section(snapshot))
        .child(section(IconName::CircleCheck, "Setup & requirements").child(view.setup.clone()))
}

fn gentle_shell_section(snapshot: &Snapshot) -> Div {
    section(IconName::SquareTerminal, "Gentle Shell")
        .child(row(
            "Home",
            if snapshot.home_mode.is_empty() {
                snapshot.agent.display().to_string()
            } else {
                snapshot.home_mode.clone()
            },
        ))
        .child(row("Agent dir", snapshot.agent.display().to_string()))
        .child(row("Config dir", snapshot.config.display().to_string()))
        .child(row(
            "Launcher",
            snapshot
                .launcher
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "not found".into()),
        ))
        .child(
            h_flex()
                .pt_2()
                .gap_2()
                .flex_wrap()
                .child({
                    let dir = snapshot.agent.clone();
                    Button::new("open-agent-dir")
                        .ghost()
                        .small()
                        .icon(Icon::new(IconName::FolderOpen))
                        .label("Open agent dir")
                        .on_click(move |_, _, cx| cx.open_url(&format!("file://{}", dir.display())))
                })
                .child({
                    let dir = snapshot.config.clone();
                    Button::new("open-config-dir")
                        .ghost()
                        .small()
                        .icon(Icon::new(IconName::FolderOpen))
                        .label("Open config dir")
                        .on_click(move |_, _, cx| cx.open_url(&format!("file://{}", dir.display())))
                }),
        )
}
