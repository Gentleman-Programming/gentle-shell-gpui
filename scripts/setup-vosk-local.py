"""Prepare the official Vosk in this workspace, fully isolated.

Everything lives under ``.local/voice/vosk``: an isolated venv, the official
PyPI wheels (hash-verified), the official Spanish model downloaded once over
TLS from alphacephei.com, and an atomic manifest. Nothing is installed
globally: no global pip, no PATH or policy changes. The model page publishes
no checksums, so the FIRST download's SHA-256 is recorded as LOCAL integrity
(it catches later corruption; it is not an upstream attestation and the docs
say so).

Integrity contract:

* every installed artifact is recorded with a SHA-256 taken from PyPI's own
  release metadata for that exact filename and re-checked after download, so
  no artifact reaches the venv unverified - including the transitive
  dependencies of ``vosk`` (nothing is installed from an unpinned file);
* the model directory digest is ``sha256-tree/1``: for each file, sorted by
  its POSIX relative path, hash ``path\\0length\\0content``. The app
  (``crates/app/src/features/dictation/setup.rs``) and the sandbox launcher
  implement the same recipe, so a recorded digest is verified, never skipped.

Usage (run by the parent):
    python -B scripts/setup-vosk-local.py --check     # read-only readiness
    python -B scripts/setup-vosk-local.py             # prepare everything

``--check`` never creates a directory, never downloads and never installs.
Run with: python -B scripts/test_setup_vosk_local.py (offline fixtures only)
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
import tarfile
import time
import urllib.request
import zipfile
from collections import deque
from pathlib import Path, PureWindowsPath
from urllib.parse import urlsplit

sys.dont_write_bytecode = True

_HERE = Path(__file__).resolve().parent
_ROOT = _HERE.parent
_VOICE = _ROOT / ".local" / "voice"
_VOSK = _VOICE / "vosk"
_SCHEMA = "gentle-shell-voice-vosk-setup/1"
DIGEST_ALGORITHM = "sha256-tree/1"
PYPI = "https://pypi.org"
_ALLOWED_HOSTS = ("pypi.org", "files.pythonhosted.org", "alphacephei.com")
_MAX_DOWNLOAD_BYTES = 256 * 1024 * 1024
_ZIP_MAX_FILES = 2000
_ZIP_MAX_TOTAL_BYTES = 400 * 1024 * 1024
_ZIP_MAX_RATIO = 1500

#: Published pins (researched from the PyPI JSON API, 2026-11-14). The cffi
#: wheel is per CPython ABI; the installer picks the file for the venv's
#: interpreter and refuses an unknown ABI instead of guessing.
PINS = {
    "vosk-0.3.45-py3-none-win_amd64.whl":
        "6994ddc68556c7e5730c3b6f6bad13320e3519b13ce3ed2aa25a86724e7c10ac",
    "requests-2.34.2-py3-none-any.whl":
        "2a0d60c172f83ac6ab31e4554906c0f3b3588d37b5cb939b1c061f4907e278e0",
    "tqdm-4.70.1-py3-none-any.whl":
        "c293e525e6fef9c20e8728fd4612df02a0aa31bb5fe91ecd93e123b1b7bffa73",
    "srt-3.5.3.tar.gz":
        "4884315043a4f0740fd1f878ed6caa376ac06d70e135f306a6dc44632eed0cc0",
    "websockets-17.4-py3-none-any.whl":
        "c6bd2c9781915592b209160d0f017d4c7c82ad53075e6b0c24d2a89299e7f9e9",
    "setuptools-84.2.1-py3-none-any.whl":
        "ca140b45d5325a9478f880e351484f62fef15f336462e449ca0c03c873d589c2",
}
CFFI_WHEELS = {
    "cp310": ("cffi-2.1.1-cp310-cp310-win_amd64.whl",
              "a48d62ab9d6f4f98c983223a547af44be6ca3691074c31cecced6facd3ba2dc1"),
    "cp311": ("cffi-2.1.1-cp311-cp311-win_amd64.whl",
              "42f6930c31dc7f50732c9ae793c2786c7b6b044195967bbdde40bb9be81c4cc0"),
    "cp312": ("cffi-2.1.1-cp312-cp312-win_amd64.whl",
              "f53e442b08449d42821fa4a4fba000095af9f62742a500f978a9f557ec44339a"),
    "cp313": ("cffi-2.1.1-cp313-cp313-win_amd64.whl",
              "1aa5645c30469b09530c4ebca77ebf8f17618293c58f8549cb1a543a50236e7d"),
    "cp314": ("cffi-2.1.1-cp314-cp314-win_amd64.whl",
              "3222ba5d678f80a030e6afbcc33dc1ae5cb45facabb61cee2c7016b8432fde48"),
}
MODEL_NAME = "vosk-model-small-es-0.42"
MODEL_URL = f"https://alphacephei.com/vosk/models/{MODEL_NAME}.zip"
MODEL_LICENSE = "Apache-2.0"


class SetupError(RuntimeError):
    """A refusal: nothing was installed, and the message says why."""


def _fail(message: str):
    print(f"setup-vosk-local: {message}", file=sys.stderr)
    raise SystemExit(2)


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with open(path, "rb") as file:
        for block in iter(lambda: file.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def _is_link(path: Path) -> bool:
    """A symlink, or a Windows junction/reparse point: never followed."""
    try:
        info = os.lstat(path)
    except OSError:
        return True
    if os.path.islink(path):
        return True
    if sys.platform.startswith("win"):
        return bool(info.st_file_attributes & 0x0400)
    return False


def _default_drive_guard(path: Path) -> str | None:
    drive = PureWindowsPath(path).drive  # portable: works on POSIX too
    if drive and drive.rstrip(":").upper() == "C":
        return drive
    return None


def validate_root(repo_root: Path = _ROOT, vosk_root: Path = _VOSK,
                  drive_guard=_default_drive_guard) -> None:
    """Fail closed BEFORE any mutation: repo shape, non-C drive, no links on
    the way, and no junction/reparse descendant inside an existing tree."""
    if not (repo_root / "Cargo.toml").is_file() or not (
            repo_root / "odd" / "tasks").is_dir():
        _fail(f"{repo_root} does not look like the gentle-shell checkout "
              "(missing Cargo.toml / odd/tasks)")
    for label, path in (("repo root", repo_root), ("Vosk root", vosk_root)):
        forbidden = drive_guard(path.resolve())
        if forbidden:
            _fail(f"{label} resolves onto drive {forbidden}; this setup must "
                  "live on a drive other than C: so the system disk is not "
                  "saturated")
    voice_root = vosk_root.parent
    if voice_root.exists() and _is_link(voice_root):
        _fail(f"{voice_root} is a link; refusing to write through it")
    if vosk_root.exists():
        if _is_link(vosk_root) or not vosk_root.is_dir():
            _fail(f"{vosk_root} exists but is not a plain folder")
        refused = _link_descendants(vosk_root)
        if refused:
            _fail(f"refusing to operate: link or junction/reparse point at "
                  f"{refused}")


def _link_descendants(root: Path) -> Path | None:
    """The first link/junction in an existing tree, else None. Pure."""
    for dirpath, dirnames, filenames in os.walk(root):
        for name in dirnames + filenames:
            path = Path(dirpath) / name
            if _is_link(path):
                return path
    return None


def check_url_allowed(url: str, what: str = "request") -> None:
    parts = urlsplit(url)
    if parts.scheme != "https":
        raise SetupError(f"{what} refused: only https is allowed ({url!r})")
    if parts.username or parts.password:
        raise SetupError(f"{what} refused: credentials in a URL are not allowed")
    if parts.hostname not in _ALLOWED_HOSTS:
        raise SetupError(f"{what} refused: host {parts.hostname!r} is not on "
                         "the allowlist")


class _SafeRedirectHandler(urllib.request.HTTPRedirectHandler):
    """Checks the initial URL and EVERY redirect before it is followed."""

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        check_url_allowed(newurl, what="redirect")
        return super().redirect_request(req, fp, code, msg, headers, newurl)

    def https_request(self, req):
        check_url_allowed(req.full_url, what="request")
        return req

    http_request = https_request


_OPENER = urllib.request.build_opener(_SafeRedirectHandler())


def _guarded_open(url: str, timeout: int = 120):
    request = urllib.request.Request(url, headers={
        "User-Agent": "gentle-shell-vosk-setup/1", "Accept-Encoding": "identity"})
    try:
        return _OPENER.open(request, timeout=timeout)
    except SetupError:
        raise
    except Exception as error:  # messages can embed the full URL
        host = urlsplit(url).hostname
        raise SetupError(f"request to {host} failed: {type(error).__name__}") from None


def download(url: str, target: Path, max_bytes: int = _MAX_DOWNLOAD_BYTES) -> None:
    """TLS-only, host-allowlisted, size-bounded, and it never overwrites a
    file it did not write: the bytes land in ``<name>.part`` first."""
    check_url_allowed(url)
    if target.exists():
        raise SetupError(f"refusing to overwrite existing {target}")
    target.parent.mkdir(parents=True, exist_ok=True)
    part = target.with_name(target.name + ".part")
    if part.exists() and (part.is_symlink() or not part.is_file()):
        raise SetupError(f"refusing to reuse {part}: not a plain file")
    seen = 0
    with _guarded_open(url) as response, open(part, "wb") as file:
        declared = response.headers.get("Content-Length")
        if declared is not None and declared.isdigit() and int(declared) > max_bytes:
            raise SetupError(f"{target.name} is larger than {max_bytes} bytes")
        for block in iter(lambda: response.read(1024 * 1024), b""):
            seen += len(block)
            if seen > max_bytes:
                raise SetupError(f"{target.name} exceeded {max_bytes} bytes")
            file.write(block)
    os.replace(part, target)


def _fetch_json(url: str) -> dict:
    with _guarded_open(url, timeout=60) as response:
        return json.loads(response.read().decode("utf-8"))


def pick_pins(abi: str) -> list[tuple[str, str]]:
    """The pinned files for this venv's interpreter; cffi is per-ABI."""
    chosen = [(name, digest) for name, digest in PINS.items()
              if not name.startswith("cffi-")]
    cffi = CFFI_WHEELS.get(abi)
    if cffi is None:
        _fail(f"no pinned cffi wheel for this Python ({abi}); report this")
    chosen.append(cffi)
    return chosen


