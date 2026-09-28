#!/usr/bin/env python3
"""Tests for scripts/glaeda-lan-fetch and glaeda-seed-serve --role product (compiled products over the LAN)."""

from __future__ import annotations

import hashlib
import importlib.machinery
import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import threading
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


lf = load("glaeda_lan_fetch", ROOT / "scripts/glaeda-lan-fetch")
serve = load("glaeda_seed_serve_products", SERVE)

# Stands in for /usr/bin/ssh to a peer: FAKE_PEERS maps an address to that peer's product cache; the real
# serve script answers as the forced command would. FAKE_PEER_MODE_<address> overrides one peer.
FAKE_SSH = """import json, os, sys
address, request = sys.argv[-2], sys.argv[-1]
with open(os.environ["FAKE_SSH_LOG"], "a") as log:
    log.write(json.dumps(sys.argv[1:]) + "\\n")
peers = json.loads(os.environ["FAKE_PEERS"])
mode = os.environ.get("FAKE_PEER_MODE_" + address.replace(".", "_"), "serve")
if address not in peers or mode == "unreachable":
    sys.stderr.write(f"ssh: connect to host {address} port 22: No route to host\\n"); sys.exit(255)
if mode == "short" and request.startswith("product-v1"):
    sha = request.split()[1]
    os.write(1, f"glaeda-seed-serve 1 hit-product {sha} 1000\\n".encode() + b"x" * 10); sys.exit(0)
if mode == "stalled" and request.startswith("product-has-v1"):
    import time
    time.sleep(60)
if mode == "silent" and request.startswith("product-v1"):
    import time
    if os.environ.get("FAKE_PIDFILE"):
        open(os.environ["FAKE_PIDFILE"], "w").write(str(os.getpid()))
    time.sleep(60)
os.environ["SSH_ORIGINAL_COMMAND"] = request
os.execv(sys.executable, [sys.executable, os.environ["FAKE_SERVE"], "--role", "product", "--products", peers[address],
                          "--run-dir", peers[address] + ".run", "--receipt", "/nonexistent"])
"""


def put_product(root: Path, data: bytes, *, digest: str | None = None, size: int | None = None,
                schema: int = serve.PRODUCT_SCHEMA) -> tuple[str, Path]:
    """A cmux node_product_cache entry holding DATA; returns (content digest, entry)."""
    sha = digest or hashlib.sha256(data).hexdigest()
    key = hashlib.sha256(os.urandom(16)).hexdigest()
    entry = root / "objects" / key[:2] / key
    entry.mkdir(parents=True)
    (entry / serve.PRODUCT_OBJECT).write_bytes(data)
    (entry / serve.PRODUCT_METADATA).write_text(json.dumps({
        "schema_generation": schema, "format_generation": serve.PRODUCT_FORMAT, "identity": {},
        "object_digest": sha, "size": len(data) if size is None else size, "source_class": "github"}))
    return sha, entry


