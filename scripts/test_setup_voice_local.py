"""Deterministic, no-network tests for scripts/setup-voice-local.py.

Every network and filesystem boundary the installer touches is injected, so
these tests never reach the network. Test scratch stays inside the project-local
ignored tree ``.local/voice/tmp`` (never the system/temp drive) and is removed
by the tests themselves. Run with:  python -B scripts/test_setup_voice_local.py
"""

from __future__ import annotations

import email.message
import hashlib
import importlib.util
import io
import json
import os
import secrets
import subprocess
import sys
import unittest
import urllib.request
import zipfile
from pathlib import Path, PureWindowsPath
from unittest import mock

sys.dont_write_bytecode = True

_HERE = Path(__file__).resolve().parent
_REAL_ROOT = _HERE.parent
_SCRATCH_BASE = _REAL_ROOT / ".local" / "voice" / "tmp"
_SPEC = importlib.util.spec_from_file_location(
    "setup_voice_local", _HERE / "setup-voice-local.py"
)
assert _SPEC is not None and _SPEC.loader is not None
svl = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(svl)


def _zip_bytes(build):
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w") as zf:
        build(zf)
    return buf.getvalue()


ENGINE_BYTES = _zip_bytes(lambda zf: (
    zf.writestr("whisper-cli.exe", b"MZ" + b"fake cli" * 64),
    zf.writestr("whisper.dll", b"PK\x03\x04-dll-ish"),
    zf.writestr("sub/whisper-server.exe", b"mz"),
))
MODEL_BYTES = b"ggml fake multilingual model payload" * 512
LICENSE_TEXT = "MIT License\n\nCopyright (c) The ggml authors (fetched text for test)\n"

ENGINE_SHA = svl.sha256_hex(io.BytesIO(ENGINE_BYTES))
MODEL_SHA = svl.sha256_hex(io.BytesIO(MODEL_BYTES))

RELEASES_JSON = [
    {
        "id": 501,
        "tag_name": "vFakeLatest",
        "draft": False,
        "prerelease": False,
        "created_at": "2026-11-01T00:00:00Z",
        "assets": [],
    },
    {
        "id": 500,
        "tag_name": "bFake1",
        "draft": False,
        "prerelease": True,
        "created_at": "2026-10-31T00:00:00Z",
        "assets": [
            {
                "id": 42,
                "name": "whisper-bin-x64.zip",
                "size": len(ENGINE_BYTES),
                "digest": "sha256:" + ENGINE_SHA,
                "browser_download_url": "https://github.com/dl/whisper-bin-x64.zip",
            }
        ],
    },
    {
        "id": 499,
        "tag_name": "bFake0",
        "draft": False,
        "prerelease": False,
        "created_at": "2026-09-01T00:00:00Z",
        "assets": [
            {
                "id": 41,
                "name": "whisper-bin-x64.zip",
                "size": 999,
                "digest": "sha256:" + "0" * 64,
                "browser_download_url": "https://github.com/dl/old.zip",
            }
        ],
    },
]
MODEL_META = {"sha": "5359" + "a" * 36}
TREE_JSON = [
    {"type": "file", "path": "ggml-base.en.bin", "size": 10, "lfs": {"oid": "a" * 64, "size": 10}},
    {
        "type": "file",
        "path": "ggml-base.bin",
        "size": len(MODEL_BYTES),
        "lfs": {"oid": MODEL_SHA, "size": len(MODEL_BYTES)},
    },
]


class FakeNet:
    """Injected stand-in for every remote call. Counts downloads; zero network."""

    def __init__(self, *, bad_engine_sha=False, wrong_size=False):
        self.bad_engine_sha = bad_engine_sha
        self.wrong_size = wrong_size
        self.streams = 0
        self.gets = 0

    def fetch_json(self, url):
        self.gets += 1
        if "releases" in url:
            return RELEASES_JSON
        if url.endswith("/api/models/ggerganov/whisper.cpp"):
            return MODEL_META
        if "/tree/" in url:
            return TREE_JSON
        raise AssertionError("unexpected json url " + url)

    def stream_opener(self, url):
        self.streams += 1
        if "whisper-bin-x64.zip" in url:
            data = bytearray(ENGINE_BYTES)
            if self.bad_engine_sha:
                data[-1] ^= 0xFF
            if self.wrong_size:
                data.extend(b"extra")
            return io.BytesIO(bytes(data))
        if "ggml-base.bin" in url:
            return io.BytesIO(MODEL_BYTES)
        if "LICENSE" in url:
            return io.BytesIO(LICENSE_TEXT.encode("utf-8"))
        raise AssertionError("unexpected stream url " + url)


