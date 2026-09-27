#[cfg(not(unix))]
compile_error!("glaeda-worktree-reclaim requires a Unix host");

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::{Parser, ValueEnum};
use glaeda::linked_worktree_reclaim::{
    GithubLookup, LINKED_WORKTREE_GIT_TIMEOUT, LinkedWorktreeFacts,
    LinkedWorktreeReclaimCompensation, LinkedWorktreeReclaimDecision, LinkedWorktreeReclaimOutcome,
    LinkedWorktreeReclaimPolicy, LinkedWorktreeReclaimVeto, LocalBranchDecision,
    LocalBranchDeletion, LocalBranchFinishedEvidence, LocalBranchReport, ProcessUseEvidence,
    landed_worktree_tips, list_linked_worktrees, observe_linked_worktrees,
    plan_linked_worktree_reclaim, reclaim_linked_worktree, reclaim_local_branches,
};
use glaeda::process::ProcessExecutor;
use glaeda::project_checkout_observation::ProjectCheckoutObserver;
use serde::Serialize;

const REPORT_SCHEMA_VERSION: u8 = 1;
const GIT_PROGRAM: &str = "/usr/bin/git";
const MAX_REPOSITORIES: usize = 32;

/// Default idle window for unfinished work: three days.
///
/// Every eligible worktree is already clean and preserved, so the window only weighs disruption:
/// someone may come back to unfinished work, and a worktree a process is working in is refused
/// outright however old its Git activity looks. Under disk pressure a caller passes a shorter one.
const DEFAULT_IDLE_SECONDS: i64 = 3 * 24 * 60 * 60;

/// Default idle window for finished work (landed in a remote default branch, squash-merged, or
/// upstream deleted): an hour, the policy floor. Nobody returns to it, so it is only clutter.
const DEFAULT_FINISHED_IDLE_SECONDS: i64 = 60 * 60;

/// Default number of worktrees one run may remove. A budget keeps an unattended run bounded even
/// if every observation is wrong in the same way.
const DEFAULT_MAX_RECLAIMS: usize = 32;

/// Default number of local branches one run may delete across all repositories.
const DEFAULT_MAX_BRANCH_DELETIONS: usize = 64;

/// Where `gh` is looked for when `--gh` is not given, after `~/.local/bin/gh`; scheduled jobs
/// run with a minimal PATH.
const GH_CANDIDATES: [&str; 3] = ["/opt/homebrew/bin/gh", "/usr/local/bin/gh", "/usr/bin/gh"];

/// Environment passed to `gh` so it finds its configuration and stored credentials.
const GH_ENVIRONMENT: [&str; 8] = [
    "HOME",
    "XDG_CONFIG_HOME",
    "GH_CONFIG_DIR",
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "USER",
    // Linux: gh reads a keyring-stored token through the session bus.
    "XDG_RUNTIME_DIR",
    "DBUS_SESSION_BUS_ADDRESS",
];

// Ignored files are gone for good; everything else comes back with `git worktree add` at the
// preserved commit, and a detached HEAD is pinned under refs/glaeda/worktree-pins/ first.
const COMPENSATION: &str = "git_worktree_add_at_preserved_commit";

// A deleted branch comes back with `git branch <name> <commit>` from the receipt.
const BRANCH_COMPENSATION: &str = "git_branch_at_receipt_commit";

#[derive(Debug, Parser)]
#[command(
    name = "glaeda-worktree-reclaim",
    about = "Find linked Git worktrees whose removal loses nothing, and optionally remove them",
    long_about = "Find linked Git worktrees whose removal loses nothing, and optionally remove them.\n\n\
Without --apply nothing is changed. With --apply the JSON or human report is the receipt: it names \
the commit and branch of every removed worktree, which `git worktree add` needs to recreate it. \
Persist it if you need that record; refs/glaeda/worktree-pins/ refs are the only state this tool \
keeps on its own."
)]
struct Cli {
    /// Canonical absolute path of a repository's main worktree. Repeat for several repositories.
    #[arg(long = "repository", required = true)]
    repositories: Vec<PathBuf>,

