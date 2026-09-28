#![allow(dead_code)]

use std::fs;

pub use glaeda::{
    execution_admission, lima_lifecycle, mac_availability, personal_worker_host_broker,
    personal_worker_queue, personal_worker_store,
};

use glaeda::artifact::{CommitId, GitTreeId, RepositoryRef, Sha256Digest};
use glaeda::execution_admission::{
    EpochMillis, ExecutionRequestId, ExecutionResourceLimits, HostCapacityObservation,
    ReservationId, RunnerProfileId,
};
use glaeda::lima_lifecycle::{
    LimaCacheDiskId, LimaCacheDiskIdentity, LimaInstanceId, LimaInstanceIdentity,
    LimaLifecycleObservation, LimaLifecycleObservationDefinition, LimaLifecyclePolicy,
    LimaLifecycleState, LimaObservedResources, LimaProfileGeneration, LimaResourceProfile,
};
use glaeda::mac_availability::{
    AvailabilityRequest, EffectiveAvailabilityMode, HostPowerSource, JobActivity,
    MacAvailabilityObservation, MemoryPressure, ObservationFreshness, VmPowerState,
};
use glaeda::personal_worker_host_broker::{HostBrokerRunnerObservation, HostBrokerRunnerState};
use glaeda::personal_worker_queue::{
    PERSONAL_WORKER_QUEUE_SCHEMA_VERSION, PERSONAL_WORKER_SCHEDULABLE_CPU_MILLIS,
    PERSONAL_WORKER_SCHEDULABLE_MEMORY_BYTES, PersonalWorkerActivityEvidence,
    PersonalWorkerCacheAccessMode, PersonalWorkerCacheLeaseState, PersonalWorkerCacheNamespace,
    PersonalWorkerJobClass, PersonalWorkerPriority, PersonalWorkerProfile,
    PersonalWorkerProfileObservation, PersonalWorkerQueueDecision, PersonalWorkerQueueEntryState,
    PersonalWorkerQueueGeneration, PersonalWorkerQueueVisibility, PersonalWorkerSelection,
};
use glaeda::personal_worker_store::PersonalWorkerStoreRevision;
use glaeda::verification_profile::{CacheId, VerificationProfileId};

#[path = "../src/personal_worker_tick.rs"]
mod personal_worker_tick;

use personal_worker_tick::{
    PersonalWorkerTickAction, PersonalWorkerTickBlocker, PersonalWorkerTickDrainStep,
    PersonalWorkerTickErrorKind, PersonalWorkerTickInput, PersonalWorkerTickJobStep,
    PersonalWorkerTickObservationTarget, PersonalWorkerTickPolicy,
};

const FRESHNESS: u64 = 30_000;

fn time(value: u64) -> EpochMillis {
    EpochMillis::new(value).expect("time")
}

fn generation(value: u64) -> LimaProfileGeneration {
    LimaProfileGeneration::new(value).expect("profile generation")
}

fn limits(cpu_millis: u32, memory_bytes: u64, pids: u32) -> ExecutionResourceLimits {
    ExecutionResourceLimits::new(cpu_millis, memory_bytes, pids).expect("resource limits")
}

fn identity() -> LimaInstanceIdentity {
    LimaInstanceIdentity::new(
        LimaInstanceId::parse("personal-lima").expect("instance"),
        LimaCacheDiskIdentity::new(
            LimaCacheDiskId::parse("personal-cache").expect("cache disk"),
            Sha256Digest::parse(&format!("sha256:{}", "ab".repeat(32))).expect("cache digest"),
        ),
    )
}

fn lifecycle(
    state: LimaLifecycleState,
    profile: LimaResourceProfile,
    observed_at: u64,
    last_activity_at: u64,
    active: bool,
) -> LimaLifecycleObservation {
    LimaLifecycleObservation::new(LimaLifecycleObservationDefinition {
        identity: identity(),
        state,
        profile,
        profile_generation: generation(3),
        observed_resources: LimaObservedResources::for_profile(profile),
        observed_at: time(observed_at),
        active_reservation_id: active
            .then(|| ReservationId::parse("reservation-active").expect("reservation identity")),
        last_activity_at: time(last_activity_at),
        idle_deadline: time(last_activity_at + profile.idle_deadline_offset_millis()),
        graceful_stop_acknowledgement: None,
    })
    .expect("lifecycle observation")
}

