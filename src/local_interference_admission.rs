//! Pure interference admission for owner-local execution intents.
//!
//! This module decides only whether a candidate is compatible with current node hold/drain,
//! pressure, quiet-window, and active-conflict observations. It grants no work authority, creates no
//! queue, acquires no lease, and performs no process/cgroup/filesystem mutation. Existing execution
//! capacity/admission remains independently decisive.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalInterferenceClass {
    Coexist,
    Yieldable,
    QuietRequired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeControlState {
    Available,
    Draining,
    Held,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalPressureClass {
    Low,
    Moderate,
    High,
    Unknown,
}

/// Compatibility is local profile evidence, never a caller-selected promise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuietCompatibility {
    Compatible,
    Conflicting,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuietLeaseObservation {
    pub generation: u64,
    pub expires_at_unix_millis: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ActiveInterferenceSummary {
    /// Active work that conflicts with a quiet window and is not declared yieldable.
    pub conflicting_non_yieldable: u32,
    /// Active work that conflicts with a quiet window but may drain/yield before it begins.
    pub conflicting_yieldable: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalInterferenceRequest {
    pub interference_class: LocalInterferenceClass,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalInterferenceObservation {
    pub observed_at_unix_millis: u64,
    pub node_control: NodeControlState,
    pub pressure: LocalPressureClass,
    /// Locally derived compatibility between this candidate's reviewed resource/profile class and
    /// the currently observed quiet window. The connected caller does not supply this fact.
    pub candidate_quiet_compatibility: QuietCompatibility,
    pub quiet_lease: Option<QuietLeaseObservation>,
    pub active: ActiveInterferenceSummary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalAdmissionDisposition {
    AdmitNow,
    Wait,
    Refuse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalAdmissionReason {
    Compatible,
    NodeDraining,
    NodeHeld,
    PressureHigh,
    PressureUnknownForQuietWindow,
    QuietLeaseAlreadyActive,
    QuietLeaseConflict,
    QuietLeaseCompatibilityUnknown,
    NonYieldableWorkActive,
    YieldableWorkMustDrain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct LocalInterferenceDecision {
    pub disposition: LocalAdmissionDisposition,
    pub reason: LocalAdmissionReason,
    pub active_quiet_lease_generation: Option<u64>,
    pub requires_new_quiet_lease: bool,
    pub requires_yieldable_drain: bool,
    pub grants_authority: bool,
    pub authorizes_preemption: bool,
    pub authorizes_execution: bool,
}

/// Compile one content-free local interference decision.
///
/// `AdmitNow` means only that this interference gate sees no current conflict. Source/work
/// authority, capacity, resource-profile, workspace, and execution admission remain separate gates.
pub fn compile_local_interference_admission(
    request: LocalInterferenceRequest,
    observation: LocalInterferenceObservation,
) -> LocalInterferenceDecision {
    if observation.node_control == NodeControlState::Held {
        return decision(
            LocalAdmissionDisposition::Refuse,
            LocalAdmissionReason::NodeHeld,
            active_quiet_generation(observation),
            false,
            false,
        );
    }
    if observation.node_control == NodeControlState::Draining {
        return decision(
            LocalAdmissionDisposition::Wait,
            LocalAdmissionReason::NodeDraining,
            active_quiet_generation(observation),
            false,
            false,
        );
    }

    let active_quiet = active_quiet_generation(observation);
    if let Some(generation) = active_quiet {
        if request.interference_class == LocalInterferenceClass::QuietRequired {
            return decision(
                LocalAdmissionDisposition::Wait,
                LocalAdmissionReason::QuietLeaseAlreadyActive,
                Some(generation),
                false,
                false,
            );
        }
        match observation.candidate_quiet_compatibility {
            QuietCompatibility::Compatible => {}
            QuietCompatibility::Conflicting => {
                return decision(
                    LocalAdmissionDisposition::Wait,
                    LocalAdmissionReason::QuietLeaseConflict,
                    Some(generation),
                    false,
                    false,
                );
            }
            QuietCompatibility::Unknown => {
                return decision(
                    LocalAdmissionDisposition::Wait,
                    LocalAdmissionReason::QuietLeaseCompatibilityUnknown,
                    Some(generation),
                    false,
                    false,
                );
            }
        }
    }

    if observation.pressure == LocalPressureClass::High {
        return decision(
            LocalAdmissionDisposition::Wait,
            LocalAdmissionReason::PressureHigh,
            active_quiet,
            false,
            false,
        );
    }

    if request.interference_class == LocalInterferenceClass::QuietRequired {
        if observation.pressure == LocalPressureClass::Unknown {
            return decision(
                LocalAdmissionDisposition::Wait,
                LocalAdmissionReason::PressureUnknownForQuietWindow,
                active_quiet,
                false,
                false,
            );
        }
        if observation.active.conflicting_non_yieldable > 0 {
            return decision(
                LocalAdmissionDisposition::Wait,
                LocalAdmissionReason::NonYieldableWorkActive,
                active_quiet,
                false,
                false,
            );
        }
        if observation.active.conflicting_yieldable > 0 {
            return decision(
                LocalAdmissionDisposition::Wait,
                LocalAdmissionReason::YieldableWorkMustDrain,
                active_quiet,
                false,
                true,
            );
        }
        return decision(
            LocalAdmissionDisposition::AdmitNow,
            LocalAdmissionReason::Compatible,
            active_quiet,
            true,
            false,
        );
    }

    decision(
        LocalAdmissionDisposition::AdmitNow,
        LocalAdmissionReason::Compatible,
        active_quiet,
        false,
        false,
    )
}

fn active_quiet_generation(observation: LocalInterferenceObservation) -> Option<u64> {
    observation.quiet_lease.and_then(|lease| {
        (observation.observed_at_unix_millis < lease.expires_at_unix_millis)
            .then_some(lease.generation)
    })
}

const fn decision(
    disposition: LocalAdmissionDisposition,
    reason: LocalAdmissionReason,
    active_quiet_lease_generation: Option<u64>,
    requires_new_quiet_lease: bool,
    requires_yieldable_drain: bool,
) -> LocalInterferenceDecision {
    LocalInterferenceDecision {
        disposition,
        reason,
        active_quiet_lease_generation,
        requires_new_quiet_lease,
        requires_yieldable_drain,
        grants_authority: false,
        authorizes_preemption: false,
        authorizes_execution: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observation() -> LocalInterferenceObservation {
        LocalInterferenceObservation {
            observed_at_unix_millis: 1_000,
            node_control: NodeControlState::Available,
            pressure: LocalPressureClass::Low,
            candidate_quiet_compatibility: QuietCompatibility::Conflicting,
            quiet_lease: None,
            active: ActiveInterferenceSummary::default(),
        }
    }

    const fn request(interference_class: LocalInterferenceClass) -> LocalInterferenceRequest {
        LocalInterferenceRequest { interference_class }
    }

    #[test]
    fn coexist_and_yieldable_work_admit_without_current_interference() {
        for class in [
            LocalInterferenceClass::Coexist,
            LocalInterferenceClass::Yieldable,
        ] {
            assert_eq!(
                compile_local_interference_admission(request(class), observation()),
                decision(
                    LocalAdmissionDisposition::AdmitNow,
                    LocalAdmissionReason::Compatible,
                    None,
                    false,
                    false,
                )
            );
        }
    }

    #[test]
    fn operator_hold_refuses_and_drain_waits() {
        let mut held = observation();
        held.node_control = NodeControlState::Held;
        assert_eq!(
            compile_local_interference_admission(request(LocalInterferenceClass::Coexist), held)
                .disposition,
            LocalAdmissionDisposition::Refuse
        );

        let mut draining = observation();
        draining.node_control = NodeControlState::Draining;
        let result = compile_local_interference_admission(
            request(LocalInterferenceClass::Coexist),
            draining,
        );
        assert_eq!(result.disposition, LocalAdmissionDisposition::Wait);
        assert_eq!(result.reason, LocalAdmissionReason::NodeDraining);
    }

    #[test]
    fn active_quiet_lease_fences_conflicting_or_unknown_locally_observed_candidates() {
        for compatibility in [QuietCompatibility::Conflicting, QuietCompatibility::Unknown] {
            let mut observed = observation();
            observed.candidate_quiet_compatibility = compatibility;
            observed.quiet_lease = Some(QuietLeaseObservation {
                generation: 7,
                expires_at_unix_millis: 2_000,
            });
            let result = compile_local_interference_admission(
                request(LocalInterferenceClass::Coexist),
                observed,
            );
            assert_eq!(result.disposition, LocalAdmissionDisposition::Wait);
            assert_eq!(result.active_quiet_lease_generation, Some(7));
            assert!(!result.authorizes_preemption);
        }
    }

    #[test]
    fn active_quiet_lease_allows_only_locally_proven_compatible_non_measurement_work() {
        let mut observed = observation();
        observed.candidate_quiet_compatibility = QuietCompatibility::Compatible;
        observed.quiet_lease = Some(QuietLeaseObservation {
            generation: 7,
            expires_at_unix_millis: 2_000,
        });
        let result = compile_local_interference_admission(
            request(LocalInterferenceClass::Coexist),
            observed,
        );
        assert_eq!(result.disposition, LocalAdmissionDisposition::AdmitNow);
        assert_eq!(result.active_quiet_lease_generation, Some(7));

        let result = compile_local_interference_admission(
            request(LocalInterferenceClass::QuietRequired),
            observed,
        );
        assert_eq!(result.disposition, LocalAdmissionDisposition::Wait);
        assert_eq!(result.reason, LocalAdmissionReason::QuietLeaseAlreadyActive);
    }

    #[test]
    fn exact_expiry_releases_quiet_fence() {
        let mut observed = observation();
        observed.observed_at_unix_millis = 2_000;
        observed.quiet_lease = Some(QuietLeaseObservation {
            generation: 7,
            expires_at_unix_millis: 2_000,
        });
        let result = compile_local_interference_admission(
            request(LocalInterferenceClass::Coexist),
            observed,
        );
        assert_eq!(result.disposition, LocalAdmissionDisposition::AdmitNow);
        assert_eq!(result.active_quiet_lease_generation, None);
    }

    #[test]
    fn high_pressure_waits_all_work_but_unknown_pressure_only_blocks_new_quiet_window() {
        let mut high = observation();
        high.pressure = LocalPressureClass::High;
        assert_eq!(
            compile_local_interference_admission(request(LocalInterferenceClass::Yieldable), high,)
                .reason,
            LocalAdmissionReason::PressureHigh
        );

        let mut unknown = observation();
        unknown.pressure = LocalPressureClass::Unknown;
        assert_eq!(
            compile_local_interference_admission(
                request(LocalInterferenceClass::Coexist),
                unknown,
            )
            .disposition,
            LocalAdmissionDisposition::AdmitNow
        );
        assert_eq!(
            compile_local_interference_admission(
                request(LocalInterferenceClass::QuietRequired),
                unknown,
            )
            .reason,
            LocalAdmissionReason::PressureUnknownForQuietWindow
        );
    }

    #[test]
    fn quiet_window_waits_for_conflicting_active_work_then_requests_lease_when_clear() {
        let mut non_yieldable = observation();
        non_yieldable.active.conflicting_non_yieldable = 1;
        assert_eq!(
            compile_local_interference_admission(
                request(LocalInterferenceClass::QuietRequired),
                non_yieldable,
            )
            .reason,
            LocalAdmissionReason::NonYieldableWorkActive
        );

        let mut yieldable = observation();
        yieldable.active.conflicting_yieldable = 3;
        let wait = compile_local_interference_admission(
            request(LocalInterferenceClass::QuietRequired),
            yieldable,
        );
        assert_eq!(wait.reason, LocalAdmissionReason::YieldableWorkMustDrain);
        assert!(wait.requires_yieldable_drain);
        assert!(!wait.authorizes_preemption);

        let clear = compile_local_interference_admission(
            request(LocalInterferenceClass::QuietRequired),
            observation(),
        );
        assert_eq!(clear.disposition, LocalAdmissionDisposition::AdmitNow);
        assert!(clear.requires_new_quiet_lease);
        assert!(!clear.grants_authority);
        assert!(!clear.authorizes_execution);
    }

    #[test]
    fn decision_is_deterministic_for_same_observation() {
        let request = request(LocalInterferenceClass::QuietRequired);
        let observed = observation();
        assert_eq!(
            compile_local_interference_admission(request, observed),
            compile_local_interference_admission(request, observed)
        );
    }
}