    /// Remove eligible worktrees. Without it this only reports and changes nothing.
    #[arg(long)]
    apply: bool,

    /// Most removal attempts one run may make across all repositories.
    #[arg(long, default_value_t = DEFAULT_MAX_RECLAIMS)]
    max_reclaims: usize,

    /// Seconds without Git activity before a worktree holding unfinished work may be reclaimed.
    #[arg(long, default_value_t = DEFAULT_IDLE_SECONDS)]
    minimum_idle_seconds: i64,

    /// Seconds without Git activity before a worktree whose work has landed may be reclaimed.
    #[arg(long, default_value_t = DEFAULT_FINISHED_IDLE_SECONDS)]
    finished_idle_seconds: i64,

    /// Also delete local branches that no worktree has checked out and whose work has landed,
    /// after the finished window. Runs after worktree removal, which frees their branches.
    #[arg(long)]
    branches: bool,

    /// `gh` used to ask GitHub whether a branch's pull request merged (squash merges that main
    /// has since changed further look unfinished to Git alone). Found automatically if omitted.
    #[arg(long)]
    gh: Option<PathBuf>,

    /// Decide from Git alone, without asking GitHub whether a pull request merged: applies to
    /// worktree work state and to --branches.
    #[arg(long, conflicts_with = "gh")]
    no_github: bool,

    /// Most local branches one run may delete across all repositories.
    #[arg(long, default_value_t = DEFAULT_MAX_BRANCH_DELETIONS)]
    max_branch_deletions: usize,

    /// List every worktree (and, with --branches, every branch) in human output, not only the
    /// actionable ones.
    #[arg(long)]
    all: bool,

    /// Select human or JSON output.
    #[arg(long, value_enum, default_value_t = OutputFormat::Human)]
    output: OutputFormat,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum OutputFormat {
    Human,
    Json,
}

#[derive(Debug, Serialize)]
#[serde(tag = "result", rename_all = "snake_case")]
enum WorktreeResult {
    Planned {
        facts: LinkedWorktreeFacts,
        decision: LinkedWorktreeReclaimDecision,
        #[serde(skip_serializing_if = "Option::is_none")]
        reclaim: Option<LinkedWorktreeReclaimOutcome>,
    },
    /// The registration is stale; see `LinkedWorktreeEntry::prunable`. Nothing here recommends
    /// pruning, which deletes per-worktree refs and logs.
    Prunable,
    Unobservable {
        code: &'static str,
    },
}

#[derive(Debug, Serialize)]
struct WorktreeReport {
    /// Position among linked worktrees in `git worktree list` order, starting at 1.
    ordinal: usize,
    /// The registered checkout path. The report goes to the operator who owns these checkouts,
    /// and an ordinal alone had to be mapped back through `git worktree list` by hand.
    path: PathBuf,
    #[serde(flatten)]
    result: WorktreeResult,
}

#[derive(Debug, Default, Serialize)]
struct Summary {
    linked: usize,
    eligible: usize,
    eligible_requiring_head_pin: usize,
    refused: usize,
    prunable: usize,
    unobservable: usize,
    reclaimed: usize,
}

#[derive(Debug, Serialize)]
#[serde(tag = "result", rename_all = "snake_case")]
enum RepositoryResult {
    Listed {
        summary: Summary,
        worktrees: Vec<WorktreeReport>,
    },
    Unlisted {
        code: &'static str,
    },
}

#[derive(Debug, Serialize)]
struct RepositoryReport {
    /// Position of the repository among the `--repository` arguments, starting at 1.
    ordinal: usize,
    /// Free bytes on the repository's filesystem around this repository's removals.
    #[serde(skip_serializing_if = "Option::is_none")]
    available_bytes_before: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    available_bytes_after: Option<u64>,
    #[serde(flatten)]
    result: RepositoryResult,
    #[serde(skip_serializing_if = "Option::is_none")]
    branches: Option<BranchSection>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "result", rename_all = "snake_case")]
