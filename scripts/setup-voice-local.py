#!/usr/bin/env python3
"""Isolated, portable local voice setup for Gentle Shell (Windows x64, CPU).

Prepares, entirely under <repo>/.local/voice (ignored by git, on a non-C drive):

  downloads/whisper-bin-x64.zip   official ggml-org/whisper.cpp release asset,
                                  exact name `whisper-bin-x64.zip` (CPU x64),
                                  SHA-256 verified against GitHub's own asset
                                  digest metadata (fail-closed).
  engine/<tag>-<unique>/         safely-extracted engine (whisper-cli.exe +
                                  adjacent DLLs), written directly in place
                                  under an ownership/integrity marker
                                  (setup-in-progress.json carrying the verified
                                  ZIP hash). The directory is NEVER renamed;
                                  a failed attempt keeps its directory marked
                                  "incomplete", preserved, never deleted.
  models/ggml-base.bin            official multilingual ggml model pinned at an
                                  immutable Hugging Face revision, SHA-256
                                  verified against the LFS blob pointer.
  licenses/                       published upstream license texts + URLs.
  setup.json                      machine-readable manifest of everything above,
                                  published atomically LAST (the commit point)
                                  and only after every extracted executable/DLL
                                  hash matches its verified ZIP member and the
                                  model hash verifies.
  README.local.md                 setup guide with the exact GUI settings paths.

Rules enforced by design:

  * Nothing outside <repo>/.local/voice is written; the root must resolve on a
    non-C drive and inside the checkout, validated before any mutation.
  * No package manager, no global install, no PATH change, no real app config.
  * Idempotent: artifacts are reused only when size and SHA-256 match the
    freshly fetched upstream metadata; unverified ("unknown ownership") files
    are never overwritten or deleted.
  * The engine directory is never renamed and never reused unverified: each
    extraction targets a NEW nonce-suffixed directory; a prior destination is
    reused only via the published manifest when its SHA-256 still matches
    upstream and every binary verifies in place. Unknown files/links and
    incomplete owned directories are preserved, never deleted.
  * Downloads are streamed into sibling *.part files and promoted atomically
    only after full hash + size verification; only files matching this script's
    own *.part naming are ever cleaned up.
  * ZIP extraction rejects path traversal, absolute/drive paths, symlink and
    non-regular members, duplicates and declared-total-size bombs, before any
    byte is extracted.
  * The script never loads the model, runs inference or touches the microphone;
    run `--check` to verify an existing install without downloading anything.

Usage (parent-invoked):   python -B scripts/setup-voice-local.py [--check] [--pin-tag TAG]

Requires only the Python standard library.
"""

from __future__ import annotations

import argparse
import hashlib
import io
import json
import os
import re
import secrets
import shutil
import stat
import sys
import time
import urllib.parse
import urllib.request
import zipfile
from pathlib import Path, PureWindowsPath

# ---------------------------------------------------------------------------
# Fixed targets (verified read-only against upstream metadata; see
# odd/tasks/voice-dictation.md). The digest of a release asset is re-fetched
# and re-verified on every run, so a moving target can never pass silently.
# ---------------------------------------------------------------------------

GH_REPO = "ggml-org/whisper.cpp"
GH_RELEASES_API = f"https://api.github.com/repos/{GH_REPO}/releases"
GH_LICENSE_RAW = "https://raw.githubusercontent.com/{repo}/{tag}/LICENSE"
ENGINE_ASSET = "whisper-bin-x64.zip"

HF_MODEL_REPO = "ggerganov/whisper.cpp"
HF_MODEL_API = f"https://huggingface.co/api/models/{HF_MODEL_REPO}"
HF_TREE_API = f"https://huggingface.co/api/models/{HF_MODEL_REPO}/tree"
HF_RESOLVE = f"https://huggingface.co/{HF_MODEL_REPO}/resolve"
MODEL_FILE = "ggml-base.bin"  # multilingual; NOT ggml-base.en.bin

ALLOWED_HOSTS = {
    "api.github.com",
    "github.com",
    "raw.githubusercontent.com",
    "release-assets.githubusercontent.com",  # observed official release redirect
    "huggingface.co",
    "cdn-lfs.huggingface.co",                 # observed HF LFS CDN
    "cdn-lfs-us-1.hf.co",                     # observed HF LFS CDN (us-1)
    "cas-bridge.xethub.hf.co",                # observed HF xet CDN
    "us.aws.cdn.hf.co",                       # observed HF xet-bridge CDN (us-aws)
}
# No wildcards: any host not listed (observed) fails closed before a request.

# ZIP extraction safety caps (the asset is ~8.9 MB compressed; this is generous).
ZIP_MAX_FILES = 2000
ZIP_MAX_TOTAL_BYTES = 400 * 1024 * 1024
ZIP_MAX_RATIO = 1500  # per-entry uncompressed/compressed ratio cap

# Ownership/integrity marker written into every extraction directory before
# the first byte is extracted; kept, and updated to "complete" after all
# executable/DLL members verify against the verified ZIP members.
SETUP_MARKER_NAME = "setup-in-progress.json"

_USER_AGENT = "gentle-shell-voice-local-setup/1"


class SetupError(Exception):
    """Any fail-closed condition. The message is user-facing."""


def sha256_hex(fileobj) -> str:
    h = hashlib.sha256()
    for chunk in iter(lambda: fileobj.read(1024 * 1024), b""):
        h.update(chunk)
    return h.hexdigest()


def sha256_file(path: Path, expected_size: int | None = None) -> str:
    with open(path, "rb") as fh:
        if expected_size is not None:
            actual = os.fstat(fh.fileno()).st_size
            if actual != expected_size:
                raise SetupError(
                    f"{path.name}: size is {actual} bytes, expected "
                    f"{expected_size}; not reusing this file (unknown ownership)."
                )
        return sha256_hex(fh)


def rmtree_strict(path: Path) -> None:
    """Best-effort recursive delete for KNOWN test/companionship dirs only."""
    shutil.rmtree(path, ignore_errors=True)


