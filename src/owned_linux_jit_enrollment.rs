//! Canonical, secret-free enrollment for one native owned-Linux GitHub Actions JIT worker.
//!
//! The document binds one repository, one reviewed Scale Set/label set, one fixed runner payload
//! generation, and one controller-only GitHub App key file identity. Secret bytes never enter the
//! enrollment or durable attempt catalog.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::artifact::Sha256Digest;
use crate::disposable_host_storage::DisposableHostStorage;
use crate::disposable_runner_runtime::DisposableRunnerRuntime;
use crate::github_scale_set_bridge::{
    GitHubAppKeychainConfig, ScaleSetBridgeConfig, ScaleSetBridgeTarget,
};
use crate::github_scale_set_delivery_consumer::ScaleSetDeliveryConsumerPolicy;
use crate::owned_linux_jit_runtime::OwnedLinuxJitRuntime;

pub const OWNED_LINUX_JIT_ENROLLMENT_SCHEMA_VERSION: u8 = 1;
pub const MAX_OWNED_LINUX_JIT_ENROLLMENT_BYTES: usize = 16 * 1024;
const BRIDGE_PROGRAM: &str = "/opt/smolrunner/bin/scaleset-bridge";
const SHARED_ADMISSION_RELATIVE: &str = ".local/state/glaeda/direct-owned-admission-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OwnedLinuxJitEnrollmentErrorKind {
    InvalidDocument,
    VersionIncompatible,
    NonCanonical,
    InvalidConfiguration,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
pub struct OwnedLinuxJitEnrollmentError {
    kind: OwnedLinuxJitEnrollmentErrorKind,
    code: &'static str,
    message: &'static str,
}

impl OwnedLinuxJitEnrollmentError {
    #[must_use]
    pub const fn kind(self) -> OwnedLinuxJitEnrollmentErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for OwnedLinuxJitEnrollmentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OwnedLinuxJitEnrollmentError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .finish()
    }
}

impl fmt::Display for OwnedLinuxJitEnrollmentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for OwnedLinuxJitEnrollmentError {}

pub struct OwnedLinuxJitEnrollment {
    state_root: PathBuf,
    bridge_config: ScaleSetBridgeConfig,
    private_key_path: PathBuf,
    private_key_digest: Sha256Digest,
    consumer_policy: ScaleSetDeliveryConsumerPolicy,
    host_storage: DisposableHostStorage,
    runtime: OwnedLinuxJitRuntime,
    runner_runtime: DisposableRunnerRuntime,
}

impl fmt::Debug for OwnedLinuxJitEnrollment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OwnedLinuxJitEnrollment")
            .field("state_root", &"<private-state-root>")
            .field("bridge", &"<enrolled-scale-set>")
            .field("credential", &"<private-controller-file>")
            .field("policy", &self.consumer_policy)
            .finish()
    }
}

pub(crate) struct OwnedLinuxJitEnrollmentParts {
    pub(crate) state_root: PathBuf,
    pub(crate) bridge_config: ScaleSetBridgeConfig,
    pub(crate) private_key_path: PathBuf,
    pub(crate) private_key_digest: Sha256Digest,
    pub(crate) consumer_policy: ScaleSetDeliveryConsumerPolicy,
    pub(crate) host_storage: DisposableHostStorage,
    pub(crate) runtime: OwnedLinuxJitRuntime,
    pub(crate) runner_runtime: DisposableRunnerRuntime,
}

impl OwnedLinuxJitEnrollment {
    pub(crate) fn into_parts(self) -> OwnedLinuxJitEnrollmentParts {
        OwnedLinuxJitEnrollmentParts {
            state_root: self.state_root,
            bridge_config: self.bridge_config,
            private_key_path: self.private_key_path,
            private_key_digest: self.private_key_digest,
            consumer_policy: self.consumer_policy,
            host_storage: self.host_storage,
            runtime: self.runtime,
            runner_runtime: self.runner_runtime,
        }
    }
}

pub fn decode_owned_linux_jit_enrollment(
    bytes: &[u8],
) -> Result<OwnedLinuxJitEnrollment, OwnedLinuxJitEnrollmentError> {
    if bytes.len() > MAX_OWNED_LINUX_JIT_ENROLLMENT_BYTES {
        return Err(enrollment_error(
            OwnedLinuxJitEnrollmentErrorKind::InvalidDocument,
            "owned_linux_jit_enrollment_too_large",
            "the owned-Linux JIT enrollment exceeds the reviewed byte limit",
        ));
    }
    let version: VersionWire = serde_json::from_slice(bytes).map_err(|_| invalid_document())?;
    if version.schema_version != OWNED_LINUX_JIT_ENROLLMENT_SCHEMA_VERSION {
        return Err(enrollment_error(
            OwnedLinuxJitEnrollmentErrorKind::VersionIncompatible,
            "owned_linux_jit_enrollment_version_incompatible",
            "the owned-Linux JIT enrollment schema version is unsupported",
        ));
    }
    let wire: EnrollmentWire = serde_json::from_slice(bytes).map_err(|_| invalid_document())?;
    if canonical_bytes(&wire)? != bytes {
        return Err(enrollment_error(
            OwnedLinuxJitEnrollmentErrorKind::NonCanonical,
            "owned_linux_jit_enrollment_noncanonical",
            "the owned-Linux JIT enrollment is not canonically encoded",
        ));
    }
    build_enrollment(wire)
}

