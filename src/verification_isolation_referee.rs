//! Pure sealed cross-attempt verification-isolation compatibility.
//!
//! This module compares already-proven isolation claims. It performs no observation, allocation,
//! admission, scheduling, cache management, filesystem or network access, process management,
//! runtime enforcement, cleanup, diagnostics, persistence, or measurement.

use std::fmt;

use serde::Serialize;

use crate::execution_admission::{ExecutionRequestId, ReservationGeneration, ReservationId};

pub const VERIFICATION_ISOLATION_SCHEMA_VERSION: u8 = 1;

const REDACTED_PRIVATE_EVIDENCE: &str = "<private-verification-isolation-evidence>";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationIsolationDomain {
    WorkspaceState,
    BuildState,
    TemporaryState,
    ServiceEndpoint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationIsolationMode {
    Unused,
    SharedReadOnly,
    AttemptPrivate,
    Serialized,
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct AttemptOwner {
    request_id: ExecutionRequestId,
    reservation_id: ReservationId,
    reservation_generation: ReservationGeneration,
}

/// Equality-only collision identity. The owning evidence producer must eventually mint this from
/// its already-authoritative resource proof; #762 itself grants no authority from this value.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, PartialEq, Eq)]
struct CollisionScope(String);

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, PartialEq, Eq)]
enum IsolationClaim {
    Unused,
    SharedReadOnly(CollisionScope),
    AttemptPrivate(CollisionScope),
    Serialized(CollisionScope),
}

impl IsolationClaim {
    const fn mode(&self) -> VerificationIsolationMode {
        match self {
            Self::Unused => VerificationIsolationMode::Unused,
            Self::SharedReadOnly(_) => VerificationIsolationMode::SharedReadOnly,
            Self::AttemptPrivate(_) => VerificationIsolationMode::AttemptPrivate,
            Self::Serialized(_) => VerificationIsolationMode::Serialized,
        }
    }

    const fn scope(&self) -> Option<&CollisionScope> {
        match self {
            Self::Unused => None,
            Self::SharedReadOnly(scope) | Self::AttemptPrivate(scope) | Self::Serialized(scope) => {
                Some(scope)
            }
        }
    }
}

/// Sealed comparison input. V1 deliberately exposes no production constructor or serialization
/// surface; later integration must be added by the exact owning evidence producers.
pub struct VerificationIsolationEvidence {
    owner: AttemptOwner,
    workspace_state: IsolationClaim,
    build_state: IsolationClaim,
    temporary_state: IsolationClaim,
    service_endpoint: IsolationClaim,
}

impl VerificationIsolationEvidence {
    #[must_use]
    pub fn summary(&self) -> VerificationIsolationSummary {
        VerificationIsolationSummary {
            schema_version: VERIFICATION_ISOLATION_SCHEMA_VERSION,
            request_id: self.owner.request_id.clone(),
            workspace_state: self.workspace_state.mode(),
            build_state: self.build_state.mode(),
            temporary_state: self.temporary_state.mode(),
            service_endpoint: self.service_endpoint.mode(),
        }
    }
}

