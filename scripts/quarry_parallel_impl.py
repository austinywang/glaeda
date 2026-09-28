#!/usr/bin/env python3
"""Glaeda-managed Quarry parallel-full batch on owned Linux compute.

One admitted task runs Quarry's exact repository-owned `parallel-full`
verifier (complete corpus, internal sharding, `--receipt-stdout`) inside
the shared task-private systemd/bubblewrap boundary and returns one
bounded canonical receipt. This is a physical-experiment adapter, not a
scheduler: no queue, no daemon, no remote CLI, no caller-selected
command, mounts, or resource properties.

Workload authority stays with the Quarry repository: the task runs
Quarry's exact launcher with the exact admitted worker grant. The
pytest closure is the resident development venv's pure-Python
site-packages rebound read-only (no installs, no network); the Quarry
package itself always resolves to the task-private source first.
Glaeda owns admission, isolation, bounded capture, cleanup observation,
and the outer receipt. Machine stdout is evidence transport: with
`--receipt-stdout` it carries exactly the canonical v2 receipt bytes
(<= 65,536); only digests and a closed terminal set enter the durable
receipt. Semantic equality is proven by digest comparison against the
cold direct path, never by parsing human output.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import stat
import subprocess
import sys
import tempfile
import time
from pathlib import Path

import owned_linux_task as owned_task
from owned_linux_task import Refusal
from quarry_sweep_impl import (
    SHA256_PATTERN,
    TERMINALS,
    canonical_bytes,
    decide_admission,
    exact_directory,
    observe_node,
    sha256,
)

SCHEMA_VERSION = 1
DOCUMENT_TYPE = "glaeda-quarry-parallel-receipt"
WORKLOAD_ID = "quarry-parallel/v1"
REPOSITORY = "teamleaderleo/quarry"
RECEIPT_SCHEMA_VERSION = 2
MAX_RECEIPT_BYTES = 65_536

# parallel-full at grant 4 measured 84 s wall / 359% CPU direct on Big Red.
# Grant: 4 of 16 CPUs, 8 GiB of 30 GiB, no network. VPN lanes and
# interactive RDP keep the remaining headroom by construction.
WORKERS = 4
SYSTEMD_PROPERTIES = (
    "CPUQuota=400%",
    "MemoryHigh=6G",
    "MemoryMax=8G",
    "TasksMax=512",
    "RuntimeMaxSec=900",
    "KillMode=mixed",
    "NoNewPrivileges=yes",
    "RestrictSUIDSGID=yes",
)
BUILD_TMPFS_BYTES = 256 * 1024 * 1024
QUARRY_TMP_TMPFS_BYTES = 2 * 1024 * 1024 * 1024
DEADLINE_SECONDS = 900

VENV_SITE_PACKAGES = (
    "/home/leo/Projects/quarry/.venv/lib/python3.14/site-packages"
)
CLOSED_PYTHONPATH = "/workspace/source/src:/quarry-venv"
TASK_VENV = "/venv"
TASK_PYTHON = "/venv/bin/python"
# Minimal venv skeleton: pyvenv.cfg beside bin/ makes CPython treat the
# bound interpreter as a venv whose site-packages is the read-only
# resident closure. Fixed bytes, pinned in the workload identity.
PYVENV_CFG = b"home = /usr/bin\ninclude-system-site-packages = false\n"

OID_PATTERN = re.compile(r"^[a-f0-9]{40}$")


def workload_identity(commit: str, tree: str, toolchain: dict[str, object]) -> dict[str, object]:
    return {
        "schema_version": 1,
        "workload_id": WORKLOAD_ID,
        "repository": REPOSITORY,
        "commit": commit,
        "tree": tree,
        "command": [
            TASK_PYTHON,
            "scripts/run_local_tests.py",
            "parallel-full",
            "--workers",
            str(WORKERS),
            "--receipt-stdout",
        ],
        "workers": WORKERS,
        "venv_skeleton_sha256": sha256(PYVENV_CFG),
        "toolchain": toolchain,
        "grant": {
            "cpu_quota": "400%",
            "memory_high": "6G",
            "memory_max": "8G",
            "tasks_max": 512,
            "deadline_seconds": DEADLINE_SECONDS,
            "network": "none",
        },
    }


def command_fingerprint(commit: str, tree: str, toolchain: dict[str, object]) -> str:
    return sha256(canonical_bytes(workload_identity(commit, tree, toolchain)))


def observe_toolchain(src_root: Path | None = None) -> dict[str, object]:
    """Observe the exact resident interpreter + pytest closure identity.

    Runs the closed interpreter once with the closure on PYTHONPATH and
    reports versions plus a content digest over sorted (path, size,
    bytes). No installs, no network, read-only.
    """
    packages = Path(VENV_SITE_PACKAGES)
    try:
        resolved = packages.resolve(strict=True)
    except OSError as error:
        raise Refusal("pytest closure is unavailable") from error
    if (
        resolved != packages
        or not packages.is_dir()
        or packages.is_symlink()
        or resolved.stat().st_uid != os.getuid()
    ):
        raise Refusal("pytest closure is not one canonical plain directory")
    entries: list[tuple[str, int]] = []
    digest = hashlib.sha256()
    for parent, directories, files in os.walk(resolved, followlinks=False):
        directories.sort()
        for name in sorted(files):
            path = Path(parent) / name
            try:
                info = path.lstat()
            except OSError as error:
                raise Refusal("pytest closure changed during observation") from error
            if not stat.S_ISREG(info.st_mode) or path.is_symlink():
                raise Refusal("pytest closure contains a non-regular object")
            relative = str(path.relative_to(resolved))
            entries.append((relative, info.st_size))
            digest.update(relative.encode("utf-8") + b"\0" + str(info.st_size).encode() + b"\0")
            with open(path, "rb") as stream:
                while block := stream.read(1024 * 1024):
                    digest.update(block)
    probe_env = {
        "PATH": "/usr/bin:/bin",
        "LC_ALL": "C",
        "PYTHONPATH": os.fspath(resolved),
        "PYTHONNOUSERSITE": "1",
    }
    if src_root is not None:
        probe_env["PYTHONPATH"] = f"{src_root}:{resolved}"
    completed = subprocess.run(
        [
            "/usr/bin/python3",
            "-c",
            "import sys, pytest; print(sys.version.split()[0]); print(pytest.__version__)",
        ],
        env=probe_env,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        timeout=60,
        check=False,
    )
    if completed.returncode != 0:
        raise Refusal("pytest closure does not execute under the closed interpreter")
    lines = completed.stdout.decode("ascii", errors="strict").splitlines()
    if len(lines) != 2 or not re.fullmatch(r"3\.14\.[0-9]+", lines[0]):
        raise Refusal("pytest closure interpreter is not canonical Python 3.14")
    return {
        "interpreter": "/usr/bin/python3",
        "interpreter_version": lines[0],
        "pytest_version": lines[1],
        "closure": os.fspath(resolved),
        "closure_entries": len(entries),
        "closure_sha256": "sha256:" + digest.hexdigest(),
        "source_precedence": check_source_precedence(src_root, resolved),
    }


def check_source_precedence(src_root: Path | None, closure: Path) -> str | None:
    """Prove the task source shadows the venv's editable install, if given."""
    if src_root is None:
        return None
    completed = subprocess.run(
        ["/usr/bin/python3", "-c", "import quarry; print(quarry.__file__)"],
        env={
            "PATH": "/usr/bin:/bin",
            "LC_ALL": "C",
            "PYTHONPATH": f"{src_root}:{closure}",
            "PYTHONNOUSERSITE": "1",
        },
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        timeout=60,
        check=False,
    )
    if completed.returncode != 0:
        raise Refusal("source precedence probe failed")
    location = completed.stdout.decode("ascii", errors="strict").strip()
    if not location.startswith(os.fspath(src_root) + os.sep):
        raise Refusal("task source does not shadow the resident editable install")
    return location


