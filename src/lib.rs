/// Read-only observation of one configured official Actions runner.
pub mod actions_runner_readiness;
/// Pure recommendation-only adaptive CI pool routing from bounded evidence.
pub mod adaptive_ci_routing;
/// Pure deterministic optimization discovery from bounded verification observations.
pub mod adaptive_verification_compiler;
pub mod artifact;
/// Pure, path-free classification of explicit hot-state inventory observations.
pub mod cache_inventory;
/// Positive-only Linux process and mount reference observation for one Cargo target.
#[cfg(target_os = "linux")]
pub mod cargo_target_holder_observation;
/// Read-only, descriptor-bound observation of one Linux Cargo target tree.
#[cfg(target_os = "linux")]
pub mod cargo_target_observation;
/// Pure projection of measured CMUX product transport/restore receipts into adaptive observations.
pub mod cmux_product_transport_adapter;
/// Pure projection of validated CMUX workload results into adaptive verification observations.
pub mod cmux_workload_verification_adapter;
/// Pure workload-neutral pre-admission compute request.
pub mod compute_execution_request;
/// Pure workload-family-neutral identity for declared compute semantics.
pub mod compute_workload;
#[cfg(target_os = "linux")]
pub mod debian_package_plan;
#[cfg(target_os = "linux")]
pub mod debian_package_probe;
#[cfg(target_os = "linux")]
pub mod debian_package_recovery;
/// Descriptor-bound execution of already reviewed Linux launch plans.
#[cfg(target_os = "linux")]
pub mod descriptor_bound_launcher;
/// Pure bounded multi-attempt resource ledger and atomic store contract.
pub mod disposable_attempt_catalog;
mod disposable_attempt_catalog_job_lookup;
/// Pure durable state, revisions, and codec for one disposable worker attempt.
pub mod disposable_attempt_state;
/// Same-lock execution of one authorized disposable Lima clone.
#[cfg(unix)]
pub mod disposable_clone_runtime;
#[cfg(unix)]
pub(crate) mod disposable_host_storage;
/// Exact plan plus explicitly approved macOS apply boundary for the disposable-worker LaunchAgent.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub mod disposable_launchd_service;
/// Read-only exact installed-state observation for the disposable-worker LaunchAgent.
#[cfg(target_os = "macos")]
pub mod disposable_launchd_service_status;
/// Sealed fixed Lima command plans for one durably planned disposable worker.
pub mod disposable_lima_worker;
/// Canonical supply-chain and isolation identity for the prepared disposable VM template.
pub mod disposable_prepared_template;
/// Private, secret-safe command binding for one durably registered disposable guest runner.
#[cfg(unix)]
pub(crate) mod disposable_runner_runtime;
/// Pure bounded diagnostic document for one controller-service failure.
pub mod disposable_service_failure_receipt;
pub mod disposable_template_generation;
/// Same-lock bounded Lima supervisor for the disposable source-template lifecycle.
#[cfg(unix)]
pub mod disposable_template_runtime;
#[cfg(unix)]
pub(crate) mod disposable_worker_coordinator;
/// Canonical, secret-free operator enrollment for one disposable Scale Set worker.
#[cfg(unix)]
pub mod disposable_worker_enrollment;
/// Pure capacity and lifecycle reconciliation for one-job disposable workers.
pub mod disposable_worker_reconciler;
/// Process-lifetime composition of enrollment, durable recovery, coordinator, and supervisor.
#[cfg(unix)]
pub mod disposable_worker_service;
#[cfg(unix)]
pub(crate) mod disposable_worker_supervisor;
pub mod doctor;
pub mod durable_journal;
#[cfg(target_os = "linux")]
pub mod durable_lane_execution;
#[cfg_attr(test, allow(clippy::too_many_arguments))]
pub mod execution_admission;
/// Pure product-neutral resource ownership and fail-closed capacity arithmetic.
pub mod execution_capacity;
pub mod execution_receipt;
pub mod execution_receipt_store;
/// Pure bounded failure classification, diagnostic probes, remedies, and preflight learning.
pub mod failure_diagnostic;
/// Pure model-derived frontier-inference workload vocabulary and synthetic sensitivity fixtures.
pub mod frontier_inference_workload;
/// Pure Git index-v2 stat-cache patching for CoW task materialization.
pub mod git_index_stat_patch;
/// Private-process adapter for the pinned official Runner Scale Set bridge.
#[cfg(unix)]
pub(crate) mod github_scale_set_bridge;
/// Canonical bounded durable record of one polled Runner Scale Set delivery.
#[cfg(unix)]
pub(crate) mod github_scale_set_delivery;
/// Pure exact reconciliation of one retained Scale Set delivery into disposable-attempt state.
#[cfg(unix)]
pub(crate) mod github_scale_set_delivery_consumer;
/// Crash-safe poll, durable reconciliation, acknowledgement, and acquisition recovery.
#[cfg(unix)]
pub(crate) mod github_scale_set_delivery_controller;
/// Pure catalog settlement after conclusive Scale Set acknowledgement evidence.
#[cfg(unix)]
pub(crate) mod github_scale_set_delivery_settlement;
/// Pure crash/replay phases for one durably reconciled Scale Set delivery.
#[cfg(unix)]
pub(crate) mod github_scale_set_delivery_state;
/// Pure bounded vocabulary for GitHub Runner Scale Set job and runner identities.
pub mod github_scale_set_protocol;
/// Pure, fail-closed mapping of reviewed GitHub workflow-job evidence into typed broker intents.
pub mod github_workflow_job_mapper;
/// Pure, bounded normalization of complete GitHub workflow-job reconciliation snapshots.
pub mod github_workflow_job_reconciliation;
pub mod host;
#[cfg(target_os = "linux")]
pub mod host_package_plan;
#[cfg(target_os = "linux")]
pub mod host_preparation_command;
#[cfg(target_os = "linux")]
pub mod host_preparation_execution;
#[cfg(target_os = "linux")]
pub mod host_preparation_plan;
#[cfg(target_os = "linux")]
pub mod host_preparation_receipt;
#[cfg(target_os = "linux")]
pub mod host_preparation_receipt_binding;
#[cfg(target_os = "linux")]
pub mod host_readiness;
#[cfg(target_os = "linux")]
pub mod host_readiness_verdict;
#[cfg(target_os = "linux")]
pub mod host_rootless_podman;
/// Pure bounded observation-only receipts for blazingly hot execution measurements.
pub mod hot_execution_performance;
/// Pure descriptive p50/p90 summaries around exact hot-fleet A-B-B-A comparisons.
pub mod hot_fleet_latency_summary;
/// Pure bounded contended fleet-window receipts and exact A-B-B-A comparison evidence.
pub mod hot_fleet_window;
/// Read-only, descriptor-bound observation of an explicit Linux hot-run cache root.
#[cfg(target_os = "linux")]
pub mod hot_run_cache_observation;
/// Pure path-class policy for selecting reviewed hot-state sharing mechanisms.
pub mod hot_state_path_policy;
/// Pure transport-independent identity and source observations for immutable build products.
pub mod immutable_artifact_distribution;
/// Pure immutable resident Git object-pool generation and consumer-lease core.
pub mod immutable_git_object_pool;
/// Pure sealed non-task Git producer planning for immutable pool publication.
#[cfg(target_os = "linux")]
pub mod immutable_git_object_pool_admin_producer_plan;
/// Publication-time descriptor-bound audit of staged immutable Git pool candidates.
#[cfg(target_os = "linux")]
pub mod immutable_git_object_pool_generation_audit;
/// Pure fixed marker codec for immutable Git object-pool generations.
pub mod immutable_git_object_pool_marker;
/// Read-only descriptor-bound ownership observation of immutable Git object-pool generations.
#[cfg(target_os = "linux")]
pub mod immutable_git_object_pool_observation;
#[cfg(target_os = "linux")]
pub mod installation_id;
pub mod journal;
pub mod journal_document;
pub mod lane_command;
#[cfg(target_os = "linux")]
pub mod lane_executable;
#[cfg(target_os = "linux")]
pub mod lane_executor;
pub mod lease;
pub mod lease_catalog;
pub mod lease_document;
/// Descriptor-bound host identity for one reviewed Lima VZ instance and raw root disk.
#[cfg(unix)]
pub mod lima_host_identity;
/// Pure Lima policy: work while active, interactive after 10 idle minutes, stopped after 30.
pub mod lima_lifecycle;
/// Fixed direct executor for accepted personal-worker Lima lifecycle actions.
pub mod lima_lifecycle_executor;
/// Read-only, bounded exact observation of one Lima instance and running guest.
pub mod lima_observation;
/// Read-only observation and pure reclaim planning for linked Git worktrees.
#[cfg(unix)]
pub mod linked_worktree_reclaim;
/// Pure bounded parsing of the admitted glibc dynamic-loader cache.
#[cfg(target_os = "linux")]
pub mod linux_dynamic_loader_cache;
/// Pure bounded parsing of the admitted Linux dynamic-loader configuration.
#[cfg(target_os = "linux")]
pub mod linux_dynamic_loader_config;
/// Pure bounded ELF64 dependency parsing for the Linux runtime closure.
#[cfg(target_os = "linux")]
pub mod linux_elf_runtime_dependency;
/// Bounded path-private observation of one Linux operator machine.
#[cfg(target_os = "linux")]
pub mod linux_host_observation;
/// Read-only, fail-closed lookup of persisted project installations.
#[cfg(target_os = "linux")]
pub mod linux_installation_catalog;
/// Nonblocking coordination for installation-catalog discovery and creation.
#[cfg(target_os = "linux")]
pub mod linux_installation_catalog_lock;
/// Locked, race-free create-or-load orchestration for local project installations.
#[cfg(target_os = "linux")]
pub mod linux_installation_enrollment;
/// Staged, durable, no-replace publication of complete project installations.
#[cfg(target_os = "linux")]
pub mod linux_installation_publication;
/// Durable, revision-checked lease persistence beneath one installation directory.
#[cfg(target_os = "linux")]
pub mod linux_lease_store;
/// Direct command-free observation of the five account-related personal-worker runtime classes.
#[cfg(target_os = "linux")]
pub mod linux_personal_worker_runtime_account_evidence;
/// Descriptor-bound prerequisites for the fixed personal-worker runtime executables.
#[cfg(target_os = "linux")]
pub mod linux_personal_worker_runtime_executable_prerequisite;
/// Direct command-free Linux kernel and cgroup-v2 prerequisites for the personal-worker runtime.
#[cfg(target_os = "linux")]
pub mod linux_personal_worker_runtime_kernel_prerequisite;
/// Same-lock snapshot of current executable and dynamic-loader prerequisites.
#[cfg(target_os = "linux")]
pub mod linux_personal_worker_runtime_linkage_prerequisite;
/// Read-only, descriptor-bound observation of the fixed GNU dynamic-loader object.
#[cfg(target_os = "linux")]
pub mod linux_personal_worker_runtime_loader_object_prerequisite;
/// Descriptor-bound prerequisite for fixed loader configuration, cache, and preload absence.
#[cfg(target_os = "linux")]
pub mod linux_personal_worker_runtime_loader_state_prerequisite;
/// Read-only, locked discovery of one protected recorded personal-worker runtime manifest.
#[cfg(target_os = "linux")]
pub mod linux_personal_worker_runtime_manifest;
/// Native Linux research adapter for exact same-HEAD reflink task worktrees.
#[cfg(target_os = "linux")]
pub mod linux_reflink_task_materialization;
#[cfg(target_os = "linux")]
pub mod linux_state;
#[cfg(target_os = "linux")]
pub mod linux_state_prepare;
#[cfg(target_os = "linux")]
pub mod linux_state_recovery;
/// Pure fixed offline Cargo command policy for exact local self-builds.
pub mod local_install_build_command;
/// Bounded exact-source local self-build execution and artifact evidence.
#[cfg(unix)]
pub mod local_install_build_execution;
/// Read-only, path-private proof that the isolated self-build Cargo lookup path is config-free.
pub mod local_install_cargo_config_preflight;
/// Crash-safe private per-user publication of verified Glaeda binary generations.
#[cfg(unix)]
pub mod local_install_generation_store;
/// Descriptor-bound observation and publication of the canonical local `glaeda` launcher.
#[cfg(unix)]
pub mod local_install_launcher;
/// Pure exact-source local binary generation and stable launcher planning.
pub mod local_install_plan;
/// Read-only exact checkout and Cargo.lock proof for local self-builds.
#[cfg(unix)]
pub mod local_install_source_preflight;
/// Pure interference policy shared by local owned-execution adapters.
pub mod local_interference_admission;
pub mod mac_availability;
pub mod macos_resource_observation;
pub mod manifest;
/// Pure schema-versioned personal-worker operator configuration and public identity.
pub mod operator_config;
/// Private-path-safe discovery and atomic persistence of operator configuration.
pub mod operator_config_store;
/// Closed public operator error, retry, remediation, dependency, approval, and command vocabulary.
pub mod operator_error;
/// Pure non-authorizing remediation applicability, safety, and confidence vocabulary.
pub mod operator_remediation;
/// Pure unified personal-worker operator status report and human renderer.
pub mod operator_status;
/// Typed, read-only aggregation of one coherent operator status evidence bundle.
pub mod operator_status_service;
/// Canonical controller enrollment for one trusted native-Linux GitHub Actions JIT worker.
#[cfg(target_os = "linux")]
pub mod owned_linux_jit_enrollment;
/// Native owned-Linux task backend for one trusted GitHub Actions JIT runner.
#[cfg(unix)]
pub(crate) mod owned_linux_jit_runtime;
pub mod ownership;
/// Pure composition of durable queue, Lima lifecycle, and runner-readiness evidence.
pub mod personal_worker_host_broker;
/// Same-lock durable execution of one exact personal-worker Lima lifecycle tick.
pub mod personal_worker_lima_adapter;
/// Pure, path-private durable ownership and crash-phase authority for personal-worker Lima.
pub mod personal_worker_lima_authority;
/// Read-only Mac/Lima observation composed for personal-worker planning.
pub mod personal_worker_mac_observation;
/// Config-bound ergonomic submission and queued cancellation.
pub mod personal_worker_operator_mutation;
/// Config-bound, current-snapshot status, queue, and job reads.
pub mod personal_worker_operator_read;
/// Config-bound read-only discovery and explicit first initialization of durable worker state.
pub mod personal_worker_operator_store;
pub mod personal_worker_queue;
/// Pure bounded projection of durable personal-worker status, queue pages, and job state.
pub mod personal_worker_read_model;
/// Pure binding of one repository-owned parallel verifier to one personal-worker attempt/result.
pub mod personal_worker_repository_result;
/// Read-only official-runner readiness composed with exact personal-worker evidence.
pub mod personal_worker_runner_readiness;
/// Pure sealed authority for the exact personal-worker Linux verification-runtime closure.
pub mod personal_worker_runtime_contract;
/// Strict canonical declaration of one installed personal-worker runtime closure.
pub mod personal_worker_runtime_manifest;
pub mod personal_worker_store;
pub mod personal_worker_store_transaction;
/// Pure, one-action personal-worker planning over accepted queue and host evidence.
pub mod personal_worker_tick;
/// Pure immutable verification authorization planning from sealed personal-worker evidence.
#[cfg(target_os = "linux")]
pub mod personal_worker_verification_plan;
pub mod plan;
pub mod process;
/// Pure, strict logical project catalog identities and alias resolution.
pub mod project_catalog;
/// Read-only, credentialless observation of one developer Git checkout on Unix hosts.
#[cfg(unix)]
pub mod project_checkout_observation;
/// Read-only, bounded immediate-child discovery beneath one explicit project root.
#[cfg(unix)]
pub mod project_discovery;
/// Pure no-replay lifecycle for one exact formatted project-disk attachment.
pub mod project_disk_attachment_state;
/// Pure identity vocabulary for one formatted project-disk filesystem generation.
pub mod project_disk_filesystem;
/// Descriptor-bound read-only observation of Lima standalone project disks.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod project_disk_host_observation;
/// Pure crash-safe single-writer lease planning for resident trusted project disks.
pub mod project_disk_lease;
/// Pure bounded validator for the final #628 full project-filesystem correlation receipt.
pub mod project_filesystem_correlation_receipt;
pub mod project_workspace_identity;
/// Pure path-free catalog-wide authority vocabulary for protected cache generations.
pub mod protected_cache_generation_catalog;
/// Conservative namespace-wide lease visibility from personal-worker durable state.
pub mod protected_cache_namespace_lease_visibility;
/// Strict path-free replacement-equivalence receipt vocabulary for protected cache generations.
pub mod protected_cache_replacement_equivalence;
pub mod quarry_parallel_verification_adapter;
/// Bounded exact-byte capture from one already-open Quarry verification receipt channel.
pub mod quarry_parallel_verification_capture;
/// Strict supplied-receipt-only decoder for Quarry parallel verification receipt v2.
pub mod quarry_parallel_verification_receipt;
pub mod renderprove_artifact_binding;
pub mod renderprove_execution;
#[cfg(target_os = "linux")]
pub mod renderprove_native_probe;
/// Descriptor-bound protected project/evidence mount lease for native Renderprove probes.
#[cfg(target_os = "linux")]
pub mod renderprove_protected_mount;
pub mod renderprove_verification;
pub mod renderprove_vision_profile;
pub mod renderprove_vision_result;
/// Credentialless, bounded observation of one immutable reviewed repository source.
pub mod repository_source_observation;
/// Pure frozen preregistration and sample binding for resident backend comparisons.
pub mod resident_backend_comparison;
/// Bounded exact-OID repository evidence for resident trusted-agent work.
#[cfg(unix)]
pub mod resident_repo_query;
/// Pure, authority-free resident-sandbox generation/config/locator catalog and strict codec.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod resident_sandbox_catalog;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod resident_sandbox_host_observation;
pub mod resource;
/// Binding from the reusable-state identity contract to the hot-state path-class policy.
pub mod reusable_state_hot_state_policy;
/// Pure evidence-driven lifecycle and retention policy for expensive reusable state.
pub mod reusable_state_lifecycle;
#[cfg(target_os = "linux")]
pub mod rootless_podman_config;
/// Strict, bounded, nonblocking, descriptor-relative, identity-bound observation of reviewed Podman sources.
#[cfg(target_os = "linux")]
pub mod rootless_podman_config_observation;
/// Pure, bounded, fail-closed precedence resolution and static-preflight assessment of Podman config.
#[cfg(target_os = "linux")]
pub mod rootless_podman_config_resolution;
#[cfg(target_os = "linux")]
pub mod rootless_podman_preflight;
#[cfg(target_os = "linux")]
pub mod runner_account_observation;
#[cfg(target_os = "linux")]
pub mod runner_account_plan;
#[cfg(target_os = "linux")]
pub mod runner_user;
#[cfg(target_os = "linux")]
pub mod runner_user_observation;
/// Pure classification of trusted bounded Rust memory-pressure observations.
pub mod rust_memory_diagnostic;
/// Pure repository-declared Rust build-scope and bounded resource-envelope contracts.
pub mod rust_verification_envelope;
/// Canonical digest binding for reviewed Rust verification envelopes.
pub mod rust_verification_envelope_digest;
pub mod state;
pub mod state_document;
/// Closed current/legacy selection for Glaeda's fixed Linux durable-state roots.
pub mod state_root_generation;
pub mod state_store;
#[cfg(target_os = "linux")]
pub mod subordinate_id;
/// Pure sealed task-private Git clone planning from exact hot-path leases.
#[cfg(target_os = "linux")]
pub mod task_private_git_clone_plan;
/// Pure reviewed identity for one resident Linux guest-control executable generation.
pub mod trusted_guest_control_binary_generation;
/// Root-only, closed, one-transaction Linux guest-control dispatcher.
#[cfg(unix)]
pub mod trusted_guest_control_dispatcher;
/// Pure sealed one-shot invocation plan for purpose-typed Linux guest-control transactions.
pub mod trusted_guest_control_invocation_plan;
/// Mutation-free resident guest-control transport probe protocol.
pub mod trusted_guest_control_probe_protocol;
/// Pure canonical one-shot protocol envelope for purpose-typed Linux guest-control transactions.
pub mod trusted_guest_control_protocol;
/// Pure canonical request+payload and receipt+result guest-control transport frames.
pub mod trusted_guest_control_transaction;
#[cfg(target_os = "linux")]
/// Sealed all-FD trusted OverlayFS mount transaction behind exact correlation evidence.
pub mod trusted_overlay_mount_execution;
#[cfg(target_os = "linux")]
/// Read-only exact prerequisite observation and sealed intent for trusted OverlayFS mounts.
pub mod trusted_overlay_mount_plan;
/// Pure authority and lifecycle core for trusted resident OverlayFS task views.
pub mod trusted_overlay_task_view;
/// Read-only exact whole-block-device correlation for the mounted project filesystem.
pub mod trusted_project_block_device_correlation;
/// Opaque project-disk/filesystem correlation gate for trusted resident Linux mutation.
pub mod trusted_project_filesystem_correlation;
/// Opaque composition of exact guest filesystem + whole-block-device evidence.
pub mod trusted_project_filesystem_full_guest_correlation;
/// Read-only guest correlation of one held project-filesystem device with exact mountinfo evidence.
pub mod trusted_project_filesystem_guest_observation;
/// Canonical path-free payload/result for the guest project-filesystem observation operation.
pub mod trusted_project_filesystem_guest_protocol;
/// Descriptor-relative trusted producer for runner workspace and cache identity receipts.
#[cfg(target_os = "linux")]
pub mod trusted_workspace_receipt;
#[cfg(unix)]
pub mod unix_personal_worker_store;
/// Unix descriptor-retained persistence for the empty protected cache-generation catalog.
#[cfg(unix)]
pub mod unix_protected_cache_generation_catalog_store;
/// Descriptor-bound private persistence for the resident-sandbox catalog.
#[cfg(unix)]
pub mod unix_resident_sandbox_store;
/// Pure fixed repository verification command meanings for identity derivation and later execution.
pub mod verification_command_semantics;
/// Pure sealed cross-attempt verification-isolation compatibility.
pub mod verification_isolation_referee;
pub mod verification_profile;
pub mod verification_profile_preflight_adapter;
pub mod verification_profile_registry;

