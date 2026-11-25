//! Models › Sign-ins on a Gentle Shell server: sign in to a provider there
//! (`login_start`), set or remove an API key (`set_api_key`,
//! `remove_credentials`), when its Shell offers them; otherwise the screen
//! keeps saying "Do this on <server>".
//!
//! A sign-in runs Pi's own login on the server. Its steps come back as
//! `login_prompt` events: a page to open (shown in a browser card here; when
//! the login waits for the provider's redirect to the server's loopback
//! callback, `redirectPrefix`, the card never loads it and sends the whole
//! URL back with `login_complete`, as that callback would have received
//! it), a device code (with a copy button), a question (a field, or its
//! choices) and progress lines; `login_finished` ends it. The credentials
//! are saved there, and the server's sessions pick them up without
//! restarting. A key typed here goes only to the server, is cleared from
//! the field at once and never shown or logged.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use pi_rpc::desktop_rpc::{
    AuthSource, AuthType, DesktopMethod, LoginKind, LoginPrompt, LoginPromptKind, ProviderAuth,
};

use super::layout::section;
use super::{SettingsEvent, SettingsView};
use crate::features::browser::WebSurface;
use crate::features::session::{
    AccountEvent, ReadOutcome, ReadRequest, ReadSupport, ReadTicket, Secret,
};
use crate::shared::ui;

/// Progress lines kept for a sign-in.
const MAX_LINES: usize = 50;
/// Events kept while `login_start` has not answered yet.
const MAX_EARLY: usize = 32;
/// Above this many providers the list gets a filter.
const FILTER_FROM: usize = 8;

/// Where a sign-in is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Step {
    /// `login_start` asked; no login id yet.
    Starting,
    /// Waiting for the user or the provider: the prompt shows.
    Running,
    /// An answer, or the redirect the browser card caught, was sent.
    Answering,
    /// A cancel was asked.
    Cancelling,
    Done(Result<(), String>),
}

/// One sign-in on the server, as the screen follows it.
#[derive(Debug)]
pub(super) struct LoginFlow {
    pub(super) provider: String,
    pub(super) name: String,
    pub(super) login_id: Option<String>,
    pub(super) step: Step,
    /// The step waiting for the user: a page, a device code or a question.
    pub(super) prompt: Option<LoginPrompt>,
    pub(super) lines: Vec<String>,
    /// Events that arrived before `login_start` answered (any login's).
    early: Vec<AccountEvent>,
    cancel_when_started: bool,
}

impl LoginFlow {
    pub(super) fn new(provider: String, name: String) -> Self {
        Self {
            provider,
            name,
            login_id: None,
            step: Step::Starting,
            prompt: None,
            lines: Vec::new(),
            early: Vec::new(),
            cancel_when_started: false,
        }
    }

    pub(super) fn finished(&self) -> bool {
        matches!(self.step, Step::Done(_))
    }

    /// `login_start` answered with `login_id`: the events that came first
    /// apply now. The login id to cancel at once, when a cancel was asked
    /// meanwhile.
    pub(super) fn started(&mut self, login_id: String) -> Option<String> {
        if self.finished() {
            return None;
        }
        self.login_id = Some(login_id.clone());
        if self.cancel_when_started {
            return Some(login_id);
        }
        self.step = Step::Running;
        for event in std::mem::take(&mut self.early) {
            self.apply(&event);
        }
        None
    }

    /// One sign-in event: whether it was this login's.
    pub(super) fn apply(&mut self, event: &AccountEvent) -> bool {
        let ours = |id: &str| self.login_id.as_deref() == Some(id);
        match event {
            AccountEvent::LoginPrompt(_) | AccountEvent::LoginFinished(_)
                if self.login_id.is_none() && self.step == Step::Starting =>
            {
                if self.early.len() < MAX_EARLY {
                    self.early.push(event.clone());
                }
                false
            }
            AccountEvent::LoginPrompt(prompt) if ours(&prompt.login_id) && !self.finished() => {
                if prompt.kind == LoginPromptKind::Progress {
                    if let Some(message) = prompt.message.as_ref().filter(|m| !m.is_empty()) {
                        if self.lines.len() >= MAX_LINES {
                            self.lines.remove(0);
                        }
                        self.lines.push(message.clone());
                    }
                } else {
                    self.prompt = Some(prompt.clone());
                    if self.step == Step::Answering {
                        self.step = Step::Running;
                    }
                }
                true
            }
            AccountEvent::LoginFinished(finished) if ours(&finished.login_id) => {
                self.step = Step::Done(if finished.ok {
                    Ok(())
                } else {
                    Err(finished
                        .error
                        .clone()
                        .filter(|error| !error.is_empty())
                        .unwrap_or_else(|| "The sign-in didn't finish.".into()))
                });
                self.prompt = None;
                true
            }
            _ => false,
        }
    }

