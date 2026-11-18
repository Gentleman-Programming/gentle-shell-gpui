//! Conversation state rebuilt from Pi session events. Pure data: the view
//! layer renders it and owns no protocol knowledge.

use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Text(String),
    Thinking(String),
    /// A tool call announced by the model; the matching [`Item::Tool`] shows it.
    ToolCall,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    Running,
    Done,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeLevel {
    Info,
    Warning,
    Error,
}

/// An image in a user message, still base64-encoded as Pi stores it.
#[derive(Debug, Clone, PartialEq)]
pub struct UserImage {
    pub mime_type: String,
    pub data: std::sync::Arc<str>,
    /// [`image_key`] of `data`, computed once: the view's decode-cache key.
    pub key: u64,
}

impl UserImage {
    pub fn new(mime_type: impl Into<String>, data: std::sync::Arc<str>) -> Self {
        Self {
            mime_type: mime_type.into(),
            key: image_key(&data),
            data,
        }
    }

    /// Pi's `ImageContent` block: `{"type":"image","data":…,"mimeType":…}`.
    fn parse(block: &Value) -> Option<Self> {
        if block["type"] != "image" {
            return None;
        }
        let data = block["data"].as_str()?;
        Some(Self::new(
            block["mimeType"].as_str().unwrap_or_default(),
            data.into(),
        ))
    }
}

/// A stable identity for base64 image data within this process.
pub fn image_key(data: &str) -> u64 {
    use std::hash::{DefaultHasher, Hash as _, Hasher as _};
    let mut hasher = DefaultHasher::new();
    data.hash(&mut hasher);
    hasher.finish()
}

/// Whether a step starts open, from its kind and status: a failed tool
/// step does (its error is why you'd look at it), every other collapsible
/// step starts folded to its quiet one-line row. `None` for items that
/// never fold this way — `Shell` (what you ran yourself) keeps its output
/// visible, and the other items are not steps.
#[derive(Debug, Clone)]
pub enum Item {
    User {
        text: String,
        images: Vec<UserImage>,
    },
    Assistant {
        blocks: Vec<Block>,
        streaming: bool,
        error: Option<String>,
    },
    Tool {
        call_id: String,
        name: String,
        args: Value,
        status: ToolStatus,
        output: String,
        details: Value,
        duration_ms: Option<u64>,
    },
    /// A `!command` run through RPC `bash`.
    Shell {
        command: String,
        output: String,
        exit_code: Option<i64>,
        running: bool,
    },
    Notice {
        level: NoticeLevel,
        text: String,
    },
    /// A visible extension message, e.g. `gentle-agents.result` (a helper's
    /// answer); `details` is the extension's display data.
    Custom {
        kind: String,
        text: String,
        details: Value,
    },
    /// What an extension command (`/gentle:doctor`) reported through
    /// `notify`, gathered into one result.
    CommandOutput {
        command: String,
        notices: Vec<(NoticeLevel, String)>,
    },
}

/// Whether a step starts open, from its kind and status: a failed tool
/// step does (its error is why you'd look at it), every other collapsible
/// step starts folded to its quiet one-line row. `None` for items that
/// never fold this way — `Shell` (what you ran yourself) keeps its output
/// visible, the generative-UI tool and its event cards are rendered by the
/// genui views as their own live cards (content, not collapsible steps),
/// and the other items are not steps.
pub fn step_open_by_default(item: &Item) -> Option<bool> {
    match item {
        Item::Tool { name, status, .. } if name == crate::genui::TOOL => None,
        Item::Tool { status, .. } => Some(*status == ToolStatus::Failed),
        Item::Custom { kind, .. } if kind == crate::genui::EVENT_MESSAGE => None,
        Item::Custom { .. } => Some(false),
        _ => None,
    }
}

/// A run of consecutive finished steps folded to one quiet summary line
/// (S11c): the transcript items it covers and the label it shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepGroup {
    /// The items the group folds, always two or more.
    pub items: std::ops::Range<usize>,
    /// The summary, e.g. "Ran 3 commands (1 error)".
    pub label: String,
}

/// What a step counts as in its group's summary, classified the way the
/// app's cards are (`bash` and friends run commands, `read` reads files,
/// `edit`/`write` edit them, helper cards finish background tasks).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StepKind {
    Command,
    Read,
    Edit,
    Helper,
    Other,
}

fn step_kind(item: &Item) -> StepKind {
    match item {
        Item::Tool { name, .. } => match name.as_str() {
            "bash" | "bash_background" | "monitor" | "codemode" => StepKind::Command,
            "read" => StepKind::Read,
            "edit" | "write" => StepKind::Edit,
            _ => StepKind::Other,
        },
        Item::Custom { .. } => StepKind::Helper,
        _ => StepKind::Other,
    }
}

/// One clause of a group summary, capitalized only when first ("Read 2
/// files, edited 1 file").
fn step_clause(kind: StepKind, n: usize, first: bool) -> String {
    let noun = |one: &'static str, many: &'static str| if n == 1 { one } else { many };
    let verb = match kind {
        StepKind::Command => {
            if first {
                "Ran"
            } else {
                "ran"
            }
        }
        StepKind::Read => {
            if first {
                "Read"
            } else {
                "read"
            }
        }
        StepKind::Edit => {
            if first {
                "Edited"
            } else {
                "edited"
            }
        }
        StepKind::Helper => {
            if first {
                "Finished"
            } else {
                "finished"
            }
        }
        StepKind::Other => {
            if first {
                "Used"
            } else {
                "used"
            }
        }
    };
    let (one, many) = match kind {
        StepKind::Command => ("command", "commands"),
        StepKind::Read | StepKind::Edit => ("file", "files"),
        StepKind::Helper => ("background task", "background tasks"),
        StepKind::Other => ("tool", "tools"),
    };
    format!("{verb} {} {}", n, noun(one, many))
}

/// Whether a group member failed, for the summary's error count. Helper
/// cards carry no status; only failed tool steps count.
fn step_failed(item: &Item) -> bool {
    matches!(
        item,
        Item::Tool {
            status: ToolStatus::Failed,
            ..
        }
    )
}

/// The summary of the steps in a run: one clause per kind in order of first
/// appearance, joined with ", ", with the errors named — " (1 error)" after
/// a single clause, ", 1 with error" after several.
pub fn step_group_label(steps: &[&Item]) -> String {
    let mut clauses: Vec<(StepKind, usize)> = Vec::new();
    let mut errors = 0;
    for step in steps {
        let kind = step_kind(step);
        match clauses.iter_mut().find(|(k, _)| *k == kind) {
            Some((_, count)) => *count += 1,
            None => clauses.push((kind, 1)),
        }
        errors += usize::from(step_failed(step));
    }
    let label = clauses
        .iter()
        .enumerate()
        .map(|(ix, (kind, n))| step_clause(*kind, *n, ix == 0))
        .collect::<Vec<_>>()
        .join(", ");
    let noun = if errors == 1 { "error" } else { "errors" };
    match (errors, clauses.len()) {
        (0, _) => label,
        (_, 1) => format!("{label} ({errors} {noun})"),
        (_, _) => format!("{label}, {errors} with {noun}"),
    }
}

