//! Read-only observation and pure reclaim planning for linked Git worktrees.
//!
//! A linked worktree is not reconstructible the way a Cargo target is. It can hold the only copy of
//! uncommitted edits (including edits `git status` hides behind assume-unchanged or skip-worktree
//! index flags), untracked files, populated submodule repositories, an interrupted operation,
//! per-worktree refs, or a commit that no ref reaches. Authority here therefore comes from
//! preservation: everything the worktree holds must already exist elsewhere in the repository, or be
//! preservable by exactly one named compensation -- pinning a detached HEAD under a ref before
//! removal. Apparent idleness and a path that looks like scratch grant nothing; the idle window is a
//! grace period, not evidence.
//!
//! Outside the preservation guarantee, by design:
//!
//! - Ignored files. They are what makes worktrees expensive (`target/`, `node_modules/`), and
//!   `git worktree remove` discards them too, but ignore rules are repository-controlled, so an
//!   ignored file can still be unique local data such as a `.env`. Callers that act on a plan own
//!   that residual risk.
//! - History reached only through this worktree's reflog or `ORIG_HEAD`, which Git itself already
//!   treats as garbage once it expires.
//! - The objects of this worktree's own submodule repositories. A submodule counts as preserved
//!   when it is clean, at the commit the superproject records, and every ref it holds is reachable
//!   from its remote-tracking refs or from that commit when a superproject remote's default branch
//!   records it (the published gitlink); those objects then come back from the remote, not disk,
//!   and a remote that later drops them takes them with it. Any ref under `refs/remotes/` counts,
//!   including one written by hand or fetched from a local path, as does whatever branch
//!   `refs/remotes/<name>/HEAD` names; a default branch that records a commit its submodule remote
//!   never had is trusted too. History the submodule reaches
//!   only through its reflog goes too, even when preserved superproject history records it. The
//!   decision says so with
//!   [`LinkedWorktreeReclaimAuthority::PreservedInRepositoryAndSubmoduleRemotes`].
//!
//! Shared refs mean branches, tags, and remote-tracking refs. `refs/stash`, prefetch refs, and other
//! worktrees' per-worktree refs are rewritten or dropped by routine commands, so they never count
//! as preservation.
//!
//! Planning performs no mutation and emits no paths. It consumes observations the caller took and
//! returns one decision with every veto that applied, so a refusal explains itself completely.
//! Observed facts are assembled from several reads at slightly different moments, so they are one
//! bounded observation, not an atomic snapshot. Nothing here removes a worktree; an executor must
//! re-observe immediately before acting and belongs behind its own destructive review.

use std::ffi::OsStr;
use std::fmt;
use std::io::Read as _;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use crate::process::{ExecutionRecord, MAX_CAPTURED_STREAM_BYTES, TimedCommandExecutor};
use crate::project_checkout_observation::{
    ProjectBranchState, ProjectCheckoutObservationError, ProjectCheckoutObserver,
};

mod branches;

pub use branches::{
    GITHUB_SECRET_ENVIRONMENT, GithubLookup, LocalBranchDecision, LocalBranchDeletion,
    LocalBranchFinishedEvidence, LocalBranchKeepReason, LocalBranchReport, MAX_LOCAL_BRANCHES,
    landed_worktree_tips, reclaim_local_branches,
};

pub const LINKED_WORKTREE_RECLAIM_SCHEMA_VERSION: u8 = 1;

/// Most linked worktrees one inventory may describe. Larger inventories fail closed.
pub const MAX_LINKED_WORKTREES: usize = 256;

/// Largest per-worktree Git index this module reads. Larger indexes fail closed.
pub const MAX_LINKED_WORKTREE_INDEX_BYTES: u64 = 64 * 1024 * 1024;

/// Most submodule entries one index may declare. More fail closed.
pub const MAX_LINKED_WORKTREE_GITLINKS: usize = 1_024;

/// Smallest idle window a policy may require.
///
/// The window only protects a worktree someone is actively driving through Git between clean
/// states. Preservation is what grants authority, so the floor matches the Cargo target planner
/// rather than trying to prove anything about idleness.
pub const MIN_LINKED_WORKTREE_RECLAIM_IDLE_SECONDS: i64 = 3_600;

/// Per-worktree Git state that marks an interrupted operation holding unique intermediate state.
const OPERATION_MARKERS: [&str; 6] = [
    "MERGE_HEAD",
    "CHERRY_PICK_HEAD",
    "REVERT_HEAD",
    "BISECT_LOG",
    "rebase-merge",
    "rebase-apply",
];

/// Per-worktree Git entry whose modification time records Git activity in that worktree.
///
/// The index is deliberately absent: any tool running a plain `git status` rewrites it, so its
/// mtime measures who looked rather than who worked, and staged changes already veto as tracked
/// changes. So is the mtime of `logs/HEAD`: `git gc` (and `git maintenance run --auto`) runs
/// `reflog expire --all`, which rewrites every worktree's reflog file. On a repository busy
/// enough to keep gc running (Big Red, 2026-09-24: gc every ~30 s) every worktree looked active
/// and a gc landing mid-run read as a future timestamp. The reflog's newest entry keeps the time
/// Git wrote it, so activity comes from that instead.
const ACTIVITY_ENTRY: &str = "HEAD";

/// The per-worktree HEAD reflog, whose newest entry records the last HEAD move.
const HEAD_REFLOG: &str = "logs/HEAD";

/// How much of a reflog's end is read to find its newest entry.
const REFLOG_TAIL_BYTES: u64 = 64 * 1024;

/// `rev-list` selectors for the ref namespaces whose tips survive routine Git commands and so can
/// preserve a commit: branches, tags, and remote-tracking refs.
const PRESERVING_REF_SELECTORS: [&str; 3] = ["--branches", "--tags", "--remotes"];

/// Per-worktree ref namespaces that vanish with the worktree.
const PER_WORKTREE_REF_PATTERNS: [&str; 3] = ["refs/worktree/", "refs/bisect/", "refs/rewritten/"];

/// Whether the work a worktree holds has landed, which decides how long it is kept once idle.
///
/// Removal is already lossless (see the module docs), so this only weighs disruption: a finished
/// worktree nobody touches is clutter, while an unfinished one may be picked up again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkState {
    /// The worktree made a commit of its own, and HEAD is in a remote's default branch or every
    /// file it changed is already identical there (a squash merge).
    Finished,
    /// Anything else, including every case the checks could not settle.
    InProgress,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HeadReachability {
    /// A branch, tag, or remote-tracking ref contains the HEAD commit.
    ReachableFromSharedRef,
    /// No preserving ref reaches the commit; only this worktree's detached HEAD does.
    OnlyFromThisWorktree,
}

/// Facts about one checkout, observed by [`observe_linked_worktree`] or supplied by a caller.
///
/// Planning trusts these as observations, never as ownership proof.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct LinkedWorktreeFacts {
    pub linked: bool,
    pub locked: bool,
    pub operation_in_progress: bool,
    pub tracked_changes_present: bool,
    pub untracked_entry_count: u32,
    /// At least one index entry carries assume-unchanged or skip-worktree, which hides edits from
    /// `git status`.
    pub hidden_index_entries_present: bool,
    /// A submodule path holds a `.git` entry, or per-worktree submodule repositories exist.
    pub populated_submodules_present: bool,
    /// Every populated submodule is clean, checked out at the commit the superproject records,
    /// and holds nothing its remote-tracking refs, or that commit when a superproject remote's
    /// default branch records it, do not reach. True when none is populated.
    pub submodules_preserved_on_remotes: bool,
    /// Refs under this worktree's own `refs/worktree/`, `refs/bisect/`, or `refs/rewritten/`.
    pub per_worktree_refs_present: bool,
    /// The checkout directory belongs to the effective user that would remove it.
    pub owned_by_current_user: bool,
    pub head_reachability: HeadReachability,
    /// A process has its working directory (or, on Linux, an open file) inside the checkout.
    pub in_use: bool,
    pub work_state: WorkState,
    /// Newest of the checkout root's and HEAD's modification times and the newest HEAD reflog
    /// entry, in Unix seconds.
    pub last_activity_seconds: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkedWorktreeReclaimVeto {
    /// The main worktree owns the repository and is never reclaimable here.
    MainWorktree,
    /// Someone ran `git worktree lock`, which is an explicit request to keep it.
    Locked,
    /// A merge, rebase, cherry-pick, revert, or bisect is in progress.
    OperationInProgress,
    /// Tracked files differ from HEAD or the index.
    TrackedChangesPresent,
    /// Untracked, non-ignored files exist and exist nowhere else.
    UntrackedEntriesPresent,
    /// Index flags hide possible edits from `git status`.
    HiddenIndexEntriesPresent,
    /// Submodule repositories are populated inside this worktree and at least one may hold
    /// unique commits or edits: see [`LinkedWorktreeFacts::submodules_preserved_on_remotes`].
    PopulatedSubmodulesPresent,
    /// Per-worktree refs exist and would vanish with the worktree.
    PerWorktreeRefsPresent,
    /// The checkout belongs to a different user than the one planning its removal.
    NotOwnedByCurrentUser,
    /// A running process is working inside the checkout.
    InUse,
    /// Git activity happened inside the idle window for its work state.
    RecentlyActive,
}

impl LinkedWorktreeReclaimVeto {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::MainWorktree => "main_worktree",
            Self::Locked => "locked",
            Self::OperationInProgress => "operation_in_progress",
            Self::TrackedChangesPresent => "tracked_changes_present",
            Self::UntrackedEntriesPresent => "untracked_entries_present",
            Self::HiddenIndexEntriesPresent => "hidden_index_entries_present",
            Self::PopulatedSubmodulesPresent => "populated_submodules_present",
            Self::PerWorktreeRefsPresent => "per_worktree_refs_present",
            Self::NotOwnedByCurrentUser => "not_owned_by_current_user",
            Self::InUse => "in_use",
            Self::RecentlyActive => "recently_active",
        }
    }
}

