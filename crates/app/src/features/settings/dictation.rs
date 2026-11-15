//! The Dictation section: Off, a local whisper.cpp CLI and model, or a
//! transcription provider. Edits are held in the form and saved only by
//! the explicit Save button, which validates first and writes only this
//! feature's key. Local mode uses only what the user supplies; in provider
//! mode the audio goes to the configured endpoint when a transcription
//! runs, with the key read from an environment variable then.
//!
//! The section also reports readiness while the form is edited: what the
//! chosen engine expects at each local path (whisper.cpp a model FILE, Vosk a
//! model DIRECTORY) and whether that expected kind is really there, the
//! provider variable's presence by name only, and the mic's own verdict on its
//! own line. The advisory lines never claim the mic verified the kind: its rule
//! is unchanged (`ensure_ready`), so an existing path of another kind is
//! reported as exactly that. Saving a syntactically valid but unready
//! configuration is allowed and says "saved, but not ready" instead of claiming
//! success.

use std::path::{Path, PathBuf};

use gentle_core::dictation as core_dictation;
use gentle_core::dictation::{Language, Local, LocalEngine, Mode, Provider, Settings};
use gpui_kit::Hsla;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{Disableable as _, Sizable as _};
use gpui_kit::component::{Icon, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::SettingsView;
use super::layout::section;
use crate::features::dictation;
use crate::shared::ui;

/// A labelled control row, like the other sections'.
fn control(label: &'static str, dim: bool, control: impl IntoElement) -> Div {
    h_flex()
        .pt_2()
        .gap_2()
        .child(
            div()
                .w(px(120.))
                .flex_shrink_0()
                .text_xs()
                .text_color(if dim { ui::faint() } else { ui::muted() })
                .child(label),
        )
        .child(control)
}

fn hint(text: &str) -> Div {
    div()
        .text_xs()
        .text_color(ui::faint())
        .child(text.to_string())
}

/// A `well` field with an input, for the section's text values.
fn text_input(state: &Entity<InputState>) -> Div {
    ui::field()
        .flex_1()
        .child(Input::new(state).appearance(false).small())
}

/// How a note under the Save button reads: a save that leaves the mic usable,
/// a save that leaves it blocked, or a refusal that wrote nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NoteKind {
    Ok,
    Warning,
    Failed,
}

impl NoteKind {
    fn color(self) -> Hsla {
        match self {
            NoteKind::Ok => ui::success(),
            NoteKind::Warning => ui::warning(),
            NoteKind::Failed => ui::danger(),
        }
    }

    fn icon(self) -> IconName {
        match self {
            NoteKind::Ok => IconName::CircleCheck,
            NoteKind::Warning | NoteKind::Failed => IconName::CircleAlert,
        }
    }
}

/// What the last action reported, and how loudly.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Note {
    message: String,
    kind: NoteKind,
}

impl Note {
    fn new(kind: NoteKind, message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            kind,
        }
    }

    fn ok(message: impl Into<String>) -> Self {
        Self::new(NoteKind::Ok, message)
    }

    fn warning(message: impl Into<String>) -> Self {
        Self::new(NoteKind::Warning, message)
    }

    fn failed(message: impl Into<String>) -> Self {
        Self::new(NoteKind::Failed, message)
    }
}

/// What Save does before writing anything: invalid syntax is refused and
/// nothing is written. A syntactically valid form is written even when the mic
/// would still refuse it; `saved_note` reports that case as a warning, so a
/// configuration can be saved before its model or key exists.
fn save_plan(settings: &Settings) -> Result<(), Note> {
    settings.validate().map_err(Note::failed)
}

/// The note for a completed Save: plain success only when the mic would take
/// what was just written, an explicit "saved, but not ready" warning when it
/// would not — never a plain success for a configuration that cannot record.
fn saved_note(settings: &Settings, readiness: Result<(), String>) -> Note {
    const SAVED: &str = "Dictation settings saved.";
    // Off is a choice, not a blocked configuration: nothing is expected to
    // record, so there is no readiness warning to give.
    if settings.mode == Mode::Off {
        return Note::ok(SAVED);
    }
    match readiness {
        Ok(()) => Note::ok(SAVED),
        Err(reason) => Note::warning(format!("Dictation settings saved, but not ready: {reason}")),
    }
}

/// What the selected engine expects at its two configured paths, in field
/// order: (field label, expectation, it is a directory). whisper.cpp takes an
/// executable and a model FILE, Vosk an isolated Python and a model DIRECTORY.
/// The labels are the field labels, so the rows and the advisory lines never
/// disagree.
fn local_fields(engine: LocalEngine) -> [(&'static str, &'static str, bool); 2] {
    match engine {
        LocalEngine::Whisper => [
            ("Executable", "the whisper-cli executable (a file)", false),
            (
                "Model",
                "a whisper.cpp model file (for example ggml-base.bin)",
                false,
            ),
        ],
        LocalEngine::Vosk => [
            (
                "Python (venv)",
                "the isolated Python executable (a file)",
                false,
            ),
            (
                "Model directory",
                "a Vosk model directory (for example vosk-model-small-es-0.42)",
                true,
            ),
        ],
    }
}

