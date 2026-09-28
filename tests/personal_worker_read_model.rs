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
    PersonalWorkerPendingProfileChange, PersonalWorkerPriority, PersonalWorkerProfile,
    PersonalWorkerProfileObservation, PersonalWorkerQueueGeneration, PersonalWorkerQueueInput,
    PersonalWorkerSourceIdentity,
};
use glaeda::personal_worker_read_model::{
    MAX_PERSONAL_WORKER_QUEUE_PAGE_SIZE, PersonalWorkerJobReadRequest, PersonalWorkerJobStateView,
    PersonalWorkerQueuePageRequest, PersonalWorkerReadErrorKind, personal_worker_job_view,
    personal_worker_queue_page, personal_worker_status,
};
use glaeda::personal_worker_store::{
    PersonalWorkerDurableCacheLease, PersonalWorkerStoreDocument, PersonalWorkerStoreRevision,
    PersonalWorkerTerminalMutationClass, PersonalWorkerTerminalTombstone,
};
use glaeda::verification_profile::{CacheId, VerificationProfileId};

const GIB: u64 = 1_024 * 1_024 * 1_024;
const BASE: u64 = 5_000_000;

fn time(value: u64) -> EpochMillis {
    EpochMillis::new(value).expect("time")
}

fn limits(cpu_millis: u32, memory_gib: u64) -> ExecutionResourceLimits {
    ExecutionResourceLimits::new(cpu_millis, memory_gib * GIB, 512).expect("limits")
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
            .expect("namespace digest"),
    }
}

fn request(
    id: &str,
    repository: &str,
    digit: char,
    submitted_at: u64,
    priority: PersonalWorkerPriority,
    cancellation: PersonalWorkerCancellationState,
) -> PersonalWorkerJobRequest {
    PersonalWorkerJobRequest {
        identity: identity(id),
        source: source(repository, digit),
        priority,
        requested_limits: limits(2_000, 2),
        cache_namespace: namespace(repository),
        cache_access: PersonalWorkerCacheAccessMode::Write,
        submitted_at: time(submitted_at),
        operator_deadline: None,
        cancellation,
        fallback_eligibility: FallbackProfileEligibility::ineligible(),
    }
}

