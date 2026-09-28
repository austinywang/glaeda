//! Same-lock execution boundary for one disposable Lima clone.
//!
//! The injected transaction tests choose only an already-reserved attempt. Current durable state,
//! live capacity/cancellation evidence, time, prepared-source readiness, target absence, the fixed
//! command, its bounded process cleanup, and post-clone identity remain inside the canonical store
//! lock. The disposable worker coordinator supplies the live Scale Set veto through the
//! crate-sealed source before each pre-clone checkpoint boundary.

use std::fmt;
use std::io;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::artifact::Sha256Digest;
use crate::disposable_attempt_catalog::{
    DisposableAttemptCatalogAction, DisposableAttemptCatalogDocument,
    DisposableAttemptCatalogRevision, DisposableAttemptReservation,
};
use crate::disposable_attempt_state::DisposableAttemptRevision;
use crate::disposable_lima_worker::{
    DisposableLimaWorkerAdapter, DisposableLimaWorkerCommandKind, DisposableLimaWorkerCommandPlan,
};
use crate::disposable_prepared_template::{
    DisposablePreparedTemplateIdentity, current_disposable_prepared_template,
};
use crate::disposable_template_generation::DisposableTemplateGenerationDocument;
use crate::disposable_template_runtime::{
    ConfirmedDisposableCloneSource, DisposableTemplateRuntime, DisposableTemplateRuntimeDisposition,
};
use crate::disposable_worker_reconciler::{
    CapacityClaimId, DisposableAttemptId, DisposableAttemptPhase, DisposableVmId,
    DisposableVmIdentity, DisposableVmObservation, DisposableWorkerAction,
    DisposableWorkerReconcileInput, DisposableWorkerResources, ScaleSetRunnerObservation,
    reconcile_attempt,
};
use crate::execution_admission::EpochMillis;
use crate::github_scale_set_bridge::{ScaleSetBridgeClient, ScaleSetRunnerLookup};
use crate::github_scale_set_protocol::{ScaleSetRunnerName, ScaleSetRunnerReference};
use crate::lima_host_identity::{LimaHostIdentityAdapter, LimaHostIdentityErrorKind};
use crate::lima_observation::{
    LimaInstanceName, LimaObservationAdapter, LimaObservationClock, LimaObservationRefusalCode,
    LimaObservationRequest,
};
use crate::process::{
    CommandExecutor, CommandSpec, ExecutionRecord, ProcessExecutor, TimedCommandExecutor,
};
use crate::unix_personal_worker_store::UnixPersonalWorkerStore;

pub const DISPOSABLE_CLONE_RUNTIME_SCHEMA_VERSION: u8 = 1;
const OBSERVATION_TIMEOUT: Duration = Duration::from_secs(30);
const CLONE_ADMISSION_MAX_MILLIS: u64 = 30_000;

/// One live capacity/cancellation observation bound to the exact durable reservation.
pub struct DisposableCloneAdmissionObservation {
    catalog_revision: DisposableAttemptCatalogRevision,
    attempt_id: DisposableAttemptId,
    attempt_revision: DisposableAttemptRevision,
    capacity_claim_id: CapacityClaimId,
    vm_id: DisposableVmId,
    resources: DisposableWorkerResources,
    prepared_template_identity: DisposablePreparedTemplateIdentity,
    observed_at: EpochMillis,
    expires_at: EpochMillis,
    capacity_reserved: bool,
    cancellation_requested: bool,
}

impl DisposableCloneAdmissionObservation {
    pub(crate) fn new(
        catalog: &DisposableAttemptCatalogDocument,
        reservation: &DisposableAttemptReservation,
        observed_at: EpochMillis,
        expires_at: EpochMillis,
        capacity_reserved: bool,
        cancellation_requested: bool,
    ) -> Self {
        Self {
            catalog_revision: catalog.revision(),
            attempt_id: reservation.attempt().attempt_id().clone(),
            attempt_revision: reservation.attempt().revision(),
            capacity_claim_id: reservation.attempt().capacity_claim_id().clone(),
            vm_id: reservation.attempt().vm_id().clone(),
            resources: reservation.resources(),
            prepared_template_identity: reservation.prepared_template_identity().clone(),
            observed_at,
            expires_at,
            capacity_reserved,
            cancellation_requested,
        }
    }

    pub(crate) fn validate_for(
        &self,
        catalog: &DisposableAttemptCatalogDocument,
        reservation: &DisposableAttemptReservation,
        now: EpochMillis,
    ) -> Result<(), DisposableCloneRuntimeError> {
        self.validate_identity_and_freshness_for(catalog, reservation, now)?;
        if !self.capacity_reserved {
            return Err(DisposableCloneRuntimeError::recovery("clone_capacity_lost"));
        }
        if self.cancellation_requested {
            return Err(DisposableCloneRuntimeError::recovery("clone_cancelled"));
        }
        Ok(())
    }

    pub(crate) const fn capacity_reserved(&self) -> bool {
        self.capacity_reserved
    }

    pub(crate) const fn cancellation_requested(&self) -> bool {
        self.cancellation_requested
    }

    pub(crate) fn validate_identity_and_freshness_for(
        &self,
        catalog: &DisposableAttemptCatalogDocument,
        reservation: &DisposableAttemptReservation,
        now: EpochMillis,
    ) -> Result<(), DisposableCloneRuntimeError> {
        if self.catalog_revision != catalog.revision()
            || &self.attempt_id != reservation.attempt().attempt_id()
            || self.attempt_revision != reservation.attempt().revision()
            || &self.capacity_claim_id != reservation.attempt().capacity_claim_id()
            || &self.vm_id != reservation.attempt().vm_id()
            || self.resources != reservation.resources()
            || &self.prepared_template_identity != reservation.prepared_template_identity()
        {
            return Err(DisposableCloneRuntimeError::recovery(
                "clone_admission_identity_drift",
            ));
        }
        if self.observed_at > now
            || now > self.expires_at
            || self
                .expires_at
                .get()
                .checked_sub(self.observed_at.get())
                .is_none_or(|window| window > CLONE_ADMISSION_MAX_MILLIS)
        {
            return Err(observation("clone_admission_stale"));
        }
        Ok(())
    }
}

/// Live source for current capacity ownership and cancellation state.
///
/// The disposable worker coordinator supplies this evidence from a live zero-capacity Scale Set
/// poll. Clone execution and registration checkpointing each invoke it exactly once, after their
/// other non-mutating preflight, while holding the canonical store lock immediately before their
/// durable authority checkpoint. Earlier lifecycle polls belong to other phases and are not reused
/// as command or registration authority.
pub(crate) mod admission_seal {
    pub trait Sealed {}
}

pub trait DisposableCloneAdmissionSource: admission_seal::Sealed {
    fn observe(
        &self,
        catalog: &DisposableAttemptCatalogDocument,
        reservation: &DisposableAttemptReservation,
    ) -> Result<DisposableCloneAdmissionObservation, DisposableCloneRuntimeError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisposableCloneRuntimeErrorKind {
    InvalidConfiguration,
    DurableState,
    Observation,
    Command,
    RecoveryRequired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DisposableCloneRuntimeError {
    kind: DisposableCloneRuntimeErrorKind,
    code: &'static str,
    message: &'static str,
}

impl DisposableCloneRuntimeError {
    #[must_use]
    pub const fn kind(&self) -> DisposableCloneRuntimeErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }

    pub(crate) const fn durable(code: &'static str) -> Self {
        runtime_error(
            DisposableCloneRuntimeErrorKind::DurableState,
            code,
            "durable disposable-clone state is unavailable or inconsistent",
        )
    }

    pub(crate) const fn recovery(code: &'static str) -> Self {
        runtime_error(
            DisposableCloneRuntimeErrorKind::RecoveryRequired,
            code,
            "the disposable clone requires recovery before it can advance",
        )
    }

    pub(crate) const fn observation(code: &'static str) -> Self {
        observation(code)
    }

    pub(crate) const fn configuration(code: &'static str) -> Self {
        invalid_configuration(code)
    }

    pub(crate) const fn command(code: &'static str) -> Self {
        command(code)
    }
}

impl fmt::Display for DisposableCloneRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for DisposableCloneRuntimeError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DisposableCloneRuntimeReceipt {
    schema_version: u8,
    attempt_id: String,
    catalog_revision: u64,
    attempt_revision: u64,
    command_identity: Sha256Digest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DisposableCloneTransactionOutcome {
    PrecloneCheckpointed {
        attempt_id: String,
        phase: DisposableAttemptPhase,
    },
    RegistrationCheckpointed {
        attempt_id: String,
        phase: DisposableAttemptPhase,
    },
    Completed(DisposableCloneRuntimeReceipt),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DisposableCleanupTransactionOutcome {
    CleanupCheckpointed {
        attempt_id: String,
        phase: DisposableAttemptPhase,
    },
    VmDestroyed {
        attempt_id: String,
    },
    RunnerDeleted {
        attempt_id: String,
    },
    CapacityReleased {
        attempt_id: String,
    },
    AttemptRetired {
        attempt_id: String,
    },
}

/// Private bridge surface used only while reconciling exact durable cleanup ownership.
pub(crate) trait DisposableCleanupRunnerSource {
    fn observe_runner(
        &mut self,
        runner_name: &ScaleSetRunnerName,
    ) -> Result<ScaleSetRunnerLookup, DisposableCloneRuntimeError>;

    fn remove_runner(
        &mut self,
        runner: &ScaleSetRunnerReference,
    ) -> Result<(), DisposableCloneRuntimeError>;
}

impl DisposableCleanupRunnerSource for ScaleSetBridgeClient {
    fn observe_runner(
        &mut self,
        runner_name: &ScaleSetRunnerName,
    ) -> Result<ScaleSetRunnerLookup, DisposableCloneRuntimeError> {
        ScaleSetBridgeClient::observe_runner(self, runner_name).map_err(|_| {
            DisposableCloneRuntimeError::observation("cleanup_runner_observation_failed")
        })
    }

    fn remove_runner(
        &mut self,
        runner: &ScaleSetRunnerReference,
    ) -> Result<(), DisposableCloneRuntimeError> {
        ScaleSetBridgeClient::remove_runner(self, runner)
            .map_err(|_| DisposableCloneRuntimeError::recovery("cleanup_runner_delete_failed"))
    }
}

impl DisposableCloneRuntimeReceipt {
    pub(crate) fn from_owned_linux(
        attempt_id: &DisposableAttemptId,
        catalog_revision: u64,
        attempt_revision: u64,
        command_identity: Sha256Digest,
    ) -> Self {
        Self {
            schema_version: DISPOSABLE_CLONE_RUNTIME_SCHEMA_VERSION,
            attempt_id: attempt_id.as_str().to_owned(),
            catalog_revision,
            attempt_revision,
            command_identity,
        }
    }

    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub fn attempt_id(&self) -> &str {
        &self.attempt_id
    }

    #[must_use]
    pub const fn catalog_revision(&self) -> u64 {
        self.catalog_revision
    }

    #[must_use]
    pub const fn attempt_revision(&self) -> u64 {
        self.attempt_revision
    }

    #[must_use]
    pub const fn command_identity(&self) -> &Sha256Digest {
        &self.command_identity
    }
}

/// Fixed production runtime for one controller-owned prepared source template.
pub struct DisposableCloneRuntime {
    state_root: PathBuf,
    limactl_program: PathBuf,
    template_runtime: DisposableTemplateRuntime,
    worker: DisposableLimaWorkerAdapter,
}

impl fmt::Debug for DisposableCloneRuntime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DisposableCloneRuntime")
            .field("state_root", &"<private-state-root>")
            .field("limactl_program", &"<private-program-path>")
            .field("worker", &self.worker)
            .finish()
    }
}

impl DisposableCloneRuntime {
    /// Construct the fixed runtime around the checked-in prepared-template declaration.
    pub fn new(
        state_root: impl Into<PathBuf>,
        limactl_program: impl Into<PathBuf>,
        lima_home: impl Into<PathBuf>,
        source_instance: LimaInstanceName,
    ) -> Result<Self, DisposableCloneRuntimeError> {
        let state_root = state_root.into();
        let limactl_program = limactl_program.into();
        let lima_home = lima_home.into();
        let prepared = current_disposable_prepared_template()
            .map_err(|_| invalid_configuration("clone_prepared_template_invalid"))?;
        let template_runtime = DisposableTemplateRuntime::new(
            state_root.clone(),
            limactl_program.clone(),
            lima_home.clone(),
            source_instance.clone(),
        )
        .map_err(|_| invalid_configuration("clone_template_runtime_invalid"))?;
        let worker = DisposableLimaWorkerAdapter::new(
            limactl_program.clone(),
            lima_home,
            source_instance,
            &prepared,
        )
        .map_err(|_| invalid_configuration("clone_worker_runtime_invalid"))?;
        Ok(Self {
            state_root,
            limactl_program,
            template_runtime,
            worker,
        })
    }

    pub(crate) fn reconcile_template_once(
        &self,
        executor: &impl TimedCommandExecutor,
        clock: &impl CloneRuntimeClock,
    ) -> Result<DisposableTemplateRuntimeDisposition, DisposableCloneRuntimeError> {
        self.template_runtime
            .reconcile_once(executor, clock)
            .map(|receipt| receipt.disposition)
            .map_err(|error| DisposableCloneRuntimeError::recovery(error.code()))
    }

    /// Execute at most one clone using a crate-sealed live admission source.
    ///
    /// External callers cannot implement the sealed source or manufacture an admission
    /// observation. The production coordinator supplies the live zero-capacity Scale Set poll.
    pub fn clone_once(
        &self,
        attempt_id: &DisposableAttemptId,
        admission: &impl DisposableCloneAdmissionSource,
    ) -> Result<DisposableCloneRuntimeReceipt, DisposableCloneRuntimeError> {
        self.clone_once_with(
            attempt_id,
            admission,
            &ProcessExecutor,
            &SystemCloneRuntimeClock,
        )
    }

    /// Exercise the future transaction with injected admission, process, and clock sources.
    pub(crate) fn clone_once_with(
        &self,
        attempt_id: &DisposableAttemptId,
        admission: &impl DisposableCloneAdmissionSource,
        executor: &impl TimedCommandExecutor,
        clock: &impl CloneRuntimeClock,
    ) -> Result<DisposableCloneRuntimeReceipt, DisposableCloneRuntimeError> {
        let mut store =
            UnixPersonalWorkerStore::open_or_create_disposable_catalog(&self.state_root)
                .map_err(|_| DisposableCloneRuntimeError::durable("clone_catalog_unavailable"))?;
        match store
            .execute_disposable_clone_transaction(self, attempt_id, admission, executor, clock)?
        {
            DisposableCloneTransactionOutcome::Completed(receipt) => Ok(receipt),
            DisposableCloneTransactionOutcome::PrecloneCheckpointed { .. }
            | DisposableCloneTransactionOutcome::RegistrationCheckpointed { .. } => Err(
                DisposableCloneRuntimeError::recovery("clone_transaction_incomplete"),
            ),
        }
    }

