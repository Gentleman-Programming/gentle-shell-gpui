//! Where agents run: this computer, a WSL distro (Windows), an environment
//! a command reaches (a Linux VM through OrbStack or Lima, a container), or a
//! Gentle Shell server (`gentle-shell serve`) this device pairs with. The
//! choice is `features::backend`'s; new agent tabs start there, open tabs
//! stay where they started.

use gentle_core::backend::{Backend, Distro};
use gentle_core::serve::{Entered, Nearby, Pick};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use pi_rpc::serve::ServerInfo;

use super::SettingsView;
use super::layout::{control, row, section};
use crate::features::backend;
use crate::shared::ui;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Local,
    Wsl,
    Command,
    Server,
}

impl Kind {
    /// The options this computer offers: WSL only on Windows.
    fn all() -> &'static [Kind] {
        if cfg!(windows) {
            &[Kind::Local, Kind::Wsl, Kind::Command, Kind::Server]
        } else {
            &[Kind::Local, Kind::Command, Kind::Server]
        }
    }

    fn label(self) -> &'static str {
        match self {
            Kind::Local => "This computer",
            Kind::Wsl => "WSL",
            Kind::Command => "Command",
            Kind::Server => "Gentle Shell server",
        }
    }

    fn of(choice: &Backend) -> Kind {
        match choice {
            Backend::Local => Kind::Local,
            Backend::Wsl { .. } => Kind::Wsl,
            Backend::Command { .. } => Kind::Command,
            Backend::Server { .. } => Kind::Server,
        }
    }
}

pub(super) struct RunsOn {
    /// What new agent tabs use, as saved.
    choice: Backend,
    /// The option shown: Command shows its field before a command is saved.
    kind: Kind,
    /// WSL's distros and whether each has `gentle-shell`; `None` while
    /// looking.
    distros: Option<Vec<(Distro, bool)>>,
    command: Entity<InputState>,
    _command: Subscription,
    /// Whether the saved backend has `gentle-shell`, once checked.
    found: Option<bool>,
    checking: bool,
    task: Task<()>,
    check: Task<()>,
    /// Pointing Configuration at the new backend's Gentle Shell home.
    homes: Task<()>,
    /// The Code field: the short pairing code `gentle-shell serve share`
    /// shows (its server is found on the network), or the long connection
    /// code it prints as a fallback (pasting one connects).
    code_field: Entity<InputState>,
    _code_field: Subscription,
    /// Several servers on the network answered: the user picks the one
    /// the short code (the first) is for.
    choices: Option<(String, Vec<Nearby>)>,
    /// Whether the address and pairing code fields show (Enter address
    /// manually).
    manual: bool,
    /// A server's address and the pairing code `gentle-shell serve pair`
    /// printed there.
    server_url: Entity<InputState>,
    server_code: Entity<InputState>,
    _server_code: Subscription,
    /// What the saved server says about itself, once asked (or why it
    /// couldn't be asked).
    server: Option<Result<ServerInfo, String>>,
    pairing: bool,
    /// Where connecting with a code is ("Looking for computers on your
    /// network…").
    progress: Option<String>,
    pair_error: Option<String>,
    pair: Task<()>,
}