# ---------------------------------------------------------------------------
# Root validation (before any mutation)
# ---------------------------------------------------------------------------

def _default_drive_guard(path):
    """Map a resolved path to the saturating drive label, else None."""
    drive = PureWindowsPath(path).drive  # portable: works on POSIX too
    if drive and drive.rstrip(":").upper() == "C":
        return drive
    return None


def validate_root(repo_root: Path, keeper: Path,
                  drive_guard=_default_drive_guard) -> None:
    """Fail closed unless keeper stays inside repo_root on a non-C drive.

    Pure checks only: nothing is created, moved or deleted by this function.
    """
    if not repo_root.is_dir():
        raise SetupError(f"repo root {repo_root} does not exist.")
    if not (repo_root / "Cargo.toml").is_file() or not (
        repo_root / "odd" / "tasks"
    ).is_dir():
        raise SetupError(
            f"{repo_root} does not look like the gentle-shell checkout "
            "(missing Cargo.toml / odd/tasks)."
        )
    resolved_root = repo_root.resolve()
    resolved_keeper = (resolved_root / keeper).resolve()
    for label, p in (("repo root", resolved_root), ("data root", resolved_keeper)):
        forbidden = drive_guard(p)
        if forbidden:
            raise SetupError(
                f'{label} resolves onto drive {forbidden}; this setup must live '
                "on a drive other than C: so the system disk is not saturated."
            )
    try:
        resolved_keeper.relative_to(resolved_root)
    except ValueError:
        raise SetupError(
            f"data root {resolved_keeper} escapes the checkout "
            f"{resolved_root}; refusing to write outside the repository."
        ) from None


def layout(repo_root: Path | None = None) -> dict:
    """Canonical paths under .local/voice. Pure: creates nothing."""
    root = (Path(__file__).resolve().parent.parent if repo_root is None
            else Path(repo_root)) / ".local" / "voice"
    return {
        "root": root,
        "downloads": root / "downloads",
        "tmp": root / "tmp",
        "logs": root / "logs",
        "licenses": root / "licenses",
        "engine": root / "engine",
        "engine_zip": root / "downloads" / ENGINE_ASSET,
        "model_bin": root / "models" / MODEL_FILE,
    }


# ---------------------------------------------------------------------------
# Link/tamper guard: junctions and reparse points must never be followed
# ---------------------------------------------------------------------------

def _reparse_reason(path: Path) -> str | None:
    """Return a reason string if path is a symlink/junction/reparse point."""
    try:
        st = os.lstat(path)
    except OSError:
        return None
    if stat.S_ISLNK(st.st_mode):
        return f"symbolic link at {path}"
    attrs = getattr(st, "st_file_attributes", 0)
    if getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0) and (
            attrs & stat.FILE_ATTRIBUTE_REPARSE_POINT):
        return f"junction/reparse point at {path}"
    return None


def assert_no_repo_links(root: Path) -> None:
    """Refuse ANY symlink/junction/reparse descendant of root BEFORE mutation.

    Windows resolves linked children outside the root: resolve() alone does not
    stop those escapes. Unknown links are never deleted; the user decides.
    """
    if not root.exists():
        return
    pro = _reparse_reason(root)
    if pro:
        raise SetupError(
            f"refusing to operate: {pro}. Unknown links are never deleted; "
            "remove them manually if unwanted.")
    for dirpath, dirnames, filenames in os.walk(root, followlinks=False):
        for name in (*dirnames, *filenames):
            pro = _reparse_reason(Path(dirpath) / name)
            if pro:
                raise SetupError(
                    f"refusing to operate: junction/reparse point or symlink "
                    f"at {Path(dirpath) / name}. Unknown links are never "
                    "deleted; remove them manually if unwanted.")


# ---------------------------------------------------------------------------
# Network boundary (all remote goes through these two, with a host allowlist)
# ---------------------------------------------------------------------------

def sanitize_url(url: str) -> str:
    """Drop query/fragment/userinfo: signed CDN URLs never reach logs/stdout."""
    parts = urllib.parse.urlsplit(url)
    return f"{parts.scheme}://{(parts.hostname or '').lower()}{parts.path or ''}"


def sanitize_error(reason: str, url: str) -> str:
    return f"{reason} {sanitize_url(url)}; request denied."


def check_url_allowed(url: str, *, what: str = "request") -> None:
    """HTTPS-only, no userinfo, explicit host allowlist — checked BEFORE use."""
    parts = urllib.parse.urlsplit(url)
    if parts.scheme.lower() != "https":
        raise SetupError(sanitize_error(f"{what}: only HTTPS is allowed for", url))
    if parts.username is not None or parts.password is not None:
        raise SetupError(
            sanitize_error(f"{what}: URLs with userinfo are not allowed for", url))
    host = (parts.hostname or "").lower()
    if host not in ALLOWED_HOSTS:
        raise SetupError(sanitize_error(f"{what}: non-allowlisted host for", url))


class SafeRedirectHandler(urllib.request.HTTPRedirectHandler):
    """Validates the initial request and EVERY redirect before following it.

    `http(s)_request` are pre-open processors, so the initial URL is checked
    before any handler opens a connection; `redirect_request` is called by the
    inherited http_error_30x machinery before a redirect is followed.
    """

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        check_url_allowed(newurl, what="redirect")
        return super().redirect_request(req, fp, code, msg, headers, newurl)

    def https_request(self, req):
        check_url_allowed(req.full_url, what="request")
        return req

    http_request = https_request


_OPENER = urllib.request.build_opener(SafeRedirectHandler())


def _guarded_open(url: str, timeout: int):
    open_req = urllib.request.Request(
        url, headers={"User-Agent": _USER_AGENT, "Accept-Encoding": "identity"}
    )
    try:
        return _OPENER.open(open_req, timeout=timeout)  # host checked pre-open + pre-redirect
    except SetupError:
        raise
    except Exception as exc:  # urllib messages embed full URLs incl. any query
        raise SetupError(
            f"fetch of {sanitize_url(url)} failed: {exc.__class__.__name__}."
        ) from exc


