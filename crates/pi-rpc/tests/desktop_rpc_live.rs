//! Live Desktop↔Shell maintainer probe for `pi-rpc`.
//!
//! This is the real counterpart of the hermetic `desktop_rpc.rs` suite: it
//! spawns the actual Pi RPC backend and drives the exact `PiProcess` /
//! `Incoming::Ui` / `DesktopSession` loop the app uses, against an isolated
//! home so no real configuration, auth, model, provider, agent, job, canvas or
//! audio is ever touched.
//!
//! It is `#[ignore]` by default: `cargo test -p pi-rpc --locked` only compiles
//! it, which keeps ordinary runs hermetic. To run it:
//!
//! ```text
//! GENTLE_PI_RUNTIME_CLI=.../node_modules/@earendil-works/pi-coding-agent/dist/bundle/cli.js \
//!   cargo test -p pi-rpc --test desktop_rpc_live -- --ignored --nocapture
//! ```
//!
//! `GENTLE_PI_RUNTIME_CLI` selects the candidate Pi CLI bundle; the Gentle
//! extension files (`extensions/gentle-{desktop-rpc,ai,shell,design,agents}.ts`)
//! are resolved as absolute paths from the Gentle package root above that file.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, ensure};
use pi_rpc::desktop_rpc::{
    ApplyProfileOutcome, ApplyProfileResult, Capabilities, ChangeSource, ChangeStatus,
    ConfigChangeOrigin, ConfigStatus, ConfigStore, ConfigWrite, ConfigWriteTarget,
    DesktopErrorCode, DesktopEvent, DesktopMethod, DesktopRecord, DesktopRequest, DesktopResponse,
    DesktopRpcError, DesktopSession, ListSessionsQuery, PROMPT_COMMAND, PermissionMode,
    ProfileApplyMode, ProfilesData, ReviewEffective, ReviewMode, ReviewScope, ReviewSetting,
    SCHEMA, SessionSection, SessionsScope, SetReviewModeOutcome, Snapshot, StatsRange, StatsScope,
    UsageRefresh, WIDGET_KEY,
};
use pi_rpc::{Incoming, LaunchSpec, PiProcess, UiMethod, UiRequest};
use serde_json::{Value, json};

/// How long one Desktop roundtrip may take before the probe gives up. The
/// first request also covers backend boot, so this is deliberately generous.
const STEP_TIMEOUT: Duration = Duration::from_secs(60);
/// How long to watch for an event that must NOT arrive (a `rate_limit` after
/// an `unsupported` refresh). Short: its absence is immediate.
const NO_EVENT_WINDOW: Duration = Duration::from_secs(2);
/// Grace handed to `PiProcess::shutdown` before the process tree is killed.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

/// The candidate extensions, in the order the desktop app loads them.
const EXTENSIONS: [&str; 5] = [
    "gentle-desktop-rpc",
    "gentle-ai",
    "gentle-shell",
    "gentle-design",
    "gentle-agents",
];

// ---------------------------------------------------------------------------
// Locating the runtime and extensions
// ---------------------------------------------------------------------------

/// The candidate Pi CLI bundle from `GENTLE_PI_RUNTIME_CLI`, or a clear skip.
fn locate_runtime() -> Option<PathBuf> {
    let path = match std::env::var_os("GENTLE_PI_RUNTIME_CLI") {
        Some(path) if !path.is_empty() => PathBuf::from(path),
        _ => {
            eprintln!(
                "desktop_rpc_live: GENTLE_PI_RUNTIME_CLI is not set; \
                 set it to .../pi-coding-agent/dist/bundle/cli.js to run this probe \
                 (it is #[ignore] by default)"
            );
            return None;
        }
    };
    let absolute = std::fs::canonicalize(&path).unwrap_or(path);
    if absolute.is_file() {
        Some(absolute)
    } else {
        eprintln!(
            "desktop_rpc_live: GENTLE_PI_RUNTIME_CLI={} is not a file; skipping live probe",
            absolute.display()
        );
        None
    }
}

/// The five extension files, resolved as absolute paths from the Gentle package
/// root that also owns the candidate Pi bundle. A missing set is a clear skip,
/// never a guess.
fn locate_extensions(runtime: &Path) -> Option<Vec<PathBuf>> {
    for ancestor in runtime.ancestors().skip(1) {
        let dir = ancestor.join("extensions");
        let all = EXTENSIONS
            .iter()
            .all(|name| dir.join(format!("{name}.ts")).is_file());
        if all {
            return Some(
                EXTENSIONS
                    .iter()
                    .map(|name| dir.join(format!("{name}.ts")))
                    .collect(),
            );
        }
    }
    eprintln!(
        "desktop_rpc_live: no extensions/gentle-*.ts set found above {}; skipping live probe",
        runtime.display()
    );
    None
}

/// `node` on the inherited PATH, searched directly (no login-shell merge).
fn find_node() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let name = if cfg!(windows) { "node.exe" } else { "node" };
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

// ---------------------------------------------------------------------------
// Isolation
// ---------------------------------------------------------------------------

/// A unique temp root with every subdirectory the backend needs. Removed on
/// drop, after `Probe` has shut the backend down (field order guarantees the
/// process is gone first).
struct TempRoot {
    path: PathBuf,
}

impl TempRoot {
    fn new() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path =
            std::env::temp_dir().join(format!("desktop-rpc-live-{}-{nanos}", std::process::id()));
        for sub in ["home", "tmp", "config", "agent", "sessions", "workspace"] {
            std::fs::create_dir_all(path.join(sub)).expect("create probe temp subdir");
        }
        Self { path }
    }

    fn dir(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn null_device() -> &'static str {
    if cfg!(windows) { "NUL" } else { "/dev/null" }
}

/// The child environment: every state directory is inside the temp root, the
/// interactive-host marker is set, and there is no real auth or network.
fn isolation_env(root: &TempRoot) -> Vec<(String, OsString)> {
    let home = root.dir("home");
    let tmp = root.dir("tmp");
    vec![
        ("HOME".into(), home.as_os_str().to_os_string()),
        ("USERPROFILE".into(), home.as_os_str().to_os_string()),
        ("TMPDIR".into(), tmp.as_os_str().to_os_string()),
        ("TMP".into(), tmp.as_os_str().to_os_string()),
        ("TEMP".into(), tmp.as_os_str().to_os_string()),
        (
            "GENTLE_PI_CONFIG_HOME".into(),
            root.dir("config").as_os_str().to_os_string(),
        ),
        (
            "GENTLE_PI_AGENT_HOME".into(),
            root.dir("agent").as_os_str().to_os_string(),
        ),
        (
            "PI_CODING_AGENT_DIR".into(),
            root.dir("agent").as_os_str().to_os_string(),
        ),
        ("GENTLE_SHELL_INTERACTIVE_HOST".into(), "1".into()),
        ("PI_OFFLINE".into(), "1".into()),
        ("PI_NO_LOCAL_LLM".into(), "1".into()),
        ("NO_COLOR".into(), "1".into()),
        ("GIT_CONFIG_GLOBAL".into(), null_device().into()),
        ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
        ("GIT_TERMINAL_PROMPT".into(), "0".into()),
        // Neutralize any inherited bundled-launcher markers so config/home
        // resolution cannot escape the isolated root.
        ("GENTLE_SHELL_PREFIX".into(), OsString::new()),
        ("GENTLE_SHELL_BIN".into(), OsString::new()),
        ("GENTLE_SHELL_CONFIG_HOME".into(), OsString::new()),
    ]
}

/// The launch spec: explicit `node <cli.js>`, no login-shell backend resolution.
fn launch_spec(runtime: &Path, node: &Path, root: &TempRoot, extensions: &[PathBuf]) -> LaunchSpec {
    let mut args: Vec<String> = vec![
        runtime.to_string_lossy().into_owned(),
        "--mode".into(),
        "rpc".into(),
        "--no-extensions".into(),
    ];
    for extension in extensions {
        args.push("-e".into());
        args.push(extension.to_string_lossy().into_owned());
    }
    args.extend([
        "--session-dir".into(),
        root.dir("sessions").to_string_lossy().into_owned(),
        "--offline".into(),
        "--no-mcp".into(),
        "--no-skills".into(),
        "--no-context-files".into(),
    ]);
    LaunchSpec {
        program: node.to_path_buf(),
        args,
        cwd: root.dir("workspace"),
        env: isolation_env(root),
        launcher: node.to_path_buf(),
        bundled: None,
    }
}

