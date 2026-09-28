#!/usr/bin/env python3
"""Export one saved Blender project into Glaeda's portable snapshot input.

This module is intentionally local and read-only with respect to Blender project state. It hashes
already-saved files and writes one explicitly requested JSON document. Cloud/provider transfer and
execution live outside this boundary.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import sys
import tempfile
from pathlib import Path
from typing import Any, Iterable, Sequence


SCHEMA_VERSION = 1
MAX_FILES = 4096
MAX_RELATIVE_PATH_BYTES = 512
MAX_FILE_BYTES = 16 * 1024 * 1024 * 1024 * 1024
CHUNK_BYTES = 1024 * 1024

CACHE_SUFFIXES = {
    ".abc",
    ".bphys",
    ".mdd",
    ".pc2",
    ".usd",
    ".usda",
    ".usdc",
    ".vdb",
}
UNRESOLVED_TOKENS = ("<UDIM>", "<UVTILE>", "#", "{", "}")
FILE_BACKED_IMAGE_SOURCES = {"FILE", "MOVIE", "SEQUENCE", "TILED"}
MULTI_FILE_IMAGE_SOURCES = {"SEQUENCE", "TILED"}
MULTI_FILE_MOVIE_CLIP_SOURCES = {"SEQUENCE"}


class SnapshotExportError(RuntimeError):
    def __init__(self, code: str, message: str) -> None:
        super().__init__(message)
        self.code = code
        self.message = message


def refuse(code: str, message: str) -> SnapshotExportError:
    return SnapshotExportError(code, message)


def blender_cli_arguments(argv: Sequence[str]) -> list[str]:
    try:
        separator = argv.index("--")
    except ValueError:
        return []
    return list(argv[separator + 1 :])


def parse_args(argv: Sequence[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Export a portable Glaeda Blender snapshot")
    parser.add_argument("--project-root", required=True)
    parser.add_argument("--output", required=True)
    return parser.parse_args(list(argv))


def logical_root(path: Path) -> Path:
    root = Path(os.path.abspath(os.fspath(path)))
    if not root.is_dir():
        raise refuse("invalid_project_root", "project root must be an existing directory")
    return root


def is_control_character(character: str) -> bool:
    codepoint = ord(character)
    return codepoint < 32 or codepoint == 127


def portable_file(
    project_root: Path,
    raw_path: str | os.PathLike[str],
) -> tuple[Path, str]:
    raw_text = os.fspath(raw_path)
    if any(token in raw_text for token in UNRESOLVED_TOKENS):
        raise refuse(
            "unresolved_blender_dependency",
            "Blender dependency contains an unresolved multi-file or template token",
        )

    root_real = logical_root(project_root).resolve(strict=True)
    candidate = Path(os.path.abspath(raw_text))
    try:
        real = candidate.resolve(strict=True)
    except OSError as error:
        raise refuse(
            "missing_blender_dependency",
            "Blender dependency is missing or unreadable",
        ) from error

    # Containment is decided once, with both sides canonicalized. The declared project root and the
    # Blender-reported dependency arrive from independent sources and may name the same directory
    # through different symlink spellings -- on macOS `$TMPDIR` lives under `/var`, itself a symlink
    # to `/private/var` -- so comparing an unresolved candidate against an unresolved root refuses
    # paths that are genuinely inside the project. Resolving only one side is the defect; resolving
    # both is identical on Linux, where the root and its canonical form are the same path.
    try:
        relative = real.relative_to(root_real)
    except ValueError as error:
        raise refuse(
            "dependency_outside_project_root",
            "Blender dependency resolves outside the declared project root",
        ) from error

    if not real.is_file():
        raise refuse(
            "invalid_blender_dependency_type",
            "Blender dependency must resolve to one regular file",
        )

    logical = relative.as_posix()
    encoded = logical.encode("utf-8")
    if not logical or len(encoded) > MAX_RELATIVE_PATH_BYTES:
        raise refuse(
            "invalid_blender_relative_path",
            "portable Blender dependency path is empty or oversized",
        )
    if any(part in {"", ".", ".."} for part in relative.parts):
        raise refuse(
            "invalid_blender_relative_path",
            "portable Blender dependency path contains a forbidden segment",
        )
    if any(is_control_character(character) for character in logical):
        raise refuse(
            "invalid_blender_relative_path",
            "portable Blender dependency path contains a control character",
        )
    return real, logical


def sha256_file(path: Path) -> tuple[str, int]:
    try:
        size = path.stat().st_size
    except OSError as error:
        raise refuse(
            "unreadable_blender_dependency",
            "Blender dependency cannot be inspected",
        ) from error
    if size < 0 or size > MAX_FILE_BYTES:
        raise refuse(
            "blender_dependency_too_large",
            "Blender dependency exceeds the bounded file size",
        )

    digest = hashlib.sha256()
    try:
        with path.open("rb") as source:
            while chunk := source.read(CHUNK_BYTES):
                digest.update(chunk)
    except OSError as error:
        raise refuse(
            "unreadable_blender_dependency",
            "Blender dependency cannot be read",
        ) from error
    return f"sha256:{digest.hexdigest()}", size


def classify_dependency(path: Path) -> str:
    suffix = path.suffix.lower()
    if suffix == ".blend":
        return "linked_library"
    if suffix == ".py":
        return "script"
    if suffix in CACHE_SUFFIXES:
        return "baked_cache"
    return "asset"


def make_file_record(
    project_root: Path,
    raw_path: str | os.PathLike[str],
    file_class: str,
) -> dict[str, object]:
    real, logical = portable_file(project_root, raw_path)
    digest, size = sha256_file(real)
    return {
        "relative_path": logical,
        "class": file_class,
        "digest": digest,
        "bytes": size,
    }


def build_snapshot_document(
    project_root: Path,
    runtime_id: str,
    main_scene: str | os.PathLike[str],
    external_paths: Iterable[str | os.PathLike[str]],
) -> dict[str, object]:
    records: dict[str, dict[str, object]] = {}

    main = make_file_record(project_root, main_scene, "scene")
    main_logical = str(main["relative_path"])
    if not main_logical.lower().endswith(".blend"):
        raise refuse(
            "invalid_main_blend",
            "main Blender scene must use a .blend project path",
        )
    records[main_logical] = main

    for raw_path in external_paths:
        real, logical = portable_file(project_root, raw_path)
        record = make_file_record(project_root, real, classify_dependency(real))
        existing = records.get(logical)
        if existing is not None:
            if existing != record:
                raise refuse(
                    "conflicting_blender_dependency",
                    "one Blender project path resolved to conflicting content metadata",
                )
            continue
        records[logical] = record
        if len(records) > MAX_FILES:
            raise refuse(
                "too_many_blender_dependencies",
                "Blender snapshot exceeds the bounded project file count",
            )

    return {
        "schema_version": SCHEMA_VERSION,
        "runtime_id": runtime_id,
        "main_scene": main_logical,
        "files": [records[key] for key in sorted(records)],
    }


def is_packed(data_block: Any) -> bool:
    if getattr(data_block, "packed_file", None) is not None:
        return True
    packed_files = getattr(data_block, "packed_files", None)
    if packed_files is None:
        return False
    try:
        return len(packed_files) > 0
    except TypeError:
        return bool(tuple(packed_files))


def refuse_unexpanded_multi_file_dependencies(bpy_module: Any) -> None:
    data = bpy_module.data
    for image in getattr(data, "images", ()):
        source = str(getattr(image, "source", ""))
        if source in MULTI_FILE_IMAGE_SOURCES and not is_packed(image):
            raise refuse(
                "unresolved_blender_dependency",
                "Blender image sequence or tiled image requires exact expanded-member enumeration",
            )
    for movie_clip in getattr(data, "movieclips", ()):
        if str(getattr(movie_clip, "source", "")) in MULTI_FILE_MOVIE_CLIP_SOURCES:
            raise refuse(
                "unresolved_blender_dependency",
                "Blender movie-clip sequence requires exact expanded-member enumeration",
            )
    for cache_file in getattr(data, "cache_files", ()):
        if bool(getattr(cache_file, "is_sequence", False)):
            raise refuse(
                "unresolved_blender_dependency",
                "Blender cache-file sequence requires exact expanded-member enumeration",
            )
    for volume in getattr(data, "volumes", ()):
        if bool(getattr(volume, "is_sequence", False)):
            raise refuse(
                "unresolved_blender_dependency",
                "Blender volume sequence requires exact expanded-member enumeration",
            )


def refuse_dirty_external_data(bpy_module: Any) -> None:
    for image in getattr(bpy_module.data, "images", ()):
        source = str(getattr(image, "source", ""))
        filepath = str(getattr(image, "filepath", ""))
        if (
            source in FILE_BACKED_IMAGE_SOURCES
            and bool(filepath)
            and getattr(image, "is_dirty", False)
            and not is_packed(image)
        ):
            raise refuse(
                "dirty_external_blender_dependency",
                "an external Blender image has unsaved in-memory changes",
            )
    for text in getattr(bpy_module.data, "texts", ()):
        if (
            getattr(text, "is_dirty", False)
            and bool(getattr(text, "filepath", ""))
            and not getattr(text, "is_in_memory", False)
        ):
            raise refuse(
                "dirty_external_blender_dependency",
                "an external Blender text dependency has unsaved in-memory changes",
            )


def blender_runtime_id(bpy_module: Any) -> str:
    version = tuple(int(value) for value in bpy_module.app.version[:3])
    if len(version) != 3 or any(value < 0 for value in version):
        raise refuse(
            "invalid_blender_runtime",
            "Blender runtime version is unavailable",
        )
    return f"blender-{version[0]}.{version[1]}.{version[2]}"


def export_from_bpy(bpy_module: Any, project_root: Path) -> dict[str, object]:
    data = bpy_module.data
    main_scene = str(getattr(data, "filepath", ""))
    if not getattr(data, "is_saved", False) or not main_scene:
        raise refuse(
            "unsaved_main_blend",
            "Blender project must be saved before snapshot export",
        )
    if getattr(data, "is_dirty", False):
        raise refuse(
            "dirty_main_blend",
            "Blender project has unsaved edits and cannot be represented by current file bytes",
        )

    refuse_unexpanded_multi_file_dependencies(bpy_module)
    refuse_dirty_external_data(bpy_module)
    try:
        external_paths = bpy_module.utils.blend_paths(
            absolute=True,
            packed=False,
            local=False,
        )
    except Exception as error:
        raise refuse(
            "blender_dependency_enumeration_failed",
            "Blender could not enumerate external project dependencies",
        ) from error

    return build_snapshot_document(
        project_root,
        blender_runtime_id(bpy_module),
        main_scene,
        external_paths,
    )


def write_private_json(output: Path, document: dict[str, object]) -> None:
    parent = output.parent
    if not parent.is_dir():
        raise refuse(
            "invalid_snapshot_output_parent",
            "snapshot output parent must already exist",
        )
    temporary: Path | None = None
    try:
        with tempfile.NamedTemporaryFile(
            mode="w",
            encoding="utf-8",
            dir=parent,
            prefix=".glaeda-blender-snapshot-",
            suffix=".json.tmp",
            delete=False,
        ) as destination:
            temporary = Path(destination.name)
            json.dump(document, destination, sort_keys=True, separators=(",", ":"))
            destination.write("\n")
            destination.flush()
            os.fsync(destination.fileno())
        os.chmod(temporary, 0o600)
        os.replace(temporary, output)
        temporary = None
    except OSError as error:
        raise refuse(
            "snapshot_output_failed",
            "portable Blender snapshot could not be written",
        ) from error
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)


def error_document(error: SnapshotExportError) -> dict[str, object]:
    return {
        "schema_version": SCHEMA_VERSION,
        "status": "refused",
        "code": error.code,
        "message": error.message,
    }


def main(argv: Sequence[str] | None = None) -> int:
    if argv is None:
        argv = blender_cli_arguments(sys.argv)
    args = parse_args(argv)
    try:
        import bpy  # type: ignore[import-not-found]

        document = export_from_bpy(bpy, Path(args.project_root))
        write_private_json(Path(args.output), document)
    except SnapshotExportError as error:
        print(json.dumps(error_document(error), sort_keys=True), file=sys.stderr)
        return 2
    except ModuleNotFoundError:
        error = refuse(
            "blender_runtime_required",
            "snapshot exporter must run inside Blender's Python runtime",
        )
        print(json.dumps(error_document(error), sort_keys=True), file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