impl fmt::Display for LinkedWorktreeReclaimVeto {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkedWorktreeReclaimAuthority {
    /// Everything the worktree holds, ignored files aside, already exists elsewhere in the
    /// repository or is covered by the named compensation.
    PreservedInRepository,
    /// As [`Self::PreservedInRepository`], and the worktree's own submodule repositories hold
    /// nothing their remote-tracking refs, or the gitlink a superproject remote's default branch
    /// records, do not reach. Their objects go with the worktree, so
    /// recreating it (`git submodule update --init`) fetches them from those remotes again.
    PreservedInRepositoryAndSubmoduleRemotes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkedWorktreeReclaimCompensation {
    /// A preserving ref already reaches HEAD.
    NoneRequired,
    /// The detached HEAD commit must be pinned under a ref before the worktree is removed.
    PinHeadCommit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkedWorktreeReclaimPolicy {
    minimum_idle_seconds: i64,
    finished_idle_seconds: i64,
}

impl LinkedWorktreeReclaimPolicy {
    /// Build a policy whose one idle window applies to finished and unfinished work alike.
    ///
    /// # Errors
    ///
    /// Refuses a window below [`MIN_LINKED_WORKTREE_RECLAIM_IDLE_SECONDS`].
    pub const fn new(minimum_idle_seconds: i64) -> Result<Self, LinkedWorktreeReclaimError> {
        Self::with_finished_window(minimum_idle_seconds, minimum_idle_seconds)
    }

    /// Build a policy with a separate, usually shorter, window for finished work.
    ///
    /// # Errors
    ///
    /// Refuses either window below [`MIN_LINKED_WORKTREE_RECLAIM_IDLE_SECONDS`].
    pub const fn with_finished_window(
        minimum_idle_seconds: i64,
        finished_idle_seconds: i64,
    ) -> Result<Self, LinkedWorktreeReclaimError> {
        if minimum_idle_seconds < MIN_LINKED_WORKTREE_RECLAIM_IDLE_SECONDS
            || finished_idle_seconds < MIN_LINKED_WORKTREE_RECLAIM_IDLE_SECONDS
        {
            return Err(idle_window_too_small());
        }
        Ok(Self {
            minimum_idle_seconds,
            finished_idle_seconds,
        })
    }

    /// The idle window for unfinished work.
    #[must_use]
    pub const fn minimum_idle_seconds(self) -> i64 {
        self.minimum_idle_seconds
    }

    #[must_use]
    pub const fn finished_idle_seconds(self) -> i64 {
        self.finished_idle_seconds
    }

    const fn window_for(self, work_state: WorkState) -> i64 {
        match work_state {
            WorkState::Finished => self.finished_idle_seconds,
            WorkState::InProgress => self.minimum_idle_seconds,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum LinkedWorktreeReclaimDecision {
    Refused {
        vetoes: Vec<LinkedWorktreeReclaimVeto>,
    },
    Eligible {
        authority: LinkedWorktreeReclaimAuthority,
        compensation: LinkedWorktreeReclaimCompensation,
        idle_seconds: i64,
    },
}

impl LinkedWorktreeReclaimDecision {
    #[must_use]
    pub const fn is_eligible(&self) -> bool {
        matches!(self, Self::Eligible { .. })
    }

    #[must_use]
    pub fn vetoes(&self) -> &[LinkedWorktreeReclaimVeto] {
        match self {
            Self::Refused { vetoes } => vetoes,
            Self::Eligible { .. } => &[],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LinkedWorktreeReclaimPlan {
    schema_version: u8,
    decision: LinkedWorktreeReclaimDecision,
    /// The idle window that applied to this worktree's work state.
    idle_window_seconds: i64,
}

impl LinkedWorktreeReclaimPlan {
    #[must_use]
    pub const fn decision(&self) -> &LinkedWorktreeReclaimDecision {
        &self.decision
    }

    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn idle_window_seconds(&self) -> i64 {
        self.idle_window_seconds
    }
}

/// Decide whether one observed linked worktree may be reclaimed.
///
/// Every applicable veto is collected, so one refusal reports all of its reasons. `now_seconds` is
/// supplied rather than read, which keeps the decision a pure function of its observations.
///
/// # Errors
///
/// Refuses facts whose idle arithmetic would overflow. Activity newer than `now_seconds` is
/// planned as recently active, never as idle.
pub fn plan_linked_worktree_reclaim(
    facts: &LinkedWorktreeFacts,
    policy: LinkedWorktreeReclaimPolicy,
    now_seconds: i64,
) -> Result<LinkedWorktreeReclaimPlan, LinkedWorktreeReclaimError> {
    let mut vetoes = Vec::new();
    if !facts.linked {
        vetoes.push(LinkedWorktreeReclaimVeto::MainWorktree);
    }
    if facts.locked {
        vetoes.push(LinkedWorktreeReclaimVeto::Locked);
    }
    if facts.operation_in_progress {
        vetoes.push(LinkedWorktreeReclaimVeto::OperationInProgress);
    }
    if facts.tracked_changes_present {
        vetoes.push(LinkedWorktreeReclaimVeto::TrackedChangesPresent);
    }
    if facts.untracked_entry_count > 0 {
        vetoes.push(LinkedWorktreeReclaimVeto::UntrackedEntriesPresent);
    }
    if facts.hidden_index_entries_present {
        vetoes.push(LinkedWorktreeReclaimVeto::HiddenIndexEntriesPresent);
    }
    if facts.populated_submodules_present && !facts.submodules_preserved_on_remotes {
        vetoes.push(LinkedWorktreeReclaimVeto::PopulatedSubmodulesPresent);
    }
    if facts.per_worktree_refs_present {
        vetoes.push(LinkedWorktreeReclaimVeto::PerWorktreeRefsPresent);
    }
    if !facts.owned_by_current_user {
        vetoes.push(LinkedWorktreeReclaimVeto::NotOwnedByCurrentUser);
    }
    if facts.in_use {
        vetoes.push(LinkedWorktreeReclaimVeto::InUse);
    }

    let idle_seconds = now_seconds
        .checked_sub(facts.last_activity_seconds)
        .ok_or_else(idle_overflow)?;
    // Activity newer than the clock reading is recent by definition. A sweep reads the clock once
    // and then observes for many minutes on a loaded host, so a worktree touched meanwhile shows
    // up here; a clock stepped backwards keeps worktrees too, which is the safe direction.
    let window = policy.window_for(facts.work_state);
    if idle_seconds < window {
        vetoes.push(LinkedWorktreeReclaimVeto::RecentlyActive);
    }

    vetoes.sort_unstable();
    vetoes.dedup();

    let decision = if vetoes.is_empty() {
        LinkedWorktreeReclaimDecision::Eligible {
            authority: if facts.populated_submodules_present {
                LinkedWorktreeReclaimAuthority::PreservedInRepositoryAndSubmoduleRemotes
            } else {
                LinkedWorktreeReclaimAuthority::PreservedInRepository
            },
            compensation: match facts.head_reachability {
                HeadReachability::ReachableFromSharedRef => {
                    LinkedWorktreeReclaimCompensation::NoneRequired
                }
                HeadReachability::OnlyFromThisWorktree => {
                    LinkedWorktreeReclaimCompensation::PinHeadCommit
                }
            },
            idle_seconds,
        }
    } else {
        LinkedWorktreeReclaimDecision::Refused { vetoes }
    };

    Ok(LinkedWorktreeReclaimPlan {
        schema_version: LINKED_WORKTREE_RECLAIM_SCHEMA_VERSION,
        decision,
        idle_window_seconds: window,
    })
}

// --- Process evidence ------------------------------------------------------

/// Where running processes of this user are working, from one bounded observation.
///
/// Linux reads every readable process's working directory and open files from `/proc`. macOS asks
/// `lsof` for working directories only: every open file of every process exceeds the 1 MiB capture
/// limit on a busy Mac, and a process working in a checkout normally has its cwd there. Paths are
/// kept private and only ever compared.
#[derive(Clone, PartialEq, Eq)]
pub struct ProcessUseEvidence {
    paths: Vec<PathBuf>,
}

impl fmt::Debug for ProcessUseEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProcessUseEvidence")
            .field("paths", &self.paths.len())
            .finish()
    }
}

const LSOF_PROGRAM: &str = "/usr/sbin/lsof";
const LSOF_TIMEOUT: Duration = Duration::from_secs(60);

impl ProcessUseEvidence {
    /// Evidence from explicit paths, for callers that observed processes themselves and for tests.
    #[must_use]
    pub fn from_paths(paths: impl IntoIterator<Item = PathBuf>) -> Self {
        Self {
            paths: paths.into_iter().collect(),
        }
    }

    /// Observe the processes running now.
    ///
    /// # Errors
    ///
    /// Fails when no process could be observed at all (this process always has a working
    /// directory, so empty evidence means the source is unusable) or `lsof` fails. Callers must
    /// then treat every checkout as possibly in use.
    pub fn collect(
        executor: &impl TimedCommandExecutor,
    ) -> Result<Self, LinkedWorktreeReclaimError> {
        let proc_root = Path::new("/proc/self/cwd");
        let paths = if proc_root.exists() {
            collect_proc_paths()
        } else {
            let spec = crate::process::CommandSpec::new(LSOF_PROGRAM)
                .argument("-n")
                .argument("-P")
                .argument("-w")
                .argument("-Fn")
                .argument("-a")
                .argument("-d")
                .argument("cwd");
            let record = executor
                .execute_with_timeout(&spec, LSOF_TIMEOUT)
                .map_err(|_| process_evidence_unavailable())?;
            // lsof exits 1 when some process could not be fully read; anything else is a failure.
            if !matches!(record.status, Some(0 | 1)) {
                return Err(process_evidence_unavailable());
            }
            parse_lsof_names(&record.stdout)
        };
        if paths.is_empty() {
            return Err(process_evidence_unavailable());
        }
        Ok(Self { paths })
    }

    /// Whether any observed path is `directory` itself or inside it.
    #[must_use]
    pub fn uses(&self, directory: &Path) -> bool {
        self.paths.iter().any(|path| path.starts_with(directory))
    }
}

fn collect_proc_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return paths;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !name.as_bytes().iter().all(u8::is_ascii_digit) {
            continue;
        }
        let process = entry.path();
        // Other users' processes are unreadable; that is expected, not a finding.
        if let Ok(cwd) = std::fs::read_link(process.join("cwd")) {
            paths.push(cwd);
        }
        if let Ok(descriptors) = std::fs::read_dir(process.join("fd")) {
            paths.extend(
                descriptors
                    .flatten()
                    .filter_map(|descriptor| std::fs::read_link(descriptor.path()).ok())
                    .filter(|target| target.is_absolute()),
            );
        }
    }
    paths
}

fn parse_lsof_names(output: &str) -> Vec<PathBuf> {
    output
        .lines()
        .filter_map(|line| line.strip_prefix('n'))
        .filter(|name| name.starts_with('/'))
        .map(PathBuf::from)
        .collect()
}

// --- Work state ------------------------------------------------------------

/// Most changed paths compared against a default branch; a larger branch counts as unfinished.
const MAX_FINISHED_COMPARE_PATHS: usize = 512;

/// Decide whether `commit` has landed.
///
/// A worktree that never made a commit of its own did nothing yet, however recently what it
/// checked out reached a default branch, so it stays in progress. An upstream marked `[gone]` is not
/// used: follow-up commits after the remote branch was deleted are common. Every question is asked
/// from the main worktree, and any failure answers "in progress", which only keeps the worktree
/// longer.
fn observe_work_state(
    observer: &ProjectCheckoutObserver,
    repository: &Path,
    commit: &str,
    authored: bool,
    executor: &impl TimedCommandExecutor,
) -> WorkState {
    if !authored {
        return WorkState::InProgress;
    }
    let run = |arguments: &[&str]| observer.git(repository, arguments, executor).ok();
    let Some(heads) = run(&["for-each-ref", "--format=%(symref)", "refs/remotes/*/HEAD"])
        .filter(|record| record.success)
    else {
        return WorkState::InProgress;
    };
    for target in heads
        .stdout
        .lines()
        .filter(|line| line.starts_with("refs/remotes/"))
    {
        match run(&["merge-base", "--is-ancestor", commit, target]).and_then(|r| r.status) {
            Some(0) => return WorkState::Finished,
            Some(1) => {}
            _ => continue,
        }
        let Some(base) = run(&["merge-base", commit, target]).filter(|record| record.success)
        else {
            continue;
        };
        let base = base.stdout.trim().to_owned();
        // --no-renames: a rename must list both names, or a branch that renamed `a` to `a2`
        // would compare only `a2` and miss that the default branch still has `a`.
        let Some(changed) = run(&["diff", "--no-renames", "--name-only", "-z", &base, commit])
            .filter(|record| record.success)
        else {
            continue;
        };
        let paths: Vec<String> = changed
            .stdout
            .split('\0')
            .filter(|path| !path.is_empty())
            .map(|path| format!(":(literal){path}"))
            .collect();
        if paths.is_empty() || paths.len() > MAX_FINISHED_COMPARE_PATHS {
            continue;
        }
        let mut arguments = vec!["diff", "--no-renames", "--quiet", commit, target, "--"];
        arguments.extend(paths.iter().map(String::as_str));
        if run(&arguments).and_then(|record| record.status) == Some(0) {
            return WorkState::Finished;
        }
    }
    WorkState::InProgress
}

/// Whether this worktree made a commit of its own, according to its HEAD reflog.
///
/// Moving HEAD by syncing (`merge --ff-only`, `pull` fast-forward, `reset`, `checkout`) is not
/// work: such a worktree would otherwise look finished as soon as main contains what it synced to.
/// An expired or unreadable reflog answers `false`, which only keeps the worktree longer.
fn authored_work(git_dir: &Path) -> bool {
    let Ok(text) = std::fs::read(git_dir.join(HEAD_REFLOG)) else {
        return false;
    };
    text.split(|byte| *byte == b'\n').any(|line| {
        let Some(tab) = line.iter().position(|byte| *byte == b'\t') else {
            return false;
        };
        let subject = &line[tab + 1..];
        let fast_forward = subject.windows(12).any(|window| window == b"Fast-forward");
        [
            &b"commit"[..],
            b"cherry-pick",
            b"revert",
            b"rebase (finish)",
        ]
        .iter()
        .any(|action| subject.starts_with(action))
            || ((subject.starts_with(b"merge ") || subject.starts_with(b"pull")) && !fast_forward)
    })
}

// --- Observation -----------------------------------------------------------

/// One linked worktree registered with the repository, as `git worktree list` reported it.
#[derive(Clone, PartialEq, Eq)]
pub struct LinkedWorktreeEntry {
    path: PathBuf,
    locked: bool,
    prunable: bool,
    duplicate: bool,
}

impl LinkedWorktreeEntry {
    /// Registered checkout path. Private: callers must not publish it.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub const fn locked(&self) -> bool {
        self.locked
    }

    /// Git reports the registration as stale: the checkout directory or its `.git` file is
    /// missing. The directory may still hold data, and pruning deletes per-worktree refs and logs,
    /// so a stale registration grants no cleanup authority either.
    #[must_use]
    pub const fn prunable(&self) -> bool {
        self.prunable
    }
}

impl fmt::Debug for LinkedWorktreeEntry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LinkedWorktreeEntry")
            .field("path", &"<private-worktree-path>")
            .field("locked", &self.locked)
            .field("prunable", &self.prunable)
            .field("duplicate", &self.duplicate)
            .finish()
    }
}

/// The linked worktrees of one repository, bound to its common Git directory.
#[derive(Clone, PartialEq, Eq)]
pub struct LinkedWorktreeInventory {
    repository: PathBuf,
    common_dir: PathBuf,
    linked: Vec<LinkedWorktreeEntry>,
    /// `branch -> tip` a merged pull request contains, supplied by the caller (see
    /// [`landed_worktree_tips`]); observation itself stays offline.
    landed_tips: std::collections::BTreeMap<String, String>,
}

impl LinkedWorktreeInventory {
    #[must_use]
    pub fn linked(&self) -> &[LinkedWorktreeEntry] {
        &self.linked
    }

    /// Record branch tips known to have landed through a merged pull request. A worktree on one
    /// of these branches, still at exactly that tip, counts as finished. This only shortens the
    /// idle window: removal keeps the branch either way.
    pub fn record_landed_tips(&mut self, tips: std::collections::BTreeMap<String, String>) {
        self.landed_tips = tips;
    }
}

impl fmt::Debug for LinkedWorktreeInventory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LinkedWorktreeInventory")
            .field("repository", &"<private-repository-path>")
            .field("linked_count", &self.linked.len())
            .finish_non_exhaustive()
    }
}

