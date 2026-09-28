# Fleet compilation cache: prior art and design notes for glaeda#1134

Researched 2026-09-23 for #1134. Prices, licences and project states change; verify before relying on them.

## 1. Flags that could change the RFC plan

1. **Tuist dropped `COMPILATION_CACHE_REMOTE_SERVICE_PATH`.** Their `cas-plugin/AGENTS.md` (https://github.com/tuist/tuist/tree/main/cas-plugin) says Xcode's built-in remote mode "stalls in 30-50s bursts" on deep module graphs, turning a 274 s no-cache build into 325-445 s even with every hit served in about 2 ms. They now ship a Rust `cdylib` implementing the LLVM CAS plugin ABI (`llcas_*` v0.1), loaded through `COMPILATION_CACHE_PLUGIN_PATH`, which wraps Apple's `libToolchainCASPlugin.dylib` for local storage and does remote read-through and write-through itself via a per-machine proxy. Our M0 win (118 s to 30 s) was on a 600-file package chain. **Before M2, repeat the measurement on the full cmux app graph.** If the stall reproduces, M2's client interface becomes a wrapper plugin, not a gRPC socket server.
2. **Signing makes the store untrusted.** If every node daemon verifies index signatures before inserting an entry locally, the fleet store and R2 become dumb storage. Tailscale identity then only controls who may write at all, not what readers believe. This simplifies M3 and makes the R2 tier safe to add without its own trust model.
3. **Tailscale tags identify machines, not jobs.** A fork PR running on a trusted-tagged node inherits the node's tag. Job trust has to be decided by the local daemon (which signs only for trusted jobs), and the signing key must be unreachable from build processes.
4. **Xcode's own size limit does not bound its local store.** Tuist measured `COMPILATION_CACHE_LIMIT_SIZE` pruning nothing (9.4x over after 16 rounds); stores only rotate generations when the last handle closes. Anything that holds Xcode's CAS open (a daemon, a hot slot) needs an explicit prune path.
5. **`cache poisoned` conflicts are routine.** Tuist sees them "often" because recompiles are not byte-reproducible, and `SWBBuildService` issues most puts (71 of 103), often for keys a compiler also put. First-writer-wins is right, but the conflict counter should be a metric, not an alarm.

## 2. Who implements the Xcode/LLVM cache service

**Reference server in swiftlang/llvm-project.** `llvm/lib/RemoteCachingService/` has `RemoteCacheServer/RemoteCacheServer.cpp` and `LLVMCASCacheProvider.cpp`, plus the tool `llvm/tools/llvm-remote-cache-test` (https://github.com/swiftlang/llvm-project/tree/next/llvm/tools/llvm-remote-cache-test), "a server for the remote cache service protocol, for testing purposes" backed by an on-disk LLVM CAS. It can wrap a command and set `LLVM_CACHE_REMOTE_SERVICE_SOCKET_PATH`. Useful as a conformance oracle. Semantics worth copying:
- `CASBytes` is `oneof { bytes data; string file_path }`. On Put with `file_path`, the provider reads the client's file directly (`MemoryBuffer::getFile`), so the server must share a filesystem with the client. This confirms the RFC's node-daemon requirement.
- `write_to_disk` on Get/Load is advisory: the proto says the service may still return bytes, and if it writes a file "it should have the right access for the client to be able to move it". The provider writes `%%%%%%%.blob` into its temp dir. Put that dir on the same volume as the client's CAS, owned so the build user can rename it.
- KV `Value` is `map<string, bytes> entries`; the reference `PutValue` just stores the serialized value as a CAS object and maps the key to it. No closure check, no conflict policy.
- Protos: https://github.com/swiftlang/llvm-project/tree/next/llvm/lib/RemoteCachingService/RemoteCacheProto

**Tuist** (https://github.com/tuist/tuist, CLI and `cas-plugin` MIT; `server/` and `kura/` MPL-2.0 per `LICENSE.md`).
- Client: `tuist-cas-plugin` (Rust cdylib) plus `tuist-cas-proxy` (launchd daemon). Remote transport is REAPI over gRPC: llcas key K maps to AC digest `sha256(K)`, and each llcas node becomes one CAS blob encoding `"TCP0" | ref_count | refs | data`, zstd-compressed. Every downloaded blob is checked against its sha256; mismatch becomes a miss.
- Closure discipline was learned the hard way: "demand loads never persist an incomplete graph"; a local AC hit is served only after walking the full local closure; parents are never stored over missing descendants.
- Uploads: background spool on dev machines, synchronous with a 30 s budget and breaker on CI (PR https://github.com/tuist/tuist/pull/13317), so ephemeral VMs do not exit owing uploads. A store copied to other machines must be drained before promotion.
- Auth: the proxy holds a bearer JWT from `tuist auth token`; `upload: false` makes a client read-only, enforced in the proxy.
- Server: **Kura** (https://github.com/tuist/kura, Rust). RocksDB metadata plus segment files, leaderless replication with DNS peer discovery, optional peer mTLS, Xcode CAS and KV HTTP endpoints plus REAPI gRPC, auth through an operator Lua extension (JWT verifier), memory soft/hard limits that shed writes with `RESOURCE_EXHAUSTED`. Kura "refuses to serve an ActionResult whose blobs are gone" (`first_evicted_output`), which is read-time completeness gating.
- Placement data: on-host Kura 113.6 s, same-LAN node at 0.89 ms 118 s, 66 ms WAN about 65 s over the local floor. Supports "one central store per site" for #1134's open question.

**Bitrise.** Local proxy implementing the LLVM gRPC server, relaying into the same Build Cache backend they use for Bazel and Gradle (REAPI CAS). CLI is open source, Go, MIT: https://github.com/bitrise-io/bitrise-build-cache-cli. Blog: https://bitrise.io/blog/post/lifting-the-hood-on-build-cache-for-xcode

**CASBuildCache** (https://github.com/Timing-GmbH/CASBuildCache, Rust, MIT). File or SQLite CAS, SQLite KV, BLAKE3, `write_to_disk` support, LRU/TTL with high/low water marks that evict KV before CAS, `verify_on_read`. Self-described as "entirely AI-generated" and unmaintained; read for ideas only.

Not found: BuildBuddy, NativeLink, Namespace, Depot shipping an Xcode compilation-cache front end. The common pattern (Tuist, Bitrise) is a local proxy that maps the LLVM protocol onto REAPI.

## 3. Nix binary cache signing

- Fingerprint (NixOS/nix `src/libstore/path-info.cc`): `"1;" + storePath + ";" + narHash(nix32, with "sha256:") + ";" + narSize + ";" + comma-joined full reference store paths`. Signing `ValidPathInfo::sign` inserts `signDetached(fingerprint)`.
- narinfo `Sig:` is `keyname:base64(ed25519 signature)`, repeatable. It covers StorePath, NarHash, NarSize, References, and not URL, Compression, FileHash, FileSize, so the same signature is valid from any mirror (https://docs.tvix.dev/rust/nix_compat/narinfo/index.html, https://notashelf.dev/posts/nix-cache-proxy). This is the property #1134 wants.
- Keys: `trusted-public-keys` entries are `name:base64(pubkey)`; secret keys in `secret-key-files`. `require-sigs` (default true) demands a trusted signature for any non-content-addressed path; content-addressed paths need none, exactly like CAS objects (https://nix.dev/manual/nix/latest/command-ref/conf-file.html).
- `trusted-users` may add substituters and import unsigned realisations or unsigned input-addressed paths; untrusted users are limited to `trusted-substituters`. Nix's content-addressed derivations sign **realisations** (derivation output to store path), the closest analog of our KV entries.
- Rotation: list old and new keys in `trusted-public-keys`, re-sign with `nix store sign`. There is no revocation list; you remove the key. Weakness to avoid: no validity window in the signed data.

## 4. Bazel remote cache ecosystem

- **REAPI AC contract.** GetActionResult: servers SHOULD ensure referenced blobs are available when returning a result and for some time after; UpdateActionResult may be refused (https://github.com/bazelbuild/remote-apis/blob/main/build/bazel/remote/execution/v2/remote_execution.proto). Bazel docs recommend only CI writes; developers use `--remote_upload_local_results=false` (https://bazel.build/remote/caching). Bazel fails builds when an AC hit's blobs are gone (https://github.com/bazelbuild/bazel/issues/12423), which is why servers gate.
- **Buildbarn bb-storage** (https://github.com/buildbarn/bb-storage, config https://github.com/buildbarn/bb-storage/blob/main/pkg/proto/configuration/blobstore/blobstore.proto): `CompletenessCheckingBlobAccess` returns an ActionResult only if all outputs are in CAS, otherwise treats it as absent (https://pkg.go.dev/github.com/buildbarn/bb-storage/pkg/blobstore/completenesschecking), with a bound on Tree size. `LocalBlobAccess` uses old/current/new block rings with refresh-on-read of old blocks (FIFO that approximates LRU, no per-object bookkeeping). `MirroredBlobAccess` repairs blobs present in only one backend. `ShardingBlobAccess` uses rendezvous hashing. Per-instance `getAuthorizer`/`putAuthorizer`/`findMissingAuthorizer`.
- **NativeLink**: `CompletenessCheckingStore`, `VerifyStore` (hash and size verified while streaming, cancels on mismatch), evicting map with `max_bytes` (https://nativelink.pages.dev/config/production-config/).
- **bazel-remote** (https://github.com/buchgr/bazel-remote): required `max_size`, LRU file eviction, ActionResult validation (`disable_http_ac_validation`) and gRPC AC dependency check on read (`disable_grpc_ac_deps_check`), htpasswd or mTLS, `allow_unauthenticated_reads`.
- **BuildBuddy**: API keys can be read-only or "CAS-only" (write CAS, not AC), which is the RFC's "untrusted uploads objects, not index" split (https://www.buildbuddy.io/docs/guide-auth/, https://github.com/buildbuddy-io/buildbuddy/pull/13391).

Summary: (a) missing blobs are handled by read-time completeness gating everywhere; (b) eviction is byte-budget LRU or block rings; (c) write authorization is per credential, with AC write the privileged bit; (d) integrity is hash-on-read or hash-while-streaming. None sign AC entries.

## 5. Signed entries elsewhere

- **Turborepo**: HMAC-SHA256 over the artifact with `TURBO_REMOTE_CACHE_SIGNATURE_KEY`, sent as `x-artifact-tag`; failures become misses (https://turborepo.dev/docs/core-concepts/remote-caching). Symmetric, so any reader can forge. Do not copy.
- **Gradle**: no signing; guidance is `isPush = isCiServer` (https://docs.gradle.org/current/userguide/build_cache.html).
- **ccache**: `read-only` remote attribute, XXH3 checksums for corruption only, "trust all users of the shared cache" (https://ccache.dev/manual/latest.html). **sccache**: no signing, relies on backend credentials.
- **Sigstore/in-toto**: sign a statement about subject digests in a DSSE envelope with a pre-authentication encoding. Too heavy for per-compile entries, but borrow two ideas: domain-separated, length-prefixed encoding (no JSON canonicalization) and optional provenance fields (repo, commit, job).

## 6. Tailscale identity

- A server learns the caller via LocalAPI WhoIs: `tailscale whois <ip:port>`, or `Server.LocalClient().WhoIs` in tsnet (https://pkg.go.dev/tailscale.com/tsnet). The response carries the node (including tags), user profile, and a capability map.
- Prefer **grants with app capabilities** over hardcoded tag names: `{"src":["tag:glaeda-builder"],"dst":["tag:glaeda-cache"],"app":{"<our-domain>/cap/cache":[{"index":["write"]}]}}`, read back through whois (https://tailscale.com/kb/1537/grants-app-capabilities).
- Caveats: tagging removes user identity; owners/admins can apply any tag (https://tailscale.com/docs/features/tags); tagged nodes have key expiry disabled by default (https://tailscale.com/docs/features/access-control/key-expiry); sharing strips tags, and tagged nodes cannot accept shares, so a cross-tailnet mini gets no tag-based rights (https://tailscale.com/kb/1084/sharing). Anyone with root on a node holds its identity, and every process there shares it.

## 7. Recommendations for M2/M3

### Signing scheme
- **Algorithm:** ed25519, with an `alg` field so P-256 ECDSA can be added. Secure Enclave keys on Macs are P-256 only, which is the path to hardware-bound builder keys later.
- **Signed statement** (length-prefixed fields, domain tag first):
  `"glaeda-cc-index/v1" | fleet_id | toolchain_generation | key | value_entries | closure_count | closure_bytes | builder_key_id | issued_at | provenance?`
  - `toolchain_generation`: Xcode build, swift-frontend and clang version strings, SDK build, CAS hash schema. This makes cross-generation replay impossible, not just unlikely.
  - `key`: the raw KV key bytes (or sha256 of them if large).
  - `value_entries`: sorted `(name, bytes)` pairs, exactly as returned to Xcode.
  - **No separate closure digest needed.** LLVM CAS IDs hash data plus refs, so root IDs in the value already commit to the whole closure (as NarHash does for Nix). Sign `closure_count`/`closure_bytes` as hints for completeness checks and budgeting. Confirm in M2 that value entries decode to CAS IDs for both Swift and Clang puts.
  - `provenance` (optional): repo, commit, job id, for audit and targeted revocation.
- **Keys:** one per trusted builder node, generated at enrollment (#1056), never per fleet. The fleet trust set is a signed list of `(key_id, pubkey, not_before, not_after, scope)` distributed by glaeda.
- **Custody:** the daemon runs as its own user (LaunchDaemon) and holds the key; build jobs talk to the socket and never see the key. The daemon signs only when the job it is serving is trusted (per #1056/#1057 job trust), never because of the node's tag.
- **Verification:** every node daemon verifies before inserting a fetched entry into its local store and caches the result. The fleet store also verifies on publish (defense in depth, cheap rejection), but readers are the trust anchor.
- **Rotation/revocation:** overlap old and new keys. Revoke by removing the key or shortening `not_after`; readers drop entries signed by revoked keys as misses. Entries die with their toolchain generation anyway, so re-signing is rarely needed. Revoking a key should also purge that key's entries from the fleet store.

### Closure completeness
- Publish order: objects bottom-up, then the index entry. The fleet store accepts an entry only when every reachable object is present (Buildbarn/NativeLink rule).
- **Also gate on read** (Kura, bazel-remote): eviction can remove objects after publication. Store a closure manifest (the ID list) beside each entry so checks are set lookups, not object decodes.
- Node daemon: never record a KV entry locally until the full local closure exists (Tuist's hard-won invariant).

### Eviction
- Node: whole-generation eviction as in #21, plus a byte budget with Buildbarn-style block rings or LRU within the live generation. Evict index entries first, then GC objects unreachable from live entries (CASBuildCache order), so eviction never creates dangling entries.
- Separately prune Xcode's own `COMPILATION_CACHE_CAS_PATH` on hot slots; its size limit does not self-enforce.

### Writer identity: tags vs signatures
- **Tailscale (connection layer):** may this node reach the store; may it write objects; may it submit index entries at all; which namespace; rate limits. Use grants app capabilities checked via whois.
- **Signatures (data layer):** should a reader trust this entry, independent of transport, replication, R2, or backups.
- Untrusted clients get object writes into a scratch namespace and no index writes, as in BuildBuddy's CAS-only keys.

### Build on existing vs own daemon
- **Node daemon: keep our own, thin.** The gRPC surface is six RPCs, and signing, job trust, and receipts (#1095) are ours regardless. Use `llvm-remote-cache-test` as the conformance oracle. Keep Tuist's `cas-plugin` as the design reference and as the fallback client shape if flag 1 reproduces.
- **Fleet store: evaluate Kura before writing one.** It already handles replication, REAPI, write shedding, and read-time eviction gating, and with reader-side signatures it does not need to be trusted. bb-storage is solid, but Tuist's refs-inside-blob encoding hides references from its completeness checker, so it would need a custom mapping.
- Do not adopt Tuist's client wholesale: it is tied to Tuist auth and accounts.
