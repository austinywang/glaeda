#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use glaeda::resident_repo_query::MAX_RESPONSE_BYTES;

const BINARY: &str = env!("CARGO_BIN_EXE_glaeda-repo-query");
const GIT: &str = "/usr/bin/git";
static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

struct Fixture {
    root: PathBuf,
    checkout: PathBuf,
    base: String,
    head: String,
    tree: String,
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
            "glaeda-repo-query-cli-{}-{nonce}-{sequence}",
            std::process::id()
        ));
        let checkout = root.join("checkout");
        fs::create_dir_all(&checkout).expect("create checkout");
        git(&checkout, &["init", "-b", "main"]);
        git(
            &checkout,
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/teamleaderleo/glaeda.git",
            ],
        );
        fs::write(checkout.join("one.txt"), "one\n").expect("write base");
        git(&checkout, &["add", "one.txt"]);
        commit(&checkout, "base");
        let base = git_stdout(&checkout, &["rev-parse", "HEAD"]);
        fs::write(checkout.join("one.txt"), "one\ntwo\n").expect("change file");
        fs::write(checkout.join("two.txt"), "three\n").expect("add file");
        git(&checkout, &["add", "one.txt", "two.txt"]);
        commit(&checkout, "head");
        let head = git_stdout(&checkout, &["rev-parse", "HEAD"]);
        let tree = git_stdout(&checkout, &["rev-parse", "HEAD^{tree}"]);
        Self {
            root,
            checkout,
            base,
            head,
            tree,
        }
    }

    fn query(&self, extra: &[&str]) -> Output {
        Command::new(BINARY)
            .args([
                "--checkout",
                self.checkout.to_str().expect("UTF-8 checkout"),
                "--project",
                "github.com/teamleaderleo/glaeda",
                "--base",
                &self.base,
                "--head",
                &self.head,
                "--tree",
                &self.tree,
            ])
            .args(extra)
            .output()
            .expect("run repo query")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let temporary_root =
            fs::canonicalize(std::env::temp_dir()).expect("canonicalize test temporary directory");
        if self.root.parent() == Some(temporary_root.as_path()) {
            fs::remove_dir_all(&self.root).expect("remove fixture")
        }
    }
}

fn git(checkout: &Path, arguments: &[&str]) {
    let output = Command::new(GIT)
        .arg("-C")
        .arg(checkout)
        .args(arguments)
        .env_clear()
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("LC_ALL", "C")
        .output()
        .expect("run fixture Git");
    assert!(
        output.status.success(),
        "fixture Git failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_stdout(checkout: &Path, arguments: &[&str]) -> String {
    let output = Command::new(GIT)
        .arg("-C")
        .arg(checkout)
        .args(arguments)
        .env_clear()
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("LC_ALL", "C")
        .output()
        .expect("read fixture Git");
    assert!(output.status.success());
    String::from_utf8(output.stdout)
        .expect("UTF-8 Git output")
        .trim()
        .to_owned()
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
            "-m",
            message,
        ],
    );
}

#[test]
fn exact_bundle_is_compact_path_private_and_complete() {
    let fixture = Fixture::new();
    let output = fixture.query(&[]);
    assert!(
        output.status.success(),
        "query failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON report");
    assert_eq!(report["document_type"], "glaeda-resident-repo-query");
    assert_eq!(report["profile_id"], "repo-query/v1");
    assert_eq!(report["authority"], "observation_only");
    assert_eq!(report["requested_base"], fixture.base);
    assert_eq!(report["head"], fixture.head);
    assert_eq!(report["base_is_ancestor"], true);
    assert_eq!(report["commits_since_merge_base"], 1);
    assert_eq!(report["diff_summary"]["files_changed"], 2);
    assert_eq!(report["diff_summary"]["insertions"], 2);
    assert_eq!(report["diff_summary"]["deletions"], 0);
    assert_eq!(report["patch"]["included"], true);
    assert_eq!(report["object_format"], "sha1");
    assert_eq!(report["metrics"]["git_processes"], 13);
    let encoded = String::from_utf8(output.stdout).expect("UTF-8 report");
    assert!(!encoded.contains(fixture.root.to_str().expect("UTF-8 root")));
    assert!(!encoded.contains("/usr/bin/git"));
}

#[test]
fn patch_limit_omits_content_without_losing_digest_or_counts() {
    let fixture = Fixture::new();
    let output = fixture.query(&["--max-patch-bytes", "0"]);
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON report");
    assert_eq!(report["patch"]["included"], false);
    assert!(report["patch"].get("text").is_none());
    assert!(report["patch"]["bytes"].as_u64().expect("bytes") > 0);
    assert!(
        report["patch"]["sha256"]
            .as_str()
            .expect("digest")
            .starts_with("sha256:")
    );
}

