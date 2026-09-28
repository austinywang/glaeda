//! Unix persistence for the protected cache-generation catalog.
//!
//! This module supports empty creation plus a crate-sealed current-generation transition built
//! from an exact loaded revision. It never accepts a caller-supplied catalog for publication and
//! exposes no public generation producer, path adoption, quarantine, or deletion API.

use std::fmt;
use std::fmt::Write as _;
use std::fs::File;
use std::io::{Read as _, Write as _};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use rustix::fs::{self, AtFlags, Dir, FileType, FlockOperation, Mode, OFlags, RenameFlags};
use rustix::io::Errno;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use crate::cache_inventory::CacheStateId;
use crate::protected_cache_generation_catalog::{
    MAX_PROTECTED_CACHE_GENERATION_CATALOG_BYTES, ProtectedCacheCatalogError,
    ProtectedCacheCatalogErrorKind, ProtectedCacheCatalogRevision,
    ProtectedCacheGenerationCatalogDocument, ProtectedCacheGenerationFamily,
    ProtectedCacheGenerationIdentity, ProtectedCacheNamespaceIdentity,
    decode_protected_cache_generation_catalog, encode_protected_cache_generation_catalog,
};
use crate::state::InstallationId;

pub const PROTECTED_CACHE_CATALOG_STORE_SCHEMA_VERSION: u8 = 1;
pub const MAX_PROTECTED_CACHE_CATALOG_STORE_BYTES: usize =
    MAX_PROTECTED_CACHE_GENERATION_CATALOG_BYTES + 4_096;

/// Closed state-root generation accepted by this first store.
///
/// Legacy state cannot be selected by path discovery or directory presence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtectedCacheCatalogStateRootGeneration {
    GlaedaCurrentV1,
}

const STORE_DIRECTORY: &str = "protected-cache-generation-catalog";
const LOCK_FILE: &str = "catalog.lock";
const CATALOG_FILE: &str = "catalog.json";
const STAGE_PREFIX: &str = ".catalog-stage-";
const STAGE_SUFFIX: &str = ".json";
const MAX_STORE_ENTRIES: usize = 4;

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
const INSTALLATION_DIRECTORY_MODE: Mode = Mode::RUSR
    .union(Mode::WUSR)
    .union(Mode::XUSR)
    .union(Mode::RGRP)
    .union(Mode::XGRP);
const STORE_DIRECTORY_MODE: Mode = Mode::RUSR.union(Mode::WUSR).union(Mode::XUSR);
const PRIVATE_FILE_MODE: Mode = Mode::RUSR.union(Mode::WUSR);

static NEXT_STAGE: AtomicU64 = AtomicU64::new(1);

/// Exact path-free identity expected around one protected catalog store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtectedCacheCatalogStoreBinding {
    installation_id: InstallationId,
    state_root_generation: ProtectedCacheCatalogStateRootGeneration,
    namespace_identity: ProtectedCacheNamespaceIdentity,
}

impl ProtectedCacheCatalogStoreBinding {
    #[must_use]
    pub const fn new(
        installation_id: InstallationId,
        state_root_generation: ProtectedCacheCatalogStateRootGeneration,
        namespace_identity: ProtectedCacheNamespaceIdentity,
    ) -> Self {
        Self {
            installation_id,
            state_root_generation,
            namespace_identity,
        }
    }

    #[must_use]
    pub const fn installation_id(&self) -> &InstallationId {
        &self.installation_id
    }

    #[must_use]
    pub const fn state_root_generation(&self) -> ProtectedCacheCatalogStateRootGeneration {
        self.state_root_generation
    }

    #[must_use]
    pub const fn namespace_identity(&self) -> &ProtectedCacheNamespaceIdentity {
        &self.namespace_identity
    }
}

/// Narrow authority of a successfully loaded store envelope.
///
/// This proves only exact private-store persistence. It grants no physical cache ownership,
/// adoption, reconstruction, lease, inventory, quarantine, or deletion authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtectedCacheCatalogStoreAuthority {
    ProtectedStoreSnapshotOnly,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtectedCacheCatalogStoreSnapshot {
    binding: ProtectedCacheCatalogStoreBinding,
    document: ProtectedCacheGenerationCatalogDocument,
}

impl ProtectedCacheCatalogStoreSnapshot {
    #[must_use]
    pub const fn authority(&self) -> ProtectedCacheCatalogStoreAuthority {
        ProtectedCacheCatalogStoreAuthority::ProtectedStoreSnapshotOnly
    }

    #[must_use]
    pub const fn binding(&self) -> &ProtectedCacheCatalogStoreBinding {
        &self.binding
    }

