#!/usr/bin/env python3

import concurrent.futures
import contextlib
import datetime as dt
import importlib.util
import io
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest import mock


MODULE_PATH = Path(__file__).with_name("github_resident_snapshot.py")
SPEC = importlib.util.spec_from_file_location("github_resident_snapshot", MODULE_PATH)
assert SPEC and SPEC.loader
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)

BASE_TIME = dt.datetime(2026, 9, 21, 10, 0, tzinfo=dt.UTC)
GIT = Path("/usr/bin/git")
SSH_KEYGEN = Path("/usr/bin/ssh-keygen")


def fake_public_key(seed: str) -> str:
    return "ssh-ed25519 " + (seed * 68)[:68]


def trust(*, nodes=None):
    return {
        "document_type": MODULE.TRUST_DOCUMENT,
        "schema_version": 1,
        "repositories": ["teamleaderleo/glaeda"],
        "nodes": nodes
        or [
            {
                "id": "node-1111111111111111",
                "key_id": "key-aaaaaaaaaaaaaaaa",
                "ssh_public_key": fake_public_key("A"),
                "os_class": "linux",
                "architecture_class": "x86_64",
            }
        ],
    }


def capability(*, node_generation=7, glaeda="a", observed=BASE_TIME, expires=None, hot=True, os_class="linux", architecture="x86_64"):
    expires = expires or (observed + dt.timedelta(minutes=10))
    return {
        "schema": "glaeda-owned-workstation-capability/v1",
        "advisoryOnly": True,
        "authorizesDispatch": False,
        "authorizesExecution": False,
        "expiresAt": MODULE.format_time(expires),
        "node": {
            "architectureClass": architecture,
            "generation": node_generation,
            "id": "private-hostname-that-must-never-publish",
            "osClass": os_class,
        },
        "observedAt": MODULE.format_time(observed),
        "producer": {
            "glaedaRuntimeSha256": "sha256:" + glaeda * 64,
            "python": {"version": "3.14.7", "executableSha256": "sha256:" + "b" * 64},
            "workspaceCapabilitySha256": "sha256:" + "c" * 64,
        },
        "profiles": [
            {"class": "repo_query", "id": "repo-query/v1", "versionSha256": "sha256:" + "d" * 64},
            {"class": "verify_focused", "id": "verify-focused/v1", "versionSha256": "sha256:" + "e" * 64},
        ],
        "projects": [
            {
                "heatClass": "resident_hot" if hot else "resident_cold",
                "repository": "teamleaderleo/glaeda",
                "source": {"commitOid": "1" * 40, "treeOid": "2" * 40},
                "sourceObjectClass": "exact_commit_and_tree_present",
                "verificationProfiles": ["glaeda.required", "verify-focused/v1"],
            }
        ],
    }


def admission(outcome="ready", reason="compatible"):
    return {
        "document_type": "glaeda-owned-admission-observation",
        "schema_version": 1,
        "outcome": outcome,
        "reason": reason,
        "grants_authority": False,
        "authorizes_execution": False,
        "authorizes_redispatch": False,
    }


def request_state(state="running", receipt=None):
    return {
        "document_type": "glaeda-github-request-state-input",
        "schema_version": 1,
        "requests": [
            {
                "request_id": "req-1234567890abcdef",
                "source": {
                    "repository": "teamleaderleo/glaeda",
                    "commit_oid": "1" * 40,
                    "tree_oid": "2" * 40,
                },
                "profile": "verify-focused/v1",
                "state": state,
                "terminal_receipt_ref": receipt,
                "elapsed_class": "10s_to_1m",
                "estimate_class": "under_10s" if state == "running" else "unknown",
            }
        ],
    }


def reusable_state_summary(
    *,
    cache_class="incremental_build_state",
    generation="7",
    heat="hot",
    size="medium",
    recent_hit="within_hour",
    revalidation_required=False,
):
    generation_digest = generation if len(generation) == 64 else generation * 64
    return {
        "schema_version": 1,
        "cache_class": cache_class,
        "generation": "sha256:" + generation_digest,
        "heat": heat,
        "size": size,
        "recent_hit": recent_hit,
        "revalidation_required": revalidation_required,
    }


def project_state(*states):
    return {
        "document_type": "glaeda-github-project-heat-input",
        "schema_version": 1,
        "projects": [
            {
                "repository": "teamleaderleo/glaeda",
                "source": {"commit_oid": "1" * 40, "tree_oid": "2" * 40},
                "heat_class": "resident_hot",
                "verification_profiles": ["glaeda.required", "verify-focused/v1"],
                "dependency_build_state_class": "resident_generation",
                "reusable_states": list(states),
                "active_task_count": 0,
                "recent_compatible_receipt_ref": None,
            }
        ],
    }


