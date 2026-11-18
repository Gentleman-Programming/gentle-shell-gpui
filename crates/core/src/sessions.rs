//! Session browser model: the summary shown for one Pi session file (title,
//! search match, ordering), Pi's per-cwd session directory name, and the
//! timestamp parsing/formatting the summaries use.

use std::path::{Path, PathBuf};

/// Titles are cut to this many characters, like the summaries and the title
/// bar's conversation chip.
pub const TITLE_MAX_CHARS: usize = 120;

/// The title a conversation shows everywhere (the sidebar's rows, the
/// session browser and the title bar's chip): the explicit name, else its
/// first user message; trimmed. `None` while there is neither — callers
/// pick their own untitled fallback (`"New conversation"`,
/// `"(no messages)"`).
pub fn conversation_title<'a>(
    name: Option<&'a str>,
    first_message: Option<&'a str>,
) -> Option<&'a str> {
    [name, first_message]
        .into_iter()
        .flatten()
        .find(|text| !text.trim().is_empty())
        .map(str::trim)
}

/// The title bar's conversation chip label and whether it is the "New
/// conversation" fallback: a session with a title of its own shows it in the
/// main color; an untitled one (no name, no message yet, or a blank one)
/// shows the fallback faint.
pub fn conversation_chip_label(title: Option<&str>) -> (&str, bool) {
    match title.filter(|t| !t.trim().is_empty()) {
        Some(title) => (title, false),
        None => ("New conversation", true),
    }
}

/// What the browser shows for one session file.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionSummary {
    pub path: PathBuf,
    pub id: String,
    /// The header's `cwd`; empty when the header has none.
    pub cwd: PathBuf,
    /// Latest `session_info` name (`/name`, `set_session_name`); a later empty
    /// name clears it, as in Pi.
    pub name: Option<String>,
    /// Text of the first user message.
    pub first_message: Option<String>,
    /// Last user/assistant activity, else header time, else file mtime (ms).
    pub modified_ms: i64,
    /// Number of `message` entries (Pi counts every role).
    pub message_count: usize,
    /// The scan stopped at [`MAX_SCAN_BYTES`]; counts are a lower bound.
    pub truncated: bool,
    /// Model of the last assistant reply or `model_change`, whichever is later.
    pub model: Option<String>,
}

impl SessionSummary {
    pub fn title(&self) -> String {
        crate::text::first_line(
            conversation_title(self.name.as_deref(), self.first_message.as_deref())
                .unwrap_or("(no messages)"),
            TITLE_MAX_CHARS,
        )
    }

    pub fn matches(&self, query_lower: &str) -> bool {
        query_lower.is_empty()
            || [self.name.as_deref(), self.first_message.as_deref()]
                .into_iter()
                .flatten()
                .any(|t| t.to_lowercase().contains(query_lower))
            || self
                .cwd
                .to_string_lossy()
                .to_lowercase()
                .contains(query_lower)
            || self
                .path
                .to_string_lossy()
                .to_lowercase()
                .contains(query_lower)
    }
}

/// Stop reading a session after this many bytes; huge transcripts (inline
/// images, long tool output) would otherwise stall the list.
pub const MAX_SCAN_BYTES: u64 = 16 * 1024 * 1024;

/// Pi's per-cwd directory name: drop the leading separator, map `/ \ :` to `-`.
pub fn session_dir_name(cwd: &Path) -> String {
    let s = cwd.to_string_lossy();
    let trimmed = s.strip_prefix(['/', '\\']).unwrap_or(&s);
    let safe: String = trimmed
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':') {
                '-'
            } else {
                c
            }
        })
        .collect();
    format!("--{safe}--")
}

pub fn sort_newest_first(sessions: &mut [SessionSummary]) {
    sessions.sort_by(|a, b| {
        b.modified_ms
            .cmp(&a.modified_ms)
            .then_with(|| b.path.cmp(&a.path))
    });
}