    #[must_use]
    pub const fn document(&self) -> &ProtectedCacheGenerationCatalogDocument {
        &self.document
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtectedCacheCatalogStoreRead {
    Missing,
    Present(ProtectedCacheCatalogStoreSnapshot),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtectedCacheCatalogRecoveryDisposition {
    PublishedAbandonedCreate,
    PublishedAbandonedTransition,
    RemovedDuplicateStage,
}

/// Sealed path-free authorization for one current-generation catalog transition.
///
/// The fields and constructor are intentionally unavailable outside this module. A later
/// replacement-equivalence producer must be separately reviewed before it can mint this type.
#[derive(Debug)]
pub struct ProtectedCacheCurrentTransitionAuthorization {
    expected_revision: ProtectedCacheCatalogRevision,
    state_id: CacheStateId,
    generation_identity: ProtectedCacheGenerationIdentity,
}

/// Descriptor-retained private store for one protected cache-generation catalog.
///
/// `open_or_create` may create only the private store directory and persistent empty lock.
/// `create_empty` atomically publishes the initial fully synchronized envelope. A sealed
/// authorization may later advance currentness by one exact revision. Every operation retains the
/// installation and store descriptors, validates the persistent lock, and refuses staging debt
/// before returning a clean snapshot.
#[derive(Debug)]
pub struct UnixProtectedCacheGenerationCatalogStore {
    installation: OwnedFd,
    directory: OwnedFd,
    lock: OwnedFd,
    owner: (u32, u32),
    binding: ProtectedCacheCatalogStoreBinding,
}

impl UnixProtectedCacheGenerationCatalogStore {
    /// Open one exact installation directory and prepare the private catalog-store boundary.
    ///
    /// The installation must already be a real `0750` directory. This operation creates or
    /// validates only a fixed `0700` store directory and empty `0600` lock file. It never creates a
    /// catalog, discovers a cache, or adopts physical state.
    ///
    /// # Errors
    ///
    /// Returns a bounded fail-closed error for unsafe filesystem shape or I/O failure.
    pub fn open_or_create(
        installation_path: impl AsRef<Path>,
        binding: ProtectedCacheCatalogStoreBinding,
    ) -> Result<Self, ProtectedCacheCatalogStoreError> {
        let installation = fs::open(installation_path.as_ref(), DIRECTORY_FLAGS, Mode::empty())
            .map_err(map_installation_open_error)?;
        let installation_stat = inspect_directory(
            installation.as_fd(),
            "installation directory",
            INSTALLATION_DIRECTORY_MODE,
            None,
        )?;
        let owner = (installation_stat.st_uid, installation_stat.st_gid);
        let directory = ensure_store_directory(&installation, owner)?;
        let lock = ensure_lock_file(&directory, owner)?;
        Ok(Self {
            installation,
            directory,
            lock,
            owner,
            binding,
        })
    }

    /// Load the clean protected catalog snapshot, if present.
    ///
    /// Any abandoned stage blocks the read as recovery-required. Missing state is distinct from a
    /// malformed, unsafe, or binding-mismatched catalog.
    pub fn load(&self) -> Result<ProtectedCacheCatalogStoreRead, ProtectedCacheCatalogStoreError> {
        let _lock = self.acquire_lock(StoreLockMode::Shared)?;
        let entries = self.inspect_entries()?;
        if !entries.stages.is_empty() {
            return Err(store_error(
                ProtectedCacheCatalogStoreErrorKind::RecoveryRequired,
                "protected cache catalog has abandoned publication state",
            ));
        }
        let read = self.load_clean_locked(entries.catalog_present)?;
        if matches!(read, ProtectedCacheCatalogStoreRead::Present(_)) {
            // A prior publisher may have observed a directory-sync failure after the atomic rename.
            // Requiring a successful barrier before returning a snapshot re-establishes durability.
            synchronize_directory(&self.directory, "protected cache catalog directory")?;
        }
        Ok(read)
    }

    /// Atomically publish the only currently supported production document: an empty revision-one
    /// catalog for the exact store namespace.
    ///
    /// # Errors
    ///
    /// Refuses existing catalog state, abandoned stages, unsafe metadata, or a concurrent writer.
    pub fn create_empty(
        &self,
    ) -> Result<ProtectedCacheCatalogStoreSnapshot, ProtectedCacheCatalogStoreError> {
        let _lock = self.acquire_lock(StoreLockMode::Exclusive)?;
        let entries = self.inspect_entries()?;
        if !entries.stages.is_empty() {
            return Err(store_error(
                ProtectedCacheCatalogStoreErrorKind::RecoveryRequired,
                "protected cache catalog has abandoned publication state",
            ));
        }
        if entries.catalog_present {
            return Err(store_error(
                ProtectedCacheCatalogStoreErrorKind::Conflict,
                "protected cache catalog already exists",
            ));
        }

        let document =
            ProtectedCacheGenerationCatalogDocument::empty(self.binding.namespace_identity.clone());
        let encoded = encode_store_envelope(&self.binding, &document)?;
        let mut staged = self.stage_envelope(&encoded)?;
        self.verify_retained_store_path()?;
        self.verify_retained_lock_path()?;
        verify_retained_file_path(
            &self.directory,
            staged.name(),
            staged.file(),
            self.owner,
            "staged protected cache catalog",
        )?;
        match fs::renameat_with(
            &self.directory,
            staged.name(),
            &self.directory,
            CATALOG_FILE,
            RenameFlags::NOREPLACE,
        ) {
            Ok(()) => staged.disarm(),
            Err(Errno::EXIST) => {
                return Err(store_error(
                    ProtectedCacheCatalogStoreErrorKind::Conflict,
                    "protected cache catalog already exists",
                ));
            }
            Err(_) => {
                return Err(store_error(
                    ProtectedCacheCatalogStoreErrorKind::Io,
                    "could not atomically publish the protected cache catalog",
                ));
            }
        }
        synchronize_directory(&self.directory, "protected cache catalog directory")?;
        match self.load_clean_locked(true)? {
            ProtectedCacheCatalogStoreRead::Present(snapshot) => Ok(snapshot),
            ProtectedCacheCatalogStoreRead::Missing => Err(store_error(
                ProtectedCacheCatalogStoreErrorKind::RecoveryRequired,
                "published protected cache catalog disappeared",
            )),
        }
    }

    /// Atomically advance the exact clean catalog revision to one new current generation.
    ///
    /// Its authorization type is sealed until a separately reviewed replacement-equivalence
    /// producer exists. The operation records path-free metadata only; the returned snapshot grants
    /// no physical cache ownership, adoption, reconstruction, lease, inventory, or cleanup
    /// authority.
    pub fn transition_current(
        &self,
        authorization: ProtectedCacheCurrentTransitionAuthorization,
    ) -> Result<ProtectedCacheCatalogStoreSnapshot, ProtectedCacheCatalogStoreError> {
        let _lock = self.acquire_lock(StoreLockMode::Exclusive)?;
        let entries = self.inspect_entries()?;
        if !entries.stages.is_empty() {
            return Err(store_error(
                ProtectedCacheCatalogStoreErrorKind::RecoveryRequired,
                "protected cache catalog has abandoned publication state",
            ));
        }
        if !entries.catalog_present {
            return Err(store_error(
                ProtectedCacheCatalogStoreErrorKind::Missing,
                "protected cache catalog does not exist",
            ));
        }

        let current = self.open_catalog_locked()?;
        let successor = current
            .snapshot
            .document
            .prepare_current_transition(
                authorization.expected_revision,
                authorization.state_id,
                authorization.generation_identity,
            )
            .map_err(map_catalog_transition_error)?;
        let encoded = encode_store_envelope(&self.binding, &successor)?;
        let mut staged = self.stage_envelope(&encoded)?;

        self.verify_retained_store_path()?;
        self.verify_retained_lock_path()?;
        verify_open_catalog_unchanged(self, &current)?;
        verify_retained_file_path(
            &self.directory,
            staged.name(),
            staged.file(),
            self.owner,
            "staged protected cache catalog",
        )?;
        replace_between_directory_barriers(
            || synchronize_directory(&self.directory, "protected cache catalog directory"),
            || {
                fs::renameat_with(
                    &self.directory,
                    staged.name(),
                    &self.directory,
                    CATALOG_FILE,
                    RenameFlags::empty(),
                )
                .map_err(|_| {
                    store_error(
                        ProtectedCacheCatalogStoreErrorKind::Io,
                        "could not atomically replace the protected cache catalog",
                    )
                })
            },
        )?;
        staged.disarm();
        let published = self.open_catalog_locked()?.snapshot;
        if published.document != successor {
            return Err(store_error(
                ProtectedCacheCatalogStoreErrorKind::RecoveryRequired,
                "published protected cache catalog differs from the requested successor",
            ));
        }
        Ok(published)
    }

    /// Resolve exactly one abandoned empty-catalog creation.
    ///
    /// A valid, private, exact-binding stage is synchronized again before publication. If the same
    /// catalog is already final, the duplicate stage is removed. Multiple, malformed, mismatched,
    /// or nonempty stages remain recovery-required and are never guessed away.
    pub fn recover_abandoned_create(
        &self,
    ) -> Result<ProtectedCacheCatalogRecoveryDisposition, ProtectedCacheCatalogStoreError> {
        let _lock = self.acquire_lock(StoreLockMode::Exclusive)?;
        let entries = self.inspect_entries()?;
        let [stage_name] = entries.stages.as_slice() else {
            return Err(store_error(
                if entries.stages.is_empty() {
                    ProtectedCacheCatalogStoreErrorKind::Missing
                } else {
                    ProtectedCacheCatalogStoreErrorKind::RecoveryRequired
                },
                if entries.stages.is_empty() {
                    "protected cache catalog has no abandoned create"
                } else {
                    "protected cache catalog has ambiguous abandoned publication state"
                },
            ));
        };

        let stage = open_private_file(
            &self.directory,
            stage_name,
            self.owner,
            "staged protected cache catalog",
        )?;
        let stage_bytes =
            read_bounded_envelope(&stage, self.owner, "staged protected cache catalog")?;
        let snapshot = decode_store_envelope(&stage_bytes, &self.binding)?;
        require_empty_revision_one(&snapshot.document)?;
        fs::fsync(&stage).map_err(|_| {
            store_error(
                ProtectedCacheCatalogStoreErrorKind::Io,
                "could not synchronize the abandoned protected cache catalog",
            )
        })?;

        self.verify_retained_store_path()?;
        self.verify_retained_lock_path()?;
        verify_retained_file_path(
            &self.directory,
            stage_name,
            &stage,
            self.owner,
            "staged protected cache catalog",
        )?;
        if entries.catalog_present {
            let final_file = open_private_file(
                &self.directory,
                CATALOG_FILE,
                self.owner,
                "protected cache catalog",
            )?;
            let final_bytes =
                read_bounded_envelope(&final_file, self.owner, "protected cache catalog")?;
            verify_retained_file_path(
                &self.directory,
                CATALOG_FILE,
                &final_file,
                self.owner,
                "protected cache catalog",
            )?;
            let final_snapshot = decode_store_envelope(&final_bytes, &self.binding)?;
            require_empty_revision_one(&final_snapshot.document)?;
            if final_bytes != stage_bytes {
                return Err(store_error(
                    ProtectedCacheCatalogStoreErrorKind::RecoveryRequired,
                    "abandoned and published protected cache catalogs disagree",
                ));
            }
            fs::unlinkat(&self.directory, stage_name, AtFlags::empty()).map_err(|_| {
                store_error(
                    ProtectedCacheCatalogStoreErrorKind::Io,
                    "could not remove the duplicate protected cache catalog stage",
                )
            })?;
            synchronize_directory(&self.directory, "protected cache catalog directory")?;
            return Ok(ProtectedCacheCatalogRecoveryDisposition::RemovedDuplicateStage);
        }

        fs::renameat_with(
            &self.directory,
            stage_name,
            &self.directory,
            CATALOG_FILE,
            RenameFlags::NOREPLACE,
        )
        .map_err(|error| {
            store_error(
                if error == Errno::EXIST {
                    ProtectedCacheCatalogStoreErrorKind::RecoveryRequired
                } else {
                    ProtectedCacheCatalogStoreErrorKind::Io
                },
                "could not publish the abandoned protected cache catalog",
            )
        })?;
        synchronize_directory(&self.directory, "protected cache catalog directory")?;
        Ok(ProtectedCacheCatalogRecoveryDisposition::PublishedAbandonedCreate)
    }

    /// Resolve exactly one abandoned current-generation transition.
    ///
    /// The stage must either be byte-identical to the published catalog or be the one exact
    /// revision successor of the retained final catalog. Missing finals, skipped revisions,
    /// altered prior entries, malformed stages, and ambiguous stages remain recovery-required.
    pub fn recover_abandoned_transition(
        &self,
    ) -> Result<ProtectedCacheCatalogRecoveryDisposition, ProtectedCacheCatalogStoreError> {
        let _lock = self.acquire_lock(StoreLockMode::Exclusive)?;
        let entries = self.inspect_entries()?;
        let [stage_name] = entries.stages.as_slice() else {
            return Err(store_error(
                if entries.stages.is_empty() {
                    ProtectedCacheCatalogStoreErrorKind::Missing
                } else {
                    ProtectedCacheCatalogStoreErrorKind::RecoveryRequired
                },
                if entries.stages.is_empty() {
                    "protected cache catalog has no abandoned transition"
                } else {
                    "protected cache catalog has ambiguous abandoned publication state"
                },
            ));
        };
        if !entries.catalog_present {
            return Err(store_error(
                ProtectedCacheCatalogStoreErrorKind::RecoveryRequired,
                "abandoned protected cache catalog transition has no predecessor",
            ));
        }

        let stage = open_private_file(
            &self.directory,
            stage_name,
            self.owner,
            "staged protected cache catalog",
        )?;
        let stage_bytes =
            read_bounded_envelope(&stage, self.owner, "staged protected cache catalog")?;
        let stage_snapshot = decode_store_envelope(&stage_bytes, &self.binding)?;
        fs::fsync(&stage).map_err(|_| {
            store_error(
                ProtectedCacheCatalogStoreErrorKind::Io,
                "could not synchronize the abandoned protected cache catalog transition",
            )
        })?;
        let stage_stat = inspect_private_file(
            &stage,
            self.owner,
            "staged protected cache catalog",
            Some(stage_bytes.len()),
        )?;
        let current = self.open_catalog_locked()?;

        self.verify_retained_store_path()?;
        self.verify_retained_lock_path()?;
        verify_open_catalog_unchanged(self, &current)?;
        verify_retained_file_path(
            &self.directory,
            stage_name,
            &stage,
            self.owner,
            "staged protected cache catalog",
        )?;
        let current_stage_stat = inspect_private_file(
            &stage,
            self.owner,
            "staged protected cache catalog",
            Some(stage_bytes.len()),
        )?;
        if !same_file_snapshot(&stage_stat, &current_stage_stat) {
            return Err(store_error(
                ProtectedCacheCatalogStoreErrorKind::RecoveryRequired,
                "staged protected cache catalog changed during recovery",
            ));
        }

        if stage_bytes == current.bytes {
            fs::unlinkat(&self.directory, stage_name, AtFlags::empty()).map_err(|_| {
                store_error(
                    ProtectedCacheCatalogStoreErrorKind::Io,
                    "could not remove the duplicate protected cache catalog stage",
                )
            })?;
            synchronize_directory(&self.directory, "protected cache catalog directory")?;
            return Ok(ProtectedCacheCatalogRecoveryDisposition::RemovedDuplicateStage);
        }

        stage_snapshot
            .document
            .require_exact_current_successor(&current.snapshot.document)
            .map_err(|_| {
                store_error(
                    ProtectedCacheCatalogStoreErrorKind::RecoveryRequired,
                    "abandoned protected cache catalog is not an exact revision successor",
                )
            })?;
        replace_between_directory_barriers(
            || synchronize_directory(&self.directory, "protected cache catalog directory"),
            || {
                fs::renameat_with(
                    &self.directory,
                    stage_name,
                    &self.directory,
                    CATALOG_FILE,
                    RenameFlags::empty(),
                )
                .map_err(|_| {
                    store_error(
                        ProtectedCacheCatalogStoreErrorKind::Io,
                        "could not publish the abandoned protected cache catalog transition",
                    )
                })
            },
        )?;
        Ok(ProtectedCacheCatalogRecoveryDisposition::PublishedAbandonedTransition)
    }

    fn acquire_lock(
        &self,
        mode: StoreLockMode,
    ) -> Result<StoreLock, ProtectedCacheCatalogStoreError> {
        let retained_stat = inspect_private_file(
            &self.lock,
            self.owner,
            "protected cache catalog lock",
            Some(0),
        )?;
        // `flock` authority belongs to an open-file description. Open a fresh descriptor for
        // every operation so concurrent calls through one shared store cannot convert or release
        // each other's lock.
        let current = fs::openat(
            &self.directory,
            LOCK_FILE,
            EXISTING_LOCK_FLAGS,
            Mode::empty(),
        )
        .map_err(map_lock_open_error)?;
        let current_stat = inspect_private_file(
            &current,
            self.owner,
            "protected cache catalog lock",
            Some(0),
        )?;
        if retained_stat.st_dev != current_stat.st_dev
            || retained_stat.st_ino != current_stat.st_ino
        {
            return Err(store_error(
                ProtectedCacheCatalogStoreErrorKind::UnsafeFilesystem,
                "protected cache catalog lock identity changed",
            ));
        }
        let operation = match mode {
            StoreLockMode::Shared => FlockOperation::NonBlockingLockShared,
            StoreLockMode::Exclusive => FlockOperation::NonBlockingLockExclusive,
        };
        match fs::flock(&current, operation) {
            Ok(()) => {
                let guard = StoreLock { lock: current };
                self.verify_retained_store_path()?;
                self.verify_retained_lock_path()?;
                Ok(guard)
            }
            Err(Errno::AGAIN) => Err(store_error(
                ProtectedCacheCatalogStoreErrorKind::Busy,
                "another protected cache catalog operation holds the lock",
            )),
            Err(_) => Err(store_error(
                ProtectedCacheCatalogStoreErrorKind::Io,
                "could not acquire the protected cache catalog lock",
            )),
        }
    }

    fn verify_retained_store_path(&self) -> Result<(), ProtectedCacheCatalogStoreError> {
        let current = fs::openat(
            &self.installation,
            STORE_DIRECTORY,
            DIRECTORY_FLAGS,
            Mode::empty(),
        )
        .map_err(map_store_directory_open_error)?;
        let retained_stat = inspect_directory(
            self.directory.as_fd(),
            "protected cache catalog directory",
            STORE_DIRECTORY_MODE,
            Some(self.owner),
        )?;
        let current_stat = inspect_directory(
            current.as_fd(),
            "protected cache catalog directory",
            STORE_DIRECTORY_MODE,
            Some(self.owner),
        )?;
        if retained_stat.st_dev != current_stat.st_dev
            || retained_stat.st_ino != current_stat.st_ino
        {
            return Err(store_error(
                ProtectedCacheCatalogStoreErrorKind::UnsafeFilesystem,
                "protected cache catalog directory identity changed",
            ));
        }
        Ok(())
    }

    fn verify_retained_lock_path(&self) -> Result<(), ProtectedCacheCatalogStoreError> {
        let current = fs::openat(
            &self.directory,
            LOCK_FILE,
            EXISTING_LOCK_FLAGS,
            Mode::empty(),
        )
        .map_err(map_lock_open_error)?;
        inspect_private_file(
            &current,
            self.owner,
            "protected cache catalog lock",
            Some(0),
        )?;
        let retained_stat = fs::fstat(&self.lock).map_err(|_| {
            store_error(
                ProtectedCacheCatalogStoreErrorKind::Io,
                "could not inspect the retained protected cache catalog lock",
            )
        })?;
        let current_stat = fs::fstat(&current).map_err(|_| {
            store_error(
                ProtectedCacheCatalogStoreErrorKind::Io,
                "could not inspect the current protected cache catalog lock",
            )
        })?;
        if retained_stat.st_dev != current_stat.st_dev
            || retained_stat.st_ino != current_stat.st_ino
            || retained_stat.st_nlink != 1
        {
            return Err(store_error(
                ProtectedCacheCatalogStoreErrorKind::UnsafeFilesystem,
                "protected cache catalog lock identity changed",
            ));
        }
        Ok(())
    }

    fn inspect_entries(&self) -> Result<StoreEntries, ProtectedCacheCatalogStoreError> {
        inspect_directory(
            self.directory.as_fd(),
            "protected cache catalog directory",
            STORE_DIRECTORY_MODE,
            Some(self.owner),
        )?;
        let mut entries = Dir::read_from(&self.directory).map_err(|_| {
            store_error(
                ProtectedCacheCatalogStoreErrorKind::Io,
                "could not enumerate the protected cache catalog directory",
            )
        })?;
        let mut observed = 0_usize;
        let mut lock_present = false;
        let mut catalog_present = false;
        let mut stages = Vec::new();
        for entry in &mut entries {
            let entry = entry.map_err(|_| {
                store_error(
                    ProtectedCacheCatalogStoreErrorKind::Io,
                    "could not read a protected cache catalog directory entry",
                )
            })?;
            let bytes = entry.file_name().to_bytes();
            if bytes == b"." || bytes == b".." {
                continue;
            }
            observed += 1;
            if observed > MAX_STORE_ENTRIES {
                return Err(store_error(
                    ProtectedCacheCatalogStoreErrorKind::CorruptState,
                    "protected cache catalog directory contains too many entries",
                ));
            }
            if bytes == LOCK_FILE.as_bytes() {
                lock_present = true;
            } else if bytes == CATALOG_FILE.as_bytes() {
                catalog_present = true;
            } else if is_canonical_stage_name(bytes) {
                stages.push(
                    std::str::from_utf8(bytes)
                        .expect("ASCII stage name")
                        .to_owned(),
                );
            } else {
                return Err(store_error(
                    ProtectedCacheCatalogStoreErrorKind::CorruptState,
                    "protected cache catalog directory contains an unexpected entry",
                ));
            }
        }
        if !lock_present {
            return Err(store_error(
                ProtectedCacheCatalogStoreErrorKind::UnsafeFilesystem,
                "protected cache catalog lock disappeared",
            ));
        }
        stages.sort();
        Ok(StoreEntries {
            catalog_present,
            stages,
        })
    }

    fn load_clean_locked(
        &self,
        catalog_present: bool,
    ) -> Result<ProtectedCacheCatalogStoreRead, ProtectedCacheCatalogStoreError> {
        if !catalog_present {
            return Ok(ProtectedCacheCatalogStoreRead::Missing);
        }
        self.open_catalog_locked()
            .map(|catalog| ProtectedCacheCatalogStoreRead::Present(catalog.snapshot))
    }

    fn open_catalog_locked(&self) -> Result<OpenCatalog, ProtectedCacheCatalogStoreError> {
        let file = open_private_file(
            &self.directory,
            CATALOG_FILE,
            self.owner,
            "protected cache catalog",
        )?;
        let bytes = read_bounded_envelope(&file, self.owner, "protected cache catalog")?;
        verify_retained_file_path(
            &self.directory,
            CATALOG_FILE,
            &file,
            self.owner,
            "protected cache catalog",
        )?;
        let stat = inspect_private_file(
            &file,
            self.owner,
            "protected cache catalog",
            Some(bytes.len()),
        )?;
        let snapshot = decode_store_envelope(&bytes, &self.binding)?;
        Ok(OpenCatalog {
            file,
            bytes,
            stat,
            snapshot,
        })
    }

    fn stage_envelope<'a>(
        &'a self,
        encoded: &[u8],
    ) -> Result<StagedEnvelope<'a>, ProtectedCacheCatalogStoreError> {
        let name = stage_file_name();
        let file = fs::openat(&self.directory, &name, NEW_FILE_FLAGS, PRIVATE_FILE_MODE)
            .map_err(map_stage_create_error)?;
        let staged = StagedEnvelope {
            parent: self.directory.as_fd(),
            file,
            name,
            armed: true,
        };
        fs::fchmod(&staged.file, PRIVATE_FILE_MODE).map_err(|_| {
            store_error(
                ProtectedCacheCatalogStoreErrorKind::Io,
                "could not set staged protected cache catalog permissions",
            )
        })?;
        inspect_private_file(
            &staged.file,
            self.owner,
            "staged protected cache catalog",
            Some(0),
        )?;
        let duplicated = rustix::io::dup(&staged.file).map_err(|_| {
            store_error(
                ProtectedCacheCatalogStoreErrorKind::Io,
                "could not retain the staged protected cache catalog for writing",
            )
        })?;
        let mut file = File::from(duplicated);
        file.write_all(encoded).map_err(|_| {
            store_error(
                ProtectedCacheCatalogStoreErrorKind::Io,
                "could not write the staged protected cache catalog",
            )
        })?;
        file.sync_all().map_err(|_| {
            store_error(
                ProtectedCacheCatalogStoreErrorKind::Io,
                "could not synchronize the staged protected cache catalog",
            )
        })?;
        inspect_private_file(
            file.as_fd(),
            self.owner,
            "staged protected cache catalog",
            Some(encoded.len()),
        )?;
        Ok(staged)
    }
}

#[derive(Debug, Clone, Copy)]
enum StoreLockMode {
    Shared,
    Exclusive,
}

#[derive(Debug)]
struct StoreLock {
    lock: OwnedFd,
}

impl Drop for StoreLock {
    fn drop(&mut self) {
        // Explicit unlock prevents a duplicate inherited across fork from retaining authority.
        let _ = fs::flock(&self.lock, FlockOperation::Unlock);
    }
}

#[derive(Debug)]
struct StoreEntries {
    catalog_present: bool,
    stages: Vec<String>,
}

struct OpenCatalog {
    file: OwnedFd,
    bytes: Vec<u8>,
    stat: rustix::fs::Stat,
    snapshot: ProtectedCacheCatalogStoreSnapshot,
}

struct StagedEnvelope<'a> {
    parent: BorrowedFd<'a>,
    file: OwnedFd,
    name: String,
    armed: bool,
}

