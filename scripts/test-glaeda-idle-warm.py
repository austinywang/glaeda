#!/usr/bin/env python3
"""Tests for scripts/glaeda-idle-warm."""

from __future__ import annotations

import fcntl
import importlib.machinery
import importlib.util
import json
import os
import signal
import subprocess
import sys
import tempfile
import textwrap
import threading
import time
import types
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def load(name: str, path: Path):
    loader = importlib.machinery.SourceFileLoader(name, os.fspath(path))
    spec = importlib.util.spec_from_loader(name, loader)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    loader.exec_module(module)
    return module


warm = load("glaeda_idle_warm", ROOT / "scripts" / "glaeda-idle-warm")
hook = load("glaeda_cmux_runner_hook", ROOT / "scripts" / "glaeda-cmux-runner-hook")

HEAD = "a" * 40
# Stands in for cmux's scripts/ci/owned_catch_up.sh: records its arguments and environment, then answers.
FAKE_CATCH_UP = """#!/usr/bin/env bash
echo "$@ xcode=$CMUX_CI_XCODE_APP log=$CMUX_CATCH_UP_LOG github=${GITHUB_TOKEN:-none}" >> "$FAKE_CALLS"
echo "a progress line"
sleep "${FAKE_SLEEP:-0}"
echo '{"kept": "true", "root": '"$1"', "head": "x", "phase": "done"}'
"""


class Base(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self.tmp.name)
        self.state = self.dir / "fleet" / "ci"
        self.state.mkdir(parents=True)
        self.capacity = self.dir / "fleet" / "capacity"
        self.saved = {name: getattr(warm, name) for name in
                      ("FLEET_DIR", "HOME", "KILL_SWITCH", "runner_setup", "idle_refusal", "main_head",
                       "prepare_checkout", "running", "host_denied", "console_refusal", "free_bytes")}
        self.saved_env = dict(os.environ)
        warm.FLEET_DIR = self.dir / "fleet"
        warm.HOME = self.dir
        warm.KILL_SWITCH = self.dir / "idle-warm.disabled"

    def tearDown(self) -> None:
        for name, value in self.saved.items():
            setattr(warm, name, value)
        os.environ.clear()
        os.environ.update(self.saved_env)
        self.tmp.cleanup()

    def runner(self, name: str, flags: str) -> Path:
        runner = self.dir / name
        (runner / "glaeda-hooks").mkdir(parents=True)
        (runner / "glaeda-hooks" / "job-started.sh").write_text(
            f"exec python3 {runner}/glaeda-hooks/glaeda-cmux-runner-hook job-started {flags}\n")
        (runner / "glaeda-hooks" / "glaeda-cmux-runner-hook").write_bytes(
            (ROOT / "scripts" / "glaeda-cmux-runner-hook").read_bytes())
        return runner


