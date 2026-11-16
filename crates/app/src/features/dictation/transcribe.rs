//! Transcription backends: the user's `whisper-cli` as a bounded child
//! process on a private WAV file that dies with the job, or the configured
//! HTTPS provider. Both are cancellable jobs: cancelling kills and reaps
//! the child, or shuts the request's transport down. Cancelling cannot
//! recall audio that was already sent; a cancelled job reports that
//! honestly instead of inventing a transcript.

use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gentle_core::dictation as core;

/// The embedded Vosk helper, written into each job's private temp folder
/// and run by the user's isolated venv Python (no FFI in the GUI).
const VOSK_HELPER: &str = include_str!("vosk_transcribe.py");

/// How long a local transcription may run before the child is killed and
/// reaped, and a provider request is stopped.
pub const LOCAL_TIMEOUT: Duration = Duration::from_secs(300);
pub const PROVIDER_TIMEOUT: Duration = Duration::from_secs(60);

/// How often a provider cancellation is re-checked: within this window the
/// select drops the request future and its sockets close.
const CANCEL_GRANULARITY: Duration = Duration::from_millis(25);

/// The provider's JSON answer is read up to this bound, never further.
pub const PROVIDER_BODY_LIMIT: u64 = 1024 * 1024;

const CANCELLED: &str = "The transcription was cancelled.";

// ── Local mode: the user's whisper-cli ──────────────────────────────────────

/// What the two local engines are called in user-facing messages. Whisper's
/// wording is unchanged; Vosk names its own helper.
#[derive(Clone, Copy)]
struct LocalLabels {
    failed: &'static str,
    no_transcript: &'static str,
    unreadable: &'static str,
    too_large: &'static str,
}

const WHISPER_LABELS: LocalLabels = LocalLabels {
    failed: "whisper-cli could not transcribe the recording. Check the executable and model              paths in Configuration → Dictation.",
    no_transcript: "whisper-cli did not write a transcript.",
    unreadable: "whisper-cli's transcript could not be read.",
    too_large: "whisper-cli wrote a transcript too large to use (over 64 KiB).",
};

const VOSK_LABELS: LocalLabels = LocalLabels {
    failed: "The Vosk helper could not transcribe the recording. Check the Python path and the              model directory in Configuration → Dictation.",
    no_transcript: "The Vosk helper did not write a transcript.",
    unreadable: "The Vosk helper's transcript could not be read.",
    too_large: "The Vosk helper wrote a transcript too large to use (over 64 KiB).",
};

fn local_labels(engine: core::LocalEngine) -> LocalLabels {
    match engine {
        core::LocalEngine::Whisper => WHISPER_LABELS,
        core::LocalEngine::Vosk => VOSK_LABELS,
    }
}

struct LocalInner {
    labels: LocalLabels,
    child: Mutex<Option<std::process::Child>>,
    /// The per-run private folder: the WAV and whisper's own files die
    /// with the job, on success, on error, on cancel and on drop. Never
    /// read directly; holding it is what deletes the audio.
    _audio_dir: tempfile::TempDir,
    transcript: std::path::PathBuf,
    cancelled: AtomicBool,
    finished: AtomicBool,
}

/// A running local transcription. Cloneable: the state machine keeps one
/// handle to cancel it and the waiting task holds another. Dropping the
/// last handle kills and reaps the child and deletes the audio.
#[derive(Clone)]
pub struct LocalJob {
    inner: Arc<LocalInner>,
}

impl LocalJob {
    /// Start a transcription with the environment the process inherits,
    /// filtered down to the platform runtime variables.
    pub fn start(local: &core::Local, wav: &[u8]) -> Result<Self, String> {
        match local.engine {
            core::LocalEngine::Whisper => {
                Self::spawn_whisper(local, wav, child_environment(std::env::vars()).into_iter())
            }
            core::LocalEngine::Vosk => Self::spawn_vosk(local, wav),
        }
    }

