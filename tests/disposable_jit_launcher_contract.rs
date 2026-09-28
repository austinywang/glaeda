const JIT_LAUNCHER: &str = include_str!("../examples/lima/glaeda-jit-launcher");

#[test]
fn jit_launcher_keeps_workflow_result_out_of_runner_process_exit_semantics() {
    assert!(
        JIT_LAUNCHER.contains("export ACTIONS_RUNNER_INPUT_JITCONFIG=\"${jit_config}\""),
        "the launcher must still hand the exact JIT config to Runner.Listener"
    );
    assert!(
        !JIT_LAUNCHER.contains("ACTIONS_RUNNER_RETURN_JOB_RESULT_FOR_HOSTED"),
        "the self-hosted ephemeral runner must not enable GitHub's internal hosted-result exit mode"
    );
    assert!(
        JIT_LAUNCHER.contains("exec \"${runner_listener}\" run --startuptype service"),
        "the launcher must still replace itself with the one-job Runner.Listener process"
    );
}
