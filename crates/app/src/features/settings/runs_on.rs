//! Where agents run: the machine a new workspace goes to ("Agents run on":
//! this computer, a WSL distro on Windows, an environment a command reaches
//! such as a Linux VM through OrbStack or Lima or a container, or a Gentle
//! Shell server), and the Gentle Shell servers this device paired with,
//! each with its address, whether it answers and Unpair, and pairing
//! another. The default is `features::backend`'s; open workspaces keep the
//! machine they were opened on, and choosing this computer forgets no
//! server.

use std::collections::HashMap;

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

use super::layout::{control, row, section};
use super::{SettingsEvent, SettingsView};
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

/// What a paired server said when asked: about itself, and where it
/// answers now (its paired address, or the one it was found at since); or
/// why it couldn't be asked.
type Answer = Result<(ServerInfo, String), String>;

pub(super) struct RunsOn {
    /// Where new workspaces go, as saved.
    choice: Backend,
    /// The option shown: Command shows its field before a command is saved.
    kind: Kind,
    /// WSL's distros and whether each has `gentle-shell`; `None` while
    /// looking.
    distros: Option<Vec<(Distro, bool)>>,
    command: Entity<InputState>,
    _command: Subscription,
    /// Whether the default (not a server) has `gentle-shell`, once checked.
    found: Option<bool>,
    checking: bool,
    task: Task<()>,
    check: Task<()>,
    /// The servers this device is paired with.
    paired: Vec<Backend>,
    /// What each paired server answered, by address; missing while asked.
    answers: HashMap<String, Answer>,
    ask: Task<()>,
    /// Pairing another server: the Code field shows.
    adding: bool,
    /// Connecting with a server's code (or its address).
    connect: Entity<ServerConnect>,
    _connect: Subscription,
    /// The server being unpaired (its address): asking it to remove this
    /// device, then forgetting its token.
    unpairing: Option<String>,
    unpair: Task<()>,
    /// What the last Unpair did (the server removed this device, or why it
    /// still lists it).
    unpaired: Option<String>,
}

impl RunsOn {
    pub(super) fn new(window: &mut Window, cx: &mut Context<SettingsView>) -> Self {
        let choice = backend::default_machine();
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
        let connect = cx.new(|cx| ServerConnect::new(String::new(), window, cx));
        let paired = cx.subscribe(&connect, |this, _, event: &ConnectEvent, cx| {
            let ConnectEvent::Paired(choice, info) = event;
            this.server_paired(choice.clone(), info.clone(), cx);
        });
        let paired_servers = backend::server::paired();
        Self {
            kind: Kind::of(&choice),
            adding: paired_servers.is_empty(),
            paired: paired_servers,
            choice,
            distros: None,
            command,
            _command: subscription,
            found: None,
            checking: false,
            task: Task::ready(()),
            check: Task::ready(()),
            answers: HashMap::new(),
            ask: Task::ready(()),
            connect,
            _connect: paired,
            unpairing: None,
            unpair: Task::ready(()),
            unpaired: None,
        }
    }
}

/// Connecting this device to a Gentle Shell server: the Code field (the
/// short pairing code `gentle-shell serve share` shows, whose server is found
/// on the network, or the long connection code it prints as a fallback,
/// which connects when pasted), the servers that answered when several did,
/// and, under Enter address manually, the address and the code `gentle-shell
/// serve pair` printed. Configuration's Where agents run and the first-run
/// screens show it; once paired (the token saved) it says so with
/// [`ConnectEvent::Paired`] and its owner saves where agents run.
pub(crate) struct ServerConnect {
    code_field: Entity<InputState>,
    _code_field: Subscription,
    /// Several servers on the network answered: the user picks the one
    /// the short code (the first) is for.
    choices: Option<(String, Vec<Nearby>)>,
    /// Whether the address and pairing code fields show (Enter address
    /// manually).
    manual: bool,
    server_url: Entity<InputState>,
    server_code: Entity<InputState>,
    _server_code: Subscription,
    pairing: bool,
    /// Where connecting with a code is ("Looking for computers on your
    /// network…").
    progress: Option<String>,
    pair_error: Option<String>,
    pair: Task<()>,
}

pub(crate) enum ConnectEvent {
    /// Paired with this server, which said this about itself.
    Paired(Backend, ServerInfo),
}

impl EventEmitter<ConnectEvent> for ServerConnect {}

