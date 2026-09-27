use std::collections::BTreeSet;
use std::fmt;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use crate::artifact::{CommitId, GitTreeId, Sha256Digest};
use crate::process::{CommandSpec, ExecutionRecord, TimedCommandExecutor};
use crate::project_catalog::{GitHubProjectSource, ProjectIdentity};
use crate::project_workspace_identity::{
    ProjectWorkspaceFilesystemIdentityKind, ProjectWorkspaceIdentityGeneration,
    project_workspace_filesystem_identity,
};
use serde::Serialize;

pub const PROJECT_CHECKOUT_OBSERVATION_SCHEMA_VERSION: u8 = 2;
pub const PROJECT_CHECKOUT_COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
/// Deadline for the reads whose cost scales with the working tree: `status` and `ls-files`.
///
/// `status` stats every tracked file and walks every non-ignored directory. On a Mac with a load
/// average above 100, an 18,000-file checkout's status spent under a second of CPU but 10 to 54
/// seconds of wall time, so the general deadline refused most of a 144-worktree repository as
/// unobservable. A hung Git still fails closed, just later.
pub const PROJECT_CHECKOUT_TREE_SCAN_TIMEOUT: Duration = Duration::from_secs(180);
const _: () = assert!(
    PROJECT_CHECKOUT_TREE_SCAN_TIMEOUT.as_secs() > PROJECT_CHECKOUT_COMMAND_TIMEOUT.as_secs()
);
pub const MAX_PROJECT_CHECKOUT_OUTPUT_BYTES: usize = 65_536;
/// Bound for the one read that scales with index size: one mode line per tracked file.
///
/// At seven bytes per entry the general bound stops at roughly 9,000 tracked files, which refuses
/// ordinary large repositories outright. This matches the process executor's capture ceiling, the
/// real limit, so roughly 150,000 tracked files still observe and larger indexes fail closed. Each
/// line is still validated strictly, so the larger bound buys coverage without loosening what is
/// accepted.
pub const MAX_PROJECT_INDEX_MODE_OUTPUT_BYTES: usize = crate::process::MAX_CAPTURED_STREAM_BYTES;
const _: () = assert!(MAX_PROJECT_INDEX_MODE_OUTPUT_BYTES > MAX_PROJECT_CHECKOUT_OUTPUT_BYTES);
/// Bound for `status`, which prints one record per changed or untracked path. A checkout with a
/// few hundred untracked files is an ordinary refusal, not an unobservable one, so this matches
/// the capture ceiling too.
pub const MAX_PROJECT_STATUS_OUTPUT_BYTES: usize = crate::process::MAX_CAPTURED_STREAM_BYTES;
pub const MAX_PROJECT_REMOTES: usize = 16;
pub const MAX_REMOTE_NAME_BYTES: usize = 100;
pub const MAX_BRANCH_NAME_BYTES: usize = 512;

#[derive(Clone, PartialEq, Eq)]
pub struct ProjectCheckoutLocationIdentity {
    device: u64,
    inode: u64,
    owner: u32,
}

impl ProjectCheckoutLocationIdentity {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            owner: metadata.uid(),
        }
    }

    fn matches(&self, metadata: &std::fs::Metadata) -> bool {
        metadata.is_dir()
            && metadata.dev() == self.device
            && metadata.ino() == self.inode
            && metadata.uid() == self.owner
    }

    fn materialization_id(
        &self,
        generation: ProjectWorkspaceIdentityGeneration,
    ) -> Result<Sha256Digest, ProjectCheckoutObservationError> {
        project_workspace_filesystem_identity(
            generation,
            ProjectWorkspaceFilesystemIdentityKind::Materialization,
            self.device,
            self.inode,
            self.owner,
        )
        .map_err(|_| invalid_output())
    }
}