def safe_extract_zip(zip_path: Path, into: Path, expected_prefix: str) -> None:
    """Safe ZIP extraction: relative members only, no traversal, no
    absolute/drive paths, no links, bounded counts and sizes, and no
    overwrite of a file that is already there."""
    with zipfile.ZipFile(zip_path) as archive:
        members = archive.infolist()
        if len(members) > _ZIP_MAX_FILES:
            _fail(f"the archive has {len(members)} entries; refusing")
        total = 0
        for info in members:
            name = info.filename
            if name.startswith("/") or ".." in Path(name).parts or ":" in name:
                _fail(f"unsafe ZIP member {name!r}")
            if info.file_size < 0 or info.file_size > _ZIP_MAX_TOTAL_BYTES:
                _fail(f"unsafe ZIP member size for {name!r}")
            total += info.file_size
            if total > _ZIP_MAX_TOTAL_BYTES:
                _fail(f"the archive expands to more than {_ZIP_MAX_TOTAL_BYTES} bytes")
            if info.compress_size > 0 and info.file_size // info.compress_size > _ZIP_MAX_RATIO:
                _fail(f"suspicious compression ratio for {name!r}; refusing")
            # Unix mode 0o120000 is a symlink, and Windows mode bits can carry
            # a reparse marker: neither is ever created here.
            mode = (info.external_attr >> 16) & 0xFFFF
            if mode and (mode & 0o170000) == 0o120000:
                _fail(f"ZIP member {name!r} is a link; refusing")
        if not any(name.startswith(expected_prefix) for name in
                   (info.filename for info in members) if not name.endswith("/")):
            _fail(f"the archive does not contain {expected_prefix}")
        root = into.resolve()
        for info in members:
            name = info.filename
            target = (into / name).resolve()
            if not target.is_relative_to(root):
                _fail(f"ZIP member escapes the target: {name!r}")
            if name.endswith("/"):
                if target.exists() and (target.is_symlink() or not target.is_dir()):
                    _fail(f"refusing to reuse {target}")
                target.mkdir(parents=True, exist_ok=True)
                continue
            if target.exists():
                _fail(f"refusing to overwrite {target}")
            target.parent.mkdir(parents=True, exist_ok=True)
            with archive.open(name) as source, open(target, "wb") as sink:
                for block in iter(lambda: source.read(1024 * 1024), b""):
                    sink.write(block)


