#!/usr/bin/env python3
"""Bounded, on-demand native request worker for initialized trusted checkouts."""
import argparse
import contextlib
import fcntl
import json
import os
from pathlib import Path
import re
import select
import stat
import subprocess
import sys
import time
import uuid

import apple_build as native

MAX_REQUESTS = 32
TERMINAL = {"completed", "failed", "interrupted"}
FIELDS = {"id", "state", "operation", "profile", "generation", "configuration", "runtime", "batch", "result"}


def runtime_identity():
    return native.digest([Path(__file__).read_bytes().hex(), Path(native.__file__).read_bytes().hex()])


RUNTIME = runtime_identity()


def storage_plan(project):
    project = Path(project).resolve(strict=True)
    info = project.stat()
    return {"project": project, "owner": {"schema_version": 1, "project": native.digest([str(project), info.st_dev, info.st_ino])}}


def valid_id(value):
    return isinstance(value, str) and re.fullmatch(r"[a-f0-9]{32}", value)


@contextlib.contextmanager
def mutex(state, name, blocking=True):
    fd = os.open(name, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW | os.O_NONBLOCK, 0o600, dir_fd=state)
    try:
        info = os.fstat(fd)
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_nlink != 1 or info.st_mode & 0o077:
            raise native.Refusal("unsafe request lock")
        deadline = time.monotonic() + 5
        while True:
            try:
                fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
                break
            except BlockingIOError:
                if not blocking:
                    raise
                if time.monotonic() >= deadline:
                    raise native.Refusal("request transition lock deadline expired")
                time.sleep(0.05)
        current = os.stat(name, dir_fd=state, follow_symlinks=False)
        if (current.st_dev, current.st_ino) != (info.st_dev, info.st_ino) or os.fstat(fd).st_nlink != 1:
            raise native.Refusal("request lock changed")
        yield fd
    finally:
        os.close(fd)


def read_queue(state):
    for attempt in range(4):
        try:
            fd = os.open("requests.json", os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=state)
        except FileNotFoundError:
            return []
        with os.fdopen(fd, "rb") as stream:
            info = os.fstat(stream.fileno())
            if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_nlink > 1 or info.st_mode & 0o077:
                raise native.Refusal("unsafe request ledger")
            if info.st_nlink == 0:
                # Atomic publication may unlink the opened version before fstat.
                # Retry the canonical name; never adopt the detached inode.
                continue
            ledger = native.bounded_json(stream.read(native.LIMIT + 1))
            break
    else:
        raise native.Refusal("request ledger changed repeatedly; retry observation")
    if not isinstance(ledger, dict) or set(ledger) != {"schema_version", "requests"} or type(ledger["schema_version"]) is not int or ledger["schema_version"] != 1:
        raise native.Refusal("invalid request ledger")
    rows = ledger["requests"]
    if not isinstance(rows, list) or len(rows) > MAX_REQUESTS:
        raise native.Refusal("request ledger exceeds bounds")
    ids = set()
    for row in rows:
        if (not isinstance(row, dict) or set(row) != FIELDS or not valid_id(row["id"]) or row["id"] in ids
                or row["state"] not in TERMINAL | {"pending", "running"}
                or row["operation"] not in {"build", "check", "dependencies", "refresh"}
                or not isinstance(row["profile"], str) or not re.fullmatch(r"[a-zA-Z0-9_-]{1,64}", row["profile"])
                or not isinstance(row["generation"], str) or not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,63}", row["generation"])
                or any(not isinstance(row[k], str) or not re.fullmatch(r"[a-f0-9]{64}", row[k]) for k in ("configuration", "runtime"))
                or (row["batch"] is not None and not valid_id(row["batch"]))):
            raise native.Refusal("invalid request record")
        validate_result(row["result"])
        if ((row["state"] == "pending" and (row["batch"] is not None or row["result"] is not None))
                or (row["state"] == "running" and (row["batch"] is None or row["result"] is not None))
                or (row["state"] in TERMINAL and (row["batch"] is None or row["result"] is None))):
            raise native.Refusal("inconsistent request transition")
        if row["state"] == "completed" and (row["result"]["reason"] is not None or row["result"]["exit_code"] != 0
                or (row["operation"] != "dependencies" and any(row["result"][k] is None for k in ("run_id", "source_before", "source_after")))):
            raise native.Refusal("incomplete request success evidence")
        ids.add(row["id"])
    return rows


