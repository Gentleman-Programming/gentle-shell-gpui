"""Deterministic, no-network tests for scripts/launch-voice-sandbox.py.

The launcher isolates a build of this checkout for voice-dictation testing.
These tests never spawn the app, never touch the real user configuration,
and keep all scratch inside the project-local ignored tree
``.local/voice/tmp``. Run with:  python -B scripts/test_launch_voice_sandbox.py
"""

from __future__ import annotations

import contextlib
import importlib.util
import io
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path

sys.dont_write_bytecode = True

_HERE = Path(__file__).resolve().parent
_REAL_ROOT = _HERE.parent
_SCRATCH_BASE = _REAL_ROOT / ".local" / "voice" / "tmp"
_SPEC = importlib.util.spec_from_file_location(
    "launch_voice_sandbox", _HERE / "launch-voice-sandbox.py"
)
assert _SPEC is not None and _SPEC.loader is not None
lvs = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(lvs)


class Sandbox(unittest.TestCase):
    """A fake checkout with a prepared voice setup, on the project scratch."""

    def _fail_silently(self, fn):
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr), self.assertRaises(SystemExit) as ctx:
            fn()
        self.assertEqual(ctx.exception.code, 2)
        return stderr.getvalue()

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory(dir=_SCRATCH_BASE)
        self.root = Path(self._tmp.name) / "repo"
        (self.root / "scripts").mkdir(parents=True)
        voice = self.root / ".local" / "voice"
        (voice / "engine" / "e-1" / "Release").mkdir(parents=True)
        (voice / "models").mkdir(parents=True)
        self.exe = voice / "engine" / "e-1" / "Release" / "whisper-cli.exe"
        self.model = voice / "models" / "ggml-base.bin"
        self.exe.write_bytes(b"fake engine")
        self.model.write_bytes(b"fake model")
        # A verified voice tree: a tiny source archive whose members match
        # the engine files exactly, and a model the pins describe.
        import hashlib
        import io as _io
        import zipfile
        (voice / "downloads").mkdir(exist_ok=True)
        members = {"Release/whisper-cli.exe": b"fake engine", "Release/ggml.dll": b"dll"}
        buf = _io.BytesIO()
        with zipfile.ZipFile(buf, "w") as archive:
            for name, payload in members.items():
                archive.writestr(name, payload)
        (voice / "downloads" / "whisper-bin-x64.zip").write_bytes(buf.getvalue())
        (voice / "engine" / "e-1" / "Release" / "ggml.dll").write_bytes(b"dll")
        self.pins = {
            "zip_sha256": hashlib.sha256(buf.getvalue()).hexdigest(),
            "model_sha256": hashlib.sha256(b"fake model").hexdigest(),
            "model_size": len(b"fake model"),
        }
        manifest = {
            "schema": "gentle-shell-voice-local-setup/1",
            "project_root": str(self.root),
            "local_root": str(voice),
            "engine": {
                "dir": str(voice / "engine" / "e-1"),
                "main_executable": "Release/whisper-cli.exe",
            },
            "model": {"file": str(self.model), "size": self.model.stat().st_size},
        }
        (voice / "setup.json").write_text(json.dumps(manifest), encoding="utf-8")
        (self.root / "scripts" / "setup-voice-local.py").write_text("# bootstrap\n")

    def tearDown(self) -> None:
        self._tmp.cleanup()

    # -- helpers ---------------------------------------------------------

    def write_fake_exe(self) -> Path:
        exe = self.root / "target" / "debug"
        exe.mkdir(parents=True)
        binary = exe / lvs.APP_BINARY
        binary.write_bytes(b"MZ fake build")
        return binary


