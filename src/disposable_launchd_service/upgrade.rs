//! Journaled canary switching through the existing exact-plan service adapter.
//! The caller owns a fresh journal and an exclusive scheduler drain for the entire attempt.

use std::time::Duration;

use sha2::{Digest as _, Sha256};

use super::{
    DisposableLaunchdServiceDesiredState, DisposableLaunchdServicePlan,
    plan_disposable_launchd_service,
};
use crate::artifact::Sha256Digest;
use crate::durable_journal::{DurableExecutionError, JournalCheckpoint, execute_plan_durably};
use crate::journal::{
    ActionFailure, ActionReceipt, ExecutionJournal, ExecutionLane, MutationExecutor,
    PlannedMutation, Preconditions, RollbackClass,
};

/// Exact forward and recovery plans. Previous inputs must remain available throughout the attempt.
#[derive(Debug)]
pub struct CanaryUpgradePlan {
    previous: DisposableLaunchdServicePlan,
    candidate: DisposableLaunchdServicePlan,
    remove_previous: DisposableLaunchdServicePlan,
    remove_candidate: DisposableLaunchdServicePlan,
    identity: Sha256Digest,
    health_profile: Sha256Digest,
    health_timeout: Duration,
}

#[derive(Debug)]
pub enum CanaryUpgradeError {
    InvalidPlan,
    ApprovalMismatch,
    PreflightRefused,
    Journal(DurableExecutionError),
}

impl CanaryUpgradePlan {
    /// Bind retained/candidate service plans, health scope and bounded timeout into one approval.
    ///
    /// # Errors
    /// Rejects mismatched services, shared mutable inputs, no-op candidates and invalid timeouts.
    pub fn new(
        previous: DisposableLaunchdServicePlan,
        candidate: DisposableLaunchdServicePlan,
        health_profile: Sha256Digest,
        health_timeout: Duration,
    ) -> Result<Self, CanaryUpgradeError> {
        if previous.report.desired_state != DisposableLaunchdServiceDesiredState::Installed
            || candidate.report.desired_state != DisposableLaunchdServiceDesiredState::Installed
            || previous.operator_uid != candidate.operator_uid
            || previous.launch_agent != candidate.launch_agent
            || previous.program == candidate.program
            || previous.enrollment == candidate.enrollment
            || previous.program == candidate.enrollment
            || previous.enrollment == candidate.program
            || previous.program_digest == candidate.program_digest
            || health_timeout.is_zero()
            || health_timeout > Duration::from_secs(300)
            || !health_timeout.subsec_nanos().is_multiple_of(1_000_000)
        {
            return Err(CanaryUpgradeError::InvalidPlan);
        }
        let inverse = |plan: &DisposableLaunchdServicePlan| {
            let home = plan
                .launch_agent
                .parent()
                .and_then(|p| p.parent())
                .and_then(|p| p.parent())
                .ok_or(CanaryUpgradeError::InvalidPlan)?;
            plan_disposable_launchd_service(
                DisposableLaunchdServiceDesiredState::Removed,
                plan.operator_uid,
                home,
                &plan.program,
                &plan.program_digest,
                &plan.enrollment,
                &plan.enrollment_digest,
            )
            .map_err(|_| CanaryUpgradeError::InvalidPlan)
        };
        let remove_previous = inverse(&previous)?;
        let remove_candidate = inverse(&candidate)?;
        let bytes = serde_json::to_vec(&(
            "glaeda-canary-service-upgrade/v1",
            previous.report.plan_identity(),
            candidate.report.plan_identity(),
            &health_profile,
            health_timeout.as_millis(),
        ))
        .map_err(|_| CanaryUpgradeError::InvalidPlan)?;
        let identity = Sha256Digest::parse(&format!("sha256:{:x}", Sha256::digest(bytes)))
            .map_err(|_| CanaryUpgradeError::InvalidPlan)?;
        Ok(Self {
            previous,
            candidate,
            remove_previous,
            remove_candidate,
            identity,
            health_profile,
            health_timeout,
        })
    }

    #[must_use]
    pub fn identity(&self) -> &Sha256Digest {
        &self.identity
    }
    #[must_use]
    pub fn previous(&self) -> &DisposableLaunchdServicePlan {
        &self.previous
    }
    #[must_use]
    pub fn candidate(&self) -> &DisposableLaunchdServicePlan {
        &self.candidate
    }
    #[must_use]
    pub fn health_profile(&self) -> &Sha256Digest {
        &self.health_profile
    }

    #[must_use]
    pub fn actions(&self) -> Vec<PlannedMutation> {
        [
            ("stop_previous", "Stop the exact previous service"),
            ("start_candidate", "Start the verified candidate service"),
            (
                "check_candidate",
                "Check candidate health and canary result",
            ),
        ]
        .into_iter()
        .map(|(id, summary)| {
            PlannedMutation::new(
                id,
                ExecutionLane::Operator,
                summary,
                RollbackClass::Compensating,
                Preconditions::new([self.identity.as_str()]),
            )
        })
        .collect()
    }
}

/// Fresh result of the reviewed controller-health/canary profile, not a process-start observation.
pub struct UpgradeHealthEvidence {
    pub service_plan: Sha256Digest,
    pub health_profile: Sha256Digest,
    pub healthy: bool,
}