class Base(unittest.TestCase):
    def setUp(self):
        # scratch stays on the project drive, inside the ignored setup tree
        self.tmp = _SCRATCH_BASE / ("test-" + secrets.token_hex(6))
        self.tmp.mkdir(parents=True)
        self.addCleanup(svl.rmtree_strict, self.tmp)
        self.root = self.tmp / "repo"
        (self.root / "odd" / "tasks").mkdir(parents=True)
        (self.root / "Cargo.toml").write_text("[workspace]\n")
        (self.root / "odd" / "tasks" / "voice-dictation.md").write_text("stub\n")

    def run_setup(self, net, **kw):
        kw.setdefault("validate", lambda r, k: None)  # scratch is a fake checkout
        kw.setdefault("log", lambda *a: None)
        return svl.run(repo_root=self.root, fetch_json=net.fetch_json,
                       stream_opener=net.stream_opener, **kw)

    def verify(self, net, **kw):
        kw.setdefault("validate", lambda r, k: None)
        return svl.verify_only(self.root, svl.make_resolver(net.fetch_json), **kw)


class ScratchLocation(Base):
    def test_scratch_never_uses_the_temp_dir_drive(self):
        self.assertTrue(self.tmp.is_relative_to(_REAL_ROOT))
        self.assertTrue(self.tmp.is_relative_to(_REAL_ROOT / ".local" / "voice"))
        self.assertEqual(PureWindowsPath(self.tmp.resolve()).drive.upper(),
                         PureWindowsPath(_REAL_ROOT).drive.upper())


