#!/usr/bin/env python3
"""Deterministic tests for scripts/blender_snapshot_export.py."""

from __future__ import annotations

import json
import runpy
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace


ROOT = Path(__file__).resolve().parent.parent
HELPER = ROOT / "scripts" / "blender_snapshot_export.py"
MODULE = runpy.run_path(str(HELPER), run_name="glaeda_blender_snapshot_export_test")

SnapshotExportError = MODULE["SnapshotExportError"]
build_snapshot_document = MODULE["build_snapshot_document"]
export_from_bpy = MODULE["export_from_bpy"]
write_private_json = MODULE["write_private_json"]


class FakeUtils:
    def __init__(self, paths: list[str]) -> None:
        self.paths = paths
        self.calls: list[dict[str, object]] = []

    def blend_paths(self, **kwargs: object) -> list[str]:
        self.calls.append(kwargs)
        return list(self.paths)


class FakeBpy:
    def __init__(
        self,
        main_scene: Path,
        dependencies: list[Path],
        *,
        saved: bool = True,
        dirty: bool = False,
        images: tuple[object, ...] = (),
        texts: tuple[object, ...] = (),
        movieclips: tuple[object, ...] = (),
        cache_files: tuple[object, ...] = (),
        volumes: tuple[object, ...] = (),
    ) -> None:
        self.data = SimpleNamespace(
            filepath=str(main_scene) if saved else "",
            is_saved=saved,
            is_dirty=dirty,
            images=images,
            texts=texts,
            movieclips=movieclips,
            cache_files=cache_files,
            volumes=volumes,
        )
        self.app = SimpleNamespace(version=(5, 2, 0))
        self.utils = FakeUtils([str(path) for path in dependencies])


