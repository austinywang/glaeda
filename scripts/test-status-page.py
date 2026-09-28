#!/usr/bin/env python3
from __future__ import annotations

import datetime as dt
import importlib.util
from pathlib import Path
import unittest

MODULE_PATH = Path(__file__).with_name("status_page.py")
SPEC = importlib.util.spec_from_file_location("status_page", MODULE_PATH)
assert SPEC and SPEC.loader
s = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(s)

AS_OF = dt.date(2026, 9, 23)
LIVING = (
    "## Current status \u2014 23 September 2026\n\n"
    "Merged: #1 and [the peer seam](https://example.invalid/x). Next: #2.\n\n"
    "Second paragraph is not included.\n\n"
    "<details>\n<summary>Original scope</summary>\n\n## Goal\n\nOld text.\n</details>\n"
)


def issue(number, title, body="", labels=(), updated="2026-09-20T00:00:00Z"):
    return {
        "number": number,
        "title": title,
        "body": body,
        "updatedAt": updated,
        "labels": {"nodes": [{"name": name} for name in labels]},
    }


def pr(number, title, merged=None, state="SUCCESS", draft=False):
    return {
        "number": number,
        "title": title,
        "isDraft": draft,
        "mergedAt": merged,
        "updatedAt": "2026-09-22T00:00:00Z",
        "commits": {"nodes": [{"commit": {"statusCheckRollup": {"state": state}}}]},
    }


def data(reverse=False):
    issues = [
        issue(10, "RFC: shared cache", LIVING),
        issue(11, "Fix a small bug", "Plain body."),
        issue(12, "Operate the fleet", LIVING),
        issue(13, "M5: prove recovery", "Parent: #10", labels=("current-critical",)),
    ]
    merged = [
        pr(20, "Land A", merged="2026-09-22T10:00:00Z"),
        pr(21, "Land B", merged="2026-09-22T10:00:00Z"),
        pr(19, "Land C", merged="2026-09-21T10:00:00Z"),
    ]
    opened = [pr(30, "Open A", state="FAILURE"), pr(31, "Open B", draft=True)]
    closed = [
        {"number": 5, "title": "RFC: done thing", "closedAt": "2026-09-10T00:00:00Z", "body": "", "labels": {"nodes": []}},
        {"number": 6, "title": "Small fix", "closedAt": "2026-09-11T00:00:00Z", "body": "", "labels": {"nodes": []}},
    ]
    if reverse:
        issues, merged, opened, closed = issues[::-1], merged[::-1], opened[::-1], closed[::-1]
    return {
        "repo": "owner/repo",
        "openIssues": issues,
        "openIssuesTruncated": False,
        "milestones": [],
        "merged": {"issueCount": len(merged), "nodes": merged},
        "open": {"issueCount": len(opened), "nodes": opened},
        "closed": closed,
    }


class StatusPageTests(unittest.TestCase):
    def test_current_status_takes_living_heading_and_first_paragraph(self):
        text = s.current_status(LIVING)
        self.assertTrue(text.startswith("Current status, 23 September 2026: Merged: #1 and the peer seam."))
        self.assertNotIn("Second paragraph", text)
        self.assertNotIn("Old text", text)
        self.assertNotIn("\u2014", text)

    def test_current_status_falls_back_to_first_paragraph(self):
        self.assertEqual(s.current_status("Purpose: keep one rule.\n\nMore."), "Purpose: keep one rule.")
        self.assertEqual(s.current_status(""), "(no status paragraph)")

    def test_clean_text_is_bounded_and_path_free(self):
        text = s.clean_text("see /Users/someone/Projects/x and ~/secret/file " + "word " * 200, 80)
        self.assertLessEqual(len(text), 80)
        self.assertNotIn("/Users", text)
        self.assertNotIn("~/", text)
        self.assertTrue(text.endswith("..."))

    def test_epic_classification(self):
        self.assertTrue(s.is_epic(issue(1, "RFC: x")))
        self.assertTrue(s.is_epic(issue(1, "Glaeda north star: x")))
        self.assertTrue(s.is_epic(issue(1, "x", labels=("current-critical",))))
        self.assertTrue(s.is_epic(issue(1, "x", LIVING)))
        self.assertFalse(s.is_epic(issue(1, "Fix a small bug", "Plain body.")))

    def test_render_is_deterministic_and_sorted(self):
        page = s.render(data(), AS_OF)
        self.assertEqual(page, s.render(data(reverse=True), AS_OF))
        self.assertIn("## Open RFCs and epics (3 of 4 open issues)", page)
        self.assertLess(page.index("**#10**"), page.index("**#12**"))
        self.assertNotIn("**#11**", page)
        self.assertLess(page.index("#21 Land B"), page.index("#20 Land A"))
        self.assertIn("| #30 | failure | no |", page)
        self.assertIn("#5 RFC: done thing", page)
        self.assertNotIn("#6 Small fix", page)
        self.assertNotIn("\u2014", page)

    def test_render_bounds_sections(self):
        many = data()
        many["merged"] = {
            "issueCount": 120,
            "nodes": [pr(n, f"PR {n}", merged="2026-09-20T00:00:00Z") for n in range(100)],
        }
        page = s.render(many, AS_OF)
        self.assertEqual(page.count("2026-09-20 #"), s.MAX_MERGED)
        self.assertIn(f"... {120 - s.MAX_MERGED} more", page)


if __name__ == "__main__":
    unittest.main()
