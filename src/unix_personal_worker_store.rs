use std::fmt;
use std::fs::File;
use std::io::{Read as _, Seek as _, Write as _};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use rustix::fs::{self, AtFlags, FileType, FlockOperation, Mode, OFlags, RenameFlags};
use rustix::io::Errno;

use crate::personal_worker_store::{
    MAX_PERSONAL_WORKER_STORE_BYTES, PersonalWorkerStore, PersonalWorkerStoreDocument,
    PersonalWorkerStoreError, PersonalWorkerStoreErrorKind,
    PersonalWorkerStoreInitializationDisposition, PersonalWorkerStoreInitializationReceipt,
    PersonalWorkerStoreMigrationDisposition, PersonalWorkerStoreMigrationReceipt,
    PersonalWorkerStoreRecovery, PersonalWorkerStoreRecoveryDisposition,
    PersonalWorkerStoreRevision, PersonalWorkerStoreWriteDisposition,
    PersonalWorkerStoreWriteReceipt, decode_personal_worker_store_document,
    encode_personal_worker_store_document, migrate_personal_worker_store_v1_document,
};

/// Same-lock durable persistence for the disposable-attempt catalog.
mod disposable_attempt_catalog;
/// Same-lock observation-first teardown of one exact disposable VM and runner registration.
pub(crate) mod disposable_cleanup_transaction;
/// Same-lock clone checkpoint, execution, and identity publication.
mod disposable_clone_transaction;
/// Same-lock JIT generation, runner registration, and guest start transaction.
pub(crate) mod disposable_runner_transaction;
/// Same-lock durable persistence for prepared-template generation state.
pub(crate) mod disposable_template_generation;
/// Same-lock durable persistence for the personal-worker Lima lifecycle authority.
pub mod lima_authority;
/// Same-lock native-Linux task preparation, recovery, and teardown.
pub(crate) mod owned_linux_jit_transaction;
#[cfg(test)]
mod publication_fault;
#[cfg(test)]
mod publication_fault_tests;
/// Same-lock durable persistence for Runner Scale Set delivery recovery state.
pub(crate) mod scale_set_delivery_recovery;

const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const EXISTING_FILE_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
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
const MANAGED_DIRECTORY_MODE: Mode = Mode::RUSR
    .union(Mode::WUSR)
    .union(Mode::XUSR)
    .union(Mode::RGRP)
    .union(Mode::XGRP);
const PRIVATE_FILE_MODE: Mode = Mode::RUSR.union(Mode::WUSR);
pub(crate) const STORE_DIRECTORY: &str = "personal-worker";
const STORE_LOCK_FILE: &str = "store.lock";
const DISPOSABLE_WORKER_SERVICE_LOCK_FILE: &str = "disposable-worker-service.lock";
const CURRENT_DOCUMENT: &str = "current.json";
const STAGED_DOCUMENT: &str = ".next.json";
static NEXT_INITIALIZATION_STAGE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
pub struct UnixPersonalWorkerStore {
    _root: OwnedFd,
    directory: OwnedFd,
    owner: (u32, u32),
}

#[derive(Clone, PartialEq, Eq)]
pub enum PersonalWorkerStoreReadOnlyInspection {
    Missing,
    Current(PersonalWorkerStoreDocument),
    RecoveryRequired {
        revision: PersonalWorkerStoreRevision,
    },
}

impl fmt::Debug for PersonalWorkerStoreReadOnlyInspection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => formatter.write_str("Missing"),
            Self::Current(document) => formatter
                .debug_struct("Current")
                .field("revision", &document.revision())
                .finish(),
            Self::RecoveryRequired { revision } => formatter
                .debug_struct("RecoveryRequired")
                .field("revision", revision)
                .finish(),
        }
    }
}

impl PersonalWorkerStoreReadOnlyInspection {
    #[must_use]
    pub const fn revision(&self) -> Option<PersonalWorkerStoreRevision> {
        match self {
            Self::Missing => None,
            Self::Current(document) => Some(document.revision()),
            Self::RecoveryRequired { revision } => Some(*revision),
        }
    }
}

