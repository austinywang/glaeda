//! Canonical catalog transactions for the native owned-Linux JIT backend.
//!
//! GitHub delivery and attempt state remain the single durable ledger. These transactions replace
//! only the Lima physical mutations with exact owned-Linux task preparation and settlement.

use super::*;

use crate::artifact::Sha256Digest;
use crate::disposable_attempt_catalog::{
    DisposableAttemptCatalogAction, DisposableAttemptCatalogDocument,
};
use crate::disposable_clone_runtime::{
    CloneRuntimeClock, DisposableCleanupRunnerSource, DisposableCleanupTransactionOutcome,
    DisposableCloneAdmissionSource, DisposableCloneRuntimeError, DisposableCloneRuntimeReceipt,
    DisposableCloneTransactionOutcome,
};
use crate::disposable_worker_reconciler::{DisposableAttemptId, DisposableAttemptPhase};
use crate::github_scale_set_bridge::ScaleSetRunnerLookup;
use crate::owned_linux_jit_runtime::{OwnedLinuxJitRuntime, OwnedLinuxTaskState};
use crate::process::TimedCommandExecutor;

impl UnixPersonalWorkerStore {
    pub(crate) fn authorize_owned_linux_task_transaction(
        &mut self,
        runtime: &OwnedLinuxJitRuntime,
        attempt_id: &DisposableAttemptId,
        executor: &impl TimedCommandExecutor,
        clock: &impl CloneRuntimeClock,
    ) -> Result<DisposableCloneTransactionOutcome, DisposableCloneRuntimeError> {
        let _lock = self.prepare_owned_linux_transaction("owned_linux_authorize")?;
        let current = self.load_owned_linux_catalog("owned_linux_authorize")?;
        let reservation = current
            .find_active(attempt_id)
            .ok_or_else(|| DisposableCloneRuntimeError::durable("owned_linux_attempt_missing"))?;
        if reservation.attempt().phase() != DisposableAttemptPhase::Reserved {
            return Err(DisposableCloneRuntimeError::recovery(
                "owned_linux_authorize_phase_mismatch",
            ));
        }
        runtime.validate_reservation_generation(reservation)?;
        if runtime.probe(reservation, executor)? != OwnedLinuxTaskState::Absent {
            return Err(DisposableCloneRuntimeError::recovery(
                "owned_linux_task_exists_before_authorization",
            ));
        }
        let now = clock.epoch_millis().map_err(|_| {
            DisposableCloneRuntimeError::observation("owned_linux_clock_unavailable")
        })?;
        let action = if now > reservation.attempt().not_after() {
            DisposableAttemptCatalogAction::BeginUnprovisionedRelease
        } else {
            DisposableAttemptCatalogAction::AuthorizeClone
        };
        self.publish_owned_linux_action(&current, attempt_id, action)
    }

