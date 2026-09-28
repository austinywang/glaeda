#[path = "../src/state_root_generation.rs"]
mod state_root_generation;

use std::path::Path;

use state_root_generation::{
    GLAEDA_CURRENT_STATE_ROOT, SMOLRUNNER_LEGACY_STATE_ROOT, STATE_ROOT_GENERATION_SCHEMA_VERSION,
    StateRootGeneration, StateRootSelection, select_state_root,
};

#[test]
fn current_selection_is_exactly_the_glaeda_root_generation() {
    let selected = select_state_root(StateRootSelection::Current);

    assert_eq!(
        selected.schema_version(),
        STATE_ROOT_GENERATION_SCHEMA_VERSION
    );
    assert_eq!(selected.generation(), StateRootGeneration::GlaedaCurrentV1);
    assert!(selected.generation().is_current());
    assert!(!selected.generation().is_legacy());
    assert_eq!(selected.fixed_path(), Path::new(GLAEDA_CURRENT_STATE_ROOT));
    assert_eq!(selected.fixed_path(), Path::new("/var/lib/glaeda"));
}

#[test]
fn legacy_selection_requires_the_explicit_smolrunner_generation() {
    let selected = select_state_root(StateRootSelection::LegacySmolrunnerV1);

    assert_eq!(
        selected.generation(),
        StateRootGeneration::SmolrunnerLegacyV1
    );
    assert!(selected.generation().is_legacy());
    assert!(!selected.generation().is_current());
    assert_eq!(
        selected.fixed_path(),
        Path::new(SMOLRUNNER_LEGACY_STATE_ROOT)
    );
    assert_eq!(selected.fixed_path(), Path::new("/var/lib/smolrunner"));
}

#[test]
fn current_and_legacy_roots_cannot_compare_as_the_same_generation_or_location() {
    let current = select_state_root(StateRootSelection::Current);
    let legacy = select_state_root(StateRootSelection::LegacySmolrunnerV1);

    assert_ne!(current.generation(), legacy.generation());
    assert_ne!(current.fixed_path(), legacy.fixed_path());
}

#[test]
fn selected_root_report_exposes_generation_without_the_fixed_path() {
    let current = select_state_root(StateRootSelection::Current);
    let legacy = select_state_root(StateRootSelection::LegacySmolrunnerV1);

    let current_json = serde_json::to_string(&current).expect("current selection serializes");
    let legacy_json = serde_json::to_string(&legacy).expect("legacy selection serializes");

    assert!(current_json.contains("\"generation\":\"glaeda_current_v1\""));
    assert!(legacy_json.contains("\"generation\":\"smolrunner_legacy_v1\""));
    assert!(!current_json.contains("/var/lib"));
    assert!(!legacy_json.contains("/var/lib"));
}

#[test]
fn selection_vocabulary_has_no_implicit_adoption_or_fallback_mode() {
    assert_eq!(
        serde_json::to_string(&StateRootSelection::Current).unwrap(),
        "\"current\""
    );
    assert_eq!(
        serde_json::to_string(&StateRootSelection::LegacySmolrunnerV1).unwrap(),
        "\"legacy_smolrunner_v1\""
    );

    assert_eq!(
        select_state_root(StateRootSelection::Current),
        select_state_root(StateRootSelection::Current)
    );
    assert_eq!(
        select_state_root(StateRootSelection::LegacySmolrunnerV1),
        select_state_root(StateRootSelection::LegacySmolrunnerV1)
    );
}
