use std::collections::BTreeSet;
use std::io::{self, Read};
use std::process::ExitCode;

use clap::{Parser, ValueEnum};
use serde::{Deserialize, Serialize};

const DOCUMENT_TYPE: &str = "glaeda-owned-agent-presence";
const ERROR_DOCUMENT_TYPE: &str = "glaeda-owned-agent-presence-error";
const SCHEMA_VERSION: u8 = 1;
const AUTHORITY: &str = "observation_only";
const MAX_INPUT_BYTES: usize = 128 * 1024;
const MAX_OUTPUT_BYTES: usize = 128 * 1024;
const MAX_WORKERS: usize = 64;
const MAX_UNKNOWN_REASONS: usize = 8;
const MAX_REFERENCE_BYTES: usize = 160;
const MAX_REPOSITORY_BYTES: usize = 200;
const MAX_CHANGED_PATHS: u32 = 100_000;
const MAX_PROCESS_COUNT: u32 = 100_000;
const MAX_AGE_SECONDS: u64 = 31 * 24 * 60 * 60;
const MAX_RSS_BYTES: u64 = 1 << 50;

#[derive(Debug, Parser)]
#[command(
    name = "glaeda-owned-agent-presence",
    about = "Normalize bounded local worker presence without reading conversation content"
)]
struct Cli {
    #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
    output: OutputFormat,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum OutputFormat {
    Human,
    Json,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct PresenceInput {
    schema_version: u8,
    observed_at_unix_ms: u64,
    workers: Vec<WorkerObservation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct PresenceReport {
    document_type: &'static str,
    schema_version: u8,
    authority: &'static str,
    observed_at_unix_ms: u64,
    workers: Vec<WorkerObservation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkerObservation {
    worker_id: String,
    node_id: String,
    node_generation: u64,
    harness: HarnessClass,
    runtime_state: RuntimeState,
    work: WorkBinding,
    source: Option<SourceEvidence>,
    freshness: FreshnessClass,
    last_activity: Option<TimedActivity>,
    last_result: Option<TimedResult>,
    process: ProcessSummary,
    #[serde(default)]
    unknown_reasons: Vec<UnknownReason>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum HarnessClass {
    Codex,
    Pi,
    ClaudeCode,
    GeminiCli,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RuntimeState {
    Active,
    Waiting,
    Blocked,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "class", rename_all = "snake_case", deny_unknown_fields)]
enum WorkBinding {
    Managed {
        repository: String,
        project_ref: String,
        work_ref: String,
        run_ref: String,
        generation: u64,
    },
    UnboundLocalWork,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceEvidence {
    repository: String,
    commit_oid: String,
    tree_oid: String,
    branch_state: BranchState,
    worktree_state: WorktreeState,
    changed_path_count: Option<u32>,
    jj_change_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum BranchState {
    Attached,
    Detached,
    Unborn,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WorktreeState {
    Clean,
    Dirty,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FreshnessClass {
    Fresh,
    Stale,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TimedActivity {
    class: ActivityClass,
    age_seconds: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ActivityClass {
    ToolUse,
    ModelResponse,
    Verification,
    Handoff,
    Heartbeat,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TimedResult {
    class: ResultClass,
    age_seconds: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ResultClass {
    Passed,
    Failed,
    Blocked,
    Cancelled,
    Ambiguous,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProcessSummary {
    scope_class: ProcessScopeClass,
    process_count: u32,
    rss_bytes: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ProcessScopeClass {
    ManagedRoute,
    ManagedRunner,
    UnboundSameUser,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum UnknownReason {
    WorkIdentityUnavailable,
    SourceUnavailable,
    RuntimeStateUnavailable,
    ActivityUnavailable,
    ProcessScopeUnavailable,
    EvidenceStale,
    FreshnessUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ProjectionErrorKind {
    InvalidInput,
    InvalidWorker,
    DuplicateWorker,
    ContradictoryEvidence,
    TooManyWorkers,
    OutputTooLarge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
struct ProjectionError {
    kind: ProjectionErrorKind,
    code: &'static str,
    problem: &'static str,
}

impl ProjectionError {
    const fn new(kind: ProjectionErrorKind, code: &'static str, problem: &'static str) -> Self {
        Self {
            kind,
            code,
            problem,
        }
    }
}

#[derive(Debug, Serialize)]
struct ErrorReport<'a> {
    document_type: &'static str,
    schema_version: u8,
    authority: &'static str,
    error: &'a ProjectionError,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match read_and_project() {
        Ok(report) => emit_report(cli.output, report),
        Err(error) => emit_error(cli.output, &error),
    }
}

fn read_and_project() -> Result<PresenceReport, ProjectionError> {
    let mut bytes = Vec::new();
    io::stdin()
        .take((MAX_INPUT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid_input("presence input could not be read"))?;
    if bytes.len() > MAX_INPUT_BYTES {
        return Err(invalid_input("presence input exceeds the byte limit"));
    }
    let input: PresenceInput = serde_json::from_slice(&bytes)
        .map_err(|_| invalid_input("presence input is not valid schema JSON"))?;
    project_presence(input)
}

fn project_presence(input: PresenceInput) -> Result<PresenceReport, ProjectionError> {
    if input.schema_version != SCHEMA_VERSION || input.observed_at_unix_ms == 0 {
        return Err(invalid_input("presence envelope identity is invalid"));
    }
    if input.workers.len() > MAX_WORKERS {
        return Err(ProjectionError::new(
            ProjectionErrorKind::TooManyWorkers,
            "owned_agent_presence_too_many_workers",
            "presence input contains too many workers",
        ));
    }

    let mut worker_ids = BTreeSet::new();
    let mut workers = Vec::with_capacity(input.workers.len());
    for mut worker in input.workers {
        normalize_worker(&mut worker)?;
        if !worker_ids.insert(worker.worker_id.clone()) {
            return Err(ProjectionError::new(
                ProjectionErrorKind::DuplicateWorker,
                "owned_agent_presence_duplicate_worker",
                "presence input repeats one worker identity",
            ));
        }
        workers.push(worker);
    }
    workers.sort_by(|left, right| {
        (&left.node_id, &left.worker_id).cmp(&(&right.node_id, &right.worker_id))
    });

    let report = PresenceReport {
        document_type: DOCUMENT_TYPE,
        schema_version: SCHEMA_VERSION,
        authority: AUTHORITY,
        observed_at_unix_ms: input.observed_at_unix_ms,
        workers,
    };
    let encoded = serde_json::to_vec(&report).map_err(|_| {
        ProjectionError::new(
            ProjectionErrorKind::OutputTooLarge,
            "owned_agent_presence_encode_failed",
            "presence report could not be encoded",
        )
    })?;
    if encoded.len() > MAX_OUTPUT_BYTES {
        return Err(ProjectionError::new(
            ProjectionErrorKind::OutputTooLarge,
            "owned_agent_presence_output_too_large",
            "presence report exceeds the byte limit",
        ));
    }
    Ok(report)
}

fn normalize_worker(worker: &mut WorkerObservation) -> Result<(), ProjectionError> {
    if !valid_worker_id(&worker.worker_id)
        || !valid_node_id(&worker.node_id)
        || worker.node_generation == 0
    {
        return Err(invalid_worker("worker or node identity is invalid"));
    }

    let managed_repository = match &mut worker.work {
        WorkBinding::Managed {
            repository,
            project_ref,
            work_ref,
            run_ref,
            generation,
        } => {
            if *generation == 0
                || !valid_reference(project_ref)
                || !valid_reference(work_ref)
                || !valid_reference(run_ref)
            {
                return Err(invalid_worker("managed work identity is invalid"));
            }
            *repository = canonical_repository(repository)?;
            Some(repository.clone())
        }
        WorkBinding::UnboundLocalWork | WorkBinding::Unknown => None,
    };

    if let Some(source) = &mut worker.source {
        source.repository = canonical_repository(&source.repository)?;
        if !lower_hex(&source.commit_oid, 40) || !lower_hex(&source.tree_oid, 40) {
            return Err(invalid_worker("Git source identity is invalid"));
        }
        if let Some(change_id) = &source.jj_change_id
            && !valid_jj_change_id(change_id)
        {
            return Err(invalid_worker("JJ change identity is invalid"));
        }
        match (source.worktree_state, source.changed_path_count) {
            (WorktreeState::Clean, Some(0)) | (WorktreeState::Unknown, None) => {}
            (WorktreeState::Dirty, Some(count)) if (1..=MAX_CHANGED_PATHS).contains(&count) => {}
            _ => return Err(invalid_worker("working-copy evidence is contradictory")),
        }
        if let Some(repository) = &managed_repository
            && source.repository != *repository
        {
            return Err(ProjectionError::new(
                ProjectionErrorKind::ContradictoryEvidence,
                "owned_agent_presence_work_source_mismatch",
                "managed work and Git source name different repositories",
            ));
        }
    }

    if let Some(activity) = &worker.last_activity
        && activity.age_seconds > MAX_AGE_SECONDS
    {
        return Err(invalid_worker("activity age exceeds the supported bound"));
    }
    if let Some(result) = &worker.last_result
        && result.age_seconds > MAX_AGE_SECONDS
    {
        return Err(invalid_worker("result age exceeds the supported bound"));
    }
    if worker.process.process_count > MAX_PROCESS_COUNT
        || worker
            .process
            .rss_bytes
            .is_some_and(|value| value > MAX_RSS_BYTES)
    {
        return Err(invalid_worker(
            "process summary exceeds the supported bound",
        ));
    }
    if worker.process.process_count == 0 && worker.process.rss_bytes.is_some_and(|value| value != 0)
    {
        return Err(invalid_worker("process count and memory evidence disagree"));
    }

    let mut reasons: BTreeSet<UnknownReason> = worker.unknown_reasons.iter().copied().collect();
    match worker.work {
        WorkBinding::Unknown => {
            reasons.insert(UnknownReason::WorkIdentityUnavailable);
        }
        WorkBinding::Managed { .. } | WorkBinding::UnboundLocalWork => {}
    }
    if worker.source.is_none() {
        reasons.insert(UnknownReason::SourceUnavailable);
    }
    if worker.runtime_state == RuntimeState::Unknown {
        reasons.insert(UnknownReason::RuntimeStateUnavailable);
    }
    if worker.last_activity.is_none() {
        reasons.insert(UnknownReason::ActivityUnavailable);
    }
    if worker.process.scope_class == ProcessScopeClass::Unknown {
        reasons.insert(UnknownReason::ProcessScopeUnavailable);
    }
    match worker.freshness {
        FreshnessClass::Fresh => {}
        FreshnessClass::Stale => {
            reasons.insert(UnknownReason::EvidenceStale);
        }
        FreshnessClass::Unknown => {
            reasons.insert(UnknownReason::FreshnessUnavailable);
        }
    }
    if reasons.len() > MAX_UNKNOWN_REASONS {
        return Err(invalid_worker("worker has too many unknown-reason classes"));
    }
    worker.unknown_reasons = reasons.into_iter().collect();
    Ok(())
}

fn canonical_repository(value: &str) -> Result<String, ProjectionError> {
    if value.len() > MAX_REPOSITORY_BYTES {
        return Err(invalid_worker(
            "repository identity exceeds the supported bound",
        ));
    }
    let mut parts = value.split('/');
    let owner = parts.next().unwrap_or_default();
    let repository = parts.next().unwrap_or_default();
    if parts.next().is_some() || !valid_repo_component(owner) || !valid_repo_component(repository) {
        return Err(invalid_worker(
            "repository identity is not canonical owner/name",
        ));
    }
    Ok(format!(
        "{}/{}",
        owner.to_ascii_lowercase(),
        repository.to_ascii_lowercase()
    ))
}

fn valid_repo_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn valid_reference(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_REFERENCE_BYTES
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'.' | b'_' | b'-' | b':' | b'/' | b'#' | b'@' | b'+')
        })
}

fn valid_worker_id(value: &str) -> bool {
    value
        .strip_prefix("sha256:")
        .is_some_and(|digest| lower_hex(digest, 64))
}

fn valid_node_id(value: &str) -> bool {
    (2..=64).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && value
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric)
}

fn valid_jj_change_id(value: &str) -> bool {
    (1..=128).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
}

fn lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn invalid_input(problem: &'static str) -> ProjectionError {
    ProjectionError::new(
        ProjectionErrorKind::InvalidInput,
        "owned_agent_presence_invalid_input",
        problem,
    )
}

fn invalid_worker(problem: &'static str) -> ProjectionError {
    ProjectionError::new(
        ProjectionErrorKind::InvalidWorker,
        "owned_agent_presence_invalid_worker",
        problem,
    )
}

fn emit_report(output: OutputFormat, report: PresenceReport) -> ExitCode {
    match output {
        OutputFormat::Json => match serde_json::to_string(&report) {
            Ok(json) => println!("{json}"),
            Err(_) => {
                eprintln!("presence report could not be encoded");
                return ExitCode::from(2);
            }
        },
        OutputFormat::Human => {
            println!(
                "owned agent presence: workers={} authority={}",
                report.workers.len(),
                report.authority
            );
            for worker in &report.workers {
                let work = match &worker.work {
                    WorkBinding::Managed { work_ref, .. } => work_ref.as_str(),
                    WorkBinding::UnboundLocalWork => "unbound_local_work",
                    WorkBinding::Unknown => "unknown",
                };
                let repository = worker
                    .source
                    .as_ref()
                    .map_or("unknown", |source| source.repository.as_str());
                println!(
                    "{} {} {:?} {:?} work={} repository={} unknowns={}",
                    worker.node_id,
                    worker.worker_id,
                    worker.harness,
                    worker.runtime_state,
                    work,
                    repository,
                    worker.unknown_reasons.len()
                );
            }
        }
    }
    ExitCode::SUCCESS
}

fn emit_error(output: OutputFormat, error: &ProjectionError) -> ExitCode {
    match output {
        OutputFormat::Json => {
            let report = ErrorReport {
                document_type: ERROR_DOCUMENT_TYPE,
                schema_version: SCHEMA_VERSION,
                authority: AUTHORITY,
                error,
            };
            match serde_json::to_string(&report) {
                Ok(json) => eprintln!("{json}"),
                Err(_) => eprintln!("presence error could not be encoded"),
            }
        }
        OutputFormat::Human => eprintln!("presence unavailable: {}", error.problem),
    }
    ExitCode::from(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn managed_worker(worker_hex: char, node: &str) -> WorkerObservation {
        WorkerObservation {
            worker_id: format!("sha256:{}", worker_hex.to_string().repeat(64)),
            node_id: node.to_owned(),
            node_generation: 1,
            harness: HarnessClass::Codex,
            runtime_state: RuntimeState::Active,
            work: WorkBinding::Managed {
                repository: "TeamLeaderLeo/Glaeda".to_owned(),
                project_ref: "stensibly:project:glaeda".to_owned(),
                work_ref: "stensibly:work:991".to_owned(),
                run_ref: "stensibly:run:abc-123".to_owned(),
                generation: 4,
            },
            source: Some(SourceEvidence {
                repository: "teamleaderleo/glaeda".to_owned(),
                commit_oid: "1".repeat(40),
                tree_oid: "2".repeat(40),
                branch_state: BranchState::Attached,
                worktree_state: WorktreeState::Clean,
                changed_path_count: Some(0),
                jj_change_id: None,
            }),
            freshness: FreshnessClass::Fresh,
            last_activity: Some(TimedActivity {
                class: ActivityClass::ToolUse,
                age_seconds: 3,
            }),
            last_result: None,
            process: ProcessSummary {
                scope_class: ProcessScopeClass::ManagedRoute,
                process_count: 4,
                rss_bytes: Some(512 * 1024 * 1024),
            },
            unknown_reasons: vec![],
        }
    }

    fn input(workers: Vec<WorkerObservation>) -> PresenceInput {
        PresenceInput {
            schema_version: SCHEMA_VERSION,
            observed_at_unix_ms: 1_788_200_000_000,
            workers,
        }
    }

    #[test]
    fn preserves_managed_work_and_distinguishes_concurrent_workers() {
        let first = managed_worker('a', "big-red");
        let mut second = managed_worker('b', "air-blue");
        second.harness = HarnessClass::Pi;
        second.work = WorkBinding::UnboundLocalWork;
        second.source.as_mut().unwrap().repository = "teamleaderleo/quarry".to_owned();

        let report = project_presence(input(vec![first, second])).expect("projection");
        assert_eq!(report.workers.len(), 2);
        assert_eq!(report.workers[0].node_id, "air-blue");
        assert_eq!(report.workers[1].node_id, "big-red");
        assert!(matches!(
            report.workers[0].work,
            WorkBinding::UnboundLocalWork
        ));
        match &report.workers[1].work {
            WorkBinding::Managed {
                repository,
                work_ref,
                generation,
                ..
            } => {
                assert_eq!(repository, "teamleaderleo/glaeda");
                assert_eq!(work_ref, "stensibly:work:991");
                assert_eq!(*generation, 4);
            }
            other => panic!("unexpected work binding: {other:?}"),
        }
    }

    #[test]
    fn stale_and_missing_evidence_becomes_explicit_unknown_reasons() {
        let mut worker = managed_worker('a', "big-red");
        worker.work = WorkBinding::Unknown;
        worker.source = None;
        worker.runtime_state = RuntimeState::Unknown;
        worker.freshness = FreshnessClass::Stale;
        worker.last_activity = None;
        worker.process = ProcessSummary {
            scope_class: ProcessScopeClass::Unknown,
            process_count: 0,
            rss_bytes: None,
        };

        let report = project_presence(input(vec![worker])).expect("projection");
        assert_eq!(
            report.workers[0].unknown_reasons,
            vec![
                UnknownReason::WorkIdentityUnavailable,
                UnknownReason::SourceUnavailable,
                UnknownReason::RuntimeStateUnavailable,
                UnknownReason::ActivityUnavailable,
                UnknownReason::ProcessScopeUnavailable,
                UnknownReason::EvidenceStale,
            ]
        );
    }

    #[test]
    fn rejects_duplicate_or_contradictory_worker_evidence() {
        let first = managed_worker('a', "big-red");
        let duplicate = first.clone();
        let error = project_presence(input(vec![first, duplicate])).expect_err("duplicate");
        assert_eq!(error.kind, ProjectionErrorKind::DuplicateWorker);

        let mut mismatch = managed_worker('b', "big-red");
        mismatch.source.as_mut().unwrap().repository = "teamleaderleo/quarry".to_owned();
        let error = project_presence(input(vec![mismatch])).expect_err("mismatch");
        assert_eq!(error.kind, ProjectionErrorKind::ContradictoryEvidence);
    }

    #[test]
    fn rejects_incoherent_worktree_and_resource_counts() {
        let mut worker = managed_worker('a', "big-red");
        let source = worker.source.as_mut().unwrap();
        source.worktree_state = WorktreeState::Dirty;
        source.changed_path_count = Some(0);
        assert!(project_presence(input(vec![worker])).is_err());

        let mut worker = managed_worker('b', "big-red");
        worker.process.process_count = 0;
        worker.process.rss_bytes = Some(1);
        assert!(project_presence(input(vec![worker])).is_err());
    }

    #[test]
    fn serialized_projection_has_no_transcript_process_or_path_fields() {
        let report =
            project_presence(input(vec![managed_worker('a', "big-red")])).expect("projection");
        let json = serde_json::to_string(&report).expect("json");
        for forbidden in [
            "session_id",
            "thread_id",
            "pid",
            "prompt",
            "response",
            "command",
            "/home/",
            "/Users/",
        ] {
            assert!(
                !json.contains(forbidden),
                "unexpected private field: {forbidden}"
            );
        }
        assert!(json.contains("\"authority\":\"observation_only\""));
        assert!(json.contains("\"repository\":\"teamleaderleo/glaeda\""));
    }
}
