//! Extensions › Packages on a Gentle Shell server, when its Shell offers
//! them: the configured packages (`list_packages`), installing one from a
//! source and removing one (`install_package`, `remove_package`) with the
//! job's output as it comes (`package_progress`, `package_finished`). The
//! server validates the source as `gentle-shell install` does and reloads
//! as it does afterwards. One job at a time from this screen.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::Input;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use pi_rpc::desktop_rpc::{DesktopMethod, InstalledPackage, PackageScope};

use super::SettingsView;
use super::layout::section;
use super::remote::ServerPlace;
use crate::features::session::{AccountEvent, ReadOutcome, ReadRequest, ReadTicket};
use crate::shared::ui;

/// Output lines kept for a job.
const MAX_LINES: usize = 200;
/// Events kept while the job's request has not answered yet.
const MAX_EARLY: usize = 64;

/// One install or removal on the server, as the screen follows it.
#[derive(Debug)]
pub(super) struct PackageJob {
    pub(super) install: bool,
    pub(super) source: String,
    pub(super) job_id: Option<String>,
    pub(super) lines: Vec<String>,
    pub(super) done: Option<Result<(), String>>,
    early: Vec<AccountEvent>,
}

impl PackageJob {
    pub(super) fn new(install: bool, source: String) -> Self {
        Self {
            install,
            source,
            job_id: None,
            lines: Vec::new(),
            done: None,
            early: Vec::new(),
        }
    }

    pub(super) fn running(&self) -> bool {
        self.done.is_none()
    }

    /// The request answered with `job_id`: the events that came first apply.
    pub(super) fn started(&mut self, job_id: String) {
        if self.done.is_some() {
            return;
        }
        self.job_id = Some(job_id);
        for event in std::mem::take(&mut self.early) {
            self.apply(&event);
        }
    }

    /// One package event: whether it was this job's and ended it.
    pub(super) fn apply(&mut self, event: &AccountEvent) -> bool {
        if self.done.is_some() {
            return false;
        }
        let Some(job_id) = self.job_id.as_deref() else {
            if matches!(
                event,
                AccountEvent::PackageProgress(_) | AccountEvent::PackageFinished(_)
            ) && self.early.len() < MAX_EARLY
            {
                self.early.push(event.clone());
            }
            return false;
        };
        match event {
            AccountEvent::PackageProgress(progress) if progress.job_id == job_id => {
                if self.lines.len() >= MAX_LINES {
                    self.lines.remove(0);
                }
                self.lines.push(progress.line.clone());
                false
            }
            AccountEvent::PackageFinished(finished) if finished.job_id == job_id => {
                self.done = Some(if finished.ok {
                    Ok(())
                } else {
                    Err(finished
                        .error
                        .clone()
                        .filter(|error| !error.is_empty())
                        .unwrap_or_else(|| "It didn't finish.".into()))
                });
                true
            }
            _ => false,
        }
    }

    pub(super) fn failed(&mut self, message: String) {
        self.done = Some(Err(message));
    }

    fn verb(&self) -> &'static str {
        if self.install { "install" } else { "remove" }
    }
}

/// The server's packages and the job running.
#[derive(Default)]
pub(super) struct ServerPackages {
    list: Option<Vec<InstalledPackage>>,
    /// Listing failed; not asked again until something changes.
    list_error: Option<String>,
    list_ticket: Option<ReadTicket>,
    pub(super) job: Option<PackageJob>,
    job_ticket: Option<ReadTicket>,
}

impl ServerPackages {
    /// Read the list again on the next ask.
    pub(super) fn forget_list(&mut self) {
        self.list = None;
        self.list_error = None;
    }
}

impl SettingsView {
    /// Ask the server for its packages, when its Shell lists them and the
    /// list isn't known (or being read) yet.
    pub(super) fn ask_server_packages(&mut self, cx: &mut Context<Self>) {
        let state = &self.server_packages;
        if state.list_ticket.is_some()
            || state.list.is_some()
            || state.list_error.is_some()
            || !self.server_offers(DesktopMethod::ListPackages, cx)
        {
            return;
        }
        match self.ask_server(ReadRequest::ListPackages, cx) {
            Ok(ticket) => self.server_packages.list_ticket = Some(ticket),
            Err(why) => self.server_packages.list_error = Some(why),
        }
    }

    /// Install (`install`) or remove `source` on the server.
    pub(super) fn run_server_package(
        &mut self,
        install: bool,
        source: String,
        cx: &mut Context<Self>,
    ) {
        if self
            .server_packages
            .job
            .as_ref()
            .is_some_and(PackageJob::running)
        {
            return;
        }
        let mut job = PackageJob::new(install, source.clone());
        match self.ask_server(ReadRequest::PackageJob { install, source }, cx) {
            Ok(ticket) => self.server_packages.job_ticket = Some(ticket),
            Err(why) => job.failed(why),
        }
        self.server_packages.job = Some(job);
        cx.notify();
    }

