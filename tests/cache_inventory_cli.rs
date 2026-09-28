use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(target_os = "linux")]
use std::os::unix::fs::MetadataExt as _;

use serde_json::Value;

static NEXT_INVENTORY: AtomicU64 = AtomicU64::new(1);

struct TempInventory(PathBuf);

impl TempInventory {
    fn new(bytes: &[u8]) -> Self {
        let sequence = NEXT_INVENTORY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "glaeda-cache-inventory-cli-{}-{sequence}.json",
            std::process::id()
        ));
        fs::write(&path, bytes).expect("write temporary inventory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

#[cfg(target_os = "linux")]
struct TempHotRunRoot(PathBuf);

#[cfg(target_os = "linux")]
impl TempHotRunRoot {
    fn new() -> Self {
        let sequence = NEXT_INVENTORY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "glaeda-hot-run-cache-cli-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create temporary hot-run root");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

#[cfg(target_os = "linux")]
impl Drop for TempHotRunRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

impl Drop for TempInventory {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn inventory() -> TempInventory {
    TempInventory::new(
        br#"{
  "schema_version": 1,
  "states": [
    {
      "state_id": "owned-retired-one",
      "ownership": "exact_glaeda_owned",
      "generation": "retired",
      "worktree": "removed",
      "reconstruction": "proven",
      "logical_bytes": 100,
      "allocated_bytes": 80,
      "active_lease": false,
      "active_lock": false,
      "mounted": false,
      "open_file_count": 0,
      "live_owned_process_count": 0,
      "interrupted_cleanup": false,
      "quarantined": false
    },
    {
      "state_id": "unmanaged-cargo-target",
      "ownership": "unmanaged",
      "generation": "unknown",
      "worktree": "present",
      "reconstruction": "unproven",
      "logical_bytes": 200,
      "allocated_bytes": 160,
      "active_lease": null,
      "active_lock": null,
      "mounted": false,
      "open_file_count": null,
      "live_owned_process_count": null,
      "interrupted_cleanup": null,
      "quarantined": false
    }
  ]
}"#,
    )
}

fn run(arguments: &[&OsStr]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_glaeda"))
        .args(arguments)
        .output()
        .expect("run glaeda")
}

fn json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).expect("parse JSON command output")
}

#[test]
fn status_and_reclaim_dry_run_share_the_safe_typed_report() {
    let inventory = inventory();
    let status = run(&[
        OsStr::new("--output"),
        OsStr::new("json"),
        OsStr::new("cache"),
        OsStr::new("status"),
        OsStr::new("--inventory"),
        inventory.path().as_os_str(),
    ]);
    assert!(
        status.status.success(),
        "status stderr: {:?}",
        status.stderr
    );
    let status_json = json(&status);
    assert_eq!(status_json["authority"], "supplied_observation_only");
    assert_eq!(status_json["operation"], "status");
    assert_eq!(status_json["mutation_performed"], false);
    assert_eq!(status_json["summary"]["state_count"], 2);
    assert_eq!(status_json["summary"]["reclaimable_count"], 1);
    assert_eq!(status_json["summary"]["unknown_count"], 1);

    let reclaim = run(&[
        OsStr::new("--output"),
        OsStr::new("json"),
        OsStr::new("cache"),
        OsStr::new("reclaim"),
        OsStr::new("--dry-run"),
        OsStr::new("--inventory"),
        inventory.path().as_os_str(),
    ]);
    assert!(
        reclaim.status.success(),
        "reclaim stderr: {:?}",
        reclaim.stderr
    );
    let reclaim_json = json(&reclaim);
    assert_eq!(reclaim_json["operation"], "reclaim_dry_run");
    assert_eq!(reclaim_json["mutation_performed"], false);
    assert_eq!(reclaim_json["states"], status_json["states"]);
}

#[test]
fn explain_selects_one_exact_opaque_identity() {
    let inventory = inventory();
    let output = run(&[
        OsStr::new("--output"),
        OsStr::new("json"),
        OsStr::new("cache"),
        OsStr::new("explain"),
        OsStr::new("unmanaged-cargo-target"),
        OsStr::new("--inventory"),
        inventory.path().as_os_str(),
    ]);
    assert!(
        output.status.success(),
        "explain stderr: {:?}",
        output.stderr
    );
    let report = json(&output);
    assert_eq!(report["operation"], "explain");
    assert_eq!(report["states"].as_array().expect("states").len(), 1);
    assert_eq!(report["states"][0]["classification"], "unknown");
    assert!(
        report["states"][0]["reasons"]
            .as_array()
            .expect("reasons")
            .contains(&Value::String("unmanaged".to_owned()))
    );
}

