#!/usr/bin/env python3
"""Ultra-trusted native Apple builds with project-private, toolchain-keyed state."""

from __future__ import annotations

import argparse
import contextlib
import fcntl
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import signal
import stat
import subprocess
import sys
import time
import uuid


LIMIT = 65536
ENV_NAMES = ("HOME", "USER", "LOGNAME", "PATH", "TMPDIR", "LANG", "LC_ALL", "LC_CTYPE")
CACHE_NAMES = ("derived_data", "source_packages", "scratch", "module_cache", "products", "extensions")
RECEIPTS = {"build": "last-run.json", "dependencies": "last-dependencies.json", "check": "last-check.json"}
NATIVE_PHASES = {"SwiftEmitModule", "SwiftDriver", "SwiftDriver Compilation", "SwiftCompile",
                 "CompileC", "Ld", "PhaseScriptExecution", "CompileXCStrings", "CopyStringsFile",
                 "CodeSign", "SwiftGeneratePch", "CpResource", "CopySwiftLibs"}
XCODE_SETTINGS = {
    "ARCHS", "ONLY_ACTIVE_ARCH", "CODE_SIGNING_ALLOWED", "CODE_SIGNING_REQUIRED", "CODE_SIGN_IDENTITY",
    "DEVELOPMENT_TEAM", "CODE_SIGN_STYLE", "PRODUCT_BUNDLE_IDENTIFIER", "MACOSX_DEPLOYMENT_TARGET",
    "IPHONEOS_DEPLOYMENT_TARGET", "TVOS_DEPLOYMENT_TARGET", "WATCHOS_DEPLOYMENT_TARGET", "XROS_DEPLOYMENT_TARGET",
    "SWIFT_VERSION", "SWIFT_OPTIMIZATION_LEVEL", "GCC_OPTIMIZATION_LEVEL", "ENABLE_TESTABILITY",
    "SWIFT_ACTIVE_COMPILATION_CONDITIONS", "DEBUG_INFORMATION_FORMAT",
    "COMPILATION_CACHE_ENABLE_CACHING", "COMPILATION_CACHE_ENABLE_DIAGNOSTIC_REMARKS",
    "COMPILATION_CACHE_LIMIT_SIZE",
}


class Refusal(Exception):
    pass


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


def bounded_json(raw):
    if len(raw) > LIMIT:
        raise Refusal("JSON document exceeds the size limit")
    try:
        return json.loads(raw)
    except (ValueError, UnicodeError) as error:
        raise Refusal("invalid JSON document") from error


def base_environment():
    return {key: os.environ[key] for key in ENV_NAMES if key in os.environ}


def probe(argv, environment):
    result = subprocess.run(argv, env=environment, capture_output=True, timeout=30, check=False)
    if result.returncode or len(result.stdout) > LIMIT:
        raise Refusal("Apple toolchain or project probe failed")
    return result.stdout.decode().strip()


def apple_toolchain(environment, sdk="macosx"):
    if sys.platform != "darwin":
        raise Refusal("native Apple builds require macOS")
    developer = Path(os.environ.get("DEVELOPER_DIR") or probe(["/usr/bin/xcode-select", "-p"], environment)).resolve(strict=True)
    environment = {**environment, "DEVELOPER_DIR": str(developer)}
    swift = Path(probe(["/usr/bin/xcrun", "--find", "swift"], environment)).resolve(strict=True)
    xcodebuild = developer / "usr/bin/xcodebuild"
    return {
        "developer": str(developer), "swift": str(swift), "xcodebuild": str(xcodebuild),
        "swift_version": probe([str(swift), "--version"], environment),
        "xcode_version": probe([str(xcodebuild), "-version"], environment),
        "sdk": probe(["/usr/bin/xcrun", "--sdk", sdk, "--show-sdk-path"], environment),
        "sdk_build": probe(["/usr/bin/xcrun", "--sdk", sdk, "--show-sdk-build-version"], environment),
        "host": platform.machine(),
    }


def relative_path(value):
    if not isinstance(value, str) or not value or "\x00" in value:
        raise Refusal("expected a relative project path")
    path = Path(value)
    if path.is_absolute() or any(part in ("", ".", "..") for part in value.split("/")):
        raise Refusal("project paths must be normalized and relative")
    return path


def project_file(project, value):
    path = project / relative_path(value)
    resolved = path.resolve(strict=True)
    if not resolved.is_relative_to(project):
        raise Refusal("project file escapes the checkout")
    return resolved


def configuration(project, name):
    path = project_file(project, "glaeda.apple.json")
    with path.open("rb") as stream:
        config = bounded_json(stream.read(LIMIT + 1))
    if not isinstance(config, dict) or not {"schema_version", "profiles"} <= set(config) or set(config) - {"schema_version", "profiles", "preparations", "checks", "cache_policies"} or config["schema_version"] != 1:
        raise Refusal("expected Apple build configuration schema 1")
    profiles = config["profiles"]
    if not isinstance(profiles, dict) or name not in profiles or not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,63}", name):
        raise Refusal("unknown or invalid build profile")
    profile = profiles[name]
    common = {"engine", "environment", "sdk"}
    allowed = {
        "swiftpm": common | {"configuration", "product", "action", "triple", "jobs", "package", "incremental_file_hashing", "incremental_diagnostics"},
        "xcode": common | {"project", "workspace", "scheme", "configuration", "destination", "settings", "action"},
        "script": common | {"executable", "arguments"},
    }
    if not isinstance(profile, dict) or profile.get("engine") not in allowed or set(profile) - allowed[profile["engine"]]:
        raise Refusal("unknown engine or profile fields")
    return profile


