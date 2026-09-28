use std::process::Command;

const PRODUCT_SENTENCE: &str = "Run trustworthy compute across local and fleet execution capacity";

#[test]
fn root_help_describes_glaeda_as_a_general_compute_runtime() {
    let output = Command::new(env!("CARGO_BIN_EXE_glaeda"))
        .arg("--help")
        .output()
        .expect("execute glaeda --help");

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("help output is UTF-8");
    assert!(stdout.contains(PRODUCT_SENTENCE));
    assert!(!stdout.contains("Tend a small fleet of self-hosted GitHub Actions runners"));
}
