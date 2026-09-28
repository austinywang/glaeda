use std::fmt;
use std::marker::PhantomData;

use serde::Serialize;

use crate::project_catalog::ProjectIdentity;

use super::{
    HotStateCapabilityGenerationId, HotStateCapabilityObservation, HotStateGenerationId,
    HotStatePathClassId, HotStatePathPolicy, HotStateReuseIdentity,
};

#[derive(Clone, PartialEq, Eq)]
pub struct HotStateFamilyRef(HotStateGenerationId);

impl HotStateFamilyRef {
    #[must_use]
    pub fn new(value: HotStateGenerationId) -> Self {
        Self(value)
    }
}

impl fmt::Debug for HotStateFamilyRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<opaque-hot-state-family-ref>")
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct HotStateBindingRef(HotStateGenerationId);

impl HotStateBindingRef {
    #[must_use]
    pub fn new(value: HotStateGenerationId) -> Self {
        Self(value)
    }
}

impl fmt::Debug for HotStateBindingRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<opaque-hot-state-binding-ref>")
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct HotStateAdmissionTarget {
    family: HotStateFamilyRef,
    binding: HotStateBindingRef,
    project: ProjectIdentity,
    path_class: HotStatePathClassId,
}

impl HotStateAdmissionTarget {
    #[must_use]
    pub fn new(
        family: HotStateFamilyRef,
        binding: HotStateBindingRef,
        project: ProjectIdentity,
        path_class: HotStatePathClassId,
    ) -> Self {
        Self {
            family,
            binding,
            project,
            path_class,
        }
    }
}

impl fmt::Debug for HotStateAdmissionTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<opaque-hot-state-admission-target>")
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct HotStateAdmissionSemantics {
    reuse_identity: HotStateReuseIdentity,
    profile: HotStateGenerationId,
    validator: HotStateGenerationId,
    platform: HotStateGenerationId,
}

impl HotStateAdmissionSemantics {
    #[must_use]
    pub fn new(
        reuse_identity: HotStateReuseIdentity,
        profile: HotStateGenerationId,
        validator: HotStateGenerationId,
        platform: HotStateGenerationId,
    ) -> Self {
        Self {
            reuse_identity,
            profile,
            validator,
            platform,
        }
    }
}

impl fmt::Debug for HotStateAdmissionSemantics {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<opaque-hot-state-admission-semantics>")
    }
}

#[derive(Clone, PartialEq, Eq)]
pub enum HotStateReusableState {
    SealedImmutable,
    LeaseMutable {
        lease_generation: HotStateGenerationId,
        lease_capability_generation: HotStateGenerationId,
    },
}

impl fmt::Debug for HotStateReusableState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SealedImmutable => formatter.write_str("SealedImmutable"),
            Self::LeaseMutable { .. } => formatter.write_str("LeaseMutable"),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct HotStateAdmissionContext {
    target: HotStateAdmissionTarget,
    semantics: HotStateAdmissionSemantics,
    reusable_state: HotStateReusableState,
}

impl HotStateAdmissionContext {
    #[must_use]
    pub fn new(
        target: HotStateAdmissionTarget,
        semantics: HotStateAdmissionSemantics,
        reusable_state: HotStateReusableState,
    ) -> Self {
        Self {
            target,
            semantics,
            reusable_state,
        }
    }
}

