use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use rustix::process;

use super::{
    DescriptorBoundLaunchError, DescriptorBoundLaunchErrorKind, DescriptorBoundTermination,
    LaunchHooks, ReviewedFilesystemIdentity, ReviewedLaunchCredentials, ReviewedLaunchValue,
    ReviewedLinuxLaunchPlan, execute_reviewed_linux_launch, execute_with_hooks,
};

static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(1);

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "smolrunner-descriptor-launch-{name}-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("create fixture root");
        Self { root }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    fn directory(&self, name: &str) -> PathBuf {
        let path = self.path(name);
        fs::create_dir(&path).expect("create fixture directory");
        path
    }

    fn copy_binary(&self, name: &str, candidates: &[&str]) -> PathBuf {
        let source = candidates
            .iter()
            .map(Path::new)
            .find(|candidate| candidate.is_file())
            .unwrap_or_else(|| panic!("no reviewed fixture binary for {name}"));
        let destination = self.path(name);
        // Keep the writable destination descriptor out of this multithreaded test process.
        // Otherwise a concurrent fork can briefly inherit it and make an immediate exec of the
        // copied inode fail with ETXTBSY even though `fs::copy` has returned in this thread.
        let status = Command::new("/usr/bin/cp")
            .arg("--")
            .arg(source)
            .arg(&destination)
            .status()
            .expect("spawn fixture copy");
        assert!(status.success(), "copy fixture binary");
        fs::set_permissions(&destination, fs::Permissions::from_mode(0o755))
            .expect("set fixture executable mode");
        destination
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn identity(path: &Path) -> ReviewedFilesystemIdentity {
    let metadata = fs::metadata(path).expect("fixture metadata");
    ReviewedFilesystemIdentity::new(
        metadata.dev(),
        metadata.ino(),
        metadata.uid(),
        metadata.gid(),
        metadata.mode() & 0o7777,
    )
    .expect("reviewed identity")
}

fn current_credentials() -> ReviewedLaunchCredentials {
    ReviewedLaunchCredentials::Inherit {
        uid: process::geteuid().as_raw(),
        gid: process::getegid().as_raw(),
    }
}

fn plan(
    command_id: &str,
    executable: &Path,
    working_directory: &Path,
    arguments: Vec<ReviewedLaunchValue>,
    environment: BTreeMap<String, ReviewedLaunchValue>,
) -> ReviewedLinuxLaunchPlan {
    ReviewedLinuxLaunchPlan::new(
        command_id,
        executable,
        identity(executable),
        working_directory,
        identity(working_directory),
        arguments,
        environment,
        current_credentials(),
    )
    .expect("reviewed launch plan")
}

struct ExecutableAbaHooks {
    target: PathBuf,
    original_hidden: PathBuf,
    replacement: PathBuf,
    replacement_hidden: PathBuf,
}

impl LaunchHooks for ExecutableAbaHooks {
    fn after_descriptors_opened(&self) -> Result<(), DescriptorBoundLaunchError> {
        fs::rename(&self.target, &self.original_hidden).expect("hide reviewed executable");
        fs::rename(&self.replacement, &self.target).expect("install replacement executable");
        Ok(())
    }

    fn after_spawn(&self) -> Result<(), DescriptorBoundLaunchError> {
        fs::rename(&self.target, &self.replacement_hidden).expect("hide replacement executable");
        fs::rename(&self.original_hidden, &self.target).expect("restore reviewed executable");
        Ok(())
    }
}

struct WorkingDirectoryAbaHooks {
    target: PathBuf,
    original_hidden: PathBuf,
    replacement: PathBuf,
    replacement_hidden: PathBuf,
}

impl LaunchHooks for WorkingDirectoryAbaHooks {
    fn after_descriptors_opened(&self) -> Result<(), DescriptorBoundLaunchError> {
        fs::rename(&self.target, &self.original_hidden).expect("hide reviewed cwd");
        fs::rename(&self.replacement, &self.target).expect("install replacement cwd");
        Ok(())
    }

    fn after_spawn(&self) -> Result<(), DescriptorBoundLaunchError> {
        fs::rename(&self.target, &self.replacement_hidden).expect("hide replacement cwd");
        fs::rename(&self.original_hidden, &self.target).expect("restore reviewed cwd");
        Ok(())
    }
}

#[test]
fn executable_aba_runs_the_held_reviewed_object() {
    let fixture = Fixture::new("executable-aba");
    let working_directory = fixture.directory("cwd");
    let executable = fixture.copy_binary("reviewed", &["/usr/bin/true", "/bin/true"]);
    let replacement = fixture.copy_binary("replacement", &["/usr/bin/false", "/bin/false"]);
    let expected_identity = identity(&executable);
    let launch = plan(
        "descriptor.executable_aba",
        &executable,
        &working_directory,
        Vec::new(),
        BTreeMap::new(),
    );
    let hooks = ExecutableAbaHooks {
        target: executable.clone(),
        original_hidden: fixture.path("reviewed-original"),
        replacement,
        replacement_hidden: fixture.path("replacement-hidden"),
    };

    let receipt = execute_with_hooks(&launch, &hooks).expect("held executable must run");

    assert!(receipt.success());
    assert_eq!(
        receipt.termination(),
        DescriptorBoundTermination::Exited { code: 0 }
    );
    assert_eq!(identity(&executable), expected_identity);
}

#[test]
fn working_directory_aba_enters_the_held_reviewed_directory() {
    let fixture = Fixture::new("cwd-aba");
    let working_directory = fixture.directory("cwd");
    let replacement = fixture.directory("replacement-cwd");
    let executable = fixture.copy_binary("stat", &["/usr/bin/stat", "/bin/stat"]);
    let expected_inode = fs::metadata(&working_directory)
        .expect("reviewed cwd metadata")
        .ino();
    let launch = plan(
        "descriptor.cwd_aba",
        &executable,
        &working_directory,
        vec![
            ReviewedLaunchValue::plain("-c"),
            ReviewedLaunchValue::plain("%i"),
            ReviewedLaunchValue::plain("."),
        ],
        BTreeMap::new(),
    );
    let hooks = WorkingDirectoryAbaHooks {
        target: working_directory.clone(),
        original_hidden: fixture.path("cwd-original"),
        replacement,
        replacement_hidden: fixture.path("cwd-replacement-hidden"),
    };

    let receipt = execute_with_hooks(&launch, &hooks).expect("held cwd must be entered");

    assert!(receipt.success());
    assert_eq!(
        receipt.diagnostics.stdout,
        format!("{expected_inode}\n"),
        "the child must observe the held original cwd inode"
    );
    assert_eq!(
        fs::metadata(&working_directory)
            .expect("restored reviewed cwd")
            .ino(),
        expected_inode
    );
}

#[test]
fn replacement_before_descriptor_acquisition_fails_closed() {
    let fixture = Fixture::new("pre-open-replacement");
    let working_directory = fixture.directory("cwd");
    let executable = fixture.copy_binary("reviewed", &["/usr/bin/true", "/bin/true"]);
    let replacement = fixture.copy_binary("replacement", &["/usr/bin/false", "/bin/false"]);
    let launch = plan(
        "descriptor.pre_open",
        &executable,
        &working_directory,
        Vec::new(),
        BTreeMap::new(),
    );
    fs::rename(&executable, fixture.path("reviewed-old")).expect("hide reviewed executable");
    fs::rename(&replacement, &executable).expect("replace executable");

    let error = execute_reviewed_linux_launch(&launch).expect_err("identity replacement must fail");

    assert_eq!(
        error.kind(),
        DescriptorBoundLaunchErrorKind::FilesystemIdentity
    );
    assert_eq!(error.stage(), "executable");
}

#[test]
fn symlinked_executable_alias_is_rejected() {
    let fixture = Fixture::new("symlink");
    let working_directory = fixture.directory("cwd");
    let executable = fixture.copy_binary("reviewed", &["/usr/bin/true", "/bin/true"]);
    let executable_identity = identity(&executable);
    let alias = fixture.path("alias");
    std::os::unix::fs::symlink(&executable, &alias).expect("create executable alias");
    let launch = ReviewedLinuxLaunchPlan::new(
        "descriptor.symlink",
        &alias,
        executable_identity,
        &working_directory,
        identity(&working_directory),
        Vec::new(),
        BTreeMap::new(),
        current_credentials(),
    )
    .expect("lexical plan");

    let error = execute_reviewed_linux_launch(&launch).expect_err("symlinked executable must fail");

    assert_eq!(
        error.kind(),
        DescriptorBoundLaunchErrorKind::FilesystemIdentity
    );
}

#[test]
fn hard_linked_executable_is_rejected() {
    let fixture = Fixture::new("hard-link");
    let working_directory = fixture.directory("cwd");
    let executable = fixture.copy_binary("reviewed", &["/usr/bin/true", "/bin/true"]);
    fs::hard_link(&executable, fixture.path("second-link")).expect("create hard link");
    let launch = plan(
        "descriptor.hard_link",
        &executable,
        &working_directory,
        Vec::new(),
        BTreeMap::new(),
    );

    let error =
        execute_reviewed_linux_launch(&launch).expect_err("hard-linked executable must fail");

    assert_eq!(
        error.kind(),
        DescriptorBoundLaunchErrorKind::FilesystemIdentity
    );
}

#[test]
fn scripts_are_rejected_before_spawn() {
    let fixture = Fixture::new("script");
    let working_directory = fixture.directory("cwd");
    let executable = fixture.path("reviewed-script");
    fs::write(&executable, "#!/bin/sh\nexit 0\n").expect("write script");
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).expect("set script mode");
    let launch = plan(
        "descriptor.script",
        &executable,
        &working_directory,
        Vec::new(),
        BTreeMap::new(),
    );

    let error = execute_reviewed_linux_launch(&launch).expect_err("script must fail");

    assert_eq!(
        error.kind(),
        DescriptorBoundLaunchErrorKind::UnsupportedExecutable
    );
}