enum BranchSection {
    Listed {
        eligible: usize,
        deleted: usize,
        branches: Vec<LocalBranchReport>,
    },
    Unlisted {
        code: &'static str,
    },
}

#[derive(Debug, Serialize)]
struct Report {
    document_type: &'static str,
    schema_version: u8,
    mutation_performed: bool,
    compensation: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    branch_compensation: Option<&'static str>,
    minimum_idle_seconds: i64,
    finished_idle_seconds: i64,
    max_reclaims: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_branch_deletions: Option<usize>,
    budget_exhausted: bool,
    circuit_breaker_tripped: bool,
    repositories: Vec<RepositoryReport>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    if cli.repositories.len() > MAX_REPOSITORIES {
        return fail(
            cli.output,
            "too_many_repositories",
            "one run accepts at most 32 repositories",
        );
    }
    let policy = match LinkedWorktreeReclaimPolicy::with_finished_window(
        cli.minimum_idle_seconds,
        cli.finished_idle_seconds,
    ) {
        Ok(policy) => policy,
        Err(error) => return fail(cli.output, error.code(), error.problem()),
    };
    let Some(now_seconds) = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|now| i64::try_from(now.as_secs()).ok())
    else {
        return fail(
            cli.output,
            "clock_unavailable",
            "the system clock is unusable",
        );
    };
    let observer = match ProjectCheckoutObserver::new(GIT_PROGRAM) {
        Ok(observer) => observer.with_command_timeout(LINKED_WORKTREE_GIT_TIMEOUT),
        Err(error) => return fail(cli.output, error.code, error.problem),
    };
    let executor = ProcessExecutor;

    let mut remaining_budget = cli.max_reclaims;
    let mut remaining_branch_budget = cli.max_branch_deletions;
    let github = (!cli.no_github)
        .then(|| {
            cli.gh.clone().or_else(|| {
                std::env::var_os("HOME")
                    .map(|home| PathBuf::from(home).join(".local/bin/gh"))
                    .into_iter()
                    .chain(GH_CANDIDATES.iter().map(PathBuf::from))
                    .find(|path| path.is_file())
            })
        })
        .flatten()
        .map(|gh_program| GithubLookup {
            gh_program,
            environment: GH_ENVIRONMENT
                .iter()
                .filter_map(|key| {
                    std::env::var(key)
                        .ok()
                        .map(|value| ((*key).to_owned(), value))
                })
                .collect(),
        });
    let mut branch_budget_exhausted = false;
    let mut budget_exhausted = false;
    let mut circuit_breaker_tripped = false;
    let mut mutation_performed = false;
    let mut repositories = Vec::with_capacity(cli.repositories.len());

    for (index, repository) in cli.repositories.iter().enumerate() {
        let mut inventory = match list_linked_worktrees(&observer, repository, &executor) {
            Ok(inventory) => inventory,
            Err(error) => {
                repositories.push(RepositoryReport {
                    ordinal: index + 1,
                    available_bytes_before: None,
                    available_bytes_after: None,
                    result: RepositoryResult::Unlisted { code: error.code() },
                    branches: None,
                });
                continue;
            }
        };
        let available_bytes_before = cli.apply.then(|| available_bytes(repository)).flatten();
        let evidence = match ProcessUseEvidence::collect(&executor) {
            Ok(evidence) => evidence,
            Err(error) => {
                repositories.push(RepositoryReport {
                    ordinal: index + 1,
                    available_bytes_before: None,
                    available_bytes_after: None,
                    result: RepositoryResult::Unlisted { code: error.code() },
                    branches: None,
                });
                continue;
            }
        };
        if let Some(github) = github.as_ref() {
            inventory.record_landed_tips(landed_worktree_tips(
                &observer, &inventory, github, &executor,
            ));
        }
        let observations = observe_linked_worktrees(&observer, &inventory, &evidence, &executor);
        let mut summary = Summary {
            linked: inventory.linked().len(),
            ..Summary::default()
        };
        let mut worktrees = Vec::with_capacity(inventory.linked().len());
        for (position, (entry, observation)) in
            inventory.linked().iter().zip(observations).enumerate()
        {
            let result = if entry.prunable() {
                summary.prunable += 1;
                WorktreeResult::Prunable
            } else {
                match observation.and_then(|facts| {
                    plan_linked_worktree_reclaim(&facts, policy, now_seconds)
                        .map(|plan| (facts, plan.decision().clone()))
                }) {
                    Ok((facts, decision)) => {
                        let mut reclaim = None;
                        match &decision {
                            LinkedWorktreeReclaimDecision::Eligible { compensation, .. } => {
                                summary.eligible += 1;
                                if *compensation == LinkedWorktreeReclaimCompensation::PinHeadCommit
                                {
                                    summary.eligible_requiring_head_pin += 1;
                                }
                                if cli.apply && !circuit_breaker_tripped {
                                    if remaining_budget == 0 {
                                        budget_exhausted = true;
                                    } else {
                                        remaining_budget -= 1;
                                        let outcome = reclaim_linked_worktree(
                                            &observer,
                                            &inventory,
                                            entry,
                                            policy,
                                            now_seconds,
                                            &executor,
                                        );
                                        if matches!(
                                            outcome,
                                            LinkedWorktreeReclaimOutcome::Reclaimed { .. }
                                        ) {
                                            summary.reclaimed += 1;
                                        }
                                        mutation_performed |= outcome.mutated();
                                        circuit_breaker_tripped |= !outcome.batch_may_continue();
                                        reclaim = Some(outcome);
                                    }
                                }
                            }
                            LinkedWorktreeReclaimDecision::Refused { .. } => summary.refused += 1,
                        }
                        WorktreeResult::Planned {
                            facts,
                            decision,
                            reclaim,
                        }
                    }
                    Err(error) => {
                        summary.unobservable += 1;
                        WorktreeResult::Unobservable { code: error.code() }
                    }
                }
            };
            worktrees.push(WorktreeReport {
                ordinal: position + 1,
                path: entry.path().to_path_buf(),
                result,
            });
        }
        let branches = (cli.branches && !circuit_breaker_tripped).then(|| {
            match reclaim_local_branches(
                &observer,
                &inventory,
                policy.finished_idle_seconds(),
                now_seconds,
                cli.apply,
                &mut remaining_branch_budget,
                cli.all,
                github.as_ref(),
                &executor,
            ) {
                Ok(branches) => {
                    let eligible = branches
                        .iter()
                        .filter(|branch| {
                            matches!(branch.decision, LocalBranchDecision::Eligible { .. })
                        })
                        .count();
                    let deleted = branches
                        .iter()
                        .filter(|branch| branch.deletion == Some(LocalBranchDeletion::Deleted))
                        .count();
                    mutation_performed |= deleted > 0;
                    let attempted = branches
                        .iter()
                        .filter(|branch| branch.deletion.is_some())
                        .count();
                    circuit_breaker_tripped |= branches
                        .iter()
                        .any(|branch| branch.deletion == Some(LocalBranchDeletion::GitRefused));
                    branch_budget_exhausted |=
                        cli.apply && !circuit_breaker_tripped && eligible > attempted;
                    BranchSection::Listed {
                        eligible,
                        deleted,
                        branches,
                    }
                }
                Err(error) => BranchSection::Unlisted { code: error.code() },
            }
        });
        repositories.push(RepositoryReport {
            ordinal: index + 1,
            available_bytes_before,
            available_bytes_after: cli.apply.then(|| available_bytes(repository)).flatten(),
            result: RepositoryResult::Listed { summary, worktrees },
            branches,
        });
    }

    let report = Report {
        document_type: if cli.apply {
            "glaeda-worktree-reclaim-receipt"
        } else {
            "glaeda-worktree-reclaim-plan"
        },
        schema_version: REPORT_SCHEMA_VERSION,
        mutation_performed,
        compensation: COMPENSATION,
        branch_compensation: cli.branches.then_some(BRANCH_COMPENSATION),
        minimum_idle_seconds: policy.minimum_idle_seconds(),
        finished_idle_seconds: policy.finished_idle_seconds(),
        max_reclaims: cli.max_reclaims,
        max_branch_deletions: cli.branches.then_some(cli.max_branch_deletions),
        budget_exhausted: budget_exhausted || branch_budget_exhausted,
        circuit_breaker_tripped,
        repositories,
    };
    match cli.output {
        OutputFormat::Json => match serde_json::to_string_pretty(&report) {
            Ok(text) => println!("{text}"),
            Err(_) => {
                return fail(
                    cli.output,
                    "serialization_failed",
                    "report could not be serialized",
                );
            }
        },
        OutputFormat::Human => render_human(&report, cli.apply, cli.all),
    }
    let any_unlisted = report.repositories.iter().any(|repository| {
        matches!(repository.result, RepositoryResult::Unlisted { .. })
            || matches!(repository.branches, Some(BranchSection::Unlisted { .. }))
    });
    if circuit_breaker_tripped {
        ExitCode::from(3)
    } else if any_unlisted {
        // A mistyped or invalid repository must not pass silently in an unattended run.
        ExitCode::from(2)
    } else {
        ExitCode::SUCCESS
    }
}

