//! Canonical identity for the trusted inputs used to build one disposable Lima/VZ template.
//!
//! This module does not download, provision, inspect, or mutate a VM. It gives the future template
//! builder and live mutation boundary one closed identity that changes whenever the admitted guest
//! image, official Actions runner archive, provisioning recipe, or isolation policy changes.

use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::artifact::Sha256Digest;

pub const DISPOSABLE_PREPARED_TEMPLATE_SCHEMA_VERSION: u8 = 3;
pub const MAX_DISPOSABLE_PREPARED_TEMPLATE_BYTES: usize = 16_384;
pub const MAX_DISPOSABLE_LIMA_TEMPLATE_BYTES: usize = 64 * 1_024;
const IDENTITY_DOMAIN: &[u8] = b"smolrunner.disposable-prepared-template.v3\0";
const CURRENT_MANIFEST_BYTES: &[u8] =
    include_bytes!("../examples/lima/glaeda-prepared-template.json");
const CURRENT_LIMA_TEMPLATE_BYTES: &[u8] =
    include_bytes!("../examples/lima/glaeda-prepared-template.yaml");
const MAX_DOWNLOAD_LOCATION_BYTES: usize = 512;
const MAX_RUNNER_ARCHIVE_BYTES: u64 = 1 << 30;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct DisposablePreparedTemplateIdentity(Sha256Digest);

impl DisposablePreparedTemplateIdentity {
    /// Parse one durable prepared-template identity.
    ///
    /// # Errors
    ///
    /// Returns an error unless `value` is a canonical SHA-256 digest.
    pub(crate) fn parse(value: &str) -> Result<Self, DisposablePreparedTemplateError> {
        Sha256Digest::parse(value).map(Self).map_err(|_| {
            template_error(
                DisposablePreparedTemplateErrorKind::InvalidDocument,
                "prepared-template identity is not a canonical SHA-256 digest",
            )
        })
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    #[must_use]
    pub const fn digest(&self) -> &Sha256Digest {
        &self.0
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct DisposablePreparedTemplateManifest {
    wire: PreparedTemplateWire,
    guest_image_digest: Sha256Digest,
    actions_runner_digest: Sha256Digest,
    lima_template_digest: Sha256Digest,
}

impl fmt::Debug for DisposablePreparedTemplateManifest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DisposablePreparedTemplateManifest")
            .field("schema_version", &self.wire.schema_version)
            .field("guest_architecture", &self.wire.guest_image.architecture)
            .field("actions_runner_version", &self.wire.actions_runner.version)
            .field(
                "provisioning_recipe_revision",
                &self.wire.provisioning.recipe_revision,
            )
            .field("isolation", &"<fixed-hostile-ci-policy>")
            .finish()
    }
}

impl DisposablePreparedTemplateManifest {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.wire.schema_version
    }

    #[must_use]
    pub fn guest_image_location(&self) -> &str {
        &self.wire.guest_image.location
    }

    #[must_use]
    pub const fn guest_image_digest(&self) -> &Sha256Digest {
        &self.guest_image_digest
    }

    #[must_use]
    pub fn guest_architecture(&self) -> &str {
        &self.wire.guest_image.architecture
    }

    #[must_use]
    pub fn actions_runner_version(&self) -> &str {
        &self.wire.actions_runner.version
    }

    #[must_use]
    pub fn actions_runner_location(&self) -> &str {
        &self.wire.actions_runner.location
    }

    #[must_use]
    pub const fn actions_runner_digest(&self) -> &Sha256Digest {
        &self.actions_runner_digest
    }

    #[must_use]
    pub const fn actions_runner_archive_bytes(&self) -> u64 {
        self.wire.actions_runner.archive_bytes
    }

    #[must_use]
    pub const fn source_cpu_count(&self) -> u32 {
        self.wire.source_resources.cpu_count
    }

    #[must_use]
    pub const fn source_memory_bytes(&self) -> u64 {
        self.wire.source_resources.memory_bytes
    }

    #[must_use]
    pub const fn source_disk_bytes(&self) -> u64 {
        self.wire.source_resources.disk_bytes
    }

    #[must_use]
    pub const fn provisioning_recipe_revision(&self) -> u64 {
        self.wire.provisioning.recipe_revision
    }