impl RunsOn {
    pub(super) fn new(window: &mut Window, cx: &mut Context<SettingsView>) -> Self {
        let choice = backend::saved();
        let text = match &choice {
            Backend::Command { prefix } => prefix.join(" "),
            _ => String::new(),
        };
        let command = cx.new(|cx| {
            let mut state = InputState::new(window, cx).placeholder("orb -m dev --");
            state.set_value(text, window, cx);
            state
        });
        let subscription = cx.subscribe(&command, |this, input, event: &InputEvent, cx| {
            if let InputEvent::PressEnter { .. } = event {
                let prefix = pi_rpc::split_args(&input.read(cx).value());
                if !prefix.is_empty() {
                    this.choose_runs_on(Backend::Command { prefix }, cx);
                }
            }
        });
        let url = match &choice {
            Backend::Server { url, .. } => url.clone(),
            _ => String::new(),
        };
        let server_url = cx.new(|cx| {
            let mut state =
                InputState::new(window, cx).placeholder(gentle_core::serve::DEFAULT_URL);
            state.set_value(url, window, cx);
            state
        });
        let server_code = cx.new(|cx| InputState::new(window, cx).placeholder("ABCD-EFGH"));
        let code_subscription = cx.subscribe(&server_code, |this, _, event: &InputEvent, cx| {
            if let InputEvent::PressEnter { .. } = event {
                this.pair_server(cx);
            }
        });
        let code_field = cx.new(|cx| InputState::new(window, cx).placeholder("XXXX-XXXX"));
        let field_subscription =
            cx.subscribe(&code_field, |this, input, event: &InputEvent, cx| {
                let text = input.read(cx).value().to_string();
                match event {
                    // A pasted connection code connects at once.
                    InputEvent::Change if gentle_core::serve::is_connection_code(&text) => {
                        this.connect_code(&text, cx);
                    }
                    InputEvent::Change if text.trim().is_empty() && !this.runs_on.pairing => {
                        this.runs_on.pair_error = None;
                        this.runs_on.choices = None;
                        cx.notify();
                    }
                    InputEvent::PressEnter { .. } => this.connect_code(&text, cx),
                    _ => {}
                }
            });
        Self {
            kind: Kind::of(&choice),
            choice,
            distros: None,
            command,
            _command: subscription,
            found: None,
            checking: false,
            task: Task::ready(()),
            check: Task::ready(()),
            homes: Task::ready(()),
            code_field,
            _code_field: field_subscription,
            choices: None,
            manual: false,
            server_url,
            server_code,
            _server_code: code_subscription,
            server: None,
            pairing: false,
            progress: None,
            pair_error: None,
            pair: Task::ready(()),
        }
    }
}

impl SettingsView {
    /// Look for WSL's distros (Windows) and check the saved backend.
    pub(super) fn refresh_runs_on(&mut self, cx: &mut Context<Self>) {
        if cfg!(windows) {
            self.runs_on.task = cx.spawn(async move |this, cx| {
                let distros = cx
                    .background_executor()
                    .spawn(async {
                        backend::wsl_distros()
                            .into_iter()
                            .map(|distro| {
                                let found = backend::has_gentle_shell(&Backend::Wsl {
                                    distro: distro.name.clone(),
                                });
                                (distro, found)
                            })
                            .collect::<Vec<_>>()
                    })
                    .await;
                let _ = this.update(cx, |this, cx| {
                    this.runs_on.distros = Some(distros);
                    cx.notify();
                });
            });
        }
        self.check_runs_on(cx);
    }