    /// A request of this sign-in failed: it is over.
    pub(super) fn failed(&mut self, message: String) {
        self.step = Step::Done(Err(message));
        self.prompt = None;
    }

    /// The login to answer the prompt of, marking the answer sent.
    pub(super) fn answer(&mut self) -> Option<String> {
        if self.step != Step::Running || self.prompt.is_none() {
            return None;
        }
        self.step = Step::Answering;
        self.login_id.clone()
    }

    /// The server refused an answer: the prompt shows again, with why.
    pub(super) fn answer_refused(&mut self, message: String) {
        if self.step == Step::Answering {
            self.step = Step::Running;
            self.lines.push(message);
        }
    }

    /// The browser card caught a navigation to `url`: the login to send it
    /// to, when it is the redirect the page shown waits for.
    pub(super) fn caught(&mut self, url: &str) -> Option<String> {
        let prefix = self
            .prompt
            .as_ref()
            .filter(|prompt| prompt.kind == LoginPromptKind::OpenUrl)?
            .redirect_prefix
            .as_deref()?;
        if !gentle_core::sign_in::is_login_redirect(prefix, url) {
            return None;
        }
        self.answer()
    }

    /// Cancel: the login to cancel now, if its id is known (one still
    /// starting is cancelled once `login_start` answers).
    pub(super) fn cancel(&mut self) -> Option<String> {
        match self.step {
            Step::Done(_) | Step::Cancelling => None,
            Step::Starting => {
                self.cancel_when_started = true;
                self.step = Step::Cancelling;
                None
            }
            Step::Running | Step::Answering => {
                self.step = Step::Cancelling;
                self.login_id.clone()
            }
        }
    }

    /// The page the browser card shows, and the redirect it must catch:
    /// the prompt's while it waits for the user.
    pub(super) fn page(&self) -> Option<(String, Option<String>)> {
        if self.step != Step::Running {
            return None;
        }
        let prompt = self.prompt.as_ref()?;
        match prompt.kind {
            LoginPromptKind::OpenUrl | LoginPromptKind::DeviceCode => Some((
                prompt.url.clone()?,
                prompt
                    .redirect_prefix
                    .clone()
                    .filter(|_| prompt.kind == LoginPromptKind::OpenUrl),
            )),
            _ => None,
        }
    }
}

/// What a ticket asked.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Asked {
    Start,
    Answer,
    Cancel,
    Key(String),
    Remove(String),
}

/// The browser card of a sign-in: the page, the redirect it catches, and
/// what listens for it.
struct LoginPage {
    key: (String, Option<String>),
    surface: Entity<WebSurface>,
    _caught: Task<()>,
}

/// Models › Sign-ins on a server: the sign-in shown, the requests in
/// flight and the fields.
pub(super) struct ServerSignIn {
    pub(super) flow: Option<LoginFlow>,
    tickets: Vec<(ReadTicket, Asked)>,
    page: Option<LoginPage>,
    /// The provider whose API key field is open.
    key_for: Option<String>,
    /// The last key or removal result: ok, and the words.
    notice: Option<(bool, String)>,
    key: Entity<InputState>,
    answer: Entity<InputState>,
    secret_answer: Entity<InputState>,
    filter: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

impl ServerSignIn {
    pub(super) fn new(window: &mut Window, cx: &mut Context<SettingsView>) -> Self {
        let key = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("API key")
        });
        let answer = cx.new(|cx| InputState::new(window, cx));
        let secret_answer = cx.new(|cx| InputState::new(window, cx).masked(true));
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Find a provider"));
        let submit = |this: &mut SettingsView,
                      input: &Entity<InputState>,
                      event: &InputEvent,
                      window: &mut Window,
                      cx: &mut Context<SettingsView>| {
            if let InputEvent::PressEnter { .. } = event {
                this.submit_login_answer(input.clone(), window, cx);
            }
        };
        let subscriptions = vec![
            cx.subscribe_in(&answer, window, submit),
            cx.subscribe_in(&secret_answer, window, submit),
            cx.subscribe_in(&key, window, |this, _, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = event
                    && let Some(provider) = this.server_sign_in.key_for.clone()
                {
                    this.save_api_key(provider, window, cx);
                }
            }),
            cx.subscribe_in(&filter, window, |_, _, _: &InputEvent, _, cx| cx.notify()),
        ];
        Self {
            flow: None,
            tickets: Vec::new(),
            page: None,
            key_for: None,
            notice: None,
            key,
            answer,
            secret_answer,
            filter,
            _subscriptions: subscriptions,
        }
    }
}

impl SettingsView {
    /// Whether the server's Shell offers `method` (its control session's
    /// capabilities).
    pub(super) fn server_offers(&self, method: DesktopMethod, cx: &App) -> bool {
        self.server().is_some()
            && self
                .session
                .as_ref()
                .is_some_and(|session| session.read(cx).read_support(method) == ReadSupport::Rpc)
    }

