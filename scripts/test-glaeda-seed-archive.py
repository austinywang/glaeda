#!/usr/bin/env python3
"""Tests for scripts/glaeda-seed-archive against a real glaeda-seed-serve standing in for the seeder."""

from __future__ import annotations

import importlib.machinery
import importlib.util
import io
import json
import os
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SERVE = ROOT / "scripts/glaeda-seed-serve"


def load(name: str, path: Path):
    loader = importlib.machinery.SourceFileLoader(name, os.fspath(path))
    spec = importlib.util.spec_from_loader(name, loader)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    loader.exec_module(module)
    return module


archive = load("glaeda_seed_archive", ROOT / "scripts/glaeda-seed-archive")
pm = archive.pm
PREFIX = "admission-derived-data-v1-macOS-ARM64-" + "a" * 32 + "-j14-"
KEYS = [PREFIX + c * 40 for c in "bcdef"]

# Stands in for the archive host's /usr/bin/ssh to the seeder: runs the seeder's forced command locally.
FAKE_SSH = """import json, os, sys
os.environ["SSH_ORIGINAL_COMMAND"] = sys.argv[-1]
os.environ["SSH_CLIENT"] = "172.20.21.158 50000 22"
args = json.loads(os.environ["FAKE_SERVE_ARGS"])
os.execv(sys.executable, [sys.executable, *args])
"""