def cache_lineage(project, name, owner, tools, engine, generation, initial_key):
    with project_file(project, "glaeda.apple.json").open("rb") as stream:
        policies = bounded_json(stream.read(LIMIT + 1)).get("cache_policies", {})
    if not isinstance(policies, dict) or len(policies) > 64 or any(
            not isinstance(key, str) or not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,63}", key)
            or value not in ("recipe", "native") for key, value in policies.items()):
        raise Refusal("cache policies must map profiles to recipe or native")
    if policies.get(name, "recipe") == "recipe":
        return initial_key, None
    # Native tools own input invalidation. This opt-in changes storage lifetime,
    # never command/result reuse. The first binding retains the existing cache.
    identity = digest([owner, name, tools, engine, generation])
    binding = {"schema_version": 1, "lineage": identity, "cache_key": initial_key}
    try:
        with store({"project": project, "owner": owner}) as state:
            binding = read_json(state, "lineage-" + identity + ".json")
    except FileNotFoundError:
        pass
    if (not isinstance(binding, dict) or set(binding) != {"schema_version", "lineage", "cache_key"}
            or binding["schema_version"] != 1 or binding["lineage"] != identity
            or not isinstance(binding["cache_key"], str)
            or not re.fullmatch(r"[0-9a-f]{64}", binding["cache_key"])):
        raise Refusal("native cache lineage identity mismatch")
    return binding["cache_key"], binding


def expand(value, paths):
    if not isinstance(value, str) or len(value) > 4096 or "\x00" in value:
        raise Refusal("invalid build argument")
    try:
        return value.format_map(paths)
    except (KeyError, ValueError, AttributeError, IndexError) as error:
        raise Refusal("unknown cache placeholder") from error


def swift_driver(toolchain):
    # Apple's swift symlink selects driver mode through argv[0]. Toolchain
    # identity resolves it to swift-frontend, which must not be invoked directly
    # for SwiftPM commands. Preserve identity while restoring the driver name.
    binary = Path(toolchain["swift"])
    if binary.name == "swift-frontend":
        driver = binary.with_name("swift")
        if driver.resolve(strict=True) != binary:
            raise Refusal("Swift driver no longer matches the observed toolchain")
        return str(driver)
    return str(binary)


def build_helper(project, profile):
    try:
        executable = project_file(project, profile.get("executable"))
    except FileNotFoundError as error:
        raise Refusal("configured build helper is missing; check the profile executable") from error
    if not executable.is_file() or not os.access(executable, os.X_OK):
        raise Refusal("build helper must be an executable project file")
    return executable


def command_for(project, profile, toolchain, paths):
    kind = profile["engine"]
    if kind == "script":
        executable = build_helper(project, profile)
        arguments = profile.get("arguments", [])
        if not isinstance(arguments, list) or len(arguments) > 64:
            raise Refusal("invalid helper arguments")
        return [str(executable), *(expand(arg, paths) for arg in arguments)]
    action = profile.get("action", "build")
    if action not in ("build", "test"):
        raise Refusal("native action must be build or test")
    if kind == "swiftpm":
        package = project_file(project, profile["package"]) if "package" in profile else project
        project_file(project, str((package / "Package.swift").relative_to(project)))
        config = profile.get("configuration", "debug")
        if config not in ("debug", "release"):
            raise Refusal("invalid SwiftPM configuration")
        argv = [swift_driver(toolchain), action, "--package-path", str(package), "--scratch-path", paths["scratch"],
                "--cache-path", paths["source_packages"], "--sdk", toolchain["sdk"], "--configuration", config]
        for key, option in (("product", "--product"), ("triple", "--triple")):
            if key in profile:
                if key == "product" and action == "test":
                    raise Refusal("SwiftPM test profiles cannot select a product")
                argv.extend([option, expand(profile[key], {})])
        if "jobs" in profile:
            jobs = profile["jobs"]
            if type(jobs) is not int or not 1 <= jobs <= 64:
                raise Refusal("jobs must be an integer between 1 and 64")
            argv.extend(["--jobs", str(jobs)])
        for key in ("incremental_file_hashing", "incremental_diagnostics"):
            if key in profile and type(profile[key]) is not bool:
                raise Refusal(key + " must be a boolean")
        if "incremental_file_hashing" in profile:
            flag = "-enable-incremental-file-hashing" if profile["incremental_file_hashing"] else "-disable-incremental-file-hashing"
            argv.extend(["-Xswiftc", flag])
        if profile.get("incremental_diagnostics"):
            argv.extend(["-Xswiftc", "-driver-show-incremental", "-Xswiftc", "-driver-show-job-lifecycle"])
        return argv
    containers = [key for key in ("project", "workspace") if key in profile]
    if len(containers) != 1:
        raise Refusal("Xcode needs exactly one project or workspace")
    key = containers[0]
    container = project_file(project, profile[key])
    argv = [toolchain["xcodebuild"], "-" + key, str(container)]
    for key in ("scheme", "configuration", "destination"):
        value = profile.get(key)
        if not isinstance(value, str) or not value or value.startswith("-"):
            raise Refusal("Xcode profiles must declare scheme, configuration and destination")
        argv.extend(["-" + key, expand(value, {})])
    argv.extend(["-sdk", toolchain["sdk"], "-derivedDataPath", paths["derived_data"], "-clonedSourcePackagesDirPath", paths["source_packages"]])
    settings = profile.get("settings", {})
    if not isinstance(settings, dict) or len(settings) > 32:
        raise Refusal("invalid Xcode settings")
    for key, value in sorted(settings.items()):
        if key not in XCODE_SETTINGS:
            raise Refusal("Xcode setting is not in the native adapter allowlist; use an explicit project helper")
        argv.append(key + "=" + expand(value, {}))
    argv.extend(["CLANG_MODULE_CACHE_PATH=" + paths["module_cache"], action])
    return argv