    /// Ask the control session; the ticket, or why not.
    pub(super) fn ask_server(
        &mut self,
        request: ReadRequest,
        cx: &mut Context<Self>,
    ) -> Result<ReadTicket, String> {
        let session = self
            .session
            .clone()
            .ok_or_else(|| "No Gentle Shell session to ask.".to_string())?;
        session
            .update(cx, |session, cx| session.read(request, cx))
            .map_err(|failure| failure.message())
    }

    /// Sign in to `provider` on the server.
    fn start_login(&mut self, provider: String, name: String, cx: &mut Context<Self>) {
        if self
            .server_sign_in
            .flow
            .as_ref()
            .is_some_and(|flow| !flow.finished())
        {
            return;
        }
        let mut flow = LoginFlow::new(provider.clone(), name);
        match self.ask_server(ReadRequest::LoginStart { provider }, cx) {
            Ok(ticket) => self.server_sign_in.tickets.push((ticket, Asked::Start)),
            Err(why) => flow.failed(why),
        }
        self.server_sign_in.flow = Some(flow);
        self.server_sign_in.notice = None;
        self.sync_login_page(cx);
        cx.notify();
    }

    /// Send `value` as the answer to the prompt shown.
    fn answer_login(&mut self, value: String, cx: &mut Context<Self>) {
        let Some(login_id) = self
            .server_sign_in
            .flow
            .as_mut()
            .and_then(LoginFlow::answer)
        else {
            return;
        };
        self.send_login_answer(login_id, value, cx);
    }

    fn send_login_answer(&mut self, login_id: String, value: String, cx: &mut Context<Self>) {
        match self.ask_server(
            ReadRequest::LoginComplete {
                login_id,
                value: Secret(value),
            },
            cx,
        ) {
            Ok(ticket) => self.server_sign_in.tickets.push((ticket, Asked::Answer)),
            Err(why) => {
                if let Some(flow) = self.server_sign_in.flow.as_mut() {
                    flow.answer_refused(why);
                }
            }
        }
        self.sync_login_page(cx);
        cx.notify();
    }

    /// The answer typed in `input` (Enter, or Continue).
    fn submit_login_answer(
        &mut self,
        input: Entity<InputState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let value = input.read(cx).value().trim().to_string();
        if value.is_empty() {
            return;
        }
        input.update(cx, |state, cx| state.set_value("", window, cx));
        self.answer_login(value, cx);
    }

    /// The browser card caught a navigation to `url`.
    fn redirect_caught(&mut self, url: String, cx: &mut Context<Self>) {
        let Some(login_id) = self
            .server_sign_in
            .flow
            .as_mut()
            .and_then(|flow| flow.caught(&url))
        else {
            return;
        };
        self.send_login_answer(login_id, url, cx);
    }

    fn cancel_login(&mut self, cx: &mut Context<Self>) {
        let Some(login_id) = self
            .server_sign_in
            .flow
            .as_mut()
            .and_then(LoginFlow::cancel)
        else {
            self.sync_login_page(cx);
            cx.notify();
            return;
        };
        self.send_login_cancel(login_id, cx);
    }

    fn send_login_cancel(&mut self, login_id: String, cx: &mut Context<Self>) {
        match self.ask_server(ReadRequest::LoginCancel { login_id }, cx) {
            Ok(ticket) => self.server_sign_in.tickets.push((ticket, Asked::Cancel)),
            Err(_) => {
                if let Some(flow) = self.server_sign_in.flow.as_mut() {
                    flow.failed("Cancelled.".into());
                }
            }
        }
        self.sync_login_page(cx);
        cx.notify();
    }

    /// Save the key typed for `provider`; the field is cleared at once.
    fn save_api_key(&mut self, provider: String, window: &mut Window, cx: &mut Context<Self>) {
        let key = self.server_sign_in.key.read(cx).value().trim().to_string();
        if key.is_empty() {
            return;
        }
        self.server_sign_in
            .key
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.server_sign_in.key_for = None;
        let asked = Asked::Key(provider.clone());
        self.server_sign_in.notice = match self.ask_server(
            ReadRequest::SetApiKey {
                provider,
                key: Secret(key),
            },
            cx,
        ) {
            Ok(ticket) => {
                self.server_sign_in.tickets.push((ticket, asked));
                None
            }
            Err(why) => Some((false, why)),
        };
        cx.notify();
    }

    fn remove_server_credentials(&mut self, provider: String, cx: &mut Context<Self>) {
        let asked = Asked::Remove(provider.clone());
        self.server_sign_in.notice =
            match self.ask_server(ReadRequest::RemoveCredentials { provider }, cx) {
                Ok(ticket) => {
                    self.server_sign_in.tickets.push((ticket, asked));
                    None
                }
                Err(why) => Some((false, why)),
            };
        cx.notify();
    }

