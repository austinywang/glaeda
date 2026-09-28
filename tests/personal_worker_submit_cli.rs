#![cfg(unix)]

use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use glaeda::artifact::{CommitId, GitTreeId, RepositoryRef, Sha256Digest};
use glaeda::execution_admission::{
    DrainAcknowledgement, EpochMillis, ExecutionAdmissionIdentity, ExecutionAdmissionInput,
    ExecutionAdmissionRecord, ExecutionAdmissionState, ExecutionRequestId, ExecutionResourceLimits,
    FallbackProfileEligibility, HostCapacityObservation, ReservationEvidence,
    ReservationGeneration, ReservationId, RunnerProfileId, UnavailableReason,
};
use glaeda::personal_worker_queue::{
    PersonalWorkerActiveReservation, PersonalWorkerActivityEvidence, PersonalWorkerCacheAccessMode,
    PersonalWorkerCacheNamespace, PersonalWorkerCancellationState, PersonalWorkerJobRequest,
    PersonalWorkerPriority, PersonalWorkerProfile, PersonalWorkerProfileObservation,
    PersonalWorkerQueueGeneration, PersonalWorkerQueueInput, PersonalWorkerSourceIdentity,
};
use glaeda::personal_worker_store::{
    PersonalWorkerDurableCacheLease, PersonalWorkerStore, PersonalWorkerStoreDocument,
    PersonalWorkerTerminalTombstone, encode_personal_worker_store_document,
};
use glaeda::unix_personal_worker_store::UnixPersonalWorkerStore;
use glaeda::verification_profile::{CacheId, VerificationProfileId};
use rustix::fs::{FlockOperation, flock};

const GIB: u64 = 1_024 * 1_024 * 1_024;
const BASE: u64 = 10_000_000;
static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

struct TempRoot(PathBuf);

