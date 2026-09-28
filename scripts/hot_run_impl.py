#!/usr/bin/env python3
"""Run an ultra-trusted task worktree on a resident project's hot build tree.

This is a local development accelerator, not a security sandbox. It deliberately inherits the
caller's environment, host filesystem, devices, processes, and network. Bubblewrap supplies only a
stable project pathname plus explicit read-only, task-private directory, or task-private OverlayFS
cache views. A task-private directory may start empty or copy one exact warm resident parent.
"""

from __future__ import annotations

import argparse
import fcntl
import hashlib
import json
import math
import os
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import time
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
RESOURCE_PROFILE_PROPERTIES = {
    "big-red-heavy": (
        "CPUQuota=1200%",
        "MemoryHigh=8G",
        "MemoryMax=12G",
        "TasksMax=1024",
    ),
    "big-red-background": ("CPUWeight=25",),
}
TERMINATION_GRACE_SECONDS = 2.0
MAX_PROC_OBSERVATION_BYTES = 64 * 1024
MAX_TRACKED_PATH_BYTES = 32 * 1024 * 1024
FILE_COMPARE_CHUNK_BYTES = 1024 * 1024
PRESSURE_KINDS = ("cpu", "memory", "io")
SHA256_PREFIX = "sha256:"
HOT_STATE_SCHEMA_VERSION = 1
HOT_STATE_PRODUCER = "glaeda-hot-run-python-state-v1"
HOT_STATE_MANIFEST_SCHEMA_VERSION = 2
HOT_STATE_MANIFEST_PRODUCER = "glaeda-hot-run-python-state-v2"
HOT_STATE_MANIFEST_LEGACY_SCHEMA_VERSION = 1
HOT_STATE_MANIFEST_LEGACY_PRODUCER = "glaeda-hot-run-python-state-v1"
HOT_STATE_NAMESPACE_LEASE_PROTOCOL = "full_execution_shared_namespace_v1"
HOT_STATE_MANIFEST = "producer-manifest.json"
HOT_STATE_MANIFEST_STAGING_PREFIX = ".producer-manifest.json.creating-"
HOT_STATE_NAMESPACE_LOCK = ".namespace-lock"
HOT_STATE_VALUE_LOCK = ".value-lock-v2"
HOT_STATE_CREATING_PREFIX = ".creating-v1-"
HOT_STATE_RETIRED_PREFIX = ".retired-v1-"
HOT_STATE_RETIREMENT_RECORD_PREFIX = ".retirement-v1-"
HOT_STATE_RETIREMENT_RECORD_SUFFIX = ".json"
HOT_STATE_VALUE_CATALOG = ".value-catalog-v1.json"
HOT_STATE_VALUE_CATALOG_STAGING = ".value-catalog-v1.json.creating"
HOT_STATE_VALUE_CATALOG_SCHEMA_VERSION = 2
HOT_STATE_VALUE_CATALOG_PRODUCER = "glaeda-hot-run-value-catalog-v2"
HOT_STATE_VALUE_CATALOG_V1_SCHEMA_VERSION = 1
HOT_STATE_VALUE_CATALOG_V1_PRODUCER = "glaeda-hot-run-value-catalog-v1"
HOT_STATE_VALUE_CATALOG_V1_MAX_STATES = 256
HOT_STATE_VALUE_RECORDS = ".value-records-v2"
HOT_STATE_VALUE_RECORD_SCHEMA_VERSION = 1
HOT_STATE_VALUE_RECORD_PRODUCER = "glaeda-hot-run-value-record-v1"
HOT_STATE_VALUE_RECORD_SUFFIX = ".json"
HOT_STATE_VALUE_RECORD_STAGING_SUFFIX = ".creating"
HOT_STATE_VALUE_TICKETS = ".value-tickets-v2"
HOT_STATE_VALUE_TICKET_SCHEMA_VERSION = 1
HOT_STATE_VALUE_TICKET_PRODUCER = "glaeda-hot-run-value-ticket-v1"
HOT_STATE_VALUE_TICKET_SUFFIX = ".json"
HOT_STATE_RECONCILE_CATALOG = ".reconcile-v1.json"
HOT_STATE_RECONCILE_CATALOG_STAGING = ".reconcile-v1.json.creating"
HOT_STATE_RECONCILE_SCHEMA_VERSION = 1
HOT_STATE_RECONCILE_PRODUCER = "glaeda-hot-run-reconcile-v1"
HOT_STATE_RECONCILE_TICKETS = ".reconcile-tickets-v1"
HOT_STATE_RECONCILE_TICKET_SCHEMA_VERSION = 1
HOT_STATE_RECONCILE_TICKET_PRODUCER = "glaeda-hot-run-reconcile-ticket-v1"
HOT_STATE_RECONCILE_TICKET_SUFFIX = ".json"
HOT_STATE_RECONCILE_TICKETS_PER_PASS = 32
HOT_STATE_VALUE_TICKETS_PER_PASS = 32
MAX_HOT_STATE_MANIFEST_BYTES = 32 * 1024
MAX_HOT_STATE_VALUE_CATALOG_BYTES = 32 * 1024
MAX_HOT_STATE_VALUE_CATALOG_V1_BYTES = 256 * 1024
MAX_HOT_STATE_VALUE_RECORD_BYTES = 8 * 1024
MAX_HOT_STATE_VALUE_TICKET_BYTES = 4 * 1024
MAX_HOT_STATE_RECONCILE_CATALOG_BYTES = 4 * 1024
MAX_HOT_STATE_RECONCILE_TICKET_BYTES = 4 * 1024
MAX_HOT_STATE_CREATING_ENTRIES = 2
MAX_HOT_STATE_DELETE_ENTRIES = 2048
HOT_STATE_RETIRE_START_USED_PERCENT = 90
HOT_STATE_RETIRE_STOP_USED_PERCENT = 85
RENAME_NOREPLACE = 1


@dataclass(frozen=True)
class CacheSpec:
    path: Path
    mode: str


@dataclass(frozen=True)
class CachePreparation:
    path: Path
    mode: str
    disposition: str
    elapsed_seconds: float


@dataclass(frozen=True)
class SourcePreparation:
    mode: str
    disposition: str
    tracked_path_count: int
    normalized_regular_file_count: int
    differing_regular_file_count: int
    skipped_path_count: int
    elapsed_seconds: float


@dataclass(frozen=True)
class ExecutionObservation:
    elapsed_seconds: float
    preparation_elapsed_seconds: float


@dataclass(frozen=True)
class ManifestObjectIdentity:
    device: int
    inode: int
    creation_witness_ns: int


@dataclass(frozen=True)
class RuntimeContract:
    runtime_id: str
    program_sha256: str
    runtime_bin_binding_sha256: str | None = None


@dataclass(frozen=True)
class RuntimeDeclaration:
    runtime_id: str
    expected_program_sha256: str | None


@dataclass(frozen=True)
class RuntimeBinBinding:
    path: Path
    identity_sha256: str


@dataclass(frozen=True)
class DirectoryObjectIdentity:
    device: int
    inode: int


@dataclass(frozen=True)
class WorktreePointerIdentity:
    device: int
    inode: int
    creation_witness_ns: int


@dataclass(frozen=True)
class WorktreeStateIdentity:
    resident_root: DirectoryObjectIdentity
    task_root: DirectoryObjectIdentity
    common_git_directory: DirectoryObjectIdentity
    resident_git_directory: DirectoryObjectIdentity
    task_git_directory: DirectoryObjectIdentity
    resident_git_pointer: WorktreePointerIdentity
    task_git_pointer: WorktreePointerIdentity
    resident_git_relative: Path
    task_git_relative: Path


@dataclass(frozen=True)
class PinnedWorktreeState:
    identity: WorktreeStateIdentity
    resident_root_fd: int
    task_root_fd: int
    common_git_directory_fd: int
    resident_git_directory_fd: int
    task_git_directory_fd: int
    resident_git_pointer_fd: int
    task_git_pointer_fd: int

    def file_descriptors(self) -> tuple[int, ...]:
        return (
            self.resident_root_fd,
            self.task_root_fd,
            self.common_git_directory_fd,
            self.resident_git_directory_fd,
            self.task_git_directory_fd,
            self.resident_git_pointer_fd,
            self.task_git_pointer_fd,
        )


@dataclass
class DeleteBudget:
    remaining_entries: int


def nonnegative_finite(raw: str) -> float:
    value = float(raw)
    if not math.isfinite(value) or value < 0:
        raise ValueError("machine observation value is not finite and nonnegative")
    return value


def parse_load_average(raw: str) -> dict[str, float]:
    fields = raw.split()
    if len(fields) < 3:
        raise ValueError("load-average observation is incomplete")
    return {
        "one_minute": nonnegative_finite(fields[0]),
        "five_minutes": nonnegative_finite(fields[1]),
        "fifteen_minutes": nonnegative_finite(fields[2]),
    }


def parse_meminfo(raw: str) -> dict[str, int]:
    values: dict[str, int] = {}
    seen: set[str] = set()
    for line in raw.splitlines():
        key, separator, remainder = line.partition(":")
        if not separator or key in seen:
            raise ValueError("memory observation contains malformed or duplicate fields")
        seen.add(key)
        fields = remainder.split()
        if len(fields) != 2 or fields[1] != "kB":
            continue
        if (
            not fields[0].isdigit()
            or len(fields[0]) > 20
            or int(fields[0]) > (2**64 - 1) // 1024
        ):
            raise ValueError("memory observation value is out of range")
        values[key] = int(fields[0]) * 1024
    required = ("MemAvailable", "SwapTotal", "SwapFree")
    if any(key not in values for key in required):
        raise ValueError("memory observation is incomplete")
    if values["SwapFree"] > values["SwapTotal"]:
        raise ValueError("memory observation has inconsistent swap totals")
    return {
        "available_bytes": values["MemAvailable"],
        "swap_total_bytes": values["SwapTotal"],
        "swap_used_bytes": values["SwapTotal"] - values["SwapFree"],
    }


def parse_pressure(raw: str) -> dict[str, dict[str, float | int]]:
    result: dict[str, dict[str, float | int]] = {}
    for line in raw.splitlines():
        fields = line.split()
        if len(fields) != 5 or fields[0] not in ("some", "full"):
            raise ValueError("pressure observation has an unsupported shape")
        kind = fields[0]
        if kind in result:
            raise ValueError("pressure observation contains duplicate classes")
        pairs: dict[str, str] = {}
        for field in fields[1:]:
            key, separator, value = field.partition("=")
            if not separator or key in pairs:
                raise ValueError("pressure observation contains malformed fields")
            pairs[key] = value
        if set(pairs) != {"avg10", "avg60", "avg300", "total"}:
            raise ValueError("pressure observation is incomplete")
        if (
            not pairs["total"].isdigit()
            or len(pairs["total"]) > 20
            or int(pairs["total"]) > 2**64 - 1
        ):
            raise ValueError("pressure total is not a nonnegative integer")
        result[kind] = {
            "avg10": nonnegative_finite(pairs["avg10"]),
            "avg60": nonnegative_finite(pairs["avg60"]),
            "avg300": nonnegative_finite(pairs["avg300"]),
            "total_microseconds": int(pairs["total"]),
        }
    if "some" not in result:
        raise ValueError("pressure observation does not contain the some class")
    return result


def read_bounded(path: Path) -> str:
    with path.open("r", encoding="ascii") as source:
        raw = source.read(MAX_PROC_OBSERVATION_BYTES + 1)
    if len(raw) > MAX_PROC_OBSERVATION_BYTES:
        raise ValueError("machine observation exceeds the bounded input size")
    return raw


def optional_observation(
    path: Path, parser: Callable[[str], object]
) -> object | None:
    try:
        return parser(read_bounded(path))
    except (OSError, UnicodeError, ValueError):
        return None


def observe_machine() -> dict[str, object]:
    started = time.monotonic()
    online_cpus = os.cpu_count()
    try:
        allowed_cpus = len(os.sched_getaffinity(0))
    except (AttributeError, OSError):
        allowed_cpus = None
    load_average = optional_observation(Path("/proc/loadavg"), parse_load_average)
    memory = optional_observation(Path("/proc/meminfo"), parse_meminfo)
    pressure = {
        kind: optional_observation(Path(f"/proc/pressure/{kind}"), parse_pressure)
        for kind in PRESSURE_KINDS
    }
    facts = (
        online_cpus,
        allowed_cpus,
        load_average,
        memory,
        *pressure.values(),
    )
    present = sum(value is not None for value in facts)
    if present == len(facts):
        status = "observed"
    elif present:
        status = "partial"
    else:
        status = "unavailable"
    return {
        "status": status,
        "observation_elapsed_seconds": round(time.monotonic() - started, 6),
        "online_logical_cpus": online_cpus,
        "allowed_logical_cpus": allowed_cpus,
        "load_average": load_average,
        "memory": memory,
        "pressure": pressure,
    }


def nested_integer(observation: dict[str, object], *keys: str) -> int | None:
    value: object = observation
    for key in keys:
        if not isinstance(value, dict):
            return None
        value = value.get(key)
    if isinstance(value, bool) or not isinstance(value, int):
        return None
    return value


def signed_observation_delta(
    before: dict[str, object], after: dict[str, object], *keys: str
) -> int | None:
    before_value = nested_integer(before, *keys)
    after_value = nested_integer(after, *keys)
    if before_value is None or after_value is None:
        return None
    return after_value - before_value


def pressure_observation_interval(
    before: dict[str, object],
    after: dict[str, object],
    elapsed_seconds: float,
) -> dict[str, object]:
    pressure: dict[str, object] = {}
    for kind in PRESSURE_KINDS:
        classes: dict[str, object] = {}
        for pressure_class in ("some", "full"):
            before_total = nested_integer(
                before,
                "pressure",
                kind,
                pressure_class,
                "total_microseconds",
            )
            after_total = nested_integer(
                after,
                "pressure",
                kind,
                pressure_class,
                "total_microseconds",
            )
            delta = (
                after_total - before_total
                if before_total is not None
                and after_total is not None
                and after_total >= before_total
                else None
            )
            stall_fraction = (
                round(delta / (elapsed_seconds * 1_000_000), 9)
                if delta is not None
                and math.isfinite(elapsed_seconds)
                and elapsed_seconds > 0
                else None
            )
            classes[pressure_class] = {
                "total_microseconds_delta": delta,
                "stall_fraction_of_command_elapsed": stall_fraction,
            }
        pressure[kind] = classes
    return {
        "duration_basis": "command_elapsed",
        "elapsed_seconds": round(elapsed_seconds, 6),
        "memory": {
            "available_bytes_delta": signed_observation_delta(
                before, after, "memory", "available_bytes"
            ),
            "swap_used_bytes_delta": signed_observation_delta(
                before, after, "memory", "swap_used_bytes"
            ),
        },
        "pressure": pressure,
    }


def git_path(root: Path, argument: str) -> Path:
    git = shutil.which("git")
    if git is None:
        raise RuntimeError("required capability git is unavailable")
    result = subprocess.run(
        [os.path.abspath(git), "rev-parse", "--path-format=absolute", argument],
        cwd=root,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
        timeout=10,
        check=False,
    )
    if result.returncode != 0:
        raise RuntimeError(f"not a Git worktree: {root}")
    return Path(result.stdout.strip()).resolve()


def worktree_root(path: Path) -> Path:
    return git_path(path.resolve(), "--show-toplevel")


def common_git_directory(root: Path) -> Path:
    return git_path(root, "--git-common-dir")


def task_git_directory(root: Path) -> Path:
    return git_path(root, "--git-dir")


def observe_directory_object(path: Path, label: str) -> DirectoryObjectIdentity:
    try:
        details = path.lstat()
    except OSError as error:
        raise RuntimeError(f"{label} is unavailable") from error
    if stat.S_ISLNK(details.st_mode) or not stat.S_ISDIR(details.st_mode):
        raise RuntimeError(f"{label} is not a plain directory")
    if details.st_uid != os.getuid():
        raise RuntimeError(f"{label} is not owned by the current user")
    return DirectoryObjectIdentity(details.st_dev, details.st_ino)


def observe_worktree_pointer(root: Path, label: str) -> WorktreePointerIdentity:
    pointer = root / ".git"
    try:
        details = pointer.lstat()
    except OSError as error:
        raise RuntimeError(f"{label} is unavailable") from error
    if stat.S_ISLNK(details.st_mode) or not (
        stat.S_ISREG(details.st_mode) or stat.S_ISDIR(details.st_mode)
    ):
        raise RuntimeError(f"{label} is not a plain file or directory")
    if details.st_uid != os.getuid():
        raise RuntimeError(f"{label} is not owned by the current user")
    # Linked worktrees receive a regular .git pointer exactly once. Its ctime is
    # stable for that worktree's lifetime and distinguishes a replacement even
    # when the filesystem immediately recycles directory and file inodes. A main
    # worktree has a mutable .git directory, so only its object identity is used.
    creation_witness_ns = details.st_ctime_ns if stat.S_ISREG(details.st_mode) else 0
    return WorktreePointerIdentity(
        details.st_dev,
        details.st_ino,
        creation_witness_ns,
    )


def observe_worktree_state_identity(
    resident: Path,
    task: Path,
    common_git: Path,
    resident_git: Path,
    task_git: Path,
    resident_git_relative: Path,
    task_git_relative: Path,
) -> WorktreeStateIdentity:
    return WorktreeStateIdentity(
        resident_root=observe_directory_object(resident, "resident worktree root"),
        task_root=observe_directory_object(task, "task worktree root"),
        common_git_directory=observe_directory_object(
            common_git, "common Git directory"
        ),
        resident_git_directory=observe_directory_object(
            resident_git, "resident Git directory"
        ),
        task_git_directory=observe_directory_object(
            task_git, "task Git directory"
        ),
        resident_git_pointer=observe_worktree_pointer(
            resident, "resident worktree Git pointer"
        ),
        task_git_pointer=observe_worktree_pointer(task, "task worktree Git pointer"),
        resident_git_relative=resident_git_relative,
        task_git_relative=task_git_relative,
    )


def pin_plain_object(
    path: Path,
    label: str,
    *,
    directory: bool,
    dir_fd: int | None = None,
    require_current_user: bool = True,
) -> tuple[int, os.stat_result]:
    flags = os.O_PATH | os.O_CLOEXEC | os.O_NOFOLLOW
    if directory:
        flags |= os.O_DIRECTORY
    try:
        descriptor = os.open(path, flags, dir_fd=dir_fd)
    except OSError as error:
        raise RuntimeError(f"{label} is unavailable") from error
    try:
        details = os.fstat(descriptor)
        expected = stat.S_ISDIR(details.st_mode) if directory else (
            stat.S_ISREG(details.st_mode) or stat.S_ISDIR(details.st_mode)
        )
        if stat.S_ISLNK(details.st_mode) or not expected:
            raise RuntimeError(f"{label} is not a plain filesystem object")
        if require_current_user and details.st_uid != os.getuid():
            raise RuntimeError(f"{label} is not owned by the current user")
        return descriptor, details
    except BaseException:
        os.close(descriptor)
        raise


