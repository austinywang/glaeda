#![cfg(unix)]

use std::fs::{self, OpenOptions};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;

use glaeda::artifact::{CommitId, GitTreeId, RepositoryRef, Sha256Digest};
use glaeda::execution_admission::{
    EpochMillis, ExecutionAdmissionIdentity, ExecutionRequestId, ExecutionResourceLimits,
    FallbackProfileEligibility, RunnerProfileId,
};
use glaeda::lima_observation::LimaInstanceName;
use glaeda::mac_availability::AvailabilityRequest;
use glaeda::operator_config::{
    GuestWorkspacePath, OperatorConfig, OperatorIdlePolicy, OperatorOutputPreference,
    OperatorRemediationPreference, PersonalWorkerStateRoot,
};
use glaeda::operator_error::OperatorErrorCode;
use glaeda::personal_worker_operator_mutation::{
    PersonalWorkerMutationExpectation, PersonalWorkerOperatorMutationErrorKind,
    PersonalWorkerOperatorMutationService, PersonalWorkerSubmissionInput,
};
use glaeda::personal_worker_operator_read::PersonalWorkerOperatorReadService;
use glaeda::personal_worker_operator_store::{
    PersonalWorkerInitializationInput, PersonalWorkerOperatorStore,
};
use glaeda::personal_worker_queue::{
    MAX_PERSONAL_WORKER_QUEUE_ENTRIES, PersonalWorkerActivityEvidence,
    PersonalWorkerCacheAccessMode, PersonalWorkerCacheNamespace, PersonalWorkerCancellationState,
    PersonalWorkerJobRequest, PersonalWorkerPriority, PersonalWorkerProfileObservation,
    PersonalWorkerQueueGeneration, PersonalWorkerQueueInput, PersonalWorkerSourceIdentity,
};
use glaeda::personal_worker_read_model::PersonalWorkerJobReadRequest;
use glaeda::personal_worker_store::{PersonalWorkerStoreDocument, PersonalWorkerStoreRevision};
use glaeda::personal_worker_store_transaction::PersonalWorkerStoreMutationDisposition;
use glaeda::unix_personal_worker_store::UnixPersonalWorkerStore;
use glaeda::verification_profile::{CacheId, VerificationProfileId};
use rustix::fs::{FlockOperation, flock};

const BASE: u64 = 1_000_000;
const GIB: u64 = 1_024 * 1_024 * 1_024;
static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

struct TempRoot(PathBuf);