def verify_resident_source(repository_root: Path, commit: str, tree: str) -> None:
    if not OID_PATTERN.fullmatch(commit) or not OID_PATTERN.fullmatch(tree):
        raise Refusal("source commit/tree identity is invalid")
    observed = owned_task.run_control(
        ["/usr/bin/git", "rev-parse", f"{commit}^{{commit}}", f"{commit}^{{tree}}"],
        repository_root,
    ).decode("ascii", errors="strict").splitlines()
    if observed != [commit, tree]:
        raise Refusal("resident Quarry source does not match the exact request")
    remotes = owned_task.run_control(
        ["/usr/bin/git", "remote", "get-url", "--all", "origin"], repository_root
    ).decode("ascii", errors="strict").splitlines()
    admitted = {
        f"git@github.com:{REPOSITORY}.git",
        f"https://github.com:{REPOSITORY}.git",
        f"https://github.com/{REPOSITORY}.git",
        f"https://github.com/{REPOSITORY}",
    }
    if len(remotes) != 1 or remotes[0] not in admitted:
        raise Refusal("resident Quarry origin does not match the exact repository")
    status = owned_task.run_control(
        ["/usr/bin/git", "status", "--porcelain=v1", "-z", "--untracked-files=all"],
        repository_root,
    )
    if status:
        raise Refusal("resident Quarry checkout is not clean")