impl ServerConnect {
    /// `url`: the address field's first value (the saved server's).
    pub(crate) fn new(url: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
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
                    InputEvent::Change if text.trim().is_empty() && !this.pairing => {
                        this.pair_error = None;
                        this.choices = None;
                        cx.notify();
                    }
                    InputEvent::PressEnter { .. } => this.connect_code(&text, cx),
                    _ => {}
                }
            });
        Self {
            code_field,
            _code_field: field_subscription,
            choices: None,
            manual: false,
            server_url,
            server_code,
            _server_code: code_subscription,
            pairing: false,
            progress: None,
            pair_error: None,
            pair: Task::ready(()),
        }
    }

    /// Pair with the server at the typed address using the typed code
    /// (`POST /api/pair`). A connection code typed in either field connects
    /// with it instead.
    fn pair_server(&mut self, cx: &mut Context<Self>) {
        if self.pairing {
            return;
        }
        let url = self.server_url.read(cx).value().to_string();
        let code = self.server_code.read(cx).value().to_string();
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
        self.pairing = true;
        self.pair_error = None;
        self.pair = cx.spawn(async move |this, cx| {
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
        if self.pairing {
            return;
        }
        self.choices = None;
        let code = match gentle_core::serve::read_code(text) {
            Ok(Entered::Connection(code)) => code,
            Ok(Entered::Short(code)) => return self.find_nearby(code, cx),
            Err(error) => {
                self.pair_error = Some(error.to_string());
                cx.notify();
                return;
            }
        };
        self.pairing = true;
        self.pair_error = None;
        self.progress = Some(format!("Looking for {} on your network…", code.name));
        self.pair = cx.spawn(async move |this, cx| {
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
                this.progress = Some(format!("Pairing with {}…", code.name));
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
        self.pairing = true;
        self.pair_error = None;
        self.progress = Some("Looking for computers on your network…".into());
        self.pair = cx.spawn(async move |this, cx| {
            let found = cx
                .background_executor()
                .spawn(async move { backend::server::nearby() })
                .await;
            let server = match found.map(gentle_core::serve::pick) {
                Ok(Pick::One(server)) => server,
                Ok(Pick::Many(servers)) => {
                    let _ = this.update(cx, |this, cx| {
                        this.pairing = false;
                        this.progress = None;
                        this.choices = Some((code, servers));
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
                this.progress = Some(format!("Pairing with {}…", server.name));
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
        if self.pairing {
            return;
        }
        self.choices = None;
        self.pairing = true;
        self.pair_error = None;
        self.progress = Some(format!("Pairing with {}…", server.name));
        self.pair = cx.spawn(async move |this, cx| {
            let paired = cx
                .background_executor()
                .spawn(async move { backend::server::pair_nearby(&server, &code) })
                .await;
            let _ = this.update(cx, |this, cx| this.paired(paired, cx));
        });
        cx.notify();
    }

    /// A pairing ended: say so, or why it didn't pair.
    fn paired(&mut self, paired: Result<(Backend, ServerInfo), String>, cx: &mut Context<Self>) {
        self.pairing = false;
        self.progress = None;
        match paired {
            Ok((choice, info)) => cx.emit(ConnectEvent::Paired(choice, info)),
            Err(error) => self.pair_error = Some(error),
        }
        cx.notify();
    }
}

impl SettingsView {
    /// Look for WSL's distros (Windows), check the default and ask every
    /// paired server whether it answers.
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
        self.ask_servers(cx);
    }

    /// Whether the default (not a server) has `gentle-shell`.
    fn check_runs_on(&mut self, cx: &mut Context<Self>) {
        let choice = self.runs_on.choice.clone();
        if choice == Backend::Local || choice.is_server() {
            self.runs_on.found = None;
            self.runs_on.checking = false;
            return;
        }
        self.runs_on.checking = true;
        self.runs_on.check = cx.spawn(async move |this, cx| {
            let found = cx
                .background_executor()
                .spawn({
                    let choice = choice.clone();
                    async move { backend::has_gentle_shell(&choice) }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.runs_on.choice == choice {
                    this.runs_on.found = Some(found);
                    this.runs_on.checking = false;
                    cx.notify();
                }
            });
        });
    }

    /// List the paired servers again and ask each what it is and where it
    /// answers now, together.
    fn ask_servers(&mut self, cx: &mut Context<Self>) {
        self.runs_on.paired = backend::server::paired();
        self.runs_on.answers.clear();
        let servers = self.runs_on.paired.clone();
        self.runs_on.ask = cx.spawn(async move |this, cx| {
            let asked: Vec<_> = servers
                .into_iter()
                .map(|server| {
                    cx.background_executor().spawn(async move {
                        let Backend::Server { url, .. } = &server else {
                            return None;
                        };
                        let info = backend::server::info(&server);
                        // Asked after the answer: where it was found, should
                        // its address have changed.
                        let address = backend::server::address(url);
                        Some((url.clone(), info.map(|info| (info, address))))
                    })
                })
                .collect();
            for answer in asked {
                let Some((url, answer)) = answer.await else {
                    continue;
                };
                let _ = this.update(cx, |this, cx| {
                    this.runs_on.answers.insert(url, answer);
                    cx.notify();
                });
            }
        });
        cx.notify();
    }

    /// Save where new workspaces go and check it has `gentle-shell`. Open
    /// workspaces stay where they are; no server is forgotten.
    fn choose_runs_on(&mut self, choice: Backend, cx: &mut Context<Self>) {
        self.runs_on.kind = Kind::of(&choice);
        if self.runs_on.choice == choice {
            cx.notify();
            return;
        }
        if let Err(err) = backend::save_default(&choice) {
            eprintln!("gentle-shell-gpui: could not save where agents run: {err}");
        }
        self.runs_on.choice = choice;
        self.runs_on.found = None;
        self.check_runs_on(cx);
        cx.notify();
    }

    /// This device paired with `server` (which said `info` about itself):
    /// it is listed; it is where new workspaces go when "Gentle Shell
    /// server" was picked without one. The window's tabs that let go of it
    /// (a server paired again) follow it.
    fn server_paired(&mut self, server: Backend, info: ServerInfo, cx: &mut Context<Self>) {
        self.runs_on.unpaired = None;
        self.runs_on.adding = false;
        if self.runs_on.kind == Kind::Server && !self.runs_on.choice.is_server() {
            self.choose_runs_on(server.clone(), cx);
        }
        self.ask_servers(cx);
        if let Backend::Server { url, .. } = &server {
            let address = backend::server::address(url);
            self.runs_on
                .answers
                .insert(url.clone(), Ok((info, address)));
        }
        cx.emit(SettingsEvent::Paired(server));
        cx.notify();
    }

    /// Unpair this device from `server`: the server is asked to remove it,
    /// then its token is forgotten here (also when the server doesn't
    /// answer: it then still lists the device, and says how to remove it
    /// there). The window's tabs and terminals on it let go of it; its
    /// sessions keep running there. When it was where new workspaces go,
    /// they go to this computer from now on.
    fn unpair_server(&mut self, server: Backend, cx: &mut Context<Self>) {
        let Backend::Server { url, .. } = &server else {
            return;
        };
        if self.runs_on.unpairing.is_some() {
            return;
        }
        let name = gentle_core::machines::name(&server);
        let key = backend::server::key(&server).unwrap_or_else(|| url.clone());
        self.runs_on.unpairing = Some(url.clone());
        self.runs_on.unpaired = None;
        self.runs_on.unpair = cx.spawn(async move |this, cx| {
            let unpaired = cx
                .background_executor()
                .spawn({
                    let server = server.clone();
                    async move { backend::server::unpair(&server) }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.runs_on.unpairing = None;
                let unpairing = match unpaired {
                    Ok(unpairing) => unpairing,
                    Err(err) => {
                        this.runs_on.unpaired = Some(format!(
                            "Couldn't forget this computer's token for {name}: {err}"
                        ));
                        cx.notify();
                        return;
                    }
                };
                let revoked = unpairing.still_listed.is_none();
                this.runs_on.unpaired = Some(match unpairing.still_listed {
                    None => format!(
                        "Unpaired from {name}, which no longer lists this computer and closed \
                         its terminals there. Its agent sessions keep running; this window's \
                         tabs let go of them."
                    ),
                    Some(why) => format!(
                        "Unpaired here. {why} Its sessions and terminals keep running there; \
                         this window's tabs let go of them."
                    ),
                });
                cx.emit(SettingsEvent::ServerUnpaired {
                    server: key,
                    addresses: unpairing.addresses,
                    name,
                    revoked,
                });
                if gentle_core::machines::same(&this.runs_on.choice, &server) {
                    this.choose_runs_on(Backend::Local, cx);
                    this.runs_on.kind = Kind::Server;
                }
                this.ask_servers(cx);
            });
        });
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
            // The only paired server is the one; with several, or none,
            // the user picks (or pairs) one.
            Kind::Server if self.runs_on.paired.len() == 1 => {
                self.choose_runs_on(self.runs_on.paired[0].clone(), cx)
            }
            Kind::Command | Kind::Server => {
                self.runs_on.kind = kind;
                if kind == Kind::Server && self.runs_on.paired.is_empty() {
                    self.runs_on.adding = true;
                }
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
            Kind::Server => self.render_default_server(cx).into_any_element(),
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
                            "Where new workspaces run: a command that runs a program in that \
                             environment, as argv (OrbStack: orb -m dev --, Lima: limactl shell \
                             dev, a container: docker exec -i dev). Press Enter to use it. Open \
                             workspaces keep the machine they were opened on."
                        }
                        Kind::Server => {
                            "New workspaces open on this server: it runs their agents and keeps \
                             them running while this app is closed. Their folders, files, git \
                             and terminals are the server's. Open workspaces keep the machine \
                             they were opened on, so this computer's and a server's can sit \
                             side by side."
                        }
                        _ => {
                            "New workspaces run here; open workspaces keep the machine they \
                             were opened on, and paired servers stay paired. A project opened \
                             from \\\\wsl.localhost\\<distro>\\ always runs in that distro."
                        }
                    }),
            )
            .child(self.render_paired(cx))
    }

    /// "Gentle Shell server" picked: the paired servers to send new
    /// workspaces to (the chosen one marked), or a word that one must be
    /// paired first (below).
    fn render_default_server(&self, cx: &mut Context<Self>) -> Div {
        if self.runs_on.paired.is_empty() {
            return div()
                .pt_2()
                .text_xs()
                .text_color(ui::muted())
                .child("Pair a server below: new workspaces then open on it.");
        }
        let rows = self.runs_on.paired.iter().enumerate().map(|(ix, server)| {
            let chosen = gentle_core::machines::same(server, &self.runs_on.choice);
            let pick = server.clone();
            ui::picker_row(
                ("runs-on-default", ix),
                false,
                chosen,
                Some(IconName::Server),
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(if chosen { ui::fg() } else { ui::muted() })
                    .child(gentle_core::machines::name(server)),
            )
            .on_click(cx.listener(move |this, _, _, cx| this.choose_runs_on(pick.clone(), cx)))
        });
        control(
            "Server",
            false,
            v_flex().flex_1().min_w_0().gap_px().children(rows),
        )
    }

    /// The paired servers, each with its address, whether it answers and
    /// Unpair; what the last Unpair did; and pairing another (the Code
    /// field, open while there is none).
    fn render_paired(&self, cx: &mut Context<Self>) -> Div {
        let rows = self.runs_on.paired.iter().enumerate().map(|(ix, server)| {
            let Backend::Server { url, device, .. } = server else {
                return div().into_any_element();
            };
            let answer = self.runs_on.answers.get(url);
            let address = match answer {
                Some(Ok((_, address))) => address.clone(),
                _ => url.clone(),
            };
            let (status, color, icon) = match answer {
                None => ("Checking…".to_string(), ui::muted(), IconName::Info),
                Some(Ok((info, _))) => (
                    match &info.version {
                        Some(version) => format!("Answers · Gentle Shell {version}"),
                        None => "Answers".to_string(),
                    },
                    ui::success(),
                    IconName::CircleCheck,
                ),
                Some(Err(error)) => (error.clone(), ui::warning(), IconName::CircleAlert),
            };
            let unpairing = self.runs_on.unpairing.as_deref() == Some(url.as_str());
            let target = server.clone();
            v_flex()
                .pt_2()
                .gap_1()
                .child(
                    h_flex()
                        .gap_2()
                        .child(Icon::new(IconName::Server).xsmall().text_color(ui::faint()))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_sm()
                                .text_color(ui::fg())
                                .child(gentle_core::machines::name(server)),
                        )
                        .child(
                            Button::new(("runs-on-unpair", ix))
                                .xsmall()
                                .ghost()
                                .icon(Icon::new(IconName::Unlink))
                                .label(if unpairing { "Unpairing…" } else { "Unpair" })
                                .loading(unpairing)
                                .disabled(self.runs_on.unpairing.is_some())
                                .tooltip(
                                    "Remove this computer from the server. Its sessions and \
                                     terminals keep running there.",
                                )
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.unpair_server(target.clone(), cx)
                                })),
                        ),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .items_start()
                        .text_xs()
                        .text_color(color)
                        .child(Icon::new(icon).xsmall())
                        .child(div().min_w_0().child(status)),
                )
                .child(row("Address", address))
                .child(row("This device", device.clone()))
                .into_any_element()
        });
        let adding = self.runs_on.adding;
        v_flex()
            .pt_4()
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(ui::fg())
                    .child("Paired servers"),
            )
            .when(self.runs_on.paired.is_empty(), |d| {
                d.child(
                    div()
                        .pt_1()
                        .text_xs()
                        .text_color(ui::muted())
                        .child("None yet."),
                )
            })
            .children(rows)
            .when_some(self.runs_on.unpaired.clone(), |this, unpaired| {
                this.child(
                    h_flex()
                        .pt_2()
                        .gap_2()
                        .items_start()
                        .text_xs()
                        .text_color(ui::muted())
                        .child(Icon::new(IconName::Info).xsmall())
                        .child(div().min_w_0().child(unpaired)),
                )
            })
            .when(!adding, |d| {
                d.child(
                    h_flex().pt_2().child(
                        Button::new("runs-on-pair-another")
                            .small()
                            .ghost()
                            .icon(Icon::new(IconName::Plus))
                            .label("Pair another server")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.runs_on.adding = true;
                                cx.notify();
                            })),
                    ),
                )
            })
            .when(adding, |d| {
                d.child(div().pt_2().text_xs().text_color(ui::faint()).child(
                    "On the other computer, run gentle-shell serve share and enter the \
                             code it shows (from outside its network, paste the long code: it \
                             goes through the server's relay). The connection is encrypted and \
                             pinned to that server's certificate.",
                ))
                .child(self.runs_on.connect.clone())
            })
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

    /// Whether the default (not a server) has `gentle-shell`, or whether
    /// the default server answers, under the controls.
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
        // The default's status shows under its own option only.
        if Kind::of(&self.runs_on.choice) != self.runs_on.kind {
            return div();
        }
        let label = self.runs_on.choice.label();
        let checking = || {
            h_flex()
                .pt_2()
                .gap_2()
                .text_xs()
                .text_color(ui::muted())
                .child(Spinner::new().xsmall().color(ui::muted()))
                .child(format!("Checking {label}…"))
        };
        let ready = || {
            line(
                IconName::CircleCheck,
                ui::success(),
                format!("New workspaces run on {label}."),
            )
        };
        match &self.runs_on.choice {
            Backend::Local => div(),
            Backend::Server { url, .. } => match self.runs_on.answers.get(url) {
                None => checking(),
                Some(Ok(_)) => ready(),
                Some(Err(error)) => line(IconName::CircleAlert, ui::warning(), error.clone()),
            },
            _ => match (self.runs_on.checking, self.runs_on.found) {
                (true, _) => checking(),
                (false, Some(true)) => ready(),
                (false, Some(false)) => line(
                    IconName::CircleAlert,
                    ui::warning(),
                    format!("gentle-shell wasn't found on {label}. Install it there first."),
                ),
                (false, None) => div(),
            },
        }
    }
}

