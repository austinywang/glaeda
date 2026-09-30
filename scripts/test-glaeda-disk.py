#!/usr/bin/env python3
"""Contract tests for scripts/glaeda-disk."""

from __future__ import annotations

import importlib.machinery
import importlib.util
import contextlib
import fcntl
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
from unittest import mock
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
        gd._DISCOVERED.clear()
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

    @staticmethod
    def age(*tops: Path, hours: float = 48) -> None:
        old = time.time() - hours * 3600
        for top in tops:
            for dirpath, dirs, files in os.walk(top):
                for n in dirs + files:
                    os.utime(os.path.join(dirpath, n), (old, old), follow_symlinks=False)
            os.utime(top, (old, old))

    @staticmethod
    def hex_store(root: Path, shards: int = 20, files: int = 5) -> Path:
        for i in range(shards):
            shard = root / f"{i + 0xa0:02x}"
            shard.mkdir(parents=True)
            for j in range(files):
                (shard / f"{i:02x}{j:062x}-a").write_bytes(b"x")
        return root

    def test_sealed_caches_are_recognized_by_content_not_name(self) -> None:
        # The Go build cache outgrew git_state's budget and stayed "unchecked" forever. A tree whose
        # layout a tool writes is sealed: only its top two levels are searched, whatever its name.
        go = self.hex_store(self.root / "whatever-name")
        self.assertEqual(gd.cache_layout(go), "hash-sharded-store")
        self.assertEqual(gd.git_state(go, budget=50, sealed=True), "none")
        # a family that deletes disposable checkouts (tmp, scratch) never takes the shortcut
        self.assertEqual(gd.git_state(go, budget=50), "unchecked")
        plain = self.root / "plain"
        for i in range(20):
            (plain / f"d{i}").mkdir(parents=True)
            for j in range(5):
                (plain / f"d{i}" / f"f{j}").write_bytes(b"x")
        self.assertEqual(gd.git_state(plain, budget=50, sealed=True), "unchecked")
        chrome = self.root / "Browser/Default/Cache/Cache_Data"
        (chrome / "index-dir").mkdir(parents=True)
        (chrome / "index-dir/the-real-index").write_bytes(b"x")
        # an index and a data_0 are any dataset's names; Chromium's blockfile index carries its magic
        dataset = self.root / "dataset"
        dataset.mkdir()
        for n in ("index", "data_0", "data_1", "data_2", "data_3"):
            (dataset / n).write_bytes(b"rows")
        self.assertEqual(gd.cache_layout(dataset), "")
        (dataset / "index").write_bytes(gd.CHROMIUM_INDEX_MAGIC + b"rest")
        self.assertEqual(gd.cache_layout(dataset), "chromium-disk-cache")
        for j in range(100):
            (chrome / f"{j:016x}_0").write_bytes(b"x")
        self.assertEqual(gd.cache_layout(chrome), "chromium-disk-cache")
        self.assertEqual(gd.git_state(self.root / "Browser", budget=50, sealed=True), "none")
        tagged = self.root / "tagged"
        (tagged / "a/b/c/d/e/f/g").mkdir(parents=True)
        self.assertEqual(gd.git_state(tagged, sealed=True), "unchecked")
        (tagged / "CACHEDIR.TAG").write_bytes(gd.CACHEDIR_TAG + b"\n")
        self.assertEqual(gd.git_state(tagged, sealed=True), "none")
        # a FIFO carrying the name is never opened (the read would block)
        fifo = self.root / "fifo"
        (fifo / "a/b/c/d/e/f/g").mkdir(parents=True)
        os.mkfifo(fifo / "CACHEDIR.TAG")
        self.assertEqual(gd.cache_layout(fifo), "")
        self.assertEqual(gd.git_state(fifo, sealed=True), "unchecked")
        # ~/.cache/bazel on Linux: an output user root whose bases hold bazel's own fetched clones
        bazel = self.root / "bazel/_bazel_me"
        (bazel / "install/abc").mkdir(parents=True)
        (bazel / ("0" * 31 + "a") / "external/rules_x/.git").mkdir(parents=True)
        self.assertEqual(gd.cache_layout(bazel), "bazel-output-root")
        self.assertEqual(gd.git_state(self.root / "bazel", sealed=True), "none")
        (self.root / "notbazel/install").mkdir(parents=True)
        (self.root / "notbazel" / ("0" * 31 + "a") / "src/repo/.git").mkdir(parents=True)
        self.assertEqual(gd.git_state(self.root / "notbazel", sealed=True), "git")
        # numbered directories (Maven versions) are not hex shards
        maven = self.root / "maven"
        for v in range(10, 30):
            (maven / str(v)).mkdir(parents=True)
            (maven / str(v) / "x.pom").write_bytes(b"x")
        self.assertEqual(gd.cache_layout(maven), "")
        # shards are only sampled to recognize the store; every shard is still read, so a worktree
        # in the last one is found
        mixed = self.hex_store(self.root / "mixed", files=40)
        (mixed / "b3/worktree/.git").mkdir(parents=True)
        self.assertEqual(gd.git_state(mixed, budget=50, sealed=True), "git")
        (mixed / "b3/worktree/.git").rmdir()
        self.assertEqual(gd.git_state(mixed, budget=50, sealed=True), "none")

    def test_a_clone_at_the_top_of_a_sealed_cache_is_still_kept(self) -> None:
        # sealing skips the deep search, not the shallow one: a person's clone at the top of a cache,
        # or one level down, is never deleted without a look; a clone deeper in the blobs is the tool's.
        self.fam = gd.Family("user-cache", self.root, True, "re-download")
        tag = gd.CACHEDIR_TAG + b"\n"
        top = make(self.root / "top")
        (top / "CACHEDIR.TAG").write_bytes(tag)
        (top / ".git").mkdir()
        nested = make(self.root / "nested")
        (nested / "CACHEDIR.TAG").write_bytes(tag)
        (nested / "repo/.git").mkdir(parents=True)
        deep = make(self.root / "deep")
        (deep / "CACHEDIR.TAG").write_bytes(tag)
        (deep / "a/b/.git").mkdir(parents=True)
        inner = make(self.root / "inner")  # a sealed store below an ordinary directory
        self.hex_store(inner / "store")
        (inner / "store/a0/clone/.git").mkdir(parents=True)
        self.age(top, nested, deep, inner)
        self.assertEqual(self.verdicts(), {"top": "git-checkout", "nested": "git-checkout",
                                           "deep": "reclaimable", "inner": "git-checkout"})

    def test_home_caches_are_discovered_by_layout(self) -> None:
        home = self.root / "home"
        tag = gd.CACHEDIR_TAG + b"\n"
        cargo_tag = gd.CACHEDIR_TAG + b"\n# This file is a cache directory tag created by cargo.\n"
        (home / ".npm/_cacache/index-v5").mkdir(parents=True)
        (home / ".npm/_cacache/content-v2").mkdir()
        bun = home / ".bun/install/cache"
        for i in range(10):
            (bun / f"pkg{i}@1.0.{i}@@@1").mkdir(parents=True)
        (home / ".bun/bin").mkdir()
        (home / "go/pkg/mod/cache/download").mkdir(parents=True)
        (home / "go/pkg/mod/cache/lock").write_bytes(b"")
        (home / "go/src/mine").mkdir(parents=True)
        (home / ".cargo/registry").mkdir(parents=True)
        (home / ".cargo/registry/CACHEDIR.TAG").write_bytes(cargo_tag)
        (home / ".cargo/bin").mkdir()
        # never candidates: secrets, source, a checkout's build output, the Chromium trees,
        # a project's node_modules, and a hex-sharded store (a backup repository looks the same)
        for keep in (".ssh/cache", ".config/gh/cache", "Documents/cache", "Projects/x/cache",
                     "code/repo/target", "cmux-browser-fleet/cache", "app/node_modules/.cache",
                     ".my-tokens/cache", ".tool/untagged-creator"):
            (home / keep).mkdir(parents=True)
            (home / keep / "CACHEDIR.TAG").write_bytes(tag)
        # installed environments uv and direnv tag as caches, even ones naming cargo, are not recreatable
        for env in (".local/share/uv/tools/ruff", ".local/share/uv/python/cpython-3.12", "tools/.direnv",
                    "envs/myenv", ".pipx-like/venvs/x"):
            (home / env).mkdir(parents=True)
            (home / env / "CACHEDIR.TAG").write_bytes(cargo_tag)
        (home / "envs/myenv/pyvenv.cfg").write_text("home = /usr/bin")
        # cargo tags a target directory too: one outside a project (CARGO_TARGET_DIR) is a build
        (home / ".cargo-target/debug").mkdir(parents=True)
        (home / "shared-target").mkdir()
        (home / "shared-target/.rustc_info.json").write_text("{}")
        for build in (".cargo-target", "shared-target"):
            (home / build / "CACHEDIR.TAG").write_bytes(cargo_tag)
        (home / ".pipx-like/venvs/x/pyvenv.cfg").write_text("home = /usr/bin")
        (home / "code/repo/.git").mkdir()
        self.hex_store(home / "backups/restic/data")
        with mock.patch.object(gd, "HOME", home):
            found = sorted(os.path.relpath(p, home) for p in gd.discover_caches(home))
        self.assertEqual(found, [".bun/install/cache", ".cargo/registry", ".npm/_cacache", "go/pkg/mod"])
        # a tree too large for the budget hides its own caches, and only its own
        wide = home / "wide"
        for i in range(50):
            (wide / f"d{i}").mkdir(parents=True)
        (wide / "d0/CACHEDIR.TAG").write_bytes(cargo_tag)
        with mock.patch.object(gd, "HOME", home):
            found = {os.path.relpath(p, home) for p in gd.discover_caches(home, budget=20)}
        self.assertNotIn("wide/d0", found)
        self.assertIn(".cargo/registry", found)

    def test_other_filesystems_and_read_errors_fail_closed(self) -> None:
        tree = self.root / "tree"
        (tree / "mnt/data").mkdir(parents=True)
        (tree / "plain").mkdir()
        self.assertEqual(gd.git_state(tree), "none")
        real = os.DirEntry.stat
        dev = os.lstat(tree).st_dev

        def fake(entry, *a, **k):
            st = real(entry, *a, **k)
            if entry.name == "mnt":
                return os.stat_result((st.st_mode, st.st_ino, dev + 1) + tuple(st)[3:])
            return st
        with mock.patch.object(os.DirEntry, "stat", fake):
            self.assertEqual(gd.git_state(tree), "unchecked")  # a delete would descend into the mount
        real_lstat = os.lstat

        def fake_lstat(p, *a, **k):
            st = real_lstat(p, *a, **k)
            if os.fspath(p).endswith("/mnt"):
                return os.stat_result((st.st_mode, st.st_ino, dev + 1) + tuple(st)[3:])
            return st
        home = self.root / "home"
        (home / "mnt/cache/index-v5").mkdir(parents=True)
        (home / "mnt/cache/content-v2").mkdir()
        with mock.patch.object(os, "lstat", fake_lstat):
            self.assertEqual(gd.discover_caches(home), [])
        self.assertEqual(gd.discover_caches(home), [home / "mnt/cache"])
        # EIO or ESTALE while reading a directory is not evidence that nothing is there
        real_scandir = os.scandir

        def failing(p="."):
            if os.fspath(p).endswith("/plain"):
                raise OSError(5, "Input/output error")
            return real_scandir(p)
        with mock.patch.object(os, "scandir", failing):
            self.assertEqual(gd.git_state(tree), "unchecked")
        with self.assertRaisesRegex(OSError, "mount"), mock.patch.object(os.path, "ismount", return_value=True):
            gd.remove(tree / "plain")
        self.assertTrue((tree / "plain").is_dir())

    def test_apply_refreshes_only_discovered_candidates_sizes(self) -> None:
        home = self.root / "home"
        (home / ".npm/_cacache/index-v5").mkdir(parents=True)
        (home / ".npm/_cacache/content-v2").mkdir()
        (home / "code/checkout").mkdir(parents=True)
        disc = gd.Family("home-caches", home, True, "re-download", discover=True)
        other = gd.Family("xcode-derived-data", self.root / "dd", True, "rebuild")
        known = {str(home / ".npm/_cacache"): 1, str(home / "code/checkout"): 2,
                 str(self.root / "dd/x"): 3, str(self.root / "elsewhere"): 4}
        self.assertEqual(gd.fresh_for_apply(known, [disc, other]),
                         {str(home / "code/checkout"): 2, str(self.root / "elsewhere"): 4})

    def rustup_home(self, home: Path) -> Path:
        rh = home / ".rustup"
        for tc in ("stable-aarch64-apple-darwin", "nightly-aarch64-apple-darwin",
                   "1.88.0-aarch64-apple-darwin", "1.90.0-aarch64-apple-darwin"):
            lib = rh / "toolchains" / tc / "lib/rustlib"
            lib.mkdir(parents=True)
            (lib / "multirust-channel-manifest.toml").write_text("x")
            (lib / "components").write_text("rustc")
            (rh / "toolchains" / tc / "bin").mkdir()
            (rh / "toolchains" / tc / "bin/rustc").write_bytes(b"\0" * 1024 * 1024)
            (rh / "update-hashes").mkdir(exist_ok=True)
            (rh / "update-hashes" / tc).write_text("hash")
        (rh / "settings.toml").write_text('version = "12"\ndefault_toolchain = "stable"\n'
                                          '[overrides]\n"/p/x" = "1.90.0-aarch64-apple-darwin"\n')
        return rh

    def test_rustup_toolchains_other_than_the_default_go_lru(self) -> None:
        home = self.root / "home"
        rh = self.rustup_home(home)
        self.age(rh)
        # read an hour ago, written long ago: where the filesystem records access, LRU order sees it
        now = time.time()
        os.utime(rh / "toolchains/nightly-aarch64-apple-darwin/bin/rustc", (now - 3600, now - 90 * 24 * 3600))
        self.fam = gd.Family("home-caches", home, True, "re-download", discover=True)
        with mock.patch.object(gd, "HOME", home), mock.patch.object(gd.shutil, "which", return_value=None), \
                mock.patch.dict(os.environ, {"CARGO_HOME": str(home / ".cargo")}):
            self.assertTrue(all(v == "kept" for v in self.verdicts().values()))  # no rustup to reinstall
            (home / ".cargo/bin").mkdir(parents=True)
            (home / ".cargo/bin/rustup").write_bytes(b"")
            self.assertEqual(self.verdicts(), {"stable-aarch64-apple-darwin": "kept",
                                               "1.90.0-aarch64-apple-darwin": "kept",
                                               "nightly-aarch64-apple-darwin": "recent",
                                               "1.88.0-aarch64-apple-darwin": "reclaimable"})
            items = gd.survey([self.fam], 24, 0)
            receipt = self.root.parent / f"{self.root.name}-r.jsonl"
            with contextlib.redirect_stdout(io.StringIO()):
                gd.apply(items, {self.fam.id: self.fam}, receipt, None, 24)
            receipt.unlink(missing_ok=True)
        self.assertFalse((rh / "toolchains/1.88.0-aarch64-apple-darwin").exists())
        # rustup would skip the reinstall while the old update hash still matches the channel
        self.assertFalse((rh / "update-hashes/1.88.0-aarch64-apple-darwin").exists())
        self.assertTrue((rh / "update-hashes/nightly-aarch64-apple-darwin").exists())
        self.assertTrue((rh / "toolchains/stable-aarch64-apple-darwin").exists())
        self.assertTrue((rh / "settings.toml").exists())

    def test_rustup_toolchains_a_checkout_or_the_environment_pins_stay(self) -> None:
        home = self.root / "home"
        rh = self.rustup_home(home)
        (home / ".cargo/bin").mkdir(parents=True)
        (home / ".cargo/bin/rustup").write_bytes(b"")
        (home / "Projects/app").mkdir(parents=True)
        (home / "Projects/app/rust-toolchain.toml").write_text('[toolchain]\nchannel = "1.88.0"\n')
        (home / "Projects/app-worktrees/b").mkdir(parents=True)
        (home / "Projects/app-worktrees/b/rust-toolchain").write_text("nightly\n")
        with mock.patch.object(gd, "HOME", home), mock.patch.object(gd.shutil, "which", return_value=None), \
                mock.patch.dict(os.environ, {"CARGO_HOME": str(home / ".cargo")}):
            os.environ.pop("RUSTUP_TOOLCHAIN", None)
            tcs = rh / "toolchains"
            self.assertTrue(gd.rustup_verdict(tcs / "1.88.0-aarch64-apple-darwin"))
            self.assertTrue(gd.rustup_verdict(tcs / "nightly-aarch64-apple-darwin"))
            (home / "Projects/app-worktrees/b/rust-toolchain").unlink()
            self.assertEqual(gd.rustup_verdict(tcs / "nightly-aarch64-apple-darwin"), "")
            os.environ["RUSTUP_TOOLCHAIN"] = "nightly"
            self.assertTrue(gd.rustup_verdict(tcs / "nightly-aarch64-apple-darwin"))

    def test_apply_rechecks_a_discovered_cache_for_a_new_clone(self) -> None:
        home = self.root / "home"
        cache = home / ".npm/_cacache"
        (cache / "index-v5").mkdir(parents=True)
        (cache / "content-v2").mkdir()
        (cache / "content-v2/blob").write_bytes(b"\0" * 1024 * 1024)
        self.age(home / ".npm")
        self.fam = gd.Family("home-caches", home, True, "re-download", discover=True)
        receipt = self.root.parent / f"{self.root.name}-r.jsonl"
        with mock.patch.object(gd, "HOME", home):
            items = gd.survey([self.fam], 24, 0)
            self.assertEqual([i.verdict for i in items], ["reclaimable"])
            (cache / "mine/.git").mkdir(parents=True)  # someone cloned into it after the survey
            self.age(home / ".npm")
            with contextlib.redirect_stdout(io.StringIO()):
                gd.apply(items, {self.fam.id: self.fam}, receipt, None, 24)
        outcome = json.loads(receipt.read_text().splitlines()[-1])["outcome"]
        receipt.unlink(missing_ok=True)
        self.assertEqual(outcome, "changed:git")
        self.assertTrue((cache / "mine/.git").is_dir())

    def test_discovered_cache_in_use_is_kept(self) -> None:
        home = self.root / "home"
        cache = home / ".npm/_cacache"
        (cache / "index-v5").mkdir(parents=True)
        (cache / "content-v2").mkdir()
        (cache / "content-v2/blob").write_bytes(b"\0" * 1024 * 1024)
        self.age(home / ".npm")
        self.fam = gd.Family("home-caches", home, True, "re-download", discover=True)
        with mock.patch.object(gd, "HOME", home):
            self.assertEqual(self.verdicts(), {"_cacache": "reclaimable"})
            gd.process_evidence = lambda: ([str(cache / "content-v2/blob")], "")
            self.assertEqual(self.verdicts(), {"_cacache": "in-use"})

    def test_bazel_output_bases_go_and_the_repository_cache_stays(self) -> None:
        # /private/var/tmp/_bazel_$USER on macOS: output bases (md5 names) and install/ are rebuilt by
        # bazel, even with fetched clones inside; cache/ is the repository cache and is never a candidate.
        self.fam = gd.bazel_family(self.root)
        base = make(self.root / "0123456789abcdef0123456789abcdef")
        (base / "external/rules_x/.git").mkdir(parents=True)
        (base / "execroot/_main").mkdir(parents=True)
        os.symlink(self.root.parent, base / "execroot/_main/outside")
        (make(self.root / "cache") / "repos/v1").mkdir(parents=True)
        old = time.time() - 48 * 3600
        for top in (base, self.root / "cache"):
            for dirpath, dirs, files in os.walk(top):
                for n in dirs + files:
                    os.utime(os.path.join(dirpath, n), (old, old), follow_symlinks=False)
            os.utime(top, (old, old))
        self.assertEqual(self.verdicts(), {base.name: "reclaimable"})
        items = [i for i in gd.survey([self.fam], 24, 0) if i.verdict == "reclaimable"]
        with contextlib.redirect_stdout(io.StringIO()):
            gd.apply(items, {self.fam.id: self.fam}, self.root.parent / f"{self.root.name}-r.jsonl", None, 24)
        (self.root.parent / f"{self.root.name}-r.jsonl").unlink(missing_ok=True)
        self.assertFalse(base.exists())
        self.assertTrue((self.root / "cache/repos/v1").is_dir() and self.root.parent.is_dir())

    def test_only_home_caches_discovers(self) -> None:
        fams = gd.default_families()
        self.assertEqual([(f.id, f.root) for f in fams if f.discover], [("home-caches", gd.HOME)])
        for n in (".ssh", ".gnupg", ".secrets", "Library", "Projects", "Documents", ".cache"):
            self.assertIn(n, gd.HOME_NEVER)

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

    def test_snapshot_records_per_item_measurement_times(self) -> None:
        snap = self.root / "times.json"
        gd.save_snapshot({"/old": 1, "/new": 2}, snap, at=1000.0,
                         measured_at={"/old": 1000.0, "/new": 2000.0})
        self.assertEqual(gd.load_snapshot_meta(snap), (1000.0, {"/old": 1, "/new": 2},
                                                        {"/old": 1000.0, "/new": 2000.0}))

    def test_accounting_reports_df_space_outside_measured_items(self) -> None:
        path = self.root / "known"
        path.write_bytes(b"x")
        fs = gd.Fs(path.stat().st_dev, "/volume", 2 * gd.GIB, 10 * gd.GIB, 3 * gd.GIB, 4 * gd.GIB)
        item = gd.Item("xcode-derived-data", str(path), 3 * gd.GIB, 2.0, "report-only")
        saved = (gd.DARWIN, gd.ACCOUNTING)
        gd.DARWIN, gd.ACCOUNTING = True, self.root / "accounting.json"
        gd._ACCOUNTING_MEM.clear()
        self.addCleanup(lambda: (setattr(gd, "DARWIN", saved[0]), setattr(gd, "ACCOUNTING", saved[1]),
                                  gd._ACCOUNTING_MEM.clear()))
        with mock.patch.object(gd, "du_children", return_value={"/volume/ci": 8 * gd.GIB}):
            got = gd.filesystem_accounting([fs], [item])
        self.assertEqual(got[0]["measured"], 3 * gd.GIB)
        self.assertEqual(got[0]["unmeasured"], 5 * gd.GIB)
        self.assertEqual(got[0]["top_level"][0]["path"], "/volume/ci")

    def test_accounting_does_not_double_count_nested_items(self) -> None:
        parent = self.root / "parent"
        child = parent / "child"
        parent.mkdir()
        child.mkdir()
        fs = gd.Fs(parent.stat().st_dev, str(self.root), 2 * gd.GIB, 10 * gd.GIB, 3 * gd.GIB, 4 * gd.GIB)
        items = [gd.Item("projects", str(parent), 7 * gd.GIB, 2.0, "report-only"),
                 gd.Item("cargo-target", str(child), 3 * gd.GIB, 2.0, "report-only")]
        saved = (gd.DARWIN, gd.ACCOUNTING)
        gd.DARWIN, gd.ACCOUNTING = False, self.root / "accounting-overlap.json"
        gd._ACCOUNTING_MEM.clear()
        self.addCleanup(lambda: (setattr(gd, "DARWIN", saved[0]), setattr(gd, "ACCOUNTING", saved[1]),
                                  gd._ACCOUNTING_MEM.clear()))
        got = gd.filesystem_accounting([fs], items)
        self.assertEqual(got[0]["measured"], 7 * gd.GIB)

    def test_accounting_cache_keeps_walk_timestamp_until_walk_is_due(self) -> None:
        path = self.root / "known"
        path.write_bytes(b"x")
        fs = gd.Fs(path.stat().st_dev, "/volume", 2 * gd.GIB, 10 * gd.GIB, 3 * gd.GIB, 4 * gd.GIB)
        saved = (gd.DARWIN, gd.ACCOUNTING)
        gd.DARWIN, gd.ACCOUNTING = True, self.root / "accounting-cache.json"
        gd._ACCOUNTING_MEM.clear()
        self.addCleanup(lambda: (setattr(gd, "DARWIN", saved[0]), setattr(gd, "ACCOUNTING", saved[1]),
                                  gd._ACCOUNTING_MEM.clear()))
        with mock.patch.object(gd, "du_children", return_value={"/volume/ci": 8 * gd.GIB}):
            gd.filesystem_accounting([fs], [])
        first = json.loads(gd.ACCOUNTING.read_text())[str(fs.dev)]["at"]
        gd._ACCOUNTING_MEM.clear()
        with mock.patch.object(gd, "du_children", side_effect=AssertionError("cache should be used")):
            gd.filesystem_accounting([fs], [])
        second = json.loads(gd.ACCOUNTING.read_text())[str(fs.dev)]["at"]
        self.assertEqual(second, first)

    def test_retired_owner_family_is_reclaimable_when_unloaded(self) -> None:
        make(self.root / "old")
        fam = gd.Family("retired", self.root, True, "rebuild", retired_only=True,
                        owner_job="system/example.retired")
        with mock.patch.object(gd, "owner_retired", return_value=True):
            self.assertEqual(self.verdicts_for([fam])["old"], "reclaimable")

    def verdicts_for(self, families: list[gd.Family]) -> dict[str, str]:
        return {Path(i.path).name: i.verdict for i in gd.survey(families, 24, 0)}

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
        # a 50 GiB runner floor lifts the floor to 70 GiB on HOME's volume, so a busy mini reclaims before
        # its gate stops the runners, not only after
        home = gd.replace(low, dev=gd.HOME.stat().st_dev)
        self.assertIn("deferred", gd.ci_defer({1: home}, [job], "10%:30-60", floor_gib=40))
        self.assertEqual(gd.ci_defer({1: home}, [job], "10%:30-60", floor_gib=50), "")
        self.assertIn("deferred", gd.ci_defer({1: gd.replace(home, free=75 * gd.GIB)}, [job], "10%:30-60", 50))

    def test_chromium_trees_are_never_candidates(self) -> None:
        saved = gd.HOME
        gd.HOME = self.root
        try:
            caches = self.root / "Library/Caches"
            for name in ("siso", "cmux-release-support", "go-build", "chromium-cache"):
                make(caches / name)
            make(self.root / "cmux-browser-fleet/build/chromium/src")
            make(self.root / "other")
            fams = [gd.Family("library-caches", caches, True, "rebuild"),
                    gd.Family("home", self.root, True, "rebuild"),
                    gd.Family("deep", self.root / "cmux-browser-fleet", True, "rebuild", depth=2)]
            got = {Path(i.path).name: i.verdict for i in gd.survey(fams, 1, 0)}
            self.assertEqual(got, {"go-build": "reclaimable", "other": "reclaimable"})
            for rel in ("cmux-browser-fleet", "cmux-browser-fleet/build", "Library/Caches/siso",
                        "Library/Caches/cmux-release-support/ghostty", "Library"):  # Library holds some
                self.assertTrue(gd.never_delete(self.root / rel), rel)
            self.assertTrue(gd.never_delete(Path("/private/tmp/chromium-out")))
            self.assertFalse(gd.never_delete(self.root / "Library/Caches/go-build"))
        finally:
            gd.HOME = saved

    def test_fleet_ci_hot_tier(self) -> None:
        ci = self.root / "ci"
        (ci / "seeds").mkdir(parents=True)
        (ci / "seed-source.json").write_text(json.dumps({"prefix": "p-"}))
        for name, age in (("p-a", 1), ("p-b", 2), ("p-c", 48), ("q-x", 48)):
            make(ci / "seeds" / name, age_hours=age)
        make(ci / "pr-builds/pr-1", age_hours=0.5)
        make(ci / "pr-builds/pr-2", age_hours=10)
        make(ci / "pr-builds/.pr-3.discard-9", age_hours=2)  # cmux's clear() owns it
        make(ci / "pr-builds/other", age_hours=10)
        make(ci / "derived-data", age_hours=10)
        make(ci / ".derived-data.discard-7", age_hours=60)
        make(ci / "source-packages", age_hours=60)
        make(ci / "cmux-ci-2/derived-data", age_hours=60)
        # a root with no seed source keeps its newest two of any prefix; dot entries are never seeds
        for name, age in (("p-y", 48), ("p-z", 49), ("q-z", 50), (".q-w.incoming-3", 50)):
            make(ci / "cmux-ci-2/seeds" / name, age_hours=age)
        make(ci / "cmux-ci-x/derived-data", age_hours=60)  # not a root store
        # SwiftPM checkouts inside DerivedData do not make it a checkout
        (ci / "pr-builds/pr-2/SourcePackages/checkouts/dep/.git").mkdir(parents=True)
        for rel in ("SourcePackages/checkouts/dep/.git", "SourcePackages/checkouts/dep", "SourcePackages/checkouts",
                    "SourcePackages", ""):
            os.utime(ci / "pr-builds/pr-2" / rel, (time.time() - 36000,) * 2)
        self.assertEqual(gd.fleet_stores(ci), [ci, ci / "cmux-ci-2"])
        items = gd.survey(gd.fleet_families(ci), 6, 0)
        got = {str(Path(i.path).relative_to(ci)): i.verdict for i in items}
        self.assertEqual(got, {
            "seeds/p-a": "kept", "seeds/p-b": "kept", "seeds/p-c": "reclaimable", "seeds/q-x": "reclaimable",
            "pr-builds/pr-1": "recent", "pr-builds/pr-2": "recent",
            "derived-data": "recent", ".derived-data.discard-7": "reclaimable",
            "cmux-ci-2/derived-data": "reclaimable", "cmux-ci-2/seeds/p-y": "kept", "cmux-ci-2/seeds/p-z": "kept",
            "cmux-ci-2/seeds/q-z": "reclaimable", "cmux-ci-2/seeds/.q-w.incoming-3": "reclaimable"})
        # apply keeps the hot-tier windows, and re-reads command lines per item: a job that starts cloning a
        # parked build during the run keeps it
        # (apply looks families up by id, as main passes them: every root's family of an id behaves the same)
        fams = {f.id: f for f in gd.fleet_families(ci)}
        with mock.patch.object(gd, "FLEET_CI", ci), \
                mock.patch.object(gd, "command_lines", lambda: f"cp -cR {ci}/pr-builds/pr-2 /tmp/x\n"):
            reclaimable = [i for i in items if i.verdict == "reclaimable"]
            gd.apply(reclaimable, fams, self.root / "r.jsonl", None, 6)
        self.assertTrue((ci / "pr-builds/pr-2").exists())
        self.assertFalse((ci / "seeds/p-c").exists())
        self.assertTrue((ci / "seeds/p-a").exists())
        self.assertFalse((ci / "cmux-ci-2/derived-data").exists())

    def test_spm_scratch_goes_under_its_own_lock(self) -> None:
        """cmux9s, 2026-09-30: 31 GiB of SwiftPM scratch no job held, 0.0 GiB reclaimable, every job refused."""
        ci = self.root / "ci"
        make(ci / "seeds/p-a", age_hours=1)
        scratch = ci / "spm-scratch"
        for name, age in (("held", 1), ("free", 1), ("old", 50), (".trash-gone-7", 0.1)):
            make(scratch / name, age_hours=age)
        for name, age in (("held", 0.5), ("free", 0.5), ("old", 50)):
            lock = scratch / f"{name}.lock"
            lock.touch()
            t = time.time() - age * 3600
            os.utime(lock, (t, t))
        (scratch / "free.size").write_text("1048576")
        make(ci / "cmux-ci-2/spm-scratch/other", age_hours=50)  # only root 1's store holds scratch (cmux mini_store)
        # a job's holder: owned_spm_scratch.py hold keeps a shared flock for the whole job
        holder = subprocess.Popen([sys.executable, "-c", "import fcntl, sys, time\n"
                                   "f = open(sys.argv[1], 'a'); fcntl.flock(f, fcntl.LOCK_SH)\n"
                                   "print('held', flush=True); time.sleep(60)\n", os.fspath(scratch / "held.lock")],
                                  stdout=subprocess.PIPE, text=True)
        self.addCleanup(holder.stdout.close)
        self.addCleanup(holder.wait)
        self.addCleanup(holder.kill)
        self.assertEqual(holder.stdout.readline().strip(), "held")
        self.assertTrue(gd.spm_scratch_held(scratch / "held"))
        self.assertFalse(gd.spm_scratch_held(scratch / "free"))
        self.assertFalse(gd.spm_scratch_held(scratch / "old"))

        fams = [f for f in gd.fleet_families(ci) if f.id == gd.SPM_SCRATCH]
        self.assertEqual([f.root for f in fams], [scratch])
        self.assertFalse(gd.job_busy(fams[0], scratch / "free"), "its lock, not a slot marker search, decides")

        def verdicts(emergency: bool) -> dict[str, str]:
            devs = frozenset({scratch.stat().st_dev}) if emergency else frozenset()
            with mock.patch.object(gd, "EMERGENCY_DEVS", devs):
                items = gd.survey(fams, 6, 0)
            return {Path(i.path).name: i.verdict for i in items}

        # under pressure a used scratch ages like any cache; a leftover of a killed delete always goes
        self.assertEqual(verdicts(False), {"held": "in-use", "free": "recent", "old": "reclaimable",
                                           ".trash-gone-7": "reclaimable"})
        # below the emergency floor, whatever no job holds goes: it rebuilds in minutes, the runner floor is near
        got = verdicts(True)
        self.assertEqual(got, {"held": "in-use", "free": "reclaimable", "old": "reclaimable",
                               ".trash-gone-7": "reclaimable"})
        with mock.patch.object(gd, "EMERGENCY_DEVS", frozenset({scratch.stat().st_dev})):
            items = [i for i in gd.survey(fams, 6, 0) if i.verdict == "reclaimable"]
            # a job links "old" between the survey and the delete: apply re-checks the lock and keeps it
            late = open(scratch / "old.lock", "a")
            fcntl.flock(late, fcntl.LOCK_SH)
            gd.apply(items, {f.id: f for f in fams}, self.root / "r.jsonl", None, 6)
            late.close()
        self.assertFalse((scratch / "free").exists())
        self.assertFalse((scratch / "free.size").exists())
        self.assertFalse((scratch / ".trash-gone-7").exists())
        self.assertTrue((scratch / "old").exists(), "held at delete time")
        self.assertTrue((scratch / "held").exists())
        self.assertEqual(sorted(p.name for p in scratch.iterdir()),
                         ["free.lock", "held", "held.lock", "old", "old.lock"], "no .trash-* left behind")
        outcomes = {Path(r["path"]).name: r["outcome"]
                    for r in map(json.loads, (self.root / "r.jsonl").read_text().splitlines())}
        self.assertEqual(outcomes, {"free": "reclaimed", ".trash-gone-7": "reclaimed", "old": "changed:in-use"})
        # the delete itself takes the lock: a holder that arrives after every re-check still keeps its directory
        make(scratch / "raced", age_hours=50)
        late = open(scratch / "raced.lock", "a")
        fcntl.flock(late, fcntl.LOCK_SH)
        self.assertFalse(gd.remove_spm_scratch(scratch / "raced"))
        late.close()
        self.assertTrue(gd.remove_spm_scratch(scratch / "raced"))
        self.assertFalse((scratch / "raced").exists())

    def test_the_emergency_floor_names_its_filesystems(self) -> None:
        total = 460 * gd.GIB  # emergency 10%:30-60 is 46 GiB; a 30 GiB runner floor lifts it to 50 on HOME's volume
        home = gd.HOME.stat().st_dev
        fss = {1: gd.Fs(1, "/a", 40 * gd.GIB, total, 115 * gd.GIB, 161 * gd.GIB),
               2: gd.Fs(2, "/b", 90 * gd.GIB, total, 115 * gd.GIB, 161 * gd.GIB),
               home: gd.Fs(home, "/", 48 * gd.GIB, total, 115 * gd.GIB, 161 * gd.GIB)}
        self.assertEqual(gd.emergency_devs(fss, "10%:30-60"), {1})
        self.assertEqual(gd.emergency_devs(fss, "10%:30-60", floor_gib=30), {1, home})

    def test_a_seed_the_archive_holds_goes_without_waiting(self) -> None:
        ci = self.root / "ci"
        (ci / "seeds").mkdir(parents=True)
        (ci / "seed-source.json").write_text(json.dumps({"prefix": "p-"}))
        for name, age in (("p-a", 0.2), ("p-b", 0.3), ("p-c", 1), ("p-d", 2), ("p-e", 3)):
            make(ci / "seeds" / name, age_hours=age)
        log = self.root / "serve.jsonl"
        archive, mini = "172.20.21.158", "172.20.21.9"

        def rec(client: str, outcome: str, **kw) -> str:
            return json.dumps({"client": client, "outcome": outcome, "role": "seed", **kw})

        lst = rec(archive, "list", count=5)
        (log.with_name("serve.jsonl.1")).write_text("\n".join([
            lst, rec(archive, "served", status=0, key="p-c"),
            rec(archive, "served", status=0, key="p-b"), lst, lst]) + "\n")
        log.write_text("\n".join([
            "not json", json.dumps({"client": ["x"], "outcome": "list"}), "[" * 5000,
            lst, rec(archive, "served", status=0, key="p-a"),
            rec(archive, "failed", status=1, key="p-d"),
            # a client that never listed is not the archive
            rec(mini, "served", status=0, key="p-e"),
            lst, lst,
            # served but not yet seen held by two later lists: the archive may still reject it
            rec(archive, "served", status=0, key="p-f"), lst,
            # the archive asked for p-b again (its check failed there): only a key's newest request counts
            rec(archive, "served", status=0, key="p-b")]) + "\n")
        self.assertEqual(gd.archived_seeds(log), {"p-a", "p-c"})
        self.assertEqual(gd.archived_seeds(self.root / "missing.jsonl"), frozenset())
        # a real DerivedData's module cache is wider than cmux_active's search budget, which reads as busy:
        # that must not keep cmux CI's warm state, which no hq reload slot marker ever covers
        with mock.patch.object(gd, "SEED_SERVE_LOG", log), mock.patch.object(gd, "cmux_active", lambda *a, **k: True):
            items = gd.survey(gd.fleet_families(ci), 24, 0)
            got = {Path(i.path).name: (i.verdict, i.reasons) for i in items}
            # the newest two stay even when archived; an archived older one goes; the rest wait out the window
            self.assertEqual(got["p-a"][0], "kept")
            self.assertEqual(got["p-b"][0], "kept")
            self.assertEqual(got["p-c"], ("reclaimable", ["the seed archive holds it"]))
            self.assertEqual(got["p-d"][0], "recent")
            self.assertEqual(got["p-e"][0], "recent")
            fams = {f.id: f for f in gd.fleet_families(ci)}
            with mock.patch.object(gd, "FLEET_CI", ci):
                gd.apply([i for i in items if i.verdict == "reclaimable"], fams, self.root / "r.jsonl", None, 24)
        self.assertFalse((ci / "seeds/p-c").exists())
        for name in ("p-a", "p-b", "p-d", "p-e"):
            self.assertTrue((ci / "seeds" / name).exists(), name)

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

    def test_a_runner_disk_floor_lifts_the_pressure_thresholds(self) -> None:
        home = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, home, ignore_errors=True)
        self.assertEqual(gd.runner_floor_gib(home), 0.0, "no runner: no floor")
        for name, text in (("actions-runner-glaeda", "exec x job-started --min-free-gib 100 --min-free-gib 150\n"),
                           ("actions-runner-glaeda-1", "exec x job-started --min-free-gib 1e+02\n"),
                           ("actions-runner-glaeda-2", "exec x job-started --min-free-gib nope\n"),
                           ("actions-runner-glaeda-3", "exec x job-started --min-free-gib 1e999\n")):
            hook = home / name / "glaeda-hooks/job-started.sh"
            hook.parent.mkdir(parents=True)
            hook.write_text(text)
        self.assertEqual(gd.runner_floor_gib(home), 150.0, "the highest, each hook's last")
        free, total = gd.free_bytes(self.root)
        with mock.patch.object(gd, "HOME", self.root):  # the floor lifts only HOME's volume
            plain = gd.filesystems([self.fam], "0", "0")
            lifted = gd.filesystems([self.fam], "0", "0", floor_gib=1)
            (fs,), (up,) = plain.values(), lifted.values()
            self.assertEqual((fs.low, fs.target), (0, 0))
            self.assertEqual(up.low, min(int(5 * gd.GIB), total // 2), "pressure starts before the resume mark")
            self.assertEqual(up.target, min(int(21 * gd.GIB), total // 2))
            huge = next(iter(gd.filesystems([self.fam], "0", "0", floor_gib=1e6).values()))
            self.assertEqual((huge.low, huge.target), (total // 2, total // 2), "never past half the disk")
            high = gd.filesystems([self.fam], "100%", "100%", floor_gib=150)
            self.assertEqual(next(iter(high.values())).low, total, "never lowers a higher threshold")
        with mock.patch.object(gd, "HOME", Path("/dev")):  # another volume: no lift
            other = next(iter(gd.filesystems([self.fam], "0", "0", floor_gib=1).values()))
            self.assertEqual(other.low, 0)

    def test_floor_gib_adds_the_callers_floor(self) -> None:
        seen: list[float] = []

        def fss(*args: object) -> dict:
            seen.append(args[-1])
            return {}

        with mock.patch.object(gd, "runner_floor_gib", return_value=50.0), \
                mock.patch.object(gd, "filesystems", side_effect=fss), contextlib.redirect_stdout(io.StringIO()):
            gd.main(["--pressure", "--floor-gib", "150", "--no-snapshot"])
            gd.main(["--pressure", "--floor-gib", "10", "--no-snapshot"])
            gd.main(["--pressure", "--floor-gib", "inf", "--no-snapshot"])
        self.assertEqual(seen, [150.0, 50.0, 50.0], "the higher of the hooks' floors and the caller's")

    def test_one_eviction_at_a_time(self) -> None:
        lock = self.root / "evict.lock"
        receipt = ["--receipt", os.fspath(self.receipt())]
        with mock.patch.object(gd, "EVICT_LOCK", lock), mock.patch.object(gd, "filesystems", return_value={}), \
                mock.patch.object(gd, "survey", return_value=[]) as survey, \
                mock.patch.object(gd, "apply", return_value=0):
            with lock.open("a") as held:
                fcntl.flock(held, fcntl.LOCK_EX | fcntl.LOCK_NB)
                out = io.StringIO()
                with contextlib.redirect_stdout(out):
                    self.assertEqual(gd.main(["--apply", "--no-snapshot", "--top", "0", *receipt]), 0)
                self.assertIn("another eviction running", out.getvalue())
                survey.assert_not_called()
            with contextlib.redirect_stdout(io.StringIO()):
                gd.main(["--apply", "--no-snapshot", "--top", "0", *receipt])
            survey.assert_called_once()

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

    def _checkout_builds(self) -> tuple[Path, gd.Family]:
        """projects/app (tracked root and nested packages) with a linked worktree in projects/wts and one
        outside projects; every .build is tagged unless the test says otherwise."""
        projects = self.root / "projects"
        app = projects / "app"
        self._git("init", "-q", "-b", "main", str(app))
        for rel in ("Package.swift", "Packages/macOS/Core/Package.swift", "Sources/x.swift"):
            (app / rel).parent.mkdir(parents=True, exist_ok=True)
            (app / rel).write_text("x")
        self._git("-C", str(app), "add", ".")
        self._git("-C", str(app), "commit", "-qm", "c")
        self._git("-C", str(app), "worktree", "add", "-q", str(projects / "wts/feature"), "-b", "feature")
        self._git("-C", str(app), "worktree", "add", "-q", str(self.root / "scratch/wt"), "-b", "scratch")
        tag = "Signature: 8a477f597d28d172789f06886806bc55"
        for co in (app, projects / "wts/feature", self.root / "scratch/wt"):
            for pkg in ("", "Packages/macOS/Core"):
                make(co / pkg / ".build")
                (co / pkg / ".build/CACHEDIR.TAG").write_text(tag)
        make(app / "Sources/.build")  # no Package.swift there: not SwiftPM's
        (app / "Sources/.build/CACHEDIR.TAG").write_text(tag)
        make(app / "Packages/macOS/Core/Tests/.build")  # untracked location, not a package
        make(app / ".glaeda/apple-build/cache/k1")
        (app / ".glaeda/apple-build/receipt.json").write_text("{}")
        for co in (app, projects / "wts/feature"):
            self._age(co)
        return projects, gd.checkout_build_family(projects)

    def test_checkout_builds_are_package_builds_and_apple_generations(self) -> None:
        projects, fam = self._checkout_builds()
        found = sorted(str(p.relative_to(projects)) for p in gd.candidates(fam))
        self.assertEqual(found, ["app/.build", "app/.glaeda/apple-build/cache/k1",
                                 "app/Packages/macOS/Core/.build", "wts/feature/.build",
                                 "wts/feature/Packages/macOS/Core/.build"])

    def test_checkout_build_goes_after_a_day_and_the_checkout_stays(self) -> None:
        projects, fam = self._checkout_builds()
        self._age(projects / "wts/feature/.build", hours=12)
        gd.process_evidence = lambda: ([], f"swift-frontend {projects}/app/Packages/macOS/Core/.build/x.o\n")
        items = gd.survey([fam], 6, 0)
        v = {str(Path(i.path).relative_to(projects)): i.verdict for i in items}
        self.assertEqual(v["app/.build"], "reclaimable")
        self.assertEqual(v["app/Packages/macOS/Core/.build"], "in-use")
        self.assertEqual(v["wts/feature/.build"], "recent")  # 12 h: inside the day a session may return in
        receipt = self.receipt()
        gd.apply(items, {fam.id: fam}, receipt, None, 6)
        self.assertFalse((projects / "app/.build").exists())
        self.assertFalse((projects / "app/.glaeda/apple-build/cache/k1").exists())
        self.assertTrue((projects / "app/.glaeda/apple-build/receipt.json").exists())
        self.assertTrue((projects / "app/Packages/macOS/Core/.build").exists())
        self.assertTrue((projects / "wts/feature/.build").exists())
        self.assertEqual(self._git("-C", str(projects / "app"), "status", "--porcelain", "-uno"), "")

    def test_apple_generation_stays_while_a_run_holds_the_lock(self) -> None:
        projects, fam = self._checkout_builds()
        state = projects / "app/.glaeda/apple-build"
        fd = os.open(state / "build.lock", os.O_RDWR | os.O_CREAT, 0o600)
        self.addCleanup(os.close, fd)
        fcntl.flock(fd, fcntl.LOCK_EX)
        v = {str(Path(i.path).relative_to(projects)): i.verdict for i in gd.survey([fam], 6, 0)}
        self.assertEqual(v["app/.glaeda/apple-build/cache/k1"], "kept")
        fcntl.flock(fd, fcntl.LOCK_UN)
        (state / "inflight.json").write_text("{}")
        v = {str(Path(i.path).relative_to(projects)): i.verdict for i in gd.survey([fam], 6, 0)}
        self.assertEqual(v["app/.glaeda/apple-build/cache/k1"], "kept")
        (state / "inflight.json").unlink()
        v = {str(Path(i.path).relative_to(projects)): i.verdict for i in gd.survey([fam], 6, 0)}
        self.assertEqual(v["app/.glaeda/apple-build/cache/k1"], "reclaimable")

    def test_checkout_builds_ignore_the_callers_git_dir(self) -> None:
        projects, fam = self._checkout_builds()
        with mock.patch.dict(os.environ, {"GIT_DIR": str(self.root / "nowhere/.git")}):
            found = {str(p.relative_to(projects)) for p in gd.checkout_builds(projects)}
        self.assertIn("wts/feature/Packages/macOS/Core/.build", found)

    def test_apply_refreshes_only_checkout_build_sizes(self) -> None:
        projects, fam = self._checkout_builds()
        known = {str(projects / "app/.build"): 1, str(projects / "app"): 2, str(projects / "wts/feature"): 3}
        self.assertEqual(gd.fresh_for_apply(known, [fam]),
                         {str(projects / "app"): 2, str(projects / "wts/feature"): 3})


    def test_idle_sweep_windows_follow_reuse(self) -> None:
        ci = self.root / "ci"
        (ci / "seeds").mkdir(parents=True)
        (ci / "seed-source.json").write_text(json.dumps({"prefix": "p-"}))
        for name, age in (("p-a", 1), ("p-b", 2), ("p-c", 12), ("p-d", 30)):
            make(ci / "seeds" / name, age_hours=age)
        make(ci / "pr-builds/pr-1", age_hours=3)
        make(ci / "pr-builds/pr-2", age_hours=7)
        make(ci / "derived-data", age_hours=30)
        fams = gd.idle_families(gd.fleet_families(ci))
        got = {str(Path(i.path).relative_to(ci)): i.verdict for i in gd.survey(fams, gd.IDLE_SWEEP_HOURS, 0)}
        self.assertEqual(got, {"seeds/p-a": "kept", "seeds/p-b": "kept", "seeds/p-c": "recent",
                               "seeds/p-d": "reclaimable", "pr-builds/pr-1": "recent",
                               "pr-builds/pr-2": "recent", "derived-data": "recent"})
        # DerivedData and caches wait the full window
        make(self.root / "dd/old", age_hours=50)
        make(self.root / "dd/day", age_hours=30)
        fam = gd.Family("xcode-derived-data", self.root / "dd", True, "rebuild")
        got = {Path(i.path).name: i.verdict for i in gd.survey(gd.idle_families([fam]), gd.IDLE_SWEEP_HOURS, 0)}
        self.assertEqual(got, {"old": "reclaimable", "day": "recent"})

    def test_host_busy_sees_jobs_builds_locks_and_reservations(self) -> None:
        fleet = self.root / "fleet"
        fleet.mkdir()
        ps = lambda out: mock.patch.object(gd.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, out, ""))
        with ps("/sbin/launchd\n/usr/bin/python3 x\n"):
            self.assertEqual(gd.host_busy(fleet), "")
            (fleet / "host.lock").touch()
            self.assertEqual(gd.host_busy(fleet), "")
            with (fleet / "host.lock").open() as held:
                fcntl.flock(held, fcntl.LOCK_SH)  # a PR job holds it shared: Runner.Worker says busy
                self.assertEqual(gd.host_busy(fleet), "")
            with (fleet / "host.lock").open() as held:
                fcntl.flock(held, fcntl.LOCK_EX)  # a fleet build
                self.assertIn("host lock", gd.host_busy(fleet))
            marker = {"schema": "glaeda-reservation/v1", "until": int(time.time()) + 60}
            (fleet / "reservation.json").write_text(json.dumps(marker))
            self.assertIn("reserved", gd.host_busy(fleet))
            (fleet / "reservation.json").write_text(json.dumps({**marker, "until": int(time.time()) - 60}))
            self.assertEqual(gd.host_busy(fleet), "")
            (fleet / "reservation.json").write_text(json.dumps({"until": int(time.time()) - 60}))
            self.assertIn("reserved", gd.host_busy(fleet))  # another schema is invalid, so active
            (fleet / "reservation.json").write_text("{")
            self.assertIn("unreadable", gd.host_busy(fleet))
            (fleet / "reservation.json").unlink()
            (fleet / "jobs").mkdir()
            (fleet / "jobs/.DS_Store").touch()
            self.assertEqual(gd.host_busy(fleet), "")
            (fleet / "jobs/abc").mkdir()
            self.assertIn("dev-build worker", gd.host_busy(fleet))
            (fleet / "jobs/abc").rmdir()
        with ps("/Users/cmux/actions-runner-glaeda/bin/Runner.Worker spawnclient 1 2\n"):
            self.assertIn("CI job", gd.host_busy(fleet))
        with ps("/Applications/Xcode 26.app/Contents/Developer/usr/bin/xcodebuild build\n"):
            self.assertIn("build", gd.host_busy(fleet))
        with ps(""):
            self.assertIn("cannot list", gd.host_busy(fleet))

    def test_idle_apply_stops_once_the_host_turns_busy(self) -> None:
        for name in ("a", "b", "c"):
            make(self.root / name)
        items = gd.survey([self.fam], 24, 0)
        calls = iter(["", "a CI job is running (x)"])
        with contextlib.redirect_stdout(io.StringIO()):
            gd.apply(items, {self.fam.id: self.fam}, self.receipt(), None, 24, halt=lambda: next(calls))
        self.assertEqual(sum((self.root / n).exists() for n in ("a", "b", "c")), 2)
        recs = [json.loads(l) for l in self.receipt().read_text().splitlines()]
        self.assertEqual([(r["outcome"], r["sweep"]) for r in recs], [("reclaimed", "idle")])

    def test_idle_sweep_in_main(self) -> None:
        make(self.root / "old", age_hours=50)
        make(self.root / "day", age_hours=30)
        stamp = self.root.parent / f"{self.root.name}-idle.last"
        self.addCleanup(stamp.unlink, missing_ok=True)
        args = ["--pressure", "--idle", "--apply", "--no-snapshot", "--top", "0", "--min-mib", "0", "--low", "1", "--target", "2",
                "--receipt", os.fspath(self.receipt())]
        with mock.patch.object(gd, "default_families", return_value=[self.fam]), \
                mock.patch.object(gd, "free_bytes", return_value=(100 * gd.GIB, 400 * gd.GIB)), \
                mock.patch.object(gd, "IDLE_STAMP", stamp), mock.patch.object(gd, "EVICT_LOCK", self.root.parent / f"{self.root.name}.lock"), \
                mock.patch.object(gd, "simulator_runtimes", return_value=[]):
            with mock.patch.object(gd, "host_busy", return_value="a CI job is running (x)"), \
                    mock.patch.dict(gd.POLICY, defer_during_ci=True), contextlib.redirect_stdout(io.StringIO()) as out:
                gd.main(args)
            self.assertIn("host busy", out.getvalue())
            self.assertTrue((self.root / "old").exists(), "defer_during_ci keeps the old hold")
            # a busy host sweeps too by default: only what a job uses is held
            with mock.patch.object(gd, "host_busy", return_value="a CI job is running (x)"), \
                    contextlib.redirect_stdout(io.StringIO()):
                gd.main(args)
            self.assertFalse((self.root / "old").exists())
            self.assertTrue((self.root / "day").exists())
            self.assertTrue(stamp.exists())
            make(self.root / "old", age_hours=50)
            with mock.patch.object(gd, "host_busy", return_value=""), contextlib.redirect_stdout(io.StringIO()) as out:
                gd.main(args)  # within the hour: no second sweep
            self.assertIn("within", out.getvalue())
            self.assertTrue((self.root / "old").exists())

    def test_simulator_runtimes_go_only_when_nothing_uses_them(self) -> None:
        now = time.time()
        old = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(now - 30 * 86400))
        new = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(now - 86400))

        def rt(version, used, deletable=True):
            return {"runtimeIdentifier": f"ios-{version}", "platformIdentifier": "iphonesimulator",
                    "version": version, "deletable": deletable, "lastUsedAt": used, "sizeBytes": 8 * gd.GIB}
        runtimes = {"A": rt("26.5", old), "B": rt("26.2", old), "C": rt("26.1", new), "D": rt("18.0", old),
                    "E": rt("17.0", old, deletable=False)}
        devices = {"devices": {"ios-18.0": [{"state": "Shutdown"}]}}
        got = {i.path: i.verdict for i in gd.runtime_items(runtimes, devices, now)}
        self.assertEqual(got, {"A": "kept", "B": "reclaimable", "C": "recent", "D": "kept", "E": "kept"})
        devices["devices"]["ios-18.0"][0]["state"] = "Booted"
        self.assertNotIn("reclaimable", {i.verdict for i in gd.runtime_items(runtimes, devices, now)})
        self.assertEqual(gd.runtime_items({"A": rt("1", old)}, None, now), [])

    def test_xcode_copies_are_report_only_with_their_pins(self) -> None:
        apps = self.root / "Applications"
        for name in ("Xcode.app", "Xcode_26.6.app"):
            (apps / name / "Contents").mkdir(parents=True)
        pins = {os.path.realpath(apps / "Xcode_26.6.app"): "a runner hook's --toolchain-xcode"}
        items = {Path(i.path).name: i for i in gd.xcode_items(apps, pins)}
        self.assertEqual({i.verdict for i in items.values()}, {"report-only"})
        self.assertIn("no pin", items["Xcode.app"].reasons[1])
        self.assertIn("runner hook", items["Xcode_26.6.app"].reasons[1])
        home = self.root / "home"
        (home / "actions-runner-glaeda/glaeda-hooks").mkdir(parents=True)
        (home / "actions-runner-glaeda/glaeda-hooks/job-started.sh").write_text(
            "exec x job-started --min-free-gib 50 --toolchain-xcode /Applications/Xcode_26.6.app --instance 0\n")
        self.assertIn(os.path.realpath("/Applications/Xcode_26.6.app"), gd.xcode_pins(home))


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

    def test_cache_families_skip_state_that_is_not_a_cache(self) -> None:
        fams = {f.id: f for f in gd.default_families() if f.id in ("user-cache", "library-caches")}
        for fam in fams.values():
            fam.root.mkdir(parents=True, exist_ok=True)
        keep = {"user-cache": ["glaeda", "glaeda-disk", "glaeda-fullapp", "glaeda-fleet-cas", "cmux-job",
                               "huggingface", "codex-runtimes"],
                "library-caches": ["glaeda", "PassKit", "tidy-branches", "CloudKit", "com.apple.Safari"]}
        for fid, names in keep.items():
            fam = fams.get(fid)
            if fam is None:
                continue
            for n in names:
                self.assertTrue(n in fam.skip or n.startswith(fam.skip_prefixes), f"{fid}/{n}")
            self.assertTrue(fam.reclaimable)

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
        self.assertTrue(reclaimable <= {"tmp", "claude-scratchpad", "user-cache", "home-caches", "checkout-build"})
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


class ActivityClockTest(unittest.TestCase):
    """The hot tier ages in host CI jobs since last use, not hours; thresholds come from disk-policy.json."""

    def setUp(self) -> None:
        self.dir = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.dir, ignore_errors=True)
        self.fam = gd.Family("cmux-pr-builds", self.dir, True, "x")
        self.now = time.time()

    def starts(self, *hours_ago: float) -> list[float]:
        return sorted(self.now - h * 3600 for h in hours_ago)

    def due(self, mode: str, idle: float, starts: list[float] | None, window: float = 1.0) -> bool:
        with mock.patch.object(gd, "ACTIVITY_MODE", mode), mock.patch.object(gd, "job_starts", return_value=starts), \
                mock.patch.object(gd, "running_job_hours", return_value=0.0):
            return gd.item_due(self.fam, idle, window)[0]

    def test_running_job_hours_reads_the_oldest_runner_worker(self) -> None:
        out = (" 1-02:03:04 /Users/cmux/actions-runner-glaeda-2/bin/Runner.Worker spawnclient 1 2\n"
               "   05:06 /Users/cmux/actions-runner-glaeda/bin/Runner.Worker spawnclient\n 99:00:00 /bin/zsh\n")
        with mock.patch.object(gd, "_RUNNING_JOB_HOURS", None), \
                mock.patch.object(gd.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, out, "")):
            self.assertAlmostEqual(gd.running_job_hours(), 26 + 3 / 60 + 4 / 3600, places=4)
        with mock.patch.object(gd, "_RUNNING_JOB_HOURS", None), mock.patch.object(gd.subprocess, "run", side_effect=OSError):
            self.assertEqual(gd.running_job_hours(), 24.0)

    def test_pressure_does_not_use_running_job_age_as_a_retention_window(self) -> None:
        busy = self.starts(*[i / 100 for i in range(100)])
        with mock.patch.object(gd, "ACTIVITY_MODE", "pressure"), mock.patch.object(gd, "job_starts", return_value=busy), \
                mock.patch.object(gd, "running_job_hours", return_value=2.0):
            self.assertTrue(gd.item_due(self.fam, 1.5, 1.0)[0])  # process evidence, not job age, protects it
            self.assertTrue(gd.item_due(self.fam, 2.5, 1.0)[0])  # older than every running job

    def test_a_quiet_fleet_keeps_its_cache(self) -> None:
        # idle 30 h, but only 2 jobs ran since: not stale, nothing had a chance to use it
        self.assertTrue(self.due("pressure", 30, self.starts(1, 2)))
        self.assertFalse(self.due("sweep", 30, self.starts(1, 2)))

    def test_many_jobs_past_it_make_it_a_candidate(self) -> None:
        busy = self.starts(*[i / 100 for i in range(100)])  # 100 jobs in the last hour
        self.assertTrue(self.due("pressure", 1.5, busy))    # past pressure_jobs (80)
        self.assertFalse(self.due("sweep", 1.5, busy))      # the sweep waits for sweep_jobs (190)
        self.assertTrue(self.due("sweep", 30, busy[:90] + self.starts(29)))  # soft 24 h: idle a day, 80+ jobs

    def test_window_needs_some_activity(self) -> None:
        self.assertTrue(self.due("pressure", 2, self.starts(*[0.1] * 6)))  # past its window, min_jobs since
        self.assertTrue(self.due("pressure", 2, self.starts(0.1, 0.2)))

    def test_no_job_log_keeps_parked_builds_until_a_reuse_observation(self) -> None:
        self.assertTrue(self.due("pressure", 2, None))
        self.assertTrue(self.due(None, 2, self.starts(0.1)))
        other = gd.Family("tmp", self.dir, True, "x")
        with mock.patch.object(gd, "ACTIVITY_MODE", "pressure"), mock.patch.object(gd, "job_starts", return_value=[]):
            self.assertEqual(gd.item_due(other, 2, 1.0), (True, None))

    def test_job_log_and_policy_file(self) -> None:
        log = self.dir / "jobs.jsonl"
        log.write_text('{"event":"started","at":5}\n{"event":"completed","at":6}\nnot json\n{"event":"started","at":3}\n')
        self.assertEqual(gd.job_starts(log), [3.0, 5.0])
        self.assertIsNone(gd.job_starts(self.dir / "missing"))
        policy = self.dir / "disk-policy.json"
        policy.write_text('{"pressure_jobs": 40, "defer_during_ci": true, "min_jobs": "x", "sweep_jobs": -1}')
        got = gd.disk_policy(policy)
        self.assertEqual((got["pressure_jobs"], got["defer_during_ci"], got["min_jobs"], got["sweep_jobs"]),
                         (40, True, gd.POLICY_DEFAULTS["min_jobs"], gd.POLICY_DEFAULTS["sweep_jobs"]))
        policy.write_text("[")
        self.assertEqual(gd.disk_policy(policy), gd.POLICY_DEFAULTS)

    def test_fleet_disk_thresholds_override_defaults_and_need_margins(self) -> None:
        policy = self.dir / "disk-policy.json"
        policy.write_text('{"low":"54","target":"70","emergency":"50",'
                          '"low_margin_gib":5,"target_margin_gib":21}')
        settings = gd.disk_policy(policy)
        self.assertEqual((settings["low"], settings["target"], settings["emergency"]),
                         ("54", "70", "50"))
        seen: list[tuple[str, str, float]] = []

        def fss(*args: object) -> dict:
            seen.append((args[1], args[2], args[-1]))
            return {}

        with mock.patch.object(gd, "POLICY", settings), \
                mock.patch.object(gd, "runner_floor_gib", return_value=30.0), \
                mock.patch.object(gd, "filesystems", side_effect=fss), \
                contextlib.redirect_stdout(io.StringIO()):
            gd.main(["--pressure", "--no-snapshot"])
            gd.main(["--pressure", "--need", "50", "--no-snapshot"])
        self.assertEqual(seen, [("54", "70", 30.0), ("55", "71", 50.0)])

if __name__ == "__main__":
    unittest.main()