    pub(crate) fn execute_owned_linux_task_transaction(
        &mut self,
        runtime: &OwnedLinuxJitRuntime,
        attempt_id: &DisposableAttemptId,
        admission: &impl DisposableCloneAdmissionSource,
        executor: &impl TimedCommandExecutor,
        clock: &impl CloneRuntimeClock,
    ) -> Result<DisposableCloneTransactionOutcome, DisposableCloneRuntimeError> {
        let _lock = self.prepare_owned_linux_transaction("owned_linux_prepare")?;
        let current = self.load_owned_linux_catalog("owned_linux_prepare")?;
        let reservation = current
            .find_active(attempt_id)
            .ok_or_else(|| DisposableCloneRuntimeError::durable("owned_linux_attempt_missing"))?;
        if reservation.attempt().phase() != DisposableAttemptPhase::CloneAuthorized {
            return Err(DisposableCloneRuntimeError::recovery(
                "owned_linux_prepare_phase_mismatch",
            ));
        }
        runtime.validate_reservation_generation(reservation)?;

        let now = clock.epoch_millis().map_err(|_| {
            DisposableCloneRuntimeError::observation("owned_linux_clock_unavailable")
        })?;
        let physical = runtime.probe(reservation, executor)?;
        let observed = admission.observe(&current, reservation)?;
        let observed_at = clock.epoch_millis().map_err(|_| {
            DisposableCloneRuntimeError::observation("owned_linux_clock_unavailable")
        })?;
        observed.validate_identity_and_freshness_for(&current, reservation, observed_at)?;
        let cleanup = !observed.capacity_reserved()
            || observed.cancellation_requested()
            || now > reservation.attempt().not_after()
            || observed_at > reservation.attempt().not_after();
        if cleanup {
            if physical != OwnedLinuxTaskState::Absent {
                runtime.cleanup(reservation, executor)?;
            }
            return self.publish_owned_linux_action(
                &current,
                attempt_id,
                DisposableAttemptCatalogAction::BeginUnprovisionedRelease,
            );
        }

        let identity = match physical {
            OwnedLinuxTaskState::Absent => runtime.prepare(reservation, executor)?,
            OwnedLinuxTaskState::Preparing => {
                let observed = runtime.confirm(reservation, executor)?;
                crate::disposable_worker_reconciler::DisposableVmIdentity::parse(&observed)
                    .map_err(|_| {
                        DisposableCloneRuntimeError::recovery(
                            "owned_linux_recovered_task_identity_invalid",
                        )
                    })?
            }
            OwnedLinuxTaskState::Launching => {
                return Err(DisposableCloneRuntimeError::recovery(
                    "owned_linux_launch_before_start_checkpoint",
                ));
            }
        };

        let command_now = clock.epoch_millis().map_err(|_| {
            DisposableCloneRuntimeError::observation("owned_linux_clock_unavailable")
        })?;
        if command_now > reservation.attempt().not_after() {
            runtime.cleanup(reservation, executor)?;
            return self.publish_owned_linux_action(
                &current,
                attempt_id,
                DisposableAttemptCatalogAction::BeginUnprovisionedRelease,
            );
        }
        let started = current
            .checkpoint_clone_started(attempt_id, reservation.attempt().revision())
            .map_err(|_| {
                DisposableCloneRuntimeError::recovery("owned_linux_start_checkpoint_refused")
            })?;
        started.validate_successor_of(&current).map_err(|_| {
            DisposableCloneRuntimeError::recovery("owned_linux_start_checkpoint_invalid")
        })?;
        self.publish_owned_linux_catalog(
            &started,
            "owned_linux_start_stage_failed",
            "owned_linux_start_publish_ambiguous",
        )?;

        let started_reservation = started
            .find_active(attempt_id)
            .ok_or_else(|| DisposableCloneRuntimeError::durable("owned_linux_attempt_missing"))?;
        let bound = started
            .bind_vm_identity_after_clone(
                attempt_id,
                started_reservation.attempt().revision(),
                identity.clone(),
            )
            .map_err(|_| {
                DisposableCloneRuntimeError::recovery("owned_linux_identity_bind_refused")
            })?;
        bound
            .validate_recovery_successor_of(&started)
            .map_err(|_| {
                DisposableCloneRuntimeError::recovery("owned_linux_identity_bind_invalid")
            })?;
        self.publish_owned_linux_catalog(
            &bound,
            "owned_linux_identity_stage_failed",
            "owned_linux_identity_publish_ambiguous",
        )?;
        let attempt = bound
            .find_active(attempt_id)
            .ok_or_else(|| DisposableCloneRuntimeError::durable("owned_linux_attempt_missing"))?
            .attempt();
        let command_identity = Sha256Digest::parse(identity.as_str()).map_err(|_| {
            DisposableCloneRuntimeError::recovery("owned_linux_identity_digest_invalid")
        })?;
        Ok(DisposableCloneTransactionOutcome::Completed(
            DisposableCloneRuntimeReceipt::from_owned_linux(
                attempt_id,
                bound.revision().get(),
                attempt.revision().get(),
                command_identity,
            ),
        ))
    }

