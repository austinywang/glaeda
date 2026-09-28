#![cfg(target_os = "linux")]

use glaeda::debian_package_plan::{DEBIAN_PACKAGE_PLAN_SCHEMA_VERSION, PackagePlanDisposition};
use glaeda::host_preparation_plan::{
    HostReadinessSourceIdentity, SourceExecutableIdentity, SourceRootlessPodmanIdentity,
    SourceRunnerAccountIdentity,
};
use glaeda::host_preparation_receipt_binding::{
    HostPreparationReceiptBindingErrorKind, MAX_HOST_PREPARATION_SOURCE_DIGEST_BYTES,
    digest_host_preparation_source,
};
use glaeda::host_readiness::HostObservationState;
use glaeda::rootless_podman_preflight::RootlessPodmanPreflightState;

const PRIVATE_OVERSIZED_MARKER: &str = "PRIVATE_OVERSIZED_SOURCE_SENTINEL";

#[test]
fn oversized_private_source_stops_at_the_streaming_digest_limit() {
    let private_path = format!(
        "/private/{PRIVATE_OVERSIZED_MARKER}/{}",
        "x".repeat(MAX_HOST_PREPARATION_SOURCE_DIGEST_BYTES)
    );
    let source = HostReadinessSourceIdentity {
        kind: "host_readiness".to_owned(),
        schema_version: 1,
        repository: "example/project".to_owned(),
        executables: vec![SourceExecutableIdentity {
            name: "git".to_owned(),
            path: private_path,
            state: HostObservationState::Matching,
        }],
        package_plan_schema_version: DEBIAN_PACKAGE_PLAN_SCHEMA_VERSION,
        package_disposition: PackagePlanDisposition::Ready,
        runner_account: SourceRunnerAccountIdentity::NeedsConfiguration,
        rootless_podman: SourceRootlessPodmanIdentity::Deferred {
            state: RootlessPodmanPreflightState::Unknown,
        },
    };

    let error = digest_host_preparation_source(&source).expect_err("oversized source must fail");
    assert_eq!(
        error.kind(),
        HostPreparationReceiptBindingErrorKind::SourceTooLarge
    );
    assert!(!error.message().contains(PRIVATE_OVERSIZED_MARKER));
    assert!(!format!("{error:?}").contains(PRIVATE_OVERSIZED_MARKER));
}