def pin_worktree_state(
    resident: Path,
    task: Path,
    common_git: Path,
    resident_git: Path,
    task_git: Path,
    resident_git_relative: Path,
    task_git_relative: Path,
) -> PinnedWorktreeState:
    opened: list[int] = []

    def pin(path: Path, label: str, *, directory: bool) -> os.stat_result:
        descriptor, details = pin_plain_object(
            path,
            label,
            directory=directory,
        )
        opened.append(descriptor)
        return details

    try:
        resident_details = pin(
            resident, "resident worktree root", directory=True
        )
        task_details = pin(task, "task worktree root", directory=True)
        common_git_details = pin(
            common_git, "common Git directory", directory=True
        )
        resident_git_details = pin(
            resident_git, "resident Git directory", directory=True
        )
        task_git_details = pin(task_git, "task Git directory", directory=True)
        resident_pointer_details = pin(
            resident / ".git",
            "resident worktree Git pointer",
            directory=False,
        )
        task_pointer_details = pin(
            task / ".git",
            "task worktree Git pointer",
            directory=False,
        )
        identity = WorktreeStateIdentity(
            resident_root=DirectoryObjectIdentity(
                resident_details.st_dev, resident_details.st_ino
            ),
            task_root=DirectoryObjectIdentity(task_details.st_dev, task_details.st_ino),
            common_git_directory=DirectoryObjectIdentity(
                common_git_details.st_dev, common_git_details.st_ino
            ),
            resident_git_directory=DirectoryObjectIdentity(
                resident_git_details.st_dev, resident_git_details.st_ino
            ),
            task_git_directory=DirectoryObjectIdentity(
                task_git_details.st_dev, task_git_details.st_ino
            ),
            resident_git_pointer=WorktreePointerIdentity(
                resident_pointer_details.st_dev,
                resident_pointer_details.st_ino,
                (
                    resident_pointer_details.st_ctime_ns
                    if stat.S_ISREG(resident_pointer_details.st_mode)
                    else 0
                ),
            ),
            task_git_pointer=WorktreePointerIdentity(
                task_pointer_details.st_dev,
                task_pointer_details.st_ino,
                (
                    task_pointer_details.st_ctime_ns
                    if stat.S_ISREG(task_pointer_details.st_mode)
                    else 0
                ),
            ),
            resident_git_relative=resident_git_relative,
            task_git_relative=task_git_relative,
        )
        return PinnedWorktreeState(identity, *opened)
    except BaseException:
        for descriptor in reversed(opened):
            os.close(descriptor)
        raise


def close_pinned_worktree_state(pinned: PinnedWorktreeState) -> None:
    for descriptor in reversed(pinned.file_descriptors()):
        os.close(descriptor)


def revalidate_worktree_state_identity(
    expected: WorktreeStateIdentity,
    resident: Path,
    task: Path,
    common_git: Path,
    resident_git: Path,
    task_git: Path,
    resident_git_relative: Path,
    task_git_relative: Path,
) -> None:
    observed = observe_worktree_state_identity(
        resident,
        task,
        common_git,
        resident_git,
        task_git,
        resident_git_relative,
        task_git_relative,
    )
    if observed != expected:
        raise RuntimeError("worktree generation changed during hot-state preflight")


def parse_cache_specs(values: list[str]) -> tuple[CacheSpec, ...]:
    specs: list[CacheSpec] = []
    for value in values:
        raw_path, separator, mode = value.rpartition(":")
        if not separator:
            raw_path, mode = value, "overlay"
        path = Path(raw_path)
        if (
            not raw_path
            or path.is_absolute()
            or path in (Path("."), Path(".."))
            or ".." in path.parts
            or mode not in ("native", "overlay", "private", "private-copy", "ro")
        ):
            raise RuntimeError(f"invalid cache specification: {value}")
        specs.append(CacheSpec(path, mode))
    if len({spec.path for spec in specs}) != len(specs):
        raise RuntimeError("cache paths must be unique")
    for index, left in enumerate(specs):
        for right in specs[index + 1 :]:
            if left.path in right.path.parents or right.path in left.path.parents:
                raise RuntimeError("cache paths must not overlap")
    return tuple(specs)


def default_cache_specs(resident: Path) -> tuple[CacheSpec, ...]:
    target = resident / "target"
    return (CacheSpec(Path("target"), "private-copy"),) if target.is_dir() else ()


def default_state_root(
    resident: Path,
    task: Path,
    cache_specs: tuple[CacheSpec, ...],
    worktree_identity: WorktreeStateIdentity,
) -> Path:
    cache_home = Path(
        os.environ.get("XDG_CACHE_HOME", os.path.join(Path.home(), ".cache"))
    )
    identity = default_state_identity(resident, task, cache_specs, worktree_identity)
    return cache_home / "glaeda" / "hot-run" / identity


def default_state_identity(
    resident: Path,
    task: Path,
    cache_specs: tuple[CacheSpec, ...],
    worktree_identity: WorktreeStateIdentity,
) -> str:
    # v3 prevents older hot-run implementations, which do not participate in the
    # namespace protocol, from opening a producer-managed state concurrently.
    digest = hashlib.sha256(b"glaeda-hot-run-default-state-v3\0")
    for value in (
        resident,
        task,
        worktree_identity.resident_git_relative,
        worktree_identity.task_git_relative,
    ):
        encoded = os.fsencode(value)
        digest.update(len(encoded).to_bytes(8, "big"))
        digest.update(encoded)
    for identity in (
        worktree_identity.resident_root,
        worktree_identity.task_root,
        worktree_identity.common_git_directory,
        worktree_identity.resident_git_directory,
        worktree_identity.task_git_directory,
    ):
        digest.update(identity.device.to_bytes(8, "big", signed=False))
        digest.update(identity.inode.to_bytes(8, "big", signed=False))
    for identity in (
        worktree_identity.resident_git_pointer,
        worktree_identity.task_git_pointer,
    ):
        digest.update(identity.device.to_bytes(8, "big", signed=False))
        digest.update(identity.inode.to_bytes(8, "big", signed=False))
        digest.update(identity.creation_witness_ns.to_bytes(8, "big", signed=False))
    for spec in cache_specs:
        encoded_path = os.fsencode(spec.path)
        encoded_mode = spec.mode.encode("ascii")
        digest.update(len(encoded_path).to_bytes(8, "big"))
        digest.update(encoded_path)
        digest.update(len(encoded_mode).to_bytes(8, "big"))
        digest.update(encoded_mode)
    return digest.hexdigest()


def runtime_state_root(base: Path, runtime: RuntimeContract | None) -> Path:
    if runtime is None:
        return base
    digest = hashlib.sha256(
        runtime.runtime_id.encode("ascii")
        + b"\0"
        + runtime.program_sha256.encode("ascii")
    )
    if runtime.runtime_bin_binding_sha256 is not None:
        digest.update(
            b"\0runtime-bin-first-v1\0"
            + runtime.runtime_bin_binding_sha256.encode("ascii")
        )
    identity = digest.hexdigest()
    return base / f"runtime-{identity}"


def state_identity_name(value: str) -> bool:
    return len(value) == 64 and all(
        character in "0123456789abcdef" for character in value
    )


def directory_identity_document(
    role: str, path: Path, identity: DirectoryObjectIdentity
) -> dict[str, object]:
    return {
        "role": role,
        "kind": "directory",
        "path": os.fspath(path),
        "device": identity.device,
        "inode": identity.inode,
    }


def pointer_identity_document(
    role: str, path: Path, identity: WorktreePointerIdentity
) -> dict[str, object]:
    return {
        "role": role,
        "kind": "worktree_pointer",
        "path": os.fspath(path),
        "device": identity.device,
        "inode": identity.inode,
        "creation_witness_ns": identity.creation_witness_ns,
    }


def producer_manifest_document(
    state_base: Path,
    resident: Path,
    task: Path,
    common_git: Path,
    resident_git: Path,
    task_git: Path,
    cache_specs: tuple[CacheSpec, ...],
    worktree_identity: WorktreeStateIdentity,
) -> dict[str, object]:
    expected_identity = default_state_identity(
        resident, task, cache_specs, worktree_identity
    )
    if state_base.name != expected_identity:
        raise RuntimeError("implicit hot-state identity is invalid")
    objects = (
        directory_identity_document(
            "resident_root", resident, worktree_identity.resident_root
        ),
        directory_identity_document("task_root", task, worktree_identity.task_root),
        directory_identity_document(
            "common_git_directory",
            common_git,
            worktree_identity.common_git_directory,
        ),
        directory_identity_document(
            "resident_git_directory",
            resident_git,
            worktree_identity.resident_git_directory,
        ),
        directory_identity_document(
            "task_git_directory", task_git, worktree_identity.task_git_directory
        ),
        pointer_identity_document(
            "resident_git_pointer",
            resident / ".git",
            worktree_identity.resident_git_pointer,
        ),
        pointer_identity_document(
            "task_git_pointer", task / ".git", worktree_identity.task_git_pointer
        ),
    )
    return {
        "schema_version": HOT_STATE_MANIFEST_SCHEMA_VERSION,
        "producer": HOT_STATE_MANIFEST_PRODUCER,
        "state_identity": state_base.name,
        "reconstructible": True,
        "namespace_lease_protocol": HOT_STATE_NAMESPACE_LEASE_PROTOCOL,
        "cache_views": [
            {"path": os.fspath(spec.path), "mode": spec.mode}
            for spec in cache_specs
        ],
        "generation_objects": list(objects),
    }


def manifest_generation_contract_equal(
    expected: dict[str, object],
    observed: dict[str, object],
) -> bool:
    return (
        expected.get("state_identity") == observed.get("state_identity")
        and expected.get("reconstructible") == observed.get("reconstructible")
        and expected.get("cache_views") == observed.get("cache_views")
        and expected.get("generation_objects") == observed.get("generation_objects")
    )


def canonical_manifest_bytes(document: dict[str, object]) -> bytes:
    encoded = (
        json.dumps(document, sort_keys=True, separators=(",", ":")) + "\n"
    ).encode("utf-8")
    if len(encoded) > MAX_HOT_STATE_MANIFEST_BYTES:
        raise RuntimeError("hot-state producer manifest exceeds its size bound")
    return encoded


def validate_manifest_document(
    document: object, expected_state_identity: str
) -> dict[str, object]:
    if not isinstance(document, dict):
        raise RuntimeError("hot-state producer manifest has an unsupported shape")
    legacy_keys = {
        "schema_version",
        "producer",
        "state_identity",
        "reconstructible",
        "cache_views",
        "generation_objects",
    }
    current_keys = legacy_keys | {"namespace_lease_protocol"}
    legacy = (
        set(document) == legacy_keys
        and document.get("schema_version")
        == HOT_STATE_MANIFEST_LEGACY_SCHEMA_VERSION
        and document.get("producer") == HOT_STATE_MANIFEST_LEGACY_PRODUCER
    )
    current = (
        set(document) == current_keys
        and document.get("schema_version") == HOT_STATE_MANIFEST_SCHEMA_VERSION
        and document.get("producer") == HOT_STATE_MANIFEST_PRODUCER
        and document.get("namespace_lease_protocol")
        == HOT_STATE_NAMESPACE_LEASE_PROTOCOL
    )
    if not legacy and not current:
        raise RuntimeError("hot-state producer manifest identity is not accepted")
    if (
        document["state_identity"] != expected_state_identity
        or not state_identity_name(expected_state_identity)
        or document["reconstructible"] is not True
    ):
        raise RuntimeError("hot-state producer manifest identity is not accepted")
    cache_views = document["cache_views"]
    if not isinstance(cache_views, list) or len(cache_views) > 32:
        raise RuntimeError("hot-state producer manifest cache views are invalid")
    for view in cache_views:
        if not isinstance(view, dict) or set(view) != {"path", "mode"}:
            raise RuntimeError("hot-state producer manifest cache view is invalid")
        path = view["path"]
        mode = view["mode"]
        if not isinstance(path, str) or not isinstance(mode, str):
            raise RuntimeError("hot-state producer manifest cache view is invalid")
        parsed = Path(path)
        if (
            not path
            or parsed.is_absolute()
            or parsed in (Path("."), Path(".."))
            or ".." in parsed.parts
            or mode not in ("overlay", "private", "private-copy", "ro")
        ):
            raise RuntimeError("hot-state producer manifest cache view is invalid")
    objects = document["generation_objects"]
    if not isinstance(objects, list) or len(objects) != 7:
        raise RuntimeError("hot-state producer manifest generation is invalid")
    expected_roles = (
        ("resident_root", "directory"),
        ("task_root", "directory"),
        ("common_git_directory", "directory"),
        ("resident_git_directory", "directory"),
        ("task_git_directory", "directory"),
        ("resident_git_pointer", "worktree_pointer"),
        ("task_git_pointer", "worktree_pointer"),
    )
    for item, (expected_role, expected_kind) in zip(objects, expected_roles):
        if not isinstance(item, dict):
            raise RuntimeError("hot-state producer manifest generation is invalid")
        kind = item.get("kind")
        expected_keys = (
            {"role", "kind", "path", "device", "inode"}
            if kind == "directory"
            else {
                "role",
                "kind",
                "path",
                "device",
                "inode",
                "creation_witness_ns",
            }
            if kind == "worktree_pointer"
            else set()
        )
        if (
            set(item) != expected_keys
            or item.get("role") != expected_role
            or kind != expected_kind
        ):
            raise RuntimeError("hot-state producer manifest generation is invalid")
        path = item["path"]
        if (
            not isinstance(path, str)
            or "\0" in path
            or not Path(path).is_absolute()
            or os.path.normpath(path) != path
        ):
            raise RuntimeError("hot-state producer manifest generation is invalid")
        for key in expected_keys - {"role", "kind", "path"}:
            value = item[key]
            if (
                isinstance(value, bool)
                or not isinstance(value, int)
                or value < 0
                or value > (1 << 64) - 1
            ):
                raise RuntimeError("hot-state producer manifest generation is invalid")
    return document


def manifest_has_full_execution_namespace_lease(
    document: dict[str, object],
) -> bool:
    return (
        document.get("schema_version") == HOT_STATE_MANIFEST_SCHEMA_VERSION
        and document.get("producer") == HOT_STATE_MANIFEST_PRODUCER
        and document.get("namespace_lease_protocol")
        == HOT_STATE_NAMESPACE_LEASE_PROTOCOL
    )


def recompute_manifest_state_identity(document: dict[str, object]) -> str:
    objects = document["generation_objects"]
    cache_views = document["cache_views"]
    assert isinstance(objects, list)
    assert isinstance(cache_views, list)
    by_role = {item["role"]: item for item in objects}
    resident = Path(by_role["resident_root"]["path"])
    task = Path(by_role["task_root"]["path"])
    common_git = Path(by_role["common_git_directory"]["path"])
    resident_git = Path(by_role["resident_git_directory"]["path"])
    task_git = Path(by_role["task_git_directory"]["path"])
    if (
        Path(by_role["resident_git_pointer"]["path"]) != resident / ".git"
        or Path(by_role["task_git_pointer"]["path"]) != task / ".git"
    ):
        raise RuntimeError(
            "hot-state producer manifest Git relationships are invalid"
        )
    try:
        resident_git_relative = resident_git.relative_to(common_git)
        task_git_relative = task_git.relative_to(common_git)
    except ValueError as error:
        raise RuntimeError(
            "hot-state producer manifest Git relationships are invalid"
        ) from error

    def directory(role: str) -> DirectoryObjectIdentity:
        item = by_role[role]
        return DirectoryObjectIdentity(item["device"], item["inode"])

    def pointer(role: str) -> WorktreePointerIdentity:
        item = by_role[role]
        return WorktreePointerIdentity(
            item["device"], item["inode"], item["creation_witness_ns"]
        )

    identity = WorktreeStateIdentity(
        resident_root=directory("resident_root"),
        task_root=directory("task_root"),
        common_git_directory=directory("common_git_directory"),
        resident_git_directory=directory("resident_git_directory"),
        task_git_directory=directory("task_git_directory"),
        resident_git_pointer=pointer("resident_git_pointer"),
        task_git_pointer=pointer("task_git_pointer"),
        resident_git_relative=resident_git_relative,
        task_git_relative=task_git_relative,
    )
    cache_specs = tuple(
        CacheSpec(Path(view["path"]), view["mode"]) for view in cache_views
    )
    return default_state_identity(resident, task, cache_specs, identity)


def read_producer_manifest_with_identity(
    directory: Path | int, state_identity: str
) -> tuple[dict[str, object], bytes, ManifestObjectIdentity]:
    descriptor = os.open(
        HOT_STATE_MANIFEST if isinstance(directory, int) else directory / HOT_STATE_MANIFEST,
        os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW,
        dir_fd=directory if isinstance(directory, int) else None,
    )
    try:
        details = os.fstat(descriptor)
        if (
            not stat.S_ISREG(details.st_mode)
            or details.st_uid != os.getuid()
            or details.st_nlink != 1
            or stat.S_IMODE(details.st_mode) != 0o600
            or details.st_size > MAX_HOT_STATE_MANIFEST_BYTES
        ):
            raise RuntimeError("hot-state producer manifest is not a private regular file")
        with os.fdopen(os.dup(descriptor), "rb") as source:
            encoded = source.read(MAX_HOT_STATE_MANIFEST_BYTES + 1)
        if len(encoded) > MAX_HOT_STATE_MANIFEST_BYTES:
            raise RuntimeError("hot-state producer manifest exceeds its size bound")
        try:
            document = json.loads(encoded)
        except (UnicodeError, json.JSONDecodeError) as error:
            raise RuntimeError("hot-state producer manifest is malformed") from error
        manifest = validate_manifest_document(document, state_identity)
        if canonical_manifest_bytes(manifest) != encoded:
            raise RuntimeError("hot-state producer manifest is not canonical")
        if recompute_manifest_state_identity(manifest) != state_identity:
            raise RuntimeError("hot-state producer manifest generation is not authentic")
        return (
            manifest,
            encoded,
            ManifestObjectIdentity(
                details.st_dev,
                details.st_ino,
                details.st_ctime_ns,
            ),
        )
    finally:
        os.close(descriptor)


def read_producer_manifest(
    directory: Path | int, state_identity: str
) -> tuple[dict[str, object], bytes]:
    manifest, encoded, _ = read_producer_manifest_with_identity(
        directory, state_identity
    )
    return manifest, encoded


def write_producer_manifest(directory: Path, encoded: bytes) -> None:
    staging: Path | None = None
    descriptor: int | None = None
    for attempt in range(32):
        candidate = directory / (
            f"{HOT_STATE_MANIFEST_STAGING_PREFIX}{os.getpid()}-"
            f"{time.time_ns()}-{attempt}"
        )
        try:
            descriptor = os.open(
                candidate,
                os.O_WRONLY
                | os.O_CREAT
                | os.O_EXCL
                | os.O_CLOEXEC
                | os.O_NOFOLLOW,
                0o600,
            )
            staging = candidate
            break
        except FileExistsError:
            continue
    if descriptor is None or staging is None:
        raise RuntimeError("could not allocate a hot-state manifest stage")
    try:
        written = 0
        while written < len(encoded):
            count = os.write(descriptor, encoded[written:])
            if count <= 0:
                raise OSError("hot-state producer manifest write made no progress")
            written += count
        os.fsync(descriptor)
    finally:
        os.close(descriptor)
    rename_noreplace(staging, directory / HOT_STATE_MANIFEST)
    fsync_directory(directory)