fn request_id(value: &str) -> ExecutionRequestId {
    ExecutionRequestId::parse(value).expect("request identity")
}

fn cache_namespace() -> PersonalWorkerCacheNamespace {
    PersonalWorkerCacheNamespace::RepositoryBuild {
        cache_id: CacheId::parse("cargo-build").expect("cache id"),
        repository: RepositoryRef::parse("teamleaderleo/smolrunner").expect("repository"),
        namespace_digest: Sha256Digest::parse(&format!("sha256:{}", "cd".repeat(32)))
            .expect("namespace digest"),
    }
}

fn selection(value: &str) -> PersonalWorkerSelection {
    PersonalWorkerSelection {
        request_id: request_id(value),
        repository: RepositoryRef::parse("teamleaderleo/smolrunner").expect("repository"),
        verification_profile_id: VerificationProfileId::parse("smolrunner.required")
            .expect("verification profile"),
        runner_profile_id: RunnerProfileId::parse("work").expect("runner profile"),
        priority: PersonalWorkerPriority::Normal,
        effective_priority_rank: 1,
        job_class: PersonalWorkerJobClass::Light,
        reserved_limits: limits(2_000, 2 * 1024 * 1024 * 1024, 768),
        cache_namespace: cache_namespace(),
        cache_access: PersonalWorkerCacheAccessMode::Write,
    }
}

fn visibility(value: &str, state: PersonalWorkerQueueEntryState) -> PersonalWorkerQueueVisibility {
    let reserved = matches!(
        state,
        PersonalWorkerQueueEntryState::Reserved
            | PersonalWorkerQueueEntryState::Starting
            | PersonalWorkerQueueEntryState::Running
            | PersonalWorkerQueueEntryState::Draining
    );
    PersonalWorkerQueueVisibility {
        request_id: request_id(value),
        repository: RepositoryRef::parse("teamleaderleo/smolrunner").expect("repository"),
        commit: CommitId::parse(&"12".repeat(20)).expect("commit"),
        tree: GitTreeId::parse(&"34".repeat(20)).expect("tree"),
        verification_profile_id: VerificationProfileId::parse("smolrunner.required")
            .expect("verification profile"),
        runner_profile_id: RunnerProfileId::parse("work").expect("runner profile"),
        priority: PersonalWorkerPriority::Normal,
        effective_priority_rank: 1,
        age_millis: 10,
        state,
        queue_position: (state == PersonalWorkerQueueEntryState::Queued).then_some(1),
        requested_cpu_millis: 2_000,
        requested_memory_bytes: 2 * 1024 * 1024 * 1024,
        reserved_cpu_millis: reserved.then_some(2_000),
        reserved_memory_bytes: reserved.then_some(2 * 1024 * 1024 * 1024),
        cache_namespace: cache_namespace(),
        cache_access: PersonalWorkerCacheAccessMode::Write,
        cache_lease: if reserved {
            PersonalWorkerCacheLeaseState::HeldWrite
        } else {
            PersonalWorkerCacheLeaseState::Available
        },
        start_time: matches!(
            state,
            PersonalWorkerQueueEntryState::Starting
                | PersonalWorkerQueueEntryState::Running
                | PersonalWorkerQueueEntryState::Draining
        )
        .then(|| time(990)),
        worker_profile: PersonalWorkerProfile::Work,
    }
}

fn queue(
    observed_at: u64,
    current: PersonalWorkerProfile,
    desired: PersonalWorkerProfile,
    selected: Vec<PersonalWorkerSelection>,
    visibility: Vec<PersonalWorkerQueueVisibility>,
) -> PersonalWorkerQueueDecision {
    PersonalWorkerQueueDecision {
        schema_version: PERSONAL_WORKER_QUEUE_SCHEMA_VERSION,
        generation: PersonalWorkerQueueGeneration::new(7).expect("queue generation"),
        observed_at: time(observed_at),
        profile_observation: PersonalWorkerProfileObservation::observed(current),
        activity_evidence: PersonalWorkerActivityEvidence::observed(time(observed_at - 1)),
        desired_profile: desired,
        cancel_pending_downscale: false,
        profile_change_permitted: visibility.iter().all(|entry| {
            !matches!(
                entry.state,
                PersonalWorkerQueueEntryState::Reserved
                    | PersonalWorkerQueueEntryState::Starting
                    | PersonalWorkerQueueEntryState::Running
                    | PersonalWorkerQueueEntryState::Draining
            )
        }),
        schedulable_cpu_millis: PERSONAL_WORKER_SCHEDULABLE_CPU_MILLIS,
        schedulable_memory_bytes: PERSONAL_WORKER_SCHEDULABLE_MEMORY_BYTES,
        selected,
        visibility,
    }
}