class NoDirectoryRename(Base):
    """The installer must publish by extraction into a unique directory plus
    atomic FILE promotion only; a directory rename is lock-fragile on Windows
    (WinError 5 under an external holder) and must never be attempted."""

    def test_install_succeeds_without_any_directory_rename(self):
        net = FakeNet()
        with mock.patch("os.rename",
                        side_effect=AssertionError("directory rename attempted")):
            result = self.run_setup(net)
        layout = svl.layout(self.root)
        manifest = json.loads((layout["root"] / "setup.json")
                              .read_text(encoding="utf-8"))
        engine_dir = Path(manifest["engine"]["dir"])
        # unique nonce-style destination inside the engine root, not <tag>
        self.assertTrue(engine_dir.is_relative_to(layout["engine"]))
        self.assertNotEqual(engine_dir.name, result["engine"]["tag"])
        self.assertIn("-", engine_dir.name)
        # owner marker with the verified ZIP hash
        marker = json.loads((engine_dir / svl.SETUP_MARKER_NAME)
                            .read_text(encoding="utf-8"))
        self.assertEqual(marker["zip_sha256"], ENGINE_SHA)
        self.assertEqual(marker["state"], "complete")
        # exact binary + model hashes and published manifest paths
        exe = next(b for b in manifest["engine"]["binaries"]
                   if b["path"].endswith("whisper-cli.exe"))
        self.assertEqual(exe["sha256"],
                         hashlib.sha256(b"MZ" + b"fake cli" * 64).hexdigest())
        self.assertEqual(manifest["model"]["sha256"], MODEL_SHA)
        self.assertEqual(manifest["model"]["file"],
                         str(layout["model_bin"]))
        self.assertTrue((engine_dir / "whisper-cli.exe").is_file())
        self.assertTrue(layout["model_bin"].is_file())

    def test_extraction_failure_preserves_incomplete_dir_no_manifest(self):
        unrelated = svl.layout(self.root)["engine"] / "unrelated-dir"
        unrelated.mkdir(parents=True)
        (unrelated / "owners-file.txt").write_text("keep")
        def boom(zip_path, dest_dir):
            raise svl.SetupError("simulated extraction failure")
        with mock.patch.object(svl, "safe_extract", boom):
            with self.assertRaises(svl.SetupError):
                self.run_setup(FakeNet())
        layout = svl.layout(self.root)
        self.assertFalse((layout["root"] / "setup.json").exists())
        self.assertFalse(layout["model_bin"].exists())
        dirs = [p for p in layout["engine"].iterdir() if p.is_dir()]
        self.assertIn(unrelated.name, [d.name for d in dirs])
        owned = [d for d in dirs if d.name != unrelated.name]
        self.assertEqual(len(owned), 1)
        marker = json.loads((owned[0] / svl.SETUP_MARKER_NAME)
                            .read_text(encoding="utf-8"))
        self.assertEqual(marker["state"], "incomplete")
        self.assertEqual(marker["zip_sha256"], ENGINE_SHA)

    def test_reuse_follows_manifest_destination_not_assumed_tag(self):
        self.run_setup(FakeNet())
        layout = svl.layout(self.root)
        first = next(d for d in layout["engine"].iterdir()
                      if d.name.startswith("bFake1-"))
        moved = layout["engine"] / "custom-name"
        first.rename(moved)  # test-side relocation of OUR OWN scratch dir
        mf = json.loads((layout["root"] / "setup.json").read_text("utf-8"))
        mf["engine"]["dir"] = str(moved)
        (layout["root"] / "setup.json").write_text(
            json.dumps(mf), encoding="utf-8")
        with mock.patch("os.rename",
                        side_effect=AssertionError("directory rename attempted")):
            result = self.run_setup(FakeNet())
        self.assertEqual(Path(result["engine"]["dir"]), moved)
        dirs = [p.name for p in layout["engine"].iterdir() if p.is_dir()]
        self.assertEqual(dirs, ["custom-name"],
                         "reuse must not mint a duplicate directory")

    def test_check_rejects_manifest_destination_outside_engine_root(self):
        self.run_setup(FakeNet())
        layout = svl.layout(self.root)
        mf_path = layout["root"] / "setup.json"
        mf = json.loads(mf_path.read_text("utf-8"))
        outside = self.tmp / "outside-engine"
        Path(mf["engine"]["dir"]).rename(outside)  # test-side relocation
        mf["engine"]["dir"] = str(outside)
        mf_path.write_text(json.dumps(mf), encoding="utf-8")
        with self.assertRaises(svl.SetupError) as cm:
            self.verify(FakeNet())
        self.assertIn("contained", str(cm.exception))


