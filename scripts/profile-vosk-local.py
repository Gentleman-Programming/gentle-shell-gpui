"""Measure the prepared Vosk child process on a SYNTHETIC silent recording.

This is the profiling helper S12 asks for: it answers "what does one Vosk
transcription cost?" with numbers that were actually observed, instead of a
claim invented by whoever wrote the feature. It never opens a microphone and
never calls a provider; it builds a silent 16 kHz mono WAV in a temporary
directory, runs the app's real helper through the prepared venv Python with
the production command line (`python -I -B vosk_transcribe.py ...`), and
reports the child's wall time, CPU time, peak working set and thread count.

The model is the prepared one and its recorded ``sha256-tree/1`` digest is
verified before anything runs (a tampered model is refused, never measured).

Usage:
    python -B scripts/profile-vosk-local.py --check
    python -B scripts/profile-vosk-local.py [--seconds 3]

Exit codes: 0 measured, 1 not ready/refused, 2 usage error.
Run with: python -B scripts/test_profile_vosk_local.py (offline, no model).
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import struct
import subprocess
import sys
import tempfile
import time
from pathlib import Path

sys.dont_write_bytecode = True

_HERE = Path(__file__).resolve().parent
_ROOT = _HERE.parent
_SCHEMA = "gentle-shell-voice-vosk-setup/1"
DIGEST_ALGORITHM = "sha256-tree/1"
_RATE = 16000
_MAX_SECONDS = 30
_HELPER = _ROOT / "crates" / "app" / "src" / "features" / "dictation" / "vosk_transcribe.py"


class Refused(RuntimeError):
    """The setup cannot be profiled, and the message says why."""


def _is_link(path: Path) -> bool:
    try:
        info = os.lstat(path)
    except OSError:
        return True
    if os.path.islink(path):
        return True
    if sys.platform.startswith("win"):
        return bool(info.st_file_attributes & 0x0400)
    return False


def tree_digest(root: Path) -> str:
    """``sha256-tree/1``, the recipe the installer, the launcher and the app
    all implement: files sorted by POSIX relative path, hashing
    ``path NUL length NUL content``."""
    entries = list(root.rglob("*"))
    for entry in entries:
        if _is_link(entry):
            raise Refused(f"the model tree contains a link at {entry}")
    digest = hashlib.sha256()
    for path in sorted((entry for entry in entries if entry.is_file()),
                       key=lambda entry: entry.relative_to(root).as_posix()):
        relative = path.relative_to(root).as_posix().encode("utf-8")
        content = path.read_bytes()
        digest.update(relative)
        digest.update(bytes([0]))
        digest.update(str(len(content)).encode("ascii"))
        digest.update(bytes([0]))
        digest.update(content)
    return digest.hexdigest()


def load_prepared(project_root: Path) -> dict:
    """The manifest, with its containment, recipe and digest verified."""
    manifest_path = project_root / ".local" / "voice" / "vosk" / "setup.json"
    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        raise Refused("there is no prepared Vosk setup; run setup-vosk-local.py first")
    if manifest.get("schema") != _SCHEMA:
        raise Refused("the Vosk manifest is not ours (wrong schema)")
    vosk_root = (project_root / ".local" / "voice" / "vosk").resolve()
    python = Path(manifest.get("python", ""))
    model = Path(manifest.get("model_dir", ""))
    if not python.is_absolute() or not model.is_absolute():
        raise Refused("the manifest paths must be absolute")
    for label, path in (("venv Python", python), ("model directory", model)):
        if _is_link(path):
            raise Refused(f"the {label} is a link; refusing to measure through it")
        if not path.resolve().is_relative_to(vosk_root):
            raise Refused(f"the {label} is outside .local/voice/vosk")
    if not python.is_file():
        raise Refused("the prepared venv Python is missing")
    if not model.is_dir():
        raise Refused("the prepared model directory is missing")
    record = manifest.get("model") or {}
    if record.get("digest_algorithm") != DIGEST_ALGORITHM:
        raise Refused(f"the manifest does not record {DIGEST_ALGORITHM}; "
                      "re-run the Vosk setup")
    if tree_digest(model) != record.get("sha256_local"):
        raise Refused("the model does not match its recorded digest; refusing")
    return {"python": python, "model": model, "manifest": manifest_path}


def silent_wav(seconds: int) -> bytes:
    """A 16 kHz mono 16-bit silent WAV: no device, no speech."""
    frames = b"\x00\x00" * (_RATE * seconds)
    header = struct.pack("<4sI4s4sIHHIIHH4sI",
                         b"RIFF", 36 + len(frames), b"WAVE", b"fmt ", 16, 1, 1,
                         _RATE, _RATE * 2, 2, 16, b"data", len(frames))
    return header + frames


def _posix_counters(pid: int) -> dict:
    counters: dict = {}
    try:
        with open(f"/proc/{pid}/status", encoding="utf-8") as status:
            for line in status:
                key, _, value = line.partition(":")
                value = value.strip()
                if key == "Threads" and value.isdigit():
                    counters["threads"] = int(value)
                elif key == "VmHWM" and value.endswith("kB"):
                    counters["peak_rss_bytes"] = int(value.split()[0]) * 1024
    except OSError:
        pass
    if "peak_rss_bytes" not in counters and shutil.which("ps"):
        try:
            out = subprocess.run(["ps", "-o", "rss=", "-p", str(pid)],
                                 capture_output=True, text=True, check=True).stdout
            if out.strip().isdigit():
                counters["peak_rss_bytes"] = int(out.strip()) * 1024
        except (subprocess.CalledProcessError, ValueError):
            pass
    return counters


def _windows_counters(handle) -> dict:
    import ctypes
    import ctypes.wintypes as wintypes

    class Counters(ctypes.Structure):
        _fields_ = [
            ("cb", wintypes.DWORD),
            ("PageFaultCount", wintypes.DWORD),
            ("PeakWorkingSetSize", ctypes.c_size_t),
            ("WorkingSetSize", ctypes.c_size_t),
            ("QuotaPeakPagedPoolUsage", ctypes.c_size_t),
            ("QuotaPagedPoolUsage", ctypes.c_size_t),
            ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t),
            ("QuotaNonPagedPoolUsage", ctypes.c_size_t),
            ("PagefileUsage", ctypes.c_size_t),
            ("PeakPagefileUsage", ctypes.c_size_t),
        ]

    counters: dict = {}
    creation, exit_time, kernel, user = (wintypes.FILETIME() for _ in range(4))
    if ctypes.windll.kernel32.GetProcessTimes(
            handle, ctypes.byref(creation), ctypes.byref(exit_time),
            ctypes.byref(kernel), ctypes.byref(user)):
        ticks = ((kernel.dwHighDateTime << 32 | kernel.dwLowDateTime)
                 + (user.dwHighDateTime << 32 | user.dwLowDateTime))
        counters["cpu_seconds"] = ticks / 10_000_000
    memory = Counters()
    memory.cb = ctypes.sizeof(Counters)
    if ctypes.windll.psapi.GetProcessMemoryInfo(handle, ctypes.byref(memory),
                                                memory.cb):
        counters["peak_rss_bytes"] = int(memory.PeakWorkingSetSize)
    return counters


def profile(project_root: Path, seconds: int, timeout: float,
            python: Path | None = None, helper: Path | None = None) -> dict:
    prepared = load_prepared(project_root)
    helper = _HELPER if helper is None else helper
    if not helper.is_file():
        raise Refused(f"the transcription helper is missing at {helper}")
    python = prepared["python"] if python is None else python
    workdir = Path(tempfile.mkdtemp(prefix="gentle-vosk-profile-"))
    try:
        wav = workdir / "synthetic.wav"
        wav.write_bytes(silent_wav(seconds))
        transcript = workdir / "transcript.txt"
        output = workdir / "helper-stdout.txt"
        command = [str(python), "-I", "-B", str(helper),
                   "--model", str(prepared["model"]),
                   "--input", str(wav), "--output", str(transcript)]
        cpu_before = None
        if not sys.platform.startswith("win"):
            import resource

            cpu_before = resource.getrusage(resource.RUSAGE_CHILDREN)
        env = {key: value for key, value in os.environ.items()
               if key.upper() in {"PATH", "SYSTEMROOT", "WINDIR", "COMSPEC",
                                  "TEMP", "TMP", "HOME", "USERPROFILE",
                                  "LANG", "LC_ALL"}
               or key.upper().startswith("LC_")}
        # The child's own stdout is not the measurement: send it to a file so
        # a chatty helper cannot fill a pipe while we sample it.
        with open(output, "wb") as sink:
            started = time.monotonic()
            process = subprocess.Popen(command, stdout=sink,
                                       stderr=subprocess.DEVNULL, env=env)
            counters: dict = {}
            while process.poll() is None:
                if time.monotonic() - started > timeout:
                    process.kill()
                    process.wait()
                    raise Refused(f"the profiled child ran past {timeout:.0f} s")
                sample = (_windows_counters(process._handle)
                          if sys.platform.startswith("win")
                          else _posix_counters(process.pid))
                for key, value in sample.items():
                    counters[key] = max(counters.get(key, 0), value)
                time.sleep(0.1)
            wall = time.monotonic() - started
            process.wait()
        if cpu_before is not None:
            cpu_after = resource.getrusage(resource.RUSAGE_CHILDREN)
            counters["cpu_seconds"] = ((cpu_after.ru_utime - cpu_before.ru_utime)
                                       + (cpu_after.ru_stime - cpu_before.ru_stime))
        recognized = None
        if transcript.is_file():
            recognized = transcript.read_text(encoding="utf-8", errors="replace")
        return {
            "ok": process.returncode == 0,
            "engine": "vosk",
            "synthetic": {"wave": "16 kHz mono PCM silence",
                          "seconds": seconds,
                          "microphone": False,
                          "model": "prepared model, digest verified"},
            "measured": {
                "wall_seconds": round(wall, 3),
                "cpu_seconds": (round(counters["cpu_seconds"], 3)
                                if counters.get("cpu_seconds") is not None else None),
                "peak_rss_bytes": counters.get("peak_rss_bytes"),
                "threads": counters.get("threads"),
                "exit_code": process.returncode,
                "transcript_bytes": len(recognized or ""),
            },
            "notes": [
                "One synthetic silent run on the prepared model; no microphone "
                "and no provider were used.",
                "threads is null where this platform does not expose it to "
                "the sampler (Windows); cpu_seconds is null when the platform "
                "cannot report it.",
                "The numbers describe THIS machine and THIS model; they are "
                "evidence, not a promise about other hardware.",
            ],
        }
    finally:
        shutil.rmtree(workdir, ignore_errors=True)


def check(project_root: Path) -> dict:
    try:
        prepared = load_prepared(project_root)
    except Refused as refusal:
        return {"ok": False, "engine": "vosk", "reasons": [str(refusal)]}
    return {"ok": True, "engine": "vosk",
            "manifest": str(prepared["manifest"]),
            "python": str(prepared["python"]),
            "model": str(prepared["model"]),
            "next": "python -B scripts/profile-vosk-local.py --seconds 3 --json"}


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Measure one synthetic Vosk run.")
    parser.add_argument("--project-root", default=str(_ROOT))
    parser.add_argument("--check", action="store_true",
                        help="report readiness; runs nothing")
    parser.add_argument("--seconds", type=int, default=3,
                        help=f"synthetic silence length ({1}-{_MAX_SECONDS})")
    parser.add_argument("--timeout", type=float, default=120.0)
    parser.add_argument("--python", default=None,
                        help="override the venv Python (defaults to the manifest)")
    parser.add_argument("--helper", default=None,
                        help="override the transcription helper path")
    args = parser.parse_args(argv)
    if not 1 <= args.seconds <= _MAX_SECONDS:
        parser.error(f"--seconds must be between 1 and {_MAX_SECONDS}")
    project_root = Path(args.project_root).resolve()
    try:
        if args.check:
            report = check(project_root)
        else:
            report = profile(project_root, args.seconds, args.timeout,
                             Path(args.python) if args.python else None,
                             Path(args.helper) if args.helper else None)
    except Refused as refusal:
        print(json.dumps({"ok": False, "engine": "vosk", "reasons": [str(refusal)]}))
        return 1
    print(json.dumps(report))
    return 0 if report.get("ok") else 1


if __name__ == "__main__":
    sys.exit(main())