// ---------------------------------------------------------------------------
// Incoming pump (bounded record waits)
// ---------------------------------------------------------------------------

enum PumpItem {
    Message(Incoming),
    Closed,
    Timeout,
}

/// Forwards the async-channel stream into a `std::sync::mpsc` receiver so the
/// probe can wait with a hard deadline (`recv_timeout`) instead of sleeping.
struct IncomingPump {
    rx: std::sync::mpsc::Receiver<Incoming>,
}

impl IncomingPump {
    fn next(&self, timeout: Duration) -> PumpItem {
        match self.rx.recv_timeout(timeout) {
            Ok(item) => PumpItem::Message(item),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => PumpItem::Timeout,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => PumpItem::Closed,
        }
    }
}

/// A backend record other than stderr (which is collected) or exit (an error).
enum LiveRecord {
    Ui(UiRequest),
    Event,
}

// ---------------------------------------------------------------------------
// The probe itself
// ---------------------------------------------------------------------------

struct Probe {
    process: Option<Arc<PiProcess>>,
    incoming: IncomingPump,
    session: DesktopSession,
    /// Monotonic ids for raw (not yet typed in the adapter) Desktop methods.
    raw_ids: u64,
    stderr: Vec<String>,
    /// Events consumed while waiting for a response, kept for inspection.
    events: Vec<DesktopEvent>,
    root: TempRoot,
}

impl Probe {
    fn process(&self) -> Result<&Arc<PiProcess>> {
        self.process
            .as_ref()
            .ok_or_else(|| anyhow!("backend process is gone"))
    }