/// Days since 1970-01-01 for a proleptic Gregorian date.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `YYYY-MM-DDTHH:MM:SS[.fff][Z|±HH:MM]` to Unix milliseconds.
pub fn parse_iso_ms(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    let num = |r: std::ops::Range<usize>| -> Option<i64> { s.get(r)?.parse().ok() };
    if b.len() < 19
        || b[4] != b'-'
        || b[7] != b'-'
        || !matches!(b[10], b'T' | b' ')
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let (y, mo, d, h, mi, sec) = (
        num(0..4)?,
        num(5..7)?,
        num(8..10)?,
        num(11..13)?,
        num(14..16)?,
        num(17..19)?,
    );
    let mut i = 19;
    let mut ms = 0;
    if b.get(i) == Some(&b'.') {
        let start = i + 1;
        i = start;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        let frac = s.get(start..i)?;
        let padded: String = frac.chars().chain("000".chars()).take(3).collect();
        ms = padded.parse::<i64>().ok()?;
    }
    let offset_min = match b.get(i) {
        None | Some(b'Z' | b'z') => 0,
        Some(sign @ (b'+' | b'-')) => {
            let oh = num(i + 1..i + 3)?;
            let om = match b.get(i + 3) {
                Some(b':') => num(i + 4..i + 6)?,
                _ => num(i + 3..i + 5).unwrap_or(0),
            };
            let total = oh * 60 + om;
            if *sign == b'+' { total } else { -total }
        }
        _ => return None,
    };
    let days = days_from_civil(y, mo, d);
    Some(((days * 86_400 + h * 3600 + mi * 60 + sec - offset_min * 60) * 1000) + ms)
}

pub fn relative_time(now_ms: i64, then_ms: i64) -> String {
    let secs = (now_ms - then_ms).max(0) / 1000;
    match secs {
        0..60 => "just now".to_string(),
        60..3600 => format!("{}m ago", secs / 60),
        3600..86_400 => format!("{}h ago", secs / 3600),
        86_400..604_800 => format!("{}d ago", secs / 86_400),
        604_800..2_592_000 => format!("{}w ago", secs / 604_800),
        2_592_000..31_536_000 => format!("{}mo ago", secs / 2_592_000),
        _ => format!("{}y ago", secs / 31_536_000),
    }
}

/// The title of a session from Pi's own listing (Gentle Shell's
/// `list_sessions`): its name, else its first message, on one line.
pub fn listed_title(name: Option<&str>, first_message: &str) -> String {
    crate::text::first_line(name.unwrap_or(first_message).trim(), 120)
}

/// Whether a session from Pi's own listing matches a lowercase query: by
/// name, first message or project name (the listing carries no path).
pub fn listed_matches(
    query_lower: &str,
    name: Option<&str>,
    first_message: &str,
    project: &str,
) -> bool {
    query_lower.is_empty()
        || [name, Some(first_message), Some(project)]
            .into_iter()
            .flatten()
            .any(|t| t.to_lowercase().contains(query_lower))
}