    #[must_use]
    pub const fn lima_template_digest(&self) -> &Sha256Digest {
        &self.lima_template_digest
    }

    #[must_use]
    pub fn ready_marker_path(&self) -> &str {
        &self.wire.provisioning.ready_marker_path
    }

    /// Check that one Lima template is the exact controller-owned construction input.
    ///
    /// This deliberately binds bytes rather than attempting to reimplement Lima or cloud-init
    /// semantics. Lima owns parsing and provisioning after this boundary.
    ///
    /// # Errors
    ///
    /// Returns a bounded refusal when the input is oversized or differs from the manifest digest.
    pub fn validate_lima_template(
        &self,
        bytes: &[u8],
    ) -> Result<(), DisposablePreparedTemplateError> {
        if bytes.len() > MAX_DISPOSABLE_LIMA_TEMPLATE_BYTES
            || digest_bytes(bytes)? != self.lima_template_digest
        {
            return Err(unsafe_policy());
        }
        Ok(())
    }

    #[must_use]
    pub fn lima_version(&self) -> &str {
        &self.wire.isolation.lima_version
    }

    #[must_use]
    pub fn vm_type(&self) -> &str {
        &self.wire.isolation.vm_type
    }

    /// Derive the domain-separated identity of every canonical manifest field.
    ///
    /// # Errors
    ///
    /// Returns an error only if the already-validated manifest cannot be canonically encoded.
    pub fn identity(
        &self,
    ) -> Result<DisposablePreparedTemplateIdentity, DisposablePreparedTemplateError> {
        let canonical = encode_disposable_prepared_template(self)?;
        let mut hasher = Sha256::new();
        hasher.update(IDENTITY_DOMAIN);
        hasher.update(
            u64::try_from(canonical.len())
                .unwrap_or(u64::MAX)
                .to_be_bytes(),
        );
        hasher.update(canonical);
        DisposablePreparedTemplateIdentity::parse(&format!("sha256:{:x}", hasher.finalize()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisposablePreparedTemplateErrorKind {
    VersionIncompatible,
    InvalidDocument,
    UnsafePolicy,
    NonCanonical,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct DisposablePreparedTemplateError {
    kind: DisposablePreparedTemplateErrorKind,
    code: &'static str,
    message: &'static str,
}

impl DisposablePreparedTemplateError {
    #[must_use]
    pub const fn kind(&self) -> DisposablePreparedTemplateErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for DisposablePreparedTemplateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DisposablePreparedTemplateError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for DisposablePreparedTemplateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for DisposablePreparedTemplateError {}

#[derive(Deserialize)]
struct VersionWire {
    schema_version: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PreparedTemplateWire {
    schema_version: u8,
    guest_image: GuestImageWire,
    actions_runner: ActionsRunnerWire,
    source_resources: SourceResourcesWire,
    provisioning: ProvisioningWire,
    isolation: IsolationWire,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GuestImageWire {
    location: String,
    architecture: String,
    variant: String,
    digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActionsRunnerWire {
    version: String,
    location: String,
    architecture: String,
    archive_bytes: u64,
    digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceResourcesWire {
    cpu_count: u32,
    memory_bytes: u64,
    disk_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProvisioningWire {
    recipe_revision: u64,
    lima_template_digest: String,
    admin_user: String,
    admin_uid: u32,
    admin_comment: String,
    admin_passwordless_sudo: bool,
    workload_user: String,
    runner_install_directory: String,
    runner_work_directory: String,
    jit_launcher_path: String,
    jit_launcher_digest: String,
    ready_marker_path: String,
    runner_dependency_install: String,
    os_package_source: String,
    automatic_os_updates_after_readiness: bool,
    runner_auto_update: bool,
    workload_sudo: bool,
    repository_controlled_input: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct IsolationWire {
    lima_version: String,
    vm_type: String,
    plain_mode: bool,
    lima_global_overrides: bool,
    host_mounts: bool,
    additional_networks: bool,
    port_forwards: bool,
    host_resolver: bool,
    dns_servers: Vec<String>,
    ssh_over_vsock: bool,
    ssh_load_dot_public_keys: bool,
    ssh_agent_forwarding: bool,
    ssh_forward_x11: bool,
    ssh_forward_x11_trusted: bool,
    proxy_environment: bool,
    built_in_containerd: bool,
    rosetta: bool,
    display: bool,
}

/// Load the single checked-in production prepared-template declaration.
///
/// # Errors
///
/// Returns an error if the checked-in declaration is invalid, unsafe, or noncanonical.
pub fn current_disposable_prepared_template()
-> Result<DisposablePreparedTemplateManifest, DisposablePreparedTemplateError> {
    let manifest = decode_disposable_prepared_template(CURRENT_MANIFEST_BYTES)?;
    manifest.validate_lima_template(CURRENT_LIMA_TEMPLATE_BYTES)?;
    Ok(manifest)
}

/// Return the exact checked-in Lima input whose digest is bound by the current manifest.
#[must_use]
pub const fn current_disposable_lima_template_bytes() -> &'static [u8] {
    CURRENT_LIMA_TEMPLATE_BYTES
}

/// Strictly decode one canonical prepared-template declaration.
///
/// # Errors
///
/// Returns a bounded error for oversize, invalid, unsafe, noncanonical, or unsupported input.
pub fn decode_disposable_prepared_template(
    bytes: &[u8],
) -> Result<DisposablePreparedTemplateManifest, DisposablePreparedTemplateError> {
    if bytes.len() > MAX_DISPOSABLE_PREPARED_TEMPLATE_BYTES {
        return Err(template_error(
            DisposablePreparedTemplateErrorKind::InvalidDocument,
            "prepared-template declaration exceeds the reviewed byte limit",
        ));
    }
    let version: VersionWire = serde_json::from_slice(bytes).map_err(|_| invalid_document())?;
    if version.schema_version != DISPOSABLE_PREPARED_TEMPLATE_SCHEMA_VERSION {
        return Err(template_error(
            DisposablePreparedTemplateErrorKind::VersionIncompatible,
            "prepared-template schema version is unsupported",
        ));
    }
    let wire: PreparedTemplateWire =
        serde_json::from_slice(bytes).map_err(|_| invalid_document())?;
    let (guest_image_digest, actions_runner_digest, lima_template_digest) = validate_wire(&wire)?;
    let manifest = DisposablePreparedTemplateManifest {
        wire,
        guest_image_digest,
        actions_runner_digest,
        lima_template_digest,
    };
    if encode_disposable_prepared_template(&manifest)? != bytes {
        return Err(template_error(
            DisposablePreparedTemplateErrorKind::NonCanonical,
            "prepared-template declaration is not canonically encoded",
        ));
    }
    Ok(manifest)
}

/// Encode one prepared-template declaration into its unique durable JSON representation.
///
/// # Errors
///
/// Returns an error if serialization unexpectedly fails.
pub fn encode_disposable_prepared_template(
    manifest: &DisposablePreparedTemplateManifest,
) -> Result<Vec<u8>, DisposablePreparedTemplateError> {
    let mut bytes = serde_json::to_vec_pretty(&manifest.wire).map_err(|_| invalid_document())?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn validate_wire(
    wire: &PreparedTemplateWire,
) -> Result<(Sha256Digest, Sha256Digest, Sha256Digest), DisposablePreparedTemplateError> {
    let guest_image_digest =
        Sha256Digest::parse(&wire.guest_image.digest).map_err(|_| invalid_document())?;
    let actions_runner_digest =
        Sha256Digest::parse(&wire.actions_runner.digest).map_err(|_| invalid_document())?;
    let lima_template_digest = Sha256Digest::parse(&wire.provisioning.lima_template_digest)
        .map_err(|_| invalid_document())?;
    Sha256Digest::parse(&wire.provisioning.jit_launcher_digest).map_err(|_| invalid_document())?;
    if wire.guest_image.architecture != "aarch64"
        || wire.guest_image.variant != "server"
        || !valid_noble_arm64_image_location(&wire.guest_image.location)
    {
        return Err(unsafe_policy());
    }
    if !valid_runner_version(&wire.actions_runner.version)
        || wire.actions_runner.architecture != "arm64"
        || wire.actions_runner.archive_bytes == 0
        || wire.actions_runner.archive_bytes > MAX_RUNNER_ARCHIVE_BYTES
        || wire.actions_runner.location != expected_runner_location(&wire.actions_runner.version)
    {
        return Err(unsafe_policy());
    }
    if wire.source_resources.cpu_count != 2
        || wire.source_resources.memory_bytes != 2 * (1 << 30)
        || wire.source_resources.disk_bytes != 20 * (1 << 30)
    {
        return Err(unsafe_policy());
    }
    let provisioning = &wire.provisioning;
    if provisioning.recipe_revision == 0
        || provisioning.admin_user != "smolrunner-admin"
        || provisioning.admin_uid != 1000
        || provisioning.admin_comment != "SmolRunner controller"
        || !provisioning.admin_passwordless_sudo
        || provisioning.workload_user != "smolrunner-runner"
        || provisioning.admin_user == provisioning.workload_user
        || provisioning.runner_install_directory != "/opt/smolrunner/actions-runner"
        || provisioning.runner_work_directory != "/opt/smolrunner/actions-runner/_work"
        || provisioning.jit_launcher_path != "/opt/smolrunner/bin/smolrunner-jit-launcher"
        || provisioning.jit_launcher_digest
            != "sha256:6f096fb518b6d40d45ca1a9923e423da436b6269fceec5a5989566abbc93a76a"
        || provisioning.ready_marker_path != "/etc/smolrunner/prepared-template.json"
        || provisioning.runner_dependency_install != "official_archive_script"
        || provisioning.os_package_source != "ubuntu_noble_signed_repositories_at_build"
        || provisioning.automatic_os_updates_after_readiness
        || provisioning.runner_auto_update
        || provisioning.workload_sudo
        || provisioning.repository_controlled_input
    {
        return Err(unsafe_policy());
    }
    let isolation = &wire.isolation;
    if isolation.lima_version != "2.2.0"
        || isolation.vm_type != "vz"
        || !isolation.plain_mode
        || isolation.lima_global_overrides
        || isolation.host_mounts
        || isolation.additional_networks
        || isolation.port_forwards
        || isolation.host_resolver
        || isolation.dns_servers != ["1.1.1.1", "1.0.0.1"]
        || !isolation.ssh_over_vsock
        || isolation.ssh_load_dot_public_keys
        || isolation.ssh_agent_forwarding
        || isolation.ssh_forward_x11
        || isolation.ssh_forward_x11_trusted
        || isolation.proxy_environment
        || isolation.built_in_containerd
        || isolation.rosetta
        || isolation.display
    {
        return Err(unsafe_policy());
    }
    Ok((
        guest_image_digest,
        actions_runner_digest,
        lima_template_digest,
    ))
}

fn digest_bytes(bytes: &[u8]) -> Result<Sha256Digest, DisposablePreparedTemplateError> {
    Sha256Digest::parse(&format!("sha256:{:x}", Sha256::digest(bytes)))
        .map_err(|_| invalid_document())
}

fn valid_noble_arm64_image_location(value: &str) -> bool {
    const PREFIX: &str = "https://cloud-images.ubuntu.com/releases/noble/release-";
    const SUFFIX: &str = "/ubuntu-24.04-server-cloudimg-arm64.img";
    valid_https_location(value)
        && value
            .strip_prefix(PREFIX)
            .and_then(|suffix| suffix.strip_suffix(SUFFIX))
            .is_some_and(|date| date.len() == 8 && date.bytes().all(|byte| byte.is_ascii_digit()))
}

fn valid_https_location(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_DOWNLOAD_LOCATION_BYTES
        && value.is_ascii()
        && value.starts_with("https://")
        && !value.contains(['?', '#', '@', '\\'])
        && !value.bytes().any(|byte| byte.is_ascii_control())
}

fn valid_runner_version(value: &str) -> bool {
    let mut parts = value.split('.');
    let valid = (0..3).all(|_| {
        parts.next().is_some_and(|part| {
            !part.is_empty()
                && part.len() <= 6
                && part.bytes().all(|byte| byte.is_ascii_digit())
                && (part == "0" || !part.starts_with('0'))
        })
    });
    valid && parts.next().is_none()
}

fn expected_runner_location(version: &str) -> String {
    format!(
        "https://github.com/actions/runner/releases/download/v{version}/actions-runner-linux-arm64-{version}.tar.gz"
    )
}

const fn invalid_document() -> DisposablePreparedTemplateError {
    template_error(
        DisposablePreparedTemplateErrorKind::InvalidDocument,
        "prepared-template declaration is invalid",
    )
}

const fn unsafe_policy() -> DisposablePreparedTemplateError {
    template_error(
        DisposablePreparedTemplateErrorKind::UnsafePolicy,
        "prepared-template declaration widens the reviewed hostile-CI policy",
    )
}

const fn template_error(
    kind: DisposablePreparedTemplateErrorKind,
    message: &'static str,
) -> DisposablePreparedTemplateError {
    let code = match kind {
        DisposablePreparedTemplateErrorKind::VersionIncompatible => {
            "prepared_template_version_incompatible"
        }
        DisposablePreparedTemplateErrorKind::InvalidDocument => "prepared_template_invalid",
        DisposablePreparedTemplateErrorKind::UnsafePolicy => "prepared_template_unsafe_policy",
        DisposablePreparedTemplateErrorKind::NonCanonical => "prepared_template_noncanonical",
    };
    DisposablePreparedTemplateError {
        kind,
        code,
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_in_manifest_is_canonical_pinned_and_domain_bound() {
        let manifest = current_disposable_prepared_template().unwrap();
        assert_eq!(manifest.schema_version(), 3);
        assert_eq!(manifest.actions_runner_version(), "2.336.0");
        assert_eq!(manifest.lima_version(), "2.2.0");
        assert_eq!(manifest.actions_runner_archive_bytes(), 138_824_064);
        assert_eq!(manifest.source_cpu_count(), 2);
        assert_eq!(manifest.source_memory_bytes(), 2 * (1 << 30));
        assert_eq!(manifest.source_disk_bytes(), 20 * (1 << 30));
        assert_eq!(
            manifest.actions_runner_digest().as_str(),
            "sha256:58b758e420b87093fbd4bfddd368074960053e2f1388f01848c82624b90f27d1"
        );
        assert_eq!(
            manifest.guest_image_digest().as_str(),
            "sha256:7df0201546f75b8bcc1044594c806c35749421ad3c9bc1be2a3ab806cfae39cc"
        );
        assert_eq!(
            manifest.lima_template_digest().as_str(),
            "sha256:7051a881544cc083677a569c1f1d69c027accd2b64c2b6577c4d3a53b5f661a2"
        );
        assert_eq!(
            manifest.ready_marker_path(),
            "/etc/smolrunner/prepared-template.json"
        );
        manifest
            .validate_lima_template(current_disposable_lima_template_bytes())
            .unwrap();
        assert_eq!(
            encode_disposable_prepared_template(&manifest).unwrap(),
            CURRENT_MANIFEST_BYTES
        );
        assert_eq!(
            manifest.identity().unwrap().as_str(),
            "sha256:6a47d79a6a66e0e267104c39b8de7b818aaaffd691c653013a4cce907ae87a15"
        );
    }

    #[test]
    fn version_precedes_new_fields_and_unknown_or_noncanonical_input_fails_closed() {
        for version in [1, 2, 4] {
            let mut value: serde_json::Value =
                serde_json::from_slice(CURRENT_MANIFEST_BYTES).unwrap();
            value["schema_version"] = serde_json::json!(version);
            value.as_object_mut().unwrap().remove("actions_runner");
            assert_eq!(
                decode_disposable_prepared_template(&serde_json::to_vec(&value).unwrap())
                    .unwrap_err()
                    .kind(),
                DisposablePreparedTemplateErrorKind::VersionIncompatible
            );
        }

        let mut value: serde_json::Value = serde_json::from_slice(CURRENT_MANIFEST_BYTES).unwrap();
        value["unexpected"] = serde_json::json!(true);
        assert_eq!(
            decode_disposable_prepared_template(&serde_json::to_vec_pretty(&value).unwrap())
                .unwrap_err()
                .kind(),
            DisposablePreparedTemplateErrorKind::InvalidDocument
        );
        let compact: Vec<u8> = serde_json::to_vec(&value_without_extra()).unwrap();
        assert_eq!(
            decode_disposable_prepared_template(&compact)
                .unwrap_err()
                .kind(),
            DisposablePreparedTemplateErrorKind::NonCanonical
        );
    }

    #[test]
    fn moving_or_digestless_inputs_and_isolation_widening_are_refused() {
        for (path, changed) in [
            (
                &["guest_image", "location"][..],
                serde_json::json!(
                    "https://cloud-images.ubuntu.com/releases/noble/release/ubuntu-24.04-server-cloudimg-arm64.img"
                ),
            ),
            (
                &["guest_image", "location"][..],
                serde_json::json!(
                    "https://cloud-images.ubuntu.com/releases/noble/release-current/ubuntu-24.04-server-cloudimg-arm64.img"
                ),
            ),
            (
                &["guest_image", "location"][..],
                serde_json::json!(
                    "https://cloud-images.ubuntu.com/releases/noble/release-20260705/ubuntu-24.04-server-cloudimg-arm64.img\u{7f}"
                ),
            ),
            (
                &["actions_runner", "location"][..],
                serde_json::json!(
                    "https://github.com/actions/runner/releases/latest/download/actions-runner-linux-arm64.tar.gz"
                ),
            ),
            (&["isolation", "host_mounts"][..], serde_json::json!(true)),
            (&["isolation", "host_resolver"][..], serde_json::json!(true)),
            (
                &["source_resources", "disk_bytes"][..],
                serde_json::json!(8 * (1_u64 << 30)),
            ),
            (
                &["provisioning", "workload_sudo"][..],
                serde_json::json!(true),
            ),
            (
                &["provisioning", "runner_dependency_install"][..],
                serde_json::json!("repository_script"),
            ),
        ] {
            let mut value = value_without_extra();
            value[path[0]][path[1]] = changed;
            let bytes = pretty(&value);
            assert_eq!(
                decode_disposable_prepared_template(&bytes)
                    .unwrap_err()
                    .kind(),
                DisposablePreparedTemplateErrorKind::UnsafePolicy
            );
        }

        let mut missing_digest = value_without_extra();
        missing_digest["actions_runner"]
            .as_object_mut()
            .unwrap()
            .remove("digest");
        assert_eq!(
            decode_disposable_prepared_template(&pretty(&missing_digest))
                .unwrap_err()
                .kind(),
            DisposablePreparedTemplateErrorKind::InvalidDocument
        );
    }

    #[test]
    fn guest_runner_and_provisioning_changes_produce_distinct_identities() {
        let baseline = current_disposable_prepared_template().unwrap();
        let baseline_identity = baseline.identity().unwrap();
        let mut changed_manifests = Vec::new();

        let mut changed = baseline.clone();
        changed.wire.guest_image.digest = format!("sha256:{}", "ab".repeat(32));
        changed_manifests.push(redecode(&changed));

        let mut changed = baseline.clone();
        changed.wire.actions_runner.digest = format!("sha256:{}", "cd".repeat(32));
        changed_manifests.push(redecode(&changed));

        let mut changed = baseline.clone();
        changed.wire.provisioning.recipe_revision = baseline.provisioning_recipe_revision() + 1;
        changed_manifests.push(redecode(&changed));

        for changed_manifest in changed_manifests {
            assert_ne!(changed_manifest.identity().unwrap(), baseline_identity);
        }
    }

    #[test]
    fn lima_template_bytes_are_exact_and_tampering_is_refused() {
        let manifest = current_disposable_prepared_template().unwrap();
        let mut changed = current_disposable_lima_template_bytes().to_vec();
        changed.push(b'\n');
        assert_eq!(
            manifest
                .validate_lima_template(&changed)
                .unwrap_err()
                .kind(),
            DisposablePreparedTemplateErrorKind::UnsafePolicy
        );

        let oversized = vec![b'x'; MAX_DISPOSABLE_LIMA_TEMPLATE_BYTES + 1];
        assert_eq!(
            manifest
                .validate_lima_template(&oversized)
                .unwrap_err()
                .kind(),
            DisposablePreparedTemplateErrorKind::UnsafePolicy
        );
    }

    fn redecode(
        manifest: &DisposablePreparedTemplateManifest,
    ) -> DisposablePreparedTemplateManifest {
        decode_disposable_prepared_template(&encode_disposable_prepared_template(manifest).unwrap())
            .unwrap()
    }

    fn value_without_extra() -> serde_json::Value {
        serde_json::from_slice(CURRENT_MANIFEST_BYTES).unwrap()
    }

    fn pretty(value: &serde_json::Value) -> Vec<u8> {
        let mut bytes = serde_json::to_vec_pretty(value).unwrap();
        bytes.push(b'\n');
        bytes
    }
}