/// What the advisory check found at one configured path: the expected kind is
/// there, a path is there but not of that kind, or nothing is there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Found,
    Unexpected,
    Missing,
}

/// Existence *and* kind, so the section never calls an unexpected kind "found".
/// Only `is_file`/`is_dir`: nothing is opened, hashed, loaded or read.
fn kind_at(path: &str, directory: bool) -> Kind {
    let path = Path::new(path.trim());
    let expected = if directory {
        path.is_dir()
    } else {
        path.is_file()
    };
    if expected {
        Kind::Found
    } else if path.exists() {
        Kind::Unexpected
    } else {
        Kind::Missing
    }
}

/// One advisory line under the local fields: which path, what the engine
/// expects there, and whether that expected kind is actually there. Advisory
/// only: the mic's own rule is unchanged (`ensure_ready`) and is reported on
/// its own line, so a path that exists with another kind is not called found.
#[derive(Debug, PartialEq, Eq)]
struct Status {
    label: &'static str,
    message: String,
    present: bool,
}

fn local_status(engine: LocalEngine, executable: &str, model: &str) -> [Status; 2] {
    let fields = local_fields(engine);
    let paths = [executable, model];
    std::array::from_fn(|i| {
        let (label, expectation, directory) = fields[i];
        let kind = kind_at(paths[i], directory);
        Status {
            label,
            present: kind == Kind::Found,
            message: match kind {
                Kind::Found => format!("expects {expectation} — found."),
                Kind::Unexpected => {
                    format!("expects {expectation} — a path exists here, but not of that kind.")
                }
                Kind::Missing => format!("expects {expectation} — not found."),
            },
        }
    })
}

/// The provider's variable, by NAME only: whether this app's environment has
/// it. The value is never read for display, presence proves nothing about the
/// provider, and nothing here contacts it.
fn provider_status(settings: &Settings, readiness: &Result<(), String>) -> Option<Status> {
    if settings.mode != Mode::Provider {
        return None;
    }
    // Only a complete form can be asked about the environment, and the
    // boolean comes from the one helper that reads it (`ensure_ready`): an
    // incomplete name never gets a made-up answer.
    settings.validate().ok()?;
    let name = settings.provider.api_key_env.trim();
    let present = match readiness {
        Ok(()) => true,
        // `ensure_ready` validates no endpoint (that happens at transcription
        // time), so its only Provider reason is the variable's own blocker.
        // Anything else is unknown, never reported as "not present".
        Err(reason) => {
            if settings.provider_blocker(false).as_deref() != Some(reason.as_str()) {
                return None;
            }
            false
        }
    };
    Some(Status {
        label: "API key variable",
        present,
        message: if present {
            format!(
                "{name} is present in this app's environment. Presence is all this line \
                 reports: the provider is not contacted and nothing confirms it is reachable."
            )
        } else {
            format!(
                "{name} is not present in this app's environment. Set it before recording; \
                 only the name is saved, never the key."
            )
        },
    })
}

fn status_line(status: &Status) -> AnyElement {
    verdict_line(
        status.present,
        format!("{} {}", status.label, status.message),
    )
}

/// The mic's own verdict, in its own words and labelled as such: the unchanged
/// predicate the record button enforces, separate from the advisory lines.
fn mic_line(readiness: &Result<(), String>) -> AnyElement {
    match readiness {
        Ok(()) => verdict_line(
            true,
            "Mic readiness: the mic can use this configuration.".into(),
        ),
        Err(reason) => verdict_line(false, format!("Mic readiness: {reason}")),
    }
}

fn verdict_line(ok: bool, text: String) -> AnyElement {
    let color = if ok { ui::success() } else { ui::warning() };
    let icon = if ok {
        IconName::CircleCheck
    } else {
        IconName::CircleAlert
    };
    h_flex()
        .pt_1()
        .gap_1()
        .text_xs()
        .text_color(color)
        .child(Icon::new(icon).xsmall().text_color(color))
        .child(text)
        .into_any_element()
}

fn note_element(note: &Note) -> AnyElement {
    let color = note.kind.color();
    h_flex()
        .gap_1()
        .text_xs()
        .text_color(color)
        .child(Icon::new(note.kind.icon()).xsmall().text_color(color))
        .child(note.message.clone())
        .into_any_element()
}

/// The copyable bootstrap script that prepares this workspace for the selected
/// engine, as `(file name, command)`, when this checkout ships it. Copying is
/// all the section does: the app never runs it.
fn bootstrap_for(project_root: &Path, engine: LocalEngine) -> Option<(String, String)> {
    let script = match engine {
        LocalEngine::Whisper => "setup-voice-local.py",
        LocalEngine::Vosk => "setup-vosk-local.py",
    };
    if !project_root.join("scripts").join(script).is_file() {
        return None;
    }
    let root = project_root.display().to_string();
    let command = match engine {
        LocalEngine::Whisper => core_dictation::bootstrap_command(&root, cfg!(windows)),
        LocalEngine::Vosk => core_dictation::vosk_bootstrap_command(&root, cfg!(windows)),
    };
    Some((script.to_string(), command))
}

