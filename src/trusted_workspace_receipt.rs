use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fmt;
use std::fs::File;
use std::io::Read as _;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::{Component, Path, PathBuf};

use crate::artifact::{CommitId, GitTreeId, RepositoryRef, Sha256Digest};
use crate::ownership::{OwnershipMarker, ProjectIdentity, ResourceIdentity, ResourceKind};
use crate::project_workspace_identity::{
    ProjectWorkspaceIdentityGeneration, TrustedWorkspaceIdentityKind, trusted_workspace_identity,
};
use crate::repository_source_observation::RepositoryWorkspaceLocationIdentity;
use crate::state::InstallationId;
use crate::state_document::{ProjectStateDocument, StateDocument, decode_state_document};
use crate::state_root_generation::{
    SelectedStateRoot, StateRootGeneration, StateRootSelection, select_state_root,
};
use crate::state_store::MAX_STATE_DOCUMENT_BYTES;
use crate::verification_profile::{
    CacheId, CapabilityObservation, HostResourceObservation, RepositoryCommandIdentity,
    RequestedAuthority, ResolvedRef, RunnerInstallationId, RunnerWorkspaceId, WorkspaceCleanliness,
};
use crate::verification_profile_preflight_adapter::{
    TrustedRunnerPrivateEvidence, TrustedRunnerWorkspaceReceipt,
    TrustedRunnerWorkspaceReceiptDefinition,
};
use rustix::fs::{self, AtFlags, Dir, FileType, Mode, OFlags};
use rustix::io::Errno;
use serde::Serialize;

pub const TRUSTED_WORKSPACE_RECEIPT_SCHEMA_VERSION: u8 = 2;

const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const FILE_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const INSTALLATIONS_DIRECTORY: &str = "installations";
const RESOURCES_DIRECTORY: &str = "resources";
const PROJECT_FILE: &str = "project.json";
const WORKSPACE_RESOURCE_FILE: &str = "verification-workspace.json";
const CACHE_RESOURCE_FILE: &str = "verification-cache.json";
const CACHE_ID: &str = "cargo-target";
const MAX_INSTALLATIONS: usize = 1_024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustedWorkspaceReceiptErrorKind {
    MissingState,
    AmbiguousState,
    UnsafeFilesystem,
    CorruptState,
    IdentityMismatch,
    ReceiptConstruction,
    Io,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct TrustedWorkspaceReceiptError {
    kind: TrustedWorkspaceReceiptErrorKind,
    stage: &'static str,
    public_message: String,
}

impl TrustedWorkspaceReceiptError {
    fn new(
        kind: TrustedWorkspaceReceiptErrorKind,
        stage: &'static str,
        public_message: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            stage,
            public_message: public_message.into(),
        }
    }

    #[must_use]
    pub const fn kind(&self) -> TrustedWorkspaceReceiptErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn stage(&self) -> &'static str {
        self.stage
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.public_message
    }
}

impl fmt::Debug for TrustedWorkspaceReceiptError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TrustedWorkspaceReceiptError")
            .field("kind", &self.kind)
            .field("stage", &self.stage)
            .field("message", &self.public_message)
            .finish()
    }
}

impl fmt::Display for TrustedWorkspaceReceiptError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.public_message)
    }
}

impl std::error::Error for TrustedWorkspaceReceiptError {}

/// Descriptor-bound workspace and cache identity derived only from protected versioned state.
#[derive(Serialize)]
pub struct TrustedWorkspaceCacheReceipt {
    schema_version: u8,
    identity_generation: ProjectWorkspaceIdentityGeneration,
    installation_id: RunnerInstallationId,
    workspace_id: RunnerWorkspaceId,
    repository: RepositoryRef,
    cache_id: CacheId,
    cache_owner_workspace_id: RunnerWorkspaceId,
    cache_namespace_digest: Sha256Digest,
    cache_present: bool,
    trusted_evidence_digest: Sha256Digest,
    #[serde(skip)]
    workspace_location_identity: RepositoryWorkspaceLocationIdentity,
    #[serde(skip)]
    workspace_root: PathBuf,
    #[serde(skip)]
    cache_path: PathBuf,
}

impl TrustedWorkspaceCacheReceipt {
    #[must_use]
    pub const fn identity_generation(&self) -> ProjectWorkspaceIdentityGeneration {
        self.identity_generation
    }

    #[must_use]
    pub const fn installation_id(&self) -> &RunnerInstallationId {
        &self.installation_id
    }

    #[must_use]
    pub const fn workspace_id(&self) -> &RunnerWorkspaceId {
        &self.workspace_id
    }

    #[must_use]
    pub const fn repository(&self) -> &RepositoryRef {
        &self.repository
    }

    #[must_use]
    pub const fn cache_id(&self) -> &CacheId {
        &self.cache_id
    }