class SandboxEnvironment(Sandbox):
    def test_every_managed_variable_points_into_the_sandbox(self) -> None:
        env = lvs.sandbox_env(self.root, Path(self._tmp.name) / "sandbox")
        sandbox = Path(self._tmp.name) / "sandbox"
        for key in lvs.MANAGED_ENV:
            self.assertIn(key, env, key)
            if key == "GENTLE_SHELL_ARGS":
                self.assertEqual(env[key], lvs.ISOLATED_SHELL_ARGS)
            else:
                self.assertTrue(env[key].startswith(str(sandbox)), f"{key}={env[key]}")

    def test_the_real_user_configuration_is_never_referenced(self) -> None:
        real_appdata = os.environ.get("APPDATA", "")
        env = lvs.sandbox_env(self.root, Path(self._tmp.name) / "sandbox")
        if real_appdata:
            for key, value in env.items():
                self.assertFalse(
                    value.startswith(real_appdata),
                    f"{key} leaks the real APPDATA",
                )
        self.assertNotEqual(env["APPDATA"], os.environ.get("APPDATA"))

    def test_managed_keys_split_across_the_documented_homes(self) -> None:
        env = lvs.sandbox_env(self.root, Path(self._tmp.name) / "sandbox")
        # Agent homes (Pi) and the app/config homes must be distinct trees.
        agent = Path(env["PI_CODING_AGENT_DIR"])
        config = Path(env["GENTLE_PI_CONFIG_HOME"])
        appdata = Path(env["APPDATA"])
        self.assertEqual(agent, Path(env["GENTLE_SHELL_USER_PI_HOME"]))
        self.assertEqual(agent, Path(env["GENTLE_SHELL_HOME"]))
        self.assertNotEqual(agent, config)
        self.assertNotEqual(agent, appdata)


class SandboxPreparation(Sandbox):
    def test_prepare_creates_a_fresh_isolated_sandbox(self) -> None:
        self.write_fake_exe()
        plan = lvs.prepare(self.root, sandbox_root=None, app_exe=None, pins=self.pins)
        sandbox = Path(plan["sandbox"])
        settings_path = Path(plan["settings"])
        self.assertTrue(sandbox.is_dir())
        self.assertTrue(settings_path.is_file())
        settings = json.loads(settings_path.read_text(encoding="utf-8"))
        self.assertTrue(settings["onboarding"]["done"], "onboarding must be marked done")
        dictation = settings["dictation"]
        self.assertEqual(dictation["mode"], "Local")
        self.assertEqual(dictation["local"]["executable"], str(self.exe))
        self.assertEqual(dictation["local"]["model"], str(self.model))

    def test_prepare_writes_nothing_outside_the_sandbox(self) -> None:
        self.write_fake_exe()
        # A fake "real" user config tree: the launcher must not touch it.
        with tempfile.TemporaryDirectory(dir=_SCRATCH_BASE) as real:
            real_appdata = Path(real) / "real-appdata"
            real_appdata.mkdir()
            before = sorted(p.name for p in real_appdata.iterdir())
            os.environ["APPDATA"] = str(real_appdata)
            try:
                lvs.prepare(self.root, sandbox_root=None, app_exe=None, pins=self.pins)
            finally:
                restored = os.environ.pop("APPDATA", None)
                if restored is not None and restored == str(real_appdata):
                    pass  # removed below
            after = sorted(p.name for p in real_appdata.iterdir())
            self.assertEqual(before, after, "the real APPDATA must stay untouched")

    def test_prepare_rejects_a_manifest_that_leaves_the_workspace(self) -> None:
        self.write_fake_exe()
        voice = self.root / ".local" / "voice"
        outside = self._tmp.name + "/outside-model.bin"
        Path(outside).write_bytes(b"model")
        manifest = {
            "schema": "gentle-shell-voice-local-setup/1",
            "project_root": str(self.root),
            "local_root": str(voice),
            "engine": {"dir": str(voice / "engine" / "e-1"),
                       "main_executable": "Release/whisper-cli.exe"},
            "model": {"file": outside, "size": 5},
        }
        (voice / "setup.json").write_text(json.dumps(manifest), encoding="utf-8")
        with self.assertRaises(SystemExit):
            lvs.prepare(self.root, sandbox_root=None, app_exe=None, pins=self.pins)

    def test_prepare_rejects_a_missing_build(self) -> None:
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr), self.assertRaises(SystemExit) as ctx:
            lvs.prepare(self.root, sandbox_root=None, app_exe=None, pins=self.pins)
        self.assertEqual(ctx.exception.code, 2)
        self.assertIn("gentle-shell-gpui", stderr.getvalue())


class SpawnPlan(Sandbox):
    def test_child_environment_is_isolated_without_mutating_the_parent(self) -> None:
        self.write_fake_exe()
        plan = lvs.prepare(self.root, sandbox_root=None, app_exe=None, pins=self.pins)
        original = os.environ.get("APPDATA")
        child = lvs.child_environment(plan["env"])
        self.assertIsNot(child, os.environ)
        self.assertEqual(os.environ.get("APPDATA"), original)
        for key in lvs.MANAGED_ENV:
            self.assertIn(key, child)
            if key == "GENTLE_SHELL_ARGS":
                self.assertEqual(child[key], lvs.ISOLATED_SHELL_ARGS)
            else:
                self.assertTrue(child[key].startswith(plan["sandbox"]))

    def test_a_prepared_plan_reports_the_log_location(self) -> None:
        self.write_fake_exe()
        plan = lvs.prepare(self.root, sandbox_root=None, app_exe=None, pins=self.pins)
        self.assertTrue(plan["log"].endswith(".log"))
        self.assertTrue(plan["app"].endswith(lvs.APP_BINARY))


