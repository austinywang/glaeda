#![cfg(unix)]

use std::fs::{self, File, OpenOptions};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use glaeda::execution_admission::EpochMillis;
use glaeda::personal_worker_queue::{
    PersonalWorkerActivityEvidence, PersonalWorkerProfile, PersonalWorkerProfileObservation,
    PersonalWorkerQueueGeneration, PersonalWorkerQueueInput,
};
use glaeda::personal_worker_store::{
    PersonalWorkerStore, PersonalWorkerStoreDocument, PersonalWorkerStoreErrorKind,
    encode_personal_worker_store_document,
};
use glaeda::unix_personal_worker_store::UnixPersonalWorkerStore;
use rustix::fs::{FlockOperation, flock};

static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

struct TempRoot(PathBuf);

impl TempRoot {
    fn new(label: &str) -> Self {
        let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "smolrunner-personal-worker-lock-contract-{label}-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create temporary state root");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o750)).expect("set state root mode");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn store_directory(&self) -> PathBuf {
        self.0.join("personal-worker")
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn time(value: u64) -> EpochMillis {
    EpochMillis::new(value).expect("bounded epoch")
}

fn empty_queue(generation: u64, observed_at: u64) -> PersonalWorkerQueueInput {
    PersonalWorkerQueueInput {
        generation: PersonalWorkerQueueGeneration::new(generation).expect("queue generation"),
        observed_at: time(observed_at),
        profile_observation: PersonalWorkerProfileObservation::observed(
            PersonalWorkerProfile::Interactive,
        ),
        activity_evidence: PersonalWorkerActivityEvidence::observed(time(observed_at - 1)),
        queued: vec![],
        active: vec![],
        pending_profile_change: None,
    }
}

fn hold_writer_lock(store_directory: &Path) -> File {
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(store_directory.join("store.lock"))
        .expect("open persistent writer lock");
    flock(&lock, FlockOperation::NonBlockingLockExclusive).expect("hold writer lock");
    lock
}

#[test]
fn every_supported_mutation_requires_the_persistent_writer_lock() {
    let root = TempRoot::new("all-mutations");
    let (mut store, _) = UnixPersonalWorkerStore::open_or_create(root.path()).expect("open store");
    let initial = PersonalWorkerStoreDocument::new(empty_queue(1, 1_000_000), vec![])
        .expect("initial document");

    let create_lock = hold_writer_lock(&root.store_directory());
    let create_error = store
        .create(&initial)
        .expect_err("create must not bypass writer lock");
    assert_eq!(create_error.kind(), PersonalWorkerStoreErrorKind::Busy);
    drop(create_lock);

    store.create(&initial).expect("create after lock release");
    let next = initial
        .advance(empty_queue(2, 1_000_001), vec![])
        .expect("successor document");

    let replace_lock = hold_writer_lock(&root.store_directory());
    let replace_error = store
        .replace_if_revision(initial.revision(), &next)
        .expect_err("replacement must not bypass writer lock");
    assert_eq!(replace_error.kind(), PersonalWorkerStoreErrorKind::Busy);
    drop(replace_lock);

    let _recovery_lock = hold_writer_lock(&root.store_directory());
    let recovery_error = store
        .recover()
        .expect_err("recovery must not bypass writer lock");
    assert_eq!(recovery_error.kind(), PersonalWorkerStoreErrorKind::Busy);
}

#[test]
fn replacement_completed_before_lock_acquisition_is_detected_by_revision_guard() {
    let root = TempRoot::new("preexisting-replacement");
    let (mut store, _) = UnixPersonalWorkerStore::open_or_create(root.path()).expect("open store");
    let initial = PersonalWorkerStoreDocument::new(empty_queue(1, 2_000_000), vec![])
        .expect("initial document");
    store.create(&initial).expect("create initial state");

    let next = initial
        .advance(empty_queue(2, 2_000_001), vec![])
        .expect("successor document");
    let replacement_path = root.store_directory().join("replacement.json");
    fs::write(
        &replacement_path,
        encode_personal_worker_store_document(&next).expect("encode replacement"),
    )
    .expect("write replacement");
    fs::set_permissions(&replacement_path, fs::Permissions::from_mode(0o600))
        .expect("set replacement mode");
    fs::rename(
        &replacement_path,
        root.store_directory().join("current.json"),
    )
    .expect("install replacement before supported mutation");

    let error = store
        .replace_if_revision(initial.revision(), &next)
        .expect_err("stale caller revision must fail after direct pre-entry replacement");
    assert_eq!(error.kind(), PersonalWorkerStoreErrorKind::RevisionConflict);
    assert_eq!(store.load().expect("load replacement"), Some(next));
}
