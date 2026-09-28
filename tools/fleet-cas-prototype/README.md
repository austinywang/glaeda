# Fleet compilation-cache prototype

Throwaway measurement tool for the fleet-shared compilation cache experiment
([`docs/experiments/fleet-compilation-cache-2026-09-23.md`](../../docs/experiments/fleet-compilation-cache-2026-09-23.md)).
It is not part of the `glaeda` crate, is not built by `scripts/verify`, and is
not a supported service.

It serves the two gRPC services Xcode's `COMPILATION_CACHE_REMOTE_SERVICE_PATH`
talks to, from swiftlang/llvm-project
`llvm/lib/RemoteCachingService/RemoteCacheProto` (Apache-2.0 WITH
LLVM-exception, copied unmodified into `proto/`):

- `compilation_cache_service.cas.v1.CASDBService`: content-addressed objects.
  The server computes every ID from the object's bytes and references, so an
  upload cannot claim someone else's ID. Damaged objects read as misses and a
  re-upload replaces them.
- `compilation_cache_service.keyvalue.v1.KeyValueDB`: cache key to result.
  This is the only poisoning surface. `--read-only-kv` refuses every write.
  Existing entries are never replaced (first writer wins).

Storage is one file per object under the store directory. Counters go to
`<store>/stats.json`.

One binary plays two roles:

- **Fleet store**: `fleet-cas tcp:<addr> <store>` serves the same two services
  over TCP to node daemons on other machines.
- **Node daemon**: `fleet-cas <socket> <store> --upstream http://<addr>` is
  what Xcode talks to on each machine. Reads try the node's own store, then
  the fleet store; fetched objects are verified by recomputing their ID and
  kept. Writes land locally and are forwarded before Xcode gets its answer,
  so an index entry reaches the fleet store only after the objects Xcode
  uploaded for it. With `--read-only-kv` the node forwards nothing.

```sh
cargo build --release
# writer: fleet store plus a writing node
target/release/fleet-cas tcp:<lan-ip>:7450 $HOME/.cache/fleet-cas/store
target/release/fleet-cas $HOME/.cache/fleet-cas/n.sock $HOME/.cache/fleet-cas/node \
  --upstream http://<lan-ip>:7450

PKG=... SCHEME=... WORK=... DD=... scripts/xcode-cache-build.sh fill \
  COMPILATION_CACHE_ENABLE_PLUGIN=YES \
  COMPILATION_CACHE_REMOTE_SERVICE_PATH=$HOME/.cache/fleet-cas/n.sock

# reader: empty read-only node per run, then one run on a warm node
PKG=... SCHEME=... WORK=... DD=... scripts/reader-runs.sh http://<lan-ip>:7450 3
```

Keep the socket path short; macOS limits unix socket paths to 104 bytes.
`scripts/fleet-cas-settings.sh <socket>` prints the two plugin settings only
when the node answers; with a dead socket Xcode 26.3 crawls instead of
failing, so wrappers should add the settings only from its output.

The node prefetches: after fetching an index entry it asks the fleet store for
the entry's whole object closure in one streamed call (`GetClosure` in
`proto/fleet_cas.proto`, a glaeda extension, not part of Xcode's protocol).
`--no-prefetch` turns it off.

A TCP store accepts writes only from `--writers IP,IP` (the peer address of each request);
with no list it is read-only. Fleet deployment: `scripts/glaeda-fleet-cas-rollout` and
`docs/CMUX_BUILD_SLOT_MODES.md`.

Signed index entries (#1134 M3, `src/sign.rs`):

- `fleet-cas keygen PATH` creates the writer's Ed25519 key (a 0600 file, never
  overwritten) and prints its public key; `fleet-cas pubkey PATH` prints it again.
- The writer's node runs with `--sign-key PATH` and signs every index entry Xcode
  writes before storing or forwarding it. The signature is one reserved entry
  (`glaeda.fleet-cas.sig.v1`) inside Xcode's value map, so the protocol is unchanged;
  nodes strip it before answering Xcode.
- `--trusted-keys HEX,HEX` on a store or node: only entries signed by those keys are
  accepted on a put or served on a get (local or fetched). Anything else is refused or
  a miss (`kv_sig_fail`). Readers check signatures themselves, so a tampered or
  impersonated store cannot poison them. Objects stay unsigned: their IDs are
  recomputed from content.