impl UnixPersonalWorkerStore {
    /// Open or create only the shared disposable-worker store and its canonical lock.
    ///
    /// This neutral opener intentionally does not inspect or recover any product document. It is
    /// used to acquire the process-lifetime service lease before choosing which document owner is
    /// allowed to recover a crash stage.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn open_or_create_disposable_worker_service_store(
        root_path: impl AsRef<Path>,
    ) -> Result<Self, PersonalWorkerStoreError> {
        let root = fs::open(root_path.as_ref(), DIRECTORY_FLAGS, Mode::empty())
            .map_err(map_root_open_error)?;
        let root_stat = inspect_directory(&root, "disposable worker service root", None)?;
        let owner = (root_stat.st_uid, root_stat.st_gid);
        let (directory, publication_lock) = open_or_publish_initialization_directory(&root, owner)?;
        let store = Self {
            _root: root,
            directory,
            owner,
        };
        let _lock = match publication_lock {
            Some(lock) => lock,
            None => store.acquire_mutation_lock()?,
        };
        synchronize_directory(&store._root, "disposable worker service root")?;
        synchronize_directory(&store.directory, "personal worker store directory")?;
        Ok(store)
    }

    pub fn open_or_create(
        root_path: impl AsRef<Path>,
    ) -> Result<(Self, PersonalWorkerStoreRecovery), PersonalWorkerStoreError> {
        let root = fs::open(root_path.as_ref(), DIRECTORY_FLAGS, Mode::empty())
            .map_err(map_root_open_error)?;
        let root_stat = inspect_directory(&root, "personal worker state root", None)?;
        let owner = (root_stat.st_uid, root_stat.st_gid);
        let directory = ensure_store_directory(&root, owner)?;
        ensure_lock_file(&directory, owner)?;
        let mut store = Self {
            _root: root,
            directory,
            owner,
        };
        let recovery = store.recover()?;
        Ok((store, recovery))
    }

    /// Open one already-created personal-worker store without taking the writer lock or recovering.
    ///
    /// This constructor never creates the managed directory, lock, current document, or staged
    /// document. Callers receive only the existing canonical `current.json` view through `load`.
    pub fn open_existing_read_only(
        root_path: impl AsRef<Path>,
    ) -> Result<Self, PersonalWorkerStoreError> {
        let root = fs::open(root_path.as_ref(), DIRECTORY_FLAGS, Mode::empty())
            .map_err(map_root_open_error)?;
        let root_stat = inspect_directory(&root, "personal worker state root", None)?;
        let owner = (root_stat.st_uid, root_stat.st_gid);
        let directory = fs::openat(&root, STORE_DIRECTORY, DIRECTORY_FLAGS, Mode::empty())
            .map_err(map_existing_store_directory_open_error)?;
        inspect_directory(&directory, "personal worker store directory", Some(owner))?;
        Ok(Self {
            _root: root,
            directory,
            owner,
        })
    }

    /// Inspect one existing store without creating, recovering, cleaning, or publishing state.
    ///
    /// A nonblocking shared lock makes the current/staged classification atomic with respect to
    /// cooperative writers while preserving this operation's read-only authority.
    pub fn inspect_read_only(
        &self,
    ) -> Result<PersonalWorkerStoreReadOnlyInspection, PersonalWorkerStoreError> {
        let _lock = self.acquire_read_lock()?;
        match self.recovery_plan()? {
            StoreRecoveryPlan::Clean { revision: None } => {
                Ok(PersonalWorkerStoreReadOnlyInspection::Missing)
            }
            StoreRecoveryPlan::Clean {
                revision: Some(revision),
            } => {
                let document = self.load_named(CURRENT_DOCUMENT)?.ok_or_else(|| {
                    PersonalWorkerStoreError::new(
                        PersonalWorkerStoreErrorKind::CorruptState,
                        "durable personal worker state changed during read-only inspection",
                    )
                })?;
                if document.revision() != revision {
                    return Err(PersonalWorkerStoreError::corrupt_state());
                }
                Ok(PersonalWorkerStoreReadOnlyInspection::Current(document))
            }
            StoreRecoveryPlan::PublishStaged { revision, .. }
            | StoreRecoveryPlan::RemoveStaleStaged { revision } => {
                Ok(PersonalWorkerStoreReadOnlyInspection::RecoveryRequired { revision })
            }
        }
    }

    /// Create the exact initial document only when no current or staged state exists.
    ///
    /// The writer lock is acquired before inspecting durable state. Any valid staged
    /// recovery state is reported without publication or cleanup, and an existing current
    /// document is returned as an idempotent result without changing its bytes.
    pub fn initialize_if_clean(
        root_path: impl AsRef<Path>,
        document: &PersonalWorkerStoreDocument,
    ) -> Result<PersonalWorkerStoreInitializationReceipt, PersonalWorkerStoreError> {
        if document.revision().get() != 1 || !document.history().is_empty() {
            return Err(store_error(
                PersonalWorkerStoreErrorKind::RevisionConflict,
                "initial personal worker state must use revision one without history",
            ));
        }
        let root = fs::open(root_path.as_ref(), DIRECTORY_FLAGS, Mode::empty())
            .map_err(map_root_open_error)?;
        let root_stat = inspect_directory(&root, "personal worker state root", None)?;
        let owner = (root_stat.st_uid, root_stat.st_gid);
        let (directory, publication_lock) = open_or_publish_initialization_directory(&root, owner)?;
        let store = Self {
            _root: root,
            directory,
            owner,
        };
        let _lock = match publication_lock {
            Some(lock) => lock,
            None => store.acquire_mutation_lock()?,
        };
        // A previous initializer may have crashed or received an fsync error after publishing the
        // managed directory but before its parent entry became durable. Every initializer closes
        // that recovery window under the canonical lock before it can inspect, publish, or report
        // success from the store.
        synchronize_directory(&store._root, "personal worker state root")?;
        disposable_attempt_catalog::refuse_unsettled(&store)?;
        scale_set_delivery_recovery::refuse_unsettled(&store)?;
        disposable_template_generation::refuse_unsettled(&store)?;
        lima_authority::refuse_unsettled_lima_authority(&store)?;
        match store.recovery_plan()? {
            StoreRecoveryPlan::Clean {
                revision: Some(revision),
            } => Ok(PersonalWorkerStoreInitializationReceipt::new(
                PersonalWorkerStoreInitializationDisposition::AlreadyExists,
                Some(revision),
                0,
            )),
            StoreRecoveryPlan::Clean { revision: None } => {
                let bytes_written = encode_personal_worker_store_document(document)?.len();
                let mut staged = store.stage_document(document)?;
                store.publish_staged(&mut staged, true)?;
                Ok(PersonalWorkerStoreInitializationReceipt::new(
                    PersonalWorkerStoreInitializationDisposition::Created,
                    Some(document.revision()),
                    bytes_written,
                ))
            }
            StoreRecoveryPlan::PublishStaged { revision, .. }
            | StoreRecoveryPlan::RemoveStaleStaged { revision } => {
                Ok(PersonalWorkerStoreInitializationReceipt::new(
                    PersonalWorkerStoreInitializationDisposition::RecoveryRequired,
                    Some(revision),
                    0,
                ))
            }
        }
    }

    /// Explicitly migrate one canonical schema-v1 store to schema v2 under its durable writer lock.
    ///
    /// This operation never creates missing store authority. A pre-existing staged file is
    /// published only when it is the exact canonical v2 image of the current canonical v1 state;
    /// every other staged shape is preserved and reported as recovery-required.
    pub fn migrate_v1(
        root_path: impl AsRef<Path>,
    ) -> Result<PersonalWorkerStoreMigrationReceipt, PersonalWorkerStoreError> {
        let root = fs::open(root_path.as_ref(), DIRECTORY_FLAGS, Mode::empty())
            .map_err(map_root_open_error)?;
        let root_stat = inspect_directory(&root, "personal worker state root", None)?;
        let owner = (root_stat.st_uid, root_stat.st_gid);
        let directory = open_existing_initialization_directory(&root, owner)?;
        let store = Self {
            _root: root,
            directory,
            owner,
        };
        let _lock = store.acquire_mutation_lock()?;
        synchronize_directory(&store._root, "personal worker state root")?;
        disposable_attempt_catalog::refuse_unsettled(&store)?;
        scale_set_delivery_recovery::refuse_unsettled(&store)?;
        disposable_template_generation::refuse_unsettled(&store)?;
        lima_authority::refuse_unsettled_lima_authority(&store)?;

        let current_bytes = store.read_named_bytes(CURRENT_DOCUMENT)?.ok_or_else(|| {
            store_error(
                PersonalWorkerStoreErrorKind::Missing,
                "personal worker state does not exist",
            )
        })?;
        let staged_bytes = store.read_named_bytes(STAGED_DOCUMENT)?;

        match decode_personal_worker_store_document(&current_bytes) {
            Ok(current) => {
                synchronize_directory(&store.directory, "personal worker store directory")?;
                Ok(PersonalWorkerStoreMigrationReceipt::new(
                    if staged_bytes.is_some() {
                        PersonalWorkerStoreMigrationDisposition::RecoveryRequired
                    } else {
                        PersonalWorkerStoreMigrationDisposition::AlreadyCurrent
                    },
                    current.schema_version(),
                    current.revision(),
                    current.queue().generation,
                    0,
                ))
            }
            Err(error) if error.kind() == PersonalWorkerStoreErrorKind::VersionIncompatible => {
                let migrated = migrate_personal_worker_store_v1_document(&current_bytes)?;
                let encoded = encode_personal_worker_store_document(&migrated)?;
                if let Some(staged_bytes) = staged_bytes {
                    let exact_stage = decode_personal_worker_store_document(&staged_bytes)
                        .is_ok_and(|staged| staged == migrated);
                    if !exact_stage || staged_bytes != encoded {
                        return Ok(PersonalWorkerStoreMigrationReceipt::new(
                            PersonalWorkerStoreMigrationDisposition::RecoveryRequired,
                            1,
                            migrated.revision(),
                            migrated.queue().generation,
                            0,
                        ));
                    }
                    store.synchronize_existing_staged(&migrated)?;
                    let mut staged =
                        StagedDocument::existing(store.directory.as_fd(), STAGED_DOCUMENT);
                    store.publish_staged(&mut staged, false)?;
                    return Ok(PersonalWorkerStoreMigrationReceipt::new(
                        PersonalWorkerStoreMigrationDisposition::Migrated,
                        1,
                        migrated.revision(),
                        migrated.queue().generation,
                        0,
                    ));
                }
                let bytes_written = encoded.len();
                let mut staged = store.stage_document(&migrated)?;
                store.publish_staged(&mut staged, false)?;
                Ok(PersonalWorkerStoreMigrationReceipt::new(
                    PersonalWorkerStoreMigrationDisposition::Migrated,
                    1,
                    migrated.revision(),
                    migrated.queue().generation,
                    bytes_written,
                ))
            }
            Err(error) => Err(error),
        }
    }

    fn acquire_mutation_lock(&self) -> Result<StoreMutationLock, PersonalWorkerStoreError> {
        acquire_mutation_lock_in(&self.directory, self.owner)
    }

    /// Acquire the process-lifetime lease for the single disposable-worker controller.
    ///
    /// Individual durable mutations continue to use `store.lock`; this separate inode prevents
    /// two bridge sessions from racing across otherwise valid short store transactions. The guard
    /// must remain owned until the bridge and supervisor have stopped.
    #[allow(dead_code)] // Consumed by the next service-driver slice.
    pub(crate) fn acquire_disposable_worker_service_lock(
        &self,
    ) -> Result<DisposableWorkerServiceLock, PersonalWorkerStoreError> {
        let _mutation = self.acquire_mutation_lock()?;
        ensure_named_lock_file(
            &self.directory,
            self.owner,
            DISPOSABLE_WORKER_SERVICE_LOCK_FILE,
            "disposable worker service lock",
        )?;
        let lock = fs::openat(
            &self.directory,
            DISPOSABLE_WORKER_SERVICE_LOCK_FILE,
            EXISTING_LOCK_FLAGS,
            Mode::empty(),
        )
        .map_err(map_lock_open_error)?;
        inspect_private_file(&lock, self.owner, "disposable worker service lock", Some(0))?;
        match fs::flock(&lock, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => Ok(DisposableWorkerServiceLock { _lock: lock }),
            Err(Errno::AGAIN) => Err(store_error(
                PersonalWorkerStoreErrorKind::Busy,
                "another disposable worker service owns the controller lease",
            )),
            Err(_) => Err(store_error(
                PersonalWorkerStoreErrorKind::Io,
                "could not acquire the disposable worker service lock",
            )),
        }
    }

    fn acquire_read_lock(&self) -> Result<StoreReadLock, PersonalWorkerStoreError> {
        let lock = fs::openat(
            &self.directory,
            STORE_LOCK_FILE,
            EXISTING_FILE_FLAGS,
            Mode::empty(),
        )
        .map_err(map_existing_initialization_lock_open_error)?;
        inspect_private_file(&lock, self.owner, "personal worker store lock", Some(0))?;
        match fs::flock(&lock, FlockOperation::NonBlockingLockShared) {
            Ok(()) => Ok(StoreReadLock { _lock: lock }),
            Err(Errno::AGAIN) => Err(store_error(
                PersonalWorkerStoreErrorKind::Busy,
                "another personal worker store mutation holds the writer lock",
            )),
            Err(_) => Err(store_error(
                PersonalWorkerStoreErrorKind::Io,
                "could not acquire the personal worker store reader lock",
            )),
        }
    }

    fn load_named(
        &self,
        name: &str,
    ) -> Result<Option<PersonalWorkerStoreDocument>, PersonalWorkerStoreError> {
        self.read_named_bytes(name)?
            .map(|bytes| decode_personal_worker_store_document(&bytes))
            .transpose()
    }

    fn read_named_bytes(&self, name: &str) -> Result<Option<Vec<u8>>, PersonalWorkerStoreError> {
        self.read_named_bytes_bounded(name, MAX_PERSONAL_WORKER_STORE_BYTES)
    }

    fn read_named_bytes_bounded(
        &self,
        name: &str,
        max_bytes: usize,
    ) -> Result<Option<Vec<u8>>, PersonalWorkerStoreError> {
        inspect_directory(
            &self.directory,
            "personal worker store directory",
            Some(self.owner),
        )?;
        let file = match fs::openat(&self.directory, name, EXISTING_FILE_FLAGS, Mode::empty()) {
            Ok(file) => file,
            Err(Errno::NOENT) => return Ok(None),
            Err(error) => return Err(map_document_open_error(error)),
        };
        inspect_private_file(&file, self.owner, "personal worker state document", None)?;
        let mut bytes = Vec::new();
        File::from(file)
            .take((max_bytes + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| {
                store_error(
                    PersonalWorkerStoreErrorKind::Io,
                    "could not read the personal worker state document",
                )
            })?;
        if bytes.len() > max_bytes {
            return Err(PersonalWorkerStoreError::corrupt_state());
        }
        Ok(Some(bytes))
    }

    fn stage_document(
        &self,
        document: &PersonalWorkerStoreDocument,
    ) -> Result<StagedDocument<'_>, PersonalWorkerStoreError> {
        let encoded = encode_personal_worker_store_document(document)?;
        self.stage_named_bytes(STAGED_DOCUMENT, &encoded)
    }

    fn stage_named_bytes(
        &self,
        staged_name: &'static str,
        encoded: &[u8],
    ) -> Result<StagedDocument<'_>, PersonalWorkerStoreError> {
        let file = fs::openat(
            &self.directory,
            staged_name,
            NEW_FILE_FLAGS,
            PRIVATE_FILE_MODE,
        )
        .map_err(map_stage_create_error)?;
        let mut staged = StagedDocument {
            directory: self.directory.as_fd(),
            name: staged_name,
            file: Some(file),
            armed: true,
        };
        let opened = staged.file.as_ref().expect("staged file is present");
        fs::fchmod(opened, PRIVATE_FILE_MODE).map_err(|_| {
            store_error(
                PersonalWorkerStoreErrorKind::Io,
                "could not set private staged-state permissions",
            )
        })?;
        inspect_private_file(
            opened,
            self.owner,
            "staged personal worker document",
            Some(0),
        )?;
        let mut file = File::from(staged.file.take().expect("staged file is present"));
        #[cfg(test)]
        publication_fault::maybe_fail_publication(
            publication_fault::PublicationFaultPoint::StageWrite,
        )?;
        file.write_all(encoded).map_err(|_| {
            store_error(
                PersonalWorkerStoreErrorKind::Io,
                "could not write the staged personal worker document",
            )
        })?;
        #[cfg(test)]
        publication_fault::maybe_fail_publication(
            publication_fault::PublicationFaultPoint::StageFileSync,
        )?;
        file.sync_all().map_err(|_| {
            store_error(
                PersonalWorkerStoreErrorKind::Io,
                "could not synchronize the staged personal worker document",
            )
        })?;
        inspect_private_file(
            file.as_fd(),
            self.owner,
            "staged personal worker document",
            Some(encoded.len()),
        )?;
        staged.file = Some(file.into());
        Ok(staged)
    }

    fn publish_staged(
        &self,
        staged: &mut StagedDocument<'_>,
        no_replace: bool,
    ) -> Result<(), PersonalWorkerStoreError> {
        self.publish_named_staged(staged, CURRENT_DOCUMENT, no_replace)
    }

    fn publish_named_staged(
        &self,
        staged: &mut StagedDocument<'_>,
        current_name: &'static str,
        no_replace: bool,
    ) -> Result<(), PersonalWorkerStoreError> {
        let flags = if no_replace {
            RenameFlags::NOREPLACE
        } else {
            RenameFlags::empty()
        };
        #[cfg(test)]
        publication_fault::maybe_fail_publication(
            publication_fault::PublicationFaultPoint::PublishRename,
        )?;
        fs::renameat_with(
            &self.directory,
            staged.name,
            &self.directory,
            current_name,
            flags,
        )
        .map_err(|error| map_publish_error(error, no_replace))?;
        staged.disarm();
        #[cfg(test)]
        publication_fault::maybe_fail_publication(
            publication_fault::PublicationFaultPoint::PublicationDirectorySync,
        )?;
        synchronize_directory(&self.directory, "personal worker store directory")
    }

    fn synchronize_existing_staged(
        &self,
        expected: &PersonalWorkerStoreDocument,
    ) -> Result<(), PersonalWorkerStoreError> {
        let file = fs::openat(
            &self.directory,
            STAGED_DOCUMENT,
            EXISTING_FILE_FLAGS,
            Mode::empty(),
        )
        .map_err(map_document_open_error)?;
        inspect_private_file(&file, self.owner, "staged personal worker document", None)?;
        let mut file = File::from(file);
        let mut bytes = Vec::new();
        std::io::Read::by_ref(&mut file)
            .take((MAX_PERSONAL_WORKER_STORE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| {
                store_error(
                    PersonalWorkerStoreErrorKind::Io,
                    "could not read the staged personal worker document",
                )
            })?;
        if bytes.len() > MAX_PERSONAL_WORKER_STORE_BYTES
            || decode_personal_worker_store_document(&bytes).as_ref() != Ok(expected)
        {
            return Err(PersonalWorkerStoreError::corrupt_state());
        }
        file.sync_all().map_err(|_| {
            store_error(
                PersonalWorkerStoreErrorKind::Io,
                "could not synchronize the staged personal worker document",
            )
        })?;
        inspect_private_file(
            file.as_fd(),
            self.owner,
            "staged personal worker document",
            Some(bytes.len()),
        )?;
        Ok(())
    }

    fn remove_staged(&self) -> Result<(), PersonalWorkerStoreError> {
        match fs::unlinkat(&self.directory, STAGED_DOCUMENT, AtFlags::empty()) {
            Ok(()) => synchronize_directory(&self.directory, "personal worker store directory"),
            Err(Errno::NOENT) => Ok(()),
            Err(_) => Err(store_error(
                PersonalWorkerStoreErrorKind::Io,
                "could not remove stale staged personal worker state",
            )),
        }
    }

    fn recovery_plan(&self) -> Result<StoreRecoveryPlan, PersonalWorkerStoreError> {
        let Some(staged) = self.load_named(STAGED_DOCUMENT)? else {
            return Ok(StoreRecoveryPlan::Clean {
                revision: self
                    .load_named(CURRENT_DOCUMENT)?
                    .map(|document| document.revision()),
            });
        };
        let current = self.load_named(CURRENT_DOCUMENT)?;
        match current {
            None => {
                if staged.revision().get() != 1 || !staged.history().is_empty() {
                    return Err(PersonalWorkerStoreError::corrupt_state());
                }
                Ok(StoreRecoveryPlan::PublishStaged {
                    revision: staged.revision(),
                    no_replace: true,
                })
            }
            Some(current) if staged.revision() <= current.revision() => {
                Ok(StoreRecoveryPlan::RemoveStaleStaged {
                    revision: current.revision(),
                })
            }
            Some(current) => {
                staged
                    .validate_successor_of(&current)
                    .map_err(|_| PersonalWorkerStoreError::corrupt_state())?;
                Ok(StoreRecoveryPlan::PublishStaged {
                    revision: staged.revision(),
                    no_replace: false,
                })
            }
        }
    }

    fn recover_locked(&mut self) -> Result<PersonalWorkerStoreRecovery, PersonalWorkerStoreError> {
        match self.recovery_plan()? {
            StoreRecoveryPlan::Clean { revision } => Ok(PersonalWorkerStoreRecovery::new(
                PersonalWorkerStoreRecoveryDisposition::Clean,
                revision,
            )),
            StoreRecoveryPlan::PublishStaged {
                revision,
                no_replace,
            } => {
                let staged = self
                    .load_named(STAGED_DOCUMENT)?
                    .ok_or_else(PersonalWorkerStoreError::corrupt_state)?;
                self.synchronize_existing_staged(&staged)?;
                let mut staged_guard =
                    StagedDocument::existing(self.directory.as_fd(), STAGED_DOCUMENT);
                self.publish_staged(&mut staged_guard, no_replace)?;
                Ok(PersonalWorkerStoreRecovery::new(
                    PersonalWorkerStoreRecoveryDisposition::PublishedStaged,
                    Some(revision),
                ))
            }
            StoreRecoveryPlan::RemoveStaleStaged { revision } => {
                self.remove_staged()?;
                Ok(PersonalWorkerStoreRecovery::new(
                    PersonalWorkerStoreRecoveryDisposition::RemovedStaleStaged,
                    Some(revision),
                ))
            }
        }
    }
}