def rename_noreplace_at(
    source_directory: int,
    source_name: str,
    destination_directory: int,
    destination_name: str,
) -> None:
    # Keep ctypes off the warm path; it is needed only for publication or retirement.
    import ctypes

    libc = ctypes.CDLL(None, use_errno=True)
    rename = getattr(libc, "renameat2", None)
    if rename is None:
        raise RuntimeError("required capability renameat2 is unavailable")
    rename.argtypes = (
        ctypes.c_int,
        ctypes.c_char_p,
        ctypes.c_int,
        ctypes.c_char_p,
        ctypes.c_uint,
    )
    rename.restype = ctypes.c_int
    result = rename(
        source_directory,
        os.fsencode(source_name),
        destination_directory,
        os.fsencode(destination_name),
        RENAME_NOREPLACE,
    )
    if result != 0:
        error_number = ctypes.get_errno()
        raise OSError(
            error_number,
            os.strerror(error_number),
            destination_name,
        )


def rename_noreplace(source: Path, destination: Path) -> None:
    rename_noreplace_at(
        -100,
        os.fspath(source),
        -100,
        os.fspath(destination),
    )


def fsync_directory(path: Path) -> None:
    descriptor = os.open(
        path, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC | os.O_NOFOLLOW
    )
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def publish_implicit_state_base(
    state_base: Path, expected_manifest: dict[str, object]
) -> str:
    encoded = canonical_manifest_bytes(expected_manifest)
    try:
        details = state_base.lstat()
    except FileNotFoundError:
        details = None
    if details is not None:
        if (
            stat.S_ISLNK(details.st_mode)
            or not stat.S_ISDIR(details.st_mode)
            or details.st_uid != os.getuid()
            or stat.S_IMODE(details.st_mode) != 0o700
        ):
            raise RuntimeError(
                "implicit hot-state generation is not an owner-private directory"
            )
        try:
            observed_manifest, observed = read_producer_manifest(
                state_base, state_base.name
            )
        except FileNotFoundError:
            raise RuntimeError(
                "implicit hot-state generation collides with manifestless state"
            )
        if observed != encoded and not manifest_generation_contract_equal(
            expected_manifest, observed_manifest
        ):
            raise RuntimeError("hot-state producer manifest conflicts with this generation")
        return "reused"

    namespace_root = state_base.parent
    for attempt in range(32):
        staging = namespace_root / (
            f"{HOT_STATE_CREATING_PREFIX}{state_base.name}-"
            f"{os.getpid()}-{time.time_ns()}-{attempt}"
        )
        enqueue_hot_state_reconcile_ticket(
            namespace_root,
            "creating",
            state_base.name,
            staging.name,
        )
        try:
            staging.mkdir(mode=0o700)
            break
        except FileExistsError:
            continue
    else:
        raise RuntimeError("could not allocate a hot-state publication stage")
    # Tickets are published before mutation so a crash leaves either a
    # harmless stale ticket or exact recovery debt.
    write_producer_manifest(staging, encoded)
    enqueue_hot_state_reconcile_ticket(
        namespace_root,
        "state",
        state_base.name,
        state_base.name,
    )
    rename_noreplace(staging, state_base)
    fsync_directory(namespace_root)
    return "created"


def ensure_private_directory(path: Path) -> None:
    path.mkdir(mode=0o700, parents=True, exist_ok=True)
    details = path.lstat()
    if stat.S_ISLNK(details.st_mode) or not stat.S_ISDIR(details.st_mode):
        raise RuntimeError(f"hot-run state is not a directory: {path}")
    if details.st_uid != os.getuid():
        raise RuntimeError(f"hot-run state is not owned by the current user: {path}")
    path.chmod(0o700)


def scan_directory_bounded(
    directory: Path | int, maximum_entries: int
) -> list[os.DirEntry[str]] | None:
    entries: list[os.DirEntry[str]] = []
    with os.scandir(directory) as iterator:
        for entry in iterator:
            if len(entries) >= maximum_entries:
                return None
            entries.append(entry)
    return entries


def open_private_lock(path: Path, label: str) -> int:
    descriptor = os.open(
        path,
        os.O_CLOEXEC | os.O_CREAT | os.O_NOFOLLOW | os.O_RDWR,
        0o600,
    )
    try:
        details = os.fstat(descriptor)
        if (
            not stat.S_ISREG(details.st_mode)
            or details.st_uid != os.getuid()
            or details.st_nlink != 1
        ):
            raise RuntimeError(f"{label} is not a private regular file")
        os.fchmod(descriptor, 0o600)
        return descriptor
    except BaseException:
        os.close(descriptor)
        raise


def manifest_generation_reachable(document: dict[str, object]) -> bool | None:
    objects = document["generation_objects"]
    assert isinstance(objects, list)
    for item in objects:
        assert isinstance(item, dict)
        path = Path(item["path"])
        try:
            details = path.lstat()
        except FileNotFoundError:
            return False
        except OSError:
            return None
        if details.st_uid != os.getuid():
            return False
        if item["kind"] == "directory":
            if not stat.S_ISDIR(details.st_mode) or stat.S_ISLNK(details.st_mode):
                return False
        else:
            if stat.S_ISLNK(details.st_mode) or not (
                stat.S_ISREG(details.st_mode) or stat.S_ISDIR(details.st_mode)
            ):
                return False
            expected_witness = item["creation_witness_ns"]
            observed_witness = details.st_ctime_ns if stat.S_ISREG(details.st_mode) else 0
            if observed_witness != expected_witness:
                return False
        if details.st_dev != item["device"] or details.st_ino != item["inode"]:
            return False
    return True


def promoted_delete_name(details: os.stat_result) -> str:
    return (
        f".delete-v2-{details.st_dev:016x}-"
        f"{details.st_ino:016x}-{details.st_ctime_ns:016x}"
    )


def delete_directory_contents_bounded(
    descriptor: int,
    expected_device: int,
    budget: DeleteBudget,
    *,
    preserve_manifest: bool,
) -> bool:
    # Nested directories are promoted to the held retired-root descriptor one
    # level at a time. Every unlink, promotion, or rmdir spends one work unit.
    # Descriptor use is constant with tree depth; the mutated retired tree plus
    # its retirement record is durable continuation state for the next pass.
    while budget.remaining_entries > 0:
        selected_name: str | None = None
        try:
            with os.scandir(descriptor) as entries:
                for entry in entries:
                    if preserve_manifest and entry.name == HOT_STATE_MANIFEST:
                        continue
                    selected_name = entry.name
                    break
        except OSError:
            return False
        if selected_name is None:
            return True

        try:
            details = os.stat(
                selected_name,
                dir_fd=descriptor,
                follow_symlinks=False,
            )
        except FileNotFoundError:
            continue
        except OSError:
            return False
        if details.st_uid != os.getuid() or details.st_dev != expected_device:
            return False

        if stat.S_ISREG(details.st_mode) or stat.S_ISLNK(details.st_mode):
            try:
                os.unlink(selected_name, dir_fd=descriptor)
            except FileNotFoundError:
                continue
            except OSError:
                return False
            budget.remaining_entries -= 1
            continue

        if not stat.S_ISDIR(details.st_mode) or stat.S_ISLNK(details.st_mode):
            return False

        try:
            child = os.open(
                selected_name,
                os.O_RDONLY
                | os.O_DIRECTORY
                | os.O_CLOEXEC
                | os.O_NOFOLLOW,
                dir_fd=descriptor,
            )
        except OSError:
            return False
        try:
            pinned = os.fstat(child)
            if (
                pinned.st_dev != expected_device
                or pinned.st_uid != os.getuid()
                or pinned.st_ino != details.st_ino
            ):
                return False

            child_name: str | None = None
            try:
                with os.scandir(child) as child_entries:
                    for child_entry in child_entries:
                        child_name = child_entry.name
                        break
            except OSError:
                return False

            if child_name is None:
                try:
                    os.rmdir(selected_name, dir_fd=descriptor)
                except FileNotFoundError:
                    continue
                except OSError:
                    return False
                budget.remaining_entries -= 1
                continue

            try:
                child_details = os.stat(
                    child_name,
                    dir_fd=child,
                    follow_symlinks=False,
                )
            except FileNotFoundError:
                continue
            except OSError:
                return False
            if (
                child_details.st_uid != os.getuid()
                or child_details.st_dev != expected_device
            ):
                return False

            if (
                stat.S_ISREG(child_details.st_mode)
                or stat.S_ISLNK(child_details.st_mode)
            ):
                try:
                    os.unlink(child_name, dir_fd=child)
                except FileNotFoundError:
                    continue
                except OSError:
                    return False
                budget.remaining_entries -= 1
                continue

            if (
                not stat.S_ISDIR(child_details.st_mode)
                or stat.S_ISLNK(child_details.st_mode)
            ):
                return False

            promoted_name = promoted_delete_name(child_details)
            try:
                rename_noreplace_at(
                    child,
                    child_name,
                    descriptor,
                    promoted_name,
                )
            except (FileExistsError, OSError):
                return False
            try:
                promoted = os.stat(
                    promoted_name,
                    dir_fd=descriptor,
                    follow_symlinks=False,
                )
            except OSError:
                return False
            if (
                not stat.S_ISDIR(promoted.st_mode)
                or stat.S_ISLNK(promoted.st_mode)
                or promoted.st_uid != os.getuid()
                or promoted.st_dev != child_details.st_dev
                or promoted.st_ino != child_details.st_ino
            ):
                return False
            try:
                os.fsync(child)
                os.fsync(descriptor)
            except OSError:
                return False
            budget.remaining_entries -= 1
        finally:
            os.close(child)

    return False


def retirement_record_name(retired_name: str, state_identity: str) -> str:
    name_digest = hashlib.sha256(os.fsencode(retired_name)).hexdigest()[:16]
    return (
        f"{HOT_STATE_RETIREMENT_RECORD_PREFIX}{state_identity}-{name_digest}"
        f"{HOT_STATE_RETIREMENT_RECORD_SUFFIX}"
    )


def retirement_record_document(
    retired_name: str,
    state_identity: str,
    details: os.stat_result,
) -> dict[str, object]:
    return {
        "schema_version": HOT_STATE_SCHEMA_VERSION,
        "producer": HOT_STATE_PRODUCER,
        "state_identity": state_identity,
        "retired_name": retired_name,
        "directory_device": details.st_dev,
        "directory_inode": details.st_ino,
    }


def validate_retirement_record(document: object) -> dict[str, object]:
    if not isinstance(document, dict) or set(document) != {
        "schema_version",
        "producer",
        "state_identity",
        "retired_name",
        "directory_device",
        "directory_inode",
    }:
        raise RuntimeError("hot-state retirement record has an unsupported shape")
    state_identity = document["state_identity"]
    retired_name = document["retired_name"]
    if (
        document["schema_version"] != HOT_STATE_SCHEMA_VERSION
        or document["producer"] != HOT_STATE_PRODUCER
        or not isinstance(state_identity, str)
        or not state_identity_name(state_identity)
        or not isinstance(retired_name, str)
        or "/" in retired_name
        or retired_name in ("", ".", "..")
    ):
        raise RuntimeError("hot-state retirement record identity is not accepted")
    for key in ("directory_device", "directory_inode"):
        value = document[key]
        if isinstance(value, bool) or not isinstance(value, int) or value < 0:
            raise RuntimeError("hot-state retirement record identity is invalid")
    return document


def read_private_json(
    path: Path,
    label: str,
    maximum_bytes: int = MAX_HOT_STATE_MANIFEST_BYTES,
) -> tuple[dict[str, object], bytes]:
    descriptor = os.open(path, os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW)
    try:
        details = os.fstat(descriptor)
        if (
            not stat.S_ISREG(details.st_mode)
            or details.st_uid != os.getuid()
            or details.st_nlink != 1
            or stat.S_IMODE(details.st_mode) != 0o600
            or details.st_size > maximum_bytes
        ):
            raise RuntimeError(f"{label} is not a private regular file")
        with os.fdopen(os.dup(descriptor), "rb") as source:
            encoded = source.read(maximum_bytes + 1)
        if len(encoded) > maximum_bytes:
            raise RuntimeError(f"{label} exceeds its size bound")
        try:
            document = json.loads(encoded)
        except (UnicodeError, json.JSONDecodeError) as error:
            raise RuntimeError(f"{label} is malformed") from error
        if not isinstance(document, dict):
            raise RuntimeError(f"{label} has an unsupported shape")
        return document, encoded
    finally:
        os.close(descriptor)


def write_private_json_noreplace(path: Path, document: dict[str, object]) -> None:
    encoded = canonical_manifest_bytes(document)
    staging = path.parent / (
        f".{path.name}.creating-{os.getpid()}-{time.time_ns()}"
    )
    descriptor = os.open(
        staging,
        os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_CLOEXEC | os.O_NOFOLLOW,
        0o600,
    )
    try:
        written = 0
        while written < len(encoded):
            written += os.write(descriptor, encoded[written:])
        os.fsync(descriptor)
    finally:
        os.close(descriptor)
    rename_noreplace(staging, path)
    fsync_directory(path.parent)


def empty_hot_state_value_catalog() -> dict[str, object]:
    return {
        "schema_version": HOT_STATE_VALUE_CATALOG_SCHEMA_VERSION,
        "producer": HOT_STATE_VALUE_CATALOG_PRODUCER,
        "pressure_active": False,
        "retire_start_used_percent": HOT_STATE_RETIRE_START_USED_PERCENT,
        "retire_stop_used_percent": HOT_STATE_RETIRE_STOP_USED_PERCENT,
        "next_use_sequence": 0,
        "next_value_ticket_sequence": 0,
        "value_cursor_ticket_sequence": 0,
    }


def validate_hot_state_value_catalog_v1(document: object) -> dict[str, object]:
    if not isinstance(document, dict) or set(document) != {
        "schema_version",
        "producer",
        "pressure_active",
        "retire_start_used_percent",
        "retire_stop_used_percent",
        "next_use_sequence",
        "states",
    }:
        raise RuntimeError("hot-state v1 value catalog has an unsupported shape")
    sequence = document["next_use_sequence"]
    states = document["states"]
    if (
        document["schema_version"] != HOT_STATE_VALUE_CATALOG_V1_SCHEMA_VERSION
        or document["producer"] != HOT_STATE_VALUE_CATALOG_V1_PRODUCER
        or not isinstance(document["pressure_active"], bool)
        or document["retire_start_used_percent"] != HOT_STATE_RETIRE_START_USED_PERCENT
        or document["retire_stop_used_percent"] != HOT_STATE_RETIRE_STOP_USED_PERCENT
        or isinstance(sequence, bool)
        or not isinstance(sequence, int)
        or sequence < 0
        or not isinstance(states, dict)
        or len(states) > HOT_STATE_VALUE_CATALOG_V1_MAX_STATES
    ):
        raise RuntimeError("hot-state v1 value catalog identity is not accepted")
    for state_identity, record in states.items():
        validate_hot_state_value_record_fields(
            state_identity,
            record,
            sequence,
            value_ticket_required=False,
        )
    return document


def validate_hot_state_value_catalog(document: object) -> dict[str, object]:
    if not isinstance(document, dict) or set(document) != {
        "schema_version",
        "producer",
        "pressure_active",
        "retire_start_used_percent",
        "retire_stop_used_percent",
        "next_use_sequence",
        "next_value_ticket_sequence",
        "value_cursor_ticket_sequence",
    }:
        raise RuntimeError("hot-state value catalog has an unsupported shape")
    sequence = document["next_use_sequence"]
    next_ticket = document["next_value_ticket_sequence"]
    cursor = document["value_cursor_ticket_sequence"]
    if (
        document["schema_version"] != HOT_STATE_VALUE_CATALOG_SCHEMA_VERSION
        or document["producer"] != HOT_STATE_VALUE_CATALOG_PRODUCER
        or not isinstance(document["pressure_active"], bool)
        or document["retire_start_used_percent"] != HOT_STATE_RETIRE_START_USED_PERCENT
        or document["retire_stop_used_percent"] != HOT_STATE_RETIRE_STOP_USED_PERCENT
        or isinstance(sequence, bool)
        or not isinstance(sequence, int)
        or sequence < 0
        or isinstance(next_ticket, bool)
        or not isinstance(next_ticket, int)
        or next_ticket < 0
        or isinstance(cursor, bool)
        or not isinstance(cursor, int)
        or cursor < 0
        or cursor > next_ticket
    ):
        raise RuntimeError("hot-state value catalog identity is not accepted")
    return document


def validate_hot_state_value_record_fields(
    state_identity: object,
    record: object,
    maximum_sequence: int | None = None,
    *,
    value_ticket_required: bool = True,
) -> dict[str, object]:
    record_keys = {
        "manifest_device",
        "manifest_inode",
        "manifest_creation_witness_ns",
        "last_successful_use_sequence",
        "successful_use_count",
        "value_identity",
        "reconstruction_elapsed_ns",
        "reuse_elapsed_ns",
    }
    if value_ticket_required:
        record_keys.add("value_ticket_sequence")
    if (
        not isinstance(state_identity, str)
        or not state_identity_name(state_identity)
        or not isinstance(record, dict)
        or set(record) != record_keys
    ):
        raise RuntimeError("hot-state value record is invalid")
    integer_keys = [
        "manifest_device",
        "manifest_inode",
        "manifest_creation_witness_ns",
        "last_successful_use_sequence",
        "successful_use_count",
    ]
    if value_ticket_required:
        integer_keys.append("value_ticket_sequence")
    for key in integer_keys:
        value = record[key]
        if isinstance(value, bool) or not isinstance(value, int) or value < 0:
            raise RuntimeError("hot-state value record is invalid")
    if (
        record["last_successful_use_sequence"] == 0
        or record["successful_use_count"] == 0
        or (
            value_ticket_required
            and record["value_ticket_sequence"] == 0
        )
        or (
            maximum_sequence is not None
            and record["last_successful_use_sequence"] > maximum_sequence
        )
    ):
        raise RuntimeError("hot-state value record sequence is invalid")
    value_identity = record["value_identity"]
    if value_identity is not None and (
        not isinstance(value_identity, str)
        or not state_identity_name(value_identity)
    ):
        raise RuntimeError("hot-state value record value identity is invalid")
    for key in ("reconstruction_elapsed_ns", "reuse_elapsed_ns"):
        value = record[key]
        if value is not None and (
            isinstance(value, bool)
            or not isinstance(value, int)
            or value <= 0
        ):
            raise RuntimeError("hot-state value record timing is invalid")
    if value_identity is None and (
        record["reconstruction_elapsed_ns"] is not None
        or record["reuse_elapsed_ns"] is not None
    ):
        raise RuntimeError("hot-state value record timing is unbound")
    return record


def hot_state_value_record_document(
    state_identity: str, record: dict[str, object]
) -> dict[str, object]:
    validate_hot_state_value_record_fields(state_identity, record)
    return {
        "schema_version": HOT_STATE_VALUE_RECORD_SCHEMA_VERSION,
        "producer": HOT_STATE_VALUE_RECORD_PRODUCER,
        "state_identity": state_identity,
        **record,
    }


def validate_hot_state_value_record(document: object) -> dict[str, object]:
    if not isinstance(document, dict):
        raise RuntimeError("hot-state value record has an unsupported shape")
    expected_keys = {
        "schema_version",
        "producer",
        "state_identity",
        "manifest_device",
        "manifest_inode",
        "manifest_creation_witness_ns",
        "last_successful_use_sequence",
        "successful_use_count",
        "value_identity",
        "reconstruction_elapsed_ns",
        "reuse_elapsed_ns",
        "value_ticket_sequence",
    }
    if set(document) != expected_keys:
        raise RuntimeError("hot-state value record has an unsupported shape")
    if (
        document["schema_version"] != HOT_STATE_VALUE_RECORD_SCHEMA_VERSION
        or document["producer"] != HOT_STATE_VALUE_RECORD_PRODUCER
    ):
        raise RuntimeError("hot-state value record identity is not accepted")
    state_identity = document["state_identity"]
    record = {key: value for key, value in document.items() if key not in {
        "schema_version", "producer", "state_identity"
    }}
    validate_hot_state_value_record_fields(state_identity, record)
    return document