def net_fetch_json(url: str):
    try:
        with _guarded_open(url, 120) as resp:
            return json.loads(resp.read().decode("utf-8"))
    except SetupError:
        raise
    except Exception as exc:
        raise SetupError(
            f"fetch of {sanitize_url(url)} failed: {exc.__class__.__name__}."
        ) from exc


def net_stream_opener(url: str):
    """Return a context-manager binary stream for a pinned, allowlisted URL."""
    return _guarded_open(url, 300)


def _clean_digest(digest: str | None, what: str, *, bare_hex_ok: bool = True) -> str:
    """Normalize a SHA-256 (`sha256:<hex>` prefixed or bare hex). ABSENT is fatal."""
    if not digest:
        raise SetupError(
            f"{what}: upstream metadata carries no SHA-256 digest; refusing to "
            "download an unverifiable artifact. Re-run later or pin another tag."
        )
    raw = digest.strip()
    if raw.lower().startswith("sha256:"):
        raw = raw[7:]
    elif not bare_hex_ok:
        raise SetupError(f"{what}: unexpected digest format {digest!r}.")
    if not re.fullmatch(r"[0-9a-fA-F]{64}", raw):
        raise SetupError(f"{what}: unexpected digest format {digest!r}.")
    return raw.lower()


# ---------------------------------------------------------------------------
# Release / model resolution (metadata only; deterministic and test-injectable)
# ---------------------------------------------------------------------------

def resolve_engine_release(fetch_json, pin_tag: str | None = None) -> dict:
    """Pick the newest non-draft release that ships `whisper-bin-x64.zip`.

    The GitHub release list is ordered newest-first. Tagged releases without
    Windows binaries (e.g. tag-only builds) are skipped; the CI-tagged release
    that actually carries the asset is the authoritative target. Any CUDA or
    other variant is excluded by exact-name matching.
    """
    newest = None
    for page in (1, 2, 3, 4):
        releases = fetch_json(f"{GH_RELEASES_API}?per_page=100&page={page}")
        if not releases:
            break
        for rel in releases:
            if rel.get("draft"):
                continue
            if pin_tag and rel.get("tag_name") != pin_tag:
                continue
            for asset in rel.get("assets", []):
                if asset.get("name") != ENGINE_ASSET:
                    continue
                digest = _clean_digest(
                    asset.get("digest"),
                    f"{GH_REPO} release {rel.get('tag_name')} asset "
                    f"{ENGINE_ASSET}",
                )
                newest = {
                    "repo": GH_REPO,
                    "tag": rel["tag_name"],
                    "release_id": rel["id"],
                    "asset_id": asset["id"],
                    "asset": ENGINE_ASSET,
                    "created_at": rel.get("created_at", ""),
                    "prerelease": bool(rel.get("prerelease")),
                    "size": int(asset["size"]),
                    "sha256": digest,
                    "digest_source": "github-release-asset-digest",
                    "url": asset["browser_download_url"],
                    "api_url": f"{GH_RELEASES_API}/tags/{rel['tag_name']}",
                }
                break  # exact-name match only; no variant guessing
            if newest:
                break
        if newest or pin_tag:
            break
    if newest is None:
        hint = f" pinned at tag {pin_tag}" if pin_tag else ""
        raise SetupError(
            f"no whisper.cpp release with asset {ENGINE_ASSET}{hint}; aborting "
            "without downloading anything."
        )
    return newest


def resolve_model(fetch_json, pin_revision: str | None = None) -> dict:
    """Pin the multilingual ggml-base.bin at an immutable HF revision."""
    meta = fetch_json(HF_MODEL_API)
    revision = pin_revision or (meta.get("sha") or "")
    if not re.fullmatch(r"[0-9a-f]{40}", revision):
        raise SetupError(
            f"{HF_MODEL_REPO}: API returned no usable commit sha "
            f"({revision!r}); refusing to fetch from a moving revision."
        )
    tree = fetch_json(f"{HF_TREE_API}/{revision}?recursive=true")
    entry = next((e for e in tree if e.get("path") == MODEL_FILE and
                  e.get("type") == "file"), None)
    if entry is None:
        raise SetupError(
            f"{HF_MODEL_REPO}@{revision} has no {MODEL_FILE}; aborting."
        )
    lfs = entry.get("lfs") or {}
    sha256 = _clean_digest(
        lfs.get("oid"), f"{HF_MODEL_REPO}@{revision}:{MODEL_FILE} LFS blob"
    )
    size = int(lfs.get("size") or entry.get("size") or 0)
    if size <= 0:
        raise SetupError(f"{MODEL_FILE}: upstream size metadata missing.")
    return {
        "repo": HF_MODEL_REPO,
        "revision": revision,
        "path": MODEL_FILE,
        "size": size,
        "sha256": sha256,
        "digest_source": "huggingface-lfs-oid",
        "url": f"{HF_RESOLVE}/{revision}/{MODEL_FILE}?download=true",
        "api_url": f"{HF_MODEL_API}/tree/{revision}",
    }


def make_resolver(fetch_json):
    def resolve(pin_tag=None):
        return (resolve_engine_release(fetch_json, pin_tag),
                resolve_model(fetch_json))
    return resolve


# ---------------------------------------------------------------------------
# Streaming download with atomic promotion
# ---------------------------------------------------------------------------

def _part_name(final_name: str) -> str:
    return f"{final_name}.{secrets.token_hex(6)}.part"


def clean_own_part_files(directory: Path) -> None:
    """Remove only this script's incomplete <name>.<12hex>.part files."""
    if not directory.is_dir():
        return
    for entry in directory.iterdir():
        if entry.is_file() and re.fullmatch(
                r".+\.[0-9a-f]{12}\.part", entry.name):
            entry.unlink(missing_ok=True)