def resident_venv_bin() -> Path:
    return Path(VENV_SITE_PACKAGES).parents[2] / "bin"


# Never mirrored into the task prefix: interpreters (bound explicitly),
# shell activation scripts (resident paths), directories.
_SKIPPED_BIN_NAMES = ("python", "activate")
_SKIPPED_BIN_SUFFIXES = (".csh", ".fish", ".ps1", ".bat")


def mirror_venv_binaries(venv: Path, source_bin: Path) -> list[str]:
    """Mirror the resident venv's console scripts into the skeleton prefix.

    Profile commands resolve native binaries (ruff) under the executing
    interpreter's prefix. The skeleton prefix (/venv) must provide the
    same binaries or nested profile runs fail with a short receipt far
    from the cause (quarry #1136): entries living inside the resident
    venv are COPIED (a symlink would dangle — resident paths are not
    mounted in the task boundary), scripts whose shebang points into the
    resident venv are copied with the shebang rewritten to
    /venv/bin/python, and entries resolving outside the venv (system
    paths, visible in the boundary) are symlinked. Activation scripts
    and interpreters are never mirrored. Returns the sorted mirrored
    names (deterministic, receipt-safe).
    """
    bin_dir = venv / "bin"
    venv_root = source_bin.parent
    venv_root_bytes = os.fsencode(str(venv_root))
    mirrored: list[str] = []
    for entry in sorted(source_bin.iterdir(), key=lambda candidate: candidate.name):
        name = entry.name
        lowered = name.lower()
        if (
            lowered.startswith(_SKIPPED_BIN_NAMES)
            or lowered.endswith(_SKIPPED_BIN_SUFFIXES)
            or not entry.is_file()
        ):
            continue
        target = bin_dir / name
        try:
            content = entry.read_bytes()
            mode = entry.stat().st_mode
            try:
                inside = entry.resolve().is_relative_to(venv_root)
            except (OSError, RuntimeError):
                inside = True
        except OSError:
            continue
        first = content.split(b"\n", 1)[0]
        if first.startswith(b"#!") and venv_root_bytes in first:
            # Preserve a shebang argument vector if present (#!/x/python -E).
            pieces = first[2:].split()
            arguments = b" ".join(pieces[1:]) if len(pieces) > 1 else b""
            rewritten = b"#!/venv/bin/python" + (b" " + arguments if arguments else b"")
            target.write_bytes(rewritten + b"\n" + content[len(first) + 1 :])
            try:
                target.chmod(0o755)
            except OSError:
                pass
        elif inside:
            target.write_bytes(content)
            try:
                target.chmod(stat.S_IMODE(mode))
            except OSError:
                pass
        else:
            try:
                os.symlink(entry.resolve(), target)
            except OSError:
                continue
        mirrored.append(name)
    return mirrored


def build_venv_skeleton(task_root: Path) -> Path:
    """Materialize the fixed task-private venv skeleton (no resident writes).

    Layout: venv/pyvenv.cfg, venv/bin/python{,3,3.14} symlinks to the
    system interpreter, venv/lib/python3.14/site-packages symlink to the
    read-only closure bind. Returns the venv root for mounting at /venv.
    """
    venv = task_root / "venv"
    bin_dir = venv / "bin"
    site_parent = venv / "lib" / "python3.14"
    bin_dir.mkdir(mode=0o700, parents=True)
    site_parent.mkdir(mode=0o700, parents=True)
    (venv / "pyvenv.cfg").write_bytes(PYVENV_CFG)
    os.symlink("/usr/bin/python3", bin_dir / "python")
    os.symlink("/usr/bin/python3", bin_dir / "python3")
    os.symlink("/usr/bin/python3", bin_dir / "python3.14")
    os.symlink("/quarry-venv", site_parent / "site-packages")
    mirror_venv_binaries(venv, resident_venv_bin())
    return venv