impl PersonalWorkerStore for UnixPersonalWorkerStore {
    fn load(&self) -> Result<Option<PersonalWorkerStoreDocument>, PersonalWorkerStoreError> {
        self.load_named(CURRENT_DOCUMENT)
    }

    fn create(
        &mut self,
        document: &PersonalWorkerStoreDocument,
    ) -> Result<PersonalWorkerStoreWriteReceipt, PersonalWorkerStoreError> {
        if document.revision().get() != 1 || !document.history().is_empty() {
            return Err(store_error(
                PersonalWorkerStoreErrorKind::RevisionConflict,
                "initial personal worker state must use revision one without history",
            ));
        }
        let _lock = self.acquire_mutation_lock()?;
        disposable_attempt_catalog::refuse_unsettled(self)?;
        scale_set_delivery_recovery::refuse_unsettled(self)?;
        disposable_template_generation::refuse_unsettled(self)?;
        lima_authority::refuse_unsettled_lima_authority(self)?;
        self.recover_locked()?;
        if self.load_named(CURRENT_DOCUMENT)?.is_some() {
            return Err(store_error(
                PersonalWorkerStoreErrorKind::RevisionConflict,
                "personal worker state already exists",
            ));
        }
        let bytes_written = encode_personal_worker_store_document(document)?.len();
        let mut staged = self.stage_document(document)?;
        self.publish_staged(&mut staged, true)?;
        Ok(PersonalWorkerStoreWriteReceipt::new(
            PersonalWorkerStoreWriteDisposition::Created,
            document.revision(),
            bytes_written,
        ))
    }

