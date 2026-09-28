use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Component, Path, PathBuf};

use crate::artifact::Sha256Digest;
use crate::process::TimedCommandExecutor;
use crate::project_catalog::{ProjectCatalog, ProjectCatalogIdentity, ProjectIdentity};
use crate::project_checkout_observation::{
    ProjectCheckoutObservation, ProjectCheckoutObservationErrorKind, ProjectCheckoutObserver,
};
use crate::project_workspace_identity::{
    ProjectWorkspaceFilesystemIdentityKind, ProjectWorkspaceIdentityGeneration,
    project_workspace_filesystem_identity,
};
use serde::Serialize;

pub const PROJECT_DISCOVERY_SCHEMA_VERSION: u8 = 2;
pub const MAX_PROJECT_DISCOVERY_ENTRIES: usize = 512;

#[derive(Clone, PartialEq, Eq)]
struct PrivateDiscoveryLocation {
    path: PathBuf,
    identity: Option<FilesystemIdentity>,
}

impl PrivateDiscoveryLocation {
    fn observed(path: PathBuf, identity: FilesystemIdentity) -> Self {
        Self {
            path,
            identity: Some(identity),
        }
    }

    fn unavailable(path: PathBuf) -> Self {
        Self {
            path,
            identity: None,
        }
    }
}

impl fmt::Debug for PrivateDiscoveryLocation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<private-project-discovery-location>")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectDiscoveryEntryKind {
    Checkout,
    NonGitDirectory,
    BareRepository,
    Symlink,
    NonDirectory,
    Changed,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ProjectDiscoveryMatch {
    Catalogued { project: ProjectIdentity },
    Uncatalogued { project: ProjectIdentity },
    AmbiguousCatalog { projects: Vec<ProjectIdentity> },
    AmbiguousSource,
    NoCanonicalSource,
}

