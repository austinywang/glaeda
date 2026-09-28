use glaeda::artifact::{CommitId, GitTreeId, RepositoryRef, Sha256Digest};
use glaeda::execution_admission::{
    EpochMillis, ExecutionAdmissionIdentity, ExecutionAdmissionInput, ExecutionAdmissionRecord,
    ExecutionAdmissionState, ExecutionRequestId, ExecutionResourceLimits,
    FallbackProfileEligibility, HostCapacityObservation, ReservationEvidence,
    ReservationGeneration, ReservationId, RunnerProfileId,
};
use glaeda::personal_worker_queue::{
    PersonalWorkerActiveReservation, PersonalWorkerActivityEvidence, PersonalWorkerCacheAccessMode,
    PersonalWorkerCacheLeaseState, PersonalWorkerCacheNamespace, PersonalWorkerCancellationState,
    PersonalWorkerJobRequest, PersonalWorkerPendingProfileChange, PersonalWorkerPriority,
    PersonalWorkerProfile, PersonalWorkerProfileObservation, PersonalWorkerQueueEntryState,
    PersonalWorkerQueueGeneration, PersonalWorkerQueueInput, PersonalWorkerSourceIdentity,
    evaluate_personal_worker_queue,
};
use glaeda::verification_profile::{CacheId, VerificationProfileId};

const GIB: u64 = 1_024 * 1_024 * 1_024;

fn time(value: u64) -> EpochMillis {
    EpochMillis::new(value).expect("time")
}

fn limits_with_pids(cpu_millis: u32, memory_gib: u64, pids: u32) -> ExecutionResourceLimits {
    ExecutionResourceLimits::new(cpu_millis, memory_gib * GIB, pids).expect("limits")
}

fn limits(cpu_millis: u32, memory_gib: u64) -> ExecutionResourceLimits {
    limits_with_pids(cpu_millis, memory_gib, 512)
}

fn source(repository: &str, digit: char) -> PersonalWorkerSourceIdentity {
    PersonalWorkerSourceIdentity::new(
        RepositoryRef::parse(repository).expect("repository"),
        CommitId::parse(&digit.to_string().repeat(40)).expect("commit"),
        GitTreeId::parse(&digit.to_string().repeat(40)).expect("tree"),
    )
}

fn identity(id: &str) -> ExecutionAdmissionIdentity {
    ExecutionAdmissionIdentity::new(
        ExecutionRequestId::parse(id).expect("request ID"),
        VerificationProfileId::parse("smolrunner.required").expect("profile"),
        RunnerProfileId::parse("personal-lima-work").expect("runner profile"),
    )
}

fn namespace(repository: &str) -> PersonalWorkerCacheNamespace {
    PersonalWorkerCacheNamespace::RepositoryBuild {
        cache_id: CacheId::parse("build-cache").expect("cache ID"),
        repository: RepositoryRef::parse(repository).expect("cache repository"),
        namespace_digest: Sha256Digest::parse(&format!("sha256:{}", "ab".repeat(32)))
            .expect("digest"),
    }
}

fn request(
    id: &str,
    repository: &str,
    digit: char,
    priority: PersonalWorkerPriority,
    submitted_at: u64,
    requested_limits: ExecutionResourceLimits,
    cache_access: PersonalWorkerCacheAccessMode,
) -> PersonalWorkerJobRequest {
    PersonalWorkerJobRequest {
        identity: identity(id),
        source: source(repository, digit),
        priority,
        requested_limits,
        cache_namespace: namespace(repository),
        cache_access,
        submitted_at: time(submitted_at),
        operator_deadline: None,
        cancellation: PersonalWorkerCancellationState::Active,
        fallback_eligibility: FallbackProfileEligibility::ineligible(),
    }
}

