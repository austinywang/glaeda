"""glaeda_fleet_labels: the one rule that turns a fleet manifest member into GitHub runner labels.

cmuxterm-hq build-fleet/FLEET-MEMBERSHIP.md makes the manifest the only source of a member's
behaviour. glaeda-cmux-runner issues these labels at install time, and glaeda-mini-fleet counts
members per pool label, so both import this module instead of keeping their own copy.

Pure: no I/O. Whether an Xcode is really installed is the caller's question (xcode_ok), because
the runner checks the machine it runs on and glaeda-mini-fleet checks what it observed over SSH.
"""

from __future__ import annotations

import re
from typing import Any, Callable

MINI_LABEL = "glaeda-mini"
# A host with the ios-simulators role, verified on the mini: every runtime build the manifest's ios_simulator.runtimes
# declares (23F77) is available, because the pinned Xcode refuses any other iOS SDK's simulators. With nothing
# declared, the member carries no label.
IOS_SIM_LABEL = "glaeda-ios-sim"
IOS_SIM_ROLE = "ios-simulators"
# dev machines take no jobs; borrowed machines run jobs only inside a VM, never as a host runner.
RUNNER_CLASSES = ("xl", "std", "light")
MEMBER_CLASSES = RUNNER_CLASSES + ("dev", "borrowed")
AVAILABILITY = ("dedicated", "opportunistic")
RUNNER_ROLE = "ci-runner"
VERSION_RE = r"[0-9][0-9.]*"
# Runners per member and the weighted capacity units they share (glaeda-cmux-runner-hook): a compile is
# 2 units, a light or GUI job 1. The manifest's defaults.runner.classes.<class> {runners, capacityUnits}
# overrides these, and a host's overrides.runner.classes likewise. compileSlots (default 1) is how many
# compiles run at once: more than one needs cmux's compile admission to keep its state per runner.
# canonicalRoots (default 1) is how many canonical roots (/private/tmp/cmux-ci, -2, ...) the mini has: that
# many runners (instances 0 to canonicalRoots - 1) also carry the root pool label (root_label), and the rest
# carry the side pool label (side_label) instead. guiRunners (0 or 1, default 0) makes the mini's last runner its
# gui runner: it carries only the gui pool label (gui_label), so GitHub hands each mini at most one GUI job, the
# one its single gui token allows, and a second GUI job waits in GitHub's queue for another mini's gui runner
# instead of on a root runner here (cmuxterm-hq#661). It is neither a root nor a side runner.
CLASS_CAPACITY = {"xl": (8, 8), "std": (4, 4), "light": (2, 2)}
MAX_RUNNERS = 16
# A host's overrides.runner {trustedRef: refs/heads/main, trustedRepo: owner/name} makes its runners trusted-only:
# the hook admits only push and schedule jobs of that one repository on that ref, and the pool label is
# glaeda-trusted-<class>-xcode-<version>, so a PR run's picker never counts or routes to them. For a
# mini that holds a secret, such as the fleet-cas signing key.
TRUSTED_REF_RE = r"refs/heads/[A-Za-z0-9._/-]+"
REPO_RE = r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+"


def merge(base: dict[str, Any], over: dict[str, Any]) -> dict[str, Any]:
    """The deep merge glaeda-mini-fleet uses for a host's overrides over the manifest defaults."""
    out = dict(base)
    for key, value in over.items():
        out[key] = merge(out[key], value) if isinstance(value, dict) and isinstance(out.get(key), dict) else value
    return out


def pool_label(klass: str, version: str, trusted: bool = False) -> str:
    """The single label a pool picker routes a whole run to: class plus verified Xcode, dedicated only."""
    return f"glaeda-{'trusted-' if trusted else ''}{klass}-xcode-{version}"


def root_label(pool: str) -> str:
    """The label of a pool's root runners (instances below canonicalRoots): the jobs that build in or restore
    into the canonical root (compile, app-host shards, cli-product, lag) run on it, so GitHub queues them
    until a root runner is free instead of handing one to a runner whose mini has no free root."""
    return pool.replace("glaeda-", "glaeda-root-", 1)


def side_label(pool: str) -> str:
    """The label of a pool's non-root runners (instances from canonicalRoots on). A light side lane (no
    canonical root, no persistent state, no GUI) runs on it, so it never takes a root runner that a compile
    or app-host job is waiting for. A mini whose every runner is a root runner has none."""
    return pool.replace("glaeda-", "glaeda-side-", 1)