class ProductServeTest(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        # realpath: the serve refuses a symlink anywhere on the path, and macOS's /var is one.
        self.products = Path(os.path.realpath(self.tmp.name)) / "node-products"
        (self.products / "objects").mkdir(parents=True)

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def ask(self, request: str, role: str = "product") -> tuple[int, bytes]:
        env = {**os.environ, "SSH_ORIGINAL_COMMAND": request, "SSH_CLIENT": "172.20.21.196 1 22"}
        proc = subprocess.run([sys.executable, os.fspath(SERVE), "--role", role, "--products", os.fspath(self.products),
                               "--run-dir", os.fspath(Path(self.tmp.name) / "run"), "--receipt", "/nonexistent",
                               "--state", os.fspath(Path(self.tmp.name) / "ci")], env=env, capture_output=True, timeout=60)
        return proc.returncode, proc.stdout

    def test_a_cached_product_is_found_by_content_digest_and_streamed_exactly(self):
        data = os.urandom(300000)
        sha, _ = put_product(self.products, data)
        self.assertEqual(self.ask(f"product-has-v1 {sha}"), (0, f"glaeda-seed-serve 1 has {sha} {len(data)}\n".encode()))
        code, out = self.ask(f"product-v1 {sha}")
        self.assertEqual(code, 0)
        self.assertEqual(out, f"glaeda-seed-serve 1 hit-product {sha} {len(data)}\n".encode() + data)
        self.assertEqual(self.ask("product-has-v1 " + "0" * 64), (3, b"glaeda-seed-serve 1 miss\n"))

    def test_only_what_the_cache_layout_names_is_served(self):
        data = b"x" * 1000
        cases = []
        sha, entry = put_product(self.products, data)
        (entry / serve.PRODUCT_OBJECT).unlink()
        (entry / serve.PRODUCT_OBJECT).symlink_to("/etc/hosts")  # a symlinked object
        cases.append(sha)
        sha, entry = put_product(self.products, data, size=999)  # size disagrees with the file
        cases.append(sha)
        sha, _ = put_product(self.products, data, size=serve.MAX_OBJECT_BYTES + 1)
        cases.append(sha)
        sha, _ = put_product(self.products, b"y" * 1000, schema=1)  # an older cache schema
        cases.append(sha)
        real = Path(self.tmp.name) / "elsewhere"
        sha, entry = put_product(real, b"z" * 1000)
        link = self.products / "objects" / entry.parent.name
        link.mkdir(exist_ok=True)
        (link / entry.name).symlink_to(entry)  # a symlinked entry directory
        cases.append(sha)
        for sha in cases:
            with self.subTest(sha=sha[:8]):
                self.assertEqual(self.ask(f"product-v1 {sha}")[0], 3)

    def test_the_roles_never_answer_each_others_verbs(self):
        sha, _ = put_product(self.products, b"x")
        key = "admission-derived-data-v1-macOS-ARM64-" + "a" * 32 + "-j14-" + "b" * 40
        for request in (f"seed-v1 tar {key}", f"product-v1 {sha.upper()}", f"product-v1 {sha} x",
                        f"product-v1 ../{sha[3:]}", "product-v1", f"product-v2 {sha}"):
            with self.subTest(request=request[:40]):
                code, out = self.ask(request)
                self.assertEqual(code, 2)
                self.assertTrue(out.startswith(b"glaeda-seed-serve 1 refused"))
        # The seed role refuses product verbs (and, here, everything: no trusted receipt).
        self.assertEqual(self.ask(f"product-v1 {sha}", role="seed")[0], 2)
        self.assertEqual(self.ask("ping-v1"), (0, b"glaeda-seed-serve 1 pong\n"))  # products need no receipt

    def test_product_streams_share_the_seed_caps(self):
        sha, _ = put_product(self.products, b"x" * 100)
        run = Path(self.tmp.name) / "run"
        run.mkdir()
        import fcntl
        fds = []
        for n in range(serve.QUEUE):
            fd = os.open(run / f"queue-{n}.lock", os.O_RDONLY | os.O_CREAT, 0o600)
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            fds.append(fd)
        try:
            self.assertEqual(self.ask(f"product-v1 {sha}"), (4, b"glaeda-seed-serve 1 busy\n"))
            # Lookups are admitted before the cache is scanned: a flood of them is capped too.
            self.assertEqual(self.ask(f"product-has-v1 {sha}"), (4, b"glaeda-seed-serve 1 busy\n"))
        finally:
            for fd in fds:
                os.close(fd)
        fd = os.open(run / "client-172.20.21.196.lock", os.O_RDONLY | os.O_CREAT, 0o600)
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        try:
            self.assertEqual(self.ask(f"product-has-v1 {sha}")[0], 4)  # one request per client address
        finally:
            os.close(fd)
        self.assertEqual(self.ask(f"product-has-v1 {sha}")[0], 0)

    def test_hard_links_other_owners_big_metadata_and_symlinked_paths_are_refused(self):
        data = b"x" * 1000
        sha, entry = put_product(self.products, data)
        os.link(entry / serve.PRODUCT_OBJECT, Path(self.tmp.name) / "second-link")  # st_nlink == 2
        self.assertEqual(self.ask(f"product-has-v1 {sha}")[0], 3)
        (Path(self.tmp.name) / "second-link").unlink()
        self.assertEqual(self.ask(f"product-has-v1 {sha}")[0], 0)
        sha2, entry2 = put_product(self.products, b"y" * 10)
        metadata = json.loads((entry2 / serve.PRODUCT_METADATA).read_text())
        (entry2 / serve.PRODUCT_METADATA).write_text(json.dumps({**metadata, "pad": "p" * serve.MAX_METADATA_BYTES}))
        self.assertEqual(self.ask(f"product-has-v1 {sha2}")[0], 3)
        with unittest.mock.patch.object(serve.os, "getuid", return_value=os.getuid() + 1):
            self.assertIsNone(serve.open_product(self.products, sha))  # owned by someone else
        self.assertIsNotNone(serve.open_product(self.products, sha))
        # A symlink anywhere on the cache path, not only at the object.
        link = Path(os.path.realpath(self.tmp.name)) / "products-link"
        link.symlink_to(self.products)
        self.assertIsNone(serve.open_product(link, sha))
        self.assertIsNone(serve.open_product(link / "..", sha))
        objects = self.products / "objects"
        moved = Path(os.path.realpath(self.tmp.name)) / "objects-real"
        objects.rename(moved)
        objects.symlink_to(moved)
        self.assertIsNone(serve.open_product(self.products, sha))


class LanFetchTest(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        base = Path(os.path.realpath(self.tmp.name))
        self.peers = {"172.20.21.197": base / "p1", "172.20.21.198": base / "p2"}
        for root in self.peers.values():
            (root / "objects").mkdir(parents=True)
        conf = base / "lan-mesh"
        conf.mkdir()
        (conf / "id_ed25519").write_text("k")
        (conf / "known_hosts").write_text("x")
        (conf / "config.json").write_text(json.dumps({"schema": 1, "user": "cmux", "peers": [
            {"name": "cmux13s-mac-mini", "address": "172.20.21.197"},
            {"name": "cmux14", "address": "172.20.21.198"}]}))
        self.conf = conf / "config.json"
        fake = base / "fake-ssh"
        fake.write_text(f"#!{sys.executable}\n" + FAKE_SSH)
        fake.chmod(0o755)
        self.log = base / "ssh.log"
        self.saved = (lf.SSH, lf.TRANSFER_TIMEOUT, dict(os.environ))
        lf.SSH = os.fspath(fake)
        os.environ.update({"FAKE_PEERS": json.dumps({a: os.fspath(r) for a, r in self.peers.items()}),
                           "FAKE_SERVE": os.fspath(SERVE), "FAKE_SSH_LOG": os.fspath(self.log)})
        self.out = base / "job"
        self.out.mkdir()

    def tearDown(self) -> None:
        lf.SSH, lf.TRANSFER_TIMEOUT, env = self.saved
        os.environ.clear()
        os.environ.update(env)
        self.tmp.cleanup()

    def fetch(self, sha: str, name: str = "app-host-products.tar.gz") -> tuple[int, dict, Path]:
        dest = self.out / name
        code, record = lf.fetch(sha, dest, lf.load_config(self.conf))
        return code, record, dest

    def leftovers(self) -> list[str]:
        return sorted(p.name for p in self.out.iterdir() if p.name.startswith("."))

    def test_a_peer_that_has_it_serves_a_verified_file(self):
        data = os.urandom(500000)
        sha, _ = put_product(self.peers["172.20.21.198"], data)
        code, record, dest = self.fetch(sha)
        self.assertEqual((code, record["peer"], record["bytes"]), (0, "cmux14", len(data)))
        self.assertEqual(dest.read_bytes(), data)
        self.assertEqual(self.leftovers(), [])

    def test_nobody_has_it_is_a_miss(self):
        code, record, dest = self.fetch("1" * 64)
        self.assertEqual((code, record["reason"]), (3, "no peer has it"))
        self.assertFalse(dest.exists())

    def test_a_peer_serving_wrong_bytes_never_reaches_dest(self):
        # A PR job on the peer rewrote its cache: metadata still names the digest, the bytes differ.
        data = os.urandom(10000)
        sha = hashlib.sha256(data).hexdigest()
        put_product(self.peers["172.20.21.197"], os.urandom(10000), digest=sha)
        code, record, dest = self.fetch(sha)
        self.assertEqual(code, 1)
        self.assertIn("sha256 mismatch", record["error"])
        self.assertFalse(dest.exists())
        self.assertEqual(self.leftovers(), [])
        # Another peer with the right bytes still serves it.
        put_product(self.peers["172.20.21.198"], data)
        code, record, dest = self.fetch(sha)
        self.assertEqual((code, record["peer"]), (0, "cmux14"))
        self.assertEqual(dest.read_bytes(), data)

    def test_short_silent_and_unreachable_peers_fall_back(self):
        data = os.urandom(5000)
        sha, _ = put_product(self.peers["172.20.21.197"], data)
        os.environ["FAKE_PEER_MODE_172_20_21_197"] = "short"
        code, record, dest = self.fetch(sha)
        self.assertEqual(code, 1)
        self.assertFalse(dest.exists())
        os.environ["FAKE_PEER_MODE_172_20_21_197"] = "silent"
        lf.TRANSFER_TIMEOUT = 1
        started = time.monotonic()
        code, record, dest = self.fetch(sha)
        self.assertLess(time.monotonic() - started, 20)
        self.assertEqual(code, 1)
        self.assertIn("timed out", record["error"])
        os.environ["FAKE_PEER_MODE_172_20_21_197"] = "unreachable"
        self.assertEqual(self.fetch(sha)[0], 3)
        self.assertEqual(self.leftovers(), [])

    def test_a_stalled_peer_delays_a_miss_but_not_a_hit(self):
        data = os.urandom(5000)
        sha, _ = put_product(self.peers["172.20.21.198"], data)
        os.environ["FAKE_PEER_MODE_172_20_21_197"] = "stalled"  # listed first, never answers its lookup
        self.addCleanup(setattr, lf, "LOOKUP_TIMEOUT", lf.LOOKUP_TIMEOUT)
        lf.LOOKUP_TIMEOUT = 3
        started = time.monotonic()
        code, record, dest = self.fetch(sha)
        self.assertEqual((code, record["peer"]), (0, "cmux14"))
        self.assertEqual(dest.read_bytes(), data)
        self.assertLess(time.monotonic() - started, 2.5)
        started = time.monotonic()
        self.assertEqual(self.fetch("2" * 64, "other")[0], 3)
        self.assertGreaterEqual(time.monotonic() - started, 3)

    def test_dest_is_never_replaced(self):
        sha, _ = put_product(self.peers["172.20.21.197"], b"x" * 100)
        (self.out / "taken").write_text("mine")
        code, record, dest = self.fetch(sha, "taken")
        self.assertEqual(code, 1)
        self.assertEqual(dest.read_text(), "mine")
        self.assertEqual(lf.fetch(sha, Path("relative"), lf.load_config(self.conf))[0], 1)
        self.assertEqual(lf.fetch("not-a-sha", self.out / "x", lf.load_config(self.conf))[0], 1)

    def test_no_config_is_a_miss_and_bad_configs_are_refused(self):
        self.assertEqual(lf.fetch("1" * 64, self.out / "x", None)[0], 3)
        good = json.loads(self.conf.read_text())
        for bad in ({**good, "peers": [{"name": "a", "address": "-oProxyCommand=x"}]}, {**good, "peers": []},
                    {**good, "user": "a b"}, {**good, "schema": 2}):
            with self.subTest(bad=str(bad)[:40]):
                self.conf.write_text(json.dumps(bad))
                self.assertIsNone(lf.load_config(self.conf))

    def test_ssh_pins_each_peers_host_key_and_forwards_nothing(self):
        config = lf.load_config(self.conf)
        argv = lf.ssh_command(config, config["peers"][0], "ping-v1")
        self.assertEqual(argv[:4], [lf.SSH, "-F", "/dev/null", "-T"])
        for option in ("HostKeyAlias=cmux13s-mac-mini", "StrictHostKeyChecking=yes", "IdentityAgent=none",
                       "ClearAllForwardings=yes", "GlobalKnownHostsFile=/dev/null", "BatchMode=yes"):
            self.assertIn(option, argv)

    def test_a_peer_announcing_more_than_the_callers_bound_is_not_asked(self):
        data = os.urandom(5000)
        sha, _ = put_product(self.peers["172.20.21.197"], data)
        code, record = lf.fetch(sha, self.out / "small", lf.load_config(self.conf), max_bytes=4999)
        self.assertEqual((code, record["reason"]), (3, "no peer has it"))
        self.assertEqual(lf.fetch(sha, self.out / "exact", lf.load_config(self.conf), max_bytes=5000)[0], 0)
        self.assertEqual(lf.fetch(sha, self.out / "x", lf.load_config(self.conf), max_bytes=0)[0], 1)

    def test_at_most_two_peers_are_tried_and_one_deadline_bounds_the_request(self):
        data = os.urandom(3000)
        sha = hashlib.sha256(data).hexdigest()
        third = Path(self.tmp.name) / "p3"
        (third / "objects").mkdir(parents=True)
        peers = {**{a: os.fspath(r) for a, r in self.peers.items()}, "172.20.21.199": os.fspath(third)}
        os.environ["FAKE_PEERS"] = json.dumps(peers)
        conf = json.loads(self.conf.read_text())
        conf["peers"].append({"name": "cmux11s-mac-mini", "address": "172.20.21.199"})
        self.conf.write_text(json.dumps(conf))
        for root in (self.peers["172.20.21.197"], self.peers["172.20.21.198"], third):
            put_product(Path(root), os.urandom(3000), digest=sha)  # everyone serves wrong bytes
        code, record, _ = self.fetch(sha)
        self.assertEqual(code, 1)
        transfers = [json.loads(l)[-1] for l in self.log.read_text().splitlines() if "product-v1" in l]
        self.assertEqual(len(transfers), lf.MAX_OFFERS)
        # A deadline already spent: no lookups get to run, and nothing is transferred.
        started = time.monotonic()
        code, record = lf.fetch(sha, self.out / "late", lf.load_config(self.conf), deadline=time.monotonic() - 1)
        self.assertEqual(code, 3)
        self.assertLess(time.monotonic() - started, 5)
        self.assertLess(lf.REQUEST_DEADLINE, 200)  # cmux waits 200 s for the helper

    def test_partial_files_are_unique_per_thread(self):
        sha, _ = put_product(self.peers["172.20.21.197"], os.urandom(100))
        names = []
        real_open = lf.os.open

        def spy(path, flags, *args, **kwargs):
            if ".lan-" in os.fspath(path):
                names.append(Path(path).name)
            return real_open(path, flags, *args, **kwargs)

        with unittest.mock.patch.object(lf.os, "open", spy):
            self.fetch(sha, "one")
            self.fetch(sha, "two")
        self.assertEqual(len(names), 2)
        for name in names:
            self.assertRegex(name, rf"^\.(one|two)\.lan-{os.getpid()}-{threading.get_ident()}-[0-9a-f]{{12}}$")

    def start_broker(self):
        sock = Path(tempfile.mkdtemp(prefix="glf", dir="/tmp")) / "s" / "fetch.sock"  # AF_UNIX path limit
        saved = lf.LAN_CONFIG
        lf.LAN_CONFIG = self.conf
        self.addCleanup(setattr, lf, "LAN_CONFIG", saved)
        ready = threading.Event()
        threading.Thread(target=lf.serve_local, args=(sock, ready), daemon=True).start()
        self.assertTrue(ready.wait(10))
        return sock

    def test_an_idle_broker_reexecutes_itself_once_its_file_changes(self):
        copy = Path(self.tmp.name) / "glaeda-lan-fetch-copy"
        copy.write_text("v1")
        sock = Path(tempfile.mkdtemp(prefix="glf", dir="/tmp")) / "s" / "fetch.sock"
        saved = lf.RELOAD_CHECK_SECONDS
        lf.RELOAD_CHECK_SECONDS = 0.1
        self.addCleanup(setattr, lf, "RELOAD_CHECK_SECONDS", saved)
        calls, ready = [], threading.Event()
        broker = threading.Thread(target=lf.serve_local, kwargs={"sock_path": sock, "ready": ready, "me": copy,
                                  "reexec": lambda *argv: calls.append(argv)}, daemon=True)
        broker.start()
        self.assertTrue(ready.wait(10))
        time.sleep(0.5)
        self.assertEqual(calls, [])  # unchanged: keeps serving
        os.utime(copy, (time.time() + 5, time.time() + 5))
        broker.join(10)
        self.assertFalse(broker.is_alive())
        self.assertEqual(calls[0][1][-2:], [os.fspath(copy), "serve-local"])
        self.assertFalse(sock.exists())

    def test_the_broker_stops_a_fetch_whose_client_left(self):
        sha, _ = put_product(self.peers["172.20.21.197"], os.urandom(100))
        os.environ["FAKE_PEER_MODE_172_20_21_197"] = "silent"
        pidfile = Path(self.tmp.name) / "silent.pid"
        os.environ["FAKE_PIDFILE"] = os.fspath(pidfile)
        sock = self.start_broker()
        import socket
        client = socket.socket(socket.AF_UNIX)
        client.connect(os.fspath(sock))
        client.sendall((json.dumps({"verb": "product", "sha256": sha, "dest": os.fspath(self.out / "gone")}) + "\n").encode())
        deadline = time.time() + 20
        while not (pidfile.exists() and pidfile.read_text()) and time.time() < deadline:
            time.sleep(0.1)
        pid = int(pidfile.read_text())
        client.close()  # the job gave up
        deadline = time.time() + 10
        while time.time() < deadline:
            try:
                os.kill(pid, 0)
            except ProcessLookupError:
                break
            time.sleep(0.1)
        else:
            os.kill(pid, 9)
            self.fail("the transfer outlived its client")
        self.assertFalse((self.out / "gone").exists())

    def test_the_broker_answers_busy_past_its_connection_bound(self):
        sha, _ = put_product(self.peers["172.20.21.197"], os.urandom(100))
        os.environ["FAKE_PEER_MODE_172_20_21_197"] = "silent"
        with unittest.mock.patch.object(lf, "BROKER_THREADS", 1):
            sock = self.start_broker()
            import socket
            holder = socket.socket(socket.AF_UNIX)
            holder.connect(os.fspath(sock))
            holder.sendall((json.dumps({"verb": "product", "sha256": sha, "dest": os.fspath(self.out / "h")}) + "\n").encode())
            time.sleep(1)
            started = time.monotonic()
            code, record = lf.broker_request(sha, self.out / "second", sock)
            self.assertEqual((code, record.get("error")), (1, "broker busy"))
            self.assertLess(time.monotonic() - started, 5)
            holder.close()

    def test_the_broker_fetches_for_a_job_over_its_socket(self):
        data = os.urandom(20000)
        sha, _ = put_product(self.peers["172.20.21.197"], data)
        sock = Path(tempfile.mkdtemp(prefix="glf", dir="/tmp")) / "s" / "fetch.sock"  # AF_UNIX path limit
        saved = lf.LAN_CONFIG
        lf.LAN_CONFIG = self.conf
        ready = threading.Event()
        threading.Thread(target=lf.serve_local, args=(sock, ready), daemon=True).start()
        try:
            self.assertTrue(ready.wait(10))
            self.assertEqual(sock.parent.stat().st_mode & 0o777, 0o700)
            code, record = lf.broker_request(sha, self.out / "via-broker", sock)
            self.assertEqual((code, record.get("via"), record.get("peer")), (0, "broker", "cmux13s-mac-mini"))
            self.assertEqual((self.out / "via-broker").read_bytes(), data)
            self.assertEqual(lf.broker_request("2" * 64, self.out / "miss", sock)[0], 3)
            import socket
            with socket.socket(socket.AF_UNIX) as raw:
                raw.connect(os.fspath(sock))
                raw.sendall(b'{"verb": "shell", "cmd": "id"}\n')
                self.assertEqual(json.loads(raw.recv(4096))["code"], 1)
        finally:
            lf.LAN_CONFIG = saved
        self.assertIsNone(lf.broker_request(sha, self.out / "x", sock.parent / "absent.sock"))

    def test_the_cli_prints_one_record_and_exits_with_the_code(self):
        data = os.urandom(1000)
        sha, _ = put_product(self.peers["172.20.21.197"], data)
        script = f"""
import importlib.machinery, sys
from pathlib import Path
m = importlib.machinery.SourceFileLoader('lf', {os.fspath(ROOT / 'scripts/glaeda-lan-fetch')!r}).load_module()
m.SSH = {lf.SSH!r}; m.LAN_CONFIG = Path({os.fspath(self.conf)!r}); m.SOCKET = Path('/nonexistent/sock')
sys.exit(m.main(sys.argv[1:]))
"""
        proc = subprocess.run([sys.executable, "-c", script, "product", sha, os.fspath(self.out / "cli")],
                              capture_output=True, text=True, timeout=60)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertEqual(json.loads(proc.stdout)["peer"], "cmux13s-mac-mini")
        proc = subprocess.run([sys.executable, "-c", script, "product", "3" * 64, os.fspath(self.out / "cli2")],
                              capture_output=True, text=True, timeout=60)
        self.assertEqual(proc.returncode, 3)

    def test_the_helper_runs_apples_python_isolated(self):
        first = (ROOT / "scripts/glaeda-lan-fetch").read_text().splitlines()[0]
        self.assertEqual(first, "#!/usr/bin/python3 -I")


if __name__ == "__main__":
    unittest.main()
