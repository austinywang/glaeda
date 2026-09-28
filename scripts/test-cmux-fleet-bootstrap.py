#!/usr/bin/env python3
import importlib.util
from pathlib import Path
import json
import platform
import os
import subprocess
import tempfile
import unittest
from unittest import mock

MODULE_PATH = Path(__file__).with_name("cmux_fleet_bootstrap.py")
SPEC = importlib.util.spec_from_file_location("cmux_fleet_bootstrap", MODULE_PATH)
assert SPEC and SPEC.loader
b = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(b)

A = "sha256:" + "a" * 64
B = "sha256:" + "b" * 64


def observation(platform="macos", failed=()):
    checks = {
        "supportedOs": True,
        "hardwareCapability": True,
        "git": True,
        "glaedaExecutable": True,
        "diskAdmission": True,
        "profileRunnerInterpreter": True,
        "workloadToolPath": True,
    }
    if platform == "macos":
        checks.update(
            {"cmuxCheckout": True, "xcodePin": True, "unattendedPower": True}
        )
    else:
        checks.update(
            {
                "cmuxCheckout": True,
                "systemd": True,
                "bubblewrap": True,
                "cgroupV2": True,
                "pressureSignals": True,
                "memoryAdmission": True,
                "actionsPrerequisites": True,
            }
        )
    for key in failed:
        checks[key] = False
    return {
        "platform": platform,
        "architecture": "arm64" if platform == "macos" else "x86_64",
        "osVersionClass": "macos-26" if platform == "macos" else "ubuntu-24.04",
        "glaedaGeneration": A,
        "toolchainGeneration": B,
        "roleProfiles": {
            "cmux_macos_native_build" if platform == "macos" else "cmux_linux_ci":
                dict(
                    b.ROLE_PROFILES[
                        "cmux_macos_native_build" if platform == "macos" else "cmux_linux_ci"
                    ]
                )
        },
        "checks": checks,
        "observed": {},
    }


