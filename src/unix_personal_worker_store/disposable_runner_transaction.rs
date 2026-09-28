//! Canonical-lock transaction for one JIT registration and one disposable runner command.
//!
//! The transaction distinguishes proven GitHub registration absence from observation failure,
//! repeatedly reconfirms the complete running Lima target, publishes the exact service-assigned
//! runner ID, then publishes a no-replay Started checkpoint before the only secret-bearing command.
//! A discovered registration is retained for exact cleanup and never receives a second JIT value.

#![allow(dead_code)] // Wired into the production supervisor in the next lifecycle-composition slice.

use super::*;

use crate::disposable_attempt_catalog::DisposableAttemptCatalogAction;
use crate::disposable_clone_runtime::CloneRuntimeClock;
use crate::disposable_runner_runtime::{
    DisposableRunnerCommandReceipt, DisposableRunnerRegistrationSource, DisposableRunnerRuntime,
    DisposableRunnerRuntimeError, DisposableRunnerTargetRuntime,
};
use crate::disposable_worker_reconciler::DisposableAttemptId;
use crate::github_scale_set_bridge::ScaleSetRunnerLookup;
use crate::process::TimedCommandExecutor;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DisposableRunnerTransactionOutcome {
    RegistrationRecovered { attempt_id: String },
    CommandCompleted(DisposableRunnerCommandReceipt),
}