class SandboxRootSafety(Sandbox):
    """--sandbox-root and --exe must never reach outside this workspace,
    and no file may be created before every refusal check has passed."""

    def test_an_explicit_sandbox_root_outside_the_voice_tree_is_refused(self) -> None:
        self.write_fake_exe()
        outside = self._tmp.name + "/elsewhere/run-x"
        message = self._fail_silently(
            lambda: lvs.prepare(self.root, sandbox_root=Path(outside), app_exe=None)
        )
        self.assertIn("sandbox", message)
        self.assertFalse(Path(outside).exists(), "nothing may be created outside")

    def test_a_sandbox_under_the_fake_real_appdata_is_refused(self) -> None:
        self.write_fake_exe()
        with tempfile.TemporaryDirectory(dir=_SCRATCH_BASE) as real:
            fake_real = Path(real) / "appdata"
            fake_real.mkdir()
            old = os.environ.get("APPDATA")
            os.environ["APPDATA"] = str(fake_real)
            try:
                target = fake_real / "run-x"
                self._fail_silently(
                    lambda: lvs.prepare(self.root, sandbox_root=target, app_exe=None)
                )
                self.assertFalse(target.exists())
            finally:
                if old is None:
                    os.environ.pop("APPDATA", None)
                else:
                    os.environ["APPDATA"] = old

    @unittest.skipUnless(os.name == "posix", "symlink escape")
    def test_a_parent_link_escape_is_refused_before_any_creation(self) -> None:
        self.write_fake_exe()
        outside = Path(self._tmp.name) / "outside-tree"
        outside.mkdir()
        sandbox = self.root / ".local" / "voice" / "sandbox"
        sandbox.mkdir(parents=True)
        os.symlink(outside, sandbox / "run-link")
        self._fail_silently(
            lambda: lvs.prepare(self.root, sandbox_root=sandbox / "run-link", app_exe=None)
        )
        self.assertEqual(list(outside.iterdir()), [], "the linked target stayed empty")

    def test_an_existing_run_directory_is_never_reused(self) -> None:
        self.write_fake_exe()
        existing = self.root / ".local" / "voice" / "sandbox" / "run-old"
        existing.mkdir(parents=True)
        marker = existing / "state.json"
        marker.write_text("{}", encoding="utf-8")
        self._fail_silently(
            lambda: lvs.prepare(self.root, sandbox_root=existing, app_exe=None)
        )
        self.assertEqual(marker.read_text(encoding="utf-8"), "{}")

    def test_run_directory_names_carry_a_nonce(self) -> None:
        self.write_fake_exe()
        first = lvs.prepare(self.root, sandbox_root=None, app_exe=None, pins=self.pins)["sandbox"]
        second = lvs.prepare(self.root, sandbox_root=None, app_exe=None, pins=self.pins)["sandbox"]
        self.assertNotEqual(first, second, "same-second launches need distinct sandboxes")

    def test_the_exe_override_must_stay_inside_the_checkout(self) -> None:
        outside = Path(self._tmp.name) / "elsewhere"
        outside.mkdir(exist_ok=True)
        outside = outside / "gentle-shell-gpui.exe"
        outside.write_bytes(b"MZ")
        message = self._fail_silently(
            lambda: lvs.prepare(self.root, sandbox_root=None, app_exe=outside)
        )
        self.assertIn("checkout", message)


