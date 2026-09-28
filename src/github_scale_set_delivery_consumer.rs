#![allow(dead_code)]

use std::collections::BTreeSet;
use std::fmt;

use sha2::{Digest, Sha256};

use crate::disposable_attempt_catalog::{
    DisposableAttemptCatalogAction, DisposableAttemptCatalogDocument, DisposableAttemptReservation,
};
use crate::disposable_attempt_state::DisposableAttemptState;
use crate::disposable_lima_worker::DISPOSABLE_LIMA_VM_ID_HEX_CHARS;
use crate::disposable_prepared_template::{
    DisposablePreparedTemplateIdentity, DisposablePreparedTemplateManifest,
};
use crate::disposable_worker_reconciler::{
    CapacityClaimId, DisposableAttemptId, DisposableVmId, DisposableWorkerResources,
};
use crate::execution_admission::EpochMillis;
use crate::github_scale_set_delivery::{
    ScaleSetDelivery, ScaleSetDeliveryJobEvidence, ScaleSetDeliveryLifecycleEvent,
};
use crate::github_scale_set_protocol::ScaleSetRunnerName;

const ATTEMPT_IDENTITY_DOMAIN: &[u8] = b"smolrunner.scale-set-delivery-attempt.v1\0";
const MAX_REPOSITORY_COMPONENT_BYTES: usize = 100;
const MAX_LABELS: usize = 32;
const DISPOSABLE_JOB_MAX_MILLIS: u64 = 6 * 60 * 60 * 1_000;

/// Exact enrolled inputs that may turn one retained Scale Set delivery into catalog state.
///
/// This policy is crate-private and contains no credential or process authority. A later service
/// transaction will reconstruct it from the validated operator enrollment while holding the
/// canonical mutation lock.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ScaleSetDeliveryConsumerPolicy {
    scale_set_id: u32,
    repository: String,
    owner: String,
    request_labels: Vec<String>,
    resources: DisposableWorkerResources,
    prepared_template_identity: DisposablePreparedTemplateIdentity,
}

impl ScaleSetDeliveryConsumerPolicy {
    pub(crate) fn new(
        scale_set_id: u32,
        repository: &str,
        owner: &str,
        request_labels: &[String],
        resources: DisposableWorkerResources,
        prepared_template: &DisposablePreparedTemplateManifest,
    ) -> Result<Self, ScaleSetDeliveryConsumerError> {
        let source_cpu_millis = prepared_template
            .source_cpu_count()
            .checked_mul(1_000)
            .ok_or_else(invalid_policy)?;
        if resources.cpu_millis() < source_cpu_millis
            || resources.memory_bytes() < prepared_template.source_memory_bytes()
            || resources.disk_bytes() < prepared_template.source_disk_bytes()
        {
            return Err(invalid_policy());
        }
        Self::new_with_identity(
            scale_set_id,
            repository,
            owner,
            request_labels,
            resources,
            prepared_template.identity().map_err(|_| invalid_policy())?,
        )
    }

    pub(crate) fn new_owned_linux(
        scale_set_id: u32,
        repository: &str,
        owner: &str,
        request_labels: &[String],
        resources: DisposableWorkerResources,
        generation: DisposablePreparedTemplateIdentity,
    ) -> Result<Self, ScaleSetDeliveryConsumerError> {
        Self::new_with_identity(
            scale_set_id,
            repository,
            owner,
            request_labels,
            resources,
            generation,
        )
    }

    fn new_with_identity(
        scale_set_id: u32,
        repository: &str,
        owner: &str,
        request_labels: &[String],
        resources: DisposableWorkerResources,
        prepared_template_identity: DisposablePreparedTemplateIdentity,
    ) -> Result<Self, ScaleSetDeliveryConsumerError> {
        if scale_set_id == 0
            || !bounded_token(repository, MAX_REPOSITORY_COMPONENT_BYTES)
            || !bounded_token(owner, MAX_REPOSITORY_COMPONENT_BYTES)
            || request_labels.is_empty()
            || request_labels.len() > MAX_LABELS
        {
            return Err(invalid_policy());
        }

        let mut labels = request_labels.to_vec();
        labels.sort();
        if labels
            .iter()
            .any(|label| !bounded_token(label, MAX_REPOSITORY_COMPONENT_BYTES))
            || labels.windows(2).any(|pair| pair[0] == pair[1])
        {
            return Err(invalid_policy());
        }

        Ok(Self {
            scale_set_id,
            repository: repository.to_owned(),
            owner: owner.to_owned(),
            request_labels: labels,
            resources,
            prepared_template_identity,
        })
    }
}

impl fmt::Debug for ScaleSetDeliveryConsumerPolicy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScaleSetDeliveryConsumerPolicy")
            .field("scale_set_id", &self.scale_set_id)
            .field("repository", &"<enrolled>")
            .field("label_count", &self.request_labels.len())
            .field("resources", &self.resources)
            .field("prepared_template_identity", &"<private>")
            .finish()
    }
}

/// Apply every retained event in order to a private catalog snapshot.
///
/// This function performs no persistence or external action. Returning an error leaves the caller's
/// catalog unchanged. A later paired transaction is responsible for durably publishing the exact
/// returned successor before acknowledgement or acquisition.
pub(crate) fn reconcile_scale_set_delivery(
    policy: &ScaleSetDeliveryConsumerPolicy,
    delivery: &ScaleSetDelivery,
    catalog: &DisposableAttemptCatalogDocument,
    observed_at: EpochMillis,
) -> Result<DisposableAttemptCatalogDocument, ScaleSetDeliveryConsumerError> {
    let events = delivery
        .retained_events()
        .map_err(|_| consumer_error("delivery_consumer_evidence_invalid"))?;
    events
        .into_iter()
        .try_fold(catalog.clone(), |current, event| {
            reconcile_event(policy, &event, &current, observed_at)
        })
}

/// Reconcile one zero-capacity lifecycle delivery that resolves an earlier ambiguous acquisition.
///
/// The earlier Available delivery has already reserved local capacity but its acknowledgement
/// response was lost. A later exact Assigned event is positive service evidence that GitHub
/// acquired that request, while the documented runnerless Completed(canceled) shape proves that
/// the acquired assignment ended before any runner accepted it. The latter is released through the
/// unprovisioned path and never gains VM-cleanup authority. Started or runner-bearing completion
/// cannot legitimately precede SmolRunner's durable clone and runner bindings and therefore fail
/// closed.
pub(crate) fn reconcile_scale_set_acquisition_resolution(
    policy: &ScaleSetDeliveryConsumerPolicy,
    ambiguous_requests: &[crate::github_scale_set_protocol::ScaleSetRunnerRequestId],
    delivery: &ScaleSetDelivery,
    catalog: &DisposableAttemptCatalogDocument,
) -> Result<
    (
        DisposableAttemptCatalogDocument,
        Vec<crate::github_scale_set_protocol::ScaleSetRunnerRequestId>,
    ),
    ScaleSetDeliveryConsumerError,