impl fmt::Debug for HotStateAdmissionContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<opaque-hot-state-admission-context>")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HotStateAdmissionMismatchField {
    Family,
    Binding,
    Project,
    PathClass,
    Source,
    Toolchain,
    Trust,
    Policy,
    Profile,
    Validator,
    Platform,
    StateClass,
    LeaseGeneration,
    LeaseCapabilityGeneration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotStateForbiddenReason {
    CredentialOrSecretDerivedAuthority,
    LiveExecutionAuthorityOrProcess,
    UniqueLocalWork,
    DiagnosticOnlyState,
    CrossTrustWritableState,
    UnsupportedLiveState,
    UnknownPrivacy,
    AuthorityBearingMetadata,
    TaskRecoveryState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotStateQuarantineReason {
    UnknownProducer,
    UnknownVersion,
    UnknownWriterHistory,
    ActiveOrUnsettledDescendants,
    IncompletePublication,
    RebindAmbiguous,
    MissingCanonicalParent,
    RestoredUnobserved,
    IdentityOrIntegrityAmbiguous,
    OwnerAmbiguous,
    ModeAmbiguous,
    SameNameAmbiguous,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotStateResourceDisposition {
    Accepted,
    Refused,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotStateAdmissionAuthority {
    AccelerationOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotStateAdmissionRefusal {
    Mismatch {
        field: HotStateAdmissionMismatchField,
    },
    QuarantineRequired {
        reason: HotStateQuarantineReason,
    },
    ResourceRefused,
    Forbidden {
        reason: HotStateForbiddenReason,
    },
}

impl HotStateAdmissionRefusal {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Mismatch { .. } => "hot_state_admission_mismatch",
            Self::QuarantineRequired { .. } => "hot_state_quarantine_required",
            Self::ResourceRefused => "hot_state_resource_refused",
            Self::Forbidden { .. } => "hot_state_reuse_forbidden",
        }
    }
}

/// Standing of one reusable-state family as its reviewed owner already observed it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotStateFamilyStanding {
    Eligible,
    Quarantined(HotStateQuarantineReason),
    PermanentlyForbidden(HotStateForbiddenReason),
}

struct AdmissionSeal;

/// One family owner's observation offered as admission evidence.
///
/// There is no public constructor. `observed` is crate-visible, so only a reviewed in-crate
/// reusable-state owner that already holds the family record can offer evidence. Holding this
/// value is not admission: only `admit_family_evidence` mints `AdmittedHotStateCandidate`, and
/// the seal that type carries stays private to this module.
pub struct HotStateFamilyAdmissionEvidence<'proof> {
    context: HotStateAdmissionContext,
    standing: HotStateFamilyStanding,
    unique_local_work: bool,
    _proof: PhantomData<&'proof ()>,
}

impl<'proof> HotStateFamilyAdmissionEvidence<'proof> {
    /// Offer one family observation for admission.
    ///
    /// `proof` is the live borrow of the family record the observation came from. The returned
    /// evidence cannot outlive it, so a released or superseded record cannot be re-offered.
    pub(crate) fn observed<P: ?Sized>(
        proof: &'proof P,
        context: HotStateAdmissionContext,
        standing: HotStateFamilyStanding,
        unique_local_work: bool,
    ) -> Self {
        let _ = proof;
        Self {
            context,
            standing,
            unique_local_work,
            _proof: PhantomData,
        }
    }
}

impl fmt::Debug for HotStateFamilyAdmissionEvidence<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<opaque-hot-state-family-admission-evidence>")
    }
}

pub struct AdmittedHotStateCandidate<'proof> {
    current_context: HotStateAdmissionContext,
    policy_path_class: HotStatePathClassId,
    policy_reuse_identity: HotStateReuseIdentity,
    capability_generation: HotStateCapabilityGenerationId,
    _proof: PhantomData<&'proof ()>,
    _seal: AdmissionSeal,
}

impl AdmittedHotStateCandidate<'_> {
    #[must_use]
    pub const fn authority(&self) -> HotStateAdmissionAuthority {
        HotStateAdmissionAuthority::AccelerationOnly
    }

    pub(super) fn matches_selector(
        self,
        current_context: &HotStateAdmissionContext,
        policy: &HotStatePathPolicy,
        capabilities: &HotStateCapabilityObservation,
    ) -> bool {
        &self.current_context == current_context
            && self.policy_path_class == policy.path_class
            && self.policy_reuse_identity == policy.expected_identity
            && &self.capability_generation == capabilities.generation()
    }
}

impl fmt::Debug for AdmittedHotStateCandidate<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<admitted-hot-state-candidate>")
    }
}

