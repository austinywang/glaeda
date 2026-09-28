#!/usr/bin/env python3
"""Tests for scripts/glaeda-route. No network, no SSH, no GitHub: every source is a fixture."""
from __future__ import annotations

import contextlib
import importlib.machinery
import importlib.util
import io
import json
import os
import subprocess
import sys
import tempfile
import unittest
import urllib.error
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "glaeda-route"
loader = importlib.machinery.SourceFileLoader("glaeda_route", str(SCRIPT))
spec = importlib.util.spec_from_loader("glaeda_route", loader)
gr = importlib.util.module_from_spec(spec)
sys.modules["glaeda_route"] = gr
loader.exec_module(gr)

NOW = 1_800_000_000.0
STD = "glaeda-std-xcode-26.6"
LIGHT = "glaeda-light-xcode-26.6"
REPO = "manaflow-ai/cmux"
SECRET = "ghs_supersecretvalue123"


def pools_doc(observed_at=NOW - 10, std=None, light=None):
    std = std or {"declared": ["m1", "m2", "m3", "m4"], "conforming": ["m1", "m2", "m3"],
                  "reserved": ["m4"], "busy": []}
    light = light or {"declared": ["l1", "l2"], "conforming": ["l1", "l2"]}
    return {"schema": "glaeda-mini-fleet/v1/pools", "observed_at": observed_at,
            "pools": {STD: std, LIGHT: light, "glaeda-mini": {"declared": ["x"]}}}


def runner(name, label, status="online", busy=False):
    return {"name": f"{name}-glaeda", "status": status, "busy": busy,
            "labels": [{"name": n} for n in ("self-hosted", "macOS", "glaeda-mini", label)]}


def runners():
    return {"total_count": 6, "runners": [
        runner("m1", STD), runner("m2", STD, busy=True), runner("m3", STD, status="offline"),
        runner("m4", STD), runner("l1", LIGHT), runner("l2", LIGHT),
        {"name": "tart-x", "status": "online", "busy": False, "labels": [{"name": "tart-canary"}]},
    ]}


def state(**kw):
    return gr.build_state(kw.pop("pools", pools_doc()), kw.pop("runners", runners()), repo=REPO, now=NOW, **kw)


class FakeResponse:
    def __init__(self, status, body):
        self.status, self._body = status, body

    def read(self):
        return b"" if self._body is None else json.dumps(self._body).encode()

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        return False


class FakeGitHub:
    """A urlopen stand-in: routes (method, path) to canned answers and records every request."""

    def __init__(self, routes):
        self.routes, self.calls = routes, []

    def __call__(self, request, timeout=None):
        path = request.full_url.removeprefix(gr.API)
        body = json.loads(request.data) if request.data else None
        self.calls.append((request.get_method(), path, body, dict(request.header_items())))
        for (method, prefix), answer in self.routes.items():
            if method == request.get_method() and path.startswith(prefix):
                status, data = answer(body) if callable(answer) else answer
                if status >= 400:
                    raise urllib.error.HTTPError(request.full_url, status, "err", {}, io.BytesIO(json.dumps(data).encode()))
                return FakeResponse(status, data)
        raise AssertionError(f"unexpected {request.get_method()} {path}")


def run_main(argv, env=None, opener=None):
    out, err = io.StringIO(), io.StringIO()
    with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
        code = gr.main(argv, env=env or {}, now=lambda: NOW, opener=opener)
    return code, out.getvalue(), err.getvalue()


class StateTests(unittest.TestCase):
    def test_counts_join_fleet_and_github(self):
        doc = state()
        self.assertEqual(doc["schema"], gr.STATE_SCHEMA)
        self.assertEqual(doc["order"], [STD, LIGHT])
        std = doc["pools"][STD]
        # m1 idle; m2 busy on GitHub; m3 offline; m4 reserved by the fleet.
        self.assertEqual((std["declared"], std["conforming"], std["reserved"], std["runners"],
                          std["online"], std["busy"], std["idle"]), (4, 3, 1, 4, 3, 1, 1))
        self.assertEqual(doc["pools"][LIGHT]["idle"], 2)
        self.assertEqual((std["class"], std["xcode"], std["rank"]), ("std", "26.6", 0))

    def test_no_host_names_leave_the_document(self):
        text = json.dumps(state())
        for name in ("m1", "m2", "l1", "tart"):
            self.assertNotIn(f'"{name}', text)

    def test_locked_or_reserved_member_is_never_idle(self):
        doc = state(pools=pools_doc(std={"declared": ["m1"], "conforming": ["m1"], "busy": ["m1"]}))
        self.assertEqual(doc["pools"][STD]["idle"], 0)
        self.assertEqual(doc["pools"][STD]["locked"], 1)

    def test_runner_without_pool_label_or_unknown_member_is_not_idle(self):
        rows = {"runners": [runner("m1", "glaeda-mini"), runner("stranger", STD)]}
        doc = state(runners=rows)
        self.assertEqual(doc["pools"][STD]["idle"], 0)
        self.assertEqual(doc["pools"][STD]["runners"], 1)

    def test_observed_at_is_the_oldest_input(self):
        doc = state(runners_observed_at=NOW - 5)
        self.assertEqual(doc["observed_at"], gr.utc(NOW - 10))
        doc = state(pools=pools_doc(observed_at=None))
        self.assertEqual(doc["observed_at"], gr.utc(NOW))

    def test_light_before_std_is_reordered_and_xl_is_not_routed(self):
        doc = state(pools={"observed_at": NOW, "pools": {LIGHT: {}, STD: {}, "glaeda-xl-xcode-26.6": {}}})
        self.assertEqual(doc["order"], [STD, LIGHT])

    def test_newer_xcode_sorts_first_within_a_class(self):
        doc = state(pools={"observed_at": NOW, "pools": {"glaeda-std-xcode-26.4": {}, STD: {}}})
        self.assertEqual(doc["order"], [STD, "glaeda-std-xcode-26.4"])

    def test_runner_in_two_pools_is_idle_in_one_only(self):
        both = {"name": "m1-glaeda", "status": "online", "busy": False,
                "labels": [{"name": STD}, {"name": "glaeda-std-xcode-26.4"}]}
        pools = {"observed_at": NOW, "pools": {STD: {"conforming": ["m1"]},
                                               "glaeda-std-xcode-26.4": {"conforming": ["m1"]}}}
        doc = gr.build_state(pools, {"runners": [both]}, repo=REPO, now=NOW)
        self.assertEqual((doc["pools"][STD]["idle"], doc["pools"]["glaeda-std-xcode-26.4"]["idle"]), (1, 0))

    def test_malformed_inputs_fail(self):
        with self.assertRaises(gr.Failure):
            state(pools={"pools": []})
        with self.assertRaises(gr.Failure):
            state(runners="nope")


