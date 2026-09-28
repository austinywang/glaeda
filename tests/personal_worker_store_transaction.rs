use glaeda::artifact::{CommitId, GitTreeId, RepositoryRef, Sha256Digest};
use glaeda::execution_admission::{
    DrainAcknowledgement, EpochMillis, ExecutionAdmissionIdentity, ExecutionAdmissionInput,
    ExecutionAdmissionRecord, ExecutionAdmissionState, ExecutionRequestId, ExecutionResourceLimits,
    FallbackProfileEligibility, HostCapacityObservation, ReservationEvidence,
    ReservationGeneration, ReservationId, RunnerProfileId, UnavailableReason,
};
use glaeda::personal_worker_queue::{
    PersonalWorkerActivityEvidence, PersonalWorkerCacheAccessMode, PersonalWorkerCacheNamespace,
    PersonalWorkerCancellationState, PersonalWorkerJobRequest, PersonalWorkerPriority,
    PersonalWorkerProfile, PersonalWorkerProfileObservation, PersonalWorkerQueueGeneration,
    PersonalWorkerQueueInput, PersonalWorkerSourceIdentity,
};
use glaeda::personal_worker_store::{
    PersonalWorkerDurableCacheLease, PersonalWorkerStore, PersonalWorkerStoreDocument,
    PersonalWorkerStoreError, PersonalWorkerStoreErrorKind, PersonalWorkerStoreRecovery,
    PersonalWorkerStoreRecoveryDisposition, PersonalWorkerStoreRevision,
    PersonalWorkerStoreWriteDisposition, PersonalWorkerStoreWriteReceipt,
};
use glaeda::personal_worker_store_transaction::{
    PersonalWorkerStoreMutation, PersonalWorkerStoreMutationDisposition,
    PersonalWorkerStoreMutationErrorKind, apply_personal_worker_store_mutation,
};
use glaeda::verification_profile::{CacheId, VerificationProfileId};

const GIB: u64 = 1_024 * 1_024 * 1_024;
const BASE: u64 = 1_000_000;

#[derive(Default)]
struct MemoryStore {
    current: Option<PersonalWorkerStoreDocument>,
    staged: Option<PersonalWorkerStoreDocument>,
    recoveries: usize,
}

impl MemoryStore {
    fn with_current(current: PersonalWorkerStoreDocument) -> Self {
        Self {
            current: Some(current),
            staged: None,
            recoveries: 0,
        }
    }
}

impl PersonalWorkerStore for MemoryStore {
    fn load(&self) -> Result<Option<PersonalWorkerStoreDocument>, PersonalWorkerStoreError> {
        Ok(self.current.clone())
    }

    fn create(
        &mut self,
        document: &PersonalWorkerStoreDocument,
    ) -> Result<PersonalWorkerStoreWriteReceipt, PersonalWorkerStoreError> {
        if self.current.is_some() {
            return Err(PersonalWorkerStoreError::new(
                PersonalWorkerStoreErrorKind::RevisionConflict,
                "state already exists",
            ));
        }
        self.current = Some(document.clone());
        Ok(PersonalWorkerStoreWriteReceipt::new(
            PersonalWorkerStoreWriteDisposition::Created,
            document.revision(),
            1,
        ))
    }

    fn replace_if_revision(
        &mut self,
        expected_revision: PersonalWorkerStoreRevision,
        document: &PersonalWorkerStoreDocument,
    ) -> Result<PersonalWorkerStoreWriteReceipt, PersonalWorkerStoreError> {
        let current = self.current.as_ref().ok_or_else(|| {
            PersonalWorkerStoreError::new(PersonalWorkerStoreErrorKind::Missing, "state missing")
        })?;
        if current.revision() != expected_revision {
            return Err(PersonalWorkerStoreError::new(
                PersonalWorkerStoreErrorKind::RevisionConflict,
                "stale revision",
            ));
        }
        document.validate_successor_of(current)?;
        self.current = Some(document.clone());
        Ok(PersonalWorkerStoreWriteReceipt::new(
            PersonalWorkerStoreWriteDisposition::Replaced,
            document.revision(),
            1,
        ))
    }

