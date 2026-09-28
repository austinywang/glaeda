# Runner market: funding, sentiment, Bazel, and where glaeda fits

Researched 2026-09-23. Extends [runner-market-hot-state.md](runner-market-hot-state.md) (pricing,
hot-state mechanics, Apple licensing) and [fleet-cache-prior-art.md](fleet-cache-prior-art.md)
(cache protocols, Kura, Tuist). Those are not repeated here.

Limits: Reddit and X were not reachable, so sentiment comes from Hacker News, GitHub issues and
vendor posts. Tuist's site blocks automated fetches; its blog was read from GitHub source.
Anything marked **estimate** or **unverified** should not be quoted as fact.

## What each vendor sells (Linux list prices, 2026-09-23)

| Vendor | Product | Headline price | Technically distinctive |
| --- | --- | --- | --- |
| [Blacksmith](https://www.blacksmith.sh/pricing) | Drop-in GitHub Actions runners | 2 vCPU x64 $0.004/min, ARM $0.0025/min; sticky disks $0.50/GB-mo | Owned bare metal, consumer gaming CPUs for single-core speed, colocated cache |
| [Depot](https://depot.dev/pricing) | Remote Docker builds, runners, Depot CI, generic cache | $20 and $200/mo plans; runners $0.006/min overage; Docker $0.04/min | Persistent NVMe cache outside build VMs; one cache for Bazel, Gradle, sccache, Turbo, Pants, Maven |
| [Namespace](https://namespace.so/pricing) | Runners, devboxes, Docker builds, caches | $100 and $250/mo plans; about $0.001 per vCPU-min; macOS 10x | Local NVMe cache volumes, including macOS |
| [WarpBuild](https://www.warpbuild.com/pricing) | Runners, bring-your-own-cloud | $0.004/min x64; BYOC $0.002/min | BYOC; Linux-only snapshots |
| [Ubicloud](https://www.ubicloud.com/blog/ubicloud-price-adjustment-2026) | Open-source cloud and runners | 2 vCPU $0.00125/min after a 25% rise on 2026-09-01 | Cheapest; self-hostable (AGPL) |
| RWX (Mint) | Task-graph CI with content-based caching | not found | Per-task content cache |
| [BuildBuddy](https://www.buildbuddy.io/pricing) | Bazel cache, remote execution, results UI | free tier, then pay as you go | Open core; Mac remote execution |
| EngFlow | Bazel remote execution and observability | enterprise only | Bought tipi.build (2025-03) for C/C++/Rust |
| [NativeLink](https://www.nativelink.com/pricing) | Rust REAPI server | open source; enterprise by quote | Bazel, Buck2, Pants, Siso |
| [Aspect](https://www.aspect.build/pricing) | Bazel CLI and Workflows | per team, no usage billing when self-hosted | Runs in the customer's cloud |
| Tuist | Xcode, Gradle and Bazel cache and insights | cache egress 100 GB free, then $0.35/GB (**unverified**, page blocked) | Kura cache; moving compute onto owned Macs |
| Bitrise | Mobile CI and build cache | credit packages | One REAPI backend for Xcode, Gradle, Bazel |

None of them owns a build graph except RWX (task graph) and the Bazel vendors (Tuist is building
one, see below). The large runner companies sell fast machines plus state that survives between jobs.

## Funding, valuation, exits

| Company | Rounds | Traction |
| --- | --- | --- |
| Blacksmith | Seed $3.5M (GV, YC, 2025-05); Series A $10M at $60M (GV, 2025-09); Series B $45M at **$550M** (Peak XV, 2026-08-12) ([TechCrunch](https://techcrunch.com/2026/08/12/blacksmiths-valuation-jumps-10x-to-550m-as-ai-coding-fuels-software-validation/)) | $10M run rate with 10 staff at Series A; now "tens of millions", about 30 staff, 5,000+ customers |
| Depot (YC W23) | Seed $4.1M (2024-08); Series A $10M (Felicis, 2026-03) ([Depot](https://depot.dev/blog/depot-raises-series-a)) | "thousands of teams"; revenue $7.3M is an **estimate** (Extruct) |
| Namespace | $23M seed and Series A led by NEA (2026-03-23) ([Namespace](https://namespace.so/blog/series-a)) | "1,000+" companies |
| RWX | Seed $7M (2024-04); Series A $12M (Hyde Park, 2026-08-05) | Honeycomb, Verkada, nCino |
| EngFlow | Seed $3.7M; Series A $18M (Tiger, a16z, 2022-11) | 63 staff (2026-06) |
| BuildBuddy | YC plus $3.15M Series A (2020-12) | not disclosed |
| Aspect | $3.85M seed (FirstMark, 2024-10) | |
| Trace Machina (NativeLink) | $4.7M seed (2024-08) | |
| WarpBuild | YC; funding and ARR **unverified** | |
| Bitrise | about $100M+ total; Series C $60M (Insight, 2021-11) | |

Exits: Cirrus Labs joined OpenAI on 2026-04-07 and shut down Cirrus CI on 2026-06-01; the price
is **undisclosed** (a circulating $450M figure has no credible source). Cursor bought Graphite
(2025-12), an adjacent space.

The category is being funded because AI coding agents multiply the code that must be built and
tested. Blacksmith's press explicitly ties its growth to that.

## Sentiment

What users like:

- Faster and cheaper at once, and drop-in (a Depot user: runners "much faster, are also much
  cheaper", [HN 2025-12](https://news.ycombinator.com/item?id=46190267)).
- Easy to leave: users switch back to GitHub runners during an outage.
- Shared pools absorb peaks that a fixed self-hosted fleet queues on
  ([HN 2026-06](https://news.ycombinator.com/item?id=48475720)).

What users dislike:

1. **Outages block merges.** Blacksmith was degraded about 7.5 hours on 2026-07-21 when a
   GitHub webhook burst overloaded control-plane Redis and cache and sticky-disk requests timed
   out ([postmortem](https://www.blacksmith.sh/blog/blacksmith-outage-on-july-21-2026)).
   Downstream projects saw checks QUEUED for hours and added GitHub-hosted fallbacks.
2. **Per-minute rounding** on short jobs ("the per minute is biting me",
   [HN](https://news.ycombinator.com/item?id=44658909)).
3. **Cache over a WAN is slow**; colocated cache is what delivers the speed.
4. **Self-hosting is cheaper but is operations work** (Firecracker on one server cut thousands
   a month to hundreds, [HN](https://news.ycombinator.com/item?id=48476035); Kubernetes runners
   are "no walk in the park").
5. **Trust**: untrusted PR code next to secrets on third-party machines; a monitoring market
   exists for exactly this ([StepSecurity](https://www.stepsecurity.io/blog/runtime-security-for-third-party-github-actions-runners)).
6. **Price changes** (Ubicloud +25%; GitHub's announced then paused self-hosted fee).
7. **macOS queue times** on hosted runners, sometimes hours.

## Bazel for Apple apps

- Bazel 9 LTS (2026-01-20) removed WORKSPACE; Bzlmod is the only dependency system
  ([blog](https://blog.bazel.build/2026/01/20/bazel-9.html)). With remote builds that skip
  intermediate downloads, a missing cache blob fails the build, so the store is load-bearing.
- Adopters: Spotify (p75 build plus test 80 to 20 min,
  [InfoQ](https://www.infoq.com/news/2023/10/spotify-bazel-ios-transition/)), Airbnb (from Buck,
  [InfoQ](https://www.infoq.com/news/2024/02/airbnb-bazel-migration-ios/)), Gojek. Lyft, Uber and
  Snap are commonly cited but **unverified** here. No documented iOS team moved off Bazel.
- Pain points: Xcode integration (indexing, debugging, SwiftUI Previews), rules breaking on
  Bazel or Xcode upgrades, migration cost.
- Xcode's compilation cache protocol is not REAPI. Tuist and Bitrise run a local proxy that maps
  it onto a REAPI CAS and action cache. A REAPI backend can serve Xcode only through such a
  proxy, which is the shape of glaeda's node daemon.
- Non-Bazel tools on REAPI stores: ccache has a bazel-remote layout; sccache has no REAPI
  support ([#358](https://github.com/mozilla/sccache/issues/358)); Gradle has its own HTTP API.
  Kura, Bitrise and Depot Cache already serve several tools from one store.
- Tuist now also caches Bazel and is building "Once", a build system that loads existing
  projects as they are. It argues compute and cache "had to be physically colocated", and says
  renting a Mac costs its purchase price in about three months; it runs operations and product with a team of four
  ([post](https://github.com/tuist/tuist/blob/main/server/priv/marketing/blog/2026/09/12/the-new-tuist.md)).

## Where glaeda fits

Glaeda is Bazel's layer 3 (remote cache and execution) plus things Bazel does not do: state kept
hot at `main` per PR, fleet and disk lifecycle, and trust tiers for owned hardware. Xcode 27
already computes cache keys inside the compiler, so glaeda never needs Bazel's hardest part.

Likely buyers: iOS and macOS teams of about 10 to 200 engineers who own or lease Macs, suffer
hosted macOS queues or cold builds, will not migrate to Bazel, and do not want fork PRs near
their secrets.

Pricing shape (inference): software for the customer's own fleet, per Mac per month, not compute
minutes. That fits Apple's licence (2 VMs per Mac, leases of at least 24 h) and avoids the
rounding complaint. Reference points: Cirrus charged $150/mo per concurrent Mac (sourced in runner-market-hot-state.md;
Cirrus has since stopped onboarding customers); a hosted Mac at 8 h/day and $0.08/min
is about $800/mo over 21 workdays (about $1,150 over 30 days). **$100 to $300 per Mac per month** looks credible.

Contested: Tuist is building a colocated owned-Mac stack; Namespace has macOS cache volumes.
Uncontested so far: state hot at `main` per PR (the cmux Air campaign measured 36 s vs 953 s).

Back-of-envelope, illustration only: 100 teams x 8 Macs x $200/mo is about $1.9M ARR, plausible
for a 2 to 4 person team given Tuist's precedent. Blacksmith-scale value would need Linux and a
hosted pool. Blacksmith's rounds imply about 6x run rate at Series A and, as an **estimate** from the
unquantified "tens of millions", roughly 15 to 30x at Series B.

## Design rules taken from the dislikes

1. Fall back to GitHub-hosted or other runners automatically; never leave checks QUEUED.
2. Keep the control plane off the data path, so a webhook burst cannot block cache or disk.
3. Bill per second or flat; no per-minute rounding.
4. Cache on the same LAN as compute, never across a WAN.
5. Written trust tiers; fork PRs cannot reach secrets or write trusted cache entries.
6. No price changes on existing plans.
7. Receipts for queue time, cache hits and fallbacks.
8. Easy to leave: stock GitHub Actions YAML, no forked actions.

These are recorded as product rules in RFC #1134.
