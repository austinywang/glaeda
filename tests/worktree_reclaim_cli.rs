#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const BINARY: &str = env!("CARGO_BIN_EXE_glaeda-worktree-reclaim");
const GIT: &str = "/usr/bin/git";
const TOUCH: &str = "/usr/bin/touch";
/// Older than any idle window the tests use.
const LONG_AGO: &str = "202001010000";
/// `LONG_AGO` (2020-01-01 00:00 local) as reflog seconds; any time that old is idle.
const LONG_AGO_SECONDS: &str = "1577836800";
static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

struct Fixture {
    root: PathBuf,
    main: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let sequence = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("current time")
            .as_nanos();
        let temporary_root =
            fs::canonicalize(std::env::temp_dir()).expect("canonicalize test temporary directory");
        let root = temporary_root.join(format!(
            "glaeda-worktree-reclaim-cli-{}-{nonce}-{sequence}",
            std::process::id()
        ));
        let main = root.join("main");
        fs::create_dir_all(&main).expect("create main checkout");
        git(&main, &["init", "-b", "main"]);
        fs::write(main.join("tracked.txt"), "initial\n").expect("write tracked fixture");
        git(&main, &["add", "tracked.txt"]);
        commit(&main, "fixture");
        Self { root, main }
    }

    /// Add a linked worktree on a new branch and return its path.
    fn add(&self, name: &str) -> PathBuf {
        let path = self.root.join(name);
        git(
            &self.main,
            &[
                "worktree",
                "add",
                "-b",
                name,
                path.to_str().expect("UTF-8 fixture path"),
            ],
        );
        path
    }

    /// Make every activity signal of one linked worktree old: its reflog entries' own times and
    /// the modification times of the checkout, HEAD and the reflog file.
    fn age(&self, name: &str) {
        let git_dir = self.main.join(".git/worktrees").join(name);
        let reflog = git_dir.join("logs/HEAD");
        if let Ok(text) = std::fs::read_to_string(&reflog) {
            let aged: String = text
                .lines()
                .map(|line| {
                    let (header, message) = line.split_once('\t').unwrap_or((line, ""));
                    let mut fields: Vec<&str> = header.split(' ').collect();
                    let at = fields.len() - 2;
                    fields[at] = LONG_AGO_SECONDS;
                    format!("{}\t{message}\n", fields.join(" "))
                })
                .collect();
            std::fs::write(&reflog, aged).expect("backdate reflog entries");
        }
        for path in [
            self.root.join(name),
            git_dir.join("HEAD"),
            git_dir.join("logs/HEAD"),
        ] {
            let output = Command::new(TOUCH)
                .args(["-h", "-t", LONG_AGO])
                .arg(&path)
                .output()
                .expect("run touch");
            assert_child_success("backdate fixture", &output);
        }
    }

    /// Make every activity signal of one linked worktree exactly `seconds` old.
    fn age_by(&self, name: &str, seconds: u64) {
        let at = SystemTime::now() - std::time::Duration::from_secs(seconds);
        let epoch = at
            .duration_since(UNIX_EPOCH)
            .expect("epoch")
            .as_secs()
            .to_string();
        let git_dir = self.main.join(".git/worktrees").join(name);
        let reflog = git_dir.join("logs/HEAD");
        let text = fs::read_to_string(&reflog).expect("read reflog");
        let aged: String = text
            .lines()
            .map(|line| {
                let (header, message) = line.split_once('\t').unwrap_or((line, ""));
                let mut fields: Vec<&str> = header.split(' ').collect();
                let at = fields.len() - 2;
                fields[at] = &epoch;
                format!("{}\t{message}\n", fields.join(" "))
            })
            .collect();
        fs::write(&reflog, aged).expect("backdate reflog entries");
        for path in [self.root.join(name), git_dir.join("HEAD"), reflog] {
            fs::File::open(&path)
                .expect("open for times")
                .set_times(fs::FileTimes::new().set_modified(at))
                .expect("backdate");
        }
    }

    fn plan(&self, repository: &Path) -> Output {
        self.run(repository, &[])
    }

    fn run(&self, repository: &Path, extra: &[&str]) -> Output {
        Command::new(BINARY)
            .args([
                "--repository",
                repository.to_str().expect("UTF-8 fixture path"),
                "--output",
                "json",
            ])
            .args(extra)
            .output()
            .expect("run worktree reclaim")
    }

    /// Linked worktree basenames in `git worktree list` order, which the report's ordinals follow.
    fn linked_order(&self) -> Vec<String> {
        let output = git_output(&self.main, &["worktree", "list", "--porcelain"]);
        String::from_utf8(output.stdout)
            .expect("UTF-8 worktree list")
            .lines()
            .filter_map(|line| line.strip_prefix("worktree "))
            .skip(1)
            .map(|path| {
                Path::new(path)
                    .file_name()
                    .expect("worktree basename")
                    .to_string_lossy()
                    .into_owned()
            })
            .collect()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let temporary_root =
            fs::canonicalize(std::env::temp_dir()).expect("canonicalize test temporary directory");
        if self.root.parent() == Some(temporary_root.as_path()) {
            fs::remove_dir_all(&self.root).expect("remove exact fixture root");
        }
    }
}

fn git_output(checkout: &Path, arguments: &[&str]) -> Output {
    let output = Command::new(GIT)
        .arg("-C")
        .arg(checkout)
        .args(arguments)
        .env_clear()
        .env("HOME", checkout)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("LC_ALL", "C")
        .output()
        .expect("run fixture Git");
    assert_child_success("fixture Git", &output);
    output
}

fn git(checkout: &Path, arguments: &[&str]) {
    git_output(checkout, arguments);
}

fn commit(checkout: &Path, message: &str) {
    git(
        checkout,
        &[
            "-c",
            "user.name=Glaeda Test",
            "-c",
            "user.email=glaeda-test@example.invalid",
            "commit",
            "--allow-empty",
            "-m",
            message,
        ],
    );
}

