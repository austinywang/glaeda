"""Command-line output for the Glaeda workspace preflight."""

from __future__ import annotations

import argparse
import json
import os
import sys

from .probe import EXPECTED_REPOSITORY, PROFILE_NAMES, public_issue
from .receipt import (
    RECEIPT_TYPE,
    SCHEMA_VERSION,
    build_receipt,
    capability_fingerprint,
)


class SafeArgumentParser(argparse.ArgumentParser):
    def error(self, _message: str) -> None:
        self.print_usage(sys.stderr)
        self.exit(2, "bootstrap arguments are invalid\n")


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = SafeArgumentParser(
        prog="./scripts/bootstrap",
        description="Evaluate the Glaeda checkout and emit a capability receipt.",
    )
    parser.add_argument("--output", choices=["human", "json"], default="human")
    parser.add_argument(
        "--operation", choices=["verify", "commit", "publish"], default="verify"
    )
    return parser.parse_args(argv)


def blocked_internal_receipt(operation: str) -> dict[str, object]:
    receipt: dict[str, object] = {
        "schema_version": SCHEMA_VERSION,
        "receipt_type": RECEIPT_TYPE,
        "state": "blocked",
        "operation": operation,
        "repository_root": {
            "kind": "unresolved",
            "repository": None,
            "expected_repository": EXPECTED_REPOSITORY,
            "required_marker": "Cargo.toml",
            "required_lockfile": "Cargo.lock",
            "working_directory": "repository-root",
            "cwd_is_repository_root": False,
            "private_path_exposed": False,
        },
        "source": {
            "commit": None,
            "tree": None,
            "clean_before": None,
            "clean_after": None,
            "cleanliness_unchanged": False,
        },
        "required_tools": [],
        "optional_tools": [],
        "observed_tool_versions": {},
        "verification_backends": [],
        "formatter_capabilities": [],
        "declared_cache_paths": [],
        "resources": {
            "available_memory_mib": None,
            "available_swap_mib": None,
            "logical_cpu_count": max(1, os.cpu_count() or 1),
            "recommended_concurrency": 1,
        },
        "git_identity": {
            "evaluated": False,
            "ready": None,
            "name_configured": None,
            "email_configured": None,
        },
        "publication_readiness": {
            "evaluated": False,
            "ready": None,
            "remote_configured": None,
            "authorization": "not-requested",
        },
        "next_verification_profiles": PROFILE_NAMES,
        "deviations": [],
        "blocking_reasons": [
            public_issue(
                "internal_probe_failure",
                "the bounded workspace probe failed internally",
            )
        ],
    }
    receipt["capability_fingerprint"] = capability_fingerprint(receipt)
    return receipt


def render_human(receipt: dict[str, object]) -> str:
    heading = {
        "ready": "READY",
        "ready_with_declared_deviations": "READY WITH DECLARED DEVIATIONS",
        "blocked": "BLOCKED",
    }[str(receipt["state"])]
    root = receipt["repository_root"]
    source = receipt["source"]
    resources = receipt["resources"]
    assert isinstance(root, dict)
    assert isinstance(source, dict)
    assert isinstance(resources, dict)
    lines = [
        f"Glaeda workspace bootstrap: {heading}",
        f"Operation: {receipt['operation']}",
        f"Repository: {root['repository'] or 'unidentified'}",
        f"Source commit: {source['commit'] or 'unavailable'}",
        f"Source tree: {source['tree'] or 'unavailable'}",
        f"Checkout clean: before={source['clean_before']} after={source['clean_after']}",
        f"Recommended concurrency: {resources['recommended_concurrency']}",
        "Required tools:",
    ]
    for item in receipt["required_tools"]:
        assert isinstance(item, dict)
        value = item["version"] if item["available"] else "unavailable"
        lines.append(f"  - {item['name']}: {value}")
    lines.append("Optional tools:")
    for item in receipt["optional_tools"]:
        assert isinstance(item, dict)
        value = item["version"] if item["available"] else "unavailable"
        lines.append(f"  - {item['name']}: {value}")
    lines.append("Caches:")
    for item in receipt["declared_cache_paths"]:
        assert isinstance(item, dict)
        lines.append(
            "  - "
            f"{item['name']}: class={item['path_class']} "
            f"ownership={item['ownership']} path={item['public_path']}"
        )
    lines.append("Next verification profiles:")
    lines.extend(f"  - {name}" for name in receipt["next_verification_profiles"])
    for title, key in (
        ("Declared deviations:", "deviations"),
        ("Blocking reasons:", "blocking_reasons"),
    ):
        entries = receipt[key]
        assert isinstance(entries, list)
        if entries:
            lines.append(title)
            for item in entries:
                assert isinstance(item, dict)
                lines.append(f"  - {item['code']}: {item['message']}")
    lines.append(f"Capability fingerprint: {receipt['capability_fingerprint']}")
    return "\n".join(lines) + "\n"


def main(argv: list[str]) -> int:
    parsed = parse_args(argv)
    try:
        receipt = build_receipt(parsed.operation)
    except Exception:
        receipt = blocked_internal_receipt(parsed.operation)
    if parsed.output == "json":
        print(json.dumps(receipt, sort_keys=True, indent=2))
    else:
        sys.stdout.write(render_human(receipt))
    return 1 if receipt["state"] == "blocked" else 0
