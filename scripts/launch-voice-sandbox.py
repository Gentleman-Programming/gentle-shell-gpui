"""Launch a build of this checkout in an isolated sandbox for voice-dictation
testing, in parallel to any other execution of the app.

Everything the sandboxed app could write — the app's own settings, the Pi
agent home, the Gentle AI config, temp files, the single-instance lock —
lives under ``<repo>/.local/voice/sandbox/run-<timestamp>/``. The real user
configuration (%APPDATA%, ~/.pi, ~/.gentle-shell) is never read for state,
never written, and never copied: no auth.json, no provider secrets. The
sandbox's settings.json marks onboarding done (so no installer runs) and
pre-configures dictation with this workspace's verified local engine and
model from ``.local/voice/setup.json``.

The sandboxed app is free to resolve the installed Gentle Shell backend
launcher; its package internals and global config are not read or modified
by this script. Offline transcription needs no agent sign-in.

Usage (run from the repository root, by the parent):
    python -B scripts/launch-voice-sandbox.py             # build + launch
    python -B scripts/launch-voice-sandbox.py --prepare   # sandbox only, no app
    python -B scripts/launch-voice-sandbox.py --exe PATH  # non-default build
"""

from __future__ import annotations

import argparse
import io
import json
import os
import subprocess
import sys
import time
from pathlib import Path

sys.dont_write_bytecode = True

_HERE = Path(__file__).resolve().parent
_REPO_ROOT = _HERE.parent

APP_BINARY = "gentle-shell-gpui.exe"

#: The child's shell arguments: `--isolated` explicitly, so an inherited
#: `GENTLE_SHELL_ARGS` (for example `--home` pointing at the real agent
#: home, or `--link`) can never migrate the real agent state into or out
#: of the sandbox.
ISOLATED_SHELL_ARGS = "--isolated"

#: The published pins of the recommended first setup (immutable here): the
#: pinned whisper.cpp source archive and the recommended multilingual base
#: model. A manifest's own hashes are never trusted.
PUBLISHED_PINS = {
    "zip_sha256": "6ba69e3482d7826214f90a6a9c84ca07782aec1e1d0c6a7c30c994fd5d816ccb",
    "model_sha256": "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe",
    "model_size": 147951465,
}
DEFAULT_APP = Path("target") / "debug" / APP_BINARY
ONBOARDING_VERSION = "0.1.17"

#: Every environment variable the launcher points into the sandbox. The
#: child inherits the rest of the parent's environment unchanged, and this
#: process's own environment is never modified.
MANAGED_ENV = (
    "APPDATA",
    "GENTLE_SHELL_GPUI_DATA_DIR",
    "LOCALAPPDATA",
    "TEMP",
    "TMP",
    "GENTLE_PI_CONFIG_HOME",
    "GENTLE_SHELL_CONFIG",
    "GENTLE_SHELL_HOME",
    "GENTLE_SHELL_USER_PI_HOME",
    "PI_CODING_AGENT_DIR",
    "GENTLE_SHELL_ARGS",
)

_SCHEMA = "gentle-shell-voice-local-setup/1"


def _fail(message: str) -> "NoReturn":  # type: ignore[name-defined]
    print(f"launch-voice-sandbox: {message}", file=sys.stderr)
    raise SystemExit(2)


def _is_link(path: Path) -> bool:
    """Whether ``path`` sits on a symlink or a Windows junction/reparse
    point: the sandbox never follows links out of the workspace. A missing
    path is not a link (there is nothing to follow); any other read error
    is treated as untrustworthy."""
    if not os.path.lexists(path):
        return False
    try:
        st = os.lstat(path)
    except OSError:
        return True
    if os.path.islink(path):
        return True
    if sys.platform.startswith("win"):
        return bool(st.st_file_attributes & 0x0400)  # FILE_ATTRIBUTE_REPARSE_POINT
    return False


def _inside(child: Path, parent: Path) -> bool:
    try:
        Path(child).resolve().relative_to(Path(parent).resolve())
    except ValueError:
        return False
    return True


