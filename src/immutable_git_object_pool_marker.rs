use std::fmt;

use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::artifact::Sha256Digest;
use crate::immutable_git_object_pool::{GitObjectFormat, GitObjectPoolBinding};

pub const IMMUTABLE_GIT_OBJECT_POOL_MARKER_SCHEMA_VERSION: u8 = 1;
pub const GLAEDA_IMMUTABLE_GIT_OBJECT_POOL_MARKER_SCHEMA_VERSION: u8 = 2;
pub const IMMUTABLE_GIT_OBJECT_POOL_MARKER_BYTES: usize = 64;
const SMOLRUNNER_V1_MARKER_MAGIC: &[u8; 8] = b"SMOLGOP1";
const GLAEDA_V2_MARKER_MAGIC: &[u8; 8] = b"GLAEGOP2";
const RESERVED_START: usize = 9;
const DIGEST_START: usize = 16;
const NONCE_START: usize = 48;
const SMOLRUNNER_V1_BINDING_DIGEST_DOMAIN: &[u8] =
    b"smolrunner-immutable-git-object-pool-binding-v1\0";
const GLAEDA_V2_BINDING_DIGEST_DOMAIN: &[u8] = b"glaeda-immutable-git-object-pool-binding-v2\0";
const SMOLRUNNER_V1_PHYSICAL_AUDIT_IDENTITY_DOMAIN: &[u8] =
    b"smolrunner-immutable-git-pool-audit-physical-v1\0";
const GLAEDA_V2_PHYSICAL_AUDIT_IDENTITY_DOMAIN: &[u8] =
    b"glaeda-immutable-git-pool-audit-physical-v2\0";
const SHA256_PREFIX: &str = "sha256:";
const HEX: &[u8; 16] = b"0123456789abcdef";

/// Closed identity generation for immutable Git object-pool marker and audit evidence.
///
/// Existing public constructors continue to mean `SmolrunnerV1`. `GlaedaV2` is an explicit pure
/// successor vocabulary for later #592 publication composition; selecting it here grants no file,
/// publication, ownership, lease, adoption, or cleanup authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImmutableGitObjectPoolIdentityGeneration {
    SmolrunnerV1,
    GlaedaV2,
}

impl ImmutableGitObjectPoolIdentityGeneration {
    #[must_use]
    pub const fn schema_version(self) -> u8 {
        match self {
            Self::SmolrunnerV1 => IMMUTABLE_GIT_OBJECT_POOL_MARKER_SCHEMA_VERSION,
            Self::GlaedaV2 => GLAEDA_IMMUTABLE_GIT_OBJECT_POOL_MARKER_SCHEMA_VERSION,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SmolrunnerV1 => "smolrunner_v1",
            Self::GlaedaV2 => "glaeda_v2",
        }
    }