def build_unsigned(**changes):
    values = {
        "capability": capability(),
        "admission": admission(),
        "trust_value": trust(),
        "public_node_id": "node-1111111111111111",
        "producer_generation": 4,
        "snapshot_sequence": 8,
        "observed_at": BASE_TIME + dt.timedelta(seconds=5),
        "published_at": BASE_TIME + dt.timedelta(seconds=6),
        "maximum_useful_age_seconds": 300,
        "request_state": request_state(state="queued"),
    }
    values.update(changes)
    return MODULE.build_unsigned_snapshot(**values)


def fake_signed(unsigned):
    value = dict(unsigned)
    value["signature"] = {
        "algorithm": "sshsig-ed25519",
        "key_id": unsigned["payload"]["producer"]["key_id"],
        "namespace": MODULE.SIGNING_NAMESPACE,
        "value": "-----BEGIN SSH SIGNATURE-----\nfake\n-----END SSH SIGNATURE-----\n",
    }
    return value


class ResidentSnapshotPureTests(unittest.TestCase):
    def test_compose_is_bounded_sanitized_and_zero_authority(self):
        snapshot = build_unsigned()
        payload = snapshot["payload"]
        self.assertEqual(payload["freshness"]["observed_at"], MODULE.format_time(BASE_TIME))
        self.assertEqual(payload["node"]["id"], "node-1111111111111111")
        self.assertEqual(payload["node"]["availability_class"], "available")
        self.assertEqual(payload["node"]["pressure_class"], "low")
        self.assertEqual(payload["node"]["capacity_class"], "available")
        self.assertEqual(payload["projects"][0]["heat_class"], "resident_hot")
        self.assertEqual(payload["projects"][0]["reusable_states"], [])
        self.assertEqual(payload["requests"][0]["state"], "queued")
        self.assertEqual(payload["authority"], {
            "advisory_only": True,
            "authorizes_dispatch": False,
            "authorizes_execution": False,
            "authorizes_host_selection": False,
            "authorizes_cleanup": False,
        })
        raw = MODULE.canonical_json(snapshot)
        self.assertLessEqual(len(raw), MODULE.MAX_NODE_BYTES)
        text = raw.decode()
        for forbidden in ("private-hostname", "/home/", "/Users/", "argv", "environment", "pid", "command"):
            self.assertNotIn(forbidden.lower(), text.lower())

    def test_reusable_state_summary_matches_lifecycle_public_contract(self):
        summary = reusable_state_summary()
        snapshot = build_unsigned(project_state=project_state(summary))
        published = snapshot["payload"]["projects"][0]["reusable_states"]
        self.assertEqual(published, [summary])
        raw = MODULE.canonical_json(snapshot).decode()
        for forbidden in ("/home/", "/Users/", "target/", "node_modules", "credential", "command_output"):
            self.assertNotIn(forbidden.lower(), raw.lower())

        duplicate = reusable_state_summary()
        with self.assertRaisesRegex(MODULE.SnapshotError, "identities must be unique"):
            build_unsigned(project_state=project_state(summary, duplicate))

        requires_reset = reusable_state_summary(revalidation_required=True)
        with self.assertRaisesRegex(MODULE.SnapshotError, "revalidation requires cold heat"):
            build_unsigned(project_state=project_state(requires_reset))

        cold_reset = reusable_state_summary(
            heat="cold",
            recent_hit="within_day",
            revalidation_required=True,
        )
        accepted = build_unsigned(project_state=project_state(cold_reset))
        self.assertTrue(
            accepted["payload"]["projects"][0]["reusable_states"][0]["revalidation_required"]
        )

    def test_cardinality_is_governed_by_document_bytes_not_magic_counts(self):
        nodes = [
            {
                "id": f"node-{index:016x}",
                "key_id": f"key-{index:016x}",
                "ssh_public_key": fake_public_key("A"),
                "os_class": "linux",
                "architecture_class": "x86_64",
            }
            for index in range(1, 15)
        ]
        reviewed = MODULE.validate_trust(trust(nodes=nodes))
        self.assertEqual(len(reviewed["nodes"]), 14)

        summaries = [
            reusable_state_summary(generation=f"{index:064x}")
            for index in range(1, 25)
        ]
        snapshot = build_unsigned(project_state=project_state(*summaries))
        self.assertEqual(
            len(snapshot["payload"]["projects"][0]["reusable_states"]),
            24,
        )
        self.assertLessEqual(len(MODULE.canonical_json(snapshot)), MODULE.MAX_NODE_BYTES)

        base = fake_signed(build_unsigned())
        fleet_nodes = []
        for index in range(1, 15):
            entry = json.loads(json.dumps(base))
            entry["payload"]["node"]["id"] = f"node-{index:016x}"
            entry["payload"]["producer"]["key_id"] = f"key-{index:016x}"
            entry["signature"]["key_id"] = f"key-{index:016x}"
            fleet_nodes.append(entry)
        fleet = {
            "document_type": MODULE.FLEET_DOCUMENT,
            "schema_version": 1,
            "nodes": fleet_nodes,
        }
        MODULE.validate_fleet(fleet)
        self.assertLessEqual(len(MODULE.canonical_json(fleet)), MODULE.MAX_FLEET_BYTES)

    def test_admission_reduces_only_to_bounded_classes(self):
        expected = {
            ("ready", "compatible"): ("available", "low", "available", 0),
            ("wait", "reserved"): ("available", "unknown", "reserved", 1),
            ("wait", "node_held"): ("held", "unknown", "unknown", 0),
            ("wait", "node_draining"): ("draining", "unknown", "unknown", 0),
            ("wait", "pressure_high"): ("available", "high", "insufficient", 0),
            ("wait", "capacity_unavailable"): ("available", "unknown", "insufficient", 0),
            ("refused", "observation_unavailable"): ("unknown", "unknown", "unknown", 0),
        }
        for pair, classes in expected.items():
            with self.subTest(pair=pair):
                value = MODULE.admission_projection(admission(*pair))
                self.assertEqual(
                    (value["availability_class"], value["pressure_class"], value["capacity_class"], value["active_work_count"]),
                    classes,
                )

    def test_old_project_observation_ages_entire_snapshot(self):
        old = BASE_TIME - dt.timedelta(seconds=301)
        value = build_unsigned(
            capability=capability(observed=old, expires=BASE_TIME + dt.timedelta(minutes=5)),
            observed_at=BASE_TIME,
            published_at=BASE_TIME,
        )
        self.assertEqual(value["payload"]["freshness"]["observed_at"], MODULE.format_time(old))
        with self.assertRaisesRegex(MODULE.SnapshotError, "stale"):
            MODULE.validate_unsigned_snapshot(value, trust(), now=BASE_TIME)

    def test_delayed_publication_is_stale_on_arrival(self):
        value = build_unsigned(
            capability=capability(observed=BASE_TIME),
            observed_at=BASE_TIME,
            published_at=BASE_TIME + dt.timedelta(seconds=301),
        )
        with self.assertRaisesRegex(MODULE.SnapshotError, "stale"):
            MODULE.validate_unsigned_snapshot(value, trust(), now=BASE_TIME + dt.timedelta(seconds=301))

    def test_project_heat_disappearance_is_a_semantic_transition(self):
        hot = fake_signed(build_unsigned(snapshot_sequence=8))
        cold_unsigned = build_unsigned(
            capability=capability(hot=False),
            snapshot_sequence=9,
            published_at=BASE_TIME + dt.timedelta(seconds=20),
        )
        cold = fake_signed(cold_unsigned)
        fleet = {"document_type": MODULE.FLEET_DOCUMENT, "schema_version": 1, "nodes": [hot]}
        with mock.patch.object(MODULE, "validate_signed_snapshot", side_effect=lambda value, *_a, **_kw: value):
            updated, reason = MODULE.upsert_fleet(fleet, cold, trust(), now=BASE_TIME + dt.timedelta(seconds=20))
        self.assertEqual(reason, "transition")
        self.assertEqual(updated["nodes"][0]["payload"]["projects"][0]["heat_class"], "cold")

    def test_old_producer_cannot_overwrite_newer_state(self):
        current = fake_signed(build_unsigned(producer_generation=5, snapshot_sequence=1))
        old = fake_signed(build_unsigned(producer_generation=4, snapshot_sequence=99))
        fleet = {"document_type": MODULE.FLEET_DOCUMENT, "schema_version": 1, "nodes": [current]}
        with mock.patch.object(MODULE, "validate_signed_snapshot", side_effect=lambda value, *_a, **_kw: value):
            with self.assertRaisesRegex(MODULE.SnapshotError, "older producer"):
                MODULE.upsert_fleet(fleet, old, trust(), now=BASE_TIME + dt.timedelta(seconds=10))

    def test_reboot_generation_restarts_sequence_and_changes_glaeda_generation(self):
        current = fake_signed(build_unsigned(producer_generation=5, snapshot_sequence=9))
        reboot = fake_signed(build_unsigned(
            capability=capability(node_generation=8, glaeda="f"),
            producer_generation=6,
            snapshot_sequence=1,
            published_at=BASE_TIME + dt.timedelta(seconds=30),
        ))
        fleet = {"document_type": MODULE.FLEET_DOCUMENT, "schema_version": 1, "nodes": [current]}
        with mock.patch.object(MODULE, "validate_signed_snapshot", side_effect=lambda value, *_a, **_kw: value):
            updated, reason = MODULE.upsert_fleet(fleet, reboot, trust(), now=BASE_TIME + dt.timedelta(seconds=30))
        self.assertEqual(reason, "transition")
        self.assertEqual(updated["nodes"][0]["payload"]["freshness"]["snapshot_sequence"], 1)
        self.assertEqual(updated["nodes"][0]["payload"]["producer"]["glaeda_generation"], "sha256:" + "f" * 64)

    def test_unchanged_refresh_is_suppressed_until_interval(self):
        current = fake_signed(build_unsigned(snapshot_sequence=8))
        candidate = fake_signed(build_unsigned(
            snapshot_sequence=9,
            observed_at=BASE_TIME + dt.timedelta(seconds=20),
            published_at=BASE_TIME + dt.timedelta(seconds=20),
            capability=capability(observed=BASE_TIME + dt.timedelta(seconds=20)),
        ))
        fleet = {"document_type": MODULE.FLEET_DOCUMENT, "schema_version": 1, "nodes": [current]}
        with mock.patch.object(MODULE, "validate_signed_snapshot", side_effect=lambda value, *_a, **_kw: value):
            with self.assertRaises(MODULE.PublicationSuppressed):
                MODULE.upsert_fleet(fleet, candidate, trust(), now=BASE_TIME + dt.timedelta(seconds=20))

    def test_running_request_requires_matching_local_active_work(self):
        with self.assertRaisesRegex(MODULE.SnapshotError, "active work"):
            build_unsigned(request_state=request_state(state="running"), admission=admission())
        value = build_unsigned(
            request_state=request_state(state="running"),
            admission=admission("wait", "reserved"),
        )
        self.assertEqual(value["payload"]["node"]["active_work_count"], 1)
        self.assertEqual(value["payload"]["requests"][0]["state"], "running")

    def test_terminal_state_requires_bounded_receipt(self):
        with self.assertRaisesRegex(MODULE.SnapshotError, "terminal receipt"):
            build_unsigned(request_state=request_state(state="terminal", receipt=None))
        value = build_unsigned(request_state=request_state(state="terminal", receipt="sha256:" + "9" * 64))
        self.assertEqual(value["payload"]["requests"][0]["state"], "terminal")