impl fmt::Debug for ProjectCheckoutLocationIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<private-project-checkout-location>")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ProjectBranchState {
    Attached { name: String },
    Detached,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectRemoteObservation {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectIdentity>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteFreshness {
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectCheckoutObservation {
    schema_version: u8,
    identity_generation: ProjectWorkspaceIdentityGeneration,
    materialization_id: Sha256Digest,
    #[serde(skip_serializing_if = "Option::is_none")]
    primary_project: Option<ProjectIdentity>,
    remotes: Vec<ProjectRemoteObservation>,
    source_ambiguous: bool,
    commit: CommitId,
    tree: GitTreeId,
    branch: ProjectBranchState,
    tracked_changes_present: bool,
    untracked_entry_count: u32,
    upstream_configured: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    local_commits_ahead: Option<u32>,
    linked_worktree_count: u16,
    submodules_present: bool,
    owner_matches_parent: bool,
    remote_freshness: RemoteFreshness,
    #[serde(skip)]
    location_identity: ProjectCheckoutLocationIdentity,
}

impl ProjectCheckoutObservation {
    #[must_use]
    pub const fn identity_generation(&self) -> ProjectWorkspaceIdentityGeneration {
        self.identity_generation
    }

    #[must_use]
    pub const fn materialization_id(&self) -> &Sha256Digest {
        &self.materialization_id
    }

    #[must_use]
    pub const fn primary_project(&self) -> Option<&ProjectIdentity> {
        self.primary_project.as_ref()
    }

    #[must_use]
    pub fn remotes(&self) -> &[ProjectRemoteObservation] {
        &self.remotes
    }

    #[must_use]
    pub const fn source_ambiguous(&self) -> bool {
        self.source_ambiguous
    }

    #[must_use]
    pub const fn commit(&self) -> &CommitId {
        &self.commit
    }

    #[must_use]
    pub const fn tree(&self) -> &GitTreeId {
        &self.tree
    }

    #[must_use]
    pub const fn branch(&self) -> &ProjectBranchState {
        &self.branch
    }

    #[must_use]
    pub const fn tracked_changes_present(&self) -> bool {
        self.tracked_changes_present
    }

    #[must_use]
    pub const fn untracked_entry_count(&self) -> u32 {
        self.untracked_entry_count
    }

    #[must_use]
    pub const fn upstream_configured(&self) -> bool {
        self.upstream_configured
    }

    #[must_use]
    pub const fn local_commits_ahead(&self) -> Option<u32> {
        self.local_commits_ahead
    }

    #[must_use]
    pub const fn linked_worktree_count(&self) -> u16 {
        self.linked_worktree_count
    }

    #[must_use]
    pub const fn submodules_present(&self) -> bool {
        self.submodules_present
    }

    #[must_use]
    pub const fn owner_matches_parent(&self) -> bool {
        self.owner_matches_parent
    }

    #[must_use]
    pub const fn location_identity(&self) -> &ProjectCheckoutLocationIdentity {
        &self.location_identity
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectCheckoutObservationErrorKind {
    NotWorktree,
    BareRepository,
    UnsafePath,
    SourceChanged,
    Unavailable,
    InvalidOutput,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectCheckoutObservationError {
    pub kind: ProjectCheckoutObservationErrorKind,
    pub code: &'static str,
    pub problem: &'static str,
}

impl fmt::Display for ProjectCheckoutObservationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.problem)
    }
}

impl std::error::Error for ProjectCheckoutObservationError {}

#[derive(Clone, PartialEq, Eq)]
pub struct ProjectCheckoutObserver {
    git_program: PathBuf,
    identity_generation: ProjectWorkspaceIdentityGeneration,
    command_timeout: Duration,
}

impl fmt::Debug for ProjectCheckoutObserver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProjectCheckoutObserver")
            .field("git_program", &"<reviewed-absolute-git-program>")
            .field("identity_generation", &self.identity_generation)
            .field("command_timeout", &self.command_timeout)
            .finish()
    }
}

impl ProjectCheckoutObserver {
    /// Create one read-only checkout observer using a reviewed absolute Git executable.
    ///
    /// # Errors
    ///
    /// Returns a bounded error for a relative or aliased executable path.
    pub fn new(git_program: impl Into<PathBuf>) -> Result<Self, ProjectCheckoutObservationError> {
        Self::with_identity_generation(git_program, ProjectWorkspaceIdentityGeneration::CURRENT)
    }

    /// Create one observer for an explicit retained identity generation.
    ///
    /// Fresh observations use [`Self::new`]. The legacy generation exists only to interpret or
    /// reproduce exact historical evidence and grants no adoption or mutation authority.
    ///
    /// # Errors
    ///
    /// Returns a bounded error for a relative or aliased executable path.
    pub fn with_identity_generation(
        git_program: impl Into<PathBuf>,
        identity_generation: ProjectWorkspaceIdentityGeneration,
    ) -> Result<Self, ProjectCheckoutObservationError> {
        let git_program = git_program.into();
        if !is_normalized_absolute_path(&git_program) {
            return Err(unsafe_path());
        }
        Ok(Self {
            git_program,
            identity_generation,
            command_timeout: PROJECT_CHECKOUT_COMMAND_TIMEOUT,
        })
    }

    /// Give every constant-cost read `timeout` instead of [`PROJECT_CHECKOUT_COMMAND_TIMEOUT`].
    ///
    /// For background sweeps, where a slow answer is still useful and a refusal is not: on a host
    /// with a load average above 100 even `rev-list` against a 9,000-ref repository ran past ten
    /// seconds. Tree-scaled reads keep at least [`PROJECT_CHECKOUT_TREE_SCAN_TIMEOUT`].
    #[must_use]
    pub const fn with_command_timeout(mut self, timeout: Duration) -> Self {
        self.command_timeout = timeout;
        self
    }

    fn tree_scan_timeout(&self) -> Duration {
        self.command_timeout.max(PROJECT_CHECKOUT_TREE_SCAN_TIMEOUT)
    }

    #[must_use]
    pub const fn identity_generation(&self) -> ProjectWorkspaceIdentityGeneration {
        self.identity_generation
    }