class RedirectGuard(Base):
    """No network: a fake redirect chain records every URL actually opened."""

    class FakeResponse:
        def __init__(self, url, code, headers, body=b""):
            self.url, self.code, self.msg = url, code, "fake"
            self._headers = email.message.Message()
            for k, v in headers.items():
                self._headers[k] = v
            self._body = body

        def info(self):
            return self._headers

        def geturl(self):
            return self.url

        def getcode(self):
            return self.code

        def read(self):
            return self._body

        def close(self):
            pass

    def build(self, routes):
        opened = []

        class FakeHandler(urllib.request.BaseHandler):
            def https_open(self, req):
                opened.append(req.full_url)
                if req.full_url not in routes:
                    raise AssertionError(f"forbidden target opened: {req.full_url}")
                code, headers, body = routes[req.full_url]
                return RedirectGuard.FakeResponse(req.full_url, code, headers, body)
            http_open = https_open

        director = urllib.request.OpenerDirector()
        director.add_handler(FakeHandler())
        director.add_handler(urllib.request.HTTPErrorProcessor())
        director.add_handler(svl.SafeRedirectHandler())
        return director, opened

    def test_forbidden_redirect_host_never_opened(self):
        opener, opened = self.build({
            "https://huggingface.co/a":
                (302, {"Location": "https://evil.example.com/b?token=SECRET"},
                 b""),
        })
        with self.assertRaises(svl.SetupError) as cm:
            opener.open("https://huggingface.co/a")
        self.assertEqual(opened, ["https://huggingface.co/a"],
                         "the redirect target must never be opened")
        self.assertIn("evil.example.com", str(cm.exception))
        self.assertNotIn("SECRET", str(cm.exception))
        self.assertNotIn("?", str(cm.exception),
                         "no signed query/token may leak into errors")

    def test_observed_hf_cdn_host_allowed_evil_lookalike_rejected(self):
        self.assertIn("us.aws.cdn.hf.co", svl.ALLOWED_HOSTS)
        cdn_url = "https://us.aws.cdn.hf.co/xet-bridge-us/ggml-base.bin/resolve"
        opener, opened = self.build({
            "https://huggingface.co/m": (302, {
                "Location": cdn_url + "?token=SECRET"}, b""),
            cdn_url + "?token=SECRET": (200, {}, b"model data"),
        })
        self.assertEqual(
            opener.open("https://huggingface.co/m").read(), b"model data")
        self.assertEqual(set(opened), {"https://huggingface.co/m",
                                       cdn_url + "?token=SECRET"})
        # lookalike super-domain is NOT allowlisted and is never opened
        opener2, opened2 = self.build({})
        with self.assertRaises(svl.SetupError) as cm:
            opener2.open("https://us.aws.cdn.hf.co.evil/x?token=SECRET")
        self.assertEqual(opened2, [], "lookalike host must not be opened")
        self.assertIn("us.aws.cdn.hf.co.evil", str(cm.exception))
        self.assertNotIn("SECRET", str(cm.exception))

    def test_official_release_assets_host_is_allowed(self):
        opener, opened = self.build({
            "https://github.com/download/zip": (302, {
                "Location": ("https://release-assets.githubusercontent.com/"
                             "x?jwt=abc")}, b""),
            "https://release-assets.githubusercontent.com/x?jwt=abc": (200, {}, b"data"),
        })
        self.assertIn("release-assets.githubusercontent.com", svl.ALLOWED_HOSTS)
        # full opener built with query carries: must still open; the query is
        # only carried internally, never printed
        self.assertEqual(opener.open("https://github.com/download/zip").read(),
                         b"data")
        self.assertEqual(set(opened),
                         {"https://github.com/download/zip",
                          "https://release-assets.githubusercontent.com/x?jwt=abc"})

    def test_allowed_redirect_chain_is_followed(self):
        opener, opened = self.build({
            "https://huggingface.co/a": (302, {
                "Location": "https://cdn-lfs-us-1.hf.co/blob"}, b""),
            "https://cdn-lfs-us-1.hf.co/blob": (200, {}, b"data"),
        })
        self.assertEqual(opener.open("https://huggingface.co/a").read(), b"data")
        self.assertEqual(len(opened), 2)

    def test_http_and_userinfo_refused_before_any_open(self):
        opener, opened = self.build({})
        for bad in ("http://github.com/x", "https://user:pw@github.com/x"):
            with self.assertRaises(svl.SetupError, msg=bad):
                opener.open(bad)
        self.assertEqual(opened, [], "refusal must happen before any open")

    def test_errors_never_carry_signed_query(self):
        self.assertNotIn("TOKEN", str(svl.sanitize_error(
            "refusing", "https://evil.example/b?token=TOKEN")))


