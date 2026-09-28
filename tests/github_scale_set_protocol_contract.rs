use glaeda::github_scale_set_protocol::{
    GITHUB_SCALE_SET_PROTOCOL_SCHEMA_VERSION, ScaleSetJobEvent, ScaleSetJobId, ScaleSetJobResult,
    ScaleSetRunnerId, ScaleSetRunnerName, ScaleSetRunnerReference,
};

fn runner() -> ScaleSetRunnerReference {
    ScaleSetRunnerReference::new(
        ScaleSetRunnerId::new(42).unwrap(),
        ScaleSetRunnerName::parse("smol-attempt-42").unwrap(),
    )
}

#[test]
fn job_identity_preserves_opaque_nonnumeric_wire_values() {
    let job = ScaleSetJobId::parse("job_01JZ9Y6V6F8Q9W7M3N2K1P0ABC").unwrap();
    assert_eq!(job.as_str(), "job_01JZ9Y6V6F8Q9W7M3N2K1P0ABC");
    assert_eq!(
        serde_json::to_string(&job).unwrap(),
        "\"job_01JZ9Y6V6F8Q9W7M3N2K1P0ABC\""
    );
}

#[test]
fn completion_result_is_open_but_bounded() {
    for value in ["succeeded", "canceled", "future-service-result"] {
        let result = ScaleSetJobResult::parse(value).unwrap();
        assert_eq!(result.as_str(), value);
    }

    assert_eq!(
        ScaleSetJobResult::parse("").unwrap_err().code(),
        "invalid_job_result"
    );
    assert_eq!(
        ScaleSetJobResult::parse(" succeeded ").unwrap_err().code(),
        "invalid_job_result"
    );
}

#[test]
fn runner_name_exists_before_service_runner_id() {
    let name = ScaleSetRunnerName::parse("smol-attempt-7").unwrap();
    assert_eq!(name.as_str(), "smol-attempt-7");
    assert_eq!(
        ScaleSetRunnerId::new(0).unwrap_err().code(),
        "invalid_runner_id"
    );
}

#[test]
fn started_event_binds_exact_runner_and_job() {
    let event = ScaleSetJobEvent::Started {
        runner: runner(),
        job_id: ScaleSetJobId::parse("job-7").unwrap(),
    };

    assert_eq!(
        event.schema_version(),
        GITHUB_SCALE_SET_PROTOCOL_SCHEMA_VERSION
    );
    assert_eq!(event.runner().unwrap().id.get(), 42);
    assert_eq!(event.runner().unwrap().name.as_str(), "smol-attempt-42");
    assert_eq!(event.job_id().as_str(), "job-7");
}

#[test]
fn completion_can_exist_without_an_assigned_runner() {
    let event = ScaleSetJobEvent::Completed {
        runner: None,
        job_id: ScaleSetJobId::parse("job-cancelled-before-start").unwrap(),
        result: ScaleSetJobResult::parse("canceled").unwrap(),
    };

    assert!(event.runner().is_none());
    assert_eq!(event.job_id().as_str(), "job-cancelled-before-start");

    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("job-cancelled-before-start"));
    assert!(json.contains("canceled"));
    assert!(!json.contains("runner"));
}

#[test]
fn protocol_values_reject_unbounded_or_control_bearing_input() {
    let oversized_job = "j".repeat(257);
    assert_eq!(
        ScaleSetJobId::parse(&oversized_job).unwrap_err().code(),
        "invalid_job_id"
    );
    assert_eq!(
        ScaleSetJobId::parse("job\n7").unwrap_err().code(),
        "invalid_job_id"
    );
    assert_eq!(
        ScaleSetRunnerName::parse("Smol Runner").unwrap_err().code(),
        "invalid_runner_name"
    );
}