def clean_own_staging_dirs(tmp_dir: Path) -> None:
    if not tmp_dir.is_dir():
        return
    for entry in tmp_dir.iterdir():
        if entry.is_dir() and re.fullmatch(r"[0-9a-f]{12}\.stage", entry.name):
            rmtree_strict(entry)


def download_to(url: str, final_path: Path, expected_size: int,
                expected_sha: str, stream_opener, directory: Path,
                log=print) -> Path:
    """Stream `url` into <name>.<hex>.part, verify, then atomically promote.

    Enforces the expected size exactly (early lengths abort; any over-read
    aborts; truncation aborts) and the expected SHA-256. On any failure the
    own .part file is removed and nothing is promoted.
    """
    directory.mkdir(parents=True, exist_ok=True)
    part = directory / _part_name(final_path.name)
    hasher = hashlib.sha256()
    total = 0
    try:
        with stream_opener(url) as stream, open(part, "wb") as out:
            headers = getattr(stream, "headers", None)
            if headers is not None:
                declared = headers.get("Content-Length")
                if declared is not None and int(declared) != expected_size:
                    raise SetupError(
                        f"{final_path.name}: Content-Length {declared} does not "
                        f"match expected {expected_size}; aborting download."
                    )
            while True:
                # one extra byte is tolerated to detect oversized streams
                chunk = stream.read(min(1024 * 1024, expected_size - total + 1))
                if not chunk:
                    break
                total += len(chunk)
                if total > expected_size:
                    raise SetupError(
                        f"{final_path.name}: downloaded {total} bytes, more than "
                        f"the verified upstream size {expected_size}; aborting."
                    )
                hasher.update(chunk)
                out.write(chunk)
        if total != expected_size:
            raise SetupError(
                f"{final_path.name}: got {total} bytes, expected exactly "
                f"{expected_size}; aborting."
            )
        actual = hasher.hexdigest()
        if actual != expected_sha:
            raise SetupError(
                f"{final_path.name}: SHA-256 mismatch (got {actual}, expected "
                f"{expected_sha}); discarding the download."
            )
        os.replace(part, final_path)  # atomic promotion of the verified file
        log(f"verified {final_path.name}: {expected_size} bytes, "
            f"sha256 {expected_sha}")
        return final_path
    except BaseException:
        part.unlink(missing_ok=True)
        raise


def verify_artifact(path: Path, expected_size: int, expected_sha: str) -> bool:
    if not path.is_file():
        return False
    try:
        return (path.stat().st_size == expected_size and
                sha256_file(path, expected_size) == expected_sha)
    except (OSError, SetupError):
        return False


# ---------------------------------------------------------------------------
# Safe ZIP extraction
# ---------------------------------------------------------------------------

def _check_zip_entry(info: zipfile.ZipInfo, seen: set, running_total: list) -> None:
    raw = info.filename
    name = raw.replace("\\", "/")
    if not name or name.endswith("/"):
        if not name or name.startswith("/") or ".." in name.split("/"):
            raise SetupError(f"unsafe directory entry {raw!r} in archive.")
    pw = PureWindowsPath(name)
    if pw.is_absolute():
        raise SetupError(f"absolute path {raw!r} rejected from archive.")
    if pw.drive:
        raise SetupError(f"drive-qualified path {raw!r} rejected from archive.")
    parts = name.split("/")
    if ".." in parts or "" in parts[:-1] or parts[0] in ("", "."):
        if name != "":
            raise SetupError(
                f"path traversal {raw!r} rejected from archive."
            )
    if name not in seen:
        seen.add(name)
    else:
        raise SetupError(f"duplicate archive entry {raw!r} rejected.")
    if len(seen) > ZIP_MAX_FILES:
        raise SetupError("archive declares too many entries (possible bomb).")
    if info.file_size < 0 or info.file_size > ZIP_MAX_TOTAL_BYTES:
        raise SetupError(f"entry {raw!r} declares an unsafe size.")
    running_total[0] += max(info.file_size, 0)
    if running_total[0] > ZIP_MAX_TOTAL_BYTES:
        raise SetupError(
            "archive total declared size exceeds the safety cap "
            f"({ZIP_MAX_TOTAL_BYTES} bytes); refusing a possible zip bomb."
        )
    if info.compress_size > 0 and info.file_size // info.compress_size > ZIP_MAX_RATIO:
        raise SetupError(f"entry {raw!r} has an unsafe compression ratio.")
    mode = (info.external_attr >> 16) & 0o170000
    if mode == 0 and name.endswith("/"):
        mode = stat.S_IFDIR
    if mode not in (0, stat.S_IFREG, stat.S_IFDIR):
        raise SetupError(
            f"entry {raw!r} is not a regular file (symlink/device?) - rejected."
        )


def safe_extract(zip_path: Path, dest_dir: Path) -> list:
    """Validate every entry first, then extract with exact-member hashing.

    Returns per-member records ``[{"path", "size", "sha256"}]`` for every
    extracted regular file so later verification can prove the on-disk bytes
    still match the verified ZIP members. Directory entries are applied early.
    Refuses traversal/absolute/drive paths, symlink or device members,
    duplicate names, declared-size/cap mismatches and bombs before writing a
    single byte into dest_dir.
    """
    seen: set = set()
    running = [0]
    with zipfile.ZipFile(zip_path) as zf:
        infos = zf.infolist()
        for info in infos:
            _check_zip_entry(info, seen, running)
        dest_dir.mkdir(parents=True, exist_ok=True)
        records = []
        for info in infos:
            name = info.filename.replace("\\", "/")
            # canonical target is computed for EVERY member before branching
            target = dest_dir.joinpath(*[p for p in name.split("/") if p])
            if name.endswith("/") or target == dest_dir.resolve() or name == "":
                target.mkdir(parents=True, exist_ok=True)
                continue
            target.parent.mkdir(parents=True, exist_ok=True)
            hasher = hashlib.sha256()
            size = 0
            with zf.open(info) as src, open(target, "wb") as out:
                for chunk in iter(lambda: src.read(1024 * 1024), b""):
                    size += len(chunk)
                    hasher.update(chunk)
                    out.write(chunk)
            if size != info.file_size:
                raise SetupError(
                    f"member {info.filename!r} decompressed to {size} bytes "
                    f"but declares {info.file_size}; refusing the archive.")
            records.append({
                "path": str(target.relative_to(dest_dir)).replace(os.sep, "/"),
                "size": size,
                "sha256": hasher.hexdigest(),
            })
        return records