> {
    if ambiguous_requests.is_empty()
        || !delivery
            .available_request_ids()
            .map_err(|_| consumer_error("delivery_consumer_evidence_invalid"))?
            .is_empty()
    {
        return Err(consumer_error("delivery_consumer_resolution_invalid"));
    }
    let ambiguous = ambiguous_requests.iter().copied().collect::<BTreeSet<_>>();
    if ambiguous.len() != ambiguous_requests.len() {
        return Err(consumer_error("delivery_consumer_resolution_invalid"));
    }

    let mut next = catalog.clone();
    let mut acquired = BTreeSet::new();
    for event in delivery
        .retained_events()
        .map_err(|_| consumer_error("delivery_consumer_evidence_invalid"))?
    {
        let job = event_job(&event);
        if !ambiguous.contains(&job.runner_request_id) {
            return Err(consumer_error("delivery_consumer_resolution_conflict"));
        }

        validate_job(policy, job)?;
        let identities = derive_identities(policy, job)?;
        let reservation = next
            .find_active_by_runner_request_id(job.runner_request_id)
            .ok_or_else(|| consumer_error("delivery_consumer_attempt_missing"))?;
        validate_reservation(policy, reservation, job, &identities)?;
        if reservation.attempt().phase()
            != crate::disposable_worker_reconciler::DisposableAttemptPhase::Reserved
        {
            return Err(consumer_error("delivery_consumer_resolution_conflict"));
        }

        match &event {
            ScaleSetDeliveryLifecycleEvent::Assigned { .. } => {
                acquired.insert(job.runner_request_id);
            }
            ScaleSetDeliveryLifecycleEvent::Completed {
                runner: None,
                result,
                ..
            } if result.as_str() == "canceled" => {
                acquired.insert(job.runner_request_id);
                next = retire_unprovisioned_cancellation(&next, reservation)
                    .map_err(|_| consumer_error("delivery_consumer_resolution_conflict"))?;
            }
            ScaleSetDeliveryLifecycleEvent::Available { .. }
            | ScaleSetDeliveryLifecycleEvent::Started { .. }
            | ScaleSetDeliveryLifecycleEvent::Completed { .. } => {
                return Err(consumer_error("delivery_consumer_resolution_conflict"));
            }
        }
    }
    if acquired.is_empty() {
        return Err(consumer_error("delivery_consumer_resolution_inconclusive"));
    }
    Ok((next, acquired.into_iter().collect()))
}

fn reconcile_event(
    policy: &ScaleSetDeliveryConsumerPolicy,
    event: &ScaleSetDeliveryLifecycleEvent,
    catalog: &DisposableAttemptCatalogDocument,
    observed_at: EpochMillis,
) -> Result<DisposableAttemptCatalogDocument, ScaleSetDeliveryConsumerError> {
    let job = event_job(event);
    validate_job(policy, job)?;
    let identities = derive_identities(policy, job)?;

    if matches!(
        event,
        ScaleSetDeliveryLifecycleEvent::Available { .. }
            | ScaleSetDeliveryLifecycleEvent::Assigned { .. }
    ) {
        let available = matches!(event, ScaleSetDeliveryLifecycleEvent::Available { .. });
        let not_after = attempt_not_after(observed_at)?;
        if let Some(existing) = catalog.find_active_by_runner_request_id(job.runner_request_id) {
            validate_reservation(policy, existing, job, &identities)?;
            if available
                || (existing.attempt().phase()
                    == crate::disposable_worker_reconciler::DisposableAttemptPhase::Reserved
                    && existing.attempt().github_job_id().is_none())
            {
                if existing.attempt().not_after() != not_after {
                    return Err(consumer_error("delivery_consumer_identity_drift"));
                }
                return Ok(catalog.clone());
            }
            return catalog
                .replace_attempt(
                    existing.attempt().attempt_id(),
                    existing.attempt().revision(),
                    event_action(event)?,
                )
                .map_err(|_| consumer_error("delivery_consumer_event_conflict"));
        }
        if let Some(existing) = catalog.find_tombstone_by_runner_request_id(job.runner_request_id) {
            validate_attempt(existing, job, &identities)?;
            if available && existing.not_after() != not_after {
                return Err(consumer_error("delivery_consumer_identity_drift"));
            }
            validate_tombstone_event(existing, event)?;
            return Ok(catalog.clone());
        }
        if catalog
            .host_usage()
            .map_err(|_| consumer_error("delivery_consumer_catalog_invalid"))?
            .workers()
            != 0
        {
            return Err(consumer_error("delivery_consumer_capacity_unavailable"));
        }
        let reservation = DisposableAttemptReservation::new(
            DisposableAttemptState::reserved(
                identities.attempt_id,
                identities.claim_id,
                identities.vm_id,
                identities.runner_name,
                job.runner_request_id,
                not_after,
            ),
            policy.resources,
            policy.prepared_template_identity.clone(),
        )
        .map_err(|_| consumer_error("delivery_consumer_reservation_invalid"))?;
        return catalog
            .reserve(reservation)
            .map_err(|_| consumer_error("delivery_consumer_reservation_failed"));
    }

    if let Some(existing) = catalog.find_active_by_runner_request_id(job.runner_request_id) {
        validate_reservation(policy, existing, job, &identities)?;
        if is_unbound_runnerless_cancellation(event, existing.attempt()) {
            use crate::disposable_worker_reconciler::DisposableAttemptPhase;
            return match existing.attempt().phase() {
                DisposableAttemptPhase::Reserved | DisposableAttemptPhase::CloneAuthorized => {
                    retire_unprovisioned_cancellation(catalog, existing)
                }
                DisposableAttemptPhase::CloneStarted
                    if existing.attempt().vm_identity().is_none() =>
                {
                    // A crash after the durable clone-start checkpoint can leave the Lima
                    // command outcome ambiguous. The cancellation clears the upstream job, but
                    // it cannot manufacture VM ownership or deletion authority. Settle the
                    // delivery while preserving the exact unbound recovery debt.
                    Ok(catalog.clone())
                }
                DisposableAttemptPhase::CloneStarted
                | DisposableAttemptPhase::Registering
                | DisposableAttemptPhase::Waiting => catalog
                    .replace_attempt(
                        existing.attempt().attempt_id(),
                        existing.attempt().revision(),
                        DisposableAttemptCatalogAction::BeginCleanup,
                    )
                    .map_err(|_| consumer_error("delivery_consumer_event_conflict")),
                DisposableAttemptPhase::Destroying
                | DisposableAttemptPhase::Deregistering
                | DisposableAttemptPhase::Releasing
                | DisposableAttemptPhase::UnprovisionedReleasing
                | DisposableAttemptPhase::Complete => Ok(catalog.clone()),
                DisposableAttemptPhase::Provisioning
                | DisposableAttemptPhase::Assigned
                | DisposableAttemptPhase::Running
                | DisposableAttemptPhase::Terminal => {
                    Err(consumer_error("delivery_consumer_event_conflict"))
                }
            };
        }
        return catalog
            .replace_attempt(
                existing.attempt().attempt_id(),
                existing.attempt().revision(),
                event_action(event)?,
            )
            .map_err(|_| consumer_error("delivery_consumer_event_conflict"));
    }
    if let Some(existing) = catalog.find_tombstone_by_runner_request_id(job.runner_request_id) {
        validate_attempt(existing, job, &identities)?;
        validate_tombstone_event(existing, event)?;
        return Ok(catalog.clone());
    }
    Err(consumer_error("delivery_consumer_attempt_missing"))
}

