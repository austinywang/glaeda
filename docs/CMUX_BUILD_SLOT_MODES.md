# cmux build slots: catch-up mode and iteration mode

Status: design, from measurements on the Manaflow minis (cmuxterm-hq#573 slice 5, #1134).
Evidence: [mini to mini](experiments/fleet-compilation-cache-minis-2026-09-24.md),
[prefetch and floor](experiments/fleet-compilation-cache-prefetch-2026-09-24.md).

## Why two modes

On Xcode 26.3, the compilation cache and incremental builds did not mix for the cmux app target
(26.6 evidence below says otherwise):

| Build (full `cmux` app, M4 Pro) | Caching on | Caching off |
| --- | ---: | ---: |
| fresh DerivedData, fleet store warm | 90 to 130 s | 710 s (cold) |
| one-line app-target edit, warm DerivedData | 451 s (whole target recompiles) | 125 to 156 s (323 files recompiled) |
| no-op, warm DerivedData | | 13 to 14 s |
| flip caching on / off in the same DerivedData | 606 s (full rebuild) | 755 s (full rebuild) |

The caching-off column and the flip row are from cmux8s at the fleet pin, Xcode 26.6
(17F113); the caching-on column is from the minis on Xcode 26.3, with a different edit, so the
two edit cells are not a strict pair. The edit was a new
file-scope declaration (`private let`) in one app file (`WorkspaceTodoState.swift`), which
recompiled 323 of the app target's 2,655 Swift files (Xcode 26.6 logs each compile twice, 646
lines); its revert took 125 s. An edit inside a function body recompiles
less: on the pin with caching on it compiled 1 app file (canary run 36081880621, below). The fresh-build row assumes the fleet store already holds that commit. With caching on (Xcode 26.3),
every compile job's key covered the whole module, so any edit missed every job in the app
target (but see the 26.6 evidence below).

Turning caching off for the app target alone (a per-target macro) while packages stay cached
is not adopted for now: one fresh build that way (Xcode 26.3, against a 26.3 store) lost the
package hits (76 against about 1,535) and took 802 s. The cause was not diagnosed (the macro
may reach package compiles), and the warm edit and no-op runs that would decide the question
were invalidated by an Xcode switch mid-run, so this stays open (Next, item 1).

Later evidence on the pin reopens the first conclusion; treat both as early data points, not
rules. cmux canary run 36081880621 (Xcode 26.6 17F113, 12vcpu, plain
`COMPILATION_CACHE_ENABLE_CACHING=YES`) did not reproduce the whole-target rebuild. A
function-body app edit compiled 1 app file (2 log lines), and a new top-level `private func`
compiled about 4 (8 lines). A second series, with a per-target macro like cmux#14349's and the app
cache still on, gave the same two counts; there, a new internal function used across the app
fanned out (1,078 lines), likely ordinary dependency spread (no caching-off comparison exists).
The 451 s cell (a different edit, on the minis) and the 802 s trial were both on Xcode
26.3, and in the per-target-macro series 26.3 recompiled the whole app target on the body-only
edit where 26.6 compiled 1 file (2,609 tasks, 5,218 compile lines, run 36035657899).

