use std::collections::BTreeSet;
use std::fs::File;
use std::io::Read;
use std::os::fd::AsFd as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Component, Path};

use rustix::fs::{self, Mode, OFlags};
use rustix::io::Errno;
use rustix::process::{getegid, geteuid};
use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::artifact::Sha256Digest;
use crate::local_install_plan::{LocalInstallIdentityGeneration, LocalInstallSourceIdentity};
use crate::process::TimedCommandExecutor;
use crate::project_checkout_observation::{
    ProjectCheckoutObservation, ProjectCheckoutObservationErrorKind, ProjectCheckoutObserver,
};
use crate::project_workspace_identity::{
    ProjectWorkspaceFilesystemIdentityKind, ProjectWorkspaceIdentityGeneration,
    project_workspace_filesystem_identity,
};

pub const LOCAL_INSTALL_SOURCE_PREFLIGHT_SCHEMA_VERSION: u8 = 3;
pub const MAX_LOCAL_INSTALL_CARGO_LOCK_BYTES: usize = 4 * 1024 * 1024;

const SMOLRUNNER_PROJECT: &str = "github.com/teamleaderleo/smolrunner";
const GLAEDA_PROJECT: &str = "github.com/teamleaderleo/glaeda";
const SHA256_PREFIX: &str = "sha256:";
const HEX: &[u8; 16] = b"0123456789abcdef";
const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const FILE_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::NONBLOCK)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalInstallSourceProjectDisposition {
    ExactSmolrunner,
    ExactGlaeda,
    Other,
    Ambiguous,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalInstallSourceBlockingCode {
    IdentityGenerationMismatch,
    WrongProject,
    CommitMismatch,
    TreeMismatch,
    LockfileMismatch,
    CheckoutDirty,
    SourceChanged,
    UnsafeSource,
    UnknownSource,
}

/// Path-private proof that one current checkout matches an exact local-install source identity.
///
/// The fields are private so callers may inspect or serialize accepted evidence without mutating a
/// validated receipt afterward.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalInstallSourcePreflightReceipt {
    schema_version: u8,
    identity_generation: ProjectWorkspaceIdentityGeneration,
    local_install_identity_generation: LocalInstallIdentityGeneration,
    expected_source_digest: Sha256Digest,
    observed_project: LocalInstallSourceProjectDisposition,
    commit_match: bool,
    tree_match: bool,
    lockfile_digest_match: bool,
    checkout_clean: bool,
    observation_stable: bool,
    ready: bool,
    blocking_codes: Vec<LocalInstallSourceBlockingCode>,
}

impl LocalInstallSourcePreflightReceipt {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn identity_generation(&self) -> ProjectWorkspaceIdentityGeneration {
        self.identity_generation
    }

    #[must_use]
    pub const fn local_install_identity_generation(&self) -> LocalInstallIdentityGeneration {
        self.local_install_identity_generation
    }

    #[must_use]
    pub const fn expected_source_digest(&self) -> &Sha256Digest {
        &self.expected_source_digest
    }

    #[must_use]
    pub const fn observed_project(&self) -> LocalInstallSourceProjectDisposition {
        self.observed_project
    }

    #[must_use]
    pub const fn commit_match(&self) -> bool {
        self.commit_match
    }

    #[must_use]
    pub const fn tree_match(&self) -> bool {
        self.tree_match
    }

    #[must_use]
    pub const fn lockfile_digest_match(&self) -> bool {
        self.lockfile_digest_match
    }

    #[must_use]
    pub const fn checkout_clean(&self) -> bool {
        self.checkout_clean
    }

    #[must_use]
    pub const fn observation_stable(&self) -> bool {
        self.observation_stable
    }

    #[must_use]
    pub const fn ready(&self) -> bool {
        self.ready
    }

    #[must_use]
    pub fn blocking_codes(&self) -> &[LocalInstallSourceBlockingCode] {
        &self.blocking_codes
    }
}