    #[must_use]
    pub const fn cache_namespace_digest(&self) -> &Sha256Digest {
        &self.cache_namespace_digest
    }

    #[must_use]
    pub const fn trusted_evidence_digest(&self) -> &Sha256Digest {
        &self.trusted_evidence_digest
    }

    #[must_use]
    pub const fn workspace_location_identity(&self) -> &RepositoryWorkspaceLocationIdentity {
        &self.workspace_location_identity
    }

    #[cfg(all(test, target_os = "linux"))]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn for_verification_plan_test(
        installation_id: RunnerInstallationId,
        workspace_id: RunnerWorkspaceId,
        repository: RepositoryRef,
        cache_id: CacheId,
        cache_namespace_digest: Sha256Digest,
        trusted_evidence_digest: Sha256Digest,
        workspace_location_identity: RepositoryWorkspaceLocationIdentity,
    ) -> Self {
        Self {
            schema_version: TRUSTED_WORKSPACE_RECEIPT_SCHEMA_VERSION,
            identity_generation: ProjectWorkspaceIdentityGeneration::CURRENT,
            installation_id,
            workspace_id: workspace_id.clone(),
            repository,
            cache_id,
            cache_owner_workspace_id: workspace_id,
            cache_namespace_digest,
            cache_present: true,
            trusted_evidence_digest,
            workspace_location_identity,
            workspace_root: PathBuf::from("/private/test-workspace"),
            cache_path: PathBuf::from("/private/test-workspace/target"),
        }
    }

    /// Combine descriptor-derived identity with separately observed non-identity preflight evidence.
    ///
    /// This performs no readiness decision and does not reopen either private path.
    ///
    /// # Errors
    ///
    /// Returns a bounded construction error when the pure preflight receipt rejects the supplied
    /// source, capability, resource, command, or authority evidence.
    pub fn bind_preflight_evidence(
        self,
        evidence: TrustedRunnerPreflightEvidence,
    ) -> Result<TrustedRunnerWorkspaceReceipt, TrustedWorkspaceReceiptError> {
        TrustedRunnerWorkspaceReceipt::new(TrustedRunnerWorkspaceReceiptDefinition {
            repository: self.repository,
            installation_id: self.installation_id,
            workspace_id: self.workspace_id.clone(),
            cleanliness: evidence.cleanliness,
            resolved_refs: evidence.resolved_refs,
            tested_commit: evidence.tested_commit,
            tested_tree: evidence.tested_tree,
            cache_id: self.cache_id,
            cache_owner_workspace_id: self.cache_owner_workspace_id,
            cache_namespace_digest: self.cache_namespace_digest,
            cache_present: self.cache_present,
            resources: evidence.resources,
            capabilities: evidence.capabilities,
            selected_command: evidence.selected_command,
            requested_authorities: evidence.requested_authorities,
            private_evidence: TrustedRunnerPrivateEvidence::new(
                self.workspace_root,
                self.cache_path,
            ),
        })
        .map_err(|_| {
            TrustedWorkspaceReceiptError::new(
                TrustedWorkspaceReceiptErrorKind::ReceiptConstruction,
                "preflight_receipt",
                "descriptor-bound identity could not be combined with preflight evidence",
            )
        })
    }
}

