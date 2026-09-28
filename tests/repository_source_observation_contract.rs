#![cfg(unix)]
#![allow(dead_code)]

pub use glaeda::{
    artifact, operator_error, personal_worker_queue, process, verification_profile,
    verification_profile_registry,
};

#[path = "../src/repository_source_observation.rs"]
mod repository_source_observation;

use std::cell::RefCell;
use std::collections::VecDeque;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use glaeda::operator_error::OperatorErrorCode;
use glaeda::process::{
    CommandExecutor, CommandSpec, CommandValue, ExecutionRecord, ProcessExecutor,
    TimedCommandExecutor,
};
use glaeda::verification_profile::VerificationProfileId;
use repository_source_observation::{
    REPOSITORY_SOURCE_COMMAND_TIMEOUT, RepositoryCleanliness, RepositorySourceObservationErrorKind,
    RepositorySourceObserver,
};

const COMMIT: &str = "1111111111111111111111111111111111111111";
const TREE: &str = "2222222222222222222222222222222222222222";
const REMOTE: &str = "remote.origin.url\nhttps://github.com/teamleaderleo/smolrunner.git\0";
static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

struct TempCheckout(PathBuf);

impl TempCheckout {
    fn new(label: &str) -> Self {
        let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("repository-source-observation-tests")
            .join(format!("{label}-{}-{sequence}", std::process::id()));
        fs::create_dir_all(&path).expect("create checkout fixture");
        Self(fs::canonicalize(path).expect("canonical checkout fixture"))
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
enum ScriptedResponse {
    Record {
        stdout: String,
        stderr: String,
        status: i32,
        alter_identity: bool,
    },
    Io(io::ErrorKind),
}

impl ScriptedResponse {
    fn success(stdout: impl Into<String>) -> Self {
        Self::Record {
            stdout: stdout.into(),
            stderr: String::new(),
            status: 0,
            alter_identity: false,
        }
    }

    fn failed(status: i32) -> Self {
        Self::Record {
            stdout: String::new(),
            stderr: String::new(),
            status,
            alter_identity: false,
        }
    }
}

struct ScriptedExecutor {
    responses: RefCell<VecDeque<ScriptedResponse>>,
    commands: RefCell<Vec<CommandSpec>>,
}

struct SamePathRebindingExecutor {
    inner: ScriptedExecutor,
    checkout: PathBuf,
    displaced: PathBuf,
}

impl SamePathRebindingExecutor {
    fn new(checkout: &Path, responses: Vec<ScriptedResponse>) -> Self {
        Self {
            inner: ScriptedExecutor::new(responses),
            checkout: checkout.to_path_buf(),
            displaced: checkout.with_extension("displaced"),
        }
    }
}

impl Drop for SamePathRebindingExecutor {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.displaced);
    }
}

impl ScriptedExecutor {
    fn new(responses: Vec<ScriptedResponse>) -> Self {
        Self {
            responses: RefCell::new(responses.into()),
            commands: RefCell::new(Vec::new()),
        }
    }

    fn command_count(&self) -> usize {
        self.commands.borrow().len()
    }
}

impl CommandExecutor for ScriptedExecutor {
    fn execute(&self, _spec: &CommandSpec) -> io::Result<ExecutionRecord> {
        panic!("repository observation must use the timed executor boundary")
    }
}

impl TimedCommandExecutor for ScriptedExecutor {
    fn execute_with_timeout(
        &self,
        spec: &CommandSpec,
        timeout: Duration,
    ) -> io::Result<ExecutionRecord> {
        assert_eq!(timeout, REPOSITORY_SOURCE_COMMAND_TIMEOUT);
        self.commands.borrow_mut().push(spec.clone());
        match self
            .responses
            .borrow_mut()
            .pop_front()
            .expect("scripted Git response")
        {
            ScriptedResponse::Io(kind) => Err(io::Error::new(kind, "private io marker")),
            ScriptedResponse::Record {
                stdout,
                stderr,
                status,
                alter_identity,
            } => {
                let mut argv = spec.displayed_argv();
                if alter_identity {
                    argv.push("unexpected-private-argument".to_owned());
                }
                Ok(ExecutionRecord {
                    argv,
                    environment_keys: spec.environment.keys().cloned().collect(),
                    status: Some(status),
                    success: status == 0,
                    stdout,
                    stderr,
                })
            }
        }
    }
}

impl CommandExecutor for SamePathRebindingExecutor {
    fn execute(&self, _spec: &CommandSpec) -> io::Result<ExecutionRecord> {
        panic!("repository observation must use the timed executor boundary")
    }
}