    /// Next `Ui` or `Event`, skipping stderr (collected) and failing on exit,
    /// channel close or timeout.
    fn next_live_record(&mut self, deadline: Instant) -> Result<LiveRecord> {
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            ensure!(!remaining.is_zero(), "timed out waiting for backend output");
            match self.incoming.next(remaining) {
                PumpItem::Message(Incoming::Stderr(line)) => {
                    self.stderr.push(line.clone());
                    eprintln!("[backend stderr] {line}");
                }
                PumpItem::Message(Incoming::Exited(code)) => {
                    anyhow::bail!("backend exited ({code:?})")
                }
                PumpItem::Message(Incoming::Ui(ui)) => return Ok(LiveRecord::Ui(ui)),
                PumpItem::Message(Incoming::Event(_)) => return Ok(LiveRecord::Event),
                // Only a server's stream sends these; a child never does.
                PumpItem::Message(Incoming::Resync | Incoming::DialogResolved { .. }) => {}
                PumpItem::Closed => anyhow::bail!("backend closed"),
                PumpItem::Timeout => anyhow::bail!("timed out waiting for backend output"),
            }
        }
    }

    /// Next record the `DesktopSession` accepts, skipping unrelated widgets and
    /// plain session events.
    fn next_desktop_record_until(&mut self, deadline: Instant) -> Result<DesktopRecord> {
        loop {
            match self.next_live_record(deadline)? {
                LiveRecord::Ui(ui) => match self.session.accept(&ui) {
                    Ok(Some(record)) => return Ok(record),
                    Ok(None) => continue,
                    Err(error) => anyhow::bail!("Desktop session rejected a record: {error}"),
                },
                LiveRecord::Event => continue,
            }
        }
    }

    /// Issue a typed method, send its prompt, prove the outer `disposition` is
    /// `handled` (not the domain answer), then wait for the correlated widget
    /// response. Events seen before the response are consumed and discarded:
    /// they are unrelated startup events, and the session tracks their sequence.
    fn typed_roundtrip(&mut self, request: DesktopRequest) -> Result<DesktopResponse> {
        let request_id = request.request_id().to_string();
        let outer = pollster::block_on(self.process()?.request(request.prompt_command()))
            .map_err(|e| anyhow!("outer prompt for {request_id} failed: {e:#}"))?;
        let disposition = outer.data.get("disposition").and_then(Value::as_str);
        ensure!(
            disposition == Some("handled"),
            "outer prompt for {request_id} was not handled (disposition {disposition:?}); \
             the domain answer must arrive as a widget, not here"
        );
        let deadline = Instant::now() + STEP_TIMEOUT;
        loop {
            match self.next_desktop_record_until(deadline)? {
                DesktopRecord::Response(response) => {
                    ensure!(
                        response.request_id() == Some(request_id.as_str()),
                        "correlation mismatch: expected {request_id}, got {:?}",
                        response.request_id()
                    );
                    return Ok(response);
                }
                DesktopRecord::Event(event) => self.events.push(event),
            }
        }
    }

    fn typed(&mut self, method: DesktopMethod) -> Result<DesktopResponse> {
        let request = self
            .session
            .issue(method)
            .map_err(|e| anyhow!("issue {method}: {e}"))?;
        self.typed_roundtrip(request)
    }

    fn typed_set_permission(
        &mut self,
        mode: PermissionMode,
        persist: Option<bool>,
    ) -> Result<DesktopResponse> {
        let request = self
            .session
            .issue_set_permission_mode(mode, persist)
            .map_err(|e| anyhow!("issue set_permission_mode: {e}"))?;
        self.typed_roundtrip(request)
    }

    fn typed_get_config(&mut self, store: ConfigStore) -> Result<DesktopResponse> {
        let request = self
            .session
            .issue_get_config(store)
            .map_err(|e| anyhow!("issue get_config: {e}"))?;
        self.typed_roundtrip(request)
    }

    fn typed_set_config(
        &mut self,
        write: ConfigWrite,
        expected_revision: &str,
    ) -> Result<DesktopResponse> {
        let request = self
            .session
            .issue_set_config(write, expected_revision)
            .map_err(|e| anyhow!("issue set_config: {e}"))?;
        self.typed_roundtrip(request)
    }

    /// `apply_profile`, decoded; a domain error comes back as `Err`.
    fn typed_apply_profile(
        &mut self,
        profile: &str,
        mode: ProfileApplyMode,
        confirmation: Option<&str>,
    ) -> Result<ApplyProfileResult> {
        let request = self
            .session
            .issue_apply_profile(profile, mode, confirmation)
            .map_err(|e| anyhow!("issue apply_profile: {e}"))?;
        self.typed_roundtrip(request)?
            .apply_profile()
            .map_err(|e| anyhow!("apply_profile: {e}"))
    }

    /// Issue `refresh_subscription_usage` for one provider and wait for its
    /// correlated response, failing if a `rate_limit` event appears either
    /// before or shortly after the answer. The child environment is fully
    /// offline, so no provider traffic happens.
    fn typed_refresh_subscription_usage(&mut self, provider: &str) -> Result<DesktopResponse> {
        let request = self
            .session
            .issue_refresh_subscription_usage(Some(provider))
            .map_err(|e| anyhow!("issue refresh_subscription_usage: {e}"))?;
        let request_id = request.request_id().to_string();
        let outer = pollster::block_on(self.process()?.request(request.prompt_command()))
            .map_err(|e| anyhow!("outer prompt for {request_id} failed: {e:#}"))?;
        let disposition = outer.data.get("disposition").and_then(Value::as_str);
        ensure!(
            disposition == Some("handled"),
            "outer prompt for {request_id} was not handled (disposition {disposition:?})"
        );
        let deadline = Instant::now() + STEP_TIMEOUT;
        let response = loop {
            match self.next_desktop_record_until(deadline)? {
                DesktopRecord::Response(response) => {
                    ensure!(
                        response.request_id() == Some(request_id.as_str()),
                        "correlation mismatch: expected {request_id}, got {:?}",
                        response.request_id()
                    );
                    break response;
                }
                DesktopRecord::Event(event) => {
                    ensure!(
                        event.event != "rate_limit",
                        "unexpected rate_limit event during refresh: {event:?}"
                    );
                }
            }
        };
        self.assert_no_rate_limit_event(Instant::now() + NO_EVENT_WINDOW)?;
        Ok(response)
    }

    /// Assert that no Desktop `rate_limit` event is published before
    /// `deadline`. Other records are consumed, and Desktop records still route
    /// through the session so its event sequence stays in step.
    fn assert_no_rate_limit_event(&mut self, deadline: Instant) -> Result<()> {
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(());
            }
            match self.incoming.next(remaining) {
                PumpItem::Message(Incoming::Stderr(line)) => {
                    self.stderr.push(line.clone());
                    eprintln!("[backend stderr] {line}");
                }
                PumpItem::Message(Incoming::Exited(code)) => {
                    anyhow::bail!("backend exited ({code:?})")
                }
                PumpItem::Message(Incoming::Ui(ui)) => match self.session.accept(&ui)? {
                    Some(DesktopRecord::Event(event)) => {
                        ensure!(
                            event.event != "rate_limit",
                            "unexpected rate_limit event: {event:?}"
                        );
                    }
                    Some(DesktopRecord::Response(_)) => {
                        anyhow::bail!("unexpected response while watching for rate_limit")
                    }
                    None => {}
                },
                PumpItem::Message(
                    Incoming::Event(_) | Incoming::Resync | Incoming::DialogResolved { .. },
                ) => continue,
                PumpItem::Closed => anyhow::bail!("backend closed"),
                PumpItem::Timeout => return Ok(()),
            }
        }
    }

    /// A raw Desktop request (a method the typed adapter does not name yet),
    /// correlated by its own monotonic `probe-N` id.
    fn raw_roundtrip(&mut self, method: &str, params: Value) -> Result<RawDesktopAnswer> {
        self.raw_ids += 1;
        let request_id = format!("probe-{}", self.raw_ids);
        let payload = json!({
            "schema": SCHEMA,
            "requestId": request_id,
            "method": method,
            "params": params,
        })
        .to_string();
        let prompt = json!({
            "type": "prompt",
            "message": format!("{PROMPT_COMMAND} {payload}"),
        });
        let outer = pollster::block_on(self.process()?.request(prompt))
            .map_err(|e| anyhow!("outer prompt for {request_id} failed: {e:#}"))?;
        let disposition = outer.data.get("disposition").and_then(Value::as_str);
        ensure!(
            disposition == Some("handled"),
            "outer prompt for {request_id} was not handled (disposition {disposition:?})"
        );
        let deadline = Instant::now() + STEP_TIMEOUT;
        loop {
            match self.next_live_record(deadline)? {
                LiveRecord::Ui(ui) => {
                    let Some(line) = desktop_widget_line(&ui) else {
                        continue;
                    };
                    let value: Value = serde_json::from_str(line)
                        .map_err(|e| anyhow!("malformed Desktop widget JSON: {e}"))?;
                    if value.get("kind").and_then(Value::as_str) != Some("response") {
                        continue;
                    }
                    if value.get("requestId").and_then(Value::as_str) != Some(request_id.as_str()) {
                        continue;
                    }
                    return Ok(RawDesktopAnswer::from_value(&value));
                }
                LiveRecord::Event => continue,
            }
        }
    }

    /// A plain RPC command (`get_commands`, `get_messages`): the future
    /// resolves to its `data`.
    fn rpc_command(&mut self, command: Value) -> Result<Value> {
        let response = pollster::block_on(self.process()?.request(command))
            .map_err(|e| anyhow!("RPC command failed: {e:#}"))?;
        Ok(response.data)
    }

    fn run(&mut self) -> Result<()> {
        // 1. Discovery: the slash command must be registered before any prompt.
        let commands = self.rpc_command(json!({"type": "get_commands"}))?;
        assert_desktop_command_listed(&commands)?;

        // 2. Capabilities through the real DesktopSession.
        let capabilities = self
            .typed(DesktopMethod::Capabilities)?
            .capabilities()
            .map_err(|e| anyhow!("capabilities decode: {e}"))?;
        assert_capabilities(&capabilities)?;

        // 3. Snapshot: the config projection is present and profiles writable.
        let snapshot = self
            .typed(DesktopMethod::GetSnapshot)?
            .snapshot()
            .map_err(|e| anyhow!("snapshot decode: {e}"))?;
        assert_snapshot(&snapshot)?;

        // 4. Permission read: a fresh isolated session defaults to normal.
        let state = self
            .typed(DesktopMethod::GetPermissionMode)?
            .permission_mode()
            .map_err(|e| anyhow!("permission_mode decode: {e}"))?;
        ensure!(
            state.mode == PermissionMode::Normal && state.default_mode == PermissionMode::Normal,
            "fresh session must default to normal/normal, got {state:?}"
        );

        // 5. Permission set: normal -> auto, persist:false.
        let applied = self
            .typed_set_permission(PermissionMode::Auto, Some(false))?
            .set_permission_mode()
            .map_err(|e| anyhow!("set_permission_mode decode: {e}"))?;
        ensure!(
            applied.mode == PermissionMode::Auto,
            "live mode must become auto: {applied:?}"
        );
        ensure!(
            applied.default_mode == PermissionMode::Normal,
            "persist:false must leave the default normal: {applied:?}"
        );
        ensure!(
            !applied.persisted,
            "persist:false must not persist: {applied:?}"
        );
        ensure!(
            applied.changed,
            "normal -> auto must be a change: {applied:?}"
        );
        ensure!(
            applied.persist_error.is_none(),
            "no persist error expected: {applied:?}"
        );

        // The permission_mode_changed event follows the response, exactly once.
        let event = self.next_desktop_record_until(Instant::now() + STEP_TIMEOUT)?;
        let DesktopRecord::Event(event) = event else {
            anyhow::bail!("expected a permission event, got a response");
        };
        ensure!(
            event.event == "permission_mode_changed",
            "unexpected event: {event:?}"
        );
        let changed = event
            .permission_mode_changed()
            .map_err(|e| anyhow!("permission_mode_changed decode: {e}"))?;
        ensure!(
            changed.mode == PermissionMode::Auto
                && changed.previous_mode == PermissionMode::Normal
                && changed.default_mode == PermissionMode::Normal
                && !changed.persisted
                && changed.source == ChangeSource::Desktop,
            "unexpected permission event payload: {changed:?}"
        );

        // No default file was written, and the same backend still answers (no
        // restart): persist:false changes only the live session.
        let permission_file = self.root.dir("config").join("permission-mode.json");
        ensure!(
            !permission_file.exists(),
            "persist:false must not write a default permission file"
        );
        let rechecked = self
            .typed(DesktopMethod::GetPermissionMode)?
            .permission_mode()
            .map_err(|e| anyhow!("permission_mode recheck decode: {e}"))?;
        ensure!(
            rechecked.mode == PermissionMode::Auto
                && rechecked.default_mode == PermissionMode::Normal,
            "live session must keep auto/normal after the set: {rechecked:?}"
        );

        // 6. History stays empty: Desktop prompts are handled by the extension.
        let messages = self.rpc_command(json!({"type": "get_messages"}))?;
        ensure!(
            messages
                .get("messages")
                .and_then(Value::as_array)
                .is_some_and(|messages| messages.is_empty()),
            "history must be empty after Desktop traffic, got {messages}"
        );

        // 7. Profiles read: capture the write revision from the missing store.
        let read = self
            .typed_get_config(ConfigStore::Profiles)?
            .config()
            .map_err(|e| anyhow!("get_config decode: {e}"))?;
        ensure!(
            read.store == ConfigStore::Profiles,
            "unexpected read: {read:?}"
        );
        ensure!(!read.read_only, "profiles must be writable: {read:?}");
        ensure!(
            read.status == ConfigStatus::Missing,
            "fresh profiles store: {read:?}"
        );
        let write_targets = read
            .write_targets
            .as_deref()
            .ok_or_else(|| anyhow!("profiles read has no writeTargets: {read:?}"))?;
        ensure!(
            write_targets.len() == 1,
            "one profiles write target: {read:?}"
        );
        let expected_revision = write_targets[0].write_revision.clone();
        ensure!(
            !expected_revision.is_empty(),
            "empty writeRevision: {read:?}"
        );

        // 7b. Profiles write: the canonical empty file, guarded by the revision.
        let profiles_data = ProfilesData::new(json!({
            "kind": "gentle-pi.agent_model_profiles",
            "version": 1,
            "profiles": {},
        }))
        .map_err(|e| anyhow!("profiles data: {e}"))?;
        let write = self
            .typed_set_config(ConfigWrite::Profiles(profiles_data), &expected_revision)?
            .set_config()
            .map_err(|e| anyhow!("set_config decode: {e}"))?;
        ensure!(write.changed, "write must change: {write:?}");
        ensure!(
            write.write_target == ConfigWriteTarget::Global,
            "profiles writes to the global target: {write:?}"
        );
        let new_write_revision = write.write_revision.clone();
        ensure!(
            !new_write_revision.is_empty() && new_write_revision != expected_revision,
            "writeRevision must advance after a write: {expected_revision} -> {new_write_revision}"
        );

        // The config_changed event follows the response (an event published
        // before it was discarded by the roundtrip, so this wait would time
        // out), carrying the same write revision.
        let deadline = Instant::now() + STEP_TIMEOUT;
        let config_event = loop {
            match self.next_desktop_record_until(deadline)? {
                DesktopRecord::Event(event) if event.event == "config_changed" => break event,
                DesktopRecord::Event(_) => continue,
                DesktopRecord::Response(response) => {
                    anyhow::bail!("expected config_changed, got a response: {response:?}")
                }
            }
        };
        let changed = config_event
            .config_changed()
            .map_err(|e| anyhow!("config_changed decode: {e}"))?;
        ensure!(
            changed.store == ConfigStore::Profiles,
            "config event store: {changed:?}"
        );
        ensure!(
            changed.origin
                == ConfigChangeOrigin::Desktop {
                    write_target: ConfigWriteTarget::Global,
                    write_revision: new_write_revision.clone(),
                },
            "config event must name the global target and match the response's writeRevision: {changed:?}"
        );
        ensure!(
            changed.revision == write.state.revision,
            "config event revision must match the response: {changed:?}"
        );

        // Disk bytes: the owner wrote exactly the canonical serialized file.
        let profiles_path = self.root.dir("config").join("profiles.json");
        let on_disk = std::fs::read_to_string(&profiles_path)
            .map_err(|e| anyhow!("read {}: {e}", profiles_path.display()))?;
        ensure!(
            on_disk == expected_profiles_bytes(),
            "profiles.json bytes mismatch:\n{on_disk:?}"
        );

        // 8. Offline usage: a supported provider with no data is `pending`.
        let usage =
            self.raw_roundtrip("get_subscription_usage", json!({"provider": "anthropic"}))?;
        let usage = usage.ensure_ok()?.clone();
        let providers = usage["providers"]
            .as_array()
            .ok_or_else(|| anyhow!("usage has no providers array: {usage}"))?;
        ensure!(
            providers.len() == 1,
            "exactly one provider requested: {usage}"
        );
        ensure!(
            providers[0]["provider"].as_str() == Some("anthropic"),
            "usage provider: {usage}"
        );
        ensure!(
            providers[0]["status"].as_str() == Some("pending"),
            "offline usage must be pending: {usage}"
        );
        ensure!(
            providers[0].get("limits").is_none(),
            "pending usage must carry no limits: {usage}"
        );

        // 8b. Refresh disposition: anthropic is offline/unsupported, so the
        // typed refresh reports `refresh: "unsupported"`, emits no rate_limit
        // event, and touches no network (the child env is fully offline).
        let refreshed = self
            .typed_refresh_subscription_usage("anthropic")?
            .subscription_usage()
            .map_err(|e| anyhow!("subscription_usage decode: {e}"))?;
        ensure!(
            refreshed.providers.len() == 1,
            "exactly one provider requested: {refreshed:?}"
        );
        ensure!(
            refreshed.providers[0].provider == "anthropic",
            "usage provider: {refreshed:?}"
        );
        ensure!(
            refreshed.providers[0].refresh == Some(UsageRefresh::Unsupported),
            "anthropic refresh must be unsupported: {refreshed:?}"
        );

        // 9. Stopped design canvas: no server, no designs, and no URL.
        let design = self.raw_roundtrip("get_design_canvas", json!({}))?;
        let design = design.ensure_ok()?.clone();
        ensure!(
            design["running"].as_bool() == Some(false),
            "canvas must be stopped: {design}"
        );
        ensure!(design["slug"].is_null(), "stopped canvas slug: {design}");
        ensure!(
            design["connectedClients"].as_i64() == Some(0),
            "stopped canvas clients: {design}"
        );
        ensure!(
            design["designs"].as_array().is_some_and(|d| d.is_empty()),
            "no designs in a fresh workspace: {design}"
        );
        ensure!(
            design.get("url").is_none(),
            "stopped canvas must not expose a URL: {design}"
        );

        // 10. Empty subagents and jobs.
        let agents = self
            .typed(DesktopMethod::ListSubagents)?
            .list_subagents()
            .map_err(|e| anyhow!("list_subagents decode: {e}"))?;
        ensure!(
            agents.agents.is_empty(),
            "no live subagents expected: {agents:?}"
        );

        let jobs = self
            .typed(DesktopMethod::ListJobs)?
            .list_jobs()
            .map_err(|e| anyhow!("list_jobs decode: {e}"))?;
        ensure!(jobs.jobs.is_empty(), "no live jobs expected: {jobs:?}");

        // 11. apply_profile: an unknown profile writes nothing; a saved one
        // decodes to one of the documented outcomes, and a confirmation's
        // token, resent, is never asked for again with the same text.
        let unknown = self.typed_apply_profile("nobody", ProfileApplyMode::Session, None);
        ensure!(
            unknown.is_err(),
            "an unsaved profile must be refused: {unknown:?}"
        );
        let read = self
            .typed_get_config(ConfigStore::Profiles)?
            .config()
            .map_err(|e| anyhow!("get_config decode: {e}"))?;
        let revision = read
            .write_targets
            .as_deref()
            .and_then(<[_]>::first)
            .map(|t| t.write_revision.clone())
            .ok_or_else(|| anyhow!("profiles has no write target: {read:?}"))?;
        let team = ProfilesData::new(json!({
            "kind": "gentle-pi.agent_model_profiles",
            "version": 1,
            "profiles": {"team": {}},
        }))
        .map_err(|e| anyhow!("profiles data: {e}"))?;
        self.typed_set_config(ConfigWrite::Profiles(team), &revision)?
            .set_config()
            .map_err(|e| anyhow!("set_config decode: {e}"))?;
        for mode in ProfileApplyMode::ALL {
            let first = self.typed_apply_profile("team", mode, None)?;
            ensure!(first.profile == "team" && first.mode == mode, "{first:?}");
            if let ApplyProfileOutcome::ConfirmationRequired(confirm) = first.outcome {
                ensure!(
                    mode == ProfileApplyMode::Default,
                    "only Make default asks: {confirm:?}"
                );
                let again = self.typed_apply_profile("team", mode, Some(&confirm.token))?;
                ensure!(
                    !matches!(again.outcome, ApplyProfileOutcome::ConfirmationRequired(_)),
                    "the approved token must apply: {again:?}"
                );
            }
        }

        Ok(())
    }
}

