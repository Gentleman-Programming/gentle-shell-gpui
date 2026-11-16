//! The prepared local setup of this workspace (`.local/voice/setup.json`,
//! written by `scripts/setup-voice-local.py`): read, validated against the
//! PUBLISHED pins — never against the editable manifest — and turned into
//! Dictation paths. The manifest is data, not instruction: only paths
//! inside this workspace's `.local/voice` are accepted, links are refused
//! in every ancestor, the source ZIP must match its published digest
//! before any per-file record is derived from it, the engine tree must
//! match the verified archive byte for byte, and the model must match the
//! published digest. Nothing here executes or downloads anything, and a
//! rejection never modifies the artifacts.

use serde_json::Value;
use std::path::{Path, PathBuf};

use sha2::{Digest as _, Sha256};

/// The schema marker the bootstrap writes; a manifest without it is not
/// ours and is never interpreted.
const SCHEMA: &str = "gentle-shell-voice-local-setup/1";

/// The published pins of the recommended first setup: immutable here, so a
/// hand-edited manifest can never weaken what "verified" means. The first
/// recommendation is deliberately limited to this one engine tag and this
/// one multilingual base model.
pub const RECOMMENDED_PINS: PublishedPins = PublishedPins {
    zip_sha256: "6ba69e3482d7826214f90a6a9c84ca07782aec1e1d0c6a7c30c994fd5d816ccb",
    model_sha256: "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe",
    model_size: 147_951_465,
};

/// What the recommended setup's artifacts must hash to. Tests inject a
/// tiny fixture's pins through the same seam; production always passes
/// [`RECOMMENDED_PINS`].
#[derive(Clone, Debug)]
pub struct PublishedPins {
    pub zip_sha256: &'static str,
    pub model_sha256: &'static str,
    pub model_size: u64,
}

/// The local paths a prepared setup offers, ready for the Dictation form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedSetup {
    pub executable: String,
    pub model: String,
}

/// Where the workspace's prepared manifest lives.
pub fn prepared_setup_path(project_root: &Path) -> PathBuf {
    project_root.join(".local").join("voice").join("setup.json")
}

/// Whether this workspace HAS a prepared setup worth offering — the cheap
/// checks only (manifest, schema, containment, links, existence), never
/// the hashing: this runs while rendering. The full verification against
/// the published pins happens off-thread when the user applies the setup.
pub fn prepared_setup_available(project_root: &Path) -> bool {
    present_prepared(project_root).is_ok()
}

/// The cheap half of [`verify_prepared`]: everything that does not hash.
pub fn present_prepared(project_root: &Path) -> Result<(), String> {
    let manifest_path = prepared_setup_path(project_root);
    let bytes = std::fs::read(&manifest_path)
        .map_err(|_| "There is no prepared local setup in this workspace.".to_string())?;
    let json: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|_| "The prepared local setup manifest is not valid JSON.".to_string())?;
    if json["schema"].as_str() != Some(SCHEMA) {
        return Err("The prepared local setup manifest is not ours.".to_string());
    }
    let local_root = PathBuf::from(json["local_root"].as_str().unwrap_or_default());
    let engine_dir = PathBuf::from(json["engine"]["dir"].as_str().unwrap_or_default());
    let main_executable = json["engine"]["main_executable"]
        .as_str()
        .unwrap_or_default();
    let model_file = PathBuf::from(json["model"]["file"].as_str().unwrap_or_default());
    let workspace_root = project_root
        .canonicalize()
        .map_err(|error| format!("This workspace's folder could not be resolved: {error}."))?;
    let voice_root = workspace_root.join(".local").join("voice");
    for (what, path) in [
        ("the local root", local_root.as_path()),
        ("the engine directory", engine_dir.as_path()),
        ("the model file", model_file.as_path()),
    ] {
        let resolved = path.canonicalize().map_err(|error| {
            format!("{what} of the prepared setup could not be resolved: {error}.")
        })?;
        if !resolved.starts_with(&voice_root) {
            return Err(format!(
                "{what} of the prepared setup must stay inside this workspace."
            ));
        }
    }
    for path in [&local_root, &engine_dir, &model_file] {
        if has_link_metadata(path) {
            return Err(format!(
                "{} of the prepared setup is a link; the app only accepts real files in                  this workspace.",
                path.display()
            ));
        }
    }
    if main_executable.is_empty()
        || Path::new(main_executable)
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(
            "The prepared setup's main executable must be a relative path inside the              engine directory."
                .to_string(),
        );
    }
    if !engine_dir.join(main_executable).is_file() {
        return Err(
            "The prepared setup's whisper-cli executable is missing from this workspace."
                .to_string(),
        );
    }
    if !model_file.is_file() {
        return Err("The prepared setup's model file is missing from this workspace.".to_string());
    }
    Ok(())
}

