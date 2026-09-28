#![cfg(unix)]

use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
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
    PersonalWorkerDurableCacheLease, PersonalWorkerStore, PersonalWorkerStoreDocument,
    encode_personal_worker_store_document,
};
use glaeda::unix_personal_worker_store::UnixPersonalWorkerStore;
use glaeda::verification_profile::{CacheId, VerificationProfileId};
use rustix::fs::{FlockOperation, flock};

const GIB: u64 = 1_024 * 1_024 * 1_024;
const BASE: u64 = 9_000_000;
const CANCELLED_AT: u64 = BASE + 1_000;
static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

struct TempRoot(PathBuf);

impl TempRoot {
    fn new(label: &str) -> Self {
        let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "smolrunner-personal-worker-cancel-cli-{label}-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create temporary state root");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o750)).expect("set root mode");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn store_directory(&self) -> PathBuf {
        self.0.join("personal-worker")
    }

    fn current_document(&self) -> PathBuf {
        self.store_directory().join("current.json")
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

fn limits() -> ExecutionResourceLimits {
    ExecutionResourceLimits::new(2_000, 2 * GIB, 2_048).expect("limits")
}

fn identity(id: &str) -> ExecutionAdmissionIdentity {
    ExecutionAdmissionIdentity::new(
        ExecutionRequestId::parse(id).expect("request ID"),
        VerificationProfileId::parse("smolrunner.required").expect("verification profile"),
        RunnerProfileId::parse("personal-lima-work").expect("runner profile"),
    )
}

fn request(id: &str, repository: &str, digit: char) -> PersonalWorkerJobRequest {
    let repository = RepositoryRef::parse(repository).expect("repository");
    PersonalWorkerJobRequest {
        identity: identity(id),
        source: PersonalWorkerSourceIdentity::new(
            repository.clone(),
            CommitId::parse(&digit.to_string().repeat(40)).expect("commit"),
            GitTreeId::parse(&digit.to_string().repeat(40)).expect("tree"),
        ),
        priority: PersonalWorkerPriority::Normal,
        requested_limits: limits(),
        cache_namespace: PersonalWorkerCacheNamespace::RepositoryBuild {
            cache_id: CacheId::parse("build-cache").expect("cache ID"),
            repository,
            namespace_digest: Sha256Digest::parse(&format!("sha256:{}", "ab".repeat(32)))
                .expect("namespace digest"),
        },
        cache_access: PersonalWorkerCacheAccessMode::Write,
        submitted_at: time(BASE - 10_000),
        operator_deadline: None,
        cancellation: PersonalWorkerCancellationState::Active,
        fallback_eligibility: FallbackProfileEligibility::ineligible(),
    }
}

fn queued_document() -> PersonalWorkerStoreDocument {
    PersonalWorkerStoreDocument::new(
        PersonalWorkerQueueInput {
            generation: PersonalWorkerQueueGeneration::new(1).expect("queue generation"),
            observed_at: time(BASE),
            profile_observation: PersonalWorkerProfileObservation::observed(
                PersonalWorkerProfile::Interactive,
            ),
            activity_evidence: PersonalWorkerActivityEvidence::observed(time(BASE - 1_000)),
            queued: vec![request("queued-one", "example/queued", 'a')],
            active: vec![],
            pending_profile_change: None,
        },
        vec![],
    )
    .expect("queued document")
}

fn active_document() -> PersonalWorkerStoreDocument {
    let request = request("active-one", "example/active", 'b');
    let reservation_id = ReservationId::parse("reservation-active-one").expect("reservation ID");
    let generation = ReservationGeneration::new(7).expect("reservation generation");
    let reserved_at = time(BASE - 5_000);
    let admission = ExecutionAdmissionRecord::from_input(ExecutionAdmissionInput {
        identity: request.identity.clone(),
        state: ExecutionAdmissionState::Running,
        observed_at: time(BASE),
        requested_limits: request.requested_limits,
        host_capacity: Some(HostCapacityObservation::new(time(BASE - 5_000), limits())),
        applied_limits: Some(request.requested_limits),
        queue_position: None,
        reservation: Some(ReservationEvidence::new(
            reservation_id.clone(),
            generation,
            reserved_at,
            time(BASE + 60_000),
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
        generation,
        reserved_at,
    );
    PersonalWorkerStoreDocument::new(
        PersonalWorkerQueueInput {
            generation: PersonalWorkerQueueGeneration::new(1).expect("queue generation"),
            observed_at: time(BASE),
            profile_observation: PersonalWorkerProfileObservation::observed(
                PersonalWorkerProfile::Work,
            ),
            activity_evidence: PersonalWorkerActivityEvidence::observed(time(BASE)),
            queued: vec![],
            active: vec![PersonalWorkerActiveReservation {
                request,
                admission,
                started_at: Some(time(BASE - 2_000)),
            }],
            pending_profile_change: None,
        },
        vec![lease],
    )
    .expect("active document")
}

fn create_store(root: &TempRoot, document: &PersonalWorkerStoreDocument) {
    let (mut store, _) = UnixPersonalWorkerStore::open_or_create(root.path()).expect("open store");
    store.create(document).expect("create durable document");
}

fn load_store(root: &TempRoot) -> PersonalWorkerStoreDocument {
    UnixPersonalWorkerStore::open_existing_read_only(root.path())
        .expect("open existing store")
        .load()
        .expect("load durable state")
        .expect("current document")
}

fn run_glaeda(arguments: &[&OsStr]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_glaeda"))
        .args(arguments)
        .output()
        .expect("run installed glaeda binary")
}

fn cancel_arguments<'a>(
    root: &'a TempRoot,
    revision: &'a str,
    generation: &'a str,
    cancelled_at: &'a str,
    request_id: &'a str,
) -> Vec<&'a OsStr> {
    vec![
        OsStr::new("--output"),
        OsStr::new("json"),
        OsStr::new("job"),
        OsStr::new("cancel"),
        OsStr::new("--store-root"),
        root.path().as_os_str(),
        OsStr::new("--revision"),
        OsStr::new(revision),
        OsStr::new("--generation"),
        OsStr::new(generation),
        OsStr::new("--cancelled-at"),
        OsStr::new(cancelled_at),
        OsStr::new(request_id),
    ]
}

fn json_output(output: &Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).expect("JSON command output")
}

fn current_bytes(root: &TempRoot) -> Vec<u8> {
    fs::read(root.current_document()).expect("read current document")
}

#[test]
fn queued_cancellation_applies_once_and_exact_replay_is_duplicate() {
    let root = TempRoot::new("queued");
    create_store(&root, &queued_document());

    let applied = run_glaeda(&cancel_arguments(
        &root,
        "1",
        "1",
        &CANCELLED_AT.to_string(),
        "queued-one",
    ));
    assert!(applied.status.success(), "{:?}", applied.stderr);
    let applied_json = json_output(&applied);
    assert_eq!(applied_json["disposition"], "applied");
    assert_eq!(applied_json["mutation"], "cancel");
    assert_eq!(applied_json["old_revision"], 1);
    assert_eq!(applied_json["new_revision"], 2);
    assert_eq!(applied_json["old_queue_generation"], 1);
    assert_eq!(applied_json["new_queue_generation"], 2);

    let document = load_store(&root);
    assert_eq!(document.revision().get(), 2);
    assert_eq!(document.queue().generation.get(), 2);
    assert_eq!(
        document.queue().queued[0].cancellation,
        PersonalWorkerCancellationState::Cancelled {
            cancelled_at: time(CANCELLED_AT),
        }
    );

    let before_duplicate = current_bytes(&root);
    let duplicate = run_glaeda(&cancel_arguments(
        &root,
        "2",
        "2",
        &CANCELLED_AT.to_string(),
        "queued-one",
    ));
    assert!(duplicate.status.success(), "{:?}", duplicate.stderr);
    let duplicate_json = json_output(&duplicate);
    assert_eq!(duplicate_json["disposition"], "duplicate");
    assert_eq!(duplicate_json["old_revision"], 2);
    assert_eq!(duplicate_json["new_revision"], 2);
    assert_eq!(duplicate_json["old_queue_generation"], 2);
    assert_eq!(duplicate_json["new_queue_generation"], 2);
    assert_eq!(current_bytes(&root), before_duplicate);
}

#[test]
fn conflicts_stale_expectations_and_missing_identity_are_bounded() {
    let root = TempRoot::new("errors");
    create_store(&root, &queued_document());
    let applied = run_glaeda(&cancel_arguments(
        &root,
        "1",
        "1",
        &CANCELLED_AT.to_string(),
        "queued-one",
    ));
    assert!(applied.status.success());
    let unchanged = current_bytes(&root);

    for (revision, generation, cancelled_at, request_id, expected_kind) in [
        ("2", "2", "9002000", "queued-one", "conflict"),
        ("1", "2", "9001000", "queued-one", "stale_revision"),
        ("2", "1", "9001000", "queued-one", "stale_queue_generation"),
        ("2", "2", "9001000", "missing-one", "not_found"),
    ] {
        let output = run_glaeda(&cancel_arguments(
            &root,
            revision,
            generation,
            cancelled_at,
            request_id,
        ));
        assert!(!output.status.success());
        assert_eq!(json_output(&output)["kind"], expected_kind);
        assert_eq!(current_bytes(&root), unchanged);
    }
}

#[test]
fn active_cancellation_is_refused_without_drain_evidence() {
    let root = TempRoot::new("active");
    create_store(&root, &active_document());
    let before = current_bytes(&root);

    let output = run_glaeda(&cancel_arguments(
        &root,
        "1",
        "1",
        &CANCELLED_AT.to_string(),
        "active-one",
    ));
    assert!(!output.status.success());
    let json = json_output(&output);
    assert_eq!(json["kind"], "invalid_mutation");
    assert_eq!(
        json["message"],
        "active cancellation requires exact draining admission evidence"
    );
    assert_eq!(current_bytes(&root), before);
}

#[test]
fn missing_state_invalid_inputs_and_lock_contention_do_not_mutate() {
    let missing = TempRoot::new("missing");
    let missing_output = run_glaeda(&cancel_arguments(
        &missing,
        "1",
        "1",
        &CANCELLED_AT.to_string(),
        "queued-one",
    ));
    assert!(!missing_output.status.success());
    assert_eq!(json_output(&missing_output)["kind"], "missing_store");
    assert!(!missing.store_directory().exists());

    let root = TempRoot::new("invalid-and-busy");
    create_store(&root, &queued_document());
    let before = current_bytes(&root);

    for (cancelled_at, request_id, expected_kind) in [
        ("0", "queued-one", "invalid_cancellation_time"),
        ("9001000", "../private-id", "invalid_request_id"),
    ] {
        let output = run_glaeda(&cancel_arguments(&root, "1", "1", cancelled_at, request_id));
        assert!(!output.status.success());
        let text = String::from_utf8(output.stdout).expect("UTF-8 error JSON");
        let json: serde_json::Value = serde_json::from_str(&text).expect("error JSON");
        assert_eq!(json["kind"], expected_kind);
        assert!(!text.contains(root.path().to_string_lossy().as_ref()));
        assert!(!text.contains("../private-id"));
        assert_eq!(current_bytes(&root), before);
    }

    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.store_directory().join("store.lock"))
        .expect("open writer lock");
    flock(&lock, FlockOperation::NonBlockingLockExclusive).expect("hold writer lock");
    let busy = run_glaeda(&cancel_arguments(
        &root,
        "1",
        "1",
        &CANCELLED_AT.to_string(),
        "queued-one",
    ));
    assert!(!busy.status.success());
    assert_eq!(json_output(&busy)["kind"], "busy");
    assert_eq!(current_bytes(&root), before);
}