def validate_result(result):
    if result is None:
        return
    if not isinstance(result, dict) or set(result) != {"reason", "run_id", "exit_code", "source_before", "source_after"}:
        raise native.Refusal("invalid request result")
    if result["reason"] not in (None, "worker_interrupted", "configuration_changed", "native_refused", "runtime_changed", "dependency_preparation_failed"):
        raise native.Refusal("invalid request outcome")
    if result["run_id"] is not None and not valid_id(result["run_id"]):
        raise native.Refusal("invalid native run identity")
    if result["exit_code"] is not None and (type(result["exit_code"]) is not int or not 0 <= result["exit_code"] <= 255):
        raise native.Refusal("invalid native exit status")
    for key in ("source_before", "source_after"):
        source = result[key]
        if source is None:
            continue
        if (not isinstance(source, dict) or set(source) not in ({"commit", "clean"}, {"commit", "tree", "clean"})
                or type(source["clean"]) is not bool or not isinstance(source["commit"], str)
                or not re.fullmatch(r"[a-f0-9]{40,64}", source["commit"])):
            raise native.Refusal("invalid source observation")
        if ("tree" in source and (not isinstance(source["tree"], str)
                or not re.fullmatch(r"[a-f0-9]{40,64}", source["tree"]))):
            raise native.Refusal("invalid source observation")


def write_queue(state, rows):
    ledger = {"schema_version": 1, "requests": rows}
    if len(json.dumps(ledger).encode()) > native.LIMIT - 1:
        raise native.Refusal("request ledger exceeds byte bound")
    native.write_json(state, "requests.json", ledger)


def request_plan(project, profile, generation, operation, prepare=native.prepare):
    if operation != "refresh":
        return prepare(project, profile, generation, operation=operation)
    check = prepare(project, profile, generation, operation="check")
    dependencies = prepare(project, profile, generation, operation="dependencies")
    if check["key"] != dependencies["key"]:
        raise native.Refusal("refresh plans have different cache generations")
    return {**check, "operation": "refresh", "refresh_dependencies": dependencies}


def configuration(plan):
    identity = [plan["key"], plan.get("invocation_identity"), plan.get("operation_identity"), plan.get("lineage")]
    if plan["operation"] == "refresh":
        identity.append(configuration(plan["refresh_dependencies"]))
    return native.digest(identity)


def execute_request(plan, prepare, execute):
    if plan["operation"] == "refresh":
        dependency_result = execute(plan["refresh_dependencies"], prepare, reuse_dependencies=True, wait_seconds=300)
        if dependency_result.get("exit_code") != 0:
            return "dependency_preparation_failed", dependency_result
        check = {k: v for k, v in plan.items() if k != "refresh_dependencies"}
        check["operation"] = "check"
        return None, execute(check, prepare, wait_seconds=300)
    return None, execute(plan, prepare, reuse_dependencies=plan["operation"] == "dependencies", wait_seconds=300)


@contextlib.contextmanager
def request_events(state):
    if not hasattr(select, "kqueue"):
        raise native.Refusal("request event waiting requires macOS or BSD kqueue")
    # Watch the directory because the ledger is replaced atomically. Arm before
    # reading status so completion between the read and sleep cannot be lost.
    with contextlib.closing(select.kqueue()) as events:
        event = select.kevent(state, filter=select.KQ_FILTER_VNODE,
                             flags=select.KQ_EV_ADD | select.KQ_EV_CLEAR,
                             fflags=select.KQ_NOTE_WRITE | select.KQ_NOTE_RENAME | select.KQ_NOTE_DELETE)
        events.control([event], 0, 0)
        yield lambda seconds: events.control(None, 1, seconds)


def wait_request(project, request_id, seconds, events=request_events):
    if not valid_id(request_id) or type(seconds) is not int or not 0 <= seconds <= 3600:
        raise native.Refusal("request wait requires an exact id and a deadline from 0 to 3600 seconds")
    deadline = time.monotonic() + seconds
    # Immediate/terminal reads need no platform-specific event facility.
    result = status(project, request_id)
    if result["state"] in TERMINAL or seconds == 0:
        return {**result, "wait": "terminal" if result["state"] in TERMINAL else "deadline"}
    with native.store(storage_plan(project)) as state, events(state) as changed:
        while True:
            # Reopen and validate the canonical store each time; an event itself
            # never grants authority to a replaced path or proves completion.
            result = status(project, request_id)
            if result["state"] in TERMINAL:
                return {**result, "wait": "terminal"}
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                return {**result, "wait": "deadline"}
            changed(remaining)


def public(row):
    return {"request_id": row["id"], "state": row["state"], "operation": row["operation"],
            "batch_id": row["batch"], "result": row["result"], "source_policy": "latest_at_execution_observed_only"}


def wake(project):
    project = storage_plan(project)["project"]
    with native.store(storage_plan(project)):
        pass
    try:
        subprocess.Popen([sys.executable, str(Path(__file__).resolve()), "worker", "--project", str(project)],
                         cwd=project, env=native.base_environment(), stdin=subprocess.DEVNULL,
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True, close_fds=True)
    except OSError:
        return {"wake": "failed", "retry": "wake"}
    return {"wake": "requested"}


