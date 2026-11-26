//! Pi's own configuration documents a Gentle Shell server serves over
//! Desktop RPC: `custom_models` (its `models.json`) and `mcp` (its
//! `mcp.json`, global and project, with the servers Pi loads from them).
//! Each is edited here as the whole JSON document, secrets shown as
//! [`SECRET_MARKER`] (sent back in place, the marker keeps that secret).

use serde_json::{Value, json};

/// What the server shows instead of a secret, and takes back to keep it.
pub const SECRET_MARKER: &str = "•••";

/// The providers a `models.json` document defines, in its order.
pub fn custom_providers(document: Option<&Value>) -> Vec<String> {
    keys(document.map(|document| &document["providers"]))
}

fn keys(object: Option<&Value>) -> Vec<String> {
    object
        .and_then(Value::as_object)
        .map(|object| object.keys().cloned().collect())
        .unwrap_or_default()
}

/// One server Pi loads from the `mcp.json` layers it reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServer {
    pub name: String,
    /// `stdio` or `http`, as Pi connects.
    pub transport: String,
    pub enabled: bool,
}

/// The `mcp` store's value: the servers Pi loads (at most 64, in its
/// order), how many were left out, and each layer's document where the
/// server reads it (a missing or unreadable file, or a project Pi doesn't
/// trust, has none).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct McpLayers {
    pub servers: Vec<McpServer>,
    /// Servers beyond the 64 listed, or left out for an overlong name.
    pub more: bool,
    pub global: Option<Value>,
    pub project: Option<Value>,
}

impl McpLayers {
    pub fn from_value(value: Option<&Value>) -> Self {
        let Some(value) = value else {
            return Self::default();
        };
        let servers = value["servers"]
            .as_array()
            .map(|all| {
                all.iter()
                    .filter_map(|server| {
                        Some(McpServer {
                            name: server["name"].as_str()?.to_string(),
                            transport: server["transport"].as_str().unwrap_or("").to_string(),
                            enabled: server["enabled"].as_bool().unwrap_or(true),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let documents = &value["documents"];
        let document = |key: &str| documents.get(key).filter(|doc| doc.is_object()).cloned();
        Self {
            servers,
            more: value["truncated"].as_bool().unwrap_or(false)
                || value["omitted"].as_u64().unwrap_or(0) > 0,
            global: document("global"),
            project: document("project"),
        }
    }

    /// The servers as a line: each name, a disabled one marked so.
    pub fn summary(&self) -> String {
        let mut names: Vec<String> = self
            .servers
            .iter()
            .map(|server| {
                if server.enabled {
                    server.name.clone()
                } else {
                    format!("{} (off)", server.name)
                }
            })
            .collect();
        if self.more {
            names.push("…".into());
        }
        if names.is_empty() {
            "—".into()
        } else {
            names.join(", ")
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

    use super::{AgentDocument, McpLayers, custom_providers, parse_document};

    #[test]
    fn providers_come_from_the_document() {
        let models = json!({ "providers": { "ollama": {}, "lab": {} } });
        assert_eq!(custom_providers(Some(&models)), ["ollama", "lab"]);
        assert!(custom_providers(None).is_empty());
        assert!(custom_providers(Some(&json!({ "providers": [] }))).is_empty());
    }

    #[test]
    fn the_mcp_value_lists_the_servers_and_each_readable_document() {
        let layers = McpLayers::from_value(Some(&json!({
            "servers": [
                { "name": "docs", "transport": "http", "enabled": true },
                { "name": "fs", "transport": "stdio", "enabled": false },
            ],
            "truncated": false,
            "omitted": 0,
            "documents": { "global": { "mcpServers": {} } },
        })));
        assert_eq!(layers.servers.len(), 2);
        assert_eq!(layers.servers[0].transport, "http");
        assert_eq!(layers.summary(), "docs, fs (off)");
        assert_eq!(layers.global, Some(json!({ "mcpServers": {} })));
        assert_eq!(layers.project, None);
        let cut = McpLayers::from_value(Some(&json!({ "servers": [], "omitted": 1 })));
        assert_eq!(cut.summary(), "…");
        assert_eq!(McpLayers::from_value(None), McpLayers::default());
        assert_eq!(McpLayers::default().summary(), "—");
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
