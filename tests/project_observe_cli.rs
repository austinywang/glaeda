#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const BINARY: &str = env!("CARGO_BIN_EXE_glaeda-project-observe");
const GIT: &str = "/usr/bin/git";
const FAILURE_STREAM_LIMIT: usize = 4_096;
static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

struct Fixture {
    root: PathBuf,
    checkout: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let sequence = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("current time")
            .as_nanos();
        let temporary_root =
            fs::canonicalize(std::env::temp_dir()).expect("canonicalize test temporary directory");
        let root = temporary_root.join(format!(
            "glaeda-project-observe-cli-{}-{nonce}-{sequence}",
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
            .expect("run project observer")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let temporary_root =
            fs::canonicalize(std::env::temp_dir()).expect("canonicalize test temporary directory");
        if self.root.parent() == Some(temporary_root.as_path()) {
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
    assert_child_success("fixture Git", &output);
}

fn failure_stream(bytes: &[u8]) -> String {
    if bytes.len() <= FAILURE_STREAM_LIMIT {
        return String::from_utf8_lossy(bytes).into_owned();
    }

    let half = FAILURE_STREAM_LIMIT / 2;
    format!(
        "{}\n[… {} bytes omitted …]\n{}",
        String::from_utf8_lossy(&bytes[..half]),
        bytes.len() - FAILURE_STREAM_LIMIT,
        String::from_utf8_lossy(&bytes[bytes.len() - half..])
    )
}

fn assert_child_success(operation: &str, output: &Output) {
    assert!(
        output.status.success(),
        "{operation} failed\nstatus: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        failure_stream(&output.stdout),
        failure_stream(&output.stderr)
    );
}

#[test]
fn json_observation_is_path_private_and_reports_dirty_recovery_state() {
    let fixture = Fixture::new();
    fs::write(fixture.checkout.join("tracked.txt"), "changed\n").expect("change tracked file");
    fs::write(fixture.checkout.join("untracked.txt"), "local\n").expect("write untracked file");

    let output = fixture.observe("json");
    assert_child_success("JSON project observation", &output);
    assert!(output.stderr.is_empty());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON report");
    assert_eq!(report["document_type"], "glaeda-project-observation");
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["authority"], "observation_only");
    assert_eq!(
        report["observation"]["primary_project"],
        "github.com/teamleaderleo/glaeda"
    );
    assert_eq!(report["observation"]["tracked_changes_present"], true);
    assert_eq!(report["observation"]["untracked_entry_count"], 1);
    assert_eq!(report["observation"]["branch"]["state"], "attached");
    assert_eq!(report["observation"]["branch"]["name"], "main");
    let encoded = String::from_utf8(output.stdout).expect("UTF-8 JSON");
    assert!(!encoded.contains(fixture.root.to_str().expect("UTF-8 root")));
    assert!(!encoded.contains("tracked.txt"));
    assert!(!encoded.contains("untracked.txt"));
}

#[test]
fn human_observation_is_derived_from_the_same_typed_state_without_paths() {
    let fixture = Fixture::new();
    let output = fixture.observe("human");
    assert_child_success("human project observation", &output);
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 human output");
    assert!(stdout.contains("authority=observation_only"));
    assert!(stdout.contains("project=github.com/teamleaderleo/glaeda"));
    assert!(stdout.contains("tracked_changes=false untracked_entries=0"));
    assert!(!stdout.contains(fixture.root.to_str().expect("UTF-8 root")));
}

#[test]
fn unsafe_checkout_fails_without_echoing_private_input() {
    let checkout_output = Command::new(BINARY)
        .args(["--checkout", "relative/private", "--output", "json"])
        .output()
        .expect("run relative checkout refusal");
    assert_eq!(checkout_output.status.code(), Some(2));
    assert!(checkout_output.stdout.is_empty());
    let checkout_error = String::from_utf8(checkout_output.stderr).expect("UTF-8 error");
    assert!(checkout_error.contains("\"code\":\"unsafe_path\""));
    assert!(!checkout_error.contains("relative/private"));
}

#[test]
fn git_executable_is_fixed_instead_of_a_caller_selected_command_surface() {
    let help = Command::new(BINARY)
        .arg("--help")
        .output()
        .expect("read project observer help");
    assert_child_success("project observer help", &help);
    assert!(help.stderr.is_empty());
    let stdout = String::from_utf8(help.stdout).expect("UTF-8 help");
    assert!(!stdout.contains("git-program"));
}

#[test]
fn observation_accepts_an_index_larger_than_the_general_output_bound() {
    // Seven bytes of mode output per tracked file: 12,000 files is about 84 KB, which the general
    // 64 KiB bound used to refuse outright for every ordinary large repository.
    let fixture = Fixture::new();
    let bulk = fixture.checkout.join("bulk");
    fs::create_dir(&bulk).expect("create bulk directory");
    for index in 0..12_000 {
        fs::write(bulk.join(format!("{index}.txt")), "x\n").expect("write bulk file");
    }
    git(&fixture.checkout, &["add", "bulk"]);
    git(
        &fixture.checkout,
        &[
            "-c",
            "user.name=Glaeda Test",
            "-c",
            "user.email=glaeda-test@example.invalid",
            "commit",
            "-q",
            "-m",
            "bulk",
        ],
    );

    let output = fixture.observe("json");
    assert_child_success("large-index project observation", &output);
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON report");
    assert_eq!(report["observation"]["tracked_changes_present"], false);
    assert_eq!(report["observation"]["submodules_present"], false);
}
