//! Signing in to a provider on the Shell's side (a Gentle Shell server's, or
//! any Shell that offers it): `login_start` runs Pi's own login for the
//! provider there, its steps arrive as `login_prompt` events (a page to open,
//! a device code, a question, a progress line) and its end as
//! `login_finished`. The client answers a prompt with `login_complete` (a
//! pasted code, an answer, or the full redirect URL it intercepted on its
//! way to the provider's loopback callback) and may `login_cancel`.
//! `set_api_key` and `remove_credentials` write and remove a stored key.
//!
//! A key or a pasted value is never echoed back by the server, and never
//! printed by this client: [`super::DesktopRequest`]'s `Debug` leaves the
//! payload of these requests out.

use super::{DesktopRpcError, decode_data};
use serde::Deserialize;
use serde_json::{Value, json};

/// The event a login's next step arrives as.
pub const LOGIN_PROMPT: &str = "login_prompt";
/// The event a login's end arrives as.
pub const LOGIN_FINISHED: &str = "login_finished";

wire_enum! {
    /// What a `login_prompt` asks of the client: open `url` (when
    /// `redirectPrefix` is set the login waits for the provider to redirect
    /// there), show `userCode` to enter at `url`, ask for a value (or one of
    /// `options`) and answer it, or show a line of progress.
    LoginPromptKind, "kind" {
        OpenUrl => "open_url",
        DeviceCode => "device_code",
        Input => "input",
        Progress => "progress",
    }
}

/// One choice of an `input` prompt: answering it sends its `id`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LoginOption {
    pub id: String,
    pub label: String,
}

/// The decoded `login_prompt` event.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginPrompt {
    pub login_id: String,
    pub kind: LoginPromptKind,
    pub url: Option<String>,
    pub user_code: Option<String>,
    pub message: Option<String>,
    pub placeholder: Option<String>,
    /// The loopback callback the provider redirects to (for example
    /// `http://localhost:1455/`): any navigation starting with it is the
    /// client's to intercept and send back with `login_complete`.
    pub redirect_prefix: Option<String>,
    /// An `input` that is a choice.
    #[serde(default)]
    pub options: Vec<LoginOption>,
    /// An `input` whose answer is masked.
    #[serde(default)]
    pub secret: bool,
}

impl LoginPrompt {
    pub(super) fn from_data(data: &Value) -> Result<Self, DesktopRpcError> {
        let prompt: Self = decode_data(data)?;
        if prompt.login_id.is_empty() {
            return Err(malformed("login_prompt has an empty loginId"));
        }
        match prompt.kind {
            // The page is loaded in the app's browser: only the web.
            LoginPromptKind::OpenUrl if !prompt.url.as_deref().is_some_and(is_web_url) => {
                Err(malformed("an open_url prompt needs an http(s) url"))
            }
            LoginPromptKind::DeviceCode
                if prompt.user_code.as_deref().is_none_or(str::is_empty)
                    || prompt.url.as_deref().is_some_and(|url| !is_web_url(url)) =>
            {
                Err(malformed(
                    "a device_code prompt needs a userCode and an http(s) url",
                ))
            }
            _ if prompt
                .redirect_prefix
                .as_deref()
                .is_some_and(|prefix| !is_web_url(prefix)) =>
            {
                Err(malformed("redirectPrefix must be an http(s) URL"))
            }
            _ => Ok(prompt),
        }
    }
}

/// The decoded `login_finished` event. `error` is the Shell's words.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginFinished {
    pub login_id: String,
    pub ok: bool,
    pub error: Option<String>,
}

impl LoginFinished {
    pub(super) fn from_data(data: &Value) -> Result<Self, DesktopRpcError> {
        let finished: Self = decode_data(data)?;
        if finished.login_id.is_empty() {
            return Err(malformed("login_finished has an empty loginId"));
        }
        Ok(finished)
    }
}

/// The decoded `login_start` data: the login the events name.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginStarted {
    pub login_id: String,
}

impl LoginStarted {
    pub(super) fn from_data(data: &Value) -> Result<Self, DesktopRpcError> {
        let started: Self = decode_data(data)?;
        if started.login_id.is_empty() {
            return Err(malformed("login_start answered an empty loginId"));
        }
        Ok(started)
    }
}

/// The decoded `set_api_key` data.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ApiKeySaved {
    pub saved: bool,
}

/// The decoded `remove_credentials` data: `false` when nothing was stored.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CredentialsRemoved {
    pub removed: bool,
}

/// An http(s) URL with something after the scheme.
fn is_web_url(url: &str) -> bool {
    let lower = url.get(..8).unwrap_or(url).to_ascii_lowercase();
    (lower.starts_with("https://") && url.len() > 8)
        || (lower.starts_with("http://") && url.len() > 7)
}

fn malformed(text: &str) -> DesktopRpcError {
    DesktopRpcError::MalformedPayload(text.to_string())
}

fn non_empty(field: &'static str, value: &str) -> Result<(), DesktopRpcError> {
    if value.is_empty() {
        return Err(DesktopRpcError::InvalidValue {
            field,
            value: String::new(),
        });
    }
    Ok(())
}

/// The wire `params` of `login_start` and `remove_credentials`: `{ provider }`.
pub(super) fn provider_params(provider: &str) -> Result<Value, DesktopRpcError> {
    if provider.is_empty() {
        return Err(DesktopRpcError::InvalidProvider);
    }
    Ok(json!({ "provider": provider }))
}

/// The wire `params` of `login_complete`: `{ loginId, value }`.
pub(super) fn login_complete_params(login_id: &str, value: &str) -> Result<Value, DesktopRpcError> {
    non_empty("loginId", login_id)?;
    // The value is never quoted back, even in an error.
    non_empty("value", value)?;
    Ok(json!({ "loginId": login_id, "value": value }))
}

/// The wire `params` of `login_cancel`: `{ loginId }`.
pub(super) fn login_cancel_params(login_id: &str) -> Result<Value, DesktopRpcError> {
    non_empty("loginId", login_id)?;
    Ok(json!({ "loginId": login_id }))
}

/// The wire `params` of `set_api_key`: `{ provider, key }`.
pub(super) fn set_api_key_params(provider: &str, key: &str) -> Result<Value, DesktopRpcError> {
    if provider.is_empty() {
        return Err(DesktopRpcError::InvalidProvider);
    }
    non_empty("key", key)?;
    Ok(json!({ "provider": provider, "key": key }))
}
