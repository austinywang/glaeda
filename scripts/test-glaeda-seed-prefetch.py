#!/usr/bin/env python3
"""Tests for scripts/glaeda-seed-prefetch."""

from __future__ import annotations

import importlib.machinery
import importlib.util
import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
loader = importlib.machinery.SourceFileLoader("glaeda_seed_prefetch",
                                              os.fspath(ROOT / "scripts" / "glaeda-seed-prefetch"))
spec = importlib.util.spec_from_loader("glaeda_seed_prefetch", loader)
sp = importlib.util.module_from_spec(spec)
sys.modules["glaeda_seed_prefetch"] = sp
loader.exec_module(sp)

# Stands in for cmux's scripts/ci/seed_derived_data.py: logs each call and answers with the
# distance the test put in FAKE_DISTANCE.
FAKE_SEED = """#!/usr/bin/env python3
import json, os, sys
with open(os.environ["FAKE_CALLS"], "a") as log:
    log.write(" ".join(sys.argv[1:]) + " url=" + os.environ.get("CI_CACHE_R2_PUBLIC_URL", "")
              + " git=" + os.environ.get("CMUX_SEED_GIT_DIR", "")
              + " curlrc=" + (open(os.path.join(os.environ["CURL_HOME"], ".curlrc")).read().strip().replace(" ", "")
                              if os.environ.get("CURL_HOME") else "") + "\\n")
distance = int(os.environ.get("FAKE_DISTANCE", "0"))
print("some progress line")
print(json.dumps({"fetched": "true", "key": "k", "distance": distance}))
"""