class ZipSafety(Base):
    def _zip_bytes(self, build):
        return _zip_bytes(build)

    def _extract(self, payload, dest_name="engine"):
        dest = Path(self.tmp) / dest_name
        zip_path = Path(self.tmp) / "t.zip"
        zip_path.write_bytes(payload)
        return svl.safe_extract(zip_path, dest), dest

    def test_traversal_and_absolute_and_symlinks_rejected(self):
        cases = [
            lambda zf: zf.writestr("../escape.txt", "x"),
            lambda zf: zf.writestr("/abs/root.txt", "x"),
            lambda zf: zf.writestr("a/../../up.txt", "x"),
            lambda zf: zf.writestr("C:\\evil.txt", "x"),
            lambda zf: zf.writestr("..\\..\\win-up.txt", "x"),
            lambda zf: zf.writestr("dup.txt", "one")
            or zf.writestr("dup.txt", "two"),
        ]
        for i, build in enumerate(cases):
            with self.assertRaises(svl.SetupError, msg=f"case {i}"):
                self._extract(self._zip_bytes(build))
        self.assertFalse((Path(self.tmp) / "escape.txt").exists())
        self.assertFalse((Path(self.tmp) / "up.txt").exists())
        self.assertFalse((Path(self.tmp) / "evil.txt").exists())

    def test_symlink_member_rejected(self):
        def build(zf):
            zi = zipfile.ZipInfo("sneaky.exe")
            zi.external_attr = (0o120777 & 0xFFFF) << 16  # S_IFLNK
            zf.writestr(zi, "target")
        with self.assertRaises(svl.SetupError):
            self._extract(self._zip_bytes(build))

    def test_zip_bomb_declared_size_rejected(self):
        def build(zf):
            zf.writestr("ok.exe", b"a")
            zf.writestr("bomb.bin", b"")
            # simulate a lying central directory (writestr resets file_size)
            zf.filelist[-1].file_size = svl.ZIP_MAX_TOTAL_BYTES * 4
        with self.assertRaises(svl.SetupError):
            self._extract(self._zip_bytes(build))

    def test_directory_entries_first_and_nested_extract_fine(self):
        def build(zf):
            zf.writestr(zipfile.ZipInfo("d1/"), b"")
            zf.writestr(zipfile.ZipInfo("d1/d2/"), b"")
            zf.writestr("d1/d2/x.bin", b"payload")
            zf.writestr("exe/whisper-cli.exe", b"MZ payload")
            zf.writestr("d2/", b"")
        records, dest = self._extract(self._zip_bytes(build))
        self.assertEqual((dest / "d1" / "d2" / "x.bin").read_bytes(), b"payload")
        self.assertTrue((dest / "d2").is_dir())
        by_path = {r["path"]: r for r in records}
        self.assertIn("d1/d2/x.bin", by_path)
        self.assertIn(str(Path("exe") / "whisper-cli.exe")
                      .replace(os.sep, "/"), by_path)
        self.assertEqual(by_path["d1/d2/x.bin"]["sha256"],
                         hashlib.sha256(b"payload").hexdigest())
        self.assertEqual(by_path["d1/d2/x.bin"]["size"], len(b"payload"))

    def test_valid_engine_zip_extracts_records_binaries_with_hashes(self):
        cli_payload = b"MZ" + b"fake cli" * 64

        def build(zf):
            zf.writestr("whisper-cli.exe", cli_payload)
            zf.writestr("whisper.dll", b"PK\x03\x04-dll-ish")
            zf.writestr("sub/whisper-server.exe", b"mz")
        records, dest = self._extract(self._zip_bytes(build))
        self.assertEqual((dest / "whisper-cli.exe").read_bytes(), cli_payload)
        by_path = {r["path"]: r for r in records}
        self.assertEqual(by_path["whisper-cli.exe"]["sha256"],
                         hashlib.sha256(cli_payload).hexdigest())
        self.assertEqual(by_path["whisper-cli.exe"]["size"], len(cli_payload))
        names = {r["path"] for r in records
                 if Path(r["path"]).suffix.lower() in (".exe", ".dll")}
        self.assertEqual(names, {"whisper-cli.exe", "whisper.dll"} | {
            str(Path("sub") / "whisper-server.exe").replace(os.sep, "/")})


class ChecksumAndPromotion(Base):
    def test_sha_mismatch_promotes_nothing_and_leaves_no_final(self):
        net = FakeNet(bad_engine_sha=True)
        with self.assertRaises(svl.SetupError) as cm:
            self.run_setup(net)
        self.assertIn("SHA-256", str(cm.exception))
        layout = svl.layout(self.root)
        self.assertFalse(layout["engine_zip"].exists())
        self.assertFalse(layout["model_bin"].exists())

    def test_size_mismatch_is_rejected(self):
        net = FakeNet(wrong_size=True)
        with self.assertRaises(svl.SetupError):
            self.run_setup(net)
        self.assertFalse(svl.layout(self.root)["engine_zip"].exists())

    def test_mismatch_never_leans_on_existing_unknown_file(self):
        net = FakeNet(bad_engine_sha=True)
        with self.assertRaises(svl.SetupError):
            self.run_setup(net)
        layout = svl.layout(self.root)
        layout["downloads"].mkdir(parents=True, exist_ok=True)
        layout["engine_zip"].write_bytes(b"someone else's file")
        net2 = FakeNet()  # good network now, but the unknown file is not trusted
        with self.assertRaises(svl.SetupError) as cm:
            self.run_setup(net2, reuse_unknown=False)
        self.assertEqual(layout["engine_zip"].read_bytes(),
                         b"someone else's file")
        self.assertIn("exists", str(cm.exception))