    fn replace_if_revision(
        &mut self,
        expected_revision: PersonalWorkerStoreRevision,
        document: &PersonalWorkerStoreDocument,
    ) -> Result<PersonalWorkerStoreWriteReceipt, PersonalWorkerStoreError> {
        let _lock = self.acquire_mutation_lock()?;
        disposable_attempt_catalog::refuse_unsettled(self)?;
        scale_set_delivery_recovery::refuse_unsettled(self)?;
        disposable_template_generation::refuse_unsettled(self)?;
        lima_authority::refuse_unsettled_lima_authority(self)?;
        self.recover_locked()?;
        let current = self.load_named(CURRENT_DOCUMENT)?.ok_or_else(|| {
            store_error(
                PersonalWorkerStoreErrorKind::Missing,
                "personal worker state does not exist",
            )
        })?;
        if current.revision() != expected_revision {
            return Err(store_error(
                PersonalWorkerStoreErrorKind::RevisionConflict,
                "personal worker state revision changed before publication",
            ));
        }
        document.validate_successor_of(&current)?;
        let bytes_written = encode_personal_worker_store_document(document)?.len();
        let mut staged = self.stage_document(document)?;
        self.publish_staged(&mut staged, false)?;
        Ok(PersonalWorkerStoreWriteReceipt::new(
            PersonalWorkerStoreWriteDisposition::Replaced,
            document.revision(),
            bytes_written,
        ))
    }