/// Read and fully verify the prepared manifest against `pins`. Any doubt —
/// wrong schema, a path outside the workspace, a link in any ancestor, a
/// hostile `main_executable`, a source ZIP that is not the pinned one, an
/// engine file that does not match the verified archive, a model that does
/// not match the published digest — is an error, and means "no offer".
/// A rejection never modifies the artifacts.
pub fn verify_prepared(project_root: &Path, pins: &PublishedPins) -> Result<PreparedSetup, String> {
    let manifest_path = prepared_setup_path(project_root);
    let bytes = std::fs::read(&manifest_path)
        .map_err(|_| "There is no prepared local setup in this workspace.".to_string())?;
    let json: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|_| "The prepared local setup manifest is not valid JSON.".to_string())?;
    if json["schema"].as_str() != Some(SCHEMA) {
        return Err("The prepared local setup manifest is not ours.".to_string());
    }
    let local_root = PathBuf::from(
        json["local_root"]
            .as_str()
            .ok_or("The prepared local setup manifest has no local root.")?,
    );
    let engine_dir = PathBuf::from(
        json["engine"]["dir"]
            .as_str()
            .ok_or("The prepared local setup manifest has no engine directory.")?,
    );
    let main_executable = json["engine"]["main_executable"]
        .as_str()
        .ok_or("The prepared local setup manifest has no main executable.")?;
    let model_file = PathBuf::from(
        json["model"]["file"]
            .as_str()
            .ok_or("The prepared local setup manifest has no model file.")?,
    );

    // Containment: every declared location must resolve inside this
    // workspace's `.local/voice`. Canonicalization also resolves any link
    // on the way, so an escape through one is caught here too.
    let workspace_root = project_root
        .canonicalize()
        .map_err(|error| format!("This workspace's folder could not be resolved: {error}."))?;
    let voice_root = workspace_root.join(".local").join("voice");
    for (what, path) in [
        ("the local root", local_root.as_path()),
        ("the engine directory", engine_dir.as_path()),
        ("the model file", model_file.as_path()),
    ] {
        let resolved = path.canonicalize().map_err(|error| {
            format!("{what} of the prepared setup could not be resolved: {error}.")
        })?;
        if !resolved.starts_with(&voice_root) {
            return Err(format!(
                "{what} of the prepared setup must stay inside this workspace."
            ));
        }
    }

    // No links in place, in the declared entries…
    for path in [&local_root, &engine_dir, &model_file] {
        if has_link_metadata(path) {
            return Err(format!(
                "{} of the prepared setup is a link; the app only accepts real files in \
                 this workspace.",
                path.display()
            ));
        }
    }
    // …and in every ancestor component of the engine directory.
    let mut ancestor = engine_dir.as_path();
    while let Some(parent) = ancestor.parent() {
        if has_link_metadata(parent) {
            return Err(format!(
                "{} of the prepared setup is a link; the app only accepts real files in \
                 this workspace.",
                parent.display()
            ));
        }
        ancestor = parent;
        if ancestor == workspace_root {
            break;
        }
    }

    // `main_executable` is untrusted data joined onto the engine directory:
    // it must be a plain relative path with no traversal and no absolute,
    // drive or UNC form, and its result must stay inside the engine
    // directory and inside `.local/voice`.
    let relative = Path::new(main_executable);
    if main_executable.is_empty()
        || relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(
            "The prepared setup's main executable must be a relative path inside the \
             engine directory."
                .to_string(),
        );
    }
    let executable = engine_dir.join(relative);
    let executable_canonical = executable.canonicalize().map_err(|error| {
        format!("The prepared setup's whisper-cli executable is missing or unreadable: {error}.")
    })?;
    let engine_canonical = engine_dir.canonicalize().map_err(|error| {
        format!("The prepared setup's engine directory could not be resolved: {error}.")
    })?;
    if !executable_canonical.starts_with(&engine_canonical)
        || !executable_canonical.starts_with(&voice_root)
    {
        return Err(
            "The prepared setup's main executable must resolve inside the engine directory \
             in this workspace."
                .to_string(),
        );
    }

    // The source archive is the trust root: verify its bytes against the
    // published pin BEFORE deriving any per-file record from it.
    let zip_path = find_source_zip(&workspace_root, &voice_root)?;
    let zip_bytes = std::fs::read(&zip_path)
        .map_err(|_| "The pinned whisper.cpp archive could not be read.".to_string())?;
    if sha256_hex(&zip_bytes) != pins.zip_sha256 {
        return Err(format!(
            "The whisper.cpp archive in this workspace does not match the published \
             archive (sha256 {}). Delete it and run the bootstrap again.",
            &pins.zip_sha256[..12]
        ));
    }
    verify_engine_against_zip(&engine_dir, &zip_bytes)?;
    verify_model(&model_file, pins)?;

    Ok(PreparedSetup {
        executable: executable.display().to_string(),
        model: model_file.display().to_string(),
    })
}

/// `load_prepared` is `verify_prepared` with the immutable published pins:
/// a manifest can never weaken what "verified" means.
pub fn load_prepared(project_root: &Path) -> Result<PreparedSetup, String> {
    verify_prepared(project_root, &RECOMMENDED_PINS)
}