impl StagedEnvelope<'_> {
    fn name(&self) -> &str {
        &self.name
    }

    fn file(&self) -> &OwnedFd {
        &self.file
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for StagedEnvelope<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        // Never unlink a replacement that appeared at the stage name after this guard opened it.
        if let Ok(current) = fs::openat(self.parent, &self.name, EXISTING_FILE_FLAGS, Mode::empty())
            && let (Ok(retained), Ok(current)) = (fs::fstat(&self.file), fs::fstat(&current))
            && retained.st_dev == current.st_dev
            && retained.st_ino == current.st_ino
        {
            let _ = fs::unlinkat(self.parent, &self.name, AtFlags::empty());
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum StateRootGenerationWire {
    GlaedaCurrentV1,
}

#[derive(Deserialize)]
struct EnvelopeVersionWire {
    store_schema_version: u8,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvelopeDecodeWire<'a> {
    store_schema_version: u8,
    installation_id: String,
    state_root_generation: StateRootGenerationWire,
    namespace_identity: String,
    #[serde(borrow)]
    catalog: &'a RawValue,
}

#[derive(Serialize)]
struct EnvelopeEncodeWire<'a> {
    store_schema_version: u8,
    installation_id: &'a str,
    state_root_generation: StateRootGenerationWire,
    namespace_identity: &'a str,
    catalog: &'a RawValue,
}

fn encode_store_envelope(
    binding: &ProtectedCacheCatalogStoreBinding,
    document: &ProtectedCacheGenerationCatalogDocument,
) -> Result<Vec<u8>, ProtectedCacheCatalogStoreError> {
    if document.namespace_identity() != binding.namespace_identity() {
        return Err(store_error(
            ProtectedCacheCatalogStoreErrorKind::Conflict,
            "protected cache catalog namespace differs from its store binding",
        ));
    }
    let catalog_bytes = encode_protected_cache_generation_catalog(document).map_err(|_| {
        store_error(
            ProtectedCacheCatalogStoreErrorKind::CorruptState,
            "protected cache catalog cannot be encoded for private-store publication",
        )
    })?;
    let catalog = RawValue::from_string(
        String::from_utf8(catalog_bytes).expect("catalog encoding is UTF-8 JSON"),
    )
    .map_err(|_| {
        store_error(
            ProtectedCacheCatalogStoreErrorKind::CorruptState,
            "protected cache catalog encoding is invalid",
        )
    })?;
    let wire = EnvelopeEncodeWire {
        store_schema_version: PROTECTED_CACHE_CATALOG_STORE_SCHEMA_VERSION,
        installation_id: binding.installation_id.as_str(),
        state_root_generation: StateRootGenerationWire::GlaedaCurrentV1,
        namespace_identity: binding.namespace_identity.as_str(),
        catalog: &catalog,
    };
    let bytes = serde_json::to_vec(&wire).map_err(|_| {
        store_error(
            ProtectedCacheCatalogStoreErrorKind::CorruptState,
            "protected cache catalog store envelope cannot encode",
        )
    })?;
    if bytes.len() > MAX_PROTECTED_CACHE_CATALOG_STORE_BYTES {
        return Err(store_error(
            ProtectedCacheCatalogStoreErrorKind::CorruptState,
            "protected cache catalog store envelope exceeds the byte limit",
        ));
    }
    Ok(bytes)
}

fn decode_store_envelope(
    bytes: &[u8],
    expected: &ProtectedCacheCatalogStoreBinding,
) -> Result<ProtectedCacheCatalogStoreSnapshot, ProtectedCacheCatalogStoreError> {
    if bytes.len() > MAX_PROTECTED_CACHE_CATALOG_STORE_BYTES {
        return Err(store_error(
            ProtectedCacheCatalogStoreErrorKind::CorruptState,
            "protected cache catalog store envelope exceeds the byte limit",
        ));
    }
    let version: EnvelopeVersionWire = serde_json::from_slice(bytes).map_err(|_| {
        store_error(
            ProtectedCacheCatalogStoreErrorKind::CorruptState,
            "protected cache catalog store envelope is invalid",
        )
    })?;
    if version.store_schema_version != PROTECTED_CACHE_CATALOG_STORE_SCHEMA_VERSION {
        return Err(store_error(
            ProtectedCacheCatalogStoreErrorKind::VersionIncompatible,
            "protected cache catalog store schema version is unsupported",
        ));
    }
    let wire: EnvelopeDecodeWire<'_> = serde_json::from_slice(bytes).map_err(|_| {
        store_error(
            ProtectedCacheCatalogStoreErrorKind::CorruptState,
            "protected cache catalog store envelope is invalid",
        )
    })?;
    if wire.store_schema_version != PROTECTED_CACHE_CATALOG_STORE_SCHEMA_VERSION {
        return Err(store_error(
            ProtectedCacheCatalogStoreErrorKind::VersionIncompatible,
            "protected cache catalog store schema version is unsupported",
        ));
    }
    let installation_id = InstallationId::parse(&wire.installation_id).map_err(|_| {
        store_error(
            ProtectedCacheCatalogStoreErrorKind::CorruptState,
            "protected cache catalog store installation identity is invalid",
        )
    })?;
    let namespace_identity = ProtectedCacheNamespaceIdentity::parse(&wire.namespace_identity)
        .map_err(|_| {
            store_error(
                ProtectedCacheCatalogStoreErrorKind::CorruptState,
                "protected cache catalog store namespace identity is invalid",
            )
        })?;
    let binding = ProtectedCacheCatalogStoreBinding {
        installation_id,
        state_root_generation: match wire.state_root_generation {
            StateRootGenerationWire::GlaedaCurrentV1 => {
                ProtectedCacheCatalogStateRootGeneration::GlaedaCurrentV1
            }
        },
        namespace_identity,
    };
    if &binding != expected {
        return Err(store_error(
            ProtectedCacheCatalogStoreErrorKind::Conflict,
            "protected cache catalog store binding differs from the expected identity",
        ));
    }
    let document = decode_protected_cache_generation_catalog(wire.catalog.get().as_bytes())
        .map_err(|_| {
            store_error(
                ProtectedCacheCatalogStoreErrorKind::CorruptState,
                "protected cache catalog document is invalid",
            )
        })?;
    if document.family() != ProtectedCacheGenerationFamily::CargoTargetV1
        || document.namespace_identity() != expected.namespace_identity()
    {
        return Err(store_error(
            ProtectedCacheCatalogStoreErrorKind::Conflict,
            "protected cache catalog document differs from its store binding",
        ));
    }
    let snapshot = ProtectedCacheCatalogStoreSnapshot { binding, document };
    if encode_store_envelope(&snapshot.binding, &snapshot.document)? != bytes {
        return Err(store_error(
            ProtectedCacheCatalogStoreErrorKind::NonCanonical,
            "protected cache catalog store envelope is not canonical",
        ));
    }
    Ok(snapshot)
}