impl UnixPersonalWorkerStore {
    pub(crate) fn execute_disposable_runner_transaction<T: DisposableRunnerTargetRuntime>(
        &mut self,
        runtime: &DisposableRunnerRuntime,
        target_runtime: &T,
        attempt_id: &DisposableAttemptId,
        registration: &mut impl DisposableRunnerRegistrationSource,
        executor: &impl TimedCommandExecutor,
        clock: &impl CloneRuntimeClock,
    ) -> Result<DisposableRunnerTransactionOutcome, DisposableRunnerRuntimeError> {
        let _lock = self
            .acquire_mutation_lock()
            .map_err(|_| DisposableRunnerRuntimeError::durable("runner_store_lock_unavailable"))?;
        synchronize_directory(&self.directory, "personal worker store directory")
            .map_err(|_| DisposableRunnerRuntimeError::durable("runner_store_sync_failed"))?;
        super::disposable_template_generation::refuse_unsettled(self).map_err(|_| {
            DisposableRunnerRuntimeError::recovery("runner_template_recovery_required")
        })?;
        super::scale_set_delivery_recovery::refuse_unsettled(self).map_err(|_| {
            DisposableRunnerRuntimeError::recovery("runner_delivery_recovery_required")
        })?;
        super::lima_authority::refuse_unsettled_lima_authority(self)
            .map_err(|_| DisposableRunnerRuntimeError::recovery("runner_lima_recovery_required"))?;
        self.refuse_unsettled_personal_worker_state().map_err(|_| {
            DisposableRunnerRuntimeError::recovery("runner_worker_recovery_required")
        })?;
        self.recover_catalog_locked().map_err(|_| {
            DisposableRunnerRuntimeError::recovery("runner_catalog_recovery_failed")
        })?;

        let current = self
            .load_catalog_named(super::disposable_attempt_catalog::CATALOG_DOCUMENT)
            .map_err(|_| DisposableRunnerRuntimeError::durable("runner_catalog_unavailable"))?
            .ok_or_else(|| DisposableRunnerRuntimeError::durable("runner_catalog_missing"))?;
        let reservation = current
            .find_active(attempt_id)
            .ok_or_else(|| DisposableRunnerRuntimeError::durable("runner_attempt_missing"))?;
        if reservation.attempt().runner_start_started()
            || reservation.attempt().runner_id().is_some()
        {
            return Err(DisposableRunnerRuntimeError::recovery(
                "runner_handoff_already_checkpointed",
            ));
        }

        if reservation.attempt().jit_generation_started() {
            return match registration.observe_runner(reservation.attempt().runner_name())? {
                ScaleSetRunnerLookup::Present(runner) => {
                    let recovered = current
                        .replace_attempt(
                            attempt_id,
                            reservation.attempt().revision(),
                            DisposableAttemptCatalogAction::RecordRegistration(runner),
                        )
                        .map_err(|_| {
                            DisposableRunnerRuntimeError::recovery(
                                "runner_registration_recovery_refused",
                            )
                        })?;
                    recovered.validate_successor_of(&current).map_err(|_| {
                        DisposableRunnerRuntimeError::recovery(
                            "runner_registration_recovery_invalid",
                        )
                    })?;
                    self.publish_runner_catalog(
                        &recovered,
                        "runner_registration_recovery_stage_failed",
                        "runner_registration_recovery_publish_ambiguous",
                    )?;
                    Ok(DisposableRunnerTransactionOutcome::RegistrationRecovered {
                        attempt_id: attempt_id.as_str().to_owned(),
                    })
                }
                ScaleSetRunnerLookup::Absent => Err(DisposableRunnerRuntimeError::recovery(
                    "runner_jit_outcome_unknown",
                )),
            };
        }

        let now = clock
            .epoch_millis()
            .map_err(|_| DisposableRunnerRuntimeError::observation("runner_clock_unavailable"))?;
        runtime.validate_pre_jit_candidate(reservation, now)?;
        let ready = target_runtime.confirm_runner_target(reservation, executor, clock)?;
        target_runtime.reconfirm_runner_target(&ready, reservation, executor, clock)?;

        match registration.observe_runner(reservation.attempt().runner_name())? {
            ScaleSetRunnerLookup::Present(runner) => {
                let _ = runner;
                return Err(DisposableRunnerRuntimeError::recovery(
                    "runner_pre_jit_name_conflict",
                ));
            }
            ScaleSetRunnerLookup::Absent => {}
        }

        let ready_after_lookup =
            target_runtime.confirm_runner_target(reservation, executor, clock)?;
        target_runtime.reconfirm_runner_target(&ready, reservation, executor, clock)?;
        target_runtime.reconfirm_runner_target(
            &ready_after_lookup,
            reservation,
            executor,
            clock,
        )?;
        let before_jit = clock
            .epoch_millis()
            .map_err(|_| DisposableRunnerRuntimeError::observation("runner_clock_unavailable"))?;
        runtime.validate_pre_jit_candidate(reservation, before_jit)?;

        let jit_started = current
            .replace_attempt(
                attempt_id,
                reservation.attempt().revision(),
                DisposableAttemptCatalogAction::RecordJitGenerationStarted,
            )
            .map_err(|_| DisposableRunnerRuntimeError::recovery("runner_jit_checkpoint_refused"))?;
        jit_started
            .validate_successor_of(&current)
            .map_err(|_| DisposableRunnerRuntimeError::recovery("runner_jit_checkpoint_invalid"))?;
        self.publish_runner_catalog(
            &jit_started,
            "runner_jit_stage_failed",
            "runner_jit_publish_ambiguous",
        )?;

        let jit_started_reservation = jit_started
            .find_active(attempt_id)
            .ok_or_else(|| DisposableRunnerRuntimeError::durable("runner_attempt_missing"))?;
        let ready_after_jit_checkpoint =
            target_runtime.confirm_runner_target(jit_started_reservation, executor, clock)?;
        target_runtime.reconfirm_runner_target(
            &ready_after_lookup,
            jit_started_reservation,
            executor,
            clock,
        )?;
        target_runtime.reconfirm_runner_target(
            &ready_after_jit_checkpoint,
            jit_started_reservation,
            executor,
            clock,
        )?;
        let before_generate = clock
            .epoch_millis()
            .map_err(|_| DisposableRunnerRuntimeError::observation("runner_clock_unavailable"))?;
        if before_generate > jit_started_reservation.attempt().not_after() {
            return Err(DisposableRunnerRuntimeError::recovery(
                "runner_attempt_expired_before_jit",
            ));
        }
        target_runtime.reconfirm_runner_target(
            &ready_after_jit_checkpoint,
            jit_started_reservation,
            executor,
            clock,
        )?;

        let jit = registration.generate_jit(jit_started_reservation.attempt().runner_name())?;
        let plan = runtime.plan_launch(jit_started_reservation, before_generate, jit)?;
        let registered = jit_started
            .replace_attempt(
                attempt_id,
                jit_started_reservation.attempt().revision(),
                DisposableAttemptCatalogAction::RecordRegistration(plan.runner().clone()),
            )
            .map_err(|_| {
                DisposableRunnerRuntimeError::recovery("runner_registration_checkpoint_refused")
            })?;
        registered
            .validate_successor_of(&jit_started)
            .map_err(|_| {
                DisposableRunnerRuntimeError::recovery("runner_registration_checkpoint_invalid")
            })?;
        self.publish_runner_catalog(
            &registered,
            "runner_registration_stage_failed",
            "runner_registration_publish_ambiguous",
        )?;

        let registered_reservation = registered
            .find_active(attempt_id)
            .ok_or_else(|| DisposableRunnerRuntimeError::durable("runner_attempt_missing"))?;
        let ready_after_jit =
            target_runtime.confirm_runner_target(registered_reservation, executor, clock)?;
        target_runtime.reconfirm_runner_target(
            &ready_after_lookup,
            registered_reservation,
            executor,
            clock,
        )?;
        target_runtime.reconfirm_runner_target(
            &ready_after_jit,
            registered_reservation,
            executor,
            clock,
        )?;
        let before_checkpoint = clock
            .epoch_millis()
            .map_err(|_| DisposableRunnerRuntimeError::observation("runner_clock_unavailable"))?;
        plan.validate_registered(registered_reservation, before_checkpoint)?;

        let started = registered
            .replace_attempt(
                attempt_id,
                registered_reservation.attempt().revision(),
                DisposableAttemptCatalogAction::RecordRunnerStartStarted,
            )
            .map_err(|_| {
                DisposableRunnerRuntimeError::recovery("runner_start_checkpoint_refused")
            })?;
        started.validate_successor_of(&registered).map_err(|_| {
            DisposableRunnerRuntimeError::recovery("runner_start_checkpoint_invalid")
        })?;
        self.publish_runner_catalog(
            &started,
            "runner_start_stage_failed",
            "runner_start_publish_ambiguous",
        )?;

        let started_reservation = started
            .find_active(attempt_id)
            .ok_or_else(|| DisposableRunnerRuntimeError::durable("runner_attempt_missing"))?;
        target_runtime.reconfirm_runner_target(
            &ready_after_jit,
            started_reservation,
            executor,
            clock,
        )?;
        let ready_after_checkpoint =
            target_runtime.confirm_runner_target(started_reservation, executor, clock)?;
        target_runtime.reconfirm_runner_target(
            &ready_after_checkpoint,
            started_reservation,
            executor,
            clock,
        )?;
        let command_started_at = clock
            .epoch_millis()
            .map_err(|_| DisposableRunnerRuntimeError::observation("runner_clock_unavailable"))?;
        target_runtime.reconfirm_runner_target(
            &ready_after_checkpoint,
            started_reservation,
            executor,
            clock,
        )?;
        let receipt = plan.execute_started(started_reservation, command_started_at, executor)?;
        target_runtime
            .reconfirm_runner_target(
                &ready_after_checkpoint,
                started_reservation,
                executor,
                clock,
            )
            .map_err(|_| {
                DisposableRunnerRuntimeError::recovery("runner_target_post_command_drift")
            })?;
        Ok(DisposableRunnerTransactionOutcome::CommandCompleted(
            receipt,
        ))
    }

    fn publish_runner_catalog(
        &mut self,
        document: &crate::disposable_attempt_catalog::DisposableAttemptCatalogDocument,
        stage_code: &'static str,
        publish_code: &'static str,
    ) -> Result<(), DisposableRunnerRuntimeError> {
        let mut staged = self
            .stage_catalog(document)
            .map_err(|_| DisposableRunnerRuntimeError::durable(stage_code))?;
        self.publish_named_staged(
            &mut staged,
            super::disposable_attempt_catalog::CATALOG_DOCUMENT,
            false,
        )
        .map_err(|_| DisposableRunnerRuntimeError::recovery(publish_code))
    }
}
