use glaeda::artifact::{CommitId, GitTreeId, RepositoryRef, Sha256Digest};
use glaeda::renderprove_vision_profile::*;
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

fn slots(with_receipt: bool) -> RenderproveVisionInputSlots {
    let screenshot =
        RenderproveVisionInputSlot::screenshot_png("evidence/screen.png", 1_024, digest("11"))
            .expect("screenshot");
    let brief =
        RenderproveVisionInputSlot::operator_brief_utf8("evidence/brief.txt", 120, digest("22"))
            .expect("brief");
    let receipt = with_receipt.then(|| {
        RenderproveVisionInputSlot::receipt_v1("evidence/receipt.json", 2_048, digest("33"))
            .expect("receipt")
    });
    RenderproveVisionInputSlots::new(screenshot, brief, receipt).expect("input slots")
}

fn resources() -> ResourceDefaults {
    ResourceDefaults::new(
        MemoryPolicy::new(512 * 1024 * 1024, 0, 256 * 1024 * 1024).expect("memory"),
        ConcurrencyPolicy::new(1, 1, 1).expect("concurrency"),
    )
}

fn profile(with_receipt: bool) -> RenderproveVisionPacketProfile {
    let repository = RepositoryRef::parse("example/project").expect("project repository");
    let project_commit = commit("12");
    let tested_source = TestedSourceIdentity::Commit {
        commit: project_commit,
        tree: GitTreeId::parse(&"34".repeat(20)).expect("tree"),
    };
    let project_command = RepositoryCommandIdentity::new(
        repository.clone(),
        RepositoryCommandId::parse("renderprove-vision-packet").expect("project command"),
        digest("44"),
    );
    RenderproveVisionPacketProfile::new(RenderproveVisionPacketProfileDefinition {
        profile_id: VerificationProfileId::parse("renderprove-vision-packet").expect("profile ID"),
        project_repository: repository,
        tested_source,
        project_command,
        tool: tool(),
        inputs: slots(with_receipt),
        resources: resources(),
        timeout: TimeoutPolicy::new(300, Vec::new()).expect("timeout"),
    })
    .expect("profile")
}

fn preview_json(contract_digest: &str, request_digest: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "$schema": RENDERPROVE_VISION_REQUEST_SCHEMA_URI,
        "schemaVersion": RENDERPROVE_VISION_REQUEST_SCHEMA,
        "mode": "dry-run",
        "authority": "advisory",
        "commandContractId": RENDERPROVE_VISION_COMMAND_ID,
        "commandContractDigest": contract_digest,
        "promptPolicyVersion": RENDERPROVE_VISION_PROMPT_POLICY,
        "canonicalizationProfile": RENDERPROVE_VISION_CANONICALIZATION_PROFILE,
        "requestDigest": request_digest,
        "inputs": {},
        "receiptSummary": null,
        "includedFactNames": [],
        "exclusions": [],
        "promptSafety": {},
        "limits": {}
    }))
    .expect("preview JSON")
}

#[test]
fn separates_project_command_from_external_renderprove_tool() {
    let profile = profile(true);
    assert_eq!(profile.project_repository().as_str(), "example/project");
    assert_eq!(
        profile.project_command().repository().as_str(),
        "example/project"
    );
    assert_eq!(profile.tool().repository().as_str(), RENDERPROVE_REPOSITORY);
    assert_ne!(
        profile.project_command().repository(),
        profile.tool().repository()
    );

    let public = serde_json::to_string(&profile).expect("public profile");
    for private in [
        "evidence/screen.png",
        "evidence/brief.txt",
        "evidence/receipt.json",
    ] {
        assert!(!public.contains(private));
        assert!(!format!("{profile:?}").contains(private));
    }
}

#[test]
fn generates_one_fixed_argument_vector_with_optional_receipt() {
    let expected_with_receipt = [
        "renderprove",
        "vision-check",
        ".",
        "--screenshot",
        "evidence/screen.png",
        "--brief",
        "evidence/brief.txt",
        "--receipt",
        "evidence/receipt.json",
        "--dry-run",
        "--json",
    ]
    .map(str::to_owned);
    assert_eq!(
        profile(true).command_plan().argv(),
        expected_with_receipt.as_slice()
    );

    let expected_without_receipt = [
        "renderprove",
        "vision-check",
        ".",
        "--screenshot",
        "evidence/screen.png",
        "--brief",
        "evidence/brief.txt",
        "--dry-run",
        "--json",
    ]
    .map(str::to_owned);
    assert_eq!(
        profile(false).command_plan().argv(),
        expected_without_receipt.as_slice()
    );
    assert!(format!("{:?}", profile(true).command_plan()).contains("<fixed-argv"));
}

#[test]
fn rejects_tool_repository_command_and_contract_drift() {
    assert!(
        RenderproveVisionToolIdentity::new(
            RepositoryRef::parse("example/renderprove").expect("repository"),
            commit("ab"),
            digest("cd"),
            RepositoryCommandId::parse(RENDERPROVE_VISION_COMMAND_ID).expect("command"),
            Sha256Digest::parse(RENDERPROVE_VISION_COMMAND_CONTRACT_DIGEST).expect("contract"),
        )
        .is_err()
    );
    assert!(
        RenderproveVisionToolIdentity::new(
            RepositoryRef::parse(RENDERPROVE_REPOSITORY).expect("repository"),
            commit("ab"),
            digest("cd"),
            RepositoryCommandId::parse("renderprove.vision-check.v2").expect("command"),
            Sha256Digest::parse(RENDERPROVE_VISION_COMMAND_CONTRACT_DIGEST).expect("contract"),
        )
        .is_err()
    );
    assert!(
        RenderproveVisionToolIdentity::new(
            RepositoryRef::parse(RENDERPROVE_REPOSITORY).expect("repository"),
            commit("ab"),
            digest("cd"),
            RepositoryCommandId::parse(RENDERPROVE_VISION_COMMAND_ID).expect("command"),
            digest("ef"),
        )
        .is_err()
    );
}