class ContentVerification(Sandbox):
    """The engine tree and model are verified against the pinned source
    archive, not against the editable manifest. Fixture pins stand in for
    the published ones through the verifier's seam."""

    def _fixture_voice(self) -> dict:
        import hashlib
        return self.pins

    def test_a_tampered_engine_file_is_refused_without_overwrite(self) -> None:
        pins = self._fixture_voice()
        self.write_fake_exe()
        dll = self.root / ".local" / "voice" / "engine" / "e-1" / "Release" / "ggml.dll"
        dll.write_bytes(b"tampered")
        message = self._fail_silently(
            lambda: lvs.prepare(self.root, sandbox_root=None, app_exe=None, pins=pins)
        )
        self.assertIn("does not match", message)
        self.assertEqual(dll.read_bytes(), b"tampered", "artifacts stay intact")

    def test_a_tampered_manifest_cannot_bypass_the_verification(self) -> None:
        import hashlib
        pins = self._fixture_voice()
        self.write_fake_exe()
        dll = self.root / ".local" / "voice" / "engine" / "e-1" / "Release" / "ggml.dll"
        dll.write_bytes(b"tampered")
        # The manifest now claims the correct sha256 for the tampered file;
        # records are derived from the verified archive, so this must not help.
        voice = self.root / ".local" / "voice"
        manifest = json.loads((voice / "setup.json").read_text(encoding="utf-8"))
        import hashlib as _h
        manifest["engine"]["binaries"] = [
            {"path": "Release/ggml.dll", "sha256": _h.sha256(b"tampered").hexdigest()}
        ]
        (voice / "setup.json").write_text(json.dumps(manifest), encoding="utf-8")
        self._fail_silently(
            lambda: lvs.prepare(self.root, sandbox_root=None, app_exe=None, pins=pins)
        )
        self.assertEqual(dll.read_bytes(), b"tampered")

    def test_a_zip_that_does_not_match_its_pin_is_refused(self) -> None:
        pins = self._fixture_voice()
        self.write_fake_exe()
        zip_path = self.root / ".local" / "voice" / "downloads" / "whisper-bin-x64.zip"
        zip_path.write_bytes(b"not the archive")
        message = self._fail_silently(
            lambda: lvs.prepare(self.root, sandbox_root=None, app_exe=None, pins=pins)
        )
        self.assertIn("archive", message)


class ChildArgs(Sandbox):
    def test_inherited_shell_args_cannot_override_the_sandbox(self) -> None:
        self.write_fake_exe()
        old = os.environ.get("GENTLE_SHELL_ARGS")
        os.environ["GENTLE_SHELL_ARGS"] = "--home C:\\Users\\real\\pi --link"
        try:
            plan = lvs.prepare(self.root, sandbox_root=None, app_exe=None, pins=self.pins)
            child = lvs.child_environment(plan["env"])
        finally:
            if old is None:
                os.environ.pop("GENTLE_SHELL_ARGS", None)
            else:
                os.environ["GENTLE_SHELL_ARGS"] = old
        self.assertEqual(child["GENTLE_SHELL_ARGS"], lvs.ISOLATED_SHELL_ARGS)
        self.assertNotIn("Users\\real", json.dumps(child))
        self.assertNotIn("--link", child["GENTLE_SHELL_ARGS"])

    def test_no_environment_values_are_printed(self) -> None:
        self.write_fake_exe()
        plan = lvs.prepare(self.root, sandbox_root=None, app_exe=None, pins=self.pins)
        printed = json.dumps({k: v for k, v in plan.items() if k != "env"})
        real_appdata = os.environ.get("APPDATA", "")
        if real_appdata:
            self.assertNotIn(real_appdata, printed)


