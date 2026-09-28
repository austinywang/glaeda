"""Bounded, read-only observations for the Glaeda workspace receipt."""

from __future__ import annotations

import os
import re
import shutil
import stat
import subprocess
import sys
from pathlib import Path
from urllib.parse import urlparse

EXPECTED_REPOSITORY = "teamleaderleo/glaeda"
PROFILE_NAMES = ["glaeda.required", "glaeda.doctor", "glaeda.plan"]
VERSION_RE = re.compile(r"\b\d+\.\d+(?:\.\d+)?(?:[-+][A-Za-z0-9.-]+)?\b")
SHA_RE = re.compile(r"^[0-9a-f]{40}$")
REQUIRED_COMMANDS = [
    ("git", ["git", "--version"]),
    ("cargo", ["cargo", "--version"]),
    ("rustc", ["rustc", "--version"]),
    ("rustfmt", ["rustfmt", "--version"]),
    ("clippy", ["cargo-clippy", "--version"]),
]
OPTIONAL_COMMANDS = [
    ("cargo-nextest", ["cargo-nextest", "--version"]),
    ("just", ["just", "--version"]),
    ("podman", ["podman", "--version"]),
]


def public_issue(code: str, message: str) -> dict[str, str]:
    return {"code": code, "message": message}


def child_environment() -> dict[str, str]:
    result = {
        "PATH": os.environ.get("PATH", "/usr/local/bin:/usr/bin:/bin"),
        "LANG": "C",
        "LC_ALL": "C",
        "GIT_TERMINAL_PROMPT": "0",
        "GIT_OPTIONAL_LOCKS": "0",
    }
    for key in ("HOME", "CARGO_HOME", "RUSTUP_HOME", "TMPDIR"):
        value = os.environ.get(key)
        if value:
            result[key] = value
    return result