fn is_unbound_runnerless_cancellation(
    event: &ScaleSetDeliveryLifecycleEvent,
    attempt: &DisposableAttemptState,
) -> bool {
    attempt.github_job_id().is_none()
        && matches!(
            event,
            ScaleSetDeliveryLifecycleEvent::Completed {
                runner: None,
                result,
                ..
            } if result.as_str() == "canceled"
        )
}

fn retire_unprovisioned_cancellation(
    catalog: &DisposableAttemptCatalogDocument,
    reservation: &DisposableAttemptReservation,
) -> Result<DisposableAttemptCatalogDocument, ScaleSetDeliveryConsumerError> {
    let attempt_id = reservation.attempt().attempt_id().clone();
    let releasing = catalog
        .replace_attempt(
            &attempt_id,
            reservation.attempt().revision(),
            DisposableAttemptCatalogAction::BeginUnprovisionedRelease,
        )
        .map_err(|_| consumer_error("delivery_consumer_event_conflict"))?;
    let releasing_attempt = releasing
        .active()
        .iter()
        .find(|candidate| candidate.attempt().attempt_id() == &attempt_id)
        .ok_or_else(|| consumer_error("delivery_consumer_attempt_missing"))?;
    let complete = releasing
        .replace_attempt(
            &attempt_id,
            releasing_attempt.attempt().revision(),
            DisposableAttemptCatalogAction::CompleteUnprovisioned,
        )
        .map_err(|_| consumer_error("delivery_consumer_event_conflict"))?;
    let complete_attempt = complete
        .active()
        .iter()
        .find(|candidate| candidate.attempt().attempt_id() == &attempt_id)
        .ok_or_else(|| consumer_error("delivery_consumer_attempt_missing"))?;
    complete
        .retire_complete(&attempt_id, complete_attempt.attempt().revision())
        .map_err(|_| consumer_error("delivery_consumer_event_conflict"))
}

fn attempt_not_after(
    observed_at: EpochMillis,
) -> Result<EpochMillis, ScaleSetDeliveryConsumerError> {
    observed_at
        .get()
        .checked_add(DISPOSABLE_JOB_MAX_MILLIS)
        .and_then(|value| EpochMillis::new(value).ok())
        .ok_or_else(|| consumer_error("delivery_consumer_deadline_invalid"))
}

struct DerivedIdentities {
    attempt_id: DisposableAttemptId,
    claim_id: CapacityClaimId,
    vm_id: DisposableVmId,
    runner_name: ScaleSetRunnerName,
}

fn derive_identities(
    policy: &ScaleSetDeliveryConsumerPolicy,
    job: &ScaleSetDeliveryJobEvidence,
) -> Result<DerivedIdentities, ScaleSetDeliveryConsumerError> {
    let mut labels = job.request_labels.clone();
    labels.sort();
    let mut hasher = Sha256::new();
    hasher.update(ATTEMPT_IDENTITY_DOMAIN);
    hash_field(&mut hasher, &policy.scale_set_id.to_be_bytes());
    hash_field(&mut hasher, &policy.resources.cpu_millis().to_be_bytes());
    hash_field(&mut hasher, &policy.resources.memory_bytes().to_be_bytes());
    hash_field(&mut hasher, &policy.resources.disk_bytes().to_be_bytes());
    hash_field(
        &mut hasher,
        policy.prepared_template_identity.as_str().as_bytes(),
    );
    hash_field(&mut hasher, &job.runner_request_id.get().to_be_bytes());
    hash_field(&mut hasher, job.repository.as_bytes());
    hash_field(&mut hasher, job.owner.as_bytes());
    hash_field(&mut hasher, job.job_id.as_str().as_bytes());
    hash_field(&mut hasher, &job.workflow_run_id.to_be_bytes());
    for label in labels {
        hash_field(&mut hasher, label.as_bytes());
    }
    let digest = format!("{:x}", hasher.finalize());
    let short = &digest[..32];
    // Lima appends a private Unix socket beneath LIMA_HOME/<instance>. Keep only the shared 96-bit
    // VM suffix while the attempt retains 128 bits and the capacity claim retains the full digest.
    let vm_short = &digest[..DISPOSABLE_LIMA_VM_ID_HEX_CHARS];
    Ok(DerivedIdentities {
        attempt_id: DisposableAttemptId::parse(&format!("attempt-{short}"))
            .map_err(|_| consumer_error("delivery_consumer_identity_invalid"))?,
        claim_id: CapacityClaimId::parse(&format!("claim-{digest}"))
            .map_err(|_| consumer_error("delivery_consumer_identity_invalid"))?,
        vm_id: DisposableVmId::parse(&format!("w-{vm_short}"))
            .map_err(|_| consumer_error("delivery_consumer_identity_invalid"))?,
        runner_name: ScaleSetRunnerName::parse(&format!("smolrunner-{short}"))
            .map_err(|_| consumer_error("delivery_consumer_identity_invalid"))?,
    })
}