def tree_digest(root: Path) -> str:
    """``sha256-tree/1``: files sorted by POSIX relative path, hashing
    ``relpath NUL length NUL content``. Mirrored by the app and the launcher."""
    digest = hashlib.sha256()
    entries = list(root.rglob("*"))
    for entry in entries:
        if _is_link(entry):
            _fail(f"the model tree contains a link at {entry}; refusing")
    files = sorted((path for path in entries if path.is_file()),
                   key=lambda path: path.relative_to(root).as_posix())
    for path in files:
        relative = path.relative_to(root).as_posix().encode("utf-8")
        content = path.read_bytes()
        digest.update(relative)
        digest.update(b"\0")
        digest.update(str(len(content)).encode("ascii"))
        digest.update(b"\0")
        digest.update(content)
    return digest.hexdigest()


def _sha256_dir(model_dir: Path) -> str:
    """Backwards-compatible name for the documented ``tree_digest``."""
    return tree_digest(model_dir)


def _venv_python(venv: Path) -> Path:
    return (venv / "Scripts" / "python.exe" if sys.platform.startswith("win")
            else venv / "bin" / "python")


def _published_files(metadata: dict) -> list[dict]:
    """Every file record PyPI publishes for this project (all releases)."""
    files: list[dict] = list(metadata.get("urls", []))
    for entries in (metadata.get("releases") or {}).values():
        files.extend(entries or [])
    return files