/// How Pi's listing names `cwd`'s project: its last segment, or `unknown`.
pub fn project_name(cwd: &Path) -> String {
    cwd.file_name().map_or_else(
        || "unknown".to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// The session id in a Pi session file name (`<timestamp>_<id>.jsonl`).
pub fn file_session_id(path: &Path) -> Option<&str> {
    let stem = path.file_stem()?.to_str()?;
    let (_, id) = stem.rsplit_once('_')?;
    (!id.is_empty()).then_some(id)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        SessionSummary, conversation_chip_label, conversation_title, file_session_id,
        listed_matches, listed_title, parse_iso_ms, project_name, relative_time, session_dir_name,
    };
    use std::path::PathBuf;

    #[test]
    fn listed_sessions_title_and_match_without_paths() {
        assert_eq!(
            listed_title(Some("Release prep"), "Prepare"),
            "Release prep"
        );
        assert_eq!(listed_title(None, "  Why\nmore"), "Why …");
        let matches = |q: &str| listed_matches(q, Some("Release prep"), "Notes", "gentle-shell");
        assert!(matches(""));
        assert!(matches("release"));
        assert!(matches("notes"));
        assert!(matches("gentle-sh"));
        assert!(!matches("windows"));
        assert!(listed_matches("why", None, "Why does it fail", "x"));
    }

    #[test]
    fn project_names_follow_pi() {
        assert_eq!(project_name(Path::new("/w/gentle-shell")), "gentle-shell");
        assert_eq!(project_name(Path::new("/")), "unknown");
    }

    #[test]
    fn file_names_carry_the_session_id() {
        let path = Path::new("/s/--w--/2026-11-17T10-00-00-000Z_5e21f0aa-3b7c.jsonl");
        assert_eq!(file_session_id(path), Some("5e21f0aa-3b7c"));
        assert_eq!(file_session_id(Path::new("/s/plain.jsonl")), None);
    }

    #[test]
    fn encodes_cwd_like_pi() {
        assert_eq!(
            session_dir_name(Path::new(
                "/Users/ismaelosuna/Documents/Git/personal/gentle-shell-gpui"
            )),
            "--Users-ismaelosuna-Documents-Git-personal-gentle-shell-gpui--"
        );
        assert_eq!(session_dir_name(Path::new("C:\\work\\x")), "--C--work-x--");
    }

    #[test]
    fn parses_iso_timestamps() {
        assert_eq!(
            parse_iso_ms("2024-12-03T14:00:00.000Z"),
            Some(1_733_234_400_000)
        );
        assert_eq!(
            parse_iso_ms("2024-12-03T14:00:00Z"),
            Some(1_733_234_400_000)
        );
        assert_eq!(
            parse_iso_ms("2024-12-03T14:00:00.5Z"),
            Some(1_733_234_400_500)
        );
        assert_eq!(
            parse_iso_ms("2024-12-03T16:00:00+02:00"),
            Some(1_733_234_400_000)
        );
        assert_eq!(parse_iso_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_iso_ms("yesterday"), None);
    }

    #[test]
    fn formats_relative_time() {
        let now = 1_000_000_000_000;
        assert_eq!(relative_time(now, now + 5_000), "just now");
        assert_eq!(relative_time(now, now - 30_000), "just now");
        assert_eq!(relative_time(now, now - 5 * 60_000), "5m ago");
        assert_eq!(relative_time(now, now - 3 * 3_600_000), "3h ago");
        assert_eq!(relative_time(now, now - 2 * 86_400_000), "2d ago");
        assert_eq!(relative_time(now, now - 14 * 86_400_000), "2w ago");
        assert_eq!(relative_time(now, now - 400 * 86_400_000), "1y ago");
    }

    #[test]
    fn conversation_title_prefers_the_name_then_the_first_message() {
        assert_eq!(
            conversation_title(Some("Rebase, push to main"), Some("hi")),
            Some("Rebase, push to main")
        );
        assert_eq!(conversation_title(Some("Deploy"), None), Some("Deploy"));
        assert_eq!(
            conversation_title(None, Some("hola esto es una prueba")),
            Some("hola esto es una prueba")
        );
    }

    #[test]
    fn a_conversation_without_name_or_message_is_untitled() {
        assert_eq!(conversation_title(None, None), None);
        // A blank name was cleared: fall through to the first message.
        assert_eq!(conversation_title(Some(" "), Some("hi")), Some("hi"));
        assert_eq!(conversation_title(Some(""), Some("")), None);
    }

    #[test]
    fn chip_label_shows_the_title_or_the_fallback() {
        assert_eq!(
            conversation_chip_label(Some("Rebase, push to main")),
            ("Rebase, push to main", false)
        );
        assert_eq!(conversation_chip_label(None), ("New conversation", true));
        assert_eq!(
            conversation_chip_label(Some("")),
            ("New conversation", true)
        );
        assert_eq!(
            conversation_chip_label(Some("   ")),
            ("New conversation", true)
        );
    }

    fn summary(name: Option<&str>, first_message: Option<&str>) -> SessionSummary {
        SessionSummary {
            path: PathBuf::new(),
            id: String::new(),
            cwd: PathBuf::new(),
            name: name.map(str::to_string),
            first_message: first_message.map(str::to_string),
            modified_ms: 0,
            message_count: 0,
            truncated: false,
            model: None,
        }
    }

    #[test]
    fn summaries_title_from_name_then_first_message() {
        assert_eq!(summary(Some("work"), None).title(), "work");
        assert_eq!(
            summary(Some("work"), Some("one line\nmore")).title(),
            "work"
        );
        // A blank name falls through to the first message, on one line.
        assert_eq!(
            summary(Some("  "), Some("one line\nmore")).title(),
            "one line …"
        );
        assert_eq!(summary(None, None).title(), "(no messages)");
    }
}