impl Render for ServerConnect {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pairing = self.pairing;
        let connecting = pairing && self.progress.is_some();
        let manual = self.manual;
        let manual_fields = v_flex()
            .child(control(
                "Address",
                false,
                ui::field()
                    .flex_1()
                    .min_w_0()
                    .h(rems(30. / 16.))
                    .child(Icon::new(IconName::Server).small().text_color(ui::faint()))
                    .child(Input::new(&self.server_url).appearance(false).small()),
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
                            .child(Input::new(&self.server_code).appearance(false).small()),
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
                            .child(Input::new(&self.code_field).appearance(false).small()),
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
                                let text = this.code_field.read(cx).value().to_string();
                                this.connect_code(&text, cx);
                            })),
                    ),
            ))
            .children(self.progress.clone().map(|progress| {
                h_flex()
                    .pt_2()
                    .gap_2()
                    .text_xs()
                    .text_color(ui::muted())
                    .child(Spinner::new().xsmall().color(ui::muted()))
                    .child(div().min_w_0().child(progress))
            }))
            .children(self.pair_error.clone().map(|error| {
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
                        this.manual = !this.manual;
                        cx.notify();
                    })),
            )
            .when(manual, |d| d.child(manual_fields))
    }
}

impl ServerConnect {
    /// The servers that answered on the network, when several did: one
    /// row each (its name and address) to pair the short code with.
    fn render_nearby(&self, cx: &mut Context<Self>) -> Option<Div> {
        let (code, servers) = self.choices.as_ref()?;
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
}