    fn recover(&mut self) -> Result<PersonalWorkerStoreRecovery, PersonalWorkerStoreError> {
        self.recoveries += 1;
        if let Some(staged) = self.staged.take() {
            if let Some(current) = &self.current {
                staged.validate_successor_of(current)?;
            }
            let revision = staged.revision();
            self.current = Some(staged);
            return Ok(PersonalWorkerStoreRecovery::new(
                PersonalWorkerStoreRecoveryDisposition::PublishedStaged,
                Some(revision),
            ));
        }
        Ok(PersonalWorkerStoreRecovery::new(
            PersonalWorkerStoreRecoveryDisposition::Clean,
            self.current
                .as_ref()
                .map(PersonalWorkerStoreDocument::revision),
        ))
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

fn namespace(repository: &str, cache_id: &str) -> PersonalWorkerCacheNamespace {
    PersonalWorkerCacheNamespace::RepositoryBuild {
        cache_id: CacheId::parse(cache_id).expect("cache ID"),
        repository: RepositoryRef::parse(repository).expect("cache repository"),
        namespace_digest: Sha256Digest::parse(&format!("sha256:{}", "ab".repeat(32)))
            .expect("namespace digest"),
    }
}

fn request_with_cache(id: &str, repository: &str, cache_id: &str) -> PersonalWorkerJobRequest {
    PersonalWorkerJobRequest {
        identity: identity(id),
        source: PersonalWorkerSourceIdentity::new(
            RepositoryRef::parse(repository).expect("repository"),
            CommitId::parse(&"a".repeat(40)).expect("commit"),
            GitTreeId::parse(&"b".repeat(40)).expect("tree"),
        ),
        priority: PersonalWorkerPriority::Normal,
        requested_limits: limits(2_000, 2),
        cache_namespace: namespace(repository, cache_id),
        cache_access: PersonalWorkerCacheAccessMode::Write,
        submitted_at: time(BASE),
        operator_deadline: None,
        cancellation: PersonalWorkerCancellationState::Active,
        fallback_eligibility: FallbackProfileEligibility::ineligible(),
    }
}

fn request(id: &str) -> PersonalWorkerJobRequest {
    request_with_cache(id, "example/project", "build-cache")
}

fn empty_queue(generation: u64, observed_at: u64) -> PersonalWorkerQueueInput {
    PersonalWorkerQueueInput {
        generation: PersonalWorkerQueueGeneration::new(generation).expect("generation"),
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

fn work_queue(requests: Vec<PersonalWorkerJobRequest>) -> PersonalWorkerQueueInput {
    PersonalWorkerQueueInput {
        generation: PersonalWorkerQueueGeneration::new(1).expect("generation"),
        observed_at: time(BASE + 10),
        profile_observation: PersonalWorkerProfileObservation::observed(
            PersonalWorkerProfile::Work,
        ),
        activity_evidence: PersonalWorkerActivityEvidence::observed(time(BASE + 10)),
        queued: requests,
        active: vec![],
        pending_profile_change: None,
    }
}

fn reservation() -> ReservationEvidence {
    ReservationEvidence::new(
        ReservationId::parse("reservation-job-one").expect("reservation ID"),
        ReservationGeneration::new(1).expect("reservation generation"),
        time(BASE + 20),
        time(BASE + 100_000),
    )
}

fn admission(
    request: &PersonalWorkerJobRequest,
    state: ExecutionAdmissionState,
    observed_at: u64,
    acknowledgement: Option<DrainAcknowledgement>,
    unavailable_reason: Option<UnavailableReason>,
) -> ExecutionAdmissionRecord {
    ExecutionAdmissionRecord::from_input(ExecutionAdmissionInput {
        identity: request.identity.clone(),
        state,
        observed_at: time(observed_at),
        requested_limits: request.requested_limits,
        host_capacity: Some(HostCapacityObservation::new(
            time(BASE + 15),
            limits(8_000, 10),
        )),
        applied_limits: Some(request.requested_limits),
        queue_position: None,
        reservation: Some(reservation()),
        acknowledgement,
        fallback_eligibility: request.fallback_eligibility.clone(),
        unavailable_reason,
    })
    .expect("admission")
}

fn cache_lease(request: &PersonalWorkerJobRequest) -> PersonalWorkerDurableCacheLease {
    let evidence = reservation();
    PersonalWorkerDurableCacheLease::new(
        request.identity.request_id.clone(),
        request.cache_namespace.clone(),
        request.cache_access,
        evidence.id,
        evidence.generation,
        evidence.reserved_at,
    )
}

fn current(store: &MemoryStore) -> PersonalWorkerStoreDocument {
    store.current.clone().expect("current document")
}

#[test]
fn submit_is_exactly_idempotent_and_stale_expectations_fail_closed() {
    let document =
        PersonalWorkerStoreDocument::new(empty_queue(1, BASE), vec![]).expect("initial document");
    let mut store = MemoryStore::with_current(document);
    let submitted = request("job-one");
    let receipt = apply_personal_worker_store_mutation(
        &mut store,
        PersonalWorkerStoreRevision::new(1).expect("revision"),
        PersonalWorkerQueueGeneration::new(1).expect("generation"),
        PersonalWorkerStoreMutation::Submit {
            request: submitted.clone(),
            observed_at: time(BASE + 10),
        },
    )
    .expect("submit");
    assert_eq!(
        receipt.disposition(),
        PersonalWorkerStoreMutationDisposition::Applied
    );
    assert_eq!(receipt.new_revision().get(), 2);
    assert_eq!(receipt.new_queue_generation().get(), 2);

    let duplicate = apply_personal_worker_store_mutation(
        &mut store,
        receipt.new_revision(),
        receipt.new_queue_generation(),
        PersonalWorkerStoreMutation::Submit {
            request: submitted.clone(),
            observed_at: time(BASE + 10),
        },
    )
    .expect("duplicate submit");
    assert_eq!(
        duplicate.disposition(),
        PersonalWorkerStoreMutationDisposition::Duplicate
    );
    assert_eq!(duplicate.old_revision(), duplicate.new_revision());

    let error = apply_personal_worker_store_mutation(
        &mut store,
        PersonalWorkerStoreRevision::new(1).expect("stale revision"),
        receipt.new_queue_generation(),
        PersonalWorkerStoreMutation::Submit {
            request: request("job-two"),
            observed_at: time(BASE + 20),
        },
    )
    .expect_err("stale revision");
    assert_eq!(
        error.kind(),
        PersonalWorkerStoreMutationErrorKind::StaleRevision
    );

    let error = apply_personal_worker_store_mutation(
        &mut store,
        receipt.new_revision(),
        PersonalWorkerQueueGeneration::new(1).expect("stale generation"),
        PersonalWorkerStoreMutation::Submit {
            request: request("job-two"),
            observed_at: time(BASE + 20),
        },
    )
    .expect_err("stale generation");
    assert_eq!(
        error.kind(),
        PersonalWorkerStoreMutationErrorKind::StaleQueueGeneration
    );
}

#[test]
fn reservation_lifecycle_rejects_cache_conflicts_and_state_reversal() {
    let first = request("job-one");
    let second = request("job-two");
    let document =
        PersonalWorkerStoreDocument::new(work_queue(vec![first.clone(), second.clone()]), vec![])
            .expect("initial document");
    let mut store = MemoryStore::with_current(document);

    let reserved = admission(
        &first,
        ExecutionAdmissionState::Reserved,
        BASE + 30,
        None,
        None,
    );
    let receipt = apply_personal_worker_store_mutation(
        &mut store,
        PersonalWorkerStoreRevision::new(1).expect("revision"),
        PersonalWorkerQueueGeneration::new(1).expect("generation"),
        PersonalWorkerStoreMutation::RecordReservationAndAcquireCacheLease {
            request_id: first.identity.request_id.clone(),
            admission: reserved,
            cache_lease: cache_lease(&first),
        },
    )
    .expect("reserve first request");

    let conflicting = admission(
        &second,
        ExecutionAdmissionState::Reserved,
        BASE + 40,
        None,
        None,
    );
    let error = apply_personal_worker_store_mutation(
        &mut store,
        receipt.new_revision(),
        receipt.new_queue_generation(),
        PersonalWorkerStoreMutation::RecordReservationAndAcquireCacheLease {
            request_id: second.identity.request_id.clone(),
            admission: conflicting,
            cache_lease: cache_lease(&second),
        },
    )
    .expect_err("writer conflict");
    assert_eq!(
        error.kind(),
        PersonalWorkerStoreMutationErrorKind::InvalidMutation
    );

    let starting = admission(
        &first,
        ExecutionAdmissionState::Starting,
        BASE + 50,
        None,
        None,
    );
    let receipt = apply_personal_worker_store_mutation(
        &mut store,
        receipt.new_revision(),
        receipt.new_queue_generation(),
        PersonalWorkerStoreMutation::MarkStarting {
            request_id: first.identity.request_id.clone(),
            admission: starting,
            started_at: time(BASE + 45),
        },
    )
    .expect("mark starting");
    let running = admission(
        &first,
        ExecutionAdmissionState::Running,
        BASE + 60,
        None,
        None,
    );
    let receipt = apply_personal_worker_store_mutation(
        &mut store,
        receipt.new_revision(),
        receipt.new_queue_generation(),
        PersonalWorkerStoreMutation::MarkRunning {
            request_id: first.identity.request_id.clone(),
            admission: running,
        },
    )
    .expect("mark running");
    let reversed = admission(
        &first,
        ExecutionAdmissionState::Starting,
        BASE + 70,
        None,
        None,
    );
    let error = apply_personal_worker_store_mutation(
        &mut store,
        receipt.new_revision(),
        receipt.new_queue_generation(),
        PersonalWorkerStoreMutation::MarkStarting {
            request_id: first.identity.request_id.clone(),
            admission: reversed,
            started_at: time(BASE + 45),
        },
    )
    .expect_err("state reversal");
    assert_eq!(
        error.kind(),
        PersonalWorkerStoreMutationErrorKind::InvalidMutation
    );
}

#[test]
fn draining_release_and_profile_activity_mutations_are_atomic() {
    let request = request("job-one");
    let document = PersonalWorkerStoreDocument::new(work_queue(vec![request.clone()]), vec![])
        .expect("initial document");
    let mut store = MemoryStore::with_current(document);
    let mut revision = PersonalWorkerStoreRevision::new(1).expect("revision");
    let mut generation = PersonalWorkerQueueGeneration::new(1).expect("generation");

    for mutation in [
        PersonalWorkerStoreMutation::RecordReservationAndAcquireCacheLease {
            request_id: request.identity.request_id.clone(),
            admission: admission(
                &request,
                ExecutionAdmissionState::Reserved,
                BASE + 30,
                None,
                None,
            ),
            cache_lease: cache_lease(&request),
        },
        PersonalWorkerStoreMutation::MarkStarting {
            request_id: request.identity.request_id.clone(),
            admission: admission(
                &request,
                ExecutionAdmissionState::Starting,
                BASE + 40,
                None,
                None,
            ),
            started_at: time(BASE + 35),
        },
        PersonalWorkerStoreMutation::MarkRunning {
            request_id: request.identity.request_id.clone(),
            admission: admission(
                &request,
                ExecutionAdmissionState::Running,
                BASE + 50,
                None,
                None,
            ),
        },
        PersonalWorkerStoreMutation::MarkDraining {
            request_id: request.identity.request_id.clone(),
            admission: admission(
                &request,
                ExecutionAdmissionState::Draining,
                BASE + 60,
                Some(DrainAcknowledgement::Drain),
                None,
            ),
        },
        PersonalWorkerStoreMutation::ReleaseCompletionAndCacheLease {
            request_id: request.identity.request_id.clone(),
            terminal_admission: admission(
                &request,
                ExecutionAdmissionState::Unavailable,
                BASE + 70,
                Some(DrainAcknowledgement::Drain),
                Some(UnavailableReason::Drained),
            ),
        },
    ] {
        let receipt =
            apply_personal_worker_store_mutation(&mut store, revision, generation, mutation)
                .expect("lifecycle mutation");
        revision = receipt.new_revision();
        generation = receipt.new_queue_generation();
    }
    assert!(current(&store).queue().active.is_empty());
    assert!(current(&store).cache_leases().is_empty());

    let receipt = apply_personal_worker_store_mutation(
        &mut store,
        revision,
        generation,
        PersonalWorkerStoreMutation::SetProfileIntent {
            target: PersonalWorkerProfile::Interactive,
            requested_at: time(BASE + 80),
            observed_at: time(BASE + 80),
        },
    )
    .expect("set profile intent");
    let receipt = apply_personal_worker_store_mutation(
        &mut store,
        receipt.new_revision(),
        receipt.new_queue_generation(),
        PersonalWorkerStoreMutation::CancelProfileIntent {
            observed_at: time(BASE + 90),
        },
    )
    .expect("cancel profile intent");
    let receipt = apply_personal_worker_store_mutation(
        &mut store,
        receipt.new_revision(),
        receipt.new_queue_generation(),
        PersonalWorkerStoreMutation::UpdateLastActivity {
            last_activity_at: time(BASE + 95),
            observed_at: time(BASE + 100),
        },
    )
    .expect("update activity");
    assert_eq!(
        current(&store).queue().activity_evidence.last_activity_at(),
        Some(time(BASE + 95))
    );
    assert_eq!(receipt.new_revision().get(), 9);
}

#[test]
fn recovery_runs_before_exact_expectation_and_receipts_are_private() {
    let initial =
        PersonalWorkerStoreDocument::new(empty_queue(1, BASE), vec![]).expect("initial document");
    let staged = initial
        .advance(empty_queue(2, BASE + 10), vec![])
        .expect("staged successor");
    let mut store = MemoryStore::with_current(initial);
    store.staged = Some(staged);
    let private_request = request("secret-request");
    let mutation = PersonalWorkerStoreMutation::Submit {
        request: private_request,
        observed_at: time(BASE + 20),
    };
    assert!(!format!("{mutation:?}").contains("secret-request"));

    let receipt = apply_personal_worker_store_mutation(
        &mut store,
        PersonalWorkerStoreRevision::new(2).expect("recovered revision"),
        PersonalWorkerQueueGeneration::new(2).expect("recovered generation"),
        mutation,
    )
    .expect("apply after recovery");
    assert_eq!(store.recoveries, 1);
    assert_eq!(receipt.old_revision().get(), 2);
    assert_eq!(receipt.new_revision().get(), 3);
    let public = serde_json::to_string(&receipt).expect("serialize receipt");
    assert!(!public.contains("secret-request"));
    assert!(!public.contains("path"));
    assert!(!public.contains("environment"));
    assert!(!public.contains("credential"));
}

#[test]
fn first_profile_observation_does_not_invent_activity_and_submission_records_it() {
    let mut initial_queue = empty_queue(1, BASE);
    initial_queue.profile_observation = PersonalWorkerProfileObservation::Unobserved;
    initial_queue.activity_evidence = PersonalWorkerActivityEvidence::Never;
    let initial =
        PersonalWorkerStoreDocument::new(initial_queue, vec![]).expect("initial document");
    let mut store = MemoryStore::with_current(initial);

    let observed = apply_personal_worker_store_mutation(
        &mut store,
        PersonalWorkerStoreRevision::new(1).expect("revision"),
        PersonalWorkerQueueGeneration::new(1).expect("generation"),
        PersonalWorkerStoreMutation::ObserveProfile {
            profile: PersonalWorkerProfile::Stopped,
            observed_at: time(BASE + 10),
        },
    )
    .expect("record first profile observation");
    assert_eq!(observed.new_revision().get(), 2);
    let after_observation = current(&store);
    assert_eq!(
        after_observation.queue().profile_observation,
        PersonalWorkerProfileObservation::observed(PersonalWorkerProfile::Stopped)
    );
    assert_eq!(
        after_observation.queue().activity_evidence,
        PersonalWorkerActivityEvidence::Never
    );

    let submitted = apply_personal_worker_store_mutation(
        &mut store,
        observed.new_revision(),
        observed.new_queue_generation(),
        PersonalWorkerStoreMutation::Submit {
            request: request("first-activity"),
            observed_at: time(BASE + 20),
        },
    )
    .expect("submit first activity");
    assert_eq!(submitted.new_revision().get(), 3);
    assert_eq!(
        current(&store).queue().activity_evidence,
        PersonalWorkerActivityEvidence::observed(time(BASE + 20))
    );
}
