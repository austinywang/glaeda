use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

fn input() -> Value {
    json!({
        "schema_version": 1,
        "request": {"interference_class": "coexist"},
        "observation": {
            "observed_at_unix_millis": 1000, "node_control": "available",
            "pressure": "low", "candidate_quiet_compatibility": "unknown",
            "quiet_lease": null,
            "active": {"conflicting_non_yieldable": 0, "conflicting_yieldable": 0}
        }
    })
}

fn run(raw: &[u8], arguments: &[&str]) -> (bool, Value) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_glaeda-local-admission"))
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Large-input rejection may close stdin before the test writes everything.
    let _ = child.stdin.take().unwrap().write_all(raw);
    let result = child.wait_with_output().unwrap();
    assert!(result.stderr.is_empty());
    assert!(result.stdout.len() < 2048);
    let document: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(document["grants_authority"], false);
    assert_eq!(document["authorizes_execution"], false);
    (result.status.success(), document)
}

#[test]
fn local_policy_decisions_bind_exact_input_without_execution_authority() {
    for (control, pressure, disposition, reason) in [
        ("available", "low", "admit_now", "compatible"),
        ("held", "high", "refuse", "node_held"),
        ("draining", "low", "wait", "node_draining"),
        ("available", "high", "wait", "pressure_high"),
    ] {
        let mut value = input();
        value["observation"]["node_control"] = json!(control);
        value["observation"]["pressure"] = json!(pressure);
        let raw = serde_json::to_vec(&value).unwrap();
        let (success, result) = run(&raw, &[]);
        assert!(success);
        assert_eq!(
            result["input_sha256"],
            format!("sha256:{:x}", Sha256::digest(&raw))
        );
        assert_eq!(result["decision"]["disposition"], disposition);
        assert_eq!(result["decision"]["reason"], reason);
        assert_eq!(result["decision"]["authorizes_execution"], false);
        assert_eq!(result["decision"]["authorizes_preemption"], false);
    }
}

#[test]
fn quiet_lease_expiry_is_decided_by_the_existing_policy() {
    let mut value = input();
    value["observation"]["candidate_quiet_compatibility"] = json!("conflicting");
    value["observation"]["quiet_lease"] = json!({"generation": 1, "expires_at_unix_millis": 1001});
    let (_, active) = run(&serde_json::to_vec(&value).unwrap(), &[]);
    assert_eq!(active["decision"]["reason"], "quiet_lease_conflict");
    value["observation"]["observed_at_unix_millis"] = json!(1001);
    let (_, expired) = run(&serde_json::to_vec(&value).unwrap(), &[]);
    assert_eq!(expired["decision"]["reason"], "compatible");
}

#[test]
fn closed_input_rejects_unknown_fields_types_and_unsupported_versions() {
    let mut cases = Vec::new();
    for path in [
        vec!["unexpected"],
        vec!["request", "unexpected"],
        vec!["observation", "unexpected"],
        vec!["observation", "active", "unexpected"],
    ] {
        let mut value = input();
        let mut field = &mut value;
        for key in path {
            field = &mut field[key];
        }
        *field = json!("private input must never be echoed");
        cases.push(serde_json::to_vec(&value).unwrap());
    }
    for (key, invalid) in [("schema_version", json!(2)), ("request", json!("coexist"))] {
        let mut value = input();
        value[key] = invalid;
        cases.push(serde_json::to_vec(&value).unwrap());
    }
    let mut zero = input();
    zero["observation"]["observed_at_unix_millis"] = json!(0);
    cases.push(serde_json::to_vec(&zero).unwrap());
    let mut lease = input();
    lease["observation"]["quiet_lease"] = json!({"generation": 0, "expires_at_unix_millis": 2000});
    cases.push(serde_json::to_vec(&lease).unwrap());
    let valid = serde_json::to_vec(&input()).unwrap();
    let mut duplicate = b"{\"schema_version\":1,".to_vec();
    duplicate.extend_from_slice(&valid[1..]);
    cases.push(duplicate);
    let mut invalid_enum = input();
    invalid_enum["observation"]["pressure"] = json!("caller_says_safe");
    cases.push(serde_json::to_vec(&invalid_enum).unwrap());
    cases.extend([
        b"{\"schema_version\":1,\"schema_version\":1}".to_vec(),
        b"[]".to_vec(),
        vec![255],
        vec![b'x'; 5000],
    ]);
    for raw in cases {
        let (success, result) = run(&raw, &[]);
        assert!(!success);
        assert_eq!(result["document_type"], "glaeda-local-admission-error");
        assert!(!result.to_string().contains("private input"));
    }
    assert!(!run(&[], &["--execute"]).0);
}

#[test]
fn input_ceiling_is_enforced_on_otherwise_valid_json() {
    let mut raw = serde_json::to_vec(&input()).unwrap();
    raw.resize(4096, b' ');
    assert!(run(&raw, &[]).0);
    raw.push(b' ');
    let (success, result) = run(&raw, &[]);
    assert!(!success);
    assert_eq!(result["code"], "input_too_large");
}
