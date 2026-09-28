#!/usr/bin/env python3
"""Build, verify and stage exact-source fleet candidates."""
from __future__ import annotations

import argparse
import gzip
import hashlib
import io
import json
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import tarfile
import tempfile
import tomllib

SCHEMA = "glaeda-fleet-candidate/v1"
TARGETS = {"aarch64-apple-darwin", "x86_64-unknown-linux-gnu"}
FILES = (
    "LICENSE",
    "scripts/cmux-fleet",
    "scripts/cmux-fleet-bootstrap-macos",
    "scripts/cmux-fleet-bootstrap-linux",
    "scripts/cmux_fleet.py",
    "scripts/cmux_fleet_bootstrap.py",
    "scripts/cmux_fleet_operator_summary.py",
    "scripts/cmux_execution_roles.py",
    "scripts/cmux_workload_request.py",
    "docs/CMUX_FLEET_ENROLLMENT.md",
)
PAYLOAD = {*FILES, "bin/glaeda", "THIRD_PARTY_NOTICES.txt"}
MAX_ARCHIVE = 100 * 1024 * 1024
MAX_FILE = 100 * 1024 * 1024
MAX_TOTAL = 200 * 1024 * 1024


class BundleError(RuntimeError):
    pass


def canonical(value: object) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":"), allow_nan=False) + "\n").encode()


def sha256(raw: bytes) -> str:
    return hashlib.sha256(raw).hexdigest()


def file_bytes(path: Path) -> bytes:
    info = path.lstat()
    if not stat.S_ISREG(info.st_mode) or info.st_size > MAX_FILE:
        raise BundleError("bundle input must be a bounded regular file")
    with path.open("rb") as stream:
        raw = stream.read(MAX_FILE + 1)
    if len(raw) > MAX_FILE:
        raise BundleError("bundle input exceeds size limit")
    return raw


def child_environment() -> dict[str, str]:
    return {key: os.environ[key] for key in (
        "PATH", "HOME", "CARGO_HOME", "RUSTUP_HOME", "TMPDIR", "TMP", "TEMP",
    ) if key in os.environ}


def command(root: Path, argv: list[str], *, capture: bool = True) -> bytes:
    executable = shutil.which(argv[0])
    if executable is None:
        raise BundleError(f"required tool unavailable: {argv[0]}")
    result = subprocess.run(
        [executable, *argv[1:]], cwd=root, env=child_environment(),
        stdin=subprocess.DEVNULL, stdout=subprocess.PIPE if capture else None,
        check=False,
    )
    if result.returncode:
        raise BundleError(f"{argv[0]} failed with exit code {result.returncode}")
    return result.stdout if capture else b""


def source_identity(root: Path, expected: str) -> dict[str, str]:
    if re.fullmatch(r"[0-9a-f]{40}", expected) is None:
        raise BundleError("expected source must be a full commit SHA")
    if command(root, ["git", "status", "--porcelain", "--untracked-files=normal"]):
        raise BundleError("candidate source checkout must be clean")
    commit = command(root, ["git", "rev-parse", "HEAD^{commit}"]).decode().strip()
    if commit != expected:
        raise BundleError("candidate source commit does not match request")
    tree = command(root, ["git", "rev-parse", "HEAD^{tree}"]).decode().strip()
    return {"repository": "teamleaderleo/glaeda", "commit": commit, "tree": tree}


def notices(root: Path, target: str) -> bytes:
    metadata = json.loads(command(root, [
        "cargo", "metadata", "--locked", "--format-version", "1", "--filter-platform", target,
    ]))
    sections = []
    for package in sorted(metadata["packages"], key=lambda p: (p["name"], p["version"])):
        directory = Path(package["manifest_path"]).parent
        paths = {path for pattern in ("LICENSE*", "COPYING*", "NOTICE*")
                 for path in directory.glob(pattern) if path.is_file()}
        if package.get("license_file"):
            path = directory / package["license_file"]
            if not path.resolve().is_relative_to(directory.resolve()):
                raise BundleError("dependency license path escapes its package")
            paths.add(path)
        if not paths:
            raise BundleError(f"dependency license text unavailable: {package['name']}")
        sections.append(f"\n{package['name']} {package['version']} ({package.get('license') or 'custom'})\n".encode())
        for path in sorted(paths):
            sections.extend([f"\n{path.name}\n".encode(), file_bytes(path), b"\n"])
    raw = b"".join(sections)
    if len(raw) > MAX_FILE:
        raise BundleError("dependency notices exceed size limit")
    return raw