def canonical_hot_state_value_catalog_bytes(document: dict[str, object]) -> bytes:
    validate_hot_state_value_catalog(document)
    encoded = (
        json.dumps(document, sort_keys=True, separators=(",", ":")) + "\n"
    ).encode("utf-8")
    if len(encoded) > MAX_HOT_STATE_VALUE_CATALOG_BYTES:
        raise RuntimeError("hot-state value catalog exceeds its byte budget")
    return encoded


def canonical_hot_state_value_record_bytes(document: dict[str, object]) -> bytes:
    validate_hot_state_value_record(document)
    encoded = (
        json.dumps(document, sort_keys=True, separators=(",", ":")) + "\n"
    ).encode("utf-8")
    if len(encoded) > MAX_HOT_STATE_VALUE_RECORD_BYTES:
        raise RuntimeError("hot-state value record exceeds its byte budget")
    return encoded


def hot_state_value_records_root(namespace_root: Path) -> Path:
    return namespace_root / HOT_STATE_VALUE_RECORDS


def validate_hot_state_value_records_root(
    namespace_root: Path, *, create: bool
) -> Path | None:
    root = hot_state_value_records_root(namespace_root)
    if create:
        try:
            root.mkdir(mode=0o700)
        except FileExistsError:
            pass
    try:
        details = root.stat(follow_symlinks=False)
    except FileNotFoundError:
        if create:
            raise
        return None
    namespace_details = namespace_root.stat(follow_symlinks=False)
    if (
        not stat.S_ISDIR(details.st_mode)
        or stat.S_ISLNK(details.st_mode)
        or details.st_uid != os.getuid()
        or stat.S_IMODE(details.st_mode) != 0o700
        or details.st_dev != namespace_details.st_dev
    ):
        raise RuntimeError("hot-state value-record root is not owner-private")
    return root


def ensure_hot_state_value_records_root(namespace_root: Path) -> Path:
    root = validate_hot_state_value_records_root(namespace_root, create=True)
    assert root is not None
    return root


def read_hot_state_value_record(
    namespace_root: Path, state_identity: str
) -> dict[str, object] | None:
    root = validate_hot_state_value_records_root(
        namespace_root, create=False
    )
    if root is None:
        return None
    if not state_identity_name(state_identity):
        raise RuntimeError("hot-state value record state identity is invalid")
    path = root / (state_identity + HOT_STATE_VALUE_RECORD_SUFFIX)
    try:
        document, encoded = read_private_json(
            path, "hot-state value record", MAX_HOT_STATE_VALUE_RECORD_BYTES
        )
    except FileNotFoundError:
        return None
    record = validate_hot_state_value_record(document)
    if record["state_identity"] != state_identity:
        raise RuntimeError("hot-state value record filename conflicts with identity")
    if canonical_hot_state_value_record_bytes(record) != encoded:
        raise RuntimeError("hot-state value record is not canonical")
    return record


def write_hot_state_value_record(
    namespace_root: Path,
    state_identity: str,
    record_fields: dict[str, object],
) -> None:
    root = ensure_hot_state_value_records_root(namespace_root)
    document = hot_state_value_record_document(state_identity, record_fields)
    encoded = canonical_hot_state_value_record_bytes(document)
    final = root / (state_identity + HOT_STATE_VALUE_RECORD_SUFFIX)
    staging = root / (
        "." + state_identity + HOT_STATE_VALUE_RECORD_SUFFIX
        + HOT_STATE_VALUE_RECORD_STAGING_SUFFIX
    )
    try:
        details = staging.stat(follow_symlinks=False)
    except FileNotFoundError:
        pass
    else:
        if (
            not stat.S_ISREG(details.st_mode)
            or details.st_uid != os.getuid()
            or details.st_nlink != 1
            or stat.S_IMODE(details.st_mode) != 0o600
            or details.st_size > MAX_HOT_STATE_VALUE_RECORD_BYTES
        ):
            raise RuntimeError("hot-state value record stage is not recoverable")
        staging.unlink()
        fsync_directory(root)
    descriptor = os.open(
        staging,
        os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_CLOEXEC | os.O_NOFOLLOW,
        0o600,
    )
    try:
        written = 0
        while written < len(encoded):
            count = os.write(descriptor, encoded[written:])
            if count <= 0:
                raise OSError("hot-state value record write made no progress")
            written += count
        os.fsync(descriptor)
    finally:
        os.close(descriptor)
    os.replace(staging, final)
    fsync_directory(root)


def remove_hot_state_value_record(
    namespace_root: Path, state_identity: str
) -> bool:
    root = validate_hot_state_value_records_root(
        namespace_root, create=False
    )
    if root is None:
        return False
    if not state_identity_name(state_identity):
        raise RuntimeError("hot-state value record state identity is invalid")
    path = root / (state_identity + HOT_STATE_VALUE_RECORD_SUFFIX)
    try:
        path.unlink()
    except FileNotFoundError:
        return False
    fsync_directory(root)
    return True


def hot_state_value_tickets_root(namespace_root: Path) -> Path:
    return namespace_root / HOT_STATE_VALUE_TICKETS


def validate_hot_state_value_tickets_root(
    namespace_root: Path, *, create: bool
) -> Path | None:
    root = hot_state_value_tickets_root(namespace_root)
    if create:
        try:
            root.mkdir(mode=0o700)
        except FileExistsError:
            pass
    try:
        details = root.stat(follow_symlinks=False)
    except FileNotFoundError:
        if create:
            raise
        return None
    namespace_details = namespace_root.stat(follow_symlinks=False)
    if (
        not stat.S_ISDIR(details.st_mode)
        or stat.S_ISLNK(details.st_mode)
        or details.st_uid != os.getuid()
        or stat.S_IMODE(details.st_mode) != 0o700
        or details.st_dev != namespace_details.st_dev
    ):
        raise RuntimeError("hot-state value-ticket root is not owner-private")
    return root


def value_ticket_name(ticket_sequence: int) -> str:
    if ticket_sequence <= 0:
        raise RuntimeError("hot-state value ticket sequence is invalid")
    return f"{ticket_sequence:020d}{HOT_STATE_VALUE_TICKET_SUFFIX}"


def value_ticket_document(
    ticket_sequence: int,
    state_identity: str,
    last_successful_use_sequence: int,
) -> dict[str, object]:
    if (
        ticket_sequence <= 0
        or last_successful_use_sequence <= 0
        or not state_identity_name(state_identity)
    ):
        raise RuntimeError("hot-state value ticket is invalid")
    return {
        "schema_version": HOT_STATE_VALUE_TICKET_SCHEMA_VERSION,
        "producer": HOT_STATE_VALUE_TICKET_PRODUCER,
        "ticket_sequence": ticket_sequence,
        "state_identity": state_identity,
        "last_successful_use_sequence": last_successful_use_sequence,
    }


def validate_hot_state_value_ticket(document: object) -> dict[str, object]:
    if not isinstance(document, dict) or set(document) != {
        "schema_version",
        "producer",
        "ticket_sequence",
        "state_identity",
        "last_successful_use_sequence",
    }:
        raise RuntimeError("hot-state value ticket has an unsupported shape")
    if (
        document["schema_version"] != HOT_STATE_VALUE_TICKET_SCHEMA_VERSION
        or document["producer"] != HOT_STATE_VALUE_TICKET_PRODUCER
        or isinstance(document["ticket_sequence"], bool)
        or not isinstance(document["ticket_sequence"], int)
        or document["ticket_sequence"] <= 0
        or not isinstance(document["state_identity"], str)
        or not state_identity_name(document["state_identity"])
        or isinstance(document["last_successful_use_sequence"], bool)
        or not isinstance(document["last_successful_use_sequence"], int)
        or document["last_successful_use_sequence"] <= 0
    ):
        raise RuntimeError("hot-state value ticket identity is invalid")
    return document


def canonical_hot_state_value_ticket_bytes(
    document: dict[str, object],
) -> bytes:
    validate_hot_state_value_ticket(document)
    encoded = (
        json.dumps(document, sort_keys=True, separators=(",", ":")) + "\n"
    ).encode("utf-8")
    if len(encoded) > MAX_HOT_STATE_VALUE_TICKET_BYTES:
        raise RuntimeError("hot-state value ticket exceeds its byte budget")
    return encoded


def read_hot_state_value_ticket(
    namespace_root: Path, ticket_sequence: int
) -> dict[str, object] | None:
    root = validate_hot_state_value_tickets_root(
        namespace_root, create=False
    )
    if root is None:
        return None
    path = root / value_ticket_name(ticket_sequence)
    try:
        document, encoded = read_private_json(
            path, "hot-state value ticket", MAX_HOT_STATE_VALUE_TICKET_BYTES
        )
    except FileNotFoundError:
        return None
    ticket = validate_hot_state_value_ticket(document)
    if ticket["ticket_sequence"] != ticket_sequence:
        raise RuntimeError("hot-state value ticket filename conflicts with identity")
    if canonical_hot_state_value_ticket_bytes(ticket) != encoded:
        raise RuntimeError("hot-state value ticket is not canonical")
    return ticket


def write_hot_state_value_ticket(
    namespace_root: Path,
    ticket_sequence: int,
    state_identity: str,
    last_successful_use_sequence: int,
) -> None:
    root = validate_hot_state_value_tickets_root(
        namespace_root, create=True
    )
    assert root is not None
    document = value_ticket_document(
        ticket_sequence,
        state_identity,
        last_successful_use_sequence,
    )
    encoded = canonical_hot_state_value_ticket_bytes(document)
    path = root / value_ticket_name(ticket_sequence)
    descriptor = os.open(
        path,
        os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_CLOEXEC | os.O_NOFOLLOW,
        0o600,
    )
    try:
        written = 0
        while written < len(encoded):
            count = os.write(descriptor, encoded[written:])
            if count <= 0:
                raise OSError("hot-state value ticket write made no progress")
            written += count
        os.fsync(descriptor)
    finally:
        os.close(descriptor)
    fsync_directory(root)


def remove_hot_state_value_ticket(
    namespace_root: Path, ticket_sequence: int
) -> bool:
    root = validate_hot_state_value_tickets_root(
        namespace_root, create=False
    )
    if root is None:
        return False
    path = root / value_ticket_name(ticket_sequence)
    try:
        path.unlink()
    except FileNotFoundError:
        return False
    fsync_directory(root)
    return True


def remove_stale_hot_state_value_catalog_stage(namespace_root: Path) -> bool:
    staging = namespace_root / HOT_STATE_VALUE_CATALOG_STAGING
    try:
        details = staging.stat(follow_symlinks=False)
    except FileNotFoundError:
        return False
    if (
        not stat.S_ISREG(details.st_mode)
        or details.st_uid != os.getuid()
        or details.st_nlink != 1
        or stat.S_IMODE(details.st_mode) != 0o600
        or details.st_size > MAX_HOT_STATE_VALUE_CATALOG_V1_BYTES
    ):
        raise RuntimeError("hot-state value catalog stage is not recoverable")
    staging.unlink()
    fsync_directory(namespace_root)
    return True


def write_hot_state_value_catalog(
    namespace_root: Path, document: dict[str, object]
) -> None:
    encoded = canonical_hot_state_value_catalog_bytes(document)
    remove_stale_hot_state_value_catalog_stage(namespace_root)
    staging = namespace_root / HOT_STATE_VALUE_CATALOG_STAGING
    descriptor = os.open(
        staging,
        os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_CLOEXEC | os.O_NOFOLLOW,
        0o600,
    )
    try:
        written = 0
        while written < len(encoded):
            count = os.write(descriptor, encoded[written:])
            if count <= 0:
                raise OSError("hot-state value catalog write made no progress")
            written += count
        os.fsync(descriptor)
    finally:
        os.close(descriptor)
    os.replace(staging, namespace_root / HOT_STATE_VALUE_CATALOG)
    fsync_directory(namespace_root)


def migrate_hot_state_value_catalog_v1(
    namespace_root: Path, catalog: dict[str, object]
) -> dict[str, object]:
    validated = validate_hot_state_value_catalog_v1(catalog)
    sequence = validated["next_use_sequence"]
    states = validated["states"]
    assert isinstance(sequence, int)
    assert isinstance(states, dict)
    ensure_hot_state_value_records_root(namespace_root)
    validate_hot_state_value_tickets_root(namespace_root, create=True)

    ordered = sorted(
        states.items(),
        key=lambda item: (
            int(item[1]["last_successful_use_sequence"]),
            item[0],
        ),
    )
    for ticket_sequence, (state_identity, legacy_fields) in enumerate(
        ordered, start=1
    ):
        assert isinstance(state_identity, str)
        assert isinstance(legacy_fields, dict)
        fields = {
            **legacy_fields,
            "value_ticket_sequence": ticket_sequence,
        }
        expected_ticket = value_ticket_document(
            ticket_sequence,
            state_identity,
            int(legacy_fields["last_successful_use_sequence"]),
        )
        observed_ticket = read_hot_state_value_ticket(
            namespace_root, ticket_sequence
        )
        if observed_ticket is None:
            write_hot_state_value_ticket(
                namespace_root,
                ticket_sequence,
                state_identity,
                int(legacy_fields["last_successful_use_sequence"]),
            )
        elif canonical_hot_state_value_ticket_bytes(observed_ticket) != (
            canonical_hot_state_value_ticket_bytes(expected_ticket)
        ):
            raise RuntimeError("hot-state v1 migration conflicts with value ticket")

        existing = read_hot_state_value_record(namespace_root, state_identity)
        expected = hot_state_value_record_document(state_identity, fields)
        if existing is None:
            write_hot_state_value_record(namespace_root, state_identity, fields)
        elif canonical_hot_state_value_record_bytes(existing) != (
            canonical_hot_state_value_record_bytes(expected)
        ):
            raise RuntimeError("hot-state v1 migration conflicts with value record")

    migrated = {
        "schema_version": HOT_STATE_VALUE_CATALOG_SCHEMA_VERSION,
        "producer": HOT_STATE_VALUE_CATALOG_PRODUCER,
        "pressure_active": validated["pressure_active"],
        "retire_start_used_percent": HOT_STATE_RETIRE_START_USED_PERCENT,
        "retire_stop_used_percent": HOT_STATE_RETIRE_STOP_USED_PERCENT,
        "next_use_sequence": sequence,
        "next_value_ticket_sequence": len(ordered),
        "value_cursor_ticket_sequence": 0,
    }
    write_hot_state_value_catalog(namespace_root, migrated)
    return migrated


def read_hot_state_value_catalog(namespace_root: Path) -> dict[str, object]:
    path = namespace_root / HOT_STATE_VALUE_CATALOG
    try:
        document, encoded = read_private_json(
            path,
            "hot-state value catalog",
            MAX_HOT_STATE_VALUE_CATALOG_V1_BYTES,
        )
    except FileNotFoundError:
        return empty_hot_state_value_catalog()
    if document.get("schema_version") == HOT_STATE_VALUE_CATALOG_V1_SCHEMA_VERSION:
        legacy = validate_hot_state_value_catalog_v1(document)
        legacy_encoded = (
            json.dumps(legacy, sort_keys=True, separators=(",", ":")) + "\n"
        ).encode("utf-8")
        if legacy_encoded != encoded:
            raise RuntimeError("hot-state v1 value catalog is not canonical")
        return migrate_hot_state_value_catalog_v1(namespace_root, legacy)
    catalog = validate_hot_state_value_catalog(document)
    if canonical_hot_state_value_catalog_bytes(catalog) != encoded:
        raise RuntimeError("hot-state value catalog is not canonical")
    return catalog

def hot_state_value_identity(
    comparison_key: str,
    runtime_contract: RuntimeContract | None,
    resource_profile: str | None,
) -> str:
    digest = hashlib.sha256(b"glaeda-hot-state-value-v1\0")
    for value in (
        comparison_key,
        runtime_contract.runtime_id if runtime_contract is not None else "",
        runtime_contract.program_sha256 if runtime_contract is not None else "",
        (
            runtime_contract.runtime_bin_binding_sha256 or ""
            if runtime_contract is not None
            else ""
        ),
        resource_profile or "",
    ):
        encoded = value.encode("ascii")
        digest.update(len(encoded).to_bytes(8, "big"))
        digest.update(encoded)
    return digest.hexdigest()


def elapsed_nanoseconds(seconds: float) -> int:
    if not math.isfinite(seconds) or seconds < 0:
        raise RuntimeError("hot-state successful-use duration is invalid")
    return max(1, round(seconds * 1_000_000_000))