class VoskEngine(Sandbox):
    def _vosk_fixture(self) -> None:
        voice = self.root / ".local" / "voice"
        venv_python = voice / "vosk" / "venv" / "Scripts" / "python.exe"
        venv_python.parent.mkdir(parents=True, exist_ok=True)
        venv_python.write_bytes(b"venv python")
        if os.name != "nt":
            venv_python = voice / "vosk" / "venv" / "bin" / "python"
            venv_python.parent.mkdir(parents=True, exist_ok=True)
            venv_python.write_bytes(b"venv python")
        model_dir = voice / "vosk" / "models" / "vosk-model-small-es-0.42"
        model_dir.mkdir(parents=True, exist_ok=True)
        (model_dir / "am" / "final.mdl").parent.mkdir(parents=True, exist_ok=True)
        (model_dir / "am" / "final.mdl").write_bytes(b"model")
        (model_dir / "conf").mkdir(parents=True, exist_ok=True)
        (model_dir / "conf" / "model.conf").write_bytes(b"a=v")
        manifest = {
            "schema": "gentle-shell-voice-vosk-setup/1",
            "python": str(venv_python),
            "model_dir": str(model_dir),
            # The real installer contract: a nested model record with the
            # documented tree algorithm. A flat model_sha256 is legacy and is
            # never accepted (it used to skip verification silently).
            "model": {"name": "vosk-model-small-es-0.42",
                      "sha256_local": lvs.vosk_model_digest(model_dir),
                      "digest_algorithm": lvs.VOSK_DIGEST_ALGORITHM},
        }
        (voice / "vosk" / "setup.json").write_text(json.dumps(manifest), encoding="utf-8")
        return str(venv_python), str(model_dir)

    def test_prepare_vosk_prefills_the_vosk_engine(self) -> None:
        self.write_fake_exe()
        python, model = self._vosk_fixture()
        plan = lvs.prepare(self.root, sandbox_root=None, app_exe=None,
                           pins=self.pins, engine="vosk")
        settings = json.loads(Path(plan["settings"]).read_text(encoding="utf-8"))
        local = settings["dictation"]["local"]
        self.assertEqual(local["engine"], "vosk")
        self.assertEqual(local["executable"], python)
        self.assertEqual(local["model"], model)

    def test_vosk_outside_the_workspace_is_refused(self) -> None:
        self.write_fake_exe()
        self._vosk_fixture()
        voice = self.root / ".local" / "voice"
        manifest_path = voice / "vosk" / "setup.json"
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        manifest["model_dir"] = self._tmp.name + "/outside/model"
        manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr), self.assertRaises(SystemExit):
            lvs.prepare(self.root, sandbox_root=None, app_exe=None,
                        pins=self.pins, engine="vosk")
        self.assertIn("vosk", stderr.getvalue().lower())

    def test_a_manifest_without_its_paths_is_refused_cleanly(self) -> None:
        self.write_fake_exe()
        python, model = self._vosk_fixture()
        manifest_path = self.root / ".local" / "voice" / "vosk" / "setup.json"
        for missing in ("python", "model_dir"):
            manifest = {
                "schema": "gentle-shell-voice-vosk-setup/1",
                "python": python,
                "model_dir": model,
                "model": {"sha256_local": "0" * 64,
                          "digest_algorithm": lvs.VOSK_DIGEST_ALGORITHM},
            }
            del manifest[missing]
            manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
            stderr = io.StringIO()
            with contextlib.redirect_stderr(stderr),                  self.assertRaises(SystemExit) as raised:
                lvs.prepare(self.root, sandbox_root=None, app_exe=None,
                            pins=self.pins, engine="vosk")
            self.assertEqual(raised.exception.code, 2)
            self.assertIn(f"no {missing} path", stderr.getvalue())
            self.assertNotIn("Traceback", stderr.getvalue())

    def test_a_tampered_nested_model_file_is_refused(self) -> None:
        self.write_fake_exe()
        _, model = self._vosk_fixture()
        Path(model, "am", "final.mdl").write_bytes(b"tampered")
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr), self.assertRaises(SystemExit):
            lvs.prepare(self.root, sandbox_root=None, app_exe=None,
                        pins=self.pins, engine="vosk")
        self.assertIn("digest", stderr.getvalue().lower())

    def test_a_missing_model_digest_is_refused_never_skipped(self) -> None:
        self.write_fake_exe()
        self._vosk_fixture()
        manifest_path = self.root / ".local" / "voice" / "vosk" / "setup.json"
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        manifest["model"] = {}
        manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr), self.assertRaises(SystemExit):
            lvs.prepare(self.root, sandbox_root=None, app_exe=None,
                        pins=self.pins, engine="vosk")
        self.assertIn("digest", stderr.getvalue().lower())

    def test_a_legacy_flat_model_digest_is_refused(self) -> None:
        # The installer writes model.sha256_local; a flat model_sha256 is a
        # different contract and used to skip integrity silently.
        self.write_fake_exe()
        self._vosk_fixture()
        manifest_path = self.root / ".local" / "voice" / "vosk" / "setup.json"
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        manifest.pop("model")
        manifest["model_sha256"] = "0" * 64
        manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr), self.assertRaises(SystemExit):
            lvs.prepare(self.root, sandbox_root=None, app_exe=None,
                        pins=self.pins, engine="vosk")
        self.assertIn("digest", stderr.getvalue().lower())

    def test_the_digest_recipe_matches_the_installer(self) -> None:
        # sha256-tree/1: files sorted by POSIX relative path, hashing
        # path NUL length NUL content - the same recipe as setup-vosk-local.py
        # and crates/app/src/features/dictation/setup.rs.
        _, model = self._vosk_fixture()
        directory = Path(model)
        import hashlib
        digest = hashlib.sha256()
        for relative in sorted(path.relative_to(directory).as_posix()
                               for path in directory.rglob("*") if path.is_file()):
            content = (directory / relative).read_bytes()
            digest.update(relative.encode("utf-8") + bytes([0])
                          + str(len(content)).encode("ascii") + bytes([0]) + content)
        self.assertEqual(lvs.vosk_model_digest(directory), digest.hexdigest())


if __name__ == "__main__":
    unittest.main(verbosity=2)