    pub(crate) fn checkpoint_owned_linux_registration_transaction(
        &mut self,
        runtime: &OwnedLinuxJitRuntime,
        attempt_id: &DisposableAttemptId,
        admission: &impl DisposableCloneAdmissionSource,
        executor: &impl TimedCommandExecutor,
        clock: &impl CloneRuntimeClock,
    ) -> Result<DisposableCloneTransactionOutcome, DisposableCloneRuntimeError> {
        let _lock = self.prepare_owned_linux_transaction("owned_linux_registration")?;
        let current = self.load_owned_linux_catalog("owned_linux_registration")?;
        let reservation = current
            .find_active(attempt_id)
            .ok_or_else(|| DisposableCloneRuntimeError::durable("owned_linux_attempt_missing"))?;
        if reservation.attempt().phase() != DisposableAttemptPhase::CloneStarted {
            return Err(DisposableCloneRuntimeError::recovery(
                "owned_linux_registration_phase_mismatch",
            ));
        }
        runtime.validate_reservation_generation(reservation)?;

        if reservation.attempt().vm_identity().is_none() {
            let physical = runtime.probe(reservation, executor)?;
            if physical == OwnedLinuxTaskState::Launching {
                return Err(DisposableCloneRuntimeError::recovery(
                    "owned_linux_unbound_task_already_launching",
                ));
            }
            if physical == OwnedLinuxTaskState::Absent {
                return self.publish_owned_linux_action(
                    &current,
                    attempt_id,
                    DisposableAttemptCatalogAction::BeginCleanup,
                );
            }
            let observed = runtime.confirm(reservation, executor)?;
            let identity =
                crate::disposable_worker_reconciler::DisposableVmIdentity::parse(&observed)
                    .map_err(|_| {
                        DisposableCloneRuntimeError::recovery(
                            "owned_linux_recovered_task_identity_invalid",
                        )
                    })?;
            let bound = current
                .bind_vm_identity_after_clone(
                    attempt_id,
                    reservation.attempt().revision(),
                    identity.clone(),
                )
                .map_err(|_| {
                    DisposableCloneRuntimeError::recovery("owned_linux_identity_bind_refused")
                })?;
            bound
                .validate_recovery_successor_of(&current)
                .map_err(|_| {
                    DisposableCloneRuntimeError::recovery("owned_linux_identity_bind_invalid")
                })?;
            self.publish_owned_linux_catalog(
                &bound,
                "owned_linux_identity_stage_failed",
                "owned_linux_identity_publish_ambiguous",
            )?;
            let attempt = bound
                .find_active(attempt_id)
                .ok_or_else(|| DisposableCloneRuntimeError::durable("owned_linux_attempt_missing"))?
                .attempt();
            let command_identity = Sha256Digest::parse(identity.as_str()).map_err(|_| {
                DisposableCloneRuntimeError::recovery("owned_linux_identity_digest_invalid")
            })?;
            return Ok(DisposableCloneTransactionOutcome::Completed(
                DisposableCloneRuntimeReceipt::from_owned_linux(
                    attempt_id,
                    bound.revision().get(),
                    attempt.revision().get(),
                    command_identity,
                ),
            ));
        }

        runtime.confirm(reservation, executor)?;
        let observed = admission.observe(&current, reservation)?;
        let observed_at = clock.epoch_millis().map_err(|_| {
            DisposableCloneRuntimeError::observation("owned_linux_clock_unavailable")
        })?;
        observed.validate_identity_and_freshness_for(&current, reservation, observed_at)?;
        let action = if !observed.capacity_reserved()
            || observed.cancellation_requested()
            || observed_at > reservation.attempt().not_after()
        {
            DisposableAttemptCatalogAction::BeginCleanup
        } else {
            DisposableAttemptCatalogAction::BeginRegistration
        };
        self.publish_owned_linux_action(&current, attempt_id, action)
    }

