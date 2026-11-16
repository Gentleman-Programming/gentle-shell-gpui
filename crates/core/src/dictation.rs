//! Pure dictation rules: the configuration model and its validation, the
//! microphone audio's conversion into a 16 kHz mono PCM WAV, the multipart
//! body a transcription provider receives, and transcript text parsing.
//! Capture, processes and HTTP live in the app (`features/dictation` and the
//! session glue); none of this touches GPUI, files, processes or the network.

use serde_json::Value;

/// Recording is bounded: at most this many seconds enter the buffer, and
/// with them its whole 16-bit mono memory (a few MB).
pub const SAMPLE_RATE: u32 = 16_000;
pub const MAX_SECONDS: u32 = 120;
pub const MAX_SAMPLES: usize = MAX_SECONDS as usize * SAMPLE_RATE as usize;

/// A dictation shorter than this says nothing useful: the user clicked stop
/// by accident, or the device hadn't delivered anything yet.
pub const MIN_SAMPLES: usize = SAMPLE_RATE as usize / 3;

/// Process output, transcript files and other text a transcription run
/// leaves behind are read up to this bound and never further.
pub const MAX_TEXT_BYTES: usize = 64 * 1024;

/// The raw input cap for a recording at the device's own sample rate and
/// channel count: [`MAX_SECONDS`] of audio, however the device delivers it
/// (a 48 kHz stereo device records 120 s, not [`MAX_SAMPLES`] / rate). The
/// capture callback stops appending here, so the recording's memory is
/// bounded whatever the hardware is.
pub fn raw_sample_cap(sample_rate: u32, channels: usize) -> usize {
    MAX_SECONDS as usize * sample_rate.max(1) as usize * channels.max(1)
}

/// Which transcription backend the composer's mic button uses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// Nothing is configured: the mic button explains where to set it up.
    #[default]
    Off,
    /// A user-supplied `whisper-cli` executable and multilingual model run
    /// entirely on this machine.
    Local,
    /// The recording is sent to a configured HTTPS transcription endpoint.
    Provider,
}

impl Mode {
    /// The label Configuration's segmented control shows.
    pub fn label(self) -> &'static str {
        match self {
            Mode::Off => "Off",
            Mode::Local => "Local",
            Mode::Provider => "Provider",
        }
    }

    fn from_label(label: &str) -> Self {
        match label.trim().to_ascii_lowercase().as_str() {
            "provider" => Mode::Provider,
            "local" => Mode::Local,
            _ => Mode::Off,
        }
    }
}

/// The dictation language, in whisper-cli's `--language` spelling.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Language {
    #[default]
    Auto,
    Spanish,
}

impl Language {
    /// The label Configuration's segmented control shows.
    pub fn label(self) -> &'static str {
        match self {
            Language::Auto => "auto",
            Language::Spanish => "es",
        }
    }

    /// The `--language` argument.
    pub fn code(self) -> &'static str {
        match self {
            Language::Auto => "auto",
            Language::Spanish => "es",
        }
    }

    fn from_code(code: &str) -> Self {
        match code.trim().to_ascii_lowercase().as_str() {
            "es" | "spanish" => Language::Spanish,
            _ => Language::Auto,
        }
    }
}

/// Local mode: the user's own whisper.cpp CLI and model. The app never
/// installs them; nothing leaves the machine.
/// Which local engine a Local dictation runs. Absent in older settings
/// files: the default is Whisper, so existing JSON needs no migration.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LocalEngine {
    /// whisper.cpp's `whisper-cli` on a model file (older default).
    #[default]
    Whisper,
    /// The official Vosk Python API on a model DIRECTORY, through an
    /// isolated venv Python the user supplies.
    Vosk,
}

impl LocalEngine {
    /// The label Configuration's selector shows.
    pub fn label(self) -> &'static str {
        match self {
            LocalEngine::Whisper => "Whisper",
            LocalEngine::Vosk => "Vosk",
        }
    }

    fn from_label(label: &str) -> Self {
        match label.trim().to_ascii_lowercase().as_str() {
            "vosk" => LocalEngine::Vosk,
            _ => LocalEngine::Whisper,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Local {
    /// Which engine runs (absent in old JSON: Whisper).
    pub engine: LocalEngine,
    /// Whisper: the whisper.cpp executable. Vosk: the isolated venv
    /// Python that runs the helper.
    pub executable: String,
    /// Whisper: a whisper.cpp model FILE. Vosk: a Vosk model DIRECTORY.
    pub model: String,
    pub language: Language,
}

/// Provider mode: a full-transcription HTTPS endpoint and a model, with the
/// API key read from an environment variable when a transcription runs.
/// The key itself is never configured, stored or shown.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Provider {
    pub endpoint: String,
    pub model: String,
    /// The environment variable's *name* (`GENTLE_TRANSCRIBE_API_KEY`…).
    pub api_key_env: String,
}

/// What Configuration → Dictation saves in the app's `settings.json`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Settings {
    pub mode: Mode,
    pub local: Local,
    pub provider: Provider,
}