fn availability(
    mode: EffectiveAvailabilityMode,
    vm_power: VmPowerState,
    job_activity: JobActivity,
) -> MacAvailabilityObservation {
    MacAvailabilityObservation {
        effective_mode: mode,
        vm_power,
        job_activity,
        freshness: ObservationFreshness::Fresh,
        host_power: HostPowerSource::Ac,
        memory_pressure: MemoryPressure::Normal,
        operator_hold: false,
    }
}

fn runner(observed_at: u64, state: HostBrokerRunnerState) -> HostBrokerRunnerObservation {
    HostBrokerRunnerObservation::new(
        LimaInstanceId::parse("personal-lima").expect("instance"),
        generation(3),
        time(observed_at),
        state,
    )
}

fn policy() -> PersonalWorkerTickPolicy {
    PersonalWorkerTickPolicy::new(FRESHNESS, FRESHNESS, FRESHNESS).expect("tick policy")
}

fn lifecycle_policy() -> LimaLifecyclePolicy {
    LimaLifecyclePolicy::new(FRESHNESS).expect("lifecycle policy")
}

struct Evidence {
    queue: PersonalWorkerQueueDecision,
    lifecycle: Option<LimaLifecycleObservation>,
    runner: Option<HostBrokerRunnerObservation>,
    capacity: Option<HostCapacityObservation>,
    availability: Option<MacAvailabilityObservation>,
    availability_request: AvailabilityRequest,
    decision_at: u64,
}

fn plan(
    evidence: &Evidence,
) -> Result<
    personal_worker_tick::PersonalWorkerTickPlan,
    personal_worker_tick::PersonalWorkerTickError,
> {
    let lifecycle_policy = lifecycle_policy();
    policy().plan(PersonalWorkerTickInput {
        store_revision: PersonalWorkerStoreRevision::new(11).expect("store revision"),
        decision_at: time(evidence.decision_at),
        queue: &evidence.queue,
        lifecycle_policy: &lifecycle_policy,
        lifecycle: evidence.lifecycle.as_ref(),
        runner: evidence.runner.as_ref(),
        capacity: evidence.capacity,
        availability_request: evidence.availability_request,
        availability: evidence.availability,
    })
}

#[test]
fn stale_queue_is_observed_before_other_missing_evidence() {
    let evidence = Evidence {
        queue: queue(
            900,
            PersonalWorkerProfile::Stopped,
            PersonalWorkerProfile::Stopped,
            Vec::new(),
            Vec::new(),
        ),
        lifecycle: None,
        runner: None,
        capacity: None,
        availability: None,
        availability_request: AvailabilityRequest::Active,
        decision_at: 40_000,
    };

    let tick = plan(&evidence).expect("observation plan");

    assert_eq!(
        tick.action(),
        &PersonalWorkerTickAction::Observe {
            target: PersonalWorkerTickObservationTarget::Queue,
        }
    );
}