    fn recover(&mut self) -> Result<PersonalWorkerStoreRecovery, PersonalWorkerStoreError> {
        let _lock = self.acquire_mutation_lock()?;
        disposable_attempt_catalog::refuse_unsettled(self)?;
        scale_set_delivery_recovery::refuse_unsettled(self)?;
        disposable_template_generation::refuse_unsettled(self)?;
        lima_authority::refuse_unsettled_lima_authority(self)?;
        self.recover_locked()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StoreRecoveryPlan {
    Clean {
        revision: Option<PersonalWorkerStoreRevision>,
    },
    PublishStaged {
        revision: PersonalWorkerStoreRevision,
        no_replace: bool,
    },
    RemoveStaleStaged {
        revision: PersonalWorkerStoreRevision,
    },
}

#[derive(Debug)]
struct StoreMutationLock {
    _lock: OwnedFd,
}

#[derive(Debug)]
struct StoreReadLock {
    _lock: OwnedFd,
}

#[derive(Debug)]
pub(crate) struct DisposableWorkerServiceLock {
    _lock: OwnedFd,
}

impl Drop for StoreMutationLock {
    fn drop(&mut self) {
        // `CLOEXEC` closes this descriptor at exec, but a concurrent fork can briefly inherit the
        // same open-file description after this guard is dropped. Explicitly unlocking prevents
        // that inherited duplicate from extending the mutation boundary.
        let _ = fs::flock(&self._lock, FlockOperation::Unlock);
    }
}

impl Drop for StoreReadLock {
    fn drop(&mut self) {
        // Keep read-only inspection from leaving the same transient inherited-lock window.
        let _ = fs::flock(&self._lock, FlockOperation::Unlock);
    }
}

impl Drop for DisposableWorkerServiceLock {
    fn drop(&mut self) {
        // A fork can briefly retain the same open-file description. Explicit unlock keeps that
        // inherited duplicate from extending process-lifetime controller ownership.
        let _ = fs::flock(&self._lock, FlockOperation::Unlock);
    }
}

fn acquire_mutation_lock_in(
    directory: &OwnedFd,
    owner: (u32, u32),
) -> Result<StoreMutationLock, PersonalWorkerStoreError> {
    let lock = fs::openat(
        directory,
        STORE_LOCK_FILE,
        EXISTING_LOCK_FLAGS,
        Mode::empty(),
    )
    .map_err(map_lock_open_error)?;
    inspect_private_file(&lock, owner, "personal worker store lock", Some(0))?;
    match fs::flock(&lock, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => Ok(StoreMutationLock { _lock: lock }),
        Err(Errno::AGAIN) => Err(store_error(
            PersonalWorkerStoreErrorKind::Busy,
            "another personal worker store mutation holds the writer lock",
        )),
        Err(_) => Err(store_error(
            PersonalWorkerStoreErrorKind::Io,
            "could not acquire the personal worker store writer lock",
        )),
    }
}

struct StagedDocument<'a> {
    directory: BorrowedFd<'a>,
    name: &'static str,
    file: Option<OwnedFd>,
    armed: bool,
}

impl<'a> StagedDocument<'a> {
    fn existing(directory: BorrowedFd<'a>, name: &'static str) -> Self {
        Self {
            directory,
            name,
            file: None,
            armed: false,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for StagedDocument<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::unlinkat(self.directory, self.name, AtFlags::empty());
        }
    }
}

fn ensure_store_directory(
    root: &OwnedFd,
    owner: (u32, u32),
) -> Result<OwnedFd, PersonalWorkerStoreError> {
    match fs::openat(root, STORE_DIRECTORY, DIRECTORY_FLAGS, Mode::empty()) {
        Ok(directory) => {
            inspect_directory(&directory, "personal worker store directory", Some(owner))?;
            Ok(directory)
        }
        Err(Errno::NOENT) => {
            let created = match fs::mkdirat(root, STORE_DIRECTORY, MANAGED_DIRECTORY_MODE) {
                Ok(()) => true,
                Err(Errno::EXIST) => false,
                Err(_) => {
                    return Err(store_error(
                        PersonalWorkerStoreErrorKind::Io,
                        "could not create the personal worker store directory",
                    ));
                }
            };
            let directory = fs::openat(root, STORE_DIRECTORY, DIRECTORY_FLAGS, Mode::empty())
                .map_err(map_store_directory_open_error)?;
            if created {
                fs::fchmod(&directory, MANAGED_DIRECTORY_MODE).map_err(|_| {
                    store_error(
                        PersonalWorkerStoreErrorKind::Io,
                        "could not set personal worker store directory permissions",
                    )
                })?;
            }
            inspect_directory(&directory, "personal worker store directory", Some(owner))?;
            if created {
                synchronize_directory(root, "personal worker state root")?;
            }
            Ok(directory)
        }
        Err(error) => Err(map_store_directory_open_error(error)),
    }
}

fn ensure_lock_file(
    directory: &OwnedFd,
    owner: (u32, u32),
) -> Result<(), PersonalWorkerStoreError> {
    ensure_named_lock_file(
        directory,
        owner,
        STORE_LOCK_FILE,
        "personal worker store lock",
    )
}

fn ensure_named_lock_file(
    directory: &OwnedFd,
    owner: (u32, u32),
    name: &str,
    description: &'static str,
) -> Result<(), PersonalWorkerStoreError> {
    match fs::openat(directory, name, NEW_LOCK_FLAGS, PRIVATE_FILE_MODE) {
        Ok(lock) => {
            // The canonical lock inode becomes synchronization authority as soon as the directory
            // entry is visible. Never unlink it after that point: another process may already hold
            // this inode, and replacing its name would split exclusive-writer authority.
            fs::fchmod(&lock, PRIVATE_FILE_MODE).map_err(|_| {
                store_error(
                    PersonalWorkerStoreErrorKind::Io,
                    "could not set personal worker store lock permissions",
                )
            })?;
            inspect_private_file(&lock, owner, description, Some(0))?;
            fs::fsync(&lock).map_err(|_| {
                store_error(
                    PersonalWorkerStoreErrorKind::Io,
                    "could not synchronize the personal worker store lock",
                )
            })?;
            synchronize_directory(directory, "personal worker store directory")?;
            Ok(())
        }
        Err(Errno::EXIST) => {
            let lock = fs::openat(directory, name, EXISTING_LOCK_FLAGS, Mode::empty())
                .map_err(map_lock_open_error)?;
            inspect_private_file(&lock, owner, description, Some(0))
        }
        Err(error) => Err(map_lock_open_error(error)),
    }
}

fn open_or_publish_initialization_directory(
    root: &OwnedFd,
    owner: (u32, u32),
) -> Result<(OwnedFd, Option<StoreMutationLock>), PersonalWorkerStoreError> {
    match open_existing_initialization_directory(root, owner) {
        Ok(directory) => return Ok((directory, None)),
        Err(error) if error.kind() != PersonalWorkerStoreErrorKind::Missing => return Err(error),
        Err(_) => {}
    }

    let stage_name = create_initialization_stage_name()?;
    fs::mkdirat(root, stage_name.as_str(), MANAGED_DIRECTORY_MODE).map_err(|error| {
        store_error(
            if error == Errno::EXIST {
                PersonalWorkerStoreErrorKind::Busy
            } else {
                PersonalWorkerStoreErrorKind::Io
            },
            "could not create a private personal worker initialization directory",
        )
    })?;
    let mut staged = StagedStoreDirectory {
        root: root.as_fd(),
        name: stage_name,
        armed: true,
    };
    let directory = fs::openat(root, staged.name.as_str(), DIRECTORY_FLAGS, Mode::empty())
        .map_err(map_store_directory_open_error)?;
    fs::fchmod(&directory, MANAGED_DIRECTORY_MODE).map_err(|_| {
        store_error(
            PersonalWorkerStoreErrorKind::Io,
            "could not set private personal worker initialization directory permissions",
        )
    })?;
    inspect_directory(
        &directory,
        "private personal worker initialization directory",
        Some(owner),
    )?;
    ensure_lock_file(&directory, owner)?;
    let publication_lock = acquire_mutation_lock_in(&directory, owner)?;
    synchronize_directory(
        &directory,
        "private personal worker initialization directory",
    )?;

    match fs::renameat_with(
        root,
        staged.name.as_str(),
        root,
        STORE_DIRECTORY,
        RenameFlags::NOREPLACE,
    ) {
        Ok(()) => {
            staged.armed = false;
            synchronize_directory(root, "personal worker state root")?;
            inspect_directory(&directory, "personal worker store directory", Some(owner))?;
            Ok((directory, Some(publication_lock)))
        }
        Err(Errno::EXIST) => {
            drop(publication_lock);
            drop(directory);
            drop(staged);
            open_existing_initialization_directory(root, owner).map(|directory| (directory, None))
        }
        Err(_) => Err(store_error(
            PersonalWorkerStoreErrorKind::Io,
            "could not publish the personal worker store directory",
        )),
    }
}

fn open_existing_initialization_directory(
    root: &OwnedFd,
    owner: (u32, u32),
) -> Result<OwnedFd, PersonalWorkerStoreError> {
    let directory = fs::openat(root, STORE_DIRECTORY, DIRECTORY_FLAGS, Mode::empty())
        .map_err(map_existing_store_directory_open_error)?;
    inspect_directory(&directory, "personal worker store directory", Some(owner))?;
    let lock = fs::openat(
        &directory,
        STORE_LOCK_FILE,
        EXISTING_LOCK_FLAGS,
        Mode::empty(),
    )
    .map_err(map_existing_initialization_lock_open_error)?;
    inspect_private_file(&lock, owner, "personal worker store lock", Some(0))?;
    Ok(directory)
}

fn create_initialization_stage_name() -> Result<String, PersonalWorkerStoreError> {
    let sequence = NEXT_INITIALIZATION_STAGE.fetch_add(1, Ordering::Relaxed);
    Ok(format!(
        ".personal-worker.init-{}-{sequence}",
        std::process::id()
    ))
}

struct StagedStoreDirectory<'a> {
    root: BorrowedFd<'a>,
    name: String,
    armed: bool,
}