/// The pinned source archive, where the bootstrap puts it.
fn find_source_zip(_workspace_root: &Path, voice_root: &Path) -> Result<PathBuf, String> {
    let candidate = voice_root.join("downloads").join("whisper-bin-x64.zip");
    if candidate.is_file() {
        return Ok(candidate);
    }
    Err(
        "The pinned whisper.cpp archive (whisper-bin-x64.zip) is not in this workspace's \
         downloads; run the bootstrap again."
            .to_string(),
    )
}

/// Every file member of the verified archive must exist in the engine
/// directory with the same bytes, and no extra executable may sit there.
/// A mismatch is an error; nothing is overwritten or executed.
fn verify_engine_against_zip(engine_dir: &Path, zip_bytes: &[u8]) -> Result<(), String> {
    let cursor = std::io::Cursor::new(zip_bytes);
    let mut archive = zip::ZipArchive::new(cursor)
        .map_err(|_| "The pinned whisper.cpp archive is unreadable.".to_string())?;
    let mut expected: Vec<(String, u64, String)> = Vec::new();
    for index in 0..archive.len() {
        let mut member = archive
            .by_index(index)
            .map_err(|_| "The pinned whisper.cpp archive is unreadable.".to_string())?;
        if member.is_dir() {
            continue;
        }
        let name = member
            .name()
            .map_err(|_| "The pinned whisper.cpp archive is unreadable.".to_string())?
            .replace('\\', "/");
        let mut bytes = Vec::with_capacity(member.size() as usize);
        std::io::Read::read_to_end(&mut member, &mut bytes)
            .map_err(|_| "The pinned whisper.cpp archive is unreadable.".to_string())?;
        expected.push((name, member.size(), sha256_hex(&bytes)));
    }
    if expected.is_empty() {
        return Err("The pinned whisper.cpp archive is empty.".to_string());
    }
    for (name, size, hash) in &expected {
        let member_path = engine_dir.join(name.replace('/', std::path::MAIN_SEPARATOR_STR));
        let bytes = std::fs::read(&member_path).map_err(|_| {
            format!(
                "The prepared engine is missing {name} from the verified archive; run the \
                 bootstrap again."
            )
        })?;
        if bytes.len() as u64 != *size || sha256_hex(&bytes) != *hash {
            return Err(format!(
                "The prepared engine's {name} does not match the verified archive; run the \
                 bootstrap again."
            ));
        }
    }
    // No extra executables the archive never shipped.
    let archived: std::collections::HashSet<String> =
        expected.iter().map(|(name, _, _)| name.clone()).collect();
    let mut extras = Vec::new();
    collect_extra_executables(engine_dir, engine_dir, &archived, &mut extras);
    if let Some(extra) = extras.first() {
        return Err(format!(
            "The prepared engine contains {} which is not in the verified archive; remove \
             it or run the bootstrap again.",
            extra.display()
        ));
    }
    Ok(())
}

fn collect_extra_executables(
    root: &Path,
    dir: &Path,
    archived: &std::collections::HashSet<String>,
    extras: &mut Vec<PathBuf>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_extra_executables(root, &path, archived, extras);
            continue;
        }
        let is_executable = path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("exe") || ext.eq_ignore_ascii_case("dll"));
        if !is_executable {
            continue;
        }
        let Ok(relative) = path.strip_prefix(root) else {
            continue;
        };
        let name = relative.to_string_lossy().replace('\\', "/");
        if !archived.contains(&name) {
            extras.push(path);
        }
    }
}

