//! Voice dictation for the composer: the per-tab state machine, the app
//! settings it reads, and the capture and transcription effects it drives.
//! The pure rules (configuration validation, WAV conversion, transcripts)
//! live in `gentle_core::dictation`.
//!
//! The validated configuration is snapshotted when the recording starts:
//! a change in Configuration (even to Off, or from Local to Provider)
//! never re-routes a recording that is already being taken.

mod capture;
pub mod endpoint;
pub mod setup;
mod transcribe;

pub use capture::{Live, STARTUP_BUDGET, start_capture_opening};
pub use transcribe::{LOCAL_TIMEOUT, LocalJob, PROVIDER_TIMEOUT, ProviderJob};

use std::path::Path;

use gentle_core::dictation::{Local, LocalEngine, Mode, Provider, Settings};

/// The app's `settings.json` key.
const KEY: &str = "dictation";

/// The saved dictation configuration, whatever its validity: Configuration
/// shows it so the user can fix it; the mic button only uses validated ones.
pub fn read_config_raw() -> Settings {
    crate::shared::themes::store_path()
        .and_then(|path| crate::shared::themes::read_key(&path, KEY))
        .map(|value| Settings::from_json(&value))
        .unwrap_or_default()
}

/// The configuration, ready to transcribe: an error names what to fix.
pub fn read_config() -> Result<Settings, String> {
    let settings = read_config_raw();
    settings.validate()?;
    Ok(settings)
}

/// Save the Configuration screen's edits: only this feature's key is
/// touched, every other key of the file is preserved as it was read, and
/// the write is atomic (temp file, then rename). The API key is never part
/// of the file: only its environment variable's name is. Errors never
/// carry paths or file contents.
pub fn write_config(settings: &Settings) -> Result<(), String> {
    let Some(path) = crate::shared::themes::store_path() else {
        return Err("The app settings folder could not be found.".to_string());
    };
    if let Err(error) = write_key_atomic(&path, KEY, settings.to_json()) {
        let reason = match error.kind() {
            std::io::ErrorKind::PermissionDenied => "the file is not writable",
            std::io::ErrorKind::NotFound => "the settings folder is missing",
            _ => "the file could not be written",
        };
        return Err(format!(
            "The dictation settings could not be saved: {reason}."
        ));
    }
    Ok(())
}

/// Set one key in a JSON file atomically, preserving every other key as
/// it was read. A malformed or unreadable (non-missing) file is rejected
/// with no mutation at all: the user's existing settings are too valuable
/// to replace with defaults. This serializes writes made on this app's
/// main thread (the Settings UI's click, like the theme pickers'), but it
/// is not a cross-owner lock: another process or thread writing the same
/// file between the read and the rename still wins last.
/// (`themes::write_key` stays untouched.)
fn write_key_atomic(
    path: &std::path::Path,
    key: &str,
    value: serde_json::Value,
) -> std::io::Result<()> {
    let root: serde_json::Value = match std::fs::read(path) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(value @ serde_json::Value::Object(_)) => value,
            _ => {
                return Err(std::io::Error::other(
                    "the existing settings file is not valid JSON",
                ));
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            serde_json::json!({})
        }
        Err(error) => return Err(error),
    };
    let mut root = root;
    root[key] = value;
    let text = serde_json::to_string_pretty(&root).map_err(std::io::Error::other)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // A private, uniquely named temp file in the same folder, persisted
    // over the target with an atomic replace: no fixed temp name to collide.
    let mut temp = tempfile::NamedTempFile::new_in(path.parent().unwrap_or(path))?;
    use std::io::Write as _;
    temp.write_all(text.as_bytes())?;
    temp.write_all(b"\n")?;
    temp.persist(path).map_err(|error| error.error)?;
    Ok(())
}