#[test]
fn malformed_input_emits_bounded_path_free_error() {
    let inventory =
        TempInventory::new(br#"{"schema_version":1,"states":[],"private_path":"/do/not/echo"}"#);
    let output = run(&[
        OsStr::new("--output"),
        OsStr::new("json"),
        OsStr::new("cache"),
        OsStr::new("status"),
        OsStr::new("--inventory"),
        inventory.path().as_os_str(),
    ]);
    assert_eq!(output.status.code(), Some(2));
    let error = json(&output);
    assert_eq!(error["kind"], "cache_inventory_invalid_document");
    let rendered = String::from_utf8(output.stdout).expect("UTF-8 output");
    assert!(!rendered.contains("/do/not/echo"));
    assert!(!rendered.contains(inventory.path().to_string_lossy().as_ref()));
}

#[cfg(target_os = "linux")]
#[test]
fn hot_run_observation_is_path_free_unknown_and_non_mutating() {
    let root = TempHotRunRoot::new();
    let state = root.path().join("a".repeat(64));
    fs::create_dir(&state).expect("create state");
    let private_name = "private-project-output-do-not-print";
    fs::write(state.join(private_name), b"abc").expect("write state data");
    let before_root = fs::metadata(root.path()).expect("observe root before");
    let before_state = fs::metadata(&state).expect("observe state before");
    let data = fs::metadata(state.join(private_name)).expect("observe data");

    let output = run(&[
        OsStr::new("--output"),
        OsStr::new("json"),
        OsStr::new("cache"),
        OsStr::new("observe-hot-run"),
        OsStr::new("--root"),
        root.path().as_os_str(),
    ]);
    assert!(
        output.status.success(),
        "observation stderr: {:?}",
        output.stderr
    );
    let report = json(&output);
    assert_eq!(report["schema_version"], 2);
    assert_eq!(report["authority"], "local_hot_run_filesystem_observation");
    assert_eq!(report["mutation_performed"], false);
    assert_eq!(report["completeness"], "complete");
    assert_eq!(report["summary"]["state_count"], 1);
    assert_eq!(report["summary"]["unknown_count"], 1);
    assert_eq!(report["summary"]["reclaimable_count"], 0);
    assert_eq!(report["summary"]["logical_bytes"], data.size());
    assert_eq!(
        report["summary"]["allocated_bytes"],
        (before_state.blocks() + data.blocks()) * 512
    );
    assert_eq!(report["states"][0]["classification"], "unknown");
    assert!(
        report["states"][0]["reasons"]
            .as_array()
            .expect("reasons")
            .contains(&Value::String("ownership_unknown".to_owned()))
    );

    let rendered = String::from_utf8(output.stdout).expect("UTF-8 output");
    assert!(!rendered.contains(root.path().to_string_lossy().as_ref()));
    assert!(!rendered.contains(private_name));
    let after_root = fs::metadata(root.path()).expect("observe root after");
    let after_state = fs::metadata(&state).expect("observe state after");
    assert_eq!(before_root.atime(), after_root.atime());
    assert_eq!(before_root.mtime(), after_root.mtime());
    assert_eq!(before_root.ctime(), after_root.ctime());
    assert_eq!(before_state.atime(), after_state.atime());
    assert_eq!(before_state.mtime(), after_state.mtime());
    assert_eq!(before_state.ctime(), after_state.ctime());
}

#[cfg(target_os = "linux")]
#[test]
fn partial_hot_run_observation_keeps_bytes_and_classifier_unknown() {
    let root = TempHotRunRoot::new();
    let state = root.path().join("a".repeat(64));
    fs::create_dir(&state).expect("create state");
    let private_name = "private-socket-name-do-not-print";
    let status = Command::new("/usr/bin/mkfifo")
        .arg(state.join(private_name))
        .status()
        .expect("run mkfifo");
    assert!(status.success(), "create fixture FIFO");

    let output = run(&[
        OsStr::new("--output"),
        OsStr::new("json"),
        OsStr::new("cache"),
        OsStr::new("observe-hot-run"),
        OsStr::new("--root"),
        root.path().as_os_str(),
    ]);
    assert!(
        output.status.success(),
        "partial observation stderr: {:?}",
        output.stderr
    );
    let report = json(&output);
    assert_eq!(report["schema_version"], 2);
    assert_eq!(report["authority"], "local_hot_run_filesystem_observation");
    assert_eq!(report["mutation_performed"], false);
    assert_eq!(report["completeness"], "partial");
    assert_eq!(report["summary"]["state_count"], 1);
    assert_eq!(report["summary"]["logical_bytes"], Value::Null);
    assert_eq!(report["summary"]["allocated_bytes"], Value::Null);
    assert_eq!(report["summary"]["reclaimable_count"], Value::Null);
    assert_eq!(
        report["summary"]["reclaimable_allocated_bytes"],
        Value::Null
    );
    assert_eq!(report["states"], Value::Array(Vec::new()));
    assert_eq!(report["problems"], serde_json::json!(["unsupported_node"]));

    let rendered = String::from_utf8(output.stdout).expect("UTF-8 output");
    assert!(!rendered.contains(root.path().to_string_lossy().as_ref()));
    assert!(!rendered.contains(private_name));

    let human = run(&[
        OsStr::new("cache"),
        OsStr::new("observe-hot-run"),
        OsStr::new("--root"),
        root.path().as_os_str(),
    ]);
    assert!(human.status.success(), "human partial observation");
    let human = String::from_utf8(human.stdout).expect("UTF-8 human output");
    assert!(human.contains("completeness: partial"));
    assert!(human.contains("reclaimable=unknown"));
    assert!(human.contains("problems: unsupported_node"));
    assert!(!human.contains(root.path().to_string_lossy().as_ref()));
    assert!(!human.contains(private_name));
}
