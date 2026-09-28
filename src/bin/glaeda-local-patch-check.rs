//! Read-only applicability check for one exact sandbox-authored patch.
//!
//! This front door stops before source mutation. It revalidates one exact clean checkout, verifies
//! the supplied patch content identity, loads the exact expected Git tree into a private alternate
//! index, asks fixed `/usr/bin/git apply --check --cached` whether the patch is applicable, and
//! proves the checkout observation is unchanged afterward. It performs no
//! provider call, branch/commit creation, patch application, publication, cleanup, or authority
//! grant.

use std::fmt;
use std::fs;
use std::io::{self, Read as _};
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clap::Parser;
use glaeda::artifact::{CommitId, GitTreeId, Sha256Digest};
use glaeda::process::{
    CommandSpec, MAX_CAPTURED_STDIN_BYTES, ProcessExecutor, TimedInputCommandExecutor,
};
use glaeda::project_checkout_observation::{ProjectCheckoutObservation, ProjectCheckoutObserver};
use serde::Serialize;
use sha1::{Digest as _, Sha1};
use sha2::Sha256;

const SCHEMA_VERSION: u8 = 1;
const GIT: &str = "/usr/bin/git";
const CHECK_TIMEOUT: Duration = Duration::from_secs(15);
const SHA1_HEX_BYTES: usize = 40;
const SHA256_PREFIX: &str = "sha256:";
static TEMP_INDEX_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Parser)]
#[command(about = "Check one exact patch against an exact clean checkout without applying it")]
struct Args {
    /// Canonical absolute task-private Git checkout path. Never emitted in the public report.
    #[arg(long)]
    repository: PathBuf,
    /// Exact complete lowercase Git commit expected at HEAD.
    #[arg(long)]
    expected_head: String,
    /// Exact complete lowercase Git tree expected at HEAD^{tree}.
    #[arg(long)]
    expected_tree: String,
    /// Exact lowercase SHA-1 Git blob object ID expected for the patch bytes.
    #[arg(long)]
    git_blob_sha1: String,
    /// Exact canonical sha256:<hex> digest expected for the raw patch bytes.
    #[arg(long)]
    sha256: String,
    /// Exact raw patch byte count. V1 uses the existing reviewed process-input ceiling.
    #[arg(long)]
    bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PatchCheckExpectation {
    expected_head: CommitId,
    expected_tree: GitTreeId,
    git_blob_sha1: String,
    sha256: Sha256Digest,
    bytes: usize,
}

impl PatchCheckExpectation {
    fn new(args: &Args) -> Result<Self, PatchCheckError> {
        let expected_head =
            CommitId::parse(&args.expected_head).map_err(|_| invalid_expectation())?;
        let expected_tree =
            GitTreeId::parse(&args.expected_tree).map_err(|_| invalid_expectation())?;
        if !is_lower_hex(&args.git_blob_sha1, SHA1_HEX_BYTES)
            || args.bytes == 0
            || args.bytes > MAX_CAPTURED_STDIN_BYTES
        {
            return Err(invalid_expectation());
        }
        let sha256 = Sha256Digest::parse(&args.sha256).map_err(|_| invalid_expectation())?;
        Ok(Self {
            expected_head,
            expected_tree,
            git_blob_sha1: args.git_blob_sha1.clone(),
            sha256,
            bytes: args.bytes,
        })
    }
}

#[derive(Debug)]
struct TemporaryGitIndex {
    directory: PathBuf,
    path: PathBuf,
}

impl TemporaryGitIndex {
    fn new() -> Result<Self, PatchCheckError> {
        let root = fs::canonicalize("/tmp").map_err(|_| check_unavailable())?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| check_unavailable())?
            .as_nanos();
        for _ in 0..32 {
            let sequence = TEMP_INDEX_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let directory = root.join(format!(
                "glaeda-local-patch-index-{}-{now}-{sequence}",
                std::process::id()
            ));
            match fs::DirBuilder::new().mode(0o700).create(&directory) {
                Ok(()) => {
                    let path = directory.join("index");
                    return Ok(Self { directory, path });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(_) => return Err(check_unavailable()),
            }
        }
        Err(check_unavailable())
    }
}

impl Drop for TemporaryGitIndex {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct PatchApplicabilityReport {
    schema_version: u8,
    document_type: &'static str,
    authority: &'static str,
    materialization_id: Sha256Digest,
    expected_head: CommitId,
    expected_tree: GitTreeId,
    git_blob_sha1: String,
    sha256: Sha256Digest,
    bytes: usize,
    input_limit_bytes: usize,
    check_only: bool,
    index_consistency_required: bool,
    applicable: bool,
    source_unchanged: bool,
    contains_patch_content: bool,
    contains_private_path: bool,
    authorizes_source_mutation: bool,
    authorizes_execution: bool,
    authorizes_publication: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum PatchCheckErrorKind {
    InvalidExpectation,
    InputTooLarge,
    ByteCountMismatch,
    Sha256Mismatch,
    GitBlobMismatch,
    InvalidUtf8,
    ContainsNul,
    NotUnifiedDiff,
    InputUnavailable,
    CheckoutUnavailable,
    BaseMismatch,
    CheckoutDirty,
    CheckUnavailable,
    SourceChanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
struct PatchCheckError {
    kind: PatchCheckErrorKind,
    code: &'static str,
    problem: &'static str,
}

impl fmt::Display for PatchCheckError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.problem)
    }
}

impl std::error::Error for PatchCheckError {}

#[derive(Debug, Serialize)]
struct RefusalReceipt<'a> {
    schema_version: u8,
    document_type: &'static str,
    admitted: bool,
    code: &'a str,
    problem: &'a str,
    contains_patch_content: bool,
    contains_private_path: bool,
    authorizes_source_mutation: bool,
    authorizes_execution: bool,
    authorizes_publication: bool,
}

