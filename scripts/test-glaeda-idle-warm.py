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
                       "prepare_checkout", "running", "host_denied")}
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

    def test_idle_after_three_quiet_minutes(self) -> None:
        self.assertEqual(warm.IDLE_S, 180)
        saved = warm.last_job_at
        stub = types.SimpleNamespace(thermal_pressure_level=lambda: 1)
        try:
            warm.running = lambda: ""
            warm.last_job_at = lambda: 1000.0
            self.assertIn("a job ran 179 s ago", warm.idle_refusal(stub, 1179.0, self.state))
            # Past IDLE_S a later gate (load, heat, disk) answers, never the job gate.
            self.assertNotIn("a job ran", warm.idle_refusal(stub, 1181.0, self.state))
        finally:
            warm.last_job_at = saved

    def test_pick_root_warms_the_farthest_root_once_per_head(self) -> None:
        predicted = {"root-1": {"seconds": 140.0, "tier": "near", "app_swift_files": 2},
                     "root-2": {"seconds": 266.5, "tier": "far", "app_swift_files": 9},
                     "root-3": {"seconds": 400.7, "tier": "rebuild", "app_swift_files": 3}}
        stub = types.SimpleNamespace(warm_root_costs=lambda order, base, number, state: (order, predicted))
        self.assertEqual(warm.pick_root(stub, 3, HEAD, self.state, {})[0], 3)
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


class NoEmDashTest(unittest.TestCase):
    def test_no_em_dashes(self) -> None:
        self.assertNotIn("—", (ROOT / "scripts" / "glaeda-idle-warm").read_text())


if __name__ == "__main__":
    unittest.main()