class BinaryVerification(Base):
    """Extracted exe/dll bytes must match their verified ZIP members on
    reuse AND --check; tampered binaries are refused, never executed."""

    def test_extraction_records_member_hashes_for_binaries(self):
        result = self.run_setup(FakeNet())
        manifest = json.loads((svl.layout(self.root)["root"] / "setup.json")
                              .read_text(encoding="utf-8"))
        binaries = manifest["engine"]["binaries"]
        self.assertTrue(all({"path", "size", "sha256"} <= set(b)
                            for b in binaries))
        exe = next(b for b in binaries
                   if b["path"].endswith("whisper-cli.exe"))
        self.assertEqual(exe["sha256"],
                         hashlib.sha256(b"MZ" + b"fake cli" * 64).hexdigest())

    def test_tampered_binary_blocks_reuse_and_check_and_is_kept(self):
        self.run_setup(FakeNet())
        layout = svl.layout(self.root)
        manifest = json.loads((layout["root"] / "setup.json")
                              .read_text("utf-8"))
        exe_dir = Path(manifest["engine"]["dir"])
        exe_path = exe_dir / "whisper-cli.exe"
        exe_path.write_bytes(b"MZ" + b"TAMPERED" * 64)
        net2 = FakeNet()
        net2.stream_opener = lambda url: (_ for _ in ()).throw(
            AssertionError("no download expected"))
        with self.assertRaises(svl.SetupError) as cm1:
            self.run_setup(net2)
        self.assertIn("whisper-cli.exe", str(cm1.exception))
        with self.assertRaises(svl.SetupError) as cm2:
            self.verify(net2)
        self.assertIn("whisper-cli.exe", str(cm2.exception))
        # the tampered file is preserved untouched (unknown; never replaced)
        self.assertEqual(exe_path.read_bytes(), b"MZ" + b"TAMPERED" * 64)

    def test_intact_binaries_pass_check(self):
        self.run_setup(FakeNet())
        svl.verify_only(self.root, svl.make_resolver(FakeNet().fetch_json),
                        validate=lambda r, k: None)  # no exception is the pass


