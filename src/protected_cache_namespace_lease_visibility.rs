//! Conservative namespace-wide cache-lease visibility from personal-worker durable state.
//!
//! The report is a read-only snapshot. It cannot retain the store lock after return, infer
//! per-generation lease scope, authorize a catalog transition, or authorize cache reuse, reset,
//! eviction, deletion, or cleanup. `Active` and every `Unknown` reason conservatively veto the
//! entire protected namespace. Only an exact store revision captured between adapter-owned clock
//! observations with no matching or colliding durable lease reports snapshot inactivity.

use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::execution_admission::EpochMillis;
use crate::operator_config::OperatorConfig;
use crate::personal_worker_operator_store::{
    PersonalWorkerOperatorStore, PersonalWorkerOperatorStoreErrorKind,
};
use crate::personal_worker_queue::{
    PersonalWorkerCacheAccessMode, PersonalWorkerCacheNamespace, PersonalWorkerQueueGeneration,
};
use crate::personal_worker_store::{PersonalWorkerStoreDocument, PersonalWorkerStoreRevision};
use crate::protected_cache_generation_catalog::{
    ProtectedCacheGenerationFamily, ProtectedCacheNamespaceIdentity,
};

pub const PROTECTED_CACHE_NAMESPACE_LEASE_VISIBILITY_SCHEMA_VERSION: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtectedCacheNamespaceLeaseAuthority {
    ReadOnlyStoreSnapshotOnly,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtectedCacheNamespaceLeaseExpectation {
    family: ProtectedCacheGenerationFamily,
    namespace_identity: ProtectedCacheNamespaceIdentity,
    personal_worker_namespace: PersonalWorkerCacheNamespace,
    expected_store_revision: PersonalWorkerStoreRevision,
}

impl ProtectedCacheNamespaceLeaseExpectation {
    /// Bind one protected Cargo-target namespace to one exact personal-worker build namespace.
    ///
    /// # Errors
    ///
    /// Returns an error for a non-build namespace or a namespace-digest mismatch.
    pub fn new(
        family: ProtectedCacheGenerationFamily,
        namespace_identity: ProtectedCacheNamespaceIdentity,
        personal_worker_namespace: PersonalWorkerCacheNamespace,
        expected_store_revision: PersonalWorkerStoreRevision,
    ) -> Result<Self, ProtectedCacheNamespaceLeaseVisibilityError> {
        match (&family, &personal_worker_namespace) {
            (
                ProtectedCacheGenerationFamily::CargoTargetV1,
                PersonalWorkerCacheNamespace::RepositoryBuild {
                    namespace_digest, ..
                },
            ) if namespace_digest.as_str() == namespace_identity.as_str() => {}
            (
                ProtectedCacheGenerationFamily::CargoTargetV1,
                PersonalWorkerCacheNamespace::RepositoryBuild { .. },
            ) => {
                return Err(visibility_error(
                    ProtectedCacheNamespaceLeaseVisibilityErrorKind::NamespaceIdentityMismatch,
                    "protected and personal-worker cache namespace identities differ",
                ));
            }
            _ => {
                return Err(visibility_error(
                    ProtectedCacheNamespaceLeaseVisibilityErrorKind::UnsupportedNamespace,
                    "protected Cargo targets require one repository-build cache namespace",
                ));
            }
        }
        Ok(Self {
            family,
            namespace_identity,
            personal_worker_namespace,
            expected_store_revision,
        })
    }

    #[must_use]
    pub const fn family(&self) -> ProtectedCacheGenerationFamily {
        self.family
    }

    #[must_use]
    pub const fn namespace_identity(&self) -> &ProtectedCacheNamespaceIdentity {
        &self.namespace_identity
    }

    #[must_use]
    pub const fn personal_worker_namespace(&self) -> &PersonalWorkerCacheNamespace {
        &self.personal_worker_namespace
    }

    #[must_use]
    pub const fn expected_store_revision(&self) -> PersonalWorkerStoreRevision {
        self.expected_store_revision
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ProtectedCacheNamespaceLeaseCounts {
    read: u32,
    write: u32,
    exclusive: u32,
}

impl ProtectedCacheNamespaceLeaseCounts {
    #[must_use]
    pub const fn read(self) -> u32 {
        self.read
    }

    #[must_use]
    pub const fn write(self) -> u32 {
        self.write
    }

    #[must_use]
    pub const fn exclusive(self) -> u32 {
        self.exclusive
    }

    #[must_use]
    pub const fn total(self) -> u32 {
        self.read + self.write + self.exclusive
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum ProtectedCacheNamespaceLeaseUnknownReason {
    Store {
        kind: PersonalWorkerOperatorStoreErrorKind,
    },
    RevisionMismatch,
    CaptureClockUnavailable,
    CaptureClockMovedBackwards,
    NamespaceIdentityCollision,
    LeaseCountOverflow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ProtectedCacheNamespaceLeaseDisposition {
    Active {
        counts: ProtectedCacheNamespaceLeaseCounts,
    },
    InactiveAtExpectedRevision,
    Unknown {
        reason: ProtectedCacheNamespaceLeaseUnknownReason,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProtectedCacheNamespaceLeaseVisibility {
    schema_version: u8,
    authority: ProtectedCacheNamespaceLeaseAuthority,
    family: ProtectedCacheGenerationFamily,
    namespace_identity: ProtectedCacheNamespaceIdentity,
    personal_worker_namespace: PersonalWorkerCacheNamespace,
    expected_store_revision: PersonalWorkerStoreRevision,
    observed_store_revision: Option<PersonalWorkerStoreRevision>,
    observed_queue_generation: Option<PersonalWorkerQueueGeneration>,
    durable_observed_at: Option<EpochMillis>,
    capture_started_at: Option<EpochMillis>,
    capture_completed_at: Option<EpochMillis>,
    disposition: ProtectedCacheNamespaceLeaseDisposition,
}

impl ProtectedCacheNamespaceLeaseVisibility {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn authority(&self) -> ProtectedCacheNamespaceLeaseAuthority {
        self.authority
    }

    #[must_use]
    pub const fn family(&self) -> ProtectedCacheGenerationFamily {
        self.family
    }

    #[must_use]
    pub const fn namespace_identity(&self) -> &ProtectedCacheNamespaceIdentity {
        &self.namespace_identity
    }

    #[must_use]
    pub const fn personal_worker_namespace(&self) -> &PersonalWorkerCacheNamespace {
        &self.personal_worker_namespace
    }

    #[must_use]
    pub const fn expected_store_revision(&self) -> PersonalWorkerStoreRevision {
        self.expected_store_revision
    }

    #[must_use]
    pub const fn observed_store_revision(&self) -> Option<PersonalWorkerStoreRevision> {
        self.observed_store_revision
    }

    #[must_use]
    pub const fn observed_queue_generation(&self) -> Option<PersonalWorkerQueueGeneration> {
        self.observed_queue_generation
    }

    #[must_use]
    pub const fn durable_observed_at(&self) -> Option<EpochMillis> {
        self.durable_observed_at
    }

    #[must_use]
    pub const fn capture_started_at(&self) -> Option<EpochMillis> {
        self.capture_started_at
    }

    #[must_use]
    pub const fn capture_completed_at(&self) -> Option<EpochMillis> {
        self.capture_completed_at
    }

    #[must_use]
    pub const fn disposition(&self) -> ProtectedCacheNamespaceLeaseDisposition {
        self.disposition
    }

    /// Report whether the observation vetoes reuse, transition, or reclamation.
    ///
    /// This is conservative policy vocabulary, not positive mutation authority. Only an exact
    /// snapshot-inactive result avoids the veto, and every mutating consumer must still freshly
    /// revalidate the personal-worker store under its own reviewed lock composition.
    #[must_use]
    pub const fn requires_conservative_veto(&self) -> bool {
        !matches!(
            self.disposition,
            ProtectedCacheNamespaceLeaseDisposition::InactiveAtExpectedRevision
        )
    }
}

/// Derive conservative namespace-wide lease visibility from one fresh config-bound read-only open.
///
/// The adapter brackets its own store read with wall-clock observations. Any clock or store failure
/// remains `unknown`; callers cannot relabel an older retained document as a fresh observation.
#[must_use]
pub fn inspect_protected_cache_namespace_leases(
    config: &OperatorConfig,
    expectation: &ProtectedCacheNamespaceLeaseExpectation,
) -> ProtectedCacheNamespaceLeaseVisibility {
    inspect_with_clock(config, expectation, system_epoch_millis)
}

fn inspect_with_clock(
    config: &OperatorConfig,
    expectation: &ProtectedCacheNamespaceLeaseExpectation,
    mut clock: impl FnMut() -> Result<EpochMillis, ()>,
) -> ProtectedCacheNamespaceLeaseVisibility {
    let Ok(capture_started_at) = clock() else {
        return unknown_visibility(
            expectation,
            None,
            None,
            None,
            None,
            None,
            ProtectedCacheNamespaceLeaseUnknownReason::CaptureClockUnavailable,
        );
    };
    let opened = PersonalWorkerOperatorStore::open_current(config);
    let Ok(capture_completed_at) = clock() else {
        return unknown_visibility(
            expectation,
            opened
                .as_ref()
                .ok()
                .map(|state| state.document().revision()),
            opened
                .as_ref()
                .ok()
                .map(|state| state.document().queue().generation),
            opened
                .as_ref()
                .ok()
                .map(|state| state.document().queue().observed_at),
            Some(capture_started_at),
            None,
            ProtectedCacheNamespaceLeaseUnknownReason::CaptureClockUnavailable,
        );
    };
    if capture_completed_at < capture_started_at {
        return unknown_visibility(
            expectation,
            opened
                .as_ref()
                .ok()
                .map(|state| state.document().revision()),
            opened
                .as_ref()
                .ok()
                .map(|state| state.document().queue().generation),
            opened
                .as_ref()
                .ok()
                .map(|state| state.document().queue().observed_at),
            Some(capture_started_at),
            Some(capture_completed_at),
            ProtectedCacheNamespaceLeaseUnknownReason::CaptureClockMovedBackwards,
        );
    }
    match opened {
        Ok(opened) => observe_current_document(
            opened.document(),
            expectation,
            capture_started_at,
            capture_completed_at,
        ),
        Err(error) => unknown_visibility(
            expectation,
            None,
            None,
            None,
            Some(capture_started_at),
            Some(capture_completed_at),
            ProtectedCacheNamespaceLeaseUnknownReason::Store { kind: error.kind() },
        ),
    }
}

fn observe_current_document(
    document: &PersonalWorkerStoreDocument,
    expectation: &ProtectedCacheNamespaceLeaseExpectation,
    capture_started_at: EpochMillis,
    capture_completed_at: EpochMillis,
) -> ProtectedCacheNamespaceLeaseVisibility {
    let revision = document.revision();
    let generation = document.queue().generation;
    let observed_at = document.queue().observed_at;
    if revision != expectation.expected_store_revision {
        return unknown_visibility(
            expectation,
            Some(revision),
            Some(generation),
            Some(observed_at),
            Some(capture_started_at),
            Some(capture_completed_at),
            ProtectedCacheNamespaceLeaseUnknownReason::RevisionMismatch,
        );
    }
    if capture_completed_at < capture_started_at {
        return unknown_visibility(
            expectation,
            Some(revision),
            Some(generation),
            Some(observed_at),
            Some(capture_started_at),
            Some(capture_completed_at),
            ProtectedCacheNamespaceLeaseUnknownReason::CaptureClockMovedBackwards,
        );
    }

    let mut counts = ProtectedCacheNamespaceLeaseCounts {
        read: 0,
        write: 0,
        exclusive: 0,
    };
    for lease in document.cache_leases() {
        if namespace_digest(lease.namespace()).as_str() != expectation.namespace_identity.as_str() {
            continue;
        }
        if lease.namespace() != &expectation.personal_worker_namespace {
            return unknown_visibility(
                expectation,
                Some(revision),
                Some(generation),
                Some(observed_at),
                Some(capture_started_at),
                Some(capture_completed_at),
                ProtectedCacheNamespaceLeaseUnknownReason::NamespaceIdentityCollision,
            );
        }
        let count = match lease.access() {
            PersonalWorkerCacheAccessMode::Read => &mut counts.read,
            PersonalWorkerCacheAccessMode::Write => &mut counts.write,
            PersonalWorkerCacheAccessMode::Exclusive => &mut counts.exclusive,
        };
        let Some(next) = count.checked_add(1) else {
            return unknown_visibility(
                expectation,
                Some(revision),
                Some(generation),
                Some(observed_at),
                Some(capture_started_at),
                Some(capture_completed_at),
                ProtectedCacheNamespaceLeaseUnknownReason::LeaseCountOverflow,
            );
        };
        *count = next;
    }

    visibility(
        expectation,
        Some(revision),
        Some(generation),
        Some(observed_at),
        Some(capture_started_at),
        Some(capture_completed_at),
        if counts.total() == 0 {
            ProtectedCacheNamespaceLeaseDisposition::InactiveAtExpectedRevision
        } else {
            ProtectedCacheNamespaceLeaseDisposition::Active { counts }
        },
    )
}

fn namespace_digest(namespace: &PersonalWorkerCacheNamespace) -> &crate::artifact::Sha256Digest {
    match namespace {
        PersonalWorkerCacheNamespace::RepositoryBuild {
            namespace_digest, ..
        }
        | PersonalWorkerCacheNamespace::SharedDownload {
            namespace_digest, ..
        } => namespace_digest,
    }
}

fn unknown_visibility(
    expectation: &ProtectedCacheNamespaceLeaseExpectation,
    observed_store_revision: Option<PersonalWorkerStoreRevision>,
    observed_queue_generation: Option<PersonalWorkerQueueGeneration>,
    durable_observed_at: Option<EpochMillis>,
    capture_started_at: Option<EpochMillis>,
    capture_completed_at: Option<EpochMillis>,
    reason: ProtectedCacheNamespaceLeaseUnknownReason,
) -> ProtectedCacheNamespaceLeaseVisibility {
    visibility(
        expectation,
        observed_store_revision,
        observed_queue_generation,
        durable_observed_at,
        capture_started_at,
        capture_completed_at,
        ProtectedCacheNamespaceLeaseDisposition::Unknown { reason },
    )
}

fn visibility(
    expectation: &ProtectedCacheNamespaceLeaseExpectation,
    observed_store_revision: Option<PersonalWorkerStoreRevision>,
    observed_queue_generation: Option<PersonalWorkerQueueGeneration>,
    durable_observed_at: Option<EpochMillis>,
    capture_started_at: Option<EpochMillis>,
    capture_completed_at: Option<EpochMillis>,
    disposition: ProtectedCacheNamespaceLeaseDisposition,
) -> ProtectedCacheNamespaceLeaseVisibility {
    ProtectedCacheNamespaceLeaseVisibility {
        schema_version: PROTECTED_CACHE_NAMESPACE_LEASE_VISIBILITY_SCHEMA_VERSION,
        authority: ProtectedCacheNamespaceLeaseAuthority::ReadOnlyStoreSnapshotOnly,
        family: expectation.family,
        namespace_identity: expectation.namespace_identity.clone(),
        personal_worker_namespace: expectation.personal_worker_namespace.clone(),
        expected_store_revision: expectation.expected_store_revision,
        observed_store_revision,
        observed_queue_generation,
        durable_observed_at,
        capture_started_at,
        capture_completed_at,
        disposition,
    }
}

fn system_epoch_millis() -> Result<EpochMillis, ()> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| ())?;
    let millis = u64::try_from(elapsed.as_millis()).map_err(|_| ())?;
    EpochMillis::new(millis).map_err(|_| ())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtectedCacheNamespaceLeaseVisibilityErrorKind {
    UnsupportedNamespace,
    NamespaceIdentityMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProtectedCacheNamespaceLeaseVisibilityError {
    kind: ProtectedCacheNamespaceLeaseVisibilityErrorKind,
    message: &'static str,
}

impl ProtectedCacheNamespaceLeaseVisibilityError {
    #[must_use]
    pub const fn kind(&self) -> ProtectedCacheNamespaceLeaseVisibilityErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn message(&self) -> &'static str {
        self.message
    }
}

impl fmt::Display for ProtectedCacheNamespaceLeaseVisibilityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for ProtectedCacheNamespaceLeaseVisibilityError {}

const fn visibility_error(
    kind: ProtectedCacheNamespaceLeaseVisibilityErrorKind,
    message: &'static str,
) -> ProtectedCacheNamespaceLeaseVisibilityError {
    ProtectedCacheNamespaceLeaseVisibilityError { kind, message }
}

#[cfg(test)]
mod tests {
    use crate::artifact::{CommitId, GitTreeId, RepositoryRef, Sha256Digest};
    use crate::execution_admission::{
        ExecutionAdmissionIdentity, ExecutionAdmissionInput, ExecutionAdmissionRecord,
        ExecutionAdmissionState, ExecutionRequestId, ExecutionResourceLimits,
        FallbackProfileEligibility, HostCapacityObservation, ReservationEvidence,
        ReservationGeneration, ReservationId, RunnerProfileId,
    };
    use crate::lima_observation::LimaInstanceName;
    use crate::mac_availability::AvailabilityRequest;
    use crate::operator_config::{
        GuestWorkspacePath, OperatorIdlePolicy, OperatorOutputPreference,
        OperatorRemediationPreference, PersonalWorkerStateRoot,
    };
    use crate::personal_worker_queue::{
        PersonalWorkerActiveReservation, PersonalWorkerActivityEvidence,
        PersonalWorkerCancellationState, PersonalWorkerJobRequest, PersonalWorkerPriority,
        PersonalWorkerProfile, PersonalWorkerProfileObservation, PersonalWorkerQueueInput,
        PersonalWorkerSourceIdentity,
    };
    use crate::personal_worker_store::PersonalWorkerDurableCacheLease;
    use crate::verification_profile::{CacheId, VerificationProfileId};

    use super::*;

    const OBSERVED_AT: u64 = 1_000_000;
    const GIB: u64 = 1_024 * 1_024 * 1_024;

    fn time(value: u64) -> EpochMillis {
        EpochMillis::new(value).unwrap()
    }

    fn digest(character: char) -> Sha256Digest {
        Sha256Digest::parse(&format!("sha256:{}", character.to_string().repeat(64))).unwrap()
    }

    fn namespace(
        cache_id: &str,
        repository: &str,
        namespace_digest: Sha256Digest,
    ) -> PersonalWorkerCacheNamespace {
        PersonalWorkerCacheNamespace::RepositoryBuild {
            cache_id: CacheId::parse(cache_id).unwrap(),
            repository: RepositoryRef::parse(repository).unwrap(),
            namespace_digest,
        }
    }

    fn expectation(
        namespace: PersonalWorkerCacheNamespace,
    ) -> ProtectedCacheNamespaceLeaseExpectation {
        let namespace_identity =
            ProtectedCacheNamespaceIdentity::parse(namespace_digest(&namespace).as_str()).unwrap();
        ProtectedCacheNamespaceLeaseExpectation::new(
            ProtectedCacheGenerationFamily::CargoTargetV1,
            namespace_identity,
            namespace,
            PersonalWorkerStoreRevision::new(1).unwrap(),
        )
        .unwrap()
    }

    fn limits() -> ExecutionResourceLimits {
        ExecutionResourceLimits::new(2_000, 2 * GIB, 2_048).unwrap()
    }

    fn config() -> OperatorConfig {
        OperatorConfig::new(
            PersonalWorkerStateRoot::parse("/private/test-state").unwrap(),
            LimaInstanceName::parse("glaeda").unwrap(),
            GuestWorkspacePath::parse("/home/runner/workspace").unwrap(),
            VerificationProfileId::parse("glaeda.required").unwrap(),
            AvailabilityRequest::Auto,
            OperatorIdlePolicy::new(600_000, 1_800_000).unwrap(),
            OperatorOutputPreference::Json,
            OperatorRemediationPreference::CodesOnly,
        )
        .unwrap()
    }

    fn active(
        id: &str,
        namespace: PersonalWorkerCacheNamespace,
        access: PersonalWorkerCacheAccessMode,
    ) -> (
        PersonalWorkerActiveReservation,
        PersonalWorkerDurableCacheLease,
    ) {
        let repository = match &namespace {
            PersonalWorkerCacheNamespace::RepositoryBuild { repository, .. } => repository.clone(),
            PersonalWorkerCacheNamespace::SharedDownload { .. } => {
                RepositoryRef::parse("example/project").unwrap()
            }
        };
        let identity = ExecutionAdmissionIdentity::new(
            ExecutionRequestId::parse(id).unwrap(),
            VerificationProfileId::parse("glaeda.required").unwrap(),
            RunnerProfileId::parse("personal-linux-work").unwrap(),
        );
        let request = PersonalWorkerJobRequest {
            identity: identity.clone(),
            source: PersonalWorkerSourceIdentity::new(
                repository,
                CommitId::parse(&"1a".repeat(20)).unwrap(),
                GitTreeId::parse(&"2b".repeat(20)).unwrap(),
            ),
            priority: PersonalWorkerPriority::Normal,
            requested_limits: limits(),
            cache_namespace: namespace.clone(),
            cache_access: access,
            submitted_at: time(OBSERVED_AT - 100),
            operator_deadline: None,
            cancellation: PersonalWorkerCancellationState::Active,
            fallback_eligibility: FallbackProfileEligibility::ineligible(),
        };
        let reservation_id = ReservationId::parse(&format!("reservation-{id}")).unwrap();
        let reservation_generation = ReservationGeneration::new(1).unwrap();
        let acquired_at = time(OBSERVED_AT - 50);
        let admission = ExecutionAdmissionRecord::from_input(ExecutionAdmissionInput {
            identity,
            state: ExecutionAdmissionState::Running,
            observed_at: time(OBSERVED_AT),
            requested_limits: limits(),
            host_capacity: Some(HostCapacityObservation::new(
                time(OBSERVED_AT),
                ExecutionResourceLimits::new(8_000, 10 * GIB, 8_192).unwrap(),
            )),
            applied_limits: Some(limits()),
            queue_position: None,
            reservation: Some(ReservationEvidence::new(
                reservation_id.clone(),
                reservation_generation,
                acquired_at,
                time(OBSERVED_AT + 10_000),
            )),
            acknowledgement: None,
            fallback_eligibility: FallbackProfileEligibility::ineligible(),
            unavailable_reason: None,
        })
        .unwrap();
        let lease = PersonalWorkerDurableCacheLease::new(
            request.identity.request_id.clone(),
            namespace,
            access,
            reservation_id,
            reservation_generation,
            acquired_at,
        );
        (
            PersonalWorkerActiveReservation {
                request,
                admission,
                started_at: Some(time(OBSERVED_AT - 25)),
            },
            lease,
        )
    }

    fn document(
        active: Vec<PersonalWorkerActiveReservation>,
        leases: Vec<PersonalWorkerDurableCacheLease>,
    ) -> PersonalWorkerStoreDocument {
        PersonalWorkerStoreDocument::new(
            PersonalWorkerQueueInput {
                generation: PersonalWorkerQueueGeneration::new(1).unwrap(),
                observed_at: time(OBSERVED_AT),
                profile_observation: PersonalWorkerProfileObservation::observed(
                    if active.is_empty() {
                        PersonalWorkerProfile::Interactive
                    } else {
                        PersonalWorkerProfile::Work
                    },
                ),
                activity_evidence: PersonalWorkerActivityEvidence::observed(time(OBSERVED_AT)),
                queued: vec![],
                active,
                pending_profile_change: None,
            },
            leases,
        )
        .unwrap()
    }

    #[test]
    fn exact_fresh_empty_namespace_is_inactive_at_the_expected_revision() {
        let namespace = namespace("cargo-target", "teamleaderleo/glaeda", digest('a'));
        let expected = expectation(namespace);
        let report = observe_current_document(
            &document(vec![], vec![]),
            &expected,
            time(OBSERVED_AT + 10),
            time(OBSERVED_AT + 11),
        );

        assert_eq!(report.schema_version(), 1);
        assert_eq!(
            report.authority(),
            ProtectedCacheNamespaceLeaseAuthority::ReadOnlyStoreSnapshotOnly
        );
        assert_eq!(
            report.disposition(),
            ProtectedCacheNamespaceLeaseDisposition::InactiveAtExpectedRevision
        );
        assert!(!report.requires_conservative_veto());
        assert_eq!(report.observed_store_revision().unwrap().get(), 1);
        assert_eq!(report.observed_queue_generation().unwrap().get(), 1);
        assert_eq!(report.durable_observed_at().unwrap().get(), OBSERVED_AT);
        assert_eq!(report.capture_started_at().unwrap().get(), OBSERVED_AT + 10);
        assert_eq!(
            report.capture_completed_at().unwrap().get(),
            OBSERVED_AT + 11
        );
    }

    #[test]
    fn every_exact_access_mode_vetoes_the_whole_namespace() {
        for (access, expected_counts) in [
            (
                PersonalWorkerCacheAccessMode::Read,
                ProtectedCacheNamespaceLeaseCounts {
                    read: 1,
                    write: 0,
                    exclusive: 0,
                },
            ),
            (
                PersonalWorkerCacheAccessMode::Write,
                ProtectedCacheNamespaceLeaseCounts {
                    read: 0,
                    write: 1,
                    exclusive: 0,
                },
            ),
            (
                PersonalWorkerCacheAccessMode::Exclusive,
                ProtectedCacheNamespaceLeaseCounts {
                    read: 0,
                    write: 0,
                    exclusive: 1,
                },
            ),
        ] {
            let namespace = namespace("cargo-target", "teamleaderleo/glaeda", digest('a'));
            let expected = expectation(namespace.clone());
            let (active, lease) = active("job-one", namespace, access);
            let report = observe_current_document(
                &document(vec![active], vec![lease]),
                &expected,
                time(OBSERVED_AT + 10),
                time(OBSERVED_AT + 11),
            );

            assert_eq!(
                report.disposition(),
                ProtectedCacheNamespaceLeaseDisposition::Active {
                    counts: expected_counts
                }
            );
            assert!(report.requires_conservative_veto());
        }
    }

    #[test]
    fn revision_mismatch_and_backwards_capture_clock_remain_unknown() {
        let namespace = namespace("cargo-target", "teamleaderleo/glaeda", digest('a'));
        let mut expected = expectation(namespace);
        let current = document(vec![], vec![]);

        expected.expected_store_revision = PersonalWorkerStoreRevision::new(2).unwrap();
        let revision = observe_current_document(
            &current,
            &expected,
            time(OBSERVED_AT + 10),
            time(OBSERVED_AT + 11),
        );
        assert_eq!(
            revision.disposition(),
            ProtectedCacheNamespaceLeaseDisposition::Unknown {
                reason: ProtectedCacheNamespaceLeaseUnknownReason::RevisionMismatch
            }
        );
        assert!(revision.requires_conservative_veto());

        expected.expected_store_revision = PersonalWorkerStoreRevision::new(1).unwrap();
        let stale = observe_current_document(
            &current,
            &expected,
            time(OBSERVED_AT + 11),
            time(OBSERVED_AT + 10),
        );
        assert_eq!(
            stale.disposition(),
            ProtectedCacheNamespaceLeaseDisposition::Unknown {
                reason: ProtectedCacheNamespaceLeaseUnknownReason::CaptureClockMovedBackwards
            }
        );
        assert!(stale.requires_conservative_veto());
    }

    #[test]
    fn same_digest_under_another_namespace_is_unknown_not_inactive() {
        let expected_namespace = namespace("cargo-target", "teamleaderleo/glaeda", digest('a'));
        let colliding_namespace = namespace("other-cache", "example/other", digest('a'));
        let expected = expectation(expected_namespace);
        let (active, lease) = active(
            "job-collision",
            colliding_namespace,
            PersonalWorkerCacheAccessMode::Read,
        );
        let report = observe_current_document(
            &document(vec![active], vec![lease]),
            &expected,
            time(OBSERVED_AT + 10),
            time(OBSERVED_AT + 11),
        );

        assert_eq!(
            report.disposition(),
            ProtectedCacheNamespaceLeaseDisposition::Unknown {
                reason: ProtectedCacheNamespaceLeaseUnknownReason::NamespaceIdentityCollision
            }
        );
        assert!(report.requires_conservative_veto());
    }

    #[test]
    fn store_failures_are_path_free_unknown_vetoes() {
        let namespace = namespace("cargo-target", "teamleaderleo/glaeda", digest('a'));
        let expected = expectation(namespace);
        for kind in [
            PersonalWorkerOperatorStoreErrorKind::Missing,
            PersonalWorkerOperatorStoreErrorKind::UnsafeFilesystem,
            PersonalWorkerOperatorStoreErrorKind::CorruptState,
            PersonalWorkerOperatorStoreErrorKind::VersionIncompatible,
            PersonalWorkerOperatorStoreErrorKind::RecoveryRequired,
            PersonalWorkerOperatorStoreErrorKind::Busy,
            PersonalWorkerOperatorStoreErrorKind::Unavailable,
            PersonalWorkerOperatorStoreErrorKind::UnsupportedPlatform,
            PersonalWorkerOperatorStoreErrorKind::InvalidInitialState,
        ] {
            let report = unknown_visibility(
                &expected,
                None,
                None,
                None,
                Some(time(OBSERVED_AT + 10)),
                Some(time(OBSERVED_AT + 11)),
                ProtectedCacheNamespaceLeaseUnknownReason::Store { kind },
            );
            assert!(report.requires_conservative_veto());
            assert_eq!(report.observed_store_revision(), None);
            let json = serde_json::to_string(&report).unwrap();
            for forbidden in ["/home/", "/tmp/", "current.json", "store.lock"] {
                assert!(!json.contains(forbidden));
            }
        }
    }

    #[test]
    fn adapter_owned_capture_clock_failure_is_unknown_without_opening_the_store() {
        let namespace = namespace("cargo-target", "teamleaderleo/glaeda", digest('a'));
        let report = inspect_with_clock(&config(), &expectation(namespace), || Err(()));

        assert_eq!(
            report.disposition(),
            ProtectedCacheNamespaceLeaseDisposition::Unknown {
                reason: ProtectedCacheNamespaceLeaseUnknownReason::CaptureClockUnavailable
            }
        );
        assert!(report.requires_conservative_veto());
        assert_eq!(report.capture_started_at(), None);
        assert_eq!(report.capture_completed_at(), None);
    }

    #[test]
    fn expectation_rejects_wrong_namespace_class_and_digest() {
        let protected = ProtectedCacheNamespaceIdentity::parse(digest('a').as_str()).unwrap();
        let shared = PersonalWorkerCacheNamespace::SharedDownload {
            cache_id: CacheId::parse("downloads").unwrap(),
            namespace_digest: digest('a'),
        };
        assert_eq!(
            ProtectedCacheNamespaceLeaseExpectation::new(
                ProtectedCacheGenerationFamily::CargoTargetV1,
                protected.clone(),
                shared,
                PersonalWorkerStoreRevision::new(1).unwrap(),
            )
            .unwrap_err()
            .kind(),
            ProtectedCacheNamespaceLeaseVisibilityErrorKind::UnsupportedNamespace
        );

        let wrong_digest = namespace("cargo-target", "teamleaderleo/glaeda", digest('b'));
        assert_eq!(
            ProtectedCacheNamespaceLeaseExpectation::new(
                ProtectedCacheGenerationFamily::CargoTargetV1,
                protected.clone(),
                wrong_digest,
                PersonalWorkerStoreRevision::new(1).unwrap(),
            )
            .unwrap_err()
            .kind(),
            ProtectedCacheNamespaceLeaseVisibilityErrorKind::NamespaceIdentityMismatch
        );
    }
}
