//! Live check of the screens' reads against a real Gentle Shell launcher:
//! the tab's own `Desktop` channel and reads slice, fed by a real
//! `PiProcess`, routed the way `session/events.rs` and `session/rpc.rs` route
//! them, in a temporary home with two seeded sessions (one in another
//! project) so no real configuration, auth or session is touched.
//!
//! `#[ignore]`d: run it with
//!
//! ```text
//! GENTLE_SHELL_LIVE_LAUNCHER=/path/to/gentle-shell/bin/gentle-shell.mjs \
//!   cargo test -p gentle-shell-gpui --locked live_screen_reads -- --ignored --nocapture
//! ```
//!
//! Against Gentle Shell `main` it lists (with a cursor), refuses a switch to
//! the other project's session in project scope, switches to the seeded one
//! by id (the tab's reset and re-probe included), reads stats and update
//! status. Against 4.8.0 it checks that no read is offered or sent.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use pi_rpc::desktop_rpc::{
    DesktopErrorCode, DesktopMethod, DesktopRecord, ListSessionsQuery, SessionsScope, StatsRange,
    StatsScope, decode_payload,
};
use pi_rpc::{Incoming, LaunchSpec, PiProcess, UiMethod, UiRequest};
use serde_json::{Value, json};

use super::super::{Desktop, Send, Step};
use super::{ReadFailure, ReadOutcome, ReadRequest, ReadSupport, ReadTicket, is_read};

const STEP_TIMEOUT: Duration = Duration::from_secs(60);

struct Root(PathBuf);

impl Root {
    fn new() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let path =
            std::env::temp_dir().join(format!("gs-reads-live-{}-{nanos}", std::process::id()));
        for sub in ["home", "tmp", "config", "agent", "workspace", "other"] {
            std::fs::create_dir_all(path.join(sub)).unwrap();
        }
        // Pi resolves the working directory as the OS reports it.
        Self(dunce::canonicalize(&path).unwrap())
    }

    fn dir(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn iso(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let (y, m, d) = gentle_core::usage::time::civil_from_days(secs.div_euclid(86_400));
    let rem = secs.rem_euclid(86_400);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.000Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// One Pi session file with a question and a priced answer, an hour ago.
fn seed(agent: &Path, cwd: &Path, id: &str, question: &str) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let at = now - 3_600_000;
    let dir = agent
        .join("sessions")
        .join(gentle_core::sessions::session_dir_name(cwd));
    std::fs::create_dir_all(&dir).unwrap();
    let lines = [
        json!({"type": "session", "version": 3, "id": id, "timestamp": iso(at), "cwd": cwd}),
        json!({"type": "message", "id": "u1", "parentId": null, "timestamp": iso(at + 1000),
            "message": {"role": "user", "content": [{"type": "text", "text": question}], "timestamp": at + 1000}}),
        json!({"type": "message", "id": "a1", "parentId": "u1", "timestamp": iso(at + 2000),
            "message": {"role": "assistant", "content": [{"type": "text", "text": "Seeded answer"}],
                "provider": "anthropic", "model": "claude-seeded", "stopReason": "stop",
                "usage": {"input": 100, "output": 20, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 120,
                    "cost": {"input": 0.001, "output": 0.002, "cacheRead": 0, "cacheWrite": 0, "total": 0.003}},
                "timestamp": at + 2000}}),
    ];
    let text: Vec<String> = lines.iter().map(Value::to_string).collect();
    let file = dir.join(format!("{}_{id}.jsonl", iso(at).replace([':', '.'], "-")));
    std::fs::write(file, text.join("\n") + "\n").unwrap();
}

fn find_node() -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join("node"))
        .find(|path| path.is_file())
}

