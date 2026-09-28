use std::collections::BTreeSet;
use std::fmt;

use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::artifact::Sha256Digest;

mod admission;
pub use admission::{
    AdmittedHotStateCandidate, HotStateAdmissionAuthority, HotStateAdmissionContext,
    HotStateAdmissionMismatchField, HotStateAdmissionRefusal, HotStateAdmissionSemantics,
    HotStateAdmissionTarget, HotStateBindingRef, HotStateFamilyAdmissionEvidence,
    HotStateFamilyRef, HotStateFamilyStanding, HotStateForbiddenReason, HotStateQuarantineReason,
    HotStateResourceDisposition, HotStateReusableState, admit_family_evidence,
};

pub const HOT_STATE_PATH_POLICY_SCHEMA_VERSION: u8 = 1;
const MAX_IDENTIFIER_BYTES: usize = 96;
const MAX_POLICY_MODES: usize = 4;
const IDENTITY_DIGEST_DOMAIN: &[u8] = b"smolrunner-hot-state-reuse-identity-v1\0";
const SHA256_PREFIX: &str = "sha256:";
const HEX: &[u8; 16] = b"0123456789abcdef";

macro_rules! identifier_type {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Parse one bounded public identity token.
            ///
            /// # Errors
            ///
            /// Returns a bounded error for an empty, oversized, or non-canonical token.
            pub fn parse(value: &str) -> Result<Self, HotStatePolicyError> {
                validate_identifier(value)?;
                Ok(Self(value.to_owned()))
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

identifier_type!(HotStatePathClassId);
identifier_type!(HotStateGenerationId);
identifier_type!(HotStateCapabilityGenerationId);

#[derive(Clone, PartialEq, Eq)]
pub struct HotStateReuseIdentity {
    source: HotStateGenerationId,
    toolchain: HotStateGenerationId,
    trust: HotStateGenerationId,
    policy: HotStateGenerationId,
}

impl HotStateReuseIdentity {
    #[must_use]
    pub const fn new(
        source: HotStateGenerationId,
        toolchain: HotStateGenerationId,
        trust: HotStateGenerationId,
        policy: HotStateGenerationId,
    ) -> Self {
        Self {
            source,
            toolchain,
            trust,
            policy,
        }
    }

    #[must_use]
    pub const fn source(&self) -> &HotStateGenerationId {
        &self.source
    }

    #[must_use]
    pub const fn toolchain(&self) -> &HotStateGenerationId {
        &self.toolchain
    }

    #[must_use]
    pub const fn trust(&self) -> &HotStateGenerationId {
        &self.trust
    }

    #[must_use]
    pub const fn policy(&self) -> &HotStateGenerationId {
        &self.policy
    }

    /// Return the path-private digest used by selection receipts.
    ///
    /// # Errors
    ///
    /// Returns a bounded error only if the canonical digest cannot be represented by the shared
    /// digest type.
    pub fn digest(&self) -> Result<Sha256Digest, HotStatePolicyError> {
        let mut hasher = Sha256::new();
        hasher.update(IDENTITY_DIGEST_DOMAIN);
        for value in [&self.source, &self.toolchain, &self.trust, &self.policy] {
            let bytes = value.as_str().as_bytes();
            let length = u32::try_from(bytes.len()).map_err(|_| invalid_identifier())?;
            hasher.update(length.to_be_bytes());
            hasher.update(bytes);
        }
        digest_to_sha256(hasher.finalize().as_slice())
    }
}

impl fmt::Debug for HotStateReuseIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<opaque-hot-state-reuse-identity>")
    }
}