/// Host boundary. Implementations retain exclusive admission ownership through recovery.
/// Preflight verifies exact predecessor, compatibility, both saved generations, and no recovery
/// debt. Before-change reobserves zero active/reserved work and external mutations, and owner holds.
/// Health must honor its timeout and return only the result of the bound reviewed profile.
pub trait CanaryUpgradeHost {
    fn preflight(&mut self, plan: &CanaryUpgradePlan) -> Result<(), ActionFailure>;
    fn before_change(&mut self, plan: &CanaryUpgradePlan) -> Result<(), ActionFailure>;
    fn apply(&mut self, plan: &DisposableLaunchdServicePlan) -> Result<(), ActionFailure>;
    fn health(
        &mut self,
        plan: &DisposableLaunchdServicePlan,
        profile: &Sha256Digest,
        timeout: Duration,
    ) -> Result<UpgradeHealthEvidence, ActionFailure>;
}

/// Execute once under caller-held exclusive admission ownership and a fresh durable journal.
/// Failed health compensates by removing the candidate and restoring/checking the previous service.
/// Ambiguous effects or failed checkpoints require observation-based recovery of the returned
/// journal; never blindly restart this function or retry a quarantined candidate.
///
/// # Errors
/// Refuses changed approval or failed preflight before mutation. Persistence failures stop effects.
pub fn execute_canary_upgrade(
    plan: &CanaryUpgradePlan,
    approval: &Sha256Digest,
    host: &mut impl CanaryUpgradeHost,
    checkpoint: &mut impl JournalCheckpoint,
) -> Result<ExecutionJournal, CanaryUpgradeError> {
    if approval != plan.identity() {
        return Err(CanaryUpgradeError::ApprovalMismatch);
    }
    host.preflight(plan)
        .map_err(|_| CanaryUpgradeError::PreflightRefused)?;
    let mut executor = UpgradeExecutor {
        plan,
        host,
        candidate_removed: false,
    };
    execute_plan_durably(plan.actions(), &mut executor, checkpoint, false)
        .map_err(CanaryUpgradeError::Journal)
}

struct UpgradeExecutor<'a, H> {
    plan: &'a CanaryUpgradePlan,
    host: &'a mut H,
    candidate_removed: bool,
}

impl<H: CanaryUpgradeHost> UpgradeExecutor<'_, H> {
    fn check(&mut self, candidate: bool) -> Result<ActionReceipt, ActionFailure> {
        let service = if candidate {
            &self.plan.candidate
        } else {
            &self.plan.previous
        };
        let start = std::time::Instant::now();
        let evidence =
            self.host
                .health(service, &self.plan.health_profile, self.plan.health_timeout)?;
        if start.elapsed() > self.plan.health_timeout
            || !evidence.healthy
            || evidence.service_plan != *service.report.plan_identity()
            || evidence.health_profile != self.plan.health_profile
        {
            return Err(ActionFailure::public(
                "upgrade_health_failed",
                "bound health/canary check failed",
            ));
        }
        Ok(ActionReceipt::public("bound health/canary check passed"))
    }
}

impl<H: CanaryUpgradeHost> MutationExecutor for UpgradeExecutor<'_, H> {
    fn execute(&mut self, action: &PlannedMutation) -> Result<ActionReceipt, ActionFailure> {
        match action.id.as_str() {
            "stop_previous" => {
                self.host.before_change(self.plan)?;
                self.host.apply(&self.plan.remove_previous)?;
            }
            "start_candidate" => {
                self.host.before_change(self.plan)?;
                self.host.apply(&self.plan.candidate)?;
            }
            "check_candidate" => return self.check(true),
            _ => {
                return Err(ActionFailure::public(
                    "upgrade_action_invalid",
                    "unknown upgrade action",
                ));
            }
        }
        Ok(ActionReceipt::public("exact service plan applied"))
    }

    fn rollback(
        &mut self,
        action: &PlannedMutation,
        _: &ActionReceipt,
    ) -> Result<ActionReceipt, ActionFailure> {
        self.host.before_change(self.plan)?;
        match action.id.as_str() {
            "start_candidate" => {
                self.host.apply(&self.plan.remove_candidate)?;
                self.candidate_removed = true;
            }
            "stop_previous" => {
                // A failed candidate install may have partially started it. Reconcile/removal
                // must succeed before attempting to restore the previous configuration.
                if !self.candidate_removed {
                    self.host.apply(&self.plan.remove_candidate)?;
                }
                self.host.apply(&self.plan.previous)?;
                return self.check(false);
            }
            _ => {
                return Err(ActionFailure::public(
                    "upgrade_rollback_invalid",
                    "unknown rollback action",
                ));
            }
        }
        Ok(ActionReceipt::public("candidate service removed"))
    }
}

/// Native macOS service application uses the existing exact-plan approval and ownership checks.
/// Callers supply the scheduler/health observer through `CanaryUpgradeHost`; this helper is its
/// apply implementation, independent of the candidate executable.
///
/// # Errors
/// Returns a fixed public failure when the existing service adapter refuses or cannot settle.
#[cfg(target_os = "macos")]
pub fn apply_upgrade_service_plan(
    plan: &DisposableLaunchdServicePlan,
    executor: &impl crate::process::TimedCommandExecutor,
) -> Result<(), ActionFailure> {
    super::apply_disposable_launchd_service(plan, plan.report.plan_identity(), executor)
        .map(|_| ())
        .map_err(|_| {
            ActionFailure::public(
                "upgrade_service_unsettled",
                "exact service change did not settle",
            )
        })
}