    pub(crate) fn authorize_locked(
        &self,
        catalog: &DisposableAttemptCatalogDocument,
        attempt_id: &DisposableAttemptId,
        executor: &impl TimedCommandExecutor,
        clock: &impl CloneRuntimeClock,
    ) -> Result<DisposableAttemptCatalogAction, DisposableCloneRuntimeError> {
        let reservation = catalog
            .find_active(attempt_id)
            .ok_or_else(|| DisposableCloneRuntimeError::durable("clone_attempt_missing"))?;
        if reservation.attempt().phase() != DisposableAttemptPhase::Reserved {
            return Err(DisposableCloneRuntimeError::recovery(
                "clone_authorization_phase_mismatch",
            ));
        }
        let prepared_template_identity = current_disposable_prepared_template()
            .and_then(|manifest| manifest.identity())
            .map_err(|_| invalid_configuration("clone_prepared_template_invalid"))?;
        if reservation.prepared_template_identity() != &prepared_template_identity {
            return Err(DisposableCloneRuntimeError::recovery(
                "clone_prepared_template_drift",
            ));
        }
        let target_request = self
            .worker
            .target_observation_request(reservation)
            .map_err(|_| invalid_configuration("clone_target_request_invalid"))?;
        self.verify_limactl(executor)?;
        self.confirm_target_absent(&target_request, executor, clock)?;
        self.verify_limactl(executor)?;
        let checkpoint_at = clock
            .epoch_millis()
            .map_err(|_| observation("clone_clock_unavailable"))?;
        if checkpoint_at > reservation.attempt().not_after() {
            Ok(DisposableAttemptCatalogAction::BeginUnprovisionedRelease)
        } else {
            Ok(DisposableAttemptCatalogAction::AuthorizeClone)
        }
    }

    pub(crate) fn authorize_clone_execution_locked(
        &self,
        catalog: &DisposableAttemptCatalogDocument,
        attempt_id: &DisposableAttemptId,
        executor: &impl TimedCommandExecutor,
        clock: &impl CloneRuntimeClock,
    ) -> Result<Option<DisposableAttemptCatalogAction>, DisposableCloneRuntimeError> {
        let reservation = catalog
            .find_active(attempt_id)
            .ok_or_else(|| DisposableCloneRuntimeError::durable("clone_attempt_missing"))?;
        if reservation.attempt().phase() != DisposableAttemptPhase::CloneAuthorized {
            return Err(DisposableCloneRuntimeError::recovery(
                "clone_execution_phase_mismatch",
            ));
        }
        let _ = executor;
        let checkpoint_at = clock
            .epoch_millis()
            .map_err(|_| observation("clone_clock_unavailable"))?;
        if checkpoint_at > reservation.attempt().not_after() {
            Ok(Some(
                DisposableAttemptCatalogAction::BeginUnprovisionedRelease,
            ))
        } else {
            Ok(None)
        }
    }

    pub(crate) fn prepare_locked(
        &self,
        catalog: &DisposableAttemptCatalogDocument,
        generation: &DisposableTemplateGenerationDocument,
        attempt_id: &DisposableAttemptId,
        executor: &impl TimedCommandExecutor,
        clock: &impl CloneRuntimeClock,
    ) -> Result<PreparedClone, DisposableCloneRuntimeError> {
        let reservation = catalog
            .find_active(attempt_id)
            .ok_or_else(|| DisposableCloneRuntimeError::durable("clone_attempt_missing"))?;
        if reservation.attempt().phase() != DisposableAttemptPhase::CloneAuthorized {
            return Err(DisposableCloneRuntimeError::recovery(
                "clone_execution_phase_mismatch",
            ));
        }
        let source = self
            .template_runtime
            .confirm_stopped_clone_source(generation, executor, clock)
            .map_err(|_| observation("clone_source_not_ready"))?;
        let target_request = self
            .worker
            .target_observation_request(reservation)
            .map_err(|_| invalid_configuration("clone_target_request_invalid"))?;
        self.verify_limactl(executor)?;
        self.confirm_target_absent(&target_request, executor, clock)?;
        source
            .confirm_current()
            .map_err(|_| observation("clone_source_drift"))?;
        let now = clock
            .epoch_millis()
            .map_err(|_| observation("clone_clock_unavailable"))?;
        if now > reservation.attempt().not_after() {
            return Err(DisposableCloneRuntimeError::recovery(
                "clone_expired_before_command",
            ));
        }
        let action = DisposableWorkerAction::CheckpointAndCloneVm;
        let plan = self
            .worker
            .plan(now, reservation, &action)
            .map_err(|_| DisposableCloneRuntimeError::recovery("clone_plan_refused"))?;
        if plan.kind() != DisposableLimaWorkerCommandKind::Clone {
            return Err(DisposableCloneRuntimeError::recovery(
                "clone_plan_kind_mismatch",
            ));
        }
        Ok(PreparedClone {
            plan,
            target_request,
            source,
            generation: generation.clone(),
        })
    }

    pub(crate) fn preflight_checkpointed_clone_locked(
        &self,
        started: &DisposableAttemptCatalogDocument,
        attempt_id: &DisposableAttemptId,
        prepared: PreparedClone,
        admission: &impl DisposableCloneAdmissionSource,
        executor: &impl TimedCommandExecutor,
        clock: &impl CloneRuntimeClock,
    ) -> Result<CheckpointReadyClone, DisposableCloneRuntimeError> {
        let reservation = started
            .find_active(attempt_id)
            .ok_or_else(|| DisposableCloneRuntimeError::durable("clone_attempt_missing"))?;
        let attempt = reservation.attempt();
        if attempt.phase() != DisposableAttemptPhase::CloneStarted
            || attempt.revision().get() != prepared.plan.attempt_revision() + 1
            || attempt.attempt_id().as_str() != prepared.plan.attempt_id()
            || attempt.vm_id().as_str() != prepared.plan.vm_id()
            || reservation.prepared_template_identity()
                != prepared.plan.prepared_template_identity()
            || attempt.vm_identity().is_some()
        {
            return Err(DisposableCloneRuntimeError::recovery(
                "clone_started_checkpoint_mismatch",
            ));
        }
        let now = clock
            .epoch_millis()
            .map_err(|_| observation("clone_clock_unavailable"))?;
        if now > attempt.not_after() {
            return Err(DisposableCloneRuntimeError::recovery(
                "clone_expired_before_command",
            ));
        }
        prepared
            .source
            .confirm_current()
            .map_err(|_| observation("clone_source_drift"))?;
        let fresh_source = self
            .template_runtime
            .confirm_stopped_clone_source(&prepared.generation, executor, clock)
            .map_err(|_| observation("clone_source_drift"))?;
        self.confirm_target_absent(&prepared.target_request, executor, clock)?;
        fresh_source
            .confirm_current()
            .map_err(|_| observation("clone_source_drift"))?;
        self.verify_limactl(executor)?;
        // This is the only live admission poll in the clone transaction. Any earlier sample would
        // be superseded by the source, target, and Lima checks above while adding a full GitHub
        // long-poll interval to the assignment-to-runner latency.
        let final_admission_observation = admission.observe(started, reservation)?;
        let command_now = clock
            .epoch_millis()
            .map_err(|_| observation("clone_clock_unavailable"))?;
        if command_now > attempt.not_after() {
            return Err(DisposableCloneRuntimeError::recovery(
                "clone_expired_before_command",
            ));
        }
        final_admission_observation.validate_for(started, reservation, command_now)?;

        Ok(CheckpointReadyClone {
            prepared,
            expected_disk_bytes: reservation.resources().disk_bytes(),
        })
    }

    pub(crate) fn execute_checkpointed_clone_locked(
        &self,
        checkpoint_ready: CheckpointReadyClone,
        executor: &impl TimedCommandExecutor,
        clock: &impl CloneRuntimeClock,
    ) -> Result<(DisposableVmIdentity, DisposableLimaWorkerCommandPlan), DisposableCloneRuntimeError>
    {
        let CheckpointReadyClone {
            prepared,
            expected_disk_bytes,
        } = checkpoint_ready;
        let record = executor
            .execute_with_timeout(prepared.plan.command(), prepared.plan.timeout())
            .map_err(|_| command("clone_command_failed"))?;
        validate_record(prepared.plan.command(), &record)?;
        let host = LimaHostIdentityAdapter
            .observe(&prepared.target_request)
            .map_err(|_| observation("clone_host_identity_unavailable"))?;
        if host.root_disk_bytes() != expected_disk_bytes {
            return Err(observation("clone_host_disk_mismatch"));
        }
        host.confirm(&prepared.target_request)
            .map_err(|_| observation("clone_host_identity_drift"))?;
        self.template_runtime
            .confirm_stopped_clone_source(&prepared.generation, executor, clock)
            .map_err(|_| observation("clone_source_drift"))?;
        host.confirm(&prepared.target_request)
            .map_err(|_| observation("clone_host_identity_drift"))?;
        Ok((
            DisposableVmIdentity::from_host_identity(host.identity()),
            prepared.plan,
        ))
    }

    pub(crate) fn receipt(
        &self,
        bound: &DisposableAttemptCatalogDocument,
        attempt_id: &DisposableAttemptId,
        plan: &DisposableLimaWorkerCommandPlan,
    ) -> Result<DisposableCloneRuntimeReceipt, DisposableCloneRuntimeError> {
        let attempt = bound
            .find_active(attempt_id)
            .ok_or_else(|| DisposableCloneRuntimeError::durable("clone_attempt_missing"))?
            .attempt();
        if attempt.phase() != DisposableAttemptPhase::CloneStarted
            || attempt.vm_identity().is_none()
        {
            return Err(DisposableCloneRuntimeError::recovery(
                "clone_identity_not_bound",
            ));
        }
        Ok(DisposableCloneRuntimeReceipt {
            schema_version: DISPOSABLE_CLONE_RUNTIME_SCHEMA_VERSION,
            attempt_id: attempt_id.as_str().to_owned(),
            catalog_revision: bound.revision().get(),
            attempt_revision: attempt.revision().get(),
            command_identity: plan.command_identity().clone(),
        })
    }

    pub(crate) fn authorize_registration_locked(
        &self,
        catalog: &DisposableAttemptCatalogDocument,
        attempt_id: &DisposableAttemptId,
        admission: &impl DisposableCloneAdmissionSource,
        executor: &impl TimedCommandExecutor,
        clock: &impl CloneRuntimeClock,
    ) -> Result<DisposableAttemptCatalogAction, DisposableCloneRuntimeError> {
        let reservation = catalog
            .find_active(attempt_id)
            .ok_or_else(|| DisposableCloneRuntimeError::durable("clone_attempt_missing"))?;
        if reservation.attempt().phase() != DisposableAttemptPhase::CloneStarted
            || reservation.attempt().vm_identity().is_none()
        {
            return Err(DisposableCloneRuntimeError::recovery(
                "clone_registration_phase_mismatch",
            ));
        }
        let preflight_at = clock
            .epoch_millis()
            .map_err(|_| observation("clone_clock_unavailable"))?;
        if preflight_at > reservation.attempt().not_after() {
            let action = reconcile_attempt(DisposableWorkerReconcileInput {
                now: preflight_at,
                attempt: reservation.attempt(),
                vm: DisposableVmObservation::Unknown,
                vm_identity: None,
                runner: ScaleSetRunnerObservation::Unknown,
                job_event: None,
                capacity_reserved: true,
                cancellation_requested: false,
            })
            .map_err(|_| DisposableCloneRuntimeError::recovery("clone_reconcile_failed"))?;
            if action
                == (DisposableWorkerAction::Persist {
                    transition: DisposableAttemptCatalogAction::BeginCleanup,
                })
            {
                return Ok(DisposableAttemptCatalogAction::BeginCleanup);
            }
            return Err(DisposableCloneRuntimeError::recovery(
                "clone_cleanup_checkpoint_refused",
            ));
        }
        let ready = self.confirm_ready_worker_bound(reservation, executor, clock)?;
        ready
            .confirm_current()
            .map_err(|_| observation("clone_worker_identity_drift"))?;
        // Guest readiness is the expensive part of this transaction. Observe it before the live
        // Scale Set gate so the gate remains fresh at the checkpoint barrier. No workload or JIT
        // credential exists yet, and the retained host observations bracket the live poll below.
        let admission_observation = admission.observe(catalog, reservation)?;
        let observed_at = clock
            .epoch_millis()
            .map_err(|_| observation("clone_clock_unavailable"))?;
        admission_observation.validate_identity_and_freshness_for(
            catalog,
            reservation,
            observed_at,
        )?;
        ready
            .confirm_current()
            .map_err(|_| observation("clone_worker_identity_drift"))?;
        let cleanup = !admission_observation.capacity_reserved
            || admission_observation.cancellation_requested
            || observed_at > reservation.attempt().not_after();
        if cleanup {
            let action = reconcile_attempt(DisposableWorkerReconcileInput {
                now: observed_at,
                attempt: reservation.attempt(),
                vm: DisposableVmObservation::Unknown,
                vm_identity: None,
                runner: ScaleSetRunnerObservation::Unknown,
                job_event: None,
                capacity_reserved: admission_observation.capacity_reserved,
                cancellation_requested: admission_observation.cancellation_requested,
            })
            .map_err(|_| DisposableCloneRuntimeError::recovery("clone_reconcile_failed"))?;
            if action
                == (DisposableWorkerAction::Persist {
                    transition: DisposableAttemptCatalogAction::BeginCleanup,
                })
            {
                return Ok(DisposableAttemptCatalogAction::BeginCleanup);
            }
            return Err(DisposableCloneRuntimeError::recovery(
                "clone_cleanup_checkpoint_refused",
            ));
        }

        let checkpoint_at = clock
            .epoch_millis()
            .map_err(|_| observation("clone_clock_unavailable"))?;
        admission_observation.validate_identity_and_freshness_for(
            catalog,
            reservation,
            checkpoint_at,
        )?;
        let transition = if checkpoint_at > reservation.attempt().not_after() {
            DisposableAttemptCatalogAction::BeginCleanup
        } else {
            DisposableAttemptCatalogAction::BeginRegistration
        };
        let action = reconcile_attempt(DisposableWorkerReconcileInput {
            now: checkpoint_at,
            attempt: reservation.attempt(),
            vm: DisposableVmObservation::Ready,
            vm_identity: reservation.attempt().vm_identity(),
            runner: ScaleSetRunnerObservation::Unknown,
            job_event: None,
            capacity_reserved: admission_observation.capacity_reserved,
            cancellation_requested: admission_observation.cancellation_requested,
        })
        .map_err(|_| DisposableCloneRuntimeError::recovery("clone_reconcile_failed"))?;
        if action
            != (DisposableWorkerAction::Persist {
                transition: transition.clone(),
            })
        {
            return Err(DisposableCloneRuntimeError::recovery(
                "clone_registration_refused",
            ));
        }
        if transition == DisposableAttemptCatalogAction::BeginRegistration {
            ready
                .confirm_current()
                .map_err(|_| observation("clone_worker_identity_drift"))?;
        }
        Ok(transition)
    }