    /// Observe one exact checkout without network access or repository mutation.
    ///
    /// Dirty files, detached HEAD, missing upstream, forks, and multiple remotes are successful
    /// observations because they are recovery evidence. Two complete snapshots must agree.
    ///
    /// # Errors
    ///
    /// Returns a bounded error for unsafe or aliased paths, non-worktrees, bare repositories,
    /// source drift, unavailable commands, or malformed bounded Git output.
    pub fn observe(
        &self,
        checkout: &Path,
        executor: &impl TimedCommandExecutor,
    ) -> Result<ProjectCheckoutObservation, ProjectCheckoutObservationError> {
        if !is_normalized_absolute_path(checkout) || checkout.to_str().is_none() {
            return Err(unsafe_path());
        }
        let source_metadata = std::fs::symlink_metadata(checkout).map_err(|_| unavailable())?;
        if source_metadata.file_type().is_symlink() || !source_metadata.is_dir() {
            return Err(unsafe_path());
        }
        let canonical = std::fs::canonicalize(checkout).map_err(|_| unavailable())?;
        if canonical.as_path() != checkout {
            return Err(unsafe_path());
        }
        let checkout = canonical;
        let metadata = std::fs::metadata(&checkout).map_err(|_| unavailable())?;
        let parent = checkout.parent().ok_or_else(unsafe_path)?;
        let parent_metadata = std::fs::metadata(parent).map_err(|_| unavailable())?;
        let owner_matches_parent = metadata.uid() == parent_metadata.uid();
        let location_identity = ProjectCheckoutLocationIdentity::from_metadata(&metadata);
        let materialization_id = location_identity.materialization_id(self.identity_generation)?;

        let bare = self.git(&checkout, &["rev-parse", "--is-bare-repository"], executor)?;
        if !bare.success {
            return Err(not_worktree());
        }
        require_success(&bare)?;
        match parse_single_line(&bare.stdout)? {
            "true" => return Err(bare_repository()),
            "false" => {}
            _ => return Err(invalid_output()),
        }

        let top_level = self.git(&checkout, &["rev-parse", "--show-toplevel"], executor)?;
        if !top_level.success {
            return Err(not_worktree());
        }
        require_success(&top_level)?;
        if Path::new(parse_single_line(&top_level.stdout)?) != checkout.as_path() {
            return Err(not_worktree());
        }

        let first = self.snapshot(&checkout, executor)?;
        let second = self.snapshot(&checkout, executor)?;
        let final_metadata = std::fs::metadata(&checkout).map_err(|_| source_changed())?;
        if !first.same_checkout(&second) || !location_identity.matches(&final_metadata) {
            return Err(source_changed());
        }

        Ok(ProjectCheckoutObservation {
            schema_version: PROJECT_CHECKOUT_OBSERVATION_SCHEMA_VERSION,
            identity_generation: self.identity_generation,
            materialization_id,
            primary_project: second.primary_project,
            remotes: second.remotes,
            source_ambiguous: second.source_ambiguous,
            commit: second.commit,
            tree: second.tree,
            branch: second.status.branch,
            tracked_changes_present: second.status.tracked_changes_present,
            untracked_entry_count: second.status.untracked_entry_count,
            upstream_configured: second.status.upstream_configured,
            local_commits_ahead: second.status.local_commits_ahead,
            linked_worktree_count: second.linked_worktree_count,
            submodules_present: second.submodules_present,
            owner_matches_parent,
            remote_freshness: RemoteFreshness::Unknown,
            location_identity,
        })
    }

    fn snapshot(
        &self,
        checkout: &Path,
        executor: &impl TimedCommandExecutor,
    ) -> Result<ProjectCheckoutSnapshot, ProjectCheckoutObservationError> {
        let commit = self.read_commit(checkout, executor)?;
        let tree = self.read_tree(checkout, executor)?;
        let remotes = self.git(
            checkout,
            &[
                "config",
                "--no-includes",
                "--null",
                "--get-regexp",
                "^remote\\..*\\.url$",
            ],
            executor,
        )?;
        let remotes = parse_remotes(&remotes)?;
        let (primary_project, source_ambiguous) = select_primary_project(&remotes);
        let raw_status = self.read_status(checkout, executor)?;
        let status = parse_status(&raw_status)?;
        let modes = self.git_with_limits(
            checkout,
            &["ls-files", "--format=%(objectmode)"],
            MAX_PROJECT_INDEX_MODE_OUTPUT_BYTES,
            self.tree_scan_timeout(),
            executor,
        )?;
        require_success(&modes)?;
        let submodules_present = parse_submodule_presence(&modes.stdout)?;
        let worktrees = self.git(
            checkout,
            &["worktree", "list", "--porcelain", "-z"],
            executor,
        )?;
        require_success(&worktrees)?;
        let linked_worktree_count = parse_worktree_count(&worktrees.stdout)?;

        Ok(ProjectCheckoutSnapshot {
            commit,
            tree,
            remotes,
            primary_project,
            source_ambiguous,
            status,
            raw_status,
            linked_worktree_count,
            submodules_present,
        })
    }

    fn read_commit(
        &self,
        checkout: &Path,
        executor: &impl TimedCommandExecutor,
    ) -> Result<CommitId, ProjectCheckoutObservationError> {
        let record = self.git(
            checkout,
            &["rev-parse", "--verify", "HEAD^{commit}"],
            executor,
        )?;
        require_success(&record)?;
        CommitId::parse(parse_single_line(&record.stdout)?).map_err(|_| invalid_output())
    }

    fn read_tree(
        &self,
        checkout: &Path,
        executor: &impl TimedCommandExecutor,
    ) -> Result<GitTreeId, ProjectCheckoutObservationError> {
        let record = self.git(
            checkout,
            &["rev-parse", "--verify", "HEAD^{tree}"],
            executor,
        )?;
        require_success(&record)?;
        GitTreeId::parse(parse_single_line(&record.stdout)?).map_err(|_| invalid_output())
    }

    fn read_status(
        &self,
        checkout: &Path,
        executor: &impl TimedCommandExecutor,
    ) -> Result<String, ProjectCheckoutObservationError> {
        let filters = self.git(
            checkout,
            &[
                "config",
                "--includes",
                "--get-regexp",
                "^filter\\..*\\.(clean|process)$",
            ],
            executor,
        )?;
        if filters.success
            || filters.status != Some(1)
            || !filters.stdout.is_empty()
            || !filters.stderr.is_empty()
        {
            return Err(unavailable());
        }

        let record = self.git_with_limits(
            checkout,
            &[
                "status",
                "--porcelain=v2",
                "--branch",
                "-z",
                "--untracked-files=all",
                "--ignore-submodules=all",
            ],
            MAX_PROJECT_STATUS_OUTPUT_BYTES,
            self.tree_scan_timeout(),
            executor,
        )?;
        require_success(&record)?;
        Ok(record.stdout)
    }

    /// Run one hardened, credentialless, lock-free Git command beneath `checkout`.
    ///
    /// Shared with sibling read-only observers so every Git read uses the same environment and
    /// output bounds.
    pub(crate) fn git(
        &self,
        checkout: &Path,
        arguments: &[&str],
        executor: &impl TimedCommandExecutor,
    ) -> Result<ExecutionRecord, ProjectCheckoutObservationError> {
        self.git_bounded(
            checkout,
            arguments,
            MAX_PROJECT_CHECKOUT_OUTPUT_BYTES,
            executor,
        )
    }