fn build_enrollment(
    wire: EnrollmentWire,
) -> Result<OwnedLinuxJitEnrollment, OwnedLinuxJitEnrollmentError> {
    // Initial trust class is one exact reviewed label/Scale Set for one exact repository.
    if wire.scale_set.labels.len() != 1
        || wire.scale_set.labels[0].is_empty()
        || wire.scale_set.owner.is_empty()
        || wire.scale_set.repository.is_empty()
    {
        return Err(invalid_configuration());
    }

    let state_root = PathBuf::from(&wire.state_root);
    let bridge_digest =
        Sha256Digest::parse(&wire.bridge.program_digest).map_err(|_| invalid_configuration())?;
    let helper_digest = Sha256Digest::parse(&wire.owned_linux.helper_sha256)
        .map_err(|_| invalid_configuration())?;
    let payload_tree_digest = Sha256Digest::parse(&wire.owned_linux.payload_tree_sha256)
        .map_err(|_| invalid_configuration())?;
    let egress_guard_digest = Sha256Digest::parse(&wire.owned_linux.egress_guard_sha256)
        .map_err(|_| invalid_configuration())?;
    let private_key_digest = Sha256Digest::parse(&wire.github.private_key_sha256)
        .map_err(|_| invalid_configuration())?;
    let github_app = GitHubAppKeychainConfig::new_without_keychain(
        &wire.github.config_url,
        &wire.github.client_id,
        wire.github.installation_id,
    )
    .map_err(|_| invalid_configuration())?;
    let target = ScaleSetBridgeTarget::new(
        wire.scale_set.id,
        &wire.scale_set.name,
        wire.scale_set.runner_group_id,
        &wire.scale_set.labels,
        &wire.scale_set.owner,
        1,
    )
    .map_err(|_| invalid_configuration())?;
    let bridge_config =
        ScaleSetBridgeConfig::new(Path::new(BRIDGE_PROGRAM), bridge_digest, github_app, target)
            .map_err(|_| invalid_configuration())?;

    let runtime = OwnedLinuxJitRuntime::new(
        PathBuf::from(&wire.owned_linux.helper),
        helper_digest,
        shared_owned_linux_admission_root()?,
        PathBuf::from(&wire.owned_linux.task_root),
        PathBuf::from(&wire.owned_linux.payload_root),
        payload_tree_digest,
        PathBuf::from(&wire.owned_linux.launcher),
        PathBuf::from(&wire.owned_linux.egress_guard),
        egress_guard_digest,
    )
    .map_err(|_| invalid_configuration())?;
    let resources = OwnedLinuxJitRuntime::fixed_resources().map_err(|_| invalid_configuration())?;
    let generation = runtime
        .generation_identity()
        .map_err(|_| invalid_configuration())?;
    let consumer_policy = ScaleSetDeliveryConsumerPolicy::new_owned_linux(
        wire.scale_set.id,
        &wire.scale_set.repository,
        &wire.scale_set.owner,
        &wire.scale_set.labels,
        resources,
        generation,
    )
    .map_err(|_| invalid_configuration())?;
    let host_storage = DisposableHostStorage::new(
        PathBuf::from(&wire.owned_linux.task_root),
        resources.disk_bytes(),
    )
    .map_err(|_| invalid_configuration())?;
    let runner_runtime = DisposableRunnerRuntime::new_owned_linux(runtime.clone());

    Ok(OwnedLinuxJitEnrollment {
        state_root,
        bridge_config,
        private_key_path: PathBuf::from(wire.github.private_key_path),
        private_key_digest,
        consumer_policy,
        host_storage,
        runtime,
        runner_runtime,
    })
}

fn shared_owned_linux_admission_root() -> Result<PathBuf, OwnedLinuxJitEnrollmentError> {
    let home = std::env::var_os("HOME").ok_or_else(invalid_configuration)?;
    let root = PathBuf::from(home).join(SHARED_ADMISSION_RELATIVE);
    if !root.is_absolute() || root == Path::new("/") {
        return Err(invalid_configuration());
    }
    Ok(root)
}

fn canonical_bytes(wire: &EnrollmentWire) -> Result<Vec<u8>, OwnedLinuxJitEnrollmentError> {
    let mut bytes = serde_json::to_vec_pretty(wire).map_err(|_| invalid_document())?;
    bytes.push(b'\n');
    Ok(bytes)
}

const fn invalid_document() -> OwnedLinuxJitEnrollmentError {
    enrollment_error(
        OwnedLinuxJitEnrollmentErrorKind::InvalidDocument,
        "owned_linux_jit_enrollment_invalid",
        "the owned-Linux JIT enrollment document is invalid",
    )
}

const fn invalid_configuration() -> OwnedLinuxJitEnrollmentError {
    enrollment_error(
        OwnedLinuxJitEnrollmentErrorKind::InvalidConfiguration,
        "owned_linux_jit_enrollment_configuration_invalid",
        "the owned-Linux JIT enrollment configuration is invalid",
    )
}

const fn enrollment_error(
    kind: OwnedLinuxJitEnrollmentErrorKind,
    code: &'static str,
    message: &'static str,
) -> OwnedLinuxJitEnrollmentError {
    OwnedLinuxJitEnrollmentError {
        kind,
        code,
        message,
    }
}

#[derive(Deserialize)]
struct VersionWire {
    schema_version: u8,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct EnrollmentWire {
    schema_version: u8,
    state_root: String,
    bridge: BridgeWire,
    github: GitHubWire,
    scale_set: ScaleSetWire,
    owned_linux: OwnedLinuxWire,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct BridgeWire {
    program_digest: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct GitHubWire {
    config_url: String,
    client_id: String,
    installation_id: u64,
    private_key_path: String,
    private_key_sha256: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ScaleSetWire {
    id: u32,
    name: String,
    runner_group_id: u32,
    owner: String,
    repository: String,
    labels: Vec<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct OwnedLinuxWire {
    helper: String,
    helper_sha256: String,
    task_root: String,
    payload_root: String,
    payload_tree_sha256: String,
    launcher: String,
    egress_guard: String,
    egress_guard_sha256: String,
}