fn available_bytes(path: &Path) -> Option<u64> {
    let stat = rustix::fs::statvfs(path).ok()?;
    stat.f_bavail.checked_mul(stat.f_frsize)
}

fn render_human(report: &Report, apply: bool, all: bool) {
    let mut eligible_total = 0;
    for repository in &report.repositories {
        match &repository.result {
            RepositoryResult::Unlisted { code } => {
                println!("repository {}: not listed: {code}", repository.ordinal);
            }
            RepositoryResult::Listed { summary, worktrees } => {
                eligible_total += summary.eligible;
                println!(
                    "repository {}: {} linked worktrees: {} eligible ({} need a HEAD pin), {} refused, {} prunable, {} unobservable{}",
                    repository.ordinal,
                    summary.linked,
                    summary.eligible,
                    summary.eligible_requiring_head_pin,
                    summary.refused,
                    summary.prunable,
                    summary.unobservable,
                    if apply {
                        format!(", {} reclaimed", summary.reclaimed)
                    } else {
                        String::new()
                    },
                );
                for worktree in worktrees {
                    if let Some(line) = worktree_line(&worktree.result, all) {
                        println!(
                            "  #{} {}: {line}",
                            worktree.ordinal,
                            worktree.path.display()
                        );
                    }
                }
                match &repository.branches {
                    None => {}
                    Some(BranchSection::Unlisted { code }) => {
                        println!("  branches: not listed: {code}");
                    }
                    Some(BranchSection::Listed {
                        eligible,
                        deleted,
                        branches,
                    }) => {
                        eligible_total += eligible;
                        println!(
                            "  branches: {eligible} finished and idle{}",
                            if apply {
                                format!(", {deleted} deleted")
                            } else {
                                String::new()
                            }
                        );
                        for branch in branches {
                            println!("    {}", branch_line(branch));
                        }
                    }
                }
                if let (Some(before), Some(after)) = (
                    repository.available_bytes_before,
                    repository.available_bytes_after,
                ) {
                    println!(
                        "  free space: {:.1} GiB -> {:.1} GiB",
                        gib(before),
                        gib(after)
                    );
                }
            }
        }
    }
    if apply {
        if report.budget_exhausted {
            println!(
                "stopped at the --max-reclaims or --max-branch-deletions budget; run again to continue"
            );
        }
        if report.circuit_breaker_tripped {
            println!(
                "stopped: a removal ended in an unexpected state; inspect it before rerunning"
            );
        }
    } else if eligible_total > 0 {
        println!("nothing was changed; run again with --apply to reclaim the eligible worktrees");
    } else {
        println!("nothing was changed");
    }
    if !all {
        println!("--all lists every worktree");
    }
}