class ValidateTests(unittest.TestCase):
    def check(self, doc, **kw):
        return gr.validate_state(doc, now=kw.pop("now", NOW), repo=kw.pop("repo", REPO), **kw)

    def test_fresh_document_is_usable(self):
        usable, reason = self.check(state())
        self.assertTrue(usable, reason)

    def test_stale_missing_foreign_future_and_malformed_are_not(self):
        doc = state()
        self.assertFalse(self.check(doc, now=NOW + 61)[0])
        self.assertTrue(self.check(doc, now=NOW + 49)[0])
        self.assertFalse(self.check(None)[0])
        self.assertFalse(self.check("")[0])
        self.assertFalse(self.check(doc, repo="someone/else")[0])
        self.assertFalse(self.check(doc, now=NOW - 120)[0])
        for mutate in (
            lambda d: d.update(schema="glaeda-pool-state/v0"),
            lambda d: d.update(generated_at="2027-01-15"),
            lambda d: d.update(observed_at=None),
            lambda d: d.update(order=[STD]),
            lambda d: d.update(order=[STD, 3]),
            lambda d: d["pools"][STD].update(idle=-1),
            lambda d: d["pools"][STD].update(idle=True),
            lambda d: d["pools"][STD].update(idle=9),
            lambda d: d["pools"].update({"blacksmith-6vcpu-macos-26": d["pools"].pop(LIGHT)}),
        ):
            bad = json.loads(json.dumps(doc))
            mutate(bad)
            if "order" in bad and isinstance(bad.get("pools"), dict) and "blacksmith-6vcpu-macos-26" in bad["pools"]:
                bad["order"] = sorted(bad["pools"])
            self.assertFalse(self.check(bad)[0], bad)

    def test_check_command_exit_codes(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "state.json"
            path.write_text(json.dumps(state()))
            code, out, _ = run_main(["check", "--state", str(path), "--repo", REPO])
            self.assertEqual(code, 0, out)
            path.write_text(json.dumps({**state(), "observed_at": gr.utc(NOW - 3600)}))
            code, out, _ = run_main(["check", "--state", str(path)])
            self.assertEqual(code, 1)
            self.assertIn("stale", out)


class PublishTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self.tmp.name)
        (self.dir / "pools.json").write_text(json.dumps(pools_doc()))
        (self.dir / "runners.json").write_text(json.dumps(runners()))

    def tearDown(self):
        self.tmp.cleanup()

    def args(self, *extra):
        return ["publish", "--pools", str(self.dir / "pools.json"), *extra]

    def test_dry_run_writes_nothing(self):
        api = FakeGitHub({("GET", f"/repos/{REPO}/actions/runners"): (200, runners())})
        code, out, _ = run_main(self.args(), env={"GLAEDA_ROUTE_TOKEN": SECRET}, opener=api)
        self.assertEqual(code, 0)
        self.assertIn("would write GLAEDA_POOL_STATE", out)
        self.assertEqual([c[0] for c in api.calls], ["GET"])

    def test_publish_updates_variable_with_compact_document(self):
        api = FakeGitHub({("GET", f"/repos/{REPO}/actions/runners"): (200, runners()),
                          ("PATCH", f"/repos/{REPO}/actions/variables/GLAEDA_POOL_STATE"): (204, None)})
        code, out, err = run_main(self.args("--yes"), env={"GLAEDA_ROUTE_TOKEN": SECRET}, opener=api)
        self.assertEqual(code, 0, err)
        method, _, body, headers = api.calls[-1]
        self.assertEqual(method, "PATCH")
        doc = json.loads(body["value"])
        self.assertTrue(gr.validate_state(doc, now=NOW, repo=REPO)[0])
        self.assertEqual(headers["Authorization"], f"Bearer {SECRET}")
        self.assertNotIn(SECRET, out + err)
        self.assertIn("updated GLAEDA_POOL_STATE", out)

    def test_publish_creates_missing_variable(self):
        api = FakeGitHub({("PATCH", f"/repos/{REPO}/actions/variables/"): (404, {"message": "Not Found"}),
                          ("POST", f"/repos/{REPO}/actions/variables"): (201, None)})
        code, out, err = run_main(self.args("--runners", str(self.dir / "runners.json"), "--yes"),
                                  env={"GLAEDA_ROUTE_TOKEN": SECRET}, opener=api)
        self.assertEqual(code, 0, err)
        self.assertIn("created", out)

    def test_api_error_exits_2_without_leaking_the_token(self):
        api = FakeGitHub({("GET", f"/repos/{REPO}/actions/runners"): (403, {"message": "Resource not accessible"})})
        code, out, err = run_main(self.args("--yes"), env={"GLAEDA_ROUTE_TOKEN": SECRET}, opener=api)
        self.assertEqual(code, 2)
        self.assertIn("403", err)
        self.assertNotIn(SECRET, out + err)

    def test_missing_credentials_fail_closed(self):
        code, _, err = run_main(self.args("--yes"))
        self.assertEqual(code, 2)
        self.assertIn("no credentials", err)

    def test_token_file_must_be_private(self):
        path = self.dir / "token"
        path.write_text(SECRET + "\n")
        os.chmod(path, 0o644)
        code, _, err = run_main(self.args("--token-file", str(path), "--yes"))
        self.assertEqual(code, 2)
        self.assertIn("chmod 600", err)
        os.chmod(path, 0o600)
        api = FakeGitHub({("GET", f"/repos/{REPO}/actions/runners"): (200, runners()),
                          ("PATCH", f"/repos/{REPO}/actions/variables/"): (204, None)})
        code, _, err = run_main(self.args("--token-file", str(path), "--yes"), opener=api)
        self.assertEqual(code, 0, err)
        self.assertEqual(api.calls[0][3]["Authorization"], f"Bearer {SECRET}")

    def test_runner_listing_pages(self):
        page = {"runners": [runner(f"m{i}", STD) for i in range(100)]}
        api = FakeGitHub({("GET", f"/repos/{REPO}/actions/runners?per_page=100&page=1"): (200, page),
                          ("GET", f"/repos/{REPO}/actions/runners?per_page=100&page=2"): (200, {"runners": []})})
        self.assertEqual(len(gr.GitHub(SECRET, api).runners(REPO)), 100)


class AppTokenTests(unittest.TestCase):
    def test_jwt_is_signed_by_openssl_with_the_key_path_only(self):
        with tempfile.TemporaryDirectory() as tmp:
            key = Path(tmp) / "app.pem"
            key.write_text("-----BEGIN PRIVATE KEY-----\nfake\n-----END PRIVATE KEY-----\n")
            os.chmod(key, 0o600)
            seen = {}

            def fake_run(argv, **kw):
                seen["argv"], seen["input"], seen["env"] = argv, kw["input"], kw["env"]
                return subprocess.CompletedProcess(argv, 0, b"signature", b"")

            jwt = gr.app_jwt("123", key, NOW, run=fake_run)
            self.assertEqual(seen["argv"][:4], [gr.OPENSSL, "dgst", "-sha256", "-sign"])
            self.assertNotIn("fake", " ".join(seen["argv"]))
            header, payload, sig = jwt.split(".")
            claims = json.loads(gr.base64.urlsafe_b64decode(payload + "=="))
            self.assertEqual((claims["iss"], claims["exp"] - claims["iat"]), ("123", 600))
            self.assertEqual(seen["input"], f"{header}.{payload}".encode())

            api = FakeGitHub({("GET", f"/repos/{REPO}/installation"): (200, {"id": 42}),
                              ("POST", "/app/installations/42/access_tokens"): (201, {"token": SECRET})})
            token = gr.installation_token("123", key, REPO, NOW, opener=api, run=fake_run)
            self.assertEqual(token, SECRET)
            self.assertEqual(api.calls[1][2], {"repositories": ["cmux"]})
            self.assertTrue(api.calls[0][3]["Authorization"].startswith("Bearer "))

    def test_failed_signature_is_a_failure(self):
        with tempfile.TemporaryDirectory() as tmp:
            key = Path(tmp) / "app.pem"
            key.write_text("x")
            os.chmod(key, 0o600)
            with self.assertRaises(gr.Failure):
                gr.app_jwt("1", key, NOW, run=lambda argv, **kw: subprocess.CompletedProcess(argv, 1, b"", b"bad"))