/// The Gentle Shell `main` additions, driven through the launcher.
impl Probe {
    /// Issue a typed request only when the peer advertises its method, then
    /// roundtrip it.
    fn gated(
        &mut self,
        capabilities: &Capabilities,
        method: DesktopMethod,
        issue: impl FnOnce(&mut DesktopSession) -> Result<DesktopRequest, DesktopRpcError>,
    ) -> Result<DesktopResponse> {
        capabilities
            .require(method)
            .map_err(|e| anyhow!("{method}: {e}"))?;
        let request = issue(&mut self.session).map_err(|e| anyhow!("issue {method}: {e}"))?;
        ensure!(request.method() == method, "issued {}", request.method());
        self.typed_roundtrip(request)
    }

    /// The next event named `name`, keeping the others.
    fn wait_for_event(&mut self, name: &str, within: Duration) -> Result<Option<DesktopEvent>> {
        let deadline = Instant::now() + within;
        loop {
            match self.next_desktop_record_until(deadline) {
                Ok(DesktopRecord::Event(event)) if event.event == name => return Ok(Some(event)),
                Ok(DesktopRecord::Event(event)) => self.events.push(event),
                Ok(DesktopRecord::Response(response)) => {
                    anyhow::bail!("unexpected response while waiting for {name}: {response:?}")
                }
                Err(_) if Instant::now() >= deadline => return Ok(None),
                Err(error) => return Err(error),
            }
        }
    }

