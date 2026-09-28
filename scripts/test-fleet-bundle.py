#!/usr/bin/env python3
import importlib.util
import io
import json
import os
import stat
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest
from unittest import mock

SPEC = importlib.util.spec_from_file_location("bundle", Path(__file__).with_name("fleet_bundle.py"))
b = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(b)
SOURCE = "a" * 40
TARGET = "aarch64-apple-darwin"


def fixture():
    payload = {name: b"fixture\n" for name in b.PAYLOAD}
    manifest = {
        "schema": b.SCHEMA,
        "source": {"repository": "teamleaderleo/glaeda", "commit": SOURCE, "tree": "b" * 40},
        "target": TARGET, "version": "0.1.0", "toolchain": "rustc fixture",
        "channel": "candidate", "automaticUpdateAuthorized": False,
        "files": {name: {"sha256": b.sha256(raw), "size": len(raw)} for name, raw in payload.items()},
    }
    return payload, manifest


class BundleTests(unittest.TestCase):
    def test_round_trip_is_deterministic_and_contains_complete_fleet_tools(self):
        payload, manifest = fixture()
        raw = b.archive_bytes(payload, manifest)
        self.assertEqual(raw, b.archive_bytes(payload, manifest))
        self.assertEqual(b.verify(raw, b.sha256(raw), SOURCE, TARGET), manifest)
        self.assertIn("scripts/cmux_fleet_bootstrap.py", manifest["files"])
        self.assertIn("bin/glaeda", manifest["files"])

    def test_digest_source_target_and_inventory_must_match(self):
        payload, manifest = fixture()
        raw = b.archive_bytes(payload, manifest)
        for digest, source, target in (
            ("0" * 64, SOURCE, TARGET), (b.sha256(raw), "c" * 40, TARGET),
            (b.sha256(raw), SOURCE, "x86_64-unknown-linux-gnu"),
        ):
            with self.subTest(source=source, target=target, digest=digest):
                with self.assertRaises(b.BundleError):
                    b.verify(raw, digest, source, target)
        payload["bin/glaeda"] = b"tampered binary"
        raw = b.archive_bytes(payload, manifest)
        with self.assertRaisesRegex(b.BundleError, "inventory"):
            b.verify(raw, b.sha256(raw), SOURCE, TARGET)

    def test_manifest_cannot_grant_update_authority(self):
        for key, value in (("automaticUpdateAuthorized", True), ("channel", "stable"),
                           ("source", []), ("version", {}), ("unexpected", "field")):
            payload, manifest = fixture()
            manifest[key] = value
            raw = b.archive_bytes(payload, manifest)
            with self.subTest(key=key), self.assertRaises(b.BundleError):
                b.verify(raw, b.sha256(raw), SOURCE, TARGET)

    def test_archive_rejects_missing_foreign_duplicate_and_link_entries(self):
        for kind in ("missing", "foreign", "duplicate", "symlink", "hardlink", "mode"):
            payload, manifest = fixture()
            output = io.BytesIO()
            with tarfile.open(fileobj=output, mode="w:gz") as archive:
                members = list({**payload, "manifest.json": b.canonical(manifest)}.items())
                if kind == "missing":
                    members.pop()
                if kind == "foreign":
                    members.append(("../escape", b"bad"))
                if kind == "duplicate":
                    members.append(members[0])
                for index, (name, raw) in enumerate(members):
                    entry = tarfile.TarInfo(name)
                    entry.size = len(raw)
                    entry.mode = 0o755 if name == "bin/glaeda" else 0o644
                    if index == 0 and kind in ("symlink", "hardlink"):
                        entry.type = tarfile.SYMTYPE if kind == "symlink" else tarfile.LNKTYPE
                        entry.linkname = "../escape"
                        entry.size = 0
                    if index == 0 and kind == "mode":
                        entry.mode = 0o4777
                    archive.addfile(entry, io.BytesIO(raw))
            raw = output.getvalue()
            with self.subTest(kind=kind), self.assertRaises(b.BundleError):
                b.verify(raw, b.sha256(raw), SOURCE, TARGET)

    def test_compressed_extended_headers_cannot_bypass_expansion_limit(self):
        payload, manifest = fixture()
        output = io.BytesIO()
        with tarfile.open(fileobj=output, mode="w:gz", format=tarfile.PAX_FORMAT) as archive:
            for name, raw in {**payload, "manifest.json": b.canonical(manifest)}.items():
                entry = tarfile.TarInfo(name)
                entry.size = len(raw)
                entry.mode = 0o755 if name == "bin/glaeda" else 0o644
                entry.pax_headers = {"comment": "x" * 65536}
                archive.addfile(entry, io.BytesIO(raw))
        raw = output.getvalue()
        self.assertLess(len(raw), 16384)
        with mock.patch.object(b, "MAX_TOTAL", 16384):
            with self.assertRaisesRegex(b.BundleError, "expands beyond"):
                b.verify(raw, b.sha256(raw), SOURCE, TARGET)

    def test_stage_preview_and_apply_preserve_previous_generation(self):
        payload, manifest = fixture()
        raw = b.archive_bytes(payload, manifest)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            old = root / "previous"
            old.mkdir()
            (old / "binary").write_bytes(b"previous")
            destination = root / "candidate"
            preview = b.stage(raw, b.sha256(raw), SOURCE, TARGET, destination)
            self.assertEqual(preview["state"], "planned")
            self.assertFalse(destination.exists())
            result = b.stage(raw, b.sha256(raw), SOURCE, TARGET, destination, apply=True)
            self.assertEqual(result["state"], "staged")
            self.assertEqual(json.loads((destination / "stage-receipt.json").read_bytes()), result)
            self.assertFalse(result["automaticUpdateAuthorized"])
            for name, data in payload.items():
                self.assertEqual((destination / name).read_bytes(), data)
            self.assertEqual(stat.S_IMODE((destination / "bin/glaeda").stat().st_mode), 0o700)
            self.assertEqual((old / "binary").read_bytes(), b"previous")
            with self.assertRaisesRegex(b.BundleError, "already exists"):
                b.stage(raw, b.sha256(raw), SOURCE, TARGET, destination, apply=True)

    def test_stage_refuses_bad_digest_before_any_write(self):
        payload, manifest = fixture()
        raw = b.archive_bytes(payload, manifest)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            with self.assertRaises(b.BundleError):
                b.stage(raw, "0" * 64, SOURCE, TARGET, root / "candidate", apply=True)
            self.assertEqual(list(root.iterdir()), [])

    def test_stage_requires_private_canonical_parent_and_fresh_destination(self):
        payload, manifest = fixture()
        raw = b.archive_bytes(payload, manifest)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            private = root / "private"
            private.mkdir(mode=0o700)
            alias = root / "alias"
            alias.symlink_to(private, target_is_directory=True)
            occupied = private / "candidate"
            occupied.symlink_to(root / "missing")
            for destination in (alias / "new", occupied, Path("relative/new")):
                with self.subTest(destination=destination), self.assertRaises((b.BundleError, OSError)):
                    b.stage(raw, b.sha256(raw), SOURCE, TARGET, destination, apply=True)
            private.chmod(0o755)
            with self.assertRaisesRegex(b.BundleError, "0700"):
                b.stage(raw, b.sha256(raw), SOURCE, TARGET, private / "new", apply=True)
            self.assertFalse((private / "new").exists())

    def test_stage_failure_retains_incomplete_directory_without_adopting_it(self):
        payload, manifest = fixture()
        raw = b.archive_bytes(payload, manifest)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            destination = root / "candidate"
            real_fsync = os.fsync
            count = 0
            def fail_during_files(fd):
                nonlocal count
                count += 1
                if count == 3:
                    raise OSError("injected I/O failure")
                return real_fsync(fd)
            with mock.patch.object(b.os, "fsync", side_effect=fail_during_files):
                with self.assertRaises(OSError):
                    b.stage(raw, b.sha256(raw), SOURCE, TARGET, destination, apply=True)
            self.assertTrue(destination.is_dir())
            self.assertFalse((destination / "stage-receipt.json").exists())
            with self.assertRaisesRegex(b.BundleError, "already exists"):
                b.stage(raw, b.sha256(raw), SOURCE, TARGET, destination, apply=True)

    def test_stage_rejects_parent_replacement_before_completion(self):
        payload, manifest = fixture()
        raw = b.archive_bytes(payload, manifest)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            parent = root / "private"
            parent.mkdir(mode=0o700)
            original_open = b.private_parent
            calls = 0
            def replace_parent(path):
                nonlocal calls
                calls += 1
                if calls == 2:
                    parent.rename(root / "moved")
                    parent.mkdir(mode=0o700)
                return original_open(path)
            with mock.patch.object(b, "private_parent", side_effect=replace_parent):
                with self.assertRaisesRegex(b.BundleError, "parent moved"):
                    b.stage(raw, b.sha256(raw), SOURCE, TARGET, parent / "candidate", apply=True)
            self.assertEqual(list(parent.iterdir()), [])
            self.assertFalse((root / "moved/candidate/stage-receipt.json").exists())

    def test_stage_rejects_payload_changes_before_completion(self):
        payload, manifest = fixture()
        raw = b.archive_bytes(payload, manifest)
        for attack in ("directory", "replace", "contents", "mode", "hardlink", "symlink", "generation-mode"):
            with self.subTest(attack=attack), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary).resolve()
                destination = root / "candidate"
                original_open = b.private_parent
                calls = 0
                def change_payload(path):
                    nonlocal calls
                    calls += 1
                    if calls == 2:
                        binary = destination / "bin/glaeda"
                        if attack == "directory":
                            binary.parent.rename(destination / "moved-bin")
                            binary.parent.mkdir(mode=0o700)
                            binary.write_bytes(b"foreign")
                        elif attack == "replace":
                            binary.unlink()
                            binary.write_bytes(b"foreign")
                        elif attack == "contents":
                            binary.write_bytes(b"changed")
                        elif attack == "generation-mode":
                            destination.chmod(0o777)
                        elif attack == "mode":
                            binary.chmod(0o777)
                        elif attack == "hardlink":
                            os.link(binary, root / "alias")
                        elif attack == "symlink":
                            binary.unlink()
                            binary.symlink_to(root / "missing")
                    return original_open(path)
                with mock.patch.object(b, "private_parent", side_effect=change_payload):
                    with self.assertRaises((b.BundleError, OSError)):
                        b.stage(raw, b.sha256(raw), SOURCE, TARGET, destination, apply=True)
                self.assertFalse((destination / "stage-receipt.json").exists())

    def test_saved_generation_inspection_is_repeatable_and_read_only(self):
        payload, manifest = fixture()
        raw = b.archive_bytes(payload, manifest)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            destination = root / "candidate"
            b.stage(raw, b.sha256(raw), SOURCE, TARGET, destination, apply=True)
            before = {str(p.relative_to(root)): p.read_bytes() for p in root.rglob("*") if p.is_file()}
            self.assertEqual((destination / "candidate.tar.gz").read_bytes(), raw)
            first = b.inspect_generation(destination, b.sha256(raw), SOURCE, TARGET)
            self.assertEqual(first, b.inspect_generation(destination, b.sha256(raw), SOURCE, TARGET))
            self.assertEqual(first["state"], "verified")
            self.assertEqual(first["authority"], "generation_integrity_only")
            self.assertFalse(first["automaticUpdateAuthorized"])
            self.assertEqual(before, {str(p.relative_to(root)): p.read_bytes() for p in root.rglob("*") if p.is_file()})

    def test_saved_generation_rejects_damage_and_extra_code(self):
        payload, manifest = fixture()
        raw = b.archive_bytes(payload, manifest)
        for attack in ("archive", "binary", "manifest", "receipt", "missing-archive", "missing-receipt",
                       "file-mode", "directory-mode", "symlink", "hardlink", "extra", "bytecode"):
            with self.subTest(attack=attack), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary).resolve()
                destination = root / "candidate"
                b.stage(raw, b.sha256(raw), SOURCE, TARGET, destination, apply=True)
                paths = {"archive": "candidate.tar.gz", "binary": "bin/glaeda",
                         "manifest": "manifest.json", "receipt": "stage-receipt.json"}
                if attack in paths:
                    (destination / paths[attack]).write_bytes(b"changed")
                elif attack == "missing-archive":
                    (destination / "candidate.tar.gz").unlink()
                elif attack == "missing-receipt":
                    (destination / "stage-receipt.json").unlink()
                elif attack == "file-mode":
                    (destination / "bin/glaeda").chmod(0o777)
                elif attack == "directory-mode":
                    (destination / "scripts").chmod(0o755)
                elif attack == "symlink":
                    (destination / "bin/glaeda").unlink()
                    (destination / "bin/glaeda").symlink_to(root / "missing")
                elif attack == "hardlink":
                    os.link(destination / "bin/glaeda", root / "alias")
                elif attack == "extra":
                    (destination / "scripts/sitecustomize.py").write_bytes(b"foreign")
                elif attack == "bytecode":
                    (destination / "scripts/__pycache__").mkdir()
                with self.assertRaises((b.BundleError, OSError)):
                    b.inspect_generation(destination, b.sha256(raw), SOURCE, TARGET)

    def test_saved_generation_requires_independent_archive_pin(self):
        payload, manifest = fixture()
        original = b.archive_bytes(payload, manifest)
        payload["bin/glaeda"] = b"foreign binary"
        manifest["files"]["bin/glaeda"] = {"sha256": b.sha256(payload["bin/glaeda"]), "size": len(payload["bin/glaeda"])}
        replacement = b.archive_bytes(payload, manifest)
        with tempfile.TemporaryDirectory() as temporary:
            destination = Path(temporary).resolve() / "candidate"
            b.stage(replacement, b.sha256(replacement), SOURCE, TARGET, destination, apply=True)
            with self.assertRaisesRegex(b.BundleError, "digest"):
                b.inspect_generation(destination, b.sha256(original), SOURCE, TARGET)
            for source, target in (("c" * 40, TARGET), (SOURCE, "x86_64-unknown-linux-gnu")):
                with self.assertRaises(b.BundleError):
                    b.inspect_generation(destination, b.sha256(replacement), source, target)

    def test_saved_generation_rejects_changes_during_inspection(self):
        payload, manifest = fixture()
        raw = b.archive_bytes(payload, manifest)
        for attack in ("parent", "generation", "directory", "file", "contents", "extra"):
            with self.subTest(attack=attack), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary).resolve()
                parent = root / "private"
                parent.mkdir(mode=0o700)
                destination = parent / "candidate"
                b.stage(raw, b.sha256(raw), SOURCE, TARGET, destination, apply=True)
                original_open = b.private_parent
                calls = 0
                def mutate(path):
                    nonlocal calls
                    calls += 1
                    if calls == 2:
                        if attack == "parent":
                            parent.rename(root / "moved")
                            parent.mkdir(mode=0o700)
                        elif attack == "generation":
                            destination.rename(parent / "moved")
                            destination.mkdir(mode=0o700)
                        elif attack == "directory":
                            (destination / "bin").rename(destination / "moved-bin")
                            (destination / "bin").mkdir(mode=0o700)
                        elif attack == "file":
                            (destination / "bin/glaeda").unlink()
                            (destination / "bin/glaeda").write_bytes(payload["bin/glaeda"])
                        elif attack == "contents":
                            (destination / "bin/glaeda").write_bytes(b"tampered")
                        elif attack == "extra":
                            (destination / "scripts/extra.py").write_bytes(b"foreign")
                    return original_open(path)
                with mock.patch.object(b, "private_parent", side_effect=mutate):
                    with self.assertRaises((b.BundleError, OSError)):
                        b.inspect_generation(destination, b.sha256(raw), SOURCE, TARGET)

    def test_source_requires_exact_clean_checkout(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            def git(*args):
                return subprocess.check_output(["git", *args], cwd=root, stderr=subprocess.DEVNULL).decode().strip()
            git("init", "-q")
            (root / "source").write_text("source")
            git("add", "source")
            git("-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "commit", "-qm", "fixture")
            source = git("rev-parse", "HEAD")
            self.assertEqual(b.source_identity(root, source)["commit"], source)
            with self.assertRaises(b.BundleError):
                b.source_identity(root, SOURCE)
            (root / "source").write_text("dirty")
            with self.assertRaisesRegex(b.BundleError, "clean"):
                b.source_identity(root, source)


if __name__ == "__main__":
    unittest.main()