/// The model must hash to the published digest and have the published
/// size; the manifest's own numbers are ignored for trust.
fn verify_model(model_file: &Path, pins: &PublishedPins) -> Result<(), String> {
    let bytes = std::fs::read(model_file)
        .map_err(|_| "The prepared setup's model file could not be read.".to_string())?;
    if bytes.len() as u64 != pins.model_size {
        return Err(format!(
            "The prepared setup's model is {} bytes; the published recommended model is {}.",
            bytes.len(),
            pins.model_size
        ));
    }
    if sha256_hex(&bytes) != pins.model_sha256 {
        return Err(
            "The prepared setup's model does not match the published recommended model; \
             download it again with the bootstrap."
                .to_string(),
        );
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// Whether `path` sits on a link (symlink, or a Windows junction/reparse
/// point): such an entry could redirect outside the workspace, so the
/// prepared setup refuses them instead of following.
pub fn has_link_metadata(path: &Path) -> bool {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return true; // unreadable: treat as untrustworthy
    };
    #[cfg(unix)]
    {
        metadata.file_type().is_symlink()
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
}

/// The prepared Vosk manifest: `.local/voice/vosk/setup.json`, written by
/// `scripts/setup-vosk-local.py`. One contract: the isolated venv Python,
/// the model directory, and the model's recorded LOCAL digest
/// (`model.sha256_local`) - which is verified here against the CURRENT
/// model tree before anything is offered. The digest is a LOCAL
/// first-download integrity record, not a publisher attestation, and it
/// is REQUIRED: a manifest without it is refused, never accepted silently.
pub const VOSK_SCHEMA: &str = "gentle-shell-voice-vosk-setup/1";

/// The digest recipe recorded in the manifest, shared with
/// `scripts/setup-vosk-local.py` and `scripts/launch-voice-sandbox.py`.
/// A digest without a recorded recipe cannot be verified, so it is refused.
pub const VOSK_DIGEST_ALGORITHM: &str = "sha256-tree/1";

/// Whether there is a Vosk setup worth OFFERING. This runs while rendering,
/// so it never hashes or reads the model: only the manifest shape, the
/// containment rules and existence. The full digest check happens off-thread
/// when the user applies the setup ([`load_prepared_vosk`]).
pub fn prepared_vosk_available(project_root: &Path) -> bool {
    present_prepared_vosk(project_root).is_ok()
}

/// The cheap half of [`load_prepared_vosk`]: everything that does not hash.
pub fn present_prepared_vosk(project_root: &Path) -> Result<(), String> {
    let (python, model_dir, _recorded, _algorithm, _json) = read_vosk_manifest(project_root)?;
    if !python.is_file() {
        return Err("The prepared Vosk setup's venv Python is missing.".to_string());
    }
    if !model_dir.is_dir() {
        return Err("The prepared Vosk setup's model directory is missing.".to_string());
    }
    Ok(())
}

/// The manifest fields both halves need, with the containment and link rules
/// applied. Returns the recorded digest and its recipe verbatim.
fn read_vosk_manifest(
    project_root: &Path,
) -> Result<(PathBuf, PathBuf, String, String, serde_json::Value), String> {
    let manifest_path = project_root
        .join(".local")
        .join("voice")
        .join("vosk")
        .join("setup.json");
    let bytes = std::fs::read(&manifest_path)
        .map_err(|_| "There is no prepared Vosk setup in this workspace.".to_string())?;
    let json: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|_| "The prepared Vosk manifest is not valid JSON.".to_string())?;
    if json["schema"].as_str() != Some(VOSK_SCHEMA) {
        return Err("The prepared Vosk manifest is not ours.".to_string());
    }
    let python = PathBuf::from(json["python"].as_str().unwrap_or_default());
    let model_dir = PathBuf::from(json["model_dir"].as_str().unwrap_or_default());
    let recorded = json["model"]
        .get("sha256_local")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if recorded.is_empty() {
        return Err("The prepared Vosk manifest records no model digest.".to_string());
    }
    let algorithm = json["model"]
        .get("digest_algorithm")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let workspace_root = project_root
        .canonicalize()
        .map_err(|error| format!("This workspace's folder could not be resolved: {error}."))?;
    let vosk_root = workspace_root.join(".local").join("voice").join("vosk");
    for (what, path) in [
        ("the venv Python", python.as_path()),
        ("the model directory", model_dir.as_path()),
    ] {
        let resolved = path.canonicalize().map_err(|error| {
            format!("{what} of the prepared Vosk setup could not be resolved: {error}.")
        })?;
        if !resolved.starts_with(&vosk_root) {
            return Err(format!(
                "{what} of the prepared Vosk setup must stay inside this workspace."
            ));
        }
    }
    for path in [&python, &model_dir] {
        if has_link_metadata(path) {
            return Err(format!(
                "{} of the prepared Vosk setup is a link; the app only accepts real files.",
                path.display()
            ));
        }
    }
    Ok((python, model_dir, recorded, algorithm, json))
}

/// The documented `sha256-tree/1` digest: every file under `model_dir`,
/// sorted by its POSIX relative path, hashing `path NUL length NUL content`.
/// Links are refused instead of followed.
pub fn vosk_model_digest(model_dir: &Path) -> Result<String, String> {
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    let mut pending = vec![model_dir.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let entries = std::fs::read_dir(&directory)
            .map_err(|_| "The Vosk model could not be read.".to_string())?;
        for entry in entries.flatten() {
            let path = entry.path();
            if has_link_metadata(&path) {
                return Err(format!(
                    "The Vosk model contains a link at {}; refusing.",
                    path.display()
                ));
            }
            let metadata = entry
                .metadata()
                .map_err(|_| "The Vosk model could not be read.".to_string())?;
            if metadata.is_dir() {
                pending.push(path);
                continue;
            }
            let relative = path
                .strip_prefix(model_dir)
                .map_err(|_| "The Vosk model could not be read.".to_string())?
                .components()
                .map(|part| part.as_os_str().to_string_lossy().to_string())
                .collect::<Vec<_>>()
                .join("/");
            files.push((relative, path));
        }
    }
    files.sort_by(|left, right| left.0.cmp(&right.0));
    let mut digest = Sha256::new();
    for (relative, path) in files {
        let bytes =
            std::fs::read(&path).map_err(|_| "The Vosk model could not be read.".to_string())?;
        digest.update(relative.as_bytes());
        digest.update([0u8]);
        digest.update(bytes.len().to_string().as_bytes());
        digest.update([0u8]);
        digest.update(&bytes);
    }
    Ok(format!("{:x}", digest.finalize()))
}

pub fn load_prepared_vosk(project_root: &Path) -> Result<PreparedSetup, String> {
    let (python, model_dir, recorded, algorithm, _json) = read_vosk_manifest(project_root)?;
    if algorithm != VOSK_DIGEST_ALGORITHM {
        return Err(format!(
            "The prepared Vosk manifest does not record the {VOSK_DIGEST_ALGORITHM} digest              recipe, so it cannot be verified; run the Vosk setup again."
        ));
    }
    if !python.is_file() {
        return Err("The prepared Vosk setup's venv Python is missing.".to_string());
    }
    if !model_dir.is_dir() {
        return Err("The prepared Vosk setup's model directory is missing.".to_string());
    }
    // The recorded LOCAL digest over the CURRENT model tree: tampered or
    // replaced models are refused, never silently re-pinned.
    if vosk_model_digest(&model_dir)? != recorded {
        return Err(
            "The prepared Vosk model does not match its recorded digest; run the Vosk setup again."
                .to_string(),
        );
    }
    Ok(PreparedSetup {
        executable: python.display().to_string(),
        model: model_dir.display().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::{
        PreparedSetup, PublishedPins, load_prepared, load_prepared_vosk, prepared_setup_path,
        verify_prepared,
    };
    use std::io::Write as _;
    use std::path::PathBuf;

    /// A fixture that mirrors the published manifest's shape, with files
    /// and sizes the test controls, plus a tiny source ZIP standing in for
    /// the pinned whisper.cpp archive.
    struct Fixture {
        root: PathBuf,
        zip_sha: String,
        model_sha: String,
        _dir: tempfile::TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            // macOS puts temporary folders under `/var`, which is itself a
            // link, and the guard under test refuses a linked ancestor of the
            // workspace. Resolve the fixture root first so the fixture is a
            // plain tree, the way an ordinary workspace path already is.
            #[cfg(unix)]
            let root = std::fs::canonicalize(dir.path()).unwrap();
            #[cfg(not(unix))]
            let root = dir.path().to_path_buf();
            let voice = root.join(".local").join("voice");
            std::fs::create_dir_all(voice.join("models")).unwrap();
            std::fs::create_dir_all(voice.join("engine").join("e-1").join("Release")).unwrap();
            std::fs::create_dir_all(voice.join("downloads")).unwrap();

            // The engine tree: two files, matching the tiny ZIP below.
            std::fs::write(
                voice.join("engine/e-1/Release/whisper-cli.exe"),
                b"engine-exe",
            )
            .unwrap();
            std::fs::write(voice.join("engine/e-1/Release/ggml.dll"), b"engine-dll").unwrap();

            // The source ZIP holding exactly those bytes.
            let zip_path = voice.join("downloads/whisper-bin-x64.zip");
            let zip_file = std::fs::File::create(&zip_path).unwrap();
            let mut zip = zip::ZipWriter::new(zip_file);
            zip.add_directory("Release", zip::write::SimpleFileOptions::default())
                .unwrap();
            let options = zip::write::SimpleFileOptions::default();
            zip.start_file("Release/whisper-cli.exe", options).unwrap();
            zip.write_all(b"engine-exe").unwrap();
            zip.start_file("Release/ggml.dll", options).unwrap();
            zip.write_all(b"engine-dll").unwrap();
            let zip_file = zip.finish().unwrap();
            drop(zip_file);

            // The model: content whose hash the fixture pins carry.
            std::fs::write(voice.join("models/ggml-base.bin"), b"model-bytes").unwrap();

            let sha = |bytes: &[u8]| {
                use sha2::{Digest as _, Sha256};
                let mut hasher = Sha256::new();
                hasher.update(bytes);
                format!("{:x}", hasher.finalize())
            };
            let zip_sha = sha(&std::fs::read(&zip_path).unwrap());
            let model_sha = sha(b"model-bytes");
            Self {
                root,
                zip_sha,
                model_sha,
                _dir: dir,
            }
        }

        fn pins(&self) -> PublishedPins {
            PublishedPins {
                zip_sha256: Box::leak(self.zip_sha.clone().into_boxed_str()),
                model_sha256: Box::leak(self.model_sha.clone().into_boxed_str()),
                model_size: 11,
            }
        }

        fn manifest(&self, json: &str) {
            std::fs::write(prepared_setup_path(&self.root), json).unwrap();
        }

        fn manifest_with_main_executable(&self, main_executable: &str) -> String {
            let local = self.root.join(".local").join("voice");
            let engine_dir = local.join("engine").join("e-1");
            format!(
                r#"{{"schema": "gentle-shell-voice-local-setup/1",
                    "project_root": "{}",
                    "local_root": "{}",
                    "engine": {{"dir": "{}", "main_executable": "{}"}},
                    "model": {{"file": "{}", "size": 11}}}}"#,
                self.root.display(),
                local.display(),
                engine_dir.display(),
                main_executable,
                local.join("models").join("ggml-base.bin").display(),
            )
            .replace('\\', "\\\\")
        }

        fn published(&self) -> String {
            self.manifest_with_main_executable("Release/whisper-cli.exe")
        }
    }

    fn prepared(executable: PathBuf, model: PathBuf) -> Result<PreparedSetup, String> {
        Ok(PreparedSetup {
            executable: executable.display().to_string(),
            model: model.display().to_string(),
        })
    }

    #[test]
    fn a_published_manifest_loads_into_dictation_paths() {
        let f = Fixture::new();
        f.manifest(&f.published());
        let local = f.root.join(".local").join("voice");
        assert_eq!(
            verify_prepared(&f.root, &f.pins()),
            prepared(
                local
                    .join("engine")
                    .join("e-1")
                    .join("Release/whisper-cli.exe"),
                local.join("models").join("ggml-base.bin"),
            )
        );
    }

    #[test]
    fn a_missing_or_foreign_manifest_is_no_offer() {
        let f = Fixture::new();
        assert!(
            verify_prepared(&f.root, &f.pins()).is_err(),
            "no manifest at all"
        );

        // A manifest without our schema marker is not ours to trust.
        f.manifest(r#"{"schema": "someone-elses/9"}"#);
        assert!(verify_prepared(&f.root, &f.pins()).is_err());
    }

    #[test]
    fn paths_outside_the_workspace_are_refused() {
        let f = Fixture::new();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("model.bin"), b"model-bytes").unwrap();
        let local = f.root.join(".local").join("voice");
        let manifest = format!(
            r#"{{"schema": "gentle-shell-voice-local-setup/1",
                "project_root": "{}",
                "local_root": "{}",
                "engine": {{"dir": "{}", "main_executable": "Release/whisper-cli.exe"}},
                "model": {{"file": "{}", "size": 11}}}}"#,
            f.root.display(),
            local.display(),
            f.root.join(".local/voice/engine/e-1").display(),
            outside.path().join("model.bin").display(),
        )
        .replace('\\', "\\\\");
        f.manifest(&manifest);
        let error = verify_prepared(&f.root, &f.pins()).unwrap_err();
        assert!(error.contains("inside this workspace"), "{error}");
    }

    #[test]
    fn the_main_executable_must_be_a_plain_relative_path() {
        let f = Fixture::new();
        // Each entry has to be hostile on the platform running it: a
        // back-slashed Windows path is an ordinary relative file name on
        // POSIX, and a leading `/` is one on Windows.
        #[cfg(windows)]
        let hostile = [
            // Absolute (Windows drive and POSIX root)…
            "C:\\evil\\whisper-cli.exe",
            "/usr/bin/evil",
            // UNC…
            "\\\\server\\share\\whisper-cli.exe",
            // …and traversal out of the engine directory.
            "..\\..\\evil.exe",
            "Release/../../evil.exe",
        ];
        #[cfg(unix)]
        let hostile = [
            // Absolute…
            "/usr/bin/evil",
            // …and traversal out of the engine directory, in both the bare
            // and the tucked-after-a-normal-component form.
            "../../evil",
            "Release/../../evil",
        ];
        for entry in hostile {
            f.manifest(&f.manifest_with_main_executable(entry));
            let error = verify_prepared(&f.root, &f.pins()).unwrap_err();
            assert!(
                error.contains("relative path") || error.contains("inside"),
                "{entry}: {error}"
            );
        }
        // No side effects: the traversal target was never touched or created.
        assert!(!f.root.join("evil.exe").exists());
        assert!(!f.root.parent().unwrap().join("evil.exe").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_link_anywhere_in_the_engine_ancestry_is_refused() {
        let f = Fixture::new();
        let release = f.root.join(".local/voice/engine/e-1/Release");
        let real = tempfile::tempdir().unwrap();
        std::fs::write(real.path().join("whisper-cli.exe"), b"engine-exe").unwrap();
        std::fs::write(real.path().join("ggml.dll"), b"engine-dll").unwrap();
        std::fs::remove_dir_all(&release).unwrap();
        std::os::unix::fs::symlink(real.path(), &release).unwrap();
        f.manifest(&f.published());
        assert!(
            verify_prepared(&f.root, &f.pins()).is_err(),
            "a linked engine directory must be refused"
        );
    }

    #[test]
    fn the_engine_tree_must_match_the_verified_source_zip() {
        let f = Fixture::new();
        f.manifest(&f.published());

        // A tampered DLL (the manifest's own hashes are not trusted) is
        // caught against the record derived from the verified ZIP.
        std::fs::write(
            f.root.join(".local/voice/engine/e-1/Release/ggml.dll"),
            b"tampered",
        )
        .unwrap();
        let error = verify_prepared(&f.root, &f.pins()).unwrap_err();
        assert!(error.contains("does not match"), "{error}");
        // The tampered file is left exactly as it was: no overwrites.
        assert_eq!(
            std::fs::read(f.root.join(".local/voice/engine/e-1/Release/ggml.dll")).unwrap(),
            b"tampered"
        );

        // An unexpected executable that the ZIP never shipped is rejected.
        std::fs::write(
            f.root.join(".local/voice/engine/e-1/Release/ggml.dll"),
            b"engine-dll",
        )
        .unwrap();
        std::fs::write(
            f.root.join(".local/voice/engine/e-1/Release/extra.exe"),
            b"surprise",
        )
        .unwrap();
        let error = verify_prepared(&f.root, &f.pins()).unwrap_err();
        assert!(error.contains("not in the verified archive"), "{error}");
        std::fs::remove_file(f.root.join(".local/voice/engine/e-1/Release/extra.exe")).unwrap();
        assert!(verify_prepared(&f.root, &f.pins()).is_ok());
    }

    #[test]
    fn the_source_zip_itself_must_match_the_published_pin() {
        let f = Fixture::new();
        f.manifest(&f.published());
        // The ZIP on disk is not the one the pin describes.
        std::fs::write(
            f.root.join(".local/voice/downloads/whisper-bin-x64.zip"),
            b"not the archive",
        )
        .unwrap();
        let error = verify_prepared(&f.root, &f.pins()).unwrap_err();
        assert!(error.contains("archive"), "{error}");
    }

    #[test]
    fn the_model_must_match_the_published_digest_not_the_manifest() {
        let f = Fixture::new();
        f.manifest(&f.published());
        std::fs::write(
            f.root.join(".local/voice/models/ggml-base.bin"),
            b"tampered model",
        )
        .unwrap();
        let error = verify_prepared(&f.root, &f.pins()).unwrap_err();
        assert!(error.contains("model"), "{error}");
        assert_eq!(
            std::fs::read(f.root.join(".local/voice/models/ggml-base.bin")).unwrap(),
            b"tampered model",
            "the model is rejected, never overwritten"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_model_is_refused() {
        let f = Fixture::new();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("model.bin"), b"model-bytes").unwrap();
        let link = f.root.join(".local/voice/models/ggml-base.bin");
        // The fixture wrote a real model here; put the link in its place.
        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(outside.path().join("model.bin"), &link).unwrap();
        f.manifest(&f.published());
        assert!(
            verify_prepared(&f.root, &f.pins()).is_err(),
            "a symlink must be refused"
        );
    }

    #[test]
    fn link_metadata_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("real.bin");
        std::fs::write(&target, b"x").unwrap();
        assert!(!super::has_link_metadata(&target));
        #[cfg(unix)]
        {
            let link = dir.path().join("link.bin");
            std::os::unix::fs::symlink(&target, &link).unwrap();
            assert!(super::has_link_metadata(&link));
        }
    }

    /// The documented ``sha256-tree/1`` recipe, written out longhand so the
    /// test proves the contract independently of the implementation.
    fn tree_digest_of(model: &std::path::Path) -> String {
        use sha2::{Digest as _, Sha256};
        let mut files = Vec::new();
        let mut pending = vec![model.to_path_buf()];
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(&dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    pending.push(path);
                } else {
                    files.push(path);
                }
            }
        }
        let mut relative: Vec<(String, std::path::PathBuf)> = files
            .into_iter()
            .map(|path| {
                (
                    path.strip_prefix(model)
                        .unwrap()
                        .components()
                        .map(|part| part.as_os_str().to_string_lossy().to_string())
                        .collect::<Vec<_>>()
                        .join("/"),
                    path,
                )
            })
            .collect();
        relative.sort_by(|left, right| left.0.cmp(&right.0));
        let mut hasher = Sha256::new();
        for (name, path) in relative {
            let bytes = std::fs::read(&path).unwrap();
            hasher.update(name.as_bytes());
            hasher.update([0u8]);
            hasher.update(bytes.len().to_string().as_bytes());
            hasher.update([0u8]);
            hasher.update(&bytes);
        }
        format!("{:x}", hasher.finalize())
    }

    fn write_vosk_fixture(root: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
        let vosk = root.join(".local").join("voice").join("vosk");
        let venv = vosk.join("venv").join("Scripts");
        std::fs::create_dir_all(&venv).unwrap();
        std::fs::write(venv.join("python.exe"), b"py").unwrap();
        let model = vosk.join("models").join("small-es");
        std::fs::create_dir_all(model.join("am")).unwrap();
        std::fs::create_dir_all(model.join("conf")).unwrap();
        std::fs::write(model.join("am").join("final.mdl"), b"weights").unwrap();
        std::fs::write(model.join("conf").join("model.conf"), b"a=v").unwrap();
        (venv.join("python.exe"), model)
    }

    #[test]
    fn a_prepared_vosk_setup_verifies_the_whole_model_tree() {
        // The installer (and the launcher) hash every file, not only the
        // top level: a nested model file is part of the recorded digest.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let (python, model) = write_vosk_fixture(&root);
        let manifest = serde_json::json!({
            "schema": super::VOSK_SCHEMA,
            "python": python.display().to_string(),
            "model_dir": model.display().to_string(),
            "model": {"sha256_local": tree_digest_of(&model),
                      "digest_algorithm": "sha256-tree/1"},
        });
        std::fs::write(
            root.join(".local/voice/vosk/setup.json"),
            serde_json::to_string(&manifest).unwrap(),
        )
        .unwrap();
        assert!(
            load_prepared_vosk(&root).is_ok(),
            "a manifest hashed over the whole tree must load"
        );

        // A nested change is refused (the old top-level-only digest missed it).
        std::fs::write(model.join("am").join("final.mdl"), b"tampered").unwrap();
        assert!(load_prepared_vosk(&root).is_err());
        assert_eq!(
            std::fs::read(model.join("am").join("final.mdl")).unwrap(),
            b"tampered",
            "refusal leaves the artifacts untouched"
        );
    }

    #[test]
    fn a_vosk_manifest_without_the_documented_algorithm_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let (python, model) = write_vosk_fixture(&root);
        let manifest = serde_json::json!({
            "schema": super::VOSK_SCHEMA,
            "python": python.display().to_string(),
            "model_dir": model.display().to_string(),
            "model": {"sha256_local": tree_digest_of(&model)},
        });
        std::fs::write(
            root.join(".local/voice/vosk/setup.json"),
            serde_json::to_string(&manifest).unwrap(),
        )
        .unwrap();
        assert!(
            load_prepared_vosk(&root).is_err(),
            "a digest with no recorded recipe cannot be verified, so it is refused"
        );
    }

    #[test]
    fn the_vosk_offer_is_cheap_and_apply_still_verifies() {
        // Rendering only needs "there is something to offer": presence must
        // not hash the model. Applying still refuses a wrong digest.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let (python, model) = write_vosk_fixture(&root);
        let manifest = serde_json::json!({
            "schema": super::VOSK_SCHEMA,
            "python": python.display().to_string(),
            "model_dir": model.display().to_string(),
            "model": {"sha256_local": "0".repeat(64),
                      "digest_algorithm": "sha256-tree/1"},
        });
        std::fs::write(
            root.join(".local/voice/vosk/setup.json"),
            serde_json::to_string(&manifest).unwrap(),
        )
        .unwrap();
        assert!(
            super::prepared_vosk_available(&root),
            "presence does not hash"
        );
        assert!(load_prepared_vosk(&root).is_err(), "apply verifies");
    }

    #[test]
    fn a_prepared_vosk_setup_loads_and_tamper_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let vosk = root.join(".local").join("voice").join("vosk");
        let venv = vosk.join("venv").join("Scripts");
        std::fs::create_dir_all(&venv).unwrap();
        std::fs::write(venv.join("python.exe"), b"py").unwrap();
        let model = vosk.join("models").join("small-es");
        std::fs::create_dir_all(&model).unwrap();
        std::fs::write(model.join("final.mdl"), b"weights").unwrap();
        let manifest = serde_json::json!({
            "schema": super::VOSK_SCHEMA,
            "python": venv.join("python.exe").display().to_string(),
            "model_dir": model.display().to_string(),
            "model": {"sha256_local": tree_digest_of(&model),
                      "digest_algorithm": "sha256-tree/1"},
        });
        std::fs::write(
            vosk.join("setup.json"),
            serde_json::to_string(&manifest).unwrap(),
        )
        .unwrap();
        let prepared = load_prepared_vosk(&root).unwrap();
        assert_eq!(
            prepared.executable,
            venv.join("python.exe").display().to_string()
        );

        // A tampered model is refused against the recorded LOCAL digest.
        std::fs::write(model.join("final.mdl"), b"tampered").unwrap();
        assert!(load_prepared_vosk(&root).is_err());
        assert_eq!(
            std::fs::read(model.join("final.mdl")).unwrap(),
            b"tampered",
            "refusal leaves the artifacts untouched"
        );
    }

    #[test]
    fn a_vosk_manifest_without_a_digest_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let vosk = root.join(".local").join("voice").join("vosk");
        let venv = vosk.join("venv").join("Scripts");
        std::fs::create_dir_all(&venv).unwrap();
        std::fs::write(venv.join("python.exe"), b"py").unwrap();
        let model = vosk.join("models").join("small-es");
        std::fs::create_dir_all(&model).unwrap();
        std::fs::write(model.join("final.mdl"), b"weights").unwrap();
        let manifest = serde_json::json!({
            "schema": super::VOSK_SCHEMA,
            "python": venv.join("python.exe").display().to_string(),
            "model_dir": model.display().to_string(),
        });
        std::fs::write(
            vosk.join("setup.json"),
            serde_json::to_string(&manifest).unwrap(),
        )
        .unwrap();
        assert!(load_prepared_vosk(&root).is_err());
    }

    #[test]
    fn presence_is_cheap_and_verification_is_not_skipped() {
        let f = Fixture::new();
        f.manifest(&f.published());
        // The right shape, wrong content: the cheap presence check offers
        // the button; the full verification still refuses at apply time.
        std::fs::write(
            f.root.join(".local/voice/models/ggml-base.bin"),
            b"wrong content",
        )
        .unwrap();
        assert!(super::present_prepared(&f.root).is_ok());
        assert!(load_prepared(&f.root).is_err());
    }

    #[test]
    fn load_prepared_uses_the_published_pins() {
        // load_prepared is verify_prepared with the immutable recommended
        // pins; a fixture cannot satisfy them, and that is the point.
        let f = Fixture::new();
        f.manifest(&f.published());
        assert!(load_prepared(&f.root).is_err());
    }
}