# ---------------------------------------------------------------- route, ledger, agent

LEDGER_REPO = "manaflow-ai/glaeda-route-state"
SHA = "a" * 40
OTHER_SHA = "b" * 40


def request(**kw):
    base = dict(repo=REPO, event="pull_request", head_repo=REPO, run_id=100, run_attempt=1, head_sha=SHA,
                kind="ci-macos", priority="pr", slots=3, xcode="/Applications/Xcode_26.6.app",
                default="blacksmith-6vcpu-macos-26")
    base.update(kw)
    return gr.Request(**base)


def idle_state(std_idle=5, light_idle=2, at=NOW):
    doc = gr.build_state(pools_doc(observed_at=at), runners(), repo=REPO, now=at)
    doc["pools"][STD].update(online=max(11, std_idle), idle=std_idle)
    doc["pools"][LIGHT].update(online=max(2, light_idle), idle=light_idle)
    return doc


def reservation(pool=STD, slots=3, started=0, state="held", priority="pr", run_id=7, created=NOW - 10,
                hold=NOW + 600, head=SHA, **kw):
    return {"id": f"{REPO}#{run_id}.1/ci-macos", "repo": REPO, "run_id": run_id, "run_attempt": 1,
            "head_sha": head, "kind": "ci-macos", "priority": priority, "pool": pool, "slots": slots,
            "started": started, "state": state, "created_at": gr.utc(created), "hold_until": gr.utc(hold), **kw}


def ledger_doc(*entries):
    return {"schema": gr.LEDGER_SCHEMA, "reservations": list(entries)}


class FakeStateRepo:
    """The git data API of the private state repository, in memory, with real fast-forward rules."""

    def __init__(self, doc=None, repo=LEDGER_REPO):
        self.repo, self.commits, self.trees, self.n = repo, {}, {}, 0
        self.head = None
        self.before_patch = None  # a hook that can move the branch between a read and a write
        self.fail = None
        self.writes = 0
        if doc is not None:
            self.head = self._commit(self._tree(json.dumps(doc)), [])

    def _sha(self):
        self.n += 1
        return f"{self.n:040x}"

    def _tree(self, text):
        sha = self._sha()
        self.trees[sha] = text
        return sha

    def _commit(self, tree, parents):
        sha = self._sha()
        self.commits[sha] = {"tree": tree, "parents": parents}
        return sha

    def ancestors(self, sha):
        seen, todo = set(), [sha]
        while todo:
            cur = todo.pop()
            if cur not in seen:
                seen.add(cur)
                todo.extend(self.commits[cur]["parents"])
        return seen

    def doc(self):
        return json.loads(self.trees[self.commits[self.head]["tree"]])

    def push_elsewhere(self, doc):
        """Another writer wins the race."""
        self.head = self._commit(self._tree(json.dumps(doc)), [self.head])

    def __call__(self, request, timeout=None):
        method, path = request.get_method(), request.full_url.removeprefix(gr.API)
        body = json.loads(request.data) if request.data else None
        base = f"/repos/{self.repo}"
        if self.fail:
            raise self.fail
        if not path.startswith(base):
            raise AssertionError(f"ledger client touched {path}")
        path = path[len(base):]

        def answer(status, data):
            if status >= 400:
                raise urllib.error.HTTPError(request.full_url, status, "err", {}, io.BytesIO(json.dumps(data).encode()))
            return FakeResponse(status, data)

        if method == "GET" and path == "/git/ref/heads/ledger":
            return answer(200, {"object": {"sha": self.head}}) if self.head else answer(404, {"message": "Not Found"})
        if method == "GET" and path.startswith("/contents/ledger.json?ref="):
            sha = path.split("=", 1)[1]
            text = self.trees[self.commits[sha]["tree"]]
            return answer(200, {"encoding": "base64", "content": gr.base64.b64encode(text.encode()).decode()})
        if method == "POST" and path == "/git/trees":
            return answer(201, {"sha": self._tree(body["tree"][0]["content"])})
        if method == "POST" and path == "/git/commits":
            return answer(201, {"sha": self._commit(body["tree"], body["parents"])})
        if method == "PATCH" and path == "/git/refs/heads/ledger":
            if self.before_patch:
                hook, self.before_patch = self.before_patch, None
                hook(self)
            assert body["force"] is False
            if self.head not in self.ancestors(body["sha"]):
                return answer(422, {"message": "Update is not a fast forward"})
            self.head = body["sha"]
            self.writes += 1
            return answer(200, {"object": {"sha": self.head}})
        if method == "POST" and path == "/git/refs":
            self.head = body["sha"]
            return answer(201, {})
        raise AssertionError(f"unexpected {method} {path}")


def fake_ledger(doc=None):
    store = FakeStateRepo(ledger_doc() if doc is None else doc)
    return store, gr.Ledger(gr.GitHub("ledger-token", store), LEDGER_REPO)


