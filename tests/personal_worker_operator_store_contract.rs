#![cfg(unix)]

use std::fs::{self, OpenOptions};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;

use glaeda::execution_admission::EpochMillis;
use glaeda::lima_observation::LimaInstanceName;
use glaeda::mac_availability::AvailabilityRequest;
use glaeda::operator_config::{
    GuestWorkspacePath, OperatorConfig, OperatorIdlePolicy, OperatorOutputPreference,
    OperatorRemediationPreference, PersonalWorkerStateRoot,
};
use glaeda::operator_error::OperatorErrorCode;
use glaeda::personal_worker_operator_store::{
    PersonalWorkerInitializationDisposition, PersonalWorkerInitializationInput,
    PersonalWorkerOperatorStore, PersonalWorkerOperatorStoreErrorKind,
};
use glaeda::personal_worker_queue::{
    PersonalWorkerActivityEvidence, PersonalWorkerProfileObservation,
};
use glaeda::verification_profile::VerificationProfileId;
use rustix::fs::{FlockOperation, flock};

static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

struct TempRoot(PathBuf);

impl TempRoot {
    fn new(label: &str) -> Self {
        let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "smolrunner-operator-store-{label}-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create state root");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o750)).expect("set state root mode");
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

fn input(value: u64) -> PersonalWorkerInitializationInput {
    PersonalWorkerInitializationInput::new(EpochMillis::new(value).expect("time"))
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
fn missing_discovery_is_read_only_and_initialization_is_exactly_replayable() {
    let root = TempRoot::new("initial");
    let missing_root = root.path().join("not-created");
    let missing_config = operator_config(&missing_root);
    for error in [
        PersonalWorkerOperatorStore::open_current(&missing_config)
            .expect_err("missing state root read"),
        PersonalWorkerOperatorStore::initialize(&missing_config, input(999_999))
            .expect_err("missing state root init"),
    ] {
        assert_eq!(error.kind(), PersonalWorkerOperatorStoreErrorKind::Missing);
    }
    assert!(!missing_root.exists());

    let config = operator_config(root.path());
    let missing =
        PersonalWorkerOperatorStore::open_current(&config).expect_err("missing managed store");
    assert_eq!(
        missing.kind(),
        PersonalWorkerOperatorStoreErrorKind::Missing
    );
    assert_eq!(
        missing.public_error().code(),
        OperatorErrorCode::DurableStateMissing
    );
    assert!(!root.store_directory().exists());

    let created = PersonalWorkerOperatorStore::initialize(&config, input(1_000_000))
        .expect("initialize worker state");
    assert_eq!(
        created.disposition(),
        PersonalWorkerInitializationDisposition::Initialized
    );
    assert_eq!(created.config_identity(), config.identity());
    assert_eq!(created.durable_schema_version(), 2);
    assert_eq!(created.store_revision().get(), 1);
    assert_eq!(created.queue_generation().get(), 1);
    assert_eq!(
        created.observed_at(),
        EpochMillis::new(1_000_000).expect("time")
    );
    assert_eq!(
        created.profile_observation(),
        PersonalWorkerProfileObservation::Unobserved
    );
    assert_eq!(
        created.activity_evidence(),
        PersonalWorkerActivityEvidence::Never
    );
    assert_eq!(created.queued_count(), 0);
    assert_eq!(created.active_count(), 0);
    assert_eq!(created.cache_lease_count(), 0);
    assert_eq!(created.terminal_tombstone_count(), 0);
    assert!(created.bytes_written() > 0);
    let created_snapshot = snapshot(&root);

    let opened = PersonalWorkerOperatorStore::open_current(&config).expect("open current");
    assert_eq!(opened.config_identity(), config.identity());
    assert_eq!(opened.document().revision().get(), 1);
    assert_eq!(
        opened.document().queue().profile_observation,
        PersonalWorkerProfileObservation::Unobserved
    );
    assert_eq!(
        opened.document().queue().activity_evidence,
        PersonalWorkerActivityEvidence::Never
    );

    let replay = PersonalWorkerOperatorStore::initialize(&config, input(9_999_999))
        .expect("replay initialization");
    assert_eq!(
        replay.disposition(),
        PersonalWorkerInitializationDisposition::AlreadyInitialized
    );
    assert_eq!(replay.store_revision().get(), 1);
    assert_eq!(replay.queue_generation().get(), 1);
    assert_eq!(replay.bytes_written(), 0);
    assert_eq!(snapshot(&root), created_snapshot);
}

#[test]
fn valid_staged_state_is_recovery_required_and_never_published() {
    let root = TempRoot::new("recovery");
    let config = operator_config(root.path());
    PersonalWorkerOperatorStore::initialize(&config, input(2_000_000)).expect("initialize");
    let current = root.store_directory().join("current.json");
    let staged = root.store_directory().join(".next.json");
    fs::rename(&current, &staged).expect("stage initial state");
    let before = snapshot(&root);

    for error in [
        PersonalWorkerOperatorStore::open_current(&config).expect_err("read recovery debt"),
        PersonalWorkerOperatorStore::initialize(&config, input(2_000_001))
            .expect_err("init recovery debt"),
    ] {
        assert_eq!(
            error.kind(),
            PersonalWorkerOperatorStoreErrorKind::RecoveryRequired
        );
        assert_eq!(
            error.public_error().code(),
            OperatorErrorCode::DurableStateRecoveryRequired
        );
    }
    assert_eq!(snapshot(&root), before);
    assert!(!current.exists());
    assert!(staged.exists());

    let stale_root = TempRoot::new("stale-recovery");
    let stale_config = operator_config(stale_root.path());
    PersonalWorkerOperatorStore::initialize(&stale_config, input(2_100_000))
        .expect("initialize stale fixture");
    let stale_current = stale_root.store_directory().join("current.json");
    let stale_stage = stale_root.store_directory().join(".next.json");
    fs::copy(&stale_current, &stale_stage).expect("copy stale stage");
    fs::set_permissions(&stale_stage, fs::Permissions::from_mode(0o600))
        .expect("set stale stage mode");
    let stale_before = snapshot(&stale_root);
    assert_eq!(
        PersonalWorkerOperatorStore::open_current(&stale_config)
            .expect_err("stale staged state")
            .kind(),
        PersonalWorkerOperatorStoreErrorKind::RecoveryRequired
    );
    assert_eq!(snapshot(&stale_root), stale_before);
}

#[test]
fn reader_and_initializer_fail_immediately_while_writer_lock_is_held() {
    let root = TempRoot::new("busy");
    let config = operator_config(root.path());
    PersonalWorkerOperatorStore::initialize(&config, input(3_000_000)).expect("initialize");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.store_directory().join("store.lock"))
        .expect("open lock");
    flock(&lock, FlockOperation::NonBlockingLockExclusive).expect("hold writer lock");
    let before = snapshot(&root);

    assert_eq!(
        PersonalWorkerOperatorStore::open_current(&config)
            .expect_err("reader contention")
            .kind(),
        PersonalWorkerOperatorStoreErrorKind::Busy
    );
    assert_eq!(
        PersonalWorkerOperatorStore::initialize(&config, input(3_000_001))
            .expect_err("initializer contention")
            .kind(),
        PersonalWorkerOperatorStoreErrorKind::Busy
    );
    assert_eq!(snapshot(&root), before);
}