/// Admit one hot-state reuse candidate from family evidence.
///
/// The closed refusal precedence is permanent-forbidden, unique local work, quarantine, exact
/// candidate/current context mismatch, policy mismatch, then physical resource refusal. The
/// returned capability is acceleration-only: it encodes no retain, drain, evict, release,
/// cancellation, or lifetime authority.
///
/// # Errors
///
/// Returns the first bounded refusal in that precedence order.
pub fn admit_family_evidence<'proof>(
    evidence: HotStateFamilyAdmissionEvidence<'proof>,
    current_context: &HotStateAdmissionContext,
    policy: &HotStatePathPolicy,
    capabilities: &HotStateCapabilityObservation,
    resource_disposition: HotStateResourceDisposition,
) -> Result<AdmittedHotStateCandidate<'proof>, HotStateAdmissionRefusal> {
    if let HotStateFamilyStanding::PermanentlyForbidden(reason) = evidence.standing {
        return Err(HotStateAdmissionRefusal::Forbidden { reason });
    }
    if evidence.unique_local_work {
        return Err(HotStateAdmissionRefusal::Forbidden {
            reason: HotStateForbiddenReason::UniqueLocalWork,
        });
    }
    if let HotStateFamilyStanding::Quarantined(reason) = evidence.standing {
        return Err(HotStateAdmissionRefusal::QuarantineRequired { reason });
    }
    if let Some(field) = first_context_mismatch(&evidence.context, current_context) {
        return Err(HotStateAdmissionRefusal::Mismatch { field });
    }
    if let Some(field) = first_policy_mismatch(current_context, policy) {
        return Err(HotStateAdmissionRefusal::Mismatch { field });
    }
    if resource_disposition == HotStateResourceDisposition::Refused {
        return Err(HotStateAdmissionRefusal::ResourceRefused);
    }

    Ok(AdmittedHotStateCandidate {
        current_context: current_context.clone(),
        policy_path_class: policy.path_class.clone(),
        policy_reuse_identity: policy.expected_identity.clone(),
        capability_generation: capabilities.generation().clone(),
        _proof: PhantomData,
        _seal: AdmissionSeal,
    })
}

fn first_context_mismatch(
    candidate: &HotStateAdmissionContext,
    current: &HotStateAdmissionContext,
) -> Option<HotStateAdmissionMismatchField> {
    use HotStateAdmissionMismatchField as Field;

    if candidate.target.family != current.target.family {
        return Some(Field::Family);
    }
    if candidate.target.binding != current.target.binding {
        return Some(Field::Binding);
    }
    if candidate.target.project != current.target.project {
        return Some(Field::Project);
    }
    if candidate.target.path_class != current.target.path_class {
        return Some(Field::PathClass);
    }

    let candidate_identity = &candidate.semantics.reuse_identity;
    let current_identity = &current.semantics.reuse_identity;
    if candidate_identity.source() != current_identity.source() {
        return Some(Field::Source);
    }
    if candidate_identity.toolchain() != current_identity.toolchain() {
        return Some(Field::Toolchain);
    }
    if candidate_identity.trust() != current_identity.trust() {
        return Some(Field::Trust);
    }
    if candidate_identity.policy() != current_identity.policy() {
        return Some(Field::Policy);
    }
    if candidate.semantics.profile != current.semantics.profile {
        return Some(Field::Profile);
    }
    if candidate.semantics.validator != current.semantics.validator {
        return Some(Field::Validator);
    }
    if candidate.semantics.platform != current.semantics.platform {
        return Some(Field::Platform);
    }

    match (&candidate.reusable_state, &current.reusable_state) {
        (HotStateReusableState::SealedImmutable, HotStateReusableState::SealedImmutable) => None,
        (
            HotStateReusableState::LeaseMutable {
                lease_generation: candidate_lease,
                lease_capability_generation: candidate_capability,
            },
            HotStateReusableState::LeaseMutable {
                lease_generation: current_lease,
                lease_capability_generation: current_capability,
            },
        ) => {
            if candidate_lease != current_lease {
                Some(Field::LeaseGeneration)
            } else if candidate_capability != current_capability {
                Some(Field::LeaseCapabilityGeneration)
            } else {
                None
            }
        }
        _ => Some(Field::StateClass),
    }
}

fn first_policy_mismatch(
    current: &HotStateAdmissionContext,
    policy: &HotStatePathPolicy,
) -> Option<HotStateAdmissionMismatchField> {
    use HotStateAdmissionMismatchField as Field;

    if current.target.path_class != policy.path_class {
        return Some(Field::PathClass);
    }
    let current_identity = &current.semantics.reuse_identity;
    let policy_identity = &policy.expected_identity;
    if current_identity.source() != policy_identity.source() {
        return Some(Field::Source);
    }
    if current_identity.toolchain() != policy_identity.toolchain() {
        return Some(Field::Toolchain);
    }
    if current_identity.trust() != policy_identity.trust() {
        return Some(Field::Trust);
    }
    if current_identity.policy() != policy_identity.policy() {
        return Some(Field::Policy);
    }
    None
}

