#![cfg(unix)]

use std::fs::{self, OpenOptions};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use glaeda::execution_admission::{EpochMillis, ExecutionRequestId};
use glaeda::lima_observation::LimaInstanceName;
use glaeda::mac_availability::AvailabilityRequest;
use glaeda::operator_config::{
    GuestWorkspacePath, OperatorConfig, OperatorIdlePolicy, OperatorOutputPreference,
    OperatorRemediationPreference, PersonalWorkerStateRoot,
};
use glaeda::operator_error::OperatorErrorCode;
use glaeda::personal_worker_operator_read::{
    PERSONAL_WORKER_OPERATOR_READ_SCHEMA_VERSION, PersonalWorkerOperatorReadErrorKind,
    PersonalWorkerOperatorReadService, PersonalWorkerSnapshotExpectation,
};
use glaeda::personal_worker_operator_store::{
    PersonalWorkerInitializationInput, PersonalWorkerOperatorStore,
};
use glaeda::personal_worker_queue::PersonalWorkerQueueGeneration;
use glaeda::personal_worker_read_model::{
    PersonalWorkerJobReadRequest, PersonalWorkerQueuePageRequest,
};
use glaeda::personal_worker_store::PersonalWorkerStoreRevision;
use glaeda::verification_profile::VerificationProfileId;
use rustix::fs::{FlockOperation, flock};

static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

struct TempRoot(PathBuf);

