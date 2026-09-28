use glaeda::artifact::{CommitId, GitTreeId, RepositoryRef, Sha256Digest};
use glaeda::execution_admission::{
    ExecutionAdmissionIdentity, ExecutionRequestId, RunnerProfileId,
};
use glaeda::renderprove_vision_profile::*;
use glaeda::renderprove_vision_result::*;
use glaeda::verification_profile::{
    ConcurrencyPolicy, MemoryPolicy, RepositoryCommandId, RepositoryCommandIdentity,
    ResourceDefaults, TestedSourceIdentity, TimeoutPolicy, VerificationProfileId,
};

fn digest(pair: &str) -> Sha256Digest {
    Sha256Digest::parse(&format!("sha256:{}", pair.repeat(32))).expect("test digest")
}

fn commit(pair: &str) -> CommitId {
    CommitId::parse(&pair.repeat(20)).expect("test commit")
}

fn tool() -> RenderproveVisionToolIdentity {
    RenderproveVisionToolIdentity::new(
        RepositoryRef::parse(RENDERPROVE_REPOSITORY).expect("repository"),
        commit("ab"),
        digest("cd"),
        RepositoryCommandId::parse(RENDERPROVE_VISION_COMMAND_ID).expect("command ID"),
        Sha256Digest::parse(RENDERPROVE_VISION_COMMAND_CONTRACT_DIGEST).expect("contract digest"),
    )
    .expect("tool identity")
}

fn slots() -> RenderproveVisionInputSlots {
    RenderproveVisionInputSlots::new(
        RenderproveVisionInputSlot::screenshot_png("private/screen.png", 1_024, digest("11"))
            .expect("screenshot"),
        RenderproveVisionInputSlot::operator_brief_utf8("private/brief.txt", 120, digest("22"))
            .expect("brief"),
        None,
    )
    .expect("slots")
}

fn profile() -> RenderproveVisionPacketProfile {
    let repository = RepositoryRef::parse("example/project").expect("project repository");
    let tested_source = TestedSourceIdentity::Commit {
        commit: commit("12"),
        tree: GitTreeId::parse(&"34".repeat(20)).expect("tree"),
    };
    let project_command = RepositoryCommandIdentity::new(
        repository.clone(),
        RepositoryCommandId::parse("renderprove-vision-packet").expect("command"),
        digest("44"),
    );
    RenderproveVisionPacketProfile::new(RenderproveVisionPacketProfileDefinition {
        profile_id: VerificationProfileId::parse("renderprove-vision-packet").expect("profile"),
        project_repository: repository,
        tested_source,
        project_command,
        tool: tool(),
        inputs: slots(),
        resources: ResourceDefaults::new(
            MemoryPolicy::new(512 * 1024 * 1024, 0, 256 * 1024 * 1024).expect("memory"),
            ConcurrencyPolicy::new(1, 1, 1).expect("concurrency"),
        ),
        timeout: TimeoutPolicy::new(300, Vec::new()).expect("timeout"),
    })
    .expect("profile")
}

fn admission(profile: &RenderproveVisionPacketProfile) -> ExecutionAdmissionIdentity {
    ExecutionAdmissionIdentity::new(
        ExecutionRequestId::parse("request-a").expect("request"),
        profile.profile_id().clone(),
        RunnerProfileId::parse("local-linux").expect("runner profile"),
    )
}

fn preview(tool: &RenderproveVisionToolIdentity) -> RenderproveVisionPreviewEvidence {
    let contract_digest = RENDERPROVE_VISION_COMMAND_CONTRACT_DIGEST
        .strip_prefix("sha256:")
        .expect("digest prefix");
    let bytes = serde_json::to_vec(&serde_json::json!({
        "$schema": RENDERPROVE_VISION_REQUEST_SCHEMA_URI,
        "schemaVersion": RENDERPROVE_VISION_REQUEST_SCHEMA,
        "mode": "dry-run",
        "authority": "advisory",
        "commandContractId": RENDERPROVE_VISION_COMMAND_ID,
        "commandContractDigest": contract_digest,
        "promptPolicyVersion": RENDERPROVE_VISION_PROMPT_POLICY,
        "canonicalizationProfile": RENDERPROVE_VISION_CANONICALIZATION_PROFILE,
        "requestDigest": "55".repeat(32),
        "inputs": {},
        "receiptSummary": null,
        "includedFactNames": [],
        "exclusions": [],
        "promptSafety": {},
        "limits": {}
    }))
    .expect("preview JSON");
    RenderproveVisionPreviewEvidence::from_public_preview(&bytes, digest("66"), tool)
        .expect("preview")
}

fn successful_evidence(
    profile: &RenderproveVisionPacketProfile,
) -> RenderproveVisionTerminalEvidence {
    RenderproveVisionTerminalEvidence::new(RenderproveVisionTerminalEvidenceDefinition {
        admission: admission(profile),
        tested_source: profile.tested_source().clone(),
        project_command: profile.project_command().clone(),
        tool: profile.tool().clone(),
        process: RenderproveVisionProcessOutcome::Succeeded,
        cleanup: RenderproveVisionCleanupOutcome::Completed,
        preview: Some(preview(profile.tool())),
    })
    .expect("terminal evidence")
}