class PrefetchBase(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        base = Path(self.tmp.name)
        self.state = base / "ci"
        self.state.mkdir()
        self.repo = base / "cmux"
        (self.repo / "scripts/ci").mkdir(parents=True)
        (self.repo / "scripts/ci/seed_derived_data.py").write_text(FAKE_SEED)
        self.git = ["git", "-C", os.fspath(self.repo), "-c", "user.name=t", "-c", "user.email=t@t"]
        subprocess.run(["git", "init", "-q", "-b", "main", os.fspath(self.repo)], check=True)
        self.commit()
        self.calls = base / "calls"
        self.saved = (sp.REPO_URL, sp.running_commands, dict(os.environ))
        sp.REPO_URL = self.repo.as_uri()
        sp.running_commands = lambda: ["/usr/libexec/something", "zsh"]
        os.environ["FAKE_CALLS"] = os.fspath(self.calls)

    def tearDown(self) -> None:
        sp.REPO_URL, sp.running_commands, env = self.saved
        os.environ.clear()
        os.environ.update(env)
        self.tmp.cleanup()

    def commit(self) -> str:
        subprocess.run([*self.git, "add", "-A"], check=True)
        subprocess.run([*self.git, "commit", "-q", "--allow-empty", "-m", "c"], check=True)
        return subprocess.run([*self.git, "rev-parse", "HEAD"], check=True, capture_output=True,
                              text=True).stdout.strip()

    def record(self, store: Path) -> None:
        store.mkdir(parents=True, exist_ok=True)
        (store / sp.SOURCE).write_text(json.dumps({"prefix": "admission-derived-data-v1-macOS-ARM64-fp-"}))

    def call_lines(self) -> list[str]:
        return self.calls.read_text().splitlines() if self.calls.exists() else []


class SeedPrefetchTest(PrefetchBase):
    def test_a_mini_no_owned_job_has_recorded_is_skipped(self):
        result = sp.run(True, self.state)
        self.assertEqual(result["state"], "skip")
        self.assertFalse((self.state / ".prefetch").exists())

    def test_a_busy_mini_fetches_under_the_governor_and_never_at_a_fixed_rate(self):
        self.record(self.state)
        sp.running_commands = lambda: ["/Users/cmux/actions-runner-glaeda/bin/Runner.Worker spawnclient 1 2"]
        os.environ["FAKE_DISTANCE"] = "1"
        (self.state / ".prefetch/curl-busy").mkdir(parents=True)  # an older version's fixed-rate .curlrc
        (self.state / ".prefetch/curl-busy/.curlrc").write_text("limit-rate = 3M\n")
        os.environ["CURL_HOME"] = os.fspath(self.state / ".prefetch/curl-busy")
        result = sp.run(True, self.state)["results"][os.fspath(self.state)]
        self.assertIn("Runner.Worker", result["busy"])
        self.assertEqual(result["paced"]["rate"], sp.BUSY_RATE)
        sp.running_commands = lambda: ["zsh"]
        result = sp.run(True, self.state)["results"][os.fspath(self.state)]
        self.assertNotIn("busy", result)
        self.assertEqual(result["paced"]["paused_seconds"], 0)
        self.assertEqual([line.rsplit(" curlrc=", 1)[1] for line in self.call_lines()], ["", ""])
        self.assertFalse((self.state / ".prefetch/curl-busy").exists())

    def keep_seed(self, store: Path, commit: str) -> None:
        width = os.sysconf("SC_NPROCESSORS_ONLN")
        (store / "seeds" / f"admission-derived-data-v1-macOS-ARM64-fp-j{width}-{commit}").mkdir(parents=True)

    def write(self, path: str, text: str = "x") -> None:
        (self.repo / path).parent.mkdir(parents=True, exist_ok=True)
        (self.repo / path).write_text(text)

    def test_a_kept_seed_near_main_skips_the_download(self):
        self.record(self.state)
        kept = self.commit()
        self.keep_seed(self.state, kept)
        for n in range(sp.NEAR_APP_SWIFT_FILES):
            self.write(f"Sources/File{n}.swift")
        # Not app sources: tests, docs. A path git would quote still counts as a file.
        self.write("cmuxTests/ATests.swift")
        self.write("Packages/macOS/CmuxFoundation/Tests/CmuxFoundationTests/ATests.swift")
        self.write("docs/a.md")
        self.write("Sources/\u00e9 tab\t.md")
        self.commit()
        result = sp.run(True, self.state)["results"][os.fspath(self.state)]
        self.assertEqual((result["fetched"], result["app_swift_files"]), ("false", sp.NEAR_APP_SWIFT_FILES))
        self.assertEqual(self.call_lines(), [])
        # One more app Swift file is past the bar: fetch.
        self.write("Sources/OneMore.swift")
        self.commit()
        sp.run(True, self.state)
        self.assertEqual(len(self.call_lines()), 1)

    def test_a_package_source_change_fetches_however_small(self):
        for path in ("Packages/macOS/CmuxFoundation/Sources/CmuxFoundation/A.swift",
                     # The macOS app imports several iOS packages (CmuxMobileRPC, ...).
                     "Packages/iOS/CmuxMobileRPC/Sources/CmuxMobileRPC/A.swift",
                     "Packages/macOS/CmuxFoundation/Sources/CmuxFoundation/\u00e9 b.swift"):
            with self.subTest(path=path):
                self.calls.unlink(missing_ok=True)
                self.record(self.state)
                kept = self.commit()
                shutil.rmtree(self.state / "seeds", ignore_errors=True)
                self.keep_seed(self.state, kept)
                self.write(path, path)
                self.commit()
                sp.run(True, self.state)
                self.assertEqual(len(self.call_lines()), 1)

    def test_a_quoted_app_path_counts(self):
        self.record(self.state)
        kept = self.commit()
        self.keep_seed(self.state, kept)
        changes = sp.app_swift_changes(self.state / ".prefetch/cmux.git", kept, kept)
        self.assertIsNone(changes)  # no mirror yet: cannot compare, so never near
        sp.run(True, self.state)  # builds the mirror
        for n in range(sp.NEAR_APP_SWIFT_FILES + 1):
            self.write(f"Sources/\u00e9{n}.swift")
        head = self.commit()
        sp.main_head(self.state / ".prefetch")
        self.assertEqual(sp.app_swift_changes(self.state / ".prefetch/cmux.git", kept, head),
                         (sp.NEAR_APP_SWIFT_FILES + 1, False))

    def test_a_submodule_bump_counts_as_a_package_change(self):
        self.record(self.state)
        sub = Path(self.tmp.name) / "sub"
        subprocess.run(["git", "init", "-q", "-b", "main", os.fspath(sub)], check=True)
        subprocess.run(["git", "-C", os.fspath(sub), "-c", "user.name=t", "-c", "user.email=t@t",
                        "commit", "-q", "--allow-empty", "-m", "s"], check=True)
        kept = self.commit()
        self.keep_seed(self.state, kept)
        subprocess.run([*self.git, "-c", "protocol.file.allow=always", "submodule", "add", "-q",
                        sub.as_uri(), "vendor/bonsplit"], check=True)
        head = self.commit()
        sp.run(True, self.state)
        self.assertEqual(len(self.call_lines()), 1)
        self.assertEqual(sp.app_swift_changes(self.state / ".prefetch/cmux.git", kept, head), (0, True))

    def test_a_seed_off_mains_history_does_not_count(self):
        self.record(self.state)
        self.keep_seed(self.state, "0" * 40)
        self.commit()
        sp.run(True, self.state)
        self.assertEqual(len(self.call_lines()), 1)

    def test_plan_changes_nothing(self):
        self.record(self.state)
        result = sp.run(False, self.state)
        self.assertEqual((result["state"], result["would_fetch"]), ("plan", [os.fspath(self.state)]))
        self.assertEqual(self.call_lines(), [])

    def test_every_recorded_root_fetches_main_head_once(self):
        self.record(self.state)
        self.record(self.state / "cmux-ci-2")
        (self.state / "cmux-ci-3").mkdir()  # no job has recorded this root
        head = self.commit()
        result = sp.run(True, self.state)
        self.assertEqual((result["state"], result["head"]), ("applied", head))
        calls = self.call_lines()
        self.assertEqual([line.split()[:3] for line in calls],
                         [["prefetch", os.fspath(self.state), head],
                          ["prefetch", os.fspath(self.state / "cmux-ci-2"), head]])
        self.assertTrue(calls[0].endswith("git=" + os.fspath(self.state / ".prefetch/cmux.git") + " curlrc="))
        self.assertIn(" url=https://ci-cache.cmux.com ", calls[0])
        # Both roots have HEAD's own seed: the next run only fetches git.
        self.assertEqual(sp.run(True, self.state)["state"], "current")
        self.assertEqual(len(self.call_lines()), 2)
        # A new main commit fetches again, from freshly extracted scripts.
        newer = self.commit()
        self.assertEqual(sp.run(True, self.state)["head"], newer)
        self.assertEqual(len(self.call_lines()), 4)
        self.assertEqual(sorted(p.name for p in (self.state / ".prefetch").glob("scripts-*")),
                         sorted({f"scripts-{head[:12]}", f"scripts-{newer[:12]}"}))

    def test_a_seed_behind_head_is_retried_until_heads_own_lands(self):
        self.record(self.state)
        os.environ["FAKE_DISTANCE"] = "1"
        sp.run(True, self.state)
        sp.run(True, self.state)
        self.assertEqual(len(self.call_lines()), 2)
        os.environ["FAKE_DISTANCE"] = "0"
        sp.run(True, self.state)
        self.assertEqual(sp.run(True, self.state)["state"], "current")
        self.assertEqual(len(self.call_lines()), 3)

    def test_a_broken_mirror_is_rebuilt_on_the_next_run(self):
        self.record(self.state)
        sp.run(True, self.state)
        # A fetch killed mid-way leaves a lock that makes every later fetch fail.
        (self.state / ".prefetch/cmux.git/shallow.lock").write_text("")
        self.commit()
        failed = sp.run(True, self.state)
        self.assertEqual(failed["state"], "error")
        self.assertIn("shallow.lock", failed["reason"])
        self.assertEqual(sp.run(True, self.state)["state"], "applied")

    def test_a_failed_prefetch_is_reported_and_retried(self):
        self.record(self.state)
        (self.repo / "scripts/ci/seed_derived_data.py").write_text("import sys\nsys.exit('boom')\n")
        self.commit()
        result = sp.run(True, self.state)
        outcome = result["results"][os.fspath(self.state)]
        self.assertEqual(outcome["fetched"], "false")
        self.assertIn("boom", outcome["reason"])
        self.assertNotEqual(sp.run(True, self.state)["state"], "current")


    def test_a_killed_run_stops_its_download(self):
        # launchd's bootout signals only the job's own process group; the download runs in another.
        base = Path(self.tmp.name)
        scripts = base / "fake-scripts"
        (scripts / "scripts/ci").mkdir(parents=True)
        pidfile = base / "download.pid"
        (scripts / "scripts/ci/seed_derived_data.py").write_text(
            "import os, subprocess, sys, time\n"
            "d = subprocess.Popen(['sleep', '300'])\n"
            f"open({os.fspath(pidfile)!r}, 'w').write(str(d.pid))\n"
            "time.sleep(300)\n")
        runner = subprocess.Popen([sys.executable, "-c",
                                   "import importlib.machinery, sys; from pathlib import Path; "
                                   f"m = importlib.machinery.SourceFileLoader('p', {os.fspath(ROOT / 'scripts/glaeda-seed-prefetch')!r}).load_module(); "
                                   f"m.prefetch(Path({os.fspath(scripts)!r}), Path({os.fspath(base)!r}), 'x', Path({os.fspath(base / 'm')!r}))"])
        deadline = time.time() + 20
        while not (pidfile.exists() and pidfile.read_text()) and time.time() < deadline:
            time.sleep(0.1)
        download = int(pidfile.read_text())
        runner.terminate()
        self.assertEqual(runner.wait(timeout=20), 128 + signal.SIGTERM)
        deadline = time.time() + 10
        while time.time() < deadline:
            try:
                os.kill(download, 0)
            except ProcessLookupError:
                break
            time.sleep(0.1)
        else:
            os.kill(download, signal.SIGKILL)
            self.fail("the download outlived the killed run")


class GovernorTest(unittest.TestCase):
    """The R2 download is paused only while a job's own traffic is heavy, and only when ahead of BUSY_RATE."""

    def setUp(self) -> None:
        self.saved = (sp.HOT_HOLD, sp.JOB_CHECK_SECONDS, sp.WINDOW)
        sp.HOT_HOLD, sp.JOB_CHECK_SECONDS, sp.WINDOW = 0.3, 0.0, 0.2
        self.proc = subprocess.Popen(["sleep", "60"], start_new_session=True)
        self.received = 0
        self.own = 0
        self.job = "Runner.Worker"

    def tearDown(self) -> None:
        sp.HOT_HOLD, sp.JOB_CHECK_SECONDS, sp.WINDOW = self.saved
        self.proc.kill()
        self.proc.wait()

    def stopped(self) -> bool:
        out = subprocess.run(["ps", "-o", "stat=", "-p", str(self.proc.pid)], capture_output=True, text=True)
        return out.stdout.strip().startswith("T")

    def governor(self, rate: int = 1000) -> "sp.Governor":
        return sp.Governor(self.proc.pid, rate, lambda: self.job,
                           read_interface=lambda: self.received, read_own=lambda: self.own, tick=0.05)

    def run_for(self, seconds: float, job_bps: int, own_bps: int) -> None:
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            time.sleep(0.02)
            self.own += int(own_bps * 0.02)
            self.received += int((job_bps + own_bps * sp.WIRE_OVERHEAD) * 0.02)

    def test_a_download_beside_a_job_that_is_not_downloading_runs_free(self):
        governor = self.governor().start()
        try:
            self.run_for(0.6, job_bps=10_000, own_bps=50_000_000)
            self.assertFalse(self.stopped())
        finally:
            governor.stop()
        self.assertEqual(governor.stats["paused_seconds"], 0)
        self.assertEqual(governor.stats["hot_seconds"], 0)

    def test_a_job_downloading_holds_the_download_to_the_busy_rate_then_frees_it(self):
        governor = self.governor().start()
        try:
            self.run_for(0.6, job_bps=sp.FOREIGN_BUSY_BPS * 8, own_bps=50_000_000)
            self.assertTrue(self.stopped())
            self.run_for(1.0, job_bps=0, own_bps=0)  # the job starts compiling
            self.assertFalse(self.stopped())
        finally:
            governor.stop()
        self.assertGreater(governor.stats["paused_seconds"], 0)
        self.assertGreaterEqual(governor.stats["peak_job_bps"], sp.FOREIGN_BUSY_BPS * 6)
        self.assertFalse(self.stopped())

    def test_header_overhead_on_a_fast_download_is_not_the_jobs_traffic(self):
        governor = self.governor().start()
        try:
            end = time.monotonic() + 0.6
            while time.monotonic() < end:  # 1% more header than WIRE_OVERHEAD assumes, at 100 MB/s
                time.sleep(0.02)
                self.own += 2_000_000
                self.received += int(2_000_000 * (sp.WIRE_OVERHEAD + 0.01))
            self.assertFalse(self.stopped())
        finally:
            governor.stop()
        self.assertEqual(governor.stats["hot_seconds"], 0)

    def test_no_job_means_no_pause_whatever_the_traffic(self):
        self.job = ""
        governor = self.governor().start()
        try:
            self.run_for(0.6, job_bps=sp.FOREIGN_BUSY_BPS * 10, own_bps=50_000_000)
            self.assertFalse(self.stopped())
        finally:
            governor.stop()

    def test_unreadable_counters_count_a_job_as_downloading(self):
        governor = sp.Governor(self.proc.pid, 1000, lambda: "job",
                               read_interface=lambda: None, read_own=lambda: self.own, tick=0.05)
        governor.start()
        try:
            self.run_for(0.4, job_bps=0, own_bps=50_000_000)
            self.assertTrue(self.stopped())
        finally:
            governor.stop()
        self.assertFalse(self.stopped())  # stopping the governor always resumes the download
        governor.set_paused(True)  # and it never signals the group again
        self.assertFalse(self.stopped())

    def test_download_bytes_follows_the_curls_in_the_group_by_their_output_file(self):
        base = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, base, ignore_errors=True)
        archive = base / "tmp.X" / "archive"
        archive.parent.mkdir()
        archive.write_bytes(b"x" * 100)
        pgid = self.proc.pid
        lines = [f"curl --fail --silent -o {archive} https://ci-cache.cmux.com/v1/k.tar.zst", "bash r2-cache.sh restore"]
        saved = sp.running_group_commands
        sp.running_group_commands = lambda group: lines if group == pgid else []
        try:
            counter = sp.DownloadBytes(pgid)
            self.assertEqual(counter(), 100)
            archive.write_bytes(b"x" * 350)
            self.assertEqual(counter(), 350)
            archive.unlink()  # unpacked and removed: nothing more, nothing less
            self.assertEqual(counter(), 350)
        finally:
            sp.running_group_commands = saved
        self.assertIn(f"sleep 60", " ".join(sp.running_group_commands(pgid)))

    def test_interface_bytes_reads_only_physical_links(self):
        out = subprocess.CompletedProcess([], 0, (
            "Name       Mtu   Network       Address            Ipkts Ierrs     Ibytes    Opkts Oerrs     Obytes  Coll\n"
            "lo0        16384 <Link#1>                      5 0 999999 5 0 999999 0\n"
            "en0        1500  <Link#7>    d0:11:e5:34:ce:ba 2 0 1000 3 0 50 0\n"
            "en0        1500  172.20.20/22 172.20.21.194    2 - 1000 3 - 50 -\n"
            "en5        1500  <Link#8>    ce:0c:b3:4b:f3:b0 0 0 20 0 0 0 0\n"
            "utun3      1280  <Link#20>                     9 0 777777 9 0 777 0\n"
            "en9        1500  <Link#21>                     1 0 300 1 0 3 0\n"), "")
        saved = subprocess.run
        subprocess.run = lambda *a, **k: out
        try:
            self.assertEqual(sp.interface_bytes(), 1320)
        finally:
            subprocess.run = saved