impl TimedCommandExecutor for SamePathRebindingExecutor {
    fn execute_with_timeout(
        &self,
        spec: &CommandSpec,
        timeout: Duration,
    ) -> io::Result<ExecutionRecord> {
        let record = self.inner.execute_with_timeout(spec, timeout)?;
        if self.inner.command_count() == 14 {
            fs::rename(&self.checkout, &self.displaced).expect("displace observed checkout");
            fs::create_dir(&self.checkout).expect("replace checkout at the same path");
        }
        Ok(record)
    }
}

fn profile() -> VerificationProfileId {
    VerificationProfileId::parse("smolrunner.required").expect("profile")
}

fn observer() -> RepositorySourceObserver {
    RepositorySourceObserver::new("/usr/bin/git").expect("observer")
}

fn snapshot(root: &Path, remote: &str) -> Vec<ScriptedResponse> {
    vec![
        ScriptedResponse::success(format!("{}\n", root.display())),
        ScriptedResponse::success("refs/heads/main\n"),
        ScriptedResponse::success(remote),
        ScriptedResponse::success("100644\n100755\n120000\n"),
        ScriptedResponse::success(format!("{COMMIT}\n")),
        ScriptedResponse::success(format!("{TREE}\n")),
        ScriptedResponse::success(""),
    ]
}

fn clean_script(root: &Path) -> Vec<ScriptedResponse> {
    let mut responses = snapshot(root, REMOTE);
    responses.extend(responses.clone());
    responses
}

#[test]
fn clean_exact_source_uses_two_fixed_credentialless_snapshots() {
    let checkout = TempCheckout::new("clean");
    let executor = ScriptedExecutor::new(clean_script(checkout.path()));

    let observation = observer()
        .observe(checkout.path(), &profile(), &executor)
        .expect("exact source");

    assert_eq!(observation.schema_version(), 1);
    assert_eq!(
        observation.source().repository.as_str(),
        "teamleaderleo/smolrunner"
    );
    assert_eq!(observation.source().commit.as_str(), COMMIT);
    assert_eq!(observation.source().tree.as_str(), TREE);
    assert_eq!(observation.cleanliness(), RepositoryCleanliness::Clean);
    let location_debug = format!("{:?}", observation.workspace_location_identity());
    assert_eq!(location_debug, "<private-workspace-location>");
    assert!(!location_debug.contains(checkout.path().to_string_lossy().as_ref()));
    assert!(
        !serde_json::to_string(&observation)
            .expect("source JSON")
            .contains(checkout.path().to_string_lossy().as_ref())
    );
    assert_eq!(executor.command_count(), 14);

    let commands = executor.commands.borrow();
    let expected_suffixes = [
        ["rev-parse", "--show-toplevel"].as_slice(),
        ["symbolic-ref", "--quiet", "HEAD"].as_slice(),
        ["config", "--no-includes", "--null", "--list"].as_slice(),
        ["ls-files", "--format=%(objectmode)"].as_slice(),
        ["rev-parse", "--verify", "HEAD^{commit}"].as_slice(),
        ["rev-parse", "--verify", "HEAD^{tree}"].as_slice(),
        [
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--ignore-submodules=all",
        ]
        .as_slice(),
    ];
    for (index, command) in commands.iter().enumerate() {
        assert_eq!(command.program, PathBuf::from("/usr/bin/git"));
        assert_eq!(
            command
                .environment
                .iter()
                .map(|(key, value)| {
                    let CommandValue::Plain(value) = value else {
                        panic!("repository observation environment must contain no secrets")
                    };
                    (key.as_str(), value.as_str())
                })
                .collect::<Vec<_>>(),
            vec![
                ("GIT_ALLOW_PROTOCOL", ""),
                ("GIT_ASKPASS", "/bin/false"),
                ("GIT_CONFIG_GLOBAL", "/dev/null"),
                ("GIT_CONFIG_NOSYSTEM", "1"),
                ("GIT_NO_LAZY_FETCH", "1"),
                ("GIT_NO_REPLACE_OBJECTS", "1"),
                ("GIT_PROTOCOL_FROM_USER", "0"),
                ("GIT_TERMINAL_PROMPT", "0"),
                ("LANG", "C"),
                ("LC_ALL", "C")
            ]
        );
        let argv = command.displayed_argv();
        let suffix = expected_suffixes[index % expected_suffixes.len()];
        let mut expected_argv = vec![
            "/usr/bin/git".to_owned(),
            "--no-optional-locks".to_owned(),
            "-c".to_owned(),
            "credential.helper=".to_owned(),
            "-c".to_owned(),
            "core.fsmonitor=false".to_owned(),
            "-c".to_owned(),
            "core.hooksPath=/dev/null".to_owned(),
            "-c".to_owned(),
            "diff.external=".to_owned(),
            "-C".to_owned(),
            checkout.path().to_str().expect("UTF-8 path").to_owned(),
        ];
        expected_argv.extend(suffix.iter().map(|value| (*value).to_owned()));
        assert_eq!(argv, expected_argv);
    }

    let encoded = serde_json::to_string(&observation).expect("public JSON");
    let debug = format!("{observation:?} {:?}", observer());
    assert!(!encoded.contains(checkout.path().to_str().expect("UTF-8 path")));
    assert!(!debug.contains(checkout.path().to_str().expect("UTF-8 path")));
}