    /// Credentials changed on the server: read the sign-ins again, and let
    /// the agent tabs pick them up.
    fn server_credentials_changed(&mut self, cx: &mut Context<Self>) {
        self.ask_sign_ins(cx);
        cx.emit(SettingsEvent::CredentialsChanged);
    }

    /// The control session changed: the answers to this section's requests
    /// and the sign-in and package events, once each.
    pub(super) fn server_accounts_changed(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            return;
        };
        let tickets = std::mem::take(&mut self.server_sign_in.tickets);
        let (outcomes, events) = session.update(cx, |session, _| {
            let mut outcomes = Vec::new();
            for (ticket, asked) in tickets {
                match session.take_read(ticket) {
                    Some(outcome) => outcomes.push((asked, outcome)),
                    None => self.server_sign_in.tickets.push((ticket, asked)),
                }
            }
            (outcomes, session.take_account_events())
        });
        if outcomes.is_empty() && events.is_empty() {
            self.package_outcomes(cx);
            return;
        }
        for (asked, outcome) in outcomes {
            self.sign_in_outcome(asked, outcome, cx);
        }
        let mut finished_ok = false;
        if let Some(flow) = self.server_sign_in.flow.as_mut() {
            for event in &events {
                let was = flow.finished();
                if flow.apply(event) && !was && flow.step == Step::Done(Ok(())) {
                    finished_ok = true;
                }
            }
        }
        if finished_ok {
            self.server_credentials_changed(cx);
        }
        self.package_events(&events, cx);
        self.package_outcomes(cx);
        self.sync_login_page(cx);
        cx.notify();
    }

    fn sign_in_outcome(&mut self, asked: Asked, outcome: ReadOutcome, cx: &mut Context<Self>) {
        let failure = |outcome: &ReadOutcome| match outcome {
            ReadOutcome::Failed(failure) => Some(failure.message()),
            _ => None,
        };
        match asked {
            Asked::Start => {
                let Some(flow) = self.server_sign_in.flow.as_mut() else {
                    return;
                };
                match outcome {
                    ReadOutcome::LoginStarted(login_id) => {
                        if let Some(cancel) = flow.started(login_id) {
                            self.send_login_cancel(cancel, cx);
                        }
                    }
                    other => flow.failed(
                        failure(&other).unwrap_or_else(|| "The sign-in didn't start.".into()),
                    ),
                }
            }
            Asked::Answer => {
                if let (Some(why), Some(flow)) =
                    (failure(&outcome), self.server_sign_in.flow.as_mut())
                {
                    flow.answer_refused(why);
                }
            }
            Asked::Cancel => {
                // A login already gone answers with an error: it is over.
                if failure(&outcome).is_some()
                    && let Some(flow) = self.server_sign_in.flow.as_mut()
                    && !flow.finished()
                {
                    flow.failed("Cancelled.".into());
                }
            }
            Asked::Key(provider) => {
                self.server_sign_in.notice = Some(match outcome {
                    ReadOutcome::ApiKeySaved(true) => {
                        self.server_credentials_changed(cx);
                        (true, format!("Saved the key for {provider}."))
                    }
                    other => (
                        false,
                        failure(&other)
                            .unwrap_or_else(|| format!("The key for {provider} wasn't saved.")),
                    ),
                });
            }
            Asked::Remove(provider) => {
                self.server_sign_in.notice = Some(match outcome {
                    ReadOutcome::CredentialsRemoved(true) => {
                        self.server_credentials_changed(cx);
                        (true, format!("Removed the credentials for {provider}."))
                    }
                    ReadOutcome::CredentialsRemoved(false) => (
                        true,
                        format!("{provider} had no stored credentials to remove."),
                    ),
                    other => (
                        false,
                        failure(&other).unwrap_or_else(|| {
                            format!("The credentials for {provider} weren't removed.")
                        }),
                    ),
                });
            }
        }
    }

    /// Show the page the sign-in waits on in the browser card (built for
    /// each new page, catching its redirect), or drop the card.
    fn sync_login_page(&mut self, cx: &mut Context<Self>) {
        let wanted = self.server_sign_in.flow.as_ref().and_then(LoginFlow::page);
        if self.server_sign_in.page.as_ref().map(|page| &page.key) == wanted.as_ref() {
            return;
        }
        self.server_sign_in.page = wanted.map(|(url, prefix)| {
            let mut caught = None;
            let shown = self.page == super::navigation::SettingsPage::Models;
            let surface = cx.new(|cx| {
                let mut surface = WebSurface::new(url.clone(), None);
                surface.follow_popups(cx);
                if let Some(prefix) = &prefix {
                    caught = Some(surface.catch_login_redirects(prefix.clone()));
                }
                if !shown {
                    surface.hide(cx);
                }
                surface
            });
            let listen = match caught {
                Some(caught) => cx.spawn(async move |this, cx| {
                    while let Ok(url) = caught.recv().await {
                        if this
                            .update(cx, |this, cx| this.redirect_caught(url, cx))
                            .is_err()
                        {
                            return;
                        }
                    }
                }),
                None => Task::ready(()),
            };
            LoginPage {
                key: (url, prefix),
                surface,
                _caught: listen,
            }
        });
    }

    /// The browser card's native view only shows on Models.
    pub(super) fn login_page_shown(&mut self, shown: bool, cx: &mut Context<Self>) {
        if let Some(page) = &self.server_sign_in.page {
            page.surface.update(cx, |surface, cx| {
                if shown {
                    surface.show(cx);
                } else {
                    surface.hide(cx);
                }
            });
        }
    }

    /// Models › Sign-ins on the server, when its Shell signs in itself.
    pub(super) fn server_sign_in_section(&self, cx: &mut Context<Self>) -> Option<Div> {
        let server = self.server()?.name.clone();
        let login = self.server_offers(DesktopMethod::LoginStart, cx);
        let keys = self.server_offers(DesktopMethod::SetApiKey, cx);
        let remove = self.server_offers(DesktopMethod::RemoveCredentials, cx);
        if !login && !keys {
            return None;
        }
        let state = &self.server_sign_in;
        let busy = state.flow.as_ref().is_some_and(|flow| !flow.finished());
        let query = state.filter.read(cx).value().trim().to_lowercase();
        let status = self.sign_ins.status.as_ref();
        let mut providers: Vec<&ProviderAuth> = status
            .map(|status| {
                status
                    .providers
                    .iter()
                    .filter(|entry| entry.configured || !entry.login_kinds.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        let many = providers.len() > FILTER_FROM;
        providers.retain(|entry| {
            query.is_empty()
                || entry.provider.to_lowercase().contains(&query)
                || entry.display_name.to_lowercase().contains(&query)
        });
        // Signed in first, then by name, as the server sorted them.
        providers.sort_by_key(|entry| !entry.configured);
        Some(
            section(IconName::KeyRound, "Sign-ins")
                .child(
                    div()
                        .pb_2()
                        .text_xs()
                        .text_color(ui::muted())
                        .child(format!(
                            "Sign in to providers on {server}. Credentials go to its auth.json; \
                             its agents pick them up without restarting."
                        )),
                )
                .children(
                    state
                        .flow
                        .as_ref()
                        .map(|flow| self.login_panel(flow, &server, cx)),
                )
                .children(state.notice.as_ref().map(|(ok, text)| {
                    div()
                        .py_1()
                        .text_xs()
                        .text_color(if *ok { ui::success() } else { ui::danger() })
                        .child(text.clone())
                }))
                .when(many, |d| {
                    d.child(
                        div().pb_1().child(
                            ui::field()
                                .child(Icon::new(IconName::Search).small().text_color(ui::faint()))
                                .child(Input::new(&state.filter).appearance(false).small()),
                        ),
                    )
                })
                .when(status.is_none(), |d| {
                    d.child(
                        h_flex()
                            .py_1()
                            .gap_2()
                            .text_xs()
                            .text_color(ui::muted())
                            .child(Spinner::new().xsmall().color(ui::accent()))
                            .child(format!("Asking {server} which providers it knows…")),
                    )
                })
                .children(providers.into_iter().enumerate().map(|(index, entry)| {
                    self.provider_row(index, entry, busy, (login, keys, remove), cx)
                }))
                .when(status.is_some_and(|status| status.truncated), |d| {
                    d.child(
                        div()
                            .pt_1()
                            .text_xs()
                            .text_color(ui::faint())
                            .child("The server listed only its first 200 providers."),
                    )
                }),
        )
    }

    fn provider_row(
        &self,
        index: usize,
        entry: &ProviderAuth,
        busy: bool,
        (login, keys, remove): (bool, bool, bool),
        cx: &mut Context<Self>,
    ) -> Div {
        let provider = entry.provider.clone();
        let name = entry.display_name.clone();
        let browser = login
            && entry
                .login_kinds
                .iter()
                .any(|kind| matches!(kind, LoginKind::OAuth | LoginKind::Device));
        let api_key = keys && entry.login_kinds.contains(&LoginKind::ApiKey);
        let removable = remove && entry.configured && entry.source == Some(AuthSource::Stored);
        let editing = self.server_sign_in.key_for.as_deref() == Some(entry.provider.as_str());
        let signing = self
            .server_sign_in
            .flow
            .as_ref()
            .is_some_and(|flow| !flow.finished() && flow.provider == entry.provider);
        let status = entry.kind.map(|kind| match kind {
            AuthType::OAuth => "signed in",
            AuthType::ApiKey => "API key",
        });
        v_flex()
            .child(
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
                            .text_sm()
                            .text_color(ui::fg())
                            .child(name.clone()),
                    )
                    .children(status.map(|text| ui::chip(text, ui::success()).flex_shrink_0()))
                    .when(browser, |d| {
                        let (provider, name) = (provider.clone(), name.clone());
                        d.child(
                            Button::new(("server-sign-in", index))
                                .xsmall()
                                .ghost()
                                .icon(Icon::new(IconName::LogIn))
                                .label(if signing {
                                    "Signing in…"
                                } else if entry.configured {
                                    "Sign in again"
                                } else {
                                    "Sign in"
                                })
                                .loading(signing)
                                .disabled(busy)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.start_login(provider.clone(), name.clone(), cx)
                                })),
                        )
                    })
                    .when(api_key, |d| {
                        let provider = provider.clone();
                        d.child(
                            Button::new(("server-api-key", index))
                                .xsmall()
                                .ghost()
                                .icon(Icon::new(IconName::KeyRound))
                                .label("API key")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    let open = this.server_sign_in.key_for.as_deref()
                                        != Some(provider.as_str());
                                    this.server_sign_in.key_for = open.then(|| provider.clone());
                                    this.server_sign_in
                                        .key
                                        .update(cx, |state, cx| state.set_value("", window, cx));
                                    if open {
                                        this.server_sign_in
                                            .key
                                            .update(cx, |state, cx| state.focus(window, cx));
                                    }
                                    cx.notify();
                                })),
                        )
                    })
                    .when(removable, |d| {
                        let provider = provider.clone();
                        d.child(
                            Button::new(("server-remove-credentials", index))
                                .xsmall()
                                .ghost()
                                .icon(Icon::new(IconName::LogOut))
                                .tooltip("Remove the stored credentials")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.remove_server_credentials(provider.clone(), cx)
                                })),
                        )
                    }),
            )
            .when(editing, |d| {
                let provider = provider.clone();
                d.child(
                    h_flex()
                        .px_2()
                        .pb_2()
                        .gap_2()
                        .child(
                            ui::field()
                                .flex_1()
                                .min_w_0()
                                .child(Icon::new(IconName::Lock).small().text_color(ui::faint()))
                                .child(
                                    Input::new(&self.server_sign_in.key)
                                        .appearance(false)
                                        .small(),
                                ),
                        )
                        .child(
                            Button::new(("server-save-key", index))
                                .small()
                                .primary()
                                .label("Save")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.save_api_key(provider.clone(), window, cx)
                                })),
                        ),
                )
            })
    }

    /// The sign-in running (or just finished): its page, code or question,
    /// its progress, and cancel or close.
    fn login_panel(&self, flow: &LoginFlow, server: &str, cx: &mut Context<Self>) -> Div {
        let state = &self.server_sign_in;
        let mut panel = ui::card().gap_2().p_3().mb_2().child(
            h_flex()
                .gap_2()
                .child(Icon::new(IconName::LogIn).small().text_color(ui::accent()))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(ui::fg())
                        .child(format!("Signing in to {} on {server}", flow.name)),
                )
                .child(if flow.finished() {
                    Button::new("server-login-close")
                        .xsmall()
                        .ghost()
                        .label("Close")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.server_sign_in.flow = None;
                            this.sync_login_page(cx);
                            cx.notify();
                        }))
                } else {
                    Button::new("server-login-cancel")
                        .xsmall()
                        .ghost()
                        .icon(Icon::new(IconName::X))
                        .label("Cancel")
                        .disabled(flow.step == Step::Cancelling)
                        .on_click(cx.listener(|this, _, _, cx| this.cancel_login(cx)))
                }),
        );
        let waiting = |text: String| {
            h_flex()
                .gap_2()
                .text_xs()
                .text_color(ui::muted())
                .child(Spinner::new().xsmall().color(ui::accent()))
                .child(text)
        };
        match &flow.step {
            Step::Starting => panel = panel.child(waiting(format!("Starting on {server}…"))),
            Step::Answering => {
                panel = panel.child(waiting(format!("Finishing the sign-in on {server}…")))
            }
            Step::Cancelling => panel = panel.child(waiting("Cancelling…".into())),
            Step::Done(Ok(())) => {
                panel = panel.child(
                    div()
                        .text_xs()
                        .text_color(ui::success())
                        .child(format!("Signed in to {}.", flow.name)),
                )
            }
            Step::Done(Err(error)) => {
                panel = panel.child(
                    div()
                        .text_xs()
                        .text_color(ui::danger())
                        .child(error.clone()),
                )
            }
            Step::Running => {
                if let Some(prompt) = &flow.prompt {
                    panel = panel.child(self.login_prompt(prompt, cx));
                }
            }
        }
        if let Some(page) = &state.page {
            panel = panel.child(
                div()
                    .h(rems(30.))
                    .w_full()
                    .rounded_lg()
                    .overflow_hidden()
                    .border_1()
                    .border_color(ui::border())
                    .child(page.surface.clone()),
            );
        }
        panel.when(!flow.lines.is_empty(), |d| {
            d.child(
                ui::well_box().px_3().py_2().gap_0p5().children(
                    flow.lines
                        .iter()
                        .rev()
                        .take(6)
                        .rev()
                        .map(|line| div().text_color(ui::muted()).child(line.clone())),
                ),
            )
        })
    }

    fn login_prompt(&self, prompt: &LoginPrompt, cx: &mut Context<Self>) -> Div {
        let state = &self.server_sign_in;
        let message = prompt.message.clone().filter(|message| !message.is_empty());
        let column = v_flex().gap_2().children(
            message.map(|message| div().text_xs().text_color(ui::muted()).child(message)),
        );
        match prompt.kind {
            LoginPromptKind::OpenUrl => {
                let url = prompt.url.clone().unwrap_or_default();
                let field = &state.answer;
                column
                    .child(div().text_xs().text_color(ui::muted()).child(
                        if prompt.redirect_prefix.is_some() {
                            "Sign in below; the app hands the provider's answer to the server. \
                             In your own browser instead, paste the address it ends on here."
                        } else {
                            "Sign in below, then paste the code the page shows here."
                        },
                    ))
                    .child(self.answer_row(field, "Code or address", cx))
                    .child(
                        h_flex().child(
                            Button::new("server-login-open")
                                .xsmall()
                                .ghost()
                                .icon(Icon::new(IconName::ExternalLink))
                                .label("Open in my browser")
                                .on_click(move |_, _, cx| cx.open_url(&url)),
                        ),
                    )
            }
            LoginPromptKind::DeviceCode => {
                let code = prompt.user_code.clone().unwrap_or_default();
                let copied = code.clone();
                column
                    .child(div().text_xs().text_color(ui::muted()).child(
                        "Enter this code on the provider's page below (or in your own browser):",
                    ))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                ui::well_box().px_3().py_2().child(
                                    div()
                                        .text_size(ui::code_rems(20.))
                                        .text_color(ui::champagne())
                                        .child(code),
                                ),
                            )
                            .child(
                                Button::new("server-login-copy")
                                    .small()
                                    .ghost()
                                    .icon(Icon::new(IconName::Copy))
                                    .label("Copy")
                                    .on_click(move |_, _, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            copied.clone(),
                                        ))
                                    }),
                            ),
                    )
                    .children(prompt.url.clone().map(|url| {
                        h_flex().child(
                            Button::new("server-login-open-device")
                                .xsmall()
                                .ghost()
                                .icon(Icon::new(IconName::ExternalLink))
                                .label("Open in my browser")
                                .on_click(move |_, _, cx| cx.open_url(&url)),
                        )
                    }))
            }
            LoginPromptKind::Input if !prompt.options.is_empty() => {
                column.child(h_flex().gap_2().flex_wrap().children(
                    prompt.options.iter().enumerate().map(|(index, option)| {
                        let id = option.id.clone();
                        Button::new(("server-login-option", index))
                            .small()
                            .label(option.label.clone())
                            .on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.answer_login(id.clone(), cx)
                                }),
                            )
                    }),
                ))
            }
            LoginPromptKind::Input => {
                let field = if prompt.secret {
                    &state.secret_answer
                } else {
                    &state.answer
                };
                let placeholder = prompt.placeholder.clone().unwrap_or_default();
                column.child(self.answer_row(field, placeholder, cx))
            }
            LoginPromptKind::Progress => column,
        }
    }

    /// A field and its Continue button, answering the prompt.
    fn answer_row(
        &self,
        field: &Entity<InputState>,
        placeholder: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) -> Div {
        let hint: SharedString = placeholder.into();
        let input = field.clone();
        h_flex()
            .gap_2()
            .when(!hint.is_empty(), |d| {
                d.child(
                    div()
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(ui::faint())
                        .child(hint),
                )
            })
            .child(
                ui::field()
                    .flex_1()
                    .min_w_0()
                    .child(Input::new(field).appearance(false).small()),
            )
            .child(
                Button::new("server-login-answer")
                    .small()
                    .primary()
                    .label("Continue")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.submit_login_answer(input.clone(), window, cx)
                    })),
            )
    }
}