#[cfg(test)]
fn synthetic_evidence<'proof>(
    proof: &'proof (),
    context: HotStateAdmissionContext,
    standing: HotStateFamilyStanding,
    unique_local_work: bool,
) -> HotStateFamilyAdmissionEvidence<'proof> {
    HotStateFamilyAdmissionEvidence::observed(proof, context, standing, unique_local_work)
}

#[cfg(test)]
pub(super) fn admitted_candidate_for_test<'proof>(
    proof: &'proof (),
    candidate_context: HotStateAdmissionContext,
    current_context: &HotStateAdmissionContext,
    policy: &HotStatePathPolicy,
    capabilities: &HotStateCapabilityObservation,
) -> Result<AdmittedHotStateCandidate<'proof>, HotStateAdmissionRefusal> {
    admit_family_evidence(
        synthetic_evidence(
            proof,
            candidate_context,
            HotStateFamilyStanding::Eligible,
            false,
        ),
        current_context,
        policy,
        capabilities,
        HotStateResourceDisposition::Accepted,
    )
}

#[cfg(test)]
mod tests {
    use crate::project_catalog::ProjectIdentity;

    use super::super::HotStateSharingMode;
    use super::*;

    type ContextMutation = fn(&mut HotStateAdmissionContext);

    fn generation(value: &str) -> HotStateGenerationId {
        HotStateGenerationId::parse(value).unwrap()
    }

    fn reuse_identity(
        source: &str,
        toolchain: &str,
        trust: &str,
        policy: &str,
    ) -> HotStateReuseIdentity {
        HotStateReuseIdentity::new(
            generation(source),
            generation(toolchain),
            generation(trust),
            generation(policy),
        )
    }

    fn base_identity() -> HotStateReuseIdentity {
        reuse_identity("source-1", "toolchain-1", "trust-1", "policy-1")
    }

    fn base_target() -> HotStateAdmissionTarget {
        HotStateAdmissionTarget::new(
            HotStateFamilyRef::new(generation("family-1")),
            HotStateBindingRef::new(generation("binding-1")),
            ProjectIdentity::parse("github.com/teamleaderleo/smolrunner").unwrap(),
            HotStatePathClassId::parse("compiler-output").unwrap(),
        )
    }

    fn base_semantics() -> HotStateAdmissionSemantics {
        HotStateAdmissionSemantics::new(
            base_identity(),
            generation("profile-1"),
            generation("validator-1"),
            generation("platform-1"),
        )
    }

    fn sealed_context() -> HotStateAdmissionContext {
        HotStateAdmissionContext::new(
            base_target(),
            base_semantics(),
            HotStateReusableState::SealedImmutable,
        )
    }

    fn mutable_context() -> HotStateAdmissionContext {
        HotStateAdmissionContext::new(
            base_target(),
            base_semantics(),
            HotStateReusableState::LeaseMutable {
                lease_generation: generation("lease-1"),
                lease_capability_generation: generation("lease-capability-1"),
            },
        )
    }

    fn policy_for(path_class: &str, identity: HotStateReuseIdentity) -> HotStatePathPolicy {
        HotStatePathPolicy::new(
            HotStatePathClassId::parse(path_class).unwrap(),
            identity,
            vec![
                HotStateSharingMode::PrivateCow,
                HotStateSharingMode::PrivateEmpty,
            ],
        )
        .unwrap()
    }

    fn base_policy() -> HotStatePathPolicy {
        policy_for("compiler-output", base_identity())
    }

    fn capabilities(generation_id: &str) -> HotStateCapabilityObservation {
        HotStateCapabilityObservation::new(
            HotStateCapabilityGenerationId::parse(generation_id).unwrap(),
            true,
            true,
            true,
            true,
            true,
        )
    }