def _verify_prepared_voice(repo_root: Path, pins: dict) -> dict[str, str]:
    """Read and verify ``.local/voice/setup.json`` against the published
    pins: the source archive must match its pinned digest first, then every
    archive member must exist in the engine tree with the same bytes, no
    extra executable may sit there, and the model must match the published
    digest and size. The manifest's own hashes are never trusted. A
    rejection leaves every artifact exactly as it was."""
    import hashlib
    import zipfile

    manifest_path = repo_root / ".local" / "voice" / "setup.json"
    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        _fail(f"no usable prepared voice setup in this workspace ({error})")
    if manifest.get("schema") != _SCHEMA:
        _fail("the prepared voice manifest is not ours (wrong schema)")
    voice_root = repo_root / ".local" / "voice"
    engine_dir = Path(manifest["engine"]["dir"])
    model_file = Path(manifest["model"]["file"])
    for what, path in (
        ("engine directory", engine_dir),
        ("model file", model_file),
    ):
        if not _inside(path, voice_root):
            _fail(f"the prepared setup's {what} must stay inside {voice_root}")
        if _is_link(path):
            _fail(f"the prepared setup's {what} is a link; refusing")
        if _is_link_any_ancestor(path, voice_root):
            _fail(f"the prepared setup's {what} sits under a link; refusing")

    # The source archive is the trust root.
    zip_path = voice_root / "downloads" / "whisper-bin-x64.zip"
    try:
        zip_bytes = zip_path.read_bytes()
    except OSError:
        _fail("the pinned whisper.cpp archive (whisper-bin-x64.zip) is not in this "
              "workspace's downloads; run the bootstrap again")
    if hashlib.sha256(zip_bytes).hexdigest() != pins["zip_sha256"]:
        _fail("the whisper.cpp archive in this workspace does not match the published "
              "archive; delete it and run the bootstrap again")
    try:
        archive = zipfile.ZipFile(io.BytesIO(zip_bytes))
    except zipfile.BadZipFile:
        _fail("the pinned whisper.cpp archive is unreadable")
    expected = {}
    for info in archive.infolist():
        if info.is_dir():
            continue
        expected[info.filename.replace("\\", "/")] = (
            hashlib.sha256(archive.read(info)).hexdigest(),
            info.file_size,
        )
    if not expected:
        _fail("the pinned whisper.cpp archive is empty")

    # Every archived file in place, byte for byte.
    for name, (hash_hex, size) in expected.items():
        member = engine_dir / name
        try:
            payload = member.read_bytes()
        except OSError:
            _fail(f"the prepared engine is missing {name} from the verified archive; "
                  "run the bootstrap again")
        if len(payload) != size or hashlib.sha256(payload).hexdigest() != hash_hex:
            _fail(f"the prepared engine's {name} does not match the verified archive; "
                  "run the bootstrap again")

    # No extra executable the archive never shipped.
    for path in engine_dir.rglob("*"):
        if not path.is_file():
            continue
        if path.suffix.lower() not in (".exe", ".dll"):
            continue
        relative = path.relative_to(engine_dir).as_posix()
        if relative not in expected:
            _fail(f"the prepared engine contains {relative} which is not in the verified "
                  "archive; remove it or run the bootstrap again")

    # The model against the published digest; the manifest's numbers are
    # not trusted.
    try:
        model = model_file.read_bytes()
    except OSError:
        _fail("the prepared setup's model could not be read")
    if len(model) != pins["model_size"]:
        _fail(f"the prepared setup's model is {len(model)} bytes; the published "
              f"recommended model is {pins['model_size']}")
    if hashlib.sha256(model).hexdigest() != pins["model_sha256"]:
        _fail("the prepared setup's model does not match the published recommended "
              "model; download it again with the bootstrap")

    main_executable = manifest["engine"]["main_executable"]
    executable = engine_dir / main_executable
    if not _inside(executable, engine_dir) or not _inside(executable, voice_root):
        _fail("the prepared setup's main executable must resolve inside the engine "
              "directory in this workspace")
    if not executable.is_file():
        _fail(f"the prepared setup's executable is missing: {executable}")
    return {"executable": str(executable), "model": str(model_file)}