fn main() {
    let args = Args::parse();
    match run(args) {
        Ok(report) => println!(
            "{}",
            serde_json::to_string(&report).expect("patch applicability report is serializable")
        ),
        Err(error) => {
            let receipt = RefusalReceipt {
                schema_version: SCHEMA_VERSION,
                document_type: "glaeda-local-patch-applicability-refusal",
                admitted: false,
                code: error.code,
                problem: error.problem,
                contains_patch_content: false,
                contains_private_path: false,
                authorizes_source_mutation: false,
                authorizes_execution: false,
                authorizes_publication: false,
            };
            eprintln!(
                "{}",
                serde_json::to_string(&receipt)
                    .expect("patch applicability refusal is serializable")
            );
            std::process::exit(2);
        }
    }
}

fn run(args: Args) -> Result<PatchApplicabilityReport, PatchCheckError> {
    let expectation = PatchCheckExpectation::new(&args)?;
    let mut input = Vec::with_capacity(expectation.bytes.min(MAX_CAPTURED_STDIN_BYTES));
    io::stdin()
        .take((MAX_CAPTURED_STDIN_BYTES + 1) as u64)
        .read_to_end(&mut input)
        .map_err(|_| input_unavailable())?;
    evaluate_patch(&args.repository, &expectation, &input, &ProcessExecutor)
}