    /// Gentle Shell 4.8.0: none of the additions is offered, the gate refuses
    /// each one before a request exists, and the legacy reads still work.
    fn run_release_gates(&mut self, capabilities: &Capabilities) -> Result<()> {
        for method in NEW_METHODS {
            ensure!(
                capabilities.require(method) == Err(DesktopRpcError::NotAdvertised(method)),
                "{method} must not be offered by this backend: {capabilities:?}"
            );
        }
        ensure!(
            !capabilities.supports_session_sections() && !capabilities.emits("review_mode_changed"),
            "no new events expected: {capabilities:?}"
        );
        ensure!(
            self.session.pending().is_none(),
            "a refused method must leave nothing in flight"
        );
        let snapshot = self
            .typed(DesktopMethod::GetSnapshot)?
            .snapshot()
            .map_err(|e| anyhow!("snapshot decode: {e}"))?;
        ensure!(snapshot.session.is_none(), "{snapshot:?}");
        eprintln!(
            "desktop_rpc_live: release backend refuses {} new methods; methods {:?}; events {:?}",
            NEW_METHODS.len(),
            capabilities.methods,
            capabilities.events
        );
        Ok(())
    }

    fn run_additions(&mut self, capabilities: &Capabilities) -> Result<()> {
        ensure!(
            capabilities.supports_session_sections() && capabilities.emits("review_mode_changed"),
            "main advertises session_snapshot and review_mode_changed: {capabilities:?}"
        );
        self.run_binding_and_definitions(capabilities)?;

        // 1. get_snapshot with every section.
        capabilities.require(DesktopMethod::GetSnapshot)?;
        let request = self
            .session
            .issue_get_snapshot_sections(SessionSection::ALL)
            .map_err(|e| anyhow!("issue sections: {e}"))?;
        let snapshot = self
            .typed_roundtrip(request)?
            .snapshot()
            .map_err(|e| anyhow!("snapshot decode: {e}"))?;
        ensure!(snapshot.state.get("config").is_some(), "{snapshot:?}");
        let session = snapshot
            .session
            .ok_or_else(|| anyhow!("no state.session"))?;
        let project = session
            .project
            .clone()
            .ok_or_else(|| anyhow!("no project"))?;
        ensure!(project.name.as_deref() == Some("workspace"), "{project:?}");
        let changes = session
            .changes
            .clone()
            .flatten()
            .ok_or_else(|| anyhow!("no changes section: {session:?}"))?;
        ensure!(
            changes.files.iter().any(|f| f.path == "notes.txt"),
            "{changes:?}"
        );
        ensure!(
            changes.worktrees.iter().all(|w| w
                .root
                .as_deref()
                .is_some_and(|root| root.starts_with("root-"))),
            "worktree roots must be opaque labels: {changes:?}"
        );
        eprintln!(
            "desktop_rpc_live: get_snapshot sections -> sequence {}, project {project:?}, \
             changes {} file(s) {:?}, activity {:?}, commands {}",
            snapshot.sequence,
            changes.files.len(),
            changes.files,
            session
                .activity
                .as_ref()
                .map(|a| (a.state, a.label.clone())),
            session.commands.as_ref().map_or(0, |c| c.items.len()),
        );

        // 2. get_changes and get_change_diff.
        let changed = self
            .gated(capabilities, DesktopMethod::GetChanges, |s| {
                s.issue(DesktopMethod::GetChanges)
            })?
            .changes()
            .map_err(|e| anyhow!("changes decode: {e}"))?;
        eprintln!("desktop_rpc_live: get_changes -> {changed:?}");
        let notes = changed
            .files
            .iter()
            .find(|f| f.path == "notes.txt")
            .cloned()
            .ok_or_else(|| anyhow!("notes.txt not listed: {changed:?}"))?;
        ensure!(notes.status == ChangeStatus::Modified, "{notes:?}");
        ensure!(
            changed
                .files
                .iter()
                .any(|f| f.path == "fresh.txt" && f.status == ChangeStatus::Untracked),
            "{changed:?}"
        );
        let diff = self
            .gated(capabilities, DesktopMethod::GetChangeDiff, |s| {
                s.issue_get_change_diff(&notes.root, &notes.path)
            })?
            .change_diff()
            .map_err(|e| anyhow!("change_diff decode: {e}"))?;
        ensure!(diff.diff.contains("+two") && !diff.truncated, "{diff:?}");
        eprintln!("desktop_rpc_live: get_change_diff -> {diff:?}");
        let outside = self.gated(capabilities, DesktopMethod::GetChangeDiff, |s| {
            s.issue_get_change_diff(&notes.root, ".git/config")
        })?;
        expect_error(&outside, DesktopErrorCode::NotFound)?;

        // 3. Unknown job and subagent ids.
        let job = self.gated(capabilities, DesktopMethod::GetJobOutput, |s| {
            s.issue_get_job_output("job-unknown")
        })?;
        expect_error(&job, DesktopErrorCode::NotFound)?;
        let thread = self.gated(capabilities, DesktopMethod::GetSubagentThread, |s| {
            s.issue_get_subagent_thread("agent-unknown", Some(0))
        })?;
        expect_error(&thread, DesktopErrorCode::NotFound)?;

        // 4. Sessions: a fresh home lists none; an unknown id never switches.
        for query in [
            ListSessionsQuery::default(),
            ListSessionsQuery {
                scope: Some(SessionsScope::All),
                limit: Some(1),
                cursor: None,
            },
        ] {
            let list = self
                .gated(capabilities, DesktopMethod::ListSessions, |s| {
                    s.issue_list_sessions(&query)
                })?
                .list_sessions()
                .map_err(|e| anyhow!("list_sessions decode: {e}"))?;
            eprintln!("desktop_rpc_live: list_sessions {query:?} -> {list:?}");
        }
        let switch = self.gated(capabilities, DesktopMethod::SwitchSession, |s| {
            s.issue_switch_session("00000000-0000-0000-0000-000000000000", None)
        })?;
        expect_error(&switch, DesktopErrorCode::NotFound)?;

        // 5. Review mode: read, then the two-call confirmation round on this
        // temporary repository's own (clone) setting.
        let review = self.gated(capabilities, DesktopMethod::GetReviewMode, |s| {
            s.issue(DesktopMethod::GetReviewMode)
        })?;
        match review.review_mode() {
            Ok(mode) => {
                eprintln!("desktop_rpc_live: get_review_mode -> {mode:?}");
                self.review_round(capabilities, mode)?;
            }
            Err(DesktopRpcError::RemoteError(info))
                if info.kind() == Some(DesktopErrorCode::ReviewUnavailable) =>
            {
                eprintln!(
                    "desktop_rpc_live: get_review_mode -> review_unavailable ({}); \
                     gentle-ai is not installed next to this launcher, so set_review_mode is skipped",
                    info.message
                );
            }
            Err(error) => anyhow::bail!("get_review_mode: {error}"),
        }

        // 6. Usage stats.
        for (range, scope) in [(None, None), (Some(StatsRange::All), Some(StatsScope::All))] {
            let stats = self
                .gated(capabilities, DesktopMethod::GetUsageStats, |s| {
                    s.issue_get_usage_stats(range, scope)
                })?
                .usage_stats()
                .map_err(|e| anyhow!("usage_stats decode: {e}"))?;
            eprintln!("desktop_rpc_live: get_usage_stats {range:?} {scope:?} -> {stats:?}");
        }

        // 7. Update status: the child is offline, so even a refresh looks
        // nothing up and claims nothing.
        for refresh in [None, Some(true)] {
            let status = self
                .gated(capabilities, DesktopMethod::GetUpdateStatus, |s| {
                    s.issue_get_update_status(refresh)
                })?
                .update_status()
                .map_err(|e| anyhow!("update_status decode: {e}"))?;
            ensure!(status.offline && !status.refreshed, "{status:?}");
            eprintln!("desktop_rpc_live: get_update_status {refresh:?} -> {status:?}");
        }

        // 7b. Gentle Shell's own status (5.0.0 on): the facts
        // `/gentle:status` prints. An older launcher does not advertise it.
        if capabilities.supports(DesktopMethod::GetStatus) {
            let status = self
                .gated(capabilities, DesktopMethod::GetStatus, |s| {
                    s.issue(DesktopMethod::GetStatus)
                })?
                .status()
                .map_err(|e| anyhow!("status decode: {e}"))?;
            eprintln!("desktop_rpc_live: get_status -> {status:?}");
        }

        // 8. An external config change (#3335): a profiles.json written
        // behind the channel's back is announced with `source: external`.
        std::fs::write(
            self.root.dir("config").join("profiles.json"),
            expected_profiles_bytes(),
        )?;
        let external = self
            .wait_for_event("config_changed", Duration::from_secs(10))?
            .ok_or_else(|| anyhow!("no external config_changed within 10s"))?
            .config_changed()
            .map_err(|e| anyhow!("config_changed decode: {e}"))?;
        ensure!(
            external.store == ConfigStore::Profiles
                && external.origin
                    == ConfigChangeOrigin::External {
                        status: ConfigStatus::Ok
                    },
            "{external:?}"
        );
        eprintln!("desktop_rpc_live: external config_changed -> {external:?}");

        // Every session_snapshot the backend published decodes.
        let snapshots: Vec<_> = self
            .events
            .iter()
            .filter(|event| event.event == "session_snapshot")
            .collect();
        for event in &snapshots {
            event
                .session_snapshot()
                .map_err(|e| anyhow!("session_snapshot seq {} decode: {e}", event.seq))?;
        }
        eprintln!(
            "desktop_rpc_live: {} session_snapshot event(s) decoded",
            snapshots.len()
        );
        Ok(())
    }

