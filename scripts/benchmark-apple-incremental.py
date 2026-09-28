#!/usr/bin/env python3
"""Physical Mac benchmark: owned Swift fixture, real Git pull and dependency update.

Builds stay under this repository's target directory and are removed on exit.
The output directory retains private logs and a bounded JSON summary. No user
checkout is edited and no network remote is contacted.
"""
import argparse
import contextlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time


@contextlib.contextmanager
def owned_fixture(parent):
    root = Path(tempfile.mkdtemp(prefix="apple-incremental-", dir=parent)).resolve()
    try:
        yield root
    finally:
        if (root / "checkout/.glaeda/apple-build/inflight.json").exists():
            print(f"Unsettled native work: preserved fixture at {root}", flush=True)
        else:
            shutil.rmtree(root)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, type=Path, help="new directory for benchmark evidence")
    args = parser.parse_args()
    args.output.mkdir(mode=0o700)
    output = args.output.resolve()
    repo = Path(__file__).resolve().parent.parent
    (repo / "target").mkdir(exist_ok=True)
    results = []

    def command(argv, cwd=None):
        result = subprocess.run(argv, cwd=cwd, capture_output=True, text=True, timeout=300)
        if result.returncode:
            raise RuntimeError(f"fixture command failed: {argv[0]}: {result.stderr[-2000:]}")
        return result.stdout.strip()

    def git(root, *args):
        return command(["/usr/bin/git", "-c", "user.name=Glaeda Benchmark", "-c",
                        "user.email=benchmark@example.invalid", "-C", str(root), *args])

    def commit(root, message):
        git(root, "add", ".")
        git(root, "commit", "-qm", message)
        return git(root, "rev-parse", "HEAD")

    with owned_fixture(repo / "target") as base:
        dependency = base / "Dependency"
        dependency.mkdir()
        git(dependency, "init", "-q")
        (dependency / "Sources/Dependency").mkdir(parents=True)
        (dependency / "Package.swift").write_text('''// swift-tools-version: 6.0
import PackageDescription
let package = Package(name: "Dependency", products: [.library(name: "Dependency", targets: ["Dependency"])], targets: [.target(name: "Dependency")])
''')
        dependency_source = dependency / "Sources/Dependency/Value.swift"
        dependency_source.write_text('public func dependencyValue() -> Int { 10 }\n')
        commit(dependency, "version one")
        git(dependency, "tag", "1.0.0")
        dependency_source.write_text('public func dependencyValue() -> Int { 20 }\n')
        commit(dependency, "version two")
        git(dependency, "tag", "1.1.0")
        upstream = base / "upstream"
        upstream.mkdir()
        git(upstream, "init", "-q")
        manifest = '''// swift-tools-version: 6.0
import PackageDescription
let package = Package(name: "IncrementalFixture", products: [.executable(name: "Fixture", targets: ["Fixture"])], dependencies: [.package(url: "DEPENDENCY", exact: "1.0.0")], targets: [.target(name: "Core", dependencies: [.product(name: "Dependency", package: "Dependency")]), .target(name: "Consumer", dependencies: ["Core"]), .executableTarget(name: "Fixture", dependencies: ["Consumer"])])
'''.replace("DEPENDENCY", dependency.as_uri())
        (upstream / "Package.swift").write_text(manifest)
        (upstream / ".gitignore").write_text('.glaeda/\n.build/\nPackage.resolved\nglaeda.apple.json\n')
        for name in ("Core", "Consumer", "Fixture"):
            (upstream / "Sources" / name).mkdir(parents=True)
        core = 'import Dependency\npublic func value() -> Int { dependencyValue() + 1 }\n'
        (upstream / "Sources/Core/Value.swift").write_text(core)
        (upstream / "Sources/Consumer/Consumer.swift").write_text('import Core\npublic func answer() -> Int { value() }\n')
        (upstream / "Sources/Fixture/main.swift").write_text('import Consumer\nprint(answer())\n')
        revision_a = commit(upstream, "baseline")
        project = base / "checkout"
        command(["/usr/bin/git", "clone", "--quiet", str(upstream), str(project)])

        for hashing in (False, True):
            git(project, "checkout", "-q", "-B", "trial", revision_a)
            config = {"schema_version": 1, "profiles": {"app": {"engine": "swiftpm", "product": "Fixture",
                      "incremental_file_hashing": hashing, "incremental_diagnostics": True}}}
            (project / "glaeda.apple.json").write_text(json.dumps(config))
            prefix = "hash" if hashing else "mtime"

            def build(case, expected):
                started = time.monotonic()
                run = subprocess.Popen([str(repo / "scripts/apple-build"), "run", "--project", str(project)],
                                       stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
                try:
                    stdout, stderr = run.communicate(timeout=300)
                except BaseException:
                    # Let the controller forward termination to its native group.
                    # An unsettled in-flight receipt prevents fixture cleanup.
                    run.terminate()
                    run.communicate(timeout=30)
                    raise
                (output / f"{prefix}-{case}.stderr").write_text(stderr)
                if run.returncode:
                    raise RuntimeError(f"{prefix}-{case}: build failed; inspect private evidence")
                receipt = json.loads(stdout)
                elapsed = time.monotonic() - started
                log = (project / ".glaeda/apple-build" / f"run-{receipt['run_id']}.log").read_text(errors="replace")
                (output / f"{prefix}-{case}.log").write_text(log)
                scratch = project / ".glaeda/apple-build/cache" / receipt["cache_key"] / "scratch"
                binaries = list(scratch.glob("*/debug/Fixture"))
                if len(binaries) != 1 or command([str(binaries[0])]) != str(expected):
                    raise RuntimeError(f"{case}: compiled executable produced the wrong answer")
                diagnostics = [line for line in log.splitlines() if "remark:" in line or "Compiling " in line]
                results.append({"file_hashing": hashing, "case": case, "wall_seconds": round(elapsed, 6),
                                "output_validated": expected, "cache_key": receipt["cache_key"],
                                "diagnostics": diagnostics[:300]})
                (output / "results.json").write_text(json.dumps(results, indent=2) + "\n")
                print(prefix, case, round(elapsed, 3), flush=True)

            source = project / "Sources/Core/Value.swift"
            build("cold", 11)
            build("unchanged", 11)
            os.utime(source, None)
            build("timestamp-only", 11)
            source.write_text(core.replace("+ 1", "+ 2"))
            build("implementation-edit", 12)
            revision_b = commit(project, "implementation edit")
            git(project, "checkout", "-q", revision_a)
            build("revert", 11)
            git(project, "checkout", "-q", revision_b)
            build("branch-return", 12)
            git(project, "checkout", "-q", "-B", "trial", revision_a)
            source.write_text(core.replace("value()", "value(extra: Int = 0)").replace("+ 1", "+ 1 + extra"))
            build("public-api", 11)
            git(project, "restore", "Sources/Core/Value.swift")
            git(upstream, "checkout", "-q", "-B", "main", revision_a)
            (upstream / "Sources/Core/Value.swift").write_text(core.replace("+ 1", "+ 3"))
            commit(upstream, "incoming source change")
            git(project, "pull", "--ff-only", "origin", "main")
            build("git-pull", 13)
            (project / "Package.swift").write_text(manifest.replace('exact: "1.0.0"', 'exact: "1.1.0"'))
            build("dependency-update", 23)
            git(project, "restore", "Package.swift")
        (output / "metadata.json").write_text(json.dumps({
            "glaeda_head": git(repo, "rev-parse", "HEAD"), "glaeda_dirty": bool(git(repo, "status", "--porcelain")),
            "swift": command(["/usr/bin/xcrun", "swift", "--version"]),
            "xcode": command(["/usr/bin/xcodebuild", "-version"]),
            "workload": "three small Swift targets and one local versioned dependency",
            "scope": "fixture timings, not cmux or Idlesse latency", "fixture_cleanup": "temporary directory removed on exit"
        }, indent=2) + "\n")
    for file in output.iterdir():
        file.chmod(0o600)


if __name__ == "__main__":
    main()
