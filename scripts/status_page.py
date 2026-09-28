#!/usr/bin/env python3
"""Generate docs/STATUS.md from live GitHub state.

The page is a bounded, path-free, deterministic snapshot: open RFC/epic issues with their
current-status paragraph, recently merged PRs, open PRs with CI state, and landed milestones.
It reads GitHub through `gh api graphql` and performs no writes other than the output file.

Usage:
    scripts/status_page.py [--repo OWNER/NAME] [--as-of YYYY-MM-DD] [--output PATH | --stdout]
"""
from __future__ import annotations

import argparse
import datetime as dt
import json
import re
import subprocess
import sys
from pathlib import Path

DEFAULT_REPO = "teamleaderleo/glaeda"
DEFAULT_OUTPUT = Path(__file__).resolve().parent.parent / "docs" / "STATUS.md"
GH = "gh"

MERGED_WINDOW_DAYS = 14
LANDED_WINDOW_DAYS = 30
MAX_EPICS = 25
MAX_MERGED = 40
MAX_OPEN_PRS = 30
MAX_LANDED = 20
MAX_ISSUE_PAGES = 5
STATUS_CHARS = 360
TITLE_CHARS = 100

EPIC_TITLE = re.compile(r"^(?:Glaeda |SmolRunner )?(RFC|Programme|Program|Epic|North star|Product direction)\b", re.I)
EPIC_LABELS = {"epic", "rfc", "current-critical"}
LIVING_STATUS_MARKER = re.compile(r"<summary>\s*Original", re.I)

PR_FIELDS = """
  number title url isDraft mergedAt updatedAt
  commits(last: 1) { nodes { commit { statusCheckRollup { state } } } }
"""

FIRST_QUERY = """
query($owner: String!, $name: String!, $merged: String!, $openPrs: String!, $closed: String!) {
  repository(owner: $owner, name: $name) {
    issues(states: OPEN, first: 100, orderBy: {field: CREATED_AT, direction: ASC}) {
      pageInfo { hasNextPage endCursor }
      nodes { number title updatedAt body labels(first: 20) { nodes { name } } }
    }
    milestones(first: 50, states: [CLOSED], orderBy: {field: NUMBER, direction: ASC}) {
      nodes { number title closedAt }
    }
  }
  merged: search(type: ISSUE, query: $merged, first: 100) {
    issueCount
    nodes { ... on PullRequest { %s } }
  }
  open: search(type: ISSUE, query: $openPrs, first: 100) {
    issueCount
    nodes { ... on PullRequest { %s } }
  }
  closed: search(type: ISSUE, query: $closed, first: 100) {
    nodes { ... on Issue { number title closedAt labels(first: 20) { nodes { name } } body } }
  }
}
""" % (PR_FIELDS, PR_FIELDS)

ISSUE_PAGE_QUERY = """
query($owner: String!, $name: String!, $after: String!) {
  repository(owner: $owner, name: $name) {
    issues(states: OPEN, first: 100, after: $after, orderBy: {field: CREATED_AT, direction: ASC}) {
      pageInfo { hasNextPage endCursor }
      nodes { number title updatedAt body labels(first: 20) { nodes { name } } }
    }
  }
}
"""


def gh_graphql(query: str, variables: dict[str, str]) -> dict:
    argv = [GH, "api", "graphql", "-f", f"query={query}"]
    for key, value in sorted(variables.items()):
        argv += ["-f", f"{key}={value}"]
    done = subprocess.run(argv, check=False, capture_output=True, text=True, timeout=120)
    if done.returncode != 0:
        raise SystemExit(f"gh api graphql failed (exit {done.returncode}): {done.stderr.strip()[:300]}")
    payload = json.loads(done.stdout)
    if payload.get("errors"):
        raise SystemExit(f"gh api graphql errors: {json.dumps(payload['errors'])[:300]}")
    return payload["data"]