#[cfg(test)]
mod tests {
    use pi_rpc::desktop_rpc::{LoginFinished, LoginPrompt, LoginPromptKind};

    use super::{LoginFlow, Step};
    use crate::features::session::AccountEvent;

    fn prompt(id: &str, kind: LoginPromptKind) -> LoginPrompt {
        LoginPrompt {
            login_id: id.into(),
            kind,
            url: Some("https://auth.example/authorize".into()),
            user_code: Some("ABCD-1234".into()),
            message: Some(format!("{kind:?}")),
            placeholder: None,
            redirect_prefix: Some("http://localhost:1455/".into()),
            options: Vec::new(),
            secret: false,
        }
    }

    fn finished(id: &str, ok: bool, error: Option<&str>) -> AccountEvent {
        AccountEvent::LoginFinished(LoginFinished {
            login_id: id.into(),
            ok,
            error: error.map(str::to_string),
        })
    }

    fn started() -> LoginFlow {
        let mut flow = LoginFlow::new("openai-codex".into(), "OpenAI Codex".into());
        assert_eq!(flow.started("l1".into()), None);
        flow
    }

    #[test]
    fn a_browser_login_catches_its_redirect_and_finishes() {
        let mut flow = started();
        assert_eq!(flow.step, Step::Running);
        assert!(flow.apply(&AccountEvent::LoginPrompt(prompt(
            "l1",
            LoginPromptKind::OpenUrl
        ))));
        assert_eq!(
            flow.page(),
            Some((
                "https://auth.example/authorize".into(),
                Some("http://localhost:1455/".into())
            ))
        );
        // The provider's own pages load; the callback is caught, once.
        assert_eq!(flow.caught("https://auth.example/consent"), None);
        assert_eq!(
            flow.caught("http://localhost:1455/auth/callback?code=c&state=s"),
            Some("l1".into())
        );
        assert_eq!(flow.step, Step::Answering);
        assert_eq!(flow.page(), None, "the card goes once the answer is sent");
        assert_eq!(
            flow.caught("http://localhost:1455/auth/callback?code=c"),
            None
        );
        assert!(flow.apply(&finished("l1", true, None)));
        assert_eq!(flow.step, Step::Done(Ok(())));
        assert!(flow.prompt.is_none());
    }