/// Whether `item` is a finished collapsible step that can join a run: what
/// [`step_open_by_default`] folds to its quiet line, that isn't running.
fn groupable_step(item: &Item) -> bool {
    match item {
        Item::Tool { status, .. } => {
            step_open_by_default(item).is_some() && *status != ToolStatus::Running
        }
        _ => step_open_by_default(item).is_some(),
    }
}

/// The runs of two or more consecutive finished collapsible steps
/// ([`Item::Tool`], helper cards) in a transcript, each with its summary
/// label. Everything else — assistant text, thinking, what you ran with
/// `!`, generative-UI cards, notices — ends a run; a running step breaks
/// it and stands alone until it finishes. A finished assistant message
/// with nothing to show ([`assistant_visible`], a message that only
/// announced tool calls) is transparent: the run flows across it.
pub fn step_groups(items: &[Item]) -> Vec<StepGroup> {
    let mut groups = Vec::new();
    let mut run: Vec<usize> = Vec::new();
    for (ix, item) in items.iter().enumerate() {
        if groupable_step(item) {
            run.push(ix);
        } else if assistant_visible(item) {
            flush_group(&mut run, &mut groups, items);
        }
    }
    flush_group(&mut run, &mut groups, items);
    groups
}

/// Turn the current run into a group if it holds two or more steps.
fn flush_group(run: &mut Vec<usize>, groups: &mut Vec<StepGroup>, items: &[Item]) {
    if run.len() >= 2 {
        let steps: Vec<&Item> = run.iter().map(|&ix| &items[ix]).collect();
        groups.push(StepGroup {
            items: run[0]..run[run.len() - 1] + 1,
            label: step_group_label(&steps),
        });
    }
    run.clear();
}

/// Whether an assistant message shows anything to the user: text beyond
/// whitespace, thinking (kept as today), an error — or it is still
/// streaming ("Working…"). A finished message that only announced tool
/// calls shows nothing: it renders no row, and a run of steps flows
/// across it as if it weren't there.
pub fn assistant_visible(item: &Item) -> bool {
    match item {
        Item::Assistant {
            blocks,
            streaming,
            error,
        } => {
            *streaming
                || error.is_some()
                || blocks.iter().any(|b| match b {
                    Block::Text(text) | Block::Thinking(text) => !text.trim().is_empty(),
                    Block::ToolCall => false,
                })
        }
        _ => true,
    }
}

/// Whether a step shows its quiet one-line row instead of its card: its
/// default ([`step_open_by_default`]) flipped once by the user's click
/// (`toggled`). Items that never fold this way always show their card.
pub fn step_folded(item: &Item, toggled: bool) -> bool {
    step_open_by_default(item).is_some_and(|open| open == toggled)
}

/// Token usage reported while the latest assistant message streams.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub cost: f64,
}

pub struct Transcript {
    pub items: Vec<Item>,
    /// Index into `items` of the assistant message being streamed.
    streaming: Option<usize>,
    pub running: bool,
    pub last_usage: Option<Usage>,
    /// Changes not yet taken by the view: see [`Transcript::take_dirty`].
    dirty: Option<Dirty>,
    /// `!command` shells still running.
    running_shells: usize,
    /// Tool items still [`ToolStatus::Running`].
    running_tools: usize,
}

impl Default for Transcript {
    /// A fresh transcript reports itself replaced, so views drop what they
    /// derived from the previous one.
    fn default() -> Self {
        Self {
            items: Vec::new(),
            streaming: None,
            running: false,
            last_usage: None,
            dirty: Some(Dirty {
                from: 0,
                reset: true,
            }),
            running_shells: 0,
            running_tools: 0,
        }
    }
}

/// What changed in `items` since the view last looked: every item at
/// `from` or later may differ (or be new); earlier items are untouched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dirty {
    pub from: usize,
    /// The whole list was replaced; indices no longer name the same items.
    pub reset: bool,
}

/// What an applied event changed, for side effects the view performs.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Effects {
    pub settled: bool,
    /// A tool finished that may have changed the working tree.
    pub files_touched: bool,
}

impl Transcript {
    /// Take the changes since the last call (`None`: nothing changed).
    /// The assistant message being streamed, if any.
    pub fn streaming_item(&self) -> Option<usize> {
        self.streaming
    }

    pub fn take_dirty(&mut self) -> Option<Dirty> {
        self.dirty.take()
    }

    /// Whether a `!command` shell is still running.
    pub fn shells_running(&self) -> bool {
        self.running_shells > 0
    }

    /// How many tool calls are still running.
    pub fn running_tools(&self) -> usize {
        self.running_tools
    }

    /// Item `ix` changed (or was added).
    fn touch(&mut self, ix: usize) {
        match &mut self.dirty {
            Some(dirty) => dirty.from = dirty.from.min(ix),
            None => {
                self.dirty = Some(Dirty {
                    from: ix,
                    reset: false,
                })
            }
        }
    }

    fn push(&mut self, item: Item) {
        self.touch(self.items.len());
        self.items.push(item);
    }