class GateTest(Base):
    def test_never_on_the_ios_soak_box_or_lawrences_machines(self) -> None:
        self.assertIn("never", warm.host_denied(["cmuxs-Mac-mini-5.local"]))
        self.assertIn("never", warm.host_denied(["cmuxs-mac-mini-5.lan"]))
        self.assertIn("never", warm.host_denied(["cmux12s-Mac-mini", "cmuxs-Mac-mini-5"]), "any of its names")
        self.assertIn("never", warm.host_denied(["cmux-lawrences-mac-mini"]))
        self.assertIn("never", warm.host_denied(["Lawrence’s Mac mini"]))
        self.assertEqual(warm.host_denied(["cmux12s-Mac-mini.local", "cmux12s-Mac-mini"]), "")
        self.assertEqual(warm.host_denied(["cmuxs-Mac-mini-4"]), "", "cmuxs-mac-mini-5 on the tailnet is a PR mini")

    def test_runner_setup_needs_capacity_runners_whose_hook_yields(self) -> None:
        self.assertEqual(warm.runner_setup([]), "no glaeda runner on this mini")
        xcode = self.dir / "Xcode_26.6.app"
        xcode.mkdir()
        flags = f"--min-free-gib 100 --toolchain-xcode {xcode} --capacity-units 5 --compile-slots 2 --canonical-roots 2"
        runners = [self.runner("actions-runner-glaeda", flags + " --instance 0"),
                   self.runner("actions-runner-glaeda-1", flags + " --instance 1")]
        module, scope = warm.runner_setup(runners)
        self.assertEqual(scope, {"units": 5, "roots": 2, "compile_slots": 2, "xcode": os.fspath(xcode)})
        self.assertTrue(hasattr(module, "WARM_HOLDER"))
        trusted = self.runner("actions-runner-glaeda-2", flags + " --trusted-ref refs/heads/main")
        self.assertIn("trusted-only", warm.runner_setup([*runners, trusted]))
        old = self.runner("actions-runner-glaeda-3", flags)
        hook_file = old / "glaeda-hooks" / "glaeda-cmux-runner-hook"
        hook_file.write_text(hook_file.read_text().replace("WARM_HOLDER", "OLD_NAME"))
        self.assertIn("predates the idle catch-up", warm.runner_setup([old]))
        single = self.runner("actions-runner-glaeda-4", f"--toolchain-xcode {xcode}")
        self.assertEqual(warm.runner_setup([single]), "runners are not in capacity mode")
        unpinned = self.runner("actions-runner-glaeda-5", "--capacity-units 4")
        self.assertEqual(warm.runner_setup([unpinned]), "no toolchain Xcode pin in the runner hooks")

    def test_last_job_reads_the_telemetry_tail(self) -> None:
        log = self.dir / "jobs.jsonl"
        self.assertIsNone(warm.last_job_at(log))
        log.write_text(json.dumps({"event": "completed", "started_at": 100, "ended_at": 200}) + "\nnot json\n")
        os.utime(log, (50, 50))
        self.assertEqual(warm.last_job_at(log), 200)

    def test_idle_after_three_quiet_minutes_of_compiles(self) -> None:
        self.assertEqual(warm.IDLE_S, 180)
        saved = warm.compile_state
        stub = types.SimpleNamespace(thermal_pressure_level=lambda: 1)
        try:
            warm.running = lambda: ""
            warm.compile_state = lambda now=None: (1000.0, "")
            self.assertIn("a compile ran 179 s ago", warm.idle_refusal(stub, 1179.0, self.state))
            # Past IDLE_S a later gate (load, heat, disk) answers, never the job gate.
            self.assertNotIn("a compile ran", warm.idle_refusal(stub, 1181.0, self.state))
            warm.compile_state = lambda now=None: (1.0, "macos-compile-admission on m-glaeda")
            self.assertIn("a compile is running", warm.idle_refusal(stub, 5000.0, self.state))
        finally:
            warm.compile_state = saved

    def test_only_compiles_and_xcode_runs_count_as_busy(self) -> None:
        log = self.dir / "jobs.jsonl"
        self.assertIsNone(warm.last_compile_at(log))
        log.write_text("\n".join(json.dumps(event) for event in (
            {"class": "compile", "event": "completed", "started_at": 100, "ended_at": 200},
            {"class": "gui", "event": "started", "at": 900},
            {"class": "light", "event": "completed", "ended_at": 950}, "not json")) + "\n")
        self.assertEqual(warm.last_compile_at(log), 200)
        with open(log, "a") as handle:
            handle.write(json.dumps({"class": "compile", "event": "started", "at": 5000, "runner": "r",
                                     "run_id": "1", "job": "macos-compile-admission"}) + "\n")
        self.assertEqual(warm.compile_state(log, now=5100), (5000, "macos-compile-admission on r"))
        with open(log, "a") as handle:
            handle.write(json.dumps({"class": "compile", "event": "completed", "ended_at": 5300, "runner": "r",
                                     "run_id": "1", "job": "macos-compile-admission"}) + "\n")
        self.assertEqual(warm.compile_state(log, now=5400), (5300, ""))
        worker = "/Users/cmux/actions-runner-glaeda-2/bin.2.330.0/Runner.Worker spawnclient 1 2"
        self.assertIsNone(warm.BUSY.search(worker), "another job no longer stops a catch-up")
        self.assertIsNotNone(warm.BUSY.search("/Applications/Xcode.app/Contents/Developer/usr/bin/xcodebuild test"))

    def test_main_head_catches_the_mirror_up_to_github(self) -> None:
        mirror = self.state / ".prefetch" / "cmux.git"
        subprocess.run(["git", "init", "-q", "--bare", os.fspath(mirror)], check=True)
        heads = {"mirror": HEAD}
        calls = []
        saved = (warm.mirror_head, warm.remote_head, warm.subprocess.run)
        try:
            warm.mirror_head = lambda _mirror: heads["mirror"]
            warm.remote_head = lambda: "b" * 40
            def fetch(args, **_kwargs):
                calls.append(args)
                heads["mirror"] = "b" * 40
                return subprocess.CompletedProcess(args, 0)
            warm.subprocess.run = fetch
            self.assertEqual(warm.main_head(self.state), "b" * 40)
            self.assertIn("+refs/heads/main:refs/remotes/origin/main", calls[0])
            # Up to date, or GitHub unreachable: no fetch, the mirror's head.
            self.assertEqual(warm.main_head(self.state), "b" * 40)
            warm.remote_head = lambda: ""
            self.assertEqual(warm.main_head(self.state), "b" * 40)
            self.assertEqual(len(calls), 1)
        finally:
            warm.mirror_head, warm.remote_head, warm.subprocess.run = saved

    def test_pick_root_warms_the_farthest_root_once_per_head(self) -> None:
        predicted = {"root-1": {"seconds": 140.0, "tier": "near", "app_swift_files": 2},
                     "root-2": {"seconds": 266.5, "tier": "far", "app_swift_files": 9},
                     "root-3": {"seconds": 400.7, "tier": "rebuild", "app_swift_files": 3}}
        stub = types.SimpleNamespace(warm_root_costs=lambda order, base, number, state: (order, predicted))
        self.assertEqual(warm.pick_root(stub, 3, HEAD, self.state, {})[0], 3)
        # The root that keeps main is refreshed first, even when another root is farther.
        mains = types.SimpleNamespace(warm_root_costs=stub.warm_root_costs,
                                      root_stamp=lambda k, _state: {"merged_onto": HEAD, **({} if k == 2 else {"pr": 5})})
        self.assertEqual(warm.pick_root(mains, 3, HEAD, self.state, {})[0], 2)
        memory = {"roots": {"3": {"head": HEAD}}}
        self.assertEqual(warm.pick_root(stub, 3, HEAD, self.state, memory)[0], 2)
        memory["roots"]["2"] = {"head": HEAD}
        self.assertIn("near", warm.pick_root(stub, 3, HEAD, self.state, memory))
        self.assertEqual(warm.pick_root(stub, 3, "b" * 40, self.state, memory)[0], 3, "main moved")
        mixed = {"root-1": {"tier": "cold", "app_swift_files": -1}, "root-2": {"tier": "unknown", "app_swift_files": -1},
                 "root-3": {"tier": "far", "app_swift_files": 7}}
        ranked = types.SimpleNamespace(warm_root_costs=lambda order, base, number, state: (order, mixed))
        self.assertEqual(warm.pick_root(ranked, 3, HEAD, self.state, {"roots": []})[0], 3, "far before unknown, cold")
        blind = types.SimpleNamespace(warm_root_costs=lambda *args: ([], {}))
        root, guess = warm.pick_root(blind, 2, HEAD, self.state, {})
        self.assertEqual(root, 1, "the hook compared nothing (a stamp older than its fields): warm anyway")
        self.assertEqual(guess["root-2"]["tier"], "unknown")
        self.assertEqual(warm.pick_root(blind, 2, HEAD, self.state, {"roots": {"1": {"head": HEAD}}})[0], 2)
        self.assertIn("near", warm.pick_root(blind, 2, HEAD, self.state,
                                             {"roots": {"1": {"head": HEAD}, "2": {"head": HEAD}}}),
                      "once per root per head")


