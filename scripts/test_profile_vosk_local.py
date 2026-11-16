"""Deterministic, offline tests for scripts/profile-vosk-local.py.

No model, no microphone and no network: the measured child is the system
Python running a fixture helper. Scratch stays inside the project-local
ignored tree ``.local/voice/tmp``.
Run with: python -B scripts/test_profile_vosk_local.py
"""

from __future__ import annotations

import contextlib
import hashlib
import importlib.util
import io
import json
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.dont_write_bytecode = True

_HERE = Path(__file__).resolve().parent
_SCRATCH_BASE = _HERE.parent / ".local" / "voice" / "tmp"
_SPEC = importlib.util.spec_from_file_location(
    "profile_vosk_local", _HERE / "profile-vosk-local.py")
assert _SPEC is not None and _SPEC.loader is not None
pvl = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(pvl)


def tree_digest(root: Path) -> str:
    digest = hashlib.sha256()
    for path in sorted((entry for entry in root.rglob("*") if entry.is_file()),
                       key=lambda entry: entry.relative_to(root).as_posix()):
        relative = path.relative_to(root).as_posix().encode("utf-8")
        content = path.read_bytes()
        digest.update(relative + b"\x00" + str(len(content)).encode("ascii")
                      + b"\x00" + content)
    return digest.hexdigest()


