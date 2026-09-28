#![cfg(unix)]
#![allow(dead_code)]

pub use glaeda::{operator_config, operator_error};

#[path = "../src/operator_config_store.rs"]
mod operator_config_store;

use std::cell::Cell;
use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::OsStringExt as _;
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;

use glaeda::lima_observation::LimaInstanceName;
use glaeda::mac_availability::AvailabilityRequest;
use glaeda::operator_config::{
    GuestWorkspacePath, OperatorConfig, OperatorIdlePolicy, OperatorOutputPreference,
    OperatorRemediationPreference, PersonalWorkerStateRoot,
};
use glaeda::operator_error::OperatorErrorCode;
use glaeda::verification_profile::VerificationProfileId;
use operator_config_store::{
    OperatorConfigCreateDisposition, OperatorConfigDiscoveryContext,
    OperatorConfigDiscoveryRequest, OperatorConfigSource, OperatorConfigStore,
    OperatorConfigStoreErrorKind,
};

static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

struct TempRoot(PathBuf);

impl TempRoot {
    fn new(label: &str) -> Self {
        let test_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("operator-config-test-state");
        fs::create_dir_all(&test_root).expect("create private test root");
        fs::set_permissions(&test_root, fs::Permissions::from_mode(0o700))
            .expect("set private test-root mode");
        let path = test_root.join(format!(
            "smolrunner-operator-config-{label}-{}-{}",
            std::process::id(),
            NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).expect("create temporary directory");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).expect("set private mode");
        Self(fs::canonicalize(path).expect("canonical temporary directory"))
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[derive(Default)]
struct Context {
    environment: Option<OsString>,
    home: Option<OsString>,
    supports_default: bool,
    environment_calls: Cell<u8>,
    home_calls: Cell<u8>,
}

impl OperatorConfigDiscoveryContext for Context {
    fn environment_config(&self) -> Option<OsString> {
        self.environment_calls.set(self.environment_calls.get() + 1);
        self.environment.clone()
    }

    fn operator_home(&self) -> Option<OsString> {
        self.home_calls.set(self.home_calls.get() + 1);
        self.home.clone()
    }

    fn supports_macos_default(&self) -> bool {
        self.supports_default
    }
}

fn config(suffix: &str) -> OperatorConfig {
    OperatorConfig::new(
        PersonalWorkerStateRoot::parse(format!("/private/state/{suffix}")).expect("state root"),
        LimaInstanceName::parse("smolrunner").expect("instance"),
        GuestWorkspacePath::parse(format!("/workspace/{suffix}")).expect("workspace"),
        VerificationProfileId::parse("smolrunner.required").expect("profile"),
        AvailabilityRequest::Auto,
        OperatorIdlePolicy::new(600_000, 1_800_000).expect("idle policy"),
        OperatorOutputPreference::Json,
        OperatorRemediationPreference::IncludeSuggestions,
    )
    .expect("config")
}

fn request(path: &Path) -> OperatorConfigDiscoveryRequest {
    OperatorConfigDiscoveryRequest::new(Some(path.as_os_str().to_owned()))
}

#[test]
fn explicit_selection_never_reads_environment_or_home() {
    let root = TempRoot::new("precedence");
    let path = root.0.join("config.json");
    let context = Context {
        environment: Some(OsString::from("/ignored/environment")),
        home: Some(OsString::from("/ignored/home")),
        supports_default: true,
        ..Context::default()
    };
    let receipt = OperatorConfigStore::create(&request(&path), &context, &config("one"))
        .expect("create explicit config");
    assert_eq!(receipt.source(), OperatorConfigSource::Explicit);
    assert_eq!(context.environment_calls.get(), 0);
    assert_eq!(context.home_calls.get(), 0);
}

#[test]
fn environment_selection_prevents_home_fallback() {
    let root = TempRoot::new("environment");
    let path = root.0.join("config.json");
    let context = Context {
        environment: Some(path.as_os_str().to_owned()),
        home: Some(OsString::from("/ignored/home")),
        supports_default: true,
        ..Context::default()
    };
    let receipt = OperatorConfigStore::create(
        &OperatorConfigDiscoveryRequest::default(),
        &context,
        &config("two"),
    )
    .expect("create environment config");
    assert_eq!(receipt.source(), OperatorConfigSource::Environment);
    assert_eq!(context.environment_calls.get(), 1);
    assert_eq!(context.home_calls.get(), 0);
}

#[test]
fn create_load_and_replay_are_private_and_byte_stable() {
    let root = TempRoot::new("round-trip");
    let path = root.0.join("config.json");
    let desired = config("round-trip-private");
    let created = OperatorConfigStore::create(&request(&path), &Context::default(), &desired)
        .expect("create config");
    assert_eq!(
        created.disposition(),
        OperatorConfigCreateDisposition::Created
    );
    assert!(created.bytes_written() > 0);
    assert_eq!(
        fs::metadata(&path).expect("metadata").permissions().mode() & 0o7777,
        0o600
    );
    let before = fs::read(&path).expect("read config");
    let loaded =
        OperatorConfigStore::load(&request(&path), &Context::default()).expect("load config");
    assert_eq!(loaded.config().identity(), desired.identity());
    let replay = OperatorConfigStore::create(&request(&path), &Context::default(), &desired)
        .expect("replay config");
    assert_eq!(
        replay.disposition(),
        OperatorConfigCreateDisposition::AlreadyExists
    );
    assert_eq!(replay.bytes_written(), 0);
    assert_eq!(fs::read(&path).expect("re-read config"), before);
    let public = format!(
        "{loaded:?} {}",
        serde_json::to_string(&loaded).expect("JSON")
    );
    assert!(!public.contains("round-trip-private"));
    assert!(!public.contains(root.0.to_string_lossy().as_ref()));
}

#[test]
fn incompatible_replay_and_unsafe_objects_fail_closed() {
    let root = TempRoot::new("unsafe");
    let path = root.0.join("config.json");
    OperatorConfigStore::create(&request(&path), &Context::default(), &config("original"))
        .expect("create config");
    let before = fs::read(&path).expect("read config");
    let error =
        OperatorConfigStore::create(&request(&path), &Context::default(), &config("changed"))
            .expect_err("changed config must conflict");
    assert_eq!(error.kind(), OperatorConfigStoreErrorKind::Incompatible);
    assert_eq!(fs::read(&path).expect("re-read config"), before);

    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("weaken mode");
    let error = OperatorConfigStore::load(&request(&path), &Context::default())
        .expect_err("public config must fail");
    assert_eq!(error.kind(), OperatorConfigStoreErrorKind::UnsafeFilesystem);
}

#[test]
fn missing_invalid_and_unsupported_default_are_distinct() {
    let root = TempRoot::new("errors");
    let missing = root.0.join("missing.json");
    assert_eq!(
        OperatorConfigStore::load(&request(&missing), &Context::default())
            .expect_err("missing")
            .kind(),
        OperatorConfigStoreErrorKind::Missing
    );
    assert_eq!(
        OperatorConfigStore::load(
            &OperatorConfigDiscoveryRequest::new(Some(OsString::from("relative.json"))),
            &Context::default(),
        )
        .expect_err("relative")
        .kind(),
        OperatorConfigStoreErrorKind::InvalidLocation
    );
    assert_eq!(
        OperatorConfigStore::load(
            &OperatorConfigDiscoveryRequest::default(),
            &Context::default(),
        )
        .expect_err("unsupported default")
        .kind(),
        OperatorConfigStoreErrorKind::UnsupportedPlatform
    );
}

#[test]
fn macos_default_creates_only_the_private_managed_directory() {
    let root = TempRoot::new("default");
    let application_support = root.0.join("Library").join("Application Support");
    fs::create_dir_all(&application_support).expect("create default ancestors");
    fs::set_permissions(root.0.join("Library"), fs::Permissions::from_mode(0o700))
        .expect("set Library mode");
    fs::set_permissions(&application_support, fs::Permissions::from_mode(0o700))
        .expect("set Application Support mode");
    let context = Context {
        home: Some(root.0.as_os_str().to_owned()),
        supports_default: true,
        ..Context::default()
    };
    let desired = config("default");
    let receipt = OperatorConfigStore::create(
        &OperatorConfigDiscoveryRequest::default(),
        &context,
        &desired,
    )
    .expect("create default config");
    assert_eq!(receipt.source(), OperatorConfigSource::MacosDefault);
    let managed = application_support.join("SmolRunner");
    assert_eq!(
        fs::metadata(&managed)
            .expect("managed metadata")
            .permissions()
            .mode()
            & 0o7777,
        0o700
    );
    assert_eq!(
        fs::metadata(managed.join("config.json"))
            .expect("config metadata")
            .permissions()
            .mode()
            & 0o7777,
        0o600
    );
    assert_eq!(context.environment_calls.get(), 1);
    assert_eq!(context.home_calls.get(), 1);
}

#[test]
fn strict_version_unknown_field_and_symlink_cases_fail_closed() {
    let root = TempRoot::new("strict");
    let path = root.0.join("config.json");
    let desired = config("strict");
    let mut document: serde_json::Value =
        serde_json::from_slice(&desired.encode_persisted_json().expect("encode"))
            .expect("document");
    document["schema_version"] = serde_json::Value::from(2_u64);
    fs::write(&path, serde_json::to_vec(&document).expect("version bytes")).expect("write version");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("private mode");
    assert_eq!(
        OperatorConfigStore::load(&request(&path), &Context::default())
            .expect_err("unsupported version")
            .kind(),
        OperatorConfigStoreErrorKind::UnsupportedVersion
    );

    document["schema_version"] = serde_json::Value::from(1_u64);
    document["unexpected"] = serde_json::Value::Bool(true);
    fs::write(&path, serde_json::to_vec(&document).expect("unknown bytes")).expect("write unknown");
    assert_eq!(
        OperatorConfigStore::load(&request(&path), &Context::default())
            .expect_err("unknown field")
            .kind(),
        OperatorConfigStoreErrorKind::InvalidDocument
    );

    fs::remove_file(&path).expect("remove document");
    let target = root.0.join("target.json");
    fs::write(
        &target,
        desired.encode_persisted_json().expect("target bytes"),
    )
    .expect("write target");
    fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).expect("target mode");
    symlink(&target, &path).expect("create symlink");
    assert_eq!(
        OperatorConfigStore::load(&request(&path), &Context::default())
            .expect_err("symlink")
            .kind(),
        OperatorConfigStoreErrorKind::UnsafeFilesystem
    );
}

#[test]
fn stale_stage_and_request_debug_are_private_and_fail_closed() {
    let root = TempRoot::new("stale-stage");
    let path = root.0.join("config.json");
    let desired = config("stale-stage-private");
    OperatorConfigStore::create(&request(&path), &Context::default(), &desired)
        .expect("create config");
    let current = fs::read(&path).expect("current bytes");
    let stage = root.0.join(".config.json.next");
    fs::write(
        &stage,
        desired.encode_persisted_json().expect("stage bytes"),
    )
    .expect("write stale stage");
    fs::set_permissions(&stage, fs::Permissions::from_mode(0o600)).expect("stage mode");
    let staged_before = fs::read(&stage).expect("staged before");

    let error = OperatorConfigStore::load(&request(&path), &Context::default())
        .expect_err("stale stage must block load");
    assert_eq!(error.kind(), OperatorConfigStoreErrorKind::InvalidDocument);
    assert_eq!(fs::read(&path).expect("current after"), current);
    assert_eq!(fs::read(&stage).expect("stage after"), staged_before);

    let request_debug = format!("{:?}", request(&path));
    assert!(!request_debug.contains(root.0.to_string_lossy().as_ref()));
    assert!(!format!("{error:?} {error}").contains(root.0.to_string_lossy().as_ref()));
}

#[test]
fn unsafe_aliases_hardlinks_and_missing_parent_creation_fail_closed() {
    let root = TempRoot::new("filesystem");
    let missing_parent = root.0.join("missing").join("config.json");
    let error = OperatorConfigStore::create(
        &request(&missing_parent),
        &Context::default(),
        &config("missing-parent"),
    )
    .expect_err("explicit parent must not be created");
    assert_eq!(error.kind(), OperatorConfigStoreErrorKind::Missing);
    assert!(!root.0.join("missing").exists());

    let path = root.0.join("config.json");
    OperatorConfigStore::create(&request(&path), &Context::default(), &config("hardlink"))
        .expect("create config");
    fs::hard_link(&path, root.0.join("alias.json")).expect("create hard link");
    assert_eq!(
        OperatorConfigStore::load(&request(&path), &Context::default())
            .expect_err("hard link")
            .kind(),
        OperatorConfigStoreErrorKind::UnsafeFilesystem
    );

    let linked_parent = root.0.join("linked-parent");
    symlink(&root.0, &linked_parent).expect("parent symlink");
    assert_eq!(
        OperatorConfigStore::load(
            &request(&linked_parent.join("config.json")),
            &Context::default(),
        )
        .expect_err("parent symlink")
        .kind(),
        OperatorConfigStoreErrorKind::UnsafeFilesystem
    );

    let writable_parent = root.0.join("writable-parent");
    let private_child = writable_parent.join("private-child");
    fs::create_dir(&writable_parent).expect("create writable ancestor");
    fs::set_permissions(&writable_parent, fs::Permissions::from_mode(0o777))
        .expect("set writable ancestor mode");
    fs::create_dir(&private_child).expect("create private descendant");
    fs::set_permissions(&private_child, fs::Permissions::from_mode(0o700))
        .expect("set private descendant mode");
    let unsafe_path = private_child.join("config.json");
    assert_eq!(
        OperatorConfigStore::create(
            &request(&unsafe_path),
            &Context::default(),
            &config("writable-ancestor"),
        )
        .expect_err("writable ancestor")
        .kind(),
        OperatorConfigStoreErrorKind::UnsafeFilesystem
    );
    assert!(!unsafe_path.exists());

    let non_utf8 = OsString::from_vec(b"/private/config-\xff.json".to_vec());
    assert_eq!(
        OperatorConfigStore::load(
            &OperatorConfigDiscoveryRequest::new(Some(non_utf8)),
            &Context::default(),
        )
        .expect_err("non-UTF-8")
        .kind(),
        OperatorConfigStoreErrorKind::InvalidLocation
    );
}

#[test]
fn concurrent_create_never_overwrites_and_replay_is_idempotent() {
    let root = TempRoot::new("concurrent");
    let path = root.0.join("config.json");
    let desired = config("concurrent");
    let barrier = Arc::new(Barrier::new(2));
    let mut handles = Vec::new();
    for _ in 0..2 {
        let barrier = Arc::clone(&barrier);
        let path = path.clone();
        let desired = desired.clone();
        handles.push(thread::spawn(move || {
            barrier.wait();
            OperatorConfigStore::create(&request(&path), &Context::default(), &desired)
                .map(|receipt| receipt.disposition())
                .map_err(|error| error.kind())
        }));
    }
    let results: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().expect("creator thread"))
        .collect();
    assert!(
        results
            .iter()
            .filter(|result| **result == Ok(OperatorConfigCreateDisposition::Created))
            .count()
            <= 1
    );
    assert!(results.iter().all(|result| {
        matches!(
            result,
            Ok(OperatorConfigCreateDisposition::Created)
                | Ok(OperatorConfigCreateDisposition::AlreadyExists)
                | Err(OperatorConfigStoreErrorKind::InvalidDocument)
        )
    }));
    let replay = OperatorConfigStore::create(&request(&path), &Context::default(), &desired)
        .expect("replay after concurrent create");
    assert_eq!(
        replay.disposition(),
        OperatorConfigCreateDisposition::AlreadyExists
    );
}

#[test]
fn public_error_projection_uses_the_closed_operator_catalog() {
    let root = TempRoot::new("public-error");
    let error =
        OperatorConfigStore::load(&request(&root.0.join("missing.json")), &Context::default())
            .expect_err("missing config");
    assert_eq!(
        error.public().code(),
        OperatorErrorCode::ConfigurationMissing
    );
    let public = format!("{error:?} {error}");
    assert!(!public.contains(root.0.to_string_lossy().as_ref()));
}
