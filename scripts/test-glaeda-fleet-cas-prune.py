#!/usr/bin/env python3
"""Tests for scripts/glaeda-fleet-cas-prune."""

from __future__ import annotations

import contextlib
import fcntl
import importlib.machinery
import importlib.util
import io
import json
import os
import sys
import tempfile
import time
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
loader = importlib.machinery.SourceFileLoader("glaeda_fleet_cas_prune",
                                              os.fspath(ROOT / "scripts" / "glaeda-fleet-cas-prune"))
spec = importlib.util.spec_from_loader("glaeda_fleet_cas_prune", loader)
fp = importlib.util.module_from_spec(spec)
sys.modules["glaeda_fleet_cas_prune"] = fp
loader.exec_module(fp)


class PruneTest(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name) / "xcode"
        self.root.mkdir()
        self.lock = Path(self.tmp.name) / "host.lock"
        self.lock.write_text("")
        self.saved = (fp.ROOT, fp.HOST_LOCK, fp.running_commands)
        fp.ROOT, fp.HOST_LOCK = self.root, self.lock
        fp.running_commands = lambda: ["/usr/libexec/something", "zsh"]

    def tearDown(self) -> None:
        fp.ROOT, fp.HOST_LOCK, fp.running_commands = self.saved
        self.tmp.cleanup()

    def env(self, sign_key: str = "") -> None:
        (self.root / "fleet-cas.env").write_text(
            f"FLEET_CAS_STORE=100.89.140.13:7450\nFLEET_CAS_TRUSTED_KEYS=ab\nFLEET_CAS_SIGN_KEY={sign_key}\n")

    def node(self, n: int, size: int = 4096) -> list[Path]:
        paths = []
        for i in range(n):
            sub = "kv" if i % 3 == 0 else "cas"
            p = self.root / "node-store" / sub / f"{i:02x}"[:2] / f"obj{i}"
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_bytes(b"\0" * size)
            t = time.time() - (n - i) * 3600  # obj0 is the oldest
            os.utime(p, (t, t))
            paths.append(p)
        (self.root / "node-store/stats.json").write_text("{}")
        return paths

    def run_main(self, *args: str) -> dict:
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            self.assertEqual(fp.main(list(args)), 0)
        return json.loads(out.getvalue())

    def test_busy_matches_builds_not_listeners(self) -> None:
        self.assertTrue(fp.busy(["/Applications/Xcode_26.6.app/Contents/Developer/usr/bin/xcodebuild -scheme cmux"]))
        self.assertTrue(fp.busy(["/Users/cmux/actions-runner-glaeda-2/bin/Runner.Worker spawnclient 1 2"]))
        self.assertTrue(fp.busy(["/Users/cmux/actions-runner-x/bin.2.337.0/Runner.Worker spawnclient 1 2"]))
        self.assertTrue(fp.busy(["/Applications/Xcode_26.6.app/Contents/SharedFrameworks/SwiftBuild.framework/"
                                 "Versions/A/PlugIns/SWBBuildService.bundle/Contents/MacOS/SWBBuildService"]))
        self.assertFalse(fp.busy(["/Users/cmux/actions-runner-glaeda/bin/Runner.Listener run",
                                  "python3 xcodebuild-log-parser.py"]))

    def test_no_fleet_cas_is_a_quiet_no_op(self) -> None:
        self.assertEqual(self.run_main("--apply")["result"], "no fleet-cas on this host")

    def test_reader_evicts_oldest_node_files_to_ninety_percent(self) -> None:
        self.env()
        paths = self.node(10)
        per = paths[0].stat().st_blocks * 512
        rec = self.run_main("--apply", "--node-gib", str(6 * per / fp.GIB))
        self.assertEqual(rec["role"], "reader")
        self.assertEqual(rec["result"], "ok")
        left = [p for p in paths if p.exists()]
        # budget 6 files, goal 90% of that: the 5 newest stay, the 5 oldest go
        self.assertEqual(left, paths[5:])
        self.assertTrue((self.root / "node-store/stats.json").exists())

    def test_under_budget_touches_nothing(self) -> None:
        self.env()
        paths = self.node(4)
        self.run_main("--apply", "--node-gib", "1")
        self.assertTrue(all(p.exists() for p in paths))

    def test_plan_only_deletes_nothing(self) -> None:
        self.env()
        paths = self.node(10)
        rec = self.run_main("--node-gib", "0.000001", "--local-cas-gib", "0")
        self.assertIn("would remove", rec["node_store"])
        self.assertTrue(all(p.exists() for p in paths))

    def test_writer_keeps_both_stores(self) -> None:
        self.env("/Users/cmux/.config/fleet-cas/sign.key")
        paths = self.node(10)
        (self.root / "cas/v1").mkdir(parents=True)
        (self.root / "cas/v1/blob").write_bytes(b"\0" * 8192)
        rec = self.run_main("--apply", "--node-gib", "0", "--local-cas-gib", "0")
        self.assertEqual(rec["role"], "writer")
        self.assertTrue(all(p.exists() for p in paths))
        self.assertTrue((self.root / "cas/v1/blob").exists())

    def test_reader_local_cas_goes_whole_over_budget(self) -> None:
        self.env()
        (self.root / "cas/v1").mkdir(parents=True)
        (self.root / "cas/v1/blob").write_bytes(b"\0" * 8192)
        self.assertEqual(self.run_main("--apply", "--local-cas-gib", "0")["local_cas"], "removed")
        self.assertFalse((self.root / "cas").exists())
        self.assertEqual([p.name for p in self.root.iterdir() if p.name.startswith(".cas")], [])

    def test_leftover_renamed_cas_is_swept(self) -> None:
        self.env()
        left = self.root / ".cas.pruning-4242/v1"
        left.mkdir(parents=True)
        (left / "blob").write_bytes(b"\0" * 8192)
        rec = self.run_main("--apply")
        self.assertGreater(rec["swept_bytes"], 0)
        self.assertFalse((self.root / ".cas.pruning-4242").exists())

    def test_nothing_to_delete_never_takes_the_lock(self) -> None:
        self.env()
        self.node(3)
        held = os.open(self.lock, os.O_RDONLY)
        try:
            fcntl.flock(held, fcntl.LOCK_EX)
            self.assertEqual(self.run_main("--apply", "--node-gib", "1")["result"], "ok")
        finally:
            os.close(held)

    def fake_store(self, no_roots: int) -> Path:
        for sub in ("kv", "cas"):
            (self.root / "fleet-store" / sub).mkdir(parents=True)
        (self.root / "bin").mkdir()
        log = self.root / "gc.calls"
        binary = self.root / "bin/fleet-cas"
        binary.write_text("#!/bin/sh\n"
                          f"echo \"$*\" >> {log}\n"
                          "case \"$*\" in *--dry-run*) p='dry run: ';; *) p='';; esac\n"
                          f"echo \"${{p}}kv kept 5 deleted 1; cas kept 9 deleted 2 (4096 bytes); "
                          f"kept entries naming no stored object: {no_roots}\"\n")
        binary.chmod(0o755)
        return log

    def test_store_gc_dry_runs_first_and_runs_daily(self) -> None:
        log = self.fake_store(0)
        rec = fp.store_gc(self.root, 14)
        self.assertEqual(rec["gc"], "ran")
        calls = log.read_text().splitlines()
        self.assertEqual(len(calls), 2)
        self.assertIn("--dry-run", calls[0])
        self.assertNotIn("--dry-run", calls[1])
        self.assertIn("--keep-days 14", calls[1])
        self.assertEqual(fp.store_gc(self.root, 14), {"gc": "not due"})
        self.assertEqual(fp.store_gc(self.root, 14, now=time.time() + fp.GC_EVERY + 1)["gc"], "ran")

    def test_store_gc_refuses_when_entries_name_no_object(self) -> None:
        log = self.fake_store(3)
        rec = fp.store_gc(self.root, 14)
        self.assertTrue(rec["gc"].startswith("skipped: kept entries name no stored object"))
        self.assertEqual(len(log.read_text().splitlines()), 1)  # only the dry run

    def test_no_store_no_gc(self) -> None:
        self.assertEqual(fp.store_gc(self.root, 14), {})

    def test_defers_while_a_build_runs_or_holds_the_lock(self) -> None:
        self.env()
        paths = self.node(10)
        fp.running_commands = lambda: ["/Users/cmux/actions-runner-glaeda/bin/Runner.Worker spawnclient 1 2"]
        self.assertTrue(self.run_main("--apply", "--node-gib", "0")["result"].startswith("deferred: a build is running"))
        fp.running_commands = lambda: None
        self.assertTrue(self.run_main("--apply", "--node-gib", "0")["result"].startswith("deferred: ps failed"))
        fp.running_commands = lambda: []
        held = os.open(self.lock, os.O_RDWR)
        try:
            fcntl.flock(held, fcntl.LOCK_EX)
            self.assertEqual(self.run_main("--apply", "--node-gib", "0")["result"],
                             "deferred: a build holds the host lock")
        finally:
            os.close(held)
        self.assertTrue(all(p.exists() for p in paths))


if __name__ == "__main__":
    unittest.main()
