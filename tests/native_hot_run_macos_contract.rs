#![cfg(target_os = "macos")]

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("glaeda-macos-{}-{nonce}", std::process::id()));
        fs::create_dir(&root).unwrap();
        assert!(
            Command::new("/usr/bin/git")
                .args(["init", "--quiet"])
                .arg(&root)
                .status()
                .unwrap()
                .success()
        );
        Self(root.canonicalize().unwrap())
    }

    fn run(&self, options: &[&str], script: &str) -> Output {
        self.run_at(&self.0, options, script)
    }

    fn run_at(&self, task: &std::path::Path, options: &[&str], script: &str) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_glaeda-hot-run"));
        for name in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_COMMON_DIR",
            "GIT_CEILING_DIRECTORIES",
            "GIT_DISCOVERY_ACROSS_FILESYSTEM",
        ] {
            command.env_remove(name);
        }
        command
            .arg("--resident")
            .arg(&self.0)
            .arg("--task")
            .arg(task)
            .arg("--measurement")
            .arg(self.0.join("receipt.json"))
            .args(options)
            .args(["--", "/bin/sh", "-c", script])
            .output()
            .unwrap()
    }

    fn receipt(&self) -> Value {
        serde_json::from_slice(&fs::read(self.0.join("receipt.json")).unwrap()).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn native_macos_reuses_project_state_and_reports_only_observed_resources() {
    let fixture = Fixture::new();
    let warm = fixture.run(
        &["--cache", "target:native"],
        "mkdir target; printf warm > target/state",
    );
    assert!(
        warm.status.success(),
        "{}",
        String::from_utf8_lossy(&warm.stderr)
    );
    let reuse = fixture.run(
        &["--cache", "target:native"],
        "test \"$(cat target/state)\" = warm",
    );
    assert!(
        reuse.status.success(),
        "{}",
        String::from_utf8_lossy(&reuse.stderr)
    );
    let receipt = fixture.receipt();
    assert_eq!(receipt["schema_version"], 6);
    assert_eq!(receipt["exit_code"], 0);
    assert_eq!(receipt["completion_reason"], "exited");
    assert_eq!(receipt["cross_worktree"], false);
    assert_eq!(receipt["cache_views"][0]["mode"], "native");
    assert_eq!(
        receipt["resource_accounting"],
        "unavailable_for_measured_command"
    );
    assert!(receipt["max_rss_kib"].is_null());
    assert!(receipt["user_cpu_seconds"].is_null());
    assert!(receipt["native_target_observation"].is_null());
    assert!(receipt["elapsed_seconds"].as_f64().unwrap() >= 0.0);
    // A cold native rebuild remains possible without a Glaeda-owned cache generation.
    fs::remove_dir_all(fixture.0.join("target")).unwrap();
    assert!(
        fixture
            .run(
                &["--cache", "target:native"],
                "mkdir target; printf rebuilt > target/state"
            )
            .status
            .success()
    );
}

#[test]
fn native_macos_distinguishes_exit_from_signal() {
    let fixture = Fixture::new();
    assert_eq!(fixture.run(&[], "exit 143").status.code(), Some(143));
    assert!(fixture.receipt()["signal"].is_null());
    assert_eq!(fixture.receipt()["completion_reason"], "exited");
    assert_eq!(fixture.run(&[], "kill -TERM $$").status.code(), Some(143));
    assert_eq!(fixture.receipt()["signal"], 15);
    assert_eq!(fixture.receipt()["completion_reason"], "signaled");
}

#[test]
fn native_macos_rejects_unimplemented_controls_and_private_views_before_execution() {
    for options in [
        vec!["--timeout", "1"],
        vec!["--resource-profile", "big-red-heavy"],
        vec!["--cpu-set", "0"],
        vec!["--cache", "target:private-copy"],
    ] {
        let fixture = Fixture::new();
        let result = fixture.run(&options, "touch executed");
        assert!(!result.status.success());
        assert!(!fixture.0.join("executed").exists());
        assert!(!fixture.0.join("receipt.json").exists());
    }
}

#[test]
fn native_macos_rejects_another_worktree_before_execution() {
    let fixture = Fixture::new();
    let other = Fixture::new();
    let result = fixture.run_at(&other.0, &[], "touch executed");
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("physical Git worktree root"));
    assert!(!other.0.join("executed").exists());
    assert!(!fixture.0.join("receipt.json").exists());
}