impl fmt::Debug for TrustedWorkspaceCacheReceipt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TrustedWorkspaceCacheReceipt")
            .field("schema_version", &self.schema_version)
            .field("identity_generation", &self.identity_generation)
            .field("installation_id", &self.installation_id)
            .field("workspace_id", &self.workspace_id)
            .field("repository", &self.repository)
            .field("cache_id", &self.cache_id)
            .field("cache_owner_workspace_id", &self.cache_owner_workspace_id)
            .field("cache_namespace_digest", &self.cache_namespace_digest)
            .field("cache_present", &self.cache_present)
            .field("trusted_evidence_digest", &self.trusted_evidence_digest)
            .field("workspace_root", &"<private-path>")
            .field("cache_path", &"<private-path>")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedRunnerPreflightEvidence {
    pub cleanliness: WorkspaceCleanliness,
    pub resolved_refs: Vec<ResolvedRef>,
    pub tested_commit: CommitId,
    pub tested_tree: GitTreeId,
    pub resources: HostResourceObservation,
    pub capabilities: Vec<CapabilityObservation>,
    pub selected_command: RepositoryCommandIdentity,
    pub requested_authorities: BTreeSet<RequestedAuthority>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TrustedWorkspaceRootBinding {
    root: SelectedStateRoot,
    identity_generation: ProjectWorkspaceIdentityGeneration,
}

fn select_trusted_workspace_root(selection: StateRootSelection) -> TrustedWorkspaceRootBinding {
    let root = select_state_root(selection);
    let identity_generation = match root.generation() {
        StateRootGeneration::SmolrunnerLegacyV1 => ProjectWorkspaceIdentityGeneration::SmolrunnerV1,
        StateRootGeneration::GlaedaCurrentV1 => ProjectWorkspaceIdentityGeneration::GlaedaV2,
    };
    TrustedWorkspaceRootBinding {
        root,
        identity_generation,
    }
}

/// Produce one trusted workspace/cache receipt from the canonical protected state root.
///
/// The project identity is only a lookup key. Installation, workspace, cache, paths, ownership, and
/// namespace identities are derived from fixed protected state records and independently opened
/// filesystem descriptors. Repository code cannot select a state root, installation, record name,
/// workspace path, cache path, workspace ID, cache ID, or namespace digest.
///
/// # Errors
///
/// Returns a bounded error for missing or ambiguous project state, malformed records, unsafe state
/// permissions, symbolic aliases, path escape, ownership drift, replacement races, or incompatible
/// durable identity evidence.
pub fn produce_default_trusted_workspace_cache_receipt(
    project: &ProjectIdentity,
) -> Result<TrustedWorkspaceCacheReceipt, TrustedWorkspaceReceiptError> {
    let binding = select_trusted_workspace_root(StateRootSelection::Current);
    produce_with_hook_for_generation(
        binding.root.fixed_path(),
        project,
        binding.identity_generation,
        || {},
    )
}

#[cfg(test)]
fn produce_trusted_workspace_cache_receipt(
    root_path: &Path,
    project: &ProjectIdentity,
) -> Result<TrustedWorkspaceCacheReceipt, TrustedWorkspaceReceiptError> {
    produce_trusted_workspace_cache_receipt_for_generation(
        root_path,
        project,
        ProjectWorkspaceIdentityGeneration::CURRENT,
    )
}

#[cfg(test)]
fn produce_trusted_workspace_cache_receipt_for_generation(
    root_path: &Path,
    project: &ProjectIdentity,
    identity_generation: ProjectWorkspaceIdentityGeneration,
) -> Result<TrustedWorkspaceCacheReceipt, TrustedWorkspaceReceiptError> {
    produce_with_hook_for_generation(root_path, project, identity_generation, || {})
}

#[cfg(test)]
fn produce_with_hook(
    root_path: &Path,
    project: &ProjectIdentity,
    after_open: impl FnOnce(),
) -> Result<TrustedWorkspaceCacheReceipt, TrustedWorkspaceReceiptError> {
    produce_with_hook_for_generation(
        root_path,
        project,
        ProjectWorkspaceIdentityGeneration::CURRENT,
        after_open,
    )
}

fn produce_with_hook_for_generation(
    root_path: &Path,
    project: &ProjectIdentity,
    identity_generation: ProjectWorkspaceIdentityGeneration,
    after_open: impl FnOnce(),
) -> Result<TrustedWorkspaceCacheReceipt, TrustedWorkspaceReceiptError> {
    let root = open_root(root_path)?;
    let root_stat = inspect_managed_directory(root.as_fd(), None, "state_root")?;
    let owner = (root_stat.st_uid, root_stat.st_gid);
    let installations = open_fixed_directory(
        root.as_fd(),
        INSTALLATIONS_DIRECTORY,
        owner,
        "installations",
    )?;
    let installation = locate_installation(&installations, owner, project)?;
    let resources = open_fixed_directory(
        installation.directory.as_fd(),
        RESOURCES_DIRECTORY,
        owner,
        "resources",
    )?;

    let workspace_record = read_resource_record(&resources, owner, WORKSPACE_RESOURCE_FILE)?;
    let cache_record = read_resource_record(&resources, owner, CACHE_RESOURCE_FILE)?;
    validate_resource_marker(
        &workspace_record.marker,
        &installation.id,
        project,
        "workspace_record",
    )?;
    validate_resource_marker(
        &cache_record.marker,
        &installation.id,
        project,
        "cache_record",
    )?;

    let workspace_path = marker_directory_path(&workspace_record.marker, "workspace_record")?;
    let cache_path = marker_directory_path(&cache_record.marker, "cache_record")?;
    let cache_relative = cache_path.strip_prefix(&workspace_path).map_err(|_| {
        identity_error(
            "cache",
            "durable cache path is outside the durable workspace root",
        )
    })?;
    if cache_relative.as_os_str().is_empty() {
        return Err(identity_error(
            "cache",
            "durable cache path must be a strict child of the workspace root",
        ));
    }

    let workspace = open_absolute_directory(&workspace_path, "workspace")?;
    validate_observed_directory(
        &workspace_record.marker,
        &installation.id,
        &workspace,
        None,
        "workspace",
    )?;
    let cache = open_relative_directory(
        workspace.directory.as_fd(),
        cache_relative,
        cache_path.clone(),
        "cache",
    )?;
    validate_observed_directory(
        &cache_record.marker,
        &installation.id,
        &cache,
        Some((workspace.stat.st_uid, workspace.stat.st_gid)),
        "cache",
    )?;

    after_open();

    verify_directory_entry(
        &installations,
        installation.id.as_str(),
        &installation.directory,
        "installation",
    )?;
    verify_file_entry(
        installation.directory.as_fd(),
        PROJECT_FILE,
        &installation.project.file,
        "project_record",
    )?;
    verify_directory_entry(
        &installation.directory,
        RESOURCES_DIRECTORY,
        &resources,
        "resources",
    )?;
    verify_file_entry(
        resources.as_fd(),
        WORKSPACE_RESOURCE_FILE,
        &workspace_record.file,
        "workspace_record",
    )?;
    verify_file_entry(
        resources.as_fd(),
        CACHE_RESOURCE_FILE,
        &cache_record.file,
        "cache_record",
    )?;
    verify_opened_directory(&workspace, "workspace")?;
    verify_opened_directory(&cache, "cache")?;

    let installation_id = RunnerInstallationId::parse(installation.id.as_str())
        .map_err(|_| corrupt_error("installation", "durable installation ID is invalid"))?;
    let repository = RepositoryRef::parse(&project.repository)
        .map_err(|_| corrupt_error("project_record", "durable repository identity is invalid"))?;
    let workspace_identity_material = serde_json::to_vec(&workspace_record.marker.resource)
        .map_err(|_| {
            corrupt_error(
                "workspace_record",
                "workspace identity could not be encoded",
            )
        })?;
    let workspace_id_digest = trusted_workspace_identity(
        identity_generation,
        TrustedWorkspaceIdentityKind::WorkspaceId,
        [
            installation.id.as_str().as_bytes(),
            workspace_identity_material.as_slice(),
        ],
    )
    .map_err(|_| corrupt_error("workspace_record", "derived workspace ID is invalid"))?;
    let workspace_id_hex = workspace_id_digest
        .as_str()
        .strip_prefix("sha256:")
        .ok_or_else(|| corrupt_error("workspace_record", "derived workspace ID is invalid"))?;
    let workspace_id = RunnerWorkspaceId::parse(&format!("workspace-{}", &workspace_id_hex[..40]))
        .map_err(|_| corrupt_error("workspace_record", "derived workspace ID is invalid"))?;
    let cache_id = CacheId::parse(CACHE_ID)
        .map_err(|_| corrupt_error("cache_record", "fixed cache ID is invalid"))?;
    let cache_identity_material = serde_json::to_vec(&cache_record.marker.resource)
        .map_err(|_| corrupt_error("cache_record", "cache identity could not be encoded"))?;
    let cache_namespace_digest = trusted_workspace_identity(
        identity_generation,
        TrustedWorkspaceIdentityKind::CacheNamespace,
        [
            installation.id.as_str().as_bytes(),
            workspace_id.as_str().as_bytes(),
            cache_identity_material.as_slice(),
        ],
    )
    .map_err(|_| corrupt_error("cache_record", "cache identity could not be encoded"))?;

    let workspace_stat_evidence = stat_evidence(&workspace.stat);
    let cache_stat_evidence = stat_evidence(&cache.stat);
    let trusted_evidence_digest = trusted_workspace_identity(
        identity_generation,
        TrustedWorkspaceIdentityKind::Evidence,
        [
            installation.project.bytes.as_slice(),
            workspace_record.bytes.as_slice(),
            cache_record.bytes.as_slice(),
            workspace_stat_evidence.as_slice(),
            cache_stat_evidence.as_slice(),
        ],
    )
    .map_err(|_| corrupt_error("evidence", "workspace evidence could not be encoded"))?;
    let workspace_location_identity = RepositoryWorkspaceLocationIdentity::from_validated(
        workspace_path.clone(),
        workspace.stat.st_dev,
        workspace.stat.st_ino,
    );

    Ok(TrustedWorkspaceCacheReceipt {
        schema_version: TRUSTED_WORKSPACE_RECEIPT_SCHEMA_VERSION,
        identity_generation,
        installation_id,
        workspace_id: workspace_id.clone(),
        repository,
        cache_id,
        cache_owner_workspace_id: workspace_id,
        cache_namespace_digest,
        cache_present: true,
        trusted_evidence_digest,
        workspace_location_identity,
        workspace_root: workspace_path,
        cache_path,
    })
}

struct LocatedInstallation {
    id: InstallationId,
    directory: OwnedFd,
    project: OpenedStateFile,
}

fn locate_installation(
    installations: &OwnedFd,
    owner: (u32, u32),
    project: &ProjectIdentity,
) -> Result<LocatedInstallation, TrustedWorkspaceReceiptError> {
    let mut entries = Dir::read_from(installations).map_err(|_| {
        io_error(
            "installation",
            "installation catalog could not be enumerated",
        )
    })?;
    let mut count = 0_usize;
    let mut found = None;
    for entry in &mut entries {
        let entry = entry.map_err(|_| {
            io_error(
                "installation",
                "installation catalog entry could not be read",
            )
        })?;
        let name = entry.file_name().to_bytes();
        if name == b"." || name == b".." {
            continue;
        }
        count += 1;
        if count > MAX_INSTALLATIONS {
            return Err(corrupt_error(
                "installation",
                "installation catalog exceeds the reviewed entry limit",
            ));
        }
        let name = std::str::from_utf8(name).map_err(|_| {
            corrupt_error(
                "installation",
                "installation directory name is not valid UTF-8",
            )
        })?;
        let id = InstallationId::parse(name)
            .map_err(|_| corrupt_error("installation", "installation directory name is invalid"))?;
        let directory = fs::openat(
            installations.as_fd(),
            id.as_str(),
            DIRECTORY_FLAGS,
            Mode::empty(),
        )
        .map_err(|error| map_open_error(error, "installation", false))?;
        inspect_managed_directory(directory.as_fd(), Some(owner), "installation")?;
        let project_file = read_project_record(&directory, owner)?;
        if project_file.document.installation_id() != &id {
            return Err(corrupt_error(
                "project_record",
                "project record installation ID differs from its directory",
            ));
        }
        if project_file.document.project() == project {
            if found.is_some() {
                return Err(TrustedWorkspaceReceiptError::new(
                    TrustedWorkspaceReceiptErrorKind::AmbiguousState,
                    "installation",
                    "multiple protected installations claim the requested project",
                ));
            }
            found = Some(LocatedInstallation {
                id,
                directory,
                project: project_file.opened,
            });
        }
    }
    found.ok_or_else(|| {
        TrustedWorkspaceReceiptError::new(
            TrustedWorkspaceReceiptErrorKind::MissingState,
            "installation",
            "no protected installation claims the requested project",
        )
    })
}

struct DecodedProjectRecord {
    document: ProjectStateDocument,
    opened: OpenedStateFile,
}

struct OpenedStateFile {
    file: File,
    bytes: Vec<u8>,
}

struct OpenedResourceRecord {
    marker: OwnershipMarker,
    file: File,
    bytes: Vec<u8>,
}

fn read_project_record(
    installation: &OwnedFd,
    owner: (u32, u32),
) -> Result<DecodedProjectRecord, TrustedWorkspaceReceiptError> {
    let opened = read_state_file(installation.as_fd(), PROJECT_FILE, owner, "project_record")?;
    let input = std::str::from_utf8(&opened.bytes)
        .map_err(|_| corrupt_error("project_record", "project record is not valid UTF-8"))?;
    let document = decode_state_document(input)
        .map_err(|_| corrupt_error("project_record", "project record is invalid"))?;
    match document {
        StateDocument::Project(document) => Ok(DecodedProjectRecord { document, opened }),
        StateDocument::Resource(_) => Err(corrupt_error(
            "project_record",
            "project path contains a resource record",
        )),
    }
}

fn read_resource_record(
    resources: &OwnedFd,
    owner: (u32, u32),
    name: &'static str,
) -> Result<OpenedResourceRecord, TrustedWorkspaceReceiptError> {
    let stage = if name == WORKSPACE_RESOURCE_FILE {
        "workspace_record"
    } else {
        "cache_record"
    };
    let opened = read_state_file(resources.as_fd(), name, owner, stage)?;
    let input = std::str::from_utf8(&opened.bytes)
        .map_err(|_| corrupt_error(stage, "resource record is not valid UTF-8"))?;
    let document = decode_state_document(input)
        .map_err(|_| corrupt_error(stage, "resource record is invalid"))?;
    match document {
        StateDocument::Resource(document) => Ok(OpenedResourceRecord {
            marker: document.marker().clone(),
            file: opened.file,
            bytes: opened.bytes,
        }),
        StateDocument::Project(_) => Err(corrupt_error(
            stage,
            "resource path contains a project record",
        )),
    }
}

fn read_state_file(
    parent: BorrowedFd<'_>,
    name: &'static str,
    owner: (u32, u32),
    stage: &'static str,
) -> Result<OpenedStateFile, TrustedWorkspaceReceiptError> {
    let descriptor = fs::openat(parent, name, FILE_FLAGS, Mode::empty())
        .map_err(|error| map_open_error(error, stage, true))?;
    let stat = fs::fstat(&descriptor)
        .map_err(|_| io_error(stage, "protected state record could not be inspected"))?;
    validate_state_file_stat(&stat, owner, stage)?;
    let file = File::from(descriptor);
    let mut reader = (&file).take((MAX_STATE_DOCUMENT_BYTES + 1) as u64);
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .map_err(|_| io_error(stage, "protected state record could not be read"))?;
    if bytes.len() > MAX_STATE_DOCUMENT_BYTES {
        return Err(corrupt_error(
            stage,
            "protected state record exceeds the size limit",
        ));
    }
    Ok(OpenedStateFile { file, bytes })
}

fn open_root(path: &Path) -> Result<OwnedFd, TrustedWorkspaceReceiptError> {
    fs::open(path, DIRECTORY_FLAGS, Mode::empty())
        .map_err(|error| map_open_error(error, "state_root", true))
}

fn open_fixed_directory(
    parent: BorrowedFd<'_>,
    name: &'static str,
    owner: (u32, u32),
    stage: &'static str,
) -> Result<OwnedFd, TrustedWorkspaceReceiptError> {
    let directory = fs::openat(parent, name, DIRECTORY_FLAGS, Mode::empty())
        .map_err(|error| map_open_error(error, stage, true))?;
    inspect_managed_directory(directory.as_fd(), Some(owner), stage)?;
    Ok(directory)
}

fn inspect_managed_directory(
    directory: BorrowedFd<'_>,
    expected_owner: Option<(u32, u32)>,
    stage: &'static str,
) -> Result<rustix::fs::Stat, TrustedWorkspaceReceiptError> {
    let stat = fs::fstat(directory)
        .map_err(|_| io_error(stage, "protected state directory could not be inspected"))?;
    if !FileType::from_raw_mode(stat.st_mode).is_dir() {
        return Err(unsafe_error(
            stage,
            "protected state object is not a directory",
        ));
    }
    if stat.st_mode & 0o7777 != 0o750 {
        return Err(unsafe_error(
            stage,
            "protected state directory does not have mode 0750",
        ));
    }
    if expected_owner.is_some_and(|owner| owner != (stat.st_uid, stat.st_gid)) {
        return Err(unsafe_error(
            stage,
            "protected state directory owner or group changed",
        ));
    }
    Ok(stat)
}

fn validate_state_file_stat(
    stat: &rustix::fs::Stat,
    owner: (u32, u32),
    stage: &'static str,
) -> Result<(), TrustedWorkspaceReceiptError> {
    if !FileType::from_raw_mode(stat.st_mode).is_file() {
        return Err(unsafe_error(
            stage,
            "protected state record is not a regular file",
        ));
    }
    if stat.st_nlink != 1 {
        return Err(unsafe_error(
            stage,
            "protected state record has multiple hard links",
        ));
    }
    if stat.st_mode & 0o7777 != 0o600 {
        return Err(unsafe_error(
            stage,
            "protected state record does not have mode 0600",
        ));
    }
    if owner != (stat.st_uid, stat.st_gid) {
        return Err(unsafe_error(
            stage,
            "protected state record owner or group changed",
        ));
    }
    if stat.st_size < 0 || stat.st_size as u64 > MAX_STATE_DOCUMENT_BYTES as u64 {
        return Err(corrupt_error(
            stage,
            "protected state record exceeds the size limit",
        ));
    }
    Ok(())
}

fn validate_resource_marker(
    marker: &OwnershipMarker,
    installation_id: &InstallationId,
    project: &ProjectIdentity,
    stage: &'static str,
) -> Result<(), TrustedWorkspaceReceiptError> {
    if marker.installation_id != installation_id.as_str() || marker.project != *project {
        return Err(identity_error(
            stage,
            "resource record is not bound to the exact protected installation and project",
        ));
    }
    if marker.resource.kind != ResourceKind::Directory {
        return Err(corrupt_error(
            stage,
            "resource record does not describe a directory",
        ));
    }
    Ok(())
}

fn marker_directory_path(
    marker: &OwnershipMarker,
    stage: &'static str,
) -> Result<PathBuf, TrustedWorkspaceReceiptError> {
    let path = PathBuf::from(&marker.resource.locator);
    validate_absolute_path(&path).map_err(|_| {
        corrupt_error(
            stage,
            "resource record contains a noncanonical private directory path",
        )
    })?;
    Ok(path)
}

struct OpenedDirectory {
    directory: OwnedFd,
    parent: OwnedFd,
    name: OsString,
    stat: rustix::fs::Stat,
    path: PathBuf,
}

fn open_absolute_directory(
    path: &Path,
    stage: &'static str,
) -> Result<OpenedDirectory, TrustedWorkspaceReceiptError> {
    validate_absolute_path(path)?;
    let components = normal_components(path)?;
    let current = fs::open("/", DIRECTORY_FLAGS, Mode::empty())
        .map_err(|_| io_error(stage, "filesystem root could not be opened"))?;
    open_directory_components(current, &components, path.to_path_buf(), stage)
}

fn open_relative_directory(
    start: BorrowedFd<'_>,
    path: &Path,
    absolute_path: PathBuf,
    stage: &'static str,
) -> Result<OpenedDirectory, TrustedWorkspaceReceiptError> {
    if path.is_absolute() {
        return Err(identity_error(
            stage,
            "cache containment evidence is invalid",
        ));
    }
    let components = path
        .components()
        .map(|component| match component {
            Component::Normal(value) => Ok(value.to_os_string()),
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => Err(identity_error(
                stage,
                "cache containment evidence is invalid",
            )),
        })
        .collect::<Result<Vec<_>, _>>()?;
    if components.is_empty() {
        return Err(identity_error(
            stage,
            "cache containment evidence is invalid",
        ));
    }
    let current = fs::openat(start, ".", DIRECTORY_FLAGS, Mode::empty())
        .map_err(|_| io_error(stage, "workspace descriptor could not be retained"))?;
    open_directory_components(current, &components, absolute_path, stage)
}

fn open_directory_components(
    mut current: OwnedFd,
    components: &[OsString],
    absolute_path: PathBuf,
    stage: &'static str,
) -> Result<OpenedDirectory, TrustedWorkspaceReceiptError> {
    for (index, component) in components.iter().enumerate() {
        let final_component = index + 1 == components.len();
        if final_component {
            let directory = fs::openat(
                current.as_fd(),
                component.as_os_str(),
                DIRECTORY_FLAGS,
                Mode::empty(),
            )
            .map_err(|error| map_open_error(error, stage, true))?;
            let stat = fs::fstat(&directory)
                .map_err(|_| io_error(stage, "directory identity could not be inspected"))?;
            return Ok(OpenedDirectory {
                directory,
                parent: current,
                name: component.clone(),
                stat,
                path: absolute_path,
            });
        }
        current = fs::openat(
            current.as_fd(),
            component.as_os_str(),
            DIRECTORY_FLAGS,
            Mode::empty(),
        )
        .map_err(|error| map_open_error(error, stage, true))?;
        let stat = fs::fstat(&current)
            .map_err(|_| io_error(stage, "directory parent could not be inspected"))?;
        if !FileType::from_raw_mode(stat.st_mode).is_dir() {
            return Err(unsafe_error(
                stage,
                "directory path contains a non-directory parent",
            ));
        }
    }
    Err(identity_error(stage, "directory path is empty"))
}

fn validate_observed_directory(
    marker: &OwnershipMarker,
    installation_id: &InstallationId,
    observed: &OpenedDirectory,
    expected_owner: Option<(u32, u32)>,
    stage: &'static str,
) -> Result<(), TrustedWorkspaceReceiptError> {
    if !FileType::from_raw_mode(observed.stat.st_mode).is_dir() {
        return Err(unsafe_error(
            stage,
            "observed filesystem object is not a directory",
        ));
    }
    let owner = (observed.stat.st_uid, observed.stat.st_gid);
    if owner.0 == 0 || owner.1 == 0 {
        return Err(unsafe_error(
            stage,
            "runner-owned directory has a root owner or group",
        ));
    }
    if expected_owner.is_some_and(|expected| expected != owner) {
        return Err(identity_error(
            stage,
            "workspace and cache ownership differ",
        ));
    }
    let mode = observed.stat.st_mode & 0o7777;
    if mode & 0o022 != 0 {
        return Err(unsafe_error(
            stage,
            "runner-owned directory is writable by an untrusted identity",
        ));
    }
    let path = observed
        .path
        .to_str()
        .ok_or_else(|| corrupt_error(stage, "resource record path is not valid UTF-8"))?;
    let exact = ResourceIdentity::directory(
        path,
        installation_id.as_str(),
        observed.stat.st_uid,
        observed.stat.st_gid,
        mode,
    )
    .map_err(|_| corrupt_error(stage, "observed directory identity is invalid"))?;
    if exact != marker.resource {
        return Err(identity_error(
            stage,
            "observed directory identity differs from protected durable state",
        ));
    }
    Ok(())
}

fn verify_opened_directory(
    opened: &OpenedDirectory,
    stage: &'static str,
) -> Result<(), TrustedWorkspaceReceiptError> {
    let held = fs::fstat(&opened.directory)
        .map_err(|_| io_error(stage, "held directory identity could not be rechecked"))?;
    if !same_identity(&opened.stat, &held) {
        return Err(identity_error(
            stage,
            "held directory identity changed during observation",
        ));
    }
    let path = fs::statat(
        opened.parent.as_fd(),
        opened.name.as_os_str(),
        AtFlags::SYMLINK_NOFOLLOW,
    )
    .map_err(|_| io_error(stage, "directory path identity could not be rechecked"))?;
    if !same_identity(&opened.stat, &path) {
        return Err(identity_error(
            stage,
            "directory path identity changed during observation",
        ));
    }
    Ok(())
}

fn verify_directory_entry(
    parent: &OwnedFd,
    name: &str,
    directory: &OwnedFd,
    stage: &'static str,
) -> Result<(), TrustedWorkspaceReceiptError> {
    let held = fs::fstat(directory)
        .map_err(|_| io_error(stage, "held directory identity could not be rechecked"))?;
    let path = fs::statat(parent.as_fd(), name, AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|_| io_error(stage, "directory path identity could not be rechecked"))?;
    if !same_identity(&held, &path) {
        return Err(identity_error(
            stage,
            "protected directory was replaced during observation",
        ));
    }
    Ok(())
}

fn verify_file_entry(
    parent: BorrowedFd<'_>,
    name: &str,
    file: &File,
    stage: &'static str,
) -> Result<(), TrustedWorkspaceReceiptError> {
    let held = fs::fstat(file)
        .map_err(|_| io_error(stage, "held state record identity could not be rechecked"))?;
    let path = fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|_| io_error(stage, "state record path identity could not be rechecked"))?;
    if !same_identity(&held, &path) {
        return Err(identity_error(
            stage,
            "protected state record was replaced during observation",
        ));
    }
    Ok(())
}

