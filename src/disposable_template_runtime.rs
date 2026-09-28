//! Same-lock runtime supervision for the controller-owned disposable Lima source template.
//!
//! Lima owns VM creation, provisioning, stopping, and deletion. This module owns only the small
//! product-specific boundary around those calls: a fixed pinned template, a private Lima home,
//! exact observations, a durable Started checkpoint before mutation, bounded subprocesses, and no
//! command replay after an ambiguous crash.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::artifact::Sha256Digest;
use crate::disposable_prepared_template::{
    DisposablePreparedTemplateIdentity, DisposablePreparedTemplateManifest,
    current_disposable_lima_template_bytes, current_disposable_prepared_template,
};
use crate::disposable_template_generation::{
    DisposableTemplateGenerationDisposition, DisposableTemplateGenerationDocument,
    DisposableTemplateGenerationId, DisposableTemplateGenerationPhase,
    DisposableTemplateObjectIdentity, DisposableTemplateObservedState,
    DisposableTemplatePriorOperationState, DisposableTemplateSourceIdentity,
    reconcile_disposable_template_generation, runtime_disposable_template_observation,
};
use crate::disposable_worker_reconciler::DisposableWorkerResources;
use crate::lima_host_identity::{LimaHostIdentityAdapter, LimaHostIdentityObservation};
use crate::lima_observation::{
    LIMACTL_SAFE_PATH, LimaArchitecture, LimaGuestObservation, LimaInstanceName,
    LimaObservationAdapter, LimaObservationClock, LimaObservationFailure,
    LimaObservationRefusalCode, LimaObservationRequest, LimaRuntimeState, LimaVmType,
};
use crate::personal_worker_store::{PersonalWorkerStoreError, PersonalWorkerStoreErrorKind};
use crate::process::{CommandExecutor, CommandSpec, ExecutionRecord, TimedCommandExecutor};
use crate::unix_personal_worker_store::disposable_template_generation::TEMPLATE_INPUT_DOCUMENT;
use crate::unix_personal_worker_store::{STORE_DIRECTORY, UnixPersonalWorkerStore};

pub const DISPOSABLE_TEMPLATE_RUNTIME_SCHEMA_VERSION: u8 = 1;

