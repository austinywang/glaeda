# CMUX-owned fleet enrollment

Tracking issue: #1056.

This path turns a CMUX-controlled Mac or Linux host into a reviewed fleet node while the existing owners keep their responsibilities. GitHub Actions, CMUX's controller, direct SSH, MDM/configuration management, hosted runners, and future schedulers continue to manage the parts they already own. Glaeda owns the bounded enrollment record, role acceptance binding, lifecycle state, and a machine-status projection that reports role/candidate eligibility.

## Supported v1 bootstrap classes

The repository-owned bootstrap commands are read-only. They inspect an exact CMUX checkout and exact Glaeda executable, produce a bounded receipt, and leave package installation, MDM, SSH, networking, and machine account policy with their existing owners.

- macOS: Apple Silicon for the v1 `cmux_macos_native_build` role; macOS major 15 or 26; CMUX `.xcode-version` must be `26.0`; the selected Xcode and macOS SDK must be generation 26; Git present; at least 25 GiB plus 25 GiB per concurrent build slot free by default (`--build-slots`, default 1, so 50 GiB; see `MACOS_SLOT_FREE_GIB` in `scripts/cmux_fleet_bootstrap.py` for what a slot holds); AC system sleep disabled for unattended work. `cmux-mac-build-large` requires at least 8 logical CPUs and 16 GiB total memory.
- Linux: x86_64 or arm64; Ubuntu 24.04 or Debian 12; kernel major 6 or newer; Git, Python, systemd, bubblewrap, cgroup v2, pressure signals; at least 40 GiB free and 8 GiB available memory by default. `cmux-linux-ci-medium` and `cmux-linux-agent-medium` require at least 4 logical CPUs and 8 GiB total memory. `cmux_linux_ci` also checks `curl`, `tar`, `gzip`, and `ldd`.

A different machine class starts as a bootstrap refusal until the reviewed policy changes.

## Roles

The role vocabulary is stable enough to name future work:

| Role | v1 enrollment | Exact acceptance owner |
| --- | --- | --- |
| `cmux_macos_native_build` | supported | `cmux.macos.dev-check@1` via `scripts/ci/cmux_workload_profile.py` |
| `cmux_linux_ci` | supported | `cmux.ci.guard@1` via `scripts/ci/cmux_workload_profile.py` |
| `cmux_macos_test` | reserved | acceptance workload must land first |
| `cmux_linux_agent` | reserved | acceptance workload must land first |
| `artifact_cache` | reserved | acceptance workload must land first |
| `background_replay` | reserved | acceptance workload must land first |
| `benchmark` | reserved | acceptance workload must land first |
| `diagnostic` | reserved | acceptance workload must land first |

The v1 enrollment validator and bootstrap refuse reserved roles. A role becomes enrolable only when its repository-owned CMUX profile is present at the reviewed generation in the exact CMUX checkout. For supported roles, eligibility then requires node state `eligible` plus a current accepted receipt matching node ID, enrollment generation, Glaeda generation, enrolled toolchain generation, and the role's exact profile ID/generation.

The versioned schema carries the full role vocabulary in `$defs.knownRole` while `allowedExecutionRoles` accepts only the v1 supported subset.

## Enrollment identity and privacy

`examples/cmux-fleet/enrollment-v1.schema.json` is the public v1 schema. The record contains only:

- opaque `cmux-*` node ID;
- architecture;
- OS family and version class;
- hardware capability class;
- accepted toolchain generations;
- current repository-owned CMUX profile ID/generation for every allowed role;
- allowed roles;
- operator/fleet ownership scope;
- enrollment generation;
- installed Glaeda generation;
- lifecycle state and reviewed quarantine reason.

The schema has no hostname, serial number, username, private address, SSH identity, MDM identifier, or raw hardware inventory fields. Unknown top-level fields are rejected.

Role eligibility binds repository-owned CMUX semantic profiles rather than a duplicate acceptance recipe. The low-level `finalize-acceptance` command can validate externally supplied semantic evidence for diagnostics, but v2 marks that evidence as externally supplied and it cannot mint an accepted receipt. Candidate-eligible receipts come from `accept-local`, which owns the local CMUX process invocation. Finalization emits `glaeda-cmux-fleet-acceptance/v2`, which binds the exact CMUX result digest, CMUX environment class/toolchain identity, a fresh post-run bootstrap digest, and the exact Glaeda fleet-contract generation in addition to enrollment/Glaeda/toolchain/process-settlement identity. A change to `cmux_fleet.py` or `cmux_fleet_bootstrap.py` therefore makes prior acceptance stale until the role is accepted again. The CMUX semantic result also carries a bounded repository-owned environment class; Glaeda does not interpret that class, but it requires the class to participate in CMUX's recomputed semantic comparison identity before accepting the result. Bootstrap reads `scripts/ci/cmux-workload-profiles.json`, requires `cmux_linux_ci -> cmux.ci.guard@1` or `cmux_macos_native_build -> cmux.macos.dev-check@1`, and checks that the selected profile admits the observed OS/architecture. `accept-local` consumes the canonical `cmux-workload-result/v1` emitted by that profile and records its exact digest/state together with CMUX environment/toolchain identity, a Glaeda-local attempt digest, the fresh post-run bootstrap digest, and Glaeda-owned enrollment/toolchain/process-settlement identity. Glaeda never reconstructs CMUX argv, artifacts, or pass/fail semantics.

Acceptance v2 intentionally supersedes the earlier v1 receipt format. A v1 receipt cannot supply candidate eligibility after this change; rerun bootstrap/enrollment as required for the current Glaeda generation, then run `accept-local` to mint a v2 receipt. No physical v1 acceptance receipt was claimed by the original rollout.

Canonical enrollment and acceptance documents used for lifecycle or routing-candidate decisions are read through a no-follow private-file gate: current-user owned, regular, single-link, mode `0600`, bounded size, and unchanged inode/metadata across the read. A loose, symlinked, or swapped file cannot supply eligibility. `transition` remains a side-effect-free planner. `transition-apply` holds one private mutation lock, re-reads the current enrollment under that lock, writes a mode-0600 same-directory stage, fsyncs the staged bytes, atomically replaces `enrollment.json`, fsyncs the fleet directory, and revalidates the published bytes before returning.

## Lifecycle

States are:

`discovered -> enrolling -> eligible -> draining/quarantined -> retired`

Supported recovery paths also include `draining -> eligible` and `quarantined -> enrolling`. Every transition into `eligible` requires at least one current accepted role receipt. Leaving quarantine advances the enrollment generation, so every pre-quarantine acceptance receipt becomes stale immediately.

`scripts/cmux-fleet status` reports role/candidate eligibility only. Its `routingCandidateEligible` field means the enrollment and current acceptance receipt satisfy this contract; `automaticDispatchAuthorized` stays `false`. #546 or another separately approved routing-promotion gate owns queue selection and dispatch, including queue/start prediction, hot locality, pressure, allowance scarcity, workload-class evidence, and operator policy. The status projection includes only bounded architecture, OS class, hardware class, accepted toolchain generations, role profiles, Glaeda generation, fleet scope, lifecycle state, and per-role eligibility. A node in `draining`, `quarantined`, or `retired` produces zero eligible roles even when an older acceptance receipt exists. An `eligible` node still produces zero eligible roles when its acceptance evidence is absent, rejected, or stale. Live pressure/heat remains a fresh local admission veto: #970 may publish advisory bounded snapshots, while #546/local execution admission re-observes the machine before dispatch.