/// The guided setup's recommendation, correct for the engine the form has
/// selected: whisper.cpp on the CPU, or the official Vosk through your own
/// isolated Python. Both are local and free; the app itself never installs or
/// downloads either.
fn recommendation(engine: LocalEngine) -> &'static str {
    match engine {
        LocalEngine::Whisper => {
            "Nothing is configured for the microphone yet. For a first \
             test, the recommended setup is local and free: whisper.cpp's whisper-cli on the \
             CPU with the multilingual base model — the executable and a model FILE (for \
             example ggml-base.bin, about 148 MB, Spanish-capable, offline). You supply both; \
             the app never installs or downloads them itself. Provider mode instead sends \
             each recording to a transcription endpoint you configure; its API key is read \
             from an environment variable only while a transcription runs and is never \
             stored here, and the provider may charge."
        }
        LocalEngine::Vosk => {
            "Nothing is configured for the microphone yet. For a first test, \
             the recommended setup is local and free: the official Vosk (alphacephei.com) \
             through your own isolated Python — the venv's Python and a model DIRECTORY (for \
             example vosk-model-small-es-0.42, about 39 MB, Apache-2.0, Spanish, offline, \
             needs Python 3). You supply both; the app never installs or downloads them \
             itself. Provider mode instead sends each recording to a transcription endpoint \
             you configure; its API key is read from an environment variable only while a \
             transcription runs and is never stored here, and the provider may charge."
        }
    }
}

/// The section's editable state: the five values the two modes need. They
/// are a form, not the saved settings: nothing is written until Save.
pub(super) struct Dictation {
    mode: Mode,
    language: Language,
    engine: LocalEngine,
    /// The workspace the screen was opened from: where a prepared local
    /// setup (`.local/voice/setup.json`) and the bootstrap script are
    /// looked for.
    project_root: PathBuf,
    /// Nothing usable is configured yet (Off, invalid, or local paths
    /// that do not exist): the section leads with the guided setup. Read
    /// from the form on every render, not cached, so the guidance follows
    /// every edit and every Save.
    local_executable: Entity<InputState>,
    local_model: Entity<InputState>,
    provider_endpoint: Entity<InputState>,
    provider_model: Entity<InputState>,
    provider_env: Entity<InputState>,
    /// What the last Save or apply did, and how loudly it reads.
    note: Option<Note>,
    /// The prepared setup is being verified (hashing runs off-thread).
    verifying: bool,
}

impl Dictation {
    pub(super) fn new(
        project_root: PathBuf,
        window: &mut Window,
        cx: &mut Context<SettingsView>,
    ) -> Self {
        let saved = dictation::read_config_raw();
        let input = |placeholder: &str,
                     value: &str,
                     window: &mut Window,
                     cx: &mut Context<SettingsView>| {
            let state = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
            state.update(cx, |state, cx| state.set_value(value, window, cx));
            state
        };
        Self {
            mode: saved.mode,
            language: saved.local.language,
            engine: saved.local.engine,
            project_root,
            // Absolute paths only: `~` is not expanded by the app, so an
            // example that suggests it would silently not work.
            local_executable: input(
                r"C:\tools\whisper-cli.exe, /opt/whisper-cli, …",
                &saved.local.executable,
                window,
                cx,
            ),
            local_model: input(
                r"C:\models\ggml-base.bin, /opt/models/ggml-base.bin, …",
                &saved.local.model,
                window,
                cx,
            ),
            provider_endpoint: input(
                "https://…/v1/transcriptions",
                &saved.provider.endpoint,
                window,
                cx,
            ),
            provider_model: input(
                "The provider's transcription model",
                &saved.provider.model,
                window,
                cx,
            ),
            provider_env: input(
                "The environment variable's name, e.g. GENTLE_TRANSCRIBE_API_KEY",
                &saved.provider.api_key_env,
                window,
                cx,
            ),
            note: None,
            verifying: false,
        }
    }
}

impl SettingsView {
    /// Compose the settings the form shows right now.
    fn dictation_settings(&self, cx: &App) -> Settings {
        let d = &self.dictation;
        crate::features::dictation::form_settings(
            d.mode,
            Local {
                engine: d.engine,
                executable: d.local_executable.read(cx).value().to_string(),
                model: d.local_model.read(cx).value().to_string(),
                language: d.language,
            },
            Provider {
                endpoint: d.provider_endpoint.read(cx).value().to_string(),
                model: d.provider_model.read(cx).value().to_string(),
                api_key_env: d.provider_env.read(cx).value().to_string(),
            },
        )
    }

    /// Save: validate first (the error names what to fix and nothing is
    /// written), then write only the dictation key, atomically, preserving
    /// every other key. A syntactically valid form is written even when the
    /// mic would still refuse it, and the note then says "saved, but not
    /// ready" with the mic's own reason instead of a plain success. The
    /// message never carries paths or file contents.
    fn save_dictation(&mut self, cx: &mut Context<Self>) {
        let settings = self.dictation_settings(cx);
        self.dictation.note = Some(match save_plan(&settings) {
            Err(refusal) => refusal,
            Ok(()) => match dictation::write_config(&settings) {
                Err(message) => Note::failed(message),
                Ok(()) => saved_note(&settings, dictation::ensure_ready(&settings)),
            },
        });
        cx.notify();
    }

