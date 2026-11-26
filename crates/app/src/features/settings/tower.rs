//! Gentle Tower on the machine Configuration is about: on this computer
//! (or its WSL distro or command environment), whether the Gentle Shell
//! there has Tower's web app, and building it when its release shipped
//! without it (`setup::tower`); on a Gentle Shell server, whether the
//! server offers Tower (it runs it there, for every device), and opening
//! it.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::layout::{row, section};
use super::{SettingsEvent, SettingsView, on_machine};
use crate::features::backend::server;
use crate::features::setup::tower::{self, TowerStatus};
use crate::shared::ui;

/// Build output lines kept on screen.
const LOG_KEEP: usize = 8;

#[derive(Default)]
pub(super) struct Tower {
    status: Option<TowerStatus>,
    /// On a server: whether it offers Tower, or why it couldn't be asked;
    /// `None` while asking.
    offered: Option<Result<bool, String>>,
    busy: bool,
    log: Vec<String>,
    note: Option<(String, bool)>,
    task: Option<Task<()>>,
}

impl SettingsView {
    pub(super) fn refresh_tower(&mut self, cx: &mut Context<Self>) {
        let machine = self.machine.clone();
        if machine.is_server() {
            self.tower.offered = None;
            self.tower.task = Some(cx.spawn(async move |this, cx| {
                let offered = cx
                    .background_executor()
                    .spawn(async move { server::info(&machine).map(|info| info.has("tower")) })
                    .await;
                let _ = this.update(cx, |this, cx| {
                    this.tower.offered = Some(offered);
                    cx.notify();
                });
            }));
            return;
        }
        self.tower.task = Some(cx.spawn(async move |this, cx| {
            let status = on_machine(cx.background_executor(), &machine, tower::status).await;
            let _ = this.update(cx, |this, cx| {
                this.tower.status = Some(status);
                cx.notify();
            });
        }));
    }