/// List the linked worktrees of the repository whose main worktree is `repository`.
///
/// # Errors
///
/// Refuses a `repository` that is not the canonical main worktree, malformed or oversized Git
/// output, and more than [`MAX_LINKED_WORKTREES`] linked worktrees.
pub fn list_linked_worktrees(
    observer: &ProjectCheckoutObserver,
    repository: &Path,
    executor: &impl TimedCommandExecutor,
) -> Result<LinkedWorktreeInventory, LinkedWorktreeReclaimError> {
    let canonical = std::fs::canonicalize(repository).map_err(|_| unavailable())?;
    if canonical.as_path() != repository {
        return Err(not_main_worktree());
    }
    let common_dir = absolute_git_path(observer, repository, "--git-common-dir", executor)?;
    let git_dir = absolute_git_path(observer, repository, "--git-dir", executor)?;
    if git_dir != common_dir {
        return Err(not_main_worktree());
    }
    // The index layout scanned below embeds object names; only the reviewed SHA-1 width is read.
    let format = git(
        observer,
        repository,
        &["rev-parse", "--show-object-format"],
        executor,
    )?;
    if format.stdout != "sha1\n" {
        return Err(object_format_unsupported());
    }
    let record = git_bounded(
        observer,
        repository,
        &["worktree", "list", "--porcelain", "-z"],
        MAX_CAPTURED_STREAM_BYTES,
        executor,
    )?;
    let mut records = parse_worktree_list(&record.stdout)?.into_iter();
    let main = records.next().ok_or_else(invalid_output)?;
    if main.path != repository {
        return Err(not_main_worktree());
    }
    let mut linked = records.collect::<Vec<_>>();
    if linked.len() > MAX_LINKED_WORKTREES {
        return Err(too_many_worktrees());
    }
    // Two registrations naming one checkout cannot both be described by its facts.
    for index in 0..linked.len() {
        let path = linked[index].path.clone();
        let shared = path == main.path
            || linked
                .iter()
                .enumerate()
                .any(|(other, entry)| other != index && entry.path == path);
        linked[index].duplicate = shared;
    }
    Ok(LinkedWorktreeInventory {
        repository: repository.to_path_buf(),
        common_dir,
        linked,
        landed_tips: std::collections::BTreeMap::new(),
    })
}

/// Observe the reclaim-relevant facts of one inventory entry without mutation or network access.
///
/// # Errors
///
/// Refuses a stale, duplicated, or aliased registration; a checkout the project observer cannot
/// observe consistently; a checkout bound to a different repository or administrative directory;
/// an unreadable or malformed index; and malformed or oversized Git output.
pub fn observe_linked_worktree(
    observer: &ProjectCheckoutObserver,
    inventory: &LinkedWorktreeInventory,
    entry: &LinkedWorktreeEntry,
    evidence: &ProcessUseEvidence,
    executor: &impl TimedCommandExecutor,
) -> Result<LinkedWorktreeFacts, LinkedWorktreeReclaimError> {
    observe_detailed(observer, inventory, entry, evidence, executor).map(|observed| observed.facts)
}

/// Facts plus the exact identities an executor must bind its action to.
struct ObservedWorktree {
    facts: LinkedWorktreeFacts,
    head: String,
    branch: Option<String>,
    git_dir: PathBuf,
    fingerprint: AdministrativeFingerprint,
    /// The worktree's own submodule repositories, fingerprinted like `git_dir` before their
    /// content was read. Empty unless every one of them was proven preserved.
    submodule_fingerprints: Vec<(PathBuf, AdministrativeFingerprint)>,
}

/// Modification time and size of the per-worktree entries any Git activity rewrites.
///
/// Compared again immediately before removal: a commit, checkout, reset, `update-index` flag change,
/// or staging in the meantime changes at least one of them. The index is included here, unlike in
/// the idle signal, because a spurious change only makes the executor keep a worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AdministrativeFingerprint(Vec<Option<(i64, i64, u64)>>);

const FINGERPRINT_ENTRIES: [&str; 3] = ["HEAD", "logs/HEAD", "index"];

fn administrative_fingerprint(
    git_dir: &Path,
) -> Result<AdministrativeFingerprint, LinkedWorktreeReclaimError> {
    FINGERPRINT_ENTRIES
        .iter()
        .map(|name| match std::fs::symlink_metadata(git_dir.join(name)) {
            Ok(metadata) => Ok(Some((
                metadata.mtime(),
                metadata.mtime_nsec(),
                metadata.size(),
            ))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(unavailable()),
        })
        .collect::<Result<Vec<_>, _>>()
        .map(AdministrativeFingerprint)
}

fn observe_detailed(
    observer: &ProjectCheckoutObserver,
    inventory: &LinkedWorktreeInventory,
    entry: &LinkedWorktreeEntry,
    evidence: &ProcessUseEvidence,
    executor: &impl TimedCommandExecutor,
) -> Result<ObservedWorktree, LinkedWorktreeReclaimError> {
    if entry.prunable {
        return Err(registration_stale());
    }
    if entry.duplicate {
        return Err(registration_duplicated());
    }
    // Git records real paths when it adds a worktree. A registered path that no longer resolves to
    // itself has been replaced by a symlink or alias, and would let one checkout's facts stand in
    // for another registration.
    let checkout = entry.path.clone();
    if std::fs::canonicalize(&checkout).map_err(|_| registration_stale())? != checkout {
        return Err(registration_aliased());
    }
    let common_dir = absolute_git_path(observer, &checkout, "--git-common-dir", executor)?;
    if common_dir != inventory.common_dir {
        return Err(foreign_repository());
    }
    let git_dir = absolute_git_path(observer, &checkout, "--git-dir", executor)?;
    // Taken before any content is read, so every later read -- status, refs, index -- happens after
    // it. Git activity during observation then shows up as a fingerprint change at removal time
    // instead of being folded into an already-stale observation.
    let fingerprint = administrative_fingerprint(&git_dir)?;
    let observation = observer
        .observe(&checkout, executor)
        .map_err(|error| match error.code {
            "observation_timed_out" => git_timed_out(),
            "source_changed" => checkout_changed(),
            _ => checkout_unobservable(),
        })?;
    let linked = git_dir != common_dir;
    if linked && read_gitdir_backlink(&git_dir)? != checkout.join(".git") {
        return Err(administrative_directory_mismatch());
    }

    let head_reachability = match observation.branch() {
        // Removing a worktree never deletes its branch, so the branch ref keeps HEAD reachable.
        ProjectBranchState::Attached { .. } => HeadReachability::ReachableFromSharedRef,
        ProjectBranchState::Detached => {
            // Asked from the main worktree, restricted to preserving namespaces, so neither this
            // worktree's own refs nor short-lived refs such as the stash count as preservation.
            // `rev-list --not` walks only back to the merge base. `for-each-ref --contains` walks
            // every ref's history when the answer is "none", which took about eight seconds on a
            // 9,000-ref repository and ran into the command timeout under load.
            let commit = observation.commit().as_str();
            let mut arguments = vec!["rev-list", "-n1", commit, "--not"];
            arguments.extend(PRESERVING_REF_SELECTORS);
            let unreached = git(observer, &inventory.repository, &arguments, executor)?;
            match unreached.stdout.strip_suffix('\n') {
                None if unreached.stdout.is_empty() => HeadReachability::ReachableFromSharedRef,
                Some(line) if line == commit => HeadReachability::OnlyFromThisWorktree,
                _ => return Err(invalid_output()),
            }
        }
    };

    let per_worktree_refs_present = if linked {
        let mut arguments = vec!["for-each-ref", "--count=1", "--format=%(refname)"];
        arguments.extend(PER_WORKTREE_REF_PATTERNS);
        !git(observer, &checkout, &arguments, executor)?
            .stdout
            .is_empty()
    } else {
        false
    };

    let index = scan_index(&read_index(&git_dir.join("index"))?)?;
    let mut populated_submodules_present = directory_nonempty(&git_dir.join("modules"))?;
    let mut populated_gitlinks = Vec::new();
    for path in &index.gitlink_paths {
        let relative = relative_index_path(path)?;
        if submodule_path_populated(&checkout.join(relative))? {
            populated_submodules_present = true;
            populated_gitlinks.push(relative.to_path_buf());
        }
    }
    let submodule_fingerprints = if populated_submodules_present {
        observe_submodule_preservation(
            observer,
            &checkout,
            &git_dir,
            &populated_gitlinks,
            executor,
        )?
    } else {
        Some(Vec::new())
    };

    let operation_in_progress = OPERATION_MARKERS
        .iter()
        .map(|marker| entry_present(&git_dir.join(marker)))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .any(|present| present);
    let locked = entry.locked || entry_present(&git_dir.join("locked"))?;
    let last_activity_seconds = last_activity_seconds(&git_dir, &checkout)?;
    let branch = match observation.branch() {
        ProjectBranchState::Attached { name } => Some(name.clone()),
        ProjectBranchState::Detached => None,
    };
    let head = observation.commit().as_str().to_owned();
    // Like the git-only test, only a worktree that made a commit of its own can be finished: a
    // fresh branch whose name and start point match an old merged pull request did nothing yet.
    let authored = authored_work(&git_dir);
    let landed = authored
        && branch
            .as_ref()
            .is_some_and(|name| inventory.landed_tips.get(name) == Some(&head));
    let work_state = if landed {
        WorkState::Finished
    } else {
        observe_work_state(observer, &inventory.repository, &head, authored, executor)
    };

    let facts = LinkedWorktreeFacts {
        linked,
        locked,
        operation_in_progress,
        tracked_changes_present: observation.tracked_changes_present(),
        untracked_entry_count: observation.untracked_entry_count(),
        hidden_index_entries_present: index.hidden_entries_present,
        populated_submodules_present,
        submodules_preserved_on_remotes: submodule_fingerprints.is_some(),
        per_worktree_refs_present,
        // Compared with the effective user rather than the parent directory: shared scratch roots
        // such as `/tmp` are root-owned, and the parent's owner says nothing about who may remove
        // a checkout beneath it.
        owned_by_current_user: std::fs::symlink_metadata(&checkout)
            .map_err(|_| unavailable())?
            .uid()
            == rustix::process::geteuid().as_raw(),
        head_reachability,
        in_use: evidence.uses(&checkout),
        work_state,
        last_activity_seconds,
    };
    Ok(ObservedWorktree {
        facts,
        head,
        branch,
        git_dir,
        fingerprint,
        submodule_fingerprints: submodule_fingerprints.unwrap_or_default(),
    })
}

/// Most concurrent observations [`observe_linked_worktrees`] runs.
pub const MAX_LINKED_WORKTREE_OBSERVATION_WORKERS: usize = 8;

/// Observe every inventory entry with a small bounded pool, in inventory order.
///
/// Each observation runs several `git status`-class reads, which dominate a sweep: sequentially a
/// 33-worktree cmux checkout took about 50 seconds. Stale registrations are reported as errors
/// without being observed, and a worker that panics leaves its entries unobservable rather than
/// guessed at.
pub fn observe_linked_worktrees(
    observer: &ProjectCheckoutObserver,
    inventory: &LinkedWorktreeInventory,
    evidence: &ProcessUseEvidence,
    executor: &(impl TimedCommandExecutor + Sync),
) -> Vec<Result<LinkedWorktreeFacts, LinkedWorktreeReclaimError>> {
    let entries = &inventory.linked;
    let workers = std::thread::available_parallelism()
        .map_or(1, std::num::NonZeroUsize::get)
        .clamp(1, MAX_LINKED_WORKTREE_OBSERVATION_WORKERS)
        .min(entries.len().max(1));
    let next = std::sync::atomic::AtomicUsize::new(0);
    let mut ordered: Vec<Result<LinkedWorktreeFacts, LinkedWorktreeReclaimError>> = (0..entries
        .len())
        .map(|_| Err(observation_lost()))
        .collect();
    let observed = std::thread::scope(|scope| {
        let handles = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    let mut local = Vec::new();
                    loop {
                        let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(entry) = entries.get(index) else {
                            break;
                        };
                        local.push((
                            index,
                            observe_linked_worktree(observer, inventory, entry, evidence, executor),
                        ));
                    }
                    local
                })
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .flat_map(|handle| handle.join().unwrap_or_default())
            .collect::<Vec<_>>()
    });
    for (index, result) in observed {
        ordered[index] = result;
    }
    ordered
}

fn git(
    observer: &ProjectCheckoutObserver,
    checkout: &Path,
    arguments: &[&str],
    executor: &impl TimedCommandExecutor,
) -> Result<ExecutionRecord, LinkedWorktreeReclaimError> {
    let record = observer
        .git(checkout, arguments, executor)
        .map_err(|error| git_failure(&error))?;
    require_success(record)
}

fn git_bounded(
    observer: &ProjectCheckoutObserver,
    checkout: &Path,
    arguments: &[&str],
    max_stdout_bytes: usize,
    executor: &impl TimedCommandExecutor,
) -> Result<ExecutionRecord, LinkedWorktreeReclaimError> {
    let record = observer
        .git_bounded(checkout, arguments, max_stdout_bytes, executor)
        .map_err(|error| git_failure(&error))?;
    require_success(record)
}

fn git_failure(error: &ProjectCheckoutObservationError) -> LinkedWorktreeReclaimError {
    if error.code == "observation_timed_out" {
        git_timed_out()
    } else {
        unavailable()
    }
}

fn require_success(record: ExecutionRecord) -> Result<ExecutionRecord, LinkedWorktreeReclaimError> {
    if record.success && record.status == Some(0) && record.stderr.is_empty() {
        Ok(record)
    } else {
        Err(unavailable())
    }
}