def fetch(repo: str, as_of: dt.date) -> dict:
    owner, name = repo.split("/", 1)
    merged_since = (as_of - dt.timedelta(days=MERGED_WINDOW_DAYS)).isoformat()
    closed_since = (as_of - dt.timedelta(days=LANDED_WINDOW_DAYS)).isoformat()
    data = gh_graphql(
        FIRST_QUERY,
        {
            "owner": owner,
            "name": name,
            "merged": f"repo:{repo} is:pr is:merged merged:>={merged_since}",
            "openPrs": f"repo:{repo} is:pr is:open",
            "closed": f"repo:{repo} is:issue is:closed closed:>={closed_since}",
        },
    )
    repository = data["repository"]
    issues = list(repository["issues"]["nodes"])
    page = repository["issues"]["pageInfo"]
    pages = 1
    while page["hasNextPage"] and pages < MAX_ISSUE_PAGES:
        more = gh_graphql(ISSUE_PAGE_QUERY, {"owner": owner, "name": name, "after": page["endCursor"]})
        issues += more["repository"]["issues"]["nodes"]
        page = more["repository"]["issues"]["pageInfo"]
        pages += 1
    return {
        "repo": repo,
        "openIssues": issues,
        "openIssuesTruncated": bool(page["hasNextPage"]),
        "milestones": repository["milestones"]["nodes"],
        "merged": data["merged"],
        "open": data["open"],
        "closed": data["closed"]["nodes"],
    }


# ---------------------------------------------------------------------------------------------
# Pure rendering helpers (tested without network).

_LINK = re.compile(r"\[([^\]]+)\]\([^)]+\)")
_LOCAL_PATH = re.compile(r"(?<![\w.])(?:~|/Users|/home|/private|/var/folders|/tmp)(?:/[^\s`)\]]*)+")
_HTML = re.compile(r"<[^>]+>")


def clean_text(text: str, limit: int) -> str:
    """Collapse markdown to one bounded, path-free line."""
    text = _LINK.sub(r"\1", text)
    text = _HTML.sub("", text)
    text = _LOCAL_PATH.sub("<path>", text)
    text = text.replace("\u2014", ",").replace("\u2013", "-")
    text = re.sub(r"\s+,", ",", text)
    text = text.replace("|", "/")
    text = re.sub(r"\s+", " ", text).strip()
    if len(text) > limit:
        cut = text[: limit - 3].rsplit(" ", 1)[0].rstrip(",;:")
        text = cut + "..."
    return text


def labels_of(item: dict) -> set[str]:
    return {node["name"].lower() for node in (item.get("labels") or {}).get("nodes", [])}


def is_epic(issue: dict) -> bool:
    return bool(
        EPIC_TITLE.search(issue.get("title", ""))
        or labels_of(issue) & EPIC_LABELS
        or LIVING_STATUS_MARKER.search(issue.get("body") or "")
    )


def current_status(body: str) -> str:
    """Return the living-status heading plus its first paragraph.

    Issue bodies that follow the living-status shape start with a dated `##` section and fold
    the original specification into `<details>`. Otherwise the first paragraph is used.
    """
    body = (body or "").replace("\r\n", "\n")
    head = re.split(r"<details", body, maxsplit=1, flags=re.I)[0]
    if not head.strip():
        head = body
    heading = ""
    paragraph: list[str] = []
    for line in head.split("\n"):
        stripped = line.strip()
        if stripped.startswith("#"):
            if paragraph:
                break
            if not heading:
                heading = stripped.lstrip("#").strip()
            continue
        if not stripped:
            if paragraph:
                break
            continue
        paragraph.append(stripped)
    text = " ".join(paragraph)
    if heading:
        text = f"{heading}: {text}" if text else heading
    return clean_text(text, STATUS_CHARS) or "(no status paragraph)"


def ci_state(pr: dict) -> str:
    nodes = (pr.get("commits") or {}).get("nodes") or []
    rollup = nodes[0]["commit"].get("statusCheckRollup") if nodes else None
    return (rollup or {}).get("state", "NONE").lower()