class DecideTests(unittest.TestCase):
    def decide(self, state="fresh", ledger=None, **kw):
        return gr.decide(idle_state() if state == "fresh" else state, ledger or ledger_doc(), request(**kw), now=NOW)

    def test_std_first_then_light_then_default(self):
        d = self.decide()
        self.assertEqual((d.runs_on, d.owned, d.pool), (STD, True, STD))
        self.assertEqual(d.new["slots"], 3)
        d = self.decide(state=idle_state(std_idle=2, light_idle=3))
        self.assertEqual(d.runs_on, LIGHT)
        d = self.decide(state=idle_state(std_idle=2, light_idle=2))
        self.assertEqual((d.runs_on, d.owned), ("blacksmith-6vcpu-macos-26", False))
        self.assertIn("no owned pool has 3 free", d.reason)

    def test_pending_reservations_are_subtracted_and_started_or_expired_ones_are_not(self):
        held = reservation(slots=3)
        self.assertEqual(self.decide(state=idle_state(std_idle=5), ledger=ledger_doc(held)).runs_on, "blacksmith-6vcpu-macos-26")
        started = reservation(slots=3, started=2)
        self.assertEqual(self.decide(state=idle_state(std_idle=5, light_idle=0), ledger=ledger_doc(started)).runs_on, STD)
        lapsed = reservation(hold=NOW - 1)
        self.assertEqual(self.decide(state=idle_state(std_idle=3, light_idle=0), ledger=ledger_doc(lapsed)).runs_on, STD)
        done = reservation(state="released")
        self.assertEqual(self.decide(state=idle_state(std_idle=3, light_idle=0), ledger=ledger_doc(done)).runs_on, STD)

    def test_forks_retries_untrusted_events_and_bad_requests_never_take_owned(self):
        for kw in ({"head_repo": "someone/cmux"}, {"head_repo": ""}, {"run_attempt": 2},
                   {"event": "pull_request_target"}, {"event": "issue_comment"}, {"repo": "other/repo"},
                   {"head_sha": ""}, {"slots": 0}, {"slots": 11}, {"priority": "urgent"}):
            d = self.decide(**kw)
            self.assertFalse(d.owned, kw)
            self.assertEqual(d.runs_on, "blacksmith-6vcpu-macos-26")
        for event in ("push", "merge_group", "schedule", "workflow_dispatch"):
            self.assertTrue(self.decide(event=event, head_repo="", priority="dev").owned, event)

    def test_state_must_be_fresh_and_ours(self):
        self.assertFalse(self.decide(state=idle_state(at=NOW - 61)).owned)
        self.assertFalse(self.decide(state=None).owned)
        self.assertFalse(self.decide(state={**idle_state(), "repo": "x/y"}).owned)

    def test_xcode_selects_the_pool_and_missing_xcode_means_default(self):
        self.assertTrue(self.decide(xcode="26.6").owned)
        d = self.decide(xcode="/Applications/Xcode_26.4.app")
        self.assertFalse(d.owned)
        self.assertIn("26.4", d.reason)
        self.assertFalse(self.decide(xcode="").owned)
        self.assertEqual(gr.xcode_version("/Applications/Xcode_26.6.app/"), "26.6")
        self.assertEqual(gr.xcode_version("/Applications/Xcode.app"), "")

    def test_low_priority_only_on_idle_slots_never_ahead_of_pull_requests(self):
        d = self.decide(priority="nightly", slots=2, state=idle_state(std_idle=3, light_idle=0))
        self.assertTrue(d.owned)  # 3 idle >= 2 + 1 kept for pull requests
        d = self.decide(priority="warm", slots=2, state=idle_state(std_idle=2, light_idle=2))
        self.assertFalse(d.owned)
        pr_waiting = reservation(pool=LIGHT, slots=1)
        d = self.decide(priority="nightly", slots=1, state=idle_state(std_idle=9), ledger=ledger_doc(pr_waiting))
        self.assertFalse(d.owned)
        self.assertIn("wait for owned machines", d.reason)
        nightly_waiting = reservation(priority="nightly", slots=1, run_id=8)
        self.assertTrue(self.decide(state=idle_state(std_idle=4), ledger=ledger_doc(nightly_waiting)).owned)

    def test_asking_twice_returns_the_same_reservation(self):
        mine = reservation(run_id=100, pool=LIGHT)
        d = self.decide(ledger=ledger_doc(mine))
        self.assertEqual((d.runs_on, d.new), (LIGHT, None))
        rescued = reservation(run_id=100, state="rescued")
        self.assertFalse(self.decide(ledger=ledger_doc(rescued)).owned)


class RouteTests(unittest.TestCase):
    def route(self, store_ledger, state=None, **kw):
        return gr.route(request(**kw), json.dumps(idle_state() if state is None else state), store_ledger,
                        now=lambda: NOW, sleep=lambda s: None)

    def test_reservation_is_committed_by_compare_and_swap(self):
        store, ledger = fake_ledger()
        d = self.route(ledger)
        self.assertTrue(d.owned)
        self.assertEqual(store.writes, 1)
        [entry] = store.doc()["reservations"]
        self.assertEqual((entry["pool"], entry["slots"], entry["state"], entry["id"]),
                         (STD, 3, "held", f"{REPO}#100.1/ci-macos"))

    def test_two_routers_never_share_the_last_slots(self):
        store, ledger = fake_ledger()
        rival = reservation(run_id=55, slots=3)
        store.before_patch = lambda s: s.push_elsewhere(ledger_doc(rival))
        d = self.route(ledger, state=idle_state(std_idle=5, light_idle=0))
        self.assertFalse(d.owned)
        self.assertEqual([r["run_id"] for r in store.doc()["reservations"]], [55])

    def test_lost_race_retries_and_takes_what_is_left(self):
        store, ledger = fake_ledger()
        store.before_patch = lambda s: s.push_elsewhere(ledger_doc(reservation(run_id=55, slots=3)))
        d = self.route(ledger, state=idle_state(std_idle=5, light_idle=3))
        self.assertEqual(d.runs_on, LIGHT)
        self.assertEqual(sorted(r["pool"] for r in store.doc()["reservations"]), [LIGHT, STD])

    def test_a_ledger_that_never_settles_means_default(self):
        store, ledger = fake_ledger()

        def keep_moving(s):
            s.push_elsewhere(s.doc())
            s.before_patch = keep_moving

        store.before_patch = keep_moving
        d = self.route(ledger)
        self.assertFalse(d.owned)
        self.assertIn("stayed busy", d.reason)

    def test_unreachable_or_malformed_ledger_means_default(self):
        store, ledger = fake_ledger()
        store.fail = urllib.error.URLError("no route to host")
        self.assertFalse(self.route(ledger).owned)
        store, ledger = fake_ledger({"schema": gr.LEDGER_SCHEMA, "reservations": [{"id": 1}]})
        d = self.route(ledger)
        self.assertFalse(d.owned)
        self.assertIn("malformed", d.reason)
        self.assertFalse(self.route(None).owned)

    def test_state_alone_saying_no_makes_no_ledger_call(self):
        store, ledger = fake_ledger()
        store.fail = AssertionError("must not be called")
        self.assertFalse(self.route(ledger, head_repo="fork/cmux").owned)
        self.assertFalse(self.route(ledger, state=idle_state(at=NOW - 300)).owned)
        self.assertFalse(gr.route(request(), "not json", ledger, now=lambda: NOW).owned)
        self.assertFalse(gr.route(request(), "", ledger, now=lambda: NOW).owned)

    def test_route_command_writes_outputs_and_always_exits_zero(self):
        with tempfile.TemporaryDirectory() as tmp:
            out_file, summary = Path(tmp) / "out", Path(tmp) / "summary"
            env = {"GITHUB_REPOSITORY": REPO, "GITHUB_EVENT_NAME": "pull_request", "HEAD_REPO": REPO,
                   "HEAD_SHA": SHA, "GITHUB_RUN_ID": "100", "GITHUB_RUN_ATTEMPT": "1",
                   "GLAEDA_POOL_STATE": json.dumps(idle_state()), "GLAEDA_LEDGER_TOKEN": SECRET,
                   "GITHUB_OUTPUT": str(out_file), "GITHUB_STEP_SUMMARY": str(summary)}
            store = FakeStateRepo(ledger_doc())
            argv = ["route", "--kind", "ci-macos", "--priority", "pr", "--slots", "3",
                    "--xcode", "/Applications/Xcode_26.6.app", "--default", "blacksmith-6vcpu-macos-26"]
            code, out, err = run_main(argv, env=env, opener=store)
            self.assertEqual(code, 0, err)
            outputs = dict(line.split("=", 1) for line in out_file.read_text().splitlines())
            self.assertEqual((outputs["runs_on"], outputs["owned"], outputs["pool"]), (STD, "true", STD))
            self.assertNotIn(SECRET, out + err + out_file.read_text() + summary.read_text())
            out_file.write_text("")
            store.fail = urllib.error.URLError("down")
            code, _, _ = run_main(argv, env={**env, "GITHUB_RUN_ID": "101"}, opener=store)
            self.assertEqual(code, 0)
            outputs = dict(line.split("=", 1) for line in out_file.read_text().splitlines())
            self.assertEqual((outputs["runs_on"], outputs["owned"]), ("blacksmith-6vcpu-macos-26", "false"))
            out_file.write_text("")
            code, _, _ = run_main(argv, env={k: v for k, v in env.items() if k != "GLAEDA_LEDGER_TOKEN"})
            self.assertEqual(code, 0)
            self.assertIn("owned=false", out_file.read_text())

    def test_init_ledger_creates_once(self):
        store = FakeStateRepo()
        code, out, _ = run_main(["init-ledger"], env={"GLAEDA_LEDGER_TOKEN": SECRET}, opener=store)
        self.assertEqual((code, store.head), (0, None))
        code, out, _ = run_main(["init-ledger", "--yes"], env={"GLAEDA_LEDGER_TOKEN": SECRET}, opener=store)
        self.assertIn("created", out)
        self.assertEqual(store.doc(), ledger_doc())
        code, out, _ = run_main(["init-ledger", "--yes"], env={"GLAEDA_LEDGER_TOKEN": SECRET}, opener=store)
        self.assertIn("exists", out)


