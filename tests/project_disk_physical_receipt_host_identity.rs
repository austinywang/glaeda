#![cfg(unix)]

use std::env;

use glaeda::lima_host_identity::LimaHostIdentityAdapter;
use glaeda::lima_observation::{
    LimaArchitecture, LimaInstanceName, LimaObservationRequest, LimaVmType,
};
use serde_json::json;

const RECEIPT_TYPE: &str = "smolrunner-lima-host-identity-observation";

#[test]
#[ignore = "requires an explicitly selected operator-Mac Lima VZ instance"]
fn emits_descriptor_bound_lima_host_identity_for_project_disk_receipt() {
    let lima_home = env::var("SMOLRUNNER_TEST_LIMA_HOME")
        .expect("set SMOLRUNNER_TEST_LIMA_HOME to the exact private Lima home");
    let instance = env::var("SMOLRUNNER_TEST_LIMA_INSTANCE")
        .expect("set SMOLRUNNER_TEST_LIMA_INSTANCE to the exact resident sandbox instance");
    let guest_cache = env::var("SMOLRUNNER_TEST_GUEST_CACHE_PATH")
        .expect("set SMOLRUNNER_TEST_GUEST_CACHE_PATH to the exact reviewed guest cache path");

    let request = LimaObservationRequest::new(
        LimaInstanceName::parse(&instance).expect("exact Lima instance name"),
        lima_home,
        LimaVmType::Vz,
        LimaArchitecture::Aarch64,
        guest_cache,
        30,
    )
    .expect("exact Lima observation request");
    let observation = LimaHostIdentityAdapter
        .observe(&request)
        .expect("descriptor-bound Lima host identity observation");

    let receipt = json!({
        "schema_version": 1,
        "receipt_type": RECEIPT_TYPE,
        "instance": request.instance().as_str(),
        "lima_host_identity": observation.identity().digest().as_str(),
        "lima_request_identity": request.request_identity().digest().as_str(),
    });
    println!(
        "SMOLRUNNER_PROJECT_DISK_HOST_IDENTITY_V1 {}",
        serde_json::to_string(&receipt).expect("encode bounded host identity receipt")
    );
}
