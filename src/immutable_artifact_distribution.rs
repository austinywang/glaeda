//! Transport-independent identity and bounded source observations for immutable build products.
//!
//! This module is path-free and side-effect-free. Product owners retain archive validation,
//! publication, leases, deletion, and transport adapters. A transport can report availability or
//! deliver bytes; it cannot mint product authority.

use std::fmt;

use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::artifact::{CommitId, RepositoryRef, Sha256Digest};
use crate::reusable_state_lifecycle::{
    ReusableStateClass, ReusableStateGenerationId, ReusableStateSupersession,
};

pub const IMMUTABLE_ARTIFACT_DISTRIBUTION_SCHEMA_VERSION: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactPlatformClass {
    Macos,
    Linux,
    PlatformIndependent,
}

impl ArtifactPlatformClass {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Macos => "macos",
            Self::Linux => "linux",
            Self::PlatformIndependent => "platform_independent",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactArchitecture {
    Arm64,
    X86_64,
    Universal,
    PlatformIndependent,
}

impl ArtifactArchitecture {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Arm64 => "arm64",
            Self::X86_64 => "x86_64",
            Self::Universal => "universal",
            Self::PlatformIndependent => "platform_independent",
        }
    }

    fn accepts(self, consumer: Self) -> bool {
        match self {
            Self::PlatformIndependent => true,
            Self::Universal => matches!(consumer, Self::Arm64 | Self::X86_64 | Self::Universal),
            exact => exact == consumer,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArtifactCompatibility {
    pub platform: ArtifactPlatformClass,
    pub architecture: ArtifactArchitecture,
    pub sdk_generation: Option<Sha256Digest>,
    pub toolchain_generation: Option<Sha256Digest>,
    pub build_configuration: Sha256Digest,
    pub product_schema: Sha256Digest,
}

impl ArtifactCompatibility {
    #[must_use]
    pub const fn new(
        platform: ArtifactPlatformClass,
        architecture: ArtifactArchitecture,
        sdk_generation: Option<Sha256Digest>,
        toolchain_generation: Option<Sha256Digest>,
        build_configuration: Sha256Digest,
        product_schema: Sha256Digest,
    ) -> Self {
        Self {
            platform,
            architecture,
            sdk_generation,
            toolchain_generation,
            build_configuration,
            product_schema,
        }
    }

    #[must_use]
    pub fn accepts(&self, consumer: &ArtifactConsumerCompatibility) -> bool {
        (self.platform == ArtifactPlatformClass::PlatformIndependent
            || self.platform == consumer.platform)
            && self.architecture.accepts(consumer.architecture)
            && self.sdk_generation.as_ref() == consumer.sdk_generation.as_ref()
            && self.toolchain_generation.as_ref() == consumer.toolchain_generation.as_ref()
            && self.build_configuration == consumer.build_configuration
            && self.product_schema == consumer.product_schema
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArtifactConsumerCompatibility {
    pub platform: ArtifactPlatformClass,
    pub architecture: ArtifactArchitecture,
    pub sdk_generation: Option<Sha256Digest>,
    pub toolchain_generation: Option<Sha256Digest>,
    pub build_configuration: Sha256Digest,
    pub product_schema: Sha256Digest,
}

impl ArtifactConsumerCompatibility {
    #[must_use]
    pub const fn new(
        platform: ArtifactPlatformClass,
        architecture: ArtifactArchitecture,
        sdk_generation: Option<Sha256Digest>,
        toolchain_generation: Option<Sha256Digest>,
        build_configuration: Sha256Digest,
        product_schema: Sha256Digest,
    ) -> Self {
        Self {
            platform,
            architecture,
            sdk_generation,
            toolchain_generation,
            build_configuration,
            product_schema,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactProducerProvider {
    GitHubActions,
}

impl ArtifactProducerProvider {
    const fn as_str(self) -> &'static str {
        match self {
            Self::GitHubActions => "github_actions",
        }
    }
}

/// Accepted origin provenance. A local, peer, or optional remote-broker copy preserves this exact producer record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArtifactProducerProvenance {
    pub provider: ArtifactProducerProvider,
    pub repository: RepositoryRef,
    pub artifact_id: u64,
    pub provider_digest: Sha256Digest,
    pub producer_run_id: u64,
    pub producer_run_attempt: u32,
    pub source_revision: CommitId,
}

impl ArtifactProducerProvenance {
    /// # Errors
    ///
    /// Returns an error when a provider-owned identifier is zero.
    pub fn github_actions(
        repository: RepositoryRef,
        artifact_id: u64,
        provider_digest: Sha256Digest,
        producer_run_id: u64,
        producer_run_attempt: u32,
        source_revision: CommitId,
    ) -> Result<Self, ImmutableArtifactDistributionError> {
        if artifact_id == 0 || producer_run_id == 0 || producer_run_attempt == 0 {
            return Err(ImmutableArtifactDistributionError::InvalidProducerProvenance);
        }
        Ok(Self {
            provider: ArtifactProducerProvider::GitHubActions,
            repository,
            artifact_id,
            provider_digest,
            producer_run_id,
            producer_run_attempt,
            source_revision,
        })
    }
}

/// One immutable object identity shared unchanged by local, peer, optional remote-broker, and provider copies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImmutableArtifactObjectIdentity {
    pub schema_version: u8,
    pub artifact_schema: Sha256Digest,
    pub source_identity: Sha256Digest,
    pub build_identity: Sha256Digest,
    pub product_contract: Sha256Digest,
    pub content_digest: Sha256Digest,
    pub producer: ArtifactProducerProvenance,
    pub compatibility: ArtifactCompatibility,
}

impl ImmutableArtifactObjectIdentity {
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub const fn new(
        artifact_schema: Sha256Digest,
        source_identity: Sha256Digest,
        build_identity: Sha256Digest,
        product_contract: Sha256Digest,
        content_digest: Sha256Digest,
        producer: ArtifactProducerProvenance,
        compatibility: ArtifactCompatibility,
    ) -> Self {
        Self {
            schema_version: IMMUTABLE_ARTIFACT_DISTRIBUTION_SCHEMA_VERSION,
            artifact_schema,
            source_identity,
            build_identity,
            product_contract,
            content_digest,
            producer,
            compatibility,
        }
    }

    /// Return the canonical transport-independent object key.
    ///
    /// # Errors
    ///
    /// Returns an error only if the generated SHA-256 cannot be represented canonically.
    pub fn digest(&self) -> Result<Sha256Digest, ImmutableArtifactDistributionError> {
        let mut hasher = Sha256::new();
        hash_field(&mut hasher, b"schema_version", &[self.schema_version]);
        hash_digest(&mut hasher, b"artifact_schema", &self.artifact_schema);
        hash_digest(&mut hasher, b"source_identity", &self.source_identity);
        hash_digest(&mut hasher, b"build_identity", &self.build_identity);
        hash_digest(&mut hasher, b"product_contract", &self.product_contract);
        hash_digest(&mut hasher, b"content_digest", &self.content_digest);
        hash_field(
            &mut hasher,
            b"producer_provider",
            self.producer.provider.as_str().as_bytes(),
        );
        hash_field(
            &mut hasher,
            b"producer_repository",
            self.producer.repository.as_str().as_bytes(),
        );
        hash_field(
            &mut hasher,
            b"producer_artifact_id",
            &self.producer.artifact_id.to_be_bytes(),
        );
        hash_digest(
            &mut hasher,
            b"producer_provider_digest",
            &self.producer.provider_digest,
        );
        hash_field(
            &mut hasher,
            b"producer_run_id",
            &self.producer.producer_run_id.to_be_bytes(),
        );
        hash_field(
            &mut hasher,
            b"producer_run_attempt",
            &self.producer.producer_run_attempt.to_be_bytes(),
        );
        hash_field(
            &mut hasher,
            b"producer_source_revision",
            self.producer.source_revision.as_str().as_bytes(),
        );
        hash_field(
            &mut hasher,
            b"platform",
            self.compatibility.platform.as_str().as_bytes(),
        );
        hash_field(
            &mut hasher,
            b"architecture",
            self.compatibility.architecture.as_str().as_bytes(),
        );
        hash_optional_digest(
            &mut hasher,
            b"sdk_generation",
            self.compatibility.sdk_generation.as_ref(),
        );
        hash_optional_digest(
            &mut hasher,
            b"toolchain_generation",
            self.compatibility.toolchain_generation.as_ref(),
        );
        hash_digest(
            &mut hasher,
            b"build_configuration",
            &self.compatibility.build_configuration,
        );
        hash_digest(
            &mut hasher,
            b"product_schema",
            &self.compatibility.product_schema,
        );
        digest_bytes(&hasher.finalize())
    }
}

/// Build one explicit reusable-state supersession edge for the same immutable product lineage.
///
/// Source/build/content/provenance generations may change. Artifact schema, product contract,
/// repository, and compatibility must stay equal so transport code cannot infer replacement across
/// unrelated products.
///
/// # Errors
///
/// Returns an error when the two objects are identical, their product lineages differ, or either
/// canonical identity digest cannot be encoded.
pub fn immutable_artifact_supersession(
    predecessor: &ImmutableArtifactObjectIdentity,
    successor: &ImmutableArtifactObjectIdentity,
) -> Result<ReusableStateSupersession, ImmutableArtifactDistributionError> {
    let predecessor_digest = predecessor.digest()?;
    let successor_digest = successor.digest()?;
    if predecessor_digest == successor_digest {
        return Err(ImmutableArtifactDistributionError::IdenticalSupersession);
    }
    if predecessor.artifact_schema != successor.artifact_schema
        || predecessor.product_contract != successor.product_contract
        || predecessor.producer.repository != successor.producer.repository
        || predecessor.compatibility != successor.compatibility
    {
        return Err(ImmutableArtifactDistributionError::SupersessionLineageMismatch);
    }
    Ok(ReusableStateSupersession {
        cache_class: ReusableStateClass::ImmutableCompiledProduct,
        predecessor: ReusableStateGenerationId(predecessor_digest),
        successor: ReusableStateGenerationId(successor_digest),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImmutableArtifactSourceClass {
    NodeLocal,
    TrustedPeer,
    PrivateR2,
    GitHubActions,
    Rebuild,
}

impl ImmutableArtifactSourceClass {
    /// Source order for a co-located persistent fleet that does not need a remote broker.
    ///
    /// GitHub Actions remains the canonical provider fallback. Skipping R2 changes only
    /// acceleration topology; it does not change object identity or validity.
    #[must_use]
    pub const fn same_site_first_lookup_order() -> [Self; 4] {
        [
            Self::NodeLocal,
            Self::TrustedPeer,
            Self::GitHubActions,
            Self::Rebuild,
        ]
    }

    /// Source order when an optional private R2 broker is configured for remote distribution,
    /// off-site resilience, or retention.
    #[must_use]
    pub const fn with_private_r2_lookup_order() -> [Self; 5] {
        [
            Self::NodeLocal,
            Self::TrustedPeer,
            Self::PrivateR2,
            Self::GitHubActions,
            Self::Rebuild,
        ]
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "availability", rename_all = "snake_case")]
pub enum ImmutableArtifactAvailability {
    Unavailable,
    Available {
        object_identity: Sha256Digest,
        schema_version: u8,
        size_bytes: u64,
    },
}

impl ImmutableArtifactAvailability {
    /// Build the bounded, path-free response for one exact object.
    ///
    /// # Errors
    ///
    /// Returns an error when the exact object size is zero.
    pub fn available(
        object_identity: Sha256Digest,
        size_bytes: u64,
    ) -> Result<Self, ImmutableArtifactDistributionError> {
        if size_bytes == 0 {
            return Err(ImmutableArtifactDistributionError::InvalidObjectSize);
        }
        Ok(Self::Available {
            object_identity,
            schema_version: IMMUTABLE_ARTIFACT_DISTRIBUTION_SCHEMA_VERSION,
            size_bytes,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImmutableArtifactLookupOutcome {
    Hit,
    Miss,
    Corrupt,
    SourceUnavailable,
    Incompatible,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImmutableArtifactLookupObservation {
    pub source: ImmutableArtifactSourceClass,
    pub outcome: ImmutableArtifactLookupOutcome,
    pub object_size_bytes: u64,
    pub lookup_duration_millis: u64,
    pub transfer_duration_millis: u64,
    pub bytes_transferred: u64,
    pub restore_duration_millis: u64,
}

impl ImmutableArtifactLookupObservation {
    /// # Errors
    ///
    /// Returns an error when byte accounting is internally inconsistent.
    pub fn new(
        source: ImmutableArtifactSourceClass,
        outcome: ImmutableArtifactLookupOutcome,
        object_size_bytes: u64,
        lookup_duration_millis: u64,
        transfer_duration_millis: u64,
        bytes_transferred: u64,
        restore_duration_millis: u64,
    ) -> Result<Self, ImmutableArtifactDistributionError> {
        if bytes_transferred > object_size_bytes
            || (outcome == ImmutableArtifactLookupOutcome::Hit && object_size_bytes == 0)
        {
            return Err(ImmutableArtifactDistributionError::InvalidObservationBytes);
        }
        Ok(Self {
            source,
            outcome,
            object_size_bytes,
            lookup_duration_millis,
            transfer_duration_millis,
            bytes_transferred,
            restore_duration_millis,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImmutableArtifactDistributionError {
    InvalidProducerProvenance,
    InvalidObjectSize,
    InvalidObservationBytes,
    IdenticalSupersession,
    SupersessionLineageMismatch,
    DigestEncoding,
}

impl fmt::Display for ImmutableArtifactDistributionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidProducerProvenance => "immutable artifact producer provenance is invalid",
            Self::InvalidObjectSize => "immutable artifact object size must be nonzero",
            Self::InvalidObservationBytes => {
                "immutable artifact byte accounting is internally inconsistent"
            }
            Self::IdenticalSupersession => {
                "immutable artifact supersession requires two distinct exact objects"
            }
            Self::SupersessionLineageMismatch => {
                "immutable artifact supersession crosses product lineage"
            }
            Self::DigestEncoding => "immutable artifact identity digest could not be encoded",
        })
    }
}

impl std::error::Error for ImmutableArtifactDistributionError {}

fn hash_field(hasher: &mut Sha256, label: &[u8], value: &[u8]) {
    hasher.update((label.len() as u64).to_be_bytes());
    hasher.update(label);
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
}

fn hash_digest(hasher: &mut Sha256, label: &[u8], value: &Sha256Digest) {
    hash_field(hasher, label, value.as_str().as_bytes());
}

fn hash_optional_digest(hasher: &mut Sha256, label: &[u8], value: Option<&Sha256Digest>) {
    match value {
        Some(value) => hash_digest(hasher, label, value),
        None => hash_field(hasher, label, b""),
    }
}

fn digest_bytes(bytes: &[u8]) -> Result<Sha256Digest, ImmutableArtifactDistributionError> {
    let hex = bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Sha256Digest::parse(&format!("sha256:{hex}"))
        .map_err(|_| ImmutableArtifactDistributionError::DigestEncoding)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(byte: &str) -> Sha256Digest {
        Sha256Digest::parse(&format!("sha256:{}", byte.repeat(64))).expect("digest")
    }

    fn object() -> ImmutableArtifactObjectIdentity {
        let producer = ArtifactProducerProvenance::github_actions(
            RepositoryRef::parse("manaflow-ai/cmux").expect("repo"),
            123,
            digest("1"),
            456,
            2,
            CommitId::parse(&"ab".repeat(20)).expect("revision"),
        )
        .expect("producer");
        ImmutableArtifactObjectIdentity::new(
            digest("2"),
            digest("3"),
            digest("4"),
            digest("5"),
            digest("6"),
            producer,
            ArtifactCompatibility::new(
                ArtifactPlatformClass::Macos,
                ArtifactArchitecture::Arm64,
                Some(digest("7")),
                Some(digest("8")),
                digest("9"),
                digest("a"),
            ),
        )
    }

    #[test]
    fn source_order_keeps_remote_broker_optional() {
        assert_eq!(
            ImmutableArtifactSourceClass::same_site_first_lookup_order(),
            [
                ImmutableArtifactSourceClass::NodeLocal,
                ImmutableArtifactSourceClass::TrustedPeer,
                ImmutableArtifactSourceClass::GitHubActions,
                ImmutableArtifactSourceClass::Rebuild,
            ]
        );
        assert_eq!(
            ImmutableArtifactSourceClass::with_private_r2_lookup_order(),
            [
                ImmutableArtifactSourceClass::NodeLocal,
                ImmutableArtifactSourceClass::TrustedPeer,
                ImmutableArtifactSourceClass::PrivateR2,
                ImmutableArtifactSourceClass::GitHubActions,
                ImmutableArtifactSourceClass::Rebuild,
            ]
        );
    }

    #[test]
    fn object_digest_is_transport_independent_and_binds_provenance() {
        let exact = object();
        let same = object();
        assert_eq!(
            exact.digest().expect("digest"),
            same.digest().expect("digest")
        );

        let mut different = object();
        different.producer.artifact_id += 1;
        assert_ne!(
            exact.digest().expect("exact digest"),
            different.digest().expect("different digest")
        );
    }

    #[test]
    fn compatibility_is_explicit_for_platform_architecture_and_toolchain() {
        let exact = object();
        let compatible = ArtifactConsumerCompatibility::new(
            ArtifactPlatformClass::Macos,
            ArtifactArchitecture::Arm64,
            Some(digest("7")),
            Some(digest("8")),
            digest("9"),
            digest("a"),
        );
        assert!(exact.compatibility.accepts(&compatible));

        let wrong_architecture = ArtifactConsumerCompatibility::new(
            ArtifactPlatformClass::Macos,
            ArtifactArchitecture::X86_64,
            Some(digest("7")),
            Some(digest("8")),
            digest("9"),
            digest("a"),
        );
        assert!(!exact.compatibility.accepts(&wrong_architecture));
    }

    #[test]
    fn universal_and_platform_independent_artifacts_have_deliberate_compatibility() {
        let consumer = ArtifactConsumerCompatibility::new(
            ArtifactPlatformClass::Linux,
            ArtifactArchitecture::X86_64,
            None,
            None,
            digest("b"),
            digest("c"),
        );
        let portable = ArtifactCompatibility::new(
            ArtifactPlatformClass::PlatformIndependent,
            ArtifactArchitecture::PlatformIndependent,
            None,
            None,
            digest("b"),
            digest("c"),
        );
        assert!(portable.accepts(&consumer));

        let universal = ArtifactCompatibility::new(
            ArtifactPlatformClass::Macos,
            ArtifactArchitecture::Universal,
            None,
            None,
            digest("b"),
            digest("c"),
        );
        let mac = ArtifactConsumerCompatibility::new(
            ArtifactPlatformClass::Macos,
            ArtifactArchitecture::Arm64,
            None,
            None,
            digest("b"),
            digest("c"),
        );
        assert!(universal.accepts(&mac));
    }

    #[test]
    fn availability_discloses_only_exact_identity_schema_and_size() {
        let identity = object().digest().expect("identity");
        let availability =
            ImmutableArtifactAvailability::available(identity.clone(), 606_055_512).expect("offer");
        assert_eq!(
            availability,
            ImmutableArtifactAvailability::Available {
                object_identity: identity,
                schema_version: IMMUTABLE_ARTIFACT_DISTRIBUTION_SCHEMA_VERSION,
                size_bytes: 606_055_512,
            }
        );
        assert!(ImmutableArtifactAvailability::available(digest("d"), 0).is_err());
    }

    #[test]
    fn observations_are_bounded_and_source_specific() {
        let observation = ImmutableArtifactLookupObservation::new(
            ImmutableArtifactSourceClass::TrustedPeer,
            ImmutableArtifactLookupOutcome::Hit,
            606_055_512,
            12,
            1_234,
            606_055_512,
            4_321,
        )
        .expect("observation");
        assert_eq!(
            observation.source,
            ImmutableArtifactSourceClass::TrustedPeer
        );
        assert_eq!(observation.bytes_transferred, 606_055_512);

        assert!(
            ImmutableArtifactLookupObservation::new(
                ImmutableArtifactSourceClass::PrivateR2,
                ImmutableArtifactLookupOutcome::Miss,
                10,
                u64::MAX,
                u64::MAX,
                11,
                u64::MAX,
            )
            .is_err()
        );

        let huge = ImmutableArtifactLookupObservation::new(
            ImmutableArtifactSourceClass::PrivateR2,
            ImmutableArtifactLookupOutcome::Hit,
            u64::MAX,
            u64::MAX,
            u64::MAX,
            u64::MAX,
            u64::MAX,
        )
        .expect("u64-bounded metadata has no arbitrary protocol ceiling");
        assert_eq!(huge.object_size_bytes, u64::MAX);
    }

    #[test]
    fn exact_product_lineage_emits_reusable_state_supersession_edge() {
        let predecessor = object();
        let mut successor = object();
        successor.source_identity = digest("b");
        successor.build_identity = digest("c");
        successor.content_digest = digest("d");
        successor.producer.artifact_id += 1;
        successor.producer.producer_run_id += 1;
        successor.producer.source_revision =
            CommitId::parse(&"cd".repeat(20)).expect("successor revision");

        let edge = immutable_artifact_supersession(&predecessor, &successor).expect("supersession");
        assert_eq!(
            edge.cache_class,
            ReusableStateClass::ImmutableCompiledProduct
        );
        assert_eq!(
            edge.predecessor.0,
            predecessor.digest().expect("predecessor digest")
        );
        assert_eq!(
            edge.successor.0,
            successor.digest().expect("successor digest")
        );
    }

    #[test]
    fn supersession_refuses_identical_or_cross_product_objects() {
        let predecessor = object();
        assert_eq!(
            immutable_artifact_supersession(&predecessor, &predecessor).unwrap_err(),
            ImmutableArtifactDistributionError::IdenticalSupersession
        );

        let mut unrelated = object();
        unrelated.product_contract = digest("f");
        assert_eq!(
            immutable_artifact_supersession(&predecessor, &unrelated).unwrap_err(),
            ImmutableArtifactDistributionError::SupersessionLineageMismatch
        );
    }

    #[test]
    fn artifact_metadata_has_no_arbitrary_size_ceiling() {
        let identity = object().digest().expect("identity");
        let availability =
            ImmutableArtifactAvailability::available(identity, u64::MAX).expect("large object");
        assert!(matches!(
            availability,
            ImmutableArtifactAvailability::Available {
                size_bytes: u64::MAX,
                ..
            }
        ));
    }

    #[test]
    fn producer_ids_must_be_complete_provider_owned_values() {
        assert!(
            ArtifactProducerProvenance::github_actions(
                RepositoryRef::parse("manaflow-ai/cmux").expect("repo"),
                0,
                digest("1"),
                456,
                1,
                CommitId::parse(&"ab".repeat(20)).expect("revision"),
            )
            .is_err()
        );
    }
}
