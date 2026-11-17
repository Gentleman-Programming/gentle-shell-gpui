//! Session file fixtures: summarizing, caching and listing real JSONL files
//! in a temp directory; who lists and opens sessions; and the screen drawn
//! from Gentle Shell's listing in a headless window.

// Not `super::*`: that re-exports `gpui_kit::*`, whose `test` attribute
// shadows the built-in one and recurses.
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use gentle_core::sessions::{SessionSummary, session_dir_name};
use gpui_kit::{
    AppContext as _, Context, Entity, IntoElement, Modifiers, ParentElement as _, Render,
    Styled as _, Subscription, TestAppContext, Window, div,
};
use pi_rpc::desktop_rpc::{DesktopErrorCode, SessionEntry, SessionsScope};
use pi_rpc::file_cache::FileCache;

use super::discovery::{find_session, list_sessions};
use super::summary::{cached_summary_in, summarize};
use super::view::{choose_source, opens_elsewhere, switch_scope};
use super::{Scope, SessionsEvent, SessionsView, Source};
use crate::features::session::{ReadFailure, ReadSupport};

fn read_summary(path: &Path) -> Option<SessionSummary> {
    summarize(std::fs::File::open(path).ok()?, path)
        .ok()
        .flatten()
}

#[test]
fn summaries_are_cached_by_mtime_and_len() {
    use std::time::Duration;
    let tmp = TempDir::new("cache");
    let set_mtime = |path: &Path, secs: u64| {
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(UNIX_EPOCH + Duration::from_secs(secs))
            .unwrap();
    };
    let cache = FileCache::new(8);
    let head = header("abc", "/w", "2024-12-03T14:00:00.000Z");
    let path = write_session(&tmp.0, "a.jsonl", &[head.clone(), user("u1", "aaaa", 1)]);
    set_mtime(&path, 6_000_000);
    let first = cached_summary_in(&cache, &path).unwrap();
    assert_eq!(first.first_message.as_deref(), Some("aaaa"));

    // Same mtime and length: the cached summary, not the new text.
    write_session(&tmp.0, "a.jsonl", &[head.clone(), user("u1", "bbbb", 1)]);
    set_mtime(&path, 6_000_000);
    assert_eq!(cached_summary_in(&cache, &path), Some(first));

    set_mtime(&path, 6_000_001);
    let second = cached_summary_in(&cache, &path).unwrap();
    assert_eq!(second.first_message.as_deref(), Some("bbbb"));
    assert_eq!(Some(second), read_summary(&path));

    // An appended turn changes the length.
    write_session(
        &tmp.0,
        "a.jsonl",
        &[head, user("u1", "bbbb", 1), user("u2", "c", 2)],
    );
    set_mtime(&path, 6_000_001);
    assert_eq!(cached_summary_in(&cache, &path).unwrap().message_count, 2);

    // Non-session files are cached as such.
    let other = write_session(&tmp.0, "b.jsonl", &[user("u1", "hi", 1)]);
    assert_eq!(cached_summary_in(&cache, &other), None);
    assert_eq!(
        cached_summary_in(&cache, &tmp.0.join("missing.jsonl")),
        None
    );
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("gs-sessions-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn header(id: &str, cwd: &str, ts: &str) -> String {
    format!(r#"{{"type":"session","version":3,"id":"{id}","timestamp":"{ts}","cwd":"{cwd}"}}"#)
}

fn user(id: &str, text: &str, ts_ms: i64) -> String {
    format!(
        r#"{{"type":"message","id":"{id}","parentId":null,"timestamp":"2024-12-03T14:00:01.000Z","message":{{"role":"user","content":[{{"type":"text","text":"{text}"}},{{"type":"image","data":"AAAA","mimeType":"image/png"}}],"timestamp":{ts_ms}}}}}"#
    )
}

fn write_session(dir: &Path, file: &str, lines: &[String]) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(file);
    std::fs::write(&path, lines.join("\n") + "\n").unwrap();
    path
}

#[test]
fn parses_synthetic_session() {
    let tmp = TempDir::new("parse");
    let lines = vec![
        header("abc-123", "/work/proj", "2024-12-03T14:00:00.000Z"),
        r#"{"type":"message","id":"s1","parentId":null,"timestamp":"2024-12-03T14:00:00.500Z","message":{"role":"system","content":"","sections":{"preamble":"x"},"timestamp":1733234400500}}"#.to_string(),
        "not json at all".to_string(),
        String::new(),
        // JSON-escaped newline: the stored text is "Fix the login bug\nplease".
        user("u1", "Fix the login bug\\nplease", 1_733_234_401_000),
        r#"{"type":"message","id":"a1","parentId":"u1","timestamp":"2024-12-03T14:00:02.000Z","message":{"role":"assistant","content":[{"type":"toolCall","id":"c","name":"bash","arguments":{"model":"decoy"}}],"api":"anthropic-messages","provider":"anthropic","model":"claude-sonnet-4-5","stopReason":"stop","timestamp":1733234402000}}"#.to_string(),
        r#"{"type":"message","id":"t1","parentId":"a1","timestamp":"2024-12-03T14:00:03.000Z","message":{"role":"toolResult","toolCallId":"c","toolName":"bash","content":[{"type":"text","text":"ok"}],"isError":false,"timestamp":1733234499000}}"#.to_string(),
        r#"{"type":"session_info","id":"i1","parentId":"t1","timestamp":"2024-12-03T14:01:00.000Z","name":"  Auth fixes  "}"#.to_string(),
        user("u2", "second question", 1_733_234_410_000),
    ];
    let path = write_session(&tmp.0, "2024-12-03T14-00-00-000Z_abc-123.jsonl", &lines);
    let s = read_summary(&path).expect("session parses");
    assert_eq!(s.id, "abc-123");
    assert_eq!(s.cwd, PathBuf::from("/work/proj"));
    assert_eq!(s.name.as_deref(), Some("Auth fixes"));
    assert_eq!(
        s.first_message.as_deref(),
        Some("Fix the login bug\nplease")
    );
    assert_eq!(s.message_count, 5);
    assert_eq!(s.model.as_deref(), Some("claude-sonnet-4-5"));
    // Tool results do not count as activity; the last user message does.
    assert_eq!(s.modified_ms, 1_733_234_410_000);
    assert!(!s.truncated);
    assert_eq!(s.title(), "Auth fixes");
    assert!(s.matches("login"));
    assert!(s.matches("auth"));
    assert!(s.matches("work/proj"));
    assert!(!s.matches("kubernetes"));
}

#[test]
fn later_empty_name_clears_and_model_change_counts() {
    let tmp = TempDir::new("clear");
    let lines = vec![
        header("x", "/p", "2024-12-03T14:00:00.000Z"),
        r#"{"type":"session_info","id":"i1","parentId":null,"timestamp":"2024-12-03T14:01:00.000Z","name":"Old"}"#.to_string(),
        r#"{"type":"session_info","id":"i2","parentId":"i1","timestamp":"2024-12-03T14:02:00.000Z","name":""}"#.to_string(),
        r#"{"type":"model_change","id":"m1","parentId":"i2","timestamp":"2024-12-03T14:05:00.000Z","provider":"openai","modelId":"gpt-4o"}"#.to_string(),
    ];
    let s = read_summary(&write_session(&tmp.0, "a.jsonl", &lines)).unwrap();
    assert_eq!(s.name, None);
    assert_eq!(s.first_message, None);
    assert_eq!(s.model.as_deref(), Some("gpt-4o"));
    assert_eq!(s.message_count, 0);
    // No activity: falls back to the header timestamp.
    assert_eq!(s.modified_ms, 1_733_234_400_000);
    assert_eq!(s.title(), "(no messages)");
}

#[test]
fn rejects_files_without_header() {
    let tmp = TempDir::new("noheader");
    let path = write_session(&tmp.0, "b.jsonl", &[user("u1", "hi", 1)]);
    assert_eq!(read_summary(&path), None);
    let empty = write_session(&tmp.0, "c.jsonl", &[]);
    assert_eq!(read_summary(&empty), None);
}

#[test]
fn lists_project_and_all_sorted_newest_first() {
    let tmp = TempDir::new("list");
    let agent = tmp.0.join("agent");
    let cwd = PathBuf::from("/work/proj");
    let project = agent.join("sessions").join(session_dir_name(&cwd));
    let other = agent.join("sessions").join("--work-other--");
    write_session(
        &project,
        "old.jsonl",
        &[
            header("old", "/work/proj", "2024-01-01T00:00:00Z"),
            user("u", "older", 1_000_000),
        ],
    );
    write_session(
        &project,
        "new.jsonl",
        &[
            header("new", "/work/proj", "2024-01-01T00:00:00Z"),
            user("u", "newer", 3_000_000),
        ],
    );
    write_session(&project, "notes.txt", &["ignored".to_string()]);
    write_session(
        &project,
        "junk.jsonl",
        &["{\"type\":\"message\"}".to_string()],
    );
    write_session(
        &other,
        "x.jsonl",
        &[
            header("other", "/work/other", "2024-01-01T00:00:00Z"),
            user("u", "elsewhere", 2_000_000),
        ],
    );

    let ids = |v: Vec<SessionSummary>| v.into_iter().map(|s| s.id).collect::<Vec<_>>();
    assert_eq!(
        ids(list_sessions(&agent, &cwd, Scope::Project)),
        ["new", "old"]
    );
    assert_eq!(
        ids(list_sessions(&agent, &cwd, Scope::All)),
        ["new", "other", "old"]
    );
    assert!(list_sessions(&agent, Path::new("/nowhere"), Scope::Project).is_empty());
}

#[test]
fn custom_session_dir_filters_by_header_cwd() {
    let tmp = TempDir::new("custom");
    let agent = tmp.0.join("agent");
    let flat = tmp.0.join("flat");
    std::fs::create_dir_all(&agent).unwrap();
    std::fs::write(
        agent.join("settings.json"),
        format!(
            "{{\"sessionDir\": {}}}",
            serde_json::to_string(&flat.to_string_lossy()).unwrap()
        ),
    )
    .unwrap();
    write_session(
        &flat,
        "a.jsonl",
        &[header("mine", "/work/proj", "2024-01-01T00:00:00Z")],
    );
    write_session(
        &flat,
        "b.jsonl",
        &[header("theirs", "/work/other", "2024-01-02T00:00:00Z")],
    );
    let cwd = Path::new("/work/proj");
    let ids = |v: Vec<SessionSummary>| v.into_iter().map(|s| s.id).collect::<Vec<_>>();
    assert_eq!(ids(list_sessions(&agent, cwd, Scope::Project)), ["mine"]);
    assert_eq!(
        ids(list_sessions(&agent, cwd, Scope::All)),
        ["theirs", "mine"]
    );
}

#[test]
fn a_session_is_found_by_id_in_any_project() {
    let tmp = TempDir::new("find");
    let agent = tmp.0.join("agent");
    let other = Path::new("/elsewhere/proj");
    let dir = agent.join("sessions").join(session_dir_name(other));
    write_session(
        &dir,
        "2024-12-03T14-00-00-000Z_abc-1.jsonl",
        &[header(
            "abc-1",
            "/elsewhere/proj",
            "2024-12-03T14:00:00.000Z",
        )],
    );
    // A file named otherwise is still found by its header.
    write_session(
        &dir,
        "renamed.jsonl",
        &[header(
            "abc-2",
            "/elsewhere/proj",
            "2024-12-03T14:00:00.000Z",
        )],
    );
    let found = find_session(&agent, "abc-1").unwrap();
    assert_eq!(found.cwd, other);
    assert!(found.path.ends_with("2024-12-03T14-00-00-000Z_abc-1.jsonl"));
    assert_eq!(find_session(&agent, "abc-2").unwrap().id, "abc-2");
    assert!(find_session(&agent, "missing").is_none());
}

#[test]
fn the_shell_lists_only_its_own_folder_and_when_it_offers_both_methods() {
    let cwd = Path::new("/w/proj");
    let rpc = [ReadSupport::Rpc, ReadSupport::Rpc];
    assert_eq!(choose_source(Some(cwd), cwd, rpc), Source::Shell);
    assert_eq!(
        choose_source(Some(Path::new("/w/other")), cwd, rpc),
        Source::Files,
        "another folder's tab would list another project"
    );
    assert_eq!(choose_source(None, cwd, rpc), Source::Files);
    assert_eq!(
        choose_source(Some(cwd), cwd, [ReadSupport::Rpc, ReadSupport::Legacy]),
        Source::Files,
        "listing without switching by id is the files' job"
    );
    assert_eq!(
        choose_source(Some(cwd), cwd, [ReadSupport::NotReady, ReadSupport::Rpc]),
        Source::Undecided
    );
}

fn entry(id: &str, project: &str, current: bool) -> SessionEntry {
    SessionEntry {
        id: id.into(),
        name: None,
        modified: 1_793_577_600_000,
        message_count: Some(3),
        first_message: format!("Question in {project}"),
        project: project.into(),
        current,
    }
}

#[test]
fn other_projects_open_in_a_new_tab() {
    let cwd = Path::new("/w/gentle-shell");
    let here = entry("a", "gentle-shell", false);
    let there = entry("b", "other", false);
    assert_eq!(switch_scope(Scope::Project, &here, cwd), Some(None));
    assert_eq!(
        switch_scope(Scope::All, &here, cwd),
        Some(Some(SessionsScope::Project)),
        "a same-named folder is tried here first"
    );
    assert_eq!(switch_scope(Scope::All, &there, cwd), None);

    let not_found = ReadFailure::Refused {
        code: Some(DesktopErrorCode::NotFound),
        message: "No session with that id in the requested scope".into(),
    };
    assert!(opens_elsewhere(&not_found, Some(SessionsScope::Project)));
    assert!(
        !opens_elsewhere(&not_found, None),
        "not found in its own project is gone, not elsewhere"
    );
    assert!(opens_elsewhere(&ReadFailure::Unsupported, None));
    let refused = ReadFailure::Refused {
        code: Some(DesktopErrorCode::SessionsUnavailable),
        message: "x".into(),
    };
    assert!(!opens_elsewhere(&refused, Some(SessionsScope::Project)));
    assert!(!opens_elsewhere(
        &ReadFailure::Timeout,
        Some(SessionsScope::Project)
    ));
}

struct Host {
    view: Entity<SessionsView>,
    closed: usize,
    _subscription: Subscription,
}

impl Render for Host {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.view.clone())
    }
}