def dependency_command(project, name, tools, paths):
    with project_file(project, "glaeda.apple.json").open("rb") as stream:
        config = bounded_json(stream.read(LIMIT + 1))
    preparations = config.get("preparations", {})
    if not isinstance(preparations, dict) or not isinstance(preparations.get(name), dict):
        raise Refusal("profile has no declared dependency preparation")
    recipe = dict(preparations[name])
    reuse = recipe.pop("reuse", None)
    kind = recipe.get("engine")
    if kind == "swiftpm" and set(recipe) <= {"engine", "package"}:
        package = project_file(project, recipe["package"]) if "package" in recipe else project
        project_file(project, str((package / "Package.swift").relative_to(project)))
        argv = [swift_driver(tools), "package", "--package-path", str(package), "--scratch-path", paths["scratch"],
                "--cache-path", paths["source_packages"], "resolve"]
    elif kind == "xcode" and set(recipe) <= {"engine", "project", "workspace", "scheme"}:
        containers = [key for key in ("project", "workspace") if key in recipe]
        scheme = recipe.get("scheme")
        if len(containers) != 1 or not isinstance(scheme, str) or not scheme or scheme.startswith("-"):
            raise Refusal("dependency preparation needs one Xcode container and scheme")
        container = containers[0]
        argv = [tools["xcodebuild"], "-" + container, str(project_file(project, recipe[container])),
                "-scheme", expand(scheme, {}), "-derivedDataPath", paths["derived_data"],
                "-clonedSourcePackagesDirPath", paths["source_packages"], "-resolvePackageDependencies"]
    else:
        raise Refusal("invalid native dependency preparation recipe")
    observation = dependency_observation(project, paths, reuse) if reuse is not None else None
    return argv, digest(preparations[name]), observation


def dependency_observation(project, paths, declaration):
    """Bounded hints for skipping optional resolution, never native validation."""
    if not isinstance(declaration, dict) or set(declaration) != {"inputs", "required_files"}:
        raise Refusal("dependency reuse requires inputs and required_files")
    patterns, outputs = declaration["inputs"], declaration["required_files"]
    if not isinstance(patterns, list) or not 1 <= len(patterns) <= 32 or not isinstance(outputs, list) or not 1 <= len(outputs) <= 32:
        raise Refusal("dependency reuse declarations exceed bounds")
    files = set()
    for pattern in patterns:
        relative_path(pattern)
        for path in project.glob(pattern):
            files.add(str(path.relative_to(project)))
            if len(files) > 512:
                raise Refusal("too many dependency inputs")
    if not files:
        raise Refusal("dependency reuse matched no input files")
    total = 0
    def content(path, match="content"):
        nonlocal total
        with path.open("rb") as stream:
            if not stat.S_ISREG(os.fstat(stream.fileno()).st_mode):
                raise Refusal("dependency observation requires regular files")
            if match == "exists":
                return "regular_file_present"
            data = stream.read(16 * 1024 * 1024 + 1)
        total += len(data)
        if total > 16 * 1024 * 1024:
            raise Refusal("dependency observation exceeds byte limit")
        return hashlib.sha256(data).hexdigest()
    inputs = digest([(name, content(project_file(project, name))) for name in sorted(files)])
    observed = []
    for item in outputs:
        if (not isinstance(item, dict) or not {"cache", "path"} <= set(item) or set(item) - {"cache", "path", "match"}
                or item["cache"] not in CACHE_NAMES or item.get("match", "content") not in ("content", "exists")):
            raise Refusal("invalid required dependency file")
        relative = relative_path(item["path"])
        base = Path(paths[item["cache"]])
        candidate = base / relative
        try:
            resolved = candidate.resolve(strict=True)
            if not resolved.is_relative_to(base):
                raise Refusal("dependency file escapes its cache")
            observed.append(content(resolved, item.get("match", "content")))
        except FileNotFoundError:
            return {"inputs": inputs, "outputs": None}
    return {"inputs": inputs, "outputs": digest(observed)}


def check_command(project, name, tools, paths):
    with project_file(project, "glaeda.apple.json").open("rb") as stream:
        config = bounded_json(stream.read(LIMIT + 1))
    checks = config.get("checks", {})
    recipe = checks.get(name) if isinstance(checks, dict) else None
    if (not isinstance(recipe, dict) or recipe.get("engine") != "script"
            or set(recipe) - {"engine", "executable", "arguments"}):
        raise Refusal("profile needs a declared check helper")
    argv = command_for(project, recipe, tools, paths)
    identity = digest([recipe, hashlib.sha256(Path(argv[0]).read_bytes()).hexdigest()])
    return argv, identity


