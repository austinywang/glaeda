#![cfg(unix)]

use std::cell::RefCell;
use std::collections::VecDeque;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use glaeda::artifact::{CommitId, GitTreeId, Sha256Digest};
use glaeda::local_install_plan::{LocalInstallSourceIdentity, LocalInstallToolchainIdentity};
use glaeda::local_install_source_preflight::{
    LocalInstallSourceBlockingCode, observe_local_install_source_preflight,
};
use glaeda::process::{CommandExecutor, CommandSpec, ExecutionRecord, TimedCommandExecutor};
use glaeda::project_checkout_observation::{
    PROJECT_CHECKOUT_COMMAND_TIMEOUT, PROJECT_CHECKOUT_TREE_SCAN_TIMEOUT, ProjectCheckoutObserver,
};
use sha2::{Digest as _, Sha256};

const COMMIT: &str = "1111111111111111111111111111111111111111";
const TREE: &str = "2222222222222222222222222222222222222222";
const CHANGED_COMMIT: &str = "3333333333333333333333333333333333333333";
const LOCK_BYTES: &[u8] = b"# exact Cargo lock\nversion = 4\n";
const SHA256_PREFIX: &str = "sha256:";
const HEX: &[u8; 16] = b"0123456789abcdef";
static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

struct TempCheckout(PathBuf);

