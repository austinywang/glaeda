use std::path::PathBuf;
use std::process::Command;

#[test]
fn native_apple_build_policy_contracts_pass() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let status = Command::new("python3")
        .arg(root.join("scripts/test-apple-build.py"))
        .current_dir(&root)
        .status()
        .expect("python3 is required by the repository verification profile");
    assert!(status.success(), "native Apple build contract tests failed");
}