fn evaluate_patch(
    repository: &Path,
    expectation: &PatchCheckExpectation,
    patch: &[u8],
    executor: &impl TimedInputCommandExecutor,
) -> Result<PatchApplicabilityReport, PatchCheckError> {
    validate_patch_identity(expectation, patch)?;

    let observer = ProjectCheckoutObserver::new(GIT).map_err(|_| checkout_unavailable())?;
    let before = observer
        .observe(repository, executor)
        .map_err(|_| checkout_unavailable())?;
    validate_base(&before, expectation)?;

    let temporary_index = TemporaryGitIndex::new()?;
    let read_tree = read_tree_command(
        repository,
        &temporary_index.path,
        &expectation.expected_tree,
    )?;
    let expected_read_tree_argv = read_tree.displayed_argv();
    let expected_read_tree_environment = read_tree.environment.keys().cloned().collect::<Vec<_>>();
    let read_tree_record = executor
        .execute_with_timeout(&read_tree, CHECK_TIMEOUT)
        .map_err(|_| check_unavailable())?;
    if read_tree_record.argv != expected_read_tree_argv
        || read_tree_record.environment_keys != expected_read_tree_environment
        || !read_tree_record.success
        || read_tree_record.status != Some(0)
    {
        return Err(check_unavailable());
    }

    let spec = applicability_command(repository, &temporary_index.path)?;
    let expected_argv = spec.displayed_argv();
    let expected_environment_keys = spec.environment.keys().cloned().collect::<Vec<_>>();
    let record = executor
        .execute_with_input(&spec, patch, CHECK_TIMEOUT)
        .map_err(|_| check_unavailable())?;
    if record.argv != expected_argv || record.environment_keys != expected_environment_keys {
        return Err(check_unavailable());
    }
    let applicable = match (record.success, record.status) {
        (true, Some(0)) => true,
        (false, Some(1)) => false,
        _ => return Err(check_unavailable()),
    };

    let after = observer
        .observe(repository, executor)
        .map_err(|_| source_changed())?;
    if after != before {
        return Err(source_changed());
    }

    Ok(PatchApplicabilityReport {
        schema_version: SCHEMA_VERSION,
        document_type: "glaeda-local-patch-applicability",
        authority: "read_only_applicability_only",
        materialization_id: before.materialization_id().clone(),
        expected_head: expectation.expected_head.clone(),
        expected_tree: expectation.expected_tree.clone(),
        git_blob_sha1: expectation.git_blob_sha1.clone(),
        sha256: expectation.sha256.clone(),
        bytes: expectation.bytes,
        input_limit_bytes: MAX_CAPTURED_STDIN_BYTES,
        check_only: true,
        index_consistency_required: true,
        applicable,
        source_unchanged: true,
        contains_patch_content: false,
        contains_private_path: false,
        authorizes_source_mutation: false,
        authorizes_execution: false,
        authorizes_publication: false,
    })
}

fn validate_base(
    observation: &ProjectCheckoutObservation,
    expectation: &PatchCheckExpectation,
) -> Result<(), PatchCheckError> {
    if observation.commit() != &expectation.expected_head
        || observation.tree() != &expectation.expected_tree
    {
        return Err(base_mismatch());
    }
    if observation.tracked_changes_present() || observation.untracked_entry_count() != 0 {
        return Err(checkout_dirty());
    }
    Ok(())
}

fn git_command(repository: &Path, index: &Path) -> Result<CommandSpec, PatchCheckError> {
    let repository = repository.to_str().ok_or_else(checkout_unavailable)?;
    let index = index.to_str().ok_or_else(check_unavailable)?;
    Ok(CommandSpec::new(GIT)
        .argument("--no-optional-locks")
        .argument("-c")
        .argument("credential.helper=")
        .argument("-c")
        .argument("core.fsmonitor=false")
        .argument("-c")
        .argument("core.hooksPath=/dev/null")
        .argument("-c")
        .argument("diff.external=")
        .argument("-C")
        .argument(repository)
        .environment("GIT_ASKPASS", "/bin/false")
        .environment("GIT_ALLOW_PROTOCOL", "")
        .environment("GIT_ATTR_NOSYSTEM", "1")
        .environment("GIT_CONFIG_GLOBAL", "/dev/null")
        .environment("GIT_CONFIG_NOSYSTEM", "1")
        .environment("GIT_CONFIG_SYSTEM", "/dev/null")
        .environment("GIT_INDEX_FILE", index)
        .environment("GIT_NO_LAZY_FETCH", "1")
        .environment("GIT_NO_REPLACE_OBJECTS", "1")
        .environment("GIT_PROTOCOL_FROM_USER", "0")
        .environment("GIT_TERMINAL_PROMPT", "0")
        .environment("LANG", "C")
        .environment("LC_ALL", "C"))
}

