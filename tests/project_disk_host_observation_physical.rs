#![cfg(any(target_os = "linux", target_os = "macos"))]

use std::env;
use std::fs;

use glaeda::project_disk_host_observation::{
    LimaStandaloneDiskDisposition, LimaStandaloneDiskFixtureObservationRequest,
    LimaStandaloneDiskName, observe_lima_standalone_disk_fixture,
};

const MAX_INVENTORY_BYTES: u64 = 64 * 1024;

#[test]
#[ignore = "requires the explicitly retained #634 operator-Mac test fixture"]
fn observes_retained_test_fixture_without_project_disk_adoption() {
    let lima_home = env::var_os("SMOLRUNNER_TEST_LIMA_HOME")
        .expect("set the exact retained test-only Lima home");
    let disk_directory = env::var_os("SMOLRUNNER_TEST_DISK_DIRECTORY")
        .expect("set the exact directly observed test-only disk directory");
    let disk_name =
        env::var("SMOLRUNNER_TEST_DISK_NAME").expect("set the exact test-only Lima disk locator");
    let inventory_path = env::var_os("SMOLRUNNER_TEST_DISK_INVENTORY")
        .expect("set a fresh private read-only Lima disk inventory receipt");
    let expected = env::var("SMOLRUNNER_TEST_DISK_STATE")
        .expect("set detached or attached for the current fixture state");

    let metadata = fs::metadata(&inventory_path).expect("inventory receipt metadata");
    assert!(metadata.is_file());
    assert!(metadata.len() <= MAX_INVENTORY_BYTES);
    let inventory = fs::read(inventory_path).expect("bounded private inventory receipt");
    let request = LimaStandaloneDiskFixtureObservationRequest::new(
        LimaStandaloneDiskName::parse(&disk_name).expect("exact test disk locator"),
        lima_home,
        disk_directory,
    )
    .expect("exact retained fixture request");
    let mut observation = observe_lima_standalone_disk_fixture(request, &inventory)
        .expect("descriptor-bound observation");
    observation
        .confirm(&inventory)
        .expect("held descriptors remain exact");

    let expected = match expected.as_str() {
        "detached" => LimaStandaloneDiskDisposition::Detached,
        "attached" => LimaStandaloneDiskDisposition::Attached,
        _ => panic!("expected state must be detached or attached"),
    };
    assert_eq!(observation.summary().disposition(), expected);
    println!(
        "SMOLRUNNER_PROJECT_DISK_P2_PHYSICAL_V1 {}",
        serde_json::to_string(observation.summary()).expect("bounded sanitized summary")
    );
}