use serde::Serialize;

pub const REPORT_SCHEMA_VERSION: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Pass,
    Warn,
    Fail,
}

impl CheckStatus {
    #[must_use]
    pub const fn rank(self) -> u8 {
        match self {
            Self::Pass => 0,
            Self::Warn => 1,
            Self::Fail => 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Check {
    pub id: String,
    pub status: CheckStatus,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl Check {
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        status: CheckStatus,
        summary: impl Into<String>,
        detail: Option<String>,
    ) -> Self {
        Self {
            id: id.into(),
            status,
            summary: summary.into(),
            detail,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DoctorReport {
    pub schema_version: u8,
    pub overall: CheckStatus,
    pub checks: Vec<Check>,
}

impl DoctorReport {
    #[must_use]
    pub fn from_checks(checks: Vec<Check>) -> Self {
        let overall = checks
            .iter()
            .map(|check| check.status)
            .max_by_key(|status| status.rank())
            .unwrap_or(CheckStatus::Pass);

        Self {
            schema_version: REPORT_SCHEMA_VERSION,
            overall,
            checks,
        }
    }

    #[must_use]
    pub fn has_failures(&self) -> bool {
        self.overall == CheckStatus::Fail
    }

    #[must_use]
    pub fn has_warnings(&self) -> bool {
        self.checks
            .iter()
            .any(|check| check.status == CheckStatus::Warn)
    }
}

#[cfg(test)]
mod tests {
    use super::{Check, CheckStatus, DoctorReport};

    #[test]
    fn report_uses_most_severe_status() {
        let report = DoctorReport::from_checks(vec![
            Check::new("ok", CheckStatus::Pass, "ok", None),
            Check::new("warning", CheckStatus::Warn, "warning", None),
            Check::new("failure", CheckStatus::Fail, "failure", None),
        ]);

        assert_eq!(report.overall, CheckStatus::Fail);
        assert!(report.has_failures());
        assert!(report.has_warnings());
    }
}