    #[test]
    fn exact_sealed_immutable_admits() {
        let proof = ();
        let context = sealed_context();
        let policy = base_policy();
        let capabilities = capabilities("capability-1");
        let candidate = admit_family_evidence(
            synthetic_evidence(
                &proof,
                context.clone(),
                HotStateFamilyStanding::Eligible,
                false,
            ),
            &context,
            &policy,
            &capabilities,
            HotStateResourceDisposition::Accepted,
        )
        .unwrap();
        assert_eq!(
            candidate.authority(),
            HotStateAdmissionAuthority::AccelerationOnly
        );
    }

    #[test]
    fn exact_lease_mutable_admits() {
        let proof = ();
        let context = mutable_context();
        let policy = base_policy();
        let capabilities = capabilities("capability-1");
        let candidate = admit_family_evidence(
            synthetic_evidence(
                &proof,
                context.clone(),
                HotStateFamilyStanding::Eligible,
                false,
            ),
            &context,
            &policy,
            &capabilities,
            HotStateResourceDisposition::Accepted,
        )
        .unwrap();
        assert_eq!(
            candidate.authority(),
            HotStateAdmissionAuthority::AccelerationOnly
        );
    }

    #[test]
    fn every_context_field_mismatch_refuses_and_post_mint_drift_misses() {
        use HotStateAdmissionMismatchField as Field;

        let mutations: &[(Field, ContextMutation)] = &[
            (Field::Family, |context| {
                context.target.family = HotStateFamilyRef::new(generation("family-2"));
            }),
            (Field::Binding, |context| {
                context.target.binding = HotStateBindingRef::new(generation("binding-2"));
            }),
            (Field::Project, |context| {
                context.target.project =
                    ProjectIdentity::parse("github.com/teamleaderleo/other").unwrap();
            }),
            (Field::PathClass, |context| {
                context.target.path_class = HotStatePathClassId::parse("source-view").unwrap();
            }),
            (Field::Source, |context| {
                context.semantics.reuse_identity =
                    reuse_identity("source-2", "toolchain-1", "trust-1", "policy-1");
            }),
            (Field::Toolchain, |context| {
                context.semantics.reuse_identity =
                    reuse_identity("source-1", "toolchain-2", "trust-1", "policy-1");
            }),
            (Field::Trust, |context| {
                context.semantics.reuse_identity =
                    reuse_identity("source-1", "toolchain-1", "trust-2", "policy-1");
            }),
            (Field::Policy, |context| {
                context.semantics.reuse_identity =
                    reuse_identity("source-1", "toolchain-1", "trust-1", "policy-2");
            }),
            (Field::Profile, |context| {
                context.semantics.profile = generation("profile-2");
            }),
            (Field::Validator, |context| {
                context.semantics.validator = generation("validator-2");
            }),
            (Field::Platform, |context| {
                context.semantics.platform = generation("platform-2");
            }),
            (Field::StateClass, |context| {
                context.reusable_state = HotStateReusableState::SealedImmutable;
            }),
            (Field::LeaseGeneration, |context| {
                context.reusable_state = HotStateReusableState::LeaseMutable {
                    lease_generation: generation("lease-2"),
                    lease_capability_generation: generation("lease-capability-1"),
                };
            }),
            (Field::LeaseCapabilityGeneration, |context| {
                context.reusable_state = HotStateReusableState::LeaseMutable {
                    lease_generation: generation("lease-1"),
                    lease_capability_generation: generation("lease-capability-2"),
                };
            }),
        ];

        for (expected_field, mutate) in mutations {
            let proof = ();
            let candidate_context = mutable_context();
            let mut current_context = candidate_context.clone();
            mutate(&mut current_context);
            let policy = base_policy();
            let capabilities = capabilities("capability-1");

            let refusal = admit_family_evidence(
                synthetic_evidence(
                    &proof,
                    candidate_context.clone(),
                    HotStateFamilyStanding::Eligible,
                    false,
                ),
                &current_context,
                &policy,
                &capabilities,
                HotStateResourceDisposition::Accepted,
            )
            .unwrap_err();
            assert_eq!(
                refusal,
                HotStateAdmissionRefusal::Mismatch {
                    field: *expected_field
                }
            );

            let admitted = admit_family_evidence(
                synthetic_evidence(
                    &proof,
                    candidate_context.clone(),
                    HotStateFamilyStanding::Eligible,
                    false,
                ),
                &candidate_context,
                &policy,
                &capabilities,
                HotStateResourceDisposition::Accepted,
            )
            .unwrap();
            assert!(!admitted.matches_selector(&current_context, &policy, &capabilities));
        }
    }
    #[test]
    fn policy_path_and_reuse_identity_drift_refuse() {
        use HotStateAdmissionMismatchField as Field;

        let cases = vec![
            (Field::PathClass, policy_for("source-view", base_identity())),
            (
                Field::Source,
                policy_for(
                    "compiler-output",
                    reuse_identity("source-2", "toolchain-1", "trust-1", "policy-1"),
                ),
            ),
            (
                Field::Toolchain,
                policy_for(
                    "compiler-output",
                    reuse_identity("source-1", "toolchain-2", "trust-1", "policy-1"),
                ),
            ),
            (
                Field::Trust,
                policy_for(
                    "compiler-output",
                    reuse_identity("source-1", "toolchain-1", "trust-2", "policy-1"),
                ),
            ),
            (
                Field::Policy,
                policy_for(
                    "compiler-output",
                    reuse_identity("source-1", "toolchain-1", "trust-1", "policy-2"),
                ),
            ),
        ];

        for (expected_field, policy) in cases {
            let proof = ();
            let context = mutable_context();
            let capabilities = capabilities("capability-1");
            let refusal = admit_family_evidence(
                synthetic_evidence(
                    &proof,
                    context.clone(),
                    HotStateFamilyStanding::Eligible,
                    false,
                ),
                &context,
                &policy,
                &capabilities,
                HotStateResourceDisposition::Accepted,
            )
            .unwrap_err();
            assert_eq!(
                refusal,
                HotStateAdmissionRefusal::Mismatch {
                    field: expected_field
                }
            );
        }
    }