    #[test]
    fn events_before_the_login_id_apply_once_it_arrives() {
        let mut flow = LoginFlow::new("github-copilot".into(), "GitHub Copilot".into());
        let device = AccountEvent::LoginPrompt(prompt("l9", LoginPromptKind::DeviceCode));
        let other = AccountEvent::LoginPrompt(prompt("l8", LoginPromptKind::Progress));
        assert!(!flow.apply(&device));
        assert!(!flow.apply(&other));
        assert_eq!(flow.started("l9".into()), None);
        assert_eq!(
            flow.prompt.as_ref().map(|p| p.kind),
            Some(LoginPromptKind::DeviceCode)
        );
        assert!(
            flow.lines.is_empty(),
            "another login's progress is not ours"
        );
        // A device page never catches a redirect.
        assert_eq!(flow.page().and_then(|(_, prefix)| prefix), None);
        assert_eq!(flow.caught("http://localhost:1455/"), None);
    }

    #[test]
    fn progress_is_kept_and_a_question_is_answered() {
        let mut flow = started();
        flow.apply(&AccountEvent::LoginPrompt(prompt(
            "l1",
            LoginPromptKind::Progress,
        )));
        assert_eq!(flow.lines, ["Progress"]);
        assert_eq!(flow.answer(), None, "nothing asked yet");
        flow.apply(&AccountEvent::LoginPrompt(prompt(
            "l1",
            LoginPromptKind::Input,
        )));
        assert_eq!(flow.answer(), Some("l1".into()));
        assert_eq!(flow.answer(), None, "one answer at a time");
        flow.answer_refused("Invalid code.".into());
        assert_eq!(flow.step, Step::Running);
        assert_eq!(flow.lines.last().map(String::as_str), Some("Invalid code."));
        // The next question after an answer shows.
        flow.answer();
        flow.apply(&AccountEvent::LoginPrompt(prompt(
            "l1",
            LoginPromptKind::OpenUrl,
        )));
        assert_eq!(flow.step, Step::Running);
    }

    #[test]
    fn cancel_while_starting_waits_for_the_id_and_a_failure_ends_it() {
        let mut flow = LoginFlow::new("anthropic".into(), "Anthropic".into());
        assert_eq!(flow.cancel(), None);
        assert_eq!(flow.step, Step::Cancelling);
        assert_eq!(flow.started("l2".into()), Some("l2".into()));
        assert!(flow.apply(&finished("l2", false, Some("Cancelled"))));
        assert_eq!(flow.step, Step::Done(Err("Cancelled".into())));
        // Events after the end change nothing.
        assert!(!flow.apply(&AccountEvent::LoginPrompt(prompt(
            "l2",
            LoginPromptKind::OpenUrl
        ))));

        let mut running = started();
        assert_eq!(running.cancel(), Some("l1".into()));
        assert_eq!(running.cancel(), None);
        running.apply(&finished("l1", false, None));
        assert_eq!(
            running.step,
            Step::Done(Err("The sign-in didn't finish.".into()))
        );

        let mut refused = LoginFlow::new("x".into(), "X".into());
        refused.failed("This Gentle Shell does not offer that over Desktop.".into());
        assert!(refused.finished());
        assert_eq!(refused.started("l3".into()), None);
    }
}
