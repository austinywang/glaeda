//! Crash-safe private per-user storage for verified Glaeda binary generations.
//!
//! The store owns only binary-generation persistence. It does not build binaries, inspect or
//! modify `PATH`, switch a launcher, adopt legacy SmolRunner state, or grant execution authority.
//! All owned path components are fixed or derived from canonical digests and are traversed through
//! retained directory descriptors.

use std::ffi::OsStr;
use std::fmt;
use std::fs::File;
use std::io::{Read as _, Seek as _, Write as _};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::FileExt as _;
use std::path::{Component, Path, PathBuf};

use rustix::fs::{self, AtFlags, Dir, FileType, FlockOperation, Mode, OFlags, RenameFlags};
use rustix::io::Errno;
use rustix::process::{getegid, geteuid};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::artifact::{CommitId, GitTreeId, Sha256Digest};
use crate::local_install_plan::{
    BuiltLocalBinaryEvidence, InstalledLocalBinaryGeneration, LocalInstallBuildPlan,
    LocalInstallGenerationIdentity, LocalInstallIdentityGeneration, LocalInstallSourceIdentity,
    LocalInstallState, LocalInstallToolchainIdentity, complete_local_install_build,
};

pub const LOCAL_INSTALL_GENERATION_STORE_SCHEMA_VERSION: u8 = 1;
pub const MAX_LOCAL_INSTALL_GENERATION_DOCUMENT_BYTES: usize = 16 * 1024;
pub const MAX_LOCAL_INSTALL_CURRENT_DOCUMENT_BYTES: usize = 4 * 1024;
pub const MAX_LOCAL_INSTALL_BINARY_BYTES: u64 = 512 * 1024 * 1024;
const MAX_LOCAL_INSTALL_STORE_IDENTITY_BYTES: usize = 1024;

#[cfg(target_os = "linux")]
const GLAEDA_DIRECTORY: &str = "glaeda";
#[cfg(target_os = "macos")]
const GLAEDA_DIRECTORY: &str = "Glaeda";
const LOCAL_INSTALL_DIRECTORY: &str = "local-install";
const STORE_IDENTITY_FILE: &str = "store.identity.json";
const INITIALIZATION_STAGE_PREFIX: &str = ".local-install.init-";
const INITIALIZATION_RANDOM_BYTES: usize = 16;
const INITIALIZATION_CREATE_ATTEMPTS: usize = 4;
const SYSTEM_RANDOM_SOURCE: &str = "/dev/urandom";
const LOCK_FILE: &str = "lock";
const CURRENT_FILE: &str = "current";
const CURRENT_NEXT_FILE: &str = "current.next";
const GENERATIONS_DIRECTORY: &str = "generations";
const STAGED_DIRECTORY: &str = "staged";
const GENERATION_DOCUMENT: &str = "generation.json";
const BINARY_FILE: &str = "glaeda";
const OPERATION_PREFIX: &str = "op-";
const RETIREMENT_PREFIX: &str = "retire-";
const GENERATION_PREFIX: &str = "g";
const SHA256_PREFIX: &str = "sha256:";
const MAX_GENERATION_ENTRIES: usize = 4;
const MAX_STAGED_ENTRIES: usize = 2;

const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const EXISTING_FILE_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::NONBLOCK)
    .union(OFlags::CLOEXEC);
const EXISTING_LOCK_FLAGS: OFlags = OFlags::RDWR.union(OFlags::NOFOLLOW).union(OFlags::CLOEXEC);
const NEW_FILE_FLAGS: OFlags = OFlags::WRONLY
    .union(OFlags::CREATE)
    .union(OFlags::EXCL)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const NEW_LOCK_FLAGS: OFlags = EXISTING_LOCK_FLAGS
    .union(OFlags::CREATE)
    .union(OFlags::EXCL);
const PRIVATE_DIRECTORY_MODE: Mode = Mode::RUSR.union(Mode::WUSR).union(Mode::XUSR);
const PRIVATE_FILE_MODE: Mode = Mode::RUSR.union(Mode::WUSR);
const PRIVATE_BINARY_MODE: Mode = Mode::RUSR.union(Mode::XUSR);

/// Closed namespace generation for fresh current-product state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalInstallStoreGeneration {
    GlaedaCurrentV1,
}

/// Public class of the automatically selected private user-data location.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalInstallStoreLocationClass {
    LinuxXdgDataHome,
    LinuxHomeDefault,
    MacosApplicationSupport,
}

/// Canonical caller identity for one replayable publication operation.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct LocalInstallStoreOperationId(String);

impl LocalInstallStoreOperationId {
    /// Parse canonical SHA-256 operation identity.
    ///
    /// # Errors
    ///
    /// Returns an error unless `value` is `sha256:` plus 64 lowercase hexadecimal characters.
    pub fn parse(value: &str) -> Result<Self, LocalInstallGenerationStoreError> {
        if !is_sha256(value) {
            return Err(store_error(
                LocalInstallGenerationStoreErrorKind::InvalidRequest,
                "the local-install operation identity is invalid",
            ));
        }
        Ok(Self(value.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn directory_name(&self) -> String {
        format!("{OPERATION_PREFIX}{}", &self.0[SHA256_PREFIX.len()..])
    }
}

impl fmt::Debug for LocalInstallStoreOperationId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("LocalInstallStoreOperationId")
            .field(&self.0)
            .finish()
    }
}

/// One exact CAS-bound request to persist an already verified generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalInstallGenerationPublishRequest {
    operation_id: LocalInstallStoreOperationId,
    expected_predecessor: Option<LocalInstallGenerationIdentity>,
    candidate: InstalledLocalBinaryGeneration,
    request_digest: Sha256Digest,
}

impl LocalInstallGenerationPublishRequest {
    /// Bind a current-Glaeda candidate to its exact predecessor and replay identity.
    ///
    /// This re-derives the generation identity from its source and artifact evidence. Legacy
    /// identity generations and non-successor candidates are refused before filesystem access.
    ///
    /// # Errors
    ///
    /// Returns a bounded error when the request is internally inconsistent.
    pub fn new(
        operation_id: LocalInstallStoreOperationId,
        expected_predecessor: Option<LocalInstallGenerationIdentity>,
        candidate: InstalledLocalBinaryGeneration,
    ) -> Result<Self, LocalInstallGenerationStoreError> {
        validate_candidate(&candidate, expected_predecessor.as_ref())?;
        let request_digest = digest_serialized(&RequestIdentityWire {
            operation_id: operation_id.as_str(),
            expected_predecessor: expected_predecessor.as_ref().map(IdentityEncodeWire::from),
            candidate: GenerationEncodeWire::from(&candidate),
        })?;
        Ok(Self {
            operation_id,
            expected_predecessor,
            candidate,
            request_digest,
        })
    }

    #[must_use]
    pub const fn operation_id(&self) -> &LocalInstallStoreOperationId {
        &self.operation_id
    }

    #[must_use]
    pub const fn expected_predecessor(&self) -> Option<&LocalInstallGenerationIdentity> {
        self.expected_predecessor.as_ref()
    }

    #[must_use]
    pub const fn candidate(&self) -> &InstalledLocalBinaryGeneration {
        &self.candidate
    }
}

/// Narrow authority of a verified store snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalInstallGenerationStoreAuthority {
    PrivateGenerationSnapshotOnly,
}

/// One fully verified accepted state loaded through the atomic current document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalInstallGenerationStoreSnapshot {
    store_generation: LocalInstallStoreGeneration,
    state: LocalInstallState,
    last_operation: Option<LocalInstallStoreOperationId>,
}

impl LocalInstallGenerationStoreSnapshot {
    #[must_use]
    pub const fn store_generation(&self) -> LocalInstallStoreGeneration {
        self.store_generation
    }

    #[must_use]
    pub const fn authority(&self) -> LocalInstallGenerationStoreAuthority {
        LocalInstallGenerationStoreAuthority::PrivateGenerationSnapshotOnly
    }

    #[must_use]
    pub const fn state(&self) -> &LocalInstallState {
        &self.state
    }

