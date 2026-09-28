# Podman current execution-closure audit

Issue: #291  
Lane: #778 lane 8  
Audited baseline: `6788185a3a994f248638189e4cd6da631a930c86`

This audit records the Podman execution authority reachable from the current Renderprove native
probe planner and separates it from the tighter disposable Ubuntu Noble probe model. The evidence
here is source inspection plus disposable Linux CI evidence. It makes zero physical Mac, Lima, or
personal-worker host claim.

Repository search at the audited baseline finds `plan_renderprove_native_probe` only in its defining
module and tests. There is no in-tree production caller at that baseline. The planner is therefore a
dormant execution-boundary surface today; wiring it later would activate the authorities listed
below unless the complete closure is consumed first.

## Current Renderprove planner reachability

`src/renderprove_native_probe.rs` binds the outer `/usr/bin/podman` executable through the existing
launch path, drops launcher credentials to the runner uid/gid, sets a fixed PATH, and supplies the
container-side isolation flags described below. That outer binding closes only the first executable.
Podman remains a host-side program that reads configuration, opens mutable state, loads ELF objects,
and selects helpers after launch.

| Reachable authority | Current source | Lifetime / mutability | #291 status |
| --- | --- | --- | --- |
| `/usr/bin/podman` | fixed outer executable | host package | outer entry is bound; downstream closure remains open |
| runner `HOME` | `runner.home` is passed to Podman | runner-writable, cross-run | user container config, auth, policy, and storage defaults remain reachable |
| `XDG_RUNTIME_DIR` | `runner.runtime_directory` is passed | runner-writable, cross-run | rootless libpod state and pause-process state remain reachable |
| `PATH=/usr/local/bin:/usr/bin:/bin` | fixed environment string | host-wide lookup directories | downstream helper lookup can still depend on mutable/unbound directory contents |
| user container configuration | default `$HOME/.config/containers/*` lookup | runner-writable, cross-run | can redirect runtime, conmon, hooks, storage, networking, and policy |
| system/vendor container configuration | Podman default `/etc/containers` and `/usr/share/containers` lookup | host package/admin state | content is outside the current planner receipt |
| graphroot, runroot, static dir, volume path, libpod database | Podman storage defaults | writable persistent host state | explicit attempt-private lifecycle is absent |
| OCI runtime | no current `--runtime=/usr/bin/crun` selector | config/default/PATH selected | exact executable and ELF closure are unbound |
| conmon | no current `--conmon=/usr/bin/conmon` selector | config/default/PATH selected | exact executable and ELF closure are unbound |
| init helper | current argv uses `--init` without an exact init path | Podman default helper selection | catatonit identity and ELF/static facts are unbound |
| rootless namespace helpers | `--userns=keep-id` can require `newuidmap` / `newgidmap` | host helper binaries plus `/etc/subuid` and `/etc/subgid` | helper identity, package mode, and subordinate-ID authority are unbound |
| rootless pause process | Podman rootless first-use behavior | mutable XDG runtime state | cross-run PID-file/process debt is reachable |
| pre-exec hooks, OCI hooks, CDI | current planner supplies no empty/exact hook or CDI inputs | user/system configuration | arbitrary helper authority remains reachable through Podman defaults |
| storage helper programs | storage driver/config may select helpers such as `fuse-overlayfs` | config/PATH selected | executable and transitive ELF closure are unbound |
| network backend/helper state | current planner supplies `--network=none` but no exact network-config directory | Podman config plus helper/state defaults | config/helper selection and lock/state lifecycle remain open |
| registry/auth/policy/cert inputs | current planner supplies no explicit sealed files | user/system config, often cross-run | mutable redirection and policy input remain reachable |
| seccomp/AppArmor defaults | no exact host-side profile receipt in the current planner | package/system configuration | effective profile input remains outside the current receipt |
| GNU dynamic loader and `DT_NEEDED` closure | every dynamic host executable/helper | host loader config/cache and shared libraries | interpreter, cache/config, preload absence, and transitive objects are unbound |
| protected target mount | attempt receipt supplies the target source | attempt-scoped | source binding is admitted; this does not close Podman's host dependencies |
| image digest plus `--pull=never` | fixed image reference/argv | image/content state | reduces image ambiguity; host execution closure remains open |
| container isolation flags | fixed argv (`--read-only`, `--network=none`, capability drop, no-new-privileges, limits, tmpfs) | per container | useful container boundary; host helper/config selection remains open |

The current planner also uses `--rm`, which bounds the named container object after a normal run.
That flag does not define a complete lifecycle for libpod databases, graph/run roots, pause state,
helper state, auth/config inputs, or other Podman-owned files under the runner account.

## Disposable closure demonstrated in Ubuntu Noble CI

The disposable package and stopped-container probes exercise a narrower model before any product
activation. They currently validate, among other checks:

- exact Ubuntu Noble architecture, package set, Podman/Git baselines, executable ownership/modes,
  catatonit fallback identity, setuid namespace helpers, and protected/empty pre-exec hook inputs;
- exact absolute `crun` and `conmon` selectors, local-only Podman mode, cgroup manager, transient
  store, run-private tmp/config/network/auth/hook inputs, and explicit storage roots;
- empty hostile user configuration, explicit registries/storage/auth files, precreated network locks,
  rootless pause-process containment, crash recovery, and bounded cleanup;
- bounded target tmpfs, explicit cgroup limits, offline image installation, read-only additional image
  store, exact seccomp profile receipt, stopped-container inspection, hostile resource pressure, and
  authoritative cgroup kill/cleanup;
- admitted ELF dependency declarations, dynamic-loader object shape, loader configuration/cache,
  descriptor-bound top-level executable prerequisites, and descriptor-bound loader prerequisites.

These probes are disposable Linux evidence. They do not grant production readiness to the current
Renderprove planner and do not establish any physical personal-worker host fact.

## Cross-run lifecycle required by the disposable model

Attempt-private writable state is created under a unique disposable root and destroyed after the
probe: HOME/XDG state, Podman config, graphroot, runroot, temp state, auth input, hook input, network
locks/state, container logs/IDs, generated OCI state, cgroups, and bounded target files.

Shared read-only state is limited to reviewed package executables/loader state and, in the container
probe, an image store that is remounted read-only and content-snapshotted across execution.

The model admits no shared writable Podman state. Runner HOME configuration/auth, persistent XDG
libpod state, global writable graphroot/runroot, mutable hook/CDI directories, and ambient helper
search are outside the admitted closure.

## Provider-neutral probe admission

Disposable integration probes use one deliberate execution opt-in:

`GLAEDA_DISPOSABLE_PROBE=1`

Provider identity is provenance only. CI records `GLAEDA_PROBE_PROVIDER=github-hosted-ubuntu`, while
probe code never treats that value as admission evidence. The workflows exercise the legacy hosted
provider token and the provenance token without the opt-in and require refusal before package or
container probe mutation begins.

Admission still depends on the concrete checks inside each probe: uid 0 where required, supported
native architecture, exact tools/packages, safe metadata, exact configuration/helper/runtime facts,
cgroup behavior, and bounded cleanup.

## Remaining product boundary

The smallest supported hardening in this lane is the provider-neutral disposable admission rule and
this complete current-code reachability record. Production Renderprove execution remains deferred
until its launch path consumes a complete descriptor/content-bound Podman closure and an explicit
attempt-private lifecycle for every reachable mutable input and helper. Any future activation must
preserve the existing direct-ELF, privilege-drop, protected-mount, and exact-evidence requirements.
