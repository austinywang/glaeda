#!/usr/bin/env python3
from __future__ import annotations

import contextlib
import copy
import io
import json
import fcntl
import importlib.util
import os
from pathlib import Path
import tempfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]

MODULE_PATH = Path(__file__).with_name("cmux_fleet.py")
SPEC = importlib.util.spec_from_file_location("cmux_fleet", MODULE_PATH)
assert SPEC and SPEC.loader
f = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(f)

A = "sha256:" + "a" * 64
B = "sha256:" + "b" * 64
C = "sha256:" + "c" * 64
D = "sha256:" + "d" * 64
E = "sha256:" + "e" * 64
COMMIT = "1" * 40


def enrollment(os_family="macos", state="eligible", roles=None):
    if roles is None:
        roles = (
            ["cmux_macos_native_build"]
            if os_family == "macos"
            else ["cmux_linux_ci"]
        )
    return {
        "schema": f.ENROLLMENT_SCHEMA,
        "nodeId": "cmux-fixture-001",
        "architecture": "arm64" if os_family == "macos" else "x86_64",
        "os": {
            "family": os_family,
            "versionClass": "macos-26" if os_family == "macos" else "ubuntu-24.04",
        },
        "hardwareCapabilityClass": (
            "cmux-mac-build-large" if os_family == "macos" else "cmux-linux-ci-medium"
        ),
        "supportedToolchainGenerations": [A, B],
        "roleProfiles": {
            role: dict(
                f.ROLE_PROFILES.get(
                    role,
                    {"id": "reserved.placeholder", "generation": 1},
                )
            )
            for role in sorted(roles)
        },
        "allowedExecutionRoles": sorted(roles),
        "operatorFleetScope": "cmux-founders",
        "enrollmentGeneration": 3,
        "glaedaGeneration": C,
        "state": state,
        "quarantineReason": None,
    }


def bootstrap_for(enrollment_value, toolchain=A):
    return {
        "schema": f.BOOTSTRAP_SCHEMA,
        "platform": enrollment_value["os"]["family"],
        "architecture": enrollment_value["architecture"],
        "osVersionClass": enrollment_value["os"]["versionClass"],
        "hardwareCapabilityClass": enrollment_value["hardwareCapabilityClass"],
        "roles": list(enrollment_value["allowedExecutionRoles"]),
        "glaedaGeneration": enrollment_value["glaedaGeneration"],
        "toolchainGeneration": toolchain,
        "roleProfiles": copy.deepcopy(enrollment_value["roleProfiles"]),
        "checks": {"ready": True},
        "observed": {},
        "eligibleForEnrollment": True,
        "blockingChecks": [],
        "authority": "observation_only",
    }


def cmux_result(
    role="cmux_macos_native_build",
    *,
    state="passed",
    cleanup_state=None,
    process_group_settled=None,
    profile=None,
):
    chosen_profile = dict(profile or f.ROLE_PROFILES[role])
    if cleanup_state is None:
        cleanup_state = "forced" if state == "ambiguous" else "complete"
    if process_group_settled is None:
        process_group_settled = state != "ambiguous"
    exit_code = {
        "passed": 0,
        "failed": 1,
        "timed_out": 124,
        "ambiguous": 0,
    }[state]
    observations = {"fixture": "cmux-test"}
    result = {
        "document_type": f.CMUX_RESULT_DOCUMENT_TYPE,
        "schema_version": f.CMUX_RESULT_SCHEMA_VERSION,
        "source": {
            "repository": f.CMUX_REPOSITORY,
            "commit": COMMIT,
            "tree": "2" * 40,
        },
        "profile": chosen_profile,
        "semantic_validator": "cmux.fixture/v1",
        "environment_class": (
            "isolated-build"
            if role == "cmux_macos_native_build"
            else "isolated-portable"
        ),
        "expected_result_class": "cmux.fixture-result/v1",
        "result": state,
        "parameters": {},
        "runtime_input_identities": [],
        "artifact_identities": [],
        "validation": {
            "missing_required_artifact_classes": [],
        },
        "stage_timings": [{"stage": "test", "seconds": 1.25}],
        "resource_summary": {
            "resource_class": "cmux-fixture",
            "cpu_count": 4,
            "memory_bytes": 16 * 1024**3,
            "architecture": "arm64" if role == "cmux_macos_native_build" else "x86_64",
        },
        "toolchain": {
            "identity": f.cmux_digest(observations),
            "observations": observations,
        },
        "benchmark": {
            "state_class": "cold",
            "semantic_comparison_key": A,
            "comparison_context_key": B,
        },
        "network_class": "none",
        "timeout_class": "fixture",
        "cleanup": {
            "state": cleanup_state,
            "process_group_settled": process_group_settled,
        },
        "exit_code": exit_code,
        "started_at_unix_millis": 1000,
        "ended_at_unix_millis": 2250,
    }
    semantic = f._cmux_semantic_key(result)
    result["benchmark"]["semantic_comparison_key"] = semantic
    result["benchmark"]["comparison_context_key"] = f._cmux_context_key(
        semantic,
        "cold",
        result["toolchain"]["identity"],
    )
    return result


def finalized(
    enrollment_value,
    role=None,
    *,
    result=None,
    toolchain=A,
    post_bootstrap=None,
):
    role = role or enrollment_value["allowedExecutionRoles"][0]
    return f.finalize_acceptance(
        enrollment_value,
        role,
        toolchain,
        result or cmux_result(role),
        post_bootstrap or bootstrap_for(enrollment_value, toolchain),
        execution_class=f.LOCAL_EXECUTION_CLASS,
        local_execution_attempt_sha256=E,
    )