fn active(
    id: &str,
    repository: &str,
    digit: char,
    observed_at: u64,
    applied_limits: ExecutionResourceLimits,
    cache_access: PersonalWorkerCacheAccessMode,
) -> PersonalWorkerActiveReservation {
    let request = request(
        id,
        repository,
        digit,
        PersonalWorkerPriority::Normal,
        observed_at - 60_000,
        applied_limits,
        cache_access,
    );
    let admission = ExecutionAdmissionRecord::from_input(ExecutionAdmissionInput {
        identity: request.identity.clone(),
        state: ExecutionAdmissionState::Running,
        observed_at: time(observed_at),
        requested_limits: applied_limits,
        host_capacity: Some(HostCapacityObservation::new(
            time(observed_at),
            limits_with_pids(8_000, 10, 4_096),
        )),
        applied_limits: Some(applied_limits),
        queue_position: None,
        reservation: Some(ReservationEvidence::new(
            ReservationId::parse(&format!("reservation-{id}")).expect("reservation"),
            ReservationGeneration::new(1).expect("generation"),
            time(observed_at - 30_000),
            time(observed_at + 3_600_000),
        )),
        acknowledgement: None,
        fallback_eligibility: FallbackProfileEligibility::ineligible(),
        unavailable_reason: None,
    })
    .expect("active admission");
    PersonalWorkerActiveReservation {
        request,
        admission,
        started_at: Some(time(observed_at - 20_000)),
    }
}

fn input(
    generation: u64,
    observed_at: u64,
    queued: Vec<PersonalWorkerJobRequest>,
    active: Vec<PersonalWorkerActiveReservation>,
) -> PersonalWorkerQueueInput {
    PersonalWorkerQueueInput {
        generation: PersonalWorkerQueueGeneration::new(generation).expect("queue generation"),
        observed_at: time(observed_at),
        profile_observation: PersonalWorkerProfileObservation::observed(if active.is_empty() {
            PersonalWorkerProfile::Interactive
        } else {
            PersonalWorkerProfile::Work
        }),
        activity_evidence: PersonalWorkerActivityEvidence::observed(time(observed_at - 1_000)),
        queued,
        active,
        pending_profile_change: None,
    }
}

#[test]
fn priority_aging_promotes_old_background_work_without_losing_fifo() {
    let observed_at = 4_000_000;
    let old_background = request(
        "old-background",
        "teamleaderleo/smolrunner",
        'a',
        PersonalWorkerPriority::Background,
        observed_at - 2 * 30 * 60 * 1_000,
        limits(3_000, 3),
        PersonalWorkerCacheAccessMode::Read,
    );
    let recent_normal = request(
        "recent-normal",
        "example/other",
        'b',
        PersonalWorkerPriority::Normal,
        observed_at - 60_000,
        limits(3_000, 3),
        PersonalWorkerCacheAccessMode::Read,
    );

    let decision = evaluate_personal_worker_queue(
        &input(1, observed_at, vec![recent_normal, old_background], vec![]),
        None,
    )
    .expect("decision");

    assert_eq!(decision.selected.len(), 2);
    assert_eq!(decision.selected[0].request_id.as_str(), "old-background");
    assert_eq!(decision.selected[0].effective_priority_rank, 2);
}

#[test]
fn repository_fairness_prefers_an_unrepresented_repository() {
    let observed_at = 5_000_000;
    let running = active(
        "running-a",
        "teamleaderleo/smolrunner",
        'a',
        observed_at,
        limits(2_000, 2),
        PersonalWorkerCacheAccessMode::Read,
    );
    let same_repository = request(
        "queued-a",
        "teamleaderleo/smolrunner",
        'b',
        PersonalWorkerPriority::Normal,
        observed_at - 120_000,
        limits(2_000, 2),
        PersonalWorkerCacheAccessMode::Read,
    );
    let other_repository = request(
        "queued-b",
        "example/other",
        'c',
        PersonalWorkerPriority::Normal,
        observed_at - 60_000,
        limits(2_000, 2),
        PersonalWorkerCacheAccessMode::Read,
    );

    let decision = evaluate_personal_worker_queue(
        &input(
            1,
            observed_at,
            vec![same_repository, other_repository],
            vec![running],
        ),
        None,
    )
    .expect("decision");

    assert_eq!(decision.selected.len(), 1);
    assert_eq!(decision.selected[0].request_id.as_str(), "queued-b");
}