    /// 5.0.0: the agent definitions, the session's profile binding and its
    /// clear decode against the real owners; skipped on an older Shell.
    fn run_binding_and_definitions(&mut self, capabilities: &Capabilities) -> Result<()> {
        if capabilities.supports(DesktopMethod::ListAgentDefinitions) {
            let definitions = self
                .typed(DesktopMethod::ListAgentDefinitions)?
                .agent_definitions()
                .map_err(|e| anyhow!("list_agent_definitions decode: {e}"))?;
            eprintln!(
                "desktop_rpc_live: list_agent_definitions -> {} agent(s), orchestrator {:?}",
                definitions.agents.len(),
                definitions.default_orchestrator
            );
        }
        if capabilities.supports(DesktopMethod::GetProfileBinding) {
            let binding = self
                .typed(DesktopMethod::GetProfileBinding)?
                .profile_binding()
                .map_err(|e| anyhow!("get_profile_binding decode: {e}"))?;
            eprintln!("desktop_rpc_live: get_profile_binding -> {binding:?}");
        }
        if capabilities.supports(DesktopMethod::ClearProfileBinding) {
            let cleared = self
                .typed(DesktopMethod::ClearProfileBinding)?
                .clear_profile_binding()
                .map_err(|e| anyhow!("clear_profile_binding decode: {e}"))?;
            eprintln!("desktop_rpc_live: clear_profile_binding -> {cleared:?}");
        }
        if capabilities.supports(DesktopMethod::ListSubagents) {
            let agents = self
                .typed(DesktopMethod::ListSubagents)?
                .list_subagents()
                .map_err(|e| anyhow!("list_subagents decode: {e}"))?;
            eprintln!(
                "desktop_rpc_live: list_subagents -> {} agent(s)",
                agents.agents.len()
            );
        }
        Ok(())
    }

    /// `set_review_mode` off for this clone: a confirmation, then the
    /// approved resend applies and emits `review_mode_changed`, and the same
    /// token resent is stale and asks again.
    fn review_round(&mut self, capabilities: &Capabilities, before: ReviewMode) -> Result<()> {
        let first = self
            .gated(capabilities, DesktopMethod::SetReviewMode, |s| {
                s.issue_set_review_mode(false, ReviewScope::Clone, None)
            })?
            .set_review_mode()
            .map_err(|e| anyhow!("set_review_mode decode: {e}"))?;
        eprintln!("desktop_rpc_live: set_review_mode (no token) -> {first:?}");
        ensure!(first.mode == before, "{first:?}");
        let SetReviewModeOutcome::ConfirmationRequired(confirmation) = first.outcome else {
            anyhow::bail!("the first call must ask: {first:?}");
        };
        let applied = self
            .gated(capabilities, DesktopMethod::SetReviewMode, |s| {
                s.issue_set_review_mode(false, ReviewScope::Clone, Some(&confirmation.token))
            })?
            .set_review_mode()
            .map_err(|e| anyhow!("set_review_mode decode: {e}"))?;
        eprintln!("desktop_rpc_live: set_review_mode (token) -> {applied:?}");
        ensure!(
            applied.outcome == SetReviewModeOutcome::Applied { changed: true }
                && applied.mode.effective == ReviewEffective::Disabled
                && applied.mode.scopes.clone == ReviewSetting::Disabled,
            "{applied:?}"
        );
        let event = self
            .wait_for_event("review_mode_changed", STEP_TIMEOUT)?
            .ok_or_else(|| anyhow!("no review_mode_changed"))?
            .review_mode_changed()
            .map_err(|e| anyhow!("review_mode_changed decode: {e}"))?;
        ensure!(
            event.mode == applied.mode && event.previous_mode == before,
            "{event:?}"
        );
        eprintln!("desktop_rpc_live: review_mode_changed -> {event:?}");
        let stale = self
            .gated(capabilities, DesktopMethod::SetReviewMode, |s| {
                s.issue_set_review_mode(false, ReviewScope::Clone, Some(&confirmation.token))
            })?
            .set_review_mode()
            .map_err(|e| anyhow!("set_review_mode decode: {e}"))?;
        ensure!(
            matches!(&stale.outcome, SetReviewModeOutcome::ConfirmationRequired(fresh) if fresh.token != confirmation.token),
            "a stale token must ask again: {stale:?}"
        );
        eprintln!("desktop_rpc_live: set_review_mode (stale token) -> {stale:?}");
        Ok(())
    }
}

/// The methods Gentle Shell `main` added after 4.8.0.
const NEW_METHODS: [DesktopMethod; 11] = [
    DesktopMethod::GetJobOutput,
    DesktopMethod::GetSubagentThread,
    DesktopMethod::ListSessions,
    DesktopMethod::SwitchSession,
    DesktopMethod::GetChanges,
    DesktopMethod::GetChangeDiff,
    DesktopMethod::GetReviewMode,
    DesktopMethod::SetReviewMode,
    DesktopMethod::GetUsageStats,
    DesktopMethod::GetUpdateStatus,
    DesktopMethod::GetStatus,
];

/// Require a response to be the documented error `code`.
fn expect_error(response: &DesktopResponse, code: DesktopErrorCode) -> Result<()> {
    let info = response
        .error()
        .ok_or_else(|| anyhow!("expected {code}, got {response:?}"))?;
    ensure!(info.kind() == Some(code), "expected {code}, got {info:?}");
    eprintln!(
        "desktop_rpc_live: {:?} -> {} ({})",
        response.request_id(),
        info.code,
        info.message
    );
    Ok(())
}

impl Drop for Probe {
    fn drop(&mut self) {
        let Some(process) = self.process.take() else {
            return;
        };
        // Orderly shutdown first, then wait for the reaped child before the
        // temp root (dropped after this struct's fields) is removed. This also
        // runs on assertion failure, so the process tree is never leaked.
        process.shutdown(SHUTDOWN_GRACE);
        let deadline = Instant::now() + SHUTDOWN_GRACE + Duration::from_secs(10);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match self.incoming.next(remaining) {
                PumpItem::Message(Incoming::Exited(_)) | PumpItem::Closed => break,
                PumpItem::Message(_) => continue,
                PumpItem::Timeout => break,
            }
        }
        // `process` (the last Arc) drops here, before the struct fields, so
        // `PiProcess`'s own Drop runs (a no-op after the explicit shutdown)
        // before `root` removes the temporary directory.
    }
}