fn same_identity(left: &rustix::fs::Stat, right: &rustix::fs::Stat) -> bool {
    left.st_dev == right.st_dev
        && left.st_ino == right.st_ino
        && left.st_uid == right.st_uid
        && left.st_gid == right.st_gid
        && left.st_mode == right.st_mode
        && left.st_nlink == right.st_nlink
}

fn validate_absolute_path(path: &Path) -> Result<(), TrustedWorkspaceReceiptError> {
    let valid = path.is_absolute()
        && path != Path::new("/")
        && path.to_str().is_some_and(|value| {
            !value.is_empty()
                && value.len() <= 4_096
                && !value.ends_with('/')
                && !value.contains("//")
                && !value.chars().any(char::is_control)
        })
        && path
            .components()
            .all(|component| matches!(component, Component::RootDir | Component::Normal(_)));
    if valid {
        Ok(())
    } else {
        Err(identity_error(
            "filesystem_path",
            "durable filesystem path is not a canonical non-root absolute path",
        ))
    }
}

fn normal_components(path: &Path) -> Result<Vec<OsString>, TrustedWorkspaceReceiptError> {
    let components = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_os_string()),
            Component::RootDir => None,
            Component::CurDir | Component::ParentDir | Component::Prefix(_) => None,
        })
        .collect::<Vec<_>>();
    if components.is_empty() {
        Err(identity_error(
            "filesystem_path",
            "durable filesystem path is empty",
        ))
    } else {
        Ok(components)
    }
}