#[test]
fn stopped_work_progresses_one_profile_or_start_action_at_a_time() {
    let mut evidence = Evidence {
        queue: queue(
            1_000,
            PersonalWorkerProfile::Stopped,
            PersonalWorkerProfile::Work,
            vec![selection("request-one")],
            vec![visibility(
                "request-one",
                PersonalWorkerQueueEntryState::Selected,
            )],
        ),
        lifecycle: Some(lifecycle(
            LimaLifecycleState::Stopped,
            LimaResourceProfile::Interactive,
            1_000,
            999,
            false,
        )),
        runner: None,
        capacity: None,
        availability: Some(availability(
            EffectiveAvailabilityMode::Off,
            VmPowerState::Stopped,
            JobActivity::Idle,
        )),
        availability_request: AvailabilityRequest::Active,
        decision_at: 1_001,
    };

    let first = plan(&evidence).expect("profile action");
    assert!(matches!(
        first.action(),
        PersonalWorkerTickAction::ChangeProfile {
            from_profile: LimaResourceProfile::Interactive,
            to_profile: LimaResourceProfile::Work,
            current_generation,
            next_generation,
            ..
        } if current_generation.get() == 3 && next_generation.get() == 4
    ));

    evidence.lifecycle = Some(lifecycle(
        LimaLifecycleState::Stopped,
        LimaResourceProfile::Work,
        1_001,
        999,
        false,
    ));
    evidence.decision_at = 1_002;
    let missing = plan(&evidence).expect("start capacity observation");
    assert_eq!(
        missing.action(),
        &PersonalWorkerTickAction::Observe {
            target: PersonalWorkerTickObservationTarget::Capacity,
        }
    );

    evidence.capacity = Some(HostCapacityObservation::new(
        time(1_001),
        limits(4_000, 4 * 1024 * 1024 * 1024, 4_096),
    ));
    let insufficient = plan(&evidence).expect("start capacity blocker");
    assert_eq!(
        insufficient.action(),
        &PersonalWorkerTickAction::Blocked {
            blocker: PersonalWorkerTickBlocker::CapacityInsufficient,
        }
    );

    evidence.capacity = Some(HostCapacityObservation::new(
        time(1_001),
        limits(8_000, 12 * 1024 * 1024 * 1024, 4_096),
    ));
    let second = plan(&evidence).expect("start action");
    assert!(matches!(
        second.action(),
        PersonalWorkerTickAction::StartVm {
            profile: LimaResourceProfile::Work,
            profile_generation,
            capacity,
            ..
        } if profile_generation.get() == 3 && capacity.observed_at.get() == 1_001
    ));
}

#[test]
fn ready_selection_requires_one_fresh_fitting_capacity() {
    let base = || Evidence {
        queue: queue(
            1_000,
            PersonalWorkerProfile::Work,
            PersonalWorkerProfile::Work,
            vec![selection("request-one")],
            vec![visibility(
                "request-one",
                PersonalWorkerQueueEntryState::Selected,
            )],
        ),
        lifecycle: Some(lifecycle(
            LimaLifecycleState::Running,
            LimaResourceProfile::Work,
            1_000,
            999,
            false,
        )),
        runner: Some(runner(1_000, HostBrokerRunnerState::IdleReady)),
        capacity: None,
        availability: Some(availability(
            EffectiveAvailabilityMode::Active,
            VmPowerState::Running,
            JobActivity::Idle,
        )),
        availability_request: AvailabilityRequest::Active,
        decision_at: 1_001,
    };

    let missing = plan(&base()).expect("missing capacity plan");
    assert_eq!(
        missing.action(),
        &PersonalWorkerTickAction::Observe {
            target: PersonalWorkerTickObservationTarget::Capacity,
        }
    );

    let mut insufficient = base();
    insufficient.capacity = Some(HostCapacityObservation::new(
        time(1_000),
        limits(1_000, 1024, 64),
    ));
    let insufficient = plan(&insufficient).expect("capacity blocker");
    assert_eq!(
        insufficient.action(),
        &PersonalWorkerTickAction::Blocked {
            blocker: PersonalWorkerTickBlocker::CapacityInsufficient,
        }
    );

    let mut ready = base();
    ready.capacity = Some(HostCapacityObservation::new(
        time(1_000),
        limits(4_000, 4 * 1024 * 1024 * 1024, 2_048),
    ));
    let ready = plan(&ready).expect("reserve action");
    assert_eq!(ready.store_revision().get(), 11);
    assert_eq!(ready.queue_generation().get(), 7);
    assert_eq!(ready.decision_at().get(), 1_001);
    assert!(matches!(
        ready.action(),
        PersonalWorkerTickAction::RunJob {
            job: PersonalWorkerTickJobStep::Reserve { selection, capacity },
        } if selection.request_id.as_str() == "request-one" && capacity.observed_at.get() == 1_000
    ));
}