    /// Whether the saved backend has `gentle-shell`; a server, whether it
    /// answers this device.
    fn check_runs_on(&mut self, cx: &mut Context<Self>) {
        let choice = self.runs_on.choice.clone();
        if choice == Backend::Local {
            self.runs_on.found = None;
            self.runs_on.checking = false;
            return;
        }
        self.runs_on.checking = true;
        self.runs_on.check = cx.spawn(async move |this, cx| {
            let (found, server) = cx
                .background_executor()
                .spawn({
                    let choice = choice.clone();
                    async move {
                        if choice.is_server() {
                            let info = backend::server::info(&choice);
                            (info.is_ok(), Some(info))
                        } else {
                            (backend::has_gentle_shell(&choice), None)
                        }
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.runs_on.choice == choice {
                    this.runs_on.found = Some(found);
                    this.runs_on.server = server;
                    this.runs_on.checking = false;
                    cx.notify();
                }
            });
        });
    }

    /// Save where new agent tabs run, check it has `gentle-shell`, and read
    /// Configuration from its Gentle Shell home.
    fn choose_runs_on(&mut self, choice: Backend, cx: &mut Context<Self>) {
        self.runs_on.kind = Kind::of(&choice);
        if self.runs_on.choice == choice {
            cx.notify();
            return;
        }
        if let Err(err) = backend::save(&choice) {
            eprintln!("gentle-shell-gpui: could not save where agents run: {err}");
        }
        self.runs_on.choice = choice;
        self.runs_on.found = None;
        self.runs_on.server = None;
        self.check_runs_on(cx);
        self.runs_on.homes = cx.spawn(async move |this, cx| {
            cx.background_executor()
                .spawn(async { backend::sync_homes() })
                .await;
            let _ = this.update(cx, |this, cx| this.reload(cx));
        });
        cx.notify();
    }

    /// Pair with the server at the typed address using the typed code
    /// (`POST /api/pair`); once paired, new agent tabs run there. A
    /// connection code typed in either field connects with it instead.
    fn pair_server(&mut self, cx: &mut Context<Self>) {
        if self.runs_on.pairing {
            return;
        }
        let url = self.runs_on.server_url.read(cx).value().to_string();
        let code = self.runs_on.server_code.read(cx).value().to_string();
        if let Some(link) = [&url, &code]
            .into_iter()
            .find(|text| gentle_core::serve::is_connection_code(text))
        {
            self.connect_code(&link.clone(), cx);
            return;
        }
        let url = if url.trim().is_empty() {
            gentle_core::serve::DEFAULT_URL.to_string()
        } else {
            url
        };
        self.runs_on.pairing = true;
        self.runs_on.pair_error = None;
        self.runs_on.pair = cx.spawn(async move |this, cx| {
            let paired = cx
                .background_executor()
                .spawn(async move { backend::server::pair(&url, &code) })
                .await;
            let _ = this.update(cx, |this, cx| this.paired(paired, cx));
        });
        cx.notify();
    }

    /// Connect with what the Code field holds: a short pairing code (find
    /// the servers on the network, then pair with the one that answered,
    /// or let the user pick when several did), or a connection code (look
    /// for its server at every address in it, then pair where it answered).
    fn connect_code(&mut self, text: &str, cx: &mut Context<Self>) {
        if self.runs_on.pairing {
            return;
        }
        self.runs_on.choices = None;
        let code = match gentle_core::serve::read_code(text) {
            Ok(Entered::Connection(code)) => code,
            Ok(Entered::Short(code)) => return self.find_nearby(code, cx),
            Err(error) => {
                self.runs_on.pair_error = Some(error.to_string());
                cx.notify();
                return;
            }
        };
        self.runs_on.pairing = true;
        self.runs_on.pair_error = None;
        self.runs_on.progress = Some(format!("Looking for {} on your network…", code.name));
        self.runs_on.pair = cx.spawn(async move |this, cx| {
            let found = cx
                .background_executor()
                .spawn({
                    let code = code.clone();
                    async move { pi_rpc::serve::locate(&code) }
                })
                .await;
            let url = match found {
                Ok(url) => url,
                Err(error) => {
                    let _ = this.update(cx, |this, cx| this.paired(Err(error), cx));
                    return;
                }
            };
            let _ = this.update(cx, |this, cx| {
                this.runs_on.progress = Some(format!("Pairing with {}…", code.name));
                cx.notify();
            });
            let paired = cx
                .background_executor()
                .spawn(async move { backend::server::pair_code(&code, &url) })
                .await;
            let _ = this.update(cx, |this, cx| this.paired(paired, cx));
        });
        cx.notify();
    }

    /// Find the servers on the network for the short pairing `code`: one
    /// that answered is paired with at once; several are listed to pick
    /// from.
    fn find_nearby(&mut self, code: String, cx: &mut Context<Self>) {
        self.runs_on.pairing = true;
        self.runs_on.pair_error = None;
        self.runs_on.progress = Some("Looking for computers on your network…".into());
        self.runs_on.pair = cx.spawn(async move |this, cx| {
            let found = cx
                .background_executor()
                .spawn(async move { backend::server::nearby() })
                .await;
            let server = match found.map(gentle_core::serve::pick) {
                Ok(Pick::One(server)) => server,
                Ok(Pick::Many(servers)) => {
                    let _ = this.update(cx, |this, cx| {
                        this.runs_on.pairing = false;
                        this.runs_on.progress = None;
                        this.runs_on.choices = Some((code, servers));
                        cx.notify();
                    });
                    return;
                }
                Ok(Pick::None) => {
                    let error = gentle_core::serve::NONE_NEARBY.to_string();
                    let _ = this.update(cx, |this, cx| this.paired(Err(error), cx));
                    return;
                }
                Err(error) => {
                    let _ = this.update(cx, |this, cx| this.paired(Err(error), cx));
                    return;
                }
            };
            let _ = this.update(cx, |this, cx| {
                this.runs_on.progress = Some(format!("Pairing with {}…", server.name));
                cx.notify();
            });
            let paired = cx
                .background_executor()
                .spawn(async move { backend::server::pair_nearby(&server, &code) })
                .await;
            let _ = this.update(cx, |this, cx| this.paired(paired, cx));
        });
        cx.notify();
    }

    /// Pair with `server` (found on the network) using the short `code`.
    fn pair_nearby(&mut self, server: Nearby, code: String, cx: &mut Context<Self>) {
        if self.runs_on.pairing {
            return;
        }
        self.runs_on.choices = None;
        self.runs_on.pairing = true;
        self.runs_on.pair_error = None;
        self.runs_on.progress = Some(format!("Pairing with {}…", server.name));
        self.runs_on.pair = cx.spawn(async move |this, cx| {
            let paired = cx
                .background_executor()
                .spawn(async move { backend::server::pair_nearby(&server, &code) })
                .await;
            let _ = this.update(cx, |this, cx| this.paired(paired, cx));
        });
        cx.notify();
    }

    /// A pairing ended: save the server, or say why it didn't pair.
    fn paired(&mut self, paired: Result<(Backend, ServerInfo), String>, cx: &mut Context<Self>) {
        self.runs_on.pairing = false;
        self.runs_on.progress = None;
        match paired {
            Ok((choice, info)) => {
                self.choose_runs_on(choice, cx);
                self.runs_on.server = Some(Ok(info));
            }
            Err(error) => self.runs_on.pair_error = Some(error),
        }
        cx.notify();
    }

    /// Forget this device's token for the saved server and run agents on
    /// this computer again. The server keeps the device until it is revoked
    /// there; its sessions keep running.
    fn unpair_server(&mut self, cx: &mut Context<Self>) {
        if let Backend::Server { url, .. } = &self.runs_on.choice
            && let Err(err) = backend::server::forget(url)
        {
            eprintln!("gentle-shell-gpui: could not forget the server's token: {err}");
        }
        self.choose_runs_on(Backend::Local, cx);
        self.runs_on.kind = Kind::Server;
        cx.notify();
    }

    /// Picking WSL takes the distro to suggest: the default one with
    /// `gentle-shell`, else any with it, else the default.
    fn suggested_distro(&self) -> Option<String> {
        let distros = self.runs_on.distros.as_ref()?;
        distros
            .iter()
            .find(|(d, found)| d.default && *found)
            .or_else(|| distros.iter().find(|(_, found)| *found))
            .or_else(|| distros.iter().find(|(d, _)| d.default))
            .map(|(d, _)| d.name.clone())
    }

    fn pick_kind(&mut self, kind: Kind, cx: &mut Context<Self>) {
        match kind {
            Kind::Local => self.choose_runs_on(Backend::Local, cx),
            Kind::Wsl => match self.suggested_distro() {
                Some(distro) => self.choose_runs_on(Backend::Wsl { distro }, cx),
                None => {
                    self.runs_on.kind = kind;
                    cx.notify();
                }
            },
            // A server is saved once paired.
            Kind::Command | Kind::Server => {
                self.runs_on.kind = kind;
                cx.notify();
            }
        }
    }

    pub(super) fn runs_on_section(&self, cx: &mut Context<Self>) -> Div {
        let kinds = Kind::all();
        let labels: Vec<&'static str> = kinds.iter().map(|k| k.label()).collect();
        let selected = kinds.iter().position(|k| *k == self.runs_on.kind);
        let pick = cx.listener(move |this, index: &usize, _, cx| {
            if let Some(kind) = Kind::all().get(*index) {
                this.pick_kind(*kind, cx);
            }
        });
        let detail: AnyElement = match self.runs_on.kind {
            Kind::Local => div().into_any_element(),
            Kind::Wsl => self.render_distros(cx).into_any_element(),
            Kind::Command => control(
                "Command",
                false,
                ui::field()
                    .flex_1()
                    .min_w_0()
                    .h(rems(30. / 16.))
                    .child(
                        Icon::new(IconName::SquareTerminal)
                            .small()
                            .text_color(ui::faint()),
                    )
                    .child(Input::new(&self.runs_on.command).appearance(false).small()),
            )
            .into_any_element(),
            Kind::Server => self.render_server(cx).into_any_element(),
        };
        section(IconName::Server, "Where agents run")
            .child(control(
                "Agents run on",
                false,
                ui::segmented("runs-on", &labels, selected, pick),
            ))
            .child(detail)
            .child(self.render_runs_on_status())
            .child(
                div()
                    .pt_2()
                    .text_xs()
                    .text_color(ui::faint())
                    .child(match self.runs_on.kind {
                        Kind::Command => {
                            "A command that runs a program in that environment, as argv \
                             (OrbStack: orb -m dev --, Lima: limactl shell dev, a container: \
                             docker exec -i dev). Press Enter to use it."
                        }
                        Kind::Server => {
                            "On the other computer, run gentle-shell serve share and enter the \
                             code it shows. Use it on a network you trust: the server speaks \
                             plain HTTP. It runs the agents and keeps them running while this \
                             app is closed. Project folders are paths on the server; files, \
                             git, terminals and Setup aren't available there yet."
                        }
                        _ => {
                            "Agents, terminals, git and Setup run here; open agent tabs stay \
                             where they started. A project opened from \
                             \\\\wsl.localhost\\<distro>\\ always runs in that distro."
                        }
                    }),
            )
    }

    /// The paired server (its name, address and id, and Unpair), or the
    /// Code field to connect with one (the servers that answered on the
    /// network when several did, and, under Enter address manually, the
    /// address and pairing code).
    fn render_server(&self, cx: &mut Context<Self>) -> Div {
        if let Backend::Server { url, device, name } = &self.runs_on.choice {
            let server_id = match &self.runs_on.server {
                Some(Ok(info)) => info.server_id.clone(),
                Some(Err(_)) => "unreachable".to_string(),
                None => "…".to_string(),
            };
            let version = match &self.runs_on.server {
                Some(Ok(ServerInfo {
                    version: Some(version),
                    ..
                })) => version.clone(),
                _ => "—".to_string(),
            };
            return v_flex()
                .pt_2()
                .gap_1()
                .child(row("Server", name.clone()))
                .child(row("Address", url.clone()))
                .child(row("Server id", server_id))
                .child(row("Gentle Shell", version))
                .child(row("This device", device.clone()))
                .child(
                    h_flex().pt_1().child(
                        Button::new("runs-on-unpair")
                            .small()
                            .ghost()
                            .icon(Icon::new(IconName::Unlink))
                            .label("Unpair")
                            .tooltip(
                                "Forget this computer's token and run agents here. Revoke the \
                                 device on the server with gentle-shell serve devices --revoke.",
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.unpair_server(cx))),
                    ),
                );
        }
        let pairing = self.runs_on.pairing;
        let connecting = pairing && self.runs_on.progress.is_some();
        let manual = self.runs_on.manual;
        let manual_fields = v_flex()
            .child(control(
                "Address",
                false,
                ui::field()
                    .flex_1()
                    .min_w_0()
                    .h(rems(30. / 16.))
                    .child(Icon::new(IconName::Server).small().text_color(ui::faint()))
                    .child(
                        Input::new(&self.runs_on.server_url)
                            .appearance(false)
                            .small(),
                    ),
            ))
            .child(control(
                "Pairing code",
                false,
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_2()
                    .child(
                        ui::field()
                            .flex_1()
                            .min_w_0()
                            .h(rems(30. / 16.))
                            .child(
                                Icon::new(IconName::KeyRound)
                                    .small()
                                    .text_color(ui::faint()),
                            )
                            .child(
                                Input::new(&self.runs_on.server_code)
                                    .appearance(false)
                                    .small(),
                            ),
                    )
                    .child(
                        Button::new("runs-on-pair")
                            .small()
                            .ghost()
                            .when(pairing && !connecting, |b| b.loading(true))
                            .label(if pairing && !connecting {
                                "Pairing…"
                            } else {
                                "Pair"
                            })
                            .disabled(pairing)
                            .on_click(cx.listener(|this, _, _, cx| this.pair_server(cx))),
                    ),
            ));
        v_flex()
            .child(control(
                "Code",
                false,
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_2()
                    .child(
                        ui::field()
                            .flex_1()
                            .min_w_0()
                            .h(rems(30. / 16.))
                            .child(
                                Icon::new(IconName::KeyRound)
                                    .small()
                                    .text_color(ui::faint()),
                            )
                            .child(
                                Input::new(&self.runs_on.code_field)
                                    .appearance(false)
                                    .small(),
                            ),
                    )
                    .child(
                        Button::new("runs-on-connect")
                            .small()
                            .primary()
                            .when(connecting, |b| b.loading(true))
                            .label(if connecting {
                                "Connecting…"
                            } else {
                                "Connect"
                            })
                            .disabled(pairing)
                            .on_click(cx.listener(|this, _, _, cx| {
                                let text = this.runs_on.code_field.read(cx).value().to_string();
                                this.connect_code(&text, cx);
                            })),
                    ),
            ))
            .children(self.runs_on.progress.clone().map(|progress| {
                h_flex()
                    .pt_2()
                    .gap_2()
                    .text_xs()
                    .text_color(ui::muted())
                    .child(Spinner::new().xsmall().color(ui::muted()))
                    .child(div().min_w_0().child(progress))
            }))
            .children(self.runs_on.pair_error.clone().map(|error| {
                h_flex()
                    .pt_2()
                    .gap_2()
                    .text_xs()
                    .text_color(ui::danger())
                    .child(Icon::new(IconName::CircleAlert).xsmall())
                    .child(div().min_w_0().child(error))
            }))
            .children(self.render_nearby(cx))
            .child(
                h_flex()
                    .id("runs-on-manual")
                    .pt_3()
                    .gap_1()
                    .cursor_pointer()
                    .text_xs()
                    .text_color(ui::muted())
                    .hover(|s| s.text_color(ui::fg()))
                    .child(
                        Icon::new(if manual {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .xsmall()
                        .text_color(ui::faint()),
                    )
                    .child("Enter address manually")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.runs_on.manual = !this.runs_on.manual;
                        cx.notify();
                    })),
            )
            .when(manual, |d| d.child(manual_fields))
    }

    /// The servers that answered on the network, when several did: one
    /// row each (its name and address) to pair the short code with.
    fn render_nearby(&self, cx: &mut Context<Self>) -> Option<Div> {
        let (code, servers) = self.runs_on.choices.as_ref()?;
        let rows = servers.iter().enumerate().map(|(ix, server)| {
            let (server, code) = (server.clone(), code.clone());
            h_flex()
                .id(("runs-on-nearby", ix))
                .h(rems(26. / 16.))
                .px_2()
                .gap_2()
                .rounded_md()
                .cursor_pointer()
                .hover(|s| s.bg(ui::surface_hover()))
                .child(Icon::new(IconName::Server).xsmall().text_color(ui::faint()))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(ui::fg())
                        .child(server.name.clone()),
                )
                .child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(ui::faint())
                        .child(server.ip.to_string()),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.pair_nearby(server.clone(), code.clone(), cx)
                }))
        });
        Some(
            v_flex()
                .pt_2()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .text_color(ui::muted())
                        .child("Several computers answered. Which one showed this code?"),
                )
                .child(v_flex().gap_px().children(rows)),
        )
    }

    fn render_distros(&self, cx: &mut Context<Self>) -> Div {
        let Some(distros) = &self.runs_on.distros else {
            return control(
                "Distro",
                false,
                h_flex()
                    .gap_2()
                    .text_xs()
                    .text_color(ui::muted())
                    .child(Spinner::new().xsmall().color(ui::muted()))
                    .child("Looking for WSL distros…"),
            );
        };
        if distros.is_empty() {
            return control(
                "Distro",
                false,
                div()
                    .text_xs()
                    .text_color(ui::muted())
                    .child("No WSL distros found. Install one with wsl --install."),
            );
        }
        let current = match &self.runs_on.choice {
            Backend::Wsl { distro } => Some(distro.as_str()),
            _ => None,
        };
        let rows = distros.iter().enumerate().map(|(ix, (distro, found))| {
            let selected = current.is_some_and(|c| c.eq_ignore_ascii_case(&distro.name));
            let name = distro.name.clone();
            let note = match (found, distro.default) {
                (true, true) => "default · gentle-shell found",
                (true, false) => "gentle-shell found",
                (false, true) => "default · no gentle-shell",
                (false, false) => "no gentle-shell",
            };
            h_flex()
                .id(("runs-on-distro", ix))
                .h(rems(26. / 16.))
                .px_2()
                .gap_2()
                .rounded_md()
                .cursor_pointer()
                .when(selected, |d| d.bg(ui::raised()))
                .when(!selected, |d| d.hover(|s| s.bg(ui::surface_hover())))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.choose_runs_on(
                        Backend::Wsl {
                            distro: name.clone(),
                        },
                        cx,
                    )
                }))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(if selected { ui::fg() } else { ui::muted() })
                        .child(distro.name.clone()),
                )
                .child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(if *found { ui::faint() } else { ui::warning() })
                        .child(note),
                )
        });
        control(
            "Distro",
            false,
            v_flex().flex_1().min_w_0().gap_px().children(rows),
        )
    }

    /// Whether the saved backend has `gentle-shell` (a server: whether it
    /// answers), under the controls.
    fn render_runs_on_status(&self) -> Div {
        let line = |icon: IconName, color: Hsla, text: String| {
            h_flex()
                .pt_2()
                .gap_2()
                .text_xs()
                .text_color(color)
                .child(Icon::new(icon).xsmall())
                .child(div().min_w_0().child(text))
        };
        // The saved choice's status shows under its own option only.
        if Kind::of(&self.runs_on.choice) != self.runs_on.kind {
            return div();
        }
        let label = self.runs_on.choice.label();
        match (
            &self.runs_on.choice,
            self.runs_on.checking,
            self.runs_on.found,
        ) {
            (Backend::Local, _, _) => div(),
            (_, true, _) => h_flex()
                .pt_2()
                .gap_2()
                .text_xs()
                .text_color(ui::muted())
                .child(Spinner::new().xsmall().color(ui::muted()))
                .child(format!("Checking {label}…")),
            (_, false, Some(true)) => line(
                IconName::CircleCheck,
                ui::success(),
                format!("New agent tabs run on {label}."),
            ),
            (Backend::Server { .. }, false, Some(false)) => line(
                IconName::CircleAlert,
                ui::warning(),
                match &self.runs_on.server {
                    Some(Err(error)) => error.clone(),
                    _ => format!("{label} doesn't answer."),
                },
            ),
            (_, false, Some(false)) => line(
                IconName::CircleAlert,
                ui::warning(),
                format!("gentle-shell wasn't found on {label}. Install it there first."),
            ),
            (_, false, None) => div(),
        }
    }
}
