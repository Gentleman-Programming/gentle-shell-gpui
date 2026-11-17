//! Live check of the session sections, job output and subagent thread slices
//! against a real Gentle Shell, through the tab's own routing.
//!
//! `#[ignore]` by default. It starts `node <launcher> --mode rpc` in a
//! temporary home (no credentials, no setup, offline) with a local
//! OpenAI-compatible stand-in model that starts one background job:
//!
//! ```text
//! GENTLE_SHELL_LIVE_LAUNCHER=/tmp/gs-upstream/bin/gentle-shell.mjs \
//!   cargo test -p gentle-shell-gpui --locked desktop_slices_live -- --ignored --nocapture
//! ```
//!
//! Against Gentle Shell `main` it follows a real job's output to its end,
//! reads the session sections the tab shows, and reads an unknown helper's
//! thread; against 4.8.0 it checks that none of it is asked.

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, sync_channel};
use std::time::{Duration, Instant};

use pi_rpc::desktop_rpc::{DesktopMethod, DesktopRecord, decode_widget};
use pi_rpc::{Incoming, LaunchSpec, PiProcess, UiRequest};
use serde_json::{Value, json};

use super::output::Failure;
use super::permission::Permission;
use super::{Desktop, Send, Step, is_desktop_widget};
use crate::features::session::events::{DesktopRoute, route_desktop_widget};

const WAIT: Duration = Duration::from_secs(60);
const LABEL: &str = "probe job";
/// Prints six numbered lines and six colored ones, a second apart.
const COMMAND: &str = "for i in 1 2 3 4 5 6; do echo \"line $i\"; printf '\\033[31mred %s\\033[0m\\n' $i; sleep 1; done";

/// The stand-in model: a first turn calls `bash_background`, the turn after
/// the tool result says it started.
fn serve_model(stream: TcpStream) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
    let mut length = 0;
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let header = line.trim().to_ascii_lowercase();
        if header.is_empty() {
            break;
        }
        if let Some(value) = header.strip_prefix("content-length:") {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0; length];
    if reader.read_exact(&mut body).is_err() {
        return;
    }
    let request: Value = serde_json::from_slice(&body).unwrap_or_default();
    let after_tool = request["messages"]
        .as_array()
        .and_then(|messages| messages.last())
        .is_some_and(|message| message["role"] == "tool");
    let chunk = |delta: Value, finish: Value| {
        let data = json!({
            "id": "c1", "object": "chat.completion.chunk", "model": "mock-1",
            "choices": [{"index": 0, "delta": delta, "finish_reason": finish}],
        });
        format!("data: {data}\n\n")
    };
    let mut events = if after_tool {
        chunk(
            json!({"role": "assistant", "content": "Started it."}),
            json!(null),
        ) + &chunk(json!({}), json!("stop"))
    } else {
        let arguments = json!({"command": COMMAND, "label": LABEL}).to_string();
        chunk(
            json!({"role": "assistant", "tool_calls": [{
                "index": 0, "id": "call_1", "type": "function",
                "function": {"name": "bash_background", "arguments": arguments},
            }]}),
            json!(null),
        ) + &chunk(json!({}), json!("tool_calls"))
    };
    let usage = json!({"id": "c1", "object": "chat.completion.chunk", "choices": [],
                       "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}});
    events += &format!("data: {usage}\n\ndata: [DONE]\n\n");
    let mut stream = stream;
    let _ = write!(
        stream,
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n{events}"
    );
}

fn start_model() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind the stand-in model");
    let port = listener.local_addr().expect("model address").port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            std::thread::spawn(move || serve_model(stream));
        }
    });
    port
}

/// A temporary root with every directory the backend writes, removed on drop.
struct Root(PathBuf);