class AttemptTest(Base):
    def test_a_build_killed_without_a_job_counts_as_a_failed_try(self) -> None:
        memory = {"roots": {}, "attempt": {"root": 2, "head": HEAD, "at": 1000}}
        self.assertEqual(warm.settle_attempt(memory, last_job=900), "killed with no job")
        self.assertEqual(memory["roots"]["2"]["head"], HEAD, "not rebuilt for this head")
        self.assertEqual(memory["failures"], 1)
        self.assertNotIn("attempt", memory)
        memory = {"roots": {}, "attempt": {"root": 2, "head": HEAD, "at": 1000}}
        self.assertEqual(warm.settle_attempt(memory, last_job=1100), "preempted by a job")
        self.assertEqual(memory["roots"], {}, "a job took over: the next idle spell tries again")
        self.assertEqual(warm.settle_attempt({"roots": {}}), "")


class LedgerTest(Base):
    def test_take_holds_a_compile_worth_of_locks_and_names_them(self) -> None:
        scope = {"units": 4, "roots": 2, "compile_slots": 2, "xcode": ""}
        (self.dir / "fleet" / "host.lock").touch()
        taken = warm.take(hook, self.capacity, scope, 2)
        self.assertIsInstance(taken, tuple, taken)
        fds, held = taken
        self.assertEqual(held, ["root-2", "persistent-dd", "unit-0", "unit-1"])
        holder = json.loads((self.capacity / "idle-warm.json").read_text())
        self.assertEqual(holder, {"pid": os.getpid(), "held": held, "root": 2})
        # the flocks are real: another taker (a job's admission) finds them taken
        self.assertIsNone(hook.lock_file(self.capacity / "root-2.token", fcntl.LOCK_EX))
        self.assertEqual(warm.take(hook, self.capacity, scope, 2), "root-2 is taken")
        # the host lock is shared, so a fleet build (LOCK_EX) waits
        host = os.open(self.dir / "fleet" / "host.lock", os.O_RDONLY)
        with self.assertRaises(OSError):
            fcntl.flock(host, fcntl.LOCK_EX | fcntl.LOCK_NB)
        warm.release(fds)
        fcntl.flock(host, fcntl.LOCK_EX | fcntl.LOCK_NB)
        os.close(host)
        self.assertFalse((self.capacity / "idle-warm.json").exists())

    def test_take_backs_off_while_an_admission_or_a_fleet_build_holds_the_mini(self) -> None:
        scope = {"units": 4, "roots": 1, "compile_slots": 1, "xcode": ""}
        self.capacity.mkdir(parents=True)
        admission = os.open(self.capacity / "admission.lock", os.O_RDONLY | os.O_CREAT)
        fcntl.flock(admission, fcntl.LOCK_EX)
        self.assertEqual(warm.take(hook, self.capacity, scope, 1), "an admission is running")
        os.close(admission)
        host = os.open(self.dir / "fleet" / "host.lock", os.O_RDONLY | os.O_CREAT)
        fcntl.flock(host, fcntl.LOCK_EX)
        self.assertEqual(warm.take(hook, self.capacity, scope, 1), "a fleet build holds the host lock")
        os.close(host)
        dd = hook.lock_file(self.capacity / "persistent-dd.token", fcntl.LOCK_EX)
        self.assertEqual(warm.take(hook, self.capacity, scope, 1), "every persistent-dd token is taken")
        os.close(dd)
        self.assertFalse((self.capacity / "idle-warm.json").exists())
        # nothing was left held by the refusals
        self.assertIsNotNone(hook.lock_file(self.capacity / "root-1.token", fcntl.LOCK_EX))