fn spec(launcher: &Path, node: &Path, root: &Root) -> LaunchSpec {
    let home = root.dir("home");
    let tmp = root.dir("tmp");
    let env: Vec<(String, std::ffi::OsString)> = vec![
        ("HOME".into(), home.clone().into()),
        ("USERPROFILE".into(), home.clone().into()),
        ("TMPDIR".into(), tmp.clone().into()),
        ("TMP".into(), tmp.clone().into()),
        ("TEMP".into(), tmp.into()),
        ("GENTLE_PI_CONFIG_HOME".into(), root.dir("config").into()),
        ("GENTLE_PI_AGENT_HOME".into(), root.dir("agent").into()),
        ("PI_CODING_AGENT_DIR".into(), root.dir("agent").into()),
        ("GENTLE_SHELL_INTERACTIVE_HOST".into(), "1".into()),
        ("GENTLE_SHELL_NO_AUTO_SETUP".into(), "1".into()),
        ("GENTLE_PI_SKIP_GENTLE_AI_INSTALL".into(), "1".into()),
        ("PI_OFFLINE".into(), "1".into()),
        ("PI_NO_LOCAL_LLM".into(), "1".into()),
        ("NO_COLOR".into(), "1".into()),
        ("XDG_CONFIG_HOME".into(), home.join(".config").into()),
        ("XDG_DATA_HOME".into(), home.join(".local/share").into()),
        ("XDG_STATE_HOME".into(), home.join(".local/state").into()),
        ("XDG_CACHE_HOME".into(), home.join(".cache").into()),
        ("GENTLE_SHELL_PREFIX".into(), "".into()),
        ("GENTLE_SHELL_BIN".into(), "".into()),
        ("GENTLE_SHELL_CONFIG_HOME".into(), "".into()),
    ];
    LaunchSpec {
        program: node.to_path_buf(),
        args: vec![
            launcher.to_string_lossy().into_owned(),
            "--mode".into(),
            "rpc".into(),
        ],
        cwd: root.dir("workspace"),
        env,
        launcher: node.to_path_buf(),
        bundled: None,
    }
}

/// The tab, minus its UI: the channel and its reads, fed by the process.
struct Tab {
    process: Arc<PiProcess>,
    rx: std::sync::mpsc::Receiver<Incoming>,
    desktop: Desktop,
    /// Replaced sessions seen (`switch_session` answered `changed`).
    switches: usize,
}

impl Tab {
    fn command(&self, command: Value) -> Value {
        smol::block_on(self.process.request(command))
            .unwrap_or_else(|e| panic!("rpc command failed: {e:#}"))
            .data
    }

    /// Hand requests to the backend and the outer disposition to the slice
    /// that issued them, as `send_one_desktop`/`outer_desktop` do.
    fn deliver(&mut self, sends: Vec<Send>) {
        for send in sends {
            let method = send.request.method();
            let data = self.command(send.request.prompt_command());
            let handled = data["disposition"] == "handled";
            assert!(handled, "{method} not handled: {data}");
            if is_read(method) {
                let applied = self.desktop.reads_outer(send.generation, handled);
                self.after_reads(applied);
            } else {
                let applied = self.desktop.outer(send.generation, handled);
                self.deliver(applied.sends);
            }
        }
    }

    fn after_reads(&mut self, applied: super::Applied) {
        if applied.switched {
            // `apply_reads`: the session was replaced; re-probe it.
            self.switches += 1;
            self.desktop.reset();
            self.ready();
        }
        if applied.retry {
            std::thread::sleep(Duration::from_millis(300));
            self.desktop.reads_release();
        }
        self.deliver(applied.sends);
    }

    fn ready(&mut self) {
        let commands = self.command(json!({"type": "get_commands"}));
        let registered = commands["commands"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|c| c["name"] == "gentle:desktop");
        let applied = self.desktop.ready(registered);
        self.deliver(applied.sends);
    }

