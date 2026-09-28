#![cfg(unix)]

use std::fs::{self, OpenOptions};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;

use glaeda::execution_admission::EpochMillis;
use glaeda::personal_worker_queue::{
    PersonalWorkerActivityEvidence, PersonalWorkerProfileObservation,
    PersonalWorkerQueueGeneration, PersonalWorkerQueueInput,
};
use glaeda::personal_worker_store::{
    PersonalWorkerStore, PersonalWorkerStoreDocument, PersonalWorkerStoreErrorKind,
    PersonalWorkerStoreInitializationDisposition, PersonalWorkerStoreMigrationDisposition,
    decode_personal_worker_store_document, encode_personal_worker_store_document,
    migrate_personal_worker_store_v1_document,
};
use glaeda::unix_personal_worker_store::UnixPersonalWorkerStore;
use rustix::fs::{FlockOperation, flock};

static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

struct TempRoot(PathBuf);

impl TempRoot {
    fn new(label: &str) -> Self {
        let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "smolrunner-personal-worker-initialization-{label}-{}-{sequence}",
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
    EpochMillis::new(value).expect("time")
}

fn queue(generation: u64, observed_at: u64) -> PersonalWorkerQueueInput {
    PersonalWorkerQueueInput {
        generation: PersonalWorkerQueueGeneration::new(generation).expect("queue generation"),
        observed_at: time(observed_at),
        profile_observation: PersonalWorkerProfileObservation::Unobserved,
        activity_evidence: PersonalWorkerActivityEvidence::Never,
        queued: vec![],
        active: vec![],
        pending_profile_change: None,
    }
}

fn initial_document(observed_at: u64) -> PersonalWorkerStoreDocument {
    PersonalWorkerStoreDocument::new(queue(1, observed_at), vec![]).expect("initial document")
}

fn write_private(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).expect("write private fixture");
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).expect("set private fixture mode");
}

#[test]
fn top_level_store_version_is_distinct_from_corruption() {
    let encoded = encode_personal_worker_store_document(&initial_document(1_000_000))
        .expect("encode initial document");
    let mut incompatible: serde_json::Value =
        serde_json::from_slice(&encoded).expect("parse document");
    incompatible["schema_version"] = serde_json::Value::from(3_u64);
    let incompatible_bytes =
        serde_json::to_vec(&incompatible).expect("encode incompatible document");
    assert_eq!(
        decode_personal_worker_store_document(&incompatible_bytes)
            .expect_err("unsupported top-level version")
            .kind(),
        PersonalWorkerStoreErrorKind::VersionIncompatible
    );
    assert_eq!(
        decode_personal_worker_store_document(b"not-json")
            .expect_err("malformed state")
            .kind(),
        PersonalWorkerStoreErrorKind::CorruptState
    );
}

fn canonical_v1_initial_document(observed_at: u64) -> Vec<u8> {
    format!(
        concat!(
            "{{\n",
            "  \"schema_version\": 1,\n",
            "  \"revision\": 1,\n",
            "  \"queue\": {{\n",
            "    \"generation\": 1,\n",
            "    \"observed_at\": {observed_at},\n",
            "    \"current_profile\": \"interactive\",\n",
            "    \"last_activity_at\": {observed_at},\n",
            "    \"queued\": [],\n",
            "    \"active\": [],\n",
            "    \"pending_profile_change\": null\n",
            "  }},\n",
            "  \"cache_leases\": [],\n",
            "  \"history\": []\n",
            "}}\n"
        ),
        observed_at = observed_at
    )
    .into_bytes()
}

fn canonical_v1_revision_two_document(observed_at: u64) -> Vec<u8> {
    format!(
        concat!(
            "{{\n",
            "  \"schema_version\": 1,\n",
            "  \"revision\": 2,\n",
            "  \"queue\": {{\n",
            "    \"generation\": 2,\n",
            "    \"observed_at\": {observed_at},\n",
            "    \"current_profile\": \"work\",\n",
            "    \"last_activity_at\": {observed_at},\n",
            "    \"queued\": [],\n",
            "    \"active\": [],\n",
            "    \"pending_profile_change\": null\n",
            "  }},\n",
            "  \"cache_leases\": [],\n",
            "  \"history\": [\n",
            "    {{\n",
            "      \"revision\": 1,\n",
            "      \"queue_generation\": 1,\n",
            "      \"observed_at\": {previous_observed_at},\n",
            "      \"queued_count\": 0,\n",
            "      \"active_count\": 0,\n",
            "      \"cache_lease_count\": 0,\n",
            "      \"state_digest\": \"sha256:{digest}\"\n",
            "    }}\n",
            "  ]\n",
            "}}\n"
        ),
        observed_at = observed_at,
        previous_observed_at = observed_at - 1,
        digest = "00".repeat(32),
    )
    .into_bytes()
}