#[test]
fn staged_successor_is_recovered_before_cancellation() {
    let root = TempRoot::new("recovery");
    let current = queued_document();
    create_store(&root, &current);
    let staged = current
        .advance(
            PersonalWorkerQueueInput {
                generation: PersonalWorkerQueueGeneration::new(2).expect("queue generation"),
                observed_at: time(BASE + 500),
                profile_observation: current.queue().profile_observation,
                activity_evidence: current.queue().activity_evidence,
                queued: current.queue().queued.clone(),
                active: current.queue().active.clone(),
                pending_profile_change: current.queue().pending_profile_change,
            },
            current.cache_leases().to_vec(),
        )
        .expect("valid staged successor");
    let staged_path = root.store_directory().join(".next.json");
    fs::write(
        &staged_path,
        encode_personal_worker_store_document(&staged).expect("encode staged successor"),
    )
    .expect("write staged successor");
    fs::set_permissions(&staged_path, fs::Permissions::from_mode(0o600)).expect("set staged mode");

    let output = run_glaeda(&cancel_arguments(
        &root,
        "2",
        "2",
        &CANCELLED_AT.to_string(),
        "queued-one",
    ));
    assert!(output.status.success(), "{:?}", output.stderr);
    let json = json_output(&output);
    assert_eq!(json["old_revision"], 2);
    assert_eq!(json["new_revision"], 3);
    assert_eq!(json["old_queue_generation"], 2);
    assert_eq!(json["new_queue_generation"], 3);
    assert!(!staged_path.exists());

    let document = load_store(&root);
    assert_eq!(document.revision().get(), 3);
    assert_eq!(document.queue().generation.get(), 3);
    assert_eq!(
        document.queue().queued[0].cancellation,
        PersonalWorkerCancellationState::Cancelled {
            cancelled_at: time(CANCELLED_AT),
        }
    );
}