def prepare(project, name, generation="default", toolchain_probe=apple_toolchain, operation="build"):
    project = Path(project).resolve(strict=True)
    environment = base_environment()
    root = Path(probe(["/usr/bin/git", "-C", str(project), "rev-parse", "--show-toplevel"], environment)).resolve(strict=True)
    if project != root:
        raise Refusal("project must be the physical Git checkout root")
    if not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,63}", generation):
        raise Refusal("invalid cache generation label")
    profile = configuration(project, name)
    sdk = profile.get("sdk", "macosx")
    if sdk not in ("macosx", "iphoneos", "iphonesimulator", "appletvos", "appletvsimulator", "watchos", "watchsimulator", "xros", "xrsimulator"):
        raise Refusal("unsupported Apple SDK name")
    tools = toolchain_probe(environment, sdk)
    info = project.stat()
    owner = {"schema_version": 1, "project": digest([str(project), info.st_dev, info.st_ino])}
    recipe = None
    if profile["engine"] == "script":
        recipe_path = build_helper(project, profile)
        recipe = hashlib.sha256(recipe_path.read_bytes()).hexdigest()
    key = digest([owner, profile, tools, generation, recipe])
    invocation_identity = key
    key, lineage = cache_lineage(project, name, owner, tools, profile["engine"], generation, key)
    cache = project / ".glaeda/apple-build/cache" / key
    paths = {name: str(cache / name) for name in CACHE_NAMES}
    argv = command_for(project, profile, tools, paths)
    if operation not in RECEIPTS:
        raise Refusal("unknown native operation")
    operation_identity = None
    dependency_state = None
    if operation == "dependencies":
        argv, operation_identity, dependency_state = dependency_command(project, name, tools, paths)
    elif operation == "check":
        argv, operation_identity = check_command(project, name, tools, paths)
    configured = profile.get("environment", {})
    if not isinstance(configured, dict) or len(configured) > 32:
        raise Refusal("invalid build environment")
    for variable, value in configured.items():
        if not re.fullmatch(r"[A-Z][A-Z0-9_]*", variable) or variable in ENV_NAMES or variable in {"DEVELOPER_DIR", "SDKROOT", "TOOLCHAINS", "DYLD_INSERT_LIBRARIES", "DYLD_LIBRARY_PATH"}:
            raise Refusal("build environment overrides a reserved variable")
        environment[variable] = expand(value, paths)
    environment["DEVELOPER_DIR"] = tools["developer"]
    environment["CLANG_MODULE_CACHE_PATH"] = paths["module_cache"]
    return {"project": project, "profile": name, "owner": owner, "key": key, "paths": paths,
            "argv": argv, "environment": environment, "engine": profile["engine"], "generation": generation,
            "operation": operation, "operation_identity": operation_identity, "dependency_state": dependency_state,
            "invocation_identity": invocation_identity, "lineage": lineage}


def directory(parent, name, create=False):
    made = False
    if create:
        try:
            os.mkdir(name, 0o700, dir_fd=parent)
            made = True
        except FileExistsError:
            pass
    fd = os.open(name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=parent)
    info = os.fstat(fd)
    if info.st_uid != os.getuid() or info.st_mode & 0o022:
        os.close(fd)
        raise Refusal("state directory ownership or permissions are unsafe")
    return fd, made


def read_json(fd, name):
    file = os.open(name, os.O_RDONLY | os.O_NOFOLLOW, dir_fd=fd)
    with os.fdopen(file, "rb") as stream:
        info = os.fstat(stream.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_nlink != 1 or info.st_mode & 0o077:
            raise Refusal("state file ownership or permissions are unsafe")
        return bounded_json(stream.read(LIMIT + 1))


def write_json(fd, name, document):
    temporary = "write-" + uuid.uuid4().hex
    file = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600, dir_fd=fd)
    try:
        with os.fdopen(file, "w") as stream:
            json.dump(document, stream, sort_keys=True)
            stream.write("\n")
            stream.flush()
            os.fsync(stream.fileno())
        os.rename(temporary, name, src_dir_fd=fd, dst_dir_fd=fd)
        os.fsync(fd)
    finally:
        with contextlib.suppress(FileNotFoundError):
            os.unlink(temporary, dir_fd=fd)


@contextlib.contextmanager
def store(plan, create=False):
    descriptors = [os.open(plan["project"], os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)]
    try:
        info = os.fstat(descriptors[0])
        if digest([str(plan["project"]), info.st_dev, info.st_ino]) != plan["owner"]["project"]:
            raise Refusal("checkout identity changed before state access")
        parent, _ = directory(descriptors[-1], ".glaeda", create)
        descriptors.append(parent)
        state, made = directory(parent, "apple-build", create)
        descriptors.append(state)
        if made:
            write_json(state, "owner.json", plan["owner"])
        if read_json(state, "owner.json") != plan["owner"]:
            raise Refusal("state belongs to another checkout; no adoption performed")
        yield state
    finally:
        for descriptor in reversed(descriptors):
            os.close(descriptor)


