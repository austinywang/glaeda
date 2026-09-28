#![cfg(target_os = "linux")]

use std::fs::{self, OpenOptions};
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

const BINARY: &str = env!("CARGO_BIN_EXE_glaeda-cargo-target-value");
const COST_BINARY: &str = env!("CARGO_BIN_EXE_glaeda-cargo-target-observe");
const GIT: &str = "/usr/bin/git";
const COMPARISON_KEY: &str =
    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

struct Fixture {
    root: PathBuf,
    checkout: PathBuf,
    checkout_observation: Value,
    target_observation: Value,
}

impl Fixture {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("current time")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "glaeda-cargo-target-value-cli-{}-{nonce}",
            std::process::id()
        ));
        let checkout = root.join("checkout");
        fs::create_dir_all(checkout.join("target/debug")).expect("create target fixture");
        fs::write(checkout.join("tracked.txt"), "initial\n").expect("write tracked fixture");
        fs::write(checkout.join("target/debug/artifact"), "compiled\n")
            .expect("write target fixture");
        git(&checkout, &["init", "-b", "main"]);
        git(&checkout, &["add", "tracked.txt"]);
        git(
            &checkout,
            &[
                "-c",
                "user.name=Glaeda Test",
                "-c",
                "user.email=glaeda-test@example.invalid",
                "commit",
                "-m",
                "fixture",
            ],
        );
        git(
            &checkout,
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/teamleaderleo/glaeda.git",
            ],
        );
        let output = Command::new(COST_BINARY)
            .args([
                "--checkout",
                checkout.to_str().expect("UTF-8 fixture path"),
                "--output",
                "json",
            ])
            .output()
            .expect("observe target fixture");
        assert!(output.status.success(), "stderr: {:?}", output.stderr);
        let report: Value = serde_json::from_slice(&output.stdout).expect("cost report");
        Self {
            root,
            checkout,
            checkout_observation: report["checkout"].clone(),
            target_observation: report["target"].clone(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn measurement(
        &self,
        name: &str,
        cold: bool,
        comparison_key: &str,
        elapsed_seconds: f64,
        user_cpu_seconds: f64,
        system_cpu_seconds: f64,
        max_rss_kib: u64,
    ) -> PathBuf {
        let before_target = if cold {
            json!({"schema_version": 1, "state": {"state": "absent"}})
        } else {
            self.target_observation.clone()
        };
        let receipt = json!({
            "schema_version": 6,
            "document_type": "glaeda-hot-run-measurement",
            "authority": "developer_observation_only",
            "comparison_key": comparison_key,
            "cross_worktree": false,
            "resource_profile": null,
            "machine_observation": {},
            "timeout_seconds": 30.0,
            "cache_views": [{"path": "target", "mode": "native"}],
            "state_preparation": [],
            "source_preparation": null,
            "native_target_observation": {
                "authority": "performance_observation_only",
                "atomic": false,
                "before": {
                    "checkout": self.checkout_observation,
                    "cargo_target": before_target,
                },
                "after": {
                    "checkout": {
                        "state": "observed",
                        "observation": self.checkout_observation,
                    },
                    "cargo_target": {
                        "state": "observed",
                        "observation": self.target_observation,
                    },
                },
                "before_elapsed_seconds": 0.001,
                "after_elapsed_seconds": 0.001,
                "observation_elapsed_seconds": 0.002,
                "command_plus_observation_elapsed_seconds": elapsed_seconds + 0.002,
            },
            "runtime": null,
            "elapsed_seconds": elapsed_seconds,
            "preparation_elapsed_seconds": 0.0,
            "command_plus_preparation_elapsed_seconds": elapsed_seconds,
            "user_cpu_seconds": user_cpu_seconds,
            "system_cpu_seconds": system_cpu_seconds,
            "max_rss_kib": max_rss_kib,
            "resource_accounting": "gnu_time_command_tree",
            "exit_code": 0,
            "signal": null,
            "completion_reason": "exited",
        });
        let path = self.root.join(name);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true).mode(0o600);
        serde_json::to_writer(options.open(&path).expect("create receipt"), &receipt)
            .expect("write receipt");
        path
    }

    fn run(&self, cold: &[PathBuf], warm: &[PathBuf]) -> Output {
        let mut command = Command::new(BINARY);
        command
            .arg("--checkout")
            .arg(&self.checkout)
            .arg("--output")
            .arg("json");
        for path in cold {
            command.arg("--cold").arg(path);
        }
        for path in warm {
            command.arg("--warm").arg(path);
        }
        command.output().expect("run value observer")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if self.root.starts_with(std::env::temp_dir()) {
            fs::remove_dir_all(&self.root).expect("remove exact fixture root");
        }
    }
}

fn git(checkout: &Path, arguments: &[&str]) {
    let output = Command::new(GIT)
        .arg("-C")
        .arg(checkout)
        .args(arguments)
        .env_clear()
        .env("HOME", checkout)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("LC_ALL", "C")
        .output()
        .expect("run fixture Git");
    assert!(
        output.status.success(),
        "fixture Git failed with status {:?}",
        output.status.code()
    );
}