fn absolute_git_path(
    observer: &ProjectCheckoutObserver,
    checkout: &Path,
    selector: &str,
    executor: &impl TimedCommandExecutor,
) -> Result<PathBuf, LinkedWorktreeReclaimError> {
    let record = git(
        observer,
        checkout,
        &["rev-parse", "--path-format=absolute", selector],
        executor,
    )?;
    let line = record.stdout.strip_suffix('\n').unwrap_or(&record.stdout);
    if line.is_empty() || line.contains('\n') || line.contains('\0') {
        return Err(invalid_output());
    }
    let path = PathBuf::from(line);
    if !path.is_absolute() {
        return Err(invalid_output());
    }
    std::fs::canonicalize(&path).map_err(|_| unavailable())
}

/// Read the administrative directory's record of which checkout owns it.
fn read_gitdir_backlink(git_dir: &Path) -> Result<PathBuf, LinkedWorktreeReclaimError> {
    let mut contents = String::new();
    std::fs::File::open(git_dir.join("gitdir"))
        .map_err(|_| administrative_directory_mismatch())?
        .take(4_097)
        .read_to_string(&mut contents)
        .map_err(|_| administrative_directory_mismatch())?;
    let line = contents.strip_suffix('\n').unwrap_or(&contents);
    if line.is_empty() || line.len() > 4_096 || line.contains('\n') || line.contains('\0') {
        return Err(administrative_directory_mismatch());
    }
    // `worktree add --relative-paths` records the backlink relative to the administrative
    // directory; resolve it so both layouts compare against the same canonical checkout.
    let recorded = PathBuf::from(line);
    let resolved = if recorded.is_absolute() {
        recorded
    } else {
        git_dir.join(recorded)
    };
    std::fs::canonicalize(resolved).map_err(|_| administrative_directory_mismatch())
}

fn parse_worktree_list(
    value: &str,
) -> Result<Vec<LinkedWorktreeEntry>, LinkedWorktreeReclaimError> {
    if !value.is_empty() && !value.ends_with('\0') {
        return Err(invalid_output());
    }
    let mut entries = Vec::new();
    let mut current: Option<LinkedWorktreeEntry> = None;
    for field in value.split_terminator('\0') {
        if field.is_empty() {
            entries.extend(current.take());
            continue;
        }
        if let Some(path) = field.strip_prefix("worktree ") {
            entries.extend(current.take());
            let path = PathBuf::from(path);
            if !path.is_absolute() {
                return Err(invalid_output());
            }
            current = Some(LinkedWorktreeEntry {
                path,
                locked: false,
                prunable: false,
                duplicate: false,
            });
            continue;
        }
        let entry = current.as_mut().ok_or_else(invalid_output)?;
        if field == "locked" || field.starts_with("locked ") {
            entry.locked = true;
        } else if field == "prunable" || field.starts_with("prunable ") {
            entry.prunable = true;
        } else if field == "bare" {
            return Err(not_main_worktree());
        }
    }
    entries.extend(current);
    if entries.len() > MAX_LINKED_WORKTREES.saturating_add(1) {
        return Err(too_many_worktrees());
    }
    Ok(entries)
}

// --- Index scan ------------------------------------------------------------
//
// `git ls-files -v` would report the same flags, but its output scales with path length and exceeds
// the process capture ceiling on ordinary large repositories. The index file itself carries both
// facts this module needs in a fixed layout, so it is read directly and bounded. Only flags, modes,
// and paths are interpreted. The checksum is not verified; instead the extension chain must end
// exactly at the trailing SHA-1, which catches any misparsed layout. A split index keeps entries and
// their flags in a separate shared file, so its `link` extension is refused rather than trusted.

const INDEX_SIGNATURE: &[u8; 4] = b"DIRC";
const INDEX_HEADER_BYTES: usize = 12;
const INDEX_ENTRY_FIXED_BYTES: usize = 62;
const INDEX_ENTRY_MODE_OFFSET: usize = 24;
const INDEX_ENTRY_FLAGS_OFFSET: usize = 60;
const INDEX_ASSUME_VALID_FLAG: u16 = 0x8000;
const INDEX_EXTENDED_FLAG: u16 = 0x4000;
const INDEX_SKIP_WORKTREE_FLAG: u16 = 0x4000;
const INDEX_NAME_LENGTH_MASK: u16 = 0x0fff;
const INDEX_GITLINK_MODE: u32 = 0o160_000;
const INDEX_MAX_PATH_BYTES: usize = 4_096;
const INDEX_MAX_VARINT_BYTES: usize = 10;
const INDEX_EXTENSION_HEADER_BYTES: usize = 8;
const INDEX_SHA1_BYTES: usize = 20;
const INDEX_SPLIT_LINK_EXTENSION: &[u8; 4] = b"link";

#[derive(Debug, Default, PartialEq, Eq)]
struct IndexScan {
    hidden_entries_present: bool,
    gitlink_paths: Vec<Vec<u8>>,
}

fn read_index(path: &Path) -> Result<Vec<u8>, LinkedWorktreeReclaimError> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        // A worktree that has never staged anything may have no index; it hides nothing.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err(unavailable()),
    };
    let mut bytes = Vec::new();
    file.take(MAX_LINKED_WORKTREE_INDEX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| unavailable())?;
    if bytes.len() as u64 > MAX_LINKED_WORKTREE_INDEX_BYTES {
        return Err(index_too_large());
    }
    Ok(bytes)
}

fn scan_index(bytes: &[u8]) -> Result<IndexScan, LinkedWorktreeReclaimError> {
    let mut scan = IndexScan::default();
    if bytes.is_empty() {
        return Ok(scan);
    }
    if bytes.len() < INDEX_HEADER_BYTES || &bytes[..4] != INDEX_SIGNATURE {
        return Err(invalid_index());
    }
    let version = read_u32(bytes, 4)?;
    if !(2..=4).contains(&version) {
        return Err(invalid_index());
    }
    let entries = read_u32(bytes, 8)?;
    let mut offset = INDEX_HEADER_BYTES;
    let mut previous: Vec<u8> = Vec::new();
    // A split index stores placeholder entries with stripped, empty names. They are tolerated only
    // until the extension walk proves the index is split, which is refused with its own code.
    let mut empty_path_seen = false;
    for _ in 0..entries {
        let fixed_end = offset
            .checked_add(INDEX_ENTRY_FIXED_BYTES)
            .ok_or_else(invalid_index)?;
        let mode = read_u32(bytes, offset + INDEX_ENTRY_MODE_OFFSET)?;
        let flags = read_u16(bytes, offset + INDEX_ENTRY_FLAGS_OFFSET)?;
        if flags & INDEX_ASSUME_VALID_FLAG != 0 {
            scan.hidden_entries_present = true;
        }
        let mut name_start = fixed_end;
        if flags & INDEX_EXTENDED_FLAG != 0 {
            if version < 3 {
                return Err(invalid_index());
            }
            if read_u16(bytes, fixed_end)? & INDEX_SKIP_WORKTREE_FLAG != 0 {
                scan.hidden_entries_present = true;
            }
            name_start = fixed_end + 2;
        }

        let (path, next) = if version == 4 {
            let (strip, varint_bytes) = read_index_varint(bytes, name_start)?;
            let suffix_start = name_start + varint_bytes;
            let terminator = find_nul(bytes, suffix_start)?;
            let keep = usize::try_from(strip)
                .ok()
                .and_then(|strip| previous.len().checked_sub(strip))
                .ok_or_else(invalid_index)?;
            let mut path = previous[..keep].to_vec();
            path.extend_from_slice(&bytes[suffix_start..terminator]);
            (path, terminator + 1)
        } else {
            let declared = usize::from(flags & INDEX_NAME_LENGTH_MASK);
            let terminator = if declared < usize::from(INDEX_NAME_LENGTH_MASK) {
                let terminator = name_start + declared;
                if bytes.get(terminator) != Some(&0) {
                    return Err(invalid_index());
                }
                terminator
            } else {
                find_nul(bytes, name_start + declared)?
            };
            // Entries are NUL-padded to a multiple of eight bytes, with at least one NUL.
            let entry_bytes = ((terminator - offset) + 8) & !7;
            let next = offset.checked_add(entry_bytes).ok_or_else(invalid_index)?;
            if next > bytes.len() {
                return Err(invalid_index());
            }
            (bytes[name_start..terminator].to_vec(), next)
        };
        if path.len() > INDEX_MAX_PATH_BYTES {
            return Err(invalid_index());
        }
        empty_path_seen |= path.is_empty();
        if mode == INDEX_GITLINK_MODE && !path.is_empty() {
            if scan.gitlink_paths.len() >= MAX_LINKED_WORKTREE_GITLINKS {
                return Err(invalid_index());
            }
            scan.gitlink_paths.push(path.clone());
        }
        previous = path;
        offset = next;
    }

    let trailer = bytes
        .len()
        .checked_sub(INDEX_SHA1_BYTES)
        .ok_or_else(invalid_index)?;
    while offset < trailer {
        let header_end = offset
            .checked_add(INDEX_EXTENSION_HEADER_BYTES)
            .ok_or_else(invalid_index)?;
        if header_end > trailer {
            return Err(invalid_index());
        }
        if &bytes[offset..offset + 4] == INDEX_SPLIT_LINK_EXTENSION {
            return Err(split_index_unsupported());
        }
        let size = usize::try_from(read_u32(bytes, offset + 4)?).map_err(|_| invalid_index())?;
        offset = header_end.checked_add(size).ok_or_else(invalid_index)?;
    }
    if offset != trailer || empty_path_seen {
        return Err(invalid_index());
    }
    Ok(scan)
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, LinkedWorktreeReclaimError> {
    let end = offset.checked_add(2).ok_or_else(invalid_index)?;
    let slice = bytes.get(offset..end).ok_or_else(invalid_index)?;
    Ok(u16::from_be_bytes([slice[0], slice[1]]))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, LinkedWorktreeReclaimError> {
    let end = offset.checked_add(4).ok_or_else(invalid_index)?;
    let slice = bytes.get(offset..end).ok_or_else(invalid_index)?;
    Ok(u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn find_nul(bytes: &[u8], start: usize) -> Result<usize, LinkedWorktreeReclaimError> {
    let window_end = start
        .checked_add(INDEX_MAX_PATH_BYTES + 1)
        .map_or(bytes.len(), |end| end.min(bytes.len()));
    let window = bytes.get(start..window_end).ok_or_else(invalid_index)?;
    window
        .iter()
        .position(|byte| *byte == 0)
        .map(|position| start + position)
        .ok_or_else(invalid_index)
}

/// Decode Git's offset varint: big-endian groups of seven bits, adding one per continuation.
fn read_index_varint(
    bytes: &[u8],
    start: usize,
) -> Result<(u64, usize), LinkedWorktreeReclaimError> {
    let mut consumed = 0;
    let mut value: u64 = 0;
    loop {
        if consumed == INDEX_MAX_VARINT_BYTES {
            return Err(invalid_index());
        }
        let byte = *bytes.get(start + consumed).ok_or_else(invalid_index)?;
        if consumed > 0 {
            value = value
                .checked_add(1)
                .and_then(|value| value.checked_mul(128))
                .ok_or_else(invalid_index)?;
        }
        value |= u64::from(byte & 0x7f);
        consumed += 1;
        if byte & 0x80 == 0 {
            return Ok((value, consumed));
        }
    }
}

/// Accept only a plain relative path of normal components from an index entry.
fn relative_index_path(bytes: &[u8]) -> Result<&Path, LinkedWorktreeReclaimError> {
    let path = Path::new(OsStr::from_bytes(bytes));
    if path
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
    {
        Ok(path)
    } else {
        Err(invalid_index())
    }
}

fn entry_present(path: &Path) -> Result<bool, LinkedWorktreeReclaimError> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(unavailable()),
    }
}

// --- Submodules ------------------------------------------------------------
//
// `git worktree remove` deletes the worktree's `modules/` directory, and with it every commit,
// branch, stash, and edit that exists only in those submodule repositories. A submodule counts as
// preserved only when all of that is reachable from its remote-tracking refs or is the exact commit
// a superproject remote's default branch records for it: the remote is where `git submodule update`
// fetches that commit from again. Everything else -- an unusual layout, a nested submodule, a
// filter, a single unreached ref -- keeps the worktree.

/// Most submodule repositories one worktree's `modules/` directory may hold. More fail closed.
const MAX_SUBMODULE_REPOSITORIES: usize = 64;

/// Deepest `modules/` nesting searched for repositories (`modules/vendor/bonsplit` is two).
const MAX_SUBMODULE_REPOSITORY_DEPTH: usize = 8;

/// Fingerprints of every submodule repository when all are preserved on their remotes, `None`
/// when any is not. Errors mean the evidence could not be read.
fn observe_submodule_preservation(
    observer: &ProjectCheckoutObserver,
    checkout: &Path,
    git_dir: &Path,
    populated_gitlinks: &[PathBuf],
    executor: &impl TimedCommandExecutor,
) -> Result<Option<Vec<(PathBuf, AdministrativeFingerprint)>>, LinkedWorktreeReclaimError> {
    let Some(repositories) = submodule_repositories(&git_dir.join("modules"))? else {
        return Ok(None);
    };
    // Taken before any submodule content is read, like the worktree's own fingerprint.
    let fingerprints = repositories
        .iter()
        .map(|repository| Ok((repository.clone(), administrative_fingerprint(repository)?)))
        .collect::<Result<Vec<_>, LinkedWorktreeReclaimError>>()?;
    // Each repository must be the checkout of exactly one populated gitlink: a repository no
    // checkout uses keeps an index and a `core.worktree` nothing here inspects.
    let published_bases = superproject_published_bases(observer, checkout, executor)?;
    let mut checked_out = Vec::with_capacity(populated_gitlinks.len());
    for relative in populated_gitlinks {
        let Some(preserved) = submodule_checkout_preserved(
            observer,
            checkout,
            relative,
            &repositories,
            &published_bases,
            executor,
        )?
        else {
            return Ok(None);
        };
        checked_out.push(preserved);
    }
    checked_out.sort_by(|left, right| left.repository.cmp(&right.repository));
    if !checked_out
        .iter()
        .map(|preserved| &preserved.repository)
        .eq(repositories.iter())
    {
        return Ok(None);
    }
    for preserved in &checked_out {
        if !submodule_repository_preserved(
            observer,
            &preserved.repository,
            preserved.published_commit.as_deref(),
            executor,
        )? {
            return Ok(None);
        }
    }
    Ok(Some(fingerprints))
}

/// Every Git repository beneath `modules/`, canonicalized; `None` for any layout this module does
/// not recognize (a symlink, a stray file, a repository nested inside another, a repository with
/// linked worktrees of its own, too many).
fn submodule_repositories(root: &Path) -> Result<Option<Vec<PathBuf>>, LinkedWorktreeReclaimError> {
    let mut repositories = Vec::new();
    let mut pending = vec![(root.to_path_buf(), 0_usize)];
    while let Some((directory, depth)) = pending.pop() {
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && depth == 0 => {
                return Ok(Some(repositories));
            }
            Err(_) => return Err(unavailable()),
        };
        if is_git_directory(&directory)? {
            // Nested submodules, and linked worktrees of the submodule whose per-worktree refs,
            // HEADs and indexes live under `worktrees/` and go with it, are not proven here.
            if depth == 0
                || directory_nonempty(&directory.join("modules"))?
                || directory_nonempty(&directory.join("worktrees"))?
            {
                return Ok(None);
            }
            if repositories.len() >= MAX_SUBMODULE_REPOSITORIES {
                return Ok(None);
            }
            repositories.push(std::fs::canonicalize(&directory).map_err(|_| unavailable())?);
            continue;
        }
        if depth >= MAX_SUBMODULE_REPOSITORY_DEPTH {
            return Ok(None);
        }
        for entry in entries {
            let entry = entry.map_err(|_| unavailable())?;
            let file_type = entry.file_type().map_err(|_| unavailable())?;
            if !file_type.is_dir() {
                return Ok(None);
            }
            pending.push((entry.path(), depth + 1));
        }
    }
    repositories.sort();
    Ok(Some(repositories))
}

fn is_git_directory(directory: &Path) -> Result<bool, LinkedWorktreeReclaimError> {
    let is = |name: &str, directory_expected: bool| match std::fs::symlink_metadata(
        directory.join(name),
    ) {
        Ok(metadata) => Ok(if directory_expected {
            metadata.is_dir()
        } else {
            metadata.is_file()
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(unavailable()),
    };
    Ok(is("HEAD", false)? && is("objects", true)? && is("refs", true)?)
}

/// One populated gitlink path: an absorbed checkout of a repository under `modules/`, clean, at
/// the commit the superproject's HEAD and index both record, with nothing Git would hide from
/// status. Returns that repository, or `None` when anything does not hold.
fn submodule_checkout_preserved(
    observer: &ProjectCheckoutObserver,
    checkout: &Path,
    relative: &Path,
    repositories: &[PathBuf],
    published_bases: &[String],
    executor: &impl TimedCommandExecutor,
) -> Result<Option<PreservedCheckout>, LinkedWorktreeReclaimError> {
    let path = checkout.join(relative);
    // A `.git` directory here is an embedded repository, which removal deletes outright.
    match std::fs::symlink_metadata(path.join(".git")) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => return Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(unavailable()),
    }
    if std::fs::canonicalize(&path).map_err(|_| unavailable())? != path {
        return Ok(None);
    }
    let Ok(git_dir) = absolute_git_path(observer, &path, "--git-dir", executor) else {
        return Ok(None);
    };
    let Ok(git_dir) = std::fs::canonicalize(git_dir) else {
        return Ok(None);
    };
    if !repositories.contains(&git_dir) {
        return Ok(None);
    }
    let top_level = git(observer, &path, &["rev-parse", "--show-toplevel"], executor)?;
    if top_level.stdout.strip_suffix('\n').map(Path::new) != Some(path.as_path()) {
        return Ok(None);
    }

    // Content filters run configured programs during status; the superproject refuses them too.
    let filters = observer
        .git(
            &path,
            &[
                "config",
                "--includes",
                "--get-regexp",
                "^filter\\..*\\.(clean|process)$",
            ],
            executor,
        )
        .map_err(|error| git_failure(&error))?;
    if filters.status != Some(1) || !filters.stdout.is_empty() || !filters.stderr.is_empty() {
        return Ok(None);
    }
    let index = scan_index(&read_index(&git_dir.join("index"))?)?;
    if index.hidden_entries_present || !index.gitlink_paths.is_empty() {
        return Ok(None);
    }
    for marker in OPERATION_MARKERS {
        if entry_present(&git_dir.join(marker))? {
            return Ok(None);
        }
    }
    let status = observer
        .git_with_limits(
            &path,
            &[
                "status",
                "--porcelain=v2",
                "-z",
                "--untracked-files=all",
                "--ignore-submodules=none",
            ],
            crate::project_checkout_observation::MAX_PROJECT_STATUS_OUTPUT_BYTES,
            crate::project_checkout_observation::PROJECT_CHECKOUT_TREE_SCAN_TIMEOUT,
            executor,
        )
        .map_err(|error| git_failure(&error))
        .and_then(require_success)?;
    if !status.stdout.is_empty() {
        return Ok(None);
    }

    let Some(relative_text) = relative.to_str() else {
        return Ok(None);
    };
    let submodule_head = git(
        observer,
        &path,
        &["rev-parse", "--verify", "HEAD^{commit}"],
        executor,
    )?;
    // A submodule added but not yet committed has no recorded commit, so nothing preserves it.
    let recorded = observer
        .git(
            checkout,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("HEAD:{relative_text}"),
            ],
            executor,
        )
        .map_err(|error| git_failure(&error))?;
    if recorded.status != Some(0) {
        return Ok(None);
    }
    let staged = git(
        observer,
        checkout,
        &[
            "ls-files",
            "--stage",
            "-z",
            "--",
            &format!(":(literal){relative_text}"),
        ],
        executor,
    )?;
    let recorded = recorded.stdout.strip_suffix('\n').unwrap_or_default();
    let staged_matches = staged
        .stdout
        .strip_prefix("160000 ")
        .and_then(|rest| rest.split_once(' '))
        .is_some_and(|(object, rest)| {
            object == recorded && rest == format!("0\t{relative_text}\0")
        });
    let at_recorded_commit = !recorded.is_empty()
        && staged_matches
        && submodule_head.stdout.strip_suffix('\n') == Some(recorded);
    if !at_recorded_commit {
        return Ok(None);
    }
    let mut published = false;
    for base in published_bases {
        let at_base = observer
            .git(
                checkout,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("{base}:{relative_text}"),
                ],
                executor,
            )
            .map_err(|error| git_failure(&error))?;
        if at_base.status == Some(0) && at_base.stdout.strip_suffix('\n') == Some(recorded) {
            published = true;
            break;
        }
    }
    Ok(Some(PreservedCheckout {
        repository: git_dir,
        published_commit: published.then(|| recorded.to_owned()),
    }))
}