class RunTest(Base):
    def fake_setup(self, sleep: str = "0") -> Path:
        (self.state / "seed-source.json").write_text("{}")
        (self.dir / "fleet" / "host.lock").touch()
        checkout = self.state / ".catch-up" / "cmux"
        (checkout / "scripts/ci").mkdir(parents=True)
        script = checkout / "scripts/ci/owned_catch_up.sh"
        script.write_text(FAKE_CATCH_UP)
        script.chmod(0o755)
        os.environ.update(FAKE_CALLS=os.fspath(self.dir / "calls"), FAKE_SLEEP=sleep, GITHUB_TOKEN="secret")
        scope = {"units": 4, "roots": 2, "compile_slots": 2, "xcode": "/Applications/Xcode_26.6.app"}
        predicted = {"root-1": {"seconds": 140.0, "tier": "near"}, "root-2": {"seconds": 400.7, "tier": "rebuild"}}
        stub = types.SimpleNamespace(**{name: getattr(hook, name) for name in ("lock_file", "CLASS_COST", "WARM_HOLDER")},
                                     warm_root_costs=lambda order, base, number, state: (order, predicted))
        warm.host_denied = lambda names=None: ""
        warm.runner_setup = lambda dirs: (stub, scope)
        warm.idle_refusal = lambda hook_module, now, state: ""
        warm.main_head = lambda state: HEAD
        warm.prepare_checkout = lambda work, head: None
        warm.running = lambda: ""
        return checkout

    @unittest.skipUnless(sys.platform == "darwin", "runs only on macOS")
    def test_apply_builds_the_far_root_once_and_releases(self) -> None:
        self.fake_setup()
        plan = warm.run(False, self.state)
        self.assertEqual((plan["state"], plan["root"]), ("plan", 2), plan)
        self.assertFalse((self.dir / "calls").exists(), "a plan builds nothing")
        done = warm.run(True, self.state)
        self.assertEqual(done["state"], "applied", done)
        self.assertEqual(done["result"]["kept"], "true")
        self.assertEqual(done["held"], ["root-2", "persistent-dd", "unit-0", "unit-1"])
        call = (self.dir / "calls").read_text()
        self.assertTrue(call.startswith(f"2 {self.state} xcode=/Applications/Xcode_26.6.app log="), call)
        self.assertIn("github=none", call, "no GitHub token reaches the build")
        self.assertFalse((self.capacity / "idle-warm.json").exists())
        self.assertIsNotNone(hook.lock_file(self.capacity / "root-2.token", fcntl.LOCK_EX))
        memory = json.loads((self.state / ".catch-up" / "state.json").read_text())
        self.assertEqual(memory["roots"]["2"]["head"], HEAD)
        again = warm.run(True, self.state)
        self.assertEqual(again["state"], "skip")
        self.assertIn("warmed for it", again["reason"])

    @unittest.skipUnless(sys.platform == "darwin", "runs only on macOS")
    def test_failures_pause_the_catch_up(self) -> None:
        checkout = self.fake_setup()
        (checkout / "scripts/ci/owned_catch_up.sh").write_text('#!/usr/bin/env bash\necho \'{"kept": "false"}\'\nexit 1\n')
        for n in range(warm.FAILURES_TO_PAUSE):
            memory_path = self.state / ".catch-up" / "state.json"
            if memory_path.exists():  # a new main head each time
                memory = json.loads(memory_path.read_text())
                memory["roots"] = {}
                memory_path.write_text(json.dumps(memory))
            self.assertEqual(warm.run(True, self.state)["result"]["kept"], "false")
        paused = warm.run(True, self.state)
        self.assertEqual(paused["state"], "skip")
        self.assertIn("paused", paused["reason"])

    def test_kill_switch_and_no_owned_state(self) -> None:
        if sys.platform != "darwin":
            self.assertEqual(warm.run(True, self.state)["reason"], "not macOS")
            return
        warm.host_denied = lambda names=None: ""
        self.assertIn("no owned compile", warm.run(True, self.state)["reason"])
        warm.KILL_SWITCH.write_text("")
        self.assertIn("exists", warm.run(True, self.state)["reason"])