/// Compose one exact no-follow `Cargo.lock` proof around the existing fixed Git checkout observer.
///
/// The lockfile is observed before and after the complete Git observation. Both lock snapshots bind
/// to the checkout observer's existing materialization identity. Any object, metadata, or digest
/// drift becomes `source_changed`. No raw path, Git output, lockfile content, UID/GID, device/inode,
/// or operating-system error enters the returned receipt.
#[must_use]
pub fn observe_local_install_source_preflight(
    expected: &LocalInstallSourceIdentity,
    checkout: &Path,
    observer: &ProjectCheckoutObserver,
    executor: &impl TimedCommandExecutor,
) -> LocalInstallSourcePreflightReceipt {
    let identity_generation = observer.identity_generation();
    let local_install_identity_generation = expected.identity_generation();
    if identity_generation != workspace_generation(local_install_identity_generation) {
        return root_cause_receipt(
            expected,
            identity_generation,
            LocalInstallSourceBlockingCode::IdentityGenerationMismatch,
        );
    }
    let first_lock = match observe_lock_snapshot(checkout, identity_generation) {
        Ok(snapshot) => snapshot,
        Err(LockSnapshotError::Unsafe) => {
            return root_cause_receipt(
                expected,
                identity_generation,
                LocalInstallSourceBlockingCode::UnsafeSource,
            );
        }
        Err(LockSnapshotError::Unknown) => {
            return root_cause_receipt(
                expected,
                identity_generation,
                LocalInstallSourceBlockingCode::UnknownSource,
            );
        }
        Err(LockSnapshotError::Changed) => {
            return root_cause_receipt(
                expected,
                identity_generation,
                LocalInstallSourceBlockingCode::SourceChanged,
            );
        }
    };

    let checkout_observation = match observer.observe(checkout, executor) {
        Ok(observation) => observation,
        Err(error) => {
            let code = match error.kind {
                ProjectCheckoutObservationErrorKind::SourceChanged => {
                    LocalInstallSourceBlockingCode::SourceChanged
                }
                ProjectCheckoutObservationErrorKind::NotWorktree
                | ProjectCheckoutObservationErrorKind::BareRepository
                | ProjectCheckoutObservationErrorKind::UnsafePath => {
                    LocalInstallSourceBlockingCode::UnsafeSource
                }
                ProjectCheckoutObservationErrorKind::Unavailable
                | ProjectCheckoutObservationErrorKind::InvalidOutput => {
                    LocalInstallSourceBlockingCode::UnknownSource
                }
            };
            return root_cause_receipt(expected, identity_generation, code);
        }
    };

    let second_lock = match observe_lock_snapshot(checkout, identity_generation) {
        Ok(snapshot) => snapshot,
        Err(LockSnapshotError::Unsafe | LockSnapshotError::Changed) => {
            return root_cause_receipt(
                expected,
                identity_generation,
                LocalInstallSourceBlockingCode::SourceChanged,
            );
        }
        Err(LockSnapshotError::Unknown) => {
            return root_cause_receipt(
                expected,
                identity_generation,
                LocalInstallSourceBlockingCode::UnknownSource,
            );
        }
    };

    if !first_lock.same_as(&second_lock)
        || first_lock.materialization_id != *checkout_observation.materialization_id()
    {
        return root_cause_receipt(
            expected,
            identity_generation,
            LocalInstallSourceBlockingCode::SourceChanged,
        );
    }

    stable_receipt(
        expected,
        identity_generation,
        &checkout_observation,
        &second_lock,
    )
}