    const fn marker_magic(self) -> &'static [u8; 8] {
        match self {
            Self::SmolrunnerV1 => SMOLRUNNER_V1_MARKER_MAGIC,
            Self::GlaedaV2 => GLAEDA_V2_MARKER_MAGIC,
        }
    }

    const fn binding_digest_domain(self) -> &'static [u8] {
        match self {
            Self::SmolrunnerV1 => SMOLRUNNER_V1_BINDING_DIGEST_DOMAIN,
            Self::GlaedaV2 => GLAEDA_V2_BINDING_DIGEST_DOMAIN,
        }
    }

    const fn physical_audit_identity_domain(self) -> &'static [u8] {
        match self {
            Self::SmolrunnerV1 => SMOLRUNNER_V1_PHYSICAL_AUDIT_IDENTITY_DOMAIN,
            Self::GlaedaV2 => GLAEDA_V2_PHYSICAL_AUDIT_IDENTITY_DOMAIN,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub struct GitObjectPoolMarkerNonce([u8; 16]);

impl GitObjectPoolMarkerNonce {
    /// Construct one non-reusable marker nonce supplied by a later publication transaction.
    ///
    /// This pure constructor does not generate randomness.
    ///
    /// # Errors
    ///
    /// Returns a bounded error when the supplied nonce is all zero bytes.
    pub fn new(bytes: [u8; 16]) -> Result<Self, ImmutableGitObjectPoolMarkerError> {
        if bytes == [0; 16] {
            return Err(invalid_nonce());
        }
        Ok(Self(bytes))
    }
}

impl fmt::Debug for GitObjectPoolMarkerNonce {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<opaque-git-object-pool-marker-nonce>")
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct ImmutableGitObjectPoolMarker {
    generation: ImmutableGitObjectPoolIdentityGeneration,
    binding_digest: Sha256Digest,
    nonce: GitObjectPoolMarkerNonce,
}

impl ImmutableGitObjectPoolMarker {
    /// Build one exact retained SmolRunner-v1 marker for a reviewed immutable Git object-pool
    /// binding.
    ///
    /// This compatibility constructor intentionally remains v1. Fresh Glaeda publication must opt
    /// into `new_for_generation(GlaedaV2, ...)` inside the later #592 publication transaction.
    ///
    /// # Errors
    ///
    /// Returns a bounded error only when the canonical binding digest cannot be represented.
    pub fn new(
        binding: &GitObjectPoolBinding,
        nonce: GitObjectPoolMarkerNonce,
    ) -> Result<Self, ImmutableGitObjectPoolMarkerError> {
        Self::new_for_generation(
            ImmutableGitObjectPoolIdentityGeneration::SmolrunnerV1,
            binding,
            nonce,
        )
    }

    /// Build one exact marker for an explicitly selected closed identity generation.
    ///
    /// This is pure codec work only. Constructing marker bytes grants no authority to write them or
    /// to reinterpret an existing physical generation.
    ///
    /// # Errors
    ///
    /// Returns a bounded error only when the canonical binding digest cannot be represented.
    pub fn new_for_generation(
        generation: ImmutableGitObjectPoolIdentityGeneration,
        binding: &GitObjectPoolBinding,
        nonce: GitObjectPoolMarkerNonce,
    ) -> Result<Self, ImmutableGitObjectPoolMarkerError> {
        Ok(Self {
            generation,
            binding_digest: git_object_pool_binding_digest_for_generation(generation, binding)?,
            nonce,
        })
    }

    #[must_use]
    pub const fn generation(&self) -> ImmutableGitObjectPoolIdentityGeneration {
        self.generation
    }

    #[must_use]
    pub const fn binding_digest(&self) -> &Sha256Digest {
        &self.binding_digest
    }

    /// Encode the strict 64-byte marker document for this marker's closed generation.
    ///
    /// Existing values constructed by `new` remain byte-for-byte SmolRunner-v1 documents.
    ///
    /// # Errors
    ///
    /// Returns a bounded error if the stored canonical digest cannot be decoded to 32 raw bytes.
    pub fn encode(
        &self,
    ) -> Result<[u8; IMMUTABLE_GIT_OBJECT_POOL_MARKER_BYTES], ImmutableGitObjectPoolMarkerError>
    {
        let mut bytes = [0_u8; IMMUTABLE_GIT_OBJECT_POOL_MARKER_BYTES];
        bytes[..8].copy_from_slice(self.generation.marker_magic());
        bytes[8] = self.generation.schema_version();
        bytes[DIGEST_START..NONCE_START].copy_from_slice(&digest_to_raw(&self.binding_digest)?);
        bytes[NONCE_START..].copy_from_slice(&self.nonce.0);
        Ok(bytes)
    }

    /// Decode and verify one strict retained SmolRunner-v1 marker against the exact expected
    /// logical binding.
    ///
    /// This compatibility decoder intentionally remains v1 and therefore refuses Glaeda-v2 bytes.
    ///
    /// # Errors
    ///
    /// Returns a bounded error for wrong length/magic/version/reserved bytes, zero nonce, malformed
    /// digest material, or a marker belonging to another logical binding.
    pub fn decode_and_verify(
        bytes: &[u8],
        binding: &GitObjectPoolBinding,
    ) -> Result<Self, ImmutableGitObjectPoolMarkerError> {
        Self::decode_and_verify_for_generation(
            bytes,
            ImmutableGitObjectPoolIdentityGeneration::SmolrunnerV1,
            binding,
        )
    }

    /// Decode and verify one strict marker against an explicit expected identity generation and
    /// exact logical binding.
    ///
    /// The expected generation is caller-selected from a closed enum; marker magic, version, and
    /// digest domain cannot be supplied as free-form values.
    ///
    /// # Errors
    ///
    /// Returns a bounded error for wrong length/generation/magic/version/reserved bytes, zero nonce,
    /// malformed digest material, or a marker belonging to another logical binding.
    pub fn decode_and_verify_for_generation(
        bytes: &[u8],
        generation: ImmutableGitObjectPoolIdentityGeneration,
        binding: &GitObjectPoolBinding,
    ) -> Result<Self, ImmutableGitObjectPoolMarkerError> {
        if bytes.len() != IMMUTABLE_GIT_OBJECT_POOL_MARKER_BYTES {
            return Err(invalid_marker());
        }
        if bytes[..8] != generation.marker_magic()[..]
            || bytes[8] != generation.schema_version()
            || bytes[RESERVED_START..DIGEST_START] != [0; DIGEST_START - RESERVED_START]
        {
            return Err(invalid_marker());
        }

        let mut raw_digest = [0_u8; 32];
        raw_digest.copy_from_slice(&bytes[DIGEST_START..NONCE_START]);
        let binding_digest = raw_to_digest(&raw_digest)?;
        if binding_digest != git_object_pool_binding_digest_for_generation(generation, binding)? {
            return Err(binding_mismatch());
        }

        let mut raw_nonce = [0_u8; 16];
        raw_nonce.copy_from_slice(&bytes[NONCE_START..]);
        let nonce = GitObjectPoolMarkerNonce::new(raw_nonce)?;
        Ok(Self {
            generation,
            binding_digest,
            nonce,
        })
    }
}

impl fmt::Debug for ImmutableGitObjectPoolMarker {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ImmutableGitObjectPoolMarker")
            .field("generation", &self.generation)
            .field("binding_digest", &self.binding_digest)
            .field("nonce", &"<opaque-nonzero-generation-nonce>")
            .finish()
    }
}

/// Compute the exact retained SmolRunner-v1 domain-separated digest for one #583 object-pool
/// binding.
///
/// This compatibility entry point intentionally remains v1.
///
/// # Errors
///
/// Returns a bounded error only if a canonical token length or SHA-256 digest cannot be represented.
pub fn git_object_pool_binding_digest(
    binding: &GitObjectPoolBinding,
) -> Result<Sha256Digest, ImmutableGitObjectPoolMarkerError> {
    git_object_pool_binding_digest_for_generation(
        ImmutableGitObjectPoolIdentityGeneration::SmolrunnerV1,
        binding,
    )
}

/// Compute the canonical binding digest for an explicitly selected closed identity generation.
///
/// # Errors
///
/// Returns a bounded error only if a canonical token length or SHA-256 digest cannot be represented.
pub fn git_object_pool_binding_digest_for_generation(
    generation: ImmutableGitObjectPoolIdentityGeneration,
    binding: &GitObjectPoolBinding,
) -> Result<Sha256Digest, ImmutableGitObjectPoolMarkerError> {
    let mut hasher = Sha256::new();
    hasher.update(generation.binding_digest_domain());
    hasher.update([generation.schema_version()]);
    hash_token(&mut hasher, binding.project().as_str())?;
    hash_token(&mut hasher, binding.pool_id().as_str())?;
    hasher.update(binding.generation().get().to_be_bytes());
    hash_token(&mut hasher, binding.project_disk_id().as_str())?;
    hasher.update(binding.project_disk_generation().get().to_be_bytes());
    hasher.update([match binding.object_format() {
        GitObjectFormat::Sha1 => 1,
        GitObjectFormat::Sha256 => 2,
    }]);
    hash_token(&mut hasher, binding.producer_generation().as_str())?;
    hash_token(&mut hasher, binding.trust_generation().as_str())?;
    raw_to_digest(hasher.finalize().as_slice())
}

/// Closed physical role used by the generation-audit identity codec.
///
/// This remains crate-private so external callers cannot mint public physical-evidence vocabulary.
#[allow(dead_code)] // Consumed when #592 composes Glaeda-v2 publication audit evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImmutableGitObjectPoolPhysicalAuditRole {
    Source,
    Candidate,
}