/// The Configuration form (mode, engine, edited fields) mapped onto the
/// saved settings: the SAVE path, pure so the engine choice can be tested
/// without a window. The form hands over the same shapes the settings keep,
/// and every text field is trimmed; the chosen engine is serialized
/// verbatim - a Vosk form saves Vosk, never Whisper.
pub fn form_settings(mode: Mode, local: Local, provider: Provider) -> Settings {
    Settings {
        mode,
        local: Local {
            executable: local.executable.trim().to_string(),
            model: local.model.trim().to_string(),
            ..local
        },
        provider: Provider {
            endpoint: provider.endpoint.trim().to_string(),
            model: provider.model.trim().to_string(),
            api_key_env: provider.api_key_env.trim().to_string(),
        },
    }
}

/// Everything that must hold before the microphone may open, checked
/// before recording starts: the mode's fields (validate) and, beyond
/// that, the files a local setup points at and the environment variable a
/// provider needs. The user learns what is missing before saying a word.
pub fn ensure_ready(config: &Settings) -> Result<(), String> {
    config.validate()?;
    match config.mode {
        Mode::Off => Err(
            "Dictation is off. Choose Local or Provider in Configuration → Dictation.".to_string(),
        ),
        Mode::Local => {
            let executable = Path::new(config.local.executable.trim());
            let model = Path::new(config.local.model.trim());
            match config.local.engine {
                LocalEngine::Vosk => {
                    if !executable.is_file() {
                        return Err(format!(
                            "The Vosk Python was not found ({}). Run the Vosk setup or point \
                             Dictation at the right path in Configuration → Dictation.",
                            config.local.executable
                        ));
                    } else if !model.is_dir() {
                        return Err(format!(
                            "The Vosk model directory was not found ({}). Download the model \
                             or point Dictation at the right path in Configuration →
                             Dictation.",
                            config.local.model
                        ));
                    }
                    Ok(())
                }
                LocalEngine::Whisper => config
                    .local_blockers(executable.exists(), model.exists())
                    .into_iter()
                    .next()
                    .map_or(Ok(()), Err),
            }
        }
        Mode::Provider => {
            let env_set = std::env::var(config.provider.api_key_env.trim())
                .ok()
                .is_some_and(|value| !value.trim().is_empty());
            config.provider_blocker(env_set).map_or(Ok(()), Err)
        }
    }
}

/// Whether Configuration should lead with the guided setup: the exact
/// readiness predicate the mic enforces, so a provider key whose NAME is
/// configured but whose value is missing from the environment gets the
/// same help a mic click would give. Rendering never changes config.
pub fn setup_needed(config: &Settings) -> bool {
    ensure_ready(config).is_err()
}

/// The mic button's phases.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Idle,
    /// The microphone is opening off the GUI thread. The configuration is
    /// already reserved; a cancel during opening bumps the generation, so
    /// the device, once opened, is closed again instead of recording.
    Opening,
    Recording,
    Transcribing,
}

/// One tab's dictation. The validated configuration is snapshotted at
/// `begin`; every superseding start or cancel bumps the generation, so a
/// late result never lands in the composer; and `cancel` also runs the
/// transcription's cancel hook, so the child dies and the request's
/// transport shuts down instead of running on behind a discarded result.
pub struct State {
    phase: Phase,
    recording: Option<Box<dyn Live>>,
    /// The configuration the recording and its transcription will use:
    /// snapshotted, never re-read.
    config: Option<Settings>,
    /// Cancels the in-flight transcription (kill the child, shut the
    /// transport down), installed by the session when the job starts.
    transcribing: Option<Box<dyn FnOnce() + Send>>,
    /// Cancels the microphone opening (close the device, discard the
    /// buffer), installed the moment the opening starts so Esc, a restart
    /// or a drop reach the keeper thread without waiting for readiness.
    opening_cancel: Option<Box<dyn Fn() + Send>>,
    /// The mode the recording was started in (its status text says where
    /// the audio will go when it stops).
    provider: bool,
    generation: u64,
}

impl Default for State {
    fn default() -> Self {
        Self {
            phase: Phase::Idle,
            recording: None,
            config: None,
            transcribing: None,
            opening_cancel: None,
            provider: false,
            generation: 0,
        }
    }
}

impl State {
    pub fn phase(&self) -> Phase {
        self.phase
    }

