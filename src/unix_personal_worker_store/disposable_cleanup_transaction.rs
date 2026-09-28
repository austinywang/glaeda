//! Canonical-lock reconciliation for terminal disposable worker cleanup.
//!
//! Each invocation begins from current durable state and performs at most one external mutation.
//! VM and runner deletion are observation-first and idempotent: an ambiguous result leaves the
//! cleanup phase unchanged, so the next tick observes exact ownership or absence before retrying.

use super::*;

use crate::disposable_attempt_catalog::{
    DisposableAttemptCatalogAction, DisposableAttemptCatalogDocument,
};
use crate::disposable_clone_runtime::{
    CloneRuntimeClock, DisposableCleanupRunnerSource, DisposableCleanupTransactionOutcome,
    DisposableCleanupVmObservation, DisposableCloneRuntime, DisposableCloneRuntimeError,
};
use crate::disposable_worker_reconciler::{
    DisposableAttemptId, DisposableAttemptPhase, DisposableVmObservation, DisposableWorkerAction,
    DisposableWorkerReconcileInput, ScaleSetRunnerObservation, reconcile_attempt,
};
use crate::github_scale_set_bridge::ScaleSetRunnerLookup;
use crate::process::TimedCommandExecutor;

impl UnixPersonalWorkerStore {
    pub(crate) fn execute_disposable_cleanup_transaction(
        &mut self,
        runtime: &DisposableCloneRuntime,
        attempt_id: &DisposableAttemptId,
        runner_source: &mut impl DisposableCleanupRunnerSource,
        executor: &impl TimedCommandExecutor,
        clock: &impl CloneRuntimeClock,
    ) -> Result<DisposableCleanupTransactionOutcome, DisposableCloneRuntimeError> {
        let _lock = self
            .acquire_mutation_lock()
            .map_err(|_| DisposableCloneRuntimeError::durable("cleanup_store_lock_unavailable"))?;
        synchronize_directory(&self.directory, "personal worker store directory")
            .map_err(|_| DisposableCloneRuntimeError::durable("cleanup_store_sync_failed"))?;
        super::disposable_template_generation::refuse_unsettled(self).map_err(|_| {
            DisposableCloneRuntimeError::recovery("cleanup_template_recovery_required")
        })?;
        super::scale_set_delivery_recovery::refuse_unsettled(self).map_err(|_| {
            DisposableCloneRuntimeError::recovery("cleanup_delivery_recovery_required")
        })?;
        super::lima_authority::refuse_unsettled_lima_authority(self)
            .map_err(|_| DisposableCloneRuntimeError::recovery("cleanup_lima_recovery_required"))?;
        self.refuse_unsettled_personal_worker_state().map_err(|_| {
            DisposableCloneRuntimeError::recovery("cleanup_worker_recovery_required")
        })?;
        self.recover_catalog_locked().map_err(|_| {
            DisposableCloneRuntimeError::recovery("cleanup_catalog_recovery_failed")
        })?;

        let current = self
            .load_catalog_named(super::disposable_attempt_catalog::CATALOG_DOCUMENT)
            .map_err(|_| DisposableCloneRuntimeError::durable("cleanup_catalog_unavailable"))?
            .ok_or_else(|| DisposableCloneRuntimeError::durable("cleanup_catalog_missing"))?;
        let reservation = current
            .find_active(attempt_id)
            .ok_or_else(|| DisposableCloneRuntimeError::durable("cleanup_attempt_missing"))?;
        let phase = reservation.attempt().phase();
        if phase == DisposableAttemptPhase::UnprovisionedReleasing {
            return self.publish_cleanup_action(
                &current,
                attempt_id,
                DisposableAttemptCatalogAction::CompleteUnprovisioned,
            );
        }
        if phase == DisposableAttemptPhase::Complete {
            let retired = current
                .retire_complete(attempt_id, reservation.attempt().revision())
                .map_err(|_| DisposableCloneRuntimeError::recovery("cleanup_retirement_refused"))?;
            retired
                .validate_successor_of(&current)
                .map_err(|_| DisposableCloneRuntimeError::recovery("cleanup_retirement_invalid"))?;
            let mut staged = self.stage_catalog(&retired).map_err(|_| {
                DisposableCloneRuntimeError::durable("cleanup_retirement_stage_failed")
            })?;
            self.publish_named_staged(
                &mut staged,
                super::disposable_attempt_catalog::CATALOG_DOCUMENT,
                false,
            )
            .map_err(|_| {
                DisposableCloneRuntimeError::recovery("cleanup_retirement_publish_ambiguous")
            })?;
            return Ok(DisposableCleanupTransactionOutcome::AttemptRetired {
                attempt_id: attempt_id.as_str().to_owned(),
            });
        }
        if phase == DisposableAttemptPhase::Terminal {
            return self.publish_cleanup_action(
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
            return self.publish_cleanup_action(
                &current,
                attempt_id,
                DisposableAttemptCatalogAction::BeginCleanup,
            );
        }
        if !matches!(
            phase,
            DisposableAttemptPhase::Destroying
                | DisposableAttemptPhase::Deregistering
                | DisposableAttemptPhase::Releasing
        ) {
            return Err(DisposableCloneRuntimeError::recovery(
                "cleanup_phase_mismatch",
            ));
        }

        let now = clock
            .epoch_millis()
            .map_err(|_| DisposableCloneRuntimeError::observation("cleanup_clock_unavailable"))?;
        let vm = runtime.observe_cleanup_worker(reservation, executor, clock)?;
        let (vm_observation, vm_identity) = match &vm {
            DisposableCleanupVmObservation::Absent => (DisposableVmObservation::Absent, None),
            DisposableCleanupVmObservation::Present(_) => (
                DisposableVmObservation::Stopped,
                reservation.attempt().vm_identity(),
            ),
        };
        if let DisposableCleanupVmObservation::Present(confirmed) = &vm {
            let action = reconcile_attempt(DisposableWorkerReconcileInput {
                now,
                attempt: reservation.attempt(),
                vm: vm_observation,
                vm_identity,
                runner: ScaleSetRunnerObservation::Unknown,
                job_event: None,
                capacity_reserved: true,
                cancellation_requested: false,
            })
            .map_err(|_| DisposableCloneRuntimeError::recovery("cleanup_reconcile_failed"))?;
            if action != DisposableWorkerAction::DestroyVm {
                return Err(DisposableCloneRuntimeError::recovery(
                    "cleanup_destroy_not_authorized",
                ));
            }
            runtime.destroy_cleanup_worker(reservation, confirmed, executor, clock)?;
            return Ok(DisposableCleanupTransactionOutcome::VmDestroyed {
                attempt_id: attempt_id.as_str().to_owned(),
            });
        }

        let runner = if phase == DisposableAttemptPhase::Destroying {
            ScaleSetRunnerObservation::Unknown
        } else {
            match runner_source.observe_runner(reservation.attempt().runner_name())? {
                ScaleSetRunnerLookup::Absent => ScaleSetRunnerObservation::Absent,
                ScaleSetRunnerLookup::Present(runner) => {
                    ScaleSetRunnerObservation::RegistrationOnly { runner }
                }
            }
        };
        let action = reconcile_attempt(DisposableWorkerReconcileInput {
            now,
            attempt: reservation.attempt(),
            vm: DisposableVmObservation::Absent,
            vm_identity: None,
            runner,
            job_event: None,
            capacity_reserved: true,
            cancellation_requested: false,
        })
        .map_err(|_| DisposableCloneRuntimeError::recovery("cleanup_reconcile_failed"))?;
        match action {
            DisposableWorkerAction::Persist { transition } => {
                self.publish_cleanup_action(&current, attempt_id, transition)
            }
            DisposableWorkerAction::DeleteRunner { runner } => {
                runner_source.remove_runner(&runner)?;
                match runner_source.observe_runner(reservation.attempt().runner_name())? {
                    ScaleSetRunnerLookup::Absent => {
                        Ok(DisposableCleanupTransactionOutcome::RunnerDeleted {
                            attempt_id: attempt_id.as_str().to_owned(),
                        })
                    }
                    ScaleSetRunnerLookup::Present(_) => Err(DisposableCloneRuntimeError::recovery(
                        "cleanup_runner_delete_not_observed",
                    )),
                }
            }
            DisposableWorkerAction::ReleaseCapacity => self.publish_cleanup_action(
                &current,
                attempt_id,
                DisposableAttemptCatalogAction::AdvanceCleanup(DisposableAttemptPhase::Complete),
            ),
            _ => Err(DisposableCloneRuntimeError::recovery(
                "cleanup_action_not_executable",
            )),
        }
    }

    fn publish_cleanup_action(
        &mut self,
        current: &DisposableAttemptCatalogDocument,
        attempt_id: &DisposableAttemptId,
        action: DisposableAttemptCatalogAction,
    ) -> Result<DisposableCleanupTransactionOutcome, DisposableCloneRuntimeError> {
        let reservation = current
            .find_active(attempt_id)
            .ok_or_else(|| DisposableCloneRuntimeError::durable("cleanup_attempt_missing"))?;
        let next = current
            .replace_attempt(attempt_id, reservation.attempt().revision(), action)
            .map_err(|_| DisposableCloneRuntimeError::recovery("cleanup_checkpoint_refused"))?;
        next.validate_successor_of(current)
            .map_err(|_| DisposableCloneRuntimeError::recovery("cleanup_checkpoint_invalid"))?;
        let next_phase = next
            .find_active(attempt_id)
            .ok_or_else(|| DisposableCloneRuntimeError::durable("cleanup_attempt_missing"))?
            .attempt()
            .phase();
        let mut staged = self
            .stage_catalog(&next)
            .map_err(|_| DisposableCloneRuntimeError::durable("cleanup_checkpoint_stage_failed"))?;
        self.publish_named_staged(
            &mut staged,
            super::disposable_attempt_catalog::CATALOG_DOCUMENT,
            false,
        )
        .map_err(|_| {
            DisposableCloneRuntimeError::recovery("cleanup_checkpoint_publish_ambiguous")
        })?;
        if next_phase == DisposableAttemptPhase::Complete {
            Ok(DisposableCleanupTransactionOutcome::CapacityReleased {
                attempt_id: attempt_id.as_str().to_owned(),
            })
        } else {
            Ok(DisposableCleanupTransactionOutcome::CleanupCheckpointed {
                attempt_id: attempt_id.as_str().to_owned(),
                phase: next_phase,
            })
        }
    }
}
