use std::fmt;

use serde::Serialize;

use crate::execution_admission::{EpochMillis, ExecutionRequestId};
use crate::lima_lifecycle::{
    LimaInstanceId, LimaInstanceIdentity, LimaLifecycleObservation, LimaLifecyclePolicy,
    LimaLifecycleState, LimaLifecycleTarget, LimaProfileGeneration, LimaResourceProfile,
};
use crate::personal_worker_queue::{
    PERSONAL_WORKER_QUEUE_SCHEMA_VERSION, PersonalWorkerProfile, PersonalWorkerQueueDecision,
    PersonalWorkerQueueEntryState, PersonalWorkerQueueGeneration, PersonalWorkerSelection,
};

pub const PERSONAL_WORKER_HOST_BROKER_SCHEMA_VERSION: u8 = 1;
pub const MAX_HOST_BROKER_OBSERVATION_AGE_MILLIS: u64 = 300_000;
const MAX_HOST_BROKER_STATE_REVISION: u64 = 1_000_000_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct HostBrokerStateRevision(u64);

impl HostBrokerStateRevision {
    /// Construct one positive bounded durable broker-state revision.
    ///
    /// # Errors
    ///
    /// Returns an error for zero or an implementation-exceeding revision.
    pub fn new(value: u64) -> Result<Self, HostBrokerReducerError> {
        if !(1..=MAX_HOST_BROKER_STATE_REVISION).contains(&value) {
            return Err(HostBrokerReducerError::new(
                "state_revision",
                "invalid_state_revision",
                "broker state revision must be within the bounded positive range",
            ));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    fn next(self) -> Result<Self, HostBrokerReducerError> {
        let value = self.0.checked_add(1).ok_or_else(|| {
            HostBrokerReducerError::new(
                "state_revision",
                "state_revision_exhausted",
                "broker state revision space is exhausted",
            )
        })?;
        Self::new(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostBrokerObservationTarget {
    Queue,
    Lima,
    Runner,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum HostBrokerRunnerState {
    Offline,
    Starting,
    IdleReady,
    Busy { request_id: ExecutionRequestId },
    Draining { request_id: ExecutionRequestId },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HostBrokerRunnerObservation {
    instance_id: LimaInstanceId,
    profile_generation: LimaProfileGeneration,
    observed_at: EpochMillis,
    state: HostBrokerRunnerState,
}

impl HostBrokerRunnerObservation {
    #[must_use]
    pub const fn new(
        instance_id: LimaInstanceId,
        profile_generation: LimaProfileGeneration,
        observed_at: EpochMillis,
        state: HostBrokerRunnerState,
    ) -> Self {
        Self {
            instance_id,
            profile_generation,
            observed_at,
            state,
        }
    }

    #[must_use]
    pub const fn instance_id(&self) -> &LimaInstanceId {
        &self.instance_id
    }

    #[must_use]
    pub const fn profile_generation(&self) -> LimaProfileGeneration {
        self.profile_generation
    }

    #[must_use]
    pub const fn observed_at(&self) -> EpochMillis {
        self.observed_at
    }

    #[must_use]
    pub const fn state(&self) -> &HostBrokerRunnerState {
        &self.state
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum HostBrokerAction {
    Observe {
        target: HostBrokerObservationTarget,
    },
    Start {
        identity: LimaInstanceIdentity,
        profile: LimaResourceProfile,
        profile_generation: LimaProfileGeneration,
    },
    Stop {
        identity: LimaInstanceIdentity,
        current_profile: LimaResourceProfile,
        profile_generation: LimaProfileGeneration,
        target_after_stop: PersonalWorkerProfile,
    },
    ChangeProfile {
        identity: LimaInstanceIdentity,
        from_profile: LimaResourceProfile,
        to_profile: LimaResourceProfile,
        current_generation: LimaProfileGeneration,
        next_generation: LimaProfileGeneration,
    },
    WaitForRunner {
        identity: LimaInstanceIdentity,
        profile: LimaResourceProfile,
        profile_generation: LimaProfileGeneration,
    },
    Reserve {
        selection: PersonalWorkerSelection,
    },
    Release {
        request_id: ExecutionRequestId,
    },
    CancelDownscale {
        target: PersonalWorkerProfile,
    },
    NoOp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostBrokerReducerInput<'a> {
    pub state_revision: HostBrokerStateRevision,
    pub decision_at: EpochMillis,
    pub queue: &'a PersonalWorkerQueueDecision,
    pub lifecycle_policy: &'a LimaLifecyclePolicy,
    pub lifecycle: Option<&'a LimaLifecycleObservation>,
    pub runner: Option<&'a HostBrokerRunnerObservation>,
    pub previous: Option<&'a HostBrokerPlan>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HostBrokerPlan {
    schema_version: u8,
    state_revision: HostBrokerStateRevision,
    queue_generation: PersonalWorkerQueueGeneration,
    decision_at: EpochMillis,
    action: HostBrokerAction,
}

impl HostBrokerPlan {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn state_revision(&self) -> HostBrokerStateRevision {
        self.state_revision
    }

    #[must_use]
    pub const fn queue_generation(&self) -> PersonalWorkerQueueGeneration {
        self.queue_generation
    }

    #[must_use]
    pub const fn decision_at(&self) -> EpochMillis {
        self.decision_at
    }

    #[must_use]
    pub const fn action(&self) -> &HostBrokerAction {
        &self.action
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HostBrokerReducerPolicy {
    schema_version: u8,
    max_queue_age_millis: u64,
    max_runner_age_millis: u64,
}

impl HostBrokerReducerPolicy {
    /// Define bounded queue and runner freshness windows for pure broker decisions.
    ///
    /// # Errors
    ///
    /// Returns an error for a zero or excessive freshness window.
    pub fn new(
        max_queue_age_millis: u64,
        max_runner_age_millis: u64,
    ) -> Result<Self, HostBrokerReducerError> {
        validate_freshness_window("policy.max_queue_age_millis", max_queue_age_millis)?;
        validate_freshness_window("policy.max_runner_age_millis", max_runner_age_millis)?;
        Ok(Self {
            schema_version: PERSONAL_WORKER_HOST_BROKER_SCHEMA_VERSION,
            max_queue_age_millis,
            max_runner_age_millis,
        })
    }

    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    /// Reduce trusted durable queue, Lima lifecycle, and runner-readiness evidence into one action.
    ///
    /// The caller supplies every timestamp and observation. This function reads no clock, process,
    /// filesystem, network, credential, queue store, or VM state and performs no mutation.
    ///
    /// # Errors
    ///
    /// Returns a bounded static error for revision/generation reversal, unsupported schemas,
    /// queue/lifecycle policy drift, identity mismatch, impossible runner evidence, or unsafe profile
    /// movement while work remains active.
    pub fn reduce(
        &self,
        input: HostBrokerReducerInput<'_>,
    ) -> Result<HostBrokerPlan, HostBrokerReducerError> {
        validate_revision(&input)?;
        validate_queue_schema(input.queue)?;

        if observation_is_stale(
            "queue.observed_at",
            input.queue.observed_at,
            input.decision_at,
            self.max_queue_age_millis,
        )? {
            return Ok(plan(
                &input,
                HostBrokerAction::Observe {
                    target: HostBrokerObservationTarget::Queue,
                },
            ));
        }

        if input.queue.cancel_pending_downscale {
            if !queue_has_work(input.queue) {
                return Err(HostBrokerReducerError::new(
                    "queue.cancel_pending_downscale",
                    "unproven_downscale_cancellation",
                    "downscale cancellation requires queued, selected, reserved, or active work",
                ));
            }
            return Ok(plan(
                &input,
                HostBrokerAction::CancelDownscale {
                    target: PersonalWorkerProfile::Work,
                },
            ));
        }

        let Some(lifecycle) = input.lifecycle else {
            return Ok(plan(
                &input,
                HostBrokerAction::Observe {
                    target: HostBrokerObservationTarget::Lima,
                },
            ));
        };
        if lifecycle.observed_at() > input.decision_at {
            return Err(HostBrokerReducerError::new(
                "lifecycle.observed_at",
                "future_lifecycle_observation",
                "Lima lifecycle observation cannot be newer than the broker decision",
            ));
        }
        let lifecycle_target = match input
            .lifecycle_policy
            .desired_target(lifecycle, input.decision_at)
        {
            Ok(target) => target,
            Err(error) if error.code == "stale_observation" => {
                return Ok(plan(
                    &input,
                    HostBrokerAction::Observe {
                        target: HostBrokerObservationTarget::Lima,
                    },
                ));
            }
            Err(_) => {
                return Err(HostBrokerReducerError::new(
                    "lifecycle",
                    "invalid_lifecycle_evidence",
                    "Lima lifecycle evidence cannot produce a reviewed broker target",
                ));
            }
        };

        validate_queue_lifecycle_alignment(input.queue, lifecycle)?;
        let desired = desired_profile(input.queue, lifecycle_target)?;
        choose_action(self, &input, lifecycle, desired)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct HostBrokerReducerError {
    pub field: &'static str,
    pub code: &'static str,
    pub message: &'static str,
}

impl HostBrokerReducerError {
    const fn new(field: &'static str, code: &'static str, message: &'static str) -> Self {
        Self {
            field,
            code,
            message,
        }
    }
}

impl fmt::Display for HostBrokerReducerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for HostBrokerReducerError {}

fn validate_freshness_window(
    field: &'static str,
    value: u64,
) -> Result<(), HostBrokerReducerError> {
    if !(1..=MAX_HOST_BROKER_OBSERVATION_AGE_MILLIS).contains(&value) {
        return Err(HostBrokerReducerError::new(
            field,
            "invalid_freshness_window",
            "broker freshness window must be positive and within the reviewed maximum",
        ));
    }
    Ok(())
}

fn validate_revision(input: &HostBrokerReducerInput<'_>) -> Result<(), HostBrokerReducerError> {
    let Some(previous) = input.previous else {
        return Ok(());
    };
    if previous.schema_version != PERSONAL_WORKER_HOST_BROKER_SCHEMA_VERSION {
        return Err(HostBrokerReducerError::new(
            "previous.schema_version",
            "unsupported_previous_broker_schema",
            "previous broker plan schema is not supported",
        ));
    }
    if input.state_revision != previous.state_revision.next()? {
        return Err(HostBrokerReducerError::new(
            "state_revision",
            "stale_or_skipped_state_revision",
            "durable broker state revision must advance by exactly one",
        ));
    }
    if input.decision_at < previous.decision_at {
        return Err(HostBrokerReducerError::new(
            "decision_at",
            "broker_decision_time_reversal",
            "broker decision time cannot move backwards",
        ));
    }
    if input.queue.generation < previous.queue_generation {
        return Err(HostBrokerReducerError::new(
            "queue.generation",
            "queue_generation_reversal",
            "queue generation cannot move backwards across broker reductions",
        ));
    }
    Ok(())
}

fn validate_queue_schema(
    queue: &PersonalWorkerQueueDecision,
) -> Result<(), HostBrokerReducerError> {
    if queue.schema_version != PERSONAL_WORKER_QUEUE_SCHEMA_VERSION {
        return Err(HostBrokerReducerError::new(
            "queue.schema_version",
            "unsupported_queue_schema",
            "personal worker queue decision schema is not supported",
        ));
    }
    Ok(())
}

fn observation_is_stale(
    field: &'static str,
    observed_at: EpochMillis,
    decision_at: EpochMillis,
    max_age_millis: u64,
) -> Result<bool, HostBrokerReducerError> {
    let age = decision_at
        .get()
        .checked_sub(observed_at.get())
        .ok_or_else(|| {
            HostBrokerReducerError::new(
                field,
                "future_observation",
                "broker input observation cannot be newer than the decision time",
            )
        })?;
    Ok(age > max_age_millis)
}

fn queue_has_work(queue: &PersonalWorkerQueueDecision) -> bool {
    !queue.selected.is_empty()
        || queue
            .visibility
            .iter()
            .any(|entry| !matches!(entry.state, PersonalWorkerQueueEntryState::Cancelled))
}

fn queue_has_active_work(queue: &PersonalWorkerQueueDecision) -> bool {
    queue.visibility.iter().any(|entry| {
        matches!(
            entry.state,
            PersonalWorkerQueueEntryState::Reserved
                | PersonalWorkerQueueEntryState::Starting
                | PersonalWorkerQueueEntryState::Running
                | PersonalWorkerQueueEntryState::Draining
        )
    })
}

fn validate_queue_lifecycle_alignment(
    queue: &PersonalWorkerQueueDecision,
    lifecycle: &LimaLifecycleObservation,
) -> Result<(), HostBrokerReducerError> {
    let expected_current = match lifecycle.state() {
        LimaLifecycleState::Stopped => PersonalWorkerProfile::Stopped,
        LimaLifecycleState::Starting
        | LimaLifecycleState::Running
        | LimaLifecycleState::Draining
        | LimaLifecycleState::Stopping
        | LimaLifecycleState::Unavailable => map_lima_profile(lifecycle.profile()),
    };
    let current_profile = queue.profile_observation.profile().ok_or_else(|| {
        HostBrokerReducerError::new(
            "queue.profile_observation",
            "queue_profile_unobserved",
            "queue profile must be observed before lifecycle planning",
        )
    })?;
    if current_profile != expected_current {
        return Err(HostBrokerReducerError::new(
            "queue.profile_observation",
            "queue_lifecycle_profile_mismatch",
            "queue current profile must match the exact Lima lifecycle observation",
        ));
    }
    if queue_has_active_work(queue) && lifecycle.profile() != LimaResourceProfile::Work {
        return Err(HostBrokerReducerError::new(
            "lifecycle.profile",
            "active_work_requires_work_profile",
            "active broker work requires the exact Lima work profile",
        ));
    }
    if lifecycle.active_reservation_id().is_some() != queue_has_active_work(queue) {
        return Err(HostBrokerReducerError::new(
            "lifecycle.active_reservation_id",
            "queue_lifecycle_reservation_mismatch",
            "queue active-work evidence and Lima reservation evidence must agree",
        ));
    }
    Ok(())
}

fn desired_profile(
    queue: &PersonalWorkerQueueDecision,
    lifecycle_target: LimaLifecycleTarget,
) -> Result<PersonalWorkerProfile, HostBrokerReducerError> {
    let expected = if queue_has_work(queue) {
        PersonalWorkerProfile::Work
    } else {
        match lifecycle_target {
            LimaLifecycleTarget::Work => PersonalWorkerProfile::Work,
            LimaLifecycleTarget::Interactive => PersonalWorkerProfile::Interactive,
            LimaLifecycleTarget::Stopped => PersonalWorkerProfile::Stopped,
        }
    };
    if queue.desired_profile != expected {
        return Err(HostBrokerReducerError::new(
            "queue.desired_profile",
            "queue_lifecycle_policy_mismatch",
            "queue and lifecycle policies must select the same exact desired profile",
        ));
    }
    Ok(expected)
}

fn choose_action(
    policy: &HostBrokerReducerPolicy,
    input: &HostBrokerReducerInput<'_>,
    lifecycle: &LimaLifecycleObservation,
    desired: PersonalWorkerProfile,
) -> Result<HostBrokerPlan, HostBrokerReducerError> {
    if lifecycle.state() == LimaLifecycleState::Unavailable {
        return Err(HostBrokerReducerError::new(
            "lifecycle.state",
            "lima_unavailable",
            "unavailable Lima lifecycle evidence cannot produce an execution action",
        ));
    }

    match lifecycle.state() {
        LimaLifecycleState::Stopped => choose_stopped_action(input, lifecycle, desired),
        LimaLifecycleState::Starting => choose_wait_action(policy, input, lifecycle, desired),
        LimaLifecycleState::Running => choose_running_action(policy, input, lifecycle, desired),
        LimaLifecycleState::Draining => choose_draining_action(policy, input, lifecycle, desired),
        LimaLifecycleState::Stopping => Ok(plan(input, HostBrokerAction::NoOp)),
        LimaLifecycleState::Unavailable => unreachable!("handled before lifecycle match"),
    }
}

fn choose_stopped_action(
    input: &HostBrokerReducerInput<'_>,
    lifecycle: &LimaLifecycleObservation,
    desired: PersonalWorkerProfile,
) -> Result<HostBrokerPlan, HostBrokerReducerError> {
    match desired {
        PersonalWorkerProfile::Stopped => Ok(plan(input, HostBrokerAction::NoOp)),
        PersonalWorkerProfile::Interactive | PersonalWorkerProfile::Work => {
            let desired_lima = map_worker_profile(desired).expect("non-stopped profile");
            if lifecycle.profile() == desired_lima {
                Ok(plan(
                    input,
                    HostBrokerAction::Start {
                        identity: lifecycle.identity().clone(),
                        profile: desired_lima,
                        profile_generation: lifecycle.profile_generation(),
                    },
                ))
            } else {
                require_profile_change_permitted(input.queue)?;
                let next_generation = LimaProfileGeneration::new(
                    lifecycle
                        .profile_generation()
                        .get()
                        .checked_add(1)
                        .ok_or_else(|| {
                            HostBrokerReducerError::new(
                                "lifecycle.profile_generation",
                                "profile_generation_exhausted",
                                "Lima profile generation space is exhausted",
                            )
                        })?,
                )
                .map_err(|_| {
                    HostBrokerReducerError::new(
                        "lifecycle.profile_generation",
                        "invalid_next_profile_generation",
                        "next Lima profile generation is invalid",
                    )
                })?;
                Ok(plan(
                    input,
                    HostBrokerAction::ChangeProfile {
                        identity: lifecycle.identity().clone(),
                        from_profile: lifecycle.profile(),
                        to_profile: desired_lima,
                        current_generation: lifecycle.profile_generation(),
                        next_generation,
                    },
                ))
            }
        }
    }
}

fn choose_running_action(
    policy: &HostBrokerReducerPolicy,
    input: &HostBrokerReducerInput<'_>,
    lifecycle: &LimaLifecycleObservation,
    desired: PersonalWorkerProfile,
) -> Result<HostBrokerPlan, HostBrokerReducerError> {
    match desired {
        PersonalWorkerProfile::Work if lifecycle.profile() == LimaResourceProfile::Work => {
            choose_runner_action(policy, input, lifecycle)
        }
        PersonalWorkerProfile::Work => {
            require_profile_change_permitted(input.queue)?;
            Ok(stop_plan(input, lifecycle, PersonalWorkerProfile::Work))
        }
        PersonalWorkerProfile::Interactive
            if lifecycle.profile() == LimaResourceProfile::Interactive =>
        {
            Ok(plan(input, HostBrokerAction::NoOp))
        }
        PersonalWorkerProfile::Interactive => {
            require_profile_change_permitted(input.queue)?;
            Ok(stop_plan(
                input,
                lifecycle,
                PersonalWorkerProfile::Interactive,
            ))
        }
        PersonalWorkerProfile::Stopped => {
            require_profile_change_permitted(input.queue)?;
            Ok(stop_plan(input, lifecycle, PersonalWorkerProfile::Stopped))
        }
    }
}

fn choose_draining_action(
    policy: &HostBrokerReducerPolicy,
    input: &HostBrokerReducerInput<'_>,
    lifecycle: &LimaLifecycleObservation,
    desired: PersonalWorkerProfile,
) -> Result<HostBrokerPlan, HostBrokerReducerError> {
    if desired != PersonalWorkerProfile::Work && queue_has_active_work(input.queue) {
        return Err(HostBrokerReducerError::new(
            "queue.desired_profile",
            "downscale_while_draining",
            "broker cannot downscale while an active reservation is draining",
        ));
    }
    choose_runner_action(policy, input, lifecycle)
}

fn choose_wait_action(
    policy: &HostBrokerReducerPolicy,
    input: &HostBrokerReducerInput<'_>,
    lifecycle: &LimaLifecycleObservation,
    _desired: PersonalWorkerProfile,
) -> Result<HostBrokerPlan, HostBrokerReducerError> {
    match validated_runner(policy, input, lifecycle)? {
        RunnerEvidence::MissingOrStale => Ok(plan(
            input,
            HostBrokerAction::Observe {
                target: HostBrokerObservationTarget::Runner,
            },
        )),
        RunnerEvidence::Fresh(_) => Ok(wait_plan(input, lifecycle)),
    }
}

fn choose_runner_action(
    policy: &HostBrokerReducerPolicy,
    input: &HostBrokerReducerInput<'_>,
    lifecycle: &LimaLifecycleObservation,
) -> Result<HostBrokerPlan, HostBrokerReducerError> {
    let runner = match validated_runner(policy, input, lifecycle)? {
        RunnerEvidence::MissingOrStale => {
            return Ok(plan(
                input,
                HostBrokerAction::Observe {
                    target: HostBrokerObservationTarget::Runner,
                },
            ));
        }
        RunnerEvidence::Fresh(runner) => runner,
    };

    validate_runner_request_identity(input.queue, runner.state())?;
    match runner.state() {
        HostBrokerRunnerState::Offline | HostBrokerRunnerState::Starting => {
            Ok(wait_plan(input, lifecycle))
        }
        HostBrokerRunnerState::IdleReady => {
            if let Some(draining) = input
                .queue
                .visibility
                .iter()
                .find(|entry| entry.state == PersonalWorkerQueueEntryState::Draining)
            {
                return Ok(plan(
                    input,
                    HostBrokerAction::Release {
                        request_id: draining.request_id.clone(),
                    },
                ));
            }
            if let Some(selection) = input.queue.selected.first() {
                return Ok(plan(
                    input,
                    HostBrokerAction::Reserve {
                        selection: selection.clone(),
                    },
                ));
            }
            Ok(plan(input, HostBrokerAction::NoOp))
        }
        HostBrokerRunnerState::Busy { .. } | HostBrokerRunnerState::Draining { .. } => {
            Ok(plan(input, HostBrokerAction::NoOp))
        }
    }
}

enum RunnerEvidence<'a> {
    MissingOrStale,
    Fresh(&'a HostBrokerRunnerObservation),
}

fn validated_runner<'a>(
    policy: &HostBrokerReducerPolicy,
    input: &'a HostBrokerReducerInput<'_>,
    lifecycle: &LimaLifecycleObservation,
) -> Result<RunnerEvidence<'a>, HostBrokerReducerError> {
    let Some(runner) = input.runner else {
        return Ok(RunnerEvidence::MissingOrStale);
    };
    if observation_is_stale(
        "runner.observed_at",
        runner.observed_at,
        input.decision_at,
        policy.max_runner_age_millis,
    )? {
        return Ok(RunnerEvidence::MissingOrStale);
    }
    if runner.instance_id() != lifecycle.identity().instance_id() {
        return Err(HostBrokerReducerError::new(
            "runner.instance_id",
            "runner_instance_identity_mismatch",
            "runner readiness must bind the exact Lima instance identity",
        ));
    }
    if runner.profile_generation() != lifecycle.profile_generation() {
        return Err(HostBrokerReducerError::new(
            "runner.profile_generation",
            "runner_profile_generation_mismatch",
            "runner readiness must bind the exact Lima profile generation",
        ));
    }
    Ok(RunnerEvidence::Fresh(runner))
}

fn validate_runner_request_identity(
    queue: &PersonalWorkerQueueDecision,
    runner_state: &HostBrokerRunnerState,
) -> Result<(), HostBrokerReducerError> {
    let (request_id, expected_states): (&ExecutionRequestId, &[PersonalWorkerQueueEntryState]) =
        match runner_state {
            HostBrokerRunnerState::Busy { request_id } => (
                request_id,
                &[
                    PersonalWorkerQueueEntryState::Starting,
                    PersonalWorkerQueueEntryState::Running,
                ],
            ),
            HostBrokerRunnerState::Draining { request_id } => {
                (request_id, &[PersonalWorkerQueueEntryState::Draining])
            }
            HostBrokerRunnerState::Offline
            | HostBrokerRunnerState::Starting
            | HostBrokerRunnerState::IdleReady => return Ok(()),
        };
    if !queue
        .visibility
        .iter()
        .any(|entry| &entry.request_id == request_id && expected_states.contains(&entry.state))
    {
        return Err(HostBrokerReducerError::new(
            "runner.state.request_id",
            "runner_queue_identity_mismatch",
            "busy or draining runner identity must match exact active queue evidence",
        ));
    }
    Ok(())
}

fn require_profile_change_permitted(
    queue: &PersonalWorkerQueueDecision,
) -> Result<(), HostBrokerReducerError> {
    if !queue.profile_change_permitted || queue_has_active_work(queue) {
        return Err(HostBrokerReducerError::new(
            "queue.profile_change_permitted",
            "profile_change_while_active",
            "profile change or stop is forbidden while a reservation remains active",
        ));
    }
    Ok(())
}

fn map_lima_profile(profile: LimaResourceProfile) -> PersonalWorkerProfile {
    match profile {
        LimaResourceProfile::Interactive => PersonalWorkerProfile::Interactive,
        LimaResourceProfile::Work => PersonalWorkerProfile::Work,
    }
}

fn map_worker_profile(profile: PersonalWorkerProfile) -> Option<LimaResourceProfile> {
    match profile {
        PersonalWorkerProfile::Stopped => None,
        PersonalWorkerProfile::Interactive => Some(LimaResourceProfile::Interactive),
        PersonalWorkerProfile::Work => Some(LimaResourceProfile::Work),
    }
}

fn stop_plan(
    input: &HostBrokerReducerInput<'_>,
    lifecycle: &LimaLifecycleObservation,
    target_after_stop: PersonalWorkerProfile,
) -> HostBrokerPlan {
    plan(
        input,
        HostBrokerAction::Stop {
            identity: lifecycle.identity().clone(),
            current_profile: lifecycle.profile(),
            profile_generation: lifecycle.profile_generation(),
            target_after_stop,
        },
    )
}

fn wait_plan(
    input: &HostBrokerReducerInput<'_>,
    lifecycle: &LimaLifecycleObservation,
) -> HostBrokerPlan {
    plan(
        input,
        HostBrokerAction::WaitForRunner {
            identity: lifecycle.identity().clone(),
            profile: lifecycle.profile(),
            profile_generation: lifecycle.profile_generation(),
        },
    )
}

fn plan(input: &HostBrokerReducerInput<'_>, action: HostBrokerAction) -> HostBrokerPlan {
    HostBrokerPlan {
        schema_version: PERSONAL_WORKER_HOST_BROKER_SCHEMA_VERSION,
        state_revision: input.state_revision,
        queue_generation: input.queue.generation,
        decision_at: input.decision_at,
        action,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::{RepositoryRef, Sha256Digest};
    use crate::execution_admission::{
        ExecutionRequestId, ExecutionResourceLimits, RunnerProfileId,
    };
    use crate::lima_lifecycle::{
        INTERACTIVE_AFTER_IDLE_MILLIS, LimaCacheDiskId, LimaCacheDiskIdentity,
        LimaDrainAcknowledgement, LimaInstanceId, LimaLifecycleObservationDefinition,
        LimaObservedResources, STOP_AFTER_IDLE_MILLIS,
    };
    use crate::personal_worker_queue::{
        PERSONAL_WORKER_SCHEDULABLE_CPU_MILLIS, PERSONAL_WORKER_SCHEDULABLE_MEMORY_BYTES,
        PersonalWorkerCacheAccessMode, PersonalWorkerCacheNamespace, PersonalWorkerJobClass,
        PersonalWorkerPriority, PersonalWorkerQueueVisibility,
    };
    use crate::personal_worker_queue::{
        PersonalWorkerActivityEvidence, PersonalWorkerProfileObservation,
    };
    use crate::verification_profile::{CacheId, VerificationProfileId};

    const FRESHNESS: u64 = 30_000;

    fn time(value: u64) -> EpochMillis {
        EpochMillis::new(value).expect("time")
    }

    fn generation(value: u64) -> LimaProfileGeneration {
        LimaProfileGeneration::new(value).expect("generation")
    }

    fn instance() -> LimaInstanceIdentity {
        LimaInstanceIdentity::new(
            LimaInstanceId::parse("personal-lima").expect("instance"),
            LimaCacheDiskIdentity::new(
                LimaCacheDiskId::parse("personal-cache").expect("cache"),
                Sha256Digest::parse(&format!("sha256:{}", "ab".repeat(32))).expect("digest"),
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
            identity: instance(),
            state,
            profile,
            profile_generation: generation(3),
            observed_resources: LimaObservedResources::for_profile(profile),
            observed_at: time(observed_at),
            active_reservation_id: active.then(|| {
                crate::execution_admission::ReservationId::parse("reservation-active")
                    .expect("reservation")
            }),
            last_activity_at: time(last_activity_at),
            idle_deadline: time(last_activity_at + profile.idle_deadline_offset_millis()),
            graceful_stop_acknowledgement: None,
        })
        .expect("lifecycle")
    }

    fn decision(
        observed_at: u64,
        current: PersonalWorkerProfile,
        desired: PersonalWorkerProfile,
    ) -> PersonalWorkerQueueDecision {
        PersonalWorkerQueueDecision {
            schema_version: PERSONAL_WORKER_QUEUE_SCHEMA_VERSION,
            generation: PersonalWorkerQueueGeneration::new(1).expect("queue generation"),
            observed_at: time(observed_at),
            profile_observation: PersonalWorkerProfileObservation::observed(current),
            activity_evidence: PersonalWorkerActivityEvidence::observed(time(observed_at - 1)),
            desired_profile: desired,
            cancel_pending_downscale: false,
            profile_change_permitted: true,
            schedulable_cpu_millis: PERSONAL_WORKER_SCHEDULABLE_CPU_MILLIS,
            schedulable_memory_bytes: PERSONAL_WORKER_SCHEDULABLE_MEMORY_BYTES,
            schedulable_pids: 3_072,
            selected: Vec::new(),
            visibility: Vec::new(),
        }
    }

    fn policy() -> HostBrokerReducerPolicy {
        HostBrokerReducerPolicy::new(FRESHNESS, FRESHNESS).expect("policy")
    }

    fn lifecycle_policy() -> LimaLifecyclePolicy {
        LimaLifecyclePolicy::new(FRESHNESS).expect("lifecycle policy")
    }

    fn reduce<'a>(
        revision: u64,
        decision_at: u64,
        queue: &'a PersonalWorkerQueueDecision,
        lifecycle: Option<&'a LimaLifecycleObservation>,
        runner: Option<&'a HostBrokerRunnerObservation>,
        previous: Option<&'a HostBrokerPlan>,
    ) -> Result<HostBrokerPlan, HostBrokerReducerError> {
        let lifecycle_policy = lifecycle_policy();
        policy().reduce(HostBrokerReducerInput {
            state_revision: HostBrokerStateRevision::new(revision).expect("revision"),
            decision_at: time(decision_at),
            queue,
            lifecycle_policy: &lifecycle_policy,
            lifecycle,
            runner,
            previous,
        })
    }

    fn runner(observed_at: u64, state: HostBrokerRunnerState) -> HostBrokerRunnerObservation {
        HostBrokerRunnerObservation::new(
            LimaInstanceId::parse("personal-lima").expect("instance"),
            generation(3),
            time(observed_at),
            state,
        )
    }

    fn selection(id: &str) -> PersonalWorkerSelection {
        let repository = RepositoryRef::parse("teamleaderleo/smolrunner").expect("repository");
        PersonalWorkerSelection {
            request_id: ExecutionRequestId::parse(id).expect("request"),
            repository: repository.clone(),
            verification_profile_id: VerificationProfileId::parse("smolrunner.required")
                .expect("verification profile"),
            runner_profile_id: RunnerProfileId::parse("personal-lima-work")
                .expect("runner profile"),
            priority: PersonalWorkerPriority::Normal,
            effective_priority_rank: 1,
            job_class: PersonalWorkerJobClass::Light,
            reserved_limits: ExecutionResourceLimits::new(2_000, 2 * 1_024 * 1_024 * 1_024, 2_048)
                .expect("limits"),
            cache_namespace: PersonalWorkerCacheNamespace::RepositoryBuild {
                cache_id: CacheId::parse("build-cache").expect("cache ID"),
                repository,
                namespace_digest: Sha256Digest::parse(&format!("sha256:{}", "cd".repeat(32)))
                    .expect("namespace digest"),
            },
            cache_access: PersonalWorkerCacheAccessMode::Write,
        }
    }

    fn visibility(id: &str, state: PersonalWorkerQueueEntryState) -> PersonalWorkerQueueVisibility {
        let selection = selection(id);
        PersonalWorkerQueueVisibility {
            request_id: selection.request_id,
            repository: selection.repository.clone(),
            commit: crate::artifact::CommitId::parse(&"a".repeat(40)).expect("commit"),
            tree: crate::artifact::GitTreeId::parse(&"b".repeat(40)).expect("tree"),
            verification_profile_id: selection.verification_profile_id,
            runner_profile_id: selection.runner_profile_id,
            priority: selection.priority,
            effective_priority_rank: selection.effective_priority_rank,
            age_millis: 1_000,
            state,
            queue_position: None,
            requested_cpu_millis: selection.reserved_limits.cpu_millis,
            requested_memory_bytes: selection.reserved_limits.memory_bytes,
            reserved_cpu_millis: Some(selection.reserved_limits.cpu_millis),
            reserved_memory_bytes: Some(selection.reserved_limits.memory_bytes),
            cache_namespace: selection.cache_namespace,
            cache_access: selection.cache_access,
            cache_lease: crate::personal_worker_queue::PersonalWorkerCacheLeaseState::HeldWrite,
            start_time: Some(time(10_000)),
            worker_profile: PersonalWorkerProfile::Work,
        }
    }

    #[test]
    fn stale_queue_and_missing_lima_produce_typed_observations() {
        let stale_queue = decision(
            10_000,
            PersonalWorkerProfile::Work,
            PersonalWorkerProfile::Work,
        );
        assert_eq!(
            reduce(1, 50_001, &stale_queue, None, None, None)
                .expect("stale queue plan")
                .action(),
            &HostBrokerAction::Observe {
                target: HostBrokerObservationTarget::Queue,
            }
        );

        let fresh_queue = decision(
            50_000,
            PersonalWorkerProfile::Work,
            PersonalWorkerProfile::Work,
        );
        assert_eq!(
            reduce(1, 50_000, &fresh_queue, None, None, None)
                .expect("missing Lima plan")
                .action(),
            &HostBrokerAction::Observe {
                target: HostBrokerObservationTarget::Lima,
            }
        );
    }

    #[test]
    fn new_work_cancels_pending_downscale_before_lima_actions() {
        let mut queue = decision(
            100_000,
            PersonalWorkerProfile::Interactive,
            PersonalWorkerProfile::Work,
        );
        queue.cancel_pending_downscale = true;
        queue.selected.push(selection("queued-a"));
        let plan = reduce(1, 100_000, &queue, None, None, None).expect("cancel plan");
        assert_eq!(
            plan.action(),
            &HostBrokerAction::CancelDownscale {
                target: PersonalWorkerProfile::Work,
            }
        );
    }

    #[test]
    fn stopped_worker_starts_or_changes_to_exact_desired_profile() {
        let queue = decision(
            200_000,
            PersonalWorkerProfile::Stopped,
            PersonalWorkerProfile::Work,
        );
        let stopped_work = lifecycle(
            LimaLifecycleState::Stopped,
            LimaResourceProfile::Work,
            200_000,
            199_000,
            false,
        );
        assert!(matches!(
            reduce(1, 200_000, &queue, Some(&stopped_work), None, None)
                .expect("start")
                .action(),
            HostBrokerAction::Start {
                profile: LimaResourceProfile::Work,
                profile_generation,
                ..
            } if *profile_generation == generation(3)
        ));

        let stopped_interactive = lifecycle(
            LimaLifecycleState::Stopped,
            LimaResourceProfile::Interactive,
            200_000,
            199_000,
            false,
        );
        assert!(matches!(
            reduce(1, 200_000, &queue, Some(&stopped_interactive), None, None)
                .expect("change profile")
                .action(),
            HostBrokerAction::ChangeProfile {
                from_profile: LimaResourceProfile::Interactive,
                to_profile: LimaResourceProfile::Work,
                current_generation,
                next_generation,
                ..
            } if *current_generation == generation(3) && *next_generation == generation(4)
        ));
    }

    #[test]
    fn selected_work_reserves_only_after_exact_idle_ready_evidence() {
        let mut queue = decision(
            300_000,
            PersonalWorkerProfile::Work,
            PersonalWorkerProfile::Work,
        );
        queue.selected.push(selection("selected-a"));
        let running = lifecycle(
            LimaLifecycleState::Running,
            LimaResourceProfile::Work,
            300_000,
            299_000,
            false,
        );
        let ready = runner(300_000, HostBrokerRunnerState::IdleReady);
        assert!(matches!(
            reduce(1, 300_000, &queue, Some(&running), Some(&ready), None)
                .expect("reserve")
                .action(),
            HostBrokerAction::Reserve { selection }
                if selection.request_id.as_str() == "selected-a"
        ));
    }

    #[test]
    fn drained_active_request_releases_only_after_idle_ready() {
        let mut queue = decision(
            400_000,
            PersonalWorkerProfile::Work,
            PersonalWorkerProfile::Work,
        );
        queue.profile_change_permitted = false;
        queue.visibility.push(visibility(
            "draining-a",
            PersonalWorkerQueueEntryState::Draining,
        ));
        let draining = lifecycle(
            LimaLifecycleState::Draining,
            LimaResourceProfile::Work,
            400_000,
            399_000,
            true,
        );
        let ready = runner(400_000, HostBrokerRunnerState::IdleReady);
        assert_eq!(
            reduce(1, 400_000, &queue, Some(&draining), Some(&ready), None)
                .expect("release")
                .action(),
            &HostBrokerAction::Release {
                request_id: ExecutionRequestId::parse("draining-a").expect("request"),
            }
        );
    }

    #[test]
    fn exact_ten_and_thirty_minute_targets_produce_downscale_actions() {
        let last_activity = 500_000;
        let ten_minutes = last_activity + INTERACTIVE_AFTER_IDLE_MILLIS;
        let queue_interactive = decision(
            ten_minutes,
            PersonalWorkerProfile::Work,
            PersonalWorkerProfile::Interactive,
        );
        let running_work = lifecycle(
            LimaLifecycleState::Running,
            LimaResourceProfile::Work,
            ten_minutes,
            last_activity,
            false,
        );
        assert!(matches!(
            reduce(
                1,
                ten_minutes,
                &queue_interactive,
                Some(&running_work),
                None,
                None,
            )
            .expect("interactive downscale")
            .action(),
            HostBrokerAction::Stop {
                target_after_stop: PersonalWorkerProfile::Interactive,
                ..
            }
        ));

        let thirty_minutes = last_activity + STOP_AFTER_IDLE_MILLIS;
        let queue_stopped = decision(
            thirty_minutes,
            PersonalWorkerProfile::Interactive,
            PersonalWorkerProfile::Stopped,
        );
        let running_interactive = lifecycle(
            LimaLifecycleState::Running,
            LimaResourceProfile::Interactive,
            thirty_minutes,
            last_activity,
            false,
        );
        assert!(matches!(
            reduce(
                1,
                thirty_minutes,
                &queue_stopped,
                Some(&running_interactive),
                None,
                None,
            )
            .expect("stop")
            .action(),
            HostBrokerAction::Stop {
                target_after_stop: PersonalWorkerProfile::Stopped,
                ..
            }
        ));
    }

    #[test]
    fn runner_identity_and_durable_revision_drift_fail_closed() {
        let queue = decision(
            600_000,
            PersonalWorkerProfile::Work,
            PersonalWorkerProfile::Work,
        );
        let running = lifecycle(
            LimaLifecycleState::Running,
            LimaResourceProfile::Work,
            600_000,
            599_000,
            false,
        );
        let wrong_runner = HostBrokerRunnerObservation::new(
            LimaInstanceId::parse("wrong-instance").expect("instance"),
            generation(3),
            time(600_000),
            HostBrokerRunnerState::IdleReady,
        );
        assert_eq!(
            reduce(
                1,
                600_000,
                &queue,
                Some(&running),
                Some(&wrong_runner),
                None,
            )
            .expect_err("runner mismatch")
            .code,
            "runner_instance_identity_mismatch"
        );

        let ready = runner(600_000, HostBrokerRunnerState::IdleReady);
        let previous =
            reduce(1, 600_000, &queue, Some(&running), Some(&ready), None).expect("previous plan");
        assert_eq!(
            reduce(
                3,
                600_001,
                &queue,
                Some(&running),
                Some(&ready),
                Some(&previous),
            )
            .expect_err("skipped revision")
            .code,
            "stale_or_skipped_state_revision"
        );
    }

    #[test]
    fn unobserved_durable_profile_refuses_lima_mutation_planning() {
        let mut queue = decision(
            650_000,
            PersonalWorkerProfile::Stopped,
            PersonalWorkerProfile::Stopped,
        );
        queue.profile_observation = PersonalWorkerProfileObservation::Unobserved;
        let stopped = lifecycle(
            LimaLifecycleState::Stopped,
            LimaResourceProfile::Work,
            650_000,
            649_000,
            false,
        );
        assert_eq!(
            reduce(1, 650_000, &queue, Some(&stopped), None, None)
                .expect_err("unobserved durable profile")
                .code,
            "queue_profile_unobserved"
        );
    }

    #[test]
    fn public_plan_is_bounded_and_path_free() {
        let queue = decision(
            700_000,
            PersonalWorkerProfile::Stopped,
            PersonalWorkerProfile::Work,
        );
        let stopped = lifecycle(
            LimaLifecycleState::Stopped,
            LimaResourceProfile::Work,
            700_000,
            699_000,
            false,
        );
        let plan = reduce(1, 700_000, &queue, Some(&stopped), None, None).expect("plan");
        let debug = format!("{plan:?}");
        let json = serde_json::to_string(&plan).expect("JSON");
        for output in [debug, json] {
            assert!(!output.contains("/Users/"));
            assert!(!output.contains("limactl"));
            assert!(!output.contains("credential"));
            assert!(!output.contains("stdout"));
            assert!(!output.contains("stderr"));
        }
    }

    #[test]
    fn graceful_stop_acknowledgement_remains_lifecycle_owned() {
        let acknowledgement = crate::lima_lifecycle::GracefulStopAcknowledgement::new(
            time(1_000),
            generation(3),
            instance().cache_disk().clone(),
            LimaDrainAcknowledgement::Completed,
        );
        assert_eq!(acknowledgement.drain(), LimaDrainAcknowledgement::Completed);
    }
}
