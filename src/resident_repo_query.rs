//! One fixed, bounded `repo-query/v1` evidence bundle for exact candidate review.
//!
//! Git owns repository semantics. This module contributes an explicit scrubbed process boundary,
//! exact object coordinates, aggregate limits, and a compact typed result. It performs no fetch,
//! checkout, ref resolution, mutation, publication, or result-reuse decision.

use std::fmt;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::artifact::{CommitId, GitTreeId};
use crate::process::{CommandSpec, ExecutionRecord, TimedCommandExecutor};
use crate::project_catalog::{GitHubProjectSource, ProjectIdentity};

pub const RESIDENT_REPO_QUERY_SCHEMA_VERSION: u8 = 1;
pub const RESIDENT_REPO_QUERY_PROFILE_ID: &str = "repo-query/v1";
pub const DEFAULT_PATCH_BYTES: usize = 16 * 1024;
pub const MAX_PATCH_BYTES: usize = 64 * 1024;
pub const MAX_CHANGED_FILES: usize = 64;
pub const MAX_GIT_OUTPUT_BYTES: usize = 1024 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 128 * 1024;
pub const MAX_AUXILIARY_QUERIES: usize = 8;
pub const MAX_GREP_MATCHES: usize = 64;
pub const MAX_BLOB_BYTES: usize = 16 * 1024;
pub const MAX_HISTORY_COMMITS: usize = 32;
pub const REPO_QUERY_COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

const MAX_PATH_BYTES: usize = 1024;
const MAX_LITERAL_BYTES: usize = 512;
const MAX_MATCH_TEXT_BYTES: usize = 1024;
const MAX_SUBJECT_BYTES: usize = 512;