#[test]
fn environment_is_cleared_and_replaced_exactly() {
    let fixture = Fixture::new("environment");
    let working_directory = fixture.directory("cwd");
    let executable = fixture.copy_binary("env", &["/usr/bin/env", "/bin/env"]);
    let environment = BTreeMap::from([(
        "ONLY_REVIEWED".to_owned(),
        ReviewedLaunchValue::plain("present"),
    )]);
    let launch = plan(
        "descriptor.environment",
        &executable,
        &working_directory,
        Vec::new(),
        environment,
    );

    let receipt = execute_reviewed_linux_launch(&launch).expect("run reviewed env");

    assert!(receipt.success());
    assert_eq!(receipt.diagnostics.stdout, "ONLY_REVIEWED=present\n");
    assert_eq!(receipt.environment_keys(), &["ONLY_REVIEWED".to_owned()]);
}

#[test]
fn nonzero_exit_is_a_typed_receipt_not_an_executor_error() {
    let fixture = Fixture::new("nonzero");
    let working_directory = fixture.directory("cwd");
    let executable = fixture.copy_binary("false", &["/usr/bin/false", "/bin/false"]);
    let launch = plan(
        "descriptor.nonzero",
        &executable,
        &working_directory,
        Vec::new(),
        BTreeMap::new(),
    );

    let receipt = execute_reviewed_linux_launch(&launch).expect("collect nonzero status");

    assert!(!receipt.success());
    assert_eq!(
        receipt.termination(),
        DescriptorBoundTermination::Exited { code: 1 }
    );
}