#[test]
fn identical_source_from_different_workspace_roots_retains_distinct_private_identity() {
    let first = TempCheckout::new("location-first");
    let second = TempCheckout::new("location-second");
    let first_executor = ScriptedExecutor::new(clean_script(first.path()));
    let second_executor = ScriptedExecutor::new(clean_script(second.path()));

    let first_observation = observer()
        .observe(first.path(), &profile(), &first_executor)
        .expect("first source");
    let second_observation = observer()
        .observe(second.path(), &profile(), &second_executor)
        .expect("second source");

    assert_eq!(first_observation.source(), second_observation.source());
    assert_ne!(
        first_observation.workspace_location_identity(),
        second_observation.workspace_location_identity()
    );
    let public = format!("{first_observation:?}{second_observation:?}");
    assert!(!public.contains(first.path().to_string_lossy().as_ref()));
    assert!(!public.contains(second.path().to_string_lossy().as_ref()));
}

#[test]
fn same_path_directory_rebind_after_git_snapshots_is_source_drift() {
    let checkout = TempCheckout::new("same-path-rebind");
    let executor = SamePathRebindingExecutor::new(checkout.path(), clean_script(checkout.path()));

    let error = observer()
        .observe(checkout.path(), &profile(), &executor)
        .expect_err("a replacement directory at the same path must not retain source authority");

    assert_eq!(
        error.kind(),
        RepositorySourceObservationErrorKind::RepositorySourceChanged
    );
    assert_eq!(executor.inner.command_count(), 14);
}

#[test]
fn production_executor_observes_a_disposable_local_git_fixture() {
    let git = Path::new("/usr/bin/git");
    if !git.is_file() {
        return;
    }
    let checkout = TempCheckout::new("production");
    run_fixture_git(checkout.path(), &["init", "-b", "main"]);
    run_fixture_git(checkout.path(), &["config", "user.name", "SmolRunner Test"]);
    run_fixture_git(
        checkout.path(),
        &["config", "user.email", "smolrunner@example.invalid"],
    );
    run_fixture_git(
        checkout.path(),
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/teamleaderleo/smolrunner.git",
        ],
    );
    fs::write(
        checkout.path().join(".gitattributes"),
        b"fixture.txt filter=hidden\n",
    )
    .expect("write attributes fixture");
    fs::write(checkout.path().join("fixture.txt"), b"exact fixture\n").expect("write fixture");
    run_fixture_git(checkout.path(), &["add", ".gitattributes", "fixture.txt"]);
    run_fixture_git(checkout.path(), &["commit", "-m", "fixture"]);

    let observation = observer()
        .observe(checkout.path(), &profile(), &ProcessExecutor)
        .expect("production Git observation");

    assert_eq!(
        observation.source().repository.as_str(),
        "teamleaderleo/smolrunner"
    );
    assert_eq!(observation.source().commit.as_str().len(), 40);
    assert_eq!(observation.source().tree.as_str().len(), 40);

    for rewrite_key in [
        "url.file:///private/repository/.insteadOf",
        "url.file:///private/repository/.pushInsteadOf",
    ] {
        run_fixture_git(
            checkout.path(),
            &["config", rewrite_key, "https://github.com/"],
        );
        let error = observer()
            .observe(checkout.path(), &profile(), &ProcessExecutor)
            .expect_err("effective URL rewrites must fail before object resolution");
        assert_eq!(
            error.kind(),
            RepositorySourceObservationErrorKind::RepositoryUnavailable
        );
        run_fixture_git(checkout.path(), &["config", "--unset-all", rewrite_key]);
    }

    let marker = checkout.path().join("filter-command-ran");
    let filter_command = format!("/usr/bin/touch {}", marker.display());
    run_fixture_git(
        checkout.path(),
        &["config", "extensions.worktreeConfig", "true"],
    );
    run_fixture_git(
        checkout.path(),
        &[
            "config",
            "--worktree",
            "filter.hidden.clean",
            &filter_command,
        ],
    );

    let error = observer()
        .observe(checkout.path(), &profile(), &ProcessExecutor)
        .expect_err("per-worktree filter configuration must fail before status");
    assert_eq!(
        error.kind(),
        RepositorySourceObservationErrorKind::RepositoryUnavailable
    );
    assert!(!marker.exists(), "Git filter command must never execute");
}