def launcher_argv() -> list[str]:
    return [
        TASK_PYTHON,
        "scripts/run_local_tests.py",
        "parallel-full",
        "--workers",
        str(WORKERS),
        "--receipt-stdout",
    ]


def task_environment() -> dict[str, str]:
    return {
        "PATH": "/usr/bin:/bin",
        "HOME": "/home/project",
        "LC_ALL": "C",
        "PYTHONPATH": CLOSED_PYTHONPATH,
        "PYTHONNOUSERSITE": "1",
        "PYTHONDONTWRITEBYTECODE": "1",
        "TZ": "UTC",
        # Shard worktrees live under TMPDIR; the shared 512 MiB /tmp
        # tmpfs cannot hold fifteen materialized worktrees, so this
        # workload mounts its own task-private tmpfs (see sandbox below).
        "TMPDIR": "/quarry-tmp",
    }


def run_launcher_direct(
    repository_root: Path, venv_bin: Path
) -> tuple[str, int, float, bytes, bytes]:
    """Cold path: same fixed argv/env, same interpreter, no sandbox or grant."""
    started = time.monotonic()
    try:
        completed = subprocess.run(
            [
                os.fspath(venv_bin),
                "scripts/run_local_tests.py",
                "parallel-full",
                "--workers",
                str(WORKERS),
                "--receipt-stdout",
            ],
            cwd=repository_root,
            env={
                **task_environment(),
                "PYTHONPATH": f"{repository_root}/src:{VENV_SITE_PACKAGES}",
            },
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=DEADLINE_SECONDS,
            check=False,
        )
    except subprocess.TimeoutExpired:
        elapsed = time.monotonic() - started
        return ("timed_out", 124, elapsed, b"", b"cold parallel-full exceeded its deadline")
    elapsed = time.monotonic() - started
    terminal = "succeeded" if completed.returncode == 0 else "failed"
    return (
        terminal,
        completed.returncode,
        elapsed,
        completed.stdout,
        completed.stderr,
    )


def materialize_writable(
    repository_root: Path, task_root: Path, commit: str, tree: str
) -> Path:
    """Task-private WRITABLE checkout for worktree-creating verifiers.

    parallel-full creates git worktrees inside the source checkout, which
    a read-only bind cannot serve. Clone the exact admitted pristine copy
    locally (full object copy, no hardlinks back to shared state) and
    revalidate commit/tree/cleanliness on the copy itself.
    """
    pristine = owned_task.materialize(repository_root, task_root, commit, tree)
    writable = task_root / "writable"
    owned_task.run_control(
        [
            "/usr/bin/git",
            "clone",
            "--quiet",
            "--local",
            "--no-hardlinks",
            "--no-tags",
            os.fspath(pristine),
            os.fspath(writable),
        ]
    )
    # The verifier observes refs/remotes/origin/main (falling back to
    # refs/heads/main) for repository state. A detached single-commit
    # clone carries neither, so fetch exactly that ref from the admitted
    # resident checkout. Refs are not working-tree files; cleanliness is
    # unaffected. Fail closed if the resident ref is absent or mismatched.
    resident_main = owned_task.run_control(
        ["/usr/bin/git", "rev-parse", "refs/remotes/origin/main"], repository_root
    ).decode("ascii", errors="strict").strip()
    if not OID_PATTERN.fullmatch(resident_main):
        raise Refusal("resident origin/main identity is invalid")
    owned_task.run_control(
        [
            "/usr/bin/git",
            "-C",
            os.fspath(writable),
            "fetch",
            "--quiet",
            "--no-tags",
            os.fspath(repository_root),
            "refs/remotes/origin/main:refs/remotes/origin/main",
        ]
    )
    copied_main = owned_task.run_control(
        ["/usr/bin/git", "rev-parse", "refs/remotes/origin/main"], writable
    ).decode("ascii", errors="strict").strip()
    if copied_main != resident_main:
        raise Refusal("task-private origin/main ref does not match the resident")
    observed = owned_task.run_control(
        ["/usr/bin/git", "rev-parse", "HEAD", "HEAD^{tree}"], writable
    ).decode("ascii").splitlines()
    status = owned_task.run_control(
        ["/usr/bin/git", "status", "--porcelain=v1", "-z", "--untracked-files=all"],
        writable,
    )
    if observed != [commit, tree] or status:
        raise Refusal("task-private writable copy does not match the exact clean commit/tree")
    # The pristine copy carries an empty target/ mountpoint for the build
    # tmpfs; a clone drops empty directories, so recreate it on the copy.
    # The verifier demands a fully clean worktree including untracked
    # files, so the mountpoint is hidden via task-local .git/info/exclude
    # (per-clone metadata, never committed, canonical repo untouched).
    (writable / "target").mkdir(mode=0o700, exist_ok=True)
    with open(writable / ".git" / "info" / "exclude", "a", encoding="ascii") as stream:
        stream.write("target/\n")
    status = owned_task.run_control(
        ["/usr/bin/git", "status", "--porcelain=v1", "-z", "--untracked-files=all"],
        writable,
    )
    if status:
        raise Refusal("task-private writable copy is not clean after mountpoint setup")
    entries = 0
    bytes_seen = 0
    for parent, directories, files in os.walk(writable, followlinks=False):
        entries += len(directories) + len(files)
        if entries > owned_task.MAX_MATERIALIZED_SOURCE_ENTRIES:
            raise Refusal("task-private writable copy exceeds the entry ceiling")
        for name in files:
            bytes_seen += (Path(parent) / name).lstat().st_size
            if bytes_seen > owned_task.MAX_MATERIALIZED_SOURCE_BYTES:
                raise Refusal("task-private writable copy exceeds the byte ceiling")
    return writable


