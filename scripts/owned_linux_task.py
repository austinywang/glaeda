"""Internal owned-Linux task mechanics; no remote CLI or request deserializer.

Only checked-in adapters supply commands, mounts, and resource properties. This
module grants no caller or execution authority. The verifier is its sole production
consumer until physical parity is accepted.
"""
from __future__ import annotations

from contextlib import nullcontext
from enum import Enum
import hashlib
import os
from pathlib import Path
import selectors
import shutil
import subprocess
import sys
import time


class TaskNetwork(Enum):
    NONE = "none"
    # Trusted first-party GitHub Actions only. The task shares host routing only
    # after the caller has verified the exact root-owned external egress-authority
    # marker. That marker binds a separately reviewed host firewall/gateway which
    # denies private/link-local egress; this helper does not create that authority.
    GITHUB_ACTIONS_TRUSTED_EGRESS = "github_actions_trusted_egress"


MAX_CONTROL_OUTPUT_BYTES = 64 * 1024
MAX_SOURCE_OUTPUT_BYTES = 1024 * 1024
FAILURE_TAIL_BYTES = 8 * 1024
MAX_MATERIALIZED_SOURCE_BYTES = 512 * 1024 * 1024
MAX_MATERIALIZED_SOURCE_ENTRIES = 100_000
CARGO_HOME_TMPFS_BYTES = 512 * 1024 * 1024
TEMP_TMPFS_BYTES = 512 * 1024 * 1024
PROJECT_HOME_TMPFS_BYTES = 64 * 1024 * 1024


class Refusal(RuntimeError):
    pass


def closed_environment(extra: dict[str, str] | None = None) -> dict[str, str]:
    environment = {
        "GIT_CONFIG_GLOBAL": "/dev/null",
        "GIT_CONFIG_NOSYSTEM": "1",
        "LC_ALL": "C",
        "PATH": "/usr/bin:/bin",
    }
    if extra:
        environment.update(extra)
    return environment


def run_control(argv: list[str], cwd: Path | None = None) -> bytes:
    completed = subprocess.run(
        argv,
        cwd=cwd,
        env=closed_environment(),
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        timeout=60,
        check=False,
    )
    if len(completed.stdout) > MAX_CONTROL_OUTPUT_BYTES:
        raise Refusal("control command output exceeded its fixed ceiling")
    if completed.returncode != 0:
        raise Refusal("exact source materialization or observation failed")
    return completed.stdout


def remove_task(task_root: Path) -> None:
    try:
        shutil.rmtree(task_root)
    except FileNotFoundError:
        return
    if task_root.exists() or task_root.is_symlink():
        raise Refusal("task-private source/build state cleanup is incomplete")


def materialize(repository_root: Path, task_root: Path, commit: str, tree: str) -> Path:
    source = task_root / "source"
    source.mkdir(mode=0o700)
    template = task_root / "git-template"
    template.mkdir(mode=0o700)
    environment = closed_environment({"GIT_TEMPLATE_DIR": os.fspath(template)})
    commands = (
        ["/usr/bin/git", "init", "--quiet", os.fspath(source)],
        [
            "/usr/bin/git",
            "-c",
            "protocol.file.allow=always",
            "fetch",
            "--quiet",
            "--no-tags",
            "--depth=1",
            os.fspath(repository_root),
            commit,
        ],
        ["/usr/bin/git", "checkout", "--quiet", "--detach", commit],
    )
    for index, command in enumerate(commands):
        cwd = None if index == 0 else source
        completed = subprocess.run(
            command,
            cwd=cwd,
            env=environment,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            timeout=60,
            check=False,
        )
        if completed.returncode != 0 or len(completed.stdout) > MAX_CONTROL_OUTPUT_BYTES:
            raise Refusal("exact task-private source materialization failed")
    try:
        (source / ".git" / "FETCH_HEAD").unlink()
    except FileNotFoundError:
        pass
    observed = run_control(
        ["/usr/bin/git", "rev-parse", "HEAD", "HEAD^{tree}"], source
    ).decode("ascii").splitlines()
    status = run_control(
        ["/usr/bin/git", "status", "--porcelain=v1", "-z", "--untracked-files=all"], source
    )
    if observed != [commit, tree] or status:
        raise Refusal("task-private source does not match the exact clean commit/tree")
    # Bubblewrap cannot create a child mountpoint beneath a read-only bind. Create
    # the ignored target mountpoint only after exact source admission; the tmpfs
    # mounted over it remains the sole writable build surface seen by the recipe.
    (source / "target").mkdir(mode=0o700)
    entries = 0
    bytes_seen = 0
    for parent, directories, files in os.walk(source, followlinks=False):
        entries += len(directories) + len(files)
        if entries > MAX_MATERIALIZED_SOURCE_ENTRIES:
            raise Refusal("task-private source exceeds the entry ceiling")
        for name in files:
            bytes_seen += (Path(parent) / name).lstat().st_size
            if bytes_seen > MAX_MATERIALIZED_SOURCE_BYTES:
                raise Refusal("task-private source exceeds the byte ceiling")
    return source