fn assert_child_success(operation: &str, output: &Output) {
    assert!(
        output.status.success(),
        "{operation} failed\nstatus: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn document(output: &Output) -> serde_json::Value {
    assert_child_success("worktree reclaim", output);
    serde_json::from_slice(&output.stdout).expect("JSON report")
}

/// The single repository section of a one-repository report.
fn report(output: &Output) -> serde_json::Value {
    let document = document(output);
    assert_eq!(document["repositories"][0]["result"], "listed");
    document["repositories"][0].clone()
}

/// Landed work goes after the short window, unfinished work waits for the long one, and a process
/// working inside keeps any worktree however old its Git activity is.
#[test]
fn work_state_picks_the_window_and_a_working_process_vetoes() {
    let fixture = Fixture::new();

    // Squash-merged: main gains the same file content in a different commit.
    let landed = fixture.add("landed");
    fs::write(landed.join("feature.txt"), "feature\n").expect("write feature");
    git(&landed, &["add", "feature.txt"]);
    commit(&landed, "feature on its branch");
    fs::write(fixture.main.join("feature.txt"), "feature\n").expect("write squashed feature");
    git(&fixture.main, &["add", "feature.txt"]);
    commit(&fixture.main, "feature (squashed)");
    git(
        &fixture.main,
        &["update-ref", "refs/remotes/origin/main", "main"],
    );
    git(
        &fixture.main,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    fixture.age_by("landed", 2 * 60 * 60);

    // Created from main and never committed to: it did nothing yet, so it is not finished.
    fixture.add("fresh");
    fixture.age_by("fresh", 2 * 60 * 60);

    // Only synced to main (fast-forward), never committed: not finished either.
    let synced = fixture.add("synced");
    git(&synced, &["reset", "-q", "--hard", "HEAD~1"]);
    git(&synced, &["merge", "--ff-only", "main"]);
    let reflog = fs::read_to_string(fixture.main.join(".git/worktrees/synced/logs/HEAD"))
        .expect("synced reflog");
    assert!(
        reflog.contains("Fast-forward"),
        "the sync really moved HEAD"
    );
    fixture.age_by("synced", 2 * 60 * 60);

    // Renamed tracked.txt to renamed.txt; main gained renamed.txt but kept tracked.txt.
    let renamed = fixture.add("renamed");
    git(&renamed, &["mv", "tracked.txt", "renamed.txt"]);
    commit(&renamed, "rename");
    fs::write(fixture.main.join("renamed.txt"), "initial\n").expect("write renamed copy");
    git(&fixture.main, &["add", "renamed.txt"]);
    commit(&fixture.main, "copy, not rename");
    git(
        &fixture.main,
        &["update-ref", "refs/remotes/origin/main", "main"],
    );
    fixture.age_by("renamed", 2 * 60 * 60);

    let unfinished = fixture.add("unfinished");
    fs::write(unfinished.join("wip.txt"), "wip\n").expect("write wip");
    git(&unfinished, &["add", "wip.txt"]);
    commit(&unfinished, "work in progress");
    fixture.age_by("unfinished", 2 * 60 * 60);

    let busy = fixture.add("busy");
    fixture.age("busy");
    let mut sleeper = Command::new("/bin/sleep")
        .arg("60")
        .current_dir(&busy)
        .spawn()
        .expect("start a process working in the worktree");

    let output = fixture.plan(&fixture.main);
    let _ = sleeper.kill();
    let _ = sleeper.wait();
    let document = document(&output);
    assert_eq!(document["finished_idle_seconds"], 3_600);
    assert_eq!(document["minimum_idle_seconds"], 3 * 24 * 60 * 60);
    let report = document["repositories"][0].clone();
    let order = fixture.linked_order();
    let by_name = |name: &str| {
        let ordinal = order
            .iter()
            .position(|entry| entry == name)
            .expect("listed")
            + 1;
        report["worktrees"]
            .as_array()
            .expect("worktree array")
            .iter()
            .find(|worktree| worktree["ordinal"] == ordinal)
            .expect("reported ordinal")
            .clone()
    };

    let landed = by_name("landed");
    assert_eq!(landed["facts"]["work_state"], "finished");
    assert_eq!(landed["decision"]["decision"], "eligible");

    let unfinished = by_name("unfinished");
    assert_eq!(unfinished["facts"]["work_state"], "in_progress");
    assert_eq!(
        unfinished["decision"]["vetoes"],
        serde_json::json!(["recently_active"])
    );

    for name in ["fresh", "synced", "renamed"] {
        let worktree = by_name(name);
        assert_eq!(worktree["facts"]["work_state"], "in_progress", "{name}");
        assert_eq!(
            worktree["decision"]["vetoes"],
            serde_json::json!(["recently_active"]),
            "{name}"
        );
    }

    let busy = by_name("busy");
    assert_eq!(busy["facts"]["in_use"], true);
    assert_eq!(busy["decision"]["vetoes"], serde_json::json!(["in_use"]));
}

/// Every worktree state the planner distinguishes, observed through the real binary and real Git.
#[test]
fn plan_classifies_real_worktrees_and_changes_nothing() {
    let fixture = Fixture::new();

    fixture.add("clean");
    fixture.age("clean");

    // A detached commit no branch reaches: removing the worktree would orphan it.
    let detached = fixture.add("detached");
    git(&detached, &["checkout", "--detach"]);
    commit(&detached, "only reachable from this worktree");
    git(&fixture.main, &["branch", "-D", "detached"]);
    fixture.age("detached");

    let dirty = fixture.add("dirty");
    fs::write(dirty.join("tracked.txt"), "edited\n").expect("edit tracked file");
    fs::write(dirty.join("scratch.txt"), "local\n").expect("write untracked file");
    fixture.age("dirty");

    let locked = fixture.add("locked");
    git(
        &fixture.main,
        &["worktree", "lock", locked.to_str().expect("UTF-8")],
    );
    fixture.age("locked");

    fixture.add("recent");

    let missing = fixture.add("missing");
    fs::remove_dir_all(&missing).expect("remove worktree directory behind Git's back");

    let listing_before = git_output(&fixture.main, &["worktree", "list", "--porcelain"]).stdout;
    let refs_before = git_output(&fixture.main, &["for-each-ref"]).stdout;

    let document = document(&fixture.plan(&fixture.main));
    assert_eq!(document["document_type"], "glaeda-worktree-reclaim-plan");
    assert_eq!(document["mutation_performed"], false);
    let report = document["repositories"][0].clone();
    assert_eq!(report["summary"]["linked"], 6);
    assert_eq!(report["summary"]["eligible"], 2);
    assert_eq!(report["summary"]["eligible_requiring_head_pin"], 1);
    assert_eq!(report["summary"]["refused"], 3);
    assert_eq!(report["summary"]["prunable"], 1);
    assert_eq!(report["summary"]["unobservable"], 0);

    let order = fixture.linked_order();
    let by_name = |name: &str| {
        let ordinal = order
            .iter()
            .position(|entry| entry == name)
            .expect("listed")
            + 1;
        report["worktrees"]
            .as_array()
            .expect("worktree array")
            .iter()
            .find(|worktree| worktree["ordinal"] == ordinal)
            .expect("reported ordinal")
            .clone()
    };

    let clean = by_name("clean");
    assert_eq!(clean["decision"]["decision"], "eligible");
    assert_eq!(clean["decision"]["compensation"], "none_required");

    let detached = by_name("detached");
    assert_eq!(detached["decision"]["decision"], "eligible");
    assert_eq!(detached["decision"]["compensation"], "pin_head_commit");
    assert_eq!(
        detached["facts"]["head_reachability"],
        "only_from_this_worktree"
    );

    assert_eq!(
        by_name("dirty")["decision"]["vetoes"],
        serde_json::json!(["tracked_changes_present", "untracked_entries_present"])
    );
    assert_eq!(
        by_name("locked")["decision"]["vetoes"],
        serde_json::json!(["locked"])
    );
    assert_eq!(
        by_name("recent")["decision"]["vetoes"],
        serde_json::json!(["recently_active"])
    );
    assert_eq!(by_name("missing")["result"], "prunable");

    // Each worktree carries its registered path, so an operator never maps ordinals by hand.
    for name in &order {
        let path = by_name(name)["path"]
            .as_str()
            .expect("worktree path")
            .to_owned();
        assert!(path.ends_with(&format!("/{name}")), "{path} is not {name}");
    }

    assert_eq!(
        git_output(&fixture.main, &["worktree", "list", "--porcelain"]).stdout,
        listing_before
    );
    assert_eq!(
        git_output(&fixture.main, &["for-each-ref"]).stdout,
        refs_before
    );
    assert!(dirty.join("scratch.txt").exists());
}

#[test]
fn a_linked_worktree_is_not_accepted_as_the_repository() {
    let fixture = Fixture::new();
    let linked = fixture.add("linked");
    let output = fixture.plan(&linked);
    assert_eq!(
        output.status.code(),
        Some(2),
        "an unlisted repository must not pass silently"
    );
    let document: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON report");
    assert_eq!(document["repositories"][0]["result"], "unlisted");
    assert_eq!(document["repositories"][0]["code"], "not_main_worktree");
}

fn git_as_user(checkout: &Path, arguments: &[&str]) {
    let mut full = vec![
        "-c",
        "user.name=Glaeda Test",
        "-c",
        "user.email=glaeda-test@example.invalid",
        "-c",
        "protocol.file.allow=always",
    ];
    full.extend_from_slice(arguments);
    git(checkout, &full);
}

fn entry_by_name(fixture: &Fixture, report: &serde_json::Value, name: &str) -> serde_json::Value {
    let ordinal = fixture
        .linked_order()
        .iter()
        .position(|entry| entry == name)
        .expect("listed")
        + 1;
    report["worktrees"]
        .as_array()
        .expect("worktree array")
        .iter()
        .find(|worktree| worktree["ordinal"] == ordinal)
        .expect("reported ordinal")
        .clone()
}

fn vetoes_of(entry: &serde_json::Value) -> Vec<String> {
    entry["decision"]["vetoes"]
        .as_array()
        .unwrap_or_else(|| panic!("refused entry expected, got {entry}"))
        .iter()
        .map(|veto| veto.as_str().expect("veto code").to_owned())
        .collect()
}

/// Regression fixtures for state `git status` does not show but removal would destroy.
#[test]
fn hidden_unique_state_is_never_eligible() {
    let fixture = Fixture::new();

    // Edits hidden from status by index flags. skip-worktree also forces an index v3 layout.
    let assumed = fixture.add("assumed");
    git(
        &assumed,
        &["update-index", "--assume-unchanged", "tracked.txt"],
    );
    fs::write(assumed.join("tracked.txt"), "hidden edit\n").expect("edit assumed file");
    fixture.age("assumed");

    let skipped = fixture.add("skipped");
    git(
        &skipped,
        &["update-index", "--skip-worktree", "tracked.txt"],
    );
    fs::write(skipped.join("tracked.txt"), "hidden edit\n").expect("edit skipped file");
    fixture.age("skipped");

    // A submodule whose repository lives inside the worktree as a `.git` directory.
    let embedded = fixture.add("embedded");
    let nested = embedded.join("nested");
    fs::create_dir(&nested).expect("create nested repository");
    git(&nested, &["init", "-b", "main"]);
    commit(&nested, "unique nested commit");
    git_as_user(&embedded, &["submodule", "add", "./nested", "nested"]);
    git_as_user(&embedded, &["commit", "-m", "add embedded submodule"]);
    fixture.age("embedded");

    // A submodule cloned normally, which Git absorbs into the worktree's own modules directory.
    let source = fixture.root.join("submodule-source");
    fs::create_dir(&source).expect("create submodule source");
    git(&source, &["init", "-b", "main"]);
    commit(&source, "source commit");
    commit(&source, "second source commit");
    let add_submodule = |name: &str| {
        let worktree = fixture.add(name);
        git_as_user(
            &worktree,
            &["submodule", "add", source.to_str().expect("UTF-8"), "sub"],
        );
        git_as_user(&worktree, &["commit", "-m", "add absorbed submodule"]);
        worktree
    };
    // Clean, at the recorded commit, and on its remote: preserved, so it does not veto.
    add_submodule("absorbed");
    fixture.age("absorbed");
    // A submodule commit recorded by the superproject but never pushed.
    let unpushed = add_submodule("sub-unpushed");
    commit(&unpushed.join("sub"), "only this submodule has it");
    git_as_user(
        &unpushed,
        &["commit", "-am", "record unpushed submodule commit"],
    );
    fixture.age("sub-unpushed");
    // An untracked file inside the submodule, which superproject status does not see.
    let dirty = add_submodule("sub-dirty");
    fs::write(dirty.join("sub/scratch.txt"), "local\n").expect("write submodule scratch");
    fixture.age("sub-dirty");
    // A submodule checked out away from the commit the superproject records.
    let moved = add_submodule("sub-moved");
    git(&moved.join("sub"), &["checkout", "--detach", "HEAD~1"]);
    fixture.age("sub-moved");
    // A submodule with a linked worktree of its own, whose per-worktree state lives in the
    // submodule repository that removal deletes.
    let with_worktree = add_submodule("sub-worktree");
    let submodule_worktree = fixture.root.join("submodule-own-worktree");
    git(
        &with_worktree.join("sub"),
        &[
            "worktree",
            "add",
            "--detach",
            submodule_worktree.to_str().expect("UTF-8"),
        ],
    );
    fixture.age("sub-worktree");

    // A per-worktree ref pointing at a commit nothing else reaches.
    let per_worktree = fixture.add("per-worktree");
    commit(&per_worktree, "only a per-worktree ref reaches this");
    git(&per_worktree, &["update-ref", "refs/worktree/keep", "HEAD"]);
    git(&per_worktree, &["reset", "--hard", "HEAD~1"]);
    fixture.age("per-worktree");

    // An interrupted cherry-pick.
    git(&fixture.main, &["branch", "conflict-source"]);
    let conflict_source = fixture.root.join("conflict-source-wt");
    git(
        &fixture.main,
        &[
            "worktree",
            "add",
            conflict_source.to_str().expect("UTF-8"),
            "conflict-source",
        ],
    );
    fs::write(conflict_source.join("tracked.txt"), "theirs\n").expect("write theirs");
    git_as_user(&conflict_source, &["commit", "-am", "theirs"]);
    let operation = fixture.add("operation");
    fs::write(operation.join("tracked.txt"), "ours\n").expect("write ours");
    git_as_user(&operation, &["commit", "-am", "ours"]);
    let status = Command::new(GIT)
        .arg("-C")
        .arg(&operation)
        .args([
            "-c",
            "user.name=Glaeda Test",
            "-c",
            "user.email=glaeda-test@example.invalid",
            "cherry-pick",
            "conflict-source",
        ])
        .env_clear()
        .env("HOME", &operation)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .expect("run conflicting cherry-pick");
    assert!(
        !status.status.success(),
        "cherry-pick must stop on conflict"
    );
    fixture.age("operation");

    let report = report(&fixture.plan(&fixture.main));
    assert_eq!(
        vetoes_of(&entry_by_name(&fixture, &report, "assumed")),
        ["hidden_index_entries_present"]
    );
    assert_eq!(
        vetoes_of(&entry_by_name(&fixture, &report, "skipped")),
        ["hidden_index_entries_present"]
    );
    assert_eq!(
        vetoes_of(&entry_by_name(&fixture, &report, "embedded")),
        ["populated_submodules_present"]
    );
    let absorbed = entry_by_name(&fixture, &report, "absorbed");
    assert_eq!(absorbed["decision"]["decision"], "eligible", "{absorbed}");
    assert_eq!(
        absorbed["decision"]["authority"],
        "preserved_in_repository_and_submodule_remotes"
    );
    for name in ["sub-unpushed", "sub-dirty", "sub-moved", "sub-worktree"] {
        assert_eq!(
            vetoes_of(&entry_by_name(&fixture, &report, name)),
            ["populated_submodules_present"],
            "{name}"
        );
    }
    assert_eq!(
        vetoes_of(&entry_by_name(&fixture, &report, "per-worktree")),
        ["per_worktree_refs_present"]
    );
    assert!(
        vetoes_of(&entry_by_name(&fixture, &report, "operation"))
            .contains(&"operation_in_progress".to_owned())
    );
}

/// Only branches, tags, and remote-tracking refs preserve a detached HEAD.
#[test]
fn detached_head_preservation_ignores_short_lived_refs() {
    let fixture = Fixture::new();

    let reachable = fixture.add("reachable");
    git(&reachable, &["checkout", "--detach"]);
    fixture.age("reachable");

    // The stash contains this commit, but a stash drop would orphan it.
    let stashed = fixture.add("stashed");
    git(&stashed, &["checkout", "--detach"]);
    commit(&stashed, "reached only by the stash");
    fs::write(stashed.join("tracked.txt"), "to stash\n").expect("edit for stash");
    git_as_user(&stashed, &["stash"]);
    git(&fixture.main, &["branch", "-D", "stashed"]);
    fixture.age("stashed");

    let report = report(&fixture.plan(&fixture.main));
    let reachable = entry_by_name(&fixture, &report, "reachable");
    assert_eq!(reachable["decision"]["compensation"], "none_required");
    let stashed = entry_by_name(&fixture, &report, "stashed");
    assert_eq!(stashed["decision"]["compensation"], "pin_head_commit");
}

/// A registration whose path became a symlink to another worktree must not borrow its facts.
#[test]
fn aliased_registration_is_unobservable() {
    let fixture = Fixture::new();
    let target = fixture.add("target");
    fixture.age("target");
    let alias = fixture.add("alias");
    fs::rename(&alias, fixture.root.join("alias-moved")).expect("move alias checkout away");
    std::os::unix::fs::symlink(&target, &alias).expect("replace alias with symlink");

    let report = report(&fixture.plan(&fixture.main));
    let alias = entry_by_name(&fixture, &report, "alias");
    assert_eq!(alias["result"], "unobservable");
    assert_eq!(alias["code"], "registration_aliased");
    assert_eq!(
        entry_by_name(&fixture, &report, "target")["decision"]["decision"],
        "eligible"
    );
}

/// Layouts the index scanner must either read correctly or refuse, never misread.
#[test]
fn index_layouts_are_read_or_refused_never_misread() {
    let fixture = Fixture::new();

    // Index v4 prefix-compresses paths; skip-worktree must still be found through it.
    let compressed = fixture.add("compressed");
    git(&compressed, &["update-index", "--index-version", "4"]);
    git(
        &compressed,
        &["update-index", "--skip-worktree", "tracked.txt"],
    );
    fs::write(compressed.join("tracked.txt"), "hidden edit\n").expect("edit v4 file");
    fixture.age("compressed");

    // A split index keeps entries and flags in a shared file this observer does not read.
    let split = fixture.add("split");
    git(
        &split,
        &["update-index", "--assume-unchanged", "tracked.txt"],
    );
    git(&split, &["update-index", "--split-index"]);
    fs::write(split.join("tracked.txt"), "hidden edit\n").expect("edit split file");
    git(
        &split,
        &[
            "-c",
            "splitIndex.maxPercentChange=0",
            "update-index",
            "--split-index",
        ],
    );
    fixture.age("split");

    // A submodule path whose repository was deleted but whose files remain.
    let source = fixture.root.join("submodule-source");
    fs::create_dir(&source).expect("create submodule source");
    git(&source, &["init", "-b", "main"]);
    commit(&source, "source commit");
    let orphaned = fixture.add("orphaned");
    git_as_user(
        &orphaned,
        &["submodule", "add", source.to_str().expect("UTF-8"), "sub"],
    );
    git_as_user(&orphaned, &["commit", "-m", "add submodule"]);
    fs::remove_file(orphaned.join("sub/.git")).expect("remove submodule gitfile");
    fs::remove_dir_all(fixture.main.join(".git/worktrees/orphaned/modules"))
        .expect("remove absorbed submodule repository");
    fs::write(orphaned.join("sub/work.txt"), "local only\n").expect("write orphaned file");
    fixture.age("orphaned");

    let report = report(&fixture.plan(&fixture.main));
    assert_eq!(
        vetoes_of(&entry_by_name(&fixture, &report, "compressed")),
        ["hidden_index_entries_present"]
    );
    let split = entry_by_name(&fixture, &report, "split");
    assert_eq!(split["result"], "unobservable");
    assert_eq!(split["code"], "split_index_unsupported");
    assert_eq!(
        vetoes_of(&entry_by_name(&fixture, &report, "orphaned")),
        ["populated_submodules_present"]
    );
}

/// `worktree add --relative-paths` (Git 2.48+) records a relative backlink that must still bind.
#[test]
fn relative_path_worktrees_are_observable() {
    let fixture = Fixture::new();
    let path = fixture.root.join("relative");
    let added = Command::new(GIT)
        .arg("-C")
        .arg(&fixture.main)
        .args([
            "worktree",
            "add",
            "--relative-paths",
            "-b",
            "relative",
            path.to_str().expect("UTF-8"),
        ])
        .env_clear()
        .env("HOME", &fixture.main)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .expect("run git worktree add");
    if !added.status.success() {
        // Older Git has no relative worktree links, so there is nothing to bind.
        assert!(
            String::from_utf8_lossy(&added.stderr).contains("relative-paths"),
            "unexpected worktree add failure: {}",
            String::from_utf8_lossy(&added.stderr)
        );
        return;
    }
    fixture.age("relative");
    let report = report(&fixture.plan(&fixture.main));
    assert_eq!(
        entry_by_name(&fixture, &report, "relative")["decision"]["decision"],
        "eligible"
    );
}

fn ref_exists(repository: &Path, reference: &str) -> bool {
    Command::new(GIT)
        .arg("-C")
        .arg(repository)
        .args(["rev-parse", "--quiet", "--verify", reference])
        .output()
        .expect("run rev-parse")
        .status
        .success()
}

/// `--apply` removes exactly the eligible worktrees, pins an unreachable HEAD first, and keeps
/// everything a fresh check refuses.
#[test]
fn apply_reclaims_only_eligible_worktrees_and_pins_orphan_heads() {
    let fixture = Fixture::new();

    let clean = fixture.add("clean");
    fixture.age("clean");

    let detached = fixture.add("detached");
    git(&detached, &["checkout", "--detach"]);
    commit(&detached, "only this worktree reaches it");
    let orphan = String::from_utf8(git_output(&detached, &["rev-parse", "HEAD"]).stdout)
        .expect("UTF-8")
        .trim()
        .to_owned();
    git(&fixture.main, &["branch", "-D", "detached"]);
    fixture.age("detached");

    let dirty = fixture.add("dirty");
    fs::write(dirty.join("scratch.txt"), "local\n").expect("write untracked file");
    fixture.age("dirty");

    let recent = fixture.add("recent");

    let receipt = document(&fixture.run(&fixture.main, &["--apply"]));
    assert_eq!(receipt["document_type"], "glaeda-worktree-reclaim-receipt");
    assert_eq!(receipt["mutation_performed"], true);
    assert_eq!(receipt["circuit_breaker_tripped"], false);
    assert_eq!(receipt["repositories"][0]["summary"]["reclaimed"], 2);
    let outcomes = receipt["repositories"][0]["worktrees"]
        .as_array()
        .expect("worktrees")
        .iter()
        .filter_map(|worktree| worktree.get("reclaim"))
        .collect::<Vec<_>>();
    assert!(outcomes.iter().any(|outcome| {
        outcome["target"]["branch"] == "clean"
            && outcome["target"]["commit"]
                .as_str()
                .is_some_and(|commit| commit.len() == 40)
    }));
    assert!(
        outcomes
            .iter()
            .any(|outcome| outcome["target"]["commit"] == orphan.as_str()
                && outcome["target"]["pinned"] == true)
    );

    assert!(!clean.exists(), "clean worktree removed");
    assert!(!detached.exists(), "detached worktree removed");
    assert!(
        ref_exists(&fixture.main, "refs/heads/clean"),
        "branch survives removal"
    );
    assert!(
        ref_exists(
            &fixture.main,
            &format!("refs/glaeda/worktree-pins/{orphan}")
        ),
        "orphan HEAD pinned before removal"
    );
    assert!(dirty.join("scratch.txt").exists(), "dirty worktree kept");
    assert!(recent.exists(), "recent worktree kept");

    let listed = fixture.linked_order();
    assert_eq!(listed, ["dirty", "recent"]);

    // A second run finds nothing left to do and changes nothing.
    let again = document(&fixture.run(&fixture.main, &["--apply"]));
    assert_eq!(again["mutation_performed"], false);
    assert_eq!(again["repositories"][0]["summary"]["eligible"], 0);
}

/// Git refuses to remove any worktree with submodules; a preserved one is removed with its
/// submodule repositories, and one whose submodule holds local data is kept.
#[test]
fn apply_reclaims_a_worktree_whose_submodules_are_on_their_remotes() {
    let fixture = Fixture::new();
    let source = fixture.root.join("submodule-source");
    fs::create_dir(&source).expect("create submodule source");
    git(&source, &["init", "-b", "main"]);
    commit(&source, "source commit");
    let add_submodule = |name: &str| {
        let worktree = fixture.add(name);
        git_as_user(
            &worktree,
            &["submodule", "add", source.to_str().expect("UTF-8"), "sub"],
        );
        git_as_user(&worktree, &["commit", "-m", "add submodule"]);
        worktree
    };
    let preserved = add_submodule("preserved");
    fixture.age("preserved");
    let dirty = add_submodule("dirty");
    fs::write(dirty.join("sub/scratch.txt"), "local\n").expect("write submodule scratch");
    fixture.age("dirty");

    let receipt = document(&fixture.run(&fixture.main, &["--apply"]));
    assert_eq!(receipt["circuit_breaker_tripped"], false);
    assert_eq!(receipt["repositories"][0]["summary"]["reclaimed"], 1);
    assert!(!preserved.exists(), "preserved worktree removed");
    assert!(
        !fixture.main.join(".git/worktrees/preserved").exists(),
        "its submodule repositories went with it"
    );
    assert!(
        dirty.join("sub/scratch.txt").exists(),
        "dirty submodule kept"
    );
    assert!(
        ref_exists(&source, "refs/heads/main"),
        "the submodule's remote is untouched"
    );
    assert_eq!(fixture.linked_order(), ["dirty"]);
}

/// `git submodule update` fetches the recorded commit itself, so a clone often has no
/// remote-tracking ref reaching its HEAD. A superproject remote's default branch that records it
/// counts as the evidence; an unpushed recording, or one pushed only on another branch, does not.
#[test]
fn a_submodule_commit_recorded_by_published_superproject_history_is_preserved() {
    let fixture = Fixture::new();
    let remote = fixture.root.join("super-remote.git");
    git(
        &fixture.root,
        &[
            "init",
            "--bare",
            "-b",
            "main",
            remote.to_str().expect("UTF-8"),
        ],
    );
    git(
        &fixture.main,
        &["remote", "add", "origin", remote.to_str().expect("UTF-8")],
    );
    let source = fixture.root.join("submodule-source");
    fs::create_dir(&source).expect("create submodule source");
    git(&source, &["init", "-b", "main"]);
    commit(&source, "source commit");
    git(&source, &["checkout", "-b", "side"]);
    commit(&source, "side commit");
    git(&source, &["checkout", "main"]);

    let pinned_submodule = |name: &str| {
        let worktree = fixture.add(name);
        git_as_user(
            &worktree,
            &["submodule", "add", source.to_str().expect("UTF-8"), "sub"],
        );
        let sub = worktree.join("sub");
        git(&sub, &["checkout", "--detach", "origin/side"]);
        // As if only the recorded commit had been fetched: no remote-tracking ref reaches it.
        git(&sub, &["update-ref", "-d", "refs/remotes/origin/side"]);
        git_as_user(&worktree, &["add", "sub"]);
        // The name keeps the two recording commits distinct: made in the same second, identical
        // commits would share an id, and the pushed one would publish both.
        git_as_user(
            &worktree,
            &["commit", "-m", &format!("record the side commit in {name}")],
        );
        worktree
    };
    // Landed on the superproject remote's default branch.
    let published = pinned_submodule("published");
    git(&published, &["push", "origin", "published:main"]);
    git(&fixture.main, &["remote", "set-head", "origin", "main"]);
    fixture.age("published");
    pinned_submodule("unpublished");
    fixture.age("unpublished");
    // A pushed branch recording a submodule commit only this worktree has: Git pushes the
    // superproject without the submodule, so a branch other than the default proves nothing.
    let branch_pushed = pinned_submodule("branch-pushed");
    commit(&branch_pushed.join("sub"), "only this submodule has it");
    git_as_user(
        &branch_pushed,
        &["commit", "-am", "record a submodule commit nobody pushed"],
    );
    git(&branch_pushed, &["push", "origin", "branch-pushed"]);
    fixture.age("branch-pushed");

    let report = report(&fixture.plan(&fixture.main));
    let published = entry_by_name(&fixture, &report, "published");
    assert_eq!(published["decision"]["decision"], "eligible", "{published}");
    assert_eq!(
        published["decision"]["authority"],
        "preserved_in_repository_and_submodule_remotes"
    );
    for name in ["unpublished", "branch-pushed"] {
        assert_eq!(
            vetoes_of(&entry_by_name(&fixture, &report, name)),
            ["populated_submodules_present"],
            "{name}"
        );
    }
}

#[test]
fn apply_stops_at_the_reclaim_budget() {
    let fixture = Fixture::new();
    let first = fixture.add("first");
    fixture.age("first");
    let second = fixture.add("second");
    fixture.age("second");

    let document = document(&fixture.run(&fixture.main, &["--apply", "--max-reclaims", "1"]));
    assert_eq!(document["budget_exhausted"], true);
    assert_eq!(document["repositories"][0]["summary"]["reclaimed"], 1);
    assert_eq!(
        usize::from(first.exists()) + usize::from(second.exists()),
        1,
        "exactly one worktree removed"
    );
}

/// A removal Git starts but cannot finish trips the circuit breaker and stops the batch.
#[test]
fn a_partial_removal_trips_the_circuit_breaker() {
    use std::os::unix::fs::PermissionsExt as _;

    let fixture = Fixture::new();
    fs::write(fixture.main.join(".git/info/exclude"), "build/\n").expect("ignore build output");
    let first = fixture.add("first");
    fixture.age("first");
    let second = fixture.add("second");
    fixture.age("second");
    let order = fixture.linked_order();
    let (stuck, untouched) = if order[0] == "first" {
        (first, second)
    } else {
        (second, first)
    };
    let build = stuck.join("build");
    fs::create_dir(&build).expect("create ignored build directory");
    fs::write(build.join("output.o"), "binary\n").expect("write ignored output");
    fs::set_permissions(&build, fs::Permissions::from_mode(0o555)).expect("make build read-only");
    // Re-age after creating the directory changed the checkout root mtime.
    fixture.age(
        stuck
            .file_name()
            .and_then(|name| name.to_str())
            .expect("name"),
    );

    let output = fixture.run(&fixture.main, &["--apply"]);
    fs::set_permissions(&build, fs::Permissions::from_mode(0o755)).expect("restore permissions");
    assert_eq!(output.status.code(), Some(3), "circuit breaker exit status");
    let receipt: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON receipt");
    assert_eq!(receipt["circuit_breaker_tripped"], true);
    assert_eq!(receipt["mutation_performed"], true);
    assert_eq!(receipt["repositories"][0]["summary"]["reclaimed"], 0);
    assert!(
        untouched.exists(),
        "the batch stopped before the next worktree"
    );
}

/// Commit on the current branch with committer and author dates `seconds` ago.
fn commit_aged(checkout: &Path, message: &str, seconds: u64) {
    let at = SystemTime::now() - std::time::Duration::from_secs(seconds);
    let date = format!(
        "@{} +0000",
        at.duration_since(UNIX_EPOCH).expect("epoch").as_secs()
    );
    let output = Command::new(GIT)
        .arg("-C")
        .arg(checkout)
        .args([
            "-c",
            "user.name=Glaeda Test",
            "-c",
            "user.email=glaeda-test@example.invalid",
            "commit",
            "--allow-empty",
            "-m",
            message,
        ])
        .env_clear()
        .env("HOME", checkout)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_COMMITTER_DATE", &date)
        .env("GIT_AUTHOR_DATE", &date)
        .env("LC_ALL", "C")
        .output()
        .expect("run fixture Git");
    assert_child_success("aged commit", &output);
}

/// Rewrite every entry of one branch's reflog to `seconds` ago.
fn age_branch_reflog(repository: &Path, branch: &str, seconds: u64) {
    let at = SystemTime::now() - std::time::Duration::from_secs(seconds);
    let epoch = at
        .duration_since(UNIX_EPOCH)
        .expect("epoch")
        .as_secs()
        .to_string();
    let reflog = repository.join(".git/logs/refs/heads").join(branch);
    let text = fs::read_to_string(&reflog).expect("read branch reflog");
    let aged: String = text
        .lines()
        .map(|line| {
            let (header, message) = line.split_once('\t').unwrap_or((line, ""));
            let mut fields: Vec<&str> = header.split(' ').collect();
            let at = fields.len() - 2;
            fields[at] = &epoch;
            format!("{}\t{message}\n", fields.join(" "))
        })
        .collect();
    fs::write(&reflog, aged).expect("backdate branch reflog");
}

/// Make a branch off main holding one aged commit that adds `file`; land the same file on main
/// when `land` is set. Returns to main.
fn branch_with_work(fixture: &Fixture, name: &str, file: &str, land: bool, seconds: u64) {
    let main = &fixture.main;
    git(main, &["checkout", "-q", "-b", name]);
    fs::write(main.join(file), format!("{name}\n")).expect("write branch file");
    git(main, &["add", file]);
    commit_aged(main, name, seconds);
    git(main, &["checkout", "-q", "main"]);
    if land {
        fs::write(main.join(file), format!("{name}\n")).expect("write squashed file");
        git(main, &["add", file]);
        commit(main, &format!("{name} (squashed)"));
    }
    age_branch_reflog(main, name, seconds);
}

fn branch_entry(report: &serde_json::Value, name: &str) -> serde_json::Value {
    report["branches"]["branches"]
        .as_array()
        .expect("branch list")
        .iter()
        .find(|branch| branch["name"] == name)
        .cloned()
        .unwrap_or_else(|| panic!("branch {name} missing from {report:#}"))
}

/// `--branches` deletes only finished, idle, unprotected branches no worktree has checked out.
#[test]
fn branches_mode_deletes_only_finished_idle_branches() {
    let fixture = Fixture::new();
    let hours = 60 * 60;
    branch_with_work(&fixture, "landed", "landed.txt", true, 2 * hours);
    branch_with_work(&fixture, "unfinished", "wip.txt", false, 2 * hours);
    branch_with_work(&fixture, "recent", "recent.txt", true, 60);
    branch_with_work(&fixture, "kept", "kept.txt", true, 2 * hours);
    // Landed and idle, but a worktree is mid-rebase of it (HEAD detached) or bisecting it.
    branch_with_work(&fixture, "rebasing", "rebasing.txt", true, 2 * hours);
    branch_with_work(&fixture, "bisecting", "bisecting.txt", true, 2 * hours);
    let rebase_worktree = fixture.root.join("rebase-wt");
    git(
        &fixture.main,
        &[
            "worktree",
            "add",
            "--detach",
            rebase_worktree.to_str().expect("UTF-8 fixture path"),
        ],
    );
    let rebase_state = fixture.main.join(".git/worktrees/rebase-wt/rebase-merge");
    fs::create_dir_all(&rebase_state).expect("create rebase state");
    fs::write(rebase_state.join("head-name"), "refs/heads/rebasing\n").expect("write head-name");
    fs::write(fixture.main.join(".git/BISECT_START"), "bisecting\n").expect("write BISECT_START");
    git(
        &fixture.main,
        &["config", "--add", "glaeda.keepBranch", "kept"],
    );
    git(
        &fixture.main,
        &[
            "config",
            "branch.landed.description",
            "gone with the branch",
        ],
    );
    // Checked out in a worktree: kept even though its work has landed.
    let checked = fixture.add("checked");
    fs::write(checked.join("checked.txt"), "checked\n").expect("write checked file");
    git(&checked, &["add", "checked.txt"]);
    commit_aged(&checked, "checked", 2 * hours);
    fs::write(fixture.main.join("checked.txt"), "checked\n").expect("land checked file");
    git(&fixture.main, &["add", "checked.txt"]);
    commit(&fixture.main, "checked (squashed)");
    age_branch_reflog(&fixture.main, "checked", 2 * hours);
    git(
        &fixture.main,
        &["update-ref", "refs/remotes/origin/main", "main"],
    );
    git(
        &fixture.main,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );

    let plan = report(&fixture.run(&fixture.main, &["--branches", "--no-github", "--all"]));
    assert_eq!(plan["branches"]["result"], "listed");
    assert_eq!(plan["branches"]["eligible"], 1, "{plan:#}");
    let landed = branch_entry(&plan, "landed");
    assert_eq!(landed["decision"], "eligible");
    assert_eq!(landed["evidence"], "default_branch");
    for (name, reason) in [
        ("unfinished", "in_progress"),
        ("recent", "recently_active"),
        ("kept", "protected"),
        // Checked out in the main worktree, which is checked before the protected names.
        ("main", "checked_out"),
        ("checked", "checked_out"),
        ("rebasing", "checked_out"),
        ("bisecting", "checked_out"),
    ] {
        assert_eq!(branch_entry(&plan, name)["reason"], reason, "{name}");
    }
    assert!(
        ref_exists(&fixture.main, "refs/heads/landed"),
        "a plan deletes nothing"
    );

    let applied = report(&fixture.run(&fixture.main, &["--branches", "--no-github", "--apply"]));
    assert_eq!(applied["branches"]["deleted"], 1, "{applied:#}");
    assert_eq!(branch_entry(&applied, "landed")["outcome"], "deleted");
    assert!(!ref_exists(&fixture.main, "refs/heads/landed"));
    let description = Command::new(GIT)
        .arg("-C")
        .arg(&fixture.main)
        .args(["config", "branch.landed.description"])
        .output()
        .expect("run git config");
    assert!(
        !description.status.success(),
        "the branch's config section goes too"
    );
    for name in [
        "unfinished",
        "recent",
        "kept",
        "main",
        "checked",
        "rebasing",
        "bisecting",
    ] {
        assert!(
            ref_exists(&fixture.main, &format!("refs/heads/{name}")),
            "{name} kept"
        );
    }
}

/// Without `--branches` the report has no branch section and no branch is touched.
#[test]
fn branches_are_untouched_without_the_flag() {
    let fixture = Fixture::new();
    branch_with_work(&fixture, "landed", "landed.txt", true, 2 * 60 * 60);
    git(
        &fixture.main,
        &["update-ref", "refs/remotes/origin/main", "main"],
    );
    git(
        &fixture.main,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    let applied = report(&fixture.run(&fixture.main, &["--apply"]));
    assert!(applied.get("branches").is_none());
    assert!(ref_exists(&fixture.main, "refs/heads/landed"));
}

/// A squash-merged worktree whose files main changed again looks unfinished to Git alone; a merged
/// pull request containing its tip makes it finished, and only at exactly that tip.
#[test]
fn a_merged_pull_request_finishes_a_worktree_git_alone_cannot() {
    let fixture = Fixture::new();
    git(
        &fixture.main,
        &["remote", "add", "origin", "git@github.com:acme/widgets.git"],
    );
    for name in ["squashed", "moved-on", "unmerged"] {
        let checkout = fixture.add(name);
        fs::write(checkout.join("tracked.txt"), format!("{name}\n")).expect("write change");
        git(&checkout, &["add", "tracked.txt"]);
        commit(&checkout, "change tracked.txt");
    }
    // Main squash-merges them, then edits the same file again, so no path comparison matches.
    fs::write(fixture.main.join("tracked.txt"), "later\n").expect("write later main");
    git(&fixture.main, &["add", "tracked.txt"]);
    commit(&fixture.main, "squash, then more");
    git(
        &fixture.main,
        &["update-ref", "refs/remotes/origin/main", "main"],
    );
    git(
        &fixture.main,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    let tip = |name: &str| {
        String::from_utf8(git_output(&fixture.root.join(name), &["rev-parse", "HEAD"]).stdout)
            .expect("UTF-8 commit")
            .trim()
            .to_owned()
    };
    // A fresh branch whose name and start point match an old merged pull request: no work yet.
    fixture.add("reused");
    let reused_tip = tip("reused");
    let squashed_tip = tip("squashed");
    let moved_on_merged = tip("moved-on");
    // moved-on kept working after its pull request merged: its new tip is not in the PR head.
    let moved_on = fixture.root.join("moved-on");
    fs::write(moved_on.join("after.txt"), "after\n").expect("write follow-up");
    git(&moved_on, &["add", "after.txt"]);
    commit(&moved_on, "follow-up after merge");
    for name in ["squashed", "moved-on", "unmerged", "reused"] {
        fixture.age_by(name, 2 * 60 * 60);
    }

    // A fake gh: squashed and moved-on have merged pull requests into main, unmerged has none.
    let gh = fixture.root.join("gh");
    fs::write(
        &gh,
        format!(
            r#"#!/bin/sh
[ "$1" = api ] && [ "$2" = graphql ] || exit 1
fields=""
for arg in "$@"; do
  case "$arg" in
    b[0-9]*=*)
      key="${{arg%%=*}}"; name="${{arg#*=}}"
      case "$name" in
        squashed) nodes='{{"number":1,"headRefOid":"{squashed_tip}","baseRefName":"main"}}' ;;
        moved-on) nodes='{{"number":2,"headRefOid":"{moved_on_merged}","baseRefName":"main"}}' ;;
        reused) nodes='{{"number":3,"headRefOid":"{reused_tip}","baseRefName":"main"}}' ;;
        *) nodes='' ;;
      esac
      fields="$fields,\"$key\":{{\"nodes\":[$nodes]}}" ;;
  esac
done
printf '{{"data":{{"repository":{{"defaultBranchRef":{{"name":"main"}}%s}}}}}}' "$fields"
"#
        ),
    )
    .expect("write fake gh");
    let output = Command::new("/bin/chmod")
        .args(["+x", gh.to_str().expect("UTF-8 fixture path")])
        .output()
        .expect("run chmod");
    assert_child_success("make fake gh executable", &output);

    let with_github = report(&fixture.run(
        &fixture.main,
        &["--gh", gh.to_str().expect("UTF-8 fixture path")],
    ));
    assert_eq!(
        entry_by_name(&fixture, &with_github, "squashed")["facts"]["work_state"],
        "finished"
    );
    assert_eq!(
        entry_by_name(&fixture, &with_github, "squashed")["decision"]["decision"],
        "eligible"
    );
    for name in ["moved-on", "unmerged", "reused"] {
        let entry = entry_by_name(&fixture, &with_github, name);
        assert_eq!(entry["facts"]["work_state"], "in_progress", "{name}");
        assert_eq!(
            entry["decision"]["vetoes"],
            serde_json::json!(["recently_active"]),
            "{name}"
        );
    }

    let without = report(&fixture.run(&fixture.main, &["--no-github"]));
    assert_eq!(
        entry_by_name(&fixture, &without, "squashed")["facts"]["work_state"],
        "in_progress"
    );
}