class FakeCmux:
    """Runs, jobs and pull requests of the routed repository, plus the cancel and re-run effects."""

    def __init__(self):
        self.runs, self.jobs, self.pulls, self.effects = {}, {}, {}, []

    def add_run(self, run_id=7, status="in_progress", attempt=1, head=SHA, event="pull_request", pr=5,
                head_repo=REPO, jobs=()):
        self.runs[run_id] = {"id": run_id, "status": status, "run_attempt": attempt, "head_sha": head,
                             "event": event, "repository": {"full_name": REPO},
                             "head_repository": {"full_name": head_repo},
                             "pull_requests": [{"number": pr}] if pr else []}
        self.jobs[run_id] = list(jobs)
        if pr:
            self.pulls.setdefault(pr, {"state": "open", "head": {"sha": head}})

    def __call__(self, request, timeout=None):
        method, path = request.get_method(), request.full_url.removeprefix(gr.API)
        base = f"/repos/{REPO}"
        assert path.startswith(base), path
        path = path[len(base):]
        m = re.fullmatch(r"/actions/runs/(\d+)(/.*)?", path.split("?")[0])
        if m and method == "GET" and not m.group(2):
            return FakeResponse(200, self.runs[int(m.group(1))])
        if m and method == "GET" and m.group(2).startswith("/attempts/"):
            return FakeResponse(200, {"jobs": self.jobs[int(m.group(1))]})
        if m and method == "POST" and m.group(2) in ("/cancel", "/force-cancel", "/rerun"):
            self.effects.append((m.group(2)[1:], int(m.group(1))))
            return FakeResponse(202, None)
        pm = re.fullmatch(r"/pulls/(\d+)", path)
        if pm and method == "GET":
            return FakeResponse(200, self.pulls[int(pm.group(1))])
        raise AssertionError(f"unexpected {method} {path}")


import re  # noqa: E402  (used by FakeCmux)


def job(job_id, pool=STD, status="queued", runner=None, created=NOW - 5, conclusion=None, steps=()):
    return {"id": job_id, "name": f"job-{job_id}", "labels": [pool], "status": status, "runner_name": runner,
            "created_at": gr.utc(created), "conclusion": conclusion, "steps": list(steps)}