impl Drop for StagedStoreDirectory<'_> {
    fn drop(&mut self) {
        if self.armed {
            if let Ok(directory) = fs::openat(
                self.root,
                self.name.as_str(),
                DIRECTORY_FLAGS,
                Mode::empty(),
            ) {
                let _ = fs::unlinkat(&directory, STORE_LOCK_FILE, AtFlags::empty());
            }
            let _ = fs::unlinkat(self.root, self.name.as_str(), AtFlags::REMOVEDIR);
        }
    }
}

fn inspect_directory(
    directory: impl AsFd,
    subject: &str,
    expected_owner: Option<(u32, u32)>,
) -> Result<rustix::fs::Stat, PersonalWorkerStoreError> {
    let stat = fs::fstat(directory.as_fd()).map_err(|_| {
        store_error(
            PersonalWorkerStoreErrorKind::Io,
            "could not inspect a personal worker state directory",
        )
    })?;
    if !FileType::from_raw_mode(stat.st_mode).is_dir() {
        return Err(store_error(
            PersonalWorkerStoreErrorKind::UnsafeFilesystem,
            "personal worker state path is not a directory",
        ));
    }
    if stat.st_mode & 0o7777 != 0o750 {
        return Err(store_error(
            PersonalWorkerStoreErrorKind::UnsafeFilesystem,
            "personal worker state directory does not have mode 0750",
        ));
    }
    if expected_owner.is_some_and(|owner| owner != (stat.st_uid, stat.st_gid)) {
        return Err(store_error(
            PersonalWorkerStoreErrorKind::UnsafeFilesystem,
            "personal worker state directory has an unexpected owner or group",
        ));
    }
    let _ = subject;
    Ok(stat)
}

