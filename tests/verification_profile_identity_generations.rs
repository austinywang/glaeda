use glaeda::artifact::{CommitId, GitTreeId, RepositoryRef, Sha256Digest};
use glaeda::rust_verification_envelope::RustVerificationSourceIdentity;
use glaeda::rust_verification_envelope_digest::digest_rust_verification_envelope;
use glaeda::verification_profile::{
    RepositoryCommandId, RepositoryCommandIdentity, VerificationProfileId,
};
use glaeda::verification_profile_registry::{
    SMOLRUNNER_DOCTOR_PROFILE_ID, SMOLRUNNER_PLAN_PROFILE_ID, SMOLRUNNER_REQUIRED_PROFILE_ID,
    smolrunner_v1_profile_registry,
};
use sha2::{Digest as _, Sha256};

const SMOLRUNNER_V1_REPOSITORY: &str = "teamleaderleo/smolrunner";
const RUST_TOOLCHAIN_CONTRACT_DIGEST: &str =
    "sha256:279d77167cec5426fa80f457cd066dc74a360fbe4e2816f4f3fa01487a918fdc";

fn source() -> RustVerificationSourceIdentity {
    RustVerificationSourceIdentity::new(
        RepositoryRef::parse(SMOLRUNNER_V1_REPOSITORY).expect("repository"),
        CommitId::parse(&"1".repeat(40)).expect("commit"),
        GitTreeId::parse(&"2".repeat(40)).expect("tree"),
    )
}

#[test]
fn smolrunner_v1_profile_command_and_envelope_vectors_remain_exact() {
    let toolchain_document_digest = format!(
        "sha256:{:x}",
        Sha256::digest(include_bytes!("../rust-toolchain.toml"))
    );
    assert_eq!(toolchain_document_digest, RUST_TOOLCHAIN_CONTRACT_DIGEST);

    let registry = smolrunner_v1_profile_registry().expect("historical registry");
    let namespace = Sha256Digest::parse(&format!("sha256:{}", "a".repeat(64))).expect("namespace");
    for (profile_id, command_id, command_digest, envelope_digest) in [
        (
            SMOLRUNNER_REQUIRED_PROFILE_ID,
            "smolrunner.required.v1",
            "sha256:fab0c53ffcb5bf63764155bc1e9dc85371cf2240190ab9cd36ad412cace62dc5",
            "sha256:a8169b6dd94905418011fc04fbe01a1c94bc730a94498eab64805d0cbe8940c7",
        ),
        (
            SMOLRUNNER_DOCTOR_PROFILE_ID,
            "smolrunner.doctor.v1",
            "sha256:46d9f7be1e888b842fe77e81e3826d6338e637901022d7acc9d18fb61b8ffe6e",
            "sha256:06f63fd4887beb67ad749469d0d5cf071604c6aa9112d603b07a84ca605eda9f",
        ),
        (
            SMOLRUNNER_PLAN_PROFILE_ID,
            "smolrunner.plan.v1",
            "sha256:cf9866af6335cd4d3a579dc2f61202cdd3652eb25031330062848251a6e8d0d1",
            "sha256:a4a1e50e5df93cf7d66ecabe24be8587fce4d5752b2f1e825c72d09ff604df2e",
        ),
    ] {
        let profile_id = VerificationProfileId::parse(profile_id).expect("profile ID");
        let expected_command = RepositoryCommandIdentity::new(
            RepositoryRef::parse(SMOLRUNNER_V1_REPOSITORY).expect("repository"),
            RepositoryCommandId::parse(command_id).expect("command ID"),
            Sha256Digest::parse(command_digest).expect("command digest"),
        );
        let profile = registry.lookup(&profile_id).expect("profile");
        assert_eq!(profile.canonical_command().identity(), &expected_command);

        let envelope = registry
            .resolve_rust_envelope(&profile_id, source(), namespace.clone())
            .expect("Rust envelope");
        assert_eq!(envelope.profile_id(), &profile_id);
        assert_eq!(envelope.command(), &expected_command);
        assert_eq!(
            digest_rust_verification_envelope(&envelope)
                .expect("envelope digest")
                .as_str(),
            envelope_digest
        );
    }
}
