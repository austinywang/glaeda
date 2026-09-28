from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path

from .economics import economics
from .model import (
    FleetError,
    catalog,
    find_workload,
    load_json,
    state_template,
    validate_machine,
    write_json,
)
from .report import collect_json_files, markdown_report, plan_text
from .run import run_benchmark
from .window import compare_windows, reduce_window


def parse_csv_int(value: str) -> list[int]:
    return [int(item.strip()) for item in value.split(",") if item.strip()]


def parse_csv_float(value: str) -> list[float]:
    return [float(item.strip()) for item in value.split(",") if item.strip()]


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__)
    sub = root.add_subparsers(dest="command", required=True)

    sub.add_parser("catalog")

    machine = sub.add_parser("validate-machine")
    machine.add_argument("--machine", type=Path, required=True)
    machine.add_argument("--require-complete", action="store_true")

    state = sub.add_parser("state-template")
    state.add_argument("--state", required=True)
    state.add_argument("--output", type=Path, required=True)

    plan = sub.add_parser("plan")
    plan.add_argument("--workload", required=True)
    plan.add_argument("--variant")
    plan.add_argument("--repo-root", type=Path, required=True)
    plan.add_argument("--state", required=True)
    plan.add_argument("--state-dir", type=Path, required=True)

    run = sub.add_parser("run")
    run.add_argument("--workload", required=True)
    run.add_argument("--variant")
    run.add_argument("--repo-root", type=Path, required=True)
    run.add_argument("--machine", type=Path, required=True)
    run.add_argument("--state-evidence", type=Path, required=True)
    run.add_argument("--state-dir", type=Path, required=True)
    run.add_argument("--backend-id", choices=("native-linux", "native-macos"), required=True)
    run.add_argument("--resource-policy-id", default="natural")
    run.add_argument(
        "--resource-policy-status",
        choices=("declared_only",),
        default="declared_only",
    )
    run.add_argument("--resource-policy-evidence-id")
    run.add_argument("--cpu-millis", type=int)
    run.add_argument("--memory-limit-bytes", type=int)
    storage_choices = ("internal", "external_nvme", "network", "other")
    run.add_argument(
        "--source-storage-tier", choices=storage_choices, default="internal"
    )
    run.add_argument("--source-storage-id", default="machine-internal")
    run.add_argument(
        "--state-storage-tier", choices=storage_choices, default="internal"
    )
    run.add_argument("--state-storage-id", default="machine-internal")
    run.add_argument("--output", type=Path, required=True)
    run.add_argument("--load-watts", type=float)
    run.add_argument("--timeout-seconds", type=float, default=7200.0)

    reduce_parser = sub.add_parser("reduce-window")
    reduce_parser.add_argument("--manifest", type=Path, required=True)
    reduce_parser.add_argument("--output", type=Path, required=True)

    compare = sub.add_parser("compare-windows")
    compare.add_argument("--window", type=Path, action="append", required=True)
    compare.add_argument("--output", type=Path, required=True)

    econ = sub.add_parser("economics")
    econ.add_argument("--owned", type=Path, required=True)
    econ.add_argument("--owned-reference", type=Path)
    econ.add_argument("--hosted", type=Path, required=True)
    econ.add_argument("--machine", type=Path, required=True)
    econ.add_argument("--electricity-price-per-kwh", type=float, required=True)
    econ.add_argument("--life-months", default="24,36,48")
    econ.add_argument("--utilizations", default="0.10,0.25,0.50,0.75")
    econ.add_argument("--observed-utilization", type=float)
    econ.add_argument("--output", type=Path, required=True)

    hosted = sub.add_parser("hosted-template")
    hosted.add_argument("--output", type=Path, required=True)

    report = sub.add_parser("report")
    report.add_argument("--machine", type=Path, required=True)
    report.add_argument("--receipts", type=Path)
    report.add_argument("--windows", type=Path)
    report.add_argument("--economics", type=Path)
    report.add_argument("--output", type=Path, required=True)
    return root