    pub fn idle(&self) -> bool {
        self.phase == Phase::Idle
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Whether a result from this generation may still land.
    pub fn accepts(&self, generation: u64) -> bool {
        self.generation == generation
    }

    /// The configuration snapshot the recording runs with.
    pub fn config(&self) -> Option<&Settings> {
        self.config.as_ref()
    }

    /// Whether the recording goes to a provider when it stops.
    pub fn provider_recording(&self) -> bool {
        self.provider && self.phase == Phase::Recording
    }

    /// The microphone already stopped on its own: the cap was reached.
    pub fn capped(&self) -> bool {
        self.recording.as_ref().is_some_and(|live| live.capped())
    }

    /// Reserve the next recording: `config` is snapshotted now, before the
    /// microphone is even opened, and the returned generation is what the
    /// opening must still match to matter. Off is refused (and never
    /// sticks); any previous recording or transcription is cancelled first
    /// (its cancel hook runs).
    pub fn reserve_opening(&mut self, config: Settings) -> Result<u64, String> {
        if config.mode == Mode::Off {
            return Err(
                "Dictation is off. Choose Local or Provider in Configuration → Dictation."
                    .to_string(),
            );
        }
        self.cancel();
        self.phase = Phase::Opening;
        self.provider = config.mode == Mode::Provider;
        self.config = Some(config);
        self.generation += 1;
        Ok(self.generation)
    }

    /// The microphone opened. It starts recording only when this opening
    /// is still the reserved one; otherwise `live` is dropped here, which
    /// closes the device, and nothing begins.
    pub fn begin(&mut self, epoch: u64, live: Box<dyn Live>) -> bool {
        if self.phase != Phase::Opening || self.generation != epoch {
            return false; // `live` drops: the microphone closes unused
        }
        // The opening is over: its cancel handle detaches uncalled (the
        // live recording carries its own).
        self.opening_cancel = None;
        self.phase = Phase::Recording;
        self.recording = Some(live);
        true
    }

    /// The microphone refused to open. Its error is reported only when this
    /// opening is still current; a superseded one is silently gone. `None`
    /// means the caller has nothing left to do.
    pub fn opening_failed(&mut self, epoch: u64, message: String) -> Option<String> {
        if self.phase != Phase::Opening || self.generation != epoch {
            return None;
        }
        // The keeper may still be waiting: cancel it for this epoch.
        if let Some(cancel) = self.opening_cancel.take() {
            cancel();
        }
        self.phase = Phase::Idle;
        self.config = None;
        self.provider = false;
        Some(message)
    }

    /// Stop capturing; the audio to transcribe, and whether the recording
    /// was cut at its time limit. The phase moves to `Transcribing`; a
    /// failed stop leaves the caller to cancel.
    pub fn stop_recording(&mut self) -> Result<(Vec<i16>, bool), String> {
        let live = self
            .recording
            .take()
            .ok_or_else(|| "Nothing is recording.".to_string())?;
        self.phase = Phase::Transcribing;
        let finished = live.finish();
        if let Some(error) = finished.error {
            self.cancel();
            return Err(error);
        }
        Ok((finished.audio, finished.hit_limit))
    }

    /// A transcription just started: register how to cancel it (kill the
    /// child, shut the transport down) so cancel, tab close and restart
    /// reach the job itself, not just a discarded result.
    pub fn transcribing(&mut self, cancel: impl FnOnce() + Send + 'static) {
        self.transcribing = Some(Box::new(cancel));
    }

    /// The microphone opening just started: register how to cancel it.
    /// The keeper thread may already be opening the device (the spawn
    /// races ahead of this attach), so a cancel BEFORE the attach cannot
    /// be signaled; from the attach on, the keeper re-checks the token
    /// after its OS open and at every poll, so a cancel always lands.
    /// Held until recording begins (detached, uncalled) or the opening
    /// fails/cancels (called once).
    pub fn attach_opening(&mut self, cancel: impl Fn() + Send + 'static) {
        self.opening_cancel = Some(Box::new(cancel));
    }

    /// A transcription finished and its result was handled: back to idle
    /// without superseding anything.
    pub fn settle(&mut self) {
        self.transcribing = None;
        self.opening_cancel = None;
        self.phase = Phase::Idle;
        self.config = None;
        self.provider = false;
    }

    /// Stop everything and make any in-flight result stale. The
    /// transcription is actually cancelled — its cancel hook is taken and
    /// run exactly once, whatever else was going on — and the composer's
    /// draft is never touched. Cancelling when idle does nothing: Esc's
    /// other jobs keep their order.
    pub fn cancel(&mut self) {
        let recording = self.recording.take().is_some();
        let opening = self.opening_cancel.take();
        let hook = self.transcribing.take();
        let was_active = recording
            || hook.is_some()
            || matches!(
                self.phase,
                Phase::Opening | Phase::Recording | Phase::Transcribing
            );
        if let Some(opening) = opening {
            opening();
        }
        if let Some(hook) = hook {
            hook();
        }
        if was_active {
            self.generation += 1;
        }
        self.phase = Phase::Idle;
        self.config = None;
        self.provider = false;
    }
}

impl Drop for State {
    /// A dropped tab cancels whatever was going on: the microphone closes
    /// and the job's cancel hook runs, once.
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::capture::Live;
    use super::{KEY, Phase, State, write_key_atomic};
    use gentle_core::dictation::{Language, Local, LocalEngine, Mode, Provider, Settings};