impl ImmutableGitObjectPoolPhysicalAuditRole {
    const fn as_bytes(self) -> &'static [u8] {
        match self {
            Self::Source => b"source",
            Self::Candidate => b"candidate",
        }
    }
}

/// Private physical directory facts already retained by the descriptor-bound generation audit.
///
/// Values remain crate-private and are hashed immediately into an opaque identity. They never enter
/// public marker or audit summaries through this type.
#[allow(dead_code)] // Consumed when #592 composes Glaeda-v2 publication audit evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ImmutableGitObjectPoolPhysicalAuditDirectoryFacts {
    pub(crate) device: u64,
    pub(crate) inode: u64,
    pub(crate) uid: u32,
    pub(crate) gid: u32,
    pub(crate) mode: u32,
    pub(crate) mtime: i64,
    pub(crate) mtime_nsec: i64,
    pub(crate) ctime: i64,
    pub(crate) ctime_nsec: i64,
}

/// Derive the opaque publication-audit physical identity for one closed marker generation.
///
/// This pure crate-private helper carries no descriptor, path, lease, publication, or ownership
/// capability. The current audit path remains on its existing v1 implementation until #592 opts in.
///
/// # Errors
///
/// Returns a bounded digest error only if canonical digest representation fails.
#[allow(dead_code)] // Consumed when #592 composes Glaeda-v2 publication audit evidence.
pub(crate) fn git_object_pool_generation_audit_physical_identity_digest_for_generation(
    generation: ImmutableGitObjectPoolIdentityGeneration,
    role: ImmutableGitObjectPoolPhysicalAuditRole,
    binding: &GitObjectPoolBinding,
    directories: &[ImmutableGitObjectPoolPhysicalAuditDirectoryFacts],
) -> Result<Sha256Digest, ImmutableGitObjectPoolMarkerError> {
    let binding_digest = git_object_pool_binding_digest_for_generation(generation, binding)?;
    let label = role.as_bytes();
    let mut hasher = Sha256::new();
    hasher.update(generation.physical_audit_identity_domain());
    hasher.update((label.len() as u64).to_be_bytes());
    hasher.update(label);
    hasher.update(binding_digest.as_str().as_bytes());
    for directory in directories {
        hasher.update(directory.device.to_be_bytes());
        hasher.update(directory.inode.to_be_bytes());
        hasher.update(directory.uid.to_be_bytes());
        hasher.update(directory.gid.to_be_bytes());
        hasher.update(directory.mode.to_be_bytes());
        hasher.update(directory.mtime.to_be_bytes());
        hasher.update(directory.mtime_nsec.to_be_bytes());
        hasher.update(directory.ctime.to_be_bytes());
        hasher.update(directory.ctime_nsec.to_be_bytes());
    }
    raw_to_digest(hasher.finalize().as_slice())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImmutableGitObjectPoolMarkerErrorKind {
    InvalidNonce,
    InvalidMarker,
    BindingMismatch,
    InvalidDigest,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct ImmutableGitObjectPoolMarkerError {
    kind: ImmutableGitObjectPoolMarkerErrorKind,
    code: &'static str,
    message: &'static str,
}

impl ImmutableGitObjectPoolMarkerError {
    #[must_use]
    pub const fn kind(&self) -> ImmutableGitObjectPoolMarkerErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for ImmutableGitObjectPoolMarkerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ImmutableGitObjectPoolMarkerError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for ImmutableGitObjectPoolMarkerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for ImmutableGitObjectPoolMarkerError {}

fn hash_token(hasher: &mut Sha256, value: &str) -> Result<(), ImmutableGitObjectPoolMarkerError> {
    let bytes = value.as_bytes();
    let length = u32::try_from(bytes.len()).map_err(|_| invalid_digest())?;
    hasher.update(length.to_be_bytes());
    hasher.update(bytes);
    Ok(())
}

fn digest_to_raw(digest: &Sha256Digest) -> Result<[u8; 32], ImmutableGitObjectPoolMarkerError> {
    let hex = digest
        .as_str()
        .strip_prefix(SHA256_PREFIX)
        .ok_or_else(invalid_digest)?;
    if hex.len() != 64 {
        return Err(invalid_digest());
    }
    let mut raw = [0_u8; 32];
    for (index, byte) in raw.iter_mut().enumerate() {
        let high = decode_hex(hex.as_bytes()[index * 2]).ok_or_else(invalid_digest)?;
        let low = decode_hex(hex.as_bytes()[index * 2 + 1]).ok_or_else(invalid_digest)?;
        *byte = (high << 4) | low;
    }
    Ok(raw)
}

fn raw_to_digest(raw: &[u8]) -> Result<Sha256Digest, ImmutableGitObjectPoolMarkerError> {
    if raw.len() != 32 {
        return Err(invalid_digest());
    }
    let mut value = String::with_capacity(SHA256_PREFIX.len() + 64);
    value.push_str(SHA256_PREFIX);
    for byte in raw {
        value.push(char::from(HEX[usize::from(byte >> 4)]));
        value.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    Sha256Digest::parse(&value).map_err(|_| invalid_digest())
}

const fn decode_hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

const fn error(
    kind: ImmutableGitObjectPoolMarkerErrorKind,
    code: &'static str,
    message: &'static str,
) -> ImmutableGitObjectPoolMarkerError {
    ImmutableGitObjectPoolMarkerError {
        kind,
        code,
        message,
    }
}

const fn invalid_nonce() -> ImmutableGitObjectPoolMarkerError {
    error(
        ImmutableGitObjectPoolMarkerErrorKind::InvalidNonce,
        "git_object_pool_marker_nonce_invalid",
        "Git object-pool marker nonce must be nonzero",
    )
}

const fn invalid_marker() -> ImmutableGitObjectPoolMarkerError {
    error(
        ImmutableGitObjectPoolMarkerErrorKind::InvalidMarker,
        "git_object_pool_marker_invalid",
        "Git object-pool marker document is invalid",
    )
}

const fn binding_mismatch() -> ImmutableGitObjectPoolMarkerError {
    error(
        ImmutableGitObjectPoolMarkerErrorKind::BindingMismatch,
        "git_object_pool_marker_binding_mismatch",
        "Git object-pool marker does not match the expected binding",
    )
}

const fn invalid_digest() -> ImmutableGitObjectPoolMarkerError {
    error(
        ImmutableGitObjectPoolMarkerErrorKind::InvalidDigest,
        "git_object_pool_marker_digest_invalid",
        "Git object-pool marker digest is invalid",
    )
}

#[cfg(test)]
mod tests {
    use crate::artifact::Sha256Digest;
    use crate::immutable_git_object_pool::{
        GitObjectFormat, GitObjectPoolBinding, GitObjectPoolGeneration, GitObjectPoolId,
        GitObjectPoolProducerGenerationId, GitObjectPoolTrustGenerationId,
    };
    use crate::project_catalog::ProjectIdentity;
    use crate::project_disk_lease::{ProjectDiskGeneration, ProjectDiskId};

    use super::{
        GitObjectPoolMarkerNonce, IMMUTABLE_GIT_OBJECT_POOL_MARKER_BYTES,
        ImmutableGitObjectPoolIdentityGeneration, ImmutableGitObjectPoolMarker,
        ImmutableGitObjectPoolMarkerErrorKind, ImmutableGitObjectPoolPhysicalAuditDirectoryFacts,
        ImmutableGitObjectPoolPhysicalAuditRole, git_object_pool_binding_digest,
        git_object_pool_binding_digest_for_generation,
        git_object_pool_generation_audit_physical_identity_digest_for_generation,
    };

    #[allow(clippy::too_many_arguments)]
    fn make_binding(
        project: &str,
        pool: &str,
        generation: u64,
        disk: &str,
        disk_generation: u64,
        object_format: GitObjectFormat,
        producer: &str,
        trust: &str,
    ) -> GitObjectPoolBinding {
        GitObjectPoolBinding::new(
            ProjectIdentity::parse(project).unwrap(),
            GitObjectPoolId::parse(pool).unwrap(),
            GitObjectPoolGeneration::new(generation).unwrap(),
            ProjectDiskId::parse(disk).unwrap(),
            ProjectDiskGeneration::new(disk_generation).unwrap(),
            object_format,
            GitObjectPoolProducerGenerationId::parse(producer).unwrap(),
            GitObjectPoolTrustGenerationId::parse(trust).unwrap(),
        )
    }

    fn base_binding() -> GitObjectPoolBinding {
        make_binding(
            "github.com/teamleaderleo/smolrunner",
            "pool-a",
            1,
            "disk-a",
            1,
            GitObjectFormat::Sha1,
            "producer-a",
            "trust-a",
        )
    }

    #[test]
    fn binding_digest_changes_for_every_identity_dimension() {
        let base = git_object_pool_binding_digest(&base_binding()).unwrap();
        let variants = [
            make_binding(
                "github.com/teamleaderleo/fex",
                "pool-a",
                1,
                "disk-a",
                1,
                GitObjectFormat::Sha1,
                "producer-a",
                "trust-a",
            ),
            make_binding(
                "github.com/teamleaderleo/smolrunner",
                "pool-b",
                1,
                "disk-a",
                1,
                GitObjectFormat::Sha1,
                "producer-a",
                "trust-a",
            ),
            make_binding(
                "github.com/teamleaderleo/smolrunner",
                "pool-a",
                2,
                "disk-a",
                1,
                GitObjectFormat::Sha1,
                "producer-a",
                "trust-a",
            ),
            make_binding(
                "github.com/teamleaderleo/smolrunner",
                "pool-a",
                1,
                "disk-b",
                1,
                GitObjectFormat::Sha1,
                "producer-a",
                "trust-a",
            ),
            make_binding(
                "github.com/teamleaderleo/smolrunner",
                "pool-a",
                1,
                "disk-a",
                2,
                GitObjectFormat::Sha1,
                "producer-a",
                "trust-a",
            ),
            make_binding(
                "github.com/teamleaderleo/smolrunner",
                "pool-a",
                1,
                "disk-a",
                1,
                GitObjectFormat::Sha256,
                "producer-a",
                "trust-a",
            ),
            make_binding(
                "github.com/teamleaderleo/smolrunner",
                "pool-a",
                1,
                "disk-a",
                1,
                GitObjectFormat::Sha1,
                "producer-b",
                "trust-a",
            ),
            make_binding(
                "github.com/teamleaderleo/smolrunner",
                "pool-a",
                1,
                "disk-a",
                1,
                GitObjectFormat::Sha1,
                "producer-a",
                "trust-b",
            ),
        ];
        for variant in variants {
            assert_ne!(base, git_object_pool_binding_digest(&variant).unwrap());
        }
    }

    #[test]
    fn exact_smolrunner_v1_marker_fixture_round_trips_byte_for_byte() {
        let binding = base_binding();
        let expected = [
            83, 77, 79, 76, 71, 79, 80, 49, 1, 0, 0, 0, 0, 0, 0, 0, 118, 236, 235, 75, 164, 229,
            107, 243, 50, 192, 164, 239, 119, 99, 31, 166, 30, 163, 29, 188, 206, 21, 145, 102,
            169, 207, 203, 27, 163, 85, 109, 27, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7,
        ];
        let marker = ImmutableGitObjectPoolMarker::new(
            &binding,
            GitObjectPoolMarkerNonce::new([7; 16]).unwrap(),
        )
        .unwrap();
        assert_eq!(
            marker.generation(),
            ImmutableGitObjectPoolIdentityGeneration::SmolrunnerV1
        );
        assert_eq!(marker.encode().unwrap(), expected);

        let decoded = ImmutableGitObjectPoolMarker::decode_and_verify(&expected, &binding).unwrap();
        assert_eq!(decoded.encode().unwrap(), expected);
        assert_eq!(
            decoded.binding_digest(),
            &Sha256Digest::parse(
                "sha256:76eceb4ba4e56bf332c0a4ef77631fa61ea31dbcce159166a9cfcb1ba3556d1b"
            )
            .unwrap()
        );
    }

    #[test]
    fn glaeda_v2_marker_is_distinct_deterministic_and_generation_bound() {
        let binding = base_binding();
        let nonce = GitObjectPoolMarkerNonce::new([5; 16]).unwrap();
        let v2 = ImmutableGitObjectPoolMarker::new_for_generation(
            ImmutableGitObjectPoolIdentityGeneration::GlaedaV2,
            &binding,
            nonce.clone(),
        )
        .unwrap();
        let v1 = ImmutableGitObjectPoolMarker::new(&binding, nonce).unwrap();

        let first = v2.encode().unwrap();
        assert_eq!(first, v2.encode().unwrap());
        assert_eq!(first.len(), IMMUTABLE_GIT_OBJECT_POOL_MARKER_BYTES);
        assert_eq!(&first[..8], b"GLAEGOP2");
        assert_eq!(first[8], 2);
        assert_eq!(&first[9..16], &[0; 7]);
        assert_ne!(first, v1.encode().unwrap());
        assert_ne!(v1.binding_digest(), v2.binding_digest());
        assert_ne!(v1, v2);

        let decoded = ImmutableGitObjectPoolMarker::decode_and_verify_for_generation(
            &first,
            ImmutableGitObjectPoolIdentityGeneration::GlaedaV2,
            &binding,
        )
        .unwrap();
        assert_eq!(decoded, v2);
    }

    #[test]
    fn marker_generations_cannot_be_cross_decoded() {
        let binding = base_binding();
        let v1 = ImmutableGitObjectPoolMarker::new(
            &binding,
            GitObjectPoolMarkerNonce::new([1; 16]).unwrap(),
        )
        .unwrap()
        .encode()
        .unwrap();
        let v2 = ImmutableGitObjectPoolMarker::new_for_generation(
            ImmutableGitObjectPoolIdentityGeneration::GlaedaV2,
            &binding,
            GitObjectPoolMarkerNonce::new([2; 16]).unwrap(),
        )
        .unwrap()
        .encode()
        .unwrap();

        assert_eq!(
            ImmutableGitObjectPoolMarker::decode_and_verify_for_generation(
                &v1,
                ImmutableGitObjectPoolIdentityGeneration::GlaedaV2,
                &binding,
            )
            .unwrap_err()
            .kind(),
            ImmutableGitObjectPoolMarkerErrorKind::InvalidMarker
        );
        assert_eq!(
            ImmutableGitObjectPoolMarker::decode_and_verify(&v2, &binding)
                .unwrap_err()
                .kind(),
            ImmutableGitObjectPoolMarkerErrorKind::InvalidMarker
        );
    }

    #[test]
    fn binding_digest_generation_is_domain_separated() {
        let binding = base_binding();
        let old = git_object_pool_binding_digest(&binding).unwrap();
        let explicit_old = git_object_pool_binding_digest_for_generation(
            ImmutableGitObjectPoolIdentityGeneration::SmolrunnerV1,
            &binding,
        )
        .unwrap();
        let new = git_object_pool_binding_digest_for_generation(
            ImmutableGitObjectPoolIdentityGeneration::GlaedaV2,
            &binding,
        )
        .unwrap();

        assert_eq!(old, explicit_old);
        assert_ne!(old, new);
    }

    #[test]
    fn audit_physical_identity_generation_preserves_v1_and_separates_v2() {
        let binding = base_binding();
        let facts = [ImmutableGitObjectPoolPhysicalAuditDirectoryFacts {
            device: 1,
            inode: 2,
            uid: 3,
            gid: 4,
            mode: 0o40_755,
            mtime: 5,
            mtime_nsec: 6,
            ctime: 7,
            ctime_nsec: 8,
        }];
        let old = git_object_pool_generation_audit_physical_identity_digest_for_generation(
            ImmutableGitObjectPoolIdentityGeneration::SmolrunnerV1,
            ImmutableGitObjectPoolPhysicalAuditRole::Source,
            &binding,
            &facts,
        )
        .unwrap();
        let new = git_object_pool_generation_audit_physical_identity_digest_for_generation(
            ImmutableGitObjectPoolIdentityGeneration::GlaedaV2,
            ImmutableGitObjectPoolPhysicalAuditRole::Source,
            &binding,
            &facts,
        )
        .unwrap();
        let candidate = git_object_pool_generation_audit_physical_identity_digest_for_generation(
            ImmutableGitObjectPoolIdentityGeneration::SmolrunnerV1,
            ImmutableGitObjectPoolPhysicalAuditRole::Candidate,
            &binding,
            &facts,
        )
        .unwrap();

        assert_eq!(
            old,
            Sha256Digest::parse(
                "sha256:2de01a5d84039800815a41f32fd5ef0e5417ab77684a71e54d7b195c82a90c2c"
            )
            .unwrap()
        );
        assert_ne!(old, new);
        assert_ne!(old, candidate);
    }

    #[test]
    fn strict_marker_header_and_length_fail_closed() {
        let binding = base_binding();
        let marker = ImmutableGitObjectPoolMarker::new(
            &binding,
            GitObjectPoolMarkerNonce::new([1; 16]).unwrap(),
        )
        .unwrap();
        let valid = marker.encode().unwrap();

        assert_eq!(
            ImmutableGitObjectPoolMarker::decode_and_verify(&valid[..63], &binding)
                .unwrap_err()
                .kind(),
            ImmutableGitObjectPoolMarkerErrorKind::InvalidMarker
        );

        for index in [0_usize, 8, 9] {
            let mut changed = valid;
            changed[index] ^= 1;
            assert_eq!(
                ImmutableGitObjectPoolMarker::decode_and_verify(&changed, &binding)
                    .unwrap_err()
                    .kind(),
                ImmutableGitObjectPoolMarkerErrorKind::InvalidMarker
            );
        }
    }

    #[test]
    fn zero_nonce_and_wrong_binding_are_refused() {
        assert_eq!(
            GitObjectPoolMarkerNonce::new([0; 16]).unwrap_err().kind(),
            ImmutableGitObjectPoolMarkerErrorKind::InvalidNonce
        );

        let binding = base_binding();
        let marker = ImmutableGitObjectPoolMarker::new(
            &binding,
            GitObjectPoolMarkerNonce::new([9; 16]).unwrap(),
        )
        .unwrap();
        let encoded = marker.encode().unwrap();
        let other = make_binding(
            "github.com/teamleaderleo/smolrunner",
            "pool-a",
            2,
            "disk-a",
            1,
            GitObjectFormat::Sha1,
            "producer-a",
            "trust-a",
        );
        assert_eq!(
            ImmutableGitObjectPoolMarker::decode_and_verify(&encoded, &other)
                .unwrap_err()
                .kind(),
            ImmutableGitObjectPoolMarkerErrorKind::BindingMismatch
        );
    }

    #[test]
    fn marker_debug_does_not_expose_raw_binding_or_nonce() {
        let binding = base_binding();
        let marker = ImmutableGitObjectPoolMarker::new_for_generation(
            ImmutableGitObjectPoolIdentityGeneration::GlaedaV2,
            &binding,
            GitObjectPoolMarkerNonce::new([0xab; 16]).unwrap(),
        )
        .unwrap();
        let debug = format!("{marker:?}");
        assert!(!debug.contains("pool-a"));
        assert!(!debug.contains("disk-a"));
        assert!(!debug.contains("producer-a"));
        assert!(!debug.contains("trust-a"));
        assert!(!debug.contains("abab"));
        assert!(debug.contains("sha256:"));
        assert!(debug.contains("GlaedaV2"));
    }
}