#[test]
fn concurrent_initializers_publish_once_without_generation_advance() {
    let root = TempRoot::new("concurrent");
    let config = Arc::new(operator_config(root.path()));
    let barrier = Arc::new(Barrier::new(2));
    let handles = (0..2)
        .map(|_| {
            let config = Arc::clone(&config);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                PersonalWorkerOperatorStore::initialize(&config, input(4_000_000))
                    .map(|receipt| receipt.disposition())
                    .map_err(|error| error.kind())
            })
        })
        .collect::<Vec<_>>();
    let dispositions = handles
        .into_iter()
        .map(|handle| handle.join().expect("join"))
        .collect::<Vec<_>>();
    assert_eq!(
        dispositions
            .iter()
            .filter(|result| {
                **result == Ok(PersonalWorkerInitializationDisposition::Initialized)
            })
            .count(),
        1
    );
    assert!(dispositions.iter().all(|result| matches!(
        result,
        Ok(PersonalWorkerInitializationDisposition::Initialized)
            | Ok(PersonalWorkerInitializationDisposition::AlreadyInitialized)
            | Err(PersonalWorkerOperatorStoreErrorKind::Busy)
    )));
    let replay = PersonalWorkerOperatorStore::initialize(&config, input(4_000_000))
        .expect("replay after concurrent initialization");
    assert_eq!(
        replay.disposition(),
        PersonalWorkerInitializationDisposition::AlreadyInitialized
    );
    let opened = PersonalWorkerOperatorStore::open_current(&config).expect("open winner");
    assert_eq!(opened.document().revision().get(), 1);
    assert_eq!(opened.document().queue().generation.get(), 1);
}