#[test]
fn wrong_launcher_identity_fails_before_spawn() {
    let fixture = Fixture::new("credentials");
    let working_directory = fixture.directory("cwd");
    let executable = fixture.copy_binary("true", &["/usr/bin/true", "/bin/true"]);
    let observed_uid = process::geteuid().as_raw();
    let wrong_uid = observed_uid.checked_add(1).unwrap_or(observed_uid - 1);
    let launch = ReviewedLinuxLaunchPlan::new(
        "descriptor.credentials",
        &executable,
        identity(&executable),
        &working_directory,
        identity(&working_directory),
        Vec::new(),
        BTreeMap::new(),
        ReviewedLaunchCredentials::Inherit {
            uid: wrong_uid,
            gid: process::getegid().as_raw(),
        },
    )
    .expect("reviewed plan");

    let error = execute_reviewed_linux_launch(&launch).expect_err("wrong identity must fail");

    assert_eq!(error.kind(), DescriptorBoundLaunchErrorKind::Credentials);
}

#[test]
fn output_limit_terminates_the_child_process_group() {
    let fixture = Fixture::new("output-limit");
    let working_directory = fixture.directory("cwd");
    let executable = fixture.copy_binary("yes", &["/usr/bin/yes", "/bin/yes"]);
    let launch = plan(
        "descriptor.output_limit",
        &executable,
        &working_directory,
        Vec::new(),
        BTreeMap::new(),
    );

    let error = execute_reviewed_linux_launch(&launch).expect_err("unbounded output must fail");

    assert_eq!(error.kind(), DescriptorBoundLaunchErrorKind::OutputLimit);
    assert_eq!(error.stage(), "stdout");
}