    /// The guided setup shown while nothing usable is configured: what
    /// the recommended local setup is, how to get it from this checkout,
    /// and one click to apply the workspace's prepared files. Nothing
    /// here downloads, installs or writes: applying prefills the form and
    /// Save (the explicit action) writes it.
    fn dictation_setup_help(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let engine = self.dictation.engine;
        let mut help = vec![
            v_flex()
                .pt_2()
                .gap_1()
                .child(hint(recommendation(engine)))
                .into_any_element(),
        ];
        // The prepared files of this workspace for the selected engine, when
        // the bootstrap ran here. The Local branch keeps its own copy of this
        // action; this one appears while the form is not on Local yet.
        let prepared = match engine {
            LocalEngine::Whisper => {
                dictation::setup::prepared_setup_available(&self.dictation.project_root)
            }
            LocalEngine::Vosk => {
                dictation::setup::prepared_vosk_available(&self.dictation.project_root)
            }
        };
        if prepared && self.dictation.mode != Mode::Local {
            let (button_id, label, description) = match engine {
                LocalEngine::Whisper => (
                    "dictation-apply-prepared-help",
                    "Use prepared local setup",
                    "Loads this workspace's verified engine and model into the fields \
                         below; press Save to apply.",
                ),
                LocalEngine::Vosk => (
                    "dictation-apply-prepared-vosk-help",
                    "Use prepared Vosk setup",
                    "Loads this workspace's verified Vosk Python and model into the \
                         fields below; press Save to apply.",
                ),
            };
            help.push(
                h_flex()
                    .pt_1()
                    .gap_2()
                    .child(
                        Button::new(button_id)
                            .small()
                            .primary()
                            .label(if self.dictation.verifying {
                                "Verifying…"
                            } else {
                                label
                            })
                            .loading(self.dictation.verifying)
                            .disabled(self.dictation.verifying)
                            .on_click(cx.listener(move |this, _, window, cx| match engine {
                                LocalEngine::Whisper => this.apply_prepared_dictation(window, cx),
                                LocalEngine::Vosk => this.apply_prepared_vosk(window, cx),
                            })),
                    )
                    .child(div().text_xs().text_color(ui::faint()).child(description))
                    .into_any_element(),
            );
        }
        // The bootstrap command for the selected engine, only where the script
        // exists (a source checkout): copyable, never run by the app.
        if let Some((_, command)) = bootstrap_for(&self.dictation.project_root, engine) {
            help.push(
                h_flex()
                    .pt_1()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .font_family(ui::MONO)
                            .text_color(ui::muted())
                            .overflow_hidden()
                            .child(command.clone()),
                    )
                    .child(
                        Button::new("dictation-copy-setup")
                            .small()
                            .label("Copy")
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(
                                    command.clone(),
                                ));
                            }),
                    )
                    .into_any_element(),
            );
            help.push(
                hint(
                    "Run the copied command from a terminal in this project: it downloads \
                     the pinned engine and the recommended model into .local/voice with \
                     checksum verification. The app itself uses no network here.",
                )
                .into_any_element(),
            );
        }
        help
    }

    /// The verified paths into the form: the deliberate apply. Saving
    /// stays the explicit step, and nothing is written before it.
    fn dictation_prefill(
        &mut self,
        prepared: dictation::setup::PreparedSetup,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dictation.mode = Mode::Local;
        let executable = prepared.executable;
        let model = prepared.model;
        self.dictation.local_executable.update(cx, |state, cx| {
            state.set_value(executable, window, cx);
        });
        self.dictation.local_model.update(cx, |state, cx| {
            state.set_value(model, window, cx);
        });
        self.dictation.note = Some(Note::ok(
            "Prepared local setup loaded. Review the paths and press Save.",
        ));
    }

    /// "Use prepared Vosk setup": load this workspace's verified Vosk
    /// files (venv Python + model directory, digest-checked) into the
    /// form. Saving stays the explicit step; nothing is written before it.
    fn apply_prepared_vosk(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.dictation.verifying {
            return;
        }
        self.dictation.verifying = true;
        let root = self.dictation.project_root.clone();
        let job = cx
            .background_executor()
            .spawn(async move { dictation::setup::load_prepared_vosk(&root) });
        cx.spawn_in(window, async move |this, cx| {
            let result = job.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.dictation.verifying = false;
                match result {
                    Ok(prepared) => {
                        this.dictation.mode = Mode::Local;
                        this.dictation.engine = LocalEngine::Vosk;
                        let executable = prepared.executable;
                        let model = prepared.model;
                        this.dictation.local_executable.update(cx, |state, cx| {
                            state.set_value(executable, window, cx);
                        });
                        this.dictation.local_model.update(cx, |state, cx| {
                            state.set_value(model, window, cx);
                        });
                        this.dictation.note = Some(Note::ok(
                            "Prepared Vosk setup loaded. Review and press Save.",
                        ));
                    }
                    Err(message) => {
                        this.dictation.note = Some(Note::failed(message));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// "Use prepared local setup": load this workspace's verified files
    /// into the form. Saving stays the explicit step, and nothing is
    /// written before it.
    fn apply_prepared_dictation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.dictation.verifying {
            return;
        }
        // Verifying hashes the model (~150 MB): off the main thread, with
        // the button showing the wait; a rejection never touches config.
        self.dictation.verifying = true;
        let root = self.dictation.project_root.clone();
        let job = cx
            .background_executor()
            .spawn(async move { dictation::setup::load_prepared(&root) });
        cx.spawn_in(window, async move |this, cx| {
            let result = job.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.dictation.verifying = false;
                match result {
                    Ok(prepared) => this.dictation_prefill(prepared, window, cx),
                    Err(message) => {
                        this.dictation.note = Some(Note::failed(message));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn dictation_section(&self, cx: &mut Context<Self>) -> Div {
        let d = &self.dictation;
        let dim = |mode: Mode| d.mode != mode;
        // The form as it stands, not as it was when the section was built: the
        // guided setup follows every edit (mode, engine, paths) and the mic's
        // own predicate decides whether it is shown. This costs a couple of
        // existence/type checks and the environment lookup the mic already
        // does; no file is opened, hashed or loaded to draw it.
        let settings = self.dictation_settings(cx);
        let needs_setup = dictation::setup_needed(&settings);
        let readiness = dictation::ensure_ready(&settings);
        let fields = local_fields(d.engine);
        let section = section(IconName::Mic, "Dictation");
        let section = if needs_setup {
            section.children(self.dictation_setup_help(cx))
        } else {
            section
        };
        section
            .child(control(
                "Mode",
                false,
                ui::segmented(
                    "dictation-mode",
                    &["Off", "Local", "Provider"],
                    Some(match d.mode {
                        Mode::Off => 0,
                        Mode::Local => 1,
                        Mode::Provider => 2,
                    }),
                    cx.listener(|this, index: &usize, _, cx| {
                        this.dictation.mode = match index {
                            1 => Mode::Local,
                            2 => Mode::Provider,
                            _ => Mode::Off,
                        };
                        cx.notify();
                    }),
                ),
            ))
            .when(d.mode == Mode::Local, |section| {
                section
                    .child(control(
                        "Engine",
                        false,
                        ui::segmented(
                            "dictation-engine",
                            &["Whisper", "Vosk"],
                            Some(if d.engine == LocalEngine::Vosk { 1 } else { 0 }),
                            cx.listener(|this, index: &usize, _, cx| {
                                this.dictation.engine = if *index == 1 {
                                    LocalEngine::Vosk
                                } else {
                                    LocalEngine::Whisper
                                };
                                this.dictation.note = None;
                                cx.notify();
                            }),
                        ),
                    ))
                    .child(control(fields[0].0, false, text_input(&d.local_executable)))
                    .child(control(fields[1].0, false, text_input(&d.local_model)))
                    .child(control(
                        "Language",
                        false,
                        ui::segmented(
                            "dictation-language",
                            &["auto", "es"],
                            Some(match d.language {
                                Language::Auto => 0,
                                Language::Spanish => 1,
                            }),
                            cx.listener(|this, index: &usize, _, cx| {
                                this.dictation.language = match index {
                                    1 => Language::Spanish,
                                    _ => Language::Auto,
                                };
                                cx.notify();
                            }),
                        ),
                    ))
                    .child(if d.engine == LocalEngine::Vosk {
                        v_flex()
                            .pt_2()
                            .gap_1()
                            .child(hint(
                                "Runs the official Vosk (alphacephei.com) through your own \
                                 isolated Python — the app installs nothing. Recommended for a \
                                 first CPU test: vosk-model-small-es-0.42 (~39 MB download, \
                                 Apache-2.0, Spanish, offline, free; needs Python 3). Optional: \
                                 vosk-model-es-0.42 (~1.4 GB) for more resource use. The official \
                                 model page publishes no accuracy numbers. Transcription is a \
                                 batch like Whisper's: stop, transcribe, review the draft.",
                            ))
                            .children({
                                let root = self.dictation.project_root.clone();
                                (root.join("scripts").join("setup-vosk-local.py").is_file()).then(
                                    move || {
                                        let command = core_dictation::vosk_bootstrap_command(
                                            &root.display().to_string(),
                                            cfg!(windows),
                                        );
                                        h_flex()
                                            .pt_1()
                                            .gap_2()
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .text_xs()
                                                    .font_family(ui::MONO)
                                                    .text_color(ui::muted())
                                                    .overflow_hidden()
                                                    .child(command.clone()),
                                            )
                                            .child(
                                                Button::new("dictation-copy-vosk-setup")
                                                    .small()
                                                    .label("Copy")
                                                    .on_click(move |_, _, cx| {
                                                        cx.write_to_clipboard(
                                                            gpui_kit::ClipboardItem::new_string(
                                                                command.clone(),
                                                            ),
                                                        );
                                                    }),
                                            )
                                    },
                                )
                            })
                            .when(
                                dictation::setup::prepared_vosk_available(
                                    &self.dictation.project_root,
                                ),
                                |help| {
                                    help.child(
                                        Button::new("dictation-apply-prepared-vosk")
                                            .small()
                                            .primary()
                                            .label(if self.dictation.verifying {
                                                "Verifying…"
                                            } else {
                                                "Use prepared Vosk setup"
                                            })
                                            .loading(self.dictation.verifying)
                                            .disabled(self.dictation.verifying)
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.apply_prepared_vosk(window, cx)
                                            })),
                                    )
                                    .children(self.dictation.note.as_ref().map(note_element))
                                },
                            )
                    } else {
                        v_flex().pt_2().child(hint(
                            "Uses your own whisper-cli executable and a whisper.cpp multilingual \
                             model that you supply and trust — the app does not download or \
                             install either. The audio is transcribed on this machine and \
                             never leaves it.",
                        ))
                    })
            })
            .when(d.mode == Mode::Provider, |section| {
                section
                    .child(control("Endpoint", false, text_input(&d.provider_endpoint)))
                    .child(control(
                        "Model",
                        dim(Mode::Provider),
                        text_input(&d.provider_model),
                    ))
                    .child(control(
                        "API key variable",
                        dim(Mode::Provider),
                        text_input(&d.provider_env),
                    ))
                    .child(
                        v_flex()
                            .pt_2()
                            .child(hint(
                                "When you stop recording, the audio is sent to this provider over \
                                 the internet, and its answer lands in the composer for you to \
                                 review. The API key is read from the named environment variable \
                                 only while a transcription runs — it is never stored or shown \
                                 here. The provider may charge for transcriptions.",
                            ))
                            .children({
                                let endpoint =
                                    d.provider_endpoint.read(cx).value().trim().to_string();
                                (!endpoint.is_empty()).then(|| match dictation::endpoint::validate(
                                    &endpoint,
                                ) {
                                    Ok(_) => h_flex()
                                        .gap_1()
                                        .text_xs()
                                        .text_color(ui::success())
                                        .child(
                                            Icon::new(IconName::CircleCheck)
                                                .xsmall()
                                                .text_color(ui::success()),
                                        )
                                        .child("The endpoint looks good."),
                                    Err(reason) => h_flex()
                                        .gap_1()
                                        .text_xs()
                                        .text_color(ui::warning())
                                        .child(
                                            Icon::new(IconName::CircleAlert)
                                                .xsmall()
                                                .text_color(ui::warning()),
                                        )
                                        .child(reason),
                                })
                            }),
                    )
            })
            .child({
                let mut lines: Vec<AnyElement> = Vec::new();
                if d.mode == Mode::Local {
                    lines.extend(
                        local_status(d.engine, &settings.local.executable, &settings.local.model)
                            .iter()
                            .map(status_line),
                    );
                }
                if let Some(status) = provider_status(&settings, &readiness) {
                    lines.push(status_line(&status));
                }
                if d.mode != Mode::Off {
                    lines.push(mic_line(&readiness));
                }
                v_flex().pt_2().children(lines)
            })
            .child(
                h_flex()
                    .pt_2()
                    .gap_2()
                    .child(
                        Button::new("dictation-save")
                            .small()
                            .primary()
                            .label("Save")
                            .on_click(cx.listener(|this, _, _, cx| this.save_dictation(cx))),
                    )
                    .children(d.note.as_ref().map(note_element)),
            )
            .child(hint(
                "The mic button appears in every composer once dictation is saved as Local or \
                 Provider. A recording becomes an editable draft in the composer and is never \
                 sent on its own; a recording in flight keeps its own copy of these settings.",
            ))
    }
}

#[cfg(test)]
mod tests {
    // Named imports, not `super::*`: the glob would also pull in the gpui test
    // attribute and shadow the built-in `#[test]`.
    use super::{
        Language, Local, LocalEngine, Mode, Note, NoteKind, Provider, Settings, bootstrap_for,
        dictation, local_status, provider_status, recommendation, save_plan, saved_note,
    };

    fn whisper_settings(executable: &str, model: &str) -> Settings {
        Settings {
            mode: Mode::Local,
            local: Local {
                engine: LocalEngine::Whisper,
                executable: executable.to_string(),
                model: model.to_string(),
                language: Language::Auto,
            },
            provider: Provider::default(),
        }
    }

    fn vosk_settings(executable: &str, model: &str) -> Settings {
        Settings {
            mode: Mode::Local,
            local: Local {
                engine: LocalEngine::Vosk,
                executable: executable.to_string(),
                model: model.to_string(),
                language: Language::Auto,
            },
            provider: Provider::default(),
        }
    }

    fn provider_settings(env: &str) -> Settings {
        Settings {
            mode: Mode::Provider,
            local: Local::default(),
            provider: Provider {
                endpoint: "https://example.com/v1/transcriptions".to_string(),
                model: "a-model".to_string(),
                api_key_env: env.to_string(),
            },
        }
    }

    #[test]
    fn a_missing_local_path_names_the_expected_file_or_directory() {
        let whisper = local_status(LocalEngine::Whisper, "", "  ");
        assert!(!whisper[0].present && !whisper[1].present);
        assert!(
            whisper[0].message.contains("whisper-cli"),
            "{}",
            whisper[0].message
        );
        assert!(
            whisper[1].message.contains("model file"),
            "{}",
            whisper[1].message
        );
        assert!(!whisper[1].message.contains("directory"));

        let vosk = local_status(LocalEngine::Vosk, "", "");
        assert_eq!(vosk[0].label, "Python (venv)");
        assert_eq!(vosk[1].label, "Model directory");
        assert!(
            vosk[0].message.contains("isolated Python"),
            "{}",
            vosk[0].message
        );
        assert!(
            vosk[1].message.contains("Vosk model directory"),
            "{}",
            vosk[1].message
        );
        assert!(
            !vosk[1].message.to_ascii_lowercase().contains("whisper"),
            "Vosk guidance must not describe a whisper model: {}",
            vosk[1].message
        );
    }

    #[test]
    fn an_existing_path_of_the_wrong_kind_is_not_reported_as_found() {
        let dir = tempfile::tempdir().unwrap();
        let executable = dir.path().join("whisper-cli");
        std::fs::write(&executable, b"x").unwrap();
        // A DIRECTORY where whisper.cpp expects its model FILE: a path exists,
        // but the expected kind does not, so the advisory line says so.
        let model = dir.path().join("ggml-base.bin");
        std::fs::create_dir(&model).unwrap();
        let whisper = whisper_settings(
            &executable.display().to_string(),
            &model.display().to_string(),
        );
        let status = local_status(
            LocalEngine::Whisper,
            &whisper.local.executable,
            &whisper.local.model,
        );
        assert!(status[0].present, "{}", status[0].message);
        assert!(!status[1].present);
        assert!(
            status[1].message.contains("not of that kind"),
            "{}",
            status[1].message
        );
        assert!(
            !status[1].message.contains("found"),
            "{}",
            status[1].message
        );
        // The mic's own rule is deliberately untouched (it accepts any existing
        // path); the section reports the expected kind, not the mic's verdict.
        assert!(
            dictation::ensure_ready(&whisper).is_ok(),
            "the unchanged mic rule still accepts an existing path"
        );

        // Vosk wants the Python as a FILE and the model as a DIRECTORY: a
        // directory in the Python field and a file in the model field are both
        // present-but-wrong, never found.
        let model_file = dir.path().join("model.bin");
        std::fs::write(&model_file, b"m").unwrap();
        let vosk = vosk_settings(
            &dir.path().display().to_string(),
            &model_file.display().to_string(),
        );
        let status = local_status(LocalEngine::Vosk, &vosk.local.executable, &vosk.local.model);
        assert!(!status[0].present && !status[1].present);
        assert!(
            status[0].message.contains("not of that kind"),
            "{}",
            status[0].message
        );
        assert!(dictation::ensure_ready(&vosk).is_err());
    }

    #[test]
    fn the_readiness_lines_agree_with_the_mic_on_a_local_form() {
        let dir = tempfile::tempdir().unwrap();
        let python = dir.path().join("python.exe");
        std::fs::write(&python, b"p").unwrap();
        let model = dir.path().join("vosk-model-small-es-0.42");
        std::fs::create_dir(&model).unwrap();
        let ready = vosk_settings(&python.display().to_string(), &model.display().to_string());
        let status = local_status(
            LocalEngine::Vosk,
            &ready.local.executable,
            &ready.local.model,
        );
        assert!(status[0].present && status[1].present);
        assert!(
            dictation::ensure_ready(&ready).is_ok(),
            "the mic accepts what the section calls ready"
        );

        // A model that is a FILE is not a Vosk model directory: both say no.
        let wrong = dir.path().join("model.bin");
        std::fs::write(&wrong, b"m").unwrap();
        let unready = vosk_settings(&python.display().to_string(), &wrong.display().to_string());
        let status = local_status(
            LocalEngine::Vosk,
            &unready.local.executable,
            &unready.local.model,
        );
        assert!(status[0].present && !status[1].present);
        assert!(dictation::ensure_ready(&unready).is_err());
    }

    #[test]
    fn the_provider_line_reports_presence_not_connectivity() {
        let settings = provider_settings("GENTLE_SYNTHETIC_TEST_KEY");
        let present = provider_status(&settings, &Ok(())).expect("a complete form gets a line");
        assert!(present.present);
        assert!(
            present.message.contains("GENTLE_SYNTHETIC_TEST_KEY"),
            "{}",
            present.message
        );
        assert!(
            present.message.contains("not contacted"),
            "{}",
            present.message
        );
        for claim in ["connected", "works", "valid key"] {
            assert!(
                !present.message.contains(claim),
                "no connectivity claim ({claim}): {}",
                present.message
            );
        }

        let missing = provider_status(
            &settings,
            &Err(settings
                .provider_blocker(false)
                .expect("a missing variable")),
        )
        .expect("the variable's own blocker is reported");
        assert!(!missing.present);
        assert!(
            missing.message.contains("not present"),
            "{}",
            missing.message
        );
        assert!(
            missing.message.contains("only the name is saved"),
            "{}",
            missing.message
        );

        // A reason that is not the variable's own blocker (an endpoint or
        // provider error, say) is unknown, never reported as "not present".
        assert!(
            provider_status(
                &settings,
                &Err("The endpoint must use https://".to_string())
            )
            .is_none()
        );

        // No usable name: nothing to ask the environment about.
        assert!(provider_status(&provider_settings(""), &Ok(())).is_none());
        assert!(provider_status(&provider_settings("  "), &Ok(())).is_none());
        assert!(provider_status(&provider_settings("MY KEY"), &Ok(())).is_none());
    }

    #[test]
    fn saving_reports_plain_success_only_when_the_mic_is_ready() {
        let ready = saved_note(
            &whisper_settings("/x/whisper-cli", "/x/ggml-base.bin"),
            Ok(()),
        );
        assert_eq!(ready, Note::ok("Dictation settings saved."));

        let settings = whisper_settings("/nowhere/whisper-cli", "/nowhere/ggml-base.bin");
        assert!(
            save_plan(&settings).is_ok(),
            "valid syntax is written, never newly refused for readiness"
        );
        let readiness = dictation::ensure_ready(&settings);
        assert!(readiness.is_err());
        let note = saved_note(&settings, readiness);
        assert_eq!(note.kind, NoteKind::Warning);
        assert!(
            note.message
                .starts_with("Dictation settings saved, but not ready:"),
            "{}",
            note.message
        );
        assert_ne!(note.message, "Dictation settings saved.");
    }

    #[test]
    fn saving_off_is_not_a_readiness_warning() {
        let note = saved_note(
            &Settings::default(),
            dictation::ensure_ready(&Settings::default()),
        );
        assert_eq!(note.kind, NoteKind::Ok);
        assert_eq!(note.message, "Dictation settings saved.");
    }

    #[test]
    fn invalid_syntax_is_refused_by_the_save_plan() {
        // Pure plan only: `save_dictation` reaches the writer only on `Ok`, but
        // these tests observe the refusal, not the absence of a write.
        let refusal = save_plan(&whisper_settings("", "  ")).unwrap_err();
        assert_eq!(refusal.kind, NoteKind::Failed);
        assert_eq!(
            refusal.message,
            "Local dictation is missing the whisper-cli executable path and the model path."
        );

        let mut provider = provider_settings("9LIVES");
        let refusal = save_plan(&provider).unwrap_err();
        assert_eq!(
            refusal.message,
            "The API key environment variable's name must be letters, digits and underscores."
        );

        provider.provider.api_key_env = "GENTLE_SYNTHETIC_TEST_KEY".to_string();
        assert!(save_plan(&provider).is_ok());
    }

    #[test]
    fn the_guided_setup_recommends_the_selected_engine() {
        let vosk = recommendation(LocalEngine::Vosk);
        assert!(vosk.contains("Vosk"), "{vosk}");
        assert!(vosk.contains("vosk-model-small-es-0.42"), "{vosk}");
        assert!(vosk.contains("DIRECTORY"), "{vosk}");
        assert!(
            !vosk.to_ascii_lowercase().contains("whisper"),
            "Vosk guidance must not recommend Whisper: {vosk}"
        );

        let whisper = recommendation(LocalEngine::Whisper);
        assert!(whisper.contains("whisper-cli"), "{whisper}");
        assert!(whisper.contains("model FILE"), "{whisper}");
        assert!(!whisper.contains("Vosk"), "{whisper}");
    }

    #[test]
    fn the_guided_setup_offers_the_selected_engines_script() {
        let bare = std::path::Path::new("no-such-checkout");
        assert_eq!(bootstrap_for(bare, LocalEngine::Whisper), None);

        let checkout = tempfile::tempdir().unwrap();
        std::fs::create_dir(checkout.path().join("scripts")).unwrap();
        for script in ["setup-voice-local.py", "setup-vosk-local.py"] {
            std::fs::write(checkout.path().join("scripts").join(script), b"#").unwrap();
        }

        let (script, command) = bootstrap_for(checkout.path(), LocalEngine::Whisper).unwrap();
        assert_eq!(script, "setup-voice-local.py");
        assert!(command.contains("setup-voice-local.py"), "{command}");
        assert!(!command.contains("vosk"), "{command}");

        let (script, command) = bootstrap_for(checkout.path(), LocalEngine::Vosk).unwrap();
        assert_eq!(script, "setup-vosk-local.py");
        assert!(command.contains("setup-vosk-local.py"), "{command}");
    }

    #[test]
    fn both_engines_save_the_same_two_paths_they_were_given() {
        for engine in [LocalEngine::Whisper, LocalEngine::Vosk] {
            let settings = dictation::form_settings(
                Mode::Local,
                Local {
                    engine,
                    executable: "  /opt/x/python  ".to_string(),
                    model: "  /opt/models/m  ".to_string(),
                    language: Language::Auto,
                },
                Provider::default(),
            );
            assert_eq!(settings.local.engine, engine);
            assert_eq!(settings.local.executable, "/opt/x/python");
            assert_eq!(settings.local.model, "/opt/models/m");
        }
    }
}