class BlenderSnapshotExportTests(unittest.TestCase):
    def fixture(self) -> tuple[tempfile.TemporaryDirectory[str], Path, Path]:
        temporary = tempfile.TemporaryDirectory()
        root = Path(temporary.name)
        scenes = root / "scenes"
        scenes.mkdir()
        main_scene = scenes / "main.blend"
        main_scene.write_bytes(b"blend-main-v1")
        return temporary, root, main_scene

    def test_build_document_hashes_and_classifies_portable_files(self) -> None:
        temporary, root, main_scene = self.fixture()
        with temporary:
            assets = root / "assets"
            libraries = root / "libraries"
            cache = root / "cache"
            scripts = root / "scripts"
            for directory in (assets, libraries, cache, scripts):
                directory.mkdir()
            image = assets / "wood.exr"
            library = libraries / "character.blend"
            alembic = cache / "sim.abc"
            script = scripts / "driver.py"
            image.write_bytes(b"image")
            library.write_bytes(b"library")
            alembic.write_bytes(b"cache")
            script.write_bytes(b"script")

            document = build_snapshot_document(
                root,
                "blender-5.2.0",
                main_scene,
                [image, library, alembic, script],
            )

            self.assertEqual(document["schema_version"], 1)
            self.assertEqual(document["runtime_id"], "blender-5.2.0")
            self.assertEqual(document["main_scene"], "scenes/main.blend")
            files = {entry["relative_path"]: entry for entry in document["files"]}
            self.assertEqual(files["scenes/main.blend"]["class"], "scene")
            self.assertEqual(files["assets/wood.exr"]["class"], "asset")
            self.assertEqual(files["libraries/character.blend"]["class"], "linked_library")
            self.assertEqual(files["cache/sim.abc"]["class"], "baked_cache")
            self.assertEqual(files["scripts/driver.py"]["class"], "script")
            for entry in files.values():
                self.assertRegex(entry["digest"], r"^sha256:[0-9a-f]{64}$")
                self.assertGreater(entry["bytes"], 0)

    def test_blender_adapter_requests_absolute_unpacked_linked_paths(self) -> None:
        temporary, root, main_scene = self.fixture()
        with temporary:
            asset = root / "asset.png"
            library = root / "library.blend"
            packed = root / "packed.png"
            asset.write_bytes(b"asset")
            library.write_bytes(b"library")
            packed.write_bytes(b"packed")
            bpy = FakeBpy(main_scene, [asset, library])

            document = export_from_bpy(bpy, root)

            self.assertEqual(
                bpy.utils.calls,
                [{"absolute": True, "packed": False, "local": False}],
            )
            paths = {entry["relative_path"] for entry in document["files"]}
            self.assertEqual(paths, {"scenes/main.blend", "asset.png", "library.blend"})
            self.assertNotIn("packed.png", paths)

    def test_dirty_or_unsaved_main_blend_refuses(self) -> None:
        temporary, root, main_scene = self.fixture()
        with temporary:
            with self.assertRaises(SnapshotExportError) as dirty:
                export_from_bpy(FakeBpy(main_scene, [], dirty=True), root)
            self.assertEqual(dirty.exception.code, "dirty_main_blend")

            with self.assertRaises(SnapshotExportError) as unsaved:
                export_from_bpy(FakeBpy(main_scene, [], saved=False), root)
            self.assertEqual(unsaved.exception.code, "unsaved_main_blend")

    def test_dirty_external_file_backed_image_refuses_but_generated_image_does_not(self) -> None:
        temporary, root, main_scene = self.fixture()
        with temporary:
            dirty_file = SimpleNamespace(
                is_dirty=True,
                source="FILE",
                filepath="//asset.png",
                packed_file=None,
                packed_files=(),
            )
            with self.assertRaises(SnapshotExportError) as refused:
                export_from_bpy(FakeBpy(main_scene, [], images=(dirty_file,)), root)
            self.assertEqual(refused.exception.code, "dirty_external_blender_dependency")

            generated = SimpleNamespace(
                is_dirty=True,
                source="GENERATED",
                filepath="",
                packed_file=None,
                packed_files=(),
            )
            document = export_from_bpy(FakeBpy(main_scene, [], images=(generated,)), root)
            self.assertEqual(len(document["files"]), 1)

    def test_multi_file_blender_sources_fail_closed_until_expanded_enumeration(self) -> None:
        temporary, root, main_scene = self.fixture()
        with temporary:
            cases = [
                {
                    "images": (
                        SimpleNamespace(
                            source="SEQUENCE",
                            packed_file=None,
                            packed_files=(),
                            filepath="//frames/frame_####.png",
                            is_dirty=False,
                        ),
                    )
                },
                {
                    "images": (
                        SimpleNamespace(
                            source="TILED",
                            packed_file=None,
                            packed_files=(),
                            filepath="//textures/skin.<UDIM>.exr",
                            is_dirty=False,
                        ),
                    )
                },
                {"movieclips": (SimpleNamespace(source="SEQUENCE"),)},
                {"cache_files": (SimpleNamespace(is_sequence=True),)},
                {"volumes": (SimpleNamespace(is_sequence=True),)},
            ]
            for kwargs in cases:
                with self.subTest(kwargs=tuple(kwargs)):
                    bpy = FakeBpy(main_scene, [], **kwargs)
                    with self.assertRaises(SnapshotExportError) as refused:
                        export_from_bpy(bpy, root)
                    self.assertEqual(refused.exception.code, "unresolved_blender_dependency")
                    self.assertEqual(bpy.utils.calls, [])

            packed_tiled = SimpleNamespace(
                source="TILED",
                packed_file=object(),
                packed_files=(),
                filepath="//textures/skin.<UDIM>.exr",
                is_dirty=False,
            )
            document = export_from_bpy(
                FakeBpy(main_scene, [], images=(packed_tiled,)),
                root,
            )
            self.assertEqual(len(document["files"]), 1)

    def test_missing_outside_and_tokenized_dependencies_fail_closed(self) -> None:
        temporary, root, main_scene = self.fixture()
        with temporary:
            outside = root.parent / f"{root.name}-outside.exr"
            outside.write_bytes(b"outside")
            try:
                with self.assertRaises(SnapshotExportError) as outside_error:
                    build_snapshot_document(root, "blender-5.2.0", main_scene, [outside])
                self.assertEqual(
                    outside_error.exception.code,
                    "dependency_outside_project_root",
                )

                with self.assertRaises(SnapshotExportError) as missing_error:
                    build_snapshot_document(
                        root,
                        "blender-5.2.0",
                        main_scene,
                        [root / "missing.exr"],
                    )
                self.assertEqual(missing_error.exception.code, "missing_blender_dependency")

                with self.assertRaises(SnapshotExportError) as token_error:
                    build_snapshot_document(
                        root,
                        "blender-5.2.0",
                        main_scene,
                        [root / "textures" / "skin.<UDIM>.exr"],
                    )
                self.assertEqual(token_error.exception.code, "unresolved_blender_dependency")
            finally:
                outside.unlink(missing_ok=True)

    def test_symlinked_project_root_spelling_does_not_change_the_result(self) -> None:
        """The declared root and the Blender path may spell one directory differently.

        macOS produces this on every run, because `$TMPDIR` lives under `/var`, itself a symlink to
        `/private/var`. Exercising it through an explicit symlink keeps the regression observable on
        Linux, where no such system symlink exists.
        """
        temporary, root, main_scene = self.fixture()
        with temporary:
            asset = root / "asset.exr"
            asset.write_bytes(b"asset")
            link = root.parent / f"{root.name}-link"
            link.symlink_to(root, target_is_directory=True)
            try:
                through_link = build_snapshot_document(
                    link,
                    "blender-5.2.0",
                    main_scene,
                    [asset],
                )
                self.assertEqual(
                    {entry["relative_path"] for entry in through_link["files"]},
                    {"scenes/main.blend", "asset.exr"},
                )

                through_root = build_snapshot_document(
                    root,
                    "blender-5.2.0",
                    link / "scenes" / "main.blend",
                    [link / "asset.exr"],
                )
                self.assertEqual(
                    {entry["relative_path"] for entry in through_root["files"]},
                    {"scenes/main.blend", "asset.exr"},
                )
            finally:
                link.unlink()

    def test_symlink_escaping_the_project_root_still_fails_closed(self) -> None:
        temporary, root, main_scene = self.fixture()
        with temporary:
            outside = root.parent / f"{root.name}-outside.exr"
            outside.write_bytes(b"outside")
            escape = root / "escape.exr"
            escape.symlink_to(outside)
            try:
                with self.assertRaises(SnapshotExportError) as escaped:
                    build_snapshot_document(root, "blender-5.2.0", main_scene, [escape])
                self.assertEqual(escaped.exception.code, "dependency_outside_project_root")
            finally:
                outside.unlink(missing_ok=True)

    def test_unicode_relative_path_is_preserved(self) -> None:
        temporary, root, main_scene = self.fixture()
        with temporary:
            directory = root / "资产" / "角色"
            directory.mkdir(parents=True)
            texture = directory / "皮肤.exr"
            texture.write_bytes(b"texture")
            document = build_snapshot_document(
                root,
                "blender-5.2.0",
                main_scene,
                [texture],
            )
            paths = [entry["relative_path"] for entry in document["files"]]
            self.assertIn("资产/角色/皮肤.exr", paths)

    def test_duplicate_dependency_is_emitted_once(self) -> None:
        temporary, root, main_scene = self.fixture()
        with temporary:
            texture = root / "texture.exr"
            texture.write_bytes(b"texture")
            document = build_snapshot_document(
                root,
                "blender-5.2.0",
                main_scene,
                [texture, texture],
            )
            paths = [entry["relative_path"] for entry in document["files"]]
            self.assertEqual(paths.count("texture.exr"), 1)

    def test_serialized_document_exposes_no_absolute_paths(self) -> None:
        temporary, root, main_scene = self.fixture()
        with temporary:
            asset = root / "asset.exr"
            asset.write_bytes(b"asset")
            document = build_snapshot_document(
                root,
                "blender-5.2.0",
                main_scene,
                [asset],
            )
            encoded = json.dumps(document, ensure_ascii=False)
            self.assertNotIn(str(root), encoded)
            self.assertNotIn("/home/", encoded)
            self.assertNotIn("/Users/", encoded)

    def test_private_json_write_is_atomic_and_mode_0600(self) -> None:
        temporary, root, main_scene = self.fixture()
        with temporary:
            document = build_snapshot_document(
                root,
                "blender-5.2.0",
                main_scene,
                [],
            )
            output = root / "snapshot.json"
            write_private_json(output, document)
            self.assertEqual(json.loads(output.read_text(encoding="utf-8")), document)
            self.assertEqual(output.stat().st_mode & 0o777, 0o600)


if __name__ == "__main__":
    unittest.main()
