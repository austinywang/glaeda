#![cfg(unix)]

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use glaeda::artifact::{CommitId, GitTreeId, RepositoryRef, Sha256Digest};
use glaeda::execution_admission::{
    EpochMillis, ExecutionAdmissionIdentity, ExecutionRequestId, ExecutionResourceLimits,
    FallbackProfileEligibility, RunnerProfileId,
};
use glaeda::personal_worker_queue::{
    MAX_PERSONAL_WORKER_QUEUE_ENTRIES, PersonalWorkerActivityEvidence,
    PersonalWorkerCacheAccessMode, PersonalWorkerCacheNamespace, PersonalWorkerCancellationState,
    PersonalWorkerJobRequest, PersonalWorkerPriority, PersonalWorkerProfile,
    PersonalWorkerProfileObservation, PersonalWorkerQueueGeneration, PersonalWorkerQueueInput,
    PersonalWorkerSourceIdentity,
};
use glaeda::personal_worker_store::{PersonalWorkerStore, PersonalWorkerStoreDocument};
use glaeda::unix_personal_worker_store::UnixPersonalWorkerStore;
use glaeda::verification_profile::{CacheId, VerificationProfileId};

const GIB: u64 = 1_024 * 1_024 * 1_024;
const BASE: u64 = 20_000_000;
static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

struct TempRoot(PathBuf);

impl TempRoot {
    fn new() -> Self {
        let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "smolrunner-submit-capacity-private-path-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create temporary state root");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o750)).expect("set root mode");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn current_document(&self) -> PathBuf {
        self.0.join("personal-worker/current.json")
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn time(value: u64) -> EpochMillis {
    EpochMillis::new(value).expect("time")
}

fn queued_request(index: usize) -> PersonalWorkerJobRequest {
    let repository = RepositoryRef::parse("example/project").expect("repository");
    PersonalWorkerJobRequest {
        identity: ExecutionAdmissionIdentity::new(
            ExecutionRequestId::parse(&format!("queued-{index:03}")).expect("request ID"),
            VerificationProfileId::parse("smolrunner.required").expect("verification profile"),
            RunnerProfileId::parse("personal-lima-work").expect("runner profile"),
        ),
        source: PersonalWorkerSourceIdentity::new(
            repository.clone(),
            CommitId::parse(&"a".repeat(40)).expect("commit"),
            GitTreeId::parse(&"b".repeat(40)).expect("tree"),
        ),
        priority: PersonalWorkerPriority::Normal,
        requested_limits: ExecutionResourceLimits::new(2_000, 2 * GIB, 2_048)
            .expect("resource limits"),
        cache_namespace: PersonalWorkerCacheNamespace::RepositoryBuild {
            cache_id: CacheId::parse("build-cache").expect("cache ID"),
            repository,
            namespace_digest: Sha256Digest::parse(&format!("sha256:{}", "ab".repeat(32)))
                .expect("namespace digest"),
        },
        cache_access: PersonalWorkerCacheAccessMode::Write,
        submitted_at: time(BASE - 1_000),
        operator_deadline: None,
        cancellation: PersonalWorkerCancellationState::Active,
        fallback_eligibility: FallbackProfileEligibility::ineligible(),
    }
}

fn full_queue_document() -> PersonalWorkerStoreDocument {
    let queued = (0..MAX_PERSONAL_WORKER_QUEUE_ENTRIES)
        .map(queued_request)
        .collect();
    PersonalWorkerStoreDocument::new(
        PersonalWorkerQueueInput {
            generation: PersonalWorkerQueueGeneration::new(1).expect("queue generation"),
            observed_at: time(BASE),
            profile_observation: PersonalWorkerProfileObservation::observed(
                PersonalWorkerProfile::Interactive,
            ),
            activity_evidence: PersonalWorkerActivityEvidence::observed(time(BASE - 1_000)),
            queued,
            active: vec![],
            pending_profile_change: None,
        },
        vec![],
    )
    .expect("full valid queue document")
}

fn create_store(root: &TempRoot) {
    let (mut store, _) = UnixPersonalWorkerStore::open_or_create(root.path()).expect("open store");
    store
        .create(&full_queue_document())
        .expect("create full queue document");
}

fn submit_arguments(root: &TempRoot, output: &str) -> Vec<OsString> {
    vec![
        OsString::from("--output"),
        OsString::from(output),
        OsString::from("queue"),
        OsString::from("submit"),
        OsString::from("--store-root"),
        root.path().as_os_str().to_owned(),
        OsString::from("--revision"),
        OsString::from("1"),
        OsString::from("--generation"),
        OsString::from("1"),
        OsString::from("--observed-at"),
        OsString::from((BASE + 1_000).to_string()),
        OsString::from("--request-id"),
        OsString::from("private-capacity-request-sentinel"),
        OsString::from("--verification-profile"),
        OsString::from("smolrunner.required"),
        OsString::from("--runner-profile"),
        OsString::from("personal-lima-work"),
        OsString::from("--repository"),
        OsString::from("example/project"),
        OsString::from("--commit"),
        OsString::from("c".repeat(40)),
        OsString::from("--tree"),
        OsString::from("d".repeat(40)),
        OsString::from("--priority"),
        OsString::from("normal"),
        OsString::from("--cpu-millis"),
        OsString::from("2000"),
        OsString::from("--memory-bytes"),
        OsString::from((2 * GIB).to_string()),
        OsString::from("--pids"),
        OsString::from("2048"),
        OsString::from("--cache-id"),
        OsString::from("build-cache"),
        OsString::from("--cache-namespace-digest"),
        OsString::from(format!("sha256:{}", "cd".repeat(32))),
        OsString::from("--cache-access"),
        OsString::from("write"),
        OsString::from("--submitted-at"),
        OsString::from((BASE - 500).to_string()),
    ]
}

fn run_glaeda(arguments: &[OsString]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_glaeda"))
        .args(arguments)
        .output()
        .expect("run installed glaeda binary")
}

fn current_bytes(root: &TempRoot) -> Vec<u8> {
    fs::read(root.current_document()).expect("read current document")
}

fn public_output(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn full_queue_rejects_submission_without_publication_or_private_disclosure() {
    let root = TempRoot::new();
    create_store(&root);
    let before = current_bytes(&root);

    let json_output = run_glaeda(&submit_arguments(&root, "json"));
    assert!(!json_output.status.success());
    let json: serde_json::Value =
        serde_json::from_slice(&json_output.stdout).expect("bounded JSON error");
    assert_eq!(json["kind"], "invalid_mutation");
    assert_eq!(current_bytes(&root), before);
    let json_public = public_output(&json_output);
    assert!(!json_public.contains("private-capacity-request-sentinel"));
    assert!(!json_public.contains(root.path().to_string_lossy().as_ref()));

    let human_output = run_glaeda(&submit_arguments(&root, "human"));
    assert!(!human_output.status.success());
    assert_eq!(current_bytes(&root), before);
    let human_public = public_output(&human_output);
    assert!(!human_public.contains("private-capacity-request-sentinel"));
    assert!(!human_public.contains(root.path().to_string_lossy().as_ref()));
}