def public_crates_io_cache_arguments(cargo_root: Path) -> list[str]:
    """Expose public crates.io cache entries without unrelated Git/private-registry state."""
    arguments = ["--dir", "/cargo-home/registry"]
    for kind in ("cache", "index", "src"):
        arguments.extend(["--dir", f"/cargo-home/registry/{kind}"])
        parent = cargo_root / "registry" / kind
        if not parent.is_dir() or parent.is_symlink():
            continue
        for source in sorted(parent.glob("index.crates.io-*")):
            if source.is_dir() and not source.is_symlink() and source.parent == parent:
                arguments.extend(
                    [
                        "--ro-bind",
                        os.fspath(source),
                        f"/cargo-home/registry/{kind}/{source.name}",
                    ]
                )
    return arguments


def sandbox_command(
    source: Path, cargo_root: Path, rustup_root: Path, unit_name: str, *,
    systemd_properties: list[str], build_tmpfs_bytes: int,
    mount_arguments: list[str], recipe_arguments: list[str],
    network: TaskNetwork, source_read_only: bool = True,
) -> list[str]:
    if network is TaskNetwork.NONE:
        namespace_arguments = ["--unshare-all", "--unshare-user"]
        network_arguments: list[str] = []
    elif network is TaskNetwork.GITHUB_ACTIONS_TRUSTED_EGRESS:
        namespace_arguments = [
            "--unshare-user",
            "--unshare-pid",
            "--unshare-ipc",
            "--unshare-uts",
            "--unshare-cgroup",
        ]
        network_arguments = [
            "--ro-bind-try", "/etc/resolv.conf", "/etc/resolv.conf",
            "--ro-bind-try", "/etc/hosts", "/etc/hosts",
            "--ro-bind-try", "/etc/nsswitch.conf", "/etc/nsswitch.conf",
            "--ro-bind-try", "/etc/ssl", "/etc/ssl",
            "--ro-bind-try", "/etc/ca-certificates", "/etc/ca-certificates",
        ]
    else:
        raise Refusal("owned task network class is unsupported")
    source_bind = "--ro-bind" if source_read_only else "--bind"
    bubblewrap = [
        "/usr/bin/bwrap",
        *namespace_arguments,
        "--die-with-parent",
        "--new-session",
        "--cap-drop",
        "ALL",
        "--disable-userns",
        "--uid",
        "65534",
        "--gid",
        "65534",
        "--hostname",
        "glaeda-task",
        "--ro-bind",
        "/usr",
        "/usr",
        "--symlink",
        "usr/bin",
        "/bin",
        "--symlink",
        "usr/lib",
        "/lib",
        "--symlink",
        "usr/lib64",
        "/lib64",
        "--dir",
        "/etc",
        "--ro-bind-try",
        "/etc/ld.so.cache",
        "/etc/ld.so.cache",
        "--ro-bind-try",
        "/etc/alternatives",
        "/etc/alternatives",
        *network_arguments,
        "--proc",
        "/proc",
        "--dev",
        "/dev",
        "--size",
        str(TEMP_TMPFS_BYTES),
        "--tmpfs",
        "/tmp",
        "--dir",
        "/workspace",
        source_bind,
        os.fspath(source),
        "/workspace/source",
        "--size",
        str(build_tmpfs_bytes),
        "--tmpfs",
        "/workspace/source/target",
        "--size",
        str(CARGO_HOME_TMPFS_BYTES),
        "--tmpfs",
        "/cargo-home",
        "--dir",
        "/home",
        "--size",
        str(PROJECT_HOME_TMPFS_BYTES),
        "--tmpfs",
        "/home/project",
        "--ro-bind",
        os.fspath(cargo_root / "bin"),
        "/cargo/bin",
        "--ro-bind",
        os.fspath(rustup_root),
        "/rustup",
    ]
    bubblewrap.extend(mount_arguments)
    bubblewrap.extend(recipe_arguments)
    return [
        "/usr/bin/systemd-run",
        "--user",
        "--wait",
        "--pipe",
        "--quiet",
        "--collect",
        "--service-type=exec",
        f"--unit={unit_name}",
        *(f"--property={value}" for value in systemd_properties),
        *bubblewrap,
    ]


