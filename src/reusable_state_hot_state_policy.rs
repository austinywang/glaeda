//! Binding between the reusable-state identity contract and the hot-state path-class policy.
//!
//! `reusable_state_lifecycle` decides whether a published generation is semantically the state a
//! consumer asked for. It does not decide how those bytes may be shared. This module supplies the
//! missing half: it derives one `HotStateAdmissionContext` from each side of the same
//! `ReusableStateIdentityContract`, offers the publisher's side as family evidence, and asks
//! `HotStatePathPolicy::select` for the reviewed sharing mode.
//!
//! Every ladder dimension except trust is one field of the contract, observed independently on the
//! publisher and consumer sides. Trust is pinned to `REUSABLE_STATE_TRUST_GENERATION` because the
//! contract records no per-generation trust generation; consumer trust stays enforced by
//! `reusable_state_lifecycle`'s own low-trust/lifecycle gate, which runs before this one.
//!
//! The module performs no filesystem, mount, publication, retention, eviction, or physical
//! resource action. The returned receipt is acceleration-only: it authorizes a sharing mechanism
//! for already-admitted bytes and grants no retain, drain, evict, release, or lifetime authority.

use std::fmt;

use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::artifact::Sha256Digest;
use crate::hot_state_path_policy::{
    HotStateAdmissionContext, HotStateAdmissionRefusal, HotStateAdmissionSemantics,
    HotStateAdmissionTarget, HotStateBindingRef, HotStateCapabilityObservation,
    HotStateFamilyAdmissionEvidence, HotStateFamilyRef, HotStateFamilyStanding,
    HotStateGenerationId, HotStatePathClassId, HotStatePathPolicy, HotStatePublisherRole,
    HotStateQuarantineReason, HotStateResourceDisposition, HotStateReusableState,
    HotStateReuseIdentity, HotStateSelection, HotStateSelectionReceipt, HotStateSharingMode,
    admit_family_evidence,
};
use crate::project_catalog::ProjectIdentity;
use crate::reusable_state_lifecycle::{
    ReusableStateGeneration, ReusableStateIdentityContract, ReusableStateIntegrityState,
    ReusableStatePublicationState,
};

pub const REUSABLE_STATE_HOT_STATE_POLICY_SCHEMA_VERSION: u8 = 1;

/// Pinned trust generation for read-only reusable-state consumption.
///
/// The identity contract records no per-generation trust generation, so this rung is equal on both
/// sides and cannot refuse on its own. Bump it when the reviewed read-only consumption trust model
/// changes; every admission minted under the previous token then fails closed.
pub const REUSABLE_STATE_TRUST_GENERATION: &str = "reusable-state-read-only-consumer-trust-v1";

const PROJECT_HOST_PREFIX: &str = "github.com/";
const SHA256_PREFIX: &str = "sha256:";
const HEX: &[u8; 16] = b"0123456789abcdef";
const ABSENT_DOMAIN: &[u8] = b"glaeda-reusable-state-hot-state-absent-v1\0";
const PLATFORM_DOMAIN: &[u8] = b"glaeda-reusable-state-hot-state-platform-v1\0";
const VALIDATOR_DOMAIN: &[u8] = b"glaeda-reusable-state-hot-state-validator-v1\0";

/// Reviewed sharing modes for every reusable-state path class.
///
/// `docs/REUSABLE_STATE_LIFECYCLE.md` reports consumption as `read_only`, so the only reuse mode
/// this layer can authorize is an immutable shared lower. `private_empty` is the fallback and
/// means the consumer must reconstruct, which is exactly the lifecycle contract's miss/reset.
const REVIEWED_MODES: [HotStateSharingMode; 2] = [
    HotStateSharingMode::ImmutableOverlay,
    HotStateSharingMode::PrivateEmpty,
];

/// Outcome of one reusable-state hot-state reuse decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReusableStateHotStateDecision {
    schema_version: u8,
    receipt: HotStateSelectionReceipt,
    #[serde(skip_serializing_if = "Option::is_none")]
    refusal_code: Option<&'static str>,
    #[serde(skip)]
    refusal: Option<HotStateAdmissionRefusal>,
}