    #[test]
    fn permanent_forbidden_laws_are_absorbing() {
        let reasons = [
            HotStateForbiddenReason::CredentialOrSecretDerivedAuthority,
            HotStateForbiddenReason::LiveExecutionAuthorityOrProcess,
            HotStateForbiddenReason::UniqueLocalWork,
            HotStateForbiddenReason::DiagnosticOnlyState,
            HotStateForbiddenReason::CrossTrustWritableState,
            HotStateForbiddenReason::UnsupportedLiveState,
            HotStateForbiddenReason::UnknownPrivacy,
            HotStateForbiddenReason::AuthorityBearingMetadata,
            HotStateForbiddenReason::TaskRecoveryState,
        ];

        for reason in reasons {
            let proof = ();
            let candidate_context = sealed_context();
            let mut current_context = candidate_context.clone();
            current_context.target.project =
                ProjectIdentity::parse("github.com/teamleaderleo/other").unwrap();
            let policy = base_policy();
            let capabilities = capabilities("capability-1");
            let refusal = admit_family_evidence(
                synthetic_evidence(
                    &proof,
                    candidate_context,
                    HotStateFamilyStanding::PermanentlyForbidden(reason),
                    false,
                ),
                &current_context,
                &policy,
                &capabilities,
                HotStateResourceDisposition::Refused,
            )
            .unwrap_err();
            assert_eq!(refusal, HotStateAdmissionRefusal::Forbidden { reason });
        }
    }

    #[test]
    fn quarantine_laws_are_absorbing() {
        let reasons = [
            HotStateQuarantineReason::UnknownProducer,
            HotStateQuarantineReason::UnknownVersion,
            HotStateQuarantineReason::UnknownWriterHistory,
            HotStateQuarantineReason::ActiveOrUnsettledDescendants,
            HotStateQuarantineReason::IncompletePublication,
            HotStateQuarantineReason::RebindAmbiguous,
            HotStateQuarantineReason::MissingCanonicalParent,
            HotStateQuarantineReason::RestoredUnobserved,
            HotStateQuarantineReason::IdentityOrIntegrityAmbiguous,
            HotStateQuarantineReason::OwnerAmbiguous,
            HotStateQuarantineReason::ModeAmbiguous,
            HotStateQuarantineReason::SameNameAmbiguous,
        ];

        for reason in reasons {
            let proof = ();
            let candidate_context = sealed_context();
            let mut current_context = candidate_context.clone();
            current_context.target.project =
                ProjectIdentity::parse("github.com/teamleaderleo/other").unwrap();
            let policy = base_policy();
            let capabilities = capabilities("capability-1");
            let refusal = admit_family_evidence(
                synthetic_evidence(
                    &proof,
                    candidate_context,
                    HotStateFamilyStanding::Quarantined(reason),
                    false,
                ),
                &current_context,
                &policy,
                &capabilities,
                HotStateResourceDisposition::Refused,
            )
            .unwrap_err();
            assert_eq!(
                refusal,
                HotStateAdmissionRefusal::QuarantineRequired { reason }
            );
        }
    }