def sandbox_command(
    source: Path,
    closure: Path,
    venv: Path,
    cargo_root: Path,
    rustup_root: Path,
    unit_name: str,
) -> list[str]:
    recipe = [
        "--chdir",
        "/workspace/source",
        "--clearenv",
        "--setenv",
        "PATH",
        "/usr/bin:/bin",
        "--setenv",
        "HOME",
        "/home/project",
        "--setenv",
        "LC_ALL",
        "C",
        "--setenv",
        "PYTHONPATH",
        CLOSED_PYTHONPATH,
        "--setenv",
        "PYTHONNOUSERSITE",
        "1",
        "--setenv",
        "PYTHONDONTWRITEBYTECODE",
        "1",
        "--setenv",
        "TZ",
        "UTC",
        "--setenv",
        "TMPDIR",
        "/quarry-tmp",
        "--",
        *launcher_argv(),
    ]
    return owned_task.sandbox_command(
        source,
        cargo_root,
        rustup_root,
        unit_name,
        systemd_properties=list(SYSTEMD_PROPERTIES),
        build_tmpfs_bytes=BUILD_TMPFS_BYTES,
        mount_arguments=[
            "--ro-bind",
            os.fspath(closure),
            "/quarry-venv",
            "--bind",
            os.fspath(venv),
            TASK_VENV,
            "--size",
            str(QUARRY_TMP_TMPFS_BYTES),
            "--tmpfs",
            "/quarry-tmp",
        ],
        recipe_arguments=recipe,
        network=owned_task.TaskNetwork.NONE,
        # parallel-full creates git worktrees inside its checkout: the
        # task-private copy (full object copy, revalidated, destroyed
        # after) is bound writable. Canonical resident state is never
        # mounted and shares no inodes (--no-hardlinks).
        source_read_only=False,
    )