def _verify_or_fetch(downloads: Path, filename: str, digest: str,
                     fetch=None) -> Path:
    """The artifact at ``downloads/filename``, verified against ``digest``."""
    fetch = _fetch_json if fetch is None else fetch
    target = downloads / filename
    if target.exists() and not target.is_file():
        raise SetupError(f"{target} is not a plain file; refusing")
    if target.is_file() and _sha256(target) == digest:
        return target
    package = filename.split("-")[0]
    metadata = fetch(f"{PYPI}/pypi/{package}/json")
    match = [entry for entry in _published_files(metadata)
             if entry.get("filename") == filename]
    if not match:
        raise SetupError(f"{filename} is not published by PyPI; refusing")
    published = match[0].get("digests", {}).get("sha256")
    if published != digest:
        raise SetupError(f"{filename}'s PyPI digest changed; refusing")
    # Our own cache entry may be corrupt: the fixed pin still decides what
    # lands there, and only a file we recognize is ever replaced.
    if target.is_file():
        target.unlink()
    download(match[0]["url"], target)
    if _sha256(target) != digest:
        raise SetupError(f"{filename} does not match its published pin")
    return target


def _read_member_text(archive: zipfile.ZipFile, suffix: str) -> str | None:
    for name in archive.namelist():
        if name.endswith(suffix):
            return archive.read(name).decode("utf-8", "replace")
    return None


def declared_requirements(artifact: Path) -> list[tuple[str, str]]:
    """The runtime dependencies an artifact declares, as (name, specifier).

    Extras are skipped: they are optional by definition. Any other marker is
    kept, and the resolver refuses what it cannot pin."""
    metadata = None
    if artifact.suffix == ".whl":
        with zipfile.ZipFile(artifact) as archive:
            metadata = _read_member_text(archive, ".dist-info/METADATA")
    elif artifact.name.endswith(".tar.gz"):
        with tarfile.open(artifact, "r:gz") as archive:
            for member in archive.getmembers():
                if member.name.endswith("PKG-INFO") and member.isfile():
                    extracted = archive.extractfile(member)
                    if extracted is not None:
                        metadata = extracted.read().decode("utf-8", "replace")
                        break
    if metadata is None:
        raise SetupError(f"{artifact.name} declares no readable metadata; refusing")
    requirements: list[tuple[str, str]] = []
    for line in metadata.splitlines():
        if not line.lower().startswith("requires-dist:"):
            continue
        value = line.split(":", 1)[1].strip()
        if "extra ==" in value.replace("'", '"'):
            continue
        name, _, rest = value.partition(";")
        name = name.strip()
        specifier = ""
        for index, character in enumerate(name):
            if character in " <>=!~([":
                name, specifier = name[:index], name[index:]
                break
        specifier = specifier.strip()
        for bracket in "([":
            specifier = specifier.replace(bracket, "")
        for bracket in ")]":
            specifier = specifier.replace(bracket, "")
        requirements.append((name.strip(), specifier.strip()))
    return requirements