fn active_reservation() -> (
    PersonalWorkerActiveReservation,
    PersonalWorkerDurableCacheLease,
) {
    let mut request = request(
        "active-one",
        "example/active",
        'c',
        BASE - 90_000,
        PersonalWorkerPriority::Normal,
        PersonalWorkerCancellationState::Active,
    );
    request.operator_deadline = Some(time(BASE + 1_800_000));
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

fn terminal_tombstone(completed_at: u64) -> PersonalWorkerTerminalTombstone {
    let request = request(
        "terminal-one",
        "example/terminal",
        'd',
        BASE - 120_000,
        PersonalWorkerPriority::Normal,
        PersonalWorkerCancellationState::Active,
    );
    let reservation_id = ReservationId::parse("reservation-terminal-one").expect("reservation ID");
    let generation = ReservationGeneration::new(11).expect("reservation generation");
    let reserved_at = time(BASE - 30_000);
    let terminal_admission = ExecutionAdmissionRecord::from_input(ExecutionAdmissionInput {
        identity: request.identity.clone(),
        state: ExecutionAdmissionState::Unavailable,
        observed_at: time(completed_at),
        requested_limits: request.requested_limits,
        host_capacity: Some(HostCapacityObservation::new(
            time(completed_at - 1_000),
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
        vec![terminal_tombstone(BASE)],
    )
    .expect("terminal document")
}

fn document() -> PersonalWorkerStoreDocument {
    let queued = request(
        "queued-one",
        "example/queued",
        'a',
        BASE - 120_000,
        PersonalWorkerPriority::Interactive,
        PersonalWorkerCancellationState::Active,
    );
    let cancelled = request(
        "cancelled-one",
        "example/cancelled",
        'b',
        BASE - 180_000,
        PersonalWorkerPriority::Background,
        PersonalWorkerCancellationState::Cancelled {
            cancelled_at: time(BASE - 60_000),
        },
    );
    let (active, lease) = active_reservation();
    PersonalWorkerStoreDocument::new(
        PersonalWorkerQueueInput {
            generation: PersonalWorkerQueueGeneration::new(1).expect("queue generation"),
            observed_at: time(BASE),
            profile_observation: PersonalWorkerProfileObservation::observed(
                PersonalWorkerProfile::Work,
            ),
            activity_evidence: PersonalWorkerActivityEvidence::observed(time(BASE - 1_000)),
            queued: vec![cancelled, queued],
            active: vec![active],
            pending_profile_change: Some(PersonalWorkerPendingProfileChange {
                target: PersonalWorkerProfile::Interactive,
                requested_at: time(BASE - 5_000),
            }),
        },
        vec![lease],
    )
    .expect("durable document")
}

fn revision() -> PersonalWorkerStoreRevision {
    PersonalWorkerStoreRevision::new(1).expect("store revision")
}

fn generation() -> PersonalWorkerQueueGeneration {
    PersonalWorkerQueueGeneration::new(1).expect("queue generation")
}

#[test]
fn status_binds_exact_snapshot_and_reports_bounded_counts() {
    let status = personal_worker_status(&document()).expect("status");

    assert_eq!(status.store_revision(), revision());
    assert_eq!(status.queue_generation(), generation());
    assert_eq!(status.current_profile(), Some(PersonalWorkerProfile::Work));
    assert_eq!(status.desired_profile(), PersonalWorkerProfile::Work);
    assert_eq!(status.queued_entry_count(), 2);
    assert_eq!(status.eligible_queue_count(), 1);
    assert_eq!(status.cancelled_queue_count(), 1);
    assert_eq!(status.selected_count(), 1);
    assert_eq!(status.active_count(), 1);
    assert_eq!(status.draining_count(), 0);
    assert_eq!(status.cache_lease_count(), 1);
    assert_eq!(status.terminal_tombstone_count(), 0);
    let pending = status.pending_profile_change().expect("profile intent");
    assert_eq!(pending.target(), PersonalWorkerProfile::Interactive);
    assert_eq!(pending.requested_at(), time(BASE - 5_000));
}

#[test]
fn queue_pages_require_exact_revision_generation_and_bounds() {
    let document = document();
    let first = personal_worker_queue_page(
        &document,
        PersonalWorkerQueuePageRequest::new(revision(), generation(), 0, 2).expect("page request"),
    )
    .expect("first page");
    assert_eq!(first.store_revision(), revision());
    assert_eq!(first.queue_generation(), generation());
    assert_eq!(first.total(), 3);
    assert_eq!(first.items().len(), 2);
    assert_eq!(first.next_offset(), Some(2));

    let final_page = personal_worker_queue_page(
        &document,
        PersonalWorkerQueuePageRequest::new(revision(), generation(), 2, 2)
            .expect("final page request"),
    )
    .expect("final page");
    assert_eq!(final_page.items().len(), 1);
    assert_eq!(final_page.items()[0].request_id.as_str(), "active-one");
    assert_eq!(final_page.next_offset(), None);

    let error = personal_worker_queue_page(
        &document,
        PersonalWorkerQueuePageRequest::new(
            PersonalWorkerStoreRevision::new(2).expect("stale revision"),
            generation(),
            0,
            1,
        )
        .expect("stale request"),
    )
    .expect_err("stale revision");
    assert_eq!(error.kind(), PersonalWorkerReadErrorKind::StaleRevision);

    let error = personal_worker_queue_page(
        &document,
        PersonalWorkerQueuePageRequest::new(
            revision(),
            PersonalWorkerQueueGeneration::new(2).expect("stale generation"),
            0,
            1,
        )
        .expect("stale request"),
    )
    .expect_err("stale generation");
    assert_eq!(
        error.kind(),
        PersonalWorkerReadErrorKind::StaleQueueGeneration
    );

    let error = PersonalWorkerQueuePageRequest::new(revision(), generation(), 0, 0)
        .expect_err("zero limit");
    assert_eq!(error.kind(), PersonalWorkerReadErrorKind::InvalidPage);
    let error = PersonalWorkerQueuePageRequest::new(
        revision(),
        generation(),
        0,
        MAX_PERSONAL_WORKER_QUEUE_PAGE_SIZE + 1,
    )
    .expect_err("oversized limit");
    assert_eq!(error.kind(), PersonalWorkerReadErrorKind::InvalidPage);

    let error = personal_worker_queue_page(
        &document,
        PersonalWorkerQueuePageRequest::new(revision(), generation(), 4, 1)
            .expect("offset request"),
    )
    .expect_err("offset beyond snapshot");
    assert_eq!(error.kind(), PersonalWorkerReadErrorKind::OffsetOutOfBounds);
}

#[test]
fn job_view_exposes_exact_queued_and_active_evidence() {
    let document = document();
    let active = personal_worker_job_view(
        &document,
        PersonalWorkerJobReadRequest::new(
            revision(),
            generation(),
            ExecutionRequestId::parse("active-one").expect("active ID"),
        ),
    )
    .expect("active job");
    assert_eq!(
        active.entry().expect("active entry").request_id.as_str(),
        "active-one"
    );
    assert_eq!(active.submitted_at(), time(BASE - 90_000));
    assert_eq!(
        active.cancellation(),
        PersonalWorkerCancellationState::Active
    );
    let admission = active.admission().expect("admission");
    assert_eq!(admission.state(), ExecutionAdmissionState::Running);
    assert_eq!(admission.observed_at(), time(BASE - 10_000));
    assert_eq!(admission.requested_limits(), limits(2_000, 2));
    assert_eq!(admission.applied_limits(), limits(2_000, 2));
    assert_eq!(admission.requested_limits().pids, 512);
    assert_eq!(active.operator_deadline(), Some(time(BASE + 1_800_000)));
    assert_eq!(
        admission.reservation().id().as_str(),
        "reservation-active-one"
    );
    assert_eq!(admission.reservation().generation().get(), 7);
    let lease = active.durable_cache_lease().expect("durable lease");
    assert_eq!(lease.reservation_id().as_str(), "reservation-active-one");
    assert_eq!(lease.reservation_generation().get(), 7);
    assert_eq!(lease.acquired_at(), time(BASE - 30_000));

    let queued = personal_worker_job_view(
        &document,
        PersonalWorkerJobReadRequest::new(
            revision(),
            generation(),
            ExecutionRequestId::parse("queued-one").expect("queued ID"),
        ),
    )
    .expect("queued job");
    assert!(queued.admission().is_none());
    assert!(queued.durable_cache_lease().is_none());

    let error = personal_worker_job_view(
        &document,
        PersonalWorkerJobReadRequest::new(
            revision(),
            generation(),
            ExecutionRequestId::parse("missing-one").expect("missing ID"),
        ),
    )
    .expect_err("missing job");
    assert_eq!(error.kind(), PersonalWorkerReadErrorKind::NotFound);
}

#[test]
fn terminal_job_view_projects_exact_proof_without_queue_visibility() {
    let document = terminal_document();
    let status = personal_worker_status(&document).expect("terminal status");
    assert_eq!(status.terminal_tombstone_count(), 1);
    assert_eq!(status.active_count(), 0);

    let page = personal_worker_queue_page(
        &document,
        PersonalWorkerQueuePageRequest::new(revision(), generation(), 0, 10)
            .expect("terminal page request"),
    )
    .expect("terminal queue page");
    assert_eq!(page.total(), 0);
    assert!(page.items().is_empty());
    assert_eq!(page.next_offset(), None);

    let job = personal_worker_job_view(
        &document,
        PersonalWorkerJobReadRequest::new(
            revision(),
            generation(),
            ExecutionRequestId::parse("terminal-one").expect("terminal ID"),
        ),
    )
    .expect("terminal job");
    assert!(job.entry().is_none());
    assert!(job.admission().is_none());
    assert!(job.durable_cache_lease().is_none());
    let terminal = job.terminal().expect("terminal evidence");
    assert_eq!(
        terminal.mutation_class(),
        PersonalWorkerTerminalMutationClass::ReleaseCompletionAndCacheLease
    );
    assert_eq!(
        terminal.request().identity().request_id.as_str(),
        "terminal-one"
    );
    assert_eq!(
        terminal.admission_state(),
        ExecutionAdmissionState::Unavailable
    );
    assert_eq!(terminal.completed_at(), time(BASE));
    assert_eq!(terminal.started_at(), Some(time(BASE - 20_000)));
    assert_eq!(terminal.unavailable_reason(), UnavailableReason::Drained);
    assert_eq!(
        terminal.acknowledgement(),
        Some(DrainAcknowledgement::Drain)
    );
    assert_eq!(
        terminal.reservation().id().as_str(),
        "reservation-terminal-one"
    );
    assert_eq!(terminal.reservation().generation().get(), 11);
    assert_eq!(
        terminal.durable_cache_lease().reservation_id().as_str(),
        "reservation-terminal-one"
    );
    assert_eq!(
        terminal
            .durable_cache_lease()
            .reservation_generation()
            .get(),
        11
    );
    assert!(matches!(
        job.state(),
        PersonalWorkerJobStateView::Terminal { .. }
    ));
}

#[test]
fn ambiguous_terminal_proof_is_rejected_and_evicted_identity_is_not_found() {
    let duplicate_error = PersonalWorkerStoreDocument::new_with_terminal_tombstones(
        PersonalWorkerQueueInput {
            generation: PersonalWorkerQueueGeneration::new(1).expect("queue generation"),
            observed_at: time(BASE + 1),
            profile_observation: PersonalWorkerProfileObservation::observed(
                PersonalWorkerProfile::Interactive,
            ),
            activity_evidence: PersonalWorkerActivityEvidence::observed(time(BASE + 1)),
            queued: vec![],
            active: vec![],
            pending_profile_change: None,
        },
        vec![],
        vec![terminal_tombstone(BASE), terminal_tombstone(BASE + 1)],
    )
    .expect_err("duplicate terminal identity");
    assert!(!format!("{duplicate_error:?}").contains("terminal-one"));

    let error = personal_worker_job_view(
        &terminal_document(),
        PersonalWorkerJobReadRequest::new(
            revision(),
            generation(),
            ExecutionRequestId::parse("evicted-terminal").expect("evicted ID"),
        ),
    )
    .expect_err("unprovable terminal identity");
    assert_eq!(error.kind(), PersonalWorkerReadErrorKind::NotFound);
}

#[test]
fn public_json_and_errors_exclude_private_runtime_material() {
    let document = document();
    let status = serde_json::to_string(&personal_worker_status(&document).expect("status JSON"))
        .expect("serialize status");
    let page = serde_json::to_string(
        &personal_worker_queue_page(
            &document,
            PersonalWorkerQueuePageRequest::new(revision(), generation(), 0, 3)
                .expect("page request"),
        )
        .expect("page"),
    )
    .expect("serialize page");
    let job = serde_json::to_string(
        &personal_worker_job_view(
            &document,
            PersonalWorkerJobReadRequest::new(
                revision(),
                generation(),
                ExecutionRequestId::parse("active-one").expect("active ID"),
            ),
        )
        .expect("job"),
    )
    .expect("serialize job");
    let terminal_job = serde_json::to_string(
        &personal_worker_job_view(
            &terminal_document(),
            PersonalWorkerJobReadRequest::new(
                revision(),
                generation(),
                ExecutionRequestId::parse("terminal-one").expect("terminal ID"),
            ),
        )
        .expect("terminal job"),
    )
    .expect("serialize terminal job");
    let error = personal_worker_job_view(
        &document,
        PersonalWorkerJobReadRequest::new(
            revision(),
            generation(),
            ExecutionRequestId::parse("missing-one").expect("missing ID"),
        ),
    )
    .expect_err("missing job");
    let public = format!("{status}\n{page}\n{job}\n{terminal_job}\n{error:?}\n{error}");
    assert!(job.contains("\"schema_version\":3"));
    assert!(job.contains("\"applied_limits\""));

    for forbidden in [
        "/tmp/private-worker",
        "limactl",
        "github_token",
        "process stderr",
        "cache contents",
    ] {
        assert!(!public.contains(forbidden));
    }
}