    /// The answers to the list and job requests.
    pub(super) fn package_outcomes(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            return;
        };
        let (list, job) = session.update(cx, |session, _| {
            (
                self.server_packages
                    .list_ticket
                    .and_then(|ticket| session.take_read(ticket)),
                self.server_packages
                    .job_ticket
                    .and_then(|ticket| session.take_read(ticket)),
            )
        });
        if let Some(outcome) = list {
            self.server_packages.list_ticket = None;
            match outcome {
                ReadOutcome::Packages(packages) => self.server_packages.list = Some(packages),
                ReadOutcome::Failed(failure) => {
                    self.server_packages.list_error = Some(failure.message())
                }
                _ => {}
            }
            cx.notify();
        }
        if let Some(outcome) = job {
            self.server_packages.job_ticket = None;
            if let Some(job) = self.server_packages.job.as_mut() {
                match outcome {
                    ReadOutcome::PackageJobStarted(job_id) => job.started(job_id),
                    ReadOutcome::Failed(failure) => job.failed(failure.message()),
                    _ => job.failed("The server didn't start it.".into()),
                }
            }
            self.package_job_settled(cx);
            cx.notify();
        }
        self.ask_server_packages(cx);
    }

    /// Package events for the job running.
    pub(super) fn package_events(&mut self, events: &[AccountEvent], cx: &mut Context<Self>) {
        let Some(job) = self.server_packages.job.as_mut() else {
            return;
        };
        let mut changed = false;
        for event in events {
            changed |= job.apply(event);
        }
        if changed {
            self.package_job_settled(cx);
        }
    }

    /// A job ended: read the packages and their update status again.
    fn package_job_settled(&mut self, cx: &mut Context<Self>) {
        if self
            .server_packages
            .job
            .as_ref()
            .is_some_and(|job| job.done == Some(Ok(())))
        {
            self.server_packages.list = None;
            self.server_packages.list_error = None;
            self.ask_server_packages(cx);
            self.ask_update_status(false, cx);
        }
    }

    /// The server's packages with install and remove, or `None` when its
    /// Shell doesn't offer them.
    pub(super) fn server_packages_live(
        &self,
        server: &ServerPlace,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        if !self.server_offers(DesktopMethod::ListPackages, cx) {
            return None;
        }
        let state = &self.server_packages;
        let running = state.job.as_ref().is_some_and(PackageJob::running);
        let can_install = self.server_offers(DesktopMethod::InstallPackage, cx);
        let can_remove = self.server_offers(DesktopMethod::RemovePackage, cx);
        let updates = self.shell_updates.status.as_ref();
        let packages = state.list.as_deref().unwrap_or_default();
        Some(
            section(IconName::Layers, "Packages")
                .when(state.list.is_none() && state.list_error.is_none(), |d| {
                    d.child(
                        h_flex()
                            .py_1()
                            .gap_2()
                            .text_xs()
                            .text_color(ui::muted())
                            .child(Spinner::new().xsmall().color(ui::accent()))
                            .child(format!("Asking {} for its packages…", server.name)),
                    )
                })
                .children(
                    state
                        .list_error
                        .clone()
                        .map(|error| div().py_1().text_xs().text_color(ui::danger()).child(error)),
                )
                .when(state.list.is_some() && packages.is_empty(), |d| {
                    d.child(
                        div()
                            .py_1()
                            .text_xs()
                            .text_color(ui::muted())
                            .child(format!("No packages on {}.", server.name)),
                    )
                })
                .children(packages.iter().enumerate().map(|(index, package)| {
                    let source = package.source.clone();
                    h_flex()
                        .gap_2()
                        .px_2()
                        .py_1()
                        .rounded_lg()
                        .hover(|d| d.bg(ui::surface_hover()))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(ui::code_rems(12.))
                                .font_family(ui::mono())
                                .text_color(ui::fg())
                                .child(package.source.clone()),
                        )
                        .when_some(package.version.clone(), |d, version| {
                            d.child(ui::chip(version, ui::info()).flex_shrink_0())
                        })
                        .when(package.scope == PackageScope::Project, |d| {
                            d.child(ui::chip("project", ui::muted()).flex_shrink_0())
                        })
                        .children(
                            updates
                                .and_then(|status| {
                                    crate::features::updates::status::package_status(
                                        &package.source,
                                        &status.packages.items,
                                    )
                                })
                                .and_then(|item| {
                                    let update = item.state
                                        == pi_rpc::desktop_rpc::PackageUpdateState::UpdateAvailable;
                                    crate::features::updates::status::package_chip(item).map(
                                        |text| {
                                            let color =
                                                if update { ui::accent() } else { ui::muted() };
                                            ui::chip(text, color).flex_shrink_0()
                                        },
                                    )
                                }),
                        )
                        .when(can_remove, |d| {
                            d.child(
                                Button::new(("server-remove-package", index))
                                    .xsmall()
                                    .ghost()
                                    .icon(Icon::new(IconName::Trash))
                                    .tooltip(format!("Remove from {}", server.name))
                                    .disabled(running)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.run_server_package(false, source.clone(), cx)
                                    })),
                            )
                        })
                }))
                .when(can_install, |d| {
                    d.child(
                        h_flex()
                            .pt_3()
                            .gap_2()
                            .child(
                                ui::field()
                                    .flex_1()
                                    .min_w_0()
                                    .child(
                                        Icon::new(IconName::Plus).small().text_color(ui::faint()),
                                    )
                                    .child(
                                        Input::new(&self.package.input).appearance(false).small(),
                                    ),
                            )
                            .child(
                                Button::new("server-install-package")
                                    .primary()
                                    .icon(Icon::new(IconName::ArrowDown))
                                    .label(format!("Install on {}", server.name))
                                    .disabled(running)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        let source =
                                            this.package.input.read(cx).value().trim().to_string();
                                        if !source.is_empty() {
                                            this.package
                                                .input
                                                .update(cx, |s, cx| s.set_value("", window, cx));
                                            this.run_server_package(true, source, cx);
                                        }
                                    })),
                            ),
                    )
                })
                .children(state.job.as_ref().map(|job| job_view(job, &server.name))),
        )
    }
}