    /// Like [`Self::git`] with an explicit stdout bound for reads that scale with repository size.
    pub(crate) fn git_bounded(
        &self,
        checkout: &Path,
        arguments: &[&str],
        max_stdout_bytes: usize,
        executor: &impl TimedCommandExecutor,
    ) -> Result<ExecutionRecord, ProjectCheckoutObservationError> {
        self.git_with_limits(
            checkout,
            arguments,
            max_stdout_bytes,
            self.command_timeout,
            executor,
        )
    }

    /// Like [`Self::git_bounded`] with an explicit deadline, for reviewed commands whose runtime
    /// scales with the size of the tree they act on.
    pub(crate) fn git_with_limits(
        &self,
        checkout: &Path,
        arguments: &[&str],
        max_stdout_bytes: usize,
        timeout: Duration,
        executor: &impl TimedCommandExecutor,
    ) -> Result<ExecutionRecord, ProjectCheckoutObservationError> {
        let checkout = checkout.to_str().ok_or_else(unsafe_path)?;
        let mut spec = CommandSpec::new(&self.git_program)
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
            .argument(checkout)
            .environment("GIT_CONFIG_NOSYSTEM", "1")
            .environment("GIT_CONFIG_GLOBAL", "/dev/null")
            .environment("GIT_NO_REPLACE_OBJECTS", "1")
            .environment("GIT_NO_LAZY_FETCH", "1")
            .environment("GIT_TERMINAL_PROMPT", "0")
            .environment("GIT_ASKPASS", "/bin/false")
            .environment("GIT_ALLOW_PROTOCOL", "")
            .environment("GIT_PROTOCOL_FROM_USER", "0")
            .environment("LC_ALL", "C")
            .environment("LANG", "C");
        for argument in arguments {
            spec = spec.argument(*argument);
        }
        let expected_argv = spec.displayed_argv();
        let expected_environment_keys = spec.environment.keys().cloned().collect::<Vec<_>>();
        let record = executor
            .execute_with_timeout(&spec, timeout)
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::TimedOut {
                    timed_out()
                } else {
                    unavailable()
                }
            })?;
        if record.argv != expected_argv
            || record.environment_keys != expected_environment_keys
            || record.stdout.len() > max_stdout_bytes
            || record.stderr.len() > MAX_PROJECT_CHECKOUT_OUTPUT_BYTES
            || record.stdout.contains('\u{fffd}')
            || record.stderr.contains('\u{fffd}')
            || record.stdout.contains('\r')
        {
            return Err(invalid_output());
        }
        Ok(record)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProjectCheckoutSnapshot {
    commit: CommitId,
    tree: GitTreeId,
    remotes: Vec<ProjectRemoteObservation>,
    primary_project: Option<ProjectIdentity>,
    source_ambiguous: bool,
    status: ParsedStatus,
    raw_status: String,
    linked_worktree_count: u16,
    submodules_present: bool,
}

impl ProjectCheckoutSnapshot {
    /// Whether two snapshots saw the same checkout.
    ///
    /// Repository-wide facts are left out: another session adding or removing a worktree changes
    /// the worktree count, and any fetch moves the upstream and with it the ahead/behind line.
    /// Neither changes this checkout, and during a long sweep both happen between snapshots.
    fn same_checkout(&self, other: &Self) -> bool {
        self.commit == other.commit
            && self.tree == other.tree
            && self.remotes == other.remotes
            && self.primary_project == other.primary_project
            && self.source_ambiguous == other.source_ambiguous
            && self.submodules_present == other.submodules_present
            && self.status.branch == other.status.branch
            && self.status.tracked_changes_present == other.status.tracked_changes_present
            && self.status.untracked_entry_count == other.status.untracked_entry_count
            && self.status.upstream_configured == other.status.upstream_configured
            && local_status_records(&self.raw_status).eq(local_status_records(&other.raw_status))
    }
}

/// Status records without the ahead/behind line, which only an upstream move changes.
fn local_status_records(raw_status: &str) -> impl Iterator<Item = &str> {
    raw_status
        .split('\0')
        .filter(|record| !record.starts_with("# branch.ab "))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedStatus {
    branch: ProjectBranchState,
    tracked_changes_present: bool,
    untracked_entry_count: u32,
    upstream_configured: bool,
    local_commits_ahead: Option<u32>,
}

fn parse_remotes(
    record: &ExecutionRecord,
) -> Result<Vec<ProjectRemoteObservation>, ProjectCheckoutObservationError> {
    if !record.success {
        if record.status == Some(1) && record.stdout.is_empty() {
            return Ok(Vec::new());
        }
        return Err(invalid_output());
    }
    if record.status != Some(0) || !record.stderr.is_empty() {
        return Err(invalid_output());
    }
    let mut remotes = Vec::new();
    for entry in record.stdout.split_terminator('\0') {
        let Some((key, url)) = entry.split_once('\n') else {
            return Err(invalid_output());
        };
        let Some(name) = key
            .strip_prefix("remote.")
            .and_then(|key| key.strip_suffix(".url"))
        else {
            return Err(invalid_output());
        };
        if !valid_remote_name(name) || remotes.len() >= MAX_PROJECT_REMOTES {
            return Err(invalid_output());
        }
        let project = GitHubProjectSource::parse(url)
            .ok()
            .map(|source| source.project().clone());
        remotes.push(ProjectRemoteObservation {
            name: name.to_owned(),
            project,
        });
    }
    if !record.stdout.is_empty() && !record.stdout.ends_with('\0') {
        return Err(invalid_output());
    }
    remotes.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then(left.project.cmp(&right.project))
    });
    Ok(remotes)
}

