use std::path::Path;

use glaeda::artifact::Sha256Digest;
use glaeda::disposable_launchd_service::{
    DisposableLaunchdServiceDesiredState, plan_disposable_launchd_service,
};

fn digest(character: char) -> Sha256Digest {
    Sha256Digest::parse(&format!("sha256:{}", character.to_string().repeat(64))).unwrap()
}

#[test]
fn launch_agent_internal_paths_are_refused_for_program_and_enrollment() {
    for reserved in [
        "/Users/operator/Library/LaunchAgents/io.smolrunner.disposable-worker.plist",
        "/Users/operator/Library/LaunchAgents/.io.smolrunner.disposable-worker.apply.lock",
        "/Users/operator/Library/LaunchAgents/.io.smolrunner.disposable-worker.plist.next.0123456789abcdef",
    ] {
        for (program, enrollment) in [
            (
                reserved,
                "/Users/operator/.config/smolrunner/enrollment.json",
            ),
            ("/opt/smolrunner/bin/smolrunner", reserved),
        ] {
            assert!(
                plan_disposable_launchd_service(
                    DisposableLaunchdServiceDesiredState::Installed,
                    501,
                    Path::new("/Users/operator"),
                    Path::new(program),
                    &digest('a'),
                    Path::new(enrollment),
                    &digest('b'),
                )
                .is_err(),
                "accepted internal path collision {reserved:?}"
            );
        }
    }
}
