#[path = "../src/disposable_launchd_service_generation.rs"]
mod disposable_launchd_service_generation;

use disposable_launchd_service_generation::{
    DISPOSABLE_LAUNCHD_SERVICE_GENERATION_SCHEMA_VERSION, DisposableLaunchdServiceGeneration,
    disposable_launchd_service_selectors,
};

#[test]
fn current_generation_uses_only_glaeda_service_selectors() {
    let selectors =
        disposable_launchd_service_selectors(DisposableLaunchdServiceGeneration::CURRENT);

    assert_eq!(
        selectors.schema_version(),
        DISPOSABLE_LAUNCHD_SERVICE_GENERATION_SCHEMA_VERSION
    );
    assert_eq!(
        selectors.generation(),
        DisposableLaunchdServiceGeneration::GlaedaCurrentV2
    );
    assert!(!selectors.generation().is_legacy());
    assert_eq!(selectors.label(), "io.glaeda.disposable-worker");
    assert_eq!(
        selectors.plist_file_name(),
        "io.glaeda.disposable-worker.plist"
    );
    assert_eq!(
        selectors.apply_lock_file_name(),
        ".io.glaeda.disposable-worker.apply.lock"
    );
    assert_eq!(
        selectors.staged_plist_prefix(),
        ".io.glaeda.disposable-worker.plist.next."
    );
}

#[test]
fn legacy_generation_reproduces_exact_smolrunner_service_selectors() {
    let selectors = disposable_launchd_service_selectors(
        DisposableLaunchdServiceGeneration::SmolrunnerLegacyV1,
    );

    assert!(selectors.generation().is_legacy());
    assert_eq!(selectors.label(), "io.smolrunner.disposable-worker");
    assert_eq!(
        selectors.plist_file_name(),
        "io.smolrunner.disposable-worker.plist"
    );
    assert_eq!(
        selectors.apply_lock_file_name(),
        ".io.smolrunner.disposable-worker.apply.lock"
    );
    assert_eq!(
        selectors.staged_plist_prefix(),
        ".io.smolrunner.disposable-worker.plist.next."
    );
}

#[test]
fn legacy_and_current_namespaces_are_disjoint() {
    let legacy = disposable_launchd_service_selectors(
        DisposableLaunchdServiceGeneration::SmolrunnerLegacyV1,
    );
    let current =
        disposable_launchd_service_selectors(DisposableLaunchdServiceGeneration::GlaedaCurrentV2);

    assert_ne!(legacy.generation(), current.generation());
    assert_ne!(legacy.label(), current.label());
    assert_ne!(legacy.plist_file_name(), current.plist_file_name());
    assert_ne!(
        legacy.apply_lock_file_name(),
        current.apply_lock_file_name()
    );
    assert_ne!(legacy.staged_plist_prefix(), current.staged_plist_prefix());
}

#[test]
fn public_selector_report_is_fixed_and_path_free() {
    let current = disposable_launchd_service_selectors(DisposableLaunchdServiceGeneration::CURRENT);
    let json = serde_json::to_string(&current).expect("selector report serializes");

    assert!(json.contains("\"generation\":\"glaeda_current_v2\""));
    assert!(json.contains("\"label\":\"io.glaeda.disposable-worker\""));
    assert!(!json.contains("/Users/"));
    assert!(!json.contains("/Library/LaunchAgents/"));
    assert!(!json.contains("gui/"));
}