impl fmt::Debug for VerificationIsolationEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerificationIsolationEvidence")
            .field("summary", &self.summary())
            .field("private_isolation_evidence", &REDACTED_PRIVATE_EVIDENCE)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VerificationIsolationSummary {
    schema_version: u8,
    request_id: ExecutionRequestId,
    workspace_state: VerificationIsolationMode,
    build_state: VerificationIsolationMode,
    temporary_state: VerificationIsolationMode,
    service_endpoint: VerificationIsolationMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum VerificationIsolationPairRefusal {
    DuplicateAttempt,
    ReservationDrift,
    ReservationAlias,
    PrivateScopeAlias { domain: VerificationIsolationDomain },
    ConflictingScopeMode { domain: VerificationIsolationDomain },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum VerificationIsolationPairVerdict {
    Concurrent,
    SerializationRequired {
        domains: Vec<VerificationIsolationDomain>,
    },
    Refused {
        reason: VerificationIsolationPairRefusal,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VerificationIsolationPairReport {
    schema_version: u8,
    left_request_id: ExecutionRequestId,
    right_request_id: ExecutionRequestId,
    verdict: VerificationIsolationPairVerdict,
}

impl VerificationIsolationPairReport {
    #[must_use]
    pub const fn verdict(&self) -> &VerificationIsolationPairVerdict {
        &self.verdict
    }
}

/// Compare two sealed attempts. Argument reversal produces the same report and serialized bytes.
#[must_use]
pub fn evaluate_verification_isolation_pair(
    left: &VerificationIsolationEvidence,
    right: &VerificationIsolationEvidence,
) -> VerificationIsolationPairReport {
    let (left, right) = if left.owner <= right.owner {
        (left, right)
    } else {
        (right, left)
    };

    if let Some(reason) = identity_refusal(&left.owner, &right.owner) {
        return report(
            left,
            right,
            VerificationIsolationPairVerdict::Refused { reason },
        );
    }

    let mut serialized = Vec::new();
    for (domain, left_claim, right_claim) in [
        (
            VerificationIsolationDomain::WorkspaceState,
            &left.workspace_state,
            &right.workspace_state,
        ),
        (
            VerificationIsolationDomain::BuildState,
            &left.build_state,
            &right.build_state,
        ),
        (
            VerificationIsolationDomain::TemporaryState,
            &left.temporary_state,
            &right.temporary_state,
        ),
        (
            VerificationIsolationDomain::ServiceEndpoint,
            &left.service_endpoint,
            &right.service_endpoint,
        ),
    ] {
        match evaluate_domain(domain, left_claim, right_claim) {
            Ok(true) => serialized.push(domain),
            Ok(false) => {}
            Err(reason) => {
                return report(
                    left,
                    right,
                    VerificationIsolationPairVerdict::Refused { reason },
                );
            }
        }
    }

    let verdict = if serialized.is_empty() {
        VerificationIsolationPairVerdict::Concurrent
    } else {
        VerificationIsolationPairVerdict::SerializationRequired {
            domains: serialized,
        }
    };
    report(left, right, verdict)
}

fn identity_refusal(
    left: &AttemptOwner,
    right: &AttemptOwner,
) -> Option<VerificationIsolationPairRefusal> {
    if left == right {
        return Some(VerificationIsolationPairRefusal::DuplicateAttempt);
    }
    if left.request_id == right.request_id {
        return Some(VerificationIsolationPairRefusal::ReservationDrift);
    }
    if left.reservation_id == right.reservation_id {
        return Some(VerificationIsolationPairRefusal::ReservationAlias);
    }
    None
}

/// `Ok(true)` means this exact domain requires serialization.
fn evaluate_domain(
    domain: VerificationIsolationDomain,
    left: &IsolationClaim,
    right: &IsolationClaim,
) -> Result<bool, VerificationIsolationPairRefusal> {
    use IsolationClaim::{AttemptPrivate, Serialized, SharedReadOnly, Unused};

    if matches!(left, Unused) || matches!(right, Unused) {
        return Ok(false);
    }

    if left.scope() != right.scope() {
        return Ok(false);
    }

    match (left, right) {
        (SharedReadOnly(_), SharedReadOnly(_)) => Ok(false),
        (SharedReadOnly(_), Serialized(_))
        | (Serialized(_), SharedReadOnly(_))
        | (Serialized(_), Serialized(_)) => Ok(true),
        (AttemptPrivate(_), AttemptPrivate(_)) => {
            Err(VerificationIsolationPairRefusal::PrivateScopeAlias { domain })
        }
        (SharedReadOnly(_), AttemptPrivate(_))
        | (AttemptPrivate(_), SharedReadOnly(_))
        | (AttemptPrivate(_), Serialized(_))
        | (Serialized(_), AttemptPrivate(_)) => {
            Err(VerificationIsolationPairRefusal::ConflictingScopeMode { domain })
        }
        (Unused, _) | (_, Unused) => Ok(false),
    }
}

fn report(
    left: &VerificationIsolationEvidence,
    right: &VerificationIsolationEvidence,
    verdict: VerificationIsolationPairVerdict,
) -> VerificationIsolationPairReport {
    VerificationIsolationPairReport {
        schema_version: VERIFICATION_ISOLATION_SCHEMA_VERSION,
        left_request_id: left.owner.request_id.clone(),
        right_request_id: right.owner.request_id.clone(),
        verdict,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRIVATE_SCOPE: &str = "private-scope-sentinel";
    const PRIVATE_RESERVATION: &str = "private-reservation-sentinel";

    fn owner(index: usize) -> AttemptOwner {
        AttemptOwner {
            request_id: ExecutionRequestId::parse(&format!("req-{index:02}")).unwrap(),
            reservation_id: ReservationId::parse(&format!("reservation-{index:02}")).unwrap(),
            reservation_generation: ReservationGeneration::new(1).unwrap(),
        }
    }

    fn scope(value: impl Into<String>) -> CollisionScope {
        CollisionScope(value.into())
    }

    fn private(value: impl Into<String>) -> IsolationClaim {
        IsolationClaim::AttemptPrivate(scope(value))
    }

    fn shared(value: impl Into<String>) -> IsolationClaim {
        IsolationClaim::SharedReadOnly(scope(value))
    }

    fn serialized(value: impl Into<String>) -> IsolationClaim {
        IsolationClaim::Serialized(scope(value))
    }

    fn attempt(index: usize) -> VerificationIsolationEvidence {
        VerificationIsolationEvidence {
            owner: owner(index),
            workspace_state: private(format!("workspace-{index:02}")),
            build_state: private(format!("build-{index:02}")),
            temporary_state: private(format!("temporary-{index:02}")),
            service_endpoint: private(format!("service-{index:02}")),
        }
    }

    fn verdict(
        left: &VerificationIsolationEvidence,
        right: &VerificationIsolationEvidence,
    ) -> VerificationIsolationPairVerdict {
        evaluate_verification_isolation_pair(left, right).verdict
    }

    #[test]
    fn pair_output_is_symmetric_and_byte_stable() {
        let a = attempt(1);
        let b = attempt(2);
        let ab = evaluate_verification_isolation_pair(&a, &b);
        let ba = evaluate_verification_isolation_pair(&b, &a);
        assert_eq!(ab, ba);
        assert_eq!(
            serde_json::to_string(&ab).unwrap(),
            serde_json::to_string(&ba).unwrap()
        );
    }

    #[test]
    fn identity_refusals_precede_domain_decisions() {
        let a = attempt(1);
        assert_eq!(
            verdict(&a, &attempt(1)),
            VerificationIsolationPairVerdict::Refused {
                reason: VerificationIsolationPairRefusal::DuplicateAttempt
            }
        );

        let mut drift = attempt(2);
        drift.owner.request_id = a.owner.request_id.clone();
        assert_eq!(
            verdict(&a, &drift),
            VerificationIsolationPairVerdict::Refused {
                reason: VerificationIsolationPairRefusal::ReservationDrift
            }
        );

        let mut alias = attempt(3);
        alias.owner.reservation_id = a.owner.reservation_id.clone();
        assert_eq!(
            verdict(&a, &alias),
            VerificationIsolationPairVerdict::Refused {
                reason: VerificationIsolationPairRefusal::ReservationAlias
            }
        );
    }

    #[test]
    fn same_scope_matrix_is_fail_closed_and_narrow() {
        let mut a = attempt(1);
        let mut b = attempt(2);

        a.build_state = IsolationClaim::Unused;
        b.build_state = serialized("build-a");
        assert_eq!(
            verdict(&a, &b),
            VerificationIsolationPairVerdict::Concurrent
        );

        a.build_state = shared("build-a");
        b.build_state = shared("build-a");
        assert_eq!(
            verdict(&a, &b),
            VerificationIsolationPairVerdict::Concurrent
        );

        a.build_state = shared("build-a");
        b.build_state = serialized("build-a");
        assert_eq!(
            verdict(&a, &b),
            VerificationIsolationPairVerdict::SerializationRequired {
                domains: vec![VerificationIsolationDomain::BuildState]
            }
        );

        a.build_state = serialized("build-a");
        b.build_state = serialized("build-a");
        assert_eq!(
            verdict(&a, &b),
            VerificationIsolationPairVerdict::SerializationRequired {
                domains: vec![VerificationIsolationDomain::BuildState]
            }
        );

        a.build_state = private("build-a");
        b.build_state = private("build-a");
        assert_eq!(
            verdict(&a, &b),
            VerificationIsolationPairVerdict::Refused {
                reason: VerificationIsolationPairRefusal::PrivateScopeAlias {
                    domain: VerificationIsolationDomain::BuildState
                }
            }
        );

        a.build_state = shared("build-a");
        b.build_state = private("build-a");
        assert_eq!(
            verdict(&a, &b),
            VerificationIsolationPairVerdict::Refused {
                reason: VerificationIsolationPairRefusal::ConflictingScopeMode {
                    domain: VerificationIsolationDomain::BuildState
                }
            }
        );

        a.build_state = private("build-a");
        b.build_state = serialized("build-a");
        assert_eq!(
            verdict(&a, &b),
            VerificationIsolationPairVerdict::Refused {
                reason: VerificationIsolationPairRefusal::ConflictingScopeMode {
                    domain: VerificationIsolationDomain::BuildState
                }
            }
        );
    }

    #[test]
    fn distinct_scopes_remain_concurrent() {
        let mut a = attempt(1);
        let mut b = attempt(2);
        a.build_state = shared("build-a");
        b.build_state = serialized("build-b");
        assert_eq!(
            verdict(&a, &b),
            VerificationIsolationPairVerdict::Concurrent
        );
    }

    #[test]
    fn thirty_two_private_attempts_have_496_concurrent_pairs() {
        let attempts = (0..32).map(attempt).collect::<Vec<_>>();
        let mut pairs = 0;
        for left in 0..attempts.len() {
            for right in (left + 1)..attempts.len() {
                pairs += 1;
                assert_eq!(
                    verdict(&attempts[left], &attempts[right]),
                    VerificationIsolationPairVerdict::Concurrent
                );
            }
        }
        assert_eq!(pairs, 496);
    }

    #[test]
    fn one_bad_private_alias_changes_exactly_one_pair() {
        let mut attempts = (0..32).map(attempt).collect::<Vec<_>>();
        attempts[19].temporary_state = private("temporary-07");

        let mut refused = Vec::new();
        for left in 0..attempts.len() {
            for right in (left + 1)..attempts.len() {
                if matches!(
                    verdict(&attempts[left], &attempts[right]),
                    VerificationIsolationPairVerdict::Refused {
                        reason: VerificationIsolationPairRefusal::PrivateScopeAlias {
                            domain: VerificationIsolationDomain::TemporaryState
                        }
                    }
                ) {
                    refused.push((left, right));
                }
            }
        }
        assert_eq!(refused, vec![(7, 19)]);
    }

    #[test]
    fn public_output_redacts_reservation_and_collision_scope() {
        let mut a = attempt(1);
        let b = attempt(2);
        a.owner.reservation_id = ReservationId::parse(PRIVATE_RESERVATION).unwrap();
        a.temporary_state = private(PRIVATE_SCOPE);

        for public in [
            serde_json::to_string(&a.summary()).unwrap(),
            format!("{a:?}"),
            serde_json::to_string(&evaluate_verification_isolation_pair(&a, &b)).unwrap(),
        ] {
            assert!(!public.contains(PRIVATE_SCOPE));
            assert!(!public.contains(PRIVATE_RESERVATION));
        }
    }
}
