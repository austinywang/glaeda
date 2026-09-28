//! Pure catalog-wide authority vocabulary for protected cache generations.
//!
//! This module defines one strict, bounded, path-free document for the first reviewed
//! `cargo-target` generation family. It does not discover, adopt, write, quarantine, restore, or
//! delete physical cache state. Decoding caller-supplied bytes carries observation authority only.
//! The protected store may build crate-sealed revision successors from an exact loaded snapshot,
//! but no public producer can yet authorize a physical generation. Lease-store visibility and
//! replacement-equivalence receipts remain required before any physical state can be managed.

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::artifact::Sha256Digest;
use crate::cache_inventory::CacheStateId;

pub const PROTECTED_CACHE_GENERATION_CATALOG_SCHEMA_VERSION: u8 = 1;
pub const MAX_PROTECTED_CACHE_GENERATION_CATALOG_BYTES: usize = 1_048_576;
pub const MAX_PROTECTED_CACHE_GENERATIONS: usize = 128;
pub const MAX_PROTECTED_CACHE_RECOVERY_STATE_IDS: usize = 2;
const MAX_PROTECTED_CACHE_CATALOG_REVISION: u64 = 1_000_000_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtectedCacheGenerationFamily {
    CargoTargetV1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtectedCacheCatalogAuthority {
    SuppliedDocumentOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct ProtectedCacheCatalogRevision(u64);

impl ProtectedCacheCatalogRevision {
    /// Construct one bounded positive catalog revision.
    ///
    /// # Errors
    ///
    /// Returns an error for zero or a value beyond the reviewed revision range.
    pub fn new(value: u64) -> Result<Self, ProtectedCacheCatalogError> {
        if !(1..=MAX_PROTECTED_CACHE_CATALOG_REVISION).contains(&value) {
            return Err(catalog_error(
                ProtectedCacheCatalogErrorKind::CorruptState,
                "protected cache catalog revision is outside the bounded positive range",
            ));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    fn next(self) -> Result<Self, ProtectedCacheCatalogError> {
        let Some(next) = self.0.checked_add(1) else {
            return Err(catalog_error(
                ProtectedCacheCatalogErrorKind::RevisionConflict,
                "protected cache catalog revision space is exhausted",
            ));
        };
        Self::new(next).map_err(|_| {
            catalog_error(
                ProtectedCacheCatalogErrorKind::RevisionConflict,
                "protected cache catalog revision space is exhausted",
            )
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ProtectedCacheNamespaceIdentity(Sha256Digest);

impl ProtectedCacheNamespaceIdentity {
    /// Parse one canonical, path-free protected namespace identity.
    ///
    /// # Errors
    ///
    /// Returns an error unless the value is canonical SHA-256.
    pub fn parse(value: &str) -> Result<Self, ProtectedCacheCatalogError> {
        Sha256Digest::parse(value).map(Self).map_err(|_| {
            catalog_error(
                ProtectedCacheCatalogErrorKind::CorruptState,
                "protected cache namespace identity is invalid",
            )
        })
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ProtectedCacheGenerationIdentity(Sha256Digest);

impl ProtectedCacheGenerationIdentity {
    /// Parse one canonical, path-free materialized generation identity.
    ///
    /// # Errors
    ///
    /// Returns an error unless the value is canonical SHA-256.
    pub fn parse(value: &str) -> Result<Self, ProtectedCacheCatalogError> {
        Sha256Digest::parse(value).map(Self).map_err(|_| {
            catalog_error(
                ProtectedCacheCatalogErrorKind::CorruptState,
                "protected cache generation identity is invalid",
            )
        })
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtectedCacheGenerationLifecycle {
    Current,
    Retired,
    Quarantined,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtectedCacheGenerationEntry {
    state_id: CacheStateId,
    generation_identity: ProtectedCacheGenerationIdentity,
    lifecycle: ProtectedCacheGenerationLifecycle,
}

impl ProtectedCacheGenerationEntry {
    #[must_use]
    pub const fn state_id(&self) -> &CacheStateId {
        &self.state_id
    }

    #[must_use]
    pub const fn generation_identity(&self) -> &ProtectedCacheGenerationIdentity {
        &self.generation_identity
    }

    #[must_use]
    pub const fn lifecycle(&self) -> ProtectedCacheGenerationLifecycle {
        self.lifecycle
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtectedCacheCatalogRecovery {
    Clean,
    Required {
        affected_state_ids: Vec<CacheStateId>,
    },
}

impl ProtectedCacheCatalogRecovery {
    #[must_use]
    pub const fn is_required(&self) -> bool {
        matches!(self, Self::Required { .. })
    }

    #[must_use]
    pub fn affected_state_ids(&self) -> &[CacheStateId] {
        match self {
            Self::Clean => &[],
            Self::Required { affected_state_ids } => affected_state_ids,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtectedCacheCatalogCorrelation {
    Absent,
    Current,
    Retired,
    Quarantined,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtectedCacheGenerationCatalogDocument {
    schema_version: u8,
    family: ProtectedCacheGenerationFamily,
    namespace_identity: ProtectedCacheNamespaceIdentity,
    revision: ProtectedCacheCatalogRevision,
    current_state_id: Option<CacheStateId>,
    generations: Vec<ProtectedCacheGenerationEntry>,
    recovery: ProtectedCacheCatalogRecovery,
}

impl ProtectedCacheGenerationCatalogDocument {
    /// Construct the only production-created v1 document: an empty, clean catalog at revision one.
    ///
    /// This constructor grants no physical ownership. Later generation publication requires a
    /// separately reviewed protected store and replacement-equivalence contract.
    #[must_use]
    pub const fn empty(namespace_identity: ProtectedCacheNamespaceIdentity) -> Self {
        Self {
            schema_version: PROTECTED_CACHE_GENERATION_CATALOG_SCHEMA_VERSION,
            family: ProtectedCacheGenerationFamily::CargoTargetV1,
            namespace_identity,
            revision: ProtectedCacheCatalogRevision(1),
            current_state_id: None,
            generations: Vec::new(),
            recovery: ProtectedCacheCatalogRecovery::Clean,
        }
    }

    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
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
    pub const fn revision(&self) -> ProtectedCacheCatalogRevision {
        self.revision
    }

    #[must_use]
    pub const fn authority(&self) -> ProtectedCacheCatalogAuthority {
        ProtectedCacheCatalogAuthority::SuppliedDocumentOnly
    }

    #[must_use]
    pub const fn observed_current_state_id(&self) -> Option<&CacheStateId> {
        self.current_state_id.as_ref()
    }

    #[must_use]
    pub fn generations(&self) -> &[ProtectedCacheGenerationEntry] {
        &self.generations
    }

    #[must_use]
    pub const fn recovery(&self) -> &ProtectedCacheCatalogRecovery {
        &self.recovery
    }

    /// Build one crate-sealed current-generation successor from an exact clean revision.
    ///
    /// This only advances path-free catalog metadata. The protected store keeps the resulting
    /// snapshot at `protected_store_snapshot_only`; no public producer can call this transition,
    /// and it grants no physical ownership, adoption, reconstruction, lease, or cleanup authority.
    pub(crate) fn prepare_current_transition(
        &self,
        expected_revision: ProtectedCacheCatalogRevision,
        state_id: CacheStateId,
        generation_identity: ProtectedCacheGenerationIdentity,
    ) -> Result<Self, ProtectedCacheCatalogError> {
        self.validate()?;
        if self.revision != expected_revision {
            return Err(catalog_error(
                ProtectedCacheCatalogErrorKind::RevisionConflict,
                "protected cache catalog revision changed",
            ));
        }
        if self.recovery.is_required() {
            return Err(catalog_error(
                ProtectedCacheCatalogErrorKind::RecoveryRequired,
                "protected cache catalog recovery is required",
            ));
        }
        if self.generations.len() >= MAX_PROTECTED_CACHE_GENERATIONS {
            return Err(corrupt(
                "protected cache catalog generation limit is exhausted",
            ));
        }
        if self.generations.iter().any(|entry| {
            entry.state_id == state_id || entry.generation_identity == generation_identity
        }) {
            return Err(corrupt(
                "protected cache catalog transition repeats an existing identity",
            ));
        }

        let mut generations = self.generations.clone();
        for entry in &mut generations {
            if entry.lifecycle == ProtectedCacheGenerationLifecycle::Current {
                entry.lifecycle = ProtectedCacheGenerationLifecycle::Retired;
            }
        }
        generations.push(ProtectedCacheGenerationEntry {
            state_id: state_id.clone(),
            generation_identity,
            lifecycle: ProtectedCacheGenerationLifecycle::Current,
        });
        generations.sort_by(|left, right| left.state_id.cmp(&right.state_id));
        let successor = Self {
            schema_version: self.schema_version,
            family: self.family,
            namespace_identity: self.namespace_identity.clone(),
            revision: self.revision.next()?,
            current_state_id: Some(state_id),
            generations,
            recovery: ProtectedCacheCatalogRecovery::Clean,
        };
        successor.validate()?;
        Ok(successor)
    }

    /// Require `self` to be the one exact current-generation successor of `previous`.
    pub(crate) fn require_exact_current_successor(
        &self,
        previous: &Self,
    ) -> Result<(), ProtectedCacheCatalogError> {
        self.validate()?;
        previous.validate()?;
        if self.schema_version != previous.schema_version
            || self.family != previous.family
            || self.namespace_identity != previous.namespace_identity
            || self.generations.len() != previous.generations.len() + 1
        {
            return Err(corrupt(
                "protected cache catalog is not an exact current-generation successor",
            ));
        }
        let Some(current_state_id) = self.current_state_id.as_ref() else {
            return Err(corrupt(
                "protected cache catalog successor has no current generation",
            ));
        };
        let current = self
            .generations
            .binary_search_by(|entry| entry.state_id.cmp(current_state_id))
            .ok()
            .and_then(|index| self.generations.get(index))
            .ok_or_else(|| {
                corrupt("protected cache catalog successor current generation is missing")
            })?;
        let expected = previous.prepare_current_transition(
            previous.revision,
            current.state_id.clone(),
            current.generation_identity.clone(),
        )?;
        if self != &expected {
            return Err(corrupt(
                "protected cache catalog is not an exact current-generation successor",
            ));
        }
        Ok(())
    }

    /// Correlate one path-free state identity only at the caller's exact catalog revision.
    ///
    /// This is equality vocabulary, not ownership or cleanup authority. Recovery debt blocks every
    /// state, including entries not named by the recovery record, because current-generation
    /// authority is catalog-wide.
    ///
    /// # Errors
    ///
    /// Returns an error on revision mismatch or any recorded recovery debt.
    pub fn correlate(
        &self,
        expected_revision: ProtectedCacheCatalogRevision,
        state_id: &CacheStateId,
    ) -> Result<ProtectedCacheCatalogCorrelation, ProtectedCacheCatalogError> {
        if self.revision != expected_revision {
            return Err(catalog_error(
                ProtectedCacheCatalogErrorKind::RevisionConflict,
                "protected cache catalog revision changed",
            ));
        }
        if self.recovery.is_required() {
            return Err(catalog_error(
                ProtectedCacheCatalogErrorKind::RecoveryRequired,
                "protected cache catalog recovery is required",
            ));
        }
        Ok(self
            .generations
            .binary_search_by(|entry| entry.state_id.cmp(state_id))
            .map_or(
                ProtectedCacheCatalogCorrelation::Absent,
                |index| match self.generations[index].lifecycle {
                    ProtectedCacheGenerationLifecycle::Current => {
                        ProtectedCacheCatalogCorrelation::Current
                    }
                    ProtectedCacheGenerationLifecycle::Retired => {
                        ProtectedCacheCatalogCorrelation::Retired
                    }
                    ProtectedCacheGenerationLifecycle::Quarantined => {
                        ProtectedCacheCatalogCorrelation::Quarantined
                    }
                },
            ))
    }

    fn validate(&self) -> Result<(), ProtectedCacheCatalogError> {
        if self.schema_version != PROTECTED_CACHE_GENERATION_CATALOG_SCHEMA_VERSION {
            return Err(corrupt("protected cache catalog schema version is invalid"));
        }
        ProtectedCacheCatalogRevision::new(self.revision.get())?;
        if self.generations.len() > MAX_PROTECTED_CACHE_GENERATIONS {
            return Err(corrupt(
                "protected cache catalog contains too many generations",
            ));
        }

        let mut state_ids = BTreeSet::new();
        let mut generation_identities = BTreeSet::new();
        let mut current = None;
        for entry in &self.generations {
            if !state_ids.insert(entry.state_id.clone()) {
                return Err(corrupt("protected cache catalog repeats a state identity"));
            }
            if !generation_identities.insert(entry.generation_identity.clone()) {
                return Err(corrupt(
                    "protected cache catalog repeats a generation identity",
                ));
            }
            if entry.lifecycle == ProtectedCacheGenerationLifecycle::Current
                && current.replace(&entry.state_id).is_some()
            {
                return Err(corrupt(
                    "protected cache catalog contains multiple current generations",
                ));
            }
        }
        if !self
            .generations
            .windows(2)
            .all(|pair| pair[0].state_id < pair[1].state_id)
        {
            return Err(corrupt(
                "protected cache catalog generations are not canonically ordered",
            ));
        }
        if current != self.current_state_id.as_ref() {
            return Err(corrupt(
                "protected cache current pointer and lifecycle disagree",
            ));
        }

        if let ProtectedCacheCatalogRecovery::Required { affected_state_ids } = &self.recovery {
            if affected_state_ids.is_empty()
                || affected_state_ids.len() > MAX_PROTECTED_CACHE_RECOVERY_STATE_IDS
                || !affected_state_ids.windows(2).all(|pair| pair[0] < pair[1])
            {
                return Err(corrupt(
                    "protected cache recovery identities are invalid or unordered",
                ));
            }
            if affected_state_ids
                .iter()
                .any(|state_id| !state_ids.contains(state_id))
            {
                return Err(corrupt(
                    "protected cache recovery refers to an unknown generation",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Deserialize)]
struct CatalogVersionWire {
    schema_version: u8,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogWire {
    schema_version: u8,
    family: FamilyWire,
    namespace_identity: String,
    revision: u64,
    current_state_id: Option<String>,
    generations: Vec<GenerationWire>,
    recovery: RecoveryWire,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FamilyWire {
    CargoTargetV1,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GenerationWire {
    state_id: String,
    generation_identity: String,
    lifecycle: LifecycleWire,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LifecycleWire {
    Current,
    Retired,
    Quarantined,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
enum RecoveryWire {
    Clean,
    Required { affected_state_ids: Vec<String> },
}

/// Encode one fully validated catalog as canonical bounded JSON.
///
/// # Errors
///
/// Returns an error for invalid invariants or an oversized document.
pub fn encode_protected_cache_generation_catalog(
    document: &ProtectedCacheGenerationCatalogDocument,
) -> Result<Vec<u8>, ProtectedCacheCatalogError> {
    document.validate()?;
    let wire = CatalogWire {
        schema_version: document.schema_version,
        family: FamilyWire::CargoTargetV1,
        namespace_identity: document.namespace_identity.as_str().to_owned(),
        revision: document.revision.get(),
        current_state_id: document
            .current_state_id
            .as_ref()
            .map(CacheStateId::as_str)
            .map(str::to_owned),
        generations: document
            .generations
            .iter()
            .map(|entry| GenerationWire {
                state_id: entry.state_id.as_str().to_owned(),
                generation_identity: entry.generation_identity.as_str().to_owned(),
                lifecycle: match entry.lifecycle {
                    ProtectedCacheGenerationLifecycle::Current => LifecycleWire::Current,
                    ProtectedCacheGenerationLifecycle::Retired => LifecycleWire::Retired,
                    ProtectedCacheGenerationLifecycle::Quarantined => LifecycleWire::Quarantined,
                },
            })
            .collect(),
        recovery: match &document.recovery {
            ProtectedCacheCatalogRecovery::Clean => RecoveryWire::Clean,
            ProtectedCacheCatalogRecovery::Required { affected_state_ids } => {
                RecoveryWire::Required {
                    affected_state_ids: affected_state_ids
                        .iter()
                        .map(CacheStateId::as_str)
                        .map(str::to_owned)
                        .collect(),
                }
            }
        },
    };
    let bytes = serde_json::to_vec(&wire).map_err(|_| {
        catalog_error(
            ProtectedCacheCatalogErrorKind::InvalidDocument,
            "protected cache catalog cannot encode",
        )
    })?;
    if bytes.len() > MAX_PROTECTED_CACHE_GENERATION_CATALOG_BYTES {
        return Err(catalog_error(
            ProtectedCacheCatalogErrorKind::DocumentTooLarge,
            "protected cache catalog exceeds the reviewed byte limit",
        ));
    }
    Ok(bytes)
}

/// Decode, canonicalize, and validate one supplied protected-cache catalog document.
///
/// Decoding does not prove that the bytes came from a protected store. The returned document keeps
/// `supplied_document_only` authority and cannot adopt or mutate physical cache state.
///
/// # Errors
///
/// Returns an error for malformed, oversized, future-version, noncanonical, conflicting, or
/// recovery-inconsistent documents.
pub fn decode_protected_cache_generation_catalog(
    bytes: &[u8],
) -> Result<ProtectedCacheGenerationCatalogDocument, ProtectedCacheCatalogError> {
    if bytes.len() > MAX_PROTECTED_CACHE_GENERATION_CATALOG_BYTES {
        return Err(catalog_error(
            ProtectedCacheCatalogErrorKind::DocumentTooLarge,
            "protected cache catalog exceeds the reviewed byte limit",
        ));
    }
    let version: CatalogVersionWire = serde_json::from_slice(bytes).map_err(|_| {
        catalog_error(
            ProtectedCacheCatalogErrorKind::InvalidDocument,
            "protected cache catalog JSON is invalid",
        )
    })?;
    if version.schema_version != PROTECTED_CACHE_GENERATION_CATALOG_SCHEMA_VERSION {
        return Err(catalog_error(
            ProtectedCacheCatalogErrorKind::VersionIncompatible,
            "protected cache catalog schema version is unsupported",
        ));
    }
    let wire: CatalogWire = serde_json::from_slice(bytes).map_err(|_| {
        catalog_error(
            ProtectedCacheCatalogErrorKind::InvalidDocument,
            "protected cache catalog JSON is invalid",
        )
    })?;
    if wire.generations.len() > MAX_PROTECTED_CACHE_GENERATIONS {
        return Err(corrupt(
            "protected cache catalog contains too many generations",
        ));
    }

    let namespace_identity = ProtectedCacheNamespaceIdentity::parse(&wire.namespace_identity)?;
    let revision = ProtectedCacheCatalogRevision::new(wire.revision)?;
    let current_state_id = wire
        .current_state_id
        .as_deref()
        .map(CacheStateId::parse)
        .transpose()
        .map_err(|_| corrupt("protected cache current state identity is invalid"))?;
    let mut generations = wire
        .generations
        .into_iter()
        .map(|entry| {
            Ok(ProtectedCacheGenerationEntry {
                state_id: CacheStateId::parse(&entry.state_id)
                    .map_err(|_| corrupt("protected cache state identity is invalid"))?,
                generation_identity: ProtectedCacheGenerationIdentity::parse(
                    &entry.generation_identity,
                )?,
                lifecycle: match entry.lifecycle {
                    LifecycleWire::Current => ProtectedCacheGenerationLifecycle::Current,
                    LifecycleWire::Retired => ProtectedCacheGenerationLifecycle::Retired,
                    LifecycleWire::Quarantined => ProtectedCacheGenerationLifecycle::Quarantined,
                },
            })
        })
        .collect::<Result<Vec<_>, ProtectedCacheCatalogError>>()?;
    generations.sort_by(|left, right| left.state_id.cmp(&right.state_id));
    let recovery = match wire.recovery {
        RecoveryWire::Clean => ProtectedCacheCatalogRecovery::Clean,
        RecoveryWire::Required { affected_state_ids } => {
            let mut affected_state_ids = affected_state_ids
                .into_iter()
                .map(|value| {
                    CacheStateId::parse(&value)
                        .map_err(|_| corrupt("protected cache recovery state identity is invalid"))
                })
                .collect::<Result<Vec<_>, ProtectedCacheCatalogError>>()?;
            affected_state_ids.sort();
            ProtectedCacheCatalogRecovery::Required { affected_state_ids }
        }
    };
    let document = ProtectedCacheGenerationCatalogDocument {
        schema_version: PROTECTED_CACHE_GENERATION_CATALOG_SCHEMA_VERSION,
        family: match wire.family {
            FamilyWire::CargoTargetV1 => ProtectedCacheGenerationFamily::CargoTargetV1,
        },
        namespace_identity,
        revision,
        current_state_id,
        generations,
        recovery,
    };
    document.validate()?;
    if encode_protected_cache_generation_catalog(&document)? != bytes {
        return Err(catalog_error(
            ProtectedCacheCatalogErrorKind::NonCanonical,
            "protected cache catalog is not canonical JSON",
        ));
    }
    Ok(document)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtectedCacheCatalogErrorKind {
    InvalidDocument,
    VersionIncompatible,
    DocumentTooLarge,
    NonCanonical,
    CorruptState,
    RevisionConflict,
    RecoveryRequired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProtectedCacheCatalogError {
    kind: ProtectedCacheCatalogErrorKind,
    message: &'static str,
}

impl ProtectedCacheCatalogError {
    #[must_use]
    pub const fn kind(&self) -> ProtectedCacheCatalogErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn message(&self) -> &'static str {
        self.message
    }
}

impl fmt::Display for ProtectedCacheCatalogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for ProtectedCacheCatalogError {}

const fn catalog_error(
    kind: ProtectedCacheCatalogErrorKind,
    message: &'static str,
) -> ProtectedCacheCatalogError {
    ProtectedCacheCatalogError { kind, message }
}

const fn corrupt(message: &'static str) -> ProtectedCacheCatalogError {
    catalog_error(ProtectedCacheCatalogErrorKind::CorruptState, message)
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn digest(character: char) -> String {
        format!("sha256:{}", character.to_string().repeat(64))
    }

    fn state(value: &str) -> CacheStateId {
        CacheStateId::parse(value).expect("state ID")
    }

    fn generation(
        state_id: &str,
        character: char,
        lifecycle: ProtectedCacheGenerationLifecycle,
    ) -> ProtectedCacheGenerationEntry {
        ProtectedCacheGenerationEntry {
            state_id: state(state_id),
            generation_identity: ProtectedCacheGenerationIdentity::parse(&digest(character))
                .expect("generation identity"),
            lifecycle,
        }
    }

    fn catalog(
        revision: u64,
        current_state_id: Option<&str>,
        generations: Vec<ProtectedCacheGenerationEntry>,
        recovery: ProtectedCacheCatalogRecovery,
    ) -> ProtectedCacheGenerationCatalogDocument {
        ProtectedCacheGenerationCatalogDocument {
            schema_version: PROTECTED_CACHE_GENERATION_CATALOG_SCHEMA_VERSION,
            family: ProtectedCacheGenerationFamily::CargoTargetV1,
            namespace_identity: ProtectedCacheNamespaceIdentity::parse(&digest('a'))
                .expect("namespace"),
            revision: ProtectedCacheCatalogRevision::new(revision).expect("revision"),
            current_state_id: current_state_id.map(state),
            generations,
            recovery,
        }
    }

    fn current_and_retired() -> ProtectedCacheGenerationCatalogDocument {
        catalog(
            7,
            Some("state-current"),
            vec![
                generation(
                    "state-current",
                    'b',
                    ProtectedCacheGenerationLifecycle::Current,
                ),
                generation(
                    "state-retired",
                    'c',
                    ProtectedCacheGenerationLifecycle::Retired,
                ),
            ],
            ProtectedCacheCatalogRecovery::Clean,
        )
    }

    #[test]
    fn empty_catalog_is_canonical_and_observation_only() {
        let document = ProtectedCacheGenerationCatalogDocument::empty(
            ProtectedCacheNamespaceIdentity::parse(&digest('a')).expect("namespace"),
        );
        let bytes = encode_protected_cache_generation_catalog(&document).expect("encode");
        let decoded = decode_protected_cache_generation_catalog(&bytes).expect("decode");

        assert_eq!(decoded, document);
        assert_eq!(decoded.revision().get(), 1);
        assert_eq!(
            decoded.authority(),
            ProtectedCacheCatalogAuthority::SuppliedDocumentOnly
        );
        assert_eq!(
            decoded.family(),
            ProtectedCacheGenerationFamily::CargoTargetV1
        );
        assert!(decoded.observed_current_state_id().is_none());
        assert!(decoded.generations().is_empty());
        assert_eq!(decoded.recovery(), &ProtectedCacheCatalogRecovery::Clean);
    }

    #[test]
    fn exact_revision_correlation_is_path_free_and_catalog_wide() {
        let document = current_and_retired();
        let bytes = encode_protected_cache_generation_catalog(&document).expect("encode");
        let decoded = decode_protected_cache_generation_catalog(&bytes).expect("decode");
        let revision = decoded.revision();

        assert_eq!(
            decoded.correlate(revision, &state("state-current")),
            Ok(ProtectedCacheCatalogCorrelation::Current)
        );
        assert_eq!(
            decoded.correlate(revision, &state("state-retired")),
            Ok(ProtectedCacheCatalogCorrelation::Retired)
        );
        assert_eq!(
            decoded.correlate(revision, &state("state-absent")),
            Ok(ProtectedCacheCatalogCorrelation::Absent)
        );
        let rendered = String::from_utf8(bytes).expect("UTF-8");
        for forbidden in ["/home/", "/tmp/", "target/", "file:"] {
            assert!(!rendered.contains(forbidden));
        }
    }

    #[test]
    fn multiple_current_generations_and_pointer_mismatch_fail_closed() {
        let multiple = catalog(
            3,
            Some("state-one"),
            vec![
                generation("state-one", 'b', ProtectedCacheGenerationLifecycle::Current),
                generation("state-two", 'c', ProtectedCacheGenerationLifecycle::Current),
            ],
            ProtectedCacheCatalogRecovery::Clean,
        );
        assert_eq!(
            encode_protected_cache_generation_catalog(&multiple)
                .expect_err("multiple current generations")
                .kind(),
            ProtectedCacheCatalogErrorKind::CorruptState
        );

        let mismatch = catalog(
            3,
            Some("state-two"),
            vec![
                generation("state-one", 'b', ProtectedCacheGenerationLifecycle::Current),
                generation("state-two", 'c', ProtectedCacheGenerationLifecycle::Retired),
            ],
            ProtectedCacheCatalogRecovery::Clean,
        );
        assert_eq!(
            encode_protected_cache_generation_catalog(&mismatch)
                .expect_err("pointer mismatch")
                .kind(),
            ProtectedCacheCatalogErrorKind::CorruptState
        );
    }

    #[test]
    fn duplicate_state_or_generation_identity_fails_closed() {
        let duplicate_state = catalog(
            2,
            None,
            vec![
                generation("state-one", 'b', ProtectedCacheGenerationLifecycle::Retired),
                generation("state-one", 'c', ProtectedCacheGenerationLifecycle::Retired),
            ],
            ProtectedCacheCatalogRecovery::Clean,
        );
        assert_eq!(
            encode_protected_cache_generation_catalog(&duplicate_state)
                .expect_err("duplicate state")
                .kind(),
            ProtectedCacheCatalogErrorKind::CorruptState
        );

        let duplicate_generation = catalog(
            2,
            None,
            vec![
                generation("state-one", 'b', ProtectedCacheGenerationLifecycle::Retired),
                generation("state-two", 'b', ProtectedCacheGenerationLifecycle::Retired),
            ],
            ProtectedCacheCatalogRecovery::Clean,
        );
        assert_eq!(
            encode_protected_cache_generation_catalog(&duplicate_generation)
                .expect_err("duplicate generation")
                .kind(),
            ProtectedCacheCatalogErrorKind::CorruptState
        );
    }

    #[test]
    fn revision_conflict_and_recovery_debt_block_every_correlation() {
        let document = current_and_retired();
        let conflict = document
            .correlate(
                ProtectedCacheCatalogRevision::new(6).expect("revision"),
                &state("state-current"),
            )
            .expect_err("revision conflict");
        assert_eq!(
            conflict.kind(),
            ProtectedCacheCatalogErrorKind::RevisionConflict
        );

        let recovery = catalog(
            8,
            Some("state-current"),
            document.generations.clone(),
            ProtectedCacheCatalogRecovery::Required {
                affected_state_ids: vec![state("state-current"), state("state-retired")],
            },
        );
        for candidate in ["state-current", "state-retired", "state-absent"] {
            let error = recovery
                .correlate(recovery.revision(), &state(candidate))
                .expect_err("recovery blocks the whole catalog");
            assert_eq!(
                error.kind(),
                ProtectedCacheCatalogErrorKind::RecoveryRequired
            );
        }
    }

    #[test]
    fn current_transition_advances_once_and_retires_the_previous_current() {
        let empty = ProtectedCacheGenerationCatalogDocument::empty(
            ProtectedCacheNamespaceIdentity::parse(&digest('a')).expect("namespace"),
        );
        let first = empty
            .prepare_current_transition(
                empty.revision(),
                state("state-first"),
                ProtectedCacheGenerationIdentity::parse(&digest('b')).expect("generation"),
            )
            .expect("prepare first current");
        assert_eq!(first.revision().get(), 2);
        assert_eq!(
            first.observed_current_state_id(),
            Some(&state("state-first"))
        );
        assert_eq!(
            first.correlate(first.revision(), &state("state-first")),
            Ok(ProtectedCacheCatalogCorrelation::Current)
        );
        first
            .require_exact_current_successor(&empty)
            .expect("first exact successor");

        let second = first
            .prepare_current_transition(
                first.revision(),
                state("state-second"),
                ProtectedCacheGenerationIdentity::parse(&digest('c')).expect("generation"),
            )
            .expect("prepare second current");
        assert_eq!(second.revision().get(), 3);
        assert_eq!(
            second.correlate(second.revision(), &state("state-first")),
            Ok(ProtectedCacheCatalogCorrelation::Retired)
        );
        assert_eq!(
            second.correlate(second.revision(), &state("state-second")),
            Ok(ProtectedCacheCatalogCorrelation::Current)
        );
        second
            .require_exact_current_successor(&first)
            .expect("second exact successor");
    }

    #[test]
    fn current_transition_refuses_stale_duplicate_recovery_limit_and_non_successor_state() {
        let base = current_and_retired();
        assert_eq!(
            base.prepare_current_transition(
                ProtectedCacheCatalogRevision::new(6).expect("stale revision"),
                state("state-next"),
                ProtectedCacheGenerationIdentity::parse(&digest('d')).expect("generation"),
            )
            .expect_err("stale revision")
            .kind(),
            ProtectedCacheCatalogErrorKind::RevisionConflict
        );
        for (state_id, generation_identity) in [
            (state("state-current"), digest('d')),
            (state("state-next"), digest('b')),
        ] {
            assert_eq!(
                base.prepare_current_transition(
                    base.revision(),
                    state_id,
                    ProtectedCacheGenerationIdentity::parse(&generation_identity)
                        .expect("generation"),
                )
                .expect_err("duplicate identity")
                .kind(),
                ProtectedCacheCatalogErrorKind::CorruptState
            );
        }

        let recovery = catalog(
            8,
            Some("state-current"),
            base.generations.clone(),
            ProtectedCacheCatalogRecovery::Required {
                affected_state_ids: vec![state("state-current")],
            },
        );
        assert_eq!(
            recovery
                .prepare_current_transition(
                    recovery.revision(),
                    state("state-next"),
                    ProtectedCacheGenerationIdentity::parse(&digest('d')).expect("generation"),
                )
                .expect_err("recovery debt")
                .kind(),
            ProtectedCacheCatalogErrorKind::RecoveryRequired
        );

        let exhausted = catalog(
            MAX_PROTECTED_CACHE_CATALOG_REVISION,
            None,
            Vec::new(),
            ProtectedCacheCatalogRecovery::Clean,
        );
        assert_eq!(
            exhausted
                .prepare_current_transition(
                    exhausted.revision(),
                    state("state-next"),
                    ProtectedCacheGenerationIdentity::parse(&digest('d')).expect("generation"),
                )
                .expect_err("revision exhaustion")
                .kind(),
            ProtectedCacheCatalogErrorKind::RevisionConflict
        );

        let full = catalog(
            9,
            None,
            (0..MAX_PROTECTED_CACHE_GENERATIONS)
                .map(|index| ProtectedCacheGenerationEntry {
                    state_id: state(&format!("state-{index:03}")),
                    generation_identity: ProtectedCacheGenerationIdentity::parse(&format!(
                        "sha256:{index:064x}"
                    ))
                    .expect("generation"),
                    lifecycle: ProtectedCacheGenerationLifecycle::Retired,
                })
                .collect(),
            ProtectedCacheCatalogRecovery::Clean,
        );
        assert_eq!(
            full.prepare_current_transition(
                full.revision(),
                state("state-next"),
                ProtectedCacheGenerationIdentity::parse(&format!(
                    "sha256:{:064x}",
                    MAX_PROTECTED_CACHE_GENERATIONS
                ))
                .expect("generation"),
            )
            .expect_err("generation limit")
            .kind(),
            ProtectedCacheCatalogErrorKind::CorruptState
        );

        let mut altered = base
            .prepare_current_transition(
                base.revision(),
                state("state-next"),
                ProtectedCacheGenerationIdentity::parse(&digest('d')).expect("generation"),
            )
            .expect("candidate successor");
        altered.generations[0].generation_identity =
            ProtectedCacheGenerationIdentity::parse(&digest('e')).expect("altered generation");
        assert_eq!(
            altered
                .require_exact_current_successor(&base)
                .expect_err("altered prior entry")
                .kind(),
            ProtectedCacheCatalogErrorKind::CorruptState
        );
    }

    #[test]
    fn recovery_must_be_bounded_ordered_and_refer_to_catalogued_states() {
        let base = current_and_retired();
        for affected_state_ids in [
            vec![],
            vec![state("state-current"), state("state-current")],
            vec![state("state-missing")],
            vec![
                state("state-current"),
                state("state-retired"),
                state("state-third"),
            ],
        ] {
            let invalid = catalog(
                8,
                Some("state-current"),
                base.generations.clone(),
                ProtectedCacheCatalogRecovery::Required { affected_state_ids },
            );
            assert_eq!(
                encode_protected_cache_generation_catalog(&invalid)
                    .expect_err("invalid recovery")
                    .kind(),
                ProtectedCacheCatalogErrorKind::CorruptState
            );
        }
    }

    #[test]
    fn decoder_rejects_noncanonical_unknown_future_and_path_identity_documents() {
        let canonical = encode_protected_cache_generation_catalog(&current_and_retired())
            .expect("canonical catalog");
        let value: Value = serde_json::from_slice(&canonical).expect("JSON");

        let pretty = serde_json::to_vec_pretty(&value).expect("pretty JSON");
        assert_eq!(
            decode_protected_cache_generation_catalog(&pretty)
                .expect_err("pretty JSON is noncanonical")
                .kind(),
            ProtectedCacheCatalogErrorKind::NonCanonical
        );

        let mut future = value.clone();
        future["schema_version"] = json!(2);
        assert_eq!(
            decode_protected_cache_generation_catalog(
                &serde_json::to_vec(&future).expect("future JSON")
            )
            .expect_err("future version")
            .kind(),
            ProtectedCacheCatalogErrorKind::VersionIncompatible
        );

        let mut unknown = value.clone();
        unknown["extra"] = json!(true);
        assert_eq!(
            decode_protected_cache_generation_catalog(
                &serde_json::to_vec(&unknown).expect("unknown JSON")
            )
            .expect_err("unknown field")
            .kind(),
            ProtectedCacheCatalogErrorKind::InvalidDocument
        );

        let mut family = value.clone();
        family["family"] = json!("future_family_v2");
        assert_eq!(
            decode_protected_cache_generation_catalog(
                &serde_json::to_vec(&family).expect("family JSON")
            )
            .expect_err("unknown family")
            .kind(),
            ProtectedCacheCatalogErrorKind::InvalidDocument
        );

        let mut unordered = value.clone();
        unordered["generations"]
            .as_array_mut()
            .expect("generation array")
            .reverse();
        assert_eq!(
            decode_protected_cache_generation_catalog(
                &serde_json::to_vec(&unordered).expect("unordered JSON")
            )
            .expect_err("unordered generations")
            .kind(),
            ProtectedCacheCatalogErrorKind::NonCanonical
        );

        let mut path = value;
        path["generations"][0]["state_id"] = json!("target/private");
        assert_eq!(
            decode_protected_cache_generation_catalog(
                &serde_json::to_vec(&path).expect("path JSON")
            )
            .expect_err("path identity")
            .kind(),
            ProtectedCacheCatalogErrorKind::CorruptState
        );
    }

    #[test]
    fn decoder_rejects_oversized_input_before_parsing() {
        let bytes = vec![b' '; MAX_PROTECTED_CACHE_GENERATION_CATALOG_BYTES + 1];
        assert_eq!(
            decode_protected_cache_generation_catalog(&bytes)
                .expect_err("oversized catalog")
                .kind(),
            ProtectedCacheCatalogErrorKind::DocumentTooLarge
        );
    }
}