    pub fn apply(&mut self, event: &Value) -> Effects {
        let mut effects = Effects::default();
        let Some(kind) = event.get("type").and_then(Value::as_str) else {
            return effects;
        };
        match kind {
            "agent_start" => self.running = true,
            "agent_settled" => {
                self.running = false;
                self.close_stream();
                // Pi ends every tool, even on abort; one left running would
                // spin forever.
                self.settle_tools();
                effects.settled = true;
            }
            "message_start" => self.message_start(&event["message"]),
            "message_update" => {
                // Streaming updates often carry an empty usage; keep the
                // previous numbers until one reports output (or `message_end`).
                if let Some(usage) = parse_usage(&event["usage"]).filter(|u| u.output > 0) {
                    self.last_usage = Some(usage);
                }
                self.message_update(&event["assistantMessageEvent"]);
            }
            "message_end" => self.message_end(&event["message"]),
            "tool_execution_start" => {
                self.running_tools += 1;
                self.push(Item::Tool {
                    call_id: str_field(event, "toolCallId"),
                    name: str_field(event, "toolName"),
                    args: event["args"].clone(),
                    status: ToolStatus::Running,
                    output: String::new(),
                    details: Value::Null,
                    duration_ms: None,
                })
            }
            "tool_execution_update" => {
                let id = str_field(event, "toolCallId");
                if let Some((
                    ix,
                    Item::Tool {
                        name,
                        output,
                        details,
                        ..
                    },
                )) = self.tool_mut(&id)
                {
                    let partial = content_text(&event["partialResult"]["content"]);
                    let mut changed = *output != partial;
                    *output = partial;
                    // A gentle_ui review waiting for the user's decisions
                    // shows its surface while the call runs.
                    let shown = &event["partialResult"]["details"];
                    if name == crate::genui::TOOL && !shown.is_null() && *details != *shown {
                        *details = shown.clone();
                        changed = true;
                    }
                    if changed {
                        self.touch(ix);
                    }
                }
            }
            "tool_execution_end" => {
                let id = str_field(event, "toolCallId");
                let failed = event["isError"].as_bool().unwrap_or(false);
                if let Some((
                    ix,
                    Item::Tool {
                        name,
                        status,
                        output,
                        details,
                        duration_ms,
                        ..
                    },
                )) = self.tool_mut(&id)
                {
                    let was_running = *status == ToolStatus::Running;
                    *status = if failed {
                        ToolStatus::Failed
                    } else {
                        ToolStatus::Done
                    };
                    *output = content_text(&event["result"]["content"]);
                    *details = event["result"]["details"].clone();
                    *duration_ms = event["durationMs"].as_u64();
                    effects.files_touched = matches!(name.as_str(), "edit" | "write" | "bash");
                    if was_running {
                        self.running_tools = self.running_tools.saturating_sub(1);
                    }
                    self.touch(ix);
                }
            }
            "auto_retry_start" => self.notice(
                NoticeLevel::Warning,
                format!(
                    "Retrying ({}/{}): {}",
                    event["attempt"].as_u64().unwrap_or(0),
                    event["maxAttempts"].as_u64().unwrap_or(0),
                    event["errorMessage"].as_str().unwrap_or("provider error")
                ),
            ),
            "auto_retry_end" if event["success"] == false => self.notice(
                NoticeLevel::Error,
                event["finalError"]
                    .as_str()
                    .unwrap_or("Retries exhausted")
                    .to_string(),
            ),
            "compaction_start" => self.notice(NoticeLevel::Info, "Compacting context…".into()),
            "compaction_end" => {
                if let Some(error) = event["errorMessage"].as_str() {
                    let error = error.strip_prefix("Compaction failed: ").unwrap_or(error);
                    self.notice(NoticeLevel::Error, format!("Compaction failed: {error}"));
                } else if event["aborted"] == true {
                    self.notice(NoticeLevel::Info, "Compaction cancelled".into());
                } else if let Some(before) = event["result"]["tokensBefore"].as_u64() {
                    let text = match event["result"]["estimatedTokensAfter"].as_u64() {
                        Some(after) => format!(
                            "Context compacted from {} to ~{}",
                            format_tokens(before),
                            format_tokens(after)
                        ),
                        None => format!("Context compacted from {}", format_tokens(before)),
                    };
                    self.notice(NoticeLevel::Info, text);
                }
            }
            "extension_error" => self.notice(
                NoticeLevel::Error,
                format!(
                    "Extension error ({}): {}",
                    event["event"].as_str().unwrap_or("?"),
                    event["error"].as_str().unwrap_or("")
                ),
            ),
            _ => {}
        }
        effects
    }

    pub fn notice(&mut self, level: NoticeLevel, text: String) {
        self.push(Item::Notice { level, text });
    }

    /// An extension command's gathered `notify` output, as one result.
    pub fn command_output(&mut self, command: String, notices: Vec<(NoticeLevel, String)>) {
        self.push(Item::CommandOutput { command, notices });
    }

    /// Start a `!command` shell item and return its index.
    pub fn start_shell(&mut self, command: String) -> usize {
        self.running_shells += 1;
        self.push(Item::Shell {
            command,
            output: String::new(),
            exit_code: None,
            running: true,
        });
        self.items.len() - 1
    }

    pub fn shell_output(&mut self, index: usize, delta: &str) {
        if let Some(Item::Shell { output, .. }) = self.items.get_mut(index) {
            output.push_str(delta);
            // Trim only past twice the cap so appends stay amortized O(chunk).
            if output.len() > 2 * SHELL_OUTPUT_CAP {
                trim_shell_output(output);
            }
            self.touch(index);
        }
    }

    /// The backend went away mid-turn: nothing is streaming or running
    /// anymore, so nothing should keep animating.
    pub fn interrupt(&mut self) {
        self.running = false;
        self.close_stream();
        self.settle_tools();
        for ix in 0..self.items.len() {
            if let Item::Shell { running, .. } = &mut self.items[ix]
                && *running
            {
                *running = false;
                self.touch(ix);
            }
        }
        self.running_shells = 0;
    }

    /// Mark tools still running as failed.
    fn settle_tools(&mut self) {
        if self.running_tools == 0 {
            return;
        }
        for ix in 0..self.items.len() {
            if let Item::Tool { status, .. } = &mut self.items[ix]
                && *status == ToolStatus::Running
            {
                *status = ToolStatus::Failed;
                self.touch(ix);
            }
        }
        self.running_tools = 0;
    }

    pub fn finish_shell(&mut self, index: usize, code: Option<i64>, final_output: Option<String>) {
        if let Some(Item::Shell {
            output,
            exit_code,
            running,
            ..
        }) = self.items.get_mut(index)
        {
            if *running {
                self.running_shells = self.running_shells.saturating_sub(1);
            }
            *running = false;
            *exit_code = code;
            if let Some(text) = final_output.filter(|_| output.is_empty()) {
                *output = text;
            }
            self.touch(index);
        }
    }

    /// Replace the conversation with persisted messages (`get_messages`).
    pub fn load_messages(&mut self, messages: &[Value]) {
        self.items.clear();
        self.streaming = None;
        self.running_shells = 0;
        self.running_tools = 0;
        self.dirty = Some(Dirty {
            from: 0,
            reset: true,
        });
        let mut results = std::collections::HashMap::new();
        for message in messages {
            if message["role"] == "toolResult" {
                results.insert(str_field(message, "toolCallId"), message);
            }
        }
        for message in messages {
            match message["role"].as_str() {
                Some("user") => self.message_start(message),
                Some("assistant") => {
                    self.message_start(message);
                    self.message_end(message);
                    for block in message["content"].as_array().into_iter().flatten() {
                        if block["type"] != "toolCall" {
                            continue;
                        }
                        let id = str_field(block, "id");
                        let result = results.get(&id);
                        let failed = result.is_some_and(|r| r["isError"] == true);
                        self.items.push(Item::Tool {
                            name: str_field(block, "name"),
                            args: block["arguments"].clone(),
                            status: match result {
                                None => ToolStatus::Failed,
                                Some(_) if failed => ToolStatus::Failed,
                                Some(_) => ToolStatus::Done,
                            },
                            output: result
                                .map(|r| content_text(&r["content"]))
                                .unwrap_or_default(),
                            details: result.map(|r| r["details"].clone()).unwrap_or_default(),
                            duration_ms: None,
                            call_id: id,
                        });
                    }
                }
                Some("custom") => self.push_custom(message),
                Some("bashExecution") => self.items.push(Item::Shell {
                    command: str_field(message, "command"),
                    output: str_field(message, "output"),
                    exit_code: message["exitCode"].as_i64(),
                    running: false,
                }),
                _ => {}
            }
        }
    }