#[test]
fn comparable_receipts_report_current_rebuild_value_without_authority() {
    let fixture = Fixture::new();
    let cold = [
        fixture.measurement("cold-a.json", true, COMPARISON_KEY, 10.0, 8.0, 1.0, 20_000),
        fixture.measurement("cold-b.json", true, COMPARISON_KEY, 12.0, 10.0, 1.0, 22_000),
    ];
    let warm = [
        fixture.measurement("warm-a.json", false, COMPARISON_KEY, 2.0, 1.0, 0.5, 18_000),
        fixture.measurement("warm-b.json", false, COMPARISON_KEY, 4.0, 2.0, 0.5, 19_000),
    ];
    let output = fixture.run(&cold, &warm);
    assert!(output.status.success(), "stderr: {:?}", output.stderr);
    assert!(output.stderr.is_empty());
    let report: Value = serde_json::from_slice(&output.stdout).expect("value report");
    assert_eq!(
        report["document_type"],
        "glaeda-cargo-target-value-observation"
    );
    assert_eq!(report["authority"], "performance_observation_only");
    assert_eq!(report["atomic"], false);
    assert_eq!(
        report["currentness"],
        "bracketed_equal_checkout_and_target_snapshots_non_atomic"
    );
    assert_eq!(report["receipt_authenticity"], "unproven_caller_supplied");
    assert_eq!(
        report["successful_use_time"],
        "unknown_schema_v6_has_no_epoch_timestamp"
    );
    assert_eq!(report["retention_authority"], "none");
    assert_eq!(report["mutation_performed"], false);
    assert_eq!(report["cold"]["sample_count"], 2);
    assert_eq!(report["warm"]["sample_count"], 2);
    assert_eq!(report["cold"]["median_elapsed_seconds"], 11.0);
    assert_eq!(report["warm"]["median_elapsed_seconds"], 3.0);
    assert_eq!(report["savings"]["median_elapsed_seconds_saved"], 8.0);
    assert_eq!(
        report["value_disposition"],
        "positive_median_rebuild_savings_observed"
    );
    assert!(report["current_target_allocated_bytes"].as_u64().unwrap() > 0);
    let encoded = String::from_utf8(output.stdout).expect("UTF-8 output");
    assert!(!encoded.contains(fixture.root.to_str().expect("UTF-8 root")));
}

#[test]
fn mixed_comparison_keys_fail_closed() {
    let fixture = Fixture::new();
    let cold = [fixture.measurement("cold.json", true, COMPARISON_KEY, 10.0, 8.0, 1.0, 20_000)];
    let warm = [fixture.measurement(
        "warm.json",
        false,
        "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        2.0,
        1.0,
        0.5,
        18_000,
    )];
    let output = fixture.run(&cold, &warm);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let report: Value = serde_json::from_slice(&output.stderr).expect("error report");
    assert_eq!(report["code"], "cargo_target_value_invalid_measurement");
    assert_eq!(report["problem"], "measurement arms are not comparable");
    assert_eq!(report["mutation_performed"], false);
}

#[test]
fn symlinked_measurement_is_refused_without_path_disclosure() {
    let fixture = Fixture::new();
    let cold = fixture.measurement("cold.json", true, COMPARISON_KEY, 10.0, 8.0, 1.0, 20_000);
    let alias = fixture.root.join("private-alias.json");
    symlink(&cold, &alias).expect("create receipt alias");
    let warm = [fixture.measurement("warm.json", false, COMPARISON_KEY, 2.0, 1.0, 0.5, 18_000)];
    let output = fixture.run(&[alias], &warm);
    assert_eq!(output.status.code(), Some(2));
    let encoded = String::from_utf8(output.stderr).expect("UTF-8 error");
    assert!(encoded.contains("measurement file is unavailable"));
    assert!(!encoded.contains("private-alias"));
    assert_eq!(
        fs::metadata(&cold).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn more_than_thirty_two_samples_are_rejected_before_receipt_reads() {
    let fixture = Fixture::new();
    let missing = fixture.root.join("must-not-be-read.json");
    let cold = vec![missing; 33];
    let warm = [fixture.root.join("also-not-read.json")];
    let output = fixture.run(&cold, &warm);
    assert_eq!(output.status.code(), Some(2));
    let report: Value = serde_json::from_slice(&output.stderr).expect("error report");
    assert_eq!(
        report["problem"],
        "cold sample count must be between 1 and 32"
    );
}

#[test]
fn duplicate_receipt_identity_cannot_inflate_sample_count() {
    let fixture = Fixture::new();
    let cold = fixture.measurement("cold.json", true, COMPARISON_KEY, 10.0, 8.0, 1.0, 20_000);
    let warm = [fixture.measurement("warm.json", false, COMPARISON_KEY, 2.0, 1.0, 0.5, 18_000)];
    let output = fixture.run(&[cold.clone(), cold], &warm);
    assert_eq!(output.status.code(), Some(2));
    let report: Value = serde_json::from_slice(&output.stderr).expect("error report");
    assert_eq!(
        report["problem"],
        "measurement file is duplicated across the sample set"
    );
}

#[test]
fn caller_supplied_runtime_fields_cannot_be_reflected() {
    let fixture = Fixture::new();
    let cold = fixture.measurement("cold.json", true, COMPARISON_KEY, 10.0, 8.0, 1.0, 20_000);
    let mut receipt: Value =
        serde_json::from_slice(&fs::read(&cold).expect("read receipt")).expect("receipt JSON");
    receipt["runtime"] = json!({
        "id": "rust-test",
        "program_sha256": "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
        "private_injected_field": "must-never-be-reflected",
    });
    let mut options = OpenOptions::new();
    options.write(true).truncate(true);
    serde_json::to_writer(options.open(&cold).expect("rewrite receipt"), &receipt)
        .expect("rewrite receipt JSON");
    let warm = [fixture.measurement("warm.json", false, COMPARISON_KEY, 2.0, 1.0, 0.5, 18_000)];
    let output = fixture.run(&[cold], &warm);
    assert_eq!(output.status.code(), Some(2));
    let encoded = String::from_utf8(output.stderr).expect("UTF-8 error");
    assert!(encoded.contains("measurement JSON is invalid"));
    assert!(!encoded.contains("must-never-be-reflected"));
}