#[test]
fn one_heavy_job_excludes_other_work_while_two_light_jobs_can_share() {
    let observed_at = 6_000_000;
    let heavy = request(
        "heavy",
        "example/heavy",
        'a',
        PersonalWorkerPriority::Interactive,
        observed_at - 1_000,
        limits(6_000, 7),
        PersonalWorkerCacheAccessMode::Read,
    );
    let light = request(
        "light",
        "example/light",
        'b',
        PersonalWorkerPriority::Normal,
        observed_at - 2_000,
        limits(1_000, 1),
        PersonalWorkerCacheAccessMode::Read,
    );
    let heavy_decision =
        evaluate_personal_worker_queue(&input(1, observed_at, vec![light, heavy], vec![]), None)
            .expect("heavy decision");
    assert_eq!(heavy_decision.selected.len(), 1);
    assert_eq!(heavy_decision.selected[0].request_id.as_str(), "heavy");

    let light_a = request(
        "light-a",
        "example/a",
        'c',
        PersonalWorkerPriority::Normal,
        observed_at - 2_000,
        limits(3_000, 3),
        PersonalWorkerCacheAccessMode::Read,
    );
    let light_b = request(
        "light-b",
        "example/b",
        'd',
        PersonalWorkerPriority::Normal,
        observed_at - 1_000,
        limits(3_000, 3),
        PersonalWorkerCacheAccessMode::Read,
    );
    let light_decision = evaluate_personal_worker_queue(
        &input(1, observed_at, vec![light_a, light_b], vec![]),
        None,
    )
    .expect("light decision");
    assert_eq!(light_decision.selected.len(), 2);
}

#[test]
fn aggregate_reservations_preserve_the_fixed_guest_and_cache_reserve() {
    let observed_at = 7_000_000;
    let oversized = request(
        "oversized",
        "example/large",
        'a',
        PersonalWorkerPriority::Interactive,
        observed_at - 1_000,
        limits(7_001, 8),
        PersonalWorkerCacheAccessMode::Read,
    );
    let error =
        evaluate_personal_worker_queue(&input(1, observed_at, vec![oversized], vec![]), None)
            .expect_err("capacity refusal");
    assert_eq!(error.code, "personal_worker_reserve_violation");
}

#[test]
fn cache_writers_are_exclusive_but_readers_can_share() {
    let observed_at = 8_000_000;
    let writer = request(
        "writer",
        "teamleaderleo/smolrunner",
        'a',
        PersonalWorkerPriority::Interactive,
        observed_at - 2_000,
        limits(2_000, 2),
        PersonalWorkerCacheAccessMode::Write,
    );
    let reader = request(
        "reader",
        "teamleaderleo/smolrunner",
        'b',
        PersonalWorkerPriority::Normal,
        observed_at - 1_000,
        limits(2_000, 2),
        PersonalWorkerCacheAccessMode::Read,
    );
    let decision =
        evaluate_personal_worker_queue(&input(1, observed_at, vec![writer, reader], vec![]), None)
            .expect("writer decision");
    assert_eq!(decision.selected.len(), 1);
    let blocked = decision
        .visibility
        .iter()
        .find(|entry| entry.request_id.as_str() == "reader")
        .expect("reader visibility");
    assert_eq!(
        blocked.cache_lease,
        PersonalWorkerCacheLeaseState::BlockedByWriter
    );

    let read_a = request(
        "read-a",
        "teamleaderleo/smolrunner",
        'c',
        PersonalWorkerPriority::Normal,
        observed_at - 2_000,
        limits(2_000, 2),
        PersonalWorkerCacheAccessMode::Read,
    );
    let read_b = request(
        "read-b",
        "teamleaderleo/smolrunner",
        'd',
        PersonalWorkerPriority::Normal,
        observed_at - 1_000,
        limits(2_000, 2),
        PersonalWorkerCacheAccessMode::Read,
    );
    let read_decision =
        evaluate_personal_worker_queue(&input(1, observed_at, vec![read_a, read_b], vec![]), None)
            .expect("reader decision");
    assert_eq!(read_decision.selected.len(), 2);
}