For the quiet operator/agent view, use the separate read-only projection:

```bash
python3 scripts/cmux_fleet_operator_summary.py "$ENROLLMENT" \
  --acceptance "$ACCEPTANCE"
```

This emits `glaeda-cmux-fleet-operator-summary/v1`. A healthy accepted node reports
`attentionRequired: false`, `action: "none"`, and only the eligible role names.
Missing or stale acceptance reports one concrete `run_role_acceptance` action;
draining reports `observe_drain`; quarantine exposes only its reviewed bounded
reason and `inspect_quarantine`; retired nodes are quiet/inactive. The projection
carries no mutation or dispatch authority and intentionally lives outside the
Glaeda fleet-contract generation, so changing operator presentation does not
invalidate otherwise-current physical acceptance receipts.

Reviewed quarantine reasons are:

- `toolchain_mismatch`
- `disk_pressure`
- `failed_acceptance`
- `dirty_canonical_checkout`
- `service_mismatch`
- `hardware_failure`
- `stale_glaeda_generation`
- `unexplained_process_settlement`

## Prepare a fresh Mac mini

Before enrollment, one command takes a fresh build host to ready. It plans by default and changes nothing until `--apply`:

```bash
scripts/glaeda-mini-setup                              # preflight + plan
scripts/glaeda-mini-setup --apply                      # install, idempotent
scripts/glaeda-mini-setup --cmux-root ~/Projects/cmux  # also run the read-only bootstrap below
scripts/glaeda-mini-setup --uninstall [--apply]        # remove what --apply installed
scripts/glaeda-mini-setup --runner [--apply]           # GitHub Actions runner, see CMUX_MINI_RUNNER.md
```