impl TempCheckout {
    fn new(label: &str) -> Self {
        let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "smolrunner-source-preflight-acceptance-{label}-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create temporary checkout");
        fs::write(path.join("Cargo.lock"), LOCK_BYTES).expect("write Cargo.lock");
        Self(fs::canonicalize(path).expect("canonical checkout"))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempCheckout {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[derive(Clone)]
struct Response {
    stdout: String,
    stderr: String,
    status: i32,
}

impl Response {
    fn success(stdout: impl Into<String>) -> Self {
        Self {
            stdout: stdout.into(),
            stderr: String::new(),
            status: 0,
        }
    }

    fn failed(status: i32, stderr: impl Into<String>) -> Self {
        Self {
            stdout: String::new(),
            stderr: stderr.into(),
            status,
        }
    }
}

struct ScriptedExecutor {
    responses: RefCell<VecDeque<Response>>,
    commands: RefCell<Vec<CommandSpec>>,
}

impl ScriptedExecutor {
    fn new(responses: Vec<Response>) -> Self {
        Self {
            responses: RefCell::new(responses.into()),
            commands: RefCell::new(Vec::new()),
        }
    }
}

impl CommandExecutor for ScriptedExecutor {
    fn execute(&self, _spec: &CommandSpec) -> io::Result<ExecutionRecord> {
        panic!("source preflight acceptance must use timed Git observation")
    }
}

impl TimedCommandExecutor for ScriptedExecutor {
    fn execute_with_timeout(
        &self,
        spec: &CommandSpec,
        timeout: std::time::Duration,
    ) -> io::Result<ExecutionRecord> {
        let argv = spec.displayed_argv();
        let tree_scan = argv
            .iter()
            .any(|argument| argument == "status" || argument == "ls-files");
        assert_eq!(
            timeout,
            if tree_scan {
                PROJECT_CHECKOUT_TREE_SCAN_TIMEOUT
            } else {
                PROJECT_CHECKOUT_COMMAND_TIMEOUT
            }
        );
        self.commands.borrow_mut().push(spec.clone());
        let response = self
            .responses
            .borrow_mut()
            .pop_front()
            .expect("scripted response");
        Ok(ExecutionRecord {
            argv: spec.displayed_argv(),
            environment_keys: spec.environment.keys().cloned().collect(),
            status: Some(response.status),
            success: response.status == 0,
            stdout: response.stdout,
            stderr: response.stderr,
        })
    }
}

fn digest(bytes: &[u8]) -> Sha256Digest {
    let digest = Sha256::digest(bytes);
    let mut value = String::with_capacity(SHA256_PREFIX.len() + digest.len() * 2);
    value.push_str(SHA256_PREFIX);
    for byte in digest {
        value.push(char::from(HEX[usize::from(byte >> 4)]));
        value.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    Sha256Digest::parse(&value).expect("canonical digest")
}

fn expected() -> LocalInstallSourceIdentity {
    LocalInstallSourceIdentity::new(
        CommitId::parse(COMMIT).expect("commit"),
        GitTreeId::parse(TREE).expect("tree"),
        digest(LOCK_BYTES),
        LocalInstallToolchainIdentity::parse("rust-1.97.1-aarch64-apple-darwin")
            .expect("toolchain"),
    )
    .expect("source identity")
}

fn observer() -> ProjectCheckoutObserver {
    ProjectCheckoutObserver::new("/usr/bin/git").expect("observer")
}

fn snapshot(commit: &str) -> Vec<Response> {
    vec![
        Response::success(format!("{commit}\n")),
        Response::success(format!("{TREE}\n")),
        Response::success("remote.origin.url\nhttps://github.com/teamleaderleo/glaeda.git\0"),
        Response::failed(1, ""),
        Response::success(format!("# branch.oid {commit}\0# branch.head main\0")),
        Response::success("100644\n"),
        Response::success(
            "worktree /private/path\0HEAD 1111111111111111111111111111111111111111\0branch refs/heads/main\0\0",
        ),
    ]
}

fn drift_script(root: &Path) -> Vec<Response> {
    let mut responses = vec![
        Response::success("false\n"),
        Response::success(format!("{}\n", root.display())),
    ];
    responses.extend(snapshot(COMMIT));
    responses.extend(snapshot(CHANGED_COMMIT));
    responses
}

#[test]
fn hard_linked_lockfile_is_unsafe_before_git_observation() {
    let checkout = TempCheckout::new("hard-link");
    fs::hard_link(
        checkout.path().join("Cargo.lock"),
        checkout.path().join("Cargo.lock.alias"),
    )
    .expect("hard link Cargo.lock");
    let executor = ScriptedExecutor::new(Vec::new());

    let receipt = observe_local_install_source_preflight(
        &expected(),
        checkout.path(),
        &observer(),
        &executor,
    );

    assert_eq!(
        receipt.blocking_codes(),
        [LocalInstallSourceBlockingCode::UnsafeSource]
    );
    assert!(executor.commands.borrow().is_empty());
}

#[test]
fn group_writable_or_non_regular_lockfile_is_unsafe_before_git_observation() {
    for case in ["group-writable", "directory"] {
        let checkout = TempCheckout::new(case);
        let lock = checkout.path().join("Cargo.lock");
        if case == "group-writable" {
            fs::set_permissions(&lock, fs::Permissions::from_mode(0o664))
                .expect("make lock group writable");
        } else {
            fs::remove_file(&lock).expect("remove lock");
            fs::create_dir(&lock).expect("replace lock with directory");
        }
        let executor = ScriptedExecutor::new(Vec::new());

        let receipt = observe_local_install_source_preflight(
            &expected(),
            checkout.path(),
            &observer(),
            &executor,
        );

        assert_eq!(
            receipt.blocking_codes(),
            [LocalInstallSourceBlockingCode::UnsafeSource]
        );
        assert!(executor.commands.borrow().is_empty());
    }
}

#[test]
fn git_snapshot_drift_maps_to_one_source_changed_refusal() {
    let checkout = TempCheckout::new("git-drift");
    let executor = ScriptedExecutor::new(drift_script(checkout.path()));

    let receipt = observe_local_install_source_preflight(
        &expected(),
        checkout.path(),
        &observer(),
        &executor,
    );

    assert!(!receipt.ready());
    assert!(!receipt.observation_stable());
    assert_eq!(
        receipt.blocking_codes(),
        [LocalInstallSourceBlockingCode::SourceChanged]
    );
    assert_eq!(executor.commands.borrow().len(), 16);
    let public = serde_json::to_string(&receipt).expect("receipt JSON");
    assert!(!public.contains(checkout.path().to_string_lossy().as_ref()));
    assert!(!public.contains(CHANGED_COMMIT));
}