#[test]
fn one_bundle_answers_tree_blob_history_and_object_questions() {
    let fixture = Fixture::new();
    let output = fixture.query(&[
        "--grep-literal",
        "two",
        "--grep-path",
        "one.txt",
        "--blob",
        "one.txt",
        "--history",
        "one.txt",
        "--max-history-commits",
        "1",
        "--object",
        &fixture.head,
        "--object",
        "ffffffffffffffffffffffffffffffffffffffff",
    ]);
    assert!(
        output.status.success(),
        "query failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON report");
    assert_eq!(report["grep"]["status"], "complete");
    assert_eq!(report["grep"]["matches"][0]["path"], "one.txt");
    assert_eq!(report["grep"]["matches"][0]["line"], 2);
    assert_eq!(report["blobs"][0]["status"], "complete");
    assert_eq!(report["blobs"][0]["text"], "one\ntwo\n");
    assert_eq!(report["path_history"][0]["status"], "truncated");
    assert_eq!(
        report["path_history"][0]["commits"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(report["objects"][0]["exists"], true);
    assert_eq!(report["objects"][0]["object_type"], "commit");
    assert_eq!(report["objects"][1]["status"], "complete");
    assert_eq!(report["objects"][1]["exists"], false);
}

#[test]
fn auxiliary_limits_and_unsafe_paths_are_explicit() {
    let fixture = Fixture::new();
    let output = fixture.query(&["--blob", "one.txt", "--max-blob-bytes", "3"]);
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON report");
    assert_eq!(report["blobs"][0]["status"], "truncated");
    assert_eq!(report["blobs"][0]["reason"], "blob_byte_limit");
    assert!(report["blobs"][0].get("text").is_none());

    let refused = fixture.query(&["--history", "../outside"]);
    assert_eq!(refused.status.code(), Some(2));
    let error = String::from_utf8(refused.stderr).expect("UTF-8 error");
    assert!(error.contains("unsafe_input"));
    assert!(!error.contains("../outside"));
}

#[test]
fn aggregate_response_limit_drops_content_with_provenance() {
    let mut fixture = Fixture::new();
    let content = "x".repeat(16_000);
    let paths = (0..8)
        .map(|index| format!("large-{index}.txt"))
        .collect::<Vec<_>>();
    for path in &paths {
        fs::write(fixture.checkout.join(path), &content).expect("write large blob");
    }
    let refs = paths.iter().map(String::as_str).collect::<Vec<_>>();
    let mut add = vec!["add"];
    add.extend(refs);
    git(&fixture.checkout, &add);
    commit(&fixture.checkout, "large response candidate");
    fixture.head = git_stdout(&fixture.checkout, &["rev-parse", "HEAD"]);
    fixture.tree = git_stdout(&fixture.checkout, &["rev-parse", "HEAD^{tree}"]);

    let mut arguments = Vec::new();
    for path in &paths {
        arguments.push("--blob");
        arguments.push(path);
    }
    let output = fixture.query(&arguments);
    assert!(output.status.success());
    assert!(output.stdout.len() <= MAX_RESPONSE_BYTES + 1);
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON report");
    assert!(report["blobs"].as_array().unwrap().iter().any(|blob| {
        blob["status"] == "truncated" && blob["reason"] == "aggregate_response_limit"
    }));
}

#[test]
fn project_mismatch_fails_without_echoing_checkout() {
    let fixture = Fixture::new();
    let output = Command::new(BINARY)
        .args([
            "--checkout",
            fixture.checkout.to_str().expect("UTF-8 checkout"),
            "--project",
            "github.com/teamleaderleo/cultist",
            "--base",
            &fixture.base,
            "--head",
            &fixture.head,
            "--tree",
            &fixture.tree,
        ])
        .output()
        .expect("run mismatch");
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let error = String::from_utf8(output.stderr).expect("UTF-8 error");
    assert!(error.contains("repository_mismatch"));
    assert!(!error.contains(fixture.root.to_str().expect("UTF-8 root")));
}

#[test]
fn commit_tree_mismatch_is_refused_before_evidence() {
    let fixture = Fixture::new();
    let base_tree = git_stdout(
        &fixture.checkout,
        &["rev-parse", &format!("{}^{{tree}}", fixture.base)],
    );
    let output = Command::new(BINARY)
        .args([
            "--checkout",
            fixture.checkout.to_str().expect("UTF-8 checkout"),
            "--project",
            "github.com/teamleaderleo/glaeda",
            "--base",
            &fixture.base,
            "--head",
            &fixture.head,
            "--tree",
            &base_tree,
        ])
        .output()
        .expect("run mismatch");
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let error = String::from_utf8(output.stderr).expect("UTF-8 error");
    assert!(error.contains("source_mismatch"));
}

#[test]
fn cli_has_no_arbitrary_git_program_or_query_argv_surface() {
    let output = Command::new(BINARY)
        .arg("--help")
        .output()
        .expect("read help");
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).expect("UTF-8 help");
    assert!(!help.contains("git-program"));
    assert!(!help.contains("command"));
    assert!(!help.contains("argument"));
}