fn worktree_line(result: &WorktreeResult, all: bool) -> Option<String> {
    match result {
        WorktreeResult::Planned {
            reclaim: Some(outcome),
            ..
        } => Some(match outcome {
            LinkedWorktreeReclaimOutcome::Reclaimed { target, .. } => format!(
                "reclaimed {}{}{}",
                short(&target.commit),
                target
                    .branch
                    .as_deref()
                    .map_or_else(String::new, |branch| format!(" ({branch})")),
                if target.pinned {
                    ", HEAD pinned first"
                } else {
                    ""
                }
            ),
            LinkedWorktreeReclaimOutcome::Changed { .. } => {
                "kept: changed since it was checked".to_owned()
            }
            LinkedWorktreeReclaimOutcome::Refused { vetoes } => {
                format!("kept: fresh check refused: {}", join_vetoes(vetoes))
            }
            LinkedWorktreeReclaimOutcome::Unobservable { code } => {
                format!("kept: fresh check failed: {code}")
            }
            LinkedWorktreeReclaimOutcome::GitRefused { .. } => {
                "kept: git worktree remove refused".to_owned()
            }
            LinkedWorktreeReclaimOutcome::Incomplete { code, .. } => format!("INCOMPLETE: {code}"),
        }),
        WorktreeResult::Planned {
            decision:
                LinkedWorktreeReclaimDecision::Eligible {
                    compensation,
                    idle_seconds,
                    ..
                },
            ..
        } => Some(format!(
            "eligible, idle {}h{}",
            idle_seconds / 3_600,
            match compensation {
                LinkedWorktreeReclaimCompensation::NoneRequired => "",
                LinkedWorktreeReclaimCompensation::PinHeadCommit => ", pin HEAD first",
            }
        )),
        WorktreeResult::Planned {
            decision: LinkedWorktreeReclaimDecision::Refused { vetoes },
            ..
        } => all.then(|| format!("refused: {}", join_vetoes(vetoes))),
        WorktreeResult::Prunable => all
            .then(|| "prunable: registration is stale (directory or .git file missing)".to_owned()),
        WorktreeResult::Unobservable { code } => Some(format!("unobservable: {code}")),
    }
}