fn select_primary_project(remotes: &[ProjectRemoteObservation]) -> (Option<ProjectIdentity>, bool) {
    let projects = remotes
        .iter()
        .filter_map(|remote| remote.project.clone())
        .collect::<BTreeSet<_>>();
    let origin = remotes
        .iter()
        .filter(|remote| remote.name == "origin")
        .collect::<Vec<_>>();
    let primary = if origin.len() == 1 {
        origin[0].project.clone()
    } else if projects.len() == 1 {
        projects.iter().next().cloned()
    } else {
        None
    };
    (primary, projects.len() > 1)
}

fn parse_status(value: &str) -> Result<ParsedStatus, ProjectCheckoutObservationError> {
    let mut branch = None;
    let mut upstream_configured = false;
    let mut local_commits_ahead = None;
    let mut tracked_changes_present = false;
    let mut untracked_entry_count = 0_u32;
    let mut skip_rename_source = false;

    for entry in value.split_terminator('\0') {
        if skip_rename_source {
            skip_rename_source = false;
            continue;
        }
        if let Some(head) = entry.strip_prefix("# branch.head ") {
            branch = Some(if head == "(detached)" {
                ProjectBranchState::Detached
            } else {
                validate_branch_name(head)?;
                ProjectBranchState::Attached {
                    name: head.to_owned(),
                }
            });
        } else if entry.starts_with("# branch.upstream ") {
            upstream_configured = true;
        } else if let Some(ab) = entry.strip_prefix("# branch.ab +") {
            let Some((ahead, behind)) = ab.split_once(" -") else {
                return Err(invalid_output());
            };
            let ahead = ahead.parse::<u32>().map_err(|_| invalid_output())?;
            let _behind = behind.parse::<u32>().map_err(|_| invalid_output())?;
            local_commits_ahead = Some(ahead);
        } else if entry.starts_with("1 ") || entry.starts_with("u ") {
            tracked_changes_present = true;
        } else if entry.starts_with("2 ") {
            tracked_changes_present = true;
            skip_rename_source = true;
        } else if entry.starts_with("? ") {
            untracked_entry_count = untracked_entry_count
                .checked_add(1)
                .ok_or_else(invalid_output)?;
        } else if entry.starts_with("! ") || entry.starts_with("# branch.oid ") {
        } else {
            return Err(invalid_output());
        }
    }
    let branch = branch.ok_or_else(invalid_output)?;
    if !upstream_configured && local_commits_ahead.is_some() {
        return Err(invalid_output());
    }
    Ok(ParsedStatus {
        branch,
        tracked_changes_present,
        untracked_entry_count,
        upstream_configured,
        local_commits_ahead,
    })
}

fn parse_submodule_presence(value: &str) -> Result<bool, ProjectCheckoutObservationError> {
    if !value.is_empty() && !value.ends_with('\n') {
        return Err(invalid_output());
    }
    let mut present = false;
    for mode in value.lines() {
        match mode {
            "100644" | "100755" | "120000" => {}
            "160000" => present = true,
            _ => return Err(invalid_output()),
        }
    }
    Ok(present)
}

fn parse_worktree_count(value: &str) -> Result<u16, ProjectCheckoutObservationError> {
    if !value.is_empty() && !value.ends_with('\0') {
        return Err(invalid_output());
    }
    let count = value
        .split_terminator('\0')
        .filter(|entry| entry.starts_with("worktree "))
        .count();
    u16::try_from(count).map_err(|_| invalid_output())
}

fn validate_branch_name(value: &str) -> Result<(), ProjectCheckoutObservationError> {
    if value.is_empty()
        || value.len() > MAX_BRANCH_NAME_BYTES
        || value.contains("..")
        || value.contains("@{")
        || value.starts_with('.')
        || value.ends_with('.')
        || value.ends_with('/')
        || value.bytes().any(|byte| {
            byte.is_ascii_control()
                || byte == b' '
                || matches!(byte, b'~' | b'^' | b':' | b'?' | b'*' | b'[' | b'\\')
        })
    {
        Err(invalid_output())
    } else {
        Ok(())
    }
}

fn valid_remote_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_REMOTE_NAME_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn parse_single_line(value: &str) -> Result<&str, ProjectCheckoutObservationError> {
    let line = value.strip_suffix('\n').unwrap_or(value);
    if line.is_empty() || line.contains('\n') || line.contains('\0') {
        Err(invalid_output())
    } else {
        Ok(line)
    }
}

fn require_success(record: &ExecutionRecord) -> Result<(), ProjectCheckoutObservationError> {
    if record.success && record.status == Some(0) && record.stderr.is_empty() {
        Ok(())
    } else {
        Err(unavailable())
    }
}

fn is_normalized_absolute_path(path: &Path) -> bool {
    path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
}

fn error(
    kind: ProjectCheckoutObservationErrorKind,
    code: &'static str,
    problem: &'static str,
) -> ProjectCheckoutObservationError {
    ProjectCheckoutObservationError {
        kind,
        code,
        problem,
    }
}

fn not_worktree() -> ProjectCheckoutObservationError {
    error(
        ProjectCheckoutObservationErrorKind::NotWorktree,
        "not_worktree",
        "candidate is not an exact Git worktree root",
    )
}

fn bare_repository() -> ProjectCheckoutObservationError {
    error(
        ProjectCheckoutObservationErrorKind::BareRepository,
        "bare_repository",
        "candidate is a bare Git repository",
    )
}

fn unsafe_path() -> ProjectCheckoutObservationError {
    error(
        ProjectCheckoutObservationErrorKind::UnsafePath,
        "unsafe_path",
        "candidate path is unsafe for checkout observation",
    )
}

