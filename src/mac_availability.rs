use serde::Serialize;

pub const MAC_AVAILABILITY_SCHEMA_VERSION: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AvailabilityRequest {
    Active,
    Away,
    Off,
    Auto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectiveAvailabilityMode {
    Active,
    Away,
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VmPowerState {
    Running,
    Stopped,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JobActivity {
    Idle,
    Active,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationFreshness {
    Fresh,
    Stale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostPowerSource {
    Ac,
    Battery,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryPressure {
    Normal,
    Elevated,
    Critical,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct MacVmProfile {
    pub cpus: u8,
    pub memory_mib: u32,
    pub max_concurrent_jobs: u8,
}

pub const ACTIVE_PROFILE: MacVmProfile = MacVmProfile {
    cpus: 4,
    memory_mib: 3 * 1024,
    max_concurrent_jobs: 1,
};

pub const AWAY_PROFILE: MacVmProfile = MacVmProfile {
    cpus: 8,
    memory_mib: 10 * 1024,
    max_concurrent_jobs: 1,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MacAvailabilityObservation {
    pub effective_mode: EffectiveAvailabilityMode,
    pub vm_power: VmPowerState,
    pub job_activity: JobActivity,
    pub freshness: ObservationFreshness,
    pub host_power: HostPowerSource,
    pub memory_pressure: MemoryPressure,
    pub operator_hold: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AvailabilityDisposition {
    NoChange,
    Ready,
    Blocked,
    ManualPolicyRequired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AvailabilityBlockerKind {
    ActiveJob,
    AutoPolicyUnavailable,
    BatteryPower,
    CriticalMemoryPressure,
    ElevatedMemoryPressure,
    InconsistentEffectiveState,
    OperatorHold,
    StaleObservation,
    UnknownJobActivity,
    UnknownMemoryPressure,
    UnknownPowerSource,
    UnknownVmPower,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AvailabilityBlocker {
    pub kind: AvailabilityBlockerKind,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AvailabilityActionKind {
    ApplyProfile,
    DrainRunner,
    StartVm,
    StopVm,
    VerifyTransition,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AvailabilityAction {
    pub kind: AvailabilityActionKind,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MacAvailabilityPlan {
    pub schema_version: u8,
    pub effective_mode: EffectiveAvailabilityMode,
    pub requested_mode: AvailabilityRequest,
    pub disposition: AvailabilityDisposition,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_profile: Option<MacVmProfile>,
    pub actions: Vec<AvailabilityAction>,
    pub blockers: Vec<AvailabilityBlocker>,
}

#[must_use]
pub fn plan_availability_transition(
    observation: MacAvailabilityObservation,
    requested_mode: AvailabilityRequest,
) -> MacAvailabilityPlan {
    let target_profile = requested_profile(requested_mode);
    let mut blockers = Vec::new();

    if observation.freshness == ObservationFreshness::Stale {
        blockers.push(blocker(
            AvailabilityBlockerKind::StaleObservation,
            "availability evidence is stale; refresh host and job observations before planning a transition",
        ));
    }
    if observation.operator_hold {
        blockers.push(blocker(
            AvailabilityBlockerKind::OperatorHold,
            "an operator hold blocks availability transitions",
        ));
    }
    if observation.vm_power == VmPowerState::Unknown {
        blockers.push(blocker(
            AvailabilityBlockerKind::UnknownVmPower,
            "the Lima VM power state is unknown",
        ));
    }
    if !effective_state_is_consistent(observation.effective_mode, observation.vm_power) {
        blockers.push(blocker(
            AvailabilityBlockerKind::InconsistentEffectiveState,
            "the effective availability mode conflicts with the observed VM power state",
        ));
    }

    classify_off_job_consistency(observation, &mut blockers);

    if requested_mode == AvailabilityRequest::Auto {
        if blockers.is_empty() {
            blockers.push(blocker(
                AvailabilityBlockerKind::AutoPolicyUnavailable,
                "auto mode requires reviewed idle, power, memory, dwell-time, and job-drain policy that is not implemented yet",
            ));
            return plan(
                observation.effective_mode,
                requested_mode,
                AvailabilityDisposition::ManualPolicyRequired,
                target_profile,
                Vec::new(),
                blockers,
            );
        }
        return blocked_plan(
            observation.effective_mode,
            requested_mode,
            target_profile,
            blockers,
        );
    }

    if request_matches_effective(requested_mode, observation.effective_mode) && blockers.is_empty()
    {
        return plan(
            observation.effective_mode,
            requested_mode,
            AvailabilityDisposition::NoChange,
            target_profile,
            Vec::new(),
            Vec::new(),
        );
    }

    if requested_mode != AvailabilityRequest::Off {
        classify_start_capacity(observation, requested_mode, &mut blockers);
    }
    classify_job_barrier(observation, requested_mode, &mut blockers);

    if !blockers.is_empty() {
        return blocked_plan(
            observation.effective_mode,
            requested_mode,
            target_profile,
            blockers,
        );
    }

    plan(
        observation.effective_mode,
        requested_mode,
        AvailabilityDisposition::Ready,
        target_profile,
        transition_actions(observation.effective_mode, requested_mode),
        Vec::new(),
    )
}

#[must_use]
pub fn render_human(plan: &MacAvailabilityPlan) -> String {
    let mut output = format!(
        "Mac availability transition: {:?} -> {:?}\nDisposition: {:?}\n",
        plan.effective_mode, plan.requested_mode, plan.disposition
    );

    if let Some(profile) = plan.target_profile {
        output.push_str(&format!(
            "Target profile: {} CPU, {} MiB RAM, {} concurrent job(s)\n",
            profile.cpus, profile.memory_mib, profile.max_concurrent_jobs
        ));
    }
    if !plan.actions.is_empty() {
        output.push_str("Actions:\n");
        for action in &plan.actions {
            output.push_str(&format!("- {}\n", action.summary));
        }
    }
    if !plan.blockers.is_empty() {
        output.push_str("Blockers:\n");
        for blocker in &plan.blockers {
            output.push_str(&format!("- {}\n", blocker.message));
        }
    }
    output
}

fn classify_off_job_consistency(
    observation: MacAvailabilityObservation,
    blockers: &mut Vec<AvailabilityBlocker>,
) {
    if observation.effective_mode != EffectiveAvailabilityMode::Off {
        return;
    }
    match observation.job_activity {
        JobActivity::Idle => {}
        JobActivity::Active => blockers.push(blocker(
            AvailabilityBlockerKind::InconsistentEffectiveState,
            "the VM is off but runner job activity is reported as active",
        )),
        JobActivity::Unknown => blockers.push(blocker(
            AvailabilityBlockerKind::UnknownJobActivity,
            "the VM is off but runner job activity is unknown; prove the listener idle",
        )),
    }
}

fn classify_start_capacity(
    observation: MacAvailabilityObservation,
    requested_mode: AvailabilityRequest,
    blockers: &mut Vec<AvailabilityBlocker>,
) {
    if requested_mode == AvailabilityRequest::Away {
        match observation.host_power {
            HostPowerSource::Ac => {}
            HostPowerSource::Battery => blockers.push(blocker(
                AvailabilityBlockerKind::BatteryPower,
                "away mode requires AC power in the initial policy",
            )),
            HostPowerSource::Unknown => blockers.push(blocker(
                AvailabilityBlockerKind::UnknownPowerSource,
                "away mode requires a known host power source",
            )),
        }
        match observation.memory_pressure {
            MemoryPressure::Normal => {}
            MemoryPressure::Elevated => blockers.push(blocker(
                AvailabilityBlockerKind::ElevatedMemoryPressure,
                "away mode is blocked while macOS memory pressure is elevated",
            )),
            MemoryPressure::Critical => blockers.push(blocker(
                AvailabilityBlockerKind::CriticalMemoryPressure,
                "away mode is blocked while macOS memory pressure is critical",
            )),
            MemoryPressure::Unknown => blockers.push(blocker(
                AvailabilityBlockerKind::UnknownMemoryPressure,
                "away mode requires a known memory-pressure observation",
            )),
        }
    } else if matches!(
        observation.memory_pressure,
        MemoryPressure::Critical | MemoryPressure::Unknown
    ) {
        let (kind, message) = match observation.memory_pressure {
            MemoryPressure::Critical => (
                AvailabilityBlockerKind::CriticalMemoryPressure,
                "starting the interactive VM is blocked while macOS memory pressure is critical",
            ),
            MemoryPressure::Unknown => (
                AvailabilityBlockerKind::UnknownMemoryPressure,
                "starting the interactive VM requires a known memory-pressure observation",
            ),
            MemoryPressure::Normal | MemoryPressure::Elevated => unreachable!(),
        };
        blockers.push(blocker(kind, message));
    }
}

fn classify_job_barrier(
    observation: MacAvailabilityObservation,
    requested_mode: AvailabilityRequest,
    blockers: &mut Vec<AvailabilityBlocker>,
) {
    if observation.effective_mode == EffectiveAvailabilityMode::Off
        || request_matches_effective(requested_mode, observation.effective_mode)
    {
        return;
    }
    match observation.job_activity {
        JobActivity::Idle => {}
        JobActivity::Active => blockers.push(blocker(
            AvailabilityBlockerKind::ActiveJob,
            "an active runner job blocks VM stop, restart, or profile replacement",
        )),
        JobActivity::Unknown => blockers.push(blocker(
            AvailabilityBlockerKind::UnknownJobActivity,
            "runner job activity is unknown; prove the listener idle before changing availability",
        )),
    }
}

fn transition_actions(
    effective_mode: EffectiveAvailabilityMode,
    requested_mode: AvailabilityRequest,
) -> Vec<AvailabilityAction> {
    match (effective_mode, requested_mode) {
        (EffectiveAvailabilityMode::Off, AvailabilityRequest::Active) => vec![
            action(
                AvailabilityActionKind::ApplyProfile,
                "apply the interactive Lima profile",
            ),
            action(AvailabilityActionKind::StartVm, "start the Lima VM"),
            action(
                AvailabilityActionKind::VerifyTransition,
                "freshly verify VM profile and runner admission state",
            ),
        ],
        (EffectiveAvailabilityMode::Off, AvailabilityRequest::Away) => vec![
            action(
                AvailabilityActionKind::ApplyProfile,
                "apply the away Lima profile",
            ),
            action(AvailabilityActionKind::StartVm, "start the Lima VM"),
            action(
                AvailabilityActionKind::VerifyTransition,
                "freshly verify VM profile and runner admission state",
            ),
        ],
        (
            EffectiveAvailabilityMode::Active | EffectiveAvailabilityMode::Away,
            AvailabilityRequest::Off,
        ) => vec![
            action(
                AvailabilityActionKind::DrainRunner,
                "drain the runner and prove no job is active",
            ),
            action(AvailabilityActionKind::StopVm, "stop the Lima VM"),
            action(
                AvailabilityActionKind::VerifyTransition,
                "freshly verify the VM is stopped and admission remains disabled",
            ),
        ],
        (EffectiveAvailabilityMode::Active, AvailabilityRequest::Away) => vec![
            action(
                AvailabilityActionKind::DrainRunner,
                "drain the runner and prove no job is active",
            ),
            action(
                AvailabilityActionKind::StopVm,
                "stop the Lima VM before changing its resource profile",
            ),
            action(
                AvailabilityActionKind::ApplyProfile,
                "apply the away Lima profile",
            ),
            action(AvailabilityActionKind::StartVm, "start the Lima VM"),
            action(
                AvailabilityActionKind::VerifyTransition,
                "freshly verify VM profile and runner admission state",
            ),
        ],
        (EffectiveAvailabilityMode::Away, AvailabilityRequest::Active) => vec![
            action(
                AvailabilityActionKind::DrainRunner,
                "drain the runner and prove no job is active",
            ),
            action(
                AvailabilityActionKind::StopVm,
                "stop the Lima VM before changing its resource profile",
            ),
            action(
                AvailabilityActionKind::ApplyProfile,
                "apply the interactive Lima profile",
            ),
            action(AvailabilityActionKind::StartVm, "start the Lima VM"),
            action(
                AvailabilityActionKind::VerifyTransition,
                "freshly verify VM profile and runner admission state",
            ),
        ],
        (_, AvailabilityRequest::Auto)
        | (EffectiveAvailabilityMode::Active, AvailabilityRequest::Active)
        | (EffectiveAvailabilityMode::Away, AvailabilityRequest::Away)
        | (EffectiveAvailabilityMode::Off, AvailabilityRequest::Off) => Vec::new(),
    }
}

const fn requested_profile(requested_mode: AvailabilityRequest) -> Option<MacVmProfile> {
    match requested_mode {
        AvailabilityRequest::Active => Some(ACTIVE_PROFILE),
        AvailabilityRequest::Away => Some(AWAY_PROFILE),
        AvailabilityRequest::Off | AvailabilityRequest::Auto => None,
    }
}

const fn request_matches_effective(
    requested_mode: AvailabilityRequest,
    effective_mode: EffectiveAvailabilityMode,
) -> bool {
    matches!(
        (requested_mode, effective_mode),
        (
            AvailabilityRequest::Active,
            EffectiveAvailabilityMode::Active
        ) | (AvailabilityRequest::Away, EffectiveAvailabilityMode::Away)
            | (AvailabilityRequest::Off, EffectiveAvailabilityMode::Off)
    )
}

const fn effective_state_is_consistent(
    effective_mode: EffectiveAvailabilityMode,
    vm_power: VmPowerState,
) -> bool {
    matches!(
        (effective_mode, vm_power),
        (
            EffectiveAvailabilityMode::Active | EffectiveAvailabilityMode::Away,
            VmPowerState::Running
        ) | (EffectiveAvailabilityMode::Off, VmPowerState::Stopped)
    )
}

fn blocked_plan(
    effective_mode: EffectiveAvailabilityMode,
    requested_mode: AvailabilityRequest,
    target_profile: Option<MacVmProfile>,
    blockers: Vec<AvailabilityBlocker>,
) -> MacAvailabilityPlan {
    plan(
        effective_mode,
        requested_mode,
        AvailabilityDisposition::Blocked,
        target_profile,
        Vec::new(),
        blockers,
    )
}

fn plan(
    effective_mode: EffectiveAvailabilityMode,
    requested_mode: AvailabilityRequest,
    disposition: AvailabilityDisposition,
    target_profile: Option<MacVmProfile>,
    actions: Vec<AvailabilityAction>,
    blockers: Vec<AvailabilityBlocker>,
) -> MacAvailabilityPlan {
    MacAvailabilityPlan {
        schema_version: MAC_AVAILABILITY_SCHEMA_VERSION,
        effective_mode,
        requested_mode,
        disposition,
        target_profile,
        actions,
        blockers,
    }
}

fn blocker(kind: AvailabilityBlockerKind, message: impl Into<String>) -> AvailabilityBlocker {
    AvailabilityBlocker {
        kind,
        message: message.into(),
    }
}

fn action(kind: AvailabilityActionKind, summary: impl Into<String>) -> AvailabilityAction {
    AvailabilityAction {
        kind,
        summary: summary.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AvailabilityActionKind, AvailabilityBlockerKind, AvailabilityDisposition,
        AvailabilityRequest, EffectiveAvailabilityMode, HostPowerSource, JobActivity,
        MacAvailabilityObservation, MemoryPressure, ObservationFreshness, VmPowerState,
        plan_availability_transition,
    };

    fn observation(effective_mode: EffectiveAvailabilityMode) -> MacAvailabilityObservation {
        MacAvailabilityObservation {
            effective_mode,
            vm_power: if effective_mode == EffectiveAvailabilityMode::Off {
                VmPowerState::Stopped
            } else {
                VmPowerState::Running
            },
            job_activity: JobActivity::Idle,
            freshness: ObservationFreshness::Fresh,
            host_power: HostPowerSource::Ac,
            memory_pressure: MemoryPressure::Normal,
            operator_hold: false,
        }
    }

    #[test]
    fn matching_mode_is_a_no_change_plan() {
        let plan = plan_availability_transition(
            observation(EffectiveAvailabilityMode::Active),
            AvailabilityRequest::Active,
        );

        assert_eq!(plan.disposition, AvailabilityDisposition::NoChange);
        assert!(plan.actions.is_empty());
        assert!(plan.blockers.is_empty());
    }

    #[test]
    fn off_idle_matching_request_is_no_change() {
        let plan = plan_availability_transition(
            observation(EffectiveAvailabilityMode::Off),
            AvailabilityRequest::Off,
        );

        assert_eq!(plan.disposition, AvailabilityDisposition::NoChange);
        assert!(plan.blockers.is_empty());
    }

    #[test]
    fn off_active_job_is_inconsistent_and_blocked() {
        let mut facts = observation(EffectiveAvailabilityMode::Off);
        facts.job_activity = JobActivity::Active;

        let plan = plan_availability_transition(facts, AvailabilityRequest::Off);

        assert_eq!(plan.disposition, AvailabilityDisposition::Blocked);
        assert!(plan.blockers.iter().any(|blocker| {
            blocker.kind == AvailabilityBlockerKind::InconsistentEffectiveState
        }));
    }

    #[test]
    fn off_unknown_job_activity_is_explicitly_blocked() {
        let mut facts = observation(EffectiveAvailabilityMode::Off);
        facts.job_activity = JobActivity::Unknown;

        let plan = plan_availability_transition(facts, AvailabilityRequest::Off);

        assert_eq!(plan.disposition, AvailabilityDisposition::Blocked);
        assert!(
            plan.blockers
                .iter()
                .any(|blocker| blocker.kind == AvailabilityBlockerKind::UnknownJobActivity)
        );
    }

    #[test]
    fn matching_running_mode_allows_an_active_job() {
        let mut facts = observation(EffectiveAvailabilityMode::Active);
        facts.job_activity = JobActivity::Active;

        let plan = plan_availability_transition(facts, AvailabilityRequest::Active);

        assert_eq!(plan.disposition, AvailabilityDisposition::NoChange);
        assert!(plan.blockers.is_empty());
    }

    #[test]
    fn active_to_away_requires_drain_restart_and_verification() {
        let plan = plan_availability_transition(
            observation(EffectiveAvailabilityMode::Active),
            AvailabilityRequest::Away,
        );

        assert_eq!(plan.disposition, AvailabilityDisposition::Ready);
        assert_eq!(
            plan.actions
                .iter()
                .map(|action| action.kind)
                .collect::<Vec<_>>(),
            vec![
                AvailabilityActionKind::DrainRunner,
                AvailabilityActionKind::StopVm,
                AvailabilityActionKind::ApplyProfile,
                AvailabilityActionKind::StartVm,
                AvailabilityActionKind::VerifyTransition,
            ]
        );
    }

    #[test]
    fn away_profile_matches_the_accepted_work_envelope() {
        let plan = plan_availability_transition(
            observation(EffectiveAvailabilityMode::Off),
            AvailabilityRequest::Away,
        );
        let profile = plan.target_profile.expect("away mode has a target profile");

        assert_eq!(profile.cpus, 8);
        assert_eq!(profile.memory_mib, 10 * 1024);
        assert_eq!(profile.max_concurrent_jobs, 1);
    }

    #[test]
    fn active_job_blocks_profile_change() {
        let mut facts = observation(EffectiveAvailabilityMode::Active);
        facts.job_activity = JobActivity::Active;

        let plan = plan_availability_transition(facts, AvailabilityRequest::Away);

        assert_eq!(plan.disposition, AvailabilityDisposition::Blocked);
        assert!(plan.actions.is_empty());
        assert!(
            plan.blockers
                .iter()
                .any(|blocker| { blocker.kind == AvailabilityBlockerKind::ActiveJob })
        );
    }

    #[test]
    fn stale_observation_blocks_even_a_matching_request() {
        let mut facts = observation(EffectiveAvailabilityMode::Active);
        facts.freshness = ObservationFreshness::Stale;

        let plan = plan_availability_transition(facts, AvailabilityRequest::Active);

        assert_eq!(plan.disposition, AvailabilityDisposition::Blocked);
        assert!(
            plan.blockers
                .iter()
                .any(|blocker| { blocker.kind == AvailabilityBlockerKind::StaleObservation })
        );
    }

    #[test]
    fn away_requires_ac_and_normal_memory_pressure() {
        let mut facts = observation(EffectiveAvailabilityMode::Off);
        facts.host_power = HostPowerSource::Battery;
        facts.memory_pressure = MemoryPressure::Elevated;

        let plan = plan_availability_transition(facts, AvailabilityRequest::Away);

        assert_eq!(plan.disposition, AvailabilityDisposition::Blocked);
        assert!(
            plan.blockers
                .iter()
                .any(|blocker| { blocker.kind == AvailabilityBlockerKind::BatteryPower })
        );
        assert!(
            plan.blockers
                .iter()
                .any(|blocker| { blocker.kind == AvailabilityBlockerKind::ElevatedMemoryPressure })
        );
    }

    #[test]
    fn off_transition_ignores_capacity_but_requires_idle_job_evidence() {
        let mut facts = observation(EffectiveAvailabilityMode::Away);
        facts.host_power = HostPowerSource::Battery;
        facts.memory_pressure = MemoryPressure::Critical;

        let plan = plan_availability_transition(facts, AvailabilityRequest::Off);

        assert_eq!(plan.disposition, AvailabilityDisposition::Ready);
        assert_eq!(plan.actions[0].kind, AvailabilityActionKind::DrainRunner);
    }

    #[test]
    fn auto_is_explicitly_deferred() {
        let plan = plan_availability_transition(
            observation(EffectiveAvailabilityMode::Active),
            AvailabilityRequest::Auto,
        );

        assert_eq!(
            plan.disposition,
            AvailabilityDisposition::ManualPolicyRequired
        );
        assert!(
            plan.blockers
                .iter()
                .any(|blocker| { blocker.kind == AvailabilityBlockerKind::AutoPolicyUnavailable })
        );
    }

    #[test]
    fn inconsistent_effective_state_fails_closed() {
        let mut facts = observation(EffectiveAvailabilityMode::Active);
        facts.vm_power = VmPowerState::Stopped;

        let plan = plan_availability_transition(facts, AvailabilityRequest::Away);

        assert_eq!(plan.disposition, AvailabilityDisposition::Blocked);
        assert!(plan.blockers.iter().any(|blocker| {
            blocker.kind == AvailabilityBlockerKind::InconsistentEffectiveState
        }));
    }

    #[test]
    fn json_contract_uses_versioned_snake_case_fields() {
        let plan = plan_availability_transition(
            observation(EffectiveAvailabilityMode::Off),
            AvailabilityRequest::Active,
        );
        let json = serde_json::to_value(plan).expect("availability plan serializes");

        assert_eq!(json["schema_version"], 1);
        assert_eq!(json["effective_mode"], "off");
        assert_eq!(json["requested_mode"], "active");
        assert_eq!(json["disposition"], "ready");
        assert_eq!(json["target_profile"]["cpus"], 4);
    }
}
