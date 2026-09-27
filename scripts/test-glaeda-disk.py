#!/usr/bin/env python3
"""Contract tests for scripts/glaeda-disk."""

from __future__ import annotations

import importlib.machinery
import importlib.util
import contextlib
import io
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
loader = importlib.machinery.SourceFileLoader("glaeda_disk", os.fspath(ROOT / "scripts" / "glaeda-disk"))
spec = importlib.util.spec_from_loader("glaeda_disk", loader)
gd = importlib.util.module_from_spec(spec)
sys.modules["glaeda_disk"] = gd
loader.exec_module(gd)


def make(path: Path, mib: int = 1, age_hours: float = 48) -> Path:
    path.mkdir(parents=True)
    (path / "blob").write_bytes(b"\0" * (mib * 1024 * 1024))
    t = time.time() - age_hours * 3600
    for p in (path / "blob", path):
        os.utime(p, (t, t))
    return path


class GlaedaDiskTest(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(os.path.realpath(self.tmp.name))
        self.fam = gd.Family("xcode-derived-data", self.root, True, "rebuild")
        self.gd_evidence = gd.process_evidence
        gd.process_evidence = lambda: ([], "")
        self.gd_keep = gd.NESTED_KEEP
        gd.NESTED_KEEP = self.root.parent / f"{self.root.name}-keep.json"

    def tearDown(self) -> None:
        gd.process_evidence = self.gd_evidence
        gd.NESTED_KEEP.unlink(missing_ok=True)
        gd.NESTED_KEEP = self.gd_keep
        self.tmp.cleanup()

    def verdicts(self, idle: float = 24) -> dict[str, str]:
        return {Path(i.path).name: i.verdict for i in gd.survey([self.fam], idle, 0)}

    def test_idle_is_reclaimable_recent_is_not(self) -> None:
        make(self.root / "old")
        make(self.root / "new", age_hours=1)
        self.assertEqual(self.verdicts(), {"old": "reclaimable", "new": "recent"})

    def test_tmpdir_double_slash_still_names_the_path(self) -> None:
        cmds = gd.command_text("bash /var/folders/px/abc/T//work/build.sh\n")
        self.assertTrue(gd.named_by("/private/var/folders/px/abc/T/work", cmds))
        self.assertFalse(gd.named_by("/private/var/folders/px/abc/T/wor", cmds))

    def test_process_cwd_and_command_line_veto(self) -> None:
        a, b = make(self.root / "a"), make(self.root / "b")
        make(self.root / "a-sibling")
        gd.process_evidence = lambda: ([str(a / "sub")], f"xcodebuild -derivedDataPath {b} build\n")
        v = self.verdicts()
        self.assertEqual(v["a"], "in-use")
        self.assertEqual(v["b"], "in-use")
        # a prefix of another path must not veto it
        self.assertEqual(v["a-sibling"], "reclaimable")

    def test_git_checkout_is_never_reclaimable_outside_derived_data(self) -> None:
        self.fam = gd.Family("tmp", self.root, True, "scratch")
        repo = make(self.root / "repo")
        subprocess.run(["git", "init", "-q", str(repo)], check=True)
        os.utime(repo, (time.time() - 48 * 3600,) * 2)
        os.utime(repo / ".git", (time.time() - 48 * 3600,) * 2)
        self.assertEqual(gd.survey([self.fam], 24, 0)[0].verdict, "git-checkout")

    def test_bulk_sizes_walk_the_root_once_and_skip_prefixes(self) -> None:
        self.fam = gd.Family("user-tmp", self.root, True, "scratch", bulk_sizes=True,
                             skip_prefixes=("com.apple.",))
        for name in ("a", "b", "c"):
            make(self.root / name, mib=2)
        saved_min, gd.BULK_SIZE_MIN = gd.BULK_SIZE_MIN, 2
        make(self.root / "com.apple.imtransferservices")
        calls: list[list[str]] = []
        real_run = gd.subprocess.run

        def counting_run(args, *a, **k):
            if args and args[0] == "du":
                calls.append(list(args))
            return real_run(args, *a, **k)

        gd.subprocess.run = counting_run
        try:
            items = gd.survey([self.fam], 24, 1 << 20)
        finally:
            gd.subprocess.run = real_run
            gd.BULK_SIZE_MIN = saved_min
        self.assertEqual(sorted(Path(i.path).name for i in items), ["a", "b", "c"])
        self.assertTrue(all(i.bytes >= 2 << 20 and i.verdict == "reclaimable" for i in items))
        self.assertEqual([c[:4] for c in calls], [["du", "-xk", "-d", "1"]])  # one walk, no per-item du
        # a report defers unsized bulk candidates to the background refresh instead of walking
        make(self.root / "d", mib=2)
        known = {i.path: i.bytes for i in items}
        deferred = gd.survey([self.fam], 24, 1 << 20, known, defer_bulk=True)
        self.assertEqual(sorted(Path(i.path).name for i in deferred), ["a", "b", "c"])

    def test_darwin_user_tmp_family(self) -> None:
        saved = (gd.DARWIN, gd.darwin_user_tmp)
        gd.DARWIN, gd.darwin_user_tmp = True, lambda: self.root
        try:
            fams = {f.id: f for f in gd.default_families()}
        finally:
            gd.DARWIN, gd.darwin_user_tmp = saved
        fam = fams["user-tmp"]
        self.assertEqual(fam.root, self.root)
        self.assertTrue(fam.reclaimable and fam.git_disposable and fam.bulk_sizes)
        self.assertIn("com.apple.", fam.skip_prefixes)

    def test_report_only_family_is_never_deleted(self) -> None:
        self.fam = gd.Family("user-cache", self.root, False, "report")
        make(self.root / "cache")
        items = gd.survey([self.fam], 24, 0)
        self.assertEqual(items[0].verdict, "report-only")
        receipt = self.root.parent / f"{self.root.name}-receipt.jsonl"
        gd.apply(items, {"user-cache": self.fam}, receipt, None, 24)
        self.assertTrue((self.root / "cache").exists())
        self.assertFalse(receipt.exists())

    def test_apply_deletes_and_writes_receipt(self) -> None:
        make(self.root / "old")
        receipt = self.root.parent / f"{self.root.name}-receipt.jsonl"
        try:
            items = gd.survey([self.fam], 24, 0)
            gd.apply(items, {self.fam.id: self.fam}, receipt, None, 24)
            self.assertFalse((self.root / "old").exists())
            self.assertIn('"outcome": "reclaimed"', receipt.read_text())
        finally:
            receipt.unlink(missing_ok=True)

    def test_apply_rechecks_and_skips_item_touched_since_survey(self) -> None:
        d = make(self.root / "old")
        items = gd.survey([self.fam], 24, 0)
        (d / "fresh").write_text("x")
        receipt = self.root.parent / f"{self.root.name}-receipt.jsonl"
        try:
            gd.apply(items, {self.fam.id: self.fam}, receipt, None, 24)
            self.assertTrue(d.exists())
            self.assertIn("changed:modified", receipt.read_text())
        finally:
            receipt.unlink(missing_ok=True)

    def test_git_clone_two_levels_down_is_a_checkout(self) -> None:
        self.fam = gd.Family("claude-scratchpad", self.root, True, "scratch")
        session = make(self.root / "session")
        (session / "scratchpad" / "repo" / ".git").mkdir(parents=True)
        for p in (session / "scratchpad" / "repo" / ".git", session / "scratchpad" / "repo",
                  session / "scratchpad", session):
            os.utime(p, (time.time() - 48 * 3600,) * 2)
        self.assertEqual(gd.survey([self.fam], 24, 0)[0].verdict, "git-checkout")

    def test_deep_tree_is_unchecked_not_git_and_build_dirs_are_skipped(self) -> None:
        deep = self.root / "deep"
        (deep / "a/b/c/d/e/f/g").mkdir(parents=True)
        self.assertEqual(gd.git_state(deep), "unchecked")
        mods = self.root / "mods"
        (mods / "node_modules/x/y/z/w/v/u").mkdir(parents=True)
        self.assertEqual(gd.git_state(mods), "none")
        (self.root / "clone/target/.git").mkdir(parents=True)
        self.assertEqual(gd.git_state(self.root / "clone"), "git")
        (self.root / "repo/sub/.git").mkdir(parents=True)
        self.assertEqual(gd.git_state(self.root / "repo"), "git")

    def test_tmp_alias_and_comma_lists_count_as_named(self) -> None:
        self.assertTrue(gd.named_by("/private/tmp/foo", "tool --out /tmp/foo/dist\n"))
        self.assertTrue(gd.named_by("/private/tmp/foo", "tool --dirs=/private/tmp/foo,/x\n"))
        self.assertFalse(gd.named_by("/private/tmp/foo", "tool /tmp/foobar\n"))
        self.assertTrue(gd.in_cwd("/private/tmp/foo", ["/tmp/foo/a.log"]))
        index = gd.open_index(["/tmp/foo/a.log", "/tmp/foobar/b"])
        self.assertTrue(gd.in_cwd("/tmp/foo", index))
        self.assertTrue(gd.in_cwd("/tmp/foo/a.log", index))
        self.assertFalse(gd.in_cwd("/tmp/fo", index))
        self.assertFalse(gd.in_cwd("/tmp/foo/a", index))

    def test_missing_process_evidence_fails_closed(self) -> None:
        make(self.root / "old")

        def blind():
            raise gd.NoEvidence("lsof missing")
        gd.process_evidence = blind
        items = gd.survey([self.fam], 24, 0)
        self.assertEqual(items[0].verdict, "in-use")
        forced = [gd.Item(i.family, i.path, i.bytes, i.idle_hours, "reclaimable") for i in items]
        receipt = self.root.parent / f"{self.root.name}-receipt.jsonl"
        gd.apply(forced, {self.fam.id: self.fam}, receipt, None, 24)
        self.assertTrue((self.root / "old").exists())

    def test_remove_never_chmods_through_symlinks(self) -> None:
        outside = self.root.parent / f"{self.root.name}-outside"
        outside.write_text("x")
        os.chmod(outside, 0o400)
        try:
            tree = self.root / "tree"
            (tree / "ro").mkdir(parents=True)
            os.symlink(outside, tree / "ro" / "link")
            os.chmod(tree / "ro", 0o500)
            gd.remove(tree)
            self.assertFalse(tree.exists())
            self.assertEqual(outside.stat().st_mode & 0o777, 0o400)
        finally:
            os.chmod(outside, 0o600)
            outside.unlink()

    def test_known_sizes_are_reused_and_new_items_measured(self) -> None:
        old, new = make(self.root / "old"), make(self.root / "new")
        known = {str(old): 7 * 1024**3}
        sizes = {Path(i.path).name: i.bytes for i in gd.survey([self.fam], 24, 0, known)}
        self.assertEqual(sizes["old"], 7 * 1024**3)
        self.assertGreater(sizes["new"], 0)
        self.assertIn(str(new), known)  # measured size is kept for the next snapshot

    def test_saving_new_paths_keeps_the_snapshot_stamp(self) -> None:
        snap = self.root / "keep.json"
        gd.save_snapshot({"/a": 1, "/b": 2}, snap, at=1000.0)
        self.assertEqual(gd.load_snapshot(snap), (1000.0, {"/a": 1, "/b": 2}))
        gd.save_snapshot({"/a": 1}, snap)
        self.assertGreater(gd.load_snapshot(snap)[0], 1000.0)  # a full measurement is now

    def test_snapshot_round_trip_and_corrupt_snapshot_is_empty(self) -> None:
        snap = self.root / "sizes.json"
        gd.save_snapshot({"/a": 1}, snap)
        self.assertEqual(gd.load_snapshot(snap)[1], {"/a": 1})
        snap.write_text("{not json")
        self.assertEqual(gd.load_snapshot(snap), (0.0, {}))

    def receipt(self) -> Path:
        r = self.root.parent / f"{self.root.name}-receipt.jsonl"
        self.addCleanup(r.unlink, missing_ok=True)
        return r

    def test_help_renders(self) -> None:  # argparse %-formats help; a bare % breaks it
        with contextlib.redirect_stdout(io.StringIO()) as out, self.assertRaises(SystemExit) as e:
            gd.main(["--help"])
        self.assertEqual(e.exception.code, 0)
        self.assertIn("--low", out.getvalue())

    def test_thresholds_take_gib_or_percent(self) -> None:
        self.assertEqual(gd.space("60", 10**15), 60 * 1024**3)
        gib = 1024**3
        # one default across a 256 GB laptop, a 1 TB mini and an 8 TB studio
        self.assertEqual(gd.space("15%:40-150", 238 * gib), 40 * gib)
        self.assertEqual(gd.space("15%:40-150", 931 * gib), int(931 * gib * 0.15))
        self.assertEqual(gd.space("15%:40-150", 7450 * gib), 150 * gib)
        # a 30 GiB VM: the 40 GiB floor stops at twice the share instead of exceeding the disk
        self.assertEqual(gd.space("15%:40-150", 30 * gib), int(30 * gib * 0.15) * 2)
        self.assertLess(gd.space("25%:80-300", 30 * gib), 30 * gib)
        for bad in ("60:1-2", "15%:9", "15%:50-40", "15%:a-b"):
            with self.assertRaises(ValueError):
                gd.space(bad, 1000)
        self.assertEqual(gd.space("15%", 1000), 150)
        self.assertEqual(gd.space("2.5", 0), int(2.5 * 1024**3))
        with self.assertRaises(ValueError):
            gd.space("lots", 1000)

    def test_cmux_active_marker_holds_a_slot(self) -> None:
        slot = self.root / "work/slot-1"
        (slot / ".cmux-active").mkdir(parents=True)
        self.assertFalse(gd.cmux_active(self.root / "work"))  # marker dir, no pid yet
        (slot / ".cmux-active/pid").write_text(f"{os.getpid()}\n")
        self.assertTrue(gd.cmux_active(self.root / "work"))  # live pid one level down
        dead = subprocess.Popen(["true"])
        dead.wait()
        (slot / ".cmux-active/pid").write_text(str(dead.pid))
        self.assertFalse(gd.cmux_active(self.root / "work"))
        (slot / ".cmux-active/pid").write_text("garbage")
        self.assertTrue(gd.cmux_active(self.root / "work"))  # unreadable: fail closed
        (slot / ".cmux-active/pid").unlink()
        (slot / ".lock").write_text("")
        self.assertTrue(gd.cmux_active(self.root / "work"))  # hq's slot lock also means busy
        (slot / ".lock").unlink()
        (self.root / "work/slot-1/SourcePackages").mkdir()
        (self.root / "work/slot-1/SourcePackages/.lock").write_text("")
        self.assertFalse(gd.cmux_active(self.root / "work"))  # a SwiftPM lock is not a slot lock
        # the marker lives in work/<slot>; the DerivedData beside it is what glaeda-disk deletes
        (slot / ".cmux-active/pid").write_text(str(os.getpid()))
        fam = gd.Family("cmux-job-cache", self.root, True, "x", depth=2)
        (self.root / "DerivedData/slot-1-simulator").mkdir(parents=True)
        self.assertTrue(gd.job_busy(fam, self.root / "DerivedData"))
        self.assertFalse(gd.cmux_active(self.root / "DerivedData"))

    def test_pressure_deletes_cheapest_loss_first(self) -> None:
        dd = gd.Family("xcode-derived-data", self.root, True, "x", rebuild_minutes=15)
        scratch = gd.Family("claude-scratchpad", self.root, True, "x", rebuild_minutes=0.5)
        big = gd.Item("xcode-derived-data", "/a", 20 * gd.GIB, 30.0, "reclaimable")
        small = gd.Item("claude-scratchpad", "/b", 2 * gd.GIB, 7.0, "reclaimable")
        self.assertGreater(gd.value(small, scratch), gd.value(big, dd))
        stale = gd.replace(big, idle_hours=24 * 7)
        self.assertGreater(gd.value(stale, dd), gd.value(big, dd))  # long idle ranks higher

    @unittest.skipUnless(sys.platform == "darwin", "launchd")
    def test_owner_retired_needs_a_named_unloaded_service(self) -> None:
        self.assertFalse(gd.owner_retired(gd.Family("x", self.root, False, "", owner="someone")))
        gone = gd.Family("x", self.root, False, "", owner="someone", owner_job="system/com.example.glaeda-none")
        self.assertTrue(gd.owner_retired(gone))

    def test_pressure_waits_for_a_ci_job_until_the_emergency_floor(self) -> None:
        job = "/Users/cmux/actions-runner-glaeda-2/bin/Runner.Worker spawnclient 148 151"
        self.assertTrue(gd.CI_WORKER.search(job))
        self.assertTrue(gd.CI_WORKER.search("/Users/cmux/actions-runner/bin/Runner.Worker"))
        self.assertTrue(gd.CI_WORKER.search("/Users/cmux/actions-runner-cmux-nightly-mini/bin.2.337.0/Runner.Worker spawnclient 1 2"))
        self.assertFalse(gd.CI_WORKER.search("/Users/cmux/actions-runner-glaeda/bin/Runner.Listener run"))
        total = 460 * gd.GIB  # a build mini: emergency 10%:30-60 is 46 GiB
        low = gd.Fs(1, "/", 60 * gd.GIB, total, 69 * gd.GIB, 115 * gd.GIB)
        self.assertIn("deferred", gd.ci_defer({1: low}, [job], "10%:30-60"))
        self.assertEqual(gd.ci_defer({1: low}, [], "10%:30-60"), "")  # no job: go ahead
        critical = gd.replace(low, free=40 * gd.GIB)
        self.assertEqual(gd.ci_defer({1: critical}, [job], "10%:30-60"), "")  # the job would fail anyway
        with self.assertRaises(SystemExit), contextlib.redirect_stderr(io.StringIO()):
            gd.main(["--pressure", "--emergency", "lots"])

    def test_pressure_targets_are_per_filesystem(self) -> None:
        make(self.root / "old")
        items = gd.survey([self.fam], 24, 0)
        fams = {self.fam.id: self.fam}
        dev = self.root.stat().st_dev
        # this filesystem is not under pressure: nothing on it may go
        gd.apply(items, fams, self.receipt(), {dev + 1: 1 << 62}, 24)
        self.assertTrue((self.root / "old").exists())
        # under pressure but already at its free target: stop before deleting
        gd.apply(items, fams, self.receipt(), {dev: 0}, 24)
        self.assertTrue((self.root / "old").exists())
        gd.apply(items, fams, self.receipt(), {dev: 1 << 62}, 24)
        self.assertFalse((self.root / "old").exists())

    def test_filesystems_group_roots_and_apply_thresholds(self) -> None:
        other = gd.Family("tmp", self.root, True, "scratch")
        fss = gd.filesystems([self.fam, other], "0", "100%")
        self.assertEqual(len(fss), 1)
        fs = next(iter(fss.values()))
        self.assertFalse(fs.under)
        self.assertEqual(fs.target, fs.total)
        self.assertEqual(gd.filesystems([self.fam], "100%", "100%")[fs.dev].low, fs.total)

    def test_directory_holding_a_socket_is_in_use(self) -> None:
        d = make(self.root / "tmux-1000")
        make(self.root / "plain")
        sock = socket.socket(socket.AF_UNIX)
        self.addCleanup(sock.close)
        sock.bind(str(d / "default"))
        # a long-lived server's socket is as old as its directory; only an old enough item pays for the socket walk
        for p in (d / "default", d):
            os.utime(p, (time.time() - 48 * 3600,) * 2)
        v = self.verdicts()
        self.assertEqual(v["tmux-1000"], "in-use")
        self.assertEqual(v["plain"], "reclaimable")
        forced = [gd.Item("xcode-derived-data", str(d), 1, 48, "reclaimable")]
        receipt = self.receipt()
        gd.apply(forced, {self.fam.id: self.fam}, receipt, None, 24)
        self.assertTrue(d.exists())
        self.assertIn("changed:in-use", receipt.read_text())

    def _age(self, root: Path, hours: float = 48) -> None:
        t = time.time() - hours * 3600
        for dirpath, dirnames, filenames in os.walk(root):
            for n in dirnames + filenames:
                os.utime(os.path.join(dirpath, n), (t, t), follow_symlinks=False)
        os.utime(root, (t, t))

    def _git(self, *args: str) -> str:
        env = {**os.environ, "GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@t",
               "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@t"}
        # no detached auto-maintenance: it writes into .git after a test has aged the tree
        return subprocess.run(["git", "-c", "maintenance.auto=false", "-c", "gc.auto=0", *args],
                              check=True, capture_output=True, text=True, env=env).stdout

    def _tmp_repos(self) -> tuple[Path, Path]:
        """A tmp-family root plus an origin repository with one commit outside it."""
        self.fam = gd.Family("tmp", self.root / "tmp", True, "scratch", files=True,
                             min_bytes=0, git_disposable=True)
        self.fam.root.mkdir()
        origin = self.root / "origin"
        # these stand in for a network server; any other local remote vouches for nothing
        gd.TRUSTED_LOCAL_REMOTES = (str(origin), str(self.root / "subsrc"))
        self.addCleanup(setattr, gd, "TRUSTED_LOCAL_REMOTES", ())
        self._git("init", "-q", "-b", "main", str(origin))
        (origin / "f").write_text("x")
        self._git("-C", str(origin), "add", "f")
        self._git("-C", str(origin), "commit", "-qm", "c")
        return self.fam.root, origin

    def test_tmpfs_is_judged_by_its_own_thresholds(self) -> None:
        mounts = self.root / "mounts"
        mounts.write_text(f"tmpfs {self.root} tmpfs rw 0 0\n")
        saved = gd.mount_point
        gd.mount_point = lambda p: self.root
        try:
            self.assertEqual(gd.fs_type(self.root, mounts), "tmpfs")
            mounts.write_text(f"/dev/x {self.root} ext4 rw 0 0\n")
            self.assertEqual(gd.fs_type(self.root, mounts), "ext4")
            self.assertEqual(gd.fs_type(self.root, self.root / "missing"), "")
        finally:
            gd.mount_point = saved
        saved_type, saved_darwin = gd.fs_type, gd.DARWIN
        gd.fs_type, gd.DARWIN = (lambda p, *_: "tmpfs"), False
        try:
            fs = next(iter(gd.filesystems([self.fam], "0", "0", "100%", "100%").values()))
        finally:
            gd.fs_type, gd.DARWIN = saved_type, saved_darwin
        self.assertTrue(fs.tmpfs)
        self.assertTrue(fs.under)  # free space alone would say no pressure with low "0"
        self.assertEqual(fs.target, fs.total)

    def test_tmp_files_are_candidates_and_removed(self) -> None:
        root, _ = self._tmp_repos()
        (root / "leaked.so").write_bytes(b"\0" * 4096)
        (root / "fresh.log").write_bytes(b"\0" * 4096)
        self._age(root / "leaked.so")
        v = {Path(i.path).name: i.verdict for i in gd.survey([self.fam], 24, 0)}
        self.assertEqual(v, {"leaked.so": "reclaimable", "fresh.log": "recent"})
        plain = gd.Family("tmp", root, True, "scratch")
        self.assertEqual(gd.survey([plain], 24, 0), [])  # files only when the family opts in
        gd.apply(gd.survey([self.fam], 24, 0), {"tmp": self.fam}, self.receipt(), None, 24)
        self.assertFalse((root / "leaked.so").exists())
        self.assertTrue((root / "fresh.log").exists())

    def test_nested_checkouts_in_idle_scratch_sessions(self) -> None:
        root, origin = self._tmp_repos()
        self.fam = gd.replace(self.fam, files=False, nested_git=True)
        for session in ("clean", "unpushed", "dirty"):
            (root / session / "scratchpad").mkdir(parents=True)
            self._git("clone", "-q", str(origin), str(root / session / "scratchpad/a"))
            self._git("clone", "-q", str(origin), str(root / session / "scratchpad/b"))
            (root / session / "scratchpad/notes.txt").write_text("n")
        (root / "unpushed/scratchpad/b/g").write_text("y")
        self._git("-C", str(root / "unpushed/scratchpad/b"), "add", "g")
        self._git("-C", str(root / "unpushed/scratchpad/b"), "commit", "-qm", "local")
        (root / "dirty/scratchpad/a/untracked").write_text("z")
        # a bare seed with a linked worktree: the branch lives only in the seed, inside the item
        (root / "seeded/scratchpad").mkdir(parents=True)
        self._git("clone", "-q", "--bare", str(origin), str(root / "seeded/scratchpad/seed.git"))
        self._git("-C", str(root / "seeded/scratchpad/seed.git"), "worktree", "add", "-q", "-b", "feat",
                  str(root / "seeded/scratchpad/wt"))
        (root / "seeded/scratchpad/wt/g").write_text("y")
        self._git("-C", str(root / "seeded/scratchpad/wt"), "add", "g")
        self._git("-C", str(root / "seeded/scratchpad/wt"), "commit", "-qm", "only in the seed")
        # a bare copy whose commits a remote confirms loses nothing; one with a local commit does
        (root / "bare/scratchpad").mkdir(parents=True)
        self._git("clone", "-q", "--bare", str(origin), str(root / "bare/scratchpad/copy.git"))
        (root / "barelocal/scratchpad").mkdir(parents=True)
        seed = root / "barelocal/scratchpad/seed"
        self._git("clone", "-q", "--bare", str(origin), str(seed))
        tree = self._git("--git-dir", str(seed), "rev-parse", "HEAD^{tree}").strip()
        local = self._git("--git-dir", str(seed), "commit-tree", tree, "-p", "HEAD", "-m", "local").strip()
        self._git("--git-dir", str(seed), "update-ref", "refs/heads/main", local)
        # a clone kept in an ignored directory of a clean checkout is judged too
        (root / "vendored/scratchpad").mkdir(parents=True)
        outer = root / "vendored/scratchpad/outer"
        self._git("clone", "-q", str(origin), str(outer))
        (outer / ".git/info/exclude").write_text("vendor/\n")
        (outer / "vendor").mkdir()
        self._git("clone", "-q", str(origin), str(outer / "vendor/lib"))
        (outer / "vendor/lib/g").write_text("y")
        self._git("-C", str(outer / "vendor/lib"), "add", "g")
        self._git("-C", str(outer / "vendor/lib"), "commit", "-qm", "vendored and unpushed")
        # checkouts nested deeper than git_state's search are still found and judged
        deep = root / "deep/scratchpad/a/b/c/d/e/f"
        deep.mkdir(parents=True)
        self._git("clone", "-q", str(origin), str(deep / "clone"))
        (deep / "clone/g").write_text("y")
        self._git("-C", str(deep / "clone"), "add", "g")
        self._git("-C", str(deep / "clone"), "commit", "-qm", "deep and unpushed")
        for d in root.iterdir():
            self._age(d)
        items = gd.survey([self.fam], 24, 0)
        v = {Path(i.path).name: i.verdict for i in items}
        self.assertEqual(v, {"clean": "reclaimable", "unpushed": "git-checkout", "dirty": "git-checkout",
                             "seeded": "git-checkout", "bare": "reclaimable", "barelocal": "git-checkout",
                             "deep": "git-checkout", "vendored": "git-checkout"})
        why = {Path(i.path).name: i.reasons for i in items}
        self.assertEqual(why["unpushed"], ["scratchpad/b: commits no remote confirms"])
        self.assertEqual(why["barelocal"], ["scratchpad/seed: commits no remote confirms"])
        self.assertEqual(why["deep"], ["scratchpad/a/b/c/d/e/f/clone: commits no remote confirms"])
        self.assertEqual(why["vendored"], ["scratchpad/outer: vendor/lib: commits no remote confirms"])
        # kept verdicts are cached, so a dead session with unpushed work is not re-walked every run
        self.assertIn(str(root / "deep"), json.loads(gd.NESTED_KEEP.read_text()))
        # a checkout rooted below the item stays protected without the opt-in
        plain = gd.replace(self.fam, nested_git=False)
        self.assertLessEqual({i.verdict for i in gd.survey([plain], 24, 0)}, {"git-checkout", "unchecked"})
        gd.apply(items, {"tmp": self.fam}, self.receipt(), None, 24)
        self.assertEqual(sorted(p.name for p in root.iterdir()),
                         ["barelocal", "deep", "dirty", "seeded", "unpushed", "vendored"])

    def test_family_size_floor_overrides_min_mib(self) -> None:
        root, _ = self._tmp_repos()
        (root / "small").write_bytes(b"\0" * 4096)
        self._age(root / "small")
        self.assertEqual(len(gd.survey([self.fam], 24, 1 << 30)), 1)
        self.fam = gd.replace(self.fam, min_bytes=None)
        self.assertEqual(gd.survey([self.fam], 24, 1 << 30), [])

    def test_disposable_checkouts_in_tmp(self) -> None:
        root, origin = self._tmp_repos()
        self._git("clone", "-q", str(origin), str(root / "pushed"))
        self._git("clone", "-q", str(origin), str(root / "unpushed"))
        (root / "unpushed/g").write_text("y")
        self._git("-C", str(root / "unpushed"), "add", "g")
        self._git("-C", str(root / "unpushed"), "commit", "-qm", "local")
        self._git("clone", "-q", str(origin), str(root / "dirty"))
        (root / "dirty/untracked").write_text("z")
        self._git("clone", "-q", str(origin), str(root / "stashed"))
        (root / "stashed/f").write_text("changed")
        self._git("-C", str(root / "stashed"), "stash", "-q")
        (root / "nested").mkdir()
        self._git("clone", "-q", str(origin), str(root / "nested/inner"))
        self._git("-C", str(origin), "worktree", "add", "-q", "-b", "wt", str(root / "worktree"))
        self._git("-C", str(origin), "worktree", "add", "-q", "--detach", str(root / "detached"))
        (root / "detached/h").write_text("w")
        self._git("-C", str(root / "detached"), "add", "h")
        self._git("-C", str(root / "detached"), "commit", "-qm", "orphan")
        self._git("clone", "-q", str(origin), str(root / "young"))
        for d in root.iterdir():
            if d.name != "young":
                self._age(d)
        items = gd.survey([self.fam], 24, 0)
        v = {Path(i.path).name: i.verdict for i in items}
        self.assertEqual(v, {"pushed": "reclaimable", "worktree": "reclaimable",
                             "unpushed": "git-checkout", "dirty": "git-checkout",
                             "stashed": "git-checkout", "nested": "git-checkout",
                             "detached": "git-checkout", "young": "git-checkout"})
        why = {Path(i.path).name: i.reasons for i in items}
        self.assertEqual(why["unpushed"], ["commits no remote confirms"])
        self.assertEqual(why["detached"], ["HEAD on no ref"])
        # without the opt-in every checkout stays protected
        plain = gd.replace(self.fam, git_disposable=False)
        self.assertEqual({i.verdict for i in gd.survey([plain], 24, 0)}, {"git-checkout"})
        gd.apply(items, {"tmp": self.fam}, self.receipt(), None, 24)
        self.assertEqual(sorted(p.name for p in root.iterdir()),
                         ["detached", "dirty", "nested", "stashed", "unpushed", "young"])
        listed = self._git("-C", str(origin), "worktree", "list", "--porcelain")
        self.assertNotIn(str(root / "worktree"), listed)  # pruned from its repository
        self.assertIn("refs/heads/wt", self._git("-C", str(origin), "for-each-ref"))

    def test_checkouts_with_hidden_git_state_are_kept(self) -> None:
        root, origin = self._tmp_repos()
        # a clean, pushed clone that backs a worktree with an unpushed detached commit
        self._git("clone", "-q", str(origin), str(root / "main"))
        self._git("-C", str(root / "main"), "worktree", "add", "-q", "--detach",
                  str(self.root / "elsewhere"))
        # a pushed superproject whose submodule commit exists only locally
        sub = self.root / "subsrc"
        self._git("clone", "-q", str(origin), str(sub))
        self._git("clone", "-q", str(origin), str(root / "super"))
        self._git("-C", str(root / "super"), "-c", "protocol.file.allow=always", "submodule",
                  "add", "-q", str(sub), "sm")
        (root / "super/sm/local").write_text("only here")
        self._git("-C", str(root / "super/sm"), "add", "local")
        self._git("-C", str(root / "super/sm"), "commit", "-qm", "local")
        self._git("-C", str(root / "super"), "add", "sm")
        self._git("-C", str(root / "super"), "commit", "-qm", "sm")
        self._git("-C", str(root / "super"), "push", "-q", "origin", "HEAD:refs/heads/super")
        self._git("-C", str(root / "super"), "fetch", "-q")
        # an edit hidden from status by skip-worktree
        self._git("clone", "-q", str(origin), str(root / "skipped"))
        self._git("-C", str(root / "skipped"), "update-index", "--skip-worktree", "f")
        (root / "skipped/f").write_text("edited")
        # a worktree whose commit only a worktree-local ref holds, and a locked one
        self._git("-C", str(origin), "worktree", "add", "-q", "--detach", str(root / "bisect"))
        (root / "bisect/b").write_text("b")
        self._git("-C", str(root / "bisect"), "add", "b")
        self._git("-C", str(root / "bisect"), "commit", "-qm", "b")
        self._git("-C", str(root / "bisect"), "update-ref", "refs/bisect/bad", "HEAD")
        self._git("-C", str(origin), "worktree", "add", "-q", "-b", "lk", str(root / "locked"))
        self._git("-C", str(origin), "worktree", "lock", str(root / "locked"))
        for d in root.iterdir():
            self._age(d)
        why = {Path(i.path).name: (i.verdict, i.reasons) for i in gd.survey([self.fam], 24, 0)}
        self.assertEqual(why, {
            "main": ("git-checkout", ["other worktrees use this repository"]),
            "super": ("git-checkout", ["submodule sm: commits no remote confirms"]),
            "skipped": ("git-checkout", ["files hidden from status"]),
            "bisect": ("git-checkout", ["HEAD on no ref"]),
            "locked": ("git-checkout", ["locked worktree"])})

    def test_submodules_are_judged_not_vetoed(self) -> None:
        root, origin = self._tmp_repos()
        sub = self.root / "subsrc"
        self._git("clone", "-q", str(origin), str(sub))
        add = ("-c", "protocol.file.allow=always", "submodule", "add", "-q", str(sub), "sm")

        def superproject(name: str, worktree: bool = False) -> Path:
            d = root / name
            if worktree:
                self._git("-C", str(origin), "worktree", "add", "-q", "-b", name, str(d))
            else:
                self._git("clone", "-q", str(origin), str(d))
            self._git("-C", str(d), *add)
            self._git("-C", str(d), "commit", "-qm", "sm")
            if not worktree:
                self._git("-C", str(d), "push", "-q", "origin", f"HEAD:refs/heads/{name}")
                self._git("-C", str(d), "fetch", "-q")
            return d

        superproject("pushed")  # submodule at a commit its remote has
        # pinned at a commit fetched by id: on the remote, but no remote-tracking ref has it
        extra = self.root / "extra"
        self._git("clone", "-q", str(sub), str(extra))
        (extra / "e").write_text("e")
        self._git("-C", str(extra), "add", "e")
        self._git("-C", str(extra), "commit", "-qm", "e")
        pinned = self._git("-C", str(extra), "rev-parse", "HEAD").strip()
        self._git("-C", str(extra), "push", "-q", "origin", "HEAD:refs/pinned/e")  # no branch
        d = superproject("fetched")
        self._git("-C", str(d / "sm"), "-c", "uploadpack.allowAnySHA1InWant=true", "fetch", "-q",
                  "origin", pinned)
        self._git("-C", str(d / "sm"), "checkout", "-q", pinned)
        self.assertTrue(self._git("-C", str(d / "sm"), "rev-list", "HEAD", "--not", "--remotes"))
        self._git("-C", str(d), "add", "sm")
        self._git("-C", str(d), "commit", "-qm", "pin")
        self._git("-C", str(d), "push", "-q", "origin", "HEAD:refs/heads/fetched")
        self._git("-C", str(d), "fetch", "-q")
        superproject("wt-pushed", worktree=True)
        d = superproject("dirty")
        (d / "sm/untracked").write_text("u")
        d = superproject("stashed")
        (d / "sm/f").write_text("changed")
        self._git("-C", str(d / "sm"), "stash", "-q")
        d = superproject("wt-local", worktree=True)  # modules live in the worktree's git dir
        (d / "sm/g").write_text("g")
        self._git("-C", str(d / "sm"), "add", "g")
        self._git("-C", str(d / "sm"), "commit", "-qm", "g")
        self._git("-C", str(d), "add", "sm")
        self._git("-C", str(d), "commit", "-qm", "bump")
        for c in root.iterdir():
            self._age(c)
        why = {Path(i.path).name: (i.verdict, i.reasons) for i in gd.survey([self.fam], 24, 0)}
        self.assertEqual(why, {
            "pushed": ("reclaimable", []),
            "fetched": ("reclaimable", []),
            "wt-pushed": ("reclaimable", []),
            "dirty": ("git-checkout", ["uncommitted or untracked changes"]),
            "stashed": ("git-checkout", ["submodule sm: stash entries"]),
            "wt-local": ("git-checkout", ["submodule sm: commits no remote confirms"])})

    def test_submodule_work_the_remote_lacks_is_kept(self) -> None:
        """The #1149 review repros: local submodule commits with no telltale reflog entry."""
        root, origin = self._tmp_repos()
        sub = self.root / "subsrc"
        self._git("clone", "-q", str(origin), str(sub))
        file_ok = ("-c", "protocol.file.allow=always")

        def superproject(name: str) -> Path:
            d = root / name
            self._git("clone", "-q", str(origin), str(d))
            self._git("-C", str(d), *file_ok, "submodule", "add", "-q", str(sub), "sm")
            self._git("-C", str(d), "commit", "-qm", "sm")
            self._git("-C", str(d), "push", "-q", "origin", f"HEAD:refs/heads/{name}")
            self._git("-C", str(d), "fetch", "-q")
            return d

        def commit(repo: Path, msg: str) -> str:
            self._git("-C", str(repo), "commit", "-q", "--allow-empty", "-m", msg)
            return self._git("-C", str(repo), "rev-parse", "HEAD").strip()

        d = superproject("orphaned")  # detached commit, then `submodule update` moves away
        commit(d / "sm", "local")
        self._git("-C", str(d), *file_ok, "submodule", "update", "-q")
        d = superproject("commit-tree")  # no commit-shaped reflog entry, only "branch: Created"
        c = self._git("-C", str(d / "sm"), "commit-tree", "HEAD^{tree}", "-p", "HEAD",
                      "-m", "ct").strip()
        self._git("-C", str(d / "sm"), "branch", "keep", c)
        d = superproject("expired")  # reflog expired
        self._git("-C", str(d / "sm"), "checkout", "-q", "-b", "feat")
        commit(d / "sm", "local")
        self._git("-C", str(d / "sm"), "checkout", "-q", "--detach", "HEAD~1")
        self._git("-C", str(d / "sm"), "reflog", "expire", "--expire=now", "--all")
        d = superproject("embedded")  # the submodule's .git is a directory in the tree
        self._git("-C", str(d), "submodule", "deinit", "-q", "-f", "sm")
        gitdir = self._git("-C", str(d), "rev-parse", "--path-format=absolute", "--git-dir")
        shutil.rmtree(Path(gitdir.strip()) / "modules/sm")
        shutil.rmtree(d / "sm")
        self._git("clone", "-q", str(sub), str(d / "sm"))
        self._git("-C", str(d / "sm"), "checkout", "-q", "-b", "feat")
        commit(d / "sm", "local")
        self._git("-C", str(d / "sm"), "checkout", "-q", "--detach", "origin/HEAD")
        d = superproject("sub-worktree")  # the submodule backs a worktree elsewhere
        self._git("-C", str(d / "sm"), "worktree", "add", "-q", "--detach",
                  str(self.root / "sub-wt"))
        d = superproject("local-fetch")  # commits fetched from another scratch repo
        other = self.root / "other"
        self._git("clone", "-q", str(sub), str(other))
        commit(other, "other")
        self._git("-C", str(d / "sm"), "fetch", "-q", str(other), "HEAD:refs/heads/fromother")
        for c in root.iterdir():
            self._age(c)
        why = {Path(i.path).name: (i.verdict, i.reasons) for i in gd.survey([self.fam], 24, 0)}
        confirm = ["submodule sm: commits no remote confirms"]
        self.assertEqual(why, {
            "orphaned": ("git-checkout", confirm),
            "commit-tree": ("git-checkout", confirm),
            "expired": ("git-checkout", confirm),
            "embedded": ("git-checkout", confirm),
            "sub-worktree": ("git-checkout", ["submodule sm: has worktrees of its own"]),
            "local-fetch": ("git-checkout", confirm)})

    def test_local_remotes_never_vouch_for_commits(self) -> None:
        root, origin = self._tmp_repos()
        a, b = root / "a", root / "b"
        self._git("clone", "-q", str(origin), str(a))
        self._git("-C", str(a), "commit", "-q", "--allow-empty", "-m", "only here")
        self._git("clone", "-q", str(a), str(b))  # b's origin is a
        self._git("-C", str(a), "remote", "add", "b", str(b))
        self._git("-C", str(a), "fetch", "-q", "b")
        for c in (a, b):
            self._age(c)
        v = {Path(i.path).name: (i.verdict, i.reasons) for i in gd.survey([self.fam], 24, 0)}
        self.assertEqual(v, {"a": ("git-checkout", ["commits no remote confirms"]),
                             "b": ("git-checkout", ["commits no remote confirms"])})
        self.assertEqual(gd.network_remotes(a / ".git"), ["origin"])  # not the sibling b

    def test_network_remote_urls(self) -> None:
        for url in ("https://github.com/o/r.git", "ssh://git@h/o/r", "git@github.com:o/r.git",
                    "big-red:Projects/x", "git://h/r"):
            self.assertTrue(gd.NETWORK_URL.match(url), url)
        for url in ("/tmp/x", "../b", "./b", "file:///tmp/x", "b", "/c/x"):
            self.assertFalse(gd.NETWORK_URL.match(url) and not url.startswith("file:"), url)

    def test_worktree_reflog_only_commit_is_kept(self) -> None:
        root, origin = self._tmp_repos()
        wt = root / "wt"
        self._git("-C", str(origin), "worktree", "add", "-q", "-b", "wt", str(wt))
        self._git("-C", str(wt), "checkout", "-q", "--detach")
        self._git("-C", str(wt), "commit", "-q", "--allow-empty", "-m", "made here")
        self._git("-C", str(wt), "checkout", "-q", "wt")
        self._age(wt)
        item = gd.survey([self.fam], 24, 0)[0]
        self.assertEqual((item.verdict, item.reasons),
                         ("git-checkout", ["commits only this worktree's reflog holds"]))

    def test_apply_rechecks_a_checkout_that_gained_work(self) -> None:
        root, origin = self._tmp_repos()
        self._git("clone", "-q", str(origin), str(root / "c"))
        self._age(root / "c")
        items = gd.survey([self.fam], 24, 0)
        self.assertEqual(items[0].verdict, "reclaimable")
        (root / "c/new").write_text("n")
        self._age(root / "c")
        receipt = self.receipt()
        gd.apply(items, {"tmp": self.fam}, receipt, None, 24)
        self.assertTrue((root / "c").exists())
        self.assertIn("changed:git", receipt.read_text())

    def test_cargo_target_search_is_report_only(self) -> None:
        fam = gd.Family("cargo-target", self.root, False, "cargo clean", depth=4,
                        match="target", marker="CACHEDIR.TAG")
        for rel in ("glaeda/target", "wts/branch/target", "mono/crates/cli/target"):
            make(self.root / rel)
            (self.root / rel / "CACHEDIR.TAG").write_text("Signature: 8a477f597d28d172789f06886806bc55")
        make(self.root / "site/target")  # not Cargo: no CACHEDIR.TAG
        (self.root / "web/node_modules/pkg/target").mkdir(parents=True)
        (self.root / "web/node_modules/pkg/target/CACHEDIR.TAG").write_text("x")
        (self.root / "glaeda/target/debug/target").mkdir(parents=True)  # never searched inside a match
        (self.root / "glaeda/target/debug/target/CACHEDIR.TAG").write_text("x")
        found = sorted(str(p.relative_to(self.root)) for p in gd.candidates(fam))
        self.assertEqual(found, ["glaeda/target", "mono/crates/cli/target", "wts/branch/target"])
        items = gd.survey([fam], 24, 0)
        self.assertEqual({i.verdict for i in items}, {"report-only"})
        receipt = self.receipt()
        gd.apply(items, {fam.id: fam}, receipt, None, 24)
        self.assertTrue((self.root / "glaeda/target").exists())
        self.assertFalse(receipt.exists())


class LinuxLayoutTest(unittest.TestCase):
    """The Linux layout, checked on every platform by pointing HOME and DARWIN at a fake host."""

    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.home = Path(os.path.realpath(self.tmp.name))
        self.saved = (gd.HOME, gd.DARWIN)
        gd.HOME, gd.DARWIN = self.home, False

    def tearDown(self) -> None:
        gd.HOME, gd.DARWIN = self.saved
        gd.UNREADABLE.clear()
        self.tmp.cleanup()

    def test_linux_families(self) -> None:
        for rel in ("Projects/glaeda", "Projects/glaeda-worktrees/a", "Projects/botany-sim-worktrees/b",
                    ".cache/pip"):
            (self.home / rel).mkdir(parents=True)
        fams = gd.default_families()
        by_id: dict[str, list[Path]] = {}
        for f in fams:
            by_id.setdefault(f.id, []).append(f.root)
        self.assertEqual(by_id["tmp"], [Path("/tmp")])
        self.assertNotIn("library-caches", by_id)
        self.assertNotIn("xcode-derived-data", by_id)
        self.assertEqual(sorted(p.name for p in by_id["worktrees"]),
                         ["botany-sim-worktrees", "glaeda-worktrees"])
        reclaimable = {f.id for f in fams if f.reclaimable}
        self.assertTrue(reclaimable <= {"tmp", "claude-scratchpad"})
        projects = next(f for f in fams if f.id == "projects")
        self.assertIn("botany-sim-worktrees", projects.skip)
        tmp = next(f for f in fams if f.id == "tmp")
        self.assertIn(f"claude-{os.getuid()}", tmp.skip)
        # scratch clones and leaked files are judged on any filesystem, not only a tmpfs
        self.assertTrue(tmp.files and tmp.git_disposable)

    def test_cmux_job_units_go_but_unpushed_checkouts_stay(self) -> None:
        job = self.home / ".cache/cmux-job"
        # as on cmux13s: SwiftPM clones inside DerivedData, and each slot's clone one level below cmux-base
        (job / "reload-cloud-ios/DerivedData/slot-1/Build").mkdir(parents=True)
        (job / "reload-cloud-ios/DerivedData/slot-1/Build/x.o").write_bytes(b"\0" * 4096)
        (job / "reload-cloud-ios/DerivedData/slot-1/SourcePackages/checkouts/dep/.git").mkdir(parents=True)
        base = job / "reload-cloud/cmux-base/slot-1"
        env = {**os.environ, "GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@t",
               "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@t"}
        subprocess.run(["git", "init", "-q", str(base)], check=True, env=env)
        (base / "f").write_text("x")
        subprocess.run(["git", "-C", str(base), "add", "f"], check=True, env=env)
        subprocess.run(["git", "-C", str(base), "commit", "-qm", "local only"], check=True, env=env)
        old = time.time() - 48 * 3600
        for dirpath, dirnames, filenames in os.walk(job):
            for n in dirnames + filenames:
                os.utime(os.path.join(dirpath, n), (old, old), follow_symlinks=False)
        saved = gd.process_evidence
        gd.process_evidence = lambda: ([], "")
        try:
            fams = [f for f in gd.default_families() if f.id in ("cmux-job-cache", "user-cache")]
            items = gd.survey(fams, 24, 0)
        finally:
            gd.process_evidence = saved
        verdicts = {Path(i.path).relative_to(self.home).as_posix(): (i.family, i.verdict) for i in items}
        self.assertEqual(verdicts[".cache/cmux-job/reload-cloud-ios/DerivedData"], ("cmux-job-cache", "reclaimable"))
        # a commit no remote holds keeps its checkout
        self.assertEqual(verdicts[".cache/cmux-job/reload-cloud/cmux-base"], ("cmux-job-cache", "git-checkout"))
        self.assertNotIn(".cache/cmux-job", verdicts)  # not listed again as a report-only tool cache
        receipt = self.home / "receipt.jsonl"
        fams_by_id = {f.id: f for f in fams}
        with contextlib.redirect_stdout(io.StringIO()):
            saved = gd.process_evidence
            gd.process_evidence = lambda: ([], "")
            try:
                gd.apply(items, fams_by_id, receipt, None, 24)
            finally:
                gd.process_evidence = saved
        self.assertFalse((job / "reload-cloud-ios/DerivedData").exists())
        self.assertEqual([c.name for c in (job / "reload-cloud-ios").iterdir()], [])  # no half-deleted leftover
        self.assertTrue((base / ".git").is_dir())
        # a delete interrupted after its rename leaves a tree nothing else will ever clean up
        left = job / "reload-cloud-ios/.glaeda-disk-deleting-DerivedData-123"
        (left / "slot-1/SourcePackages/checkouts/dep/.git").mkdir(parents=True)
        (left / "slot-1/x.o").write_bytes(b"\0" * 4096)
        for dirpath, dirnames, filenames in os.walk(left):
            for n in dirnames + filenames:
                os.utime(os.path.join(dirpath, n), (old, old), follow_symlinks=False)
        os.utime(left, (old, old))
        saved = gd.process_evidence
        gd.process_evidence = lambda: ([], "")
        try:
            again = {i.path: i.verdict for i in gd.survey(fams, 24, 0)}
        finally:
            gd.process_evidence = saved
        self.assertEqual(again[str(left)], "reclaimable")

    def test_claude_session_seen_in_alternate_config_dir(self) -> None:
        t = self.home / ".claude-outlook/projects/-home-leo-Projects/abc-123.jsonl"
        t.parent.mkdir(parents=True)
        t.write_text("{}")
        self.assertTrue(gd.claude_session_active("abc-123", 3600))
        self.assertFalse(gd.claude_session_active("other", 3600))

    def test_unreadable_message_names_no_full_disk_access_on_linux(self) -> None:
        gd.UNREADABLE.add("/tmp/locked")
        err = io.StringIO()
        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(err):
            gd.report([], [], 5, 24)
        self.assertIn("cannot read /tmp/locked", err.getvalue())
        self.assertNotIn("Full Disk Access", err.getvalue())

    def test_linux_paths_have_one_spelling(self) -> None:
        self.assertEqual(gd.spellings("/tmp/claude-1000/x"), ["/tmp/claude-1000/x"])
        self.assertTrue(gd.named_by("/tmp/claude-1000/x", "cargo build --target-dir /tmp/claude-1000/x/t\n"))


@unittest.skipUnless(sys.platform.startswith("linux"), "reads /proc")
class ProcEvidenceTest(unittest.TestCase):
    def test_proc_evidence_sees_own_cwd(self) -> None:
        opened, cmds = gd.process_evidence()
        self.assertIn(os.getcwd(), opened)
        self.assertTrue(cmds.strip())


@unittest.skipUnless(sys.platform == "darwin", "APFS clones are macOS only")
class DedupeTest(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory(dir=os.path.expanduser("~"))
        self.root = Path(self.tmp.name)
        self.state = self.root / "state.json"

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def blob(self, rel: str, data: bytes, mode: int = 0o644, mtime: float = 1_000_000) -> Path:
        p = self.root / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_bytes(data)
        os.chmod(p, mode)
        os.utime(p, (mtime, mtime))
        return p

    def test_identical_files_are_cloned_keeping_mode_and_mtime(self) -> None:
        data = os.urandom(2 * 1024 * 1024)
        a = self.blob("a/SourcePackages/x.bin", data)
        b = self.blob("b/SourcePackages/x.bin", data, mode=0o755, mtime=2_000_000)
        c = self.blob("c/SourcePackages/x.bin", os.urandom(len(data)))
        ino_b = b.stat().st_ino
        r = gd.dedupe([self.root / "a", self.root / "b", self.root / "c"], self.state)
        self.assertEqual(r["cloned_bytes"], len(data))
        self.assertEqual(b.read_bytes(), data)
        self.assertNotEqual(b.stat().st_ino, ino_b)
        self.assertEqual(oct(b.stat().st_mode & 0o777), oct(0o755))
        self.assertEqual(int(b.stat().st_mtime), 2_000_000)
        self.assertNotEqual(c.read_bytes(), data)
        self.assertEqual(a.read_bytes(), data)
        again = gd.dedupe([self.root / "a", self.root / "b", self.root / "c"], self.state)
        self.assertEqual((again["hashed_bytes"], again["cloned_bytes"]), (0, 0))
        self.assertEqual([p.name for p in self.root.rglob("*glaeda-clone*")], [])

    def test_target_changed_after_hash_is_refused(self) -> None:
        data = os.urandom(2 * 1024 * 1024)
        a = self.blob("a/x.bin", data)
        b = self.blob("b/x.bin", data)
        key = gd._file_key(os.lstat(b))
        os.utime(b, (3_000_000, 3_000_000))
        self.assertFalse(gd.clone_over(str(a), str(b), key))
        self.assertEqual(int(b.stat().st_mtime), 3_000_000)

    def test_canonical_changed_after_hash_is_refused(self) -> None:
        data = os.urandom(2 * 1024 * 1024)
        a = self.blob("a/x.bin", data)
        b = self.blob("b/x.bin", data)
        akey, bkey = gd._file_key(os.lstat(a)), gd._file_key(os.lstat(b))
        a.write_bytes(os.urandom(len(data)))
        self.assertFalse(gd.clone_over(str(a), str(b), bkey, akey))
        self.assertEqual(b.read_bytes(), data)

    def test_recently_written_files_wait_for_a_later_pass(self) -> None:
        data = os.urandom(2 * 1024 * 1024)
        self.blob("a/x.bin", data)
        self.blob("b/x.bin", data, mtime=time.time())
        r = gd.dedupe([self.root / "a", self.root / "b"], self.state, settle_s=600)
        self.assertEqual((r["young"], r["cloned_bytes"]), (1, 0))

    def test_small_and_unique_files_are_ignored(self) -> None:
        self.blob("a/small", b"x" * 10)
        self.blob("b/small", b"x" * 10)
        r = gd.dedupe([self.root / "a", self.root / "b"], self.state)
        self.assertEqual((r["files"], r["cloned_bytes"]), (0, 0))


if __name__ == "__main__":
    unittest.main()