impl Serialize for HotStateReuseIdentity {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str("<opaque-hot-state-reuse-identity>")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HotStateSharingMode {
    ImmutableOverlay,
    PrivateCow,
    PrivateEmpty,
    ReviewedSharedMutable,
}

impl HotStateSharingMode {
    #[must_use]
    pub const fn reuses_existing_bytes(self) -> bool {
        !matches!(self, Self::PrivateEmpty)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HotStatePublisherRole {
    ConsumerOnly,
    PublisherEnabled,
}

#[derive(Clone, PartialEq, Eq)]
pub struct HotStateCapabilityObservation {
    generation: HotStateCapabilityGenerationId,
    overlay_available: bool,
    private_cow_available: bool,
    private_empty_available: bool,
    reviewed_shared_mutable_available: bool,
    shared_mutable_publisher_authority: bool,
}

impl HotStateCapabilityObservation {
    #[must_use]
    pub const fn new(
        generation: HotStateCapabilityGenerationId,
        overlay_available: bool,
        private_cow_available: bool,
        private_empty_available: bool,
        reviewed_shared_mutable_available: bool,
        shared_mutable_publisher_authority: bool,
    ) -> Self {
        Self {
            generation,
            overlay_available,
            private_cow_available,
            private_empty_available,
            reviewed_shared_mutable_available,
            shared_mutable_publisher_authority,
        }
    }

    #[must_use]
    pub const fn generation(&self) -> &HotStateCapabilityGenerationId {
        &self.generation
    }

    fn supports(&self, mode: HotStateSharingMode, role: HotStatePublisherRole) -> bool {
        match mode {
            HotStateSharingMode::ImmutableOverlay => self.overlay_available,
            HotStateSharingMode::PrivateCow => self.private_cow_available,
            HotStateSharingMode::PrivateEmpty => self.private_empty_available,
            HotStateSharingMode::ReviewedSharedMutable => {
                self.reviewed_shared_mutable_available
                    && (role == HotStatePublisherRole::ConsumerOnly
                        || self.shared_mutable_publisher_authority)
            }
        }
    }
}

impl fmt::Debug for HotStateCapabilityObservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HotStateCapabilityObservation")
            .field("generation", &self.generation)
            .field("overlay_available", &self.overlay_available)
            .field("private_cow_available", &self.private_cow_available)
            .field("private_empty_available", &self.private_empty_available)
            .field(
                "reviewed_shared_mutable_available",
                &self.reviewed_shared_mutable_available,
            )
            .field(
                "shared_mutable_publisher_authority",
                &self.shared_mutable_publisher_authority,
            )
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotStatePathPolicy {
    schema_version: u8,
    path_class: HotStatePathClassId,
    expected_identity: HotStateReuseIdentity,
    modes: Vec<HotStateSharingMode>,
}

impl HotStatePathPolicy {
    /// Create one reviewed ordered policy for a logical path class.
    ///
    /// # Errors
    ///
    /// Returns a bounded error for an empty/oversized mode list or duplicate modes.
    pub fn new(
        path_class: HotStatePathClassId,
        expected_identity: HotStateReuseIdentity,
        modes: Vec<HotStateSharingMode>,
    ) -> Result<Self, HotStatePolicyError> {
        if modes.is_empty() || modes.len() > MAX_POLICY_MODES {
            return Err(invalid_mode_list());
        }
        let unique = modes.iter().copied().collect::<BTreeSet<_>>();
        if unique.len() != modes.len() {
            return Err(duplicate_mode());
        }
        Ok(Self {
            schema_version: HOT_STATE_PATH_POLICY_SCHEMA_VERSION,
            path_class,
            expected_identity,
            modes,
        })
    }

    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn path_class(&self) -> &HotStatePathClassId {
        &self.path_class
    }

    #[must_use]
    pub fn modes(&self) -> &[HotStateSharingMode] {
        &self.modes
    }

    /// Select the first reviewed sharing mode supported by current capabilities.
    ///
    /// Reused modes require one opaque admission that remains exact for the current context,
    /// policy path class/reuse identity, and capability-observation generation.
    /// `private_empty` remains the safe fallback when admission is absent or stale.
    ///
    /// # Errors
    ///
    /// Returns a bounded error only if the policy identity digest cannot be represented.
    pub fn select(
        &self,
        current_context: &HotStateAdmissionContext,
        admitted_candidate: Option<AdmittedHotStateCandidate<'_>>,
        capabilities: &HotStateCapabilityObservation,
        publisher_role: HotStatePublisherRole,
    ) -> Result<HotStateSelectionReceipt, HotStatePolicyError> {
        let identity_matches = admitted_candidate.is_some_and(|candidate| {
            candidate.matches_selector(current_context, self, capabilities)
        });
        let mut selected = None;
        for mode in &self.modes {
            if mode.reuses_existing_bytes() && !identity_matches {
                continue;
            }
            if capabilities.supports(*mode, publisher_role) {
                selected = Some(*mode);
                break;
            }
        }
        let selection = match selected {
            Some(mode) => HotStateSelection::Selected {
                mode,
                reused_state: mode.reuses_existing_bytes(),
            },
            None => HotStateSelection::Unavailable,
        };
        Ok(HotStateSelectionReceipt {
            schema_version: HOT_STATE_PATH_POLICY_SCHEMA_VERSION,
            path_class: self.path_class.clone(),
            expected_identity_digest: self.expected_identity.digest()?,
            candidate_identity_match: identity_matches,
            capability_generation: capabilities.generation.clone(),
            publisher_role,
            selection,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum HotStateSelection {
    Selected {
        mode: HotStateSharingMode,
        reused_state: bool,
    },
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HotStateSelectionReceipt {
    schema_version: u8,
    path_class: HotStatePathClassId,
    expected_identity_digest: Sha256Digest,
    candidate_identity_match: bool,
    capability_generation: HotStateCapabilityGenerationId,
    publisher_role: HotStatePublisherRole,
    selection: HotStateSelection,
}

impl HotStateSelectionReceipt {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn path_class(&self) -> &HotStatePathClassId {
        &self.path_class
    }

    #[must_use]
    pub const fn expected_identity_digest(&self) -> &Sha256Digest {
        &self.expected_identity_digest
    }

    #[must_use]
    pub const fn candidate_identity_match(&self) -> bool {
        self.candidate_identity_match
    }

    #[must_use]
    pub const fn capability_generation(&self) -> &HotStateCapabilityGenerationId {
        &self.capability_generation
    }

    #[must_use]
    pub const fn publisher_role(&self) -> HotStatePublisherRole {
        self.publisher_role
    }

    #[must_use]
    pub const fn selection(&self) -> HotStateSelection {
        self.selection
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HotStatePolicyError {
    code: &'static str,
    message: &'static str,
}

impl HotStatePolicyError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Display for HotStatePolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for HotStatePolicyError {}

fn validate_identifier(value: &str) -> Result<(), HotStatePolicyError> {
    let Some(first) = value.bytes().next() else {
        return Err(invalid_identifier());
    };
    if value.len() > MAX_IDENTIFIER_BYTES
        || !(first.is_ascii_lowercase() || first.is_ascii_digit())
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'-' | b'_' | b'.' | b':')
        })
    {
        return Err(invalid_identifier());
    }
    Ok(())
}

fn digest_to_sha256(bytes: &[u8]) -> Result<Sha256Digest, HotStatePolicyError> {
    let mut value = String::with_capacity(SHA256_PREFIX.len() + bytes.len() * 2);
    value.push_str(SHA256_PREFIX);
    for byte in bytes {
        value.push(char::from(HEX[usize::from(byte >> 4)]));
        value.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    Sha256Digest::parse(&value).map_err(|_| invalid_digest())
}

const fn error(code: &'static str, message: &'static str) -> HotStatePolicyError {
    HotStatePolicyError { code, message }
}

const fn invalid_identifier() -> HotStatePolicyError {
    error(
        "hot_state_identifier_invalid",
        "hot-state identifier must be one bounded canonical ASCII token",
    )
}

const fn invalid_mode_list() -> HotStatePolicyError {
    error(
        "hot_state_policy_modes_invalid",
        "hot-state path policy must contain one to four reviewed sharing modes",
    )
}

const fn duplicate_mode() -> HotStatePolicyError {
    error(
        "hot_state_policy_mode_duplicate",
        "hot-state path policy cannot contain duplicate sharing modes",
    )
}

const fn invalid_digest() -> HotStatePolicyError {
    error(
        "hot_state_identity_digest_invalid",
        "hot-state reuse identity digest is invalid",
    )
}

#[cfg(test)]
mod tests {
    use crate::project_catalog::ProjectIdentity;

    use super::{
        AdmittedHotStateCandidate, HotStateAdmissionContext, HotStateAdmissionMismatchField,
        HotStateAdmissionRefusal, HotStateAdmissionSemantics, HotStateAdmissionTarget,
        HotStateBindingRef, HotStateCapabilityGenerationId, HotStateCapabilityObservation,
        HotStateFamilyRef, HotStateGenerationId, HotStatePathClassId, HotStatePathPolicy,
        HotStatePublisherRole, HotStateReusableState, HotStateReuseIdentity, HotStateSelection,
        HotStateSelectionReceipt, HotStateSharingMode,
    };

    fn generation(value: &str) -> HotStateGenerationId {
        HotStateGenerationId::parse(value).unwrap()
    }

    fn identity(suffix: &str) -> HotStateReuseIdentity {
        HotStateReuseIdentity::new(
            generation(&format!("source-{suffix}")),
            generation(&format!("toolchain-{suffix}")),
            generation(&format!("trust-{suffix}")),
            generation(&format!("policy-{suffix}")),
        )
    }

    fn capabilities(
        overlay: bool,
        cow: bool,
        empty: bool,
        shared: bool,
        publisher: bool,
    ) -> HotStateCapabilityObservation {
        HotStateCapabilityObservation::new(
            HotStateCapabilityGenerationId::parse("capability-1").unwrap(),
            overlay,
            cow,
            empty,
            shared,
            publisher,
        )
    }

    fn admission_context_for_project(
        path_class: &str,
        reuse_identity: HotStateReuseIdentity,
        project: &str,
    ) -> HotStateAdmissionContext {
        HotStateAdmissionContext::new(
            HotStateAdmissionTarget::new(
                HotStateFamilyRef::new(generation("family-1")),
                HotStateBindingRef::new(generation("binding-1")),
                ProjectIdentity::parse(project).unwrap(),
                HotStatePathClassId::parse(path_class).unwrap(),
            ),
            HotStateAdmissionSemantics::new(
                reuse_identity,
                generation("profile-1"),
                generation("validator-1"),
                generation("platform-1"),
            ),
            HotStateReusableState::SealedImmutable,
        )
    }

    fn admission_context(
        path_class: &str,
        reuse_identity: HotStateReuseIdentity,
    ) -> HotStateAdmissionContext {
        admission_context_for_project(
            path_class,
            reuse_identity,
            "github.com/teamleaderleo/smolrunner",
        )
    }

    fn admitted_candidate<'proof>(
        proof: &'proof (),
        context: &HotStateAdmissionContext,
        policy: &HotStatePathPolicy,
        capabilities: &HotStateCapabilityObservation,
    ) -> AdmittedHotStateCandidate<'proof> {
        super::admission::admitted_candidate_for_test(
            proof,
            context.clone(),
            context,
            policy,
            capabilities,
        )
        .unwrap()
    }

    fn select_admitted(
        policy: &HotStatePathPolicy,
        context: &HotStateAdmissionContext,
        capabilities: &HotStateCapabilityObservation,
        publisher_role: HotStatePublisherRole,
    ) -> HotStateSelectionReceipt {
        let proof = ();
        let candidate = admitted_candidate(&proof, context, policy, capabilities);
        policy
            .select(context, Some(candidate), capabilities, publisher_role)
            .unwrap()
    }

    #[test]
    fn source_policy_uses_overlay_then_cow_then_empty_in_reviewed_order() {
        let expected = identity("a");
        let context = admission_context("source-view", expected.clone());
        let policy = HotStatePathPolicy::new(
            HotStatePathClassId::parse("source-view").unwrap(),
            expected.clone(),
            vec![
                HotStateSharingMode::ImmutableOverlay,
                HotStateSharingMode::PrivateCow,
                HotStateSharingMode::PrivateEmpty,
            ],
        )
        .unwrap();

        let overlay = select_admitted(
            &policy,
            &context,
            &capabilities(true, true, true, false, false),
            HotStatePublisherRole::ConsumerOnly,
        );
        assert_eq!(
            overlay.selection(),
            HotStateSelection::Selected {
                mode: HotStateSharingMode::ImmutableOverlay,
                reused_state: true
            }
        );

        let cow = select_admitted(
            &policy,
            &context,
            &capabilities(false, true, true, false, false),
            HotStatePublisherRole::ConsumerOnly,
        );
        assert_eq!(
            cow.selection(),
            HotStateSelection::Selected {
                mode: HotStateSharingMode::PrivateCow,
                reused_state: true
            }
        );

        let empty = select_admitted(
            &policy,
            &context,
            &capabilities(false, false, true, false, false),
            HotStatePublisherRole::ConsumerOnly,
        );
        assert_eq!(
            empty.selection(),
            HotStateSelection::Selected {
                mode: HotStateSharingMode::PrivateEmpty,
                reused_state: false
            }
        );
    }

    #[test]
    fn stale_candidate_cannot_reuse_bytes_but_can_fall_back_to_private_empty() {
        let expected = identity("a");
        let stale = identity("b");
        let context = admission_context("source-view", expected.clone());
        let stale_context = admission_context("source-view", stale);
        let policy = HotStatePathPolicy::new(
            HotStatePathClassId::parse("source-view").unwrap(),
            expected,
            vec![
                HotStateSharingMode::ImmutableOverlay,
                HotStateSharingMode::PrivateCow,
                HotStateSharingMode::PrivateEmpty,
            ],
        )
        .unwrap();
        let current_capabilities = capabilities(true, true, true, false, false);
        let proof = ();
        let refusal = super::admission::admitted_candidate_for_test(
            &proof,
            stale_context,
            &context,
            &policy,
            &current_capabilities,
        )
        .unwrap_err();
        assert_eq!(
            refusal,
            HotStateAdmissionRefusal::Mismatch {
                field: HotStateAdmissionMismatchField::Source
            }
        );
        let receipt = policy
            .select(
                &context,
                None,
                &current_capabilities,
                HotStatePublisherRole::ConsumerOnly,
            )
            .unwrap();
        assert!(!receipt.candidate_identity_match());
        assert_eq!(
            receipt.selection(),
            HotStateSelection::Selected {
                mode: HotStateSharingMode::PrivateEmpty,
                reused_state: false
            }
        );
    }

    #[test]
    fn compiler_policy_never_selects_overlay_when_policy_does_not_offer_it() {
        let expected = identity("a");
        let context = admission_context("compiler-output", expected.clone());
        let policy = HotStatePathPolicy::new(
            HotStatePathClassId::parse("compiler-output").unwrap(),
            expected.clone(),
            vec![
                HotStateSharingMode::PrivateCow,
                HotStateSharingMode::PrivateEmpty,
            ],
        )
        .unwrap();
        let receipt = select_admitted(
            &policy,
            &context,
            &capabilities(true, false, true, false, false),
            HotStatePublisherRole::ConsumerOnly,
        );
        assert_eq!(
            receipt.selection(),
            HotStateSelection::Selected {
                mode: HotStateSharingMode::PrivateEmpty,
                reused_state: false
            }
        );
    }

    #[test]
    fn shared_mutable_publish_requires_explicit_publisher_authority() {
        let expected = identity("a");
        let context = admission_context("package-store", expected.clone());
        let policy = HotStatePathPolicy::new(
            HotStatePathClassId::parse("package-store").unwrap(),
            expected.clone(),
            vec![HotStateSharingMode::ReviewedSharedMutable],
        )
        .unwrap();

        let denied = select_admitted(
            &policy,
            &context,
            &capabilities(false, false, false, true, false),
            HotStatePublisherRole::PublisherEnabled,
        );
        assert_eq!(denied.selection(), HotStateSelection::Unavailable);

        let accepted = select_admitted(
            &policy,
            &context,
            &capabilities(false, false, false, true, true),
            HotStatePublisherRole::PublisherEnabled,
        );
        assert_eq!(
            accepted.selection(),
            HotStateSelection::Selected {
                mode: HotStateSharingMode::ReviewedSharedMutable,
                reused_state: true
            }
        );
    }

    #[test]
    fn missing_candidate_skips_reuse_modes() {
        let expected = identity("a");
        let context = admission_context("dependency-view", expected.clone());
        let policy = HotStatePathPolicy::new(
            HotStatePathClassId::parse("dependency-view").unwrap(),
            expected,
            vec![
                HotStateSharingMode::ImmutableOverlay,
                HotStateSharingMode::PrivateEmpty,
            ],
        )
        .unwrap();
        let receipt = policy
            .select(
                &context,
                None,
                &capabilities(true, false, true, false, false),
                HotStatePublisherRole::ConsumerOnly,
            )
            .unwrap();
        assert_eq!(
            receipt.selection(),
            HotStateSelection::Selected {
                mode: HotStateSharingMode::PrivateEmpty,
                reused_state: false
            }
        );
    }

    #[test]
    fn duplicate_modes_and_invalid_identifiers_are_refused() {
        let expected = identity("a");
        assert_eq!(
            HotStatePathPolicy::new(
                HotStatePathClassId::parse("source-view").unwrap(),
                expected,
                vec![
                    HotStateSharingMode::ImmutableOverlay,
                    HotStateSharingMode::ImmutableOverlay,
                ],
            )
            .unwrap_err()
            .code(),
            "hot_state_policy_mode_duplicate"
        );
        assert!(HotStatePathClassId::parse("Source View").is_err());
        assert!(HotStateGenerationId::parse("").is_err());
    }

    #[test]
    fn receipt_serialization_exposes_digest_not_raw_identity_tokens() {
        let expected = identity("secretish");
        let context = admission_context("index-state", expected.clone());
        let policy = HotStatePathPolicy::new(
            HotStatePathClassId::parse("index-state").unwrap(),
            expected.clone(),
            vec![HotStateSharingMode::PrivateCow],
        )
        .unwrap();
        let current_capabilities = capabilities(false, true, false, false, false);
        let proof = ();
        let candidate = admitted_candidate(&proof, &context, &policy, &current_capabilities);
        assert_eq!(format!("{candidate:?}"), "<admitted-hot-state-candidate>");
        let receipt = policy
            .select(
                &context,
                Some(candidate),
                &current_capabilities,
                HotStatePublisherRole::ConsumerOnly,
            )
            .unwrap();
        let json = serde_json::to_string(&receipt).unwrap();
        assert!(json.contains("sha256:"));
        assert!(!json.contains("source-secretish"));
        assert!(!json.contains("toolchain-secretish"));
        assert!(!json.contains("family-1"));
        assert!(!json.contains("binding-1"));
        assert!(!json.contains("github.com/teamleaderleo/smolrunner"));
        assert!(!json.contains("profile-1"));
        assert!(!json.contains("validator-1"));
        assert!(!json.contains("platform-1"));
    }
}