#[test]
fn stale_and_future_capacity_are_distinct_without_fallback() {
    let mut evidence = Evidence {
        queue: queue(
            40_000,
            PersonalWorkerProfile::Work,
            PersonalWorkerProfile::Work,
            vec![selection("request-one")],
            vec![visibility(
                "request-one",
                PersonalWorkerQueueEntryState::Selected,
            )],
        ),
        lifecycle: Some(lifecycle(
            LimaLifecycleState::Running,
            LimaResourceProfile::Work,
            40_000,
            39_999,
            false,
        )),
        runner: Some(runner(40_000, HostBrokerRunnerState::IdleReady)),
        capacity: Some(HostCapacityObservation::new(
            time(1_000),
            limits(4_000, 4 * 1024 * 1024 * 1024, 2_048),
        )),
        availability: Some(availability(
            EffectiveAvailabilityMode::Active,
            VmPowerState::Running,
            JobActivity::Idle,
        )),
        availability_request: AvailabilityRequest::Active,
        decision_at: 40_001,
    };

    let stale = plan(&evidence).expect("stale capacity observation");
    assert_eq!(
        stale.action(),
        &PersonalWorkerTickAction::Observe {
            target: PersonalWorkerTickObservationTarget::Capacity,
        }
    );

    evidence.capacity = Some(HostCapacityObservation::new(
        time(40_002),
        limits(4_000, 4 * 1024 * 1024 * 1024, 2_048),
    ));
    let future = plan(&evidence).expect_err("future capacity must fail closed");
    assert_eq!(future.kind(), PersonalWorkerTickErrorKind::CapacityEvidence);
    assert_eq!(future.code(), "future_observation");
}

#[test]
fn existing_reservation_executes_without_selecting_a_second_job() {
    let mut evidence = Evidence {
        queue: queue(
            1_000,
            PersonalWorkerProfile::Work,
            PersonalWorkerProfile::Work,
            Vec::new(),
            vec![visibility(
                "request-one",
                PersonalWorkerQueueEntryState::Reserved,
            )],
        ),
        lifecycle: Some(lifecycle(
            LimaLifecycleState::Running,
            LimaResourceProfile::Work,
            1_000,
            999,
            true,
        )),
        runner: Some(runner(1_000, HostBrokerRunnerState::IdleReady)),
        capacity: None,
        availability: Some(availability(
            EffectiveAvailabilityMode::Active,
            VmPowerState::Running,
            JobActivity::Active,
        )),
        availability_request: AvailabilityRequest::Active,
        decision_at: 1_001,
    };

    let missing_capacity = plan(&evidence).expect("capacity observation debt");
    assert_eq!(
        missing_capacity.action(),
        &PersonalWorkerTickAction::Observe {
            target: PersonalWorkerTickObservationTarget::Capacity,
        }
    );

    evidence.capacity = Some(HostCapacityObservation::new(
        time(1_000),
        limits(1_000, 1_024, 2_048),
    ));
    let insufficient = plan(&evidence).expect("reserved capacity blocker");
    assert_eq!(
        insufficient.action(),
        &PersonalWorkerTickAction::Blocked {
            blocker: PersonalWorkerTickBlocker::CapacityInsufficient,
        }
    );

    evidence.capacity = Some(HostCapacityObservation::new(
        time(1_000),
        limits(4_000, 4 * 1024 * 1024 * 1024, 2_048),
    ));

    let tick = plan(&evidence).expect("execute action");

    assert!(matches!(
        tick.action(),
        PersonalWorkerTickAction::RunJob {
            job: PersonalWorkerTickJobStep::Execute {
                request_id,
                capacity,
            },
        } if request_id.as_str() == "request-one" && capacity.observed_at.get() == 1_000
    ));

    evidence.queue.visibility[0].state = PersonalWorkerQueueEntryState::Starting;
    evidence.queue.visibility[0].start_time = Some(time(990));
    let starting = plan(&evidence).expect("starting observation debt");
    assert_eq!(
        starting.action(),
        &PersonalWorkerTickAction::Observe {
            target: PersonalWorkerTickObservationTarget::Runner,
        }
    );

    evidence.runner = Some(runner(
        1_000,
        HostBrokerRunnerState::Busy {
            request_id: request_id("request-one"),
        },
    ));
    let busy = plan(&evidence).expect("already busy runner");
    assert_eq!(
        busy.action(),
        &PersonalWorkerTickAction::Observe {
            target: PersonalWorkerTickObservationTarget::Runner,
        }
    );

    evidence.queue.visibility[0].state = PersonalWorkerQueueEntryState::Running;
    evidence.runner = Some(runner(1_000, HostBrokerRunnerState::IdleReady));
    let lost_process = plan(&evidence).expect("running attempt without busy runner evidence");
    assert_eq!(
        lost_process.action(),
        &PersonalWorkerTickAction::Observe {
            target: PersonalWorkerTickObservationTarget::Runner,
        }
    );
}