    fn message_start(&mut self, message: &Value) {
        match message["role"].as_str() {
            Some("user") => {
                let (text, images) = match &message["content"] {
                    Value::String(text) => (text.clone(), Vec::new()),
                    Value::Array(blocks) => (
                        content_text(&message["content"]),
                        blocks.iter().filter_map(UserImage::parse).collect(),
                    ),
                    _ => (String::new(), Vec::new()),
                };
                self.push(Item::User { text, images });
            }
            Some("assistant") => {
                self.close_stream();
                self.push(Item::Assistant {
                    blocks: parse_blocks(&message["content"]),
                    streaming: true,
                    error: None,
                });
                self.streaming = Some(self.items.len() - 1);
            }
            _ => {}
        }
    }

    fn message_update(&mut self, update: &Value) {
        let Some(stream_ix) = self.streaming else {
            return;
        };
        let Some(Item::Assistant { blocks, .. }) = self.items.get_mut(stream_ix) else {
            return;
        };
        let index = update["contentIndex"].as_u64().unwrap_or(0) as usize;
        let ensure = |blocks: &mut Vec<Block>, block: Block| {
            while blocks.len() <= index {
                blocks.push(Block::ToolCall);
            }
            blocks[index] = block;
        };
        match update["type"].as_str() {
            Some("text_start") => ensure(blocks, Block::Text(String::new())),
            Some("thinking_start") => ensure(blocks, Block::Thinking(String::new())),
            Some("toolcall_start") => ensure(blocks, Block::ToolCall),
            Some("text_delta") => match blocks.get_mut(index) {
                Some(Block::Text(text)) => text.push_str(update["delta"].as_str().unwrap_or("")),
                _ => ensure(blocks, Block::Text(str_field(update, "delta"))),
            },
            Some("thinking_delta") => match blocks.get_mut(index) {
                Some(Block::Thinking(text)) => {
                    text.push_str(update["delta"].as_str().unwrap_or(""))
                }
                _ => ensure(blocks, Block::Thinking(str_field(update, "delta"))),
            },
            Some("text_end") => ensure(blocks, Block::Text(str_field(update, "content"))),
            Some("thinking_end") => ensure(blocks, Block::Thinking(str_field(update, "content"))),
            _ => {}
        }
        self.touch(stream_ix);
    }

    fn message_end(&mut self, message: &Value) {
        if message["role"] == "custom" {
            self.push_custom(message);
            return;
        }
        if message["role"] != "assistant" {
            return;
        }
        let Some(index) = self.streaming.take() else {
            return;
        };
        if let Some(Item::Assistant {
            blocks,
            streaming,
            error,
        }) = self.items.get_mut(index)
        {
            *blocks = parse_blocks(&message["content"]);
            *streaming = false;
            *error = match message["stopReason"].as_str() {
                Some("error") => Some(
                    message["errorMessage"]
                        .as_str()
                        .unwrap_or("The provider returned an error")
                        .to_string(),
                ),
                Some("aborted") => Some("Aborted".into()),
                _ => None,
            };
            self.touch(index);
        }
        if let Some(usage) = parse_usage(&message["usage"]) {
            self.last_usage = Some(usage);
        }
    }

    fn push_custom(&mut self, message: &Value) {
        if message["display"] == false {
            return;
        }
        let text = content_text(&message["content"]);
        if text.trim().is_empty() {
            return;
        }
        self.push(Item::Custom {
            kind: str_field(message, "customType"),
            text,
            details: message["details"].clone(),
        });
    }

    fn close_stream(&mut self) {
        if let Some(index) = self.streaming.take()
            && let Some(Item::Assistant { streaming, .. }) = self.items.get_mut(index)
        {
            *streaming = false;
            self.touch(index);
        }
    }

    fn tool_mut(&mut self, id: &str) -> Option<(usize, &mut Item)> {
        self.items
            .iter_mut()
            .enumerate()
            .rev()
            .find(|(_, item)| matches!(item, Item::Tool { call_id, .. } if call_id == id))
    }
}

fn parse_blocks(content: &Value) -> Vec<Block> {
    content
        .as_array()
        .into_iter()
        .flatten()
        .map(|block| match block["type"].as_str() {
            Some("text") => Block::Text(str_field(block, "text")),
            Some("thinking") => Block::Thinking(str_field(block, "thinking")),
            _ => Block::ToolCall,
        })
        .collect()
}

fn parse_usage(usage: &Value) -> Option<Usage> {
    let input = usage.get("input")?.as_u64()?;
    Some(Usage {
        input,
        output: usage["output"].as_u64().unwrap_or(0),
        cache_read: usage["cacheRead"].as_u64().unwrap_or(0),
        cache_write: usage["cacheWrite"].as_u64().unwrap_or(0),
        cost: usage["cost"]["total"].as_f64().unwrap_or(0.0),
    })
}

/// How much `!command` output a shell item keeps (its tail).
const SHELL_OUTPUT_CAP: usize = 256 * 1024;
/// First line of shell output whose head was dropped.
const SHELL_TRIMMED: &str = "[earlier output trimmed]\n";

/// Keep the last [`SHELL_OUTPUT_CAP`] bytes (on a char boundary), marked.
fn trim_shell_output(output: &mut String) {
    let body = output.strip_prefix(SHELL_TRIMMED).unwrap_or(output);
    let mut cut = output.len() - body.len() + body.len().saturating_sub(SHELL_OUTPUT_CAP);
    while !output.is_char_boundary(cut) {
        cut += 1;
    }
    output.replace_range(..cut, SHELL_TRIMMED);
}

/// Concatenate the text blocks of a tool result or user message.
pub fn content_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

pub fn str_field(value: &Value, key: &str) -> String {
    value[key].as_str().unwrap_or_default().to_string()
}