def summarize_receipt(raw: bytes, commit: str, tree: str, label: str) -> dict[str, object]:
    """Strict stdout validation: exactly the canonical v2 receipt bytes.

    Anything else (empty, prefixed, multi-line, oversized, unbound) is
    terminal evidence of a broken channel, never a partial success. A
    successful receipt binds verified_head; a terminal failure receipt
    carries verified_head null and instead binds the exact plan source
    commit/tree plus the admitted worker grant.
    """
    if not raw or len(raw) > MAX_RECEIPT_BYTES:
        raise Refusal(f"{label} receipt channel is missing or over its ceiling")
    if not raw.endswith(b"\n") or raw.count(b"\n") != 1:
        raise Refusal(f"{label} receipt channel is not one exact receipt line")
    try:
        value = json.loads(raw)
    except (UnicodeError, ValueError) as error:
        raise Refusal(f"{label} receipt is not JSON") from error
    if not isinstance(value, dict) or value.get("schema_version") != RECEIPT_SCHEMA_VERSION:
        raise Refusal(f"{label} receipt is not the exact v2 workload receipt")
    head = value.get("verified_head")
    if head is None:
        try:
            key = value["plan"]["key"]
            bound = (
                key["source"]["commit"] == commit
                and key["source"]["tree"] == tree
                and key["scheduler"]["workers"] == WORKERS
            )
        except (KeyError, TypeError) as error:
            raise Refusal(f"{label} failure receipt has no exact plan binding") from error
        if not bound:
            raise Refusal(f"{label} failure receipt does not match the exact workload")
    elif head != commit:
        raise Refusal(f"{label} receipt does not match the exact workload")
    result = value.get("result") if isinstance(value.get("result"), dict) else {}
    cleanup = value.get("cleanup") if isinstance(value.get("cleanup"), dict) else {}
    outcomes = value.get("outcomes") if isinstance(value.get("outcomes"), list) else []
    summary = []
    for outcome in outcomes:
        if not isinstance(outcome, dict) or "name" not in outcome:
            raise Refusal(f"{label} receipt outcomes are malformed")
        state = outcome.get("state")
        if state is None:
            state = "exit_" + str(outcome.get("exit_code"))
        entry: dict[str, object] = {"name": outcome["name"], "state": state}
        output_digest = outcome.get("output_sha256")
        if isinstance(output_digest, str) and SHA256_PATTERN.fullmatch(output_digest):
            entry["output_sha256"] = output_digest
        summary.append(entry)
    return {
        "termination_reason": result.get("termination_reason"),
        "cleanup_status": cleanup.get("status"),
        "aggregate_wall_millis": value.get("result", {}).get("aggregate_wall_millis")
        if isinstance(value.get("result"), dict)
        else None,
        "outcomes": summary,
        "receipt_bytes": len(raw),
    }


def receipt(
    commit: str,
    tree: str,
    fingerprint: str,
    toolchain: dict[str, object],
    admission_before: dict[str, object],
    admission_after: dict[str, object],
    admission_disposition: str,
    evidence_summary: dict[str, object] | None,
    evidence_sha256: str | None,
    terminal: str,
    exit_code: int,
    elapsed: float,
    settled: bool,
    cleanup_complete: bool,
    output_bytes: int,
    output_sha256: str,
    stderr_bytes: int,
    stderr_sha256: str,
    started_at_ms: int,
    settled_at_ms: int,
) -> dict[str, object]:
    return {
        "document_type": DOCUMENT_TYPE,
        "schema_version": SCHEMA_VERSION,
        "authority": "physical_execution_observation",
        "command_fingerprint": fingerprint,
        "source": {"repository": REPOSITORY, "commit": commit, "tree": tree},
        "toolchain": toolchain,
        "workload": {"id": WORKLOAD_ID, "workers": WORKERS, "receipt_stdout": True},
        "grant": {
            "cpu_quota": "400%",
            "memory_high": "6G",
            "memory_max": "8G",
            "tasks_max": 512,
            "deadline_seconds": DEADLINE_SECONDS,
            "network": "none",
        },
        "admission": {
            "disposition": admission_disposition,
            "before": admission_before,
            "after": admission_after,
        },
        "evidence_summary": evidence_summary,
        "evidence_sha256": evidence_sha256,
        "result": {
            "terminal_class": terminal,
            "exit_code": exit_code,
            "elapsed_seconds": round(elapsed, 6),
            "started_at_unix_millis": started_at_ms,
            "settled_at_unix_millis": settled_at_ms,
            "process_tree_settled": settled,
            "task_cleanup_complete": cleanup_complete,
            "output_bytes": output_bytes,
            "output_sha256": output_sha256,
            "stderr_bytes": stderr_bytes,
            "stderr_sha256": stderr_sha256,
        },
        "contains_private_content": False,
        "contains_credentials": False,
        "authorizes_work": False,
        "authorizes_effects": False,
        "authorizes_redispatch": False,
    }