fn validate_job(
    policy: &ScaleSetDeliveryConsumerPolicy,
    job: &ScaleSetDeliveryJobEvidence,
) -> Result<(), ScaleSetDeliveryConsumerError> {
    let mut labels = job.request_labels.clone();
    labels.sort();
    if job.repository != policy.repository
        || job.owner != policy.owner
        || labels != policy.request_labels
    {
        return Err(consumer_error("delivery_consumer_job_not_enrolled"));
    }
    Ok(())
}

fn validate_reservation(
    policy: &ScaleSetDeliveryConsumerPolicy,
    reservation: &DisposableAttemptReservation,
    job: &ScaleSetDeliveryJobEvidence,
    identities: &DerivedIdentities,
) -> Result<(), ScaleSetDeliveryConsumerError> {
    validate_attempt(reservation.attempt(), job, identities)?;
    if reservation.resources() != policy.resources
        || reservation.prepared_template_identity() != &policy.prepared_template_identity
    {
        return Err(consumer_error("delivery_consumer_identity_drift"));
    }
    Ok(())
}

fn validate_attempt(
    attempt: &DisposableAttemptState,
    job: &ScaleSetDeliveryJobEvidence,
    identities: &DerivedIdentities,
) -> Result<(), ScaleSetDeliveryConsumerError> {
    if attempt.attempt_id() != &identities.attempt_id
        || attempt.capacity_claim_id() != &identities.claim_id
        || attempt.vm_id() != &identities.vm_id
        || attempt.runner_name() != &identities.runner_name
        || attempt.runner_request_id() != job.runner_request_id
        || attempt
            .github_job_id()
            .is_some_and(|job_id| job_id != &job.job_id)
    {
        return Err(consumer_error("delivery_consumer_identity_drift"));
    }
    Ok(())
}

fn event_job(event: &ScaleSetDeliveryLifecycleEvent) -> &ScaleSetDeliveryJobEvidence {
    match event {
        ScaleSetDeliveryLifecycleEvent::Available { job }
        | ScaleSetDeliveryLifecycleEvent::Assigned { job }
        | ScaleSetDeliveryLifecycleEvent::Started { job, .. }
        | ScaleSetDeliveryLifecycleEvent::Completed { job, .. } => job,
    }
}

fn event_action(
    event: &ScaleSetDeliveryLifecycleEvent,
) -> Result<DisposableAttemptCatalogAction, ScaleSetDeliveryConsumerError> {
    match event {
        ScaleSetDeliveryLifecycleEvent::Assigned { job } => Ok(
            DisposableAttemptCatalogAction::RecordAssigned(job.job_id.clone()),
        ),
        ScaleSetDeliveryLifecycleEvent::Started { job, runner } => {
            Ok(DisposableAttemptCatalogAction::RecordRunning {
                runner: runner.clone(),
                job_id: job.job_id.clone(),
            })
        }
        ScaleSetDeliveryLifecycleEvent::Completed {
            job,
            runner,
            result,
        } => Ok(DisposableAttemptCatalogAction::RecordTerminal {
            runner: runner.clone(),
            job_id: job.job_id.clone(),
            result: result.clone(),
        }),
        ScaleSetDeliveryLifecycleEvent::Available { .. } => {
            Err(consumer_error("delivery_consumer_event_invalid"))
        }
    }
}

fn validate_tombstone_event(
    attempt: &DisposableAttemptState,
    event: &ScaleSetDeliveryLifecycleEvent,
) -> Result<(), ScaleSetDeliveryConsumerError> {
    let valid = match event {
        ScaleSetDeliveryLifecycleEvent::Available { .. } => true,
        ScaleSetDeliveryLifecycleEvent::Assigned { job } => {
            attempt.github_job_id() == Some(&job.job_id)
        }
        ScaleSetDeliveryLifecycleEvent::Started { job, runner } => {
            attempt.github_job_id() == Some(&job.job_id)
                && attempt.runner_id() == Some(runner.id)
                && attempt.runner_name() == &runner.name
        }
        ScaleSetDeliveryLifecycleEvent::Completed {
            job,
            runner,
            result,
        } => {
            (attempt.github_job_id() == Some(&job.job_id)
                && attempt.result() == Some(result)
                && runner.as_ref().is_none_or(|runner| {
                    attempt.runner_id() == Some(runner.id) && attempt.runner_name() == &runner.name
                }))
                || (attempt.phase()
                    == crate::disposable_worker_reconciler::DisposableAttemptPhase::Complete
                    && attempt.github_job_id().is_none()
                    && attempt.result().is_none()
                    && runner.is_none()
                    && result.as_str() == "canceled")
        }
    };
    if valid {
        Ok(())
    } else {
        Err(consumer_error("delivery_consumer_event_conflict"))
    }
}

fn hash_field(hasher: &mut Sha256, value: &[u8]) {
    hasher.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
    hasher.update(value);
}

fn bounded_token(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScaleSetDeliveryConsumerError {
    code: &'static str,
}

impl ScaleSetDeliveryConsumerError {
    pub(crate) const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for ScaleSetDeliveryConsumerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScaleSetDeliveryConsumerError")
            .field("code", &self.code)
            .finish()
    }
}

impl fmt::Display for ScaleSetDeliveryConsumerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code)
    }
}

impl std::error::Error for ScaleSetDeliveryConsumerError {}

const fn consumer_error(code: &'static str) -> ScaleSetDeliveryConsumerError {
    ScaleSetDeliveryConsumerError { code }
}