class AgentTests(unittest.TestCase):
    def setUp(self):
        self.cmux = FakeCmux()
        self.api = gr.GitHub("repo-token", self.cmux)
        self.logs = []

    def tick(self, store_ledger, now=NOW, seen=None):
        seen = {} if seen is None else seen
        return gr.agent_tick(self.api, store_ledger, repo=REPO, now=now, seen=seen, log=self.logs.append,
                             sleep=lambda s: None)

    def test_started_slots_are_counted_and_the_run_is_watched_until_it_ends(self):
        self.cmux.add_run(jobs=[job(1, runner="m1-glaeda", status="in_progress"), job(2)])
        store, ledger = fake_ledger(ledger_doc(reservation(slots=2)))
        self.tick(ledger)
        entry = store.doc()["reservations"][0]
        self.assertEqual((entry["state"], entry["started"], entry["started_at"]), ("held", 1, gr.utc(NOW)))
        self.cmux.jobs[7][1].update(runner_name="m2-glaeda", status="in_progress")
        self.tick(ledger, now=NOW + 20)
        entry = store.doc()["reservations"][0]
        self.assertEqual((entry["state"], entry["started"]), ("held", 2))
        self.cmux.runs[7]["status"] = "completed"
        self.tick(ledger, now=NOW + 40)
        entry = store.doc()["reservations"][0]
        self.assertEqual((entry["state"], entry["ended_at"]), ("released", gr.utc(NOW + 40)))
        self.assertEqual(self.cmux.effects, [])

    def test_stuck_job_is_written_then_cancelled_then_rerun_on_overflow(self):
        self.cmux.add_run(jobs=[job(1, created=NOW - 600)])
        store, ledger = fake_ledger(ledger_doc(reservation(created=NOW - 600)))
        seen = {}
        self.tick(ledger, now=NOW, seen=seen)  # first look: waiting counts from now
        self.assertEqual(self.cmux.effects, [])
        writes = store.writes
        self.tick(ledger, now=NOW + 91, seen=seen)
        self.assertEqual(self.cmux.effects, [("cancel", 7)])
        self.assertEqual(store.writes, writes + 2)  # rescuing first, then the cancel time
        entry = store.doc()["reservations"][0]
        self.assertEqual((entry["state"], entry["cancel_at"]), ("rescuing", gr.utc(NOW + 91)))
        self.tick(ledger, now=NOW + 100, seen=seen)  # still cancelling
        self.assertEqual(self.cmux.effects, [("cancel", 7)])
        self.cmux.runs[7].update(status="completed", conclusion="cancelled")
        self.tick(ledger, now=NOW + 110, seen=seen)
        self.assertEqual(self.cmux.effects, [("cancel", 7), ("rerun", 7)])
        entry = store.doc()["reservations"][0]
        self.assertEqual(entry["state"], "rescued")
        self.assertIn("overflow", entry["note"])

    def test_cancel_record_is_saved_before_a_failing_second_write(self):
        self.cmux.add_run(jobs=[job(1, created=NOW - 600)])
        store, ledger = fake_ledger(ledger_doc(reservation(created=NOW - 600)))
        saved = []
        seen = {"job:1": NOW - 200}
        orig = store.__call__

        def fail_second_pass(request, timeout=None):
            if self.cmux.effects and request.get_method() == "GET":
                raise urllib.error.URLError("ledger down")
            return orig(request, timeout)

        ledger.api._open = fail_second_pass
        with self.assertRaises(urllib.error.URLError):
            gr.agent_tick(self.api, ledger, repo=REPO, now=NOW, seen=seen, log=self.logs.append,
                          sleep=lambda s: None, persist=lambda: saved.append(dict(seen)))
        self.assertEqual(self.cmux.effects, [("cancel", 7)])
        self.assertIn(f"cancel:{REPO}#7.1", saved[-1])

    def test_lost_write_means_no_cancel(self):
        self.cmux.add_run(jobs=[job(1, created=NOW - 600)])
        store, ledger = fake_ledger(ledger_doc(reservation(created=NOW - 600)))

        def keep_moving(s):
            s.push_elsewhere(s.doc())
            s.before_patch = keep_moving

        store.before_patch = keep_moving
        with self.assertRaises(gr.Failure):
            self.tick(ledger, seen={"job:1": NOW - 200})
        self.assertEqual(self.cmux.effects, [])

    def test_cancel_that_hangs_is_forced_then_abandoned(self):
        self.cmux.add_run(jobs=[job(1)])
        store, ledger = fake_ledger(ledger_doc(reservation(state="rescuing", cancel_at=gr.utc(NOW))))
        seen = {f"cancel:{REPO}#7.1": NOW}
        self.tick(ledger, now=NOW + 91, seen=seen)
        self.assertEqual(self.cmux.effects, [("force-cancel", 7)])
        self.tick(ledger, now=NOW + 120, seen=seen)
        self.assertEqual(self.cmux.effects, [("force-cancel", 7)])
        self.tick(ledger, now=NOW + 181, seen=seen)
        self.assertEqual(store.doc()["reservations"][0]["state"], "failed")

    def test_forged_rescuing_entries_cause_no_effect(self):
        # A finished run this agent never cancelled is never re-run.
        self.cmux.add_run(run_id=7, status="completed")
        self.cmux.runs[7]["conclusion"] = "success"
        # A running run with nothing stuck is never cancelled.
        self.cmux.add_run(run_id=8, jobs=[job(2, runner="m1-glaeda", status="in_progress")])
        store, ledger = fake_ledger(ledger_doc(reservation(run_id=7, state="rescuing"),
                                               reservation(run_id=8, state="rescuing")))
        self.tick(ledger)
        self.assertEqual(self.cmux.effects, [])
        notes = {r["run_id"]: (r["state"], r["note"]) for r in store.doc()["reservations"]}
        self.assertEqual(notes[7], ("released", "the run finished before this agent cancelled it"))
        self.assertEqual(notes[8], ("released", "no pool job is stuck any more"))

    def test_a_run_that_finished_on_its_own_is_not_rerun(self):
        self.cmux.add_run(status="completed")
        self.cmux.runs[7]["conclusion"] = "success"
        store, ledger = fake_ledger(ledger_doc(reservation(state="rescuing")))
        self.tick(ledger, seen={f"cancel:{REPO}#7.1": NOW - 30})
        self.assertEqual(self.cmux.effects, [])
        self.assertIn("on its own", store.doc()["reservations"][0]["note"])

    def test_job_refused_by_the_runner_hook_is_rescued(self):
        refused = job(1, status="completed", runner="m1-glaeda", conclusion="failure",
                      steps=[{"name": "Set up job", "conclusion": "failure"}])
        self.cmux.add_run(jobs=[refused])
        store, ledger = fake_ledger(ledger_doc(reservation(slots=1)))
        self.tick(ledger)
        self.assertEqual(self.cmux.effects, [("cancel", 7)])

    def test_a_real_failure_is_not_a_refusal(self):
        failed = job(1, status="completed", runner="m1-glaeda", conclusion="failure",
                     steps=[{"name": "Set up job", "conclusion": "success"}, {"name": "Build", "conclusion": "failure"}])
        self.assertFalse(gr.refused_at_start(failed))

    def test_moved_pull_request_is_released_not_rescued(self):
        self.cmux.add_run(jobs=[job(1, created=NOW - 600)])
        self.cmux.pulls[5]["head"]["sha"] = OTHER_SHA
        store, ledger = fake_ledger(ledger_doc(reservation(created=NOW - 600)))
        self.tick(ledger, now=NOW, seen={"job:1": NOW - 200})
        self.assertEqual(self.cmux.effects, [])
        self.assertIn("newer head", store.doc()["reservations"][0]["note"])

    def test_forged_held_reservation_cannot_cancel_a_run_github_does_not_confirm(self):
        self.cmux.add_run(jobs=[job(1, created=NOW - 600)], head_repo="fork/cmux")
        store, ledger = fake_ledger(ledger_doc(reservation(created=NOW - 600)))
        self.tick(ledger, now=NOW, seen={"job:1": NOW - 200})
        self.assertEqual(self.cmux.effects, [])
        self.assertIn("fork", store.doc()["reservations"][0]["note"])
        self.cmux.add_run(run_id=8, jobs=[job(2, created=NOW - 600)], head=OTHER_SHA)
        store, ledger = fake_ledger(ledger_doc(reservation(run_id=8, created=NOW - 600)))
        self.tick(ledger, now=NOW, seen={"job:2": NOW - 200})
        self.assertEqual(self.cmux.effects, [])

    def test_finished_rerun_and_old_runs_are_released(self):
        self.cmux.add_run(run_id=7, status="completed")
        self.cmux.add_run(run_id=8, attempt=2)
        self.cmux.add_run(run_id=9, jobs=[])
        store, ledger = fake_ledger(ledger_doc(reservation(run_id=7), reservation(run_id=8),
                                               reservation(run_id=9, created=NOW - gr.WATCH_LIMIT_SECONDS)))
        self.tick(ledger)
        notes = {r["run_id"]: (r["state"], r["note"]) for r in store.doc()["reservations"]}
        self.assertEqual(notes[7], ("released", "the run finished"))
        self.assertEqual(notes[8][1], "someone else re-ran the run")
        self.assertEqual(notes[9][1], "watch limit reached")

    def test_nightly_run_is_rescued_without_a_pull_request(self):
        self.cmux.add_run(event="schedule", pr=None, jobs=[job(1, created=NOW - 600)])
        store, ledger = fake_ledger(ledger_doc(reservation(priority="nightly", created=NOW - 600)))
        self.tick(ledger, now=NOW, seen={"job:1": NOW - 200})
        self.assertEqual(self.cmux.effects, [("cancel", 7)])

    def test_update_is_reapplied_after_a_lost_race_but_never_over_a_newer_state(self):
        self.cmux.add_run(jobs=[job(1, runner="m1-glaeda", status="in_progress")])
        store, ledger = fake_ledger(ledger_doc(reservation(slots=1)))
        extra = reservation(run_id=99, slots=1)
        store.before_patch = lambda s: s.push_elsewhere(ledger_doc(reservation(slots=1), extra))
        self.tick(ledger)
        started = {r["run_id"]: (r["state"], r["started"]) for r in store.doc()["reservations"]}
        self.assertEqual(started, {7: ("held", 1), 99: ("held", 0)})
        self.cmux.add_run(run_id=8, jobs=[job(3, runner="m1-glaeda", status="in_progress")])
        store, ledger = fake_ledger(ledger_doc(reservation(run_id=8, slots=1)))
        store.before_patch = lambda s: s.push_elsewhere(ledger_doc(reservation(run_id=8, slots=1, state="rescued")))
        self.tick(ledger)
        self.assertEqual(store.doc()["reservations"][0]["state"], "rescued")

    def test_old_finished_reservations_are_pruned_and_live_ones_never_are(self):
        old = reservation(run_id=1, state="released", created=NOW - 7200, ended_at=gr.utc(NOW - 7000))
        store, ledger = fake_ledger(ledger_doc(old, reservation(run_id=2, state="released")))
        self.tick(ledger)
        self.assertEqual([r["run_id"] for r in store.doc()["reservations"]], [2])
        many = [reservation(run_id=1000 + i) for i in range(gr.MAX_RESERVATIONS + 5)]
        pruned = gr.prune(ledger_doc(*many, reservation(run_id=10_000, state="released")), NOW)
        self.assertEqual(len(pruned["reservations"]), gr.MAX_RESERVATIONS + 5)
        d = gr.decide(idle_state(std_idle=11), ledger_doc(*many), request(), now=NOW)
        self.assertFalse(d.owned)
        self.assertIn("full", d.reason)