def _version_key(version: str) -> tuple[int, ...]:
    """A PEP 440 subset ordered the way pip orders it: the release numbers
    first, then a suffix rank where a prerelease sits BELOW its own final
    release and a post release above it. Comparison is by tuple."""
    text = version.strip().lower()
    if text.startswith("v"):
        text = text[1:]
    match = re.fullmatch(r"(\d+(?:\.\d+)*)(.*)", text)
    if not match:
        raise SetupError(f"unparseable version {version!r}")
    release = tuple(int(part) for part in match.group(1).split("."))
    rest = match.group(2)
    if rest == "":
        return release + (2, 0)  # a final release
    post = re.fullmatch(r"\.?post(\d+)", rest)
    if post:
        return release + (3, int(post.group(1)))
    pre = re.fullmatch(r"\.?(a|b|c|rc|alpha|beta|pre|preview|dev)\.?(\d*)", rest)
    if pre:
        return release + (1, int(pre.group(2) or 0))
    raise SetupError(f"unparseable version {version!r}")


def is_final(version: str) -> bool:
    """Whether a release is a stable final one (no prerelease/dev marker)."""
    return _version_key(version)[-2] == 2


def mentions_prerelease(specifier: str) -> bool:
    """pip only considers prereleases when the requirement names one."""
    return any(character.isalpha() for character in specifier)


# A universal pure-Python wheel: the only kind this installer accepts, so it
# runs on every platform and never guesses an ABI.
_UNIVERSAL_WHEEL = re.compile(r"-(py3|py2\.py3)-none-any\.whl$")


def python_allows(requires_python: str | None,
                  version: tuple[int, ...] | None = None) -> bool:
    """PEP 440 ``requires_python`` compatibility for THIS interpreter. An
    unsupported clause is refused, never guessed."""
    if not requires_python:
        return True
    current = tuple(version or sys.version_info[:3])
    current_key = _version_key(".".join(str(part) for part in current))
    for clause in requires_python.split(","):
        clause = clause.strip()
        if not clause:
            continue
        for operator in ("==", "!=", ">=", "<=", ">", "<"):
            if not clause.startswith(operator):
                continue
            bound_text = clause[len(operator):].strip()
            if bound_text.endswith(".*"):
                if operator not in ("==", "!="):
                    raise SetupError(f"unsupported requires_python clause {clause!r}")
                prefix = tuple(int(part) for part in bound_text[:-2].split("."))
                matches = current[:len(prefix)] == prefix
                if (operator == "==" and not matches) or (operator == "!=" and matches):
                    return False
                break
            bound = _version_key(bound_text)
            satisfied = {
                "==": current_key == bound,
                "!=": current_key != bound,
                ">=": current_key >= bound,
                "<=": current_key <= bound,
                ">": current_key > bound,
                "<": current_key < bound,
            }[operator]
            if not satisfied:
                return False
            break
        else:
            raise SetupError(f"unsupported requires_python clause {clause!r}")
    return True


def satisfies(version: str, specifier: str) -> bool:
    """A deliberately small PEP 440 subset: ==, !=, >=, <=, >, <, each comma
    separated. Anything else is refused instead of guessed."""
    if not specifier:
        return True
    try:
        candidate = _version_key(version)
    except SetupError:
        return False
    for clause in specifier.split(","):
        clause = clause.strip()
        for operator in ("==", "!=", ">=", "<=", ">", "<"):
            if clause.startswith(operator):
                bound = _version_key(clause[len(operator):].strip())
                ok = {
                    "==": candidate == bound,
                    "!=": candidate != bound,
                    ">=": candidate >= bound,
                    "<=": candidate <= bound,
                    ">": candidate > bound,
                    "<": candidate < bound,
                }[operator]
                if not ok:
                    return False
                break
        else:
            raise SetupError(f"unsupported version specifier {clause!r}")
    return True