#[test]
fn v1_mapping_preserves_revision_generation_history_and_exact_observed_values() {
    let v1 = canonical_v1_revision_two_document(1_900_000);
    let migrated = migrate_personal_worker_store_v1_document(&v1).expect("map canonical v1");
    assert_eq!(migrated.revision().get(), 2);
    assert_eq!(migrated.queue().generation.get(), 2);
    assert_eq!(migrated.queue().observed_at, time(1_900_000));
    assert_eq!(
        migrated.queue().profile_observation.profile(),
        Some(glaeda::personal_worker_queue::PersonalWorkerProfile::Work)
    );
    assert_eq!(
        migrated.queue().activity_evidence.last_activity_at(),
        Some(time(1_900_000))
    );
    assert_eq!(migrated.history().len(), 1);
    assert_eq!(migrated.history()[0].revision().get(), 1);
    assert_eq!(migrated.history()[0].queue_generation().get(), 1);
    assert_eq!(migrated.history()[0].observed_at(), time(1_899_999));
}

#[test]
fn explicit_v1_migration_preserves_evidence_and_replays_without_writes() {
    let root = TempRoot::new("v1-migration");
    let initial = initial_document(2_000_000);
    UnixPersonalWorkerStore::initialize_if_clean(root.path(), &initial)
        .expect("create store authority");
    let current_path = root.store_directory().join("current.json");
    let v1 = canonical_v1_initial_document(2_000_000);
    write_private(&current_path, &v1);

    let ordinary_read = UnixPersonalWorkerStore::open_existing_read_only(root.path())
        .expect("open read-only store")
        .load()
        .expect_err("ordinary read must not migrate v1");
    assert_eq!(
        ordinary_read.kind(),
        PersonalWorkerStoreErrorKind::VersionIncompatible
    );

    let receipt = UnixPersonalWorkerStore::migrate_v1(root.path()).expect("migrate v1");
    assert_eq!(
        receipt.disposition(),
        PersonalWorkerStoreMigrationDisposition::Migrated
    );
    assert_eq!(receipt.from_schema_version(), 1);
    assert_eq!(receipt.to_schema_version(), 2);
    assert_eq!(receipt.revision().get(), 1);
    assert_eq!(receipt.queue_generation().get(), 1);
    assert!(receipt.bytes_written() > 0);

    let migrated_bytes = fs::read(&current_path).expect("read migrated current");
    let migrated =
        decode_personal_worker_store_document(&migrated_bytes).expect("decode migrated current");
    assert_eq!(
        migrated.queue().profile_observation.profile(),
        Some(glaeda::personal_worker_queue::PersonalWorkerProfile::Interactive)
    );
    assert_eq!(
        migrated.queue().activity_evidence.last_activity_at(),
        Some(time(2_000_000))
    );

    let replay = UnixPersonalWorkerStore::migrate_v1(root.path()).expect("replay migration");
    assert_eq!(
        replay.disposition(),
        PersonalWorkerStoreMigrationDisposition::AlreadyCurrent
    );
    assert_eq!(replay.bytes_written(), 0);
    assert_eq!(
        fs::read(&current_path).expect("re-read migrated current"),
        migrated_bytes
    );
}

#[test]
fn explicit_v1_migration_publishes_only_the_exact_staged_candidate() {
    let root = TempRoot::new("v1-staged-migration");
    let initial = initial_document(2_100_000);
    UnixPersonalWorkerStore::initialize_if_clean(root.path(), &initial)
        .expect("create store authority");
    let current_path = root.store_directory().join("current.json");
    let stage_path = root.store_directory().join(".next.json");
    let v1 = canonical_v1_initial_document(2_100_000);
    write_private(&current_path, &v1);
    let candidate = migrate_personal_worker_store_v1_document(&v1).expect("map v1 candidate");
    let candidate_bytes = encode_personal_worker_store_document(&candidate).expect("encode v2");
    write_private(&stage_path, &candidate_bytes);

    let receipt = UnixPersonalWorkerStore::migrate_v1(root.path()).expect("publish exact stage");
    assert_eq!(
        receipt.disposition(),
        PersonalWorkerStoreMigrationDisposition::Migrated
    );
    assert_eq!(receipt.bytes_written(), 0);
    assert_eq!(
        fs::read(&current_path).expect("read published current"),
        candidate_bytes
    );
    assert!(!stage_path.exists());

    write_private(&current_path, &v1);
    write_private(&stage_path, b"foreign staged bytes");
    let current_before = fs::read(&current_path).expect("read current before refusal");
    let stage_before = fs::read(&stage_path).expect("read stage before refusal");
    let refused = UnixPersonalWorkerStore::migrate_v1(root.path()).expect("classify foreign stage");
    assert_eq!(
        refused.disposition(),
        PersonalWorkerStoreMigrationDisposition::RecoveryRequired
    );
    assert_eq!(
        fs::read(&current_path).expect("current preserved"),
        current_before
    );
    assert_eq!(
        fs::read(&stage_path).expect("stage preserved"),
        stage_before
    );
}