def render(data: dict, as_of: dt.date) -> str:
    repo = data["repo"]
    lines = [
        "# Glaeda status",
        "",
        f"Generated by `scripts/status_page.py` for `{repo}` as of {as_of.isoformat()}.",
        "Do not hand-edit. Regenerate with `scripts/status_page.py`. Issue bodies own current status;",
        "this page only collects it.",
        "",
    ]

    issues = [i for i in data["openIssues"] if i]
    epics = sorted((i for i in issues if is_epic(i)), key=lambda i: i["number"])
    lines += [f"## Open RFCs and epics ({len(epics)} of {len(issues)} open issues)", ""]
    if data.get("openIssuesTruncated"):
        lines += ["Open-issue scan truncated at the page limit.", ""]
    for issue in epics[:MAX_EPICS]:
        title = clean_text(issue["title"], TITLE_CHARS)
        lines.append(f"- **#{issue['number']}** {title} (updated {issue['updatedAt'][:10]})")
        lines.append(f"  {current_status(issue.get('body', ''))}")
    if len(epics) > MAX_EPICS:
        lines.append(f"- ... {len(epics) - MAX_EPICS} more")
    if not epics:
        lines.append("- none")
    lines.append("")

    merged = [p for p in data["merged"]["nodes"] if p and p.get("mergedAt")]
    merged.sort(key=lambda p: (p["mergedAt"], p["number"]), reverse=True)
    lines += [f"## Merged PRs, last {MERGED_WINDOW_DAYS} days ({data['merged']['issueCount']})", ""]
    for pr in merged[:MAX_MERGED]:
        lines.append(f"- {pr['mergedAt'][:10]} #{pr['number']} {clean_text(pr['title'], TITLE_CHARS)}")
    if data["merged"]["issueCount"] > min(len(merged), MAX_MERGED):
        lines.append(f"- ... {data['merged']['issueCount'] - min(len(merged), MAX_MERGED)} more")
    if not merged:
        lines.append("- none")
    lines.append("")

    open_prs = sorted((p for p in data["open"]["nodes"] if p), key=lambda p: p["number"])
    lines += [f"## Open PRs ({data['open']['issueCount']})", ""]
    if open_prs:
        lines += ["| PR | CI | Draft | Updated | Title |", "| --- | --- | --- | --- | --- |"]
    for pr in open_prs[:MAX_OPEN_PRS]:
        draft = "yes" if pr.get("isDraft") else "no"
        lines.append(
            f"| #{pr['number']} | {ci_state(pr)} | {draft} | {pr['updatedAt'][:10]} | "
            f"{clean_text(pr['title'], TITLE_CHARS)} |"
        )
    if len(open_prs) > MAX_OPEN_PRS:
        lines.append(f"\n... {len(open_prs) - MAX_OPEN_PRS} more")
    if not open_prs:
        lines.append("- none")
    lines.append("")

    landed = [
        (m["closedAt"][:10], f"milestone {m['title']}")
        for m in data["milestones"]
        if m and m.get("closedAt")
    ]
    landed += [
        (i["closedAt"][:10], f"#{i['number']} {clean_text(i['title'], TITLE_CHARS)}")
        for i in data["closed"]
        if i and i.get("closedAt") and is_epic(i)
    ]
    landed.sort(key=lambda row: (row[0], row[1]), reverse=True)
    lines += [
        f"## Landed milestones, last {LANDED_WINDOW_DAYS} days",
        "",
        "Closed GitHub milestones plus closed RFC/epic issues.",
        "",
    ]
    for day, text in landed[:MAX_LANDED]:
        lines.append(f"- {day} {text}")
    if not landed:
        lines.append("- none")
    lines.append("")
    return "\n".join(lines)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    parser.add_argument("--repo", default=DEFAULT_REPO)
    parser.add_argument("--as-of", type=dt.date.fromisoformat, default=None)
    target = parser.add_mutually_exclusive_group()
    target.add_argument("--output", type=Path, default=DEFAULT_OUTPUT)
    target.add_argument("--stdout", action="store_true")
    args = parser.parse_args(argv)
    as_of = args.as_of or dt.datetime.now(dt.timezone.utc).date()
    page = render(fetch(args.repo, as_of), as_of)
    if args.stdout:
        sys.stdout.write(page)
    else:
        args.output.write_text(page, encoding="utf-8")
        print(f"wrote {args.output.name} ({page.count(chr(10))} lines)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