def archive_bytes(payload: dict[str, bytes], manifest: dict) -> bytes:
    output = io.BytesIO()
    with gzip.GzipFile(fileobj=output, mode="wb", mtime=0, filename="") as compressed:
        with tarfile.open(fileobj=compressed, mode="w", format=tarfile.USTAR_FORMAT) as archive:
            for name, raw in sorted({**payload, "manifest.json": canonical(manifest)}.items()):
                entry = tarfile.TarInfo(name)
                entry.size = len(raw)
                entry.mode = 0o755 if name == "bin/glaeda" else 0o644
                archive.addfile(entry, io.BytesIO(raw))
    return output.getvalue()


def verified_contents(raw: bytes, expected_sha256: str, source: str, target: str) -> tuple[dict, dict[str, bytes]]:
    if len(raw) > MAX_ARCHIVE or sha256(raw) != expected_sha256:
        raise BundleError("candidate archive digest or size mismatch")
    contents = {}
    total = 0
    try:
        # tarfile consumes PAX/GNU metadata before yielding members. Bound the
        # entire decompressed stream, including those headers, before parsing.
        with gzip.GzipFile(fileobj=io.BytesIO(raw), mode="rb") as compressed:
            tar_bytes = compressed.read(MAX_TOTAL + 1)
        if len(tar_bytes) > MAX_TOTAL:
            raise BundleError("candidate archive expands beyond size limit")
        with tarfile.open(fileobj=io.BytesIO(tar_bytes), mode="r:") as archive:
            for entry in archive:
                if (entry.name not in PAYLOAD | {"manifest.json"}
                        or entry.name in contents or not entry.isfile()
                        or entry.mode != (0o755 if entry.name == "bin/glaeda" else 0o644)
                        or entry.size < 0 or entry.size > MAX_FILE):
                    raise BundleError("candidate archive contains an unsafe or unexpected entry")
                total += entry.size
                if total > MAX_TOTAL:
                    raise BundleError("candidate archive expands beyond size limit")
                stream = archive.extractfile(entry)
                if stream is None:
                    raise BundleError("candidate archive entry is unreadable")
                contents[entry.name] = stream.read(MAX_FILE + 1)
        if set(contents) != PAYLOAD | {"manifest.json"}:
            raise BundleError("candidate archive is incomplete")
        manifest = json.loads(contents.pop("manifest.json"))
        if not isinstance(manifest, dict) or set(manifest) != {
            "schema", "source", "target", "version", "toolchain", "channel",
            "automaticUpdateAuthorized", "files",
        }:
            raise BundleError("candidate manifest fields are invalid")
        if (manifest["schema"] != SCHEMA or manifest["channel"] != "candidate"
                or manifest["automaticUpdateAuthorized"] is not False
                or target not in TARGETS or manifest["target"] != target
                or re.fullmatch(r"[0-9a-f]{40}", source) is None
                or not isinstance(manifest["source"], dict)
                or set(manifest["source"]) != {"repository", "commit", "tree"}
                or manifest["source"]["repository"] != "teamleaderleo/glaeda"
                or manifest["source"]["commit"] != source
                or not isinstance(manifest["source"]["tree"], str)
                or re.fullmatch(r"[0-9a-f]{40}", manifest["source"]["tree"]) is None):
            raise BundleError("candidate source, target or authority mismatch")
        for field in ("version", "toolchain"):
            if not isinstance(manifest[field], str) or not 1 <= len(manifest[field]) <= 512:
                raise BundleError("candidate build metadata is invalid")
        expected_files = {name: {"sha256": sha256(data), "size": len(data)}
                          for name, data in contents.items()}
        if manifest["files"] != expected_files:
            raise BundleError("candidate file inventory mismatch")
        return manifest, contents
    except (tarfile.TarError, EOFError, ValueError, TypeError, KeyError) as error:
        raise BundleError("candidate archive or manifest is malformed") from error


def verify(raw: bytes, expected_sha256: str, source: str, target: str) -> dict:
    return verified_contents(raw, expected_sha256, source, target)[0]


def private_parent(path: Path) -> int:
    """Hold a canonical, non-symlink directory chain, ending in a private root."""
    if not path.is_absolute() or ".." in path.parts or path.resolve(strict=True) != path:
        raise BundleError("generation parent must be an existing canonical absolute directory")
    fd = os.open("/", os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        for part in path.parts[1:]:
            child = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=fd)
            os.close(fd)
            fd = child
            info = os.fstat(fd)
            # Root-owned sticky temporary directories are safe ancestors of a
            # private user-owned directory. Other writable ancestors are refused.
            sticky_root = info.st_uid == 0 and bool(info.st_mode & stat.S_ISVTX)
            if info.st_uid not in (0, os.geteuid()) or (info.st_mode & 0o022 and not sticky_root):
                raise BundleError("generation parent has an untrusted ancestor")
        info = os.fstat(fd)
        if info.st_uid != os.geteuid() or stat.S_IMODE(info.st_mode) != 0o700:
            raise BundleError("generation parent must be owned by the current user with mode 0700")
        return fd
    except BaseException:
        os.close(fd)
        raise