RECEIPT_KEYS = (
    "document_type",
    "schema_version",
    "authority",
    "command_fingerprint",
    "source",
    "toolchain",
    "workload",
    "grant",
    "admission",
    "evidence_summary",
    "evidence_sha256",
    "result",
    "contains_private_content",
    "contains_credentials",
    "authorizes_work",
    "authorizes_effects",
    "authorizes_redispatch",
)


def valid_receipt(document: object, fingerprint: str) -> bool:
    if not isinstance(document, dict) or set(document) != set(RECEIPT_KEYS):
        return False
    if (
        document["document_type"] != DOCUMENT_TYPE
        or document["schema_version"] != SCHEMA_VERSION
        or document["authority"] != "physical_execution_observation"
        or document["command_fingerprint"] != fingerprint
    ):
        return False
    result = document.get("result")
    if not isinstance(result, dict) or result.get("terminal_class") not in TERMINALS:
        return False
    for key in (
        "contains_private_content",
        "contains_credentials",
        "authorizes_work",
        "authorizes_effects",
        "authorizes_redispatch",
    ):
        if document.get(key) is not False:
            return False
    return True


def emit(document: dict[str, object]) -> None:
    sys.stdout.buffer.write(canonical_bytes(document) + b"\n")


def refuse(error: BaseException) -> int:
    message = str(error) if isinstance(error, Refusal) else "quarry parallel-full failed"
    sys.stderr.write(
        json.dumps(
            {
                "document_type": "glaeda-quarry-parallel-error",
                "schema_version": SCHEMA_VERSION,
                "authority": "none",
                "problem": message[:500],
            },
            sort_keys=True,
            separators=(",", ":"),
        )
        + "\n"
    )
    return 75


def settle_producer_output(
    terminal: str, code: int, stdout: bytes, commit: str, tree: str, label: str
) -> tuple[str, int, dict[str, object] | None, str | None]:
    """Classify one producer attempt: receipt bytes or terminal evidence."""
    try:
        summary = summarize_receipt(stdout, commit, tree, label)
    except Refusal:
        summary = None
        if terminal == "succeeded":
            terminal = "failed"
            code = 99
    digest = sha256(stdout) if stdout else None
    return terminal, code, summary, digest


def run_cold(repository_root: Path, commit: str, tree: str, venv_bin: Path) -> int:
    verify_resident_source(repository_root, commit, tree)
    toolchain = observe_toolchain(repository_root / "src")
    before = observe_node()
    disposition, reason = decide_admission(before)
    if disposition != "admit":
        raise Refusal(f"parallel-full admission {disposition}: {reason}")
    started_at_ms = time.time_ns() // 1_000_000
    terminal, code, elapsed, stdout, stderr = run_launcher_direct(repository_root, venv_bin)
    terminal, code, summary, digest = settle_producer_output(
        terminal, code, stdout, commit, tree, "cold"
    )
    settled_at_ms = time.time_ns() // 1_000_000
    after = observe_node()
    emit(
        receipt(
            commit,
            tree,
            command_fingerprint(commit, tree, toolchain),
            toolchain,
            before,
            after,
            "admit",
            summary,
            digest,
            terminal,
            code,
            elapsed,
            True,
            True,
            len(stdout),
            sha256(stdout),
            len(stderr),
            sha256(stderr),
            started_at_ms,
            settled_at_ms,
        )
    )
    if stderr and terminal != "succeeded":
        sys.stderr.buffer.write(stderr[:8192])
    return 0