#[test]
fn exact_stop_requires_fresh_idle_or_offline_runner_evidence() {
    let base = |runner_state| Evidence {
        queue: queue(
            1_900_000,
            PersonalWorkerProfile::Work,
            PersonalWorkerProfile::Stopped,
            Vec::new(),
            Vec::new(),
        ),
        lifecycle: Some(lifecycle(
            LimaLifecycleState::Running,
            LimaResourceProfile::Work,
            1_900_000,
            100,
            false,
        )),
        runner: Some(runner(1_900_000, runner_state)),
        capacity: None,
        availability: Some(availability(
            EffectiveAvailabilityMode::Active,
            VmPowerState::Running,
            JobActivity::Idle,
        )),
        availability_request: AvailabilityRequest::Active,
        decision_at: 1_900_001,
    };

    let idle = plan(&base(HostBrokerRunnerState::IdleReady)).expect("idle stop action");
    assert!(matches!(
        idle.action(),
        PersonalWorkerTickAction::StopVm {
            current_profile: LimaResourceProfile::Work,
            target_after_stop: PersonalWorkerProfile::Stopped,
            ..
        }
    ));

    let stop = plan(&base(HostBrokerRunnerState::Offline)).expect("stop action");
    assert!(matches!(
        stop.action(),
        PersonalWorkerTickAction::StopVm {
            current_profile: LimaResourceProfile::Work,
            target_after_stop: PersonalWorkerProfile::Stopped,
            ..
        }
    ));
}

#[test]
fn stop_waits_for_fresh_exact_runner_identity() {
    let mut evidence = Evidence {
        queue: queue(
            1_900_000,
            PersonalWorkerProfile::Work,
            PersonalWorkerProfile::Stopped,
            Vec::new(),
            Vec::new(),
        ),
        lifecycle: Some(lifecycle(
            LimaLifecycleState::Running,
            LimaResourceProfile::Work,
            1_900_000,
            100,
            false,
        )),
        runner: Some(runner(1_800_000, HostBrokerRunnerState::Offline)),
        capacity: None,
        availability: Some(availability(
            EffectiveAvailabilityMode::Active,
            VmPowerState::Running,
            JobActivity::Idle,
        )),
        availability_request: AvailabilityRequest::Active,
        decision_at: 1_900_001,
    };

    let stale = plan(&evidence).expect("stale runner observation");
    assert_eq!(
        stale.action(),
        &PersonalWorkerTickAction::Observe {
            target: PersonalWorkerTickObservationTarget::Runner,
        }
    );

    evidence.runner = Some(runner(1_900_002, HostBrokerRunnerState::Offline));
    let future = plan(&evidence).expect_err("future runner must fail closed");
    assert_eq!(future.kind(), PersonalWorkerTickErrorKind::RunnerEvidence);
    assert_eq!(future.code(), "future_observation");
}

#[test]
fn pending_downscale_and_drained_release_remain_explicit_steps() {
    let mut cancel = Evidence {
        queue: queue(
            1_000,
            PersonalWorkerProfile::Stopped,
            PersonalWorkerProfile::Work,
            vec![selection("request-one")],
            vec![visibility(
                "request-one",
                PersonalWorkerQueueEntryState::Selected,
            )],
        ),
        lifecycle: None,
        runner: None,
        capacity: None,
        availability: None,
        availability_request: AvailabilityRequest::Active,
        decision_at: 1_001,
    };
    cancel.queue.cancel_pending_downscale = true;
    let cancel = plan(&cancel).expect("cancel pending downscale");
    assert_eq!(
        cancel.action(),
        &PersonalWorkerTickAction::RunJob {
            job: PersonalWorkerTickJobStep::CancelPendingDownscale {
                target: PersonalWorkerProfile::Work,
            },
        }
    );

    let release = Evidence {
        queue: queue(
            1_000,
            PersonalWorkerProfile::Work,
            PersonalWorkerProfile::Work,
            Vec::new(),
            vec![visibility(
                "request-one",
                PersonalWorkerQueueEntryState::Draining,
            )],
        ),
        lifecycle: Some(lifecycle(
            LimaLifecycleState::Draining,
            LimaResourceProfile::Work,
            1_000,
            999,
            true,
        )),
        runner: Some(runner(1_000, HostBrokerRunnerState::IdleReady)),
        capacity: None,
        availability: Some(availability(
            EffectiveAvailabilityMode::Active,
            VmPowerState::Running,
            JobActivity::Active,
        )),
        availability_request: AvailabilityRequest::Active,
        decision_at: 1_001,
    };
    let release = plan(&release).expect("release drained request");
    assert!(matches!(
        release.action(),
        PersonalWorkerTickAction::Drain {
            drain: PersonalWorkerTickDrainStep::Release { request_id },
        } if request_id.as_str() == "request-one"
    ));
}