    pub(crate) fn execute_owned_linux_cleanup_transaction(
        &mut self,
        runtime: &OwnedLinuxJitRuntime,
        attempt_id: &DisposableAttemptId,
        runner_source: &mut impl DisposableCleanupRunnerSource,
        executor: &impl TimedCommandExecutor,
    ) -> Result<DisposableCleanupTransactionOutcome, DisposableCloneRuntimeError> {
        let _lock = self.prepare_owned_linux_transaction("owned_linux_cleanup")?;
        let current = self.load_owned_linux_catalog("owned_linux_cleanup")?;
        let reservation = current.find_active(attempt_id).ok_or_else(|| {
            DisposableCloneRuntimeError::durable("owned_linux_cleanup_attempt_missing")
        })?;
        runtime.validate_reservation_generation(reservation)?;
        let phase = reservation.attempt().phase();

        if phase == DisposableAttemptPhase::UnprovisionedReleasing {
            runtime.cleanup(reservation, executor)?;
            return self.publish_owned_linux_cleanup_action(
                &current,
                attempt_id,
                DisposableAttemptCatalogAction::CompleteUnprovisioned,
            );
        }
        if phase == DisposableAttemptPhase::Complete {
            let retired = current
                .retire_complete(attempt_id, reservation.attempt().revision())
                .map_err(|_| {
                    DisposableCloneRuntimeError::recovery("owned_linux_retirement_refused")
                })?;
            retired.validate_successor_of(&current).map_err(|_| {
                DisposableCloneRuntimeError::recovery("owned_linux_retirement_invalid")
            })?;
            self.publish_owned_linux_catalog(
                &retired,
                "owned_linux_retirement_stage_failed",
                "owned_linux_retirement_publish_ambiguous",
            )?;
            return Ok(DisposableCleanupTransactionOutcome::AttemptRetired {
                attempt_id: attempt_id.as_str().to_owned(),
            });
        }
        if phase == DisposableAttemptPhase::Terminal {
            return self.publish_owned_linux_cleanup_action(
                &current,
                attempt_id,
                DisposableAttemptCatalogAction::BeginCleanup,
            );
        }

        if matches!(
            phase,
            DisposableAttemptPhase::Registering | DisposableAttemptPhase::Assigned
        ) && reservation.attempt().runner_id().is_some()
            && !reservation.attempt().runner_start_started()
        {
            return self.publish_owned_linux_cleanup_action(
                &current,
                attempt_id,
                DisposableAttemptCatalogAction::BeginCleanup,
            );
        }

        match phase {
            DisposableAttemptPhase::Destroying => {
                match runner_source.observe_runner(reservation.attempt().runner_name())? {
                    ScaleSetRunnerLookup::Absent => self.publish_owned_linux_cleanup_action(
                        &current,
                        attempt_id,
                        DisposableAttemptCatalogAction::AdvanceCleanup(
                            DisposableAttemptPhase::Deregistering,
                        ),
                    ),
                    ScaleSetRunnerLookup::Present(runner) => {
                        if reservation.attempt().runner_id().is_none() {
                            if !reservation.attempt().jit_generation_started() {
                                return Err(DisposableCloneRuntimeError::recovery(
                                    "owned_linux_runner_exists_before_jit",
                                ));
                            }
                            return self.publish_owned_linux_cleanup_action(
                                &current,
                                attempt_id,
                                DisposableAttemptCatalogAction::RecordRegistration(runner),
                            );
                        }
                        if reservation.attempt().runner_id() != Some(runner.id)
                            || reservation.attempt().runner_name() != &runner.name
                        {
                            return Err(DisposableCloneRuntimeError::recovery(
                                "owned_linux_cleanup_runner_identity_drift",
                            ));
                        }
                        runner_source.remove_runner(&runner)?;
                        match runner_source.observe_runner(reservation.attempt().runner_name())? {
                            ScaleSetRunnerLookup::Absent => {
                                Ok(DisposableCleanupTransactionOutcome::RunnerDeleted {
                                    attempt_id: attempt_id.as_str().to_owned(),
                                })
                            }
                            ScaleSetRunnerLookup::Present(_) => {
                                Err(DisposableCloneRuntimeError::recovery(
                                    "owned_linux_runner_delete_not_observed",
                                ))
                            }
                        }
                    }
                }
            }
            DisposableAttemptPhase::Deregistering => {
                runtime.cleanup(reservation, executor)?;
                self.publish_owned_linux_cleanup_action(
                    &current,
                    attempt_id,
                    DisposableAttemptCatalogAction::AdvanceCleanup(
                        DisposableAttemptPhase::Releasing,
                    ),
                )
            }
            DisposableAttemptPhase::Releasing => {
                if !matches!(
                    runner_source.observe_runner(reservation.attempt().runner_name())?,
                    ScaleSetRunnerLookup::Absent
                ) {
                    return Err(DisposableCloneRuntimeError::recovery(
                        "owned_linux_runner_reappeared_during_release",
                    ));
                }
                if runtime.probe(reservation, executor)? != OwnedLinuxTaskState::Absent {
                    return Err(DisposableCloneRuntimeError::recovery(
                        "owned_linux_task_present_during_release",
                    ));
                }
                self.publish_owned_linux_cleanup_action(
                    &current,
                    attempt_id,
                    DisposableAttemptCatalogAction::AdvanceCleanup(
                        DisposableAttemptPhase::Complete,
                    ),
                )
            }
            _ => Err(DisposableCloneRuntimeError::recovery(
                "owned_linux_cleanup_phase_mismatch",
            )),
        }
    }

    fn prepare_owned_linux_transaction(
        &mut self,
        prefix: &'static str,
    ) -> Result<StoreMutationLock, DisposableCloneRuntimeError> {
        let lock = self
            .acquire_mutation_lock()
            .map_err(|_| DisposableCloneRuntimeError::durable(prefix))?;
        synchronize_directory(&self.directory, "personal worker store directory")
            .map_err(|_| DisposableCloneRuntimeError::durable(prefix))?;
        super::disposable_template_generation::refuse_unsettled(self).map_err(|_| {
            DisposableCloneRuntimeError::recovery("owned_linux_template_recovery_required")
        })?;
        super::scale_set_delivery_recovery::refuse_unsettled(self).map_err(|_| {
            DisposableCloneRuntimeError::recovery("owned_linux_delivery_recovery_required")
        })?;
        super::lima_authority::refuse_unsettled_lima_authority(self).map_err(|_| {
            DisposableCloneRuntimeError::recovery("owned_linux_lima_recovery_required")
        })?;
        self.refuse_unsettled_personal_worker_state().map_err(|_| {
            DisposableCloneRuntimeError::recovery("owned_linux_worker_recovery_required")
        })?;
        self.recover_catalog_locked().map_err(|_| {
            DisposableCloneRuntimeError::recovery("owned_linux_catalog_recovery_failed")
        })?;
        Ok(lock)
    }

