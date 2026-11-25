//! `get_auth_status` and `reload_credentials`: which providers this session
//! has credentials for, read from Pi's model registry in memory, and an
//! offline refresh of that registry so credentials added or removed
//! elsewhere (another terminal's `/login`, the app's own sign-in) apply
//! without restarting. Status only: no credential value ever crosses. A
//! Shell that signs in itself (`login_start`) also says how each provider
//! signs in (`loginKinds`). `list_available_models` reads the same registry
//! for the models Pi can use now and the current model's thinking levels,
//! as Pi RPC's `get_available_models` and `get_available_thinking_levels`
//! do, for a client without a session's Pi RPC (a server's `/api/desktop`).

use super::DesktopRpcError;
use serde::{Deserialize, Deserializer};
use serde_json::Value;

wire_enum! {
    /// The kind of credentials a configured provider has.
    AuthType, "type" {
        OAuth => "oauth",
        ApiKey => "api_key",
    }
}

wire_enum! {
    /// Where Pi found a configured provider's credentials.
    AuthSource, "source" {
        Stored => "stored",
        Runtime => "runtime",
        Environment => "environment",
        ModelsJson => "models_json",
        Extension => "extension",
    }
}

wire_enum! {
    /// How a provider can sign in on the Shell's side: a browser login
    /// (`oauth`) or a device code (`device`), both through `login_start`, or
    /// a key (`api_key`, `set_api_key`).
    LoginKind, "loginKinds" {
        OAuth => "oauth",
        Device => "device",
        ApiKey => "api_key",
    }
}

/// `loginKinds` as listed, without the kinds this client doesn't know (a
/// newer Shell's): those are not offered, the rest still are.
fn known_login_kinds<'de, D>(deserializer: D) -> Result<Vec<LoginKind>, D::Error>
where
    D: Deserializer<'de>,
{
    let kinds: Option<Vec<String>> = Option::deserialize(deserializer)?;
    Ok(kinds
        .unwrap_or_default()
        .iter()
        .filter_map(|kind| kind.parse().ok())
        .collect())
}

/// One provider Pi knows models for. `kind` and `source` are `None` exactly
/// when it is not configured. `login_kinds` is empty from a Shell that
/// doesn't sign in itself.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderAuth {
    pub provider: String,
    pub display_name: String,
    pub configured: bool,
    #[serde(rename = "type")]
    pub kind: Option<AuthType>,
    pub source: Option<AuthSource>,
    #[serde(default, deserialize_with = "known_login_kinds")]
    pub login_kinds: Vec<LoginKind>,
}

/// The decoded `get_auth_status` data: providers sorted by id, at most 200.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AuthStatus {
    pub providers: Vec<ProviderAuth>,
    pub truncated: bool,
}

impl AuthStatus {
    pub(super) fn from_data(data: &Value) -> Result<Self, DesktopRpcError> {
        let status: Self = super::decode_data(data)?;
        // A provider without credentials has no kind and no source. A
        // configured one may have a source Gentle Shell does not name.
        if let Some(entry) = status.providers.iter().find(|entry| {
            entry.configured != entry.kind.is_some()
                || (!entry.configured && entry.source.is_some())
        }) {
            return Err(DesktopRpcError::MalformedPayload(format!(
                "provider {} has a type and source only when configured",
                entry.provider
            )));
        }
        Ok(status)
    }

    /// The providers with credentials Pi can use.
    pub fn configured(&self) -> impl Iterator<Item = &ProviderAuth> {
        self.providers.iter().filter(|entry| entry.configured)
    }
}

/// The decoded `reload_credentials` data: whether the registry was rebuilt,
/// the providers whose refresh reported an error (the error never crosses)
/// and the status afterwards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReloadCredentials {
    pub reloaded: bool,
    pub aborted: bool,
    pub failed_providers: Vec<String>,
    pub status: AuthStatus,
}

impl ReloadCredentials {
    pub(super) fn from_data(data: &Value) -> Result<Self, DesktopRpcError> {
        Ok(Self {
            reloaded: super::bool_field(data, "reloaded")?,
            aborted: super::bool_field(data, "aborted")?,
            failed_providers: super::name_list(data, "failedProviders")?,
            status: AuthStatus::from_data(data)?,
        })
    }
}

/// One model Pi can use now. Only `provider` and `id` always come; the rest
/// only when Pi gives them that shape.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AvailableModel {
    pub provider: String,
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub reasoning: Option<bool>,
    /// What it takes: `text`, `image`.
    #[serde(default)]
    pub input: Option<Vec<String>>,
}

/// The session's model and thinking level.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CurrentModel {
    pub provider: String,
    pub id: String,
    pub thinking_level: String,
}

/// The decoded `list_available_models` data: the models in Pi's order (at
/// most 1000), what the current model accepts, low to high, and that model
/// when one is chosen.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AvailableModels {
    pub models: Vec<AvailableModel>,
    pub thinking_levels: Vec<String>,
    #[serde(default)]
    pub current: Option<CurrentModel>,
    pub truncated: bool,
}

impl AvailableModels {
    pub(super) fn from_data(data: &Value) -> Result<Self, DesktopRpcError> {
        super::decode_data(data)
    }
}