def main() -> int:
    args = parser().parse_args()
    try:
        if args.command == "catalog":
            print(json.dumps(catalog(), indent=2, sort_keys=True))
            return 0

        if args.command == "validate-machine":
            value = validate_machine(
                load_json(args.machine), require_complete=args.require_complete
            )
            print(
                json.dumps(
                    {
                        "machine_id": value["machine_id"],
                        "status": value["status"],
                        "valid": True,
                    },
                    sort_keys=True,
                )
            )
            return 0

        if args.command == "state-template":
            value = state_template(args.state)
            write_json(args.output, value)
            print(args.output)
            return 0

        if args.command == "plan":
            workload, operation, variant = find_workload(
                args.workload, args.variant
            )
            if args.state not in catalog()["state_classes"]:
                raise FleetError("unknown state class")
            print(
                plan_text(
                    workload,
                    operation,
                    variant,
                    args.repo_root.resolve(),
                    args.state,
                    args.state_dir.resolve(),
                ),
                end="",
            )
            return 0

        if args.command == "run":
            return run_benchmark(args)

        if args.command == "reduce-window":
            manifest = load_json(args.manifest)
            reduced = reduce_window(
                manifest, args.manifest.resolve().parent
            )
            write_json(args.output, reduced)
            print(args.output)
            return 0

        if args.command == "compare-windows":
            result = compare_windows(
                [load_json(path) for path in args.window]
            )
            write_json(args.output, result)
            print(args.output)
            return 0

        if args.command == "economics":
            result = economics(
                load_json(args.owned),
                load_json(args.hosted),
                load_json(args.machine),
                args.electricity_price_per_kwh,
                parse_csv_int(args.life_months),
                parse_csv_float(args.utilizations),
                (
                    load_json(args.owned_reference)
                    if args.owned_reference is not None
                    else None
                ),
                args.observed_utilization,
            )
            write_json(args.output, result)
            print(args.output)
            return 0

        if args.command == "hosted-template":
            write_json(
                args.output,
                {
                    "schema_version": 1,
                    "document_type": "glaeda-hosted-equivalent-job-receipt",
                    "backend": "replace-with-hosted-backend",
                    "measurement_date": "YYYY-MM-DD",
                    "validated": False,
                    "workload_id": "replace-with-workload-id",
                    "variant": None,
                    "source_commit": "replace-with-exact-commit",
                    "source_tree": "replace-with-exact-tree",
                    "operation_digest": "replace-with-exact-operation-digest",
                    "toolchain_digest": "replace-with-exact-toolchain-digest",
                    "state_class": "cold",
                    "actual_wall_seconds": None,
                    "queue_delay_seconds": None,
                    "measurement_evidence_sha256": None,
                    "rate_per_minute": None,
                    "rate_source": None,
                    "billing_currency": "USD",
                    "billing_increment_seconds": 1,
                    "minimum_billed_seconds": 0,
                    "fx_to_purchase_currency": None,
                    "fx_observed_at": None,
                },
            )
            print(args.output)
            return 0

        if args.command == "report":
            machine = validate_machine(
                load_json(args.machine), require_complete=False
            )
            receipts = collect_json_files(
                args.receipts, "glaeda-owned-fleet-benchmark-receipt"
            )
            windows = collect_json_files(
                args.windows,
                (
                    "glaeda-owned-fleet-window-receipt",
                    "glaeda-owned-fleet-window-partial-receipt",
                ),
            )
            econ = collect_json_files(
                args.economics, "glaeda-owned-vs-hosted-economics"
            )
            args.output.parent.mkdir(parents=True, exist_ok=True)
            args.output.write_text(
                markdown_report(machine, receipts, windows, econ),
                encoding="utf-8",
            )
            print(args.output)
            return 0

    except (
        FleetError,
        OSError,
        subprocess.SubprocessError,
        ValueError,
        TypeError,
    ) as exc:
        print(f"fleet benchmark error: {exc}", file=sys.stderr)
        return 2
    raise AssertionError("unreachable")