class CountingTests(unittest.TestCase):
    def test_a_start_newer_than_the_pool_state_keeps_its_slots(self):
        # The state was observed at NOW; a job started after that still shows as idle there.
        fresh_start = reservation(slots=2, started=2, started_at=gr.utc(NOW + 5))
        self.assertEqual(gr.pending(fresh_start, NOW + 10, NOW), 2)
        self.assertEqual(gr.pending(fresh_start, NOW + 30, NOW + 25), 0)
        ended = reservation(slots=2, state="released", ended_at=gr.utc(NOW + 5))
        self.assertEqual(gr.pending(ended, NOW + 10, NOW), 2)
        self.assertEqual(gr.pending(ended, NOW + 30, NOW + 25), 0)
        self.assertEqual(gr.pending(reservation(slots=2, state="rescuing"), NOW, NOW), 2)
        lapsed_start = reservation(slots=2, started=2, started_at=gr.utc(NOW + 5), hold=NOW)
        self.assertEqual(gr.pending(lapsed_start, NOW + 10, NOW), 2)
        d = gr.decide(idle_state(std_idle=4, light_idle=0), ledger_doc(fresh_start), request(slots=3), now=NOW + 10)
        self.assertFalse(d.owned)

    def test_malformed_state_answers_the_default_instead_of_crashing(self):
        store, ledger = fake_ledger()
        for bad in ({**idle_state(), "order": [STD, 3]},
                    {**idle_state(), "pools": {STD: {"idle": 1}}, "order": [STD]}):
            d = gr.route(request(), json.dumps(bad), ledger, now=lambda: NOW)
            self.assertFalse(d.owned)
        self.assertEqual(store.writes, 0)

    def test_deadline_stops_a_slow_ledger(self):
        clock = [0.0]
        api = gr.GitHub("t", FakeStateRepo(ledger_doc()), deadline=10.0, clock=lambda: clock[0])
        ledger = gr.Ledger(api, LEDGER_REPO)
        ledger.read()  # inside the budget
        clock[0] = 100.0
        with self.assertRaises(gr.Failure):
            ledger.read()
        d = gr.route(request(), json.dumps(idle_state()), ledger, now=lambda: NOW)
        self.assertFalse(d.owned)
        self.assertIn("ledger unavailable", d.reason)

    def test_route_command_survives_an_unreadable_state_file(self):
        code, out, _ = run_main(["route", "--kind", "k", "--priority", "pr", "--default", "bs",
                                 "--state", "/nonexistent/state.json"], env={})
        self.assertEqual(code, 0)
        self.assertIn("runs-on: bs", out)

    def test_agent_command_publishes_then_watches(self):
        with tempfile.TemporaryDirectory() as tmp:
            pools = Path(tmp) / "pools.json"
            pools.write_text(json.dumps(pools_doc()))
            store = FakeStateRepo(ledger_doc())
            api = FakeGitHub({("GET", f"/repos/{REPO}/actions/runners"): (200, runners()),
                              ("GET", f"/repos/{REPO}/actions/runs?status=queued"): (200, {"workflow_runs": [run_row(7)]}),
                              ("GET", f"/repos/{REPO}/actions/runs?status=in_progress"): (200, {"workflow_runs": []}),
                              ("GET", f"/repos/{REPO}/actions/runs/7/attempts/1/jobs"): (200, {"jobs": [
                                  job_row("queued", BS26)]}),
                              ("PATCH", f"/repos/{REPO}/actions/variables/GLAEDA_POOL_STATE"): (204, None)})

            def opener(request, timeout=None):
                if request.full_url.startswith(f"{gr.API}/repos/{LEDGER_REPO}"):
                    return store(request, timeout)
                return api(request, timeout)

            code, out, err = run_main(["agent", "--pools", str(pools), "--seen-file", str(Path(tmp) / "seen.json"),
                                       "--overflow-cache", str(Path(tmp) / "overflow.json")],
                                      env={"GLAEDA_ROUTE_TOKEN": "repo-secret", "GLAEDA_LEDGER_TOKEN": SECRET},
                                      opener=opener)
            self.assertEqual(code, 0, err)
            self.assertIn("published GLAEDA_POOL_STATE", out)
            self.assertIn(f"{BS26} 0 running 1 queued", out)
            published = json.loads(next(c[2]["value"] for c in api.calls if c[0] == "PATCH"))
            self.assertTrue(gr.validate_state(published, now=NOW, repo=REPO)[0])
            self.assertEqual(published["overflow"]["pools"], {BS26: {"running": 0, "queued": 1}})
            self.assertIn("watched 0 reservation(s)", out)
            self.assertNotIn(SECRET, out + err)
            self.assertTrue((Path(tmp) / "seen.json").is_file())


BS26 = "blacksmith-6vcpu-macos-26"
BS12 = "blacksmith-12vcpu-macos-26"


def run_row(run_id, attempt=1, updated="2027-01-15T08:00:00Z"):
    return {"id": run_id, "run_attempt": attempt, "updated_at": updated, "created_at": "2027-01-15T07:00:00Z"}


def job_row(status, *labels):
    return {"status": status, "labels": list(labels), "runner_name": "r" if status == "in_progress" else None}