impl Root {
    fn new() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let path = std::env::temp_dir().join(format!(
            "desktop-slices-live-{}-{nanos}",
            std::process::id()
        ));
        for sub in ["home", "tmp", "config", "agent", "workspace"] {
            std::fs::create_dir_all(path.join(sub)).expect("create temp dir");
        }
        Self(path)
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

fn launch(launcher: &Path, root: &Root, model_port: u16) -> LaunchSpec {
    // The launcher keeps its own Pi home under HOME: the stand-in model is
    // declared there.
    let agent = root.dir("home").join(".gentle-shell/agent");
    std::fs::create_dir_all(&agent).expect("create launcher agent dir");
    let models = json!({"providers": {"mock": {
        "baseUrl": format!("http://127.0.0.1:{model_port}/v1"),
        "api": "openai-completions", "apiKey": "mock",
        "models": [{"id": "mock-1", "name": "Mock", "contextWindow": 100000, "maxTokens": 1000,
                    "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0}}],
    }}});
    std::fs::write(agent.join("models.json"), models.to_string()).expect("write models.json");
    let home = root.dir("home");
    let os = |path: PathBuf| path.into_os_string();
    let env: Vec<(String, OsString)> = vec![
        ("HOME".into(), os(home.clone())),
        ("USERPROFILE".into(), os(home.clone())),
        ("TMPDIR".into(), os(root.dir("tmp"))),
        ("GENTLE_PI_CONFIG_HOME".into(), os(root.dir("config"))),
        ("GENTLE_PI_AGENT_HOME".into(), os(root.dir("agent"))),
        ("PI_CODING_AGENT_DIR".into(), os(root.dir("agent"))),
        ("GENTLE_SHELL_INTERACTIVE_HOST".into(), "1".into()),
        ("GENTLE_SHELL_NO_AUTO_SETUP".into(), "1".into()),
        ("GENTLE_PI_SKIP_GENTLE_AI_INSTALL".into(), "1".into()),
        ("PI_OFFLINE".into(), "1".into()),
        ("PI_NO_LOCAL_LLM".into(), "1".into()),
        ("NO_COLOR".into(), "1".into()),
        ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
        ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
        ("XDG_CONFIG_HOME".into(), os(home.join(".config"))),
        ("XDG_DATA_HOME".into(), os(home.join(".local/share"))),
        ("XDG_STATE_HOME".into(), os(home.join(".local/state"))),
        ("XDG_CACHE_HOME".into(), os(home.join(".cache"))),
        ("GENTLE_SHELL_PREFIX".into(), OsString::new()),
        ("GENTLE_SHELL_BIN".into(), OsString::new()),
        ("GENTLE_SHELL_CONFIG_HOME".into(), OsString::new()),
    ];
    LaunchSpec {
        program: PathBuf::from("node"),
        args: vec![
            launcher.to_string_lossy().into_owned(),
            "--mode".into(),
            "rpc".into(),
        ],
        cwd: root.dir("workspace"),
        env,
        launcher: PathBuf::from("node"),
        bundled: None,
    }
}

/// The tab's Desktop slices over one live backend, routed as the tab does.
struct Live {
    process: Arc<PiProcess>,
    incoming: Receiver<Incoming>,
    desktop: Desktop,
    permission: Permission,
}

impl Live {
    fn rpc(&self, command: Value) -> Value {
        smol::block_on(self.process.request(command.clone()))
            .unwrap_or_else(|error| panic!("{command}: {error:#}"))
            .data
    }

    fn send(&mut self, sends: Vec<Send>) {
        for send in sends {
            let data = self.rpc(send.request.prompt_command());
            assert_eq!(data["disposition"], "handled", "{}", send.request.payload());
        }
    }

    /// Hand one Desktop record to the slice the tab's router picks, then
    /// send what the seeds and the owed output read ask, as the tab does.
    fn route(&mut self, request: &UiRequest) {
        let pending = self.desktop.session().pending().map(|(_, method)| method);
        let decoded = decode_widget(request);
        let sends = match route_desktop_widget(&decoded, pending) {
            DesktopRoute::Permission => {
                self.permission.apply(self.desktop.session(), request).sends
            }
            DesktopRoute::Supervision => {
                if matches!(&decoded, Ok(Some(DesktopRecord::Event(event)))
                    if event.event == super::supervision::AGENTS_UPDATE)
                {
                    self.desktop.output_mut().refresh_thread();
                }
                self.desktop.supervision_apply(request).sends
            }
            DesktopRoute::Subscription => self.desktop.subscription_apply(request).sends,
            DesktopRoute::Design => self.desktop.design_apply(request).sends,
            DesktopRoute::Config => self.desktop.config_apply(request).sends,
            DesktopRoute::Output => self.desktop.output_apply(request).sends,
            DesktopRoute::Review => self.desktop.review_apply(request).sends,
            DesktopRoute::Changes => self.desktop.changes_apply(request).sends,
            DesktopRoute::Reads => self.desktop.reads_apply(request).sends,
            DesktopRoute::Channel => self.desktop.ui(request).sends,
        };
        self.send(sends);
        let resync = self.desktop.flush_resync();
        self.send(resync);
        let seeds = self.desktop.supervision_seed().sends;
        self.send(seeds);
        let output = self.desktop.output_pump().sends;
        self.send(output);
    }

    /// Feed records until `done` holds.
    fn until(&mut self, what: &str, done: impl Fn(&mut Desktop) -> bool) {
        let deadline = Instant::now() + WAIT;
        while !done(&mut self.desktop) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.incoming.recv_timeout(remaining) {
                Ok(Incoming::Ui(request)) if is_desktop_widget(&request) => self.route(&request),
                Ok(Incoming::Stderr(line)) => eprintln!("[backend] {line}"),
                Ok(Incoming::Exited(code)) => {
                    panic!("backend exited ({code:?}) waiting for {what}")
                }
                Ok(_) => {}
                Err(RecvTimeoutError::Timeout) => panic!("timed out waiting for {what}"),
                Err(RecvTimeoutError::Disconnected) => panic!("backend closed waiting for {what}"),
            }
        }
    }
}

#[test]
#[ignore = "live: set GENTLE_SHELL_LIVE_LAUNCHER to .../bin/gentle-shell.mjs, then run with --ignored"]
fn desktop_slices_live() {
    let Some(launcher) = std::env::var_os("GENTLE_SHELL_LIVE_LAUNCHER").map(PathBuf::from) else {
        eprintln!("desktop_slices_live: GENTLE_SHELL_LIVE_LAUNCHER is not set; skipping");
        return;
    };
    let root = Root::new();
    let spec = launch(&launcher, &root, start_model());
    let (process, incoming) = PiProcess::spawn(&spec).expect("spawn the backend");
    let (tx, rx) = sync_channel(256);
    std::thread::spawn(move || {
        while let Ok(item) = incoming.recv_blocking() {
            if tx.send(item).is_err() {
                break;
            }
        }
    });
    let mut live = Live {
        process,
        incoming: rx,
        desktop: Desktop::new(),
        permission: Permission::default(),
    };

    let commands = live.rpc(json!({"type": "get_commands"}));
    let registered = commands["commands"]
        .as_array()
        .is_some_and(|list| list.iter().any(|c| c["name"] == "gentle:desktop"));
    let probe = live.desktop.ready(registered).sends;
    live.send(probe);
    live.until("the probe", |desktop| desktop.step() == Step::Ready);
    let capabilities = live.desktop.capabilities().cloned().expect("capabilities");

    if !capabilities.supports(DesktopMethod::GetJobOutput) {
        // 4.8.0: nothing new is asked, and the slices stay empty.
        assert!(!capabilities.supports_session_sections());
        assert!(!live.desktop.sections().wanted());
        assert!(live.desktop.sections().pi_commands().is_empty());
        assert!(live.desktop.sections().todo().is_none());
        live.desktop.output_mut().watch_job("job-1");
        live.desktop.output_mut().watch_thread("agent-1");
        assert!(live.desktop.output().job().is_none() && live.desktop.output().thread().is_none());
        assert!(live.desktop.output_pump().sends.is_empty());
        eprintln!("desktop_slices_live: release without the additions; nothing new asked");
        live.process.shutdown(Duration::from_secs(5));
        return;
    }

    // Sections, read by the probe itself.
    let sections = live.desktop.sections();
    let pi: Vec<(String, bool)> = sections
        .pi_commands()
        .into_iter()
        .map(|command| (command.name, command.terminal))
        .collect();
    assert!(pi.contains(&("new".into(), false)), "{pi:?}");
    assert!(pi.contains(&("model".into(), true)), "{pi:?}");
    assert_eq!(sections.todo(), Some(Vec::new()));
    eprintln!(
        "desktop_slices_live: probe sections -> {} Pi built-ins, phase {:?}, context {:?}",
        pi.len(),
        sections.phase_label(),
        sections.context()
    );

    // A turn on the stand-in model starts a real background job.
    live.until("the job list", |desktop| desktop.jobs_ready());
    live.rpc(json!({"type": "set_model", "provider": "mock", "modelId": "mock-1"}));
    let started = live.rpc(json!({"type": "prompt", "message": "Run the probe job."}));
    eprintln!("desktop_slices_live: prompt -> {started}");
    live.until("the job's jobs_update", |desktop| {
        desktop.jobs().iter().any(|job| job.label == LABEL)
    });
    let id = live
        .desktop
        .jobs()
        .iter()
        .find(|job| job.label == LABEL)
        .map(|job| job.id.clone())
        .unwrap();

    // Follow its output once a second, as the open well does, to its end.
    live.desktop.output_mut().watch_job(&id);
    let mut reads = 0;
    loop {
        let sends = live.desktop.output_pump().sends;
        live.send(sends);
        live.until("a job output read", |desktop| {
            desktop.session().pending().is_none()
        });
        reads += 1;
        let job = live.desktop.output().job().unwrap();
        let tail = job.output.as_ref().map(|read| read.output.clone());
        eprintln!("desktop_slices_live: read {reads} -> {tail:?}");
        assert!(job.failure.is_none(), "{:?}", job.failure);
        if !live.desktop.output_mut().tick_job() {
            break;
        }
        assert!(reads < 20, "the job never ended");
        std::thread::sleep(Duration::from_secs(1));
    }
    let read = live.desktop.output().job().unwrap().output.clone().unwrap();
    assert!(
        read.output.contains("line 6") && read.output.contains("red 6"),
        "{read:?}"
    );
    assert!(
        !read.output.contains('\u{1b}'),
        "control sequences are stripped"
    );
    assert!(reads > 1, "a running job is read more than once");

    // The context the feed reports for the model the turn used.
    live.until("the context section", |desktop| {
        desktop
            .sections()
            .context()
            .flatten()
            .is_some_and(|context| context.window == Some(100_000))
    });

    // An unknown helper's thread answers `not_found`, shown as gone.
    live.desktop.output_mut().watch_thread("agent-unknown");
    let sends = live.desktop.output_pump().sends;
    live.send(sends);
    live.until("the thread read", |desktop| {
        desktop
            .output()
            .thread()
            .is_some_and(|thread| thread.failure.is_some())
    });
    assert_eq!(
        live.desktop.output().thread().unwrap().failure,
        Some(Failure::Gone)
    );
    eprintln!("desktop_slices_live: main passed after {reads} output reads");
    live.process.shutdown(Duration::from_secs(5));
}
