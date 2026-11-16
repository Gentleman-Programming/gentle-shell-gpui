"""Deterministic, offline tests for scripts/setup-vosk-local.py.

No network: every download boundary is exercised through fixtures. Scratch
stays inside the project-local ignored tree ``.local/voice/tmp``.
Run with: python -B scripts/test_setup_vosk_local.py
"""

from __future__ import annotations

import contextlib
import hashlib
import importlib.util
import io
import json
import os
import subprocess
import sys
import tempfile
import unittest
import zipfile
from pathlib import Path
from unittest import mock

sys.dont_write_bytecode = True

_HERE = Path(__file__).resolve().parent
_SCRATCH_BASE = _HERE.parent / ".local" / "voice" / "tmp"
_SPEC = importlib.util.spec_from_file_location("setup_vosk_local", _HERE / "setup-vosk-local.py")
assert _SPEC is not None and _SPEC.loader is not None
svl = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(svl)


class SafeExtraction(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory(dir=_SCRATCH_BASE)
        self.dir = Path(self._tmp.name)

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def _zip(self, members) -> Path:
        path = self.dir / "model.zip"
        with zipfile.ZipFile(path, "w") as archive:
            for name, payload in members:
                archive.writestr(name, payload)
        return path

    def test_a_normal_model_archive_extracts(self) -> None:
        zip_path = self._zip([
            (svl.MODEL_NAME + "/conf/model.conf", b"a=v"),
            (svl.MODEL_NAME + "/am/final.mdl", b"weights"),
        ])
        svl.safe_extract_zip(zip_path, self.dir, svl.MODEL_NAME)
        self.assertTrue((self.dir / svl.MODEL_NAME / "am" / "final.mdl").is_file())

    def test_traversal_is_refused_before_any_write(self) -> None:
        zip_path = self._zip([("bad/../../escape.bin", b"x")])
        with self.assertRaises(SystemExit):
            svl.safe_extract_zip(zip_path, self.dir, "anything")
        self.assertEqual(list(self.dir.iterdir()), [zip_path], "nothing extracted")

    def test_a_wrong_prefix_is_refused(self) -> None:
        zip_path = self._zip([("other-model/conf", b"")])
        with self.assertRaises(SystemExit):
            svl.safe_extract_zip(zip_path, self.dir, svl.MODEL_NAME)


class PinSelection(unittest.TestCase):
    def test_the_cffi_wheel_matches_the_interpreter_abi(self) -> None:
        abi = f"cp{sys.version_info.major}{sys.version_info.minor}"
        names = [name for name, _ in svl.pick_pins(abi)]
        if abi in svl.CFFI_WHEELS:
            self.assertTrue(any(name.startswith("cffi-") for name in names),
                            f"cffi wheel for {abi} missing from {names}")
        self.assertTrue(any(name.startswith("vosk-") for name in names))
        self.assertTrue(any(name.startswith("requests-") for name in names))

    def test_an_unknown_abi_is_refused_not_guessed(self) -> None:
        with self.assertRaises(SystemExit):
            svl.pick_pins("cp999")


class ModelIntegrity(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory(dir=_SCRATCH_BASE)
        self.dir = Path(self._tmp.name)
        self.model = self.dir / svl.MODEL_NAME
        (self.model / "am").mkdir(parents=True)
        (self.model / "am" / "final.mdl").write_bytes(b"weights")

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def test_the_local_digest_covers_the_whole_model_directory(self) -> None:
        first = svl._sha256_dir(self.model)
        self.assertEqual(first, svl._sha256_dir(self.model), "stable over a re-walk")
        (self.model / "conf").mkdir()
        (self.model / "conf" / "model.conf").write_bytes(b"a=v")
        self.assertNotEqual(first, svl._sha256_dir(self.model),
                            "a changed file changes the LOCAL digest")

    def test_the_digest_is_the_documented_sha256_tree_recipe(self) -> None:
        # sha256-tree/1: files sorted by POSIX relative path, each hashing
        # path NUL length NUL content. The app and the launcher mirror this.
        digest = hashlib.sha256()
        for relative in ("am/final.mdl",):
            content = (self.model / relative).read_bytes()
            digest.update(relative.encode("utf-8") + b"\0"
                          + str(len(content)).encode("ascii") + b"\0" + content)
        self.assertEqual(svl.tree_digest(self.model), digest.hexdigest())

    def test_a_link_inside_the_model_is_refused(self) -> None:
        (self.model / "conf").mkdir()
        link = self.model / "conf" / "link"
        if not make_junction(link, self.model / "am"):
            self.skipTest("this platform does not let the test create a junction")
        self.addCleanup(remove_junction, link)
        with self.assertRaises(SystemExit):
            svl.tree_digest(self.model)


def make_junction(link: Path, target: Path) -> bool:
    """Create a symlink, or (on Windows, without privileges) a junction."""
    try:
        os.symlink(target, link, target_is_directory=True)
        return True
    except (OSError, AttributeError):
        pass
    if sys.platform.startswith("win"):
        made = subprocess.run(["cmd", "/c", "mklink", "/J", str(link), str(target)],
                              capture_output=True)
        return made.returncode == 0
    return False


def remove_junction(link: Path) -> None:
    if not os.path.lexists(link):
        return
    try:
        os.unlink(link)
    except OSError:
        os.rmdir(link)


class RootGuards(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory(dir=_SCRATCH_BASE)
        self.repo = Path(self._tmp.name)
        (self.repo / "odd" / "tasks").mkdir(parents=True)
        (self.repo / "Cargo.toml").write_text("[workspace]\n", encoding="utf-8")

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def test_the_c_drive_is_refused_before_any_mutation(self) -> None:
        vosk_root = self.repo / ".local" / "voice" / "vosk"
        with self.assertRaises(SystemExit):
            svl.validate_root(repo_root=self.repo, vosk_root=vosk_root,
                              drive_guard=lambda path: "C:")
        self.assertFalse(vosk_root.exists(), "a refusal creates nothing")

    def test_a_checkout_without_the_repo_shape_is_refused(self) -> None:
        empty = self.repo / "not-a-checkout"
        empty.mkdir()
        with self.assertRaises(SystemExit):
            svl.validate_root(repo_root=empty,
                              vosk_root=empty / ".local" / "voice" / "vosk",
                              drive_guard=lambda path: None)

    def test_a_descendant_junction_is_refused_before_mutation(self) -> None:
        vosk_root = self.repo / ".local" / "voice" / "vosk"
        (vosk_root / "downloads").mkdir(parents=True)
        link = vosk_root / "models"
        if not make_junction(link, self.repo):
            self.skipTest("this platform does not let the test create a junction")
        self.addCleanup(remove_junction, link)
        with self.assertRaises(SystemExit):
            svl.validate_root(repo_root=self.repo, vosk_root=vosk_root,
                              drive_guard=lambda path: None)


class DownloadSafety(unittest.TestCase):
    def test_a_non_https_url_is_refused(self) -> None:
        with self.assertRaises(svl.SetupError):
            svl.check_url_allowed("http://pypi.org/x")

    def test_an_unlisted_host_is_refused(self) -> None:
        with self.assertRaises(svl.SetupError):
            svl.check_url_allowed("https://example.com/x")

    def test_credentials_in_a_url_are_refused(self) -> None:
        with self.assertRaises(svl.SetupError):
            svl.check_url_allowed("https://user:pass@pypi.org/x")

    def test_download_refuses_to_overwrite_an_existing_file(self) -> None:
        with tempfile.TemporaryDirectory(dir=_SCRATCH_BASE) as name:
            target = Path(name) / "artifact.whl"
            target.write_bytes(b"mine")
            with self.assertRaises(svl.SetupError):
                svl.download("https://files.pythonhosted.org/pkg.whl", target)
            self.assertEqual(target.read_bytes(), b"mine", "nothing replaced")


class DependencyResolution(unittest.TestCase):
    """Transitive dependencies are resolved AND pinned, never assumed."""

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory(dir=_SCRATCH_BASE)
        self.dir = Path(self._tmp.name)

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def _wheel(self, name: str, version: str, requires=()) -> Path:
        path = self.dir / f"{name}-{version}-py3-none-any.whl"
        metadata = "Metadata-Version: 2.1\nName: " + name + "\nVersion: " + version + "\n"
        metadata += "".join(f"Requires-Dist: {line}\n" for line in requires)
        with zipfile.ZipFile(path, "w") as archive:
            archive.writestr(f"{name}-{version}.dist-info/METADATA", metadata)
        return path

    def test_a_wheel_declares_its_runtime_dependencies(self) -> None:
        wheel = self._wheel("vosk", "0.3.45", [
            "requests", "cffi>=1.0", "PySocks!=1.5.7,>=1.5.6; extra == 'socks'"])
        declared = svl.declared_requirements(wheel)
        self.assertIn(("requests", ""), declared)
        self.assertIn(("cffi", ">=1.0"), declared)
        self.assertFalse([item for item in declared if item[0] == "PySocks"],
                         "extras are optional and never installed")

    def test_a_transitive_dependency_is_pinned_and_downloaded(self) -> None:
        wheel = self._wheel("vosk", "0.3.45", ["idna<4,>=2.5"])
        idna = self._wheel("idna", "3.11")
        payload = idna.read_bytes()
        digest = hashlib.sha256(payload).hexdigest()
        metadata = {"info": {"version": "3.11"}, "releases": {
            "3.11": [{"filename": "idna-3.11-py3-none-any.whl",
                      "url": "https://files.pythonhosted.org/packages/idna.whl",
                      "digests": {"sha256": digest}}]}}
        written: list[Path] = []

        def fake_download(url: str, target: Path, max_bytes: int = 0) -> None:
            written.append(target)
            target.write_bytes(payload)

        downloads = self.dir / "downloads"
        downloads.mkdir()
        with mock.patch.object(svl, "download", fake_download):
            resolved, records = svl.resolve_artifacts(
                downloads, [wheel], fetch=lambda url: metadata)
        self.assertEqual([path.name for path in resolved], ["idna-3.11-py3-none-any.whl"])
        self.assertEqual(records, [{"file": "idna-3.11-py3-none-any.whl",
                                    "sha256": digest, "version": "3.11"}])
        self.assertEqual(written, [downloads / "idna-3.11-py3-none-any.whl"])

    def test_a_dependency_without_a_pure_wheel_is_refused_not_guessed(self) -> None:
        wheel = self._wheel("vosk", "0.3.45", ["native-thing"])
        metadata = {"info": {"version": "1.0"}, "releases": {"1.0": [
            {"filename": "native_thing-1.0-cp312-cp312-win_amd64.whl",
             "url": "https://files.pythonhosted.org/packages/native.whl",
             "digests": {"sha256": "00"}}]}}
        with self.assertRaises(svl.SetupError):
            svl.resolve_artifacts(self.dir, [wheel], fetch=lambda url: metadata)

    def test_a_prerelease_never_wins_over_the_stable_release(self) -> None:
        """The real failure: tqdm needs colorama, PyPI's latest is
        ``0.4.6rc1`` and the resolver picked it, then refused because that
        prerelease publishes no universal wheel. The stable release is the
        standards-compliant answer."""
        wheel = self._wheel("vosk", "0.3.45", ["colorama"])
        # A real (minimal) wheel: the resolver reads its METADATA afterwards.
        payload = self._wheel("colorama", "0.4.6").read_bytes()
        stable_digest = hashlib.sha256(payload).hexdigest()
        metadata = {
            "info": {"version": "0.4.6rc1"},
            "releases": {
                "0.4.6rc1": [{
                    "filename": "colorama-0.4.6rc1-py3-none-any.whl",
                    "url": "https://files.pythonhosted.org/packages/colorama-rc.whl",
                    "digests": {"sha256": "a" * 64},
                    "requires_python": ">=2.7"}],
                "0.4.6": [{
                    "filename": "colorama-0.4.6-py2.py3-none-any.whl",
                    "url": "https://files.pythonhosted.org/packages/colorama.whl",
                    "digests": {"sha256": stable_digest},
                    "requires_python": ">=2.7, !=3.0.*, !=3.1.*, !=3.2.*, !=3.3.*, !=3.4.*"}],
            }}
        written: list[Path] = []

        def fake_download(url: str, target: Path, max_bytes: int = 0) -> None:
            written.append(target)
            target.write_bytes(payload)

        downloads = self.dir / "downloads"
        downloads.mkdir()
        with mock.patch.object(svl, "download", fake_download):
            resolved, records = svl.resolve_artifacts(
                downloads, [wheel], fetch=lambda url: metadata)
        self.assertEqual([path.name for path in resolved],
                         ["colorama-0.4.6-py2.py3-none-any.whl"])
        self.assertEqual(records[0]["version"], "0.4.6")
        self.assertEqual(records[0]["sha256"], stable_digest)
        self.assertEqual(written, [downloads / "colorama-0.4.6-py2.py3-none-any.whl"])

    def test_a_dependency_with_only_prereleases_is_refused(self) -> None:
        wheel = self._wheel("vosk", "0.3.45", ["srt"])
        metadata = {"info": {"version": "3.6.0b1"}, "releases": {"3.6.0b1": [{
            "filename": "srt-3.6.0b1-py3-none-any.whl",
            "url": "https://files.pythonhosted.org/packages/srt.whl",
            "digests": {"sha256": "c" * 64}}]}}
        with mock.patch.object(svl, "download") as download,              self.assertRaises(svl.SetupError) as raised:
            svl.resolve_artifacts(self.dir, [wheel], fetch=lambda url: metadata)
        download.assert_not_called()
        self.assertIn("prerelease", str(raised.exception))
        self.assertIn("srt", str(raised.exception))

    def test_a_wheel_for_another_python_is_refused_not_substituted(self) -> None:
        wheel = self._wheel("vosk", "0.3.45", ["futurelib"])
        metadata = {"info": {"version": "1.0"}, "releases": {"1.0": [{
            "filename": "futurelib-1.0-py3-none-any.whl",
            "url": "https://files.pythonhosted.org/packages/futurelib.whl",
            "digests": {"sha256": "d" * 64},
            "requires_python": ">=3.99"}]}}
        with mock.patch.object(svl, "download") as download,              self.assertRaises(svl.SetupError) as raised:
            svl.resolve_artifacts(self.dir, [wheel], fetch=lambda url: metadata)
        download.assert_not_called()
        self.assertIn("Python", str(raised.exception))
        self.assertIn("futurelib", str(raised.exception))

    def test_a_published_digest_change_is_refused(self) -> None:
        wheel = self._wheel("vosk", "0.3.45", ["idna"])
        metadata = {"info": {"version": "3.11"}, "releases": {"3.11": [
            {"filename": "idna-3.11-py3-none-any.whl",
             "url": "https://files.pythonhosted.org/packages/idna.whl",
             "digests": {"sha256": "f" * 64}}]}}
        with self.assertRaises(svl.SetupError):
            svl.resolve_artifacts(self.dir, [wheel], fetch=lambda url: metadata)


class CheckIsReadOnly(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory(dir=_SCRATCH_BASE)
        self.repo = Path(self._tmp.name)
        (self.repo / "odd" / "tasks").mkdir(parents=True)
        (self.repo / "Cargo.toml").write_text("[workspace]\n", encoding="utf-8")
        self.vosk = self.repo / ".local" / "voice" / "vosk"

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def test_check_creates_nothing_and_downloads_nothing(self) -> None:
        def explode(*args, **kwargs):
            raise AssertionError("a read-only check must not touch the network")

        captured = io.StringIO()
        with mock.patch.object(svl, "_ROOT", self.repo), \
             mock.patch.object(svl, "download", explode), \
             mock.patch.object(svl, "_fetch_json", explode), \
             mock.patch.object(sys, "stdout", captured):
            with self.assertRaises(SystemExit) as raised:
                svl.prepare(check=True, vosk_root=self.vosk)
        self.assertEqual(raised.exception.code, 1)
        report = json.loads(captured.getvalue())
        self.assertFalse(report["ok"])
        self.assertIn("no setup.json manifest", report["reasons"])
        self.assertFalse(self.vosk.exists(), "--check created the data tree")

    def test_check_reports_a_prepared_tree_as_ready(self) -> None:
        model = self.vosk / "models" / svl.MODEL_NAME
        (model / "am").mkdir(parents=True)
        (model / "am" / "final.mdl").write_bytes(b"weights")
        venv_python = svl._venv_python(self.vosk / "venv")
        venv_python.parent.mkdir(parents=True)
        venv_python.write_bytes(b"py")
        manifest = {
            "schema": svl._SCHEMA,
            "python": str(venv_python),
            "model_dir": str(model),
            "model": {"sha256_local": svl.tree_digest(model),
                      "digest_algorithm": svl.DIGEST_ALGORITHM},
        }
        (self.vosk / "setup.json").write_text(json.dumps(manifest), encoding="utf-8")
        captured = io.StringIO()
        with mock.patch.object(svl, "_ROOT", self.repo), \
             mock.patch.object(sys, "stdout", captured):
            svl.prepare(check=True, vosk_root=self.vosk)
        report = json.loads(captured.getvalue())
        self.assertTrue(report["ok"], report["reasons"])
        self.assertEqual(report["model_sha256"], manifest["model"]["sha256_local"])

    def test_check_reports_a_tampered_model_as_not_ready(self) -> None:
        model = self.vosk / "models" / svl.MODEL_NAME
        (model / "am").mkdir(parents=True)
        (model / "am" / "final.mdl").write_bytes(b"weights")
        venv_python = svl._venv_python(self.vosk / "venv")
        venv_python.parent.mkdir(parents=True)
        venv_python.write_bytes(b"py")
        manifest = {
            "schema": svl._SCHEMA,
            "python": str(venv_python),
            "model_dir": str(model),
            "model": {"sha256_local": svl.tree_digest(model),
                      "digest_algorithm": svl.DIGEST_ALGORITHM},
        }
        (self.vosk / "setup.json").write_text(json.dumps(manifest), encoding="utf-8")
        (model / "am" / "final.mdl").write_bytes(b"tampered")
        captured = io.StringIO()
        with mock.patch.object(svl, "_ROOT", self.repo), \
             mock.patch.object(sys, "stdout", captured):
            with self.assertRaises(SystemExit) as raised:
                svl.prepare(check=True, vosk_root=self.vosk)
        self.assertEqual(raised.exception.code, 1)
        report = json.loads(captured.getvalue())
        self.assertIn("the model does not match its recorded digest", report["reasons"])
        self.assertEqual((model / "am" / "final.mdl").read_bytes(), b"tampered",
                         "a refusal leaves the artifacts untouched")


class OverwriteGuards(unittest.TestCase):
    """Nothing the installer did not write is ever replaced."""

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory(dir=_SCRATCH_BASE)
        self.root = Path(self._tmp.name)

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def test_extraction_never_overwrites_an_existing_member(self) -> None:
        target = self.root / "models"
        member = target / svl.MODEL_NAME / "am" / "final.mdl"
        member.parent.mkdir(parents=True)
        member.write_bytes(b"mine")
        archive = self.root / "model.zip"
        with zipfile.ZipFile(archive, "w") as zipped:
            zipped.writestr(svl.MODEL_NAME + "/am/final.mdl", b"theirs")
        with self.assertRaises(SystemExit) as raised:
            svl.safe_extract_zip(archive, target, svl.MODEL_NAME)
        self.assertEqual(raised.exception.code, 2)
        self.assertEqual(member.read_bytes(), b"mine")

    def test_a_setup_error_is_reported_as_a_refusal_not_a_traceback(self) -> None:
        stderr = io.StringIO()
        with mock.patch.object(svl, "prepare",
                               side_effect=svl.SetupError("refusing to overwrite")),              mock.patch.object(sys, "argv", ["setup-vosk-local.py"]),              contextlib.redirect_stderr(stderr),              self.assertRaises(SystemExit) as raised:
            svl.main()
        self.assertEqual(raised.exception.code, 2)
        self.assertIn("refusing to overwrite", stderr.getvalue())


class PrepareEndToEnd(unittest.TestCase):
    """The whole flow with every network boundary faked: the model lands at
    models/<MODEL_NAME>/ (never nested), every artifact is installed from an
    absolute verified path, and the manifest records the tree digest."""

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory(dir=_SCRATCH_BASE)
        self.repo = Path(self._tmp.name)
        (self.repo / "odd" / "tasks").mkdir(parents=True)
        (self.repo / "Cargo.toml").write_text("[workspace]\n", encoding="utf-8")
        self.vosk = self.repo / ".local" / "voice" / "vosk"
        self.calls: list[list[str]] = []

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def _wheel(self, name: str, version: str, requires=()) -> bytes:
        buffer = io.BytesIO()
        metadata = "Metadata-Version: 2.1\nName: " + name + "\nVersion: " + version + "\n"
        metadata += "".join(f"Requires-Dist: {line}\n" for line in requires)
        with zipfile.ZipFile(buffer, "w") as archive:
            archive.writestr(f"{name}-{version}.dist-info/METADATA", metadata)
        return buffer.getvalue()

    def _model_zip(self) -> bytes:
        buffer = io.BytesIO()
        with zipfile.ZipFile(buffer, "w") as archive:
            archive.writestr(svl.MODEL_NAME + "/conf/model.conf", b"a=v")
            archive.writestr(svl.MODEL_NAME + "/am/final.mdl", b"weights")
        return buffer.getvalue()

    def test_prepare_installs_and_records_a_verified_tree(self) -> None:
        vosk_wheel = self._wheel("vosk", "0.3.45", ["idna<4,>=2.5"])
        cffi_wheel = self._wheel("cffi", "2.1.1")
        setuptools_wheel = self._wheel("setuptools", "84.2.1")
        idna_wheel = self._wheel("idna", "3.11")
        model_zip = self._model_zip()
        payloads = {
            vosk_wheel: "vosk-0.3.45-py3-none-any.whl",
            cffi_wheel: "cffi-2.1.1-cp312-cp312-win_amd64.whl",
            setuptools_wheel: "setuptools-84.2.1-py3-none-any.whl",
        }
        pins = {name: hashlib.sha256(bytes(body)).hexdigest()
                for body, name in payloads.items()}
        idna_sha = hashlib.sha256(idna_wheel).hexdigest()
        abi = f"cp{sys.version_info.major}{sys.version_info.minor}"
        idna_meta = {"info": {"version": "3.11"}, "releases": {"3.11": [
            {"filename": "idna-3.11-py3-none-any.whl",
             "url": "https://files.pythonhosted.org/packages/idna.whl",
             "digests": {"sha256": idna_sha}}]}}
        published = [
            {"filename": name,
             "url": f"https://files.pythonhosted.org/packages/{name}",
             "digests": {"sha256": digest}}
            for name, digest in pins.items()] + [
            {"filename": "idna-3.11-py3-none-any.whl",
             "url": "https://files.pythonhosted.org/packages/idna-3.11-py3-none-any.whl",
             "digests": {"sha256": idna_sha}}]
        index = {"info": {"version": "3.11"}, "urls": published,
                 "releases": idna_meta["releases"]}

        def fake_download(url: str, target: Path, max_bytes: int = 0) -> None:
            self.assertTrue(Path(url).name or True)
            if url == svl.MODEL_URL:
                target.write_bytes(model_zip)
                return
            if target.name == "idna-3.11-py3-none-any.whl":
                target.write_bytes(idna_wheel)
                return
            for body, name in payloads.items():
                if name == target.name:
                    target.write_bytes(bytes(body))
                    return
            raise AssertionError(f"unexpected download {url}")

        def fake_run(command, check=False, env=None):
            self.calls.append([str(part) for part in command])
            return mock.Mock(returncode=0)

        venv_python = svl._venv_python(self.vosk / "venv")
        venv_python.parent.mkdir(parents=True)
        venv_python.write_bytes(b"py")
        with mock.patch.object(svl, "_ROOT", self.repo), \
             mock.patch.object(svl, "PINS", pins), \
             mock.patch.object(svl, "CFFI_WHEELS", {abi: (
                 "cffi-2.1.1-cp312-cp312-win_amd64.whl",
                 pins["cffi-2.1.1-cp312-cp312-win_amd64.whl"])}), \
             mock.patch.object(svl, "download", fake_download), \
             mock.patch.object(svl, "_fetch_json", lambda url: index), \
             mock.patch.object(svl.subprocess, "run", fake_run):
            svl.prepare(check=False, vosk_root=self.vosk)

        model = self.vosk / "models" / svl.MODEL_NAME
        self.assertTrue((model / "am" / "final.mdl").is_file())
        self.assertFalse((model / svl.MODEL_NAME).exists(),
                         "the model must not be nested twice")
        manifest = json.loads((self.vosk / "setup.json").read_text(encoding="utf-8"))
        self.assertEqual(manifest["model"]["sha256_local"], svl.tree_digest(model))
        self.assertEqual(manifest["model"]["digest_algorithm"], svl.DIGEST_ALGORITHM)
        self.assertEqual([entry["file"] for entry in
                          manifest["python_wheels"]["transitive"]],
                         ["idna-3.11-py3-none-any.whl"])
        installs = [call for call in self.calls if "install" in call]
        self.assertTrue(all("--no-deps" in call for call in installs))
        self.assertTrue(all(any(part.endswith(".whl") or part.endswith(".tar.gz")
                                for part in call) for call in installs))
        self.assertTrue(all(any(Path(part).is_absolute() and part.endswith((".whl", ".tar.gz"))
                                for part in call) for call in installs),
                        "pip receives absolute, verified artifact paths")
        self.assertEqual(installs[0][-1].split(os.sep)[-1],
                         "setuptools-84.2.1-py3-none-any.whl",
                         "setuptools goes first so the sdist can build")
        self.assertFalse(any("requirements.txt" in part for call in installs
                             for part in call),
                         "no relative requirements file is used")


if __name__ == "__main__":
    unittest.main(verbosity=2)
