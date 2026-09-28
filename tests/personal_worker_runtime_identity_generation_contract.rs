#[path = "../src/personal_worker_runtime_identity_generation.rs"]
mod personal_worker_runtime_identity_generation;

use personal_worker_runtime_identity_generation::{
    PERSONAL_WORKER_RUNTIME_IDENTITY_GENERATION_SCHEMA_VERSION,
    PersonalWorkerRuntimeIdentityDomainKind, PersonalWorkerRuntimeIdentityGeneration,
    personal_worker_runtime_identity_domain_policy,
};

#[test]
fn exact_smolrunner_v1_domain_table_remains_reproducible() {
    let expected = [
        (
            PersonalWorkerRuntimeIdentityDomainKind::Readiness,
            b"smolrunner-personal-worker-runtime-readiness-v1".as_slice(),
        ),
        (
            PersonalWorkerRuntimeIdentityDomainKind::RequiredPolicy,
            b"smolrunner-personal-worker-runtime-required-policy-v1".as_slice(),
        ),
        (
            PersonalWorkerRuntimeIdentityDomainKind::AccountEvidence,
            b"smolrunner-personal-worker-runtime-account-evidence-v1".as_slice(),
        ),
        (
            PersonalWorkerRuntimeIdentityDomainKind::KernelPrerequisite,
            b"smolrunner-personal-worker-runtime-kernel-prerequisite-v1".as_slice(),
        ),
        (
            PersonalWorkerRuntimeIdentityDomainKind::ExecutablePrerequisite,
            b"smolrunner-personal-worker-runtime-executable-prerequisite-v1".as_slice(),
        ),
        (
            PersonalWorkerRuntimeIdentityDomainKind::LoaderObjectPrerequisite,
            b"smolrunner-personal-worker-runtime-loader-object-prerequisite-v1".as_slice(),
        ),
        (
            PersonalWorkerRuntimeIdentityDomainKind::LoaderStatePrerequisite,
            b"smolrunner-personal-worker-runtime-loader-state-prerequisite-v1".as_slice(),
        ),
    ];

    for (kind, expected_domain) in expected {
        let policy = personal_worker_runtime_identity_domain_policy(
            PersonalWorkerRuntimeIdentityGeneration::SmolrunnerV1,
            kind,
        );
        assert_eq!(policy.domain(), expected_domain);
        assert!(policy.generation().is_legacy());
    }
}

#[test]
fn current_glaeda_v2_domain_table_is_complete_and_explicit() {
    let expected = [
        (
            PersonalWorkerRuntimeIdentityDomainKind::Readiness,
            b"glaeda-personal-worker-runtime-readiness-v2".as_slice(),
        ),
        (
            PersonalWorkerRuntimeIdentityDomainKind::RequiredPolicy,
            b"glaeda-personal-worker-runtime-required-policy-v2".as_slice(),
        ),
        (
            PersonalWorkerRuntimeIdentityDomainKind::AccountEvidence,
            b"glaeda-personal-worker-runtime-account-evidence-v2".as_slice(),
        ),
        (
            PersonalWorkerRuntimeIdentityDomainKind::KernelPrerequisite,
            b"glaeda-personal-worker-runtime-kernel-prerequisite-v2".as_slice(),
        ),
        (
            PersonalWorkerRuntimeIdentityDomainKind::ExecutablePrerequisite,
            b"glaeda-personal-worker-runtime-executable-prerequisite-v2".as_slice(),
        ),
        (
            PersonalWorkerRuntimeIdentityDomainKind::LoaderObjectPrerequisite,
            b"glaeda-personal-worker-runtime-loader-object-prerequisite-v2".as_slice(),
        ),
        (
            PersonalWorkerRuntimeIdentityDomainKind::LoaderStatePrerequisite,
            b"glaeda-personal-worker-runtime-loader-state-prerequisite-v2".as_slice(),
        ),
    ];

    assert_eq!(
        PersonalWorkerRuntimeIdentityGeneration::CURRENT,
        PersonalWorkerRuntimeIdentityGeneration::GlaedaV2
    );
    assert_eq!(PersonalWorkerRuntimeIdentityDomainKind::ALL.len(), 7);

    for (kind, expected_domain) in expected {
        let policy = personal_worker_runtime_identity_domain_policy(
            PersonalWorkerRuntimeIdentityGeneration::CURRENT,
            kind,
        );
        assert_eq!(policy.domain(), expected_domain);
        assert!(!policy.generation().is_legacy());
    }
}

#[test]
fn same_runtime_class_has_distinct_old_and_current_domains() {
    for kind in PersonalWorkerRuntimeIdentityDomainKind::ALL {
        let legacy = personal_worker_runtime_identity_domain_policy(
            PersonalWorkerRuntimeIdentityGeneration::SmolrunnerV1,
            kind,
        );
        let current = personal_worker_runtime_identity_domain_policy(
            PersonalWorkerRuntimeIdentityGeneration::GlaedaV2,
            kind,
        );

        assert_eq!(legacy.kind(), current.kind());
        assert_ne!(legacy.generation(), current.generation());
        assert_ne!(legacy.domain(), current.domain());
    }
}

#[test]
fn public_policy_report_carries_generation_and_kind_without_raw_domain_bytes() {
    let policy = personal_worker_runtime_identity_domain_policy(
        PersonalWorkerRuntimeIdentityGeneration::CURRENT,
        PersonalWorkerRuntimeIdentityDomainKind::LoaderStatePrerequisite,
    );
    let json = serde_json::to_string(&policy).expect("runtime identity policy serializes");

    assert_eq!(
        policy.schema_version(),
        PERSONAL_WORKER_RUNTIME_IDENTITY_GENERATION_SCHEMA_VERSION
    );
    assert!(json.contains("\"generation\":\"glaeda_v2\""));
    assert!(json.contains("\"kind\":\"loader_state_prerequisite\""));
    assert!(!json.contains("personal-worker-runtime"));
}