class ProfileCase(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory(dir=_SCRATCH_BASE)
        self.root = Path(self._tmp.name)
        self.vosk = self.root / ".local" / "voice" / "vosk"
        self.model = self.vosk / "models" / "small"
        (self.model / "am").mkdir(parents=True)
        (self.model / "am" / "final.mdl").write_bytes(b"weights")
        self.python = self.vosk / "venv" / "python"
        self.python.parent.mkdir()
        self.python.write_bytes(b"stub")
        self.helper = self.root / "helper.py"
        self.argv_log = self.root / "argv.txt"
        self.helper.write_text(
            "import json, sys, time\n"
            "flags = {'isolated': sys.flags.isolated,\n"
            "         'dont_write_bytecode': sys.flags.dont_write_bytecode,\n"
            "         'argv': sys.argv[1:]}\n"
            f"open({str(self.argv_log)!r}, 'w').write(json.dumps(flags))\n"
            "time.sleep(0.2)\n"
            "args = sys.argv[1:]\n"
            "with open(args[args.index('--output') + 1], 'w') as sink:\n"
            "    sink.write('synthetic')\n",
            encoding="utf-8")
        self.write_manifest(tree_digest(self.model))

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def write_manifest(self, digest: str) -> None:
        manifest = {
            "schema": "gentle-shell-voice-vosk-setup/1",
            "python": str(self.python),
            "model_dir": str(self.model),
            "model": {"sha256_local": digest,
                      "digest_algorithm": "sha256-tree/1"},
        }
        (self.vosk / "setup.json").write_text(json.dumps(manifest), encoding="utf-8")

    def run_cli(self, *extra: str) -> tuple[int, dict, str]:
        captured = io.StringIO()
        with contextlib.redirect_stdout(captured):
            code = pvl.main(["--project-root", str(self.root), *extra])
        output = captured.getvalue()
        return code, json.loads(output.splitlines()[-1]) if output.strip() else {}, output


class Readiness(ProfileCase):
    def test_check_reports_a_verified_setup_as_ready(self) -> None:
        code, report, _ = self.run_cli("--check")
        self.assertEqual(code, 0, report)
        self.assertTrue(report["ok"])
        self.assertIn("profile-vosk-local.py", report["next"])

    def test_check_without_a_setup_is_not_ready(self) -> None:
        (self.vosk / "setup.json").unlink()
        code, report, _ = self.run_cli("--check")
        self.assertEqual(code, 1)
        self.assertFalse(report["ok"])
        self.assertIn("no prepared Vosk setup", report["reasons"][0])

    def test_a_tampered_model_is_refused_before_measuring(self) -> None:
        (self.model / "am" / "final.mdl").write_bytes(b"tampered")
        with mock.patch.object(pvl.subprocess, "Popen") as popen:
            code, report, _ = self.run_cli("--seconds", "1", "--python", sys.executable,
                                           "--helper", str(self.helper))
        self.assertEqual(code, 1)
        self.assertFalse(report["ok"])
        self.assertIn("does not match its recorded digest", report["reasons"][0])
        popen.assert_not_called()
        self.assertEqual((self.model / "am" / "final.mdl").read_bytes(), b"tampered")


class Measurement(ProfileCase):
    def test_a_synthetic_run_reports_observed_numbers(self) -> None:
        code, report, _ = self.run_cli("--seconds", "1", "--python", sys.executable,
                                       "--helper", str(self.helper))
        self.assertEqual(code, 0, report)
        self.assertTrue(report["ok"])
        self.assertFalse(report["synthetic"]["microphone"])
        self.assertEqual(report["synthetic"]["seconds"], 1)
        measured = report["measured"]
        self.assertGreaterEqual(measured["wall_seconds"], 0.2)
        self.assertGreater(measured["peak_rss_bytes"], 0)
        self.assertIn("threads", measured)
        self.assertEqual(measured["exit_code"], 0)
        self.assertEqual(measured["transcript_bytes"], len("synthetic"))
        self.assertTrue(any("not a promise" in note for note in report["notes"]))

    def test_the_measured_child_gets_the_production_invocation(self) -> None:
        self.run_cli("--seconds", "1", "--python", sys.executable,
                     "--helper", str(self.helper))
        log = json.loads(self.argv_log.read_text(encoding="utf-8"))
        self.assertTrue(log["isolated"], "the child runs isolated (-I)")
        self.assertEqual(log["dont_write_bytecode"], 1, "the child runs with -B")
        self.assertIn("--model", log["argv"])
        self.assertIn(str(self.model), log["argv"])
        self.assertIn("--input", log["argv"])
        self.assertIn("--output", log["argv"])

    def test_a_failing_child_is_reported_not_hidden(self) -> None:
        self.helper.write_text("raise SystemExit(3)\n", encoding="utf-8")
        code, report, _ = self.run_cli("--seconds", "1", "--python", sys.executable,
                                       "--helper", str(self.helper))
        self.assertEqual(code, 1)
        self.assertFalse(report["ok"])
        self.assertEqual(report["measured"]["exit_code"], 3)

    def test_the_synthetic_wav_is_silent_sixteen_kilohertz_mono(self) -> None:
        import wave

        data = pvl.silent_wav(2)
        self.assertEqual(data[:4], b"RIFF")
        with wave.open(io.BytesIO(data), "rb") as recording:
            self.assertEqual(recording.getframerate(), 16000)
            self.assertEqual(recording.getnchannels(), 1)
            self.assertEqual(recording.getsampwidth(), 2)
            self.assertEqual(recording.getnframes(), 16000 * 2)
            self.assertEqual(set(recording.readframes(recording.getnframes())), {0})

    def test_out_of_range_seconds_is_a_usage_error(self) -> None:
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr), self.assertRaises(SystemExit) as raised:
            pvl.main(["--project-root", str(self.root), "--seconds", "0"])
        self.assertEqual(raised.exception.code, 2)
        self.assertIn("--seconds", stderr.getvalue())

    def test_the_temporary_workdir_is_removed(self) -> None:
        created: list[Path] = []
        original = pvl.tempfile.mkdtemp

        def record(*args, **kwargs):
            path = original(*args, **kwargs)
            created.append(Path(path))
            return path

        with mock.patch.object(pvl.tempfile, "mkdtemp", record):
            self.run_cli("--seconds", "1", "--python", sys.executable,
                         "--helper", str(self.helper))
        self.assertTrue(created)
        self.assertFalse(created[0].exists(), "the profiler leaves no scratch behind")


if __name__ == "__main__":
    unittest.main(verbosity=2)