fn stable_receipt(
    expected: &LocalInstallSourceIdentity,
    identity_generation: ProjectWorkspaceIdentityGeneration,
    observation: &ProjectCheckoutObservation,
    lock: &LockSnapshot,
) -> LocalInstallSourcePreflightReceipt {
    let local_install_identity_generation = expected.identity_generation();
    let (expected_project, exact_project_disposition) = match local_install_identity_generation {
        LocalInstallIdentityGeneration::SmolrunnerV1 => (
            SMOLRUNNER_PROJECT,
            LocalInstallSourceProjectDisposition::ExactSmolrunner,
        ),
        LocalInstallIdentityGeneration::GlaedaV2 => (
            GLAEDA_PROJECT,
            LocalInstallSourceProjectDisposition::ExactGlaeda,
        ),
    };
    let observed_project = if observation.source_ambiguous() {
        LocalInstallSourceProjectDisposition::Ambiguous
    } else if observation
        .primary_project()
        .is_some_and(|project| project.as_str() == expected_project)
    {
        exact_project_disposition
    } else {
        LocalInstallSourceProjectDisposition::Other
    };
    let commit_match = observation.commit() == expected.commit();
    let tree_match = observation.tree() == expected.tree();
    let lockfile_digest_match = lock.digest == *expected.cargo_lock_digest();
    let checkout_clean =
        !observation.tracked_changes_present() && observation.untracked_entry_count() == 0;

    let mut blocking_codes = BTreeSet::new();
    if observed_project != exact_project_disposition {
        blocking_codes.insert(LocalInstallSourceBlockingCode::WrongProject);
    }
    if !commit_match {
        blocking_codes.insert(LocalInstallSourceBlockingCode::CommitMismatch);
    }
    if !tree_match {
        blocking_codes.insert(LocalInstallSourceBlockingCode::TreeMismatch);
    }
    if !lockfile_digest_match {
        blocking_codes.insert(LocalInstallSourceBlockingCode::LockfileMismatch);
    }
    if !checkout_clean {
        blocking_codes.insert(LocalInstallSourceBlockingCode::CheckoutDirty);
    }
    let blocking_codes = blocking_codes.into_iter().collect::<Vec<_>>();

    LocalInstallSourcePreflightReceipt {
        schema_version: LOCAL_INSTALL_SOURCE_PREFLIGHT_SCHEMA_VERSION,
        identity_generation,
        local_install_identity_generation,
        expected_source_digest: expected.digest().clone(),
        observed_project,
        commit_match,
        tree_match,
        lockfile_digest_match,
        checkout_clean,
        observation_stable: true,
        ready: blocking_codes.is_empty(),
        blocking_codes,
    }
}

fn root_cause_receipt(
    expected: &LocalInstallSourceIdentity,
    identity_generation: ProjectWorkspaceIdentityGeneration,
    code: LocalInstallSourceBlockingCode,
) -> LocalInstallSourcePreflightReceipt {
    LocalInstallSourcePreflightReceipt {
        schema_version: LOCAL_INSTALL_SOURCE_PREFLIGHT_SCHEMA_VERSION,
        identity_generation,
        local_install_identity_generation: expected.identity_generation(),
        expected_source_digest: expected.digest().clone(),
        observed_project: LocalInstallSourceProjectDisposition::Unknown,
        commit_match: false,
        tree_match: false,
        lockfile_digest_match: false,
        checkout_clean: false,
        observation_stable: false,
        ready: false,
        blocking_codes: vec![code],
    }
}

const fn workspace_generation(
    generation: LocalInstallIdentityGeneration,
) -> ProjectWorkspaceIdentityGeneration {
    match generation {
        LocalInstallIdentityGeneration::SmolrunnerV1 => {
            ProjectWorkspaceIdentityGeneration::SmolrunnerV1
        }
        LocalInstallIdentityGeneration::GlaedaV2 => ProjectWorkspaceIdentityGeneration::GlaedaV2,
    }
}

#[derive(Debug, Clone)]
struct LockSnapshot {
    materialization_id: Sha256Digest,
    root: PrivateMetadata,
    lock: PrivateMetadata,
    digest: Sha256Digest,
}

impl LockSnapshot {
    fn same_as(&self, other: &Self) -> bool {
        self.materialization_id == other.materialization_id
            && self.root.same_as(&other.root)
            && self.lock.same_as(&other.lock)
            && self.digest == other.digest
    }
}

#[derive(Debug, Clone)]
struct PrivateMetadata {
    device: u64,
    inode: u64,
    uid: u32,
    gid: u32,
    mode: u32,
    links: u64,
    size: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

impl PrivateMetadata {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            uid: metadata.uid(),
            gid: metadata.gid(),
            mode: metadata.mode(),
            links: metadata.nlink(),
            size: metadata.size(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }

