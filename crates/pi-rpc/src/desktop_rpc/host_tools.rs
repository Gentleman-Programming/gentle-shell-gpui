//! Host tools: tools a client provides that run on the client while the
//! agent runs wherever Gentle Shell runs. `set_host_tools` registers a
//! provider's whole set (an empty list unregisters it); the agent calling
//! one arrives as a `host_tool_call` event, a stopped turn or call as
//! `host_tool_cancel`, and the client answers with `host_tool_result`
//! (optionally with the Pi tool result's `details`). While it still works
//! on a call, `host_tool_progress` restarts the call's no-result deadline
//! (10 minutes on Gentle Shell). The same client talks to the agent without
//! a call too: `host_message` is a custom message in the conversation,
//! `set_host_context` a hidden one added before every agent run while it
//! is set.
//!
//! The limits are the contract's, checked before anything is sent: a tool
//! the server would refuse never leaves the app. Through a Gentle Shell
//! server these requests go to `POST /api/sessions/:id/desktop` (the server
//! sets `provider` to the calling device), never as a `/gentle:desktop`
//! prompt.

use super::{DesktopRpcError, decode_data};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// The event the agent calling a host tool arrives as.
pub const HOST_TOOL_CALL: &str = "host_tool_call";
/// The event a stopped turn, cancelled or expired call arrives as.
pub const HOST_TOOL_CANCEL: &str = "host_tool_cancel";
/// How often a client still working on a call sends `host_tool_progress`:
/// well inside the server's 10-minute no-result deadline.
pub const HOST_TOOL_PROGRESS_EVERY: std::time::Duration = std::time::Duration::from_secs(60);

/// Longest provider name, in characters.
pub const MAX_PROVIDER_CHARS: usize = 64;
/// Most tools one provider registers.
pub const MAX_HOST_TOOLS: usize = 32;
/// Longest tool label, in characters.
pub const MAX_LABEL_CHARS: usize = 64;
/// Longest tool description, in characters.
pub const MAX_DESCRIPTION_CHARS: usize = 4096;
/// Largest parameters schema, in bytes of its JSON encoding.
pub const MAX_PARAMETERS_BYTES: usize = 16 * 1024;
/// Most guidelines one tool carries.
pub const MAX_GUIDELINES: usize = 16;
/// Longest guideline, in characters.
pub const MAX_GUIDELINE_CHARS: usize = 512;
/// Largest result content: text bytes plus base64 image data lengths.
pub const MAX_RESULT_CONTENT_BYTES: usize = 8 * 1024 * 1024;
/// Largest `set_host_tools` inner request, in bytes.
pub const MAX_SET_HOST_TOOLS_BYTES: usize = 1024 * 1024;
/// Largest `host_tool_result` inner request, in bytes: the content cap,
/// its `details`, and room for the encoding.
pub const MAX_HOST_TOOL_RESULT_BYTES: usize = 10 * 1024 * 1024;
/// Largest `details` object (a result's, message's or context's), in bytes
/// of its JSON encoding.
pub const MAX_DETAILS_BYTES: usize = 1024 * 1024;
/// Largest `host_message` or `set_host_context` content, in UTF-8 bytes.
pub const MAX_MESSAGE_CONTENT_BYTES: usize = 64 * 1024;
/// Largest `host_message` or `set_host_context` inner request, in bytes.
pub const MAX_HOST_MESSAGE_BYTES: usize = 2 * 1024 * 1024;
/// Longest `host_message` custom type, in characters (`host.` included).
const MAX_CUSTOM_TYPE_CHARS: usize = 69;

/// One tool a client provides.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HostTool {
    /// `^[a-z][a-z0-9_]{0,63}$`.
    pub name: String,
    pub label: String,
    pub description: String,
    /// A JSON Schema object.
    pub parameters: Value,
    /// Guidance the model sees while the tool is registered.
    pub guidelines: Vec<String>,
}

/// Whether `name` is a valid host tool name.
pub fn valid_tool_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && name.len() <= 64
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

impl HostTool {
    /// Refuse what the server would: a bad name, a field over its limit, or
    /// parameters that are not a JSON object.
    pub fn validate(&self) -> Result<(), DesktopRpcError> {
        let invalid = |field: &'static str, why: String| DesktopRpcError::InvalidValue {
            field,
            value: format!("{}: {why}", self.name),
        };
        if !valid_tool_name(&self.name) {
            return Err(invalid("name", "must match ^[a-z][a-z0-9_]{0,63}$".into()));
        }
        if self.label.is_empty() || self.label.chars().count() > MAX_LABEL_CHARS {
            return Err(invalid(
                "label",
                format!("1 to {MAX_LABEL_CHARS} characters"),
            ));
        }
        if self.description.is_empty() || self.description.chars().count() > MAX_DESCRIPTION_CHARS {
            return Err(invalid(
                "description",
                format!("1 to {MAX_DESCRIPTION_CHARS} characters"),
            ));
        }
        if !self.parameters.is_object() {
            return Err(invalid("parameters", "must be a JSON Schema object".into()));
        }
        let bytes = serde_json::to_string(&self.parameters)
            .map_err(|error| DesktopRpcError::MalformedPayload(error.to_string()))?
            .len();
        if bytes > MAX_PARAMETERS_BYTES {
            return Err(invalid(
                "parameters",
                format!("{bytes} bytes, over {MAX_PARAMETERS_BYTES}"),
            ));
        }
        if self.guidelines.len() > MAX_GUIDELINES {
            return Err(invalid("guidelines", format!("more than {MAX_GUIDELINES}")));
        }
        if let Some(long) = self
            .guidelines
            .iter()
            .find(|line| line.is_empty() || line.chars().count() > MAX_GUIDELINE_CHARS)
        {
            return Err(invalid(
                "guidelines",
                format!(
                    "a guideline of {} characters (1 to {MAX_GUIDELINE_CHARS})",
                    long.chars().count()
                ),
            ));
        }
        Ok(())
    }
}

