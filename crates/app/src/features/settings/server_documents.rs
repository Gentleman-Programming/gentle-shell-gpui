//! Pi's `models.json` and `mcp.json` on a Gentle Shell server, edited as
//! JSON in Configuration and saved over Desktop RPC (`custom_models`,
//! `mcp_servers`): the server validates a write with Pi's own rules and
//! refuses one over a file that changed since the editor opened it. Secrets
//! come as `•••` and stay as they are when sent back so. Nothing here
//! touches a file: a server too old for these stores keeps saying "Do this
//! on <server>".

use gentle_core::agent_config::{
    AgentDocument, McpLayers, SECRET_MARKER, custom_providers, mcp_server_names, parse_document,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Editor, EditorState};
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::*;
use pi_rpc::desktop_rpc::{
    ConfigState, ConfigStore, ConfigWrite, ConfigWriteTarget, CustomModelsData, McpDocumentData,
    McpLayer,
};

use super::SettingsView;
use super::layout::{row, section};
use super::remote::{ServerPlace, note};
use crate::shared::ui;

/// One document open in the editor.
pub(super) struct DocumentEditor {
    document: AgentDocument,
    editor: Entity<EditorState>,
    /// The file's write revision when it was opened: a save goes over it,
    /// so a change made meanwhile is refused instead of overwritten.
    revision: String,
    saving: bool,
}

/// The editor open, and what the last save said.
#[derive(Default)]
pub(super) struct ServerDocuments {
    open: Option<DocumentEditor>,
    /// A save that landed, in words.
    saved: Option<String>,
}

/// The store a document lives in.
fn store_of(document: AgentDocument) -> ConfigStore {
    match document {
        AgentDocument::CustomModels => ConfigStore::CustomModels,
        AgentDocument::McpGlobal | AgentDocument::McpProject => ConfigStore::McpServers,
    }
}

/// The write target that guards it.
fn target_of(document: AgentDocument) -> ConfigWriteTarget {
    match document {
        AgentDocument::McpProject => ConfigWriteTarget::Project,
        AgentDocument::CustomModels | AgentDocument::McpGlobal => ConfigWriteTarget::Global,
    }
}

/// The document as the store read it (`None`: its file is missing or
/// doesn't read).
fn document_in(document: AgentDocument, state: &ConfigState) -> Option<serde_json::Value> {
    match document {
        AgentDocument::CustomModels => state.value.clone(),
        AgentDocument::McpGlobal => McpLayers::from_value(state.value.as_ref()).global,
        AgentDocument::McpProject => McpLayers::from_value(state.value.as_ref()).project,
    }
}

/// The write of `text` as `document`, or what is wrong with it.
fn write_of(document: AgentDocument, text: &str) -> Result<ConfigWrite, String> {
    let value = parse_document(text)?;
    let invalid = |error: pi_rpc::desktop_rpc::DesktopRpcError| error.to_string();
    Ok(match document {
        AgentDocument::CustomModels => {
            ConfigWrite::CustomModels(CustomModelsData::new(value).map_err(invalid)?)
        }
        AgentDocument::McpGlobal | AgentDocument::McpProject => ConfigWrite::McpServers {
            layer: if document == AgentDocument::McpProject {
                McpLayer::Project
            } else {
                McpLayer::Global
            },
            document: McpDocumentData::new(value).map_err(invalid)?,
        },
    })
}