@unittest.skipUnless(sys.platform == "darwin", "runs only on macOS")
class YieldTest(Base):
    def test_sigterm_kills_the_build_and_frees_the_locks_at_once(self) -> None:
        """What a job's admission does (yield_idle_warm) to a catch-up mid-build."""
        self.fake_setup_on_disk()
        driver = self.dir / "glaeda-idle-warm-driver.py"
        driver.write_text(textwrap.dedent(f"""
            import importlib.machinery, importlib.util, os, sys, types
            from pathlib import Path
            def load(name, path):
                loader = importlib.machinery.SourceFileLoader(name, path)
                spec = importlib.util.spec_from_loader(name, loader)
                module = importlib.util.module_from_spec(spec)
                loader.exec_module(module)
                return module
            warm = load("w", {os.fspath(ROOT / 'scripts' / 'glaeda-idle-warm')!r})
            hook = load("h", {os.fspath(ROOT / 'scripts' / 'glaeda-cmux-runner-hook')!r})
            state = Path({os.fspath(self.state)!r})
            warm.FLEET_DIR = state.parent
            warm.HOME = state.parent
            warm.host_denied = lambda names=None: ""
            warm.runner_setup = lambda dirs: (hook, {{"units": 4, "roots": 1, "compile_slots": 1, "xcode": "/x.app"}})
            warm.idle_refusal = lambda *a: ""
            warm.main_head = lambda s: "{HEAD}"
            warm.pick_root = lambda *a: (1, {{}})
            warm.prepare_checkout = lambda work, head: None
            warm.running = lambda: ""
            warm.forget_inode_override = lambda: open({os.fspath(self.dir / 'forgot')!r}, "w").close()
            print(warm.run(True, state), flush=True)
        """))
        proc = subprocess.Popen([sys.executable, os.fspath(driver)], stdout=subprocess.PIPE, text=True,
                                env={**os.environ, "FAKE_SLEEP": "120", "FAKE_CALLS": os.fspath(self.dir / "calls")})
        self.addCleanup(proc.kill)
        deadline = time.monotonic() + 30
        while not (self.capacity / "idle-warm.json").exists() and time.monotonic() < deadline:
            time.sleep(0.1)
        while not (self.dir / "calls").exists() and time.monotonic() < deadline:
            time.sleep(0.1)
        self.assertTrue((self.dir / "calls").exists(), "the build started")
        self.assertIsNone(hook.lock_file(self.capacity / "root-1.token", fcntl.LOCK_EX))
        started = time.monotonic()
        proc.send_signal(signal.SIGTERM)
        # it ends its whole process group, itself included, with one SIGKILL
        self.assertEqual(proc.wait(timeout=10), -signal.SIGKILL)
        self.assertLess(time.monotonic() - started, 5)
        self.assertIsNotNone(hook.lock_file(self.capacity / "root-1.token", fcntl.LOCK_EX))
        self.assertIsNone(hook.idle_warm(self.capacity), "the file stays, naming a dead pid the hook ignores")
        self.assertTrue((self.dir / "forgot").exists(), "the Xcode inode override a kill would leave is deleted")
        leftover = subprocess.run(["/usr/bin/pgrep", "-f", "sleep 120"], capture_output=True, text=True).stdout
        self.assertEqual(leftover.strip(), "", "the build's process group was killed")

    def fake_setup_on_disk(self) -> None:
        (self.state / "seed-source.json").write_text("{}")
        (self.dir / "fleet" / "host.lock").touch()
        checkout = self.state / ".catch-up" / "cmux"
        (checkout / "scripts/ci").mkdir(parents=True)
        script = checkout / "scripts/ci/owned_catch_up.sh"
        script.write_text(FAKE_CATCH_UP)
        script.chmod(0o755)


# Stands in for cmux's scripts/fuzz: records its arguments, starts a child (the app) in its process group, sleeps,
# then writes the run summary the way `scripts/fuzz run` does.
FAKE_FUZZ = """#!/usr/bin/env python3
import json, os, subprocess, sys, time
args = sys.argv[1:]
with open(os.environ["FAKE_CALLS"], "a") as calls:
    calls.write(" ".join(args) + " nice=%d\\n" % os.nice(0))
out = args[args.index("--out") + 1]
app = subprocess.Popen(["/bin/sleep", "300"])
open(os.path.join(out, "app.pid"), "w").write(str(app.pid))
time.sleep(float(os.environ.get("FAKE_SLEEP", "0")))
app.kill()
json.dump({"seed": 1, "sessions": 1, "steps": 42, "findings": []}, open(os.path.join(out, "summary.json"), "w"))
"""

# Loads the lane in a child process (so a SIGTERM or SIGKILL of its process group does not reach the test) with
# the same stubs FuzzTest.fuzz_setup puts in, and runs it.
FUZZ_DRIVER = """
import importlib.machinery, importlib.util, json, os, sys
from pathlib import Path
def load(name, path):
    loader = importlib.machinery.SourceFileLoader(name, path)
    spec = importlib.util.spec_from_loader(name, loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module
warm = load("w", sys.argv[1])
hook = load("h", sys.argv[2])
state = Path(sys.argv[3])
warm.FLEET_DIR = state.parent
warm.HOME = Path(sys.argv[4])
warm.FUZZ_KILL_SWITCH = warm.HOME / "idle-fuzz.disabled"
hook.root_stamp = lambda k, s: {"merged_onto": sys.argv[5]} if k == 2 else None
hook.console_state = lambda: None
hook.reservation_refusal = lambda path, now: None
warm.host_denied = lambda names=None: ""
warm.fuzz_setup = lambda: (hook, 2)
warm.console_refusal = lambda h: ""
warm.gui_process = lambda ours="": ""  # other agents' Xcode tests on this Mac are not this test's
warm.free_bytes = lambda p: 500 * 1024 ** 3
print(json.dumps(warm.fuzz(True, state)), flush=True)
"""