#[test]
fn public_json_and_debug_redact_paths_identities_secrets_and_output() {
    let fixture = Fixture::new("privacy");
    let working_directory = fixture.directory("private-cwd-marker");
    fixture.directory("private-executable-marker");
    let executable = fixture.copy_binary(
        "private-executable-marker/printf",
        &["/usr/bin/printf", "/bin/printf"],
    );
    let launch = plan(
        "descriptor.privacy",
        &executable,
        &working_directory,
        vec![
            ReviewedLaunchValue::plain("%s"),
            ReviewedLaunchValue::secret("private-output-marker"),
        ],
        BTreeMap::from([(
            "SECRET_ENV".to_owned(),
            ReviewedLaunchValue::secret("private-environment-marker"),
        )]),
    );

    let receipt = execute_reviewed_linux_launch(&launch).expect("run private fixture");
    let json = serde_json::to_string(&receipt).expect("serialize receipt");
    let debug = format!("{receipt:?}");
    let plan_debug = format!("{launch:?}");

    assert!(receipt.has_private_diagnostics());
    assert_eq!(receipt.diagnostics.stdout, "[REDACTED]");
    for forbidden in [
        "private-cwd-marker",
        "private-executable-marker",
        "private-output-marker",
        "private-environment-marker",
        "/proc/self/fd",
    ] {
        assert!(!json.contains(forbidden), "JSON leaked {forbidden}");
        assert!(!debug.contains(forbidden), "Debug leaked {forbidden}");
        assert!(
            !plan_debug.contains(forbidden),
            "plan Debug leaked {forbidden}"
        );
    }
    assert_eq!(receipt.argument_count(), 2);
}

#[test]
fn public_errors_do_not_include_private_paths_or_os_errors() {
    let fixture = Fixture::new("error-privacy");
    let missing = fixture.path("private-missing-executable");
    let working_directory = fixture.directory("private-cwd");
    let launch = ReviewedLinuxLaunchPlan::new(
        "descriptor.error_privacy",
        &missing,
        ReviewedFilesystemIdentity::new(1, 1, 0, 0, 0o755).expect("identity"),
        &working_directory,
        identity(&working_directory),
        Vec::new(),
        BTreeMap::new(),
        current_credentials(),
    )
    .expect("reviewed missing plan");

    let error = execute_reviewed_linux_launch(&launch).expect_err("missing executable must fail");
    let json = serde_json::to_string(&error).expect("serialize error");
    let debug = format!("{error:?}");

    assert!(!json.contains("private-missing-executable"));
    assert!(!debug.contains("private-missing-executable"));
    assert!(!error.to_string().contains("No such file"));
}