impl TempRoot {
    fn new(label: &str) -> Self {
        let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "smolrunner-operator-read-{label}-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create state root");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o750)).expect("set root mode");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn store_directory(&self) -> PathBuf {
        self.0.join("personal-worker")
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn operator_config(path: &Path) -> OperatorConfig {
    OperatorConfig::new(
        PersonalWorkerStateRoot::parse(path).expect("state root"),
        LimaInstanceName::parse("smolrunner").expect("instance"),
        GuestWorkspacePath::parse("/home/lima/smolrunner-workspace").expect("workspace"),
        VerificationProfileId::parse("smolrunner.required").expect("profile"),
        AvailabilityRequest::Auto,
        OperatorIdlePolicy::new(600_000, 1_800_000).expect("idle policy"),
        OperatorOutputPreference::Json,
        OperatorRemediationPreference::IncludeSuggestions,
    )
    .expect("config")
}

fn initialize(config: &OperatorConfig) {
    PersonalWorkerOperatorStore::initialize(
        config,
        PersonalWorkerInitializationInput::new(EpochMillis::new(1_000_000).expect("time")),
    )
    .expect("initialize");
}

fn revision(value: u64) -> PersonalWorkerStoreRevision {
    PersonalWorkerStoreRevision::new(value).expect("revision")
}

fn generation(value: u64) -> PersonalWorkerQueueGeneration {
    PersonalWorkerQueueGeneration::new(value).expect("generation")
}

fn snapshot(root: &TempRoot) -> Vec<(String, Vec<u8>)> {
    let mut entries = fs::read_dir(root.store_directory())
        .expect("read store")
        .map(|entry| {
            let entry = entry.expect("entry");
            (
                entry.file_name().to_string_lossy().into_owned(),
                fs::read(entry.path()).expect("read entry"),
            )
        })
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    entries
}

#[test]
fn config_bound_status_queue_and_job_reads_never_mutate_the_snapshot() {
    let root = TempRoot::new("reads");
    let config = operator_config(root.path());
    initialize(&config);
    let before = snapshot(&root);

    let status = PersonalWorkerOperatorReadService::read_status(&config, None).expect("status");
    assert_eq!(
        status.schema_version(),
        PERSONAL_WORKER_OPERATOR_READ_SCHEMA_VERSION
    );
    assert_eq!(status.config_identity(), config.identity());
    assert_eq!(status.view().store_revision(), revision(1));
    assert_eq!(status.view().queue_generation(), generation(1));

    let exact = PersonalWorkerOperatorReadService::read_status(
        &config,
        Some(PersonalWorkerSnapshotExpectation::new(
            revision(1),
            generation(1),
        )),
    )
    .expect("exact status");
    assert_eq!(exact.view(), status.view());

    let page = PersonalWorkerOperatorReadService::read_queue_page(
        &config,
        PersonalWorkerQueuePageRequest::new(revision(1), generation(1), 0, 100)
            .expect("page request"),
    )
    .expect("empty page");
    assert_eq!(page.config_identity(), config.identity());
    assert_eq!(page.view().total(), 0);
    assert!(page.view().items().is_empty());
    assert_eq!(page.view().next_offset(), None);

    let missing_job = PersonalWorkerOperatorReadService::read_job(
        &config,
        PersonalWorkerJobReadRequest::new(
            revision(1),
            generation(1),
            ExecutionRequestId::parse("missing-job").expect("request ID"),
        ),
    )
    .expect_err("unprovable job");
    assert_eq!(
        missing_job.kind(),
        PersonalWorkerOperatorReadErrorKind::NotFound
    );
    assert_eq!(missing_job.public_error(), None);
    assert_eq!(snapshot(&root), before);
}

#[test]
fn status_and_page_expectations_fail_stale_revision_before_generation() {
    let root = TempRoot::new("stale");
    let config = operator_config(root.path());
    initialize(&config);

    let stale_revision = PersonalWorkerOperatorReadService::read_status(
        &config,
        Some(PersonalWorkerSnapshotExpectation::new(
            revision(2),
            generation(2),
        )),
    )
    .expect_err("stale revision");
    assert_eq!(
        stale_revision.kind(),
        PersonalWorkerOperatorReadErrorKind::StaleRevision
    );
    assert_eq!(
        stale_revision
            .public_error()
            .expect("public stale revision")
            .code(),
        OperatorErrorCode::DurableStateRevisionStale
    );

    let stale_generation = PersonalWorkerOperatorReadService::read_status(
        &config,
        Some(PersonalWorkerSnapshotExpectation::new(
            revision(1),
            generation(2),
        )),
    )
    .expect_err("stale generation");
    assert_eq!(
        stale_generation.kind(),
        PersonalWorkerOperatorReadErrorKind::StaleQueueGeneration
    );
    assert_eq!(
        stale_generation
            .public_error()
            .expect("public stale generation")
            .code(),
        OperatorErrorCode::DurableStateGenerationStale
    );

    let page_stale = PersonalWorkerOperatorReadService::read_queue_page(
        &config,
        PersonalWorkerQueuePageRequest::new(revision(2), generation(2), 0, 1)
            .expect("page request"),
    )
    .expect_err("page stale revision");
    assert_eq!(
        page_stale.kind(),
        PersonalWorkerOperatorReadErrorKind::StaleRevision
    );

    let page_generation = PersonalWorkerOperatorReadService::read_queue_page(
        &config,
        PersonalWorkerQueuePageRequest::new(revision(1), generation(2), 0, 1)
            .expect("page request"),
    )
    .expect_err("page stale generation");
    assert_eq!(
        page_generation.kind(),
        PersonalWorkerOperatorReadErrorKind::StaleQueueGeneration
    );
    assert_eq!(
        page_generation
            .public_error()
            .expect("public page generation")
            .code(),
        OperatorErrorCode::DurableStateGenerationStale
    );
}

#[test]
fn out_of_bounds_pages_remain_distinct_from_operator_failures() {
    let root = TempRoot::new("page");
    let config = operator_config(root.path());
    initialize(&config);
    let before = snapshot(&root);

    let error = PersonalWorkerOperatorReadService::read_queue_page(
        &config,
        PersonalWorkerQueuePageRequest::new(revision(1), generation(1), 1, 1)
            .expect("page request"),
    )
    .expect_err("offset outside empty snapshot");
    assert_eq!(
        error.kind(),
        PersonalWorkerOperatorReadErrorKind::OffsetOutOfBounds
    );
    assert_eq!(error.public_error(), None);
    assert_eq!(snapshot(&root), before);
}

#[test]
fn recovery_version_corruption_and_busy_fail_closed_without_cleanup() {
    let recovery_root = TempRoot::new("recovery");
    let recovery_config = operator_config(recovery_root.path());
    initialize(&recovery_config);
    let current = recovery_root.store_directory().join("current.json");
    let staged = recovery_root.store_directory().join(".next.json");
    fs::rename(&current, &staged).expect("create recovery debt");
    let recovery_before = snapshot(&recovery_root);
    let recovery = PersonalWorkerOperatorReadService::read_status(&recovery_config, None)
        .expect_err("recovery debt");
    assert_eq!(
        recovery.kind(),
        PersonalWorkerOperatorReadErrorKind::RecoveryRequired
    );
    assert_eq!(snapshot(&recovery_root), recovery_before);

    let version_root = TempRoot::new("version");
    let version_config = operator_config(version_root.path());
    initialize(&version_config);
    let version_current = version_root.store_directory().join("current.json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&fs::read(&version_current).expect("read current")).expect("JSON");
    value["schema_version"] = serde_json::json!(3);
    fs::write(
        &version_current,
        serde_json::to_vec(&value).expect("encode version"),
    )
    .expect("write version");
    let version_before = snapshot(&version_root);
    let version = PersonalWorkerOperatorReadService::read_status(&version_config, None)
        .expect_err("version incompatible");
    assert_eq!(
        version.kind(),
        PersonalWorkerOperatorReadErrorKind::VersionIncompatible
    );
    assert_eq!(snapshot(&version_root), version_before);

    fs::write(&version_current, b"not-json").expect("write corrupt state");
    let corrupt_before = snapshot(&version_root);
    let corrupt = PersonalWorkerOperatorReadService::read_status(&version_config, None)
        .expect_err("corrupt state");
    assert_eq!(
        corrupt.kind(),
        PersonalWorkerOperatorReadErrorKind::CorruptState
    );
    assert_eq!(snapshot(&version_root), corrupt_before);

    fs::set_permissions(&version_current, fs::Permissions::from_mode(0o644))
        .expect("set unsafe current mode");
    let unsafe_before = snapshot(&version_root);
    let unsafe_state = PersonalWorkerOperatorReadService::read_status(&version_config, None)
        .expect_err("unsafe state");
    assert_eq!(
        unsafe_state.kind(),
        PersonalWorkerOperatorReadErrorKind::UnsafeFilesystem
    );
    assert_eq!(snapshot(&version_root), unsafe_before);

    let busy_root = TempRoot::new("busy");
    let busy_config = operator_config(busy_root.path());
    initialize(&busy_config);
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(busy_root.store_directory().join("store.lock"))
        .expect("open lock");
    flock(&lock, FlockOperation::NonBlockingLockExclusive).expect("hold writer lock");
    let busy_before = snapshot(&busy_root);
    let busy =
        PersonalWorkerOperatorReadService::read_status(&busy_config, None).expect_err("busy state");
    assert_eq!(busy.kind(), PersonalWorkerOperatorReadErrorKind::Busy);
    assert_eq!(snapshot(&busy_root), busy_before);
}

#[test]
fn response_debug_and_public_errors_do_not_expose_private_paths_or_documents() {
    let root = TempRoot::new("privacy-sentinel");
    let config = operator_config(root.path());
    initialize(&config);
    let response =
        PersonalWorkerOperatorReadService::read_status(&config, None).expect("status response");

    let missing_root = root.path().join("missing-private-sentinel");
    let missing_config = operator_config(&missing_root);
    let missing = PersonalWorkerOperatorReadService::read_status(&missing_config, None)
        .expect_err("missing state");
    assert_eq!(missing.kind(), PersonalWorkerOperatorReadErrorKind::Missing);
    assert_eq!(
        missing.public_error().expect("public missing state").code(),
        OperatorErrorCode::DurableStateMissing
    );

    let sentinel = root.path().to_string_lossy();
    let response_debug = format!("{response:?}");
    assert!(response_debug.contains("view: \"redacted\""));
    for public in [
        response_debug,
        serde_json::to_string(&response).expect("response JSON"),
        format!("{missing:?}"),
        serde_json::to_string(&missing).expect("error JSON"),
    ] {
        assert!(!public.contains(sentinel.as_ref()));
        assert!(!public.contains("current.json"));
        assert!(!public.contains("store.lock"));
        assert!(!public.contains(".next.json"));
    }
    assert!(!missing_root.exists());
}