    fn same_as(&self, other: &Self) -> bool {
        self.device == other.device
            && self.inode == other.inode
            && self.uid == other.uid
            && self.gid == other.gid
            && self.mode == other.mode
            && self.links == other.links
            && self.size == other.size
            && self.modified_seconds == other.modified_seconds
            && self.modified_nanoseconds == other.modified_nanoseconds
            && self.changed_seconds == other.changed_seconds
            && self.changed_nanoseconds == other.changed_nanoseconds
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LockSnapshotError {
    Unsafe,
    Unknown,
    Changed,
}

fn observe_lock_snapshot(
    checkout: &Path,
    identity_generation: ProjectWorkspaceIdentityGeneration,
) -> Result<LockSnapshot, LockSnapshotError> {
    if !valid_checkout_path(checkout) {
        return Err(LockSnapshotError::Unsafe);
    }
    let root = open_checkout_root(checkout)?;
    let root_before = root.metadata().map_err(|_| LockSnapshotError::Unknown)?;
    validate_checkout_root(&root_before)?;

    let lock =
        fs::openat(root.as_fd(), "Cargo.lock", FILE_FLAGS, Mode::empty()).map_err(map_lock_open)?;
    let mut lock = File::from(lock);
    let lock_before = lock.metadata().map_err(|_| LockSnapshotError::Unknown)?;
    validate_lockfile(&lock_before)?;

    let mut bytes = Vec::new();
    Read::by_ref(&mut lock)
        .take((MAX_LOCAL_INSTALL_CARGO_LOCK_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| LockSnapshotError::Unknown)?;
    if bytes.len() > MAX_LOCAL_INSTALL_CARGO_LOCK_BYTES {
        return Err(LockSnapshotError::Unsafe);
    }

    let lock_after = lock.metadata().map_err(|_| LockSnapshotError::Unknown)?;
    let root_after = root.metadata().map_err(|_| LockSnapshotError::Unknown)?;
    if !stable_metadata(&lock_before, &lock_after) || !stable_metadata(&root_before, &root_after) {
        return Err(LockSnapshotError::Changed);
    }

    Ok(LockSnapshot {
        materialization_id: materialization_id(&root_after, identity_generation),
        root: PrivateMetadata::from_metadata(&root_after),
        lock: PrivateMetadata::from_metadata(&lock_after),
        digest: sha256_digest(&bytes),
    })
}

fn open_checkout_root(checkout: &Path) -> Result<File, LockSnapshotError> {
    let components = checkout
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value),
            Component::RootDir => None,
            Component::CurDir | Component::ParentDir | Component::Prefix(_) => None,
        })
        .collect::<Vec<_>>();
    if components.is_empty() {
        return Err(LockSnapshotError::Unsafe);
    }

