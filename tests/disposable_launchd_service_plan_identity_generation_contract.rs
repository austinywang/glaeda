#[allow(dead_code)]
#[path = "../src/disposable_launchd_service_generation.rs"]
mod disposable_launchd_service_generation;
#[path = "../src/disposable_launchd_service_plan_identity_generation.rs"]
mod disposable_launchd_service_plan_identity_generation;

use disposable_launchd_service_generation::DisposableLaunchdServiceGeneration;
use disposable_launchd_service_plan_identity_generation::{
    DISPOSABLE_LAUNCHD_SERVICE_PLAN_IDENTITY_GENERATION_SCHEMA_VERSION,
    DisposableLaunchdServicePlanIdentityGeneration,
    disposable_launchd_service_plan_identity_policy,
};

#[test]
fn current_service_generation_selects_only_the_glaeda_plan_domain() {
    let policy = disposable_launchd_service_plan_identity_policy(
        DisposableLaunchdServiceGeneration::GlaedaCurrentV2,
    );

    assert_eq!(
        DisposableLaunchdServicePlanIdentityGeneration::CURRENT,
        DisposableLaunchdServicePlanIdentityGeneration::GlaedaV2
    );
    assert_eq!(
        policy.schema_version(),
        DISPOSABLE_LAUNCHD_SERVICE_PLAN_IDENTITY_GENERATION_SCHEMA_VERSION
    );
    assert_eq!(
        policy.service_generation(),
        DisposableLaunchdServiceGeneration::GlaedaCurrentV2
    );
    assert_eq!(
        policy.plan_identity_generation(),
        DisposableLaunchdServicePlanIdentityGeneration::GlaedaV2
    );
    assert!(!policy.plan_identity_generation().is_legacy());
    assert_eq!(
        policy.domain(),
        b"glaeda.disposable-launchd-service-plan.v2\0"
    );
}

#[test]
fn legacy_service_generation_reproduces_the_exact_v1_plan_domain() {
    let policy = disposable_launchd_service_plan_identity_policy(
        DisposableLaunchdServiceGeneration::SmolrunnerLegacyV1,
    );

    assert_eq!(
        policy.service_generation(),
        DisposableLaunchdServiceGeneration::SmolrunnerLegacyV1
    );
    assert_eq!(
        policy.plan_identity_generation(),
        DisposableLaunchdServicePlanIdentityGeneration::SmolrunnerV1
    );
    assert!(policy.plan_identity_generation().is_legacy());
    assert_eq!(
        policy.domain(),
        b"smolrunner.disposable-launchd-service-plan.v1\0"
    );
}

#[test]
fn service_and_plan_generations_are_closed_pairs_with_distinct_domains() {
    let legacy = disposable_launchd_service_plan_identity_policy(
        DisposableLaunchdServiceGeneration::SmolrunnerLegacyV1,
    );
    let current = disposable_launchd_service_plan_identity_policy(
        DisposableLaunchdServiceGeneration::GlaedaCurrentV2,
    );

    assert_ne!(legacy.service_generation(), current.service_generation());
    assert_ne!(
        legacy.plan_identity_generation(),
        current.plan_identity_generation()
    );
    assert_ne!(legacy.domain(), current.domain());
}

#[test]
fn public_policy_report_exposes_the_generation_pair_without_domain_bytes() {
    let policy = disposable_launchd_service_plan_identity_policy(
        DisposableLaunchdServiceGeneration::CURRENT,
    );
    let json = serde_json::to_string(&policy).expect("policy serializes");

    assert!(json.contains("\"service_generation\":\"glaeda_current_v2\""));
    assert!(json.contains("\"plan_identity_generation\":\"glaeda_v2\""));
    assert!(!json.contains("disposable-launchd-service-plan"));
}
