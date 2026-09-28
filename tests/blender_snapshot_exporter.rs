use std::path::PathBuf;
use std::process::Command;

#[test]
fn blender_snapshot_exporter_contract_tests_pass() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let status = Command::new("python3")
        .arg(root.join("scripts/test-blender-snapshot-export.py"))
        .current_dir(&root)
        .status()
        .expect("python3 must be available in the verified Glaeda development environment");
    assert!(
        status.success(),
        "Blender snapshot exporter contract tests failed"
    );
}