    #[must_use]
    pub const fn last_operation(&self) -> Option<&LocalInstallStoreOperationId> {
        self.last_operation.as_ref()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalInstallGenerationPublishDisposition {
    Published,
    Replayed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalInstallGenerationPublishReceipt {
    disposition: LocalInstallGenerationPublishDisposition,
    location: LocalInstallStoreLocationClass,
    generation: LocalInstallGenerationIdentity,
    state: LocalInstallGenerationStoreSnapshot,
}

impl LocalInstallGenerationPublishReceipt {
    #[must_use]
    pub const fn disposition(&self) -> LocalInstallGenerationPublishDisposition {
        self.disposition
    }

    #[must_use]
    pub const fn location(&self) -> LocalInstallStoreLocationClass {
        self.location
    }

    #[must_use]
    pub const fn generation(&self) -> &LocalInstallGenerationIdentity {
        &self.generation
    }

    #[must_use]
    pub const fn state(&self) -> &LocalInstallGenerationStoreSnapshot {
        &self.state
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalInstallGenerationRecoveryDisposition {
    Clean,
    CompletedStagedGeneration,
    CompletedCurrentSwitch,
    RemovedDuplicateStage,
    RemovedCleanupDebt,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalInstallGenerationRecoveryReceipt {
    disposition: LocalInstallGenerationRecoveryDisposition,
    state: LocalInstallGenerationStoreSnapshot,
}

impl LocalInstallGenerationRecoveryReceipt {
    #[must_use]
    pub const fn disposition(&self) -> LocalInstallGenerationRecoveryDisposition {
        self.disposition
    }

    #[must_use]
    pub const fn state(&self) -> &LocalInstallGenerationStoreSnapshot {
        &self.state
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalInstallGenerationStoreErrorKind {
    UnsupportedPlatform,
    InvalidLocation,
    InvalidRequest,
    Busy,
    Conflict,
    RecoveryRequired,
    CorruptState,
    UnsafeFilesystem,
    Io,
    InjectedFailure,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalInstallGenerationStoreError {
    kind: LocalInstallGenerationStoreErrorKind,
    message: String,
}

impl LocalInstallGenerationStoreError {
    #[must_use]
    pub const fn kind(&self) -> LocalInstallGenerationStoreErrorKind {
        self.kind
    }
}

impl fmt::Display for LocalInstallGenerationStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for LocalInstallGenerationStoreError {}

/// Descriptor-retained private user-local Glaeda binary-generation store.
pub struct UnixLocalInstallGenerationStore {
    root_parent: OwnedFd,
    root: OwnedFd,
    identity: OwnedFd,
    generations: OwnedFd,
    staged: OwnedFd,
    lock: OwnedFd,
    root_path: PathBuf,
    owner: (u32, u32),
    location: LocalInstallStoreLocationClass,
}

/// Crate-private descriptor-verified target for canonical launcher composition.
pub(crate) struct LocalInstallLauncherTarget {
    pub(crate) generation: InstalledLocalBinaryGeneration,
    pub(crate) path: PathBuf,
    binary: OwnedFd,
}

/// Shared-lock target set retained across one launcher observation/publication operation.
pub(crate) struct LockedLocalInstallLauncherTargets {
    targets: Vec<LocalInstallLauncherTarget>,
    _lock: StoreLock,
}

impl LockedLocalInstallLauncherTargets {
    pub(crate) fn as_slice(&self) -> &[LocalInstallLauncherTarget] {
        &self.targets
    }
}

impl fmt::Debug for LockedLocalInstallLauncherTargets {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LockedLocalInstallLauncherTargets")
            .field("targets", &self.targets)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for LocalInstallLauncherTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalInstallLauncherTarget")
            .field("generation", &self.generation.identity)
            .field("path", &"<private-local-install-binary-path>")
            .finish()
    }
}

impl LocalInstallLauncherTarget {
    /// Prove that the absolute path embedded in a launcher still resolves to the retained,
    /// fully verified generation binary. This deliberately repeats lexical resolution from the
    /// filesystem root: retained store descriptors alone cannot prove what a later `execve` of
    /// an absolute symlink target will reach after an ancestor namespace rebind.
    pub(crate) fn verify_resolved_path(&self) -> Result<(), LocalInstallGenerationStoreError> {
        let resolved = open_absolute_private_file(
            &self.path,
            (geteuid().as_raw(), getegid().as_raw()),
            PRIVATE_BINARY_MODE,
            "resolved launcher target",
        )?;
        let retained = inspect_private_file(
            &self.binary,
            (geteuid().as_raw(), getegid().as_raw()),
            PRIVATE_BINARY_MODE,
            None,
            "retained launcher target",
        )?;
        let current = inspect_private_file(
            &resolved,
            (geteuid().as_raw(), getegid().as_raw()),
            PRIVATE_BINARY_MODE,
            None,
            "resolved launcher target",
        )?;
        if !same_file_snapshot(&retained, &current) {
            return Err(store_error(
                LocalInstallGenerationStoreErrorKind::UnsafeFilesystem,
                "the absolute launcher target no longer resolves to the verified generation",
            ));
        }
        Ok(())
    }
}

impl fmt::Debug for UnixLocalInstallGenerationStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UnixLocalInstallGenerationStore")
            .field("location", &self.location)
            .field("path", &"<private-local-install-store-path>")
            .finish_non_exhaustive()
    }
}

impl UnixLocalInstallGenerationStore {
    #[cfg(test)]
    pub(crate) fn open_for_test(parent: &Path) -> Result<Self, LocalInstallGenerationStoreError> {
        Self::open_beneath(
            open_absolute_directory(parent.as_os_str())?,
            &[LOCAL_INSTALL_DIRECTORY],
            LocalInstallStoreLocationClass::LinuxHomeDefault,
            parent.join(LOCAL_INSTALL_DIRECTORY),
        )
    }

    /// Select and open the current user's fixed Glaeda store location.
    ///
    /// Linux uses `${XDG_DATA_HOME}/glaeda/local-install` when the variable is nonempty, otherwise
    /// `$HOME/.local/share/glaeda/local-install`. macOS uses
    /// `$HOME/Library/Application Support/Glaeda/local-install`. No SmolRunner path is probed.
    ///
    /// # Errors
    ///
    /// Returns a bounded error for an unavailable location, unsafe path component, or I/O failure.
    pub fn open_current_user() -> Result<Self, LocalInstallGenerationStoreError> {
        let home = std::env::var_os("HOME");
        #[cfg(target_os = "linux")]
        {
            if let Some(xdg) = std::env::var_os("XDG_DATA_HOME").filter(|value| !value.is_empty()) {
                let base = open_absolute_directory(&xdg)?;
                return Self::open_beneath(
                    base,
                    &[GLAEDA_DIRECTORY, LOCAL_INSTALL_DIRECTORY],
                    LocalInstallStoreLocationClass::LinuxXdgDataHome,
                    PathBuf::from(xdg)
                        .join(GLAEDA_DIRECTORY)
                        .join(LOCAL_INSTALL_DIRECTORY),
                );
            }
            let home = home.ok_or_else(|| {
                store_error(
                    LocalInstallGenerationStoreErrorKind::InvalidLocation,
                    "the operator home directory is unavailable",
                )
            })?;
            let base = open_absolute_directory(&home)?;
            return Self::open_beneath(
                base,
                &[".local", "share", GLAEDA_DIRECTORY, LOCAL_INSTALL_DIRECTORY],
                LocalInstallStoreLocationClass::LinuxHomeDefault,
                PathBuf::from(home)
                    .join(".local")
                    .join("share")
                    .join(GLAEDA_DIRECTORY)
                    .join(LOCAL_INSTALL_DIRECTORY),
            );
        }
        #[cfg(target_os = "macos")]
        {
            let home = home.ok_or_else(|| {
                store_error(
                    LocalInstallGenerationStoreErrorKind::InvalidLocation,
                    "the operator home directory is unavailable",
                )
            })?;
            let base = open_absolute_directory(&home)?;
            return Self::open_beneath(
                base,
                &[
                    "Library",
                    "Application Support",
                    GLAEDA_DIRECTORY,
                    LOCAL_INSTALL_DIRECTORY,
                ],
                LocalInstallStoreLocationClass::MacosApplicationSupport,
                PathBuf::from(home)
                    .join("Library")
                    .join("Application Support")
                    .join(GLAEDA_DIRECTORY)
                    .join(LOCAL_INSTALL_DIRECTORY),
            );
        }
        #[allow(unreachable_code)]
        Err(store_error(
            LocalInstallGenerationStoreErrorKind::UnsupportedPlatform,
            "the reviewed local-install store is unavailable on this platform",
        ))
    }

    fn open_beneath(
        mut parent: OwnedFd,
        components: &[&str],
        location: LocalInstallStoreLocationClass,
        root_path: PathBuf,
    ) -> Result<Self, LocalInstallGenerationStoreError> {
        let owner = (geteuid().as_raw(), getegid().as_raw());
        let (root_name, parents) = components.split_last().ok_or_else(|| {
            store_error(
                LocalInstallGenerationStoreErrorKind::InvalidLocation,
                "the local-install store location is incomplete",
            )
        })?;
        for (index, component) in parents.iter().enumerate() {
            let exact_private = index + 1 >= parents.len();
            parent = ensure_directory(&parent, component, owner, exact_private)?;
        }
        let root_parent = parent;
        let (root, identity) = open_or_create_store_root(&root_parent, root_name, owner, location)?;
        let generations = ensure_directory(&root, GENERATIONS_DIRECTORY, owner, true)?;
        let staged = ensure_directory(&root, STAGED_DIRECTORY, owner, true)?;
        let lock = ensure_lock_file(&root, owner)?;
        synchronize_directory(&root, "local-install store root")?;
        Ok(Self {
            root_parent,
            root,
            identity,
            generations,
            staged,
            lock,
            root_path,
            owner,
            location,
        })
    }

    /// Return only the accepted and retained binary targets after a complete shared-lock load.
    ///
    /// The paths remain crate-private and are intended solely for the fixed launcher publisher.
    /// The returned generations have already passed canonical document, ownership, mode, link,
    /// byte-length and SHA-256 verification. Recovery debt fails before any target is returned.
    pub(crate) fn launcher_targets(
        &self,
    ) -> Result<LockedLocalInstallLauncherTargets, LocalInstallGenerationStoreError> {
        let guard = self.acquire_lock(StoreLockMode::Shared)?;
        self.verify_absolute_launcher_root()?;
        if self.has_recovery_debt()? {
            return Err(store_error(
                LocalInstallGenerationStoreErrorKind::RecoveryRequired,
                "the local-install generation store requires writer recovery",
            ));
        }
        let current = self.load_current_document_locked()?;
        let mut targets = Vec::with_capacity(2);
        if let Some(accepted) = current.snapshot.state().accepted.as_ref() {
            targets.push(self.launcher_target(accepted)?);
        }
        if let Some(retained) = current.snapshot.state().retained.first() {
            targets.push(self.launcher_target(retained)?);
        }
        Ok(LockedLocalInstallLauncherTargets {
            targets,
            _lock: guard,
        })
    }

    fn launcher_target(
        &self,
        generation: &InstalledLocalBinaryGeneration,
    ) -> Result<LocalInstallLauncherTarget, LocalInstallGenerationStoreError> {
        let name = generation_directory_name(&generation.identity)?;
        let directory = fs::openat(&self.generations, &name, DIRECTORY_FLAGS, Mode::empty())
            .map_err(|_| {
                store_error(
                    LocalInstallGenerationStoreErrorKind::RecoveryRequired,
                    "a launcher generation could not be reopened safely",
                )
            })?;
        inspect_directory(
            &directory,
            Some(self.owner),
            true,
            "launcher generation directory",
        )?;
        let (loaded, binary) =
            self.open_complete_generation_directory(&directory, None, Some(&generation.identity))?;
        verify_retained_directory(
            &self.generations,
            &name,
            &directory,
            self.owner,
            "launcher generation directory",
        )?;
        if loaded.generation != *generation {
            return Err(store_error(
                LocalInstallGenerationStoreErrorKind::Conflict,
                "the launcher generation changed after current-state verification",
            ));
        }
        let target = LocalInstallLauncherTarget {
            generation: generation.clone(),
            path: self
                .root_path
                .join(GENERATIONS_DIRECTORY)
                .join(name)
                .join(BINARY_FILE),
            binary,
        };
        target.verify_resolved_path()?;
        Ok(target)
    }

    fn verify_absolute_launcher_root(&self) -> Result<(), LocalInstallGenerationStoreError> {
        let current = open_absolute_directory(self.root_path.as_os_str())?;
        let retained = inspect_directory(
            &self.root,
            Some(self.owner),
            true,
            "retained local-install store root",
        )?;
        let resolved = inspect_directory(
            &current,
            Some(self.owner),
            true,
            "resolved local-install store root",
        )?;
        if !same_object(&retained, &resolved) {
            return Err(store_error(
                LocalInstallGenerationStoreErrorKind::UnsafeFilesystem,
                "the absolute local-install store path no longer reaches the retained root",
            ));
        }
        Ok(())
    }

    /// Load only a clean, fully verified atomic current state.
    ///
    /// Readers take a nonblocking shared lock, so they either observe one complete current
    /// generation or receive `Busy`; they never consume half-published state.
    ///
    /// # Errors
    ///
    /// Returns a bounded error for contention, recovery debt, corrupt state, or unsafe files.
    pub fn load(
        &self,
    ) -> Result<LocalInstallGenerationStoreSnapshot, LocalInstallGenerationStoreError> {
        let _guard = self.acquire_lock(StoreLockMode::Shared)?;
        if self.has_recovery_debt()? {
            return Err(store_error(
                LocalInstallGenerationStoreErrorKind::RecoveryRequired,
                "the local-install generation store requires writer recovery",
            ));
        }
        self.load_current_locked()
    }

    /// Recover one exact unambiguous interrupted transition under the writer lock.
    ///
    /// # Errors
    ///
    /// Refuses ambiguous, incomplete, foreign, or corrupt debt without replacing current state.
    pub fn recover(
        &self,
    ) -> Result<LocalInstallGenerationRecoveryReceipt, LocalInstallGenerationStoreError> {
        let _guard = self.acquire_lock(StoreLockMode::Exclusive)?;
        let disposition = self.recover_locked()?;
        Ok(LocalInstallGenerationRecoveryReceipt {
            disposition,
            state: self.load_current_locked()?,
        })
    }

    /// Publish one exact verified candidate binary with nonblocking CAS and replay semantics.
    ///
    /// The source path is opened without following its final component, hashed from a retained
    /// descriptor, and copied into a new private single-link inode. Its digest must match the
    /// candidate before staging begins.
    ///
    /// # Errors
    ///
    /// Returns a bounded error for contention, stale CAS evidence, changed-input replay, unsafe
    /// source/store state, recovery ambiguity, or I/O failure.
    pub fn publish(
        &self,
        request: &LocalInstallGenerationPublishRequest,
        candidate_binary: impl AsRef<Path>,
    ) -> Result<LocalInstallGenerationPublishReceipt, LocalInstallGenerationStoreError> {
        self.publish_inner(request, candidate_binary.as_ref(), None)
    }

    fn publish_inner(
        &self,
        request: &LocalInstallGenerationPublishRequest,
        candidate_binary: &Path,
        fault: Option<FaultBoundary>,
    ) -> Result<LocalInstallGenerationPublishReceipt, LocalInstallGenerationStoreError> {
        let _guard = self.acquire_lock(StoreLockMode::Exclusive)?;
        self.recover_locked()?;
        let current = self.load_current_document_locked()?;
        if let Some((operation, request_digest)) = &current.operation
            && operation == &request.operation_id
        {
            if request_digest == &request.request_digest
                && current.accepted_identity.as_ref() == Some(&request.candidate.identity)
            {
                return Ok(LocalInstallGenerationPublishReceipt {
                    disposition: LocalInstallGenerationPublishDisposition::Replayed,
                    location: self.location,
                    generation: request.candidate.identity.clone(),
                    state: current.snapshot,
                });
            }
            return Err(store_error(
                LocalInstallGenerationStoreErrorKind::Conflict,
                "the local-install operation identity was reused with changed inputs",
            ));
        }
        if current
            .retained_operation
            .as_ref()
            .is_some_and(|(operation, _)| operation == &request.operation_id)
        {
            return Err(store_error(
                LocalInstallGenerationStoreErrorKind::Conflict,
                "the local-install operation identity already belongs to a retained generation",
            ));
        }
        if current.accepted_identity.as_ref() != request.expected_predecessor.as_ref() {
            return Err(store_error(
                LocalInstallGenerationStoreErrorKind::Conflict,
                "the accepted local-install generation changed since it was observed",
            ));
        }
        let source = open_verified_source_binary(
            candidate_binary,
            self.owner,
            &request.candidate.binary_digest,
        )?;
        let artifact = ArtifactWire {
            file_type: "regular_file".to_owned(),
            owner: "store_owner".to_owned(),
            mode: "0500".to_owned(),
            links: 1,
            bytes: source.bytes,
        };
        let document_bytes = encode_generation_document(request, &artifact)?;
        let stage_name = request.operation_id.directory_name();
        let stage =
            create_private_directory(&self.staged, &stage_name, self.owner, "staged operation")?;
        maybe_fail(fault, FaultBoundary::StageDirectoryCreated)?;

        let binary = create_private_file(
            &stage,
            BINARY_FILE,
            PRIVATE_BINARY_MODE,
            self.owner,
            "staged Glaeda binary",
        )?;
        copy_source_binary(&source.file, &binary, source.bytes)?;
        maybe_fail(fault, FaultBoundary::BinaryWritten)?;
        fs::fsync(&binary).map_err(|_| {
            store_error(
                LocalInstallGenerationStoreErrorKind::Io,
                "the staged Glaeda binary could not be synchronized",
            )
        })?;
        inspect_private_file(
            &binary,
            self.owner,
            PRIVATE_BINARY_MODE,
            Some(source.bytes),
            "staged Glaeda binary",
        )?;
        maybe_fail(fault, FaultBoundary::BinarySynchronized)?;

        let document = create_private_file(
            &stage,
            GENERATION_DOCUMENT,
            PRIVATE_FILE_MODE,
            self.owner,
            "staged generation document",
        )?;
        write_all_fd(&document, &document_bytes, "staged generation document")?;
        maybe_fail(fault, FaultBoundary::DocumentWritten)?;
        fs::fsync(&document).map_err(|_| {
            store_error(
                LocalInstallGenerationStoreErrorKind::Io,
                "the staged generation document could not be synchronized",
            )
        })?;
        inspect_private_file(
            &document,
            self.owner,
            PRIVATE_FILE_MODE,
            Some(document_bytes.len() as u64),
            "staged generation document",
        )?;
        maybe_fail(fault, FaultBoundary::DocumentSynchronized)?;
        synchronize_directory(&stage, "staged generation directory")?;
        maybe_fail(fault, FaultBoundary::StageDirectorySynchronized)?;
        self.verify_complete_generation_directory(
            &stage,
            Some(&request.operation_id),
            Some(&request.candidate.identity),
        )?;

        let generation_name = generation_directory_name(&request.candidate.identity)?;
        fs::renameat_with(
            &self.staged,
            &stage_name,
            &self.generations,
            &generation_name,
            RenameFlags::NOREPLACE,
        )
        .map_err(|error| match error {
            Errno::EXIST => store_error(
                LocalInstallGenerationStoreErrorKind::Conflict,
                "the candidate generation directory already exists",
            ),
            _ => store_error(
                LocalInstallGenerationStoreErrorKind::Io,
                "the staged generation could not be published",
            ),
        })?;
        maybe_fail(fault, FaultBoundary::GenerationPublished)?;
        synchronize_directory(&self.generations, "local-install generations directory")?;
        maybe_fail(fault, FaultBoundary::GenerationsDirectorySynchronized)?;

        let next = CurrentEncodeWire {
            store_schema_version: LOCAL_INSTALL_GENERATION_STORE_SCHEMA_VERSION,
            store_generation: StoreGenerationWire::GlaedaCurrentV1,
            accepted: Some(IdentityEncodeWire::from(&request.candidate.identity)),
            retained: current
                .accepted_identity
                .as_ref()
                .map(IdentityEncodeWire::from),
            retiring: current
                .retained_identity
                .as_ref()
                .map(IdentityEncodeWire::from),
            last_operation: Some(OperationEncodeWire {
                id: request.operation_id.as_str(),
                request_digest: request.request_digest.as_str(),
            }),
        };
        let next_bytes = encode_current(&next)?;
        self.stage_and_switch_current(&next_bytes, fault)?;
        maybe_fail(fault, FaultBoundary::CleanupStarted)?;
        self.cleanup_unreferenced_generations_locked(fault)?;
        maybe_fail(fault, FaultBoundary::CleanupFinished)?;
        let state = self.load_current_locked()?;
        Ok(LocalInstallGenerationPublishReceipt {
            disposition: LocalInstallGenerationPublishDisposition::Published,
            location: self.location,
            generation: request.candidate.identity.clone(),
            state,
        })
    }

    fn stage_and_switch_current(
        &self,
        bytes: &[u8],
        fault: Option<FaultBoundary>,
    ) -> Result<(), LocalInstallGenerationStoreError> {
        let next = create_private_file(
            &self.root,
            CURRENT_NEXT_FILE,
            PRIVATE_FILE_MODE,
            self.owner,
            "staged current-generation document",
        )?;
        write_all_fd(&next, bytes, "staged current-generation document")?;
        fs::fsync(&next).map_err(|_| {
            store_error(
                LocalInstallGenerationStoreErrorKind::Io,
                "the staged current-generation document could not be synchronized",
            )
        })?;
        decode_current_bytes(bytes)?;
        synchronize_directory(&self.root, "local-install store root")?;
        maybe_fail(fault, FaultBoundary::CurrentDocumentSynchronized)?;
        fs::renameat(&self.root, CURRENT_NEXT_FILE, &self.root, CURRENT_FILE).map_err(|_| {
            store_error(
                LocalInstallGenerationStoreErrorKind::Io,
                "the current local-install generation could not be switched",
            )
        })?;
        maybe_fail(fault, FaultBoundary::CurrentSwitched)?;
        synchronize_directory(&self.root, "local-install store root")?;
        maybe_fail(fault, FaultBoundary::RootSynchronized)
    }

    fn acquire_lock(
        &self,
        mode: StoreLockMode,
    ) -> Result<StoreLock, LocalInstallGenerationStoreError> {
        self.acquire_lock_with_observer(mode, |_| {})
    }

    fn acquire_lock_with_observer(
        &self,
        mode: StoreLockMode,
        after_lock: impl FnOnce(&OwnedFd),
    ) -> Result<StoreLock, LocalInstallGenerationStoreError> {
        let retained = inspect_private_file(
            &self.lock,
            self.owner,
            PRIVATE_FILE_MODE,
            Some(0),
            "local-install store lock",
        )?;
        let lock = fs::openat(&self.root, LOCK_FILE, EXISTING_LOCK_FLAGS, Mode::empty()).map_err(
            |_| {
                store_error(
                    LocalInstallGenerationStoreErrorKind::UnsafeFilesystem,
                    "the local-install store lock could not be reopened safely",
                )
            },
        )?;
        let current = inspect_private_file(
            &lock,
            self.owner,
            PRIVATE_FILE_MODE,
            Some(0),
            "local-install store lock",
        )?;
        if !same_object(&retained, &current) {
            return Err(store_error(
                LocalInstallGenerationStoreErrorKind::UnsafeFilesystem,
                "the local-install store lock identity changed",
            ));
        }
        let operation = match mode {
            StoreLockMode::Shared => FlockOperation::NonBlockingLockShared,
            StoreLockMode::Exclusive => FlockOperation::NonBlockingLockExclusive,
        };
        match fs::flock(&lock, operation) {
            Ok(()) => {
                // Install the explicit-unlock guard before any fallible validation.
                // Closing a bare descriptor can leave a duplicate holding the flock.
                let guard = StoreLock { lock };
                after_lock(&guard.lock);
                // This is the single retained-boundary pass for every locked public operation.
                self.verify_boundaries()?;
                Ok(guard)
            }
            Err(Errno::AGAIN) => Err(store_error(
                LocalInstallGenerationStoreErrorKind::Busy,
                "another local-install store operation holds the writer lock",
            )),
            Err(_) => Err(store_error(
                LocalInstallGenerationStoreErrorKind::Io,
                "the local-install store lock could not be acquired",
            )),
        }
    }

    fn verify_boundaries(&self) -> Result<(), LocalInstallGenerationStoreError> {
        verify_retained_directory(
            &self.root_parent,
            LOCAL_INSTALL_DIRECTORY,
            &self.root,
            self.owner,
            "local-install store root",
        )?;
        verify_retained_file(
            &self.root,
            STORE_IDENTITY_FILE,
            &self.identity,
            self.owner,
            PRIVATE_FILE_MODE,
            "local-install store identity",
        )?;
        verify_store_identity_document(&self.root, &self.identity, self.owner, self.location)?;
        verify_retained_directory(
            &self.root,
            GENERATIONS_DIRECTORY,
            &self.generations,
            self.owner,
            "local-install generations directory",
        )?;
        verify_retained_directory(
            &self.root,
            STAGED_DIRECTORY,
            &self.staged,
            self.owner,
            "local-install staged directory",
        )?;
        self.inspect_root_entries()?;
        Ok(())
    }

    fn inspect_root_entries(&self) -> Result<(), LocalInstallGenerationStoreError> {
        let mut entries = Dir::read_from(&self.root).map_err(|_| {
            store_error(
                LocalInstallGenerationStoreErrorKind::Io,
                "the local-install store root could not be enumerated",
            )
        })?;
        let mut lock = false;
        let mut identity = false;
        let mut generations = false;
        let mut staged = false;
        let mut count = 0_usize;
        for entry in &mut entries {
            let entry = entry.map_err(|_| {
                store_error(
                    LocalInstallGenerationStoreErrorKind::Io,
                    "a local-install store root entry could not be read",
                )
            })?;
            let name = entry.file_name().to_bytes();
            if matches!(name, b"." | b"..") {
                continue;
            }
            count += 1;
            if count > 7 {
                return Err(store_error(
                    LocalInstallGenerationStoreErrorKind::CorruptState,
                    "the local-install store root contains too many entries",
                ));
            }
            match name {
                b"store.identity.json" => identity = true,
                b"lock" => lock = true,
                b"current" | b"current.next" => {}
                b"generations" => generations = true,
                b"staged" => staged = true,
                _ => {
                    return Err(store_error(
                        LocalInstallGenerationStoreErrorKind::CorruptState,
                        "the local-install store root contains an unexpected entry",
                    ));
                }
            }
        }
        if !identity || !lock || !generations || !staged {
            return Err(store_error(
                LocalInstallGenerationStoreErrorKind::UnsafeFilesystem,
                "a required local-install store entry disappeared",
            ));
        }
        Ok(())
    }

    fn has_recovery_debt(&self) -> Result<bool, LocalInstallGenerationStoreError> {
        let staged = self.inspect_staged_entries()?;
        if entry_exists(&self.root, CURRENT_NEXT_FILE)?
            || !staged.operations.is_empty()
            || !staged.retirements.is_empty()
        {
            return Ok(true);
        }
        let current = self.load_current_document_locked()?;
        if current.retiring_identity.is_some() {
            return Ok(true);
        }
        let referenced = referenced_generation_names(&current)?;
        Ok(self
            .generation_names()?
            .iter()
            .any(|name| !referenced.contains(name)))
    }

    fn inspect_staged_entries(&self) -> Result<StagedEntries, LocalInstallGenerationStoreError> {
        let names = enumerate_names(
            &self.staged,
            MAX_STAGED_ENTRIES,
            |name| is_operation_directory_name(name) || is_retirement_directory_name(name),
            "local-install staged directory",
        )?;
        let mut operations = Vec::new();
        let mut retirements = Vec::new();
        for name in names {
            if is_operation_directory_name(&name) {
                operations.push(name);
            } else {
                retirements.push(name);
            }
        }
        Ok(StagedEntries {
            operations,
            retirements,
        })
    }

    fn generation_names(&self) -> Result<Vec<String>, LocalInstallGenerationStoreError> {
        enumerate_names(
            &self.generations,
            MAX_GENERATION_ENTRIES,
            is_generation_directory_name,
            "local-install generations directory",
        )
    }

    fn load_current_locked(
        &self,
    ) -> Result<LocalInstallGenerationStoreSnapshot, LocalInstallGenerationStoreError> {
        Ok(self.load_current_document_locked()?.snapshot)
    }

    fn load_current_document_locked(
        &self,
    ) -> Result<LoadedCurrent, LocalInstallGenerationStoreError> {
        let file = match open_existing_file(
            &self.root,
            CURRENT_FILE,
            self.owner,
            PRIVATE_FILE_MODE,
            "current-generation document",
        ) {
            Ok(file) => file,
            Err(error) if error.kind == LocalInstallGenerationStoreErrorKind::RecoveryRequired => {
                return Ok(LoadedCurrent {
                    snapshot: LocalInstallGenerationStoreSnapshot {
                        store_generation: LocalInstallStoreGeneration::GlaedaCurrentV1,
                        state: LocalInstallState::new(None, Vec::new())
                            .expect("empty state is valid"),
                        last_operation: None,
                    },
                    operation: None,
                    retained_operation: None,
                    accepted_identity: None,
                    retained_identity: None,
                    retiring_identity: None,
                    bytes: Vec::new(),
                });
            }
            Err(error) => return Err(error),
        };
        let bytes = read_bounded_file(
            &file,
            self.owner,
            PRIVATE_FILE_MODE,
            MAX_LOCAL_INSTALL_CURRENT_DOCUMENT_BYTES,
            "current-generation document",
        )?;
        verify_retained_file(
            &self.root,
            CURRENT_FILE,
            &file,
            self.owner,
            PRIVATE_FILE_MODE,
            "current-generation document",
        )?;
        self.load_current_from_bytes(bytes)
    }

    fn load_current_next_locked(
        &self,
    ) -> Result<Option<LoadedCurrent>, LocalInstallGenerationStoreError> {
        let file = match open_existing_file(
            &self.root,
            CURRENT_NEXT_FILE,
            self.owner,
            PRIVATE_FILE_MODE,
            "staged current-generation document",
        ) {
            Ok(file) => file,
            Err(error) if error.kind == LocalInstallGenerationStoreErrorKind::RecoveryRequired => {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let bytes = read_bounded_file(
            &file,
            self.owner,
            PRIVATE_FILE_MODE,
            MAX_LOCAL_INSTALL_CURRENT_DOCUMENT_BYTES,
            "staged current-generation document",
        )?;
        Ok(Some(self.load_current_from_bytes(bytes)?))
    }

    fn load_current_from_bytes(
        &self,
        bytes: Vec<u8>,
    ) -> Result<LoadedCurrent, LocalInstallGenerationStoreError> {
        let wire = decode_current_bytes(&bytes)?;
        if wire.accepted.is_none() != wire.last_operation.is_none()
            || (wire.accepted.is_none() && (wire.retained.is_some() || wire.retiring.is_some()))
        {
            return Err(store_error(
                LocalInstallGenerationStoreErrorKind::CorruptState,
                "the current-generation document has an invalid empty-state shape",
            ));
        }
        let accepted_identity = wire.accepted.as_ref().map(parse_identity).transpose()?;
        let retained_identity = wire.retained.as_ref().map(parse_identity).transpose()?;
        let retiring_identity = wire.retiring.as_ref().map(parse_identity).transpose()?;
        let accepted = accepted_identity
            .as_ref()
            .map(|identity| self.load_generation(identity))
            .transpose()?;
        let retained = retained_identity
            .as_ref()
            .map(|identity| self.load_generation(identity))
            .transpose()?;
        if let Some(accepted) = &accepted
            && accepted.generation.predecessor.as_ref() != retained_identity.as_ref()
        {
            return Err(store_error(
                LocalInstallGenerationStoreErrorKind::CorruptState,
                "the accepted generation predecessor does not match the retained generation",
            ));
        }
        if let (Some(accepted), Some(retained)) = (&accepted, &retained)
            && (retained.generation.identity != retained_identity.clone().expect("present")
                || accepted.generation.identity.number != retained.generation.identity.number + 1)
        {
            return Err(store_error(
                LocalInstallGenerationStoreErrorKind::CorruptState,
                "the retained generation is not the exact accepted predecessor",
            ));
        }
        if let Some(retiring) = &retiring_identity {
            let Some(retained) = &retained else {
                return Err(store_error(
                    LocalInstallGenerationStoreErrorKind::CorruptState,
                    "cleanup debt exists without a retained predecessor",
                ));
            };
            if retained.generation.predecessor.as_ref() != Some(retiring) {
                return Err(store_error(
                    LocalInstallGenerationStoreErrorKind::CorruptState,
                    "cleanup debt is not the exact predecessor of the retained generation",
                ));
            }
        }
        let operation = wire
            .last_operation
            .as_ref()
            .map(parse_operation)
            .transpose()?;
        let retained_operation = retained
            .as_ref()
            .map(|value| (value.operation_id.clone(), value.request_digest.clone()));
        if let (Some((accepted_operation, _)), Some((retained_operation, _))) =
            (&operation, &retained_operation)
            && accepted_operation == retained_operation
        {
            return Err(store_error(
                LocalInstallGenerationStoreErrorKind::CorruptState,
                "accepted and retained generations reuse one operation identity",
            ));
        }
        if let (Some(accepted), Some((operation_id, request_digest))) = (&accepted, &operation)
            && (&accepted.operation_id != operation_id
                || &accepted.request_digest != request_digest)
        {
            return Err(store_error(
                LocalInstallGenerationStoreErrorKind::CorruptState,
                "the current operation record does not match the accepted generation",
            ));
        }
        let state = LocalInstallState::new(
            accepted.as_ref().map(|value| value.generation.clone()),
            retained
                .as_ref()
                .map(|value| vec![value.generation.clone()])
                .unwrap_or_default(),
        )
        .map_err(|_| {
            store_error(
                LocalInstallGenerationStoreErrorKind::CorruptState,
                "the current local-install state violates its generation bound",
            )
        })?;
        Ok(LoadedCurrent {
            snapshot: LocalInstallGenerationStoreSnapshot {
                store_generation: LocalInstallStoreGeneration::GlaedaCurrentV1,
                state,
                last_operation: operation.as_ref().map(|value| value.0.clone()),
            },
            operation,
            retained_operation,
            accepted_identity,
            retained_identity,
            retiring_identity,
            bytes,
        })
    }

    fn load_generation(
        &self,
        identity: &LocalInstallGenerationIdentity,
    ) -> Result<LoadedGeneration, LocalInstallGenerationStoreError> {
        let name = generation_directory_name(identity)?;
        let directory = fs::openat(&self.generations, &name, DIRECTORY_FLAGS, Mode::empty())
            .map_err(|error| match error {
                Errno::NOENT => store_error(
                    LocalInstallGenerationStoreErrorKind::RecoveryRequired,
                    "a referenced local-install generation is missing",
                ),
                Errno::LOOP | Errno::NOTDIR => store_error(
                    LocalInstallGenerationStoreErrorKind::UnsafeFilesystem,
                    "a local-install generation directory is symlinked or invalid",
                ),
                _ => store_error(
                    LocalInstallGenerationStoreErrorKind::Io,
                    "a local-install generation directory could not be opened",
                ),
            })?;
        inspect_directory(
            &directory,
            Some(self.owner),
            true,
            "local-install generation directory",
        )?;
        let loaded = self.verify_complete_generation_directory(&directory, None, Some(identity))?;
        verify_retained_directory(
            &self.generations,
            &name,
            &directory,
            self.owner,
            "local-install generation directory",
        )?;
        Ok(loaded)
    }

    fn verify_complete_generation_directory(
        &self,
        directory: &OwnedFd,
        expected_operation: Option<&LocalInstallStoreOperationId>,
        expected_identity: Option<&LocalInstallGenerationIdentity>,
    ) -> Result<LoadedGeneration, LocalInstallGenerationStoreError> {
        self.open_complete_generation_directory(directory, expected_operation, expected_identity)
            .map(|(loaded, _binary)| loaded)
    }

    fn open_complete_generation_directory(
        &self,
        directory: &OwnedFd,
        expected_operation: Option<&LocalInstallStoreOperationId>,
        expected_identity: Option<&LocalInstallGenerationIdentity>,
    ) -> Result<(LoadedGeneration, OwnedFd), LocalInstallGenerationStoreError> {
        let entries = enumerate_names(
            directory,
            2,
            |name| matches!(name, GENERATION_DOCUMENT | BINARY_FILE),
            "local-install generation directory",
        )?;
        if entries != [GENERATION_DOCUMENT.to_owned(), BINARY_FILE.to_owned()] {
            return Err(store_error(
                LocalInstallGenerationStoreErrorKind::RecoveryRequired,
                "the local-install generation directory is incomplete",
            ));
        }
        let document = open_existing_file(
            directory,
            GENERATION_DOCUMENT,
            self.owner,
            PRIVATE_FILE_MODE,
            "generation document",
        )?;
        let document_bytes = read_bounded_file(
            &document,
            self.owner,
            PRIVATE_FILE_MODE,
            MAX_LOCAL_INSTALL_GENERATION_DOCUMENT_BYTES,
            "generation document",
        )?;
        let loaded = decode_generation_document(&document_bytes)?;
        if expected_operation.is_some_and(|operation| operation != &loaded.operation_id)
            || expected_identity.is_some_and(|identity| identity != &loaded.generation.identity)
        {
            return Err(store_error(
                LocalInstallGenerationStoreErrorKind::Conflict,
                "the local-install generation directory does not match its canonical name",
            ));
        }
        let binary = open_existing_file(
            directory,
            BINARY_FILE,
            self.owner,
            PRIVATE_BINARY_MODE,
            "stored Glaeda binary",
        )?;
        let digest = hash_private_file(
            &binary,
            self.owner,
            PRIVATE_BINARY_MODE,
            loaded.artifact_bytes,
            "stored Glaeda binary",
        )?;
        if digest != loaded.generation.binary_digest {
            return Err(store_error(
                LocalInstallGenerationStoreErrorKind::CorruptState,
                "the stored Glaeda binary digest does not match its generation document",
            ));
        }
        verify_retained_file(
            directory,
            GENERATION_DOCUMENT,
            &document,
            self.owner,
            PRIVATE_FILE_MODE,
            "generation document",
        )?;
        verify_retained_file(
            directory,
            BINARY_FILE,
            &binary,
            self.owner,
            PRIVATE_BINARY_MODE,
            "stored Glaeda binary",
        )?;
        Ok((
            LoadedGeneration {
                document_bytes,
                ..loaded
            },
            binary,
        ))
    }

    fn recover_locked(
        &self,
    ) -> Result<LocalInstallGenerationRecoveryDisposition, LocalInstallGenerationStoreError> {
        let staged = self.inspect_staged_entries()?;
        let current_next_present = entry_exists(&self.root, CURRENT_NEXT_FILE)?;
        if staged.operations.len() > 1
            || staged.retirements.len() > 1
            || (current_next_present
                && (!staged.operations.is_empty() || !staged.retirements.is_empty()))
            || (!staged.operations.is_empty() && !staged.retirements.is_empty())
        {
            return Err(store_error(
                LocalInstallGenerationStoreErrorKind::RecoveryRequired,
                "the local-install store contains ambiguous concurrent recovery debt",
            ));
        }
        if let Some(next) = self.load_current_next_locked()? {
            let current = self.load_current_document_locked()?;
            if next.bytes == current.bytes {
                fs::unlinkat(&self.root, CURRENT_NEXT_FILE, AtFlags::empty()).map_err(|_| {
                    store_error(
                        LocalInstallGenerationStoreErrorKind::Io,
                        "duplicate staged current document could not be removed",
                    )
                })?;
                synchronize_directory(&self.root, "local-install store root")?;
                return Ok(LocalInstallGenerationRecoveryDisposition::RemovedDuplicateStage);
            }
            require_current_successor(&current, &next)?;
            fs::renameat(&self.root, CURRENT_NEXT_FILE, &self.root, CURRENT_FILE).map_err(
                |_| {
                    store_error(
                        LocalInstallGenerationStoreErrorKind::Io,
                        "the interrupted current-generation switch could not be completed",
                    )
                },
            )?;
            synchronize_directory(&self.root, "local-install store root")?;
            self.cleanup_unreferenced_generations_locked(None)?;
            return Ok(LocalInstallGenerationRecoveryDisposition::CompletedCurrentSwitch);
        }

        if let Some(retirement_name) = staged.retirements.first() {
            self.finish_retirement_locked(retirement_name, None)?;
            self.clear_retirement_marker_locked()?;
            return Ok(LocalInstallGenerationRecoveryDisposition::RemovedCleanupDebt);
        }

        if let Some(stage_name) = staged.operations.first() {
            let directory = fs::openat(&self.staged, stage_name, DIRECTORY_FLAGS, Mode::empty())
                .map_err(|_| {
                    store_error(
                        LocalInstallGenerationStoreErrorKind::RecoveryRequired,
                        "the staged local-install operation changed during recovery",
                    )
                })?;
            inspect_directory(
                &directory,
                Some(self.owner),
                true,
                "staged operation directory",
            )?;
            let operation = operation_id_from_directory_name(stage_name)?;
            let loaded =
                self.verify_complete_generation_directory(&directory, Some(&operation), None)?;
            let current = self.load_current_document_locked()?;
            if current
                .retained_operation
                .as_ref()
                .is_some_and(|(operation, _)| operation == &loaded.operation_id)
            {
                return Err(store_error(
                    LocalInstallGenerationStoreErrorKind::Conflict,
                    "the staged operation identity already belongs to a retained generation",
                ));
            }
            if current.accepted_identity.as_ref() == Some(&loaded.generation.identity) {
                if current.operation.as_ref()
                    == Some(&(loaded.operation_id.clone(), loaded.request_digest.clone()))
                {
                    remove_generation_directory(&self.staged, stage_name, &directory, self.owner)?;
                    synchronize_directory(&self.staged, "local-install staged directory")?;
                    return Ok(LocalInstallGenerationRecoveryDisposition::RemovedDuplicateStage);
                }
                return Err(store_error(
                    LocalInstallGenerationStoreErrorKind::Conflict,
                    "the staged operation conflicts with the accepted generation",
                ));
            }
            if loaded.generation.predecessor.as_ref() != current.accepted_identity.as_ref() {
                return Err(store_error(
                    LocalInstallGenerationStoreErrorKind::Conflict,
                    "the staged operation does not succeed the accepted generation",
                ));
            }
            let generation_name = generation_directory_name(&loaded.generation.identity)?;
            match fs::renameat_with(
                &self.staged,
                stage_name,
                &self.generations,
                &generation_name,
                RenameFlags::NOREPLACE,
            ) {
                Ok(()) => {}
                Err(Errno::EXIST) => {
                    let published = self.load_generation(&loaded.generation.identity)?;
                    if published.operation_id != loaded.operation_id
                        || published.request_digest != loaded.request_digest
                        || published.document_bytes != loaded.document_bytes
                    {
                        return Err(store_error(
                            LocalInstallGenerationStoreErrorKind::Conflict,
                            "the staged operation conflicts with an existing generation",
                        ));
                    }
                    remove_generation_directory(&self.staged, stage_name, &directory, self.owner)?;
                }
                Err(_) => {
                    return Err(store_error(
                        LocalInstallGenerationStoreErrorKind::Io,
                        "the staged generation could not be recovered",
                    ));
                }
            }
            synchronize_directory(&self.generations, "local-install generations directory")?;
            let next = current_successor_wire(&current, &loaded);
            let bytes = encode_current(&next)?;
            self.stage_and_switch_current(&bytes, None)?;
            self.cleanup_unreferenced_generations_locked(None)?;
            return Ok(LocalInstallGenerationRecoveryDisposition::CompletedStagedGeneration);
        }

        let current = self.load_current_document_locked()?;
        let referenced = referenced_generation_names(&current)?;
        let extras: Vec<_> = self
            .generation_names()?
            .into_iter()
            .filter(|name| !referenced.contains(name))
            .collect();
        if extras.is_empty() && current.retiring_identity.is_some() {
            self.clear_retirement_marker_locked()?;
            return Ok(LocalInstallGenerationRecoveryDisposition::RemovedCleanupDebt);
        }
        if extras.is_empty() {
            return Ok(LocalInstallGenerationRecoveryDisposition::Clean);
        }
        if extras.len() != 1 {
            return Err(store_error(
                LocalInstallGenerationStoreErrorKind::RecoveryRequired,
                "multiple unreferenced local-install generations require inspection",
            ));
        }
        let orphan_identity = identity_from_generation_directory_name(&extras[0])?;
        let orphan = self.load_generation(&orphan_identity)?;
        if current
            .retained_operation
            .as_ref()
            .is_some_and(|(operation, _)| operation == &orphan.operation_id)
        {
            return Err(store_error(
                LocalInstallGenerationStoreErrorKind::Conflict,
                "the orphan operation identity already belongs to a retained generation",
            ));
        }
        if orphan.generation.predecessor.as_ref() == current.accepted_identity.as_ref() {
            let next = current_successor_wire(&current, &orphan);
            let bytes = encode_current(&next)?;
            self.stage_and_switch_current(&bytes, None)?;
            self.cleanup_unreferenced_generations_locked(None)?;
            return Ok(LocalInstallGenerationRecoveryDisposition::CompletedCurrentSwitch);
        }
        if self.is_exact_cleanup_debt(&orphan)? {
            self.retire_loaded_generation(&extras[0], &orphan_identity, None)?;
            self.clear_retirement_marker_locked()?;
            return Ok(LocalInstallGenerationRecoveryDisposition::RemovedCleanupDebt);
        }
        Err(store_error(
            LocalInstallGenerationStoreErrorKind::RecoveryRequired,
            "an unreferenced local-install generation is not an exact recoverable transition",
        ))
    }

    fn cleanup_unreferenced_generations_locked(
        &self,
        fault: Option<FaultBoundary>,
    ) -> Result<(), LocalInstallGenerationStoreError> {
        let current = self.load_current_document_locked()?;
        let referenced = referenced_generation_names(&current)?;
        let extras: Vec<_> = self
            .generation_names()?
            .into_iter()
            .filter(|name| !referenced.contains(name))
            .collect();
        if extras.len() > 1 {
            return Err(store_error(
                LocalInstallGenerationStoreErrorKind::RecoveryRequired,
                "local-install cleanup debt exceeds the retained-generation bound",
            ));
        }
        match (current.retiring_identity.as_ref(), extras.as_slice()) {
            (None, []) => return Ok(()),
            (None, _) => {
                return Err(store_error(
                    LocalInstallGenerationStoreErrorKind::RecoveryRequired,
                    "an unreferenced generation has no durable retirement authority",
                ));
            }
            (Some(_), []) => {}
            (Some(expected), [name]) => {
                let identity = identity_from_generation_directory_name(name)?;
                if &identity != expected {
                    return Err(store_error(
                        LocalInstallGenerationStoreErrorKind::RecoveryRequired,
                        "cleanup debt does not match the durable retirement identity",
                    ));
                }
                let loaded = self.load_generation(&identity)?;
                if !self.is_exact_cleanup_debt(&loaded)? {
                    return Err(store_error(
                        LocalInstallGenerationStoreErrorKind::RecoveryRequired,
                        "an unreferenced generation is not proven superseded cleanup debt",
                    ));
                }
                self.retire_loaded_generation(name, &identity, fault)?;
            }
            _ => {
                return Err(store_error(
                    LocalInstallGenerationStoreErrorKind::CorruptState,
                    "cleanup debt violates the retained-generation bound",
                ));
            }
        }
        self.clear_retirement_marker_locked()
    }

    fn is_exact_cleanup_debt(
        &self,
        candidate: &LoadedGeneration,
    ) -> Result<bool, LocalInstallGenerationStoreError> {
        let current = self.load_current_document_locked()?;
        Ok(current.retiring_identity.as_ref() == Some(&candidate.generation.identity))
    }

    fn retire_loaded_generation(
        &self,
        name: &str,
        identity: &LocalInstallGenerationIdentity,
        fault: Option<FaultBoundary>,
    ) -> Result<(), LocalInstallGenerationStoreError> {
        let directory = fs::openat(&self.generations, name, DIRECTORY_FLAGS, Mode::empty())
            .map_err(|_| {
                store_error(
                    LocalInstallGenerationStoreErrorKind::RecoveryRequired,
                    "cleanup generation changed before removal",
                )
            })?;
        self.verify_complete_generation_directory(&directory, None, Some(identity))?;
        let retirement_name = retirement_directory_name(identity)?;
        fs::renameat_with(
            &self.generations,
            name,
            &self.staged,
            &retirement_name,
            RenameFlags::NOREPLACE,
        )
        .map_err(|error| match error {
            Errno::EXIST => store_error(
                LocalInstallGenerationStoreErrorKind::RecoveryRequired,
                "the superseded generation already has retirement debt",
            ),
            _ => store_error(
                LocalInstallGenerationStoreErrorKind::Io,
                "the superseded generation could not enter deterministic retirement",
            ),
        })?;
        verify_retained_directory(
            &self.staged,
            &retirement_name,
            &directory,
            self.owner,
            "retiring local-install generation",
        )?;
        synchronize_directory(&self.generations, "local-install generations directory")?;
        synchronize_directory(&self.staged, "local-install staged directory")?;
        maybe_fail(fault, FaultBoundary::RetirementPublished)?;
        self.finish_retirement_locked(&retirement_name, fault)
    }

    fn finish_retirement_locked(
        &self,
        retirement_name: &str,
        fault: Option<FaultBoundary>,
    ) -> Result<(), LocalInstallGenerationStoreError> {
        let identity = identity_from_retirement_directory_name(retirement_name)?;
        let current = self.load_current_document_locked()?;
        if current.accepted_identity.as_ref() == Some(&identity)
            || current.retained_identity.as_ref() == Some(&identity)
        {
            return Err(store_error(
                LocalInstallGenerationStoreErrorKind::RecoveryRequired,
                "a referenced generation cannot be retired",
            ));
        }
        if !self.is_exact_cleanup_debt_identity(&identity)? {
            return Err(store_error(
                LocalInstallGenerationStoreErrorKind::RecoveryRequired,
                "retirement debt is not the exact superseded predecessor",
            ));
        }
        let directory = fs::openat(
            &self.staged,
            retirement_name,
            DIRECTORY_FLAGS,
            Mode::empty(),
        )
        .map_err(|_| {
            store_error(
                LocalInstallGenerationStoreErrorKind::RecoveryRequired,
                "the retiring generation changed during cleanup",
            )
        })?;
        inspect_directory(
            &directory,
            Some(self.owner),
            true,
            "retiring local-install generation",
        )?;
        let entries = enumerate_names(
            &directory,
            2,
            |entry| matches!(entry, GENERATION_DOCUMENT | BINARY_FILE),
            "retiring local-install generation",
        )?;
        match entries.as_slice() {
            [document, binary] if document == GENERATION_DOCUMENT && binary == BINARY_FILE => {
                self.verify_complete_generation_directory(&directory, None, Some(&identity))?;
                fs::unlinkat(&directory, BINARY_FILE, AtFlags::empty()).map_err(|_| {
                    store_error(
                        LocalInstallGenerationStoreErrorKind::Io,
                        "the retired Glaeda binary could not be removed",
                    )
                })?;
                synchronize_directory(&directory, "retiring generation directory")?;
                maybe_fail(fault, FaultBoundary::RetirementBinaryRemoved)?;
            }
            [document] if document == GENERATION_DOCUMENT => {
                let document = open_existing_file(
                    &directory,
                    GENERATION_DOCUMENT,
                    self.owner,
                    PRIVATE_FILE_MODE,
                    "retiring generation document",
                )?;
                let bytes = read_bounded_file(
                    &document,
                    self.owner,
                    PRIVATE_FILE_MODE,
                    MAX_LOCAL_INSTALL_GENERATION_DOCUMENT_BYTES,
                    "retiring generation document",
                )?;
                let loaded = decode_generation_document(&bytes)?;
                if loaded.generation.identity != identity {
                    return Err(store_error(
                        LocalInstallGenerationStoreErrorKind::Conflict,
                        "the retiring generation document does not match its name",
                    ));
                }
            }
            [] => {}
            _ => {
                return Err(store_error(
                    LocalInstallGenerationStoreErrorKind::RecoveryRequired,
                    "the retiring generation has an unsafe partial-cleanup shape",
                ));
            }
        }
        if entry_exists(&directory, GENERATION_DOCUMENT)? {
            fs::unlinkat(&directory, GENERATION_DOCUMENT, AtFlags::empty()).map_err(|_| {
                store_error(
                    LocalInstallGenerationStoreErrorKind::Io,
                    "the retired generation document could not be removed",
                )
            })?;
            synchronize_directory(&directory, "retiring generation directory")?;
            maybe_fail(fault, FaultBoundary::RetirementDocumentRemoved)?;
        }
        verify_retained_directory(
            &self.staged,
            retirement_name,
            &directory,
            self.owner,
            "retiring local-install generation",
        )?;
        fs::unlinkat(&self.staged, retirement_name, AtFlags::REMOVEDIR).map_err(|_| {
            store_error(
                LocalInstallGenerationStoreErrorKind::Io,
                "the retired generation directory could not be removed",
            )
        })?;
        synchronize_directory(&self.staged, "local-install staged directory")
    }

    fn is_exact_cleanup_debt_identity(
        &self,
        identity: &LocalInstallGenerationIdentity,
    ) -> Result<bool, LocalInstallGenerationStoreError> {
        let current = self.load_current_document_locked()?;
        Ok(current.retiring_identity.as_ref() == Some(identity))
    }

    fn clear_retirement_marker_locked(&self) -> Result<(), LocalInstallGenerationStoreError> {
        let current = self.load_current_document_locked()?;
        if current.retiring_identity.is_none() {
            return Ok(());
        }
        let Some((operation, request_digest)) = current.operation.as_ref() else {
            return Err(store_error(
                LocalInstallGenerationStoreErrorKind::CorruptState,
                "cleanup debt has no accepted operation identity",
            ));
        };
        let next = CurrentEncodeWire {
            store_schema_version: LOCAL_INSTALL_GENERATION_STORE_SCHEMA_VERSION,
            store_generation: StoreGenerationWire::GlaedaCurrentV1,
            accepted: current
                .accepted_identity
                .as_ref()
                .map(IdentityEncodeWire::from),
            retained: current
                .retained_identity
                .as_ref()
                .map(IdentityEncodeWire::from),
            retiring: None,
            last_operation: Some(OperationEncodeWire {
                id: operation.as_str(),
                request_digest: request_digest.as_str(),
            }),
        };
        let bytes = encode_current(&next)?;
        self.stage_and_switch_current(&bytes, None)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StoreLockMode {
    Shared,
    Exclusive,
}

struct StoreLock {
    lock: OwnedFd,
}

impl Drop for StoreLock {
    fn drop(&mut self) {
        let _ = fs::flock(&self.lock, FlockOperation::Unlock);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum StoreGenerationWire {
    GlaedaCurrentV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoreIdentityWire {
    store_schema_version: u8,
    store_generation: StoreGenerationWire,
    location: LocalInstallStoreLocationClass,
    root_device: u64,
    root_inode: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum IdentityGenerationWire {
    SmolrunnerV1,
    GlaedaV2,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct IdentityWire {
    number: u64,
    digest: String,
}

#[derive(Serialize)]
struct IdentityEncodeWire<'a> {
    number: u64,
    digest: &'a str,
}

impl<'a> From<&'a LocalInstallGenerationIdentity> for IdentityEncodeWire<'a> {
    fn from(value: &'a LocalInstallGenerationIdentity) -> Self {
        Self {
            number: value.number,
            digest: value.digest.as_str(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceWire {
    identity_generation: IdentityGenerationWire,
    commit: String,
    tree: String,
    cargo_lock_digest: String,
    toolchain: String,
    digest: String,
}

#[derive(Serialize)]
struct SourceEncodeWire<'a> {
    identity_generation: IdentityGenerationWire,
    commit: &'a str,
    tree: &'a str,
    cargo_lock_digest: &'a str,
    toolchain: &'a str,
    digest: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GenerationWire {
    identity: IdentityWire,
    #[serde(skip_serializing_if = "Option::is_none")]
    predecessor: Option<IdentityWire>,
    source: SourceWire,
    binary_digest: String,
    binary_version: String,
}

#[derive(Serialize)]
struct GenerationEncodeWire<'a> {
    identity: IdentityEncodeWire<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    predecessor: Option<IdentityEncodeWire<'a>>,
    source: SourceEncodeWire<'a>,
    binary_digest: &'a str,
    binary_version: &'a str,
}

impl<'a> From<&'a InstalledLocalBinaryGeneration> for GenerationEncodeWire<'a> {
    fn from(value: &'a InstalledLocalBinaryGeneration) -> Self {
        Self {
            identity: IdentityEncodeWire::from(&value.identity),
            predecessor: value.predecessor.as_ref().map(IdentityEncodeWire::from),
            source: SourceEncodeWire {
                identity_generation: identity_generation_wire(value.source.identity_generation()),
                commit: value.source.commit().as_str(),
                tree: value.source.tree().as_str(),
                cargo_lock_digest: value.source.cargo_lock_digest().as_str(),
                toolchain: value.source.toolchain().as_str(),
                digest: value.source.digest().as_str(),
            },
            binary_digest: value.binary_digest.as_str(),
            binary_version: &value.binary_version,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactWire {
    file_type: String,
    owner: String,
    mode: String,
    links: u8,
    bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GenerationDocumentWire {
    store_schema_version: u8,
    store_generation: StoreGenerationWire,
    operation_id: String,
    request_digest: String,
    generation: GenerationWire,
    artifact: ArtifactWire,
}

#[derive(Serialize)]
struct GenerationDocumentEncodeWire<'a> {
    store_schema_version: u8,
    store_generation: StoreGenerationWire,
    operation_id: &'a str,
    request_digest: &'a str,
    generation: GenerationEncodeWire<'a>,
    artifact: &'a ArtifactWire,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OperationWire {
    id: String,
    request_digest: String,
}

#[derive(Serialize)]
struct OperationEncodeWire<'a> {
    id: &'a str,
    request_digest: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CurrentWire {
    store_schema_version: u8,
    store_generation: StoreGenerationWire,
    #[serde(skip_serializing_if = "Option::is_none")]
    accepted: Option<IdentityWire>,
    #[serde(skip_serializing_if = "Option::is_none")]
    retained: Option<IdentityWire>,
    #[serde(skip_serializing_if = "Option::is_none")]
    retiring: Option<IdentityWire>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_operation: Option<OperationWire>,
}

#[derive(Serialize)]
struct CurrentEncodeWire<'a> {
    store_schema_version: u8,
    store_generation: StoreGenerationWire,
    #[serde(skip_serializing_if = "Option::is_none")]
    accepted: Option<IdentityEncodeWire<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    retained: Option<IdentityEncodeWire<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    retiring: Option<IdentityEncodeWire<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_operation: Option<OperationEncodeWire<'a>>,
}

#[derive(Serialize)]
struct RequestIdentityWire<'a> {
    operation_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    expected_predecessor: Option<IdentityEncodeWire<'a>>,
    candidate: GenerationEncodeWire<'a>,
}

#[derive(Debug)]
struct LoadedGeneration {
    generation: InstalledLocalBinaryGeneration,
    operation_id: LocalInstallStoreOperationId,
    request_digest: Sha256Digest,
    document_bytes: Vec<u8>,
    artifact_bytes: u64,
}

#[derive(Debug)]
struct LoadedCurrent {
    snapshot: LocalInstallGenerationStoreSnapshot,
    operation: Option<(LocalInstallStoreOperationId, Sha256Digest)>,
    retained_operation: Option<(LocalInstallStoreOperationId, Sha256Digest)>,
    accepted_identity: Option<LocalInstallGenerationIdentity>,
    retained_identity: Option<LocalInstallGenerationIdentity>,
    retiring_identity: Option<LocalInstallGenerationIdentity>,
    bytes: Vec<u8>,
}

struct StagedEntries {
    operations: Vec<String>,
    retirements: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FaultBoundary {
    StageDirectoryCreated,
    BinaryWritten,
    BinarySynchronized,
    DocumentWritten,
    DocumentSynchronized,
    StageDirectorySynchronized,
    GenerationPublished,
    GenerationsDirectorySynchronized,
    CurrentSwitched,
    CurrentDocumentSynchronized,
    RootSynchronized,
    CleanupStarted,
    RetirementPublished,
    RetirementBinaryRemoved,
    RetirementDocumentRemoved,
    CleanupFinished,
}

fn store_error(
    kind: LocalInstallGenerationStoreErrorKind,
    message: impl Into<String>,
) -> LocalInstallGenerationStoreError {
    LocalInstallGenerationStoreError {
        kind,
        message: message.into(),
    }
}

fn is_sha256(value: &str) -> bool {
    value.strip_prefix(SHA256_PREFIX).is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}

fn identity_generation_wire(value: LocalInstallIdentityGeneration) -> IdentityGenerationWire {
    match value {
        LocalInstallIdentityGeneration::SmolrunnerV1 => IdentityGenerationWire::SmolrunnerV1,
        LocalInstallIdentityGeneration::GlaedaV2 => IdentityGenerationWire::GlaedaV2,
    }
}

fn digest_serialized(
    value: &impl Serialize,
) -> Result<Sha256Digest, LocalInstallGenerationStoreError> {
    let bytes = serde_json::to_vec(value).map_err(|_| {
        store_error(
            LocalInstallGenerationStoreErrorKind::InvalidRequest,
            "the local-install request identity could not be encoded",
        )
    })?;
    let digest = Sha256::digest(bytes);
    Sha256Digest::parse(&format!("sha256:{digest:x}")).map_err(|_| {
        store_error(
            LocalInstallGenerationStoreErrorKind::InvalidRequest,
            "the local-install request identity could not be derived",
        )
    })
}

fn validate_candidate(
    candidate: &InstalledLocalBinaryGeneration,
    expected_predecessor: Option<&LocalInstallGenerationIdentity>,
) -> Result<(), LocalInstallGenerationStoreError> {
    if candidate.source.identity_generation() != LocalInstallIdentityGeneration::GlaedaV2
        || candidate.predecessor.as_ref() != expected_predecessor
    {
        return Err(store_error(
            LocalInstallGenerationStoreErrorKind::InvalidRequest,
            "the candidate is not bound to the exact current-Glaeda predecessor",
        ));
    }
    let expected_number = match expected_predecessor {
        Some(predecessor) => predecessor.number.checked_add(1),
        None => Some(1),
    };
    if expected_number != Some(candidate.identity.number) {
        return Err(store_error(
            LocalInstallGenerationStoreErrorKind::InvalidRequest,
            "the candidate is not the exact successor generation",
        ));
    }
    let plan = LocalInstallBuildPlan {
        target_generation: candidate.identity.number,
        expected_predecessor: candidate.predecessor.clone(),
        source: candidate.source.clone(),
    };
    let evidence = BuiltLocalBinaryEvidence::new(
        candidate.source.digest().clone(),
        candidate.predecessor.clone(),
        candidate.binary_digest.clone(),
        candidate.binary_version.clone(),
    )
    .map_err(|_| {
        store_error(
            LocalInstallGenerationStoreErrorKind::InvalidRequest,
            "the candidate artifact evidence is invalid",
        )
    })?;
    let derived = complete_local_install_build(&plan, evidence).map_err(|_| {
        store_error(
            LocalInstallGenerationStoreErrorKind::InvalidRequest,
            "the candidate generation identity could not be reproduced",
        )
    })?;
    if derived != *candidate {
        return Err(store_error(
            LocalInstallGenerationStoreErrorKind::InvalidRequest,
            "the candidate generation identity does not match its canonical evidence",
        ));
    }
    Ok(())
}

fn open_absolute_directory(path: &OsStr) -> Result<OwnedFd, LocalInstallGenerationStoreError> {
    let path = Path::new(path);
    if !path.is_absolute()
        || path.as_os_str().as_bytes().is_empty()
        || path
            .components()
            .any(|component| !matches!(component, Component::RootDir | Component::Normal(_)))
    {
        return Err(store_error(
            LocalInstallGenerationStoreErrorKind::InvalidLocation,
            "the selected local-install data directory is invalid",
        ));
    }
    let mut current = fs::open("/", DIRECTORY_FLAGS, Mode::empty()).map_err(|_| {
        store_error(
            LocalInstallGenerationStoreErrorKind::Io,
            "the filesystem root could not be opened",
        )
    })?;
    for component in path.components() {
        let Component::Normal(component) = component else {
            continue;
        };
        current =
            fs::openat(&current, component, DIRECTORY_FLAGS, Mode::empty()).map_err(|error| {
                match error {
                    Errno::LOOP | Errno::NOTDIR => store_error(
                        LocalInstallGenerationStoreErrorKind::UnsafeFilesystem,
                        "the selected local-install data path is symlinked or invalid",
                    ),
                    _ => store_error(
                        LocalInstallGenerationStoreErrorKind::InvalidLocation,
                        "the selected local-install data directory is unavailable",
                    ),
                }
            })?;
        let stat = fs::fstat(&current).map_err(|_| {
            store_error(
                LocalInstallGenerationStoreErrorKind::Io,
                "the selected local-install data path could not be inspected",
            )
        })?;
        if !FileType::from_raw_mode(stat.st_mode).is_dir() {
            return Err(store_error(
                LocalInstallGenerationStoreErrorKind::UnsafeFilesystem,
                "the selected local-install data path is not a directory",
            ));
        }
    }
    inspect_directory(
        &current,
        Some((geteuid().as_raw(), getegid().as_raw())),
        false,
        "local-install data path",
    )?;
    Ok(current)
}

fn open_absolute_private_file(
    path: &Path,
    owner: (u32, u32),
    mode: Mode,
    subject: &str,
) -> Result<OwnedFd, LocalInstallGenerationStoreError> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::RootDir | Component::Normal(_)))
    {
        return Err(store_error(
            LocalInstallGenerationStoreErrorKind::InvalidLocation,
            format!("the {subject} path is invalid"),
        ));
    }
    let parent = path.parent().ok_or_else(|| {
        store_error(
            LocalInstallGenerationStoreErrorKind::InvalidLocation,
            format!("the {subject} parent is unavailable"),
        )
    })?;
    let name = path.file_name().ok_or_else(|| {
        store_error(
            LocalInstallGenerationStoreErrorKind::InvalidLocation,
            format!("the {subject} name is unavailable"),
        )
    })?;
    if name != OsStr::new(BINARY_FILE) {
        return Err(store_error(
            LocalInstallGenerationStoreErrorKind::InvalidLocation,
            format!("the {subject} name is invalid"),
        ));
    }
    let directory = open_absolute_directory(parent.as_os_str())?;
    open_existing_file(&directory, BINARY_FILE, owner, mode, subject)
}

fn open_or_create_store_root(
    parent: &OwnedFd,
    name: &str,
    owner: (u32, u32),
    location: LocalInstallStoreLocationClass,
) -> Result<(OwnedFd, OwnedFd), LocalInstallGenerationStoreError> {
    match fs::openat(parent, name, DIRECTORY_FLAGS, Mode::empty()) {
        Ok(root) => open_existing_store_root(root, owner, location),
        Err(Errno::NOENT) => publish_new_store_root(parent, name, owner, location),
        Err(Errno::LOOP | Errno::NOTDIR) => Err(store_error(
            LocalInstallGenerationStoreErrorKind::UnsafeFilesystem,
            "the local-install store root is symlinked or invalid",
        )),
        Err(_) => Err(store_error(
            LocalInstallGenerationStoreErrorKind::Io,
            "the local-install store root could not be opened",
        )),
    }
}

fn publish_new_store_root(
    parent: &OwnedFd,
    name: &str,
    owner: (u32, u32),
    location: LocalInstallStoreLocationClass,
) -> Result<(OwnedFd, OwnedFd), LocalInstallGenerationStoreError> {
    let (stage_name, root) = create_initialization_stage(parent, owner)?;
    let root_stat = inspect_directory(&root, Some(owner), true, "local-install store root")?;
    let mut staged = StagedStoreRoot {
        parent: parent.as_fd(),
        name: stage_name,
        root_device: store_root_device(&root_stat)?,
        root_inode: root_stat.st_ino,
        identity: None,
        armed: true,
    };
    let identity = create_store_identity(&root, owner, location)?;
    staged.identity = Some(duplicate_fd(&identity, "local-install store identity")?);
    inspect_initial_root_entries(&root)?;
    synchronize_directory(parent, "local-install store parent")?;

    match fs::renameat_with(
        parent,
        staged.name.as_str(),
        parent,
        name,
        RenameFlags::NOREPLACE,
    ) {
        Ok(()) => {
            staged.armed = false;
            synchronize_directory(parent, "local-install store parent")?;
            verify_retained_directory(parent, name, &root, owner, "local-install store root")?;
            Ok((root, identity))
        }
        Err(Errno::EXIST) => {
            drop(identity);
            drop(root);
            drop(staged);
            let root = fs::openat(parent, name, DIRECTORY_FLAGS, Mode::empty()).map_err(|_| {
                store_error(
                    LocalInstallGenerationStoreErrorKind::UnsafeFilesystem,
                    "the concurrently published local-install store root is unsafe",
                )
            })?;
            open_existing_store_root(root, owner, location)
        }
        Err(_) => Err(store_error(
            LocalInstallGenerationStoreErrorKind::Io,
            "the local-install store root could not be published atomically",
        )),
    }
}

fn create_initialization_stage(
    parent: &OwnedFd,
    owner: (u32, u32),
) -> Result<(String, OwnedFd), LocalInstallGenerationStoreError> {
    for _ in 0..INITIALIZATION_CREATE_ATTEMPTS {
        let name = create_initialization_stage_name()?;
        match fs::mkdirat(parent, name.as_str(), PRIVATE_DIRECTORY_MODE) {
            Ok(()) => {
                let root = fs::openat(parent, name.as_str(), DIRECTORY_FLAGS, Mode::empty())
                    .map_err(|_| {
                        store_error(
                            LocalInstallGenerationStoreErrorKind::Io,
                            "the private local-install initialization root could not be opened",
                        )
                    })?;
                fs::fchmod(&root, PRIVATE_DIRECTORY_MODE).map_err(|_| {
                    store_error(
                        LocalInstallGenerationStoreErrorKind::Io,
                        "private local-install initialization permissions could not be set",
                    )
                })?;
                inspect_directory(&root, Some(owner), true, "local-install store root")?;
                return Ok((name, root));
            }
            Err(Errno::EXIST) => continue,
            Err(_) => {
                return Err(store_error(
                    LocalInstallGenerationStoreErrorKind::Io,
                    "a private local-install initialization root could not be created",
                ));
            }
        }
    }
    Err(store_error(
        LocalInstallGenerationStoreErrorKind::Conflict,
        "a unique private local-install initialization root could not be created",
    ))
}

fn create_initialization_stage_name() -> Result<String, LocalInstallGenerationStoreError> {
    let mut random = [0_u8; INITIALIZATION_RANDOM_BYTES];
    File::open(SYSTEM_RANDOM_SOURCE)
        .and_then(|mut source| source.read_exact(&mut random))
        .map_err(|_| {
            store_error(
                LocalInstallGenerationStoreErrorKind::Io,
                "operating-system randomness for local-install initialization was unavailable",
            )
        })?;
    let mut name = String::with_capacity(INITIALIZATION_STAGE_PREFIX.len() + random.len() * 2);
    name.push_str(INITIALIZATION_STAGE_PREFIX);
    for byte in random {
        use std::fmt::Write as _;
        write!(&mut name, "{byte:02x}").map_err(|_| {
            store_error(
                LocalInstallGenerationStoreErrorKind::Io,
                "local-install initialization identity could not be represented",
            )
        })?;
    }
    Ok(name)
}

struct StagedStoreRoot<'a> {
    parent: BorrowedFd<'a>,
    name: String,
    root_device: u64,
    root_inode: u64,
    identity: Option<OwnedFd>,
    armed: bool,
}

impl Drop for StagedStoreRoot<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let Ok(root) = fs::openat(
            self.parent,
            self.name.as_str(),
            DIRECTORY_FLAGS,
            Mode::empty(),
        ) else {
            return;
        };
        let Ok(root_stat) = fs::fstat(&root) else {
            return;
        };
        if store_root_device(&root_stat).ok() != Some(self.root_device)
            || root_stat.st_ino != self.root_inode
        {
            return;
        }
        if let Some(identity) = self.identity.as_ref()
            && let Ok(current) = fs::openat(
                &root,
                STORE_IDENTITY_FILE,
                EXISTING_FILE_FLAGS,
                Mode::empty(),
            )
            && fs::fstat(identity)
                .ok()
                .zip(fs::fstat(&current).ok())
                .is_some_and(|(left, right)| same_object(&left, &right))
        {
            let _ = fs::unlinkat(&root, STORE_IDENTITY_FILE, AtFlags::empty());
        }
        let _ = fs::unlinkat(self.parent, self.name.as_str(), AtFlags::REMOVEDIR);
        let _ = fs::fsync(self.parent);
    }
}

fn open_existing_store_root(
    root: OwnedFd,
    owner: (u32, u32),
    location: LocalInstallStoreLocationClass,
) -> Result<(OwnedFd, OwnedFd), LocalInstallGenerationStoreError> {
    inspect_directory(&root, Some(owner), true, "local-install store root")?;
    let identity = open_existing_file(
        &root,
        STORE_IDENTITY_FILE,
        owner,
        PRIVATE_FILE_MODE,
        "local-install store identity",
    )
    .map_err(|error| {
        if error.kind == LocalInstallGenerationStoreErrorKind::RecoveryRequired {
            store_error(
                LocalInstallGenerationStoreErrorKind::UnsafeFilesystem,
                "a pre-existing local-install store root has no durable identity",
            )
        } else {
            error
        }
    })?;
    verify_store_identity_document(&root, &identity, owner, location)?;
    inspect_initial_root_entries(&root)?;
    Ok((root, identity))
}

fn create_store_identity(
    root: &OwnedFd,
    owner: (u32, u32),
    location: LocalInstallStoreLocationClass,
) -> Result<OwnedFd, LocalInstallGenerationStoreError> {
    let root_stat = inspect_directory(root, Some(owner), true, "local-install store root")?;
    let bytes = encode_store_identity(location, &root_stat)?;
    let identity = create_private_file(
        root,
        STORE_IDENTITY_FILE,
        PRIVATE_FILE_MODE,
        owner,
        "local-install store identity",
    )?;
    write_all_fd(&identity, &bytes, "local-install store identity")?;
    fs::fsync(&identity).map_err(|_| {
        store_error(
            LocalInstallGenerationStoreErrorKind::Io,
            "the local-install store identity could not be synchronized",
        )
    })?;
    synchronize_directory(root, "local-install store root")?;
    drop(identity);
    let identity = open_existing_file(
        root,
        STORE_IDENTITY_FILE,
        owner,
        PRIVATE_FILE_MODE,
        "local-install store identity",
    )?;
    verify_store_identity_document(root, &identity, owner, location)?;
    Ok(identity)
}

fn inspect_initial_root_entries(root: &OwnedFd) -> Result<(), LocalInstallGenerationStoreError> {
    let mut entries = Dir::read_from(root).map_err(|_| {
        store_error(
            LocalInstallGenerationStoreErrorKind::Io,
            "the local-install store root could not be enumerated before initialization",
        )
    })?;
    let mut identity = false;
    let mut count = 0_usize;
    for entry in &mut entries {
        let entry = entry.map_err(|_| {
            store_error(
                LocalInstallGenerationStoreErrorKind::Io,
                "a local-install store root entry could not be read before initialization",
            )
        })?;
        let name = entry.file_name().to_bytes();
        if matches!(name, b"." | b"..") {
            continue;
        }
        count += 1;
        if count > 7 {
            return Err(store_error(
                LocalInstallGenerationStoreErrorKind::CorruptState,
                "the local-install store root contains too many entries",
            ));
        }
        match name {
            b"store.identity.json" => identity = true,
            b"lock" | b"current" | b"current.next" | b"generations" | b"staged" => {}
            _ => {
                return Err(store_error(
                    LocalInstallGenerationStoreErrorKind::CorruptState,
                    "the local-install store root contains an unexpected entry",
                ));
            }
        }
    }
    if !identity {
        return Err(store_error(
            LocalInstallGenerationStoreErrorKind::UnsafeFilesystem,
            "the local-install store root has no durable identity",
        ));
    }
    Ok(())
}

fn ensure_directory(
    parent: &OwnedFd,
    name: &str,
    owner: (u32, u32),
    exact_private: bool,
) -> Result<OwnedFd, LocalInstallGenerationStoreError> {
    let mut created = false;
    let directory = match fs::openat(parent, name, DIRECTORY_FLAGS, Mode::empty()) {
        Ok(directory) => directory,
        Err(Errno::NOENT) => {
            match fs::mkdirat(parent, name, PRIVATE_DIRECTORY_MODE) {
                Ok(()) => created = true,
                Err(Errno::EXIST) => {}
                Err(_) => {
                    return Err(store_error(
                        LocalInstallGenerationStoreErrorKind::Io,
                        "a local-install store directory could not be created",
                    ));
                }
            }
            fs::openat(parent, name, DIRECTORY_FLAGS, Mode::empty()).map_err(
                |error| match error {
                    Errno::LOOP | Errno::NOTDIR => store_error(
                        LocalInstallGenerationStoreErrorKind::UnsafeFilesystem,
                        "a local-install store directory is symlinked or invalid",
                    ),
                    _ => store_error(
                        LocalInstallGenerationStoreErrorKind::Io,
                        "a local-install store directory could not be opened",
                    ),
                },
            )?
        }
        Err(Errno::LOOP | Errno::NOTDIR) => {
            return Err(store_error(
                LocalInstallGenerationStoreErrorKind::UnsafeFilesystem,
                "a local-install store directory is symlinked or invalid",
            ));
        }
        Err(_) => {
            return Err(store_error(
                LocalInstallGenerationStoreErrorKind::Io,
                "a local-install store directory could not be opened",
            ));
        }
    };
    if created {
        fs::fchmod(&directory, PRIVATE_DIRECTORY_MODE).map_err(|_| {
            store_error(
                LocalInstallGenerationStoreErrorKind::Io,
                "private local-install directory permissions could not be set",
            )
        })?;
    }
    inspect_directory(
        &directory,
        Some(owner),
        exact_private,
        "local-install store directory",
    )?;
    if created {
        synchronize_directory(parent, "local-install parent directory")?;
    }
    Ok(directory)
}

fn inspect_directory(
    directory: impl AsFd,
    expected_owner: Option<(u32, u32)>,
    exact_private: bool,
    subject: &str,
) -> Result<rustix::fs::Stat, LocalInstallGenerationStoreError> {
    let stat = fs::fstat(directory.as_fd()).map_err(|_| {
        store_error(
            LocalInstallGenerationStoreErrorKind::Io,
            format!("could not inspect {subject}"),
        )
    })?;
    if !FileType::from_raw_mode(stat.st_mode).is_dir()
        || (exact_private && stat.st_mode & 0o7777 != PRIVATE_DIRECTORY_MODE.as_raw_mode())
        || (!exact_private && stat.st_mode & 0o022 != 0)
        || expected_owner.is_some_and(|owner| owner != (stat.st_uid, stat.st_gid))
    {
        return Err(store_error(
            LocalInstallGenerationStoreErrorKind::UnsafeFilesystem,
            format!("{subject} has unsafe type, permissions, or ownership"),
        ));
    }
    Ok(stat)
}

fn ensure_lock_file(
    root: &OwnedFd,
    owner: (u32, u32),
) -> Result<OwnedFd, LocalInstallGenerationStoreError> {
    let lock = match fs::openat(root, LOCK_FILE, NEW_LOCK_FLAGS, PRIVATE_FILE_MODE) {
        Ok(lock) => {
            fs::fchmod(&lock, PRIVATE_FILE_MODE).map_err(|_| {
                store_error(
                    LocalInstallGenerationStoreErrorKind::Io,
                    "local-install store lock permissions could not be set",
                )
            })?;
            fs::fsync(&lock).map_err(|_| {
                store_error(
                    LocalInstallGenerationStoreErrorKind::Io,
                    "the local-install store lock could not be synchronized",
                )
            })?;
            synchronize_directory(root, "local-install store root")?;
            lock
        }
        Err(Errno::EXIST) => fs::openat(root, LOCK_FILE, EXISTING_LOCK_FLAGS, Mode::empty())
            .map_err(|_| {
                store_error(
                    LocalInstallGenerationStoreErrorKind::UnsafeFilesystem,
                    "the local-install store lock could not be opened safely",
                )
            })?,
        Err(_) => {
            return Err(store_error(
                LocalInstallGenerationStoreErrorKind::Io,
                "the local-install store lock could not be created",
            ));
        }
    };
    inspect_private_file(
        &lock,
        owner,
        PRIVATE_FILE_MODE,
        Some(0),
        "local-install store lock",
    )?;
    Ok(lock)
}

fn inspect_private_file(
    file: impl AsFd,
    owner: (u32, u32),
    expected_mode: Mode,
    expected_size: Option<u64>,
    subject: &str,
) -> Result<rustix::fs::Stat, LocalInstallGenerationStoreError> {
    let stat = fs::fstat(file.as_fd()).map_err(|_| {
        store_error(
            LocalInstallGenerationStoreErrorKind::Io,
            format!("could not inspect {subject}"),
        )
    })?;
    if !FileType::from_raw_mode(stat.st_mode).is_file()
        || stat.st_nlink != 1
        || stat.st_mode & 0o7777 != expected_mode.as_raw_mode()
        || owner != (stat.st_uid, stat.st_gid)
        || expected_size.is_some_and(|size| stat.st_size < 0 || stat.st_size as u64 != size)
    {
        return Err(store_error(
            LocalInstallGenerationStoreErrorKind::UnsafeFilesystem,
            format!("{subject} is not an exact private single-link regular file"),
        ));
    }
    Ok(stat)
}

fn duplicate_fd(fd: impl AsFd, subject: &str) -> Result<OwnedFd, LocalInstallGenerationStoreError> {
    rustix::io::dup(fd).map_err(|_| {
        store_error(
            LocalInstallGenerationStoreErrorKind::Io,
            format!("could not retain {subject}"),
        )
    })
}

fn synchronize_directory(
    directory: impl AsFd,
    subject: &str,
) -> Result<(), LocalInstallGenerationStoreError> {
    fs::fsync(directory.as_fd()).map_err(|_| {
        store_error(
            LocalInstallGenerationStoreErrorKind::Io,
            format!("could not synchronize {subject}"),
        )
    })
}

struct VerifiedSourceBinary {
    file: OwnedFd,
    bytes: u64,
}

fn open_verified_source_binary(
    path: &Path,
    owner: (u32, u32),
    expected_digest: &Sha256Digest,
) -> Result<VerifiedSourceBinary, LocalInstallGenerationStoreError> {
    if !path.is_absolute() {
        return Err(store_error(
            LocalInstallGenerationStoreErrorKind::InvalidRequest,
            "the candidate binary path must be absolute",
        ));
    }
    let file = fs::open(path, EXISTING_FILE_FLAGS, Mode::empty()).map_err(|error| match error {
        Errno::LOOP | Errno::NOTDIR | Errno::ISDIR => store_error(
            LocalInstallGenerationStoreErrorKind::UnsafeFilesystem,
            "the candidate binary path is symlinked or invalid",
        ),
        _ => store_error(
            LocalInstallGenerationStoreErrorKind::Io,
            "the candidate binary could not be opened",
        ),
    })?;
    let stat = fs::fstat(&file).map_err(|_| {
        store_error(
            LocalInstallGenerationStoreErrorKind::Io,
            "the candidate binary could not be inspected",
        )
    })?;
    if !FileType::from_raw_mode(stat.st_mode).is_file()
        || stat.st_nlink != 1
        || owner != (stat.st_uid, stat.st_gid)
        || stat.st_mode & 0o022 != 0
        || stat.st_mode & 0o100 == 0
        || stat.st_size <= 0
        || stat.st_size as u64 > MAX_LOCAL_INSTALL_BINARY_BYTES
    {
        return Err(store_error(
            LocalInstallGenerationStoreErrorKind::UnsafeFilesystem,
            "the candidate binary is not a bounded private executable regular file",
        ));
    }
    let digest = hash_file_stable(&file, &stat, stat.st_size as u64, "candidate binary")?;
    if &digest != expected_digest {
        return Err(store_error(
            LocalInstallGenerationStoreErrorKind::Conflict,
            "the candidate binary digest does not match the verified generation",
        ));
    }
    Ok(VerifiedSourceBinary {
        file,
        bytes: stat.st_size as u64,
    })
}

fn copy_source_binary(
    source: &OwnedFd,
    destination: &OwnedFd,
    expected_bytes: u64,
) -> Result<(), LocalInstallGenerationStoreError> {
    let source = duplicate_fd(source, "candidate binary for copying")?;
    let destination = duplicate_fd(destination, "staged binary for writing")?;
    let mut reader = File::from(source);
    reader.rewind().map_err(|_| {
        store_error(
            LocalInstallGenerationStoreErrorKind::Io,
            "the candidate binary could not be rewound for copying",
        )
    })?;
    let mut reader = reader.take(expected_bytes + 1);
    let mut writer = File::from(destination);
    let copied = std::io::copy(&mut reader, &mut writer).map_err(|_| {
        store_error(
            LocalInstallGenerationStoreErrorKind::Io,
            "the candidate binary could not be copied into staging",
        )
    })?;
    if copied != expected_bytes {
        return Err(store_error(
            LocalInstallGenerationStoreErrorKind::RecoveryRequired,
            "the candidate binary changed while it was copied",
        ));
    }
    Ok(())
}

fn create_private_directory(
    parent: &OwnedFd,
    name: &str,
    owner: (u32, u32),
    subject: &str,
) -> Result<OwnedFd, LocalInstallGenerationStoreError> {
    fs::mkdirat(parent, name, PRIVATE_DIRECTORY_MODE).map_err(|error| match error {
        Errno::EXIST => store_error(
            LocalInstallGenerationStoreErrorKind::Conflict,
            format!("{subject} already exists"),
        ),
        _ => store_error(
            LocalInstallGenerationStoreErrorKind::Io,
            format!("{subject} could not be created"),
        ),
    })?;
    let directory = fs::openat(parent, name, DIRECTORY_FLAGS, Mode::empty()).map_err(|_| {
        store_error(
            LocalInstallGenerationStoreErrorKind::Io,
            format!("{subject} could not be opened"),
        )
    })?;
    fs::fchmod(&directory, PRIVATE_DIRECTORY_MODE).map_err(|_| {
        store_error(
            LocalInstallGenerationStoreErrorKind::Io,
            format!("{subject} permissions could not be set"),
        )
    })?;
    inspect_directory(&directory, Some(owner), true, subject)?;
    synchronize_directory(parent, "local-install parent directory")?;
    Ok(directory)
}

fn create_private_file(
    parent: &OwnedFd,
    name: &str,
    mode: Mode,
    owner: (u32, u32),
    subject: &str,
) -> Result<OwnedFd, LocalInstallGenerationStoreError> {
    let file = fs::openat(parent, name, NEW_FILE_FLAGS, mode).map_err(|error| match error {
        Errno::EXIST => store_error(
            LocalInstallGenerationStoreErrorKind::Conflict,
            format!("{subject} already exists"),
        ),
        _ => store_error(
            LocalInstallGenerationStoreErrorKind::Io,
            format!("{subject} could not be created"),
        ),
    })?;
    fs::fchmod(&file, mode).map_err(|_| {
        store_error(
            LocalInstallGenerationStoreErrorKind::Io,
            format!("{subject} permissions could not be set"),
        )
    })?;
    inspect_private_file(&file, owner, mode, Some(0), subject)?;
    Ok(file)
}

fn open_existing_file(
    parent: &OwnedFd,
    name: &str,
    owner: (u32, u32),
    mode: Mode,
    subject: &str,
) -> Result<OwnedFd, LocalInstallGenerationStoreError> {
    let file =
        fs::openat(parent, name, EXISTING_FILE_FLAGS, Mode::empty()).map_err(
            |error| match error {
                Errno::NOENT => store_error(
                    LocalInstallGenerationStoreErrorKind::RecoveryRequired,
                    format!("{subject} is absent"),
                ),
                Errno::LOOP | Errno::NOTDIR | Errno::ISDIR => store_error(
                    LocalInstallGenerationStoreErrorKind::UnsafeFilesystem,
                    format!("{subject} is symlinked or invalid"),
                ),
                _ => store_error(
                    LocalInstallGenerationStoreErrorKind::Io,
                    format!("{subject} could not be opened"),
                ),
            },
        )?;
    inspect_private_file(&file, owner, mode, None, subject)?;
    Ok(file)
}

fn write_all_fd(
    file: &OwnedFd,
    bytes: &[u8],
    subject: &str,
) -> Result<(), LocalInstallGenerationStoreError> {
    let duplicate = duplicate_fd(file, subject)?;
    File::from(duplicate).write_all(bytes).map_err(|_| {
        store_error(
            LocalInstallGenerationStoreErrorKind::Io,
            format!("{subject} could not be written"),
        )
    })
}

fn read_bounded_file(
    file: &OwnedFd,
    owner: (u32, u32),
    mode: Mode,
    limit: usize,
    subject: &str,
) -> Result<Vec<u8>, LocalInstallGenerationStoreError> {
    let before = inspect_private_file(file, owner, mode, None, subject)?;
    if before.st_size < 0 || before.st_size as u64 > limit as u64 {
        return Err(store_error(
            LocalInstallGenerationStoreErrorKind::CorruptState,
            format!("{subject} exceeds its byte limit"),
        ));
    }
    let reader = File::from(duplicate_fd(file, subject)?);
    let mut bytes = vec![0_u8; limit + 1];
    let mut offset = 0_usize;
    while offset < bytes.len() {
        match reader.read_at(&mut bytes[offset..], offset as u64) {
            Ok(0) => break,
            Ok(read) => offset += read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => {
                return Err(store_error(
                    LocalInstallGenerationStoreErrorKind::Io,
                    format!("{subject} could not be read"),
                ));
            }
        }
    }
    bytes.truncate(offset);
    if bytes.len() > limit {
        return Err(store_error(
            LocalInstallGenerationStoreErrorKind::CorruptState,
            format!("{subject} exceeds its byte limit"),
        ));
    }
    let after = inspect_private_file(file, owner, mode, Some(bytes.len() as u64), subject)?;
    if !same_file_snapshot(&before, &after) {
        return Err(store_error(
            LocalInstallGenerationStoreErrorKind::RecoveryRequired,
            format!("{subject} changed while it was read"),
        ));
    }
    Ok(bytes)
}

fn hash_private_file(
    file: &OwnedFd,
    owner: (u32, u32),
    mode: Mode,
    expected_bytes: u64,
    subject: &str,
) -> Result<Sha256Digest, LocalInstallGenerationStoreError> {
    let before = inspect_private_file(file, owner, mode, Some(expected_bytes), subject)?;
    hash_file_stable(file, &before, expected_bytes, subject)
}

fn hash_file_stable(
    file: &OwnedFd,
    before: &rustix::fs::Stat,
    expected_bytes: u64,
    subject: &str,
) -> Result<Sha256Digest, LocalInstallGenerationStoreError> {
    let duplicate = duplicate_fd(file, subject)?;
    let mut reader = File::from(duplicate);
    reader.rewind().map_err(|_| {
        store_error(
            LocalInstallGenerationStoreErrorKind::Io,
            format!("{subject} could not be rewound for hashing"),
        )
    })?;
    let mut reader = reader.take(expected_bytes + 1);
    let mut hasher = Sha256::new();
    let bytes = std::io::copy(&mut reader, &mut hasher).map_err(|_| {
        store_error(
            LocalInstallGenerationStoreErrorKind::Io,
            format!("{subject} could not be hashed"),
        )
    })?;
    let after = fs::fstat(file).map_err(|_| {
        store_error(
            LocalInstallGenerationStoreErrorKind::Io,
            format!("{subject} could not be re-inspected"),
        )
    })?;
    if bytes != expected_bytes || !same_file_snapshot(before, &after) {
        return Err(store_error(
            LocalInstallGenerationStoreErrorKind::RecoveryRequired,
            format!("{subject} changed while it was hashed"),
        ));
    }
    Sha256Digest::parse(&format!("sha256:{:x}", hasher.finalize())).map_err(|_| {
        store_error(
            LocalInstallGenerationStoreErrorKind::CorruptState,
            format!("{subject} digest could not be represented"),
        )
    })
}

fn verify_retained_file(
    parent: &OwnedFd,
    name: &str,
    retained: &OwnedFd,
    owner: (u32, u32),
    mode: Mode,
    subject: &str,
) -> Result<(), LocalInstallGenerationStoreError> {
    let current = open_existing_file(parent, name, owner, mode, subject)?;
    let left = fs::fstat(retained).map_err(|_| {
        store_error(
            LocalInstallGenerationStoreErrorKind::Io,
            format!("could not inspect {subject}"),
        )
    })?;
    let right = fs::fstat(&current).map_err(|_| {
        store_error(
            LocalInstallGenerationStoreErrorKind::Io,
            format!("could not inspect {subject}"),
        )
    })?;
    if !same_object(&left, &right) {
        return Err(store_error(
            LocalInstallGenerationStoreErrorKind::UnsafeFilesystem,
            format!("{subject} identity changed"),
        ));
    }
    Ok(())
}

fn verify_retained_directory(
    parent: &OwnedFd,
    name: &str,
    retained: &OwnedFd,
    owner: (u32, u32),
    subject: &str,
) -> Result<(), LocalInstallGenerationStoreError> {
    let current = fs::openat(parent, name, DIRECTORY_FLAGS, Mode::empty()).map_err(|_| {
        store_error(
            LocalInstallGenerationStoreErrorKind::UnsafeFilesystem,
            format!("{subject} could not be reopened safely"),
        )
    })?;
    let left = inspect_directory(retained, Some(owner), true, subject)?;
    let right = inspect_directory(&current, Some(owner), true, subject)?;
    if !same_object(&left, &right) {
        return Err(store_error(
            LocalInstallGenerationStoreErrorKind::UnsafeFilesystem,
            format!("{subject} identity changed"),
        ));
    }
    Ok(())
}

fn same_object(left: &rustix::fs::Stat, right: &rustix::fs::Stat) -> bool {
    left.st_dev == right.st_dev && left.st_ino == right.st_ino
}

fn same_file_snapshot(left: &rustix::fs::Stat, right: &rustix::fs::Stat) -> bool {
    same_object(left, right)
        && left.st_uid == right.st_uid
        && left.st_gid == right.st_gid
        && left.st_mode == right.st_mode
        && left.st_nlink == right.st_nlink
        && left.st_size == right.st_size
        && left.st_mtime == right.st_mtime
        && left.st_mtime_nsec == right.st_mtime_nsec
        && left.st_ctime == right.st_ctime
        && left.st_ctime_nsec == right.st_ctime_nsec
}

fn entry_exists(parent: &OwnedFd, name: &str) -> Result<bool, LocalInstallGenerationStoreError> {
    match fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(_) => Ok(true),
        Err(Errno::NOENT) => Ok(false),
        Err(_) => Err(store_error(
            LocalInstallGenerationStoreErrorKind::Io,
            "a local-install store entry could not be inspected",
        )),
    }
}

fn enumerate_names(
    directory: &OwnedFd,
    limit: usize,
    valid: impl Fn(&str) -> bool,
    subject: &str,
) -> Result<Vec<String>, LocalInstallGenerationStoreError> {
    let mut entries = Dir::read_from(directory).map_err(|_| {
        store_error(
            LocalInstallGenerationStoreErrorKind::Io,
            format!("{subject} could not be enumerated"),
        )
    })?;
    let mut names = Vec::new();
    for entry in &mut entries {
        let entry = entry.map_err(|_| {
            store_error(
                LocalInstallGenerationStoreErrorKind::Io,
                format!("an entry in {subject} could not be read"),
            )
        })?;
        let bytes = entry.file_name().to_bytes();
        if matches!(bytes, b"." | b"..") {
            continue;
        }
        let name = std::str::from_utf8(bytes).map_err(|_| {
            store_error(
                LocalInstallGenerationStoreErrorKind::CorruptState,
                format!("{subject} contains a noncanonical entry"),
            )
        })?;
        if names.len() >= limit || !valid(name) {
            return Err(store_error(
                LocalInstallGenerationStoreErrorKind::CorruptState,
                format!("{subject} contains unexpected or excess entries"),
            ));
        }
        names.push(name.to_owned());
    }
    names.sort();
    Ok(names)
}

fn generation_directory_name(
    identity: &LocalInstallGenerationIdentity,
) -> Result<String, LocalInstallGenerationStoreError> {
    if !is_sha256(identity.digest.as_str()) || identity.number == 0 {
        return Err(store_error(
            LocalInstallGenerationStoreErrorKind::InvalidRequest,
            "the generation identity cannot form a canonical directory name",
        ));
    }
    Ok(format!(
        "{GENERATION_PREFIX}{}-{}",
        identity.number,
        &identity.digest.as_str()[SHA256_PREFIX.len()..]
    ))
}

fn is_generation_directory_name(name: &str) -> bool {
    identity_from_generation_directory_name(name).is_ok()
}

fn identity_from_generation_directory_name(
    name: &str,
) -> Result<LocalInstallGenerationIdentity, LocalInstallGenerationStoreError> {
    let Some(rest) = name.strip_prefix(GENERATION_PREFIX) else {
        return Err(store_error(
            LocalInstallGenerationStoreErrorKind::CorruptState,
            "the generation directory name is noncanonical",
        ));
    };
    let Some((number_text, hex)) = rest.split_once('-') else {
        return Err(store_error(
            LocalInstallGenerationStoreErrorKind::CorruptState,
            "the generation directory name is noncanonical",
        ));
    };
    let number = number_text
        .parse::<u64>()
        .ok()
        .filter(|value| *value != 0)
        .ok_or_else(|| {
            store_error(
                LocalInstallGenerationStoreErrorKind::CorruptState,
                "the generation directory number is noncanonical",
            )
        })?;
    if number.to_string() != number_text || hex.len() != 64 {
        return Err(store_error(
            LocalInstallGenerationStoreErrorKind::CorruptState,
            "the generation directory name is noncanonical",
        ));
    }
    let digest = Sha256Digest::parse(&format!("sha256:{hex}")).map_err(|_| {
        store_error(
            LocalInstallGenerationStoreErrorKind::CorruptState,
            "the generation directory digest is noncanonical",
        )
    })?;
    let identity = LocalInstallGenerationIdentity { number, digest };
    if generation_directory_name(&identity)? != name {
        return Err(store_error(
            LocalInstallGenerationStoreErrorKind::CorruptState,
            "the generation directory name is noncanonical",
        ));
    }
    Ok(identity)
}

fn retirement_directory_name(
    identity: &LocalInstallGenerationIdentity,
) -> Result<String, LocalInstallGenerationStoreError> {
    Ok(format!(
        "{RETIREMENT_PREFIX}{}",
        generation_directory_name(identity)?
    ))
}

fn is_retirement_directory_name(name: &str) -> bool {
    identity_from_retirement_directory_name(name).is_ok()
}

fn identity_from_retirement_directory_name(
    name: &str,
) -> Result<LocalInstallGenerationIdentity, LocalInstallGenerationStoreError> {
    let generation_name = name.strip_prefix(RETIREMENT_PREFIX).ok_or_else(|| {
        store_error(
            LocalInstallGenerationStoreErrorKind::CorruptState,
            "the retirement directory name is noncanonical",
        )
    })?;
    let identity = identity_from_generation_directory_name(generation_name)?;
    if retirement_directory_name(&identity)? != name {
        return Err(store_error(
            LocalInstallGenerationStoreErrorKind::CorruptState,
            "the retirement directory name is noncanonical",
        ));
    }
    Ok(identity)
}

fn is_operation_directory_name(name: &str) -> bool {
    operation_id_from_directory_name(name).is_ok()
}

fn operation_id_from_directory_name(
    name: &str,
) -> Result<LocalInstallStoreOperationId, LocalInstallGenerationStoreError> {
    let hex = name.strip_prefix(OPERATION_PREFIX).ok_or_else(|| {
        store_error(
            LocalInstallGenerationStoreErrorKind::CorruptState,
            "the staged operation directory name is noncanonical",
        )
    })?;
    let operation = LocalInstallStoreOperationId::parse(&format!("sha256:{hex}"))?;
    if operation.directory_name() != name {
        return Err(store_error(
            LocalInstallGenerationStoreErrorKind::CorruptState,
            "the staged operation directory name is noncanonical",
        ));
    }
    Ok(operation)
}

fn encode_store_identity(
    location: LocalInstallStoreLocationClass,
    root_stat: &rustix::fs::Stat,
) -> Result<Vec<u8>, LocalInstallGenerationStoreError> {
    let bytes = serde_json::to_vec(&StoreIdentityWire {
        store_schema_version: LOCAL_INSTALL_GENERATION_STORE_SCHEMA_VERSION,
        store_generation: StoreGenerationWire::GlaedaCurrentV1,
        location,
        root_device: store_root_device(root_stat)?,
        root_inode: root_stat.st_ino,
    })
    .map_err(|_| {
        store_error(
            LocalInstallGenerationStoreErrorKind::Io,
            "the local-install store identity could not be encoded",
        )
    })?;
    if bytes.len() > MAX_LOCAL_INSTALL_STORE_IDENTITY_BYTES {
        return Err(store_error(
            LocalInstallGenerationStoreErrorKind::Io,
            "the local-install store identity exceeds its byte limit",
        ));
    }
    Ok(bytes)
}

fn verify_store_identity_document(
    root: &OwnedFd,
    identity: &OwnedFd,
    owner: (u32, u32),
    location: LocalInstallStoreLocationClass,
) -> Result<(), LocalInstallGenerationStoreError> {
    let bytes = read_bounded_file(
        identity,
        owner,
        PRIVATE_FILE_MODE,
        MAX_LOCAL_INSTALL_STORE_IDENTITY_BYTES,
        "local-install store identity",
    )?;
    let wire: StoreIdentityWire = serde_json::from_slice(&bytes).map_err(|_| {
        store_error(
            LocalInstallGenerationStoreErrorKind::CorruptState,
            "the local-install store identity is invalid",
        )
    })?;
    let root_stat = inspect_directory(root, Some(owner), true, "local-install store root")?;
    if wire.store_schema_version != LOCAL_INSTALL_GENERATION_STORE_SCHEMA_VERSION
        || wire.store_generation != StoreGenerationWire::GlaedaCurrentV1
        || wire.location != location
        || wire.root_device != store_root_device(&root_stat)?
        || wire.root_inode != root_stat.st_ino
        || encode_store_identity(location, &root_stat)? != bytes
    {
        return Err(store_error(
            LocalInstallGenerationStoreErrorKind::CorruptState,
            "the local-install store identity does not match its root",
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn store_root_device(
    root_stat: &rustix::fs::Stat,
) -> Result<u64, LocalInstallGenerationStoreError> {
    Ok(root_stat.st_dev)
}

#[cfg(target_os = "macos")]
fn store_root_device(
    root_stat: &rustix::fs::Stat,
) -> Result<u64, LocalInstallGenerationStoreError> {
    u64::try_from(root_stat.st_dev).map_err(|_| {
        store_error(
            LocalInstallGenerationStoreErrorKind::CorruptState,
            "the local-install store root has an invalid device identity",
        )
    })
}

fn encode_generation_document(
    request: &LocalInstallGenerationPublishRequest,
    artifact: &ArtifactWire,
) -> Result<Vec<u8>, LocalInstallGenerationStoreError> {
    let wire = GenerationDocumentEncodeWire {
        store_schema_version: LOCAL_INSTALL_GENERATION_STORE_SCHEMA_VERSION,
        store_generation: StoreGenerationWire::GlaedaCurrentV1,
        operation_id: request.operation_id.as_str(),
        request_digest: request.request_digest.as_str(),
        generation: GenerationEncodeWire::from(&request.candidate),
        artifact,
    };
    let bytes = serde_json::to_vec(&wire).map_err(|_| {
        store_error(
            LocalInstallGenerationStoreErrorKind::InvalidRequest,
            "the generation document could not be encoded",
        )
    })?;
    if bytes.len() > MAX_LOCAL_INSTALL_GENERATION_DOCUMENT_BYTES {
        return Err(store_error(
            LocalInstallGenerationStoreErrorKind::InvalidRequest,
            "the generation document exceeds its byte limit",
        ));
    }
    Ok(bytes)
}

fn decode_generation_document(
    bytes: &[u8],
) -> Result<LoadedGeneration, LocalInstallGenerationStoreError> {
    let wire: GenerationDocumentWire = serde_json::from_slice(bytes).map_err(|_| {
        store_error(
            LocalInstallGenerationStoreErrorKind::CorruptState,
            "the generation document is invalid",
        )
    })?;
    if wire.store_schema_version != LOCAL_INSTALL_GENERATION_STORE_SCHEMA_VERSION
        || wire.store_generation != StoreGenerationWire::GlaedaCurrentV1
        || wire.artifact.file_type != "regular_file"
        || wire.artifact.owner != "store_owner"
        || wire.artifact.mode != "0500"
        || wire.artifact.links != 1
        || wire.artifact.bytes == 0
        || wire.artifact.bytes > MAX_LOCAL_INSTALL_BINARY_BYTES
    {
        return Err(store_error(
            LocalInstallGenerationStoreErrorKind::CorruptState,
            "the generation document has unsupported or invalid evidence",
        ));
    }
    let operation_id = LocalInstallStoreOperationId::parse(&wire.operation_id).map_err(|_| {
        store_error(
            LocalInstallGenerationStoreErrorKind::CorruptState,
            "the stored operation identity is invalid",
        )
    })?;
    let request_digest = parse_sha256(&wire.request_digest, "stored request digest")?;
    let generation = parse_generation(&wire.generation)?;
    validate_candidate(&generation, generation.predecessor.as_ref()).map_err(|_| {
        store_error(
            LocalInstallGenerationStoreErrorKind::CorruptState,
            "the stored generation does not reproduce from its canonical evidence",
        )
    })?;
    let request = LocalInstallGenerationPublishRequest::new(
        operation_id.clone(),
        generation.predecessor.clone(),
        generation.clone(),
    )
    .map_err(|_| {
        store_error(
            LocalInstallGenerationStoreErrorKind::CorruptState,
            "the stored generation request is invalid",
        )
    })?;
    if request.request_digest != request_digest {
        return Err(store_error(
            LocalInstallGenerationStoreErrorKind::CorruptState,
            "the stored generation request digest does not match its evidence",
        ));
    }
    let canonical = encode_generation_document(&request, &wire.artifact)?;
    if canonical != bytes {
        return Err(store_error(
            LocalInstallGenerationStoreErrorKind::CorruptState,
            "the generation document is not canonically encoded",
        ));
    }
    Ok(LoadedGeneration {
        generation,
        operation_id,
        request_digest,
        document_bytes: bytes.to_vec(),
        artifact_bytes: wire.artifact.bytes,
    })
}

fn parse_generation(
    wire: &GenerationWire,
) -> Result<InstalledLocalBinaryGeneration, LocalInstallGenerationStoreError> {
    let identity = parse_identity(&wire.identity)?;
    let predecessor = wire.predecessor.as_ref().map(parse_identity).transpose()?;
    let generation = match wire.source.identity_generation {
        IdentityGenerationWire::SmolrunnerV1 => LocalInstallIdentityGeneration::SmolrunnerV1,
        IdentityGenerationWire::GlaedaV2 => LocalInstallIdentityGeneration::GlaedaV2,
    };
    let source = LocalInstallSourceIdentity::with_identity_generation(
        generation,
        CommitId::parse(&wire.source.commit).map_err(|_| invalid_stored_identity())?,
        GitTreeId::parse(&wire.source.tree).map_err(|_| invalid_stored_identity())?,
        parse_sha256(&wire.source.cargo_lock_digest, "stored Cargo.lock digest")?,
        LocalInstallToolchainIdentity::parse(&wire.source.toolchain)
            .map_err(|_| invalid_stored_identity())?,
    )
    .map_err(|_| invalid_stored_identity())?;
    if source.digest().as_str() != wire.source.digest {
        return Err(invalid_stored_identity());
    }
    Ok(InstalledLocalBinaryGeneration {
        identity,
        predecessor,
        source,
        binary_digest: parse_sha256(&wire.binary_digest, "stored binary digest")?,
        binary_version: wire.binary_version.clone(),
    })
}

fn parse_identity(
    wire: &IdentityWire,
) -> Result<LocalInstallGenerationIdentity, LocalInstallGenerationStoreError> {
    if wire.number == 0 {
        return Err(invalid_stored_identity());
    }
    Ok(LocalInstallGenerationIdentity {
        number: wire.number,
        digest: parse_sha256(&wire.digest, "stored generation digest")?,
    })
}

fn parse_operation(
    wire: &OperationWire,
) -> Result<(LocalInstallStoreOperationId, Sha256Digest), LocalInstallGenerationStoreError> {
    Ok((
        LocalInstallStoreOperationId::parse(&wire.id).map_err(|_| invalid_stored_identity())?,
        parse_sha256(&wire.request_digest, "stored request digest")?,
    ))
}

fn parse_sha256(
    value: &str,
    subject: &str,
) -> Result<Sha256Digest, LocalInstallGenerationStoreError> {
    Sha256Digest::parse(value).map_err(|_| {
        store_error(
            LocalInstallGenerationStoreErrorKind::CorruptState,
            format!("{subject} is invalid"),
        )
    })
}

fn invalid_stored_identity() -> LocalInstallGenerationStoreError {
    store_error(
        LocalInstallGenerationStoreErrorKind::CorruptState,
        "the stored local-install identity is invalid",
    )
}

fn encode_current(
    wire: &CurrentEncodeWire<'_>,
) -> Result<Vec<u8>, LocalInstallGenerationStoreError> {
    let bytes = serde_json::to_vec(wire).map_err(|_| {
        store_error(
            LocalInstallGenerationStoreErrorKind::CorruptState,
            "the current-generation document could not be encoded",
        )
    })?;
    if bytes.len() > MAX_LOCAL_INSTALL_CURRENT_DOCUMENT_BYTES {
        return Err(store_error(
            LocalInstallGenerationStoreErrorKind::CorruptState,
            "the current-generation document exceeds its byte limit",
        ));
    }
    Ok(bytes)
}

fn decode_current_bytes(bytes: &[u8]) -> Result<CurrentWire, LocalInstallGenerationStoreError> {
    let wire: CurrentWire = serde_json::from_slice(bytes).map_err(|_| {
        store_error(
            LocalInstallGenerationStoreErrorKind::CorruptState,
            "the current-generation document is invalid",
        )
    })?;
    if wire.store_schema_version != LOCAL_INSTALL_GENERATION_STORE_SCHEMA_VERSION
        || wire.store_generation != StoreGenerationWire::GlaedaCurrentV1
    {
        return Err(store_error(
            LocalInstallGenerationStoreErrorKind::CorruptState,
            "the current-generation document uses an unsupported schema",
        ));
    }
    let canonical = encode_current(&CurrentEncodeWire {
        store_schema_version: wire.store_schema_version,
        store_generation: wire.store_generation,
        accepted: wire.accepted.as_ref().map(|value| IdentityEncodeWire {
            number: value.number,
            digest: &value.digest,
        }),
        retained: wire.retained.as_ref().map(|value| IdentityEncodeWire {
            number: value.number,
            digest: &value.digest,
        }),
        retiring: wire.retiring.as_ref().map(|value| IdentityEncodeWire {
            number: value.number,
            digest: &value.digest,
        }),
        last_operation: wire
            .last_operation
            .as_ref()
            .map(|value| OperationEncodeWire {
                id: &value.id,
                request_digest: &value.request_digest,
            }),
    })?;
    if canonical != bytes {
        return Err(store_error(
            LocalInstallGenerationStoreErrorKind::CorruptState,
            "the current-generation document is not canonically encoded",
        ));
    }
    Ok(wire)
}

fn current_successor_wire<'a>(
    current: &'a LoadedCurrent,
    successor: &'a LoadedGeneration,
) -> CurrentEncodeWire<'a> {
    CurrentEncodeWire {
        store_schema_version: LOCAL_INSTALL_GENERATION_STORE_SCHEMA_VERSION,
        store_generation: StoreGenerationWire::GlaedaCurrentV1,
        accepted: Some(IdentityEncodeWire::from(&successor.generation.identity)),
        retained: current
            .accepted_identity
            .as_ref()
            .map(IdentityEncodeWire::from),
        retiring: current
            .retained_identity
            .as_ref()
            .map(IdentityEncodeWire::from),
        last_operation: Some(OperationEncodeWire {
            id: successor.operation_id.as_str(),
            request_digest: successor.request_digest.as_str(),
        }),
    }
}

fn require_current_successor(
    current: &LoadedCurrent,
    next: &LoadedCurrent,
) -> Result<(), LocalInstallGenerationStoreError> {
    let cleanup_completion = current.retiring_identity.is_some()
        && next.retiring_identity.is_none()
        && next.accepted_identity == current.accepted_identity
        && next.retained_identity == current.retained_identity
        && next.operation == current.operation;
    if cleanup_completion {
        return Ok(());
    }
    let Some(next_accepted) = next.snapshot.state.accepted.as_ref() else {
        return Err(store_error(
            LocalInstallGenerationStoreErrorKind::RecoveryRequired,
            "the staged current document does not name a successor",
        ));
    };
    if current.retiring_identity.is_some()
        || next_accepted.predecessor.as_ref() != current.accepted_identity.as_ref()
        || next.retained_identity.as_ref() != current.accepted_identity.as_ref()
        || next.retiring_identity.as_ref() != current.retained_identity.as_ref()
    {
        return Err(store_error(
            LocalInstallGenerationStoreErrorKind::RecoveryRequired,
            "the staged current document is not the exact accepted successor",
        ));
    }
    Ok(())
}

fn referenced_generation_names(
    current: &LoadedCurrent,
) -> Result<Vec<String>, LocalInstallGenerationStoreError> {
    let mut names = Vec::new();
    if let Some(identity) = &current.accepted_identity {
        names.push(generation_directory_name(identity)?);
    }
    if let Some(identity) = &current.retained_identity {
        names.push(generation_directory_name(identity)?);
    }
    names.sort();
    Ok(names)
}

fn remove_generation_directory(
    parent: &OwnedFd,
    name: &str,
    directory: &OwnedFd,
    owner: (u32, u32),
) -> Result<(), LocalInstallGenerationStoreError> {
    inspect_directory(
        directory,
        Some(owner),
        true,
        "local-install generation directory",
    )?;
    let names = enumerate_names(
        directory,
        2,
        |entry| matches!(entry, GENERATION_DOCUMENT | BINARY_FILE),
        "local-install generation directory",
    )?;
    if names != [GENERATION_DOCUMENT.to_owned(), BINARY_FILE.to_owned()] {
        return Err(store_error(
            LocalInstallGenerationStoreErrorKind::RecoveryRequired,
            "the local-install generation cannot be removed because its shape changed",
        ));
    }
    fs::unlinkat(directory, BINARY_FILE, AtFlags::empty()).map_err(|_| {
        store_error(
            LocalInstallGenerationStoreErrorKind::Io,
            "the superseded Glaeda binary could not be removed",
        )
    })?;
    fs::unlinkat(directory, GENERATION_DOCUMENT, AtFlags::empty()).map_err(|_| {
        store_error(
            LocalInstallGenerationStoreErrorKind::Io,
            "the superseded generation document could not be removed",
        )
    })?;
    synchronize_directory(directory, "retired generation directory")?;
    fs::unlinkat(parent, name, AtFlags::REMOVEDIR).map_err(|_| {
        store_error(
            LocalInstallGenerationStoreErrorKind::Io,
            "the superseded generation directory could not be removed",
        )
    })
}

fn maybe_fail(
    selected: Option<FaultBoundary>,
    boundary: FaultBoundary,
) -> Result<(), LocalInstallGenerationStoreError> {
    if selected == Some(boundary) {
        return Err(store_error(
            LocalInstallGenerationStoreErrorKind::InjectedFailure,
            format!("injected local-install failure after {boundary:?}"),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs::{self as std_fs, OpenOptions};
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT_TEST: AtomicU64 = AtomicU64::new(1);

    struct TestParent {
        path: PathBuf,
    }

    impl TestParent {
        fn new(label: &str) -> Self {
            let sequence = NEXT_TEST.fetch_add(1, Ordering::Relaxed);
            let temporary_root = std_fs::canonicalize(std::env::temp_dir())
                .expect("canonicalize test temporary directory");
            let path = temporary_root.join(format!(
                "glaeda-local-generation-store-{label}-{}-{sequence}",
                std::process::id()
            ));
            std_fs::create_dir(&path).expect("create exact test parent");
            std_fs::set_permissions(&path, std_fs::Permissions::from_mode(0o700))
                .expect("set test parent mode");
            Self { path }
        }
    }

    impl std::ops::Deref for TestParent {
        type Target = Path;

        fn deref(&self) -> &Self::Target {
            &self.path
        }
    }

    impl Drop for TestParent {
        fn drop(&mut self) {
            std_fs::remove_dir_all(&self.path).expect("remove exact test parent");
        }
    }

    struct TestStore {
        parent: TestParent,
        store: UnixLocalInstallGenerationStore,
    }

    impl TestStore {
        fn new(label: &str) -> Self {
            let parent = TestParent::new(label);
            let store = UnixLocalInstallGenerationStore::open_for_test(&parent.path)
                .expect("open test generation store");
            Self { parent, store }
        }

        fn root(&self) -> PathBuf {
            self.parent.path.join(LOCAL_INSTALL_DIRECTORY)
        }

        fn binary(&self, label: &str, bytes: &[u8]) -> PathBuf {
            let path = self.parent.path.join(format!("candidate-{label}"));
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o500)
                .open(&path)
                .expect("create candidate binary");
            file.write_all(bytes).expect("write candidate binary");
            file.sync_all().expect("sync candidate binary");
            std_fs::set_permissions(&path, std_fs::Permissions::from_mode(0o500))
                .expect("set candidate mode");
            path
        }
    }

    fn digest_bytes(bytes: &[u8]) -> Sha256Digest {
        Sha256Digest::parse(&format!("sha256:{:x}", Sha256::digest(bytes)))
            .expect("canonical digest")
    }

    fn source(marker: char) -> LocalInstallSourceIdentity {
        LocalInstallSourceIdentity::new(
            CommitId::parse(&marker.to_string().repeat(40)).expect("commit"),
            GitTreeId::parse(&((marker as u8 + 1) as char).to_string().repeat(40)).expect("tree"),
            Sha256Digest::parse(&format!("sha256:{}", marker.to_string().repeat(64)))
                .expect("lock digest"),
            LocalInstallToolchainIdentity::parse("rust-1.97.1-x86_64-unknown-linux-gnu")
                .expect("toolchain"),
        )
        .expect("source")
    }

    fn candidate(
        predecessor: Option<LocalInstallGenerationIdentity>,
        marker: char,
        bytes: &[u8],
    ) -> InstalledLocalBinaryGeneration {
        let source = source(marker);
        let plan = LocalInstallBuildPlan {
            target_generation: predecessor.as_ref().map_or(1, |value| value.number + 1),
            expected_predecessor: predecessor.clone(),
            source: source.clone(),
        };
        complete_local_install_build(
            &plan,
            BuiltLocalBinaryEvidence::new(
                source.digest().clone(),
                predecessor,
                digest_bytes(bytes),
                format!("glaeda 0.1.{marker}"),
            )
            .expect("evidence"),
        )
        .expect("candidate")
    }

    fn operation(marker: char) -> LocalInstallStoreOperationId {
        LocalInstallStoreOperationId::parse(&format!("sha256:{}", marker.to_string().repeat(64)))
            .expect("operation")
    }

    fn request(
        operation_marker: char,
        predecessor: Option<LocalInstallGenerationIdentity>,
        candidate: InstalledLocalBinaryGeneration,
    ) -> LocalInstallGenerationPublishRequest {
        LocalInstallGenerationPublishRequest::new(
            operation(operation_marker),
            predecessor,
            candidate,
        )
        .expect("request")
    }

    #[test]
    fn publishes_replays_and_refuses_changed_operation_or_stale_predecessor() {
        let test = TestStore::new("publish-replay-cas");
        let empty = test.store.load().expect("load empty");
        assert!(empty.state().accepted.is_none());

        let bytes = b"#!/bin/false\nfirst verified glaeda binary\n";
        let binary = test.binary("first", bytes);
        let first = candidate(None, 'a', bytes);
        let first_request = request('1', None, first.clone());
        let published = test
            .store
            .publish(&first_request, &binary)
            .expect("publish first");
        assert_eq!(
            published.disposition(),
            LocalInstallGenerationPublishDisposition::Published
        );
        assert_eq!(published.state().state().accepted.as_ref(), Some(&first));

        let replayed = test
            .store
            .publish(&first_request, &binary)
            .expect("replay first");
        assert_eq!(
            replayed.disposition(),
            LocalInstallGenerationPublishDisposition::Replayed
        );

        let second_bytes = b"#!/bin/false\nsecond verified glaeda binary\n";
        let second_binary = test.binary("second", second_bytes);
        let second = candidate(Some(first.identity.clone()), 'b', second_bytes);
        let changed_operation = request('1', Some(first.identity.clone()), second);
        assert_eq!(
            test.store
                .publish(&changed_operation, &second_binary)
                .expect_err("changed operation conflicts")
                .kind(),
            LocalInstallGenerationStoreErrorKind::Conflict
        );

        let stale = request('2', None, first);
        assert_eq!(
            test.store
                .publish(&stale, &binary)
                .expect_err("stale predecessor conflicts")
                .kind(),
            LocalInstallGenerationStoreErrorKind::Conflict
        );
    }

    #[test]
    fn operation_identity_cannot_be_reused_from_the_retained_generation() {
        let test = TestStore::new("retained-operation");
        let first_bytes = b"#!/bin/false\nfirst operation\n";
        let first_binary = test.binary("first", first_bytes);
        let first = candidate(None, 'a', first_bytes);
        test.store
            .publish(&request('1', None, first.clone()), first_binary)
            .expect("publish first");

        let second_bytes = b"#!/bin/false\nsecond operation\n";
        let second_binary = test.binary("second", second_bytes);
        let second = candidate(Some(first.identity.clone()), 'b', second_bytes);
        test.store
            .publish(
                &request('2', Some(first.identity.clone()), second.clone()),
                second_binary,
            )
            .expect("publish second");

        let third_bytes = b"#!/bin/false\nthird operation\n";
        let third_binary = test.binary("third", third_bytes);
        let third = candidate(Some(second.identity.clone()), 'c', third_bytes);
        assert_eq!(
            test.store
                .publish(&request('1', Some(second.identity), third), third_binary,)
                .expect_err("retained operation identity is globally reserved")
                .kind(),
            LocalInstallGenerationStoreErrorKind::Conflict
        );
    }

    #[test]
    fn retains_exactly_one_predecessor_and_removes_only_proven_older_generation() {
        let test = TestStore::new("retention");
        let mut predecessor = None;
        let mut generations = Vec::new();
        for (index, marker) in ['a', 'b', 'c'].into_iter().enumerate() {
            let bytes = format!("#!/bin/false\nverified generation {marker}\n");
            let binary = test.binary(&format!("generation-{marker}"), bytes.as_bytes());
            let next = candidate(predecessor.clone(), marker, bytes.as_bytes());
            let next_request = request(
                char::from_digit((index + 1) as u32, 10).expect("operation marker"),
                predecessor.clone(),
                next.clone(),
            );
            test.store
                .publish(&next_request, binary)
                .expect("publish successor");
            predecessor = Some(next.identity.clone());
            generations.push(next);
        }
        let loaded = test.store.load().expect("load retained state");
        assert_eq!(loaded.state().accepted.as_ref(), Some(&generations[2]));
        assert_eq!(loaded.state().retained, vec![generations[1].clone()]);
        assert_eq!(
            test.store
                .generation_names()
                .expect("generation names")
                .len(),
            2
        );
        assert!(
            !test
                .root()
                .join(GENERATIONS_DIRECTORY)
                .join(generation_directory_name(&generations[0].identity).expect("name"))
                .exists()
        );
    }

    #[test]
    fn nonblocking_lock_and_recovery_debt_prevent_half_state_reads() {
        let test = TestStore::new("reader-lock");
        let competing = fs::openat(
            &test.store.root,
            LOCK_FILE,
            EXISTING_LOCK_FLAGS,
            Mode::empty(),
        )
        .expect("open competing lock");
        fs::flock(&competing, FlockOperation::NonBlockingLockExclusive).expect("hold writer lock");
        assert_eq!(
            test.store.load().expect_err("reader is nonblocking").kind(),
            LocalInstallGenerationStoreErrorKind::Busy
        );
        fs::flock(&competing, FlockOperation::Unlock).expect("release writer lock");

        let bytes = b"#!/bin/false\ninterrupted candidate\n";
        let binary = test.binary("interrupted", bytes);
        let first = candidate(None, 'a', bytes);
        let first_request = request('1', None, first);
        assert_eq!(
            test.store
                .publish_inner(
                    &first_request,
                    &binary,
                    Some(FaultBoundary::StageDirectoryCreated),
                )
                .expect_err("inject stage failure")
                .kind(),
            LocalInstallGenerationStoreErrorKind::InjectedFailure
        );
        assert_eq!(
            test.store
                .load()
                .expect_err("debt is not a half state")
                .kind(),
            LocalInstallGenerationStoreErrorKind::RecoveryRequired
        );
        assert_eq!(
            test.store
                .recover()
                .expect_err("incomplete debt is preserved")
                .kind(),
            LocalInstallGenerationStoreErrorKind::RecoveryRequired
        );
    }

    #[test]
    fn every_public_operation_rejects_unsafe_boundaries_after_lock_acquisition() {
        let test = TestStore::new("single-boundary-verification");
        let bytes = b"#!/bin/false\nunsafe boundary candidate\n";
        let binary = test.binary("candidate", bytes);
        let generation = candidate(None, 'a', bytes);
        let publish = request('1', None, generation);
        std_fs::set_permissions(
            test.root().join(GENERATIONS_DIRECTORY),
            std_fs::Permissions::from_mode(0o755),
        )
        .expect("make generations boundary unsafe");

        assert_eq!(
            test.store.load().expect_err("load refuses boundary").kind(),
            LocalInstallGenerationStoreErrorKind::UnsafeFilesystem
        );
        assert_eq!(
            test.store
                .recover()
                .expect_err("recovery refuses boundary")
                .kind(),
            LocalInstallGenerationStoreErrorKind::UnsafeFilesystem
        );
        assert_eq!(
            test.store
                .launcher_targets()
                .expect_err("launcher observation refuses boundary")
                .kind(),
            LocalInstallGenerationStoreErrorKind::UnsafeFilesystem
        );
        assert_eq!(
            test.store
                .publish(&publish, &binary)
                .expect_err("publication refuses boundary")
                .kind(),
            LocalInstallGenerationStoreErrorKind::UnsafeFilesystem
        );
        assert!(
            test.store
                .generation_names()
                .expect("inspect unchanged generations")
                .is_empty()
        );
        assert!(
            std_fs::read_dir(test.root().join(STAGED_DIRECTORY))
                .expect("inspect unchanged staging")
                .next()
                .is_none()
        );
    }

    #[test]
    fn boundary_refusal_unlocks_even_when_a_duplicate_descriptor_survives() {
        for mode in [StoreLockMode::Shared, StoreLockMode::Exclusive] {
            let test = TestStore::new("refused-lock-duplicate");
            std_fs::set_permissions(
                test.root().join(GENERATIONS_DIRECTORY),
                std_fs::Permissions::from_mode(0o755),
            )
            .expect("make generations boundary unsafe");
            let mut survivor = None;
            let refusal = test.store.acquire_lock_with_observer(mode, |lock| {
                survivor = Some(lock.try_clone().expect("duplicate acquired description"));
            });
            assert_eq!(
                refusal.err().expect("refuse unsafe boundary").kind(),
                LocalInstallGenerationStoreErrorKind::UnsafeFilesystem
            );
            let competing = fs::openat(
                &test.store.root,
                LOCK_FILE,
                EXISTING_LOCK_FLAGS,
                Mode::empty(),
            )
            .expect("open independent description");
            fs::flock(&competing, FlockOperation::NonBlockingLockExclusive)
                .expect("refusal explicitly releases lock despite surviving duplicate");
            fs::flock(&competing, FlockOperation::Unlock).expect("release test lock");
            drop(survivor.expect("keep duplicate alive through reacquisition"));
        }
    }

    #[test]
    fn restart_recovery_is_bounded_at_every_publication_boundary() {
        let boundaries = [
            FaultBoundary::StageDirectoryCreated,
            FaultBoundary::BinaryWritten,
            FaultBoundary::BinarySynchronized,
            FaultBoundary::DocumentWritten,
            FaultBoundary::DocumentSynchronized,
            FaultBoundary::StageDirectorySynchronized,
            FaultBoundary::GenerationPublished,
            FaultBoundary::GenerationsDirectorySynchronized,
            FaultBoundary::CurrentDocumentSynchronized,
            FaultBoundary::CurrentSwitched,
            FaultBoundary::RootSynchronized,
            FaultBoundary::CleanupStarted,
            FaultBoundary::CleanupFinished,
        ];
        for (index, boundary) in boundaries.into_iter().enumerate() {
            let test = TestStore::new(&format!("fault-{index}"));
            let bytes = format!("#!/bin/false\nfault boundary {index}\n");
            let binary = test.binary("candidate", bytes.as_bytes());
            let first = candidate(None, 'a', bytes.as_bytes());
            let first_request = request('1', None, first.clone());
            assert_eq!(
                test.store
                    .publish_inner(&first_request, &binary, Some(boundary))
                    .expect_err("fault must interrupt publication")
                    .kind(),
                LocalInstallGenerationStoreErrorKind::InjectedFailure,
                "boundary {boundary:?}"
            );
            let recovery = test.store.recover();
            if matches!(
                boundary,
                FaultBoundary::StageDirectoryCreated
                    | FaultBoundary::BinaryWritten
                    | FaultBoundary::BinarySynchronized
            ) {
                assert_eq!(
                    recovery
                        .expect_err("incomplete stage must be preserved")
                        .kind(),
                    LocalInstallGenerationStoreErrorKind::RecoveryRequired,
                    "boundary {boundary:?}"
                );
                let _guard = test
                    .store
                    .acquire_lock(StoreLockMode::Shared)
                    .expect("inspect protected predecessor");
                assert!(
                    test.store
                        .load_current_document_locked()
                        .expect("load protected predecessor")
                        .accepted_identity
                        .is_none(),
                    "boundary {boundary:?}"
                );
            } else {
                let recovered = recovery.expect("complete exact evidence recovers");
                assert_eq!(recovered.state().state().accepted.as_ref(), Some(&first));
                assert_eq!(
                    test.store.load().expect("clean recovered state").state(),
                    recovered.state().state()
                );
            }
        }
    }

    #[test]
    fn predecessor_retirement_recovers_after_each_destructive_boundary() {
        for (index, boundary) in [
            FaultBoundary::RetirementPublished,
            FaultBoundary::RetirementBinaryRemoved,
            FaultBoundary::RetirementDocumentRemoved,
        ]
        .into_iter()
        .enumerate()
        {
            let test = TestStore::new(&format!("retirement-fault-{index}"));
            let mut predecessor = None;
            let mut generations = Vec::new();
            for (sequence, marker) in ['a', 'b'].into_iter().enumerate() {
                let bytes = format!("#!/bin/false\nseed {marker}\n");
                let binary = test.binary(&format!("seed-{marker}"), bytes.as_bytes());
                let next = candidate(predecessor.clone(), marker, bytes.as_bytes());
                let publish = request(
                    char::from_digit((sequence + 1) as u32, 10).expect("operation"),
                    predecessor.clone(),
                    next.clone(),
                );
                test.store.publish(&publish, binary).expect("publish seed");
                predecessor = Some(next.identity.clone());
                generations.push(next);
            }
            let bytes = b"#!/bin/false\nthird successor\n";
            let binary = test.binary("third", bytes);
            let third = candidate(predecessor.clone(), 'c', bytes);
            let publish = request('3', predecessor, third.clone());
            assert_eq!(
                test.store
                    .publish_inner(&publish, &binary, Some(boundary))
                    .expect_err("interrupt retirement")
                    .kind(),
                LocalInstallGenerationStoreErrorKind::InjectedFailure
            );
            let recovered = test.store.recover().expect("finish exact retirement debt");
            assert_eq!(
                recovered.disposition(),
                LocalInstallGenerationRecoveryDisposition::RemovedCleanupDebt
            );
            assert_eq!(recovered.state().state().accepted.as_ref(), Some(&third));
            assert_eq!(
                recovered.state().state().retained,
                vec![generations[1].clone()]
            );
            let staged = test.store.inspect_staged_entries().expect("clean staged");
            assert!(staged.operations.is_empty());
            assert!(staged.retirements.is_empty());
            assert_eq!(test.store.generation_names().expect("generations").len(), 2);
        }
    }

    #[test]
    fn rejects_source_aliases_and_stored_artifact_tampering() {
        let test = TestStore::new("aliases");
        let bytes = b"#!/bin/false\nverified binary\n";
        let binary = test.binary("source", bytes);
        let first = candidate(None, 'a', bytes);
        let first_request = request('1', None, first.clone());

        let alias = test.parent.join("source-alias");
        std_fs::hard_link(&binary, &alias).expect("hard link source");
        assert_eq!(
            test.store
                .publish(&first_request, &binary)
                .expect_err("hard-linked source refused")
                .kind(),
            LocalInstallGenerationStoreErrorKind::UnsafeFilesystem
        );
        std_fs::remove_file(&alias).expect("remove exact source alias");

        let symlink = test.parent.join("source-symlink");
        std::os::unix::fs::symlink(&binary, &symlink).expect("symlink source");
        assert_eq!(
            test.store
                .publish(&first_request, &symlink)
                .expect_err("symlink source refused")
                .kind(),
            LocalInstallGenerationStoreErrorKind::UnsafeFilesystem
        );
        std_fs::remove_file(&symlink).expect("remove exact source symlink");

        test.store
            .publish(&first_request, &binary)
            .expect("publish safe source");
        let stored = test
            .root()
            .join(GENERATIONS_DIRECTORY)
            .join(generation_directory_name(&first.identity).expect("generation name"))
            .join(BINARY_FILE);
        std_fs::set_permissions(&stored, std_fs::Permissions::from_mode(0o700))
            .expect("tamper stored mode");
        assert_eq!(
            test.store.load().expect_err("tampered mode refused").kind(),
            LocalInstallGenerationStoreErrorKind::UnsafeFilesystem
        );
    }

    #[test]
    fn documents_are_strict_canonical_and_public_receipts_are_path_free() {
        let test = TestStore::new("canonical-private");
        let bytes = b"#!/bin/false\ncanonical binary\n";
        let binary = test.binary("canonical", bytes);
        let first = candidate(None, 'a', bytes);
        let first_request = request('1', None, first);
        let receipt = test
            .store
            .publish(&first_request, binary)
            .expect("publish canonical");
        let public = serde_json::to_string(&receipt).expect("serialize public receipt");
        for private in [
            test.parent.to_string_lossy().as_ref(),
            "/home/",
            "/Users/",
            "XDG_DATA_HOME",
            "HOME=",
            "uid",
            "inode",
        ] {
            assert!(
                !public.contains(private),
                "leaked private marker: {private}"
            );
        }

        let current = test.root().join(CURRENT_FILE);
        let original = std_fs::read(&current).expect("read current");
        let mut value: serde_json::Value = serde_json::from_slice(&original).expect("current json");
        value
            .as_object_mut()
            .expect("current object")
            .insert("unknown".to_owned(), serde_json::Value::Bool(true));
        let changed = serde_json::to_vec(&value).expect("changed current");
        std_fs::write(&current, changed).expect("replace current bytes in place");
        std_fs::set_permissions(&current, std_fs::Permissions::from_mode(0o600))
            .expect("restore current mode");
        assert_eq!(
            test.store
                .load()
                .expect_err("unknown current field refused")
                .kind(),
            LocalInstallGenerationStoreErrorKind::CorruptState
        );
    }

    #[test]
    fn request_constructor_rejects_legacy_and_noncanonical_successors() {
        let current = candidate(None, 'a', b"first");
        let mut wrong_number = current.clone();
        wrong_number.identity.number = 2;
        assert_eq!(
            LocalInstallGenerationPublishRequest::new(operation('1'), None, wrong_number)
                .expect_err("wrong successor number")
                .kind(),
            LocalInstallGenerationStoreErrorKind::InvalidRequest
        );

        let legacy_source = LocalInstallSourceIdentity::with_identity_generation(
            LocalInstallIdentityGeneration::SmolrunnerV1,
            CommitId::parse(&"a".repeat(40)).expect("commit"),
            GitTreeId::parse(&"b".repeat(40)).expect("tree"),
            Sha256Digest::parse(&format!("sha256:{}", "c".repeat(64))).expect("lock"),
            LocalInstallToolchainIdentity::parse("rust-1.97.1-x86_64-unknown-linux-gnu")
                .expect("toolchain"),
        )
        .expect("legacy source");
        let legacy_plan = LocalInstallBuildPlan {
            target_generation: 1,
            expected_predecessor: None,
            source: legacy_source.clone(),
        };
        let legacy = complete_local_install_build(
            &legacy_plan,
            BuiltLocalBinaryEvidence::new(
                legacy_source.digest().clone(),
                None,
                digest_bytes(b"legacy"),
                "smolrunner 0.1.0",
            )
            .expect("legacy evidence"),
        )
        .expect("legacy candidate");
        assert_eq!(
            LocalInstallGenerationPublishRequest::new(operation('2'), None, legacy)
                .expect_err("legacy publication refused")
                .kind(),
            LocalInstallGenerationStoreErrorKind::InvalidRequest
        );
    }

    #[test]
    fn operation_and_directory_names_are_strictly_canonical() {
        assert!(LocalInstallStoreOperationId::parse("sha256:ABC").is_err());
        assert!(operation_id_from_directory_name("op-ABC").is_err());
        assert!(
            identity_from_generation_directory_name(&format!("g01-{}", "a".repeat(64))).is_err()
        );
        assert!(
            identity_from_generation_directory_name(&format!("g1-{}", "A".repeat(64))).is_err()
        );
    }

    #[test]
    fn location_debug_never_contains_private_path() {
        let test = TestStore::new("debug-private");
        let debug = format!("{:?}", test.store);
        assert!(debug.contains("<private-local-install-store-path>"));
        assert!(!debug.contains(test.parent.to_string_lossy().as_ref()));
        assert!(!debug.contains("SmolRunner"));
    }

    #[test]
    fn current_next_recovery_finishes_only_the_exact_successor() {
        let test = TestStore::new("current-next");
        let bytes = b"#!/bin/false\ncurrent next\n";
        let binary = test.binary("candidate", bytes);
        let first = candidate(None, 'a', bytes);
        let first_request = request('1', None, first.clone());
        assert_eq!(
            test.store
                .publish_inner(
                    &first_request,
                    &binary,
                    Some(FaultBoundary::CurrentDocumentSynchronized),
                )
                .expect_err("interrupt before current rename")
                .kind(),
            LocalInstallGenerationStoreErrorKind::InjectedFailure
        );
        assert!(test.root().join(CURRENT_NEXT_FILE).exists());
        let recovered = test
            .store
            .recover()
            .expect("recover exact current successor");
        assert_eq!(
            recovered.disposition(),
            LocalInstallGenerationRecoveryDisposition::CompletedCurrentSwitch
        );
        assert_eq!(recovered.state().state().accepted.as_ref(), Some(&first));
        assert!(!test.root().join(CURRENT_NEXT_FILE).exists());
    }

    #[test]
    fn concurrent_same_predecessor_has_one_winner_and_one_stale_conflict() {
        use std::sync::Barrier;

        let test = TestStore::new("concurrent-cas");
        let bytes_a = b"#!/bin/false\nconcurrent a\n";
        let bytes_b = b"#!/bin/false\nconcurrent b\n";
        let binary_a = test.binary("a", bytes_a);
        let binary_b = test.binary("b", bytes_b);
        let request_a = request('1', None, candidate(None, 'a', bytes_a));
        let request_b = request('2', None, candidate(None, 'b', bytes_b));
        let barrier = Barrier::new(3);
        let (left, right) = std::thread::scope(|scope| {
            let left = scope.spawn(|| {
                barrier.wait();
                test.store.publish(&request_a, &binary_a)
            });
            let right = scope.spawn(|| {
                barrier.wait();
                test.store.publish(&request_b, &binary_b)
            });
            barrier.wait();
            (
                left.join().expect("left writer thread"),
                right.join().expect("right writer thread"),
            )
        });
        let published = [&left, &right]
            .into_iter()
            .filter(|result| {
                result.as_ref().is_ok_and(|receipt| {
                    receipt.disposition() == LocalInstallGenerationPublishDisposition::Published
                })
            })
            .count();
        assert_eq!(published, 1);
        let (loser, loser_request, loser_binary) = if left.is_err() {
            (left, &request_a, &binary_a)
        } else {
            (right, &request_b, &binary_b)
        };
        assert!(matches!(
            loser.expect_err("one writer loses initial race").kind(),
            LocalInstallGenerationStoreErrorKind::Busy
                | LocalInstallGenerationStoreErrorKind::Conflict
        ));
        assert_eq!(
            test.store
                .publish(loser_request, loser_binary)
                .expect_err("the losing writer is stale after contention")
                .kind(),
            LocalInstallGenerationStoreErrorKind::Conflict
        );
    }

    #[test]
    fn exact_generation_documents_round_trip_without_private_state() {
        let bytes = b"binary bytes";
        let generation = candidate(None, 'a', bytes);
        let request = request('1', None, generation.clone());
        let artifact = ArtifactWire {
            file_type: "regular_file".to_owned(),
            owner: "store_owner".to_owned(),
            mode: "0500".to_owned(),
            links: 1,
            bytes: bytes.len() as u64,
        };
        let encoded = encode_generation_document(&request, &artifact).expect("encode");
        let decoded = decode_generation_document(&encoded).expect("decode");
        assert_eq!(decoded.generation, generation);
        assert_eq!(decoded.operation_id, operation('1'));
        let text = std::str::from_utf8(&encoded).expect("UTF-8");
        for private in [
            "/home/",
            "/Users/",
            "HOME=",
            "XDG_DATA_HOME",
            "inode",
            "uid",
        ] {
            assert!(!text.contains(private), "leaked private marker: {private}");
        }
    }

    #[test]
    fn fixed_layout_has_no_legacy_namespace_or_caller_controlled_basename() {
        #[cfg(target_os = "linux")]
        assert_eq!(GLAEDA_DIRECTORY, "glaeda");
        #[cfg(target_os = "macos")]
        assert_eq!(GLAEDA_DIRECTORY, "Glaeda");
        assert_eq!(LOCAL_INSTALL_DIRECTORY, "local-install");
        assert_eq!(STORE_IDENTITY_FILE, "store.identity.json");
        assert_eq!(BINARY_FILE, "glaeda");
        for fixed in [
            GLAEDA_DIRECTORY,
            LOCAL_INSTALL_DIRECTORY,
            STORE_IDENTITY_FILE,
            GENERATIONS_DIRECTORY,
            STAGED_DIRECTORY,
            BINARY_FILE,
        ] {
            assert!(!fixed.contains('/'));
            assert!(!fixed.contains("smolrunner"));
        }
    }

    #[test]
    fn explicit_current_document_rejects_noncanonical_whitespace() {
        let wire = CurrentEncodeWire {
            store_schema_version: LOCAL_INSTALL_GENERATION_STORE_SCHEMA_VERSION,
            store_generation: StoreGenerationWire::GlaedaCurrentV1,
            accepted: None,
            retained: None,
            retiring: None,
            last_operation: None,
        };
        let bytes = encode_current(&wire).expect("canonical current");
        assert!(decode_current_bytes(&bytes).is_ok());
        let mut spaced = bytes;
        spaced.push(b'\n');
        assert_eq!(
            decode_current_bytes(&spaced)
                .expect_err("noncanonical whitespace")
                .kind(),
            LocalInstallGenerationStoreErrorKind::CorruptState
        );
    }

    #[test]
    fn source_digest_mismatch_fails_before_staging() {
        let test = TestStore::new("source-digest");
        let declared = b"#!/bin/false\ndeclared\n";
        let actual = b"#!/bin/false\nactual\n";
        let binary = test.binary("actual", actual);
        let first = candidate(None, 'a', declared);
        let first_request = request('1', None, first);
        assert_eq!(
            test.store
                .publish(&first_request, binary)
                .expect_err("digest mismatch")
                .kind(),
            LocalInstallGenerationStoreErrorKind::Conflict
        );
        let staged = test.store.inspect_staged_entries().expect("stages");
        assert!(staged.operations.is_empty());
        assert!(staged.retirements.is_empty());
        assert!(
            test.store
                .generation_names()
                .expect("generations")
                .is_empty()
        );
    }

    #[test]
    fn generation_directory_rejects_extra_entries_before_cleanup() {
        let test = TestStore::new("extra-entry");
        let bytes = b"#!/bin/false\nextra entry\n";
        let binary = test.binary("candidate", bytes);
        let first = candidate(None, 'a', bytes);
        let first_request = request('1', None, first.clone());
        test.store
            .publish(&first_request, binary)
            .expect("publish first");
        let generation = test
            .root()
            .join(GENERATIONS_DIRECTORY)
            .join(generation_directory_name(&first.identity).expect("generation name"));
        std_fs::write(generation.join("foreign"), b"foreign").expect("add foreign entry");
        assert_eq!(
            test.store.load().expect_err("foreign entry refused").kind(),
            LocalInstallGenerationStoreErrorKind::CorruptState
        );
    }

    #[test]
    fn store_root_rejects_foreign_entries_without_repair() {
        let test = TestStore::new("foreign-root");
        std_fs::write(test.root().join("foreign"), b"foreign").expect("foreign root entry");
        assert_eq!(
            test.store.load().expect_err("foreign root refused").kind(),
            LocalInstallGenerationStoreErrorKind::CorruptState
        );
        assert!(test.root().join("foreign").exists());
    }

    #[test]
    fn preexisting_unmarked_foreign_root_is_never_changed() {
        let parent = TestParent::new("unmarked-foreign-root");
        let root = parent.path.join(LOCAL_INSTALL_DIRECTORY);
        std_fs::create_dir(&root).expect("create unmarked root");
        std_fs::set_permissions(&root, std_fs::Permissions::from_mode(0o700))
            .expect("set unmarked root mode");
        std_fs::write(root.join("foreign"), b"foreign bytes").expect("write foreign entry");

        assert_eq!(
            UnixLocalInstallGenerationStore::open_for_test(&parent.path)
                .expect_err("unmarked foreign root must not be adopted")
                .kind(),
            LocalInstallGenerationStoreErrorKind::UnsafeFilesystem
        );
        let names = std_fs::read_dir(&root)
            .expect("read unchanged foreign root")
            .map(|entry| {
                entry
                    .expect("read foreign root entry")
                    .file_name()
                    .into_string()
                    .expect("ASCII test entry")
            })
            .collect::<Vec<_>>();
        assert_eq!(names, ["foreign"]);
        assert_eq!(
            std_fs::read(root.join("foreign")).expect("read unchanged foreign bytes"),
            b"foreign bytes"
        );
    }

    #[test]
    fn preexisting_empty_unmarked_root_is_never_adopted() {
        let parent = TestParent::new("unmarked-empty-root");
        let root = parent.path.join(LOCAL_INSTALL_DIRECTORY);
        std_fs::create_dir(&root).expect("create empty unmarked root");
        std_fs::set_permissions(&root, std_fs::Permissions::from_mode(0o700))
            .expect("set empty unmarked root mode");

        assert_eq!(
            UnixLocalInstallGenerationStore::open_for_test(&parent.path)
                .expect_err("empty unmarked root must not be adopted")
                .kind(),
            LocalInstallGenerationStoreErrorKind::UnsafeFilesystem
        );
        assert_eq!(
            std_fs::read_dir(&root)
                .expect("read unchanged empty root")
                .count(),
            0
        );
    }

    #[test]
    fn interrupted_private_initialization_stages_never_block_restart() {
        let parent = TestParent::new("interrupted-initialization-stages");
        let parent_fd =
            fs::open(&parent.path, DIRECTORY_FLAGS, Mode::empty()).expect("open exact test parent");
        let owner = (geteuid().as_raw(), getegid().as_raw());

        let (complete_name, complete) =
            create_initialization_stage(&parent_fd, owner).expect("create complete stage");
        create_store_identity(
            &complete,
            owner,
            LocalInstallStoreLocationClass::LinuxHomeDefault,
        )
        .expect("create complete staged identity");
        synchronize_directory(&complete, "test complete initialization stage")
            .expect("sync complete stage");
        drop(complete);

        let (partial_name, partial) =
            create_initialization_stage(&parent_fd, owner).expect("create partial stage");
        let identity = create_private_file(
            &partial,
            STORE_IDENTITY_FILE,
            PRIVATE_FILE_MODE,
            owner,
            "test partial store identity",
        )
        .expect("create partial identity");
        write_all_fd(&identity, b"{", "test partial store identity")
            .expect("write partial identity");
        fs::fsync(&identity).expect("sync partial identity");
        synchronize_directory(&partial, "test partial initialization stage")
            .expect("sync partial stage");
        drop(identity);
        drop(partial);
        synchronize_directory(&parent_fd, "test initialization parent")
            .expect("sync initialization parent");

        let store = UnixLocalInstallGenerationStore::open_for_test(&parent.path)
            .expect("publish fresh store despite abandoned private stages");
        store.verify_boundaries().expect("verify published store");
        assert!(parent.path.join(complete_name).is_dir());
        assert!(parent.path.join(partial_name).is_dir());
        assert!(parent.path.join(LOCAL_INSTALL_DIRECTORY).is_dir());
    }

    #[test]
    fn concurrent_first_initializers_publish_one_exact_store() {
        use std::sync::{Arc, Barrier};

        let parent = Arc::new(TestParent::new("concurrent-first-initializers"));
        let barrier = Arc::new(Barrier::new(3));
        let mut workers = Vec::new();
        for _ in 0..2 {
            let parent = Arc::clone(&parent);
            let barrier = Arc::clone(&barrier);
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                let store = UnixLocalInstallGenerationStore::open_for_test(&parent.path)
                    .expect("concurrent initializer must open one published store");
                store.verify_boundaries().expect("verify concurrent store");
                let stat = fs::fstat(&store.root).expect("inspect concurrent store root");
                (
                    store_root_device(&stat).expect("canonical root device"),
                    stat.st_ino,
                )
            }));
        }
        barrier.wait();
        let identities = workers
            .into_iter()
            .map(|worker| worker.join().expect("join concurrent initializer"))
            .collect::<Vec<_>>();
        assert_eq!(identities[0], identities[1]);

        let names = std_fs::read_dir(&parent.path)
            .expect("read initialization parent")
            .map(|entry| entry.expect("read initialization entry").file_name())
            .collect::<Vec<_>>();
        assert_eq!(names, [LOCAL_INSTALL_DIRECTORY]);
    }

    #[test]
    fn staged_operation_name_is_bound_to_document_operation() {
        let test = TestStore::new("stage-operation-bind");
        let bytes = b"#!/bin/false\nstage bind\n";
        let binary = test.binary("candidate", bytes);
        let first = candidate(None, 'a', bytes);
        let first_request = request('1', None, first);
        test.store
            .publish_inner(
                &first_request,
                &binary,
                Some(FaultBoundary::StageDirectorySynchronized),
            )
            .expect_err("leave complete stage");
        let original = test
            .store
            .inspect_staged_entries()
            .expect("stage name")
            .operations
            .remove(0);
        let changed = operation('2').directory_name();
        std_fs::rename(
            test.root().join(STAGED_DIRECTORY).join(&original),
            test.root().join(STAGED_DIRECTORY).join(&changed),
        )
        .expect("rebind stage name");
        assert_eq!(
            test.store
                .recover()
                .expect_err("operation rebind refused")
                .kind(),
            LocalInstallGenerationStoreErrorKind::Conflict
        );
    }

    #[test]
    fn multiple_staged_operations_are_ambiguous_and_never_mutated() {
        let test = TestStore::new("ambiguous-stages");
        for marker in ['1', '2'] {
            let name = operation(marker).directory_name();
            let path = test.root().join(STAGED_DIRECTORY).join(name);
            std_fs::create_dir(&path).expect("create exact ambiguous stage");
            std_fs::set_permissions(&path, std_fs::Permissions::from_mode(0o700))
                .expect("set stage mode");
        }
        assert_eq!(
            test.store
                .recover()
                .expect_err("multiple stages are ambiguous")
                .kind(),
            LocalInstallGenerationStoreErrorKind::RecoveryRequired
        );
        let staged = test.store.inspect_staged_entries().expect("stages remain");
        assert_eq!(staged.operations.len(), 2);
        assert!(staged.retirements.is_empty());
        let _guard = test
            .store
            .acquire_lock(StoreLockMode::Shared)
            .expect("inspect current");
        assert!(
            test.store
                .load_current_document_locked()
                .expect("current remains empty")
                .accepted_identity
                .is_none()
        );
    }

    #[test]
    fn public_location_class_and_authority_are_bounded() {
        let test = TestStore::new("public-bounds");
        let snapshot = test.store.load().expect("empty snapshot");
        assert_eq!(
            snapshot.authority(),
            LocalInstallGenerationStoreAuthority::PrivateGenerationSnapshotOnly
        );
        let serialized = serde_json::to_string(&snapshot).expect("serialize snapshot");
        assert_eq!(
            serialized,
            r#"{"store_generation":"glaeda_current_v1","state":{"retained":[]},"last_operation":null}"#
        );
    }

    #[test]
    fn recovery_rejects_an_orphan_that_is_not_the_exact_successor() {
        let test = TestStore::new("foreign-orphan");
        let bytes = b"#!/bin/false\norphan\n";
        let binary = test.binary("candidate", bytes);
        let first = candidate(None, 'a', bytes);
        let first_request = request('1', None, first.clone());
        test.store
            .publish_inner(
                &first_request,
                &binary,
                Some(FaultBoundary::GenerationPublished),
            )
            .expect_err("leave orphan generation");
        let name = generation_directory_name(&first.identity).expect("generation name");
        let rebound = format!(
            "g2-{}",
            &first.identity.digest.as_str()[SHA256_PREFIX.len()..]
        );
        std_fs::rename(
            test.root().join(GENERATIONS_DIRECTORY).join(name),
            test.root().join(GENERATIONS_DIRECTORY).join(rebound),
        )
        .expect("rebind generation name");
        assert!(matches!(
            test.store
                .recover()
                .expect_err("rebound orphan refused")
                .kind(),
            LocalInstallGenerationStoreErrorKind::Conflict
                | LocalInstallGenerationStoreErrorKind::CorruptState
        ));
    }

    #[test]
    fn binary_size_is_bounded_in_persisted_evidence() {
        let generation = candidate(None, 'a', b"binary");
        let request = request('1', None, generation);
        let artifact = ArtifactWire {
            file_type: "regular_file".to_owned(),
            owner: "store_owner".to_owned(),
            mode: "0500".to_owned(),
            links: 1,
            bytes: MAX_LOCAL_INSTALL_BINARY_BYTES + 1,
        };
        let encoded =
            encode_generation_document(&request, &artifact).expect("encode oversized evidence");
        assert_eq!(
            decode_generation_document(&encoded)
                .expect_err("oversized evidence refused")
                .kind(),
            LocalInstallGenerationStoreErrorKind::CorruptState
        );
    }

    #[test]
    fn operation_identity_never_accepts_abbreviation_or_uppercase() {
        for invalid in [
            "sha256:a",
            &format!("sha256:{}", "A".repeat(64)),
            &format!("sha1:{}", "a".repeat(40)),
            &"a".repeat(64),
        ] {
            assert!(LocalInstallStoreOperationId::parse(invalid).is_err());
        }
    }

    #[test]
    fn empty_recovery_is_clean_and_idempotent() {
        let test = TestStore::new("empty-recovery");
        for _ in 0..2 {
            let recovered = test.store.recover().expect("recover empty");
            assert_eq!(
                recovered.disposition(),
                LocalInstallGenerationRecoveryDisposition::Clean
            );
            assert!(recovered.state().state().accepted.is_none());
        }
    }

    #[test]
    fn exact_replay_survives_reopen() {
        let test = TestStore::new("reopen-replay");
        let bytes = b"#!/bin/false\nreopen replay\n";
        let binary = test.binary("candidate", bytes);
        let first = candidate(None, 'a', bytes);
        let first_request = request('1', None, first);
        test.store
            .publish(&first_request, &binary)
            .expect("publish first");
        let reopened = UnixLocalInstallGenerationStore::open_for_test(&test.parent)
            .expect("reopen exact store");
        assert_eq!(
            reopened
                .publish(&first_request, binary)
                .expect("replay after reopen")
                .disposition(),
            LocalInstallGenerationPublishDisposition::Replayed
        );
    }

    #[test]
    fn source_requires_absolute_path() {
        let test = TestStore::new("absolute-source");
        let bytes = b"binary";
        let first = candidate(None, 'a', bytes);
        let first_request = request('1', None, first);
        assert_eq!(
            test.store
                .publish(&first_request, Path::new("relative-glaeda"))
                .expect_err("relative source refused")
                .kind(),
            LocalInstallGenerationStoreErrorKind::InvalidRequest
        );
    }

    #[test]
    #[ignore = "physical local-install control matrix; run explicitly and record the exact head"]
    fn physical_local_install_control_matrix() {
        use std::time::{Duration, Instant};

        const SAMPLES: usize = 20;
        const BINARY_BYTES: usize = 8 * 1024 * 1024;

        fn summary(mut samples: Vec<Duration>) -> serde_json::Value {
            samples.sort_unstable();
            let p90_index = (samples.len() * 9).div_ceil(10).saturating_sub(1);
            serde_json::json!({
                "minimum_ns": samples[0].as_nanos(),
                "p50_ns": samples[samples.len() / 2].as_nanos(),
                "p90_ns": samples[p90_index].as_nanos(),
                "maximum_ns": samples[samples.len() - 1].as_nanos()
            })
        }

        fn fresh_parent(label: &str, index: usize) -> PathBuf {
            let sequence = NEXT_TEST.fetch_add(1, Ordering::Relaxed);
            let parent = std::env::temp_dir().join(format!(
                "glaeda-local-generation-bench-{label}-{}-{sequence}-{index}",
                std::process::id()
            ));
            std_fs::create_dir(&parent).expect("create benchmark parent");
            std_fs::set_permissions(&parent, std_fs::Permissions::from_mode(0o700))
                .expect("set benchmark parent mode");
            parent
        }

        fn sync_directory(path: &Path) {
            File::open(path)
                .expect("open benchmark directory")
                .sync_all()
                .expect("sync benchmark directory");
        }

        fn verify_digest(path: &Path, expected: &Sha256Digest) {
            let bytes = std_fs::read(path).expect("read benchmark artifact");
            assert_eq!(&digest_bytes(&bytes), expected);
        }

        let source_parent = fresh_parent("source", 0);
        let source_path = source_parent.join("glaeda");
        let mut source_bytes = vec![0_u8; BINARY_BYTES];
        for (index, byte) in source_bytes.iter_mut().enumerate() {
            *byte = (index % 251) as u8;
        }
        let mut source = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o500)
            .open(&source_path)
            .expect("create benchmark source");
        source
            .write_all(&source_bytes)
            .expect("write benchmark source");
        source.sync_all().expect("sync benchmark source");
        std_fs::set_permissions(&source_path, std_fs::Permissions::from_mode(0o500))
            .expect("set benchmark source mode");
        sync_directory(&source_parent);
        let digest = digest_bytes(&source_bytes);
        let generation = candidate(None, 'a', &source_bytes);
        let publish = request('1', None, generation);
        drop(source_bytes);

        let mut naive = Vec::new();
        let mut atomic = Vec::new();
        let mut exact_cold = Vec::new();
        for index in 0..SAMPLES {
            let parent = fresh_parent("naive", index);
            let destination = parent.join("glaeda");
            let started = Instant::now();
            std_fs::copy(&source_path, &destination).expect("naive copy");
            std_fs::set_permissions(&destination, std_fs::Permissions::from_mode(0o500))
                .expect("naive mode");
            File::open(&destination)
                .expect("open naive destination")
                .sync_all()
                .expect("sync naive destination");
            sync_directory(&parent);
            verify_digest(&destination, &digest);
            naive.push(started.elapsed());
            std_fs::remove_dir_all(parent).expect("remove naive root");

            let parent = fresh_parent("atomic", index);
            let staged = parent.join("glaeda.next");
            let destination = parent.join("glaeda");
            let started = Instant::now();
            std_fs::copy(&source_path, &staged).expect("atomic staged copy");
            std_fs::set_permissions(&staged, std_fs::Permissions::from_mode(0o500))
                .expect("atomic staged mode");
            File::open(&staged)
                .expect("open atomic stage")
                .sync_all()
                .expect("sync atomic stage");
            sync_directory(&parent);
            std_fs::rename(&staged, &destination).expect("atomic rename");
            sync_directory(&parent);
            verify_digest(&destination, &digest);
            atomic.push(started.elapsed());
            std_fs::remove_dir_all(parent).expect("remove atomic root");

            let parent = fresh_parent("exact-cold", index);
            let started = Instant::now();
            let store = UnixLocalInstallGenerationStore::open_for_test(&parent)
                .expect("open exact benchmark store");
            store
                .publish(&publish, &source_path)
                .expect("publish exact benchmark generation");
            exact_cold.push(started.elapsed());
            drop(store);
            std_fs::remove_dir_all(parent).expect("remove exact cold root");
        }

        let warm_parent = fresh_parent("warm", 0);
        let warm_store = UnixLocalInstallGenerationStore::open_for_test(&warm_parent)
            .expect("open warm benchmark store");
        warm_store
            .publish(&publish, &source_path)
            .expect("seed warm benchmark store");
        let atomic_warm_destination = warm_parent.join("atomic-warm-glaeda");
        std_fs::copy(&source_path, &atomic_warm_destination).expect("seed atomic warm artifact");
        std_fs::set_permissions(
            &atomic_warm_destination,
            std_fs::Permissions::from_mode(0o500),
        )
        .expect("set atomic warm mode");

        let mut digest_gated_warm = Vec::new();
        let mut exact_replay = Vec::new();
        let mut exact_load = Vec::new();
        for _ in 0..SAMPLES {
            let started = Instant::now();
            verify_digest(&source_path, &digest);
            verify_digest(&atomic_warm_destination, &digest);
            digest_gated_warm.push(started.elapsed());

            let started = Instant::now();
            let replay = warm_store
                .publish(&publish, &source_path)
                .expect("exact replay");
            assert_eq!(
                replay.disposition(),
                LocalInstallGenerationPublishDisposition::Replayed
            );
            exact_replay.push(started.elapsed());

            let started = Instant::now();
            warm_store.load().expect("exact load");
            exact_load.push(started.elapsed());
        }

        let report = serde_json::json!({
            "schema_version": 1,
            "samples_per_arm": SAMPLES,
            "binary_bytes": BINARY_BYTES,
            "controls": {
                "naive_copy_fsync_validate": summary(naive),
                "atomic_copy_rename_fsync_validate": summary(atomic),
                "digest_gated_warm_validate": summary(digest_gated_warm)
            },
            "glaeda": {
                "cold_store_open_publish_validate": summary(exact_cold),
                "exact_replay_validate": summary(exact_replay),
                "exact_load_validate": summary(exact_load)
            },
            "semantic_validator": "all accepted artifacts matched the exact source sha256; Glaeda additionally validated canonical source/generation/CAS/operation/store evidence",
            "authority": "observation_only"
        });
        println!("{report}");
        drop(warm_store);
        std_fs::remove_dir_all(warm_parent).expect("remove warm benchmark root");
        std_fs::remove_dir_all(source_parent).expect("remove benchmark source root");
    }
}
