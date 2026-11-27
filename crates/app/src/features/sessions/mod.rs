//! Sessions: browse and resume previous conversations, like Pi's `/resume`.
//!
//! When the window's active agent tab runs a Gentle Shell that advertises
//! `list_sessions` and `switch_session` (and its folder is the one browsed),
//! that Shell lists the sessions (newest first, a page at a time) and opens
//! one by id in the same tab, so the app reads no session file. Otherwise —
//! no agent tab, an older Shell, a tab in another folder — the app reads the
//! files itself:
//!
//! Pi stores one JSONL file per session under `<agent>/sessions/--<cwd>--/`
//! (`<timestamp>_<id>.jsonl`), unless `settings.json` sets `sessionDir`, in
//! which case every session lives flat in that directory and is matched to a
//! project by the header's `cwd`. We read the files mirroring
//! `SessionManager.list`/`listAll`, and resume with Pi's own `switch_session
//! {sessionPath}`.
//!
//! Either way a session from another project opens in a new tab rooted at its
//! folder; Gentle Shell's listing names projects, not folders, so that one
//! file is found by its id.

use std::io::Read as _;
use std::path::{Path, PathBuf};

use gentle_core::sessions::{MAX_SCAN_BYTES, SessionSummary};
use gpui_kit::component::input::InputState;
use gpui_kit::*;
use pi_rpc::desktop_rpc::{SessionEntry, SessionsScope};

use crate::features::session::{ReadTicket, SessionView};

mod discovery;
mod render;
mod summary;
#[cfg(test)]
mod tests;
mod view;

/// The conversation the session file at `path` resumes to, read from the
/// file alone ([`gentle_core::sessions::branch`]), for a restored tab to
/// show while its backend starts. `None` for a missing or unreadable file,
/// one that isn't a current session, and one past [`MAX_SCAN_BYTES`] (the
/// backend's own answer shows that one).
pub fn resumed_messages(path: &Path) -> Option<Vec<serde_json::Value>> {
    let file = std::fs::File::open(path).ok()?;
    if file.metadata().ok()?.len() > MAX_SCAN_BYTES {
        return None;
    }
    let mut bytes = Vec::new();
    // Still bounded if the file grows while it is read.
    file.take(MAX_SCAN_BYTES + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > MAX_SCAN_BYTES {
        return None;
    }
    gentle_core::sessions::branch::messages(&String::from_utf8_lossy(&bytes))
}

pub enum SessionsEvent {
    /// Resume `path`. When `cwd` is the tab's cwd send RPC
    /// `switch_session {sessionPath: path}`; otherwise the session belongs to
    /// another project and should open in a new tab rooted at `cwd`.
    Open {
        path: PathBuf,
        cwd: PathBuf,
    },
    /// Start a fresh conversation in this tab (RPC `new_session`).
    NewSession,
    Close,
}

/// Which sessions to list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// Sessions recorded for the tab's cwd (`--continue` / `/resume`).
    Project,
    /// Every project (`/resume` "all").
    All,
}

impl Scope {
    fn wire(self) -> SessionsScope {
        match self {
            Scope::Project => SessionsScope::Project,
            Scope::All => SessionsScope::All,
        }
    }
}

/// Who lists and opens the sessions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    /// The agent tab is still probing its Shell: wait, bounded, then decide.
    Undecided,
    /// The agent tab's Gentle Shell (`list_sessions`, `switch_session`).
    Shell,
    /// The session files, read by the app.
    Files,
}

/// The read waiting for Gentle Shell's answer.
enum Pending {
    /// A page of `scope`; `more` appends it to the pages already shown.
    List {
        ticket: ReadTicket,
        scope: Scope,
        more: bool,
    },
    /// A switch to `entry`, resolved in `scope` (`None`: the project).
    Switch {
        ticket: ReadTicket,
        entry: SessionEntry,
        scope: Option<SessionsScope>,
    },
}

pub struct SessionsView {
    cwd: PathBuf,
    current: Option<PathBuf>,
    scope: Scope,
    /// The window's active agent tab: its Shell lists and opens sessions
    /// when it offers to.
    session: Option<Entity<SessionView>>,
    source: Source,
    /// The files' summaries (`Source::Files`).
    sessions: Vec<SessionSummary>,
    /// The listed files that are `current` under any spelling, resolved
    /// when the list loads so drawing a row never touches the disk.
    current_rows: Vec<PathBuf>,
    /// Gentle Shell's pages so far (`Source::Shell`).
    entries: Vec<SessionEntry>,
    /// Where Gentle Shell's next page starts, when there is one.
    next_cursor: Option<String>,
    pending: Option<Pending>,
    /// Why the last open did not happen.
    error: Option<String>,
    loading: bool,
    filter: Entity<InputState>,
    _load: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<SessionsEvent> for SessionsView {}