#[test]
fn initialize_if_clean_creates_once_and_replays_without_byte_changes() {
    let root = TempRoot::new("create-replay");
    let initial = initial_document(2_000_000);
    let created = UnixPersonalWorkerStore::initialize_if_clean(root.path(), &initial)
        .expect("create initial state");
    assert_eq!(
        created.disposition(),
        PersonalWorkerStoreInitializationDisposition::Created
    );
    assert_eq!(created.revision(), Some(initial.revision()));
    assert!(created.bytes_written() > 0);

    let current = root.store_directory().join("current.json");
    let before = fs::read(&current).expect("read current bytes");
    let replay = UnixPersonalWorkerStore::initialize_if_clean(root.path(), &initial)
        .expect("replay initialisation");
    assert_eq!(
        replay.disposition(),
        PersonalWorkerStoreInitializationDisposition::AlreadyExists
    );
    assert_eq!(replay.revision(), Some(initial.revision()));
    assert_eq!(replay.bytes_written(), 0);
    assert_eq!(fs::read(&current).expect("read replay bytes"), before);

    let reopened = UnixPersonalWorkerStore::open_existing_read_only(root.path())
        .expect("open initialised store");
    assert_eq!(reopened.load().expect("load state"), Some(initial));
}

#[test]
fn initialize_if_clean_refuses_every_valid_recovery_shape_without_mutation() {
    let initial_only = TempRoot::new("staged-initial");
    let initial = initial_document(3_000_000);
    UnixPersonalWorkerStore::initialize_if_clean(initial_only.path(), &initial)
        .expect("create initial state");
    let initial_current = initial_only.store_directory().join("current.json");
    let initial_stage = initial_only.store_directory().join(".next.json");
    fs::rename(&initial_current, &initial_stage).expect("stage initial document");
    let initial_stage_bytes = fs::read(&initial_stage).expect("read initial stage");
    let receipt = UnixPersonalWorkerStore::initialize_if_clean(initial_only.path(), &initial)
        .expect("inspect staged initial");
    assert_eq!(
        receipt.disposition(),
        PersonalWorkerStoreInitializationDisposition::RecoveryRequired
    );
    assert_eq!(receipt.revision(), Some(initial.revision()));
    assert!(!initial_current.exists());
    assert_eq!(
        fs::read(&initial_stage).expect("re-read initial stage"),
        initial_stage_bytes
    );

    let successor_root = TempRoot::new("staged-successor");
    let current = initial_document(4_000_000);
    UnixPersonalWorkerStore::initialize_if_clean(successor_root.path(), &current)
        .expect("create current state");
    let successor = current
        .advance(queue(2, 4_000_001), vec![])
        .expect("successor document");
    let current_path = successor_root.store_directory().join("current.json");
    let stage_path = successor_root.store_directory().join(".next.json");
    let current_bytes = fs::read(&current_path).expect("read current bytes");
    let successor_bytes =
        encode_personal_worker_store_document(&successor).expect("encode successor");
    write_private(&stage_path, &successor_bytes);
    let receipt = UnixPersonalWorkerStore::initialize_if_clean(successor_root.path(), &current)
        .expect("inspect staged successor");
    assert_eq!(
        receipt.disposition(),
        PersonalWorkerStoreInitializationDisposition::RecoveryRequired
    );
    assert_eq!(receipt.revision(), Some(successor.revision()));
    assert_eq!(
        fs::read(&current_path).expect("re-read current"),
        current_bytes
    );
    assert_eq!(
        fs::read(&stage_path).expect("re-read successor"),
        successor_bytes
    );

    let stale_root = TempRoot::new("stale-stage");
    let stale = initial_document(5_000_000);
    UnixPersonalWorkerStore::initialize_if_clean(stale_root.path(), &stale)
        .expect("create stale current");
    let stale_current = stale_root.store_directory().join("current.json");
    let stale_stage = stale_root.store_directory().join(".next.json");
    let stale_bytes = fs::read(&stale_current).expect("read stale current");
    write_private(&stale_stage, &stale_bytes);
    let receipt = UnixPersonalWorkerStore::initialize_if_clean(stale_root.path(), &stale)
        .expect("inspect stale stage");
    assert_eq!(
        receipt.disposition(),
        PersonalWorkerStoreInitializationDisposition::RecoveryRequired
    );
    assert_eq!(receipt.revision(), Some(stale.revision()));
    assert_eq!(
        fs::read(&stale_current).expect("re-read stale current"),
        stale_bytes
    );
    assert_eq!(
        fs::read(&stale_stage).expect("re-read stale stage"),
        stale_bytes
    );
}