class FuzzTest(Base):
    def fuzz_setup(self, sleep: str = "0") -> types.SimpleNamespace:
        """A mini whose root 2 keeps a main build of HEAD, with main's checkout (a git repo) for the fuzzer."""
        (self.dir / "fleet" / "host.lock").touch()
        engine = self.state / ".catch-up" / "cmux"
        (engine / "scripts").mkdir(parents=True)
        (engine / "dogfood" / "fuzz").mkdir(parents=True)
        (engine / "scripts" / "fuzz").write_text(FAKE_FUZZ)
        (engine / "dogfood" / "fuzz" / "README.md").write_text("engine\n")
        git = ["git", "-C", os.fspath(engine), "-c", "user.email=t@t", "-c", "user.name=t"]
        subprocess.run([*git, "init", "-q"], check=True)
        subprocess.run([*git, "add", "-A"], check=True)
        subprocess.run([*git, "commit", "-qm", "main"], check=True)
        self.head = subprocess.run([*git, "rev-parse", "HEAD"], check=True, capture_output=True,
                                   text=True).stdout.strip()
        store = self.state / "cmux-ci-2"
        app = store / "derived-data" / "Build" / "Products" / "Debug" / "cmux DEV.app" / "Contents"
        app.mkdir(parents=True)
        (app / "Info.plist").write_text("main build")
        (store / "stamp.json").write_text(json.dumps({"merged_onto": self.head}))
        os.environ.update(FAKE_CALLS=os.fspath(self.dir / "calls"), FAKE_SLEEP=sleep)
        stamps = {1: {"merged_onto": "b" * 40, "pr": 15000}, 2: {"merged_onto": self.head}}
        stub = types.SimpleNamespace(probe=hook.probe, FUZZ_HOLDER=hook.FUZZ_HOLDER,
                                     root_stamp=lambda k, state: stamps.get(k),
                                     reservation_refusal=lambda path, now: None)
        self.saved.update({name: getattr(warm, name) for name in ("fuzz_setup", "gui_process", "FUZZ_KILL_SWITCH")})
        warm.FUZZ_KILL_SWITCH = self.dir / "idle-fuzz.disabled"
        warm.host_denied = lambda names=None: ""
        warm.fuzz_setup = lambda: (stub, 2)
        warm.console_refusal = lambda hook_module: ""
        warm.gui_process = lambda ours="": ""
        warm.free_bytes = lambda path: 500 * 1024 ** 3
        return stub

    def driver(self) -> subprocess.Popen:
        # Named glaeda-idle-warm, as the LaunchAgent runs it: the hook signals only a holder that is that program.
        program = self.dir / "bin" / "glaeda-idle-warm"
        program.parent.mkdir(exist_ok=True)
        program.write_text(FUZZ_DRIVER)
        return subprocess.Popen(
            [sys.executable, os.fspath(program), os.fspath(ROOT / "scripts" / "glaeda-idle-warm"),
             os.fspath(ROOT / "scripts" / "glaeda-cmux-runner-hook"), os.fspath(self.state), os.fspath(self.dir),
             self.head, "--fuzz"], stdout=subprocess.PIPE, text=True, env={**os.environ})

    def started_app(self, proc: subprocess.Popen) -> int:
        runs = self.dir / "fleet" / "fuzz" / "runs"
        deadline = time.monotonic() + 30
        pid_files: list[Path] = []
        while not pid_files and time.monotonic() < deadline:
            pid_files = list(runs.glob("*/app.pid")) if runs.is_dir() else []
            time.sleep(0.1)
        if not pid_files:
            proc.kill()
            self.fail(f"the fuzzer started no app: {proc.communicate(timeout=10)[0]}")
        return int(pid_files[0].read_text())

    @unittest.skipUnless(sys.platform == "darwin", "runs only on macOS")
    def test_fuzzes_a_staged_copy_of_the_main_build_beside_a_compile_at_low_priority(self) -> None:
        self.fuzz_setup()
        # A compile admitted on this mini: every unit and a root taken. The fuzzer needs none of them.
        self.capacity.mkdir(parents=True)
        held = [hook.lock_file(self.capacity / name, fcntl.LOCK_EX) for name in
                ("unit-0", "unit-1", "unit-2", "unit-3", "root-1.token", "persistent-dd.token")]
        self.addCleanup(lambda: [os.close(fd) for fd in held if fd is not None])
        plan = warm.fuzz(False, self.state)
        self.assertEqual(plan["state"], "plan", plan)
        self.assertFalse((self.dir / "calls").exists(), "a plan fuzzes nothing")
        proc = self.driver()
        out, _ = proc.communicate(timeout=60)
        result = json.loads(out.strip().splitlines()[-1])
        self.assertEqual(result["state"], "fuzzed", result)
        self.assertEqual((result["build"], result["steps"], result["findings"]), (self.head, 42, 0))
        staged = self.dir / "fleet" / "fuzz" / "builds" / self.head / "cmux DEV.app" / "Contents" / "Info.plist"
        self.assertEqual(staged.read_text(), "main build")
        engine = self.dir / "fleet" / "fuzz" / "engines" / self.head
        self.assertEqual((engine / "dogfood" / "fuzz" / "README.md").read_text(), "engine\n", "exported from git")
        call = (self.dir / "calls").read_text()
        self.assertIn(f"run --app {staged.parents[1]} --minutes {warm.FUZZ_MINUTES}", call)
        self.assertIn(f"--label main --sha {self.head}", call)
        self.assertGreaterEqual(int(call.rsplit("nice=", 1)[1]), warm.FUZZ_NICE)
        self.assertFalse((self.capacity / "idle-warm.json").exists(), "never a catch-up holder")
        # The same staged build and engine next time, even with the checkout gone.
        (self.dir / "calls").unlink()
        subprocess.run(["rm", "-rf", os.fspath(self.state / ".catch-up")], check=True)
        again = json.loads(self.driver().communicate(timeout=60)[0].strip().splitlines()[-1])
        self.assertEqual((again["state"], again["build"]), ("fuzzed", self.head), again)

    @unittest.skipUnless(sys.platform == "darwin", "runs only on macOS")
    def test_never_beside_a_gui_job_a_reservation_or_without_a_main_build(self) -> None:
        stub = self.fuzz_setup()
        self.capacity.mkdir(parents=True)
        gui = hook.lock_file(self.capacity / "gui.token", fcntl.LOCK_EX)
        self.assertEqual(warm.fuzz(True, self.state)["reason"], "a job holds the gui token")
        os.close(gui)
        stub.reservation_refusal = lambda path, now: "host reserved by leo for PR 1 dogfood until later"
        self.assertIn("dogfood", warm.fuzz(True, self.state)["reason"])
        stub.reservation_refusal = lambda path, now: None
        warm.gui_process = lambda ours="": "a job is asking for the gui token"
        self.assertEqual(warm.fuzz(True, self.state)["reason"], "a job is asking for the gui token")
        warm.gui_process = lambda ours="": ""
        (self.dir / "idle-fuzz.disabled").touch()
        self.assertIn("idle-fuzz.disabled", warm.fuzz(True, self.state)["reason"])
        (self.dir / "idle-fuzz.disabled").unlink()
        stub.root_stamp = lambda k, state: {"merged_onto": self.head, "pr": 15000}  # pull request builds only
        self.assertIn("no main build", warm.fuzz(True, self.state)["reason"])
        # a copy a kill cut short is never taken for a build
        partial = self.dir / "fleet" / "fuzz" / "builds" / f".{self.head}.123" / "cmux DEV.app"
        partial.mkdir(parents=True)
        self.assertEqual(warm.staged_builds(), [])
        self.assertIn("no main build", warm.fuzz(True, self.state)["reason"])
        warm.free_bytes = lambda path: 10 * 1024 ** 3
        stub.root_stamp = lambda k, state: {"merged_onto": self.head}
        self.assertIn("GiB free", warm.fuzz(True, self.state)["reason"])
        self.assertFalse((self.dir / "calls").exists())
        warm.free_bytes = lambda path: 500 * 1024 ** 3
        warm.fuzz_setup = lambda: "no glaeda runner on this mini"
        self.assertEqual(warm.fuzz(True, self.state)["reason"], "no glaeda runner on this mini")

    def test_gui_process_sees_tests_take_gui_and_other_dev_apps(self) -> None:
        ours = "/Users/Shared/cmux-build-fleet/fuzz/builds/"
        cases = {
            "/Applications/Xcode.app/Contents/Developer/usr/bin/xctest -XCTest All": "an Xcode test is running",
            "/usr/bin/python3 /Users/cmux/r/glaeda-hooks/glaeda-cmux-runner-hook take-gui --wait 60":
                "a job is asking for the gui token",
            "/bin/sh /Users/cmux/.local/bin/glaeda-canonical-root take-gui": "a job is asking for the gui token",
            "/tmp/dogfood/cmux DEV pr-1.app/Contents/MacOS/cmux DEV": "another cmux DEV app is running",
            f"{ours}abc/cmux DEV.app/Contents/MacOS/cmux DEV": "",
            "/usr/bin/python3 glaeda-cmux-runner-hook take-root --root 1": "",
        }
        real = subprocess.run

        def fake(comm: str, command: str):
            return lambda argv, **k: types.SimpleNamespace(returncode=0, stdout=(comm if argv[-1] == "comm=" else command) + "\n")
        try:
            for command, want in cases.items():
                subprocess.run = fake(command.split(" -")[0].split(" --")[0], command)
                self.assertEqual(warm.gui_process(ours), want, command)
            # A compile names the app and a test runner in its arguments; only running ones count.
            for command in ("/usr/bin/codesign --force --sign - /dd/Build/Products/Debug/cmux DEV.app/Contents/MacOS/cmux DEV",
                            "/usr/bin/codesign --force /dd/Build/Products/Debug/cmuxUITests-Runner.app/"):
                subprocess.run = fake("/usr/bin/codesign", command)
                self.assertEqual(warm.gui_process(ours), "", command)
        finally:
            subprocess.run = real

    @unittest.skipUnless(sys.platform == "darwin", "runs only on macOS")
    def test_taking_the_gui_token_stops_the_fuzzer_and_its_app_at_once(self) -> None:
        """What a gui job's admission or take-gui does (yield_idle_fuzz): the app goes with the process group."""
        self.fuzz_setup(sleep="120")
        proc = self.driver()
        self.addCleanup(proc.kill)
        app_pid = self.started_app(proc)
        holder = json.loads((self.capacity / hook.FUZZ_HOLDER).read_text())
        self.assertEqual(holder["pid"], proc.pid)
        self.assertEqual(hook.yield_idle_warm(self.capacity), "", "a compile's admission leaves the fuzzer alone")
        self.assertTrue(hook.pid_alive(app_pid))
        # launchd reaps the lane on a mini; here the test is its parent, so reap it as it dies (a zombie is alive
        # to kill(pid, 0), and the hook would wait on it).
        reaper = threading.Thread(target=proc.wait, daemon=True)
        reaper.start()
        started = time.monotonic()
        self.assertIn("stopped the idle fuzzer", hook.yield_idle_fuzz(self.capacity))
        self.assertLess(time.monotonic() - started, 5)
        reaper.join(timeout=10)
        self.assertEqual(proc.returncode, -signal.SIGKILL)
        self.assertFalse(hook.pid_alive(app_pid), "the app went with the fuzzer's process group")

    @unittest.skipUnless(sys.platform == "darwin", "runs only on macOS")
    def test_an_older_hook_still_loses_the_fuzzer_when_a_job_takes_the_gui_token(self) -> None:
        """A runner whose hook predates FUZZ_HOLDER never signals the fuzzer: the lane sees the token and stops."""
        self.fuzz_setup(sleep="120")
        proc = self.driver()
        self.addCleanup(proc.kill)
        app_pid = self.started_app(proc)
        gui = hook.lock_file(self.capacity / "gui.token", fcntl.LOCK_EX)
        self.addCleanup(os.close, gui)
        out, _ = proc.communicate(timeout=30)
        result = json.loads(out.strip().splitlines()[-1])
        self.assertEqual((result["state"], result["reason"]), ("stopped", "a job holds the gui token"), result)
        self.assertLess(result["wall_seconds"], 30)
        self.assertFalse(hook.pid_alive(app_pid), "the app is gone")