impl SettingsView {
    /// Open `document` in the editor, as the server last reported it.
    fn open_document(
        &mut self,
        document: AgentDocument,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.shell_config.state(store_of(document)) else {
            return;
        };
        let Some(revision) = state.write_revision(target_of(document)) else {
            return;
        };
        let revision = revision.to_string();
        let text = document.text(document_in(document, state).as_ref());
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .line_number(true)
                .soft_wrap(false)
                .language("json")
                .default_value(text)
        });
        self.server_documents.open = Some(DocumentEditor {
            document,
            editor,
            revision,
            saving: false,
        });
        self.server_documents.saved = None;
        self.config_note(store_of(document), None);
        cx.notify();
    }

    /// Drop the edits and load what the server has now.
    fn reload_document(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(open) = self.server_documents.open.take() {
            self.open_document(open.document, window, cx);
        }
    }

    fn close_document(&mut self, cx: &mut Context<Self>) {
        if let Some(open) = self.server_documents.open.take() {
            self.config_note(store_of(open.document), None);
        }
        cx.notify();
    }

    fn save_document(&mut self, cx: &mut Context<Self>) {
        let Some(open) = self.server_documents.open.as_ref() else {
            return;
        };
        let (document, revision) = (open.document, open.revision.clone());
        let text = open.editor.read(cx).text().to_string();
        match write_of(document, &text) {
            Ok(write) => {
                if self.send_shell_write(write, revision, cx)
                    && let Some(open) = self.server_documents.open.as_mut()
                {
                    open.saving = true;
                }
            }
            Err(message) => {
                self.config_note(store_of(document), Some(message));
            }
        }
        cx.notify();
    }

    /// A config answer landed: a save that went through closes the editor.
    pub(super) fn documents_changed(&mut self, cx: &mut Context<Self>) {
        let Some(open) = self.server_documents.open.as_mut() else {
            return;
        };
        let store = store_of(open.document);
        if !open.saving || self.shell_config.writing(store) {
            return;
        }
        open.saving = false;
        if self.shell_config.note(store).is_some() {
            cx.notify();
            return;
        }
        let document = open.document;
        self.server_documents.open = None;
        let server = self
            .server()
            .map_or_else(String::new, |server| server.name.clone());
        self.server_documents.saved = Some(match document {
            AgentDocument::CustomModels => format!(
                "Saved models.json on {server}. Its agents pick the models up on their own."
            ),
            AgentDocument::McpGlobal | AgentDocument::McpProject => format!(
                "Saved {} on {server}. Pi loads MCP servers when an agent starts: the session \
                 Configuration reads reloads once idle, other agents with /reload.",
                document.file_name()
            ),
        });
        cx.notify();
    }

    /// Whether the server serves `store`: `Some(state)` once read, `None`
    /// while asking; `Err(())` when it doesn't (too old).
    fn served(&self, store: ConfigStore, cx: &App) -> Result<Option<&ConfigState>, ()> {
        if self.server_lacks(store, cx) {
            return Err(());
        }
        Ok(self.shell_config.state(store))
    }

    /// Models › Custom models on a server: the providers its `models.json`
    /// defines, and editing it there.
    pub(super) fn server_custom_models(&self, server: &ServerPlace, cx: &mut Context<Self>) -> Div {
        let card = section(IconName::Braces, "Custom models");
        let state = match self.served(ConfigStore::CustomModels, cx) {
            Err(()) => {
                return card.child(note(
                    server.act("edit models.json in its Gentle Shell agent folder"),
                ));
            }
            Ok(None) => return card.child(asking(&server.name)),
            Ok(Some(state)) => state,
        };
        let providers = custom_providers(state.value.as_ref());
        card.child(row(
            "Providers",
            if providers.is_empty() {
                "—".to_string()
            } else {
                providers.join(", ")
            },
        ))
        .child(self.document_area(&[AgentDocument::CustomModels], server, state, cx))
    }

    /// Integrations › MCP servers on a server: both layers' servers, and
    /// editing their `mcp.json` there.
    pub(super) fn server_mcp(&self, server: &ServerPlace, cx: &mut Context<Self>) -> Div {
        let card = section(IconName::Bot, "MCP servers");
        let state = match self.served(ConfigStore::McpServers, cx) {
            Err(()) => {
                return card.child(note(
                    server.act("edit mcp.json in its Gentle Shell agent folder"),
                ));
            }
            Ok(None) => return card.child(asking(&server.name)),
            Ok(Some(state)) => state,
        };
        let layers = McpLayers::from_value(state.value.as_ref());
        let names = |document: Option<&serde_json::Value>| {
            let names = mcp_server_names(document);
            if names.is_empty() {
                "—".to_string()
            } else {
                names.join(", ")
            }
        };
        // A dedicated session's folder is the server's home, not a
        // project: only the active tab's project is offered.
        let project = layers.project_trusted && !self.control.hidden();
        let mut documents = vec![AgentDocument::McpGlobal];
        if project {
            documents.push(AgentDocument::McpProject);
        }
        card.child(row("Global (mcp.json)", names(layers.global.as_ref())))
            .children(project.then(|| {
                row(
                    "This project (.pi/mcp.json)",
                    names(layers.project.as_ref()),
                )
            }))
            .child(self.document_area(&documents, server, state, cx))
    }

    /// The Edit buttons for `documents`, or the editor of the one open, and
    /// what the last save said.
    fn document_area(
        &self,
        documents: &[AgentDocument],
        server: &ServerPlace,
        state: &ConfigState,
        cx: &mut Context<Self>,
    ) -> Div {
        let store = store_of(documents[0]);
        let open = self
            .server_documents
            .open
            .as_ref()
            .filter(|open| documents.contains(&open.document));
        let notice = |text: &str, color: Hsla| {
            div()
                .pt_1()
                .text_xs()
                .text_color(color)
                .whitespace_normal()
                .child(text.to_string())
        };
        let Some(open) = open else {
            let saved = self
                .server_documents
                .saved
                .as_deref()
                .filter(|_| self.server_documents.open.is_none());
            return v_flex()
                .pt_2()
                .gap_1()
                .child(h_flex().gap_2().children(documents.iter().map(|&document| {
                    Button::new(SharedString::from(format!("edit-{}", document.file_name())))
                        .ghost()
                        .small()
                        .icon(Icon::new(IconName::Pencil))
                        .label(format!("Edit {} on {}", document.file_name(), server.name))
                        .disabled(state.write_revision(target_of(document)).is_none())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_document(document, window, cx)
                        }))
                })))
                .children(saved.map(|text| notice(text, ui::success())));
        };
        let stale = state
            .write_revision(target_of(open.document))
            .is_some_and(|now| now != open.revision);
        v_flex()
            .pt_2()
            .gap_2()
            .child(
                div()
                    .text_xs()
                    .text_color(ui::muted())
                    .whitespace_normal()
                    .child(format!(
                        "{} on {}. Secrets show as {SECRET_MARKER}: leave one as it is to keep \
                         it. The server checks the file with Pi's own rules before saving.",
                        open.document.file_name(),
                        server.name
                    )),
            )
            .child(
                ui::well_box().h(rems(320. / 16.)).child(
                    Editor::new(&open.editor)
                        .appearance(false)
                        .font_family(ui::mono())
                        .text_size(ui::code_rems(12.))
                        .size_full(),
                ),
            )
            .children(stale.then(|| {
                notice(
                    "It changed on the server since you opened it: Reload shows it \
                             (your edits are dropped).",
                    ui::warning(),
                )
            }))
            .children(
                self.shell_config
                    .note(store)
                    .map(|text| notice(text, ui::danger())),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("document-save")
                            .primary()
                            .small()
                            .label("Save")
                            .loading(open.saving)
                            .disabled(open.saving)
                            .on_click(cx.listener(|this, _, _, cx| this.save_document(cx))),
                    )
                    .child(
                        Button::new("document-reload")
                            .ghost()
                            .small()
                            .label("Reload")
                            .disabled(open.saving)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.reload_document(window, cx)),
                            ),
                    )
                    .child(
                        Button::new("document-cancel")
                            .ghost()
                            .small()
                            .label("Cancel")
                            .disabled(open.saving)
                            .on_click(cx.listener(|this, _, _, cx| this.close_document(cx))),
                    ),
            )
    }
}

