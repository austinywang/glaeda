use std::path::Path;
use std::time::Duration;

use glaeda::artifact::Sha256Digest;
use glaeda::disposable_launchd_service::upgrade::{
    CanaryUpgradeError, CanaryUpgradeHost, CanaryUpgradePlan, UpgradeHealthEvidence,
    execute_canary_upgrade,
};
use glaeda::disposable_launchd_service::{
    DisposableLaunchdServiceDesiredState as Desired, DisposableLaunchdServicePlan,
    plan_disposable_launchd_service,
};
use glaeda::durable_journal::{JournalCheckpoint, JournalCheckpointFailure};
use glaeda::journal::{ActionFailure, ActionOutcome, ExecutionJournal};

fn digest(c: char) -> Sha256Digest {
    Sha256Digest::parse(&format!("sha256:{}", c.to_string().repeat(64))).unwrap()
}
fn service(candidate: bool) -> DisposableLaunchdServicePlan {
    plan_disposable_launchd_service(
        Desired::Installed,
        501,
        Path::new("/Users/operator"),
        Path::new(if candidate {
            "/private/candidate/glaeda"
        } else {
            "/private/previous/glaeda"
        }),
        &digest(if candidate { 'b' } else { 'a' }),
        Path::new(if candidate {
            "/private/candidate/enrollment"
        } else {
            "/private/previous/enrollment"
        }),
        &digest('c'),
    )
    .unwrap()
}
fn plan() -> CanaryUpgradePlan {
    CanaryUpgradePlan::new(
        service(false),
        service(true),
        digest('d'),
        Duration::from_secs(30),
    )
    .unwrap()
}
fn failure() -> ActionFailure {
    ActionFailure::public("fixture_failure", "fixture failure")
}
#[derive(Default)]
struct Checkpoint {
    snapshots: Vec<ExecutionJournal>,
    fail_at: Option<usize>,
}
impl JournalCheckpoint for Checkpoint {
    fn checkpoint(&mut self, journal: &ExecutionJournal) -> Result<(), JournalCheckpointFailure> {
        if self.fail_at == Some(self.snapshots.len()) {
            return Err(JournalCheckpointFailure::public(
                "injected checkpoint failure",
            ));
        }
        self.snapshots.push(journal.clone());
        Ok(())
    }
}
struct Host {
    current: Option<bool>,
    events: Vec<String>,
    preflight: bool,
    held: bool,
    candidate_healthy: bool,
    previous_healthy: bool,
    wrong_identity: bool,
    wrong_profile: bool,
    fail_install: bool,
    fail_remove: bool,
    partial_install: bool,
    health_timeout: bool,
}
impl Default for Host {
    fn default() -> Self {
        Self {
            current: Some(false),
            events: vec![],
            preflight: true,
            held: true,
            candidate_healthy: true,
            previous_healthy: true,
            wrong_identity: false,
            wrong_profile: false,
            fail_install: false,
            fail_remove: false,
            partial_install: false,
            health_timeout: false,
        }
    }
}
impl CanaryUpgradeHost for Host {
    fn preflight(&mut self, _: &CanaryUpgradePlan) -> Result<(), ActionFailure> {
        if self.preflight && self.current == Some(false) {
            Ok(())
        } else {
            Err(failure())
        }
    }
    fn before_change(&mut self, _: &CanaryUpgradePlan) -> Result<(), ActionFailure> {
        if self.held { Ok(()) } else { Err(failure()) }
    }
    fn apply(&mut self, plan: &DisposableLaunchdServicePlan) -> Result<(), ActionFailure> {
        let candidate = plan.report().plan_identity() == service(true).report().plan_identity();
        if plan.report().desired_state() == Desired::Installed {
            self.events.push(
                if candidate {
                    "start_candidate"
                } else {
                    "restore_previous"
                }
                .into(),
            );
            if self.current.is_some() {
                return Err(failure());
            }
            if candidate && self.fail_install {
                if self.partial_install {
                    self.current = Some(true);
                }
                return Err(failure());
            }
            self.current = Some(candidate);
        } else {
            // Derive the exact removal identities from their source inputs.
            let candidate_remove = plan_disposable_launchd_service(
                Desired::Removed,
                501,
                Path::new("/Users/operator"),
                Path::new("/private/candidate/glaeda"),
                &digest('b'),
                Path::new("/private/candidate/enrollment"),
                &digest('c'),
            )
            .unwrap();
            let candidate =
                plan.report().plan_identity() == candidate_remove.report().plan_identity();
            self.events.push(
                if candidate {
                    "remove_candidate"
                } else {
                    "stop_previous"
                }
                .into(),
            );
            if candidate && self.fail_remove {
                return Err(failure());
            }
            if self.current.is_some() && self.current != Some(candidate) {
                return Err(failure());
            }
            self.current = None;
        }
        Ok(())
    }
    fn health(
        &mut self,
        plan: &DisposableLaunchdServicePlan,
        profile: &Sha256Digest,
        timeout: Duration,
    ) -> Result<UpgradeHealthEvidence, ActionFailure> {
        let candidate = plan.report().plan_identity() == service(true).report().plan_identity();
        self.events.push(
            if candidate {
                "health_candidate"
            } else {
                "health_previous"
            }
            .into(),
        );
        if candidate && self.health_timeout {
            std::thread::sleep(timeout + Duration::from_millis(2));
        }
        Ok(UpgradeHealthEvidence {
            service_plan: if candidate && self.wrong_identity {
                digest('f')
            } else {
                plan.report().plan_identity().clone()
            },
            health_profile: if candidate && self.wrong_profile {
                digest('f')
            } else {
                profile.clone()
            },
            healthy: self.current == Some(candidate)
                && if candidate {
                    self.candidate_healthy
                } else {
                    self.previous_healthy
                },
        })
    }
}
#[test]
fn healthy_candidate_completes_with_durable_checkpoints() {
    let p = plan();
    let mut h = Host::default();
    let mut c = Checkpoint::default();
    let journal = execute_canary_upgrade(&p, p.identity(), &mut h, &mut c).unwrap();
    assert_eq!(
        h.events,
        ["stop_previous", "start_candidate", "health_candidate"]
    );
    assert_eq!(h.current, Some(true));
    assert!(
        journal
            .records
            .iter()
            .all(|r| r.outcome == ActionOutcome::Completed)
    );
    assert_eq!(c.snapshots.len(), 7);
    assert!(matches!(
        execute_canary_upgrade(&p, p.identity(), &mut h, &mut c),
        Err(CanaryUpgradeError::PreflightRefused)
    ));
    assert_eq!(h.events.len(), 3);
}
#[test]
fn unhealthy_or_misbound_candidate_restores_and_checks_previous() {
    for kind in 0..3 {
        let p = plan();
        let mut h = Host::default();
        let mut c = Checkpoint::default();
        match kind {
            0 => h.candidate_healthy = false,
            1 => h.wrong_identity = true,
            _ => h.wrong_profile = true,
        }
        let journal = execute_canary_upgrade(&p, p.identity(), &mut h, &mut c).unwrap();
        assert_eq!(
            h.events,
            [
                "stop_previous",
                "start_candidate",
                "health_candidate",
                "remove_candidate",
                "restore_previous",
                "health_previous"
            ]
        );
        assert_eq!(h.current, Some(false));
        assert_eq!(journal.records[0].outcome, ActionOutcome::Compensated);
        assert_eq!(journal.records[1].outcome, ActionOutcome::Compensated);
        assert_eq!(journal.records[2].outcome, ActionOutcome::Failed);
    }
}
#[test]
fn partial_candidate_install_is_removed_before_restore() {
    let p = plan();
    let mut h = Host {
        fail_install: true,
        partial_install: true,
        ..Host::default()
    };
    let mut c = Checkpoint::default();
    let journal = execute_canary_upgrade(&p, p.identity(), &mut h, &mut c).unwrap();
    assert_eq!(
        h.events,
        [
            "stop_previous",
            "start_candidate",
            "remove_candidate",
            "restore_previous",
            "health_previous"
        ]
    );
    assert_eq!(h.current, Some(false));
    assert_eq!(journal.records[0].outcome, ActionOutcome::Compensated);
}
#[test]
fn failed_candidate_removal_never_starts_previous_over_it() {
    let p = plan();
    let mut h = Host {
        candidate_healthy: false,
        fail_remove: true,
        ..Host::default()
    };
    let mut c = Checkpoint::default();
    let journal = execute_canary_upgrade(&p, p.identity(), &mut h, &mut c).unwrap();
    assert_eq!(h.current, Some(true));
    assert!(!h.events.iter().any(|s| s == "restore_previous"));
    assert_eq!(journal.records[0].outcome, ActionOutcome::RollbackFailed);
}
#[test]
fn rollback_health_failure_remains_recovery_debt() {
    let p = plan();
    let mut h = Host {
        candidate_healthy: false,
        previous_healthy: false,
        ..Host::default()
    };
    let mut c = Checkpoint::default();
    let journal = execute_canary_upgrade(&p, p.identity(), &mut h, &mut c).unwrap();
    assert_eq!(journal.records[0].outcome, ActionOutcome::RollbackFailed);
}
#[test]
fn approval_preflight_and_ownership_refusals_do_not_mutate() {
    let p = plan();
    let mut h = Host::default();
    let mut c = Checkpoint::default();
    assert!(matches!(
        execute_canary_upgrade(&p, &digest('f'), &mut h, &mut c),
        Err(CanaryUpgradeError::ApprovalMismatch)
    ));
    assert!(h.events.is_empty());
    assert!(c.snapshots.is_empty());
    h.preflight = false;
    assert!(matches!(
        execute_canary_upgrade(&p, p.identity(), &mut h, &mut c),
        Err(CanaryUpgradeError::PreflightRefused)
    ));
    assert!(h.events.is_empty());
    assert!(c.snapshots.is_empty());
    h.preflight = true;
    h.held = false;
    let journal = execute_canary_upgrade(&p, p.identity(), &mut h, &mut c).unwrap();
    assert!(h.events.is_empty());
    assert_eq!(journal.records[0].outcome, ActionOutcome::Failed);
}
#[test]
fn checkpoint_failure_stops_before_first_effect_or_after_stopped_service() {
    for (fail_at, expected) in [(0, 0), (1, 0), (2, 1)] {
        let p = plan();
        let mut h = Host::default();
        let mut c = Checkpoint {
            fail_at: Some(fail_at),
            ..Checkpoint::default()
        };
        assert!(matches!(
            execute_canary_upgrade(&p, p.identity(), &mut h, &mut c),
            Err(CanaryUpgradeError::Journal(_))
        ));
        assert_eq!(h.events.len(), expected);
    }
}
#[test]
fn late_health_causes_rollback_and_plan_identity_binds_health_policy() {
    let p = CanaryUpgradePlan::new(
        service(false),
        service(true),
        digest('d'),
        Duration::from_millis(5),
    )
    .unwrap();
    assert_ne!(p.identity(), plan().identity());
    let mut h = Host {
        health_timeout: true,
        ..Host::default()
    };
    let mut c = Checkpoint::default();
    let journal = execute_canary_upgrade(&p, p.identity(), &mut h, &mut c).unwrap();
    assert_eq!(h.current, Some(false));
    assert_eq!(journal.records[2].outcome, ActionOutcome::Failed);
    assert!(
        CanaryUpgradePlan::new(
            service(false),
            service(false),
            digest('d'),
            Duration::from_secs(30)
        )
        .is_err()
    );
    assert!(
        CanaryUpgradePlan::new(service(false), service(true), digest('d'), Duration::ZERO).is_err()
    );
    assert!(
        CanaryUpgradePlan::new(
            service(false),
            service(true),
            digest('d'),
            Duration::from_secs(301)
        )
        .is_err()
    );
}