fn inspect_private_file(
    file: impl AsFd,
    owner: (u32, u32),
    subject: &str,
    expected_size: Option<usize>,
) -> Result<(), PersonalWorkerStoreError> {
    let stat = fs::fstat(file.as_fd()).map_err(|_| {
        store_error(
            PersonalWorkerStoreErrorKind::Io,
            "could not inspect a personal worker state file",
        )
    })?;
    if !FileType::from_raw_mode(stat.st_mode).is_file() {
        return Err(store_error(
            PersonalWorkerStoreErrorKind::UnsafeFilesystem,
            "personal worker state object is not a regular file",
        ));
    }
    if stat.st_nlink != 1 {
        return Err(store_error(
            PersonalWorkerStoreErrorKind::UnsafeFilesystem,
            "personal worker state file has multiple hard links",
        ));
    }
    if stat.st_mode & 0o7777 != 0o600 {
        return Err(store_error(
            PersonalWorkerStoreErrorKind::UnsafeFilesystem,
            "personal worker state file does not have mode 0600",
        ));
    }
    if owner != (stat.st_uid, stat.st_gid) {
        return Err(store_error(
            PersonalWorkerStoreErrorKind::UnsafeFilesystem,
            "personal worker state file has an unexpected owner or group",
        ));
    }
    if expected_size.is_some_and(|expected| {
        stat.st_size < 0 || u64::try_from(expected).ok() != Some(stat.st_size as u64)
    }) {
        return Err(PersonalWorkerStoreError::corrupt_state());
    }
    let _ = subject;
    Ok(())
}

fn synchronize_directory(
    directory: impl AsFd,
    _subject: &str,
) -> Result<(), PersonalWorkerStoreError> {
    fs::fsync(directory.as_fd()).map_err(|_| {
        store_error(
            PersonalWorkerStoreErrorKind::Io,
            "could not synchronize a personal worker state directory",
        )
    })
}

fn store_error(
    kind: PersonalWorkerStoreErrorKind,
    message: &'static str,
) -> PersonalWorkerStoreError {
    PersonalWorkerStoreError::new(kind, message)
}