def _is_link_any_ancestor(path: Path, stop: Path) -> bool:
    """Whether any existing ancestor of ``path`` (up to ``stop``) is a link."""
    current = path
    while True:
        parent = current.parent
        if parent == current:
            return False
        if _is_link(parent):
            return True
        if _inside(parent, stop) is False and parent == stop:
            return False
        if parent == stop:
            return False
        current = parent


VOSK_DIGEST_ALGORITHM = "sha256-tree/1"


def vosk_model_digest(model_dir: Path) -> str:
    """``sha256-tree/1``, the contract shared with setup-vosk-local.py and
    crates/app/src/features/dictation/setup.rs: every file, sorted by POSIX
    relative path, hashing ``path NUL length NUL content``. Links are refused
    instead of followed, so a junction cannot stand in for the model."""
    import hashlib

    entries = list(model_dir.rglob("*"))
    for entry in entries:
        if _is_link(entry):
            _fail(f"the prepared Vosk model contains a link at {entry}; refusing")
    digest = hashlib.sha256()
    files = sorted((path for path in entries if path.is_file()),
                   key=lambda path: path.relative_to(model_dir).as_posix())
    for path in files:
        relative = path.relative_to(model_dir).as_posix().encode("utf-8")
        content = path.read_bytes()
        digest.update(relative)
        digest.update(bytes([0]))
        digest.update(str(len(content)).encode("ascii"))
        digest.update(bytes([0]))
        digest.update(content)
    return digest.hexdigest()


def _verify_prepared_vosk(repo_root: Path) -> dict[str, str]:
    """The Vosk prepared setup (schema gentle-shell-voice-vosk-setup/1):
    the venv Python and the model directory must live inside this
    workspace's .local/voice/vosk, be link-free and exist, and the model
    directory's recorded LOCAL digest must match its files."""
    import hashlib

    manifest_path = repo_root / ".local" / "voice" / "vosk" / "setup.json"
    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        _fail(f"no usable prepared Vosk setup in this workspace ({error})")
    if manifest.get("schema") != "gentle-shell-voice-vosk-setup/1":
        _fail("the prepared Vosk manifest is not ours (wrong schema)")
    vosk_root = repo_root / ".local" / "voice" / "vosk"
    # A manifest missing a path is a refusal, not a KeyError traceback.
    for key in ("python", "model_dir"):
        if not isinstance(manifest.get(key), str) or not manifest[key].strip():
            _fail(f"the prepared Vosk manifest has no {key} path; run the Vosk "
                  "setup again")
    python = Path(manifest["python"])
    model_dir = Path(manifest["model_dir"])
    for what, path in (("venv Python", python), ("model directory", model_dir)):
        if not _inside(path, vosk_root):
            _fail(f"the prepared Vosk {what} must stay inside {vosk_root}")
        if _is_link(path) or _is_link_any_ancestor(path, vosk_root):
            _fail(f"the prepared Vosk {what} sits on a link; refusing")
    if not python.is_file():
        _fail(f"the prepared Vosk Python is missing: {python}")
    if not model_dir.is_dir():
        _fail(f"the prepared Vosk model directory is missing: {model_dir}")
    # The installer records the digest under model.sha256_local with the
    # sha256-tree/1 recipe. A missing, flat or different-shaped digest means
    # "cannot verify", which is a refusal: integrity is never skipped.
    model_record = manifest.get("model")
    recorded = model_record.get("sha256_local") if isinstance(model_record, dict) else None
    if not isinstance(recorded, str) or len(recorded) != 64:
        _fail("the prepared Vosk manifest has no recorded LOCAL model digest "
              "(model.sha256_local); run the Vosk setup again")
    if model_record.get("digest_algorithm") != VOSK_DIGEST_ALGORITHM:
        _fail(f"the prepared Vosk manifest does not use {VOSK_DIGEST_ALGORITHM}; "
              "run the Vosk setup again")
    if vosk_model_digest(model_dir) != recorded:
        _fail("the prepared Vosk model does not match its recorded LOCAL digest; "
              "run the Vosk setup again")
    return {"executable": str(python), "model": str(model_dir)}