def resolve_dependency(name: str, specifier: str = "",
                       fetch=None) -> tuple[str, str, str]:
    """(filename, sha256, version) for a dependency: the newest STABLE release
    that satisfies the specifier and publishes a universal pure-Python wheel
    this interpreter can install. Prereleases are considered only when the
    requirement itself names one, exactly as pip does, and when nothing usable
    exists the refusal says so instead of silently omitting the dependency.
    The digest comes from PyPI's own file record."""
    fetch = _fetch_json if fetch is None else fetch
    metadata = fetch(f"{PYPI}/pypi/{name}/json")
    releases = metadata.get("releases", {})
    candidates = [metadata.get("info", {}).get("version", "")]
    candidates += [version for version in releases if version not in candidates]
    candidates = [version for version in candidates
                  if version and satisfies(version, specifier)]
    if not mentions_prerelease(specifier):
        candidates = [version for version in candidates if is_final(version)]
    if not candidates:
        raise SetupError(f"no stable {name} release satisfies {specifier!r}: "
                         "prereleases are not used unless the requirement asks "
                         "for one")
    version = max(candidates, key=_version_key)
    files = releases.get(version) or metadata.get("urls", [])
    universal = [entry for entry in files
                 if _UNIVERSAL_WHEEL.search(entry.get("filename", ""))]
    if not universal:
        raise SetupError(f"{name} {version} publishes no pure-Python wheel; "
                         "refusing to guess an ABI")
    usable = [entry for entry in universal
              if python_allows(entry.get("requires_python"))]
    if not usable:
        raise SetupError(f"{name} {version} requires Python "
                         f"{universal[0].get('requires_python')!r}, which this "
                         f"interpreter ({sys.version_info.major}."
                         f"{sys.version_info.minor}) does not satisfy")
    wheel = sorted(usable, key=lambda entry: entry["filename"])[0]
    digest = wheel.get("digests", {}).get("sha256")
    if not digest:
        raise SetupError(f"{name} {version} publishes no digest; refusing")
    return wheel["filename"], digest, version


def resolve_artifacts(downloads: Path, direct: list[Path],
                      fetch=None) -> tuple[list[Path], list[dict]]:
    """Every artifact to install, with the transitive closure resolved and
    pinned. Nothing is installed that has no published digest."""
    fetch = _fetch_json if fetch is None else fetch
    known = {_distribution_name(path) for path in direct}
    pending = deque()
    for path in direct:
        pending.extend(declared_requirements(path))
    resolved: list[Path] = []
    records: list[dict] = []
    while pending:
        name, specifier = pending.popleft()
        key = name.lower()
        if key in known:
            continue
        filename, digest, version = resolve_dependency(name, specifier, fetch=fetch)
        path = _verify_or_fetch(downloads, filename, digest, fetch=fetch)
        known.add(key)
        resolved.append(path)
        records.append({"file": filename, "sha256": digest, "version": version})
        pending.extend(declared_requirements(path))
    return resolved, records


def _distribution_name(artifact: Path) -> str:
    return artifact.name.split("-")[0].lower()


def check_ready(vosk_root: Path) -> dict:
    """Read-only: creates, downloads and installs nothing."""
    manifest_path = vosk_root / "setup.json"
    venv_python = _venv_python(vosk_root / "venv")
    reasons: list[str] = []
    manifest = None
    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        reasons.append("no setup.json manifest")
    if not venv_python.is_file():
        reasons.append("the isolated venv Python is missing")
    model_digest = None
    if manifest is not None:
        if manifest.get("schema") != _SCHEMA:
            reasons.append("the manifest is not ours (wrong schema)")
        model = manifest.get("model") or {}
        if model.get("digest_algorithm") != DIGEST_ALGORITHM:
            reasons.append(f"the manifest does not record {DIGEST_ALGORITHM}")
        model_dir = Path(manifest.get("model_dir", ""))
        if not model_dir.is_dir():
            reasons.append("the prepared model directory is missing")
        else:
            model_digest = tree_digest(model_dir)
            if model_digest != model.get("sha256_local"):
                reasons.append("the model does not match its recorded digest")
    return {"ok": not reasons, "engine": "vosk", "schema": _SCHEMA,
            "manifest": str(manifest_path), "model_sha256": model_digest,
            "reasons": reasons}