/// A populated gitlink whose checkout passed [`submodule_checkout_preserved`].
struct PreservedCheckout {
    repository: PathBuf,
    /// The checked-out commit, when a superproject remote's default branch records it at this path.
    published_commit: Option<String>,
}

/// Superproject commits whose recorded gitlinks count as published: HEAD's merge base with each
/// remote's default branch.
///
/// A submodule is fetched by exact commit (`git submodule update` asks the remote for the
/// recorded object), so a clone often holds its checked-out commit without any remote-tracking
/// ref reaching it. A default branch's history is then the evidence the remote has it: it is what
/// review and every fresh checkout go through. Any other pushed branch is not, since Git pushes a
/// superproject that records an unpushed submodule commit without complaint.
fn superproject_published_bases(
    observer: &ProjectCheckoutObserver,
    checkout: &Path,
    executor: &impl TimedCommandExecutor,
) -> Result<Vec<String>, LinkedWorktreeReclaimError> {
    let head = git(
        observer,
        checkout,
        &["rev-parse", "--verify", "HEAD^{commit}"],
        executor,
    )?;
    let head = head
        .stdout
        .strip_suffix('\n')
        .unwrap_or_default()
        .to_owned();
    let defaults = git(
        observer,
        checkout,
        &["for-each-ref", "--format=%(symref)", "refs/remotes/*/HEAD"],
        executor,
    )?;
    let mut bases = Vec::new();
    for target in defaults
        .stdout
        .lines()
        .filter(|line| line.starts_with("refs/remotes/"))
    {
        let base = observer
            .git(checkout, &["merge-base", &head, target], executor)
            .map_err(|error| git_failure(&error))?;
        if base.status == Some(0)
            && let Some(commit) = base.stdout.strip_suffix('\n')
            && !commit.is_empty()
            && !commit.contains('\n')
        {
            bases.push(commit.to_owned());
        }
    }
    Ok(bases)
}

/// Every ref and HEAD of one submodule repository is reachable from its remote-tracking refs, or
/// from `published_commit`, the commit a superproject remote's default branch records for it.
fn submodule_repository_preserved(
    observer: &ProjectCheckoutObserver,
    repository: &Path,
    published_commit: Option<&str>,
    executor: &impl TimedCommandExecutor,
) -> Result<bool, LinkedWorktreeReclaimError> {
    let Some(repository_text) = repository.to_str() else {
        return Ok(false);
    };
    let git_dir = format!("--git-dir={repository_text}");
    let remotes = git(
        observer,
        repository,
        &[
            &git_dir,
            "for-each-ref",
            "--count=1",
            "--format=%(refname)",
            "refs/remotes/",
        ],
        executor,
    )?;
    if remotes.stdout.is_empty() {
        return Ok(false);
    }
    // `--all` is every ref (stash included) plus HEAD; walking stops at the remote-tracking refs
    // and the published commit.
    let mut arguments = vec![
        git_dir.as_str(),
        "rev-list",
        "-n1",
        "--all",
        "--not",
        "--remotes",
    ];
    arguments.extend(published_commit);
    let unreached = git(observer, repository, &arguments, executor)?;
    Ok(unreached.stdout.is_empty())
}

/// A submodule path holds data unless it is absent or an empty directory.
///
/// Status runs with submodules ignored, so anything at a gitlink path -- a repository, a gitfile, or
/// plain files left behind after its `.git` was deleted -- is invisible to it and must veto here.
fn submodule_path_populated(path: &Path) -> Result<bool, LinkedWorktreeReclaimError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => directory_nonempty(path),
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(unavailable()),
    }
}

fn directory_nonempty(path: &Path) -> Result<bool, LinkedWorktreeReclaimError> {
    match std::fs::read_dir(path) {
        Ok(mut entries) => Ok(entries.next().is_some()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(unavailable()),
    }
}

fn last_activity_seconds(
    git_dir: &Path,
    checkout: &Path,
) -> Result<i64, LinkedWorktreeReclaimError> {
    let mut newest = std::fs::symlink_metadata(checkout)
        .map_err(|_| unavailable())?
        .mtime();
    match std::fs::symlink_metadata(git_dir.join(ACTIVITY_ENTRY)) {
        Ok(metadata) => newest = newest.max(metadata.mtime()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(unavailable()),
    }
    if let Some(entry) = newest_reflog_entry_seconds(&git_dir.join(HEAD_REFLOG))? {
        newest = newest.max(entry);
    }
    Ok(newest)
}

/// Time of the newest entry in a reflog, `None` when it is absent or empty.
///
/// An entry is `<old> <new> <identity> <seconds> <zone>\t<message>`; the seconds are the
/// second-to-last field before the tab. A tail that cannot be parsed is unobservable.
fn newest_reflog_entry_seconds(path: &Path) -> Result<Option<i64>, LinkedWorktreeReclaimError> {
    use std::io::{Seek as _, SeekFrom};
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(unavailable()),
    };
    let length = file.metadata().map_err(|_| unavailable())?.len();
    let start = length.saturating_sub(REFLOG_TAIL_BYTES);
    file.seek(SeekFrom::Start(start))
        .map_err(|_| unavailable())?;
    let mut tail = Vec::new();
    file.take(REFLOG_TAIL_BYTES)
        .read_to_end(&mut tail)
        .map_err(|_| unavailable())?;
    parse_newest_reflog_entry(&tail)
}

fn parse_newest_reflog_entry(tail: &[u8]) -> Result<Option<i64>, LinkedWorktreeReclaimError> {
    // The highest time in the tail, not the last line's: an entry written with a backdated
    // committer date (GIT_COMMITTER_DATE) must not hide newer entries before it. The first line
    // may be cut by the tail window, and the last may still be being written, so a line counts
    // only when the field before its time closes an identity (`<email>`); if none does, the
    // reflog is unobservable rather than idle.
    let mut newest = None;
    let mut saw_line = false;
    for line in tail
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        saw_line = true;
        let header = line.split(|byte| *byte == b'\t').next().unwrap_or_default();
        let header = header.strip_suffix(b"\r").unwrap_or(header);
        let mut fields = header.rsplit(|byte| *byte == b' ');
        let _zone = fields.next();
        let seconds = fields
            .next()
            .and_then(|field| std::str::from_utf8(field).ok())
            .and_then(|field| field.parse::<i64>().ok());
        let identity_closed = fields.next().is_some_and(|field| field.ends_with(b">"));
        if let (Some(seconds), true) = (seconds, identity_closed) {
            newest = Some(newest.map_or(seconds, |current: i64| current.max(seconds)));
        }
    }
    match (saw_line, newest) {
        (false, _) => Ok(None),
        (true, Some(seconds)) => Ok(Some(seconds)),
        (true, None) => Err(unavailable()),
    }
}