#[test]
fn cancelled_work_remains_visible_and_never_receives_a_reservation() {
    let observed_at = 9_000_000;
    let mut cancelled = request(
        "cancelled",
        "example/cancelled",
        'a',
        PersonalWorkerPriority::Interactive,
        observed_at - 10_000,
        limits(2_000, 2),
        PersonalWorkerCacheAccessMode::Exclusive,
    );
    cancelled.cancellation = PersonalWorkerCancellationState::Cancelled {
        cancelled_at: time(observed_at - 1_000),
    };
    let decision =
        evaluate_personal_worker_queue(&input(1, observed_at, vec![cancelled], vec![]), None)
            .expect("cancelled decision");
    assert!(decision.selected.is_empty());
    assert_eq!(
        decision.visibility[0].state,
        PersonalWorkerQueueEntryState::Cancelled
    );
    assert_eq!(
        decision.visibility[0].cache_lease,
        PersonalWorkerCacheLeaseState::Available
    );
}

#[test]
fn queue_generation_must_advance_exactly_once() {
    let observed_at = 10_000_000;
    let first = evaluate_personal_worker_queue(&input(1, observed_at, vec![], vec![]), None)
        .expect("first generation");
    let error =
        evaluate_personal_worker_queue(&input(3, observed_at + 1, vec![], vec![]), Some(&first))
            .expect_err("generation gap");
    assert_eq!(error.code, "stale_or_skipped_queue_generation");
}

#[test]
fn desired_profile_uses_work_interactive_and_stopped_cooldowns() {
    let observed_at = 11_000_000;
    let queued = request(
        "new-work",
        "example/work",
        'a',
        PersonalWorkerPriority::Normal,
        observed_at - 1_000,
        limits(2_000, 2),
        PersonalWorkerCacheAccessMode::Read,
    );
    let mut work_input = input(1, observed_at, vec![queued], vec![]);
    work_input.profile_observation =
        PersonalWorkerProfileObservation::observed(PersonalWorkerProfile::Interactive);
    work_input.pending_profile_change = Some(PersonalWorkerPendingProfileChange {
        target: PersonalWorkerProfile::Stopped,
        requested_at: time(observed_at - 500),
    });
    let work = evaluate_personal_worker_queue(&work_input, None).expect("work profile");
    assert_eq!(work.desired_profile, PersonalWorkerProfile::Work);
    assert!(work.cancel_pending_downscale);
    assert!(work.profile_change_permitted);

    let mut interactive_input = input(1, observed_at, vec![], vec![]);
    interactive_input.profile_observation =
        PersonalWorkerProfileObservation::observed(PersonalWorkerProfile::Work);
    interactive_input.activity_evidence =
        PersonalWorkerActivityEvidence::observed(time(observed_at - 10 * 60 * 1_000));
    let interactive =
        evaluate_personal_worker_queue(&interactive_input, None).expect("interactive profile");
    assert_eq!(
        interactive.desired_profile,
        PersonalWorkerProfile::Interactive
    );

    let mut stopped_input = input(1, observed_at, vec![], vec![]);
    stopped_input.profile_observation =
        PersonalWorkerProfileObservation::observed(PersonalWorkerProfile::Interactive);
    stopped_input.activity_evidence =
        PersonalWorkerActivityEvidence::observed(time(observed_at - 31 * 60 * 1_000));
    let stopped = evaluate_personal_worker_queue(&stopped_input, None).expect("stopped profile");
    assert_eq!(stopped.desired_profile, PersonalWorkerProfile::Stopped);
}