const fn invalid_policy() -> ScaleSetDeliveryConsumerError {
    consumer_error("delivery_consumer_policy_invalid")
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::disposable_prepared_template::current_disposable_prepared_template;
    use crate::disposable_worker_reconciler::{DisposableAttemptPhase, DisposableVmIdentity};
    use crate::github_scale_set_bridge::{
        ScaleSetBridgeEvent, ScaleSetBridgeJobEvidence, ScaleSetBridgePoll, ScaleSetStatistics,
    };
    use crate::github_scale_set_protocol::{
        ScaleSetJobId, ScaleSetJobResult, ScaleSetRunnerId, ScaleSetRunnerReference,
        ScaleSetRunnerRequestId,
    };

    fn policy() -> ScaleSetDeliveryConsumerPolicy {
        ScaleSetDeliveryConsumerPolicy::new(
            23,
            "project",
            "example",
            &["smolrunner".to_owned()],
            DisposableWorkerResources::new(2_000, 2 << 30, 20 << 30).unwrap(),
            &current_disposable_prepared_template().unwrap(),
        )
        .unwrap()
    }

    fn job(request_id: u64, job_id: &str) -> ScaleSetBridgeJobEvidence {
        ScaleSetBridgeJobEvidence {
            runner_request_id: request_id,
            repository: "project".to_owned(),
            owner: "example".to_owned(),
            job_id: ScaleSetJobId::parse(job_id).unwrap(),
            workflow_run_id: 99,
            request_labels: vec!["smolrunner".to_owned()],
        }
    }

    fn delivery(events: Vec<ScaleSetBridgeEvent>) -> ScaleSetDelivery {
        ScaleSetDelivery::from_bridge_poll(&ScaleSetBridgePoll::Message {
            message_id: 7,
            statistics: ScaleSetStatistics {
                available_jobs: 1,
                acquired_jobs: 0,
                assigned_jobs: 0,
                running_jobs: 0,
                registered_runners: 0,
                busy_runners: 0,
                idle_runners: 0,
            },
            events,
        })
        .unwrap()
        .unwrap()
    }

    fn observed_at() -> EpochMillis {
        EpochMillis::new(100_000).unwrap()
    }

    fn reserve() -> DisposableAttemptCatalogDocument {
        reconcile_scale_set_delivery(
            &policy(),
            &delivery(vec![ScaleSetBridgeEvent::Available(job(41, "job-1"))]),
            &DisposableAttemptCatalogDocument::empty(),
            observed_at(),
        )
        .unwrap()
    }

    fn checkpoint_runner_start(
        mut catalog: DisposableAttemptCatalogDocument,
        attempt_id: &DisposableAttemptId,
        runner: &ScaleSetRunnerReference,
    ) -> DisposableAttemptCatalogDocument {
        for action in [
            DisposableAttemptCatalogAction::RecordJitGenerationStarted,
            DisposableAttemptCatalogAction::RecordRegistration(runner.clone()),
            DisposableAttemptCatalogAction::RecordRunnerStartStarted,
        ] {
            let revision = catalog
                .find_active(attempt_id)
                .unwrap()
                .attempt()
                .revision();
            catalog = catalog
                .replace_attempt(attempt_id, revision, action)
                .unwrap();
        }
        catalog
    }

    #[test]
    fn available_delivery_reserves_once_with_exact_request_and_deadline() {
        let delivery = delivery(vec![ScaleSetBridgeEvent::Available(job(41, "job-1"))]);
        let reserved = reconcile_scale_set_delivery(
            &policy(),
            &delivery,
            &DisposableAttemptCatalogDocument::empty(),
            observed_at(),
        )
        .unwrap();
        assert_eq!(reserved.active().len(), 1);
        assert_eq!(reserved.active()[0].attempt().runner_request_id().get(), 41);
        let vm_id = reserved.active()[0].attempt().vm_id().as_str();
        assert_eq!(vm_id.len(), 2 + DISPOSABLE_LIMA_VM_ID_HEX_CHARS);
        assert!(vm_id.starts_with("w-"));
        assert!(vm_id[2..].bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_eq!(reserved.active()[0].attempt().not_after().get(), 21_700_000);
        assert_eq!(
            reconcile_scale_set_delivery(&policy(), &delivery, &reserved, observed_at()).unwrap(),
            reserved
        );
        assert_eq!(
            reconcile_scale_set_delivery(
                &policy(),
                &delivery,
                &reserved,
                EpochMillis::new(100_001).unwrap(),
            )
            .unwrap_err()
            .code(),
            "delivery_consumer_identity_drift"
        );
    }

    #[test]
    fn later_assignment_resolves_ambiguous_acquisition_without_premature_worker_authority() {
        let reserved = reserve();
        let request = ScaleSetRunnerRequestId::new(41).unwrap();
        let assigned = delivery(vec![ScaleSetBridgeEvent::Assigned(job(41, "job-1"))]);
        let (resolved, acquired) =
            reconcile_scale_set_acquisition_resolution(&policy(), &[request], &assigned, &reserved)
                .unwrap();

        assert_eq!(resolved, reserved);
        assert_eq!(acquired, [request]);
        assert_eq!(
            resolved.active()[0].attempt().phase(),
            DisposableAttemptPhase::Reserved
        );
        assert!(resolved.active()[0].attempt().github_job_id().is_none());
    }

    #[test]
    fn runnerless_cancellation_resolves_and_retires_without_vm_cleanup_authority() {
        let reserved = reserve();
        let request = ScaleSetRunnerRequestId::new(41).unwrap();
        let canceled = delivery(vec![ScaleSetBridgeEvent::Completed {
            job: job(41, "job-1"),
            runner: None,
            result: ScaleSetJobResult::parse("canceled").unwrap(),
        }]);
        let (resolved, acquired) =
            reconcile_scale_set_acquisition_resolution(&policy(), &[request], &canceled, &reserved)
                .unwrap();

        assert!(resolved.active().is_empty());
        let tombstone = resolved
            .find_tombstone_by_runner_request_id(request)
            .expect("canceled request must enter bounded replay history");
        assert_eq!(tombstone.phase(), DisposableAttemptPhase::Complete);
        assert!(tombstone.vm_identity().is_none());
        assert!(tombstone.github_job_id().is_none());
        assert_eq!(acquired, [request]);
    }

    #[test]
    fn preclone_started_or_runner_bearing_completion_cannot_resolve_acquisition() {
        let reserved = reserve();
        let request = ScaleSetRunnerRequestId::new(41).unwrap();
        let runner = ScaleSetRunnerReference::new(
            ScaleSetRunnerId::new(501).unwrap(),
            reserved.active()[0].attempt().runner_name().clone(),
        );
        for event in [
            ScaleSetBridgeEvent::Started {
                job: job(41, "job-1"),
                runner: runner.clone(),
            },
            ScaleSetBridgeEvent::Completed {
                job: job(41, "job-1"),
                runner: Some(runner.clone()),
                result: ScaleSetJobResult::parse("succeeded").unwrap(),
            },
        ] {
            assert_eq!(
                reconcile_scale_set_acquisition_resolution(
                    &policy(),
                    &[request],
                    &delivery(vec![event]),
                    &reserved,
                )
                .unwrap_err()
                .code(),
                "delivery_consumer_resolution_conflict"
            );
        }
    }

    #[test]
    fn exact_request_binding_rejects_changed_job_evidence() {
        let reserved = reserve();
        let changed = delivery(vec![ScaleSetBridgeEvent::Assigned(job(41, "job-2"))]);
        assert_eq!(
            reconcile_scale_set_delivery(&policy(), &changed, &reserved, observed_at())
                .unwrap_err()
                .code(),
            "delivery_consumer_identity_drift"
        );
    }

    #[test]
    fn direct_assignment_reserves_capacity_without_premature_runner_authority() {
        let request_id = (1_u64 << 62) + 41;
        let assigned = delivery(vec![ScaleSetBridgeEvent::Assigned(job(
            request_id, "job-1",
        ))]);
        let reserved = reconcile_scale_set_delivery(
            &policy(),
            &assigned,
            &DisposableAttemptCatalogDocument::empty(),
            observed_at(),
        )
        .unwrap();
        let attempt = reserved.active()[0].attempt();
        assert_eq!(attempt.phase(), DisposableAttemptPhase::Reserved);
        assert_eq!(attempt.runner_request_id().get(), request_id);
        assert_eq!(attempt.not_after().get(), 21_700_000);
        assert!(attempt.github_job_id().is_none());
        assert_eq!(
            reconcile_scale_set_delivery(&policy(), &assigned, &reserved, observed_at()).unwrap(),
            reserved
        );
    }

    #[test]
    fn direct_assignment_cancellation_releases_without_external_object_authority() {
        let request_id = (1_u64 << 62) + 41;
        let assigned = delivery(vec![ScaleSetBridgeEvent::Assigned(job(
            request_id, "job-1",
        ))]);
        let reserved = reconcile_scale_set_delivery(
            &policy(),
            &assigned,
            &DisposableAttemptCatalogDocument::empty(),
            observed_at(),
        )
        .unwrap();
        let canceled = delivery(vec![ScaleSetBridgeEvent::Completed {
            job: job(request_id, "job-1"),
            runner: None,
            result: ScaleSetJobResult::parse("canceled").unwrap(),
        }]);
        let retired = reconcile_scale_set_delivery(
            &policy(),
            &canceled,
            &reserved,
            EpochMillis::new(200_000).unwrap(),
        )
        .unwrap();

        assert!(retired.active().is_empty());
        let tombstone = retired
            .find_tombstone_by_runner_request_id(ScaleSetRunnerRequestId::new(request_id).unwrap())
            .unwrap();
        assert_eq!(tombstone.phase(), DisposableAttemptPhase::Complete);
        assert!(tombstone.vm_identity().is_none());
        assert!(tombstone.runner_id().is_none());
        assert!(tombstone.github_job_id().is_none());
        assert!(tombstone.result().is_none());
        assert_eq!(
            reconcile_scale_set_delivery(
                &policy(),
                &canceled,
                &retired,
                EpochMillis::new(300_000).unwrap(),
            )
            .unwrap(),
            retired
        );

        let reassigned_request_id = request_id + 1;
        let reassigned = delivery(vec![ScaleSetBridgeEvent::Assigned(job(
            reassigned_request_id,
            "job-1",
        ))]);
        let next = reconcile_scale_set_delivery(
            &policy(),
            &reassigned,
            &retired,
            EpochMillis::new(300_000).unwrap(),
        )
        .unwrap();
        assert_eq!(next.active().len(), 1);
        assert_eq!(
            next.active()[0].attempt().runner_request_id().get(),
            reassigned_request_id
        );
    }

    #[test]
    fn direct_preclone_completion_refuses_runner_bearing_evidence() {
        let request_id = (1_u64 << 62) + 41;
        let reserved = reconcile_scale_set_delivery(
            &policy(),
            &delivery(vec![ScaleSetBridgeEvent::Assigned(job(
                request_id, "job-1",
            ))]),
            &DisposableAttemptCatalogDocument::empty(),
            observed_at(),
        )
        .unwrap();
        let runner = ScaleSetRunnerReference::new(
            ScaleSetRunnerId::new(501).unwrap(),
            reserved.active()[0].attempt().runner_name().clone(),
        );
        let event = ScaleSetBridgeEvent::Completed {
            job: job(request_id, "job-1"),
            runner: Some(runner),
            result: ScaleSetJobResult::parse("canceled").unwrap(),
        };
        assert_eq!(
            reconcile_scale_set_delivery(
                &policy(),
                &delivery(vec![event]),
                &reserved,
                EpochMillis::new(200_000).unwrap(),
            )
            .unwrap_err()
            .code(),
            "delivery_consumer_event_conflict"
        );
    }

    #[test]
    fn direct_cancellation_after_clone_authorization_still_completes_unprovisioned() {
        let request_id = (1_u64 << 62) + 61;
        let mut catalog = reconcile_scale_set_delivery(
            &policy(),
            &delivery(vec![ScaleSetBridgeEvent::Assigned(job(
                request_id, "job-1",
            ))]),
            &DisposableAttemptCatalogDocument::empty(),
            observed_at(),
        )
        .unwrap();
        let attempt_id = catalog.active()[0].attempt().attempt_id().clone();
        catalog = catalog
            .replace_attempt(
                &attempt_id,
                catalog.active()[0].attempt().revision(),
                DisposableAttemptCatalogAction::AuthorizeClone,
            )
            .unwrap();
        let canceled = delivery(vec![ScaleSetBridgeEvent::Completed {
            job: job(request_id, "job-1"),
            runner: None,
            result: ScaleSetJobResult::parse("canceled").unwrap(),
        }]);
        let retired = reconcile_scale_set_delivery(
            &policy(),
            &canceled,
            &catalog,
            EpochMillis::new(200_000).unwrap(),
        )
        .unwrap();

        assert!(retired.active().is_empty());
        let tombstone = retired
            .find_tombstone_by_runner_request_id(ScaleSetRunnerRequestId::new(request_id).unwrap())
            .unwrap();
        assert_eq!(tombstone.phase(), DisposableAttemptPhase::Complete);
        assert!(tombstone.vm_identity().is_none());
        assert!(tombstone.github_job_id().is_none());
    }

    #[test]
    fn direct_cancellation_does_not_block_unprovisioned_release() {
        let request_id = (1_u64 << 62) + 62;
        let mut catalog = reconcile_scale_set_delivery(
            &policy(),
            &delivery(vec![ScaleSetBridgeEvent::Assigned(job(
                request_id, "job-1",
            ))]),
            &DisposableAttemptCatalogDocument::empty(),
            observed_at(),
        )
        .unwrap();
        let attempt_id = catalog.active()[0].attempt().attempt_id().clone();
        catalog = catalog
            .replace_attempt(
                &attempt_id,
                catalog.active()[0].attempt().revision(),
                DisposableAttemptCatalogAction::BeginUnprovisionedRelease,
            )
            .unwrap();
        let canceled = delivery(vec![ScaleSetBridgeEvent::Completed {
            job: job(request_id, "job-1"),
            runner: None,
            result: ScaleSetJobResult::parse("canceled").unwrap(),
        }]);

        assert_eq!(
            reconcile_scale_set_delivery(
                &policy(),
                &canceled,
                &catalog,
                EpochMillis::new(200_000).unwrap(),
            )
            .unwrap(),
            catalog
        );
        let completed = catalog
            .replace_attempt(
                &attempt_id,
                catalog.active()[0].attempt().revision(),
                DisposableAttemptCatalogAction::CompleteUnprovisioned,
            )
            .unwrap();
        let retired = completed
            .retire_complete(&attempt_id, completed.active()[0].attempt().revision())
            .unwrap();
        assert_eq!(
            reconcile_scale_set_delivery(
                &policy(),
                &canceled,
                &retired,
                EpochMillis::new(300_000).unwrap(),
            )
            .unwrap(),
            retired
        );
    }

    #[test]
    fn direct_cancellation_after_owned_clone_start_enters_and_replays_cleanup() {
        for (request_id, begin_registration, bind_runner, mark_ready) in [
            ((1_u64 << 62) + 71, false, false, false),
            ((1_u64 << 62) + 72, true, false, false),
            ((1_u64 << 62) + 74, true, true, false),
            ((1_u64 << 62) + 75, true, true, true),
        ] {
            let mut catalog = reconcile_scale_set_delivery(
                &policy(),
                &delivery(vec![ScaleSetBridgeEvent::Assigned(job(
                    request_id, "job-1",
                ))]),
                &DisposableAttemptCatalogDocument::empty(),
                observed_at(),
            )
            .unwrap();
            let attempt_id = catalog.active()[0].attempt().attempt_id().clone();
            for action in [
                DisposableAttemptCatalogAction::AuthorizeClone,
                DisposableAttemptCatalogAction::RecordCloneStarted,
            ] {
                catalog = catalog
                    .replace_attempt(
                        &attempt_id,
                        catalog.active()[0].attempt().revision(),
                        action,
                    )
                    .unwrap();
            }
            catalog = catalog
                .bind_vm_identity_after_clone(
                    &attempt_id,
                    catalog.active()[0].attempt().revision(),
                    DisposableVmIdentity::parse(&format!("sha256:{}", "11".repeat(32))).unwrap(),
                )
                .unwrap();
            if begin_registration {
                catalog = catalog
                    .replace_attempt(
                        &attempt_id,
                        catalog.active()[0].attempt().revision(),
                        DisposableAttemptCatalogAction::BeginRegistration,
                    )
                    .unwrap();
            }
            let runner = ScaleSetRunnerReference::new(
                ScaleSetRunnerId::new(request_id).unwrap(),
                catalog.active()[0].attempt().runner_name().clone(),
            );
            if bind_runner {
                for action in [
                    DisposableAttemptCatalogAction::RecordJitGenerationStarted,
                    DisposableAttemptCatalogAction::RecordRegistration(runner.clone()),
                ] {
                    catalog = catalog
                        .replace_attempt(
                            &attempt_id,
                            catalog.active()[0].attempt().revision(),
                            action,
                        )
                        .unwrap();
                }
            }
            if mark_ready {
                for action in [
                    DisposableAttemptCatalogAction::RecordRunnerStartStarted,
                    DisposableAttemptCatalogAction::RecordRunnerReady(runner.clone()),
                ] {
                    catalog = catalog
                        .replace_attempt(
                            &attempt_id,
                            catalog.active()[0].attempt().revision(),
                            action,
                        )
                        .unwrap();
                }
            }
            let canceled = delivery(vec![ScaleSetBridgeEvent::Completed {
                job: job(request_id, "job-1"),
                runner: None,
                result: ScaleSetJobResult::parse("canceled").unwrap(),
            }]);
            let mut cleanup = reconcile_scale_set_delivery(
                &policy(),
                &canceled,
                &catalog,
                EpochMillis::new(200_000).unwrap(),
            )
            .unwrap();
            assert_eq!(
                cleanup.active()[0].attempt().phase(),
                DisposableAttemptPhase::Destroying
            );
            assert!(cleanup.active()[0].attempt().vm_identity().is_some());
            assert!(cleanup.active()[0].attempt().github_job_id().is_none());
            assert!(cleanup.active()[0].attempt().result().is_none());

            for phase in [
                DisposableAttemptPhase::Deregistering,
                DisposableAttemptPhase::Releasing,
                DisposableAttemptPhase::Complete,
            ] {
                cleanup = cleanup
                    .replace_attempt(
                        &attempt_id,
                        cleanup.active()[0].attempt().revision(),
                        DisposableAttemptCatalogAction::AdvanceCleanup(phase),
                    )
                    .unwrap();
            }
            let retired = cleanup
                .retire_complete(&attempt_id, cleanup.active()[0].attempt().revision())
                .unwrap();
            assert_eq!(
                reconcile_scale_set_delivery(
                    &policy(),
                    &canceled,
                    &retired,
                    EpochMillis::new(300_000).unwrap(),
                )
                .unwrap(),
                retired
            );
        }
    }

    #[test]
    fn direct_cancellation_preserves_unbound_clone_recovery_debt() {
        let request_id = (1_u64 << 62) + 73;
        let mut catalog = reconcile_scale_set_delivery(
            &policy(),
            &delivery(vec![ScaleSetBridgeEvent::Assigned(job(
                request_id, "job-1",
            ))]),
            &DisposableAttemptCatalogDocument::empty(),
            observed_at(),
        )
        .unwrap();
        let attempt_id = catalog.active()[0].attempt().attempt_id().clone();
        for action in [
            DisposableAttemptCatalogAction::AuthorizeClone,
            DisposableAttemptCatalogAction::RecordCloneStarted,
        ] {
            catalog = catalog
                .replace_attempt(
                    &attempt_id,
                    catalog.active()[0].attempt().revision(),
                    action,
                )
                .unwrap();
        }
        let canceled = delivery(vec![ScaleSetBridgeEvent::Completed {
            job: job(request_id, "job-1"),
            runner: None,
            result: ScaleSetJobResult::parse("canceled").unwrap(),
        }]);

        let reconciled = reconcile_scale_set_delivery(
            &policy(),
            &canceled,
            &catalog,
            EpochMillis::new(200_000).unwrap(),
        )
        .unwrap();

        assert_eq!(reconciled, catalog);
        assert_eq!(
            reconciled.active()[0].attempt().phase(),
            DisposableAttemptPhase::CloneStarted
        );
        assert!(reconciled.active()[0].attempt().vm_identity().is_none());
    }

    #[test]
    fn ordered_lifecycle_events_advance_the_exact_request_owner() {
        let mut catalog = reserve();
        let attempt_id = catalog.active()[0].attempt().attempt_id().clone();
        let mut attempt_revision = catalog.active()[0].attempt().revision();
        for action in [
            DisposableAttemptCatalogAction::AuthorizeClone,
            DisposableAttemptCatalogAction::RecordCloneStarted,
        ] {
            catalog = catalog
                .replace_attempt(&attempt_id, attempt_revision, action)
                .unwrap();
            attempt_revision = catalog.active()[0].attempt().revision();
        }
        catalog = catalog
            .bind_vm_identity_after_clone(
                &attempt_id,
                attempt_revision,
                DisposableVmIdentity::parse(&format!("sha256:{}", "11".repeat(32))).unwrap(),
            )
            .unwrap();
        attempt_revision = catalog.active()[0].attempt().revision();
        catalog = catalog
            .replace_attempt(
                &attempt_id,
                attempt_revision,
                DisposableAttemptCatalogAction::BeginRegistration,
            )
            .unwrap();

        let runner = ScaleSetRunnerReference::new(
            ScaleSetRunnerId::new(501).unwrap(),
            catalog.active()[0].attempt().runner_name().clone(),
        );
        catalog = checkpoint_runner_start(catalog, &attempt_id, &runner);
        let lifecycle = delivery(vec![
            ScaleSetBridgeEvent::Assigned(job(41, "job-1")),
            ScaleSetBridgeEvent::Started {
                job: job(41, "job-1"),
                runner: runner.clone(),
            },
            ScaleSetBridgeEvent::Completed {
                job: job(41, "job-1"),
                runner: Some(runner),
                result: ScaleSetJobResult::parse("succeeded").unwrap(),
            },
        ]);
        let terminal = reconcile_scale_set_delivery(
            &policy(),
            &lifecycle,
            &catalog,
            EpochMillis::new(200_000).unwrap(),
        )
        .unwrap();
        let attempt = terminal.active()[0].attempt();
        assert_eq!(attempt.phase(), DisposableAttemptPhase::Terminal);
        assert_eq!(attempt.github_job_id().unwrap().as_str(), "job-1");
        assert_eq!(attempt.result().unwrap().as_str(), "succeeded");
        assert_eq!(attempt.not_after().get(), 21_700_000);
    }

    #[test]
    fn tombstone_replay_requires_the_exact_runner_name_for_start_and_completion() {
        let mut catalog = reserve();
        let attempt_id = catalog.active()[0].attempt().attempt_id().clone();
        let mut attempt_revision = catalog.active()[0].attempt().revision();
        for action in [
            DisposableAttemptCatalogAction::AuthorizeClone,
            DisposableAttemptCatalogAction::RecordCloneStarted,
        ] {
            catalog = catalog
                .replace_attempt(&attempt_id, attempt_revision, action)
                .unwrap();
            attempt_revision = catalog.active()[0].attempt().revision();
        }
        catalog = catalog
            .bind_vm_identity_after_clone(
                &attempt_id,
                attempt_revision,
                DisposableVmIdentity::parse(&format!("sha256:{}", "11".repeat(32))).unwrap(),
            )
            .unwrap();
        attempt_revision = catalog.active()[0].attempt().revision();
        catalog = catalog
            .replace_attempt(
                &attempt_id,
                attempt_revision,
                DisposableAttemptCatalogAction::BeginRegistration,
            )
            .unwrap();
        let runner = ScaleSetRunnerReference::new(
            ScaleSetRunnerId::new(501).unwrap(),
            catalog.active()[0].attempt().runner_name().clone(),
        );
        catalog = checkpoint_runner_start(catalog, &attempt_id, &runner);
        let terminal_delivery = delivery(vec![ScaleSetBridgeEvent::Completed {
            job: job(41, "job-1"),
            runner: Some(runner.clone()),
            result: ScaleSetJobResult::parse("succeeded").unwrap(),
        }]);
        catalog = reconcile_scale_set_delivery(
            &policy(),
            &terminal_delivery,
            &catalog,
            EpochMillis::new(200_000).unwrap(),
        )
        .unwrap();
        for action in [
            DisposableAttemptCatalogAction::BeginCleanup,
            DisposableAttemptCatalogAction::AdvanceCleanup(DisposableAttemptPhase::Deregistering),
            DisposableAttemptCatalogAction::AdvanceCleanup(DisposableAttemptPhase::Releasing),
            DisposableAttemptCatalogAction::AdvanceCleanup(DisposableAttemptPhase::Complete),
        ] {
            attempt_revision = catalog.active()[0].attempt().revision();
            catalog = catalog
                .replace_attempt(&attempt_id, attempt_revision, action)
                .unwrap();
        }
        attempt_revision = catalog.active()[0].attempt().revision();
        catalog = catalog
            .retire_complete(&attempt_id, attempt_revision)
            .unwrap();

        let changed_runner = ScaleSetRunnerReference::new(
            runner.id,
            ScaleSetRunnerName::parse("smolrunner-replacement").unwrap(),
        );
        let conflicting_start = delivery(vec![ScaleSetBridgeEvent::Started {
            job: job(41, "job-1"),
            runner: changed_runner.clone(),
        }]);
        let conflicting_completion = delivery(vec![ScaleSetBridgeEvent::Completed {
            job: job(41, "job-1"),
            runner: Some(changed_runner),
            result: ScaleSetJobResult::parse("succeeded").unwrap(),
        }]);
        for conflicting in [&conflicting_start, &conflicting_completion] {
            assert_eq!(
                reconcile_scale_set_delivery(
                    &policy(),
                    conflicting,
                    &catalog,
                    EpochMillis::new(300_000).unwrap(),
                )
                .unwrap_err()
                .code(),
                "delivery_consumer_event_conflict"
            );
        }
        assert_eq!(
            reconcile_scale_set_delivery(
                &policy(),
                &terminal_delivery,
                &catalog,
                EpochMillis::new(300_000).unwrap(),
            )
            .unwrap(),
            catalog
        );
    }

    #[test]
    fn a_second_available_job_fails_atomically_at_one_worker_capacity() {
        let two = delivery(vec![
            ScaleSetBridgeEvent::Available(job(41, "job-1")),
            ScaleSetBridgeEvent::Available(job(42, "job-2")),
        ]);
        let empty = DisposableAttemptCatalogDocument::empty();
        assert_eq!(
            reconcile_scale_set_delivery(&policy(), &two, &empty, observed_at())
                .unwrap_err()
                .code(),
            "delivery_consumer_capacity_unavailable"
        );
        assert!(empty.active().is_empty());
    }
}
