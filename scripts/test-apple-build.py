#!/usr/bin/env python3
"""Native Apple build policy tests; no Xcode, signing, or external services required."""

import importlib.util
import contextlib
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("apple_build", Path(__file__).with_name("apple_build.py"))
apple = importlib.util.module_from_spec(spec)
sys.modules["apple_build"] = apple
spec.loader.exec_module(apple)
import apple_queue as queue


class AppleBuildTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name).resolve()
        subprocess.run(["/usr/bin/git", "init", "--quiet", str(self.root)], check=True)
        subprocess.run(["/usr/bin/git", "-C", str(self.root), "-c", "user.name=Test", "-c", "user.email=test@example.invalid",
                        "commit", "--quiet", "--allow-empty", "-m", "fixture"], check=True)
        self.script = self.root / "build.sh"
        self.script.write_text('#!/bin/sh\nmkdir -p "$BUILD_DIR"\nprintf ok > "$BUILD_DIR/result"\nprintf private-output\n')
        self.script.chmod(0o755)
        self.profile = {"engine": "script", "executable": "build.sh", "environment": {"BUILD_DIR": "{products}"}}
        self.tools = {"developer": "/test/Xcode", "swift": "/test/swift", "xcodebuild": "/test/xcodebuild",
                      "swift_version": "Swift test", "xcode_version": "Xcode test", "sdk": "/test/SDK", "sdk_build": "test1", "host": "arm64"}
        self.write_config()

    def tearDown(self):
        self.temp.cleanup()

    def write_config(self):
        (self.root / "glaeda.apple.json").write_text(json.dumps({"schema_version": 1, "profiles": {"app": self.profile}}))

    def plan(self, generation="default"):
        return apple.prepare(self.root, "app", generation, lambda environment, sdk: self.tools)

    def run_plan(self, plan):
        return apple.execute(plan, lambda *args: self.plan(args[2]))

    def test_missing_helper_reports_configuration_failure_without_private_paths(self):
        self.script.unlink()
        output = io.StringIO()
        prepare = apple.prepare
        with patch.object(sys, "argv", ["apple-build", "plan", "--project", str(self.root)]), \
                patch.object(apple, "prepare", side_effect=lambda *args, **kwargs: prepare(
                    *args, toolchain_probe=lambda environment, sdk: self.tools, **kwargs)), \
                contextlib.redirect_stderr(output):
            self.assertEqual(apple.main(), 2)
        result = json.loads(output.getvalue())
        self.assertEqual(result["reason"], "configured build helper is missing; check the profile executable")
        self.assertNotIn(str(self.root), output.getvalue())
        self.assertFalse((self.root / ".glaeda").exists())

    def test_plan_is_read_only_and_source_edits_preserve_incremental_paths(self):
        plan = self.plan()
        observation = apple.inspect(plan)
        self.assertEqual(observation["state"], "cold")
        self.assertEqual(observation["cache_root"], ".glaeda/apple-build/cache/" + plan["key"])
        self.assertNotIn(str(self.root), json.dumps(observation))
        self.assertFalse((self.root / ".glaeda").exists())
        (self.root / "Feature.swift").write_text("// changed source\n")
        self.assertEqual(plan["key"], self.plan()["key"])
        self.assertNotEqual(plan["key"], self.plan("cold-reset")["key"])

    def native_policy(self):
        path = self.root / "glaeda.apple.json"
        config = json.loads(path.read_text())
        config["cache_policies"] = {"app": "native"}
        path.write_text(json.dumps(config))

    def test_native_lineage_retains_existing_cache_and_executes_changed_recipe(self):
        original = self.plan()
        self.run_plan(original)
        self.native_policy()
        plan = self.plan()
        self.assertEqual(original["key"], plan["key"])
        self.assertFalse(list((self.root / ".glaeda/apple-build").glob("lineage-*")))
        self.run_plan(plan)
        self.script.write_text(self.script.read_text().replace("printf ok", "printf changed"))
        changed = self.plan()
        self.assertEqual(plan["key"], changed["key"])
        self.assertNotEqual(plan["invocation_identity"], changed["invocation_identity"])
        receipt = self.run_plan(changed)
        self.assertEqual(receipt["exit_code"], 0)
        self.assertEqual((Path(changed["paths"]["products"]) / "result").read_text(), "changed")
        self.assertNotEqual(changed["key"], self.plan("cold-reset")["key"])
        self.tools["sdk_build"] = "new-sdk"
        self.assertNotEqual(changed["key"], self.plan()["key"])

    def test_native_lineage_refuses_recipe_race_and_quarantine(self):
        self.native_policy()
        plan = self.plan()
        self.run_plan(plan)
        self.script.write_text(self.script.read_text() + "# new helper\n")
        with self.assertRaisesRegex(apple.Refusal, "changed during admission"):
            self.run_plan(plan)
        fresh = self.plan()
        with apple.store(fresh) as state:
            apple.write_json(state, "quarantine-" + fresh["key"] + ".json", {})
        with self.assertRaisesRegex(apple.Refusal, "interrupted"):
            self.run_plan(fresh)

    def test_native_lineage_rejects_corruption_and_cross_lineage_binding(self):
        self.native_policy()
        plan = self.plan()
        self.run_plan(plan)
        name = "lineage-" + plan["lineage"]["lineage"] + ".json"
        for bad in ({**plan["lineage"], "cache_key": "../foreign"},
                    {**plan["lineage"], "lineage": "0" * 64}):
            with apple.store(plan) as state:
                apple.write_json(state, name, bad)
            with self.assertRaisesRegex(apple.Refusal, "lineage identity"):
                self.plan()

    def test_swift_incremental_options_are_explicit_and_typed(self):
        (self.root / "Package.swift").write_text("// fixture\n")
        self.profile = {"engine": "swiftpm", "incremental_file_hashing": True, "incremental_diagnostics": True}
        self.write_config()
        argv = self.plan()["argv"]
        self.assertIn("-enable-incremental-file-hashing", argv)
        self.assertIn("-driver-show-incremental", argv)
        self.profile["incremental_file_hashing"] = False
        self.write_config()
        self.assertIn("-disable-incremental-file-hashing", self.plan()["argv"])
        self.profile["incremental_file_hashing"] = "yes"
        self.write_config()
        with self.assertRaisesRegex(apple.Refusal, "must be a boolean"):
            self.plan()

    def test_explanation_is_read_only_and_tracks_source_without_skipping_work(self):
        plan = self.plan()
        self.assertEqual(apple.explain(plan)["explanation"]["source_observation"], "no_comparable_build")
        self.assertFalse((self.root / ".glaeda").exists())
        receipt = self.run_plan(plan)
        self.assertTrue(all(value >= 0 for value in receipt["timings_seconds"].values()))
        advice = apple.explain(plan)
        self.assertEqual(advice["last_build_timings_seconds"], receipt["timings_seconds"])
        self.assertEqual(advice["explanation"]["source_observation"], "working_tree_requires_native_validation")
        self.assertFalse(advice["result_reuse"])
        (self.root / "Feature.swift").write_text("// edited\n")
        self.assertEqual(self.plan()["key"], plan["key"])
        self.assertEqual(apple.explain(self.plan())["explanation"]["next_action"], "run_native_build")
        subprocess.run(["/usr/bin/git", "-C", str(self.root), "-c", "user.name=Test", "-c", "user.email=test@example.invalid",
                        "commit", "--quiet", "--allow-empty", "-m", "new revision"], check=True)
        self.assertEqual(apple.explain(self.plan())["explanation"]["source_observation"], "commit_changed")
        self.assertEqual(apple.explain(self.plan("new"))["explanation"]["source_observation"], "no_comparable_build")

    def test_native_work_reports_only_bounded_known_task_values(self):
        self.script.write_text("#!/bin/sh\ncat <<'EOF'\nBuild Timing Summary\nSwiftCompile (2 tasks) | 99 seconds\n"
                               "Build Timing Summary\nSwiftEmitModule (1 task) | 12.853 seconds\n"
                               "PrivateSourceName (1 task) | 5 seconds\nLd (9999999 tasks) | 2 seconds\n"
                               "SwiftCompile (1 task) | -2 seconds\nnote: 2 hits / 2 cacheable tasks (100%)\n"
                               "note: 1 hit / 3 cacheable tasks (33%)\nEOF\n")
        result = self.run_plan(self.plan())["native_work"]
        self.assertEqual(result["last_reported_task_timings"], {"SwiftEmitModule": {"tasks": 1, "seconds": 12.853}})
        self.assertEqual(result["last_reported_compilation_cache"], {"hits": 1, "cacheable_tasks": 3})
        self.assertEqual(result["timing_summaries"], 2)
        self.assertTrue(result["task_times_may_overlap"])
        self.assertNotIn("PrivateSourceName", json.dumps(result))

    def test_native_work_telemetry_failure_does_not_strand_completed_build(self):
        plan = self.plan()
        with patch.object(apple, "native_work_summary", side_effect=OSError("telemetry unavailable")):
            result = self.run_plan(plan)
        self.assertEqual(result["exit_code"], 0)
        self.assertEqual(result["native_work"], {"state": "unavailable"})
        self.assertFalse((self.root / ".glaeda/apple-build/inflight.json").exists())
        with apple.store(plan) as state:
            name = "run-" + result["run_id"] + ".log"
            fd = os.open(name, os.O_WRONLY, dir_fd=state)
            os.ftruncate(fd, 64 * 1024 * 1024 + 1)
            os.close(fd)
            self.assertEqual(apple.native_work_summary(state, result["run_id"]), {"state": "unavailable"})

    def test_explanation_filters_untrusted_timing_fields(self):
        plan = self.plan()
        self.run_plan(plan)
        with apple.store(plan) as state:
            receipt = apple.read_json(state, "last-run.json")
            receipt["timings_seconds"] = {"native_command": "private path", "store_and_lock": -1, "unexpected": "secret"}
            apple.write_json(state, "last-run.json", receipt)
        self.assertEqual(apple.explain(plan)["last_build_timings_seconds"], {})

    def test_check_uses_build_cache_but_keeps_evidence_and_recovery_separate(self):
        baseline = self.plan()
        build = self.run_plan(baseline)
        checker = self.root / "check.sh"
        checker.write_text('#!/bin/sh\nprintf checked\n')
        checker.chmod(0o755)
        config = {"schema_version": 1, "profiles": {"app": self.profile},
                  "checks": {"app": {"engine": "script", "executable": "check.sh"}}}
        (self.root / "glaeda.apple.json").write_text(json.dumps(config))
        def prepare_again(project, name, generation, **options):
            return apple.prepare(project, name, generation, lambda *args: self.tools, **options)
        plan = prepare_again(self.root, "app", "default", operation="check")
        self.assertEqual(plan["key"], baseline["key"])
        check = apple.execute(plan, prepare_again)
        self.assertEqual(check["operation"], "check")
        with apple.store(plan) as state:
            self.assertEqual(apple.read_json(state, "last-run.json"), build)
            self.assertEqual(apple.read_json(state, "last-check.json"), check)
        checker.write_text(checker.read_text() + '# changed helper\n')
        with self.assertRaisesRegex(apple.Refusal, "changed during admission"):
            apple.execute(plan, prepare_again)
        with apple.store(plan) as state:
            apple.write_json(state, "inflight.json", {"run_id": "check", "operation": "check",
                             "cache_key": plan["key"], "pgid": None})
        with patch.object(apple, "group_absent", return_value=True):
            recovered = apple.recover(plan, "check")
        with apple.store(plan) as state:
            self.assertEqual(apple.read_json(state, "last-run.json"), build)
            self.assertEqual(apple.read_json(state, "last-check.json"), recovered)

    def test_optional_preparation_reuse_rechecks_inputs_outputs_and_lock(self):
        (self.root / "Package.swift").write_text("// manifest")
        self.tools["swift"] = str(self.script)
        config = {"schema_version": 1, "profiles": {"app": self.profile},
                  "preparations": {"app": {"engine": "swiftpm", "reuse": {
                      "inputs": ["Package.swift", "Package.resolved"],
                      "required_files": [{"cache": "products", "path": "result"}]}}}}
        (self.root / "glaeda.apple.json").write_text(json.dumps(config))
        def prepare_again(project, name, generation, **options):
            return apple.prepare(project, name, generation, lambda *args: self.tools, **options)
        def plan():
            return prepare_again(self.root, "app", "default", operation="dependencies")
        def run():
            return apple.execute(plan(), prepare_again, reuse_dependencies=True)
        self.assertEqual(run()["state"], "completed")
        self.assertEqual(run()["state"], "preparation_reused")
        # Opting in on a readiness hit must bind the existing cache too.
        original_key = plan()["key"]
        self.native_policy()
        self.assertEqual(run()["state"], "preparation_reused")
        binding = plan()["lineage"]
        self.assertTrue((self.root / ".glaeda/apple-build" / ("lineage-" + binding["lineage"] + ".json")).exists())
        self.assertEqual(plan()["key"], original_key)
        (self.root / "Source.swift").write_text("// unrelated edit")
        reused = run()
        self.assertEqual(reused["state"], "preparation_reused")
        self.assertFalse(reused["result_reuse"])
        self.assertTrue(reused["native_build_validation_required"])
        (self.root / "Package.resolved").write_text("changed dependency input")
        self.assertEqual(run()["state"], "completed")
        artifact = Path(plan()["paths"]["products"]) / "result"
        artifact.write_text("damaged")
        self.assertEqual(run()["state"], "completed")
        artifact.unlink()
        self.assertEqual(run()["state"], "completed")
        with apple.store(plan()) as state, apple.lock(state):
            with self.assertRaises(apple.Refusal):
                run()
        # A resolver can finish while inputs disappear. Complete without reuse
        # evidence, rather than retaining an inflight marker for a dead child.
        self.script.write_text(self.script.read_text() + '\nrm -f Package.swift\n')
        changed = plan()
        receipt = apple.execute(changed, prepare_again)
        self.assertEqual(receipt["state"], "completed")
        self.assertEqual(receipt["exit_code"], 1)
        self.assertEqual(receipt["preparation_status"], "observation_unavailable")
        self.assertNotIn("dependency_state", receipt)
        self.assertFalse((self.root / ".glaeda/apple-build/inflight.json").exists())

    def test_swift_driver_keeps_dispatch_name_and_checks_toolchain_identity(self):
        frontend = self.root / "swift-frontend"
        frontend.write_text("fixture")
        driver = self.root / "swift"
        driver.symlink_to(frontend)
        self.assertEqual(apple.swift_driver({"swift": str(frontend)}), str(driver))
        driver.unlink()
        driver.symlink_to(self.script)
        with self.assertRaisesRegex(apple.Refusal, "no longer matches"):
            apple.swift_driver({"swift": str(frontend)})

    def test_dependencies_reuse_paths_and_keep_build_receipt_separate(self):
        build_plan = self.plan()
        build = self.run_plan(build_plan)
        (self.root / "Package.swift").write_text("// fixture")
        self.tools["swift"] = str(self.script)
        self.write_config()
        config_path = self.root / "glaeda.apple.json"
        config = json.loads(config_path.read_text())
        config["preparations"] = {"app": {"engine": "swiftpm"}}
        config_path.write_text(json.dumps(config))
        def prepare_again(project, name, generation, **options):
            return apple.prepare(project, name, generation, lambda *args: self.tools, **options)
        normal = prepare_again(self.root, "app", "default")
        plan = prepare_again(self.root, "app", "default", operation="dependencies")
        self.assertEqual(normal["key"], plan["key"])
        self.assertEqual(normal["paths"], plan["paths"])
        self.assertEqual(plan["argv"][-1], "resolve")
        receipt = apple.execute(plan, prepare_again)
        self.assertEqual(receipt["operation"], "dependencies")
        with apple.store(plan) as state:
            self.assertEqual(apple.read_json(state, "last-run.json")["run_id"], build["run_id"])
            self.assertEqual(apple.read_json(state, "last-dependencies.json")["run_id"], receipt["run_id"])
        config["preparations"]["app"]["package"] = "missing"
        config_path.write_text(json.dumps(config))
        with self.assertRaises((apple.Refusal, FileNotFoundError)):
            apple.execute(plan, prepare_again)

    def test_xcode_dependency_plan_is_read_only_and_recipe_changes_revalidate(self):
        (self.root / "App.xcodeproj").mkdir()
        config = {"schema_version": 1, "profiles": {"app": self.profile},
                  "preparations": {"app": {"engine": "xcode", "project": "App.xcodeproj", "scheme": "App"}}}
        (self.root / "glaeda.apple.json").write_text(json.dumps(config))
        def prepare_again(project, name, generation, **options):
            return apple.prepare(project, name, generation, lambda *args: self.tools, **options)
        plan = prepare_again(self.root, "app", "default", operation="dependencies")
        self.assertEqual(plan["argv"][-1], "-resolvePackageDependencies")
        self.assertEqual(apple.inspect(plan)["state"], "cold")
        self.assertFalse((self.root / ".glaeda").exists())
        config["preparations"]["app"]["scheme"] = "Other"
        (self.root / "glaeda.apple.json").write_text(json.dumps(config))
        with self.assertRaisesRegex(apple.Refusal, "changed during admission"):
            apple.execute(plan, prepare_again)

    def test_dependency_presence_sentinel_ignores_bytes_but_detects_missing_files(self):
        plan = self.plan()
        (self.root / "Package.swift").write_text("// inputs")
        cache = Path(plan["paths"]["source_packages"])
        cache.mkdir(parents=True)
        sentinel = cache / "workspace-state.json"
        artifact = cache / "artifact.plist"
        sentinel.write_text('{"artifacts":[1,2]}')
        artifact.write_text("artifact-v1")
        declaration = {"inputs": ["Package.swift"], "required_files": [
            {"cache": "source_packages", "path": "workspace-state.json", "match": "exists"},
            {"cache": "source_packages", "path": "artifact.plist"}]}
        observe = lambda: apple.dependency_observation(self.root, plan["paths"], declaration)
        first = observe()
        sentinel.write_text('{"artifacts":[2,1]}')
        self.assertEqual(first, observe())
        artifact.write_text("artifact-v2")
        self.assertNotEqual(first, observe())
        sentinel.unlink()
        self.assertIsNone(observe()["outputs"])
        sentinel.mkdir()
        with self.assertRaises((apple.Refusal, OSError)):
            observe()
        sentinel.rmdir()
        sentinel.symlink_to(self.script)
        with self.assertRaisesRegex(apple.Refusal, "escapes"):
            observe()
        declaration["required_files"][0]["match"] = "unknown"
        with self.assertRaises(apple.Refusal):
            observe()

    def test_settings_sdk_toolchain_and_recipe_separate_generations(self):
        key = self.plan()["key"]
        self.tools["sdk_build"] = "test2"
        self.assertNotEqual(key, self.plan()["key"])
        key = self.plan()["key"]
        self.profile["environment"]["CONFIG"] = "release"
        self.write_config()
        self.assertNotEqual(key, self.plan()["key"])
        key = self.plan()["key"]
        self.script.write_text(self.script.read_text() + "# new build recipe\n")
        self.assertNotEqual(key, self.plan()["key"])

    def test_repeated_build_executes_and_retains_native_state(self):
        plan = self.plan()
        first = self.run_plan(plan)
        self.assertEqual(first["exit_code"], 0)
        product = Path(plan["paths"]["products"])
        (product / "native-cache").write_text("warm")
        second = self.run_plan(self.plan())
        self.assertEqual(second["exit_code"], 0)
        self.assertEqual((product / "native-cache").read_text(), "warm")
        self.assertNotEqual(first["run_id"], second["run_id"])
        self.assertFalse(second["result_reuse"])
        self.assertEqual(apple.inspect(plan)["state"], "prepared")
        log = self.root / ".glaeda/apple-build" / ("run-" + second["run_id"] + ".log")
        self.assertEqual(log.read_text(), "private-output")
        self.assertNotIn(str(self.root), json.dumps(second))
        self.assertEqual(log.stat().st_mode & 0o777, 0o600)

    def test_exact_source_contract_binds_commit_tree_and_clean_worktree(self):
        (self.root / ".gitignore").write_text(".glaeda/\n")
        subprocess.run(["/usr/bin/git", "-C", str(self.root), "add", ".gitignore", "build.sh", "glaeda.apple.json"], check=True)
        subprocess.run(["/usr/bin/git", "-C", str(self.root), "-c", "user.name=Test", "-c", "user.email=test@example.invalid",
                        "commit", "--quiet", "-m", "bind fixture source"], check=True)
        plan = self.plan()
        expected = apple.source_snapshot(plan)
        self.assertTrue(expected["clean"])
        self.assertRegex(expected["tree"], r"^[a-f0-9]{40,64}$")
        receipt = apple.execute(
            plan, lambda *args: self.plan(args[2]),
            expected_commit=expected["commit"], expected_tree=expected["tree"], require_clean_source=True)
        self.assertEqual(receipt["source_validation"], "exact_commit_tree_clean")
        self.assertEqual(receipt["source_before"], expected)
        self.assertEqual(receipt["source_after"], expected)

        product = Path(plan["paths"]["products"]) / "result"
        product.unlink()
        (self.root / "untracked.swift").write_text("// dirty\n")
        with self.assertRaisesRegex(apple.Refusal, "clean-source"):
            apple.execute(
                self.plan(), lambda *args: self.plan(args[2]),
                expected_commit=expected["commit"], expected_tree=expected["tree"], require_clean_source=True)
        self.assertFalse(product.exists())

        (self.root / "untracked.swift").unlink()
        (self.root / "Tracked.swift").write_text("// next revision\n")
        subprocess.run(["/usr/bin/git", "-C", str(self.root), "add", "Tracked.swift"], check=True)
        subprocess.run(["/usr/bin/git", "-C", str(self.root), "-c", "user.name=Test", "-c", "user.email=test@example.invalid",
                        "commit", "--quiet", "-m", "next fixture source"], check=True)
        current = apple.source_snapshot(self.plan())
        with self.assertRaisesRegex(apple.Refusal, "expected source commit"):
            apple.execute(
                self.plan(), lambda *args: self.plan(args[2]),
                expected_commit=expected["commit"], expected_tree=current["tree"], require_clean_source=True)
        with self.assertRaisesRegex(apple.Refusal, "expected source tree"):
            apple.execute(
                self.plan(), lambda *args: self.plan(args[2]),
                expected_commit=current["commit"], expected_tree=expected["tree"], require_clean_source=True)

    def test_parent_secrets_and_build_overrides_do_not_leak(self):
        with patch.dict(os.environ, {"SECRET_TOKEN": "secret", "CONFIG": "ambient-release", "SDKROOT": "/wrong"}):
            plan = self.plan()
        for key in ("SECRET_TOKEN", "CONFIG", "SDKROOT"):
            self.assertNotIn(key, plan["environment"])

    def test_project_lock_refuses_second_builder(self):
        plan = self.plan()
        with apple.store(plan, True) as state, apple.lock(state):
            with self.assertRaisesRegex(apple.Refusal, "another Glaeda"):
                self.run_plan(plan)
        self.assertFalse(Path(plan["paths"]["products"]).exists())

    def test_waiting_builder_runs_after_owner_releases(self):
        plan = self.plan()
        with apple.store(plan, True) as state:
            fd = os.open("build.lock", os.O_RDWR | os.O_CREAT, 0o600, dir_fd=state)
            try:
                apple.fcntl.flock(fd, apple.fcntl.LOCK_EX)
                with patch.object(apple.time, "sleep", lambda seconds: apple.fcntl.flock(fd, apple.fcntl.LOCK_UN)):
                    result = apple.execute(plan, lambda *args: self.plan(), wait_seconds=1)
                self.assertEqual(result["exit_code"], 0)
                self.assertEqual(Path(plan["paths"]["products"]).joinpath("result").read_text(), "ok")
            finally:
                os.close(fd)

    def test_wait_timeout_preserves_owner_and_creates_no_run(self):
        plan = self.plan()
        with apple.store(plan, True) as state, apple.lock(state):
            with self.assertRaisesRegex(apple.Refusal, "wait deadline"):
                apple.execute(plan, wait_seconds=1)
            with self.assertRaisesRegex(apple.Refusal, "another Glaeda"):
                self.run_plan(plan)
            self.assertFalse(Path(plan["paths"]["products"]).exists())
            with self.assertRaises(FileNotFoundError):
                apple.read_json(state, "inflight.json")

    def test_waiting_builder_revalidates_changed_recipe(self):
        plan = self.plan()
        with apple.store(plan, True) as state:
            fd = os.open("build.lock", os.O_RDWR | os.O_CREAT, 0o600, dir_fd=state)
            def release(seconds):
                self.script.write_text("#!/bin/sh\necho changed\n")
                apple.fcntl.flock(fd, apple.fcntl.LOCK_UN)
            try:
                apple.fcntl.flock(fd, apple.fcntl.LOCK_EX)
                with patch.object(apple.time, "sleep", release):
                    with self.assertRaisesRegex(apple.Refusal, "changed during admission"):
                        apple.execute(plan, lambda *args: self.plan(), wait_seconds=1)
                self.assertFalse(Path(plan["paths"]["products"]).exists())
            finally:
                os.close(fd)

    def test_cancelled_waiter_returns_bounded_output_without_disturbing_owner(self):
        plan = self.plan()
        with apple.store(plan, True) as state, apple.lock(state):
            output = io.StringIO()
            with patch.object(apple, "prepare", return_value=plan), patch.object(apple.sys, "argv", ["apple-build", "warm", "--wait-seconds", "1"]), patch.object(apple.time, "sleep", side_effect=KeyboardInterrupt), contextlib.redirect_stderr(output):
                self.assertEqual(apple.main(), 130)
            self.assertEqual(json.loads(output.getvalue())["state"], "cancelled")
            self.assertNotIn(str(self.root), output.getvalue())
            with self.assertRaisesRegex(apple.Refusal, "another Glaeda"):
                self.run_plan(plan)
            with self.assertRaises(FileNotFoundError):
                apple.read_json(state, "inflight.json")

    def test_invalid_wait_refused_before_store_creation(self):
        for value in (-1, 3601, True, 1.5):
            with self.subTest(value=value), self.assertRaises(apple.Refusal):
                apple.execute(self.plan(), wait_seconds=value)
        self.assertFalse((self.root / ".glaeda").exists())

    def test_revalidation_precedes_cache_mutation(self):
        plan = self.plan()
        self.tools["swift_version"] = "changed"
        with self.assertRaisesRegex(apple.Refusal, "changed during admission"):
            self.run_plan(plan)
        self.assertFalse(Path(plan["paths"]["products"]).exists())

    def test_quarantine_published_before_lock_admission_prevents_execution(self):
        plan = self.plan()
        original_lock = apple.lock

        @contextlib.contextmanager
        def prior_owner_quarantines(state):
            with original_lock(state):
                apple.write_json(state, "quarantine-" + plan["key"] + ".json", {"state": "interrupted"})
                yield

        with patch.object(apple, "lock", prior_owner_quarantines):
            with self.assertRaisesRegex(apple.Refusal, "interrupted"):
                self.run_plan(plan)
        self.assertFalse(Path(plan["paths"]["products"]).exists())

    def test_unmarked_state_and_symlinks_are_not_adopted(self):
        plan = self.plan()
        (self.root / ".glaeda").mkdir()
        (self.root / ".glaeda/apple-build").symlink_to(self.root, target_is_directory=True)
        with self.assertRaises(OSError):
            apple.inspect(plan)
        (self.root / ".glaeda/apple-build").unlink()
        (self.root / ".glaeda/apple-build").mkdir()
        with self.assertRaisesRegex(apple.Refusal, "incomplete"):
            self.run_plan(plan)

    def test_interrupted_build_blocks_then_quarantines_after_exact_recovery(self):
        plan = self.plan()
        child = subprocess.Popen(["/usr/bin/true"], start_new_session=True)
        child.wait()
        marker = {"schema_version": 1, "run_id": "interrupted", "pgid": child.pid, "cache_key": plan["key"]}
        with apple.store(plan, True) as state:
            apple.write_json(state, "inflight.json", marker)
        with self.assertRaisesRegex(apple.Refusal, "unfinished"):
            self.run_plan(plan)
        with self.assertRaises(apple.Refusal):
            apple.recover(plan, "wrong-id")
        recovered = apple.recover(plan, "interrupted")
        self.assertEqual(recovered["state"], "interruption_recovered")
        with self.assertRaisesRegex(apple.Refusal, "interrupted"):
            self.run_plan(plan)
        self.assertEqual(self.run_plan(self.plan("rebuild"))["exit_code"], 0)

    def test_dependency_recovery_preserves_build_receipt(self):
        plan = self.plan()
        build = self.run_plan(plan)
        with apple.store(plan) as state:
            apple.write_json(state, "inflight.json", {"run_id": "deps", "pgid": None,
                             "cache_key": plan["key"], "operation": "dependencies"})
        with patch.object(apple, "group_absent", return_value=True):
            recovered = apple.recover(plan, "deps")
        self.assertEqual(recovered["operation"], "dependencies")
        with apple.store(plan) as state:
            self.assertEqual(apple.read_json(state, "last-run.json"), build)
            self.assertEqual(apple.read_json(state, "last-dependencies.json"), recovered)
        with self.assertRaisesRegex(apple.Refusal, "interrupted"):
            apple.inspect(plan)

    def test_recovery_does_not_release_a_present_process_group(self):
        plan = self.plan()
        with apple.store(plan, True) as state:
            apple.write_json(state, "inflight.json", {"run_id": "live", "pgid": os.getpgrp(), "cache_key": plan["key"]})
        with self.assertRaises(apple.Refusal):
            apple.recover(plan, "live")

    def test_native_adapters_bind_their_cache_locations(self):
        (self.root / "Package.swift").write_text("// fixture")
        self.profile = {"engine": "swiftpm", "product": "Example", "configuration": "release"}
        self.write_config()
        plan = self.plan()
        self.assertIn(plan["paths"]["scratch"], plan["argv"])
        self.assertIn("release", plan["argv"])
        (self.root / "Example.xcodeproj").mkdir()
        self.profile = {"engine": "xcode", "project": "Example.xcodeproj", "scheme": "Example", "configuration": "Debug",
                        "destination": "platform=macOS", "settings": {"CODE_SIGNING_ALLOWED": "NO"}}
        self.write_config()
        plan = self.plan()
        self.assertIn(plan["paths"]["derived_data"], plan["argv"])
        self.assertIn("CODE_SIGNING_ALLOWED=NO", plan["argv"])

    def test_invalid_configuration_has_no_state_effect(self):
        self.profile["arguments"] = ["{unknown}"]
        self.write_config()
        with self.assertRaises(apple.Refusal):
            self.plan()
        self.assertFalse((self.root / ".glaeda").exists())

    def test_malformed_interruption_record_has_a_bounded_public_refusal(self):
        plan = self.plan()
        with apple.store(plan, True) as state:
            apple.write_json(state, "inflight.json", {})
        output = io.StringIO()
        with patch.object(apple, "prepare", return_value=plan), patch("sys.argv", ["apple-build", "plan"]), contextlib.redirect_stderr(output):
            self.assertEqual(apple.main(), 2)
        refusal = json.loads(output.getvalue())
        self.assertEqual(refusal["state"], "refused")
        self.assertNotIn(str(self.root), output.getvalue())
        self.assertFalse(Path(plan["paths"]["products"]).exists())