#[test]
fn finalizes_success_against_exact_admission_and_profile_identities() {
    let profile = profile();
    let result = finalize_renderprove_vision_result(&profile, successful_evidence(&profile))
        .expect("result");
    assert_eq!(
        result.disposition(),
        RenderproveVisionDisposition::Succeeded
    );
    assert!(result.preview().is_some());
    assert_eq!(
        result.coverage().public_preview_evidence,
        RenderproveVisionEvidenceDisclosure::Included
    );
    assert_eq!(
        result.coverage().screenshot_pixels,
        RenderproveVisionEvidenceDisclosure::Omitted
    );

    let public = serde_json::to_string(&result).expect("public result");
    for private in ["private/screen.png", "private/brief.txt"] {
        assert!(!public.contains(private));
        assert!(!format!("{result:?}").contains(private));
    }
}

#[test]
fn rejects_admission_profile_and_terminal_identity_drift() {
    let profile = profile();
    let wrong_admission = ExecutionAdmissionIdentity::new(
        ExecutionRequestId::parse("request-b").expect("request"),
        VerificationProfileId::parse("other-profile").expect("profile"),
        RunnerProfileId::parse("local-linux").expect("runner profile"),
    );
    let evidence =
        RenderproveVisionTerminalEvidence::new(RenderproveVisionTerminalEvidenceDefinition {
            admission: wrong_admission,
            tested_source: profile.tested_source().clone(),
            project_command: profile.project_command().clone(),
            tool: profile.tool().clone(),
            process: RenderproveVisionProcessOutcome::Succeeded,
            cleanup: RenderproveVisionCleanupOutcome::Completed,
            preview: Some(preview(profile.tool())),
        })
        .expect("evidence");
    assert!(finalize_renderprove_vision_result(&profile, evidence).is_err());

    let drifted_command = RepositoryCommandIdentity::new(
        profile.project_repository().clone(),
        RepositoryCommandId::parse("different-command").expect("command"),
        digest("77"),
    );
    let evidence =
        RenderproveVisionTerminalEvidence::new(RenderproveVisionTerminalEvidenceDefinition {
            admission: admission(&profile),
            tested_source: profile.tested_source().clone(),
            project_command: drifted_command,
            tool: profile.tool().clone(),
            process: RenderproveVisionProcessOutcome::Failed {
                code: RenderproveVisionFailureCode::IdentityDrift,
            },
            cleanup: RenderproveVisionCleanupOutcome::Completed,
            preview: None,
        })
        .expect("evidence");
    assert!(finalize_renderprove_vision_result(&profile, evidence).is_err());
}

#[test]
fn derives_stable_failure_dispositions_without_private_diagnostics() {
    let profile = profile();
    let process_failure =
        RenderproveVisionTerminalEvidence::new(RenderproveVisionTerminalEvidenceDefinition {
            admission: admission(&profile),
            tested_source: profile.tested_source().clone(),
            project_command: profile.project_command().clone(),
            tool: profile.tool().clone(),
            process: RenderproveVisionProcessOutcome::Failed {
                code: RenderproveVisionFailureCode::TimedOut,
            },
            cleanup: RenderproveVisionCleanupOutcome::Completed,
            preview: None,
        })
        .expect("evidence");
    let result = finalize_renderprove_vision_result(&profile, process_failure).expect("result");
    assert_eq!(
        result.disposition(),
        RenderproveVisionDisposition::Failed {
            code: RenderproveVisionFailureCode::TimedOut
        }
    );
    assert_eq!(
        result.coverage().public_preview_evidence,
        RenderproveVisionEvidenceDisclosure::Unavailable
    );

    let cleanup_failure =
        RenderproveVisionTerminalEvidence::new(RenderproveVisionTerminalEvidenceDefinition {
            admission: admission(&profile),
            tested_source: profile.tested_source().clone(),
            project_command: profile.project_command().clone(),
            tool: profile.tool().clone(),
            process: RenderproveVisionProcessOutcome::Succeeded,
            cleanup: RenderproveVisionCleanupOutcome::Failed,
            preview: Some(preview(profile.tool())),
        })
        .expect("evidence");
    let result = finalize_renderprove_vision_result(&profile, cleanup_failure).expect("result");
    assert_eq!(
        result.disposition(),
        RenderproveVisionDisposition::Failed {
            code: RenderproveVisionFailureCode::CleanupFailed
        }
    );
    assert!(result.preview().is_some());
}

#[test]
fn treats_missing_preview_as_malformed_and_refuses_preview_after_process_failure() {
    let profile = profile();
    let missing_preview =
        RenderproveVisionTerminalEvidence::new(RenderproveVisionTerminalEvidenceDefinition {
            admission: admission(&profile),
            tested_source: profile.tested_source().clone(),
            project_command: profile.project_command().clone(),
            tool: profile.tool().clone(),
            process: RenderproveVisionProcessOutcome::Succeeded,
            cleanup: RenderproveVisionCleanupOutcome::Completed,
            preview: None,
        })
        .expect("evidence");
    let result = finalize_renderprove_vision_result(&profile, missing_preview).expect("result");
    assert_eq!(
        result.disposition(),
        RenderproveVisionDisposition::Failed {
            code: RenderproveVisionFailureCode::MalformedPreview
        }
    );

    assert!(
        RenderproveVisionTerminalEvidence::new(RenderproveVisionTerminalEvidenceDefinition {
            admission: admission(&profile),
            tested_source: profile.tested_source().clone(),
            project_command: profile.project_command().clone(),
            tool: profile.tool().clone(),
            process: RenderproveVisionProcessOutcome::Failed {
                code: RenderproveVisionFailureCode::ProcessFailed,
            },
            cleanup: RenderproveVisionCleanupOutcome::Completed,
            preview: Some(preview(profile.tool())),
        })
        .is_err()
    );
}