def prepare(check: bool = False, vosk_root: Path | None = None) -> None:
    vosk_root = _VOSK if vosk_root is None else vosk_root
    validate_root(repo_root=_ROOT, vosk_root=vosk_root)
    downloads = vosk_root / "downloads"
    cache = vosk_root / "pip-cache"
    tmp = vosk_root / "tmp"
    venv = vosk_root / "venv"
    models = vosk_root / "models"

    if check:
        report = check_ready(vosk_root)
        print(json.dumps(report))
        if not report["ok"]:
            raise SystemExit(1)
        return

    abi = f"cp{sys.version_info.major}{sys.version_info.minor}"
    pins = pick_pins(abi)
    for folder in (downloads, cache, tmp, models):
        folder.mkdir(parents=True, exist_ok=True)
    refused = _link_descendants(vosk_root)
    if refused:
        _fail(f"refusing to operate: link or junction/reparse point at {refused}")

    wheels = [_verify_or_fetch(downloads, name, digest) for name, digest in pins]
    transitive, records = resolve_artifacts(downloads, wheels)
    artifacts = wheels + transitive

    # Isolated venv; pip installs only the artifacts verified above.
    if not venv.exists():
        subprocess.run([str(Path(sys.executable)), "-B", "-m", "venv", str(venv)],
                       check=True)
    venv_python = _venv_python(venv)
    if not venv_python.is_file():
        _fail(f"the isolated venv Python is missing at {venv_python}")
    env = dict(os.environ)
    env.update({
        "TEMP": str(tmp), "TMP": str(tmp),
        "PIP_CACHE_DIR": str(cache),
        "PIP_NO_INDEX": "1",
        "PIP_DISABLE_PIP_VERSION_CHECK": "1",
        "PIP_CONFIG_FILE": os.devnull,
    })
    ordered = ([artifact for artifact in artifacts
                if artifact.name.startswith("setuptools-")]
               + [artifact for artifact in artifacts
                  if not artifact.name.startswith("setuptools-")])
    for artifact in ordered:
        command = [str(venv_python), "-m", "pip", "install", "--no-index",
                   "--no-deps", "--disable-pip-version-check"]
        if artifact.name.endswith(".tar.gz"):
            command.append("--no-build-isolation")
        command.append(str(artifact))
        try:
            subprocess.run(command, check=True, env=env)
        except subprocess.CalledProcessError:
            _fail(f"pip could not install {artifact.name} from the verified copy")
    try:
        subprocess.run([str(venv_python), "-c", "import vosk"], check=True, env=env)
    except subprocess.CalledProcessError:
        _fail("vosk is not importable in the isolated venv after install")

    # The official model: TLS only, downloaded once, hashed LOCALLY.
    model_zip = downloads / f"{MODEL_NAME}.zip"
    model_dir = models / MODEL_NAME
    if model_dir.exists():
        if not model_dir.is_dir() or _is_link(model_dir):
            _fail(f"{model_dir} exists but is not a plain folder; not overwriting")
        if not (vosk_root / "setup.json").is_file():
            _fail(f"{model_dir} already exists without a manifest; not overwriting")
    else:
        if not model_zip.is_file():
            download(MODEL_URL, model_zip)
        # Extraction lands at models/<MODEL_NAME>/... in one step: no
        # directory rename (and therefore no nested model directory).
        safe_extract_zip(model_zip, models, MODEL_NAME)

    model_digest = tree_digest(model_dir)
    manifest = {
        "schema": _SCHEMA,
        "created_at": time.strftime("%Y-%m-%dT%H:%M:%S"),
        "python": str(venv_python.resolve()),
        "model_dir": str(model_dir.resolve()),
        "model": {"name": MODEL_NAME, "url": MODEL_URL, "license": MODEL_LICENSE,
                  "sha256_local": model_digest,
                  "digest_algorithm": DIGEST_ALGORITHM,
                  "trust": "official TLS source; LOCAL first-download hash "
                           "(no upstream checksum published)"},
        "python_wheels": {
            "vosk": "0.3.45",
            "pins": "see setup-vosk-local.py PINS",
            "transitive": records,
            "resolution": "PyPI JSON digest per exact filename, re-verified "
                          "after download; nothing unpinned is installed",
        },
    }
    staging_manifest = vosk_root / "setup.json.staging"
    staging_manifest.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    os.replace(staging_manifest, vosk_root / "setup.json")
    print(f"prepared: {vosk_root / 'setup.json'}")


def main() -> int:
    parser = argparse.ArgumentParser(description="Prepare the official Vosk locally.")
    parser.add_argument("--check", action="store_true",
                        help="verify readiness; never installs or downloads")
    args = parser.parse_args()
    try:
        prepare(check=args.check)
    except SetupError as refusal:
        _fail(str(refusal))
    return 0


if __name__ == "__main__":
    sys.exit(main())