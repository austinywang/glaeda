#!/usr/bin/env python3
"""Contract tests for scripts/glaeda-cmux-runner and scripts/glaeda-cmux-runner-hook.

Runs on Linux CI. gh, curl, launchctl and the runner's config.sh are fakes that log every argv
and environment flag to a JSON-lines file, so the tests can prove the registration token never
reaches an argv, a file, or the output.
"""

from __future__ import annotations

import contextlib
import fcntl
import hashlib
import importlib.machinery
import importlib.util
import io
import json
import os
import plistlib
import shlex
import signal
import shutil
import subprocess
import sys
import tarfile
import tempfile
import threading
import time
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
HOOK = ROOT / "scripts" / "glaeda-cmux-runner-hook"
WORKER_STEP_EXIT = "runner-worker-step-exit "  # the Runner.Worker stand-in's last stdout line (HookTest.take)


def load(name: str, path: Path):
    loader = importlib.machinery.SourceFileLoader(name, os.fspath(path))
    spec = importlib.util.spec_from_loader(name, loader)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    loader.exec_module(module)
    return module


cr = load("glaeda_cmux_runner", ROOT / "scripts" / "glaeda-cmux-runner")
cr.DARWIN_REQUIRED = False
cr.ONLINE_WAIT_S = 0
# The load gate reads this machine's real load, which on a busy test host would hold every gate under test. Pin
# it out of reach here and in the hooks the tests start; the load test patches the thresholds itself.
os.environ["GLAEDA_RUNNER_GATE_LOAD_PAUSE"] = os.environ["GLAEDA_RUNNER_GATE_LOAD_RESUME"] = "1000000"
# Likewise the console check reads this Mac's real session, which a locked test host would turn into refusals: no
# ioreg here or in the hooks the tests start (unreadable changes nothing); console tests point it at a fake.
os.environ["GLAEDA_RUNNER_IOREG"] = "/nonexistent/ioreg"
hook = load("glaeda_cmux_runner_hook", HOOK)
REAL_LEVEL = hook.thermal_pressure_level  # GateTest patches the module attribute
setup = load("glaeda_mini_setup_for_runner", ROOT / "scripts" / "glaeda-mini-setup")
setup.DARWIN_REQUIRED = False

REG_TOKEN = "AREGTOKENSECRET0123456789"
REMOVE_TOKEN = "AREMOVETOKENSECRET9876543"
VERSION = "2.399.0"

FAKE_LOG_HEADER = r'''#!/usr/bin/env python3
import json, os, sys
state = os.environ["FAKE_STATE"]
def log(tool, argv):
    with open(os.path.join(state, "log.jsonl"), "a") as f:
        f.write(json.dumps({"tool": tool, "argv": argv,
                            "envToken": os.environ.get("ACTIONS_RUNNER_INPUT_TOKEN")}) + "\n")
def runners():
    try:
        return json.load(open(os.path.join(state, "runners.json")))
    except OSError:
        return {"runners": []}
def save(doc):
    json.dump(doc, open(os.path.join(state, "runners.json"), "w"))
'''

FAKE_GH = FAKE_LOG_HEADER + r'''
argv = sys.argv[1:]
log("gh", argv)
if argv[:2] == ["auth", "status"]:
    sys.exit(0)
if argv[0] != "api":
    sys.exit(2)
if "-X" in argv:
    i = argv.index("-X"); method, endpoint = argv[i + 1], argv[i + 2]
else:
    method, endpoint = "GET", argv[1]
if endpoint.startswith("repos/actions/runner/releases"):
    sys.stdout.write(open(os.path.join(state, "release.json")).read())
elif endpoint.endswith("/registration-token") and method == "POST":
    print(os.environ["FAKE_REG_TOKEN"])
elif endpoint.endswith("/remove-token") and method == "POST":
    if os.path.exists(os.path.join(state, "fail-remove-token")):
        sys.exit(1)
    print(os.environ["FAKE_REMOVE_TOKEN"])
elif "/actions/runners" in endpoint and method == "GET":
    print(json.dumps(runners()))
elif "/actions/runners/" in endpoint and method == "DELETE":
    if os.path.exists(os.path.join(state, "fail-delete")):
        sys.exit(1)
    rid = int(endpoint.rsplit("/", 1)[1])
    save({"runners": [r for r in runners()["runners"] if r["id"] != rid]})
else:
    sys.exit(3)
'''

FAKE_CURL = FAKE_LOG_HEADER + r'''
argv = sys.argv[1:]
log("curl", argv)
if "-o" not in argv:  # the gh-free path reads the public release API
    sys.stdout.write(open(os.path.join(state, "release.json")).read()); sys.exit(0)
dest = argv[argv.index("-o") + 1]
open(dest, "wb").write(open(os.path.join(state, "runner.tar.gz"), "rb").read())
'''

FAKE_LAUNCHCTL = FAKE_LOG_HEADER + r'''
argv = sys.argv[1:]
log("launchctl", argv)
# launchd's disabled overrides live in disabled.json (labels); bootstrap refuses a disabled one, as launchd does.
path = os.path.join(state, "disabled.json")
disabled = json.load(open(path)) if os.path.exists(path) else []
if argv[0] == "print-disabled":
    extra = open(os.path.join(state, "print-disabled-extra")).read() if os.path.exists(os.path.join(state, "print-disabled-extra")) else ""
    print("disabled services = {\n" + extra + "".join(f'\t"{l}" => disabled\n' for l in disabled) + "}"); sys.exit(0)
if argv[0] in ("enable", "disable"):
    label = argv[1].rsplit("/", 1)[1]
    disabled = [l for l in disabled if l != label] + ([label] if argv[0] == "disable" else [])
    json.dump(disabled, open(path, "w")); sys.exit(0)
if argv[0] == "bootstrap" and os.path.basename(argv[2])[:-len(".plist")] in disabled:
    print("Bootstrap failed: 5: Input/output error"); sys.exit(5)
# With loaded.json present, the fake tracks which labels are loaded; otherwise print always says not loaded.
lpath = os.path.join(state, "loaded.json")
if os.path.exists(lpath):
    loaded = json.load(open(lpath))
    if argv[0] == "print":
        sys.exit(0 if argv[1].rsplit("/", 1)[1] in loaded else 1)
    if argv[0] == "bootstrap":
        loaded.append(os.path.basename(argv[2])[:-len(".plist")])
    if argv[0] == "bootout":
        loaded = [l for l in loaded if l != argv[1].rsplit("/", 1)[1]]
    json.dump(loaded, open(lpath, "w"))
sys.exit(1 if argv[0] == "print" else 0)
'''

FAKE_CONFIG = FAKE_LOG_HEADER + r'''
argv = sys.argv[1:]
log("config.sh", argv)
here = os.path.dirname(os.path.abspath(__file__))
token = os.environ.get("ACTIONS_RUNNER_INPUT_TOKEN")
if argv[:1] == ["remove"]:
    if token != os.environ["FAKE_REMOVE_TOKEN"] or os.path.exists(os.path.join(state, "fail-config-remove")):
        print("remove failed"); sys.exit(1)
    name = json.load(open(os.path.join(here, ".runner")))["agentName"]
    save({"runners": [r for r in runners()["runners"] if r["name"] != name]})
    os.remove(os.path.join(here, ".runner"))
    sys.exit(0)
if token != os.environ["FAKE_REG_TOKEN"]:
    print("bad token " + str(token)); sys.exit(1)
name = argv[argv.index("--name") + 1]
labels = ["self-hosted", "macOS", "ARM64"] + argv[argv.index("--labels") + 1].split(",")
doc = runners()
if "--replace" in argv:
    doc["runners"] = [r for r in doc["runners"] if r["name"] != name]
elif any(r["name"] == name for r in doc["runners"]):
    print("A runner exists with the same name"); sys.exit(1)
count_path = os.path.join(state, "registrations")
count = int(open(count_path).read()) if os.path.exists(count_path) else 0
open(count_path, "w").write(str(count + 1))
rid = 4242 + count
json.dump({"agentId": rid, "agentName": name, "gitHubUrl": argv[argv.index("--url") + 1]},
          open(os.path.join(here, ".runner"), "w"))
for extra in (".credentials", ".credentials_rsaparams"):
    open(os.path.join(here, extra), "w").write("{}")
doc["runners"].append({"id": rid, "name": name, "status": "online", "busy": False,
                       "labels": [{"name": l} for l in labels]})
save(doc)
print("registered with token " + token)  # a leaky runner must still not leak it through us
'''


def make_executable(path: Path, text: str) -> Path:
    path.write_text(text)
    path.chmod(0o755)
    return path


def event(tmp: Path, name: str, payload: dict) -> Path:
    path = tmp / f"{name}.json"
    path.write_text(json.dumps(payload))
    return path


CMUX = {"full_name": "manaflow-ai/cmux", "fork": False}
FORK = {"full_name": "someone/cmux", "fork": True}
SAMPLE_EVENTS = {
    # name: (GITHUB_EVENT_NAME, payload, admitted)
    "same-repo-pr": ("pull_request", {"repository": CMUX, "pull_request": {
        "head": {"repo": CMUX}, "base": {"repo": CMUX}}}, True),
    "push": ("push", {"repository": CMUX, "ref": "refs/heads/main"}, True),
    "workflow-dispatch": ("workflow_dispatch", {"repository": CMUX, "inputs": {}}, True),
    "merge-group": ("merge_group", {"repository": CMUX, "merge_group": {"head_ref": "gh-readonly-queue/main/x"}}, True),
    "fork-pr": ("pull_request", {"repository": CMUX, "pull_request": {
        "head": {"repo": FORK}, "base": {"repo": CMUX}}}, False),
    "cross-repo-pr-not-flagged-fork": ("pull_request", {"repository": CMUX, "pull_request": {
        "head": {"repo": {"full_name": "other/cmux", "fork": False}}, "base": {"repo": CMUX}}}, False),
    "deleted-fork-pr": ("pull_request", {"repository": CMUX, "pull_request": {
        "head": {"repo": None}, "base": {"repo": CMUX}}}, False),
    "pull-request-target-same-repo": ("pull_request_target", {"repository": CMUX, "pull_request": {
        "head": {"repo": CMUX}, "base": {"repo": CMUX}}}, False),
    "fork-pr-review": ("pull_request_review", {"repository": CMUX, "pull_request": {
        "head": {"repo": FORK}, "base": {"repo": CMUX}}}, False),
    "pr-event-without-payload": ("pull_request", {"repository": CMUX}, False),
    "workflow-run-from-fork": ("workflow_run", {"repository": CMUX, "workflow_run": {
        "head_repository": FORK, "event": "pull_request"}}, False),
    "workflow-run-same-repo": ("workflow_run", {"repository": CMUX, "workflow_run": {
        "head_repository": CMUX, "event": "push"}}, True),
    "other-repository": ("push", {"repository": {"full_name": "evil/cmux"}}, False),
    "issue-comment-on-pr": ("issue_comment", {"repository": CMUX, "issue": {"pull_request": {"url": "x"}}}, False),
    "check-run": ("check_run", {"repository": CMUX, "check_run": {}}, False),
    "schedule": ("schedule", {"repository": CMUX}, True),
}