- `fleet-cas marker put|get` writes or checks a signed per-commit marker.
  `scripts/fleet-cas-writer-build.sh REPO COMMIT -- BUILD...` runs the writer's build
  and publishes the marker only if the build used the node with no write error
  (`write_failed`), no failed or skipped fleet-store call and no node restart
  (`instance`);
  `scripts/fleet-cas-marker.sh REPO COMMIT` tells a worker whether the store holds a
  commit for its Xcode build.
- `fleet-cas warm URL NAME --trusted-keys HEX --store DIR` copies a whole fill into a
  node's store before a build (`src/warm.rs`). The writer node appends every index key its
  builds look up or write to `fill-keys.log` in its store; the writer script stores the keys
  its build added as a CAS object (the manifest) and names it in the signed marker. `warm`
  fetches the listed entries with 32 calls in flight and the objects they reach in two
  closure streams at a time, checking every signature and ID. Without it, a remote-hit catch-up build
  pays two fleet-store round trips per lookup, mostly one at a time: the 09-25 pair test
  (7837 lookups, 100% hits) took 1631 s at about 336 ms per lookup against a loaded store.
  Entries are written only after the objects they reach, so a warm that stops early never
  leaves the node slower than no warm (a node answers a local entry without prefetching).
  One deadline covers the whole warm (`--timeout`, 150 s).
  `scripts/fleet-cas-warm.sh REPO COMMIT` runs it with the node's settings, as the node's
  user: exit 0 warmed, 1 no marker or a marker without a manifest, 2 something did not
  verify, 3 incomplete (store unreachable, busy, failing or too slow).
  Readers keep warm between builds: the writer script also publishes `REPO/latest/<Xcode>`
  naming its newest fill, and `scripts/fleet-cas-prewarm.sh REPO` (a 5-minute LaunchAgent
  on each node, `glaeda-fleet-cas-rollout --prewarm REPO`) warms that commit whenever the
  host lock is free, so a build's own warm finds nearly everything local. Markers are the
  one kind of entry a newer signed write replaces. Nodes record use on local hits; pruning
  the node store stays with `glaeda-fleet-cas-prune`.
  `up_prefetch_micros` and `up_slow_calls` (calls over 100 ms) in a node's stats separate
  a slow store from serial round trips.

Garbage collection (`src/gc.rs`): a fleet store bumps an index entry's mtime when a read
uses it (at most hourly), and `fleet-cas gc STORE --keep-days N [--dry-run]` keeps the
entries used within N days plus every object they reach, keeps objects written in the last
day, and deletes the rest. Marker reads do not count as uses, so a marker expires N days after its
fill. Which objects an entry names comes from a heuristic over Xcode's value format, so run
`--dry-run` on a real store first and check that "kept entries naming no stored object" is 0.
Run it on the store host; a read that races it at worst misses.

Build under `$HOME` (never `/tmp`, see swiftlang/swift#92545). On Xcode 26.3,
pass `DD=<fixed path>` (the same on every machine) instead of mapping
DerivedData; see the 2026-09-24 experiment. On a cmux build fleet mini,
`scripts/with-fleet-lock.pl` keeps the fleet worker from starting a job
during a measurement.

Known prototype gaps: the node daemon must run on the same host as the build
(the client sends large blobs as local file paths, and the node reads whatever
path it is given, so run it only for your own user; the TCP fleet store
refuses file-path uploads); object uploads are gated only by the peer address
(`--writers`), which anyone on the LAN can spoof for a connection, so a spoofer
can fill the disk but not poison a build once readers use `--trusted-keys`; the
signing key is a file readable by the build user, so any process on the writer
host can sign; a KV entry is not checked for its objects'
presence (`kv_put_dangling` only counts entries that name absent objects);
there is no size budget (eviction is by age of use, `fleet-cas gc`); and a node that cannot reach the fleet
store answers reads as misses and fails writes (counted in `up_errors`), then
skips the store for 30 s (`up_skipped`), so an outage costs about a cold build
and nothing is published half-way. Reads and writes share that backoff, so one
failed upload also pauses fleet reads for 30 s.