class TrimTest(Base):
    def test_runs_a_preemption_cut_short_are_trimmed_once_old(self) -> None:
        runs = self.dir / "runs"
        old = time.time() - warm.FUZZ_TIMEOUT_S - 60
        for n in range(warm.FUZZ_KEEP_QUIET_RUNS + 3):
            run = runs / f"cut-{n:02d}"
            (run / "session-000").mkdir(parents=True)  # no summary.json: SIGKILLed mid-run
            os.utime(run, (old - n, old - n))
        live = runs / "live"
        live.mkdir()  # no summary yet and young: still running
        found = runs / "found" / "session-000"
        found.mkdir(parents=True)
        (found / "finding.json").write_text("{}")
        os.utime(runs / "found", (old, old))
        warm.trim_fuzz_runs(runs)
        left = sorted(p.name for p in runs.iterdir())
        self.assertIn("live", left)
        self.assertIn("found", left, "a cut-short run with a finding waits for the collector")
        self.assertEqual(len([n for n in left if n.startswith("cut-")]), warm.FUZZ_KEEP_QUIET_RUNS)
        self.assertIn("cut-00", left, "the newest are kept")


class CheckoutTest(unittest.TestCase):
    def test_first_clone_is_shallow_blobless_and_checks_out_head(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            source, work = Path(tmp) / "source", Path(tmp) / "work"
            run = lambda *args, cwd=source: subprocess.run(["git", *args], cwd=cwd, check=True, capture_output=True)
            source.mkdir()
            run("init", "-q", "-b", "main")
            run("config", "uploadpack.allowFilter", "true")
            for text in ("zero\n", "one\n"):  # two commits, so a depth-1 clone is shallow
                (source / "a.txt").write_text(text)
                run("add", "a.txt")
                run("-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", text.strip())
            head = subprocess.run(["git", "rev-parse", "HEAD"], cwd=source, check=True, capture_output=True,
                                  text=True).stdout.strip()
            work.mkdir()
            saved, saved_depth = warm.REPO_URL, warm.CLONE_DEPTH
            self.addCleanup(setattr, warm, "CLONE_DEPTH", saved_depth)
            warm.CLONE_DEPTH = 1
            warm.REPO_URL = source.as_uri()
            try:
                warm.prepare_checkout(work, head)
            finally:
                warm.REPO_URL = saved
            checkout = work / "cmux"
            self.assertEqual((checkout / "a.txt").read_text(), "one\n")
            partial = subprocess.run(["git", "config", "remote.origin.partialclonefilter"], cwd=checkout,
                                     capture_output=True, text=True).stdout.strip()
            self.assertEqual(partial, "blob:none")
            self.assertTrue((checkout / ".git" / "shallow").is_file())
            warm.REPO_URL = source.as_uri()
            try:
                warm.prepare_checkout(work, head)  # the next run fetches into the kept clone
            finally:
                warm.REPO_URL = saved
            self.assertEqual((checkout / "a.txt").read_text(), "one\n")


class NoEmDashTest(unittest.TestCase):
    def test_no_em_dashes(self) -> None:
        self.assertNotIn("—", (ROOT / "scripts" / "glaeda-idle-warm").read_text())


if __name__ == "__main__":
    unittest.main()