def inspect(plan):
    status = "cold"
    active = None
    try:
        with store(plan) as state:
            try:
                read_json(state, "quarantine-" + plan["key"] + ".json")
            except FileNotFoundError:
                pass
            else:
                raise Refusal("cache was interrupted; choose a new --generation for a cold rebuild")
            try:
                active = read_json(state, "inflight.json")["run_id"]
                status = "interrupted_or_running"
            except FileNotFoundError:
                try:
                    caches, _ = directory(state, "cache")
                    try:
                        cache, _ = directory(caches, plan["key"])
                        try:
                            try:
                                owner = read_json(cache, "owner.json")
                            except FileNotFoundError as error:
                                raise Refusal("existing cache generation is unmarked") from error
                            if owner != {**plan["owner"], "key": plan["key"]}:
                                raise Refusal("cache generation identity mismatch")
                            for name in CACHE_NAMES:
                                try:
                                    folder, _ = directory(cache, name)
                                except FileNotFoundError:
                                    continue
                                os.close(folder)
                            status = "prepared"
                        finally:
                            os.close(cache)
                    finally:
                        os.close(caches)
                except FileNotFoundError:
                    pass
    except FileNotFoundError:
        # Missing state is cold, but an existing unmarked state directory is ambiguous.
        if (plan["project"] / ".glaeda/apple-build").exists():
            raise Refusal("existing Apple state is incomplete; refusing implicit adoption")
    return {"schema_version": 1, "authority": "developer_observation_only", "profile": plan["profile"],
            "engine": plan["engine"], "cache_key": plan["key"], "generation": plan["generation"],
            "cache_root": ".glaeda/apple-build/cache/" + plan["key"],
            "state": status, "active_run": active, "isolation": "trusted_native_host", "result_reuse": False,
            "operation": plan.get("operation", "build"), "invocation_identity": plan.get("invocation_identity")}


@contextlib.contextmanager
def lock(state, wait_seconds=0):
    fd = os.open("build.lock", os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600, dir_fd=state)
    try:
        info = os.fstat(fd)
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_nlink != 1 or info.st_mode & 0o077:
            raise Refusal("build lock is unsafe")
        deadline = time.monotonic() + wait_seconds
        while True:
            if wait_seconds and time.monotonic() >= deadline:
                raise Refusal("Apple build wait deadline expired; active owner was not interrupted")
            try:
                fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
                break
            except BlockingIOError as error:
                if not wait_seconds:
                    raise Refusal("another Glaeda Apple build owns this checkout") from error
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise Refusal("Apple build wait deadline expired; active owner was not interrupted") from error
                time.sleep(min(0.1, remaining))
        yield
    finally:
        os.close(fd)


def group_absent(pgid):
    if type(pgid) is not int or pgid <= 1:
        return False
    try:
        os.killpg(pgid, 0)
        return False
    except ProcessLookupError:
        return True


def source_snapshot(plan):
    git = ["/usr/bin/git", "-C", str(plan["project"])]
    environment = base_environment()
    commit = probe([*git, "rev-parse", "HEAD"], environment)
    tree = probe([*git, "rev-parse", "HEAD^{tree}"], environment)
    if (not re.fullmatch(r"[a-f0-9]{40,64}", commit)
            or not re.fullmatch(r"[a-f0-9]{40,64}", tree)):
        raise Refusal("project source identity is invalid")
    return {"commit": commit, "tree": tree,
            "clean": not bool(probe([*git, "status", "--porcelain"], environment))}


def validate_source(plan, expected_commit=None, expected_tree=None, require_clean=False):
    for value, subject in ((expected_commit, "commit"), (expected_tree, "tree")):
        if value is not None and (not isinstance(value, str) or not re.fullmatch(r"[a-f0-9]{40,64}", value)):
            raise Refusal("expected source " + subject + " is invalid")
    if type(require_clean) is not bool:
        raise Refusal("clean-source requirement must be boolean")
    snapshot = source_snapshot(plan)
    if expected_commit is not None and snapshot["commit"] != expected_commit:
        raise Refusal("project HEAD does not match the expected source commit")
    if expected_tree is not None and snapshot["tree"] != expected_tree:
        raise Refusal("project HEAD does not match the expected source tree")
    if require_clean and not snapshot["clean"]:
        raise Refusal("project working tree does not satisfy the clean-source requirement")
    return snapshot


