# Fleet-shared Xcode compilation cache: mini to mini

Status: **holds on the full app.** Two Manaflow build minis, Xcode 26.3, the full `cmux` scheme:
a fresh build whose only warm source is a fleet store on the other mini, across the LAN, takes
**132 s** against **756 s** cold (5.7x), with 4,191 of 4,191 cacheable tasks hitting and every
compiler output byte-identical to the build that filled the store. Getting there took two
cache-key fixes that apply to any machine sharing entries: a fixed DerivedData path and a fixed
local CAS path. The main runs had the filling and reading builds on the same mini (the other
was busy); a later cross-machine read (cmux7s reading from a store cmux8s filled) also hit
4,191 of 4,191, in 127 s, with every Xcode-cached output byte-identical across the two
machines (see Cross-machine read). Follow-up to
[the 2026-09-23 Air Blue measurement](fleet-compilation-cache-2026-09-23.md); design and plan
in #1134.

## Setup

- Two Manaflow build minis: cmux7s-mac-mini and cmux8s-mac-mini, Apple M4 Pro (14 cores),
  48 GiB, macOS 26.5.1, Xcode 26.3 (17C529, Swift 6.2.4).
- Network: same office LAN, **0.54 ms** RTT (ICMP, 50 samples). Tailscale is direct over the
  same LAN (2.1 ms ICMP), but Manaflow's tailnet ACL blocks TCP between the minis (port 22
  and the cache port both time out), so all cache traffic used LAN addresses.
- Source: cmux `2ae26d1c7dff6a91104d23258b395f3b1b6940e9`, the Air Blue commit. Chain: the
  `CmuxSettingsUI` scheme from a `git archive` of `Packages/`. Full app: a fresh clone with all
  submodules, the prebuilt GhosttyKit, the pinned DiffSidecar Rust toolchain built once, and
  packages resolved once into a fixed directory (`-disableAutomaticPackageResolution`).
- Everything under `$HOME/.cache/fleet-cas` (never a symlinked path), at the **same absolute
  paths on both minis**. Each build held the fleet host lock
  ([`with-fleet-lock.pl`](../../tools/fleet-cas-prototype/scripts/with-fleet-lock.pl)), so the
  launchd build worker could not start a job during a measurement.
- Topology (the M2 tier design from #1134, prototype in
  [`tools/fleet-cas-prototype`](../../tools/fleet-cas-prototype)):
  - fleet store: `fleet-cas tcp:<lan-ip>:<port> <store>`;
  - writer: a node daemon on its own unix socket, `--upstream` the fleet store, forwarding
    every write before acknowledging it;
  - reader: a new, empty node daemon per run with `--read-only-kv`, whose only warm source is
    the fleet store ([`reader-runs.sh`](../../tools/fleet-cas-prototype/scripts/reader-runs.sh)).
    A last run keeps the node's store, to separate network cost from everything else.
- Which mini did what: chain with per-run CAS paths, cmux7s wrote (store on cmux7s) and
  cmux8s read; chain with the fixed CAS path, cmux8s wrote to a store on cmux7s and cmux8s read.
  A Chromium fleet job occupied cmux7s from about 10:52 UTC onward (load 20 to 60), so for the
  full app cmux8s wrote, the store was copied to cmux7s and served from there, and cmux8s read
  with an emptied DerivedData, local CAS and node store. The serving side was loaded, which makes
  the reader numbers pessimistic, not flattering.

## Results

### `CmuxSettingsUI` chain (5 modules, ~700 Swift compile steps)

| Run | Machine | Wall | Xcode hits / cacheable |
| --- | --- | ---: | ---: |
| cold, caching off | 7s / 8s | 17.6, 19.8 / 15.1, 18.7 s | n/a |
| caching on, local CAS only | 7s / 8s | 17.2 / 16.4 s | 0 / 149 |
| fill, store on the same mini | 7s | 16.2 s | 0 / 149 |
| fill, store on the other mini | 8s | 22.1 s | 0 / 149 |
| fresh reader, per-run CAS path | 8s | 8.6, 6.6, 6.6 s | 134 / 149 |
| **fresh reader, fixed CAS path** | 8s | **6.6, 6.5 s** | **149 / 149** |
| warm node (nothing fetched) | 8s | 5.6 s | 149 / 149 |
| fleet store down, before backoff | 8s | 45.4 s | 0 / 149 |
| fleet store down, with backoff | 8s | 18.9 s | 0 / 149 |

The fill moved 184 MB (1,665 objects, 711 index entries). A fresh reader fetches 170 MB.

### Full `cmux` app