impl Settings {
    /// What stands between the user and a working local dictation: the mode's
    /// paths must not only be non-empty (that is [`Settings::validate`]) but
    /// point at files that exist. `executable_exists` / `model_exists` come
    /// from the caller's filesystem; the messages here are pure.
    pub fn local_blockers(&self, executable_exists: bool, model_exists: bool) -> Vec<String> {
        if self.mode != Mode::Local {
            return Vec::new();
        }
        [
            (!executable_exists).then(|| {
                "The whisper-cli executable was not found. Install it or point Dictation at \
             the right path in Configuration \u{2192} Dictation."
                    .to_string()
            }),
            (!model_exists).then(|| {
                "The whisper.cpp model file was not found. Download a multilingual model or \
             point Dictation at the right path in Configuration \u{2192} Dictation."
                    .to_string()
            }),
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    /// Why a provider recording cannot be transcribed yet: its API key must
    /// exist in the environment before anything is recorded or uploaded.
    /// (`None` for every other mode.)
    pub fn provider_blocker(&self, env_set: bool) -> Option<String> {
        if self.mode != Mode::Provider || env_set {
            return None;
        }
        Some(format!(
            "The API key environment variable {} is not set. Set it before recording; the \
         key is read only while a transcription runs.",
            self.provider.api_key_env
        ))
    }

    /// Tolerant read: anything missing or misspelled falls back to its
    /// default, so a hand-edited file never hides the settings from the user.
    pub fn from_json(value: &Value) -> Self {
        let string = |value: &Value, key: &str| -> String {
            value
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let inner = |value: Option<&Value>| -> Value { value.cloned().unwrap_or(Value::Null) };
        let defaults = Self::default();
        let local = inner(value.get("local").filter(|l| l.is_object()));
        let provider = inner(value.get("provider").filter(|p| p.is_object()));
        Self {
            mode: value
                .get("mode")
                .and_then(Value::as_str)
                .map(Mode::from_label)
                .unwrap_or_default(),
            local: Local {
                engine: local
                    .get("engine")
                    .and_then(Value::as_str)
                    .map(LocalEngine::from_label)
                    .unwrap_or_default(),
                executable: string(&local, "executable"),
                model: string(&local, "model"),
                language: local
                    .get("language")
                    .and_then(Value::as_str)
                    .map(Language::from_code)
                    .unwrap_or(defaults.local.language),
            },
            provider: Provider {
                endpoint: string(&provider, "endpoint"),
                model: string(&provider, "model"),
                api_key_env: string(&provider, "api_key_env"),
            },
        }
    }

    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "mode": self.mode.label(),
            "local": {
                "engine": self.local.engine.label(),
                "executable": self.local.executable,
                "model": self.local.model,
                "language": self.local.language.code(),
            },
            "provider": {
                "endpoint": self.provider.endpoint,
                "model": self.provider.model,
                "api_key_env": self.provider.api_key_env,
            },
        })
    }