def native_work_summary(state, run_id):
    """Bounded, content-minimised advice; never used for admission or reuse."""
    if not re.fullmatch(r"[0-9a-f]{32}", run_id):
        return {"state": "unavailable"}
    fd = os.open("run-" + run_id + ".log", os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=state)
    with os.fdopen(fd, "rb") as stream:
        info = os.fstat(stream.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_size > 64 * 1024 * 1024:
            return {"state": "unavailable"}
        phases = {}
        cache = None
        summaries = 0
        remaining = info.st_size
        while remaining > 0:
            line = stream.readline(min(remaining, 65536))
            if not line:
                break
            remaining -= len(line)
            if len(line) == 65536 and not line.endswith(b"\n"):
                return {"state": "unavailable"}
            text = line.decode("utf-8", errors="replace").strip()
            if text == "Build Timing Summary":
                phases = {}
                summaries += 1
            timing = re.fullmatch(r"([A-Za-z ]+) \(([0-9]+) tasks?\) \| ([0-9]+(?:\.[0-9]+)?) seconds", text)
            if summaries and timing and timing[1] in NATIVE_PHASES:
                count, seconds = int(timing[2]), float(timing[3])
                if count <= 1000000 and seconds <= 1e9:
                    phases[timing[1]] = {"tasks": count, "seconds": seconds}
            hits = re.fullmatch(r"note: ([0-9]+) hits? / ([0-9]+) cacheable tasks \([0-9]+%\)", text)
            if hits and 0 <= int(hits[1]) <= int(hits[2]) <= 1000000:
                cache = {"hits": int(hits[1]), "cacheable_tasks": int(hits[2])}
        result = {"state": "reported" if phases or cache else "not_reported",
                  "authority": "advisory_only", "task_times_may_overlap": True,
                  "timing_summaries": summaries, "last_reported_task_timings": phases}
        if cache is not None:
            result["last_reported_compilation_cache"] = cache
        return result


def execute(plan, prepare_again=prepare, reuse_dependencies=False, wait_seconds=0,
            expected_commit=None, expected_tree=None, require_clean_source=False):
    if type(wait_seconds) is not int or not 0 <= wait_seconds <= 3600:
        raise Refusal("wait seconds must be an integer from 0 to 3600")
    entered = time.monotonic()
    validate_source(plan, expected_commit, expected_tree, require_clean_source)
    inspect(plan)
    waiting = time.monotonic()
    with store(plan, create=True) as state, (lock(state, wait_seconds) if wait_seconds else lock(state)):
        admitted = time.monotonic()
        # A previous owner can quarantine this generation between the first probe and lock.
        inspect(plan)
        try:
            read_json(state, "inflight.json")
        except FileNotFoundError:
            pass
        else:
            raise Refusal("unfinished build recorded; use plan, then recover its exact run id")
        # Repeat admission after acquiring the project lock, before creating reusable state.
        options = {"operation": plan["operation"]} if plan.get("operation", "build") != "build" else {}
        fresh = prepare_again(plan["project"], plan["profile"], plan["generation"], **options)
        if (fresh["key"] != plan["key"] or fresh.get("operation_identity") != plan.get("operation_identity")
                or fresh.get("invocation_identity") != plan.get("invocation_identity")
                or fresh.get("lineage") != plan.get("lineage")):
            raise Refusal("build configuration or toolchain changed during admission")
        source_before = validate_source(fresh, expected_commit, expected_tree, require_clean_source)
        source_validation = ("exact_commit_tree_clean" if expected_commit is not None and expected_tree is not None
                             and require_clean_source else "bounded_expectation" if expected_commit is not None
                             or expected_tree is not None or require_clean_source else "observed_only")
        dependency_state = fresh.get("dependency_state")
        if reuse_dependencies and plan.get("operation") == "dependencies" and dependency_state and dependency_state["outputs"]:
            try:
                previous = read_json(state, "last-dependencies.json")
            except FileNotFoundError:
                previous = {}
            if (isinstance(previous, dict) and previous.get("exit_code") == 0
                    and previous.get("cache_key") == plan["key"]
                    and previous.get("invocation_identity") == plan.get("invocation_identity")
                    and previous.get("preparation_identity") == plan["operation_identity"]
                    and previous.get("dependency_state") == dependency_state):
                if plan.get("lineage"):
                    write_json(state, "lineage-" + plan["lineage"]["lineage"] + ".json", plan["lineage"])
                source_after = validate_source(fresh, expected_commit, expected_tree, require_clean_source)
                return {**inspect(plan), "state": "preparation_reused", "exit_code": 0,
                        "validation": "declared_inputs_and_files_match", "native_build_validation_required": True,
                        "source_validation": source_validation, "source_before": source_before,
                        "source_after": source_after, "elapsed_seconds": round(time.monotonic() - entered, 6)}
        caches, _ = directory(state, "cache", True)
        try:
            cache, made = directory(caches, plan["key"], True)
            try:
                expected = {**plan["owner"], "key": plan["key"]}
                if made:
                    write_json(cache, "owner.json", expected)
                if read_json(cache, "owner.json") != expected:
                    raise Refusal("cache generation identity mismatch")
                for name in CACHE_NAMES:
                    folder, _ = directory(cache, name, True)
                    os.close(folder)
            finally:
                os.close(cache)
        finally:
            os.close(caches)
        if plan.get("lineage"):
            write_json(state, "lineage-" + plan["lineage"]["lineage"] + ".json", plan["lineage"])
        run_id = uuid.uuid4().hex
        active = {"schema_version": 1, "run_id": run_id, "cache_key": plan["key"], "pgid": None,
                  "operation": plan.get("operation", "build")}
        write_json(state, "inflight.json", active)
        started = time.monotonic()
        child = None
        handlers = {}
        interrupted = []
        try:
            log_fd = os.open("run-" + run_id + ".log", os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600, dir_fd=state)
            with os.fdopen(log_fd, "wb") as log:
                child = subprocess.Popen(plan["argv"], cwd=plan["project"], env=plan["environment"],
                                         stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
            active["pgid"] = child.pid
            write_json(state, "inflight.json", active)
            def forward(number, frame):
                interrupted.append(number)
                with contextlib.suppress(ProcessLookupError):
                    os.killpg(child.pid, number)
            for signum in (signal.SIGINT, signal.SIGTERM):
                handlers[signum] = signal.signal(signum, forward)
            code = child.wait()
            native_finished = time.monotonic()
            deadline = time.monotonic() + 2
            while not group_absent(child.pid) and time.monotonic() < deadline:
                time.sleep(0.05)
            if not group_absent(child.pid):
                raise Refusal("build descendants remain; state kept unfinished")
            if interrupted:
                code = -interrupted[0]
            source_after = validate_source(fresh, expected_commit, expected_tree, require_clean_source)
            receipt = {**inspect(plan), "state": "completed", "active_run": None, "run_id": run_id,
                       "exit_code": code if code >= 0 else 128 - code, "signal": -code if code < 0 else None,
                       "elapsed_seconds": round(time.monotonic() - started, 6), "validation": "native_command_ran",
                       "source_validation": source_validation,
                       "source_before": source_before, "source_after": source_after}
            receipt["timings_seconds"] = {
                "initial_observation": round(waiting - entered, 6),
                "store_and_lock": round(admitted - waiting, 6),
                "admission_and_preparation": round(started - admitted, 6),
                "native_command": round(native_finished - started, 6),
                "completion_observation": round(time.monotonic() - native_finished, 6),
            }
            receipt_name = RECEIPTS[plan.get("operation", "build")]
            if plan.get("operation") == "dependencies" and dependency_state:
                receipt["preparation_status"] = "observation_unavailable"
                try:
                    after = prepare_again(plan["project"], plan["profile"], plan["generation"], operation="dependencies")
                    after_state = after.get("dependency_state")
                    if (after["key"] == plan["key"] and after.get("operation_identity") == plan["operation_identity"]
                            and after_state and dependency_state["inputs"] == after_state["inputs"]):
                        receipt["dependency_state"] = after_state
                        receipt["preparation_identity"] = plan["operation_identity"]
                        receipt["preparation_status"] = "declared_files_observed" if after_state["outputs"] else "required_files_missing"
                    else:
                        receipt["preparation_status"] = "inputs_changed"
                except (Refusal, OSError, ValueError, KeyError, TypeError, AttributeError, subprocess.SubprocessError):
                    # The child is gone. A changed/deleted manifest means a miss,
                    # not a live or interrupted build requiring recovery.
                    pass
                if code == 0 and receipt["preparation_status"] != "declared_files_observed":
                    receipt["exit_code"] = 1
            try:
                receipt["native_work"] = native_work_summary(state, run_id)
            except (OSError, ValueError, OverflowError):
                receipt["native_work"] = {"state": "unavailable"}
            write_json(state, receipt_name, receipt)
            if code < 0:
                write_json(state, "quarantine-" + plan["key"] + ".json", receipt)
            os.unlink("inflight.json", dir_fd=state)
            os.fsync(state)
            return receipt
        except OSError:
            # A failed spawn has no child. Once spawned, unknown state stays blocked.
            if child is None:
                os.unlink("inflight.json", dir_fd=state)
                os.fsync(state)
            raise
        finally:
            for signum, handler in handlers.items():
                signal.signal(signum, handler)


def recover(plan, run_id):
    with store(plan) as state, lock(state):
        active = read_json(state, "inflight.json")
        if active.get("run_id") != run_id or not group_absent(active.get("pgid")):
            raise Refusal("recovery requires the exact run id and an absent build process group")
        key = active.get("cache_key")
        if not isinstance(key, str) or not re.fullmatch(r"[a-f0-9]{64}", key):
            raise Refusal("interrupted cache identity is invalid")
        operation = active.get("operation", "build")
        if operation not in RECEIPTS:
            raise Refusal("interrupted operation is invalid")
        receipt = {"schema_version": 1, "state": "interruption_recovered", "run_id": run_id,
                   "authority": "developer_observation_only", "result_reuse": False, "operation": operation}
        write_json(state, RECEIPTS[operation], receipt)
        write_json(state, "quarantine-" + key + ".json", receipt)
        os.unlink("inflight.json", dir_fd=state)
        os.fsync(state)
        return receipt


def explain(plan):
    """Read-only advice; never used to admit work or skip the native command."""
    result = inspect(plan)
    current = source_snapshot(plan)
    previous = None
    try:
        with store(plan) as state:
            previous = read_json(state, "last-run.json")
    except FileNotFoundError:
        pass
    comparable = isinstance(previous, dict) and previous.get("cache_key") == plan["key"]
    before = previous.get("source_after") if comparable else None
    if not isinstance(before, dict):
        change = "no_comparable_build"
    elif before.get("commit") != current["commit"]:
        change = "commit_changed"
    elif not before.get("clean") or not current["clean"]:
        change = "working_tree_requires_native_validation"
    else:
        change = "same_clean_commit"
    result["explanation"] = {
        "source_observation": change,
        "cache_observation": "matching_generation_exists" if result["state"] == "prepared" else result["state"],
        "source_edits": "retain_paths_native_build_validates_inputs",
        "dependency_changes": "retain_paths_native_package_manager_resolves_inputs",
        "generation_inputs": (["physical_checkout", "profile_name", "engine", "toolchain_sdk_architecture", "generation_label"]
                              if plan.get("lineage") else ["physical_checkout", "profile", "toolchain_sdk_architecture", "direct_build_helper", "generation_label"]),
        "cache_policy": "native" if plan.get("lineage") else "recipe",
        "next_action": "resolve_active_state" if result["active_run"] else "run_native_build",
        "native_phases": "package_resolution_compilation_packaging_not_separately_instrumented",
    }
    if comparable:
        timings = previous.get("timings_seconds", {})
        names = ("initial_observation", "store_and_lock", "admission_and_preparation", "native_command", "completion_observation")
        result["last_build_timings_seconds"] = {
            name: timings[name] for name in names
            if isinstance(timings, dict) and type(timings.get(name)) in (int, float) and 0 <= timings[name] <= 1e9
        }
    return result


def main():
    parser = argparse.ArgumentParser(description="Prepare and run trusted native Apple builds with persistent project caches")
    parser.add_argument("action", choices=("plan", "plan-check", "check", "plan-dependencies", "dependencies", "ensure-dependencies", "explain", "run", "warm", "recover", "submit", "request-status", "forget-request", "requests", "wake", "wait-request", "plan-refresh", "refresh"))
    parser.add_argument("--project", type=Path, default=Path.cwd())
    parser.add_argument("--profile", default="app")
    parser.add_argument("--generation", default="default", help="stable label; use a new label for a cold rebuild without deleting old caches")
    parser.add_argument("--run-id", help="exact interrupted run id, only for recover")
    parser.add_argument("--wait-seconds", type=int, default=0, help="wait up to 3600 seconds for native execution admission; default refuses contention")
    parser.add_argument("--operation", choices=("check", "build", "dependencies"), help="operation for submit; default check")
    parser.add_argument("--request-id", help="exact id for request-status or forget-request")
    parser.add_argument("--expected-commit", help="exact Git commit required immediately before and after native execution")
    parser.add_argument("--expected-tree", help="exact Git tree required immediately before and after native execution")
    parser.add_argument("--require-clean-source", action="store_true",
                        help="require a clean Git worktree immediately before and after native execution")
    args = parser.parse_args()
    try:
        if (args.action == "recover") != bool(args.run_id):
            raise Refusal("--run-id is required only for recover")
        if not 0 <= args.wait_seconds <= 3600:
            raise Refusal("wait seconds must be an integer from 0 to 3600")
        if args.wait_seconds and args.action not in ("check", "dependencies", "ensure-dependencies", "run", "warm", "wait-request"):
            raise Refusal("--wait-seconds applies only to native execution")
        if (args.action in ("request-status", "forget-request", "wait-request")) != bool(args.request_id):
            raise Refusal("--request-id is required only for request-status, forget-request or wait-request")
        if args.operation is not None and args.action != "submit":
            raise Refusal("--operation applies only to submit")
        source_contract = args.expected_commit is not None or args.expected_tree is not None or args.require_clean_source
        if source_contract and args.action not in ("check", "dependencies", "ensure-dependencies", "run", "warm"):
            raise Refusal("exact source options apply only to direct native execution")
        if args.action in ("submit", "request-status", "forget-request", "requests", "wake", "wait-request", "plan-refresh", "refresh"):
            import apple_queue
            if args.action in ("plan-refresh", "refresh"):
                plan = apple_queue.request_plan(args.project, args.profile, args.generation, "refresh")
                result = ({"check": inspect({**plan, "operation": "check"}),
                           "dependencies": inspect(plan["refresh_dependencies"]),
                           "policy": "reuse_declared_dependency_readiness_then_native_incremental_check"}
                          if args.action == "plan-refresh" else apple_queue.submit(plan))
            elif args.action == "wait-request":
                result = apple_queue.wait_request(args.project, args.request_id, args.wait_seconds)
                print(json.dumps(result, sort_keys=True))
                return 124 if result["wait"] == "deadline" else 0 if result["state"] == "completed" else 1
            elif args.action == "submit":
                result = apple_queue.submit(prepare(args.project, args.profile, args.generation, operation=args.operation or "check"))
            elif args.action == "wake":
                result = apple_queue.wake(args.project)
            elif args.action == "requests":
                result = apple_queue.list_requests(args.project)
            else:
                result = apple_queue.status(args.project, args.request_id, forget=args.action == "forget-request")
            print(json.dumps(result, sort_keys=True))
            return 0
        preparing = time.monotonic()
        operation = "dependencies" if args.action in ("dependencies", "ensure-dependencies", "plan-dependencies") else "check" if args.action in ("check", "plan-check") else "build"
        plan = prepare(args.project, args.profile, args.generation, operation=operation)
        preparation_seconds = round(time.monotonic() - preparing, 6)
        result = explain(plan) if args.action == "explain" else inspect(plan) if args.action in ("plan", "plan-dependencies", "plan-check") else recover(plan, args.run_id) if args.action == "recover" else execute(
            plan, reuse_dependencies=args.action == "ensure-dependencies", wait_seconds=args.wait_seconds,
            expected_commit=args.expected_commit, expected_tree=args.expected_tree,
            require_clean_source=args.require_clean_source)
        result["toolchain_and_profile_probe_seconds"] = preparation_seconds
        print(json.dumps(result, sort_keys=True))
        return result.get("exit_code", 0)
    except KeyboardInterrupt:
        print(json.dumps({"schema_version": 1, "state": "cancelled", "reason": "native Apple request interrupted"}), file=sys.stderr)
        return 130
    except (Refusal, OSError, ValueError, KeyError, TypeError, AttributeError, subprocess.SubprocessError) as error:
        # Exception paths may contain private project/toolchain paths; public failures do not.
        message = str(error) if isinstance(error, Refusal) else "native Apple build observation or execution failed"
        print(json.dumps({"schema_version": 1, "state": "refused", "reason": message}), file=sys.stderr)
        return 2


if __name__ == "__main__":
    # Installed snapshots execute this file directly; queue imports must share
    # the same exception classes and loaded engine rather than importing twice.
    sys.modules.setdefault("apple_build", sys.modules[__name__])
    raise SystemExit(main())
