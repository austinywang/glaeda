mod verification_profile_registry {
    pub use glaeda::verification_profile_registry::{
        GLAEDA_DOCTOR_COMMAND_ID, GLAEDA_DOCTOR_PROFILE_ID, GLAEDA_PLAN_COMMAND_ID,
        GLAEDA_PLAN_PROFILE_ID, GLAEDA_REQUIRED_COMMAND_ID, GLAEDA_REQUIRED_PROFILE_ID,
        RegisteredVerificationProfile, SMOLRUNNER_DOCTOR_PROFILE_ID, SMOLRUNNER_PLAN_PROFILE_ID,
        SMOLRUNNER_REQUIRED_PROFILE_ID,
    };
}

#[path = "../src/verification_cache_generation.rs"]
mod verification_cache_generation;

use glaeda::verification_profile_registry::{
    glaeda_profile_registry, smolrunner_v1_profile_registry,
};
use verification_cache_generation::{
    VerificationCacheIdentityGeneration, verification_cache_identity_generation,
};

#[test]
fn every_current_glaeda_profile_selects_the_glaeda_cache_generation() {
    let registry = glaeda_profile_registry().expect("current Glaeda registry must validate");

    for profile in registry.profiles() {
        assert_eq!(
            verification_cache_identity_generation(profile).unwrap(),
            VerificationCacheIdentityGeneration::GlaedaV2
        );
    }
}

#[test]
fn every_retained_smolrunner_profile_selects_the_legacy_cache_generation() {
    let registry =
        smolrunner_v1_profile_registry().expect("retained SmolRunner registry must validate");

    for profile in registry.profiles() {
        assert_eq!(
            verification_cache_identity_generation(profile).unwrap(),
            VerificationCacheIdentityGeneration::SmolrunnerV1
        );
    }
}

#[test]
fn current_generation_is_explicit_and_serializable() {
    assert_eq!(
        VerificationCacheIdentityGeneration::CURRENT,
        VerificationCacheIdentityGeneration::GlaedaV2
    );
    assert_eq!(
        serde_json::to_string(&VerificationCacheIdentityGeneration::CURRENT).unwrap(),
        "\"glaeda_v2\""
    );
    assert_eq!(
        serde_json::to_string(&VerificationCacheIdentityGeneration::SmolrunnerV1).unwrap(),
        "\"smolrunner_v1\""
    );
}