// ---------------------------------------------------------------------------
// Raw answer parsing and small pure helpers
// ---------------------------------------------------------------------------

struct RawDesktopAnswer {
    request_id: Option<String>,
    ok: bool,
    data: Value,
    error_code: Option<String>,
    error_message: Option<String>,
}

impl RawDesktopAnswer {
    fn from_value(value: &Value) -> Self {
        let request_id = value
            .get("requestId")
            .and_then(Value::as_str)
            .map(str::to_string);
        let ok = value.get("ok").and_then(Value::as_bool).unwrap_or(false);
        let error = value.get("error").cloned().unwrap_or(Value::Null);
        let (data, error_code, error_message) = if ok {
            (
                value.get("data").cloned().unwrap_or(Value::Null),
                None,
                None,
            )
        } else {
            (
                Value::Null,
                error
                    .get("code")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                error
                    .get("message")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            )
        };
        Self {
            request_id,
            ok,
            data,
            error_code,
            error_message,
        }
    }

    fn ensure_ok(&self) -> Result<&Value> {
        ensure!(
            self.ok,
            "Desktop request failed: {} {}",
            self.error_code.as_deref().unwrap_or(""),
            self.error_message.as_deref().unwrap_or("")
        );
        Ok(&self.data)
    }
}

fn desktop_widget_line(ui: &UiRequest) -> Option<&str> {
    match &ui.method {
        UiMethod::SetWidget { key, lines, .. } if key == WIDGET_KEY => {
            lines.as_ref()?.first().map(String::as_str)
        }
        _ => None,
    }
}

fn assert_desktop_command_listed(data: &Value) -> Result<()> {
    let commands = data
        .get("commands")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("get_commands response has no commands array: {data}"))?;
    let listed = commands
        .iter()
        .filter_map(|command| command.get("name").and_then(Value::as_str))
        .any(|name| name.trim_start_matches('/') == PROMPT_COMMAND.trim_start_matches('/'));
    ensure!(
        listed,
        "gentle:desktop command not listed by get_commands: {data}"
    );
    Ok(())
}

fn assert_capabilities(capabilities: &Capabilities) -> Result<()> {
    for method in [
        DesktopMethod::Capabilities,
        DesktopMethod::GetSnapshot,
        DesktopMethod::GetPermissionMode,
        DesktopMethod::SetPermissionMode,
        DesktopMethod::ListSubagents,
        DesktopMethod::CancelSubagent,
        DesktopMethod::ListJobs,
        DesktopMethod::CancelJob,
        DesktopMethod::GetSubscriptionUsage,
        DesktopMethod::RefreshSubscriptionUsage,
        DesktopMethod::GetConfig,
        DesktopMethod::SetConfig,
        DesktopMethod::GetDesignCanvas,
    ] {
        ensure!(
            capabilities.supports(method),
            "capabilities does not advertise {method}: {capabilities:?}"
        );
    }
    for event in [
        "config_changed",
        "permission_mode_changed",
        "rate_limit",
        "design_canvas",
        "agents_update",
        "jobs_update",
    ] {
        ensure!(
            capabilities.emits(event),
            "capabilities does not emit {event}"
        );
    }
    Ok(())
}

fn assert_snapshot(snapshot: &Snapshot) -> Result<()> {
    let stores = snapshot
        .state
        .get("config")
        .and_then(|config| config.get("stores"))
        .and_then(Value::as_array)
        .ok_or_else(|| {
            anyhow!(
                "snapshot state has no config.stores array: {:?}",
                snapshot.state
            )
        })?;
    let profiles = stores
        .iter()
        .find(|store| store.get("store").and_then(Value::as_str) == Some("profiles"))
        .ok_or_else(|| anyhow!("snapshot has no profiles store"))?;
    ensure!(
        profiles.get("readOnly").and_then(Value::as_bool) == Some(false),
        "profiles must be writable: {profiles}"
    );
    Ok(())
}

/// What `serializeProfilesFile({kind, version, profiles: {}})` writes on disk.
fn expected_profiles_bytes() -> &'static str {
    "{\n  \"kind\": \"gentle-pi.agent_model_profiles\",\n  \"version\": 1,\n  \"profiles\": {}\n}\n"
}

fn spawn_probe(spec: LaunchSpec, root: TempRoot) -> Result<Probe> {
    let (process, incoming) =
        PiProcess::spawn(&spec).map_err(|e| anyhow!("spawn backend: {e:#}"))?;
    let (tx, rx) = std::sync::mpsc::sync_channel::<Incoming>(256);
    std::thread::Builder::new()
        .name("desktop-rpc-live-pump".into())
        .spawn(move || {
            while let Ok(item) = incoming.recv_blocking() {
                if tx.send(item).is_err() {
                    break;
                }
            }
        })
        .map_err(|e| anyhow!("spawn pump thread: {e}"))?;
    Ok(Probe {
        process: Some(process),
        incoming: IncomingPump { rx },
        session: DesktopSession::new(),
        raw_ids: 0,
        stderr: Vec::new(),
        events: Vec::new(),
        root,
    })
}

/// The Gentle Shell launcher (`bin/gentle-shell.mjs`) from
/// `GENTLE_SHELL_LIVE_LAUNCHER`, or a clear skip. A launcher of the active
/// bundled install (`<prefix>/versions/<id>`, `<prefix>/current` naming it)
/// is refused: it hands Pi `GENTLE_SHELL_PREFIX`, and a bundled prefix's
/// config folder wins over `GENTLE_PI_CONFIG_HOME`, so the probe would read
/// the real `<prefix>/config/gentle-ai` instead of its temporary one.
fn locate_launcher() -> Option<PathBuf> {
    let path = match std::env::var_os("GENTLE_SHELL_LIVE_LAUNCHER") {
        Some(path) if !path.is_empty() => PathBuf::from(path),
        _ => {
            eprintln!(
                "desktop_rpc_live: GENTLE_SHELL_LIVE_LAUNCHER is not set; set it to \
                 .../bin/gentle-shell.mjs to run the launcher probe (it is #[ignore] by default)"
            );
            return None;
        }
    };
    let absolute = std::fs::canonicalize(&path).unwrap_or(path);
    if !absolute.is_file() {
        eprintln!(
            "desktop_rpc_live: GENTLE_SHELL_LIVE_LAUNCHER={} is not a file; skipping",
            absolute.display()
        );
        return None;
    }
    if let Some(prefix) = active_bundled_prefix(&absolute) {
        panic!(
            "desktop_rpc_live: {} belongs to the active Gentle Shell install at {}: it would \
             read and write {}/config/gentle-ai, not the probe's temporary config. Copy the \
             version folder outside `versions/` (cp -R {}/versions/<id> /tmp/gs-copy) and \
             point GENTLE_SHELL_LIVE_LAUNCHER at the copy's bin/gentle-shell.mjs",
            absolute.display(),
            prefix.display(),
            prefix.display(),
            prefix.display(),
        );
    }
    Some(absolute)
}

/// The prefix of the active bundled install `launcher` runs from, by the
/// Shell's own rule (`resolveBundledInstall`): the first ancestor directly
/// under a `versions` folder is the version, and it is active when the
/// prefix's `current` names it.
fn active_bundled_prefix(launcher: &Path) -> Option<PathBuf> {
    let version = launcher.ancestors().find(|dir| {
        dir.parent()
            .and_then(Path::file_name)
            .is_some_and(|name| name == "versions")
    })?;
    let id = version.file_name()?.to_str()?;
    let prefix = version.parent()?.parent()?;
    let current = prefix.join("current");
    let active = if cfg!(windows) {
        std::fs::read_to_string(&current).is_ok_and(|text| text == format!("{id}\n"))
    } else {
        std::fs::read_link(&current).is_ok_and(|target| target == Path::new("versions").join(id))
    };
    active.then(|| prefix.to_path_buf())
}