    /// What the mic button demands before it does anything: the chosen
    /// mode's fields must be usable. It reports what to fix, not just that
    /// it broke.
    pub fn validate(&self) -> Result<(), String> {
        /// The mode's fields that are still empty, named for the message.
        fn missing(fields: &[(&str, &str)]) -> Option<String> {
            let missing: Vec<&str> = fields
                .iter()
                .filter(|(_, value)| value.trim().is_empty())
                .map(|(name, _)| *name)
                .collect();
            match missing.as_slice() {
                [] => None,
                [one] => Some((*one).to_string()),
                _ => {
                    let last = missing.len() - 1;
                    Some(format!(
                        "{} and {}",
                        missing[..last].join(", "),
                        missing[last]
                    ))
                }
            }
        }
        match self.mode {
            Mode::Off => Ok(()),
            Mode::Local => match self.local.engine {
                LocalEngine::Whisper => missing(&[
                    ("the whisper-cli executable path", &self.local.executable),
                    ("the model path", &self.local.model),
                ]),
                LocalEngine::Vosk => missing(&[
                    (
                        "the isolated Python executable path",
                        &self.local.executable,
                    ),
                    ("the Vosk model directory", &self.local.model),
                ]),
            }
            .map_or(Ok(()), |missing| {
                Err(format!("Local dictation is missing {missing}."))
            }),
            Mode::Provider => {
                missing(&[
                    ("an endpoint URL", &self.provider.endpoint),
                    ("the provider model name", &self.provider.model),
                    (
                        "the API key environment variable's name",
                        &self.provider.api_key_env,
                    ),
                ])
                .map_or(Ok(()), |missing| {
                    Err(format!("Provider dictation is missing {missing}."))
                })?;
                is_env_name(&self.provider.api_key_env)
                    .then_some(())
                    .ok_or_else(|| {
                        "The API key environment variable's name must be letters, digits and \
                         underscores."
                            .to_string()
                    })
            }
        }
    }
}
/// A usable environment-variable *name* (a value is never stored here).
pub fn is_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    let first_ok = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
    first_ok && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}
/// The Vosk variant of the bootstrap command: the isolated-venv helper
/// (no pin tag; the published PyPI pins live inside the script).
pub fn vosk_bootstrap_command(project_root: &str, windows: bool) -> String {
    let script = if windows {
        "scripts\\setup-vosk-local.py"
    } else {
        "scripts/setup-vosk-local.py"
    };
    let cd = if windows {
        format!("cd /d \"{project_root}\"")
    } else {
        format!("cd {}", posix_quote(project_root))
    };
    format!("{cd} && python -B {script}")
}

/// POSIX single-quote escaping for the copyable bootstrap command.
fn posix_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// The one command that prepares this workspace's local dictation from a
/// source checkout: the pinned whisper.cpp CPU build plus the recommended
/// multilingual base model, hash-verified, into `.local/voice`. The root
/// is quoted for the platform's shell (Windows `cmd`, elsewhere POSIX).
pub fn bootstrap_command(project_root: &str, windows: bool) -> String {
    let script = if windows {
        "scripts\\setup-voice-local.py"
    } else {
        "scripts/setup-voice-local.py"
    };
    let cd = if windows {
        format!("cd /d \"{project_root}\"")
    } else {
        format!("cd '{}'", project_root.replace('\'', "'\\''"))
    };
    format!("{cd} && python -B {script} --pin-tag b5454")
}

/// Downmix `raw` interleaved samples (the device's `channels`) to mono,
/// resample it linearly to [`SAMPLE_RATE`] and cap it at `cap` samples
/// (`f32` in `[-1, 1]`, as [`samples_as_f32`] produces).
pub fn mono_16k_capped(raw: &[f32], channels: usize, sample_rate: u32, cap: usize) -> Vec<i16> {
    let channels = channels.max(1);
    let mono: Vec<f32> = raw
        .chunks(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect();
    if mono.is_empty() || cap == 0 {
        return Vec::new();
    }
    let capped_mono: Vec<f32> = if sample_rate == SAMPLE_RATE {
        mono
    } else if sample_rate == 0 {
        return Vec::new();
    } else {
        let ratio = f64::from(sample_rate) / f64::from(SAMPLE_RATE);
        let out_len = ((mono.len() as f64 - 1.) / ratio).floor().max(0.) as usize + 1;
        (0..out_len)
            .map(|i| {
                let at = i as f64 * ratio;
                let low = (at.floor() as usize).min(mono.len() - 1);
                let high = (low + 1).min(mono.len() - 1);
                let mix = (at - at.floor()) as f32;
                mono[low] + (mono[high] - mono[low]) * mix
            })
            .collect()
    };
    capped_mono
        .into_iter()
        .take(cap)
        .map(|sample| (sample.clamp(-1., 1.) * i16::MAX as f32) as i16)
        .collect()
}

/// [`mono_16k_capped`] bounded by [`MAX_SAMPLES`]: the recording's own cap.
pub fn mono_16k(raw: &[f32], channels: usize, sample_rate: u32) -> Vec<i16> {
    mono_16k_capped(raw, channels, sample_rate, MAX_SAMPLES)
}

/// A 16-bit mono RIFF/WAVE file of `samples` at [`SAMPLE_RATE`].
pub fn wav_16k(samples: &[i16]) -> Vec<u8> {
    let data_len = samples.len() * 2;
    let mut wave = Vec::with_capacity(44 + data_len);
    wave.extend_from_slice(b"RIFF");
    wave.extend_from_slice(&((36 + data_len) as u32).to_le_bytes());
    wave.extend_from_slice(b"WAVE");
    wave.extend_from_slice(b"fmt ");
    wave.extend_from_slice(&16u32.to_le_bytes());
    wave.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wave.extend_from_slice(&1u16.to_le_bytes()); // mono
    wave.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    wave.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes()); // byte rate
    wave.extend_from_slice(&2u16.to_le_bytes()); // block align
    wave.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    wave.extend_from_slice(b"data");
    wave.extend_from_slice(&(data_len as u32).to_le_bytes());
    for sample in samples {
        wave.extend_from_slice(&sample.to_le_bytes());
    }
    wave
}

