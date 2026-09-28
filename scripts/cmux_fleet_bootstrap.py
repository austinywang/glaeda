#!/usr/bin/env python3
"""Read-only CMUX fleet bootstrap observation for macOS and Linux nodes."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import re
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Any

SCHEMA = "glaeda-cmux-fleet-bootstrap/v1"
MAX_OUTPUT_BYTES = 16 * 1024
MAX_COMMAND_OUTPUT_BYTES = 16 * 1024
ENROLLABLE_ROLES = {
    "cmux_macos_native_build",
    "cmux_linux_ci",
}
CMUX_REPOSITORY = "manaflow-ai/cmux"
# The fixed part of the PATH the CMUX profile runner hands its workload, from
# `workload_environment` in the repository's scripts/ci/cmux_workload_profile.py.
# The runner prepends a per-attempt Cargo home that it creates empty, so these
# six directories are everything a build can actually reach.
CMUX_WORKLOAD_TOOL_PATH = "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
# Tools the CMUX developer build itself invokes, as opposed to the ones this
# script runs to describe the machine.
MACOS_WORKLOAD_TOOLS = ("cargo", "git", "rustc", "rustup", "xcodebuild", "xcrun", "zig")
LINUX_WORKLOAD_TOOLS = ("git", "python3")
# Free disk a macOS build host needs before admission: a base so a macOS update can download
# and install, plus one cmux working set per concurrent build slot. A working set is at most
# 15 GiB DerivedData for the app and test products (8.7 GiB was the largest app-only one
# measured on Air Blue), the 3 GiB compilation-cache cap CI uses, about 3 GiB of packages and
# 4 GiB of checkout or worktree. glaeda-disk keeps it there once admitted.
MACOS_BASE_FREE_GIB = 25
MACOS_SLOT_FREE_GIB = 25
LINUX_MIN_FREE_GIB = 40


def default_min_free_gib(platform_name: str, build_slots: int = 1) -> int:
    if build_slots < 1:
        raise ValueError("build slots must be at least 1")
    if platform_name == "macos":
        return MACOS_BASE_FREE_GIB + MACOS_SLOT_FREE_GIB * build_slots
    return LINUX_MIN_FREE_GIB
# CMUX publishes GhosttyKit at the repository root when setup takes the
# prebuilt archive, and under the Ghostty submodule when it builds from source.
CMUX_GHOSTTYKIT_LOCATIONS = (
    "GhosttyKit.xcframework",
    "ghostty/macos/GhosttyKit.xcframework",
)
# The Xcode major the fleet is reviewed against (CMUX .xcode-version).
REVIEWED_XCODE_MAJOR = 26
CMUX_RESULT_CONTRACT = "cmux-workload-result/v1"
CMUX_PROFILE_REGISTRY = "scripts/ci/cmux-workload-profiles.json"
MAX_PROFILE_REGISTRY_BYTES = 64 * 1024
ROLE_PROFILES = {
    "cmux_linux_ci": {"id": "cmux.ci.guard", "generation": 1},
    "cmux_macos_native_build": {"id": "cmux.macos.dev-check", "generation": 1},
}
ROLE_OS = {
    "cmux_macos_native_build": "macos",
    "cmux_macos_test": "macos",
    "cmux_linux_ci": "linux",
    "cmux_linux_agent": "linux",
    "artifact_cache": None,
    "background_replay": None,
    "benchmark": None,
    "diagnostic": None,
}


class BootstrapError(RuntimeError):
    pass


# How to install each workload tool the build looks up on CMUX_WORKLOAD_TOOL_PATH.
# Homebrew belongs to one account on a shared mini; installing as anyone else
# leaves its owner with files it cannot update, so the fix names the owner.
TOOL_FIXES = {
    "cargo": "brew install rustup as the Homebrew owner, then link its cargo, rustc and rustup proxies into /opt/homebrew/bin",
    "rustc": "brew install rustup as the Homebrew owner, then link its cargo, rustc and rustup proxies into /opt/homebrew/bin",
    "rustup": "brew install rustup as the Homebrew owner, then link its cargo, rustc and rustup proxies into /opt/homebrew/bin",
    "zig": "brew install zig as the Homebrew owner (Ghostty needs the version in ghostty/build.zig.zon)",
    "git": "xcode-select --install, or select an Xcode with sudo xcode-select -s",
    "xcodebuild": "install the pinned Xcode and select it with sudo xcode-select -s",
    "xcrun": "install the pinned Xcode and select it with sudo xcode-select -s",
    "python3": "install python3 3.13 or newer",
}
METAL_FIX = "Metal Toolchain missing; run xcodebuild -downloadComponent MetalToolchain"
SUBMODULE_FIX = "submodules not initialized; run git submodule update --init --recursive --depth 1 in the cmux checkout"
FIRST_LAUNCH_FIX = "Xcode first launch not done (plugins fail to load); run sudo xcodebuild -runFirstLaunch"
LICENSE_FIX = "Xcode licence not accepted; run sudo xcodebuild -license accept"
# The fix for each check `evaluate` can list in blockingChecks, so a refusal says what to do.
BLOCKING_FIXES = {
    "supportedOs": "use macOS 15 or 26 (Linux: Ubuntu 24.04 or Debian 12 on kernel 6+)",
    "hardwareCapability": "use a host that meets the hardware class minimum (8 CPUs, 16 GiB on macOS)",
    "cmuxCheckout": "clone manaflow-ai/cmux (git clone --depth 1) and pass it as --cmux-root",
    "canonicalCheckoutClean": "commit, stash or remove local changes in the cmux checkout (git status)",
    "submodulesReady": SUBMODULE_FIX,
    "cmuxSetupArtifacts": "run ./scripts/setup.sh in the cmux checkout (it needs the Metal toolchain and zig first)",
    "xcodePin": "select an Xcode of the pinned major: sudo xcode-select -s /Applications/Xcode_<pin>.app",
    "git": TOOL_FIXES["git"],
    "profileRunnerInterpreter": "run the bootstrap with Python 3.13 or newer (os.waitid)",
    "workloadToolPath": "put every build tool in /opt/homebrew/bin or /usr/local/bin (see toolsMissingFromWorkloadPath)",
    "zig": "install the zig Ghostty needs: " + TOOL_FIXES["zig"],
    "rust": "rustup, cargo and rustc must run in the cmux checkout: rustup toolchain install <channel in Native/DiffSidecar/rust-toolchain.toml>",
    "metalToolchain": METAL_FIX,
    "glaedaExecutable": "install or stage the glaeda binary (glaeda-mini-enroll does this)",
    "diskAdmission": "free disk space (glaeda-disk shows where it went)",
    "nativeCacheRoot": "create the native cache root (glaeda-mini-setup --apply)",
    "nativeCacheDiskAdmission": "free disk space on the native cache volume (glaeda-disk)",
    "unattendedPower": "turn off sleep on AC power: sudo pmset -c sleep 0",
    "systemd": "run on a systemd host",
    "bubblewrap": "install bubblewrap",
    "cgroupV2": "boot with the unified cgroup v2 hierarchy",
    "pressureSignals": "enable PSI (/proc/pressure)",
    "memoryAdmission": "free memory: 8 GiB must be available",
    "actionsPrerequisites": "install curl, tar, gzip and ldd",
}


def diagnose_command(argv: list[str], output: str) -> str | None:
    """Name the missing thing behind a failed probe command, and its fix."""
    text = output.lower()
    if "dvtplugin" in text or "runfirstlaunch" in text or "dvtdownloads" in text:
        return FIRST_LAUNCH_FIX
    if "license" in text and ("agree" in text or "accept" in text):
        return LICENSE_FIX
    if "command line tools instance" in text or "xcode-select: error" in text:
        return TOOL_FIXES["xcodebuild"]
    if Path(argv[0]).name == "xcrun" and "metal" in argv[1:]:
        return METAL_FIX
    match = re.search(r"toolchain '([^']+)' is not installed", output)
    if match:
        return f"Rust toolchain {match.group(1)} is not installed; run rustup toolchain install {match.group(1)}"
    return None


def explain_error(message: str) -> str:
    """Turn a bootstrap error, including one from an older candidate, into what to fix."""
    if "build.zig.zon" in message and ("Errno 2" in message or "No such file" in message):
        return SUBMODULE_FIX
    if message == "required command failed: xcrun":
        return ("xcrun failed: usually " + METAL_FIX + " (check with xcrun metal --version); "
                "otherwise the selected Xcode is missing or needs first launch")
    missing = re.fullmatch(r"required command is missing: (\S+)", message)
    if missing and missing.group(1) in TOOL_FIXES:
        return f"{missing.group(1)} is not installed: {TOOL_FIXES[missing.group(1)]}"
    return message


def canonical(value: object) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":"), allow_nan=False) + "\n").encode()


def digest_bytes(value: bytes) -> str:
    return "sha256:" + hashlib.sha256(value).hexdigest()


def digest_file(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            h.update(chunk)
    return "sha256:" + h.hexdigest()


def run(
    argv: list[str],
    timeout: int = 10,
    cwd: Path | None = None,
) -> str:
    environment = {
        "LC_ALL": "C",
        "PATH": os.environ.get("PATH", "/usr/bin:/bin:/usr/sbin:/sbin"),
    }
    for name in ("HOME", "CARGO_HOME", "RUSTUP_HOME", "DEVELOPER_DIR"):
        value = os.environ.get(name)
        if value:
            environment[name] = value
    result = subprocess.run(
        argv,
        check=False,
        cwd=cwd,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        env=environment,
        timeout=timeout,
        text=True,
    )
    if len(result.stdout.encode("utf-8", errors="replace")) > MAX_COMMAND_OUTPUT_BYTES:
        raise BootstrapError(
            f"required command output is too large: {Path(argv[0]).name}"
        )
    if result.returncode != 0:
        command = " ".join([Path(argv[0]).name, *argv[1:3]])
        tail = result.stdout.strip().splitlines()
        hint = diagnose_command(argv, result.stdout)
        detail = hint or (f"exit {result.returncode}: {tail[-1][:200]}" if tail else f"exit {result.returncode}")
        raise BootstrapError(f"{command} failed: {detail}")
    # Column zero carries meaning for callers such as `git submodule status`,
    # whose leading space marks a checked-out submodule. Trim the trailing
    # newline and nothing else: callers that fullmatch this output are matching
    # a command's exact bytes, not a normalised form.
    return result.stdout.rstrip("\n")


def executable(name: str) -> str:
    value = shutil.which(name)
    if value is None:
        fix = TOOL_FIXES.get(name)
        raise BootstrapError(f"required command is missing: {name}" + (f"; {fix}" if fix else ""))
    return os.path.abspath(value)


def missing_workload_tools(names: tuple[str, ...]) -> list[str]:
    """Name the build tools the CMUX workload will not be able to find.

    Resolving a tool from the operator's shell says nothing about the build:
    the runner rebuilds PATH from ``CMUX_WORKLOAD_TOOL_PATH`` plus a Cargo home
    that starts empty. A tool installed under the operator's home therefore
    passes every probe here and fails the build minutes later, which is how the
    first fleet canary lost 670 seconds to a Zig it could see. Report the
    difference as an observation so the receipt still lists everything else
    wrong with the node.
    """
    return sorted(
        name
        for name in names
        if shutil.which(name, path=CMUX_WORKLOAD_TOOL_PATH) is None
    )


def profile_runner_interpreter_ready() -> bool:
    """Report whether this interpreter can run CMUX's profile runner.

    The runner waits on its child with `os.waitid` to keep the child's PID and
    process group unreleased, and CPython exposes that call on macOS only from
    3.13. An older interpreter fails a fraction of a second into acceptance,
    well after bootstrap has already called the node ready, so ask the
    interpreter for the capability rather than compare version numbers.
    """
    return hasattr(os, "waitid")


def normalize_arch(value: str) -> str:
    if value in {"arm64", "aarch64"}:
        return "arm64"
    if value == "x86_64":
        return value
    raise BootstrapError(f"unsupported architecture: {value}")


def disk_free_gib(path: Path) -> int:
    return shutil.disk_usage(path).free // (1024**3)


def role_profiles(
    cmux_root: Path,
    roles: list[str],
    platform_name: str,
    architecture: str,
) -> dict[str, dict[str, object]]:
    registry_path = cmux_root / CMUX_PROFILE_REGISTRY
    try:
        raw = registry_path.read_bytes()
    except OSError as error:
        raise BootstrapError("CMUX workload profile registry is unavailable") from error
    if len(raw) > MAX_PROFILE_REGISTRY_BYTES:
        raise BootstrapError("CMUX workload profile registry exceeds its ceiling")
    try:
        registry = json.loads(raw)
    except (UnicodeError, ValueError) as error:
        raise BootstrapError("CMUX workload profile registry is invalid JSON") from error
    if (
        not isinstance(registry, dict)
        or registry.get("schema_version") != 1
        or registry.get("repository") != CMUX_REPOSITORY
        or registry.get("result_contract") != CMUX_RESULT_CONTRACT
        or not isinstance(registry.get("profiles"), list)
    ):
        raise BootstrapError("CMUX workload profile registry contract is unsupported")

    profiles_by_id: dict[str, list[dict[str, Any]]] = {}
    for item in registry["profiles"]:
        if not isinstance(item, dict) or not isinstance(item.get("id"), str):
            raise BootstrapError("CMUX workload profile registry contains an invalid profile")
        profiles_by_id.setdefault(item["id"], []).append(item)

    selected: dict[str, dict[str, object]] = {}
    for role in sorted(set(roles)):
        expected = ROLE_PROFILES.get(role)
        if expected is None:
            raise BootstrapError("bootstrap role lacks a reviewed v1 CMUX profile")
        matches = profiles_by_id.get(expected["id"], [])
        if len(matches) != 1:
            raise BootstrapError(f"CMUX profile for role {role} is unavailable")
        profile = matches[0]
        platform_doc = profile.get("platform")
        if (
            profile.get("generation") != expected["generation"]
            or not isinstance(platform_doc, dict)
            or platform_doc.get("os") != platform_name
            or not isinstance(platform_doc.get("architectures"), list)
            or architecture not in platform_doc["architectures"]
        ):
            raise BootstrapError(f"CMUX profile for role {role} is incompatible")
        selected[role] = dict(expected)
    return selected

def mac_sleep_disabled_on_ac(raw: str) -> bool:
    in_ac = False
    for line in raw.splitlines():
        stripped = line.strip()
        if stripped == "AC Power:":
            in_ac = True
            continue
        if stripped.endswith("Power:"):
            in_ac = False
            continue
        if in_ac:
            match = re.fullmatch(r"sleep\s+(\d+)", stripped)
            if match:
                return int(match.group(1)) == 0
    return False


def cmux_checkout_clean(root: Path) -> bool:
    return (
        run(
            [
                executable("git"),
                "status",
                "--porcelain=v1",
                "--untracked-files=all",
            ],
            cwd=root,
        )
        == ""
    )


def cmux_submodules_ready(root: Path) -> bool:
    output = run(
        [executable("git"), "submodule", "status", "--recursive"],
        cwd=root,
    )
    return submodule_status_ready(output)


def submodule_status_ready(output: str) -> bool:
    """`git submodule status` marks a checked-out submodule with a leading space;
    -, + and U mean uninitialized, off its recorded commit, or conflicted."""
    lines = [line for line in output.splitlines() if line]
    return bool(lines) and all(line[0] == " " for line in lines)


def cmux_setup_artifacts_present(root: Path) -> bool:
    """Report whether CMUX setup left the Ghostty artifacts this node needs.

    CMUX publishes GhosttyKit at the repository root when setup takes the
    prebuilt archive and under the Ghostty submodule when it builds from
    source, so naming one location refuses a correctly prepared checkout.
    """
    return (root / "ghostty/include/ghostty.h").is_file() and any(
        (root / relative).is_dir() for relative in CMUX_GHOSTTYKIT_LOCATIONS
    )


def xcode_pin_ready(pin: str, xcode_version: str | None, sdk_version: str) -> bool:
    """Whether the selected Xcode and SDK satisfy CMUX's .xcode-version.

    CMUX pins a major version there: "26" since cmux#14050, "26.0" before it.
    The exact app and build are the CI Xcode variables' job, which the hosted
    adopter revalidates; this check keeps a node on the reviewed major.
    """
    def major(version: str | None) -> int | None:
        match = re.fullmatch(r"(\d+)(?:\.\d+)*", version or "")
        return int(match.group(1)) if match else None

    wanted = major(pin)
    return (
        wanted == REVIEWED_XCODE_MAJOR
        and major(xcode_version) == wanted
        and major(sdk_version) == wanted
    )


def cmux_required_zig_version(root: Path) -> str:
    manifest = root / "ghostty/build.zig.zon"
    try:
        text = manifest.read_text(encoding="utf-8")
    except FileNotFoundError:
        raise BootstrapError(f"ghostty/build.zig.zon is missing: {SUBMODULE_FIX}") from None
    required = minimum_zig_version(text)
    if required is None:
        raise BootstrapError("Ghostty minimum Zig version is unavailable")
    return required


def minimum_zig_version(build_zig_zon: str) -> str | None:
    match = re.search(
        r'^\s*\.minimum_zig_version\s*=\s*"([0-9]+\.[0-9]+\.[0-9]+)"',
        build_zig_zon,
        re.MULTILINE,
    )
    return match.group(1) if match else None


def zig_version_compatible(actual: str, required: str) -> bool:
    def parse(value: str) -> tuple[int, int, int] | None:
        core = re.split(r"[-+]", value, maxsplit=1)[0]
        match = re.fullmatch(r"(\d+)\.(\d+)\.(\d+)", core)
        if match is None:
            return None
        return tuple(int(part) for part in match.groups())

    actual_parts = parse(actual)
    required_parts = parse(required)
    return bool(
        actual_parts
        and required_parts
        and actual_parts[:2] == required_parts[:2]
        and actual_parts[2] >= required_parts[2]
    )


def cmux_diff_rust_toolchain(root: Path) -> str:
    try:
        content = (root / "Native/DiffSidecar/rust-toolchain.toml").read_text(
            encoding="utf-8"
        )
    except FileNotFoundError:
        raise BootstrapError(
            "Native/DiffSidecar/rust-toolchain.toml is missing; update the cmux checkout"
        ) from None
    match = re.search(
        r'^\s*channel\s*=\s*"([^"]+)"',
        content,
        re.MULTILINE,
    )
    if match is None:
        raise BootstrapError("CMUX DiffSidecar Rust toolchain is unavailable")
    return match.group(1)


def collect_macos(
    cmux_root: Path,
    glaeda: Path,
    min_free_gib: int,
    hardware_class: str,
    cache_root: Path | None,
) -> dict[str, Any]:
    if platform.system() != "Darwin":
        raise BootstrapError("macOS bootstrap requires Darwin")
    version = platform.mac_ver()[0]
    match = re.fullmatch(r"(\d+)\.\d+(?:\.\d+)?", version)
    if match is None:
        raise BootstrapError("macOS version is unavailable")
    major = int(match.group(1))
    try:
        pin = (cmux_root / ".xcode-version").read_text(encoding="utf-8").strip()
    except FileNotFoundError:
        raise BootstrapError(
            f"{cmux_root} has no .xcode-version, so it is not a cmux checkout; "
            + BLOCKING_FIXES["cmuxCheckout"]
        ) from None
    xcode = run([executable("xcodebuild"), "-version"])
    xcrun = executable("xcrun")
    sdk = run([xcrun, "--sdk", "macosx", "--show-sdk-version"])
    # Only the version line: the rest names the per-machine cryptex mount the
    # Metal toolchain asset landed in, which differs between identical hosts.
    metal = run([xcrun, "metal", "--version"]).strip().splitlines()[:1]
    metal = metal[0] if metal else ""
    git = run([executable("git"), "--version"])
    zig = run([executable("zig"), "version"])
    zig_required = cmux_required_zig_version(cmux_root)
    rustup = executable("rustup")
    # rustup resolves the active toolchain from the working directory's
    # rust-toolchain.toml, so observe it where the CMUX build runs. From any
    # other directory (a Glaeda checkout pins its own Rust) the toolchain
    # generation differs between enrollment and acceptance.
    rustup_version = run([rustup, "--version"], cwd=cmux_root)
    cargo = run([executable("cargo"), "--version"], cwd=cmux_root)
    rustc = run([executable("rustc"), "--version"], cwd=cmux_root)
    diff_rust = cmux_diff_rust_toolchain(cmux_root)
    diff_cargo = run([rustup, "run", diff_rust, "cargo", "--version"], cwd=cmux_root)
    diff_rustc = run([rustup, "run", diff_rust, "rustc", "--version"], cwd=cmux_root)
    pmset = run([executable("pmset"), "-g", "custom"])
    xcode_match = re.search(r"^Xcode\s+(\d+(?:\.\d+)*)$", xcode, re.MULTILINE)
    xcode_build = re.search(r"^Build version\s+([0-9A-Za-z]+)$", xcode, re.MULTILINE)
    sdk_match = re.fullmatch(r"(\d+)(?:\.\d+)*", sdk)
    toolchain = {
        "cmuxXcodePin": pin,
        "xcodeVersion": xcode_match.group(1) if xcode_match else "unknown",
        # Two builds share a version (a release candidate and the release), and
        # class acceptance is proved per Xcode build, so the build is identity.
        "xcodeBuild": xcode_build.group(1) if xcode_build else "unknown",
        "macosSdkVersion": sdk,
        "gitVersion": git,
        "metalVersion": metal,
        "zigVersion": zig,
        "zigRequired": zig_required,
        "rustupVersion": rustup_version,
        "cargoVersion": cargo,
        "rustcVersion": rustc,
        "diffRustToolchain": diff_rust,
        "diffCargoVersion": diff_cargo,
        "diffRustcVersion": diff_rustc,
    }
    free_gib = disk_free_gib(cmux_root)
    cache_required = cache_root is not None
    cache_ready = (
        cache_root is not None
        and cache_root.is_dir()
        and os.access(cache_root, os.W_OK | os.X_OK)
    )
    cache_free_gib = disk_free_gib(cache_root) if cache_ready and cache_root else 0
    cpus = os.cpu_count() or 0
    memory_gib = mac_total_memory_gib()
    hardware = mac_hardware(memory_gib)
    invisible = missing_workload_tools(MACOS_WORKLOAD_TOOLS)
    return {
        "platform": "macos",
        "architecture": normalize_arch(platform.machine()),
        "osVersionClass": f"macos-{major}",
        "glaedaGeneration": digest_file(glaeda),
        "toolchainGeneration": digest_bytes(canonical(toolchain)),
        "checks": {
            "supportedOs": major in {15, 26},
            "hardwareCapability": hardware_class_ready(
                "macos",
                hardware_class,
                cpus,
                memory_gib,
            ),
            "cmuxCheckout": (cmux_root / ".git").exists()
            and (cmux_root / ".xcode-version").is_file(),
            "canonicalCheckoutClean": cmux_checkout_clean(cmux_root),
            "submodulesReady": cmux_submodules_ready(cmux_root),
            "cmuxSetupArtifacts": cmux_setup_artifacts_present(cmux_root),
            "xcodePin": xcode_pin_ready(
                pin,
                xcode_match.group(1) if xcode_match else None,
                sdk,
            ),
            "git": git.startswith("git version "),
            "profileRunnerInterpreter": profile_runner_interpreter_ready(),
            "workloadToolPath": not invisible,
            "zig": zig_version_compatible(zig, zig_required),
            "rust": (
                rustup_version.startswith("rustup ")
                and cargo.startswith("cargo ")
                and rustc.startswith("rustc ")
                and diff_cargo.startswith("cargo ")
                and diff_rustc.startswith("rustc ")
            ),
            "metalToolchain": bool(metal),
            "glaedaExecutable": glaeda.is_file() and os.access(glaeda, os.X_OK),
            "diskAdmission": free_gib >= min_free_gib,
            "nativeCacheRoot": (not cache_required) or cache_ready,
            "nativeCacheDiskAdmission": (
                (not cache_required)
                or (cache_ready and cache_free_gib >= min_free_gib)
            ),
            "unattendedPower": mac_sleep_disabled_on_ac(pmset),
        },
        "observed": {
            "freeDiskGiBClass": (
                f"ge-{min_free_gib}" if free_gib >= min_free_gib else f"lt-{min_free_gib}"
            ),
            "xcodePin": pin,
            "nativeCacheFreeDiskGiBClass": (
                "unconfigured"
                if not cache_required
                else (
                    f"ge-{min_free_gib}"
                    if cache_ready and cache_free_gib >= min_free_gib
                    else f"lt-{min_free_gib}"
                )
            ),
            "toolsMissingFromWorkloadPath": invisible,
            "logicalCpuClass": "ge-8" if cpus >= 8 else "lt-8",
            "totalMemoryGiBClass": (
                "ge-16" if memory_gib >= 16 else "lt-16"
            ),
            # What a class acceptance receipt binds (cmux_fleet.py
            # export-class-acceptance): the model, chip and memory, and the
            # toolchain whose digest is toolchainGeneration. None of it names
            # this machine.
            "hardware": hardware,
            "toolchain": toolchain,
        },
    }


def read_os_release() -> tuple[str, str]:
    values: dict[str, str] = {}
    for line in Path("/etc/os-release").read_text(encoding="utf-8").splitlines():
        if "=" not in line:
            continue
        key, value = line.split("=", 1)
        values[key] = value.strip().strip('"')
    return values.get("ID", ""), values.get("VERSION_ID", "")


def linux_memory_gib(field: str) -> int:
    for line in Path("/proc/meminfo").read_text(encoding="utf-8").splitlines():
        if line.startswith(field + ":"):
            return int(line.split()[1]) // (1024**2)
    raise BootstrapError(f"{field} is unavailable")


def mac_total_memory_gib() -> int:
    raw = run([executable("sysctl"), "-n", "hw.memsize"])
    return int(raw) // (1024**3)


def mac_hardware(memory_gib: int) -> dict[str, Any]:
    """The hardware class identity: model identifier, chip and memory. No serial number."""
    sysctl = executable("sysctl")
    return {
        "model": run([sysctl, "-n", "hw.model"]).strip(),
        "chip": run([sysctl, "-n", "machdep.cpu.brand_string"]).strip(),
        "memoryGiB": memory_gib,
    }


def hardware_class_ready(
    platform_name: str,
    hardware_class: str,
    cpus: int,
    memory_gib: int,
) -> bool:
    minimums = {
        ("macos", "cmux-mac-build-large"): (8, 16),
        ("macos", "cmux-mac-test-large"): (8, 16),
        ("linux", "cmux-linux-ci-medium"): (4, 8),
        ("linux", "cmux-linux-agent-medium"): (4, 8),
    }
    required = minimums.get((platform_name, hardware_class))
    return (
        required is not None
        and cpus >= required[0]
        and memory_gib >= required[1]
    )


def collect_linux(
    cmux_root: Path,
    glaeda: Path,
    min_free_gib: int,
    roles: list[str],
    hardware_class: str,
) -> dict[str, Any]:
    if platform.system() != "Linux":
        raise BootstrapError("Linux bootstrap requires Linux")
    distro, version = read_os_release()
    git = run([executable("git"), "--version"])
    python = run([executable("python3"), "--version"])
    systemd = run([executable("systemctl"), "--version"]).splitlines()[0]
    bwrap = run([executable("bwrap"), "--version"])
    kernel_major = int(platform.release().split(".", 1)[0])
    actions_ok = True
    if "cmux_linux_ci" in roles:
        for name in ("curl", "tar", "gzip", "ldd"):
            actions_ok = actions_ok and shutil.which(name) is not None
    toolchain = {
        "distribution": f"{distro}-{version}",
        "kernelMajor": kernel_major,
        "gitVersion": git,
        "pythonVersion": python,
    }
    supported_distro = (distro, version) in {
        ("ubuntu", "24.04"),
        ("debian", "12"),
    }
    cgroup2 = Path("/sys/fs/cgroup/cgroup.controllers").is_file()
    pressure = all(
        Path(f"/proc/pressure/{name}").is_file()
        for name in ("cpu", "memory", "io")
    )
    available_gib = linux_memory_gib("MemAvailable")
    total_gib = linux_memory_gib("MemTotal")
    cpus = os.cpu_count() or 0
    free_gib = disk_free_gib(cmux_root)
    invisible = missing_workload_tools(LINUX_WORKLOAD_TOOLS)
    return {
        "platform": "linux",
        "architecture": normalize_arch(platform.machine()),
        "osVersionClass": f"{distro}-{version}",
        "glaedaGeneration": digest_file(glaeda),
        "toolchainGeneration": digest_bytes(canonical(toolchain)),
        "checks": {
            "supportedOs": supported_distro and kernel_major >= 6,
            "hardwareCapability": hardware_class_ready(
                "linux",
                hardware_class,
                cpus,
                total_gib,
            ),
            "cmuxCheckout": (cmux_root / ".git").exists(),
            "canonicalCheckoutClean": cmux_checkout_clean(cmux_root),
            "git": git.startswith("git version "),
            "profileRunnerInterpreter": profile_runner_interpreter_ready(),
            "workloadToolPath": not invisible,
            "glaedaExecutable": glaeda.is_file() and os.access(glaeda, os.X_OK),
            "systemd": systemd.startswith("systemd "),
            "bubblewrap": bwrap.startswith("bubblewrap "),
            "cgroupV2": cgroup2,
            "pressureSignals": pressure,
            "diskAdmission": free_gib >= min_free_gib,
            "memoryAdmission": available_gib >= 8,
            "actionsPrerequisites": actions_ok,
        },
        "observed": {
            "freeDiskGiBClass": (
                f"ge-{min_free_gib}" if free_gib >= min_free_gib else f"lt-{min_free_gib}"
            ),
            "availableMemoryGiBClass": (
                "ge-8" if available_gib >= 8 else "lt-8"
            ),
            "toolsMissingFromWorkloadPath": invisible,
            "logicalCpuClass": "ge-4" if cpus >= 4 else "lt-4",
            "totalMemoryGiBClass": (
                "ge-8" if total_gib >= 8 else "lt-8"
            ),
        },
    }


def evaluate(
    observation: dict[str, Any],
    roles: list[str],
    hardware_class: str,
) -> dict[str, Any]:
    platform_name = observation.get("platform")
    if platform_name not in {"macos", "linux"}:
        raise BootstrapError("bootstrap platform is invalid")
    roles = sorted(set(roles))
    if not roles or any(role not in ROLE_OS for role in roles):
        raise BootstrapError("bootstrap roles are invalid")
    unreviewed = [role for role in roles if role not in ENROLLABLE_ROLES]
    if unreviewed:
        raise BootstrapError(
            "bootstrap role lacks a reviewed v1 acceptance workload: "
            + ",".join(unreviewed)
        )
    for role in roles:
        required = ROLE_OS[role]
        if required is not None and required != platform_name:
            raise BootstrapError(f"role {role} is incompatible with {platform_name}")
    if re.fullmatch(r"[a-z0-9][a-z0-9._-]{0,79}", hardware_class) is None:
        raise BootstrapError("hardware capability class is invalid")
    checks = observation.get("checks")
    if (
        not isinstance(checks, dict)
        or not checks
        or any(type(value) is not bool for value in checks.values())
    ):
        raise BootstrapError("bootstrap checks are invalid")
    profiles = observation.get("roleProfiles")
    if not isinstance(profiles, dict) or set(profiles) != set(roles):
        raise BootstrapError("role profiles are invalid")
    for role in roles:
        profile = profiles.get(role)
        if (
            not isinstance(profile, dict)
            or set(profile) != {"id", "generation"}
            or profile != ROLE_PROFILES.get(role)
        ):
            raise BootstrapError("role profiles are invalid")
    failures = sorted(key for key, passed in checks.items() if not passed)
    result = {
        "schema": SCHEMA,
        "platform": platform_name,
        "architecture": observation["architecture"],
        "osVersionClass": observation["osVersionClass"],
        "hardwareCapabilityClass": hardware_class,
        "roles": roles,
        "glaedaGeneration": observation["glaedaGeneration"],
        "toolchainGeneration": observation["toolchainGeneration"],
        "roleProfiles": profiles,
        "checks": checks,
        "observed": observation.get("observed", {}),
        "eligibleForEnrollment": not failures,
        "blockingChecks": failures,
        "authority": "observation_only",
    }
    if len(canonical(result)) > MAX_OUTPUT_BYTES:
        raise BootstrapError("bootstrap receipt exceeds size ceiling")
    return result


def parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--platform", required=True, choices=("macos", "linux"))
    p.add_argument("--cmux-root", type=Path, required=True)
    p.add_argument("--glaeda", type=Path, required=True)
    p.add_argument("--hardware-class", required=True)
    p.add_argument("--role", action="append", required=True)
    p.add_argument(
        "--cache-root",
        type=Path,
        help="Existing operator-owned native build/cache root; path is never emitted",
    )
    p.add_argument("--min-free-gib", type=int,
                   help="override the free-disk admission floor (default: from --build-slots)")
    p.add_argument("--build-slots", type=int, default=1,
                   help="concurrent cmux builds this host runs (default 1: one runner)")
    return p


def main() -> int:
    try:
        args = parser().parse_args()
        cmux_root = args.cmux_root.resolve(strict=True)
        glaeda = args.glaeda.resolve(strict=True)
        cache_root = (
            args.cache_root.resolve(strict=True)
            if args.cache_root is not None
            else None
        )
        if (
            args.platform == "macos"
            and any(
                role == "cmux_macos_native_build"
                for role in args.role
            )
            and cache_root is None
        ):
            raise BootstrapError(
                "macOS native-build role requires --cache-root"
            )
        if args.build_slots < 1:
            raise BootstrapError("build slots must be at least 1")
        minimum = args.min_free_gib or default_min_free_gib(args.platform, args.build_slots)
        if minimum <= 0:
            raise BootstrapError("minimum free disk must be positive")
        observation = (
            collect_macos(
                cmux_root,
                glaeda,
                minimum,
                args.hardware_class,
                cache_root,
            )
            if args.platform == "macos"
            else collect_linux(
                cmux_root,
                glaeda,
                minimum,
                args.role,
                args.hardware_class,
            )
        )
        observation["roleProfiles"] = role_profiles(
            cmux_root,
            args.role,
            observation["platform"],
            observation["architecture"],
        )
        sys.stdout.buffer.write(
            canonical(evaluate(observation, args.role, args.hardware_class))
        )
        return 0
    except (
        OSError,
        ValueError,
        BootstrapError,
        subprocess.TimeoutExpired,
    ) as error:
        print(
            json.dumps({"error": explain_error(str(error))}, sort_keys=True, separators=(",", ":")),
            file=sys.stderr,
        )
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
