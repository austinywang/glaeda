# Fleet compilation cache: signed writes (2026-09-24)

Issue: #1134 M3, first step (signed index entries and the per-commit marker). Design:
[cmux build slots, Writer](../CMUX_BUILD_SLOT_MODES.md#writer). Prototype:
`tools/fleet-cas-prototype` (`src/sign.rs`).

## What changed

Objects are content-addressed and every reader recomputes their IDs, so only index entries
(cache key to value) can poison a build. Now:

- The writer's node signs each index entry with Ed25519 (`--sign-key`). The signature is one
  reserved entry in Xcode's value map, so Xcode's protocol is unchanged; nodes strip it
  before answering Xcode.
- A store or node with `--trusted-keys` accepts and serves only entries signed by a trusted
  key. Readers verify for themselves, so the store host, the network and the peer-address
  allowlist cannot make them use a poisoned entry. (A store can still withhold entries, and
  a marker says only that the writer's fill succeeded, not that the store still serves it.)
- A per-commit marker is itself a signed index entry under its own key prefix. The writer
  publishes it only after a build that used the node with no write error, no failed or
  skipped fleet-store call and no node restart.

## Test

Chain `CmuxSettingsUI`, Xcode 26.6 (17F113), writer and store on cmux7s, reader on cmux8s
over the LAN, with a throwaway key and scratch stores on ports 7451 to 7453; the deployed
services on 7450 were not touched. Each reader run started with an empty node and an empty
local CAS.

| Case | Result |
| --- | --- |
| Writer fill through the signing node, under `fleet-cas-writer-build.sh` | 18.5 s, 711 entries signed and pushed, 0 refused, marker published |
| Unsigned node on an allowed writer address | build passed (17.8 s); store refused all 711 entries and kept 0; script exit 4, no marker |
| Reader trusting the writer key | 149 of 149 hits, 9.5 s, 0 signature failures |
| Reader trusting another key | 0 of 149 hits, 17.0 s (a cold chain), 149 signature failures, build passed |
| Reader trusting the writer key, store without checks serving 1 altered entry | 148 of 149 hits, 1 signature failure, build passed |
| `fleet-cas-marker.sh` for the filled commit / an absent commit / with another trusted key | exit 0 with the entries / 1 / 2 |

Locally (no Xcode):

- markers from an untrusted key are refused;
- a store with trusted keys serves an entry altered on disk as a miss;
- after a key rotation, an entry signed by the dropped key reads as a miss and the next
  signed write replaces it (`kv_put_replaced`);
- `fleet-cas-writer-build.sh` refuses the marker (exit 4) for a build that never used the node
  and for a node restart during the build.

Unit tests cover signing and verification:

- a changed, added or removed value entry, and another cache key, fail;
- so do an untrusted signer, a missing or truncated signature, and an entry-boundary splice;
- the key file is 0600 and never overwritten.

Signing cost does not show at this size (711 entries); the full app writes about 7,700.

## Limits

- The key is a file readable by the build user on the writer mini, so any job there can sign.
  The writer mini must leave the PR pools before its key exists. Running the writer's node
  as a separate user would remove that condition.
- Object uploads are still gated only by the peer address, so a LAN spoofer can fill the
  store's disk but cannot make a reader use anything.
- The writer node keeps its own copy of what it wrote. A fleet store that is emptied needs the
  writer's node store emptied too, or a later marker can claim entries the store lost. Key
  rotation therefore starts the store over (design doc).
- Writer builds should start from an empty local CAS. Whether Xcode answers a lookup from its
  local CAS without asking the node (so a failed upload is never retried) is unverified.
- The full app has not been filled through a signing node yet.

## Next

1. Leo creates the production key on the writer mini (command in the design doc), after the
   fleet session takes that mini out of the PR pools.
2. `glaeda-fleet-cas-rollout --store cmux7s-mac-mini --trusted-keys <pub> --writer <writer>
   --sign-key /Users/cmux/.config/glaeda/fleet-cas-writer.key <nodes> --apply`.
3. The cmux main build runs on the writer under `fleet-cas-writer-build.sh`, and workers check
   `fleet-cas-marker.sh` before catch-up (cmuxterm-hq build-fleet recipe).