/// The wire `params` of `set_host_tools`: `{ provider, tools }`, every tool
/// valid and named once.
pub(super) fn set_host_tools_params(
    provider: &str,
    tools: &[HostTool],
) -> Result<Value, DesktopRpcError> {
    let chars = provider.chars().count();
    if chars == 0 || chars > MAX_PROVIDER_CHARS {
        return Err(DesktopRpcError::InvalidValue {
            field: "provider",
            value: provider.to_string(),
        });
    }
    if tools.len() > MAX_HOST_TOOLS {
        return Err(DesktopRpcError::InvalidValue {
            field: "tools",
            value: format!("{} tools, at most {MAX_HOST_TOOLS}", tools.len()),
        });
    }
    for (index, tool) in tools.iter().enumerate() {
        tool.validate()?;
        if tools[..index].iter().any(|other| other.name == tool.name) {
            return Err(DesktopRpcError::InvalidValue {
                field: "tools",
                value: format!("{} is listed twice", tool.name),
            });
        }
    }
    Ok(json!({ "provider": provider, "tools": tools }))
}

/// One part of a host tool's result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum HostToolContent {
    Text {
        text: String,
    },
    Image {
        /// Base64.
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
}

impl HostToolContent {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }

    /// What the content cap counts: text bytes, base64 data length.
    fn counted_bytes(&self) -> usize {
        match self {
            Self::Text { text } => text.len(),
            Self::Image { data, .. } => data.len(),
        }
    }
}

/// The wire `params` of `host_tool_result`: `{ callId, content }`, and
/// `isError` only when true, `details` only when given; content within
/// [`MAX_RESULT_CONTENT_BYTES`], details within [`MAX_DETAILS_BYTES`].
pub(super) fn host_tool_result_params(
    call_id: &str,
    content: &[HostToolContent],
    is_error: bool,
    details: Option<&Map<String, Value>>,
) -> Result<Value, DesktopRpcError> {
    if call_id.is_empty() {
        return Err(DesktopRpcError::InvalidValue {
            field: "callId",
            value: String::new(),
        });
    }
    let bytes: usize = content.iter().map(HostToolContent::counted_bytes).sum();
    if bytes > MAX_RESULT_CONTENT_BYTES {
        return Err(DesktopRpcError::RequestTooLarge {
            bytes,
            limit: MAX_RESULT_CONTENT_BYTES,
        });
    }
    let mut params = json!({ "callId": call_id, "content": content });
    if is_error {
        params["isError"] = Value::Bool(true);
    }
    if let Some(details) = details {
        params["details"] = checked_details(details)?;
    }
    Ok(params)
}

/// The wire `params` of `host_tool_progress`: `{ callId }`.
pub(super) fn host_tool_progress_params(call_id: &str) -> Result<Value, DesktopRpcError> {
    if call_id.is_empty() {
        return Err(DesktopRpcError::InvalidValue {
            field: "callId",
            value: String::new(),
        });
    }
    Ok(json!({ "callId": call_id }))
}

/// `details` as sent, refused over [`MAX_DETAILS_BYTES`].
fn checked_details(details: &Map<String, Value>) -> Result<Value, DesktopRpcError> {
    let value = Value::Object(details.clone());
    let bytes = serde_json::to_string(&value)
        .map_err(|error| DesktopRpcError::MalformedPayload(error.to_string()))?
        .len();
    if bytes > MAX_DETAILS_BYTES {
        return Err(DesktopRpcError::RequestTooLarge {
            bytes,
            limit: MAX_DETAILS_BYTES,
        });
    }
    Ok(value)
}

/// `content` refused over [`MAX_MESSAGE_CONTENT_BYTES`].
fn checked_content(content: &str) -> Result<(), DesktopRpcError> {
    if content.len() > MAX_MESSAGE_CONTENT_BYTES {
        return Err(DesktopRpcError::RequestTooLarge {
            bytes: content.len(),
            limit: MAX_MESSAGE_CONTENT_BYTES,
        });
    }
    Ok(())
}