/// Signed 16-bit samples (one device format) as plain `f32` in `[-1, 1]`,
/// the form the conversion functions take.
pub fn samples_as_f32(raw: &[i16]) -> Vec<f32> {
    raw.iter()
        .map(|&s| f32::from(s) / f32::from(i16::MAX))
        .collect()
}

/// Unsigned 8-bit samples (another device format) as plain `f32`.
pub fn samples_u8_as_f32(raw: &[u8]) -> Vec<f32> {
    raw.iter()
        .map(|&b| (i16::from(b) - 128) as f32 / 128.)
        .collect()
}

/// The text a transcription produced: trimmed; `None` when it says nothing
/// (a blank recording, or whisper-cli's silent run).
pub fn transcript_from_text(text: &str) -> Option<String> {
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// A provider's JSON answer: OpenAI-style `{"text": …}`, or a plain
/// `{"transcript": …}`. Anything else carries no transcript.
pub fn parse_provider_text(json: &Value) -> Option<String> {
    ["text", "transcript"]
        .iter()
        .filter_map(|key| json.get(*key).and_then(Value::as_str))
        .find_map(transcript_from_text)
}

/// What goes into the composer next to `before` (the character left of its
/// cursor): the dictated text, spaced away from a word but not from
/// whitespace or nothing.
pub fn draft_insertion(before: Option<char>, text: &str) -> String {
    let lead = if before.is_some_and(|c| !c.is_whitespace()) {
        " "
    } else {
        ""
    };
    format!("{lead}{text}")
}

/// A multipart/form-data body for a transcription request: the plain fields
/// first, then the WAV as one `audio/wav` file part. The caller sends
/// `boundary` as the `Content-Type` parameter.
pub fn multipart_body(boundary: &str, fields: &[(&str, &str)], audio: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    for (name, value) in fields {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(
            format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
        );
        body.extend_from_slice(value.as_bytes());
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        b"Content-Disposition: form-data; name=\"file\"; filename=\"audio.wav\"\r\n",
    );
    body.extend_from_slice(b"Content-Type: audio/wav\r\n\r\n");
    body.extend_from_slice(audio);
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    body
}

#[cfg(test)]
mod tests {
    use super::super::dictation::{self, Language, Local, LocalEngine, Mode, Provider, Settings};
    use serde_json::json;

    fn provider_settings(endpoint: &str, env: &str) -> Settings {
        Settings {
            mode: Mode::Provider,
            provider: Provider {
                endpoint: endpoint.into(),
                model: "whisper-1".into(),
                api_key_env: env.into(),
            },
            ..Settings::default()
        }
    }

    // ── Configuration ────────────────────────────────────────────────────

    #[test]
    fn local_validation_names_what_is_missing() {
        let mut settings = Settings {
            mode: Mode::Local,
            ..Settings::default()
        };
        assert_eq!(
            settings.validate().unwrap_err(),
            "Local dictation is missing the whisper-cli executable path and the model path."
        );
        settings.local.executable = "/opt/whisper-cli".into();
        assert_eq!(
            settings.validate().unwrap_err(),
            "Local dictation is missing the model path."
        );
        settings.local.model = "/models/ggml-base.bin".into();
        assert!(settings.validate().is_ok());
        settings.mode = Mode::Off;
        assert!(settings.validate().is_ok(), "Off needs nothing");
    }

    #[test]
    fn provider_validation_names_what_is_missing() {
        let mut settings = provider_settings("", "GENTLE_TRANSCRIBE_API_KEY");
        assert_eq!(
            settings.validate().unwrap_err(),
            "Provider dictation is missing an endpoint URL."
        );
        settings.provider.endpoint = "https://example.com/v1/transcriptions".into();
        settings.provider.model = String::new();
        assert_eq!(
            settings.validate().unwrap_err(),
            "Provider dictation is missing the provider model name."
        );
        settings.provider.model = "whisper-1".into();
        settings.provider.api_key_env = String::new();
        assert_eq!(
            settings.validate().unwrap_err(),
            "Provider dictation is missing the API key environment variable's name."
        );
        assert!(
            provider_settings("https://example.com", "KEY")
                .validate()
                .is_ok()
        );
    }

    #[test]
    fn provider_rejects_a_variable_name_that_is_not_one() {
        assert_eq!(
            provider_settings("https://example.com", "MY KEY")
                .validate()
                .unwrap_err(),
            "The API key environment variable's name must be letters, digits and underscores."
        );
        assert_eq!(
            provider_settings("https://example.com", "1ABC")
                .validate()
                .unwrap_err(),
            "The API key environment variable's name must be letters, digits and underscores."
        );
        assert!(dictation::is_env_name("MY_KEY"));
        assert!(dictation::is_env_name("_A1"));
        assert!(!dictation::is_env_name("A-B"));
        assert!(!dictation::is_env_name(""));
    }

    #[test]
    fn settings_round_trip_every_field() {
        let settings = Settings {
            mode: Mode::Local,
            local: Local {
                engine: LocalEngine::Whisper,
                executable: r"C:\tools\whisper-cli.exe".into(),
                model: r"C:\models\ggml-base.bin".into(),
                language: Language::Spanish,
            },
            provider: Provider {
                endpoint: "https://example.com/v1/transcriptions".into(),
                model: "whisper-1".into(),
                api_key_env: "GENTLE_TRANSCRIBE_API_KEY".into(),
            },
        };
        let json = settings.to_json();
        assert_eq!(
            json,
            json!({
                "mode": "Local",
                "local": {
                    "engine": "Whisper",
                    "executable": r"C:\tools\whisper-cli.exe",
                    "model": r"C:\models\ggml-base.bin",
                    "language": "es",
                },
                "provider": {
                    "endpoint": "https://example.com/v1/transcriptions",
                    "model": "whisper-1",
                    "api_key_env": "GENTLE_TRANSCRIBE_API_KEY",
                },
            })
        );
        assert_eq!(
            Settings::from_json(&json),
            Settings {
                mode: Mode::Local,
                local: Local {
                    engine: LocalEngine::Whisper,
                    executable: r"C:\tools\whisper-cli.exe".into(),
                    model: r"C:\models\ggml-base.bin".into(),
                    language: Language::Spanish,
                },
                provider: Provider {
                    endpoint: "https://example.com/v1/transcriptions".into(),
                    model: "whisper-1".into(),
                    api_key_env: "GENTLE_TRANSCRIBE_API_KEY".into(),
                },
            }
        );
    }

    #[test]
    fn settings_read_tolerates_missing_and_misspelled_fields() {
        let settings = Settings::from_json(&json!({
            "mode": "provider",
            "local": {"executable": "/opt/whisper-cli"},
            "provider": {"endpoint": "https://example.com", "model": "m"},
        }));
        assert_eq!(settings.mode, Mode::Provider);
        assert_eq!(settings.local.executable, "/opt/whisper-cli");
        assert_eq!(
            settings.local.model,
            String::new(),
            "absent falls back to default"
        );
        assert_eq!(settings.local.language, Language::Auto);
        assert_eq!(settings.provider.api_key_env, String::new());

        let unknown = Settings::from_json(&json!({"mode": "whisper-windows"}));
        assert_eq!(unknown.mode, Mode::Off, "an unknown mode reads as Off");
        assert_eq!(Settings::from_json(&json!(null)), Settings::default());
    }

    #[test]
    fn the_local_engine_defaults_to_whisper_when_absent() {
        // Existing settings JSON without an engine field keeps whispering:
        // no migration, no behavior change.
        let old = json!({
            "mode": "Local",
            "local": {"executable": "/opt/whisper-cli", "model": "/m/ggml-base.bin"},
        });
        let settings = Settings::from_json(&old);
        assert_eq!(settings.local.engine, LocalEngine::Whisper);
        // Vosk reads back, and unknown engine spellings fall back safely.
        let vosk = Settings::from_json(&json!({
            "mode": "Local",
            "local": {
                "engine": "vosk",
                "executable": "C:/venv/python.exe",
                "model": "C:/vosk/vosk-model-small-es-0.42",
            },
        }));
        assert_eq!(vosk.local.engine, LocalEngine::Vosk);
        let unknown = Settings::from_json(&json!({
            "mode": "Local",
            "local": {"engine": "dragon", "executable": "x", "model": "y"},
        }));
        assert_eq!(unknown.local.engine, LocalEngine::Whisper);
        // Round trip.
        assert_eq!(
            Settings::from_json(&vosk.to_json()).local.engine,
            LocalEngine::Vosk
        );
    }

    #[test]
    fn vosk_validation_demands_a_python_and_a_model_directory() {
        let settings = Settings::from_json(&json!({
            "mode": "Local",
            "local": {"engine": "vosk", "executable": "", "model": ""},
        }));
        assert_eq!(
            settings.validate().unwrap_err(),
            "Local dictation is missing the isolated Python executable path and the Vosk model directory."
        );
        let mut half = settings.clone();
        half.local.executable = "C:/venv/python.exe".into();
        assert_eq!(
            half.validate().unwrap_err(),
            "Local dictation is missing the Vosk model directory."
        );
        assert!(
            Settings {
                mode: Mode::Local,
                local: Local {
                    engine: LocalEngine::Vosk,
                    executable: "C:/venv/python.exe".into(),
                    model: "C:/vosk/small-es".into(),
                    language: Language::Auto,
                },
                ..Settings::default()
            }
            .validate()
            .is_ok()
        );
    }

    #[test]
    fn the_vosk_bootstrap_command_is_copyable() {
        // The existing Whisper bootstrap command is unchanged.
        assert_eq!(
            dictation::bootstrap_command("G:/repo", true),
            concat!(
                "cd /d \"G:/repo\" && python -B scripts\\setup-voice-local.py ",
                "--pin-tag b5454"
            )
        );
        assert_eq!(
            dictation::vosk_bootstrap_command("G:/My Proj", true),
            "cd /d \"G:/My Proj\" && python -B scripts\\setup-vosk-local.py"
        );
        assert_eq!(
            dictation::vosk_bootstrap_command("/home/me", false),
            "cd '/home/me' && python -B scripts/setup-vosk-local.py"
        );
    }

    #[test]
    fn languages_have_their_cli_spelling() {
        assert_eq!(Language::Auto.code(), "auto");
        assert_eq!(Language::Spanish.code(), "es");
        assert_eq!(
            Settings::from_json(&json!({"local": {"language": "ES"}}))
                .local
                .language,
            Language::Spanish,
            "the code is read case-insensitively"
        );
    }

    // ── Audio conversion and bounds ──────────────────────────────────────

    #[test]
    fn stereo_downmixes_to_the_average() {
        let mono =
            dictation::mono_16k_capped(&[0.0, 1.0, -1.0, 1.0], 2, dictation::SAMPLE_RATE, 100);
        // Frames (0, 1) and (-1, 1) average to 0.5 and 0.0.
        assert_eq!(mono, [16383, 0]);
    }

    #[test]
    fn resampling_keeps_16k_and_scales_other_rates() {
        let passthrough = dictation::mono_16k_capped(&[1.0, -1.0], 1, dictation::SAMPLE_RATE, 100);
        assert_eq!(passthrough, [i16::MAX, -i16::MAX]);

        // 48 kHz → 16 kHz keeps every third input frame (linear 0.0, 0.2, …).
        let raw: Vec<f32> = (0..15).map(|i| i as f32 / 15.).collect();
        let down = dictation::mono_16k_capped(&raw, 1, 48_000, 100);
        assert_eq!(down.len(), 5);
        assert_eq!(down[0], 0);
        assert_eq!(down[4], (0.8 * f32::from(i16::MAX)) as i16);

        // Upsampling interpolates between samples.
        let up = dictation::mono_16k_capped(&[1.0, -1.0], 1, 8_000, 100);
        assert_eq!(up, [i16::MAX, 0, -i16::MAX]);
    }

    #[test]
    fn conversion_caps_at_the_given_sample_count() {
        let raw = vec![1.0; 10 * dictation::SAMPLE_RATE as usize];
        let capped = dictation::mono_16k_capped(&raw, 1, dictation::SAMPLE_RATE, 100);
        assert_eq!(capped.len(), 100, "the cap wins over the input length");
        assert!(capped.iter().all(|s| *s == i16::MAX));

        let empty = dictation::mono_16k_capped(&[], 1, dictation::SAMPLE_RATE, 100);
        assert!(empty.is_empty());
    }

    #[test]
    fn wav_is_a_16_bit_mono_pcm_header_plus_samples() {
        let wave = dictation::wav_16k(&[0, 0x7fff, -2]);
        assert_eq!(&wave[0..4], b"RIFF");
        assert_eq!(&wave[8..12], b"WAVE");
        assert_eq!(&wave[12..16], b"fmt ");
        assert_eq!(u32::from_le_bytes(wave[16..20].try_into().unwrap()), 16);
        assert_eq!(
            u16::from_le_bytes(wave[20..22].try_into().unwrap()),
            1,
            "PCM"
        );
        assert_eq!(
            u16::from_le_bytes(wave[22..24].try_into().unwrap()),
            1,
            "mono"
        );
        assert_eq!(
            u32::from_le_bytes(wave[24..28].try_into().unwrap()),
            dictation::SAMPLE_RATE
        );
        assert_eq!(&wave[36..40], b"data");
        assert_eq!(u32::from_le_bytes(wave[40..44].try_into().unwrap()), 6);
        assert_eq!(wave.len(), 44 + 6);
        assert_eq!(&wave[44..], &[0, 0, 0xff, 0x7f, 0xfe, 0xff]);
    }

    #[test]
    fn i16_samples_become_f32() {
        assert_eq!(dictation::samples_as_f32(&[0, i16::MAX]), [0.0, 1.0]);
        assert_eq!(
            dictation::samples_as_f32(&[-4]),
            [-4.0 / f32::from(i16::MAX)]
        );
    }

    #[test]
    fn u8_samples_center_at_zero() {
        assert_eq!(dictation::samples_u8_as_f32(&[128]), [0.0]);
        assert_eq!(dictation::samples_u8_as_f32(&[255]), [127.0 / 128.0]);
        assert_eq!(dictation::samples_u8_as_f32(&[0]), [-1.0]);
    }

    #[test]
    fn raw_caps_follow_the_device_rate_and_channels() {
        assert_eq!(dictation::raw_sample_cap(48_000, 2), 120 * 48_000 * 2);
        assert_eq!(dictation::raw_sample_cap(44_100, 1), 120 * 44_100);
        assert_eq!(
            dictation::raw_sample_cap(0, 0),
            120,
            "a degenerate device is still bounded"
        );
    }

    #[test]
    fn a_full_recording_at_48k_stereo_covers_two_minutes() {
        // Regression: the cap used to be counted in 16 kHz output samples,
        // so a 48 kHz stereo device stopped after 40 seconds.
        let raw = vec![0.25f32; dictation::raw_sample_cap(48_000, 2)];
        let mono = dictation::mono_16k(&raw, 2, 48_000);
        assert_eq!(
            mono.len(),
            dictation::MAX_SAMPLES,
            "two minutes at the device's rate, not a third of it"
        );
    }

    #[test]
    fn a_full_recording_at_44k_stays_bounded() {
        let raw = vec![0.5f32; dictation::raw_sample_cap(44_100, 2)];
        let mono = dictation::mono_16k(&raw, 2, 44_100);
        assert_eq!(mono.len(), dictation::MAX_SAMPLES);
        assert!(
            mono.iter()
                .all(|s| *s == (0.5 * f32::from(i16::MAX)) as i16)
        );
    }

    // ── Transcripts ──────────────────────────────────────────────────────

    #[test]
    fn local_blockers_name_missing_files_before_the_mic_opens() {
        let mut settings = Settings {
            mode: Mode::Local,
            local: Local {
                engine: LocalEngine::Whisper,
                executable: "/opt/whisper-cli".into(),
                model: "/opt/models/ggml-base.bin".into(),
                language: Language::Auto,
            },
            ..Settings::default()
        };
        assert!(settings.local_blockers(true, true).is_empty());
        assert_eq!(
            settings.local_blockers(false, true),
            vec![
                "The whisper-cli executable was not found. Install it or point Dictation at \
                 the right path in Configuration → Dictation."
                    .to_string()
            ]
        );
        let both = settings.local_blockers(false, false);
        assert_eq!(both.len(), 2, "{both:?}");

        // Off and Provider never report local blockers.
        assert!(Settings::default().local_blockers(false, false).is_empty());
        settings.mode = Mode::Provider;
        assert!(settings.local_blockers(false, false).is_empty());
    }

    #[test]
    fn provider_blocker_diagnoses_the_missing_key_before_any_upload() {
        let settings = Settings {
            mode: Mode::Provider,
            provider: Provider {
                endpoint: "https://example.com".into(),
                model: "m".into(),
                api_key_env: "GENTLE_TRANSCRIBE_API_KEY".into(),
            },
            ..Settings::default()
        };
        assert!(settings.provider_blocker(true).is_none());
        assert_eq!(
            settings.provider_blocker(false).as_deref(),
            Some(
                "The API key environment variable GENTLE_TRANSCRIBE_API_KEY is not set. Set it \
                 before recording; the key is read only while a transcription runs."
            )
        );
        assert!(Settings::default().provider_blocker(false).is_none());
        assert_eq!(
            Settings {
                mode: Mode::Local,
                ..settings.clone()
            }
            .provider_blocker(false),
            None
        );
    }

    #[test]
    fn the_bootstrap_command_quotes_the_project_root_safely() {
        let windows = dictation::bootstrap_command(r"G:\My Project\repo", true);
        assert_eq!(
            windows,
            "cd /d \"G:\\My Project\\repo\" && python -B scripts\\setup-voice-local.py --pin-tag b5454"
        );

        let posix = dictation::bootstrap_command("/home/me/my repo", false);
        assert_eq!(
            posix,
            "cd '/home/me/my repo' && python -B scripts/setup-voice-local.py --pin-tag b5454"
        );
        // A single quote in a POSIX path is closed, escaped and reopened.
        let quoted = dictation::bootstrap_command("/home/me/it's", false);
        assert!(quoted.contains("'\\''"), "{quoted}");
        assert!(quoted.starts_with("cd '"));
    }

    #[test]
    fn transcripts_trim_and_drop_silence() {
        assert_eq!(
            dictation::transcript_from_text("  hola, ¿cómo estás?\n"),
            Some("hola, ¿cómo estás?".to_string())
        );
        assert_eq!(dictation::transcript_from_text(" \n\t "), None);
        assert_eq!(dictation::transcript_from_text(""), None);
    }

    #[test]
    fn provider_answers_parse_from_their_text_fields() {
        assert_eq!(
            dictation::parse_provider_text(&json!({"text": " recorded words "})),
            Some("recorded words".to_string())
        );
        assert_eq!(
            dictation::parse_provider_text(&json!({"transcript": "words"})),
            Some("words".to_string())
        );
        assert_eq!(
            dictation::parse_provider_text(&json!({"error": "no"})),
            None
        );
        assert_eq!(dictation::parse_provider_text(&json!({"text": ""})), None);
    }

    #[test]
    fn draft_insertion_spaces_from_a_word_but_not_from_whitespace() {
        assert_eq!(dictation::draft_insertion(Some('a'), "hola"), " hola");
        assert_eq!(dictation::draft_insertion(Some(' '), "hola"), "hola");
        assert_eq!(dictation::draft_insertion(Some('\n'), "hola"), "hola");
        assert_eq!(dictation::draft_insertion(None, "hola"), "hola");
    }

    #[test]
    fn multipart_body_carries_the_fields_then_the_audio_file() {
        let body = dictation::multipart_body(
            "gentle123",
            &[("model", "whisper-1"), ("language", "es")],
            &[1, 2, 3],
        );
        let text = String::from_utf8_lossy(&body);
        assert!(text.starts_with("--gentle123\r\n"));
        assert!(
            text.contains("Content-Disposition: form-data; name=\"model\"\r\n\r\nwhisper-1\r\n")
        );
        assert!(text.contains("Content-Disposition: form-data; name=\"language\"\r\n\r\nes\r\n"));
        assert!(
            text.contains(
                "Content-Disposition: form-data; name=\"file\"; filename=\"audio.wav\"\r\n"
            )
        );
        assert!(text.contains("Content-Type: audio/wav\r\n"));
        // The raw audio bytes sit between the file part's headers and its close.
        const MARKER: &[u8] = b"Content-Type: audio/wav\r\n\r\n";
        let head = body
            .windows(MARKER.len())
            .position(|window| window == MARKER)
            .expect("the file part's headers")
            + MARKER.len();
        assert_eq!(&body[head..head + 3], &[1, 2, 3]);
        assert!(body.ends_with(b"\r\n--gentle123--\r\n"));
    }
}