#[test]
fn rejects_unsafe_paths_duplicate_paths_and_input_exhaustion() {
    for path in [
        "-",
        "../screen.png",
        "/private/screen.png",
        "https://example.invalid/screen.png",
        "evidence\\screen.png",
        "evidence//screen.png",
        "evidence/screen\n.png",
    ] {
        assert!(
            RenderproveVisionInputSlot::screenshot_png(path, 1, digest("11")).is_err(),
            "{path}"
        );
    }
    assert!(
        RenderproveVisionInputSlot::screenshot_png(
            "screen.png",
            MAX_VISION_SCREENSHOT_BYTES + 1,
            digest("11"),
        )
        .is_err()
    );
    assert!(
        RenderproveVisionInputSlot::operator_brief_utf8(
            "brief.txt",
            MAX_VISION_BRIEF_BYTES + 1,
            digest("22"),
        )
        .is_err()
    );

    let screenshot =
        RenderproveVisionInputSlot::screenshot_png("same", 1, digest("11")).expect("slot");
    let brief =
        RenderproveVisionInputSlot::operator_brief_utf8("same", 1, digest("22")).expect("slot");
    assert!(RenderproveVisionInputSlots::new(screenshot, brief, None).is_err());
}

#[test]
fn rejects_project_command_repository_mismatch() {
    let repository = RepositoryRef::parse("example/project").expect("project repository");
    let foreign_command = RepositoryCommandIdentity::new(
        RepositoryRef::parse("example/other").expect("foreign repository"),
        RepositoryCommandId::parse("renderprove-vision-packet").expect("command"),
        digest("44"),
    );
    let tested_source = TestedSourceIdentity::Commit {
        commit: commit("12"),
        tree: GitTreeId::parse(&"34".repeat(20)).expect("tree"),
    };
    assert!(
        RenderproveVisionPacketProfile::new(RenderproveVisionPacketProfileDefinition {
            profile_id: VerificationProfileId::parse("renderprove-vision-packet").expect("profile"),
            project_repository: repository,
            tested_source,
            project_command: foreign_command,
            tool: tool(),
            inputs: slots(false),
            resources: resources(),
            timeout: TimeoutPolicy::new(300, Vec::new()).expect("timeout"),
        })
        .is_err()
    );
}

#[test]
fn validates_preview_contract_and_request_identity() {
    let tool = tool();
    let contract_hex = RENDERPROVE_VISION_COMMAND_CONTRACT_DIGEST
        .strip_prefix("sha256:")
        .expect("prefix");
    let bytes = preview_json(contract_hex, &"ab".repeat(32));
    let evidence =
        RenderproveVisionPreviewEvidence::from_public_preview(&bytes, digest("55"), &tool)
            .expect("preview evidence");
    assert_eq!(
        evidence.request_digest().as_str(),
        format!("sha256:{}", "ab".repeat(32))
    );
    assert_eq!(evidence.artifact_digest(), &digest("55"));

    let drifted = preview_json(&"cd".repeat(32), &"ab".repeat(32));
    assert!(
        RenderproveVisionPreviewEvidence::from_public_preview(&drifted, digest("55"), &tool,)
            .is_err()
    );
}

#[test]
fn rejects_duplicate_and_unknown_top_level_preview_fields() {
    let tool = tool();
    let contract_hex = RENDERPROVE_VISION_COMMAND_CONTRACT_DIGEST
        .strip_prefix("sha256:")
        .expect("prefix");
    let valid =
        String::from_utf8(preview_json(contract_hex, &"ab".repeat(32))).expect("UTF-8 preview");
    let duplicate = valid.replacen(
        "\"authority\":\"advisory\"",
        "\"authority\":\"advisory\",\"authority\":\"advisory\"",
        1,
    );
    assert!(
        RenderproveVisionPreviewEvidence::from_public_preview(
            duplicate.as_bytes(),
            digest("55"),
            &tool,
        )
        .is_err()
    );

    let mut unknown: serde_json::Value = serde_json::from_str(&valid).expect("JSON");
    unknown
        .as_object_mut()
        .expect("object")
        .insert("unexpected".to_owned(), serde_json::Value::Bool(true));
    let unknown = serde_json::to_vec(&unknown).expect("JSON");
    assert!(
        RenderproveVisionPreviewEvidence::from_public_preview(&unknown, digest("55"), &tool,)
            .is_err()
    );
}

#[test]
fn refuses_invalid_utf8_json_and_preview_overflow() {
    assert!(
        RenderproveVisionPreviewEvidence::from_public_preview(&[0xff], digest("55"), &tool())
            .is_err()
    );
    assert!(
        RenderproveVisionPreviewEvidence::from_public_preview(b"{", digest("55"), &tool()).is_err()
    );
    assert!(
        RenderproveVisionPreviewEvidence::from_public_preview(
            &vec![b' '; MAX_VISION_PREVIEW_BYTES + 1],
            digest("55"),
            &tool(),
        )
        .is_err()
    );
}
