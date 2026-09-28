#![cfg(unix)]

use std::ffi::OsStr;
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
const BASE: u64 = 8_000_000;
static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

struct TempRoot(PathBuf);

impl TempRoot {
    fn new(label: &str) -> Self {
        let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "smolrunner-personal-worker-read-cli-{label}-{}-{sequence}",
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

fn namespace(repository: &str, digest_byte: &str) -> PersonalWorkerCacheNamespace {
    PersonalWorkerCacheNamespace::RepositoryBuild {
        cache_id: CacheId::parse("build-cache").expect("cache ID"),
        repository: RepositoryRef::parse(repository).expect("cache repository"),
        namespace_digest: Sha256Digest::parse(&format!("sha256:{}", digest_byte.repeat(32)))
            .expect("namespace digest"),
    }
}

fn request(id: &str, repository: &str, digit: char, submitted_at: u64) -> PersonalWorkerJobRequest {
    PersonalWorkerJobRequest {
        identity: identity(id),
        source: source(repository, digit),
        priority: PersonalWorkerPriority::Normal,
        requested_limits: limits(2_000, 2),
        cache_namespace: namespace(repository, if digit == 'a' { "ab" } else { "cd" }),
        cache_access: PersonalWorkerCacheAccessMode::Write,
        submitted_at: time(submitted_at),
        operator_deadline: None,
        cancellation: PersonalWorkerCancellationState::Active,
        fallback_eligibility: FallbackProfileEligibility::ineligible(),
    }
}

fn active_reservation() -> (
    PersonalWorkerActiveReservation,
    PersonalWorkerDurableCacheLease,
) {
    let request = request("active-one", "example/active", 'c', BASE - 90_000);
    let reservation_id = ReservationId::parse("reservation-active-one").expect("reservation ID");
    let generation = ReservationGeneration::new(7).expect("reservation generation");
    let reserved_at = time(BASE - 30_000);
    let admission = ExecutionAdmissionRecord::from_input(ExecutionAdmissionInput {
        identity: request.identity.clone(),
        state: ExecutionAdmissionState::Running,
        observed_at: time(BASE - 10_000),
        requested_limits: request.requested_limits,
        host_capacity: Some(HostCapacityObservation::new(
            time(BASE - 30_000),
            limits(8_000, 10),
        )),
        applied_limits: Some(request.requested_limits),
        queue_position: None,
        reservation: Some(ReservationEvidence::new(
            reservation_id.clone(),
            generation,
            reserved_at,
            time(BASE + 3_600_000),
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
    (
        PersonalWorkerActiveReservation {
            request,
            admission,
            started_at: Some(time(BASE - 20_000)),
        },
        lease,
    )
}

fn live_document() -> PersonalWorkerStoreDocument {
    let queued_one = request("queued-one", "example/queued-one", 'a', BASE - 120_000);
    let queued_two = request("queued-two", "example/queued-two", 'b', BASE - 110_000);
    let (active, lease) = active_reservation();
    PersonalWorkerStoreDocument::new(
        PersonalWorkerQueueInput {
            generation: PersonalWorkerQueueGeneration::new(1).expect("queue generation"),
            observed_at: time(BASE),
            profile_observation: PersonalWorkerProfileObservation::observed(
                PersonalWorkerProfile::Work,
            ),
            activity_evidence: PersonalWorkerActivityEvidence::observed(time(BASE - 1_000)),
            queued: vec![queued_one, queued_two],
            active: vec![active],
            pending_profile_change: None,
        },
        vec![lease],
    )
    .expect("live document")
}

fn terminal_tombstone() -> PersonalWorkerTerminalTombstone {
    let request = request("terminal-one", "example/terminal", 'd', BASE - 120_000);
    let reservation_id = ReservationId::parse("reservation-terminal-one").expect("reservation ID");
    let generation = ReservationGeneration::new(11).expect("reservation generation");
    let reserved_at = time(BASE - 30_000);
    let terminal_admission = ExecutionAdmissionRecord::from_input(ExecutionAdmissionInput {
        identity: request.identity.clone(),
        state: ExecutionAdmissionState::Unavailable,
        observed_at: time(BASE),
        requested_limits: request.requested_limits,
        host_capacity: Some(HostCapacityObservation::new(
            time(BASE - 1_000),
            limits(8_000, 10),
        )),
        applied_limits: Some(request.requested_limits),
        queue_position: None,
        reservation: Some(ReservationEvidence::new(
            reservation_id.clone(),
            generation,
            reserved_at,
            time(BASE + 3_600_000),
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
    PersonalWorkerTerminalTombstone::new(
        request,
        terminal_admission,
        Some(time(BASE - 20_000)),
        lease,
    )
    .expect("terminal tombstone")
}

fn terminal_document() -> PersonalWorkerStoreDocument {
    PersonalWorkerStoreDocument::new_with_terminal_tombstones(
        PersonalWorkerQueueInput {
            generation: PersonalWorkerQueueGeneration::new(1).expect("queue generation"),
            observed_at: time(BASE),
            profile_observation: PersonalWorkerProfileObservation::observed(
                PersonalWorkerProfile::Interactive,
            ),
            activity_evidence: PersonalWorkerActivityEvidence::observed(time(BASE)),
            queued: vec![],
            active: vec![],
            pending_profile_change: None,
        },
        vec![],
        vec![terminal_tombstone()],
    )
    .expect("terminal document")
}

fn create_store(root: &TempRoot, document: &PersonalWorkerStoreDocument) {
    let (mut store, _) = UnixPersonalWorkerStore::open_or_create(root.path()).expect("open store");
    store.create(document).expect("create durable document");
}

fn run_glaeda(arguments: &[&OsStr]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_glaeda"))
        .args(arguments)
        .output()
        .expect("run installed glaeda binary")
}

fn output_text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).expect("UTF-8 command output")
}

fn store_snapshot(root: &TempRoot) -> Vec<(String, Vec<u8>)> {
    let mut snapshot = fs::read_dir(root.store_directory())
        .expect("read store directory")
        .map(|entry| {
            let entry = entry.expect("store entry");
            let name = entry.file_name().to_string_lossy().into_owned();
            let bytes = fs::read(entry.path()).expect("read store file");
            (name, bytes)
        })
        .collect::<Vec<_>>();
    snapshot.sort_by(|left, right| left.0.cmp(&right.0));
    snapshot
}

#[test]
fn installed_cli_reads_status_queue_and_active_job_without_lock_or_writes() {
    let root = TempRoot::new("live");
    create_store(&root, &live_document());
    let before = store_snapshot(&root);

    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.store_directory().join("store.lock"))
        .expect("open writer lock");
    flock(&lock, FlockOperation::NonBlockingLockExclusive).expect("hold writer lock");

    let root_arg = root.path().as_os_str();
    let status = run_glaeda(&[
        OsStr::new("--output"),
        OsStr::new("json"),
        OsStr::new("worker"),
        OsStr::new("status"),
        OsStr::new("--store-root"),
        root_arg,
    ]);
    assert!(status.status.success(), "{}", output_text(&status.stderr));
    let status_json: serde_json::Value =
        serde_json::from_slice(&status.stdout).expect("status JSON");
    assert_eq!(status_json["store_revision"], 1);
    assert_eq!(status_json["queue_generation"], 1);
    assert_eq!(status_json["active_count"], 1);

    let queue = run_glaeda(&[
        OsStr::new("--output"),
        OsStr::new("json"),
        OsStr::new("queue"),
        OsStr::new("list"),
        OsStr::new("--store-root"),
        root_arg,
        OsStr::new("--revision"),
        OsStr::new("1"),
        OsStr::new("--generation"),
        OsStr::new("1"),
        OsStr::new("--offset"),
        OsStr::new("0"),
        OsStr::new("--limit"),
        OsStr::new("1"),
    ]);
    assert!(queue.status.success(), "{}", output_text(&queue.stderr));
    let queue_json: serde_json::Value = serde_json::from_slice(&queue.stdout).expect("queue JSON");
    assert_eq!(queue_json["total"], 3);
    assert_eq!(
        queue_json["items"].as_array().expect("queue items").len(),
        1
    );
    assert_eq!(queue_json["next_offset"], 1);

    let job = run_glaeda(&[
        OsStr::new("job"),
        OsStr::new("show"),
        OsStr::new("--store-root"),
        root_arg,
        OsStr::new("--revision"),
        OsStr::new("1"),
        OsStr::new("--generation"),
        OsStr::new("1"),
        OsStr::new("active-one"),
    ]);
    assert!(job.status.success(), "{}", output_text(&job.stderr));
    let job_human = output_text(&job.stdout);
    assert!(job_human.contains("state: active"));
    assert!(job_human.contains("reservation-active-one"));

    assert_eq!(store_snapshot(&root), before);
}

#[test]
fn installed_cli_renders_unobserved_never_active_status_without_guessing() {
    let root = TempRoot::new("unobserved");
    let document = PersonalWorkerStoreDocument::new(
        PersonalWorkerQueueInput {
            generation: PersonalWorkerQueueGeneration::new(1).expect("queue generation"),
            observed_at: time(BASE),
            profile_observation: PersonalWorkerProfileObservation::Unobserved,
            activity_evidence: PersonalWorkerActivityEvidence::Never,
            queued: vec![],
            active: vec![],
            pending_profile_change: None,
        },
        vec![],
    )
    .expect("unobserved document");
    create_store(&root, &document);

    let json_output = run_glaeda(&[
        OsStr::new("--output"),
        OsStr::new("json"),
        OsStr::new("worker"),
        OsStr::new("status"),
        OsStr::new("--store-root"),
        root.path().as_os_str(),
    ]);
    assert!(
        json_output.status.success(),
        "{}",
        output_text(&json_output.stderr)
    );
    let value: serde_json::Value =
        serde_json::from_slice(&json_output.stdout).expect("status JSON");
    assert_eq!(
        value["profile_observation"],
        serde_json::json!({"state": "unobserved"})
    );
    assert_eq!(
        value["activity_evidence"],
        serde_json::json!({"state": "never"})
    );
    assert_eq!(value["desired_profile"], "stopped");

    let human_output = run_glaeda(&[
        OsStr::new("worker"),
        OsStr::new("status"),
        OsStr::new("--store-root"),
        root.path().as_os_str(),
    ]);
    assert!(
        human_output.status.success(),
        "{}",
        output_text(&human_output.stderr)
    );
    let human = output_text(&human_output.stdout);
    assert!(human.contains("profile: unobserved -> stopped"));
    assert!(human.contains("activity: never"));
    assert!(!human.contains("profile: null"));
}

#[test]
fn installed_cli_distinguishes_canonical_v1_from_corrupt_state() {
    let root = TempRoot::new("v1-incompatible");
    create_store(&root, &terminal_document());
    let v1 = format!(
        concat!(
            "{{\n",
            "  \"schema_version\": 1,\n",
            "  \"revision\": 1,\n",
            "  \"queue\": {{\n",
            "    \"generation\": 1,\n",
            "    \"observed_at\": {base},\n",
            "    \"current_profile\": \"interactive\",\n",
            "    \"last_activity_at\": {base},\n",
            "    \"queued\": [],\n",
            "    \"active\": [],\n",
            "    \"pending_profile_change\": null\n",
            "  }},\n",
            "  \"cache_leases\": [],\n",
            "  \"history\": []\n",
            "}}\n"
        ),
        base = BASE,
    );
    fs::write(root.store_directory().join("current.json"), v1).expect("write canonical v1");

    let output = run_glaeda(&[
        OsStr::new("--output"),
        OsStr::new("json"),
        OsStr::new("worker"),
        OsStr::new("status"),
        OsStr::new("--store-root"),
        root.path().as_os_str(),
    ]);
    assert!(!output.status.success());
    let error: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("version error JSON");
    assert_eq!(error["kind"], "durable_state_version_incompatible");
    assert!(
        error["message"]
            .as_str()
            .expect("message")
            .contains("explicit migration is required")
    );
    assert!(
        !error["message"]
            .as_str()
            .expect("message")
            .contains("corrupt")
    );
}

#[test]
fn installed_cli_projects_retained_terminal_and_bounds_errors() {
    let root = TempRoot::new("terminal");
    create_store(&root, &terminal_document());
    let root_arg = root.path().as_os_str();

    let terminal = run_glaeda(&[
        OsStr::new("--output"),
        OsStr::new("json"),
        OsStr::new("job"),
        OsStr::new("show"),
        OsStr::new("--store-root"),
        root_arg,
        OsStr::new("--revision"),
        OsStr::new("1"),
        OsStr::new("--generation"),
        OsStr::new("1"),
        OsStr::new("terminal-one"),
    ]);
    assert!(
        terminal.status.success(),
        "{}",
        output_text(&terminal.stderr)
    );
    let terminal_json: serde_json::Value =
        serde_json::from_slice(&terminal.stdout).expect("terminal JSON");
    assert_eq!(terminal_json["job"]["state"], "terminal");
    assert_eq!(
        terminal_json["job"]["terminal"]["request"]["identity"]["request_id"],
        "terminal-one"
    );

    let stale = run_glaeda(&[
        OsStr::new("--output"),
        OsStr::new("json"),
        OsStr::new("queue"),
        OsStr::new("list"),
        OsStr::new("--store-root"),
        root_arg,
        OsStr::new("--revision"),
        OsStr::new("2"),
        OsStr::new("--generation"),
        OsStr::new("1"),
    ]);
    assert!(!stale.status.success());
    let stale_text = output_text(&stale.stdout);
    let stale_json: serde_json::Value = serde_json::from_str(&stale_text).expect("stale JSON");
    assert_eq!(stale_json["kind"], "stale_revision");
    assert!(!stale_text.contains(root.path().to_string_lossy().as_ref()));

    let missing = run_glaeda(&[
        OsStr::new("--output"),
        OsStr::new("json"),
        OsStr::new("job"),
        OsStr::new("show"),
        OsStr::new("--store-root"),
        root_arg,
        OsStr::new("--revision"),
        OsStr::new("1"),
        OsStr::new("--generation"),
        OsStr::new("1"),
        OsStr::new("evicted-terminal"),
    ]);
    assert!(!missing.status.success());
    let missing_json: serde_json::Value =
        serde_json::from_slice(&missing.stdout).expect("missing JSON");
    assert_eq!(missing_json["kind"], "not_found");
}

#[test]
fn installed_cli_does_not_create_or_recover_durable_state() {
    let missing_root = TempRoot::new("missing");
    let missing = run_glaeda(&[
        OsStr::new("--output"),
        OsStr::new("json"),
        OsStr::new("worker"),
        OsStr::new("status"),
        OsStr::new("--store-root"),
        missing_root.path().as_os_str(),
    ]);
    assert!(!missing.status.success());
    let missing_json: serde_json::Value =
        serde_json::from_slice(&missing.stdout).expect("missing store JSON");
    assert_eq!(missing_json["kind"], "missing_store");
    assert!(!missing_root.store_directory().exists());

    let root = TempRoot::new("staged");
    let current = live_document();
    create_store(&root, &current);
    let successor = current
        .advance(
            PersonalWorkerQueueInput {
                generation: PersonalWorkerQueueGeneration::new(2).expect("queue generation"),
                observed_at: time(BASE + 1_000),
                profile_observation: PersonalWorkerProfileObservation::observed(
                    PersonalWorkerProfile::Work,
                ),
                activity_evidence: PersonalWorkerActivityEvidence::observed(time(BASE)),
                queued: current.queue().queued.clone(),
                active: current.queue().active.clone(),
                pending_profile_change: None,
            },
            current.cache_leases().to_vec(),
        )
        .expect("successor");
    let staged_path = root.store_directory().join(".next.json");
    fs::write(
        &staged_path,
        encode_personal_worker_store_document(&successor).expect("encode successor"),
    )
    .expect("write staged state");
    fs::set_permissions(&staged_path, fs::Permissions::from_mode(0o600)).expect("set staged mode");

    let before = store_snapshot(&root);
    let status = run_glaeda(&[
        OsStr::new("--output"),
        OsStr::new("json"),
        OsStr::new("worker"),
        OsStr::new("status"),
        OsStr::new("--store-root"),
        root.path().as_os_str(),
    ]);
    assert!(status.status.success(), "{}", output_text(&status.stderr));
    let status_json: serde_json::Value =
        serde_json::from_slice(&status.stdout).expect("status JSON");
    assert_eq!(status_json["store_revision"], 1);
    assert_eq!(store_snapshot(&root), before);
    assert!(staged_path.exists());
}