#[test]
fn initialize_if_clean_preserves_corrupt_stage_and_reports_busy_without_blocking() {
    let root = TempRoot::new("corrupt-stage");
    let initial = initial_document(6_000_000);
    UnixPersonalWorkerStore::initialize_if_clean(root.path(), &initial)
        .expect("create current state");
    let current_path = root.store_directory().join("current.json");
    let stage_path = root.store_directory().join(".next.json");
    let current_bytes = fs::read(&current_path).expect("read current bytes");
    write_private(&stage_path, b"not-json");
    let stage_bytes = fs::read(&stage_path).expect("read corrupt stage");
    let error = UnixPersonalWorkerStore::initialize_if_clean(root.path(), &initial)
        .expect_err("corrupt stage must fail closed");
    assert_eq!(error.kind(), PersonalWorkerStoreErrorKind::CorruptState);
    assert_eq!(
        fs::read(&current_path).expect("re-read current"),
        current_bytes
    );
    assert_eq!(
        fs::read(&stage_path).expect("re-read corrupt stage"),
        stage_bytes
    );

    fs::remove_file(&stage_path).expect("remove test stage");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.store_directory().join("store.lock"))
        .expect("open lock file");
    flock(&lock, FlockOperation::NonBlockingLockExclusive).expect("hold writer lock");
    let error = UnixPersonalWorkerStore::initialize_if_clean(root.path(), &initial)
        .expect_err("busy store must return immediately");
    assert_eq!(error.kind(), PersonalWorkerStoreErrorKind::Busy);
    assert_eq!(
        fs::read(&current_path).expect("read busy current"),
        current_bytes
    );
}

#[test]
fn initialize_if_clean_refuses_missing_existing_lock_without_mutation() {
    let root = TempRoot::new("missing-existing-lock");
    let initial = initial_document(7_000_000);
    UnixPersonalWorkerStore::initialize_if_clean(root.path(), &initial)
        .expect("create current state");
    let current_path = root.store_directory().join("current.json");
    let lock_path = root.store_directory().join("store.lock");
    let current_bytes = fs::read(&current_path).expect("read current bytes");
    fs::remove_file(&lock_path).expect("remove existing lock fixture");

    let error = UnixPersonalWorkerStore::initialize_if_clean(root.path(), &initial)
        .expect_err("missing synchronization metadata must fail closed");
    assert_eq!(error.kind(), PersonalWorkerStoreErrorKind::UnsafeFilesystem);
    assert!(!lock_path.exists());
    assert_eq!(
        fs::read(&current_path).expect("re-read current bytes"),
        current_bytes
    );
}

#[test]
fn concurrent_initializers_share_one_published_lock_and_document() {
    let root = TempRoot::new("concurrent");
    let root_path = root.path().to_path_buf();
    let initial = initial_document(8_000_000);
    let barrier = Arc::new(Barrier::new(2));
    let mut handles = Vec::new();
    for _ in 0..2 {
        let barrier = Arc::clone(&barrier);
        let root_path = root_path.clone();
        let initial = initial.clone();
        handles.push(thread::spawn(move || {
            barrier.wait();
            UnixPersonalWorkerStore::initialize_if_clean(root_path, &initial)
                .map(|receipt| receipt.disposition())
                .map_err(|error| error.kind())
        }));
    }

    let results: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().expect("initializer thread"))
        .collect();
    assert_eq!(
        results
            .iter()
            .filter(|result| {
                **result == Ok(PersonalWorkerStoreInitializationDisposition::Created)
            })
            .count(),
        1
    );
    assert!(results.iter().all(|result| {
        matches!(
            result,
            Ok(PersonalWorkerStoreInitializationDisposition::Created)
                | Ok(PersonalWorkerStoreInitializationDisposition::AlreadyExists)
                | Err(PersonalWorkerStoreErrorKind::Busy)
        )
    }));

    let replay = UnixPersonalWorkerStore::initialize_if_clean(root.path(), &initial)
        .expect("replay after concurrent initialization");
    assert_eq!(
        replay.disposition(),
        PersonalWorkerStoreInitializationDisposition::AlreadyExists
    );
    let reopened = UnixPersonalWorkerStore::open_existing_read_only(root.path())
        .expect("open concurrently initialized store");
    assert_eq!(reopened.load().expect("load state"), Some(initial));
    let names: Vec<_> = fs::read_dir(root.path())
        .expect("read state root")
        .map(|entry| entry.expect("directory entry").file_name())
        .collect();
    assert_eq!(names, vec!["personal-worker"]);
}