fn require_empty_revision_one(
    document: &ProtectedCacheGenerationCatalogDocument,
) -> Result<(), ProtectedCacheCatalogStoreError> {
    if document.revision() != ProtectedCacheCatalogRevision::new(1).expect("revision one")
        || document.observed_current_state_id().is_some()
        || !document.generations().is_empty()
        || document.recovery().is_required()
    {
        return Err(store_error(
            ProtectedCacheCatalogStoreErrorKind::RecoveryRequired,
            "abandoned protected cache catalog is not an empty revision-one create",
        ));
    }
    Ok(())
}

fn verify_open_catalog_unchanged(
    store: &UnixProtectedCacheGenerationCatalogStore,
    catalog: &OpenCatalog,
) -> Result<(), ProtectedCacheCatalogStoreError> {
    verify_retained_file_path(
        &store.directory,
        CATALOG_FILE,
        &catalog.file,
        store.owner,
        "protected cache catalog",
    )?;
    let current = inspect_private_file(
        &catalog.file,
        store.owner,
        "protected cache catalog",
        Some(catalog.bytes.len()),
    )?;
    if !same_file_snapshot(&catalog.stat, &current) {
        return Err(store_error(
            ProtectedCacheCatalogStoreErrorKind::RecoveryRequired,
            "protected cache catalog changed during the transition",
        ));
    }
    Ok(())
}

fn ensure_store_directory(
    installation: &OwnedFd,
    owner: (u32, u32),
) -> Result<OwnedFd, ProtectedCacheCatalogStoreError> {
    match fs::openat(
        installation,
        STORE_DIRECTORY,
        DIRECTORY_FLAGS,
        Mode::empty(),
    ) {
        Ok(directory) => {
            inspect_directory(
                directory.as_fd(),
                "protected cache catalog directory",
                STORE_DIRECTORY_MODE,
                Some(owner),
            )?;
            Ok(directory)
        }
        Err(Errno::NOENT) => {
            let created = match fs::mkdirat(installation, STORE_DIRECTORY, STORE_DIRECTORY_MODE) {
                Ok(()) => true,
                Err(Errno::EXIST) => false,
                Err(_) => {
                    return Err(store_error(
                        ProtectedCacheCatalogStoreErrorKind::Io,
                        "could not create the protected cache catalog directory",
                    ));
                }
            };
            let directory = fs::openat(
                installation,
                STORE_DIRECTORY,
                DIRECTORY_FLAGS,
                Mode::empty(),
            )
            .map_err(map_store_directory_open_error)?;
            if created {
                fs::fchmod(&directory, STORE_DIRECTORY_MODE).map_err(|_| {
                    store_error(
                        ProtectedCacheCatalogStoreErrorKind::Io,
                        "could not set protected cache catalog directory permissions",
                    )
                })?;
            }
            inspect_directory(
                directory.as_fd(),
                "protected cache catalog directory",
                STORE_DIRECTORY_MODE,
                Some(owner),
            )?;
            if created {
                synchronize_directory(installation, "installation directory")?;
            }
            Ok(directory)
        }
        Err(error) => Err(map_store_directory_open_error(error)),
    }
}

