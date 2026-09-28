#!/usr/bin/env python3
"""Deterministic tests for scripts/verify-changed-rustfmt."""

from __future__ import annotations

import os
import runpy
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
HELPER = ROOT / "scripts" / "verify-changed-rustfmt"
MODULE = runpy.run_path(str(HELPER), run_name="glaeda_changed_rustfmt_test")


class ChangedRustfmtTests(unittest.TestCase):
    def fixture(self) -> tuple[tempfile.TemporaryDirectory[str], Path, Path]:
        temporary = tempfile.TemporaryDirectory()
        root = Path(temporary.name)
        subprocess.run(["git", "init", "--quiet"], cwd=root, check=True)
        (root / "src").mkdir()
        (root / "tests").mkdir()
        (root / "src" / "changed.rs").write_text("pub fn changed() {}\n", encoding="utf-8")
        (root / "src" / "deleted.rs").write_text("pub fn deleted() {}\n", encoding="utf-8")
        (root / "tests" / "renamed.rs").write_text("fn renamed() {}\n", encoding="utf-8")
        (root / "notes.txt").write_text("base\n", encoding="utf-8")
        subprocess.run(["git", "add", "."], cwd=root, check=True)
        subprocess.run(
            [
                "git",
                "-c",
                "user.name=Glaeda test",
                "-c",
                "user.email=glaeda-test@example.invalid",
                "commit",
                "--quiet",
                "-m",
                "fixture",
            ],
            cwd=root,
            check=True,
        )
        found_git = shutil.which("git")
        self.assertIsNotNone(found_git)
        git = Path(os.path.abspath(found_git or "git"))
        return temporary, root, git

    def test_changed_files_cover_tracked_untracked_and_renamed_but_not_deleted(self) -> None:
        temporary, root, git = self.fixture()
        with temporary:
            (root / "src" / "changed.rs").write_text("pub fn changed() { }\n", encoding="utf-8")
            (root / "src" / "deleted.rs").unlink()
            subprocess.run(
                ["git", "mv", "tests/renamed.rs", "tests/space name.rs"], cwd=root, check=True
            )
            untracked = root / "tests" / "line\nbreak.rs"
            untracked.write_text("fn untracked() {}\n", encoding="utf-8")
            (root / "notes.txt").write_text("dirty\n", encoding="utf-8")

            self.assertEqual(
                MODULE["changed_rust_files"](root, git),
                (
                    Path("src/changed.rs"),
                    Path("tests/line\nbreak.rs"),
                    Path("tests/space name.rs"),
                ),
            )

    def test_symlink_rust_file_fails_closed(self) -> None:
        temporary, root, git = self.fixture()
        with temporary:
            outside = root.parent / f"{root.name}-outside.rs"
            outside.write_text("fn outside() {}\n", encoding="utf-8")
            try:
                (root / "tests" / "link.rs").symlink_to(outside)
                with self.assertRaisesRegex(RuntimeError, "regular non-symlink"):
                    MODULE["changed_rust_files"](root, git)
            finally:
                outside.unlink(missing_ok=True)

    def test_tracked_rust_type_change_fails_closed(self) -> None:
        temporary, root, git = self.fixture()
        with temporary:
            outside = root.parent / f"{root.name}-outside.rs"
            outside.write_text("fn outside() {}\n", encoding="utf-8")
            try:
                (root / "src" / "changed.rs").unlink()
                (root / "src" / "changed.rs").symlink_to(outside)
                with self.assertRaisesRegex(RuntimeError, "unsafe type change"):
                    MODULE["changed_rust_files"](root, git)
            finally:
                outside.unlink(missing_ok=True)

    def test_rustfmt_accepts_formatted_and_rejects_malformed_input(self) -> None:
        temporary, root, _git = self.fixture()
        with temporary:
            rustfmt = MODULE["capability"]("rustfmt")
            formatted = Path("src/changed.rs")
            self.assertEqual(MODULE["run_rustfmt"](root, rustfmt, (formatted,)), 0)
            (root / formatted).write_text("pub fn broken( {\n", encoding="utf-8")
            self.assertNotEqual(MODULE["run_rustfmt"](root, rustfmt, (formatted,)), 0)

    def test_batches_are_bounded_and_preserve_order(self) -> None:
        files = tuple(Path(f"src/{index:04d}.rs") for index in range(1_024))
        batches = MODULE["rustfmt_batches"](files)
        self.assertEqual(tuple(path for batch in batches for path in batch), files)
        self.assertTrue(all(batch for batch in batches))


if __name__ == "__main__":
    unittest.main()