def stage_receipt(destination: Path, digest: str, manifest: dict, state: str) -> dict:
    return {
        "schema": "glaeda-fleet-stage/v1", "state": state,
        "archiveSha256": digest, "source": manifest["source"], "target": manifest["target"],
        "generationDirectory": str(destination),
        "binary": str(destination / "bin/glaeda"),
        "fleetTool": str(destination / "scripts/cmux_fleet.py"),
        "automaticUpdateAuthorized": False,
    }


def generation_files(raw: bytes, manifest: dict, contents: dict[str, bytes]) -> dict[str, bytes]:
    return {**contents, "manifest.json": canonical(manifest), "candidate.tar.gz": raw}


def stage(raw: bytes, digest: str, source: str, target: str, destination: Path, *, apply: bool = False) -> dict:
    manifest, contents = verified_contents(raw, digest, source, target)
    if destination.name in ("", ".", ".."):
        raise BundleError("generation directory must have a new name")
    parent = private_parent(destination.parent)
    generation = None
    directories = {}
    written = {}
    try:
        try:
            os.stat(destination.name, dir_fd=parent, follow_symlinks=False)
        except FileNotFoundError:
            pass
        else:
            raise BundleError("generation directory already exists; choose a new directory")
        result = stage_receipt(destination, digest, manifest, "planned")
        if not apply:
            return result
        # mkdir is the exclusive claim: no existing or interrupted directory is
        # adopted. There is no active pointer and no write to a prior generation.
        os.mkdir(destination.name, 0o700, dir_fd=parent)
        generation = os.open(destination.name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=parent)
        os.fchmod(generation, 0o700)
        os.fsync(parent)
        for name in ("bin", "scripts", "docs"):
            os.mkdir(name, 0o700, dir_fd=generation)
            directories[name] = os.open(name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=generation)
            os.fchmod(directories[name], 0o700)
        for name, data in sorted(generation_files(raw, manifest, contents).items()):
            parts = name.split("/")
            directory = directories[parts[0]] if len(parts) == 2 else generation
            fd = os.open(parts[-1], os.O_RDWR | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600, dir_fd=directory)
            with os.fdopen(fd, "w+b") as stream:
                stream.write(data)
                stream.flush()
                os.fchmod(stream.fileno(), 0o700 if name == "bin/glaeda" else 0o600)
                os.fsync(stream.fileno())
                written[name] = os.fstat(stream.fileno())
                stream.seek(0)
                if stream.read(len(data) + 1) != data:
                    raise BundleError("staged file readback mismatch")
        for fd in directories.values():
            os.fsync(fd)
        # Re-observe the named parent and generation before issuing completion.
        observed = private_parent(destination.parent)
        try:
            fresh, original = os.fstat(observed), os.fstat(parent)
            if (fresh.st_dev, fresh.st_ino) != (original.st_dev, original.st_ino):
                raise BundleError("generation parent moved during staging")
        finally:
            os.close(observed)
        named = os.stat(destination.name, dir_fd=parent, follow_symlinks=False)
        held = os.fstat(generation)
        if ((named.st_dev, named.st_ino) != (held.st_dev, held.st_ino)
                or not stat.S_ISDIR(named.st_mode) or named.st_uid != os.geteuid()
                or stat.S_IMODE(named.st_mode) != 0o700):
            raise BundleError("generation directory moved during staging")
        for name, fd in directories.items():
            named_dir = os.stat(name, dir_fd=generation, follow_symlinks=False)
            held_dir = os.fstat(fd)
            if ((named_dir.st_dev, named_dir.st_ino) != (held_dir.st_dev, held_dir.st_ino)
                    or not stat.S_ISDIR(named_dir.st_mode)
                    or stat.S_IMODE(named_dir.st_mode) != 0o700
                    or named_dir.st_uid != os.geteuid()):
                raise BundleError("staged directory identity changed")
        for name, data in generation_files(raw, manifest, contents).items():
            parts = name.split("/")
            directory = directories[parts[0]] if len(parts) == 2 else generation
            fd = os.open(parts[-1], os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=directory)
            with os.fdopen(fd, "rb") as stream:
                fresh, original = os.fstat(stream.fileno()), written[name]
                if ((fresh.st_dev, fresh.st_ino) != (original.st_dev, original.st_ino)
                        or not stat.S_ISREG(fresh.st_mode) or fresh.st_nlink != 1
                        or fresh.st_uid != os.geteuid() or fresh.st_size != len(data)
                        or stat.S_IMODE(fresh.st_mode) != (0o700 if name == "bin/glaeda" else 0o600)
                        or stream.read(len(data) + 1) != data):
                    raise BundleError("staged file identity or contents changed")
        result["state"] = "staged"
        fd = os.open("stage-receipt.json", os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600, dir_fd=generation)
        with os.fdopen(fd, "wb") as stream:
            stream.write(canonical(result))
            stream.flush()
            os.fsync(stream.fileno())
        os.fsync(generation)
        return result
    finally:
        # Interrupted writes are retained; only a complete receipt reports staging.
        # Never recursively clean paths that another process could have replaced.
        for fd in directories.values():
            os.close(fd)
        if generation is not None:
            os.close(generation)
        os.close(parent)


