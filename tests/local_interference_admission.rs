#[path = "../src/local_interference_admission.rs"]
mod local_interference_admission;

use local_interference_admission::{
    ActiveInterferenceSummary, LocalInterferenceClass, LocalInterferenceObservation,
    LocalInterferenceRequest, LocalPressureClass, NodeControlState, QuietCompatibility,
    compile_local_interference_admission,
};

#[test]
fn integration_surface_is_content_free_and_non_authorizing() {
    let result = compile_local_interference_admission(
        LocalInterferenceRequest {
            interference_class: LocalInterferenceClass::QuietRequired,
        },
        LocalInterferenceObservation {
            observed_at_unix_millis: 1_000,
            node_control: NodeControlState::Available,
            pressure: LocalPressureClass::Low,
            candidate_quiet_compatibility: QuietCompatibility::Conflicting,
            quiet_lease: None,
            active: ActiveInterferenceSummary::default(),
        },
    );
    let encoded = serde_json::to_string(&result).expect("serializable decision");
    assert!(encoded.contains("\"requires_new_quiet_lease\":true"));
    assert!(encoded.contains("\"grants_authority\":false"));
    assert!(encoded.contains("\"authorizes_execution\":false"));
    assert!(!encoded.contains('/'));
}

#[test]
fn moderate_pressure_remains_eligible_for_the_independent_capacity_gate() {
    let result = compile_local_interference_admission(
        LocalInterferenceRequest {
            interference_class: LocalInterferenceClass::Coexist,
        },
        LocalInterferenceObservation {
            observed_at_unix_millis: 1_000,
            node_control: NodeControlState::Available,
            pressure: LocalPressureClass::Moderate,
            candidate_quiet_compatibility: QuietCompatibility::Conflicting,
            quiet_lease: None,
            active: ActiveInterferenceSummary::default(),
        },
    );
    assert_eq!(
        serde_json::to_value(result).expect("serializable decision")["disposition"],
        "admit_now"
    );
}