#[test]
fn initialized_submodule_filters_are_never_recursively_executed() {
    let git = Path::new("/usr/bin/git");
    if !git.is_file() {
        return;
    }
    let child = TempCheckout::new("submodule-child");
    run_fixture_git(child.path(), &["init", "-b", "main"]);
    run_fixture_git(child.path(), &["config", "user.name", "SmolRunner Test"]);
    run_fixture_git(
        child.path(),
        &["config", "user.email", "smolrunner@example.invalid"],
    );
    fs::write(
        child.path().join(".gitattributes"),
        b"*.txt filter=hidden\n",
    )
    .expect("write submodule attributes");
    fs::write(
        child.path().join("nested.txt"),
        b"committed nested source\n",
    )
    .expect("write nested source");
    run_fixture_git(child.path(), &["add", ".gitattributes", "nested.txt"]);
    run_fixture_git(child.path(), &["commit", "-m", "nested fixture"]);

    let checkout = TempCheckout::new("submodule-parent");
    run_fixture_git(checkout.path(), &["init", "-b", "main"]);
    run_fixture_git(checkout.path(), &["config", "user.name", "SmolRunner Test"]);
    run_fixture_git(
        checkout.path(),
        &["config", "user.email", "smolrunner@example.invalid"],
    );
    run_fixture_git(
        checkout.path(),
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/teamleaderleo/smolrunner.git",
        ],
    );
    run_fixture_git(
        checkout.path(),
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            child.path().to_str().expect("UTF-8 child path"),
            "nested",
        ],
    );
    run_fixture_git(checkout.path(), &["commit", "-am", "parent fixture"]);

    let nested = checkout.path().join("nested");
    let marker = checkout.path().join("nested-filter-command-ran");
    let filter_command = format!("/usr/bin/touch {}", marker.display());
    run_fixture_git(&nested, &["config", "extensions.worktreeConfig", "true"]);
    run_fixture_git(
        &nested,
        &[
            "config",
            "--worktree",
            "filter.hidden.clean",
            &filter_command,
        ],
    );
    fs::write(nested.join("nested.txt"), b"modified nested source\n")
        .expect("modify nested worktree");

    let error = observer()
        .observe(checkout.path(), &profile(), &ProcessExecutor)
        .expect_err("gitlinks are refused before recursive worktree inspection");
    assert_eq!(
        error.kind(),
        RepositorySourceObservationErrorKind::RepositoryUnavailable
    );
    assert!(
        !marker.exists(),
        "nested Git filter command must never execute"
    );
}