    /// On a server: Tower runs there; whether it offers it, and opening it
    /// in a tab.
    fn server_tower_section(&self, name: &str, cx: &mut Context<Self>) -> Div {
        let section = section(IconName::Building2, "Gentle Tower").child(
            div()
                .pb_2()
                .text_xs()
                .text_color(ui::muted())
                .child(format!(
                    "Every open session as a floor of its project's building: tasks, talk, \
                 artifacts, cost and permissions. It runs on {name}, for every device paired \
                 with it; a Tower tab opens it from there."
                )),
        );
        let Some(offered) = &self.tower.offered else {
            return section.child(
                h_flex()
                    .gap_2()
                    .text_xs()
                    .text_color(ui::muted())
                    .child(Spinner::new().xsmall().color(ui::accent()))
                    .child(format!("Asking {name}…")),
            );
        };
        let (state, color) = match offered {
            Ok(true) => (format!("Available on {name}"), ui::success()),
            Ok(false) => (server::UNAVAILABLE.to_string(), ui::warning()),
            Err(error) => (error.clone(), ui::warning()),
        };
        let machine = self.machine.clone();
        section
            .child(h_flex().pb_2().child(ui::chip(state, color)))
            .child(
                h_flex()
                    .pt_1()
                    .gap_2()
                    .flex_wrap()
                    .when(matches!(offered, Ok(true)), |d| {
                        d.child(
                            Button::new("tower-open")
                                .small()
                                .primary()
                                .icon(Icon::new(IconName::Building2))
                                .label("Open Gentle Tower")
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.emit(SettingsEvent::OpenTower(machine.clone()))
                                })),
                        )
                    })
                    .child(
                        Button::new("tower-refresh")
                            .small()
                            .ghost()
                            .icon(Icon::new(IconName::RefreshCw))
                            .label("Check again")
                            .on_click(cx.listener(|this, _, _, cx| this.refresh_tower(cx))),
                    ),
            )
    }

    fn enable_tower(&mut self, cx: &mut Context<Self>) {
        if self.tower.busy {
            return;
        }
        self.tower.busy = true;
        self.tower.log.clear();
        self.tower.note = None;
        let (tx, rx) = async_channel::unbounded::<String>();
        let machine = self.machine.clone();
        self.tower.task = Some(cx.spawn(async move |this, cx| {
            let done = on_machine(cx.background_executor(), &machine, move || {
                tower::install(&move |line: String| {
                    let _ = tx.send_blocking(line);
                })
            });
            while let Ok(line) = rx.recv().await {
                let alive = this.update(cx, |this, cx| {
                    // Download progress lines replace each other.
                    if line.trim_start().ends_with(" MB")
                        && this.tower.log.last().is_some_and(|l| l.ends_with(" MB"))
                    {
                        this.tower.log.pop();
                    }
                    this.tower.log.push(line);
                    let extra = this.tower.log.len().saturating_sub(LOG_KEEP);
                    this.tower.log.drain(..extra);
                    cx.notify();
                });
                if alive.is_err() {
                    return;
                }
            }
            let result = done.await;
            let status = on_machine(cx.background_executor(), &machine, tower::status).await;
            let _ = this.update(cx, |this, cx| {
                this.tower.busy = false;
                this.tower.status = Some(status);
                this.tower.note = Some(match result {
                    Ok(()) => (
                        "Gentle Tower is ready: open it from the tab bar's + or with /gentle:tower \
                         (a Tower tab showing the error: Try again)."
                            .into(),
                        true,
                    ),
                    Err(error) => (error, false),
                });
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub(super) fn tower_section(&self, cx: &mut Context<Self>) -> Div {
        if let Some(server) = self.server() {
            return self.server_tower_section(&server.name, cx);
        }
        let section = section(IconName::Building2, "Gentle Tower").child(
            div().pb_2().text_xs().text_color(ui::muted()).child(
                "Every open session as a floor of its project's building: tasks, talk, \
                 artifacts, cost and permissions. Gentle Shell 4.9.0 was published without \
                 Tower's web app; Enable downloads that version's source from GitHub and builds \
                 it into your install (needs a few minutes and an internet connection).",
            ),
        );
        let Some(status) = &self.tower.status else {
            return section.child(
                h_flex()
                    .gap_2()
                    .text_xs()
                    .text_color(ui::muted())
                    .child(Spinner::new().xsmall().color(ui::accent()))
                    .child("Looking for Gentle Shell…"),
            );
        };
        let busy = self.tower.busy;
        let (state, color) = match (&status.package, status.built) {
            (None, _) => ("Gentle Shell not found", ui::faint()),
            (Some(_), true) => ("Ready", ui::success()),
            (Some(_), false) => ("Web app missing in this Gentle Shell", ui::warning()),
        };
        section
            .child(h_flex().pb_2().child(ui::chip(state, color)))
            .child(row(
                "Gentle Shell",
                status.version.clone().unwrap_or_else(|| "—".into()),
            ))
            .when_some(status.package.as_ref(), |d, package| {
                d.child(row("Package", package.display().to_string()))
            })
            .child(
                h_flex()
                    .pt_3()
                    .gap_2()
                    .flex_wrap()
                    .when(status.package.is_some() && !status.built, |d| {
                        d.child(
                            Button::new("tower-enable")
                                .small()
                                .primary()
                                .icon(Icon::new(IconName::ArrowDown))
                                .label("Enable Gentle Tower")
                                .disabled(busy)
                                .on_click(cx.listener(|this, _, _, cx| this.enable_tower(cx))),
                        )
                    })
                    .child(
                        Button::new("tower-refresh")
                            .small()
                            .ghost()
                            .icon(Icon::new(IconName::RefreshCw))
                            .label("Check again")
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| this.refresh_tower(cx))),
                    ),
            )
            .when(busy, |d| {
                d.child(
                    h_flex()
                        .pt_2()
                        .gap_2()
                        .text_xs()
                        .text_color(ui::muted())
                        .child(Spinner::new().xsmall().color(ui::accent()))
                        .child("Building Gentle Tower…"),
                )
            })
            .when(!self.tower.log.is_empty(), |d| {
                d.child(
                    ui::well_box().mt_2().px_3().py_2().gap_0p5().children(
                        self.tower
                            .log
                            .iter()
                            .map(|line| div().text_color(ui::muted()).child(line.clone())),
                    ),
                )
            })
            .when_some(self.tower.note.clone(), |d, (text, ok)| {
                d.child(
                    div()
                        .pt_2()
                        .text_xs()
                        .text_color(if ok { ui::success() } else { ui::danger() })
                        .child(text),
                )
            })
    }
}