def _resolve_app(repo_root: Path, app_exe: str | None) -> Path:
    if app_exe:
        app = Path(os.path.abspath(app_exe))
        if not _inside(app, repo_root):
            _fail("the app build must stay inside this checkout; refusing")
    else:
        app = repo_root / DEFAULT_APP
    app = Path(os.path.abspath(app))
    if not app.is_file():
        _fail(
            f"the app build was not found at {app}. Build it first "
            f"(cargo build -p gentle-shell-gpui) or pass --exe PATH."
        )
    return app


def sandbox_env(repo_root: Path, sandbox_root: Path) -> dict[str, str]:
    """The environment overrides that isolate the child app: every managed
    variable points into the sandbox, never at the real user config."""
    return {
        "APPDATA": str(sandbox_root / "appdata"),
        # The app's own data root: on Windows dirs::data_dir() ignores
        # APPDATA (known-folder API), so the app needs this explicit
        # opt-in for its settings, instance registration, workspaces,
        # toolchain and update caches to land in the sandbox.
        "GENTLE_SHELL_GPUI_DATA_DIR": str(sandbox_root / "appdata"),
        "LOCALAPPDATA": str(sandbox_root / "localappdata"),
        "TEMP": str(sandbox_root / "tmp"),
        "TMP": str(sandbox_root / "tmp"),
        "GENTLE_PI_CONFIG_HOME": str(sandbox_root / "config"),
        "GENTLE_SHELL_CONFIG": str(sandbox_root / "config" / "launcher-config.json"),
        "GENTLE_SHELL_HOME": str(sandbox_root / "agent"),
        "GENTLE_SHELL_USER_PI_HOME": str(sandbox_root / "agent"),
        "PI_CODING_AGENT_DIR": str(sandbox_root / "agent"),
        "GENTLE_SHELL_ARGS": ISOLATED_SHELL_ARGS,
    }


def child_environment(overrides: dict[str, str]) -> dict[str, str]:
    """The child's full environment: the parent's, with the managed
    variables replaced. The parent's own environment is not touched."""
    env = dict(os.environ)
    env.update(overrides)
    return env


def prepare(
    repo_root: Path,
    sandbox_root: Path | None,
    app_exe: str | None,
    pins: dict | None = None,
    engine: str = "whisper",
) -> dict[str, str]:
    """Create a unique sandbox, the app's settings in it, and return the
    paths a launch needs. The workspace's voice artifacts are verified
    against the published pins first; nothing outside the sandbox is ever
    written, and an existing sandbox is never reused."""
    app = _resolve_app(repo_root, app_exe)
    if engine == "vosk":
        voice = _verify_prepared_vosk(repo_root)
    else:
        voice = _verify_prepared_voice(repo_root, pins or PUBLISHED_PINS)
    sandbox_tree = repo_root / ".local" / "voice" / "sandbox"
    if sandbox_root is None:
        stamp = time.strftime("%Y%m%d-%H%M%S")
        nonce = os.urandom(4).hex()
        sandbox_root = sandbox_tree / f"run-{stamp}-{nonce}"
    else:
        sandbox_root = Path(os.path.abspath(sandbox_root))
        if not _inside(sandbox_root, sandbox_tree):
            _fail(
                f"the sandbox must live inside {sandbox_tree}; refusing to "
                "write elsewhere"
            )
    if sandbox_root.exists():
        _fail(f"a sandbox already exists at {sandbox_root}; refusing to reuse it")
    # No link may sit between the sandbox tree and the target, and the
    # real user configuration is refused before the first mkdir.
    for parent in sandbox_root.parents:
        if not os.path.lexists(parent):
            break  # nothing above a missing directory can be a link
        if _is_link(parent):
            _fail("the sandbox root must not sit under a link")
        if parent == sandbox_tree:
            break
    real_appdata = os.environ.get("APPDATA")
    if real_appdata and Path(real_appdata).resolve() == Path(
        sandbox_root / "appdata"
    ).resolve():
        _fail("the sandbox would overwrite the real APPDATA; refusing")
    for name in ("appdata", "localappdata", "tmp", "agent", "config", "logs"):
        (sandbox_root / name).mkdir(parents=True, exist_ok=True)

    settings_dir = sandbox_root / "appdata" / "gentle-shell-gpui"
    settings_dir.mkdir(parents=True, exist_ok=True)
    settings_path = settings_dir / "settings.json"
    if settings_path.exists():
        _fail(f"a sandbox settings file already exists at {settings_path}")
    settings = {
        "onboarding": {"done": True, "version": ONBOARDING_VERSION},
        "dictation": {
            "mode": "Local",
            "local": {
                "engine": engine,
                "executable": voice["executable"],
                "model": voice["model"],
                "language": "auto" if engine == "whisper" else "es",
            },
            "provider": {
                "endpoint": "",
                "model": "",
                "api_key_env": "",
            },
        },
    }
    settings_path.write_text(
        json.dumps(settings, indent=2) + "\n", encoding="utf-8"
    )
    log_path = sandbox_root / "logs" / "app.log"
    log_path.touch()
    return {
        "sandbox": str(sandbox_root),
        "settings": str(settings_path),
        "log": str(log_path),
        "app": str(app),
        "env": sandbox_env(repo_root, sandbox_root),
    }