/// Make the workspace a Git repository with one committed file, then modify
/// it and add an untracked one, so the changes owner has something to list.
fn init_workspace_repo(root: &TempRoot) -> Result<()> {
    let workspace = root.dir("workspace");
    let git = |args: &[&str]| -> Result<()> {
        let status = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=probe",
                "-c",
                "user.email=probe@example.invalid",
            ])
            .args(args)
            .current_dir(&workspace)
            .envs(isolation_env(root))
            .status()
            .map_err(|e| anyhow!("git {args:?}: {e}"))?;
        ensure!(status.success(), "git {args:?} failed: {status}");
        Ok(())
    };
    git(&["init", "-q", "-b", "main"])?;
    std::fs::write(workspace.join("notes.txt"), "one\n")?;
    git(&["add", "notes.txt"])?;
    git(&["commit", "-q", "-m", "init"])?;
    std::fs::write(workspace.join("notes.txt"), "one\ntwo\n")?;
    std::fs::write(workspace.join("fresh.txt"), "new\n")?;
    Ok(())
}

/// `node <launcher> --mode rpc` in the isolated root: the launcher's own home
/// resolution, extensions and owners, with no auto setup and no network.
fn launcher_spec(launcher: &Path, node: &Path, root: &TempRoot) -> LaunchSpec {
    let home = root.dir("home");
    let mut env = isolation_env(root);
    env.extend([
        ("GENTLE_SHELL_NO_AUTO_SETUP".into(), "1".into()),
        ("GENTLE_PI_SKIP_GENTLE_AI_INSTALL".into(), "1".into()),
        (
            "XDG_CONFIG_HOME".into(),
            home.join(".config").into_os_string(),
        ),
        (
            "XDG_DATA_HOME".into(),
            home.join(".local/share").into_os_string(),
        ),
        (
            "XDG_STATE_HOME".into(),
            home.join(".local/state").into_os_string(),
        ),
        (
            "XDG_CACHE_HOME".into(),
            home.join(".cache").into_os_string(),
        ),
    ]);
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
#[ignore = "live backend probe: set GENTLE_PI_RUNTIME_CLI to .../pi-coding-agent/dist/bundle/cli.js, then run with --ignored"]
fn live_desktop_shell_roundtrip() {
    let Some(runtime) = locate_runtime() else {
        return;
    };
    let Some(node) = find_node() else {
        eprintln!("desktop_rpc_live: node was not found on PATH; skipping live probe");
        return;
    };
    let Some(extensions) = locate_extensions(&runtime) else {
        return;
    };
    let root = TempRoot::new();
    let spec = launch_spec(&runtime, &node, &root, &extensions);
    let mut probe = spawn_probe(spec, root).unwrap_or_else(|error| panic!("{error:#}"));
    if let Err(error) = probe.run() {
        panic!(
            "live probe failed: {error:#}\n--- backend stderr ---\n{}",
            probe.stderr.join("\n")
        );
    }
    eprintln!("desktop_rpc_live: Desktop↔Shell roundtrip passed");
}

/// The protocol additions of Gentle Shell `main`, through the real launcher.
/// Against `main` every added method and event is exercised; against a
/// release that predates them (4.8.0), the capability gate must refuse each
/// one without sending it. Point `GENTLE_SHELL_LIVE_LAUNCHER` at either
/// `bin/gentle-shell.mjs`, never the active bundled install's (refused: see
/// `locate_launcher`); a copy of its version folder elsewhere is fine.
#[test]
#[ignore = "live launcher probe: set GENTLE_SHELL_LIVE_LAUNCHER to .../bin/gentle-shell.mjs, then run with --ignored"]
fn live_desktop_launcher_additions() {
    let Some(launcher) = locate_launcher() else {
        return;
    };
    let Some(node) = find_node() else {
        eprintln!("desktop_rpc_live: node was not found on PATH; skipping live probe");
        return;
    };
    let root = TempRoot::new();
    init_workspace_repo(&root).unwrap_or_else(|error| panic!("{error:#}"));
    let spec = launcher_spec(&launcher, &node, &root);
    let mut probe = spawn_probe(spec, root).unwrap_or_else(|error| panic!("{error:#}"));
    let result = (|| -> Result<()> {
        let commands = probe.rpc_command(json!({"type": "get_commands"}))?;
        assert_desktop_command_listed(&commands)?;
        let capabilities = probe
            .typed(DesktopMethod::Capabilities)?
            .capabilities()
            .map_err(|e| anyhow!("capabilities decode: {e}"))?;
        if capabilities.supports(DesktopMethod::GetChanges) {
            probe.run_additions(&capabilities)
        } else {
            probe.run_release_gates(&capabilities)
        }
    })();
    if let Err(error) = result {
        panic!(
            "live launcher probe failed: {error:#}\n--- backend stderr ---\n{}",
            probe.stderr.join("\n")
        );
    }
    eprintln!("desktop_rpc_live: launcher probe passed");
}

// --- Hermetic checks of the probe's own parsing (no backend required) --------

#[test]
fn desktop_widget_line_recognizes_only_the_desktop_key() {
    let desktop = UiRequest {
        id: "u1".into(),
        timeout: None,
        method: UiMethod::SetWidget {
            key: WIDGET_KEY.into(),
            lines: Some(vec!["{}".into()]),
            placement: None,
        },
    };
    assert_eq!(desktop_widget_line(&desktop), Some("{}"));

    let other = UiRequest {
        id: "u2".into(),
        timeout: None,
        method: UiMethod::SetWidget {
            key: "gentle-agents".into(),
            lines: Some(vec!["[]".into()]),
            placement: None,
        },
    };
    assert_eq!(desktop_widget_line(&other), None);

    let cleared = UiRequest {
        id: "u3".into(),
        timeout: None,
        method: UiMethod::SetWidget {
            key: WIDGET_KEY.into(),
            lines: None,
            placement: None,
        },
    };
    assert_eq!(desktop_widget_line(&cleared), None);
}

#[test]
fn raw_answer_distinguishes_ok_from_error() {
    let ok = RawDesktopAnswer::from_value(&json!({
        "schema": SCHEMA,
        "kind": "response",
        "requestId": "probe-1",
        "ok": true,
        "data": {"store": "profiles"},
    }));
    assert!(ok.ok);
    assert_eq!(ok.request_id.as_deref(), Some("probe-1"));
    assert_eq!(ok.data["store"], "profiles");
    assert_eq!(ok.error_code, None);
    assert!(ok.ensure_ok().is_ok());

    let error = RawDesktopAnswer::from_value(&json!({
        "schema": SCHEMA,
        "kind": "response",
        "requestId": "probe-2",
        "ok": false,
        "error": {"code": "read_only", "message": "nope"},
    }));
    assert!(!error.ok);
    assert_eq!(error.error_code.as_deref(), Some("read_only"));
    assert_eq!(error.error_message.as_deref(), Some("nope"));
    assert!(error.ensure_ok().is_err());
}

#[test]
fn profiles_disk_bytes_match_the_owner_serializer() {
    assert_eq!(
        expected_profiles_bytes(),
        "{\n  \"kind\": \"gentle-pi.agent_model_profiles\",\n  \"version\": 1,\n  \"profiles\": {}\n}\n"
    );
}

#[cfg(unix)]
#[test]
fn only_the_active_bundled_version_is_refused() {
    let root = TempRoot::new();
    let prefix = root.dir("home").join(".gentle-shell");
    let launcher_in = |id: &str| {
        let bin = prefix
            .join("versions")
            .join(id)
            .join("node_modules/gentle-pi/bin");
        std::fs::create_dir_all(&bin).unwrap();
        bin.join("gentle-shell.mjs")
    };
    let active = launcher_in("5.0.0-1.6.0");
    let kept = launcher_in("4.9.0-1.6.0");
    std::os::unix::fs::symlink("versions/5.0.0-1.6.0", prefix.join("current")).unwrap();

    assert_eq!(active_bundled_prefix(&active), Some(prefix.clone()));
    assert_eq!(active_bundled_prefix(&kept), None);
    let copy = root
        .dir("tmp")
        .join("gs-copy/5.0.0-1.6.0/node_modules/gentle-pi/bin");
    assert_eq!(active_bundled_prefix(&copy.join("gentle-shell.mjs")), None);
}
