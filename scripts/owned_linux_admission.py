"""Private reviewed-workload launch admission. No queue or caller authority.

Only an installed local adapter supplies this root and an in-process reviewed demand; remote
requests never choose either. An uncompleted reservation is deliberately not reclaimed from a
dead PID or absent lock.
"""
from __future__ import annotations

from contextlib import contextmanager
from dataclasses import dataclass
import fcntl
import hashlib
import json
import os
from pathlib import Path
import selectors
import stat
import subprocess
import time

from owned_linux_task import Refusal, closed_environment

MAX_DOCUMENT = 32768
MAX_EXECUTABLE = 128 * 1024 * 1024
FRESH_SECONDS = 3
# v2 adds the reviewed demand to the durable reservation identity. A surviving v1 record
# omits part of its own binding, so it refuses and stays an explicit operator recovery.
RESERVATION_SCHEMA_VERSION = 2


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), allow_nan=False).encode() + b"\n"


def decode(raw):
    def pairs(items):
        result = {}
        for key, value in items:
            if key in result:
                raise ValueError("duplicate key")
            result[key] = value
        return result
    try:
        return json.loads(raw, object_pairs_hook=pairs,
                          parse_constant=lambda _: (_ for _ in ()).throw(ValueError()))
    except (ValueError, UnicodeError) as error:
        raise Refusal("invalid admission document") from error


def integer(value, minimum=0):
    return type(value) is int and minimum <= value <= 2**63 - 1


def digest(value):
    return "sha256:" + hashlib.sha256(value).hexdigest()


class Deferred(Refusal):
    """A closed wait category; still a refusal at the physical launch boundary."""
    def __init__(self, reason, message):
        super().__init__(message)
        self.reason = reason


@dataclass(frozen=True, slots=True)
class AdmissionDemand:
    """Reviewed in-process capacity demand; never decoded from a remote request."""

    memory_bytes: int
    minimum_logical_cpus: int

    def __post_init__(self):
        if (not integer(self.memory_bytes, 1)
                or not integer(self.minimum_logical_cpus, 1)):
            raise Refusal("invalid local admission demand")


# Exact existing verify-focused/v1 behavior: 8 GiB MemoryMax and a host with at least eight
# logical CPUs (four for the workload plus the existing fixed owner headroom assumption).
VERIFY_FOCUSED_DEMAND = AdmissionDemand(
    memory_bytes=8 * 1024**3,
    minimum_logical_cpus=8,
)


def validated_demand(value):
    if type(value) is not AdmissionDemand:
        raise Refusal("invalid local admission demand")
    return value


def demand_record(demand):
    """Canonical durable form of one reviewed demand.

    The demand is part of the reservation's semantic binding, so it is written into the
    record and compared on resume, launch, release and recovery. The record is evidence
    to match, never the authority: the caller still supplies the reviewed demand and a
    disagreement refuses instead of adopting whatever survived.
    """
    demand = validated_demand(demand)
    return {"memory_bytes": demand.memory_bytes,
            "minimum_logical_cpus": demand.minimum_logical_cpus}