    /// One incoming record, routed as `handle_desktop_ui` does for the
    /// channel and the reads; then the read queue is pumped.
    fn step(&mut self, deadline: Instant) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(!remaining.is_zero(), "timed out");
        match self.rx.recv_timeout(remaining) {
            Ok(Incoming::Ui(request)) if is_desktop(&request) => {
                let pending = self.desktop.session().pending().map(|(_, m)| m);
                let response = matches!(widget_record(&request), Some(DesktopRecord::Response(_)));
                if response && pending.is_some_and(is_read) {
                    let applied = self.desktop.reads_apply(&request);
                    self.after_reads(applied);
                } else {
                    let applied = self.desktop.ui(&request);
                    assert!(applied.report.is_none(), "{:?}", applied.report);
                    self.deliver(applied.sends);
                }
            }
            Ok(Incoming::Stderr(line)) => eprintln!("[backend] {line}"),
            Ok(Incoming::Exited(code)) => panic!("backend exited ({code:?})"),
            Ok(_) => {}
            Err(error) => panic!("no backend output: {error}"),
        }
        let pumped = self.desktop.reads_pump();
        self.after_reads(pumped);
    }

    fn until_ready(&mut self) {
        let deadline = Instant::now() + STEP_TIMEOUT;
        while self.desktop.step() != Step::Ready {
            assert_ne!(self.desktop.step(), Step::Unavailable);
            self.step(deadline);
        }
    }

    fn read(&mut self, request: ReadRequest) -> ReadOutcome {
        let ticket: ReadTicket = self.desktop.reads_enqueue(request).unwrap();
        let pumped = self.desktop.reads_pump();
        self.after_reads(pumped);
        let deadline = Instant::now() + STEP_TIMEOUT;
        loop {
            if let Some(outcome) = self.desktop.reads_take(ticket) {
                return outcome;
            }
            self.step(deadline);
        }
    }
}

fn is_desktop(request: &UiRequest) -> bool {
    super::super::is_desktop_widget(request)
}

fn widget_record(request: &UiRequest) -> Option<DesktopRecord> {
    let UiMethod::SetWidget {
        lines: Some(lines), ..
    } = &request.method
    else {
        return None;
    };
    decode_payload(lines.first()?).ok()
}