def run(command: list[str], cwd: Path) -> subprocess.CompletedProcess[str] | None:
    environment = child_environment()
    program = shutil.which(command[0], path=environment["PATH"])
    if program is None:
        return None
    absolute_program = os.path.abspath(program)
    try:
        return subprocess.run(
            [absolute_program, *command[1:]],
            cwd=cwd,
            env=environment,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=8,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None


def successful_output(command: list[str], cwd: Path) -> str | None:
    result = run(command, cwd)
    if result is None or result.returncode != 0:
        return None
    return result.stdout.strip()


def repository_root(cwd: Path) -> Path | None:
    value = successful_output(["git", "rev-parse", "--show-toplevel"], cwd)
    if not value:
        return None
    try:
        return Path(value).resolve(strict=True)
    except OSError:
        return None


def tool_observation(
    name: str, command: list[str], required: bool, root: Path
) -> dict[str, object]:
    result = run(command, root)
    available = result is not None and result.returncode == 0
    version = None
    if available and result is not None:
        bounded = (result.stdout + "\n" + result.stderr)[:256]
        match = VERSION_RE.search(bounded)
        version = match.group(0)[:32] if match else None
    return {
        "name": name,
        "required": required,
        "available": available,
        "version": version,
    }


def python_observation() -> dict[str, object]:
    return {
        "name": "python3",
        "required": True,
        "available": True,
        "version": ".".join(str(part) for part in sys.version_info[:3]),
    }


def clean_checkout(root: Path) -> bool | None:
    result = run(["git", "status", "--porcelain=v1", "--untracked-files=all"], root)
    if result is None or result.returncode != 0:
        return None
    return result.stdout == ""


def git_sha(root: Path, expression: str) -> str | None:
    value = successful_output(["git", "rev-parse", "--verify", expression], root)
    if value and SHA_RE.fullmatch(value.lower()):
        return value.lower()
    return None


def public_repository_identity(root: Path) -> str | None:
    raw = successful_output(["git", "config", "--get", "remote.origin.url"], root)
    if not raw:
        return None
    candidate = None
    if raw.startswith("git@github.com:"):
        candidate = raw.removeprefix("git@github.com:")
    else:
        parsed = urlparse(raw)
        if (parsed.hostname or "").lower() == "github.com":
            candidate = parsed.path.lstrip("/")
    if candidate and candidate.endswith(".git"):
        candidate = candidate[:-4]
    parts = candidate.split("/") if candidate else []
    if len(parts) != 2:
        return None
    if any(re.fullmatch(r"[A-Za-z0-9_.-]+", part) is None for part in parts):
        return None
    return "/".join(parts)


def package_marker_matches(root: Path) -> bool:
    try:
        text = (root / "Cargo.toml").read_text(encoding="utf-8")
    except (OSError, UnicodeError):
        return False
    package = re.search(r"(?ms)^\[package\]\s*(.*?)(?=^\[|\Z)", text)
    return bool(
        package
        and re.search(
            r'(?m)^\s*name\s*=\s*"(?:glaeda|smolrunner)"\s*$', package.group(1)
        )
    )


def available_memory_and_swap() -> tuple[int | None, int | None]:
    path = Path("/proc/meminfo")
    if not path.is_file():
        return None, None
    values: dict[str, int] = {}
    try:
        for line in path.read_text(encoding="ascii").splitlines():
            key, _, tail = line.partition(":")
            token = tail.strip().split()[0] if tail.strip() else ""
            if token.isdigit():
                values[key] = int(token)
    except (OSError, UnicodeError):
        return None, None
    memory = values.get("MemAvailable")
    swap = values.get("SwapFree")
    return (
        memory // 1024 if memory is not None else None,
        swap // 1024 if swap is not None else None,
    )


def recommended_concurrency(available_memory_mib: int | None, cpus: int) -> int:
    if available_memory_mib is None:
        return 1
    memory_jobs = max(1, available_memory_mib // 2048)
    return max(1, min(cpus, memory_jobs, 4))


def _contains_parent_reference(value: str) -> bool:
    return any(part == ".." for part in Path(value).parts)


def _lexical_absolute(value: str, base: Path) -> Path:
    path = Path(value)
    if not path.is_absolute():
        path = base / path
    return Path(os.path.abspath(os.fspath(path)))


def _is_within(path: Path, root: Path) -> bool:
    try:
        path.relative_to(root)
        return True
    except ValueError:
        return False


def _has_symlink_alias(path: Path) -> tuple[bool, bool]:
    """Return (alias_found, observation_failed) without resolving the path."""
    current = Path(path.anchor)
    for part in path.parts[1:]:
        current /= part
        try:
            metadata = os.lstat(current)
        except FileNotFoundError:
            return False, False
        except OSError:
            return False, True
        if stat.S_ISLNK(metadata.st_mode):
            return True, False
    return False, False


def observe_cache_path(
    *,
    name: str,
    configured_value: str | None,
    default_value: str | None,
    configured_base: Path,
    root: Path,
    default_base_kind: str,
    configured_public_path: str,
    default_public_path: str,
    expectation: str,
    expected_uid: int | None = None,
) -> dict[str, object]:
    """Classify one cache without returning its resolved filesystem path."""
    source = "environment" if configured_value is not None else "default"
    raw = configured_value if configured_value is not None else default_value
    base_kind = (
        "absolute"
        if configured_value is not None and Path(configured_value).is_absolute()
        else "repository-root"
        if configured_value is not None
        else default_base_kind
    )
    public_path = configured_public_path if configured_value is not None else default_public_path
    observation: dict[str, object] = {
        "name": name,
        "source": source,
        "base": base_kind,
        "public_path": public_path,
        "path_exposed": False,
        "path_class": "unsafe",
        "intended_path_class": None,
        "exists": False,
        "directory": None,
        "ownership_observed": False,
        "ownership": "unestablished",
        "parent_escape_detected": False,
        "symlink_alias_detected": False,
        "expectation": expectation,
    }
    if raw is None or raw == "":
        return observation
    if _contains_parent_reference(raw):
        observation["parent_escape_detected"] = True
        return observation

    candidate = _lexical_absolute(raw, configured_base)
    intended = "repository-local" if _is_within(candidate, root) else "external-private"
    observation["intended_path_class"] = intended
    if configured_value is not None and intended == "repository-local":
        observation["public_path"] = "<repository-root>/<configured-cache>"
    if candidate == root:
        return observation

    symlink_alias, symlink_observation_failed = _has_symlink_alias(candidate)
    observation["symlink_alias_detected"] = symlink_alias
    if symlink_alias or symlink_observation_failed:
        return observation

    try:
        metadata = os.lstat(candidate)
    except FileNotFoundError:
        observation["path_class"] = "missing"
        return observation
    except OSError:
        return observation

    observation["exists"] = True
    is_directory = stat.S_ISDIR(metadata.st_mode)
    observation["directory"] = is_directory
    if not is_directory:
        return observation

    owner = getattr(metadata, "st_uid", None)
    if expected_uid is None:
        getuid = getattr(os, "geteuid", None)
        expected_uid = getuid() if getuid is not None else None
    if owner is None or expected_uid is None:
        observation["path_class"] = intended
        return observation

    observation["ownership_observed"] = True
    if owner != expected_uid:
        observation["ownership"] = "different-user"
        return observation

    observation["ownership"] = "current-user"
    observation["path_class"] = intended
    return observation


def cache_declarations(root: Path) -> list[dict[str, object]]:
    target_configured = os.environ.get("CARGO_TARGET_DIR") if "CARGO_TARGET_DIR" in os.environ else None
    cargo_home_configured = os.environ.get("CARGO_HOME") if "CARGO_HOME" in os.environ else None
    home = os.environ.get("HOME")
    cargo_home_default = None
    if home and Path(home).is_absolute():
        cargo_home_default = os.fspath(Path(home) / ".cargo")

    return [
        observe_cache_path(
            name="cargo-target",
            configured_value=target_configured,
            default_value="target",
            configured_base=root,
            root=root,
            default_base_kind="repository-root",
            configured_public_path=(
                "<private-external-cache>/cargo-target"
                if target_configured and Path(target_configured).is_absolute()
                else "<repository-root>/<configured-cache>"
            ),
            default_public_path="<repository-root>/target",
            expectation="exclusive-writer-per-build",
        ),
        observe_cache_path(
            name="cargo-home",
            configured_value=cargo_home_configured,
            default_value=cargo_home_default,
            configured_base=root,
            root=root,
            default_base_kind="home-directory",
            configured_public_path=(
                "<private-external-cache>/cargo-home"
                if cargo_home_configured and Path(cargo_home_configured).is_absolute()
                else "<repository-root>/<configured-cache>"
            ),
            default_public_path="<private-user-cache>/cargo",
            expectation="shared-read-write-package-cache",
        ),
    ]


def cache_observation_issues(
    observations: list[dict[str, object]],
) -> tuple[list[dict[str, str]], list[dict[str, str]]]:
    deviations: list[dict[str, str]] = []
    blocking: list[dict[str, str]] = []
    for item in observations:
        name = str(item["name"]).replace("-", "_")
        if item["parent_escape_detected"]:
            blocking.append(
                public_issue(
                    f"cache_{name}_parent_escape",
                    f"declared {item['name']} cache uses a rejected parent path",
                )
            )
            continue
        if item["symlink_alias_detected"]:
            blocking.append(
                public_issue(
                    f"cache_{name}_symlink_alias",
                    f"declared {item['name']} cache resolves through a symbolic link",
                )
            )
            continue
        if item["path_class"] == "missing":
            deviations.append(
                public_issue(
                    f"cache_{name}_missing",
                    f"declared {item['name']} cache directory is missing and ownership is unestablished",
                )
            )
            continue
        if item["path_class"] == "unsafe":
            if item["exists"] and item["directory"] is False:
                code = f"cache_{name}_not_directory"
                message = f"declared {item['name']} cache exists but is not a directory"
            elif item["ownership"] == "different-user":
                code = f"cache_{name}_wrong_owner"
                message = f"declared {item['name']} cache is owned by a different user"
            else:
                code = f"cache_{name}_unsafe"
                message = f"declared {item['name']} cache path could not be classified safely"
            blocking.append(public_issue(code, message))
            continue
        if not item["ownership_observed"]:
            blocking.append(
                public_issue(
                    f"cache_{name}_ownership_unestablished",
                    f"declared {item['name']} cache exists but ownership could not be established",
                )
            )
    return deviations, blocking


def git_identity(root: Path, evaluate: bool) -> dict[str, object]:
    if not evaluate:
        return {
            "evaluated": False,
            "ready": None,
            "name_configured": None,
            "email_configured": None,
        }
    name = successful_output(["git", "config", "--get", "user.name"], root)
    email = successful_output(["git", "config", "--get", "user.email"], root)
    return {
        "evaluated": True,
        "ready": bool(name and email),
        "name_configured": bool(name),
        "email_configured": bool(email),
    }


def publication_readiness(
    operation: str, repository: str | None, identity: dict[str, object]
) -> dict[str, object]:
    if operation != "publish":
        return {
            "evaluated": False,
            "ready": None,
            "remote_configured": None,
            "authorization": "not-requested",
        }
    return {
        "evaluated": True,
        "ready": False,
        "remote_configured": repository is not None,
        "authorization": "unprobed",
        "local_prerequisites_ready": bool(repository and identity["ready"]),
    }