#[gpui::test]
fn a_shell_listing_renders_and_its_current_session_closes(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_reduce_motion(true);
    });
    let (host, cx) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let mut view =
                SessionsView::empty(PathBuf::from("/w/gentle-shell"), None, None, window, cx);
            view.source = Source::Shell;
            view.scope = Scope::All;
            view.entries = vec![entry("a", "gentle-shell", true), entry("b", "other", false)];
            view.next_cursor = Some("next".into());
            view
        });
        let subscription = cx.subscribe(&view, |host: &mut Host, _, event: &SessionsEvent, _| {
            if matches!(event, SessionsEvent::Close) {
                host.closed += 1;
            }
        });
        Host {
            view,
            closed: 0,
            _subscription: subscription,
        }
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("session-row-0").is_some());
    assert!(cx.debug_bounds("session-row-1").is_some());
    assert!(cx.debug_bounds("session-row-2").is_none());
    assert!(
        cx.debug_bounds("sessions-more").is_some(),
        "a cursor offers older sessions"
    );
    let count = cx.update(|_, cx| host.read(cx).view.read(cx).count());
    assert_eq!(count, 2);

    // The current session is already open: clicking it just closes.
    let at = cx.debug_bounds("session-row-0").unwrap().center();
    cx.simulate_click(at, Modifiers::none());
    cx.run_until_parked();
    assert_eq!(cx.update(|_, cx| host.read(cx).closed), 1);
}