impl ProjectDiscoveryMatch {
    fn dedupe_project(&self) -> Option<&ProjectIdentity> {
        match self {
            Self::Catalogued { project } | Self::Uncatalogued { project } => Some(project),
            Self::AmbiguousCatalog { .. } | Self::AmbiguousSource | Self::NoCanonicalSource => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectRecoveryRisk {
    pub tracked_changes_present: bool,
    pub untracked_entry_count: u32,
    pub upstream_missing: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_commits_ahead: Option<u32>,
    pub source_ambiguous: bool,
    pub multiple_worktrees: bool,
    pub submodules_present: bool,
    pub owner_mismatch: bool,
    pub duplicate_materialization: bool,
}

impl ProjectRecoveryRisk {
    fn from_observation(observation: &ProjectCheckoutObservation) -> Self {
        Self {
            tracked_changes_present: observation.tracked_changes_present(),
            untracked_entry_count: observation.untracked_entry_count(),
            upstream_missing: !observation.upstream_configured(),
            local_commits_ahead: observation.local_commits_ahead(),
            source_ambiguous: observation.source_ambiguous(),
            multiple_worktrees: observation.linked_worktree_count() > 1,
            submodules_present: observation.submodules_present(),
            owner_mismatch: !observation.owner_matches_parent(),
            duplicate_materialization: false,
        }
    }

    #[must_use]
    pub const fn local_only_state_present(&self) -> bool {
        self.tracked_changes_present
            || self.untracked_entry_count > 0
            || matches!(self.local_commits_ahead, Some(count) if count > 0)
            || self.upstream_missing
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectDiscoveryEntry {
    pub slot: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry_id: Option<Sha256Digest>,
    pub kind: ProjectDiscoveryEntryKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_match: Option<ProjectDiscoveryMatch>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery: Option<ProjectRecoveryRisk>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checkout: Option<ProjectCheckoutObservation>,
    #[serde(skip)]
    location: PrivateDiscoveryLocation,
}

impl ProjectDiscoveryEntry {
    fn checkout(
        slot: u16,
        observation: ProjectCheckoutObservation,
        project_match: ProjectDiscoveryMatch,
        location: PrivateDiscoveryLocation,
    ) -> Self {
        let entry_id = Some(observation.materialization_id().clone());
        let recovery = Some(ProjectRecoveryRisk::from_observation(&observation));
        Self {
            slot,
            entry_id,
            kind: ProjectDiscoveryEntryKind::Checkout,
            project_match: Some(project_match),
            recovery,
            checkout: Some(observation),
            location,
        }
    }

    fn classified(
        slot: u16,
        entry_id: Option<Sha256Digest>,
        kind: ProjectDiscoveryEntryKind,
        location: PrivateDiscoveryLocation,
    ) -> Self {
        Self {
            slot,
            entry_id,
            kind,
            project_match: None,
            recovery: None,
            checkout: None,
            location,
        }
    }

    fn dedupe_project(&self) -> Option<&ProjectIdentity> {
        self.project_match
            .as_ref()
            .and_then(ProjectDiscoveryMatch::dedupe_project)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectDiscoverySummary {
    pub entry_count: u16,
    pub checkout_count: u16,
    pub catalogued_checkout_count: u16,
    pub uncatalogued_checkout_count: u16,
    pub ambiguous_checkout_count: u16,
    pub dirty_checkout_count: u16,
    pub local_only_checkout_count: u16,
    pub duplicate_materialization_count: u16,
    pub non_git_entry_count: u16,
    pub unknown_entry_count: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectDiscoveryReport {
    schema_version: u8,
    identity_generation: ProjectWorkspaceIdentityGeneration,
    root_id: Sha256Digest,
    catalog_identity: ProjectCatalogIdentity,
    entries: Vec<ProjectDiscoveryEntry>,
    summary: ProjectDiscoverySummary,
    #[serde(skip)]
    root_location: PrivateDiscoveryLocation,
}

impl ProjectDiscoveryReport {
    #[must_use]
    pub const fn identity_generation(&self) -> ProjectWorkspaceIdentityGeneration {
        self.identity_generation
    }

    #[must_use]
    pub const fn root_id(&self) -> &Sha256Digest {
        &self.root_id
    }

    #[must_use]
    pub const fn catalog_identity(&self) -> &ProjectCatalogIdentity {
        &self.catalog_identity
    }

    #[must_use]
    pub fn entries(&self) -> &[ProjectDiscoveryEntry] {
        &self.entries
    }

    #[must_use]
    pub const fn summary(&self) -> &ProjectDiscoverySummary {
        &self.summary
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectDiscoveryErrorKind {
    UnsafeRoot,
    RootUnavailable,
    TooManyEntries,
    RootChanged,
    InvalidIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectDiscoveryError {
    pub kind: ProjectDiscoveryErrorKind,
    pub code: &'static str,
    pub problem: &'static str,
}

impl fmt::Display for ProjectDiscoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.problem)
    }
}

impl std::error::Error for ProjectDiscoveryError {}

/// Discover immediate project candidates beneath one explicit canonical root.
///
/// Discovery is offline and read-only. It never recurses beneath an immediate child. Private child
/// names are used only to produce deterministic per-scan slot ordering and never enter the public
/// report. The immediate child set and every observed child filesystem identity are rechecked after
/// checkout observation so add/remove/rename/replacement races fail as `root_changed`.
///
/// # Errors
///
/// Returns a bounded error when the root is unsafe or aliased, cannot be read completely, exceeds
/// the candidate bound, changes during discovery, or an opaque public identity cannot be encoded.
pub fn discover_project_root(
    root: &Path,
    catalog: &ProjectCatalog,
    observer: &ProjectCheckoutObserver,
    executor: &impl TimedCommandExecutor,
) -> Result<ProjectDiscoveryReport, ProjectDiscoveryError> {
    if !is_normalized_absolute_path(root) || root.to_str().is_none() {
        return Err(unsafe_root());
    }
    let source_metadata = std::fs::symlink_metadata(root).map_err(|_| root_unavailable())?;
    if source_metadata.file_type().is_symlink() || !source_metadata.is_dir() {
        return Err(unsafe_root());
    }
    let canonical = std::fs::canonicalize(root).map_err(|_| root_unavailable())?;
    if canonical.as_path() != root {
        return Err(unsafe_root());
    }
    let root_metadata = std::fs::metadata(root).map_err(|_| root_unavailable())?;
    let root_identity = FilesystemIdentity::from_metadata(&root_metadata);
    let identity_generation = observer.identity_generation();
    let root_id = opaque_filesystem_id(
        identity_generation,
        ProjectWorkspaceFilesystemIdentityKind::DiscoveryRoot,
        root_identity,
    )?;

    let candidates = read_candidates(root)?;
    let initial_keys = candidates
        .iter()
        .map(|candidate| candidate.sort_key.clone())
        .collect::<Vec<_>>();
    let mut entries = Vec::with_capacity(candidates.len());
    for (index, candidate) in candidates.into_iter().enumerate() {
        let slot = u16::try_from(index).map_err(|_| too_many_entries())?;
        entries.push(discover_candidate(
            slot,
            candidate.path,
            catalog,
            observer,
            executor,
            identity_generation,
        ));
    }

    mark_duplicate_materializations(&mut entries);
    validate_discovery_stable(root, root_identity, &initial_keys, &entries)?;
    let summary = summarize(&entries)?;
    Ok(ProjectDiscoveryReport {
        schema_version: PROJECT_DISCOVERY_SCHEMA_VERSION,
        identity_generation,
        root_id,
        catalog_identity: catalog.identity().clone(),
        entries,
        summary,
        root_location: PrivateDiscoveryLocation::observed(root.to_path_buf(), root_identity),
    })
}

#[derive(Debug)]
struct Candidate {
    sort_key: Vec<u8>,
    path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FilesystemIdentity {
    device: u64,
    inode: u64,
    owner: u32,
}

impl FilesystemIdentity {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            owner: metadata.uid(),
        }
    }

    fn matches(self, metadata: &std::fs::Metadata) -> bool {
        metadata.dev() == self.device
            && metadata.ino() == self.inode
            && metadata.uid() == self.owner
    }
}

fn read_candidates(root: &Path) -> Result<Vec<Candidate>, ProjectDiscoveryError> {
    let mut candidates = Vec::new();
    let directory = std::fs::read_dir(root).map_err(|_| root_unavailable())?;
    for entry in directory {
        let entry = entry.map_err(|_| root_changed())?;
        if candidates.len() >= MAX_PROJECT_DISCOVERY_ENTRIES {
            return Err(too_many_entries());
        }
        let name = entry.file_name();
        candidates.push(Candidate {
            sort_key: name.as_bytes().to_vec(),
            path: entry.path(),
        });
    }
    candidates.sort_by(|left, right| left.sort_key.cmp(&right.sort_key));
    Ok(candidates)
}

fn discover_candidate(
    slot: u16,
    path: PathBuf,
    catalog: &ProjectCatalog,
    observer: &ProjectCheckoutObserver,
    executor: &impl TimedCommandExecutor,
    identity_generation: ProjectWorkspaceIdentityGeneration,
) -> ProjectDiscoveryEntry {
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(_) => {
            return ProjectDiscoveryEntry::classified(
                slot,
                None,
                ProjectDiscoveryEntryKind::Unknown,
                PrivateDiscoveryLocation::unavailable(path),
            );
        }
    };
    let identity = FilesystemIdentity::from_metadata(&metadata);
    let entry_id = opaque_filesystem_id(
        identity_generation,
        ProjectWorkspaceFilesystemIdentityKind::DiscoveryEntry,
        identity,
    )
    .ok();
    let location = PrivateDiscoveryLocation::observed(path.clone(), identity);
    if metadata.file_type().is_symlink() {
        return ProjectDiscoveryEntry::classified(
            slot,
            entry_id,
            ProjectDiscoveryEntryKind::Symlink,
            location,
        );
    }
    if !metadata.is_dir() {
        return ProjectDiscoveryEntry::classified(
            slot,
            entry_id,
            ProjectDiscoveryEntryKind::NonDirectory,
            location,
        );
    }
    if path.to_str().is_none() {
        return ProjectDiscoveryEntry::classified(
            slot,
            entry_id,
            ProjectDiscoveryEntryKind::Unknown,
            location,
        );
    }

    match observer.observe(&path, executor) {
        Ok(observation) => {
            let project_match = match_project(catalog, &observation);
            ProjectDiscoveryEntry::checkout(slot, observation, project_match, location)
        }
        Err(error) => {
            let kind = match error.kind {
                ProjectCheckoutObservationErrorKind::NotWorktree => {
                    ProjectDiscoveryEntryKind::NonGitDirectory
                }
                ProjectCheckoutObservationErrorKind::BareRepository => {
                    ProjectDiscoveryEntryKind::BareRepository
                }
                ProjectCheckoutObservationErrorKind::SourceChanged => {
                    ProjectDiscoveryEntryKind::Changed
                }
                ProjectCheckoutObservationErrorKind::UnsafePath
                | ProjectCheckoutObservationErrorKind::Unavailable
                | ProjectCheckoutObservationErrorKind::InvalidOutput => {
                    ProjectDiscoveryEntryKind::Unknown
                }
            };
            ProjectDiscoveryEntry::classified(slot, entry_id, kind, location)
        }
    }
}

fn match_project(
    catalog: &ProjectCatalog,
    observation: &ProjectCheckoutObservation,
) -> ProjectDiscoveryMatch {
    let observed_projects = observation
        .remotes()
        .iter()
        .filter_map(|remote| remote.project.clone())
        .collect::<BTreeSet<_>>();
    let catalog_matches = catalog
        .projects()
        .iter()
        .filter(|entry| observed_projects.contains(entry.id()))
        .map(|entry| entry.id().clone())
        .collect::<Vec<_>>();

    match catalog_matches.as_slice() {
        [project] => ProjectDiscoveryMatch::Catalogued {
            project: project.clone(),
        },
        [] if observation.source_ambiguous() => ProjectDiscoveryMatch::AmbiguousSource,
        [] => observation.primary_project().map_or(
            ProjectDiscoveryMatch::NoCanonicalSource,
            |project| ProjectDiscoveryMatch::Uncatalogued {
                project: project.clone(),
            },
        ),
        _ => ProjectDiscoveryMatch::AmbiguousCatalog {
            projects: catalog_matches,
        },
    }
}

fn mark_duplicate_materializations(entries: &mut [ProjectDiscoveryEntry]) {
    let mut counts = BTreeMap::<ProjectIdentity, usize>::new();
    for entry in entries.iter() {
        if let Some(project) = entry.dedupe_project() {
            *counts.entry(project.clone()).or_default() += 1;
        }
    }
    for entry in entries {
        let duplicate = entry
            .dedupe_project()
            .is_some_and(|project| counts.get(project).copied().unwrap_or_default() > 1);
        if let Some(recovery) = entry.recovery.as_mut() {
            recovery.duplicate_materialization = duplicate;
        }
    }
}

fn validate_discovery_stable(
    root: &Path,
    root_identity: FilesystemIdentity,
    initial_keys: &[Vec<u8>],
    entries: &[ProjectDiscoveryEntry],
) -> Result<(), ProjectDiscoveryError> {
    let final_metadata = std::fs::metadata(root).map_err(|_| root_changed())?;
    if !final_metadata.is_dir() || !root_identity.matches(&final_metadata) {
        return Err(root_changed());
    }
    let final_candidates = read_candidates(root).map_err(|_| root_changed())?;
    let final_keys = final_candidates
        .into_iter()
        .map(|candidate| candidate.sort_key)
        .collect::<Vec<_>>();
    if final_keys != initial_keys {
        return Err(root_changed());
    }
    for entry in entries {
        let current = std::fs::symlink_metadata(&entry.location.path);
        match (entry.location.identity, current) {
            (Some(expected), Ok(metadata)) if expected.matches(&metadata) => {}
            (None, Err(_)) => {}
            _ => return Err(root_changed()),
        }
    }
    Ok(())
}

fn summarize(
    entries: &[ProjectDiscoveryEntry],
) -> Result<ProjectDiscoverySummary, ProjectDiscoveryError> {
    let mut summary = ProjectDiscoverySummary {
        entry_count: bounded_count(entries.len())?,
        checkout_count: 0,
        catalogued_checkout_count: 0,
        uncatalogued_checkout_count: 0,
        ambiguous_checkout_count: 0,
        dirty_checkout_count: 0,
        local_only_checkout_count: 0,
        duplicate_materialization_count: 0,
        non_git_entry_count: 0,
        unknown_entry_count: 0,
    };
    for entry in entries {
        match entry.kind {
            ProjectDiscoveryEntryKind::Checkout => {
                increment(&mut summary.checkout_count)?;
                match entry.project_match.as_ref() {
                    Some(ProjectDiscoveryMatch::Catalogued { .. }) => {
                        increment(&mut summary.catalogued_checkout_count)?;
                    }
                    Some(ProjectDiscoveryMatch::Uncatalogued { .. }) => {
                        increment(&mut summary.uncatalogued_checkout_count)?;
                    }
                    Some(
                        ProjectDiscoveryMatch::AmbiguousCatalog { .. }
                        | ProjectDiscoveryMatch::AmbiguousSource
                        | ProjectDiscoveryMatch::NoCanonicalSource,
                    )
                    | None => increment(&mut summary.ambiguous_checkout_count)?,
                }
                if let Some(recovery) = entry.recovery.as_ref() {
                    if recovery.tracked_changes_present || recovery.untracked_entry_count > 0 {
                        increment(&mut summary.dirty_checkout_count)?;
                    }
                    if recovery.local_only_state_present() {
                        increment(&mut summary.local_only_checkout_count)?;
                    }
                    if recovery.duplicate_materialization {
                        increment(&mut summary.duplicate_materialization_count)?;
                    }
                }
            }
            ProjectDiscoveryEntryKind::NonGitDirectory
            | ProjectDiscoveryEntryKind::BareRepository
            | ProjectDiscoveryEntryKind::Symlink
            | ProjectDiscoveryEntryKind::NonDirectory => {
                increment(&mut summary.non_git_entry_count)?;
            }
            ProjectDiscoveryEntryKind::Changed | ProjectDiscoveryEntryKind::Unknown => {
                increment(&mut summary.unknown_entry_count)?;
            }
        }
    }
    Ok(summary)
}

fn bounded_count(value: usize) -> Result<u16, ProjectDiscoveryError> {
    u16::try_from(value).map_err(|_| too_many_entries())
}

fn increment(value: &mut u16) -> Result<(), ProjectDiscoveryError> {
    *value = value.checked_add(1).ok_or_else(too_many_entries)?;
    Ok(())
}

fn opaque_filesystem_id(
    generation: ProjectWorkspaceIdentityGeneration,
    kind: ProjectWorkspaceFilesystemIdentityKind,
    identity: FilesystemIdentity,
) -> Result<Sha256Digest, ProjectDiscoveryError> {
    project_workspace_filesystem_identity(
        generation,
        kind,
        identity.device,
        identity.inode,
        identity.owner,
    )
    .map_err(|_| invalid_identity())
}

fn is_normalized_absolute_path(path: &Path) -> bool {
    path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
}

fn error(
    kind: ProjectDiscoveryErrorKind,
    code: &'static str,
    problem: &'static str,
) -> ProjectDiscoveryError {
    ProjectDiscoveryError {
        kind,
        code,
        problem,
    }
}

fn unsafe_root() -> ProjectDiscoveryError {
    error(
        ProjectDiscoveryErrorKind::UnsafeRoot,
        "unsafe_root",
        "project discovery root is unsafe or aliased",
    )
}

fn root_unavailable() -> ProjectDiscoveryError {
    error(
        ProjectDiscoveryErrorKind::RootUnavailable,
        "root_unavailable",
        "project discovery root is unavailable",
    )
}

fn too_many_entries() -> ProjectDiscoveryError {
    error(
        ProjectDiscoveryErrorKind::TooManyEntries,
        "too_many_entries",
        "project discovery root exceeds the bounded immediate-entry count",
    )
}

fn root_changed() -> ProjectDiscoveryError {
    error(
        ProjectDiscoveryErrorKind::RootChanged,
        "root_changed",
        "project discovery root changed during observation",
    )
}

fn invalid_identity() -> ProjectDiscoveryError {
    error(
        ProjectDiscoveryErrorKind::InvalidIdentity,
        "invalid_identity",
        "project discovery identity could not be encoded",
    )
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;
    use std::fs;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::process::{CommandExecutor, CommandSpec, ExecutionRecord, TimedCommandExecutor};
    use crate::project_catalog::ProjectCatalog;
    use crate::project_checkout_observation::{ProjectCheckoutObserver, expected_git_timeout};
    use crate::project_workspace_identity::ProjectWorkspaceIdentityGeneration;

    use super::{
        MAX_PROJECT_DISCOVERY_ENTRIES, ProjectDiscoveryEntryKind, ProjectDiscoveryErrorKind,
        ProjectDiscoveryMatch, discover_project_root,
    };

    const COMMIT: &str = "1111111111111111111111111111111111111111";
    const TREE: &str = "2222222222222222222222222222222222222222";
    static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new(label: &str) -> Self {
            let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "smolrunner-project-discovery-{label}-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create discovery root");
            Self(fs::canonicalize(path).expect("canonical discovery root"))
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn directory(&self, name: &str) -> PathBuf {
            let path = self.0.join(name);
            fs::create_dir(&path).expect("create candidate directory");
            fs::canonicalize(path).expect("canonical candidate")
        }
    }

    impl Drop for TempRoot {
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
        mutation_root: Option<PathBuf>,
        mutated: Cell<bool>,
    }

    impl ScriptedExecutor {
        fn new(responses: Vec<Response>) -> Self {
            Self {
                responses: RefCell::new(responses.into()),
                commands: RefCell::new(Vec::new()),
                mutation_root: None,
                mutated: Cell::new(false),
            }
        }

        fn with_root_mutation(responses: Vec<Response>, root: &Path) -> Self {
            Self {
                responses: RefCell::new(responses.into()),
                commands: RefCell::new(Vec::new()),
                mutation_root: Some(root.to_path_buf()),
                mutated: Cell::new(false),
            }
        }
    }

    impl CommandExecutor for ScriptedExecutor {
        fn execute(&self, _spec: &CommandSpec) -> io::Result<ExecutionRecord> {
            panic!("discovery observer must use timed command execution")
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
            if let Some(root) = self.mutation_root.as_ref()
                && !self.mutated.replace(true)
            {
                fs::create_dir(root.join("late-entry")).expect("mutate discovery root");
            }
            let response = self
                .responses
                .borrow_mut()
                .pop_front()
                .expect("scripted Git response");
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

    fn catalog() -> ProjectCatalog {
        ProjectCatalog::decode_yaml(
            br#"version: 1
projects:
  - id: github.com/teamleaderleo/smolrunner
    aliases: [smolrunner]
    source: https://github.com/teamleaderleo/smolrunner.git
    materialization: developer
    restore: eager
  - id: github.com/upstream/project
    aliases: [upstream]
    source: https://github.com/upstream/project.git
    materialization: developer
    restore: lazy
"#,
        )
        .expect("catalog")
    }

    fn snapshot_responses(remotes: &str, status: &str, worktree_path: &Path) -> Vec<Response> {
        let worktrees = format!(
            "worktree {}\0HEAD {COMMIT}\0branch refs/heads/main\0\0",
            worktree_path.display()
        );
        vec![
            Response::success(format!("{COMMIT}\n")),
            Response::success(format!("{TREE}\n")),
            Response::success(remotes),
            Response::failed(1, ""),
            Response::success(status),
            Response::success("100644\n"),
            Response::success(worktrees),
        ]
    }

    fn checkout_responses(path: &Path, remotes: &str, status: &str) -> Vec<Response> {
        let snapshot = snapshot_responses(remotes, status, path);
        let mut responses = vec![
            Response::success("false\n"),
            Response::success(format!("{}\n", path.display())),
        ];
        responses.extend(snapshot.clone());
        responses.extend(snapshot);
        responses
    }

    #[test]
    fn matches_catalog_across_fork_upstream_and_keeps_paths_private() {
        let root = TempRoot::new("catalog-match");
        let checkout = root.directory("secret-checkout-name");
        let nested = checkout.join("nested");
        fs::create_dir(&nested).expect("nested directory");
        fs::write(nested.join("ignored.txt"), b"ignored").expect("nested file");
        let remotes = concat!(
            "remote.origin.url\nhttps://github.com/example/fork.git\0",
            "remote.upstream.url\nhttps://github.com/upstream/project.git\0"
        );
        let status = format!(
            "# branch.oid {COMMIT}\0# branch.head main\0# branch.upstream origin/main\0# branch.ab +2 -0\0? private.txt\0"
        );
        let executor = ScriptedExecutor::new(checkout_responses(&checkout, remotes, &status));

        let report = discover_project_root(root.path(), &catalog(), &observer(), &executor)
            .expect("discovery report");

        assert_eq!(report.entries().len(), 1);
        let entry = &report.entries()[0];
        assert_eq!(entry.kind, ProjectDiscoveryEntryKind::Checkout);
        assert!(matches!(
            entry.project_match,
            Some(ProjectDiscoveryMatch::Catalogued { ref project })
                if project.as_str() == "github.com/upstream/project"
        ));
        let recovery = entry.recovery.as_ref().expect("recovery");
        assert_eq!(recovery.untracked_entry_count, 1);
        assert_eq!(recovery.local_commits_ahead, Some(2));
        assert!(recovery.source_ambiguous);
        assert!(recovery.local_only_state_present());
        assert_eq!(report.summary().catalogued_checkout_count, 1);
        assert_eq!(
            report.identity_generation(),
            ProjectWorkspaceIdentityGeneration::GlaedaV2
        );
        assert_eq!(
            entry
                .checkout
                .as_ref()
                .expect("checkout observation")
                .identity_generation(),
            report.identity_generation()
        );
        assert_eq!(executor.commands.borrow().len(), 16);

        let json = serde_json::to_string(&report).expect("public report");
        assert!(json.contains("\"schema_version\":2"));
        assert!(json.contains("\"identity_generation\":\"glaeda_v2\""));
        assert!(!json.contains(root.path().to_string_lossy().as_ref()));
        assert!(!json.contains("secret-checkout-name"));
        assert!(!json.contains("private.txt"));
        assert!(!json.contains("ignored.txt"));
    }

    #[test]
    fn duplicate_materializations_are_marked_for_catalogued_project() {
        let root = TempRoot::new("duplicates");
        let first = root.directory("a");
        let second = root.directory("b");
        let remote = "remote.origin.url\nhttps://github.com/teamleaderleo/smolrunner.git\0";
        let status = format!("# branch.oid {COMMIT}\0# branch.head main\0");
        let mut responses = checkout_responses(&first, remote, &status);
        responses.extend(checkout_responses(&second, remote, &status));
        let executor = ScriptedExecutor::new(responses);

        let report = discover_project_root(root.path(), &catalog(), &observer(), &executor)
            .expect("duplicate discovery");
        assert_eq!(report.entries().len(), 2);
        assert!(report.entries().iter().all(|entry| {
            entry
                .recovery
                .as_ref()
                .is_some_and(|recovery| recovery.duplicate_materialization)
        }));
        assert_eq!(report.summary().duplicate_materialization_count, 2);
    }

    #[test]
    fn immediate_entries_are_classified_without_recursive_scanning() {
        use std::os::unix::fs::symlink;

        let root = TempRoot::new("classification");
        let non_git = root.directory("a-non-git");
        let nested = non_git.join("nested-repository-looking-directory");
        fs::create_dir(&nested).expect("nested directory");
        let _bare = root.directory("b-bare");
        let target = root.directory("c-target");
        let link = root.path().join("d-link");
        symlink(&target, &link).expect("symlink");
        fs::write(root.path().join("e-file"), b"file").expect("regular file");

        let executor = ScriptedExecutor::new(vec![
            Response::failed(128, "fatal: private non-git path"),
            Response::success("true\n"),
            Response::failed(128, "fatal: target is non-git"),
        ]);
        let report = discover_project_root(root.path(), &catalog(), &observer(), &executor)
            .expect("classification report");

        let kinds = report
            .entries()
            .iter()
            .map(|entry| entry.kind)
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            vec![
                ProjectDiscoveryEntryKind::NonGitDirectory,
                ProjectDiscoveryEntryKind::BareRepository,
                ProjectDiscoveryEntryKind::NonGitDirectory,
                ProjectDiscoveryEntryKind::Symlink,
                ProjectDiscoveryEntryKind::NonDirectory,
            ]
        );
        assert_eq!(executor.commands.borrow().len(), 3);
        let json = serde_json::to_string(&report).expect("public report");
        assert!(!json.contains("nested-repository-looking-directory"));
        assert!(!json.contains("private non-git path"));
    }

    #[test]
    fn candidate_limit_and_root_alias_fail_before_git() {
        use std::os::unix::fs::symlink;

        let root = TempRoot::new("limit");
        for index in 0..=MAX_PROJECT_DISCOVERY_ENTRIES {
            fs::create_dir(root.path().join(format!("entry-{index:04}"))).expect("candidate");
        }
        let executor = ScriptedExecutor::new(Vec::new());
        let error = discover_project_root(root.path(), &catalog(), &observer(), &executor)
            .expect_err("bounded candidate count");
        assert_eq!(error.kind, ProjectDiscoveryErrorKind::TooManyEntries);
        assert!(executor.commands.borrow().is_empty());

        let actual = TempRoot::new("alias-actual");
        let holder = TempRoot::new("alias-holder");
        let alias = holder.path().join("alias");
        symlink(actual.path(), &alias).expect("root alias");
        let error = discover_project_root(&alias, &catalog(), &observer(), &executor)
            .expect_err("aliased root");
        assert_eq!(error.kind, ProjectDiscoveryErrorKind::UnsafeRoot);
        assert!(executor.commands.borrow().is_empty());
    }

    #[test]
    fn root_child_set_change_fails_closed() {
        let root = TempRoot::new("root-change");
        let checkout = root.directory("checkout");
        let remote = "remote.origin.url\nhttps://github.com/teamleaderleo/smolrunner.git\0";
        let status = format!("# branch.oid {COMMIT}\0# branch.head main\0");
        let responses = checkout_responses(&checkout, remote, &status);
        let executor = ScriptedExecutor::with_root_mutation(responses, root.path());

        let error = discover_project_root(root.path(), &catalog(), &observer(), &executor)
            .expect_err("root changed during discovery");
        assert_eq!(error.kind, ProjectDiscoveryErrorKind::RootChanged);
    }
}
