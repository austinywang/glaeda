#![cfg(unix)]

use std::fs::{self, OpenOptions};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use glaeda::artifact::{CommitId, GitTreeId, RepositoryRef, Sha256Digest};
use glaeda::execution_admission::{
    EpochMillis, ExecutionAdmissionIdentity, ExecutionAdmissionInput, ExecutionAdmissionRecord,
    ExecutionAdmissionState, ExecutionRequestId, ExecutionResourceLimits,
    FallbackProfileEligibility, HostCapacityObservation, ReservationEvidence,
    ReservationGeneration, ReservationId, RunnerProfileId,
};
use glaeda::personal_worker_queue::{
    PersonalWorkerActiveReservation, PersonalWorkerActivityEvidence, PersonalWorkerCacheAccessMode,
    PersonalWorkerCacheNamespace, PersonalWorkerCancellationState, PersonalWorkerJobRequest,
    PersonalWorkerPriority, PersonalWorkerProfile, PersonalWorkerProfileObservation,
    PersonalWorkerQueueGeneration, PersonalWorkerQueueInput, PersonalWorkerSourceIdentity,
};
use glaeda::personal_worker_store::{
    MAX_PERSONAL_WORKER_HISTORY_ENTRIES, PersonalWorkerDurableCacheLease, PersonalWorkerStore,
    PersonalWorkerStoreDocument, PersonalWorkerStoreErrorKind,
    PersonalWorkerStoreRecoveryDisposition, PersonalWorkerStoreRevision,
    PersonalWorkerStoreWriteDisposition, decode_personal_worker_store_document,
    encode_personal_worker_store_document,
};
use glaeda::unix_personal_worker_store::UnixPersonalWorkerStore;
use glaeda::verification_profile::{CacheId, VerificationProfileId};
use rustix::fs::{FlockOperation, flock};

const GIB: u64 = 1_024 * 1_024 * 1_024;
static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

struct TempRoot(PathBuf);