#[test]
fn plan_rejects_unbounded_or_noncanonical_inputs() {
    let fixture = Fixture::new("plan-validation");
    let working_directory = fixture.directory("cwd");
    let executable = fixture.copy_binary("true", &["/usr/bin/true", "/bin/true"]);
    let executable_identity = identity(&executable);
    let cwd_identity = identity(&working_directory);

    assert!(
        ReviewedLinuxLaunchPlan::new(
            "contains whitespace",
            &executable,
            executable_identity.clone(),
            &working_directory,
            cwd_identity.clone(),
            Vec::new(),
            BTreeMap::new(),
            current_credentials(),
        )
        .is_err()
    );
    assert!(
        ReviewedLinuxLaunchPlan::new(
            "descriptor.relative",
            "relative/executable",
            executable_identity.clone(),
            &working_directory,
            cwd_identity.clone(),
            Vec::new(),
            BTreeMap::new(),
            current_credentials(),
        )
        .is_err()
    );
    assert!(
        ReviewedLinuxLaunchPlan::new(
            "descriptor.environment_name",
            &executable,
            executable_identity,
            &working_directory,
            cwd_identity,
            Vec::new(),
            BTreeMap::from([(
                "INVALID=NAME".to_owned(),
                ReviewedLaunchValue::plain("value"),
            )]),
            current_credentials(),
        )
        .is_err()
    );
}

#[test]
fn drop_privileges_clears_groups_and_all_root_ids_without_mutating_caller() {
    if process::geteuid().as_raw() != 0 || process::getegid().as_raw() != 0 {
        return;
    }

    let caller_groups = process::getgroups().expect("read caller supplementary groups");
    let fixture = Fixture::new("credential-drop");
    let working_directory = fixture.directory("cwd");
    let executable = fixture.copy_binary("cat", &["/usr/bin/cat", "/bin/cat"]);
    let target_uid = 65534;
    let target_gid = 65534;
    let launch = ReviewedLinuxLaunchPlan::new(
        "descriptor.credential_drop",
        &executable,
        identity(&executable),
        &working_directory,
        identity(&working_directory),
        vec![ReviewedLaunchValue::plain("/proc/self/status")],
        BTreeMap::new(),
        ReviewedLaunchCredentials::DropPrivileges {
            launcher_uid: 0,
            launcher_gid: 0,
            target_uid,
            target_gid,
        },
    )
    .expect("reviewed credential-drop plan");

    let receipt = execute_reviewed_linux_launch(&launch).expect("credential drop must execute");

    assert!(receipt.success());
    let uid = status_values(&receipt.diagnostics.stdout, "Uid:");
    let gid = status_values(&receipt.diagnostics.stdout, "Gid:");
    let groups = status_values(&receipt.diagnostics.stdout, "Groups:");
    assert_eq!(uid, vec![target_uid; 4]);
    assert_eq!(gid, vec![target_gid; 4]);
    assert!(
        groups.is_empty(),
        "supplementary groups remained: {groups:?}"
    );
    assert_eq!(
        process::getgroups().expect("read caller groups after launch"),
        caller_groups,
        "the caller thread's supplementary groups must remain unchanged"
    );
}

fn status_values(output: &str, prefix: &str) -> Vec<u32> {
    output
        .lines()
        .find_map(|line| line.strip_prefix(prefix))
        .unwrap_or_else(|| panic!("missing {prefix} status line"))
        .split_whitespace()
        .map(|value| value.parse::<u32>().expect("numeric status credential"))
        .collect()
}

#[test]
fn drop_privileges_contract_rejects_nonroot_launcher_and_root_target() {
    assert!(
        ReviewedLaunchCredentials::DropPrivileges {
            launcher_uid: 1000,
            launcher_gid: 1000,
            target_uid: 1001,
            target_gid: 1001,
        }
        .validate()
        .is_err()
    );
    assert!(
        ReviewedLaunchCredentials::DropPrivileges {
            launcher_uid: 0,
            launcher_gid: 0,
            target_uid: 0,
            target_gid: 0,
        }
        .validate()
        .is_err()
    );
}
