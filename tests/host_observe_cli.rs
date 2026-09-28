#![cfg(target_os = "linux")]

use std::path::Path;
use std::process::Command;

const BINARY: &str = env!("CARGO_BIN_EXE_glaeda-host-observe");

#[test]
fn live_json_observation_is_typed_and_path_private() {
    if !live_host_surface_is_available() {
        return;
    }
    let output = Command::new(BINARY)
        .args(["--output", "json", "--port", "3000", "--port", "8080"])
        .output()
        .expect("run Linux host observer");
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON report");
    assert_eq!(report["document_type"], "glaeda-linux-host-observation");
    assert_eq!(report["schema_version"], 3);
    assert_eq!(report["authority"], "observation_only");
    assert_eq!(report["scope"], "current_execution_context");
    assert!(
        report["cpu"]["logical_cpus"]
            .as_u64()
            .is_some_and(|value| value > 0)
    );
    assert!(
        report["memory"]["total_bytes"]
            .as_u64()
            .is_some_and(|value| value > 0)
    );
    assert!(report["scheduler"]["autogroup"].is_string());
    assert!(report["scheduler"]["sched_ext"]["support"].is_string());
    let online = report["scheduler"]["cpu_policy"]["online_logical_cpus"]
        .as_u64()
        .expect("online CPU count");
    assert!(online > 0);
    assert!(report["scheduler"]["cpu_policy"]["smt"].is_string());
    assert!(
        report["scheduler"]["cpu_policy"]["nohz_full_online_logical_cpus"]
            .as_u64()
            .is_some()
    );
    assert!(
        report["scheduler"]["cpu_policy"]["isolated_online_logical_cpus"]
            .as_u64()
            .is_some()
    );
    let frequency = &report["scheduler"]["cpu_policy"]["frequency"];
    assert!(frequency["support"].is_string());
    if frequency["support"] == "supported" {
        let classified = frequency["classes"]
            .as_array()
            .expect("frequency classes")
            .iter()
            .map(|class| class["logical_cpus"].as_u64().expect("class CPU count"))
            .sum::<u64>();
        assert_eq!(classified, online);
    }
    assert_eq!(report["watched_ports"].as_array().expect("ports").len(), 2);
    let encoded = String::from_utf8(output.stdout).expect("UTF-8 output");
    for private_shape in ["/home/", "/proc/", "cmdline", "pid", "unit_name", "address"] {
        assert!(!encoded.contains(private_shape));
    }
}

#[test]
fn human_output_is_derived_from_the_same_bounded_report() {
    if !live_host_surface_is_available() {
        return;
    }
    let output = Command::new(BINARY)
        .args(["--output", "human", "--port", "3000"])
        .output()
        .expect("run Linux host observer");
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 output");
    assert!(stdout.contains("authority=observation_only"));
    assert!(stdout.contains("failed units: system="));
    assert!(stdout.contains("scheduler: autogroup="));
    assert!(stdout.contains("cpu policy: online="));
    assert!(stdout.contains("watched ports: 3000="));
    assert!(!stdout.contains("/home/"));
}

#[test]
fn caller_cannot_select_kernel_or_systemctl_paths() {
    let output = Command::new(BINARY)
        .arg("--help")
        .output()
        .expect("read help");
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 help");
    assert!(!stdout.contains("proc-root"));
    assert!(!stdout.contains("systemctl-program"));
}

#[test]
fn invalid_port_fails_before_host_observation() {
    let output = Command::new(BINARY)
        .args(["--output", "json", "--port", "0"])
        .output()
        .expect("run invalid request");
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).expect("UTF-8 error");
    assert!(stderr.contains("invalid value"));
    assert!(!stderr.contains("/home/"));
}

fn live_host_surface_is_available() -> bool {
    Path::new("/sys/devices/system/cpu/online").is_file()
}