    /// Reconfirm the exact running clone and its realized isolation policy before JIT handoff.
    pub(crate) fn confirm_ready_worker(
        &self,
        reservation: &DisposableAttemptReservation,
        executor: &impl TimedCommandExecutor,
        clock: &impl CloneRuntimeClock,
    ) -> Result<ConfirmedDisposableWorker, DisposableCloneRuntimeError> {
        let attempt = reservation.attempt();
        if !matches!(
            attempt.phase(),
            DisposableAttemptPhase::Registering | DisposableAttemptPhase::Assigned
        ) {
            return Err(DisposableCloneRuntimeError::recovery(
                "clone_worker_phase_mismatch",
            ));
        }
        self.confirm_ready_worker_bound(reservation, executor, clock)
    }

    fn confirm_ready_worker_bound(
        &self,
        reservation: &DisposableAttemptReservation,
        executor: &impl TimedCommandExecutor,
        clock: &impl CloneRuntimeClock,
    ) -> Result<ConfirmedDisposableWorker, DisposableCloneRuntimeError> {
        let expected_identity = reservation.attempt().vm_identity().ok_or_else(|| {
            DisposableCloneRuntimeError::recovery("clone_worker_identity_missing")
        })?;
        let prepared_template_identity = current_disposable_prepared_template()
            .and_then(|manifest| manifest.identity())
            .map_err(|_| invalid_configuration("clone_prepared_template_invalid"))?;
        if reservation.prepared_template_identity() != &prepared_template_identity {
            return Err(DisposableCloneRuntimeError::recovery(
                "clone_prepared_template_drift",
            ));
        }
        let request = self
            .worker
            .target_observation_request(reservation)
            .map_err(|_| invalid_configuration("clone_target_request_invalid"))?;
        let host = self
            .template_runtime
            .confirm_running_clone_target(&request, reservation.resources(), executor, clock)
            .map_err(|_| observation("clone_worker_not_ready"))?;
        if &DisposableVmIdentity::from_host_identity(host.identity()) != expected_identity {
            return Err(DisposableCloneRuntimeError::recovery(
                "clone_worker_identity_drift",
            ));
        }
        host.confirm(&request)
            .map_err(|_| observation("clone_worker_identity_drift"))?;
        Ok(ConfirmedDisposableWorker { host, request })
    }

    /// Observe an exact cleanup target without requiring its guest listener to remain healthy.
    ///
    /// Absence is accepted only when Lima's control view and the descriptor-bound host view both
    /// agree. A present target must still match the durable post-clone disk identity.
    pub(crate) fn observe_cleanup_worker(
        &self,
        reservation: &DisposableAttemptReservation,
        executor: &impl TimedCommandExecutor,
        clock: &impl CloneRuntimeClock,
    ) -> Result<DisposableCleanupVmObservation, DisposableCloneRuntimeError> {
        if !matches!(
            reservation.attempt().phase(),
            DisposableAttemptPhase::Destroying
                | DisposableAttemptPhase::Deregistering
                | DisposableAttemptPhase::Releasing
        ) {
            return Err(DisposableCloneRuntimeError::recovery(
                "cleanup_vm_phase_mismatch",
            ));
        }
        let expected_identity = reservation
            .attempt()
            .vm_identity()
            .ok_or_else(|| DisposableCloneRuntimeError::recovery("cleanup_vm_identity_missing"))?;
        let request = self
            .worker
            .target_observation_request(reservation)
            .map_err(|_| invalid_configuration("cleanup_target_request_invalid"))?;
        self.verify_limactl(executor)?;
        let adapter = LimaObservationAdapter::new(self.limactl_program.clone())
            .map_err(|_| invalid_configuration("cleanup_observer_invalid"))?;
        let bounded = BoundedExecutor { executor };
        let lima = adapter.observe(&request, &bounded, clock);
        let host = LimaHostIdentityAdapter.observe_cleanup(&request);
        match (lima, host) {
            (Err(lima_error), Err(host_error))
                if lima_error.code == LimaObservationRefusalCode::MissingInstanceEvidence
                    && host_error.kind == LimaHostIdentityErrorKind::MissingEvidence =>
            {
                Ok(DisposableCleanupVmObservation::Absent)
            }
            (Ok(_), Ok(host)) => {
                if host.root_disk_bytes() != reservation.resources().disk_bytes()
                    || !expected_identity.matches_host_identity(host.identity())
                {
                    return Err(DisposableCloneRuntimeError::recovery(
                        "cleanup_vm_identity_drift",
                    ));
                }
                host.confirm(&request)
                    .map_err(|_| observation("cleanup_vm_identity_drift"))?;
                Ok(DisposableCleanupVmObservation::Present(Box::new(
                    ConfirmedDisposableCleanupVm { host, request },
                )))
            }
            (Err(error), _)
                if error.code != LimaObservationRefusalCode::MissingInstanceEvidence =>
            {
                Err(observation("cleanup_vm_observation_unavailable"))
            }
            _ => Err(DisposableCloneRuntimeError::recovery(
                "cleanup_vm_control_host_conflict",
            )),
        }
    }

    /// Delete one freshly confirmed owned VM and require both views to prove its absence.
    ///
    /// An ambiguous command outcome leaves the durable cleanup phase unchanged. The next tick
    /// observes first and therefore never blindly replays deletion against a mutable name.
    pub(crate) fn destroy_cleanup_worker(
        &self,
        reservation: &DisposableAttemptReservation,
        confirmed: &ConfirmedDisposableCleanupVm,
        executor: &impl TimedCommandExecutor,
        clock: &impl CloneRuntimeClock,
    ) -> Result<(), DisposableCloneRuntimeError> {
        confirmed.confirm_current()?;
        self.verify_limactl(executor)?;
        confirmed.confirm_current()?;
        let now = clock
            .epoch_millis()
            .map_err(|_| observation("cleanup_clock_unavailable"))?;
        let plan = self
            .worker
            .plan(now, reservation, &DisposableWorkerAction::DestroyVm)
            .map_err(|_| DisposableCloneRuntimeError::recovery("cleanup_destroy_plan_refused"))?;
        if plan.kind() != DisposableLimaWorkerCommandKind::Destroy {
            return Err(DisposableCloneRuntimeError::recovery(
                "cleanup_destroy_plan_mismatch",
            ));
        }
        // This is the final name-to-inode barrier after every potentially injected planning step.
        confirmed.confirm_current()?;
        let record = executor
            .execute_with_timeout(plan.command(), plan.timeout())
            .map_err(|_| command("cleanup_destroy_command_failed"))?;
        validate_record(plan.command(), &record)
            .map_err(|_| command("cleanup_destroy_record_mismatch"))?;
        match self.observe_cleanup_worker(reservation, executor, clock)? {
            DisposableCleanupVmObservation::Absent => Ok(()),
            DisposableCleanupVmObservation::Present(_) => Err(
                DisposableCloneRuntimeError::recovery("cleanup_destroy_not_observed"),
            ),
        }
    }

    fn verify_limactl(
        &self,
        executor: &impl TimedCommandExecutor,
    ) -> Result<(), DisposableCloneRuntimeError> {
        let (command, expected) = self.worker.version_command();
        let record = executor
            .execute_with_timeout(&command, OBSERVATION_TIMEOUT)
            .map_err(|_| observation("clone_lima_version_unavailable"))?;
        if record.argv != command.displayed_argv()
            || record.environment_keys != command.environment.keys().cloned().collect::<Vec<_>>()
            || record.status != Some(0)
            || !record.success
            || record.stdout != expected
            || !record.stderr.is_empty()
        {
            return Err(observation("clone_lima_version_mismatch"));
        }
        Ok(())
    }

    fn confirm_target_absent(
        &self,
        request: &LimaObservationRequest,
        executor: &impl TimedCommandExecutor,
        clock: &impl LimaObservationClock,
    ) -> Result<(), DisposableCloneRuntimeError> {
        let adapter = LimaObservationAdapter::new(self.limactl_program.clone())
            .map_err(|_| invalid_configuration("clone_observer_invalid"))?;
        let bounded = BoundedExecutor { executor };
        match adapter.observe(request, &bounded, clock) {
            Err(error) if error.code == LimaObservationRefusalCode::MissingInstanceEvidence => {
                Ok(())
            }
            Ok(_) => Err(DisposableCloneRuntimeError::recovery(
                "clone_target_no_longer_absent",
            )),
            Err(_) => Err(observation("clone_target_absence_unavailable")),
        }
    }
}

pub(crate) struct PreparedClone {
    pub(crate) plan: DisposableLimaWorkerCommandPlan,
    target_request: LimaObservationRequest,
    source: ConfirmedDisposableCloneSource,
    generation: DisposableTemplateGenerationDocument,
}

/// Single-use proof that every fallible pre-command gate passed for one exact proposed checkpoint.
pub(crate) struct CheckpointReadyClone {
    prepared: PreparedClone,
    expected_disk_bytes: u64,
}

/// Descriptor-retaining proof of the exact disposable target observed ready for JIT handoff.
pub(crate) struct ConfirmedDisposableWorker {
    host: crate::lima_host_identity::LimaHostIdentityObservation,
    request: LimaObservationRequest,
}

pub(crate) enum DisposableCleanupVmObservation {
    Absent,
    Present(Box<ConfirmedDisposableCleanupVm>),
}

pub(crate) struct ConfirmedDisposableCleanupVm {
    host: crate::lima_host_identity::LimaHostIdentityObservation,
    request: LimaObservationRequest,
}

impl ConfirmedDisposableCleanupVm {
    fn confirm_current(&self) -> Result<(), DisposableCloneRuntimeError> {
        self.host
            .confirm(&self.request)
            .map_err(|_| observation("cleanup_vm_identity_drift"))
    }
}

impl ConfirmedDisposableWorker {
    pub(crate) fn confirm_current(&self) -> Result<(), DisposableCloneRuntimeError> {
        self.host
            .confirm(&self.request)
            .map_err(|_| observation("clone_worker_identity_drift"))
    }
}

impl crate::disposable_runner_runtime::DisposableRunnerTargetRuntime for DisposableCloneRuntime {
    type Confirmation = ConfirmedDisposableWorker;

    fn confirm_runner_target(
        &self,
        reservation: &DisposableAttemptReservation,
        executor: &impl TimedCommandExecutor,
        clock: &impl CloneRuntimeClock,
    ) -> Result<Self::Confirmation, crate::disposable_runner_runtime::DisposableRunnerRuntimeError>
    {
        self.confirm_ready_worker(reservation, executor, clock)
            .map_err(|_| {
                crate::disposable_runner_runtime::DisposableRunnerRuntimeError::observation(
                    "runner_target_not_ready",
                )
            })
    }

    fn reconfirm_runner_target(
        &self,
        confirmation: &Self::Confirmation,
        _reservation: &DisposableAttemptReservation,
        _executor: &impl TimedCommandExecutor,
        _clock: &impl CloneRuntimeClock,
    ) -> Result<(), crate::disposable_runner_runtime::DisposableRunnerRuntimeError> {
        confirmation.confirm_current().map_err(|_| {
            crate::disposable_runner_runtime::DisposableRunnerRuntimeError::observation(
                "runner_target_identity_drift",
            )
        })
    }
}

pub(crate) trait CloneRuntimeClock: LimaObservationClock {
    fn epoch_millis(&self) -> io::Result<EpochMillis>;
}

#[derive(Debug, Clone, Copy)]
struct SystemCloneRuntimeClock;

impl LimaObservationClock for SystemCloneRuntimeClock {
    fn unix_seconds(&self) -> io::Result<u64> {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .map_err(|_| io::Error::other("system clock precedes the Unix epoch"))
    }
}

impl CloneRuntimeClock for SystemCloneRuntimeClock {
    fn epoch_millis(&self) -> io::Result<EpochMillis> {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| io::Error::other("system clock precedes the Unix epoch"))?
            .as_millis();
        let millis = u64::try_from(millis)
            .map_err(|_| io::Error::other("system clock is outside the supported range"))?;
        EpochMillis::new(millis)
            .map_err(|_| io::Error::other("system clock is outside the supported range"))
    }
}

struct BoundedExecutor<'a, E> {
    executor: &'a E,
}

impl<E: TimedCommandExecutor> CommandExecutor for BoundedExecutor<'_, E> {
    fn execute(&self, spec: &CommandSpec) -> io::Result<ExecutionRecord> {
        self.executor
            .execute_with_timeout(spec, OBSERVATION_TIMEOUT)
    }
}

fn validate_record(
    command_spec: &CommandSpec,
    record: &ExecutionRecord,
) -> Result<(), DisposableCloneRuntimeError> {
    if record.argv != command_spec.displayed_argv()
        || record.environment_keys != command_spec.environment.keys().cloned().collect::<Vec<_>>()
        || record.status != Some(0)
        || !record.success
    {
        return Err(command("clone_command_record_mismatch"));
    }
    Ok(())
}

const fn runtime_error(
    kind: DisposableCloneRuntimeErrorKind,
    code: &'static str,
    message: &'static str,
) -> DisposableCloneRuntimeError {
    DisposableCloneRuntimeError {
        kind,
        code,
        message,
    }
}

const fn invalid_configuration(code: &'static str) -> DisposableCloneRuntimeError {
    runtime_error(
        DisposableCloneRuntimeErrorKind::InvalidConfiguration,
        code,
        "the fixed disposable-clone runtime configuration is invalid",
    )
}

const fn observation(code: &'static str) -> DisposableCloneRuntimeError {
    runtime_error(
        DisposableCloneRuntimeErrorKind::Observation,
        code,
        "the disposable-clone host evidence is unavailable or changed",
    )
}