impl TempRoot {
    fn new(label: &str) -> Self {
        let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "smolrunner-personal-worker-submit-cli-{label}-{}-{sequence}",
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

#[derive(Clone)]
struct SubmitArgs {
    revision: String,
    generation: String,
    observed_at: String,
    request_id: String,
    verification_profile: String,
    runner_profile: String,
    repository: String,
    commit: String,
    tree: String,
    priority: String,
    cpu_millis: String,
    memory_bytes: String,
    pids: String,
    cache_id: String,
    cache_namespace_digest: String,
    cache_access: String,
    submitted_at: String,
    operator_deadline: Option<String>,
}

impl SubmitArgs {
    fn exact(request_id: &str) -> Self {
        Self {
            revision: "1".to_owned(),
            generation: "1".to_owned(),
            observed_at: (BASE + 1_000).to_string(),
            request_id: request_id.to_owned(),
            verification_profile: "smolrunner.required".to_owned(),
            runner_profile: "personal-lima-work".to_owned(),
            repository: "example/project".to_owned(),
            commit: "a".repeat(40),
            tree: "b".repeat(40),
            priority: "normal".to_owned(),
            cpu_millis: "2000".to_owned(),
            memory_bytes: (2 * GIB).to_string(),
            pids: "2048".to_owned(),
            cache_id: "build-cache".to_owned(),
            cache_namespace_digest: format!("sha256:{}", "ab".repeat(32)),
            cache_access: "write".to_owned(),
            submitted_at: (BASE - 1_000).to_string(),
            operator_deadline: Some((BASE + 60_000).to_string()),
        }
    }

    fn command(&self, root: &TempRoot, output: &str) -> Vec<OsString> {
        let mut arguments = vec![
            OsString::from("--output"),
            OsString::from(output),
            OsString::from("queue"),
            OsString::from("submit"),
            OsString::from("--store-root"),
            root.path().as_os_str().to_owned(),
            OsString::from("--revision"),
            OsString::from(&self.revision),
            OsString::from("--generation"),
            OsString::from(&self.generation),
            OsString::from("--observed-at"),
            OsString::from(&self.observed_at),
            OsString::from("--request-id"),
            OsString::from(&self.request_id),
            OsString::from("--verification-profile"),
            OsString::from(&self.verification_profile),
            OsString::from("--runner-profile"),
            OsString::from(&self.runner_profile),
            OsString::from("--repository"),
            OsString::from(&self.repository),
            OsString::from("--commit"),
            OsString::from(&self.commit),
            OsString::from("--tree"),
            OsString::from(&self.tree),
            OsString::from("--priority"),
            OsString::from(&self.priority),
            OsString::from("--cpu-millis"),
            OsString::from(&self.cpu_millis),
            OsString::from("--memory-bytes"),
            OsString::from(&self.memory_bytes),
            OsString::from("--pids"),
            OsString::from(&self.pids),
            OsString::from("--cache-id"),
            OsString::from(&self.cache_id),
            OsString::from("--cache-namespace-digest"),
            OsString::from(&self.cache_namespace_digest),
            OsString::from("--cache-access"),
            OsString::from(&self.cache_access),
            OsString::from("--submitted-at"),
            OsString::from(&self.submitted_at),
        ];
        if let Some(deadline) = &self.operator_deadline {
            arguments.push(OsString::from("--operator-deadline"));
            arguments.push(OsString::from(deadline));
        }
        arguments
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

fn request(id: &str) -> PersonalWorkerJobRequest {
    let repository = RepositoryRef::parse("example/project").expect("repository");
    PersonalWorkerJobRequest {
        identity: identity(id),
        source: PersonalWorkerSourceIdentity::new(
            repository.clone(),
            CommitId::parse(&"a".repeat(40)).expect("commit"),
            GitTreeId::parse(&"b".repeat(40)).expect("tree"),
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
        submitted_at: time(BASE - 1_000),
        operator_deadline: Some(time(BASE + 60_000)),
        cancellation: PersonalWorkerCancellationState::Active,
        fallback_eligibility: FallbackProfileEligibility::ineligible(),
    }
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

fn empty_document() -> PersonalWorkerStoreDocument {
    PersonalWorkerStoreDocument::new(empty_queue(1, BASE), vec![]).expect("empty document")
}

fn active_document() -> PersonalWorkerStoreDocument {
    let request = request("occupied-one");
    let reservation_id = ReservationId::parse("reservation-occupied-one").expect("reservation ID");
    let generation = ReservationGeneration::new(7).expect("reservation generation");
    let reserved_at = time(BASE - 500);
    let admission = ExecutionAdmissionRecord::from_input(ExecutionAdmissionInput {
        identity: request.identity.clone(),
        state: ExecutionAdmissionState::Running,
        observed_at: time(BASE),
        requested_limits: request.requested_limits,
        host_capacity: Some(HostCapacityObservation::new(
            time(BASE - 500),
            ExecutionResourceLimits::new(8_000, 10 * GIB, 10_000).expect("capacity"),
        )),
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
                started_at: Some(time(BASE - 250)),
            }],
            pending_profile_change: None,
        },
        vec![lease],
    )
    .expect("active document")
}

fn terminal_document() -> PersonalWorkerStoreDocument {
    let request = request("terminal-one");
    let reservation_id = ReservationId::parse("reservation-terminal-one").expect("reservation ID");
    let generation = ReservationGeneration::new(11).expect("reservation generation");
    let reserved_at = time(BASE - 500);
    let terminal_admission = ExecutionAdmissionRecord::from_input(ExecutionAdmissionInput {
        identity: request.identity.clone(),
        state: ExecutionAdmissionState::Unavailable,
        observed_at: time(BASE),
        requested_limits: request.requested_limits,
        host_capacity: Some(HostCapacityObservation::new(
            time(BASE - 500),
            ExecutionResourceLimits::new(8_000, 10 * GIB, 10_000).expect("capacity"),
        )),
        applied_limits: Some(request.requested_limits),
        queue_position: None,
        reservation: Some(ReservationEvidence::new(
            reservation_id.clone(),
            generation,
            reserved_at,
            time(BASE + 60_000),
        )),
        acknowledgement: Some(DrainAcknowledgement::Drain),
        fallback_eligibility: FallbackProfileEligibility::ineligible(),
        unavailable_reason: Some(UnavailableReason::Drained),
    })
    .expect("terminal admission");
    let lease = PersonalWorkerDurableCacheLease::new(
        request.identity.request_id.clone(),
        request.cache_namespace.clone(),
        request.cache_access,
        reservation_id,
        generation,
        reserved_at,
    );
    let tombstone = PersonalWorkerTerminalTombstone::new(
        request,
        terminal_admission,
        Some(time(BASE - 250)),
        lease,
    )
    .expect("terminal tombstone");
    PersonalWorkerStoreDocument::new_with_terminal_tombstones(
        empty_queue(1, BASE),
        vec![],
        vec![tombstone],
    )
    .expect("terminal document")
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

fn run_glaeda(arguments: &[OsString]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_glaeda"))
        .args(arguments)
        .output()
        .expect("run installed glaeda binary")
}

fn json_output(output: &Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).expect("JSON command output")
}

fn current_bytes(root: &TempRoot) -> Vec<u8> {
    fs::read(root.current_document()).expect("read current document")
}

#[test]
fn submission_applies_once_replays_and_is_readable() {
    let root = TempRoot::new("applied");
    create_store(&root, &empty_document());
    let mut arguments = SubmitArgs::exact("request-one");

    let applied = run_glaeda(&arguments.command(&root, "json"));
    assert!(applied.status.success(), "{:?}", applied.stderr);
    let applied_json = json_output(&applied);
    assert_eq!(applied_json["disposition"], "applied");
    assert_eq!(applied_json["mutation"], "submit");
    assert_eq!(applied_json["old_revision"], 1);
    assert_eq!(applied_json["new_revision"], 2);
    assert_eq!(applied_json["old_queue_generation"], 1);
    assert_eq!(applied_json["new_queue_generation"], 2);

    let document = load_store(&root);
    assert_eq!(document.revision().get(), 2);
    assert_eq!(document.queue().generation.get(), 2);
    assert_eq!(document.queue().queued, vec![request("request-one")]);

    let queue = run_glaeda(&[
        OsString::from("--output"),
        OsString::from("json"),
        OsString::from("queue"),
        OsString::from("list"),
        OsString::from("--store-root"),
        root.path().as_os_str().to_owned(),
        OsString::from("--revision"),
        OsString::from("2"),
        OsString::from("--generation"),
        OsString::from("2"),
    ]);
    assert!(queue.status.success(), "{:?}", queue.stderr);
    let queue_json = json_output(&queue);
    assert_eq!(queue_json["total"], 1);
    assert_eq!(queue_json["items"][0]["request_id"], "request-one");

    let job = run_glaeda(&[
        OsString::from("--output"),
        OsString::from("json"),
        OsString::from("job"),
        OsString::from("show"),
        OsString::from("--store-root"),
        root.path().as_os_str().to_owned(),
        OsString::from("--revision"),
        OsString::from("2"),
        OsString::from("--generation"),
        OsString::from("2"),
        OsString::from("request-one"),
    ]);
    assert!(job.status.success(), "{:?}", job.stderr);
    assert_eq!(json_output(&job)["job"]["state"], "queued");

    let before_duplicate = current_bytes(&root);
    arguments.revision = "2".to_owned();
    arguments.generation = "2".to_owned();
    let duplicate = run_glaeda(&arguments.command(&root, "json"));
    assert!(duplicate.status.success(), "{:?}", duplicate.stderr);
    let duplicate_json = json_output(&duplicate);
    assert_eq!(duplicate_json["disposition"], "duplicate");
    assert_eq!(duplicate_json["old_revision"], 2);
    assert_eq!(duplicate_json["new_revision"], 2);
    assert_eq!(duplicate_json["old_queue_generation"], 2);
    assert_eq!(duplicate_json["new_queue_generation"], 2);
    assert_eq!(current_bytes(&root), before_duplicate);

    let human = run_glaeda(&arguments.command(&root, "human"));
    assert!(human.status.success());
    let human = String::from_utf8(human.stdout).expect("human output");
    assert!(human.contains("Personal worker submission"));
    assert!(human.contains("disposition: duplicate"));
    assert!(human.contains("store revision: 2 -> 2"));
}

#[test]
fn conflicts_stale_expectations_and_invalid_inputs_are_bounded() {
    let root = TempRoot::new("errors");
    create_store(&root, &empty_document());
    let applied_args = SubmitArgs::exact("request-one");
    assert!(
        run_glaeda(&applied_args.command(&root, "json"))
            .status
            .success()
    );
    let unchanged = current_bytes(&root);

    let mut conflict = applied_args.clone();
    conflict.revision = "2".to_owned();
    conflict.generation = "2".to_owned();
    conflict.commit = "c".repeat(40);
    let output = run_glaeda(&conflict.command(&root, "json"));
    assert!(!output.status.success());
    assert_eq!(json_output(&output)["kind"], "conflict");
    assert_eq!(current_bytes(&root), unchanged);

    for (revision, generation, expected_kind) in [
        ("1", "2", "stale_revision"),
        ("2", "1", "stale_queue_generation"),
    ] {
        let mut args = applied_args.clone();
        args.revision = revision.to_owned();
        args.generation = generation.to_owned();
        args.request_id = "request-two".to_owned();
        let output = run_glaeda(&args.command(&root, "json"));
        assert!(!output.status.success());
        assert_eq!(json_output(&output)["kind"], expected_kind);
        assert_eq!(current_bytes(&root), unchanged);
    }

    let invalid_cases = [
        ("priority", "private-priority", "invalid_priority"),
        ("cache_access", "private-access", "invalid_cache_access"),
        ("cache_digest", "private-digest", "invalid_cache_digest"),
        ("cpu", "0", "invalid_resources"),
        ("observed_at", "0", "invalid_observation_time"),
        ("submitted_at", "0", "invalid_submission_time"),
        ("repository", "private repository", "invalid_repository"),
        ("commit", "private-commit", "invalid_commit"),
    ];
    for (field, value, expected_kind) in invalid_cases {
        let mut args = SubmitArgs::exact("request-invalid");
        match field {
            "priority" => args.priority = value.to_owned(),
            "cache_access" => args.cache_access = value.to_owned(),
            "cache_digest" => args.cache_namespace_digest = value.to_owned(),
            "cpu" => args.cpu_millis = value.to_owned(),
            "observed_at" => args.observed_at = value.to_owned(),
            "submitted_at" => args.submitted_at = value.to_owned(),
            "repository" => args.repository = value.to_owned(),
            "commit" => args.commit = value.to_owned(),
            _ => unreachable!(),
        }
        let output = run_glaeda(&args.command(&root, "json"));
        assert!(!output.status.success());
        let public = String::from_utf8(output.stdout).expect("public JSON");
        let json: serde_json::Value = serde_json::from_str(&public).expect("error JSON");
        assert_eq!(json["kind"], expected_kind);
        assert!(!public.contains(value));
        assert!(!public.contains(root.path().to_string_lossy().as_ref()));
        assert_eq!(current_bytes(&root), unchanged);
    }
}

#[test]
fn existing_active_and_terminal_identities_do_not_create_queue_entries() {
    let active_root = TempRoot::new("active");
    create_store(&active_root, &active_document());
    let active_before = current_bytes(&active_root);
    let exact_active = run_glaeda(&SubmitArgs::exact("occupied-one").command(&active_root, "json"));
    assert!(exact_active.status.success());
    assert_eq!(json_output(&exact_active)["disposition"], "duplicate");
    assert_eq!(current_bytes(&active_root), active_before);
    assert!(load_store(&active_root).queue().queued.is_empty());

    let mut conflicting_active = SubmitArgs::exact("occupied-one");
    conflicting_active.commit = "c".repeat(40);
    let output = run_glaeda(&conflicting_active.command(&active_root, "json"));
    assert!(!output.status.success());
    assert_eq!(json_output(&output)["kind"], "conflict");
    assert_eq!(current_bytes(&active_root), active_before);

    let terminal_root = TempRoot::new("terminal");
    create_store(&terminal_root, &terminal_document());
    let terminal_before = current_bytes(&terminal_root);
    let terminal = run_glaeda(&SubmitArgs::exact("terminal-one").command(&terminal_root, "json"));
    assert!(!terminal.status.success());
    assert_eq!(json_output(&terminal)["kind"], "conflict");
    assert_eq!(current_bytes(&terminal_root), terminal_before);
    assert!(load_store(&terminal_root).queue().queued.is_empty());
}

#[test]
fn missing_store_busy_lock_and_staged_recovery_follow_existing_contracts() {
    let missing = TempRoot::new("missing");
    let missing_output = run_glaeda(&SubmitArgs::exact("missing-one").command(&missing, "json"));
    assert!(!missing_output.status.success());
    assert_eq!(json_output(&missing_output)["kind"], "missing_store");
    assert!(!missing.store_directory().exists());

    let busy = TempRoot::new("busy");
    create_store(&busy, &empty_document());
    let before_busy = current_bytes(&busy);
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(busy.store_directory().join("store.lock"))
        .expect("open writer lock");
    flock(&lock, FlockOperation::NonBlockingLockExclusive).expect("hold writer lock");
    let busy_output = run_glaeda(&SubmitArgs::exact("busy-one").command(&busy, "json"));
    assert!(!busy_output.status.success());
    assert_eq!(json_output(&busy_output)["kind"], "busy");
    assert_eq!(current_bytes(&busy), before_busy);
    drop(lock);

    let recovered = TempRoot::new("recovery");
    let initial = empty_document();
    create_store(&recovered, &initial);
    let staged = initial
        .advance(empty_queue(2, BASE + 100), vec![])
        .expect("staged successor");
    let staged_path = recovered.store_directory().join(".next.json");
    fs::write(
        &staged_path,
        encode_personal_worker_store_document(&staged).expect("encode staged successor"),
    )
    .expect("write staged successor");
    fs::set_permissions(&staged_path, fs::Permissions::from_mode(0o600)).expect("set staged mode");

    let mut args = SubmitArgs::exact("recovered-one");
    args.revision = "2".to_owned();
    args.generation = "2".to_owned();
    args.observed_at = (BASE + 1_000).to_string();
    let output = run_glaeda(&args.command(&recovered, "json"));
    assert!(output.status.success(), "{:?}", output.stderr);
    let json = json_output(&output);
    assert_eq!(json["old_revision"], 2);
    assert_eq!(json["new_revision"], 3);
    assert_eq!(json["old_queue_generation"], 2);
    assert_eq!(json["new_queue_generation"], 3);
    let document = load_store(&recovered);
    assert_eq!(document.revision().get(), 3);
    assert_eq!(document.queue().generation.get(), 3);
    assert_eq!(
        document.queue().queued[0].identity.request_id.as_str(),
        "recovered-one"
    );
    assert!(!staged_path.exists());
}