    fn load_owned_linux_catalog(
        &self,
        code: &'static str,
    ) -> Result<DisposableAttemptCatalogDocument, DisposableCloneRuntimeError> {
        self.load_catalog_named(super::disposable_attempt_catalog::CATALOG_DOCUMENT)
            .map_err(|_| DisposableCloneRuntimeError::durable(code))?
            .ok_or_else(|| DisposableCloneRuntimeError::durable(code))
    }

    fn publish_owned_linux_action(
        &mut self,
        current: &DisposableAttemptCatalogDocument,
        attempt_id: &DisposableAttemptId,
        action: DisposableAttemptCatalogAction,
    ) -> Result<DisposableCloneTransactionOutcome, DisposableCloneRuntimeError> {
        let reservation = current
            .find_active(attempt_id)
            .ok_or_else(|| DisposableCloneRuntimeError::durable("owned_linux_attempt_missing"))?;
        let next = current
            .replace_attempt(attempt_id, reservation.attempt().revision(), action)
            .map_err(|_| DisposableCloneRuntimeError::recovery("owned_linux_checkpoint_refused"))?;
        next.validate_successor_of(current)
            .map_err(|_| DisposableCloneRuntimeError::recovery("owned_linux_checkpoint_invalid"))?;
        let phase = next
            .find_active(attempt_id)
            .ok_or_else(|| DisposableCloneRuntimeError::durable("owned_linux_attempt_missing"))?
            .attempt()
            .phase();
        self.publish_owned_linux_catalog(
            &next,
            "owned_linux_checkpoint_stage_failed",
            "owned_linux_checkpoint_publish_ambiguous",
        )?;
        Ok(
            DisposableCloneTransactionOutcome::RegistrationCheckpointed {
                attempt_id: attempt_id.as_str().to_owned(),
                phase,
            },
        )
    }

    fn publish_owned_linux_cleanup_action(
        &mut self,
        current: &DisposableAttemptCatalogDocument,
        attempt_id: &DisposableAttemptId,
        action: DisposableAttemptCatalogAction,
    ) -> Result<DisposableCleanupTransactionOutcome, DisposableCloneRuntimeError> {
        let reservation = current.find_active(attempt_id).ok_or_else(|| {
            DisposableCloneRuntimeError::durable("owned_linux_cleanup_attempt_missing")
        })?;
        let next = current
            .replace_attempt(attempt_id, reservation.attempt().revision(), action)
            .map_err(|_| {
                DisposableCloneRuntimeError::recovery("owned_linux_cleanup_checkpoint_refused")
            })?;
        next.validate_successor_of(current).map_err(|_| {
            DisposableCloneRuntimeError::recovery("owned_linux_cleanup_checkpoint_invalid")
        })?;
        let phase = next
            .find_active(attempt_id)
            .ok_or_else(|| {
                DisposableCloneRuntimeError::durable("owned_linux_cleanup_attempt_missing")
            })?
            .attempt()
            .phase();
        self.publish_owned_linux_catalog(
            &next,
            "owned_linux_cleanup_stage_failed",
            "owned_linux_cleanup_publish_ambiguous",
        )?;
        if phase == DisposableAttemptPhase::Complete {
            Ok(DisposableCleanupTransactionOutcome::CapacityReleased {
                attempt_id: attempt_id.as_str().to_owned(),
            })
        } else {
            Ok(DisposableCleanupTransactionOutcome::CleanupCheckpointed {
                attempt_id: attempt_id.as_str().to_owned(),
                phase,
            })
        }
    }

    fn publish_owned_linux_catalog(
        &mut self,
        document: &DisposableAttemptCatalogDocument,
        stage_code: &'static str,
        publish_code: &'static str,
    ) -> Result<(), DisposableCloneRuntimeError> {
        let mut staged = self
            .stage_catalog(document)
            .map_err(|_| DisposableCloneRuntimeError::durable(stage_code))?;
        self.publish_named_staged(
            &mut staged,
            super::disposable_attempt_catalog::CATALOG_DOCUMENT,
            false,
        )
        .map_err(|_| DisposableCloneRuntimeError::recovery(publish_code))
    }
}