class Store:
    """All private state I/O stays relative to one held, verified directory descriptor."""
    def __init__(self, root):
        root = Path(root)
        if not root.is_absolute() or root.resolve() != root:
            raise Refusal("admission root must be an exact absolute directory")
        self.root = root
        self.locks = {}
        self.fd = os.open(root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC)
        info = os.fstat(self.fd)
        if info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) != 0o700:
            os.close(self.fd)
            raise Refusal("admission root is not private")

    def verify_root(self):
        info = os.stat(self.root, follow_symlinks=False)
        held = os.fstat(self.fd)
        if (self.root.resolve() != self.root or not stat.S_ISDIR(info.st_mode)
                or (info.st_dev, info.st_ino) != (held.st_dev, held.st_ino)
                or info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) != 0o700):
            raise Refusal("admission root changed")
        for name, fd in self.locks.items():
            current = os.stat(name, dir_fd=self.fd, follow_symlinks=False)
            held_lock = os.fstat(fd)
            if ((current.st_dev, current.st_ino) != (held_lock.st_dev, held_lock.st_ino)
                    or held_lock.st_nlink != 1 or held_lock.st_size != 0
                    or stat.S_IMODE(held_lock.st_mode) != 0o600):
                raise Refusal("held admission lock changed")

    def close(self):
        os.close(self.fd)

    def open(self, name, flags):
        self.verify_root()
        fd = os.open(name, flags | os.O_NOFOLLOW | os.O_CLOEXEC | os.O_NONBLOCK,
                     0o600, dir_fd=self.fd)
        info = os.fstat(fd)
        if (not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid()
                or info.st_nlink != 1 or stat.S_IMODE(info.st_mode) != 0o600
                or info.st_size > MAX_DOCUMENT):
            os.close(fd)
            raise Refusal("unsafe admission state object")
        return fd

    @contextmanager
    def lock(self, name):
        fd = self.open(name, os.O_RDWR | os.O_CREAT)
        try:
            if os.fstat(fd).st_size != 0:
                raise Refusal("admission lock is not empty")
            try:
                fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError as error:
                raise Refusal("admission state is busy") from error
            self.locks[name] = fd
            yield
        finally:
            self.locks.pop(name, None)
            os.close(fd)

    def read(self, name):
        try:
            fd = self.open(name, os.O_RDONLY)
        except FileNotFoundError:
            return None
        with os.fdopen(fd, "rb") as stream:
            raw = stream.read(MAX_DOCUMENT + 1)
        if len(raw) > MAX_DOCUMENT:
            raise Refusal("admission document exceeds ceiling")
        value = decode(raw)
        if not isinstance(value, dict) or canonical(value) != raw:
            raise Refusal("admission document is not canonical")
        return value

    def write(self, name, value):
        self.verify_root()
        raw = canonical(value)
        if len(raw) > MAX_DOCUMENT:
            raise Refusal("admission document exceeds ceiling")
        temporary = f".{name}.{os.getpid()}"
        fd = self.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL)
        try:
            with os.fdopen(fd, "wb") as stream:
                stream.write(raw)
                stream.flush()
                os.fsync(stream.fileno())
            os.replace(temporary, name, src_dir_fd=self.fd, dst_dir_fd=self.fd)
            os.fsync(self.fd)
        finally:
            try:
                os.unlink(temporary, dir_fd=self.fd)
            except FileNotFoundError:
                pass

    def remove(self, name):
        self.verify_root()
        os.unlink(name, dir_fd=self.fd)
        os.fsync(self.fd)


def policy(store):
    value = store.read("policy.json")
    if (not isinstance(value, dict)
            or set(value) != {"schema_version", "generation", "revision", "node_control",
                              "host_executable", "policy_executable", "memory_reserve_bytes"}
            or type(value["schema_version"]) is not int or value["schema_version"] != 1
            or not isinstance(value["generation"], str) or len(value["generation"]) != 64
            or any(c not in "0123456789abcdef" for c in value["generation"])
            or not integer(value["revision"], 1)
            or value["node_control"] not in ("available", "held", "draining")
            or not integer(value["memory_reserve_bytes"], 4 * 1024**3)):
        raise Refusal("invalid local admission policy")
    for key in ("host_executable", "policy_executable"):
        entry = value[key]
        if (not isinstance(entry, dict) or set(entry) != {"path", "sha256"}
                or not isinstance(entry["path"], str) or not Path(entry["path"]).is_absolute()
                or not isinstance(entry["sha256"], str) or len(entry["sha256"]) != 71
                or not entry["sha256"].startswith("sha256:")
                or any(c not in "0123456789abcdef" for c in entry["sha256"][7:])):
            raise Refusal("invalid local admission executable identity")
    return value


def query(entry, arguments, raw=b""):
    """Pin the exact executable inode; bound output and time before any workload launch."""
    fd = os.open(entry["path"], os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC | os.O_NONBLOCK)
    try:
        info = os.fstat(fd)
        if (not stat.S_ISREG(info.st_mode) or info.st_uid not in (0, os.getuid())
                or info.st_mode & 0o022 or info.st_size > MAX_EXECUTABLE):
            raise Refusal("unsafe local admission executable")
        h = hashlib.sha256()
        count = 0
        while block := os.read(fd, 1024 * 1024):
            count += len(block)
            if count > MAX_EXECUTABLE:
                raise Refusal("local admission executable exceeds ceiling")
            h.update(block)
        if "sha256:" + h.hexdigest() != entry["sha256"]:
            raise Refusal("local admission executable changed")
        with subprocess.Popen([f"/proc/self/fd/{fd}", *arguments], pass_fds=(fd,),
                              env=closed_environment(), stdin=subprocess.PIPE,
                              stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                              start_new_session=True) as child:
            output = bytearray()
            deadline = time.monotonic() + FRESH_SECONDS
            try:
                child.stdin.write(raw)
                child.stdin.close()
                with selectors.DefaultSelector() as selector:
                    selector.register(child.stdout, selectors.EVENT_READ)
                    while True:
                        remaining = deadline - time.monotonic()
                        if remaining <= 0 or not selector.select(remaining):
                            raise Refusal("local admission observation timed out")
                        block = os.read(child.stdout.fileno(), 4096)
                        if not block:
                            break
                        output.extend(block)
                        if len(output) > MAX_DOCUMENT:
                            raise Refusal("local admission output exceeds ceiling")
                if child.wait(timeout=max(0.001, deadline - time.monotonic())) != 0:
                    raise Refusal("local admission observer refused")
            except BaseException:
                try:
                    os.killpg(child.pid, 9)
                except ProcessLookupError:
                    pass
                child.wait()
                raise
        return decode(output)
    finally:
        os.close(fd)