fn source_changed() -> ProjectCheckoutObservationError {
    error(
        ProjectCheckoutObservationErrorKind::SourceChanged,
        "source_changed",
        "checkout changed during observation",
    )
}

fn unavailable() -> ProjectCheckoutObservationError {
    error(
        ProjectCheckoutObservationErrorKind::Unavailable,
        "observation_unavailable",
        "checkout observation is unavailable",
    )
}

/// Kept under the `Unavailable` kind, since callers treat both alike; the code tells them apart.
fn timed_out() -> ProjectCheckoutObservationError {
    error(
        ProjectCheckoutObservationErrorKind::Unavailable,
        "observation_timed_out",
        "a Git read did not finish before its deadline",
    )
}

/// The deadline [`ProjectCheckoutObserver`] gives the Git command `spec`, for scripted executors.
#[cfg(test)]
pub(crate) fn expected_git_timeout(spec: &CommandSpec) -> Duration {
    let argv = spec.displayed_argv();
    if argv
        .iter()
        .any(|argument| argument == "status" || argument == "ls-files")
    {
        PROJECT_CHECKOUT_TREE_SCAN_TIMEOUT
    } else {
        PROJECT_CHECKOUT_COMMAND_TIMEOUT
    }
}

fn invalid_output() -> ProjectCheckoutObservationError {
    error(
        ProjectCheckoutObservationErrorKind::InvalidOutput,
        "invalid_git_output",
        "Git returned invalid or untrusted observation output",
    )
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::fs;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::process::{CommandExecutor, CommandSpec, ExecutionRecord, TimedCommandExecutor};
    use crate::project_workspace_identity::ProjectWorkspaceIdentityGeneration;

    use super::{
        ProjectBranchState, ProjectCheckoutObservationErrorKind, ProjectCheckoutObserver,
        expected_git_timeout,
    };

    const COMMIT: &str = "1111111111111111111111111111111111111111";
    const TREE: &str = "2222222222222222222222222222222222222222";
    static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

    struct TempDirectory(PathBuf);

    impl TempDirectory {
        fn new(label: &str) -> Self {
            let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "smolrunner-project-checkout-{label}-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create temporary checkout");
            Self(fs::canonicalize(path).expect("canonical temporary checkout"))
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDirectory {
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
            panic!("checkout observation must use the timed executor")
        }
    }

    impl TimedCommandExecutor for ScriptedExecutor {
        fn execute_with_timeout(
            &self,
            spec: &CommandSpec,
            timeout: std::time::Duration,
        ) -> io::Result<ExecutionRecord> {
            assert_eq!(timeout, expected_git_timeout(spec));
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

    fn observer() -> ProjectCheckoutObserver {
        ProjectCheckoutObserver::new("/usr/bin/git").expect("observer")
    }

    fn snapshot_responses(
        remotes: &str,
        status: &str,
        modes: &str,
        worktrees: &str,
    ) -> Vec<Response> {
        vec![
            Response::success(format!("{COMMIT}\n")),
            Response::success(format!("{TREE}\n")),
            Response::success(remotes),
            Response::failed(1, ""),
            Response::success(status),
            Response::success(modes),
            Response::success(worktrees),
        ]
    }

    fn script(
        root: &Path,
        remotes: &str,
        status: &str,
        modes: &str,
        worktrees: &str,
    ) -> Vec<Response> {
        let snapshot = snapshot_responses(remotes, status, modes, worktrees);
        let mut responses = vec![
            Response::success("false\n"),
            Response::success(format!("{}\n", root.display())),
        ];
        responses.extend(snapshot.clone());
        responses.extend(snapshot);
        responses
    }

    struct TimingOutExecutor;

    impl CommandExecutor for TimingOutExecutor {
        fn execute(&self, _spec: &CommandSpec) -> io::Result<ExecutionRecord> {
            panic!("checkout observation must use the timed executor")
        }
    }

    impl TimedCommandExecutor for TimingOutExecutor {
        fn execute_with_timeout(
            &self,
            _spec: &CommandSpec,
            _timeout: std::time::Duration,
        ) -> io::Result<ExecutionRecord> {
            Err(io::Error::from(io::ErrorKind::TimedOut))
        }
    }

    #[test]
    fn deadline_is_reported_apart_from_other_failures() {
        let checkout = TempDirectory::new("deadline");
        let error = observer()
            .git(checkout.path(), &["status"], &TimingOutExecutor)
            .expect_err("timed out");
        assert_eq!(error.kind, ProjectCheckoutObservationErrorKind::Unavailable);
        assert_eq!(error.code, "observation_timed_out");
    }

    #[test]
    fn clean_checkout_reports_project_without_private_path() {
        let checkout = TempDirectory::new("clean");
        let remotes = "remote.origin.url\nhttps://github.com/TeamLeaderLeo/SmolRunner.git\0";
        let status = format!(
            "# branch.oid {COMMIT}\0# branch.head main\0# branch.upstream origin/main\0# branch.ab +0 -0\0"
        );
        let worktrees = format!(
            "worktree {}\0HEAD {COMMIT}\0branch refs/heads/main\0\0",
            checkout.path().display()
        );
        let executor = ScriptedExecutor::new(script(
            checkout.path(),
            remotes,
            &status,
            "100644\n100755\n",
            &worktrees,
        ));
        let observation = observer()
            .observe(checkout.path(), &executor)
            .expect("checkout observation");

        assert_eq!(
            observation.primary_project().expect("project").as_str(),
            "github.com/teamleaderleo/smolrunner"
        );
        assert_eq!(observation.commit().as_str(), COMMIT);
        assert_eq!(observation.tree().as_str(), TREE);
        assert!(!observation.source_ambiguous());
        assert!(!observation.tracked_changes_present());
        assert_eq!(observation.local_commits_ahead(), Some(0));
        assert_eq!(observation.linked_worktree_count(), 1);
        assert_eq!(
            observation.identity_generation(),
            ProjectWorkspaceIdentityGeneration::GlaedaV2
        );
        let json = serde_json::to_string(&observation).expect("public JSON");
        assert!(json.contains("\"schema_version\":2"));
        assert!(json.contains("\"identity_generation\":\"glaeda_v2\""));
        assert!(!json.contains(checkout.path().to_string_lossy().as_ref()));
        assert!(
            !format!("{:?}", observation.location_identity())
                .contains(checkout.path().to_string_lossy().as_ref())
        );
        assert_eq!(executor.commands.borrow().len(), 16);
    }

    #[test]
    fn explicit_legacy_generation_reproduces_a_distinct_materialization_identity() {
        let checkout = TempDirectory::new("generation");
        let status = format!("# branch.oid {COMMIT}\0# branch.head main\0");
        let worktrees = format!("worktree {}\0HEAD {COMMIT}\0\0", checkout.path().display());
        let responses = script(checkout.path(), "", &status, "100644\n", &worktrees);
        let current = observer()
            .observe(checkout.path(), &ScriptedExecutor::new(responses.clone()))
            .expect("current observation");
        let legacy_observer = ProjectCheckoutObserver::with_identity_generation(
            "/usr/bin/git",
            ProjectWorkspaceIdentityGeneration::SmolrunnerV1,
        )
        .expect("legacy observer");
        let legacy = legacy_observer
            .observe(checkout.path(), &ScriptedExecutor::new(responses))
            .expect("legacy observation");

        assert_eq!(
            current.identity_generation(),
            ProjectWorkspaceIdentityGeneration::GlaedaV2
        );
        assert_eq!(
            legacy.identity_generation(),
            ProjectWorkspaceIdentityGeneration::SmolrunnerV1
        );
        assert_ne!(current.materialization_id(), legacy.materialization_id());
        let json = serde_json::to_string(&legacy).expect("legacy JSON");
        assert!(json.contains("\"identity_generation\":\"smolrunner_v1\""));
    }

    #[test]
    fn dirty_fork_and_local_ahead_are_recovery_evidence() {
        let checkout = TempDirectory::new("dirty-fork");
        let remotes = concat!(
            "remote.origin.url\nhttps://github.com/example/fork.git\0",
            "remote.upstream.url\nhttps://github.com/upstream/project.git\0"
        );
        let status = format!(
            "# branch.oid {COMMIT}\0# branch.head feature/x\0# branch.upstream origin/feature/x\0# branch.ab +3 -1\01 .M N... 100644 100644 100644 {TREE} {TREE} file.txt\0? secret.txt\0"
        );
        let executor = ScriptedExecutor::new(script(
            checkout.path(),
            remotes,
            &status,
            "100644\n160000\n",
            "worktree /private/path\0HEAD 1111111111111111111111111111111111111111\0\0worktree /private/other\0HEAD 1111111111111111111111111111111111111111\0\0",
        ));
        let observation = observer()
            .observe(checkout.path(), &executor)
            .expect("dirty checkout is observable");

        assert_eq!(
            observation
                .primary_project()
                .expect("origin project")
                .as_str(),
            "github.com/example/fork"
        );
        assert!(observation.source_ambiguous());
        assert!(observation.tracked_changes_present());
        assert_eq!(observation.untracked_entry_count(), 1);
        assert_eq!(observation.local_commits_ahead(), Some(3));
        assert_eq!(observation.linked_worktree_count(), 2);
        assert!(observation.submodules_present());
        assert!(matches!(
            observation.branch(),
            ProjectBranchState::Attached { name } if name == "feature/x"
        ));
        let json = serde_json::to_string(&observation).expect("public JSON");
        assert!(!json.contains("file.txt"));
        assert!(!json.contains("secret.txt"));
        assert!(!json.contains("/private/path"));
    }

    #[test]
    fn configured_upstream_without_tracking_ref_has_unknown_ahead_count() {
        let checkout = TempDirectory::new("missing-upstream-ref");
        let status = format!(
            "# branch.oid {COMMIT}\0# branch.head retained/topic\0# branch.upstream origin/removed-topic\0"
        );
        let executor = ScriptedExecutor::new(script(
            checkout.path(),
            "remote.origin.url\nhttps://github.com/example/project.git\0",
            &status,
            "100644\n",
            "worktree /private/path\0HEAD 1111111111111111111111111111111111111111\0\0",
        ));

        let observation = observer()
            .observe(checkout.path(), &executor)
            .expect("configured upstream without a tracking ref remains observable");

        assert!(observation.upstream_configured());
        assert_eq!(observation.local_commits_ahead(), None);
        assert!(matches!(
            observation.branch(),
            ProjectBranchState::Attached { name } if name == "retained/topic"
        ));
    }

    #[test]
    fn detached_and_non_git_states_are_bounded() {
        let detached = TempDirectory::new("detached");
        let status = format!("# branch.oid {COMMIT}\0# branch.head (detached)\0");
        let executor = ScriptedExecutor::new(script(
            detached.path(),
            "",
            &status,
            "100644\n",
            "worktree /private/path\0HEAD 1111111111111111111111111111111111111111\0detached\0\0",
        ));
        let observation = observer()
            .observe(detached.path(), &executor)
            .expect("detached checkout");
        assert!(matches!(observation.branch(), ProjectBranchState::Detached));
        assert!(!observation.upstream_configured());
        assert_eq!(observation.local_commits_ahead(), None);

        let ordinary = TempDirectory::new("ordinary-directory");
        let executor = ScriptedExecutor::new(vec![Response::failed(128, "fatal: private path")]);
        let error = observer()
            .observe(ordinary.path(), &executor)
            .expect_err("non Git directory");
        assert_eq!(error.kind, ProjectCheckoutObservationErrorKind::NotWorktree);
        assert!(
            !serde_json::to_string(&error)
                .expect("error JSON")
                .contains("private path")
        );
    }

    #[test]
    fn configured_content_filter_is_refused_before_status() {
        let checkout = TempDirectory::new("content-filter");
        let executor = ScriptedExecutor::new(vec![
            Response::success("false\n"),
            Response::success(format!("{}\n", checkout.path().display())),
            Response::success(format!("{COMMIT}\n")),
            Response::success(format!("{TREE}\n")),
            Response::failed(1, ""),
            Response::success("filter.pwn.clean /tmp/payload\n"),
        ]);

        let error = observer()
            .observe(checkout.path(), &executor)
            .expect_err("configured content filter must fail closed");
        assert_eq!(error.kind, ProjectCheckoutObservationErrorKind::Unavailable);
        assert!(!executor.commands.borrow().iter().any(|command| {
            command
                .displayed_argv()
                .iter()
                .any(|argument| argument == "status")
        }));
    }

    #[test]
    fn bare_symlink_and_alias_candidates_are_classified() {
        let bare = TempDirectory::new("bare");
        let executor = ScriptedExecutor::new(vec![Response::success("true\n")]);
        let error = observer()
            .observe(bare.path(), &executor)
            .expect_err("bare repository");
        assert_eq!(
            error.kind,
            ProjectCheckoutObservationErrorKind::BareRepository
        );

        use std::os::unix::fs::symlink;
        let root = TempDirectory::new("symlink-root");
        let target = root.path().join("target");
        fs::create_dir(&target).expect("target directory");
        let link = root.path().join("link");
        symlink(&target, &link).expect("symlink");
        let executor = ScriptedExecutor::new(Vec::new());
        let error = observer()
            .observe(&link, &executor)
            .expect_err("symlink refused");
        assert_eq!(error.kind, ProjectCheckoutObservationErrorKind::UnsafePath);
        assert!(executor.commands.borrow().is_empty());

        let actual_parent = root.path().join("actual-parent");
        let actual_checkout = actual_parent.join("checkout");
        fs::create_dir_all(&actual_checkout).expect("actual checkout");
        let alias_parent = root.path().join("alias-parent");
        symlink(&actual_parent, &alias_parent).expect("ancestor alias");
        let aliased_checkout = alias_parent.join("checkout");
        let executor = ScriptedExecutor::new(Vec::new());
        let error = observer()
            .observe(&aliased_checkout, &executor)
            .expect_err("ancestor symlink refused");
        assert_eq!(error.kind, ProjectCheckoutObservationErrorKind::UnsafePath);
        assert!(executor.commands.borrow().is_empty());
    }

    #[test]
    fn repository_wide_drift_between_snapshots_is_not_a_checkout_change() {
        let checkout = TempDirectory::new("repository-drift");
        let remotes = "remote.origin.url\nhttps://github.com/example/project.git\0";
        let status = |behind: u32| {
            format!(
                "# branch.oid {COMMIT}\0# branch.head main\0# branch.upstream origin/main\0# branch.ab +0 -{behind}\0"
            )
        };
        let one = "worktree /private/main\0HEAD 1111111111111111111111111111111111111111\0branch refs/heads/main\0\0";
        let two = format!("{one}worktree /private/other\0HEAD {COMMIT}\0detached\0\0");
        let mut responses = vec![
            Response::success("false\n"),
            Response::success(format!("{}\n", checkout.path().display())),
        ];
        // Between the snapshots another session adds a worktree and a fetch moves the upstream.
        responses.extend(snapshot_responses(remotes, &status(0), "100644\n", one));
        responses.extend(snapshot_responses(remotes, &status(3), "100644\n", &two));
        let executor = ScriptedExecutor::new(responses);
        let observation = observer()
            .observe(checkout.path(), &executor)
            .expect("stable checkout");
        assert_eq!(observation.linked_worktree_count(), 2);
    }

    #[test]
    fn sweep_deadline_raises_constant_cost_reads_only_up_to_tree_scans() {
        let observer = observer().with_command_timeout(std::time::Duration::from_secs(60));
        assert_eq!(observer.command_timeout, std::time::Duration::from_secs(60));
        assert_eq!(
            observer.tree_scan_timeout(),
            super::PROJECT_CHECKOUT_TREE_SCAN_TIMEOUT
        );
    }

    #[test]
    fn changed_second_snapshot_fails_closed() {
        let checkout = TempDirectory::new("source-change");
        let remotes = "remote.origin.url\nhttps://github.com/example/project.git\0";
        let status = format!("# branch.oid {COMMIT}\0# branch.head main\0");
        let worktrees = "worktree /private/path\0HEAD 1111111111111111111111111111111111111111\0branch refs/heads/main\0\0";
        let mut responses = vec![
            Response::success("false\n"),
            Response::success(format!("{}\n", checkout.path().display())),
        ];
        responses.extend(snapshot_responses(remotes, &status, "100644\n", worktrees));
        responses.extend(snapshot_responses(
            "remote.origin.url\nhttps://github.com/example/changed.git\0",
            &status,
            "100644\n",
            worktrees,
        ));
        let executor = ScriptedExecutor::new(responses);
        let error = observer()
            .observe(checkout.path(), &executor)
            .expect_err("changed snapshot");
        assert_eq!(
            error.kind,
            ProjectCheckoutObservationErrorKind::SourceChanged
        );
    }
}