fn read_tree_command(
    repository: &Path,
    index: &Path,
    expected_tree: &GitTreeId,
) -> Result<CommandSpec, PatchCheckError> {
    Ok(git_command(repository, index)?
        .argument("read-tree")
        .argument(expected_tree.as_str()))
}

fn applicability_command(repository: &Path, index: &Path) -> Result<CommandSpec, PatchCheckError> {
    Ok(git_command(repository, index)?
        .argument("-c")
        .argument("apply.ignoreWhitespace=false")
        .argument("apply")
        .argument("--check")
        .argument("--cached")
        .argument("--whitespace=nowarn")
        .argument("-"))
}

fn validate_patch_identity(
    expectation: &PatchCheckExpectation,
    patch: &[u8],
) -> Result<(), PatchCheckError> {
    if patch.len() > MAX_CAPTURED_STDIN_BYTES {
        return Err(input_too_large());
    }
    if patch.len() != expectation.bytes {
        return Err(byte_count_mismatch());
    }
    if sha256(patch) != expectation.sha256.as_str() {
        return Err(sha256_mismatch());
    }
    if git_blob_sha1(patch) != expectation.git_blob_sha1 {
        return Err(git_blob_mismatch());
    }
    let text = std::str::from_utf8(patch).map_err(|_| invalid_utf8())?;
    if patch.contains(&0) {
        return Err(contains_nul());
    }
    validate_unified_diff(text)
}

fn validate_unified_diff(text: &str) -> Result<(), PatchCheckError> {
    let mut old_header = false;
    let mut new_header = false;
    let mut hunk = false;
    for line in text.lines() {
        if line.starts_with("--- ") {
            old_header = true;
        } else if line.starts_with("+++ ") && old_header {
            new_header = true;
        } else if line.starts_with("@@ ") && old_header && new_header {
            hunk = true;
        }
    }
    if text.is_empty() || !old_header || !new_header || !hunk {
        return Err(not_unified_diff());
    }
    Ok(())
}

fn git_blob_sha1(bytes: &[u8]) -> String {
    let mut hasher = Sha1::new();
    hasher.update(format!("blob {}\0", bytes.len()).as_bytes());
    hasher.update(bytes);
    lower_hex(&hasher.finalize())
}

fn sha256(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{SHA256_PREFIX}{}", lower_hex(&hasher.finalize()))
}

fn lower_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

fn is_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

const fn err(
    kind: PatchCheckErrorKind,
    code: &'static str,
    problem: &'static str,
) -> PatchCheckError {
    PatchCheckError {
        kind,
        code,
        problem,
    }
}