def submit(plan, wake_worker=wake):
    if runtime_identity() != RUNTIME:
        raise native.Refusal("request runtime changed; retry from current launcher")
    if not re.fullmatch(r"[a-zA-Z0-9_-]{1,64}", plan["profile"]):
        raise native.Refusal("invalid queued profile")
    row = {"id": uuid.uuid4().hex, "state": "pending", "operation": plan["operation"],
           "profile": plan["profile"], "generation": plan["generation"], "configuration": configuration(plan),
           "runtime": RUNTIME, "batch": None, "result": None}
    with native.store(plan) as state, mutex(state, "requests.lock"):
        rows = read_queue(state)
        if len(rows) >= MAX_REQUESTS:
            raise native.Refusal("request history full; collect and forget terminal requests")
        rows.append(row)
        write_queue(state, rows)
    # The request is durable even if spawning fails or the submitting process exits here.
    return {**public(row), **wake_worker(plan["project"])}


def list_requests(project):
    with native.store(storage_plan(project)) as state:
        return {"requests": [public(row) for row in read_queue(state)]}


def status(project, request_id, forget=False):
    if not valid_id(request_id):
        raise native.Refusal("invalid request id")
    with native.store(storage_plan(project)) as state:
        # Atomic ledger replacement permits lock-free, side-effect-free status reads.
        with mutex(state, "requests.lock") if forget else contextlib.nullcontext():
            rows = read_queue(state)
            row = next((r for r in rows if r["id"] == request_id), None)
            if row is None:
                raise native.Refusal("request not found")
            if forget:
                if row["state"] not in TERMINAL:
                    raise native.Refusal("only terminal requests can be forgotten")
                write_queue(state, [r for r in rows if r["id"] != request_id])
                return {"request_id": request_id, "state": "forgotten"}
            return public(row)


def outcome(reason=None, receipt=None):
    receipt = receipt or {}
    result = {"reason": reason, **{k: receipt.get(k) for k in ("run_id", "exit_code", "source_before", "source_after")}}
    validate_result(result)
    return result


def worker(project, prepare=native.prepare, execute=native.execute, debounce=0.3):
    with native.store(storage_plan(project)) as state:
        try:
            with mutex(state, "request-worker.lock", blocking=False) as worker_fd:
                # Lock ownership, never PID observation, authorizes classifying abandoned requests.
                with mutex(state, "requests.lock"):
                    rows = read_queue(state)
                    abandoned = [r for r in rows if r["state"] == "running"]
                    for row in abandoned:
                        row.update(state="interrupted", result=outcome("worker_interrupted"))
                    if abandoned:
                        write_queue(state, rows)
                while True:
                    time.sleep(debounce)
                    with mutex(state, "requests.lock"):
                        rows = read_queue(state)
                        pending = [r for r in rows if r["state"] == "pending"]
                        if not pending:
                            # Release under the enqueue lock: a later submit cannot lose its wake.
                            fcntl.flock(worker_fd, fcntl.LOCK_UN)
                            return
                        first = pending[0]
                        batch = uuid.uuid4().hex
                        fields = ("operation", "profile", "generation", "configuration", "runtime")
                        selected = [r for r in pending if all(r[k] == first[k] for k in fields)]
                        ids = {r["id"] for r in selected}
                        for row in selected:
                            row.update(state="running", batch=batch)
                        write_queue(state, rows)
                    reason, receipt = None, None
                    try:
                        if first["runtime"] != RUNTIME or runtime_identity() != RUNTIME:
                            reason = "runtime_changed"
                        else:
                            plan = request_plan(project, first["profile"], first["generation"], first["operation"], prepare)
                            if configuration(plan) != first["configuration"]:
                                reason = "configuration_changed"
                            else:
                                reason, receipt = execute_request(plan, prepare, execute)
                    except (native.Refusal, OSError, ValueError, KeyError, TypeError, AttributeError, subprocess.SubprocessError):
                        reason = "native_refused"
                    result = outcome(reason, receipt)
                    completed = reason is None and receipt is not None and receipt.get("exit_code", 0) == 0
                    with mutex(state, "requests.lock"):
                        rows = read_queue(state)
                        for row in rows:
                            if row["id"] in ids:
                                if row["state"] != "running" or row["batch"] != batch:
                                    raise native.Refusal("request batch changed")
                                row.update(state="completed" if completed else "failed", result=result)
                        write_queue(state, rows)
        except BlockingIOError:
            return  # Existing worker observes new requests on its next queue pass.


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("action", choices=("worker",))
    parser.add_argument("--project", type=Path, required=True)
    args = parser.parse_args()
    worker(args.project)