def record_successful_hot_state_use(
    namespace_root: Path,
    state_base: Path,
    publication_disposition: str,
    comparison_key: str | None,
    runtime_contract: RuntimeContract | None,
    resource_profile: str | None,
    observation: ExecutionObservation,
    namespace_lease_fd: int | None = None,
) -> str:
    owned_namespace_lease: int | None = None
    value_lock: int | None = None
    try:
        if namespace_lease_fd is None:
            owned_namespace_lease = open_private_lock(
                namespace_root / HOT_STATE_NAMESPACE_LOCK,
                "hot-state namespace lock",
            )
            fcntl.flock(owned_namespace_lease, fcntl.LOCK_SH)
        value_lock = open_private_lock(
            namespace_root / HOT_STATE_VALUE_LOCK,
            "hot-state value writer lock",
        )
        fcntl.flock(value_lock, fcntl.LOCK_EX)
        namespace_details = namespace_root.stat(follow_symlinks=False)
        state_details = state_base.stat(follow_symlinks=False)
        if (
            not stat.S_ISDIR(namespace_details.st_mode)
            or stat.S_ISLNK(namespace_details.st_mode)
            or namespace_details.st_uid != os.getuid()
            or stat.S_IMODE(namespace_details.st_mode) != 0o700
            or not stat.S_ISDIR(state_details.st_mode)
            or stat.S_ISLNK(state_details.st_mode)
            or state_details.st_uid != os.getuid()
            or stat.S_IMODE(state_details.st_mode) != 0o700
            or state_details.st_dev != namespace_details.st_dev
        ):
            raise RuntimeError("hot-state successful use is not producer-owned")
        manifest, _, manifest_identity = read_producer_manifest_with_identity(
            state_base, state_base.name
        )
        if manifest_generation_reachable(manifest) is not True:
            return "generation_not_current"
        remove_stale_hot_state_value_catalog_stage(namespace_root)
        catalog = read_hot_state_value_catalog(namespace_root)
        sequence = catalog["next_use_sequence"]
        next_ticket_sequence = catalog["next_value_ticket_sequence"]
        assert isinstance(sequence, int)
        assert isinstance(next_ticket_sequence, int)
        try:
            prior_document = read_hot_state_value_record(
                namespace_root, state_base.name
            )
        except RuntimeError:
            # This successful use has already rebound the exact live state to
            # its current producer manifest. Corrupt value metadata has no
            # authority of its own and can be replaced from that fresh fact.
            ensure_hot_state_value_records_root(namespace_root)
            remove_hot_state_value_record(namespace_root, state_base.name)
            prior_document = None
        prior = None
        if prior_document is not None:
            prior = {
                key: value for key, value in prior_document.items()
                if key not in {"schema_version", "producer", "state_identity"}
            }
        same_manifest = isinstance(prior, dict) and (
            prior["manifest_device"] == manifest_identity.device
            and prior["manifest_inode"] == manifest_identity.inode
            and prior["manifest_creation_witness_ns"]
            == manifest_identity.creation_witness_ns
        )
        next_sequence = sequence + 1
        new_ticket_sequence = next_ticket_sequence + 1
        old_ticket_sequence = (
            int(prior["value_ticket_sequence"])
            if isinstance(prior, dict)
            else None
        )
        successful_use_count = (
            prior["successful_use_count"] + 1
            if same_manifest
            else 1
        )
        value_identity: str | None = None
        reconstruction_elapsed_ns: int | None = None
        reuse_elapsed_ns: int | None = None
        if same_manifest:
            value_identity = prior["value_identity"]
            reconstruction_elapsed_ns = prior["reconstruction_elapsed_ns"]
            reuse_elapsed_ns = prior["reuse_elapsed_ns"]
        if comparison_key is not None:
            observed_value_identity = hot_state_value_identity(
                comparison_key, runtime_contract, resource_profile
            )
            total_elapsed_ns = elapsed_nanoseconds(
                observation.elapsed_seconds
                + observation.preparation_elapsed_seconds
            )
            if publication_disposition == "created":
                value_identity = observed_value_identity
                reconstruction_elapsed_ns = total_elapsed_ns
                reuse_elapsed_ns = None
            elif value_identity == observed_value_identity:
                reuse_elapsed_ns = total_elapsed_ns
            else:
                value_identity = observed_value_identity
                reconstruction_elapsed_ns = None
                reuse_elapsed_ns = total_elapsed_ns

        # Advance the tiny catalog first. A crash before ticket/record
        # publication leaves only harmless sequence gaps. A ticket published
        # before its record is stale until the record points at it.
        updated_catalog = {
            **catalog,
            "next_use_sequence": next_sequence,
            "next_value_ticket_sequence": new_ticket_sequence,
        }
        write_hot_state_value_catalog(namespace_root, updated_catalog)
        write_hot_state_value_ticket(
            namespace_root,
            new_ticket_sequence,
            state_base.name,
            next_sequence,
        )
        write_hot_state_value_record(
            namespace_root,
            state_base.name,
            {
                "manifest_device": manifest_identity.device,
                "manifest_inode": manifest_identity.inode,
                "manifest_creation_witness_ns": manifest_identity.creation_witness_ns,
                "last_successful_use_sequence": next_sequence,
                "successful_use_count": successful_use_count,
                "value_identity": value_identity,
                "reconstruction_elapsed_ns": reconstruction_elapsed_ns,
                "reuse_elapsed_ns": reuse_elapsed_ns,
                "value_ticket_sequence": new_ticket_sequence,
            },
        )
        if (
            old_ticket_sequence is not None
            and old_ticket_sequence != new_ticket_sequence
        ):
            try:
                remove_hot_state_value_ticket(
                    namespace_root, old_ticket_sequence
                )
            except (OSError, RuntimeError):
                pass
        return "recorded"
    finally:
        if value_lock is not None:
            os.close(value_lock)
        if owned_namespace_lease is not None:
            os.close(owned_namespace_lease)

def hot_state_filesystem_used_percent(namespace_root: Path) -> tuple[int, int]:
    details = os.statvfs(namespace_root)
    blocks = details.f_blocks
    available = details.f_bavail
    if blocks <= 0 or available < 0 or available > blocks:
        raise RuntimeError("hot-state filesystem capacity is unavailable")
    return blocks - available, blocks


def requeue_hot_state_value_ticket(
    namespace_root: Path,
    catalog: dict[str, object],
    record: dict[str, object],
    old_ticket_sequence: int,
) -> dict[str, object]:
    state_identity = record["state_identity"]
    last_use = record["last_successful_use_sequence"]
    assert isinstance(state_identity, str)
    assert isinstance(last_use, int)
    next_ticket = int(catalog["next_value_ticket_sequence"]) + 1
    updated_catalog = {
        **catalog,
        "next_value_ticket_sequence": next_ticket,
    }
    write_hot_state_value_catalog(namespace_root, updated_catalog)
    write_hot_state_value_ticket(
        namespace_root,
        next_ticket,
        state_identity,
        last_use,
    )
    fields = {
        key: value
        for key, value in record.items()
        if key not in {"schema_version", "producer", "state_identity"}
    }
    fields["value_ticket_sequence"] = next_ticket
    write_hot_state_value_record(
        namespace_root,
        state_identity,
        fields,
    )
    try:
        remove_hot_state_value_ticket(
            namespace_root, old_ticket_sequence
        )
    except (OSError, RuntimeError):
        pass
    return updated_catalog


def retire_one_low_value_state(
    namespace_root: Path, current_state_identity: str
) -> str:
    remove_stale_hot_state_value_catalog_stage(namespace_root)
    catalog = read_hot_state_value_catalog(namespace_root)
    used, total = hot_state_filesystem_used_percent(namespace_root)
    pressure_active = catalog["pressure_active"]
    assert isinstance(pressure_active, bool)
    if pressure_active:
        next_pressure_active = not (
            used * 100 <= total * HOT_STATE_RETIRE_STOP_USED_PERCENT
        )
    else:
        next_pressure_active = (
            used * 100 >= total * HOT_STATE_RETIRE_START_USED_PERCENT
        )
    if next_pressure_active != pressure_active:
        catalog = {**catalog, "pressure_active": next_pressure_active}
        write_hot_state_value_catalog(namespace_root, catalog)
    if not next_pressure_active:
        return (
            "pressure_relieved"
            if pressure_active
            else "ordinary_free_space"
        )

    cursor = int(catalog["value_cursor_ticket_sequence"])
    initial_next_ticket = int(catalog["next_value_ticket_sequence"])
    namespace_details = namespace_root.stat(follow_symlinks=False)
    processed = 0
    retired = False

    while (
        processed < HOT_STATE_VALUE_TICKETS_PER_PASS
        and cursor < initial_next_ticket
    ):
        ticket_sequence = cursor + 1
        cursor = ticket_sequence
        processed += 1
        try:
            ticket = read_hot_state_value_ticket(
                namespace_root, ticket_sequence
            )
        except (OSError, RuntimeError):
            ticket = None
        if ticket is None:
            continue

        state_identity = ticket["state_identity"]
        assert isinstance(state_identity, str)
        try:
            record = read_hot_state_value_record(
                namespace_root, state_identity
            )
        except (OSError, RuntimeError):
            record = None
        if (
            record is None
            or record["value_ticket_sequence"] != ticket_sequence
            or record["last_successful_use_sequence"]
            != ticket["last_successful_use_sequence"]
        ):
            try:
                remove_hot_state_value_ticket(
                    namespace_root, ticket_sequence
                )
            except (OSError, RuntimeError):
                pass
            continue

        if state_identity == current_state_identity:
            catalog = {
                **catalog,
                "value_cursor_ticket_sequence": cursor,
            }
            write_hot_state_value_catalog(namespace_root, catalog)
            catalog = requeue_hot_state_value_ticket(
                namespace_root,
                catalog,
                record,
                ticket_sequence,
            )
            continue

        state = namespace_root / state_identity
        state_descriptor: int | None = None
        renamed = False
        try:
            try:
                details = state.stat(follow_symlinks=False)
            except FileNotFoundError:
                remove_generation_value_metadata(
                    namespace_root, state_identity
                )
                continue
            if (
                not stat.S_ISDIR(details.st_mode)
                or stat.S_ISLNK(details.st_mode)
                or details.st_uid != os.getuid()
                or stat.S_IMODE(details.st_mode) != 0o700
                or details.st_dev != namespace_details.st_dev
            ):
                remove_generation_value_metadata(
                    namespace_root, state_identity
                )
                continue
            state_descriptor = os.open(
                state,
                os.O_RDONLY
                | os.O_DIRECTORY
                | os.O_CLOEXEC
                | os.O_NOFOLLOW,
            )
            pinned_state = os.fstat(state_descriptor)
            manifest, encoded_before, manifest_identity = (
                read_producer_manifest_with_identity(
                    state_descriptor, state_identity
                )
            )
            if not manifest_has_full_execution_namespace_lease(manifest):
                try:
                    remove_hot_state_value_ticket(
                        namespace_root, ticket_sequence
                    )
                except (OSError, RuntimeError):
                    pass
                continue
            if (
                manifest_generation_reachable(manifest) is not True
                or record["manifest_device"] != manifest_identity.device
                or record["manifest_inode"] != manifest_identity.inode
                or record["manifest_creation_witness_ns"]
                != manifest_identity.creation_witness_ns
            ):
                catalog = {
                    **catalog,
                    "value_cursor_ticket_sequence": cursor,
                }
                write_hot_state_value_catalog(namespace_root, catalog)
                catalog = requeue_hot_state_value_ticket(
                    namespace_root,
                    catalog,
                    record,
                    ticket_sequence,
                )
                continue

            manifest_after, encoded_after, identity_after = (
                read_producer_manifest_with_identity(
                    state_descriptor, state_identity
                )
            )
            named_state = state.stat(follow_symlinks=False)
            if (
                encoded_after != encoded_before
                or identity_after != manifest_identity
                or manifest_generation_reachable(manifest_after) is not True
                or not manifest_has_full_execution_namespace_lease(
                    manifest_after
                )
                or named_state.st_dev != pinned_state.st_dev
                or named_state.st_ino != pinned_state.st_ino
            ):
                catalog = {
                    **catalog,
                    "value_cursor_ticket_sequence": cursor,
                }
                write_hot_state_value_catalog(namespace_root, catalog)
                catalog = requeue_hot_state_value_ticket(
                    namespace_root,
                    catalog,
                    record,
                    ticket_sequence,
                )
                continue

            retired_name = f"{HOT_STATE_RETIRED_PREFIX}{state_identity}"
            enqueue_hot_state_reconcile_ticket(
                namespace_root,
                "retired",
                state_identity,
                retired_name,
            )
            try:
                rename_noreplace(state, namespace_root / retired_name)
            except (FileExistsError, OSError):
                catalog = {
                    **catalog,
                    "value_cursor_ticket_sequence": cursor,
                }
                write_hot_state_value_catalog(namespace_root, catalog)
                catalog = requeue_hot_state_value_ticket(
                    namespace_root,
                    catalog,
                    record,
                    ticket_sequence,
                )
                continue
            renamed = True
            fsync_directory(namespace_root)
            try:
                remove_generation_value_metadata(
                    namespace_root, state_identity
                )
            except (OSError, RuntimeError):
                return "retired_low_value_catalog_deferred"
            delete_retired_state_bounded(
                namespace_root, retired_name, state_identity
            )
            retired = True
            break
        except (OSError, RuntimeError):
            if renamed:
                return "retired_low_value_recovery_deferred"
            try:
                catalog = {
                    **catalog,
                    "value_cursor_ticket_sequence": cursor,
                }
                write_hot_state_value_catalog(namespace_root, catalog)
                catalog = requeue_hot_state_value_ticket(
                    namespace_root,
                    catalog,
                    record,
                    ticket_sequence,
                )
            except (OSError, RuntimeError):
                pass
        finally:
            if state_descriptor is not None:
                os.close(state_descriptor)

    catalog = {
        **catalog,
        "value_cursor_ticket_sequence": cursor,
    }
    write_hot_state_value_catalog(namespace_root, catalog)
    if retired:
        return "retired_low_value"
    if cursor < initial_next_ticket:
        return "pressure_scan_deferred"
    return "pressure_no_eligible_state"


def ensure_retirement_record(
    namespace_root: Path, retired_name: str, state_identity: str
) -> dict[str, object]:
    details = (namespace_root / retired_name).lstat()
    if (
        not stat.S_ISDIR(details.st_mode)
        or stat.S_ISLNK(details.st_mode)
        or details.st_uid != os.getuid()
        or stat.S_IMODE(details.st_mode) != 0o700
    ):
        raise RuntimeError("retired hot state is not an owned directory")
    expected = retirement_record_document(retired_name, state_identity, details)
    record = namespace_root / retirement_record_name(retired_name, state_identity)
    try:
        observed, observed_encoded = read_private_json(
            record, "hot-state retirement record"
        )
    except FileNotFoundError:
        write_private_json_noreplace(record, expected)
        return expected
    validate_retirement_record(observed)
    if observed_encoded != canonical_manifest_bytes(expected):
        raise RuntimeError("hot-state retirement record conflicts with its directory")
    return observed


def delete_retired_state_bounded(
    namespace_root: Path, retired_name: str, state_identity: str
) -> bool:
    root_descriptor = os.open(
        namespace_root,
        os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC | os.O_NOFOLLOW,
    )
    state_descriptor: int | None = None
    try:
        root_details = os.fstat(root_descriptor)
        record_name = retirement_record_name(retired_name, state_identity)
        try:
            state_descriptor = os.open(
                retired_name,
                os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC | os.O_NOFOLLOW,
                dir_fd=root_descriptor,
            )
        except FileNotFoundError:
            try:
                record_document, _ = read_private_json(
                    namespace_root / record_name,
                    "hot-state retirement record",
                )
                validate_retirement_record(record_document)
                if (
                    record_document["retired_name"] != retired_name
                    or record_document["state_identity"] != state_identity
                ):
                    return False
                os.unlink(record_name, dir_fd=root_descriptor)
                os.fsync(root_descriptor)
                return True
            except (OSError, RuntimeError):
                return False
        state_details = os.fstat(state_descriptor)
        if (
            state_details.st_uid != os.getuid()
            or state_details.st_dev != root_details.st_dev
            or stat.S_IMODE(state_details.st_mode) != 0o700
        ):
            return False
        record = ensure_retirement_record(
            namespace_root, retired_name, state_identity
        )
        if (
            record["directory_device"] != state_details.st_dev
            or record["directory_inode"] != state_details.st_ino
        ):
            return False
        try:
            read_producer_manifest(namespace_root / retired_name, state_identity)
        except FileNotFoundError:
            with os.scandir(state_descriptor) as remaining:
                if next(remaining, None) is not None:
                    return False
            os.close(state_descriptor)
            state_descriptor = None
            os.rmdir(retired_name, dir_fd=root_descriptor)
            os.fsync(root_descriptor)
            os.unlink(record_name, dir_fd=root_descriptor)
            os.fsync(root_descriptor)
            return True
        budget = DeleteBudget(MAX_HOT_STATE_DELETE_ENTRIES)
        complete = delete_directory_contents_bounded(
            state_descriptor,
            state_details.st_dev,
            budget,
            preserve_manifest=True,
        )
        if not complete or budget.remaining_entries <= 0:
            return False
        os.unlink(HOT_STATE_MANIFEST, dir_fd=state_descriptor)
        os.close(state_descriptor)
        state_descriptor = None
        try:
            observed = os.stat(
                retired_name, dir_fd=root_descriptor, follow_symlinks=False
            )
        except FileNotFoundError:
            try:
                os.unlink(record_name, dir_fd=root_descriptor)
            except FileNotFoundError:
                pass
            os.fsync(root_descriptor)
            return True
        if (
            observed.st_dev != state_details.st_dev
            or observed.st_ino != state_details.st_ino
            or not stat.S_ISDIR(observed.st_mode)
        ):
            return False
        os.rmdir(retired_name, dir_fd=root_descriptor)
        os.fsync(root_descriptor)
        os.unlink(record_name, dir_fd=root_descriptor)
        os.fsync(root_descriptor)
        return True
    except OSError:
        return False
    finally:
        if state_descriptor is not None:
            os.close(state_descriptor)
        os.close(root_descriptor)


def delete_unpublished_stage_bounded(
    namespace_root: Path, stage_name: str, state_identity: str
) -> bool:
    expected_prefix = f"{HOT_STATE_CREATING_PREFIX}{state_identity}-"
    if not stage_name.startswith(expected_prefix):
        return False
    root_descriptor = os.open(
        namespace_root,
        os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC | os.O_NOFOLLOW,
    )
    stage_descriptor: int | None = None
    try:
        root_details = os.fstat(root_descriptor)
        stage_descriptor = os.open(
            stage_name,
            os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC | os.O_NOFOLLOW,
            dir_fd=root_descriptor,
        )
        stage_details = os.fstat(stage_descriptor)
        if (
            stage_details.st_uid != os.getuid()
            or stage_details.st_dev != root_details.st_dev
            or stat.S_IMODE(stage_details.st_mode) != 0o700
        ):
            return False
        entries = scan_directory_bounded(
            stage_descriptor, MAX_HOT_STATE_CREATING_ENTRIES
        )
        if entries is None:
            return False
        for entry in entries:
            if not (
                entry.name == HOT_STATE_MANIFEST
                or entry.name.startswith(HOT_STATE_MANIFEST_STAGING_PREFIX)
            ):
                return False
            details = os.stat(
                entry.name, dir_fd=stage_descriptor, follow_symlinks=False
            )
            if (
                not stat.S_ISREG(details.st_mode)
                or details.st_uid != os.getuid()
                or details.st_dev != stage_details.st_dev
                or details.st_nlink != 1
                or stat.S_IMODE(details.st_mode) != 0o600
                or details.st_size > MAX_HOT_STATE_MANIFEST_BYTES
            ):
                return False
        for entry in entries:
            os.unlink(entry.name, dir_fd=stage_descriptor)
        os.fsync(stage_descriptor)
        os.close(stage_descriptor)
        stage_descriptor = None
        os.rmdir(stage_name, dir_fd=root_descriptor)
        os.fsync(root_descriptor)
        return True
    except OSError:
        return False
    finally:
        if stage_descriptor is not None:
            os.close(stage_descriptor)
        os.close(root_descriptor)


def empty_hot_state_reconcile_catalog() -> dict[str, object]:
    return {
        "schema_version": HOT_STATE_RECONCILE_SCHEMA_VERSION,
        "producer": HOT_STATE_RECONCILE_PRODUCER,
        "next_ticket_sequence": 0,
        "cursor_ticket_sequence": 0,
    }


def validate_hot_state_reconcile_catalog(
    document: object,
) -> dict[str, object]:
    if not isinstance(document, dict) or set(document) != {
        "schema_version",
        "producer",
        "next_ticket_sequence",
        "cursor_ticket_sequence",
    }:
        raise RuntimeError("hot-state reconcile catalog has an unsupported shape")
    next_ticket = document["next_ticket_sequence"]
    cursor = document["cursor_ticket_sequence"]
    if (
        document["schema_version"] != HOT_STATE_RECONCILE_SCHEMA_VERSION
        or document["producer"] != HOT_STATE_RECONCILE_PRODUCER
        or isinstance(next_ticket, bool)
        or not isinstance(next_ticket, int)
        or next_ticket < 0
        or isinstance(cursor, bool)
        or not isinstance(cursor, int)
        or cursor < 0
        or cursor > next_ticket
    ):
        raise RuntimeError("hot-state reconcile catalog identity is invalid")
    return document


def canonical_hot_state_reconcile_catalog_bytes(
    document: dict[str, object],
) -> bytes:
    validate_hot_state_reconcile_catalog(document)
    encoded = (
        json.dumps(document, sort_keys=True, separators=(",", ":")) + "\n"
    ).encode("utf-8")
    if len(encoded) > MAX_HOT_STATE_RECONCILE_CATALOG_BYTES:
        raise RuntimeError("hot-state reconcile catalog exceeds its byte budget")
    return encoded