impl ReusableStateHotStateDecision {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn receipt(&self) -> &HotStateSelectionReceipt {
        &self.receipt
    }

    /// The first bounded admission refusal, when the candidate was not admitted.
    #[must_use]
    pub const fn refusal(&self) -> Option<HotStateAdmissionRefusal> {
        self.refusal
    }

    #[must_use]
    pub const fn refusal_code(&self) -> Option<&'static str> {
        self.refusal_code
    }

    /// Whether the published bytes may back this consumer.
    #[must_use]
    pub const fn reuse_admitted(&self) -> bool {
        matches!(
            self.receipt.selection(),
            HotStateSelection::Selected {
                reused_state: true,
                ..
            }
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ReusableStateHotStatePolicyError {
    code: &'static str,
    message: &'static str,
}

impl ReusableStateHotStatePolicyError {
    #[must_use]
    pub const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Display for ReusableStateHotStatePolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for ReusableStateHotStatePolicyError {}

/// Select the reviewed sharing mode for one read-only reusable-state consumption attempt.
///
/// `required` is the contract the consumer needs; `generation` is the published state it would
/// reuse. Both sides of every ladder dimension come from their own contract, so a drifted
/// publisher and a drifted consumer are equally refused. Absent admission, a stale capability
/// generation, or an unsupported sharing mode all fall through to `private_empty`, which means no
/// reuse.
///
/// # Errors
///
/// Returns a bounded error when the consumer's contract cannot be expressed as a hot-state
/// context at all — for instance a repository reference outside the canonical
/// `github.com/owner/repository` project form. Callers must treat that as a refusal to reuse.
pub fn select_reusable_state_hot_state(
    required: &ReusableStateIdentityContract,
    generation: &ReusableStateGeneration,
    capabilities: &HotStateCapabilityObservation,
) -> Result<ReusableStateHotStateDecision, ReusableStateHotStatePolicyError> {
    let current_context = admission_context(required)?;
    let candidate_context = admission_context(generation.identity())?;
    let policy = path_policy(required)?;

    let evidence = HotStateFamilyAdmissionEvidence::observed(
        generation,
        candidate_context,
        family_standing(generation),
        generation.retention().unique_local_work,
    );

    let admission = admit_family_evidence(
        evidence,
        &current_context,
        &policy,
        capabilities,
        // This layer performs no physical consumption: the family executor owns resource
        // admission and must re-check it immediately before it opens the bytes.
        HotStateResourceDisposition::Accepted,
    );

    let (candidate, refusal) = match admission {
        Ok(candidate) => (Some(candidate), None),
        Err(refusal) => (None, Some(refusal)),
    };

    let receipt = policy
        .select(
            &current_context,
            candidate,
            capabilities,
            HotStatePublisherRole::ConsumerOnly,
        )
        .map_err(|_| invalid_context())?;

    Ok(ReusableStateHotStateDecision {
        schema_version: REUSABLE_STATE_HOT_STATE_POLICY_SCHEMA_VERSION,
        receipt,
        refusal_code: refusal.map(HotStateAdmissionRefusal::code),
        refusal,
    })
}

/// The reviewed hot-state path policy for one reusable-state contract's path class.
///
/// # Errors
///
/// Returns a bounded error when the contract cannot be expressed as a hot-state path class or
/// reuse identity.
pub fn path_policy(
    required: &ReusableStateIdentityContract,
) -> Result<HotStatePathPolicy, ReusableStateHotStatePolicyError> {
    HotStatePathPolicy::new(
        path_class(required)?,
        reuse_identity(required)?,
        REVIEWED_MODES.to_vec(),
    )
    .map_err(|_| invalid_context())
}

fn admission_context(
    contract: &ReusableStateIdentityContract,
) -> Result<HotStateAdmissionContext, ReusableStateHotStatePolicyError> {
    let target = HotStateAdmissionTarget::new(
        HotStateFamilyRef::new(token(&contract.family_inputs_digest)?),
        HotStateBindingRef::new(optional_token(
            contract.prepared_environment_generation.as_ref(),
        )?),
        project(contract)?,
        path_class(contract)?,
    );
    let semantics = HotStateAdmissionSemantics::new(
        reuse_identity(contract)?,
        optional_token(contract.compiler_flags_digest.as_ref())?,
        validator_token(contract)?,
        platform_token(contract)?,
    );
    // Published reusable state is consumed read-only, so it is a sealed immutable parent. A lease
    // -mutable candidate can never compare equal to it and is refused on the state class.
    Ok(HotStateAdmissionContext::new(
        target,
        semantics,
        HotStateReusableState::SealedImmutable,
    ))
}

fn reuse_identity(
    contract: &ReusableStateIdentityContract,
) -> Result<HotStateReuseIdentity, ReusableStateHotStatePolicyError> {
    Ok(HotStateReuseIdentity::new(
        optional_token(contract.dependency_lock_digest.as_ref())?,
        token(&contract.toolchain_generation)?,
        HotStateGenerationId::parse(REUSABLE_STATE_TRUST_GENERATION)
            .map_err(|_| invalid_context())?,
        optional_token(contract.build_configuration_digest.as_ref())?,
    ))
}

fn path_class(
    contract: &ReusableStateIdentityContract,
) -> Result<HotStatePathClassId, ReusableStateHotStatePolicyError> {
    HotStatePathClassId::parse(contract.state_class.as_str()).map_err(|_| invalid_context())
}

fn project(
    contract: &ReusableStateIdentityContract,
) -> Result<ProjectIdentity, ReusableStateHotStatePolicyError> {
    let canonical = format!("{PROJECT_HOST_PREFIX}{}", contract.repository.as_str());
    ProjectIdentity::parse(&canonical).map_err(|_| invalid_project())
}

fn validator_token(
    contract: &ReusableStateIdentityContract,
) -> Result<HotStateGenerationId, ReusableStateHotStatePolicyError> {
    composed(
        VALIDATOR_DOMAIN,
        &[
            &[contract.schema_version],
            contract.cache_schema_generation.as_str().as_bytes(),
        ],
    )
}

fn platform_token(
    contract: &ReusableStateIdentityContract,
) -> Result<HotStateGenerationId, ReusableStateHotStatePolicyError> {
    composed(
        PLATFORM_DOMAIN,
        &[
            contract.architecture.as_str().as_bytes(),
            contract.os_runtime_generation.as_str().as_bytes(),
        ],
    )
}

fn token(digest: &Sha256Digest) -> Result<HotStateGenerationId, ReusableStateHotStatePolicyError> {
    HotStateGenerationId::parse(digest.as_str()).map_err(|_| invalid_context())
}

/// An absent optional input is its own domain-separated token, so `Some`/`None` drift refuses.
fn optional_token(
    digest: Option<&Sha256Digest>,
) -> Result<HotStateGenerationId, ReusableStateHotStatePolicyError> {
    match digest {
        Some(value) => token(value),
        None => composed(ABSENT_DOMAIN, &[]),
    }
}

fn composed(
    domain: &[u8],
    parts: &[&[u8]],
) -> Result<HotStateGenerationId, ReusableStateHotStatePolicyError> {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    for part in parts {
        let length = u32::try_from(part.len()).map_err(|_| invalid_context())?;
        hasher.update(length.to_be_bytes());
        hasher.update(part);
    }
    let bytes = hasher.finalize();
    let mut value = String::with_capacity(SHA256_PREFIX.len() + bytes.len() * 2);
    value.push_str(SHA256_PREFIX);
    for byte in bytes.as_slice() {
        value.push(char::from(HEX[usize::from(byte >> 4)]));
        value.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    HotStateGenerationId::parse(&value).map_err(|_| invalid_context())
}

/// Map the publisher-side lifecycle facts onto the closed hot-state standing vocabulary.
///
/// `reusable_state_lifecycle` refuses every one of these before it reports a hit, so this mapping
/// only has to be at least as strict. It exists so a direct caller of this module fails closed
/// without that pre-check.
fn family_standing(generation: &ReusableStateGeneration) -> HotStateFamilyStanding {
    match generation.publication() {
        ReusableStatePublicationState::Complete => {}
        ReusableStatePublicationState::Partial
        | ReusableStatePublicationState::DiskFull
        | ReusableStatePublicationState::ProducerCrashed
        | ReusableStatePublicationState::ConcurrentPublisherConflict => {
            return HotStateFamilyStanding::Quarantined(
                HotStateQuarantineReason::IncompletePublication,
            );
        }
    }
    match generation.integrity() {
        ReusableStateIntegrityState::Verified => {}
        ReusableStateIntegrityState::Truncated | ReusableStateIntegrityState::Corrupt => {
            return HotStateFamilyStanding::Quarantined(
                HotStateQuarantineReason::IdentityOrIntegrityAmbiguous,
            );
        }
    }
    if generation.revalidation_required() {
        return HotStateFamilyStanding::Quarantined(HotStateQuarantineReason::RestoredUnobserved);
    }
    HotStateFamilyStanding::Eligible
}

const fn error(code: &'static str, message: &'static str) -> ReusableStateHotStatePolicyError {
    ReusableStateHotStatePolicyError { code, message }
}

const fn invalid_context() -> ReusableStateHotStatePolicyError {
    error(
        "reusable_state_hot_state_context_invalid",
        "reusable-state identity contract cannot be expressed as a hot-state admission context",
    )
}

const fn invalid_project() -> ReusableStateHotStatePolicyError {
    error(
        "reusable_state_hot_state_project_invalid",
        "reusable-state repository is not one canonical github.com/owner/repository project",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reusable_state_lifecycle::{ReusableStateArchitecture, ReusableStateClass};

    fn digest(fill: char) -> Sha256Digest {
        Sha256Digest::parse(&format!("sha256:{}", String::from(fill).repeat(64))).unwrap()
    }

    fn contract() -> ReusableStateIdentityContract {
        ReusableStateIdentityContract::new(
            ReusableStateClass::CompilerCache,
            crate::artifact::RepositoryRef::parse("teamleaderleo/glaeda").unwrap(),
            ReusableStateArchitecture::parse("aarch64").unwrap(),
            digest('1'),
            digest('2'),
            None,
            None,
            None,
            None,
            digest('7'),
            digest('8'),
        )
    }

    #[test]
    fn every_reviewed_path_class_derives_one_bounded_context() {
        for state_class in [
            ReusableStateClass::PackageManagerState,
            ReusableStateClass::CompilerCache,
            ReusableStateClass::IncrementalBuildState,
            ReusableStateClass::ImmutableCompiledProduct,
            ReusableStateClass::ContainerLayer,
            ReusableStateClass::PreparedDependencyGeneration,
            ReusableStateClass::ProjectLocalApprovedHotState,
        ] {
            let mut required = contract();
            required.state_class = state_class;
            admission_context(&required).expect("derivable context");
            path_policy(&required).expect("derivable policy");
        }
    }

    #[test]
    fn absent_and_present_optional_inputs_derive_distinct_tokens() {
        let absent = optional_token(None).unwrap();
        let present = optional_token(Some(&digest('3'))).unwrap();
        assert_ne!(absent, present);
        assert_eq!(absent, optional_token(None).unwrap());
    }

    #[test]
    fn platform_and_validator_tokens_are_domain_separated() {
        let mut shifted = contract();
        shifted.architecture = ReusableStateArchitecture::parse("x86-64").unwrap();
        assert_ne!(
            platform_token(&contract()).unwrap(),
            platform_token(&shifted).unwrap()
        );
        assert_ne!(
            platform_token(&contract()).unwrap(),
            validator_token(&contract()).unwrap()
        );
    }
}