#[test]
fn availability_and_concurrency_vetoes_never_widen_authority() {
    let mut off = Evidence {
        queue: queue(
            1_000,
            PersonalWorkerProfile::Stopped,
            PersonalWorkerProfile::Work,
            vec![selection("request-one")],
            vec![visibility(
                "request-one",
                PersonalWorkerQueueEntryState::Selected,
            )],
        ),
        lifecycle: Some(lifecycle(
            LimaLifecycleState::Stopped,
            LimaResourceProfile::Work,
            1_000,
            999,
            false,
        )),
        runner: None,
        capacity: None,
        availability: Some(availability(
            EffectiveAvailabilityMode::Off,
            VmPowerState::Stopped,
            JobActivity::Idle,
        )),
        availability_request: AvailabilityRequest::Off,
        decision_at: 1_001,
    };
    let blocked = plan(&off).expect("off blocker");
    assert_eq!(
        blocked.action(),
        &PersonalWorkerTickAction::Blocked {
            blocker: PersonalWorkerTickBlocker::AvailabilityOff,
        }
    );

    off.availability_request = AvailabilityRequest::Auto;
    let auto = plan(&off).expect("auto blocker");
    assert_eq!(
        auto.action(),
        &PersonalWorkerTickAction::Blocked {
            blocker: PersonalWorkerTickBlocker::AvailabilityAutoUnsupported,
        }
    );

    let mut pressure = Evidence {
        queue: queue(
            1_000,
            PersonalWorkerProfile::Stopped,
            PersonalWorkerProfile::Work,
            vec![selection("request-one")],
            vec![visibility(
                "request-one",
                PersonalWorkerQueueEntryState::Selected,
            )],
        ),
        lifecycle: Some(lifecycle(
            LimaLifecycleState::Stopped,
            LimaResourceProfile::Work,
            1_000,
            999,
            false,
        )),
        runner: None,
        capacity: None,
        availability: Some(availability(
            EffectiveAvailabilityMode::Off,
            VmPowerState::Stopped,
            JobActivity::Idle,
        )),
        availability_request: AvailabilityRequest::Active,
        decision_at: 1_001,
    };
    pressure
        .availability
        .as_mut()
        .expect("availability")
        .memory_pressure = MemoryPressure::Critical;
    let pressure = plan(&pressure).expect("memory pressure blocker");
    assert_eq!(
        pressure.action(),
        &PersonalWorkerTickAction::Blocked {
            blocker: PersonalWorkerTickBlocker::AvailabilityBlocked,
        }
    );

    let mut stale_availability = off;
    stale_availability.availability_request = AvailabilityRequest::Active;
    stale_availability
        .availability
        .as_mut()
        .expect("availability")
        .freshness = ObservationFreshness::Stale;
    let stale_availability = plan(&stale_availability).expect("stale availability observation");
    assert_eq!(
        stale_availability.action(),
        &PersonalWorkerTickAction::Observe {
            target: PersonalWorkerTickObservationTarget::Availability,
        }
    );

    let mut concurrent = Evidence {
        queue: queue(
            1_000,
            PersonalWorkerProfile::Stopped,
            PersonalWorkerProfile::Work,
            vec![selection("request-one")],
            vec![visibility(
                "request-one",
                PersonalWorkerQueueEntryState::Selected,
            )],
        ),
        lifecycle: None,
        runner: None,
        capacity: None,
        availability: None,
        availability_request: AvailabilityRequest::Active,
        decision_at: 1_001,
    };
    concurrent.availability_request = AvailabilityRequest::Active;
    concurrent.queue.selected.push(selection("request-two"));
    concurrent.lifecycle = Some(lifecycle(
        LimaLifecycleState::Running,
        LimaResourceProfile::Work,
        1_000,
        999,
        false,
    ));
    concurrent.queue.profile_observation =
        PersonalWorkerProfileObservation::observed(PersonalWorkerProfile::Work);
    concurrent.runner = Some(runner(1_000, HostBrokerRunnerState::IdleReady));
    concurrent.availability = Some(availability(
        EffectiveAvailabilityMode::Active,
        VmPowerState::Running,
        JobActivity::Idle,
    ));
    let concurrent = plan(&concurrent).expect("concurrency blocker");
    assert_eq!(
        concurrent.action(),
        &PersonalWorkerTickAction::Blocked {
            blocker: PersonalWorkerTickBlocker::ConcurrencyLimit,
        }
    );
}