class FleetTests(unittest.TestCase):
    def setUp(self):
        # create=True so this suite can also be run against a revision that
        # predates the operator notice, which is how its binding is measured.
        self.real_notice = getattr(f, "_notice", None)
        notices = mock.patch.object(f, "_notice", create=True)
        self.notices = notices.start()
        self.addCleanup(notices.stop)

    def test_current_accepted_role_is_eligible(self):
        e = enrollment()
        r = finalized(e)
        status = f.node_status(e, [r])
        by_role = {v["role"]: v for v in status["roles"]}
        self.assertTrue(by_role["cmux_macos_native_build"]["eligible"])
        self.assertEqual(set(by_role), {"cmux_macos_native_build"})
        self.assertTrue(status["routingCandidateEligible"])
        self.assertFalse(status["automaticDispatchAuthorized"])

    def test_enrollment_presence_does_not_grant_role(self):
        status = f.node_status(enrollment(), [])
        self.assertFalse(status["routingCandidateEligible"])
        self.assertTrue(all(not r["eligible"] for r in status["roles"]))

    def test_non_routable_states_exclude_every_role(self):
        for state in ("draining", "quarantined", "retired"):
            with self.subTest(state=state):
                e = enrollment(state=state)
                if state == "quarantined":
                    e["quarantineReason"] = "disk_pressure"
                receipt = finalized(
                    {**e, "state": "eligible", "quarantineReason": None}
                )
                status = f.node_status(e, [receipt])
                self.assertFalse(status["routingCandidateEligible"])
                self.assertTrue(
                    all(r["reason"] == f"node_{state}" for r in status["roles"])
                )

    def test_null_glaeda_generation_is_rejected(self):
        e = enrollment()
        e["glaedaGeneration"] = None
        with self.assertRaisesRegex(f.FleetError, "Glaeda generation is invalid"):
            f.validate_enrollment(e)

        receipt = finalized(enrollment())
        receipt["glaedaGeneration"] = None
        with self.assertRaisesRegex(
            f.FleetError, "acceptance receipt Glaeda generation is invalid"
        ):
            f.validate_acceptance_receipt(receipt)

    def test_stale_acceptance_generation_is_ineligible(self):
        e = enrollment()
        r = finalized(e)
        r["enrollmentGeneration"] = 2
        status = f.node_status(e, [r])
        native = next(
            v for v in status["roles"] if v["role"] == "cmux_macos_native_build"
        )
        self.assertEqual(native["reason"], "acceptance_enrollment_stale")

    def test_fleet_contract_change_stales_acceptance(self):
        e = enrollment()
        receipt = finalized(e)
        self.assertEqual(
            receipt["glaedaFleetContractGeneration"],
            f.fleet_contract_generation(),
        )
        with mock.patch.object(
            f,
            "fleet_contract_generation",
            return_value=D,
        ):
            status = f.node_status(e, [receipt])
        native = next(
            value
            for value in status["roles"]
            if value["role"] == "cmux_macos_native_build"
        )
        self.assertFalse(native["eligible"])
        self.assertEqual(
            native["reason"],
            "acceptance_glaeda_contract_stale",
        )

    def test_profile_policy_change_requires_fresh_enrollment(self):
        e = enrollment()
        receipt = finalized(e)
        with mock.patch.dict(
            f.ROLE_PROFILES,
            {
                "cmux_macos_native_build": {
                    "id": "cmux.macos.dev-check",
                    "generation": 2,
                },
                "cmux_linux_ci": dict(f.ROLE_PROFILES["cmux_linux_ci"]),
            },
            clear=True,
        ):
            with self.assertRaisesRegex(f.FleetError, "not the reviewed v1 profile"):
                f.node_status(e, [receipt])

    def test_private_identity_fields_are_rejected(self):
        for field in ("hostname", "serialNumber", "privateIp", "username"):
            with self.subTest(field=field):
                e = enrollment()
                e[field] = "secret-ish"
                with self.assertRaisesRegex(f.FleetError, "unknown or missing"):
                    f.validate_enrollment(e)

    def test_role_without_reviewed_workload_is_rejected(self):
        with self.assertRaisesRegex(f.FleetError, "reviewed v1 profile"):
            f.validate_enrollment(
                enrollment(roles=["artifact_cache"])
            )

    def test_os_role_mismatch_is_rejected(self):
        with self.assertRaisesRegex(f.FleetError, "incompatible"):
            f.validate_enrollment(
                enrollment("linux", roles=["cmux_macos_native_build"])
            )

    def test_eligible_transition_requires_current_acceptance(self):
        e = enrollment(state="enrolling")
        with self.assertRaisesRegex(f.FleetError, "accepted role receipt"):
            f.transition(e, "eligible", None)
        receipt = finalized(e)
        eligible = f.transition(e, "eligible", None, [receipt])
        self.assertEqual(eligible["state"], "eligible")

    def test_quarantine_and_reenroll_path(self):
        e = enrollment()
        q = f.transition(e, "quarantined", "service_mismatch")
        self.assertEqual(q["quarantineReason"], "service_mismatch")
        enrolling = f.transition(q, "enrolling", None)
        self.assertIsNone(enrolling["quarantineReason"])
        self.assertEqual(enrolling["state"], "enrolling")
        self.assertEqual(
            enrolling["enrollmentGeneration"],
            e["enrollmentGeneration"] + 1,
        )

    def test_acceptance_binds_current_identity(self):
        e = enrollment()
        with self.assertRaisesRegex(f.FleetError, "toolchain generation"):
            f.finalize_acceptance(
                e,
                "cmux_macos_native_build",
                D,
                cmux_result(),
                bootstrap_for(e, D),
            )

        wrong_profile = {
            "id": "cmux.macos.dev-check",
            "generation": 2,
        }
        with self.assertRaisesRegex(f.FleetError, "differs from enrolled role profile"):
            finalized(e, result=cmux_result(profile=wrong_profile))

    def test_acceptance_rejects_detached_semantic_result_digest(self):
        e = enrollment()
        with self.assertRaisesRegex(
            f.FleetError, "semantic result digest does not match"
        ):
            f.finalize_acceptance(
                e,
                "cmux_macos_native_build",
                A,
                cmux_result(),
                bootstrap_for(e),
                D,
                execution_class=f.LOCAL_EXECUTION_CLASS,
                local_execution_attempt_sha256=E,
            )

    def test_acceptance_requires_fresh_matching_post_bootstrap(self):
        e = enrollment()

        changed_toolchain = bootstrap_for(e, B)
        with self.assertRaisesRegex(
            f.FleetError,
            "post-acceptance bootstrap toolchain differs",
        ):
            finalized(e, post_bootstrap=changed_toolchain)

        changed_glaeda = bootstrap_for(e)
        changed_glaeda["glaedaGeneration"] = D
        with self.assertRaisesRegex(
            f.FleetError,
            "post-acceptance bootstrap differs",
        ):
            finalized(e, post_bootstrap=changed_glaeda)

        changed_hardware = bootstrap_for(e)
        changed_hardware["hardwareCapabilityClass"] = "cmux-mac-other"
        with self.assertRaisesRegex(
            f.FleetError,
            "post-acceptance bootstrap differs",
        ):
            finalized(e, post_bootstrap=changed_hardware)

    def test_acceptance_receipt_binds_post_bootstrap_and_cmux_context(self):
        e = enrollment()
        result = cmux_result()
        receipt = finalized(e, result=result)
        self.assertEqual(
            receipt["postBootstrapSha256"],
            f.digest(bootstrap_for(e)),
        )
        self.assertEqual(
            receipt["cmuxToolchainIdentity"],
            result["toolchain"]["identity"],
        )
        self.assertEqual(
            receipt["cmuxEnvironmentClass"],
            result["environment_class"],
        )
        self.assertEqual(
            receipt["glaedaFleetContractGeneration"],
            f.fleet_contract_generation(),
        )

    def test_acceptance_receipt_execution_binding_is_closed(self):
        e = enrollment()
        receipt = finalized(e)

        missing_attempt = copy.deepcopy(receipt)
        missing_attempt["localExecutionAttemptSha256"] = None
        with self.assertRaisesRegex(
            f.FleetError,
            "local execution evidence is inconsistent",
        ):
            f.validate_acceptance_receipt(missing_attempt)

        external_with_attempt = copy.deepcopy(receipt)
        external_with_attempt["executionClass"] = f.EXTERNAL_EVIDENCE_CLASS
        with self.assertRaisesRegex(
            f.FleetError,
            "local execution evidence is inconsistent",
        ):
            f.validate_acceptance_receipt(external_with_attempt)

    def test_external_semantic_evidence_cannot_mint_accepted_receipt(self):
        e = enrollment()
        receipt = f.finalize_acceptance(
            e,
            "cmux_macos_native_build",
            A,
            cmux_result(),
            bootstrap_for(e),
        )
        self.assertEqual(receipt["executionClass"], f.EXTERNAL_EVIDENCE_CLASS)
        self.assertIsNone(receipt["localExecutionAttemptSha256"])
        self.assertEqual(receipt["result"], "rejected")
        self.assertFalse(f.node_status(e, [receipt])["routingCandidateEligible"])

    def test_git_oid_uses_closed_environment(self):
        observed = {}

        def fake_run(argv, **kwargs):
            observed.update(kwargs)
            return __import__("subprocess").CompletedProcess(
                argv,
                0,
                stdout=COMMIT + "\n",
                stderr="",
            )

        with (
            mock.patch.dict(
                f.os.environ,
                {
                    "PATH": "/attacker/bin",
                    "HOME": "/attacker/home",
                    "PYTHONPATH": "/attacker/python",
                    "SSH_AUTH_SOCK": "/private/agent.sock",
                    "SECRET_SENTINEL": "do-not-forward",
                    "LD_PRELOAD": "/attacker/lib.so",
                    "GIT_DIR": "/attacker/git",
                    "GIT_CONFIG_GLOBAL": "/attacker/config",
                },
                clear=True,
            ),
            mock.patch.object(f.subprocess, "run", side_effect=fake_run),
        ):
            value = f._git_oid(Path("/cmux"), "HEAD^{commit}")

        self.assertEqual(value, COMMIT)
        self.assertEqual(
            observed["env"],
            {
                "LC_ALL": "C",
                "LANG": "C",
                "GIT_CONFIG_GLOBAL": "/dev/null",
                "GIT_CONFIG_NOSYSTEM": "1",
            },
        )
        for forbidden in (
            "PATH",
            "HOME",
            "PYTHONPATH",
            "SSH_AUTH_SOCK",
            "SECRET_SENTINEL",
            "LD_PRELOAD",
            "GIT_DIR",
        ):
            self.assertNotIn(forbidden, observed["env"])

    def test_acceptance_child_environment_is_explicit_allowlist(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            with mock.patch.dict(
                f.os.environ,
                {
                    "PATH": "/reviewed/bin:/usr/bin:/bin",
                    "HOME": str(root / "home"),
                    "CARGO_HOME": str(root / "cargo"),
                    "RUSTUP_HOME": str(root / "rustup"),
                    "DEVELOPER_DIR": "/Applications/Xcode.app/Contents/Developer",
                    "PYTHONPATH": "/attacker/python",
                    "PYTHONHOME": "/attacker/home",
                    "SSH_AUTH_SOCK": "/private/agent.sock",
                    "SECRET_SENTINEL": "secret",
                    "GIT_CONFIG_GLOBAL": "/attacker/gitconfig",
                },
                clear=True,
            ):
                environment = f.acceptance_child_environment(root / "tmp")

        self.assertEqual(
            set(environment),
            {
                "LC_ALL",
                "LANG",
                "TMPDIR",
                "PATH",
                "HOME",
                "CARGO_HOME",
                "RUSTUP_HOME",
                "DEVELOPER_DIR",
            },
        )
        self.assertEqual(environment["LC_ALL"], "C")
        self.assertEqual(environment["LANG"], "C")
        self.assertEqual(environment["PATH"], "/reviewed/bin:/usr/bin:/bin")
        self.assertNotIn("PYTHONPATH", environment)
        self.assertNotIn("PYTHONHOME", environment)
        self.assertNotIn("SSH_AUTH_SOCK", environment)
        self.assertNotIn("SECRET_SENTINEL", environment)
        self.assertNotIn("GIT_CONFIG_GLOBAL", environment)

    def test_accept_local_binds_profile_run_to_this_node(self):
        e = enrollment("linux", state="enrolling")
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            root.chmod(0o700)
            enrollment_path = root / "enrollment.json"
            enrollment_path.write_bytes(f.canonical(e))
            enrollment_path.chmod(0o600)

            cmux_root = root / "cmux"
            runner = cmux_root / "scripts/ci/cmux_workload_profile.py"
            runner.parent.mkdir(parents=True)
            runner.write_text("# fixture\n", encoding="utf-8")

            glaeda = root / "glaeda"
            glaeda.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            glaeda.chmod(0o755)

            result = cmux_result("cmux_linux_ci")
            post = bootstrap_for(e)
            child_environments = []

            def fake_run(argv, **kwargs):
                joined = " ".join(str(item) for item in argv)
                self.assertGreaterEqual(len(argv), 3)
                self.assertEqual(argv[1], "-I")
                environment = dict(kwargs["env"])
                child_environments.append(environment)
                for forbidden in (
                    "PYTHONPATH",
                    "PYTHONHOME",
                    "SSH_AUTH_SOCK",
                    "SECRET_SENTINEL",
                    "GIT_DIR",
                    "GIT_WORK_TREE",
                ):
                    self.assertNotIn(forbidden, environment)
                self.assertEqual(environment["LC_ALL"], "C")
                self.assertEqual(environment["LANG"], "C")
                self.assertEqual(environment["PATH"], "/reviewed/bin:/usr/bin:/bin")
                self.assertEqual(environment["HOME"], str(root / "home"))
                self.assertEqual(environment["CARGO_HOME"], str(root / "cargo"))
                self.assertEqual(environment["RUSTUP_HOME"], str(root / "rustup"))
                self.assertEqual(
                    environment["DEVELOPER_DIR"],
                    "/Applications/Xcode.app/Contents/Developer",
                )
                tmpdir = Path(environment["TMPDIR"])
                self.assertEqual(tmpdir.name, "tmp")
                self.assertEqual(tmpdir.stat().st_mode & 0o777, 0o700)

                if "cmux_workload_profile.py" in joined:
                    self.assertIn("--state-class", argv)
                    self.assertEqual(argv[argv.index("--state-class") + 1], "cold")
                    self.assertNotIn("--state-root", argv)
                    result_path = Path(argv[argv.index("--result") + 1])
                    result_path.write_bytes(f.canonical(result))
                    result_path.chmod(0o600)
                    return __import__("subprocess").CompletedProcess(argv, 0)
                if "cmux_fleet_bootstrap.py" in joined:
                    return __import__("subprocess").CompletedProcess(
                        argv,
                        0,
                        stdout=f.canonical(post),
                        stderr=b"",
                    )
                raise AssertionError(argv)

            environment = {
                "PATH": "/reviewed/bin:/usr/bin:/bin",
                "HOME": str(root / "home"),
                "CARGO_HOME": str(root / "cargo"),
                "RUSTUP_HOME": str(root / "rustup"),
                "DEVELOPER_DIR": "/Applications/Xcode.app/Contents/Developer",
                "LANG": "en_US.UTF-8",
                "LC_ALL": "en_US.UTF-8",
                "TMPDIR": str(root / "ambient-tmp"),
                "PYTHONPATH": "/attacker/python",
                "PYTHONHOME": "/attacker/home",
                "SSH_AUTH_SOCK": "/private/agent.sock",
                "SECRET_SENTINEL": "do-not-forward",
                "GIT_DIR": "/attacker/git",
                "GIT_WORK_TREE": "/attacker/tree",
            }
            with (
                mock.patch.dict(f.os.environ, environment, clear=True),
                mock.patch.object(
                    f,
                    "_git_oid",
                    side_effect=[COMMIT, "2" * 40, COMMIT, "2" * 40],
                ),
                mock.patch.object(f.subprocess, "run", side_effect=fake_run),
            ):
                receipt = f.accept_local(
                    enrollment_path,
                    cmux_root,
                    glaeda,
                    "cmux_linux_ci",
                )

            self.assertEqual(len(child_environments), 2)
            self.assertEqual(child_environments[0], child_environments[1])

        self.assertEqual(receipt["result"], "accepted")
        self.assertEqual(receipt["executionClass"], f.LOCAL_EXECUTION_CLASS)
        self.assertRegex(
            receipt["localExecutionAttemptSha256"],
            r"^sha256:[0-9a-f]{64}$",
        )
        self.assertEqual(
            receipt["cmuxSemanticResultSha256"],
            f.digest(result),
        )

    def test_local_acceptance_rejects_different_result_or_changed_checkout_source(self):
        for change in ("result_commit", "result_tree", "checkout_commit", "checkout_tree"):
            with self.subTest(change=change), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary).resolve()
                root.chmod(0o700)
                e = enrollment("linux", state="enrolling")
                enrollment_path = root / "enrollment.json"
                enrollment_path.write_bytes(f.canonical(e))
                enrollment_path.chmod(0o600)
                cmux_root = root / "cmux"
                runner = cmux_root / "scripts/ci/cmux_workload_profile.py"
                runner.parent.mkdir(parents=True)
                runner.write_text("# fixture\n")
                glaeda = root / "glaeda"
                glaeda.write_text("#!/bin/sh\nexit 0\n")
                glaeda.chmod(0o755)
                result = cmux_result("cmux_linux_ci")
                if change.startswith("result_"):
                    result["source"][change.removeprefix("result_")] = "3" * 40
                    semantic = f._cmux_semantic_key(result)
                    result["benchmark"]["semantic_comparison_key"] = semantic
                    result["benchmark"]["comparison_context_key"] = f._cmux_context_key(
                        semantic, "cold", result["toolchain"]["identity"])
                observations = [COMMIT, "2" * 40, COMMIT, "2" * 40]
                if change == "checkout_commit":
                    observations[2] = "3" * 40
                if change == "checkout_tree":
                    observations[3] = "3" * 40

                def run(argv, **kwargs):
                    if str(runner) in argv:
                        path = Path(argv[argv.index("--result") + 1])
                        path.write_bytes(f.canonical(result))
                        path.chmod(0o600)
                        return f.subprocess.CompletedProcess(argv, 0)
                    return f.subprocess.CompletedProcess(
                        argv, 0, stdout=f.canonical(bootstrap_for(e)), stderr=b"")

                with mock.patch.object(f, "_git_oid", side_effect=observations), \
                        mock.patch.object(f.subprocess, "run", side_effect=run):
                    with self.assertRaisesRegex(f.FleetError, "source"):
                        f.accept_local(enrollment_path, cmux_root, glaeda, "cmux_linux_ci")

    def _local_acceptance_fixture(self, root):
        e = enrollment("linux", state="enrolling")
        enrollment_path = root / "enrollment.json"
        enrollment_path.write_bytes(f.canonical(e))
        enrollment_path.chmod(0o600)
        cmux_root = root / "cmux"
        runner = cmux_root / "scripts/ci/cmux_workload_profile.py"
        runner.parent.mkdir(parents=True)
        runner.write_text("# fixture\n")
        glaeda = root / "glaeda"
        glaeda.write_text("#!/bin/sh\nexit 0\n")
        glaeda.chmod(0o755)
        return e, enrollment_path, cmux_root, runner, glaeda

    def test_rejected_attempt_keeps_the_evidence_an_acceptance_discards(self):
        # A rejected receipt carries a verdict and no cause, so deleting the
        # runner log with the attempt leaves the operator nothing to read.
        for state, retained in (("failed", True), ("passed", False)):
            with self.subTest(state=state), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary).resolve()
                root.chmod(0o700)
                e, enrollment_path, cmux_root, runner, glaeda = (
                    self._local_acceptance_fixture(root)
                )
                result = cmux_result("cmux_linux_ci", state=state)

                def run(argv, **kwargs):
                    if str(runner) in argv:
                        path = Path(argv[argv.index("--result") + 1])
                        (path.parent / "tmp/scratch").mkdir(parents=True, exist_ok=True)
                        kwargs["stdout"].write(b"zig 0.16.0 is required\n")
                        path.write_bytes(f.canonical(result))
                        path.chmod(0o600)
                        return f.subprocess.CompletedProcess(
                            argv, 0 if state == "passed" else 1
                        )
                    return f.subprocess.CompletedProcess(
                        argv, 0, stdout=f.canonical(bootstrap_for(e)), stderr=b"")

                with (
                    mock.patch.object(
                        f, "_git_oid", side_effect=[COMMIT, "2" * 40, COMMIT, "2" * 40]
                    ),
                    mock.patch.object(f.subprocess, "run", side_effect=run),
                ):
                    receipt = f.accept_local(
                        enrollment_path, cmux_root, glaeda, "cmux_linux_ci"
                    )

                self.assertEqual(receipt["result"] == "accepted", not retained)
                self.assertEqual(list(root.glob(f.ATTEMPT_PREFIX + "*")), [])
                kept = list(root.glob(f.RETAINED_ATTEMPT_PREFIX + "*"))
                self.assertEqual(len(kept), 1 if retained else 0)
                if retained:
                    self.notices.assert_called_once()
                    self.assertIn(
                        str(kept[0]), self.notices.call_args.args[0]
                    )
                    self.assertIn(
                        "zig 0.16.0 is required",
                        (kept[0] / "cmux-runner.log").read_text(encoding="utf-8"),
                    )
                    self.assertTrue((kept[0] / "result.json").is_file())
                    # The child's scratch tree is the large part and rebuilds.
                    self.assertFalse((kept[0] / "tmp").exists())

    def test_unwritable_scratch_tree_is_removed_not_swallowed(self):
        # A build leaves read-only directories behind; rmtree(ignore_errors=True)
        # would silently keep the whole tree and the storage bound with it.
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            locked = root / "attempt/tmp/DerivedData/locked"
            locked.mkdir(parents=True)
            (locked / "artifact").write_text("x", encoding="utf-8")
            locked.chmod(0o500)
            # Only needed if the removal under test fails; otherwise the
            # TemporaryDirectory cleanup would inherit the locked tree.
            self.addCleanup(f._remove_tree, root / "attempt")
            self.assertTrue(f._remove_tree(root / "attempt"))
            self.assertFalse((root / "attempt").exists())

    def test_removal_never_reaches_outside_the_tree(self):
        # os.walk does not traverse a symlink but os.chmod follows one, so a
        # build that links its TMPDIR at a toolchain or a Cargo registry would
        # have that directory's bits relaxed by a cleanup that does not own it.
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            outside = root / "outside"
            (outside / "keep").mkdir(parents=True)
            outside.chmod(0o500)
            attempt = root / "attempt/tmp"
            attempt.mkdir(parents=True)
            (attempt / "registry").symlink_to(outside)
            self.assertTrue(f._remove_tree(root / "attempt"))
            self.assertEqual(outside.stat().st_mode & 0o777, 0o500)
            self.assertTrue((outside / "keep").is_dir())
            # Restore before the temporary directory tries to remove it.
            outside.chmod(0o700)

    def test_removal_answers_honestly_for_links_and_unreadable_roots(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            dangling = root / "dangling"
            dangling.symlink_to(root / "never")
            self.assertTrue(f._remove_tree(dangling))
            self.assertFalse(dangling.is_symlink())

            # os.walk cannot list an unreadable directory, so repairing only
            # what it yields would leave the top entry behind.
            sealed = root / "sealed"
            (sealed / "inner").mkdir(parents=True)
            sealed.chmod(0o000)
            self.assertTrue(f._remove_tree(sealed))
            self.assertFalse(sealed.exists())

    def test_pruning_ranks_only_the_directories_it_created(self):
        # The doc tells operators this evidence is theirs to keep, so they will
        # archive it under the same name. An archive must not consume the
        # retention budget and push real evidence out of it.
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            for index in range(f.RETAINED_ATTEMPT_LIMIT):
                kept = root / f"{f.RETAINED_ATTEMPT_PREFIX}dir{index}"
                kept.mkdir()
                (kept / "cmux-runner.log").write_text(str(index), encoding="utf-8")
                os.utime(kept, (index + 1, index + 1))
            for name in ("archive.tar.gz", "elsewhere"):
                path = root / f"{f.RETAINED_ATTEMPT_PREFIX}{name}"
                if name.endswith(".tar.gz"):
                    path.write_text("archived", encoding="utf-8")
                else:
                    path.symlink_to(root)
                os.utime(path, (99, 99), follow_symlinks=False)

            f._prune_retained_attempts(root)

            for index in range(f.RETAINED_ATTEMPT_LIMIT):
                self.assertTrue(
                    (root / f"{f.RETAINED_ATTEMPT_PREFIX}dir{index}"
                     / "cmux-runner.log").is_file()
                )
            self.assertTrue((root / f"{f.RETAINED_ATTEMPT_PREFIX}archive.tar.gz").is_file())
            self.assertTrue((root / f"{f.RETAINED_ATTEMPT_PREFIX}elsewhere").is_symlink())

    def test_notice_never_raises_and_never_writes_to_stdout(self):
        # _notice runs from a finally. A closed fd 2 leaves sys.stderr as None,
        # and print(file=None) would put a private path in front of the receipt.
        captured = io.StringIO()
        with (
            mock.patch.object(f.sys, "stderr", None),
            contextlib.redirect_stdout(captured),
        ):
            self.real_notice("attempt retained")
        self.assertEqual(captured.getvalue(), "")

        class Broken:
            def write(self, _value):
                raise BrokenPipeError(32, "Broken pipe")

            def flush(self):
                raise BrokenPipeError(32, "Broken pipe")

        with (
            mock.patch.object(f.sys, "stderr", Broken()),
            contextlib.redirect_stdout(captured),
        ):
            self.real_notice("attempt retained")
        self.assertEqual(captured.getvalue(), "")

    def test_retention_keeps_evidence_when_the_name_is_taken(self):
        # The whole point of retaining is that the operator has something to
        # read, so a name collision must never be resolved by deleting it.
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            attempt = root / f"{f.ATTEMPT_PREFIX}abcd1234"
            (attempt / "tmp").mkdir(parents=True)
            (attempt / "cmux-runner.log").write_text("cause", encoding="utf-8")
            (root / f"{f.RETAINED_ATTEMPT_PREFIX}abcd1234").write_text(
                "operator file", encoding="utf-8"
            )
            f._retain_attempt(attempt, root)
            self.assertEqual(
                (attempt / "cmux-runner.log").read_text(encoding="utf-8"), "cause"
            )
            self.assertIn("kept rejected acceptance attempt in place",
                          self.notices.call_args.args[0])

    def test_pruning_survives_an_unreadable_retained_entry(self):
        # _retain_attempt runs from a finally: anything it raises replaces the
        # receipt or the error that explains the rejection.
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            (root / f"{f.RETAINED_ATTEMPT_PREFIX}dangling").symlink_to(
                root / "gone"
            )
            attempt = root / f"{f.ATTEMPT_PREFIX}abcd1234"
            (attempt / "tmp").mkdir(parents=True)
            (attempt / "cmux-runner.log").write_text("cause", encoding="utf-8")
            f._retain_attempt(attempt, root)
            self.assertTrue(
                (root / f"{f.RETAINED_ATTEMPT_PREFIX}abcd1234"
                 / "cmux-runner.log").is_file()
            )

    def test_retained_attempts_stay_bounded(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            for index in range(f.RETAINED_ATTEMPT_LIMIT + 2):
                attempt = root / f"{f.ATTEMPT_PREFIX}{index:08d}"
                (attempt / "tmp").mkdir(parents=True)
                (attempt / "cmux-runner.log").write_text(str(index), encoding="utf-8")
                os.utime(attempt, (index + 1, index + 1))
                f._retain_attempt(attempt, root)
            kept = sorted(path.name for path in root.glob(f.RETAINED_ATTEMPT_PREFIX + "*"))
            self.assertEqual(len(kept), f.RETAINED_ATTEMPT_LIMIT)
            self.assertEqual(kept[-1], f"{f.RETAINED_ATTEMPT_PREFIX}00000004")

    def test_pruning_leaves_operator_directories_alone(self):
        # The doc promises that evidence an operator renames, copies or keeps
        # under the prefix is theirs: never ranked, never removed.
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            kept_by_operator = [
                root / f"{f.RETAINED_ATTEMPT_PREFIX}abcd1234.investigating",
                root / f"{f.RETAINED_ATTEMPT_PREFIX}old-investigation",
            ]
            for index, directory in enumerate(kept_by_operator):
                directory.mkdir()
                (directory / "notes.txt").write_text("mine", encoding="utf-8")
                # Oldest and newest: either would decide a naive ranking.
                os.utime(directory, (1, 1) if index else (10**10, 10**10))
            for index in range(f.RETAINED_ATTEMPT_LIMIT):
                attempt = root / f"{f.ATTEMPT_PREFIX}{index:08d}"
                (attempt / "tmp").mkdir(parents=True)
                f._retain_attempt(attempt, root)
            for directory in kept_by_operator:
                self.assertTrue((directory / "notes.txt").is_file())
            for index in range(f.RETAINED_ATTEMPT_LIMIT):
                self.assertTrue(
                    (root / f"{f.RETAINED_ATTEMPT_PREFIX}{index:08d}").is_dir()
                )

    def test_retention_never_raises_from_an_unsearchable_attempt(self):
        # Path.is_symlink re-raises EACCES before Python 3.14. The child holds
        # the attempt path, so it can revoke search on it before the finally.
        if os.geteuid() == 0:
            self.skipTest("root ignores directory search permission")
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            attempt = root / f"{f.ATTEMPT_PREFIX}abcd1234"
            (attempt / "tmp").mkdir(parents=True)
            attempt.chmod(0o600)
            try:
                f._retain_attempt(attempt, root)
                # The scratch tree could not be reached, so it survived; the
                # operator must be told rather than shown a clean retention.
                self.assertIn("could not be removed",
                              self.notices.call_args.args[0])
            finally:
                for candidate in (attempt, root / f"{f.RETAINED_ATTEMPT_PREFIX}abcd1234"):
                    if candidate.exists():
                        candidate.chmod(0o700)

    def test_interpreter_without_waitid_refuses_before_the_attempt(self):
        # CPython exposes os.waitid on macOS only from 3.13; the CMUX profile
        # runner cannot wait on its child without it.
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            root.chmod(0o700)
            _, enrollment_path, cmux_root, _, glaeda = (
                self._local_acceptance_fixture(root)
            )
            with mock.patch.object(
                f, "profile_runner_interpreter_ready", return_value=False
            ):
                with self.assertRaisesRegex(f.FleetError, "os.waitid"):
                    f.accept_local(enrollment_path, cmux_root, glaeda, "cmux_linux_ci")
            self.assertEqual(list(root.glob(f.ATTEMPT_PREFIX + "*")), [])

    def test_accept_local_refuses_fleet_contract_replacement(self):
        e = enrollment("linux", state="enrolling")
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            root.chmod(0o700)
            enrollment_path = root / "enrollment.json"
            enrollment_path.write_bytes(f.canonical(e))
            enrollment_path.chmod(0o600)

            cmux_root = root / "cmux"
            runner = cmux_root / "scripts/ci/cmux_workload_profile.py"
            runner.parent.mkdir(parents=True)
            runner.write_text("# fixture\n", encoding="utf-8")

            glaeda = root / "glaeda"
            glaeda.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            glaeda.chmod(0o755)

            result = cmux_result("cmux_linux_ci")
            post = bootstrap_for(e)

            def fake_run(argv, **kwargs):
                joined = " ".join(str(item) for item in argv)
                if "cmux_workload_profile.py" in joined:
                    result_path = Path(argv[argv.index("--result") + 1])
                    result_path.write_bytes(f.canonical(result))
                    result_path.chmod(0o600)
                    return __import__("subprocess").CompletedProcess(argv, 0)
                if "cmux_fleet_bootstrap.py" in joined:
                    return __import__("subprocess").CompletedProcess(
                        argv,
                        0,
                        stdout=f.canonical(post),
                        stderr=b"",
                    )
                raise AssertionError(argv)

            with (
                mock.patch.object(
                    f,
                    "_git_oid",
                    side_effect=[COMMIT, "2" * 40, COMMIT, "2" * 40],
                ),
                mock.patch.object(f.subprocess, "run", side_effect=fake_run),
                mock.patch.object(
                    f,
                    "fleet_contract_generation",
                    side_effect=[A, D],
                ),
            ):
                with self.assertRaisesRegex(
                    f.FleetError,
                    "fleet contract changed during local acceptance",
                ):
                    f.accept_local(
                        enrollment_path,
                        cmux_root,
                        glaeda,
                        "cmux_linux_ci",
                    )

    def test_failed_settlement_rejects_role(self):
        e = enrollment()
        receipt = finalized(
            e,
            result=cmux_result(state="ambiguous"),
        )
        self.assertEqual(receipt["result"], "rejected")
        self.assertEqual(receipt["processSettlement"], "incomplete")
        status = f.node_status(e, [receipt])
        native = next(
            v for v in status["roles"] if v["role"] == "cmux_macos_native_build"
        )
        self.assertFalse(native["eligible"])

    def test_cmux_semantic_failure_never_accepts_role(self):
        for state in ("failed", "timed_out", "ambiguous"):
            with self.subTest(state=state):
                e = enrollment()
                receipt = finalized(e, result=cmux_result(state=state))
                self.assertEqual(receipt["result"], "rejected")
                self.assertEqual(receipt["cmuxSemanticResultState"], state)
                self.assertFalse(f.node_status(e, [receipt])["routingCandidateEligible"])

    def test_timed_out_result_preserves_actual_child_exit_code(self):
        e = enrollment()
        result = cmux_result(state="timed_out")
        result["exit_code"] = -15
        receipt = finalized(e, result=result)
        self.assertEqual(receipt["result"], "rejected")
        self.assertEqual(receipt["cmuxSemanticResultState"], "timed_out")

    def test_cmux_semantic_result_requires_positive_cpu_count(self):
        e = enrollment()
        result = cmux_result()
        result["resource_summary"]["cpu_count"] = None
        with self.assertRaisesRegex(f.FleetError, "resource summary"):
            finalized(e, result=result)

    def test_cmux_semantic_result_rejects_inconsistent_evidence(self):
        e = enrollment()
        cases = []

        extra = cmux_result()
        extra["unexpected"] = True
        cases.append(("unknown or missing", extra))

        toolchain = cmux_result()
        toolchain["toolchain"]["observations"]["fixture"] = "changed"
        cases.append(("toolchain identity", toolchain))

        semantic = cmux_result()
        semantic["benchmark"]["semantic_comparison_key"] = D
        cases.append(("semantic comparison", semantic))

        environment = cmux_result()
        environment["environment_class"] = "isolated-portable"
        cases.append(("semantic comparison", environment))

        passed_forced = cmux_result()
        passed_forced["cleanup"] = {
            "state": "forced",
            "process_group_settled": False,
        }
        cases.append(("terminal CMUX result", passed_forced))

        missing = cmux_result()
        missing["validation"]["missing_required_artifact_classes"] = [
            "cmux.required/v1"
        ]
        cases.append(("passed CMUX result", missing))

        for message, result in cases:
            with self.subTest(message=message):
                with self.assertRaisesRegex(f.FleetError, message):
                    finalized(e, result=result)

    def test_linux_fixture_can_be_eligible(self):
        e = enrollment("linux")
        r = finalized(e, "cmux_linux_ci")
        status = f.node_status(e, [r])
        linux = next(v for v in status["roles"] if v["role"] == "cmux_linux_ci")
        self.assertTrue(linux["eligible"])

    def test_status_rejects_forged_acceptance_receipt(self):
        e = enrollment()
        forged = finalized(e)
        forged["cmuxSemanticResultState"] = "failed"
        with self.assertRaisesRegex(f.FleetError, "disagrees"):
            f.node_status(e, [forged])

    def test_status_rejects_unknown_acceptance_fields(self):
        e = enrollment()
        receipt = finalized(e)
        receipt["hostname"] = "hidden-host"
        with self.assertRaisesRegex(f.FleetError, "unknown or missing"):
            f.node_status(e, [receipt])

    def test_enrollment_is_derived_from_accepted_bootstrap(self):
        bootstrap = {
            "schema": f.BOOTSTRAP_SCHEMA,
            "platform": "macos",
            "architecture": "arm64",
            "osVersionClass": "macos-26",
            "hardwareCapabilityClass": "cmux-mac-build-large",
            "roles": ["cmux_macos_native_build"],
            "glaedaGeneration": C,
            "toolchainGeneration": A,
            "roleProfiles": {"cmux_macos_native_build": dict(f.ROLE_PROFILES["cmux_macos_native_build"])},
            "checks": {"ready": True},
            "observed": {},
            "eligibleForEnrollment": True,
            "blockingChecks": [],
            "authority": "observation_only",
        }
        result = f.enrollment_from_bootstrap(
            bootstrap,
            node_id="cmux-fixture-002",
            operator_fleet_scope="cmux-founders",
            enrollment_generation=1,
        )
        self.assertEqual(result["state"], "enrolling")
        self.assertEqual(result["supportedToolchainGenerations"], [A])
        self.assertEqual(
            result["roleProfiles"],
            {"cmux_macos_native_build": dict(f.ROLE_PROFILES["cmux_macos_native_build"])},
        )

    def test_renewal_refreshes_capabilities_and_requires_new_acceptance(self):
        for family in ("macos", "linux"):
            with self.subTest(family=family):
                original = enrollment(family)
                old_receipt = finalized(original)
                current = f.transition(original, "quarantined", "toolchain_mismatch")
                bootstrap = bootstrap_for(current, D)
                bootstrap["glaedaGeneration"] = E
                bootstrap["osVersionClass"] = "updated-os"
                before = copy.deepcopy(current)
                plan = f.renewal_plan(current, bootstrap)
                self.assertEqual(current, before)
                renewed = plan["replacement"]
                self.assertEqual(renewed["nodeId"], current["nodeId"])
                self.assertEqual(renewed["operatorFleetScope"], current["operatorFleetScope"])
                self.assertEqual(renewed["enrollmentGeneration"], current["enrollmentGeneration"] + 1)
                self.assertEqual(renewed["supportedToolchainGenerations"], [D])
                self.assertEqual(renewed["glaedaGeneration"], E)
                self.assertEqual(renewed["os"]["versionClass"], "updated-os")
                self.assertEqual(renewed["state"], "enrolling")
                self.assertIsNone(renewed["quarantineReason"])
                with self.assertRaisesRegex(f.FleetError, "current accepted role"):
                    f.transition(renewed, "eligible", None, [old_receipt])
                fresh_receipt = finalized(renewed, toolchain=D)
                eligible = f.transition(renewed, "eligible", None, [fresh_receipt])
                self.assertTrue(f.node_status(eligible, [fresh_receipt])["routingCandidateEligible"])
                self.assertFalse(f.node_status(eligible, [fresh_receipt])["automaticDispatchAuthorized"])

    def test_renewal_refuses_live_terminal_blocked_and_exhausted_inputs(self):
        for state in f.STATES:
            if state == "quarantined":
                continue
            with self.subTest(state=state):
                current = enrollment(state=state)
                with self.assertRaisesRegex(f.FleetError, "quarantined"):
                    f.renewal_plan(current, bootstrap_for(current))
        current = f.transition(enrollment(), "quarantined", "service_mismatch")
        bootstrap = bootstrap_for(current)
        bootstrap["eligibleForEnrollment"] = False
        with self.assertRaisesRegex(f.FleetError, "blocking checks"):
            f.renewal_plan(current, bootstrap)
        current["enrollmentGeneration"] = 2**31 - 1
        with self.assertRaisesRegex(f.FleetError, "exhausted"):
            f.renewal_plan(current, bootstrap_for(current))

    def test_apply_renewal_binds_plan_and_publishes_once(self):
        for changed in (None, "enrollment", "bootstrap", "digest"):
            with self.subTest(changed=changed), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary).resolve()
                root.chmod(0o700)
                path = root / "enrollment.json"
                bootstrap_path = root / "bootstrap.json"
                current = f.transition(enrollment(), "quarantined", "toolchain_mismatch")
                bootstrap = bootstrap_for(current, D)
                plan = f.renewal_plan(current, bootstrap)
                if changed == "enrollment":
                    current["quarantineReason"] = "hardware_failure"
                if changed == "bootstrap":
                    bootstrap["toolchainGeneration"] = E
                for target, value in ((path, current), (bootstrap_path, bootstrap)):
                    target.write_bytes(f.canonical(value))
                    target.chmod(0o600)
                before = path.read_bytes()
                self.assertEqual(f.renewal_plan(current, bootstrap)["replacement"]["state"], "enrolling")
                self.assertFalse((root / ".mutation.lock").exists())
                if changed:
                    with self.assertRaisesRegex(f.FleetError, "plan changed"):
                        f.apply_renewal(path, bootstrap_path, A if changed == "digest" else plan["planSha256"])
                    self.assertEqual(path.read_bytes(), before)
                else:
                    result = f.apply_renewal(path, bootstrap_path, plan["planSha256"])
                    self.assertEqual(result, plan["replacement"])
                    self.assertEqual(f.load(path), result)
                    self.assertEqual(path.stat().st_mode & 0o777, 0o600)
                    with self.assertRaisesRegex(f.FleetError, "quarantined"):
                        f.apply_renewal(path, bootstrap_path, plan["planSha256"])
                    self.assertEqual(f.load(path), result)
                self.assertFalse(list(root.glob(".enrollment.next.*")))

    def test_blocked_bootstrap_cannot_enroll(self):
        bootstrap = {
            "schema": f.BOOTSTRAP_SCHEMA,
            "authority": "observation_only",
            "eligibleForEnrollment": False,
        }
        with self.assertRaisesRegex(f.FleetError, "blocking checks"):
            f.enrollment_from_bootstrap(
                bootstrap,
                node_id="cmux-fixture-002",
                operator_fleet_scope="cmux-founders",
                enrollment_generation=1,
            )

    def test_versioned_schema_and_examples_parse(self):
        schema = json.loads(
            (ROOT / "examples/cmux-fleet/enrollment-v1.schema.json").read_text(
                encoding="utf-8"
            )
        )
        self.assertFalse(schema["additionalProperties"])
        for name in (
            "macos-enrollment.example.json",
            "linux-enrollment.example.json",
        ):
            document = json.loads(
                (ROOT / "examples/cmux-fleet" / name).read_text(
                    encoding="utf-8"
                )
            )
            self.assertEqual(f.validate_enrollment(document), document)

    def test_load_requires_private_owned_regular_file(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            document = root / "enrollment.json"
            document.write_bytes(f.canonical(enrollment()))
            document.chmod(0o600)
            self.assertEqual(f.load(document), enrollment())

            document.chmod(0o644)
            with self.assertRaisesRegex(f.FleetError, "ownership or mode"):
                f.load(document)
            document.chmod(0o600)

            alias = root / "alias.json"
            alias.symlink_to(document)
            with self.assertRaisesRegex(f.FleetError, "unavailable"):
                f.load(alias)

    def test_load_rejects_file_identity_change_during_read(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "enrollment.json"
            path.write_bytes(f.canonical(enrollment()))
            path.chmod(0o600)
            original_fstat = os.fstat
            calls = 0

            def changed_fstat(fd):
                nonlocal calls
                calls += 1
                value = original_fstat(fd)
                if calls == 2:
                    values = list(value)
                    values[8] += 1
                    return os.stat_result(values)
                return value

            with mock.patch.object(f.os, "fstat", side_effect=changed_fstat):
                with self.assertRaisesRegex(f.FleetError, "changed while reading"):
                    f.load(path)

    def test_transition_apply_publishes_private_durable_state(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            root.chmod(0o700)
            enrollment_path = root / "enrollment.json"
            enrollment_path.write_bytes(f.canonical(enrollment()))
            enrollment_path.chmod(0o600)

            updated = f.apply_transition(
                enrollment_path,
                "draining",
                None,
                [],
            )

            self.assertEqual(updated["state"], "draining")
            self.assertEqual(f.load(enrollment_path), updated)
            self.assertEqual(enrollment_path.stat().st_mode & 0o777, 0o600)
            lock_path = root / ".mutation.lock"
            self.assertTrue(lock_path.is_file())
            self.assertEqual(lock_path.stat().st_mode & 0o777, 0o600)
            self.assertFalse(list(root.glob(".enrollment.next.*")))

    def test_transition_apply_refuses_concurrent_mutation(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            root.chmod(0o700)
            enrollment_path = root / "enrollment.json"
            enrollment_path.write_bytes(f.canonical(enrollment()))
            enrollment_path.chmod(0o600)

            with f.FleetMutationLock(enrollment_path):
                with self.assertRaisesRegex(f.FleetError, "mutation is busy"):
                    f.apply_transition(
                        enrollment_path,
                        "draining",
                        None,
                        [],
                    )

            self.assertEqual(f.load(enrollment_path)["state"], "eligible")

    def test_transition_refuses_replaced_or_changed_lock_before_publication(self):
        for change in ("replace", "mode", "unlink", "symlink"):
            with self.subTest(change=change), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary).resolve()
                root.chmod(0o700)
                path = root / "enrollment.json"
                original = f.canonical(enrollment())
                path.write_bytes(original)
                path.chmod(0o600)
                transition = f.transition

                def changed_lock(*args):
                    result = transition(*args)
                    lock = root / ".mutation.lock"
                    if change == "mode":
                        lock.chmod(0o644)
                    else:
                        lock.rename(root / "old-lock")
                        if change == "replace":
                            lock.touch(mode=0o600)
                        elif change == "symlink":
                            lock.symlink_to(root / "old-lock")
                    return result

                with mock.patch.object(f, "transition", side_effect=changed_lock):
                    with self.assertRaisesRegex(f.FleetError, "lock"):
                        f.apply_transition(path, "draining", None, [])
                self.assertEqual(path.read_bytes(), original)
                self.assertFalse(list(root.glob(".enrollment.next.*")))

    def test_transition_apply_refuses_parent_directory_rebind(self):
        with tempfile.TemporaryDirectory() as temporary:
            outer = Path(temporary).resolve()
            state = outer / "state"
            state.mkdir(mode=0o700)
            enrollment_path = state / "enrollment.json"
            enrollment_path.write_bytes(f.canonical(enrollment()))
            enrollment_path.chmod(0o600)
            detached = outer / "detached"

            original_transition = f.transition

            def rebind(current, target, reason, acceptances):
                result = original_transition(current, target, reason, acceptances)
                state.rename(detached)
                state.mkdir(mode=0o700)
                decoy = state / "enrollment.json"
                decoy.write_bytes(f.canonical(enrollment()))
                decoy.chmod(0o600)
                return result

            with mock.patch.object(f, "transition", side_effect=rebind):
                with self.assertRaisesRegex(
                    f.FleetError,
                    "directory identity changed",
                ):
                    f.apply_transition(
                        enrollment_path,
                        "draining",
                        None,
                        [],
                    )

            self.assertEqual(
                f.load(detached / "enrollment.json")["state"],
                "eligible",
            )
            self.assertEqual(
                f.load(state / "enrollment.json")["state"],
                "eligible",
            )

    def test_cmux_semantic_result_loader_requires_canonical_exact_bytes(self):
        result = cmux_result()
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            path = root / "cmux-result.json"
            raw = f.canonical(result)
            path.write_bytes(raw)
            path.chmod(0o600)
            loaded, exact_digest = f.load_cmux_semantic_result(path)
            self.assertEqual(loaded, result)
            self.assertEqual(
                exact_digest,
                "sha256:" + __import__("hashlib").sha256(raw).hexdigest(),
            )
            e = enrollment()
            receipt = f.finalize_acceptance(
                e,
                "cmux_macos_native_build",
                A,
                loaded,
                bootstrap_for(e),
                exact_digest,
                execution_class=f.LOCAL_EXECUTION_CLASS,
                local_execution_attempt_sha256=E,
            )
            self.assertEqual(receipt["cmuxSemanticResultSha256"], exact_digest)

            path.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
            with self.assertRaisesRegex(f.FleetError, "not canonical"):
                f.load_cmux_semantic_result(path)

    def test_cmux_semantic_result_loader_requires_private_mode(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "result.json"
            path.write_bytes(f.canonical(cmux_result()))
            path.chmod(0o644)
            with self.assertRaisesRegex(f.FleetError, "unsafe"):
                f.load_cmux_semantic_result(path)

    def test_cmux_semantic_result_loader_refuses_symlink(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            target = root / "result.json"
            target.write_bytes(f.canonical(cmux_result()))
            target.chmod(0o600)
            alias = root / "alias.json"
            alias.symlink_to(target)
            with self.assertRaisesRegex(f.FleetError, "unavailable"):
                f.load_cmux_semantic_result(alias)

    def test_fingerprint_is_canonical(self):
        e = enrollment()
        self.assertEqual(f.digest(copy.deepcopy(e)), f.digest(e))
        self.assertLess(len(f.canonical(f.node_status(e, []))), f.MAX_STATUS_BYTES)


TOOLCHAIN_OBS = {
    "cmuxXcodePin": "26",
    "xcodeVersion": "26.6",
    "xcodeBuild": "17F113",
    "macosSdkVersion": "26.5",
    "zigVersion": "0.16.0",
    "rustcVersion": "rustc 1.97.1 (8bab26f4f 2026-07-14)",
}
HARDWARE = {"model": "Mac16,11", "chip": "Apple M4 Pro", "memoryGiB": 48}
CANDIDATE = {"repository": "teamleaderleo/glaeda", "commit": "5" * 40, "tree": "6" * 40}
TG = "sha256:" + __import__("hashlib").sha256(
    (json.dumps(TOOLCHAIN_OBS, sort_keys=True, separators=(",", ":")) + "\n").encode()
).hexdigest()


def class_bootstrap(enrollment_value, *, toolchain=None, hardware=None):
    toolchain = dict(toolchain or TOOLCHAIN_OBS)
    value = bootstrap_for(enrollment_value, f.toolchain_generation_of(toolchain))
    value["observed"] = {"hardware": dict(hardware or HARDWARE), "toolchain": toolchain}
    return value


def class_enrollment(node="cmux-fixture-001", state="eligible", glaeda=C):
    e = enrollment(state=state)
    e["nodeId"] = node
    e["supportedToolchainGenerations"] = [TG]
    e["glaedaGeneration"] = glaeda
    return e


def std_class_receipt():
    accepting = class_enrollment()
    post = class_bootstrap(accepting)
    local = finalized(accepting, toolchain=TG, post_bootstrap=post)
    return f.build_class_acceptance(accepting, local, post, "std", CANDIDATE), local


class ClassAcceptanceTests(unittest.TestCase):
    def adopt(self, klass, node=None, *, bootstrap=None, expected=None, fleet_class="std",
              contract=None, candidate=None):
        node = node or class_enrollment("cmux-fixture-002", state="enrolling")
        return f.adopt_class_receipt(
            node,
            klass,
            expected or klass["receiptSha256"],
            fleet_class,
            bootstrap or class_bootstrap(node),
            contract or klass["glaedaFleetContractGeneration"],
            candidate or CANDIDATE,
        )

    def test_one_local_acceptance_makes_a_matching_node_eligible(self):
        klass, local = std_class_receipt()
        self.assertEqual(klass["fleetClass"], "std")
        self.assertEqual(klass["hardware"], HARDWARE)
        self.assertEqual(klass["toolchain"]["xcodeBuild"], "17F113")
        self.assertEqual(klass["glaedaCandidate"], CANDIDATE)
        self.assertEqual(klass["acceptingNodeId"], "cmux-fixture-001")
        self.assertEqual(klass["acceptingReceiptSha256"], f.digest(local))
        self.assertEqual(f.validate_class_acceptance(copy.deepcopy(klass)), klass)

        node = class_enrollment("cmux-fixture-002", state="enrolling")
        receipt = self.adopt(klass, node)
        self.assertEqual(receipt["nodeId"], "cmux-fixture-002")
        self.assertEqual(receipt["executionClass"], f.CLASS_EXECUTION_CLASS)
        self.assertEqual(receipt["classAcceptanceSha256"], klass["receiptSha256"])
        self.assertIsNone(receipt["localExecutionAttemptSha256"])
        self.assertEqual(receipt["result"], "accepted")

        eligible = f.transition(node, "eligible", None, [receipt])
        self.assertEqual(eligible["state"], "eligible")
        self.assertEqual(eligible["classAcceptanceSha256"], klass["receiptSha256"])
        self.assertTrue(f.node_status(eligible, [receipt])["routingCandidateEligible"])

    def test_every_mismatching_field_is_named_and_refused(self):
        klass, _ = std_class_receipt()
        light = dict(HARDWARE, chip="Apple M4", memoryGiB=16, model="Mac16,10")
        other_xcode = dict(TOOLCHAIN_OBS, xcodeBuild="17F200")
        cases = {
            "hardware.chip": {"hardware": light},
            "hardware.memoryGiB": {"hardware": dict(HARDWARE, memoryGiB=64)},
            "toolchain.xcodeBuild": {"toolchain": other_xcode},
        }
        for field, change in cases.items():
            with self.subTest(field=field):
                node = class_enrollment("cmux-fixture-002", state="enrolling")
                bootstrap = class_bootstrap(node, **change)
                node["supportedToolchainGenerations"] = [bootstrap["toolchainGeneration"]]
                with self.assertRaisesRegex(f.FleetError, "run accept-local") as caught:
                    self.adopt(klass, node, bootstrap=bootstrap)
                self.assertIn(field, str(caught.exception))
        node = class_enrollment("cmux-fixture-002", state="enrolling", glaeda=D)
        with self.assertRaisesRegex(f.FleetError, "glaedaGeneration"):
            self.adopt(klass, node)
        with self.assertRaisesRegex(f.FleetError, "glaedaFleetContractGeneration"):
            self.adopt(klass, contract=B)
        with self.assertRaisesRegex(f.FleetError, "glaedaCandidate"):
            self.adopt(klass, candidate=dict(CANDIDATE, commit="7" * 40))
        with self.assertRaisesRegex(f.FleetError, "this node is class light"):
            self.adopt(klass, fleet_class="light")

    def test_a_node_must_pass_its_own_bootstrap(self):
        klass, _ = std_class_receipt()
        node = class_enrollment("cmux-fixture-002", state="enrolling")
        blocked = class_bootstrap(node)
        blocked["eligibleForEnrollment"] = False
        blocked["blockingChecks"] = ["diskAdmission"]
        with self.assertRaisesRegex(f.FleetError, "blocking checks"):
            self.adopt(klass, node, bootstrap=blocked)
        with self.assertRaisesRegex(f.FleetError, "enrolling node"):
            self.adopt(klass, class_enrollment("cmux-fixture-002", state="eligible"))

    def test_class_receipt_integrity(self):
        klass, _ = std_class_receipt()
        edited = copy.deepcopy(klass)
        edited["hardware"]["memoryGiB"] = 16
        with self.assertRaisesRegex(f.FleetError, "digest does not match"):
            f.validate_class_acceptance(edited)
        # Re-sealed by whoever edited it: the digest is now self-consistent, so the
        # operator's expected digest is what refuses it.
        body = {k: v for k, v in edited.items() if k != "receiptSha256"}
        resealed = {**body, "receiptSha256": f.digest(body)}
        with self.assertRaisesRegex(f.FleetError, "operator expected"):
            self.adopt(resealed, expected=klass["receiptSha256"])
        toolchain_lie = copy.deepcopy(klass)
        toolchain_lie["toolchain"]["zigVersion"] = "0.15.0"
        body = {k: v for k, v in toolchain_lie.items() if k != "receiptSha256"}
        with self.assertRaisesRegex(f.FleetError, "disagrees with its observations"):
            f.validate_class_acceptance({**body, "receiptSha256": f.digest(body)})
        extra = dict(klass, nodeSerial="C02XXXX")
        with self.assertRaisesRegex(f.FleetError, "unknown or missing"):
            f.validate_class_acceptance(extra)

    def test_only_a_current_local_acceptance_seeds_a_class(self):
        klass, _ = std_class_receipt()
        node = class_enrollment("cmux-fixture-002", state="enrolling")
        derived = self.adopt(klass, node)
        eligible = f.transition(node, "eligible", None, [derived])
        with self.assertRaisesRegex(f.FleetError, "does not chain"):
            f.build_class_acceptance(eligible, derived, class_bootstrap(eligible), "std", CANDIDATE)
        accepting = class_enrollment(state="enrolling")
        local = finalized(accepting, toolchain=TG, post_bootstrap=class_bootstrap(accepting))
        with self.assertRaisesRegex(f.FleetError, "eligible node"):
            f.build_class_acceptance(accepting, local, class_bootstrap(accepting), "std", CANDIDATE)
        stale = class_enrollment()
        stale["enrollmentGeneration"] += 1
        with self.assertRaisesRegex(f.FleetError, "acceptance_enrollment_stale"):
            f.build_class_acceptance(stale, local, class_bootstrap(stale), "std", CANDIDATE)
        bare = class_enrollment()
        with self.assertRaisesRegex(f.FleetError, "no hardware or toolchain identity"):
            f.build_class_acceptance(bare, local, bootstrap_for(bare, TG), "std", CANDIDATE)

    def test_class_derived_receipt_shape_is_closed(self):
        klass, local = std_class_receipt()
        derived = self.adopt(klass)
        missing = {k: v for k, v in derived.items() if k != "classAcceptanceSha256"}
        with self.assertRaisesRegex(f.FleetError, "unknown or missing"):
            f.validate_acceptance_receipt(missing)
        with self.assertRaisesRegex(f.FleetError, "unknown or missing"):
            f.validate_acceptance_receipt(dict(local, classAcceptanceSha256=A))
        with self.assertRaisesRegex(f.FleetError, "class acceptance digest"):
            f.validate_acceptance_receipt(dict(derived, classAcceptanceSha256=None))

    def test_enrollment_records_and_forgets_the_class_it_relied_on(self):
        klass, _ = std_class_receipt()
        node = class_enrollment("cmux-fixture-002", state="enrolling")
        derived = self.adopt(klass, node)
        eligible = f.transition(node, "eligible", None, [derived])
        other = dict(eligible, classAcceptanceSha256=A)
        by_role = {r["role"]: r for r in f.node_status(other, [derived])["roles"]}
        self.assertEqual(by_role["cmux_macos_native_build"]["reason"], "acceptance_class_stale")
        quarantined = f.transition(eligible, "quarantined", "toolchain_mismatch")
        self.assertEqual(quarantined["classAcceptanceSha256"], klass["receiptSha256"])
        renewed = f.transition(quarantined, "enrolling", None)
        self.assertNotIn("classAcceptanceSha256", renewed)
        # A node accepted locally keeps the legacy shape.
        local_node = class_enrollment(state="enrolling")
        local = finalized(local_node, toolchain=TG, post_bootstrap=class_bootstrap(local_node))
        self.assertEqual(
            set(f.transition(local_node, "eligible", None, [local])),
            f.ENROLLMENT_KEYS,
        )

    def test_a_class_receipt_counts_only_while_the_enrollment_names_it(self):
        klass, _ = std_class_receipt()
        node = class_enrollment("cmux-fixture-002", state="enrolling")
        derived = self.adopt(klass, node)
        eligible = f.transition(node, "eligible", None, [derived])
        # Swapped onto a node that became eligible some other way, it does not count.
        legacy = {k: v for k, v in eligible.items() if k != "classAcceptanceSha256"}
        by_role = {r["role"]: r for r in f.node_status(legacy, [derived])["roles"]}
        self.assertEqual(by_role["cmux_macos_native_build"]["reason"], "acceptance_class_stale")
        # Draining and back re-records the same class receipt.
        draining = f.transition(eligible, "draining", None)
        back = f.transition(draining, "eligible", None, [derived])
        self.assertEqual(back["classAcceptanceSha256"], klass["receiptSha256"])
        # A class receipt offered next to a stale one records nothing it did not rest on.
        stale = dict(derived, enrollmentGeneration=derived["enrollmentGeneration"] + 1)
        with self.assertRaisesRegex(f.FleetError, "current accepted role receipt"):
            f.transition(node, "eligible", None, [stale])
        with self.assertRaisesRegex(f.FleetError, "no local execution attempt"):
            f.validate_acceptance_receipt(dict(derived, localExecutionAttemptSha256=E))

    def test_candidate_identity_believes_only_the_running_bytes(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "scripts").mkdir()
            for name in ("cmux_fleet.py", "cmux_fleet_bootstrap.py"):
                (root / "scripts" / name).write_bytes((MODULE_PATH.parent / name).read_bytes())
            files = {
                f"scripts/{name}": {"sha256": f._file_sha256(root / "scripts" / name)[7:], "size": 1}
                for name in ("cmux_fleet.py", "cmux_fleet_bootstrap.py")
            }
            files["bin/glaeda"] = {"sha256": C[7:], "size": 1}
            manifest = {"schema": f.CANDIDATE_MANIFEST_SCHEMA, "files": files, "source": CANDIDATE}
            (root / "manifest.json").write_text(json.dumps(manifest))
            self.assertEqual(f.candidate_identity(C, root), CANDIDATE)
            with self.assertRaisesRegex(f.FleetError, "running bin/glaeda"):
                f.candidate_identity(D, root)
            (root / "scripts/cmux_fleet.py").write_text("# edited after staging\n")
            with self.assertRaisesRegex(f.FleetError, "running scripts/cmux_fleet.py"):
                f.candidate_identity(C, root)
            (root / "manifest.json").unlink()
            with self.assertRaisesRegex(f.FleetError, "staged Glaeda candidate"):
                f.candidate_identity(C, root)

    def test_export_and_adopt_run_only_a_read_only_bootstrap(self):
        klass, local = std_class_receipt()
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            root.chmod(0o700)

            def private(name, value):
                path = root / name
                path.write_bytes(f.canonical(value))
                path.chmod(0o600)
                return path

            accepting = class_enrollment()
            node = class_enrollment("cmux-fixture-002", state="enrolling")
            glaeda = root / "glaeda"
            glaeda.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            glaeda.chmod(0o755)
            (root / "cmux").mkdir()
            (root / "cache").mkdir()
            calls = []

            def fake_run(argv, **kwargs):
                calls.append((argv, kwargs["env"]))
                self.assertIn("cmux_fleet_bootstrap.py", " ".join(map(str, argv)))
                self.assertEqual(argv[argv.index("--cache-root") + 1], str(root / "cache"))
                return __import__("subprocess").CompletedProcess(
                    argv, 0, stdout=f.canonical(class_bootstrap(current)), stderr=b"",
                )

            common = dict(cmux_root=root / "cmux", glaeda=glaeda, cache_root=root / "cache")
            with (
                mock.patch.dict(f.os.environ, {"PATH": "/usr/bin:/bin", "SECRET": "x"}, clear=True),
                mock.patch.object(f.subprocess, "run", side_effect=fake_run),
                mock.patch.object(f, "candidate_identity", return_value=dict(CANDIDATE)),
                mock.patch.object(f, "fleet_contract_generation", return_value=local["glaedaFleetContractGeneration"]),
            ):
                current = accepting
                exported = f.export_class_acceptance(
                    private("enrollment.json", accepting), private("acceptance.json", local), "std", **common,
                )
                current = node
                adopted = f.adopt_class_acceptance(
                    private("node.json", node), private("std.json", exported), exported["receiptSha256"],
                    "std", **common,
                )
        self.assertEqual(exported, klass)
        self.assertEqual(adopted["classAcceptanceSha256"], klass["receiptSha256"])
        self.assertEqual(len(calls), 2)
        for _argv, env in calls:
            self.assertNotIn("SECRET", env)
            self.assertNotIn("TMPDIR", env)
            self.assertEqual(env["LC_ALL"], "C")


if __name__ == "__main__":
    unittest.main()