    #[test]
    fn unique_local_work_uses_permanent_forbidden_route() {
        let proof = ();
        let candidate_context = sealed_context();
        let mut current_context = candidate_context.clone();
        current_context.target.project =
            ProjectIdentity::parse("github.com/teamleaderleo/other").unwrap();
        let policy = base_policy();
        let capabilities = capabilities("capability-1");
        let refusal = admit_family_evidence(
            synthetic_evidence(
                &proof,
                candidate_context,
                HotStateFamilyStanding::Quarantined(HotStateQuarantineReason::RebindAmbiguous),
                true,
            ),
            &current_context,
            &policy,
            &capabilities,
            HotStateResourceDisposition::Refused,
        )
        .unwrap_err();
        assert_eq!(
            refusal,
            HotStateAdmissionRefusal::Forbidden {
                reason: HotStateForbiddenReason::UniqueLocalWork
            }
        );
    }

    #[test]
    fn resource_refusal_declines_consumption_after_exact_semantic_match() {
        let proof = ();
        let context = sealed_context();
        let policy = base_policy();
        let capabilities = capabilities("capability-1");
        let refusal = admit_family_evidence(
            synthetic_evidence(
                &proof,
                context.clone(),
                HotStateFamilyStanding::Eligible,
                false,
            ),
            &context,
            &policy,
            &capabilities,
            HotStateResourceDisposition::Refused,
        )
        .unwrap_err();
        assert_eq!(refusal, HotStateAdmissionRefusal::ResourceRefused);
    }

    #[test]
    fn capability_generation_drift_invalidates_admission() {
        let proof = ();
        let context = sealed_context();
        let policy = base_policy();
        let admitted_capabilities = capabilities("capability-1");
        let current_capabilities = capabilities("capability-2");
        let candidate = admit_family_evidence(
            synthetic_evidence(
                &proof,
                context.clone(),
                HotStateFamilyStanding::Eligible,
                false,
            ),
            &context,
            &policy,
            &admitted_capabilities,
            HotStateResourceDisposition::Accepted,
        )
        .unwrap();
        assert!(!candidate.matches_selector(&context, &policy, &current_capabilities));
    }

    #[test]
    fn debug_output_is_redacted() {
        let proof = ();
        let context = mutable_context();
        let policy = base_policy();
        let capabilities = capabilities("capability-1");
        let evidence = synthetic_evidence(
            &proof,
            context.clone(),
            HotStateFamilyStanding::Eligible,
            false,
        );

        assert_eq!(
            format!("{:?}", context.target.family),
            "<opaque-hot-state-family-ref>"
        );
        assert_eq!(
            format!("{:?}", context.target.binding),
            "<opaque-hot-state-binding-ref>"
        );
        assert_eq!(
            format!("{:?}", context.target),
            "<opaque-hot-state-admission-target>"
        );
        assert_eq!(
            format!("{:?}", context.semantics),
            "<opaque-hot-state-admission-semantics>"
        );
        assert_eq!(
            format!("{context:?}"),
            "<opaque-hot-state-admission-context>"
        );
        assert_eq!(
            format!("{evidence:?}"),
            "<opaque-hot-state-family-admission-evidence>"
        );

        let candidate = admit_family_evidence(
            evidence,
            &context,
            &policy,
            &capabilities,
            HotStateResourceDisposition::Accepted,
        )
        .unwrap();
        assert_eq!(format!("{candidate:?}"), "<admitted-hot-state-candidate>");
    }
}