const PROFILE_CONTRACT: &[u8] = concat!(
    "glaeda-repo-query-profile-v1\0",
    "exact-commit-objects\0merge-base\0ancestry\0commit-count\0",
    "numstat-no-renames\0bounded-complete-patch\0literal-tree-grep\0",
    "bounded-tree-blobs\0path-history\0object-info\0object-format\0network-disabled\0",
    "checkout-and-git-directory-identity-stable\0origin-reobserved\0"
)
.as_bytes();

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResidentRepoQueryRequest {
    repository: ProjectIdentity,
    base: CommitId,
    head: CommitId,
    head_tree: GitTreeId,
    max_patch_bytes: usize,
    grep: Option<ResidentRepoGrepRequest>,
    blob_paths: Vec<String>,
    blob_max_bytes: usize,
    history_paths: Vec<String>,
    history_max_commits: usize,
    object_oids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResidentRepoGrepRequest {
    literal: String,
    paths: Vec<String>,
    max_matches: usize,
}

impl ResidentRepoQueryRequest {
    /// Define one fixed exact-candidate review bundle.
    ///
    /// # Errors
    ///
    /// Returns a bounded error when the patch inclusion ceiling exceeds the v1 contract.
    pub fn new(
        repository: ProjectIdentity,
        base: CommitId,
        head: CommitId,
        head_tree: GitTreeId,
        max_patch_bytes: usize,
    ) -> Result<Self, ResidentRepoQueryError> {
        if max_patch_bytes > MAX_PATCH_BYTES {
            return Err(ResidentRepoQueryError::new(
                "patch_limit_invalid",
                "patch inclusion limit exceeds the repo-query/v1 ceiling",
            ));
        }
        if base.as_str().len() != head.as_str().len()
            || head.as_str().len() != head_tree.as_str().len()
        {
            return Err(unsafe_input());
        }
        Ok(Self {
            repository,
            base,
            head,
            head_tree,
            max_patch_bytes,
            grep: None,
            blob_paths: Vec::new(),
            blob_max_bytes: MAX_BLOB_BYTES,
            history_paths: Vec::new(),
            history_max_commits: MAX_HISTORY_COMMITS,
            object_oids: Vec::new(),
        })
    }

    /// Add one literal exact-head-tree search to this bundle.
    ///
    /// # Errors
    ///
    /// Refuses an empty/oversized/control-bearing literal, unsafe path, too many paths, or a match
    /// ceiling outside the fixed profile.
    pub fn with_exact_tree_grep(
        mut self,
        literal: impl Into<String>,
        paths: Vec<String>,
        max_matches: usize,
    ) -> Result<Self, ResidentRepoQueryError> {
        let literal = literal.into();
        if literal.is_empty()
            || literal.len() > MAX_LITERAL_BYTES
            || literal.bytes().any(|byte| byte.is_ascii_control())
            || paths.len() > MAX_AUXILIARY_QUERIES
            || max_matches == 0
            || max_matches > MAX_GREP_MATCHES
            || paths.iter().any(|path| !valid_request_path(path))
        {
            return Err(unsafe_input());
        }
        self.grep = Some(ResidentRepoGrepRequest {
            literal,
            paths,
            max_matches,
        });
        Ok(self)
    }

    /// Add exact-head-tree blob reads with one shared fixed byte ceiling per blob.
    ///
    /// # Errors
    ///
    /// Refuses too many paths, duplicates, unsafe paths, or a byte ceiling outside the profile.
    pub fn with_blob_reads(
        mut self,
        paths: Vec<String>,
        max_bytes: usize,
    ) -> Result<Self, ResidentRepoQueryError> {
        validate_distinct_paths(&paths)?;
        if max_bytes == 0 || max_bytes > MAX_BLOB_BYTES {
            return Err(unsafe_input());
        }
        self.blob_paths = paths;
        self.blob_max_bytes = max_bytes;
        Ok(self)
    }

    /// Add exact-head history queries for literal repository-relative paths.
    ///
    /// # Errors
    ///
    /// Refuses too many paths, duplicates, unsafe paths, or a history ceiling outside the profile.
    pub fn with_path_history(
        mut self,
        paths: Vec<String>,
        max_commits: usize,
    ) -> Result<Self, ResidentRepoQueryError> {
        validate_distinct_paths(&paths)?;
        if max_commits == 0 || max_commits > MAX_HISTORY_COMMITS {
            return Err(unsafe_input());
        }
        self.history_paths = paths;
        self.history_max_commits = max_commits;
        Ok(self)
    }

    /// Add exact object existence/info questions.
    ///
    /// # Errors
    ///
    /// Refuses too many, duplicate, abbreviated, uppercase, or mixed-format object IDs.
    pub fn with_object_info(mut self, oids: Vec<String>) -> Result<Self, ResidentRepoQueryError> {
        if oids.is_empty() || oids.len() > MAX_AUXILIARY_QUERIES {
            return Err(unsafe_input());
        }
        let mut seen = std::collections::BTreeSet::new();
        if oids.iter().any(|oid| {
            oid.len() != self.head.as_str().len() || !valid_oid(oid) || !seen.insert(oid.clone())
        }) {
            return Err(unsafe_input());
        }
        self.object_oids = oids;
        Ok(self)
    }

    #[must_use]
    pub const fn repository(&self) -> &ProjectIdentity {
        &self.repository
    }

    #[must_use]
    pub const fn base(&self) -> &CommitId {
        &self.base
    }

    #[must_use]
    pub const fn head(&self) -> &CommitId {
        &self.head
    }

    #[must_use]
    pub const fn head_tree(&self) -> &GitTreeId {
        &self.head_tree
    }

    #[must_use]
    pub const fn max_patch_bytes(&self) -> usize {
        self.max_patch_bytes
    }

    #[must_use]
    pub fn digest(&self) -> String {
        let mut bytes = Vec::new();
        append_field(&mut bytes, self.repository.as_str());
        append_field(&mut bytes, self.base.as_str());
        append_field(&mut bytes, self.head.as_str());
        append_field(&mut bytes, self.head_tree.as_str());
        append_field(&mut bytes, &self.max_patch_bytes.to_string());
        append_field(&mut bytes, "grep");
        if let Some(grep) = &self.grep {
            append_field(&mut bytes, &grep.literal);
            for path in &grep.paths {
                append_field(&mut bytes, path);
            }
            append_field(&mut bytes, &grep.max_matches.to_string());
        }
        append_field(&mut bytes, "blobs");
        for path in &self.blob_paths {
            append_field(&mut bytes, path);
        }
        append_field(&mut bytes, &self.blob_max_bytes.to_string());
        append_field(&mut bytes, "history");
        for path in &self.history_paths {
            append_field(&mut bytes, path);
        }
        append_field(&mut bytes, &self.history_max_commits.to_string());
        append_field(&mut bytes, "objects");
        for oid in &self.object_oids {
            append_field(&mut bytes, oid);
        }
        append_field(&mut bytes, &profile_generation());
        sha256_digest(&bytes)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResidentRepoChangedFile {
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    insertions: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    deletions: Option<u64>,
    binary: bool,
}

impl ResidentRepoChangedFile {
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    #[must_use]
    pub const fn insertions(&self) -> Option<u64> {
        self.insertions
    }

    #[must_use]
    pub const fn deletions(&self) -> Option<u64> {
        self.deletions
    }

    #[must_use]
    pub const fn binary(&self) -> bool {
        self.binary
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResidentRepoDiffSummary {
    files_changed: u32,
    text_files: u32,
    binary_files: u32,
    insertions: u64,
    deletions: u64,
}

impl ResidentRepoDiffSummary {
    #[must_use]
    pub const fn files_changed(&self) -> u32 {
        self.files_changed
    }

    #[must_use]
    pub const fn insertions(&self) -> u64 {
        self.insertions
    }

    #[must_use]
    pub const fn deletions(&self) -> u64 {
        self.deletions
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResidentRepoPatchEvidence {
    bytes: u32,
    sha256: String,
    included: bool,
    omitted_bytes: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
}

impl ResidentRepoPatchEvidence {
    #[must_use]
    pub const fn included(&self) -> bool {
        self.included
    }

    #[must_use]
    pub const fn bytes(&self) -> u32 {
        self.bytes
    }

    #[must_use]
    pub fn digest(&self) -> &str {
        &self.sha256
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResidentRepoEvidenceStatus {
    Complete,
    Truncated,
    Unknown,
}

impl ResidentRepoEvidenceStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Truncated => "truncated",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResidentRepoGrepMatch {
    path: String,
    line: u64,
    text: String,
    text_truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResidentRepoGrepEvidence {
    status: ResidentRepoEvidenceStatus,
    tree_oid: GitTreeId,
    matches: Vec<ResidentRepoGrepMatch>,
    observed_matches: u32,
    omitted_matches: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResidentRepoBlobEvidence {
    status: ResidentRepoEvidenceStatus,
    tree_oid: GitTreeId,
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    blob_oid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    size_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    omitted_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResidentRepoTouchingCommit {
    oid: CommitId,
    committed_at_unix_seconds: u64,
    subject: String,
    subject_truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResidentRepoPathHistoryEvidence {
    status: ResidentRepoEvidenceStatus,
    start_commit_oid: CommitId,
    path: String,
    commits: Vec<ResidentRepoTouchingCommit>,
    observed_commits: u32,
    omitted_commits_at_least: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResidentRepoObjectEvidence {
    status: ResidentRepoEvidenceStatus,
    oid: String,
    exists: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    object_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    size_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResidentRepoQueryMetrics {
    git_processes: u16,
    git_stdout_bytes: u64,
    git_wall_microseconds: u64,
    complete_wall_microseconds: u64,
}

impl ResidentRepoQueryMetrics {
    #[must_use]
    pub const fn git_processes(&self) -> u16 {
        self.git_processes
    }

    #[must_use]
    pub const fn git_stdout_bytes(&self) -> u64 {
        self.git_stdout_bytes
    }

    #[must_use]
    pub const fn complete_wall_microseconds(&self) -> u64 {
        self.complete_wall_microseconds
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResidentRepoQueryReport {
    document_type: &'static str,
    schema_version: u8,
    profile_id: &'static str,
    profile_generation: String,
    authority: &'static str,
    request_digest: String,
    repository: ProjectIdentity,
    object_format: &'static str,
    requested_base: CommitId,
    head: CommitId,
    head_tree: GitTreeId,
    merge_base: CommitId,
    base_is_ancestor: bool,
    commits_since_merge_base: u32,
    changed_files: Vec<ResidentRepoChangedFile>,
    changed_files_status: ResidentRepoEvidenceStatus,
    changed_files_observed: u32,
    changed_files_omitted: u32,
    diff_summary: ResidentRepoDiffSummary,
    patch: ResidentRepoPatchEvidence,
    #[serde(skip_serializing_if = "Option::is_none")]
    grep: Option<ResidentRepoGrepEvidence>,
    blobs: Vec<ResidentRepoBlobEvidence>,
    path_history: Vec<ResidentRepoPathHistoryEvidence>,
    objects: Vec<ResidentRepoObjectEvidence>,
    metrics: ResidentRepoQueryMetrics,
}

impl ResidentRepoQueryReport {
    #[must_use]
    pub const fn repository(&self) -> &ProjectIdentity {
        &self.repository
    }

    #[must_use]
    pub const fn head(&self) -> &CommitId {
        &self.head
    }

    #[must_use]
    pub const fn head_tree(&self) -> &GitTreeId {
        &self.head_tree
    }

    #[must_use]
    pub const fn merge_base(&self) -> &CommitId {
        &self.merge_base
    }

    #[must_use]
    pub const fn base_is_ancestor(&self) -> bool {
        self.base_is_ancestor
    }

    #[must_use]
    pub fn changed_files(&self) -> &[ResidentRepoChangedFile] {
        &self.changed_files
    }

    #[must_use]
    pub const fn diff_summary(&self) -> &ResidentRepoDiffSummary {
        &self.diff_summary
    }

    #[must_use]
    pub const fn patch(&self) -> &ResidentRepoPatchEvidence {
        &self.patch
    }

    #[must_use]
    pub const fn metrics(&self) -> &ResidentRepoQueryMetrics {
        &self.metrics
    }

    #[must_use]
    pub fn render_human(&self) -> String {
        let mut output = format!(
            concat!(
                "repo query: profile={} authority={} project={}\n",
                "source: base={} head={} tree={} merge_base={} ancestor={}\n",
                "diff: files={} insertions={} deletions={} patch_bytes={} patch_included={}\n",
                "execution: git_processes={} git_stdout_bytes={} wall_us={}\n"
            ),
            self.profile_id,
            self.authority,
            self.repository.as_str(),
            self.requested_base.as_str(),
            self.head.as_str(),
            self.head_tree.as_str(),
            self.merge_base.as_str(),
            self.base_is_ancestor,
            self.diff_summary.files_changed,
            self.diff_summary.insertions,
            self.diff_summary.deletions,
            self.patch.bytes,
            self.patch.included,
            self.metrics.git_processes,
            self.metrics.git_stdout_bytes,
            self.metrics.complete_wall_microseconds,
        );
        for file in &self.changed_files {
            output.push_str(&format!("changed: {}\n", file.path));
        }
        if let Some(grep) = &self.grep {
            output.push_str(&format!(
                "grep: status={} matches={} omitted={}\n",
                grep.status.as_str(),
                grep.matches.len(),
                grep.omitted_matches,
            ));
        }
        for blob in &self.blobs {
            output.push_str(&format!(
                "blob: status={} path={} size={} omitted={}\n",
                blob.status.as_str(),
                blob.path,
                blob.size_bytes.unwrap_or(0),
                blob.omitted_bytes,
            ));
        }
        for history in &self.path_history {
            output.push_str(&format!(
                "history: status={} path={} commits={} omitted={}\n",
                history.status.as_str(),
                history.path,
                history.commits.len(),
                history.omitted_commits_at_least,
            ));
        }
        for object in &self.objects {
            output.push_str(&format!(
                "object: status={} oid={} exists={} type={} size={}\n",
                object.status.as_str(),
                object.oid,
                object.exists,
                object.object_type.as_deref().unwrap_or("unknown"),
                object.size_bytes.unwrap_or(0),
            ));
        }
        if let Some(patch) = &self.patch.text {
            output.push_str("patch:\n");
            output.push_str(patch);
            if !patch.ends_with('\n') {
                output.push('\n');
            }
        }
        output
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResidentRepoQueryError {
    pub code: &'static str,
    pub problem: &'static str,
}

impl ResidentRepoQueryError {
    const fn new(code: &'static str, problem: &'static str) -> Self {
        Self { code, problem }
    }
}

impl fmt::Display for ResidentRepoQueryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.problem)
    }
}

impl std::error::Error for ResidentRepoQueryError {}

#[derive(Clone, PartialEq, Eq)]
struct CheckoutIdentity {
    device: u64,
    inode: u64,
    owner: u32,
}

impl CheckoutIdentity {
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
}

impl fmt::Debug for CheckoutIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<private-checkout-identity>")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResidentRepoQueryObserver {
    git_program: PathBuf,
}

impl ResidentRepoQueryObserver {
    /// Create the fixed Git process boundary from one reviewed absolute program.
    ///
    /// # Errors
    ///
    /// Returns a bounded error for a relative, root, or non-normalized program path.
    pub fn new(git_program: impl Into<PathBuf>) -> Result<Self, ResidentRepoQueryError> {
        let git_program = git_program.into();
        if !valid_absolute_path(&git_program) {
            return Err(unsafe_input());
        }
        Ok(Self { git_program })
    }

    /// Execute the fixed read-only bundle against exact objects already present locally.
    ///
    /// # Errors
    ///
    /// Returns a bounded path-private error for unsafe inputs, unavailable or malformed Git
    /// evidence, identity drift, missing objects, repository mismatch, or exceeded limits.
    pub fn observe(
        &self,
        checkout: &Path,
        request: &ResidentRepoQueryRequest,
        executor: &impl TimedCommandExecutor,
    ) -> Result<ResidentRepoQueryReport, ResidentRepoQueryError> {
        let complete_started = Instant::now();
        let (checkout, checkout_identity) = validate_checkout(checkout)?;
        let mut git = GitRunner::new(&self.git_program, &checkout, executor)?;

        let top_level = git.success(&["rev-parse", "--show-toplevel"])?;
        if single_line(&top_level.stdout)? != checkout.to_str().ok_or_else(unsafe_input)? {
            return Err(repository_mismatch());
        }

        let object_format = git.success(&["rev-parse", "--show-object-format"])?;
        let object_format = match single_line(&object_format.stdout)? {
            "sha1" if request.head().as_str().len() == 40 => "sha1",
            "sha256" if request.head().as_str().len() == 64 => "sha256",
            _ => return Err(source_mismatch()),
        };

        let git_directory = git.success(&["rev-parse", "--absolute-git-dir"])?;
        let git_directory = validate_private_git_directory(single_line(&git_directory.stdout)?)?;
        let git_directory_metadata =
            std::fs::metadata(&git_directory).map_err(|_| source_changed())?;
        let git_directory_identity = CheckoutIdentity::from_metadata(&git_directory_metadata);

        let origin = git.success(&["config", "--no-includes", "--get", "remote.origin.url"])?;
        let origin_value = single_line(&origin.stdout)?.to_owned();
        let origin =
            GitHubProjectSource::parse(&origin_value).map_err(|_| repository_mismatch())?;
        if origin.project() != request.repository() {
            return Err(repository_mismatch());
        }

        require_commit_object(&mut git, request.base())?;
        require_commit_object(&mut git, request.head())?;

        let observed_head_tree = git.success(&[
            "rev-parse",
            "--verify",
            &format!("{}^{{tree}}", request.head().as_str()),
        ])?;
        let observed_head_tree = GitTreeId::parse(single_line(&observed_head_tree.stdout)?)
            .map_err(|_| invalid_output())?;
        if &observed_head_tree != request.head_tree() {
            return Err(source_mismatch());
        }
        let head_tree = request.head_tree().clone();

        let merge_base = git.success(&[
            "merge-base",
            request.base().as_str(),
            request.head().as_str(),
        ])?;
        let merge_base =
            CommitId::parse(single_line(&merge_base.stdout)?).map_err(|_| invalid_output())?;

        let ancestry = git.run(&[
            "merge-base",
            "--is-ancestor",
            request.base().as_str(),
            request.head().as_str(),
        ])?;
        if !ancestry.stdout.is_empty()
            || !ancestry.stderr.is_empty()
            || !matches!(ancestry.status, Some(0 | 1))
        {
            return Err(invalid_output());
        }
        let base_is_ancestor = ancestry.status == Some(0);

        let commit_count = git.success(&[
            "rev-list",
            "--count",
            &format!("{}..{}", merge_base.as_str(), request.head().as_str()),
        ])?;
        let commits_since_merge_base = single_line(&commit_count.stdout)?
            .parse::<u32>()
            .map_err(|_| invalid_output())?;

        let numstat = git.success(&[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
            "--numstat",
            "-z",
            merge_base.as_str(),
            request.head().as_str(),
        ])?;
        let (changed_files, diff_summary) = parse_numstat(&numstat.stdout)?;

        let patch = git.success(&[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
            "--no-color",
            "--unified=3",
            merge_base.as_str(),
            request.head().as_str(),
        ])?;
        let patch = build_patch_evidence(&patch.stdout, request.max_patch_bytes())?;

        let grep = request
            .grep
            .as_ref()
            .map(|query| observe_grep(&mut git, &head_tree, query));
        let blobs = request
            .blob_paths
            .iter()
            .map(|path| observe_blob(&mut git, &head_tree, path, request.blob_max_bytes))
            .collect::<Vec<_>>();
        let path_history = request
            .history_paths
            .iter()
            .map(|path| {
                observe_path_history(&mut git, request.head(), path, request.history_max_commits)
            })
            .collect::<Vec<_>>();
        let objects = request
            .object_oids
            .iter()
            .map(|oid| observe_object(&mut git, oid))
            .collect::<Vec<_>>();

        let final_metadata = std::fs::metadata(&checkout).map_err(|_| source_changed())?;
        let final_git_directory_metadata =
            std::fs::metadata(&git_directory).map_err(|_| source_changed())?;
        let final_origin =
            git.success(&["config", "--no-includes", "--get", "remote.origin.url"])?;
        if !checkout_identity.matches(&final_metadata)
            || !git_directory_identity.matches(&final_git_directory_metadata)
            || single_line(&final_origin.stdout)? != origin_value
        {
            return Err(source_changed());
        }

        let complete_wall_microseconds = bounded_micros(complete_started.elapsed());
        let changed_files_observed = diff_summary.files_changed;
        let changed_files_omitted = changed_files_observed
            .saturating_sub(u32::try_from(changed_files.len()).unwrap_or(u32::MAX));
        let mut report = ResidentRepoQueryReport {
            document_type: "glaeda-resident-repo-query",
            schema_version: RESIDENT_REPO_QUERY_SCHEMA_VERSION,
            profile_id: RESIDENT_REPO_QUERY_PROFILE_ID,
            profile_generation: profile_generation(),
            authority: "observation_only",
            request_digest: request.digest(),
            repository: request.repository.clone(),
            object_format,
            requested_base: request.base.clone(),
            head: request.head.clone(),
            head_tree,
            merge_base,
            base_is_ancestor,
            commits_since_merge_base,
            changed_files,
            changed_files_status: if changed_files_omitted == 0 {
                ResidentRepoEvidenceStatus::Complete
            } else {
                ResidentRepoEvidenceStatus::Truncated
            },
            changed_files_observed,
            changed_files_omitted,
            diff_summary,
            patch,
            grep,
            blobs,
            path_history,
            objects,
            metrics: ResidentRepoQueryMetrics {
                git_processes: git.processes,
                git_stdout_bytes: git.stdout_bytes,
                git_wall_microseconds: git.wall_microseconds,
                complete_wall_microseconds,
            },
        };
        compact_report(&mut report)?;
        Ok(report)
    }
}

struct GitRunner<'a, Executor> {
    git_program: &'a Path,
    checkout: &'a Path,
    executor: &'a Executor,
    processes: u16,
    stdout_bytes: u64,
    wall_microseconds: u64,
}

impl<'a, Executor: TimedCommandExecutor> GitRunner<'a, Executor> {
    fn new(
        git_program: &'a Path,
        checkout: &'a Path,
        executor: &'a Executor,
    ) -> Result<Self, ResidentRepoQueryError> {
        if checkout.to_str().is_none() {
            return Err(unsafe_input());
        }
        Ok(Self {
            git_program,
            checkout,
            executor,
            processes: 0,
            stdout_bytes: 0,
            wall_microseconds: 0,
        })
    }

    fn success(&mut self, arguments: &[&str]) -> Result<ExecutionRecord, ResidentRepoQueryError> {
        let record = self.run(arguments)?;
        if !record.success || record.status != Some(0) || !record.stderr.is_empty() {
            return Err(query_unavailable());
        }
        Ok(record)
    }

    fn run(&mut self, arguments: &[&str]) -> Result<ExecutionRecord, ResidentRepoQueryError> {
        let checkout = self.checkout.to_str().ok_or_else(unsafe_input)?;
        let mut spec = CommandSpec::new(self.git_program)
            .argument("--no-pager")
            .argument("--no-optional-locks")
            .argument("--literal-pathspecs")
            .argument("-c")
            .argument("credential.helper=")
            .argument("-c")
            .argument("core.fsmonitor=false")
            .argument("-c")
            .argument("core.hooksPath=/dev/null")
            .argument("-c")
            .argument("diff.external=")
            .argument("-c")
            .argument("core.pager=cat")
            .argument("-c")
            .argument("color.ui=false")
            .argument("-c")
            .argument("submodule.recurse=false")
            .argument("-C")
            .argument(checkout)
            .environment("GIT_CONFIG_NOSYSTEM", "1")
            .environment("GIT_CONFIG_GLOBAL", "/dev/null")
            .environment("GIT_ATTR_NOSYSTEM", "1")
            .environment("GIT_NO_REPLACE_OBJECTS", "1")
            .environment("GIT_NO_LAZY_FETCH", "1")
            .environment("GIT_TERMINAL_PROMPT", "0")
            .environment("GIT_ASKPASS", "/bin/false")
            .environment("GIT_ALLOW_PROTOCOL", "")
            .environment("GIT_PROTOCOL_FROM_USER", "0")
            .environment("GIT_PAGER", "cat")
            .environment("GIT_EXTERNAL_DIFF", "")
            .environment("LC_ALL", "C")
            .environment("LANG", "C");
        for argument in arguments {
            spec = spec.argument(*argument);
        }
        let expected_argv = spec.displayed_argv();
        let expected_environment_keys = spec.environment.keys().cloned().collect::<Vec<_>>();
        let started = Instant::now();
        let record = self
            .executor
            .execute_with_timeout(&spec, REPO_QUERY_COMMAND_TIMEOUT)
            .map_err(|_| query_unavailable())?;
        self.processes = self.processes.checked_add(1).ok_or_else(limit_exceeded)?;
        self.stdout_bytes = self
            .stdout_bytes
            .checked_add(record.stdout.len() as u64)
            .ok_or_else(limit_exceeded)?;
        self.wall_microseconds = self
            .wall_microseconds
            .saturating_add(bounded_micros(started.elapsed()));
        if record.argv != expected_argv
            || record.environment_keys != expected_environment_keys
            || record.stdout.len() > MAX_GIT_OUTPUT_BYTES
            || record.stderr.len() > MAX_GIT_OUTPUT_BYTES
            || record.stdout.contains('\u{fffd}')
            || record.stderr.contains('\u{fffd}')
        {
            return Err(invalid_output());
        }
        Ok(record)
    }
}

fn require_commit_object(
    git: &mut GitRunner<'_, impl TimedCommandExecutor>,
    object: &CommitId,
) -> Result<(), ResidentRepoQueryError> {
    let record = git.success(&["cat-file", "-t", object.as_str()])?;
    if single_line(&record.stdout)? != "commit" {
        return Err(object_unavailable());
    }
    Ok(())
}

fn observe_grep(
    git: &mut GitRunner<'_, impl TimedCommandExecutor>,
    tree: &GitTreeId,
    query: &ResidentRepoGrepRequest,
) -> ResidentRepoGrepEvidence {
    let mut owned = vec![
        "grep".to_owned(),
        "-F".to_owned(),
        "-n".to_owned(),
        "-z".to_owned(),
        "-I".to_owned(),
        "--full-name".to_owned(),
        "-e".to_owned(),
        query.literal.clone(),
        tree.as_str().to_owned(),
        "--".to_owned(),
    ];
    owned.extend(query.paths.iter().cloned());
    let arguments = owned.iter().map(String::as_str).collect::<Vec<_>>();
    let record = match git.run(&arguments) {
        Ok(record) => record,
        Err(_) => return unknown_grep(tree, "git_observation_unavailable"),
    };
    if record.status == Some(1) && record.stdout.is_empty() && record.stderr.is_empty() {
        return ResidentRepoGrepEvidence {
            status: ResidentRepoEvidenceStatus::Complete,
            tree_oid: tree.clone(),
            matches: Vec::new(),
            observed_matches: 0,
            omitted_matches: 0,
            reason: None,
        };
    }
    let Some(mut matches) = parse_grep_matches(&record, tree.as_str()) else {
        return unknown_grep(tree, "git_output_invalid");
    };
    let observed_matches = u32::try_from(matches.len()).unwrap_or(u32::MAX);
    matches.truncate(query.max_matches);
    let returned = u32::try_from(matches.len()).unwrap_or(u32::MAX);
    let omitted_matches = observed_matches.saturating_sub(returned);
    ResidentRepoGrepEvidence {
        status: if omitted_matches == 0 {
            ResidentRepoEvidenceStatus::Complete
        } else {
            ResidentRepoEvidenceStatus::Truncated
        },
        tree_oid: tree.clone(),
        matches,
        observed_matches,
        omitted_matches,
        reason: (omitted_matches > 0).then_some("match_limit"),
    }
}

fn unknown_grep(tree: &GitTreeId, reason: &'static str) -> ResidentRepoGrepEvidence {
    ResidentRepoGrepEvidence {
        status: ResidentRepoEvidenceStatus::Unknown,
        tree_oid: tree.clone(),
        matches: Vec::new(),
        observed_matches: 0,
        omitted_matches: 0,
        reason: Some(reason),
    }
}

fn observe_blob(
    git: &mut GitRunner<'_, impl TimedCommandExecutor>,
    tree: &GitTreeId,
    path: &str,
    max_bytes: usize,
) -> ResidentRepoBlobEvidence {
    let entry = match git.run(&["ls-tree", "-z", "--full-tree", tree.as_str(), "--", path]) {
        Ok(record) => record,
        Err(_) => return unknown_blob(tree, path, "git_observation_unavailable"),
    };
    let Some(blob_oid) = parse_exact_blob_entry(&entry, path) else {
        return unknown_blob(tree, path, "path_missing_or_not_blob");
    };
    let size = match git.run(&["cat-file", "-s", &blob_oid]) {
        Ok(record) => parse_u64_output(&record),
        Err(_) => None,
    };
    let Some(size_bytes) = size else {
        return unknown_blob(tree, path, "blob_size_unavailable");
    };
    if size_bytes > max_bytes as u64 {
        return ResidentRepoBlobEvidence {
            status: ResidentRepoEvidenceStatus::Truncated,
            tree_oid: tree.clone(),
            path: path.to_owned(),
            blob_oid: Some(blob_oid),
            size_bytes: Some(size_bytes),
            text: None,
            omitted_bytes: size_bytes,
            reason: Some("blob_byte_limit"),
        };
    }
    let content = match git.run(&["cat-file", "blob", &blob_oid]) {
        Ok(record) if record.success && record.status == Some(0) && record.stderr.is_empty() => {
            record.stdout
        }
        _ => return unknown_blob(tree, path, "blob_text_unavailable"),
    };
    ResidentRepoBlobEvidence {
        status: ResidentRepoEvidenceStatus::Complete,
        tree_oid: tree.clone(),
        path: path.to_owned(),
        blob_oid: Some(blob_oid),
        size_bytes: Some(size_bytes),
        text: Some(content),
        omitted_bytes: 0,
        reason: None,
    }
}

fn unknown_blob(tree: &GitTreeId, path: &str, reason: &'static str) -> ResidentRepoBlobEvidence {
    ResidentRepoBlobEvidence {
        status: ResidentRepoEvidenceStatus::Unknown,
        tree_oid: tree.clone(),
        path: path.to_owned(),
        blob_oid: None,
        size_bytes: None,
        text: None,
        omitted_bytes: 0,
        reason: Some(reason),
    }
}

fn observe_path_history(
    git: &mut GitRunner<'_, impl TimedCommandExecutor>,
    start: &CommitId,
    path: &str,
    max_commits: usize,
) -> ResidentRepoPathHistoryEvidence {
    let max_plus_one = max_commits.saturating_add(1).to_string();
    let record = match git.run(&[
        "log",
        "-z",
        "--no-decorate",
        "--no-show-signature",
        "--format=%H%x00%ct%x00%s",
        &format!("--max-count={max_plus_one}"),
        start.as_str(),
        "--",
        path,
    ]) {
        Ok(record) => record,
        Err(_) => return unknown_history(start, path, "git_observation_unavailable"),
    };
    let Some(mut commits) = parse_history_commits(&record, start.as_str().len()) else {
        return unknown_history(start, path, "git_output_invalid");
    };
    let observed_commits = u32::try_from(commits.len()).unwrap_or(u32::MAX);
    commits.truncate(max_commits);
    let returned = u32::try_from(commits.len()).unwrap_or(u32::MAX);
    let omitted_commits_at_least = observed_commits.saturating_sub(returned);
    ResidentRepoPathHistoryEvidence {
        status: if omitted_commits_at_least == 0 {
            ResidentRepoEvidenceStatus::Complete
        } else {
            ResidentRepoEvidenceStatus::Truncated
        },
        start_commit_oid: start.clone(),
        path: path.to_owned(),
        commits,
        observed_commits,
        omitted_commits_at_least,
        reason: (omitted_commits_at_least > 0).then_some("commit_limit"),
    }
}

fn unknown_history(
    start: &CommitId,
    path: &str,
    reason: &'static str,
) -> ResidentRepoPathHistoryEvidence {
    ResidentRepoPathHistoryEvidence {
        status: ResidentRepoEvidenceStatus::Unknown,
        start_commit_oid: start.clone(),
        path: path.to_owned(),
        commits: Vec::new(),
        observed_commits: 0,
        omitted_commits_at_least: 0,
        reason: Some(reason),
    }
}

fn observe_object(
    git: &mut GitRunner<'_, impl TimedCommandExecutor>,
    oid: &str,
) -> ResidentRepoObjectEvidence {
    let object_type = match git.run(&["cat-file", "-t", oid]) {
        Ok(record)
            if record.status == Some(128)
                && !record.success
                && record.stdout.is_empty()
                && record.stderr == "fatal: git cat-file: could not get object info\n" =>
        {
            return ResidentRepoObjectEvidence {
                status: ResidentRepoEvidenceStatus::Complete,
                oid: oid.to_owned(),
                exists: false,
                object_type: None,
                size_bytes: None,
                reason: None,
            };
        }
        Ok(record) if record.success && record.status == Some(0) && record.stderr.is_empty() => {
            match single_line(&record.stdout) {
                Ok(value) if matches!(value, "blob" | "tree" | "commit" | "tag") => {
                    value.to_owned()
                }
                _ => return unknown_object(oid, "git_output_invalid"),
            }
        }
        _ => return unknown_object(oid, "git_observation_unavailable"),
    };
    let size_bytes = match git.run(&["cat-file", "-s", oid]) {
        Ok(record) => parse_u64_output(&record),
        Err(_) => None,
    };
    let Some(size_bytes) = size_bytes else {
        return unknown_object(oid, "object_size_unavailable");
    };
    ResidentRepoObjectEvidence {
        status: ResidentRepoEvidenceStatus::Complete,
        oid: oid.to_owned(),
        exists: true,
        object_type: Some(object_type),
        size_bytes: Some(size_bytes),
        reason: None,
    }
}

fn unknown_object(oid: &str, reason: &'static str) -> ResidentRepoObjectEvidence {
    ResidentRepoObjectEvidence {
        status: ResidentRepoEvidenceStatus::Unknown,
        oid: oid.to_owned(),
        exists: false,
        object_type: None,
        size_bytes: None,
        reason: Some(reason),
    }
}

fn parse_grep_matches(
    record: &ExecutionRecord,
    tree_oid: &str,
) -> Option<Vec<ResidentRepoGrepMatch>> {
    if !record.success || record.status != Some(0) || !record.stderr.is_empty() {
        return None;
    }
    let prefix = format!("{tree_oid}:");
    let mut remainder = record.stdout.as_str();
    let mut matches = Vec::new();
    while !remainder.is_empty() {
        let path_end = remainder.find('\0')?;
        let path = remainder[..path_end].strip_prefix(&prefix)?;
        remainder = &remainder[path_end + 1..];
        let line_end = remainder.find('\0')?;
        let line = remainder[..line_end].parse::<u64>().ok()?;
        remainder = &remainder[line_end + 1..];
        let text_end = remainder.find('\n').unwrap_or(remainder.len());
        let text = &remainder[..text_end];
        remainder = if text_end == remainder.len() {
            ""
        } else {
            &remainder[text_end + 1..]
        };
        if !valid_repo_output_path(path) || line == 0 {
            return None;
        }
        let (text, text_truncated) = truncate_utf8(text, MAX_MATCH_TEXT_BYTES);
        matches.push(ResidentRepoGrepMatch {
            path: path.to_owned(),
            line,
            text,
            text_truncated,
        });
    }
    Some(matches)
}

fn parse_exact_blob_entry(record: &ExecutionRecord, expected_path: &str) -> Option<String> {
    if !record.success
        || record.status != Some(0)
        || !record.stderr.is_empty()
        || !record.stdout.ends_with('\0')
    {
        return None;
    }
    let mut entries = record.stdout.split_terminator('\0');
    let entry = entries.next()?;
    if entries.next().is_some() {
        return None;
    }
    let (metadata, path) = entry.split_once('\t')?;
    let mut fields = metadata.split(' ');
    let mode = fields.next()?;
    let kind = fields.next()?;
    let oid = fields.next()?;
    if fields.next().is_some()
        || path != expected_path
        || !matches!(mode, "100644" | "100755" | "120000")
        || kind != "blob"
        || !valid_oid(oid)
    {
        return None;
    }
    Some(oid.to_owned())
}

fn parse_history_commits(
    record: &ExecutionRecord,
    oid_len: usize,
) -> Option<Vec<ResidentRepoTouchingCommit>> {
    if !record.success
        || record.status != Some(0)
        || !record.stderr.is_empty()
        || (!record.stdout.is_empty() && !record.stdout.ends_with('\0'))
    {
        return None;
    }
    let fields = record.stdout.split_terminator('\0').collect::<Vec<_>>();
    if fields.len() % 3 != 0 {
        return None;
    }
    let mut commits = Vec::with_capacity(fields.len() / 3);
    for fields in fields.chunks_exact(3) {
        if fields[0].len() != oid_len || !valid_oid(fields[0]) {
            return None;
        }
        let oid = CommitId::parse(fields[0]).ok()?;
        let committed_at_unix_seconds = fields[1].parse::<u64>().ok()?;
        if fields[2].contains(['\n', '\r', '\0']) {
            return None;
        }
        let (subject, subject_truncated) = truncate_utf8(fields[2], MAX_SUBJECT_BYTES);
        commits.push(ResidentRepoTouchingCommit {
            oid,
            committed_at_unix_seconds,
            subject,
            subject_truncated,
        });
    }
    Some(commits)
}

fn parse_u64_output(record: &ExecutionRecord) -> Option<u64> {
    if record.success && record.status == Some(0) && record.stderr.is_empty() {
        single_line(&record.stdout).ok()?.parse::<u64>().ok()
    } else {
        None
    }
}

fn truncate_utf8(value: &str, max_bytes: usize) -> (String, bool) {
    if value.len() <= max_bytes {
        return (value.to_owned(), false);
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    (value[..end].to_owned(), true)
}

fn validate_distinct_paths(paths: &[String]) -> Result<(), ResidentRepoQueryError> {
    if paths.is_empty() || paths.len() > MAX_AUXILIARY_QUERIES {
        return Err(unsafe_input());
    }
    let mut seen = std::collections::BTreeSet::new();
    if paths
        .iter()
        .any(|path| !valid_request_path(path) || !seen.insert(path.clone()))
    {
        return Err(unsafe_input());
    }
    Ok(())
}

fn valid_request_path(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_PATH_BYTES
        && !value.ends_with('/')
        && value.bytes().all(|byte| !byte.is_ascii_control())
        && Path::new(value)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn valid_oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn compact_report(report: &mut ResidentRepoQueryReport) -> Result<(), ResidentRepoQueryError> {
    loop {
        let bytes = serde_json::to_vec(report).map_err(|_| limit_exceeded())?;
        if bytes.len() <= MAX_RESPONSE_BYTES {
            return Ok(());
        }
        if let Some(blob) = report
            .blobs
            .iter_mut()
            .rev()
            .find(|blob| blob.text.is_some())
        {
            blob.text = None;
            blob.omitted_bytes = blob.size_bytes.unwrap_or(0);
            blob.status = ResidentRepoEvidenceStatus::Truncated;
            blob.reason = Some("aggregate_response_limit");
            continue;
        }
        if report.patch.text.take().is_some() {
            report.patch.included = false;
            report.patch.omitted_bytes = report.patch.bytes;
            report.patch.reason = Some("aggregate_response_limit");
            continue;
        }
        if let Some(grep) = report.grep.as_mut()
            && grep.matches.pop().is_some()
        {
            grep.omitted_matches = grep.omitted_matches.saturating_add(1);
            grep.status = ResidentRepoEvidenceStatus::Truncated;
            grep.reason = Some("aggregate_response_limit");
            continue;
        }
        if let Some(history) = report
            .path_history
            .iter_mut()
            .rev()
            .find(|history| !history.commits.is_empty())
        {
            history.commits.pop();
            history.omitted_commits_at_least = history.omitted_commits_at_least.saturating_add(1);
            history.status = ResidentRepoEvidenceStatus::Truncated;
            history.reason = Some("aggregate_response_limit");
            continue;
        }
        if report.changed_files.pop().is_some() {
            report.changed_files_omitted = report.changed_files_omitted.saturating_add(1);
            report.changed_files_status = ResidentRepoEvidenceStatus::Truncated;
            continue;
        }
        return Err(limit_exceeded());
    }
}

fn validate_checkout(
    checkout: &Path,
) -> Result<(PathBuf, CheckoutIdentity), ResidentRepoQueryError> {
    if !valid_absolute_path(checkout) || checkout.to_str().is_none() {
        return Err(unsafe_input());
    }
    let link_metadata = std::fs::symlink_metadata(checkout).map_err(|_| unsafe_input())?;
    if link_metadata.file_type().is_symlink() || !link_metadata.is_dir() {
        return Err(unsafe_input());
    }
    let canonical = std::fs::canonicalize(checkout).map_err(|_| unsafe_input())?;
    if canonical.as_path() != checkout {
        return Err(unsafe_input());
    }
    let metadata = std::fs::metadata(&canonical).map_err(|_| unsafe_input())?;
    let identity = CheckoutIdentity::from_metadata(&metadata);
    Ok((canonical, identity))
}

fn validate_private_git_directory(value: &str) -> Result<PathBuf, ResidentRepoQueryError> {
    let path = PathBuf::from(value);
    if !valid_absolute_path(&path) {
        return Err(invalid_output());
    }
    let canonical = std::fs::canonicalize(&path).map_err(|_| invalid_output())?;
    if canonical != path {
        return Err(invalid_output());
    }
    let metadata = std::fs::symlink_metadata(&canonical).map_err(|_| invalid_output())?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(invalid_output());
    }
    Ok(canonical)
}

fn valid_absolute_path(path: &Path) -> bool {
    path.is_absolute()
        && path != Path::new("/")
        && path.to_str().is_some()
        && path
            .components()
            .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
}

fn single_line(value: &str) -> Result<&str, ResidentRepoQueryError> {
    let line = value.strip_suffix('\n').unwrap_or(value);
    if line.is_empty() || line.contains(['\n', '\r', '\0']) {
        return Err(invalid_output());
    }
    Ok(line)
}

fn parse_numstat(
    value: &str,
) -> Result<(Vec<ResidentRepoChangedFile>, ResidentRepoDiffSummary), ResidentRepoQueryError> {
    if !value.is_empty() && !value.ends_with('\0') {
        return Err(invalid_output());
    }
    let mut files = Vec::new();
    let mut text_files = 0_u32;
    let mut binary_files = 0_u32;
    let mut insertions = 0_u64;
    let mut deletions = 0_u64;
    let mut files_changed = 0_u32;
    for entry in value.split_terminator('\0') {
        files_changed = files_changed.checked_add(1).ok_or_else(limit_exceeded)?;
        let mut fields = entry.splitn(3, '\t');
        let additions = fields.next().ok_or_else(invalid_output)?;
        let removals = fields.next().ok_or_else(invalid_output)?;
        let path = fields.next().ok_or_else(invalid_output)?;
        if !valid_repo_output_path(path) {
            return Err(invalid_output());
        }
        let binary = additions == "-" && removals == "-";
        let (file_insertions, file_deletions) = if binary {
            binary_files = binary_files.checked_add(1).ok_or_else(limit_exceeded)?;
            (None, None)
        } else {
            let file_insertions = additions.parse::<u64>().map_err(|_| invalid_output())?;
            let file_deletions = removals.parse::<u64>().map_err(|_| invalid_output())?;
            insertions = insertions
                .checked_add(file_insertions)
                .ok_or_else(limit_exceeded)?;
            deletions = deletions
                .checked_add(file_deletions)
                .ok_or_else(limit_exceeded)?;
            text_files = text_files.checked_add(1).ok_or_else(limit_exceeded)?;
            (Some(file_insertions), Some(file_deletions))
        };
        if files.len() < MAX_CHANGED_FILES {
            files.push(ResidentRepoChangedFile {
                path: path.to_owned(),
                insertions: file_insertions,
                deletions: file_deletions,
                binary,
            });
        }
    }
    Ok((
        files,
        ResidentRepoDiffSummary {
            files_changed,
            text_files,
            binary_files,
            insertions,
            deletions,
        },
    ))
}

fn valid_repo_output_path(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_PATH_BYTES
        && !value.starts_with('/')
        && value.bytes().all(|byte| !byte.is_ascii_control())
        && Path::new(value)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn build_patch_evidence(
    patch: &str,
    max_patch_bytes: usize,
) -> Result<ResidentRepoPatchEvidence, ResidentRepoQueryError> {
    let bytes = u32::try_from(patch.len()).map_err(|_| limit_exceeded())?;
    let included = patch.len() <= max_patch_bytes;
    Ok(ResidentRepoPatchEvidence {
        bytes,
        sha256: sha256_digest(patch.as_bytes()),
        included,
        omitted_bytes: if included { 0 } else { bytes },
        reason: (!included).then_some("patch_byte_limit"),
        text: included.then(|| patch.to_owned()),
    })
}

fn profile_generation() -> String {
    let mut contract = PROFILE_CONTRACT.to_vec();
    for value in [
        MAX_PATCH_BYTES,
        MAX_CHANGED_FILES,
        MAX_GIT_OUTPUT_BYTES,
        MAX_RESPONSE_BYTES,
        MAX_AUXILIARY_QUERIES,
        MAX_GREP_MATCHES,
        MAX_BLOB_BYTES,
        MAX_HISTORY_COMMITS,
        MAX_PATH_BYTES,
        MAX_LITERAL_BYTES,
        MAX_MATCH_TEXT_BYTES,
        MAX_SUBJECT_BYTES,
    ] {
        append_field(&mut contract, &value.to_string());
    }
    append_field(
        &mut contract,
        &REPO_QUERY_COMMAND_TIMEOUT.as_millis().to_string(),
    );
    sha256_digest(&contract)
}

fn sha256_digest(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(71);
    encoded.push_str("sha256:");
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

fn append_field(bytes: &mut Vec<u8>, value: &str) {
    bytes.extend_from_slice(&(value.len() as u64).to_be_bytes());
    bytes.extend_from_slice(value.as_bytes());
}

fn bounded_micros(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

const fn unsafe_input() -> ResidentRepoQueryError {
    ResidentRepoQueryError::new(
        "unsafe_input",
        "repo query input is not one normalized exact local coordinate",
    )
}

const fn repository_mismatch() -> ResidentRepoQueryError {
    ResidentRepoQueryError::new(
        "repository_mismatch",
        "resident checkout does not match the requested canonical project",
    )
}

const fn object_unavailable() -> ResidentRepoQueryError {
    ResidentRepoQueryError::new(
        "object_unavailable",
        "an exact requested Git commit object is unavailable locally",
    )
}

const fn query_unavailable() -> ResidentRepoQueryError {
    ResidentRepoQueryError::new(
        "query_unavailable",
        "the bounded local Git query did not complete successfully",
    )
}

const fn invalid_output() -> ResidentRepoQueryError {
    ResidentRepoQueryError::new(
        "invalid_output",
        "the bounded local Git query returned malformed evidence",
    )
}

const fn source_changed() -> ResidentRepoQueryError {
    ResidentRepoQueryError::new(
        "source_changed",
        "resident repository identity changed during observation",
    )
}

const fn source_mismatch() -> ResidentRepoQueryError {
    ResidentRepoQueryError::new(
        "source_mismatch",
        "exact requested candidate commit and tree do not identify the same Git source",
    )
}

const fn limit_exceeded() -> ResidentRepoQueryError {
    ResidentRepoQueryError::new(
        "limit_exceeded",
        "repo query evidence exceeds the fixed v1 bounds",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(limit: usize) -> ResidentRepoQueryRequest {
        ResidentRepoQueryRequest::new(
            ProjectIdentity::parse("github.com/teamleaderleo/glaeda").expect("project"),
            CommitId::parse("1111111111111111111111111111111111111111").expect("base"),
            CommitId::parse("2222222222222222222222222222222222222222").expect("head"),
            GitTreeId::parse("3333333333333333333333333333333333333333").expect("tree"),
            limit,
        )
        .expect("request")
    }

    #[test]
    fn request_digest_binds_patch_limit_and_profile_generation() {
        assert_ne!(request(0).digest(), request(1).digest());
        assert!(request(0).digest().starts_with("sha256:"));
        assert_eq!(request(0).digest().len(), 71);
    }

    #[test]
    fn patch_is_all_or_nothing_and_always_digested() {
        let included = build_patch_evidence("abc", 3).expect("included patch");
        assert!(included.included());
        assert_eq!(included.bytes(), 3);
        assert_eq!(included.text.as_deref(), Some("abc"));

        let omitted = build_patch_evidence("abcd", 3).expect("omitted patch");
        assert!(!omitted.included());
        assert_eq!(omitted.omitted_bytes, 4);
        assert!(omitted.text.is_none());
        assert_ne!(included.digest(), omitted.digest());
    }

    #[test]
    fn numstat_is_exact_and_rejects_partial_records() {
        let (files, summary) =
            parse_numstat("3\t1\tsrc/lib.rs\0-\t-\tassets/image.bin\0").expect("numstat");
        assert_eq!(files.len(), 2);
        assert_eq!(summary.files_changed(), 2);
        assert_eq!(summary.insertions(), 3);
        assert_eq!(summary.deletions(), 1);
        assert!(files[1].binary());
        assert_eq!(
            parse_numstat("1\t0\tsrc/lib.rs")
                .expect_err("partial record")
                .code,
            "invalid_output"
        );
    }

    #[test]
    fn unsafe_paths_and_oversized_limits_are_refused_without_echo() {
        assert!(!valid_absolute_path(Path::new("relative/private")));
        assert!(!valid_repo_output_path("../private"));
        let error = ResidentRepoQueryRequest::new(
            ProjectIdentity::parse("github.com/teamleaderleo/glaeda").expect("project"),
            CommitId::parse("1111111111111111111111111111111111111111").expect("base"),
            CommitId::parse("2222222222222222222222222222222222222222").expect("head"),
            GitTreeId::parse("3333333333333333333333333333333333333333").expect("tree"),
            MAX_PATCH_BYTES + 1,
        )
        .expect_err("oversized limit");
        assert_eq!(error.code, "patch_limit_invalid");

        let mixed = ResidentRepoQueryRequest::new(
            ProjectIdentity::parse("github.com/teamleaderleo/glaeda").expect("project"),
            CommitId::parse("1111111111111111111111111111111111111111").expect("base"),
            CommitId::parse(&"22".repeat(32)).expect("head"),
            GitTreeId::parse(&"33".repeat(32)).expect("tree"),
            0,
        )
        .expect_err("mixed object formats");
        assert_eq!(mixed.code, "unsafe_input");
    }
}