fn map_root_open_error(error: Errno) -> PersonalWorkerStoreError {
    match error {
        Errno::LOOP | Errno::NOTDIR => store_error(
            PersonalWorkerStoreErrorKind::UnsafeFilesystem,
            "personal worker state root is symlinked or is not a directory",
        ),
        Errno::NOENT => store_error(
            PersonalWorkerStoreErrorKind::Missing,
            "personal worker state root does not exist",
        ),
        _ => store_error(
            PersonalWorkerStoreErrorKind::Io,
            "could not open the personal worker state root",
        ),
    }
}

fn map_store_directory_open_error(error: Errno) -> PersonalWorkerStoreError {
    match error {
        Errno::LOOP | Errno::NOTDIR => store_error(
            PersonalWorkerStoreErrorKind::UnsafeFilesystem,
            "personal worker store directory is symlinked or invalid",
        ),
        _ => store_error(
            PersonalWorkerStoreErrorKind::Io,
            "could not open the personal worker store directory",
        ),
    }
}

fn map_existing_store_directory_open_error(error: Errno) -> PersonalWorkerStoreError {
    match error {
        Errno::NOENT => store_error(
            PersonalWorkerStoreErrorKind::Missing,
            "personal worker store directory does not exist",
        ),
        Errno::LOOP | Errno::NOTDIR => store_error(
            PersonalWorkerStoreErrorKind::UnsafeFilesystem,
            "personal worker store directory is symlinked or invalid",
        ),
        _ => store_error(
            PersonalWorkerStoreErrorKind::Io,
            "could not open the personal worker store directory",
        ),
    }
}

fn map_lock_open_error(error: Errno) -> PersonalWorkerStoreError {
    match error {
        Errno::LOOP | Errno::NOTDIR | Errno::ISDIR => store_error(
            PersonalWorkerStoreErrorKind::UnsafeFilesystem,
            "personal worker store lock is symlinked or invalid",
        ),
        _ => store_error(
            PersonalWorkerStoreErrorKind::Io,
            "could not open the personal worker store lock",
        ),
    }
}

fn map_existing_initialization_lock_open_error(error: Errno) -> PersonalWorkerStoreError {
    match error {
        Errno::NOENT | Errno::LOOP | Errno::NOTDIR | Errno::ISDIR => store_error(
            PersonalWorkerStoreErrorKind::UnsafeFilesystem,
            "existing personal worker store synchronization metadata is missing or unsafe",
        ),
        _ => store_error(
            PersonalWorkerStoreErrorKind::Io,
            "could not open existing personal worker store synchronization metadata",
        ),
    }
}

fn map_document_open_error(error: Errno) -> PersonalWorkerStoreError {
    match error {
        Errno::LOOP | Errno::NOTDIR | Errno::ISDIR => store_error(
            PersonalWorkerStoreErrorKind::UnsafeFilesystem,
            "personal worker state document is symlinked or invalid",
        ),
        _ => store_error(
            PersonalWorkerStoreErrorKind::Io,
            "could not open the personal worker state document",
        ),
    }
}

fn map_stage_create_error(error: Errno) -> PersonalWorkerStoreError {
    match error {
        Errno::EXIST => store_error(
            PersonalWorkerStoreErrorKind::CorruptState,
            "staged personal worker state already exists after recovery",
        ),
        Errno::LOOP | Errno::NOTDIR | Errno::ISDIR => store_error(
            PersonalWorkerStoreErrorKind::UnsafeFilesystem,
            "staged personal worker state path is unsafe",
        ),
        _ => store_error(
            PersonalWorkerStoreErrorKind::Io,
            "could not create the staged personal worker state document",
        ),
    }
}

fn map_publish_error(error: Errno, no_replace: bool) -> PersonalWorkerStoreError {
    match error {
        Errno::EXIST if no_replace => store_error(
            PersonalWorkerStoreErrorKind::RevisionConflict,
            "personal worker state already exists",
        ),
        Errno::LOOP | Errno::NOTDIR | Errno::ISDIR => store_error(
            PersonalWorkerStoreErrorKind::UnsafeFilesystem,
            "personal worker state publication path is unsafe",
        ),
        _ => store_error(
            PersonalWorkerStoreErrorKind::Io,
            "could not atomically publish the personal worker state document",
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use rustix::io::dup;

    use super::UnixPersonalWorkerStore;
    use crate::personal_worker_store::PersonalWorkerStoreErrorKind;

    static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new(label: &str) -> Self {
            let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "smolrunner-personal-worker-lock-drop-{label}-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("create temporary state root");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o750))
                .expect("set temporary root mode");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn mutation_guard_drop_unlocks_an_inherited_open_file_description() {
        let root = TempRoot::new("mutation");
        let (store, _) = UnixPersonalWorkerStore::open_or_create(root.path()).expect("open store");
        let guard = store
            .acquire_mutation_lock()
            .expect("acquire mutation lock");
        let inherited = dup(&guard._lock).expect("duplicate inherited lock descriptor");

        drop(guard);
        let reacquired = store
            .acquire_mutation_lock()
            .expect("guard drop must explicitly unlock inherited description");

        drop(reacquired);
        drop(inherited);
    }

    #[test]
    fn read_guard_drop_unlocks_an_inherited_open_file_description() {
        let root = TempRoot::new("read");
        let (store, _) = UnixPersonalWorkerStore::open_or_create(root.path()).expect("open store");
        let guard = store.acquire_read_lock().expect("acquire read lock");
        let inherited = dup(&guard._lock).expect("duplicate inherited lock descriptor");

        drop(guard);
        let mutation = store
            .acquire_mutation_lock()
            .expect("read guard drop must explicitly unlock inherited description");

        drop(mutation);
        drop(inherited);
    }

    #[test]
    fn service_lock_is_exclusive_and_drop_unlocks_an_inherited_description() {
        let root = TempRoot::new("service");
        let (store, _) = UnixPersonalWorkerStore::open_or_create(root.path()).expect("open store");
        let guard = store
            .acquire_disposable_worker_service_lock()
            .expect("acquire service lock");
        let inherited = dup(&guard._lock).expect("duplicate inherited service-lock descriptor");

        let busy = store
            .acquire_disposable_worker_service_lock()
            .expect_err("a competing service must be refused");
        assert_eq!(busy.kind(), PersonalWorkerStoreErrorKind::Busy);

        drop(guard);
        let reacquired = store
            .acquire_disposable_worker_service_lock()
            .expect("service guard drop must explicitly unlock inherited description");

        drop(reacquired);
        drop(inherited);
    }
}