# Stands in for /usr/bin/ssh to the seeder. Logs its argv, then acts as FAKE_SSH_MODE says: "serve" runs the
# real glaeda-seed-serve as the forced command would, over FAKE_SEEDER_STATE.
FAKE_SSH = """import json, os, sys, tarfile, io, time
address, request = sys.argv[-2], sys.argv[-1]
with open(os.environ["FAKE_SSH_LOG"], "a") as log:
    log.write(json.dumps(sys.argv[1:]) + "\\n")
mode = os.environ.get("FAKE_SSH_MODE", "serve")
if address in os.environ.get("FAKE_UNREACHABLE", "").split(","):
    sys.stderr.write(f"ssh: connect to host {address} port 22: No route to host\\n")
    sys.exit(255)
def header(line):
    os.write(1, ("glaeda-seed-serve 1 " + line + "\\n").encode())
def tar_of(entries):
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w") as tar:
        for name, data in entries:
            info = tarfile.TarInfo(name); info.size = len(data); tar.addfile(info, io.BytesIO(data))
    return buf.getvalue()
keys = request.split()[2:]
once = os.environ.get("FAKE_ONCE")
if mode in ("busy-once", "cut-once"):
    first = once and not os.path.exists(once)
    if first:
        open(once, "w").write("")
    if first and mode == "busy-once":
        header("busy"); sys.exit(4)
    if first and mode == "cut-once":  # the seeder's serve timeout kills the stream part way
        header(f"hit {keys[0]} tar")
        os.write(1, tar_of([(keys[0] + "/cmux-seed-input-mtimes.json", b"{}"), (keys[0] + "/big", b"x" * 200000)])[:70000])
        sys.exit(255)
    mode = "serve"
if mode == "busy":
    header("busy"); sys.exit(4)
if mode == "zstd-corrupt":  # a whole zstd stream with one byte flipped in its content
    import subprocess
    body = subprocess.run([os.environ["FAKE_ZSTD"], "-q", "-c"], input=tar_of([
        (keys[0] + "/cmux-seed-input-mtimes.json", b"{}"), (keys[0] + "/big", os.urandom(200000))]),
        capture_output=True, check=True).stdout
    body = body[:len(body) // 2] + bytes([body[len(body) // 2] ^ 0xFF]) + body[len(body) // 2 + 1:]
    header(f"hit {keys[0]} zstd"); os.write(1, body); sys.exit(0)
if mode == "serve":
    race = os.environ.get("FAKE_RACE_TARGET")
    if race:  # a job stashes the same seed while the stream runs
        os.makedirs(race, exist_ok=True)
        open(os.path.join(race, "cmux-seed-input-mtimes.json"), "w").write("{}")
        open(os.path.join(race, "stashed-by-job"), "w").write("")
    os.environ["SSH_ORIGINAL_COMMAND"] = request
    os.execv(sys.executable, [sys.executable, os.environ["FAKE_SERVE"], "--state", os.environ["FAKE_SEEDER_STATE"],
                              "--run-dir", os.environ["FAKE_SERVE_RUN"], "--receipt", os.environ["FAKE_RECEIPT"]])
def hostile(entries):
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w", format=tarfile.GNU_FORMAT) as tar:
        for name, kind, value in entries:
            info = tarfile.TarInfo(name)
            if kind == "file":
                info.size = len(value); tar.addfile(info, io.BytesIO(value))
            else:
                info.type = tarfile.SYMTYPE if kind == "symlink" else tarfile.LNKTYPE
                info.linkname = value; tar.addfile(info)
    return buf.getvalue()
outside = os.environ.get("FAKE_OUTSIDE", "/nonexistent")
manifest = (keys[0] + "/cmux-seed-input-mtimes.json", "file", b"{}") if keys else None
if mode == "dotdot":
    header(f"hit {keys[0]} tar")
    os.write(1, hostile([manifest, (keys[0] + "/../../../escape-dotdot", "file", b"x"),
                         ("../escape-top", "file", b"x")])); sys.exit(0)
if mode == "symlink":
    header(f"hit {keys[0]} tar")
    os.write(1, hostile([manifest, (keys[0] + "/link", "symlink", outside),
                         (keys[0] + "/link/escape-symlink", "file", b"x")])); sys.exit(0)
if mode == "hardlink":
    header(f"hit {keys[0]} tar")
    os.write(1, hostile([manifest, (keys[0] + "/hard", "hardlink", outside + "/target"),
                         (keys[0] + "/hard", "file", b"pwned")])); sys.exit(0)
if mode == "noisy":  # a login shell that prints before the forced command runs
    os.write(1, b"Last login: yesterday\\nwelcome to cmux15\\n")
    os.environ["SSH_ORIGINAL_COMMAND"] = request
    os.execv(sys.executable, [sys.executable, os.environ["FAKE_SERVE"], "--state", os.environ["FAKE_SEEDER_STATE"],
                              "--run-dir", os.environ["FAKE_SERVE_RUN"], "--receipt", os.environ["FAKE_RECEIPT"]])
if mode == "silent":
    time.sleep(60)
if mode == "garbage":
    header(f"hit {keys[0]} tar"); os.write(1, b"not a tar archive" * 100); sys.exit(0)
if mode == "extra":
    header(f"hit {keys[0]} tar")
    os.write(1, tar_of([(keys[0] + "/cmux-seed-input-mtimes.json", b"{}"), ("elsewhere/x", b"x")])); sys.exit(0)
if mode == "no-manifest":
    header(f"hit {keys[0]} tar"); os.write(1, tar_of([(keys[0] + "/Build/x", b"x")])); sys.exit(0)
if mode == "unasked":
    header("hit " + "admission-derived-data-v1-macOS-ARM64-" + "1" * 32 + "-j14-" + "2" * 40 + " tar"); sys.exit(0)
if mode == "padded":  # a valid seed, then more padding than bsdtar reads before it exits
    header(f"hit {keys[0]} tar")
    os.write(1, tar_of([(keys[0] + "/cmux-seed-input-mtimes.json", b"{}")]) + bytes(4 << 20)); sys.exit(0)
if mode == "big":
    header(f"hit {keys[0]} tar")
    os.write(1, tar_of([(keys[0] + "/cmux-seed-input-mtimes.json", b"{}"), (keys[0] + "/big", b"x" * 200000)]))
    sys.exit(0)
sys.exit(1)
"""

