//! Local policy bridge for checked-in owned-execution adapters. No physical effects.
use std::io::{self, Read};
use std::process::ExitCode;

use glaeda::local_interference_admission::{
    LocalInterferenceObservation, LocalInterferenceRequest, compile_local_interference_admission,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};

const MAX_INPUT_BYTES: u64 = 4096;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    schema_version: u8,
    request: LocalInterferenceRequest,
    observation: LocalInterferenceObservation,
}

fn refusal(code: &str) -> ExitCode {
    println!(
        "{}",
        serde_json::json!({
            "document_type": "glaeda-local-admission-error", "schema_version": 1,
            "code": code, "authorizes_execution": false, "grants_authority": false
        })
    );
    ExitCode::from(2)
}

fn main() -> ExitCode {
    // Observations arrive only over stdin; no host paths or mutable policy flags.
    if std::env::args_os().len() != 1 {
        return refusal("unexpected_arguments");
    }
    let mut raw = Vec::new();
    if io::stdin()
        .take(MAX_INPUT_BYTES + 1)
        .read_to_end(&mut raw)
        .is_err()
    {
        return refusal("input_unavailable");
    }
    if raw.len() as u64 > MAX_INPUT_BYTES {
        return refusal("input_too_large");
    }
    let Ok(input) = serde_json::from_slice::<Input>(&raw) else {
        return refusal("invalid_input");
    };
    if input.schema_version != 1
        || input.observation.observed_at_unix_millis == 0
        || input
            .observation
            .quiet_lease
            .is_some_and(|lease| lease.generation == 0)
    {
        return refusal("invalid_input");
    }
    let decision = compile_local_interference_admission(input.request, input.observation);
    println!(
        "{}",
        serde_json::json!({
            "document_type": "glaeda-local-admission-decision", "schema_version": 1,
            "input_sha256": format!("sha256:{:x}", Sha256::digest(&raw)),
            "decision": decision,
            "authorizes_execution": false, "grants_authority": false
        })
    );
    ExitCode::SUCCESS
}