/// A job's state and its output's last lines.
fn job_view(job: &PackageJob, server: &str) -> Div {
    let headline = match &job.done {
        None => h_flex()
            .gap_2()
            .text_xs()
            .text_color(ui::muted())
            .child(Spinner::new().xsmall().color(ui::accent()))
            .child(format!(
                "gentle-shell {} {} on {server}…",
                job.verb(),
                job.source
            )),
        Some(Ok(())) => h_flex()
            .text_xs()
            .text_color(ui::success())
            .child(if job.install {
                format!("Installed {} on {server}.", job.source)
            } else {
                format!("Removed {} from {server}.", job.source)
            }),
        Some(Err(error)) => h_flex().text_xs().text_color(ui::danger()).child(format!(
            "gentle-shell {} {}: {error}",
            job.verb(),
            job.source
        )),
    };
    div()
        .pt_2()
        .child(headline)
        .when(!job.lines.is_empty(), |d| {
            d.child(
                ui::well_box().mt_2().px_3().py_2().gap_0p5().children(
                    job.lines
                        .iter()
                        .rev()
                        .take(8)
                        .rev()
                        .map(|line| div().text_color(ui::muted()).child(line.clone())),
                ),
            )
        })
}

#[cfg(test)]
mod tests {
    use pi_rpc::desktop_rpc::{PackageFinished, PackageProgress};

    use super::PackageJob;
    use crate::features::session::AccountEvent;

    fn line(job: &str, text: &str) -> AccountEvent {
        AccountEvent::PackageProgress(PackageProgress {
            job_id: job.into(),
            line: text.into(),
        })
    }

    fn finished(job: &str, ok: bool) -> AccountEvent {
        AccountEvent::PackageFinished(PackageFinished {
            job_id: job.into(),
            ok,
            error: (!ok).then(|| "npm ERR! 404".to_string()),
        })
    }

    #[test]
    fn a_job_keeps_its_own_output_and_ends_once() {
        let mut job = PackageJob::new(true, "npm:pi-btw".into());
        // Output before the request answered waits for the id.
        assert!(!job.apply(&line("j1", "resolving")));
        assert!(!job.apply(&line("j2", "another device's job")));
        job.started("j1".into());
        assert_eq!(job.lines, ["resolving"]);
        assert!(!job.apply(&line("j1", "added 1 package")));
        assert!(!job.apply(&finished("j2", false)), "not ours");
        assert!(job.running());
        assert!(job.apply(&finished("j1", true)));
        assert_eq!(job.done, Some(Ok(())));
        assert!(!job.apply(&line("j1", "late")));
        assert_eq!(job.lines, ["resolving", "added 1 package"]);
    }

    #[test]
    fn a_failed_job_says_why() {
        let mut job = PackageJob::new(false, "npm:pi-btw".into());
        job.started("j1".into());
        assert!(job.apply(&finished("j1", false)));
        assert_eq!(job.done, Some(Err("npm ERR! 404".into())));

        let mut refused = PackageJob::new(true, "nope".into());
        refused.failed("invalid source".into());
        refused.started("j9".into());
        assert_eq!(refused.job_id, None);
        assert!(!refused.running());
    }
}