class Tests(unittest.TestCase):
    def test_mac_ready(self):
        result = b.evaluate(
            observation(),
            ["cmux_macos_native_build"],
            "cmux-mac-build-large",
        )
        self.assertTrue(result["eligibleForEnrollment"])
        self.assertEqual(result["authority"], "observation_only")
        self.assertEqual(
            result["roleProfiles"],
            {"cmux_macos_native_build": dict(b.ROLE_PROFILES["cmux_macos_native_build"])},
        )

    def test_linux_ready(self):
        result = b.evaluate(
            observation("linux"),
            ["cmux_linux_ci"],
            "cmux-linux-ci-medium",
        )
        self.assertTrue(result["eligibleForEnrollment"])

    def test_failures_block_enrollment(self):
        result = b.evaluate(
            observation(failed=("xcodePin", "diskAdmission")),
            ["cmux_macos_native_build"],
            "cmux-mac-build-large",
        )
        self.assertFalse(result["eligibleForEnrollment"])
        self.assertEqual(result["blockingChecks"], ["diskAdmission", "xcodePin"])

    def test_wrong_os_role_refused(self):
        with self.assertRaisesRegex(b.BootstrapError, "incompatible"):
            b.evaluate(
                observation("linux"),
                ["cmux_macos_native_build"],
                "cmux-linux-ci-medium",
            )

    def test_reviewed_hardware_classes_have_minimums(self):
        self.assertTrue(
            b.hardware_class_ready(
                "macos",
                "cmux-mac-build-large",
                8,
                16,
            )
        )
        self.assertFalse(
            b.hardware_class_ready(
                "macos",
                "cmux-mac-build-large",
                4,
                16,
            )
        )
        self.assertTrue(
            b.hardware_class_ready(
                "linux",
                "cmux-linux-ci-medium",
                4,
                8,
            )
        )
        self.assertFalse(
            b.hardware_class_ready(
                "linux",
                "unknown",
                64,
                256,
            )
        )

    def test_role_without_workload_refused(self):
        with self.assertRaisesRegex(b.BootstrapError, "reviewed v1 acceptance workload"):
            b.evaluate(
                observation(),
                ["artifact_cache"],
                "cmux-mac-build-large",
            )

    def test_zig_version_compatibility_matches_cmux_policy(self):
        self.assertTrue(b.zig_version_compatible("0.16.0", "0.16.0"))
        self.assertTrue(b.zig_version_compatible("0.16.4", "0.16.0"))
        self.assertFalse(b.zig_version_compatible("0.15.9", "0.16.0"))
        self.assertFalse(b.zig_version_compatible("0.17.0", "0.16.0"))
        self.assertFalse(b.zig_version_compatible("nightly", "0.16.0"))

    def test_role_profiles_must_cover_exact_roles(self):
        observed = observation()
        observed["roleProfiles"] = {}
        with self.assertRaisesRegex(b.BootstrapError, "role profiles"):
            b.evaluate(
                observed,
                ["cmux_macos_native_build"],
                "cmux-mac-build-large",
            )

    def test_profile_registry_binds_role_generation_and_architecture(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            registry = root / b.CMUX_PROFILE_REGISTRY
            registry.parent.mkdir(parents=True)
            registry.write_text(
                json.dumps(
                    {
                        "schema_version": 1,
                        "repository": b.CMUX_REPOSITORY,
                        "result_contract": b.CMUX_RESULT_CONTRACT,
                        "profiles": [
                            {
                                "id": "cmux.macos.dev-check",
                                "generation": 1,
                                "platform": {
                                    "os": "macos",
                                    "architectures": ["arm64"],
                                },
                            }
                        ],
                    }
                ),
                encoding="utf-8",
            )
            self.assertEqual(
                b.role_profiles(
                    root,
                    ["cmux_macos_native_build"],
                    "macos",
                    "arm64",
                ),
                {
                    "cmux_macos_native_build": dict(
                        b.ROLE_PROFILES["cmux_macos_native_build"]
                    )
                },
            )
            with self.assertRaisesRegex(b.BootstrapError, "incompatible"):
                b.role_profiles(
                    root,
                    ["cmux_macos_native_build"],
                    "macos",
                    "x86_64",
                )

    def test_command_output_keeps_column_zero(self):
        # `git submodule status` marks a checked-out submodule with a leading
        # space, so trimming it made every clean checkout look unready.
        output = b.run(["/bin/sh", "-c", "printf ' clean sub\\n-absent sub\\n'"])
        self.assertEqual(output.splitlines(), [" clean sub", "-absent sub"])

    def test_submodules_ready_on_a_real_checked_out_submodule(self):
        # Mocking `run` here would step over the bug: the leading space that
        # marks a checked-out submodule only survives if `run` leaves it.
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            inner, outer = root / "inner", root / "outer"

            def git(*args, cwd):
                subprocess.run(
                    ["git", "-c", "protocol.file.allow=always",
                     "-c", "user.name=t", "-c", "user.email=t@example.invalid",
                     *args],
                    cwd=cwd, check=True,
                    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                    env={"PATH": os.environ.get("PATH", "/usr/bin:/bin"),
                         "HOME": str(root), "GIT_CONFIG_GLOBAL": "/dev/null",
                         "GIT_CONFIG_NOSYSTEM": "1"},
                )

            for path in (inner, outer):
                path.mkdir()
                git("init", "-q", "-b", "main", cwd=path)
                (path / "file").write_text("x", encoding="utf-8")
                git("add", "-A", cwd=path)
                git("commit", "-qm", "seed", cwd=path)
            git("submodule", "add", "-q", str(inner), "vendor", cwd=outer)
            git("commit", "-qm", "vendor", cwd=outer)

            self.assertTrue(b.cmux_submodules_ready(outer))
            self.assertTrue(b.cmux_checkout_clean(outer))

    def test_setup_artifacts_accept_either_ghosttykit_location(self):
        for location in b.CMUX_GHOSTTYKIT_LOCATIONS:
            with self.subTest(location=location), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                header = root / "ghostty/include/ghostty.h"
                header.parent.mkdir(parents=True)
                header.write_text("/* fixture */\n", encoding="utf-8")
                self.assertFalse(b.cmux_setup_artifacts_present(root))
                (root / location).mkdir(parents=True)
                self.assertTrue(b.cmux_setup_artifacts_present(root))

    def test_tools_only_on_the_operator_path_are_reported_invisible(self):
        # The build rebuilds PATH from a fixed list of system directories plus
        # an empty Cargo home, so a tool under the operator's home is one the
        # build cannot spend, however well the operator's shell resolves it.
        with tempfile.TemporaryDirectory() as temporary:
            operator_only = Path(temporary)
            tool = operator_only / "cmux-fixture-tool"
            tool.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            tool.chmod(0o755)
            with mock.patch.dict(os.environ, {"PATH": str(operator_only)}):
                # Still resolvable for probing: a node with a misplaced tool
                # must still produce a receipt naming everything else wrong.
                self.assertTrue(b.executable("cmux-fixture-tool").startswith("/"))
                self.assertEqual(
                    b.missing_workload_tools(("cmux-fixture-tool", "sh")),
                    ["cmux-fixture-tool"],
                )
            self.assertEqual(b.missing_workload_tools(("sh", "cat")), [])

    def _macos_checkout(self, root):
        """A checkout shaped enough for collect_macos to read it."""
        (root / ".xcode-version").write_text("26.0\n", encoding="utf-8")
        ghostty = root / "ghostty"
        (ghostty / "include").mkdir(parents=True)
        (ghostty / "include/ghostty.h").write_text("/* fixture */\n", encoding="utf-8")
        (root / "GhosttyKit.xcframework").mkdir()
        (ghostty / "build.zig.zon").write_text(
            '.{\n    .minimum_zig_version = "0.16.0",\n}\n', encoding="utf-8"
        )
        sidecar = root / "Native/DiffSidecar"
        sidecar.mkdir(parents=True)
        (sidecar / "rust-toolchain.toml").write_text(
            '[toolchain]\nchannel = "1.90.0"\n', encoding="utf-8"
        )

    @staticmethod
    def _macos_command(argv, **_kwargs):
        name = Path(argv[0]).name
        rest = argv[1:]
        if name == "git":
            if rest[:1] == ["status"]:
                return ""
            if rest[:1] == ["submodule"]:
                return " a1 ghostty (heads/main)"
            return "git version 2.51.0"
        if name == "xcodebuild":
            return "Xcode 26.0\nBuild version 26A123"
        if name == "xcrun":
            if "metal" in rest:
                return ("metal version 32023\nTarget: air64-apple-darwin25.5.0\n"
                        "InstalledDir: /private/var/run/mnt/MetalToolchain-v17.6.7KPGHG/bin")
            return "26.0"
        if name == "zig":
            return "0.16.0"
        if name == "rustup":
            if rest[:1] == ["run"]:
                return f"{rest[2]} 1.90.0 (abc 2026-01-01)"
            return "rustup 1.28.0"
        if name in ("cargo", "rustc"):
            return f"{name} 1.90.0 (abc 2026-01-01)"
        if name == "pmset":
            return "AC Power:\n sleep 0\nBattery Power:\n sleep 10"
        if name == "sysctl":
            if "hw.model" in rest:
                return "Mac16,11"
            if "machdep.cpu.brand_string" in rest:
                return "Apple M4 Pro"
            return str(64 * 1024**3)
        raise AssertionError(argv)

    def _collect_macos(self, root, cache_root, visible):
        with (
            mock.patch.object(b.platform, "system", return_value="Darwin"),
            mock.patch.object(b.platform, "mac_ver", return_value=("26.5", ("", "", ""), "arm64")),
            mock.patch.object(b.platform, "machine", return_value="arm64"),
            mock.patch.object(b, "executable", side_effect=lambda n: f"/usr/bin/{n}"),
            mock.patch.object(b, "run", side_effect=self._macos_command),
            mock.patch.object(
                b.shutil, "which",
                side_effect=lambda name, path=None: f"/usr/bin/{name}" if visible else None,
            ),
        ):
            return b.collect_macos(root, root / "glaeda", 1, "cmux-mac-build-large", cache_root)

    def test_rust_is_observed_from_the_cmux_checkout(self):
        # rustup picks the toolchain from the working directory, so a probe run
        # elsewhere (a Glaeda checkout pins its own Rust) moves the toolchain
        # generation between enrollment and acceptance.
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            self._macos_checkout(root)
            (root / "glaeda").write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            cache_root = root / "cache"
            cache_root.mkdir()
            seen = {}
            original = self._macos_command

            def command(argv, **kwargs):
                if Path(argv[0]).name in ("rustup", "cargo", "rustc"):
                    seen[" ".join(argv[1:])] = kwargs.get("cwd")
                return original(argv, **kwargs)

            with mock.patch.object(self, "_macos_command", side_effect=command):
                self._collect_macos(root, cache_root, visible=True)
            self.assertTrue(seen)
            self.assertEqual(set(seen.values()), {root}, seen)

    def test_xcode_pin_accepts_the_major_cmux_pins(self):
        # cmux#14050 changed .xcode-version from "26.0" to "26".
        for pin in ("26", "26.0", "26\n".strip()):
            self.assertTrue(b.xcode_pin_ready(pin, "26.6", "26.5"), pin)
        self.assertFalse(b.xcode_pin_ready("27", "27.0", "27.0"))
        self.assertFalse(b.xcode_pin_ready("26", "25.4", "26.5"))
        self.assertFalse(b.xcode_pin_ready("26", "260.1", "26.5"))
        self.assertFalse(b.xcode_pin_ready("26", "26.6", "15.5"))
        self.assertFalse(b.xcode_pin_ready("26", None, "26.5"))
        self.assertFalse(b.xcode_pin_ready("", "26.6", "26.5"))

    def test_macos_observation_accepts_a_major_only_xcode_version_file(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            self._macos_checkout(root)
            (root / ".xcode-version").write_text("26\n", encoding="utf-8")
            glaeda = root / "glaeda"
            glaeda.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            glaeda.chmod(0o755)
            cache_root = root / "cache"
            cache_root.mkdir()
            self.assertTrue(self._collect_macos(root, cache_root, visible=True)["checks"]["xcodePin"])

    def test_macos_observation_reports_class_identity(self):
        # cmux_fleet.py class acceptance binds these; the digest must be the
        # toolchain generation so a node cannot report one and run another.
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            self._macos_checkout(root)
            glaeda = root / "glaeda"
            glaeda.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            glaeda.chmod(0o755)
            cache_root = root / "cache"
            cache_root.mkdir()
            observed = self._collect_macos(root, cache_root, visible=True)
        self.assertEqual(
            observed["observed"]["hardware"],
            {"model": "Mac16,11", "chip": "Apple M4 Pro", "memoryGiB": 64},
        )
        toolchain = observed["observed"]["toolchain"]
        self.assertEqual(toolchain["xcodeBuild"], "26A123")
        self.assertEqual(toolchain["xcodeVersion"], "26.0")
        # The asset mount path differs between identical hosts, so only the version line counts.
        self.assertEqual(toolchain["metalVersion"], "metal version 32023")
        self.assertEqual(
            observed["toolchainGeneration"], b.digest_bytes(b.canonical(toolchain))
        )
        self.assertNotIn("serial", json.dumps(observed).lower())

    def test_macos_observation_reports_workload_tool_visibility(self):
        # collect_macos cannot run on the host that runs this suite, so its
        # wiring is the half most likely to be left behind by a later edit.
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            self._macos_checkout(root)
            glaeda = root / "glaeda"
            glaeda.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            glaeda.chmod(0o755)
            cache_root = root / "cache"
            cache_root.mkdir()

            visible = self._collect_macos(root, cache_root, visible=True)
            self.assertTrue(visible["checks"]["workloadToolPath"])
            self.assertEqual(visible["observed"]["toolsMissingFromWorkloadPath"], [])
            self.assertEqual(
                visible["checks"]["profileRunnerInterpreter"], hasattr(os, "waitid")
            )

            invisible = self._collect_macos(root, cache_root, visible=False)
            self.assertFalse(invisible["checks"]["workloadToolPath"])
            self.assertEqual(
                invisible["observed"]["toolsMissingFromWorkloadPath"],
                sorted(b.MACOS_WORKLOAD_TOOLS),
            )
            invisible["roleProfiles"] = {
                "cmux_macos_native_build": dict(
                    b.ROLE_PROFILES["cmux_macos_native_build"]
                )
            }
            result = b.evaluate(
                invisible, ["cmux_macos_native_build"], "cmux-mac-build-large"
            )
            self.assertFalse(result["eligibleForEnrollment"])
            self.assertIn("workloadToolPath", result["blockingChecks"])

    def test_linux_observation_reports_the_new_checks(self):
        # evaluate() is key-agnostic, so asserting on a hand-built fixture
        # proves nothing about what the collectors actually emit.
        if platform.system() != "Linux":
            self.skipTest("collect_linux observes a Linux host")
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            (root / ".git").mkdir()
            glaeda = root / "glaeda"
            glaeda.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            glaeda.chmod(0o755)
            outputs = {
                "git": "git version 2.51.0",
                "python3": "Python 3.14.0",
                "systemctl": "systemd 257 (257)\n+PAM",
                "bwrap": "bubblewrap 0.11.0",
            }
            with (
                mock.patch.object(b, "executable", side_effect=lambda name: f"/usr/bin/{name}"),
                mock.patch.object(
                    b, "run", side_effect=lambda argv, **kw: outputs[Path(argv[0]).name]
                ),
                mock.patch.object(b, "read_os_release", return_value=("ubuntu", "24.04")),
                mock.patch.object(b, "linux_memory_gib", return_value=64),
            ):
                observed = b.collect_linux(
                    root, glaeda, 1, ["cmux_linux_ci"], "cmux-linux-ci-medium"
                )
        self.assertIn("profileRunnerInterpreter", observed["checks"])
        self.assertIn("workloadToolPath", observed["checks"])
        self.assertEqual(
            observed["checks"]["profileRunnerInterpreter"], hasattr(os, "waitid")
        )
        self.assertTrue(observed["checks"]["workloadToolPath"])
        self.assertEqual(observed["observed"]["toolsMissingFromWorkloadPath"], [])

    def test_free_disk_floor_follows_build_slots(self):
        self.assertEqual(b.default_min_free_gib("macos"), 50)
        self.assertEqual(b.default_min_free_gib("macos", 4), 125)
        self.assertEqual(b.default_min_free_gib("linux", 4), 40)
        with self.assertRaises(ValueError):
            b.default_min_free_gib("macos", 0)
        args = b.parser().parse_args(
            ["--platform", "macos", "--cmux-root", ".", "--glaeda", ".",
             "--hardware-class", "x", "--role", "r"])
        self.assertEqual(args.build_slots, 1)

    def test_power_posture_parser(self):
        raw = (
            "Battery Power:\n"
            " sleep              1\n"
            "AC Power:\n"
            " sleep              0\n"
            " displaysleep       10\n"
        )
        self.assertTrue(b.mac_sleep_disabled_on_ac(raw))
        self.assertFalse(
            b.mac_sleep_disabled_on_ac(
                raw.replace("sleep              0", "sleep              1")
            )
        )


    def test_failed_commands_name_the_missing_thing_and_its_fix(self):
        def fake(output, code=1):
            return subprocess.CompletedProcess([], code, stdout=output)

        cases = (
            (["/usr/bin/xcrun", "metal", "--version"],
             "error: error: cannot execute tool 'metal' due to missing Metal Toolchain; use: xcodebuild -downloadComponent MetalToolchain",
             "xcrun metal --version failed: Metal Toolchain missing; run xcodebuild -downloadComponent MetalToolchain"),
            (["/usr/bin/xcodebuild", "-version"],
             "DVTPlugInLoading: Failed to load code for plug-in; run xcodebuild -runFirstLaunch",
             "xcodebuild -version failed: " + b.FIRST_LAUNCH_FIX),
            (["/opt/homebrew/bin/rustup", "run", "1.88.0"],
             "error: toolchain '1.88.0-aarch64-apple-darwin' is not installed",
             "rustup run 1.88.0 failed: Rust toolchain 1.88.0-aarch64-apple-darwin is not installed; "
             "run rustup toolchain install 1.88.0-aarch64-apple-darwin"),
            (["/usr/bin/git", "status"], "fatal: not a git repository\n", "git status failed: exit 1: fatal: not a git repository"),
        )
        for argv, output, message in cases:
            with self.subTest(argv=argv), mock.patch.object(b.subprocess, "run", return_value=fake(output)):
                with self.assertRaises(b.BootstrapError) as raised:
                    b.run(argv)
                self.assertEqual(str(raised.exception), message)

    def test_missing_tools_and_files_name_their_fix(self):
        with mock.patch.object(b.shutil, "which", return_value=None):
            with self.assertRaisesRegex(b.BootstrapError, "required command is missing: zig; brew install zig as the Homebrew owner"):
                b.executable("zig")
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            with self.assertRaisesRegex(b.BootstrapError, "git submodule update --init"):
                b.cmux_required_zig_version(root)
            with self.assertRaisesRegex(b.BootstrapError, "rust-toolchain.toml is missing"):
                b.cmux_diff_rust_toolchain(root)
        self.assertEqual(b.minimum_zig_version('.{\n    .minimum_zig_version = "0.16.0",\n}\n'), "0.16.0")
        self.assertIsNone(b.minimum_zig_version(".{}"))
        self.assertTrue(b.submodule_status_ready(" a1 ghostty (heads/main)\n a2 vendor/bonsplit"))
        self.assertFalse(b.submodule_status_ready("-a1 ghostty"))
        self.assertFalse(b.submodule_status_ready(""))

    def test_errors_from_older_candidates_are_explained(self):
        self.assertEqual(
            b.explain_error("[Errno 2] No such file or directory: '/Users/cmux/cmux/ghostty/build.zig.zon'"),
            b.SUBMODULE_FIX,
        )
        self.assertIn("xcodebuild -downloadComponent MetalToolchain",
                      b.explain_error("required command failed: xcrun"))
        self.assertIn("brew install rustup", b.explain_error("required command is missing: cargo"))
        self.assertEqual(b.explain_error("something else"), "something else")

    def test_every_check_the_collectors_emit_has_a_fix(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            self._macos_checkout(root)
            (root / "glaeda").write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            cache_root = root / "cache"
            cache_root.mkdir()
            macos = self._collect_macos(root, cache_root, visible=True)
        linux = observation("linux")
        for checks in (macos["checks"], linux["checks"]):
            self.assertEqual(set(checks) - set(b.BLOCKING_FIXES), set())
        self.assertNotIn("\u2014", " ".join(b.BLOCKING_FIXES.values()))


if __name__ == "__main__":
    unittest.main()