def unit_absent(unit_name: str) -> bool:
    completed = subprocess.run(
        ["/usr/bin/systemctl", "--user", "show", unit_name, "--property=LoadState", "--value"],
        env=closed_environment({"XDG_RUNTIME_DIR": f"/run/user/{os.getuid()}"}),
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        timeout=10,
        check=False,
    )
    return completed.returncode == 0 and completed.stdout.strip() in (b"", b"not-found")


def stop_unit(unit_name: str) -> None:
    subprocess.run(
        ["/usr/bin/systemctl", "--user", "stop", unit_name],
        env=closed_environment({"XDG_RUNTIME_DIR": f"/run/user/{os.getuid()}"}),
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        timeout=15,
        check=False,
    )


def execute(
    command: list[str], *, unit: str, deadline_seconds: int, label: str,
    launch_guard=None, inherit_stdin: bool = False, emit_failure_tail: bool = True,
) -> tuple[str, int, float, bool, int, str]:
    observation = _run_bounded(
        command, unit=unit, deadline_seconds=deadline_seconds, label=label,
        launch_guard=launch_guard, retain_limit=0, separate_stderr=False,
        inherit_stdin=inherit_stdin, emit_failure_tail=emit_failure_tail,
    )
    return observation[:6]


def execute_capturing(
    command: list[str], *, unit: str, deadline_seconds: int, label: str,
    max_bytes: int, launch_guard=None,
) -> tuple[str, int, float, bool, int, str, bytes, bool]:
    """Bounded execution that retains up to max_bytes of stdout.

    Returns the exact execute() observation plus the retained prefix and
    an overflow flag. Retention is evidence transport only: bytes beyond
    max_bytes still feed the digest and the byte count, and overflow is
    reported rather than truncated silently. A non-positive max_bytes
    refuses; retention never widens the output ceiling. Stderr stays
    merged into the observed stream, so this suits human-output commands,
    not machine receipt channels (see execute_capturing_split).
    """
    if type(max_bytes) is not int or max_bytes <= 0:
        raise Refusal("capture ceiling must be a positive byte count")
    return _run_bounded(
        command, unit=unit, deadline_seconds=deadline_seconds, label=label,
        launch_guard=launch_guard, retain_limit=max_bytes, separate_stderr=False,
        inherit_stdin=False, emit_failure_tail=True,
    )[:8]


def execute_capturing_split(
    command: list[str], *, unit: str, deadline_seconds: int, label: str,
    max_bytes: int, launch_guard=None,
) -> tuple[str, int, float, bool, int, str, bytes, bool, int, str]:
    """Bounded execution with a pure-stdout receipt channel.

    Stdout is retained (up to max_bytes), digested, and counted exactly
    like execute_capturing. Stderr travels on its own pipe into a private
    digest and byte count; it never enters the retained channel. Failure
    tails report both streams distinctly. Returns the capturing tuple
    plus (stderr_bytes, stderr_digest).
    """
    if type(max_bytes) is not int or max_bytes <= 0:
        raise Refusal("capture ceiling must be a positive byte count")
    observation = _run_bounded(
        command, unit=unit, deadline_seconds=deadline_seconds, label=label,
        launch_guard=launch_guard, retain_limit=max_bytes, separate_stderr=True,
        inherit_stdin=False, emit_failure_tail=True,
    )
    return observation[:8] + observation[8:10]


