#![cfg(target_os = "linux")]

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

const BINARY: &str = env!("CARGO_BIN_EXE_glaeda-cargo-target-holders");
const COST_BINARY: &str = env!("CARGO_BIN_EXE_glaeda-cargo-target-observe");
const GIT: &str = "/usr/bin/git";
const SLEEP: &str = "/usr/bin/sleep";

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

struct Fixture {
    root: PathBuf,
    checkout: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let sequence = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "glaeda-cargo-target-holders-cli-{}-{sequence}",
            std::process::id()
        ));
        let checkout = root.join("checkout");
        fs::create_dir_all(&checkout).expect("create fixture checkout");
        git(&checkout, &["init", "--quiet", "--initial-branch=main"]);
        git(&checkout, &["config", "user.name", "Glaeda Fixture"]);
        git(
            &checkout,
            &["config", "user.email", "fixture@example.invalid"],
        );
        fs::write(checkout.join("tracked"), "fixture\n").expect("write tracked fixture");
        git(&checkout, &["add", "tracked"]);
        git(&checkout, &["commit", "--quiet", "--message", "fixture"]);
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

    fn target(&self) -> PathBuf {
        let target = self.checkout.join("target");
        fs::create_dir_all(target.join("debug")).expect("create target");
        fs::write(target.join("debug/artifact"), b"artifact").expect("write artifact");
        target
    }

    fn observe(&self) -> Output {
        Command::new(BINARY)
            .args([
                "--checkout",
                self.checkout.to_str().expect("UTF-8 fixture path"),
                "--output",
                "json",
            ])
            .output()
            .expect("run holder observer")
    }

    fn observe_cost(&self) -> Output {
        Command::new(COST_BINARY)
            .args([
                "--checkout",
                self.checkout.to_str().expect("UTF-8 fixture path"),
                "--output",
                "json",
            ])
            .output()
            .expect("run cost observer")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if self.root.starts_with(std::env::temp_dir()) {
            fs::remove_dir_all(&self.root).expect("remove exact fixture root");
        }
    }
}

struct Holder(Child);

impl Drop for Holder {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
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

fn spawn_holder(target: &Path) -> Holder {
    let child = Command::new(SLEEP)
        .arg("30")
        .current_dir(target.join("debug"))
        .stdin(File::open(target.join("debug/artifact")).expect("open held artifact"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .env_clear()
        .spawn()
        .expect("spawn holder");
    let holder = Holder(child);
    let pid = holder.0.id();
    for _ in 0..1_000 {
        if fs::read_link(format!("/proc/{pid}/cwd")).is_ok_and(|cwd| cwd.starts_with(target)) {
            return holder;
        }
        std::thread::yield_now();
    }
    panic!("holder did not enter the fixture target");
}

#[test]
fn physical_cwd_and_open_file_holder_is_reported_without_private_identity() {
    let fixture = Fixture::new();
    let target = fixture.target();
    let _holder = spawn_holder(&target);

    let output = fixture.observe();
    assert!(output.status.success(), "stderr: {:?}", output.stderr);
    assert!(output.stderr.is_empty());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON report");
    let state = &report["holders"]["state"];
    assert_eq!(state["state"], "present");
    assert_eq!(state["disposition"], "holders_observed");
    assert!(state["counts"]["cwd_processes"].as_u64().unwrap() >= 1);
    assert!(state["counts"]["open_fd_processes"].as_u64().unwrap() >= 1);
    assert!(state["counts"]["open_fd_references"].as_u64().unwrap() >= 1);
    assert_eq!(state["coverage"]["universal_absence_proven"], false);
    let encoded = String::from_utf8(output.stdout).expect("UTF-8 JSON");
    assert!(!encoded.contains(fixture.root.to_str().expect("UTF-8 root")));
    assert!(!encoded.contains("artifact"));
}

#[test]
fn holder_and_cost_observers_bind_the_same_target_identity() {
    let fixture = Fixture::new();
    fixture.target();

    let holder_output = fixture.observe();
    let cost_output = fixture.observe_cost();
    assert!(holder_output.status.success());
    assert!(cost_output.status.success());
    let holder: serde_json::Value =
        serde_json::from_slice(&holder_output.stdout).expect("holder JSON");
    let cost: serde_json::Value = serde_json::from_slice(&cost_output.stdout).expect("cost JSON");
    assert_eq!(
        holder["holders"]["state"]["target_id"],
        cost["target"]["state"]["target_id"]
    );
    assert_eq!(holder["zero_means"], "none_observed_not_absence");
    assert_eq!(
        holder["holders"]["state"]["coverage"]["observer_process_excluded"],
        true
    );
    assert_eq!(
        holder["holders"]["state"]["coverage"]["universal_absence_proven"],
        false
    );
}