class HookTest(unittest.TestCase):
    """Runs the hook script itself, as the runner would, against sample event files."""

    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self.tmp.name)

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def run_hook(self, phase: str, event_name: str | None, event_path: Path | None,
                 *extra: str, repo: str | None = None, env: dict | None = None) -> subprocess.CompletedProcess:
        environ = {"PATH": "/usr/bin:/bin", "HOME": os.fspath(self.dir), "GLAEDA_RUNNER_TELEMETRY": "0",
                   "GLAEDA_FLEET_DIR": os.fspath(self.dir / "fleet")}
        if event_name is not None:
            environ["GITHUB_EVENT_NAME"] = event_name
        if event_path is not None:
            environ["GITHUB_EVENT_PATH"] = os.fspath(event_path)
        if repo:
            environ["GITHUB_REPOSITORY"] = repo
        environ.update(env or {})
        return subprocess.run([sys.executable, os.fspath(HOOK), phase, *extra], env=environ,
                              capture_output=True, text=True, timeout=30, check=False)

    def test_sample_events(self) -> None:
        for name, (event_name, payload, admitted) in SAMPLE_EVENTS.items():
            with self.subTest(name):
                path = event(self.dir, name, payload)
                repo = (payload.get("repository") or {}).get("full_name")
                result = self.run_hook("job-started", event_name, path, "--allowed-repo", "manaflow-ai/cmux",
                                       "--no-disk", repo=repo)
                self.assertEqual(result.returncode == 0, admitted, result.stdout + result.stderr)
                self.assertIn("admitted" if admitted else "refused", result.stdout)

    def test_fails_closed_without_event(self) -> None:
        self.assertEqual(self.run_hook("job-started", None, None, "--no-disk").returncode, 1)
        self.assertEqual(self.run_hook("job-started", "push", self.dir / "missing.json", "--no-disk").returncode, 1)
        bad = self.dir / "bad.json"
        bad.write_text("{not json")
        self.assertEqual(self.run_hook("job-started", "push", bad, "--no-disk").returncode, 1)

    def test_org_scope_uses_owner(self) -> None:
        path = event(self.dir, "push", {"repository": CMUX})
        self.assertEqual(self.run_hook("job-started", "push", path, "--allowed-owner", "manaflow-ai",
                                       "--no-disk", repo="manaflow-ai/cmux").returncode, 0)
        self.assertEqual(self.run_hook("job-started", "push", path, "--allowed-owner", "manaflow-ai",
                                       "--no-disk", repo="other/cmux").returncode, 1)

    def test_trusted_ref_admits_only_that_branchs_own_jobs(self) -> None:
        repo = {**CMUX, "default_branch": "main"}
        other = {"full_name": "manaflow-ai/scratch", "fork": False, "default_branch": "main"}
        main = {"ref": "refs/heads/main", "repository": repo}
        pr = {"number": 1, "head": {"repo": {**CMUX, "fork": False}}, "base": {"repo": CMUX}}
        cases = {
            "push to main": ("push", "refs/heads/main", main, "manaflow-ai/cmux", True),
            "schedule": ("schedule", "refs/heads/main", {"repository": repo}, "manaflow-ai/cmux", True),
            "dispatch on main (inputs can name any ref)": (
                "workflow_dispatch", "refs/heads/main", {"ref": "refs/heads/main", "repository": repo,
                                                         "inputs": {"ref": "refs/pull/1/merge"}}, "manaflow-ai/cmux", False),
            "push to a branch": ("push", "refs/heads/pr-branch", {**main, "ref": "refs/heads/pr-branch"},
                                 "manaflow-ai/cmux", False),
            "push, ref env and payload differ": ("push", "refs/heads/main", {**main, "ref": "refs/heads/x"},
                                                 "manaflow-ai/cmux", False),
            "another org repo's main": ("push", "refs/heads/main", {**main, "repository": other},
                                        "manaflow-ai/scratch", False),
            "repo env and payload differ": ("push", "refs/heads/main", {**main, "repository": other},
                                            "manaflow-ai/cmux", False),
            "schedule on another default branch": ("schedule", "refs/heads/main",
                                                   {"repository": {**repo, "default_branch": "dev"}},
                                                   "manaflow-ai/cmux", False),
            "push carrying a pull_request": ("push", "refs/heads/main", {**main, "pull_request": pr},
                                             "manaflow-ai/cmux", False),
            "same-repo pull request": ("pull_request", "refs/pull/1/merge", {"pull_request": pr, "repository": repo},
                                       "manaflow-ai/cmux", False),
            "merge queue": ("merge_group", "refs/heads/gh-readonly-queue/main/pr-1", {"repository": repo},
                            "manaflow-ai/cmux", False),
            "no ref": ("push", None, main, "manaflow-ai/cmux", False),
        }
        for name, (event_name, ref, payload, repository, admitted) in cases.items():
            with self.subTest(name):
                path = event(self.dir, "".join(c if c.isalnum() else "-" for c in name), payload)
                result = self.run_hook("job-started", event_name, path, "--allowed-owner", "manaflow-ai",
                                       "--trusted-ref", "refs/heads/main", "--trusted-repo", "manaflow-ai/cmux",
                                       "--no-disk", repo=repository, env={"GITHUB_REF": ref} if ref else None)
                self.assertEqual(result.returncode == 0, admitted, result.stdout + result.stderr)
                if not admitted:
                    self.assertIn("refused: ", result.stdout)
        path = event(self.dir, "trusted-ref-alone", main)
        self.assertEqual(self.run_hook("job-started", "push", path, "--trusted-ref", "refs/heads/main", "--no-disk",
                                       repo="manaflow-ai/cmux", env={"GITHUB_REF": "refs/heads/main"}).returncode, 1,
                         "--trusted-ref without --trusted-repo fails closed")
        path = event(self.dir, "plain-pr", {"pull_request": pr, "repository": CMUX})
        self.assertEqual(self.run_hook("job-started", "pull_request", path, "--allowed-owner", "manaflow-ai",
                                       "--no-disk", repo="manaflow-ai/cmux").returncode, 0,
                         "without --trusted-ref a same-repo PR is admitted as before")

    def test_disk_pressure_never_fails_and_is_bounded(self) -> None:
        marker = self.dir / "disk-ran"
        slow = make_executable(self.dir / "glaeda-disk-slow", "import time, pathlib, sys\n"
                               f"pathlib.Path({os.fspath(marker)!r}).write_text(' '.join(sys.argv[1:]))\n"
                               "time.sleep(20)\n")
        broken = make_executable(self.dir / "glaeda-disk-broken", "import sys\nsys.exit(3)\n")
        push = event(self.dir, "push", {"repository": CMUX})
        start = time.monotonic()
        result = self.run_hook("job-started", "push", push, "--allowed-repo", "manaflow-ai/cmux",
                               "--disk", os.fspath(slow), repo="manaflow-ai/cmux",
                               env={"GLAEDA_RUNNER_DISK_TIMEOUT": "1"})
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertLess(time.monotonic() - start, 10)
        self.assertIn("timed out", result.stdout)
        self.assertEqual(marker.read_text(), "--pressure --apply --top 0")
        for phase in ("job-started", "job-completed"):
            result = self.run_hook(phase, "push", push, "--allowed-repo", "manaflow-ai/cmux",
                                   "--disk", os.fspath(broken), repo="manaflow-ai/cmux")
            self.assertEqual(result.returncode, 0, result.stdout)
        # job-completed never fails, even with no event at all
        self.assertEqual(self.run_hook("job-completed", None, None, "--disk", os.fspath(broken)).returncode, 0)

    def test_refused_job_does_not_touch_disk(self) -> None:
        marker = self.dir / "disk-ran"
        disk = make_executable(self.dir / "glaeda-disk", f"import pathlib\npathlib.Path({os.fspath(marker)!r}).touch()\n")
        name, payload, _ = SAMPLE_EVENTS["fork-pr"]
        result = self.run_hook("job-started", name, event(self.dir, "fork", payload), "--allowed-repo",
                               "manaflow-ai/cmux", "--disk", os.fspath(disk), repo="manaflow-ai/cmux")
        self.assertEqual(result.returncode, 1)
        self.assertFalse(marker.exists())

    # ------------------------------------------------------------ host lock, reservation, disk floor

    def fleet(self) -> Path:
        fleet = self.dir / "fleet"
        fleet.mkdir(exist_ok=True)
        (fleet / "host.lock").touch()
        return fleet

    def started(self, *extra: str, watch: int | None = None, env: dict | None = None) -> subprocess.CompletedProcess:
        push = event(self.dir, "push", {"repository": CMUX})
        args = ["--allowed-repo", "manaflow-ai/cmux", "--no-disk", "--state-dir", os.fspath(self.dir / "state"),
                "--watch-pid", str(watch or os.getpid()), *extra]
        return self.run_hook("job-started", "push", push, *args, repo="manaflow-ai/cmux", env=env)

    def lock_free(self) -> bool:
        import fcntl
        fd = os.open(self.fleet() / "host.lock", os.O_RDWR)
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            fcntl.flock(fd, fcntl.LOCK_UN)
            return True
        except OSError:
            return False
        finally:
            os.close(fd)

    def test_reservation_markers(self) -> None:
        fleet = self.fleet()
        now = int(time.time())
        v1 = {"schema": "glaeda-reservation/v1", "owner": "fleet-session", "purpose": "chromium campaign",
              "since": now - 60, "until": now + 3600}
        cases = {
            "active": (v1, False, "host reserved by fleet-session for chromium campaign until"),
            "expired": ({**v1, "until": now - 1}, True, "admitted"),
            "iso-string-until": ({**v1, "until": "2099-01-01T00:00:00Z"}, False, "until is not integer"),
            "float-until": ({**v1, "until": now + 3600.5}, False, "until is not integer"),
            "bool-until": ({**v1, "until": True}, False, "until is not integer"),
            "millisecond-until": ({**v1, "until": now * 1000}, False, "until is outside"),
            "negative-since": ({**v1, "since": -1}, False, "since is outside"),
            "missing-since": ({k: v for k, v in v1.items() if k != "since"}, False, "since is not integer"),
            "wrong-schema": ({**v1, "schema": "other/v1"}, False, "schema is not"),
            "not-json": ("{nope", False, "not JSON"),
        }
        for name, (doc, admitted, text) in cases.items():
            with self.subTest(name):
                (fleet / "reservation.json").write_text(doc if isinstance(doc, str) else json.dumps(doc))
                result = self.started()
                self.assertEqual(result.returncode == 0, admitted, result.stdout)
                self.assertIn(text, result.stdout)
                self.run_hook("job-completed", None, None, "--no-disk", "--state-dir", os.fspath(self.dir / "state"))
        (fleet / "reservation.json").unlink()
        self.assertEqual(self.started().returncode, 0)
        self.run_hook("job-completed", None, None, "--no-disk", "--state-dir", os.fspath(self.dir / "state"))

    def test_marker_without_the_parser_refuses(self) -> None:
        fleet = self.fleet()
        (fleet / "reservation.json").write_text("{}")
        lonely = self.dir / "lonely"
        lonely.mkdir()
        (lonely / "glaeda-cmux-runner-hook").write_bytes(HOOK.read_bytes())
        push = event(self.dir, "push", {"repository": CMUX})
        result = subprocess.run([sys.executable, os.fspath(lonely / "glaeda-cmux-runner-hook"), "job-started",
                                 "--allowed-repo", "manaflow-ai/cmux", "--no-disk"], capture_output=True, text=True,
                                timeout=30, env={"PATH": "/usr/bin:/bin", "HOME": os.fspath(self.dir),
                                                 "GLAEDA_FLEET_DIR": os.fspath(fleet), "GITHUB_EVENT_NAME": "push",
                                                 "GITHUB_EVENT_PATH": os.fspath(push),
                                                 "GITHUB_REPOSITORY": "manaflow-ai/cmux"})
        self.assertEqual(result.returncode, 1)
        self.assertIn("glaeda_reservation.py is missing", result.stdout)

    def test_disk_floor_refuses(self) -> None:
        self.fleet()
        result = self.started("--min-free-gib", "999999999")
        self.assertEqual(result.returncode, 1)
        self.assertIn("GiB required", result.stdout)
        self.assertTrue(self.lock_free())

    def test_lock_held_by_another_build_refuses_fast(self) -> None:
        fleet = self.fleet()
        holder = subprocess.Popen([sys.executable, "-c",
                                   "import fcntl,os,sys,time\n"
                                   f"fd=os.open({os.fspath(fleet / 'host.lock')!r},os.O_RDWR)\n"
                                   "fcntl.flock(fd,fcntl.LOCK_EX)\nprint('held',flush=True)\ntime.sleep(30)\n"],
                                  stdout=subprocess.PIPE, text=True)
        try:
            self.assertEqual(holder.stdout.readline().strip(), "held")
            start = time.monotonic()
            result = self.started()
            self.assertEqual(result.returncode, 1, result.stdout)
            self.assertIn("another build holds the fleet host lock", result.stdout)
            self.assertLess(time.monotonic() - start, 10)
        finally:
            holder.kill()
            holder.wait()
            holder.stdout.close()

    def test_job_holds_the_lock_until_completed(self) -> None:
        self.fleet()
        start = time.monotonic()
        result = self.started()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("holding the fleet host lock", result.stdout)
        self.assertLess(time.monotonic() - start, 10, "the holder must not keep the hook's output open")
        self.assertFalse(self.lock_free())
        done = self.run_hook("job-completed", None, None, "--no-disk", "--state-dir", os.fspath(self.dir / "state"))
        self.assertIn("released the fleet host lock", done.stdout)
        self.assertTrue(self.lock_free())

    def test_lock_frees_itself_when_the_job_dies(self) -> None:
        self.fleet()
        worker = subprocess.Popen(["/bin/sleep", "30"])
        try:
            self.assertEqual(self.started(watch=worker.pid).returncode, 0)
            self.assertFalse(self.lock_free())
        finally:
            worker.kill()
            worker.wait()
        deadline = time.monotonic() + 10
        while not self.lock_free() and time.monotonic() < deadline:
            time.sleep(0.2)
        self.assertTrue(self.lock_free())

    def test_stale_holder_file_is_not_success(self) -> None:
        self.fleet()
        state = self.dir / "state"
        state.mkdir()
        stale = hook.holder_file(state)  # keyed by RUNNER_NAME, which CI's own runner sets
        stale.write_text(f"{os.getpid()}\n")  # a live pid, but not a holder
        with mock.patch.object(hook.os, "fork", side_effect=OSError("no fork")):
            held, note = hook.take_host_lock(os.fspath(self.dir / "fleet/host.lock"), os.getpid(), state)
        self.assertFalse(held)
        self.assertIn("cannot start the lock holder", note)
        self.assertFalse(stale.exists())
        self.assertTrue(self.lock_free())

    def test_huge_until_is_described_not_raised(self) -> None:
        reservation = load("glaeda_reservation_under_test", ROOT / "scripts" / "glaeda_reservation.py")
        text = reservation.describe({"owner": "o", "purpose": "p", "since": 0, "until": 10**30})
        self.assertIn("not a representable time", text)

    # ------------------------------------------------------------ weighted capacity

    def job(self, name: str, runner: str, units: int = 4, watch: int | None = None, *extra: str,
            env: dict | None = None) -> subprocess.CompletedProcess:
        return self.started("--capacity-units", str(units), "--capacity-dir", os.fspath(self.dir / "capacity"), *extra,
                            watch=watch, env={"GITHUB_JOB": name, "RUNNER_NAME": runner, **(env or {})})

    def finish(self, runner: str) -> str:
        return self.run_hook("job-completed", None, None, "--no-disk", "--state-dir", os.fspath(self.dir / "state"),
                             env={"RUNNER_NAME": runner}).stdout

    def shared_lock_blocks_exclusive(self) -> bool:
        return not self.lock_free()

    def test_capacity_admits_by_weight_and_refuses_fast_when_full(self) -> None:
        self.fleet()
        try:
            first = self.job("macos-compile-admission", "r0")
            self.assertEqual(first.returncode, 0, first.stdout + first.stderr)
            self.assertIn("holding 2/4 units+persistent-dd+root-1 for macos-compile-admission (compile", first.stdout)
            second = self.job("macos-compile-admission", "r1")
            self.assertEqual(second.returncode, 1)
            self.assertIn("refused: capacity: the persistent-dd token is taken", second.stdout)
            self.assertIn("1/4 units for swift-package-tests (light", self.job("swift-package-tests", "r2").stdout)
            self.assertEqual(self.job("cli-pipe-regressions", "r3").returncode, 0)
            start = time.monotonic()
            full = self.job("swift-package-tests", "r4")
            self.assertLess(time.monotonic() - start, 10, "a full mini refuses, it never waits")
            self.assertEqual(full.returncode, 1)
            self.assertIn("refused: capacity: 0 of 4 units free", full.stdout)
            self.assertTrue(self.shared_lock_blocks_exclusive(), "with-host-lock's LOCK_EX must wait for our jobs")
            self.assertIn("released", self.finish("r0"))
            self.assertIn("persistent-dd", self.job("macos-compile-admission", "r5").stdout)
        finally:
            for runner in ("r0", "r2", "r3", "r5"):
                self.finish(runner)
        self.assertTrue(self.lock_free(), "every holder let go")

    def test_job_log_records_started_refused_and_completed(self) -> None:
        self.fleet()
        log = self.dir / "Library/Logs/glaeda-cmux-jobs.jsonl"
        gh = {"GLAEDA_RUNNER_TELEMETRY": "1", "GITHUB_RUN_ID": "987", "GITHUB_RUN_ATTEMPT": "2",
              "GITHUB_WORKFLOW": "CI", "GITHUB_REF": "refs/pull/5/merge", "GITHUB_HEAD_REF": "feat-x",
              "GITHUB_SHA": "a" * 40}
        try:
            first = self.job("macos-compile-admission", "e0", 4, None, "--instance", "1", env=gh)
            self.assertEqual(first.returncode, 0, first.stdout + first.stderr)
            second = self.job("macos-compile-admission", "e1", 4, None, env=gh)
            self.assertEqual(second.returncode, 1, second.stdout)
        finally:
            self.run_hook("job-completed", None, None, "--no-disk", "--state-dir", os.fspath(self.dir / "state"),
                          env={"RUNNER_NAME": "e0", "GLAEDA_RUNNER_TELEMETRY": "1"})
        records = [json.loads(line) for line in log.read_text().splitlines()]
        by_event = {r["event"]: r for r in records}
        self.assertEqual(sorted(by_event), ["completed", "refused", "started"])
        started, refused, completed = by_event["started"], by_event["refused"], by_event["completed"]
        for record in records:
            self.assertEqual(record["schema"], "glaeda-cmux-job/v1")
            self.assertEqual((record["run_id"], record["run_attempt"], record["workflow"], record["ref"],
                              record["head_ref"], record["sha"], record["repository"], record["job"]),
                             ("987", "2", "CI", "refs/pull/5/merge", "feat-x", "a" * 40, "manaflow-ai/cmux",
                              "macos-compile-admission"))
            self.assertIsInstance(record["at"], int)
            self.assertIn("decision", record)
        self.assertEqual((started["runner"], started["units"], started["roots"], started["gui"],
                          started["instance"]), ("e0", 2, ["root-1"], False, 1))
        self.assertIn("holding 2/4 units+persistent-dd+root-1", started["decision"])
        self.assertIsInstance(started["wait_s"], float)
        self.assertEqual((refused["runner"], refused["units"], refused["roots"], refused["gui"]), ("e1", 0, [], False))
        self.assertIn("capacity: the persistent-dd token is taken", refused["decision"])
        self.assertIsInstance(refused["wait_s"], float)
        self.assertEqual((completed["runner"], completed["roots"], completed["units"]), ("e0", ["root-1"], 2))
        self.assertNotIn("roots_admitted", completed)
        self.assertIn("verdict", completed)

    def test_early_refusal_logs_without_a_wait(self) -> None:
        pr = event(self.dir, "pr", {"repository": {"full_name": "someone/fork"}})
        result = self.run_hook("job-started", "pull_request_target", pr, "--allowed-repo", "manaflow-ai/cmux",
                               "--no-disk", env={"GLAEDA_RUNNER_TELEMETRY": "1", "RUNNER_NAME": "e2"})
        self.assertEqual(result.returncode, 1)
        [record] = [json.loads(line) for line in
                    (self.dir / "Library/Logs/glaeda-cmux-jobs.jsonl").read_text().splitlines()]
        self.assertEqual((record["event"], record["wait_s"], record["repository"]), ("refused", None, "someone/fork"))

    def take(self, root: str, runner: str, *extra: str, env: dict | None = None) -> subprocess.CompletedProcess:
        return self.step(["take-root", "--root", root, "--canonical-roots", "2", *extra], runner, env)

    def take_gui(self, runner: str, *extra: str) -> subprocess.CompletedProcess:
        return self.step(["take-gui", *extra], runner)

    def step(self, argv: list[str], runner: str, env: dict | None = None) -> subprocess.CompletedProcess:
        """Run a hook phase as a job step would: under a process named Runner.Worker (its ancestor).
        A real Runner.Worker outlives the step and the root holder watches it, so the stand-in reports
        the step's exit status and then stays up until the test ends; one that exited with the step
        would let the holder release the root within HOLDER_POLL_S."""
        worker = self.dir / "Runner.Worker"
        if not worker.exists():  # a real parent process named Runner.Worker (a copied /bin/sh is killed on macOS)
            source = self.dir / "worker.c"
            source.write_text("#include <fcntl.h>\n#include <stdio.h>\n#include <sys/wait.h>\n#include <unistd.h>\n"
                              "int main(int c, char **v) {\n"
                              "  pid_t p = fork();\n"
                              "  if (p == 0) { dup2(open(\"/dev/null\", O_RDONLY), 0); execv(\"/bin/sh\", v); _exit(127); }\n"
                              "  int s = 0; waitpid(p, &s, 0);\n"
                              f"  printf(\"{WORKER_STEP_EXIT}%d\\n\", WIFEXITED(s) ? WEXITSTATUS(s) : 1);\n"
                              "  fflush(stdout); close(1); close(2);\n"
                              "  char b; while (read(0, &b, 1) > 0) {}\n"  # up until the test closes stdin
                              "  return 0; }\n")
            cc = shutil.which("cc") or shutil.which("clang") or shutil.which("gcc")
            if cc is None or subprocess.run([cc, "-o", os.fspath(worker), os.fspath(source)],
                                            capture_output=True).returncode != 0:
                self.skipTest("no C compiler to build a Runner.Worker stand-in")
        cmd = " ".join(shlex.quote(a) for a in [sys.executable, os.fspath(HOOK), *argv,
                                                "--capacity-dir", os.fspath(self.dir / "capacity"),
                                                "--state-dir", os.fspath(self.dir / "state")])
        environ = {"PATH": "/usr/bin:/bin", "HOME": os.fspath(self.dir), "GLAEDA_RUNNER_TELEMETRY": "0", "RUNNER_NAME": runner,
                   "GLAEDA_FLEET_DIR": os.fspath(self.dir / "fleet"), **(env or {})}
        proc = subprocess.Popen([os.fspath(worker), "-c", cmd], env=environ, stdin=subprocess.PIPE,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        self.addCleanup(self.end_worker, proc)
        timer = threading.Timer(60, proc.kill)
        timer.start()
        try:
            stdout, stderr = proc.stdout.read(), proc.stderr.read()  # EOF once the step is done
        finally:
            timer.cancel()
        stdout, marker, status = stdout.rpartition(WORKER_STEP_EXIT)
        self.assertTrue(marker, f"the Runner.Worker stand-in did not report the step: {stderr}")
        return subprocess.CompletedProcess(proc.args, int(status), stdout, stderr)

    @staticmethod
    def end_worker(proc: subprocess.Popen) -> None:
        """End a Runner.Worker stand-in, as the job ending would."""
        with contextlib.suppress(OSError):
            proc.stdin.close()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()
        proc.stdout.close()
        proc.stderr.close()

    def test_two_roots_consumers_take_the_producers_root_in_their_step(self) -> None:
        self.fleet()
        two = ("--canonical-roots", "2", "--compile-slots", "2")
        try:
            compile_ = self.job("macos-compile-admission", "p0", 8, None, *two)
            self.assertIn("persistent-dd+root-1", compile_.stdout)
            consumer = self.job("cli-product-tests", "c0", 8, None, *two)
            self.assertEqual(consumer.returncode, 0, consumer.stdout)
            self.assertNotIn("root-", consumer.stdout.split("holding", 1)[1], "a consumer takes no root at job start")
            busy = self.take("/private/tmp/cmux-ci", "c0", "--wait", "1")
            self.assertEqual(busy.returncode, 1, busy.stderr)
            self.assertIn("still in use", busy.stderr)
            got = self.take("/private/tmp/cmux-ci-2", "c0")
            self.assertEqual((got.returncode, got.stdout.strip()), (0, "/private/tmp/cmux-ci-2"), got.stderr)
            self.assertEqual(self.take("2", "c0").returncode, 0, "a re-take of a root this job holds is a no-op")
            second = self.take("/private/tmp/cmux-ci", "c0", "--wait", "0")
            self.assertEqual(second.returncode, 2, "a second, different root could deadlock against another job")
            self.assertIn("already holds root-2", second.stderr)
            other = self.take("/private/tmp/cmux-ci-2", "c1", "--wait", "1")
            self.assertEqual(other.returncode, 1, "another job cannot take a held root")
            self.assertIn("canonical root holder(s) released", self.finish("c0"))
            self.assertEqual(self.take("/private/tmp/cmux-ci-2", "c1").returncode, 0, "released with the job")
            self.finish("c1")
            again = self.take("1", "p0")
            self.assertEqual(again.returncode, 0, "the producer re-taking its own root is a no-op")
            self.assertFalse((self.dir / "state" / "host-lock-holder-p0-root-1.pid").exists())
        finally:
            for runner in ("p0", "c0", "c1"):
                self.finish(runner)
        self.assertTrue(self.lock_free())

    def test_two_roots_unknown_jobs_are_pinned_to_root_one(self) -> None:
        self.fleet()
        two = ("--canonical-roots", "2", "--compile-slots", "2")
        try:
            self.assertIn("root-1", self.job("macos-compile-admission", "u0", 8, None, *two).stdout)
            pinned = self.job("app-host-test-rerun", "u1", 8, None, *two)
            self.assertEqual(pinned.returncode, 1, "an unknown job uses /private/tmp/cmux-ci itself")
            self.assertIn("canonical root token is taken", pinned.stdout)
            self.assertIn("root-2", self.job("macos-compile-admission", "u2", 8, None, *two).stdout)
        finally:
            for runner in ("u0", "u1", "u2"):
                self.finish(runner)

    def test_switch_swaps_a_producers_root_without_holding_two(self) -> None:
        # test-e2e's build reuses a product compiled at another root: it lets its own root go, then takes that one
        self.fleet()
        e2e = {"GITHUB_WORKFLOW_REF": "manaflow-ai/cmux/.github/workflows/test-e2e.yml@refs/heads/main"}
        two = ("--canonical-roots", "2", "--compile-slots", "2")
        state = self.dir / "state"
        try:
            build = self.job("build", "e0", 8, None, *two, env=e2e)
            self.assertIn("units+root-1 for build (compile-gui", build.stdout)
            self.assertTrue((state / "host-lock-holder-e0-root-1.pid").exists(), "the root has a holder of its own")
            refused = self.take("2", "e0", "--wait", "0")
            self.assertEqual(refused.returncode, 2, "without --switch a second root is still refused")
            self.assertIn("--switch", refused.stderr)
            # root 2 busy: a switch that times out keeps the old root, never leaving the job without one
            self.assertEqual(self.take("2", "b0").returncode, 0)
            late = self.take("2", "e0", "--switch", "--wait", "1")
            self.assertEqual(late.returncode, 1, late.stderr)
            self.assertEqual(self.take("1", "x0", "--wait", "0").returncode, 1, "e0 still holds root 1")
            self.assertEqual((state / "host-lock-holder-e0.roots").read_text().split(), ["root-1"])
            self.finish("b0")
            env_file = self.dir / "switch_env"
            switched = self.take("/private/tmp/cmux-ci-2", "e0", "--switch", env={"GITHUB_ENV": os.fspath(env_file)})
            self.assertEqual((switched.returncode, switched.stdout.strip()), (0, "/private/tmp/cmux-ci-2"),
                             switched.stderr)
            self.assertIn("let go of root-1", switched.stderr)
            self.assertEqual(env_file.read_text(), "CMUX_CI_CANONICAL_ROOT=/private/tmp/cmux-ci-2\n")
            self.assertEqual((state / "host-lock-holder-e0.roots").read_text().split(), ["root-2"])
            self.assertEqual(self.take("1", "c0", "--wait", "5").returncode, 0, "root 1 is free again")
            full = self.job("swift-package-tests", "g0", 2, None, *two)  # e0 still holds unit-0 and unit-1
            self.assertIn("0 of 2 units free", full.stdout, "the admission holder keeps its units")
            self.assertEqual(self.take("2", "e0").returncode, 0, "a re-take of the new root is a no-op")
            self.assertIn("canonical root holder(s) released", self.finish("e0"))
            self.assertEqual(self.take("2", "c1", "--wait", "5").returncode, 0, "released with the job")
            self.finish("c0")
            self.finish("c1")
            compile_ = self.job("macos-compile-admission", "p0", 8, None, *two)
            self.assertIn("persistent-dd+root-1", compile_.stdout)
            self.assertEqual(self.take("2", "p0", "--switch").returncode, 2,
                             "a root in the admission holder cannot be let go")
            (state / "host-lock-holder-p0-root-1.pid").write_text("999999\n")  # a killed holder's leftover
            self.assertEqual(self.take("2", "p0", "--switch").returncode, 2, "a dead holder lets nothing go")
        finally:
            for runner in ("e0", "c0", "c1", "g0", "p0", "b0", "x0"):
                self.finish(runner)
        self.assertFalse(list((self.dir / "capacity").glob("*.want-*")), "no waiter marker outlives its wait")
        self.assertTrue(self.lock_free())

    def test_take_gui_holds_the_token_for_the_rest_of_the_job(self) -> None:
        self.fleet()
        two = ("--canonical-roots", "2", "--compile-slots", "2")
        state = self.dir / "state"
        try:
            self.assertEqual(self.job("app-host-unit-tests", "g0", 8, None, *two).returncode, 0)
            self.assertTrue((state / "host-lock-holder-g0.gui").exists())
            self.assertEqual(self.take_gui("g0").returncode, 0, "a job holding gui from admission: a no-op")
            self.assertEqual(self.job("macos-compile-admission", "c0", 8, None, *two).returncode, 0)
            waited = time.monotonic()
            busy = self.take_gui("c0", "--wait", "2")
            self.assertEqual(busy.returncode, 1, busy.stderr)
            self.assertIn("still taken after 2 s", busy.stderr)
            self.assertGreaterEqual(time.monotonic() - waited, 2)
            self.finish("g0")
            self.assertEqual(self.take_gui("c0", "--wait", "5").returncode, 0)
            self.assertTrue((state / "host-lock-holder-c0-gui.pid").exists())
            refused = self.job("tests-build-and-lag", "g1", 8, None, *two, "--gui-wait", "0")
            self.assertIn("the gui token is taken", refused.stdout)
            self.assertIn("the gui token holder released", self.finish("c0"))
            self.assertFalse((state / "host-lock-holder-c0.gui").exists())
            self.assertIn("+gui", self.job("tests-build-and-lag", "g2", 8, None, *two).stdout, "released with the job")
            outside = self.run_hook("take-gui", None, None, "--capacity-dir", os.fspath(self.dir / "capacity"),
                                    "--state-dir", os.fspath(state))
            self.assertEqual(outside.returncode, 2)
        finally:
            for runner in ("g0", "c0", "g1", "g2"):
                self.finish(runner)
        self.assertTrue(self.lock_free())

    def fake_ioreg(self, locked: bool | None) -> dict[str, str]:
        """GLAEDA_RUNNER_IOREG pointing at a script that prints a console session: locked, unlocked, or (None) at
        the login window with no user."""
        users = ('"IOConsoleUsers" = ()' if locked is None else
                 '"IOConsoleUsers" = ({"kCGSSessionOnConsoleKey"=Yes,"kCGSessionLoginDoneKey"=Yes,'
                 f'"kCGSSessionUserNameKey"="cmux"{',"CGSSessionScreenIsLocked"=Yes' if locked else ""}}})')
        script = self.dir / f"ioreg-{locked}"
        script.write_text(f"#!/bin/sh\ncat <<'X'\n+-o Root\n    {{\n      \"IOConsoleLocked\" = No\n      {users}\n    }}\nX\n")
        script.chmod(0o755)
        return {"GLAEDA_RUNNER_IOREG": os.fspath(script)}

    def test_a_locked_console_refuses_gui_jobs_before_taking_capacity(self) -> None:
        self.fleet()
        state = self.dir / "state"
        locked = self.fake_ioreg(True)
        try:
            for name in ("app-host-unit-tests", "tests-build-and-lag"):
                refused = self.job(name, "g0", 8, None, env=locked)
                self.assertEqual(refused.returncode, 1, refused.stdout)
                self.assertIn("refused: console: the console session (cmux) is screen-locked", refused.stdout)
                self.assertFalse((state / "host-lock-holder-g0.gui").exists(), "no gui token taken")
            nobody = self.job("app-host-unit-tests", "g0", 8, None, env=self.fake_ioreg(None))
            self.assertIn("refused: console: no user is logged in at the console", nobody.stdout)
            # the gui runner's label brings only console jobs, whatever their id
            side = self.job("swift-package-tests", "g0", 8, None, "--gui-runner", env=locked)
            self.assertIn("refused: console:", side.stdout)
            off = self.job("app-host-unit-tests", "g1", 8, None, env={**locked, "GLAEDA_RUNNER_CONSOLE_GATE": "0"})
            self.assertEqual(off.returncode, 0, "the kill switch turns the check off")
            self.finish("g1")
            compile_ = self.job("macos-compile-admission", "c0", 8, None, env=locked)
            self.assertEqual(compile_.returncode, 0, "a compile needs no console session")
            self.assertEqual(self.job("swift-package-tests", "l0", 8, None, env=locked).returncode, 0)
            gave = self.step(["take-gui", "--wait", "5"], "c0", env=locked)
            self.assertEqual(gave.returncode, hook.TAKE_GUI_GAVE_WAY, "the build leaves its tests to the test job")
            self.assertIn("take-gui: console: the console session (cmux) is screen-locked", gave.stderr)
            self.assertFalse((state / "host-lock-holder-c0-gui.pid").exists())
            self.finish("c0")  # its root
            admitted = self.job("app-host-unit-tests", "g2", 8, None, env=self.fake_ioreg(False))
            self.assertEqual(admitted.returncode, 0, admitted.stdout)
            self.assertIn("+gui", admitted.stdout)
        finally:
            for runner in ("g0", "g1", "g2", "c0", "l0"):
                self.finish(runner)
        self.assertTrue(self.lock_free())

    def test_take_gui_gives_way_to_a_gui_job_waiting_for_its_root(self) -> None:
        # the opposite lock orders: a build holds root 1 and wants gui; a gui job holds gui and wants root 1
        self.fleet()
        two = ("--canonical-roots", "2", "--compile-slots", "2")
        results: dict[str, subprocess.CompletedProcess] = {}
        try:
            self.assertIn("root-1", self.job("macos-compile-admission", "c0", 8, None, *two).stdout)
            self.assertEqual(self.job("app-host-unit-tests", "g0", 8, None, *two).returncode, 0)
            waiter = threading.Thread(target=lambda: results.setdefault("g0", self.take("1", "g0", "--wait", "40")))
            waiter.start()
            deadline = time.monotonic() + 15
            while not list((self.dir / "capacity").glob("root-1.want-*")) and time.monotonic() < deadline:
                time.sleep(0.2)
            self.assertEqual([m.read_text() for m in (self.dir / "capacity").glob("root-1.want-*")], ["gui\n"])
            waited = time.monotonic()
            gave = self.take_gui("c0", "--wait", "30")
            self.assertEqual(gave.returncode, hook.TAKE_GUI_GAVE_WAY, gave.stderr)
            self.assertIn("waiting for root-1, which this job holds; giving way", gave.stderr)
            self.assertLess(time.monotonic() - waited, 15, "it gives way at once, not after its wait")
            self.finish("c0")
            waiter.join(60)
            self.assertEqual(results["g0"].returncode, 0, results["g0"].stderr)
            # a gui holder not waiting for this job's root is no cycle: take-gui keeps waiting for the token
            self.assertIn("root-2", self.job("macos-compile-admission", "c1", 8, None, *two).stdout)
            self.assertEqual(self.take_gui("c1", "--wait", "1").returncode, 1)
            # a marker its waiter stopped refreshing is stale, even if its pid is alive (recycled or foreign)
            stale = self.dir / "capacity" / f"root-2.want-{os.getpid()}"
            stale.write_text("gui\n")
            os.utime(stale, (time.time() - 60, time.time() - 60))
            self.assertEqual(self.take_gui("c1", "--wait", "1").returncode, 1, "no give-way to a stale marker")
            self.assertFalse(stale.exists(), "and it is swept")
        finally:
            for runner in ("c0", "g0", "c1"):
                self.finish(runner)
        self.assertFalse(list((self.dir / "capacity").glob("*.want-*")))
        self.assertTrue(self.lock_free())

    def test_take_root_refuses_bad_roots_and_non_jobs(self) -> None:
        for bad in ("/tmp/elsewhere", "/private/tmp/cmux-ci-1", "0", "cmux-ci-2x", "02", "/private/tmp/cmux-ci-02"):
            with self.subTest(bad=bad):
                self.assertEqual(self.take(bad, "x0").returncode, 2)
        beyond = self.take("3", "x0", "--canonical-roots", "2")
        self.assertEqual(beyond.returncode, 2, "a root this mini does not have")
        self.assertIn("2 canonical root(s)", beyond.stderr)
        nameless = self.run_hook("take-root", None, None, "--root", "1", "--state-dir", os.fspath(self.dir / "state"))
        self.assertIn("RUNNER_NAME is not set", nameless.stderr)
        if os.environ.get("GITHUB_ACTIONS"):  # CI itself runs under a real Runner.Worker, so it is "inside a job"
            return
        outside = self.run_hook("take-root", None, None, "--root", "1", "--capacity-dir",
                                os.fspath(self.dir / "capacity"), "--state-dir", os.fspath(self.dir / "state"),
                                env={"RUNNER_NAME": "x0"})
        self.assertEqual(outside.returncode, 2)
        self.assertIn("not inside a runner job", outside.stderr)


    def test_job_started_links_the_root_shim_into_the_fleet_bin(self) -> None:
        fleet = self.fleet()
        (fleet / "bin").mkdir()
        hooks = self.dir / "hooks"
        hooks.mkdir()
        shutil.copy(HOOK, hooks / "glaeda-cmux-runner-hook")
        (hooks / "glaeda-canonical-root").write_text("#!/bin/sh\n")
        environ = {"PATH": "/usr/bin:/bin", "HOME": os.fspath(self.dir), "GLAEDA_RUNNER_TELEMETRY": "0", "GLAEDA_FLEET_DIR": os.fspath(fleet),
                   "GITHUB_EVENT_NAME": "push", "GITHUB_EVENT_PATH": os.fspath(event(self.dir, "push", {"repository": CMUX})),
                   "GITHUB_REPOSITORY": "manaflow-ai/cmux", "GITHUB_JOB": "swift-package-tests", "RUNNER_NAME": "s0"}
        run = subprocess.run([sys.executable, os.fspath(hooks / "glaeda-cmux-runner-hook"), "job-started",
                              "--allowed-repo", "manaflow-ai/cmux", "--no-disk", "--capacity-units", "4",
                              "--capacity-dir", os.fspath(self.dir / "capacity"), "--state-dir", os.fspath(self.dir / "state"),
                              "--watch-pid", str(os.getpid())], env=environ, capture_output=True, text=True, timeout=30)
        try:
            self.assertEqual(run.returncode, 0, run.stdout + run.stderr)
            link = fleet / "bin" / "glaeda-canonical-root"
            self.assertTrue(link.is_symlink())
            self.assertEqual(os.readlink(link), os.fspath((hooks / "glaeda-canonical-root").resolve()))
        finally:
            self.finish("s0")

    def test_capacity_one_root_job_per_canonical_root(self) -> None:
        # a consumer's rm -rf of <root>/src must never meet a compile or another consumer in that root
        self.fleet()
        env_file, temp = self.dir / "github_env", self.dir / "runner_temp"
        temp.mkdir()
        try:
            compile_ = self.job("macos-compile-admission", "k0", 8, None,
                                env={"GITHUB_ENV": os.fspath(env_file), "RUNNER_TEMP": os.fspath(temp)})
            self.assertIn("CMUX_CI_CANONICAL_ROOT=/private/tmp/cmux-ci via GITHUB_ENV+RUNNER_TEMP", compile_.stdout)
            self.assertEqual(env_file.read_text(), "CMUX_CI_CANONICAL_ROOT=/private/tmp/cmux-ci\n")
            self.assertEqual((temp / "glaeda-canonical-root").read_text(), "/private/tmp/cmux-ci\n")
            for n, consumer in enumerate(("cli-product-tests", "tests-build-and-lag", "app-host-unit-tests")):
                refused = self.job(consumer, f"k{n + 1}", 8)
                self.assertEqual(refused.returncode, 1, refused.stdout)
                self.assertIn("refused: capacity: the canonical root token is taken", refused.stdout)
            self.assertEqual(self.job("swift-package-tests", "k4", 8).returncode, 0, "light jobs take no root")
            self.finish("k0")
            self.assertIn("root-1", self.job("cli-product-tests", "k5", 8).stdout)
            two = self.job("macos-compile-admission", "k6", 8, None, "--canonical-roots", "2", "--compile-slots", "2",
                           env={"GITHUB_ENV": os.fspath(env_file)})
            self.assertIn("persistent-dd+root-2", two.stdout)
            self.assertIn("CMUX_CI_CANONICAL_ROOT=/private/tmp/cmux-ci-2 via GITHUB_ENV", two.stdout)
            self.assertTrue(env_file.read_text().endswith("CMUX_CI_CANONICAL_ROOT=/private/tmp/cmux-ci-2\n"))
        finally:
            for runner in ("k0", "k4", "k5", "k6"):
                self.finish(runner)
        self.assertTrue(self.lock_free())

    def test_two_roots_each_runner_prefers_its_own_root(self) -> None:
        # warm labels (cmux#14396) name one tree, so a root runner keeps compiling in its own root when it can
        self.fleet()
        two = ("--canonical-roots", "2", "--compile-slots", "2")
        try:
            zero = self.job("macos-compile-admission", "r0", 8, None, *two, "--instance", "0")
            one = self.job("macos-compile-admission", "r1", 8, None, *two, "--instance", "1")
            self.assertIn("root-1", zero.stdout)
            self.assertIn("root-2", one.stdout)
            self.finish("r0")
            self.finish("r1")
            # instance 1 first this time: it still gets root 2, and instance 0 still gets root 1
            self.assertIn("root-2", self.job("macos-compile-admission", "r1", 8, None, *two, "--instance", "1").stdout)
            self.assertIn("root-1", self.job("macos-compile-admission", "r0", 8, None, *two, "--instance", "0").stdout)
            self.finish("r0")
            self.finish("r1")
            # root 1 held (as a consumer's take-root holds it): instance 0 falls back to root 2
            held = os.open(self.dir / "capacity" / "root-1.token", os.O_RDWR | os.O_CREAT, 0o644)
            try:
                fcntl.flock(held, fcntl.LOCK_EX)
                fallback = self.job("macos-compile-admission", "r0", 8, None, *two, "--instance", "0")
                self.assertEqual(fallback.returncode, 0, fallback.stdout)
                self.assertIn("root-2", fallback.stdout)
            finally:
                os.close(held)
        finally:
            for runner in ("r0", "r1"):
                self.finish(runner)
        self.assertTrue(self.lock_free())

    def test_two_roots_admission_takes_the_root_warm_for_its_pull_request(self) -> None:
        # the picker sends admission to a runner whose mini is warm for it (cmux warm affinity), but the root
        # follows the token: the hook gives it the root whose kept build is stamped with its key
        fleet = self.fleet()
        two = ("--canonical-roots", "2", "--compile-slots", "2")
        base = "0123456789abcdef0123456789abcdef01234567"

        def stamp(k: int, merged_onto: str, pr: int, fingerprint: str = "fp-owned-rec1") -> None:
            # cmux owned_build_state.py keep's stamp (cmux#14717, #14718)
            store = fleet / "ci" if k == 1 else fleet / "ci" / f"cmux-ci-{k}"
            (store / "derived-data").mkdir(parents=True, exist_ok=True)
            (store / "stamp.json").write_text(json.dumps({"fingerprint": fingerprint, "merged_onto": merged_onto,
                                                          "pr": pr}))

        def admit(runner: str, instance: str, pr: dict) -> subprocess.CompletedProcess:
            payload = {"repository": CMUX, "pull_request": {"head": {"repo": CMUX}, "base": {"repo": CMUX}, **pr}}
            path = event(self.dir, f"pr-{runner}", payload)
            return self.run_hook("job-started", "pull_request", path, "--allowed-repo", "manaflow-ai/cmux",
                                 "--no-disk", "--state-dir", os.fspath(self.dir / "state"),
                                 "--watch-pid", str(os.getpid()), "--capacity-units", "8",
                                 "--capacity-dir", os.fspath(self.dir / "capacity"), *two, "--instance", instance,
                                 repo="manaflow-ai/cmux",
                                 env={"GITHUB_JOB": "macos-compile-admission", "RUNNER_NAME": runner})

        try:
            stamp(1, "f" * 40, 7)
            stamp(2, base, 42)
            # instance 0 owns root 1, but root 2 is kept for this merge base
            warm = admit("r0", "0", {"base": {"repo": CMUX, "sha": base}, "number": 7})
            self.assertEqual(warm.returncode, 0, warm.stdout + warm.stderr)
            self.assertIn("persistent-dd+root-2, warm for 0123456789ab", warm.stdout)
            self.finish("r0")
            # a re-push onto a new base: the root kept for the same pull request
            again = admit("r1", "1", {"base": {"repo": CMUX, "sha": "e" * 40}, "number": 7})
            self.assertIn("persistent-dd+root-1, warm for pr-7", again.stdout)
            self.finish("r1")
            # the merge base beats the pull request, and a warm root that is held falls back to the other
            held = os.open(self.dir / "capacity" / "root-2.token", os.O_RDWR | os.O_CREAT, 0o644)
            try:
                fcntl.flock(held, fcntl.LOCK_EX)
                busy = admit("r0", "0", {"base": {"repo": CMUX, "sha": base}, "number": 7})
                self.assertIn("persistent-dd+root-1 for", busy.stdout)
                self.assertNotIn("warm for", busy.stdout)
            finally:
                os.close(held)
            self.finish("r0")
            # no match, a stamp mid-keep (no fingerprint), or no pull request: the runner's own root
            stamp(2, base, 42, fingerprint="")
            for runner, instance, pr in (("r1", "1", {"base": {"repo": CMUX, "sha": "d" * 40}, "number": 9}),
                                         ("r0", "0", {"base": {"repo": CMUX, "sha": base}})):
                plain = admit(runner, instance, pr)
                self.assertIn(f"root-{int(instance) + 1} for", plain.stdout)
                self.assertNotIn("warm for", plain.stdout)
                self.finish(runner)
        finally:
            for runner in ("r0", "r1"):
                self.finish(runner)
        self.assertTrue(self.lock_free())

    def test_root_warm_keys_never_trust_a_stamp_blindly(self) -> None:
        # any job on the mini can write the stamp: a FIFO, junk, a huge file, another state version or a
        # missing DerivedData reads as not warm, and never blocks or raises
        base, ci = "0123456789abcdef0123456789abcdef01234567", self.dir / "ci"
        (ci / "derived-data").mkdir(parents=True)
        good = {"fingerprint": "fp-owned-rec1", "merged_onto": base.upper(), "pr": 3}
        (ci / "stamp.json").write_text(json.dumps(good))
        self.assertEqual(hook.root_warm_keys(1, os.fspath(self.dir / "ci")), ["0123456789ab", "pr-3"])
        for bad in ({**good, "fingerprint": "fp-owned-rec0"}, {**good, "fingerprint": ""}, [good],
                    {**good, "merged_onto": "HEAD", "pr": True}, "[" * 100000):
            (ci / "stamp.json").write_text(bad if isinstance(bad, str) else json.dumps(bad))
            self.assertIn(hook.root_warm_keys(1, os.fspath(ci)), ([], ), bad if isinstance(bad, dict) else "junk")
        (ci / "stamp.json").write_text(json.dumps(good))
        os.rmdir(ci / "derived-data")
        self.assertEqual(hook.root_warm_keys(1, os.fspath(ci)), [])
        (ci / "derived-data").mkdir()
        (ci / "stamp.json").unlink()
        os.mkfifo(ci / "stamp.json")
        start = time.monotonic()
        self.assertEqual(hook.root_warm_keys(1, os.fspath(ci)), [])
        self.assertLess(time.monotonic() - start, 5)
        self.assertEqual(hook.root_warm_keys(2, os.fspath(ci)), [])  # no cmux-ci-2 at all

    def test_warm_root_costs_rank_roots_by_predicted_compile(self) -> None:
        ci = self.dir / "ci"
        mirror = self.dir / "work"  # the commits; the hook reads a bare copy, as the prefetch keeps it
        mirror.mkdir(parents=True)
        git = ["git", "-C", os.fspath(mirror), "-c", "user.email=t@t", "-c", "user.name=t"]
        subprocess.run([*git, "init", "-q"], check=True)
        (mirror / "Sources").mkdir()
        (mirror / "Sources" / "A.swift").write_text("a")
        subprocess.run([*git, "add", "."], check=True)
        subprocess.run([*git, "commit", "-qm", "0"], check=True)
        old = subprocess.run([*git, "rev-parse", "HEAD"], check=True, capture_output=True, text=True).stdout.strip()
        for n in range(3):
            (mirror / "Sources" / f"M{n}.swift").write_text("m")
        subprocess.run([*git, "add", "."], check=True)
        subprocess.run([*git, "commit", "-qm", "1"], check=True)
        new = subprocess.run([*git, "rev-parse", "HEAD"], check=True, capture_output=True, text=True).stdout.strip()
        (ci / ".prefetch").mkdir(parents=True)
        subprocess.run(["git", "clone", "-q", "--bare", os.fspath(mirror), os.fspath(ci / ".prefetch" / "cmux.git")],
                       check=True)

        def stamp(k: int, onto: str, pr: int, files: list[str], **extra) -> None:
            store = ci if k == 1 else ci / f"cmux-ci-{k}"
            (store / "derived-data").mkdir(parents=True, exist_ok=True)
            (store / "stamp.json").write_text(json.dumps({"fingerprint": "fp-owned-rec1", "merged_onto": onto,
                                                          "pr": pr, "pr_app_swift_files": files, **extra}))

        # root 1 kept pull request 5 (8 files) three main files back; root 2 kept pull request 6 (1 file) on the base
        stamp(1, old, 5, [f"Sources/P{n}.swift" for n in range(8)], pr_package_interface=False)
        stamp(2, new, 6, ["Sources/Q.swift"], pr_package_interface=False)
        order, predicted = hook.warm_root_costs([0, 1], new, 7, os.fspath(ci))
        self.assertEqual(order, [1, 0])
        self.assertEqual(predicted["root-1"], {"seconds": 266.5, "tier": "far", "app_swift_files": 11})
        self.assertEqual(predicted["root-2"], {"seconds": 140.0, "tier": "near", "app_swift_files": 1})
        # A re-push of pull request 5: its own files are not undone, so both roots are near; the fewer files win
        # (root 2's 1 against root 1's 3 main files), and on an equal count the runner's own root.
        order, predicted = hook.warm_root_costs([0, 1], new, 5, os.fspath(ci))
        self.assertEqual((order, predicted["root-1"]["tier"], predicted["root-1"]["app_swift_files"]), ([1, 0], "near", 3))
        stamp(2, new, 6, [f"Sources/Q{n}.swift" for n in range(3)], pr_package_interface=False)
        self.assertEqual(hook.warm_root_costs([0, 1], new, 5, os.fspath(ci))[0], [0, 1])
        self.assertEqual(hook.warm_root_costs([1, 0], new, 5, os.fspath(ci))[0], [1, 0])
        # A list the stamp cut short may hide a package change: a rebuild unless the stamp says no interface changed.
        stamp(2, new, 6, ["Sources/Q.swift"], pr_app_swift_total=500)
        self.assertEqual(hook.warm_root_costs([0, 1], new, 7, os.fspath(ci))[1]["root-2"]["tier"], "rebuild")
        stamp(2, new, 6, ["Sources/Q.swift"], pr_package_interface=False)
        # The fitted model beside the stamps replaces the default; a package interface change or a hot file rebuilds.
        (ci / hook.WARM_MODEL).write_text(json.dumps({"near_app_swift_files": 5, "hot_files": ["Sources/Q.swift"],
                                                      "tiers": {"near": {"p50": 100}, "far": {"p50": 200},
                                                                "rebuild": {"p50": 300}}}))
        order, predicted = hook.warm_root_costs([0, 1], new, 7, os.fspath(ci))
        self.assertEqual((order, predicted["root-2"]["tier"], predicted["root-1"]["seconds"]), ([0, 1], "rebuild", 200.0))
        for junk in ("[" * 1000, '{"tiers": {"near": {"p50": NaN}, "far": {"p50": 1}, "rebuild": {"p50": 2}}}',
                     '{"tiers": {"near": {"p50": -1}, "far": {"p50": 1}, "rebuild": {"p50": 2}}}'):
            (ci / hook.WARM_MODEL).write_text(junk)
            self.assertEqual(hook.read_warm_model(os.fspath(ci)), hook.WARM_DEFAULT_MODEL, junk)
        # A root with no kept build goes last, after a rebuild.
        (ci / "cmux-ci-2" / "stamp.json").unlink()
        order, predicted = hook.warm_root_costs([1, 0], new, 7, os.fspath(ci))
        self.assertEqual((order, predicted["root-2"]["tier"]), ([0, 1], "cold"))
        stamp(2, new, 6, ["Sources/Q.swift"], pr_package_interface=False)
        # A base the mirror lacks, a stamp of an older cmux, or no base: fall back to the exact keys.
        self.assertEqual(hook.warm_root_costs([0, 1], "e" * 40, 7, os.fspath(ci)), ([], {}))
        self.assertEqual(hook.warm_root_costs([0, 1], "", 7, os.fspath(ci)), ([], {}))
        self.assertEqual(hook.warm_root_costs([0, 1], new, 7, os.fspath(self.dir / "nowhere")), ([], {}))
        (ci / "stamp.json").write_text(json.dumps({"fingerprint": "fp-owned-rec1", "merged_onto": old, "pr": 5}))
        order, predicted = hook.warm_root_costs([0, 1], new, 7, os.fspath(ci))
        self.assertEqual(predicted["root-1"]["tier"], "unknown")
        self.assertEqual(hook.job_warm_target({"pull_request": {"base": {"sha": new.upper()}, "number": 5}}), (new, 5))
        self.assertEqual(hook.job_warm_target({"pull_request": {"number": True}}), ("", None))

    def test_job_warm_keys_come_from_the_pull_request(self) -> None:
        sha = "ABCDEF0123456789abcdef0123456789abcdef01"
        self.assertEqual(hook.job_warm_keys({"pull_request": {"base": {"sha": sha}, "number": 5}}),
                         ("abcdef012345", "pr-5"))
        for payload in (None, {}, {"ref": "refs/heads/main"}, {"pull_request": {"base": {"sha": "x"}, "number": "5"}},
                        {"pull_request": {"number": True}}, {"pull_request": []}):
            self.assertEqual(hook.job_warm_keys(payload), (), payload)

    def test_capacity_ios_jobs_take_no_root_or_gui_token(self) -> None:
        self.fleet()
        try:
            self.assertIn("persistent-dd+root-1", self.job("macos-compile-admission", "i0", 8).stdout)
            for n, job in enumerate(("ios-simulator-build", "mobile-core-package")):
                result = self.job(job, f"i{n + 1}", 12)
                self.assertEqual(result.returncode, 0, result.stdout)
                self.assertIn(f"holding 2/12 units for {job} (isolated", result.stdout)
            sim = self.job("ios-simulator", "i3", 12)
            self.assertIn("2/12 units+simulator for ios-simulator (simulator", sim.stdout)
            second = self.job("screenshots", "i4", 12)
            self.assertEqual(second.returncode, 1, "one simulator job per mini")
            self.assertIn("the simulator token is taken", second.stdout)
            self.assertIn("(validate is compile)", self.job("validate", "i5", 12).stdout,
                          "validate is unknown to the hook (compile class): it stays on Blacksmith")
        finally:
            for runner in ("i0", "i1", "i2", "i3", "i4", "i5"):
                self.finish(runner)

    def test_capacity_compile_slots(self) -> None:
        self.fleet()
        slots = ("--compile-slots", "2", "--canonical-roots", "2")
        try:
            first = self.started("--capacity-units", "4", "--capacity-dir", os.fspath(self.dir / "capacity"), *slots,
                                 env={"GITHUB_JOB": "macos-compile-admission", "RUNNER_NAME": "c0"})
            self.assertIn("2/4 units+persistent-dd+root-1 for", first.stdout)
            second = self.started("--capacity-units", "4", "--capacity-dir", os.fspath(self.dir / "capacity"), *slots,
                                  env={"GITHUB_JOB": "macos-compile-admission", "RUNNER_NAME": "c1"})
            self.assertIn("2/4 units+persistent-dd-1+root-2 for", second.stdout)
            third = self.started("--capacity-units", "6", "--capacity-dir", os.fspath(self.dir / "capacity"), *slots,
                                 env={"GITHUB_JOB": "macos-compile-admission", "RUNNER_NAME": "c2"})
            self.assertIn("refused: capacity: all 2 persistent-dd tokens are taken", third.stdout)
        finally:
            for runner in ("c0", "c1"):
                self.finish(runner)

    def test_capacity_gui_token_and_unknown_jobs(self) -> None:
        self.fleet()
        try:
            self.assertEqual(self.job("tests-build-and-lag", "g0").returncode, 0)
            other = self.job("app-host-unit-tests", "g1", 4, None, "--gui-wait", "0")
            self.assertIn("refused: capacity: the gui token is taken", other.stdout)
            for n, lane in enumerate(("cli-pipe-regressions", "remote-daemon-macos-tests", "claude-wrapper")):
                side = self.job(lane, f"s{n}", units=8)
                self.assertIn(f"1/8 units for {lane} (light", side.stdout)
                self.finish(f"s{n}")
            self.assertIn("canonical root token is taken", self.job("future-unlisted-job", "u0").stdout)
            self.finish("g0")
            unknown = self.job("future-unlisted-job", "u0")
            self.assertIn("persistent-dd+root-1 for future-unlisted-job (compile", unknown.stdout)
        finally:
            for runner in ("g0", "u0"):
                self.finish(runner)

    def test_capacity_a_root_runner_listens_only_while_the_mini_fits_any_root_job(self) -> None:
        self.fleet()
        capacity = self.dir / "capacity"
        root = hook.RunnerScope(units=4, compile_slots=2, roots=2, root=True)
        side = hook.RunnerScope(units=4, compile_slots=2, roots=2, root=False)
        slots = ("--compile-slots", "2", "--canonical-roots", "2")
        try:
            first = self.job("macos-compile-admission", "c0", 4, None, *slots, "--instance", "0")
            self.assertEqual(first.returncode, 0, first.stdout + first.stderr)
            self.assertIsNone(hook.mini_full(capacity, 4, root), "2 units, pdd-1 and root-2 are free")
            shard = self.job("app-host-unit-tests", "g1", 4, None, *slots, "--instance", "1", "--gui-wait", "0")
            self.assertEqual(shard.returncode, 0, shard.stdout + shard.stderr)
            # 1 unit left: the next compile would be refused, so root runners stop; a light side job still fits
            self.assertEqual(hook.mini_full(capacity, 4, root),
                             "only 1 of 4 capacity units on this mini are free; a compile needs 2")
            self.assertIsNone(hook.mini_full(capacity, 4, side))
            self.assertIn("1 of 4 units free", self.job("macos-compile-admission", "c2", 4, None, *slots).stdout)
            self.finish("c0")
            # 3 units free while the shard holds the gui token: a compile fits, and a shard waits for the token
            self.assertIsNone(hook.mini_full(capacity, 4, root))
            self.assertIsNone(hook.mini_full(capacity, 4, side))
            self.finish("g1")
            self.assertIsNone(hook.mini_full(capacity, 4, root))
            for n in range(2):  # both compile slots: every root and persistent-dd token is taken
                both = self.job("macos-compile-admission", f"c{n}", 8, None, *slots, "--instance", str(n))
                self.assertEqual(both.returncode, 0, both.stdout + both.stderr)
            self.assertEqual(hook.mini_full(capacity, 8, root), "all 2 persistent-dd tokens on this mini are taken")
            self.assertIsNone(hook.mini_full(capacity, 8, side))
        finally:
            for runner in ("c0", "c1", "c2", "g1"):
                self.finish(runner)

    def idle_warm(self, *held: str) -> subprocess.Popen:
        """A stand-in glaeda-idle-warm: holds HELD under the capacity dir, names them in the holder file, and
        exits on SIGTERM, as the real one does."""
        capacity = self.dir / "capacity"
        capacity.mkdir(exist_ok=True)
        fake = make_executable(self.dir / "glaeda-idle-warm", f"""import fcntl, json, os, pathlib, signal, sys, time
capacity = pathlib.Path({os.fspath(capacity)!r})
fds = []
host = os.open(capacity.parent / "fleet" / "host.lock", os.O_RDONLY | os.O_CREAT)
fcntl.flock(host, fcntl.LOCK_SH | fcntl.LOCK_NB)  # as the real one does, like a job
for name in sys.argv[1:]:
    fd = os.open(capacity / (name if name.startswith("unit-") else name + ".token"), os.O_RDONLY | os.O_CREAT)
    fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
    fds.append(fd)
(capacity / "idle-warm.json").write_text(json.dumps({{"pid": os.getpid(), "held": sys.argv[1:]}}))
signal.signal(signal.SIGTERM, lambda *_: os._exit(143))
print("held", flush=True)
time.sleep(60)
""")
        warm = subprocess.Popen([sys.executable, os.fspath(fake), *held], stdout=subprocess.PIPE, text=True)
        self.assertEqual(warm.stdout.readline().strip(), "held")
        self.addCleanup(warm.kill)
        # launchd reaps the real one at once; a zombie would still look alive to the hook
        threading.Thread(target=warm.wait, daemon=True).start()
        return warm

    def test_capacity_a_job_stops_the_idle_catch_up_and_takes_its_locks(self) -> None:
        self.fleet()
        capacity = self.dir / "capacity"
        root = hook.RunnerScope(units=4, compile_slots=1, roots=1, root=True)
        warm = self.idle_warm("root-1", "persistent-dd", "unit-0", "unit-1")
        try:
            # what the catch-up holds counts as free, so the gate keeps the root runner listening
            self.assertIsNone(hook.mini_full(capacity, 4, root))
            first = self.job("macos-compile-admission", "y0")
            self.assertEqual(first.returncode, 0, first.stdout + first.stderr)
            self.assertIn("holding 2/4 units+persistent-dd+root-1", first.stdout)
            self.assertIn(f"stopped the idle catch-up (pid {warm.pid})", first.stdout)
            # SIGTERM, or SIGKILL once WARM_YIELD_S passed (a loaded machine may not run its handler in time)
            self.assertIn(warm.wait(timeout=10), (143, -signal.SIGKILL))
            # the job's own locks are not the catch-up's: the gate sees them as taken
            self.assertEqual(hook.mini_full(capacity, 4, root), "the persistent-dd token is taken")
        finally:
            self.finish("y0")

    def test_the_listener_gate_never_holds_a_runner_off_for_the_idle_catch_up(self) -> None:
        fleet = self.fleet()
        capacity = self.dir / "capacity"
        warm = self.idle_warm("root-1", "persistent-dd", "unit-0", "unit-1")
        # it holds the host lock shared, like a job: never a fleet build waiting for the host
        self.assertEqual(hook.lock_waiters(os.fspath(fleet / "host.lock"), set()), [])
        runner = self.dir / "actions-runner-glaeda"
        (runner / "glaeda-hooks").mkdir(parents=True)
        gate = hook.Gate(runner, os.fspath(fleet / "host.lock"), os.fspath(self.dir / "none.json"),
                         self.dir / "state", capacity_dir=capacity)
        self.assertIsNone(gate.shed(None))
        # a load or thermal hold stops the catch-up instead of holding the runner off
        self.assertIsNone(gate.shed("the mini is saturated"))
        self.assertIn(warm.wait(timeout=10), (143, -signal.SIGKILL))
        self.assertEqual(gate.shed("the mini is saturated"), "the mini is saturated", "no catch-up left to stop")
        # a fleet build waiting for the host lock would wait out the catch-up's shared hold: stop it
        again = self.idle_warm("root-1", "persistent-dd")
        gate.yield_to_fleet()
        self.assertIn(again.wait(timeout=10), (143, -signal.SIGKILL))

    def test_capacity_yielding_the_idle_catch_up_also_ends_its_build(self) -> None:
        capacity = self.dir / "capacity"
        capacity.mkdir()
        leader = make_executable(self.dir / "glaeda-idle-warm", f"""import json, os, pathlib, signal, subprocess, time
build = subprocess.Popen(["/bin/sleep", "60"])  # joins our process group
pathlib.Path({os.fspath(capacity)!r}, "idle-warm.json").write_text(json.dumps({{"pid": os.getpid(), "held": []}}))
signal.signal(signal.SIGTERM, lambda *_: os._exit(143))  # exits without ending its build
print(build.pid, flush=True)
time.sleep(60)
""")
        proc = subprocess.Popen([sys.executable, os.fspath(leader)], stdout=subprocess.PIPE, text=True,
                                start_new_session=True)
        self.addCleanup(proc.kill)
        orphan = int(proc.stdout.readline())
        self.addCleanup(lambda: subprocess.run(["/bin/kill", "-9", str(orphan)], capture_output=True))
        threading.Thread(target=proc.wait, daemon=True).start()
        self.assertIn("stopped the idle catch-up", hook.yield_idle_warm(capacity))
        deadline = time.monotonic() + 5
        state = "running"
        while time.monotonic() < deadline:
            state = subprocess.run(["/bin/ps", "-o", "stat=", "-p", str(orphan)], capture_output=True,
                                   text=True).stdout.strip()
            if not state or state.startswith("Z"):
                break
            time.sleep(0.1)
        self.assertTrue(not state or state.startswith("Z"), f"the build still runs: {state!r}")

    def test_capacity_a_stale_idle_catch_up_file_signals_nothing(self) -> None:
        self.fleet()
        capacity = self.dir / "capacity"
        capacity.mkdir()
        other = subprocess.Popen(["/bin/sleep", "60"])
        self.addCleanup(other.kill)
        (capacity / "idle-warm.json").write_text(json.dumps({"pid": other.pid, "held": ["root-1", "persistent-dd"]}))
        try:
            self.assertIsNone(hook.idle_warm(capacity), "a reused pid that is not glaeda-idle-warm")
            first = self.job("macos-compile-admission", "y1")
            self.assertEqual(first.returncode, 0, first.stdout + first.stderr)
            self.assertNotIn("idle catch-up", first.stdout)
            self.assertIsNone(other.poll(), "never signalled")
            for junk in ("{", "[]", json.dumps({"pid": True, "held": []}), json.dumps({"pid": 1, "held": []})):
                (capacity / "idle-warm.json").write_text(junk)
                self.assertIsNone(hook.idle_warm(capacity))
        finally:
            self.finish("y1")

    def test_capacity_a_full_mini_is_seen_by_the_listener_gate(self) -> None:
        self.fleet()
        capacity = self.dir / "capacity"
        runners = [f"f{n}" for n in range(5)]
        try:
            self.assertEqual(self.job("swift-package-tests", "f0", 4).returncode, 0)
            self.assertIsNone(hook.mini_full(capacity, 4), "3 of 4 units are still free")
            for runner in runners[1:4]:
                self.assertEqual(self.job("swift-package-tests", runner, 4).returncode, 0)
            self.assertEqual(hook.mini_full(capacity, 4), "all 4 capacity units on this mini are taken")
            # the probe took and gave back nothing: the holders keep every unit, and a job is still refused
            refused = self.job("swift-package-tests", "f4", 4)
            self.assertIn("refused: capacity: 0 of 4 units free", refused.stdout)
            self.finish("f2")
            self.assertIsNone(hook.mini_full(capacity, 4))
        finally:
            for runner in runners:
                self.finish(runner)

    def test_capacity_gui_job_waits_for_the_gui_token(self) -> None:
        self.fleet()
        try:
            self.assertEqual(self.job("app-host-unit-tests", "w0").returncode, 0)
            waited = time.monotonic()
            busy = self.job("tests-build-and-lag", "w1", 4, None, "--gui-wait", "3")
            self.assertEqual(busy.returncode, 1, busy.stdout)
            self.assertIn("refused: capacity: the gui token is taken", busy.stdout)
            self.assertGreaterEqual(time.monotonic() - waited, 3, "a gui job waits for the token before refusal")
            # the holder finishes while the next one waits: it is admitted, not refused
            release = threading.Timer(2.0, self.finish, args=("w0",))
            release.start()
            try:
                admitted = self.job("app-host-unit-tests", "w2", 4, None, "--gui-wait", "30")
            finally:
                release.join()
            self.assertEqual(admitted.returncode, 0, admitted.stdout)
            self.assertIn("+gui", admitted.stdout)
            # the wait never overshoots its deadline by a poll interval
            waited = time.monotonic()
            self.job("app-host-unit-tests", "w5", 4, None, "--gui-wait", "1")
            self.assertLess(time.monotonic() - waited, 4.5, "the last try starts by the deadline")
            # only the gui token is waited for: any other refusal is still immediate
            self.finish("w2")
            self.assertEqual(self.job("macos-compile-admission", "w3", 4).returncode, 0)
            waited = time.monotonic()
            full = self.job("cli-product-tests", "w4", 4, None, "--gui-wait", "30")
            self.assertIn("refused: capacity:", full.stdout)
            self.assertNotIn("gui token", full.stdout)
            self.assertLess(time.monotonic() - waited, 20, "a units refusal does not wait")
        finally:
            for runner in ("w0", "w1", "w2", "w3", "w4", "w5"):
                self.finish(runner)

    def test_capacity_e2e_build_takes_the_gui_token_for_its_tests_only(self) -> None:
        # test-e2e's build compiles, then takes the gui token itself (take-gui) before its console-session tests
        self.fleet()
        e2e = {"GITHUB_WORKFLOW_REF": "manaflow-ai/cmux/.github/workflows/test-e2e.yml@refs/heads/main"}
        two = ("--canonical-roots", "2", "--compile-slots", "2")
        env_file = self.dir / "github_env"
        state = self.dir / "state"
        try:
            build = self.job("build", "e0", 8, None, *two, env={**e2e, "GITHUB_ENV": os.fspath(env_file)})
            self.assertEqual(build.returncode, 0, build.stdout)
            self.assertIn("holding 2/8 units+root-1 for build (compile-gui", build.stdout)
            self.assertNotIn("gui+", build.stdout)
            self.assertNotIn("persistent-dd", build.stdout)
            self.assertFalse((state / "host-lock-holder-e0.gui").exists())
            self.assertEqual(env_file.read_text(), "CMUX_CI_CANONICAL_ROOT=/private/tmp/cmux-ci\n")
            # its own restore step re-takes the root it holds: a no-op
            self.assertEqual(self.take("/private/tmp/cmux-ci", "e0").returncode, 0)
            # while it compiles, a GUI job runs beside it, and so does a compile admission in the other root
            shard = self.job("app-host-unit-tests", "g0", 8, None, *two, "--gui-wait", "0")
            self.assertIn("+gui for app-host-unit-tests (gui", shard.stdout)
            self.assertIn("persistent-dd+root-2", self.job("macos-compile-admission", "e1", 8, None, *two).stdout)
            # the build's tests wait for that shard's token, then take it
            self.assertEqual(self.take_gui("e0", "--wait", "1").returncode, 1, "the shard still holds it")
            self.finish("g0")
            self.assertEqual(self.take_gui("e0", "--wait", "5").returncode, 0)
            refused = self.job("tests-build-and-lag", "g1", 8, None, *two, "--gui-wait", "0")
            self.assertIn("refused: capacity: the gui token is taken", refused.stdout)
            self.assertIn("the gui token holder released", self.finish("e0"))
            # with root 1 free, a build on the second root runner still takes it: root 1's seeds and caches
            # are the ones main publishes, and it keeps no per-root state to prefer its own root for
            first = self.job("build", "e3", 8, None, *two, "--instance", "1", env=e2e)
            self.assertIn("units+root-1 for build (compile-gui", first.stdout)
            self.finish("e3")
            # the fallback test job is a consumer: gui at job start, the producer's root from its restore step
            test = self.job("test", "e2", 8, None, *two, env=e2e)
            self.assertEqual(test.returncode, 0, test.stdout)
            self.assertIn("+gui for test (gui", test.stdout)
            self.assertNotIn("root-", test.stdout.split("holding", 1)[1])
            self.assertEqual(self.take("/private/tmp/cmux-ci-2", "e2", "--wait", "0").returncode, 1,
                             "the compile admission still holds root 2")
            self.assertEqual(self.take("/private/tmp/cmux-ci", "e2").returncode, 0)
        finally:
            for runner in ("e0", "e1", "e2", "e3", "g0", "g1"):
                self.finish(runner)
        self.assertTrue(self.lock_free())

    def test_capacity_gui_wait_stops_once_the_refusal_is_not_the_gui_token(self) -> None:
        self.fleet()
        release = None
        try:
            self.assertEqual(self.job("app-host-unit-tests", "x0", 4).returncode, 0)
            self.assertEqual(self.job("claude-wrapper", "x1", 4).returncode, 0)
            self.assertEqual(self.job("claude-wrapper", "x2", 4).returncode, 0)
            self.assertEqual(self.job("claude-wrapper", "x5", 4).returncode, 0)
            # the gui holder leaves, but the units it frees go to a light job first: the refusal turns to units
            def swap() -> None:
                self.finish("x0")
                self.job("claude-wrapper", "x3", 4)
            release = threading.Timer(2.0, swap)
            release.start()
            waited = time.monotonic()
            result = self.job("tests-build-and-lag", "x4", 4, None, "--gui-wait", "30")
            release.join()
            self.assertEqual(result.returncode, 1, result.stdout)
            self.assertIn("units free", result.stdout)
            self.assertLess(time.monotonic() - waited, 20, "a units refusal after a gui one does not keep waiting")
        finally:
            if release is not None:
                release.cancel()
            for runner in ("x0", "x1", "x2", "x3", "x4", "x5"):
                self.finish(runner)

    def test_job_class_keys_on_workflow_file_and_job_id(self) -> None:
        wf = hook.workflow_file
        self.assertEqual(wf("manaflow-ai/cmux/.github/workflows/cmux-tui.yml@refs/pull/1/merge"), "cmux-tui.yml")
        self.assertEqual(wf("manaflow-ai/cmux/.github/workflows/test-e2e.yml@refs/heads/main"), "test-e2e.yml")
        for bad in (None, "", "cmux-tui.yml", "manaflow-ai/cmux/cmux-tui.yml@main"):
            self.assertEqual(wf(bad), "")
        home = "manaflow-ai/cmux"
        for job in ("lint", "test", "build"):
            self.assertEqual(hook.job_class(job, home, home, "cmux-tui.yml"), ("isolated", False))
            # the same id in a workflow the table does not name keeps the unknown-job default: compile, pinned
            self.assertEqual(hook.job_class(job, home, home, "other.yml"), ("compile", True))
            self.assertEqual(hook.job_class(job, home, home), ("compile", True))
        # test-e2e runs its tests in the console session: build compiles and tests, test is the fallback
        self.assertEqual(hook.job_class("build", home, home, "test-e2e.yml"), ("compile-gui", False))
        self.assertEqual(hook.job_class("test", home, home, "test-e2e.yml"), ("gui", False))
        self.assertEqual(hook.job_class("lint", home, home, "test-e2e.yml"), ("compile", True))
        self.assertEqual(hook.job_class("build", "someone/else", home, "test-e2e.yml"), ("isolated", False))
        self.assertEqual(hook.CLASS_COST["compile-gui"], (2, ("root",)),
                         "no persistent-dd: the E2E build never writes compile admission's kept DerivedData")
        self.assertNotIn("compile-gui", hook.ROOT_CONSUMERS, "a producer takes a token-chosen root")
        for klass in {*hook.JOB_CLASSES.values(), *hook.WORKFLOW_JOB_CLASSES.values()}:
            self.assertIn(klass, hook.CLASS_COST)
        # a seed job holds the root its matrix names itself (take-root), so no token picks one for it
        self.assertEqual(hook.job_class("seed", home, home, "seed-derived-data.yml"), ("seed", False))
        self.assertEqual(hook.CLASS_COST["seed"], (2, ()))
        self.assertEqual(hook.job_class("seed", home, home, "other.yml"), ("compile", True))
        # the nightly app build compiles in its own workspace: no root a seed on the trusted mini waits for
        self.assertEqual(hook.job_class("build-nightly-app", home, home, "nightly.yml"), ("isolated", False))
        self.assertEqual(hook.job_class("build-nightly-app", home, home, "other.yml"), ("compile", True))
        self.assertEqual(hook.job_class("rerun", home, home, "app-host-test-rerun.yml"), ("gui", False))
        self.assertEqual(hook.job_class("rerun", home, home, "other.yml"), ("compile", True))
        self.assertEqual(hook.job_class("release-build", home, home, "ci.yml"), ("isolated", False))
        self.assertEqual(hook.job_class("macos-compile-admission", home, home, "ci.yml"), ("compile", False))
        self.assertEqual(hook.job_class("lint", "someone/else", home, "cmux-tui.yml"), ("isolated", False),
                         "a guest keeps the guest rule")

    def test_capacity_guest_repo_jobs_never_take_cmux_roots(self) -> None:
        self.fleet()
        env = {"GITHUB_REPOSITORY": "manaflow-ai/newapp"}

        def guest(name: str, runner: str) -> subprocess.CompletedProcess:
            return self.job(name, runner, 12, None, "--allowed-repo", "manaflow-ai/newapp", env=env)
        try:
            self.assertIn("persistent-dd+root-1", self.job("macos-compile-admission", "h0", 12,
                                                           env={"GITHUB_REPOSITORY": "manaflow-ai/cmux"}).stdout)
            build = guest("build", "n0")
            self.assertEqual(build.returncode, 0, build.stdout)
            self.assertIn("holding 2/12 units for build (isolated", build.stdout)
            # a guest id that cmux also uses gets the guest cost, not cmux's compile class
            same = guest("macos-compile-admission", "n1")
            self.assertIn("(isolated", same.stdout)
            self.assertNotIn("root-", same.stdout)
            self.assertIn("1/12 units for lint-light (light", guest("lint-light", "n2").stdout)
            self.assertIn("+simulator for ui-tests-sim (simulator", guest("ui-tests-sim", "n3").stdout)
            waited = time.monotonic()
            busy = self.job("ios-simulator", "n4", 12, None, "--allowed-repo", "manaflow-ai/newapp",
                            "--guest-wait", "3", env=env)
            self.assertIn("simulator token is taken", busy.stdout)
            self.assertGreaterEqual(time.monotonic() - waited, 3, "a guest waits for room before it is refused")
        finally:
            for runner in ("h0", "n0", "n1", "n2", "n3", "n4"):
                self.finish(runner)

    def test_capacity_refuses_while_a_fleet_build_holds_the_host(self) -> None:
        fleet = self.fleet()
        holder = subprocess.Popen([sys.executable, "-c",
                                   "import fcntl,os,time\n"
                                   f"fd=os.open({os.fspath(fleet / 'host.lock')!r},os.O_RDWR)\n"
                                   "fcntl.flock(fd,fcntl.LOCK_EX)\nprint('held',flush=True)\ntime.sleep(30)\n"],
                                  stdout=subprocess.PIPE, text=True)
        try:
            self.assertEqual(holder.stdout.readline().strip(), "held")
            result = self.job("cli-product-tests", "w0")
            self.assertEqual(result.returncode, 1)
            self.assertIn("refused: capacity: a fleet build holds the host lock", result.stdout)
        finally:
            holder.kill()
            holder.wait()
            holder.stdout.close()

    @unittest.skipUnless(os.path.exists(hook.LSOF), "needs lsof")
    def test_capacity_stops_admitting_while_a_fleet_build_waits(self) -> None:
        fleet = self.fleet()
        try:
            self.assertEqual(self.job("swift-package-tests", "l0").returncode, 0)
            waiter = subprocess.Popen([sys.executable, "-c",
                                       "import fcntl,os\n"
                                       f"fd=os.open({os.fspath(fleet / 'host.lock')!r},os.O_RDWR)\n"
                                       "print('waiting',flush=True)\nfcntl.flock(fd,fcntl.LOCK_EX)\n"],
                                      stdout=subprocess.PIPE, text=True)
            try:
                self.assertEqual(waiter.stdout.readline().strip(), "waiting")
                time.sleep(0.5)
                result = self.job("swift-package-tests", "l1")
                self.assertEqual(result.returncode, 1, result.stdout)
                self.assertIn("refused: capacity: a fleet build is waiting for the host", result.stdout)
                self.finish("l0")
                self.assertEqual(waiter.wait(timeout=10), 0, "the build worker gets the host once our job ends")
            finally:
                waiter.kill()
                waiter.wait()
                waiter.stdout.close()
        finally:
            self.finish("l0")

    def test_capacity_a_releasing_holder_is_not_a_waiter(self) -> None:
        self.fleet()
        state = self.dir / "state"
        state.mkdir()
        (state / "host-lock-holder-gone.pid.releasing").write_text("4242\n")
        (state / "host-lock-holder-live.pid").write_text("4343\n")
        self.assertEqual(hook.holder_pids(state), {os.getpid(), 4242, 4343})
        try:  # job-completed on one runner while another admits: never "a fleet build is waiting"
            self.assertEqual(self.job("swift-package-tests", "a0").returncode, 0)
            for n in range(3):
                self.finish("a0")
                self.assertEqual(self.job("swift-package-tests", "a0").returncode, 0)
                result = self.job("swift-package-tests", f"b{n}")
                self.assertEqual(result.returncode, 0, result.stdout)
                self.finish(f"b{n}")
        finally:
            self.finish("a0")
        self.assertTrue(self.lock_free())
        self.assertEqual(sorted(p.name for p in state.glob("*.releasing")), ["host-lock-holder-gone.pid.releasing"])

    def test_capacity_toolchain_gate_requires_gh(self) -> None:
        self.fleet()
        self.node(gh=False)
        result = self.eligible_start()
        self.assertEqual(result.returncode, 1)
        self.assertIn("gh is not on the job PATH", result.stdout)
        self.assertTrue(self.lock_free())

    def test_no_fleet_lock_file_admits(self) -> None:
        result = self.started()
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertIn("no fleet host lock", result.stdout)

    # ------------------------------------------------------------ eligibility gate

    COMMIT = "3809eed51fdd9f1464452f2cd37dbf3148a831fd"
    TOOLCHAIN = {"rustcVersion": "rustc 1.98.1 (48a229cea 2026-09-01)", "cargoVersion": "cargo 1.98.1 (797e8a9bc 2026-08-05)",
                 "zigVersion": "0.16.0", "xcodeVersion": "26.6", "xcodeBuild": "17F113", "macosSdkVersion": "26.5"}

    def node(self, state: str = "eligible", routing: bool = True, receipt_sha: str = "sha256:aa",
             enrolled_sha: str = "sha256:aa", tools: dict | None = None, generation: bool = True,
             valid: bool = True, role: str = "cmux_macos_native_build", receipt_toolchain: dict | None = None,
             hang: str | None = None, default: str = "1.98.1-aarch64-apple-darwin",
             installed: tuple = ("1.88.0-aarch64-apple-darwin", "1.98.1-aarch64-apple-darwin"),
             python: str = "3.13", gh: bool = True) -> None:
        """A fake Glaeda node under HOME (self.dir): enrollment, class receipt, staged CLI, toolchain."""
        config = self.dir / ".config/glaeda/cmux-fleet"
        (config / "class-acceptance").mkdir(parents=True, exist_ok=True)
        (config / "acceptance").mkdir(exist_ok=True)
        (config / "enrollment.json").write_text(json.dumps({"state": state, "classAcceptanceSha256": enrolled_sha}))
        (config / "acceptance/cmux_macos_native_build.json").write_text("{}")
        (config / "class-acceptance/m4pro-48.json").write_text(json.dumps(
            {"receiptSha256": receipt_sha, "glaedaCandidate": {"commit": self.COMMIT},
             "toolchain": self.TOOLCHAIN if receipt_toolchain is None else receipt_toolchain}))
        gen = self.dir / "Projects/glaeda-generations" / self.COMMIT[:12] / "scripts"
        if generation:
            gen.mkdir(parents=True, exist_ok=True)
            status = {"schema": "glaeda-cmux-fleet-node-status/v1", "state": state, "routingCandidateEligible": routing,
                      "roles": [{"role": role, "eligible": routing,
                                 "reason": "ok" if routing else "acceptance_missing_or_rejected"}]}
            (gen / "cmux_fleet.py").write_text(
                "import json\n"
                f"def validate_class_acceptance(doc):\n    if not {valid!r}:\n        raise ValueError('digest')\n"
                "    return doc\n"
                f"if __name__ == '__main__':\n    print(json.dumps({status!r}))\n")
        cargo = self.dir / "jobpath"
        cargo.mkdir(parents=True, exist_ok=True)
        have = {**self.TOOLCHAIN, **(tools or {})}
        (self.dir / "rustup-default").write_text(default)
        (self.dir / "rustup-installed").write_text("\n".join(installed))
        versions = {"1.98.1-aarch64-apple-darwin": self.TOOLCHAIN["rustcVersion"],
                    "1.88.0-aarch64-apple-darwin": "rustc 1.88.0 (6b00bc388 2025-06-23)"}
        (self.dir / "rustc-by-toolchain.json").write_text(json.dumps(versions))
        state = os.fspath(self.dir)
        make_executable(cargo / "rustup", f"""#!{sys.executable}
import json, sys
state = {state!r}
default = open(state + "/rustup-default").read().strip()
installed = open(state + "/rustup-installed").read().split()
args = sys.argv[1:]
if args[:2] == ["toolchain", "list"]:
    for name in installed:
        print(name + (" (active, default)" if name == default else ""))
elif args == ["default"]:
    print(default + " (default)")
elif args == ["show"]:
    print("Default host: aarch64-apple-darwin")
elif args[:1] == ["default"] and args[1] in installed:
    open(state + "/rustup-default", "w").write(args[1])
    print("info: default toolchain set to " + args[1])
elif args[:1] == ["run"]:
    print(json.load(open(state + "/rustc-by-toolchain.json")).get(args[1] + "-aarch64-apple-darwin", ""))
else:
    sys.exit(1)
""")
        rustc = have["rustcVersion"] if (tools or {}).get("rustcVersion") else None
        make_executable(cargo / "rustc", f"#!{sys.executable}\nimport json\n"
                        + (f"print({rustc!r})\n" if rustc else
                           f"import os\nprint(json.load(open({state!r} + '/rustc-by-toolchain.json'))"
                           f"[os.environ.get('RUSTUP_TOOLCHAIN') or open({state!r} + '/rustup-default').read().strip()])\n"))
        make_executable(cargo / "python3", f"#!/bin/sh\necho {python}\n")
        if gh:
            make_executable(cargo / "gh", "#!/bin/sh\necho 'gh version 2.101.0 (2026-09-15)'\n")
        else:  # shadows a gh the host has on /usr/bin (GitHub's hosted images do)
            make_executable(cargo / "gh", "#!/bin/sh\nexit 127\n")
        outputs = {"cargo": have["cargoVersion"], "zig": have["zigVersion"],
                   "xcrun": have["macosSdkVersion"],
                   "xcodebuild": f"Xcode {have['xcodeVersion']}\nBuild version {have['xcodeBuild']}"}
        for name, text in outputs.items():
            if text is None:
                (cargo / name).unlink(missing_ok=True)
                continue
            body = "sleep 60\n" if name == hang else f"cat <<'OUT'\n{text}\nOUT\n"
            make_executable(cargo / name, f"#!/bin/sh\n{body}")

    def eligible_start(self) -> subprocess.CompletedProcess:
        # The fake toolchain is on the job's PATH, which is the runner's PATH the hook inherits.
        return self.started("--require-eligible", "--fleet-class", "m4pro-48", "--toolchain-xcode",
                            "/Applications/Xcode_26.6.app", env={"PATH": f"{self.dir / 'jobpath'}:/usr/bin:/bin"})

    def done(self) -> None:
        self.run_hook("job-completed", None, None, "--no-disk", "--state-dir", os.fspath(self.dir / "state"))

    def test_the_receipts_source_node_is_admitted_without_adopting_it(self) -> None:
        self.fleet()
        self.node()
        config = self.dir / ".config/glaeda/cmux-fleet"
        receipt = json.loads((config / "class-acceptance/m4pro-48.json").read_text())
        receipt.update(fleetClass="m4pro-48", acceptingNodeId="cmux-mac-001", acceptingEnrollmentGeneration=2, glaedaGeneration="sha256:g",
                       toolchainGeneration="sha256:t", cmuxToolchainIdentity="sha256:i", cmuxSemanticResultSha256="sha256:s")
        (config / "class-acceptance/m4pro-48.json").write_text(json.dumps(receipt))
        enrollment = {"state": "eligible", "nodeId": "cmux-mac-001", "enrollmentGeneration": 2,
                      "glaedaGeneration": "sha256:g", "supportedToolchainGenerations": ["sha256:t"]}
        acceptance = {"nodeId": "cmux-mac-001", "enrollmentGeneration": 2, "toolchainGeneration": "sha256:t",
                      "cmuxToolchainIdentity": "sha256:i", "cmuxSemanticResultSha256": "sha256:s"}
        cases = {"source": ({}, {}, 0), "other-node": ({"nodeId": "cmux-mac-002"}, {}, 1),
                 "stale-generation": ({"enrollmentGeneration": 1}, {}, 1),
                 "other-toolchain": ({"supportedToolchainGenerations": ["sha256:x"]}, {}, 1),
                 "other-result": ({}, {"cmuxSemanticResultSha256": "sha256:z"}, 1),
                 "references-another-receipt": ({"classAcceptanceSha256": "sha256:bb"}, {}, 1),
                 "generations-not-a-list": ({"supportedToolchainGenerations": "sha256:t"}, {}, 1)}
        for name, (enroll_over, accept_over, code) in cases.items():
            with self.subTest(name):
                (config / "enrollment.json").write_text(json.dumps({**enrollment, **enroll_over}))
                (config / "acceptance/cmux_macos_native_build.json").write_text(json.dumps({**acceptance, **accept_over}))
                result = self.eligible_start()
                self.done()
                self.assertEqual(result.returncode, code, result.stdout)
                if code:
                    self.assertIn("does not reference the m4pro-48 class receipt", result.stdout)
        (config / "enrollment.json").write_text(json.dumps(enrollment))
        (config / "acceptance/cmux_macos_native_build.json").write_text(json.dumps(acceptance))
        (config / "class-acceptance/m4pro-48.json").write_text(json.dumps({**receipt, "fleetClass": "m4-16"}))
        self.assertEqual(self.eligible_start().returncode, 1, "a receipt for another class never admits")
        self.done()

    def test_eligible_node_on_its_toolchain_is_admitted(self) -> None:
        self.fleet()
        self.node()
        result = self.eligible_start()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.done()

    def test_ineligible_nodes_refuse(self) -> None:
        self.fleet()
        cases = {
            "enrolling": (dict(state="enrolling", routing=False), "state enrolling"),
            "routing-false": (dict(routing=False), "acceptance_missing_or_rejected"),
            "other-receipt": (dict(enrolled_sha="sha256:bb"), "does not reference the m4pro-48 class receipt"),
            "rustc-drift": (dict(tools={"rustcVersion": "rustc 1.99.0"}), "rustcVersion 'rustc 1.99.0'"),
            "xcode-drift": (dict(tools={"xcodeBuild": "17F999"}), "xcodeBuild '17F999' != '17F113'"),
            "zig-missing": (dict(tools={"zigVersion": None}), "zigVersion '' != '0.16.0'"),
            "no-generation": (dict(generation=False), "no node status from generation 3809eed51fdd"),
            "receipt-invalid": (dict(valid=False), "class receipt does not validate"),
            "wrong-role": (dict(role="some_other_role"), "some_other_role: ok"),
            "receipt-without-toolchain": (dict(receipt_toolchain={}), "records no rustcVersion"),
            "receipt-unknown-zig": (dict(receipt_toolchain={**self.TOOLCHAIN, "zigVersion": "unknown"}),
                                    "records no zigVersion"),
        }
        for name, (kwargs, text) in cases.items():
            with self.subTest(name):
                import shutil as _sh
                _sh.rmtree(self.dir / "Projects", ignore_errors=True)
                self.node(**kwargs)
                result = self.eligible_start()
                self.assertEqual(result.returncode, 1, result.stdout)
                self.assertIn("refused: node not eligible", result.stdout)
                self.assertIn(text, result.stdout)
                self.assertTrue(self.lock_free(), "a refused job never takes the host lock")

    def test_drifted_rustup_default_is_realigned_under_the_lock(self) -> None:
        self.fleet()
        self.node(default="1.88.0-aarch64-apple-darwin")  # another fleet job flipped the global default
        result = self.eligible_start()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("set the rustup default to 1.98.1-aarch64-apple-darwin (was 1.88.0-aarch64-apple-darwin)",
                      result.stdout)
        self.assertEqual((self.dir / "rustup-default").read_text(), "1.98.1-aarch64-apple-darwin")
        self.assertFalse(self.lock_free())
        self.done()

    def test_check_is_read_only_and_predicts_the_alignment(self) -> None:
        self.node(default="1.88.0-aarch64-apple-darwin")
        push = event(self.dir, "push", {"repository": CMUX})
        result = self.run_hook("check", "push", push, "--fleet-class", "m4pro-48", "--toolchain-xcode",
                               "/Applications/Xcode_26.6.app", env={"PATH": f"{self.dir / 'jobpath'}:/usr/bin:/bin"})
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertIn("eligible (would set the rustup default to 1.98.1-aarch64-apple-darwin)", result.stdout)
        self.assertEqual((self.dir / "rustup-default").read_text(), "1.88.0-aarch64-apple-darwin")
        self.assertFalse((self.dir / "state").exists())

    def test_old_python_on_the_job_path_refuses(self) -> None:
        self.fleet()
        self.node(python="3.9")
        result = self.eligible_start()
        self.assertEqual(result.returncode, 1)
        self.assertIn("python3 on the job PATH is 3.9", result.stdout)
        self.assertTrue(self.lock_free())

    def test_matching_stable_default_is_left_alone(self) -> None:
        self.fleet()
        self.node(default="stable-aarch64-apple-darwin", installed=("stable-aarch64-apple-darwin",))
        json_path = self.dir / "rustc-by-toolchain.json"
        versions = json.loads(json_path.read_text())
        versions["stable-aarch64-apple-darwin"] = self.TOOLCHAIN["rustcVersion"]
        json_path.write_text(json.dumps(versions))
        result = self.eligible_start()
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertNotIn("set the rustup default", result.stdout)
        self.assertEqual((self.dir / "rustup-default").read_text(), "stable-aarch64-apple-darwin")
        self.done()

    def test_toolchain_refusal_after_the_lock_releases_it(self) -> None:
        self.fleet()
        self.node(default="1.88.0-aarch64-apple-darwin", installed=("1.88.0-aarch64-apple-darwin",))
        result = self.eligible_start()
        self.assertEqual(result.returncode, 1)
        self.assertIn("rust toolchain 1.98.1-aarch64-apple-darwin is not installed", result.stdout)
        self.assertTrue(self.lock_free())
        self.assertEqual((self.dir / "rustup-default").read_text(), "1.88.0-aarch64-apple-darwin")

    def test_diff_sidecar_pin_is_checked(self) -> None:
        self.fleet()
        receipt = {**self.TOOLCHAIN, "diffRustToolchain": "1.88.0",
                   "diffRustcVersion": "rustc 1.88.0 (6b00bc388 2025-06-23)"}
        self.node(receipt_toolchain=receipt)
        self.assertEqual(self.eligible_start().returncode, 0)
        self.done()
        self.node(receipt_toolchain={**receipt, "diffRustcVersion": "rustc 1.88.1 (x)"})
        result = self.eligible_start()
        self.assertEqual(result.returncode, 1)
        self.assertIn("diffRustcVersion", result.stdout)
        self.assertTrue(self.lock_free())

    def test_hung_tool_refuses_within_the_budget(self) -> None:
        self.fleet()
        self.node(hang="xcodebuild")
        with mock.patch.dict(os.environ, {}):
            start = time.monotonic()
            result = self.started("--require-eligible", "--fleet-class", "m4pro-48",
                                  env={"PATH": f"{self.dir / 'jobpath'}:/usr/bin:/bin"})
        self.assertEqual(result.returncode, 1)
        self.assertIn("xcodeVersion '' != '26.6'", result.stdout)
        self.assertLess(time.monotonic() - start, 30)

    def test_no_enrollment_refuses(self) -> None:
        self.fleet()
        result = self.eligible_start()
        self.assertEqual(result.returncode, 1)
        self.assertIn("no Glaeda enrollment", result.stdout)

    def test_decide_is_pure_table(self) -> None:
        for name, (event_name, payload, admitted) in SAMPLE_EVENTS.items():
            with self.subTest(name):
                ok, _ = hook.decide(event_name, payload, (payload.get("repository") or {}).get("full_name"),
                                    ["manaflow-ai/cmux"], None)
                self.assertEqual(ok, admitted)


FAKE_LISTENER = """#!/usr/bin/env python3
import signal, sys, time
signal.signal(signal.SIGINT, lambda *_: sys.exit(0))
print("Listening for Jobs", flush=True)
while True:
    time.sleep(0.05)
"""


class GateTest(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)
        self.lock = self.tmp / "host.lock"
        self.lock.touch()
        self.state = self.tmp / "state"
        self.runner = self.tmp / "actions-runner-glaeda"
        (self.runner / "bin").mkdir(parents=True)
        listener = self.runner / "bin/Runner.Listener"
        listener.write_text(FAKE_LISTENER)
        listener.chmod(0o755)
        # The real layers: run.sh runs run-helper.sh, which runs the listener and passes its code back.
        helper = self.runner / "run-helper.sh"
        helper.write_text(f"#!/bin/bash\n{sys.executable} {listener}\nexit $?\n")
        helper.chmod(0o755)
        run_sh = self.runner / "run.sh"
        run_sh.write_text(f"#!/bin/bash\n{helper}\nexit 0\n")
        run_sh.chmod(0o755)
        self.addCleanup(self.reap)
        # A cool mini unless a test says otherwise (the real reading depends on the machine running the test).
        self.level: int | None = 0
        patcher = mock.patch.object(hook, "thermal_pressure_level", side_effect=lambda: self.level)
        patcher.start()
        self.addCleanup(patcher.stop)

    def reap(self) -> None:
        with contextlib.suppress(Exception):
            table = hook.processes()
        for pid, (_, command) in (locals().get("table") or {}).items():
            if os.fspath(self.runner) in command:
                with contextlib.suppress(OSError):
                    os.kill(pid, 9)

    def listeners(self) -> list[int]:
        return [pid for pid, (_, cmd) in hook.processes().items()
                if os.fspath(self.runner / "bin/Runner.Listener") in cmd]

    def held(self) -> str | None:
        return hook.host_held(os.fspath(self.lock), os.fspath(self.tmp / "none.json"), time.time())

    def hold(self, mode: int) -> int:
        fd = os.open(self.lock, os.O_RDONLY)
        fcntl.flock(fd, mode | fcntl.LOCK_NB)
        self.addCleanup(os.close, fd)
        return fd

    def opener(self, name: str) -> subprocess.Popen:
        """A process with the host lock open, whose argv carries `name`."""
        script = self.tmp / name
        script.write_text(f"import time\nf = open({os.fspath(self.lock)!r})\nprint('open', flush=True)\n"
                          "time.sleep(60)\n")
        proc = subprocess.Popen([sys.executable, os.fspath(script)], stdout=subprocess.PIPE)
        self.addCleanup(proc.kill)
        proc.stdout.readline()
        return proc

    def test_a_free_host_or_our_shared_jobs_leave_the_listener_on(self) -> None:
        self.assertIsNone(self.held())
        self.hold(fcntl.LOCK_SH)  # a PR job's holder
        self.assertIsNone(self.held())

    def test_an_exclusive_holder_claims_the_host(self) -> None:
        self.hold(fcntl.LOCK_EX)
        self.assertEqual(self.held(), "a fleet build holds the host lock")

    def test_a_reservation_claims_the_host(self) -> None:
        with mock.patch.object(hook, "reservation_refusal", return_value="host reserved by leo"):
            self.assertEqual(self.held(), "host reserved by leo")

    def test_real_lsof_sees_a_fleet_waiter_but_not_this_hook_in_flight(self) -> None:
        fleet = self.opener("with-host-lock.py")
        ours = self.opener("glaeda-cmux-runner-hook-job-started.py")  # another runner's admission
        self.assertEqual(hook.host_waiters(os.fspath(self.lock), self.state), {fleet.pid})
        real = subprocess.run
        def gone(cmd, **kw):  # the fleet pid exits between lsof and ps: ps lists only the hook's own pid
            if cmd[0] == "/bin/ps":
                cmd = [*cmd[:-1], str(ours.pid)]
            return real(cmd, **kw)
        with mock.patch.object(hook.subprocess, "run", side_effect=gone):
            self.assertEqual(hook.host_waiters(os.fspath(self.lock), self.state), set())
        fleet.kill(); fleet.wait()
        self.assertEqual(hook.host_waiters(os.fspath(self.lock), self.state), set())
        self.assertIsNone(ours.poll())

    def gate(self, claims: list, busy: list | None = None):
        gate = hook.Gate(self.runner, os.fspath(self.lock), "", self.state)
        gate.child = mock.Mock(pid=1)
        gate.child.poll.return_value = None
        seq = iter(claims)
        gate.claimed = lambda: next(seq)
        gate.confirmed = lambda: "held"
        gate.reload = lambda: None
        busy_seq = iter(busy or [])
        gate.busy = lambda: next(busy_seq, False)
        stops: list[str] = []
        gate.stop = lambda why: (stops.append(why), setattr(gate, "stop_deadline", time.monotonic() + 60),
                                 setattr(gate, "held", why))
        return gate, stops

    def units(self, held: int) -> Path:
        """A capacity ledger an admission has used, with its first `held` units taken."""
        capacity = Path(tempfile.mkdtemp(dir=self.tmp))
        (capacity / "admission.lock").touch()  # every admission creates it
        for slot in range(held):
            fd = os.open(capacity / f"unit-{slot}", os.O_RDONLY | os.O_CREAT, 0o644)
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            self.addCleanup(os.close, fd)
        return capacity

    def runner_hook(self, units: int | None) -> None:
        """This runner's job-started hook as glaeda-cmux-runner writes it, with or without --capacity-units."""
        script = self.runner / hook.RUNNER_HOOK_SCRIPT
        script.parent.mkdir(exist_ok=True)
        scope = "--allowed-owner manaflow-ai" + (f" --capacity-units {units}" if units is not None else "")
        script.write_text(f"#!/bin/bash\nexec /usr/bin/python3 /hook job-started {scope}\n")

    def full_gate(self, capacity: Path):
        return hook.Gate(self.runner, os.fspath(self.lock), os.fspath(self.tmp / "none.json"), self.state,
                         capacity_dir=capacity)

    def test_a_mini_with_every_unit_taken_claims_the_host(self) -> None:
        self.assertIsNone(hook.mini_full(None, 2))
        self.assertIsNone(hook.mini_full(self.units(2), 0), "a single-runner mini has no units")
        self.assertIsNone(hook.mini_full(self.units(2), 3))
        self.assertIsNone(hook.mini_full(self.tmp / "unused", 2), "no admission yet: nothing is taken")
        capacity = self.units(2)
        self.assertEqual(hook.mini_full(capacity, 2), "all 2 capacity units on this mini are taken")
        gate = self.full_gate(capacity)
        self.assertIsNone(gate.claimed(), "a runner without a hook script never counts as full")
        self.runner_hook(None)
        self.assertIsNone(gate.claimed(), "nor one whose hook takes no units")
        self.runner_hook(2)
        self.assertEqual(hook.runner_units(self.runner), 2)
        self.assertEqual(gate.claimed(), "all 2 capacity units on this mini are taken")
        self.assertEqual(gate.confirmed(), "all 2 capacity units on this mini are taken")
        # a re-apply that raised this runner's units: the new free unit counts on the next look
        self.runner_hook(4)
        self.assertIsNone(gate.claimed())
        self.runner_hook(2)
        self.hold(fcntl.LOCK_EX)
        self.assertEqual(gate.claimed(), "a fleet build holds the host lock", "the fleet's claim comes first")

    def test_a_saturated_mini_claims_the_host_with_hysteresis(self) -> None:
        pause, resume = 2.0, 1.5
        for name, value in (("GATE_LOAD_PAUSE", pause), ("GATE_LOAD_RESUME", resume)):
            patcher = mock.patch.object(hook, name, value)
            patcher.start()
            self.addCleanup(patcher.stop)
        self.assertIsNone(hook.mini_saturated(False, load=pause * 14 - 0.1, cores=14))
        why = hook.mini_saturated(False, load=pause * 14, cores=14)
        self.assertTrue(why and why.startswith(hook.SATURATED), why)
        self.assertEqual(why, hook.mini_saturated(False, load=pause * 14 + 30, cores=14), "stable text, logged once")
        self.assertIsNotNone(hook.mini_saturated(True, load=resume * 14, cores=14))
        self.assertIsNone(hook.mini_saturated(True, load=resume * 14 - 0.1, cores=14))
        self.assertIsNone(hook.mini_saturated(False, load=99, cores=0), "no core count: never saturated")
        with mock.patch.object(hook.os, "getloadavg", side_effect=OSError):
            self.assertIsNone(hook.mini_saturated(False), "an unreadable load never saturates")

        load = [pause * 14]
        patcher = mock.patch.object(hook.os, "getloadavg", side_effect=lambda: (load[0], 0.0, 0.0))
        patcher.start()
        self.addCleanup(patcher.stop)
        patcher = mock.patch.object(hook.os, "cpu_count", return_value=14)
        patcher.start()
        self.addCleanup(patcher.stop)
        # Full and saturated: the load reason wins, so the resume mark applies.
        self.runner_hook(2)
        gate = self.full_gate(self.units(2))
        self.assertTrue(gate.claimed().startswith(hook.SATURATED))
        self.assertTrue(gate.confirmed().startswith(hook.SATURATED))
        load[0] = (pause + resume) / 2 * 14
        self.assertTrue(gate.claimed().startswith(hook.SATURATED), "between the marks a held gate stays held")
        load[0] = resume * 14 - 1
        self.assertEqual(gate.claimed(), "all 2 capacity units on this mini are taken", "then the full reason")
        self.hold(fcntl.LOCK_EX)
        load[0] = pause * 14
        self.assertEqual(gate.claimed(), "a fleet build holds the host lock", "the fleet's claim comes first")

    def test_gate_lines_carry_a_utc_stamp_readers_still_match(self) -> None:
        out = io.StringIO()
        with mock.patch.object(hook.time, "gmtime", return_value=time.gmtime(1790379071)), \
                contextlib.redirect_stdout(out):
            hook.gate_log("holding the listener off: all 4 capacity units on this mini are taken")
        line = out.getvalue()
        self.assertEqual(line, "2026-09-25 23:31:11Z: glaeda-cmux-runner-gate: holding the listener off: "
                               "all 4 capacity units on this mini are taken\n")
        # glaeda-cmux-runner reads the held state by substring, so the stamp keeps it working
        self.assertIn(cr.GATE_HELD, line)

    def test_a_load_hold_ends_after_the_limit_until_the_load_falls(self) -> None:
        for name, value in (("GATE_LOAD_PAUSE", 2.0), ("GATE_LOAD_RESUME", 1.5), ("GATE_LOAD_MAX_HOLD_S", 60.0)):
            patcher = mock.patch.object(hook, name, value)
            patcher.start()
            self.addCleanup(patcher.stop)
        load, clock = [40.0], [1000.0]
        gate = hook.Gate(self.runner, os.fspath(self.lock), os.fspath(self.tmp / "none.json"), self.state)
        with mock.patch.object(hook.os, "getloadavg", side_effect=lambda: (load[0], 0.0, 0.0)), \
                mock.patch.object(hook.os, "cpu_count", return_value=14), \
                mock.patch.object(hook.time, "monotonic", side_effect=lambda: clock[0]), \
                mock.patch.object(hook, "gate_log") as log:
            self.assertIsNotNone(gate.claimed())
            clock[0] += 59
            self.assertIsNotNone(gate.claimed())
            clock[0] += 2
            self.assertIsNone(gate.claimed(), "past the limit the mini listens anyway")
            self.assertIn("longer than any compile", log.call_args[0][0])
            clock[0] += 600
            self.assertIsNone(gate.claimed(), "still waived while the load stays up")
            load[0] = 10.0
            self.assertIsNone(gate.claimed())
            load[0] = 40.0
            self.assertIsNotNone(gate.claimed(), "a fresh overload holds again")
            # A fleet claim in between restarts the clock: the limit times a load pause, not the fleet's.
            clock[0] += 50
            fd = self.hold(fcntl.LOCK_EX)
            self.assertEqual(gate.claimed(), "a fleet build holds the host lock")
            fcntl.flock(fd, fcntl.LOCK_UN)
            clock[0] += 50
            self.assertIsNotNone(gate.claimed(), "the 100 s since the first look are not all load hold")
            self.assertIsNotNone(gate.claimed())

    def test_a_saturated_mini_stops_an_idle_listener_through_step(self) -> None:
        for name, value in (("GATE_LOAD_PAUSE", 2.0), ("GATE_LOAD_RESUME", 1.5)):
            patcher = mock.patch.object(hook, name, value)
            patcher.start()
            self.addCleanup(patcher.stop)
        load = [40.0]
        gate = hook.Gate(self.runner, os.fspath(self.lock), os.fspath(self.tmp / "none.json"), self.state)
        with mock.patch.object(hook.os, "getloadavg", side_effect=lambda: (load[0], 0.0, 0.0)), \
                mock.patch.object(hook.os, "cpu_count", return_value=14), \
                mock.patch.object(gate, "start") as start, mock.patch.object(gate, "stop") as stop, \
                mock.patch.object(gate, "busy", return_value=False), mock.patch.object(gate, "reload"):
            gate.child = mock.Mock(poll=mock.Mock(return_value=None))
            gate.step()
            gate.step()
            stop.assert_called_once()
            self.assertTrue(stop.call_args[0][0].startswith(hook.SATURATED))
            gate.child, gate.held = None, stop.call_args[0][0]
            load[0] = 25.0  # below pause, above resume: stays off
            for _ in range(3):
                gate.step()
            start.assert_not_called()
            load[0] = 20.0
            for _ in range(hook.GATE_CONFIRM):
                gate.step()
            start.assert_called_once()

    def test_the_job_record_class_uses_the_workflow_like_admission(self) -> None:
        payload = self.tmp / "event.json"
        payload.write_text(json.dumps({"workflow": ".github/workflows/test-e2e.yml"}))
        env = {"GITHUB_JOB": "build", "GITHUB_EVENT_PATH": os.fspath(payload)}
        with mock.patch.dict(os.environ, env):
            os.environ.pop("GITHUB_WORKFLOW_REF", None)
            self.assertEqual(hook.job_workflow(), "test-e2e.yml", "a dispatch payload names its workflow")
            self.assertEqual(hook.job_meta("manaflow-ai/cmux", hook.DEFAULT_HOME_REPO)["class"], "compile-gui")
            os.environ["GITHUB_WORKFLOW_REF"] = "manaflow-ai/cmux/.github/workflows/cmux-tui.yml@refs/heads/main"
            self.assertEqual(hook.job_meta("manaflow-ai/cmux", hook.DEFAULT_HOME_REPO)["class"], "isolated")
        payload.write_text(json.dumps({"pull_request": {}}))
        with mock.patch.dict(os.environ, env):
            os.environ.pop("GITHUB_WORKFLOW_REF", None)
            self.assertEqual(hook.job_workflow(), "")

    def test_runner_scope_reads_the_hook_and_the_runner_directory(self) -> None:
        self.assertEqual(hook.runner_scope(self.runner), hook.RunnerScope(), "no hook script: no units, no root")
        script = self.runner / hook.RUNNER_HOOK_SCRIPT
        script.parent.mkdir(exist_ok=True)
        script.write_text("exec python3 /hook job-started --capacity-units 5 --compile-slots 2 "
                          "--canonical-roots 2 --instance 1\n")
        self.assertEqual(hook.runner_scope(self.runner), hook.RunnerScope(5, 2, 2, True))
        script.write_text("exec python3 /hook job-started --capacity-units 4 --canonical-roots 2 --instance 3\n")
        self.assertEqual(hook.runner_scope(self.runner), hook.RunnerScope(4, 1, 2, False))
        # one root: the hook passes no --instance, so the directory suffix names it (actions-runner-glaeda-K)
        script.write_text("exec python3 /hook job-started --capacity-units 4\n")
        self.assertEqual(hook.runner_scope(self.runner), hook.RunnerScope(4, 1, 1, True))
        side = self.tmp / "actions-runner-glaeda-2"
        (side / "glaeda-hooks").mkdir(parents=True)
        (side / hook.RUNNER_HOOK_SCRIPT).write_text(script.read_text())
        self.assertEqual(hook.runner_scope(side), hook.RunnerScope(4, 1, 1, False))
        self.assertEqual(hook.runner_units(side), 4)
        script.write_text("exec python3 /hook job-started --capacity-units 5 --compile-slots 2 "
                          "--canonical-roots 2 --instance 4 --gui-runner\n")
        self.assertEqual(hook.runner_scope(self.runner), hook.RunnerScope(5, 2, 2, False, True))
        script.write_text("exec python3 /hook job-started --capacity-units 5 --instance 0 --gui-runner\n")
        self.assertEqual(hook.runner_scope(self.runner), hook.RunnerScope(5, 1, 1, False, True),
                         "a gui runner is never a root runner, whatever its instance")

    def test_a_gui_runner_holds_while_the_console_is_locked(self) -> None:
        script = self.runner / hook.RUNNER_HOOK_SCRIPT
        script.parent.mkdir(parents=True, exist_ok=True)
        script.write_text("exec python3 /hook job-started --capacity-units 5 --instance 2 --gui-runner\n")
        gate = hook.Gate(self.runner, os.fspath(self.lock), os.fspath(self.tmp / "none.json"), self.state)
        why = "console: the console session (cmux) is screen-locked"
        with mock.patch.object(hook, "console_refusal", return_value=why) as read:
            self.assertEqual(gate.claimed(), why)
            self.assertEqual(gate.claimed(), why)
            self.assertEqual(read.call_count, 1, "read at most every GATE_CONSOLE_EVERY_S")
            self.assertEqual(gate.confirmed(), why)
            self.assertEqual(read.call_count, 2, "the look right before a stop is fresh")
        with mock.patch.object(hook, "console_refusal", return_value=None):
            self.assertEqual(gate.claimed(), why, "the cached reading holds until the next read")
            self.assertIsNone(gate.confirmed())
        script.write_text("exec python3 /hook job-started --capacity-units 5 --instance 0\n")
        with mock.patch.object(hook, "console_refusal", return_value=why) as read:
            self.assertIsNone(hook.Gate(self.runner, os.fspath(self.lock), os.fspath(self.tmp / "none.json"),
                                        self.state).confirmed(), "a root runner's compiles need no console")
            read.assert_not_called()

    def test_a_root_runner_holds_while_a_token_its_jobs_need_is_taken(self) -> None:
        root = hook.RunnerScope(4, 2, 2, True)
        side = hook.RunnerScope(4, 2, 2, False)
        capacity = self.units(0)
        self.assertIsNone(hook.mini_full(capacity, 4, root))
        gui = hook.lock_file(capacity / "gui.token", fcntl.LOCK_EX)
        self.assertIsNone(hook.mini_full(capacity, 4, root), "a compile needs no gui token; a shard waits for it")
        os.close(gui)
        cases = (("persistent-dd.token", "persistent-dd-1.token"), ("root-1.token", "root-2.token"))
        whys = ("all 2 persistent-dd tokens on this mini are taken", "all 2 canonical roots on this mini are taken")
        for names, why in zip(cases, whys):
            fds = [hook.lock_file(capacity / name, fcntl.LOCK_EX) for name in names]
            self.assertEqual(hook.mini_full(capacity, 4, root), why)
            self.assertIsNone(hook.mini_full(capacity, 4, side), "side runners take no root job")
            fcntl.flock(fds[-1], fcntl.LOCK_UN)
            if len(fds) > 1:
                self.assertIsNone(hook.mini_full(capacity, 4, root), "one free copy is enough")
            for fd in fds:
                os.close(fd)
        self.assertEqual(hook.mini_full(self.units(3), 4, root),
                         "only 1 of 4 capacity units on this mini are free; a compile needs 2")
        self.assertIsNone(hook.mini_full(self.units(3), 4, side))
        self.assertEqual(hook.mini_full(self.units(1), 2, root),
                         "only 1 of 2 capacity units on this mini are free; a compile needs 2")
        self.assertEqual(hook.mini_full(self.units(1), 1, root), "all 1 capacity units on this mini are taken")
        self.assertIsNone(hook.mini_full(self.units(0), 1, root), "a 1-unit mini needs just its unit")
        # the gate reads the scope from the runner's own hook script
        self.runner_hook(4)
        gate = self.full_gate(self.units(3))
        self.assertEqual(gate.claimed(), "only 1 of 4 capacity units on this mini are free; a compile needs 2")

    def test_a_gui_runner_holds_while_the_gui_token_or_every_root_is_taken(self) -> None:
        gui = hook.RunnerScope(5, 2, 2, False, True)
        capacity = self.units(0)
        self.assertIsNone(hook.mini_full(capacity, 5, gui))
        held = hook.lock_file(capacity / "gui.token", fcntl.LOCK_EX)
        self.assertEqual(hook.mini_full(capacity, 5, gui), "the gui token is taken")
        os.close(held)
        dd = [hook.lock_file(capacity / name, fcntl.LOCK_EX) for name in ("persistent-dd.token", "persistent-dd-1.token")]
        self.assertIsNone(hook.mini_full(capacity, 5, gui), "a GUI job needs no persistent-dd token")
        roots = [hook.lock_file(capacity / name, fcntl.LOCK_EX) for name in ("root-1.token", "root-2.token")]
        self.assertEqual(hook.mini_full(capacity, 5, gui), "all 2 canonical roots on this mini are taken")
        os.close(roots.pop())
        self.assertIsNone(hook.mini_full(capacity, 5, gui), "one free root is enough")
        for fd in [*roots, *dd]:
            os.close(fd)
        self.assertIsNone(hook.mini_full(self.units(4), 5, gui), "a GUI job needs one unit, not a compile's two")
        self.assertEqual(hook.mini_full(self.units(5), 5, gui), "all 5 capacity units on this mini are taken")
        one = hook.RunnerScope(5, 1, 1, False, True)
        capacity = self.units(0)
        held = hook.lock_file(capacity / "root-1.token", fcntl.LOCK_EX)
        self.assertEqual(hook.mini_full(capacity, 5, one), "the canonical root is taken")
        os.close(held)

    def test_the_full_probe_never_competes_with_an_admission(self) -> None:
        capacity = self.units(1)
        admission = os.open(capacity / "admission.lock", os.O_RDONLY)
        self.addCleanup(os.close, admission)
        fcntl.flock(admission, fcntl.LOCK_EX | fcntl.LOCK_NB)
        with mock.patch.object(hook, "lock_file", side_effect=AssertionError("probed during an admission")):
            self.assertIs(hook.mini_full(capacity, 2), hook.UNKNOWN)
        fcntl.flock(admission, fcntl.LOCK_UN)
        self.assertIsNone(hook.mini_full(capacity, 2))
        # and the probe leaves the free unit free
        fd = hook.lock_file(capacity / "unit-1", fcntl.LOCK_EX)
        self.assertIsNotNone(fd)
        os.close(fd)

    def test_a_look_an_admission_blocks_keeps_the_last_answer(self) -> None:
        capacity = self.units(2)
        self.runner_hook(2)
        gate = self.full_gate(capacity)
        self.assertEqual(gate.claimed(), "all 2 capacity units on this mini are taken")
        # a full mini's admissions keep retrying: a blocked look must not read as free and restart a listener
        with mock.patch.object(hook, "mini_full", return_value=hook.UNKNOWN):
            self.assertEqual(gate.claimed(), "all 2 capacity units on this mini are taken")
            self.assertIsNone(gate.confirmed(), "the look right before a stop needs a reading of its own")
        with mock.patch.object(hook, "mini_full", return_value=None):
            self.assertIsNone(gate.claimed())
        with mock.patch.object(hook, "mini_full", return_value=hook.UNKNOWN):
            self.assertIsNone(gate.claimed(), "and a free mini stays free")

    def test_a_stopped_listener_stays_off_while_blocked_looks_hide_a_full_mini(self) -> None:
        capacity = self.units(2)
        self.runner_hook(2)
        gate = self.full_gate(capacity)
        gate.reload = lambda: None
        gate.child, gate.held, gate.full_why = None, "all 2 capacity units on this mini are taken", \
            "all 2 capacity units on this mini are taken"
        starts: list[int] = []
        gate.start = lambda: starts.append(1)
        with mock.patch.object(hook, "mini_full", return_value=hook.UNKNOWN):
            for _ in range(4):
                self.assertIsNone(gate.step())
        self.assertEqual(starts, [])
        with mock.patch.object(hook, "mini_full", return_value=None):
            for _ in range(2):
                gate.step()
        self.assertEqual(starts, [1], "two free looks restart the listener")

    def test_an_idle_listener_stops_only_after_the_claim_holds_for_two_polls(self) -> None:
        gate, stops = self.gate(["held", None, "held", "held"])
        for _ in range(3):
            self.assertIsNone(gate.step())
        self.assertEqual(stops, [])
        gate.step()
        self.assertEqual(stops, ["held"])

    def test_a_busy_runner_is_never_stopped_and_its_polls_do_not_count(self) -> None:
        gate, stops = self.gate(["held", "held"], busy=[True, True, True, False, False])
        for _ in range(4):
            gate.step()
        self.assertEqual(stops, [])  # three busy polls, then one idle claim: not two in a row yet
        gate.step()
        self.assertEqual(stops, ["held"])

    def test_the_fresh_look_before_a_stop_can_call_it_off(self) -> None:
        gate, stops = self.gate(["held", "held"])
        gate.confirmed = lambda: None
        gate.step(); gate.step()
        self.assertEqual(stops, [])

    def test_a_waiter_must_still_wait_in_a_second_reading(self) -> None:
        gate = hook.Gate(self.runner, os.fspath(self.lock), os.fspath(self.tmp / "none.json"), self.state)
        gate.waiting = {77}
        with mock.patch.object(hook, "host_waiters", return_value={88}):
            self.assertIsNone(gate.confirmed())  # 77 was a job-started in flight; 88 is new, not yet confirmed
        gate.waiting = {77}
        with mock.patch.object(hook, "host_waiters", return_value={77, 88}):
            self.assertEqual(gate.confirmed(), "a fleet build is waiting for the host (pid 77)")

    def test_a_throttled_mini_holds_until_it_has_cooled(self) -> None:
        gate = hook.Gate(self.runner, os.fspath(self.lock), os.fspath(self.tmp / "none.json"), self.state)
        self.level = 2
        self.assertIsNone(gate.claimed())  # heavy under load is normal
        self.level = 4
        gate.thermal_at -= hook.GATE_THERMAL_EVERY_S
        self.assertEqual(gate.claimed(), "the mini is throttled (thermal pressure level 4)")
        self.assertEqual(gate.confirmed(), "the mini is throttled (thermal pressure level 4)")
        self.level = 2
        gate.thermal_at -= hook.GATE_THERMAL_EVERY_S
        self.assertEqual(gate.claimed(), "the mini is throttled (thermal pressure level 2)")  # not cool yet
        self.level = 1
        self.assertIsNotNone(gate.claimed())  # read at most every thirty seconds
        gate.thermal_at -= hook.GATE_THERMAL_EVERY_S
        self.assertIsNone(gate.claimed())

    def test_an_unreadable_level_or_the_switch_off_never_holds(self) -> None:
        gate = hook.Gate(self.runner, os.fspath(self.lock), os.fspath(self.tmp / "none.json"), self.state)
        self.level = None
        self.assertIsNone(gate.claimed())
        self.level = 4
        with mock.patch.object(hook, "GATE_THERMAL_HOLD", 0):
            self.assertIsNone(gate.confirmed())

    def test_a_hold_at_level_one_does_not_flap_at_a_steady_level(self) -> None:
        gate = hook.Gate(self.runner, os.fspath(self.lock), os.fspath(self.tmp / "none.json"), self.state)
        self.level = 1
        with mock.patch.object(hook, "GATE_THERMAL_HOLD", 1):
            self.assertIsNotNone(gate.claimed())
            gate.thermal_at -= hook.GATE_THERMAL_EVERY_S
            self.assertIsNotNone(gate.claimed())  # still held at the same level
            self.level = 0
            gate.thermal_at -= hook.GATE_THERMAL_EVERY_S
            self.assertIsNone(gate.claimed())

    def test_a_bad_hold_setting_falls_back_instead_of_breaking_the_hook(self) -> None:
        out = subprocess.run([sys.executable, "-c", f"import runpy; m = runpy.run_path({os.fspath(HOOK)!r}, "
                              "run_name='x'); print(m['GATE_THERMAL_HOLD'])"],
                             env={**os.environ, "GLAEDA_RUNNER_THERMAL_HOLD": "hot"}, capture_output=True, text=True)
        self.assertEqual(out.stdout.strip(), "3", out.stderr)

    def test_a_hot_idle_listener_stops_and_a_busy_one_keeps_its_job(self) -> None:
        self.level = 3
        gate = hook.Gate(self.runner, os.fspath(self.lock), os.fspath(self.tmp / "none.json"), self.state)
        stops: list[str] = []
        gate.stop = stops.append  # type: ignore[method-assign]
        gate.child = mock.Mock(poll=mock.Mock(return_value=None))
        gate.busy = lambda: True  # type: ignore[method-assign]
        gate.step(); gate.step(); gate.step()
        self.assertEqual(stops, [])
        gate.busy = lambda: False  # type: ignore[method-assign]
        gate.step(); gate.step()
        self.assertEqual(stops, ["the mini is throttled (thermal pressure level 3)"])

    def test_the_level_parser_reads_notifyutil(self) -> None:
        run = mock.Mock(return_value=mock.Mock(stdout="com.apple.system.thermalpressurelevel 4\n"))
        with mock.patch.object(hook.subprocess, "run", run):
            self.assertEqual(REAL_LEVEL(), 4)
            run.return_value = mock.Mock(stdout="")
            self.assertIsNone(REAL_LEVEL())

    def test_the_gate_asks_lsof_at_most_every_thirty_seconds(self) -> None:
        gate = hook.Gate(self.runner, os.fspath(self.lock), os.fspath(self.tmp / "none.json"), self.state)
        with mock.patch.object(hook, "host_waiters", return_value={77}) as lsof:
            self.assertEqual(gate.claimed(), "a fleet build is waiting for the host (pid 77)")
            gate.claimed()
            self.assertEqual(lsof.call_count, 1)
            gate.waiting_at -= hook.GATE_WAITER_EVERY_S
            lsof.return_value = set()
            self.assertIsNone(gate.claimed())
        no_waiters = hook.Gate(self.runner, os.fspath(self.lock), os.fspath(self.tmp / "none.json"), self.state,
                               waiters=False)
        with mock.patch.object(hook, "host_waiters", return_value={77}) as lsof:
            self.assertIsNone(no_waiters.claimed())
            lsof.assert_not_called()

    def test_a_stopped_listener_restarts_after_two_free_polls_and_a_runner_exit_still_exits(self) -> None:
        gate, stops = self.gate(["held", "held", "held", None, None])
        gate.step(); gate.step()
        self.assertEqual(stops, ["held"])
        gate.child.poll.return_value = 0  # the listener exited because the gate asked
        with mock.patch.object(hook.subprocess, "Popen") as popen:
            self.assertIsNone(gate.step())  # the listener is gone
            self.assertIsNone(gate.child)
            gate.step()  # still held
            gate.step()  # free once
            popen.assert_not_called()
            gate.step()  # free twice
            popen.assert_called_once()
        gate.child = mock.Mock()
        gate.child.poll.return_value = 3  # the runner exited on its own: launchd decides
        self.assertEqual(gate.step(), 3)

    def test_a_job_that_slipped_in_is_never_killed_at_the_deadline(self) -> None:
        gate, _ = self.gate([], busy=[True, False])
        gate.stop_deadline = time.monotonic() - 1
        with mock.patch.object(gate, "kill") as kill:
            gate.step()
            kill.assert_not_called()
            gate.step()
            kill.assert_called_once()

    def test_sigterm_stops_a_running_listener_and_ends_a_held_gate(self) -> None:
        gate, stops = self.gate([])
        gate.terminate()
        gate.step()
        self.assertEqual(stops, ["the runner agent is stopping"])
        gate.child.poll.return_value = 0
        self.assertEqual(gate.step(), 0)
        held, _ = self.gate([None, None])
        held.child = None
        held.terminate()
        self.assertEqual(held.step(), 0)

    def test_sigterm_during_the_look_never_starts_a_listener(self) -> None:
        gate, _ = self.gate([])
        gate.child = None
        gate.claimed = lambda: (gate.terminate(), None)[1]
        with mock.patch.object(hook.subprocess, "Popen") as popen:
            self.assertEqual(gate.step(), 0)
            popen.assert_not_called()

    def test_a_gate_bug_hands_the_runner_back_and_never_kills_its_job(self) -> None:
        child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(0.3)"])
        gate = hook.Gate(self.runner, os.fspath(self.lock), "", self.state)
        gate.child = child
        self.assertEqual(gate.unexpected(RuntimeError("boom")), 0)  # waited for the child instead
        gate.child = None
        self.assertEqual(gate.unexpected(RuntimeError("boom")), hook.GATE_FALLBACK)
        with mock.patch.object(hook.Gate, "step", side_effect=OSError("ps failed")) as step, \
                mock.patch.object(hook, "GATE_POLL_S", 0):
            self.assertEqual(hook.listen(self.runner, os.fspath(self.lock), "", self.state, True), hook.GATE_FALLBACK)
        self.assertEqual(step.call_count, hook.GATE_MAX_FAILURES)  # a transient failure keeps gating
        flaky = iter([OSError("ps timed out"), 7])
        def step_once(_self):
            item = next(flaky)
            if isinstance(item, Exception):
                raise item
            return item
        with mock.patch.object(hook.Gate, "step", step_once), mock.patch.object(hook, "GATE_POLL_S", 0):
            self.assertEqual(hook.listen(self.runner, os.fspath(self.lock), "", self.state, True), 7)

    def test_a_broken_gate_still_ends_its_runner_when_the_agent_stops(self) -> None:
        child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"])
        self.addCleanup(child.kill)
        gate = hook.Gate(self.runner, os.fspath(self.lock), "", self.state)
        gate.child = child
        gate.terminating = True
        with mock.patch.object(gate, "stop", side_effect=OSError("ps failed")):
            self.assertIsNotNone(gate.unexpected(RuntimeError("boom")))  # terminated, not waited on forever

    def test_a_stop_with_no_listener_ends_the_whole_runner_tree(self) -> None:
        (self.runner / "run-helper.sh").write_text("#!/bin/bash\nsleep 30\n")  # between listeners
        gate = hook.Gate(self.runner, os.fspath(self.lock), "", self.state)
        gate.start()
        deadline = time.monotonic() + 10
        while not [p for p, (_, c) in hook.processes().items() if "sleep 30" in c and p in hook.descendants(gate.child.pid, hook.processes())]:
            self.assertLess(time.monotonic(), deadline)
            time.sleep(0.05)
        gate.stop("held")
        self.assertIsNotNone(gate.child.wait(timeout=10))
        time.sleep(0.2)
        self.assertEqual([p for p, (_, c) in hook.processes().items() if os.fspath(self.runner) in c], [])

    def test_the_gate_reloads_a_changed_hook_and_keeps_its_runner(self) -> None:
        gate = hook.Gate(self.runner, os.fspath(self.lock), "", self.state)
        gate.child = mock.Mock(pid=4321)
        gate.source = (0, 0, 0)
        with mock.patch.object(hook.os, "execv") as execv, mock.patch.object(hook.sys, "argv", [os.fspath(HOOK), "listen"]):
            gate.reload()
        args = execv.call_args[0][1]
        self.assertEqual(args[1:], [os.fspath(HOOK), "listen", "--adopt=4321"])
        gate.source = (0, 0, 0)
        with mock.patch.object(hook.subprocess, "run", return_value=mock.Mock(returncode=2)) as check, \
                mock.patch.object(hook.os, "execv") as execv, \
                mock.patch.object(hook.sys, "argv", [os.fspath(HOOK), "listen", "--no-waiters"]):
            gate.reload()
            execv.assert_not_called()  # a hook that rejects this exact argv is never exec'd
        self.assertEqual(check.call_args[0][0][1:], [os.fspath(HOOK), "listen", "--no-waiters", "--adopt=4321",
                                                     "--parse-only"])

    def test_an_ios_sim_side_runner_listens_only_with_room_for_a_2_unit_job(self) -> None:
        self.assertNotIn("--fit-units", self.listen_sh(sys.executable).read_text())
        script = self.listen_sh(sys.executable, ["glaeda-mini", "glaeda-side-std-xcode-26.6", "glaeda-ios-sim"])
        self.assertIn(f"listen --runner-dir {self.runner} --fit-units 2 &", script.read_text())
        argv = ["listen", "--runner-dir", os.fspath(self.runner), "--fit-units", "2", "--parse-only"]
        self.assertEqual(subprocess.run([sys.executable, os.fspath(HOOK), *argv], capture_output=True).returncode, 0)
        side = hook.RunnerScope(4, 2, 2, False)
        self.assertEqual(hook.mini_full(self.units(3), 4, side, 2),
                         "only 1 of 4 capacity units on this mini are free; its next job needs 2")
        self.assertIsNone(hook.mini_full(self.units(3), 4, side), "a light-only side runner still fits a light job")
        self.assertIsNone(hook.mini_full(self.units(2), 4, side, 2))
        self.runner_hook(4)
        (self.runner / hook.RUNNER_HOOK_SCRIPT).write_text(
            "exec python3 /hook job-started --capacity-units 4 --canonical-roots 2 --instance 3\n")
        gate = hook.Gate(self.runner, os.fspath(self.lock), os.fspath(self.tmp / "none.json"), self.state,
                         capacity_dir=self.units(3), fit_units=2)
        self.assertEqual(gate.claimed(), "only 1 of 4 capacity units on this mini are free; its next job needs 2")
        # a gate started from an older listen.sh (no flag in its argv) reads the flag from the current one
        old = hook.Gate(self.runner, os.fspath(self.lock), os.fspath(self.tmp / "none.json"), self.state,
                        capacity_dir=self.units(3))
        self.assertIsNone(old.claimed())
        (self.runner / hook.LISTEN_SCRIPT).write_text(script.read_text())
        self.assertEqual(hook.listen_fit_units(self.runner), 2)
        self.assertEqual(old.claimed(), "only 1 of 4 capacity units on this mini are free; its next job needs 2")

    def test_parse_only_accepts_the_gate_argv_and_an_old_hook_would_not(self) -> None:
        argv = ["listen", "--runner-dir", os.fspath(self.runner), "--no-waiters", "--adopt=1", "--parse-only"]
        self.assertEqual(subprocess.run([sys.executable, os.fspath(HOOK), *argv], capture_output=True).returncode, 0)
        old = self.tmp / "old-hook"
        old.write_text(HOOK.read_text().replace('p.add_argument("--parse-only"', 'p.add_argument("--renamed"'))
        self.assertEqual(subprocess.run([sys.executable, os.fspath(old), *argv], capture_output=True).returncode, 2)

    def run_listen(self, *extra: str) -> tuple[subprocess.Popen, Path]:
        # The hook runs in its own process, so the setUp patch of the thermal level does not reach it.
        env = {**os.environ, "GLAEDA_RUNNER_GATE_POLL_S": "0.1", "GLAEDA_RUNNER_THERMAL_HOLD": "0"}
        log = self.tmp / "runner.log"
        with log.open("wb") as out:
            proc = subprocess.Popen([*extra] if extra else
                                    [sys.executable, os.fspath(HOOK), "listen", "--runner-dir", os.fspath(self.runner),
                                     "--host-lock", os.fspath(self.lock), "--reservation",
                                     os.fspath(self.tmp / "none.json"), "--state-dir", os.fspath(self.state)],
                                    stdout=out, stderr=subprocess.STDOUT, env=env)
        self.addCleanup(lambda: proc.poll() is None and proc.kill())
        return proc, log

    def wait_for(self, log: Path, text: str, count: int = 1) -> None:
        deadline = time.monotonic() + 15
        while log.read_text().count(text) < count:
            self.assertLess(time.monotonic(), deadline, log.read_text())
            time.sleep(0.05)

    def test_listen_stops_and_restarts_a_real_listener(self) -> None:
        proc, log = self.run_listen()
        self.wait_for(log, "Listening for Jobs")
        self.assertEqual(len(self.listeners()), 1)
        fd = os.open(self.lock, os.O_RDONLY)
        fcntl.flock(fd, fcntl.LOCK_EX)
        self.wait_for(log, "holding the listener off: a fleet build holds the host lock")
        self.assertEqual(self.listeners(), [])
        self.assertIn("stopping the idle listener: a fleet build holds the host lock", log.read_text())
        self.assertIsNone(proc.poll())
        os.close(fd)
        self.wait_for(log, "the host is free again")
        self.wait_for(log, "Listening for Jobs", 2)
        proc.terminate()
        self.assertEqual(proc.wait(timeout=15), 0)
        self.assertEqual(self.listeners(), [])

    def test_listen_leaves_a_runner_with_a_job_alone(self) -> None:
        worker = self.runner / "bin/Runner.Worker"
        worker.write_text("#!/usr/bin/env python3\nimport time\ntime.sleep(60)\n")
        worker.chmod(0o755)
        gate = hook.Gate(self.runner, os.fspath(self.lock), os.fspath(self.tmp / "none.json"), self.state)
        gate.child = subprocess.Popen([sys.executable, "-c", f"import subprocess,sys,time; subprocess.run([sys.executable, {os.fspath(worker)!r}]); time.sleep(60)"])
        self.addCleanup(gate.child.kill)
        deadline = time.monotonic() + 10
        while not gate.busy():
            self.assertLess(time.monotonic(), deadline)
            time.sleep(0.05)
        self.hold(fcntl.LOCK_EX)
        with mock.patch.object(gate, "stop") as stop:
            for _ in range(4):
                gate.step()
            stop.assert_not_called()
            for pid in hook.descendants(gate.child.pid, hook.processes()):
                os.kill(pid, 9)
            deadline = time.monotonic() + 10
            while gate.busy():
                self.assertLess(time.monotonic(), deadline)
                time.sleep(0.05)
            gate.step(); gate.step()
            stop.assert_called_once_with("a fleet build holds the host lock")

    def listen_sh(self, python: str, labels: list[str] | None = None) -> Path:
        ctx = mock.Mock(python=python, hook_dir=HOOK.parent, runner_dir=self.runner, org=None, repo="manaflow-ai/cmux",
                        min_free_gib=0, member=None, capacity_units=4, labels=labels or ["glaeda-mini"])
        script = self.tmp / "listen.sh"
        script.write_bytes(cr.hook_wrappers(ctx)["listen.sh"])
        script.chmod(0o755)
        return script

    def test_listen_sh_runs_the_runner_without_a_gate_that_cannot_start(self) -> None:
        (self.runner / "run.sh").write_text(f"#!/bin/bash\necho plain run.sh > {self.tmp / 'ran'}\nexit 0\n")
        result = subprocess.run([os.fspath(self.listen_sh("/nonexistent/python3"))], capture_output=True, text=True,
                                timeout=30)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("running the runner without it", result.stdout)
        self.assertEqual((self.tmp / "ran").read_text(), "plain run.sh\n")

    def test_listen_sh_never_starts_a_second_runner_beside_a_live_one(self) -> None:
        script = self.listen_sh(sys.executable)
        proc, log = self.run_listen(os.fspath(script))
        self.wait_for(log, "Listening for Jobs")
        gate = [pid for pid, (_, cmd) in hook.processes().items()
                if "listen --runner-dir" in cmd and os.fspath(self.runner) in cmd and "listen.sh" not in cmd]
        self.assertEqual(len(gate), 1, gate)
        os.kill(gate[0], 9)  # jetsam, or a re-exec into a hook that cannot run this gate
        self.assertEqual(proc.wait(timeout=30), 1)  # launchd restarts a fresh gate
        self.assertIn("with its runner running", log.read_text())
        self.assertNotIn("running the runner without it", log.read_text())
        self.assertEqual(log.read_text().count("Listening for Jobs"), 1)
        self.assertEqual(self.listeners(), [])

    def test_listen_sh_forwards_sigterm_and_leaves_nothing_behind(self) -> None:
        script = self.listen_sh(sys.executable)
        proc, log = self.run_listen(os.fspath(script))
        self.wait_for(log, "Listening for Jobs")
        self.assertIn("--runner-dir", script.read_text())
        self.assertNotIn("--no-waiters", script.read_text())
        proc.terminate()
        self.assertEqual(proc.wait(timeout=15), 0)
        time.sleep(0.3)
        self.assertEqual(self.listeners(), [])
        self.assertNotIn("without it", log.read_text())


class RunnerTest(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        base = Path(os.path.realpath(self.tmp.name))
        self.home = base / "operator"
        self.home.mkdir()
        self.state = base / "fake"
        self.state.mkdir()
        self.bin = base / "bin"
        self.bin.mkdir()
        self.gh = make_executable(self.bin / "gh", FAKE_GH)
        self.curl = make_executable(self.bin / "curl", FAKE_CURL)
        self.launchctl = make_executable(self.bin / "launchctl", FAKE_LAUNCHCTL)
        self.build_release()
        self.patches = [
            mock.patch.dict(os.environ, {"HOME": os.fspath(self.home), "FAKE_STATE": os.fspath(self.state),
                                         "FAKE_REG_TOKEN": REG_TOKEN, "FAKE_REMOVE_TOKEN": REMOVE_TOKEN}),
            mock.patch.object(cr, "CURL", os.fspath(self.curl)),
            mock.patch.object(cr, "LAUNCHCTL", os.fspath(self.launchctl)),
            mock.patch.object(cr, "default_name", lambda: "mini-test-glaeda"),
        ]
        for p in self.patches:
            p.start()

    def tearDown(self) -> None:
        for p in reversed(self.patches):
            p.stop()
        self.tmp.cleanup()

    def build_release(self, sha_override: str | None = None) -> None:
        stage = self.state / "stage"
        stage.mkdir(exist_ok=True)
        make_executable(stage / "config.sh", FAKE_CONFIG)
        make_executable(stage / "run.sh", "#!/bin/bash\nexit 0\n")
        tarball = self.state / "runner.tar.gz"
        with tarfile.open(tarball, "w:gz") as tar:
            for name in ("config.sh", "run.sh"):
                tar.add(stage / name, arcname=name)
        sha = sha_override or hashlib.sha256(tarball.read_bytes()).hexdigest()
        asset = f"actions-runner-osx-arm64-{VERSION}.tar.gz"
        (self.state / "release.json").write_text(json.dumps({
            "tag_name": f"v{VERSION}",
            "body": f"notes\n<!-- BEGIN SHA osx-arm64 -->{sha}<!-- END SHA osx-arm64 -->\n",
            "assets": [{"name": asset, "browser_download_url": f"https://github.com/actions/runner/releases/download/v{VERSION}/{asset}"}],
        }))

    def invoke(self, *args: str, expect: int = 0, via_setup: bool = False, gh: bool = True,
               stdin: str = "") -> dict:
        out = io.StringIO()
        argv = ["--output", "json", *(["--gh", os.fspath(self.gh)] if gh else []), "--python", sys.executable, *args]
        with contextlib.redirect_stdout(out), mock.patch.object(sys, "stdin", io.StringIO(stdin)):
            code = setup.main(["--runner", *argv]) if via_setup else cr.main(argv)
        self.assertEqual(code, expect, out.getvalue()[-3000:])
        self.last_output = out.getvalue()
        return json.loads(out.getvalue())

    def log(self) -> list[dict]:
        path = self.state / "log.jsonl"
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

    def tree(self) -> dict[str, bytes | None]:
        return {os.fspath(p.relative_to(self.home)): (p.read_bytes() if p.is_file() else None)
                for p in sorted(self.home.rglob("*"))}

    def by_kind(self, receipt: dict) -> dict[str, dict]:
        return {a["kind"] + (":" + Path(a["path"]).name if a["kind"] == "hook" else ""): a for a in receipt["actions"]}

    # ------------------------------------------------------------ plan

    def test_plan_is_side_effect_free_and_prints_routing(self) -> None:
        receipt = self.invoke()
        self.assertFalse(receipt["applied"])
        self.assertEqual(self.tree(), {})
        for entry in self.log():
            self.assertEqual(entry["tool"], "gh", entry)
            self.assertNotIn("-X", entry["argv"], entry)
        kinds = self.by_kind(receipt)
        self.assertEqual(kinds["download"]["state"], "create")
        self.assertEqual(kinds["download"]["version"], VERSION)
        self.assertEqual(kinds["register"]["labels"], ["self-hosted", "macOS", "ARM64", "glaeda-mini"])
        self.assertFalse(kinds["register"]["ephemeral"])
        self.assertIn("gh variable set MACOS_RUNNER_15 --body glaeda-mini --repo manaflow-ai/cmux",
                      receipt["routing"]["route"])
        self.assertIn("gh variable delete MACOS_RUNNER_15 --repo manaflow-ai/cmux", receipt["routing"]["rollback"])
        human = io.StringIO()
        with contextlib.redirect_stdout(human):
            cr.main(["--gh", os.fspath(self.gh), "--python", sys.executable])
        self.assertIn("gh variable set MACOS_RUNNER_26 --body glaeda-mini --repo manaflow-ai/cmux", human.getvalue())
        self.assertEqual(self.tree(), {})

    def test_mini_setup_runner_flag_delegates(self) -> None:
        receipt = self.invoke(via_setup=True)
        self.assertEqual(receipt["schema"], "glaeda-cmux-runner/v1")
        self.assertEqual(self.tree(), {})

    # ------------------------------------------------------------ apply

    def test_apply_installs_registers_and_is_idempotent(self) -> None:
        receipt = self.invoke("--apply", "--labels", "cmux-extra")
        runner = self.home / "actions-runner-glaeda"
        self.assertTrue((runner / ".runner").is_file())
        self.assertTrue((runner / ".glaeda-cmux-runner").is_file())
        kinds = self.by_kind(receipt)
        self.assertEqual(kinds["verify"]["state"], "ok", kinds["verify"])
        self.assertEqual(kinds["verify"]["runner"]["id"], 4242)
        config = [e for e in self.log() if e["tool"] == "config.sh"][0]
        self.assertIn("--unattended", config["argv"])
        self.assertNotIn("--ephemeral", config["argv"])
        self.assertEqual(config["argv"][config["argv"].index("--labels") + 1], "glaeda-mini,cmux-extra")
        self.assertEqual(config["argv"][config["argv"].index("--url") + 1], "https://github.com/manaflow-ai/cmux")
        plist = plistlib.loads((self.home / "Library/LaunchAgents/com.teamleaderleo.glaeda.cmux-runner.plist").read_bytes())
        self.assertEqual(plist["ProgramArguments"], [os.fspath(runner / "glaeda-hooks/listen.sh")])
        listen = runner / "glaeda-hooks/listen.sh"
        self.assertTrue(os.access(listen, os.X_OK))
        self.assertIn(f"listen --runner-dir {runner} --no-waiters &", listen.read_text())  # no capacity units
        self.assertIn("exec ./run.sh", listen.read_text())
        env = plist["EnvironmentVariables"]
        started = Path(env["ACTIONS_RUNNER_HOOK_JOB_STARTED"])
        self.assertTrue(os.access(started, os.X_OK))
        self.assertTrue(os.access(env["ACTIONS_RUNNER_HOOK_JOB_COMPLETED"], os.X_OK))
        self.assertIn("--allowed-repo manaflow-ai/cmux", started.read_text())
        receipt_path = self.home / ".local/state/glaeda/cmux-runner/receipt.json"
        self.assertEqual(receipt_path.stat().st_mode & 0o777, 0o600)
        self.assertEqual(json.loads(receipt_path.read_text())["registration"]["runnerId"], 4242)
        self.assertFalse(any(e["tool"] == "launchctl" for e in self.log()), "sandbox HOME never calls launchctl")

        before = self.tree()
        calls = len(self.log())
        second = self.invoke("--apply", "--labels", "cmux-extra")
        states = {a["state"] for a in second["actions"] if a["kind"] != "verify"}
        self.assertEqual(states, {"unchanged"})
        self.assertFalse(any(e["tool"] in {"curl", "config.sh"} or "-X" in e["argv"] for e in self.log()[calls:]))
        after = self.tree()
        for tree in (before, after):
            tree.pop(".local/state/glaeda/cmux-runner/receipt.json")
        self.assertEqual(before, after)

    def test_installed_wrapper_hooks_refuse_fork_prs(self) -> None:
        self.invoke("--apply")
        hooks = self.home / "actions-runner-glaeda/glaeda-hooks"
        for name, (event_name, payload, admitted) in SAMPLE_EVENTS.items():
            with self.subTest(name):
                path = event(self.state, name, payload)
                result = subprocess.run(["/bin/bash", os.fspath(hooks / "job-started.sh")], capture_output=True,
                                        text=True, timeout=60, check=False, env={
                                            "PATH": "/usr/bin:/bin", "HOME": os.fspath(self.home),
                                            "GLAEDA_FLEET_DIR": os.fspath(self.home / "fleet"),
                                            "GITHUB_EVENT_NAME": event_name, "GITHUB_EVENT_PATH": os.fspath(path),
                                            "GITHUB_REPOSITORY": (payload.get("repository") or {}).get("full_name", "")})
                self.assertEqual(result.returncode == 0, admitted, result.stdout + result.stderr)
                done = subprocess.run(["/bin/bash", os.fspath(hooks / "job-completed.sh")], capture_output=True,
                                      text=True, timeout=60, check=False, env={"PATH": "/usr/bin:/bin",
                                                                               "HOME": os.fspath(self.home)})
                self.assertEqual(done.returncode, 0)

    def test_installed_wrapper_holds_the_lock_for_its_real_parent(self) -> None:
        self.invoke("--apply")
        hooks = self.home / "actions-runner-glaeda/glaeda-hooks"
        fleet = self.home / "fleet"
        fleet.mkdir()
        (fleet / "host.lock").touch()
        env = {"PATH": "/usr/bin:/bin", "HOME": os.fspath(self.home), "GLAEDA_FLEET_DIR": os.fspath(fleet),
               "GITHUB_EVENT_NAME": "push", "GITHUB_EVENT_PATH": os.fspath(event(self.state, "push", SAMPLE_EVENTS["push"][1])),
               "GITHUB_REPOSITORY": "manaflow-ai/cmux"}
        # No --watch-pid: the wrapper execs python, so the holder watches this test process, which lives on.
        started = subprocess.run(["/bin/bash", os.fspath(hooks / "job-started.sh")], capture_output=True,
                                 text=True, timeout=60, check=False, env=env)
        self.assertEqual(started.returncode, 0, started.stdout + started.stderr)
        self.assertIn(f"watching pid {os.getpid()}", started.stdout)
        time.sleep(3)  # longer than one holder poll: a holder watching a dead parent would have let go
        import fcntl
        fd = os.open(fleet / "host.lock", os.O_RDONLY)
        try:
            with self.assertRaises(OSError):
                fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        finally:
            os.close(fd)
        subprocess.run(["/bin/bash", os.fspath(hooks / "job-completed.sh")], capture_output=True, timeout=60,
                       check=False, env=env)
        fd = os.open(fleet / "host.lock", os.O_RDONLY)
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        finally:
            os.close(fd)

    def test_missing_interpreter_fails_closed(self) -> None:
        self.invoke("--apply", "--python", "/nonexistent/python3")
        hooks = self.home / "actions-runner-glaeda/glaeda-hooks"
        path = event(self.state, "push", SAMPLE_EVENTS["push"][1])
        started = subprocess.run(["/bin/bash", os.fspath(hooks / "job-started.sh")], capture_output=True, timeout=30,
                                 check=False, env={"PATH": "/usr/bin:/bin", "GITHUB_EVENT_NAME": "push",
                                                   "GITHUB_EVENT_PATH": os.fspath(path)})
        self.assertNotEqual(started.returncode, 0)
        done = subprocess.run(["/bin/bash", os.fspath(hooks / "job-completed.sh")], capture_output=True, timeout=30,
                              check=False, env={"PATH": "/usr/bin:/bin"})
        self.assertEqual(done.returncode, 0)

    def test_token_never_in_argv_files_or_output(self) -> None:
        self.invoke("--apply")
        self.invoke("--uninstall", "--apply")
        entries = self.log()
        self.assertTrue(any(e["tool"] == "config.sh" and e["envToken"] == REG_TOKEN for e in entries))
        self.assertTrue(any(e["tool"] == "config.sh" and e["envToken"] == REMOVE_TOKEN for e in entries))
        for entry in entries:
            joined = " ".join(entry["argv"])
            self.assertNotIn(REG_TOKEN, joined)
            self.assertNotIn(REMOVE_TOKEN, joined)
            if entry["tool"] != "config.sh":
                self.assertIsNone(entry["envToken"], entry)
        self.assertNotIn(REG_TOKEN, self.last_output)
        for path in list(self.home.rglob("*")):
            if path.is_file():
                data = path.read_bytes()
                self.assertNotIn(REG_TOKEN.encode(), data, path)
                self.assertNotIn(REMOVE_TOKEN.encode(), data, path)
        self.assertNotIn("ACTIONS_RUNNER_INPUT_TOKEN", os.environ)

    def test_token_scrubbed_from_config_output_on_failure(self) -> None:
        # config.sh rejects this token and echoes it back; the receipt and output must not carry it.
        out = io.StringIO()
        with mock.patch.object(cr, "read_token", lambda gh, ep: REG_TOKEN + "X"), contextlib.redirect_stdout(out):
            code = cr.main(["--apply", "--output", "json", "--gh", os.fspath(self.gh), "--python", sys.executable])
        self.assertEqual(code, 1)
        self.assertNotIn(REG_TOKEN + "X", out.getvalue())
        receipt = json.loads(out.getvalue())
        self.assertEqual(self.by_kind(receipt)["register"]["state"], "failed")
        self.assertEqual(self.by_kind(receipt)["agent"]["state"], "skipped")

    def test_sha_mismatch_refuses_install(self) -> None:
        self.build_release(sha_override="0" * 64)
        receipt = self.invoke("--apply", expect=1)
        self.assertEqual(self.by_kind(receipt)["download"]["state"], "failed")
        self.assertIn("SHA-256 mismatch", self.by_kind(receipt)["download"]["note"])
        self.assertFalse((self.home / "actions-runner-glaeda").exists())
        self.assertFalse(any(e["tool"] == "config.sh" or "-X" in e["argv"] for e in self.log()))

    def test_existing_runner_name_blocks_without_replace(self) -> None:
        (self.state / "runners.json").write_text(json.dumps({"runners": [
            {"id": 7, "name": "mini-test-glaeda", "status": "offline", "labels": []}]}))
        receipt = self.invoke("--apply", expect=1)
        self.assertEqual(self.by_kind(receipt)["register"]["state"], "blocked")
        self.assertFalse(receipt["ready"])
        self.assertFalse((self.home / "actions-runner-glaeda").exists())

    def test_launchctl_is_managed_for_the_real_home(self) -> None:
        fake_pw = mock.Mock(pw_dir=os.fspath(self.home))
        with mock.patch.object(cr.pwd, "getpwuid", return_value=fake_pw):
            receipt = self.invoke("--apply")
            self.assertTrue(self.by_kind(receipt)["agent"]["bootstrapped"])
            self.invoke("--uninstall", "--apply")
        verbs = [e["argv"][0] for e in self.log() if e["tool"] == "launchctl"]
        self.assertIn("bootstrap", verbs)
        self.assertIn("bootout", verbs)
        self.assertLess(verbs.index("bootstrap"), verbs.index("bootout"))

    def test_a_runner_held_for_a_repair_stays_disabled(self) -> None:
        held = self.home / ".local/state/glaeda/mini-fleet/runner-held"
        held.mkdir(parents=True)
        (held / "actions-runner-glaeda").touch()
        fake_pw = mock.Mock(pw_dir=os.fspath(self.home))
        with mock.patch.object(cr.pwd, "getpwuid", return_value=fake_pw):
            self.invoke("--apply")
        verbs = [e["argv"][0] for e in self.log() if e["tool"] == "launchctl"]
        self.assertNotIn("enable", verbs)

    def test_a_disabled_label_is_enabled_before_bootstrap(self) -> None:
        fake_pw = mock.Mock(pw_dir=os.fspath(self.home))
        label = "com.teamleaderleo.glaeda.cmux-runner"
        (self.state / "disabled.json").write_text(json.dumps([label]))  # a hand `launchctl disable`, no hold mark
        with mock.patch.object(cr.pwd, "getpwuid", return_value=fake_pw):
            plan = self.invoke()
            self.assertTrue(self.by_kind(plan)["agent"]["disabled"])
            self.assertIn("enables it before bootstrap", self.by_kind(plan)["agent"]["note"])
            receipt = self.invoke("--apply")
        agent = self.by_kind(receipt)["agent"]
        self.assertTrue(agent["bootstrapped"] and agent["wasDisabled"] and agent["enabled"], agent)
        self.assertTrue(receipt["ready"], receipt["blocking"])
        verbs = [e["argv"][0] for e in self.log() if e["tool"] == "launchctl" and e["argv"][0] != "print"]
        self.assertLess(verbs.index("enable"), verbs.index("bootstrap"), verbs)
        self.assertIn(["enable", f"gui/{os.getuid()}/{label}"], [e["argv"] for e in self.log() if e["tool"] == "launchctl"])
        on_disk = json.loads((self.home / ".local/state/glaeda/cmux-runner/receipt.json").read_text())
        self.assertTrue(self.by_kind(on_disk)["agent"]["wasDisabled"])

    def test_an_enabled_label_is_bootstrapped_without_enable(self) -> None:
        fake_pw = mock.Mock(pw_dir=os.fspath(self.home))
        with mock.patch.object(cr.pwd, "getpwuid", return_value=fake_pw):
            receipt = self.invoke("--apply")
        self.assertNotIn("wasDisabled", self.by_kind(receipt)["agent"])
        self.assertNotIn("enable", [e["argv"][0] for e in self.log() if e["tool"] == "launchctl"])

    def test_a_held_runner_stays_stopped_until_released(self) -> None:
        fake_pw = mock.Mock(pw_dir=os.fspath(self.home))
        label = "com.teamleaderleo.glaeda.cmux-runner"
        with mock.patch.object(cr.pwd, "getpwuid", return_value=fake_pw):
            self.invoke("--apply")
            held = self.home / ".local/state/glaeda/mini-fleet/runner-held/actions-runner-glaeda"
            held.parent.mkdir(parents=True)
            held.write_text("")  # runner_hold: mark, disable, bootout
            (self.state / "disabled.json").write_text(json.dumps([label]))
            (self.state / "log.jsonl").unlink()
            receipt = self.invoke("--apply")
            agent = self.by_kind(receipt)["agent"]
            self.assertTrue(agent["held"], agent)
            self.assertIn("runner_release", agent["note"])
            self.assertNotIn("bootstrapped", agent)
            self.assertTrue(receipt["ready"], receipt["blocking"])
            self.assertIn("held", self.by_kind(receipt)["verify"]["note"])
            verbs = [e["argv"][0] for e in self.log() if e["tool"] == "launchctl"]
            self.assertNotIn("enable", verbs)
            self.assertNotIn("bootstrap", verbs)
            self.assertEqual(json.loads((self.state / "disabled.json").read_text()), [label])
            held.unlink()  # runner_release: enable, bootstrap, drop the mark; or a later --apply once released
            receipt = self.invoke("--apply")
        self.assertTrue(self.by_kind(receipt)["agent"]["bootstrapped"])

    def test_relabel_never_starts_a_held_runner(self) -> None:
        fake_pw = mock.Mock(pw_dir=os.fspath(self.home))
        with mock.patch.object(cr.pwd, "getpwuid", return_value=fake_pw):
            self.invoke("--apply", "--labels", "ram48")
            held = self.home / ".local/state/glaeda/mini-fleet/runner-held/actions-runner-glaeda"
            held.parent.mkdir(parents=True)
            held.write_text("")
            (self.state / "log.jsonl").unlink()
            with mock.patch.object(cr, "xcode_present", return_value=True):
                receipt = self.invoke("--apply", "--manifest", self.manifest(), "--member", "mini-std",
                                      "--name", "mini-test-glaeda")
        self.assertEqual(self.by_kind(receipt)["register"]["state"], "updated")
        self.assertNotIn("bootstrap", [e["argv"][0] for e in self.log() if e["tool"] == "launchctl"])

    def test_print_disabled_is_read_per_exact_label(self) -> None:
        fake_pw = mock.Mock(pw_dir=os.fspath(self.home))
        (self.state / "disabled.json").write_text(json.dumps(["com.teamleaderleo.glaeda.cmux-runner.1"]))
        (self.state / "print-disabled-extra").write_text(
            '\t"com.teamleaderleo.glaeda.cmux-runner" => enabled\n\t"com.teamleaderleo.glaeda.cmux-runner.2" => true\n')
        with mock.patch.object(cr.pwd, "getpwuid", return_value=fake_pw):
            receipt = self.invoke("--apply")  # instance 0: its own entry says enabled; .1 is only a prefix match
            self.assertFalse(self.by_kind(receipt)["agent"]["disabled"])
            self.assertNotIn("enable", [e["argv"][0] for e in self.log() if e["tool"] == "launchctl"])
            ctx = cr.Context(cr.parser().parse_args(["--instance", "2"]))
            self.assertTrue(cr.launchctl_disabled(ctx), "older macOS prints => true")
            ctx = cr.Context(cr.parser().parse_args(["--instance", "1"]))
            self.assertTrue(cr.launchctl_disabled(ctx))
            self.assertEqual(cr.held_marker(ctx).name, "actions-runner-glaeda-1")  # runner_hold's basename

    def test_relabel_of_a_loaded_held_runner_leaves_it_stopped_and_says_so(self) -> None:
        fake_pw = mock.Mock(pw_dir=os.fspath(self.home))
        label = "com.teamleaderleo.glaeda.cmux-runner"
        (self.state / "loaded.json").write_text("[]")
        (self.state / "disabled.json").write_text(json.dumps([label]))
        with mock.patch.object(cr.pwd, "getpwuid", return_value=fake_pw):
            first = self.invoke("--apply", "--labels", "ram48")  # a hand disable: enabled, then started
            self.assertTrue(self.by_kind(first)["agent"]["wasDisabled"])
            self.assertEqual(json.loads((self.state / "loaded.json").read_text()), [label])
            held = self.home / ".local/state/glaeda/mini-fleet/runner-held/actions-runner-glaeda"
            held.parent.mkdir(parents=True)
            held.write_text("")  # marked held, but still loaded (a hold whose bootout did not take)
            (self.state / "log.jsonl").unlink()
            with mock.patch.object(cr, "xcode_present", return_value=True):
                receipt = self.invoke("--apply", "--manifest", self.manifest(), "--member", "mini-std",
                                      "--name", "mini-test-glaeda")
        agent = self.by_kind(receipt)["agent"]
        self.assertEqual(self.by_kind(receipt)["register"]["state"], "updated")
        self.assertFalse(agent["loaded"], agent)
        self.assertIn("runner_release", agent["note"])
        self.assertIn("held", self.by_kind(receipt)["verify"]["note"])
        self.assertEqual(json.loads((self.state / "loaded.json").read_text()), [])
        verbs = [e["argv"][0] for e in self.log() if e["tool"] == "launchctl"]
        self.assertNotIn("bootstrap", verbs)
        self.assertNotIn("enable", verbs)

    def test_relabel_records_the_enable_on_the_agent_step(self) -> None:
        fake_pw = mock.Mock(pw_dir=os.fspath(self.home))
        label = "com.teamleaderleo.glaeda.cmux-runner"
        with mock.patch.object(cr.pwd, "getpwuid", return_value=fake_pw):
            self.invoke("--apply", "--labels", "ram48")
            (self.state / "disabled.json").write_text(json.dumps([label]))
            with mock.patch.object(cr, "xcode_present", return_value=True):
                receipt = self.invoke("--apply", "--manifest", self.manifest(), "--member", "mini-std",
                                      "--name", "mini-test-glaeda")
        agent = self.by_kind(receipt)["agent"]
        self.assertTrue(agent["wasDisabled"] and agent["enabled"], agent)
        self.assertTrue(receipt["ready"], receipt["blocking"])

    def test_a_failed_enable_fails_the_agent_step(self) -> None:
        fake_pw = mock.Mock(pw_dir=os.fspath(self.home))
        (self.state / "disabled.json").write_text(json.dumps(["com.teamleaderleo.glaeda.cmux-runner"]))
        make_executable(self.launchctl, FAKE_LAUNCHCTL.replace(
            'if argv[0] in ("enable", "disable"):', 'if argv[0] == "enable":\n    print("denied"); sys.exit(1)\nif argv[0] in ("enable", "disable"):'))
        with mock.patch.object(cr.pwd, "getpwuid", return_value=fake_pw):
            receipt = self.invoke("--apply", expect=1)
        agent = self.by_kind(receipt)["agent"]
        self.assertEqual(agent["state"], "failed")
        self.assertIn("launchctl enable failed: denied", agent["note"])
        self.assertNotIn("bootstrap", [e["argv"][0] for e in self.log() if e["tool"] == "launchctl"])

    def test_uninstall_drops_the_hold_mark(self) -> None:
        self.invoke("--apply")
        held = self.home / ".local/state/glaeda/mini-fleet/runner-held/actions-runner-glaeda"
        held.parent.mkdir(parents=True)
        held.write_text("")
        receipt = self.invoke("--uninstall", "--apply")
        self.assertTrue(self.by_kind(receipt)["agent"]["heldMarkRemoved"])
        self.assertFalse(held.exists())

    # ------------------------------------------------------------ uninstall

    def test_uninstall_plan_is_side_effect_free_then_apply_removes_owned(self) -> None:
        self.invoke("--apply", "--name", "custom-mini")
        before, calls = self.tree(), len(self.log())
        plan = self.invoke("--uninstall")  # no --name: the receipt decides what to remove
        self.assertEqual(before, self.tree())
        self.assertFalse(any("-X" in e["argv"] or e["tool"] == "config.sh" for e in self.log()[calls:]))
        states = {a["kind"]: a["state"] for a in plan["actions"]}
        self.assertEqual(states, {"agent": "remove", "deregister": "remove", "dir": "remove"})
        self.invoke("--uninstall", "--apply")
        self.assertFalse((self.home / "actions-runner-glaeda").exists())
        self.assertFalse((self.home / "Library/LaunchAgents/com.teamleaderleo.glaeda.cmux-runner.plist").exists())
        self.assertEqual(json.loads((self.state / "runners.json").read_text())["runners"], [])
        remove = [e for e in self.log() if e["tool"] == "config.sh" and e["argv"][:1] == ["remove"]]
        self.assertEqual(len(remove), 1)
        state_dir = self.home / ".local/state/glaeda/cmux-runner"
        self.assertTrue((state_dir / "uninstall-receipt.json").is_file())
        self.assertFalse((state_dir / "receipt.json").exists())

    def test_uninstall_without_receipt_removes_nothing(self) -> None:
        runner = self.home / "actions-runner-glaeda"
        runner.mkdir()
        (runner / "config.sh").write_text("#!/bin/sh\n")
        before = self.tree()
        receipt = self.invoke("--uninstall", "--apply")
        self.assertTrue(any("nothing is removed" in a.get("note", "") for a in receipt["actions"]))
        after = {k: v for k, v in self.tree().items() if not k.startswith(".local")}
        self.assertEqual(before, after)
        self.assertTrue((self.home / ".local/state/glaeda/cmux-runner/uninstall-receipt.json").is_file())
        self.assertFalse(any(e["tool"] == "config.sh" or "-X" in e["argv"] for e in self.log()))

    def test_preexisting_runner_dir_is_never_adopted(self) -> None:
        runner = self.home / "actions-runner-glaeda"
        runner.mkdir()
        (runner / "keep.txt").write_text("operator data")
        receipt = self.invoke("--apply", expect=1)
        self.assertEqual(self.by_kind(receipt)["dir"]["state"], "blocked")
        self.assertEqual(sorted(p.name for p in runner.iterdir()), ["keep.txt"])
        self.assertFalse(any(e["tool"] in {"curl", "config.sh"} for e in self.log()))
        self.invoke("--uninstall", "--apply")
        self.assertTrue((runner / "keep.txt").exists())

    def test_marker_mismatch_keeps_dir_and_registration(self) -> None:
        self.invoke("--apply")
        runner = self.home / "actions-runner-glaeda"
        (runner / ".glaeda-cmux-runner").write_text("someone-else\n")
        plan = self.invoke("--uninstall", "--apply")
        states = {a["kind"]: a["state"] for a in plan["actions"]}
        self.assertEqual(states["dir"], "kept")
        self.assertEqual(states["deregister"], "kept")
        self.assertTrue((runner / ".runner").exists())
        self.assertFalse(any(e["tool"] == "config.sh" and e["argv"][:1] == ["remove"] for e in self.log()))

    def test_operator_edited_plist_is_kept(self) -> None:
        self.invoke("--apply")
        plist = self.home / "Library/LaunchAgents/com.teamleaderleo.glaeda.cmux-runner.plist"
        plist.write_bytes(plist.read_bytes() + b"\n<!-- operator edit -->\n")
        plan = self.invoke("--uninstall", "--apply")
        self.assertEqual({a["kind"]: a["state"] for a in plan["actions"]}["agent"], "kept")
        self.assertTrue(plist.exists())

    def test_failed_deregistration_keeps_dir_and_receipt(self) -> None:
        self.invoke("--apply")
        (self.state / "fail-config-remove").touch()
        (self.state / "fail-delete").touch()
        receipt = self.invoke("--uninstall", "--apply", expect=1)
        states = {a["kind"]: a["state"] for a in receipt["actions"]}
        self.assertEqual(states["deregister"], "failed")
        self.assertEqual(states["dir"], "kept")
        self.assertTrue((self.home / "actions-runner-glaeda/.runner").exists())
        self.assertTrue((self.home / ".local/state/glaeda/cmux-runner/receipt.json").exists())
        (self.state / "fail-config-remove").unlink()
        (self.state / "fail-delete").unlink()
        # The plist was already removed by the failed run; the retry still deregisters and cleans up.
        retry = self.invoke("--uninstall", "--apply")
        self.assertEqual({a["kind"]: a["state"] for a in retry["actions"]}["dir"], "remove")
        self.assertFalse((self.home / "actions-runner-glaeda").exists())

    def test_api_fallback_never_deletes_another_machines_runner(self) -> None:
        self.invoke("--apply")
        # the name was re-registered elsewhere with --replace: same name, new id
        (self.state / "runners.json").write_text(json.dumps({"runners": [
            {"id": 9999, "name": "mini-test-glaeda", "status": "online", "labels": []}]}))
        (self.state / "fail-config-remove").touch()
        receipt = self.invoke("--uninstall", "--apply", expect=1)
        dereg = next(a for a in receipt["actions"] if a["kind"] == "deregister")
        self.assertEqual(dereg["state"], "failed")
        self.assertIn("not deleted", dereg["note"])
        self.assertFalse(any(e["tool"] == "gh" and "DELETE" in e["argv"] for e in self.log()))
        self.assertTrue((self.home / "actions-runner-glaeda").is_dir())
        self.assertTrue((self.home / ".local/state/glaeda/cmux-runner/receipt.json").is_file())

    def test_a_second_install_elsewhere_is_blocked_and_keeps_the_receipt(self) -> None:
        self.invoke("--apply")
        receipt_path = self.home / ".local/state/glaeda/cmux-runner/receipt.json"
        before = receipt_path.read_bytes()
        (self.home / "elsewhere").mkdir()
        for args in (("--runner-dir", os.fspath(self.home / "elsewhere")),
                     ("--runner-dir", os.fspath(self.home / "fresh")),
                     ("--name", "other-glaeda"), ("--repo", "teamleaderleo/cmux")):
            with self.subTest(args=args):
                blocked = self.invoke("--apply", *args, expect=1)
                self.assertEqual([a["state"] for a in blocked["actions"]], ["blocked"])
                self.assertEqual(receipt_path.read_bytes(), before)
                self.assertFalse((self.home / "fresh").exists())
        removed = self.invoke("--uninstall", "--apply")
        self.assertTrue(all(a["applied"] for a in removed["actions"] if a["state"] == "remove"))
        self.assertFalse((self.home / "actions-runner-glaeda").exists())

    def test_rerun_after_failed_unpack_recovers(self) -> None:
        self.invoke("--apply")
        runner = self.home / "actions-runner-glaeda"
        for name in ("config.sh", ".runner"):
            (runner / name).unlink()
        (self.state / "runners.json").write_text(json.dumps({"runners": []}))
        receipt = self.invoke("--apply")
        self.assertTrue(receipt["ready"], receipt["blocking"])
        self.assertTrue((runner / ".runner").is_file())

    def test_registration_finished_after_timeout_is_adopted_then_removed(self) -> None:
        # config.sh outlived our timeout: .runner exists but the receipt recorded no registration
        with mock.patch.object(cr, "run_with_token", side_effect=lambda argv, token, cwd, timeout=900: (
                subprocess.run(argv, env={**os.environ, "ACTIONS_RUNNER_INPUT_TOKEN": token}, cwd=cwd,
                               capture_output=True, check=False), (127, "timed out after 900 seconds"))[1]):
            first = self.invoke("--apply", expect=1)
        self.assertEqual(self.by_kind(first)["register"]["state"], "failed")
        runner = self.home / "actions-runner-glaeda"
        self.assertTrue((runner / ".runner").is_file())
        second = self.invoke("--apply")
        self.assertTrue(second["ready"], second["blocking"])
        self.assertIn("adopted", self.by_kind(second)["register"]["note"])
        receipt = json.loads((self.home / ".local/state/glaeda/cmux-runner/receipt.json").read_text())
        self.assertEqual(receipt["registration"]["runnerId"], 4242)
        removed = self.invoke("--uninstall", "--apply")
        self.assertTrue(next(a for a in removed["actions"] if a["kind"] == "deregister")["applied"])
        self.assertEqual(json.loads((self.state / "runners.json").read_text())["runners"], [])
        self.assertFalse(runner.exists())

    def test_uninstall_deregisters_an_unadopted_interrupted_registration(self) -> None:
        with mock.patch.object(cr, "run_with_token", side_effect=lambda argv, token, cwd, timeout=900: (
                subprocess.run(argv, env={**os.environ, "ACTIONS_RUNNER_INPUT_TOKEN": token}, cwd=cwd,
                               capture_output=True, check=False), (127, "timed out"))[1]):
            self.invoke("--apply", expect=1)
        removed = self.invoke("--uninstall", "--apply")
        self.assertTrue(next(a for a in removed["actions"] if a["kind"] == "deregister")["applied"])
        self.assertEqual(json.loads((self.state / "runners.json").read_text())["runners"], [])

    def test_interrupted_registration_uninstall_uses_the_runners_own_name_and_scope(self) -> None:
        with mock.patch.object(cr, "run_with_token", side_effect=lambda argv, token, cwd, timeout=900: (
                subprocess.run(argv, env={**os.environ, "ACTIONS_RUNNER_INPUT_TOKEN": token}, cwd=cwd,
                               capture_output=True, check=False), (124, "timed out"))[1]):
            self.invoke("--apply", "--repo", "teamleaderleo/cmux", "--name", "custom-glaeda", expect=1)
        removed = self.invoke("--uninstall", "--apply")
        self.assertTrue(next(a for a in removed["actions"] if a["kind"] == "deregister")["applied"])
        tokens = [e["argv"] for e in self.log() if e["tool"] == "gh" and "-X" in e["argv"]]
        self.assertIn("repos/teamleaderleo/cmux/actions/runners/remove-token", tokens[-1])
        self.assertEqual(json.loads((self.state / "runners.json").read_text())["runners"], [])

    def test_timeout_kills_the_whole_process_group(self) -> None:
        marker = self.home / "grandchild-survived"
        script = make_executable(self.home / "slow-config.sh",
                                 "#!/bin/bash\n(sleep 3; touch " + os.fspath(marker) + ") &\nsleep 30\n")
        start = time.monotonic()
        code, out = cr.run_with_token([os.fspath(script)], REG_TOKEN, self.home, timeout=1)
        self.assertEqual(code, 124)
        self.assertLess(time.monotonic() - start, 10)
        self.assertNotIn(REG_TOKEN, out)
        time.sleep(4)
        self.assertFalse(marker.exists())

    def manifest(self) -> str:
        path = self.state / "mini-fleet.json"
        path.write_text(json.dumps(MANIFEST))
        return os.fspath(path)

    def config_argvs(self) -> list[list[str]]:
        return [e["argv"] for e in self.log() if e["tool"] == "config.sh" and e["argv"][:1] != ["remove"]]

    def test_manifest_install_derives_labels_and_name(self) -> None:
        with mock.patch.object(cr, "xcode_present", return_value=True):
            receipt = self.invoke("--apply", "--manifest", self.manifest(), "--member", "mini-std")
        argv = self.config_argvs()[0]
        self.assertEqual(argv[argv.index("--name") + 1], "mini-std-glaeda")
        self.assertEqual(argv[argv.index("--labels") + 1], "glaeda-mini,glaeda-class-std,glaeda-dedicated,xcode-26.6,glaeda-std-xcode-26.6,"
                                                         "glaeda-root-std-xcode-26.6,glaeda-runner-mini-std-glaeda")
        self.assertEqual(receipt["member"]["class"], "std")
        self.assertEqual(self.by_kind(receipt)["verify"]["state"], "ok")
        # a plain re-run keeps the registered labels and never asks for a relabel
        again = self.invoke("--apply")
        self.assertEqual(self.by_kind(again)["register"]["state"], "unchanged")
        self.assertEqual(len(self.config_argvs()), 1)

    def test_manifest_disk_floor_is_baked_into_the_hook(self) -> None:
        manifest = json.loads(json.dumps(MANIFEST))
        manifest["defaults"]["disk"] = {"min_free_gib": 100}
        path = self.state / "floor.json"
        path.write_text(json.dumps(manifest))
        with mock.patch.object(cr, "xcode_present", return_value=True):
            self.invoke("--apply", "--manifest", os.fspath(path), "--member", "mini-std")
        hooks = self.home / "actions-runner-glaeda/glaeda-hooks"
        self.assertIn("--min-free-gib 100", (hooks / "job-started.sh").read_text())
        self.assertIn("--require-eligible --fleet-class m4pro-48 --toolchain-xcode /Applications/Xcode_26.6.app",
                      (hooks / "job-started.sh").read_text())
        self.assertTrue((hooks / "glaeda_reservation.py").is_file())
        shim = (hooks / "glaeda-canonical-root").read_text()
        self.assertIn("take-root --root", shim)
        self.assertIn("take-gui --capacity-dir", shim)
        for bad in ([], ["take"], ["nope"]):
            self.assertEqual(subprocess.run(["/bin/sh", os.fspath(hooks / "glaeda-canonical-root"), *bad],
                                            capture_output=True).returncode, 2, bad)
        self.assertIn("--canonical-roots 1 --capacity-dir", shim)
        self.assertIn("--state-dir", shim)
        self.assertTrue(os.access(hooks / "glaeda-canonical-root", os.X_OK))
        self.assertNotIn("--compile-slots", (hooks / "job-started.sh").read_text())  # 1 is the hook's default
        # one root: no --canonical-roots, but --instance still tells the gate a root runner from a side one
        self.assertNotIn("--canonical-roots", (hooks / "job-started.sh").read_text())
        self.assertIn("--capacity-units 4 --instance 0\n", (hooks / "job-started.sh").read_text())
        manifest["defaults"]["runner"] = {"classes": {"std": {"compileSlots": 2, "canonicalRoots": 2}}}
        path.write_text(json.dumps(manifest))
        with mock.patch.object(cr, "xcode_present", return_value=True):
            self.invoke("--apply", "--manifest", os.fspath(path), "--member", "mini-std")
        self.assertIn("--capacity-units 4 --compile-slots 2 --canonical-roots 2 --instance 0",
                      (hooks / "job-started.sh").read_text())  # instance 0 prefers root 1
        self.assertNotIn("--trusted-ref", (hooks / "job-started.sh").read_text())
        self.assertIn("--test-keychain", (hooks / "job-started.sh").read_text())
        manifest["hosts"]["mini-std"].setdefault("overrides", {})["runner"] = {
            "trustedRef": "refs/heads/main", "trustedRepo": "manaflow-ai/cmux"}
        path.write_text(json.dumps(manifest))
        with mock.patch.object(cr, "xcode_present", return_value=True):
            self.invoke("--apply", "--manifest", os.fspath(path), "--member", "mini-std")
        self.assertIn("--trusted-ref refs/heads/main --trusted-repo manaflow-ai/cmux", (hooks / "job-started.sh").read_text())
        self.assertNotIn("--test-keychain", (hooks / "job-started.sh").read_text(), "a secret-holding host keeps its keychains")

    def test_the_last_instance_is_the_gui_runner_with_only_the_gui_pool_label(self) -> None:
        manifest = json.loads(json.dumps(MANIFEST))
        manifest["hosts"]["mini-std"]["roles"].append("ios-simulators")
        manifest["hosts"]["mini-std"]["overrides"] = {"ios_simulator": {"runtimes": ["23F77"]}}
        manifest["defaults"]["runner"] = {"classes": {"std": {"runners": 5, "capacityUnits": 5, "compileSlots": 2,
                                                              "canonicalRoots": 2, "guiRunners": 1}}}
        path = self.state / "gui.json"
        path.write_text(json.dumps(manifest))
        with mock.patch.object(cr, "xcode_present", return_value=True), \
                mock.patch.object(cr, "ios_sim_present", return_value=True):
            for instance in ("0", "2", "4"):
                self.invoke("--apply", "--manifest", os.fspath(path), "--member", "mini-std", "--instance", instance)
        labels = [argv[argv.index("--labels") + 1].split(",") for argv in self.config_argvs()]
        self.assertEqual([[x for x in l if x.startswith(("glaeda-std", "glaeda-root", "glaeda-side", "glaeda-gui"))]
                          for l in labels],
                         [["glaeda-std-xcode-26.6", "glaeda-root-std-xcode-26.6"],
                          ["glaeda-std-xcode-26.6", "glaeda-side-std-xcode-26.6"],
                          ["glaeda-gui-std-xcode-26.6"]],
                         "the gui runner carries neither the pool label nor a root or side one")
        self.assertIn("glaeda-ios-sim", labels[0])
        self.assertNotIn("glaeda-ios-sim", labels[2], "no iOS job ever takes the gui runner")
        self.assertFalse([x for x in labels[2] if x.startswith("glaeda-runner-")])
        started = (self.home / "actions-runner-glaeda-4/glaeda-hooks/job-started.sh").read_text()
        self.assertIn("--canonical-roots 2 --instance 4 --gui-runner\n", started)
        self.assertNotIn("--gui-runner", (self.home / "actions-runner-glaeda-2/glaeda-hooks/job-started.sh").read_text())
        self.assertEqual(hook.runner_scope(self.home / "actions-runner-glaeda-4"), hook.RunnerScope(5, 2, 2, False, True))

    def test_test_keychain_is_created_unlocked_first_and_default(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            (home / "Library/Keychains").mkdir(parents=True)
            path = os.fspath(home / "Library/Keychains" / hook.TEST_KEYCHAIN)
            login = os.fspath(home / "Library/Keychains/login.keychain-db")
            state = {"list": [login], "default": login}
            calls: list[list[str]] = []

            def fake(argv: list[str], **_: object) -> subprocess.CompletedProcess:
                calls.append(argv[1:])
                verb, rest = argv[1], argv[2:]
                out = ""
                if verb == "create-keychain":
                    Path(rest[-1]).write_bytes(b"")
                elif verb == "list-keychains" and "-s" in rest:
                    state["list"] = rest[rest.index("-s") + 1:]
                elif verb == "list-keychains":
                    out = "".join(f'    "{k}"\n' for k in state["list"])
                elif verb == "default-keychain" and "-s" in rest:
                    state["default"] = rest[-1]
                elif verb == "default-keychain":
                    out = f'    "{state["default"]}"\n'
                return subprocess.CompletedProcess(argv, 0, out, "")

            with mock.patch.object(hook.subprocess, "run", side_effect=fake):
                self.assertIn("unlocked and default", hook.ensure_test_keychain(home))
                self.assertEqual(state, {"list": [path, login], "default": path})
                self.assertIn(["create-keychain", "-p", "", path], calls)
                self.assertIn(["set-keychain-settings", path], calls, "no lock timeout, no lock on sleep")
                calls.clear()
                self.assertIn("unlocked and default", hook.ensure_test_keychain(home))
                self.assertEqual([c[0] for c in calls],
                                 ["set-keychain-settings", "unlock-keychain", "list-keychains", "default-keychain"],
                                 "idempotent: nothing recreated or reordered")
                self.assertFalse(any(login in c for c in calls if c[0] != "list-keychains"),
                                 "the login keychain is never changed")
            state.update({"list": [login, path], "default": login})  # present but not first: reordered
            with mock.patch.object(hook.subprocess, "run", side_effect=fake):
                self.assertIn("unlocked and default", hook.ensure_test_keychain(home))
                self.assertEqual(state, {"list": [path, login], "default": path})

            def failing(verb: str):
                def run(argv: list[str], **kw: object) -> subprocess.CompletedProcess:
                    done = fake(argv, **kw)
                    return (subprocess.CompletedProcess(argv, 1, "", "no") if argv[1] == verb and "-s" in argv
                            else done)
                return run
            for verb, why in (("list-keychains", "search list"), ("default-keychain", "default")):
                state.update({"list": [login], "default": login})
                with self.subTest(verb), mock.patch.object(hook.subprocess, "run", side_effect=failing(verb)):
                    self.assertIn(f"setting the {why} failed", hook.ensure_test_keychain(home))
            with mock.patch.object(hook.subprocess, "run",
                                   return_value=subprocess.CompletedProcess([], 51, "", "locked")):
                self.assertEqual(hook.ensure_test_keychain(home), "test keychain: unlock failed")
            with mock.patch.object(hook.subprocess, "run", side_effect=subprocess.TimeoutExpired("security", 15)):
                self.assertEqual(hook.ensure_test_keychain(home), "test keychain: TimeoutExpired")

    def test_instances_get_their_own_paths_and_share_the_capacity(self) -> None:
        with mock.patch.object(cr, "xcode_present", return_value=True):
            first = self.invoke("--apply", "--manifest", self.manifest(), "--member", "mini-std")
            third = self.invoke("--apply", "--manifest", self.manifest(), "--member", "mini-std", "--instance", "3")
        names = [argv[argv.index("--name") + 1] for argv in self.config_argvs()]
        self.assertEqual(names, ["mini-std-glaeda", "mini-std-glaeda-3"])
        labels = [argv[argv.index("--labels") + 1].split(",") for argv in self.config_argvs()]
        self.assertEqual([("glaeda-root-std-xcode-26.6" in l, "glaeda-std-xcode-26.6" in l,
                           "glaeda-side-std-xcode-26.6" in l) for l in labels],
                         [(True, True, False), (False, True, True)],
                         "only instance 0 is the root runner of a one-root mini; the rest carry the side label")
        self.assertEqual([[x for x in l if x.startswith("glaeda-runner-")] for l in labels],
                         [["glaeda-runner-mini-std-glaeda"], []],
                         "a root runner carries a label naming only itself; a side runner none")
        self.assertEqual(third["runnerDir"], os.fspath(self.home / "actions-runner-glaeda-3"))
        self.assertEqual(first["runnerDir"], os.fspath(self.home / "actions-runner-glaeda"))
        plist = self.home / "Library/LaunchAgents/com.teamleaderleo.glaeda.cmux-runner.3.plist"
        self.assertEqual(plistlib.loads(plist.read_bytes())["Label"], "com.teamleaderleo.glaeda.cmux-runner.3")
        self.assertTrue((self.home / ".local/state/glaeda/cmux-runner/instance-3/receipt.json").is_file())
        self.assertTrue((self.home / ".local/state/glaeda/cmux-runner/receipt.json").is_file())
        for runner in ("actions-runner-glaeda", "actions-runner-glaeda-3"):
            self.assertIn("--capacity-units 4", (self.home / runner / "glaeda-hooks/job-started.sh").read_text())
        # a plain re-run keeps the member's capacity, gate and floor
        self.invoke("--apply", "--instance", "3")
        wrapper = (self.home / "actions-runner-glaeda-3/glaeda-hooks/job-started.sh").read_text()
        self.assertIn("--capacity-units 4", wrapper)
        self.assertIn("--require-eligible --fleet-class m4pro-48", wrapper)
        err = io.StringIO()
        with mock.patch.object(cr, "xcode_present", return_value=True), contextlib.redirect_stderr(err):
            self.assertEqual(cr.main(["--gh", os.fspath(self.gh), "--manifest", self.manifest(), "--member",
                                      "mini-std", "--instance", "4"]), 2)
        self.assertIn("instance 4 is beyond that", err.getvalue())

    def test_manifest_refusals_and_exclusive_flags(self) -> None:
        for args in (("--manifest", self.manifest(), "--member", "laptop"),
                     ("--manifest", self.manifest(), "--member", "mini-std", "--labels", "x"),
                     ("--manifest", self.manifest()), ("--member", "mini-std")):
            with self.subTest(args=args), contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(cr.main(["--gh", os.fspath(self.gh), *args]), 2)
        self.assertEqual(self.tree(), {})

    def test_relabel_reregisters_in_place_and_keeps_work(self) -> None:
        self.invoke("--apply", "--labels", "ram48")  # the pre-manifest fleet install
        runner = self.home / "actions-runner-glaeda"
        (runner / "_work").mkdir(exist_ok=True)
        (runner / "_work" / "hot").write_text("derived data")
        for stale in (".runner_migrated", ".credentials_migrated"):
            (runner / stale).write_text("{}")
        with mock.patch.object(cr, "xcode_present", return_value=True):
            plan = self.invoke("--manifest", self.manifest(), "--member", "mini-std", "--name", "mini-test-glaeda")
            self.assertEqual(self.by_kind(plan)["register"]["state"], "relabel")
            self.assertEqual(self.by_kind(plan)["register"]["previousLabels"],
                             ["self-hosted", "macOS", "ARM64", "glaeda-mini", "ram48"])
            receipt = self.invoke("--apply", "--manifest", self.manifest(), "--member", "mini-std",
                                  "--name", "mini-test-glaeda")
        reg = self.by_kind(receipt)["register"]
        self.assertEqual(reg["state"], "updated", reg)
        argv = self.config_argvs()[-1]
        self.assertIn("--replace", argv)
        self.assertEqual(argv[argv.index("--labels") + 1], "glaeda-mini,glaeda-class-std,glaeda-dedicated,xcode-26.6,glaeda-std-xcode-26.6,"
                                                         "glaeda-root-std-xcode-26.6,glaeda-runner-mini-test-glaeda")
        runners = json.loads((self.state / "runners.json").read_text())["runners"]
        self.assertEqual([(r["name"], r["id"]) for r in runners], [("mini-test-glaeda", 4243)])
        self.assertEqual((runner / "_work" / "hot").read_text(), "derived data")
        self.assertFalse((runner / ".runner_migrated").exists())
        self.assertFalse((runner / ".credentials_migrated").exists())
        saved = json.loads((self.home / ".local/state/glaeda/cmux-runner/receipt.json").read_text())
        self.assertEqual(saved["registration"]["runnerId"], 4243)
        self.assertIn("glaeda-class-std", saved["registration"]["labels"])
        self.assertEqual(self.by_kind(receipt)["verify"]["state"], "ok")

    def human(self, *args: str) -> str:
        out = io.StringIO()
        with contextlib.redirect_stdout(out), mock.patch.object(sys, "stdin", io.StringIO("")):
            cr.main(["--gh", os.fspath(self.gh), "--python", sys.executable, *args])
        return out.getvalue()

    def test_a_fleet_member_gets_no_macos_runner_hint(self) -> None:
        with mock.patch.object(cr, "xcode_present", return_value=True):
            plan = self.invoke("--manifest", self.manifest(), "--member", "mini-std")
            text = self.human("--manifest", self.manifest(), "--member", "mini-std")
        self.assertEqual((plan["routing"]["route"], plan["routing"]["rollback"]), ([], []))
        self.assertIn("pool picker", plan["routing"]["note"])
        self.assertNotIn("gh variable set MACOS_RUNNER", text)
        self.assertIn("routing: fleet member", text)
        self.assertIn("Never set MACOS_RUNNER_*", text)

    def test_a_trusted_member_prints_the_nightly_variables(self) -> None:
        manifest = json.loads(json.dumps(MANIFEST))
        manifest["hosts"]["mini-std"]["overrides"] = {"runner": {"trustedRef": "refs/heads/main",
                                                                 "trustedRepo": "manaflow-ai/cmux"}}
        path = self.state / "trusted.json"
        path.write_text(json.dumps(manifest))
        with mock.patch.object(cr, "xcode_present", return_value=True):
            plan = self.invoke("--manifest", os.fspath(path), "--member", "mini-std")
        self.assertEqual(plan["routing"]["route"], [
            "gh variable set CI_SEED_TRUSTED_POOL --body glaeda-trusted-std-xcode-26.6 --repo manaflow-ai/cmux",
            "gh variable set CI_NIGHTLY_TRUSTED_RUNNER --body glaeda-runner-mini-std-glaeda --repo manaflow-ai/cmux"])
        self.assertEqual(plan["routing"]["rollback"], ["gh variable delete CI_NIGHTLY_TRUSTED_RUNNER --repo manaflow-ai/cmux"])
        self.assertNotIn("MACOS_RUNNER_15 --body", json.dumps(plan["routing"]))

    def test_label_drift_against_the_receipt_and_github_is_reported(self) -> None:
        self.invoke("--apply", "--labels", "ram48")
        with mock.patch.object(cr, "xcode_present", return_value=True):
            plan = self.invoke("--manifest", self.manifest(), "--member", "mini-std", "--name", "mini-test-glaeda")
            text = self.human("--manifest", self.manifest(), "--member", "mini-std", "--name", "mini-test-glaeda")
        drift = self.by_kind(plan)["register"]["drift"]
        self.assertEqual({d["against"] for d in drift}, {"receipt", "github"})
        receipt_drift = next(d for d in drift if d["against"] == "receipt")
        self.assertEqual(receipt_drift["removed"], ["ram48"])
        self.assertIn("glaeda-runner-mini-test-glaeda", receipt_drift["added"])
        self.assertIn("registered labels differ from what this version would register: +glaeda-class-std", text)
        # GitHub lost a label behind the receipt's back: a plain re-run (labels from the receipt) still says so
        runners = json.loads((self.state / "runners.json").read_text())
        runners["runners"][0]["labels"] = [label for label in runners["runners"][0]["labels"] if label["name"] != "ram48"]
        (self.state / "runners.json").write_text(json.dumps(runners))
        again = self.by_kind(self.invoke())["register"]
        self.assertEqual(again["state"], "unchanged")
        self.assertEqual(again["drift"], [{"against": "github", "added": ["ram48"], "removed": []}])
        self.assertIn("registered labels differ", again["note"])
        self.assertTrue(all(e["argv"][:3] != ["api", "-X", "POST"] for e in self.log()[-6:] if e["tool"] == "gh"))

    def installed_release(self, labels: list[str], same_files: bool = False) -> Path:
        """An OTA release on this fake HOME whose plan registers `labels`; returns the log of its argv."""
        tag = "r-20260926-abcdefabcdef"
        root = self.home / ".local/state/glaeda/update"
        scripts = root / "generations" / tag / "glaeda/scripts"
        scripts.mkdir(parents=True)
        (root / "state.json").write_text(json.dumps({"current": tag, "previous": None, "quarantined": []}))
        seen = self.state / "release-plan.jsonl"
        if same_files:
            for name in cr.RUNNER_FILES:
                shutil.copy2(cr.SCRIPT_DIR / name, scripts / name)
            return seen
        make_executable(scripts / "glaeda-cmux-runner", f"""#!/usr/bin/env python3
import json, os, sys
with open({os.fspath(seen)!r}, "a") as f:
    f.write(json.dumps({{"argv": sys.argv[1:], "guard": os.environ.get("{cr.NO_RELEASE_CHECK}")}}) + "\\n")
print(json.dumps({{"actions": [{{"kind": "register", "labels": {labels!r}}}]}}))
""")
        return seen

    def test_a_copy_older_than_the_installed_release_blocks_and_shows_its_labels(self) -> None:
        seen = self.installed_release(["self-hosted", "macOS", "ARM64", "glaeda-mini", "glaeda-runner-new"])
        old = {"date": "2026-09-24", "source": "a" * 40, "tag": None, "via": "stamp by fleet"}
        with mock.patch.object(cr, "script_source", return_value=old):
            plan = self.invoke("--token-stdin", expect=0)
            self.assertFalse(plan["ready"])
            self.assertIn("scriptCopy", plan["preflight"]["blocking"])
            self.assertEqual(plan["scriptCopy"]["state"], "stale")
            self.assertEqual(plan["scriptCopy"]["labelDrift"],
                             {"against": "release", "added": ["glaeda-runner-new"], "removed": []})
            self.assertIn(".local/state/glaeda/update/generations/r-20260926-abcdefabcdef/glaeda/scripts/glaeda-cmux-runner",
                          plan["preflight"]["checks"]["scriptCopy"]["note"])
            text = self.human("--token-stdin")
            self.assertIn("the installed release would register +glaeda-runner-new against this copy", text)
            # the release's plan ran read-only, with the same arguments, and without re-checking itself
            calls = [json.loads(line) for line in seen.read_text().splitlines()]
            self.assertTrue(all("--apply" not in c["argv"] and c["guard"] == "1" for c in calls), calls)
            self.assertIn("--token-stdin", calls[0]["argv"])
            blocked = self.invoke("--apply", "--token-stdin", stdin=REG_TOKEN + "\n", expect=1)
            self.assertFalse([e for e in self.log() if e["tool"] == "config.sh"], "a stale copy registered")
            self.assertEqual(self.by_kind(blocked)["register"]["state"], "blocked")
            self.assertFalse(any(c["argv"].count("--apply") for c in
                                 (json.loads(line) for line in seen.read_text().splitlines())))
            forced = self.invoke("--apply", "--allow-stale")
            self.assertEqual(forced["preflight"]["checks"]["scriptCopy"]["level"], "warn")
            self.assertEqual(self.by_kind(forced)["verify"]["state"], "ok")

    def test_a_newer_or_identical_copy_is_not_stale(self) -> None:
        self.installed_release(["self-hosted"])
        newer = {"date": "2026-09-27", "source": "b" * 40, "tag": None, "via": "git"}
        with mock.patch.object(cr, "script_source", return_value=newer):
            plan = self.invoke()
        self.assertEqual(plan["scriptCopy"]["state"], "differs")
        self.assertEqual(plan["preflight"]["checks"]["scriptCopy"]["level"], "info")
        self.assertTrue(plan["ready"])
        # a copy of an older release is stale even from the same day
        other = {"date": "2026-09-26", "source": "c" * 40, "tag": "r-20260926-000000000000", "via": "release"}
        with mock.patch.object(cr, "script_source", return_value=other):
            self.assertEqual(self.invoke()["scriptCopy"]["state"], "stale")
        with mock.patch.object(cr, "script_source", return_value=None):  # unknown origin counts as older
            self.assertEqual(self.invoke()["scriptCopy"]["state"], "stale")

    def test_the_release_copy_itself_is_never_checked(self) -> None:
        shutil.rmtree(self.home / ".local", ignore_errors=True)
        self.installed_release([], same_files=True)
        self.assertNotIn("scriptCopy", self.invoke())

    def test_script_source_reads_a_stamp_a_release_or_git(self) -> None:
        base = Path(self.tmp.name)
        staged = base / "glaeda-runner/scripts"
        staged.mkdir(parents=True)
        (staged / cr.SOURCE_STAMP).write_text(json.dumps({"by": "fleet runner relabel", "date": "2026-09-26T10:00:00Z",
                                                          "source": "d" * 40}))
        self.assertEqual(cr.script_source(staged)["date"], "2026-09-26")
        gen = base / "generations/r-20260925-0123456789ab"
        (gen / "glaeda/scripts").mkdir(parents=True)
        (gen / "release.json").write_text(json.dumps({"tag": "r-20260925-0123456789ab", "source": "e" * 40}))
        self.assertEqual(cr.script_source(gen / "glaeda/scripts")["tag"], "r-20260925-0123456789ab")
        self.assertIsNone(cr.script_source(base / "nowhere"))

    def test_relabel_starts_a_stopped_agent_exactly_once(self) -> None:
        fake_pw = mock.Mock(pw_dir=os.fspath(self.home))
        with mock.patch.object(cr.pwd, "getpwuid", return_value=fake_pw):
            self.invoke("--apply", "--labels", "ram48")
            with mock.patch.object(cr, "xcode_present", return_value=True):
                receipt = self.invoke("--apply", "--manifest", self.manifest(), "--member", "mini-std",
                                      "--name", "mini-test-glaeda")
        self.assertEqual(self.by_kind(receipt)["register"]["state"], "updated")
        verbs = [e["argv"][0] for e in self.log() if e["tool"] == "launchctl"]
        self.assertEqual(verbs.count("bootstrap"), 2, verbs)  # install, then relabel; never a third

    def test_relabel_with_a_plist_change_restarts_on_the_new_plist(self) -> None:
        fake_pw = mock.Mock(pw_dir=os.fspath(self.home))
        with mock.patch.object(cr.pwd, "getpwuid", return_value=fake_pw):
            self.invoke("--apply", "--labels", "ram48")
            plist = self.home / "Library/LaunchAgents/com.teamleaderleo.glaeda.cmux-runner.plist"
            receipt_path = self.home / ".local/state/glaeda/cmux-runner/receipt.json"
            doc = json.loads(receipt_path.read_text())
            data = plist.read_bytes().replace(b"</dict>\n</plist>", b"<key>X</key><string>old</string></dict>\n</plist>")
            plist.write_bytes(data)  # an older plist this install wrote
            for act in doc["actions"]:
                if act.get("kind") == "agent":
                    act["ownedSha256"] = cr.sha256_bytes(data)
            receipt_path.write_text(json.dumps(doc))
            with mock.patch.object(cr, "xcode_present", return_value=True):
                receipt = self.invoke("--apply", "--manifest", self.manifest(), "--member", "mini-std",
                                      "--name", "mini-test-glaeda")
        self.assertEqual(self.by_kind(receipt)["agent"]["state"], "update")
        self.assertTrue(receipt["ready"], receipt["blocking"])
        verbs = [e["argv"][0] for e in self.log() if e["tool"] == "launchctl"
                 and e["argv"][0] not in {"print", "print-disabled", "enable"}]
        self.assertEqual(verbs[-3:], ["bootstrap", "bootout", "bootstrap"], verbs)

    def test_a_plist_change_never_boots_out_a_runner_with_a_job(self) -> None:
        fake_pw = mock.Mock(pw_dir=os.fspath(self.home))  # not a sandbox home, so launchctl runs
        patch = mock.patch.object(cr.pwd, "getpwuid", return_value=fake_pw)
        patch.start()
        self.addCleanup(patch.stop)
        self.invoke("--apply", "--labels", "ram48")
        plist = self.home / "Library/LaunchAgents/com.teamleaderleo.glaeda.cmux-runner.plist"
        receipt_path = self.home / ".local/state/glaeda/cmux-runner/receipt.json"
        doc = json.loads(receipt_path.read_text())
        data = plist.read_bytes().replace(b"</dict>\n</plist>", b"<key>X</key><string>old</string></dict>\n</plist>")
        plist.write_bytes(data)  # an older plist this install wrote, loaded and running a job
        for act in doc["actions"]:
            if act.get("kind") == "agent":
                act["ownedSha256"] = cr.sha256_bytes(data)
        receipt_path.write_text(json.dumps(doc))
        make_executable(self.launchctl, FAKE_LOG_HEADER + "argv = sys.argv[1:]\nlog('launchctl', argv)\nsys.exit(0)\n")
        with mock.patch.object(cr, "runner_busy", return_value=True):
            receipt = self.invoke("--apply", "--labels", "ram48", expect=1)
        agent = self.by_kind(receipt)["agent"]
        self.assertEqual(agent["state"], "failed")
        self.assertIn("re-run when idle", agent["note"])
        self.assertEqual(plist.read_bytes(), data)
        self.assertNotIn("bootout", [e["argv"][0] for e in self.log() if e["tool"] == "launchctl"])

    def test_relabel_without_a_token_blocks_and_changes_nothing(self) -> None:
        self.invoke("--apply", "--labels", "ram48")
        before = self.tree()
        real_which = cr.which
        with mock.patch.object(cr, "which", lambda name, extra=(): None if name == "gh" else real_which(name, extra)), \
                mock.patch.object(cr, "xcode_present", return_value=False):
            blocked = self.invoke("--apply", "--manifest", self.manifest(), "--member", "mini-std",
                                  "--name", "mini-test-glaeda", gh=False, expect=1)
        self.assertIn("--token-stdin", self.by_kind(blocked)["register"]["note"])
        after = self.tree()
        for tree in (before, after):
            tree.pop(".local/state/glaeda/cmux-runner/receipt.json", None)
        self.assertEqual(before, after)

    def test_interrupted_relabel_reregisters_its_own_name(self) -> None:
        self.invoke("--apply", "--labels", "ram48")
        (self.state / "fail-register").touch()
        real = cr.register

        def failing(ctx, act):  # config.sh fails after the local registration was cleared
            act["state"], act["note"] = "failed", "config.sh failed: network"
        with mock.patch.object(cr, "register", failing), mock.patch.object(cr, "xcode_present", return_value=False):
            self.invoke("--apply", "--manifest", self.manifest(), "--member", "mini-std",
                        "--name", "mini-test-glaeda", expect=1)
        runner = self.home / "actions-runner-glaeda"
        self.assertFalse((runner / ".runner").exists())
        with mock.patch.object(cr, "register", real), mock.patch.object(cr, "xcode_present", return_value=False):
            receipt = self.invoke("--apply", "--manifest", self.manifest(), "--member", "mini-std",
                                  "--name", "mini-test-glaeda")
        self.assertTrue(receipt["ready"], receipt["blocking"])
        self.assertIn("--replace", self.config_argvs()[-1])
        runners = json.loads((self.state / "runners.json").read_text())["runners"]
        self.assertEqual(len(runners), 1)

    def test_config_remove_failure_falls_back_to_api_delete(self) -> None:
        self.invoke("--apply")
        (self.state / "fail-config-remove").touch()
        receipt = self.invoke("--uninstall", "--apply")
        dereg = next(a for a in receipt["actions"] if a["kind"] == "deregister")
        self.assertIn("deleted by id", dereg["note"])
        self.assertTrue(any(e["tool"] == "gh" and "DELETE" in e["argv"] for e in self.log()))
        self.assertFalse((self.home / "actions-runner-glaeda").exists())


    # ------------------------------------------------------------ --token-stdin, no gh on the mini

    def test_token_stdin_without_gh_installs_and_uninstalls(self) -> None:
        real_which = cr.which
        with mock.patch.object(cr, "which", lambda name, extra=(): None if name == "gh" else real_which(name, extra)):
            plan = self.invoke(gh=False)
            self.assertEqual(self.by_kind(plan)["register"]["state"], "blocked")
            preview = self.invoke("--token-stdin", gh=False, stdin=REG_TOKEN + "\n")
            self.assertTrue(preview["ready"], preview["blocking"])
            self.assertEqual(self.by_kind(preview)["register"]["state"], "create")
            self.assertEqual(self.tree(), {})
            for entry in self.log():  # plans only read the public release API
                self.assertEqual(entry["tool"], "curl", entry)
                self.assertNotIn("-o", entry["argv"], entry)
            receipt = self.invoke("--apply", "--token-stdin", gh=False, stdin=REG_TOKEN + "\n")
            kinds = self.by_kind(receipt)
            self.assertTrue(receipt["ready"], receipt["blocking"])
            self.assertEqual(kinds["download"]["version"], VERSION)
            self.assertEqual(kinds["verify"]["state"], "ok", kinds["verify"])
            self.assertEqual(kinds["verify"]["runner"]["id"], 4242)
            self.assertFalse(any(e["tool"] == "gh" for e in self.log()))
            config = [e for e in self.log() if e["tool"] == "config.sh"][0]
            self.assertEqual(config["envToken"], REG_TOKEN)
            for entry in self.log():
                self.assertNotIn(REG_TOKEN, " ".join(entry["argv"]))
            self.assertNotIn(REG_TOKEN, self.last_output)
            for name, data in self.tree().items():
                self.assertNotIn(REG_TOKEN.encode(), data or b"", name)

            again = self.invoke("--apply", gh=False)  # an installed mini re-applies without gh or a token
            self.assertTrue(again["ready"], again["blocking"])
            self.assertEqual({a["state"] for a in again["actions"] if a["kind"] != "verify"}, {"unchanged"})

            before = self.tree()
            stuck = self.invoke("--uninstall", "--apply", gh=False, expect=1)  # no way to deregister: touch nothing
            self.assertFalse(stuck["ready"])
            self.assertTrue(all(a["state"] == "blocked" for a in stuck["actions"]), stuck["actions"])
            after = self.tree()
            after.pop(".local/state/glaeda/cmux-runner/uninstall-receipt.json")
            self.assertEqual(before, after)

            removed = self.invoke("--uninstall", "--apply", "--token-stdin", gh=False, stdin=REMOVE_TOKEN + "\n")
            self.assertTrue(next(a for a in removed["actions"] if a["kind"] == "deregister")["applied"])
            self.assertFalse((self.home / "actions-runner-glaeda").exists())
            self.assertNotIn(REMOVE_TOKEN, self.last_output)

    def test_xcode_app_check_warns_and_never_blocks(self) -> None:
        missing = self.invoke("--xcode-app", os.fspath(self.home / "Xcode_26.6.app"))
        self.assertEqual(missing["preflight"]["checks"]["xcodeApp"]["level"], "warn")
        self.assertNotIn("xcodeApp", missing["blocking"])
        real = self.home / "Real.app"
        real.mkdir()
        (self.home / "Link.app").symlink_to(real)
        linked = self.invoke("--xcode-app", os.fspath(self.home / "Link.app"))
        self.assertIn("symlink", linked["preflight"]["checks"]["xcodeApp"]["value"])
        self.assertNotIn("xcodeApp", self.invoke()["preflight"]["checks"])

    def test_token_stdin_apply_needs_a_token(self) -> None:
        for args, stdin in ((("--apply", "--token-stdin"), ""),
                            (("--apply", "--token-stdin"), "not a token; rm -rf /\n")):
            with self.subTest(args=args, stdin=stdin[:8]), contextlib.redirect_stderr(io.StringIO()) as err, \
                    mock.patch.object(sys, "stdin", io.StringIO(stdin)):
                self.assertEqual(cr.main(["--gh", os.fspath(self.gh), *args]), 2)
                self.assertNotIn("rm -rf", err.getvalue())
        self.assertEqual(self.tree(), {})
        self.assertEqual(self.log(), [])


MANIFEST = {
    "defaults": {"xcode": {"apps": [{"path": "/Applications/Xcode_26.6.app", "version": "26.6", "build": "17F113"}]}},
    "hosts": {
        "mini-std": {"class": "std", "availability": "dedicated", "roles": ["dev-builds", "ci-runner"],
                     "hardware": "m4pro-48"},
        "mini-light": {"class": "light", "availability": "opportunistic", "roles": ["ci-runner"], "owner": "x",
                       "hardware": "m4-16"},
        "no-hardware": {"class": "std", "availability": "dedicated", "roles": ["ci-runner"]},
        "mini-no-role": {"class": "std", "availability": "dedicated", "roles": ["dev-builds"]},
        "laptop": {"class": "dev", "availability": "opportunistic", "roles": ["ci-runner"]},
        "borrowed": {"class": "borrowed", "availability": "opportunistic", "roles": ["ci-runner"]},
        "old-shape": {"class": "m4pro-48", "roles": ["ci-runner"]},
        "bad-avail": {"class": "std", "availability": "sometimes", "roles": ["ci-runner"]},
        "override": {"class": "std", "availability": "dedicated", "roles": ["ci-runner"], "hardware": "m4pro-48",
                     "overrides": {"xcode": {"apps": [{"path": "/Applications/Xcode.app", "version": "26.3",
                                                       "build": "17C529"}]}}},
    },
}


class ManifestLabelsTest(unittest.TestCase):
    def test_runner_capacity_per_class(self) -> None:
        with mock.patch.object(cr, "xcode_present", return_value=True):
            std, _ = cr.member_labels(MANIFEST, "mini-std")
            light, _ = cr.member_labels(MANIFEST, "mini-light")
            self.assertEqual((std["runners"], std["capacityUnits"]), (4, 4))
            self.assertEqual((light["runners"], light["capacityUnits"]), (2, 2))
            manifest = json.loads(json.dumps(MANIFEST))
            manifest["defaults"]["runner"] = {"classes": {"std": {"runners": 3, "capacityUnits": 6}}}
            manifest["hosts"]["override"]["overrides"]["runner"] = {"classes": {"std": {"runners": 1}}}
            self.assertEqual(cr.member_labels(manifest, "mini-std")[0]["runners"], 3)
            self.assertEqual(cr.member_labels(manifest, "override")[0]["runners"], 1)
            self.assertEqual(cr.member_labels(manifest, "override")[0]["capacityUnits"], 6)
            self.assertEqual(cr.member_labels(manifest, "mini-std")[0]["compileSlots"], 1)
            self.assertEqual(cr.member_labels(manifest, "mini-std")[0]["canonicalRoots"], 1)
            self.assertEqual(cr.member_labels(manifest, "mini-std")[0]["rootPools"], ["glaeda-root-std-xcode-26.6"])
            self.assertEqual(cr.member_labels(manifest, "mini-std")[0]["sidePools"], ["glaeda-side-std-xcode-26.6"])
            manifest["defaults"]["runner"]["classes"]["std"]["compileSlots"] = 2
            self.assertIn("compileSlots 2 needs canonicalRoots 2", cr.member_labels(manifest, "mini-std")[1])
            manifest["defaults"]["runner"]["classes"]["std"]["canonicalRoots"] = 2
            self.assertEqual(cr.member_labels(manifest, "mini-std")[0]["compileSlots"], 2)
            self.assertEqual(cr.member_labels(manifest, "mini-std")[0]["canonicalRoots"], 2)
            self.assertEqual(cr.member_labels(manifest, "mini-std")[0]["guiPools"], [], "no gui runner by default")
            manifest["defaults"]["runner"]["classes"]["std"]["guiRunners"] = 1
            self.assertEqual(cr.member_labels(manifest, "mini-std")[0]["guiPools"], ["glaeda-gui-std-xcode-26.6"])
            manifest["defaults"]["runner"]["classes"]["std"]["canonicalRoots"] = 3  # 3 runners, all roots
            self.assertIn("guiRunners must be 0 or 1", cr.member_labels(manifest, "mini-std")[1])
            for bad in (2, -1, True, "1"):
                manifest["defaults"]["runner"]["classes"]["std"].update(canonicalRoots=2, guiRunners=bad)
                self.assertIn("guiRunners must be 0 or 1", cr.member_labels(manifest, "mini-std")[1], bad)
            del manifest["defaults"]["runner"]["classes"]["std"]["guiRunners"]
            manifest["defaults"]["runner"]["classes"]["std"]["canonicalRoots"] = 4  # 3 runners
            self.assertIn("canonicalRoots must be 1 to 3", cr.member_labels(manifest, "mini-std")[1])
            del manifest["defaults"]["runner"]["classes"]["std"]["canonicalRoots"]
            manifest["defaults"]["runner"]["classes"]["std"]["compileSlots"] = 4  # 6 units hold 3 compiles
            self.assertIn("compileSlots must be 1 to 3", cr.member_labels(manifest, "mini-std")[1])
            for bad in ({"runners": 0}, {"runners": True}, {"capacityUnits": 1}, "four"):
                with self.subTest(bad=bad):
                    manifest["defaults"]["runner"] = {"classes": {"std": bad}}
                    member, why = cr.member_labels(manifest, "mini-std")
                    self.assertIsNone(member)
                    self.assertIn("runner.classes.std", why)

    def test_trusted_ref_moves_the_member_to_its_own_pool(self) -> None:
        with mock.patch.object(cr, "xcode_present", return_value=True):
            manifest = json.loads(json.dumps(MANIFEST))
            trusted = {"trustedRef": "refs/heads/main", "trustedRepo": "manaflow-ai/cmux"}
            manifest["hosts"]["mini-std"].setdefault("overrides", {})["runner"] = dict(trusted)
            member, why = cr.member_labels(manifest, "mini-std")
            self.assertIsNone(why)
            self.assertEqual(member["pools"], ["glaeda-trusted-std-xcode-26.6"])
            self.assertIn("glaeda-trusted", member["labels"])
            self.assertNotIn("glaeda-std-xcode-26.6", member["labels"], "a PR run must never route here")
            self.assertEqual(member["trustedRef"], "refs/heads/main")
            self.assertIsNone(cr.member_labels(MANIFEST, "mini-std")[0]["trustedRef"])
            for bad in ({**trusted, "trustedRef": "main"}, {**trusted, "trustedRef": "refs/pull/1/merge"},
                        {**trusted, "trustedRef": "refs/heads/a b"}, {**trusted, "trustedRef": 7},
                        {"trustedRef": "refs/heads/main"}, {"trustedRepo": "manaflow-ai/cmux"},
                        {**trusted, "trustedRepo": "cmux"}):
                with self.subTest(bad=bad):
                    manifest["hosts"]["mini-std"]["overrides"]["runner"] = bad
                    self.assertIn("trustedRef", cr.member_labels(manifest, "mini-std")[1])

    def test_ios_sim_label_needs_the_manifest_and_a_runtime(self) -> None:
        def rt(build: str, available: bool = True, platform: str = "iOS") -> dict:
            return {"platform": platform, "version": "26.5", "buildversion": build, "isAvailable": available}
        with mock.patch.object(cr, "xcode_present", return_value=True):
            manifest = json.loads(json.dumps(MANIFEST))
            self.assertNotIn("glaeda-ios-sim", cr.member_labels(manifest, "mini-std")[0]["labels"])
            manifest["hosts"]["mini-std"]["roles"] = [*manifest["hosts"]["mini-std"]["roles"], "ios-simulators"]
            runtimes = {"runtimes": [rt("23F77")]}
            with mock.patch.object(cr, "run", return_value=(0, "warning: noise\n" + json.dumps(runtimes))):
                self.assertNotIn("glaeda-ios-sim", cr.member_labels(manifest, "mini-std")[0]["labels"],
                                 "no declared runtime, no label")
                manifest["defaults"]["ios_simulator"] = {"runtimes": ["23F77"], "exclusive": True}
                member, _ = cr.member_labels(manifest, "mini-std")
                self.assertIn("glaeda-ios-sim", member["labels"])
                self.assertTrue(member["iosSim"])
            for label, found in (("none", {"runtimes": []}),
                                 ("26.3.1 only", {"runtimes": [rt("23D8133")]}),
                                 ("unavailable", {"runtimes": [rt("23F77", available=False)]}),
                                 ("watchOS", {"runtimes": [rt("23F77", platform="watchOS")]})):
                with self.subTest(label), mock.patch.object(cr, "run", return_value=(0, json.dumps(found))):
                    self.assertNotIn("glaeda-ios-sim", cr.member_labels(manifest, "mini-std")[0]["labels"])
            manifest["defaults"]["ios_simulator"]["runtimes"] = ["23F77", "24A5390f"]
            with mock.patch.object(cr, "run", return_value=(0, json.dumps(runtimes))):
                self.assertNotIn("glaeda-ios-sim", cr.member_labels(manifest, "mini-std")[0]["labels"],
                                 "every declared runtime must be present")
            manifest["defaults"]["ios_simulator"]["runtimes"] = ["23F77"]
            with mock.patch.object(cr, "run", return_value=(72, "xcrun: error")) as run:
                self.assertNotIn("glaeda-ios-sim", cr.member_labels(manifest, "mini-std")[0]["labels"])
                self.assertIn("glaeda-ios-sim", cr.member_labels(manifest, "mini-std", ["glaeda-ios-sim"])[0]["labels"],
                              "a simctl hiccup keeps the label the runner registered with")
                self.assertTrue(run.call_args.kwargs["env"]["DEVELOPER_DIR"].endswith("/Contents/Developer"))
            with mock.patch.object(cr, "run", return_value=(0, json.dumps({"runtimes": []}))):
                self.assertNotIn("glaeda-ios-sim", cr.member_labels(manifest, "mini-std", ["glaeda-ios-sim"])[0]["labels"],
                                 "a clean answer with no runtime drops it")

    def test_member_labels_table(self) -> None:
        with mock.patch.object(cr, "xcode_present", return_value=True):
            std, _ = cr.member_labels(MANIFEST, "mini-std")
            self.assertEqual(std["labels"], ["glaeda-mini", "glaeda-class-std", "glaeda-dedicated", "xcode-26.6",
                                             "glaeda-std-xcode-26.6"])
            light, _ = cr.member_labels(MANIFEST, "mini-light")
            self.assertEqual(light["labels"], ["glaeda-mini", "glaeda-class-light", "glaeda-opportunistic", "xcode-26.6"])
            self.assertEqual(cr.member_labels(MANIFEST, "override")[0]["labels"][-2:],
                             ["xcode-26.3", "glaeda-std-xcode-26.3"])
        with mock.patch.object(cr, "xcode_present", return_value=False):
            self.assertEqual(cr.member_labels(MANIFEST, "mini-std")[0]["labels"],
                             ["glaeda-mini", "glaeda-class-std", "glaeda-dedicated"])
        for name, why in (("mini-no-role", "ci-runner"), ("laptop", "never runs"), ("borrowed", "never runs"),
                          ("old-shape", "m4pro-48"), ("bad-avail", "availability"), ("absent", "not a member"),
                          ("no-hardware", "no hardware class")):
            with self.subTest(name):
                member, reason = cr.member_labels(MANIFEST, name)
                self.assertIsNone(member)
                self.assertIn(why, reason)

    def test_xcode_present_needs_the_exact_build(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            app = Path(tmp) / "Xcode_26.6.app"
            app.mkdir()
            for out, want in (("Xcode 26.6\nBuild version 17F113", True), ("Xcode 26.6\nBuild version 17F1134", False),
                              ("Xcode 26.6\nBuild version 17F11", False), ("Xcode 26.3\nBuild version 17F113", False)):
                with self.subTest(out=out), mock.patch.object(cr, "run", return_value=(0, out)):
                    got = cr.xcode_present({"path": os.fspath(app), "version": "26.6", "build": "17F113"})
                    self.assertEqual(got, want)
            link = Path(tmp) / "Link.app"
            link.symlink_to(app)
            with mock.patch.object(cr, "run", return_value=(0, "Xcode 26.6\nBuild version 17F113")):
                self.assertFalse(cr.xcode_present({"path": os.fspath(link), "version": "26.6", "build": "17F113"}))

    def test_malformed_manifest_is_refused_not_a_traceback(self) -> None:
        for manifest in ({"hosts": {"m": {"class": "std", "availability": "dedicated", "roles": ["ci-runner"],
                                          "overrides": ["x"]}}},
                         {"defaults": {"xcode": "26.6"},
                          "hosts": {"m": {"class": "std", "availability": "dedicated", "roles": ["ci-runner"]}}},
                         {"hosts": {"m": "std"}}, {"hosts": []}, []):
            with self.subTest(manifest=manifest):
                member, why = cr.member_labels(manifest, "m")
                if member is not None:  # a non-object xcode block just means no Xcode labels
                    self.assertEqual(member["labels"], ["glaeda-mini", "glaeda-class-std", "glaeda-dedicated"])
                else:
                    self.assertTrue(why)


fleet_labels = load("glaeda_fleet_labels_under_test", ROOT / "scripts" / "glaeda_fleet_labels.py")


class FleetLabelsModuleTest(unittest.TestCase):
    """The shared rule glaeda-mini-fleet imports: pin its strings and its declared/conforming views."""

    def test_pool_label_string(self) -> None:
        self.assertEqual(fleet_labels.pool_label("std", "26.6"), "glaeda-std-xcode-26.6")
        self.assertEqual(fleet_labels.pool_label("light", "26.6"), "glaeda-light-xcode-26.6")

    def test_runner_and_module_agree(self) -> None:
        with mock.patch.object(cr, "xcode_present", return_value=True):
            for member in MANIFEST["hosts"]:
                with self.subTest(member):
                    self.assertEqual(cr.member_labels(MANIFEST, member),
                                     fleet_labels.member_labels(MANIFEST, member, lambda app: True))

    def test_declared_and_conforming_pools(self) -> None:
        declared = fleet_labels.declared_pools(MANIFEST)
        self.assertEqual(declared, {"glaeda-std-xcode-26.3": ["override"], "glaeda-std-xcode-26.6": ["mini-std"]})
        conforming = fleet_labels.declared_pools(MANIFEST, lambda member, app: member != "mini-std")
        self.assertEqual(conforming, {"glaeda-std-xcode-26.3": ["override"]})
        self.assertEqual(fleet_labels.declared_pools({"hosts": []}), {})

    def test_runner_without_the_module_explains(self) -> None:
        with tempfile.TemporaryDirectory() as tmp, mock.patch.object(cr, "SCRIPT_DIR", Path(tmp)):
            member, why = cr.member_labels(MANIFEST, "mini-std")
        self.assertIsNone(member)
        self.assertIn("glaeda_fleet_labels.py must sit next to this script", why)


class ConsoleTest(unittest.TestCase):
    ON = '"kCGSSessionOnConsoleKey"=Yes,"kCGSessionLoginDoneKey"=Yes,"kCGSSessionUserNameKey"="cmux"'

    def parse(self, *rows: str) -> object:
        return hook.parse_console_users("\n".join(["+-o Root  <class IORegistryEntry>", "    {", *rows, "    }"]))

    def test_parses_the_console_session(self) -> None:
        on = self.ON
        cases = {
            "unlocked": ((f'      "IOConsoleUsers" = ({{{on},"kCGSSessionIDKey"=257}})',), ("unlocked", "cmux")),
            "screen-locked": ((f'      "IOConsoleUsers" = ({{{on},"CGSSessionScreenIsLocked"=Yes}})',),
                              ("locked", "cmux")),
            "console locked": (('      "IOConsoleLocked" = Yes', f'      "IOConsoleUsers" = ({{{on}}})'),
                               ("locked", "cmux")),
            "login window": (('      "IOConsoleLocked" = Yes',), ("no_user", None)),
            "no sessions": (('      "IOConsoleUsers" = ()',), ("no_user", None)),
            "a switched-out session only": (
                ('      "IOConsoleUsers" = ({"kCGSSessionOnConsoleKey"=No,"kCGSSessionUserNameKey"="cmux"})',),
                ("no_user", None)),
            "login in progress": ((f'      "IOConsoleUsers" = ({{{on.replace("LoginDoneKey\"=Yes", "LoginDoneKey\"=No")}}})',),
                                  ("no_user", None)),
            "the on-console one of two": (
                ('      "IOConsoleUsers" = ({"kCGSSessionOnConsoleKey"=No,"kCGSSessionUserNameKey"="leo"},'
                 f'{{{on},"CGSSessionScreenIsLocked"=Yes}})',), ("locked", "cmux")),
        }
        for name, (rows, want) in cases.items():
            with self.subTest(name):
                self.assertEqual(tuple(self.parse(*rows)), want)
        self.assertIsNone(hook.parse_console_users(""), "not the Root entry: unknown")
        self.assertIsNone(hook.parse_console_users("ioreg: error"))

    def test_refusal_follows_the_state_and_an_unreadable_one_changes_nothing(self) -> None:
        cases = ((hook.ConsoleState("unlocked", "cmux"), None), (None, None),
                 (hook.ConsoleState("locked", "cmux"), "console: the console session (cmux) is screen-locked"),
                 (hook.ConsoleState("no_user"), "console: no user is logged in at the console"))
        for state, want in cases:
            with self.subTest(state), mock.patch.object(hook, "console_state", return_value=state):
                why = hook.console_refusal()
                self.assertEqual(why if why is None else why[:len(want)], want)
        with mock.patch.object(hook, "CONSOLE_GATE", False), \
                mock.patch.object(hook, "console_state", return_value=hook.ConsoleState("locked", "cmux")):
            self.assertIsNone(hook.console_refusal())
        with mock.patch.object(hook, "IOREG", "/nonexistent/ioreg"):
            self.assertIsNone(hook.console_state())


class NoEmDashTest(unittest.TestCase):
    def test_no_em_dashes(self) -> None:
        for path in (ROOT / "scripts/glaeda-cmux-runner", HOOK, Path(__file__), ROOT / "docs/CMUX_MINI_RUNNER.md",
                     ROOT / "scripts/glaeda_fleet_labels.py", ROOT / "scripts/glaeda_reservation.py"):
            self.assertNotIn(chr(0x2014), path.read_text(encoding="utf-8"), path)


class JobTelemetryTest(unittest.TestCase):
    """The per-job host sampler: attribution, verdicts, the bounded log, and the fork/finish handshake."""

    def setUp(self) -> None:
        self.hook = load("glaeda_cmux_runner_hook_telemetry", HOOK)
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self.tmp.name)

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def test_classify_splits_this_job_other_jobs_and_outside(self) -> None:
        rows = [
            (100, 1, 1.0, "cmux", "/Users/cmux/actions-runner-glaeda/bin/Runner.Worker"),
            (101, 100, 700.0, "cmux", "/Applications/Xcode.app/usr/bin/swift-frontend"),
            (200, 1, 0.5, "cmux", "/Users/cmux/actions-runner-glaeda-2/bin/Runner.Worker"),
            (201, 200, 300.0, "cmux", "/usr/local/bin/node"),
            (300, 1, 450.0, "cmux", "/opt/homebrew/bin/zig"),
            (301, 1, 50.0, "root", "/usr/libexec/mds_stores"),
            (400, 1, 90.0, "cmux", "/bin/ps"),
        ]
        job, others, outside, by_outside, by_runner = self.hook.classify(rows, 100, skip={400})
        self.assertAlmostEqual(job, 7.01)
        self.assertAlmostEqual(others, 3.005)
        self.assertAlmostEqual(outside, 5.0)
        self.assertEqual(by_runner, {"actions-runner-glaeda-2": 3.005})
        self.assertEqual(set(by_outside), {"zig (cmux)", "mds_stores (root)"})
        self.assertFalse(any("/" in key for key in by_outside), "no paths leave the host")

    def test_summary_flags_outside_cpu_not_its_own_load(self) -> None:
        samples = self.hook.JobSamples({"job": "macos-compile-admission"}, 14, 1000.0)
        for _ in range(6):
            samples.add(30.0, (8.0, 0.0, 5.0, {"zig (cmux)": 5.0}, {}), 10.0)
        record = samples.summary(1060.0)
        self.assertEqual(record["schema"], "glaeda-cmux-job/v1")
        self.assertEqual(record["verdict"], "contended")
        self.assertEqual(record["cores"], {"job": 8.0, "other_runner_jobs": 0.0, "outside": 5.0})
        self.assertEqual(record["top_outside"], [{"process": "zig (cmux)", "core_seconds": 300}])
        self.assertEqual(len(record["reasons"]), 1)
        self.assertIn("zig (cmux)", record["reasons"][0])

        busy = self.hook.JobSamples({}, 14, 0.0)
        busy.add(40.0, (13.0, 0.0, 0.5, {}, {}), 10.0)
        self.assertEqual(busy.summary(10.0)["verdict"], "clear", "a compile's own load is not contention")
        self.assertEqual(busy.summary(10.0)["load"]["mean"], 40.0)

        quiet = self.hook.JobSamples({}, 14, 0.0)
        quiet.add(9.0, (12.0, 1.0, 0.5, {"WindowServer (_windowserver)": 0.5}, {}), 10.0)
        self.assertEqual(quiet.summary(10.0)["verdict"], "clear")

    def test_job_log_keeps_its_newest_half(self) -> None:
        log = self.dir / "Logs" / "jobs.jsonl"
        with mock.patch.object(self.hook, "JOB_LOG_MAX_BYTES", 4096):
            for n in range(200):
                self.hook.append_job_record({"n": n, "pad": "x" * 40}, log)
        lines = log.read_text().splitlines()
        self.assertLessEqual(log.stat().st_size, 4096 + 200)
        self.assertEqual(json.loads(lines[-1])["n"], 199)
        self.assertTrue(all(json.loads(line) for line in lines), "every kept line is whole JSON")

    def test_job_log_cap_is_16_mib_and_trims_under_concurrent_writers(self) -> None:
        self.assertEqual(self.hook.JOB_LOG_MAX_BYTES, 16 * 1024 * 1024)
        log = self.dir / "Logs" / "jobs.jsonl"
        with mock.patch.object(self.hook, "JOB_LOG_MAX_BYTES", 8192):
            def write(w: int) -> None:
                for n in range(150):
                    self.hook.append_job_record({"w": w, "n": n, "pad": "x" * 40}, log)
            threads = [threading.Thread(target=write, args=(w,)) for w in range(4)]
            for thread in threads:
                thread.start()
            for thread in threads:
                thread.join()
        lines = log.read_text().splitlines()
        self.assertLessEqual(log.stat().st_size, 8192 + 200)
        records = [json.loads(line) for line in lines]  # every kept line is whole JSON
        self.assertEqual({r["n"] for r in records if r["n"] == 149}, {149}, "the newest lines survive a trim")

    def test_trim_renames_and_leaves_no_temp_file(self) -> None:
        log = self.dir / "Logs" / "jobs.jsonl"
        self.hook.append_job_record({"n": 0}, log)
        before = log.stat().st_ino
        with mock.patch.object(self.hook, "JOB_LOG_MAX_BYTES", 2048):
            for n in range(1, 80):
                self.hook.append_job_record({"n": n, "pad": "x" * 40}, log)
        self.assertNotEqual(log.stat().st_ino, before, "the trim replaces the file, never truncates it")
        self.assertEqual([p.name for p in log.parent.iterdir()], [log.name])
        self.assertEqual(json.loads(log.read_text().splitlines()[-1])["n"], 79)

    def test_job_started_line_skips_a_held_log_lock(self) -> None:
        log = self.dir / "jobs.jsonl"
        log.write_text("")
        with open(log, "a+b") as holder:
            fcntl.flock(holder, fcntl.LOCK_EX)
            start = time.monotonic()
            self.assertFalse(self.hook.append_job_record({"n": 1}, log, wait=False))
            self.assertLess(time.monotonic() - start, 1.0, "the log never delays a job")
            self.hook.job_event("started", {"job": "x"}, log)  # swallowed, not raised
        self.assertEqual(log.read_text(), "")
        self.assertTrue(self.hook.append_job_record({"n": 2}, log, wait=False))

    def test_oversized_record_stays_whole_json(self) -> None:
        log = self.dir / "jobs.jsonl"
        self.hook.append_job_record({"event": "completed", "decision": "d" * 9000, "job": "x"}, log)
        record = json.loads(log.read_text())
        self.assertEqual((record["job"], "decision" in record), ("x", False))

    def test_broken_roots_file_reads_as_none(self) -> None:
        self.hook.roots_file(self.dir).write_bytes(b"\xff\xfe root-1")
        self.assertEqual(self.hook.held_roots(self.dir), [])

    def test_completed_roots_come_from_while_the_worker_lived(self) -> None:
        watched = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(30)"])
        self.addCleanup(lambda: (watched.kill(), watched.wait()))
        state = self.dir / "state"
        log = self.dir / "jobs.jsonl"
        state.mkdir()
        self.hook.roots_file(state).write_text("root-1\n")
        self.hook.start_sampler(watched.pid, state, {"job": "x", "roots": ["root-1"]}, log, interval=0.2)
        deadline = time.monotonic() + 10
        while not self.hook.sampler_file(state).exists() and time.monotonic() < deadline:
            time.sleep(0.05)
        watched.kill()
        watched.wait()
        self.hook.roots_file(state).write_text("root-2\n")  # the next job on this runner, after the worker died
        self.hook.gui_file(state).write_text("gui\n")
        while (not log.exists() or not log.read_text()) and time.monotonic() < deadline:
            time.sleep(0.1)
        [record] = [json.loads(line) for line in log.read_text().splitlines()]
        self.assertEqual((record["roots"], record["gui"], "roots_admitted" in record), (["root-1"], False, False))

    def test_roots_released_after_the_pid_file_goes_are_not_read_as_empty(self) -> None:
        # finish_sampler removes the pid file, then job-completed removes .roots while the worker still lives
        watched = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(30)"])
        self.addCleanup(lambda: (watched.kill(), watched.wait()))
        state = self.dir / "state"
        log = self.dir / "jobs.jsonl"
        state.mkdir()
        self.hook.roots_file(state).write_text("root-1\n")
        self.hook.start_sampler(watched.pid, state, {"job": "x", "roots": ["root-1"]}, log, interval=0.2)
        deadline = time.monotonic() + 10
        while not self.hook.sampler_file(state).exists() and time.monotonic() < deadline:
            time.sleep(0.05)
        time.sleep(0.3)
        self.hook.sampler_file(state).unlink()
        self.hook.roots_file(state).unlink()
        while (not log.exists() or not log.read_text()) and time.monotonic() < deadline:
            time.sleep(0.1)
        [record] = [json.loads(line) for line in log.read_text().splitlines()]
        self.assertEqual((record["roots"], "roots_admitted" in record), (["root-1"], False))

    def test_completed_line_has_event_and_final_roots(self) -> None:
        watched = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(30)"])
        self.addCleanup(lambda: (watched.kill(), watched.wait()))
        state = self.dir / "state"
        log = self.dir / "jobs.jsonl"
        state.mkdir()
        self.hook.roots_file(state).write_text("root-1\n")
        self.hook.start_sampler(watched.pid, state, {"job": "x", "roots": ["root-1"], "decision": "d"}, log,
                                interval=0.2)
        deadline = time.monotonic() + 10
        while not self.hook.sampler_file(state).exists() and time.monotonic() < deadline:
            time.sleep(0.05)
        self.hook.roots_file(state).write_text("root-2\n")  # take-root --switch mid-job
        time.sleep(2.5)  # a sample takes about a second; the switch is read on the next pass while the job is ours
        self.assertEqual(self.hook.finish_sampler(state), "job telemetry recorded")
        [record] = [json.loads(line) for line in log.read_text().splitlines()]
        self.assertEqual((record["event"], record["roots"], record["roots_admitted"], record["decision"]),
                         ("completed", ["root-2"], ["root-1"], "d"))
        self.assertIsInstance(record["at"], int)

    def test_sampler_writes_one_record_when_finished(self) -> None:
        watched = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(30)"])
        self.addCleanup(lambda: (watched.kill(), watched.wait()))
        state = self.dir / "state"
        log = self.dir / "jobs.jsonl"
        note = self.hook.start_sampler(watched.pid, state, {"job": "claude-wrapper", "class": "light"}, log,
                                       interval=0.2)
        self.assertEqual(note, "job telemetry sampling")
        deadline = time.monotonic() + 10
        while not self.hook.sampler_file(state).exists() and time.monotonic() < deadline:
            time.sleep(0.05)
        time.sleep(0.6)
        self.assertEqual(self.hook.finish_sampler(state), "job telemetry recorded")
        records = [json.loads(line) for line in log.read_text().splitlines()]
        self.assertEqual(len(records), 1)
        self.assertEqual(records[0]["job"], "claude-wrapper")
        self.assertGreaterEqual(records[0]["samples"], 1)
        self.assertIn(records[0]["verdict"], ("clear", "contended"))
        self.assertFalse(self.hook.sampler_file(state).exists())

    def test_sampler_records_when_the_job_dies_first(self) -> None:
        watched = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(1)"])
        state = self.dir / "state"
        log = self.dir / "jobs.jsonl"
        self.hook.start_sampler(watched.pid, state, {"job": "x"}, log, interval=0.2)
        watched.wait()
        deadline = time.monotonic() + 10
        while (not log.exists() or not log.read_text() or self.hook.sampler_file(state).exists()) \
                and time.monotonic() < deadline:
            time.sleep(0.1)
        self.assertEqual(len(log.read_text().splitlines()), 1)
        self.assertEqual(self.hook.finish_sampler(state), "no job telemetry sampler")

    def test_host_io_reads_the_second_report(self) -> None:
        out = ("              disk0              disk4       cpu\n"
               "    KB/t  tps  MB/s     KB/t  tps  MB/s  us sy id\n"
               "   17.34 1233 20.88    49.77    0  0.00  19  9 72\n"
               "    9.36 7391 67.59     8.00   10  1.41  46 43 12\n")
        done = subprocess.CompletedProcess([], 0, stdout=out, stderr="")
        with mock.patch.object(self.hook.subprocess, "run", return_value=done):
            self.assertEqual(self.hook.host_io(), (69.0, 88.0))
        with mock.patch.object(self.hook.subprocess, "run", side_effect=FileNotFoundError):
            self.assertEqual(self.hook.host_io(), (None, None), "Linux has no BSD iostat")
        garbled = subprocess.CompletedProcess([], 0, stdout="usage: iostat\n", stderr="")
        with mock.patch.object(self.hook.subprocess, "run", return_value=garbled):
            self.assertEqual(self.hook.host_io(), (None, None))

    def test_run_queue_counts_running_and_blocked(self) -> None:
        done = subprocess.CompletedProcess([], 0, stdout="R+\nS\nSs\nU\nU+\nD\nZ\n", stderr="")
        with mock.patch.object(self.hook.subprocess, "run", return_value=done):
            self.assertEqual(self.hook.run_queue(), (1, 3))

    def test_saturation_is_contention_only_when_shared(self) -> None:
        shared = self.hook.JobSamples({}, 14, 0.0)
        shared.add(40.0, (0.3, 6.6, 1.1, {}, {}), 10.0, (20, 0), 120.0, 98.0)
        record = shared.summary(10.0)
        self.assertEqual(record["verdict"], "contended")
        self.assertIn("98% busy", record["reasons"][0])
        self.assertEqual(record["cpu_busy_pct"], 98.0)
        self.assertEqual(record["disk_mb_s"], {"mean": 120.0, "max": 120.0})

        alone = self.hook.JobSamples({}, 14, 0.0)
        alone.add(40.0, (12.0, 0.5, 0.5, {}, {}), 10.0, (20, 0), 120.0, 99.0)
        self.assertEqual(alone.summary(10.0)["verdict"], "clear", "a job saturating the host by itself is clear")

        waiting = self.hook.JobSamples({}, 14, 0.0)
        waiting.add(9.0, (4.0, 0.0, 0.2, {}, {}), 10.0, (2, 6), 900.0, 40.0)
        quiet = self.hook.JobSamples({}, 14, 0.0)
        quiet.add(3.0, (1.0, 0.0, 0.2, {}, {}), 10.0, (2, 3), 5.0, 20.0)
        self.assertEqual(quiet.summary(10.0)["verdict"], "clear", "an idle Mac shows a few U processes")
        record = waiting.summary(10.0)
        self.assertEqual(record["verdict"], "contended")
        self.assertIn("uninterruptible wait (disk or memory), disks 900 MB/s", record["reasons"][0])

        unknown = self.hook.JobSamples({}, 14, 0.0)
        unknown.add(9.0, (4.0, 0.0, 0.2, {}, {}), 10.0)
        record = unknown.summary(10.0)
        self.assertEqual((record["queue"], record["disk_mb_s"], record["cpu_busy_pct"]), (None, None, None))

    def test_sampler_file_never_looks_like_a_lock_holder(self) -> None:
        name = self.hook.sampler_file(self.dir).name
        self.assertFalse(name.startswith(self.hook.HOLDER_FILE))


if __name__ == "__main__":
    unittest.main()