def gui_label(pool: str) -> str:
    """The label of a pool's gui runner (the last instance when guiRunners is 1). The GUI jobs (app-host shards,
    tests-build-and-lag, app-host-test-rerun) run on it: each needs the mini's one gui token (one console session),
    so one runner per mini carries the label, and its listener gate holds while the token or every root is taken."""
    return pool.replace("glaeda-", "glaeda-gui-", 1)


def xcode_apps(manifest: dict[str, Any], member: str) -> list[dict[str, Any]] | None:
    """The member's Xcode apps (defaults merged with its overrides), or None if the shape is wrong."""
    host = manifest["hosts"][member]
    defaults, overrides = manifest.get("defaults") or {}, host.get("overrides") or {}
    if not isinstance(defaults, dict) or not isinstance(overrides, dict):
        return None
    xcode = merge(defaults, overrides).get("xcode") or {}
    apps = xcode.get("apps") if isinstance(xcode, dict) else None
    return [a for a in apps if isinstance(a, dict)] if isinstance(apps, list) else []


def member_labels(manifest: Any, member: str, xcode_ok: Callable[[dict[str, Any]], bool],
                  ios_sim_ok: Callable[[list[dict[str, Any]], list[str]], bool] | None = None
                  ) -> tuple[dict[str, Any] | None, str | None]:
    """(member summary with labels, None) or (None, why this member cannot be a runner)."""
    if not isinstance(manifest, dict) or not isinstance(manifest.get("hosts"), dict):
        return None, "manifest has no hosts table"
    host = manifest["hosts"].get(member)
    if not isinstance(host, dict):
        return None, f"{member} is not a member in the manifest"
    klass, availability = host.get("class"), host.get("availability")
    roles = host.get("roles") if isinstance(host.get("roles"), list) else []
    if klass not in MEMBER_CLASSES:
        return None, f"{member} class {klass!r} is not one of {', '.join(MEMBER_CLASSES)}"
    if klass not in RUNNER_CLASSES:
        return None, f"{member} is class {klass}, which never runs a GitHub runner on the host"
    if availability not in AVAILABILITY:
        return None, f"{member} availability {availability!r} is not one of {', '.join(AVAILABILITY)}"
    if RUNNER_ROLE not in roles:
        return None, f"{member} roles do not include {RUNNER_ROLE}"
    hardware = host.get("hardware")
    if not isinstance(hardware, str) or not re.fullmatch(r"[a-z0-9][a-z0-9-]*", hardware):
        return None, f"{member} has no hardware class, which names its fleet class receipt"
    apps = xcode_apps(manifest, member)
    if apps is None:
        return None, f"manifest defaults or {member} overrides is not an object"
    merged = merge(manifest.get("defaults") or {}, host.get("overrides") or {})
    runner = merged.get("runner")
    trusted_ref = runner.get("trustedRef") if isinstance(runner, dict) else None
    if trusted_ref is not None and not (isinstance(trusted_ref, str) and re.fullmatch(TRUSTED_REF_RE, trusted_ref)):
        return None, f"{member} runner.trustedRef must be a branch ref such as refs/heads/main"
    trusted_repo = runner.get("trustedRepo") if isinstance(runner, dict) else None
    if (trusted_ref is None) != (trusted_repo is None) or (
            trusted_repo is not None and not (isinstance(trusted_repo, str) and re.fullmatch(REPO_RE, trusted_repo))):
        return None, f"{member} runner.trustedRef and runner.trustedRepo (owner/name) go together"
    ready = [a for a in apps if re.fullmatch(VERSION_RE, str(a.get("version") or "")) and xcode_ok(a)]
    versions = [str(a["version"]) for a in ready]
    # Opportunistic members never carry a pool label, so they never receive a required job.
    pools = [pool_label(klass, v, bool(trusted_ref)) for v in versions] if availability == "dedicated" else []
    # Declared (the ios-simulators role) and, when the caller can look (the installer on the mini), a simulator
    # runtime actually present: an iOS job routed here must be able to boot one. Without a check (the fleet
    # plan) this is the declared view.
    ios = merged.get("ios_simulator")
    builds = ios.get("runtimes") if isinstance(ios, dict) else None
    builds = [b for b in builds if isinstance(b, str) and b] if isinstance(builds, list) else []
    ios_sim = (IOS_SIM_ROLE in roles and bool(ready) and bool(builds)
               and (ios_sim_ok is None or ios_sim_ok(ready, builds)))
    labels = [MINI_LABEL, f"glaeda-class-{klass}", f"glaeda-{availability}",
              *(["glaeda-trusted"] if trusted_ref else []), *[f"xcode-{v}" for v in versions], *pools,
              *([IOS_SIM_LABEL] if ios_sim else [])]
    disk = merged.get("disk")
    floor = disk.get("min_free_gib") if isinstance(disk, dict) else None
    runners, units = CLASS_CAPACITY[klass]
    declared = ((runner.get("classes") or {}).get(klass) if isinstance(runner, dict)
                and isinstance(runner.get("classes"), dict) else None)
    if declared is not None:
        if not isinstance(declared, dict):
            return None, f"runner.classes.{klass} is not an object"
        runners, units = declared.get("runners", runners), declared.get("capacityUnits", units)
        compile_slots = declared.get("compileSlots", 1)
        roots = declared.get("canonicalRoots", 1)
        gui = declared.get("guiRunners", 0)
    else:
        compile_slots, roots, gui = 1, 1, 0
    if not (isinstance(runners, int) and not isinstance(runners, bool) and 1 <= runners <= MAX_RUNNERS):
        return None, f"runner.classes.{klass}.runners must be 1 to {MAX_RUNNERS}"
    if not (isinstance(units, int) and not isinstance(units, bool) and 2 <= units <= 4 * MAX_RUNNERS):
        return None, f"runner.classes.{klass}.capacityUnits must be 2 to {4 * MAX_RUNNERS} (a compile is 2)"
    if not (isinstance(compile_slots, int) and not isinstance(compile_slots, bool)
            and 1 <= compile_slots <= max(1, units // 2)):
        return None, f"runner.classes.{klass}.compileSlots must be 1 to {max(1, units // 2)} (a compile is 2 units)"
    if not (isinstance(roots, int) and not isinstance(roots, bool) and 1 <= roots <= runners):
        return None, f"runner.classes.{klass}.canonicalRoots must be 1 to {runners} (one root runner per root)"
    if not (isinstance(gui, int) and not isinstance(gui, bool) and 0 <= gui <= 1 and roots + gui <= runners):
        return None, (f"runner.classes.{klass}.guiRunners must be 0 or 1, and canonicalRoots plus guiRunners at most "
                      f"runners ({runners}): the gui runner is the last instance, never a root runner")
    if compile_slots > roots:
        return None, (f"runner.classes.{klass}.compileSlots {compile_slots} needs canonicalRoots {compile_slots} "
                      "(every compile holds a root)")
    return {"member": member, "class": klass, "availability": availability, "roles": roles,
            "labels": list(dict.fromkeys(labels)), "pools": list(dict.fromkeys(pools)),
            "minFreeGib": floor if isinstance(floor, (int, float)) and floor > 0 else None,
            "hardware": hardware, "xcodeApps": [str(a.get("path")) for a in ready], "iosSim": ios_sim,
            "runners": runners, "capacityUnits": units, "compileSlots": compile_slots,
            "trustedRef": trusted_ref, "trustedRepo": trusted_repo,
            "canonicalRoots": roots, "rootPools": [root_label(label) for label in dict.fromkeys(pools)],
            "sidePools": [side_label(label) for label in dict.fromkeys(pools)],
            "guiRunners": gui, "guiPools": [gui_label(label) for label in dict.fromkeys(pools)] if gui else []}, None


def declared_pools(manifest: Any, xcode_ok: Callable[[str, dict[str, Any]], bool] | None = None) -> dict[str, list[str]]:
    """{pool label: [members]}. With no xcode_ok, every manifest Xcode counts (the declared view);
    pass xcode_ok(member, app) with observed results for the conforming view."""
    out: dict[str, list[str]] = {}
    hosts = manifest.get("hosts") if isinstance(manifest, dict) else None
    for member in hosts if isinstance(hosts, dict) else {}:
        check = (lambda app, m=member: xcode_ok(m, app)) if xcode_ok else (lambda app: True)
        summary, _ = member_labels(manifest, member, check)
        for label in (summary or {}).get("pools", []):
            out.setdefault(label, []).append(member)
    return {label: sorted(members) for label, members in sorted(out.items())}