impl TempRoot {
    fn new(label: &str) -> Self {
        let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "smolrunner-operator-mutation-{label}-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create state root");
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

fn revision(value: u64) -> PersonalWorkerStoreRevision {
    PersonalWorkerStoreRevision::new(value).expect("revision")
}

fn generation(value: u64) -> PersonalWorkerQueueGeneration {
    PersonalWorkerQueueGeneration::new(value).expect("generation")
}

fn operator_config(path: &Path) -> OperatorConfig {
    OperatorConfig::new(
        PersonalWorkerStateRoot::parse(path).expect("state root"),
        LimaInstanceName::parse("smolrunner").expect("instance"),
        GuestWorkspacePath::parse("/home/lima/smolrunner-workspace").expect("workspace"),
        VerificationProfileId::parse("smolrunner.required").expect("profile"),
        AvailabilityRequest::Auto,
        OperatorIdlePolicy::new(600_000, 1_800_000).expect("idle policy"),
        OperatorOutputPreference::Json,
        OperatorRemediationPreference::IncludeSuggestions,
    )
    .expect("config")
}

fn initialize(config: &OperatorConfig) {
    PersonalWorkerOperatorStore::initialize(
        config,
        PersonalWorkerInitializationInput::new(time(BASE)),
    )
    .expect("initialize");
}

fn limits() -> ExecutionResourceLimits {
    ExecutionResourceLimits::new(2_000, 2 * GIB, 2_048).expect("limits")
}

fn source(digit: char) -> PersonalWorkerSourceIdentity {
    PersonalWorkerSourceIdentity::new(
        RepositoryRef::parse("example/project").expect("repository"),
        CommitId::parse(&digit.to_string().repeat(40)).expect("commit"),
        GitTreeId::parse(&digit.to_string().repeat(40)).expect("tree"),
    )
}

fn cache_namespace() -> PersonalWorkerCacheNamespace {
    PersonalWorkerCacheNamespace::RepositoryBuild {
        cache_id: CacheId::parse("build-cache").expect("cache ID"),
        repository: RepositoryRef::parse("example/project").expect("repository"),
        namespace_digest: Sha256Digest::parse(&format!("sha256:{}", "ab".repeat(32)))
            .expect("namespace digest"),
    }
}

fn submission(request_id: &str, digit: char, submitted_at: u64) -> PersonalWorkerSubmissionInput {
    PersonalWorkerSubmissionInput::new(
        ExecutionRequestId::parse(request_id).expect("request ID"),
        RunnerProfileId::parse("personal-lima-work").expect("runner profile"),
        source(digit),
        PersonalWorkerPriority::Normal,
        limits(),
        cache_namespace(),
        PersonalWorkerCacheAccessMode::Write,
        time(submitted_at),
        None,
    )
}

fn durable_request(index: usize) -> PersonalWorkerJobRequest {
    PersonalWorkerJobRequest {
        identity: ExecutionAdmissionIdentity::new(
            ExecutionRequestId::parse(&format!("capacity-{index:03}"))
                .expect("capacity request ID"),
            VerificationProfileId::parse("smolrunner.required").expect("profile"),
            RunnerProfileId::parse("personal-lima-work").expect("runner profile"),
        ),
        source: source('a'),
        priority: PersonalWorkerPriority::Normal,
        requested_limits: limits(),
        cache_namespace: cache_namespace(),
        cache_access: PersonalWorkerCacheAccessMode::Read,
        submitted_at: time(BASE - 1),
        operator_deadline: None,
        cancellation: PersonalWorkerCancellationState::Active,
        fallback_eligibility: FallbackProfileEligibility::ineligible(),
    }
}

fn snapshot(root: &TempRoot) -> Vec<(String, Vec<u8>)> {
    let mut entries = fs::read_dir(root.store_directory())
        .expect("read store")
        .map(|entry| {
            let entry = entry.expect("entry");
            (
                entry.file_name().to_string_lossy().into_owned(),
                fs::read(entry.path()).expect("read entry"),
            )
        })
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    entries
}

#[test]
fn submit_applies_replays_and_preserves_changed_input_conflict() {
    let root = TempRoot::new("submit");
    let config = operator_config(root.path());
    initialize(&config);

    let applied = PersonalWorkerOperatorMutationService::submit(
        &config,
        submission("request-one", 'a', BASE),
        time(BASE + 1),
        None,
    )
    .expect("submit");
    assert_eq!(applied.config_identity(), config.identity());
    assert_eq!(applied.attempts(), 1);
    assert_eq!(
        applied.mutation().disposition(),
        PersonalWorkerStoreMutationDisposition::Applied
    );
    assert_eq!(applied.mutation().old_revision(), revision(1));
    assert_eq!(applied.mutation().new_revision(), revision(2));

    let job = PersonalWorkerOperatorReadService::read_job(
        &config,
        PersonalWorkerJobReadRequest::new(
            revision(2),
            generation(2),
            ExecutionRequestId::parse("request-one").expect("request ID"),
        ),
    )
    .expect("read submitted job");
    assert_eq!(
        job.view()
            .entry()
            .expect("queued entry")
            .verification_profile_id
            .as_str(),
        config.default_verification_profile().as_str()
    );

    let replay = PersonalWorkerOperatorMutationService::submit(
        &config,
        submission("request-one", 'a', BASE),
        time(BASE + 1),
        None,
    )
    .expect("exact replay");
    assert_eq!(
        replay.mutation().disposition(),
        PersonalWorkerStoreMutationDisposition::Duplicate
    );
    assert_eq!(replay.mutation().new_revision(), revision(2));

    let before_conflict = snapshot(&root);
    let conflict = PersonalWorkerOperatorMutationService::submit(
        &config,
        submission("request-one", 'b', BASE),
        time(BASE + 1),
        None,
    )
    .expect_err("changed request conflict");
    assert_eq!(
        conflict.kind(),
        PersonalWorkerOperatorMutationErrorKind::Conflict
    );
    assert_eq!(
        conflict.public_error().expect("public job conflict").code(),
        OperatorErrorCode::JobConflict
    );
    assert_eq!(snapshot(&root), before_conflict);
}

#[test]
fn queued_cancel_applies_replays_and_changed_time_conflicts() {
    let root = TempRoot::new("cancel");
    let config = operator_config(root.path());
    initialize(&config);
    PersonalWorkerOperatorMutationService::submit(
        &config,
        submission("cancel-one", 'a', BASE),
        time(BASE + 1),
        None,
    )
    .expect("submit cancellable job");

    let request_id = ExecutionRequestId::parse("cancel-one").expect("request ID");
    let cancelled = PersonalWorkerOperatorMutationService::cancel_queued(
        &config,
        request_id.clone(),
        time(BASE + 2),
        None,
    )
    .expect("cancel");
    assert_eq!(
        cancelled.mutation().disposition(),
        PersonalWorkerStoreMutationDisposition::Applied
    );
    assert_eq!(cancelled.mutation().new_revision(), revision(3));

    let replay = PersonalWorkerOperatorMutationService::cancel_queued(
        &config,
        request_id.clone(),
        time(BASE + 2),
        None,
    )
    .expect("cancel replay");
    assert_eq!(
        replay.mutation().disposition(),
        PersonalWorkerStoreMutationDisposition::Duplicate
    );
    let before_conflict = snapshot(&root);
    let conflict = PersonalWorkerOperatorMutationService::cancel_queued(
        &config,
        request_id,
        time(BASE + 3),
        None,
    )
    .expect_err("changed cancellation conflict");
    assert_eq!(
        conflict
            .public_error()
            .expect("public cancel conflict")
            .code(),
        OperatorErrorCode::CancellationConflict
    );
    assert_eq!(snapshot(&root), before_conflict);
}

#[test]
fn strict_expectations_and_injected_time_fail_without_retry_or_mutation() {
    let root = TempRoot::new("strict");
    let config = operator_config(root.path());
    initialize(&config);
    let before = snapshot(&root);

    let cases = [
        (
            PersonalWorkerMutationExpectation::new(revision(2), generation(2), time(BASE)),
            PersonalWorkerOperatorMutationErrorKind::StaleRevision,
        ),
        (
            PersonalWorkerMutationExpectation::new(revision(1), generation(2), time(BASE)),
            PersonalWorkerOperatorMutationErrorKind::StaleQueueGeneration,
        ),
        (
            PersonalWorkerMutationExpectation::new(revision(1), generation(1), time(BASE + 1)),
            PersonalWorkerOperatorMutationErrorKind::StaleObservation,
        ),
    ];
    for (expectation, kind) in cases {
        let error = PersonalWorkerOperatorMutationService::submit(
            &config,
            submission("strict-one", 'a', BASE),
            time(BASE + 1),
            Some(expectation),
        )
        .expect_err("strict mismatch");
        assert_eq!(error.kind(), kind);
        assert_eq!(snapshot(&root), before);
    }

    let invalid_time = PersonalWorkerOperatorMutationService::submit(
        &config,
        submission("future-one", 'a', BASE + 2),
        time(BASE + 1),
        None,
    )
    .expect_err("future submission");
    assert_eq!(
        invalid_time.kind(),
        PersonalWorkerOperatorMutationErrorKind::InvalidTime
    );
    assert_eq!(snapshot(&root), before);
}

#[test]
fn full_queue_has_an_exact_public_capacity_failure_without_publication() {
    let root = TempRoot::new("capacity");
    let config = operator_config(root.path());
    let queue = PersonalWorkerQueueInput {
        generation: generation(1),
        observed_at: time(BASE),
        profile_observation: PersonalWorkerProfileObservation::Unobserved,
        activity_evidence: PersonalWorkerActivityEvidence::observed(time(BASE)),
        queued: (0..MAX_PERSONAL_WORKER_QUEUE_ENTRIES)
            .map(durable_request)
            .collect(),
        active: Vec::new(),
        pending_profile_change: None,
    };
    let document = PersonalWorkerStoreDocument::new(queue, Vec::new()).expect("full document");
    UnixPersonalWorkerStore::initialize_if_clean(root.path(), &document)
        .expect("initialize full queue");
    let before = snapshot(&root);

    let capacity = PersonalWorkerOperatorMutationService::submit(
        &config,
        submission("capacity-new", 'b', BASE),
        time(BASE + 1),
        None,
    )
    .expect_err("capacity reached");
    assert_eq!(
        capacity.kind(),
        PersonalWorkerOperatorMutationErrorKind::CapacityReached
    );
    assert_eq!(
        capacity.public_error().expect("public capacity").code(),
        OperatorErrorCode::QueueCapacityReached
    );
    assert_eq!(snapshot(&root), before);
}

#[test]
fn automatic_identical_race_applies_once_or_refuses_busy_then_replays() {
    let root = TempRoot::new("race");
    let config = Arc::new(operator_config(root.path()));
    initialize(&config);
    let barrier = Arc::new(Barrier::new(2));
    let handles = (0..2)
        .map(|_| {
            let config = Arc::clone(&config);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                PersonalWorkerOperatorMutationService::submit(
                    &config,
                    submission("race-one", 'a', BASE),
                    time(BASE + 1),
                    None,
                )
            })
        })
        .collect::<Vec<_>>();
    let results = handles
        .into_iter()
        .map(|handle| handle.join().expect("join"))
        .collect::<Vec<_>>();
    assert_eq!(
        results
            .iter()
            .filter(|result| {
                result.as_ref().is_ok_and(|receipt| {
                    receipt.mutation().disposition()
                        == PersonalWorkerStoreMutationDisposition::Applied
                })
            })
            .count(),
        1
    );
    assert!(results.iter().all(|result| match result {
        Ok(receipt) => {
            receipt.attempts() <= 2
                && matches!(
                    receipt.mutation().disposition(),
                    PersonalWorkerStoreMutationDisposition::Applied
                        | PersonalWorkerStoreMutationDisposition::Duplicate
                )
        }
        Err(error) => error.kind() == PersonalWorkerOperatorMutationErrorKind::Busy,
    }));

    let replay = PersonalWorkerOperatorMutationService::submit(
        &config,
        submission("race-one", 'a', BASE),
        time(BASE + 1),
        None,
    )
    .expect("post-race replay");
    assert_eq!(
        replay.mutation().disposition(),
        PersonalWorkerStoreMutationDisposition::Duplicate
    );
}