def _run_bounded(
    command: list[str], *, unit: str, deadline_seconds: int, label: str,
    launch_guard=None, retain_limit: int, separate_stderr: bool,
    inherit_stdin: bool, emit_failure_tail: bool,
) -> tuple[str, int, float, bool, int, str, bytes, bool, int, str]:
    started = time.monotonic()
    with launch_guard() if launch_guard is not None else nullcontext():
        process = subprocess.Popen(
            command,
            env=closed_environment({"XDG_RUNTIME_DIR": f"/run/user/{os.getuid()}"}),
            stdin=None if inherit_stdin else subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE if separate_stderr else subprocess.STDOUT,
            start_new_session=True,
        )
    assert process.stdout is not None
    selector = selectors.DefaultSelector()
    selector.register(process.stdout, selectors.EVENT_READ)
    if separate_stderr:
        assert process.stderr is not None
        selector.register(process.stderr, selectors.EVENT_READ)
    digest = hashlib.sha256()
    output_bytes = 0
    retained = bytearray()
    overflow = False
    tail = bytearray()
    output_exceeded = False
    err_digest = hashlib.sha256()
    err_bytes = 0
    err_tail = bytearray()
    err_exceeded = False
    forced_timeout = False
    deadline = started + deadline_seconds + 30
    stdout_fd = process.stdout.fileno()
    while True:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            forced_timeout = True
            stop_unit(unit)
            try:
                os.killpg(process.pid, 9)
            except ProcessLookupError:
                pass
            break
        events = selector.select(min(0.1, remaining))
        for key, _ in events:
            chunk = os.read(key.fd, 64 * 1024)
            if not chunk:
                selector.unregister(key.fileobj)
                continue
            if separate_stderr and key.fd != stdout_fd:
                err_digest.update(chunk)
                err_bytes += len(chunk)
                err_tail.extend(chunk)
                if len(err_tail) > FAILURE_TAIL_BYTES:
                    del err_tail[:-FAILURE_TAIL_BYTES]
                if err_bytes > MAX_SOURCE_OUTPUT_BYTES and not err_exceeded:
                    err_exceeded = True
                    stop_unit(unit)
                continue
            digest.update(chunk)
            output_bytes += len(chunk)
            if len(retained) < retain_limit:
                retained.extend(chunk[: retain_limit - len(retained)])
            if output_bytes > retain_limit:
                overflow = True
            tail.extend(chunk)
            if len(tail) > FAILURE_TAIL_BYTES:
                del tail[:-FAILURE_TAIL_BYTES]
            if output_bytes > MAX_SOURCE_OUTPUT_BYTES and not output_exceeded:
                output_exceeded = True
                stop_unit(unit)
        if process.poll() is not None and not selector.get_map():
            break
    try:
        returncode = process.wait(timeout=15)
    except subprocess.TimeoutExpired:
        stop_unit(unit)
        try:
            os.killpg(process.pid, 9)
        except ProcessLookupError:
            pass
        returncode = process.wait()
    selector.close()
    process.stdout.close()
    if separate_stderr and process.stderr is not None:
        process.stderr.close()
    elapsed = time.monotonic() - started
    settled = unit_absent(unit)
    if not settled:
        stop_unit(unit)
        settled = unit_absent(unit)
    terminal = "succeeded" if returncode == 0 else "failed"
    if forced_timeout or elapsed >= deadline_seconds:
        terminal = "timed_out"
    if output_exceeded or err_exceeded:
        terminal = "failed"
    if not settled:
        terminal = "cleanup_incomplete"
    if emit_failure_tail and terminal != "succeeded" and (tail or err_tail):
        if tail:
            omitted = output_bytes - len(tail)
            print(
                f"{label} failure output: tail_bytes={len(tail)} omitted_bytes={omitted}",
                file=sys.stderr,
            )
            sys.stderr.flush()
            sys.stderr.buffer.write(bytes(tail))
            if not tail.endswith(b"\n"):
                sys.stderr.buffer.write(b"\n")
            sys.stderr.buffer.flush()
        if err_tail:
            omitted = err_bytes - len(err_tail)
            print(
                f"{label} failure stderr: tail_bytes={len(err_tail)}"
                f" omitted_bytes={omitted}",
                file=sys.stderr,
            )
            sys.stderr.flush()
            sys.stderr.buffer.write(bytes(err_tail))
            if not err_tail.endswith(b"\n"):
                sys.stderr.buffer.write(b"\n")
            sys.stderr.buffer.flush()
    return (
        terminal, returncode, elapsed, settled, output_bytes,
        f"sha256:{digest.hexdigest()}", bytes(retained), overflow,
        err_bytes, f"sha256:{err_digest.hexdigest()}",
    )


def prepare_task(task_root: Path) -> None:
    remove_task(task_root)
    task_root.mkdir(mode=0o700)