#[test]
fn version_corruption_and_private_paths_remain_distinct_and_redacted() {
    let root = TempRoot::new("errors-private-sentinel");
    let config = operator_config(root.path());
    let receipt =
        PersonalWorkerOperatorStore::initialize(&config, input(5_000_000)).expect("initialize");
    let current = root.store_directory().join("current.json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&fs::read(&current).expect("read current")).expect("JSON");
    value["schema_version"] = serde_json::json!(3);
    fs::write(&current, serde_json::to_vec(&value).expect("encode")).expect("write version");
    let incompatible =
        PersonalWorkerOperatorStore::open_current(&config).expect_err("incompatible version");
    assert_eq!(
        incompatible.kind(),
        PersonalWorkerOperatorStoreErrorKind::VersionIncompatible
    );
    assert_eq!(
        incompatible.public_error().code(),
        OperatorErrorCode::DurableStateVersionIncompatible
    );

    fs::write(&current, b"not-json").expect("write corrupt bytes");
    let corrupt = PersonalWorkerOperatorStore::open_current(&config).expect_err("corrupt state");
    assert_eq!(
        corrupt.kind(),
        PersonalWorkerOperatorStoreErrorKind::CorruptState
    );
    assert_eq!(
        corrupt.public_error().code(),
        OperatorErrorCode::DurableStateCorrupt
    );

    fs::set_permissions(&current, fs::Permissions::from_mode(0o644))
        .expect("set unsafe current mode");
    let unsafe_state =
        PersonalWorkerOperatorStore::open_current(&config).expect_err("unsafe state");
    assert_eq!(
        unsafe_state.kind(),
        PersonalWorkerOperatorStoreErrorKind::UnsafeFilesystem
    );
    assert_eq!(
        unsafe_state.public_error().code(),
        OperatorErrorCode::DurableStateUnsafe
    );

    fs::set_permissions(&current, fs::Permissions::from_mode(0o600)).expect("restore current mode");
    fs::remove_file(root.store_directory().join("store.lock")).expect("remove lock metadata");
    let missing_lock =
        PersonalWorkerOperatorStore::open_current(&config).expect_err("missing lock metadata");
    assert_eq!(
        missing_lock.kind(),
        PersonalWorkerOperatorStoreErrorKind::UnsafeFilesystem
    );

    let sentinel = root.path().to_string_lossy();
    for public in [
        format!("{receipt:?}"),
        serde_json::to_string(&receipt).expect("receipt JSON"),
        format!("{incompatible:?}"),
        serde_json::to_string(&incompatible).expect("error JSON"),
        format!("{corrupt:?}"),
        format!("{unsafe_state:?}"),
        format!("{missing_lock:?}"),
    ] {
        assert!(!public.contains(sentinel.as_ref()));
        assert!(!public.contains("current.json"));
        assert!(!public.contains("store.lock"));
        assert!(!public.contains(".next.json"));
    }
}