def read_hot_state_reconcile_catalog(
    namespace_root: Path,
) -> dict[str, object]:
    path = namespace_root / HOT_STATE_RECONCILE_CATALOG
    try:
        document, encoded = read_private_json(
            path,
            "hot-state reconcile catalog",
            MAX_HOT_STATE_RECONCILE_CATALOG_BYTES,
        )
    except FileNotFoundError:
        return empty_hot_state_reconcile_catalog()
    catalog = validate_hot_state_reconcile_catalog(document)
    if canonical_hot_state_reconcile_catalog_bytes(catalog) != encoded:
        raise RuntimeError("hot-state reconcile catalog is not canonical")
    return catalog


def write_hot_state_reconcile_catalog(
    namespace_root: Path, document: dict[str, object]
) -> None:
    encoded = canonical_hot_state_reconcile_catalog_bytes(document)
    staging = namespace_root / HOT_STATE_RECONCILE_CATALOG_STAGING
    try:
        details = staging.stat(follow_symlinks=False)
    except FileNotFoundError:
        pass
    else:
        if (
            not stat.S_ISREG(details.st_mode)
            or details.st_uid != os.getuid()
            or details.st_nlink != 1
            or stat.S_IMODE(details.st_mode) != 0o600
            or details.st_size > MAX_HOT_STATE_RECONCILE_CATALOG_BYTES
        ):
            raise RuntimeError("hot-state reconcile catalog stage is not recoverable")
        staging.unlink()
        fsync_directory(namespace_root)
    descriptor = os.open(
        staging,
        os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_CLOEXEC | os.O_NOFOLLOW,
        0o600,
    )
    try:
        written = 0
        while written < len(encoded):
            count = os.write(descriptor, encoded[written:])
            if count <= 0:
                raise OSError("hot-state reconcile catalog write made no progress")
            written += count
        os.fsync(descriptor)
    finally:
        os.close(descriptor)
    os.replace(staging, namespace_root / HOT_STATE_RECONCILE_CATALOG)
    fsync_directory(namespace_root)


def validate_hot_state_reconcile_tickets_root(
    namespace_root: Path, *, create: bool
) -> Path | None:
    root = namespace_root / HOT_STATE_RECONCILE_TICKETS
    if create:
        try:
            root.mkdir(mode=0o700)
        except FileExistsError:
            pass
    try:
        details = root.stat(follow_symlinks=False)
    except FileNotFoundError:
        if create:
            raise
        return None
    namespace_details = namespace_root.stat(follow_symlinks=False)
    if (
        not stat.S_ISDIR(details.st_mode)
        or stat.S_ISLNK(details.st_mode)
        or details.st_uid != os.getuid()
        or stat.S_IMODE(details.st_mode) != 0o700
        or details.st_dev != namespace_details.st_dev
    ):
        raise RuntimeError("hot-state reconcile-ticket root is not owner-private")
    return root


def reconcile_ticket_name(ticket_sequence: int) -> str:
    if ticket_sequence <= 0:
        raise RuntimeError("hot-state reconcile ticket sequence is invalid")
    return f"{ticket_sequence:020d}{HOT_STATE_RECONCILE_TICKET_SUFFIX}"


def reconcile_ticket_document(
    ticket_sequence: int,
    kind: str,
    state_identity: str,
    name: str,
) -> dict[str, object]:
    if (
        ticket_sequence <= 0
        or kind not in {"state", "creating", "retired"}
        or not state_identity_name(state_identity)
        or not isinstance(name, str)
        or "/" in name
        or name in {"", ".", ".."}
    ):
        raise RuntimeError("hot-state reconcile ticket is invalid")
    if kind == "state" and name != state_identity:
        raise RuntimeError("hot-state state ticket name is invalid")
    if kind == "creating" and not name.startswith(
        f"{HOT_STATE_CREATING_PREFIX}{state_identity}-"
    ):
        raise RuntimeError("hot-state creating ticket name is invalid")
    if kind == "retired" and name != f"{HOT_STATE_RETIRED_PREFIX}{state_identity}":
        raise RuntimeError("hot-state retired ticket name is invalid")
    return {
        "schema_version": HOT_STATE_RECONCILE_TICKET_SCHEMA_VERSION,
        "producer": HOT_STATE_RECONCILE_TICKET_PRODUCER,
        "ticket_sequence": ticket_sequence,
        "kind": kind,
        "state_identity": state_identity,
        "name": name,
    }


def validate_hot_state_reconcile_ticket(
    document: object,
) -> dict[str, object]:
    if not isinstance(document, dict) or set(document) != {
        "schema_version",
        "producer",
        "ticket_sequence",
        "kind",
        "state_identity",
        "name",
    }:
        raise RuntimeError("hot-state reconcile ticket has an unsupported shape")
    if (
        document["schema_version"] != HOT_STATE_RECONCILE_TICKET_SCHEMA_VERSION
        or document["producer"] != HOT_STATE_RECONCILE_TICKET_PRODUCER
        or isinstance(document["ticket_sequence"], bool)
        or not isinstance(document["ticket_sequence"], int)
    ):
        raise RuntimeError("hot-state reconcile ticket identity is invalid")
    expected = reconcile_ticket_document(
        document["ticket_sequence"],
        document["kind"],
        document["state_identity"],
        document["name"],
    )
    if document != expected:
        raise RuntimeError("hot-state reconcile ticket is not canonical")
    return document


def canonical_hot_state_reconcile_ticket_bytes(
    document: dict[str, object],
) -> bytes:
    validate_hot_state_reconcile_ticket(document)
    encoded = (
        json.dumps(document, sort_keys=True, separators=(",", ":")) + "\n"
    ).encode("utf-8")
    if len(encoded) > MAX_HOT_STATE_RECONCILE_TICKET_BYTES:
        raise RuntimeError("hot-state reconcile ticket exceeds its byte budget")
    return encoded


def read_hot_state_reconcile_ticket(
    namespace_root: Path, ticket_sequence: int
) -> dict[str, object] | None:
    root = validate_hot_state_reconcile_tickets_root(
        namespace_root, create=False
    )
    if root is None:
        return None
    path = root / reconcile_ticket_name(ticket_sequence)
    try:
        document, encoded = read_private_json(
            path,
            "hot-state reconcile ticket",
            MAX_HOT_STATE_RECONCILE_TICKET_BYTES,
        )
    except FileNotFoundError:
        return None
    ticket = validate_hot_state_reconcile_ticket(document)
    if ticket["ticket_sequence"] != ticket_sequence:
        raise RuntimeError("hot-state reconcile ticket filename conflicts with identity")
    if canonical_hot_state_reconcile_ticket_bytes(ticket) != encoded:
        raise RuntimeError("hot-state reconcile ticket is not canonical")
    return ticket


def write_hot_state_reconcile_ticket(
    namespace_root: Path,
    ticket_sequence: int,
    kind: str,
    state_identity: str,
    name: str,
) -> None:
    root = validate_hot_state_reconcile_tickets_root(
        namespace_root, create=True
    )
    assert root is not None
    document = reconcile_ticket_document(
        ticket_sequence, kind, state_identity, name
    )
    encoded = canonical_hot_state_reconcile_ticket_bytes(document)
    path = root / reconcile_ticket_name(ticket_sequence)
    descriptor = os.open(
        path,
        os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_CLOEXEC | os.O_NOFOLLOW,
        0o600,
    )
    try:
        written = 0
        while written < len(encoded):
            count = os.write(descriptor, encoded[written:])
            if count <= 0:
                raise OSError("hot-state reconcile ticket write made no progress")
            written += count
        os.fsync(descriptor)
    finally:
        os.close(descriptor)
    fsync_directory(root)


def remove_hot_state_reconcile_ticket(
    namespace_root: Path, ticket_sequence: int
) -> bool:
    root = validate_hot_state_reconcile_tickets_root(
        namespace_root, create=False
    )
    if root is None:
        return False
    path = root / reconcile_ticket_name(ticket_sequence)
    try:
        path.unlink()
    except FileNotFoundError:
        return False
    fsync_directory(root)
    return True


def enqueue_hot_state_reconcile_ticket(
    namespace_root: Path,
    kind: str,
    state_identity: str,
    name: str,
) -> int:
    catalog = read_hot_state_reconcile_catalog(namespace_root)
    ticket_sequence = int(catalog["next_ticket_sequence"]) + 1
    updated = {
        **catalog,
        "next_ticket_sequence": ticket_sequence,
    }
    write_hot_state_reconcile_catalog(namespace_root, updated)
    write_hot_state_reconcile_ticket(
        namespace_root,
        ticket_sequence,
        kind,
        state_identity,
        name,
    )
    return ticket_sequence


def requeue_hot_state_reconcile_ticket(
    namespace_root: Path,
    ticket: dict[str, object],
) -> None:
    enqueue_hot_state_reconcile_ticket(
        namespace_root,
        str(ticket["kind"]),
        str(ticket["state_identity"]),
        str(ticket["name"]),
    )
    try:
        remove_hot_state_reconcile_ticket(
            namespace_root, int(ticket["ticket_sequence"])
        )
    except (OSError, RuntimeError):
        pass


def remove_generation_value_metadata(
    namespace_root: Path, state_identity: str
) -> None:
    try:
        record = read_hot_state_value_record(
            namespace_root, state_identity
        )
    except (OSError, RuntimeError):
        record = None
    if record is not None:
        try:
            remove_hot_state_value_ticket(
                namespace_root,
                int(record["value_ticket_sequence"]),
            )
        except (OSError, RuntimeError):
            pass
    try:
        remove_hot_state_value_record(namespace_root, state_identity)
    except (OSError, RuntimeError):
        pass


def collect_one_unreachable_state(
    namespace_root: Path, current_state_identity: str
) -> str:
    try:
        namespace_details = namespace_root.stat(follow_symlinks=False)
        if (
            not stat.S_ISDIR(namespace_details.st_mode)
            or namespace_details.st_uid != os.getuid()
            or stat.S_IMODE(namespace_details.st_mode) != 0o700
        ):
            return "unavailable"
        catalog = read_hot_state_reconcile_catalog(namespace_root)
    except (OSError, RuntimeError):
        return "unavailable"

    cursor = int(catalog["cursor_ticket_sequence"])
    initial_next_ticket = int(catalog["next_ticket_sequence"])
    processed = 0
    outcome = "nothing_eligible"

    while (
        processed < HOT_STATE_RECONCILE_TICKETS_PER_PASS
        and cursor < initial_next_ticket
    ):
        ticket_sequence = cursor + 1
        cursor = ticket_sequence
        processed += 1
        try:
            ticket = read_hot_state_reconcile_ticket(
                namespace_root, ticket_sequence
            )
        except (OSError, RuntimeError):
            ticket = None
        if ticket is None:
            continue

        kind = ticket["kind"]
        state_identity = ticket["state_identity"]
        name = ticket["name"]
        assert isinstance(kind, str)
        assert isinstance(state_identity, str)
        assert isinstance(name, str)

        if kind == "creating":
            stage = namespace_root / name
            if not stage.exists():
                try:
                    remove_hot_state_reconcile_ticket(
                        namespace_root, ticket_sequence
                    )
                except (OSError, RuntimeError):
                    pass
                continue
            if delete_unpublished_stage_bounded(
                namespace_root, name, state_identity
            ):
                try:
                    remove_hot_state_reconcile_ticket(
                        namespace_root, ticket_sequence
                    )
                except (OSError, RuntimeError):
                    pass
                outcome = "creating_recovery"
                break
            requeue_hot_state_reconcile_ticket(namespace_root, ticket)
            outcome = "creating_recovery_deferred"
            break

        if kind == "retired":
            retired_path = namespace_root / name
            retirement_record = namespace_root / retirement_record_name(
                name, state_identity
            )
            try:
                retired_exists = retired_path.lstat() is not None
            except FileNotFoundError:
                retired_exists = False
            try:
                record_exists = retirement_record.lstat() is not None
            except FileNotFoundError:
                record_exists = False
            if not retired_exists and not record_exists:
                try:
                    remove_hot_state_reconcile_ticket(
                        namespace_root, ticket_sequence
                    )
                except (OSError, RuntimeError):
                    pass
                continue
            complete = delete_retired_state_bounded(
                namespace_root, name, state_identity
            )
            if complete:
                remove_generation_value_metadata(
                    namespace_root, state_identity
                )
                try:
                    remove_hot_state_reconcile_ticket(
                        namespace_root, ticket_sequence
                    )
                except (OSError, RuntimeError):
                    pass
                outcome = "retired_recovery"
            else:
                requeue_hot_state_reconcile_ticket(
                    namespace_root, ticket
                )
                outcome = "retired_recovery_deferred"
            break

        if state_identity == current_state_identity:
            requeue_hot_state_reconcile_ticket(namespace_root, ticket)
            continue

        state = namespace_root / state_identity
        state_descriptor: int | None = None
        try:
            details = state.stat(follow_symlinks=False)
        except FileNotFoundError:
            remove_generation_value_metadata(
                namespace_root, state_identity
            )
            try:
                remove_hot_state_reconcile_ticket(
                    namespace_root, ticket_sequence
                )
            except (OSError, RuntimeError):
                pass
            continue
        except OSError:
            requeue_hot_state_reconcile_ticket(namespace_root, ticket)
            continue

        try:
            if (
                not stat.S_ISDIR(details.st_mode)
                or stat.S_ISLNK(details.st_mode)
                or details.st_uid != os.getuid()
                or stat.S_IMODE(details.st_mode) != 0o700
                or details.st_dev != namespace_details.st_dev
            ):
                remove_hot_state_reconcile_ticket(
                    namespace_root, ticket_sequence
                )
                continue
            state_descriptor = os.open(
                state,
                os.O_RDONLY
                | os.O_DIRECTORY
                | os.O_CLOEXEC
                | os.O_NOFOLLOW,
            )
            pinned_state = os.fstat(state_descriptor)
            if (
                pinned_state.st_dev != details.st_dev
                or pinned_state.st_ino != details.st_ino
                or pinned_state.st_uid != os.getuid()
                or stat.S_IMODE(pinned_state.st_mode) != 0o700
            ):
                requeue_hot_state_reconcile_ticket(
                    namespace_root, ticket
                )
                continue
            manifest, encoded_before = read_producer_manifest(
                state_descriptor, state_identity
            )
            if not manifest_has_full_execution_namespace_lease(manifest):
                # Legacy state remains usable but never receives deletion
                # authority from the new namespace-lease protocol.
                remove_hot_state_reconcile_ticket(
                    namespace_root, ticket_sequence
                )
                continue
            reachable = manifest_generation_reachable(manifest)
            if reachable is not False:
                requeue_hot_state_reconcile_ticket(
                    namespace_root, ticket
                )
                continue

            try:
                manifest_after, encoded_after = read_producer_manifest(
                    state_descriptor, state_identity
                )
                named_state = state.stat(follow_symlinks=False)
            except (OSError, RuntimeError):
                requeue_hot_state_reconcile_ticket(
                    namespace_root, ticket
                )
                continue
            if (
                encoded_after != encoded_before
                or not manifest_has_full_execution_namespace_lease(
                    manifest_after
                )
                or manifest_generation_reachable(manifest_after) is not False
                or named_state.st_dev != pinned_state.st_dev
                or named_state.st_ino != pinned_state.st_ino
            ):
                requeue_hot_state_reconcile_ticket(
                    namespace_root, ticket
                )
                continue

            retired_name = f"{HOT_STATE_RETIRED_PREFIX}{state_identity}"
            enqueue_hot_state_reconcile_ticket(
                namespace_root,
                "retired",
                state_identity,
                retired_name,
            )
            try:
                rename_noreplace(
                    state, namespace_root / retired_name
                )
            except (FileExistsError, OSError):
                requeue_hot_state_reconcile_ticket(
                    namespace_root, ticket
                )
                continue
            fsync_directory(namespace_root)
            try:
                remove_hot_state_reconcile_ticket(
                    namespace_root, ticket_sequence
                )
            except (OSError, RuntimeError):
                pass
            remove_generation_value_metadata(
                namespace_root, state_identity
            )
            delete_retired_state_bounded(
                namespace_root, retired_name, state_identity
            )
            outcome = "retired_unreachable"
            break
        except (OSError, RuntimeError):
            try:
                requeue_hot_state_reconcile_ticket(
                    namespace_root, ticket
                )
            except (OSError, RuntimeError):
                pass
        finally:
            if state_descriptor is not None:
                os.close(state_descriptor)

    try:
        latest = read_hot_state_reconcile_catalog(namespace_root)
        write_hot_state_reconcile_catalog(
            namespace_root,
            {**latest, "cursor_ticket_sequence": cursor},
        )
    except (OSError, RuntimeError):
        return "unavailable"

    if outcome != "nothing_eligible":
        return outcome
    if cursor < initial_next_ticket:
        return "reconcile_scan_deferred"
    return "nothing_eligible"


def prepare_private_copy(
    spec: CacheSpec, resident_cache: Path, destination: Path
) -> CachePreparation:
    try:
        destination.lstat()
    except FileNotFoundError:
        pass
    else:
        ensure_private_directory(destination)
        return CachePreparation(spec.path, spec.mode, "reused", 0.0)

    copy_program = Path("/usr/bin/cp")
    if not copy_program.is_file() or not os.access(copy_program, os.X_OK):
        raise RuntimeError("required capability GNU cp is unavailable")
    started = time.monotonic()
    candidate = Path(
        tempfile.mkdtemp(prefix=f".{destination.name}.", dir=destination.parent)
    )
    try:
        result = subprocess.run(
            [
                os.fspath(copy_program),
                "-a",
                "--reflink=auto",
                "--",
                f"{os.fspath(resident_cache)}/.",
                os.fspath(candidate),
            ],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            check=False,
        )
        if result.returncode != 0:
            raise RuntimeError("private-copy preparation failed")
        ensure_private_directory(candidate)
        os.replace(candidate, destination)
    finally:
        if candidate.exists():
            shutil.rmtree(candidate)
    return CachePreparation(
        spec.path, spec.mode, "seeded", time.monotonic() - started
    )


def tracked_paths(root: Path) -> tuple[Path, ...]:
    git = shutil.which("git")
    if git is None:
        raise RuntimeError("required capability git is unavailable")
    result = subprocess.run(
        [os.path.abspath(git), "ls-files", "-z", "--cached", "--"],
        cwd=root,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        timeout=30,
        check=False,
    )
    if result.returncode != 0:
        raise RuntimeError("tracked source inventory is unavailable")
    if len(result.stdout) > MAX_TRACKED_PATH_BYTES:
        raise RuntimeError("tracked source inventory exceeds the bounded input size")
    if result.stdout and not result.stdout.endswith(b"\0"):
        raise RuntimeError("tracked source inventory is malformed")
    paths: list[Path] = []
    for encoded in result.stdout.split(b"\0"):
        if not encoded:
            continue
        path = Path(os.fsdecode(encoded))
        if path.is_absolute() or path in (Path("."), Path("..")) or ".." in path.parts:
            raise RuntimeError("tracked source inventory contains an invalid path")
        paths.append(path)
    return tuple(paths)


