//! Pure product-neutral resource ownership and fail-closed capacity arithmetic.
//!
//! One capacity domain tracks an exact closed set of resource dimensions. Untracked dimensions
//! are absent from both its budget and every valid claim; explicit zero is distinct from absence.
//! A child domain is bound to one exact parent claim and one sealed canonical child-domain
//! specification. Re-materializing that parent may reconstruct the same child ledger; callers
//! cannot select sibling child identities or budgets from the same parent claim.
//!
//! Claims are resource-ownership evidence only. This module grants zero execution, lifecycle,
//! observation, scheduling, spawn, kill, cleanup, residency, mutation, adoption, persistence, or
//! release authority. Production family-to-claim minting is deliberately absent from this slice.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::Serialize;

pub const EXECUTION_CAPACITY_SCHEMA_VERSION: u8 = 1;
pub const MAX_CAPACITY_DOMAIN_GENERATION: u64 = 1_000_000_000_000;

const MAX_CAPACITY_ID_BYTES: usize = 96;
const OPAQUE_ID_HEX_BYTES: usize = 64;
const CAPACITY_DOMAIN_ID_PREFIX: &str = "capacity-domain-v1-";
const CAPACITY_CLAIM_ID_PREFIX: &str = "capacity-claim-v1-";
const MAX_CAPACITY_DIMENSIONS: usize = 4;

macro_rules! opaque_identity_type {
    ($name:ident, $field:literal, $code:literal, $prefix:expr) => {
        #[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn parse(value: &str) -> Result<Self, ExecutionCapacityError> {
                validate_opaque_identity($field, $code, value, $prefix)?;
                Ok(Self(value.to_owned()))
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(concat!(stringify!($name), "(<opaque>)"))
            }
        }
    };
}

opaque_identity_type!(
    CapacityDomainId,
    "domain_id",
    "invalid_capacity_domain_id",
    CAPACITY_DOMAIN_ID_PREFIX
);
opaque_identity_type!(
    CapacityClaimId,
    "claim_id",
    "invalid_capacity_claim_id",
    CAPACITY_CLAIM_ID_PREFIX
);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CapacityDimension {
    CpuMillis,
    MemoryBytes,
    DiskBytes,
    Pids,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct CapacityDimensionSet(Vec<CapacityDimension>);

impl CapacityDimensionSet {
    pub fn new(dimensions: &[CapacityDimension]) -> Result<Self, ExecutionCapacityError> {
        if dimensions.is_empty() || dimensions.len() > MAX_CAPACITY_DIMENSIONS {
            return Err(ExecutionCapacityError::new(
                "tracked_dimensions",
                "invalid_capacity_dimension_set",
                "capacity dimension set must contain one to four tracked dimensions",
            ));
        }

        let mut canonical = dimensions.to_vec();
        canonical.sort_unstable();
        if canonical.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(ExecutionCapacityError::new(
                "tracked_dimensions",
                "duplicate_capacity_dimension",
                "capacity dimension set cannot contain duplicates",
            ));
        }
        Ok(Self(canonical))
    }

    #[must_use]
    pub fn as_slice(&self) -> &[CapacityDimension] {
        &self.0
    }

    #[must_use]
    pub fn contains(&self, dimension: CapacityDimension) -> bool {
        self.0.binary_search(&dimension).is_ok()
    }

    #[must_use]
    pub fn is_subset_of(&self, parent: &Self) -> bool {
        self.0.iter().all(|dimension| parent.contains(*dimension))
    }
}

/// Exact amounts for one canonical tracked-dimension set.
///
/// Only tracked dimensions have entries in `amounts`. Therefore `amount(d) == None` means
/// untracked, while `amount(d) == Some(0)` means the dimension is explicitly tracked at zero.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CapacityAmounts {
    tracked_dimensions: CapacityDimensionSet,
    amounts: BTreeMap<CapacityDimension, u64>,
}

impl CapacityAmounts {
    pub fn new(entries: &[(CapacityDimension, u64)]) -> Result<Self, ExecutionCapacityError> {
        let dimensions = entries
            .iter()
            .map(|(dimension, _)| *dimension)
            .collect::<Vec<_>>();
        let tracked_dimensions = CapacityDimensionSet::new(&dimensions)?;
        let amounts = entries.iter().copied().collect::<BTreeMap<_, _>>();
        Ok(Self {
            tracked_dimensions,
            amounts,
        })
    }

    #[must_use]
    pub fn tracked_dimensions(&self) -> &CapacityDimensionSet {
        &self.tracked_dimensions
    }

    #[must_use]
    pub fn amount(&self, dimension: CapacityDimension) -> Option<u64> {
        self.amounts.get(&dimension).copied()
    }

    fn zeroed(tracked_dimensions: &CapacityDimensionSet) -> Self {
        let amounts = tracked_dimensions
            .as_slice()
            .iter()
            .copied()
            .map(|dimension| (dimension, 0))
            .collect();
        Self {
            tracked_dimensions: tracked_dimensions.clone(),
            amounts,
        }
    }

    fn checked_add(&self, other: &Self) -> Option<Self> {
        if self.tracked_dimensions != other.tracked_dimensions {
            return None;
        }
        let mut next = Self::zeroed(&self.tracked_dimensions);
        for dimension in self.tracked_dimensions.as_slice() {
            let sum = self
                .amount(*dimension)?
                .checked_add(other.amount(*dimension)?)?;
            next.amounts.insert(*dimension, sum);
        }
        Some(next)
    }

