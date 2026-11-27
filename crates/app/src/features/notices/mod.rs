//! The window's notification center: extensions' informational `notify`
//! messages (plus a record of warnings and errors, which also toast) collected
//! from every tab behind a bell in the title bar instead of toasts.

use std::collections::VecDeque;
use std::rc::Rc;
use std::time::Instant;

use gpui_kit::{App, SharedString, Window};

use gentle_core::transcript::NoticeLevel;

mod bell;
mod panel;
mod store;

pub use bell::bell;

/// Most notices kept; the oldest drop first.
pub const CAP: usize = 200;

pub struct Notice {
    pub id: u64,
    pub level: NoticeLevel,
    pub message: String,
    /// The tab it came from, as titled when it arrived.
    pub source: SharedString,
    /// The session it came from, to jump back to it.
    pub origin: Option<Origin>,
    /// When it last arrived.
    pub at: Instant,
    pub read: bool,
    /// Further arrivals of the same message from the same session folded
    /// into this one.
    pub repeats: u32,
}

/// The session a notice came from: its pane, and the conversation that pane
/// showed then (a pane outlives its conversations: ⌘N, a resumed session).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Origin {
    pub pane: usize,
    /// The pane's `SessionView::conversation` when the notice arrived.
    pub conversation: u64,
}

/// Whether a notice's session can still be shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reach {
    Open,
    /// The pane now shows another conversation.
    SessionGone,
    PaneClosed,
}

impl Origin {
    /// Where the origin stands, given the conversation its pane shows now
    /// (`None`: the pane was closed).
    pub fn reach(self, shown: Option<u64>) -> Reach {
        match shown {
            None => Reach::PaneClosed,
            Some(conversation) if conversation != self.conversation => Reach::SessionGone,
            Some(_) => Reach::Open,
        }
    }
}

/// Notices, newest first.
#[derive(Default)]
pub struct Notices {
    items: VecDeque<Notice>,
    next_id: u64,
}

/// Jumps to the pane a notice came from.
pub type OnSelect = Rc<dyn Fn(usize, &mut Window, &mut App)>;

/// Where a notice's origin stands now (see [`Origin::reach`]).
pub type ReachOf = Rc<dyn Fn(&Origin, &App) -> Reach>;