/// Whether `custom_type` is a host message's:
/// `^host\.[a-z0-9][a-z0-9_.-]{0,63}$`.
pub fn valid_custom_type(custom_type: &str) -> bool {
    let Some(name) = custom_type.strip_prefix("host.") else {
        return false;
    };
    let mut chars = name.chars();
    custom_type.len() <= MAX_CUSTOM_TYPE_CHARS
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '.' | '-'))
}

/// How a host message reaches the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostDelivery {
    /// Start a run when the agent is idle (unless `trigger_turn` is
    /// false), steer the running one otherwise.
    Auto,
    Steer,
    FollowUp,
    NextTurn,
}

impl HostDelivery {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Steer => "steer",
            Self::FollowUp => "followUp",
            Self::NextTurn => "nextTurn",
        }
    }
}

/// A custom message for the conversation (`host_message`).
#[derive(Debug, Clone, PartialEq)]
pub struct HostMessage {
    /// `host.<name>`.
    pub custom_type: String,
    pub content: String,
    /// Shown in the conversation (or only read by the agent).
    pub display: bool,
    pub details: Option<Map<String, Value>>,
    pub deliver: HostDelivery,
    /// Sent only when given.
    pub trigger_turn: Option<bool>,
}

/// The wire `params` of `host_message`.
pub(super) fn host_message_params(message: &HostMessage) -> Result<Value, DesktopRpcError> {
    if !valid_custom_type(&message.custom_type) {
        return Err(DesktopRpcError::InvalidValue {
            field: "customType",
            value: message.custom_type.clone(),
        });
    }
    checked_content(&message.content)?;
    let mut params = json!({
        "customType": message.custom_type,
        "content": message.content,
        "display": message.display,
        "deliver": message.deliver.as_str(),
    });
    if let Some(details) = &message.details {
        params["details"] = checked_details(details)?;
    }
    if let Some(trigger) = message.trigger_turn {
        params["triggerTurn"] = Value::Bool(trigger);
    }
    Ok(params)
}

/// The wire `params` of `set_host_context`.
pub(super) fn set_host_context_params(
    provider: &str,
    content: &str,
    details: Option<&Map<String, Value>>,
) -> Result<Value, DesktopRpcError> {
    let chars = provider.chars().count();
    if chars == 0 || chars > MAX_PROVIDER_CHARS {
        return Err(DesktopRpcError::InvalidValue {
            field: "provider",
            value: provider.to_string(),
        });
    }
    checked_content(content)?;
    let mut params = json!({ "provider": provider, "content": content });
    if let Some(details) = details {
        params["details"] = checked_details(details)?;
    }
    Ok(params)
}

/// A tool the server would not register, and why.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RejectedHostTool {
    pub name: String,
    pub reason: String,
}

/// The decoded `set_host_tools` data.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SetHostToolsResult {
    pub registered: Vec<String>,
    pub rejected: Vec<RejectedHostTool>,
}

impl SetHostToolsResult {
    pub(super) fn from_data(data: &Value) -> Result<Self, DesktopRpcError> {
        decode_data(data)
    }
}

/// The decoded `host_tool_result` data: `{ accepted: true }`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct HostToolResultAck {
    pub accepted: bool,
}

impl HostToolResultAck {
    pub(super) fn from_data(data: &Value) -> Result<Self, DesktopRpcError> {
        decode_data(data)
    }
}

/// The decoded `host_tool_progress` data: `{ extended: true, deadlineMs }`,
/// the call's new no-result deadline in epoch milliseconds.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostToolProgressAck {
    pub extended: bool,
    pub deadline_ms: u64,
}

impl HostToolProgressAck {
    pub(super) fn from_data(data: &Value) -> Result<Self, DesktopRpcError> {
        decode_data(data)
    }
}

/// The decoded `host_message` data: `{ delivered: true }`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct HostMessageAck {
    pub delivered: bool,
}

impl HostMessageAck {
    pub(super) fn from_data(data: &Value) -> Result<Self, DesktopRpcError> {
        decode_data(data)
    }
}

/// The decoded `set_host_context` data: `{ set: true }`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SetHostContextAck {
    pub set: bool,
}

impl SetHostContextAck {
    pub(super) fn from_data(data: &Value) -> Result<Self, DesktopRpcError> {
        decode_data(data)
    }
}

/// The data of a `host_tool_call` event.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct HostToolCall {
    #[serde(rename = "callId")]
    pub call_id: String,
    pub provider: String,
    pub tool: String,
    #[serde(default)]
    pub args: Value,
}

impl HostToolCall {
    pub(super) fn from_data(data: &Value) -> Result<Self, DesktopRpcError> {
        let call: Self = decode_data(data)?;
        if call.call_id.is_empty() || call.tool.is_empty() {
            return Err(DesktopRpcError::MalformedPayload(
                "host_tool_call needs a callId and a tool".into(),
            ));
        }
        Ok(call)
    }
}

/// The data of a `host_tool_cancel` event.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct HostToolCancel {
    #[serde(rename = "callId")]
    pub call_id: String,
}

impl HostToolCancel {
    pub(super) fn from_data(data: &Value) -> Result<Self, DesktopRpcError> {
        decode_data(data)
    }
}