    fn fits_within(&self, limit: &Self) -> bool {
        self.tracked_dimensions == limit.tracked_dimensions
            && self.tracked_dimensions.as_slice().iter().all(|dimension| {
                self.amount(*dimension)
                    .zip(limit.amount(*dimension))
                    .is_some_and(|(used, maximum)| used <= maximum)
            })
    }

    fn fits_within_parent(&self, parent: &Self) -> bool {
        self.tracked_dimensions
            .is_subset_of(&parent.tracked_dimensions)
            && self.tracked_dimensions.as_slice().iter().all(|dimension| {
                self.amount(*dimension)
                    .zip(parent.amount(*dimension))
                    .is_some_and(|(used, maximum)| used <= maximum)
            })
    }

    fn remaining_within(&self, limit: &Self) -> Self {
        debug_assert!(self.fits_within(limit));
        let mut remaining = Self::zeroed(&self.tracked_dimensions);
        for dimension in self.tracked_dimensions.as_slice() {
            let value = limit
                .amount(*dimension)
                .expect("tracked limit dimension must have an amount")
                - self
                    .amount(*dimension)
                    .expect("tracked used dimension must have an amount");
            remaining.amounts.insert(*dimension, value);
        }
        remaining
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct CapacityDomainGeneration(u64);

impl CapacityDomainGeneration {
    pub fn new(value: u64) -> Result<Self, ExecutionCapacityError> {
        if !(1..=MAX_CAPACITY_DOMAIN_GENERATION).contains(&value) {
            return Err(ExecutionCapacityError::new(
                "domain_generation",
                "invalid_capacity_domain_generation",
                "capacity domain generation is outside the bounded positive range",
            ));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// The one canonical subordinate capacity ledger sealed into a parent claim.
///
/// There is deliberately no public constructor. A family owner that eventually mints a parent
/// claim must supply this binding at the same trusted boundary as the claim itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CapacityChildDomainBinding {
    domain_id: CapacityDomainId,
    domain_generation: CapacityDomainGeneration,
    budget: CapacityAmounts,
}

impl CapacityChildDomainBinding {
    #[must_use]
    pub fn domain_id(&self) -> &CapacityDomainId {
        &self.domain_id
    }

    #[must_use]
    pub fn domain_generation(&self) -> CapacityDomainGeneration {
        self.domain_generation
    }

    #[must_use]
    pub fn budget(&self) -> &CapacityAmounts {
        &self.budget
    }
}

/// Resource ownership inside one exact capacity domain generation.
///
/// Construction is crate-private so this first carrier creates no production family-to-claim
/// minting surface. A future family owner must explicitly bridge its own authoritative evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CapacityClaim {
    id: CapacityClaimId,
    domain_id: CapacityDomainId,
    domain_generation: CapacityDomainGeneration,
    resources: CapacityAmounts,
    #[serde(skip_serializing_if = "Option::is_none")]
    child_domain_binding: Option<CapacityChildDomainBinding>,
}

impl CapacityClaim {
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn for_domain(
        id: CapacityClaimId,
        domain: &CapacityDomain,
        resources: CapacityAmounts,
    ) -> Result<Self, ExecutionCapacityError> {
        Self::new(id, domain, resources, None)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn for_domain_with_child(
        id: CapacityClaimId,
        domain: &CapacityDomain,
        resources: CapacityAmounts,
        child_domain_id: CapacityDomainId,
        child_domain_generation: CapacityDomainGeneration,
        child_budget: CapacityAmounts,
    ) -> Result<Self, ExecutionCapacityError> {
        let binding = CapacityChildDomainBinding {
            domain_id: child_domain_id,
            domain_generation: child_domain_generation,
            budget: child_budget,
        };
        Self::new(id, domain, resources, Some(binding))
    }

    fn new(
        id: CapacityClaimId,
        domain: &CapacityDomain,
        resources: CapacityAmounts,
        child_domain_binding: Option<CapacityChildDomainBinding>,
    ) -> Result<Self, ExecutionCapacityError> {
        if resources.tracked_dimensions() != domain.budget().tracked_dimensions() {
            return Err(ExecutionCapacityError::new(
                "resources.tracked_dimensions",
                "capacity_dimension_set_mismatch",
                "capacity claim dimensions must exactly match the domain budget",
            ));
        }

        if let Some(binding) = &child_domain_binding {
            validate_child_domain_binding(domain, &resources, binding)?;
        }

        Ok(Self {
            id,
            domain_id: domain.id.clone(),
            domain_generation: domain.generation,
            resources,
            child_domain_binding,
        })
    }

    #[must_use]
    pub fn id(&self) -> &CapacityClaimId {
        &self.id
    }

    #[must_use]
    pub fn domain_id(&self) -> &CapacityDomainId {
        &self.domain_id
    }

    #[must_use]
    pub fn domain_generation(&self) -> CapacityDomainGeneration {
        self.domain_generation
    }

    #[must_use]
    pub fn resources(&self) -> &CapacityAmounts {
        &self.resources
    }

    #[must_use]
    pub fn child_domain_binding(&self) -> Option<&CapacityChildDomainBinding> {
        self.child_domain_binding.as_ref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "scope", rename_all = "snake_case")]
pub enum CapacityDomainScope {
    Root,
    Child { parent_claim: CapacityClaim },
}

/// One exact resource budget. A root domain charges its claims directly. A child domain is a
/// subordinate ledger under one already-owned parent claim; its claims are never charged to the
/// parent domain by this module.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CapacityDomain {
    id: CapacityDomainId,
    generation: CapacityDomainGeneration,
    budget: CapacityAmounts,
    scope: CapacityDomainScope,
}

impl CapacityDomain {
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn root(
        id: CapacityDomainId,
        generation: CapacityDomainGeneration,
        budget: CapacityAmounts,
    ) -> Self {
        Self {
            id,
            generation,
            budget,
            scope: CapacityDomainScope::Root,
        }
    }

    /// Reconstruct the one child ledger already sealed into `parent_claim`.
    ///
    /// The caller supplies no child ID, generation, dimension set, or budget. Consequently one
    /// parent claim snapshot cannot be used to mint independent sibling ledgers with separate
    /// spending envelopes.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn child(
        parent_domain: &CapacityDomain,
        parent_claim: &CapacityClaim,
    ) -> Result<Self, ExecutionCapacityError> {
        if !matches!(parent_domain.scope(), CapacityDomainScope::Root) {
            return Err(ExecutionCapacityError::new(
                "parent_domain.scope",
                "nested_child_domain_unsupported",
                "the first capacity carrier permits child domains only beneath root claims",
            ));
        }
        if parent_claim.domain_id() != parent_domain.id()
            || parent_claim.domain_generation() != parent_domain.generation()
            || parent_claim.resources().tracked_dimensions()
                != parent_domain.budget().tracked_dimensions()
        {
            return Err(ExecutionCapacityError::new(
                "parent_claim",
                "parent_claim_domain_mismatch",
                "child capacity domain requires an exact claim of the supplied root domain",
            ));
        }
        if !parent_claim.resources().fits_within(parent_domain.budget()) {
            return Err(ExecutionCapacityError::new(
                "parent_claim.resources",
                "parent_claim_exceeds_domain",
                "child capacity domain requires a parent claim within its root-domain budget",
            ));
        }

        let binding = parent_claim.child_domain_binding().ok_or_else(|| {
            ExecutionCapacityError::new(
                "parent_claim.child_domain_binding",
                "parent_claim_missing_child_domain_binding",
                "child capacity domain requires one sealed child-domain binding on its parent claim",
            )
        })?;
        validate_child_domain_binding(parent_domain, parent_claim.resources(), binding)?;

        Ok(Self {
            id: binding.domain_id.clone(),
            generation: binding.domain_generation,
            budget: binding.budget.clone(),
            scope: CapacityDomainScope::Child {
                parent_claim: parent_claim.clone(),
            },
        })
    }

    #[must_use]
    pub fn id(&self) -> &CapacityDomainId {
        &self.id
    }

    #[must_use]
    pub fn generation(&self) -> CapacityDomainGeneration {
        self.generation
    }

    #[must_use]
    pub fn budget(&self) -> &CapacityAmounts {
        &self.budget
    }

    #[must_use]
    pub fn scope(&self) -> &CapacityDomainScope {
        &self.scope
    }

    #[must_use]
    pub fn parent_claim(&self) -> Option<&CapacityClaim> {
        match &self.scope {
            CapacityDomainScope::Root => None,
            CapacityDomainScope::Child { parent_claim } => Some(parent_claim),
        }
    }
}

fn validate_child_domain_binding(
    parent_domain: &CapacityDomain,
    parent_resources: &CapacityAmounts,
    binding: &CapacityChildDomainBinding,
) -> Result<(), ExecutionCapacityError> {
    if !matches!(parent_domain.scope(), CapacityDomainScope::Root) {
        return Err(ExecutionCapacityError::new(
            "parent_domain.scope",
            "nested_child_domain_unsupported",
            "the first capacity carrier permits child domains only beneath root claims",
        ));
    }
    if !parent_resources.fits_within(parent_domain.budget()) {
        return Err(ExecutionCapacityError::new(
            "parent_claim.resources",
            "parent_claim_exceeds_domain",
            "child capacity domain requires a parent claim within its root-domain budget",
        ));
    }
    if binding.domain_id() == parent_domain.id() {
        return Err(ExecutionCapacityError::new(
            "child_domain_binding.domain_id",
            "child_domain_aliases_parent",
            "child capacity domain must have an identity distinct from its parent domain",
        ));
    }
    if !binding
        .budget()
        .tracked_dimensions()
        .is_subset_of(parent_resources.tracked_dimensions())
    {
        return Err(ExecutionCapacityError::new(
            "child_domain_binding.budget.tracked_dimensions",
            "child_dimension_set_not_subset",
            "child capacity dimensions must be a subset of the parent claim dimensions",
        ));
    }
    if !binding.budget().fits_within_parent(parent_resources) {
        return Err(ExecutionCapacityError::new(
            "child_domain_binding.budget",
            "child_budget_exceeds_parent",
            "child capacity budget cannot exceed its parent claim",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CapacityAdmissionRefusalReason {
    ForeignDomain,
    StaleGeneration,
    DimensionSetMismatch,
    DuplicateClaimIdentity,
    ArithmeticOverflow,
    InsufficientResources,
}

impl CapacityAdmissionRefusalReason {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::ForeignDomain => "foreign_domain",
            Self::StaleGeneration => "stale_generation",
            Self::DimensionSetMismatch => "dimension_set_mismatch",
            Self::DuplicateClaimIdentity => "duplicate_claim_identity",
            Self::ArithmeticOverflow => "arithmetic_overflow",
            Self::InsufficientResources => "insufficient_resources",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum CapacityAdmissionDecision {
    Accepted {
        schema_version: u8,
        total_claimed: CapacityAmounts,
        remaining: CapacityAmounts,
    },
    Refused {
        schema_version: u8,
        reason: CapacityAdmissionRefusalReason,
    },
}

impl CapacityAdmissionDecision {
    #[must_use]
    pub fn refusal_reason(&self) -> Option<CapacityAdmissionRefusalReason> {
        match self {
            Self::Accepted { .. } => None,
            Self::Refused { reason, .. } => Some(*reason),
        }
    }
}

/// Admit one candidate into exactly one domain generation.
///
/// Refusal precedence is global and independent of existing-claim order:
/// foreign domain -> stale generation -> dimension-set mismatch -> duplicate identity -> checked
/// arithmetic overflow -> insufficient resources.
#[must_use]
pub fn admit_capacity_claim(
    domain: &CapacityDomain,
    existing: &[CapacityClaim],
    candidate: &CapacityClaim,
) -> CapacityAdmissionDecision {
    if claims_with_candidate(existing, candidate).any(|claim| claim.domain_id() != domain.id()) {
        return refused(CapacityAdmissionRefusalReason::ForeignDomain);
    }

    if claims_with_candidate(existing, candidate)
        .any(|claim| claim.domain_generation() != domain.generation())
    {
        return refused(CapacityAdmissionRefusalReason::StaleGeneration);
    }

    if claims_with_candidate(existing, candidate)
        .any(|claim| claim.resources().tracked_dimensions() != domain.budget().tracked_dimensions())
    {
        return refused(CapacityAdmissionRefusalReason::DimensionSetMismatch);
    }

    let mut claim_ids = BTreeSet::new();
    for claim in claims_with_candidate(existing, candidate) {
        if !claim_ids.insert(claim.id().clone()) {
            return refused(CapacityAdmissionRefusalReason::DuplicateClaimIdentity);
        }
    }

    let mut total = CapacityAmounts::zeroed(domain.budget().tracked_dimensions());
    for claim in claims_with_candidate(existing, candidate) {
        let Some(next) = total.checked_add(claim.resources()) else {
            return refused(CapacityAdmissionRefusalReason::ArithmeticOverflow);
        };
        total = next;
    }

    if !total.fits_within(domain.budget()) {
        return refused(CapacityAdmissionRefusalReason::InsufficientResources);
    }

    let remaining = total.remaining_within(domain.budget());
    CapacityAdmissionDecision::Accepted {
        schema_version: EXECUTION_CAPACITY_SCHEMA_VERSION,
        total_claimed: total,
        remaining,
    }
}

fn claims_with_candidate<'a>(
    existing: &'a [CapacityClaim],
    candidate: &'a CapacityClaim,
) -> impl Iterator<Item = &'a CapacityClaim> {
    existing.iter().chain(std::iter::once(candidate))
}

const fn refused(reason: CapacityAdmissionRefusalReason) -> CapacityAdmissionDecision {
    CapacityAdmissionDecision::Refused {
        schema_version: EXECUTION_CAPACITY_SCHEMA_VERSION,
        reason,
    }
}

fn validate_opaque_identity(
    field: &'static str,
    code: &'static str,
    value: &str,
    prefix: &str,
) -> Result<(), ExecutionCapacityError> {
    let Some(payload) = value.strip_prefix(prefix) else {
        return Err(ExecutionCapacityError::new(
            field,
            code,
            "capacity identity must use its versioned opaque identity form",
        ));
    };

    if value.len() > MAX_CAPACITY_ID_BYTES
        || payload.len() != OPAQUE_ID_HEX_BYTES
        || !payload
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(ExecutionCapacityError::new(
            field,
            code,
            "capacity identity must use its versioned opaque identity form",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ExecutionCapacityError {
    field: &'static str,
    code: &'static str,
    message: &'static str,
}

impl ExecutionCapacityError {
    const fn new(field: &'static str, code: &'static str, message: &'static str) -> Self {
        Self {
            field,
            code,
            message,
        }
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Display for ExecutionCapacityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl std::error::Error for ExecutionCapacityError {}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1024 * 1024 * 1024;

    fn domain_id(hex: char) -> CapacityDomainId {
        CapacityDomainId::parse(&format!(
            "capacity-domain-v1-{}",
            hex.to_string().repeat(64)
        ))
        .unwrap()
    }

    fn claim_id(hex: char) -> CapacityClaimId {
        CapacityClaimId::parse(&format!("capacity-claim-v1-{}", hex.to_string().repeat(64)))
            .unwrap()
    }

    fn generation(value: u64) -> CapacityDomainGeneration {
        CapacityDomainGeneration::new(value).unwrap()
    }

    fn amounts(entries: &[(CapacityDimension, u64)]) -> CapacityAmounts {
        CapacityAmounts::new(entries).unwrap()
    }

    fn root(identity_hex: char, generation_value: u64, budget: CapacityAmounts) -> CapacityDomain {
        CapacityDomain::root(
            domain_id(identity_hex),
            generation(generation_value),
            budget,
        )
    }

    fn claim(
        identity_hex: char,
        domain: &CapacityDomain,
        resources: CapacityAmounts,
    ) -> CapacityClaim {
        CapacityClaim::for_domain(claim_id(identity_hex), domain, resources).unwrap()
    }

    fn claim_with_child(
        identity_hex: char,
        domain: &CapacityDomain,
        resources: CapacityAmounts,
        child_identity_hex: char,
        child_generation: u64,
        child_budget: CapacityAmounts,
    ) -> CapacityClaim {
        CapacityClaim::for_domain_with_child(
            claim_id(identity_hex),
            domain,
            resources,
            domain_id(child_identity_hex),
            generation(child_generation),
            child_budget,
        )
        .unwrap()
    }

    fn reason(decision: &CapacityAdmissionDecision) -> CapacityAdmissionRefusalReason {
        decision.refusal_reason().expect("fixture must refuse")
    }

    #[test]
    fn tracked_zero_is_distinct_from_untracked() {
        let cpu_memory_disk = amounts(&[
            (CapacityDimension::CpuMillis, 4_000),
            (CapacityDimension::MemoryBytes, 8 * GIB),
            (CapacityDimension::DiskBytes, 0),
        ]);
        assert_eq!(
            cpu_memory_disk.amount(CapacityDimension::DiskBytes),
            Some(0)
        );
        assert_eq!(cpu_memory_disk.amount(CapacityDimension::Pids), None);

        let cpu_memory = amounts(&[
            (CapacityDimension::CpuMillis, 4_000),
            (CapacityDimension::MemoryBytes, 8 * GIB),
        ]);
        assert_ne!(
            cpu_memory.tracked_dimensions(),
            cpu_memory_disk.tracked_dimensions()
        );

        let json = serde_json::to_string(&cpu_memory_disk).unwrap();
        assert!(json.contains("\"disk_bytes\":0"));
        assert!(!json.contains("pids"));
    }

    #[test]
    fn dimension_sets_are_closed_bounded_and_canonical() {
        let set = CapacityDimensionSet::new(&[
            CapacityDimension::Pids,
            CapacityDimension::CpuMillis,
            CapacityDimension::MemoryBytes,
        ])
        .unwrap();
        assert_eq!(
            set.as_slice(),
            &[
                CapacityDimension::CpuMillis,
                CapacityDimension::MemoryBytes,
                CapacityDimension::Pids,
            ]
        );
        assert_eq!(
            serde_json::to_string(&set).unwrap(),
            "[\"cpu_millis\",\"memory_bytes\",\"pids\"]"
        );
        assert_eq!(
            CapacityDimensionSet::new(&[]).unwrap_err().code(),
            "invalid_capacity_dimension_set"
        );
        assert_eq!(
            CapacityDimensionSet::new(&[
                CapacityDimension::CpuMillis,
                CapacityDimension::CpuMillis,
            ])
            .unwrap_err()
            .code(),
            "duplicate_capacity_dimension"
        );
    }

    #[test]
    fn mismatched_dimension_sets_refuse_even_when_absent_dimension_would_be_zero() {
        let domain_with_disk = root(
            'a',
            1,
            amounts(&[
                (CapacityDimension::CpuMillis, 4_000),
                (CapacityDimension::MemoryBytes, 8 * GIB),
                (CapacityDimension::DiskBytes, 0),
            ]),
        );
        let same_identity_without_disk = root(
            'a',
            1,
            amounts(&[
                (CapacityDimension::CpuMillis, 4_000),
                (CapacityDimension::MemoryBytes, 8 * GIB),
            ]),
        );
        let incomplete = claim(
            '1',
            &same_identity_without_disk,
            amounts(&[
                (CapacityDimension::CpuMillis, 1_000),
                (CapacityDimension::MemoryBytes, GIB),
            ]),
        );
        assert_eq!(
            reason(&admit_capacity_claim(&domain_with_disk, &[], &incomplete)),
            CapacityAdmissionRefusalReason::DimensionSetMismatch
        );

        let explicit_zero = claim(
            '2',
            &domain_with_disk,
            amounts(&[
                (CapacityDimension::CpuMillis, 1_000),
                (CapacityDimension::MemoryBytes, GIB),
                (CapacityDimension::DiskBytes, 0),
            ]),
        );
        assert!(matches!(
            admit_capacity_claim(&domain_with_disk, &[], &explicit_zero),
            CapacityAdmissionDecision::Accepted { .. }
        ));
    }

    #[test]
    fn child_binding_rejects_alias_untracked_dimensions_and_excess_budget() {
        let parent_domain = root(
            'b',
            4,
            amounts(&[
                (CapacityDimension::CpuMillis, 6_000),
                (CapacityDimension::MemoryBytes, 12 * GIB),
            ]),
        );
        let parent_resources = amounts(&[
            (CapacityDimension::CpuMillis, 4_000),
            (CapacityDimension::MemoryBytes, 8 * GIB),
        ]);

        assert_eq!(
            CapacityClaim::for_domain_with_child(
                claim_id('1'),
                &parent_domain,
                parent_resources.clone(),
                parent_domain.id().clone(),
                generation(1),
                amounts(&[(CapacityDimension::CpuMillis, 1)]),
            )
            .unwrap_err()
            .code(),
            "child_domain_aliases_parent"
        );

        assert_eq!(
            CapacityClaim::for_domain_with_child(
                claim_id('1'),
                &parent_domain,
                parent_resources.clone(),
                domain_id('c'),
                generation(1),
                amounts(&[
                    (CapacityDimension::CpuMillis, 2_000),
                    (CapacityDimension::DiskBytes, 0),
                ]),
            )
            .unwrap_err()
            .code(),
            "child_dimension_set_not_subset"
        );

        assert_eq!(
            CapacityClaim::for_domain_with_child(
                claim_id('1'),
                &parent_domain,
                parent_resources,
                domain_id('c'),
                generation(1),
                amounts(&[
                    (CapacityDimension::CpuMillis, 4_001),
                    (CapacityDimension::MemoryBytes, 8 * GIB),
                ]),
            )
            .unwrap_err()
            .code(),
            "child_budget_exceeds_parent"
        );
    }

    #[test]
    fn parent_claim_seals_one_child_and_reconstructs_only_that_child() {
        let host = root(
            'd',
            8,
            amounts(&[
                (CapacityDimension::CpuMillis, 10_000),
                (CapacityDimension::MemoryBytes, 20 * GIB),
            ]),
        );
        let parent_resources = amounts(&[
            (CapacityDimension::CpuMillis, 6_000),
            (CapacityDimension::MemoryBytes, 12 * GIB),
        ]);
        let child_budget = amounts(&[
            (CapacityDimension::CpuMillis, 6_000),
            (CapacityDimension::MemoryBytes, 12 * GIB),
        ]);
        let parent = claim_with_child(
            '1',
            &host,
            parent_resources.clone(),
            'e',
            3,
            child_budget.clone(),
        );

        let first = CapacityDomain::child(&host, &parent).unwrap();
        let second = CapacityDomain::child(&host, &parent).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.id(), &domain_id('e'));
        assert_eq!(first.generation(), generation(3));
        assert_eq!(first.budget(), &child_budget);
        assert_eq!(first.parent_claim(), Some(&parent));

        let conflicting_snapshot = claim_with_child(
            '1',
            &host,
            parent_resources,
            'f',
            4,
            amounts(&[
                (CapacityDimension::CpuMillis, 6_000),
                (CapacityDimension::MemoryBytes, 12 * GIB),
            ]),
        );
        assert_eq!(
            reason(&admit_capacity_claim(
                &host,
                std::slice::from_ref(&parent),
                &conflicting_snapshot,
            )),
            CapacityAdmissionRefusalReason::DuplicateClaimIdentity
        );

        let unbound = claim(
            '2',
            &host,
            amounts(&[
                (CapacityDimension::CpuMillis, 1_000),
                (CapacityDimension::MemoryBytes, GIB),
            ]),
        );
        assert_eq!(
            CapacityDomain::child(&host, &unbound).unwrap_err().code(),
            "parent_claim_missing_child_domain_binding"
        );
    }

    #[test]
    fn root_domain_aggregates_independent_claims_with_one_exact_set() {
        let host = root(
            'c',
            6,
            amounts(&[
                (CapacityDimension::CpuMillis, 8_000),
                (CapacityDimension::MemoryBytes, 16 * GIB),
                (CapacityDimension::DiskBytes, 100 * GIB),
            ]),
        );
        let first = claim(
            '1',
            &host,
            amounts(&[
                (CapacityDimension::CpuMillis, 2_000),
                (CapacityDimension::MemoryBytes, 4 * GIB),
                (CapacityDimension::DiskBytes, 20 * GIB),
            ]),
        );
        let second = claim(
            '2',
            &host,
            amounts(&[
                (CapacityDimension::CpuMillis, 4_000),
                (CapacityDimension::MemoryBytes, 8 * GIB),
                (CapacityDimension::DiskBytes, 60 * GIB),
            ]),
        );
        assert_eq!(
            admit_capacity_claim(&host, &[first], &second),
            CapacityAdmissionDecision::Accepted {
                schema_version: 1,
                total_claimed: amounts(&[
                    (CapacityDimension::CpuMillis, 6_000),
                    (CapacityDimension::MemoryBytes, 12 * GIB),
                    (CapacityDimension::DiskBytes, 80 * GIB),
                ]),
                remaining: amounts(&[
                    (CapacityDimension::CpuMillis, 2_000),
                    (CapacityDimension::MemoryBytes, 4 * GIB),
                    (CapacityDimension::DiskBytes, 20 * GIB),
                ]),
            }
        );
    }

    #[test]
    fn child_claims_consume_one_child_budget_without_double_charging_parent() {
        let host = root(
            'f',
            9,
            amounts(&[
                (CapacityDimension::CpuMillis, 8_000),
                (CapacityDimension::MemoryBytes, 16 * GIB),
                (CapacityDimension::DiskBytes, 100 * GIB),
            ]),
        );
        let resident_parent = claim_with_child(
            '1',
            &host,
            amounts(&[
                (CapacityDimension::CpuMillis, 6_000),
                (CapacityDimension::MemoryBytes, 12 * GIB),
                (CapacityDimension::DiskBytes, 60 * GIB),
            ]),
            'a',
            3,
            amounts(&[
                (CapacityDimension::CpuMillis, 6_000),
                (CapacityDimension::MemoryBytes, 12 * GIB),
            ]),
        );
        let host_before = admit_capacity_claim(&host, &[], &resident_parent);

        let child = CapacityDomain::child(&host, &resident_parent).unwrap();
        let task_one = claim(
            '2',
            &child,
            amounts(&[
                (CapacityDimension::CpuMillis, 3_000),
                (CapacityDimension::MemoryBytes, 6 * GIB),
            ]),
        );
        let task_two = claim(
            '3',
            &child,
            amounts(&[
                (CapacityDimension::CpuMillis, 3_000),
                (CapacityDimension::MemoryBytes, 6 * GIB),
            ]),
        );
        assert!(matches!(
            admit_capacity_claim(&child, std::slice::from_ref(&task_one), &task_two),
            CapacityAdmissionDecision::Accepted { .. }
        ));
        assert_eq!(
            CapacityDomain::child(&child, &task_two).unwrap_err().code(),
            "nested_child_domain_unsupported"
        );

        let host_after = admit_capacity_claim(&host, &[], &resident_parent);
        assert_eq!(host_before, host_after);
        assert_eq!(
            reason(&admit_capacity_claim(&host, &[], &task_two)),
            CapacityAdmissionRefusalReason::ForeignDomain
        );
    }

    #[test]
    fn refusal_precedence_is_global_and_order_independent() {
        let target = root('1', 2, amounts(&[(CapacityDimension::CpuMillis, 10)]));
        let foreign_domain = root(
            '2',
            1,
            amounts(&[(CapacityDimension::MemoryBytes, u64::MAX)]),
        );
        let foreign_claim = claim(
            '1',
            &foreign_domain,
            amounts(&[(CapacityDimension::MemoryBytes, u64::MAX)]),
        );
        let stale_target = root(
            '1',
            1,
            amounts(&[(CapacityDimension::MemoryBytes, u64::MAX)]),
        );
        let stale_claim = claim(
            '1',
            &stale_target,
            amounts(&[(CapacityDimension::MemoryBytes, u64::MAX)]),
        );
        assert_eq!(
            reason(&admit_capacity_claim(
                &target,
                &[stale_claim],
                &foreign_claim,
            )),
            CapacityAdmissionRefusalReason::ForeignDomain
        );

        let current_wrong_dimensions = root(
            '1',
            2,
            amounts(&[(CapacityDimension::MemoryBytes, u64::MAX)]),
        );
        let stale = claim(
            '2',
            &stale_target,
            amounts(&[(CapacityDimension::MemoryBytes, u64::MAX)]),
        );
        let wrong_dimensions = claim(
            '2',
            &current_wrong_dimensions,
            amounts(&[(CapacityDimension::MemoryBytes, u64::MAX)]),
        );
        assert_eq!(
            reason(&admit_capacity_claim(&target, &[wrong_dimensions], &stale)),
            CapacityAdmissionRefusalReason::StaleGeneration
        );

        let duplicate_wrong_dimensions = claim(
            '3',
            &current_wrong_dimensions,
            amounts(&[(CapacityDimension::MemoryBytes, u64::MAX)]),
        );
        let duplicate_wrong_dimensions_2 = claim(
            '3',
            &current_wrong_dimensions,
            amounts(&[(CapacityDimension::MemoryBytes, u64::MAX)]),
        );
        assert_eq!(
            reason(&admit_capacity_claim(
                &target,
                &[duplicate_wrong_dimensions],
                &duplicate_wrong_dimensions_2,
            )),
            CapacityAdmissionRefusalReason::DimensionSetMismatch
        );

        let overflow_domain = root('3', 1, amounts(&[(CapacityDimension::CpuMillis, u64::MAX)]));
        let full = claim(
            '4',
            &overflow_domain,
            amounts(&[(CapacityDimension::CpuMillis, u64::MAX)]),
        );
        let duplicate = claim(
            '4',
            &overflow_domain,
            amounts(&[(CapacityDimension::CpuMillis, 1)]),
        );
        assert_eq!(
            reason(&admit_capacity_claim(&overflow_domain, &[full], &duplicate)),
            CapacityAdmissionRefusalReason::DuplicateClaimIdentity
        );

        let maxed = claim(
            '5',
            &overflow_domain,
            amounts(&[(CapacityDimension::CpuMillis, u64::MAX)]),
        );
        let one_more = claim(
            '6',
            &overflow_domain,
            amounts(&[(CapacityDimension::CpuMillis, 1)]),
        );
        assert_eq!(
            reason(&admit_capacity_claim(&overflow_domain, &[maxed], &one_more)),
            CapacityAdmissionRefusalReason::ArithmeticOverflow
        );

        let insufficient_domain = root('4', 1, amounts(&[(CapacityDimension::CpuMillis, 10)]));
        let six = claim(
            '7',
            &insufficient_domain,
            amounts(&[(CapacityDimension::CpuMillis, 6)]),
        );
        let five = claim(
            '8',
            &insufficient_domain,
            amounts(&[(CapacityDimension::CpuMillis, 5)]),
        );
        assert_eq!(
            reason(&admit_capacity_claim(&insufficient_domain, &[six], &five)),
            CapacityAdmissionRefusalReason::InsufficientResources
        );

        let a = claim(
            '9',
            &insufficient_domain,
            amounts(&[(CapacityDimension::CpuMillis, 4)]),
        );
        let b = claim(
            'a',
            &insufficient_domain,
            amounts(&[(CapacityDimension::CpuMillis, 4)]),
        );
        let c = claim(
            'b',
            &insufficient_domain,
            amounts(&[(CapacityDimension::CpuMillis, 3)]),
        );
        let forward = admit_capacity_claim(&insufficient_domain, &[a.clone(), b.clone()], &c);
        let reversed = admit_capacity_claim(&insufficient_domain, &[b, a], &c);
        assert_eq!(forward, reversed);
        assert_eq!(
            reason(&forward),
            CapacityAdmissionRefusalReason::InsufficientResources
        );
    }

    #[test]
    fn exact_boundary_and_checked_overflow_are_distinct() {
        let bounded = root(
            '5',
            1,
            amounts(&[
                (CapacityDimension::CpuMillis, 10),
                (CapacityDimension::Pids, 20),
            ]),
        );
        let existing = claim(
            '1',
            &bounded,
            amounts(&[
                (CapacityDimension::CpuMillis, 4),
                (CapacityDimension::Pids, 7),
            ]),
        );
        let exact = claim(
            '2',
            &bounded,
            amounts(&[
                (CapacityDimension::CpuMillis, 6),
                (CapacityDimension::Pids, 13),
            ]),
        );
        assert_eq!(
            admit_capacity_claim(&bounded, std::slice::from_ref(&existing), &exact),
            CapacityAdmissionDecision::Accepted {
                schema_version: 1,
                total_claimed: amounts(&[
                    (CapacityDimension::CpuMillis, 10),
                    (CapacityDimension::Pids, 20),
                ]),
                remaining: amounts(&[
                    (CapacityDimension::CpuMillis, 0),
                    (CapacityDimension::Pids, 0),
                ]),
            }
        );

        let over = claim(
            '3',
            &bounded,
            amounts(&[
                (CapacityDimension::CpuMillis, 6),
                (CapacityDimension::Pids, 14),
            ]),
        );
        assert_eq!(
            reason(&admit_capacity_claim(&bounded, &[existing], &over)),
            CapacityAdmissionRefusalReason::InsufficientResources
        );
    }

    #[test]
    fn json_order_privacy_and_data_only_surface_are_stable() {
        let host = root(
            '6',
            11,
            amounts(&[
                (CapacityDimension::DiskBytes, 30),
                (CapacityDimension::CpuMillis, 10),
                (CapacityDimension::MemoryBytes, 20),
            ]),
        );
        let current = claim(
            '1',
            &host,
            amounts(&[
                (CapacityDimension::MemoryBytes, 2),
                (CapacityDimension::DiskBytes, 3),
                (CapacityDimension::CpuMillis, 1),
            ]),
        );
        let candidate = claim(
            '2',
            &host,
            amounts(&[
                (CapacityDimension::DiskBytes, 6),
                (CapacityDimension::CpuMillis, 4),
                (CapacityDimension::MemoryBytes, 5),
            ]),
        );
        let decision = admit_capacity_claim(&host, &[current], &candidate);
        let json = serde_json::to_string(&decision).unwrap();
        assert_eq!(
            json,
            "{\"decision\":\"accepted\",\"schema_version\":1,\"total_claimed\":{\"tracked_dimensions\":[\"cpu_millis\",\"memory_bytes\",\"disk_bytes\"],\"amounts\":{\"cpu_millis\":5,\"memory_bytes\":7,\"disk_bytes\":9}},\"remaining\":{\"tracked_dimensions\":[\"cpu_millis\",\"memory_bytes\",\"disk_bytes\"],\"amounts\":{\"cpu_millis\":5,\"memory_bytes\":13,\"disk_bytes\":21}}}"
        );

        let claim_debug = format!("{candidate:?}");
        assert!(claim_debug.contains("CapacityClaimId(<opaque>)"));
        assert!(claim_debug.contains("CapacityDomainId(<opaque>)"));
        assert!(!claim_debug.contains(candidate.id().as_str()));
        assert!(!claim_debug.contains(host.id().as_str()));

        let claim_value = serde_json::to_value(&candidate).unwrap();
        let keys = claim_value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            keys,
            BTreeSet::from(["domain_generation", "domain_id", "id", "resources"])
        );

        let parent = claim_with_child(
            '3',
            &host,
            amounts(&[
                (CapacityDimension::DiskBytes, 10),
                (CapacityDimension::CpuMillis, 5),
                (CapacityDimension::MemoryBytes, 10),
            ]),
            '7',
            2,
            amounts(&[
                (CapacityDimension::CpuMillis, 5),
                (CapacityDimension::MemoryBytes, 10),
            ]),
        );
        let parent_json = serde_json::to_string(&parent).unwrap();
        assert!(parent_json.contains("child_domain_binding"));
        assert!(!format!("{parent:?}").contains(parent.id().as_str()));
        assert!(!format!("{parent:?}").contains(domain_id('7').as_str()));

        for forbidden in [
            "/private/",
            "cargo test",
            "limactl",
            "command",
            "process",
            "spawn",
            "kill",
            "cleanup",
            "release",
            "schedule",
            "adopt",
            "persist",
        ] {
            assert!(!json.contains(forbidden));
            assert!(!parent_json.contains(forbidden));
        }

        for value in [
            "/private/operator/state",
            "cargo-test",
            "4242",
            "release-capacity",
            "resident-sandbox",
        ] {
            assert_eq!(
                CapacityClaimId::parse(value).unwrap_err().code(),
                "invalid_capacity_claim_id"
            );
        }
    }
}