def run_managed(
    repository_root: Path,
    cargo_root: Path,
    rustup_root: Path,
    commit: str,
    tree: str,
) -> int:
    verify_resident_source(repository_root, commit, tree)
    toolchain = observe_toolchain()
    before = observe_node()
    disposition, reason = decide_admission(before)
    if disposition != "admit":
        raise Refusal(f"parallel-full admission {disposition}: {reason}")
    closure = Path(toolchain["closure"])
    fingerprint = command_fingerprint(commit, tree, toolchain)
    unit = f"glaeda-quarry-parallel-{fingerprint[7:19]}.service"
    if not owned_task.unit_absent(unit):
        raise Refusal("a parallel unit with the exact workload identity is already present")
    parent = Path(tempfile.mkdtemp(prefix="glaeda-quarry-parallel-"))
    task_root = parent / "task"
    terminal = "failed"
    code = 99
    elapsed = 0.0
    settled = False
    cleanup_complete = False
    stdout = b""
    stderr_bytes = 0
    stderr_digest = sha256(b"")
    summary: dict[str, object] | None = None
    digest: str | None = None
    started_at_ms = time.time_ns() // 1_000_000
    try:
        owned_task.prepare_task(task_root)
        source = materialize_writable(repository_root, task_root, commit, tree)
        venv = build_venv_skeleton(task_root)
        command = sandbox_command(source, closure, venv, cargo_root, rustup_root, unit)
        (terminal, code, elapsed, settled, out_bytes, out_digest,
         captured, overflow, err_bytes, err_digest) = owned_task.execute_capturing_split(
            command, unit=unit, deadline_seconds=DEADLINE_SECONDS,
            label="quarry-parallel", max_bytes=MAX_RECEIPT_BYTES,
        )
        if not settled:
            raise Refusal("physical process-tree settlement is incomplete")
        if overflow:
            summary, digest = None, None
            if terminal == "succeeded":
                terminal = "failed"
                code = 99
        else:
            terminal, code, summary, digest = settle_producer_output(
                terminal, code, captured, commit, tree, "managed"
            )
        stdout, output_digest = captured, out_digest
        stderr_bytes, stderr_digest = err_bytes, err_digest
        _ = out_bytes
    finally:
        try:
            owned_task.remove_task(task_root)
            cleanup_complete = True
        except (Refusal, OSError):
            terminal = "cleanup_incomplete"
        try:
            parent.rmdir()
        except OSError:
            terminal = "cleanup_incomplete"
            cleanup_complete = False
        if not owned_task.unit_absent(unit):
            owned_task.stop_unit(unit)
            if not owned_task.unit_absent(unit):
                terminal = "cleanup_incomplete"
                cleanup_complete = False
        settled = settled and owned_task.unit_absent(unit)
    settled_at_ms = time.time_ns() // 1_000_000
    after = observe_node()
    emit(
        receipt(
            commit,
            tree,
            fingerprint,
            toolchain,
            before,
            after,
            "admit",
            summary,
            digest,
            terminal,
            code,
            elapsed,
            settled,
            cleanup_complete,
            len(stdout),
            sha256(stdout),
            stderr_bytes,
            stderr_digest,
            started_at_ms,
            settled_at_ms,
        )
    )
    return 0


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__)
    subcommands = root.add_subparsers(dest="command", required=True)
    subcommands.add_parser("toolchain", help="emit the observed closure identity")
    cold = subcommands.add_parser("cold", help="run parallel-full directly (control arm)")
    cold.add_argument("--repository-root", required=True)
    cold.add_argument("--venv-bin", required=True,
                      help="exact venv interpreter for the control arm")
    cold.add_argument("--commit", required=True)
    cold.add_argument("--tree", required=True)
    managed = subcommands.add_parser("run", help="run parallel-full under Glaeda admission")
    managed.add_argument("--repository-root", required=True)
    managed.add_argument("--cargo-root", required=True)
    managed.add_argument("--rustup-root", required=True)
    managed.add_argument("--commit", required=True)
    managed.add_argument("--tree", required=True)
    return root


def main() -> int:
    arguments = parser().parse_args()
    try:
        if arguments.command == "toolchain":
            emit(
                {
                    "document_type": "glaeda-quarry-parallel-toolchain",
                    "schema_version": SCHEMA_VERSION,
                    "toolchain": observe_toolchain(),
                }
            )
            return 0
        repository_root = exact_directory(arguments.repository_root, "resident repository")
        if arguments.command == "cold":
            venv_bin = Path(arguments.venv_bin)
            if not venv_bin.is_absolute():
                raise Refusal("venv interpreter must be an absolute path")
            return run_cold(repository_root, arguments.commit, arguments.tree, venv_bin)
        cargo_root = exact_directory(arguments.cargo_root, "Cargo root")
        rustup_root = exact_directory(arguments.rustup_root, "rustup root")
        return run_managed(
            repository_root, cargo_root, rustup_root, arguments.commit, arguments.tree
        )
    except (OSError, RuntimeError, subprocess.SubprocessError) as error:
        return refuse(error)


if __name__ == "__main__":
    raise SystemExit(main())
