//! Which providers have credentials, as the active agent tab's Gentle Shell
//! knows them (`get_auth_status`, Gentle Shell 5.0.0): status only, never a
//! value, from Pi's model registry in memory. It fills Models › Sign-ins and
//! the Setup & requirements provider check. A Shell without the method keeps
//! the reads of `auth.json`, `models.json` and the environment.

use gpui_kit::*;
use pi_rpc::desktop_rpc::{AuthStatus, AuthType, DesktopMethod};

use super::SettingsView;
use crate::features::session::{ReadOutcome, ReadRequest, ReadSupport, ReadTicket};

/// Gentle Shell's answer and the read in flight.
#[derive(Default)]
pub(super) struct SignIns {
    pub(super) status: Option<AuthStatus>,
    ticket: Option<ReadTicket>,
    /// Asked while the tab's probe had not answered.
    awaiting: bool,
}

/// Models › Sign-ins as `(provider, kind)`, like the `auth.json` read:
/// only the providers with credentials.
pub(super) fn sign_ins_of(status: &AuthStatus) -> Vec<(String, String)> {
    status
        .configured()
        .map(|entry| {
            let kind = match entry.kind {
                Some(AuthType::OAuth) => "oauth",
                Some(AuthType::ApiKey) => "api_key",
                None => "?",
            };
            (entry.provider.clone(), kind.to_string())
        })
        .collect()
}

impl SettingsView {
    /// Ask the active tab's Gentle Shell which providers have credentials,
    /// when it advertises `get_auth_status`.
    pub(super) fn ask_sign_ins(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            return;
        };
        match session.read(cx).read_support(DesktopMethod::GetAuthStatus) {
            ReadSupport::Rpc => {}
            ReadSupport::NotReady => {
                self.sign_ins.awaiting = true;
                return;
            }
            ReadSupport::Legacy => {
                self.sign_ins.awaiting = false;
                return;
            }
        }
        self.sign_ins.awaiting = false;
        if self.sign_ins.ticket.is_some() {
            return;
        }
        if let Ok(ticket) =
            session.update(cx, |session, cx| session.read(ReadRequest::AuthStatus, cx))
        {
            self.sign_ins.ticket = Some(ticket);
        }
    }

    /// The tab changed: a probe that answered, or the `get_auth_status`
    /// answer. A failed read keeps the files' read.
    pub(super) fn sign_ins_changed(&mut self, cx: &mut Context<Self>) {
        if self.sign_ins.awaiting {
            let decided = self.session.as_ref().is_some_and(|session| {
                session.read(cx).read_support(DesktopMethod::GetAuthStatus) != ReadSupport::NotReady
            });
            if decided {
                self.ask_sign_ins(cx);
            }
            return;
        }
        let (Some(ticket), Some(session)) = (self.sign_ins.ticket, self.session.clone()) else {
            return;
        };
        let Some(outcome) = session.update(cx, |session, _| session.take_read(ticket)) else {
            return;
        };
        self.sign_ins.ticket = None;
        if let ReadOutcome::AuthStatus(status) = outcome {
            self.sign_ins.status = Some(status);
            self.adopt_shell(cx);
        }
    }

    /// Hand the registry's answer to Setup & requirements' provider check.
    pub(super) fn adopt_sign_ins(&self, cx: &mut Context<Self>) {
        let Some(status) = &self.sign_ins.status else {
            return;
        };
        let configured: Vec<&str> = status
            .configured()
            .map(|entry| entry.provider.as_str())
            .collect();
        self.setup
            .update(cx, |setup, cx| setup.adopt_signed_in(&configured, cx));
    }
}

#[cfg(test)]
mod tests {
    use pi_rpc::desktop_rpc::{AuthSource, AuthStatus, AuthType, ProviderAuth};

    use super::sign_ins_of;

    fn provider(id: &str, kind: Option<AuthType>) -> ProviderAuth {
        ProviderAuth {
            provider: id.into(),
            display_name: id.into(),
            configured: kind.is_some(),
            kind,
            source: kind.map(|_| AuthSource::Stored),
            login_kinds: Vec::new(),
        }
    }

    /// The doc's answer: only the provider with credentials is a sign-in,
    /// with its kind, as the `auth.json` read showed it.
    #[test]
    fn only_configured_providers_are_sign_ins() {
        let status = AuthStatus {
            providers: vec![
                provider("anthropic", Some(AuthType::OAuth)),
                provider("groq", Some(AuthType::ApiKey)),
                provider("openai-codex", None),
            ],
            truncated: false,
        };
        assert_eq!(
            sign_ins_of(&status),
            vec![
                ("anthropic".to_string(), "oauth".to_string()),
                ("groq".to_string(), "api_key".to_string()),
            ]
        );
    }
}