def file_observation(info: os.stat_result) -> tuple:
    # Reading may update atime. Content/identity changes update mtime or ctime.
    return (info.st_dev, info.st_ino, info.st_mode, info.st_uid, info.st_gid,
            info.st_nlink, info.st_size, info.st_mtime_ns, info.st_ctime_ns)


def read_generation_file(directory: int, name: str, limit: int, mode: int) -> tuple[bytes, os.stat_result]:
    fd = os.open(name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=directory)
    with os.fdopen(fd, "rb") as stream:
        before = os.fstat(stream.fileno())
        if (not stat.S_ISREG(before.st_mode) or before.st_uid != os.geteuid()
                or before.st_nlink != 1 or stat.S_IMODE(before.st_mode) != mode
                or before.st_size > limit):
            raise BundleError("saved generation file has unsafe metadata")
        data = stream.read(limit + 1)
        if len(data) != before.st_size or file_observation(os.fstat(stream.fileno())) != file_observation(before):
            raise BundleError("saved generation file changed during inspection")
        return data, before


def inspect_generation(destination: Path, digest: str, source: str, target: str) -> dict:
    """Reobserve the saved bytes; a staging receipt alone proves no current integrity."""
    parent = private_parent(destination.parent)
    generation = None
    directories = {}
    observations = {}
    directory_observations = {}
    try:
        generation = os.open(destination.name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=parent)
        for name, parent_fd in ((destination.name, parent), ("bin", generation),
                                ("scripts", generation), ("docs", generation)):
            fd = generation if parent_fd == parent else os.open(
                name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=parent_fd)
            if parent_fd != parent:
                directories[name] = fd
            info = os.fstat(fd)
            directory_observations[fd] = file_observation(info)
            if info.st_uid != os.geteuid() or stat.S_IMODE(info.st_mode) != 0o700:
                raise BundleError("saved generation directories must be private and owned by the current user")
        raw, observations["candidate.tar.gz"] = read_generation_file(generation, "candidate.tar.gz", MAX_ARCHIVE, 0o600)
        manifest, contents = verified_contents(raw, digest, source, target)
        expected = generation_files(raw, manifest, contents)
        receipt = stage_receipt(destination, digest, manifest, "staged")
        expected["stage-receipt.json"] = canonical(receipt)
        for name, data in expected.items():
            if name == "candidate.tar.gz":
                continue
            parts = name.split("/")
            directory = directories[parts[0]] if len(parts) == 2 else generation
            observed, observations[name] = read_generation_file(
                directory, parts[-1], len(data), 0o700 if name == "bin/glaeda" else 0o600)
            if observed != data:
                raise BundleError("saved generation differs from the pinned candidate")
        # Enumerate only the four owned directories with an explicit entry cap.
        # Extra code or bytecode cannot silently become part of a checked generation.
        for prefix, fd in {"": generation, **directories}.items():
            required = ({name for name in expected if "/" not in name} | set(directories)
                        if not prefix else {name.split("/")[1] for name in expected if name.startswith(prefix + "/")})
            found = set()
            with os.scandir(fd) as entries:
                for entry in entries:
                    found.add(entry.name)
                    if len(found) > len(required):
                        raise BundleError("saved generation contains unexpected entries")
            if found != required:
                raise BundleError("saved generation inventory changed")
        observed_parent = private_parent(destination.parent)
        try:
            fresh, original = os.fstat(observed_parent), os.fstat(parent)
            if (fresh.st_dev, fresh.st_ino) != (original.st_dev, original.st_ino):
                raise BundleError("saved generation parent moved")
        finally:
            os.close(observed_parent)
        for name, fd, parent_fd in [(destination.name, generation, parent),
                                   *((name, fd, generation) for name, fd in directories.items())]:
            named, held = os.stat(name, dir_fd=parent_fd, follow_symlinks=False), os.fstat(fd)
            if ((named.st_dev, named.st_ino) != (held.st_dev, held.st_ino)
                    or not stat.S_ISDIR(named.st_mode) or named.st_uid != os.geteuid()
                    or stat.S_IMODE(named.st_mode) != 0o700
                    or file_observation(named) != directory_observations[fd]):
                raise BundleError("saved generation directory changed during inspection")
        for name, before in observations.items():
            parts = name.split("/")
            directory = directories[parts[0]] if len(parts) == 2 else generation
            if file_observation(os.stat(parts[-1], dir_fd=directory, follow_symlinks=False)) != file_observation(before):
                raise BundleError("saved generation file changed during inspection")
        return {**receipt, "schema": "glaeda-fleet-generation-check/v1", "state": "verified",
                "authority": "generation_integrity_only"}
    finally:
        for fd in directories.values():
            os.close(fd)
        if generation is not None:
            os.close(generation)
        os.close(parent)