    let mut current = File::from(
        fs::open("/", DIRECTORY_FLAGS, Mode::empty()).map_err(|_| LockSnapshotError::Unknown)?,
    );
    for component in components {
        let opened = fs::openat(current.as_fd(), component, DIRECTORY_FLAGS, Mode::empty())
            .map_err(map_directory_open)?;
        current = File::from(opened);
    }
    Ok(current)
}

fn validate_checkout_root(metadata: &std::fs::Metadata) -> Result<(), LockSnapshotError> {
    if !metadata.is_dir()
        || metadata.uid() != geteuid().as_raw()
        || metadata.gid() != getegid().as_raw()
        || metadata.mode() & 0o022 != 0
    {
        return Err(LockSnapshotError::Unsafe);
    }
    Ok(())
}

fn validate_lockfile(metadata: &std::fs::Metadata) -> Result<(), LockSnapshotError> {
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.uid() != geteuid().as_raw()
        || metadata.gid() != getegid().as_raw()
        || metadata.mode() & 0o022 != 0
        || metadata.len() > MAX_LOCAL_INSTALL_CARGO_LOCK_BYTES as u64
    {
        return Err(LockSnapshotError::Unsafe);
    }
    Ok(())
}

fn stable_metadata(before: &std::fs::Metadata, after: &std::fs::Metadata) -> bool {
    PrivateMetadata::from_metadata(before).same_as(&PrivateMetadata::from_metadata(after))
}

fn valid_checkout_path(path: &Path) -> bool {
    path != Path::new("/")
        && path.is_absolute()
        && path.to_str().is_some()
        && path
            .components()
            .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
}

fn map_directory_open(error: Errno) -> LockSnapshotError {
    match error {
        Errno::NOENT | Errno::LOOP | Errno::NOTDIR => LockSnapshotError::Unsafe,
        _ => LockSnapshotError::Unknown,
    }
}

fn map_lock_open(error: Errno) -> LockSnapshotError {
    match error {
        Errno::NOENT | Errno::LOOP | Errno::NOTDIR | Errno::ISDIR => LockSnapshotError::Unsafe,
        _ => LockSnapshotError::Unknown,
    }
}

fn materialization_id(
    metadata: &std::fs::Metadata,
    generation: ProjectWorkspaceIdentityGeneration,
) -> Sha256Digest {
    project_workspace_filesystem_identity(
        generation,
        ProjectWorkspaceFilesystemIdentityKind::Materialization,
        metadata.dev(),
        metadata.ino(),
        metadata.uid(),
    )
    .expect("validated identity fields always produce canonical SHA-256")
}

fn sha256_digest(bytes: &[u8]) -> Sha256Digest {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    digest_from_hasher(hasher)
}

fn digest_from_hasher(hasher: Sha256) -> Sha256Digest {
    let digest = hasher.finalize();
    let mut value = String::with_capacity(SHA256_PREFIX.len() + digest.len() * 2);
    value.push_str(SHA256_PREFIX);
    for byte in digest {
        value.push(char::from(HEX[usize::from(byte >> 4)]));
        value.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    Sha256Digest::parse(&value).expect("SHA-256 helper emits canonical digest")
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;
    use std::fs;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::artifact::{CommitId, GitTreeId};
    use crate::local_install_plan::LocalInstallToolchainIdentity;
    use crate::process::{CommandExecutor, CommandSpec, ExecutionRecord};

    use super::*;

    const COMMIT: &str = "1111111111111111111111111111111111111111";
    const TREE: &str = "2222222222222222222222222222222222222222";
    const LOCK_BYTES: &[u8] = b"# exact Cargo lock\nversion = 4\n";
    static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

    struct TempCheckout(PathBuf);

    impl TempCheckout {
        fn new(label: &str) -> Self {
            let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "smolrunner-local-install-source-{label}-{}-{sequence}",
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
        mutation: RefCell<Option<(PathBuf, Vec<u8>)>>,
        calls: Cell<usize>,
    }

    impl ScriptedExecutor {
        fn new(responses: Vec<Response>) -> Self {
            Self {
                responses: RefCell::new(responses.into()),
                commands: RefCell::new(Vec::new()),
                mutation: RefCell::new(None),
                calls: Cell::new(0),
            }
        }

        fn with_lock_replacement(self, path: PathBuf, bytes: Vec<u8>) -> Self {
            self.mutation.replace(Some((path, bytes)));
            self
        }
    }

    impl CommandExecutor for ScriptedExecutor {
        fn execute(&self, _spec: &CommandSpec) -> io::Result<ExecutionRecord> {
            panic!("source preflight must use the timed checkout observer")
        }
    }

    impl TimedCommandExecutor for ScriptedExecutor {
        fn execute_with_timeout(
            &self,
            spec: &CommandSpec,
            _timeout: std::time::Duration,
        ) -> io::Result<ExecutionRecord> {
            if self.calls.get() == 0
                && let Some((path, bytes)) = self.mutation.borrow_mut().take()
            {
                let staged = path.with_extension("lock.next");
                fs::write(&staged, bytes).expect("write replacement Cargo.lock");
                fs::rename(staged, path).expect("replace Cargo.lock");
            }
            self.calls.set(self.calls.get() + 1);
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

    fn expected(lock_bytes: &[u8]) -> LocalInstallSourceIdentity {
        LocalInstallSourceIdentity::new(
            CommitId::parse(COMMIT).expect("commit"),
            GitTreeId::parse(TREE).expect("tree"),
            sha256_digest(lock_bytes),
            LocalInstallToolchainIdentity::parse("rust-1.97.1-aarch64-apple-darwin")
                .expect("toolchain"),
        )
        .expect("source")
    }

    fn snapshot_responses(remotes: &str, status: &str, commit: &str, tree: &str) -> Vec<Response> {
        vec![
            Response::success(format!("{commit}\n")),
            Response::success(format!("{tree}\n")),
            Response::success(remotes),
            Response::failed(1, ""),
            Response::success(status),
            Response::success("100644\n"),
            Response::success(
                "worktree /private/path\0HEAD 1111111111111111111111111111111111111111\0branch refs/heads/main\0\0",
            ),
        ]
    }

    fn script(root: &Path, remotes: &str, status: &str, commit: &str, tree: &str) -> Vec<Response> {
        let snapshot = snapshot_responses(remotes, status, commit, tree);
        let mut responses = vec![
            Response::success("false\n"),
            Response::success(format!("{}\n", root.display())),
        ];
        responses.extend(snapshot.clone());
        responses.extend(snapshot);
        responses
    }

    fn exact_script(root: &Path) -> Vec<Response> {
        script(
            root,
            "remote.origin.url\nhttps://github.com/TeamLeaderLeo/Glaeda.git\0",
            &format!("# branch.oid {COMMIT}\0# branch.head (detached)\0"),
            COMMIT,
            TREE,
        )
    }

    fn observer() -> ProjectCheckoutObserver {
        ProjectCheckoutObserver::new("/usr/bin/git").expect("observer")
    }

    #[test]
    fn exact_clean_detached_checkout_is_ready_and_path_private() {
        let checkout = TempCheckout::new("ready");
        let executor = ScriptedExecutor::new(exact_script(checkout.path()));
        let receipt = observe_local_install_source_preflight(
            &expected(LOCK_BYTES),
            checkout.path(),
            &observer(),
            &executor,
        );

        assert!(receipt.ready());
        assert!(receipt.observation_stable());
        assert!(receipt.commit_match());
        assert!(receipt.tree_match());
        assert!(receipt.lockfile_digest_match());
        assert!(receipt.checkout_clean());
        assert_eq!(
            receipt.identity_generation(),
            ProjectWorkspaceIdentityGeneration::GlaedaV2
        );
        assert_eq!(
            receipt.observed_project(),
            LocalInstallSourceProjectDisposition::ExactGlaeda
        );
        assert!(receipt.blocking_codes().is_empty());
        assert_eq!(executor.commands.borrow().len(), 16);
        let public = serde_json::to_string(&receipt).expect("receipt JSON");
        assert!(public.contains("\"schema_version\":3"));
        assert!(public.contains("\"identity_generation\":\"glaeda_v2\""));
        assert!(public.contains("\"local_install_identity_generation\":\"glaeda_v2\""));
        assert!(!public.contains(checkout.path().to_string_lossy().as_ref()));
        assert!(!public.contains("/private/path"));
    }

    #[test]
    fn preflight_binds_lock_and_checkout_observation_to_one_explicit_generation() {
        let checkout = TempCheckout::new("generation");
        let legacy_observer = ProjectCheckoutObserver::with_identity_generation(
            "/usr/bin/git",
            ProjectWorkspaceIdentityGeneration::SmolrunnerV1,
        )
        .expect("legacy observer");
        let legacy_expected = LocalInstallSourceIdentity::with_identity_generation(
            LocalInstallIdentityGeneration::SmolrunnerV1,
            CommitId::parse(COMMIT).expect("commit"),
            GitTreeId::parse(TREE).expect("tree"),
            sha256_digest(LOCK_BYTES),
            LocalInstallToolchainIdentity::parse("rust-1.97.1-aarch64-apple-darwin")
                .expect("toolchain"),
        )
        .expect("legacy source");
        let mut legacy_script = exact_script(checkout.path());
        for response in &mut legacy_script {
            response.stdout = response.stdout.replace(
                "https://github.com/TeamLeaderLeo/Glaeda.git",
                "https://github.com/TeamLeaderLeo/SmolRunner.git",
            );
        }
        let receipt = observe_local_install_source_preflight(
            &legacy_expected,
            checkout.path(),
            &legacy_observer,
            &ScriptedExecutor::new(legacy_script),
        );

        assert!(receipt.ready());
        assert_eq!(
            receipt.identity_generation(),
            ProjectWorkspaceIdentityGeneration::SmolrunnerV1
        );
        let public = serde_json::to_string(&receipt).expect("legacy receipt JSON");
        assert!(public.contains("\"identity_generation\":\"smolrunner_v1\""));
    }

    #[test]
    fn mismatched_install_and_workspace_generations_refuse_before_observation() {
        let checkout = TempCheckout::new("generation-mismatch");
        let legacy_observer = ProjectCheckoutObserver::with_identity_generation(
            "/usr/bin/git",
            ProjectWorkspaceIdentityGeneration::SmolrunnerV1,
        )
        .expect("legacy observer");
        let executor = ScriptedExecutor::new(Vec::new());

        let receipt = observe_local_install_source_preflight(
            &expected(LOCK_BYTES),
            checkout.path(),
            &legacy_observer,
            &executor,
        );

        assert_eq!(
            receipt.blocking_codes(),
            [LocalInstallSourceBlockingCode::IdentityGenerationMismatch]
        );
        assert!(executor.commands.borrow().is_empty());
    }

    #[test]
    fn stable_mismatches_are_distinct_without_private_git_evidence() {
        let checkout = TempCheckout::new("mismatch");
        let remotes = concat!(
            "remote.origin.url\nhttps://github.com/example/fork.git\0",
            "remote.upstream.url\nhttps://github.com/teamleaderleo/smolrunner.git\0"
        );
        let status = format!("# branch.oid {COMMIT}\0# branch.head main\0? secret.txt\0");
        let executor = ScriptedExecutor::new(script(
            checkout.path(),
            remotes,
            &status,
            "3333333333333333333333333333333333333333",
            "4444444444444444444444444444444444444444",
        ));
        let receipt = observe_local_install_source_preflight(
            &expected(b"different expected lock"),
            checkout.path(),
            &observer(),
            &executor,
        );

        assert!(!receipt.ready());
        assert!(receipt.observation_stable());
        assert_eq!(
            receipt.observed_project(),
            LocalInstallSourceProjectDisposition::Ambiguous
        );
        assert_eq!(
            receipt.blocking_codes(),
            [
                LocalInstallSourceBlockingCode::WrongProject,
                LocalInstallSourceBlockingCode::CommitMismatch,
                LocalInstallSourceBlockingCode::TreeMismatch,
                LocalInstallSourceBlockingCode::LockfileMismatch,
                LocalInstallSourceBlockingCode::CheckoutDirty,
            ]
        );
        let public = serde_json::to_string(&receipt).expect("receipt JSON");
        assert!(!public.contains("secret.txt"));
        assert!(!public.contains("example/fork"));
    }

    #[test]
    fn lockfile_replacement_during_git_observation_is_source_changed() {
        let checkout = TempCheckout::new("lock-change");
        let executor = ScriptedExecutor::new(exact_script(checkout.path())).with_lock_replacement(
            checkout.path().join("Cargo.lock"),
            b"replacement Cargo lock\n".to_vec(),
        );
        let receipt = observe_local_install_source_preflight(
            &expected(LOCK_BYTES),
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
    }

    #[test]
    fn missing_or_symlinked_lockfile_is_unsafe_without_git_commands() {
        use std::os::unix::fs::symlink;

        for symlinked in [false, true] {
            let checkout = TempCheckout::new(if symlinked { "symlink" } else { "missing" });
            fs::remove_file(checkout.path().join("Cargo.lock")).expect("remove lock");
            if symlinked {
                fs::write(checkout.path().join("real.lock"), LOCK_BYTES).expect("real lock");
                symlink("real.lock", checkout.path().join("Cargo.lock")).expect("lock symlink");
            }
            let executor = ScriptedExecutor::new(Vec::new());
            let receipt = observe_local_install_source_preflight(
                &expected(LOCK_BYTES),
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
    fn aliased_checkout_root_is_unsafe_without_git_commands() {
        use std::os::unix::fs::symlink;

        let root = TempCheckout::new("alias");
        let alias = root.path().with_extension("alias");
        symlink(root.path(), &alias).expect("checkout alias");
        let executor = ScriptedExecutor::new(Vec::new());
        let receipt = observe_local_install_source_preflight(
            &expected(LOCK_BYTES),
            &alias,
            &observer(),
            &executor,
        );
        assert_eq!(
            receipt.blocking_codes(),
            [LocalInstallSourceBlockingCode::UnsafeSource]
        );
        assert!(executor.commands.borrow().is_empty());
        fs::remove_file(alias).expect("remove alias");
    }
}