// --- Execution -------------------------------------------------------------
//
// One reclaim re-observes and re-plans the worktree immediately before acting, so a stale plan never
// authorizes removal. A detached HEAD that needs a pin gets one first, under a create-only ref; that
// ref is the durable checkpoint and outlives any later failure. Removal is plain
// `git worktree remove` without `--force`: Git re-checks cleanliness itself and refuses populated
// submodules and locks, so its guard backs this module's rather than being overridden by it. Fresh
// post-effect observation decides the outcome.
//
// Removal is irreversible for ignored files. For everything else the accurate compensation is
// forward reconstruction: `git worktree add` at the preserved commit.

/// Namespace for commits pinned before their only worktree is removed.
pub const LINKED_WORKTREE_PIN_REF_PREFIX: &str = "refs/glaeda/worktree-pins/";

/// Deadline for each constant-cost Git command of a reclaim sweep: reads, plus the `update-ref`
/// pins and branch deletions, which are safer finished than killed partway. See
/// [`ProjectCheckoutObserver::with_command_timeout`]. The sweep runs in the background, where a
/// read that answers in 30 seconds beats a worktree that stays unobservable for another hour.
pub const LINKED_WORKTREE_GIT_TIMEOUT: Duration = Duration::from_secs(60);

/// Deadline for one `git worktree remove`, which scales with ignored build output in the tree.
pub const LINKED_WORKTREE_REMOVE_TIMEOUT: Duration = Duration::from_secs(600);

const ZERO_OBJECT_ID: &str = "0000000000000000000000000000000000000000";

/// The exact commit and branch a reclaim acted on: what `git worktree add` needs to recreate it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LinkedWorktreeReclaimTarget {
    pub commit: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// A `refs/glaeda/worktree-pins/<commit>` ref was written or already present for this commit.
    pub pinned: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum LinkedWorktreeReclaimOutcome {
    /// The worktree is gone and its registration removed.
    Reclaimed {
        target: LinkedWorktreeReclaimTarget,
        idle_seconds: i64,
    },
    /// Fresh observation no longer supports removal; nothing was changed.
    Refused {
        vetoes: Vec<LinkedWorktreeReclaimVeto>,
    },
    /// Fresh observation failed; nothing was changed.
    Unobservable { code: &'static str },
    /// HEAD or per-worktree Git state changed between observation and removal; the worktree was
    /// kept. A pin written before the change is reported in `target`.
    Changed { target: LinkedWorktreeReclaimTarget },
    /// `git worktree remove` exited non-zero on its own checks, and fresh observation confirms the
    /// worktree is still registered and intact.
    GitRefused { target: LinkedWorktreeReclaimTarget },
    /// A pin or removal ran and fresh observation does not confirm a clean end state. Callers must
    /// stop the batch: this is the circuit breaker.
    Incomplete {
        code: &'static str,
        #[serde(skip_serializing_if = "Option::is_none")]
        target: Option<LinkedWorktreeReclaimTarget>,
    },
}

impl LinkedWorktreeReclaimOutcome {
    /// Whether a batch may continue to the next worktree after this outcome.
    #[must_use]
    pub const fn batch_may_continue(&self) -> bool {
        !matches!(self, Self::Incomplete { .. })
    }

    /// Whether this attempt changed repository or filesystem state.
    #[must_use]
    pub fn mutated(&self) -> bool {
        match self {
            Self::Reclaimed { .. } => true,
            Self::Refused { .. } | Self::Unobservable { .. } => false,
            Self::Changed { target } | Self::GitRefused { target } => target.pinned,
            // Unknown end state: treat as mutated so receipts never understate what happened.
            Self::Incomplete { .. } => true,
        }
    }
}

/// How the removal command ended, as far as the executor could tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemovalExit {
    Succeeded,
    /// Exited normally with a non-zero status: Git's own refusal path.
    ExitedNonZero,
    /// Killed by a signal, timed out, or never reported: state is unknown.
    Interrupted,
}

/// Classify one removal from its exit and fresh post-effect observation.
///
/// Only a normal non-zero exit with the checkout still present, still registered, and observed
/// intact counts as Git refusing. A signal can kill Git after it has started deleting the tree, and
/// that partial state must trip the circuit breaker rather than read as "nothing happened".
const fn classify_removal(
    exit: RemovalExit,
    directory_gone: bool,
    still_registered: bool,
    observed_intact: bool,
) -> RemovalClass {
    match (exit, directory_gone, still_registered, observed_intact) {
        (RemovalExit::Succeeded, true, false, _) => RemovalClass::Reclaimed,
        (RemovalExit::ExitedNonZero, false, true, true) => RemovalClass::GitRefused,
        _ => RemovalClass::Incomplete,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemovalClass {
    Reclaimed,
    GitRefused,
    Incomplete,
}

/// Re-observe one worktree and remove it only if a fresh plan still makes it eligible.
///
/// Never returns an error: every failure becomes a typed outcome so a batch can record it. The
/// returned outcome is the receipt; persisting it is the caller's responsibility (pin refs are the
/// only state this function makes durable on its own).
pub fn reclaim_linked_worktree(
    observer: &ProjectCheckoutObserver,
    inventory: &LinkedWorktreeInventory,
    entry: &LinkedWorktreeEntry,
    policy: LinkedWorktreeReclaimPolicy,
    now_seconds: i64,
    executor: &impl TimedCommandExecutor,
) -> LinkedWorktreeReclaimOutcome {
    // Fresh process evidence for this removal: the run-wide snapshot may be minutes old.
    let evidence = match ProcessUseEvidence::collect(executor) {
        Ok(evidence) => evidence,
        Err(error) => return LinkedWorktreeReclaimOutcome::Unobservable { code: error.code() },
    };
    let observed = match observe_detailed(observer, inventory, entry, &evidence, executor) {
        Ok(observed) => observed,
        Err(error) => return LinkedWorktreeReclaimOutcome::Unobservable { code: error.code() },
    };
    let (compensation, idle_seconds) =
        match plan_linked_worktree_reclaim(&observed.facts, policy, now_seconds)
            .map(|plan| plan.decision)
        {
            Ok(LinkedWorktreeReclaimDecision::Eligible {
                compensation,
                idle_seconds,
                ..
            }) => (compensation, idle_seconds),
            Ok(LinkedWorktreeReclaimDecision::Refused { vetoes }) => {
                return LinkedWorktreeReclaimOutcome::Refused { vetoes };
            }
            Err(error) => return LinkedWorktreeReclaimOutcome::Unobservable { code: error.code() },
        };

    let mut target = LinkedWorktreeReclaimTarget {
        commit: observed.head.clone(),
        branch: observed.branch.clone(),
        pinned: false,
    };
    if compensation == LinkedWorktreeReclaimCompensation::PinHeadCommit {
        // Pin the commit that was observed and planned, never a re-read HEAD.
        if let Err(error) = pin_commit(observer, inventory, &observed.head, executor) {
            return LinkedWorktreeReclaimOutcome::Incomplete {
                code: error.code(),
                target: Some(target),
            };
        }
        target.pinned = true;
    }

    // Final guard, as close to removal as possible. Git's own check covers new untracked files,
    // staging, locks, and path swaps, but not hidden-flag edits or a new detached commit, so any
    // movement of HEAD or per-worktree Git state since observation keeps the worktree. The window
    // that remains is the time between these reads and Git's own status check, or, for a
    // worktree with submodules, the status check below; ref-only changes inside a submodule
    // repository (a new tag or branch) in that window are not seen.
    let head_unchanged = git(
        observer,
        &entry.path,
        &["rev-parse", "--verify", "HEAD^{commit}"],
        executor,
    )
    .is_ok_and(|record| record.stdout.strip_suffix('\n') == Some(observed.head.as_str()));
    let fingerprint_unchanged = administrative_fingerprint(&observed.git_dir)
        .is_ok_and(|fingerprint| fingerprint == observed.fingerprint)
        && observed
            .submodule_fingerprints
            .iter()
            .all(|(repository, before)| {
                administrative_fingerprint(repository).is_ok_and(|after| after == *before)
            });
    if !head_unchanged || !fingerprint_unchanged {
        return LinkedWorktreeReclaimOutcome::Changed { target };
    }
    // Git refuses to remove any worktree with submodules, so a proven one needs `--force`, which
    // also skips Git's own cleanliness check. Run that check here instead, as Git would: the
    // superproject with untracked files shown and submodule contents included. Git's check trips
    // on a `modules/` directory even when it is empty, so that needs `--force` too.
    let force = observed.facts.populated_submodules_present
        || matches!(entry_present(&observed.git_dir.join("modules")), Ok(true));
    if force {
        let clean = observer
            .git_with_limits(
                &entry.path,
                &[
                    "-c",
                    "status.showUntrackedFiles=normal",
                    "status",
                    "--porcelain",
                    "--ignore-submodules=none",
                ],
                crate::project_checkout_observation::MAX_PROJECT_STATUS_OUTPUT_BYTES,
                crate::project_checkout_observation::PROJECT_CHECKOUT_TREE_SCAN_TIMEOUT,
                executor,
            )
            .is_ok_and(|record| {
                record.success
                    && record.status == Some(0)
                    && record.stderr.is_empty()
                    && record.stdout.is_empty()
            });
        if !clean {
            return LinkedWorktreeReclaimOutcome::Changed { target };
        }
    }

    let Some(checkout) = entry.path.to_str() else {
        return LinkedWorktreeReclaimOutcome::Unobservable {
            code: registration_aliased().code(),
        };
    };
    // Repository config could hide untracked files from Git's own cleanliness check.
    let mut arguments = vec![
        "-c",
        "status.showUntrackedFiles=normal",
        "worktree",
        "remove",
    ];
    if force {
        // One `--force` never removes a locked worktree; that takes two.
        arguments.push("--force");
    }
    arguments.extend(["--", checkout]);
    let removal = observer.git_with_limits(
        &inventory.repository,
        &arguments,
        MAX_CAPTURED_STREAM_BYTES,
        LINKED_WORKTREE_REMOVE_TIMEOUT,
        executor,
    );
    let exit = match &removal {
        Ok(record) if record.success && record.status == Some(0) => RemovalExit::Succeeded,
        Ok(record) if matches!(record.status, Some(status) if status != 0) => {
            RemovalExit::ExitedNonZero
        }
        _ => RemovalExit::Interrupted,
    };

    // Fresh post-effect observation decides the outcome, whatever the command reported.
    let directory_gone = matches!(entry_present(&entry.path), Ok(false));
    let still_registered = match git_bounded(
        observer,
        &inventory.repository,
        &["worktree", "list", "--porcelain", "-z"],
        MAX_CAPTURED_STREAM_BYTES,
        executor,
    )
    .and_then(|record| parse_worktree_list(&record.stdout))
    {
        Ok(entries) => entries.iter().any(|listed| listed.path == entry.path),
        Err(error) => {
            return LinkedWorktreeReclaimOutcome::Incomplete {
                code: error.code(),
                target: Some(target),
            };
        }
    };
    let observed_intact = exit == RemovalExit::ExitedNonZero
        && !directory_gone
        && still_registered
        && observe_detailed(observer, inventory, entry, &evidence, executor)
            .is_ok_and(|after| !after.facts.tracked_changes_present && after.head == observed.head);
    match classify_removal(exit, directory_gone, still_registered, observed_intact) {
        RemovalClass::Reclaimed => LinkedWorktreeReclaimOutcome::Reclaimed {
            target,
            idle_seconds,
        },
        RemovalClass::GitRefused => LinkedWorktreeReclaimOutcome::GitRefused { target },
        RemovalClass::Incomplete => LinkedWorktreeReclaimOutcome::Incomplete {
            code: removal_postcondition_failed().code(),
            target: Some(target),
        },
    }
}

/// Pin one exact commit under a create-only ref and confirm it resolves.
fn pin_commit(
    observer: &ProjectCheckoutObserver,
    inventory: &LinkedWorktreeInventory,
    commit: &str,
    executor: &impl TimedCommandExecutor,
) -> Result<(), LinkedWorktreeReclaimError> {
    crate::artifact::CommitId::parse(commit).map_err(|_| invalid_output())?;
    let reference = format!("{LINKED_WORKTREE_PIN_REF_PREFIX}{commit}");
    let existing = observer
        .git(
            &inventory.repository,
            &["rev-parse", "--quiet", "--verify", &reference],
            executor,
        )
        .map_err(|_| unavailable())?;
    if existing.stdout.strip_suffix('\n') != Some(commit) {
        // Create-only: an all-zero old value makes Git refuse to overwrite an existing ref.
        git(
            observer,
            &inventory.repository,
            &["update-ref", &reference, commit, ZERO_OBJECT_ID],
            executor,
        )?;
    }
    let confirmed = git(
        observer,
        &inventory.repository,
        &["rev-parse", "--verify", &reference],
        executor,
    )?;
    if confirmed.stdout.strip_suffix('\n') == Some(commit) {
        Ok(())
    } else {
        Err(pin_unconfirmed())
    }
}

// --- Errors ----------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkedWorktreeReclaimErrorKind {
    InvalidPolicy,
    InvalidRepository,
    InvalidRegistration,
    DisagreeingEvidence,
    Overflow,
    Unavailable,
    InvalidOutput,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LinkedWorktreeReclaimError {
    kind: LinkedWorktreeReclaimErrorKind,
    code: &'static str,
    problem: &'static str,
}

impl LinkedWorktreeReclaimError {
    #[must_use]
    pub const fn kind(&self) -> LinkedWorktreeReclaimErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }

    #[must_use]
    pub const fn problem(&self) -> &'static str {
        self.problem
    }
}