#[test]
fn visibility_is_bounded_and_contains_no_private_execution_material() {
    let observed_at = 12_000_000;
    let queued = request(
        "visible",
        "teamleaderleo/smolrunner",
        'a',
        PersonalWorkerPriority::Normal,
        observed_at - 1_000,
        limits(2_000, 2),
        PersonalWorkerCacheAccessMode::Read,
    );
    let decision =
        evaluate_personal_worker_queue(&input(1, observed_at, vec![queued], vec![]), None)
            .expect("decision");
    let encoded = serde_json::to_string(&decision).expect("JSON");
    for private in [
        "/Users/operator/private",
        "/var/lib/private-cache",
        "TOKEN=private",
        "PRIVATE_STDOUT",
        "PRIVATE_STDERR",
        "cargo test --all-targets",
    ] {
        assert!(!encoded.contains(private));
    }
    assert!(encoded.contains("teamleaderleo/smolrunner"));
    assert!(encoded.contains("smolrunner.required"));
}

#[test]
fn fairness_is_recomputed_after_each_selected_repository() {
    let observed_at = 13_000_000;
    let same_oldest = request(
        "same-oldest",
        "example/same",
        'a',
        PersonalWorkerPriority::Normal,
        observed_at - 3_000,
        limits(2_000, 2),
        PersonalWorkerCacheAccessMode::Read,
    );
    let same_second = request(
        "same-second",
        "example/same",
        'b',
        PersonalWorkerPriority::Normal,
        observed_at - 2_000,
        limits(2_000, 2),
        PersonalWorkerCacheAccessMode::Read,
    );
    let other = request(
        "other",
        "example/other",
        'c',
        PersonalWorkerPriority::Normal,
        observed_at - 1_000,
        limits(2_000, 2),
        PersonalWorkerCacheAccessMode::Read,
    );
    let decision = evaluate_personal_worker_queue(
        &input(
            1,
            observed_at,
            vec![same_oldest, same_second, other],
            vec![],
        ),
        None,
    )
    .expect("fair decision");
    let selected = decision
        .selected
        .iter()
        .map(|entry| entry.request_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(selected, ["same-oldest", "other"]);
}

#[test]
fn pending_downscale_waits_for_active_drain_and_is_cancelled_by_work() {
    let observed_at = 14_000_000;
    let running = active(
        "running",
        "example/running",
        'a',
        observed_at,
        limits(2_000, 2),
        PersonalWorkerCacheAccessMode::Read,
    );
    let mut queue = input(1, observed_at, vec![], vec![running]);
    queue.pending_profile_change = Some(PersonalWorkerPendingProfileChange {
        target: PersonalWorkerProfile::Stopped,
        requested_at: time(observed_at - 1_000),
    });
    let decision = evaluate_personal_worker_queue(&queue, None).expect("drain decision");
    assert_eq!(decision.desired_profile, PersonalWorkerProfile::Work);
    assert!(decision.cancel_pending_downscale);
    assert!(!decision.profile_change_permitted);
}

#[test]
fn cancelled_only_queue_does_not_hold_the_work_profile() {
    let observed_at = 15_000_000;
    let mut cancelled = request(
        "cancelled-idle",
        "example/idle",
        'a',
        PersonalWorkerPriority::Normal,
        observed_at - 60_000,
        limits(2_000, 2),
        PersonalWorkerCacheAccessMode::Read,
    );
    cancelled.cancellation = PersonalWorkerCancellationState::Cancelled {
        cancelled_at: time(observed_at - 1_000),
    };
    let mut queue = input(1, observed_at, vec![cancelled], vec![]);
    queue.activity_evidence =
        PersonalWorkerActivityEvidence::observed(time(observed_at - 31 * 60 * 1_000));
    let decision = evaluate_personal_worker_queue(&queue, None).expect("idle decision");
    assert!(decision.selected.is_empty());
    assert_eq!(decision.desired_profile, PersonalWorkerProfile::Stopped);
    assert_eq!(
        decision.visibility[0].worker_profile,
        PersonalWorkerProfile::Stopped
    );
}

#[test]
fn previous_queue_schema_must_match() {
    let observed_at = 16_000_000;
    let mut previous = evaluate_personal_worker_queue(&input(1, observed_at, vec![], vec![]), None)
        .expect("previous decision");
    previous.schema_version = 99;
    let error =
        evaluate_personal_worker_queue(&input(2, observed_at + 1, vec![], vec![]), Some(&previous))
            .expect_err("schema drift");
    assert_eq!(error.code, "unsupported_previous_queue_schema");
}

#[test]
fn active_admission_time_is_bound_to_submission_and_queue_observation() {
    let observed_at = 17_000_000;
    let future = active(
        "future-admission",
        "example/future",
        'a',
        observed_at + 1_000,
        limits(2_000, 2),
        PersonalWorkerCacheAccessMode::Read,
    );
    let error = evaluate_personal_worker_queue(&input(1, observed_at, vec![], vec![future]), None)
        .expect_err("future admission");
    assert_eq!(error.code, "active_admission_time_out_of_bounds");

    let mut before_submission = active(
        "before-submission",
        "example/before",
        'b',
        observed_at - 1_000,
        limits(2_000, 2),
        PersonalWorkerCacheAccessMode::Read,
    );
    before_submission.request.submitted_at = time(observed_at);
    let error = evaluate_personal_worker_queue(
        &input(1, observed_at, vec![], vec![before_submission]),
        None,
    )
    .expect_err("admission before submission");
    assert_eq!(error.code, "active_admission_time_out_of_bounds");
}

#[test]
fn cancellation_and_start_cannot_postdate_active_admission() {
    let observed_at = 18_000_000;
    let mut cancelled = active(
        "late-cancellation",
        "example/cancel",
        'a',
        observed_at - 1_000,
        limits(2_000, 2),
        PersonalWorkerCacheAccessMode::Read,
    );
    cancelled.request.cancellation = PersonalWorkerCancellationState::Cancelled {
        cancelled_at: time(observed_at),
    };
    let error =
        evaluate_personal_worker_queue(&input(1, observed_at, vec![], vec![cancelled]), None)
            .expect_err("late cancellation");
    assert_eq!(error.code, "cancellation_after_admission_observation");

    let mut late_start = active(
        "late-start",
        "example/start",
        'b',
        observed_at - 1_000,
        limits(2_000, 2),
        PersonalWorkerCacheAccessMode::Read,
    );
    late_start.started_at = Some(time(observed_at));
    let error =
        evaluate_personal_worker_queue(&input(1, observed_at, vec![], vec![late_start]), None)
            .expect_err("late start");
    assert_eq!(error.code, "invalid_active_start_time");
}

#[test]
fn active_reservation_expiry_must_be_strictly_future() {
    let observed_at = 19_000_000;
    let request = request(
        "expiry-boundary",
        "example/expiry",
        'a',
        PersonalWorkerPriority::Normal,
        observed_at - 60_000,
        limits(2_000, 2),
        PersonalWorkerCacheAccessMode::Read,
    );
    let admission_observed_at = observed_at - 1_000;
    let admission = ExecutionAdmissionRecord::from_input(ExecutionAdmissionInput {
        identity: request.identity.clone(),
        state: ExecutionAdmissionState::Running,
        observed_at: time(admission_observed_at),
        requested_limits: request.requested_limits,
        host_capacity: Some(HostCapacityObservation::new(
            time(admission_observed_at),
            limits(8_000, 10),
        )),
        applied_limits: Some(request.requested_limits),
        queue_position: None,
        reservation: Some(ReservationEvidence::new(
            ReservationId::parse("reservation-expiry-boundary").expect("reservation"),
            ReservationGeneration::new(1).expect("generation"),
            time(observed_at - 30_000),
            time(observed_at),
        )),
        acknowledgement: None,
        fallback_eligibility: FallbackProfileEligibility::ineligible(),
        unavailable_reason: None,
    })
    .expect("active admission");
    let reservation = PersonalWorkerActiveReservation {
        request,
        admission,
        started_at: Some(time(observed_at - 20_000)),
    };
    let error =
        evaluate_personal_worker_queue(&input(1, observed_at, vec![], vec![reservation]), None)
            .expect_err("expiry boundary");
    assert_eq!(error.code, "expired_active_reservation");
}

#[test]
fn work_to_interactive_and_stopped_boundaries_are_exact() {
    let observed_at = 20_000_000;
    for (idle_millis, expected) in [
        (10 * 60 * 1_000 - 1, PersonalWorkerProfile::Work),
        (10 * 60 * 1_000, PersonalWorkerProfile::Interactive),
        (30 * 60 * 1_000 - 1, PersonalWorkerProfile::Interactive),
        (30 * 60 * 1_000, PersonalWorkerProfile::Stopped),
    ] {
        let mut queue = input(1, observed_at, vec![], vec![]);
        queue.profile_observation =
            PersonalWorkerProfileObservation::observed(PersonalWorkerProfile::Work);
        queue.activity_evidence =
            PersonalWorkerActivityEvidence::observed(time(observed_at - idle_millis));
        let decision =
            evaluate_personal_worker_queue(&queue, None).expect("cooldown boundary decision");
        assert_eq!(
            decision.desired_profile, expected,
            "idle_millis={idle_millis}"
        );
    }
}

#[test]
fn unobserved_never_active_state_stays_stopped_at_every_time_boundary() {
    for observed_at in [1_001, 10 * 60 * 1_000, 30 * 60 * 1_000, 86_400_000] {
        let mut queue = input(1, observed_at, vec![], vec![]);
        queue.profile_observation = PersonalWorkerProfileObservation::Unobserved;
        queue.activity_evidence = PersonalWorkerActivityEvidence::Never;
        let decision = evaluate_personal_worker_queue(&queue, None)
            .expect("unobserved never-active queue is valid");
        assert_eq!(decision.desired_profile, PersonalWorkerProfile::Stopped);
        assert_eq!(
            decision.profile_observation,
            PersonalWorkerProfileObservation::Unobserved
        );
        assert_eq!(
            decision.activity_evidence,
            PersonalWorkerActivityEvidence::Never
        );
    }
}

#[test]
fn active_and_pending_state_require_observed_profile_evidence() {
    let observed_at = 21_000_000;
    let reservation = active(
        "unobserved-active",
        "example/active",
        'a',
        observed_at - 1_000,
        limits(2_000, 2),
        PersonalWorkerCacheAccessMode::Read,
    );
    let mut active_queue = input(1, observed_at, vec![], vec![reservation]);
    active_queue.profile_observation = PersonalWorkerProfileObservation::Unobserved;
    let error = evaluate_personal_worker_queue(&active_queue, None)
        .expect_err("active work without observed profile");
    assert_eq!(error.code, "active_work_requires_work_profile");

    let mut pending_queue = input(1, observed_at, vec![], vec![]);
    pending_queue.profile_observation = PersonalWorkerProfileObservation::Unobserved;
    pending_queue.pending_profile_change = Some(PersonalWorkerPendingProfileChange {
        target: PersonalWorkerProfile::Stopped,
        requested_at: time(observed_at),
    });
    let error = evaluate_personal_worker_queue(&pending_queue, None)
        .expect_err("profile intent without observed profile");
    assert_eq!(error.code, "profile_intent_requires_observation");
}

#[test]
fn aggregate_pid_capacity_blocks_two_individually_valid_light_requests() {
    let observed_at = 23_000_000;
    let first = request(
        "pid-first",
        "example/pid-first",
        'a',
        PersonalWorkerPriority::Interactive,
        observed_at - 2_000,
        limits_with_pids(2_000, 2, 2_000),
        PersonalWorkerCacheAccessMode::Read,
    );
    let second = request(
        "pid-second",
        "example/pid-second",
        'b',
        PersonalWorkerPriority::Normal,
        observed_at - 1_000,
        limits_with_pids(2_000, 2, 2_000),
        PersonalWorkerCacheAccessMode::Read,
    );

    let decision =
        evaluate_personal_worker_queue(&input(1, observed_at, vec![second, first], vec![]), None)
            .expect("PID-bounded decision");

    assert_eq!(decision.schedulable_pids, 3_072);
    assert_eq!(decision.selected.len(), 1);
    assert_eq!(decision.selected[0].request_id.as_str(), "pid-first");
    let second_visibility = decision
        .visibility
        .iter()
        .find(|entry| entry.request_id.as_str() == "pid-second")
        .expect("second request visibility");
    assert_eq!(
        second_visibility.state,
        PersonalWorkerQueueEntryState::Queued
    );
}

#[test]
fn exact_aggregate_pid_boundary_allows_two_light_requests() {
    let observed_at = 24_000_000;
    let first = request(
        "pid-boundary-a",
        "example/pid-boundary-a",
        'a',
        PersonalWorkerPriority::Normal,
        observed_at - 2_000,
        limits_with_pids(2_000, 2, 1_536),
        PersonalWorkerCacheAccessMode::Read,
    );
    let second = request(
        "pid-boundary-b",
        "example/pid-boundary-b",
        'b',
        PersonalWorkerPriority::Normal,
        observed_at - 1_000,
        limits_with_pids(2_000, 2, 1_536),
        PersonalWorkerCacheAccessMode::Read,
    );

    let decision =
        evaluate_personal_worker_queue(&input(1, observed_at, vec![first, second], vec![]), None)
            .expect("exact PID boundary decision");

    assert_eq!(decision.selected.len(), 2);
    assert_eq!(decision.schedulable_pids, 3_072);
    let encoded = serde_json::to_string(&decision).expect("queue decision JSON");
    assert!(encoded.contains("\"schema_version\":3"));
    assert!(encoded.contains("\"schedulable_pids\":3072"));
}

#[test]
fn active_pid_overcommit_fails_closed() {
    let observed_at = 25_000_000;
    let first = active(
        "pid-active-a",
        "example/pid-active-a",
        'a',
        observed_at,
        limits_with_pids(1_000, 1, 2_048),
        PersonalWorkerCacheAccessMode::Read,
    );
    let second = active(
        "pid-active-b",
        "example/pid-active-b",
        'b',
        observed_at,
        limits_with_pids(1_000, 1, 1_025),
        PersonalWorkerCacheAccessMode::Read,
    );

    let error =
        evaluate_personal_worker_queue(&input(1, observed_at, vec![], vec![first, second]), None)
            .expect_err("active PID overcommit must fail closed");
    assert_eq!(error.code, "active_resource_overcommit");
}

#[test]
fn individual_pid_request_must_preserve_the_queue_reserve() {
    let observed_at = 26_000_000;
    let oversized = request(
        "pid-oversized",
        "example/pid-oversized",
        'a',
        PersonalWorkerPriority::Interactive,
        observed_at - 1_000,
        limits_with_pids(1_000, 1, 3_073),
        PersonalWorkerCacheAccessMode::Read,
    );

    let error =
        evaluate_personal_worker_queue(&input(1, observed_at, vec![oversized], vec![]), None)
            .expect_err("individual PID reserve violation");
    assert_eq!(error.code, "personal_worker_reserve_violation");
}