fn run_fixture_git(checkout: &Path, arguments: &[&str]) {
    let output = Command::new("/usr/bin/git")
        .args(arguments)
        .current_dir(checkout)
        .env_clear()
        .env("LC_ALL", "C")
        .output()
        .expect("run fixture Git");
    assert!(
        output.status.success(),
        "fixture Git failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn canonical_remote_forms_collapse_but_ambiguity_and_disallowed_urls_fail() {
    let checkout = TempCheckout::new("remotes");
    let https_remote = ScriptedExecutor::new(clean_script(checkout.path()));
    assert!(
        observer()
            .observe(checkout.path(), &profile(), &https_remote)
            .is_ok()
    );

    let three = concat!(
        "remote.origin.url\nhttps://github.com/TeamLeaderLeo/SmolRunner.git\0",
        "remote.upstream.url\ngit@github.com:teamleaderleo/smolrunner.git\0",
        "remote.mirror.url\nssh://git@github.com/teamleaderleo/smolrunner\0"
    );
    let mut responses = snapshot(checkout.path(), three);
    responses.extend(snapshot(checkout.path(), three));
    let accepted = ScriptedExecutor::new(responses);
    assert!(
        observer()
            .observe(checkout.path(), &profile(), &accepted)
            .is_ok()
    );

    for remote in [
        "remote.origin.url\nhttps://github.com/teamleaderleo/smolrunner.git\0remote.other.url\nhttps://github.com/example/other.git\0",
        "remote.origin.url\nhttps://token@github.com/teamleaderleo/smolrunner.git\0",
        "remote.origin.url\nfile:///private/repository\0",
        "remote.origin.url\nhttps://GitHub.com/teamleaderleo/smolrunner.git\0",
    ] {
        let mut responses = snapshot(checkout.path(), remote);
        responses.extend(snapshot(checkout.path(), remote));
        let result = observer().observe(
            checkout.path(),
            &profile(),
            &ScriptedExecutor::new(responses),
        );
        let error = result.expect_err("remote must fail closed");
        assert_eq!(
            error.kind(),
            RepositorySourceObservationErrorKind::RepositoryIdentityMismatch
        );
    }
}

#[test]
fn profile_detached_unborn_dirty_and_identity_refusals_are_exact() {
    let checkout = TempCheckout::new("refusals");
    let unknown = VerificationProfileId::parse("unknown.profile").expect("unknown profile");
    let no_commands = ScriptedExecutor::new(Vec::new());
    let error = observer()
        .observe(checkout.path(), &unknown, &no_commands)
        .expect_err("unknown profile");
    assert_eq!(
        error.public_error().code(),
        OperatorErrorCode::VerificationProfileUnavailable
    );
    assert_eq!(no_commands.command_count(), 0);

    let cases = [
        (
            1,
            ScriptedResponse::failed(1),
            RepositorySourceObservationErrorKind::RepositoryUnavailable,
        ),
        (
            4,
            ScriptedResponse::failed(128),
            RepositorySourceObservationErrorKind::RepositoryUnavailable,
        ),
        (
            4,
            ScriptedResponse::success(format!("{}\n", "A".repeat(40))),
            RepositorySourceObservationErrorKind::RepositoryUnavailable,
        ),
        (
            5,
            ScriptedResponse::success("2222222\n"),
            RepositorySourceObservationErrorKind::RepositoryUnavailable,
        ),
        (
            3,
            ScriptedResponse::success("100644\n160000\n"),
            RepositorySourceObservationErrorKind::RepositoryUnavailable,
        ),
        (
            2,
            ScriptedResponse::success("user.name\nSmolRunner Test\0"),
            RepositorySourceObservationErrorKind::RepositoryIdentityMismatch,
        ),
        (
            2,
            ScriptedResponse::success("remote.origin.url\nhttps://github.com/example/other.git\0"),
            RepositorySourceObservationErrorKind::RepositoryIdentityMismatch,
        ),
        (
            6,
            ScriptedResponse::success("?? private-file\0"),
            RepositorySourceObservationErrorKind::RepositoryDirty,
        ),
    ];
    for (index, response, expected) in cases {
        let mut responses = clean_script(checkout.path());
        responses[index] = response;
        let error = observer()
            .observe(
                checkout.path(),
                &profile(),
                &ScriptedExecutor::new(responses),
            )
            .expect_err("source refusal");
        assert_eq!(error.kind(), expected);
    }

    for (remote, commit) in [
        (
            "user.name\nSmolRunner Test\0",
            ScriptedResponse::failed(128),
        ),
        (
            "remote.origin.url\nfile:///private/repository\0",
            ScriptedResponse::success(format!("{}\n", "A".repeat(40))),
        ),
    ] {
        let mut responses = clean_script(checkout.path());
        responses[2] = ScriptedResponse::success(remote);
        responses[4] = commit;
        let error = observer()
            .observe(
                checkout.path(),
                &profile(),
                &ScriptedExecutor::new(responses),
            )
            .expect_err("repository availability must precede remote identity");
        assert_eq!(
            error.kind(),
            RepositorySourceObservationErrorKind::RepositoryUnavailable
        );
    }

    for unsafe_config in [
        concat!(
            "remote.origin.url\nhttps://github.com/teamleaderleo/smolrunner.git\0",
            "filter.danger.clean\n/usr/bin/touch /private/filter-marker\0"
        ),
        concat!(
            "remote.origin.url\nhttps://github.com/teamleaderleo/smolrunner.git\0",
            "include.path\n/private/included-config\0"
        ),
        concat!(
            "remote.origin.url\nhttps://github.com/teamleaderleo/smolrunner.git\0",
            "url.file:///private/repository/.insteadOf\nhttps://github.com/\0"
        ),
        concat!(
            "remote.origin.url\nhttps://github.com/teamleaderleo/smolrunner.git\0",
            "url.file:///private/repository/.pushInsteadOf\nhttps://github.com/\0"
        ),
    ] {
        let mut responses = clean_script(checkout.path());
        responses[2] = ScriptedResponse::success(unsafe_config);
        let executor = ScriptedExecutor::new(responses);
        let error = observer()
            .observe(checkout.path(), &profile(), &executor)
            .expect_err("unsafe local configuration");
        assert_eq!(
            error.kind(),
            RepositorySourceObservationErrorKind::RepositoryUnavailable
        );
        assert_eq!(executor.command_count(), 3);
    }
}

#[test]
fn every_second_snapshot_identity_or_cleanliness_change_is_source_drift() {
    let checkout = TempCheckout::new("drift");
    let changes = [
        (7, ScriptedResponse::success("/different/absolute/root\n")),
        (8, ScriptedResponse::success("refs/heads/other\n")),
        (
            11,
            ScriptedResponse::success(format!("{}\n", "3".repeat(40))),
        ),
        (
            12,
            ScriptedResponse::success(format!("{}\n", "4".repeat(40))),
        ),
        (
            9,
            ScriptedResponse::success("remote.origin.url\nhttps://github.com/example/other.git\0"),
        ),
        (8, ScriptedResponse::success("HEAD\n")),
        (10, ScriptedResponse::success("100644\n160000\n")),
        (
            11,
            ScriptedResponse::success(format!("{}\n", "A".repeat(40))),
        ),
        (
            9,
            ScriptedResponse::success("remote.origin.url\nfile:///private/repository\0"),
        ),
        (13, ScriptedResponse::success(" M private-file\0")),
    ];
    for (index, response) in changes {
        let mut responses = clean_script(checkout.path());
        responses[index] = response;
        let error = observer()
            .observe(
                checkout.path(),
                &profile(),
                &ScriptedExecutor::new(responses),
            )
            .expect_err("snapshot drift");
        assert_eq!(
            error.kind(),
            RepositorySourceObservationErrorKind::RepositorySourceChanged
        );
    }

    let mut responses = clean_script(checkout.path());
    responses[9] = ScriptedResponse::success(
        "remote.origin.url\ngit@github.com:teamleaderleo/smolrunner.git\0",
    );
    let error = observer()
        .observe(
            checkout.path(),
            &profile(),
            &ScriptedExecutor::new(responses),
        )
        .expect_err("exact remote binding drift");
    assert_eq!(
        error.kind(),
        RepositorySourceObservationErrorKind::RepositorySourceChanged
    );
}

#[test]
fn process_failures_identity_drift_and_private_evidence_map_to_bounded_errors() {
    let checkout = TempCheckout::new("process-errors");
    let private = checkout.path().to_str().expect("UTF-8 path");
    let cases = [
        ScriptedResponse::Io(io::ErrorKind::TimedOut),
        ScriptedResponse::Record {
            stdout: String::new(),
            stderr: "private stderr marker".to_owned(),
            status: 0,
            alter_identity: false,
        },
        ScriptedResponse::Record {
            stdout: String::new(),
            stderr: String::new(),
            status: 0,
            alter_identity: true,
        },
        ScriptedResponse::success("x".repeat(65_537)),
    ];
    for response in cases {
        let mut responses = clean_script(checkout.path());
        responses[0] = response;
        let error = observer()
            .observe(
                checkout.path(),
                &profile(),
                &ScriptedExecutor::new(responses),
            )
            .expect_err("process failure");
        assert_eq!(
            error.kind(),
            RepositorySourceObservationErrorKind::RepositoryUnavailable
        );
        let public = serde_json::to_string(&error).expect("public error");
        let debug = format!("{error:?}");
        assert!(!public.contains(private));
        assert!(!public.contains("private stderr marker"));
        assert!(!debug.contains(private));
        assert!(!debug.contains("private stderr marker"));
    }
}

#[test]
fn module_contains_only_the_reviewed_observation_authority() {
    let source = include_str!("../src/repository_source_observation.rs");
    for forbidden in [
        "std::process::Command",
        "Command::new",
        "std::env::var",
        "std::env::vars",
        "SystemTime::now",
        "TcpStream",
        "reqwest",
        "octocrab",
        "limactl",
        "podman",
        "queue submit",
        "store.create",
    ] {
        assert!(
            !source.contains(forbidden),
            "unexpected authority token {forbidden}"
        );
    }
}