    fn local_settings() -> Settings {
        Settings {
            mode: Mode::Local,
            local: Local {
                engine: LocalEngine::Whisper,
                executable: "/opt/whisper-cli".into(),
                model: "/opt/models/ggml-base.bin".into(),
                language: Language::Spanish,
            },
            ..Settings::default()
        }
    }

    fn provider_settings() -> Settings {
        Settings {
            mode: Mode::Provider,
            provider: Provider {
                endpoint: "https://example.com/transcribe".into(),
                model: "whisper-1".into(),
                api_key_env: "GENTLE_TRANSCRIBE_API_KEY".into(),
            },
            ..Settings::default()
        }
    }

    /// A recording that behaves as the test says, with no microphone.
    struct FakeLive {
        capped: bool,
        audio: Vec<i16>,
        error: Option<String>,
        /// Set when the microphone was asked to stop (finish or drop).
        stopped: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    impl FakeLive {
        fn audio() -> (Box<dyn Live>, std::sync::Arc<std::sync::atomic::AtomicBool>) {
            let stopped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            (
                Box::new(FakeLive {
                    capped: false,
                    audio: vec![1, 2, 3],
                    error: None,
                    stopped: stopped.clone(),
                }),
                stopped,
            )
        }

        fn capped() -> Box<dyn Live> {
            let stopped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            Box::new(FakeLive {
                capped: true,
                audio: vec![1, 2, 3],
                error: None,
                stopped,
            })
        }
    }

    impl super::Live for FakeLive {
        fn capped(&self) -> bool {
            self.capped
        }

        fn finish(mut self: Box<Self>) -> super::capture::FinishedRecording {
            self.stopped
                .store(true, std::sync::atomic::Ordering::SeqCst);
            super::capture::FinishedRecording {
                audio: std::mem::take(&mut self.audio),
                hit_limit: self.capped,
                error: self.error.take(),
            }
        }
    }

