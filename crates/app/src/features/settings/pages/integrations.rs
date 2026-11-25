//! MCP servers and optional companion tools, with a link to device settings.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::*;

use super::super::{
    SettingsView,
    layout::{row, section},
    navigation::SettingsPage,
    snapshot::Snapshot,
};
use crate::shared::ui;

pub(super) fn render(
    view: &SettingsView,
    snapshot: &Snapshot,
    cx: &mut Context<SettingsView>,
) -> Div {
    let mcp = match view.server() {
        // Its Gentle Shell's `mcp_servers` store, edited there.
        Some(server) => view.server_mcp(server, cx),
        None => section(IconName::Bot, "MCP servers")
            .child(row(
                "Configured servers",
                if snapshot.mcp_servers.is_empty() {
                    "—".into()
                } else {
                    snapshot.mcp_servers.join(", ")
                },
            ))
            .child(
                div().pt_2().text_xs().text_color(ui::faint()).child(
                    "Detected from Gentle Shell's MCP configuration; edit the server definitions in the terminal configuration.",
                ),
            ),
    };
    v_flex()
        .gap_3()
        .child(mcp)
        .child(view.maestro_section(cx))
        .child(
            h_flex().child(
                Button::new("integrations-device-settings")
                    .ghost()
                    .small()
                    .icon(Icon::new(IconName::Smartphone))
                    .label("Configure devices")
                    .on_click(
                        cx.listener(|this, _, _, cx| this.navigate_to(SettingsPage::Devices, cx)),
                    ),
            ),
        )
        .child(view.music_section(cx))
        .child(view.tower_section(cx))
}