def launch(repo_root: Path, sandbox_root: Path | None, app_exe: str | None,
           engine: str = "whisper") -> int:
    """Spawn the app detached, with its output in the sandbox log, and
    report where everything lives. Never touches a running instance."""
    plan = prepare(repo_root, sandbox_root, app_exe, engine=engine)
    app = Path(plan["app"])
    log = open(plan["log"], "ab")
    if sys.platform.startswith("win"):
        flags = 0x00000008 | 0x00000200  # DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP
        process = subprocess.Popen(
            [str(app), str(repo_root)],
            cwd=str(repo_root),
            env=child_environment(plan["env"]),
            stdout=log,
            stderr=log,
            stdin=subprocess.DEVNULL,
            creationflags=flags,
            close_fds=True,
        )
    else:
        process = subprocess.Popen(
            [str(app), str(repo_root)],
            cwd=str(repo_root),
            env=child_environment(plan["env"]),
            stdout=log,
            stderr=log,
            stdin=subprocess.DEVNULL,
            start_new_session=True,
            close_fds=True,
        )
    print(f"sandbox:  {plan['sandbox']}")
    print(f"settings: {plan['settings']}")
    print(f"log:      {plan['log']}")
    print(f"app:      {plan['app']}")
    print(f"pid:      {process.pid}")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--prepare",
        action="store_true",
        help="create the sandbox and settings only; do not spawn the app",
    )
    parser.add_argument(
        "--engine",
        choices=["whisper", "vosk"],
        default="whisper",
        help="preconfigure the sandbox dictation with whisper (default) or vosk",
    )
    parser.add_argument(
        "--exe",
        help="path to the gentle-shell-gpui build (default target/debug)",
    )
    parser.add_argument(
        "--sandbox-root",
        help="explicit sandbox directory (default .local/voice/sandbox/run-<ts>)",
    )
    args = parser.parse_args(argv)
    if args.prepare:
        plan = prepare(_REPO_ROOT, Path(args.sandbox_root) if args.sandbox_root else None,
                       args.exe, engine=args.engine)
        print(f"sandbox:  {plan['sandbox']}")
        print(f"settings: {plan['settings']}")
        print(f"log:      {plan['log']}")
        print(f"app:      {plan['app']}")
        print("mode:     prepared (app not launched)")
        return 0
    return launch(_REPO_ROOT, Path(args.sandbox_root) if args.sandbox_root else None,
                  args.exe, engine=args.engine)


if __name__ == "__main__":
    sys.exit(main())