    impl Drop for FakeLive {
        fn drop(&mut self) {
            self.stopped
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// The session's opening flow in one step: reserve, then begin.
    fn open(
        state: &mut State,
        fake: (Box<dyn Live>, std::sync::Arc<std::sync::atomic::AtomicBool>),
    ) -> u64 {
        let epoch = state.reserve_opening(local_settings()).unwrap();
        assert!(state.begin(epoch, fake.0), "a fresh opening always begins");
        epoch
    }

    #[test]
    fn ensure_ready_diagnoses_before_the_mic_opens() {
        // Local: empty paths fail validation; existing files pass.
        let mut settings = local_settings();
        assert!(super::ensure_ready(&settings).is_err(), "empty paths");

        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("whisper-cli.exe");
        let model = dir.path().join("ggml-base.bin");
        std::fs::write(&exe, b"x").unwrap();
        std::fs::write(&model, b"m").unwrap();
        settings.local.executable = exe.display().to_string();
        settings.local.model = model.display().to_string();
        assert!(super::ensure_ready(&settings).is_ok());

        std::fs::remove_file(&model).unwrap();
        let error = super::ensure_ready(&settings).unwrap_err();
        assert!(error.contains("model file was not found"), "{error}");

        // Provider: an unset variable is diagnosed before any recording.
        let provider = provider_settings();
        assert!(super::ensure_ready(&provider).is_err());
        let error = super::ensure_ready(&provider).unwrap_err();
        assert!(
            error.contains("GENTLE_TRANSCRIBE_API_KEY is not set"),
            "{error}"
        );

        // Off never passes.
        assert!(super::ensure_ready(&Settings::default()).is_err());
    }

    #[test]
    fn the_settings_form_save_uses_the_retained_engine_choice() {
        use gentle_core::dictation::Language;
        // A Vosk form saves Vosk with a model DIRECTORY and the venv
        // Python - never silently Whisper.
        let vosk = super::form_settings(
            Mode::Local,
            Local {
                engine: LocalEngine::Vosk,
                executable: "C:/venv/python.exe".to_string(),
                model: "C:/vosk/small-es".to_string(),
                language: Language::Auto,
            },
            Provider {
                endpoint: "https://example.com".to_string(),
                model: "whisper-1".to_string(),
                api_key_env: "KEY".to_string(),
            },
        );
        assert_eq!(vosk.local.engine, LocalEngine::Vosk);
        assert_eq!(vosk.local.executable, "C:/venv/python.exe");
        assert_eq!(vosk.local.model, "C:/vosk/small-es");
        // A untouched Whisper form keeps Whisper.
        let whisper = super::form_settings(
            Mode::Local,
            Local {
                engine: LocalEngine::Whisper,
                executable: "/opt/whisper-cli".to_string(),
                model: "/m/ggml-base.bin".to_string(),
                language: Language::Auto,
            },
            Provider::default(),
        );
        assert_eq!(whisper.local.engine, LocalEngine::Whisper);
        // Fields are trimmed, not dropped.
        assert_eq!(vosk.provider.endpoint, "https://example.com");
    }

    #[test]
    fn setup_needed_follows_the_same_readiness_predicate_as_the_mic() {
        // Off: help.
        assert!(super::setup_needed(&Settings::default()));
        // Local with files that exist: no help.
        let dir = tempfile::tempdir().unwrap();
        let mut local = local_settings();
        let exe = dir.path().join("whisper-cli.exe");
        let model = dir.path().join("ggml-base.bin");
        std::fs::write(&exe, b"x").unwrap();
        std::fs::write(&model, b"m").unwrap();
        local.local.executable = exe.display().to_string();
        local.local.model = model.display().to_string();
        assert!(
            !super::setup_needed(&local),
            "a working local setup needs no help"
        );
        // Local with missing files: help.
        std::fs::remove_file(&model).unwrap();
        assert!(super::setup_needed(&local));
        // Provider with a valid name but no value in the environment: help,
        // exactly like the mic would refuse.
        let provider = provider_settings();
        assert!(super::setup_needed(&provider));
    }

    #[test]
    fn off_never_starts_and_never_sticks() {
        let mut state = State::default();
        let error = state.reserve_opening(Settings::default()).unwrap_err();
        assert!(error.contains("Dictation is off"), "{error}");
        assert!(state.idle(), "a refused start leaves nothing behind");

        // A recording started as Local keeps its snapshot even if the
        // settings switch to Off while it runs: stop keeps working.
        open(&mut state, FakeLive::audio());
        state.config = Some(Settings::default()); // settings now Off
        assert_eq!(state.phase(), Phase::Recording);
        let (audio, _) = state.stop_recording().unwrap();
        assert_eq!(audio, vec![1, 2, 3], "the snapshot still transcribes");
        state.settle();
        assert!(state.idle());
    }

    #[test]
    fn the_snapshot_survives_a_settings_change_between_start_and_stop() {
        let mut state = State::default();
        open(&mut state, FakeLive::audio());
        // The user switches to Provider (or edits anything) mid-recording;
        // the state's snapshot is what the transcription will use.
        assert_eq!(state.config().unwrap().mode, Mode::Local);
        assert!(!state.provider_recording());
        let (audio, _) = state.stop_recording().unwrap();
        assert_eq!(audio, vec![1, 2, 3]);
        state.settle();

        let epoch = state.reserve_opening(provider_settings()).unwrap();
        let (live, _) = FakeLive::audio();
        assert!(state.begin(epoch, live));
        assert!(state.provider_recording());
        assert_eq!(state.config().unwrap().mode, Mode::Provider);
    }

    #[test]
    fn a_new_start_cancels_the_previous_recording_and_supersedes_it() {
        let mut state = State::default();
        let (first, first_stopped) = FakeLive::audio();
        open(&mut state, (first, first_stopped.clone()));
        let generation = state.generation();

        open(&mut state, FakeLive::audio());
        assert!(
            first_stopped.load(std::sync::atomic::Ordering::SeqCst),
            "the first microphone stopped"
        );
        assert_ne!(state.generation(), generation, "the old result is stale");
        assert!(!state.accepts(generation));
    }

    #[test]
    fn stop_then_cancel_drops_a_late_result() {
        let mut state = State::default();
        let generation = open(&mut state, FakeLive::audio());
        state.stop_recording().unwrap();
        state.transcribing(|| {});
        state.cancel();
        assert!(state.idle());
        assert!(!state.accepts(generation), "a late result never lands");
        // The job's cancel hook ran and is gone with it.
        assert!(state.config().is_none());
    }

    #[test]
    fn cancelling_when_idle_changes_nothing() {
        let mut state = State::default();
        let generation = state.generation();
        state.cancel();
        assert_eq!(state.generation(), generation, "Esc when idle is a no-op");
        assert!(state.idle());
        assert!(state.accepts(generation));
    }

    #[test]
    fn a_capped_recording_says_so() {
        let mut state = State::default();
        let epoch = state.reserve_opening(local_settings()).unwrap();
        assert!(state.begin(epoch, FakeLive::capped()));
        assert!(state.capped(), "the status can say the mic stopped");
        let (_, hit_limit) = state.stop_recording().unwrap();
        assert!(hit_limit);
    }

    #[test]
    fn cancel_runs_the_transcription_cancel_hook_exactly_once() {
        let mut state = State::default();
        open(&mut state, FakeLive::audio());
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = calls.clone();
        state.transcribing(move || {
            seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        state.cancel();
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "cancelling must actually run the job's cancel"
        );
    }

    #[test]
    fn esc_during_opening_signals_the_keeper_promptly_and_exactly_once() {
        let mut state = State::default();
        let epoch = state.reserve_opening(local_settings()).unwrap();
        assert_eq!(state.phase(), Phase::Opening);
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = calls.clone();
        state.attach_opening(move || {
            seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        state.cancel();
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "Esc during the opening must reach the keeper exactly once"
        );
        assert!(!state.accepts(epoch), "a late capture is stale");
    }

    #[test]
    fn a_successful_begin_detaches_the_opening_cancel_without_calling_it() {
        let mut state = State::default();
        let epoch = state.reserve_opening(local_settings()).unwrap();
        let (live, _) = FakeLive::audio();
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = calls.clone();
        state.attach_opening(move || {
            seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        assert!(state.begin(epoch, live));
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "a started recording keeps its own cancel path"
        );
    }

    #[test]
    fn a_failed_opening_cancels_the_keeper_for_its_epoch_only() {
        let mut state = State::default();
        let stale = state.reserve_opening(local_settings()).unwrap();
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = calls.clone();
        state.attach_opening(move || {
            seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        state.cancel();
        // A late failure for the stale epoch must not re-cancel anything.
        assert_eq!(state.opening_failed(stale, "late".into()), None);
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the hook ran once, for the cancel"
        );
    }

    #[test]
    fn restarting_cancels_the_running_transcription_hook_once() {
        let mut state = State::default();
        open(&mut state, FakeLive::audio());
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = calls.clone();
        state.transcribing(move || {
            seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        open(&mut state, FakeLive::audio());
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "a restart cancels the in-flight job exactly once"
        );
    }

    #[test]
    fn dropping_the_state_cancels_whatever_is_left() {
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = calls.clone();
        {
            let mut state = State::default();
            open(&mut state, FakeLive::audio());
            state.transcribing(move || {
                seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            });
        }
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "dropping the tab cancels the job"
        );
    }

    #[test]
    fn an_opening_reservation_beats_a_cancel_and_a_late_capture_is_dropped() {
        let mut state = State::default();
        let epoch = state.reserve_opening(local_settings()).unwrap();
        assert_eq!(state.phase(), Phase::Opening);

        // Esc while the microphone is opening: idle again, and the epoch is
        // spent, so the device, once opened, is closed unused.
        state.cancel();
        assert!(state.idle());
        let (late, late_stopped) = FakeLive::audio();
        assert!(
            !state.begin(epoch, late),
            "a cancelled opening never records"
        );
        assert!(
            late_stopped.load(std::sync::atomic::Ordering::SeqCst),
            "the late device is closed, never left recording"
        );

        // A duplicate opening after cancel gets a fresh reservation.
        let fresh = state.reserve_opening(local_settings()).unwrap();
        assert_ne!(fresh, epoch);
        let (live, _) = FakeLive::audio();
        assert!(state.begin(fresh, live));
    }

    #[test]
    fn a_capture_error_is_reported_only_for_the_current_opening() {
        let mut state = State::default();
        let epoch = state.reserve_opening(local_settings()).unwrap();
        state.cancel();
        assert_eq!(
            state.opening_failed(epoch, "No microphone was found.".into()),
            None,
            "a superseded opening's error is dropped with it"
        );

        let epoch = state.reserve_opening(local_settings()).unwrap();
        assert_eq!(
            state.opening_failed(epoch, "No microphone was found.".into()),
            Some("No microphone was found.".to_string()),
        );
        assert!(state.idle());
    }

    #[test]
    fn settling_never_cancels_the_completed_work() {
        let mut state = State::default();
        open(&mut state, FakeLive::audio());
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = calls.clone();
        state.transcribing(move || {
            seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        state.settle();
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "the job finished; its cancel must stay unused"
        );
    }

    #[test]
    fn a_malformed_settings_file_is_rejected_without_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let original = b"{ not json at all";
        std::fs::write(&path, original).unwrap();
        let error = write_key_atomic(&path, KEY, serde_json::json!({"mode": "Local"}))
            .expect_err("malformed JSON must be refused");
        assert!(
            format!("{error}").contains("not valid JSON")
                || format!("{error}").contains("invalid JSON"),
            "{error}"
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            original,
            "an unreadable settings file must keep its exact bytes"
        );
    }

    #[test]
    fn saving_preserves_every_other_settings_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(
            &path,
            r#"{"theme": "Gentleman-Cute", "hydration": {"enabled": true}}"#,
        )
        .unwrap();
        write_key_atomic(&path, KEY, serde_json::json!({"mode": "Off"})).unwrap();
        let after: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(after["theme"], "Gentleman-Cute");
        assert_eq!(after["hydration"]["enabled"], true);
        assert_eq!(after["dictation"]["mode"], "Off");
    }

    #[test]
    fn stopping_twice_is_an_error_the_first_result_still_stands() {
        let mut state = State::default();
        open(&mut state, FakeLive::audio());
        state.stop_recording().unwrap();
        assert!(state.stop_recording().is_err());
    }

    #[test]
    fn an_erroring_microphone_cancels_the_stop() {
        struct Broken;
        impl super::Live for Broken {
            fn capped(&self) -> bool {
                false
            }
            fn finish(self: Box<Self>) -> super::capture::FinishedRecording {
                super::capture::FinishedRecording {
                    audio: Vec::new(),
                    hit_limit: false,
                    error: Some("The microphone stopped: device gone.".into()),
                }
            }
        }
        let mut state = State::default();
        let generation = open(
            &mut state,
            (Box::new(Broken) as Box<dyn Live>, Default::default()),
        );
        let error = state.stop_recording().unwrap_err();
        assert!(error.contains("microphone stopped"), "{error}");
        assert!(state.idle(), "a broken recording is cancelled, not stuck");
        assert_ne!(state.generation(), generation);
    }
}