Preflight reports macOS, hardware against the M4 Pro 14-core 48 GB target, Xcode apps and the cmux pin, Command Line Tools (not needed when a licensed Xcode is selected), free disk, Tailscale, Homebrew, the workload PATH and AC power. `--apply` installs `glaeda-disk`, `glaeda-worktree-reclaim` and `glaeda-worktree-reclaim-all` into `~/.local/bin`, their three LaunchAgents templated for the current user, the build-host directories (including a `--cache-root` under `~/.cache/glaeda/cmux-native-cache`) and three git defaults that are set only when unset. It also finds the Python 3.13+ that `glaeda-mini-enroll` will use (same search: Homebrew, then `~/.local/bin`, then `PATH`). If there is none, `--apply` runs `brew install python@3.13` when this user owns Homebrew; otherwise the host is not ready and the fix is printed, because on shared minis Homebrew often belongs to another account. Everything else is under `$HOME`; nothing uses sudo. Steps that need sudo or a human (pmset, Xcode licence and selection, Tailscale login, runner registration) are printed as operator steps. Runner registration and the compilation-cache node daemon (RFC #1134 M2) are reserved slots that this command never fills. The receipt is `~/.local/state/glaeda/mini-setup/receipt.json` (mode `0600`). With a HOME other than the account's own, launchctl is never called, which is how the tests and rehearsals run.

This command does not replace the bootstrap below: it imports its read-only helpers and runs it unchanged, so fleet-contract generations and acceptance receipts are unaffected.

## Keep a fleet in line with its manifest

`scripts/glaeda-mini-fleet` holds a whole fleet of build minis to one declarative manifest: each host's hardware class, macOS floor, the Xcode apps and selection it must carry, the SSH keys its login user must, may, and must not accept, the launchd jobs that must be running, and a free-disk floor.

```bash
scripts/glaeda-mini-fleet check                       # observe every host over SSH, diff, exit 1 on drift
scripts/glaeda-mini-fleet check --host <name> --json  # one host, machine-readable
scripts/glaeda-mini-fleet observe --out observed.json # raw observation; check/plan accept --observed
scripts/glaeda-mini-fleet plan                        # the fixes, as operator steps
scripts/glaeda-mini-fleet apply --host <name> [--yes] # the additive fixes; dry run without --yes
```

Observation runs `scripts/cmux_mini_probe.sh` over SSH as the login user, with no sudo and no Python, because a mini without an accepted Xcode licence has only a stub `/usr/bin/python3`. It reports key fingerprints and comments, never key material, tokens, or log contents. `apply` only adds: it appends a required key whose `public_key` is in the manifest and whose fingerprint matches, after copying `authorized_keys` aside, and it APFS-clones an Xcode with `cp -c` when the source's version and build match. A clone is a real directory that shares blocks with its source, so it costs no disk and avoids the symlinked-path compilation-cache replay failure. `apply` never removes keys, never uses sudo, skips hosts whose overrides say `"apply": false`, refuses hosts in `never_touch`, and ends with a fresh observation. Licence acceptance, `xcode-select`, and macOS updates need sudo and stay operator steps in `plan`.

A real manifest names hosts, people, and keys, so it does not belong in this repository. The default path is `~/.config/glaeda/mini-fleet.json` (or `--manifest`, or `$GLAEDA_MINI_FLEET_MANIFEST`); `examples/mini-fleet/manifest.example.json` shows the shape. Keys that nobody has ruled on carry `"status": "review"` and sit in `allow`, so `check` stays quiet about them but flags any key the manifest does not name. Xcode apps listed under `pending` are reported without counting as drift. A host's `node_id` assigns its opaque fleet node id (`cmux-mac-NNN`, unique across the manifest), so enrollment reruns always ask for the same one. `check` reads the enrolled id from `~/.config/glaeda/cmux-fleet/enrollment.json`: a host not enrolled yet is pending, with the `--node-id` to use, and a host enrolled under a different id is drift.

### Find every onboarding blocker in one pass

```bash
scripts/glaeda-mini-fleet preflight                   # every host, in parallel; exit 1 if any is not ready
scripts/glaeda-mini-fleet preflight cmux14 cmux15     # named hosts
scripts/glaeda-mini-fleet preflight --output json     # machine-readable, with the fixes grouped
```

`preflight` checks, read-only and over the same SSH session as `observe`, everything the bootstrap, `glaeda-mini-enroll`, local acceptance and cmux `./scripts/setup.sh` will later require, so a host no longer fails one step at a time:

- the pinned Xcode (the manifest's `xcode.select`, else its first `xcode.apps` entry): present, selected with `xcode-select`, licence accepted, first launch done (`xcodebuild -checkFirstLaunchStatus`, plus plugin-load errors from `xcodebuild -showsdks`), and its Metal toolchain (`xcrun metal --version`)
- `cargo`, `rustc`, `rustup`, `zig`, `git`, `xcodebuild` and `xcrun` on the workload's fixed PATH (`CMUX_WORKLOAD_TOOL_PATH`), with versions: zig against Ghostty's `minimum_zig_version` (the host's checkout, else the operator's `$GLAEDA_CMUX_ROOT`, default `~/Projects/cmux`), and the DiffSidecar Rust toolchain installed
- Python 3.13+ where `glaeda-mini-enroll` looks for it
- the cmux checkout (`cmux_root` in the manifest, default `~/cmux`): submodules initialized, no local changes, setup artifacts present
- AC sleep off, free disk against the manifest floor, and a macOS update that is installing or prepared and waiting for a restart
- progress: the candidate cmux names staged, a node id (manifest or enrolled), the enrollment and acceptance state, a registered runner
- who owns Homebrew, which decides whether a `brew install` needs `sudo -u <owner>`
- `secrets`: the paths (never the contents) of secret files the login user can read: everything under `/Users/Shared/cmux-build-fleet/secrets`, `~/.secrets`, `~/Library/Application Support/cmux-build-fleet/secrets` and `/Library/Application Support/cmux-build-controller/secrets` (subdirectories and dotfiles included), `~/.config/glaeda/*.key`, `~/.ssh/id_*` keys, `~/.netrc`, `~/.git-credentials`, `~/.aws/credentials`, `~/.config/rclone/rclone.conf`, `~/.config/gh/hosts.yml` when it holds an `oauth_token` and `~/.docker/config.json` when it holds an `auth`. A path with a newline or tab is skipped, so a file name cannot forge probe lines. Every PR job on a runner host runs as that user, so on a host with a registered runner or the `ci-runner` role any of them is `FAIL` for a person: make them root-only or move them to a host with no PR runners (cmuxterm-hq#595). On a host with no PR runners (a host with no runners, such as a dev-build worker, whose worker reads its own tokens as that user) they are listed as info. `hosts.<host>.accepted_secrets` lists exact paths (absolute, or `~/` for the login user's home) whose risk the operator accepted, such as the `~/.ssh/id_ed25519` some minis keep for cross-mini access: they report as info (`accepted: <paths>`), and only unlisted paths fail. It is an accepted risk, not an exemption from the threat model: the #595 threat model still applies (any PR job on the host can read the file), and each entry is reviewed in cmuxterm-hq `build-fleet/mini-fleet.json`.

It prints one host-by-check table (`ok`, `FAIL`, `todo` for a step onboarding itself performs, `?` when unknown), then each host's fixes in three groups: what the login user can do itself, what needs a password, and what needs a person. `todo` does not block; anything `FAIL` does. rustup runs with `RUSTUP_AUTO_INSTALL=0` and git with `GIT_OPTIONAL_LOCKS=0`, so the probe installs and writes nothing.

### Declare the toolchain

`defaults.toolchain` (overridable per host under `overrides.toolchain`) pins what every build host must carry, in one place: `xcode` (`app`, `version`, `build`, and `select`, default true), `metal` (default true), `zig_min`, `rustup_default`, `python_min` (at least 3.13), and `cmux` (`repo`, `ref`, `root`, `submodule_depth`, where 0 means full history). `preflight` and `fix` read it; the host's own Ghostty `minimum_zig_version` still applies when it is higher. When a toolchain is declared, `check` probes the preflight section too and reports toolchain drift (on hosts without a `node_id`, the cmux checkout checks are skipped).

### Repair what needs no root, then batch what does

```bash
scripts/glaeda-mini-fleet fix cmux7s-mac-mini cmux8s-mac-mini          # dry run: what it would run, what waits
scripts/glaeda-mini-fleet fix cmux7s-mac-mini cmux8s-mac-mini --yes    # run it; one log per host
scripts/glaeda-mini-fleet sudo-plan cmux7s-mac-mini cmux8s-mac-mini    # one reviewed root script per host
scripts/glaeda-mini-fleet sudo-plan cmux7s-mac-mini cmux8s-mac-mini --run   # ssh -t, one password per host
scripts/glaeda-mini-fleet sudo-plan cmux7s-mac-mini cmux8s-mac-mini --run --ask-once   # one password for all
```

`fix` probes like `preflight`, then runs every repair the login user can make, per host in parallel and in dependency order: a shallow cmux clone at the declared ref with submodules, an APFS clone of the pinned Xcode, a relocatable CPython copied from this Mac into `~/.local` (`$GLAEDA_MINI_FLEET_PYTHON`, else the newest `uv` CPython under `~/.local/share/uv/python`), Homebrew itself when `/opt/homebrew` is empty and the login user's (a shallow `git fetch` of Homebrew/brew from the `--from` host's `/opt/homebrew` over the LAN, else from GitHub, with `origin` set to `https://github.com/Homebrew/brew`), `brew install rustup zig python@3.13` with the rustup proxy links and `/opt/homebrew/bin/python3 -> python3.13` when the login user owns Homebrew, then `brew pin` on those (class receipts pin toolchain strings; preflight's `pins` warns about unpinned ones), plus `brew install gh` when preflight's `gh` check finds none on the workload PATH (cmux CI jobs call it with `GH_TOKEN`, so no `gh auth`; left unpinned, as no class receipt names it), `rustup default`, Glaeda and `glaeda-mini-setup --apply`, `~/glaeda` moved to `--glaeda-ref` (else the tip of main) with a clean fetch and a detached checkout when its `glaeda-mini-enroll` lacks an option onboarding passes (`--renew`, `--class-receipt`, `--class-receipt-sha256`, `--fleet-class`; a dirty `~/glaeda` is refused), the Metal toolchain, cmux `./scripts/setup.sh`, and the reviewed candidate, downloaded once on this Mac with `gh`, checked against `CANDIDATE_SHA256`, and copied to where `glaeda-mini-enroll --candidate` reads it. A repair whose prerequisite needs root (setup needs zig, which needs Homebrew) is listed as waiting. Each remote step is a function in `scripts/cmux_mini_fix.sh` that looks before it acts, so a rerun resumes, and none uses sudo or overwrites what it did not create. Logs go to `~/.local/state/glaeda/mini-fleet/fix-<time>/<host>.log`; a run ends with a fresh preflight.

`sudo-plan` writes one script per host with every root step preflight found: `xcode-select -s`, licence, first launch, `pmset` (no sleep on AC, `autorestart 1`), Homebrew installs as its owner via `sudo -H -u <owner>` plus the rustup and `python3` links and the pins, or, on a host with no Homebrew, only `mkdir /opt/homebrew` and a `chown` to the login user, so `fix` can fetch Homebrew into it and later installs need no root. It no longer runs the brew.sh installer, which stalled on a Command Line Tools install that the selected Xcode makes unnecessary. A prepared macOS update is noted, never restarted. `--run` runs each plan over `ssh -t`, one host after another, so the operator types each password once; the script keeps sudo's timestamp fresh while it runs. `--run --ask-once` is for a fleet that shares one password: it reads it once on this Mac (`getpass`), checks it on every host with `sudo -k -S -p '' true` and stops at the first refusal before any plan runs, then runs each host's plan as root under that one `sudo -k -S -p ''` over plain `ssh` (no `-t`). The password is the only thing on ssh's stdin, so it never reaches argv, a tty or a log, and the plan (generated for root: no `sudo -v`, no keepalive, no `sudo` before its steps, Homebrew steps still as their owner via `sudo -H -u`) goes in argv as with `ssh -t`. `--sudoers` adds `/etc/sudoers.d/glaeda-mini-fleet`, checked with `visudo -cf` first, allowing only `launchctl kickstart -k system/<job>` for the manifest's launchd jobs. It deliberately leaves out `xcode-select` and `xcodebuild -runFirstLaunch`: `/Applications` is writable by the admin group, so the login user could swap the app behind a path rule, which would make that rule passwordless root. Those stay one-password steps, needed once per Xcode. Run `fix` again afterwards.

### Seed over the LAN

```bash
scripts/glaeda-mini-fleet share xcode --from cmux15 cmux7s-mac-mini cmux8s-mac-mini          # dry run
scripts/glaeda-mini-fleet share xcode --from cmux15 cmux7s-mac-mini cmux8s-mac-mini --yes    # copy, 2 peers at a time
scripts/glaeda-mini-fleet fix cmux7s-mac-mini --from cmux15 --yes                             # fix takes big things from cmux15
```

The minis reach each other's port 22 on their LAN, not over the tailnet, and a LAN copy is far faster than relaying through the operator Mac or downloading on each mini. `share THING --from HOST` copies `xcode` (the declared app), `metal` (an exported Metal toolchain bundle, downloaded once on the source with `xcodebuild -downloadComponent MetalToolchain -exportPath`, imported on the peer with `-importComponent`), `brew` (the Homebrew owner's downloaded bottles, staged for the next `brew install`), `rustup` (toolchains the peer lacks), `candidate` (sha256 checked on both ends), or `python` (the relocatable CPython `fix` installed). This Mac connects to the source with `ssh -A`, lending it this Mac's key for the hop (manifest `lan.auth: "agent"`, the default); `lan.auth: {"identity_file": PATH}` names a fleet-internal key on the source instead. With `-A`, anyone with root on the source can use this Mac's agent while a copy runs (up to three hours for Xcode), and the source trusts each peer's LAN host key on first use; a fleet key limits that exposure to the fleet. One source streams to at most two peers at a time. The peer's address is `hosts.<name>.lan_ip` or its `en0`, and must start with `lan.subnet`. The peer unpacks into a staging directory beside the target and moves it into place only after the copy completes and verifies. It never replaces something already there. `fix --from HOST` takes the Xcode app, Python, the candidate and the Metal toolchain this way.

### Onboard in one command

```bash
scripts/glaeda-mini-fleet onboard                        # dry run for every compile-lane host with a node_id
scripts/glaeda-mini-fleet onboard --from cmux15 --yes    # preflight, fix, sudo-plans, enroll
```

`onboard` runs `preflight`, `fix`, the sudo-plans (`--sudo wait`, the default, prints them and waits for the operator to run `sudo-plan --run`; `--sudo run` runs them itself over `ssh -t`; `--sudo stop` stops those hosts there), `fix` again, and a fresh preflight. Every host still ready is then enrolled in parallel, with a heartbeat line every 30 seconds, on the candidate the manifest pins (`candidate`, which `upgrade` writes; see [Updating the fleet](#updating-the-fleet)). It ends at an eligible enrollment. Runner registration is a hook, `runner_register_command`, that glaeda#1174 fills: for every eligible host without a runner it returns the remote command, and `onboard` mints one org registration token on this Mac (`gh api orgs/<runner.org>/actions/runners/registration-token`) and passes it over stdin, never in argv or a log. Until then the table says the host has no runner. It ends with one table: host, class, node id, generation, step reached, result, log. Hosts come from the manifest: those with a `node_id` whose hardware entry does not set `compile_lane: false`. `never_touch` hosts are refused. `onboard` is `repair` for a host that has no enrollment yet; both run the same code.

Acceptance is proved once per hardware class and Xcode build, not per machine. `hardware.<key>.acceptance` records it (`xcode_build`, `candidate`, the `node` that holds the receipt, and the exported class receipt as `receipt`, a file on the operator Mac, with its `receipt_sha256`). By default (`--acceptance class`) a host joins only when its hardware has an acceptance recorded for the declared Xcode build and the current candidate. With a `receipt` recorded, `onboard` checks the file's `receiptSha256` against the manifest, sends it over ssh stdin to `glaeda-mini-enroll --class-receipt - --class-receipt-sha256 SHA --fleet-class KEY` (`KEY` is that `hardware` key, which the receipt must name), and the host becomes `eligible` on a bootstrap re-check, no 13-minute `accept-local` (see [Class acceptance](#class-acceptance)), then is registered. Without a `receipt` it enrolls with `--no-accept` and stays `enrolling`. `--acceptance node` runs `accept-local` on the host instead. `onboard` exits 1 while any host is not eligible.

### Updating the fleet

A new Glaeda candidate (or drift on a node) is one command from the operator Mac, dry run first:

```bash
scripts/glaeda-mini-fleet upgrade --candidate-run RUN                  # dry run: seeds, steps, the manifest diff
scripts/glaeda-mini-fleet upgrade --candidate-run RUN --yes            # roll it out
scripts/glaeda-mini-fleet repair                                       # dry run: what drifted and what would fix it
scripts/glaeda-mini-fleet repair --yes                                 # fix it
```

`upgrade --candidate-run RUN` takes a `fleet-candidate.yml` run ([Fleet candidate distribution](FLEET_DISTRIBUTION.md)). It refuses a run that is not the dispatched `Fleet candidate bundles` workflow, did not succeed, was built from a commit not on `main`, or whose macOS artifact has expired. With `--yes` it downloads the archive once on this Mac with `gh` and checks it against the `receipt.json` the same build job wrote (source, target, archive name and sha256); `--candidate-sha256 SHA` also pins the digest shown in the run's summary. Then, per `hardware` class:

1. The seed (`--seed-per-class HOST`, else a host of the class already on the candidate, else the first ready one) moves `~/glaeda` to the tip of `main` (it must carry `glaeda-mini-enroll --renew`), stages the candidate, stops its runners once their current job ends, and runs `glaeda-mini-enroll --renew`: quarantine (`stale_glaeda_generation`), bootstrap from the new generation into a private file, `renew-enrollment` and `renew-enrollment-apply`, the one `accept-local`, and `eligible`. Its runners start again.
2. `export-class-acceptance` runs on the seed. The receipt is checked here (digest, class, candidate, accepting node), saved as `class-acceptance/<key>-<source12>.json` beside the manifest (mode 0600), and `hardware.<key>.acceptance` and the manifest's `candidate` pin are written to the manifest file passed with `--manifest`. The diff is printed.
3. Every other host of the class does the same renewal in parallel, adopting that receipt instead of building. A host that does not match is left `quarantined` (`failed_acceptance`) and its row names the differing fields and the fix: make them match and `repair`, or `repair HOST --acceptance node --yes` to accept it on its own.

A class whose seed fails is left as it was: its other hosts keep their current enrollment. A rerun resumes: a class that already records a receipt for this candidate is not seeded again, and hosts already on the candidate are left alone. A host already on the candidate with a current local acceptance can seed its class with the export alone. Hosts that are not enrolled are left to `onboard`, draining hosts to their operator, and hosts preflight does not pass to `repair`.

`repair` runs `preflight`, `fix`, the sudo-plans (printed; `--sudo stop` is its default), `fix` again, then renews every enrolled host that is quarantined, still `enrolling`, not accepted, or on another Glaeda generation than the pinned candidate, re-adopting its class's receipt (`--acceptance node` runs `accept-local` instead). It starts runners a renewal stopped once the node is eligible, and restarts a runner whose launchd agent is loaded but whose listener is not running. Both commands end with one table: host, class, node id, generation (the enrollment's `glaedaGeneration`, old->new), the step reached, the result and the host's log under `~/.local/state/glaeda/mini-fleet/`. Both are dry runs without `--yes`, run hosts in parallel with a heartbeat line every 30 seconds, and refuse `never_touch` hosts.

Every command that changes a host (`fix`, `share`, `sudo-plan --run`, `onboard`, `repair`, `upgrade`) first asks the host whether it is held: another build holds the fleet host lock (`/Users/Shared/cmux-build-fleet/host.lock`, the flock the build worker and the runner hook take), or an active `glaeda-reservation/v1` marker (`reservation.json` beside it) reserves it. An unreadable or invalid marker counts as held. A held host is skipped with `held: reserved by OWNER for PURPOSE until TIME, skipped` (or `held by another build`), and the run exits 1. `--wait` polls it every minute for up to 4 hours instead. Each changing step on the host checks again before it starts, and each sudo-plan script checks before it asks for the password.

Pin the toolchain a class is accepted on. Adoption compares every toolchain observation byte for byte, so `defaults.toolchain.rustup_default` should name a version such as `1.98.1` rather than `stable`, whose next release would make every later adoption fail on `toolchain.rustc`.

Each host names its `hardware`, a key into the manifest's `hardware` table (model, chip, CPUs, memory), which `check` verifies against the machine. Its scheduling `class` (`xl`, `std`, `light`, `dev`, `borrowed`) says what work it takes, and `availability` (`dedicated` or `opportunistic`) says whether it can be counted on. A manifest written before this split, with a top-level `classes` table and `class` naming the hardware, still loads: those become `hardware`, and the host has no scheduling class until it is given one.

`glaeda-mini-fleet pools --json` prints runner capacity per pool label `glaeda-<class>-xcode-<version>`, the label glaeda-cmux-runner issues. A member is declared for a pool when it is dedicated, has the `ci-runner` role and a runner class (`xl`, `std`, `light`); it conforms when the latest observation shows that exact Xcode build as a real directory, meaning a runner installed or relabelled from this manifest would carry the label (GitHub fixes labels at registration, so this is not proof the registered runner has it yet). The PR pool picker counts conforming members. A host with a `class` must also state its `availability`.

### Reserve a host for dedicated work

The fleet host lock (`/Users/Shared/cmux-build-fleet/host.lock`, taken by `with-host-lock`) covers one build at a time, so it leaves gaps between the builds of a longer campaign. A reservation covers the whole campaign:

```bash
scripts/glaeda-mini-fleet reserve cmux15 --for "Chromium build campaign" --hours 8          # dry run
scripts/glaeda-mini-fleet reserve cmux15 --for "Chromium build campaign" --hours 8 --yes
scripts/glaeda-mini-fleet release cmux15 --yes
```

`reserve` writes `/Users/Shared/cmux-build-fleet/reservation.json` on each host as the SSH user (temp file then `mv`, mode 664): `{"schema": "glaeda-reservation/v1", "owner", "purpose", "since", "until"}` with Unix-second times. It is active while now is before `until`, which is at most 72 hours ahead (`--hours N` or `--until EPOCH`), so a forgotten reservation expires on its own. The owner defaults to `$USER@$(hostname -s)` on the operator Mac (`--owner` overrides). Rerunning `reserve` as the same owner extends it and keeps `since`. Another owner's active reservation, or a marker that is not a valid v1 object, is printed and refused unless `--force`. `release` removes your own reservation or any expired one; another owner's active reservation or an invalid marker needs `--force`. Both commands are dry runs without `--yes`. They take a per-host writer lock (`.reservation.lock` beside the marker), write only if the marker is unchanged since they read it, and read it back afterwards. The marker is parsed by `scripts/glaeda_reservation.py`, the same rule the runner's job-started hook applies on the host.

The probe ships the marker's bytes (at most 4 KiB, base64) and reports whether a build holds the host lock right now. For the lock it takes a shared, non-blocking `flock` on a read-only descriptor with `/usr/bin/perl` and drops it at once, so it cannot wait and holds the lock for microseconds; `with-host-lock` and the worker retry, and a one-shot non-blocking taker that lands in that window fails or skips that one attempt. `lsof` would miss a root-owned holder. `check` lists an active reservation under `reserved`, which is informational like `pending`; an expired or invalid marker, or one ending more than 72 hours ahead, is drift, with the `release` command as its fix. In `pools`, a member with an active or invalid marker at observation time is left out of `conforming` and listed under `reserved`, so the pool picker can see why capacity dropped. A member whose host lock was held is listed under `busy` for information only and still conforms: a snapshot of a running job must not shrink capacity, since the picker counts live jobs itself.

Each host may also carry `roles` (`dev-builds`, `ci-runner`, `nightly`, `ios-simulators`, `cache-host`) and the `sudo` mode it is expected to have (`nopasswd` or `password`, observed with `sudo -n -l`, which runs nothing). `controller_token: "present"` requires the fleet worker's controller token file to exist; the probe tests existence only and never reads it.

## Class acceptance

A local acceptance proves that one machine builds `cmux.macos.dev-check@1` under one reviewed Glaeda candidate with one toolchain. Machines of the same hardware class, with the same Xcode build, SDK, toolchain and candidate, differ only in what the per-node bootstrap already checks (disk, cache root, checkout state, power). So the build is proved once per class, and each further node proves only that it is that class.

A class receipt (`glaeda-cmux-fleet-class-acceptance/v1`) records the fleet class (the operator manifest's `hardware` key, such as `m4pro-48` for the `std` minis), the role and profile, architecture and OS version class, the hardware (`hw.model`, chip, memory in GiB), the bootstrap's toolchain observations (Xcode version and build, macOS SDK, zig, rustup, cargo, rustc and the DiffSidecar toolchain, git, Metal, and the pins the cmux checkout sets) with their digest `toolchainGeneration`, the Glaeda binary digest, the fleet contract digest, the Glaeda candidate's source (from the staged generation's `manifest.json`, believed only where it names the running `cmux_fleet.py`, bootstrap and binary), the cmux source and semantic result of the accepting build, the accepting node and the digest of its receipt, and `receiptSha256` over all of that.

Produce it on the node that accepted locally, once that node is `eligible` (read-only; it re-runs the bootstrap and prints the receipt):

```bash
G="$HOME/Projects/glaeda-generations/<first 12 of the candidate source>"
F="$HOME/.config/glaeda/cmux-fleet"
python3.13 -B "$G/scripts/cmux_fleet.py" export-class-acceptance "$F/enrollment.json" \
  --acceptance "$F/acceptance/cmux_macos_native_build.json" --fleet-class m4pro-48 \
  --cmux-root ~/cmux --glaeda "$G/bin/glaeda" --cache-root ~/.cache/glaeda/cmux-native-cache
```

It refuses a receipt that is not current for the enrollment, a class-derived receipt (class acceptance does not chain), a node that is not `eligible`, and a Glaeda run from a checkout rather than a staged candidate. `cmux_fleet.py verify-class-acceptance FILE` re-validates a saved receipt (the file must be mode 0600) and prints it.

On another node, `glaeda-mini-enroll ... --class-receipt FILE|- --class-receipt-sha256 SHA --fleet-class m4pro-48 --apply` bootstraps and enrolls as usual, then runs `cmux_fleet.py adopt-class-acceptance` in place of `accept-local`. Adoption requires an `enrolling` node whose fresh bootstrap passes, whose capability matches its enrollment, whose class receipt digest equals the operator's `SHA` and whose declared class equals the receipt's, and then requires exact equality with the receipt in every one of: architecture, OS version class, `hardware.model`, `hardware.chip`, `hardware.memoryGiB`, each `toolchain.<field>`, `toolchainGeneration`, the Glaeda binary digest, the fleet contract digest and the candidate source. A difference stops with every differing field named, here and in the class; that node needs its own `accept-local`. On a match it emits the node's own receipt (`executionClass: glaeda-class-acceptance/v1`, `classAcceptanceSha256` set, no local attempt), bound to this node id, enrollment generation and bootstrap digest. The transition to `eligible` then records `classAcceptanceSha256` in the enrollment, so each node says which class receipt it relies on; the class receipt itself stays next to it in `class-acceptance/<class>.json`. A class-derived receipt counts only while the enrollment names its class receipt. Leaving quarantine and renewal advance the enrollment generation and drop that record, so the receipt stops counting, exactly like a local receipt.

Why this is safe. The candidate is the same reviewed bytes (binary and fleet tools, checked by digest), so the code that would run is the code that was proved. The toolchain is the same to the byte of every version string the bootstrap reads (including the zig and Rust pins of the cmux checkout), and the hardware is the same model, chip and memory, so the machine is the one the build was proved on. The cmux commit is not compared: a local acceptance also proves the machine at one commit, and later jobs build others. The per-node bootstrap still runs, so everything that differs between two machines of one class (free disk, cache root, a dirty checkout, power) is still checked on each. The digest protects the receipt in transit: `onboard` reads it from the operator's manifest, the host recomputes it over the content and compares it with the digest the operator passed, so a changed receipt is refused, and a receipt for other hardware or another toolchain does not match this node's own observation whatever it says. What this does not defend: receipts and enrollments are private files of the node's own user, who could hand-write a local receipt today too. The class path adds no authority that user lacks; it removes the build, not the checks.

What forces a new class acceptance: a new Glaeda candidate (binary and contract digests change), a different Xcode build or SDK, any changed toolchain observation (a new rustc from `rustup` stable, a zig upgrade, a cmux checkout that pins a different zig minimum or DiffSidecar toolchain), or a new class. Export again after one fresh `accept-local` on one node of the class. Every class that takes jobs gets its own receipt the same way, `light` included (one `accept-local` on one 16 GB mini). Class acceptance is macOS only: the Linux bootstrap reports no hardware identity, so Linux hosts accept locally.

Rolling out: changes to `cmux_fleet.py` and its bootstrap reach a mini only in a new candidate, because a staged generation runs its own copy. A candidate older than this change cannot read an enrollment that carries `classAcceptanceSha256`, so rolling a class-adopted node back to one means enrolling it again. See [Fleet candidate distribution](FLEET_DISTRIBUTION.md) for producing the candidate and pinning it in cmux.

## Enroll a Mac in one command

After `glaeda-mini-setup --apply` and cmux `./scripts/setup.sh`, one command runs every step of [Onboard a Mac](#onboard-a-mac) below. Give it the downloaded [candidate bundle](FLEET_DISTRIBUTION.md) and the checksum and source from the trusted run's receipt:

```bash
scripts/glaeda-mini-enroll --cmux-root ~/Projects/cmux --node-id cmux-mac-001 \
  --candidate ./glaeda-candidate/ARCHIVE.tar.gz --sha256 SHA256 --source COMMIT          # plan
# the same command with --apply does it
```

It stages the archive with this checkout's `fleet_bundle.py` into `~/Projects/glaeda-generations/<first 12 of source>`, then runs bootstrap, enroll, `accept-local`, the transition to `eligible`, and `status`. Every fleet step uses that generation's own binary and tools, so the node runs exactly the reviewed candidate and builds no Rust. Without `--candidate` it builds glaeda from this checkout instead, which is for development hosts only.

It uses the same paths as the manual steps: the enrollment and acceptance receipt under `~/.config/glaeda/cmux-fleet`, and `glaeda-mini-setup`'s cache root. It finds a Python 3.13+ interpreter itself, because `accept-local` needs `os.waitid`. Each step is skipped when its result already holds, so a re-run re-inspects the staged generation instead of restaging it, resumes at `accept-local` after a rejected acceptance, returns a draining node to `eligible`, and only prints status on an eligible node. An interrupted stage is left in place and reported rather than reused. `--reaccept` forces a fresh acceptance. It refuses a quarantined or retired node and a `--node-id` that differs from the existing enrollment. It never uses sudo and never registers a GitHub runner: `scripts/glaeda-cmux-runner --apply` does that ([CMUX_MINI_RUNNER.md](CMUX_MINI_RUNNER.md)), and jobs reach the runner only once routing points at its label (glaeda#1174). cmux `scripts/persistent-compile up` is retired.

## Onboard a Mac

Use an exact reviewed Glaeda checkout and the CMUX checkout that will run acceptance. A [verified native candidate bundle](FLEET_DISTRIBUTION.md) can supply `GLAEDA_BIN` and the matching fleet scripts without building Rust on the node; skip the build/copy step below when using that path.

```bash
set -euo pipefail
./scripts/bootstrap
cargo build --locked --release --bin glaeda

GLAEDA_INSTALL_ROOT="${XDG_DATA_HOME:-$HOME/.local/share}/glaeda/cmux-fleet"
GLAEDA_BIN="$GLAEDA_INSTALL_ROOT/glaeda"
install -d -m 700 "$GLAEDA_INSTALL_ROOT"
if test -f "$GLAEDA_BIN" && ! test -e "$GLAEDA_INSTALL_ROOT/glaeda.rollback"; then
  cp -p "$GLAEDA_BIN" "$GLAEDA_INSTALL_ROOT/glaeda.rollback"
fi
install -m 755 target/release/glaeda "$GLAEDA_INSTALL_ROOT/.glaeda.next"
mv "$GLAEDA_INSTALL_ROOT/.glaeda.next" "$GLAEDA_BIN"

CMUX_ROOT=/absolute/path/to/cmux
CMUX_CACHE_ROOT=/absolute/path/to/cmux-native-cache
(
  cd "$CMUX_ROOT"
  ./scripts/setup.sh
)
# Lets bootstrap describe the toolchain. It does not make it visible to the
# CMUX build, which searches a fixed list of system directories — see the
# workloadToolPath note below before installing rustup only under $HOME.
export PATH="${CARGO_HOME:-$HOME/.cargo}/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"

FLEET_ROOT="${XDG_CONFIG_HOME:-$HOME/.config}/glaeda/cmux-fleet"
umask 077
install -d -m 700 "$FLEET_ROOT" "$FLEET_ROOT/acceptance"
BOOTSTRAP="$(mktemp "$FLEET_ROOT/.bootstrap.XXXXXX")"
chmod 600 "$BOOTSTRAP"
ENROLLMENT="$FLEET_ROOT/enrollment.json"
ACCEPTANCE="$FLEET_ROOT/acceptance/cmux_macos_native_build.json"
NODE_ID=cmux-mac-001

bash scripts/cmux-fleet-bootstrap-macos \
  --cmux-root "$CMUX_ROOT" \
  --glaeda "$GLAEDA_BIN" \
  --cache-root "$CMUX_CACHE_ROOT" \
  --hardware-class cmux-mac-build-large \
  --role cmux_macos_native_build \
  > "$BOOTSTRAP"

ENROLLMENT_NEXT="$(mktemp "$FLEET_ROOT/.enrollment.XXXXXX")"
python3 scripts/cmux_fleet.py enroll "$BOOTSTRAP" \
  --node-id "$NODE_ID" \
  --scope cmux-founders \
  --generation 1 \
  > "$ENROLLMENT_NEXT"
chmod 600 "$ENROLLMENT_NEXT"
mv "$ENROLLMENT_NEXT" "$ENROLLMENT"

ACCEPTANCE_NEXT="$(mktemp "$FLEET_ROOT/acceptance/.cmux_macos_native_build.XXXXXX")"
python3 scripts/cmux_fleet.py accept-local "$ENROLLMENT" \
  --cmux-root "$CMUX_ROOT" \
  --glaeda "$GLAEDA_BIN" \
  --cache-root "$CMUX_CACHE_ROOT" \
  --role cmux_macos_native_build \
  > "$ACCEPTANCE_NEXT"
chmod 600 "$ACCEPTANCE_NEXT"
mv "$ACCEPTANCE_NEXT" "$ACCEPTANCE"

python3 scripts/cmux_fleet.py transition-apply "$ENROLLMENT" --to eligible \
  --acceptance "$ACCEPTANCE"

bash scripts/cmux-fleet status "$ENROLLMENT" \
  --acceptance "$ACCEPTANCE"
```

The macOS preparation reuses CMUX's reviewed `scripts/setup.sh` for prerequisites. Bootstrap re-observes those prerequisites read-only and verifies the exact checkout exposes `cmux.macos.dev-check@1` for the observed Apple-Silicon node. `accept-local` launches CMUX's checked-in profile runner on this node inside a private attempt directory, captures the canonical semantic result, reruns the read-only bootstrap on this same node, and emits `glaeda-cmux-fleet-acceptance/v2`. Both Python front doors execute in isolated interpreter mode (`-I`) with a closed allowlist containing only reviewed toolchain/home path inputs plus a private attempt-local `TMPDIR`; Python import controls, Git redirection variables, SSH agents, credentials, and unrelated operator environment never flow into either child. CMUX still owns the developer-build commands, validator, artifacts, timeout, and pass/fail semantics. Glaeda owns the local-attempt binding, fresh capability check, and durable receipt.

Bootstrap still probes the toolchain through the operator's shell, so a node with a misplaced tool produces a complete receipt rather than a bare error. It separately reports whether the CMUX workload will be able to find those tools: the runner rebuilds PATH as `/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin` plus a per-attempt Cargo home that starts empty, so a build tool reachable only from `$HOME` is one the build cannot spend. `workloadToolPath` fails and `observed.toolsMissingFromWorkloadPath` names them, which blocks enrollment in seconds instead of passing bootstrap and failing the build minutes later. **`rustup`, `cargo` and `rustc` therefore have to be reachable from those six directories, not only from `$CARGO_HOME/bin`** — the `export PATH` above lets bootstrap describe the toolchain, it does not make it visible to the build.

An attempt that does not accept keeps its runner log and semantic result beside the enrollment, renamed to `rejected-attempt.*`, and drops the child's scratch tree. Both steps are best-effort and say so: if the name is already taken the attempt stays under its hidden `.acceptance-run.*` name, and if the scratch tree survives a second line names it. Either way the retained path is printed on stderr, and the receipt is never sacrificed to report it.

Retention is bounded to the three most recent directories Glaeda itself created. Glaeda recognises those by name: exactly eight characters from `a-z`, `0-9` and `_` after the prefix. Anything else you leave under that prefix — an archive, a symlink onto another volume, a directory renamed `rejected-attempt.x9k2mq4p.keep` — is neither counted against the budget nor removed. Avoid a bare eight-character suffix such as a date (`rejected-attempt.20260923`) for evidence you mean to keep: it is indistinguishable from one Glaeda made. Removing a retained attempt by hand is safe at any time. The retained `cmux-runner.log` is the build's raw merged output at mode `0600` inside the `0700` fleet root: operator-private evidence with an indefinite lifetime, not something to attach to an issue unread.

## Onboard Linux

```bash
set -euo pipefail
./scripts/bootstrap
cargo build --locked --release --bin glaeda

GLAEDA_INSTALL_ROOT="${XDG_DATA_HOME:-$HOME/.local/share}/glaeda/cmux-fleet"
GLAEDA_BIN="$GLAEDA_INSTALL_ROOT/glaeda"
install -d -m 700 "$GLAEDA_INSTALL_ROOT"
if test -f "$GLAEDA_BIN" && ! test -e "$GLAEDA_INSTALL_ROOT/glaeda.rollback"; then
  cp -p "$GLAEDA_BIN" "$GLAEDA_INSTALL_ROOT/glaeda.rollback"
fi
install -m 755 target/release/glaeda "$GLAEDA_INSTALL_ROOT/.glaeda.next"
mv "$GLAEDA_INSTALL_ROOT/.glaeda.next" "$GLAEDA_BIN"

CMUX_ROOT=/absolute/path/to/cmux
FLEET_ROOT="${XDG_CONFIG_HOME:-$HOME/.config}/glaeda/cmux-fleet"
umask 077
install -d -m 700 "$FLEET_ROOT" "$FLEET_ROOT/acceptance"
BOOTSTRAP="$(mktemp "$FLEET_ROOT/.bootstrap.XXXXXX")"
chmod 600 "$BOOTSTRAP"
ENROLLMENT="$FLEET_ROOT/enrollment.json"
ACCEPTANCE="$FLEET_ROOT/acceptance/cmux_linux_ci.json"
NODE_ID=cmux-linux-001

bash scripts/cmux-fleet-bootstrap-linux \
  --cmux-root "$CMUX_ROOT" \
  --glaeda "$GLAEDA_BIN" \
  --hardware-class cmux-linux-ci-medium \
  --role cmux_linux_ci \
  > "$BOOTSTRAP"

ENROLLMENT_NEXT="$(mktemp "$FLEET_ROOT/.enrollment.XXXXXX")"
python3 scripts/cmux_fleet.py enroll "$BOOTSTRAP" \
  --node-id "$NODE_ID" \
  --scope cmux-founders \
  --generation 1 \
  > "$ENROLLMENT_NEXT"
chmod 600 "$ENROLLMENT_NEXT"
mv "$ENROLLMENT_NEXT" "$ENROLLMENT"

ACCEPTANCE_NEXT="$(mktemp "$FLEET_ROOT/acceptance/.cmux_linux_ci.XXXXXX")"
python3 scripts/cmux_fleet.py accept-local "$ENROLLMENT" \
  --cmux-root "$CMUX_ROOT" \
  --glaeda "$GLAEDA_BIN" \
  --role cmux_linux_ci \
  > "$ACCEPTANCE_NEXT"
chmod 600 "$ACCEPTANCE_NEXT"
mv "$ACCEPTANCE_NEXT" "$ACCEPTANCE"

python3 scripts/cmux_fleet.py transition-apply "$ENROLLMENT" --to eligible \
  --acceptance "$ACCEPTANCE"

bash scripts/cmux-fleet status "$ENROLLMENT" \
  --acceptance "$ACCEPTANCE"
```

The Linux `accept-local` path runs CMUX's canonical `cmux.ci.guard@1` profile against the exact local commit/tree in cold state, then re-observes this same host. Workload tool visibility and rejected-attempt retention work exactly as described for macOS above; on Linux the tools checked are `git` and `python3`. A v2 receipt becomes accepted only when CMUX reports `passed`, process settlement is complete, and the post-run capability, Glaeda generation, role profile, and selected toolchain generation still match enrollment.

After either onboarding path, remove only the transient evidence files:

```bash
rm -f "$BOOTSTRAP"
```

The canonical enrollment and finalized role receipts stay under `$FLEET_ROOT` across reboot. They contain no credentials or project secrets.

## Drain, quarantine, recover, and retire

Drain before planned operator work:

```bash
python3 scripts/cmux_fleet.py transition-apply "$ENROLLMENT" --to draining
bash scripts/cmux-fleet status "$ENROLLMENT" --acceptance "$ACCEPTANCE"
```

Return a drained node to candidate eligibility only with its still-current acceptance receipt:

```bash
python3 scripts/cmux_fleet.py transition-apply "$ENROLLMENT" --to eligible \
  --acceptance "$ACCEPTANCE"
bash scripts/cmux-fleet status "$ENROLLMENT" --acceptance "$ACCEPTANCE"
```

Quarantine on a concrete reviewed reason:

```bash
python3 scripts/cmux_fleet.py transition-apply "$ENROLLMENT" \
  --to quarantined --reason toolchain_mismatch
```

After a toolchain, OS, hardware class, or Glaeda update, quarantine the enrollment
and rerun the platform bootstrap above into a private mode-0600 `$BOOTSTRAP`
report. Renew from that report instead of hand-editing the enrollment or choosing
a generation number:

```bash
python3 scripts/cmux_fleet.py renew-enrollment "$ENROLLMENT" "$BOOTSTRAP"
# Inspect replacement capabilities and copy planSha256 from the preview.
python3 scripts/cmux_fleet.py renew-enrollment-apply "$ENROLLMENT" "$BOOTSTRAP" \
  --expected-plan-sha256 sha256:REPLACE_WITH_PREVIEW_DIGEST
```

The preview has no side effects. Apply re-reads both private documents under the
existing enrollment mutation lock and refuses if either differs from the preview.
It preserves node ID and fleet scope, refreshes capabilities from bootstrap,
advances the enrollment generation once, and atomically publishes `enrolling`.
Old receipts cannot promote this generation; rerun `accept-local` and then
`transition-apply --to eligible` using the new receipt. A repeated apply refuses
without advancing the generation again. Active, draining, and retired enrollments
cannot use renewal.

Bootstrap is observation-only: renewal does not certify report freshness or grant
execution ownership. Fresh local acceptance still re-observes the machine. The
caller must first stop new work through its actual scheduler and settle existing
work before changing software; quarantine alone does not drain that scheduler.
If the candidate fails, quarantine its enrollment, restore the reviewed previous software,
rerun bootstrap and renew again before acceptance. The old receipt is never a
rollback shortcut.

Rollback an onboarding before candidate promotion, or retire an active node, by preserving the record in the terminal `retired` state:

```bash
python3 scripts/cmux_fleet.py transition-apply "$ENROLLMENT" --to retired
bash scripts/cmux-fleet status "$ENROLLMENT"
```

This leaves an auditable local tombstone and zero candidate-eligible roles.

## Operator-owned prerequisite changes

The bootstrap commands deliberately avoid installing packages, changing SSH/Tailscale, enrolling MDM, editing GitHub runner registration, or changing service ownership. Use the machine-management system that already owns those changes, then rerun bootstrap.

For a Mac used unattended without an existing MDM power profile, this reference change is reversible:

```bash
OLD_AC_SLEEP="$(pmset -g custom | awk '/AC Power:/{on=1;next}/Power:/{on=0} on && $1=="sleep"{print $2; exit}')"
test -n "$OLD_AC_SLEEP"
sudo pmset -c sleep 0
# rollback
sudo pmset -c sleep "$OLD_AC_SLEEP"
```

For Glaeda distribution, the bootstrap consumes an exact executable path and records its SHA-256 generation. The examples above install the reviewed repository build into a stable per-user CMUX-fleet location with an atomic rename. The first update attempt preserves the previously installed binary as `glaeda.rollback`; later retries leave that copy untouched until the candidate enrollment generation is accepted. A release package, MDM payload, or configuration manager may supply the same exact file instead.

Rollback a just-installed repository build before re-enrollment with:

```bash
test -f "$GLAEDA_INSTALL_ROOT/glaeda.rollback"
mv "$GLAEDA_INSTALL_ROOT/glaeda.rollback" "$GLAEDA_BIN"
```

After any install, update, or rollback, rerun bootstrap and advance the enrollment generation before candidate eligibility resumes. Once the new generation is accepted, remove the one-step rollback copy:

```bash
rm -f "$GLAEDA_INSTALL_ROOT/glaeda.rollback"
```

## CI and synthetic evidence

`accept-local` compares the returned CMUX source identity with the exact
repository/commit/tree passed to the workload, then re-observes HEAD commit/tree
after post-run bootstrap before issuing acceptance. A self-consistent result for
another source or checkout movement refuses acceptance. These boundary checks
do not freeze the checkout; callers must still coordinate exclusive source use.

Hosted CI needs no physical CMUX machine. Glaeda CI runs the enrollment/bootstrap contract suites and synthetic CMUX semantic-result fixtures. CMUX CI owns the workload-profile contract and executes `cmux.ci.guard@1` through its canonical runner. Physical Mac/Linux acceptance receipts are attached only after the corresponding CMUX-controlled hosts exist and have been explicitly approved for the profile.

Related Glaeda work: #743, #546, #365, #492, #970, #1048, #1008, #1010, #1071.
Related CMUX work: manaflow-ai/cmux#13091, #13095, #13198, #13325, #13411.
