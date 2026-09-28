#![cfg(target_os = "linux")]

//! Acceptance tests for Cargo target reclaim planning.
//!
//! These drive real directories through the real observers, so they check the decision an operator
//! would actually get on a Linux host rather than a hand-built observation.

use std::fs::{self, File};
use std::io::Write as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use glaeda::cargo_target_holder_observation::observe_cargo_target_holders;
use glaeda::cargo_target_observation::{CargoTargetState, observe_cargo_target};
use glaeda::cargo_target_reclaim::{
    CargoTargetReclaimDecision, CargoTargetReclaimError, CargoTargetReclaimPolicy,
    CargoTargetReclaimVeto, MIN_CARGO_TARGET_RECLAIM_IDLE_SECONDS, plan_cargo_target_reclaim,
};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

const ONE_DAY: i64 = 86_400;

struct Fixture {
    root: PathBuf,
    checkout: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let sequence = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "glaeda-cargo-target-reclaim-{}-{sequence}",
            std::process::id()
        ));
        let checkout = root.join("checkout");
        fs::create_dir_all(&checkout).expect("create fixture checkout");
        Self { root, checkout }
    }

    /// A target directory shaped the way Cargo leaves one.
    fn with_cargo_target(self) -> Self {
        let target = self.checkout.join("target");
        fs::create_dir_all(target.join("debug")).expect("create target/debug");
        let mut info = File::create(target.join(".rustc_info.json")).expect("create rustc info");
        info.write_all(br#"{"rustc_fingerprint":1}"#)
            .expect("write rustc info");
        let mut artifact =
            File::create(target.join("debug").join("libfixture.rlib")).expect("create artifact");
        artifact.write_all(&[0_u8; 4096]).expect("write artifact");
        self
    }

    /// A directory named `target` that Cargo did not produce.
    fn with_foreign_target(self) -> Self {
        let target = self.checkout.join("target");
        fs::create_dir_all(&target).expect("create target");
        let mut stray = File::create(target.join("notes.txt")).expect("create stray file");
        stray.write_all(b"not cargo output").expect("write stray");
        self
    }

    fn newest_entry_seconds(&self) -> i64 {
        let observation = observe_cargo_target(&self.checkout).expect("observe target");
        match observation.state() {
            CargoTargetState::Present {
                latest_modified, ..
            } => latest_modified.seconds(),
            CargoTargetState::Absent => panic!("fixture target is absent"),
        }
    }

    fn plan_at(
        &self,
        now_seconds: i64,
    ) -> Result<glaeda::cargo_target_reclaim::CargoTargetReclaimPlan, CargoTargetReclaimError> {
        let observation = observe_cargo_target(&self.checkout).expect("observe target");
        let holders = observe_cargo_target_holders(&self.checkout).expect("observe holders");
        let policy = CargoTargetReclaimPolicy::new(MIN_CARGO_TARGET_RECLAIM_IDLE_SECONDS)
            .expect("reviewed policy");
        plan_cargo_target_reclaim(&observation, &holders, policy, now_seconds)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn an_idle_cargo_produced_target_is_eligible() {
    let fixture = Fixture::new().with_cargo_target();
    let now = fixture.newest_entry_seconds() + ONE_DAY;

    let plan = fixture.plan_at(now).expect("plan");

    match plan.decision() {
        CargoTargetReclaimDecision::Eligible {
            allocated_bytes,
            entry_count,
            idle_seconds,
            ..
        } => {
            assert!(*allocated_bytes > 0, "the fixture wrote a real artifact");
            assert!(*entry_count > 0, "the fixture wrote real entries");
            assert_eq!(*idle_seconds, ONE_DAY);
        }
        CargoTargetReclaimDecision::Refused { vetoes } => {
            panic!("an idle Cargo target should be eligible, refused by {vetoes:?}")
        }
    }
}

#[test]
fn incomplete_holder_coverage_does_not_refuse() {
    // The regression this guards: `/proc` coverage is never complete on an ordinary host, so
    // requiring completeness made every target permanently ineligible. Assert both halves -- that
    // coverage really is incomplete here, and that the decision is still eligible -- so the test
    // fails if either the host stops being representative or the veto comes back.
    let fixture = Fixture::new().with_cargo_target();
    let now = fixture.newest_entry_seconds() + ONE_DAY;

    let plan = fixture.plan_at(now).expect("plan");

    let CargoTargetReclaimDecision::Eligible {
        holder_evidence, ..
    } = plan.decision()
    else {
        panic!(
            "incomplete holder coverage must not refuse: {:?}",
            plan.decision()
        );
    };
    assert!(
        holder_evidence.process_entries_incomplete() > 0,
        "this host was expected to have unreadable /proc entries, so the case is exercised"
    );
    assert!(
        !holder_evidence.universal_absence_proven(),
        "positive-only observation never proves universal absence"
    );
}

#[test]
fn a_recently_modified_target_is_refused() {
    let fixture = Fixture::new().with_cargo_target();
    let now = fixture.newest_entry_seconds() + 60;

    let plan = fixture.plan_at(now).expect("plan");

    assert!(
        plan.decision()
            .vetoes()
            .contains(&CargoTargetReclaimVeto::RecentlyModified),
        "expected RecentlyModified, got {:?}",
        plan.decision()
    );
}

#[test]
fn a_directory_cargo_did_not_produce_is_refused() {
    let fixture = Fixture::new().with_foreign_target();
    let now = fixture.newest_entry_seconds() + ONE_DAY;

    let plan = fixture.plan_at(now).expect("plan");

    assert!(
        plan.decision()
            .vetoes()
            .contains(&CargoTargetReclaimVeto::NotCargoProduced),
        "expected NotCargoProduced, got {:?}",
        plan.decision()
    );
}

#[test]
fn an_absent_target_is_refused() {
    let fixture = Fixture::new();
    let observation = observe_cargo_target(&fixture.checkout).expect("observe target");
    let holders = observe_cargo_target_holders(&fixture.checkout).expect("observe holders");
    let policy =
        CargoTargetReclaimPolicy::new(MIN_CARGO_TARGET_RECLAIM_IDLE_SECONDS).expect("policy");

    let plan = plan_cargo_target_reclaim(&observation, &holders, policy, 0).expect("plan");

    assert_eq!(
        plan.decision().vetoes(),
        &[CargoTargetReclaimVeto::TargetAbsent]
    );
}

#[test]
fn an_idle_window_below_the_reviewed_minimum_is_refused() {
    let error = CargoTargetReclaimPolicy::new(MIN_CARGO_TARGET_RECLAIM_IDLE_SECONDS - 1)
        .expect_err("a sub-minimum idle window must not build a policy");
    assert_eq!(error.code(), "idle_window_too_small");
}

#[test]
fn a_target_newer_than_the_clock_is_disagreeing_evidence() {
    let fixture = Fixture::new().with_cargo_target();
    let now = fixture.newest_entry_seconds() - 1;

    let error = fixture
        .plan_at(now)
        .expect_err("a future target must not produce a decision");

    assert_eq!(error.code(), "future_timestamp");
}

// --- Execution -------------------------------------------------------------

use glaeda::cargo_target_reclaim::{CargoTargetReclaimOutcome, reclaim_cargo_target};

fn reviewed_policy() -> CargoTargetReclaimPolicy {
    CargoTargetReclaimPolicy::new(MIN_CARGO_TARGET_RECLAIM_IDLE_SECONDS).expect("reviewed policy")
}

/// Leave behind a retiring directory the way an interrupted pass does: build it, then name it after
/// its own inode. The resume path checks that binding, so a hand-written inode number is not a
/// retiring directory and a fixture that fabricates one is testing nothing.
fn interrupted_retiring_directory(checkout: &Path) -> PathBuf {
    let staging = checkout.join("retiring-staging");
    fs::create_dir(&staging).expect("create retiring staging");
    let inode = fs::metadata(&staging).expect("stat retiring staging").ino();
    let retiring = checkout.join(format!(".glaeda-reclaiming-{inode}"));
    fs::rename(&staging, &retiring).expect("name the retiring directory after its inode");
    retiring
}

#[test]
fn an_eligible_target_is_actually_removed() {
    let fixture = Fixture::new().with_cargo_target();
    let now = fixture.newest_entry_seconds() + ONE_DAY;
    let target = fixture.checkout.join("target");
    assert!(target.exists(), "fixture precondition");

    let receipt = reclaim_cargo_target(&fixture.checkout, reviewed_policy(), now).expect("reclaim");

    match receipt.outcome() {
        CargoTargetReclaimOutcome::Reclaimed {
            entries_removed,
            released_bytes,
            ..
        } => {
            assert!(*entries_removed > 0, "the fixture had entries to remove");
            assert!(*released_bytes > 0, "the fixture allocated real blocks");
        }
        other => panic!("expected Reclaimed, got {other:?}"),
    }
    assert!(receipt.mutation_performed());
    assert!(!target.exists(), "the target must be gone");
    assert!(
        fixture.checkout.exists(),
        "the checkout itself must survive"
    );
}

#[test]
fn a_refused_target_is_left_untouched() {
    let fixture = Fixture::new().with_cargo_target();
    // Inside the idle window, so the decision must refuse.
    let now = fixture.newest_entry_seconds() + 60;
    let artifact = fixture
        .checkout
        .join("target")
        .join("debug")
        .join("libfixture.rlib");

    let receipt = reclaim_cargo_target(&fixture.checkout, reviewed_policy(), now).expect("reclaim");

    assert!(
        matches!(receipt.outcome(), CargoTargetReclaimOutcome::Refused { .. }),
        "expected Refused, got {:?}",
        receipt.outcome()
    );
    assert!(!receipt.mutation_performed(), "a refusal mutates nothing");
    assert!(artifact.exists(), "the artifact must survive a refusal");
}

#[test]
fn an_interrupted_pass_is_finished_by_the_next_one() {
    // Simulate the crash window: a retiring directory left behind after the rename but before the
    // delete finished. A later pass must clean it up without being asked.
    let fixture = Fixture::new().with_cargo_target();
    let leftover = interrupted_retiring_directory(&fixture.checkout);
    fs::create_dir_all(leftover.join("deep").join("deeper")).expect("create leftover");
    File::create(leftover.join("deep").join("stale.rlib")).expect("create stale artifact");
    let now = fixture.newest_entry_seconds() + ONE_DAY;

    let receipt = reclaim_cargo_target(&fixture.checkout, reviewed_policy(), now).expect("reclaim");

    assert!(
        !leftover.exists(),
        "an interrupted pass must be finished by the next one"
    );
    assert!(
        receipt.resumed_entries_removed() > 0,
        "the resumed entries must be reported, got {receipt:?}"
    );
}

#[test]
fn a_retiring_tree_deeper_than_any_fixed_ceiling_is_fully_removed() {
    // #1083: a depth limit is the wrong kind of bound, because a valid tree past it can never be
    // reclaimed on any later pass. The resume path never observes, so it is the one that must hold
    // at arbitrary depth -- 300 levels is past every ceiling this codebase has used.
    let fixture = Fixture::new().with_cargo_target();
    let leftover = interrupted_retiring_directory(&fixture.checkout);
    let mut deep = leftover.clone();
    for level in 0..300 {
        deep = deep.join(format!("l{level}"));
    }
    fs::create_dir_all(&deep).expect("create deep retiring tree");
    File::create(deep.join("leaf.bin")).expect("create leaf");
    let now = fixture.newest_entry_seconds() + ONE_DAY;

    let receipt = reclaim_cargo_target(&fixture.checkout, reviewed_policy(), now).expect("reclaim");

    assert!(
        !leftover.exists(),
        "a retiring tree must be removed at any depth"
    );
    assert!(receipt.resumed_entries_removed() > 300, "{receipt:?}");
}

#[test]
fn a_target_past_the_observation_bound_says_so() {
    // The delete has no depth ceiling, but planning needs an observation and the observer does.
    // The operator should learn which bound stopped them, not a generic read failure.
    let fixture = Fixture::new().with_cargo_target();
    // Read the clock while the tree is still observable; the deep tree below is not.
    let now = fixture.newest_entry_seconds() + ONE_DAY;
    let mut deep = fixture.checkout.join("target");
    for level in 0..300 {
        deep = deep.join(format!("l{level}"));
    }
    fs::create_dir_all(&deep).expect("create deep target");

    let error = reclaim_cargo_target(&fixture.checkout, reviewed_policy(), now)
        .expect_err("a target past the observation bound must not be reclaimed silently");

    assert_eq!(error.code(), "target_exceeds_observation_bound");
}

#[test]
fn a_retiring_name_this_tool_did_not_write_is_left_alone() {
    // The resume path deletes recursively with no observation behind it, so its only evidence is
    // the directory itself. Anyone who can write in the checkout can create the name; only a rename
    // this tool performed carries the matching inode. A stranger's tree must survive untouched.
    let fixture = Fixture::new().with_cargo_target();
    let planted = fixture.checkout.join(".glaeda-reclaiming-999999");
    let keepsake = planted.join("not-ours.txt");
    fs::create_dir_all(&planted).expect("create planted directory");
    File::create(&keepsake).expect("create planted file");
    let now = fixture.newest_entry_seconds() + ONE_DAY;

    let receipt = reclaim_cargo_target(&fixture.checkout, reviewed_policy(), now).expect("reclaim");

    assert!(
        keepsake.exists(),
        "a planted retiring name must not be deleted"
    );
    assert_eq!(
        receipt.resumed_entries_removed(),
        0,
        "nothing was resumed, got {receipt:?}"
    );
    // The planted directory does not stop the pass it sits next to.
    assert!(
        !fixture.checkout.join("target").exists(),
        "the eligible target must still be reclaimed, got {receipt:?}"
    );
}

#[test]
fn a_relative_checkout_is_refused() {
    let error = reclaim_cargo_target(
        std::path::Path::new("relative/checkout"),
        reviewed_policy(),
        0,
    )
    .expect_err("a relative checkout must not be reclaimed");
    assert_eq!(error.code(), "checkout_unavailable");
}