def binaries_from_records(records: list) -> list:
    return [r for r in records
            if Path(r["path"]).suffix.lower() in (".exe", ".dll")]


def find_main_executable(binary_records: list) -> str:
    """Locate whisper-cli.exe at any depth; fail closed when it is absent."""
    for rec in binary_records:
        norm = rec["path"].replace("\\", "/")
        if norm == "whisper-cli.exe" or norm.endswith("/whisper-cli.exe"):
            return rec["path"]
    raise SetupError(
        "extracted engine does not contain whisper-cli.exe; refusing to "
        "record this extraction.")


def verify_binaries(engine_dir: Path, binary_records: list) -> None:
    """Each extracted exe/dll must equal its verified ZIP member (size+hash).

    Tampered binaries are refused loudly: nothing is executed, overwritten or
    deleted; the user resolves unknown changes manually.
    """
    for rec in binary_records:
        target = engine_dir / rec["path"]
        mismatch = False
        try:
            mismatch = (not target.is_file() or
                        target.stat().st_size != rec["size"] or
                        sha256_file(target) != rec["sha256"])
        except (OSError, SetupError):
            mismatch = True
        if mismatch:
            raise SetupError(
                f"extracted binary {rec['path']} in {engine_dir} does not "
                "match its verified ZIP member hash/size. Nothing will be "
                "executed, overwritten or deleted; resolve it manually.")


def binaries_from_records(records: list) -> list:
    return [r for r in records
            if Path(r["path"]).suffix.lower() in (".exe", ".dll")]


# ---------------------------------------------------------------------------
# Setup primitive: engine + model, manifest, readme, licenses
# ---------------------------------------------------------------------------

def _contained_under(path, root: Path) -> bool:
    try:
        Path(path).resolve().relative_to(Path(root).resolve())
        return True
    except (ValueError, OSError):
        return False


def _atomic_write_json(path: Path, doc: dict) -> None:
    """Publish a JSON document by atomic FILE promotion (no directory rename).

    Directory renames are lock-fragile on Windows (an external open handle on
    any descendant fails the parent rename with WinError 5); files are moved
    one at a time after they are verified, which is reliable and atomic.
    """
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.parent / f"{path.name}.{secrets.token_hex(6)}.part"
    try:
        tmp.write_text(
            json.dumps(doc, indent=2, ensure_ascii=False) + "\n",
            encoding="utf-8",
        )
        os.replace(tmp, path)
    except BaseException:
        tmp.unlink(missing_ok=True)
        raise


def _write_marker(path: Path, state: str, engine: dict, note: str = "",
                  files=None) -> None:
    doc = {
        "kind": "gentle-shell voice local setup extraction marker",
        "state": state,
        "asset": engine["asset"],
        "zip_sha256": engine["sha256"],
        "tag": engine["tag"],
        "updated_at": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
    }
    if note:
        doc["note"] = note
    if files is not None:
        doc["files"] = files
    _atomic_write_json(path, doc)


def zip_member_records(zip_path: Path) -> list:
    """Read-only per-member size+SHA-256 records of the VERIFIED ZIP.

    Used to reconcile an own completed extraction whose marker predates the
    per-file record: the chain is upstream digest == ZIP bytes on disk ==
    recomputed member records == directory contents. No guessing is involved;
    any mismatch means the directory is left untouched.
    """
    with zipfile.ZipFile(zip_path) as zf:
        records = []
        for info in zf.infolist():
            name = info.filename.replace("\\", "/")
            if name == "" or name.endswith("/"):
                continue
            hasher = hashlib.sha256()
            size = 0
            with zf.open(info) as src:
                for chunk in iter(lambda: src.read(1024 * 1024), b""):
                    size += len(chunk)
                    hasher.update(chunk)
            records.append({
                "path": name,
                "size": size,
                "sha256": hasher.hexdigest(),
            })
        return records


def _matching_member_tree(dir_path: Path, records: list) -> bool:
    """True only when the directory tree equals the member records exactly
    (no missing, extra or altered files; the setup marker itself excluded)."""
    expected = {r["path"]: r for r in records}
    actual = set()
    try:
        for base, _dirs, files in os.walk(dir_path):
            for f in files:
                rel = str((Path(base) / f).relative_to(dir_path)) \
                    .replace(os.sep, "/")
                if rel == SETUP_MARKER_NAME:
                    continue
                rec = expected.get(rel)
                if rec is None:
                    return False
                target = dir_path / rel
                if target.stat().st_size != rec["size"]:
                    return False
                if sha256_file(target) != rec["sha256"]:
                    return False
                actual.add(rel)
    except OSError:
        return False
    return actual == set(expected)