def check(current, demand=VERIFY_FOCUSED_DEMAND):
    demand = validated_demand(demand)
    started = time.monotonic()
    host = query(current["host_executable"], ["--output", "json"])
    try:
        if (host["document_type"] != "glaeda-linux-host-observation"
                or host["authority"] != "observation_only"
                or host["scope"] != "current_execution_context"
                or type(host["schema_version"]) is not int or host["schema_version"] != 3):
            raise ValueError()
        observed = host["observed_at_unix_millis"]
        if not integer(observed, 1) or not 0 <= time.time_ns() // 1_000_000 - observed <= FRESH_SECONDS * 1000:
            raise ValueError()
        memory = host["memory"]["available_bytes"]
        cpus = host["cpu"]["logical_cpus"]
        pressure = [host["pressure"][kind]["avg10_micros"] for kind in ("cpu", "memory", "io")]
        if not integer(memory) or not integer(cpus, 1) or not all(integer(p) for p in pressure):
            raise ValueError()
    except (KeyError, TypeError, ValueError) as error:
        raise Refusal("incomplete local admission observation") from error
    # The reviewed demand is local adapter code, never remote request data. The operator reserve
    # remains installation policy. Unknown or unavailable host facts never become zero.
    if (memory < demand.memory_bytes + current["memory_reserve_bytes"]
            or cpus < demand.minimum_logical_cpus):
        raise Deferred("capacity_unavailable", "local admission capacity unavailable")
    high = any(p >= ceiling for p, ceiling in zip(pressure, (50_000_000, 1_000_000, 20_000_000)))
    raw = canonical({"schema_version": 1, "request": {"interference_class": "coexist"},
                     "observation": {"observed_at_unix_millis": time.time_ns() // 1_000_000,
                                     "node_control": current["node_control"],
                                     "pressure": "high" if high else "low",
                                     "candidate_quiet_compatibility": "unknown", "quiet_lease": None,
                                     "active": {"conflicting_non_yieldable": 0, "conflicting_yieldable": 0}}})
    answer = query(current["policy_executable"], [], raw)
    expected = {"document_type": "glaeda-local-admission-decision", "schema_version": 1,
                "input_sha256": digest(raw), "grants_authority": False, "authorizes_execution": False,
                "decision": {"disposition": "admit_now", "reason": "compatible",
                             "active_quiet_lease_generation": None, "requires_new_quiet_lease": False,
                             "requires_yieldable_drain": False, "grants_authority": False,
                             "authorizes_preemption": False, "authorizes_execution": False}}
    reason = {"held": "node_held", "draining": "node_draining"}.get(current["node_control"])
    if reason is None and high:
        reason = "pressure_high"
    if reason is not None:
        expected["decision"].update(disposition="refuse" if reason == "node_held" else "wait",
                                    reason=reason)
    if canonical(answer) != canonical(expected):
        raise Refusal("local interference admission refused")
    if time.monotonic() - started > FRESH_SECONDS:
        raise Refusal("local admission observation expired")
    if reason is not None:
        raise Deferred(reason, "local interference admission refused")
    return started + FRESH_SECONDS


def observe(root, demand=VERIFY_FOCUSED_DEMAND):
    """Disposable advisory snapshot. Never creates locks, reservations or launch state."""
    outcome, reason = "ready", "compatible"
    started = time.monotonic()
    store = None
    try:
        demand = validated_demand(demand)
        store = Store(root)
        current = policy(store)
        if store.read("reservation.json") is not None:
            outcome, reason = "wait", "reserved"
        else:
            try:
                check(current, demand)
            except Deferred as error:
                outcome, reason = "wait", error.reason
        # A changed policy/root invalidates this observation rather than adopting it.
        if policy(store) != current:
            raise Refusal("local admission policy changed during observation")
        store.verify_root()
        if time.monotonic() - started > FRESH_SECONDS:
            raise Refusal("local admission observation expired")
    except (Refusal, OSError, ValueError, subprocess.SubprocessError, RecursionError):
        outcome, reason = "refused", "observation_unavailable"
    finally:
        if store is not None:
            store.close()
    return {"document_type": "glaeda-owned-admission-observation", "schema_version": 1,
            "outcome": outcome, "reason": reason, "grants_authority": False,
            "authorizes_execution": False, "authorizes_redispatch": False}