#[test]
#[ignore = "live launcher check: set GENTLE_SHELL_LIVE_LAUNCHER to .../bin/gentle-shell.mjs, then run with --ignored"]
fn live_screen_reads() {
    let Some(launcher) = std::env::var_os("GENTLE_SHELL_LIVE_LAUNCHER").map(PathBuf::from) else {
        eprintln!("live_screen_reads: GENTLE_SHELL_LIVE_LAUNCHER is not set; skipping");
        return;
    };
    let node = find_node().expect("node on PATH");
    let root = Root::new();
    let workspace = root.dir("workspace");
    let other = root.dir("other");
    // The launcher's own isolated home keeps Pi's agent dir under `HOME`.
    let agent = root.dir("home").join(".gentle-shell").join("agent");
    seed(
        &agent,
        &workspace,
        "11111111-aaaa-4bbb-8ccc-000000000001",
        "Seeded question",
    );
    seed(
        &agent,
        &other,
        "22222222-aaaa-4bbb-8ccc-000000000002",
        "Other project question",
    );

    let (process, incoming) = PiProcess::spawn(&spec(&launcher, &node, &root)).unwrap();
    let (tx, rx) = std::sync::mpsc::sync_channel(256);
    std::thread::spawn(move || {
        while let Ok(item) = incoming.recv_blocking() {
            if tx.send(item).is_err() {
                break;
            }
        }
    });
    let mut tab = Tab {
        process: process.clone(),
        rx,
        desktop: Desktop::new(),
        switches: 0,
    };
    tab.ready();
    tab.until_ready();

    let methods = [
        DesktopMethod::ListSessions,
        DesktopMethod::SwitchSession,
        DesktopMethod::GetUsageStats,
        DesktopMethod::GetUpdateStatus,
    ];
    let support = methods.map(|m| tab.desktop.reads_support(m));
    eprintln!("live_screen_reads: support {support:?}");
    if support.iter().all(|s| *s == ReadSupport::Legacy) {
        // A release Shell: every screen keeps its own path, nothing is sent.
        assert_eq!(
            tab.desktop
                .reads_enqueue(ReadRequest::UpdateStatus { refresh: false }),
            Err(ReadFailure::NotOffered)
        );
        assert!(tab.desktop.reads_pump().sends.is_empty());
        process.shutdown(Duration::from_secs(5));
        eprintln!("live_screen_reads: release Shell, no read offered");
        return;
    }
    assert!(
        support.iter().all(|s| *s == ReadSupport::Rpc),
        "{support:?}"
    );
    eprintln!(
        "live_screen_reads: session file {}",
        tab.command(json!({"type": "get_state"}))["sessionFile"]
    );

    // Sessions: this project's page, then every project a page at a time.
    let ReadOutcome::Sessions(project) = tab.read(ReadRequest::ListSessions(ListSessionsQuery {
        scope: Some(SessionsScope::Project),
        limit: Some(50),
        cursor: None,
    })) else {
        panic!("project listing failed");
    };
    eprintln!("live_screen_reads: project {project:?}");
    let seeded = project
        .sessions
        .iter()
        .find(|s| s.first_message == "Seeded question")
        .expect("the seeded session is listed")
        .clone();
    assert!(!seeded.current);
    assert_eq!(seeded.project, "workspace");
    let mut all = Vec::new();
    let mut cursor = None;
    loop {
        let ReadOutcome::Sessions(page) = tab.read(ReadRequest::ListSessions(ListSessionsQuery {
            scope: Some(SessionsScope::All),
            limit: Some(1),
            cursor: cursor.take(),
        })) else {
            panic!("all-projects page failed");
        };
        assert!(page.sessions.len() <= 1);
        all.extend(page.sessions);
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    eprintln!("live_screen_reads: all {all:?}");
    let elsewhere = all
        .iter()
        .find(|s| s.project == "other")
        .expect("the other project's session is listed in All")
        .clone();

    // The other project's session is not this project's: the browser opens it
    // in a new tab instead.
    let refused = tab.read(ReadRequest::SwitchSession {
        id: elsewhere.id.clone(),
        scope: Some(SessionsScope::Project),
    });
    assert!(
        matches!(&refused, ReadOutcome::Failed(f) if f.code() == Some(DesktopErrorCode::NotFound)),
        "{refused:?}"
    );

    // Open the seeded session by id: the tab resets and re-probes.
    let switched = tab.read(ReadRequest::SwitchSession {
        id: seeded.id.clone(),
        scope: None,
    });
    assert!(
        matches!(&switched, ReadOutcome::Switched(r) if r.changed && !r.cancelled),
        "{switched:?}"
    );
    assert_eq!(tab.switches, 1);
    tab.until_ready();
    let state = tab.command(json!({"type": "get_state"}));
    let file = state["sessionFile"].as_str().unwrap_or_default();
    assert!(file.ends_with(&format!("_{}.jsonl", seeded.id)), "{state}");
    let messages = tab.command(json!({"type": "get_messages"}));
    assert!(
        messages.to_string().contains("Seeded question"),
        "{messages}"
    );
    let ReadOutcome::Sessions(after) =
        tab.read(ReadRequest::ListSessions(ListSessionsQuery::default()))
    else {
        panic!("listing after the switch failed");
    };
    assert!(
        after
            .sessions
            .iter()
            .any(|s| s.id == seeded.id && s.current)
    );
    let again = tab.read(ReadRequest::SwitchSession {
        id: seeded.id.clone(),
        scope: None,
    });
    assert!(
        matches!(&again, ReadOutcome::Switched(r) if !r.changed),
        "{again:?}"
    );
    assert_eq!(
        tab.switches, 1,
        "a switch to the current session changes nothing"
    );

    // Local stats, every project.
    for range in [StatsRange::Week, StatsRange::All] {
        let ReadOutcome::UsageStats(stats) = tab.read(ReadRequest::UsageStats {
            range,
            scope: StatsScope::All,
        }) else {
            panic!("usage stats failed");
        };
        eprintln!("live_screen_reads: stats {range:?} {stats:?}");
        assert!(stats.totals.messages >= 2, "both seeded answers count");
        assert!(stats.totals.total_tokens >= 240);
        let projects = stats.by_project.expect("byProject for scope all");
        assert!(projects.iter().any(|p| p.name == "workspace"));
        assert!(projects.iter().any(|p| p.name == "other"));
        assert!(stats.by_model.iter().any(|m| m.model == "claude-seeded"));
    }

    // Update status: offline, so nothing is looked up or claimed.
    for refresh in [false, true] {
        let ReadOutcome::UpdateStatus(status) = tab.read(ReadRequest::UpdateStatus { refresh })
        else {
            panic!("update status failed");
        };
        eprintln!("live_screen_reads: update status {status:?}");
        assert!(status.offline && !status.refreshed);
    }

    process.shutdown(Duration::from_secs(5));
    eprintln!("live_screen_reads: passed");
}
