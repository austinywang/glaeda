//! Bind every stated security invariant in `docs/THREAT_MODEL.md` to its enforcement status.
//!
//! Glaeda's product claim is that it starts from state it can prove is valid. The threat model is
//! the document a reviewer reads before enrolling a repository, so a control described there in the
//! present indicative is read as a control that exists. This test makes that readable claim
//! mechanical: every invariant bullet must declare `enforced`, `partial`, or `required`, and every
//! reference it cites must still resolve in the current tree.
//!
//! It deliberately proves only the binding, never the security property itself. The owning contract
//! tests prove the properties.

use std::fs;
use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;

const THREAT_MODEL_PATH: &str = "docs/THREAT_MODEL.md";

/// Sections whose bullets state a security control and therefore carry a status marker.
const MARKED_SECTIONS: [&str; 2] = ["## Required security invariants", "## Network policy"];

/// `enforced` — current `main` refuses the unsafe configuration at every cited point.
/// `partial` — part of the statement is refused and the rest is not; the bullet says which.
/// `required` — Glaeda requires this and does not yet enforce it; the citation names the owner.
const STATUSES: [&str; 3] = ["enforced", "partial", "required"];

const STATUS_OPEN: &str = " — **";
const STATUS_CLOSE: &str = "** (";

struct MarkedBullet<'a> {
    section: &'a str,
    statement: &'a str,
    status: &'a str,
    references: Vec<&'a str>,
}

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn section_body<'a>(document: &'a str, heading: &str) -> &'a str {
    let start = document
        .find(heading)
        .unwrap_or_else(|| panic!("{THREAT_MODEL_PATH} must keep the `{heading}` section"));
    let body = &document[start + heading.len()..];
    match body.find("\n## ") {
        Some(end) => &body[..end],
        None => body,
    }
}

/// Parse one `- <statement> — **<status>** (`<reference>`, `<reference>`)` bullet.
fn parse_bullet<'a>(section: &'a str, line: &'a str) -> MarkedBullet<'a> {
    let bullet = line
        .strip_prefix("- ")
        .unwrap_or_else(|| panic!("`{section}` bullet must start with `- `: {line}"));
    let marker = bullet.rfind(STATUS_OPEN).unwrap_or_else(|| {
        panic!(
            "every `{section}` bullet must declare an enforcement status as \
             `{STATUS_OPEN}<status>{STATUS_CLOSE}<references>)`, found: {line}"
        )
    });
    let (statement, tail) = bullet.split_at(marker);
    let tail = &tail[STATUS_OPEN.len()..];
    let close = tail
        .find(STATUS_CLOSE)
        .unwrap_or_else(|| panic!("`{section}` bullet must close its status marker: {line}"));
    let status = &tail[..close];
    let references = tail[close + STATUS_CLOSE.len()..]
        .strip_suffix(')')
        .unwrap_or_else(|| panic!("`{section}` bullet must close its reference list: {line}"));

    assert!(
        STATUSES.contains(&status),
        "`{section}` bullet declares unknown status `{status}`; \
         use one of {STATUSES:?}: {line}"
    );
    assert!(
        !statement.trim().is_empty(),
        "`{section}` bullet must state the invariant before its status: {line}"
    );

    let references = references
        .split(", ")
        .map(|reference| {
            reference
                .strip_prefix('`')
                .and_then(|value| value.strip_suffix('`'))
                .unwrap_or_else(|| {
                    panic!(
                        "`{section}` bullet must cite each reference in backticks \
                         as `path` or `path::needle`, found `{reference}`: {line}"
                    )
                })
        })
        .collect::<Vec<_>>();
    assert!(
        !references.is_empty(),
        "`{section}` bullet must cite at least one reference: {line}"
    );

    MarkedBullet {
        section,
        statement,
        status,
        references,
    }
}

fn resolve_reference(bullet: &MarkedBullet<'_>, reference: &str) {
    let (relative, needle) = match reference.split_once("::") {
        Some((relative, needle)) => (relative, Some(needle)),
        None => (reference, None),
    };
    assert!(
        !relative.starts_with('/') && !relative.contains(".."),
        "`{}` must cite a repository-relative path, found `{relative}`",
        bullet.section
    );

    let path = repository_root().join(relative);
    assert!(
        path.exists(),
        "`{}` cites `{reference}` but `{relative}` no longer exists; \
         update the invariant when its enforcement point moves",
        bullet.section
    );

    let Some(needle) = needle else {
        return;
    };
    let contents = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("`{relative}` must be readable text: {error}"));
    assert!(
        contents.contains(needle),
        "`{}` cites `{reference}` but `{relative}` no longer contains `{needle}`; \
         the invariant `{}` now names an enforcement point that does not exist",
        bullet.section,
        bullet.statement.trim()
    );
}