def open_beneath(root: Path, path: Path) -> int:
    root_fd = os.open(
        root,
        os.O_PATH | os.O_DIRECTORY | os.O_CLOEXEC | os.O_NOFOLLOW,
    )
    parent_fd = root_fd
    try:
        for component in path.parts[:-1]:
            next_fd = os.open(
                component,
                os.O_PATH | os.O_DIRECTORY | os.O_CLOEXEC | os.O_NOFOLLOW,
                dir_fd=parent_fd,
            )
            if parent_fd != root_fd:
                os.close(parent_fd)
            parent_fd = next_fd
        return os.open(
            path.parts[-1],
            os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW,
            dir_fd=parent_fd,
        )
    finally:
        if parent_fd != root_fd:
            os.close(parent_fd)
        os.close(root_fd)


def normalize_matching_regular_file_mtime(
    resident_root: Path, task_root: Path, path: Path
) -> str:
    resident_fd: int | None = None
    task_fd: int | None = None
    try:
        try:
            resident_fd = open_beneath(resident_root, path)
            task_fd = open_beneath(task_root, path)
        except OSError:
            return "skipped"
        resident_before = os.fstat(resident_fd)
        task_before = os.fstat(task_fd)
        if not stat.S_ISREG(resident_before.st_mode) or not stat.S_ISREG(task_before.st_mode):
            return "skipped"
        if task_before.st_nlink != 1:
            return "skipped"
        if (
            resident_before.st_size != task_before.st_size
            or bool(resident_before.st_mode & 0o111) != bool(task_before.st_mode & 0o111)
        ):
            return "different"
        while True:
            resident_chunk = os.read(resident_fd, FILE_COMPARE_CHUNK_BYTES)
            task_chunk = os.read(task_fd, FILE_COMPARE_CHUNK_BYTES)
            if resident_chunk != task_chunk:
                return "different"
            if not resident_chunk:
                break
        resident_after = os.fstat(resident_fd)
        task_after = os.fstat(task_fd)
        for before, after in (
            (resident_before, resident_after),
            (task_before, task_after),
        ):
            if (
                before.st_dev,
                before.st_ino,
                before.st_size,
                before.st_mtime_ns,
                before.st_ctime_ns,
            ) != (
                after.st_dev,
                after.st_ino,
                after.st_size,
                after.st_mtime_ns,
                after.st_ctime_ns,
            ):
                return "skipped"
        os.utime(
            task_fd,
            ns=(task_after.st_atime_ns, resident_after.st_mtime_ns),
        )
        return "normalized"
    finally:
        if task_fd is not None:
            os.close(task_fd)
        if resident_fd is not None:
            os.close(resident_fd)


def prepare_seed_source_metadata(
    resident: Path,
    task: Path,
    cache_preparations: tuple[CachePreparation, ...],
) -> SourcePreparation | None:
    target_preparation = next(
        (
            item
            for item in cache_preparations
            if item.path == Path("target") and item.mode == "private-copy"
        ),
        None,
    )
    if target_preparation is None:
        return None
    mode = "resident_mtime_for_identical_tracked_regular_files_v1"
    if target_preparation.disposition == "reused":
        return SourcePreparation(mode, "retained_state_unchanged", 0, 0, 0, 0, 0.0)

    started = time.monotonic()
    paths = tracked_paths(task)
    normalized = 0
    different = 0
    skipped = 0
    for path in paths:
        result = normalize_matching_regular_file_mtime(resident, task, path)
        if result == "normalized":
            normalized += 1
        elif result == "different":
            different += 1
        else:
            skipped += 1
    return SourcePreparation(
        mode,
        "normalized_on_seed",
        len(paths),
        normalized,
        different,
        skipped,
        time.monotonic() - started,
    )


def resolve_program(
    command: list[str], task: Path, search_path: str | None = None
) -> list[str]:
    requested = command[0]
    if os.path.sep in requested:
        candidate = Path(requested)
        if not candidate.is_absolute():
            candidate = task / candidate
        program = Path(os.path.abspath(candidate))
    else:
        found = shutil.which(requested, path=search_path)
        if found is None:
            raise RuntimeError(f"command is unavailable: {requested}")
        # Preserve dispatch symlinks such as rustup's `cargo` and multicall utilities.
        program = Path(os.path.abspath(found))
    if not program.is_file() or not os.access(program, os.X_OK):
        raise RuntimeError(f"command is not executable: {requested}")
    return [os.fspath(program), *command[1:]]


def runtime_bin_identity(path: Path, details: os.stat_result) -> str:
    digest = hashlib.sha256(b"glaeda-hot-run-runtime-bin-v1")
    digest.update(b"\0" + os.fsencode(path))
    for value in (
        details.st_dev,
        details.st_ino,
        details.st_uid,
        details.st_gid,
        stat.S_IMODE(details.st_mode),
        details.st_nlink,
        details.st_size,
        details.st_mtime_ns,
        details.st_ctime_ns,
    ):
        digest.update(b"\0" + str(value).encode("ascii"))
    return f"sha256:{digest.hexdigest()}"


def observe_runtime_bin(
    runtime_bin: Path | None, runtime_id: str | None
) -> RuntimeBinBinding | None:
    if runtime_bin is None:
        return None
    if runtime_id is None:
        raise RuntimeError("runtime bin binding requires a runtime ID")
    if not runtime_bin.is_absolute():
        raise RuntimeError("runtime bin binding must be an absolute canonical path")
    path = Path(os.path.abspath(runtime_bin))
    if path != runtime_bin:
        raise RuntimeError("runtime bin binding must be an absolute canonical path")
    try:
        details = path.lstat()
        resolved = path.resolve(strict=True)
    except OSError as error:
        raise RuntimeError("runtime bin binding is unavailable") from error
    if stat.S_ISLNK(details.st_mode) or not stat.S_ISDIR(details.st_mode):
        raise RuntimeError("runtime bin binding is not a plain directory")
    if resolved != path:
        raise RuntimeError("runtime bin binding contains a symbolic-link component")
    return RuntimeBinBinding(path, runtime_bin_identity(path, details))


def revalidate_runtime_bin(binding: RuntimeBinBinding) -> None:
    current = observe_runtime_bin(binding.path, "revalidate")
    assert current is not None
    if current.identity_sha256 != binding.identity_sha256:
        raise RuntimeError("runtime bin binding changed during preflight")


def runtime_environment(binding: RuntimeBinBinding | None) -> dict[str, str] | None:
    if binding is None:
        return None
    environment = os.environ.copy()
    inherited = environment.get("PATH")
    environment["PATH"] = os.fspath(binding.path) + (
        os.pathsep + inherited if inherited else ""
    )
    return environment


def parse_runtime_contract(
    runtime_id: str | None, program_sha256: str | None
) -> RuntimeDeclaration | None:
    if runtime_id is None and program_sha256 is not None:
        raise RuntimeError("runtime executable digest requires a runtime ID")
    if runtime_id is None:
        return None
    valid_id = (
        0 < len(runtime_id) <= 96
        and runtime_id.isascii()
        and runtime_id[0].isalnum()
        and all(character.isalnum() or character in "._-" for character in runtime_id)
        and ".." not in runtime_id
    )
    if not valid_id:
        raise RuntimeError("runtime ID must be bounded safe ASCII")
    if program_sha256 is not None:
        digest_value = program_sha256.removeprefix("sha256:")
        valid_digest = (
            program_sha256.startswith("sha256:")
            and len(digest_value) == 64
            and all(character in "0123456789abcdef" for character in digest_value)
        )
        if not valid_digest:
            raise RuntimeError("runtime executable digest must be canonical SHA-256")
    return RuntimeDeclaration(runtime_id, program_sha256)


def canonical_comparison_key(value: str) -> str:
    digest = value.removeprefix(SHA256_PREFIX)
    if (
        not value.startswith(SHA256_PREFIX)
        or len(digest) != 64
        or any(character not in "0123456789abcdef" for character in digest)
    ):
        raise argparse.ArgumentTypeError(
            "comparison key must be canonical SHA-256"
        )
    return value


def verify_runtime_contract(
    declaration: RuntimeDeclaration | None,
    program: Path,
    runtime_bin: RuntimeBinBinding | None,
) -> RuntimeContract | None:
    if declaration is None:
        return None
    if runtime_bin is not None:
        if program.parent != runtime_bin.path:
            raise RuntimeError("runtime executable is outside the bound runtime bin")
        revalidate_runtime_bin(runtime_bin)
    digest = hashlib.sha256()
    with program.open("rb") as executable:
        while chunk := executable.read(1024 * 1024):
            digest.update(chunk)
    observed = f"sha256:{digest.hexdigest()}"
    if (
        declaration.expected_program_sha256 is not None
        and observed != declaration.expected_program_sha256
    ):
        raise RuntimeError("runtime executable content does not match declared digest")
    if runtime_bin is not None:
        revalidate_runtime_bin(runtime_bin)
    return RuntimeContract(
        declaration.runtime_id,
        observed,
        runtime_bin.identity_sha256 if runtime_bin is not None else None,
    )


def positive_seconds(value: str) -> float:
    try:
        seconds = float(value)
    except ValueError as error:
        raise argparse.ArgumentTypeError("must be a positive number") from error
    if not math.isfinite(seconds) or seconds <= 0:
        raise argparse.ArgumentTypeError("must be a positive finite number")
    return seconds


def rust_tool_environment(
    task: Path, command: list[str], environment: dict[str, str]
) -> None:
    rust_front_doors = {"cargo", "cargo-clippy", "clippy-driver", "rustc", "rustdoc"}
    if (
        Path(command[0]).name not in rust_front_doors
        or "RUSTC" in environment
        or not (task / "rust-toolchain.toml").is_file()
    ):
        return
    rustup = shutil.which("rustup")
    if rustup is None:
        return
    for variable, tool in (("RUSTC", "rustc"), ("RUSTDOC", "rustdoc")):
        result = subprocess.run(
            [os.path.abspath(rustup), "which", tool],
            cwd=task,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            timeout=30,
            check=False,
        )
        if result.returncode == 0:
            environment[variable] = os.path.abspath(result.stdout.strip())


def bind_cross_worktree_cache_environment(
    resident: Path,
    cache_specs: tuple[CacheSpec, ...],
    environment: dict[str, str],
) -> None:
    """Keep nested Cargo executions inside the selected stable target view."""

    if any(spec.path == Path("target") for spec in cache_specs):
        environment["CARGO_TARGET_DIR"] = os.fspath(resident / "target")


def write_measurement(
    destination: Path,
    elapsed_seconds: float,
    user_cpu_seconds: float | None,
    system_cpu_seconds: float | None,
    max_rss_kib: int | None,
    resource_accounting: str,
    exit_code: int,
    terminating_signal: int | None,
    completion_reason: str,
    timeout_seconds: float | None,
    cache_specs: tuple[CacheSpec, ...],
    cache_preparations: tuple[CachePreparation, ...],
    source_preparation: SourcePreparation | None,
    runtime_contract: RuntimeContract | None,
    cross_worktree: bool,
    resource_profile: str | None,
    comparison_key: str | None,
    machine_before: dict[str, object],
    machine_after: dict[str, object],
) -> None:
    destination = Path(os.path.abspath(destination))
    destination.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    preparation_elapsed = sum(item.elapsed_seconds for item in cache_preparations) + (
        source_preparation.elapsed_seconds if source_preparation is not None else 0.0
    )
    report = {
        "schema_version": 6,
        "document_type": "glaeda-hot-run-measurement",
        "authority": "developer_observation_only",
        "comparison_key": comparison_key,
        "cross_worktree": cross_worktree,
        "resource_profile": resource_profile,
        "machine_observation": {
            "scope": "host_aggregate",
            "before": machine_before,
            "after": machine_after,
            "interval": pressure_observation_interval(
                machine_before, machine_after, elapsed_seconds
            ),
        },
        "timeout_seconds": timeout_seconds,
        "cache_views": [
            {"path": os.fspath(spec.path), "mode": spec.mode} for spec in cache_specs
        ],
        "state_preparation": [
            {
                "path": os.fspath(item.path),
                "mode": item.mode,
                "disposition": item.disposition,
                "elapsed_seconds": round(item.elapsed_seconds, 6),
            }
            for item in cache_preparations
        ],
        "source_preparation": (
            {
                "mode": source_preparation.mode,
                "disposition": source_preparation.disposition,
                "tracked_path_count": source_preparation.tracked_path_count,
                "normalized_regular_file_count": (
                    source_preparation.normalized_regular_file_count
                ),
                "differing_regular_file_count": (
                    source_preparation.differing_regular_file_count
                ),
                "skipped_path_count": source_preparation.skipped_path_count,
                "elapsed_seconds": round(source_preparation.elapsed_seconds, 6),
            }
            if source_preparation is not None
            else None
        ),
        "runtime": (
            {
                "id": runtime_contract.runtime_id,
                "program_sha256": runtime_contract.program_sha256,
                **(
                    {
                        "descendant_path": "runtime_bin_first",
                        "runtime_bin_binding_sha256": (
                            runtime_contract.runtime_bin_binding_sha256
                        ),
                    }
                    if runtime_contract.runtime_bin_binding_sha256 is not None
                    else {}
                ),
            }
            if runtime_contract is not None
            else None
        ),
        "elapsed_seconds": round(elapsed_seconds, 6),
        "preparation_elapsed_seconds": round(preparation_elapsed, 6),
        "command_plus_preparation_elapsed_seconds": round(
            elapsed_seconds + preparation_elapsed, 6
        ),
        "user_cpu_seconds": (
            round(user_cpu_seconds, 6) if user_cpu_seconds is not None else None
        ),
        "system_cpu_seconds": (
            round(system_cpu_seconds, 6) if system_cpu_seconds is not None else None
        ),
        "max_rss_kib": max_rss_kib,
        "resource_accounting": resource_accounting,
        "exit_code": exit_code,
        "signal": terminating_signal,
        "completion_reason": completion_reason,
    }
    temporary_name: str | None = None
    try:
        with tempfile.NamedTemporaryFile(
            mode="w",
            encoding="utf-8",
            dir=destination.parent,
            prefix=f".{destination.name}.",
            suffix=".tmp",
            delete=False,
        ) as temporary:
            temporary_name = temporary.name
            os.chmod(temporary_name, 0o600)
            json.dump(report, temporary, indent=2, sort_keys=True)
            temporary.write("\n")
            temporary.flush()
            os.fsync(temporary.fileno())
        os.replace(temporary_name, destination)
        temporary_name = None
    finally:
        if temporary_name is not None:
            try:
                os.unlink(temporary_name)
            except FileNotFoundError:
                pass


def terminate_process(
    process: subprocess.Popen[bytes],
    initial_signal: signal.Signals,
    isolated_process_group: bool,
) -> int:
    """Stop one owned command, escalating after a short bounded grace period."""

    signal_used = initial_signal
    try:
        if isolated_process_group:
            os.killpg(process.pid, initial_signal)
        elif process.poll() is None:
            process.send_signal(initial_signal)
    except ProcessLookupError:
        pass
    grace_deadline = time.monotonic() + TERMINATION_GRACE_SECONDS
    while True:
        leader_exited = process.poll() is not None
        group_exited = leader_exited
        if isolated_process_group:
            try:
                os.killpg(process.pid, 0)
                group_exited = False
            except ProcessLookupError:
                group_exited = True
        if leader_exited and group_exited:
            break
        remaining = grace_deadline - time.monotonic()
        if remaining <= 0:
            signal_used = signal.SIGKILL
            try:
                if isolated_process_group:
                    os.killpg(process.pid, signal.SIGKILL)
                else:
                    process.kill()
            except ProcessLookupError:
                pass
            if not leader_exited:
                process.wait()
            break
        if leader_exited:
            time.sleep(min(0.01, remaining))
            continue
        try:
            process.wait(timeout=min(0.01, remaining))
        except subprocess.TimeoutExpired:
            pass
    return int(signal_used)


def execute(
    arguments: list[str],
    cwd: Path | None,
    environment: dict[str, str] | None,
    measurement: Path | None,
    cache_specs: tuple[CacheSpec, ...],
    cache_preparations: tuple[CachePreparation, ...],
    source_preparation: SourcePreparation | None,
    runtime_contract: RuntimeContract | None,
    cross_worktree: bool,
    resource_profile: str | None,
    comparison_key: str | None,
    timeout_seconds: float | None,
    pass_fds: tuple[int, ...] = (),
    observation_consumer: Callable[[ExecutionObservation], None] | None = None,
) -> int:
    time_report: Path | None = None
    systemd_run: str | None = None
    if resource_profile is not None:
        systemd_run = shutil.which("systemd-run")
        if systemd_run is None:
            raise RuntimeError("required capability systemd-run is unavailable")
    if measurement is not None:
        gnu_time = shutil.which("time")
        if gnu_time is None:
            raise RuntimeError(
                "required capability GNU time is unavailable for measurement"
            )
        with tempfile.NamedTemporaryFile(
            prefix=".glaeda-hot-run-time-", delete=False
        ) as temporary:
            time_report = Path(temporary.name)
        arguments = [
            os.path.abspath(gnu_time),
            "--quiet",
            "--format",
            "%U\n%S\n%M\n%x",
            "--output",
            os.fspath(time_report),
            *arguments,
        ]
    if systemd_run is not None:
        arguments = [
            os.path.abspath(systemd_run),
            "--user",
            "--scope",
            "--quiet",
            "--collect",
            "--expand-environment=no",
            *[
                item
                for property_value in RESOURCE_PROFILE_PROPERTIES[resource_profile]
                for item in ("--property", property_value)
            ],
            *arguments,
        ]
    machine_before = observe_machine() if measurement is not None else None
    started = time.monotonic()
    isolated_process_group = timeout_seconds is not None
    try:
        process = subprocess.Popen(
            arguments,
            cwd=cwd,
            env=environment,
            start_new_session=isolated_process_group,
            pass_fds=pass_fds,
        )
    except BaseException:
        if time_report is not None:
            try:
                time_report.unlink()
            except FileNotFoundError:
                pass
        raise
    terminating_signal: int | None = None
    completion_reason: str | None = None
    exit_code: int | None = None
    try:
        returncode = process.wait(timeout=timeout_seconds)
    except subprocess.TimeoutExpired:
        terminating_signal = terminate_process(
            process, signal.SIGTERM, isolated_process_group=True
        )
        exit_code = 124
        completion_reason = "deadline_exceeded"
    except KeyboardInterrupt:
        # Without an explicit timeout the terminal already delivered SIGINT to the
        # child. Waiting first gives it the same opportunity to clean up as before.
        if isolated_process_group:
            terminating_signal = terminate_process(
                process, signal.SIGINT, isolated_process_group=True
            )
        else:
            try:
                process.wait(timeout=TERMINATION_GRACE_SECONDS)
                terminating_signal = signal.SIGINT
            except subprocess.TimeoutExpired:
                terminating_signal = terminate_process(
                    process, signal.SIGTERM, isolated_process_group=False
                )
        exit_code = 130
        completion_reason = "operator_interrupt"
    elapsed = time.monotonic() - started
    timed_usage: tuple[float, float, int, int] | None = None
    if time_report is not None:
        try:
            lines = time_report.read_text(encoding="utf-8").splitlines()
            if len(lines) == 4:
                timed_usage = (
                    float(lines[0]),
                    float(lines[1]),
                    int(lines[2]),
                    int(lines[3]),
                )
        except (OSError, ValueError):
            pass
        finally:
            try:
                time_report.unlink()
            except FileNotFoundError:
                pass
    if completion_reason is None:
        if returncode < 0:
            terminating_signal = -returncode
            exit_code = 128 + terminating_signal
            completion_reason = "signaled"
        elif (
            timed_usage is not None
            and timed_usage[3] == 0
            and 128 <= returncode <= 255
            and returncode - 128 in signal.valid_signals()
        ):
            terminating_signal = returncode - 128
            exit_code = returncode
            completion_reason = "signaled"
        else:
            exit_code = returncode
            completion_reason = "exited"
    user_cpu_seconds: float | None
    system_cpu_seconds: float | None
    max_rss_kib: int | None
    if timed_usage is None:
        user_cpu_seconds = None
        system_cpu_seconds = None
        max_rss_kib = None
        resource_accounting = (
            "not_measured"
            if measurement is None
            else "unavailable_for_measured_command"
        )
    else:
        user_cpu_seconds, system_cpu_seconds, max_rss_kib, _ = timed_usage
        resource_accounting = (
            "gnu_time_command_tree"
            if resource_profile is None
            else "gnu_time_inside_scope"
        )
    if completion_reason in ("deadline_exceeded", "operator_interrupt"):
        user_cpu_seconds = None
        system_cpu_seconds = None
        max_rss_kib = None
        resource_accounting = "unavailable_after_forced_termination"
    if measurement is not None:
        machine_after = observe_machine()
        assert machine_before is not None
        assert exit_code is not None
        assert completion_reason is not None
        write_measurement(
            measurement,
            elapsed,
            user_cpu_seconds,
            system_cpu_seconds,
            max_rss_kib,
            resource_accounting,
            exit_code,
            terminating_signal,
            completion_reason,
            timeout_seconds,
            cache_specs,
            cache_preparations,
            source_preparation,
            runtime_contract,
            cross_worktree,
            resource_profile,
            comparison_key,
            machine_before,
            machine_after,
        )
    if observation_consumer is not None:
        observation_consumer(
            ExecutionObservation(
                elapsed_seconds=elapsed,
                preparation_elapsed_seconds=sum(
                    item.elapsed_seconds for item in cache_preparations
                )
                + (
                    source_preparation.elapsed_seconds
                    if source_preparation is not None
                    else 0.0
                ),
            )
        )
    return exit_code


