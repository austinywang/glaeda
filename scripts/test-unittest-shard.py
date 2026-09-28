#!/usr/bin/env python3
"""Tests for scripts/unittest-shard: the shards of a file run every test exactly once."""

from __future__ import annotations

import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SHARD = Path(__file__).resolve().parent / "unittest-shard"

SAMPLE = '''
import unittest
from pathlib import Path

LOG = Path(__file__).with_name("ran.log")


class A(unittest.TestCase):
''' + "".join(f"    def test_a{n}(self):\n        LOG.open('a').write('A.test_a{n}\\n')\n" for n in range(7)) + '''

class B(unittest.TestCase):
''' + "".join(f"    def test_b{n}(self):\n        LOG.open('a').write('B.test_b{n}\\n')\n" for n in range(4)) + '''

if __name__ == "__main__":
    unittest.main()
'''


class UnittestShardTest(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self.tmp.name)
        self.file = self.dir / "test-sample.py"
        self.file.write_text(SAMPLE)

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def shard(self, *args: str) -> subprocess.CompletedProcess:
        return subprocess.run([sys.executable, str(SHARD), *args], capture_output=True, text=True, timeout=60)

    def test_shards_partition_the_tests(self) -> None:
        for count in (1, 3, 11, 13):
            (self.dir / "ran.log").unlink(missing_ok=True)
            for index in range(count):
                result = self.shard(str(self.file), str(index), str(count))
                self.assertEqual(result.returncode, 0, result.stderr)
            ran = (self.dir / "ran.log").read_text().split()
            self.assertEqual(sorted(ran), sorted([f"A.test_a{n}" for n in range(7)] + [f"B.test_b{n}" for n in range(4)]),
                             f"{count} shards run each test once")

    def test_a_failure_fails_its_shard(self) -> None:
        self.file.write_text(SAMPLE.replace("def test_b0(self):\n", "def test_b0(self):\n        self.fail('x')\n"))
        codes = {self.shard(str(self.file), str(i), "2").returncode for i in range(2)}
        self.assertEqual(codes, {0, 1})

    def test_a_file_without_tests_fails(self) -> None:
        self.file.write_text("import unittest\n")
        empty = self.shard(str(self.file), "0", "1")
        self.assertEqual(empty.returncode, 1)
        self.assertIn("defines no tests", empty.stderr)

    def test_bad_arguments_and_class_fixtures_are_refused(self) -> None:
        for args in ([], [str(self.file), "2", "2"], [str(self.file), "0", "0"], [str(self.dir / "none.py"), "0", "1"],
                     [str(self.file), "x", "1"]):
            self.assertEqual(self.shard(*args).returncode, 2, args)
        self.file.write_text(SAMPLE.replace("class B(unittest.TestCase):\n",
                                            "class B(unittest.TestCase):\n    @classmethod\n"
                                            "    def setUpClass(cls):\n        pass\n\n"))
        refused = self.shard(str(self.file), "0", "2")
        self.assertEqual(refused.returncode, 2)
        self.assertIn("B share a class fixture", refused.stderr)


if __name__ == "__main__":
    unittest.main()