fn marked_bullets<'a>(document: &'a str, heading: &'a str) -> Vec<MarkedBullet<'a>> {
    section_body(document, heading)
        .lines()
        .filter(|line| line.starts_with("- "))
        .map(|line| parse_bullet(heading, line))
        .collect()
}

#[test]
fn every_stated_security_control_declares_an_enforcement_status() {
    let path = repository_root().join(THREAT_MODEL_PATH);
    let document =
        fs::read_to_string(&path).expect("the threat model must remain readable from the crate");

    let mut total = 0usize;
    for heading in MARKED_SECTIONS {
        let bullets = marked_bullets(&document, heading);
        assert!(
            !bullets.is_empty(),
            "`{heading}` must keep at least one stated control"
        );
        for bullet in &bullets {
            for reference in &bullet.references {
                resolve_reference(bullet, reference);
            }
        }
        total += bullets.len();
    }
    assert!(
        total >= MARKED_SECTIONS.len(),
        "the threat model must keep its stated controls"
    );
}

#[test]
fn unenforced_controls_name_an_owner_that_still_exists() {
    let path = repository_root().join(THREAT_MODEL_PATH);
    let document =
        fs::read_to_string(&path).expect("the threat model must remain readable from the crate");

    for heading in MARKED_SECTIONS {
        for bullet in marked_bullets(&document, heading) {
            if bullet.status == "enforced" {
                continue;
            }
            assert!(
                bullet
                    .references
                    .iter()
                    .any(|reference| reference.starts_with("docs/ROADMAP.md::")),
                "`{heading}` invariant `{}` is `{}` and must cite the roadmap milestone that owns \
                 the remaining work as `docs/ROADMAP.md::<milestone heading>`",
                bullet.statement.trim(),
                bullet.status
            );
        }
    }
}

#[test]
fn enforced_controls_do_not_point_at_the_roadmap() {
    let path = repository_root().join(THREAT_MODEL_PATH);
    let document =
        fs::read_to_string(&path).expect("the threat model must remain readable from the crate");

    for heading in MARKED_SECTIONS {
        for bullet in marked_bullets(&document, heading) {
            if bullet.status != "enforced" {
                continue;
            }
            assert!(
                bullet
                    .references
                    .iter()
                    .all(|reference| !reference.starts_with("docs/ROADMAP.md")),
                "`{heading}` invariant `{}` claims `enforced` but cites planned roadmap work; \
                 mark it `partial` or `required` instead",
                bullet.statement.trim()
            );
        }
    }
}

#[test]
fn enforcement_status_vocabulary_stays_closed() {
    let path = repository_root().join(THREAT_MODEL_PATH);
    let document =
        fs::read_to_string(&path).expect("the threat model must remain readable from the crate");

    for status in STATUSES {
        assert!(
            document.contains(&format!("`{status}` ")),
            "the threat model must keep defining the `{status}` enforcement status for readers"
        );
    }
}

/// Prove the binding check actually fails on drift rather than passing vacuously.
#[test]
fn a_stale_enforcement_point_fails_the_binding_check() {
    fn resolve(reference: &'static str) -> Result<(), Box<dyn std::any::Any + Send>> {
        let bullet = MarkedBullet {
            section: "## Required security invariants",
            statement: "self-check",
            status: "enforced",
            references: vec![reference],
        };
        let previous = panic::take_hook();
        panic::set_hook(Box::new(|_| {}));
        let outcome =
            panic::catch_unwind(AssertUnwindSafe(|| resolve_reference(&bullet, reference)));
        panic::set_hook(previous);
        outcome
    }

    resolve("src/disposable_prepared_template.rs::unsafe_policy")
        .expect("a live enforcement point must resolve");
    resolve("src/disposable_prepared_template.rs::a_symbol_that_was_renamed_away")
        .expect_err("a renamed enforcement point must fail the binding check");
    resolve("src/a_module_that_was_deleted.rs")
        .expect_err("a deleted enforcement point must fail the binding check");
}
