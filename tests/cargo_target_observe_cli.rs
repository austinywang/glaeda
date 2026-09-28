#![cfg(target_os = "linux")]

use std::fs;
use std::os::unix::fs::{MetadataExt as _, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

const BINARY: &str = env!("CARGO_BIN_EXE_glaeda-cargo-target-observe");
const GIT: &str = "/usr/bin/git";

struct Fixture {
    root: PathBuf,
    checkout: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("current time")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "glaeda-cargo-target-observe-cli-{}-{nonce}",
            std::process::id()
        ));
        let checkout = root.join("checkout");
        fs::create_dir_all(&checkout).expect("create checkout");
        git(&checkout, &["init", "-b", "main"]);
        fs::write(checkout.join("tracked.txt"), "initial\n").expect("write tracked fixture");
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
        Self { root, checkout }
    }

    fn observe(&self, output: &str) -> Output {
        Command::new(BINARY)
            .args([
                "--checkout",
                self.checkout.to_str().expect("UTF-8 fixture path"),
                "--output",
                output,
            ])
            .output()
            .expect("run Cargo target observer")
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
fn absent_target_is_successful_unknown_value_observation() {
    let fixture = Fixture::new();
    let output = fixture.observe("json");
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON report");
    assert_eq!(report["document_type"], "glaeda-cargo-target-observation");
    assert_eq!(report["authority"], "observation_only");
    assert_eq!(report["mutation_performed"], false);
    assert_eq!(report["target"]["state"]["state"], "absent");
    assert_eq!(report["activity_evidence"], "unknown");
    assert_eq!(report["retention_value"], "unknown");
}

#[test]
fn present_target_reports_bounded_path_private_physical_cost() {
    let fixture = Fixture::new();
    let target = fixture.checkout.join("target");
    let nested = target.join("debug");
    fs::create_dir_all(&nested).expect("create target");
    fs::write(target.join(".rustc_info.json"), "{}\n").expect("write rustc marker");
    let artifact = nested.join("private-artifact-name");
    fs::write(&artifact, vec![b'x'; 8192]).expect("write artifact");
    fs::hard_link(&artifact, nested.join("artifact-hardlink")).expect("link artifact");
    symlink("private-artifact-name", nested.join("artifact-symlink")).expect("symlink artifact");
    let artifact_before = fs::metadata(&artifact).expect("artifact metadata before");

    let output = fixture.observe("json");
    assert!(output.status.success(), "stderr: {:?}", output.stderr);
    assert!(output.stderr.is_empty());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON report");
    let observed = &report["target"]["state"];
    assert_eq!(observed["state"], "present");
    assert_eq!(observed["entry_count"], 6);
    assert_eq!(observed["directory_count"], 2);
    assert_eq!(observed["unique_nondirectory_object_count"], 3);
    assert_eq!(
        observed["hardlink_coverage"],
        "complete_within_observed_tree"
    );
    assert_eq!(
        observed["allocation_scope"],
        "visible_filesystem_blocks_not_exclusive"
    );
    assert_eq!(observed["rustc_info"]["state"], "observed");
    assert_eq!(observed["target_owner_matches_checkout"], true);
    assert_eq!(observed["all_entries_match_target_owner"], true);
    assert!(
        observed["target_id"]
            .as_str()
            .is_some_and(|value| value.starts_with("sha256:"))
    );
    let encoded = String::from_utf8(output.stdout).expect("UTF-8 JSON");
    assert!(!encoded.contains(fixture.root.to_str().expect("UTF-8 root")));
    assert!(!encoded.contains("private-artifact-name"));
    // The composed CLI observes the checkout with Git before the descriptor-bound target walk.
    // Git may update the ignored target directory's atime under relatime, so only file-content
    // atime is a stable assertion that the target observer did not read artifact bytes.
    assert_eq!(
        artifact_before.atime(),
        fs::metadata(&artifact).unwrap().atime()
    );
}

#[test]
fn hardlink_outside_target_is_reported_as_ambiguous_savings() {
    let fixture = Fixture::new();
    let target = fixture.checkout.join("target");
    fs::create_dir(&target).expect("create target");
    let outside = fixture.checkout.join("outside-cache-object");
    fs::write(&outside, "shared\n").expect("write outside object");
    fs::hard_link(&outside, target.join("inside-link")).expect("link into target");

    let output = fixture.observe("json");
    assert!(output.status.success(), "stderr: {:?}", output.stderr);
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON report");
    assert_eq!(
        report["target"]["state"]["hardlink_coverage"],
        "external_links_present"
    );
    assert_eq!(report["retention_value"], "unknown");
}

#[test]
fn symlinked_target_fails_without_echoing_private_input() {
    let fixture = Fixture::new();
    let outside = fixture.root.join("private-target-destination");
    fs::create_dir(&outside).expect("create outside target");
    symlink(&outside, fixture.checkout.join("target")).expect("symlink target");

    let output = fixture.observe("json");
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let error: serde_json::Value =
        serde_json::from_slice(&output.stderr).expect("JSON error report");
    assert_eq!(error["code"], "cargo_target_unsafe_shape");
    let encoded = String::from_utf8(output.stderr).expect("UTF-8 error");
    assert!(!encoded.contains(fixture.root.to_str().expect("UTF-8 root")));
    assert!(!encoded.contains("private-target-destination"));
}

#[test]
fn human_output_retains_unknown_authority_boundary_without_paths() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.checkout.join("target")).expect("create target");
    let output = fixture.observe("human");
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 human output");
    assert!(stdout.contains("authority=observation_only"));
    assert!(stdout.contains("retention_value=unknown"));
    assert!(!stdout.contains(fixture.root.to_str().expect("UTF-8 root")));
}