#[test]
fn contradictory_cross_source_activity_fails_closed() {
    let evidence = Evidence {
        queue: queue(
            1_000,
            PersonalWorkerProfile::Work,
            PersonalWorkerProfile::Work,
            Vec::new(),
            vec![visibility(
                "request-one",
                PersonalWorkerQueueEntryState::Running,
            )],
        ),
        lifecycle: Some(lifecycle(
            LimaLifecycleState::Running,
            LimaResourceProfile::Work,
            1_000,
            999,
            true,
        )),
        runner: Some(runner(
            1_000,
            HostBrokerRunnerState::Busy {
                request_id: request_id("request-one"),
            },
        )),
        capacity: None,
        availability: Some(availability(
            EffectiveAvailabilityMode::Active,
            VmPowerState::Running,
            JobActivity::Idle,
        )),
        availability_request: AvailabilityRequest::Active,
        decision_at: 1_001,
    };

    let error = plan(&evidence).expect_err("activity mismatch");

    assert_eq!(
        error.kind(),
        PersonalWorkerTickErrorKind::AvailabilityEvidence
    );
    assert_eq!(error.code(), "availability_queue_activity_mismatch");

    let missing_runner = Evidence {
        queue: queue(
            1_000,
            PersonalWorkerProfile::Work,
            PersonalWorkerProfile::Work,
            Vec::new(),
            Vec::new(),
        ),
        lifecycle: Some(lifecycle(
            LimaLifecycleState::Running,
            LimaResourceProfile::Work,
            1_000,
            999,
            false,
        )),
        runner: None,
        capacity: None,
        availability: Some(availability(
            EffectiveAvailabilityMode::Active,
            VmPowerState::Running,
            JobActivity::Active,
        )),
        availability_request: AvailabilityRequest::Active,
        decision_at: 1_001,
    };
    let error = plan(&missing_runner)
        .expect_err("runner observation debt must not hide cross-source contradiction");
    assert_eq!(error.code(), "availability_queue_activity_mismatch");
}

#[test]
fn stable_empty_worker_is_satisfied_and_module_has_no_runtime_authority() {
    let evidence = Evidence {
        queue: queue(
            700_000,
            PersonalWorkerProfile::Interactive,
            PersonalWorkerProfile::Interactive,
            Vec::new(),
            Vec::new(),
        ),
        lifecycle: Some(lifecycle(
            LimaLifecycleState::Running,
            LimaResourceProfile::Interactive,
            700_000,
            100,
            false,
        )),
        runner: None,
        capacity: None,
        availability: Some(availability(
            EffectiveAvailabilityMode::Active,
            VmPowerState::Running,
            JobActivity::Idle,
        )),
        availability_request: AvailabilityRequest::Active,
        decision_at: 700_001,
    };

    let tick = plan(&evidence).expect("satisfied plan");
    assert_eq!(tick.action(), &PersonalWorkerTickAction::Satisfied);

    let source = fs::read_to_string("src/personal_worker_tick.rs").expect("module source");
    for forbidden in [
        "std::process",
        "std::fs",
        "std::env",
        "Command::new",
        "SystemTime",
        "limactl",
        "octocrab",
        "reqwest",
        "unsafe {",
    ] {
        assert!(
            !source.contains(forbidden),
            "forbidden authority: {forbidden}"
        );
    }
}