const SOURCE_IDENTITY_DOMAIN: &[u8] = b"smolrunner-disposable-template-source-v3";
const GENERATION_ID_DOMAIN: &[u8] = b"smolrunner-disposable-template-generation-v1";
const MAX_PRIVATE_PATH_BYTES: usize = 1_024;
const OBSERVATION_TIMEOUT: Duration = Duration::from_secs(30);
const OBSERVATION_MAX_AGE_SECONDS: u64 = 30;
const CREATE_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const STOP_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const DISCARD_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const GUEST_CACHE_PATH: &str = "/opt/smolrunner/actions-runner/_work";
const JIT_LAUNCHER: &str = "/opt/smolrunner/bin/smolrunner-jit-launcher";
const JIT_LAUNCHER_BYTES: &[u8] = include_bytes!("../examples/lima/glaeda-jit-launcher");
const RUNNER_INTEGRITY: &str = "/opt/smolrunner/bin/smolrunner-runner-integrity";
const RUNNER_INTEGRITY_BYTES: &[u8] = include_bytes!("../examples/lima/glaeda-runner-integrity");
const RUNNER_USER: &str = "smolrunner-runner";
const EXPECTED_RUNNER_GROUPS: &str = "smolrunner-runner\n";
const EXPECTED_RUNNER_INTEGRITY: &str = "smolrunner-runner-integrity-ok\n";
const APT_POLICY_PATH: &str = "/etc/apt/apt.conf.d/99-smolrunner-no-automatic-updates";
const APT_POLICY_BYTES: &[u8] = b"APT::Periodic::Enable \"0\";\nAPT::Periodic::Update-Package-Lists \"0\";\nAPT::Periodic::Unattended-Upgrade \"0\";\n";
const MASKED_APT_UNITS: [&str; 4] = [
    "apt-daily.timer",
    "apt-daily-upgrade.timer",
    "apt-daily.service",
    "apt-daily-upgrade.service",
];
const READY_MARKER_BYTES: &[u8] = b"{\n  \"schema_version\": 2,\n  \"actions_runner_version\": \"2.336.0\",\n  \"actions_runner_digest\": \"sha256:58b758e420b87093fbd4bfddd368074960053e2f1388f01848c82624b90f27d1\",\n  \"workload_user\": \"smolrunner-runner\",\n  \"runner_listener_protection\": \"setgid_nondumpable\",\n  \"runner_install_directory\": \"/opt/smolrunner/actions-runner\",\n  \"runner_work_directory\": \"/opt/smolrunner/actions-runner/_work\",\n  \"jit_launcher_path\": \"/opt/smolrunner/bin/smolrunner-jit-launcher\",\n  \"jit_launcher_digest\": \"sha256:6f096fb518b6d40d45ca1a9923e423da436b6269fceec5a5989566abbc93a76a\"\n}\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisposableTemplateRuntimeCommandKind {
    Create,
    Stop,
    Discard,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DisposableTemplateRuntimeDisposition {
    Persisted,
    CommandCompleted {
        command: DisposableTemplateRuntimeCommandKind,
    },
    Satisfied,
    RebuildRequired,
    Refused,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DisposableTemplateRuntimeReceipt {
    pub schema_version: u8,
    pub generation_id: String,
    pub revision: u64,
    pub phase: DisposableTemplateGenerationPhase,
    pub disposition: DisposableTemplateRuntimeDisposition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisposableTemplateRuntimeErrorKind {
    InvalidConfiguration,
    DurableState,
    Observation,
    Command,
    RecoveryRequired,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct DisposableTemplateRuntimeError {
    kind: DisposableTemplateRuntimeErrorKind,
    code: &'static str,
    message: &'static str,
}

impl DisposableTemplateRuntimeError {
    #[must_use]
    pub const fn kind(&self) -> DisposableTemplateRuntimeErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::fs;
    #[cfg(target_os = "macos")]
    use std::fs::OpenOptions;
    #[cfg(target_os = "macos")]
    use std::io::{Read as _, Write as _};
    #[cfg(target_os = "macos")]
    use std::os::unix::fs::OpenOptionsExt as _;
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::disposable_template_generation::DisposableTemplateGenerationAction;
    #[cfg(target_os = "macos")]
    use crate::lima_observation::SystemLimaObservationClock;
    #[cfg(target_os = "macos")]
    use crate::process::ProcessExecutor;
    use crate::process::{CommandValue, SecretString};

    static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new(label: &str) -> Self {
            let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "smolrunner-template-runtime-{label}-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o750)).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(target_os = "macos")]
    struct PhysicalTemplateFixture {
        root: PathBuf,
        state_root: PathBuf,
        lima_home: PathBuf,
        instance: LimaInstanceName,
        root_device: u64,
        root_inode: u64,
        root_uid: u32,
        cleanup_armed: bool,
    }

    #[cfg(target_os = "macos")]
    impl PhysicalTemplateFixture {
        fn new() -> Self {
            let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
            let root = PathBuf::from("/private/tmp").join(format!(
                "smolrunner-physical-template-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&root).expect("create exact physical acceptance root");
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
                .expect("protect exact physical acceptance root");
            let metadata = fs::symlink_metadata(&root).expect("inspect physical acceptance root");
            let state_root = root.join("state");
            let lima_home = root.join("lima");
            fs::create_dir(&state_root).expect("create physical acceptance state root");
            fs::set_permissions(&state_root, fs::Permissions::from_mode(0o750))
                .expect("protect physical acceptance state root");
            fs::create_dir(&lima_home).expect("create physical acceptance Lima home");
            fs::set_permissions(&lima_home, fs::Permissions::from_mode(0o700))
                .expect("protect physical acceptance Lima home");
            Self {
                root,
                state_root,
                lima_home,
                instance: LimaInstanceName::parse(&format!(
                    "smolrunner-pt-{}-{sequence}",
                    std::process::id()
                ))
                .expect("physical instance name"),
                root_device: metadata.dev(),
                root_inode: metadata.ino(),
                root_uid: metadata.uid(),
                cleanup_armed: false,
            }
        }

        fn runtime(&self) -> DisposableTemplateRuntime {
            DisposableTemplateRuntime::new(
                &self.state_root,
                "/opt/homebrew/bin/limactl",
                &self.lima_home,
                self.instance.clone(),
            )
            .expect("construct physical template runtime")
        }

        fn seed_image_cache(&self, source: &Path) {
            let source = fs::canonicalize(source).expect("canonicalize physical cache source");
            assert!(
                source.is_absolute(),
                "physical cache source must be absolute"
            );
            let manifest = current_disposable_prepared_template().expect("current manifest");
            let cache_key = format!(
                "{:x}",
                Sha256::digest(manifest.guest_image_location().as_bytes())
            );
            assert_eq!(
                source.file_name().and_then(|name| name.to_str()),
                Some(cache_key.as_str()),
                "physical cache entry must match the pinned image URL"
            );
            assert_eq!(
                fs::read(source.join("url")).expect("read cached image URL"),
                manifest.guest_image_location().as_bytes(),
                "physical cache entry URL drifted"
            );
            assert_eq!(
                fs::read(source.join("sha256.digest")).expect("read cached image digest"),
                manifest.guest_image_digest().as_str().as_bytes(),
                "physical cache entry digest drifted"
            );
            self.seed_verified_image(&source.join("data"), &manifest, &cache_key);
        }

        fn seed_image_file(&self, source: &Path) {
            let manifest = current_disposable_prepared_template().expect("current manifest");
            let cache_key = format!(
                "{:x}",
                Sha256::digest(manifest.guest_image_location().as_bytes())
            );
            self.seed_verified_image(source, &manifest, &cache_key);
        }

        fn seed_verified_image(
            &self,
            source: &Path,
            manifest: &DisposablePreparedTemplateManifest,
            cache_key: &str,
        ) {
            let original_metadata =
                fs::symlink_metadata(source).expect("inspect physical image source pathname");
            assert!(
                original_metadata.file_type().is_file()
                    && !original_metadata.file_type().is_symlink(),
                "physical image source must be one regular file"
            );
            let source_path = fs::canonicalize(source).expect("canonicalize physical image source");
            let path_metadata =
                fs::symlink_metadata(&source_path).expect("inspect physical image source");
            assert!(
                path_metadata.file_type().is_file()
                    && !path_metadata.file_type().is_symlink()
                    && path_metadata.dev() == original_metadata.dev()
                    && path_metadata.ino() == original_metadata.ino(),
                "physical image source pathname changed before copy"
            );
            let mut source_file = fs::File::open(&source_path).expect("open physical image source");
            let held_metadata = source_file
                .metadata()
                .expect("inspect held physical image source");
            assert!(
                held_metadata.file_type().is_file()
                    && held_metadata.dev() == path_metadata.dev()
                    && held_metadata.ino() == path_metadata.ino(),
                "physical image source changed before copy"
            );

            let destination = self
                .state_root
                .join("Library/Caches/lima/download/by-url-sha256")
                .join(cache_key);
            fs::create_dir_all(&destination).expect("create isolated physical Lima cache entry");
            fs::set_permissions(&destination, fs::Permissions::from_mode(0o700))
                .expect("protect isolated physical Lima cache entry");
            let data_path = destination.join("data");
            let mut data_file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&data_path)
                .expect("create isolated physical image cache data");
            let mut hasher = Sha256::new();
            let mut buffer = vec![0_u8; 1024 * 1024];
            loop {
                let read = source_file
                    .read(&mut buffer)
                    .expect("read held physical image source");
                if read == 0 {
                    break;
                }
                hasher.update(&buffer[..read]);
                data_file
                    .write_all(&buffer[..read])
                    .expect("write isolated physical image cache data");
            }
            data_file
                .sync_all()
                .expect("synchronize isolated physical image cache data");
            let copied_digest = format!("sha256:{:x}", hasher.finalize());
            assert_eq!(
                copied_digest,
                manifest.guest_image_digest().as_str(),
                "physical image source digest drifted"
            );
            drop(data_file);
            let destination_digest = hash_physical_image(&data_path);
            assert_eq!(
                destination_digest,
                manifest.guest_image_digest().as_str(),
                "isolated physical image cache data drifted"
            );
            fs::write(destination.join("url"), manifest.guest_image_location())
                .expect("write isolated cache URL");
            fs::write(
                destination.join("sha256.digest"),
                manifest.guest_image_digest().as_str(),
            )
            .expect("write isolated cache digest");
        }

        fn arm_cleanup(&mut self) {
            self.cleanup_armed = true;
        }

        fn cleanup(&mut self) -> Result<(), &'static str> {
            let instance_directory = self.lima_home.join(self.instance.as_str());
            if self.cleanup_armed && instance_directory.exists() {
                let command = CommandSpec::new("/opt/homebrew/bin/limactl")
                    .argument("--tty=false")
                    .secret_environment("HOME", exact_path(&self.state_root))
                    .secret_environment("LIMA_HOME", exact_path(&self.lima_home))
                    .environment("LANG", "C")
                    .environment("LC_ALL", "C")
                    .environment("PATH", LIMACTL_SAFE_PATH)
                    .argument("delete")
                    .argument("--force")
                    .argument(self.instance.as_str());
                let record = ProcessExecutor
                    .execute_with_timeout(&command, DISCARD_TIMEOUT)
                    .map_err(|_| "physical acceptance cleanup command failed")?;
                if !record.success || instance_directory.exists() {
                    return Err("physical acceptance instance cleanup was not proven");
                }
            }
            let metadata = fs::symlink_metadata(&self.root)
                .map_err(|_| "physical acceptance root is unavailable")?;
            if !metadata.file_type().is_dir()
                || metadata.file_type().is_symlink()
                || metadata.dev() != self.root_device
                || metadata.ino() != self.root_inode
                || metadata.uid() != self.root_uid
                || metadata.mode() & 0o777 != 0o700
            {
                return Err("physical acceptance root identity drifted");
            }
            fs::remove_dir_all(&self.root)
                .map_err(|_| "physical acceptance root cleanup failed")?;
            self.cleanup_armed = false;
            Ok(())
        }
    }

    #[cfg(target_os = "macos")]
    impl Drop for PhysicalTemplateFixture {
        fn drop(&mut self) {
            if self.root.exists() && self.cleanup().is_err() {
                eprintln!("physical template acceptance retained recovery state");
            }
        }
    }

    #[cfg(target_os = "macos")]
    fn hash_physical_image(path: &Path) -> String {
        let path_metadata = fs::symlink_metadata(path).expect("inspect physical image cache data");
        assert!(
            path_metadata.file_type().is_file() && !path_metadata.file_type().is_symlink(),
            "physical image cache data must be one regular file"
        );
        let mut file = fs::File::open(path).expect("open physical image cache data");
        let held_metadata = file.metadata().expect("inspect held image cache data");
        assert!(
            held_metadata.file_type().is_file()
                && held_metadata.dev() == path_metadata.dev()
                && held_metadata.ino() == path_metadata.ino(),
            "physical image cache data changed before hashing"
        );
        let mut hasher = Sha256::new();
        let mut buffer = vec![0_u8; 1024 * 1024];
        loop {
            let read = file
                .read(&mut buffer)
                .expect("hash physical image cache data");
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        let final_metadata = fs::symlink_metadata(path).expect("reinspect image cache data");
        assert!(
            final_metadata.dev() == held_metadata.dev()
                && final_metadata.ino() == held_metadata.ino()
                && final_metadata.len() == held_metadata.len(),
            "physical image cache data changed while hashing"
        );
        format!("sha256:{:x}", hasher.finalize())
    }

    #[cfg(target_os = "macos")]
    #[derive(Default)]
    struct PhysicalAcceptanceExecutor {
        records: RefCell<Vec<ExecutionRecord>>,
    }

    #[cfg(target_os = "macos")]
    type PhysicalCommandDiagnostic = (Vec<String>, Option<i32>, bool, String, String);

    #[cfg(target_os = "macos")]
    impl PhysicalAcceptanceExecutor {
        fn redacted_records(&self, private_root: &Path) -> Vec<PhysicalCommandDiagnostic> {
            let private_root = exact_path(private_root);
            self.records
                .borrow()
                .iter()
                .map(|record| {
                    (
                        record.argv.clone(),
                        record.status,
                        record.success,
                        record.stdout.replace(&private_root, "<physical-root>"),
                        record.stderr.replace(&private_root, "<physical-root>"),
                    )
                })
                .collect()
        }
    }

    #[cfg(target_os = "macos")]
    impl CommandExecutor for PhysicalAcceptanceExecutor {
        fn execute(&self, spec: &CommandSpec) -> io::Result<ExecutionRecord> {
            let record = ProcessExecutor.execute(spec)?;
            self.records.borrow_mut().push(record.clone());
            Ok(record)
        }
    }

    #[cfg(target_os = "macos")]
    impl TimedCommandExecutor for PhysicalAcceptanceExecutor {
        fn execute_with_timeout(
            &self,
            spec: &CommandSpec,
            timeout: Duration,
        ) -> io::Result<ExecutionRecord> {
            let record = ProcessExecutor.execute_with_timeout(spec, timeout)?;
            self.records.borrow_mut().push(record.clone());
            Ok(record)
        }
    }

    #[cfg(target_os = "macos")]
    struct PhysicalTimedOutCreateExecutor {
        delegate: PhysicalAcceptanceExecutor,
        create_calls: Cell<u8>,
        create_error_kind: Cell<Option<io::ErrorKind>>,
    }

    #[cfg(target_os = "macos")]
    impl Default for PhysicalTimedOutCreateExecutor {
        fn default() -> Self {
            Self {
                delegate: PhysicalAcceptanceExecutor::default(),
                create_calls: Cell::new(0),
                create_error_kind: Cell::new(None),
            }
        }
    }

    #[cfg(target_os = "macos")]
    impl CommandExecutor for PhysicalTimedOutCreateExecutor {
        fn execute(&self, spec: &CommandSpec) -> io::Result<ExecutionRecord> {
            self.delegate.execute(spec)
        }
    }

    #[cfg(target_os = "macos")]
    impl TimedCommandExecutor for PhysicalTimedOutCreateExecutor {
        fn execute_with_timeout(
            &self,
            spec: &CommandSpec,
            timeout: Duration,
        ) -> io::Result<ExecutionRecord> {
            let argv = spec.displayed_argv();
            if argv.iter().any(|value| value == "start")
                && argv.iter().any(|value| value == "--name")
            {
                self.create_calls
                    .set(self.create_calls.get().saturating_add(1));
                let result = ProcessExecutor.execute_with_timeout(spec, Duration::from_secs(20));
                if let Err(error) = &result {
                    self.create_error_kind.set(Some(error.kind()));
                }
                return result;
            }
            self.delegate.execute_with_timeout(spec, timeout)
        }
    }

    #[derive(Default)]
    struct FixedClock;

    impl LimaObservationClock for FixedClock {
        fn unix_seconds(&self) -> io::Result<u64> {
            Ok(1_900_000_000)
        }
    }

    #[derive(Default)]
    struct StaleClock(Cell<u64>);

    impl LimaObservationClock for StaleClock {
        fn unix_seconds(&self) -> io::Result<u64> {
            let call = self.0.get();
            self.0.set(call + 1);
            Ok(if call < 2 {
                1_900_000_000
            } else {
                1_900_000_031
            })
        }
    }

    #[derive(Default)]
    struct FakeExecutor {
        calls: RefCell<Vec<(Vec<String>, Duration)>>,
        fail_start: bool,
        wrong_limactl_version: bool,
    }

    impl FakeExecutor {
        fn failing_start() -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                fail_start: true,
                wrong_limactl_version: false,
            }
        }

        fn wrong_limactl_version() -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                fail_start: false,
                wrong_limactl_version: true,
            }
        }

        fn calls(&self) -> Vec<(Vec<String>, Duration)> {
            self.calls.borrow().clone()
        }

        fn record(spec: &CommandSpec, stdout: &str) -> ExecutionRecord {
            ExecutionRecord {
                argv: spec.displayed_argv(),
                environment_keys: spec.environment.keys().cloned().collect(),
                status: Some(0),
                success: true,
                stdout: stdout.to_owned(),
                stderr: String::new(),
            }
        }
    }

    impl CommandExecutor for FakeExecutor {
        fn execute(&self, spec: &CommandSpec) -> io::Result<ExecutionRecord> {
            self.execute_with_timeout(spec, OBSERVATION_TIMEOUT)
        }
    }

    impl TimedCommandExecutor for FakeExecutor {
        fn execute_with_timeout(
            &self,
            spec: &CommandSpec,
            timeout: Duration,
        ) -> io::Result<ExecutionRecord> {
            let argv = spec.displayed_argv();
            self.calls.borrow_mut().push((argv.clone(), timeout));
            if argv.iter().any(|value| value == "start") && self.fail_start {
                return Err(io::Error::other("injected start failure"));
            }
            if argv.iter().any(|value| value == "list") {
                return Ok(ExecutionRecord {
                    argv: spec.displayed_argv(),
                    environment_keys: spec.environment.keys().cloned().collect(),
                    status: Some(1),
                    success: false,
                    stdout: String::new(),
                    stderr: concat!(
                        "time=\"2026-08-16T14:39:23+08:00\" level=warning msg=\"No instance matching smolrunner-source found.\"\n",
                        "time=\"2026-08-16T14:39:23+08:00\" level=fatal msg=\"unmatched instances\"\n",
                    )
                    .to_owned(),
                });
            }
            let stdout = if argv.iter().any(|value| value == "start") {
                "source started\n"
            } else if argv.len() == 3 && argv[1] == "--tty=false" && argv[2] == "--version" {
                if self.wrong_limactl_version {
                    "limactl version 2.3.0\n"
                } else {
                    "limactl version 2.2.0\n"
                }
            } else {
                ""
            };
            Ok(Self::record(spec, stdout))
        }
    }

    fn runtime(root: &TempRoot) -> DisposableTemplateRuntime {
        DisposableTemplateRuntime::new(
            root.path(),
            "/opt/homebrew/bin/limactl",
            "/private/var/lib/smolrunner/lima",
            LimaInstanceName::parse("smolrunner-source").unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn absent_source_is_authorized_then_checkpointed_before_one_fixed_create() {
        let root = TempRoot::new("create");
        let runtime = runtime(&root);
        let executor = FakeExecutor::default();
        let clock = FixedClock;

        let authorized = runtime.reconcile_once(&executor, &clock).unwrap();
        assert_eq!(
            authorized.phase,
            DisposableTemplateGenerationPhase::CreateAuthorized
        );
        assert_eq!(authorized.revision, 2);

        let executed = runtime.reconcile_once(&executor, &clock).unwrap();
        assert_eq!(
            executed.phase,
            DisposableTemplateGenerationPhase::CreateStarted
        );
        assert_eq!(executed.revision, 3);
        assert_eq!(
            executed.disposition,
            DisposableTemplateRuntimeDisposition::CommandCompleted {
                command: DisposableTemplateRuntimeCommandKind::Create
            }
        );

        let create_calls = executor
            .calls()
            .into_iter()
            .filter(|(argv, _)| argv.iter().any(|value| value == "start"))
            .collect::<Vec<_>>();
        assert_eq!(create_calls.len(), 1);
        assert_eq!(
            create_calls[0],
            (
                vec![
                    "/opt/homebrew/bin/limactl".to_owned(),
                    "--tty=false".to_owned(),
                    "start".to_owned(),
                    "--name".to_owned(),
                    "smolrunner-source".to_owned(),
                    "--timeout=10m".to_owned(),
                    "[REDACTED]".to_owned(),
                ],
                CREATE_TIMEOUT,
            )
        );

        let materialized = root
            .path()
            .join(STORE_DIRECTORY)
            .join(TEMPLATE_INPUT_DOCUMENT);
        assert_eq!(
            fs::read(&materialized).unwrap(),
            current_disposable_lima_template_bytes()
        );
        let metadata = fs::symlink_metadata(materialized).unwrap();
        assert_eq!(metadata.mode() & 0o777, 0o600);
        assert_eq!(metadata.nlink(), 1);

        let recovery = runtime.reconcile_once(&executor, &clock).unwrap_err();
        assert_eq!(
            recovery.kind(),
            DisposableTemplateRuntimeErrorKind::RecoveryRequired
        );
        assert_eq!(
            executor
                .calls()
                .into_iter()
                .filter(|(argv, _)| argv.iter().any(|value| value == "start"))
                .count(),
            1,
            "a Started operation must never replay from absent evidence"
        );
    }

    #[test]
    fn failed_create_leaves_started_recovery_without_replay() {
        let root = TempRoot::new("failed-create");
        let runtime = runtime(&root);
        let executor = FakeExecutor::failing_start();
        let clock = FixedClock;

        runtime.reconcile_once(&executor, &clock).unwrap();
        let error = runtime.reconcile_once(&executor, &clock).unwrap_err();
        assert_eq!(
            error.kind(),
            DisposableTemplateRuntimeErrorKind::RecoveryRequired
        );
        let durable =
            UnixPersonalWorkerStore::open_or_create_disposable_template_generation(root.path())
                .unwrap()
                .load_disposable_template_generation()
                .unwrap()
                .unwrap();
        assert_eq!(
            durable.phase(),
            DisposableTemplateGenerationPhase::CreateStarted
        );

        let _ = runtime.reconcile_once(&executor, &clock).unwrap_err();
        assert_eq!(
            executor
                .calls()
                .into_iter()
                .filter(|(argv, _)| argv.iter().any(|value| value == "start"))
                .count(),
            1
        );
    }

    #[test]
    fn mismatched_materialized_template_refuses_before_started_or_command() {
        let root = TempRoot::new("template-drift");
        let runtime = runtime(&root);
        let executor = FakeExecutor::default();
        let clock = FixedClock;

        runtime.reconcile_once(&executor, &clock).unwrap();
        let input = root
            .path()
            .join(STORE_DIRECTORY)
            .join(TEMPLATE_INPUT_DOCUMENT);
        fs::write(&input, b"foreign template\n").unwrap();
        fs::set_permissions(&input, fs::Permissions::from_mode(0o600)).unwrap();

        let error = runtime.reconcile_once(&executor, &clock).unwrap_err();
        assert_eq!(
            error.kind(),
            DisposableTemplateRuntimeErrorKind::RecoveryRequired
        );
        let durable =
            UnixPersonalWorkerStore::open_or_create_disposable_template_generation(root.path())
                .unwrap()
                .load_disposable_template_generation()
                .unwrap()
                .unwrap();
        assert_eq!(
            durable.phase(),
            DisposableTemplateGenerationPhase::CreateAuthorized
        );
        assert!(
            !executor
                .calls()
                .iter()
                .any(|(argv, _)| argv.iter().any(|value| value == "start"))
        );
    }

    #[test]
    fn stop_and_discard_use_only_fixed_bounded_commands() {
        let root = TempRoot::new("commands");
        let runtime = runtime(&root);
        let executor = FakeExecutor::default();
        let initial = DisposableTemplateGenerationDocument::runtime_initial(
            runtime.generation_id.clone(),
            runtime.prepared_template_identity.clone(),
            runtime.source_identity.clone(),
            runtime.source_instance.clone(),
        );
        let create_started = initial
            .transition(1, DisposableTemplateGenerationAction::AuthorizeCreate, None)
            .unwrap()
            .transition(
                2,
                DisposableTemplateGenerationAction::RecordCreateStarted,
                None,
            )
            .unwrap();
        runtime
            .execute(
                DisposableTemplateRuntimeCommandKind::Create,
                &create_started,
                None,
                &executor,
            )
            .unwrap();

        let object = DisposableTemplateObjectIdentity::from_host_digest(
            Sha256Digest::parse(&format!("sha256:{}", "ab".repeat(32))).unwrap(),
        );
        let stop_started = create_started
            .transition(
                3,
                DisposableTemplateGenerationAction::RecordVerified,
                Some(object),
            )
            .unwrap()
            .transition(4, DisposableTemplateGenerationAction::AuthorizeStop, None)
            .unwrap()
            .transition(
                5,
                DisposableTemplateGenerationAction::RecordStopStarted,
                None,
            )
            .unwrap();
        assert_eq!(
            stop_started.phase(),
            DisposableTemplateGenerationPhase::StopStarted
        );
        let (stop, stop_timeout) =
            runtime.fixed_command(DisposableTemplateRuntimeCommandKind::Stop);
        executor.execute_with_timeout(&stop, stop_timeout).unwrap();

        let discard_started = create_started
            .transition(
                3,
                DisposableTemplateGenerationAction::AuthorizeDiscard,
                None,
            )
            .unwrap()
            .transition(
                4,
                DisposableTemplateGenerationAction::RecordDiscardStarted,
                None,
            )
            .unwrap();
        assert_eq!(
            discard_started.phase(),
            DisposableTemplateGenerationPhase::DiscardStarted
        );
        let (discard, discard_timeout) =
            runtime.fixed_command(DisposableTemplateRuntimeCommandKind::Discard);
        executor
            .execute_with_timeout(&discard, discard_timeout)
            .unwrap();

        let mutations = executor
            .calls()
            .into_iter()
            .filter(|(argv, _)| {
                argv.iter()
                    .any(|value| matches!(value.as_str(), "start" | "stop" | "delete"))
            })
            .collect::<Vec<_>>();
        assert_eq!(mutations.len(), 3);
        assert_eq!(mutations[0].1, CREATE_TIMEOUT);
        assert_eq!(mutations[1].1, STOP_TIMEOUT);
        assert_eq!(mutations[2].1, DISCARD_TIMEOUT);
        assert_eq!(
            mutations[1].0,
            vec![
                "/opt/homebrew/bin/limactl",
                "--tty=false",
                "stop",
                "--force",
                "smolrunner-source",
            ]
        );
        assert_eq!(
            mutations[2].0,
            vec![
                "/opt/homebrew/bin/limactl",
                "--tty=false",
                "delete",
                "--force",
                "smolrunner-source",
            ]
        );
    }

    #[test]
    fn lima_client_cache_home_is_private_separate_and_source_bound() {
        let first_root = TempRoot::new("client-home-first");
        let second_root = TempRoot::new("client-home-second");
        let first = runtime(&first_root);
        let second = runtime(&second_root);
        let (command, _) = first.fixed_command(DisposableTemplateRuntimeCommandKind::Create);

        assert_eq!(
            command.environment.get("HOME"),
            Some(&CommandValue::Secret(SecretString::new(exact_path(
                first_root.path()
            ))))
        );
        assert_eq!(
            command.environment.get("LIMA_HOME"),
            Some(&CommandValue::Secret(SecretString::new(
                "/private/var/lib/smolrunner/lima"
            )))
        );
        assert_ne!(
            first.initial_document().source_identity(),
            second.initial_document().source_identity(),
            "the private Lima client home participates in source identity"
        );
    }

    #[test]
    fn readiness_marker_bytes_match_the_checked_in_provisioning_recipe() {
        let template = std::str::from_utf8(current_disposable_lima_template_bytes()).unwrap();
        for line in std::str::from_utf8(READY_MARKER_BYTES).unwrap().lines() {
            assert!(
                template.contains(&format!("'{line}'")),
                "checked-in Lima provisioning must emit the exact readiness line"
            );
        }
        assert!(template.contains("verify_runner_install"));
        assert!(template.contains("verify_automatic_updates_disabled"));
        for download_argument in [
            "--retry 8",
            "--retry-all-errors",
            "--retry-delay 2",
            "--retry-max-time 300",
            "--max-time 300",
            "--connect-timeout 30",
            "--speed-limit 1024",
            "--speed-time 60",
            "--continue-at -",
        ] {
            assert!(
                template.contains(download_argument),
                "runner download must retain {download_argument}"
            );
        }
        let launcher_digest = format!("{:x}", Sha256::digest(JIT_LAUNCHER_BYTES));
        assert!(template.contains(&format!(
            "readonly jit_launcher_sha256=\"{launcher_digest}\""
        )));
        let integrity_digest = format!("{:x}", Sha256::digest(RUNNER_INTEGRITY_BYTES));
        assert!(template.contains(&format!(
            "readonly integrity_verifier_sha256=\"{integrity_digest}\""
        )));
        assert!(template.contains("/usr/bin/chown -R root:root \"${install_stage}\""));
        assert!(template.contains("/usr/bin/chmod 1775 \"${install_stage}\""));
        assert!(template.contains("/usr/bin/chmod 2755 \"${install_stage}/bin/Runner.Listener\""));
        assert!(template.contains("fs.suid_dumpable = 0"));
        assert!(template.contains("--word-regexp nosuid"));
        assert!(template.contains("\"${install_stage}/_work\" \"${install_stage}/_diag\""));
        assert!(!template.contains("/usr/bin/ln --symbolic \"${runner_work}\""));
    }

    #[test]
    fn realized_config_must_match_before_and_after_readiness_without_host_mounts() {
        let expected = serde_json::json!({
            "vmType": "vz",
            "arch": "aarch64",
            "plain": true,
            "networks": [],
            "portForwards": [],
            "propagateProxyEnv": false,
            "hostResolver": { "enabled": false },
            "containerd": { "system": false, "user": false }
        });
        let before = RealizedInstanceConfig {
            runtime_state: LimaRuntimeState::Running,
            config: expected.clone(),
        };
        let middle = RealizedInstanceConfig {
            runtime_state: LimaRuntimeState::Running,
            config: expected.clone(),
        };
        let after = RealizedInstanceConfig {
            runtime_state: LimaRuntimeState::Running,
            config: expected.clone(),
        };
        assert!(exact_realized_config_matches(
            &expected,
            [&before, &middle, &after]
        ));

        let mut unsafe_after = expected.clone();
        unsafe_after["mounts"] = serde_json::json!([
            { "location": "/Users/operator", "writable": true }
        ]);
        let unsafe_after = RealizedInstanceConfig {
            runtime_state: LimaRuntimeState::Stopped,
            config: unsafe_after,
        };
        assert!(!exact_realized_config_matches(
            &expected,
            [&before, &middle, &unsafe_after]
        ));
    }

    #[test]
    fn clone_mount_policy_uses_lima_canonical_absence_and_rejects_real_mounts() {
        let mut omitted = serde_json::Map::new();
        assert!(remove_canonical_empty_mounts(&mut omitted));
        assert!(!omitted.contains_key("mounts"));

        let mut empty = serde_json::Map::from_iter([("mounts".to_owned(), serde_json::json!([]))]);
        assert!(remove_canonical_empty_mounts(&mut empty));
        assert!(!empty.contains_key("mounts"));

        let mut configured = serde_json::Map::from_iter([(
            "mounts".to_owned(),
            serde_json::json!([{ "location": "/Users/operator" }]),
        )]);
        assert!(!remove_canonical_empty_mounts(&mut configured));
    }

    #[test]
    fn wrong_lima_version_refuses_before_observation_or_mutation() {
        let root = TempRoot::new("lima-version");
        let runtime = runtime(&root);
        let executor = FakeExecutor::wrong_limactl_version();

        let error = runtime.reconcile_once(&executor, &FixedClock).unwrap_err();
        assert_eq!(error.code(), "template_lima_version_mismatch");
        assert_eq!(executor.calls().len(), 1);
        assert_eq!(
            executor.calls()[0].0,
            vec![
                "/opt/homebrew/bin/limactl".to_owned(),
                "--tty=false".to_owned(),
                "--version".to_owned()
            ]
        );
    }

    #[test]
    fn outer_observation_window_rejects_an_expired_absence_snapshot() {
        let root = TempRoot::new("outer-freshness");
        let runtime = runtime(&root);
        let executor = FakeExecutor::default();

        let error = runtime
            .reconcile_once(&executor, &StaleClock::default())
            .unwrap_err();
        assert_eq!(error.code(), "template_composite_observation_stale");
        assert!(
            !executor
                .calls()
                .iter()
                .any(|(argv, _)| argv.iter().any(|value| value == "start"))
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[should_panic(expected = "physical image source digest drifted")]
    fn physical_file_seed_rejects_bytes_that_do_not_match_the_manifest() {
        let fixture = PhysicalTemplateFixture::new();
        let source = fixture.root.join("wrong-image.img");
        fs::write(&source, b"not the pinned Ubuntu image").unwrap();

        fixture.seed_image_file(&source);
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[should_panic(expected = "physical image source digest drifted")]
    fn physical_cache_seed_rejects_valid_sidecars_with_poisoned_data() {
        let fixture = PhysicalTemplateFixture::new();
        let manifest = current_disposable_prepared_template().unwrap();
        let cache_key = format!(
            "{:x}",
            Sha256::digest(manifest.guest_image_location().as_bytes())
        );
        let source = fixture.root.join(cache_key);
        fs::create_dir(&source).unwrap();
        fs::write(source.join("url"), manifest.guest_image_location()).unwrap();
        fs::write(
            source.join("sha256.digest"),
            manifest.guest_image_digest().as_str(),
        )
        .unwrap();
        fs::write(source.join("data"), b"poisoned cached image").unwrap();

        fixture.seed_image_cache(&source);
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "creates and deletes one isolated physical Lima/VZ prepared template"]
    fn physical_prepared_template_reaches_ready_stopped_and_cleans_up() {
        assert_eq!(
            std::env::var("SMOLRUNNER_PHYSICAL_TEMPLATE_ACCEPTANCE").as_deref(),
            Ok("create-ready-stop-delete"),
            "set the exact physical acceptance opt-in token"
        );
        let mut fixture = PhysicalTemplateFixture::new();
        if let Some(source) = std::env::var_os("SMOLRUNNER_PHYSICAL_TEMPLATE_IMAGE_CACHE_ENTRY") {
            fixture.seed_image_cache(Path::new(&source));
        } else if let Some(source) = std::env::var_os("SMOLRUNNER_PHYSICAL_TEMPLATE_IMAGE_FILE") {
            fixture.seed_image_file(Path::new(&source));
        }
        let runtime = fixture.runtime();
        let executor = PhysicalAcceptanceExecutor::default();
        let clock = SystemLimaObservationClock;

        let authorized = runtime
            .reconcile_once(&executor, &clock)
            .expect("authorize physical source creation from proven absence");
        assert_eq!(
            authorized.phase,
            DisposableTemplateGenerationPhase::CreateAuthorized
        );
        fixture.arm_cleanup();

        let mut receipts = vec![authorized];
        for _ in 0..8 {
            let receipt = runtime
                .reconcile_once(&executor, &clock)
                .unwrap_or_else(|error| {
                    panic!(
                        "advance physical prepared-template lifecycle: {error:?}; records={:?}",
                        executor.redacted_records(&fixture.root)
                    )
                });
            let satisfied = receipt.disposition == DisposableTemplateRuntimeDisposition::Satisfied;
            receipts.push(receipt);
            if satisfied {
                break;
            }
        }
        assert_eq!(
            receipts.last().map(|receipt| receipt.disposition),
            Some(DisposableTemplateRuntimeDisposition::Satisfied),
            "physical source did not converge to ready and stopped: {receipts:?}"
        );
        assert!(receipts.iter().any(|receipt| {
            receipt.disposition
                == DisposableTemplateRuntimeDisposition::CommandCompleted {
                    command: DisposableTemplateRuntimeCommandKind::Create,
                }
        }));
        assert!(receipts.iter().any(|receipt| {
            receipt.disposition
                == DisposableTemplateRuntimeDisposition::CommandCompleted {
                    command: DisposableTemplateRuntimeCommandKind::Stop,
                }
        }));

        fixture
            .cleanup()
            .expect("delete the exact accepted source and its isolated state");
        assert!(!fixture.root.exists());
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "forces one isolated physical Lima/VZ create timeout and deletes all test state"]
    fn physical_timed_out_create_retains_started_debt_without_replay_and_cleans_up() {
        assert_eq!(
            std::env::var("SMOLRUNNER_PHYSICAL_TEMPLATE_ACCEPTANCE").as_deref(),
            Ok("create-timeout-delete"),
            "set the exact physical failure-acceptance opt-in token"
        );
        let mut fixture = PhysicalTemplateFixture::new();
        if let Some(source) = std::env::var_os("SMOLRUNNER_PHYSICAL_TEMPLATE_IMAGE_CACHE_ENTRY") {
            fixture.seed_image_cache(Path::new(&source));
        } else if let Some(source) = std::env::var_os("SMOLRUNNER_PHYSICAL_TEMPLATE_IMAGE_FILE") {
            fixture.seed_image_file(Path::new(&source));
        }
        let runtime = fixture.runtime();
        let executor = PhysicalTimedOutCreateExecutor::default();
        let clock = SystemLimaObservationClock;

        let authorized = runtime
            .reconcile_once(&executor, &clock)
            .expect("authorize physical source creation from proven absence");
        assert_eq!(
            authorized.phase,
            DisposableTemplateGenerationPhase::CreateAuthorized
        );
        fixture.arm_cleanup();

        let error = runtime
            .reconcile_once(&executor, &clock)
            .expect_err("the physical create command must hit the injected wall timeout");
        assert_eq!(
            error.kind(),
            DisposableTemplateRuntimeErrorKind::RecoveryRequired
        );
        assert_eq!(
            executor.create_error_kind.get(),
            Some(io::ErrorKind::TimedOut)
        );
        let durable = UnixPersonalWorkerStore::open_or_create_disposable_template_generation(
            &fixture.state_root,
        )
        .expect("reopen exact physical template store")
        .load_disposable_template_generation()
        .expect("read exact physical template generation")
        .expect("physical template generation exists");
        assert_eq!(
            durable.phase(),
            DisposableTemplateGenerationPhase::CreateStarted
        );

        let retry = runtime
            .reconcile_once(&executor, &clock)
            .expect_err("a timed-out Started command must never replay");
        assert!(matches!(
            retry.kind(),
            DisposableTemplateRuntimeErrorKind::Observation
                | DisposableTemplateRuntimeErrorKind::RecoveryRequired
        ));
        assert_eq!(executor.create_calls.get(), 1);

        fixture
            .cleanup()
            .expect("delete the timed-out source and its isolated state");
        assert!(!fixture.root.exists());
    }
}

impl fmt::Debug for DisposableTemplateRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DisposableTemplateRuntimeError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for DisposableTemplateRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for DisposableTemplateRuntimeError {}

/// Fixed supervisor for the single controller-owned disposable-worker source template.
///
/// Public callers can choose only canonical private paths and the bounded source name. Template
/// bytes, guest readiness probes, command arguments, environments, and deadlines are fixed by the
/// binary. The supervisor never accepts repository input or arbitrary guest commands.
pub struct DisposableTemplateRuntime {
    state_root: PathBuf,
    lima_home: PathBuf,
    limactl_program: PathBuf,
    source_instance: LimaInstanceName,
    source_identity: DisposableTemplateSourceIdentity,
    generation_id: DisposableTemplateGenerationId,
    prepared_template: DisposablePreparedTemplateManifest,
    prepared_template_identity: DisposablePreparedTemplateIdentity,
    observation_request: LimaObservationRequest,
    template_input_path: PathBuf,
}

struct RuntimeObservation {
    sealed: crate::disposable_template_generation::DisposableTemplateObservation,
    retained_host: Option<LimaHostIdentityObservation>,
}

/// Descriptor-retaining proof that the exact prepared source is currently ready and stopped.
pub(crate) struct ConfirmedDisposableCloneSource {
    host: LimaHostIdentityObservation,
    request: LimaObservationRequest,
}

impl ConfirmedDisposableCloneSource {
    pub(crate) fn confirm_current(&self) -> Result<(), DisposableTemplateRuntimeError> {
        self.host
            .confirm(&self.request)
            .map_err(|_| observation_failure("template_host_identity_drift"))
    }
}

#[derive(PartialEq, Eq)]
struct RealizedInstanceConfig {
    runtime_state: LimaRuntimeState,
    config: serde_json::Value,
}

impl fmt::Debug for DisposableTemplateRuntime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DisposableTemplateRuntime")
            .field("state_root", &"<private-state-root>")
            .field("lima_home", &"<private-lima-home>")
            .field("limactl_program", &"<private-program-path>")
            .field("source_instance", &self.source_instance)
            .field("generation_id", &self.generation_id)
            .finish()
    }
}

impl DisposableTemplateRuntime {
    pub(crate) fn initial_document(&self) -> DisposableTemplateGenerationDocument {
        DisposableTemplateGenerationDocument::runtime_initial(
            self.generation_id.clone(),
            self.prepared_template_identity.clone(),
            self.source_identity.clone(),
            self.source_instance.clone(),
        )
    }

    /// Construct the fixed production source-template supervisor.
    ///
    /// # Errors
    ///
    /// Returns a path-free refusal unless every host path is canonical absolute UTF-8 and the
    /// checked-in prepared-template declaration remains valid.
    pub fn new(
        state_root: impl Into<PathBuf>,
        limactl_program: impl Into<PathBuf>,
        lima_home: impl Into<PathBuf>,
        source_instance: LimaInstanceName,
    ) -> Result<Self, DisposableTemplateRuntimeError> {
        let state_root = validate_private_path(state_root.into())?;
        let limactl_program = validate_private_path(limactl_program.into())?;
        let lima_home = validate_private_path(lima_home.into())?;
        let prepared_template =
            current_disposable_prepared_template().map_err(|_| invalid_configuration())?;
        let prepared_template_identity = prepared_template
            .identity()
            .map_err(|_| invalid_configuration())?;
        let source_identity = derive_source_identity(
            &source_instance,
            &state_root,
            &lima_home,
            &limactl_program,
            prepared_template.lima_version(),
        )?;
        let generation_id = derive_generation_id(
            &source_identity,
            &prepared_template_identity,
            &source_instance,
        )?;
        let observation_request = LimaObservationRequest::new(
            source_instance.clone(),
            lima_home.clone(),
            LimaVmType::Vz,
            LimaArchitecture::Aarch64,
            GUEST_CACHE_PATH,
            OBSERVATION_MAX_AGE_SECONDS,
        )
        .map_err(|_| invalid_configuration())?;
        let template_input_path = state_root
            .join(STORE_DIRECTORY)
            .join(TEMPLATE_INPUT_DOCUMENT);
        validate_private_path(template_input_path.clone())?;
        Ok(Self {
            state_root,
            lima_home,
            limactl_program,
            source_instance,
            source_identity,
            generation_id,
            prepared_template,
            prepared_template_identity,
            observation_request,
            template_input_path,
        })
    }

    /// Advance at most one durable or external source-template action.
    ///
    /// A runtime command is executed only while the canonical state lock is held, after a second
    /// complete observation matches the advisory plan and after the corresponding Started phase
    /// has been durably published. Command completion deliberately leaves that Started phase in
    /// place; the next call must freshly observe and persist the external result.
    ///
    /// # Errors
    ///
    /// Returns a bounded path-free error for unavailable/corrupt durable state, missing or drifting
    /// Lima evidence, a command failure, or a crash-ambiguous phase that cannot be replayed.
    pub fn reconcile_once(
        &self,
        executor: &impl TimedCommandExecutor,
        clock: &impl LimaObservationClock,
    ) -> Result<DisposableTemplateRuntimeReceipt, DisposableTemplateRuntimeError> {
        let mut store = UnixPersonalWorkerStore::open_or_create_disposable_template_generation(
            &self.state_root,
        )
        .map_err(DisposableTemplateRuntimeError::from_store)?;
        let current = match store
            .load_disposable_template_generation()
            .map_err(DisposableTemplateRuntimeError::from_store)?
        {
            Some(current) => current,
            None => {
                let initial = self.initial_document();
                store
                    .create_disposable_template_generation(&initial)
                    .map_err(DisposableTemplateRuntimeError::from_store)?;
                initial
            }
        };
        self.validate_document_identity(&current)?;

        let first = self.observe(&current, executor, clock)?;
        let plan = reconcile_disposable_template_generation(&current, &first.sealed);
        match plan.disposition() {
            DisposableTemplateGenerationDisposition::Persist { .. } => {
                let confirmation = self.observe(&current, executor, clock)?.sealed;
                let successor = store
                    .persist_confirmed_disposable_template_generation(plan, confirmation)
                    .map_err(DisposableTemplateRuntimeError::from_store)?;
                Ok(receipt(
                    &successor,
                    DisposableTemplateRuntimeDisposition::Persisted,
                ))
            }
            DisposableTemplateGenerationDisposition::CreateCandidate
            | DisposableTemplateGenerationDisposition::StopCandidate
            | DisposableTemplateGenerationDisposition::DiscardCandidate => {
                let command_kind = command_kind(plan.disposition());
                let result = store
                    .execute_confirmed_disposable_template_candidate(
                        plan,
                        &self.state_root,
                        current_disposable_lima_template_bytes(),
                        |locked| {
                            let observed = self.observe(locked, executor, clock)?;
                            Ok((observed.sealed, observed.retained_host))
                        },
                        |started, retained_host| {
                            self.execute(command_kind, started, retained_host, executor)
                        },
                    )
                    .map_err(DisposableTemplateRuntimeError::from_store)?;
                let (started, ()) = result?;
                Ok(receipt(
                    &started,
                    DisposableTemplateRuntimeDisposition::CommandCompleted {
                        command: command_kind,
                    },
                ))
            }
            DisposableTemplateGenerationDisposition::Satisfied => Ok(receipt(
                &current,
                DisposableTemplateRuntimeDisposition::Satisfied,
            )),
            DisposableTemplateGenerationDisposition::RebuildRequired => Ok(receipt(
                &current,
                DisposableTemplateRuntimeDisposition::RebuildRequired,
            )),
            DisposableTemplateGenerationDisposition::Refuse { reason } => {
                let kind = if matches!(
                    reason,
                    crate::disposable_template_generation::DisposableTemplateGenerationRefusal::RecoveryRequired
                ) {
                    DisposableTemplateRuntimeErrorKind::RecoveryRequired
                } else {
                    DisposableTemplateRuntimeErrorKind::Observation
                };
                Err(runtime_error(
                    kind,
                    "template_runtime_refused",
                    "the disposable-template runtime refused the current observed state",
                ))
            }
        }
    }

    /// Reconfirm that the exact durable source generation is ready and stopped for cloning.
    pub(crate) fn confirm_stopped_clone_source(
        &self,
        document: &DisposableTemplateGenerationDocument,
        executor: &impl TimedCommandExecutor,
        clock: &impl LimaObservationClock,
    ) -> Result<ConfirmedDisposableCloneSource, DisposableTemplateRuntimeError> {
        self.validate_document_identity(document)?;
        let observed = self.observe(document, executor, clock)?;
        let plan = reconcile_disposable_template_generation(document, &observed.sealed);
        if plan.disposition() != DisposableTemplateGenerationDisposition::Satisfied {
            return Err(observation_failure("template_source_not_ready_stopped"));
        }
        let host = observed
            .retained_host
            .ok_or_else(|| observation_failure("template_host_identity_missing"))?;
        host.confirm(&self.observation_request)
            .map_err(|_| observation_failure("template_host_identity_drift"))?;
        Ok(ConfirmedDisposableCloneSource {
            host,
            request: self.observation_request.clone(),
        })
    }

    /// Reconfirm one running disposable clone against the prepared-template isolation policy.
    #[allow(dead_code)] // Consumed by the pending production runner-service composition.
    pub(crate) fn confirm_running_clone_target(
        &self,
        request: &LimaObservationRequest,
        resources: DisposableWorkerResources,
        executor: &impl TimedCommandExecutor,
        clock: &impl LimaObservationClock,
    ) -> Result<LimaHostIdentityObservation, DisposableTemplateRuntimeError> {
        let composite_started = clock
            .unix_seconds()
            .map_err(|_| observation_failure("template_observation_clock_failed"))?;
        self.verify_limactl(executor)?;
        let bounded = BoundedExecutor { executor };
        let adapter = LimaObservationAdapter::new(&self.limactl_program)
            .map_err(|_| invalid_configuration())?;
        let observed = adapter
            .observe(request, &bounded, clock)
            .map_err(|error| observation_error(&error))?;
        let report = observed.report();
        let expected_cpus = u16::try_from(resources.cpu_millis() / 1_000)
            .map_err(|_| observation_failure("template_clone_resources_invalid"))?;
        if !resources.cpu_millis().is_multiple_of(1_000)
            || !resources.memory_bytes().is_multiple_of(1 << 30)
            || !resources.disk_bytes().is_multiple_of(1 << 30)
            || report.configured.runtime_state != LimaRuntimeState::Running
            || report.configured.cpus != expected_cpus
            || report.configured.memory_bytes != resources.memory_bytes()
            || report.configured.primary_disk_bytes != resources.disk_bytes()
            || !matches!(report.guest, LimaGuestObservation::Observed(_))
        {
            return Err(observation_failure(
                "template_clone_resources_or_state_mismatch",
            ));
        }

        let expected_config = self.validated_clone_config(resources, executor)?;
        let first_config = self.observed_instance_config_for(request.instance(), executor)?;
        if first_config.runtime_state != LimaRuntimeState::Running
            || first_config.config != expected_config
            || !self.ready_probe_for(request.instance(), executor)?
        {
            return Err(observation_failure("template_clone_policy_mismatch"));
        }
        let host = LimaHostIdentityAdapter
            .observe(request)
            .map_err(|_| observation_failure("template_host_identity_unavailable"))?;
        if host.root_disk_bytes() != resources.disk_bytes() {
            return Err(observation_failure("template_host_disk_mismatch"));
        }
        host.confirm(request)
            .map_err(|_| observation_failure("template_host_identity_drift"))?;

        let middle_config = self.observed_instance_config_for(request.instance(), executor)?;
        let final_observed = adapter
            .observe(request, &bounded, clock)
            .map_err(|error| observation_error(&error))?;
        if final_observed.report().configured != report.configured
            || final_observed.report().guest != report.guest
        {
            return Err(observation_failure("template_composite_observation_drift"));
        }
        let final_config = self.observed_instance_config_for(request.instance(), executor)?;
        self.verify_limactl(executor)?;
        if !exact_realized_config_matches(
            &expected_config,
            [&first_config, &middle_config, &final_config],
        ) || final_config.runtime_state != LimaRuntimeState::Running
        {
            return Err(observation_failure("template_clone_policy_mismatch"));
        }
        host.confirm(request)
            .map_err(|_| observation_failure("template_host_identity_drift"))?;
        ensure_composite_observation_fresh(clock, composite_started)?;
        Ok(host)
    }

    fn validate_document_identity(
        &self,
        document: &DisposableTemplateGenerationDocument,
    ) -> Result<(), DisposableTemplateRuntimeError> {
        if document.generation_id() != &self.generation_id
            || document.prepared_template_identity() != &self.prepared_template_identity
            || document.source_identity() != &self.source_identity
            || document.source_instance() != &self.source_instance
        {
            return Err(runtime_error(
                DisposableTemplateRuntimeErrorKind::DurableState,
                "template_generation_identity_mismatch",
                "durable template-generation identity differs from the configured runtime",
            ));
        }
        Ok(())
    }

    fn observe(
        &self,
        document: &DisposableTemplateGenerationDocument,
        executor: &impl TimedCommandExecutor,
        clock: &impl LimaObservationClock,
    ) -> Result<RuntimeObservation, DisposableTemplateRuntimeError> {
        let composite_started = clock
            .unix_seconds()
            .map_err(|_| observation_failure("template_observation_clock_failed"))?;
        self.verify_limactl(executor)?;
        let bounded = BoundedExecutor { executor };
        let adapter = LimaObservationAdapter::new(&self.limactl_program)
            .map_err(|_| invalid_configuration())?;
        let observed = match adapter.observe(&self.observation_request, &bounded, clock) {
            Ok(observed) => observed,
            Err(error) if error.code == LimaObservationRefusalCode::MissingInstanceEvidence => {
                ensure_composite_observation_fresh(clock, composite_started)?;
                return Ok(RuntimeObservation {
                    sealed: runtime_disposable_template_observation(
                        document,
                        self.source_identity.clone(),
                        None,
                        None,
                        prior_operation(document, DisposableTemplateObservedState::Absent),
                        DisposableTemplateObservedState::Absent,
                    ),
                    retained_host: None,
                });
            }
            Err(error) => return Err(observation_error(&error)),
        };

        let report = observed.report();
        let resources_match = u32::from(report.configured.cpus)
            == self.prepared_template.source_cpu_count()
            && report.configured.memory_bytes == self.prepared_template.source_memory_bytes()
            && report.configured.primary_disk_bytes == self.prepared_template.source_disk_bytes();
        let expected_config = self.validated_template_config(executor)?;
        let first_config = self.observed_instance_config(executor)?;
        if first_config.runtime_state != report.configured.runtime_state {
            return Err(observation_failure("template_composite_observation_drift"));
        }
        let host = LimaHostIdentityAdapter
            .observe(&self.observation_request)
            .map_err(|_| observation_failure("template_host_identity_unavailable"))?;
        if host.root_disk_bytes() != report.configured.primary_disk_bytes {
            return Err(observation_failure("template_host_disk_mismatch"));
        }
        host.confirm(&self.observation_request)
            .map_err(|_| observation_failure("template_host_identity_drift"))?;
        let object_identity =
            DisposableTemplateObjectIdentity::from_host_digest(host.identity().digest().clone());
        if document
            .owned_object_identity()
            .is_some_and(|expected| expected != &object_identity)
        {
            return Ok(RuntimeObservation {
                sealed: runtime_disposable_template_observation(
                    document,
                    self.source_identity.clone(),
                    Some(object_identity),
                    None,
                    prior_operation(document, DisposableTemplateObservedState::Conflicting),
                    DisposableTemplateObservedState::Conflicting,
                ),
                retained_host: Some(host),
            });
        }

        let ready = report.configured.runtime_state == LimaRuntimeState::Running
            && resources_match
            && first_config.config == expected_config
            && self.ready_probe(executor)?;
        let middle_config = self.observed_instance_config(executor)?;
        let final_observed = adapter
            .observe(&self.observation_request, &bounded, clock)
            .map_err(|error| observation_error(&error))?;
        if final_observed.report().configured != report.configured
            || final_observed.report().guest != report.guest
        {
            return Err(observation_failure("template_composite_observation_drift"));
        }
        let final_config = self.observed_instance_config(executor)?;
        self.verify_limactl(executor)?;
        let realized_policy_matches = exact_realized_config_matches(
            &expected_config,
            [&first_config, &middle_config, &final_config],
        ) && final_config.runtime_state
            == report.configured.runtime_state;
        let (state, prepared_identity) = if !resources_match || !realized_policy_matches {
            (DisposableTemplateObservedState::Conflicting, None)
        } else {
            match report.configured.runtime_state {
                LimaRuntimeState::Running if ready => (
                    DisposableTemplateObservedState::ReadyRunning,
                    Some(self.prepared_template_identity.clone()),
                ),
                LimaRuntimeState::Stopped
                    if document.owned_object_identity().is_some()
                        && matches!(
                            document.phase(),
                            DisposableTemplateGenerationPhase::Verified
                                | DisposableTemplateGenerationPhase::StopAuthorized
                                | DisposableTemplateGenerationPhase::StopStarted
                                | DisposableTemplateGenerationPhase::Ready
                        ) =>
                {
                    (
                        DisposableTemplateObservedState::ReadyStopped,
                        Some(self.prepared_template_identity.clone()),
                    )
                }
                LimaRuntimeState::Running
                | LimaRuntimeState::Stopped
                | LimaRuntimeState::Uninitialized
                | LimaRuntimeState::Installing
                | LimaRuntimeState::Broken => {
                    (DisposableTemplateObservedState::OwnedIncomplete, None)
                }
            }
        };
        if matches!(report.guest, LimaGuestObservation::NotRunning { .. })
            && report.configured.runtime_state == LimaRuntimeState::Running
        {
            return Err(observation_failure("template_guest_evidence_inconsistent"));
        }
        host.confirm(&self.observation_request)
            .map_err(|_| observation_failure("template_host_identity_drift"))?;
        ensure_composite_observation_fresh(clock, composite_started)?;
        Ok(RuntimeObservation {
            sealed: runtime_disposable_template_observation(
                document,
                self.source_identity.clone(),
                Some(object_identity),
                prepared_identity,
                prior_operation(document, state),
                state,
            ),
            retained_host: Some(host),
        })
    }

    fn ready_probe(
        &self,
        executor: &impl TimedCommandExecutor,
    ) -> Result<bool, DisposableTemplateRuntimeError> {
        self.ready_probe_for(&self.source_instance, executor)
    }

    fn ready_probe_for(
        &self,
        instance: &LimaInstanceName,
        executor: &impl TimedCommandExecutor,
    ) -> Result<bool, DisposableTemplateRuntimeError> {
        let marker_digest = format!("{:x}", Sha256::digest(READY_MARKER_BYTES));
        let expected_marker = format!(
            "{marker_digest}  {}\n",
            self.prepared_template.ready_marker_path()
        );
        let marker = self
            .guest_command_for(instance, "/usr/bin/sha256sum")
            .argument(self.prepared_template.ready_marker_path());
        if !command_matches(executor, &marker, OBSERVATION_TIMEOUT, &expected_marker)? {
            return Ok(false);
        }
        let integrity_digest = format!("{:x}", Sha256::digest(RUNNER_INTEGRITY_BYTES));
        let expected_integrity = format!("{integrity_digest}  {RUNNER_INTEGRITY}\n");
        let integrity_file = self
            .guest_command_for(instance, "/usr/bin/sha256sum")
            .argument(RUNNER_INTEGRITY);
        if !command_matches(
            executor,
            &integrity_file,
            OBSERVATION_TIMEOUT,
            &expected_integrity,
        )? {
            return Ok(false);
        }
        let integrity = self
            .guest_command_for(instance, "/usr/bin/sudo")
            .argument("--non-interactive")
            .argument(RUNNER_INTEGRITY);
        if !command_matches(
            executor,
            &integrity,
            OBSERVATION_TIMEOUT,
            EXPECTED_RUNNER_INTEGRITY,
        )? {
            return Ok(false);
        }
        let launcher_digest = format!("{:x}", Sha256::digest(JIT_LAUNCHER_BYTES));
        let expected_launcher = format!("{launcher_digest}  {JIT_LAUNCHER}\n");
        let launcher = self
            .guest_command_for(instance, "/usr/bin/sha256sum")
            .argument(JIT_LAUNCHER);
        if !command_matches(executor, &launcher, OBSERVATION_TIMEOUT, &expected_launcher)? {
            return Ok(false);
        }
        let apt_policy_digest = format!("{:x}", Sha256::digest(APT_POLICY_BYTES));
        let expected_apt_policy = format!("{apt_policy_digest}  {APT_POLICY_PATH}\n");
        let apt_policy = self
            .guest_command_for(instance, "/usr/bin/sha256sum")
            .argument(APT_POLICY_PATH);
        if !command_matches(
            executor,
            &apt_policy,
            OBSERVATION_TIMEOUT,
            &expected_apt_policy,
        )? {
            return Ok(false);
        }
        for unit in MASKED_APT_UNITS {
            let masked = self
                .guest_command_for(instance, "/usr/bin/systemctl")
                .argument("is-enabled")
                .argument(unit);
            if !command_matches_status(
                executor,
                &masked,
                OBSERVATION_TIMEOUT,
                1,
                false,
                "masked\n",
            )? {
                return Ok(false);
            }
        }
        let groups = self
            .guest_command_for(instance, "/usr/bin/id")
            .argument("-Gn")
            .argument(RUNNER_USER);
        command_matches(
            executor,
            &groups,
            OBSERVATION_TIMEOUT,
            EXPECTED_RUNNER_GROUPS,
        )
    }

    fn verify_limactl(
        &self,
        executor: &impl TimedCommandExecutor,
    ) -> Result<(), DisposableTemplateRuntimeError> {
        let command = self.base_command().argument("--version");
        let expected = format!(
            "limactl version {}\n",
            self.prepared_template.lima_version()
        );
        if !command_matches(executor, &command, OBSERVATION_TIMEOUT, &expected)? {
            return Err(observation_failure("template_lima_version_mismatch"));
        }
        Ok(())
    }

    fn validated_template_config(
        &self,
        executor: &impl TimedCommandExecutor,
    ) -> Result<serde_json::Value, DisposableTemplateRuntimeError> {
        let command = self
            .base_command()
            .argument("--log-level=error")
            .argument("validate")
            .argument("--fill")
            .secret_argument(exact_path(&self.template_input_path));
        let output = exact_observation_output(executor, &command)?;
        let value = serde_yaml::from_str::<serde_json::Value>(&output)
            .map_err(|_| observation_failure("template_normalized_config_invalid"))?;
        if !value.is_object() {
            return Err(observation_failure("template_normalized_config_invalid"));
        }
        Ok(value)
    }

    #[allow(dead_code)] // Consumed through the pending production runner-service composition.
    fn validated_clone_config(
        &self,
        resources: DisposableWorkerResources,
        executor: &impl TimedCommandExecutor,
    ) -> Result<serde_json::Value, DisposableTemplateRuntimeError> {
        const GIB: u64 = 1 << 30;
        if !resources.cpu_millis().is_multiple_of(1_000)
            || !resources.memory_bytes().is_multiple_of(GIB)
            || !resources.disk_bytes().is_multiple_of(GIB)
        {
            return Err(observation_failure("template_clone_resources_invalid"));
        }
        let mut config = self.validated_template_config(executor)?;
        let object = config
            .as_object_mut()
            .ok_or_else(|| observation_failure("template_normalized_config_invalid"))?;
        object.insert(
            "cpus".to_owned(),
            serde_json::json!(resources.cpu_millis() / 1_000),
        );
        object.insert(
            "memory".to_owned(),
            serde_json::json!(format!("{}GiB", resources.memory_bytes() / GIB)),
        );
        object.insert(
            "disk".to_owned(),
            serde_json::json!(format!("{}GiB", resources.disk_bytes() / GIB)),
        );
        if !remove_canonical_empty_mounts(object) {
            return Err(observation_failure("template_clone_mount_policy_invalid"));
        }
        Ok(config)
    }

    fn observed_instance_config(
        &self,
        executor: &impl TimedCommandExecutor,
    ) -> Result<RealizedInstanceConfig, DisposableTemplateRuntimeError> {
        self.observed_instance_config_for(&self.source_instance, executor)
    }

    fn observed_instance_config_for(
        &self,
        instance: &LimaInstanceName,
        executor: &impl TimedCommandExecutor,
    ) -> Result<RealizedInstanceConfig, DisposableTemplateRuntimeError> {
        let command = self
            .base_command()
            .argument("list")
            .argument("--format=json")
            .argument("--all-fields")
            .argument(instance.as_str());
        let output = exact_observation_output(executor, &command)?;
        let line = output
            .strip_suffix('\n')
            .filter(|line| !line.is_empty() && !line.contains(['\n', '\r']))
            .ok_or_else(|| observation_failure("template_realized_config_invalid"))?;
        let mut value = serde_json::from_str::<serde_json::Value>(line)
            .map_err(|_| observation_failure("template_realized_config_invalid"))?;
        let object = value
            .as_object_mut()
            .ok_or_else(|| observation_failure("template_realized_config_invalid"))?;
        let name = object
            .get("name")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| observation_failure("template_realized_config_invalid"))?;
        if name != instance.as_str()
            || object
                .get("errors")
                .is_some_and(|errors| errors.as_array().is_none_or(|items| !items.is_empty()))
        {
            return Err(observation_failure("template_realized_config_invalid"));
        }
        let runtime_state = match object.get("status").and_then(serde_json::Value::as_str) {
            Some("Uninitialized") => LimaRuntimeState::Uninitialized,
            Some("Installing") => LimaRuntimeState::Installing,
            Some("Broken") => LimaRuntimeState::Broken,
            Some("Stopped") => LimaRuntimeState::Stopped,
            Some("Running") => LimaRuntimeState::Running,
            _ => return Err(observation_failure("template_realized_config_invalid")),
        };
        let config = object
            .remove("config")
            .filter(serde_json::Value::is_object)
            .ok_or_else(|| observation_failure("template_realized_config_missing"))?;
        Ok(RealizedInstanceConfig {
            runtime_state,
            config,
        })
    }

    fn execute(
        &self,
        kind: DisposableTemplateRuntimeCommandKind,
        document: &DisposableTemplateGenerationDocument,
        retained_host: Option<LimaHostIdentityObservation>,
        executor: &impl TimedCommandExecutor,
    ) -> Result<(), DisposableTemplateRuntimeError> {
        let expected_phase = match kind {
            DisposableTemplateRuntimeCommandKind::Create => {
                DisposableTemplateGenerationPhase::CreateStarted
            }
            DisposableTemplateRuntimeCommandKind::Stop => {
                DisposableTemplateGenerationPhase::StopStarted
            }
            DisposableTemplateRuntimeCommandKind::Discard => {
                DisposableTemplateGenerationPhase::DiscardStarted
            }
        };
        if document.phase() != expected_phase {
            return Err(runtime_error(
                DisposableTemplateRuntimeErrorKind::RecoveryRequired,
                "template_started_checkpoint_mismatch",
                "the durable Started checkpoint does not authorize the fixed Lima command",
            ));
        }
        match kind {
            DisposableTemplateRuntimeCommandKind::Create if retained_host.is_none() => {}
            DisposableTemplateRuntimeCommandKind::Stop
            | DisposableTemplateRuntimeCommandKind::Discard => retained_host
                .as_ref()
                .ok_or_else(|| observation_failure("template_host_identity_missing"))?
                .confirm(&self.observation_request)
                .map_err(|_| observation_failure("template_host_identity_drift"))?,
            DisposableTemplateRuntimeCommandKind::Create => {
                return Err(observation_failure("template_source_no_longer_absent"));
            }
        }
        self.verify_limactl(executor)?;
        let (command, timeout) = self.fixed_command(kind);
        let record = executor
            .execute_with_timeout(&command, timeout)
            .map_err(|_| command_failure())?;
        validate_mutation_record(&command, &record).map_err(|_| command_failure())?;
        Ok(())
    }

    fn fixed_command(&self, kind: DisposableTemplateRuntimeCommandKind) -> (CommandSpec, Duration) {
        match kind {
            DisposableTemplateRuntimeCommandKind::Create => (
                self.base_command()
                    .argument("start")
                    .argument("--name")
                    .argument(self.source_instance.as_str())
                    .argument("--timeout=10m")
                    .secret_argument(exact_path(&self.template_input_path)),
                CREATE_TIMEOUT,
            ),
            DisposableTemplateRuntimeCommandKind::Stop => (
                self.base_command()
                    .argument("stop")
                    .argument("--force")
                    .argument(self.source_instance.as_str()),
                STOP_TIMEOUT,
            ),
            DisposableTemplateRuntimeCommandKind::Discard => (
                self.base_command()
                    .argument("delete")
                    .argument("--force")
                    .argument(self.source_instance.as_str()),
                DISCARD_TIMEOUT,
            ),
        }
    }

    fn base_command(&self) -> CommandSpec {
        CommandSpec::new(&self.limactl_program)
            .argument("--tty=false")
            // Lima 2.2 uses macOS's user-cache directory while fetching the pinned image. Keep
            // that cache inside the already-private, source-identity-bound state root instead of
            // exposing the operator's real home or mixing cache entries into LIMA_HOME.
            .secret_environment("HOME", exact_path(&self.state_root))
            .secret_environment("LIMA_HOME", exact_path(&self.lima_home))
            .environment("LANG", "C")
            .environment("LC_ALL", "C")
            .environment("PATH", LIMACTL_SAFE_PATH)
    }

    fn guest_command_for(&self, instance: &LimaInstanceName, program: &str) -> CommandSpec {
        self.base_command()
            .argument("shell")
            .argument(instance.as_str())
            .argument("--")
            .argument(program)
    }
}

struct BoundedExecutor<'a, E> {
    executor: &'a E,
}

impl<E: TimedCommandExecutor> CommandExecutor for BoundedExecutor<'_, E> {
    fn execute(&self, spec: &CommandSpec) -> io::Result<ExecutionRecord> {
        self.executor
            .execute_with_timeout(spec, OBSERVATION_TIMEOUT)
    }
}

fn command_kind(
    disposition: DisposableTemplateGenerationDisposition,
) -> DisposableTemplateRuntimeCommandKind {
    match disposition {
        DisposableTemplateGenerationDisposition::CreateCandidate => {
            DisposableTemplateRuntimeCommandKind::Create
        }
        DisposableTemplateGenerationDisposition::StopCandidate => {
            DisposableTemplateRuntimeCommandKind::Stop
        }
        DisposableTemplateGenerationDisposition::DiscardCandidate => {
            DisposableTemplateRuntimeCommandKind::Discard
        }
        _ => unreachable!("command kind is requested only for runtime candidates"),
    }
}

fn prior_operation(
    document: &DisposableTemplateGenerationDocument,
    state: DisposableTemplateObservedState,
) -> DisposableTemplatePriorOperationState {
    use DisposableTemplateGenerationPhase as Phase;
    use DisposableTemplateObservedState as Observed;
    match (document.phase(), state) {
        (Phase::CreateStarted, Observed::ReadyRunning)
        | (Phase::StopStarted, Observed::ReadyStopped)
        | (Phase::DiscardStarted, Observed::Absent) => {
            DisposableTemplatePriorOperationState::Quiescent
        }
        (Phase::CreateStarted | Phase::StopStarted | Phase::DiscardStarted, _) => {
            DisposableTemplatePriorOperationState::InFlight
        }
        _ => DisposableTemplatePriorOperationState::NoPriorOperation,
    }
}

fn command_matches(
    executor: &impl TimedCommandExecutor,
    command: &CommandSpec,
    timeout: Duration,
    expected_stdout: &str,
) -> Result<bool, DisposableTemplateRuntimeError> {
    let record = executor
        .execute_with_timeout(command, timeout)
        .map_err(|_| observation_failure("template_readiness_command_failed"))?;
    Ok(validate_record(command, &record, expected_stdout).is_ok())
}

fn command_matches_status(
    executor: &impl TimedCommandExecutor,
    command: &CommandSpec,
    timeout: Duration,
    expected_status: i32,
    expected_success: bool,
    expected_stdout: &str,
) -> Result<bool, DisposableTemplateRuntimeError> {
    let record = executor
        .execute_with_timeout(command, timeout)
        .map_err(|_| observation_failure("template_readiness_command_failed"))?;
    Ok(record.argv == command.displayed_argv()
        && record.environment_keys == command.environment.keys().cloned().collect::<Vec<_>>()
        && record.status == Some(expected_status)
        && record.success == expected_success
        && record.stdout == expected_stdout
        && record.stderr.is_empty())
}

fn exact_observation_output(
    executor: &impl TimedCommandExecutor,
    command: &CommandSpec,
) -> Result<String, DisposableTemplateRuntimeError> {
    let record = executor
        .execute_with_timeout(command, OBSERVATION_TIMEOUT)
        .map_err(|_| observation_failure("template_configuration_command_failed"))?;
    if record.argv != command.displayed_argv()
        || record.environment_keys != command.environment.keys().cloned().collect::<Vec<_>>()
        || record.status != Some(0)
        || !record.success
        || !record.stderr.is_empty()
    {
        return Err(observation_failure("template_configuration_command_failed"));
    }
    Ok(record.stdout)
}

fn exact_realized_config_matches(
    expected: &serde_json::Value,
    observed: [&RealizedInstanceConfig; 3],
) -> bool {
    expected.is_object()
        && observed
            .iter()
            .all(|observation| observation.config == *expected)
        && observed
            .windows(2)
            .all(|pair| pair[0].runtime_state == pair[1].runtime_state)
}

fn remove_canonical_empty_mounts(object: &mut serde_json::Map<String, serde_json::Value>) -> bool {
    match object.remove("mounts") {
        None => true,
        Some(serde_json::Value::Array(mounts)) => mounts.is_empty(),
        Some(_) => false,
    }
}

fn ensure_composite_observation_fresh(
    clock: &impl LimaObservationClock,
    started_at: u64,
) -> Result<(), DisposableTemplateRuntimeError> {
    let completed_at = clock
        .unix_seconds()
        .map_err(|_| observation_failure("template_observation_clock_failed"))?;
    let duration = completed_at
        .checked_sub(started_at)
        .ok_or_else(|| observation_failure("template_observation_clock_failed"))?;
    if duration > OBSERVATION_MAX_AGE_SECONDS {
        return Err(observation_failure("template_composite_observation_stale"));
    }
    Ok(())
}

fn validate_record(
    command: &CommandSpec,
    record: &ExecutionRecord,
    expected_stdout: &str,
) -> Result<(), ()> {
    if record.argv != command.displayed_argv()
        || record.environment_keys != command.environment.keys().cloned().collect::<Vec<_>>()
        || record.status != Some(0)
        || !record.success
        || record.stdout != expected_stdout
        || !record.stderr.is_empty()
    {
        return Err(());
    }
    Ok(())
}

fn validate_mutation_record(command: &CommandSpec, record: &ExecutionRecord) -> Result<(), ()> {
    if record.argv != command.displayed_argv()
        || record.environment_keys != command.environment.keys().cloned().collect::<Vec<_>>()
        || record.status != Some(0)
        || !record.success
    {
        return Err(());
    }
    Ok(())
}

fn receipt(
    document: &DisposableTemplateGenerationDocument,
    disposition: DisposableTemplateRuntimeDisposition,
) -> DisposableTemplateRuntimeReceipt {
    DisposableTemplateRuntimeReceipt {
        schema_version: DISPOSABLE_TEMPLATE_RUNTIME_SCHEMA_VERSION,
        generation_id: document.generation_id().as_str().to_owned(),
        revision: document.revision(),
        phase: document.phase(),
        disposition,
    }
}

fn derive_source_identity(
    instance: &LimaInstanceName,
    state_root: &Path,
    lima_home: &Path,
    limactl_program: &Path,
    lima_version: &str,
) -> Result<DisposableTemplateSourceIdentity, DisposableTemplateRuntimeError> {
    let client_home = exact_path(state_root);
    let home = exact_path(lima_home);
    let program = exact_path(limactl_program);
    let mut hasher = Sha256::new();
    hasher.update(SOURCE_IDENTITY_DOMAIN);
    hash_field(&mut hasher, instance.as_str().as_bytes());
    hash_field(&mut hasher, client_home.as_bytes());
    hash_field(&mut hasher, home.as_bytes());
    hash_field(&mut hasher, program.as_bytes());
    hash_field(&mut hasher, lima_version.as_bytes());
    let digest = Sha256Digest::parse(&format!("sha256:{:x}", hasher.finalize()))
        .map_err(|_| invalid_configuration())?;
    Ok(DisposableTemplateSourceIdentity::from_runtime_digest(
        digest,
    ))
}

fn derive_generation_id(
    source: &DisposableTemplateSourceIdentity,
    prepared: &DisposablePreparedTemplateIdentity,
    instance: &LimaInstanceName,
) -> Result<DisposableTemplateGenerationId, DisposableTemplateRuntimeError> {
    let mut hasher = Sha256::new();
    hasher.update(GENERATION_ID_DOMAIN);
    hash_field(&mut hasher, source.as_str().as_bytes());
    hash_field(&mut hasher, prepared.as_str().as_bytes());
    hash_field(&mut hasher, instance.as_str().as_bytes());
    DisposableTemplateGenerationId::parse(&format!("{:x}", hasher.finalize()))
        .map_err(|_| invalid_configuration())
}

fn hash_field(hasher: &mut Sha256, value: &[u8]) {
    hasher.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
    hasher.update(value);
}

fn validate_private_path(path: PathBuf) -> Result<PathBuf, DisposableTemplateRuntimeError> {
    let Some(text) = path.to_str() else {
        return Err(invalid_configuration());
    };
    if !path.is_absolute()
        || path == Path::new("/")
        || text.len() > MAX_PRIVATE_PATH_BYTES
        || text.bytes().any(|byte| byte.is_ascii_control())
        || text.contains("//")
        || text.ends_with('/')
        || text
            .split('/')
            .any(|component| matches!(component, "." | ".."))
    {
        return Err(invalid_configuration());
    }
    Ok(path)
}

fn exact_path(path: &Path) -> String {
    path.to_str()
        .expect("validated private runtime path remains exact UTF-8")
        .to_owned()
}

fn observation_error(error: &LimaObservationFailure) -> DisposableTemplateRuntimeError {
    let code = match error.code {
        LimaObservationRefusalCode::MissingInstanceEvidence => "template_source_missing",
        LimaObservationRefusalCode::StaleObservation => "template_observation_stale",
        LimaObservationRefusalCode::InstanceDrift => "template_instance_drift",
        _ => "template_observation_failed",
    };
    observation_failure(code)
}

fn invalid_configuration() -> DisposableTemplateRuntimeError {
    runtime_error(
        DisposableTemplateRuntimeErrorKind::InvalidConfiguration,
        "template_runtime_invalid_configuration",
        "the disposable-template runtime configuration is invalid",
    )
}

fn observation_failure(code: &'static str) -> DisposableTemplateRuntimeError {
    runtime_error(
        DisposableTemplateRuntimeErrorKind::Observation,
        code,
        "the disposable-template source observation could not be established",
    )
}

fn command_failure() -> DisposableTemplateRuntimeError {
    runtime_error(
        DisposableTemplateRuntimeErrorKind::RecoveryRequired,
        "template_command_outcome_unknown",
        "the bounded Lima command failed after its durable Started checkpoint",
    )
}

fn runtime_error(
    kind: DisposableTemplateRuntimeErrorKind,
    code: &'static str,
    message: &'static str,
) -> DisposableTemplateRuntimeError {
    DisposableTemplateRuntimeError {
        kind,
        code,
        message,
    }
}

impl DisposableTemplateRuntimeError {
    fn from_store(error: PersonalWorkerStoreError) -> Self {
        let kind = match error.kind() {
            PersonalWorkerStoreErrorKind::RevisionConflict => {
                DisposableTemplateRuntimeErrorKind::RecoveryRequired
            }
            PersonalWorkerStoreErrorKind::Busy
            | PersonalWorkerStoreErrorKind::InvalidDocument
            | PersonalWorkerStoreErrorKind::Missing
            | PersonalWorkerStoreErrorKind::Io
            | PersonalWorkerStoreErrorKind::UnsafeFilesystem
            | PersonalWorkerStoreErrorKind::VersionIncompatible
            | PersonalWorkerStoreErrorKind::CorruptState => {
                DisposableTemplateRuntimeErrorKind::DurableState
            }
        };
        runtime_error(
            kind,
            "template_durable_state_unavailable",
            "the disposable-template durable state could not be opened or advanced",
        )
    }
}