class AppleQueueTests(unittest.TestCase):
    setUp = AppleBuildTests.setUp
    tearDown = AppleBuildTests.tearDown
    write_config = AppleBuildTests.write_config
    plan = AppleBuildTests.plan

    def initialized(self):
        plan = self.plan()
        with apple.store(plan, True):
            pass
        return plan

    def prepare(self, project, name, generation, **options):
        return apple.prepare(project, name, generation, lambda *args: self.tools, **options)

    def submit(self):
        return queue.submit(self.plan(), lambda project: {"wake": "deferred_for_test"})

    def work(self, execute=apple.execute):
        queue.worker(self.root, self.prepare, execute, debounce=0)

    def refresh_plan(self):
        (self.root / "Package.swift").write_text("// fixture")
        config = json.loads((self.root / "glaeda.apple.json").read_text())
        config["preparations"] = {"app": {"engine": "swiftpm"}}
        config["checks"] = {"app": {"engine": "script", "executable": "build.sh"}}
        (self.root / "glaeda.apple.json").write_text(json.dumps(config))
        return queue.request_plan(self.root, "app", "default", "refresh", self.prepare)

    def test_refresh_batches_preparation_and_check(self):
        self.initialized()
        plan = self.refresh_plan()
        requests = [queue.submit(plan, lambda p: {}) for _ in range(2)]
        operations = []
        def execute(plan, prepare, **options):
            operations.append((plan["operation"], options.get("reuse_dependencies", False)))
            if plan["operation"] == "dependencies":
                return {"exit_code": 0, "state": "preparation_reused"}
            return apple.execute(plan, prepare)
        self.work(execute)
        self.assertEqual(operations, [("dependencies", True), ("check", False)])
        results = [queue.status(self.root, r["request_id"]) for r in requests]
        self.assertEqual({r["state"] for r in results}, {"completed"})
        self.assertEqual(len({r["result"]["run_id"] for r in results}), 1)

    def test_refresh_failure_stops_before_check(self):
        self.initialized()
        request = queue.submit(self.refresh_plan(), lambda p: {})
        operations = []
        def execute(plan, prepare, **options):
            operations.append(plan["operation"])
            return {"exit_code": 7}
        self.work(execute)
        result = queue.status(self.root, request["request_id"])
        self.assertEqual(operations, ["dependencies"])
        self.assertEqual(result["state"], "failed")
        self.assertEqual(result["result"]["reason"], "dependency_preparation_failed")

    def test_refresh_recipe_change_refuses_before_preparation(self):
        self.initialized()
        request = queue.submit(self.refresh_plan(), lambda p: {})
        config = json.loads((self.root / "glaeda.apple.json").read_text())
        config["preparations"]["app"]["package"] = "sub"
        (self.root / "sub").mkdir()
        (self.root / "sub/Package.swift").write_text("// fixture")
        (self.root / "glaeda.apple.json").write_text(json.dumps(config))
        self.work(lambda *args, **kwargs: self.fail("must refuse before execution"))
        self.assertEqual(queue.status(self.root, request["request_id"])["result"]["reason"], "configuration_changed")

    def test_refresh_revalidates_check_after_preparation(self):
        self.initialized()
        request = queue.submit(self.refresh_plan(), lambda p: {})
        def execute(plan, prepare, **options):
            if plan["operation"] == "dependencies":
                self.script.write_text(self.script.read_text() + "# changed during preparation\n")
                return {"exit_code": 0}
            return apple.execute(plan, prepare)
        self.work(execute)
        result = queue.status(self.root, request["request_id"])
        self.assertEqual((result["state"], result["result"]["reason"]), ("failed", "native_refused"))
        self.assertFalse(list((self.root / ".glaeda/apple-build").glob("run-*.log")))

    def test_wait_cli_deadline_and_terminal_exit_codes(self):
        self.initialized()
        request = self.submit()
        argv = [sys.executable, str(Path(apple.__file__).resolve()), "wait-request",
                "--project", str(self.root), "--request-id", request["request_id"]]
        pending = subprocess.run(argv, capture_output=True, text=True, timeout=5)
        self.assertEqual(pending.returncode, 124)
        self.assertEqual(json.loads(pending.stdout)["wait"], "deadline")
        self.work()
        completed = subprocess.run(argv, capture_output=True, text=True, timeout=5)
        self.assertEqual(completed.returncode, 0)
        self.assertEqual(json.loads(completed.stdout)["state"], "completed")

    @unittest.skipUnless(hasattr(queue.select, "kqueue"), "requires native kqueue")
    def test_real_wait_deadline_without_worker_does_not_mutate(self):
        self.initialized()
        request = self.submit()
        ledger = self.root / ".glaeda/apple-build/requests.json"
        before = ledger.read_bytes()
        result = queue.wait_request(self.root, request["request_id"], 1)
        self.assertEqual((result["state"], result["wait"]), ("pending", "deadline"))
        self.assertEqual(ledger.read_bytes(), before)

    def test_ledger_replacement_between_open_and_fstat_retries(self):
        plan = self.initialized()
        request = self.submit()
        ledger = self.root / ".glaeda/apple-build/requests.json"
        inode = ledger.stat().st_ino
        original_fstat = os.fstat
        replaced = []
        def fstat(fd):
            info = original_fstat(fd)
            if info.st_ino == inode and not replaced:
                replaced.append(True)
                replacement = ledger.with_suffix(".replacement")
                replacement.write_bytes(ledger.read_bytes())
                replacement.chmod(0o600)
                replacement.replace(ledger)
                info = original_fstat(fd)
                self.assertEqual(info.st_nlink, 0)
            return info
        with patch.object(queue.os, "fstat", fstat):
            result = queue.status(self.root, request["request_id"])
        self.assertTrue(replaced)
        self.assertEqual(result["state"], "pending")

    def test_wait_deadline_preserves_pending_request(self):
        self.initialized()
        request = self.submit()
        result = queue.wait_request(self.root, request["request_id"], 0)
        self.assertEqual((result["state"], result["wait"]), ("pending", "deadline"))
        self.assertEqual(queue.status(self.root, request["request_id"])["state"], "pending")
        with self.assertRaises(apple.Refusal):
            queue.wait_request(self.root, request["request_id"], 3601)

    def test_wait_completion_during_event_registration_is_not_lost(self):
        self.initialized()
        request = self.submit()
        @contextlib.contextmanager
        def events(state):
            self.work()
            yield lambda seconds: self.fail("already completed before second read")
        result = queue.wait_request(self.root, request["request_id"], 2, events)
        self.assertEqual((result["state"], result["wait"]), ("completed", "terminal"))

    def test_wait_ignores_unrelated_event_and_revalidates_ledger(self):
        self.initialized()
        request = self.submit()
        ledger = self.root / ".glaeda/apple-build/requests.json"
        @contextlib.contextmanager
        def events(state):
            def changed(seconds):
                ledger.write_text("invalid")
            yield changed
        with self.assertRaises(apple.Refusal):
            queue.wait_request(self.root, request["request_id"], 2, events)

    @unittest.skipUnless(hasattr(queue.select, "kqueue"), "requires native kqueue")
    def test_real_event_wait_observes_atomic_completion(self):
        self.initialized()
        request = self.submit()
        ready = threading.Event()
        errors = []
        receipt = {"run_id": "a" * 32, "exit_code": 0,
                   "source_before": {"commit": "b" * 40, "clean": True},
                   "source_after": {"commit": "b" * 40, "clean": True}}
        def complete():
            try:
                if not ready.wait(2):
                    raise RuntimeError("waiter did not arm")
                self.work(lambda *args, **kwargs: receipt)
            except BaseException as error:
                errors.append(error)
        @contextlib.contextmanager
        def events(state):
            with queue.request_events(state) as changed:
                def wait(seconds):
                    ready.set()
                    return changed(seconds)
                yield wait
        thread = threading.Thread(target=complete)
        thread.start()
        try:
            result = queue.wait_request(self.root, request["request_id"], 3, events)
        finally:
            thread.join(3)
        self.assertFalse(thread.is_alive())
        self.assertEqual(errors, [])
        self.assertEqual(result["state"], "completed")

    def test_pending_requests_share_one_native_run(self):
        self.initialized()
        requests = [self.submit() for _ in range(3)]
        self.work()
        results = [queue.status(self.root, r["request_id"]) for r in requests]
        self.assertEqual({r["state"] for r in results}, {"completed"})
        self.assertEqual(len({r["result"]["run_id"] for r in results}), 1)
        self.assertFalse(results[0]["result"]["source_before"]["clean"])
        self.assertEqual(len(list((self.root / ".glaeda/apple-build").glob("run-*.log"))), 1)

    def test_arrival_during_execution_gets_another_batch(self):
        self.initialized()
        first = self.submit()
        later = []
        def execute(plan, prepare, **options):
            if not later:
                later.append(self.submit())
            return apple.execute(plan, prepare, **options)
        self.work(execute)
        a = queue.status(self.root, first["request_id"])
        b = queue.status(self.root, later[0]["request_id"])
        self.assertNotEqual(a["batch_id"], b["batch_id"])
        self.assertNotEqual(a["result"]["run_id"], b["result"]["run_id"])

    def test_drift_refuses_without_native_execution(self):
        plan = self.initialized()
        request = self.submit()
        self.script.write_text("#!/bin/sh\necho changed\n")
        self.work()
        result = queue.status(self.root, request["request_id"])
        self.assertEqual(result["state"], "failed")
        self.assertEqual(result["result"]["reason"], "configuration_changed")
        self.assertFalse(Path(plan["paths"]["products"]).exists())

    def test_spawn_failure_preserves_request_for_explicit_wake(self):
        plan = self.initialized()
        with patch.object(queue.subprocess, "Popen", side_effect=OSError):
            request = queue.submit(plan)
        self.assertEqual(request["wake"], "failed")
        self.assertEqual(queue.status(self.root, request["request_id"])["state"], "pending")
        self.work()
        self.assertEqual(queue.status(self.root, request["request_id"])["state"], "completed")

    def test_abandoned_batch_is_not_replayed_or_claimed_successful(self):
        self.initialized()
        request = self.submit()
        with self.assertRaises(SystemExit):
            self.work(lambda *args, **options: (_ for _ in ()).throw(SystemExit()))
        self.assertEqual(queue.status(self.root, request["request_id"])["state"], "running")
        self.work()
        result = queue.status(self.root, request["request_id"])
        self.assertEqual(result["state"], "interrupted")
        self.assertEqual(result["result"]["reason"], "worker_interrupted")
        self.assertFalse(list((self.root / ".glaeda/apple-build").glob("run-*.log")))

    def test_crash_after_native_completion_never_infers_queue_success(self):
        self.initialized()
        request = self.submit()
        def execute(plan, prepare, **options):
            apple.execute(plan, prepare, **options)
            raise SystemExit()
        with self.assertRaises(SystemExit):
            self.work(execute)
        self.assertTrue((self.root / ".glaeda/apple-build/last-run.json").exists())
        self.work()
        result = queue.status(self.root, request["request_id"])
        self.assertEqual(result["state"], "interrupted")
        self.assertIsNone(result["result"]["run_id"])
        self.assertEqual(len(list((self.root / ".glaeda/apple-build").glob("run-*.log"))), 1)

    def test_enqueue_at_idle_exit_receives_a_worker(self):
        plan = self.initialized()
        original_flock = queue.fcntl.flock
        entered = threading.Event()
        finished = threading.Event()
        requests, errors, threads = [], [], []
        receipt = {"run_id": "a" * 32, "exit_code": 0,
                   "source_before": {"commit": "b" * 40, "clean": True},
                   "source_after": {"commit": "b" * 40, "clean": True}}
        def enqueue():
            entered.set()
            try:
                def wake(project):
                    queue.worker(project, self.prepare, lambda *args, **kwargs: receipt, debounce=0)
                    return {"wake": "requested"}
                requests.append(queue.submit(plan, wake))
            except BaseException as error:
                errors.append(error)
            finally:
                finished.set()
        def flock(fd, operation):
            original_flock(fd, operation)
            if operation == queue.fcntl.LOCK_UN and not threads:
                # Idle release must occur before the enqueue critical section ends.
                with apple.store(plan) as state:
                    probe = os.open("requests.lock", os.O_RDWR, dir_fd=state)
                    try:
                        with self.assertRaises(BlockingIOError):
                            original_flock(probe, queue.fcntl.LOCK_EX | queue.fcntl.LOCK_NB)
                    finally:
                        os.close(probe)
                thread = threading.Thread(target=enqueue)
                threads.append(thread)
                thread.start()
                self.assertTrue(entered.wait(1))
        with patch.object(queue.fcntl, "flock", flock):
            self.work()
            self.assertTrue(finished.wait(5))
            for thread in threads:
                thread.join(timeout=1)
        self.assertEqual(errors, [])
        self.assertEqual(len(requests), 1)
        self.assertEqual(queue.status(self.root, requests[0]["request_id"])["state"], "completed")

    def test_existing_worker_does_not_reclassify_running_records(self):
        plan = self.initialized()
        request = self.submit()
        with apple.store(plan) as state, queue.mutex(state, "request-worker.lock", False):
            self.work()
        self.assertEqual(queue.status(self.root, request["request_id"])["state"], "pending")

    def test_status_is_read_only_and_forget_requires_terminal(self):
        plan = self.initialized()
        with apple.store(plan) as state:
            self.assertEqual(queue.read_queue(state), [])
        self.assertFalse((self.root / ".glaeda/apple-build/requests.json").exists())
        request = self.submit()
        ledger = self.root / ".glaeda/apple-build/requests.json"
        before = ledger.stat().st_mtime_ns
        queue.status(self.root, request["request_id"])
        self.assertEqual(ledger.stat().st_mtime_ns, before)
        with self.assertRaises(apple.Refusal):
            queue.status(self.root, request["request_id"], forget=True)
        self.work()
        self.assertEqual(queue.status(self.root, request["request_id"], forget=True)["state"], "forgotten")
        with self.assertRaises(apple.Refusal):
            queue.status(self.root, request["request_id"])

    def test_history_bound_requires_explicit_collection(self):
        self.initialized()
        with patch.object(queue, "MAX_REQUESTS", 2):
            self.submit()
            self.submit()
            with self.assertRaisesRegex(apple.Refusal, "history full"):
                self.submit()

    def test_unsafe_ledger_is_refused_without_mutation(self):
        plan = self.initialized()
        ledger = self.root / ".glaeda/apple-build/requests.json"
        ledger.symlink_to(self.script)
        original = self.script.read_bytes()
        with self.assertRaises(OSError):
            self.submit()
        self.assertEqual(self.script.read_bytes(), original)
        ledger.unlink()
        ledger.write_text('{"schema_version":1,"requests":[{"id":"bad"}]}')
        ledger.chmod(0o600)
        with self.assertRaises(apple.Refusal):
            self.submit()

    def test_direct_snapshot_entrypoint_has_bounded_queue_refusals(self):
        self.initialized()
        run = subprocess.run([sys.executable, apple.__file__, "request-status", "--project", str(self.root),
                              "--request-id", "a" * 32], capture_output=True, text=True)
        self.assertEqual(run.returncode, 2)
        self.assertEqual(json.loads(run.stderr)["reason"], "request not found")
        self.assertNotIn("Traceback", run.stderr)

    def test_changed_worker_generation_does_not_execute(self):
        self.initialized()
        request = self.submit()
        with patch.object(queue, "RUNTIME", "0" * 64):
            self.work()
        result = queue.status(self.root, request["request_id"])
        self.assertEqual(result["result"]["reason"], "runtime_changed")


if __name__ == "__main__":
    unittest.main()