pub fn format_tokens(tokens: u64) -> String {
    match tokens {
        0..1_000 => tokens.to_string(),
        1_000..1_000_000 => format!("{:.1}k", tokens as f64 / 1e3).replace(".0k", "k"),
        _ => format!("{:.1}M", tokens as f64 / 1e6).replace(".0M", "M"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_textless_assistant_message_shows_nothing() {
        let assistant =
            |blocks: Vec<Block>, streaming: bool, error: Option<&str>| Item::Assistant {
                blocks,
                streaming,
                error: error.map(String::from),
            };
        // A message that only announced tool calls shows nothing.
        assert!(!assistant_visible(&assistant(
            vec![Block::ToolCall, Block::ToolCall],
            false,
            None
        )));
        // Whitespace text alone is no content either.
        assert!(!assistant_visible(&assistant(
            vec![Block::Text("  \n\t".into())],
            false,
            None
        )));
        // Text, thinking, an error and a live message all show.
        assert!(assistant_visible(&assistant(
            vec![Block::Text("hi".into())],
            false,
            None
        )));
        assert!(assistant_visible(&assistant(
            vec![Block::Thinking("hmm".into())],
            false,
            None
        )));
        assert!(assistant_visible(&assistant(vec![], false, Some("boom"))));
        assert!(assistant_visible(&assistant(vec![], true, None)));
        // Anything else always shows.
        assert!(assistant_visible(&Item::User {
            text: "hi".into(),
            images: vec![],
        }));
    }

    #[test]
    fn a_step_shows_its_line_until_a_click_unfolds_it() {
        let tool = |status| Item::Tool {
            call_id: "a".into(),
            name: "bash".into(),
            args: json!({}),
            status,
            output: "out".into(),
            details: json!({}),
            duration_ms: Some(10),
        };
        // A finished step is folded; a click unfolds it, another folds it.
        assert!(step_folded(&tool(ToolStatus::Done), false));
        assert!(!step_folded(&tool(ToolStatus::Done), true));
        // A failed step starts open; a click folds it.
        assert!(!step_folded(&tool(ToolStatus::Failed), false));
        assert!(step_folded(&tool(ToolStatus::Failed), true));
        // A helper card folds like a finished step.
        let helper = Item::Custom {
            kind: "gentle-agents.result".into(),
            text: "done".into(),
            details: json!({}),
        };
        assert!(step_folded(&helper, false));
        // What never folds stays open whatever the toggle says.
        let shell = Item::Shell {
            command: "ls".into(),
            output: String::new(),
            exit_code: Some(0),
            running: false,
        };
        assert!(!step_folded(&shell, false));
        assert!(!step_folded(&shell, true));
    }

    #[test]
    fn steps_open_by_default_only_when_failed() {
        let tool = |status| Item::Tool {
            call_id: "a".into(),
            name: "bash".into(),
            args: json!({}),
            status,
            output: String::new(),
            details: json!({}),
            duration_ms: None,
        };
        // Running and done steps fold to their quiet line.
        assert_eq!(
            step_open_by_default(&tool(ToolStatus::Running)),
            Some(false)
        );
        assert_eq!(step_open_by_default(&tool(ToolStatus::Done)), Some(false));
        // A failed one starts open, so you see why it broke.
        assert_eq!(step_open_by_default(&tool(ToolStatus::Failed)), Some(true));
        // Helper cards fold; what you ran yourself keeps its output, and
        // the generative-UI cards stay as their own live views.
        assert_eq!(
            step_open_by_default(&Item::Custom {
                kind: "gentle-agents.result".into(),
                text: "done".into(),
                details: json!({}),
            }),
            Some(false)
        );
        assert_eq!(
            step_open_by_default(&Item::Tool {
                call_id: "g".into(),
                name: crate::genui::TOOL.into(),
                args: json!({}),
                status: ToolStatus::Done,
                output: String::new(),
                details: json!({}),
                duration_ms: None,
            }),
            None
        );
        assert_eq!(
            step_open_by_default(&Item::Custom {
                kind: crate::genui::EVENT_MESSAGE.into(),
                text: String::new(),
                details: json!({}),
            }),
            None
        );
        assert_eq!(
            step_open_by_default(&Item::Shell {
                command: "ls".into(),
                output: String::new(),
                exit_code: Some(0),
                running: false,
            }),
            None
        );
        assert_eq!(
            step_open_by_default(&Item::Notice {
                level: NoticeLevel::Info,
                text: "hi".into(),
            }),
            None
        );
        assert_eq!(
            step_open_by_default(&Item::User {
                text: "hi".into(),
                images: vec![],
            }),
            None
        );
    }

    fn apply_all(t: &mut Transcript, events: &[Value]) -> Vec<Effects> {
        events.iter().map(|e| t.apply(e)).collect()
    }

    // ── Step groups (S11c) ─────────────────────────────────────────────────

    fn step(name: &str, status: ToolStatus) -> Item {
        Item::Tool {
            call_id: format!("c-{name}"),
            name: name.into(),
            args: json!({}),
            status,
            output: String::new(),
            details: json!({}),
            duration_ms: None,
        }
    }

    fn helper() -> Item {
        Item::Custom {
            kind: "gentle-jobs.notice".into(),
            text: "done".into(),
            details: json!({}),
        }
    }

    fn text_item(text: &str) -> Item {
        Item::Assistant {
            blocks: vec![Block::Text(text.into())],
            streaming: false,
            error: None,
        }
    }

    #[test]
    fn consecutive_finished_steps_fold_into_one_group() {
        let items = vec![
            text_item("On it"),
            step("bash", ToolStatus::Done),
            step("read", ToolStatus::Done),
            step("edit", ToolStatus::Done),
            text_item("Done"),
        ];
        assert_eq!(
            step_groups(&items),
            vec![StepGroup {
                items: 1..4,
                label: "Ran 1 command, read 1 file, edited 1 file".into(),
            }]
        );
    }

    #[test]
    fn a_run_becomes_a_group_as_its_steps_finish() {
        let mut t = Transcript::default();
        apply_all(
            &mut t,
            &[
                json!({"type":"message_start","message":{"role":"user","content":"go"}}),
                json!({"type":"message_start","message":{"role":"assistant","content":[]}}),
                json!({"type":"message_end","message":{"role":"assistant","stopReason":"toolUse","content":[]}}),
                json!({"type":"tool_execution_start","toolCallId":"a","toolName":"bash","args":{}}),
                json!({"type":"tool_execution_start","toolCallId":"b","toolName":"bash","args":{}}),
                json!({"type":"tool_execution_start","toolCallId":"c","toolName":"bash","args":{}}),
            ],
        );
        assert!(step_groups(&t.items).is_empty(), "nothing finished yet");
        t.apply(&json!({"type":"tool_execution_end","toolCallId":"a","result":{}}));
        // One finished step alone is not a group.
        assert!(step_groups(&t.items).is_empty());
        t.apply(&json!({"type":"tool_execution_end","toolCallId":"b","result":{}}));
        let groups = step_groups(&t.items);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].items, 2..4);
        assert_eq!(groups[0].label, "Ran 2 commands");
    }

    #[test]
    fn a_running_step_splits_off_and_stands_alone() {
        let mut t = Transcript::default();
        apply_all(
            &mut t,
            &[
                json!({"type":"message_start","message":{"role":"user","content":"go"}}),
                json!({"type":"message_start","message":{"role":"assistant","content":[]}}),
                json!({"type":"message_end","message":{"role":"assistant","stopReason":"toolUse","content":[]}}),
                json!({"type":"tool_execution_start","toolCallId":"a","toolName":"bash","args":{}}),
                json!({"type":"tool_execution_start","toolCallId":"b","toolName":"read","args":{}}),
                json!({"type":"tool_execution_start","toolCallId":"c","toolName":"bash","args":{}}),
                json!({"type":"tool_execution_end","toolCallId":"a","result":{}}),
                json!({"type":"tool_execution_end","toolCallId":"b","result":{}}),
            ],
        );
        // The finished steps before it stay grouped; the running one is not in it.
        let groups = step_groups(&t.items);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].items, 2..4);
        assert_eq!(groups[0].label, "Ran 1 command, read 1 file");
        // When it finishes, it joins.
        t.apply(&json!({"type":"tool_execution_end","toolCallId":"c","result":{}}));
        assert_eq!(step_groups(&t.items)[0].items, 2..5);
    }

    #[test]
    fn a_failed_step_stays_in_its_group_and_is_counted() {
        let items = vec![
            text_item("On it"),
            step("bash", ToolStatus::Done),
            step("bash", ToolStatus::Failed),
            text_item("Sorry"),
        ];
        assert_eq!(
            step_groups(&items),
            vec![StepGroup {
                items: 1..3,
                label: "Ran 2 commands (1 error)".into(),
            }]
        );
    }

    #[test]
    fn anything_but_finished_steps_breaks_a_run() {
        let shell = Item::Shell {
            command: "ls".into(),
            output: String::new(),
            exit_code: None,
            running: false,
        };
        let thinking = Item::Assistant {
            blocks: vec![Block::Thinking("hmm".into())],
            streaming: false,
            error: None,
        };
        let genui = Item::Tool {
            call_id: "g".into(),
            name: crate::genui::TOOL.into(),
            args: json!({}),
            status: ToolStatus::Done,
            output: String::new(),
            details: json!({}),
            duration_ms: None,
        };
        let notice = Item::Notice {
            level: NoticeLevel::Info,
            text: "n".into(),
        };
        // What you ran yourself, thinking and notices are not steps.
        for before in [&shell, &thinking, &notice] {
            let items = vec![
                text_item("A"),
                before.clone(),
                step("bash", ToolStatus::Done),
            ];
            assert!(step_groups(&items).is_empty());
        }
        // A `!command` between runs keeps both runs single.
        let items = vec![
            step("bash", ToolStatus::Done),
            shell.clone(),
            step("bash", ToolStatus::Done),
            step("read", ToolStatus::Done),
        ];
        assert_eq!(step_groups(&items)[0].items, 2..4);
        // The generative-UI tool is a live card, not a step.
        let items = vec![
            genui,
            step("bash", ToolStatus::Done),
            step("read", ToolStatus::Done),
        ];
        assert_eq!(step_groups(&items)[0].items, 1..3);
        // A running step starts its own run, alone until it finishes.
        let items = vec![
            step("bash", ToolStatus::Running),
            step("read", ToolStatus::Done),
            step("read", ToolStatus::Done),
        ];
        assert_eq!(step_groups(&items)[0].items, 1..3);
    }

    #[test]
    fn single_finished_steps_and_empty_runs_do_not_group() {
        assert!(step_groups(&[]).is_empty());
        assert!(step_groups(&[text_item("hi")]).is_empty());
        assert!(step_groups(&[text_item("hi"), step("bash", ToolStatus::Done)]).is_empty());
        assert!(
            step_groups(&[
                step("bash", ToolStatus::Done),
                text_item("ok"),
                step("read", ToolStatus::Done)
            ])
            .is_empty()
        );
    }

    /// A message that only announced tool calls (or whitespace text) is
    /// invisible: a run of steps flows across it. Thinking with content and
    /// a live message still break the run, as today.
    #[test]
    fn a_textless_assistant_message_does_not_break_a_run() {
        let tool_only = Item::Assistant {
            blocks: vec![Block::ToolCall],
            streaming: false,
            error: None,
        };
        let whitespace = Item::Assistant {
            blocks: vec![Block::Text("  \n".into())],
            streaming: false,
            error: None,
        };
        for textless in [&tool_only, &whitespace] {
            let items = vec![
                text_item("A"),
                step("bash", ToolStatus::Done),
                textless.clone(),
                step("read", ToolStatus::Done),
                step("bash", ToolStatus::Done),
                text_item("B"),
            ];
            assert_eq!(step_groups(&items)[0].items, 1..5);
            assert_eq!(step_groups(&items)[0].label, "Ran 2 commands, read 1 file");
        }
        // While it streams the message is visible, so it breaks the run.
        let live = Item::Assistant {
            blocks: vec![],
            streaming: true,
            error: None,
        };
        let items = vec![
            step("bash", ToolStatus::Done),
            live,
            step("read", ToolStatus::Done),
            step("edit", ToolStatus::Done),
        ];
        assert_eq!(step_groups(&items)[0].items, 2..4);
        // Thinking with content breaks the run, as today.
        let thinking = Item::Assistant {
            blocks: vec![Block::Thinking("hmm".into())],
            streaming: false,
            error: None,
        };
        let items = vec![
            step("bash", ToolStatus::Done),
            thinking,
            step("read", ToolStatus::Done),
        ];
        assert!(step_groups(&items).is_empty());
    }

    #[test]
    fn helpers_group_as_background_tasks() {
        let groups = step_groups(&[
            text_item("x"),
            helper(),
            helper(),
            step("grep", ToolStatus::Done),
            text_item("y"),
        ]);
        assert_eq!(groups[0].label, "Finished 2 background tasks, used 1 tool");
    }

    #[test]
    fn group_labels_pluralize_and_count_errors() {
        let done = step("bash", ToolStatus::Done);
        let failed = step("bash", ToolStatus::Failed);
        let edit = step("edit", ToolStatus::Done);
        let grep = step("grep", ToolStatus::Done);
        let three: Vec<&Item> = (0..3).map(|_| &done).collect();
        assert_eq!(step_group_label(&[]), "");
        assert_eq!(step_group_label(&[&done]), "Ran 1 command");
        assert_eq!(step_group_label(&three), "Ran 3 commands");
        assert_eq!(step_group_label(&[&failed]), "Ran 1 command (1 error)");
        assert_eq!(
            step_group_label(&[&done, &failed]),
            "Ran 2 commands (1 error)"
        );
        assert_eq!(
            step_group_label(&[&failed, &failed]),
            "Ran 2 commands (2 errors)"
        );
        assert_eq!(
            step_group_label(&[&done, &failed, &edit]),
            "Ran 2 commands, edited 1 file, 1 with error"
        );
        assert_eq!(
            step_group_label(&[&failed, &failed, &edit]),
            "Ran 2 commands, edited 1 file, 2 with errors"
        );
        // The order of mention is each kind's first appearance.
        assert_eq!(
            step_group_label(&[&edit, &done]),
            "Edited 1 file, ran 1 command"
        );
        assert_eq!(step_group_label(&[&grep, &grep]), "Used 2 tools");
    }

    #[test]
    fn streaming_text_is_replaced_by_the_final_message() {
        let mut t = Transcript::default();
        apply_all(
            &mut t,
            &[
                json!({"type":"agent_start"}),
                json!({"type":"message_start","message":{"role":"user","content":"hi"}}),
                json!({"type":"message_start","message":{"role":"assistant","content":[]}}),
                json!({"type":"message_update","assistantMessageEvent":{"type":"thinking_start","contentIndex":0}}),
                json!({"type":"message_update","assistantMessageEvent":{"type":"thinking_delta","contentIndex":0,"delta":"hmm"}}),
                json!({"type":"message_update","assistantMessageEvent":{"type":"text_start","contentIndex":1}}),
                json!({"type":"message_update","usage":{"input":10,"output":2,"cost":{"total":0.5}},"assistantMessageEvent":{"type":"text_delta","contentIndex":1,"delta":"Hel"}}),
            ],
        );
        let Item::Assistant {
            blocks, streaming, ..
        } = &t.items[1]
        else {
            panic!()
        };
        assert!(*streaming);
        assert_eq!(
            blocks,
            &[Block::Thinking("hmm".into()), Block::Text("Hel".into())]
        );
        assert_eq!(t.last_usage.unwrap().input, 10);

        let effects = apply_all(
            &mut t,
            &[
                json!({"type":"message_end","message":{"role":"assistant","stopReason":"stop","content":[{"type":"thinking","thinking":"hmm."},{"type":"text","text":"Hello"}]}}),
                json!({"type":"agent_settled","aborted":false}),
            ],
        );
        let Item::Assistant {
            blocks,
            streaming,
            error,
        } = &t.items[1]
        else {
            panic!()
        };
        assert!(!*streaming && error.is_none());
        assert_eq!(
            blocks,
            &[Block::Thinking("hmm.".into()), Block::Text("Hello".into())]
        );
        assert!(effects[1].settled && !t.running);
    }

    #[test]
    fn tool_lifecycle_correlates_by_call_id() {
        let mut t = Transcript::default();
        apply_all(
            &mut t,
            &[
                json!({"type":"tool_execution_start","toolCallId":"a","toolName":"bash","args":{"command":"ls"}}),
                json!({"type":"tool_execution_start","toolCallId":"b","toolName":"read","args":{"path":"x"}}),
                json!({"type":"tool_execution_update","toolCallId":"a","partialResult":{"content":[{"type":"text","text":"par"}]}}),
            ],
        );
        let effects = t.apply(&json!({"type":"tool_execution_end","toolCallId":"a","toolName":"bash","isError":true,"durationMs":12,"result":{"content":[{"type":"text","text":"boom"}]}}));
        assert!(effects.files_touched);
        let Item::Tool {
            status,
            output,
            duration_ms,
            ..
        } = &t.items[0]
        else {
            panic!()
        };
        assert_eq!(
            (*status, output.as_str(), *duration_ms),
            (ToolStatus::Failed, "boom", Some(12))
        );
        let Item::Tool { status, .. } = &t.items[1] else {
            panic!()
        };
        assert_eq!(*status, ToolStatus::Running);
    }

    #[test]
    fn a_waiting_gentle_ui_call_shows_its_partial_details() {
        let mut t = Transcript::default();
        let surface = json!({"id": "r", "version": 1, "kind": "review", "title": "Review", "spec": {"files": []}});
        apply_all(
            &mut t,
            &[
                json!({"type":"tool_execution_start","toolCallId":"a","toolName":"gentle_ui","args":{"action":"show"}}),
                json!({"type":"tool_execution_start","toolCallId":"b","toolName":"bash","args":{"command":"ls"}}),
                json!({"type":"tool_execution_update","toolCallId":"a","partialResult":{"content":[{"type":"text","text":"waiting"}],"details":surface}}),
                json!({"type":"tool_execution_update","toolCallId":"b","partialResult":{"content":[],"details":{"x":1}}}),
            ],
        );
        let Item::Tool {
            status, details, ..
        } = &t.items[0]
        else {
            panic!()
        };
        assert_eq!((*status, details), (ToolStatus::Running, &surface));
        // Other tools keep their details for the end.
        let Item::Tool { details, .. } = &t.items[1] else {
            panic!()
        };
        assert!(details.is_null());
    }

    #[test]
    fn empty_streaming_usage_keeps_the_previous_response() {
        let mut t = Transcript::default();
        // The previous response.
        t.apply(&json!({"type":"message_start","message":{"role":"assistant","content":[]}}));
        t.apply(&json!({"type":"message_end","message":{"role":"assistant","stopReason":"stop","content":[],"usage":{"input":5,"output":40}}}));

        t.apply(&json!({"type":"message_start","message":{"role":"assistant","content":[]}}));
        t.apply(&json!({"type":"message_update","usage":{"input":0,"output":0},"assistantMessageEvent":{"type":"text_start","contentIndex":0}}));
        assert_eq!(t.last_usage.map(|u| u.output), Some(40));

        t.apply(&json!({"type":"message_end","message":{"role":"assistant","stopReason":"stop","content":[],"usage":{"input":7,"output":12}}}));
        assert_eq!(t.last_usage.map(|u| (u.input, u.output)), Some((7, 12)));
    }

    #[test]
    fn provider_errors_surface_on_the_message() {
        let mut t = Transcript::default();
        t.apply(&json!({"type":"message_start","message":{"role":"assistant","content":[]}}));
        t.apply(&json!({"type":"message_end","message":{"role":"assistant","stopReason":"error","errorMessage":"No API key","content":[]}}));
        let Item::Assistant { error, .. } = &t.items[0] else {
            panic!()
        };
        assert_eq!(error.as_deref(), Some("No API key"));
    }

    #[test]
    fn history_pairs_tool_calls_with_results() {
        let mut t = Transcript::default();
        t.load_messages(&[
            json!({"role":"user","content":[{"type":"text","text":"go"},{"type":"image","data":"AAAA","mimeType":"image/png"}]}),
            json!({"role":"assistant","stopReason":"toolUse","content":[{"type":"text","text":"ok"},{"type":"toolCall","id":"c1","name":"edit","arguments":{"path":"a.rs"}}]}),
            json!({"role":"toolResult","toolCallId":"c1","toolName":"edit","isError":false,"content":[{"type":"text","text":"done"}],"details":{"diff":"+x"}}),
        ]);
        assert!(matches!(
            &t.items[0],
            Item::User { text, images } if text == "go"
                && images.len() == 1
                && images[0].mime_type == "image/png"
                && &*images[0].data == "AAAA"
                && images[0].key == image_key("AAAA")
        ));
        assert!(matches!(
            &t.items[1],
            Item::Assistant {
                streaming: false,
                ..
            }
        ));
        let Item::Tool {
            status,
            output,
            details,
            ..
        } = &t.items[2]
        else {
            panic!()
        };
        assert_eq!((*status, output.as_str()), (ToolStatus::Done, "done"));
        assert_eq!(details["diff"], "+x");
    }

    #[test]
    fn visible_custom_messages_become_items() {
        let mut t = Transcript::default();
        t.apply(&json!({"type":"message_end","message":{"role":"custom","customType":"gentle-agents.result","content":"Explorer found 3 files","display":true,"details":{"gentleAgents":{"agent":"explorer","status":"completed"}}}}));
        t.apply(&json!({"type":"message_end","message":{"role":"custom","customType":"gentle-todo-state","content":"hidden","display":false}}));
        assert_eq!(t.items.len(), 1);
        assert!(matches!(&t.items[0], Item::Custom { kind, details, .. }
            if kind == "gentle-agents.result" && details["gentleAgents"]["agent"] == "explorer"));
    }

    #[test]
    fn token_formatting() {
        assert_eq!(format_tokens(999), "999");
        assert_eq!(format_tokens(200_000), "200k");
        assert_eq!(format_tokens(1_500), "1.5k");
        assert_eq!(format_tokens(1_000_000), "1M");
    }

    #[test]
    fn dirty_from_tracks_the_first_changed_item() {
        let mut t = Transcript::default();
        assert_eq!(
            t.take_dirty(),
            Some(Dirty {
                from: 0,
                reset: true
            })
        );
        assert_eq!(t.take_dirty(), None);

        apply_all(
            &mut t,
            &[
                json!({"type":"message_start","message":{"role":"user","content":"hi"}}),
                json!({"type":"tool_execution_start","toolCallId":"a","toolName":"bash","args":{}}),
                json!({"type":"message_start","message":{"role":"assistant","content":[]}}),
            ],
        );
        let first = Dirty {
            from: 0,
            reset: false,
        };
        assert_eq!(t.take_dirty(), Some(first));

        // Streaming only touches the streamed message.
        t.apply(&json!({"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"x"}}));
        assert_eq!(t.take_dirty().map(|d| d.from), Some(2));

        // A late tool update lowers the mark to the tool; later items stay included.
        t.apply(&json!({"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"y"}}));
        t.apply(&json!({"type":"tool_execution_update","toolCallId":"a","partialResult":{"content":[{"type":"text","text":"out"}]}}));
        assert_eq!(t.take_dirty().map(|d| d.from), Some(1));

        // Unknown events and updates for missing tools change nothing.
        t.apply(&json!({"type":"queue_update"}));
        t.apply(&json!({"type":"tool_execution_update","toolCallId":"zz","partialResult":{}}));
        assert_eq!(t.take_dirty(), None);

        t.notice(NoticeLevel::Info, "n".into());
        assert_eq!(t.take_dirty().map(|d| d.from), Some(3));

        t.load_messages(&[json!({"role":"user","content":"again"})]);
        assert_eq!(
            t.take_dirty(),
            Some(Dirty {
                from: 0,
                reset: true
            })
        );
    }

    #[test]
    fn running_counters_follow_tools_and_shells() {
        let mut t = Transcript::default();
        t.apply(
            &json!({"type":"tool_execution_start","toolCallId":"a","toolName":"bash","args":{}}),
        );
        t.apply(
            &json!({"type":"tool_execution_start","toolCallId":"b","toolName":"read","args":{}}),
        );
        assert_eq!(t.running_tools(), 2);
        t.apply(
            &json!({"type":"tool_execution_end","toolCallId":"a","toolName":"bash","result":{}}),
        );
        t.apply(
            &json!({"type":"tool_execution_end","toolCallId":"a","toolName":"bash","result":{}}),
        );
        assert_eq!(t.running_tools(), 1);

        let shell = t.start_shell("ls".into());
        assert!(t.shells_running());
        t.finish_shell(shell, Some(0), None);
        t.finish_shell(shell, Some(0), None);
        assert!(!t.shells_running());
    }

    #[test]
    fn interrupt_settles_stream_tools_and_shells() {
        let mut t = Transcript::default();
        apply_all(
            &mut t,
            &[
                json!({"type":"agent_start"}),
                json!({"type":"tool_execution_start","toolCallId":"a","toolName":"bash","args":{}}),
                json!({"type":"message_start","message":{"role":"assistant","content":[]}}),
            ],
        );
        let shell = t.start_shell("ls".into());
        t.take_dirty();
        t.interrupt();
        assert!(!t.running);
        assert_eq!(t.streaming_item(), None);
        assert_eq!(t.running_tools(), 0);
        assert!(!t.shells_running());
        assert!(matches!(
            t.items[0],
            Item::Tool {
                status: ToolStatus::Failed,
                ..
            }
        ));
        assert!(matches!(t.items[shell], Item::Shell { running: false, .. }));
        assert_eq!(t.take_dirty().map(|d| d.from), Some(0));
    }

    #[test]
    fn settled_turn_fails_tools_left_running() {
        let mut t = Transcript::default();
        t.apply(
            &json!({"type":"tool_execution_start","toolCallId":"a","toolName":"bash","args":{}}),
        );
        t.apply(&json!({"type":"agent_settled","aborted":true}));
        assert_eq!(t.running_tools(), 0);
        assert!(matches!(
            t.items[0],
            Item::Tool {
                status: ToolStatus::Failed,
                ..
            }
        ));
    }

    #[test]
    fn shell_output_keeps_a_bounded_tail_on_a_char_boundary() {
        let mut t = Transcript::default();
        let shell = t.start_shell("yes".into());
        let chunk = "é".repeat(1000) + "x";
        for _ in 0..1000 {
            t.shell_output(shell, &chunk);
        }
        t.shell_output(shell, "end");
        let Item::Shell { output, .. } = &t.items[shell] else {
            panic!()
        };
        assert!(output.len() <= 2 * SHELL_OUTPUT_CAP + SHELL_TRIMMED.len());
        assert!(output.starts_with(SHELL_TRIMMED));
        assert!(output.ends_with("xend"));
    }

    #[test]
    fn unchanged_partial_update_is_not_dirty() {
        let mut t = Transcript::default();
        let update = json!({"type":"tool_execution_update","toolCallId":"a","partialResult":{"content":[{"type":"text","text":"out"}]}});
        t.apply(
            &json!({"type":"tool_execution_start","toolCallId":"a","toolName":"bash","args":{}}),
        );
        t.apply(&update);
        t.take_dirty();
        t.apply(&update);
        assert_eq!(t.take_dirty(), None);
    }
}