@unittest.skipUnless(pm.zstd_tool(), "needs zstd")
class ArchiveTest(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        base = Path(self.tmp.name).resolve()
        self.state = base / "seeder/ci"
        (self.state / "seeds").mkdir(parents=True)
        receipt = base / "seeder/receipt.json"
        receipt.write_text(json.dumps({"member": {"trustedRef": "refs/heads/main"}}))
        self.archive = base / "archive"
        fake = base / "ssh"
        fake.write_text(f"#!{sys.executable}\n" + FAKE_SSH)
        fake.chmod(0o755)
        self.env = dict(os.environ)
        os.environ["FAKE_SERVE_ARGS"] = json.dumps([os.fspath(SERVE), "--state", os.fspath(self.state), "--run-dir",
                                                  os.fspath(base / "seeder/run"), "--receipt", os.fspath(receipt)])
        self.saved = (pm.ssh_command, archive.STATE, archive.GRACE, archive.KEEP_NEWEST)
        pm.ssh_command = lambda config, address, request: [os.fspath(fake), request]
        archive.STATE = base / "state"
        self.config = {"user": "cmux", "addresses": ["172.20.21.202"], "identity": base / "k", "known_hosts": base / "kh"}
        self.saved_config = pm.lan_config
        pm.lan_config = lambda path=None: self.config

    def tearDown(self) -> None:
        pm.ssh_command, archive.STATE, archive.GRACE, archive.KEEP_NEWEST = self.saved
        pm.lan_config = self.saved_config
        os.environ.clear()
        os.environ.update(self.env)
        self.tmp.cleanup()

    def keep(self, key: str, mtime: int) -> None:
        seed = self.state / "seeds" / key
        (seed / "Build").mkdir(parents=True)
        (seed / "Build/x").write_bytes(os.urandom(4096))
        (seed / pm.MANIFEST).write_text("{}")
        os.utime(seed, (mtime, mtime))

    def run_archive(self, apply: bool = True, **kwargs) -> dict:
        kwargs.setdefault("min_free", 0)  # the test machine's own free space is not the archive disk's
        return archive.run(apply, self.archive, mounted=True, **kwargs)

    def test_plan_changes_nothing(self):
        self.keep(KEYS[0], 100)
        record = self.run_archive(apply=False)
        self.assertEqual(record["state"], "planned")
        self.assertFalse(self.archive.exists())

    def test_fill_takes_every_kept_seed_newest_first_then_is_a_no_op(self):
        for n, key in enumerate(KEYS[:3]):
            self.keep(key, 100 + n)
        record = self.run_archive()
        self.assertEqual(record["state"], "applied", record)
        self.assertEqual([f["key"] for f in record["fetched"]], [KEYS[2], KEYS[1], KEYS[0]])
        self.assertTrue(all(f["outcome"] == "fetched" for f in record["fetched"]), record)
        self.assertEqual(sorted(p.name for p in self.archive.iterdir()), sorted(k + ".tar.zst" for k in KEYS[:3]))
        again = self.run_archive()
        self.assertEqual((again["missing"], again["fetched"]), (0, []))

    def test_the_archive_serves_what_the_filler_wrote(self):
        self.keep(KEYS[0], 100)
        self.run_archive()
        env = {**os.environ, "SSH_ORIGINAL_COMMAND": f"seed-v1 tar {KEYS[1]} {KEYS[0]}", "SSH_CLIENT": "172.20.21.192 1 22"}
        proc = subprocess.run([sys.executable, os.fspath(SERVE), "--role", "archive", "--archive", os.fspath(self.archive),
                               "--run-dir", os.fspath(self.archive.parent / "run"), "--receipt",
                               os.fspath(self.archive.parent / "no-receipt")], env=env, capture_output=True, timeout=60)
        head, _, body = proc.stdout.partition(b"\n")
        self.assertEqual((proc.returncode, head.decode()), (0, f"glaeda-seed-serve 1 hit {KEYS[0]} tar"))
        names = tarfile.open(fileobj=io.BytesIO(body)).getnames()
        self.assertIn(f"{KEYS[0]}/{pm.MANIFEST}", names)

    def test_prune_drops_the_oldest_over_budget(self):
        for n, key in enumerate(KEYS):
            self.keep(key, 100 + n)
        self.run_archive()
        for n, key in enumerate(KEYS):  # fetched long ago, oldest first
            os.utime(self.archive / (key + ".tar.zst"), (1000 + n, 1000 + n))
        for key in KEYS:  # the seeder dropped them all; only then may prune take them
            shutil.rmtree(self.state / "seeds" / key)
        archive.KEEP_NEWEST = 2
        size = (self.archive / (KEYS[0] + ".tar.zst")).stat().st_size
        record = self.run_archive(budget=size * 2 + size // 2)
        self.assertEqual([d["key"] for d in record["pruned"]], KEYS[:3])
        self.assertEqual(sorted(p.name[:-8] for p in self.archive.iterdir()), sorted(KEYS[3:]))

    def test_a_full_archive_fetches_nothing_and_spares_the_seeders_seeds(self):
        self.keep(KEYS[0], 100)
        self.run_archive()
        self.keep(KEYS[1], 101)
        os.utime(self.archive / (KEYS[0] + ".tar.zst"), (1000, 1000))
        archive.KEEP_NEWEST = 0
        record = self.run_archive(budget=1)
        self.assertTrue(record["full"])
        self.assertEqual((record["fetched"], record["pruned"]), ([], []))  # KEYS[0] is still on the seeder
        self.assertTrue((self.archive / (KEYS[0] + ".tar.zst")).exists())

    def test_prune_keeps_recently_used_seeds(self):
        self.keep(KEYS[0], 100)
        self.keep(KEYS[1], 101)
        self.run_archive()
        archive.KEEP_NEWEST = 0
        record = self.run_archive(budget=0)
        self.assertEqual(record["pruned"], [])  # both touched within GRACE

    def test_check_rejects_entries_outside_the_key_and_missing_manifests(self):
        def pack(names: list[str]) -> Path:
            buf = io.BytesIO()
            with tarfile.open(fileobj=buf, mode="w") as tar:
                for name in names:
                    info = tarfile.TarInfo(name)
                    info.size = 1
                    tar.addfile(info, io.BytesIO(b"x"))
            path = self.archive.parent / "t.tar.zst"
            path.write_bytes(subprocess.run([pm.zstd_tool(), "-q", "-c"], input=buf.getvalue(), capture_output=True,
                                            check=True).stdout)
            return path
        key = KEYS[0]
        self.assertEqual(archive.check(pack([f"{key}/{pm.MANIFEST}", f"{key}/._x"]), key), "")
        self.assertIn("outside", archive.check(pack([f"{key}/{pm.MANIFEST}", "../evil"]), key))
        self.assertIn("outside", archive.check(pack([f"{key}/{pm.MANIFEST}", f"{KEYS[1]}/x"]), key))
        self.assertIn("outside", archive.check(pack([f"{key}/{pm.MANIFEST}", f"{key}/../x"]), key))
        self.assertIn("outside", archive.check(pack([f"{key}/{pm.MANIFEST}", "._other"]), key))
        self.assertEqual(archive.check(pack([f"{key}/{pm.MANIFEST}", f"._{key}"]), key), "")
        self.assertEqual(archive.check(pack([f"{key}/x"]), key), "no seed manifest")

    def test_an_untrusted_seeder_is_an_error(self):
        (self.state.parent / "receipt.json").write_text("{}")
        record = self.run_archive()
        self.assertEqual(record["state"], "error")
        self.assertIn("refused", record["reason"])

    def test_the_launch_agent_runs_apples_python(self):
        plist = archive.plist(Path("/Users/x/.local/libexec/glaeda-seed-archive"))
        self.assertEqual(plist["ProgramArguments"][:2], ["/usr/bin/python3", "-I"])
        self.assertEqual(plist["StartInterval"], 300)


if __name__ == "__main__":
    unittest.main()