| Run | Wall | Xcode hits / cacheable | Notes |
| --- | ---: | ---: | --- |
| cold, caching off | 756.5 s | n/a | |
| fill | 703.9, 784.8 s | 0 / 4,191 | 2.26 GB uploaded; store 1.4 GiB on disk, 17,803 objects, 7,705 entries |
| fresh reader, per-run CAS path | 656.7, 669.3 s | 1,534 / 4,191 | the app target missed |
| warm node, per-run CAS path | 618.8 s | 1,534 / 4,191 | |
| **fresh reader, fixed CAS path** | **132.2 s** | **4,191 / 4,191** | 1.47 GB fetched, 0 errors |
| warm node, fixed CAS path | 79.9 s | 4,191 / 4,191 | nothing fetched |

No stall: socket mode held on the full graph (Tuist's reason for dropping it, see the prior-art
notes, did not reproduce here), and every build succeeded.

### Against the Air Blue numbers

| | Air Blue (M5, Xcode 27) | Minis (M4 Pro, Xcode 26.3) |
| --- | ---: | ---: |
| chain cold | 118 s | 15 to 20 s |
| chain fresh reader | 30 to 33 s (same host) | 6.5 s (across the LAN) |
| full app cold | 694 to 953 s | 756 s |
| full app fresh reader | not measured | 132 s |

The minis compile this chain about 6x faster than Air Blue did, so the chain's absolute
saving shrinks from 88 s to about 10 s. The full app is where the cache matters: 624 s saved
per fresh machine.

### Correctness

- **Byte-identical outputs.** Chain, writer cmux7s and reader cmux8s: all 669 `.o`,
  `.swiftmodule`, `.swiftdoc` and ABI files identical. Full app (same mini, see Setup): 7,631 of 7,633 `.o`, `.swiftmodule`, `.pcm`,
  `.a` and debug-dylib files identical; the two that differ are the Go-built WireGuard
  library (`libwg-go.a`), which a script phase builds and Xcode never caches. With fixed paths
  even the linked per-product objects match on the same host (they differed on Air Blue, where
paths were mapped).
- **Verified fetches.** The node recomputes every fetched object's ID and treats a mismatch
  as a miss; no mismatches occurred.
- **Read-only reader.** Every reader-side index write was refused (15 on the chain with the old
  CAS path, 2,657 on the full app), and nothing reached the fleet store.
- **Publication order.** Write-through orders it by design: the writer node forwards each object
  and gets the fleet store's answer before Xcode sends the index entry that names it. After the
  fill, an offline walk of the fleet store found all 7,705 entries' 25,031 references present
  (end state, not arrival order; see the [prefetch follow-up](fleet-compilation-cache-prefetch-2026-09-24.md)).
  An earlier version of this doc cited the `kv_put_dangling` counter; it only looked at
  top-level values and saw no IDs, so its zero proved nothing.

### Cross-machine read