def find_reusable_engine_dir(engine_root: Path, zip_path: Path,
                             engine: dict, log=print):
    """Return (dir, member_records) for an own completed extraction directory
    that exactly matches the verified ZIP, else None. Bounded scan: only the
    immediate children of the engine root that carry our marker; candidates
    failing any check are preserved untouched and never deleted.
    """
    if not engine_root.is_dir():
        return None
    for cand in sorted(p for p in engine_root.iterdir() if p.is_dir()):
        marker_path = cand / SETUP_MARKER_NAME
        if not marker_path.is_file():
            continue
        try:
            marker = json.loads(marker_path.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            continue
        if (not isinstance(marker, dict) or
                marker.get("state") != "complete" or
                marker.get("zip_sha256") != engine["sha256"] or
                not _contained_under(cand, engine_root)):
            continue
        files = marker.get("files")
        records = files if (isinstance(files, list) and files) else None
        if records is None:
            records = zip_member_records(zip_path)
        if not _matching_member_tree(cand, records):
            log(f"engine candidate {cand.name} does not match the verified "
                "ZIP members; preserved untouched, not reused")
            continue
        return cand, records
    return None


def _ensure_owned_dir(path: Path) -> None:
    if path.exists() and not path.is_dir():
        raise SetupError(f"{path} exists and is not a directory; aborting.")
    path.mkdir(parents=True, exist_ok=True)


def prepare_engine(layout_d: dict, engine: dict, fetch_json, stream_opener,
                   log=print) -> dict:
    tag = engine["tag"]
    engine_dir = layout_d["engine"] / tag
    zip_path = layout_d["engine_zip"]

    if zip_path.exists() and not verify_artifact(
            zip_path, engine["size"], engine["sha256"]):
        raise SetupError(
            f"{zip_path} exists but does not match the upstream SHA-256/size "
            "for this release. It will NOT be overwritten or deleted (unknown "
            "ownership). Remove it manually if you do not need it, then re-run."
        )
    if not zip_path.exists():
        log(f"downloading engine asset from {sanitize_url(engine['url'])} "
            f"({engine['size']} bytes)...")
        download_to(engine["url"], zip_path, engine["size"], engine["sha256"],
                    stream_opener, layout_d["downloads"], log=log)
    else:
        log(f"reusing verified {zip_path.name}.")

    manifest = _load_manifest(layout_d["root"])
    recorded = ((manifest.get("engine") or {}) if manifest else {})
    recorded_dir = recorded.get("dir")
    if recorded_dir:
        rd = Path(recorded_dir)
        if (recorded.get("sha256") == engine["sha256"] and
                recorded.get("binaries") and
                _contained_under(rd, layout_d["engine"])):
            verify_binaries(rd, recorded["binaries"])
            log(f"reusing verified engine at {rd} (published manifest "
                "destination; binaries verified).")
            return {"dir": rd, "binaries": recorded["binaries"],
                    "main": (recorded.get("main_executable") or
                             find_main_executable(recorded["binaries"])),
                    "extracted": [], "duplicates": []}
        if rd.exists():
            raise SetupError(
                f"{rd} is the published engine destination but does not "
                "verify against this release; it will NOT be reused, "
                "overwritten or deleted (unknown ownership). Remove it "
                "manually if you do not need it, then re-run."
            )

    # No verified published destination: try to reconcile an own completed
    # extraction directory (marker binds it to the verified ZIP hash; every
    # file must match the verified ZIP members' records exactly).
    found = find_reusable_engine_dir(layout_d["engine"], zip_path, engine,
                                     log=log)
    if found:
        rd, records = found
        binaries = binaries_from_records(records)
        main = find_main_executable(binaries)
        verify_binaries(rd, binaries)  # re-proof at the actual location
        _write_marker(rd / SETUP_MARKER_NAME, "complete", engine,
                      note="destination published atomically in setup.json "
                           "after binary and model verification",
                      files=records)
        log(f"reusing own verified extraction at {rd} (marker bound to the "
            "verified ZIP; every member record matched).")
        return {"dir": rd, "binaries": binaries, "main": main,
                "extracted": records, "extracted_now": False,
                "duplicates": []}
    log("no verified reusable engine directory found; "
        "creating a new unique directory")

    # No verified published destination available: extract into a NEW unique
    # directory under engine/. The directory itself is NEVER renamed; only
    # individual files are promoted atomically after verification.
    os.makedirs(layout_d["engine"], exist_ok=True)
    pre_existing = [p.name for p in sorted(layout_d["engine"].iterdir())
                    if p.is_dir()]
    target = layout_d["engine"] / f"{engine['tag']}-{secrets.token_hex(6)}"
    os.mkdir(target)  # nonce-named; a collision must never be reused/overwritten
    if pre_existing:
        log("duplicate engine storage preserved for user review (not touched, "
            "not deleted): " + ", ".join(pre_existing))
    marker = target / SETUP_MARKER_NAME
    _write_marker(marker, "extracting", engine)
    records = None
    try:
        records = safe_extract(zip_path, target)
        binaries = binaries_from_records(records)
        main = find_main_executable(binaries)  # fails closed when absent
        verify_binaries(target, binaries)
    except BaseException:
        # preserve the owned incomplete directory with a clear marker; never
        # delete it here and never touch unrelated directories
        try:
            _write_marker(marker, "incomplete", engine,
                          note="extraction incomplete; directory preserved; "
                               "do not execute anything in it",
                          files=records)
        except OSError:
            pass
        raise
    _write_marker(marker, "complete", engine,
                  note="destination published atomically in setup.json only "
                       "after binary and model verification",
                  files=records)
    log(f"extracted engine to {target} ({len(binaries)} verified "
        "exe/dll members).")
    return {"dir": target, "binaries": binaries, "main": main,
            "extracted": records, "extracted_now": True,
            "duplicates": pre_existing}


def prepare_model(layout_d: dict, model: dict, stream_opener, log=print) -> dict:
    final = layout_d["model_bin"]
    if final.exists() and not verify_artifact(
            final, model["size"], model["sha256"]):
        raise SetupError(
            f"{final} exists but does not match the upstream SHA-256/size for "
            f"revision {model['revision']}. It will NOT be overwritten or "
            "deleted (unknown ownership). Remove it manually if you do not "
            "need it, then re-run."
        )
    if not final.exists():
        log(f"downloading model from {sanitize_url(model['url'])} "
            f"({model['size']} bytes)...")
        download_to(model["url"], final, model["size"], model["sha256"],
                    stream_opener, layout_d["downloads"], log=log)
    else:
        log(f"reusing verified {final.name}.")
    return {"file": final}


def _load_manifest(root: Path) -> dict | None:
    f = root / "setup.json"
    if not f.is_file():
        return None
    try:
        return json.loads(f.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None


def _fetch_license_text(fetch_json_unused, stream_opener, tag: str, log=print):
    """Fetch the pinned MIT license text; tolerate unavailability (URL kept)."""
    url = GH_LICENSE_RAW.format(repo=GH_REPO, tag=tag)
    try:
        with stream_opener(url) as stream:
            return stream.read().decode("utf-8", "replace")
    except Exception as exc:  # license must not block installation
        log(f"license text not fetched ({exc.__class__.__name__}); URL will "
            "be recorded instead.")
        return None


def write_docs(root: Path, engine: dict, engine_result: dict, model: dict,
               license_text: str | None, log=print) -> dict:
    licenses_dir = root / "licenses"
    _ensure_owned_dir(licenses_dir)
    license_file = None
    if license_text:
        license_file = licenses_dir / "whisper.cpp-LICENSE.txt"
        license_file.write_text(license_text, encoding="utf-8")

    manifest = {
        "schema": "gentle-shell-voice-local-setup/1",
        "created_at": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        "project_root": str(root.parent.parent),
        "local_root": str(root),
        "engine": {
            "kind": "whisper.cpp official prerelease-flagged CPU x64 build",
            "repo": engine["repo"],
            "tag": engine["tag"],
            "release_id": engine["release_id"],
            "asset": engine["asset"],
            "url": engine["url"],
            "api_url": engine["api_url"],
            "size": engine["size"],
            "sha256": engine["sha256"],
            "digest_source": engine["digest_source"],
            "dir": str(engine_result["dir"]),
            "main_executable": engine_result["main"],
            "binaries": engine_result["binaries"],
            "license": {"name": "MIT",
                        "url": GH_LICENSE_RAW.format(repo=GH_REPO,
                                                     tag=engine["tag"])},
        },
        "model": {
            "kind": "whisper.cpp ggml multilingual base model (not .en)",
            "repo": model["repo"],
            "revision": model["revision"],
            "path": model["path"],
            "url": model["url"],
            "api_url": model["api_url"],
            "size": model["size"],
            "sha256": model["sha256"],
            "digest_source": model["digest_source"],
            "file": str(layout(repo_root=root.parent.parent)["model_bin"]),
            "license": {"name": "MIT",
                        "url": f"https://huggingface.co/{model['repo']}"},
        },
        "notes": {
            "isolation": "everything lives under .local/voice (git-ignored); "
                         "no system PATH, registry or global install changes.",
            "gui_wiring": "the app does NOT read setup.json automatically; "
                          "copy the two paths below into the app's Settings.",
            "engine_duplicate_storage": engine_result.get("duplicates", []),
        },
    }

    prior = _load_manifest(root)
    if prior and prior.get("engine", {}).get("tag") != engine["tag"]:
        history = prior.get("history", [])
        history.append({"tag": prior["engine"]["tag"],
                        "sha256": prior["engine"]["sha256"],
                        "superseded_at": time.strftime("%Y-%m-%dT%H:%M:%S%z")})
        manifest["history"] = history

    # README first, then the manifest published atomically LAST: setup.json is
    # the commit point that says this destination is ready, and its write is a
    # single atomic FILE promotion (no directory rename anywhere).
    (root / "README.local.md").write_text(_readme(manifest, license_file is not None),
                                          encoding="utf-8")
    _atomic_write_json(root / "setup.json", manifest)
    log("published setup.json and README.local.md.")
    return manifest


def _readme(manifest: dict, license_saved: bool) -> str:
    e, m = manifest["engine"], manifest["model"]
    exe = e["main_executable"]
    engine_dir = Path(e["dir"])
    return f"""# Local voice setup (isolated)

Everything in this folder is isolated to `<repo>/.local/voice` (git-ignored) on a
non-C drive. No package manager was used, nothing was installed globally, and no
system PATH or real app setting was changed. Free/public upstream binaries only.

## What was installed (hash-verified)

| Piece | Pinned source | Verified |
| --- | --- | --- |
| Engine `{exe}` | {e['repo']} official **prerelease-flagged** release `{e['tag']}` (latest tagged releases carry no Windows binaries), asset `{e['asset']}` | sha256 `{e['sha256']}` ({e['size']} bytes) |
| Model `ggml-base.bin` (multilingual) | {m['repo']} revision `{m['revision']}` | sha256 `{m['sha256']}` ({m['size']} bytes) |

Origin URLs:
- engine asset: {e['url']}
- engine release metadata: {e['api_url']}
- model (immutable revision): {m['url']}
- model metadata: {m['api_url']}

Licenses: both the whisper.cpp engine and the ggml models published in that repo
are MIT{"" if license_saved else " (text not fetchable this run; URLs recorded)"}
- {e['license']['url']}

## Exact GUI settings to use (copy these two paths)

Open the app's **Settings → Dictation** section (header "Dictation", icon mic):

1. **Mode**: choose `Local` (choices are Off / Local / Provider).
2. **Executable**: `{engine_dir / exe}`
3. **Model**: `{m['file']}`
4. Save. Recording still needs your explicit action (mic button in the
   composer); nothing is sent anywhere automatically.

This folder's `setup.json` and `README.local.md` are only a record for you; the
app does **not** read them automatically. You are in control of the two paths.

## Known prerequisite (not downloaded by this script)

`{exe}` is an MSVC build. If Windows reports a missing runtime DLL such as
`VCRUNTIME140.dll` or `MSVCP140.dll` when *you* try the CLI later, the official
Microsoft **Visual C++ Redistributable (x64)** is the prerequisite to install
yourself from https://aka.ms/vs/17/release/vc_redist.x64.exe ; this
bootstrap deliberately does not download unrelated installers.

## Re-running / verifying

    python -B scripts/setup-voice-local.py          # install/resume (idempotent)
    python -B scripts/setup-voice-local.py --check  # verify without downloading

Every run re-fetches upstream metadata and re-verifies SHA-256 + size; only
verified artifacts are reused. Extracted executables/DLLs are individually
hash-checked against their verified ZIP members on every run and `--check`; a
tampered binary is refused (never executed, overwritten or deleted). No model
load, inference, microphone capture or provider test is performed here.
"""


# ---------------------------------------------------------------------------
# Orchestration
# ---------------------------------------------------------------------------

def run(repo_root: Path | None = None, fetch_json=net_fetch_json,
        stream_opener=net_stream_opener, log=print, pin_tag: str | None = None,
        pin_revision: str | None = None, reuse_unknown: bool = False,
        validate=validate_root) -> dict:
    """Full preparation. Raises SetupError on any fail-closed condition.

    `validate` is the root validator (tests inject a permissive one; the
    production default enforces non-C drive + project containment).
    """
    if reuse_unknown:
        raise SetupError("trusting unverified existing files is not supported.")

    layout_d = layout(repo_root)
    validate(layout_d["root"].parent.parent, layout_d["root"])
    assert_no_repo_links(layout_d["root"])  # never mutate through linked children

    log_ptr = _file_log(layout_d["logs"], log)
    core = Path(repo_root) if repo_root is not None else \
        Path(__file__).resolve().parent.parent
    log_ptr(f"root validated: {core}; data under {layout_d['root']}")

    clean_own_part_files(layout_d["downloads"])
    clean_own_staging_dirs(layout_d["tmp"])

    engine = resolve_engine_release(fetch_json, pin_tag)
    log(f"engine pinned: {engine['tag']} sha256 {engine['sha256']} "
        f"({engine['size']} bytes)")
    model = resolve_model(fetch_json, pin_revision)
    log(f"model pinned: {model['revision']} sha256 {model['sha256']} "
        f"({model['size']} bytes)")

    _ensure_owned_dir(layout_d["root"])
    for sub in ("downloads", "tmp", "logs", "licenses", "engine", "models"):
        _ensure_owned_dir(layout_d["root"] / sub)

    engine_result = prepare_engine(layout_d, engine, fetch_json, stream_opener,
                                   log=log_ptr)
    model_result = prepare_model(layout_d, model, stream_opener, log=log_ptr)
    license_text = _fetch_license_text(fetch_json, stream_opener,
                                       engine["tag"], log=log_ptr)
    manifest = write_docs(layout_d["root"], engine, engine_result, model,
                          license_text, log=log_ptr)
    log("local voice setup ready; run the app tests when you are ready.")
    return {"engine": {**engine, "dir": engine_result["dir"],
                       "binaries": engine_result["binaries"]},
            "model": {**model, "file": model_result["file"]},
            "manifest": manifest}


def verify_only(repo_root: Path | None, resolve, validate=validate_root) -> dict:
    """Check an existing installation against fresh upstream metadata."""
    layout_d = layout(repo_root)
    validate(layout_d["root"].parent.parent, layout_d["root"])
    assert_no_repo_links(layout_d["root"])
    engine, model = resolve()
    ok_zip = verify_artifact(layout_d["engine_zip"], engine["size"],
                             engine["sha256"])
    ok_model = verify_artifact(layout_d["model_bin"], model["size"],
                               model["sha256"])
    manifest = _load_manifest(layout_d["root"]) or {}
    e = manifest.get("engine", {})
    ok_extract = bool(e) and (engine["sha256"] == e.get("sha256") and
                              e.get("dir") and
                              _contained_under(e["dir"], layout_d["engine"]))
    main = e.get("main_executable") or "whisper-cli.exe"
    if ok_extract and not (Path(e.get("dir")) / main).is_file():
        ok_extract = False
    ok_binaries = False
    if ok_extract and e.get("binaries"):
        # hard, loud failure naming the tampered member before the summary
        verify_binaries(Path(e["dir"]), e["binaries"])
        ok_binaries = True
    results = {"engine_zip": ok_zip, "engine_extracted": ok_extract,
               "engine_dir_contained": bool(e) and bool(e.get("dir")) and
                                       _contained_under(e.get("dir"),
                                                        layout_d["engine"]),
               "engine_binaries": ok_binaries, "model_file": ok_model,
               "engine": engine, "model": model}
    if not all((ok_zip, ok_model, ok_extract, ok_binaries)):
        raise SetupError(
            "verification failed (published engine destination must be "
            "contained in the engine root): " + json.dumps(
                {k: v for k, v in results.items() if isinstance(v, bool)}))
    return results


class _file_log:
    """Tee log lines into .local/voice/logs/ with a bounded, timestamped file."""

    def __init__(self, logs_dir: Path, echo=print):
        self._echo = echo
        self._fh = None
        try:
            logs_dir.mkdir(parents=True, exist_ok=True)
            f = logs_dir / f"setup-{time.strftime('%Y%m%dT%H%M%S')}.log"
            self._fh = open(f, "a", encoding="utf-8")
        except OSError:
            self._fh = None

    def __call__(self, message: str) -> None:
        try:
            self._echo(message)
            if self._fh:
                self._fh.write(f"{time.strftime('%H:%M:%S')} {message}\n")
                self._fh.flush()
        except Exception:
            pass

    def __del__(self):
        try:
            if self._fh:
                self._fh.close()
        except Exception:
            pass


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(
        description="Prepare an isolated local whisper.cpp setup under "
                    "<repo>/.local/voice (non-C drive only).")
    parser.add_argument("--check", action="store_true",
                        help="verify an existing install without downloading")
    parser.add_argument("--pin-tag", metavar="TAG", default=None,
                        help="pin the engine release tag instead of newest")
    args = parser.parse_args(argv)
    try:
        if args.check:
            results = verify_only(None, make_resolver(net_fetch_json))
            print(json.dumps({"ok": True, "engine": results["engine"]["tag"],
                              "model": results["model"]["revision"]},
                             indent=2))
        else:
            result = run(pin_tag=args.pin_tag)
            print(json.dumps({"ok": True, "engine_tag": result["engine"]["tag"],
                              "engine_dir": str(result["engine"]["dir"]),
                              "model": str(result["model"]["file"])},
                             indent=2))
        return 0
    except SetupError as exc:
        print(f"FAIL: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