fn ensure_lock_file(
    directory: &OwnedFd,
    owner: (u32, u32),
) -> Result<OwnedFd, ProtectedCacheCatalogStoreError> {
    match fs::openat(directory, LOCK_FILE, NEW_LOCK_FLAGS, PRIVATE_FILE_MODE) {
        Ok(lock) => {
            fs::fchmod(&lock, PRIVATE_FILE_MODE).map_err(|_| {
                store_error(
                    ProtectedCacheCatalogStoreErrorKind::Io,
                    "could not set protected cache catalog lock permissions",
                )
            })?;
            inspect_private_file(&lock, owner, "protected cache catalog lock", Some(0))?;
            fs::fsync(&lock).map_err(|_| {
                store_error(
                    ProtectedCacheCatalogStoreErrorKind::Io,
                    "could not synchronize the protected cache catalog lock",
                )
            })?;
            synchronize_directory(directory, "protected cache catalog directory")?;
            Ok(lock)
        }
        Err(Errno::EXIST) => {
            let lock = fs::openat(directory, LOCK_FILE, EXISTING_LOCK_FLAGS, Mode::empty())
                .map_err(map_lock_open_error)?;
            inspect_private_file(&lock, owner, "protected cache catalog lock", Some(0))?;
            Ok(lock)
        }
        Err(error) => Err(map_lock_open_error(error)),
    }
}

fn inspect_directory(
    directory: BorrowedFd<'_>,
    subject: &str,
    expected_mode: Mode,
    expected_owner: Option<(u32, u32)>,
) -> Result<rustix::fs::Stat, ProtectedCacheCatalogStoreError> {
    let stat = fs::fstat(directory).map_err(|_| {
        store_error(
            ProtectedCacheCatalogStoreErrorKind::Io,
            format!("could not inspect {subject}"),
        )
    })?;
    if !FileType::from_raw_mode(stat.st_mode).is_dir() {
        return Err(store_error(
            ProtectedCacheCatalogStoreErrorKind::UnsafeFilesystem,
            format!("{subject} is not a directory"),
        ));
    }
    if Mode::from_raw_mode(stat.st_mode) != expected_mode {
        return Err(store_error(
            ProtectedCacheCatalogStoreErrorKind::UnsafeFilesystem,
            format!("{subject} has unsafe permissions"),
        ));
    }
    if expected_owner.is_some_and(|owner| owner != (stat.st_uid, stat.st_gid)) {
        return Err(store_error(
            ProtectedCacheCatalogStoreErrorKind::UnsafeFilesystem,
            format!("{subject} has an unexpected owner or group"),
        ));
    }
    Ok(stat)
}

fn open_private_file(
    directory: &OwnedFd,
    name: &str,
    owner: (u32, u32),
    subject: &str,
) -> Result<OwnedFd, ProtectedCacheCatalogStoreError> {
    let file = fs::openat(directory, name, EXISTING_FILE_FLAGS, Mode::empty()).map_err(
        |error| match error {
            Errno::LOOP | Errno::NOTDIR => store_error(
                ProtectedCacheCatalogStoreErrorKind::UnsafeFilesystem,
                format!("{subject} is symlinked or invalid"),
            ),
            Errno::NOENT => store_error(
                ProtectedCacheCatalogStoreErrorKind::RecoveryRequired,
                format!("{subject} changed during inspection"),
            ),
            _ => store_error(
                ProtectedCacheCatalogStoreErrorKind::Io,
                format!("could not open {subject}"),
            ),
        },
    )?;
    inspect_private_file(&file, owner, subject, None)?;
    Ok(file)
}

fn verify_retained_file_path(
    directory: &OwnedFd,
    name: &str,
    retained: &OwnedFd,
    owner: (u32, u32),
    subject: &str,
) -> Result<(), ProtectedCacheCatalogStoreError> {
    let current = open_private_file(directory, name, owner, subject)?;
    let retained_stat = inspect_private_file(retained, owner, subject, None)?;
    let current_stat = inspect_private_file(&current, owner, subject, None)?;
    if retained_stat.st_dev != current_stat.st_dev || retained_stat.st_ino != current_stat.st_ino {
        return Err(store_error(
            ProtectedCacheCatalogStoreErrorKind::UnsafeFilesystem,
            format!("{subject} identity changed"),
        ));
    }
    Ok(())
}

fn inspect_private_file(
    file: impl AsFd,
    owner: (u32, u32),
    subject: &str,
    expected_size: Option<usize>,
) -> Result<rustix::fs::Stat, ProtectedCacheCatalogStoreError> {
    let stat = fs::fstat(file.as_fd()).map_err(|_| {
        store_error(
            ProtectedCacheCatalogStoreErrorKind::Io,
            format!("could not inspect {subject}"),
        )
    })?;
    if !FileType::from_raw_mode(stat.st_mode).is_file() || stat.st_nlink != 1 {
        return Err(store_error(
            ProtectedCacheCatalogStoreErrorKind::UnsafeFilesystem,
            format!("{subject} is not a private single-link regular file"),
        ));
    }
    if stat.st_mode & 0o7777 != PRIVATE_FILE_MODE.as_raw_mode() {
        return Err(store_error(
            ProtectedCacheCatalogStoreErrorKind::UnsafeFilesystem,
            format!("{subject} has unsafe permissions"),
        ));
    }
    if owner != (stat.st_uid, stat.st_gid) {
        return Err(store_error(
            ProtectedCacheCatalogStoreErrorKind::UnsafeFilesystem,
            format!("{subject} has an unexpected owner or group"),
        ));
    }
    if expected_size.is_some_and(|size| stat.st_size < 0 || stat.st_size as usize != size) {
        return Err(store_error(
            ProtectedCacheCatalogStoreErrorKind::CorruptState,
            format!("{subject} has an unexpected size"),
        ));
    }
    Ok(stat)
}