fn stat_evidence(stat: &rustix::fs::Stat) -> Vec<u8> {
    let mut evidence = Vec::with_capacity(40);
    evidence.extend_from_slice(&stat.st_dev.to_be_bytes());
    evidence.extend_from_slice(&stat.st_ino.to_be_bytes());
    evidence.extend_from_slice(&u64::from(stat.st_uid).to_be_bytes());
    evidence.extend_from_slice(&u64::from(stat.st_gid).to_be_bytes());
    evidence.extend_from_slice(&u64::from(stat.st_mode).to_be_bytes());
    evidence
}

fn map_open_error(
    error: Errno,
    stage: &'static str,
    missing_is_missing: bool,
) -> TrustedWorkspaceReceiptError {
    match error {
        Errno::NOENT if missing_is_missing => TrustedWorkspaceReceiptError::new(
            TrustedWorkspaceReceiptErrorKind::MissingState,
            stage,
            "required protected state or filesystem object is missing",
        ),
        Errno::LOOP | Errno::NOTDIR => unsafe_error(
            stage,
            "protected traversal encountered a symbolic link or invalid object",
        ),
        _ => io_error(stage, "protected object could not be opened"),
    }
}

fn io_error(stage: &'static str, message: impl Into<String>) -> TrustedWorkspaceReceiptError {
    TrustedWorkspaceReceiptError::new(TrustedWorkspaceReceiptErrorKind::Io, stage, message)
}

fn unsafe_error(stage: &'static str, message: impl Into<String>) -> TrustedWorkspaceReceiptError {
    TrustedWorkspaceReceiptError::new(
        TrustedWorkspaceReceiptErrorKind::UnsafeFilesystem,
        stage,
        message,
    )
}

fn corrupt_error(stage: &'static str, message: impl Into<String>) -> TrustedWorkspaceReceiptError {
    TrustedWorkspaceReceiptError::new(
        TrustedWorkspaceReceiptErrorKind::CorruptState,
        stage,
        message,
    )
}

fn identity_error(stage: &'static str, message: impl Into<String>) -> TrustedWorkspaceReceiptError {
    TrustedWorkspaceReceiptError::new(
        TrustedWorkspaceReceiptErrorKind::IdentityMismatch,
        stage,
        message,
    )
}

#[cfg(test)]
mod tests;