FINGERPRINT = "0" * 32
PREFIX = f"admission-derived-data-v1-macOS-ARM64-{FINGERPRINT}-"


class LanSeedTest(PrefetchBase):
    """Seeds over the LAN from the trusted seeder, and R2 after it."""

    def setUp(self) -> None:
        super().setUp()
        base = Path(self.tmp.name)
        self.width = os.sysconf("SC_NPROCESSORS_ONLN")
        self.seeder = base / "seeder/ci"
        (self.seeder / "seeds").mkdir(parents=True)
        receipt = base / "seeder/receipt.json"
        receipt.write_text(json.dumps({"member": {"trustedRef": "refs/heads/main"}}))
        conf = base / "seed-lan"
        conf.mkdir()
        (conf / "id_ed25519").write_text("not a real key")
        (conf / "known_hosts").write_text("glaeda-seeder ssh-ed25519 AAAA\n")
        (conf / "config.json").write_text(json.dumps({"schema": 1, "user": "cmux", "addresses": ["172.20.21.202"]}))
        self.conf = conf
        fake = base / "fake-ssh"
        fake.write_text(f"#!{sys.executable}\n" + FAKE_SSH)
        fake.chmod(0o755)
        self.ssh_log = base / "ssh-log"
        self.saved_lan = (sp.LAN_CONFIG, sp.SSH, sp.zstd_tool, sp.LAN_MIN_FREE_BYTES, sp.LAN_MAX_BYTES,
                          sp.LAN_BUSY_BACKOFF, sp.LAN_RETRY_BACKOFF)
        sp.LAN_CONFIG, sp.SSH = conf / "config.json", os.fspath(fake)
        sp.zstd_tool = lambda: None
        sp.LAN_MIN_FREE_BYTES = 0
        sp.LAN_BUSY_BACKOFF = sp.LAN_RETRY_BACKOFF = (0.0, 0.0)
        os.environ["FAKE_ONCE"] = os.fspath(base / "once")
        os.environ.update({"FAKE_SSH_LOG": os.fspath(self.ssh_log), "FAKE_SERVE": os.fspath(ROOT / "scripts/glaeda-seed-serve"),
                           "FAKE_SEEDER_STATE": os.fspath(self.seeder), "FAKE_SERVE_RUN": os.fspath(base / "serve-run"),
                           "FAKE_RECEIPT": os.fspath(receipt)})

    def tearDown(self) -> None:
        (sp.LAN_CONFIG, sp.SSH, sp.zstd_tool, sp.LAN_MIN_FREE_BYTES, sp.LAN_MAX_BYTES,
         sp.LAN_BUSY_BACKOFF, sp.LAN_RETRY_BACKOFF) = self.saved_lan
        super().tearDown()

    def record(self, store: Path) -> None:
        store.mkdir(parents=True, exist_ok=True)
        (store / sp.SOURCE).write_text(json.dumps({"prefix": PREFIX}))

    def key(self, commit: str) -> str:
        return f"{PREFIX}j{self.width}-{commit}"

    def seeder_keeps(self, commit: str, root: str = "") -> Path:
        seed = self.seeder / root / "seeds" / self.key(commit)
        (seed / "Build/Products").mkdir(parents=True)
        (seed / sp.MANIFEST).write_text('{"a": 1}')
        (seed / "Build/Products/app").write_bytes(os.urandom(4096))
        return seed

    def mirror(self) -> tuple[Path, str]:
        return sp.main_head(self.state / ".prefetch")

    def lan(self, **kwargs) -> dict:
        mirror, head = self.mirror()
        return sp.lan_fetch(self.state, mirror, head, sp.lan_config(), **kwargs)

    def assert_clean(self) -> None:
        self.assertEqual(list((self.state / "seeds").glob(".lan-*")), [])

    def test_lan_seed_lands_first_then_r2_finds_it_kept(self):
        self.record(self.state)
        head = self.commit()
        served = self.seeder_keeps(head)
        result = sp.run(True, self.state)["results"][os.fspath(self.state)]
        lan = result["lan"]
        self.assertEqual((lan["fetched"], lan["key"], lan["distance"], lan["codec"], lan["address"]),
                         ("true", self.key(head), 0, "tar", "172.20.21.202"))
        kept = self.state / "seeds" / self.key(head)
        self.assertEqual((kept / "Build/Products/app").read_bytes(), (served / "Build/Products/app").read_bytes())
        self.assertTrue(sp.seed_complete(kept))
        self.assert_clean()
        self.assertEqual(len(self.call_lines()), 1)  # R2's prefetch still runs, and prunes
        argv = json.loads(self.ssh_log.read_text().splitlines()[0])
        self.assertEqual(argv[-1].split()[:2], ["seed-v1", "tar"])
        self.assertEqual(argv[-1].split()[2], self.key(head))

    def test_the_seeders_nearest_kept_seed_is_served(self):
        self.record(self.state)
        older = self.commit()
        self.seeder_keeps(older, "cmux-ci-2")  # any root's cache on the seeder
        self.commit()
        lan = self.lan()
        self.assertEqual((lan["fetched"], lan["key"], lan["distance"]), ("true", self.key(older), 1))

    def test_only_keys_nearer_than_a_kept_seed_are_asked_for(self):
        self.record(self.state)
        far = self.commit()
        near = self.commit()
        head = self.commit()
        mirror, _ = self.mirror()
        (self.state / "seeds" / self.key(near)).mkdir(parents=True)  # incomplete: no manifest
        self.assertEqual(sp.lan_candidates(mirror, self.state, head)[0][:3],
                         [self.key(head), self.key(near), self.key(far)])
        (self.state / "seeds" / self.key(near) / sp.MANIFEST).write_text("{}")
        self.assertEqual(sp.lan_candidates(mirror, self.state, head)[0], [self.key(head)])
        (self.state / "seeds" / self.key(head)).mkdir()
        (self.state / "seeds" / self.key(head) / sp.MANIFEST).write_text("{}")
        self.assertEqual(sp.lan_candidates(mirror, self.state, head), ([], "the nearest seed is kept here"))

    def test_a_prefix_that_makes_no_seed_key_asks_nothing(self):
        self.state.mkdir(exist_ok=True)
        (self.state / sp.SOURCE).write_text(json.dumps({"prefix": "admission-derived-data-v1-x/../-"}))
        head = self.commit()
        mirror, _ = self.mirror()
        self.assertEqual(sp.lan_candidates(mirror, self.state, head)[0], [])

    def test_a_miss_falls_back_to_r2(self):
        self.record(self.state)
        self.commit()
        result = sp.run(True, self.state)["results"][os.fspath(self.state)]
        self.assertEqual(result["lan"]["reason"], "seeder: miss")
        self.assertEqual(result["fetched"], "true")  # R2's answer
        self.assertEqual(len(self.call_lines()), 1)
        self.assert_clean()

    def test_an_untrusted_seeder_refuses_and_r2_serves(self):
        self.record(self.state)
        self.seeder_keeps(self.commit())
        Path(os.environ["FAKE_RECEIPT"]).write_text(json.dumps({"member": {}}))
        result = sp.run(True, self.state)["results"][os.fspath(self.state)]
        self.assertIn("refused", result["lan"]["reason"])
        self.assertEqual(len(self.call_lines()), 1)

    def test_an_unreachable_address_tries_the_next(self):
        self.record(self.state)
        head = self.commit()
        self.seeder_keeps(head)
        (self.conf / "config.json").write_text(json.dumps({"schema": 1, "user": "cmux",
                                                           "addresses": ["172.20.21.202", "cmux15.local"]}))
        os.environ["FAKE_UNREACHABLE"] = "172.20.21.202"
        lan = self.lan()
        self.assertEqual((lan["fetched"], lan["address"]), ("true", "cmux15.local"))
        os.environ["FAKE_UNREACHABLE"] = "172.20.21.202,cmux15.local"
        shutil.rmtree(self.state / "seeds" / self.key(head))
        lan = self.lan()
        self.assertEqual(lan["fetched"], "false")
        self.assertIn("No route to host", lan["reason"])
        self.assertIn("Local Network Privacy", lan["reason"])
        self.assert_clean()

    def test_every_bad_stream_leaves_the_cache_as_it_was(self):
        self.record(self.state)
        head = self.commit()
        for mode, reason in (("garbage", "stream exits"), ("extra", "not one complete seed"),
                             ("no-manifest", "not one complete seed"),
                             ("unasked", "not asked for"), ("nonsense", "unreachable")):
            with self.subTest(mode=mode):
                os.environ["FAKE_SSH_MODE"] = mode
                lan = self.lan()
                self.assertEqual(lan["fetched"], "false")
                self.assertIn(reason, lan["reason"])
                self.assertFalse((self.state / "seeds" / self.key(head)).exists())
                self.assertEqual(sorted(p.name for p in (self.state / "seeds").iterdir()), [])

    def test_a_hostile_tar_writes_nothing_outside_staging(self):
        self.record(self.state)
        head = self.commit()
        base = Path(self.tmp.name)
        outside = base / "outside"
        outside.mkdir()
        (outside / "target").write_text("original")
        os.environ["FAKE_OUTSIDE"] = os.fspath(outside)
        for mode in ("dotdot", "symlink", "hardlink"):
            with self.subTest(mode=mode):
                os.environ["FAKE_SSH_MODE"] = mode
                lan = self.lan()
                self.assertEqual([p for p in base.rglob("escape-*")], [])
                self.assertEqual(sorted(p.name for p in outside.iterdir()), ["target"])
                self.assertEqual(((outside / "target").read_text(), (outside / "target").stat().st_nlink),
                                 ("original", 1))
                self.assert_clean()
                if lan["fetched"] == "true":  # the tool refused the escape and kept the rest
                    seed = self.state / "seeds" / self.key(head)
                    self.assertTrue(sp.seed_complete(seed))
                    self.assertEqual([p for p in seed.rglob("*") if p.is_symlink() and
                                      not os.path.realpath(p).startswith(os.path.realpath(seed))], [])
                    shutil.rmtree(seed)

    def test_lines_a_login_shell_prints_before_the_header_are_skipped(self):
        self.record(self.state)
        head = self.commit()
        self.seeder_keeps(head)
        os.environ["FAKE_SSH_MODE"] = "noisy"
        lan = self.lan()
        self.assertEqual((lan["fetched"], lan["key"]), ("true", self.key(head)))
        r, w = os.pipe()
        os.write(w, b"x" * (sp.HEADER_SCAN_BYTES + 10) + b"\nglaeda-seed-serve 1 hit k tar\n")
        os.close(w)
        self.assertEqual(sp.read_header(r), b"")  # only the first HEADER_SCAN_BYTES are searched
        os.close(r)

    def test_a_busy_mini_paces_the_lan_extraction(self):
        self.record(self.state)
        head = self.commit()
        os.environ["FAKE_SSH_MODE"] = "big"
        started = time.monotonic()
        lan = self.lan(rate=100000)  # ~210 KB of tar at 100 KB/s
        self.assertEqual(lan["fetched"], "true", lan)
        self.assertGreater(time.monotonic() - started, 1.5)
        shutil.rmtree(self.state / "seeds" / self.key(head))
        sp.running_commands = lambda: ["/Users/cmux/actions-runner-glaeda/bin/Runner.Worker spawnclient 1 2"]
        saved = sp.LAN_BUSY_RATE
        sp.LAN_BUSY_RATE = 10 ** 9
        try:
            result = sp.run(True, self.state)["results"][os.fspath(self.state)]
        finally:
            sp.LAN_BUSY_RATE = saved
        self.assertIn("paced", result["lan"])
        self.assertIn("busy", result)

    def test_a_busy_seeder_is_asked_again_and_serves(self):
        self.record(self.state)
        head = self.commit()
        self.seeder_keeps(head)
        os.environ["FAKE_SSH_MODE"] = "busy-once"
        lan = self.lan()
        self.assertEqual((lan["fetched"], lan["key"], lan["attempts"]), ("true", self.key(head), 2), lan)
        self.assertTrue(sp.seed_complete(self.state / "seeds" / self.key(head)))
        self.assert_clean()

    def test_a_seeder_busy_every_time_gives_up_after_the_attempts(self):
        self.record(self.state)
        self.commit()
        os.environ["FAKE_SSH_MODE"] = "busy"
        lan = self.lan()
        self.assertEqual((lan["fetched"], lan["reason"], lan["attempts"]), ("false", "seeder: busy", sp.LAN_ATTEMPTS))
        self.assertEqual(len(self.ssh_log.read_text().splitlines()), sp.LAN_ATTEMPTS)
        self.assert_clean()

    def test_a_busy_seeder_is_waited_for_only_within_the_budget(self):
        self.record(self.state)
        self.commit()
        os.environ["FAKE_SSH_MODE"] = "busy"
        saved = sp.LAN_BUSY_BUDGET
        sp.LAN_BUSY_BUDGET = 0.0
        try:
            lan = self.lan()
        finally:
            sp.LAN_BUSY_BUDGET = saved
        self.assertEqual((lan["reason"], lan["attempts"]), ("seeder: busy", 1))

    def test_a_reaped_process_is_never_signalled(self):
        run = sp.Pipeline()
        done = run.spawn(["true"])
        run.reap()
        alive = run.spawn(["sleep", "30"])
        signalled = []
        saved = os.killpg
        os.killpg = lambda pgid, signum: signalled.append(pgid)
        try:
            run.stop()
        finally:
            os.killpg = saved
            alive.kill()
            alive.wait()
        self.assertEqual(signalled, [alive.pid])
        self.assertNotIn(done.pid, signalled)

    def test_a_killed_runs_lan_staging_is_swept_for_every_root(self):
        self.record(self.state)
        self.record(self.state / "cmux-ci-2")
        head = self.commit()
        width = os.sysconf("SC_NPROCESSORS_ONLN")
        for store in (self.state, self.state / "cmux-ci-2"):  # kept: nothing to ask the seeder for
            seed = store / "seeds" / f"{PREFIX}j{width}-{head}"
            seed.mkdir(parents=True)
            (seed / sp.MANIFEST).write_text("{}")
            (store / "seeds/.lan-4242/x").mkdir(parents=True)
            (store / "seeds/.lan-4242/stream").write_bytes(b"z" * 1000)
        sp.run(True, self.state)
        self.assertEqual(list(self.state.glob("**/.lan-*")), [])

    def test_a_stream_cut_part_way_is_fetched_again_and_nothing_partial_lands(self):
        self.record(self.state)
        head = self.commit()
        self.seeder_keeps(head)
        os.environ["FAKE_SSH_MODE"] = "cut-once"
        lan = self.lan()
        self.assertEqual((lan["fetched"], lan["attempts"]), ("true", 2), lan)
        seed = self.state / "seeds" / self.key(head)
        self.assertTrue((seed / "Build/Products/app").is_file())
        self.assertFalse((seed / "big").exists())  # nothing from the cut stream
        self.assert_clean()

    def test_a_zstd_stream_that_fails_its_checksum_is_never_extracted(self):
        zstd = shutil.which("zstd") or next((c for c in sp.ZSTD_CANDIDATES if os.access(c, os.X_OK)), None)
        if not zstd:
            self.skipTest("no zstd")
        sp.zstd_tool = lambda: zstd
        os.environ["FAKE_ZSTD"] = zstd
        self.record(self.state)
        self.commit()
        os.environ["FAKE_SSH_MODE"] = "zstd-corrupt"
        lan = self.lan()
        self.assertEqual(lan["fetched"], "false")
        self.assertIn("checksum", lan["reason"])
        self.assertEqual(lan["attempts"], sp.LAN_ATTEMPTS)
        self.assertEqual(sorted(p.name for p in (self.state / "seeds").iterdir()), [])

    def test_padding_tar_leaves_unread_is_drained(self):
        self.record(self.state)
        head = self.commit()
        os.environ["FAKE_SSH_MODE"] = "padded"
        lan = self.lan()
        self.assertEqual(lan["fetched"], "true", lan)
        self.assertTrue(sp.seed_complete(self.state / "seeds" / self.key(head)))

    def test_a_stream_over_the_bound_is_dropped(self):
        self.record(self.state)
        self.commit()
        os.environ["FAKE_SSH_MODE"] = "big"
        sp.LAN_MAX_BYTES = 100000
        lan = self.lan()
        self.assertIn("over 100000 bytes", lan["reason"])
        self.assertEqual(list((self.state / "seeds").iterdir()), [])

    def test_a_silent_seeder_times_out_and_is_stopped(self):
        self.record(self.state)
        self.commit()
        os.environ["FAKE_SSH_MODE"] = "silent"
        started = time.monotonic()
        lan = self.lan(timeout=1)
        self.assertLess(time.monotonic() - started, 20)
        self.assertEqual(lan["fetched"], "false")
        self.assertIn("timed out", lan["reason"])
        self.assert_clean()

    def test_a_seed_a_job_stashed_meanwhile_is_kept(self):
        self.record(self.state)
        head = self.commit()
        self.seeder_keeps(head)
        target = self.state / "seeds" / self.key(head)
        os.environ["FAKE_RACE_TARGET"] = os.fspath(target)
        lan = self.lan()
        self.assertEqual((lan["fetched"], lan["reason"]), ("false", "kept meanwhile"))
        self.assertTrue((target / "stashed-by-job").exists())
        self.assertFalse((target / "Build").exists())
        self.assert_clean()

    def test_a_killed_runs_staging_is_removed_and_the_disk_floor_holds(self):
        self.record(self.state)
        self.commit()
        stale = self.state / "seeds/.lan-99999/partial"
        stale.mkdir(parents=True)
        sp.LAN_MIN_FREE_BYTES = 1 << 62
        lan = self.lan()
        self.assertIn("free-space floor", lan["reason"])
        self.assertFalse(stale.parent.exists())
        self.assertEqual(self.ssh_log.exists(), False)

    def test_zstd_streams_when_both_ends_have_it(self):
        zstd = shutil.which("zstd") or next((c for c in sp.ZSTD_CANDIDATES if os.access(c, os.X_OK)), None)
        if not zstd:
            self.skipTest("no zstd")
        sp.zstd_tool = lambda: zstd
        self.record(self.state)
        head = self.commit()
        self.seeder_keeps(head)
        lan = self.lan()
        # The seeder answers tar when it has no zstd; either way the seed lands.
        self.assertEqual(lan["fetched"], "true")
        self.assertIn(lan["codec"], ("zstd", "tar"))
        self.assertTrue(sp.seed_complete(self.state / "seeds" / self.key(head)))

    def test_no_config_means_no_lan_step(self):
        self.record(self.state)
        self.commit()
        (self.conf / "config.json").unlink()
        result = sp.run(True, self.state)
        self.assertNotIn("lan", result["results"][os.fspath(self.state)])
        self.assertFalse(self.ssh_log.exists())

    def test_plan_reports_lan_without_contacting_the_seeder(self):
        self.record(self.state)
        self.assertEqual(sp.run(False, self.state)["lan"], "configured")
        self.assertFalse(self.ssh_log.exists())

    def test_a_malformed_config_turns_lan_off(self):
        good = {"schema": 1, "user": "cmux", "addresses": ["172.20.21.202"]}
        for bad in ({**good, "addresses": ["-oProxyCommand=sh"]}, {**good, "addresses": ["a b"]},
                    {**good, "addresses": []}, {**good, "user": "root;x"}, {**good, "schema": 2}, ["x"]):
            with self.subTest(bad=bad):
                (self.conf / "config.json").write_text(json.dumps(bad))
                self.assertIsNone(sp.lan_config())
        (self.conf / "config.json").write_text(json.dumps(good))
        (self.conf / "id_ed25519").unlink()
        self.assertIsNone(sp.lan_config())

    def test_ssh_reads_nothing_from_home_and_forwards_nothing(self):
        argv = sp.ssh_command(sp.lan_config(), "172.20.21.202", "ping-v1")
        self.assertEqual(argv[:4], [sp.SSH, "-F", "/dev/null", "-T"])
        for option in ("BatchMode=yes", "IdentitiesOnly=yes", "IdentityAgent=none", "StrictHostKeyChecking=yes",
                       "GlobalKnownHostsFile=/dev/null", "HostKeyAlias=glaeda-seeder", "ClearAllForwardings=yes",
                       "ControlPath=none", f"UserKnownHostsFile={self.conf / 'known_hosts'}"):
            self.assertIn(option, argv)
        self.assertIn("-a", argv)
        self.assertEqual(argv[-2:], ["172.20.21.202", "ping-v1"])

    def test_cmux_scripts_run_under_a_python_other_than_apples(self):
        self.assertTrue(os.access(sp.cmux_python(), os.X_OK))


if __name__ == "__main__":
    unittest.main()