def run(
    resident_argument: Path,
    task_argument: Path,
    state_argument: Path | None,
    cache_arguments: list[str],
    seed_source_mtimes: bool,
    runtime_id: str | None,
    runtime_sha256: str | None,
    runtime_bin_argument: Path | None,
    measurement: Path | None,
    resource_profile: str | None,
    comparison_key: str | None,
    timeout_seconds: float | None,
    command: list[str],
    verbose: bool,
) -> int:
    resident = worktree_root(resident_argument)
    task = worktree_root(task_argument)
    common_git = common_git_directory(resident)
    if common_git != common_git_directory(task):
        raise RuntimeError("resident and task must be worktrees of the same Git repository")
    resident_git = task_git_directory(resident)
    task_git = task_git_directory(task)
    try:
        resident_git_relative = resident_git.relative_to(common_git)
        task_git_relative = task_git.relative_to(common_git)
    except ValueError as error:
        raise RuntimeError("worktree Git metadata is outside the common Git directory") from error

    task_cwd = task_argument.resolve()
    try:
        relative_cwd = task_cwd.relative_to(task)
    except ValueError as error:
        raise RuntimeError("task working directory is outside its Git worktree") from error
    stable_cwd = resident / relative_cwd
    runtime_declaration = parse_runtime_contract(runtime_id, runtime_sha256)
    runtime_bin = observe_runtime_bin(
        runtime_bin_argument,
        runtime_declaration.runtime_id if runtime_declaration is not None else None,
    )
    bound_environment = runtime_environment(runtime_bin)
    resolved_command = resolve_program(
        command,
        task_cwd,
        bound_environment.get("PATH") if bound_environment is not None else None,
    )
    runtime_contract = verify_runtime_contract(
        runtime_declaration,
        Path(resolved_command[0]),
        runtime_bin,
    )
    explicit_cache_specs = (
        parse_cache_specs(cache_arguments) if cache_arguments else None
    )
    cache_specs = explicit_cache_specs or default_cache_specs(resident)

    if seed_source_mtimes and not any(
        spec.path == Path("target") and spec.mode == "private-copy"
        for spec in cache_specs
    ):
        raise RuntimeError(
            "seed source mtimes require a task-private target copy"
        )

    if resident == task:
        direct_cache_specs = explicit_cache_specs or ()
        if any(spec.mode != "native" for spec in direct_cache_specs):
            raise RuntimeError(
                "same-worktree cache observations require explicit native mode"
            )
        return execute(
            resolved_command,
            task_cwd,
            bound_environment,
            measurement,
            direct_cache_specs,
            (),
            None,
            runtime_contract,
            cross_worktree=False,
            resource_profile=resource_profile,
            comparison_key=comparison_key,
            timeout_seconds=timeout_seconds,
        )

    if any(spec.mode == "native" for spec in cache_specs):
        raise RuntimeError("native cache mode requires the same worktree")

    bubblewrap = shutil.which("bwrap")
    if bubblewrap is None:
        raise RuntimeError("required capability bwrap is unavailable")
    has_private_state = any(
        spec.mode in ("overlay", "private", "private-copy") for spec in cache_specs
    )
    implicit_worktree_identity: WorktreeStateIdentity | None = None
    if state_argument is None:
        implicit_worktree_identity = observe_worktree_state_identity(
            resident,
            task,
            common_git,
            resident_git,
            task_git,
            resident_git_relative,
            task_git_relative,
        )
        state_base = Path(
            os.path.abspath(
                default_state_root(
                    resident,
                    task,
                    cache_specs,
                    implicit_worktree_identity,
                )
            )
        )
    else:
        state_base = Path(os.path.abspath(state_argument))
    state_root = runtime_state_root(state_base, runtime_contract)

    lock_fd: int | None = None
    namespace_lock_fd: int | None = None
    namespace_root: Path | None = None
    publication_disposition: str | None = None
    value_publication_disposition: str | None = None
    lifecycle_disposition = "explicit_state"
    git_view: tempfile.TemporaryDirectory[str] | None = None
    pinned_worktree: PinnedWorktreeState | None = None
    cache_source_fds: list[int] = []
    try:
        if implicit_worktree_identity is not None:
            namespace_root = state_base.parent
            ensure_private_directory(namespace_root)
            namespace_lock_fd = open_private_lock(
                namespace_root / HOT_STATE_NAMESPACE_LOCK,
                "hot-state namespace lock",
            )
            try:
                fcntl.flock(
                    namespace_lock_fd, fcntl.LOCK_EX | fcntl.LOCK_NB
                )
                collection = collect_one_unreachable_state(
                    namespace_root, state_base.name
                )
                lifecycle_disposition = collection
                if collection == "nothing_eligible":
                    try:
                        lifecycle_disposition = retire_one_low_value_state(
                            namespace_root, state_base.name
                        )
                    except (OSError, RuntimeError):
                        lifecycle_disposition = "value_catalog_unavailable"
                has_exclusive_namespace = True
            except BlockingIOError:
                fcntl.flock(namespace_lock_fd, fcntl.LOCK_SH)
                has_exclusive_namespace = False
                lifecycle_disposition = "concurrent_namespace"

            if not has_exclusive_namespace and not state_base.exists():
                fcntl.flock(namespace_lock_fd, fcntl.LOCK_UN)
                fcntl.flock(namespace_lock_fd, fcntl.LOCK_EX)
                has_exclusive_namespace = True

            expected_manifest = producer_manifest_document(
                state_base,
                resident,
                task,
                common_git,
                resident_git,
                task_git,
                cache_specs,
                implicit_worktree_identity,
            )
            publication_disposition = publish_implicit_state_base(
                state_base, expected_manifest
            )
            if has_exclusive_namespace:
                fcntl.flock(namespace_lock_fd, fcntl.LOCK_SH)
        else:
            ensure_private_directory(state_base)

        try:
            state_root.lstat()
            runtime_state_preexisting = True
        except FileNotFoundError:
            runtime_state_preexisting = False
        ensure_private_directory(state_root)
        value_publication_disposition = (
            publication_disposition
            if state_root == state_base
            else "reused" if runtime_state_preexisting else "created"
        )
        lock_fd = open_private_lock(state_root / "lock", "hot-state lock")
        try:
            fcntl.flock(lock_fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            raise RuntimeError("this task's hot state is already in use") from error
        private_copy_candidates: list[tuple[CacheSpec, Path, Path]] = []
        for spec in cache_specs:
            resident_cache = resident / spec.path
            task_cache = task / spec.path
            if not resident_cache.is_dir() or resident_cache.is_symlink():
                raise RuntimeError(f"resident cache is unavailable: {spec.path}")
            if task_cache.exists() and (
                not task_cache.is_dir() or task_cache.is_symlink()
            ):
                raise RuntimeError(
                    f"task cache path is not a plain directory: {spec.path}"
                )
            task_cache.mkdir(mode=0o700, parents=True, exist_ok=True)
            if spec.mode == "ro":
                continue
            cache_identity = hashlib.sha256(os.fsencode(spec.path)).hexdigest()[:16]
            if spec.mode in ("private", "private-copy"):
                private_prefix = (
                    "private" if spec.mode == "private" else "private-copy"
                )
                private = state_root / f"{private_prefix}-{cache_identity}"
                if spec.mode == "private":
                    ensure_private_directory(private)
                else:
                    private_copy_candidates.append((spec, resident_cache, private))
                continue
            upper = state_root / f"upper-{cache_identity}"
            work = state_root / f"work-{cache_identity}"
            ensure_private_directory(upper)
            ensure_private_directory(work)

        pinned_worktree = pin_worktree_state(
            resident,
            task,
            common_git,
            resident_git,
            task_git,
            resident_git_relative,
            task_git_relative,
        )
        if (
            implicit_worktree_identity is not None
            and pinned_worktree.identity != implicit_worktree_identity
        ):
            raise RuntimeError("worktree generation changed during hot-state preflight")
        if implicit_worktree_identity is not None:
            revalidate_worktree_state_identity(
                pinned_worktree.identity,
                resident,
                task,
                common_git,
                resident_git,
                task_git,
                resident_git_relative,
                task_git_relative,
            )

        cache_preparations = tuple(
            prepare_private_copy(spec, resident_cache, private)
            for spec, resident_cache, private in private_copy_candidates
        )
        source_preparation = (
            prepare_seed_source_metadata(resident, task, cache_preparations)
            if seed_source_mtimes
            else None
        )

        git_view = tempfile.TemporaryDirectory(prefix=".git-view-", dir=state_root)
        git_view_root = Path(git_view.name)
        git_common_view = git_view_root / "common"
        git_common_view.mkdir(mode=0o700)
        stable_task_git = git_common_view / task_git_relative
        git_pointer = git_view_root / "pointer"
        git_pointer.write_text(f"gitdir: {stable_task_git}\n", encoding="utf-8")
        git_pointer.chmod(0o600)

        environment = bound_environment or os.environ.copy()
        bind_cross_worktree_cache_environment(resident, cache_specs, environment)
        if runtime_bin is None:
            rust_tool_environment(task, resolved_command, environment)

        revalidate_worktree_state_identity(
            pinned_worktree.identity,
            resident,
            task,
            common_git,
            resident_git,
            task_git,
            resident_git_relative,
            task_git_relative,
        )

        cache_arguments_for_bwrap: list[str] = []
        for spec in cache_specs:
            resident_cache = resident / spec.path
            if spec.mode == "ro":
                resident_cache_fd, _ = pin_plain_object(
                    spec.path,
                    "resident cache",
                    directory=True,
                    dir_fd=pinned_worktree.resident_root_fd,
                    require_current_user=False,
                )
                cache_source_fds.append(resident_cache_fd)
                cache_arguments_for_bwrap.extend(
                    [
                        "--ro-bind-fd",
                        str(resident_cache_fd),
                        os.fspath(resident_cache),
                    ]
                )
                continue
            cache_identity = hashlib.sha256(os.fsencode(spec.path)).hexdigest()[:16]
            if spec.mode in ("private", "private-copy"):
                private_prefix = (
                    "private" if spec.mode == "private" else "private-copy"
                )
                private = state_root / f"{private_prefix}-{cache_identity}"
                private_fd, _ = pin_plain_object(
                    private,
                    "private cache",
                    directory=True,
                )
                cache_source_fds.append(private_fd)
                cache_arguments_for_bwrap.extend(
                    ["--bind-fd", str(private_fd), os.fspath(resident_cache)]
                )
                continue
            resident_cache_fd, _ = pin_plain_object(
                spec.path,
                "resident cache",
                directory=True,
                dir_fd=pinned_worktree.resident_root_fd,
                require_current_user=False,
            )
            cache_source_fds.append(resident_cache_fd)
            lower = git_view_root / f"lower-{cache_identity}"
            lower.mkdir(mode=0o700)
            upper = state_root / f"upper-{cache_identity}"
            work = state_root / f"work-{cache_identity}"
            cache_arguments_for_bwrap.extend(
                [
                    "--ro-bind-fd",
                    str(resident_cache_fd),
                    os.fspath(lower),
                    "--overlay-src",
                    os.fspath(lower),
                    "--overlay",
                    os.fspath(upper),
                    os.fspath(work),
                    os.fspath(resident_cache),
                ]
            )

        arguments = [
            os.path.abspath(bubblewrap),
            "--die-with-parent",
            "--dev-bind",
            "/",
            "/",
            "--bind-fd",
            str(pinned_worktree.common_git_directory_fd),
            os.fspath(git_common_view),
            "--bind-fd",
            str(pinned_worktree.task_git_directory_fd),
            os.fspath(stable_task_git),
            "--bind-fd",
            str(pinned_worktree.task_root_fd),
            os.fspath(resident),
            "--ro-bind",
            os.fspath(git_pointer),
            os.fspath(resident / ".git"),
            *cache_arguments_for_bwrap,
            "--chdir",
            os.fspath(stable_cwd),
            *resolved_command,
        ]
        if verbose:
            state_name = (
                state_root.name
                if has_private_state or implicit_worktree_identity is not None
                else "none"
            )
            print(
                f"hot-run: trusted task={task.name} resident={resident.name} "
                f"state={state_name} lifecycle={lifecycle_disposition}",
                file=sys.stderr,
            )
        execution_observations: list[ExecutionObservation] = []
        exit_code = execute(
            arguments,
            None,
            environment,
            measurement,
            cache_specs,
            cache_preparations,
            source_preparation,
            runtime_contract,
            cross_worktree=True,
            resource_profile=resource_profile,
            comparison_key=comparison_key,
            timeout_seconds=timeout_seconds,
            pass_fds=(
                pinned_worktree.common_git_directory_fd,
                pinned_worktree.task_git_directory_fd,
                pinned_worktree.task_root_fd,
                *cache_source_fds,
            ),
            observation_consumer=execution_observations.append,
        )
        if (
            exit_code == 0
            and implicit_worktree_identity is not None
            and namespace_root is not None
            and value_publication_disposition is not None
            and len(execution_observations) == 1
        ):
            try:
                use_disposition = record_successful_hot_state_use(
                    namespace_root,
                    state_base,
                    value_publication_disposition,
                    comparison_key,
                    runtime_contract,
                    resource_profile,
                    execution_observations[0],
                    namespace_lock_fd,
                )
            except (OSError, RuntimeError):
                use_disposition = "record_unavailable"
            if verbose:
                print(
                    f"hot-run: lifecycle-success={use_disposition}",
                    file=sys.stderr,
                )
        if namespace_lock_fd is not None:
            fcntl.flock(namespace_lock_fd, fcntl.LOCK_UN)
            os.close(namespace_lock_fd)
            namespace_lock_fd = None
        return exit_code
    finally:
        for descriptor in reversed(cache_source_fds):
            os.close(descriptor)
        if pinned_worktree is not None:
            close_pinned_worktree_state(pinned_worktree)
        if lock_fd is not None:
            os.close(lock_fd)
        if namespace_lock_fd is not None:
            os.close(namespace_lock_fd)
        if git_view is not None:
            git_view.cleanup()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--resident", type=Path, default=ROOT)
    parser.add_argument("--task", type=Path, default=Path.cwd())
    parser.add_argument("--state", type=Path)
    parser.add_argument(
        "--runtime-id",
        help=(
            "bounded public runtime identity; without --runtime-sha256, observe and "
            "namespace by the resolved command's exact digest"
        ),
    )
    parser.add_argument(
        "--runtime-sha256",
        metavar="sha256:HEX",
        help=(
            "optional expected SHA-256 of the resolved command executable; requires "
            "--runtime-id and refuses drift"
        ),
    )
    parser.add_argument(
        "--runtime-bin",
        type=Path,
        help=(
            "bind one canonical toolchain bin directory, resolve the launched "
            "executable there, and place it first in descendant PATH; requires "
            "--runtime-id"
        ),
    )
    parser.add_argument(
        "--measurement",
        type=Path,
        help="atomically write one bounded developer-observation JSON measurement",
    )
    parser.add_argument(
        "--comparison-key",
        type=canonical_comparison_key,
        metavar="sha256:HEX",
        help=(
            "optional caller-owned exact-work comparison digest recorded only in "
            "the observation receipt"
        ),
    )
    parser.add_argument(
        "--resource-profile",
        choices=tuple(RESOURCE_PROFILE_PROPERTIES),
        help=(
            "run in the measured heavy scope or the work-conserving "
            "CPUWeight=25 background scope"
        ),
    )
    parser.add_argument(
        "--timeout",
        type=positive_seconds,
        metavar="SECONDS",
        help="terminate the owned command tree after this wall-clock deadline",
    )
    parser.add_argument(
        "--cache",
        action="append",
        default=[],
        metavar="PATH[:native|overlay|private|private-copy|ro]",
        help=(
            "select one resident-relative state path; native records direct "
            "same-worktree mutation, while cross-worktree execution defaults to "
            "target:private-copy when present"
        ),
    )
    parser.add_argument(
        "--seed-source-mtimes",
        action="store_true",
        help=(
            "on first target:private-copy seed only, give byte-identical tracked "
            "regular task files resident mtimes; requires caller-owned exact warm-parent proof"
        ),
    )
    parser.add_argument("--verbose", action="store_true")
    parser.add_argument("command", nargs=argparse.REMAINDER)
    arguments = parser.parse_args()
    command = arguments.command
    if command[:1] == ["--"]:
        command = command[1:]
    if not command:
        parser.error("a command is required after --")
    if arguments.comparison_key is not None and arguments.measurement is None:
        parser.error("--comparison-key requires --measurement")
    if arguments.resource_profile is not None and arguments.timeout is None:
        parser.error("--resource-profile requires --timeout")
    try:
        return run(
            arguments.resident,
            arguments.task,
            arguments.state,
            arguments.cache,
            arguments.seed_source_mtimes,
            arguments.runtime_id,
            arguments.runtime_sha256,
            arguments.runtime_bin,
            arguments.measurement,
            arguments.resource_profile,
            arguments.comparison_key,
            arguments.timeout,
            command,
            arguments.verbose,
        )
    except (OSError, RuntimeError, subprocess.SubprocessError) as error:
        print(f"hot-run error: {error}", file=sys.stderr)
        return 2
    except KeyboardInterrupt:
        return 130


if __name__ == "__main__":
    raise SystemExit(main())
