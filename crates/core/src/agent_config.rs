//! Pi's own configuration documents a Gentle Shell server serves over
//! Desktop RPC: `custom_models` (its `models.json`) and `mcp_servers` (its
//! `mcp.json`, global and project). Each is edited here as the whole JSON
//! document, secrets shown as [`SECRET_MARKER`] (sent back in place, the
//! marker keeps that secret).

use serde_json::{Value, json};

/// What the server shows instead of a secret, and takes back to keep it.
pub const SECRET_MARKER: &str = "•••";

/// The providers a `models.json` document defines, in its order.
pub fn custom_providers(document: Option<&Value>) -> Vec<String> {
    keys(document.map(|document| &document["providers"]))
}

/// The servers an `mcp.json` document defines, in its order.
pub fn mcp_server_names(document: Option<&Value>) -> Vec<String> {
    keys(document.map(|document| &document["mcpServers"]))
}

fn keys(object: Option<&Value>) -> Vec<String> {
    object
        .and_then(Value::as_object)
        .map(|object| object.keys().cloned().collect())
        .unwrap_or_default()
}

/// The `mcp_servers` store's value: each layer's document when its file
/// reads well, and whether Pi reads the project one (a trusted project).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct McpLayers {
    pub project_trusted: bool,
    pub global: Option<Value>,
    pub project: Option<Value>,
}

impl McpLayers {
    pub fn from_value(value: Option<&Value>) -> Self {
        let Some(value) = value else {
            return Self::default();
        };
        let document = |key: &str| value.get(key).filter(|doc| doc.is_object()).cloned();
        Self {
            project_trusted: value["projectTrusted"].as_bool().unwrap_or(false),
            global: document("global"),
            project: document("project"),
        }
    }
}

/// Which document an editor holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentDocument {
    /// Pi's `models.json`.
    CustomModels,
    /// The agent home's `mcp.json`.
    McpGlobal,
    /// The project's `.pi/mcp.json`.
    McpProject,
}

impl AgentDocument {
    /// The file it is, as people know it.
    pub fn file_name(self) -> &'static str {
        match self {
            Self::CustomModels => "models.json",
            Self::McpGlobal => "mcp.json",
            Self::McpProject => ".pi/mcp.json",
        }
    }

    /// What a missing file starts as.
    pub fn empty(self) -> Value {
        match self {
            Self::CustomModels => json!({ "providers": {} }),
            Self::McpGlobal | Self::McpProject => json!({ "mcpServers": {} }),
        }
    }

    /// `document` (or, when its file is missing, [`Self::empty`]) as the
    /// editor shows it.
    pub fn text(self, document: Option<&Value>) -> String {
        let empty = self.empty();
        let document = document.unwrap_or(&empty);
        let mut text = serde_json::to_string_pretty(document).unwrap_or_else(|_| "{}".into());
        text.push('\n');
        text
    }
}

/// The edited text as a document to send, or what is wrong with it.
pub fn parse_document(text: &str) -> Result<Value, String> {
    let value: Value = serde_json::from_str(text).map_err(|error| {
        format!(
            "Not valid JSON (line {}, column {}): {}",
            error.line(),
            error.column(),
            reason(&error)
        )
    })?;
    if value.is_object() {
        Ok(value)
    } else {
        Err("The file must be a JSON object ({ … }).".into())
    }
}

/// serde's message without its own " at line … column …" suffix.
fn reason(error: &serde_json::Error) -> String {
    let text = error.to_string();
    match text.rfind(" at line ") {
        Some(at) => text[..at].to_string(),
        None => text,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{AgentDocument, McpLayers, custom_providers, mcp_server_names, parse_document};

    #[test]
    fn names_come_from_the_documents() {
        let models = json!({ "providers": { "ollama": {}, "lab": {} } });
        assert_eq!(custom_providers(Some(&models)), ["ollama", "lab"]);
        assert!(custom_providers(None).is_empty());
        assert!(custom_providers(Some(&json!({ "providers": [] }))).is_empty());
        let mcp = json!({ "mcpServers": { "docs": { "url": "https://x" } } });
        assert_eq!(mcp_server_names(Some(&mcp)), ["docs"]);
    }

    #[test]
    fn the_mcp_value_names_each_layer_that_reads_well() {
        let layers = McpLayers::from_value(Some(&json!({
            "projectTrusted": true,
            "global": { "mcpServers": {} },
        })));
        assert!(layers.project_trusted);
        assert_eq!(layers.global, Some(json!({ "mcpServers": {} })));
        assert_eq!(layers.project, None);
        assert_eq!(McpLayers::from_value(None), McpLayers::default());
    }

    #[test]
    fn a_missing_file_starts_empty_and_text_round_trips() {
        let text = AgentDocument::CustomModels.text(None);
        assert_eq!(parse_document(&text).unwrap(), json!({ "providers": {} }));
        let doc = json!({ "mcpServers": { "docs": { "env": { "TOKEN": "•••" } } } });
        let text = AgentDocument::McpGlobal.text(Some(&doc));
        assert!(text.ends_with('\n') && text.contains("\"•••\""));
        assert_eq!(parse_document(&text).unwrap(), doc);
    }

    #[test]
    fn bad_text_says_where() {
        let error = parse_document("{\n  \"providers\": {,\n}").unwrap_err();
        assert!(
            error.starts_with("Not valid JSON (line 2, column"),
            "{error}"
        );
        assert!(!error.contains(" at line "), "{error}");
        assert_eq!(
            parse_document("[1]").unwrap_err(),
            "The file must be a JSON object ({ … })."
        );
    }
}