impl fmt::Display for LinkedWorktreeReclaimError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.problem)
    }
}

impl std::error::Error for LinkedWorktreeReclaimError {}

const fn error(
    kind: LinkedWorktreeReclaimErrorKind,
    code: &'static str,
    problem: &'static str,
) -> LinkedWorktreeReclaimError {
    LinkedWorktreeReclaimError {
        kind,
        code,
        problem,
    }
}

const fn idle_window_too_small() -> LinkedWorktreeReclaimError {
    error(
        LinkedWorktreeReclaimErrorKind::InvalidPolicy,
        "idle_window_too_small",
        "the reclaim idle window is below the reviewed minimum",
    )
}

const fn not_main_worktree() -> LinkedWorktreeReclaimError {
    error(
        LinkedWorktreeReclaimErrorKind::InvalidRepository,
        "not_main_worktree",
        "the repository path is not the canonical main worktree of a non-bare repository",
    )
}

const fn foreign_repository() -> LinkedWorktreeReclaimError {
    error(
        LinkedWorktreeReclaimErrorKind::InvalidRepository,
        "foreign_repository",
        "the registered worktree resolves to a checkout of a different repository",
    )
}

const fn too_many_worktrees() -> LinkedWorktreeReclaimError {
    error(
        LinkedWorktreeReclaimErrorKind::InvalidOutput,
        "too_many_worktrees",
        "the repository has more linked worktrees than one inventory may describe",
    )
}

const fn registration_stale() -> LinkedWorktreeReclaimError {
    error(
        LinkedWorktreeReclaimErrorKind::InvalidRegistration,
        "registration_stale",
        "the worktree registration is stale: its directory or .git file is missing",
    )
}

const fn registration_duplicated() -> LinkedWorktreeReclaimError {
    error(
        LinkedWorktreeReclaimErrorKind::InvalidRegistration,
        "registration_duplicated",
        "more than one worktree registration names the same checkout",
    )
}

const fn registration_aliased() -> LinkedWorktreeReclaimError {
    error(
        LinkedWorktreeReclaimErrorKind::InvalidRegistration,
        "registration_aliased",
        "the registered worktree path no longer resolves to itself",
    )
}

const fn administrative_directory_mismatch() -> LinkedWorktreeReclaimError {
    error(
        LinkedWorktreeReclaimErrorKind::InvalidRegistration,
        "administrative_directory_mismatch",
        "the worktree's administrative directory does not point back to this checkout",
    )
}

const fn checkout_unobservable() -> LinkedWorktreeReclaimError {
    error(
        LinkedWorktreeReclaimErrorKind::Unavailable,
        "checkout_unobservable",
        "the checkout could not be observed consistently",
    )
}

const fn checkout_changed() -> LinkedWorktreeReclaimError {
    error(
        LinkedWorktreeReclaimErrorKind::Unavailable,
        "checkout_changed",
        "the checkout changed between two observations; it is probably in use",
    )
}

const fn git_timed_out() -> LinkedWorktreeReclaimError {
    error(
        LinkedWorktreeReclaimErrorKind::Unavailable,
        "git_timed_out",
        "a Git read did not finish before its deadline",
    )
}

const fn index_too_large() -> LinkedWorktreeReclaimError {
    error(
        LinkedWorktreeReclaimErrorKind::InvalidOutput,
        "index_too_large",
        "the worktree index exceeds the reviewed size bound",
    )
}

const fn split_index_unsupported() -> LinkedWorktreeReclaimError {
    error(
        LinkedWorktreeReclaimErrorKind::Unavailable,
        "split_index_unsupported",
        "the worktree uses a split index, whose shared entries this observer does not read",
    )
}

const fn object_format_unsupported() -> LinkedWorktreeReclaimError {
    error(
        LinkedWorktreeReclaimErrorKind::InvalidRepository,
        "object_format_unsupported",
        "the repository object format is outside the reviewed SHA-1 index layout",
    )
}

const fn observation_lost() -> LinkedWorktreeReclaimError {
    error(
        LinkedWorktreeReclaimErrorKind::Unavailable,
        "observation_lost",
        "the observation worker for this worktree did not report a result",
    )
}

const fn pin_unconfirmed() -> LinkedWorktreeReclaimError {
    error(
        LinkedWorktreeReclaimErrorKind::DisagreeingEvidence,
        "pin_unconfirmed",
        "the HEAD pin ref did not resolve to the pinned commit after it was written",
    )
}

const fn removal_postcondition_failed() -> LinkedWorktreeReclaimError {
    error(
        LinkedWorktreeReclaimErrorKind::DisagreeingEvidence,
        "removal_postcondition_failed",
        "after removal the checkout or its registration was observed in an unexpected state",
    )
}

const fn invalid_index() -> LinkedWorktreeReclaimError {
    error(
        LinkedWorktreeReclaimErrorKind::InvalidOutput,
        "invalid_index",
        "the worktree index is malformed or outside the supported v2-v4 layout",
    )
}

const fn idle_overflow() -> LinkedWorktreeReclaimError {
    error(
        LinkedWorktreeReclaimErrorKind::Overflow,
        "idle_overflow",
        "the idle window could not be computed without overflow",
    )
}

const fn process_evidence_unavailable() -> LinkedWorktreeReclaimError {
    error(
        LinkedWorktreeReclaimErrorKind::Unavailable,
        "process_evidence_unavailable",
        "which processes are working in checkouts could not be observed",
    )
}

const fn unavailable() -> LinkedWorktreeReclaimError {
    error(
        LinkedWorktreeReclaimErrorKind::Unavailable,
        "unavailable",
        "worktree evidence could not be read",
    )
}