    /// Vosk: the embedded helper is written into the job's private temp
    /// folder and run by the user's isolated venv Python
    /// (`python -I -B vosk_transcribe.py --model DIR --input WAV --output
    /// TXT`); cancellation, reaping, env stripping and bounds are the
    /// LocalJob ones.
    fn spawn_vosk(local: &core::Local, wav: &[u8]) -> Result<Self, String> {
        let dir = tempfile::tempdir().map_err(|_| {
            "A temporary folder for the recording could not be created.".to_string()
        })?;
        let input = dir.path().join("recording.wav");
        std::fs::write(&input, wav)
            .map_err(|_| "The recording could not be saved for transcription.".to_string())?;
        let helper = dir.path().join("vosk_transcribe.py");
        std::fs::write(&helper, VOSK_HELPER)
            .map_err(|_| "The transcription helper could not be written.".to_string())?;
        let transcript = dir.path().join("transcript.txt");
        let mut command = pi_rpc::command(local.executable.trim());
        command.env_clear();
        for (key, value) in child_environment(std::env::vars()) {
            command.env(&key, &value);
        }
        command.env("PATH", pi_rpc::child_path());
        cap_vosk_threads(&mut command);
        command.args([
            "-I",
            "-B",
            helper
                .to_str()
                .ok_or("The helper path is not valid Unicode.")?,
            "--model",
            local.model.trim(),
            "--input",
            &input.display().to_string(),
            "--output",
            &transcript.display().to_string(),
        ]);
        let mut child = command
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|_| {
                "The isolated Vosk Python could not start. Check its path in Configuration → Dictation."
                    .to_string()
            })?;
        if let Some(stdout) = child.stdout.take() {
            std::thread::spawn(move || drain_bounded(stdout));
        }
        if let Some(stderr) = child.stderr.take() {
            std::thread::spawn(move || drain_bounded(stderr));
        }
        Ok(Self {
            inner: Arc::new(LocalInner {
                labels: local_labels(local.engine),
                child: Mutex::new(Some(child)),
                transcript,
                _audio_dir: dir,
                cancelled: AtomicBool::new(false),
                finished: AtomicBool::new(false),
            }),
        })
    }

    /// Whether the user-supplied executable is plausibly there: an absolute
    /// or relative path must exist; a bare name is looked up on the PATH
    /// children get. A missing executable is reported before a process is
    /// attempted.
    fn executable_found(executable: &str) -> bool {
        let path = std::path::Path::new(executable);
        if path.components().count() > 1 {
            return path.exists();
        }
        #[cfg(windows)]
        let names: Vec<String> = if executable.ends_with(".exe") {
            vec![executable.to_string()]
        } else {
            vec![executable.to_string(), format!("{executable}.exe")]
        };
        #[cfg(not(windows))]
        let names: Vec<String> = vec![executable.to_string()];
        std::env::split_paths(&pi_rpc::child_path())
            .any(|dir| names.iter().any(|name| dir.join(name).is_file()))
    }

    fn spawn_whisper(
        local: &core::Local,
        wav: &[u8],
        env: impl Iterator<Item = (String, String)>,
    ) -> Result<Self, String> {
        if !Self::executable_found(&local.executable) {
            return Err(format!(
                "The whisper-cli executable could not be found ({}). Check the path in \
                 Configuration → Dictation.",
                local.executable
            ));
        }
        let dir = tempfile::tempdir().map_err(|_| {
            "A temporary folder for the recording could not be created.".to_string()
        })?;
        let input = dir.path().join("recording.wav");
        let output_base = dir.path().join("transcript");
        std::fs::write(&input, wav)
            .map_err(|_| "The recording could not be saved for transcription.".to_string())?;
        let mut command = pi_rpc::command(&local.executable);
        // The child gets only the platform runtime variables it may need:
        // provider keys, tokens and the rest of the session stay here.
        command.env_clear();
        for (key, value) in env {
            command.env(&key, &value);
        }
        command.env("PATH", pi_rpc::child_path());
        command.args([
            "--model",
            local.model.trim(),
            "--file",
            &input.display().to_string(),
            "--language",
            local.language.code(),
            "--output-txt",
            "--output-file",
            &output_base.display().to_string(),
        ]);
        // Pipes are drained on threads of their own (bounded capture, then
        // plain draining): a chatty child never blocks on a full pipe.
        let mut child = command
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|_| {
                "whisper-cli could not start. Check the executable in Configuration → Dictation."
                    .to_string()
            })?;
        if let Some(stdout) = child.stdout.take() {
            std::thread::spawn(move || drain_bounded(stdout));
        }
        if let Some(stderr) = child.stderr.take() {
            std::thread::spawn(move || drain_bounded(stderr));
        }
        Ok(Self {
            inner: Arc::new(LocalInner {
                labels: local_labels(local.engine),
                child: Mutex::new(Some(child)),
                transcript: dir.path().join("transcript.txt"),
                _audio_dir: dir,
                cancelled: AtomicBool::new(false),
                finished: AtomicBool::new(false),
            }),
        })
    }

    /// Kill and reap the child promptly (also what dropping the job does).
    /// The audio folder dies with the last handle.
    pub fn cancel(&self) {
        self.inner.cancelled.store(true, Ordering::SeqCst);
        let child = self
            .inner
            .child
            .lock()
            .ok()
            .and_then(|mut guard| guard.take());
        if let Some(mut child) = child {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    /// Wait for the child, bounded by `timeout`, then read its transcript
    /// file (bounded). Runs on a background thread: it blocks.
    pub fn wait(&self, timeout: Duration) -> Result<String, String> {
        let deadline = Instant::now() + timeout;
        loop {
            let exited = self.inner.child.lock().ok().and_then(|mut guard| {
                guard
                    .as_mut()
                    .and_then(|child| child.try_wait().ok().flatten())
            });
            if let Some(status) = exited {
                self.inner.finished.store(true, Ordering::SeqCst);
                if !status.success() {
                    return Err(self.inner.labels.failed.to_string());
                }
                return self.read_transcript();
            }
            if self.inner.cancelled.load(Ordering::SeqCst) {
                // The child is already killed and reaped by `cancel`.
                return Err(CANCELLED.to_string());
            }
            if Instant::now() >= deadline {
                self.cancel();
                return Err("Transcription was stopped: it ran past five minutes.".to_string());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// `--output-txt` writes `<output-file>.txt`; read it bounded. A
    /// transcript larger than the bound is refused outright, never
    /// silently truncated: dictated words must not be lost invisibly.
    fn read_transcript(&self) -> Result<String, String> {
        let file = std::fs::File::open(&self.inner.transcript)
            .map_err(|_| self.inner.labels.no_transcript.to_string())?;
        let mut bytes = Vec::with_capacity(core::MAX_TEXT_BYTES + 1);
        file.take(core::MAX_TEXT_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| self.inner.labels.unreadable.to_string())?;
        if bytes.len() > core::MAX_TEXT_BYTES {
            return Err(self.inner.labels.too_large.to_string());
        }
        let text = String::from_utf8_lossy(&bytes);
        core::transcript_from_text(&text)
            .ok_or_else(|| "No speech was detected in the recording.".to_string())
    }
}

/// Drain a child pipe, keeping at most [`core::MAX_TEXT_BYTES`] (the job
/// never shows it, but it must never grow unbounded) and never letting a
/// chatty child block on a full pipe. Returns what was kept.
fn drain_bounded(mut pipe: impl Read) -> Vec<u8> {
    let mut kept = Vec::with_capacity(core::MAX_TEXT_BYTES);
    let mut chunk = [0u8; 8192];
    loop {
        match pipe.read(&mut chunk) {
            Ok(0) | Err(_) => return kept,
            Ok(n) => {
                let room = core::MAX_TEXT_BYTES.saturating_sub(kept.len());
                let keep = n.min(room);
                kept.extend_from_slice(&chunk[..keep]);
            }
        }
    }
}

/// The environment a local transcription child inherits: only the platform
/// runtime variables it may need (paths, temp folders, locale). Everything
/// else — provider keys, tokens, the rest of the session — stays here.
/// The math-library thread pools a Vosk child opens (Kaldi and its BLAS).
/// Untouched they size themselves from every core on the machine; dictation
/// needs a fraction, and only this child is capped - never the GUI.
const VOSK_THREAD_VARS: [&str; 3] = ["OMP_NUM_THREADS", "MKL_NUM_THREADS", "OPENBLAS_NUM_THREADS"];
const VOSK_THREAD_CAP: u32 = 2;

/// A ceiling, not a value: an inherited setting below the cap is kept.
fn thread_cap(inherited: Option<&str>) -> String {
    inherited
        .and_then(|value| value.trim().parse::<u32>().ok())
        .filter(|value| *value >= 1 && *value < VOSK_THREAD_CAP)
        .unwrap_or(VOSK_THREAD_CAP)
        .to_string()
}

/// Cap the Vosk child's math-library pools on the child's own environment.
fn cap_vosk_threads(command: &mut std::process::Command) {
    for name in VOSK_THREAD_VARS {
        command.env(name, thread_cap(std::env::var(name).ok().as_deref()));
    }
}

pub fn child_environment(
    inherited: impl Iterator<Item = (String, String)>,
) -> Vec<(String, String)> {
    const KEEP: [&str; 11] = [
        "PATH",
        "HOME",
        "USERPROFILE",
        "TEMP",
        "TMP",
        "SystemRoot",
        "windir",
        "ComSpec",
        "LANG",
        "LC_ALL",
        "LC_CTYPE",
    ];
    inherited
        .filter(|(key, _)| KEEP.iter().any(|keep| key.eq_ignore_ascii_case(keep)))
        .collect()
}

// ── Provider mode: one HTTPS request ────────────────────────────────────────

#[derive(Clone)]
pub struct ProviderJob {
    inner: Arc<ProviderInner>,
}

struct ProviderInner {
    result: Mutex<mpsc::Receiver<Result<String, String>>>,
    cancelled: Arc<AtomicBool>,
}

impl ProviderJob {
    /// Start the request. The endpoint is re-validated here, at the only
    /// place a request is built; the key exists only inside the request.
    pub fn start(endpoint: &str, model: &str, api_key: &str, wav: &[u8]) -> Result<Self, String> {
        let url = super::endpoint::validate(endpoint)?;
        let boundary = format!(
            "gentle-dictation-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let body = core::multipart_body(&boundary, &[("model", model)], wav);
        let url = url.to_string();
        let api_key = api_key.to_string();
        let (tx, rx) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = cancelled.clone();
        std::thread::Builder::new()
            .name("gentle-dictation-provider".into())
            .spawn(move || {
                // A current-thread runtime owned by this worker alone: no
                // global networking change, dropped with the thread.
                let result = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime.block_on(async move {
                        let task = tokio::spawn(send_provider(url, boundary, api_key, body));
                        // Cancellation wins within CANCEL_GRANULARITY: the
                        // losing future is dropped, closing its sockets.
                        let mut ticker = tokio::time::interval(CANCEL_GRANULARITY);
                        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                        tokio::select! {
                            result = task => result
                                .unwrap_or_else(|_| Err(CANCELLED.to_string())),
                            _ = async {
                                loop {
                                    ticker.tick().await;
                                    if worker_cancelled.load(Ordering::SeqCst) {
                                        break;
                                    }
                                }
                            } => Err(CANCELLED.to_string()),
                        }
                    }),
                    Err(_) => Err("The transcription request could not start.".to_string()),
                };
                let _ = tx.send(result);
            })
            .map_err(|_| "The transcription request could not start.".to_string())?;
        Ok(Self {
            inner: Arc::new(ProviderInner {
                result: Mutex::new(rx),
                cancelled,
            }),
        })
    }

    /// Cancel the request. Within 25 ms the request future is dropped with
    /// its connection pool and sockets — promptly on every platform. The
    /// upload stops where the transport can stop it; audio already sent
    /// cannot be recalled, and nothing the server answers is used.
    pub fn cancel(&self) {
        self.inner.cancelled.store(true, Ordering::SeqCst);
    }

    /// Wait for the answer, bounded by `timeout`. Runs on a background
    /// thread: it blocks.
    pub fn wait(&self, timeout: Duration) -> Result<String, String> {
        let deadline = Instant::now() + timeout;
        let remaining = deadline.saturating_duration_since(Instant::now());
        let result = match self
            .inner
            .result
            .lock()
            .expect("the provider result channel")
            .recv_timeout(remaining)
        {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.cancel();
                return Err("The transcription service took too long to answer.".to_string());
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("The transcription request ended without an answer.".to_string());
            }
        };
        // A cancelled request reports exactly that, never a transcript.
        if self.inner.cancelled.load(Ordering::SeqCst) {
            return Err(CANCELLED.to_string());
        }
        result
    }
}

impl Drop for ProviderJob {
    fn drop(&mut self) {
        if Arc::strong_count(&self.inner) == 1 {
            self.cancel();
        }
    }
}

/// The request itself: no redirects, one bounded streamed read, and errors
/// that name the step without quoting the server, its body or its headers.
async fn send_provider(
    url: String,
    boundary: String,
    api_key: String,
    body: Vec<u8>,
) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(PROVIDER_TIMEOUT)
        .build()
        .map_err(|_| "The transcription request could not be built.".to_string())?;
    let response = client
        .post(&url)
        .header("Authorization", format!("Bearer {api_key}"))
        .header(
            "Content-Type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(body)
        .send()
        .await
        .map_err(|_| {
            "The transcription service could not be reached. Check the endpoint and your \
             connection."
                .to_string()
        })?;
    if !response.status().is_success() {
        return Err(
            "The transcription service refused the recording. Check the endpoint and model \
             in Configuration → Dictation."
                .to_string(),
        );
    }
    let mut response = response;
    // Stream the answer with a hard bound: more than the limit is refused,
    // never buffered.
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "The transcription service answered unreadably.".to_string())?
    {
        if bytes.len() as u64 + chunk.len() as u64 > PROVIDER_BODY_LIMIT {
            return Err(format!(
                "The transcription service answered more than {PROVIDER_BODY_LIMIT} bytes."
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    let text = String::from_utf8_lossy(&bytes);
    let json: serde_json::Value = serde_json::from_str(&text)
        .map_err(|_| "The transcription service answered without JSON.".to_string())?;
    core::parse_provider_text(&json)
        .ok_or_else(|| "The transcription service answered without a transcript.".to_string())
}

#[cfg(test)]
mod tests {
    use super::{LocalJob, ProviderJob, child_environment, drain_bounded};
    use gentle_core::dictation::{Language, Local, LocalEngine, wav_16k};
    use std::time::{Duration, Instant};

    fn local_at(exe: &std::path::Path) -> Local {
        Local {
            engine: LocalEngine::Whisper,
            executable: exe.display().to_string(),
            model: "model.bin".into(),
            language: Language::Auto,
        }
    }

    /// A whisper-cli stand-in as a POSIX shell script: only `[`, no Bash
    /// assumptions (the earlier `[[` broke under `/bin/sh`).
    #[cfg(unix)]
    fn write_fake_sh(dir: &std::path::Path, body: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let exe = dir.join("whisper-cli");
        std::fs::write(&exe, format!("#!/bin/sh\n{body}")).unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        exe
    }

    /// The same stand-in for Windows, where the app would spawn a `.cmd`.
    #[cfg(windows)]
    fn write_fake_cmd(dir: &std::path::Path, body: &str) -> std::path::PathBuf {
        let exe = dir.join("whisper-cli.cmd");
        std::fs::write(
            &exe,
            format!("@echo off\nsetlocal enabledelayedexpansion\n{body}"),
        )
        .unwrap();
        exe
    }

    /// The argument loop both fakes share: find `--output-file <base>`.
    #[cfg(unix)]
    const ARG_LOOP: &str = r#"out=""
while [ $# -gt 0 ]; do
  if [ "$1" = "--output-file" ]; then out="$2.txt"; fi
  shift
done
"#;
    #[cfg(windows)]
    const ARG_LOOP: &str = r#"set "out="
:loop
if "%~1"=="" goto done
if /i "%~1"=="--output-file" set "out=%~2.txt"
shift
goto loop
:done
"#;

    #[test]
    fn local_transcription_reports_a_missing_executable_before_running() {
        let local = Local {
            engine: LocalEngine::Whisper,
            executable: "no-such-whisper-cli-anywhere".into(),
            model: "ggml-base.bin".into(),
            language: Language::Auto,
        };
        let Err(error) = LocalJob::start(&local, &wav_16k(&[0; 100])) else {
            panic!("a missing executable must not start a job");
        };
        assert!(error.contains("could not be found"), "{error}");
    }

    #[test]
    fn local_transcription_runs_the_user_cli_and_reads_its_txt() {
        let dir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        let exe = write_fake_sh(
            dir.path(),
            &format!("{ARG_LOOP}printf '  hola, mundo \\n' > \"$out\"\n"),
        );
        #[cfg(windows)]
        let exe = write_fake_cmd(
            dir.path(),
            &format!("{ARG_LOOP}> \"!out!\" echo hola, mundo\n"),
        );
        let job = LocalJob::spawn_whisper(&local_at(&exe), &wav_16k(&[0; 100]), std::env::vars())
            .unwrap();
        assert_eq!(job.wait(Duration::from_secs(60)).unwrap(), "hola, mundo");
    }

    #[test]
    fn local_transcription_reports_failure_without_child_output() {
        let dir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        let exe = write_fake_sh(
            dir.path(),
            &format!("{ARG_LOOP}echo secret-path /home/me >&2\nexit 3\n"),
        );
        #[cfg(windows)]
        let exe = write_fake_cmd(
            dir.path(),
            &format!("{ARG_LOOP}echo secret-path C:\\Users\\me 1>&2\nexit /b 3\n"),
        );
        let job = LocalJob::spawn_whisper(&local_at(&exe), &wav_16k(&[0; 100]), std::env::vars())
            .unwrap();
        let error = job.wait(Duration::from_secs(60)).unwrap_err();
        assert!(!error.contains("secret-path"), "{error}");
        // Byte-identical to the message Whisper always produced: the engine
        // labels changed the plumbing, not what a Whisper user reads.
        assert_eq!(
            error,
            "whisper-cli could not transcribe the recording. Check the executable and model              paths in Configuration → Dictation."
        );
    }

    /// The child inherits the platform runtime variables, never the rest of
    /// the session's environment: the fake CLI answers with what it saw.
    #[test]
    fn local_child_gets_runtime_variables_but_not_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let env = vec![
            (
                "PATH".to_string(),
                std::env::var("PATH").unwrap_or_default(),
            ),
            ("GENTLE_TEST_SECRET".to_string(), "leaked-key".to_string()),
            (
                "GENTLE_TRANSCRIBE_API_KEY".to_string(),
                "leaked-too".to_string(),
            ),
        ];
        #[cfg(unix)]
        let exe = write_fake_sh(
            dir.path(),
            &format!(
                "{ARG_LOOP}if [ -n \"$GENTLE_TEST_SECRET\" ] || [ -n \"$GENTLE_TRANSCRIBE_API_KEY\" ]; then echo leaked; else echo stripped; fi > \"$out\"\n"
            ),
        );
        #[cfg(windows)]
        let exe = write_fake_cmd(
            dir.path(),
            &format!(
                "{ARG_LOOP}if defined GENTLE_TEST_SECRET (echo leaked>\"!out!\") else (echo stripped>\"!out!\")\n"
            ),
        );
        let job = LocalJob::spawn_whisper(
            &local_at(&exe),
            &wav_16k(&[0; 100]),
            // The same filtering `LocalJob::start` does with the process env.
            child_environment(env.into_iter()).into_iter(),
        )
        .unwrap();
        assert_eq!(job.wait(Duration::from_secs(60)).unwrap(), "stripped");
    }

    #[test]
    fn child_environment_keeps_the_runtime_and_drops_the_rest() {
        let env = [
            ("PATH", "/usr/bin"),
            ("SystemRoot", r"C:\Windows"),
            ("systemroot", r"C:\Windows"),
            ("TEMP", r"C:\Temp"),
            ("GENTLE_TRANSCRIBE_API_KEY", "sk-secret"),
            ("AWS_SESSION_TOKEN", "tok"),
        ]
        .map(|(k, v)| (k.to_string(), v.to_string()));
        let kept = child_environment(env.into_iter());
        let names: Vec<&str> = kept.iter().map(|(k, _)| k.as_str()).collect();
        assert!(names.iter().any(|n| n.eq_ignore_ascii_case("path")));
        // Both spellings pass through: Windows matches variable names
        // case-insensitively, and a real process env has no dupes.
        assert_eq!(
            kept.iter()
                .filter(|(k, _)| k.eq_ignore_ascii_case("systemroot"))
                .count(),
            2
        );
        assert!(
            !names
                .iter()
                .any(|n| n.contains("GENTLE") || n.contains("AWS"))
        );
    }

    /// A transcript larger than the bound is refused outright, never
    /// silently truncated: dictated words must not be lost invisibly.
    #[cfg(unix)]
    #[test]
    fn an_oversized_transcript_is_refused_not_truncated() {
        let dir = tempfile::tempdir().unwrap();
        let exe = write_fake_sh(
            dir.path(),
            &format!(
                "{ARG_LOOP}yes palabra | head -c 200000 > \"$out\"
"
            ),
        );
        let job = LocalJob::spawn_whisper(&local_at(&exe), &wav_16k(&[0; 100]), std::env::vars())
            .unwrap();
        let error = job.wait(Duration::from_secs(60)).unwrap_err();
        assert!(error.contains("too large"), "{error}");
    }

    /// On every platform a cancelled provider request must stop promptly:
    /// the server observes its connection close, and the wait resolves
    /// with the cancellation, never with a transcript or a long timeout.
    #[test]
    fn cancelling_a_provider_job_closes_the_connection_promptly() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (close_tx, close_rx) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            // Hold the connection open, answering nothing, until the peer
            // hangs up; measure how long that takes.
            let started = Instant::now();
            let mut socket = socket;
            let mut leftover = Vec::new();
            let _ = std::io::Read::read_to_end(&mut socket, &mut leftover);
            let _ = close_tx.send(started.elapsed());
        });
        let url = format!("http://127.0.0.1:{port}/transcribe");
        let job = ProviderJob::start(&url, "whisper-1", "test-key", &wav_16k(&[0; 100])).unwrap();
        let watcher = job.clone();
        std::thread::sleep(Duration::from_millis(300));
        let started = Instant::now();
        job.cancel();
        let error = watcher.wait(Duration::from_secs(60)).unwrap_err();
        assert_eq!(error, "The transcription was cancelled.");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the wait must resolve promptly, took {:?}",
            started.elapsed()
        );
        let close_after = close_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the server to see the connection end");
        assert!(
            close_after < Duration::from_secs(5),
            "the cancelled upload's socket must close promptly, took {close_after:?}"
        );
        server.join().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn cancelling_a_local_job_kills_and_reaps_the_child_promptly() {
        let dir = tempfile::tempdir().unwrap();
        let exe = write_fake_sh(dir.path(), "exec sleep 30\n");
        let job = LocalJob::spawn_whisper(&local_at(&exe), &wav_16k(&[0; 100]), std::env::vars())
            .unwrap();
        let watcher = job.clone();
        let started = Instant::now();
        job.cancel();
        // The killed job's wait ends promptly with the cancellation.
        let error = watcher.wait(Duration::from_secs(30)).unwrap_err();
        assert_eq!(error, "The transcription was cancelled.");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "kill was prompt"
        );
    }

    #[cfg(windows)]
    #[test]
    fn cancelling_a_local_job_kills_and_reaps_the_child_promptly() {
        let dir = tempfile::tempdir().unwrap();
        let exe = write_fake_cmd(dir.path(), "ping -n 31 127.0.0.1 >nul\n");
        let job = LocalJob::spawn_whisper(&local_at(&exe), &wav_16k(&[0; 100]), std::env::vars())
            .unwrap();
        let watcher = job.clone();
        let started = Instant::now();
        job.cancel();
        let error = watcher.wait(Duration::from_secs(30)).unwrap_err();
        assert_eq!(error, "The transcription was cancelled.");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "kill was prompt"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_local_job_past_its_deadline_is_killed() {
        let dir = tempfile::tempdir().unwrap();
        let exe = write_fake_sh(dir.path(), "exec sleep 30\n");
        let job = LocalJob::spawn_whisper(&local_at(&exe), &wav_16k(&[0; 100]), std::env::vars())
            .unwrap();
        let started = Instant::now();
        let error = job.wait(Duration::from_millis(300)).unwrap_err();
        assert!(error.contains("five minutes"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[cfg(windows)]
    #[test]
    fn a_local_job_past_its_deadline_is_killed() {
        let dir = tempfile::tempdir().unwrap();
        let exe = write_fake_cmd(dir.path(), "ping -n 31 127.0.0.1 >nul\n");
        let job = LocalJob::spawn_whisper(&local_at(&exe), &wav_16k(&[0; 100]), std::env::vars())
            .unwrap();
        let started = Instant::now();
        let error = job.wait(Duration::from_millis(300)).unwrap_err();
        assert!(error.contains("five minutes"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    /// A fake `vosk` module that also floods stdout: the real helper must
    /// still finish, because the app drains the pipes bounded instead of
    /// letting a chatty child block on a full pipe.
    const FAKE_VOSK_MODULE: &str = r#"
import json
for _ in range(512):
    print("x" * 1024)
class KaldiRecognizer:
    def __init__(self, model, rate):
        self.steps = 0
        self._last = '{"text": ""}'
    def AcceptWaveform(self, chunk):
        self.steps += 1
        self._last = '{"text": "chunk %d"}' % self.steps
        return True
    def Result(self):
        return self._last
    def FinalResult(self):
        return json.dumps({"text": "final words"})
class Model:
    def __init__(self, path):
        pass
def SetLogLevel(level):
    pass
"#;

    /// A throwaway venv with the fake module installed in its site-packages.
    /// `-I` disables PYTHONPATH and user site, so this is the only way to
    /// exercise the PRODUCTION invocation (`python -I -B helper ...`) with a
    /// fake engine.
    fn venv_with_fake_vosk(dir: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
        use std::process::Command;
        let venv = dir.join("venv");
        let made = Command::new("python")
            .args(["-I", "-B", "-m", "venv", "--without-pip"])
            .arg(&venv)
            .status()
            .expect("python is available for tests");
        assert!(made.success(), "the test venv could not be created");
        let python = if cfg!(windows) {
            venv.join("Scripts").join("python.exe")
        } else {
            venv.join("bin").join("python")
        };
        assert!(python.is_file(), "{} is missing", python.display());
        let output = Command::new(&python)
            .args([
                "-I",
                "-c",
                "import sysconfig; print(sysconfig.get_paths()['purelib'])",
            ])
            .output()
            .unwrap();
        assert!(output.status.success());
        let site = std::path::PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
        std::fs::create_dir_all(&site).unwrap();
        std::fs::write(site.join("vosk.py"), FAKE_VOSK_MODULE).unwrap();
        (python, site)
    }

    #[test]
    fn the_vosk_helper_runs_as_production_does_and_a_chatty_child_cannot_block() {
        use std::process::Command;
        let dir = tempfile::tempdir().unwrap();
        let (python, _site) = venv_with_fake_vosk(dir.path());
        let helper = dir.path().join("vosk_transcribe.py");
        std::fs::write(&helper, super::VOSK_HELPER).unwrap();
        let model = dir.path().join("model");
        std::fs::create_dir_all(&model).unwrap();
        let wav = dir.path().join("recording.wav");
        std::fs::write(&wav, make_wav_bytes()).unwrap();

        // 1) The exact production command line, `-I -B` included.
        let output = dir.path().join("transcript.txt");
        let status = Command::new(&python)
            .args(["-I", "-B"])
            .arg(&helper)
            .arg("--model")
            .arg(&model)
            .arg("--input")
            .arg(&wav)
            .arg("--output")
            .arg(&output)
            .output()
            .expect("the test venv runs the helper");
        assert!(
            status.status.success(),
            "{:?}",
            String::from_utf8_lossy(&status.stderr)
        );
        assert_eq!(
            std::fs::read_to_string(&output).unwrap().trim(),
            "chunk 1 chunk 2 chunk 3 final words",
            "every chunk plus the final result is collected"
        );

        // 2) A wrong WAV is refused with its own bounded message.
        let bad = dir.path().join("bad.wav");
        std::fs::write(&bad, b"not a wav").unwrap();
        let refused = Command::new(&python)
            .args(["-I", "-B"])
            .arg(&helper)
            .arg("--model")
            .arg(&model)
            .arg("--input")
            .arg(&bad)
            .arg("--output")
            .arg(dir.path().join("bad.txt"))
            .output()
            .unwrap();
        assert!(!refused.status.success());
        let stdout = String::from_utf8_lossy(&refused.stdout);
        let report = stdout
            .lines()
            .find(|line| line.starts_with("{\"error\""))
            .unwrap_or_else(|| panic!("no structured failure line in {stdout:.200}"));
        assert!(
            report.contains("could not be read") || report.contains("not 16 kHz mono"),
            "{report}"
        );
        assert!(
            report.len() < 4096,
            "the failure report stays bounded: {report}"
        );
        assert!(
            !report.contains(dir.path().to_str().unwrap()) && !report.contains("bad.wav"),
            "the failure report never echoes paths: {report}"
        );

        // 3) The same helper through the real job: 512 KiB on stdout must not
        // block it, and the transcript still arrives.
        let local = Local {
            engine: LocalEngine::Vosk,
            executable: python.display().to_string(),
            model: model.display().to_string(),
            language: Language::Auto,
        };
        let job = LocalJob::start(&local, &make_wav_bytes()).unwrap();
        let text = job.wait(Duration::from_secs(60)).unwrap();
        assert_eq!(text, "chunk 1 chunk 2 chunk 3 final words");
    }

    /// The Vosk child must not size its math-library pools from the whole
    /// machine, and a failing Vosk child must report itself as Vosk. The fake
    /// child dumps the three caps and exits non-zero, so both are observed
    /// through the public job interface, never through internals.
    #[cfg(windows)]
    #[test]
    fn the_vosk_child_is_capped_and_a_failure_names_vosk_not_whisper() {
        let dir = tempfile::tempdir().unwrap();
        let dump = dir.path().join("env.txt");
        let exe = write_fake_cmd(
            dir.path(),
            &format!(
                "> \"{d}\" echo OMP_CAP=%OMP_NUM_THREADS%
                 >> \"{d}\" echo MKL_CAP=%MKL_NUM_THREADS%
                 >> \"{d}\" echo BLAS_CAP=%OPENBLAS_NUM_THREADS%
                 exit /b 1
",
                d = dump.display()
            ),
        );
        assert_vosk_child(dump, &exe);
    }

    #[cfg(unix)]
    #[test]
    fn the_vosk_child_is_capped_and_a_failure_names_vosk_not_whisper() {
        let dir = tempfile::tempdir().unwrap();
        let dump = dir.path().join("env.txt");
        let exe = write_fake_sh(
            dir.path(),
            &format!(
                "printf 'OMP_CAP=%s\nMKL_CAP=%s\nBLAS_CAP=%s\n'                  \"$OMP_NUM_THREADS\" \"$MKL_NUM_THREADS\" \"$OPENBLAS_NUM_THREADS\"                  > '{d}'
exit 1
",
                d = dump.display()
            ),
        );
        assert_vosk_child(dump, &exe);
    }

    /// A cap is a ceiling: a lower value already in the environment stands.
    #[test]
    fn the_thread_cap_never_raises_a_lower_inherited_value() {
        assert_eq!(super::thread_cap(None), "2");
        assert_eq!(super::thread_cap(Some("1")), "1");
        assert_eq!(super::thread_cap(Some("16")), "2");
        assert_eq!(super::thread_cap(Some("0")), "2");
        assert_eq!(super::thread_cap(Some("many")), "2");
    }

    fn assert_vosk_child(dump: std::path::PathBuf, exe: &std::path::Path) {
        let dir = exe.parent().unwrap().to_path_buf();
        let local = Local {
            engine: LocalEngine::Vosk,
            executable: exe.display().to_string(),
            model: dir.join("model").display().to_string(),
            language: Language::Auto,
        };
        let job = LocalJob::start(&local, &make_wav_bytes()).unwrap();
        let error = job.wait(Duration::from_secs(60)).unwrap_err();
        assert!(error.contains("Vosk"), "a Vosk failure names Vosk: {error}");
        assert!(!error.to_lowercase().contains("whisper"), "{error}");
        let dumped = String::from_utf8_lossy(&std::fs::read(&dump).unwrap()).to_string();
        for marker in ["OMP_CAP=", "MKL_CAP=", "BLAS_CAP="] {
            let value = dumped
                .lines()
                .find_map(|line| line.trim().strip_prefix(marker))
                .unwrap_or_else(|| panic!("{marker} missing from the child env: {dumped}"));
            let cap: u32 = value
                .trim()
                .parse()
                .unwrap_or_else(|_| panic!("{marker}{value:?} is not a number: {dumped}"));
            assert!((1..=2).contains(&cap), "{marker}{cap} must stay <= 2");
        }
    }

    fn make_wav_bytes() -> Vec<u8> {
        // 16 kHz mono 16-bit PCM, a handful of samples.
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&((36 + 18000u32).to_le_bytes()));
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&16000u32.to_le_bytes());
        wav.extend_from_slice(&32000u32.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(18000u32).to_le_bytes());
        for frame in 0..9000i16 {
            wav.extend_from_slice(&frame.to_le_bytes());
        }
        wav
    }

    #[test]
    fn drain_bounded_keeps_at_most_the_cap_and_consumes_the_pipe() {
        let source = vec![7u8; 200 * 1024];
        let kept = drain_bounded(source.as_slice());
        assert!(kept.len() <= gentle_core::dictation::MAX_TEXT_BYTES);
        assert!(kept.iter().all(|b| *b == 7));
    }

    /// An insecure endpoint away from the loopback is rejected before any
    /// request is built or sent.
    #[test]
    fn provider_refuses_an_insecure_remote_without_sending() {
        assert!(ProviderJob::start("http://example.com/transcribe", "m", "k", &[0; 4]).is_err());
    }
}