const fn invalid_expectation() -> PatchCheckError {
    err(
        PatchCheckErrorKind::InvalidExpectation,
        "patch_check_expectation_invalid",
        "patch check expectation is outside the reviewed boundary",
    )
}
const fn input_too_large() -> PatchCheckError {
    err(
        PatchCheckErrorKind::InputTooLarge,
        "patch_check_input_too_large",
        "patch input exceeds the reviewed process-input limit",
    )
}
const fn byte_count_mismatch() -> PatchCheckError {
    err(
        PatchCheckErrorKind::ByteCountMismatch,
        "patch_check_byte_count_mismatch",
        "patch byte count does not match the expected identity",
    )
}
const fn sha256_mismatch() -> PatchCheckError {
    err(
        PatchCheckErrorKind::Sha256Mismatch,
        "patch_check_sha256_mismatch",
        "patch SHA-256 does not match the expected identity",
    )
}
const fn git_blob_mismatch() -> PatchCheckError {
    err(
        PatchCheckErrorKind::GitBlobMismatch,
        "patch_check_git_blob_mismatch",
        "patch Git blob identity does not match the expected identity",
    )
}
const fn invalid_utf8() -> PatchCheckError {
    err(
        PatchCheckErrorKind::InvalidUtf8,
        "patch_check_utf8_invalid",
        "patch input is not valid UTF-8",
    )
}
const fn contains_nul() -> PatchCheckError {
    err(
        PatchCheckErrorKind::ContainsNul,
        "patch_check_nul_forbidden",
        "patch input contains a NUL byte",
    )
}
const fn not_unified_diff() -> PatchCheckError {
    err(
        PatchCheckErrorKind::NotUnifiedDiff,
        "patch_check_unified_diff_invalid",
        "patch input is not an ordinary unified diff",
    )
}
const fn input_unavailable() -> PatchCheckError {
    err(
        PatchCheckErrorKind::InputUnavailable,
        "patch_check_input_unavailable",
        "patch input could not be read",
    )
}
const fn checkout_unavailable() -> PatchCheckError {
    err(
        PatchCheckErrorKind::CheckoutUnavailable,
        "patch_check_checkout_unavailable",
        "exact checkout evidence is unavailable",
    )
}
const fn base_mismatch() -> PatchCheckError {
    err(
        PatchCheckErrorKind::BaseMismatch,
        "patch_check_base_mismatch",
        "checkout commit or tree does not match the expected base",
    )
}
const fn checkout_dirty() -> PatchCheckError {
    err(
        PatchCheckErrorKind::CheckoutDirty,
        "patch_check_checkout_dirty",
        "checkout is not clean at the expected base",
    )
}
const fn check_unavailable() -> PatchCheckError {
    err(
        PatchCheckErrorKind::CheckUnavailable,
        "patch_check_unavailable",
        "patch applicability could not be established",
    )
}
const fn source_changed() -> PatchCheckError {
    err(
        PatchCheckErrorKind::SourceChanged,
        "patch_check_source_changed",
        "checkout changed during the applicability check",
    )
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io;
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use glaeda::process::{CommandExecutor, ExecutionRecord, TimedCommandExecutor};

    use super::*;

    const PATCH: &[u8] = b"diff --git a/example.txt b/example.txt\n--- a/example.txt\n+++ b/example.txt\n@@ -1 +1 @@\n-before\n+after\n";
    const BAD_CONTEXT_PATCH: &[u8] = b"diff --git a/example.txt b/example.txt\n--- a/example.txt\n+++ b/example.txt\n@@ -1 +1 @@\n-missing\n+after\n";
    static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct Fixture {
        root: PathBuf,
        head: String,
        tree: String,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn fixture() -> Fixture {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temp_root = fs::canonicalize(std::env::temp_dir()).expect("canonical temp root");
        let root = temp_root.join(format!(
            "glaeda-local-patch-check-{}-{nonce}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("fixture root");
        run_git(&root, &["init", "-q"]);
        run_git(&root, &["config", "user.name", "Glaeda Test"]);
        run_git(&root, &["config", "user.email", "glaeda@example.invalid"]);
        fs::write(root.join("example.txt"), "before\n").expect("source");
        run_git(&root, &["add", "example.txt"]);
        run_git(&root, &["commit", "-qm", "base"]);
        let head = git_output(&root, &["rev-parse", "HEAD"]);
        let tree = git_output(&root, &["rev-parse", "HEAD^{tree}"]);
        Fixture { root, head, tree }
    }

    fn git(root: &Path, args: &[&str]) -> Command {
        let mut command = Command::new(GIT);
        command
            .env_clear()
            .env("HOME", root)
            .env("LANG", "C")
            .env("LC_ALL", "C")
            .arg("-C")
            .arg(root)
            .args(args);
        command
    }

    fn run_git(root: &Path, args: &[&str]) {
        let status = git(root, args).status().expect("git process");
        assert!(status.success(), "git fixture command failed: {args:?}");
    }

    fn git_output(root: &Path, args: &[&str]) -> String {
        let output = git(root, args).output().expect("git process");
        assert!(
            output.status.success(),
            "git fixture command failed: {args:?}"
        );
        String::from_utf8(output.stdout)
            .expect("utf8")
            .trim()
            .to_owned()
    }

    fn expectation(fixture: &Fixture, patch: &[u8]) -> PatchCheckExpectation {
        PatchCheckExpectation {
            expected_head: CommitId::parse(&fixture.head).expect("head"),
            expected_tree: GitTreeId::parse(&fixture.tree).expect("tree"),
            git_blob_sha1: git_blob_sha1(patch),
            sha256: Sha256Digest::parse(&sha256(patch)).expect("sha256"),
            bytes: patch.len(),
        }
    }

    struct SwapDuringPatchExecutor {
        inner: ProcessExecutor,
        root: PathBuf,
        parked: PathBuf,
        replacement: PathBuf,
    }

    impl CommandExecutor for SwapDuringPatchExecutor {
        fn execute(&self, spec: &CommandSpec) -> io::Result<ExecutionRecord> {
            self.inner.execute(spec)
        }
    }

    impl TimedCommandExecutor for SwapDuringPatchExecutor {
        fn execute_with_timeout(
            &self,
            spec: &CommandSpec,
            timeout: Duration,
        ) -> io::Result<ExecutionRecord> {
            self.inner.execute_with_timeout(spec, timeout)
        }
    }

    impl TimedInputCommandExecutor for SwapDuringPatchExecutor {
        fn execute_with_input(
            &self,
            spec: &CommandSpec,
            input: &[u8],
            timeout: Duration,
        ) -> io::Result<ExecutionRecord> {
            fs::rename(&self.root, &self.parked)?;
            fs::rename(&self.replacement, &self.root)?;
            let result = self.inner.execute_with_input(spec, input, timeout);
            let restore_replacement = fs::rename(&self.root, &self.replacement);
            let restore_original = fs::rename(&self.parked, &self.root);
            restore_replacement?;
            restore_original?;
            result
        }
    }

    #[test]
    fn transient_same_tree_replacement_cannot_change_applicability() {
        let replacement = fixture();
        let fixture = fixture();
        assert_eq!(fixture.tree, replacement.tree);
        fs::write(replacement.root.join("example.txt"), "replacement\n")
            .expect("dirty replacement");
        let parked = fixture.root.with_file_name(format!(
            "{}-parked",
            fixture
                .root
                .file_name()
                .expect("fixture name")
                .to_string_lossy()
        ));
        let executor = SwapDuringPatchExecutor {
            inner: ProcessExecutor,
            root: fixture.root.clone(),
            parked,
            replacement: replacement.root.clone(),
        };

        let report = evaluate_patch(
            &fixture.root,
            &expectation(&fixture, PATCH),
            PATCH,
            &executor,
        )
        .expect("tree-bound applicability");
        assert!(report.applicable);
        assert!(report.source_unchanged);
        assert_eq!(git_output(&fixture.root, &["status", "--porcelain=v1"]), "");
        assert!(!git_output(&replacement.root, &["status", "--porcelain=v1"]).is_empty());
    }

    #[test]
    fn exact_applicable_patch_is_checked_without_mutating_source() {
        let fixture = fixture();
        let before_status = git_output(&fixture.root, &["status", "--porcelain=v1"]);
        let report = evaluate_patch(
            &fixture.root,
            &expectation(&fixture, PATCH),
            PATCH,
            &ProcessExecutor,
        )
        .expect("applicability");
        assert!(report.applicable);
        assert!(report.source_unchanged);
        assert!(!report.contains_patch_content);
        assert!(!report.contains_private_path);
        assert!(!report.authorizes_source_mutation);
        assert!(!report.authorizes_execution);
        assert_eq!(
            git_output(&fixture.root, &["rev-parse", "HEAD"]),
            fixture.head
        );
        assert_eq!(
            git_output(&fixture.root, &["status", "--porcelain=v1"]),
            before_status
        );
    }

    #[test]
    fn context_mismatch_is_bounded_and_source_stays_clean() {
        let fixture = fixture();
        let report = evaluate_patch(
            &fixture.root,
            &expectation(&fixture, BAD_CONTEXT_PATCH),
            BAD_CONTEXT_PATCH,
            &ProcessExecutor,
        )
        .expect("negative applicability");
        assert!(!report.applicable);
        assert!(report.source_unchanged);
        assert_eq!(git_output(&fixture.root, &["status", "--porcelain=v1"]), "");
    }

    #[test]
    fn dirty_or_wrong_base_is_refused_before_check() {
        let fixture = fixture();
        fs::write(fixture.root.join("untracked.txt"), "foreign\n").expect("untracked");
        assert_eq!(
            evaluate_patch(
                &fixture.root,
                &expectation(&fixture, PATCH),
                PATCH,
                &ProcessExecutor
            )
            .expect_err("dirty refusal")
            .kind,
            PatchCheckErrorKind::CheckoutDirty
        );
        fs::remove_file(fixture.root.join("untracked.txt")).expect("remove untracked");

        let mut wrong = expectation(&fixture, PATCH);
        wrong.expected_head = CommitId::parse(&"0".repeat(40)).expect("synthetic oid");
        assert_eq!(
            evaluate_patch(&fixture.root, &wrong, PATCH, &ProcessExecutor)
                .expect_err("base refusal")
                .kind,
            PatchCheckErrorKind::BaseMismatch
        );
    }

    #[test]
    fn identity_and_v1_input_limit_fail_closed() {
        let fixture = fixture();
        let expected = expectation(&fixture, PATCH);
        assert_eq!(
            validate_patch_identity(&expected, &PATCH[..PATCH.len() - 1])
                .expect_err("byte mismatch")
                .kind,
            PatchCheckErrorKind::ByteCountMismatch
        );

        let oversized = vec![b'a'; MAX_CAPTURED_STDIN_BYTES + 1];
        let mut oversized_expectation = expected;
        oversized_expectation.bytes = oversized.len();
        assert_eq!(
            validate_patch_identity(&oversized_expectation, &oversized)
                .expect_err("oversized")
                .kind,
            PatchCheckErrorKind::InputTooLarge
        );
    }

    #[test]
    fn report_and_refusal_are_content_and_path_free() {
        let fixture = fixture();
        let report = evaluate_patch(
            &fixture.root,
            &expectation(&fixture, PATCH),
            PATCH,
            &ProcessExecutor,
        )
        .expect("applicability");
        let encoded = serde_json::to_string(&report).expect("json");
        assert!(!encoded.contains("example.txt"));
        assert!(!encoded.contains(fixture.root.to_string_lossy().as_ref()));
        assert!(!encoded.contains("before"));
        assert!(!encoded.contains("after"));

        let error = checkout_unavailable();
        let refusal = RefusalReceipt {
            schema_version: SCHEMA_VERSION,
            document_type: "glaeda-local-patch-applicability-refusal",
            admitted: false,
            code: error.code,
            problem: error.problem,
            contains_patch_content: false,
            contains_private_path: false,
            authorizes_source_mutation: false,
            authorizes_execution: false,
            authorizes_publication: false,
        };
        let encoded = serde_json::to_string(&refusal).expect("json");
        assert!(!encoded.contains(fixture.root.to_string_lossy().as_ref()));
        assert!(!encoded.contains("example.txt"));
    }
}
