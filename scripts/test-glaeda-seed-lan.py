#!/usr/bin/env python3
"""Tests for scripts/glaeda-seed-serve (the seeder's forced command) and scripts/glaeda-seed-lan (the installer)."""

from __future__ import annotations

import base64
import contextlib
import fcntl
import hashlib
import importlib.machinery
import importlib.util
import io
import json
import os
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time
import unittest
import unittest.mock
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SERVE = ROOT / "scripts/glaeda-seed-serve"


def load(name: str, path: Path):
    loader = importlib.machinery.SourceFileLoader(name, os.fspath(path))
    spec = importlib.util.spec_from_loader(name, loader)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    loader.exec_module(module)
    return module


serve = load("glaeda_seed_serve", SERVE)
lan = load("glaeda_seed_lan", ROOT / "scripts/glaeda-seed-lan")

KEY = "admission-derived-data-v1-macOS-ARM64-" + "a" * 32 + "-j14-" + "b" * 40
OTHER = "admission-derived-data-v1-macOS-ARM64-" + "a" * 32 + "-j14-" + "c" * 40


class ServeTest(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        base = Path(self.tmp.name)
        self.state = base / "ci"
        (self.state / "seeds").mkdir(parents=True)
        self.run_dir = base / "run"
        self.receipt = base / "receipt.json"
        self.receipt.write_text(json.dumps({"member": {"trustedRef": "refs/heads/main"}}))

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def keep(self, key: str, root: str = "", manifest: bool = True) -> Path:
        seed = self.state / root / "seeds" / key
        (seed / "Build").mkdir(parents=True)
        (seed / "Build/x").write_text("x")
        if manifest:
            (seed / serve.MANIFEST).write_text("{}")
        return seed

    def ask(self, request: str | None, slot_wait: float = 60) -> tuple[int, bytes, str]:
        env = {k: v for k, v in os.environ.items() if k != "SSH_ORIGINAL_COMMAND"}
        if request is not None:
            env["SSH_ORIGINAL_COMMAND"] = request
        env["SSH_CLIENT"] = "172.20.21.196 50000 22"
        proc = subprocess.run([sys.executable, os.fspath(SERVE), "--state", os.fspath(self.state), "--run-dir",
                               os.fspath(self.run_dir), "--receipt", os.fspath(self.receipt), "--slot-wait",
                               str(slot_wait)], env=env, capture_output=True, timeout=60)
        return proc.returncode, proc.stdout, proc.stderr.decode()

    def header(self, out: bytes) -> tuple[str, bytes]:
        line, _, rest = out.partition(b"\n")
        self.assertTrue(line.startswith(b"glaeda-seed-serve 1 "), out[:200])
        return line.decode()[len("glaeda-seed-serve 1 "):], rest

    def test_only_well_formed_seed_keys_are_accepted(self):
        self.keep(KEY)
        bad = [
            "", "sh", "seed-v1", f"seed-v1 zstd", f"seed-v1 gzip {KEY}", f"seed-v2 tar {KEY}",
            f"seed-v1 tar ../{KEY}", f"seed-v1 tar {KEY}/..", f"seed-v1 tar /etc/passwd",
            f"seed-v1 tar {KEY};id", f"seed-v1 tar {KEY} $(id)", f"seed-v1 tar -C/ {KEY}", f"seed-v1 tar .{KEY}",
            f"seed-v1  tar {KEY}", f"seed-v1 tar {KEY}\nid", f"seed-v1 tar {KEY.upper()}", f"seed-v1 tar {KEY} {KEY}",
            "seed-v1 tar " + " ".join([KEY[:-3] + f"{n:03x}" for n in range(serve.MAX_KEYS + 1)]),
            f"seed-v1 tar {KEY}é", "ping-v1 extra",
        ]
        for request in bad:
            with self.subTest(request=request[:60]):
                code, out, _ = self.ask(request)
                self.assertEqual(code, 2)
                self.assertTrue(self.header(out)[0].startswith("refused"))
                self.assertEqual(self.header(out)[1], b"")
        self.assertEqual(self.ask(None)[0], 2)  # not run as a forced command

    def test_an_untrusted_host_refuses_everything(self):
        self.keep(KEY)
        for receipt in ("", "{}", json.dumps({"member": {"trustedRef": ""}}), json.dumps({"member": None})):
            with self.subTest(receipt=receipt):
                self.receipt.write_text(receipt)
                for request in (f"seed-v1 tar {KEY}", "ping-v1"):
                    code, out, _ = self.ask(request)
                    self.assertEqual((code, self.header(out)), (2, ("refused this-host-is-not-a-trusted-only-seeder", b"")))
        self.receipt.unlink()
        self.assertEqual(self.ask("ping-v1")[0], 2)

    def test_ping_and_miss(self):
        self.assertEqual(self.ask("ping-v1")[:2], (0, b"glaeda-seed-serve 1 pong\n"))
        self.assertEqual(self.ask(f"seed-v1 tar {KEY}")[:2], (3, b"glaeda-seed-serve 1 miss\n"))

    def test_the_nearest_kept_seed_streams_as_tar(self):
        self.keep(OTHER, "cmux-ci-2")
        code, out, _ = self.ask(f"seed-v1 tar {KEY} {OTHER}")
        self.assertEqual(code, 0)
        head, body = self.header(out)
        self.assertEqual(head, f"hit {OTHER} tar")
        # macOS bsdtar adds AppleDouble ._ entries for extended attributes, as R2's seeds carry them.
        names = [n for n in tarfile.open(fileobj=io.BytesIO(body)).getnames() if not n.split("/")[-1].startswith("._")]
        self.assertIn(f"{OTHER}/{serve.MANIFEST}", names)
        self.assertTrue(all(n == OTHER or n.startswith(OTHER + "/") for n in names), names)
        self.keep(KEY)
        self.assertEqual(self.header(self.ask(f"seed-v1 tar {KEY} {OTHER}")[1])[0], f"hit {KEY} tar")
        log = [json.loads(line) for line in (self.run_dir / "serve.jsonl").read_text().splitlines()]
        self.assertEqual([(r["outcome"], r.get("distance")) for r in log], [("served", 1), ("served", 0)])
        self.assertEqual(log[0]["client"], "172.20.21.196")

    def test_zstd_is_used_when_the_seeder_has_it(self):
        self.keep(KEY)
        code, out, _ = self.ask(f"seed-v1 zstd {KEY}")
        head, body = self.header(out)
        zstd = serve.zstd_tool()
        self.assertEqual((code, head), (0, f"hit {KEY} {'zstd' if zstd else 'tar'}"))
        if zstd:
            body = subprocess.run([zstd, "-d", "-c"], input=body, capture_output=True, check=True).stdout
        self.assertIn(f"{KEY}/{serve.MANIFEST}", tarfile.open(fileobj=io.BytesIO(body)).getnames())

    def test_symlinks_and_incomplete_seeds_are_not_served(self):
        real = self.keep(OTHER)
        (self.state / "seeds" / KEY).symlink_to(real)
        self.assertEqual(self.ask(f"seed-v1 tar {KEY}")[0], 3)
        (self.state / "seeds" / KEY).unlink()
        seed = self.keep(KEY, manifest=False)
        (seed / serve.MANIFEST).symlink_to("/etc/hosts")
        self.assertEqual(self.ask(f"seed-v1 tar {KEY}")[0], 3)
        # A symlinked root seed cache does not count either.
        shutil.rmtree(self.state / "seeds")
        elsewhere = Path(self.tmp.name) / "elsewhere/seeds"
        elsewhere.mkdir(parents=True)
        (self.state / "cmux-ci-3").symlink_to(elsewhere.parent)
        self.keep(KEY, "../elsewhere")
        self.assertEqual(self.ask(f"seed-v1 tar {KEY}")[0], 3)

    def test_at_most_two_serves_run_at_once(self):
        self.keep(KEY)
        self.run_dir.mkdir()
        held = []
        for slot in range(serve.SLOTS):
            fd = os.open(self.run_dir / f"slot-{slot}.lock", os.O_RDONLY | os.O_CREAT, 0o600)
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            held.append(fd)
        try:
            code, out, _ = self.ask(f"seed-v1 tar {KEY}", slot_wait=0.5)
            self.assertEqual((code, out), (4, b"glaeda-seed-serve 1 busy\n"))
            os.close(held.pop())  # one slot frees up
            self.assertEqual(self.ask(f"seed-v1 tar {KEY}", slot_wait=0.5)[0], 0)
        finally:
            for fd in held:
                os.close(fd)

    def hold(self, names: list[str]) -> list[int]:
        self.run_dir.mkdir(exist_ok=True)
        fds = []
        for name in names:
            fd = os.open(self.run_dir / name, os.O_RDONLY | os.O_CREAT, 0o600)
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            fds.append(fd)
        return fds

    def test_a_flood_of_waiters_is_answered_busy_at_once(self):
        self.keep(KEY)
        held = self.hold([f"queue-{n}.lock" for n in range(serve.QUEUE)])
        try:
            started = time.monotonic()
            code, out, _ = self.ask(f"seed-v1 tar {KEY}", slot_wait=60)
            self.assertEqual((code, out), (4, b"glaeda-seed-serve 1 busy\n"))
            self.assertLess(time.monotonic() - started, 10)  # no 60 s slot wait
        finally:
            for fd in held:
                os.close(fd)

    def test_one_request_at_a_time_per_client(self):
        self.keep(KEY)
        held = self.hold(["client-172.20.21.196.lock"])
        try:
            started = time.monotonic()
            self.assertEqual(self.ask(f"seed-v1 tar {KEY}", slot_wait=60)[:2], (4, b"glaeda-seed-serve 1 busy\n"))
            self.assertLess(time.monotonic() - started, 10)
        finally:
            os.close(held[0])
        self.assertEqual(self.ask(f"seed-v1 tar {KEY}")[0], 0)
        self.assertLessEqual(serve.SERVE_TIMEOUT, 180)

    def test_the_log_stays_bounded(self):
        self.run_dir.mkdir()
        (self.run_dir / "serve.jsonl").write_text("x" * (serve.LOG_BYTES + 1))
        self.ask("ping-v1")
        self.assertTrue((self.run_dir / "serve.jsonl.1").exists())
        self.assertLess((self.run_dir / "serve.jsonl").stat().st_size, 1000)

    def test_the_seeder_lists_its_kept_seeds_newest_first(self):
        os.utime(self.keep(KEY), (100, 100))
        os.utime(self.keep(OTHER, "cmux-ci-2"), (200, 200))
        self.keep(KEY[:-3] + "fff", manifest=False)  # incomplete: not listed
        os.utime(self.keep(OTHER), (150, 150))  # kept in two roots: listed once, newest time
        code, out, _ = self.ask("seed-list-v1")
        head, body = self.header(out)
        self.assertEqual((code, head), (0, "list 2"))
        self.assertEqual(body.decode().splitlines(), [f"{OTHER} 200", f"{KEY} 100"])
        for request in ("seed-list-v1 x", "seed-list-v2"):
            self.assertEqual(self.ask(request)[0], 2)

    def test_the_client_and_server_agree_on_keys(self):
        prefetch = load("glaeda_seed_prefetch_keys", ROOT / "scripts/glaeda-seed-prefetch")
        self.assertEqual(prefetch.KEY_RE.pattern, serve.KEY_RE.pattern)
        self.assertEqual(prefetch.MANIFEST, serve.MANIFEST)


class ArchiveServeTest(unittest.TestCase):
    """--role archive: seeds kept as ARCHIVE/<KEY>.tar.zst on a host that runs no jobs."""

    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        base = Path(self.tmp.name).resolve()  # the serve walk refuses a symlinked /var
        self.archive = base / "archive"
        self.archive.mkdir()
        self.run_dir = base / "run"
        self.receipt = base / "receipt.json"  # absent: the archive host has no glaeda runner

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def store(self, key: str, body: bytes = b"zstd bytes") -> Path:
        path = self.archive / (key + serve.ARCHIVE_SUFFIX)
        path.write_bytes(body)
        os.utime(path, (100, 100))
        return path

    def ask(self, request: str) -> tuple[int, bytes]:
        env = {k: v for k, v in os.environ.items()}
        env["SSH_ORIGINAL_COMMAND"] = request
        env["SSH_CLIENT"] = "172.20.21.196 50000 22"
        proc = subprocess.run([sys.executable, os.fspath(SERVE), "--role", "archive", "--archive",
                               os.fspath(self.archive), "--run-dir", os.fspath(self.run_dir), "--receipt",
                               os.fspath(self.receipt)], env=env, capture_output=True, timeout=60)
        return proc.returncode, proc.stdout

    def test_a_hit_streams_the_file_as_is_and_touches_it(self):
        path = self.store(OTHER, b"\x28\xb5\x2f\xfd stored")
        code, out = self.ask(f"seed-v1 zstd {KEY} {OTHER}")
        self.assertEqual((code, out), (0, f"glaeda-seed-serve 1 hit {OTHER} zstd\n".encode() + b"\x28\xb5\x2f\xfd stored"))
        self.assertGreater(path.stat().st_mtime, 1000)
        log = [json.loads(line) for line in (self.run_dir / "serve.jsonl").read_text().splitlines()]
        self.assertEqual((log[-1]["outcome"], log[-1]["role"], log[-1]["distance"]), ("served", "archive", 1))

    @unittest.skipUnless(serve.zstd_tool(), "needs zstd")
    def test_a_tar_client_gets_the_stream_decompressed(self):
        raw = b"a tar stream"
        packed = subprocess.run([serve.zstd_tool(), "-q", "-c"], input=raw, capture_output=True, check=True).stdout
        self.store(KEY, packed)
        code, out = self.ask(f"seed-v1 tar {KEY}")
        self.assertEqual((code, out), (0, f"glaeda-seed-serve 1 hit {KEY} tar\n".encode() + raw))

    def test_a_miss_logs_the_wanted_key(self):
        self.assertEqual(self.ask(f"seed-v1 zstd {KEY} {OTHER}"), (3, b"glaeda-seed-serve 1 miss\n"))
        log = json.loads((self.run_dir / "serve.jsonl").read_text().splitlines()[-1])
        self.assertEqual((log["outcome"], log["want"]), ("miss", KEY))

    def test_symlinks_and_other_names_are_not_served(self):
        target = self.archive.parent / "elsewhere"
        target.write_bytes(b"secret")
        (self.archive / (KEY + serve.ARCHIVE_SUFFIX)).symlink_to(target)
        self.assertEqual(self.ask(f"seed-v1 zstd {KEY}")[0], 3)
        (self.archive / (OTHER + serve.ARCHIVE_SUFFIX)).mkdir()
        self.assertEqual(self.ask(f"seed-v1 zstd {OTHER}")[0], 3)

    def test_the_list_names_archived_seeds(self):
        self.store(KEY)
        os.utime(self.store(OTHER), (300, 300))
        (self.archive / ".incoming").write_bytes(b"x")
        code, out = self.ask("seed-list-v1")
        self.assertEqual((code, out), (0, f"glaeda-seed-serve 1 list 2\n{OTHER} 300\n{KEY} 100\n".encode()))

    def test_a_host_with_a_runner_receipt_refuses(self):
        self.store(KEY)
        self.receipt.write_text(json.dumps({"member": {"trustedRef": "refs/heads/main"}}))
        code, out = self.ask(f"seed-v1 zstd {KEY}")
        self.assertEqual(code, 2)
        self.assertTrue(out.startswith(b"glaeda-seed-serve 1 refused this-host-runs-glaeda-runners"))

    def test_product_verbs_are_refused(self):
        self.assertEqual(self.ask("product-has-v1 " + "a" * 64)[0], 2)


# Stands in for /usr/bin/ssh from the operator Mac: runs the remote command locally with HOME set to that
# host's sandbox, and python3 for /usr/bin/python3.
FAKE_OPERATOR_SSH = """import os, subprocess, sys
host, command = sys.argv[-2], sys.argv[-1]
homes = os.environ["FAKE_HOMES"]
if host not in os.listdir(homes):
    sys.stderr.write("ssh: Could not resolve hostname\\n"); sys.exit(255)
env = {**os.environ, "HOME": os.path.join(homes, host)}
command = command.replace("/usr/bin/python3 -I -", sys.executable + " -I -", 1)
sys.exit(subprocess.run(["/bin/sh", "-c", command], env=env).returncode)
"""

# Stands in for the mini's /usr/bin/ssh to the seeder in the ping check.
FAKE_CLIENT_SSH = """import sys
print("glaeda-seed-serve 1 pong" if sys.argv[-1] == "ping-v1" else "?")
"""


@unittest.skipUnless(shutil.which("ssh-keygen"), "needs ssh-keygen")
class InstallerTest(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        base = Path(self.tmp.name)
        self.homes = base / "homes"
        self.seeder = self.homes / "cmux15"
        for host in ("cmux15", "cmux11s-mac-mini", "cmux12s-mac-mini", "cmux13s-mac-mini", "cmuxs-mac-mini-6"):
            (self.homes / host / ".ssh").mkdir(parents=True)
        receipt = self.seeder / ".local/state/glaeda/cmux-runner/receipt.json"
        receipt.parent.mkdir(parents=True)
        receipt.write_text(json.dumps({"member": {"trustedRef": "refs/heads/main"}}))
        self.foreign = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIPxRvAUcfoOkBbc1rcpW04SWVIFt7ZqcybRWbr6LSTQ+ operator"
        (self.seeder / ".ssh/authorized_keys").write_text(self.foreign + "\n")
        host_key = base / "ssh_host_ed25519_key.pub"
        host_key.write_text("ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIA74eb98EvwA426g7Bxmia1OV3JM2goHU6AQHsG5IhkX root@cmux15\n")
        fakes = []
        for name, text in (("ssh", FAKE_OPERATOR_SSH), ("client-ssh", FAKE_CLIENT_SSH)):
            path = base / name
            path.write_text(f"#!{sys.executable}\n" + text)
            path.chmod(0o755)
            fakes.append(os.fspath(path))
        self.saved = (lan.SSH, lan.CLIENT_SSH, dict(os.environ))
        lan.SSH, lan.CLIENT_SSH = fakes
        os.environ.update({"FAKE_HOMES": os.fspath(self.homes), "GLAEDA_SEED_LAN_HOST_KEY": os.fspath(host_key)})

    def tearDown(self) -> None:
        lan.SSH, lan.CLIENT_SSH, env = self.saved
        os.environ.clear()
        os.environ.update(env)
        self.tmp.cleanup()

    def cli(self, *argv: str) -> tuple[int, dict]:
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = lan.main(list(argv))
        return code, json.loads(out.getvalue())

    def install(self, *hosts: str, apply: bool = True) -> tuple[int, dict]:
        return self.cli("install", "--seeder", "cmux15", "--address", "172.20.21.202", "--address", "cmux15.local",
                        *hosts, *(["--apply"] if apply else []))

    def tree(self) -> dict:
        # Apple's python3 keeps caches under ~/Library; they are the interpreter's, not ours.
        return {os.fspath(p.relative_to(self.homes)): p.read_bytes() if p.is_file() else None
                for p in sorted(self.homes.rglob("*")) if p.relative_to(self.homes).parts[1:2] != ("Library",)}

    def authorized(self) -> list[str]:
        return (self.seeder / ".ssh/authorized_keys").read_text().splitlines()

    def test_plan_changes_nothing(self):
        before = self.tree()
        code, report = self.install("cmux12s-mac-mini", apply=False)
        self.assertEqual(code, 0)
        self.assertEqual(self.tree(), before)
        self.assertEqual(report["hosts"]["cmux12s-mac-mini"]["config"], "missing")
        self.assertEqual(report["seeder_state"]["serve"], "missing")

    def test_apply_installs_both_ends_then_is_idempotent(self):
        code, report = self.install("cmux12s-mac-mini", "cmux13s-mac-mini")
        self.assertEqual(code, 0, report)
        serve_path = self.seeder / ".local/libexec/glaeda-seed-serve"
        self.assertEqual(serve_path.read_bytes(), SERVE.read_bytes())
        self.assertEqual(serve_path.stat().st_mode & 0o777, 0o755)
        lines = self.authorized()
        self.assertEqual(lines[0], self.foreign)
        command = f"/usr/bin/python3 -I {self.seeder}/.local/libexec/glaeda-seed-serve"
        for host in ("cmux12s-mac-mini", "cmux13s-mac-mini"):
            conf = self.homes / host / ".config/glaeda/seed-lan"
            self.assertEqual(conf.stat().st_mode & 0o777, 0o700)
            public = (conf / "id_ed25519.pub").read_text().split()
            line = f'restrict,from="172.20.20.0/22",command="{command}" {public[0]} {public[1]} glaeda-seed-lan@{host}'
            self.assertIn(line, lines)
            self.assertEqual((conf / "known_hosts").read_text(),
                             "glaeda-seeder ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIA74eb98EvwA426g7Bxmia1OV3JM2goHU6AQHsG5IhkX\n")
            config = json.loads((conf / "config.json").read_text())
            self.assertEqual((config["user"], config["addresses"]), ("cmux", ["172.20.21.202", "cmux15.local"]))
            self.assertEqual(report["hosts"][host]["ping"], "glaeda-seed-serve 1 pong")
            keygen = subprocess.run(["ssh-keygen", "-lf", os.fspath(conf / "id_ed25519.pub")], capture_output=True,
                                    text=True, check=True).stdout.split()[1]
            self.assertEqual(report["hosts"][host]["fingerprint"], keygen)
        self.assertEqual(len(lines), 3)
        self.assertEqual({k["id"] for k in report["manifest_keys"]},
                         {"seed-lan-cmux12s-mac-mini", "seed-lan-cmux13s-mac-mini"})
        backups = list((self.seeder / ".local/state/glaeda/seed-lan").glob("authorized_keys.*"))
        self.assertEqual([b.read_text() for b in backups], [self.foreign + "\n"])
        # The prefetch client reads what the installer wrote.
        pm = load("glaeda_seed_prefetch_for_test", ROOT / "scripts/glaeda-seed-prefetch")
        self.assertIsNotNone(pm.lan_config(self.homes / "cmux12s-mac-mini/.config/glaeda/seed-lan/config.json"))
        before = self.tree()
        code, again = self.install("cmux12s-mac-mini", "cmux13s-mac-mini")
        self.assertEqual(self.tree(), before)
        self.assertEqual(again["seeder_state"]["changed"], [])
        self.assertTrue(all(h["changed"] == [] and h["authorized"] == "current" for h in again["hosts"].values()))

    def test_the_archive_role_needs_a_host_without_runners(self):
        archive = self.homes / "cmux-lawrence"
        (archive / ".ssh").mkdir(parents=True)
        (archive / ".ssh/authorized_keys").write_text(self.foreign + "\n")
        argv = ("install", "--seeder", "cmux-lawrence", "--role", "archive", "--user", "cmux-lawrence",
                "--address", "172.20.21.158", "cmux12s-mac-mini", "--apply")
        code, report = self.cli(*argv)
        self.assertEqual(code, 0, report)
        command = f"/usr/bin/python3 -I {archive}/.local/libexec/glaeda-seed-serve --role archive"
        lines = (archive / ".ssh/authorized_keys").read_text().splitlines()
        self.assertEqual(lines[0], self.foreign)
        self.assertEqual(len(lines), 2)
        self.assertIn(f'restrict,from="172.20.20.0/22",command="{command}" ', lines[1])
        self.assertTrue(lines[1].endswith(" glaeda-seed-lan@cmux12s-mac-mini"))
        config = json.loads((self.homes / "cmux12s-mac-mini/.config/glaeda/seed-lan/config.json").read_text())
        self.assertEqual((config["seeder"], config["user"]), ("cmux-lawrence", "cmux-lawrence"))
        self.assertEqual(report["seeder_state"]["role"], "archive")
        # Re-running is a no-op, and removal finds the archive's lines by their forced command.
        before = self.tree()
        self.assertEqual(self.cli(*argv)[0], 0)
        self.assertEqual(self.tree(), before)
        code, removed = self.cli("remove", "--seeder", "cmux-lawrence", "--role", "archive", "cmux12s-mac-mini",
                                 "--apply")
        self.assertEqual(removed["seeder_state"]["remaining"], [])
        self.assertEqual((archive / ".ssh/authorized_keys").read_text().splitlines(), [self.foreign])
        # The trusted seed writers never read from the archive.
        code, report = self.cli("install", "--seeder", "cmux-lawrence", "--role", "archive", "--user", "cmux-lawrence",
                                "--address", "172.20.21.158", "cmux12s-mac-mini", "cmux15", "--apply")
        self.assertEqual(code, 1)
        self.assertIn("never read from the archive", report["error"])
        # A host that runs glaeda runners (cmux15 here) can never be the archive.
        code, report = self.cli("install", "--seeder", "cmux15", "--role", "archive", "--address", "x",
                                "cmux12s-mac-mini", "--apply")
        self.assertEqual(code, 1)
        self.assertIn("the seed archive must run no jobs", report["error"])

    def test_a_regenerated_client_key_replaces_its_line(self):
        self.install("cmux12s-mac-mini")
        conf = self.homes / "cmux12s-mac-mini/.config/glaeda/seed-lan"
        old = (conf / "id_ed25519.pub").read_text().split()[1]
        (conf / "id_ed25519").unlink()
        self.install("cmux12s-mac-mini")
        new = (conf / "id_ed25519.pub").read_text().split()[1]
        ours = [line for line in self.authorized() if "glaeda-seed-lan@" in line]
        self.assertNotEqual(old, new)
        self.assertEqual(len(ours), 1)
        self.assertIn(new, ours[0])

    def test_an_untrusted_or_forbidden_seeder_is_refused_untouched(self):
        before = self.tree()
        code, report = self.cli("install", "--seeder", "cmuxs-mac-mini-6", "--address", "x", "cmux12s-mac-mini",
                                "--apply")
        self.assertEqual(code, 1)
        self.assertIn("never be a seed source", report["error"])
        (self.seeder / ".local/state/glaeda/cmux-runner/receipt.json").write_text(json.dumps({"member": {}}))
        code, report = self.install("cmux12s-mac-mini")
        self.assertEqual(code, 1)
        self.assertIn("trusted ref", report["error"])
        self.assertEqual({k: v for k, v in self.tree().items() if "receipt" not in k},
                         {k: v for k, v in before.items() if "receipt" not in k})
        for argv in (["--address", "-oProxyCommand=x", "cmux12s-mac-mini"], ["--address", "a", "cmux15"],
                     ["--address", "a", "--from", 'x",command="sh', "cmux12s-mac-mini"], ["--address", "a", "-bad"]):
            with self.subTest(argv=argv), contextlib.redirect_stderr(io.StringIO()):
                try:
                    code, _ = self.cli("install", "--seeder", "cmux15", *argv, "--apply")
                except SystemExit as exit_:
                    code = exit_.code
                self.assertNotEqual(code, 0)

    def test_malformed_or_duplicate_client_keys_are_refused(self):
        self.install("cmux12s-mac-mini")
        pub = self.homes / "cmux12s-mac-mini/.config/glaeda/seed-lan/id_ed25519.pub"
        good = pub.read_text()
        before = self.authorized()
        for bad in ("ssh-ed25519 !!!notbase64 x\n", "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABAQ x\n",
                    "ssh-ed25519 " + base64.b64encode(b"\0\0\0\x0bssh-ed25519\0\0\0\x10" + b"k" * 16).decode() + " x\n"):
            with self.subTest(bad=bad[:20]):
                pub.write_text(bad)
                code, report = self.install("cmux12s-mac-mini")
                self.assertEqual(code, 1)
                self.assertIn("not a well-formed", report["hosts"]["cmux12s-mac-mini"]["authorized"])
                self.assertEqual(self.authorized(), before)
        pub.write_text(good)
        # Another mini carrying the same key (copied), and a key already on a foreign line.
        other = self.homes / "cmux13s-mac-mini/.config/glaeda/seed-lan"
        other.mkdir(parents=True)
        for name in ("id_ed25519", "id_ed25519.pub"):
            shutil.copy(pub.parent / name, other / name)
        code, report = self.install("cmux13s-mac-mini")
        self.assertEqual(code, 1)
        self.assertIn("already authorized", report["hosts"]["cmux13s-mac-mini"]["authorized"])
        self.assertFalse(any("glaeda-seed-lan@cmux13s-mac-mini" in line for line in self.authorized()))
        self.cli("remove", "--seeder", "cmux15", "cmux12s-mac-mini", "--apply")
        with (self.seeder / ".ssh/authorized_keys").open("a") as ak:
            ak.write(" ".join(good.split()[:2]) + " someone-else\n")
        code, report = self.install("cmux13s-mac-mini")
        self.assertIn("already authorized", report["hosts"]["cmux13s-mac-mini"]["authorized"])
        self.assertEqual(lan.ed25519_blob(" ".join(good.split()[:2])) is not None, True)

    def test_a_symlinked_authorized_keys_or_a_forbidden_host_name_is_refused(self):
        ak = self.seeder / ".ssh/authorized_keys"
        real = self.seeder / "ak-real"
        ak.rename(real)
        ak.symlink_to(real)
        code, report = self.install("cmux12s-mac-mini")
        self.assertEqual(code, 1)
        self.assertIn("symlink", report["error"])
        self.assertTrue(ak.is_symlink())
        ak.unlink()
        real.rename(ak)
        os.environ["GLAEDA_SEED_LAN_NAMES"] = "cmuxs-mac-mini-6"  # the probe's LocalHostName
        code, report = self.install("cmux12s-mac-mini")
        self.assertEqual(code, 1)
        self.assertIn("never be a seed source", report["error"])

    def test_old_authorized_keys_backups_are_pruned(self):
        state = self.seeder / ".local/state/glaeda/seed-lan"
        state.mkdir(parents=True)
        for n in range(8):
            (state / f"authorized_keys.20260101T00000{n}Z.1").write_text("old")
        self.install("cmux12s-mac-mini")
        self.assertEqual(len(list(state.glob("authorized_keys.*"))), lan.KEEP_BACKUPS)

    def test_remote_python_runs_isolated_and_ssh_forwards_nothing(self):
        for option in ("-a", "-x", "ForwardAgent=no", "ClearAllForwardings=yes", "BatchMode=yes"):
            self.assertIn(option, lan.SSH_OPTIONS)
        calls = []
        real = subprocess.run

        def spy(argv, **kwargs):
            calls.append(argv)
            return real(argv, **kwargs)

        with unittest.mock.patch.object(lan.subprocess, "run", spy):
            self.install("cmux12s-mac-mini", apply=False)
        self.assertTrue(calls and all(c[-1].startswith("/usr/bin/python3 -I - ") for c in calls))

    def mesh(self, *extra: str) -> tuple[int, dict]:
        hosts = ["cmux12s-mac-mini", "cmux13s-mac-mini", "cmux11s-mac-mini"]
        return self.cli("mesh", *hosts, "--address", "cmux12s-mac-mini=172.20.21.196", "--address",
                        "cmux13s-mac-mini=172.20.21.197", "--address", "cmux11s-mac-mini=172.20.21.195", *extra)

    def test_the_product_mesh_authorizes_every_other_mini_for_products_only(self):
        before = self.tree()
        code, plan = self.mesh()
        self.assertEqual((code, self.tree()), (0, before))
        code, report = self.mesh("--apply")
        self.assertEqual(code, 0, report)
        hosts = ["cmux12s-mac-mini", "cmux13s-mac-mini", "cmux11s-mac-mini"]
        keys = {h: " ".join((self.homes / h / ".config/glaeda/lan-mesh/id_ed25519.pub").read_text().split()[:2])
                for h in hosts}
        for host in hosts:
            home = self.homes / host
            conf = json.loads((home / ".config/glaeda/lan-mesh/config.json").read_text())
            self.assertEqual([p["name"] for p in conf["peers"]], [h for h in hosts if h != host])
            self.assertEqual(len((home / ".config/glaeda/lan-mesh/known_hosts").read_text().splitlines()), 3)
            lines = [l for l in (home / ".ssh/authorized_keys").read_text().splitlines() if "glaeda-lan-mesh@" in l]
            command = f"/usr/bin/python3 -I {home}/.local/libexec/glaeda-seed-serve --role product"
            self.assertEqual(sorted(lines), sorted(
                f'restrict,from="172.20.20.0/22",command="{command}" {keys[p]} glaeda-lan-mesh@{p}'
                for p in hosts if p != host))
            self.assertEqual((home / ".local/libexec/glaeda-seed-serve").read_bytes(), SERVE.read_bytes())
            self.assertEqual(list(report["hosts"][host]["ping"].values()), ["glaeda-seed-serve 1 pong"])
        # The prefetch-side seed key is separate; the mesh config reads back through glaeda-lan-fetch.
        lf = load("glaeda_lan_fetch_for_test", ROOT / "scripts/glaeda-lan-fetch")
        self.assertIsNotNone(lf.load_config(self.homes / "cmux13s-mac-mini/.config/glaeda/lan-mesh/config.json"))
        self.assertEqual(len(report["manifest_keys"]), 3)
        after = self.tree()
        code, again = self.mesh("--apply")
        self.assertEqual(self.tree(), after)
        code, removed = self.cli("mesh-remove", "cmux13s-mac-mini", "--apply")
        self.assertEqual(code, 0)
        self.assertFalse((self.homes / "cmux13s-mac-mini/.config/glaeda/lan-mesh").exists())
        self.assertFalse(any("glaeda-lan-mesh@" in l for l in
                             (self.homes / "cmux13s-mac-mini/.ssh/authorized_keys").read_text().splitlines()))
        self.assertTrue(any("glaeda-lan-mesh@" in l for l in
                            (self.homes / "cmux12s-mac-mini/.ssh/authorized_keys").read_text().splitlines()))

    def test_the_mesh_refuses_a_duplicate_key_and_needs_two_hosts(self):
        self.mesh("--apply")
        a = self.homes / "cmux12s-mac-mini/.config/glaeda/lan-mesh"
        b = self.homes / "cmux13s-mac-mini/.config/glaeda/lan-mesh"
        for name in ("id_ed25519", "id_ed25519.pub"):
            shutil.copy(a / name, b / name)
        code, report = self.mesh("--apply")
        self.assertEqual(code, 1)
        self.assertIn("refused", report["hosts"]["cmux13s-mac-mini"]["key"])
        code, report = self.cli("mesh", "cmux12s-mac-mini")
        self.assertEqual(code, 1)

    def test_helper_install_never_runs_sudo_and_prints_the_commands(self):
        state = Path(self.tmp.name) / "operator-state"
        calls = []
        real = subprocess.run

        def spy(argv, **kwargs):
            calls.append(argv)
            return real(argv, **kwargs)

        with unittest.mock.patch.object(lan, "SCRIPT_DIR_OUT", state), \
                unittest.mock.patch.object(lan.subprocess, "run", spy):
            code, plan = self.cli("helper-install", "cmux12s-mac-mini")
            self.assertEqual(plan["hosts"]["cmux12s-mac-mini"]["state"], "missing")
            self.assertFalse(state.exists())
            code, report = self.cli("helper-install", "cmux12s-mac-mini", "--admin", "leoadmin", "--apply")
        # Only read-only probes over plain ssh: no sudo, no fleet-sudo, no password on any stdin.
        self.assertTrue(calls)
        for argv in calls:
            self.assertEqual(argv[0], lan.SSH)
            self.assertTrue(argv[-1].startswith("/usr/bin/python3 -I - "))
            self.assertNotIn("sudo", " ".join(argv))
        script = Path(report["script"]["path"])
        self.assertEqual(script.parent, state)
        self.assertEqual(hashlib.sha256(script.read_bytes()).hexdigest(), report["script"]["sha256"])
        self.assertEqual(script.read_text(), lan.helper_root_script((ROOT / "scripts/glaeda-lan-fetch").read_bytes()))
        scp, run = report["commands"]["cmux12s-mac-mini"]
        self.assertEqual(scp, f"scp {script} leoadmin@cmux12s-mac-mini:")
        self.assertTrue(run.startswith("ssh -t leoadmin@cmux12s-mac-mini "))
        self.assertIn(f"sudo /bin/bash {script.name}", run)
        self.assertNotIn("cmux@", " ".join(report["commands"]["cmux12s-mac-mini"]))
        self.assertIn("other than cmux", report["next"][0])
        self.assertEqual((code, report["ok"]), (1, False))  # not current until someone runs the commands

    def run_root_script(self, data: bytes, base: Path, **kwargs) -> subprocess.CompletedProcess:
        """Run the root script as ourselves: uid = ours, the chain checked from BASE down."""
        target = base / "Library/Application Support/glaeda/bin/glaeda-lan-fetch"
        script = lan.helper_root_script(data, path=os.fspath(target), owner=f"{os.getuid()}:{os.getgid()}",
                                        lock=os.fspath(base / "install.lock"), uid=os.getuid(), top=os.fspath(base),
                                        **kwargs)
        return subprocess.run(["/bin/bash", "-c", script], capture_output=True, text=True, timeout=60)

    def root_base(self) -> Path:
        base = Path(os.path.realpath(self.tmp.name)) / "root"
        base.mkdir(mode=0o755)
        base.chmod(0o755)
        return base

    def test_the_root_script_installs_and_fails_closed(self):
        base = self.root_base()
        target = base / "Library/Application Support/glaeda/bin/glaeda-lan-fetch"
        proc = self.run_root_script(b"helper v1", base)
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        self.assertEqual(target.read_bytes(), b"helper v1")
        self.assertEqual((target.stat().st_mode & 0o777, target.parent.stat().st_mode & 0o777), (0o755, 0o755))
        self.assertFalse((base / "install.lock").exists())
        self.assertEqual(self.run_root_script(b"helper v2", base).returncode, 0)
        self.assertEqual(target.read_bytes(), b"helper v2")
        # Each unsafe component refuses, and the file stays as it was.
        glaeda = base / "Library/Application Support/glaeda"
        for unsafe, undo in ((lambda: glaeda.chmod(0o775), lambda: glaeda.chmod(0o755)),
                             (lambda: target.chmod(0o757), lambda: target.chmod(0o755))):
            unsafe()
            proc = self.run_root_script(b"helper v3", base)
            undo()
            self.assertEqual(proc.returncode, 1, proc.stdout)
            self.assertIn("FAILED", proc.stdout)
            self.assertEqual(target.read_bytes(), b"helper v2")
        real = base / "elsewhere"
        real.mkdir()
        (base / "Library/Application Support/glaeda/bin").rename(real / "bin")
        (base / "Library/Application Support/glaeda/bin").symlink_to(real / "bin")
        proc = self.run_root_script(b"helper v3", base)
        self.assertEqual(proc.returncode, 1)
        self.assertIn("symlink", proc.stdout)
        self.assertEqual((real / "bin/glaeda-lan-fetch").read_bytes(), b"helper v2")

    def test_the_root_scripts_lock_checks_its_holder(self):
        base = self.root_base()
        lock = base / "install.lock"
        lock.mkdir()
        (lock / "pid").write_text(str(os.getpid()))  # a live holder: busy
        proc = self.run_root_script(b"x", base)
        self.assertEqual(proc.returncode, 75)
        self.assertIn("BUSY", proc.stdout)
        dead = subprocess.Popen(["true"])
        dead.wait()
        (lock / "pid").write_text(str(dead.pid))  # a dead holder: taken over
        self.assertEqual(self.run_root_script(b"x", base).returncode, 0)
        lock.mkdir()
        (lock / "pid").write_text(str(os.getpid()))
        old = time.time() - 3600
        os.utime(lock, (old, old))  # a live pid but an hour-old lock: taken over
        self.assertEqual(self.run_root_script(b"y", base).returncode, 0)

    def test_the_mesh_refuses_the_seeder_and_trusted_hosts(self):
        for hosts, why in ((["cmux12s-mac-mini", "cmux15"], "trusted seeder"),
                           (["cmux12s-mac-mini", "cmuxs-mac-mini-6"], "never-source")):
            with self.subTest(hosts=hosts):
                code, report = self.cli("mesh", *hosts, "--address", f"{hosts[0]}=172.20.21.196",
                                        "--address", f"{hosts[1]}=172.20.21.197", "--apply")
                self.assertEqual(code, 1)
                self.assertIn("cannot join the product mesh", report["error"])
        receipt = self.homes / "cmux13s-mac-mini/.local/state/glaeda/cmux-runner/receipt.json"
        receipt.parent.mkdir(parents=True)
        receipt.write_text(json.dumps({"member": {"trustedRef": "refs/heads/main"}}))
        code, report = self.mesh("--apply")
        self.assertIn("trusted-only runner", report["error"])
        receipt.unlink()
        # A mesh host that the seed-lan config names as the seeder, whatever its alias here.
        self.install("cmux12s-mac-mini")
        seed_config = self.homes / "cmux12s-mac-mini/.config/glaeda/seed-lan/config.json"
        seed_config.write_text(json.dumps({**json.loads(seed_config.read_text()), "seeder": "cmux13s-mac-mini"}))
        code, report = self.mesh("--apply")
        self.assertIn("cannot join the product mesh", report["error"])
        self.assertFalse(any("glaeda-lan-mesh@" in line for line in self.authorized()))

    def test_the_mesh_drops_its_lines_for_hosts_it_no_longer_names(self):
        self.mesh("--apply")
        code, report = self.cli("mesh", "cmux12s-mac-mini", "cmux13s-mac-mini", "--address",
                                "cmux12s-mac-mini=172.20.21.196", "--address", "cmux13s-mac-mini=172.20.21.197",
                                "--apply")
        self.assertEqual(code, 0, report)
        lines = [l for l in (self.homes / "cmux12s-mac-mini/.ssh/authorized_keys").read_text().splitlines()
                 if "glaeda-lan-mesh@" in l]
        self.assertEqual([l.split()[-1] for l in lines], ["glaeda-lan-mesh@cmux13s-mac-mini"])

    def test_forced_commands_come_from_constants_and_a_validated_home(self):
        good = {"home": "/Users/cmux", "command": "/usr/bin/python3 -I /Users/cmux/.local/libexec/glaeda-seed-serve"}
        self.assertEqual(lan.forced_command(good, "h"), good["command"])
        for bad in ({**good, "home": "/Users/cmux\" x"}, {**good, "home": "/Users/../etc"}, {**good, "home": "rel"},
                    {**good, "command": good["command"] + " --role product"}):
            with self.subTest(bad=bad), self.assertRaises(lan.Failure):
                lan.forced_command(bad, "h")

    def test_the_root_script_carries_its_own_bytes_and_checks_them(self):
        script = lan.helper_root_script(b"helper bytes")
        self.assertIn(hashlib.sha256(b"helper bytes").hexdigest(), script)
        self.assertNotIn(".local/bin", script)  # never copies the mini's user-writable copy
        self.assertIn("install -d -o root -g wheel -m 0755 '/Library/Application Support/glaeda/bin'", script)
        self.assertIn("-user 0 ! -perm -0020 ! -perm -0002", script)
        self.assertIn("chown root:wheel", script)
        self.assertEqual(lan.HELPER_PATH, "/Library/Application Support/glaeda/bin/glaeda-lan-fetch")
        good = [["/x/glaeda-lan-fetch", 0, 0o755, False], ["/x", 0, 0o755, False], ["/", 0, 0o755, False]]
        want = "a" * 64
        self.assertEqual(lan.helper_state({"sha": want, "chain": good}, want), "current")
        self.assertEqual(lan.helper_state({"sha": "", "chain": good}, want), "missing")
        self.assertEqual(lan.helper_state({"sha": "b" * 64, "chain": good}, want), "stale")
        for bad in ([["/x", 501, 0o755, False]], [["/x", 0, 0o775, False]], [["/x", 0, 0o755, True]]):
            self.assertTrue(lan.helper_state({"sha": want, "chain": good[:1] + bad}, want).startswith("not root-owned"))

    def test_remove_drops_only_its_own_lines(self):
        self.install("cmux12s-mac-mini", "cmux13s-mac-mini")
        code, plan = self.cli("remove", "--seeder", "cmux15", "cmux12s-mac-mini")
        self.assertEqual(plan["seeder_state"]["would_drop"], ["glaeda-seed-lan@cmux12s-mac-mini"])
        self.assertTrue((self.homes / "cmux12s-mac-mini/.config/glaeda/seed-lan").exists())
        code, report = self.cli("remove", "--seeder", "cmux15", "cmux12s-mac-mini", "--apply")
        self.assertEqual(code, 0)
        self.assertFalse((self.homes / "cmux12s-mac-mini/.config/glaeda/seed-lan").exists())
        self.assertEqual(report["seeder_state"]["remaining"], ["glaeda-seed-lan@cmux13s-mac-mini"])
        self.assertEqual(self.authorized()[0], self.foreign)
        self.assertTrue((self.seeder / ".local/libexec/glaeda-seed-serve").exists())
        code, report = self.cli("remove", "--seeder", "cmux15", "--apply")  # every client, and the serve script
        self.assertEqual(self.authorized(), [self.foreign])
        self.assertFalse((self.seeder / ".local/libexec/glaeda-seed-serve").exists())


if __name__ == "__main__":
    unittest.main()