@unittest.skipUnless(GIT.exists() and SSH_KEYGEN.exists(), "OpenSSH signing integration unavailable")
class ResidentSnapshotSigningAndTransportTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="glaeda-status-test-")
        self.root = Path(self.temp.name)

    def tearDown(self):
        self.temp.cleanup()

    def key(self, name):
        path = self.root / name
        subprocess.run(
            [str(SSH_KEYGEN), "-q", "-t", "ed25519", "-N", "", "-f", str(path)],
            check=True,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        public = Path(str(path) + ".pub").read_text(encoding="ascii").strip()
        return path, public

    def one_signed(
        self,
        *,
        now=BASE_TIME,
        sequence=1,
        generation=1,
        hot=True,
        request=None,
        admission_value=None,
        project=None,
    ):
        key, public = self.key(f"key-{sequence}-{generation}")
        trust_value = trust(nodes=[{
            "id": "node-1111111111111111",
            "key_id": "key-aaaaaaaaaaaaaaaa",
            "ssh_public_key": public,
            "os_class": "linux",
            "architecture_class": "x86_64",
        }])
        unsigned = build_unsigned(
            capability=capability(observed=now, hot=hot),
            admission=admission_value or admission(),
            trust_value=trust_value,
            producer_generation=generation,
            snapshot_sequence=sequence,
            observed_at=now,
            published_at=now,
            project_state=project,
            request_state=request if request is not None else request_state(state="queued"),
        )
        return MODULE.sign_snapshot(unsigned, trust_value, private_key=key, ssh_keygen=SSH_KEYGEN), trust_value, key

    def signed_for(
        self,
        trust_value,
        key,
        node_id,
        *,
        sequence,
        generation=1,
        now=BASE_TIME,
        os_class="linux",
        architecture="x86_64",
    ):
        return MODULE.sign_snapshot(
            MODULE.build_unsigned_snapshot(
                capability(
                    observed=now,
                    os_class=os_class,
                    architecture=architecture,
                ),
                admission(),
                trust_value,
                public_node_id=node_id,
                producer_generation=generation,
                snapshot_sequence=sequence,
                observed_at=now,
                published_at=now,
            ),
            trust_value,
            private_key=key,
            ssh_keygen=SSH_KEYGEN,
        )

    def test_signature_round_trip_and_manual_forgery_degrades_to_unknown(self):
        signed, trust_value, _ = self.one_signed()
        fleet = {"document_type": MODULE.FLEET_DOCUMENT, "schema_version": 1, "nodes": [signed]}
        view = MODULE.consume_fleet(fleet, trust_value, now=BASE_TIME + dt.timedelta(seconds=10), ssh_keygen=SSH_KEYGEN)
        self.assertEqual(view["nodes"][0]["freshness_class"], "fresh")
        forged = json.loads(json.dumps(signed))
        forged["payload"]["projects"][0]["heat_class"] = "cold"
        view = MODULE.consume_fleet(
            {"document_type": MODULE.FLEET_DOCUMENT, "schema_version": 1, "nodes": [forged]},
            trust_value,
            now=BASE_TIME + dt.timedelta(seconds=10),
            ssh_keygen=SSH_KEYGEN,
        )
        self.assertEqual(view["nodes"][0]["freshness_class"], "unknown")
        self.assertEqual(view["nodes"][0]["reason"], "untrusted")
        self.assertEqual(view["nodes"][0]["projects"], [])

    def test_reusable_state_summary_round_trips_to_agent_view(self):
        summary = reusable_state_summary(
            cache_class="package_manager_state",
            generation="8",
            heat="warm",
            size="small",
            recent_hit="within_day",
        )
        signed, trust_value, _ = self.one_signed(project=project_state(summary))
        fleet = {"document_type": MODULE.FLEET_DOCUMENT, "schema_version": 1, "nodes": [signed]}
        view = MODULE.consume_fleet(
            fleet,
            trust_value,
            now=BASE_TIME + dt.timedelta(seconds=10),
            ssh_keygen=SSH_KEYGEN,
        )
        project = view["nodes"][0]["projects"][0]
        self.assertEqual(project["reusable_states"], [summary])

    def test_node_dies_after_available_and_lost_terminal_update_degrade_to_unknown(self):
        signed, trust_value, key = self.one_signed(
            request=request_state(state="running"),
            admission_value=admission("wait", "reserved"),
        )
        terminal_time = BASE_TIME + dt.timedelta(seconds=30)
        terminal_unsigned = MODULE.build_unsigned_snapshot(
            capability(observed=terminal_time),
            admission(),
            trust_value,
            public_node_id="node-1111111111111111",
            producer_generation=1,
            snapshot_sequence=2,
            observed_at=terminal_time,
            published_at=terminal_time,
            request_state=request_state(state="terminal", receipt="sha256:" + "9" * 64),
        )
        terminal = MODULE.sign_snapshot(terminal_unsigned, trust_value, private_key=key, ssh_keygen=SSH_KEYGEN)
        self.assertEqual(terminal["payload"]["requests"][0]["state"], "terminal")
        # Simulate the terminal GitHub update being lost: the remote fleet still contains sequence 1.
        fleet = {"document_type": MODULE.FLEET_DOCUMENT, "schema_version": 1, "nodes": [signed]}
        stale = MODULE.consume_fleet(fleet, trust_value, now=BASE_TIME + dt.timedelta(seconds=301), ssh_keygen=SSH_KEYGEN)
        self.assertEqual(stale["nodes"][0]["freshness_class"], "unknown")
        self.assertEqual(stale["nodes"][0]["reason"], "stale")
        self.assertEqual(stale["nodes"][0]["requests"], [])

    def test_signed_snapshot_and_fleet_byte_measurements(self):
        signed, trust_value, _ = self.one_signed(
            project=project_state(reusable_state_summary())
        )
        fleet = {"document_type": MODULE.FLEET_DOCUMENT, "schema_version": 1, "nodes": [signed]}
        node_bytes = len(MODULE.canonical_json(signed))
        fleet_bytes = len(MODULE.canonical_json(fleet))
        self.assertLessEqual(node_bytes, MODULE.MAX_NODE_BYTES)
        self.assertLessEqual(fleet_bytes, MODULE.MAX_FLEET_BYTES)
        print(f"MEASURE github_resident_snapshot node_bytes={node_bytes} fleet_bytes={fleet_bytes} reusable_state_count=1 agent_reads=2 successful_write_remote_round_trips=2 idle_refresh_seconds={MODULE.DEFAULT_REFRESH_INTERVAL_SECONDS}")

    def test_removed_trust_node_does_not_block_other_node_publication(self):
        node_a = "node-1111111111111111"
        node_b = "node-2222222222222222"
        key_a, pub_a = self.key("removed-node-a")
        key_b, pub_b = self.key("removed-node-b")
        old_trust = trust(nodes=[
            {
                "id": node_a,
                "key_id": "key-aaaaaaaaaaaaaaaa",
                "ssh_public_key": pub_a,
                "os_class": "linux",
                "architecture_class": "x86_64",
            },
            {
                "id": node_b,
                "key_id": "key-bbbbbbbbbbbbbbbb",
                "ssh_public_key": pub_b,
                "os_class": "macos",
                "architecture_class": "arm64",
            },
        ])
        current_trust = trust(nodes=[old_trust["nodes"][1]])
        old_a = self.signed_for(
            old_trust, key_a, node_a, sequence=1
        )
        old_b = self.signed_for(
            old_trust,
            key_b,
            node_b,
            sequence=1,
            os_class="macos",
            architecture="arm64",
        )
        update_time = BASE_TIME + dt.timedelta(seconds=10)
        new_b = self.signed_for(
            current_trust,
            key_b,
            node_b,
            sequence=2,
            now=update_time,
            os_class="macos",
            architecture="arm64",
        )
        fleet = {
            "document_type": MODULE.FLEET_DOCUMENT,
            "schema_version": 1,
            "nodes": [old_a, old_b],
        }

        updated, reason = MODULE.upsert_fleet(
            fleet,
            new_b,
            current_trust,
            now=update_time,
            refresh_interval_seconds=30,
            ssh_keygen=SSH_KEYGEN,
        )

        self.assertEqual(reason, "transition")
        self.assertEqual(
            [item["payload"]["node"]["id"] for item in updated["nodes"]],
            [node_b],
        )
        view = MODULE.consume_fleet(
            updated,
            current_trust,
            now=update_time + dt.timedelta(seconds=5),
            ssh_keygen=SSH_KEYGEN,
        )
        self.assertEqual(view["nodes"][0]["freshness_class"], "fresh")

    def test_rotated_key_allows_other_publish_then_current_key_replacement(self):
        node_a = "node-1111111111111111"
        node_b = "node-2222222222222222"
        old_key_a, old_pub_a = self.key("rotated-node-a-old")
        new_key_a, new_pub_a = self.key("rotated-node-a-new")
        key_b, pub_b = self.key("rotated-node-b")
        old_trust = trust(nodes=[
            {
                "id": node_a,
                "key_id": "key-aaaaaaaaaaaaaaaa",
                "ssh_public_key": old_pub_a,
                "os_class": "linux",
                "architecture_class": "x86_64",
            },
            {
                "id": node_b,
                "key_id": "key-bbbbbbbbbbbbbbbb",
                "ssh_public_key": pub_b,
                "os_class": "macos",
                "architecture_class": "arm64",
            },
        ])
        current_trust = trust(nodes=[
            {
                "id": node_a,
                "key_id": "key-cccccccccccccccc",
                "ssh_public_key": new_pub_a,
                "os_class": "linux",
                "architecture_class": "x86_64",
            },
            old_trust["nodes"][1],
        ])
        old_a = self.signed_for(
            old_trust, old_key_a, node_a, sequence=7, generation=9
        )
        old_b = self.signed_for(
            old_trust,
            key_b,
            node_b,
            sequence=1,
            os_class="macos",
            architecture="arm64",
        )
        fleet = {
            "document_type": MODULE.FLEET_DOCUMENT,
            "schema_version": 1,
            "nodes": [old_a, old_b],
        }

        update_time = BASE_TIME + dt.timedelta(seconds=30)
        new_b = self.signed_for(
            current_trust,
            key_b,
            node_b,
            sequence=2,
            now=update_time,
            os_class="macos",
            architecture="arm64",
        )
        after_b, _ = MODULE.upsert_fleet(
            fleet,
            new_b,
            current_trust,
            now=update_time,
            refresh_interval_seconds=30,
            ssh_keygen=SSH_KEYGEN,
        )
        interim = MODULE.consume_fleet(
            after_b,
            current_trust,
            now=update_time + dt.timedelta(seconds=5),
            ssh_keygen=SSH_KEYGEN,
        )
        by_id = {item["id"]: item for item in interim["nodes"]}
        self.assertEqual(by_id[node_a]["freshness_class"], "unknown")
        self.assertEqual(by_id[node_a]["reason"], "untrusted")
        self.assertEqual(by_id[node_b]["freshness_class"], "fresh")

        bad_a = self.signed_for(
            current_trust,
            new_key_a,
            node_a,
            sequence=2,
            generation=10,
            now=update_time,
        )
        with self.assertRaisesRegex(MODULE.SnapshotError, "restart snapshot sequence at one"):
            MODULE.upsert_fleet(
                after_b,
                bad_a,
                current_trust,
                now=update_time,
                ssh_keygen=SSH_KEYGEN,
            )

        replacement_time = BASE_TIME + dt.timedelta(seconds=60)
        new_a = self.signed_for(
            current_trust,
            new_key_a,
            node_a,
            sequence=1,
            generation=10,
            now=replacement_time,
        )
        updated, reason = MODULE.upsert_fleet(
            after_b,
            new_a,
            current_trust,
            now=replacement_time,
            ssh_keygen=SSH_KEYGEN,
        )
        self.assertEqual(reason, "transition")
        final = MODULE.consume_fleet(
            updated,
            current_trust,
            now=replacement_time + dt.timedelta(seconds=5),
            ssh_keygen=SSH_KEYGEN,
        )
        self.assertTrue(
            all(item["freshness_class"] == "fresh" for item in final["nodes"])
        )

    def test_trust_transition_does_not_allow_cross_node_forgery(self):
        node_a = "node-1111111111111111"
        node_b = "node-2222222222222222"
        key_a, pub_a = self.key("forgery-node-a")
        key_b, pub_b = self.key("forgery-node-b")
        trust_value = trust(nodes=[
            {
                "id": node_a,
                "key_id": "key-aaaaaaaaaaaaaaaa",
                "ssh_public_key": pub_a,
                "os_class": "linux",
                "architecture_class": "x86_64",
            },
            {
                "id": node_b,
                "key_id": "key-bbbbbbbbbbbbbbbb",
                "ssh_public_key": pub_b,
                "os_class": "macos",
                "architecture_class": "arm64",
            },
        ])
        snap_a = self.signed_for(trust_value, key_a, node_a, sequence=1)
        snap_b = self.signed_for(
            trust_value,
            key_b,
            node_b,
            sequence=1,
            os_class="macos",
            architecture="arm64",
        )
        forged = json.loads(json.dumps(snap_b))
        forged["payload"]["node"]["id"] = node_a
        forged["payload"]["node"]["os_class"] = "linux"
        forged["payload"]["node"]["architecture_class"] = "x86_64"
        forged["payload"]["producer"]["key_id"] = "key-aaaaaaaaaaaaaaaa"
        forged["signature"]["key_id"] = "key-aaaaaaaaaaaaaaaa"
        fleet = {
            "document_type": MODULE.FLEET_DOCUMENT,
            "schema_version": 1,
            "nodes": [snap_a, snap_b],
        }

        with self.assertRaisesRegex(MODULE.SnapshotError, "signature verification failed"):
            MODULE.upsert_fleet(
                fleet,
                forged,
                trust_value,
                now=BASE_TIME + dt.timedelta(seconds=10),
                ssh_keygen=SSH_KEYGEN,
            )

    def test_two_nodes_can_publish_concurrently_and_converge(self):
        remote = self.root / "remote.git"
        subprocess.run([str(GIT), "init", "--bare", "--quiet", str(remote)], check=True)
        repos = []
        for index in (1, 2):
            repo = self.root / f"repo-{index}"
            subprocess.run([str(GIT), "init", "--quiet", str(repo)], check=True)
            subprocess.run([str(GIT), "-C", str(repo), "remote", "add", "origin", str(remote)], check=True)
            repos.append(repo)

        key1, pub1 = self.key("node1")
        key2, pub2 = self.key("node2")
        trust_value = trust(nodes=[
            {
                "id": "node-1111111111111111",
                "key_id": "key-aaaaaaaaaaaaaaaa",
                "ssh_public_key": pub1,
                "os_class": "linux",
                "architecture_class": "x86_64",
            },
            {
                "id": "node-2222222222222222",
                "key_id": "key-bbbbbbbbbbbbbbbb",
                "ssh_public_key": pub2,
                "os_class": "macos",
                "architecture_class": "arm64",
            },
        ])
        snap1 = MODULE.sign_snapshot(
            MODULE.build_unsigned_snapshot(
                capability(observed=BASE_TIME), admission(), trust_value,
                public_node_id="node-1111111111111111", producer_generation=1, snapshot_sequence=1,
                observed_at=BASE_TIME, published_at=BASE_TIME,
            ),
            trust_value, private_key=key1, ssh_keygen=SSH_KEYGEN,
        )
        snap2 = MODULE.sign_snapshot(
            MODULE.build_unsigned_snapshot(
                capability(observed=BASE_TIME, os_class="macos", architecture="arm64"), admission(), trust_value,
                public_node_id="node-2222222222222222", producer_generation=1, snapshot_sequence=1,
                observed_at=BASE_TIME, published_at=BASE_TIME,
            ),
            trust_value, private_key=key2, ssh_keygen=SSH_KEYGEN,
        )
        def publish(args):
            snapshot, repo = args
            return MODULE.publish_snapshot(
                snapshot, trust_value, repository_root=repo, now=BASE_TIME,
                git=GIT, ssh_keygen=SSH_KEYGEN, retries=3,
            )
        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
            receipts = list(pool.map(publish, [(snap1, repos[0]), (snap2, repos[1])]))
        self.assertTrue(all(item["state"] == "published" for item in receipts))
        head = subprocess.check_output([str(GIT), "--git-dir", str(remote), "rev-parse", f"refs/heads/{MODULE.STATUS_BRANCH}"], text=True).strip()
        raw = subprocess.check_output([str(GIT), "--git-dir", str(remote), "show", f"{head}:{MODULE.STATUS_FILE}"])
        fleet = json.loads(raw)
        view = MODULE.consume_fleet(fleet, trust_value, now=BASE_TIME + dt.timedelta(seconds=5), ssh_keygen=SSH_KEYGEN)
        self.assertEqual([item["id"] for item in view["nodes"]], ["node-1111111111111111", "node-2222222222222222"])
        self.assertTrue(all(item["freshness_class"] == "fresh" for item in view["nodes"]))


class ResidentSnapshotErrorChannelTests(unittest.TestCase):
    """The CLI's error channel is held to the same privacy ceiling as the published document."""

    def run_cli(self, argv):
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr):
            code = MODULE.main(argv)
        return code, stderr.getvalue()

    def assert_no_private_paths(self, text):
        for forbidden in ("/home/", "/Users/", "/tmp/", "secret", "Traceback"):
            self.assertNotIn(forbidden.lower(), text.lower())

    def test_a_missing_repository_root_refuses_without_naming_it(self):
        with tempfile.TemporaryDirectory() as scratch:
            snapshot_path = Path(scratch) / "snapshot.json"
            trust_path = Path(scratch) / "trust.json"
            snapshot_path.write_text(json.dumps(fake_signed(build_unsigned())), encoding="utf-8")
            trust_path.write_text(json.dumps(trust()), encoding="utf-8")
            code, text = self.run_cli([
                "publish",
                "--snapshot", str(snapshot_path),
                "--trust", str(trust_path),
                "--repository-root", "/nonexistent-private-workspace-xyz",
            ])
        self.assertEqual(code, 1)
        self.assertNotIn("nonexistent-private-workspace-xyz", text)
        self.assert_no_private_paths(text)
        self.assertEqual(json.loads(text)["error"], "repository root is unavailable")

    def test_a_missing_signing_key_refuses_without_naming_it(self):
        with self.assertRaises(MODULE.SnapshotError) as caught:
            MODULE.sign_snapshot(
                build_unsigned(),
                trust(),
                private_key=Path("/nonexistent-private-key-xyz"),
                ssh_keygen=SSH_KEYGEN,
            )
        self.assertNotIn("nonexistent-private-key-xyz", str(caught.exception))
        self.assertEqual(str(caught.exception), "snapshot signing key is unavailable")

    def test_an_unverifiable_rotation_entry_is_not_recommitted_verbatim(self):
        # A fleet entry that fails verification is retained under the rotation branch. It is
        # evidence of nothing and it gets recommitted under the next honest publisher's
        # authorship, so an injected blob there would both launder and, once the fleet crossed
        # the read ceiling, stop every node from publishing again.
        node_a = "node-1111111111111111"
        node_b = "node-2222222222222222"
        trust_value = trust(nodes=[
            {"id": node_a, "key_id": "key-aaaaaaaaaaaaaaaa", "ssh_public_key": fake_public_key("a"),
             "os_class": "linux", "architecture_class": "x86_64"},
            {"id": node_b, "key_id": "key-bbbbbbbbbbbbbbbb", "ssh_public_key": fake_public_key("b"),
             "os_class": "linux", "architecture_class": "x86_64"},
        ])
        candidate = fake_signed(build_unsigned(trust_value=trust_value, public_node_id=node_b))
        junk = {
            "document_type": MODULE.NODE_DOCUMENT,
            "schema_version": MODULE.SCHEMA_VERSION,
            "payload": {
                "node": {"id": node_a},
                "producer": {"key_id": "key-9999999999999999"},
                "filler": "x" * (MODULE.MAX_NODE_BYTES + 1),
            },
            "signature": {"key_id": "key-9999999999999999"},
        }
        fleet = {"document_type": MODULE.FLEET_DOCUMENT, "schema_version": MODULE.SCHEMA_VERSION,
                 "nodes": [junk]}

        def verify(value, *_args, **_kwargs):
            if value is junk:
                raise MODULE.SnapshotError("snapshot signature verification failed")
            return value

        with mock.patch.object(MODULE, "validate_signed_snapshot", side_effect=verify):
            with self.assertRaisesRegex(MODULE.SnapshotError, "exceeds the byte ceiling"):
                MODULE.upsert_fleet(fleet, candidate, trust_value, now=BASE_TIME)

        smuggled = dict(junk)
        smuggled["payload"] = {"node": {"id": node_a}, "producer": {"key_id": "key-9999999999999999"}}
        smuggled["extra"] = "smuggled"
        fleet["nodes"] = [smuggled]

        def verify_smuggled(value, *_args, **_kwargs):
            if value is smuggled:
                raise MODULE.SnapshotError("snapshot signature verification failed")
            return value

        with mock.patch.object(MODULE, "validate_signed_snapshot", side_effect=verify_smuggled):
            with self.assertRaisesRegex(MODULE.SnapshotError, "unsupported fields"):
                MODULE.upsert_fleet(fleet, candidate, trust_value, now=BASE_TIME)

    def test_the_subprocess_ceiling_can_read_any_fleet_the_validator_accepts(self):
        # remote_fleet reads the status file through git show, so a subprocess ceiling at or below
        # the fleet ceiling would wedge every node's publish once an accepted fleet crossed it.
        self.assertGreater(MODULE.MAX_GIT_OUTPUT_BYTES, MODULE.MAX_FLEET_BYTES)


if __name__ == "__main__":
    unittest.main(verbosity=2)