class EngineReconciliation(Base):
    """Reuse of an own completed-but-unpublished extraction directory is only
    allowed when the marker carries the exact verified ZIP hash (directly or
    via an authoritative file record) and EVERY member of the directory still
    matches the VERIFIED ZIP members' records; anything else is preserved and
    reported, never guessed or deleted."""

    def _orphan_state(self):
        result = self.run_setup(FakeNet())
        layout = svl.layout(self.root)
        engine_dir = Path(result["engine"]["dir"])
        (layout["root"] / "setup.json").unlink()  # simulate job5: no manifest
        return layout, engine_dir

    def test_completed_marker_dir_is_reused_without_manifest(self):
        layout, engine_dir = self._orphan_state()
        dup_log = []
        with mock.patch("os.rename",
                        side_effect=AssertionError("directory rename attempted")):
            result = self.run_setup(FakeNet(), log=dup_log.append)
        self.assertEqual(Path(result["engine"]["dir"]), engine_dir,
                         "existing verified extraction must be reused")
        dirs = [d.name for d in layout["engine"].iterdir() if d.is_dir()]
        self.assertEqual(dirs, [engine_dir.name], "no duplicate directory minted")
        # the upgraded marker now carries authoritative file records
        marker = json.loads((engine_dir / svl.SETUP_MARKER_NAME)
                            .read_text("utf-8"))
        self.assertEqual(marker["state"], "complete")
        self.assertIn("files", marker)
        rec = {r["path"]: r for r in marker["files"]}
        self.assertEqual(rec["whisper-cli.exe"]["sha256"],
                         hashlib.sha256(b"MZ" + b"fake cli" * 64).hexdigest())
        self.assertFalse([s for s in dup_log if "duplicate" in s.lower()],
                         "full reuse must not report duplicated storage")

    def test_tampered_orphan_is_not_reused_dup_reported_and_preserved(self):
        layout, engine_dir = self._orphan_state()
        (engine_dir / "whisper-cli.exe").write_bytes(b"MZ" + b"EVIL" * 64)
        lines = []
        with mock.patch("os.rename",
                        side_effect=AssertionError("directory rename attempted")):
            result = self.run_setup(FakeNet(), log=lines.append)
        dirs = [d.name for d in layout["engine"].iterdir() if d.is_dir()]
        self.assertIn(engine_dir.name, dirs)
        self.assertEqual(len(dirs), 2, "tampered orphan preserved, new dir made")
        self.assertNotEqual(Path(result["engine"]["dir"]), engine_dir)
        self.assertTrue(any("duplicate" in s.lower() for s in lines))
        manifest = json.loads((layout["root"] / "setup.json")
                              .read_text("utf-8"))
        self.assertIn(engine_dir.name,
                      manifest["notes"].get("engine_duplicate_storage", []))
        self.assertEqual((engine_dir / "whisper-cli.exe").read_bytes(),
                         b"MZ" + b"EVIL" * 64)  # unknown content preserved

    def test_incomplete_or_unknown_marker_dirs_are_never_reused(self):
        result = self.run_setup(FakeNet())
        layout = svl.layout(self.root)
        good = Path(result["engine"]["dir"])
        (layout["root"] / "setup.json").unlink()
        engine_dir = Path(result["engine"]["dir"])
        marker = json.loads((engine_dir / svl.SETUP_MARKER_NAME)
                            .read_text("utf-8"))
        # incomplete marker: authoritative metadata says "not done"
        marker["state"] = "incomplete"
        (engine_dir / svl.SETUP_MARKER_NAME).write_text(json.dumps(marker),
                                                        encoding="utf-8")
        lines = []
        with mock.patch("os.rename",
                        side_effect=AssertionError("directory rename attempted")):
            result2 = self.run_setup(FakeNet(), log=lines.append)
        dirs = [d.name for d in layout["engine"].iterdir() if d.is_dir()]
        self.assertEqual(len(dirs), 2)
        self.assertNotEqual(Path(result2["engine"]["dir"]), engine_dir)
        self.assertIn(good.name, dirs)  # preserved untouched
        # a directory without our marker (unknown-ish) is never reused,
        # never touched: a new unique directory is created beside it
        mystery = layout["engine"] / "mystery-dir"
        mystery.mkdir()
        (mystery / "file.txt").write_text("not mine, keep")
        lines2 = []
        with mock.patch("os.rename",
                        side_effect=AssertionError("directory rename attempted")):
            result3 = self.run_setup(FakeNet(), log=lines2.append)
        dirs = [d.name for d in layout["engine"].iterdir() if d.is_dir()]
        self.assertEqual(len(dirs), 3)  # mystery dir preserved, new dir made
        self.assertEqual(result3["engine"]["dir"],
                         Path(result3["engine"]["dir"]))
        self.assertNotEqual(Path(result3["engine"]["dir"]), mystery)
        self.assertTrue((mystery / "file.txt").exists())
        self.assertIn("mystery-dir", dirs)

    def test_unexpected_unknown_file_inside_orphan_blocks_reuse(self):
        layout, engine_dir = self._orphan_state()
        (engine_dir / "unknown-note.txt").write_text("not mine")
        lines = []
        with mock.patch("os.rename",
                        side_effect=AssertionError("directory rename attempted")):
            result = self.run_setup(FakeNet(), log=lines.append)
        dirs = [d.name for d in layout["engine"].iterdir() if d.is_dir()]
        self.assertEqual(len(dirs), 2)
        self.assertTrue(any("duplicate" in s.lower() for s in lines))
        self.assertTrue((engine_dir / "unknown-note.txt").exists())


