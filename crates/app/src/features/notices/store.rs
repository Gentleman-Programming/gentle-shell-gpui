//! State transitions of the notice list: record (deduping repeats), read
//! marks, clear and iteration.

use std::time::Instant;

use gentle_core::transcript::NoticeLevel;
use gpui_kit::SharedString;

use super::{CAP, Notice, Notices, Origin};

impl Notices {
    /// Record a notice. A message already listed at the same level from the
    /// same session is moved to the top and counted instead of listed twice;
    /// another session's keeps its own entry, so each opens its own session.
    /// A repeat keeps its read state, and a new entry for a message already
    /// read from another session starts read, so startup chatter repeated by
    /// every new tab does not light the bell again.
    pub fn push(
        &mut self,
        level: NoticeLevel,
        message: String,
        source: SharedString,
        origin: Option<Origin>,
        at: Instant,
    ) {
        let same = |n: &Notice| n.level == level && n.message == message;
        let notice = match self
            .items
            .iter()
            .position(|n| same(n) && n.origin == origin)
            .and_then(|ix| self.items.remove(ix))
        {
            Some(mut old) => {
                old.repeats += 1;
                old.at = at;
                old.source = source;
                old
            }
            None => {
                self.next_id += 1;
                Notice {
                    id: self.next_id,
                    read: self.items.iter().any(|n| same(n) && n.read),
                    level,
                    message,
                    source,
                    origin,
                    at,
                    repeats: 0,
                }
            }
        };
        self.items.push_front(notice);
        self.items.truncate(CAP);
    }

    pub fn unread(&self) -> usize {
        self.items.iter().filter(|n| !n.read).count()
    }

    pub fn mark_read(&mut self, id: u64) {
        if let Some(notice) = self.items.iter_mut().find(|n| n.id == id) {
            notice.read = true;
        }
    }

    pub fn mark_all_read(&mut self) {
        for notice in &mut self.items {
            notice.read = true;
        }
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }

    pub fn iter(&self) -> impl Iterator<Item = &Notice> {
        self.items.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use crate::features::notices::{CAP, Notices, Origin};
    use gentle_core::transcript::NoticeLevel;

    fn origin(pane: usize, conversation: u64) -> Option<Origin> {
        Some(Origin { pane, conversation })
    }

    fn push_from(store: &mut Notices, message: &str, from: Option<Origin>, at: Instant) {
        store.push(
            NoticeLevel::Info,
            message.to_string(),
            "tab".into(),
            from,
            at,
        );
    }

    fn push(store: &mut Notices, message: &str, at: Instant) {
        push_from(store, message, origin(1, 0), at);
    }

    #[test]
    fn dedupes_repeats_to_the_top() {
        let mut store = Notices::default();
        let t0 = Instant::now();
        push(&mut store, "a", t0);
        push(&mut store, "b", t0);
        push(&mut store, "a", t0 + Duration::from_secs(5));
        assert_eq!(store.iter().count(), 2);
        let first = store.iter().next().unwrap();
        assert_eq!(first.message, "a");
        assert_eq!(first.repeats, 1);
        assert_eq!(first.at, t0 + Duration::from_secs(5));
        // Same text at another level is a different notice.
        store.push(NoticeLevel::Error, "a".into(), "tab".into(), None, t0);
        assert_eq!(store.iter().count(), 3);
    }

    #[test]
    fn counts_unread() {
        let mut store = Notices::default();
        let t0 = Instant::now();
        push(&mut store, "a", t0);
        push(&mut store, "b", t0);
        assert_eq!(store.unread(), 2);
        let id = store.iter().next().unwrap().id;
        store.mark_read(id);
        assert_eq!(store.unread(), 1);
        store.mark_all_read();
        assert_eq!(store.unread(), 0);
        // A repeat of a read notice stays read.
        push(&mut store, "a", t0);
        assert_eq!(store.unread(), 0);
        push(&mut store, "c", t0);
        assert_eq!(store.unread(), 1);
        store.clear();
        assert!(store.is_empty());
        assert_eq!(store.unread(), 0);
    }

    #[test]
    fn caps_at_200_dropping_oldest() {
        let mut store = Notices::default();
        let t0 = Instant::now();
        for i in 0..CAP + 25 {
            push(&mut store, &format!("n{i}"), t0);
        }
        assert_eq!(CAP, 200);
        assert_eq!(store.iter().count(), CAP);
        assert_eq!(
            store.iter().next().unwrap().message,
            format!("n{}", CAP + 24)
        );
        assert_eq!(store.iter().last().unwrap().message, "n25");
        assert_eq!(store.unread(), CAP);
    }

    #[test]
    fn equal_messages_from_two_sessions_stay_apart() {
        let mut store = Notices::default();
        let t0 = Instant::now();
        push_from(&mut store, "done", origin(1, 0), t0);
        push_from(&mut store, "done", origin(2, 0), t0);
        // Same pane, after ⌘N: another session too.
        push_from(&mut store, "done", origin(1, 1), t0);
        let origins: Vec<_> = store.iter().map(|n| n.origin).collect();
        assert_eq!(origins, [origin(1, 1), origin(2, 0), origin(1, 0)]);
        // A repeat from the same session still folds, into its own entry.
        push_from(&mut store, "done", origin(2, 0), t0);
        assert_eq!(store.iter().count(), 3);
        let first = store.iter().next().unwrap();
        assert_eq!((first.origin, first.repeats), (origin(2, 0), 1));
        let id = first.id;

        // Opening one entry leaves the others unread.
        store.mark_read(id);
        assert_eq!(store.unread(), 2);
    }

    #[test]
    fn a_message_read_elsewhere_arrives_read() {
        let mut store = Notices::default();
        let t0 = Instant::now();
        push_from(&mut store, "started", origin(1, 0), t0);
        store.mark_all_read();
        push_from(&mut store, "started", origin(2, 0), t0);
        assert_eq!(store.iter().count(), 2);
        assert_eq!(store.unread(), 0);
        // Unread elsewhere: the new entry is unread too.
        push_from(&mut store, "failed", origin(1, 0), t0);
        push_from(&mut store, "failed", origin(2, 0), t0);
        assert_eq!(store.unread(), 2);
    }
}