class OverflowTests(unittest.TestCase):
    def test_load_counts_running_and_queued_per_blacksmith_label(self):
        load = gr.overflow_load([job_row("queued", BS26), job_row("in_progress", BS26), job_row("queued", BS12),
                                 job_row("completed", BS26), job_row("queued", STD), job_row("queued", "ubuntu-24.04"),
                                 "junk", {"status": "queued"}])
        self.assertEqual(load, {BS12: {"running": 0, "queued": 1}, BS26: {"running": 1, "queued": 1}})

    def listing(self, runs, jobs=None):
        return FakeGitHub({
            ("GET", f"/repos/{REPO}/actions/runs?status=queued"): lambda _: (200, {"workflow_runs": runs["q"]}),
            ("GET", f"/repos/{REPO}/actions/runs?status=in_progress"): lambda _: (200, {"workflow_runs": runs["p"]}),
            ("GET", f"/repos/{REPO}/actions/runs/"): jobs or (200, {"jobs": [job_row("queued", BS26)]})})

    def test_listing_reuses_unchanged_runs_and_drops_finished_ones(self):
        runs = {"q": [run_row(1), run_row(2)], "p": [run_row(3, updated="2027-01-15T08:01:00Z")]}
        api = self.listing(runs)
        client, cache = gr.GitHub(SECRET, api), {"999": {"key": "1:old", "at": NOW, "jobs": [job_row("queued", BS26)]}}
        jobs, complete = client.active_jobs(REPO, cache, now=NOW)
        self.assertTrue(complete)
        self.assertEqual(len(jobs), 3)
        self.assertNotIn("999", cache)
        first = len(api.calls)
        client.active_jobs(REPO, cache, now=NOW + 20)
        self.assertEqual(len(api.calls) - first, 2, "a quiet tick lists runs only")
        runs["p"] = [run_row(3, updated="2027-01-15T08:02:00Z")]
        first = len(api.calls)
        client.active_jobs(REPO, cache, now=NOW + 40)
        self.assertEqual(len(api.calls) - first, 3, "a moved run is listed again")
        first = len(api.calls)
        client.active_jobs(REPO, cache, now=NOW + 40 + gr.OVERFLOW_RELIST_SECONDS)
        self.assertEqual(len(api.calls) - first, 5, "an old listing is refreshed even when the run did not move")

    def test_a_run_in_both_listings_counts_once(self):
        runs = {"q": [run_row(1)], "p": [run_row(1, updated="2027-01-15T08:03:00Z")]}
        jobs, complete = gr.GitHub(SECRET, self.listing(runs)).active_jobs(REPO, {}, now=NOW)
        self.assertEqual(len(jobs), 1)
        self.assertTrue(complete)

    def test_jobs_are_read_past_the_first_page(self):
        pages = {1: [job_row("queued", BS26)] * 100, 2: [job_row("in_progress", BS26)] * 3}

        def jobs(_body, calls=[]):
            calls.append(1)
            return 200, {"jobs": pages[len(calls)]}

        api = self.listing({"q": [run_row(1)], "p": []}, jobs=jobs)
        found, complete = gr.GitHub(SECRET, api).active_jobs(REPO, {}, now=NOW)
        self.assertEqual(len(found), 103)
        self.assertTrue(complete)

    def test_too_many_new_runs_is_a_partial_listing_that_catches_up(self):
        runs = {"q": [run_row(i) for i in range(gr.OVERFLOW_RUN_LOOKUPS + 5)], "p": []}
        client, cache = gr.GitHub(SECRET, self.listing(runs)), {}
        jobs, complete = client.active_jobs(REPO, cache, now=NOW)
        self.assertFalse(complete)
        self.assertEqual(len(jobs), gr.OVERFLOW_RUN_LOOKUPS)
        jobs, complete = client.active_jobs(REPO, cache, now=NOW + 20)
        self.assertTrue(complete)
        self.assertEqual(len(jobs), gr.OVERFLOW_RUN_LOOKUPS + 5)

    def test_low_rate_budget_stops_job_listings(self):
        runs = {"q": [run_row(1)], "p": []}
        client = gr.GitHub(SECRET, self.listing(runs))
        client.remaining = gr.OVERFLOW_RATE_FLOOR - 1
        original = client.call

        def keep_low(*a, **k):  # the fake answers carry no rate header, so the budget stays low
            return original(*a, **k)

        client.call = keep_low
        jobs, complete = client.active_jobs(REPO, {}, now=NOW)
        self.assertFalse(complete)
        self.assertEqual(jobs, [])

    def test_rate_header_is_recorded(self):
        class Headed(FakeResponse):
            headers = {"X-RateLimit-Remaining": "42"}

        client = gr.GitHub(SECRET, lambda request, timeout=None: Headed(200, {}))
        client.ok("GET", "/rate_limit")
        self.assertEqual(client.remaining, 42)

    def test_overflow_section_is_validated(self):
        doc = state()
        doc["overflow"] = gr.overflow_section([job_row("queued", BS26)], observed_at=NOW, complete=True)
        self.assertTrue(gr.validate_state(doc, now=NOW, repo=REPO)[0])
        for bad in ({"observed_at": "x", "complete": True, "pools": {}},
                    {**doc["overflow"], "pools": {STD: {"running": 0, "queued": 0}}},
                    {**doc["overflow"], "pools": {BS26: {"running": -1, "queued": 0}}},
                    {**doc["overflow"], "complete": "yes"}):
            self.assertFalse(gr.validate_state({**doc, "overflow": bad}, now=NOW, repo=REPO)[0], bad)

    def test_a_failed_overflow_listing_still_publishes_the_owned_pools(self):
        with tempfile.TemporaryDirectory() as tmp:
            pools = Path(tmp) / "pools.json"
            pools.write_text(json.dumps(pools_doc()))
            api = FakeGitHub({("GET", f"/repos/{REPO}/actions/runners"): (200, runners()),
                              ("GET", f"/repos/{REPO}/actions/runs"): (502, {"message": "bad gateway"})})
            code, out, err = run_main(["state", "--pools", str(pools), "--overflow-load",
                                       "--overflow-cache", str(Path(tmp) / "c.json")],
                                      env={"GLAEDA_ROUTE_TOKEN": SECRET}, opener=api)
            self.assertEqual(code, 0, err)
            self.assertNotIn("overflow", json.loads(out))
            self.assertIn("overflow load unavailable", err)
            self.assertNotIn(SECRET, out + err)

    def test_state_and_publish_leave_overflow_off_by_default(self):
        with tempfile.TemporaryDirectory() as tmp:
            pools = Path(tmp) / "pools.json"
            pools.write_text(json.dumps(pools_doc()))
            api = FakeGitHub({("GET", f"/repos/{REPO}/actions/runners"): (200, runners())})
            code, out, _ = run_main(["state", "--pools", str(pools)], env={"GLAEDA_ROUTE_TOKEN": SECRET}, opener=api)
            self.assertEqual(code, 0)
            self.assertNotIn("overflow", json.loads(out))


class RouteFailOpenTests(unittest.TestCase):
    def test_unreadable_route_arguments_answer_the_caller_default(self):
        with tempfile.TemporaryDirectory() as tmp:
            out_file = Path(tmp) / "out"
            code, out, _ = run_main(["route", "--priority", "bogus"], env={"GITHUB_OUTPUT": str(out_file)})
            self.assertEqual(code, 0)
            self.assertIn("runs_on=\n", out_file.read_text())
            self.assertIn("could not read its arguments", out)

    def test_a_hung_route_is_cut_off_and_answers_the_default(self):
        import time as _time
        original, wall = gr.route, gr.ROUTE_WALL_SECONDS
        gr.route = lambda *a, **k: _time.sleep(5)
        gr.ROUTE_WALL_SECONDS = 0.2
        try:
            started = _time.monotonic()
            code, out, _ = run_main(["route", "--kind", "k", "--priority", "pr", "--default", "bs"], env={})
        finally:
            gr.route, gr.ROUTE_WALL_SECONDS = original, wall
        self.assertEqual(code, 0)
        self.assertLess(_time.monotonic() - started, 2)
        self.assertIn("runs-on: bs", out)
        self.assertIn("no answer within", out)


if __name__ == "__main__":
    unittest.main()