#[test]
fn recovery_busy_and_public_surfaces_remain_bounded_and_private() {
    let root = TempRoot::new("privacy-sentinel");
    let config = operator_config(root.path());
    initialize(&config);
    let current = root.store_directory().join("current.json");
    let staged = root.store_directory().join(".next.json");
    fs::rename(&current, &staged).expect("create recovery debt");
    let input = submission("private-request-sentinel", 'a', BASE);
    let input_debug = format!("{input:?}");
    let recovered =
        PersonalWorkerOperatorMutationService::submit(&config, input, time(BASE + 1), None)
            .expect("recover then submit");
    assert!(current.exists());
    assert!(!staged.exists());

    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.store_directory().join("store.lock"))
        .expect("open lock");
    flock(&lock, FlockOperation::NonBlockingLockExclusive).expect("hold writer lock");
    let before_busy = snapshot(&root);
    let busy = PersonalWorkerOperatorMutationService::cancel_queued(
        &config,
        ExecutionRequestId::parse("private-request-sentinel").expect("request ID"),
        time(BASE + 2),
        None,
    )
    .expect_err("busy mutation");
    assert_eq!(busy.kind(), PersonalWorkerOperatorMutationErrorKind::Busy);
    assert_eq!(snapshot(&root), before_busy);

    let sentinel = root.path().to_string_lossy();
    for public in [
        input_debug,
        format!("{recovered:?}"),
        serde_json::to_string(&recovered).expect("receipt JSON"),
        format!("{busy:?}"),
        serde_json::to_string(&busy).expect("error JSON"),
    ] {
        assert!(!public.contains(sentinel.as_ref()));
        assert!(!public.contains("private-request-sentinel"));
        assert!(!public.contains("current.json"));
        assert!(!public.contains("store.lock"));
        assert!(!public.contains(".next.json"));
    }
}