Run after the Chromium job released cmux7s, both minis pinned to Xcode 26.3 with
`DEVELOPER_DIR=/Applications/Xcode.app` (provisioning had switched `xcode-select` to 26.6
between 15:16 and 15:31 UTC; one unpinned attempt compiled everything, since the store's keys
were 26.3's). cmux8s filled the store and served it; cmux7s read with an empty node.

| Run | Machine | Wall | Xcode hits / cacheable |
| --- | --- | ---: | ---: |
| fresh reader, store on the other mini | cmux7s | 127.4 s | 4,191 / 4,191 |
| warm node | cmux7s | 77.3 s | 4,191 / 4,191 |
| fresh reader, same-host reference | cmux8s | 107.8 s | 4,191 / 4,191 |

Both nodes ran the prototype at 7af907c (closure prefetch on). cmux7s started the fresh run at
load 27, still settling from the Chromium job, so its wall time is likely pessimistic.
Outputs: 7,631 of 7,634 compared `.o`, `.swiftmodule`, `.pcm`, `.a` and debug-dylib files are
identical between the two machines (one more file than the same-host comparison, the
architecture-specific `libwg-go-arm64.a` copy). The three that differ are the script-built Go
WireGuard library (two copies) and the app's debug dylib; none of them goes through the
compilation cache. Why the dylib differs across machines was not examined (on the same host it
matched even though the WireGuard library differed, so it is probably a host-specific link
input). Everything Xcode caches matched.

A related rule for the fleet: members should share entries only when they build with the same
Xcode build number. Consistent with that, the unpinned 26.6 attempt against the 26.3 store
compiled everything; the key inputs themselves were not inspected. The store should be segmented or keyed by
it, and the fleet check should treat the selected Xcode as part of the cache contract.

## Findings

1. **The bridging-header PCH key includes the local CAS path.** On the full app, two builds
   with identical sources and DerivedData paths but different `COMPILATION_CACHE_CAS_PATH`
   produced byte-identical bridging-header PCHs (`SwiftGeneratePch`, same output CAS ID) under
   different cache keys. Every Swift compile in the target carries that key as
   `-bridging-header-pch-key`, so the whole `cmux` target (2,657 tasks) missed, and not
   reproducibly: two reader runs shared no miss keys with each other or with the fill. The only
   difference in the target's arguments was the CAS path. With one fixed absolute CAS path
   (emptied per run), the app target hit completely, and the chain's 15 leftover
   misses disappeared too. Rule: every machine and job that shares cache entries uses the same
   `COMPILATION_CACHE_CAS_PATH`. Package-only measurements hide this, since packages have no
   bridging header. Posted on manaflow-ai/cmux#13514.
2. **On Xcode 26.3, mapping DerivedData crashes swift-frontend.**
   `SWIFT_OTHER_PREFIX_MAPPINGS=<DerivedData>=/^derived` plus a Swift 6 Sendable warning
   (`ComputerAccessMenuItems.swift`, "converting non-Sendable function value to
   '@isolated(any) @Sendable'") aborts a *first* compile with `bad_optional_access` in
   `CachingDiagnosticsProcessor` → `FileSpecificDiagnosticConsumer::handleDiagnostic`. Isolation
   on the chain: local CAS only, no plugin, still crashes; without the DerivedData mapping it
   passes; project mapping alone passes. It is not the Xcode 27 replay crash
   (swiftlang/swift#92545: a replay, symlinked paths only), and Xcode 27 builds the same chain
   with the mapping. Rule for 26.x: a fixed DerivedData path shared by every machine instead of
   mapping it ([`xcode-cache-build.sh`](../../tools/fleet-cas-prototype/scripts/xcode-cache-build.sh)
   `DD=` mode).
3. **A missing socket makes Xcode crawl, not fail.** With
   `COMPILATION_CACHE_REMOTE_SERVICE_PATH` naming a socket nobody listens on, the chain took
   about 400 s instead of about 17 s, and succeeded. Anything that turns the plugin on must first
   check that the node daemon is up.
4. **A dead fleet store must be cheap.** A reader node that tried the unreachable store on
   every lookup turned a 17 s build into 45 s: 149 lookups failed slowly, adding about 28 s. The
   node now answers reads as misses and skips the store for 30 s after a failure: 18.9 s, about
   a cold build. Writes fail immediately during the backoff (not measured: the outage runs were
   read-only readers), so nothing is published half-way.
5. **Fetching adds about 20 to 30 s to a full-app fresh read on the LAN.** The first fresh read
   (132 s against an 80 s warm node) was taken while the store host ran a Chromium build; later
   runs put the network share near 20 s (127 s across machines against 108 s with the store on
   the same host, and 91 to 97 s same-host in the
   [prefetch follow-up](fleet-compilation-cache-prefetch-2026-09-24.md)). So most of the
   original 52 s gap was the loaded store host. The chain's split is 6.5 s against 5.6 s.
6. **The tailnet blocks mini-to-mini TCP.** ICMP passes (2.1 ms), but TCP on 22 and on the
   cache port time out; the LAN is open. A fleet store reachable from every machine needs a
   Manaflow ACL grant for the cache port between tagged devices (their admin's change), or a
   LAN-only deployment.

## What M2 and M3 need next

M2, node daemon (the prototype now does the tiering; these are what it lacks):

- **Batch and prefetch fetches.** Closure prefetch has landed (see the
  [prefetch follow-up](fleet-compilation-cache-prefetch-2026-09-24.md)); the remaining change
  is answering the index lookup before the prefetch finishes.
- **Supervision.** A launchd unit per mini, a health check the build wrapper runs before it
  sets the plugin settings (finding 3), and a bounded local store with eviction.
- **Fixed paths as the contract.** The wrapper sets one DerivedData path and one CAS path per
  build slot, the same on every machine (findings 1 and 2). This is a cmux build-contract change
  as much as a glaeda one: cmux CI already builds under a canonical root, and its seeding and
  consuming jobs must use the identical CAS path.
- **Restrict `file_path` uploads** to the local user's build directories, as #1134 requires.

M3, fleet store:

- **Authenticated writes.** Signed index entries (per-builder ed25519) and refuse unsigned
  writes; readers need only reach the port. The TCP store in this prototype has no
  authentication and was bound to the LAN only for the duration of the runs.
- **Enforce publication order** at the store, not just in the writer: refuse an entry until
  its objects are present (the prototype only counts violations; there were none here).
- **Reachability:** a tailnet ACL grant for the cache port, or a per-site store on the LAN.
- **Capacity.** One full-app fill is 1.4 GB on disk (2.26 GB uploaded, deduplicated). With
  main moving about 1.5 commits an hour, a store needs eviction by age or reference, and Kura
  remains the candidate implementation.

## Cleanup

All fleet-cas processes on both minis were stopped and the cmux8s host lock was released. DerivedData,
stores and node caches were deleted. Both minis keep `~/.cache/fleet-cas` (about 5 GB each:
the prototype source and binary, the chain sources, the full-app checkout with resolved packages,
and build logs under `logs/`) for the next M2 runs; delete the directory to reclaim it. Nothing
was installed or registered.