class LinkGuard(Base):
    """Junctions/symlinks/reparse points inside the data root are refused
    before any mutation; unknown links are never deleted."""

    @unittest.skipUnless(sys.platform.startswith("win"),
                         "junction creation is a Windows-only shortcut")
    def test_junction_inside_data_root_blocks_run_without_outside_writes(self):
        local = self.root / ".local" / "voice"
        (local / "logs").mkdir(parents=True)
        (local / "logs" / "old.log").write_text("preserve")
        outside = self.tmp / "outside-target"
        outside.mkdir()
        (outside / "canary.txt").write_text("keep")
        link = local / "models"
        subprocess.run(["cmd", "/c", "mklink", "/J", str(link), str(outside)],
                       check=True, capture_output=True)
        net = FakeNet()
        with self.assertRaises(svl.SetupError) as cm:
            self.run_setup(net)
        self.assertIn("reparse", str(cm.exception).lower())
        # nothing was written through the junction; unknown links kept intact
        self.assertEqual([p.name for p in outside.iterdir()], ["canary.txt"])
        self.assertTrue((local / "logs" / "old.log").exists())
        self.assertTrue((local / "models").is_dir() or link.exists())
        self.assertFalse(svl.layout(self.root)["engine_zip"].exists())


class Idempotence(Base):
    def test_second_run_downloads_nothing_and_reuses_verified_artifacts(self):
        net = FakeNet()
        result = self.run_setup(net)
        layout = svl.layout(self.root)
        self.assertTrue(layout["engine_zip"].exists())
        self.assertTrue(layout["model_bin"].exists())
        self.assertEqual(result["engine"]["tag"], "bFake1")
        self.assertTrue((result["engine"]["dir"] / "whisper-cli.exe").exists())
        net2 = FakeNet()
        net2.stream_opener = lambda url: (_ for _ in ()).throw(
            AssertionError("no download expected on resume"))
        svl.verify_only(self.root, svl.make_resolver(net2.fetch_json),
                        validate=lambda r, k: None)
        self.assertEqual(layout["engine_zip"].read_bytes(), ENGINE_BYTES)

    def test_part_cleanup_only_removes_own_part_files(self):
        net = FakeNet()
        layout = svl.layout(self.root)
        layout["downloads"].mkdir(parents=True)
        own_part = layout["downloads"] / ("whisper-bin-x64.zip."
                                          + "a" * 12 + ".part")
        own_part.write_bytes(b"half")
        stranger = layout["downloads"] / "stranger-notes.txt"
        stranger.write_bytes(b"keep me")
        self.run_setup(net)
        self.assertFalse(own_part.exists())
        self.assertTrue(stranger.exists())
        self.assertTrue(layout["engine_zip"].exists())

    def test_saved_manifest_matches_downloaded_hashes(self):
        self.run_setup(FakeNet())
        manifest = json.loads((svl.layout(self.root)["root"] / "setup.json")
                              .read_text(encoding="utf-8"))
        self.assertEqual(manifest["engine"]["sha256"], ENGINE_SHA)
        self.assertEqual(manifest["model"]["sha256"], MODEL_SHA)
        self.assertEqual(manifest["engine"]["size"], len(ENGINE_BYTES))
        self.assertEqual(manifest["model"]["size"], len(MODEL_BYTES))
        self.assertEqual(manifest["engine"]["digest_source"],
                         "github-release-asset-digest")
        readme = (svl.layout(self.root)["root"] / "README.local.md").read_text(
            encoding="utf-8")
        self.assertIn("whisper-cli.exe", readme)
        self.assertIn("ggml-base.bin", readme)
        self.assertIn("prerelease", readme.lower())
        lic = (svl.layout(self.root)["root"] / "licenses" /
               "whisper.cpp-LICENSE.txt").read_text(encoding="utf-8")
        self.assertEqual(lic, LICENSE_TEXT)


if __name__ == "__main__":
    unittest.main(verbosity=2)