fn read_bounded_envelope(
    file: &OwnedFd,
    owner: (u32, u32),
    subject: &str,
) -> Result<Vec<u8>, ProtectedCacheCatalogStoreError> {
    let before = inspect_private_file(file, owner, subject, None)?;
    if before.st_size < 0 || before.st_size as u64 > MAX_PROTECTED_CACHE_CATALOG_STORE_BYTES as u64
    {
        return Err(store_error(
            ProtectedCacheCatalogStoreErrorKind::CorruptState,
            format!("{subject} exceeds the byte limit"),
        ));
    }
    let duplicated = rustix::io::dup(file).map_err(|_| {
        store_error(
            ProtectedCacheCatalogStoreErrorKind::Io,
            format!("could not retain {subject} for reading"),
        )
    })?;
    let mut bytes = Vec::new();
    File::from(duplicated)
        .take((MAX_PROTECTED_CACHE_CATALOG_STORE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| {
            store_error(
                ProtectedCacheCatalogStoreErrorKind::Io,
                format!("could not read {subject}"),
            )
        })?;
    if bytes.len() > MAX_PROTECTED_CACHE_CATALOG_STORE_BYTES {
        return Err(store_error(
            ProtectedCacheCatalogStoreErrorKind::CorruptState,
            format!("{subject} exceeds the byte limit"),
        ));
    }
    let after = inspect_private_file(file, owner, subject, None)?;
    if !same_file_snapshot(&before, &after) || after.st_size as usize != bytes.len() {
        return Err(store_error(
            ProtectedCacheCatalogStoreErrorKind::RecoveryRequired,
            format!("{subject} changed while it was read"),
        ));
    }
    Ok(bytes)
}

fn same_file_snapshot(left: &rustix::fs::Stat, right: &rustix::fs::Stat) -> bool {
    left.st_dev == right.st_dev
        && left.st_ino == right.st_ino
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

fn synchronize_directory(
    directory: impl AsFd,
    subject: &str,
) -> Result<(), ProtectedCacheCatalogStoreError> {
    fs::fsync(directory.as_fd()).map_err(|_| {
        store_error(
            ProtectedCacheCatalogStoreErrorKind::Io,
            format!("could not synchronize {subject}"),
        )
    })
}

fn replace_between_directory_barriers<E>(
    mut synchronize: impl FnMut() -> Result<(), E>,
    replace: impl FnOnce() -> Result<(), E>,
) -> Result<(), E> {
    synchronize()?;
    replace()?;
    synchronize()
}

fn stage_file_name() -> String {
    let sequence = NEXT_STAGE.fetch_add(1, Ordering::Relaxed);
    let mut name = String::new();
    write!(
        &mut name,
        "{STAGE_PREFIX}{}-{sequence}{STAGE_SUFFIX}",
        std::process::id()
    )
    .expect("write to string");
    name
}

fn is_canonical_stage_name(bytes: &[u8]) -> bool {
    let Some(middle) = bytes
        .strip_prefix(STAGE_PREFIX.as_bytes())
        .and_then(|value| value.strip_suffix(STAGE_SUFFIX.as_bytes()))
    else {
        return false;
    };
    let mut parts = middle.split(|byte| *byte == b'-');
    let Some(pid) = parts.next() else {
        return false;
    };
    let Some(sequence) = parts.next() else {
        return false;
    };
    parts.next().is_none()
        && !pid.is_empty()
        && !sequence.is_empty()
        && pid.iter().all(u8::is_ascii_digit)
        && sequence.iter().all(u8::is_ascii_digit)
}

fn map_installation_open_error(error: Errno) -> ProtectedCacheCatalogStoreError {
    match error {
        Errno::LOOP | Errno::NOTDIR => store_error(
            ProtectedCacheCatalogStoreErrorKind::UnsafeFilesystem,
            "installation directory is symlinked or invalid",
        ),
        Errno::NOENT => store_error(
            ProtectedCacheCatalogStoreErrorKind::Missing,
            "installation directory does not exist",
        ),
        _ => store_error(
            ProtectedCacheCatalogStoreErrorKind::Io,
            "could not open the installation directory",
        ),
    }
}

fn map_store_directory_open_error(error: Errno) -> ProtectedCacheCatalogStoreError {
    match error {
        Errno::LOOP | Errno::NOTDIR => store_error(
            ProtectedCacheCatalogStoreErrorKind::UnsafeFilesystem,
            "protected cache catalog directory is symlinked or invalid",
        ),
        _ => store_error(
            ProtectedCacheCatalogStoreErrorKind::Io,
            "could not open the protected cache catalog directory",
        ),
    }
}

fn map_lock_open_error(error: Errno) -> ProtectedCacheCatalogStoreError {
    match error {
        Errno::LOOP | Errno::NOTDIR => store_error(
            ProtectedCacheCatalogStoreErrorKind::UnsafeFilesystem,
            "protected cache catalog lock is symlinked or invalid",
        ),
        Errno::NOENT => store_error(
            ProtectedCacheCatalogStoreErrorKind::UnsafeFilesystem,
            "protected cache catalog lock is missing",
        ),
        _ => store_error(
            ProtectedCacheCatalogStoreErrorKind::Io,
            "could not open the protected cache catalog lock",
        ),
    }
}

fn map_stage_create_error(error: Errno) -> ProtectedCacheCatalogStoreError {
    match error {
        Errno::EXIST => store_error(
            ProtectedCacheCatalogStoreErrorKind::RecoveryRequired,
            "protected cache catalog stage identity already exists",
        ),
        Errno::LOOP | Errno::NOTDIR => store_error(
            ProtectedCacheCatalogStoreErrorKind::UnsafeFilesystem,
            "protected cache catalog stage path is unsafe",
        ),
        _ => store_error(
            ProtectedCacheCatalogStoreErrorKind::Io,
            "could not create the protected cache catalog stage",
        ),
    }
}

fn map_catalog_transition_error(
    error: ProtectedCacheCatalogError,
) -> ProtectedCacheCatalogStoreError {
    match error.kind() {
        ProtectedCacheCatalogErrorKind::RevisionConflict => store_error(
            ProtectedCacheCatalogStoreErrorKind::Conflict,
            "protected cache catalog transition revision conflicts with the stored snapshot",
        ),
        ProtectedCacheCatalogErrorKind::RecoveryRequired => store_error(
            ProtectedCacheCatalogStoreErrorKind::RecoveryRequired,
            "protected cache catalog recovery is required before transition",
        ),
        _ => store_error(
            ProtectedCacheCatalogStoreErrorKind::Conflict,
            "protected cache catalog transition is invalid for the stored snapshot",
        ),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtectedCacheCatalogStoreErrorKind {
    Busy,
    Conflict,
    Missing,
    RecoveryRequired,
    VersionIncompatible,
    NonCanonical,
    UnsafeFilesystem,
    CorruptState,
    Io,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProtectedCacheCatalogStoreError {
    kind: ProtectedCacheCatalogStoreErrorKind,
    message: String,
}

impl ProtectedCacheCatalogStoreError {
    #[must_use]
    pub const fn kind(&self) -> ProtectedCacheCatalogStoreErrorKind {
        self.kind
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for ProtectedCacheCatalogStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ProtectedCacheCatalogStoreError {}

fn store_error(
    kind: ProtectedCacheCatalogStoreErrorKind,
    message: impl Into<String>,
) -> ProtectedCacheCatalogStoreError {
    ProtectedCacheCatalogStoreError {
        kind,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::fs as stdfs;
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, mpsc};
    use std::thread;
    use std::time::Duration;

    use rustix::fs::{self, AtFlags, Mode};

    use super::{
        CATALOG_FILE, LOCK_FILE, PRIVATE_FILE_MODE, ProtectedCacheCatalogRecoveryDisposition,
        ProtectedCacheCatalogStateRootGeneration, ProtectedCacheCatalogStoreAuthority,
        ProtectedCacheCatalogStoreBinding, ProtectedCacheCatalogStoreErrorKind,
        ProtectedCacheCatalogStoreRead, ProtectedCacheCurrentTransitionAuthorization,
        STORE_DIRECTORY, StoreLockMode, UnixProtectedCacheGenerationCatalogStore,
        encode_store_envelope, open_private_file, replace_between_directory_barriers,
        verify_retained_file_path,
    };
    use crate::cache_inventory::CacheStateId;
    use crate::protected_cache_generation_catalog::{
        ProtectedCacheCatalogAuthority, ProtectedCacheCatalogCorrelation,
        ProtectedCacheCatalogRevision, ProtectedCacheGenerationCatalogDocument,
        ProtectedCacheGenerationIdentity, ProtectedCacheNamespaceIdentity,
        decode_protected_cache_generation_catalog,
    };
    use crate::state::InstallationId;

    static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

    struct TestInstallation {
        path: PathBuf,
    }

    impl TestInstallation {
        fn new(label: &str) -> Self {
            let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "glaeda-protected-cache-store-{label}-{}-{sequence}",
                std::process::id()
            ));
            stdfs::create_dir(&path).expect("create test installation");
            stdfs::set_permissions(&path, stdfs::Permissions::from_mode(0o750))
                .expect("set installation mode");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }

        fn store_path(&self) -> PathBuf {
            self.path.join(STORE_DIRECTORY)
        }
    }

    impl Drop for TestInstallation {
        fn drop(&mut self) {
            let _ = stdfs::remove_dir_all(&self.path);
        }
    }

    fn namespace(value: char) -> ProtectedCacheNamespaceIdentity {
        ProtectedCacheNamespaceIdentity::parse(&format!("sha256:{}", value.to_string().repeat(64)))
            .expect("namespace")
    }

    fn binding(value: char) -> ProtectedCacheCatalogStoreBinding {
        ProtectedCacheCatalogStoreBinding::new(
            InstallationId::parse("0123456789abcdef").expect("installation"),
            ProtectedCacheCatalogStateRootGeneration::GlaedaCurrentV1,
            namespace(value),
        )
    }

    fn open(root: &TestInstallation) -> UnixProtectedCacheGenerationCatalogStore {
        UnixProtectedCacheGenerationCatalogStore::open_or_create(root.path(), binding('a'))
            .expect("open store")
    }

    fn state(value: &str) -> CacheStateId {
        CacheStateId::parse(value).expect("state identity")
    }

    fn generation(value: char) -> ProtectedCacheGenerationIdentity {
        ProtectedCacheGenerationIdentity::parse(&format!("sha256:{}", value.to_string().repeat(64)))
            .expect("generation identity")
    }

    fn transition_authorization(
        expected_revision: ProtectedCacheCatalogRevision,
        state_id: &str,
        generation_identity: char,
    ) -> ProtectedCacheCurrentTransitionAuthorization {
        ProtectedCacheCurrentTransitionAuthorization {
            expected_revision,
            state_id: state(state_id),
            generation_identity: generation(generation_identity),
        }
    }

    #[test]
    fn currentness_replacement_requires_pre_barrier_and_preserves_order() {
        let events = RefCell::new(Vec::new());
        let sync_count = Cell::new(0_u8);
        replace_between_directory_barriers(
            || {
                sync_count.set(sync_count.get() + 1);
                events.borrow_mut().push("barrier");
                Ok::<(), &'static str>(())
            },
            || {
                events.borrow_mut().push("replace");
                Ok(())
            },
        )
        .expect("barrier-replace-barrier sequence");
        assert_eq!(sync_count.get(), 2);
        assert_eq!(*events.borrow(), ["barrier", "replace", "barrier"]);

        let replacement_attempted = Cell::new(false);
        let barrier_calls = Cell::new(0_u8);
        let error = replace_between_directory_barriers(
            || {
                barrier_calls.set(barrier_calls.get() + 1);
                Err::<(), _>("pre-replacement barrier failed")
            },
            || {
                replacement_attempted.set(true);
                Ok(())
            },
        )
        .expect_err("failed pre-replacement barrier must stop publication");
        assert_eq!(error, "pre-replacement barrier failed");
        assert_eq!(barrier_calls.get(), 1);
        assert!(!replacement_attempted.get());
    }

    fn abandon_exact_stage(store: &UnixProtectedCacheGenerationCatalogStore) -> String {
        let document = ProtectedCacheGenerationCatalogDocument::empty(
            store.binding.namespace_identity.clone(),
        );
        let encoded = encode_store_envelope(&store.binding, &document).expect("encode envelope");
        let mut staged = store.stage_envelope(&encoded).expect("stage envelope");
        let name = staged.name().to_owned();
        staged.disarm();
        drop(staged);
        name
    }

    fn abandon_current_transition(
        store: &UnixProtectedCacheGenerationCatalogStore,
        expected_revision: ProtectedCacheCatalogRevision,
        state_id: &str,
        generation_identity: char,
    ) -> String {
        let ProtectedCacheCatalogStoreRead::Present(current) =
            store.load().expect("load current catalog")
        else {
            panic!("catalog must be present");
        };
        let successor = current
            .document()
            .prepare_current_transition(
                expected_revision,
                state(state_id),
                generation(generation_identity),
            )
            .expect("prepare transition stage");
        let encoded = encode_store_envelope(&store.binding, &successor).expect("encode successor");
        let mut staged = store.stage_envelope(&encoded).expect("stage successor");
        let name = staged.name().to_owned();
        staged.disarm();
        drop(staged);
        name
    }

    #[test]
    fn create_and_load_bind_one_empty_private_snapshot() {
        let root = TestInstallation::new("create-load");
        let store = open(&root);
        assert_eq!(
            store.load().expect("load missing"),
            ProtectedCacheCatalogStoreRead::Missing
        );

        let created = store.create_empty().expect("create empty catalog");
        assert_eq!(
            created.authority(),
            ProtectedCacheCatalogStoreAuthority::ProtectedStoreSnapshotOnly
        );
        assert_eq!(
            created.document().authority(),
            ProtectedCacheCatalogAuthority::SuppliedDocumentOnly
        );
        assert_eq!(created.document().revision().get(), 1);
        assert!(created.document().generations().is_empty());
        assert_eq!(created.binding(), &binding('a'));

        let ProtectedCacheCatalogStoreRead::Present(loaded) = store.load().expect("load catalog")
        else {
            panic!("catalog must be present");
        };
        assert_eq!(loaded, created);
        assert_eq!(
            stdfs::metadata(root.store_path())
                .expect("store metadata")
                .permissions()
                .mode()
                & 0o7777,
            0o700
        );
        for name in [LOCK_FILE, CATALOG_FILE] {
            assert_eq!(
                stdfs::metadata(root.store_path().join(name))
                    .expect("file metadata")
                    .permissions()
                    .mode()
                    & 0o7777,
                0o600
            );
        }
    }

    #[test]
    fn duplicate_create_and_binding_drift_fail_closed() {
        let root = TestInstallation::new("conflicts");
        let store = open(&root);
        store.create_empty().expect("create catalog");
        assert_eq!(
            store
                .create_empty()
                .expect_err("duplicate must fail")
                .kind(),
            ProtectedCacheCatalogStoreErrorKind::Conflict
        );
        let foreign =
            UnixProtectedCacheGenerationCatalogStore::open_or_create(root.path(), binding('b'))
                .expect("open same filesystem with foreign expectation");
        assert_eq!(
            foreign.load().expect_err("binding drift must fail").kind(),
            ProtectedCacheCatalogStoreErrorKind::Conflict
        );
    }

    #[test]
    fn current_transition_is_revision_cas_and_retires_the_previous_current() {
        let root = TestInstallation::new("transition-current");
        let store = open(&root);
        let empty = store.create_empty().expect("create catalog");
        let first = store
            .transition_current(transition_authorization(
                empty.document().revision(),
                "state-first",
                'b',
            ))
            .expect("publish first current");
        assert_eq!(
            first.authority(),
            ProtectedCacheCatalogStoreAuthority::ProtectedStoreSnapshotOnly
        );
        assert_eq!(first.document().revision().get(), 2);
        assert_eq!(
            first
                .document()
                .correlate(first.document().revision(), &state("state-first")),
            Ok(ProtectedCacheCatalogCorrelation::Current)
        );

        assert_eq!(
            store
                .transition_current(transition_authorization(
                    empty.document().revision(),
                    "state-stale",
                    'c',
                ))
                .expect_err("stale transition")
                .kind(),
            ProtectedCacheCatalogStoreErrorKind::Conflict
        );
        let second = store
            .transition_current(transition_authorization(
                first.document().revision(),
                "state-second",
                'c',
            ))
            .expect("publish second current");
        assert_eq!(second.document().revision().get(), 3);
        assert_eq!(
            second
                .document()
                .correlate(second.document().revision(), &state("state-first")),
            Ok(ProtectedCacheCatalogCorrelation::Retired)
        );
        assert_eq!(
            second
                .document()
                .correlate(second.document().revision(), &state("state-second")),
            Ok(ProtectedCacheCatalogCorrelation::Current)
        );
    }

    #[test]
    fn abandoned_exact_transition_is_published_and_duplicate_stage_is_removed() {
        let root = TestInstallation::new("recover-transition");
        let store = open(&root);
        let empty = store.create_empty().expect("create catalog");
        abandon_current_transition(&store, empty.document().revision(), "state-first", 'b');
        assert_eq!(
            store.load().expect_err("stage debt").kind(),
            ProtectedCacheCatalogStoreErrorKind::RecoveryRequired
        );
        assert_eq!(
            store
                .recover_abandoned_transition()
                .expect("publish abandoned transition"),
            ProtectedCacheCatalogRecoveryDisposition::PublishedAbandonedTransition
        );
        let ProtectedCacheCatalogStoreRead::Present(current) =
            store.load().expect("load recovered transition")
        else {
            panic!("catalog must be present");
        };
        assert_eq!(current.document().revision().get(), 2);

        let encoded =
            encode_store_envelope(&store.binding, current.document()).expect("encode current");
        let duplicate_name = {
            let mut staged = store.stage_envelope(&encoded).expect("stage duplicate");
            let name = staged.name().to_owned();
            staged.disarm();
            name
        };
        assert_eq!(
            store
                .recover_abandoned_transition()
                .expect("remove duplicate transition stage"),
            ProtectedCacheCatalogRecoveryDisposition::RemovedDuplicateStage
        );
        assert!(!root.store_path().join(duplicate_name).exists());
    }

    #[test]
    fn skipped_revision_and_missing_predecessor_transition_recovery_fail_closed() {
        let root = TestInstallation::new("invalid-transition-recovery");
        let store = open(&root);
        store.create_empty().expect("create catalog");
        let skipped = format!(
            "{{\"schema_version\":1,\"family\":\"cargo_target_v1\",\"namespace_identity\":\"{}\",\"revision\":3,\"current_state_id\":\"state-current\",\"generations\":[{{\"state_id\":\"state-current\",\"generation_identity\":\"{}\",\"lifecycle\":\"current\"}}],\"recovery\":{{\"state\":\"clean\"}}}}",
            namespace('a').as_str(),
            generation('b').as_str()
        );
        let document = decode_protected_cache_generation_catalog(skipped.as_bytes())
            .expect("canonical skipped revision fixture");
        let encoded = encode_store_envelope(&binding('a'), &document).expect("encode envelope");
        let mut staged = store
            .stage_envelope(&encoded)
            .expect("stage invalid successor");
        staged.disarm();
        drop(staged);
        assert_eq!(
            store
                .recover_abandoned_transition()
                .expect_err("skipped revision")
                .kind(),
            ProtectedCacheCatalogStoreErrorKind::RecoveryRequired
        );

        drop(store);
        for entry in stdfs::read_dir(root.store_path()).expect("list store") {
            let entry = entry.expect("entry");
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with(super::STAGE_PREFIX)
            {
                stdfs::remove_file(entry.path()).expect("remove invalid stage");
            }
        }
        stdfs::remove_file(root.store_path().join(CATALOG_FILE)).expect("remove predecessor");
        let store = open(&root);
        let empty = ProtectedCacheGenerationCatalogDocument::empty(namespace('a'));
        let successor = empty
            .prepare_current_transition(empty.revision(), state("state-current"), generation('b'))
            .expect("prepare successor");
        let encoded = encode_store_envelope(&binding('a'), &successor).expect("encode successor");
        let mut staged = store
            .stage_envelope(&encoded)
            .expect("stage without predecessor");
        staged.disarm();
        drop(staged);
        assert_eq!(
            store
                .recover_abandoned_transition()
                .expect_err("missing predecessor")
                .kind(),
            ProtectedCacheCatalogStoreErrorKind::RecoveryRequired
        );
    }

    #[test]
    fn abandoned_create_blocks_load_then_recovers_exactly() {
        let root = TestInstallation::new("recover-publish");
        let store = open(&root);
        abandon_exact_stage(&store);
        assert_eq!(
            store.load().expect_err("stage debt must block").kind(),
            ProtectedCacheCatalogStoreErrorKind::RecoveryRequired
        );
        assert_eq!(
            store.recover_abandoned_create().expect("recover create"),
            ProtectedCacheCatalogRecoveryDisposition::PublishedAbandonedCreate
        );
        assert!(matches!(
            store.load().expect("load recovered catalog"),
            ProtectedCacheCatalogStoreRead::Present(_)
        ));
    }

    #[test]
    fn identical_abandoned_stage_after_publish_is_removed() {
        let root = TestInstallation::new("recover-duplicate");
        let store = open(&root);
        store.create_empty().expect("create catalog");
        let name = abandon_exact_stage(&store);
        assert_eq!(
            store.recover_abandoned_create().expect("remove duplicate"),
            ProtectedCacheCatalogRecoveryDisposition::RemovedDuplicateStage
        );
        assert!(!root.store_path().join(name).exists());
        assert!(matches!(
            store.load().expect("load clean catalog"),
            ProtectedCacheCatalogStoreRead::Present(_)
        ));
    }

    #[test]
    fn multiple_or_malformed_stages_are_never_guessed_away() {
        let root = TestInstallation::new("ambiguous-stages");
        let store = open(&root);
        abandon_exact_stage(&store);
        abandon_exact_stage(&store);
        assert_eq!(
            store
                .recover_abandoned_create()
                .expect_err("multiple stages must fail")
                .kind(),
            ProtectedCacheCatalogStoreErrorKind::RecoveryRequired
        );

        drop(store);
        for entry in stdfs::read_dir(root.store_path()).expect("list store") {
            let entry = entry.expect("entry");
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with(super::STAGE_PREFIX)
            {
                stdfs::remove_file(entry.path()).expect("remove exact test stage");
            }
        }
        let store = open(&root);
        let name = abandon_exact_stage(&store);
        stdfs::write(root.store_path().join(name), b"{}").expect("corrupt stage");
        assert_eq!(
            store
                .recover_abandoned_create()
                .expect_err("malformed stage must fail")
                .kind(),
            ProtectedCacheCatalogStoreErrorKind::CorruptState
        );
    }

    #[test]
    fn valid_nonempty_stage_cannot_be_promoted_by_recovery() {
        let root = TestInstallation::new("nonempty-stage");
        let store = open(&root);
        let catalog = format!(
            "{{\"schema_version\":1,\"family\":\"cargo_target_v1\",\"namespace_identity\":\"{}\",\"revision\":2,\"current_state_id\":\"state-current\",\"generations\":[{{\"state_id\":\"state-current\",\"generation_identity\":\"{}\",\"lifecycle\":\"current\"}}],\"recovery\":{{\"state\":\"clean\"}}}}",
            namespace('a').as_str(),
            namespace('b').as_str()
        );
        let document = decode_protected_cache_generation_catalog(catalog.as_bytes())
            .expect("canonical nonempty catalog fixture");
        let encoded = encode_store_envelope(&binding('a'), &document).expect("encode envelope");
        let mut staged = store.stage_envelope(&encoded).expect("stage envelope");
        staged.disarm();
        drop(staged);

        assert_eq!(
            store
                .recover_abandoned_create()
                .expect_err("recovery cannot publish a nonempty supplied document")
                .kind(),
            ProtectedCacheCatalogStoreErrorKind::RecoveryRequired
        );
        assert_eq!(
            store.load().expect_err("stage debt remains visible").kind(),
            ProtectedCacheCatalogStoreErrorKind::RecoveryRequired
        );
    }

    #[test]
    fn concurrent_lock_refuses_a_second_operation() {
        let root = TestInstallation::new("busy");
        let first = open(&root);
        let second = open(&root);
        let _guard = first
            .acquire_lock(StoreLockMode::Exclusive)
            .expect("hold exclusive lock");
        assert_eq!(
            second
                .load()
                .expect_err("second operation must be busy")
                .kind(),
            ProtectedCacheCatalogStoreErrorKind::Busy
        );
    }

    #[test]
    fn shared_store_instance_uses_independent_operation_locks() {
        let root = TestInstallation::new("shared-busy");
        let store = Arc::new(open(&root));
        let _guard = store
            .acquire_lock(StoreLockMode::Exclusive)
            .expect("hold exclusive lock");
        let shared = Arc::clone(&store);
        assert_eq!(
            shared
                .load()
                .expect_err("shared-instance operation must be busy")
                .kind(),
            ProtectedCacheCatalogStoreErrorKind::Busy
        );
    }

    #[test]
    fn catalog_and_stage_fifos_fail_without_blocking_for_a_writer() {
        let root = TestInstallation::new("fifo");
        let store = Arc::new(open(&root));
        let catalog = root.store_path().join(CATALOG_FILE);
        assert!(
            Command::new("/usr/bin/mkfifo")
                .arg(&catalog)
                .status()
                .expect("create catalog FIFO")
                .success()
        );
        let (sender, receiver) = mpsc::channel();
        let shared = Arc::clone(&store);
        thread::spawn(move || {
            let _ = sender.send(shared.load());
        });
        assert_eq!(
            receiver
                .recv_timeout(Duration::from_secs(2))
                .expect("catalog FIFO load must not wait for a writer")
                .expect_err("catalog FIFO must fail")
                .kind(),
            ProtectedCacheCatalogStoreErrorKind::UnsafeFilesystem
        );
        stdfs::remove_file(&catalog).expect("remove catalog FIFO");

        let stage = root.store_path().join(".catalog-stage-1-1.json");
        assert!(
            Command::new("/usr/bin/mkfifo")
                .arg(&stage)
                .status()
                .expect("create stage FIFO")
                .success()
        );
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let _ = sender.send(store.recover_abandoned_create());
        });
        assert_eq!(
            receiver
                .recv_timeout(Duration::from_secs(2))
                .expect("stage FIFO recovery must not wait for a writer")
                .expect_err("stage FIFO must fail")
                .kind(),
            ProtectedCacheCatalogStoreErrorKind::UnsafeFilesystem
        );
    }

    #[test]
    fn symlink_hardlink_mode_and_lock_replacement_are_refused() {
        let root = TestInstallation::new("unsafe");
        let store = open(&root);
        store.create_empty().expect("create catalog");
        let catalog = root.store_path().join(CATALOG_FILE);
        let outside = root.path().join("outside");
        stdfs::write(&outside, b"outside").expect("outside file");
        stdfs::set_permissions(&outside, stdfs::Permissions::from_mode(0o600))
            .expect("outside mode");

        stdfs::remove_file(&catalog).expect("remove catalog");
        symlink(&outside, &catalog).expect("symlink catalog");
        assert_eq!(
            store.load().expect_err("symlink must fail").kind(),
            ProtectedCacheCatalogStoreErrorKind::UnsafeFilesystem
        );
        stdfs::remove_file(&catalog).expect("remove symlink");
        store.create_empty().expect("recreate catalog");
        stdfs::hard_link(&catalog, root.path().join("catalog-hardlink"))
            .expect("hard link catalog");
        assert_eq!(
            store.load().expect_err("hard link must fail").kind(),
            ProtectedCacheCatalogStoreErrorKind::UnsafeFilesystem
        );
        stdfs::remove_file(root.path().join("catalog-hardlink")).expect("remove hard link");
        stdfs::set_permissions(&catalog, stdfs::Permissions::from_mode(0o644))
            .expect("broaden catalog mode");
        assert_eq!(
            store.load().expect_err("broad mode must fail").kind(),
            ProtectedCacheCatalogStoreErrorKind::UnsafeFilesystem
        );
        stdfs::set_permissions(&catalog, stdfs::Permissions::from_mode(0o600))
            .expect("restore catalog mode");

        let lock_path = root.store_path().join(LOCK_FILE);
        stdfs::remove_file(&lock_path).expect("remove lock path");
        stdfs::write(&lock_path, []).expect("replace lock");
        stdfs::set_permissions(&lock_path, stdfs::Permissions::from_mode(0o600))
            .expect("replacement lock mode");
        assert_eq!(
            store.load().expect_err("lock replacement must fail").kind(),
            ProtectedCacheCatalogStoreErrorKind::UnsafeFilesystem
        );
    }

    #[test]
    fn unexpected_and_noncanonical_store_state_fail_closed() {
        let root = TestInstallation::new("corrupt");
        let store = open(&root);
        store.create_empty().expect("create catalog");
        stdfs::write(root.store_path().join("surprise"), []).expect("unexpected entry");
        stdfs::set_permissions(
            root.store_path().join("surprise"),
            stdfs::Permissions::from_mode(0o600),
        )
        .expect("unexpected mode");
        assert_eq!(
            store.load().expect_err("unexpected entry must fail").kind(),
            ProtectedCacheCatalogStoreErrorKind::CorruptState
        );
        stdfs::remove_file(root.store_path().join("surprise")).expect("remove unexpected");

        let path = root.store_path().join(CATALOG_FILE);
        let bytes = stdfs::read(&path).expect("read catalog");
        let mut noncanonical = bytes;
        noncanonical.push(b'\n');
        stdfs::write(&path, noncanonical).expect("write noncanonical envelope");
        stdfs::set_permissions(&path, stdfs::Permissions::from_mode(0o600)).expect("catalog mode");
        assert_eq!(
            store.load().expect_err("noncanonical must fail").kind(),
            ProtectedCacheCatalogStoreErrorKind::NonCanonical
        );
    }

    #[test]
    fn installation_and_store_directory_symlinks_are_refused() {
        let root = TestInstallation::new("symlink-root");
        let alias = root.path().with_extension("alias");
        symlink(root.path(), &alias).expect("installation symlink");
        let error = UnixProtectedCacheGenerationCatalogStore::open_or_create(&alias, binding('a'))
            .expect_err("installation symlink must fail");
        assert_eq!(
            error.kind(),
            ProtectedCacheCatalogStoreErrorKind::UnsafeFilesystem
        );
        stdfs::remove_file(&alias).expect("remove alias");

        let target = root.path().join("target");
        stdfs::create_dir(&target).expect("store target");
        stdfs::set_permissions(&target, stdfs::Permissions::from_mode(0o700)).expect("target mode");
        symlink(&target, root.store_path()).expect("store symlink");
        let error =
            UnixProtectedCacheGenerationCatalogStore::open_or_create(root.path(), binding('a'))
                .expect_err("store symlink must fail");
        assert_eq!(
            error.kind(),
            ProtectedCacheCatalogStoreErrorKind::UnsafeFilesystem
        );
    }

    #[test]
    fn retained_store_and_catalog_paths_must_still_name_the_opened_objects() {
        let root = TestInstallation::new("retained-paths");
        let store = open(&root);
        store.create_empty().expect("create catalog");
        let catalog_path = root.store_path().join(CATALOG_FILE);
        let original = open_private_file(
            &store.directory,
            CATALOG_FILE,
            store.owner,
            "protected cache catalog",
        )
        .expect("open original catalog");
        let bytes = stdfs::read(&catalog_path).expect("read original catalog");
        stdfs::remove_file(&catalog_path).expect("unlink original catalog");
        stdfs::write(&catalog_path, bytes).expect("replace catalog path");
        stdfs::set_permissions(&catalog_path, stdfs::Permissions::from_mode(0o600))
            .expect("replacement catalog mode");
        assert_eq!(
            verify_retained_file_path(
                &store.directory,
                CATALOG_FILE,
                &original,
                store.owner,
                "protected cache catalog",
            )
            .expect_err("replacement catalog identity must fail")
            .kind(),
            ProtectedCacheCatalogStoreErrorKind::UnsafeFilesystem
        );

        let old_store = root.path().join("old-store");
        stdfs::rename(root.store_path(), &old_store).expect("move retained store");
        stdfs::create_dir(root.store_path()).expect("create replacement store");
        stdfs::set_permissions(root.store_path(), stdfs::Permissions::from_mode(0o700))
            .expect("replacement store mode");
        let replacement_lock = root.store_path().join(LOCK_FILE);
        stdfs::write(&replacement_lock, []).expect("create replacement lock");
        stdfs::set_permissions(&replacement_lock, stdfs::Permissions::from_mode(0o600))
            .expect("replacement lock mode");
        assert_eq!(
            store
                .load()
                .expect_err("replacement store directory must fail")
                .kind(),
            ProtectedCacheCatalogStoreErrorKind::UnsafeFilesystem
        );
    }

    #[test]
    fn lock_is_empty_private_and_stage_cleanup_is_armed_by_default() {
        let root = TestInstallation::new("stage-cleanup");
        let store = open(&root);
        let document = ProtectedCacheGenerationCatalogDocument::empty(namespace('a'));
        let encoded = encode_store_envelope(&binding('a'), &document).expect("encode");
        let stage_name = {
            let staged = store.stage_envelope(&encoded).expect("stage");
            staged.name().to_owned()
        };
        assert!(!root.store_path().join(stage_name).exists());
        let lock = fs::openat(
            &store.directory,
            LOCK_FILE,
            super::EXISTING_FILE_FLAGS,
            Mode::empty(),
        )
        .expect("open lock");
        let stat = fs::fstat(&lock).expect("lock stat");
        assert_eq!(stat.st_size, 0);
        assert_eq!(stat.st_mode & 0o7777, PRIVATE_FILE_MODE.as_raw_mode());
        assert_eq!(stat.st_nlink, 1);
        fs::unlinkat(&store.directory, CATALOG_FILE, AtFlags::empty()).ok();
    }

    #[test]
    fn armed_stage_cleanup_never_unlinks_a_replacement() {
        let root = TestInstallation::new("stage-replacement");
        let store = open(&root);
        let document = ProtectedCacheGenerationCatalogDocument::empty(namespace('a'));
        let encoded = encode_store_envelope(&binding('a'), &document).expect("encode");
        let staged = store.stage_envelope(&encoded).expect("stage");
        let path = root.store_path().join(staged.name());
        stdfs::remove_file(&path).expect("unlink retained stage path");
        stdfs::write(&path, b"replacement").expect("create replacement stage");
        stdfs::set_permissions(&path, stdfs::Permissions::from_mode(0o600))
            .expect("replacement stage mode");
        drop(staged);
        assert_eq!(
            stdfs::read(&path).expect("replacement must remain"),
            b"replacement"
        );
    }
}