const fn command(code: &'static str) -> DisposableCloneRuntimeError {
    runtime_error(
        DisposableCloneRuntimeErrorKind::Command,
        code,
        "the fixed disposable-clone command failed or returned inconsistent evidence",
    )
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::{Path, PathBuf};
    use std::rc::Rc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::disposable_attempt_catalog::{
        DisposableAttemptCatalog, DisposableAttemptCatalogAction, DisposableAttemptReservation,
    };
    use crate::disposable_attempt_state::DisposableAttemptState;
    use crate::disposable_host_storage::{DisposableHostStorageError, DisposableHostStorageSource};
    use crate::disposable_prepared_template::current_disposable_prepared_template;
    use crate::disposable_runner_runtime::{
        DisposableRunnerRegistrationSource, DisposableRunnerRuntime, DisposableRunnerRuntimeError,
    };
    use crate::disposable_template_generation::{
        DisposableTemplateGenerationAction, DisposableTemplateObjectIdentity,
        encode_disposable_template_generation,
    };
    use crate::disposable_worker_coordinator::{
        DisposableWorkerCoordinator, DisposableWorkerCoordinatorDisposition,
        DisposableWorkerServiceBridge,
    };
    use crate::disposable_worker_reconciler::{
        CapacityClaimId, DisposableVmId, DisposableWorkerResources,
    };
    use crate::github_scale_set_bridge::{
        EncodedJitConfig, ScaleSetBridgeError, ScaleSetBridgeEvent, ScaleSetBridgeJobEvidence,
        ScaleSetBridgePoll, ScaleSetJitReceipt, ScaleSetRunnerLookup, ScaleSetStatistics,
    };
    use crate::github_scale_set_delivery::ScaleSetDelivery;
    use crate::github_scale_set_delivery_consumer::ScaleSetDeliveryConsumerPolicy;
    use crate::github_scale_set_delivery_controller::DeliveryBridge;
    use crate::github_scale_set_protocol::{
        ScaleSetJobId, ScaleSetJobResult, ScaleSetRunnerId, ScaleSetRunnerName,
        ScaleSetRunnerReference, ScaleSetRunnerRequestId,
    };
    use crate::lima_observation::{LimaArchitecture, LimaVmType};
    use crate::unix_personal_worker_store::STORE_DIRECTORY;
    use crate::unix_personal_worker_store::disposable_runner_transaction::DisposableRunnerTransactionOutcome;

    #[allow(dead_code)]
    mod lima_host_identity_support {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/lima_host_identity_support/mod.rs"
        ));
    }
    use lima_host_identity_support::LimaHostIdentityFixture;

    const SOURCE: &str = "smolrunner-source";
    const TARGET: &str = "smol-clone-1";
    const GIB: u64 = 1 << 30;
    const SOURCE_DISK: u64 = 20 * GIB;
    const TARGET_DISK: u64 = 80 * GIB;
    static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new(label: &str) -> Self {
            let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "smolrunner-clone-runtime-{label}-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o750)).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    struct FixedClock;

    struct AfterDeadlineClock;

    impl LimaObservationClock for FixedClock {
        fn unix_seconds(&self) -> io::Result<u64> {
            Ok(1_900_000_000)
        }
    }

    impl CloneRuntimeClock for FixedClock {
        fn epoch_millis(&self) -> io::Result<EpochMillis> {
            EpochMillis::new(1_900_000_000_000).map_err(io::Error::other)
        }
    }

    impl LimaObservationClock for AfterDeadlineClock {
        fn unix_seconds(&self) -> io::Result<u64> {
            Ok(1_900_000_601)
        }
    }

    impl CloneRuntimeClock for AfterDeadlineClock {
        fn epoch_millis(&self) -> io::Result<EpochMillis> {
            EpochMillis::new(1_900_000_600_001).map_err(io::Error::other)
        }
    }

    struct AdmissionOrderedClock {
        observed: Rc<Cell<bool>>,
    }

    impl LimaObservationClock for AdmissionOrderedClock {
        fn unix_seconds(&self) -> io::Result<u64> {
            Ok(1_900_000_000)
        }
    }

    impl CloneRuntimeClock for AdmissionOrderedClock {
        fn epoch_millis(&self) -> io::Result<EpochMillis> {
            let value = if self.observed.get() {
                1_900_000_000_001
            } else {
                1_900_000_000_000
            };
            EpochMillis::new(value).map_err(io::Error::other)
        }
    }

    struct SequencedClock {
        calls: Cell<u8>,
        final_millis: u64,
    }

    struct CleanupRebindClock<'a> {
        host: &'a LimaHostIdentityFixture,
        calls: Cell<u8>,
    }

    impl LimaObservationClock for CleanupRebindClock<'_> {
        fn unix_seconds(&self) -> io::Result<u64> {
            Ok(1_900_000_000)
        }
    }

    impl CloneRuntimeClock for CleanupRebindClock<'_> {
        fn epoch_millis(&self) -> io::Result<EpochMillis> {
            let call = self.calls.get();
            self.calls.set(call.saturating_add(1));
            if call == 1 {
                fs::remove_dir_all(self.host.lima_home().join(TARGET))?;
                self.host.add_instance(TARGET, TARGET_DISK);
            }
            EpochMillis::new(1_900_000_000_000).map_err(io::Error::other)
        }
    }

    impl LimaObservationClock for SequencedClock {
        fn unix_seconds(&self) -> io::Result<u64> {
            Ok(1_900_000_000)
        }
    }

    impl CloneRuntimeClock for SequencedClock {
        fn epoch_millis(&self) -> io::Result<EpochMillis> {
            let call = self.calls.get();
            self.calls.set(call.saturating_add(1));
            let value = if call < 2 {
                1_900_000_000_000
            } else {
                self.final_millis
            };
            EpochMillis::new(value).map_err(io::Error::other)
        }
    }

    struct FakeExecutor<'a> {
        host: &'a LimaHostIdentityFixture,
        calls: RefCell<Vec<Vec<String>>>,
        fail_clone: bool,
        fail_runner: bool,
        fail_delete_after_remove: bool,
        rewrite_target_after_runner: bool,
        target_ready: bool,
        rewrite_target_during_final_source: bool,
        target_rewritten: Cell<bool>,
        version_calls: Cell<u8>,
        drift_version_on: Option<u8>,
    }

    impl FakeExecutor<'_> {
        fn record(spec: &CommandSpec, stdout: String) -> ExecutionRecord {
            ExecutionRecord {
                argv: spec.displayed_argv(),
                environment_keys: spec.environment.keys().cloned().collect(),
                status: Some(0),
                success: true,
                stdout,
                stderr: String::new(),
            }
        }

        fn source_config() -> serde_json::Value {
            serde_json::json!({
                "vmType": "vz",
                "arch": "aarch64",
                "plain": true,
                "networks": [],
                "portForwards": [],
                "propagateProxyEnv": false,
                "hostResolver": { "enabled": false },
                "containerd": { "system": false, "user": false }
            })
        }

        fn source_json(&self) -> String {
            serde_json::json!({
                "name": SOURCE,
                "status": "Stopped",
                "dir": self.host.lima_home().join(SOURCE),
                "vmType": "vz",
                "arch": "aarch64",
                "cpus": 2,
                "memory": 2 * GIB,
                "disk": SOURCE_DISK,
                "errors": [],
                "config": Self::source_config()
            })
            .to_string()
                + "\n"
        }

        fn target_config() -> serde_json::Value {
            let mut config = Self::source_config();
            let object = config.as_object_mut().unwrap();
            object.insert("cpus".to_owned(), serde_json::json!(4));
            object.insert("memory".to_owned(), serde_json::json!("8GiB"));
            object.insert("disk".to_owned(), serde_json::json!("80GiB"));
            config
        }

        fn target_json(&self) -> String {
            serde_json::json!({
                "name": TARGET,
                "status": "Running",
                "dir": self.host.lima_home().join(TARGET),
                "vmType": "vz",
                "arch": "aarch64",
                "cpus": 4,
                "memory": 8 * GIB,
                "disk": TARGET_DISK,
                "errors": [],
                "config": Self::target_config()
            })
            .to_string()
                + "\n"
        }

        fn clone_count(&self) -> usize {
            self.calls
                .borrow()
                .iter()
                .filter(|argv| argv.iter().any(|value| value == "clone"))
                .count()
        }

        fn runner_launch_count(&self) -> usize {
            self.calls
                .borrow()
                .iter()
                .filter(|argv| {
                    argv.iter()
                        .any(|value| value == "/opt/smolrunner/bin/smolrunner-jit-launcher")
                        && !argv.iter().any(|value| value == "/usr/bin/sha256sum")
                })
                .count()
        }
    }

    impl CommandExecutor for FakeExecutor<'_> {
        fn execute(&self, spec: &CommandSpec) -> io::Result<ExecutionRecord> {
            self.execute_with_timeout(spec, OBSERVATION_TIMEOUT)
        }
    }

    impl TimedCommandExecutor for FakeExecutor<'_> {
        fn execute_with_timeout(
            &self,
            spec: &CommandSpec,
            _timeout: Duration,
        ) -> io::Result<ExecutionRecord> {
            let argv = spec.displayed_argv();
            self.calls.borrow_mut().push(argv.clone());
            if argv.iter().any(|value| value == "clone") {
                if self.fail_clone {
                    return Err(io::Error::other("injected clone failure"));
                }
                self.host.add_instance(TARGET, TARGET_DISK);
                return Ok(Self::record(spec, "clone complete\n".to_owned()));
            }
            if argv.iter().any(|value| value == "delete")
                && argv.iter().any(|value| value == TARGET)
            {
                fs::remove_dir_all(self.host.lima_home().join(TARGET))?;
                if self.fail_delete_after_remove {
                    return Err(io::Error::other("injected ambiguous delete failure"));
                }
                return Ok(Self::record(spec, "deleted\n".to_owned()));
            }
            if argv.last().is_some_and(|value| value == "--version") {
                let call = self.version_calls.get().saturating_add(1);
                self.version_calls.set(call);
                let stdout = if self.drift_version_on == Some(call) {
                    "limactl version 2.3.0\n"
                } else {
                    "limactl version 2.2.0\n"
                };
                return Ok(Self::record(spec, stdout.to_owned()));
            }
            if argv.iter().any(|value| value == "validate") {
                return Ok(Self::record(
                    spec,
                    serde_json::to_string(&Self::source_config()).unwrap() + "\n",
                ));
            }
            if argv.iter().any(|value| value == "list") {
                let target = argv.last().map(String::as_str).unwrap_or_default();
                let stdout = if target == SOURCE {
                    if self.rewrite_target_during_final_source
                        && self.clone_count() > 0
                        && !self.target_rewritten.replace(true)
                    {
                        self.host.rewrite_disk_identity(TARGET, 0x88);
                    }
                    self.source_json()
                } else if target == TARGET && !self.host.lima_home().join(TARGET).exists() {
                    "\n".to_owned()
                } else if target == TARGET && self.target_ready {
                    self.target_json()
                } else {
                    return Err(io::Error::other("unexpected present target observation"));
                };
                return Ok(Self::record(spec, stdout));
            }
            if self.target_ready && argv.iter().any(|value| value == TARGET) {
                let runner_launch = argv
                    .iter()
                    .any(|value| value == "/opt/smolrunner/bin/smolrunner-jit-launcher")
                    && !argv.iter().any(|value| value == "/usr/bin/sha256sum");
                if runner_launch && self.fail_runner {
                    return Err(io::Error::other("injected runner failure"));
                }
                if runner_launch && self.rewrite_target_after_runner {
                    self.host.rewrite_disk_identity(TARGET, 0x99);
                }
                let stdout = if argv.iter().any(|value| value == "/usr/bin/uname") {
                    "aarch64\n".to_owned()
                } else if argv.iter().any(|value| value == "_NPROCESSORS_ONLN") {
                    "4\n".to_owned()
                } else if argv.iter().any(|value| value == "PAGE_SIZE") {
                    "4096\n".to_owned()
                } else if argv.iter().any(|value| value == "_PHYS_PAGES") {
                    "2097152\n".to_owned()
                } else if argv.iter().any(|value| value == "/etc/machine-id") {
                    format!("{}  /etc/machine-id\n", "aa".repeat(32))
                } else if argv.last().is_some_and(|value| value == "/") {
                    "1:2\n".to_owned()
                } else if argv.last().is_some_and(|value| value == "[REDACTED]") {
                    "3:4\n".to_owned()
                } else if argv
                    .last()
                    .is_some_and(|value| value == "/etc/smolrunner/prepared-template.json")
                {
                    "091e1324f9371a31f0f01ffd2696ff3cc23c459654d2701b0b0149643ed7957f  /etc/smolrunner/prepared-template.json\n".to_owned()
                } else if argv
                    .last()
                    .is_some_and(|value| value == "/opt/smolrunner/bin/smolrunner-runner-integrity")
                    && argv.iter().any(|value| value == "/usr/bin/sha256sum")
                {
                    "d8ee50c0981fb6eec4fdc957491e492f3a5befc7153b8916466627c9812a379f  /opt/smolrunner/bin/smolrunner-runner-integrity\n".to_owned()
                } else if argv
                    .last()
                    .is_some_and(|value| value == "/opt/smolrunner/bin/smolrunner-runner-integrity")
                {
                    "smolrunner-runner-integrity-ok\n".to_owned()
                } else if argv
                    .last()
                    .is_some_and(|value| value == "/opt/smolrunner/bin/smolrunner-jit-launcher")
                    && argv.iter().any(|value| value == "/usr/bin/sha256sum")
                {
                    "6f096fb518b6d40d45ca1a9923e423da436b6269fceec5a5989566abbc93a76a  /opt/smolrunner/bin/smolrunner-jit-launcher\n".to_owned()
                } else if argv.last().is_some_and(|value| {
                    value == "/etc/apt/apt.conf.d/99-smolrunner-no-automatic-updates"
                }) {
                    "b10384a904cdd14d18af31a7754a19ca0c67c237f3ca7bd239f4cf64102ffedb  /etc/apt/apt.conf.d/99-smolrunner-no-automatic-updates\n".to_owned()
                } else if argv.iter().any(|value| value == "/usr/bin/id") {
                    "smolrunner-runner\n".to_owned()
                } else if argv.iter().any(|value| value == "/usr/bin/systemctl") {
                    return Ok(ExecutionRecord {
                        argv: spec.displayed_argv(),
                        environment_keys: spec.environment.keys().cloned().collect(),
                        status: Some(1),
                        success: false,
                        stdout: "masked\n".to_owned(),
                        stderr: String::new(),
                    });
                } else if runner_launch {
                    String::new()
                } else {
                    return Err(io::Error::other("unexpected target guest command"));
                };
                return Ok(Self::record(spec, stdout));
            }
            Err(io::Error::other("unexpected fake command"))
        }
    }

    fn runtime(root: &TempRoot, host: &LimaHostIdentityFixture) -> DisposableCloneRuntime {
        DisposableCloneRuntime::new(
            root.path(),
            "/opt/homebrew/bin/limactl",
            host.lima_home(),
            LimaInstanceName::parse(SOURCE).unwrap(),
        )
        .unwrap()
    }

    fn executor(host: &LimaHostIdentityFixture, fail_clone: bool) -> FakeExecutor<'_> {
        FakeExecutor {
            host,
            calls: RefCell::new(Vec::new()),
            fail_clone,
            fail_runner: false,
            fail_delete_after_remove: false,
            rewrite_target_after_runner: false,
            target_ready: false,
            rewrite_target_during_final_source: false,
            target_rewritten: Cell::new(false),
            version_calls: Cell::new(0),
            drift_version_on: None,
        }
    }

    struct FakeRegistration {
        observed: ScaleSetRunnerLookup,
        observe_calls: u8,
        jit_calls: u8,
        fail_jit: bool,
    }

    impl DisposableRunnerRegistrationSource for FakeRegistration {
        fn observe_runner(
            &mut self,
            _runner_name: &ScaleSetRunnerName,
        ) -> Result<ScaleSetRunnerLookup, DisposableRunnerRuntimeError> {
            self.observe_calls = self.observe_calls.saturating_add(1);
            Ok(self.observed.clone())
        }

        fn generate_jit(
            &mut self,
            runner_name: &ScaleSetRunnerName,
        ) -> Result<ScaleSetJitReceipt, DisposableRunnerRuntimeError> {
            self.jit_calls = self.jit_calls.saturating_add(1);
            if self.fail_jit {
                return Err(DisposableRunnerRuntimeError::bridge(
                    "runner_jit_generation_failed",
                ));
            }
            Ok(ScaleSetJitReceipt {
                runner: ScaleSetRunnerReference::new(
                    ScaleSetRunnerId::new(77).unwrap(),
                    runner_name.clone(),
                ),
                config: EncodedJitConfig::for_test("eyJ0b2tlbiI6InNlY3JldCJ9"),
            })
        }
    }

    struct FakeCleanup {
        runner: Option<ScaleSetRunnerReference>,
        remove_calls: u8,
        fail_remove_after_remove: bool,
    }

    impl DisposableCleanupRunnerSource for FakeCleanup {
        fn observe_runner(
            &mut self,
            _runner_name: &ScaleSetRunnerName,
        ) -> Result<ScaleSetRunnerLookup, DisposableCloneRuntimeError> {
            Ok(match &self.runner {
                Some(runner) => ScaleSetRunnerLookup::Present(runner.clone()),
                None => ScaleSetRunnerLookup::Absent,
            })
        }

        fn remove_runner(
            &mut self,
            runner: &ScaleSetRunnerReference,
        ) -> Result<(), DisposableCloneRuntimeError> {
            if self.runner.as_ref() != Some(runner) {
                return Err(DisposableCloneRuntimeError::recovery(
                    "injected_runner_identity_mismatch",
                ));
            }
            self.remove_calls = self.remove_calls.saturating_add(1);
            self.runner = None;
            if self.fail_remove_after_remove {
                return Err(DisposableCloneRuntimeError::recovery(
                    "injected_runner_delete_failed",
                ));
            }
            Ok(())
        }
    }

    struct FakeCoordinatorBridge {
        runner: Option<ScaleSetRunnerReference>,
        capacities: Vec<u16>,
        polls: std::collections::VecDeque<ScaleSetBridgePoll>,
        acknowledged: Vec<u64>,
    }

    struct MutableHostStorage(Rc<Cell<bool>>);

    impl DisposableHostStorageSource for MutableHostStorage {
        fn admits_new_worker(&self) -> Result<bool, DisposableHostStorageError> {
            Ok(self.0.get())
        }
    }

    impl DeliveryBridge for FakeCoordinatorBridge {
        fn resume_after(&mut self, _: u32) -> Result<(), ScaleSetBridgeError> {
            Ok(())
        }

        fn poll(
            &mut self,
            available_capacity: u16,
        ) -> Result<ScaleSetBridgePoll, ScaleSetBridgeError> {
            self.capacities.push(available_capacity);
            if let Some(poll) = self.polls.pop_front() {
                return Ok(poll);
            }
            Ok(ScaleSetBridgePoll::Idle {
                statistics: ScaleSetStatistics {
                    available_jobs: 0,
                    acquired_jobs: 1,
                    assigned_jobs: 0,
                    running_jobs: 0,
                    registered_runners: u32::from(self.runner.is_some()),
                    busy_runners: 0,
                    idle_runners: u32::from(self.runner.is_some()),
                },
            })
        }

        fn ack(&mut self, _: u32) -> Result<Vec<u64>, ScaleSetBridgeError> {
            Ok(self.acknowledged.clone())
        }

        fn acquire(
            &mut self,
            _: &[ScaleSetRunnerRequestId],
        ) -> Result<Vec<ScaleSetRunnerRequestId>, ScaleSetBridgeError> {
            Ok(Vec::new())
        }

        fn poison(&mut self) {}
    }

    impl DisposableWorkerServiceBridge for FakeCoordinatorBridge {
        fn observe_runner(
            &mut self,
            _: &ScaleSetRunnerName,
        ) -> Result<ScaleSetRunnerLookup, ScaleSetBridgeError> {
            Ok(match &self.runner {
                Some(runner) => ScaleSetRunnerLookup::Present(runner.clone()),
                None => ScaleSetRunnerLookup::Absent,
            })
        }

        fn generate_jit(
            &mut self,
            runner_name: &ScaleSetRunnerName,
        ) -> Result<ScaleSetJitReceipt, ScaleSetBridgeError> {
            let runner = ScaleSetRunnerReference::new(
                ScaleSetRunnerId::new(77).unwrap(),
                runner_name.clone(),
            );
            self.runner = Some(runner.clone());
            Ok(ScaleSetJitReceipt {
                runner,
                config: EncodedJitConfig::for_test("eyJ0b2tlbiI6InNlY3JldCJ9"),
            })
        }

        fn remove_runner(
            &mut self,
            runner: &ScaleSetRunnerReference,
        ) -> Result<(), ScaleSetBridgeError> {
            if self.runner.as_ref() != Some(runner) {
                return Err(ScaleSetBridgeError::new("runner_identity_mismatch"));
            }
            self.runner = None;
            Ok(())
        }
    }

    struct FakeAdmission {
        calls: Cell<u8>,
        lose_capacity_on: Option<u8>,
        cancel_on: Option<u8>,
        error_on: Option<u8>,
    }

    struct TimedAdmission {
        observed_at: u64,
    }

    impl admission_seal::Sealed for TimedAdmission {}

    impl DisposableCloneAdmissionSource for TimedAdmission {
        fn observe(
            &self,
            catalog: &DisposableAttemptCatalogDocument,
            reservation: &DisposableAttemptReservation,
        ) -> Result<DisposableCloneAdmissionObservation, DisposableCloneRuntimeError> {
            Ok(DisposableCloneAdmissionObservation::new(
                catalog,
                reservation,
                EpochMillis::new(self.observed_at).unwrap(),
                EpochMillis::new(self.observed_at + 30_000).unwrap(),
                true,
                false,
            ))
        }
    }

    impl FakeAdmission {
        fn available() -> Self {
            Self {
                calls: Cell::new(0),
                lose_capacity_on: None,
                cancel_on: None,
                error_on: None,
            }
        }
    }

    impl admission_seal::Sealed for FakeAdmission {}

    impl DisposableCloneAdmissionSource for FakeAdmission {
        fn observe(
            &self,
            catalog: &DisposableAttemptCatalogDocument,
            reservation: &DisposableAttemptReservation,
        ) -> Result<DisposableCloneAdmissionObservation, DisposableCloneRuntimeError> {
            let call = self.calls.get().saturating_add(1);
            self.calls.set(call);
            if self.error_on == Some(call) {
                return Err(DisposableCloneRuntimeError::observation(
                    "clone_test_admission_failed",
                ));
            }
            Ok(DisposableCloneAdmissionObservation::new(
                catalog,
                reservation,
                EpochMillis::new(1_900_000_000_000).unwrap(),
                EpochMillis::new(1_900_000_030_000).unwrap(),
                self.lose_capacity_on != Some(call),
                self.cancel_on == Some(call),
            ))
        }
    }

    struct OrderedAdmission {
        observed: Rc<Cell<bool>>,
    }

    impl admission_seal::Sealed for OrderedAdmission {}

    impl DisposableCloneAdmissionSource for OrderedAdmission {
        fn observe(
            &self,
            catalog: &DisposableAttemptCatalogDocument,
            reservation: &DisposableAttemptReservation,
        ) -> Result<DisposableCloneAdmissionObservation, DisposableCloneRuntimeError> {
            self.observed.set(true);
            Ok(DisposableCloneAdmissionObservation::new(
                catalog,
                reservation,
                EpochMillis::new(1_900_000_000_001).unwrap(),
                EpochMillis::new(1_900_000_030_001).unwrap(),
                true,
                false,
            ))
        }
    }

    fn install_ready_generation(
        root: &TempRoot,
        host: &LimaHostIdentityFixture,
        runtime: &DisposableCloneRuntime,
    ) {
        let source_request = LimaObservationRequest::new(
            LimaInstanceName::parse(SOURCE).unwrap(),
            host.lima_home(),
            LimaVmType::Vz,
            LimaArchitecture::Aarch64,
            "/opt/smolrunner/actions-runner/_work",
            30,
        )
        .unwrap();
        let source_host = LimaHostIdentityAdapter.observe(&source_request).unwrap();
        let object = DisposableTemplateObjectIdentity::from_host_digest(
            source_host.identity().digest().clone(),
        );
        let initial = runtime.template_runtime.initial_document();
        let ready = initial
            .transition(1, DisposableTemplateGenerationAction::AuthorizeCreate, None)
            .unwrap()
            .transition(
                2,
                DisposableTemplateGenerationAction::RecordCreateStarted,
                None,
            )
            .unwrap()
            .transition(
                3,
                DisposableTemplateGenerationAction::RecordVerified,
                Some(object),
            )
            .unwrap()
            .transition(4, DisposableTemplateGenerationAction::AuthorizeStop, None)
            .unwrap()
            .transition(
                5,
                DisposableTemplateGenerationAction::RecordStopStarted,
                None,
            )
            .unwrap()
            .transition(6, DisposableTemplateGenerationAction::RecordReady, None)
            .unwrap();
        let mut store =
            UnixPersonalWorkerStore::open_or_create_disposable_template_generation(root.path())
                .unwrap();
        store
            .create_disposable_template_generation(&initial)
            .unwrap();
        drop(store);
        fs::write(
            root.path()
                .join(STORE_DIRECTORY)
                .join("disposable-template-generation.json"),
            encode_disposable_template_generation(&ready).unwrap(),
        )
        .unwrap();
    }

    fn install_reserved_attempt(root: &TempRoot) -> DisposableAttemptId {
        install_reserved_attempt_until(root, 1_900_000_600_000)
    }

    fn install_reserved_attempt_until(root: &TempRoot, not_after: u64) -> DisposableAttemptId {
        let store =
            UnixPersonalWorkerStore::open_or_create_disposable_catalog(root.path()).unwrap();
        let mut catalog = DisposableAttemptCatalog::new(store);
        let (empty, _) = catalog.initialize().unwrap();
        let attempt_id = DisposableAttemptId::parse("attempt-clone-1").unwrap();
        let attempt = DisposableAttemptState::reserved(
            attempt_id.clone(),
            CapacityClaimId::parse("claim-clone-1").unwrap(),
            DisposableVmId::parse(TARGET).unwrap(),
            ScaleSetRunnerName::parse("smol-clone-1").unwrap(),
            ScaleSetRunnerRequestId::new(41).unwrap(),
            EpochMillis::new(not_after).unwrap(),
        );
        let reservation = DisposableAttemptReservation::new(
            attempt,
            DisposableWorkerResources::new(4_000, 8 * GIB, TARGET_DISK).unwrap(),
            current_disposable_prepared_template()
                .unwrap()
                .identity()
                .unwrap(),
        )
        .unwrap();
        catalog.reserve(empty.revision(), reservation).unwrap();
        attempt_id
    }

    fn install_authorized_attempt(root: &TempRoot) -> DisposableAttemptId {
        let attempt_id = install_reserved_attempt(root);
        let store =
            UnixPersonalWorkerStore::open_or_create_disposable_catalog(root.path()).unwrap();
        let mut catalog = DisposableAttemptCatalog::new(store);
        let reserved = catalog.load().unwrap();
        catalog
            .transition(
                reserved.revision(),
                &attempt_id,
                reserved
                    .find_active(&attempt_id)
                    .unwrap()
                    .attempt()
                    .revision(),
                DisposableAttemptCatalogAction::AuthorizeClone,
            )
            .unwrap();
        attempt_id
    }

    fn durable_attempt(
        root: &TempRoot,
        attempt_id: &DisposableAttemptId,
    ) -> crate::disposable_attempt_state::DisposableAttemptState {
        let store =
            UnixPersonalWorkerStore::open_or_create_disposable_catalog(root.path()).unwrap();
        DisposableAttemptCatalog::new(store)
            .load()
            .unwrap()
            .find_active(attempt_id)
            .unwrap()
            .attempt()
            .clone()
    }

    fn install_running_registering_attempt(
        root: &TempRoot,
        host: &LimaHostIdentityFixture,
        runtime: &DisposableCloneRuntime,
        executor: &mut FakeExecutor<'_>,
    ) -> DisposableAttemptId {
        install_ready_generation(root, host, runtime);
        let attempt_id = install_authorized_attempt(root);
        runtime
            .clone_once_with(
                &attempt_id,
                &FakeAdmission::available(),
                executor,
                &FixedClock,
            )
            .unwrap();
        executor.target_ready = true;
        let store =
            UnixPersonalWorkerStore::open_or_create_disposable_catalog(root.path()).unwrap();
        let mut catalog = DisposableAttemptCatalog::new(store);
        let cloned = catalog.load().unwrap();
        catalog
            .transition(
                cloned.revision(),
                &attempt_id,
                cloned
                    .find_active(&attempt_id)
                    .unwrap()
                    .attempt()
                    .revision(),
                DisposableAttemptCatalogAction::BeginRegistration,
            )
            .unwrap();
        attempt_id
    }

    fn runner_runtime(host: &LimaHostIdentityFixture) -> DisposableRunnerRuntime {
        DisposableRunnerRuntime::new("/opt/homebrew/bin/limactl", host.lima_home()).unwrap()
    }

    fn install_terminal_attempt(
        root: &TempRoot,
        host: &LimaHostIdentityFixture,
        clone_runtime: &DisposableCloneRuntime,
        executor: &mut FakeExecutor<'_>,
    ) -> (
        UnixPersonalWorkerStore,
        DisposableAttemptId,
        ScaleSetRunnerReference,
    ) {
        let attempt_id = install_running_registering_attempt(root, host, clone_runtime, executor);
        let mut store =
            UnixPersonalWorkerStore::open_or_create_disposable_catalog(root.path()).unwrap();
        let runner = ScaleSetRunnerReference::new(
            ScaleSetRunnerId::new(77).unwrap(),
            ScaleSetRunnerName::parse(TARGET).unwrap(),
        );
        let mut registration = FakeRegistration {
            observed: ScaleSetRunnerLookup::Absent,
            observe_calls: 0,
            jit_calls: 0,
            fail_jit: false,
        };
        store
            .execute_disposable_runner_transaction(
                &runner_runtime(host),
                clone_runtime,
                &attempt_id,
                &mut registration,
                executor,
                &FixedClock,
            )
            .unwrap();

        let mut catalog = DisposableAttemptCatalog::new(store);
        let registering = catalog.load().unwrap();
        let assigned = catalog
            .transition(
                registering.revision(),
                &attempt_id,
                registering
                    .find_active(&attempt_id)
                    .unwrap()
                    .attempt()
                    .revision(),
                DisposableAttemptCatalogAction::RecordAssigned(
                    ScaleSetJobId::parse("job-terminal-cleanup").unwrap(),
                ),
            )
            .unwrap()
            .0;
        let terminal = catalog
            .transition(
                assigned.revision(),
                &attempt_id,
                assigned
                    .find_active(&attempt_id)
                    .unwrap()
                    .attempt()
                    .revision(),
                DisposableAttemptCatalogAction::RecordTerminal {
                    runner: Some(runner.clone()),
                    job_id: ScaleSetJobId::parse("job-terminal-cleanup").unwrap(),
                    result: ScaleSetJobResult::parse("succeeded").unwrap(),
                },
            )
            .unwrap()
            .0;
        assert_eq!(
            terminal.find_active(&attempt_id).unwrap().attempt().phase(),
            DisposableAttemptPhase::Terminal
        );
        (catalog.into_store(), attempt_id, runner)
    }

    #[test]
    fn reserved_authorization_and_bound_registration_use_same_lock_checkpoints() {
        let root = TempRoot::new("coordinator-checkpoints");
        let host = LimaHostIdentityFixture::new_with_disk_bytes(
            "clone-runtime-coordinator-checkpoints",
            SOURCE,
            SOURCE_DISK,
        );
        let runtime = runtime(&root, &host);
        let mut executor = executor(&host, false);
        install_ready_generation(&root, &host, &runtime);
        let attempt_id = install_reserved_attempt(&root);
        let mut store =
            UnixPersonalWorkerStore::open_or_create_disposable_catalog(root.path()).unwrap();
        let commands_before_clone_authorization = executor.calls.borrow().len();

        assert!(matches!(
            store
                .authorize_disposable_clone_transaction(
                    &runtime,
                    &attempt_id,
                    &executor,
                    &FixedClock,
                )
                .unwrap(),
            DisposableCloneTransactionOutcome::PrecloneCheckpointed {
                phase: DisposableAttemptPhase::CloneAuthorized,
                ..
            }
        ));
        assert_eq!(
            executor.calls.borrow().len() - commands_before_clone_authorization,
            3,
            "clone authorization must not repeat an already-bracketed absence observation"
        );
        assert_eq!(executor.clone_count(), 0);

        assert!(matches!(
            store
                .execute_disposable_clone_transaction(
                    &runtime,
                    &attempt_id,
                    &FakeAdmission::available(),
                    &executor,
                    &FixedClock,
                )
                .unwrap(),
            DisposableCloneTransactionOutcome::Completed(_)
        ));
        executor.target_ready = true;
        let commands_before_registration_checkpoint = executor.calls.borrow().len();
        assert!(matches!(
            store
                .checkpoint_disposable_registration_transaction(
                    &runtime,
                    &attempt_id,
                    &FakeAdmission::available(),
                    &executor,
                    &FixedClock,
                )
                .unwrap(),
            DisposableCloneTransactionOutcome::RegistrationCheckpointed {
                phase: DisposableAttemptPhase::Registering,
                ..
            }
        ));
        assert_eq!(
            executor.calls.borrow().len() - commands_before_registration_checkpoint,
            34,
            "registration must perform one complete readiness composite"
        );
        assert_eq!(
            durable_attempt(&root, &attempt_id).phase(),
            DisposableAttemptPhase::Registering
        );
    }

    #[test]
    fn expired_clone_authorization_checkpoints_unprovisioned_release_without_a_command() {
        let root = TempRoot::new("expired-clone-authorization");
        let host = LimaHostIdentityFixture::new_with_disk_bytes(
            "clone-runtime-expired-clone-authorization",
            SOURCE,
            SOURCE_DISK,
        );
        let runtime = runtime(&root, &host);
        let executor = executor(&host, false);
        install_ready_generation(&root, &host, &runtime);
        let attempt_id = install_reserved_attempt_until(&root, 1_899_999_999_999);
        let store =
            UnixPersonalWorkerStore::open_or_create_disposable_catalog(root.path()).unwrap();
        let mut catalog = DisposableAttemptCatalog::new(store);
        let reserved = catalog.load().unwrap();
        catalog
            .transition(
                reserved.revision(),
                &attempt_id,
                reserved
                    .find_active(&attempt_id)
                    .unwrap()
                    .attempt()
                    .revision(),
                DisposableAttemptCatalogAction::AuthorizeClone,
            )
            .unwrap();
        let mut store = catalog.into_store();

        assert!(matches!(
            store
                .execute_disposable_clone_transaction(
                    &runtime,
                    &attempt_id,
                    &FakeAdmission::available(),
                    &executor,
                    &FixedClock,
                )
                .unwrap(),
            DisposableCloneTransactionOutcome::PrecloneCheckpointed {
                phase: DisposableAttemptPhase::UnprovisionedReleasing,
                ..
            }
        ));
        assert_eq!(executor.clone_count(), 0);
    }

    #[test]
    fn expired_bound_clone_checkpoints_cleanup_without_requiring_guest_readiness() {
        let root = TempRoot::new("expired-registration");
        let host = LimaHostIdentityFixture::new_with_disk_bytes(
            "clone-runtime-expired-registration",
            SOURCE,
            SOURCE_DISK,
        );
        let runtime = runtime(&root, &host);
        let executor = executor(&host, false);
        install_ready_generation(&root, &host, &runtime);
        let attempt_id = install_authorized_attempt(&root);
        runtime
            .clone_once_with(
                &attempt_id,
                &FakeAdmission::available(),
                &executor,
                &FixedClock,
            )
            .unwrap();
        let mut store =
            UnixPersonalWorkerStore::open_or_create_disposable_catalog(root.path()).unwrap();

        assert!(matches!(
            store
                .checkpoint_disposable_registration_transaction(
                    &runtime,
                    &attempt_id,
                    &TimedAdmission {
                        observed_at: 1_900_000_600_001,
                    },
                    &executor,
                    &AfterDeadlineClock,
                )
                .unwrap(),
            DisposableCloneTransactionOutcome::RegistrationCheckpointed {
                phase: DisposableAttemptPhase::Destroying,
                ..
            }
        ));
        assert_eq!(
            durable_attempt(&root, &attempt_id).phase(),
            DisposableAttemptPhase::Destroying
        );
    }

    #[test]
    fn coordinator_drives_reserved_worker_through_runner_and_scale_to_zero_cleanup() {
        let root = TempRoot::new("coordinator-lifecycle");
        let host = LimaHostIdentityFixture::new_with_disk_bytes(
            "clone-runtime-coordinator-lifecycle",
            SOURCE,
            SOURCE_DISK,
        );
        let clone_runtime = runtime(&root, &host);
        let runner_runtime = runner_runtime(&host);
        let mut executor = executor(&host, false);
        install_ready_generation(&root, &host, &clone_runtime);
        let attempt_id = install_reserved_attempt(&root);
        let prepared = current_disposable_prepared_template().unwrap();
        let policy = ScaleSetDeliveryConsumerPolicy::new(
            7,
            "repo",
            "owner",
            &["self-hosted".to_owned()],
            DisposableWorkerResources::new(4_000, 8 * GIB, TARGET_DISK).unwrap(),
            &prepared,
        )
        .unwrap();
        let host_storage_available = Rc::new(Cell::new(true));
        let coordinator = DisposableWorkerCoordinator::new(
            root.path(),
            policy,
            Box::new(MutableHostStorage(Rc::clone(&host_storage_available))),
        );
        let mut bridge = FakeCoordinatorBridge {
            runner: None,
            capacities: Vec::new(),
            polls: std::collections::VecDeque::new(),
            acknowledged: Vec::new(),
        };

        assert!(matches!(
            coordinator
                .supervise_with_bridge(
                    &mut bridge,
                    &clone_runtime,
                    &runner_runtime,
                    &executor,
                    &FixedClock,
                )
                .unwrap(),
            DisposableWorkerCoordinatorDisposition::PrecloneCheckpointed {
                phase: DisposableAttemptPhase::CloneAuthorized,
                ..
            }
        ));
        assert!(matches!(
            coordinator
                .supervise_with_bridge(
                    &mut bridge,
                    &clone_runtime,
                    &runner_runtime,
                    &executor,
                    &FixedClock,
                )
                .unwrap(),
            DisposableWorkerCoordinatorDisposition::CloneCompleted { .. }
        ));
        // Reserved authorization used the normal coordinator poll; clone execution added only its
        // exact under-lock live poll and no stale outer poll.
        assert_eq!(bridge.capacities, [0, 0]);
        executor.target_ready = true;
        assert!(matches!(
            coordinator
                .supervise_with_bridge(
                    &mut bridge,
                    &clone_runtime,
                    &runner_runtime,
                    &executor,
                    &FixedClock,
                )
                .unwrap(),
            DisposableWorkerCoordinatorDisposition::RegistrationCheckpointed {
                phase: DisposableAttemptPhase::Registering,
                ..
            }
        ));
        // Registration likewise adds only its exact under-lock live poll.
        assert_eq!(bridge.capacities, [0, 0, 0]);
        assert!(matches!(
            coordinator
                .supervise_with_bridge(
                    &mut bridge,
                    &clone_runtime,
                    &runner_runtime,
                    &executor,
                    &FixedClock,
                )
                .unwrap(),
            DisposableWorkerCoordinatorDisposition::RunnerCommandCompleted { .. }
        ));

        let store =
            UnixPersonalWorkerStore::open_or_create_disposable_catalog(root.path()).unwrap();
        let mut catalog = DisposableAttemptCatalog::new(store);
        let started = catalog.load().unwrap();
        let assigned = catalog
            .transition(
                started.revision(),
                &attempt_id,
                started
                    .find_active(&attempt_id)
                    .unwrap()
                    .attempt()
                    .revision(),
                DisposableAttemptCatalogAction::RecordAssigned(
                    ScaleSetJobId::parse("job-coordinator").unwrap(),
                ),
            )
            .unwrap()
            .0;
        catalog
            .transition(
                assigned.revision(),
                &attempt_id,
                assigned
                    .find_active(&attempt_id)
                    .unwrap()
                    .attempt()
                    .revision(),
                DisposableAttemptCatalogAction::RecordTerminal {
                    runner: bridge.runner.clone(),
                    job_id: ScaleSetJobId::parse("job-coordinator").unwrap(),
                    result: ScaleSetJobResult::parse("succeeded").unwrap(),
                },
            )
            .unwrap();

        host_storage_available.set(false);

        let mut dispositions = Vec::new();
        for _ in 0..7 {
            dispositions.push(
                coordinator
                    .supervise_with_bridge(
                        &mut bridge,
                        &clone_runtime,
                        &runner_runtime,
                        &executor,
                        &FixedClock,
                    )
                    .unwrap(),
            );
        }
        assert!(matches!(
            dispositions.last(),
            Some(DisposableWorkerCoordinatorDisposition::AttemptRetired { .. })
        ));
        assert!(bridge.runner.is_none());
        assert!(!host.lima_home().join(TARGET).exists());
        let store =
            UnixPersonalWorkerStore::open_or_create_disposable_catalog(root.path()).unwrap();
        let final_catalog = DisposableAttemptCatalog::new(store).load().unwrap();
        assert!(final_catalog.active().is_empty());
        assert_eq!(final_catalog.host_usage().unwrap().workers(), 0);
        assert!(bridge.capacities.iter().all(|capacity| *capacity == 0));
    }

    #[test]
    fn coordinator_resumes_durable_delivery_before_template_or_worker_dispatch() {
        let root = TempRoot::new("coordinator-delivery-restart");
        let host = LimaHostIdentityFixture::new_with_disk_bytes(
            "clone-runtime-coordinator-delivery-restart",
            SOURCE,
            SOURCE_DISK,
        );
        let clone_runtime = runtime(&root, &host);
        let runner_runtime = runner_runtime(&host);
        let executor = executor(&host, false);
        let prepared = current_disposable_prepared_template().unwrap();
        let resources = DisposableWorkerResources::new(4_000, 8 * GIB, TARGET_DISK).unwrap();
        let policy = ScaleSetDeliveryConsumerPolicy::new(
            7,
            "repo",
            "owner",
            &["self-hosted".to_owned()],
            resources,
            &prepared,
        )
        .unwrap();
        let store =
            UnixPersonalWorkerStore::open_or_create_disposable_catalog(root.path()).unwrap();
        let mut catalog = DisposableAttemptCatalog::new(store);
        let (empty, _) = catalog.initialize().unwrap();
        drop(catalog);

        let event = ScaleSetBridgeEvent::Available(ScaleSetBridgeJobEvidence {
            runner_request_id: 41,
            repository: "repo".to_owned(),
            owner: "owner".to_owned(),
            job_id: ScaleSetJobId::parse("job-delivery-restart").unwrap(),
            workflow_run_id: 7,
            request_labels: vec!["self-hosted".to_owned()],
        });
        let poll = ScaleSetBridgePoll::Message {
            message_id: 9,
            statistics: ScaleSetStatistics {
                available_jobs: 1,
                acquired_jobs: 0,
                assigned_jobs: 0,
                running_jobs: 0,
                registered_runners: 0,
                busy_runners: 0,
                idle_runners: 0,
            },
            events: vec![event],
        };
        let delivery = ScaleSetDelivery::from_bridge_poll(&poll).unwrap().unwrap();
        let mut paired =
            UnixPersonalWorkerStore::open_or_create_scale_set_reconcile_transaction(root.path())
                .unwrap();
        paired
            .publish_scale_set_reconciled_delivery(
                empty.revision(),
                &policy,
                &delivery,
                EpochMillis::new(1_900_000_000_000).unwrap(),
            )
            .unwrap();
        drop(paired);

        let coordinator = DisposableWorkerCoordinator::new_for_test(root.path(), policy);
        let mut bridge = FakeCoordinatorBridge {
            runner: None,
            capacities: Vec::new(),
            polls: std::collections::VecDeque::from([poll]),
            acknowledged: vec![41],
        };
        assert_eq!(
            coordinator
                .supervise_with_bridge(
                    &mut bridge,
                    &clone_runtime,
                    &runner_runtime,
                    &executor,
                    &FixedClock,
                )
                .unwrap(),
            DisposableWorkerCoordinatorDisposition::DeliverySettled { acquired: 1 }
        );
        let store =
            UnixPersonalWorkerStore::open_or_create_disposable_catalog(root.path()).unwrap();
        let catalog = DisposableAttemptCatalog::new(store).load().unwrap();
        assert_eq!(catalog.active().len(), 1);
        assert_eq!(catalog.host_usage().unwrap().workers(), 1);
        assert_eq!(bridge.capacities, [1]);
    }

    #[test]
    fn runner_transaction_checkpoints_jit_registration_and_start_before_one_command() {
        let root = TempRoot::new("runner-transaction");
        let host = LimaHostIdentityFixture::new_with_disk_bytes(
            "clone-runtime-runner-transaction",
            SOURCE,
            SOURCE_DISK,
        );
        let clone_runtime = runtime(&root, &host);
        let mut executor = executor(&host, false);
        let attempt_id =
            install_running_registering_attempt(&root, &host, &clone_runtime, &mut executor);
        let mut store =
            UnixPersonalWorkerStore::open_or_create_disposable_catalog(root.path()).unwrap();
        let mut registration = FakeRegistration {
            observed: ScaleSetRunnerLookup::Absent,
            observe_calls: 0,
            jit_calls: 0,
            fail_jit: false,
        };
        let commands_before_runner_transaction = executor.calls.borrow().len();

        let outcome = store
            .execute_disposable_runner_transaction(
                &runner_runtime(&host),
                &clone_runtime,
                &attempt_id,
                &mut registration,
                &executor,
                &FixedClock,
            )
            .unwrap();
        assert_eq!(
            executor.calls.borrow().len() - commands_before_runner_transaction,
            171,
            "runner transaction command budget changed"
        );

        let DisposableRunnerTransactionOutcome::CommandCompleted(receipt) = outcome else {
            panic!("expected completed runner command")
        };
        assert_eq!(receipt.runner().id.get(), 77);
        assert_eq!(registration.observe_calls, 1);
        assert_eq!(registration.jit_calls, 1);
        assert_eq!(executor.runner_launch_count(), 1);
        let durable = durable_attempt(&root, &attempt_id);
        assert!(durable.jit_generation_started());
        assert!(durable.runner_start_started());
        assert_eq!(durable.runner_id().map(ScaleSetRunnerId::get), Some(77));
    }

    #[test]
    fn failed_jit_leaves_no_replay_checkpoint_and_retry_cannot_regenerate() {
        let root = TempRoot::new("runner-jit-failure");
        let host = LimaHostIdentityFixture::new_with_disk_bytes(
            "clone-runtime-runner-jit-failure",
            SOURCE,
            SOURCE_DISK,
        );
        let clone_runtime = runtime(&root, &host);
        let mut executor = executor(&host, false);
        let attempt_id =
            install_running_registering_attempt(&root, &host, &clone_runtime, &mut executor);
        let mut store =
            UnixPersonalWorkerStore::open_or_create_disposable_catalog(root.path()).unwrap();
        let mut registration = FakeRegistration {
            observed: ScaleSetRunnerLookup::Absent,
            observe_calls: 0,
            jit_calls: 0,
            fail_jit: true,
        };

        assert_eq!(
            store
                .execute_disposable_runner_transaction(
                    &runner_runtime(&host),
                    &clone_runtime,
                    &attempt_id,
                    &mut registration,
                    &executor,
                    &FixedClock,
                )
                .unwrap_err()
                .code(),
            "runner_jit_generation_failed"
        );
        assert!(durable_attempt(&root, &attempt_id).jit_generation_started());
        registration.fail_jit = false;
        assert_eq!(
            store
                .execute_disposable_runner_transaction(
                    &runner_runtime(&host),
                    &clone_runtime,
                    &attempt_id,
                    &mut registration,
                    &executor,
                    &FixedClock,
                )
                .unwrap_err()
                .code(),
            "runner_jit_outcome_unknown"
        );
        assert_eq!(registration.jit_calls, 1);
        assert_eq!(executor.runner_launch_count(), 0);
    }

    #[test]
    fn pre_jit_same_name_runner_is_never_adopted_or_deleted() {
        let root = TempRoot::new("runner-pre-jit-conflict");
        let host = LimaHostIdentityFixture::new_with_disk_bytes(
            "clone-runtime-runner-pre-jit-conflict",
            SOURCE,
            SOURCE_DISK,
        );
        let clone_runtime = runtime(&root, &host);
        let mut executor = executor(&host, false);
        let attempt_id =
            install_running_registering_attempt(&root, &host, &clone_runtime, &mut executor);
        let mut store =
            UnixPersonalWorkerStore::open_or_create_disposable_catalog(root.path()).unwrap();
        let mut registration = FakeRegistration {
            observed: ScaleSetRunnerLookup::Present(ScaleSetRunnerReference::new(
                ScaleSetRunnerId::new(78).unwrap(),
                ScaleSetRunnerName::parse(TARGET).unwrap(),
            )),
            observe_calls: 0,
            jit_calls: 0,
            fail_jit: false,
        };

        assert_eq!(
            store
                .execute_disposable_runner_transaction(
                    &runner_runtime(&host),
                    &clone_runtime,
                    &attempt_id,
                    &mut registration,
                    &executor,
                    &FixedClock,
                )
                .unwrap_err()
                .code(),
            "runner_pre_jit_name_conflict"
        );
        let durable = durable_attempt(&root, &attempt_id);
        assert!(!durable.jit_generation_started());
        assert!(durable.runner_id().is_none());
        assert_eq!(registration.jit_calls, 0);
    }

    #[test]
    fn post_jit_discovered_runner_is_bound_for_cleanup_without_secret_replay() {
        let root = TempRoot::new("runner-registration-recovery");
        let host = LimaHostIdentityFixture::new_with_disk_bytes(
            "clone-runtime-runner-registration-recovery",
            SOURCE,
            SOURCE_DISK,
        );
        let clone_runtime = runtime(&root, &host);
        let mut executor = executor(&host, false);
        let attempt_id =
            install_running_registering_attempt(&root, &host, &clone_runtime, &mut executor);
        let store =
            UnixPersonalWorkerStore::open_or_create_disposable_catalog(root.path()).unwrap();
        let mut catalog = DisposableAttemptCatalog::new(store);
        let current = catalog.load().unwrap();
        catalog
            .transition(
                current.revision(),
                &attempt_id,
                current
                    .find_active(&attempt_id)
                    .unwrap()
                    .attempt()
                    .revision(),
                DisposableAttemptCatalogAction::RecordJitGenerationStarted,
            )
            .unwrap();
        let mut store =
            UnixPersonalWorkerStore::open_or_create_disposable_catalog(root.path()).unwrap();
        let mut registration = FakeRegistration {
            observed: ScaleSetRunnerLookup::Present(ScaleSetRunnerReference::new(
                ScaleSetRunnerId::new(78).unwrap(),
                ScaleSetRunnerName::parse(TARGET).unwrap(),
            )),
            observe_calls: 0,
            jit_calls: 0,
            fail_jit: false,
        };

        assert!(matches!(
            store
                .execute_disposable_runner_transaction(
                    &runner_runtime(&host),
                    &clone_runtime,
                    &attempt_id,
                    &mut registration,
                    &executor,
                    &FixedClock,
                )
                .unwrap(),
            DisposableRunnerTransactionOutcome::RegistrationRecovered { .. }
        ));
        let durable = durable_attempt(&root, &attempt_id);
        assert_eq!(durable.runner_id().map(ScaleSetRunnerId::get), Some(78));
        assert!(!durable.runner_start_started());
        assert_eq!(registration.jit_calls, 0);
        assert_eq!(executor.runner_launch_count(), 0);
    }

    #[test]
    fn failed_runner_command_leaves_started_debt_and_never_replays_secret() {
        let root = TempRoot::new("runner-command-failure");
        let host = LimaHostIdentityFixture::new_with_disk_bytes(
            "clone-runtime-runner-command-failure",
            SOURCE,
            SOURCE_DISK,
        );
        let clone_runtime = runtime(&root, &host);
        let mut executor = executor(&host, false);
        let attempt_id =
            install_running_registering_attempt(&root, &host, &clone_runtime, &mut executor);
        executor.fail_runner = true;
        let mut store =
            UnixPersonalWorkerStore::open_or_create_disposable_catalog(root.path()).unwrap();
        let mut registration = FakeRegistration {
            observed: ScaleSetRunnerLookup::Absent,
            observe_calls: 0,
            jit_calls: 0,
            fail_jit: false,
        };

        assert_eq!(
            store
                .execute_disposable_runner_transaction(
                    &runner_runtime(&host),
                    &clone_runtime,
                    &attempt_id,
                    &mut registration,
                    &executor,
                    &FixedClock,
                )
                .unwrap_err()
                .code(),
            "runner_command_failed"
        );
        assert!(durable_attempt(&root, &attempt_id).runner_start_started());
        assert_eq!(registration.jit_calls, 1);
        assert_eq!(executor.runner_launch_count(), 1);

        assert_eq!(
            store
                .execute_disposable_runner_transaction(
                    &runner_runtime(&host),
                    &clone_runtime,
                    &attempt_id,
                    &mut registration,
                    &executor,
                    &FixedClock,
                )
                .unwrap_err()
                .code(),
            "runner_handoff_already_checkpointed"
        );
        assert_eq!(registration.jit_calls, 1);
        assert_eq!(executor.runner_launch_count(), 1);
    }

    #[test]
    fn post_command_target_rebind_withholds_success_without_replay() {
        let root = TempRoot::new("runner-post-command-rebind");
        let host = LimaHostIdentityFixture::new_with_disk_bytes(
            "clone-runtime-runner-post-command-rebind",
            SOURCE,
            SOURCE_DISK,
        );
        let clone_runtime = runtime(&root, &host);
        let mut executor = executor(&host, false);
        let attempt_id =
            install_running_registering_attempt(&root, &host, &clone_runtime, &mut executor);
        executor.rewrite_target_after_runner = true;
        let mut store =
            UnixPersonalWorkerStore::open_or_create_disposable_catalog(root.path()).unwrap();
        let mut registration = FakeRegistration {
            observed: ScaleSetRunnerLookup::Absent,
            observe_calls: 0,
            jit_calls: 0,
            fail_jit: false,
        };

        assert_eq!(
            store
                .execute_disposable_runner_transaction(
                    &runner_runtime(&host),
                    &clone_runtime,
                    &attempt_id,
                    &mut registration,
                    &executor,
                    &FixedClock,
                )
                .unwrap_err()
                .code(),
            "runner_target_post_command_drift"
        );
        assert!(durable_attempt(&root, &attempt_id).runner_start_started());
        assert_eq!(registration.jit_calls, 1);
        assert_eq!(executor.runner_launch_count(), 1);
    }

    #[test]
    fn terminal_cleanup_observes_then_destroys_deregisters_and_releases() {
        let root = TempRoot::new("terminal-cleanup");
        let host = LimaHostIdentityFixture::new_with_disk_bytes(
            "clone-runtime-terminal-cleanup",
            SOURCE,
            SOURCE_DISK,
        );
        let clone_runtime = runtime(&root, &host);
        let mut executor = executor(&host, false);
        let (mut store, attempt_id, runner) =
            install_terminal_attempt(&root, &host, &clone_runtime, &mut executor);
        let mut cleanup = FakeCleanup {
            runner: Some(runner),
            remove_calls: 0,
            fail_remove_after_remove: false,
        };

        assert!(matches!(
            store
                .execute_disposable_cleanup_transaction(
                    &clone_runtime,
                    &attempt_id,
                    &mut cleanup,
                    &executor,
                    &FixedClock,
                )
                .unwrap(),
            DisposableCleanupTransactionOutcome::CleanupCheckpointed {
                phase: DisposableAttemptPhase::Destroying,
                ..
            }
        ));

        let original_identifier = fs::read(host.lima_home().join(TARGET).join("vz-identifier"))
            .expect("read original target identifier");
        let original_identity_byte = original_identifier[19];

        executor.fail_delete_after_remove = true;
        assert_eq!(
            store
                .execute_disposable_cleanup_transaction(
                    &clone_runtime,
                    &attempt_id,
                    &mut cleanup,
                    &executor,
                    &FixedClock,
                )
                .unwrap_err()
                .code(),
            "cleanup_destroy_command_failed"
        );
        assert!(!host.lima_home().join(TARGET).exists());
        assert_eq!(
            durable_attempt(&root, &attempt_id).phase(),
            DisposableAttemptPhase::Destroying
        );

        executor.fail_delete_after_remove = false;
        assert!(matches!(
            store
                .execute_disposable_cleanup_transaction(
                    &clone_runtime,
                    &attempt_id,
                    &mut cleanup,
                    &executor,
                    &FixedClock,
                )
                .unwrap(),
            DisposableCleanupTransactionOutcome::CleanupCheckpointed {
                phase: DisposableAttemptPhase::Deregistering,
                ..
            }
        ));

        // A late/reappearing exact owned VM cannot be ignored after the first absence checkpoint.
        // Cleanup returns to VM destruction before runner deletion or capacity release.
        host.add_instance(TARGET, TARGET_DISK);
        fs::write(
            host.lima_home().join(TARGET).join("vz-identifier"),
            original_identifier,
        )
        .expect("restore original target identifier");
        lima_host_identity_support::rewrite_disk_identity(
            &host.lima_home().join(TARGET).join("disk"),
            original_identity_byte,
        );
        assert!(matches!(
            store
                .execute_disposable_cleanup_transaction(
                    &clone_runtime,
                    &attempt_id,
                    &mut cleanup,
                    &executor,
                    &FixedClock,
                )
                .unwrap(),
            DisposableCleanupTransactionOutcome::VmDestroyed { .. }
        ));
        assert_eq!(cleanup.remove_calls, 0);
        assert_eq!(
            durable_attempt(&root, &attempt_id).phase(),
            DisposableAttemptPhase::Deregistering
        );

        cleanup.fail_remove_after_remove = true;
        assert_eq!(
            store
                .execute_disposable_cleanup_transaction(
                    &clone_runtime,
                    &attempt_id,
                    &mut cleanup,
                    &executor,
                    &FixedClock,
                )
                .unwrap_err()
                .code(),
            "injected_runner_delete_failed"
        );
        assert_eq!(cleanup.remove_calls, 1);
        assert_eq!(
            durable_attempt(&root, &attempt_id).phase(),
            DisposableAttemptPhase::Deregistering
        );

        cleanup.fail_remove_after_remove = false;
        assert!(matches!(
            store
                .execute_disposable_cleanup_transaction(
                    &clone_runtime,
                    &attempt_id,
                    &mut cleanup,
                    &executor,
                    &FixedClock,
                )
                .unwrap(),
            DisposableCleanupTransactionOutcome::CleanupCheckpointed {
                phase: DisposableAttemptPhase::Releasing,
                ..
            }
        ));
        assert!(matches!(
            store
                .execute_disposable_cleanup_transaction(
                    &clone_runtime,
                    &attempt_id,
                    &mut cleanup,
                    &executor,
                    &FixedClock,
                )
                .unwrap(),
            DisposableCleanupTransactionOutcome::CapacityReleased { .. }
        ));
        let complete = durable_attempt(&root, &attempt_id);
        assert_eq!(complete.phase(), DisposableAttemptPhase::Complete);
        let catalog = DisposableAttemptCatalog::new(store).load().unwrap();
        assert_eq!(catalog.host_usage().unwrap().workers(), 0);
        let mut store =
            UnixPersonalWorkerStore::open_or_create_disposable_catalog(root.path()).unwrap();
        assert!(matches!(
            store
                .execute_disposable_cleanup_transaction(
                    &clone_runtime,
                    &attempt_id,
                    &mut cleanup,
                    &executor,
                    &FixedClock,
                )
                .unwrap(),
            DisposableCleanupTransactionOutcome::AttemptRetired { .. }
        ));
        let catalog = DisposableAttemptCatalog::new(store).load().unwrap();
        assert!(catalog.active().is_empty());
        assert!(catalog.find_tombstone(&attempt_id).is_some());
    }

    #[test]
    fn cleanup_refuses_same_name_rebind_immediately_before_delete() {
        let root = TempRoot::new("cleanup-rebind");
        let host = LimaHostIdentityFixture::new_with_disk_bytes(
            "clone-runtime-cleanup-rebind",
            SOURCE,
            SOURCE_DISK,
        );
        let clone_runtime = runtime(&root, &host);
        let mut executor = executor(&host, false);
        let (mut store, attempt_id, runner) =
            install_terminal_attempt(&root, &host, &clone_runtime, &mut executor);
        let mut cleanup = FakeCleanup {
            runner: Some(runner),
            remove_calls: 0,
            fail_remove_after_remove: false,
        };
        store
            .execute_disposable_cleanup_transaction(
                &clone_runtime,
                &attempt_id,
                &mut cleanup,
                &executor,
                &FixedClock,
            )
            .unwrap();
        let deletes_before = executor
            .calls
            .borrow()
            .iter()
            .filter(|argv| argv.iter().any(|value| value == "delete"))
            .count();

        let error = store
            .execute_disposable_cleanup_transaction(
                &clone_runtime,
                &attempt_id,
                &mut cleanup,
                &executor,
                &CleanupRebindClock {
                    host: &host,
                    calls: Cell::new(0),
                },
            )
            .unwrap_err();

        assert_eq!(error.code(), "cleanup_vm_identity_drift");
        let deletes_after = executor
            .calls
            .borrow()
            .iter()
            .filter(|argv| argv.iter().any(|value| value == "delete"))
            .count();
        assert_eq!(deletes_after, deletes_before);
        assert_eq!(
            durable_attempt(&root, &attempt_id).phase(),
            DisposableAttemptPhase::Destroying
        );
    }

    #[test]
    fn cleanup_deletes_owned_vm_after_guest_disk_identity_drift() {
        let root = TempRoot::new("cleanup-guest-disk-drift");
        let host = LimaHostIdentityFixture::new_with_disk_bytes(
            "clone-runtime-cleanup-guest-disk-drift",
            SOURCE,
            SOURCE_DISK,
        );
        let clone_runtime = runtime(&root, &host);
        let mut executor = executor(&host, false);
        let (mut store, attempt_id, runner) =
            install_terminal_attempt(&root, &host, &clone_runtime, &mut executor);
        let mut cleanup = FakeCleanup {
            runner: Some(runner),
            remove_calls: 0,
            fail_remove_after_remove: false,
        };
        store
            .execute_disposable_cleanup_transaction(
                &clone_runtime,
                &attempt_id,
                &mut cleanup,
                &executor,
                &FixedClock,
            )
            .unwrap();

        host.rewrite_disk_identity(TARGET, 0x77);

        assert!(matches!(
            store
                .execute_disposable_cleanup_transaction(
                    &clone_runtime,
                    &attempt_id,
                    &mut cleanup,
                    &executor,
                    &FixedClock,
                )
                .unwrap(),
            DisposableCleanupTransactionOutcome::VmDestroyed { .. }
        ));
        assert!(!host.lima_home().join(TARGET).exists());
    }

    #[test]
    fn cleanup_never_deletes_a_different_same_name_runner_id() {
        let root = TempRoot::new("cleanup-runner-conflict");
        let host = LimaHostIdentityFixture::new_with_disk_bytes(
            "clone-runtime-cleanup-runner-conflict",
            SOURCE,
            SOURCE_DISK,
        );
        let clone_runtime = runtime(&root, &host);
        let mut executor = executor(&host, false);
        let (mut store, attempt_id, _) =
            install_terminal_attempt(&root, &host, &clone_runtime, &mut executor);
        let mut cleanup = FakeCleanup {
            runner: None,
            remove_calls: 0,
            fail_remove_after_remove: false,
        };
        store
            .execute_disposable_cleanup_transaction(
                &clone_runtime,
                &attempt_id,
                &mut cleanup,
                &executor,
                &FixedClock,
            )
            .unwrap();
        store
            .execute_disposable_cleanup_transaction(
                &clone_runtime,
                &attempt_id,
                &mut cleanup,
                &executor,
                &FixedClock,
            )
            .unwrap();
        store
            .execute_disposable_cleanup_transaction(
                &clone_runtime,
                &attempt_id,
                &mut cleanup,
                &executor,
                &FixedClock,
            )
            .unwrap();
        assert_eq!(
            durable_attempt(&root, &attempt_id).phase(),
            DisposableAttemptPhase::Deregistering
        );
        cleanup.runner = Some(ScaleSetRunnerReference::new(
            ScaleSetRunnerId::new(78).unwrap(),
            ScaleSetRunnerName::parse(TARGET).unwrap(),
        ));

        assert_eq!(
            store
                .execute_disposable_cleanup_transaction(
                    &clone_runtime,
                    &attempt_id,
                    &mut cleanup,
                    &executor,
                    &FixedClock,
                )
                .unwrap_err()
                .code(),
            "cleanup_reconcile_failed"
        );
        assert_eq!(cleanup.remove_calls, 0);
        assert_eq!(
            durable_attempt(&root, &attempt_id).phase(),
            DisposableAttemptPhase::Deregistering
        );
    }

    #[test]
    fn successful_clone_checkpoints_once_and_binds_the_observed_host_identity() {
        let root = TempRoot::new("success");
        let host = LimaHostIdentityFixture::new_with_disk_bytes(
            "clone-runtime-success",
            SOURCE,
            SOURCE_DISK,
        );
        let runtime = runtime(&root, &host);
        install_ready_generation(&root, &host, &runtime);
        let attempt_id = install_authorized_attempt(&root);
        let executor = executor(&host, false);
        let admission = FakeAdmission::available();

        let receipt = runtime
            .clone_once_with(&attempt_id, &admission, &executor, &FixedClock)
            .unwrap();
        assert_eq!(receipt.schema_version(), 1);
        assert_eq!(receipt.attempt_revision(), 4);
        assert_eq!(admission.calls.get(), 1);
        assert_eq!(executor.clone_count(), 1);
        let attempt = durable_attempt(&root, &attempt_id);
        assert_eq!(attempt.phase(), DisposableAttemptPhase::CloneStarted);
        assert!(attempt.vm_identity().is_some());
    }

    #[test]
    fn admission_sample_precedes_its_validation_clock() {
        let root = TempRoot::new("admission-clock-order");
        let host = LimaHostIdentityFixture::new_with_disk_bytes(
            "clone-runtime-admission-clock-order",
            SOURCE,
            SOURCE_DISK,
        );
        let runtime = runtime(&root, &host);
        install_ready_generation(&root, &host, &runtime);
        let attempt_id = install_authorized_attempt(&root);
        let executor = executor(&host, false);
        let observed = Rc::new(Cell::new(false));
        let admission = OrderedAdmission {
            observed: Rc::clone(&observed),
        };
        let clock = AdmissionOrderedClock { observed };

        let receipt = runtime
            .clone_once_with(&attempt_id, &admission, &executor, &clock)
            .unwrap();

        assert_eq!(receipt.attempt_revision(), 4);
        assert_eq!(executor.clone_count(), 1);
    }

    #[test]
    fn failed_clone_leaves_unbound_started_debt_and_never_replays() {
        let root = TempRoot::new("failure");
        let host = LimaHostIdentityFixture::new_with_disk_bytes(
            "clone-runtime-failure",
            SOURCE,
            SOURCE_DISK,
        );
        let runtime = runtime(&root, &host);
        install_ready_generation(&root, &host, &runtime);
        let attempt_id = install_authorized_attempt(&root);
        let executor = executor(&host, true);
        let admission = FakeAdmission::available();

        let error = runtime
            .clone_once_with(&attempt_id, &admission, &executor, &FixedClock)
            .unwrap_err();
        assert_eq!(error.kind(), DisposableCloneRuntimeErrorKind::Command);
        let attempt = durable_attempt(&root, &attempt_id);
        assert_eq!(attempt.phase(), DisposableAttemptPhase::CloneStarted);
        assert!(attempt.vm_identity().is_none());

        let current = durable_attempt(&root, &attempt_id);
        assert_eq!(current.phase(), DisposableAttemptPhase::CloneStarted);
        let retry = runtime
            .clone_once_with(&attempt_id, &admission, &executor, &FixedClock)
            .unwrap_err();
        assert_eq!(
            retry.kind(),
            DisposableCloneRuntimeErrorKind::RecoveryRequired
        );
        assert_eq!(executor.clone_count(), 1);
    }

    #[test]
    fn expiry_during_precheckpoint_preflight_preserves_clone_authority() {
        let root = TempRoot::new("deadline");
        let host = LimaHostIdentityFixture::new_with_disk_bytes(
            "clone-runtime-deadline",
            SOURCE,
            SOURCE_DISK,
        );
        let runtime = runtime(&root, &host);
        install_ready_generation(&root, &host, &runtime);
        let attempt_id = install_authorized_attempt(&root);
        let executor = executor(&host, false);
        let admission = FakeAdmission::available();
        let clock = SequencedClock {
            calls: Cell::new(0),
            final_millis: 1_900_000_700_000,
        };

        let error = runtime
            .clone_once_with(&attempt_id, &admission, &executor, &clock)
            .unwrap_err();
        assert_eq!(
            error.kind(),
            DisposableCloneRuntimeErrorKind::RecoveryRequired
        );
        assert_eq!(error.code(), "clone_expired_before_command");
        assert_eq!(executor.clone_count(), 0);
        let attempt = durable_attempt(&root, &attempt_id);
        assert_eq!(attempt.phase(), DisposableAttemptPhase::CloneAuthorized);
        assert!(attempt.vm_identity().is_none());
    }

    #[test]
    fn lost_capacity_before_checkpoint_blocks_without_consuming_clone_authority() {
        let root = TempRoot::new("capacity-lost");
        let host = LimaHostIdentityFixture::new_with_disk_bytes(
            "clone-runtime-capacity-lost",
            SOURCE,
            SOURCE_DISK,
        );
        let runtime = runtime(&root, &host);
        install_ready_generation(&root, &host, &runtime);
        let attempt_id = install_authorized_attempt(&root);
        let executor = executor(&host, false);
        let admission = FakeAdmission {
            calls: Cell::new(0),
            lose_capacity_on: Some(1),
            cancel_on: None,
            error_on: None,
        };

        let error = runtime
            .clone_once_with(&attempt_id, &admission, &executor, &FixedClock)
            .unwrap_err();
        assert_eq!(error.code(), "clone_capacity_lost");
        assert_eq!(executor.clone_count(), 0);
        let attempt = durable_attempt(&root, &attempt_id);
        assert_eq!(attempt.phase(), DisposableAttemptPhase::CloneAuthorized);
    }

    #[test]
    fn cancellation_at_final_command_barrier_is_rechecked_before_clone() {
        let root = TempRoot::new("cancelled");
        let host = LimaHostIdentityFixture::new_with_disk_bytes(
            "clone-runtime-cancelled",
            SOURCE,
            SOURCE_DISK,
        );
        let runtime = runtime(&root, &host);
        install_ready_generation(&root, &host, &runtime);
        let attempt_id = install_authorized_attempt(&root);
        let executor = executor(&host, false);
        let admission = FakeAdmission {
            calls: Cell::new(0),
            lose_capacity_on: None,
            cancel_on: Some(1),
            error_on: None,
        };

        let error = runtime
            .clone_once_with(&attempt_id, &admission, &executor, &FixedClock)
            .unwrap_err();
        assert_eq!(error.code(), "clone_cancelled");
        assert_eq!(admission.calls.get(), 1);
        assert_eq!(executor.clone_count(), 0);
        let attempt = durable_attempt(&root, &attempt_id);
        assert_eq!(attempt.phase(), DisposableAttemptPhase::CloneAuthorized);
        assert!(attempt.vm_identity().is_none());
    }

    #[test]
    fn final_admission_failure_preserves_clone_authority_for_retry() {
        let root = TempRoot::new("admission-failure");
        let host = LimaHostIdentityFixture::new_with_disk_bytes(
            "clone-runtime-admission-failure",
            SOURCE,
            SOURCE_DISK,
        );
        let runtime = runtime(&root, &host);
        install_ready_generation(&root, &host, &runtime);
        let attempt_id = install_authorized_attempt(&root);
        let executor = executor(&host, false);
        let admission = FakeAdmission {
            calls: Cell::new(0),
            lose_capacity_on: None,
            cancel_on: None,
            error_on: Some(1),
        };

        let error = runtime
            .clone_once_with(&attempt_id, &admission, &executor, &FixedClock)
            .unwrap_err();
        assert_eq!(error.code(), "clone_test_admission_failed");
        assert_eq!(admission.calls.get(), 1);
        assert_eq!(executor.clone_count(), 0);
        let attempt = durable_attempt(&root, &attempt_id);
        assert_eq!(attempt.phase(), DisposableAttemptPhase::CloneAuthorized);
        assert!(attempt.vm_identity().is_none());

        let receipt = runtime
            .clone_once_with(
                &attempt_id,
                &FakeAdmission::available(),
                &executor,
                &FixedClock,
            )
            .unwrap();
        assert_eq!(receipt.attempt_revision(), 4);
        assert_eq!(executor.clone_count(), 1);
    }

    #[test]
    fn target_drift_during_final_source_observation_is_never_bound() {
        let root = TempRoot::new("target-drift");
        let host = LimaHostIdentityFixture::new_with_disk_bytes(
            "clone-runtime-target-drift",
            SOURCE,
            SOURCE_DISK,
        );
        let runtime = runtime(&root, &host);
        install_ready_generation(&root, &host, &runtime);
        let attempt_id = install_authorized_attempt(&root);
        let mut executor = executor(&host, false);
        executor.rewrite_target_during_final_source = true;
        let admission = FakeAdmission::available();

        let error = runtime
            .clone_once_with(&attempt_id, &admission, &executor, &FixedClock)
            .unwrap_err();
        assert_eq!(error.code(), "clone_host_identity_drift");
        assert_eq!(executor.clone_count(), 1);
        let attempt = durable_attempt(&root, &attempt_id);
        assert_eq!(attempt.phase(), DisposableAttemptPhase::CloneStarted);
        assert!(attempt.vm_identity().is_none());
    }

    #[test]
    fn final_lima_version_drift_blocks_immediately_before_clone() {
        let root = TempRoot::new("version-drift");
        let host = LimaHostIdentityFixture::new_with_disk_bytes(
            "clone-runtime-version-drift",
            SOURCE,
            SOURCE_DISK,
        );
        let runtime = runtime(&root, &host);
        install_ready_generation(&root, &host, &runtime);
        let attempt_id = install_authorized_attempt(&root);
        let mut executor = executor(&host, false);
        executor.drift_version_on = Some(6);
        let admission = FakeAdmission::available();

        let error = runtime
            .clone_once_with(&attempt_id, &admission, &executor, &FixedClock)
            .unwrap_err();
        assert_eq!(error.code(), "clone_lima_version_mismatch");
        assert_eq!(executor.version_calls.get(), 6);
        assert_eq!(executor.clone_count(), 0);
        let attempt = durable_attempt(&root, &attempt_id);
        assert_eq!(attempt.phase(), DisposableAttemptPhase::CloneAuthorized);
        assert!(attempt.vm_identity().is_none());
    }
}