const fn invalid_output() -> LinkedWorktreeReclaimError {
    error(
        LinkedWorktreeReclaimErrorKind::InvalidOutput,
        "invalid_output",
        "Git returned malformed worktree evidence",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;
    const DAY: i64 = 86_400;

    fn policy() -> LinkedWorktreeReclaimPolicy {
        LinkedWorktreeReclaimPolicy::new(DAY).expect("valid policy")
    }

    fn clean_idle() -> LinkedWorktreeFacts {
        LinkedWorktreeFacts {
            linked: true,
            locked: false,
            operation_in_progress: false,
            tracked_changes_present: false,
            untracked_entry_count: 0,
            hidden_index_entries_present: false,
            populated_submodules_present: false,
            submodules_preserved_on_remotes: true,
            per_worktree_refs_present: false,
            owned_by_current_user: true,
            head_reachability: HeadReachability::ReachableFromSharedRef,
            in_use: false,
            work_state: WorkState::InProgress,
            last_activity_seconds: NOW - 2 * DAY,
        }
    }

    #[test]
    fn a_process_working_inside_vetoes_however_idle() {
        let facts = LinkedWorktreeFacts {
            in_use: true,
            last_activity_seconds: NOW - 30 * DAY,
            ..clean_idle()
        };
        let plan = plan_linked_worktree_reclaim(&facts, policy(), NOW).expect("plan");
        assert_eq!(plan.decision().vetoes(), [LinkedWorktreeReclaimVeto::InUse]);
    }

    #[test]
    fn finished_work_uses_the_shorter_window() {
        let policy =
            LinkedWorktreeReclaimPolicy::with_finished_window(3 * DAY, 3_600).expect("policy");
        let two_hours = LinkedWorktreeFacts {
            last_activity_seconds: NOW - 2 * 3_600,
            ..clean_idle()
        };
        let unfinished = plan_linked_worktree_reclaim(&two_hours, policy, NOW).expect("plan");
        assert_eq!(
            unfinished.decision().vetoes(),
            [LinkedWorktreeReclaimVeto::RecentlyActive]
        );
        assert_eq!(unfinished.idle_window_seconds(), 3 * DAY);
        let finished = LinkedWorktreeFacts {
            work_state: WorkState::Finished,
            ..two_hours
        };
        let plan = plan_linked_worktree_reclaim(&finished, policy, NOW).expect("plan");
        assert!(plan.decision().is_eligible());
        assert_eq!(plan.idle_window_seconds(), 3_600);
        assert_eq!(
            LinkedWorktreeReclaimPolicy::with_finished_window(DAY, 60)
                .unwrap_err()
                .code(),
            "idle_window_too_small"
        );
    }

    #[test]
    fn process_evidence_matches_whole_path_components() {
        let evidence = ProcessUseEvidence::from_paths([
            PathBuf::from("/w/a/src/main.rs"),
            PathBuf::from("/w/b"),
        ]);
        assert!(evidence.uses(Path::new("/w/a")));
        assert!(evidence.uses(Path::new("/w/b")));
        assert!(!evidence.uses(Path::new("/w/a-sibling")));
        assert!(!evidence.uses(Path::new("/w/c")));
        assert_eq!(
            parse_lsof_names("p1\nfcwd\nn/w/a\np2\nfcwd\nn/\nnnot-a-path\n"),
            [PathBuf::from("/w/a"), PathBuf::from("/")]
        );
    }

    #[test]
    fn clean_preserved_idle_worktree_is_eligible_without_compensation() {
        let plan = plan_linked_worktree_reclaim(&clean_idle(), policy(), NOW).expect("plan");
        assert_eq!(
            plan.decision(),
            &LinkedWorktreeReclaimDecision::Eligible {
                authority: LinkedWorktreeReclaimAuthority::PreservedInRepository,
                compensation: LinkedWorktreeReclaimCompensation::NoneRequired,
                idle_seconds: 2 * DAY,
            }
        );
    }

    #[test]
    fn unreachable_detached_head_requires_a_pin_rather_than_refusing() {
        let facts = LinkedWorktreeFacts {
            head_reachability: HeadReachability::OnlyFromThisWorktree,
            ..clean_idle()
        };
        let plan = plan_linked_worktree_reclaim(&facts, policy(), NOW).expect("plan");
        assert!(matches!(
            plan.decision(),
            LinkedWorktreeReclaimDecision::Eligible {
                compensation: LinkedWorktreeReclaimCompensation::PinHeadCommit,
                ..
            }
        ));
    }

    #[test]
    fn submodules_preserved_on_remotes_are_eligible_under_their_own_authority() {
        let facts = LinkedWorktreeFacts {
            populated_submodules_present: true,
            submodules_preserved_on_remotes: true,
            ..clean_idle()
        };
        let plan = plan_linked_worktree_reclaim(&facts, policy(), NOW).expect("plan");
        assert!(matches!(
            plan.decision(),
            LinkedWorktreeReclaimDecision::Eligible {
                authority: LinkedWorktreeReclaimAuthority::PreservedInRepositoryAndSubmoduleRemotes,
                ..
            }
        ));
        let unproven = LinkedWorktreeFacts {
            submodules_preserved_on_remotes: false,
            ..facts
        };
        assert_eq!(
            plan_linked_worktree_reclaim(&unproven, policy(), NOW)
                .expect("plan")
                .decision()
                .vetoes(),
            &[LinkedWorktreeReclaimVeto::PopulatedSubmodulesPresent]
        );
    }

    #[test]
    fn every_applicable_veto_is_reported_in_order() {
        let facts = LinkedWorktreeFacts {
            linked: false,
            locked: true,
            operation_in_progress: true,
            tracked_changes_present: true,
            untracked_entry_count: 3,
            hidden_index_entries_present: true,
            populated_submodules_present: true,
            submodules_preserved_on_remotes: false,
            per_worktree_refs_present: true,
            owned_by_current_user: false,
            head_reachability: HeadReachability::OnlyFromThisWorktree,
            in_use: true,
            work_state: WorkState::InProgress,
            last_activity_seconds: NOW,
        };
        let plan = plan_linked_worktree_reclaim(&facts, policy(), NOW).expect("plan");
        assert_eq!(
            plan.decision().vetoes(),
            &[
                LinkedWorktreeReclaimVeto::MainWorktree,
                LinkedWorktreeReclaimVeto::Locked,
                LinkedWorktreeReclaimVeto::OperationInProgress,
                LinkedWorktreeReclaimVeto::TrackedChangesPresent,
                LinkedWorktreeReclaimVeto::UntrackedEntriesPresent,
                LinkedWorktreeReclaimVeto::HiddenIndexEntriesPresent,
                LinkedWorktreeReclaimVeto::PopulatedSubmodulesPresent,
                LinkedWorktreeReclaimVeto::PerWorktreeRefsPresent,
                LinkedWorktreeReclaimVeto::NotOwnedByCurrentUser,
                LinkedWorktreeReclaimVeto::InUse,
                LinkedWorktreeReclaimVeto::RecentlyActive,
            ]
        );
    }

    #[test]
    fn idle_window_boundary_is_inclusive() {
        let facts = LinkedWorktreeFacts {
            last_activity_seconds: NOW - DAY,
            ..clean_idle()
        };
        assert!(
            plan_linked_worktree_reclaim(&facts, policy(), NOW)
                .expect("plan")
                .decision()
                .is_eligible()
        );
        let facts = LinkedWorktreeFacts {
            last_activity_seconds: NOW - DAY + 1,
            ..clean_idle()
        };
        assert_eq!(
            plan_linked_worktree_reclaim(&facts, policy(), NOW)
                .expect("plan")
                .decision()
                .vetoes(),
            &[LinkedWorktreeReclaimVeto::RecentlyActive]
        );
    }

    #[test]
    fn activity_after_the_clock_reading_is_recent() {
        let facts = LinkedWorktreeFacts {
            last_activity_seconds: NOW + 1,
            ..clean_idle()
        };
        assert_eq!(
            plan_linked_worktree_reclaim(&facts, policy(), NOW)
                .expect("plan")
                .decision()
                .vetoes(),
            &[LinkedWorktreeReclaimVeto::RecentlyActive]
        );
    }

    #[test]
    fn newest_reflog_entry_time_is_read_from_the_last_line() {
        let zero = "0".repeat(40);
        let one = "1".repeat(40);
        let log = format!(
            "{zero} {one} A U Thor <a@example.com> 1790000000 -0700\tclone: from x\n\
             {one} {zero} A U Thor <a@example.com> 1790000100 +0000\tcheckout: moving\n"
        );
        assert_eq!(
            parse_newest_reflog_entry(log.as_bytes()).unwrap(),
            Some(1_790_000_100)
        );
        // a backdated last entry does not hide the newer one before it
        let backdated = format!(
            "{zero} {one} A <a@b> 1790000500 +0000\tcommit: now\n\
             {one} {zero} A <a@b> 1577836800 +0000\tcommit: GIT_COMMITTER_DATE=2020\n"
        );
        assert_eq!(
            parse_newest_reflog_entry(backdated.as_bytes()).unwrap(),
            Some(1_790_000_500)
        );
        // a torn first entry (all-zero old id, part of the new id) is not time 0
        assert_eq!(
            parse_newest_reflog_entry(format!("{zero} 3010").as_bytes())
                .unwrap_err()
                .code(),
            "unavailable"
        );
        assert_eq!(parse_newest_reflog_entry(b"").unwrap(), None);
        assert_eq!(parse_newest_reflog_entry(b"\n\n").unwrap(), None);
        assert_eq!(
            parse_newest_reflog_entry(b"garbage without a time\n")
                .unwrap_err()
                .code(),
            "unavailable"
        );
    }

    #[test]
    fn rewriting_the_reflog_file_is_not_activity() {
        let dir = std::env::temp_dir().join(format!(
            "glaeda-reclaim-reflog-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let checkout = dir.join("checkout");
        let git_dir = dir.join("gitdir");
        std::fs::create_dir_all(git_dir.join("logs")).unwrap();
        std::fs::create_dir_all(&checkout).unwrap();
        std::fs::write(git_dir.join("HEAD"), "ref: refs/heads/x\n").unwrap();
        let zero = "0".repeat(40);
        std::fs::write(
            git_dir.join("logs/HEAD"),
            format!("{zero} {zero} A <a@b> 1000 +0000\tcommit: x\n"),
        )
        .unwrap();
        let old = std::time::UNIX_EPOCH + Duration::from_secs(500);
        for path in [&checkout, &git_dir.join("HEAD")] {
            std::fs::File::open(path)
                .unwrap()
                .set_times(std::fs::FileTimes::new().set_modified(old))
                .unwrap();
        }
        // gc just rewrote logs/HEAD: its mtime is now, its newest entry is still 1000
        let activity = last_activity_seconds(&git_dir, &checkout);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(activity.unwrap(), 1000);
    }

    #[test]
    fn idle_window_below_floor_is_refused() {
        let error = LinkedWorktreeReclaimPolicy::new(MIN_LINKED_WORKTREE_RECLAIM_IDLE_SECONDS - 1)
            .expect_err("too small");
        assert_eq!(error.code(), "idle_window_too_small");
    }

    #[test]
    fn worktree_list_parses_locked_and_prunable_records() {
        let listing = "worktree /repo\0HEAD 1111111111111111111111111111111111111111\0branch refs/heads/main\0\0\
worktree /wt/a\0HEAD 2222222222222222222222222222222222222222\0detached\0locked keep me\0\0\
worktree /wt/b\0HEAD 3333333333333333333333333333333333333333\0branch refs/heads/b\0prunable gitdir file points to non-existent location\0\0";
        let entries = parse_worktree_list(listing).expect("parse");
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].path(), Path::new("/repo"));
        assert!(entries[1].locked() && !entries[1].prunable());
        assert!(!entries[2].locked() && entries[2].prunable());
    }

    #[test]
    fn worktree_list_refuses_relative_paths_and_unterminated_output() {
        assert!(parse_worktree_list("worktree relative\0\0").is_err());
        assert!(parse_worktree_list("worktree /repo").is_err());
        assert!(parse_worktree_list("HEAD 1111\0\0").is_err());
    }

    /// Build one index entry in the v2/v3 layout, padded as Git pads it.
    fn padded_entry(mode: u32, flags: u16, extended: Option<u16>, path: &[u8]) -> Vec<u8> {
        let mut entry = vec![0_u8; INDEX_ENTRY_FIXED_BYTES];
        entry[INDEX_ENTRY_MODE_OFFSET..INDEX_ENTRY_MODE_OFFSET + 4]
            .copy_from_slice(&mode.to_be_bytes());
        let length = u16::try_from(path.len()).expect("short fixture path");
        let flags = flags | length.min(INDEX_NAME_LENGTH_MASK);
        entry[INDEX_ENTRY_FLAGS_OFFSET..INDEX_ENTRY_FLAGS_OFFSET + 2]
            .copy_from_slice(&flags.to_be_bytes());
        if let Some(extended) = extended {
            entry.extend_from_slice(&extended.to_be_bytes());
        }
        entry.extend_from_slice(path);
        let padded = (entry.len() + 8) & !7;
        entry.resize(padded, 0);
        entry
    }

    fn index(version: u32, entries: &[Vec<u8>]) -> Vec<u8> {
        let mut bytes = INDEX_SIGNATURE.to_vec();
        bytes.extend_from_slice(&version.to_be_bytes());
        bytes.extend_from_slice(&u32::try_from(entries.len()).expect("count").to_be_bytes());
        for entry in entries {
            bytes.extend_from_slice(entry);
        }
        bytes.extend_from_slice(&[0_u8; 20]);
        bytes
    }

    #[test]
    fn index_scan_reports_assume_valid_skip_worktree_and_gitlinks() {
        let plain = padded_entry(0o100_644, 0, None, b"a.txt");
        let clean = scan_index(&index(2, std::slice::from_ref(&plain))).expect("scan");
        assert_eq!(clean, IndexScan::default());

        let assumed = padded_entry(0o100_644, INDEX_ASSUME_VALID_FLAG, None, b"b.txt");
        assert!(
            scan_index(&index(2, &[plain.clone(), assumed]))
                .expect("scan")
                .hidden_entries_present
        );

        let skipped = padded_entry(
            0o100_644,
            INDEX_EXTENDED_FLAG,
            Some(INDEX_SKIP_WORKTREE_FLAG),
            b"c.txt",
        );
        assert!(
            scan_index(&index(3, &[plain.clone(), skipped]))
                .expect("scan")
                .hidden_entries_present
        );

        let gitlink = padded_entry(INDEX_GITLINK_MODE, 0, None, b"vendor/sub");
        let scan = scan_index(&index(2, &[plain, gitlink])).expect("scan");
        assert_eq!(scan.gitlink_paths, vec![b"vendor/sub".to_vec()]);
    }

    #[test]
    fn index_scan_refuses_extended_flags_in_v2_and_truncation() {
        let skipped = padded_entry(
            0o100_644,
            INDEX_EXTENDED_FLAG,
            Some(INDEX_SKIP_WORKTREE_FLAG),
            b"c.txt",
        );
        assert!(scan_index(&index(2, std::slice::from_ref(&skipped))).is_err());
        let mut truncated = index(3, &[skipped]);
        truncated.truncate(INDEX_HEADER_BYTES + 20);
        assert!(scan_index(&truncated).is_err());
        assert!(scan_index(b"DIRX\0\0\0\x02\0\0\0\0").is_err());
    }

    #[test]
    fn index_extension_chain_must_end_at_the_trailer_and_refuses_split_index() {
        let plain = padded_entry(0o100_644, 0, None, b"a.txt");
        let mut with_tree = index(2, std::slice::from_ref(&plain));
        let trailer = with_tree.split_off(with_tree.len() - INDEX_SHA1_BYTES);
        with_tree.extend_from_slice(b"TREE");
        with_tree.extend_from_slice(&3_u32.to_be_bytes());
        with_tree.extend_from_slice(b"abc");
        let mut valid = with_tree.clone();
        valid.extend_from_slice(&trailer);
        assert!(scan_index(&valid).is_ok());

        let mut overrun = with_tree.clone();
        overrun.pop();
        overrun.extend_from_slice(&trailer);
        assert_eq!(
            scan_index(&overrun).expect_err("overrun").code(),
            "invalid_index"
        );

        let mut split = index(2, std::slice::from_ref(&plain));
        let trailer = split.split_off(split.len() - INDEX_SHA1_BYTES);
        split.extend_from_slice(INDEX_SPLIT_LINK_EXTENSION);
        split.extend_from_slice(&20_u32.to_be_bytes());
        split.extend_from_slice(&[0_u8; 20]);
        split.extend_from_slice(&trailer);
        assert_eq!(
            scan_index(&split).expect_err("split").code(),
            "split_index_unsupported"
        );
    }

    #[test]
    fn index_v4_reconstructs_prefix_compressed_paths() {
        let mut bytes = INDEX_SIGNATURE.to_vec();
        bytes.extend_from_slice(&4_u32.to_be_bytes());
        bytes.extend_from_slice(&2_u32.to_be_bytes());
        for (mode, strip, suffix) in [
            (0o100_644_u32, 0_u8, b"vendor/a".as_slice()),
            (INDEX_GITLINK_MODE, 1, b"sub".as_slice()),
        ] {
            let mut entry = vec![0_u8; INDEX_ENTRY_FIXED_BYTES];
            entry[INDEX_ENTRY_MODE_OFFSET..INDEX_ENTRY_MODE_OFFSET + 4]
                .copy_from_slice(&mode.to_be_bytes());
            entry.push(strip);
            entry.extend_from_slice(suffix);
            entry.push(0);
            bytes.extend_from_slice(&entry);
        }
        bytes.extend_from_slice(&[0_u8; INDEX_SHA1_BYTES]);
        let scan = scan_index(&bytes).expect("scan");
        assert_eq!(scan.gitlink_paths, vec![b"vendor/sub".to_vec()]);
    }

    #[test]
    fn index_varint_matches_git_offset_encoding() {
        assert_eq!(read_index_varint(&[0x05], 0).expect("one byte"), (5, 1));
        // Git encodes 128 as 0x80 0x00: (0 + 1) * 128 + 0.
        assert_eq!(
            read_index_varint(&[0x80, 0x00], 0).expect("two bytes"),
            (128, 2)
        );
        assert!(read_index_varint(&[0xff; 11], 0).is_err());
    }

    #[test]
    fn only_a_clean_nonzero_exit_with_everything_intact_counts_as_git_refusing() {
        use RemovalClass::{GitRefused, Incomplete, Reclaimed};
        use RemovalExit::{ExitedNonZero, Interrupted, Succeeded};
        assert_eq!(classify_removal(Succeeded, true, false, false), Reclaimed);
        assert_eq!(
            classify_removal(ExitedNonZero, false, true, true),
            GitRefused
        );
        // A signal after deletion started: tree partly gone but still present and registered.
        assert_eq!(classify_removal(Interrupted, false, true, true), Incomplete);
        // Non-zero exit but fresh observation no longer shows an intact tree.
        assert_eq!(
            classify_removal(ExitedNonZero, false, true, false),
            Incomplete
        );
        // Git deleted the tree, then failed on its administrative directory.
        assert_eq!(
            classify_removal(ExitedNonZero, false, false, false),
            Incomplete
        );
        assert_eq!(
            classify_removal(ExitedNonZero, true, true, false),
            Incomplete
        );
        // Success reported but the postcondition does not hold.
        assert_eq!(classify_removal(Succeeded, false, false, false), Incomplete);
        assert_eq!(classify_removal(Succeeded, true, true, false), Incomplete);
    }

    #[test]
    fn index_paths_must_be_plain_relative_components() {
        assert!(relative_index_path(b"vendor/sub").is_ok());
        assert!(relative_index_path(b"../escape").is_err());
        assert!(relative_index_path(b"/absolute").is_err());
    }
}