class Reservation:
    def __init__(self, root, fingerprint, unit, binding, demand,
                 *, resume_existing=False):
        self.demand = validated_demand(demand)
        self.store = Store(root)
        self.identity = {"schema_version": RESERVATION_SCHEMA_VERSION,
                         "command_fingerprint": fingerprint, "unit": unit,
                         "binding_sha256": binding, "demand": demand_record(self.demand)}
        self.resume_existing = resume_existing
        self.launch_attempted = False
        self.owned = False
        self.phase = "preparing"

    @classmethod
    def resume(cls, root, fingerprint, unit, binding, demand=VERIFY_FOCUSED_DEMAND):
        """Reacquire one exact pre-launch reservation after a controller boundary.

        The reviewed demand is part of that exact identity: a resumed reservation
        rechecks the final launch boundary against the demand it was admitted under,
        and a different demand refuses instead of launching on smaller capacity.

        Only the preparing phase is resumable. A launching record is an ambiguous
        physical side effect and therefore remains recovery-only.
        """
        return cls(root, fingerprint, unit, binding, demand, resume_existing=True)

    def __enter__(self):
        self.lock = self.store.lock("slot.lock")
        try:
            self.lock.__enter__()
            with self.store.lock("policy.lock"):
                current = policy(self.store)
                existing = self.store.read("reservation.json")
                if self.resume_existing:
                    self.identity["generation"] = current["generation"]
                    if existing != {**self.identity, "phase": "preparing"}:
                        raise Refusal("local admission reservation is not exact resumable preparation")
                    self.owned = True
                    return self
                if existing is not None:
                    raise Refusal("previous local reservation requires exact recovery")
                check(current, self.demand)
                self.identity["generation"] = current["generation"]
                self.store.write("reservation.json", {**self.identity, "phase": "preparing"})
                self.owned = True
            return self
        except BaseException:
            self.lock.__exit__(None, None, None)
            self.store.close()
            raise

    @contextmanager
    def launch(self):
        with self.store.lock("policy.lock"):
            current = policy(self.store)
            if current["generation"] != self.identity["generation"]:
                raise Refusal("local admission installation changed")
            if self.store.read("reservation.json") != {**self.identity, "phase": "preparing"}:
                raise Refusal("local admission reservation changed")
            deadline = check(current, self.demand)
            self.store.write("reservation.json", {**self.identity, "phase": "launching"})
            self.phase = "launching"
            self.store.verify_root()
            if time.monotonic() > deadline:
                raise Refusal("local admission observation expired before launch")
            self.launch_attempted = True
            yield

    def release(self):
        expected = {**self.identity, "phase": self.phase}
        if self.store.read("reservation.json") != expected:
            raise Refusal("local admission reservation changed before release")
        self.store.remove("reservation.json")
        self.owned = False

    def __exit__(self, *error):
        self.lock.__exit__(*error)
        self.store.close()


def set_control(root, state):
    if state not in ("available", "held", "draining"):
        raise Refusal("invalid local node control")
    store = Store(root)
    try:
        with store.lock("policy.lock"):
            current = policy(store)
            if current["revision"] == 2**63 - 1:
                raise Refusal("local policy revision exhausted")
            current["node_control"] = state
            current["revision"] += 1
            store.write("policy.json", current)
    finally:
        store.close()


def recover(root, fingerprint, unit, binding, observe_settled, demand=VERIFY_FOCUSED_DEMAND):
    """Release only after the verifier has validated its exact terminal receipt.

    The callback re-observes exact unit/task absence and settles its matching intent.
    No PID liveness, age, or lock disappearance is sufficient recovery evidence.
    """
    record = demand_record(demand)
    store = Store(root)
    try:
        with store.lock("slot.lock"), store.lock("policy.lock"):
            current = policy(store)
            reservation = store.read("reservation.json")
            if reservation is None:
                return
            expected = {"schema_version": RESERVATION_SCHEMA_VERSION,
                        "command_fingerprint": fingerprint, "unit": unit,
                        "generation": current["generation"], "binding_sha256": binding,
                        "demand": record}
            if (reservation not in ({**expected, "phase": "preparing"},
                                    {**expected, "phase": "launching"})):
                raise Refusal("reservation does not match exact terminal recovery")
            observe_settled()
            store.remove("reservation.json")
    finally:
        store.close()