impl TempRoot {
    fn new(label: &str) -> Self {
        let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "smolrunner-personal-worker-store-{label}-{}-{sequence}",
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

fn limits(cpu_millis: u32, memory_gib: u64) -> ExecutionResourceLimits {
    ExecutionResourceLimits::new(cpu_millis, memory_gib * GIB, 2_048).expect("limits")
}

fn identity(id: &str) -> ExecutionAdmissionIdentity {
    ExecutionAdmissionIdentity::new(
        ExecutionRequestId::parse(id).expect("request ID"),
        VerificationProfileId::parse("smolrunner.required").expect("verification profile"),
        RunnerProfileId::parse("personal-lima-work").expect("runner profile"),
    )
}

fn source(repository: &str, digit: char) -> PersonalWorkerSourceIdentity {
    PersonalWorkerSourceIdentity::new(
        RepositoryRef::parse(repository).expect("repository"),
        CommitId::parse(&digit.to_string().repeat(40)).expect("commit"),
        GitTreeId::parse(&digit.to_string().repeat(40)).expect("tree"),
    )
}

fn namespace(repository: &str) -> PersonalWorkerCacheNamespace {
    PersonalWorkerCacheNamespace::RepositoryBuild {
        cache_id: CacheId::parse("build-cache").expect("cache ID"),
        repository: RepositoryRef::parse(repository).expect("cache repository"),
        namespace_digest: Sha256Digest::parse(&format!("sha256:{}", "ab".repeat(32)))
            .expect("cache namespace digest"),
    }
}

fn request(id: &str, repository: &str, observed_at: u64) -> PersonalWorkerJobRequest {
    PersonalWorkerJobRequest {
        identity: identity(id),
        source: source(repository, 'a'),
        priority: PersonalWorkerPriority::Normal,
        requested_limits: limits(2_000, 2),
        cache_namespace: namespace(repository),
        cache_access: PersonalWorkerCacheAccessMode::Write,
        submitted_at: time(observed_at - 60_000),
        operator_deadline: None,
        cancellation: PersonalWorkerCancellationState::Active,
        fallback_eligibility: FallbackProfileEligibility::ineligible(),
    }
}

fn active_reservation(
    id: &str,
    repository: &str,
    observed_at: u64,
) -> (
    PersonalWorkerActiveReservation,
    PersonalWorkerDurableCacheLease,
) {
    let request = request(id, repository, observed_at);
    let reservation_id =
        ReservationId::parse(&format!("reservation-{id}")).expect("reservation ID");
    let reservation_generation = ReservationGeneration::new(1).expect("reservation generation");
    let reserved_at = time(observed_at - 30_000);
    let admission = ExecutionAdmissionRecord::from_input(ExecutionAdmissionInput {
        identity: request.identity.clone(),
        state: ExecutionAdmissionState::Running,
        observed_at: time(observed_at),
        requested_limits: request.requested_limits,
        host_capacity: Some(HostCapacityObservation::new(
            time(observed_at),
            limits(8_000, 10),
        )),
        applied_limits: Some(request.requested_limits),
        queue_position: None,
        reservation: Some(ReservationEvidence::new(
            reservation_id.clone(),
            reservation_generation,
            reserved_at,
            time(observed_at + 3_600_000),
        )),
        acknowledgement: None,
        fallback_eligibility: FallbackProfileEligibility::ineligible(),
        unavailable_reason: None,
    })
    .expect("active admission");
    let lease = PersonalWorkerDurableCacheLease::new(
        request.identity.request_id.clone(),
        request.cache_namespace.clone(),
        request.cache_access,
        reservation_id,
        reservation_generation,
        reserved_at,
    );
    (
        PersonalWorkerActiveReservation {
            request,
            admission,
            started_at: Some(time(observed_at - 20_000)),
        },
        lease,
    )
}

fn empty_queue(generation: u64, observed_at: u64) -> PersonalWorkerQueueInput {
    PersonalWorkerQueueInput {
        generation: PersonalWorkerQueueGeneration::new(generation).expect("queue generation"),
        observed_at: time(observed_at),
        profile_observation: PersonalWorkerProfileObservation::observed(
            PersonalWorkerProfile::Interactive,
        ),
        activity_evidence: PersonalWorkerActivityEvidence::observed(time(observed_at - 1_000)),
        queued: vec![],
        active: vec![],
        pending_profile_change: None,
    }
}

fn active_queue(
    generation: u64,
    observed_at: u64,
) -> (PersonalWorkerQueueInput, PersonalWorkerDurableCacheLease) {
    let (active, lease) = active_reservation("active-one", "example/project", observed_at);
    (
        PersonalWorkerQueueInput {
            generation: PersonalWorkerQueueGeneration::new(generation).expect("queue generation"),
            observed_at: time(observed_at),
            profile_observation: PersonalWorkerProfileObservation::observed(
                PersonalWorkerProfile::Work,
            ),
            activity_evidence: PersonalWorkerActivityEvidence::observed(time(observed_at)),
            queued: vec![],
            active: vec![active],
            pending_profile_change: None,
        },
        lease,
    )
}

#[test]
fn canonical_document_round_trips_and_rejects_alternate_bytes() {
    let document = PersonalWorkerStoreDocument::new(empty_queue(1, 1_000_000), vec![])
        .expect("initial document");
    let encoded = encode_personal_worker_store_document(&document).expect("encode document");
    assert!(encoded.ends_with(b"\n"));
    assert_eq!(
        decode_personal_worker_store_document(&encoded).expect("decode canonical document"),
        document
    );

    let mut alternate = b" ".to_vec();
    alternate.extend_from_slice(&encoded);
    let error = decode_personal_worker_store_document(&alternate)
        .expect_err("alternate JSON bytes must fail closed");
    assert_eq!(error.kind(), PersonalWorkerStoreErrorKind::CorruptState);

    let mut unknown: serde_json::Value =
        serde_json::from_slice(&encoded).expect("parse canonical JSON for fixture");
    unknown
        .as_object_mut()
        .expect("document object")
        .insert("unknown".to_owned(), serde_json::Value::Bool(true));
    let error = decode_personal_worker_store_document(
        serde_json::to_string_pretty(&unknown)
            .expect("encode unknown-field fixture")
            .as_bytes(),
    )
    .expect_err("unknown field must fail closed");
    assert_eq!(error.kind(), PersonalWorkerStoreErrorKind::CorruptState);
}

#[test]
fn exact_revisions_advance_with_bounded_consecutive_history() {
    let mut document = PersonalWorkerStoreDocument::new(empty_queue(1, 2_000_000), vec![])
        .expect("initial document");
    for step in 2..=41 {
        document = document
            .advance(empty_queue(step, 2_000_000 + step), vec![])
            .expect("advance document");
    }
    assert_eq!(document.revision().get(), 41);
    assert_eq!(
        document.history().len(),
        MAX_PERSONAL_WORKER_HISTORY_ENTRIES
    );
    assert_eq!(document.history()[0].revision().get(), 9);
    assert_eq!(
        document
            .history()
            .last()
            .expect("last history")
            .revision()
            .get(),
        40
    );

    let skipped = document.advance(empty_queue(43, 3_000_000), vec![]);
    assert_eq!(
        skipped.expect_err("skipped generation").kind(),
        PersonalWorkerStoreErrorKind::RevisionConflict
    );
}

#[test]
fn durable_cache_lease_binds_the_exact_active_reservation() {
    let (queue, lease) = active_queue(1, 3_000_000);
    PersonalWorkerStoreDocument::new(queue.clone(), vec![lease.clone()])
        .expect("matching durable lease");

    let mismatched = PersonalWorkerDurableCacheLease::new(
        lease.request_id().clone(),
        lease.namespace().clone(),
        lease.access(),
        ReservationId::parse("reservation-other").expect("mismatched reservation ID"),
        lease.reservation_generation(),
        lease.acquired_at(),
    );
    let error = PersonalWorkerStoreDocument::new(queue, vec![mismatched])
        .expect_err("mismatched reservation binding");
    assert_eq!(error.kind(), PersonalWorkerStoreErrorKind::InvalidDocument);
}

#[test]
fn durable_store_reopens_and_replaces_only_the_expected_revision() {
    let root = TempRoot::new("replace");
    let (mut store, recovery) =
        UnixPersonalWorkerStore::open_or_create(root.path()).expect("open store");
    assert_eq!(
        recovery.disposition(),
        PersonalWorkerStoreRecoveryDisposition::Clean
    );
    let initial = PersonalWorkerStoreDocument::new(empty_queue(1, 4_000_000), vec![])
        .expect("initial document");
    let created = store.create(&initial).expect("create durable state");
    assert_eq!(
        created.disposition(),
        PersonalWorkerStoreWriteDisposition::Created
    );
    drop(store);

    let (mut reopened, recovery) =
        UnixPersonalWorkerStore::open_or_create(root.path()).expect("reopen store");
    assert_eq!(
        recovery.disposition(),
        PersonalWorkerStoreRecoveryDisposition::Clean
    );
    assert_eq!(reopened.load().expect("load state"), Some(initial.clone()));

    let next = initial
        .advance(empty_queue(2, 4_000_001), vec![])
        .expect("next document");
    let error = reopened
        .replace_if_revision(
            PersonalWorkerStoreRevision::new(2).expect("stale revision"),
            &next,
        )
        .expect_err("stale expected revision");
    assert_eq!(error.kind(), PersonalWorkerStoreErrorKind::RevisionConflict);
    let replaced = reopened
        .replace_if_revision(initial.revision(), &next)
        .expect("replace exact revision");
    assert_eq!(
        replaced.disposition(),
        PersonalWorkerStoreWriteDisposition::Replaced
    );
    assert_eq!(reopened.load().expect("load replacement"), Some(next));
}

#[test]
fn recovery_publishes_an_exact_successor_and_removes_a_stale_stage() {
    let root = TempRoot::new("recovery");
    let (mut store, _) = UnixPersonalWorkerStore::open_or_create(root.path()).expect("open store");
    let initial = PersonalWorkerStoreDocument::new(empty_queue(1, 5_000_000), vec![])
        .expect("initial document");
    store.create(&initial).expect("create initial state");
    let next = initial
        .advance(empty_queue(2, 5_000_001), vec![])
        .expect("next document");
    drop(store);

    let staged_path = root.store_directory().join(".next.json");
    fs::write(
        &staged_path,
        encode_personal_worker_store_document(&next).expect("encode next state"),
    )
    .expect("write staged successor");
    fs::set_permissions(&staged_path, fs::Permissions::from_mode(0o600)).expect("set staged mode");

    let (store, recovery) =
        UnixPersonalWorkerStore::open_or_create(root.path()).expect("recover successor");
    assert_eq!(
        recovery.disposition(),
        PersonalWorkerStoreRecoveryDisposition::PublishedStaged
    );
    assert_eq!(
        store.load().expect("load recovered state"),
        Some(next.clone())
    );
    drop(store);

    fs::write(
        &staged_path,
        encode_personal_worker_store_document(&next).expect("encode stale stage"),
    )
    .expect("write stale stage");
    fs::set_permissions(&staged_path, fs::Permissions::from_mode(0o600))
        .expect("set stale staged mode");
    let (store, recovery) =
        UnixPersonalWorkerStore::open_or_create(root.path()).expect("remove stale stage");
    assert_eq!(
        recovery.disposition(),
        PersonalWorkerStoreRecoveryDisposition::RemovedStaleStaged
    );
    assert_eq!(store.load().expect("load current state"), Some(next));
    assert!(!staged_path.exists());
}

#[test]
fn a_held_writer_lock_returns_busy_without_blocking() {
    let root = TempRoot::new("busy");
    let (mut store, _) = UnixPersonalWorkerStore::open_or_create(root.path()).expect("open store");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.store_directory().join("store.lock"))
        .expect("open lock file");
    flock(&lock, FlockOperation::NonBlockingLockExclusive).expect("hold writer lock");
    let error = store.recover().expect_err("busy store");
    assert_eq!(error.kind(), PersonalWorkerStoreErrorKind::Busy);
}

#[test]
fn unsafe_and_corrupt_filesystem_state_fails_closed() {
    let root = TempRoot::new("symlink");
    let target = root.path().join("redirected");
    fs::create_dir(&target).expect("create redirected directory");
    fs::set_permissions(&target, fs::Permissions::from_mode(0o750)).expect("set redirected mode");
    symlink(&target, root.store_directory()).expect("symlink store directory");
    let error = UnixPersonalWorkerStore::open_or_create(root.path())
        .expect_err("symlinked store directory");
    assert_eq!(error.kind(), PersonalWorkerStoreErrorKind::UnsafeFilesystem);

    let corrupt_root = TempRoot::new("corrupt");
    let (store, _) =
        UnixPersonalWorkerStore::open_or_create(corrupt_root.path()).expect("prepare store");
    drop(store);
    let current = corrupt_root.store_directory().join("current.json");
    fs::write(&current, b"not-json\n").expect("write corrupt current state");
    fs::set_permissions(&current, fs::Permissions::from_mode(0o600))
        .expect("set corrupt state mode");
    let error = UnixPersonalWorkerStore::open_or_create(corrupt_root.path())
        .expect_err("corrupt current state");
    assert_eq!(error.kind(), PersonalWorkerStoreErrorKind::CorruptState);
}

#[test]
fn bounded_revision_and_queue_generation_spaces_fail_closed() {
    let exhausted = PersonalWorkerStoreRevision::new(1_000_000_000_000)
        .expect("maximum store revision")
        .next()
        .expect_err("store revision exhaustion");
    assert_eq!(
        exhausted.kind(),
        PersonalWorkerStoreErrorKind::RevisionConflict
    );

    let document =
        PersonalWorkerStoreDocument::new(empty_queue(1_000_000_000_000, 6_000_000), vec![])
            .expect("maximum queue generation document");
    let error = document
        .advance(empty_queue(1_000_000_000_000, 6_000_001), vec![])
        .expect_err("queue generation exhaustion");
    assert_eq!(error.kind(), PersonalWorkerStoreErrorKind::RevisionConflict);
}

#[test]
fn canonical_decode_rejects_missing_or_skipped_history() {
    let initial = PersonalWorkerStoreDocument::new(empty_queue(1, 7_000_000), vec![])
        .expect("initial document");
    let next = initial
        .advance(empty_queue(2, 7_000_001), vec![])
        .expect("next document");
    let encoded =
        encode_personal_worker_store_document(&next).expect("encode revision with history");

    let mut missing: serde_json::Value =
        serde_json::from_slice(&encoded).expect("parse missing-history fixture");
    missing["history"] = serde_json::Value::Array(vec![]);
    let mut missing_bytes =
        serde_json::to_vec_pretty(&missing).expect("encode missing-history fixture");
    missing_bytes.push(b'\n');
    let error =
        decode_personal_worker_store_document(&missing_bytes).expect_err("missing bounded history");
    assert_eq!(error.kind(), PersonalWorkerStoreErrorKind::CorruptState);

    let mut skipped: serde_json::Value =
        serde_json::from_slice(&encoded).expect("parse skipped-generation fixture");
    skipped["queue"]["generation"] = serde_json::Value::from(3_u64);
    let mut skipped_bytes =
        serde_json::to_vec_pretty(&skipped).expect("encode skipped-generation fixture");
    skipped_bytes.push(b'\n');
    let error = decode_personal_worker_store_document(&skipped_bytes)
        .expect_err("skipped current queue generation");
    assert_eq!(error.kind(), PersonalWorkerStoreErrorKind::CorruptState);
}