fn branch_line(branch: &LocalBranchReport) -> String {
    let state = match (&branch.deletion, &branch.decision) {
        (Some(LocalBranchDeletion::Deleted), _) => "deleted".to_owned(),
        (Some(LocalBranchDeletion::Changed), _) => "kept: changed since it was checked".to_owned(),
        (Some(LocalBranchDeletion::GitRefused), _) => "kept: git refused the delete".to_owned(),
        (
            None,
            LocalBranchDecision::Eligible {
                idle_seconds,
                finished,
            },
        ) => format!(
            "eligible, idle {}h, {}",
            idle_seconds / 3_600,
            match finished {
                LocalBranchFinishedEvidence::DefaultBranch =>
                    "landed on a default branch".to_owned(),
                LocalBranchFinishedEvidence::MergedPullRequest { repository, number } => {
                    format!("merged as {repository}#{number}")
                }
            }
        ),
        (None, LocalBranchDecision::Kept { reason }) => format!(
            "kept: {}",
            serde_json::to_value(reason)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_default()
        ),
    };
    format!("{} {}: {state}", branch.name, short(&branch.commit))
}

fn short(commit: &str) -> &str {
    commit.get(..12).unwrap_or(commit)
}

#[allow(clippy::cast_precision_loss)]
fn gib(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0 * 1024.0)
}

fn join_vetoes(vetoes: &[LinkedWorktreeReclaimVeto]) -> String {
    vetoes
        .iter()
        .map(|veto| veto.code())
        .collect::<Vec<_>>()
        .join(", ")
}

fn fail(output: OutputFormat, code: &str, problem: &str) -> ExitCode {
    match output {
        OutputFormat::Json => println!(
            "{}",
            serde_json::json!({
                "document_type": "glaeda-worktree-reclaim-error",
                "code": code,
                "problem": problem,
            })
        ),
        OutputFormat::Human => eprintln!("{code}: {problem}"),
    }
    ExitCode::from(2)
}