def build(root: Path, expected_source: str, target: str, output: Path) -> dict:
    source = source_identity(root, expected_source)
    toolchain = command(root, ["rustc", "-Vv"]).decode().strip()
    if f"host: {target}" not in toolchain.splitlines() or target not in TARGETS:
        raise BundleError("candidate build requires a supported native target")
    build_root = root / "target/fleet-candidate-build"
    command(root, ["cargo", "build", "--locked", "--release", "--bin", "glaeda", "--target", target, "--target-dir", str(build_root)], capture=False)
    payload = {name: file_bytes(root / name) for name in FILES}
    payload["bin/glaeda"] = file_bytes(build_root / target / "release/glaeda")
    payload["THIRD_PARTY_NOTICES.txt"] = notices(root, target)
    manifest = {
        "schema": SCHEMA, "source": source, "target": target,
        "version": tomllib.loads((root / "Cargo.toml").read_text())["package"]["version"],
        "toolchain": toolchain, "channel": "candidate", "automaticUpdateAuthorized": False,
        "files": {name: {"sha256": sha256(raw), "size": len(raw)} for name, raw in payload.items()},
    }
    if source_identity(root, expected_source) != source:
        raise BundleError("candidate source changed during build")
    raw = archive_bytes(payload, manifest)
    archive_sha256 = sha256(raw)
    verify(raw, archive_sha256, expected_source, target)
    output.mkdir(parents=True, exist_ok=True)
    destination = output / f"glaeda-{expected_source}-{target}.tar.gz"
    with tempfile.NamedTemporaryFile(dir=output, prefix=".candidate.") as stage:
        stage.write(raw)
        stage.flush()
        os.fsync(stage.fileno())
        os.link(stage.name, destination)  # Refuse overwrite, including a dangling symlink.
    return {"archive": destination.name, "sha256": archive_sha256, "source": source, "target": target}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    b = sub.add_parser("build")
    b.add_argument("--output", type=Path, required=True)
    v = sub.add_parser("verify")
    v.add_argument("archive", type=Path)
    v.add_argument("--sha256", required=True)
    st = sub.add_parser("stage", help="preview or prepare a fresh private generation")
    st.add_argument("archive", type=Path)
    st.add_argument("--sha256", required=True)
    st.add_argument("--directory", type=Path, required=True)
    st.add_argument("--apply", action="store_true", help="write the previewed generation")
    inspection = sub.add_parser("inspect", help="verify a saved generation before preflight or rollback")
    inspection.add_argument("--directory", type=Path, required=True)
    inspection.add_argument("--sha256", required=True)
    for p in (b, v, st, inspection):
        p.add_argument("--source", required=True)
        p.add_argument("--target", required=True, choices=sorted(TARGETS))
    args = parser.parse_args()
    try:
        if args.command == "build":
            result = build(Path(__file__).resolve().parents[1], args.source, args.target, args.output)
        elif args.command == "inspect":
            result = inspect_generation(args.directory, args.sha256, args.source, args.target)
        elif args.command == "stage":
            result = stage(file_bytes(args.archive), args.sha256, args.source, args.target, args.directory, apply=args.apply)
        else:
            result = verify(file_bytes(args.archive), args.sha256, args.source, args.target)
        print(canonical(result).decode(), end="")
        return 0
    except (BundleError, OSError) as error:
        print(json.dumps({"error": str(error)}))
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