/// "Asking <server>…" while its answer is on the way.
fn asking(server: &str) -> Div {
    div()
        .text_xs()
        .text_color(ui::muted())
        .child(format!("Asking {server}…"))
}

#[cfg(test)]
mod tests {
    use gentle_core::agent_config::AgentDocument;
    use pi_rpc::desktop_rpc::{
        ConfigLayer, ConfigLayerSource, ConfigScope, ConfigState, ConfigStatus, ConfigStore,
        ConfigWrite, ConfigWriteTarget, ConfigWriteTargetState, McpLayer,
    };
    use serde_json::json;

    use super::{document_in, write_of};

    fn mcp_state() -> ConfigState {
        ConfigState {
            store: ConfigStore::McpServers,
            scope: ConfigScope::GlobalProject,
            read_only: false,
            status: ConfigStatus::Ok,
            revision: "r1".into(),
            value: Some(json!({
                "projectTrusted": true,
                "global": { "mcpServers": { "docs": { "url": "https://x" } } },
            })),
            layers: vec![ConfigLayer {
                source: ConfigLayerSource::Global,
                status: ConfigStatus::Ok,
            }],
            write_targets: Some(vec![
                ConfigWriteTargetState {
                    target: ConfigWriteTarget::Global,
                    status: ConfigStatus::Ok,
                    write_revision: "wg".into(),
                },
                ConfigWriteTargetState {
                    target: ConfigWriteTarget::Project,
                    status: ConfigStatus::Missing,
                    write_revision: "wp".into(),
                },
            ]),
        }
    }

    #[test]
    fn each_mcp_layer_opens_its_own_document_and_saves_to_it() {
        let state = mcp_state();
        assert_eq!(
            document_in(AgentDocument::McpGlobal, &state),
            Some(json!({ "mcpServers": { "docs": { "url": "https://x" } } }))
        );
        // A missing project file opens empty.
        assert_eq!(document_in(AgentDocument::McpProject, &state), None);
        let write = write_of(AgentDocument::McpProject, "{ \"mcpServers\": {} }").unwrap();
        assert!(matches!(
            &write,
            ConfigWrite::McpServers {
                layer: McpLayer::Project,
                ..
            }
        ));
        assert_eq!(write.write_target(), ConfigWriteTarget::Project);
    }

    #[test]
    fn text_that_isnt_a_document_is_refused_before_sending() {
        assert!(
            write_of(AgentDocument::CustomModels, "{ providers: }")
                .unwrap_err()
                .starts_with("Not valid JSON")
        );
        let write = write_of(AgentDocument::CustomModels, "{ \"providers\": {} }").unwrap();
        assert_eq!(write.store(), ConfigStore::CustomModels);
    }
}