The same work found a separate cache problem: under the cache the `cmuxTests` driver rewrote
its chained bridging header on every build, invalidating all 1,073 test inputs. cmux CI now
turns the cache off for `cmuxTests` only (not the app target) with a per-target macro,
`COMPILATION_CACHE_ENABLE_CACHING=$(CMUX_CI_COMPILATION_CACHE_$(TARGET_NAME):default=YES)`
(manaflow-ai/cmux#14349). A one-test-file edit spent 139 s in `cmuxTests` with the cache on and
31 s with it off (44 s for the whole rebuild). The first admission with the macro (job
107918674068) recompiled only `cmuxTests` (1 driver, 43 batched compiles) and no package or app
files, but that was a seeded incremental build. Package hits on a fresh cache-fed build with
the macro are still unmeasured, so the second conclusion stays open.

On 26.6, flipping caching in one DerivedData still rebuilds everything (606 s and 755 s), so one
DerivedData cannot serve both modes. Whether iteration mode needs caching off at all on 26.6 is
now open (Next, item 1), since cache-on edits stayed incremental there. Until that is measured,
a slot keeps two DerivedData directories and picks per job:

- **Catch-up mode (caching on):** a fresh machine, a slot far behind main, or a one-shot build
  of a commit (CI, a PR product). Fed by the fleet store: about 100 s from nothing.
- **Iteration mode (caching off):** edit loops on a slot that is already warm at, or near, the
  commit being edited. 125 to 156 s for an app edit that adds a declaration, 13 to 14 s no-op
  (Xcode 26.6); well under a minute needs a smaller app module (manaflow-ai/cmux#13108).

A **slot** is one checkout plus its iteration DerivedData, leased to one user or agent at a
time. Slots on a host never build at the same time: every build takes the host lock.

## Paths

| What | Path | Same path on every machine? |
| --- | --- | --- |
| catch-up DerivedData | `/Users/Shared/cmux-build-fleet/xcode/DerivedData` | required: part of every cache key |
| local CAS | `/Users/Shared/cmux-build-fleet/xcode/cas` | required: part of the app target's keys |
| node socket | `/Users/Shared/cmux-build-fleet/xcode/fleet-cas.sock` | per host |
| node store | `/Users/Shared/cmux-build-fleet/xcode/node-store` | per host |
| catch-up checkout | `/Users/Shared/cmux-build-fleet/xcode/src/cmux` | required: part of the keys of every target in the checkout |
| iteration DerivedData | `/Users/Shared/cmux-build-fleet/xcode/slot-<n>/iter/DerivedData` | no: never touches the cache |

The checkout path enters the keys despite project prefix mapping: a fresh full-app read on
cmux8s (Xcode 26.6) from a copy of the writer's checkout at another path hit 334 of 4,192,
the SPM packages, whose clone directory kept its path. So catch-up builds use one fixed
checkout per host (under the host lock), not the slot's own checkout, and SPM clones stay
inside the fixed DerivedData.

The catch-up DerivedData is one path per host, not per slot, because its path is in every key
(mapping it crashes swift-frontend on Xcode 26.3 and 26.6). The host lock already serializes
builds, so one catch-up build per host at a time is not a new limit. Iteration directories can
be per slot because nothing is shared from them.

Both path rules hold on the fleet pin, Xcode 26.6 (17F113), measured on cmux8s:

- the `CmuxSettingsUI` chain with the DerivedData mapping still aborts with
  `bad_optional_access`;
- a fresh full-app build against a filled local CAS at the same path hit 4,192 of 4,192 in
  102 s, while the same CAS cloned to a different path hit 1,535 of 4,192 and took 600 s (the
  whole app target missed). Xcode 26.6 reports 4,192 cacheable tasks where 26.3 reported
  4,191, so counts are not comparable across versions.

Catch-up DerivedData lifecycle, since one directory serves every slot on the host:

- emptied at the start of each catch-up build (a kept one would turn the next edit into the
  451 s whole-target rebuild on Xcode 26.3, and it is not where anyone iterates);
- the job copies its products out before releasing the host lock, because the next catch-up
  build on the host empties the directory;
- the local CAS is kept between builds (the 102 s result depends on a filled one), except on
  the writer, whose builds start from an empty one (Writer, below). It is Xcode's own on-disk CAS, which the node daemon does not manage, so its size budget comes
  from a separate pruner; the daemon's `node-store` gets its own eviction (both not built yet).

Contract. The host check (`glaeda-mini-fleet check`) can verify:

- the selected Xcode build (`xcodebuild -version`, today 26.6 17F113) is the fleet pin, since
  entries are only shared between identical toolchains (exists: the check verifies the pin);
- the paths above are real directories (not symlinks, not under `/tmp`), owned by the build
  user (planned, once the manifest declares them).

The worker recipe must enforce, since a host check cannot see a job's settings:

- catch-up builds set `COMPILATION_CACHE_ENABLE_CACHING=YES`, the CAS path above,
  `SWIFT/CLANG_ENABLE_PREFIX_MAPPING=YES` and `SWIFT/CLANG_ENABLE_PROJECT_PREFIX_MAPPING=YES`,
  and no DerivedData mapping (planned);
- the two plugin settings come only from `fleet-cas-settings.sh <socket>`, which prints them
  only when the node answers (a dead socket makes a build crawl instead of failing; the script
  exists, the recipe change is planned);
- entries from different Xcode builds are kept apart. One store serves every build: a 26.6
  build against a 26.3 store compiled everything, consistent with the compiler being in the
  key (the key inputs were not inspected). A per-build segment is planned only for eviction.

## Writer

CI's main build is the only writer (#1134 M3; tested in
[signed writes](experiments/fleet-compilation-cache-signed-writes-2026-09-24.md)):

- It runs on one dedicated writer mini whose node daemon holds the signing key and signs
  every index entry (`glaeda-fleet-cas writer`). The store keeps, and every node uses, only
  entries signed by a trusted key (`--trusted-keys`); objects need no signature.
- The main build runs under `xcode/bin/fleet-cas-writer-build.sh cmux <sha> -- <build>`. It
  publishes the signed marker `cmux/<sha>/<Xcode build>` only after a successful build that
  used the node, with no write error, no failed or skipped fleet-store call and no node
  restart, and with the node still up afterwards; otherwise it exits 4 and later main builds
  fill the rest. It empties the local CAS (`xcode/cas`) first, so every lookup reaches the node (an entry answered from the
  local CAS would never be uploaded again; whether Xcode does that is unverified).
- The writer mini runs no PR or other untrusted job. Any process running as the build user
  there can read the key, so a PR job on that host could sign a poisoned entry. It leaves the
  PR pools (a reservation or a manifest role without `ci-runner`) before its key exists.

Key custody (proposed): a 0600 file on the writer mini at
`/Users/cmux/.config/glaeda/fleet-cas-writer.key`, created there by Leo:

```sh
ssh <writer> 'mkdir -m 700 -p ~/.config/glaeda &&
  /Users/Shared/cmux-build-fleet/xcode/bin/fleet-cas keygen ~/.config/glaeda/fleet-cas-writer.key'
```

The command prints the public key, which is all the rollout needs (`--trusted-keys`). This
design never copies the private key anywhere: not into a repository, a log or a job's
environment. Anything running as the build user on the writer could still read it and send
it elsewhere, which is why that host runs main builds only.
The alternatives were considered and not proposed:

- A GitHub environment secret for a main-only job restricts which workflow gets the key, but
  the signer is the long-running node daemon, not the job, so a job would have to write the
  secret to disk on the mini anyway, as the same user every other job on that host runs as.
- The login keychain adds an unlock step for a LaunchAgent and no protection from same-user
  processes.

The step that would add real isolation is running the writer's node as its own user, so the
build user cannot read the key; it matters only if the writer host ever runs anything but main
builds. Rotation starts the store over: create the new key, empty the fleet store and the
writer's node store, and roll out with only the new key trusted. Nodes treat entries signed
by the old key as misses and replace them with verified fetches, and the store refills from
the next main builds. A gradual rotation with both keys trusted would leave old-key entries
behind new-key markers, and those markers would claim commits the store stops serving once
the old key is dropped.

## Deployment

`scripts/glaeda-fleet-cas-rollout --store HOST NODE_HOST... [--apply]` from an operator Mac
deploys both services (plan by default). On each host it runs `scripts/glaeda-fleet-cas`,
which builds the prototype and installs launchd services (a LaunchAgent, or a LaunchDaemon
run as the build user where root has installed its plist from `xcode/launchd/`). Each runs
`xcode/bin/fleet-cas-run ROLE`, which reads its arguments from `xcode/run/ROLE.args`, so an
upgrade needs no root: the installer rewrites the arguments or binary and stops the process,
and launchd starts it again.

- `com.teamleaderleo.glaeda.fleet-cas-store` on the store host: the fleet store on the host's
  tailnet address, port 7450, store under `xcode/fleet-store`. Only `--writers` addresses may
  write (checked per request against the TCP peer address); none by default, so a new store
  is read-only until the trusted writer exists.
- `com.teamleaderleo.glaeda.fleet-cas-node` on every build host: the node daemon on the socket
  above, read-only (`--read-only-kv`) unless it is the writer, and
  `xcode/bin/fleet-cas-settings.sh`.

Addresses are tailnet addresses because macOS Local Network privacy blocks launchd-started
third-party binaries on these headless minis from other hosts' LAN addresses, as
LaunchAgents and as LaunchDaemons alike: the node on cmux8s got "No route to host" for the
store on cmux7s's LAN address while the same binary run from ssh reached it. The same
binary started by launchd reaches cmux7s's tailnet address (verified on cmux8s with
`launchctl submit`), and the tailnet grants the store port to the build minis
(cmuxterm-hq#591). Every client then connects from its own tailnet address, so the
`--writers` check sees real peers. An ssh relay was built and dropped: a port-forward key
also allowed Unix-socket and remote forwards (`permitopen` limits TCP forwards only), for
example to the writer's signing socket, and the tailnet makes it unnecessary.

`--writer HOST --sign-key PATH --trusted-keys HEX` makes that host the writer (above), and
its tailnet address the store's only allowed writer. The writer runs on an hq-only builder
with no PR runners: any process on the writer host can reach the signing node's socket.
`--trusted-keys` alone makes every node and the store use only signed entries. A
LaunchAgent needs the build user's GUI session; the fleet minis log in automatically.
`glaeda-fleet-cas uninstall --apply` removes the services and keeps the stores (a
LaunchDaemon's plist needs root to remove; the command prints how). Deployed on cmux7s
(store and writer) and cmux8s (node), 2026-09-24.

## When to switch

Catch-up is only fast when the store already holds the target commit; on a store miss it
is a cold build (756 s on Xcode 26.3 minis; a full caching-on rebuild took 606 s on 26.6), slower than an incremental caching-off rebuild. So the
worker asks first: the writer records a marker per commit it has filled, and catch-up is
chosen only when `xcode/bin/fleet-cas-marker.sh cmux <sha>` exits 0 (the signed marker for
that commit and this host's Xcode build exists). Before building, the worker then runs
`xcode/bin/fleet-cas-warm.sh cmux <sha>` under a timeout: it copies the whole fill into the
node, so the build's lookups are local hits. On any other exit (1: no manifest, 2: something
did not verify, 3: incomplete) or a timeout, fall back to a slot build. Unwarmed, each lookup is two
serial fleet-store round trips, and a full catch-up read took 1631 s on 09-25. Between builds,
each reader's prewarm agent copies the writer's newest fill (the `latest` marker) into the node,
so the in-build warm mostly confirms what is already local. The rows below are read
top to bottom, first match wins, and the writer is exempt: CI's main build always runs catch-up
with write-through, since filling the store is its job. A PR commit counts as held when its
merge base with main has a marker (its own changes are few, and they miss either way).

| Situation | Mode |
| --- | --- |
| CI main build (the writer) | catch-up, writing |
| store lacks the target commit | iteration (incremental from wherever the slot is) |
| no warm iteration DerivedData for this slot, store has the commit | catch-up for the first product; warm iteration behind it |
| iteration DerivedData at main, change is app-target only | iteration |
| main moved and touched a low-level package (the app would recompile anyway) | catch-up for the product, iteration rewarms behind it |
| CI or a PR product build, nobody iterating | catch-up |

"Behind it" means a background build in the slot's iteration DerivedData at the same commit,
started once the catch-up product is delivered. It takes the host lock like any job, and a
`flock` does not preempt, so a foreground job must be able to cancel it: the warmer registers
its xcodebuild process, the foreground job stops it and requeues the warm (planned). An
build stopped during planning left DerivedData usable: on the pin, a build stopped after 25 s
was followed by an ordinary incremental one (143 s, the same 323 files as the uncancelled
edit). That stop landed during planning; a stop in the middle of compiling is still
unmeasured. Because every slot shares the
host lock, an iteration edit can also wait behind another slot's foreground build, about 100 s
for a catch-up; edit latency includes that wait.

## Warming the iteration DerivedData

It cannot be derived from the catch-up DerivedData: caching changes every compile job's command
line, so flipping a DerivedData between the modes rebuilds everything, in both directions
(on the pin: 606 s turning caching on, 755 s turning it off). The iteration
DerivedData is therefore warmed by its own caching-off builds:

- an idle warmer rebuilds a slot's iteration DerivedData at main's tip, but only a slot with
  no active lease and a clean tree; it never moves a checkout that has local changes or is on
  someone's branch;
- main moves about 1.5 commits an hour; the warm-slots campaign (Air Blue, Xcode 27) measured
  36 s for a new tag into a warm pair and 620 s when a low-level package changed. How often a
  main commit touches a low-level package has not been counted, so the warmer's average cost
  is unknown;
- warming a slot from nothing is a cold caching-off build (710 s on a mini at the pin), so a host
  warms its slots one at a time, idle only, and a new slot is usable in catch-up mode before
  its iteration DerivedData is ready;
- the source checkout per slot stays at a fixed path, so incremental state stays valid.

## Next

1. On the pin: a cancellation in the middle of compiling, the mixed-mode edit and no-op runs,
   and package hits on a fresh cache-fed build with the cmux#14349 macro applied to the app
   target. Optionally rerun the `private let` edit with caching on, to pair with the 323-file
   caching-off result. Then decide whether iteration mode still needs caching off on 26.6.
2. Cut the non-compiler work a catch-up build still does. Summed task time, not wall time:
   SwiftDriver planning and scanning 163 s, script phases 18 s (Rust diff sidecar, nucleo FFI,
   wireguard-go), App Intents extraction 16 s over 89 tasks. Script phases can be cached by
   their inputs and App Intents extraction skipped for modules without intents; planning is
   the largest item and has no lever yet. How far below the 77 to 86 s warm-node floor this
   gets has to be measured.
3. Worker and recipe change in cmuxterm-hq build-fleet: move catch-up builds from per-job
   `job-volumes` DerivedData to the fixed path, add the iteration directories, run the main
   build under `fleet-cas-writer-build.sh` on the writer mini, check `fleet-cas-marker.sh`
   before catch-up, and add warmer cancellation.
