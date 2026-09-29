# CMUX compiled-product reuse

CMUX has a compiled-product reuse mechanism that ran **0 hits in 112 attempts**. The failure was
an identity failure, which is the thing this runtime claims to own. This document states the
identity function that mechanism actually needs, what Glaeda's model does and does not express,
what a fork pull request is allowed to consume, and the first integration step that is real.

Read `docs/REUSABLE_STATE_LIFECYCLE.md` for the reusable-state contract, `docs/THREAT_MODEL.md`
for the hostile boundary, and `docs/CMUX_WORKLOAD_PROFILES.md` for the rule that CMUX owns the
meaning of CMUX workloads. This document adds no new authority to any of them.

## 1. The measured failure

`manaflow-ai/cmux#13758` measured the 12 hours ending 2026-09-22T18:06Z across 386 `ci.yml`
runs, and `manaflow-ai/cmux#13709` explains the mechanism behind the macOS row:

| signal | hits | attempts |
| --- | ---: | ---: |
| macOS compiled-product reuse | **0** | 112 completed admission jobs |
| Linux admission skip from an earlier run | **0** | 157 lookups |
| Linux admission skip because nothing the build reads changed | 69 | 336 |

Miss reasons across the 112: **101 `consumer_revision_mismatch`, 11 `consumer_untrusted`**.

Neither is a cache miss. Both are consumer-side admissibility refusals raised *before any
producer was considered*. The producer sealed `git rev-parse HEAD`, which on a `pull_request`
event is the ephemeral merge of the head into the base; the consumer required its checkout to
equal the run's attested `head_sha`, which a `pull_request` checkout never is. Two sides
addressed two different key domains and never met, so neither ever had a disagreeing field to
report.

The cost was real: of 206 consecutive pushes to one branch in that window, 87 (42%) changed
nothing that reaches the compiled product, and each paid a fresh 20-35 minute macOS compile
against a Blacksmith pool running at 11.22 busy servers against a 9-13 slot cap.

## 2. The identity function the workload needs

CMUX's reuse needs **two identities that are bound simultaneously and never required to be
equal**.

### 2.1 Content identity

`cmux-app-host-product-inputs/v1`, computed by `scripts/ci/product_input_identity.py`:

```
{ schema, algorithm, source, recipe }
```

- `algorithm` — self-hash of the identity script, so changing how identity is computed
  invalidates everything computed the old way;
- `source` — SHA-256 over the sorted `git ls-tree -r` entries whose path satisfies
  `reaches_product()`;
- `recipe` — SHA-256 over a projection of the `macos-compile-admission` job in
  `.github/workflows/ci-macos.yml`: job-level `env` minus an explicit orchestration-only set,
  `defaults`, and every named step minus an explicit `NON_PRODUCT_RECIPE_STEPS` set. The
  projection is fail-closed: an unclassified job key or an unknown step name raises rather than
  being ignored.

This is a good content identity and Glaeda's `ReusableStateIdentityContract` expresses it
directly: `family_inputs_digest` carries `source`, `cache_schema_generation` carries `algorithm`,
`build_configuration_digest` / `compiler_flags_digest` carry what `recipe` projects, with
`toolchain_generation`, `os_runtime_generation` and `architecture` carrying the environment
terms that `reuse_app_host_products.py contract()` gathers separately.

### 2.2 Provenance binding

The second identity has no name in Glaeda. It is the binding between **the bytes that were
built** and **the name the scheduling platform attests for them**.

A `pull_request` run has three distinct revisions:

| term | what it is |
| --- | --- |
| `github.sha` | the ephemeral merge commit that was checked out and compiled |
| `head_sha` | the pull-request head, which GitHub attests for the run |
| first parent of the merge | the base the head was merged into |

The 0/112 bug is the assertion `checked_out_revision == head_sha`. Those two are equal only when
the base contributed nothing, which is almost never. The correct predicate is not equality but a
*binding*: the checked-out revision is a two-parent merge whose **second** parent is the attested
head, and whose own tree carries the same product inputs. That is what
`manaflow-ai/cmux#13718` added on the consumer side (`attested_checkout`) and
`manaflow-ai/cmux#13758` on the producer side (`attested_producer_revision`).

Note the asymmetry those fixes preserve: first-parent position is the base merged *into* the
pull request, second-parent position is the pull request merged *for testing*. Only the second
is the tested form of that head. An octopus merge is refused outright.

### 2.3 Would Glaeda's model have caught this? No.

This is the honest answer, and it is worth more than a success story would have been.

**Glaeda's mismatch machinery is a comparator, never a resolver.**
`evaluate_reusable_state_consumption(expected, generation, …)`,
`HotStateAdmissionTarget::first_context_mismatch(candidate, current)` and
`ArtifactCompatibility::accepts(consumer)` all take both identities as arguments. Nothing in the
crate resolves a generation *from* a key; `ReusableStateIdentityContract::digest()` is computed
and compared but never used as a lookup key. A key-domain defect therefore produces "no candidate
was ever presented," which is precisely the case those enums cannot observe.

**The telemetry then disguised it.** Before `#1106`:

- `ReusableStateMetrics::hit_rate_basis_points()` returned `0` when `lookups == 0`, so a
  generation nobody can address read the same as one nobody had tried yet;
- `recommendation()` gates `StopPublishing` on `metrics.lookups >= min_lookups_for_value`, which
  such a generation never accumulates, so it returned `Observe` indefinitely;
- `ReusableStateStatusSummary` reported `heat: Cold` and `recent_hit: Never`, which is also what
  day one looks like.

Reproduced on `origin/main` before the fix:

```
untried:                             hit_rate=0 recommendation=Observe
unreachable (112 refused attempts):  hit_rate=0 recommendation=Observe
```

CMUX independently invented the vocabulary this model lacked. `record_reason` separates
`consumer_untrusted`, `consumer_revision_mismatch`, `consumer_product_inputs_mismatch` and
`consumer_provenance_unavailable` — all recorded before a candidate is considered — from the
candidate-side reasons. That separation is what made the bug findable there. Having the miss
reason printed per job is how 101 and 11 were counted at all.

Two gaps follow.

| gap | status |
| --- | --- |
| A reuse identity no consumer can resolve is invisible, and reads as a healthy new generation | closed by `#1106` / `#1107`: `unresolved_identity_attempts`, `hit_rate_basis_points -> Option<u16>`, `ReusableStateUnresolvedReason`, `InvestigateUnreachableIdentity` |
| `ReusableStateIdentityContract` has no **provenance** term, so the merge-commit-versus-attested-head binding is not expressible — only equality is | open |

The second gap is the deeper one. `ArtifactProducerProvenance` exists in
`immutable_artifact_distribution`, but it is not part of the reusable-state identity contract and
there is no provenance-refusal miss reason. A model that can only say "these identities are equal
or not" will keep reinventing the 0/112 bug, because the interesting cases are the ones where two
different names must be bound to the same bytes.

## 3. The trust boundary: what a fork pull request may reuse

A fork head is attacker-authored content from the open internet. Today CMUX refuses fork reuse
entirely, which is why 11 of the 112 misses were `consumer_untrusted`. Glaeda's trust tiers are
supposed to answer exactly this.

### 3.1 The invariant that does not move

**Owned hardware never materializes fork-authored source.** The producer side is closed, in both
directions:

- `trusted_ci_run()` requires `run.head_repository.full_name == repository`, so a fork run can
  never enter the trusted producer set;
- `persistent-macos-compile.yml`'s `authorize` job refuses anything that is not an open
  same-repository pull request from a `MEMBER`/`OWNER` at the exact attested head/base/merge
  triple.

A fork consumes from the GitHub artifact store. It never causes a mini to fetch its code.

### 3.2 The rule

> **A fork pull request may adopt a compiled product whose identity depends on no input the fork
> controls — that is, the product of its merge base — and only when the product inputs of the
> fork's head are identical to the merge base's. It may adopt nothing built from its own head or
> its own merge tree, and it may publish nothing.**

Mechanically, with no local file taken on trust:

```
github_product_identity(api, base_sha) == github_product_identity(api, head_sha)
    -> the fork may restore the base-anchored product
    -> otherwise it compiles
```

Both sides are recomputed from GitHub's immutable Git objects, which is what
`github_product_identity` already does. The fork controls the *content* of `head_sha`, but it can
only make the two identities equal by making its product inputs literally equal to the base's,
which is the condition being tested.

This requires a producer class CMUX does not have today. `PERMITTED_PRODUCERS` currently maps
`pull_request -> {pull_request}` and `permitted_pair` additionally requires the same pull-request
number, so a pull request can only reuse its own earlier runs. A **base-anchored** product — the
product of `main`'s tip, maintained continuously — is the missing piece, and §4 is where it comes
from.

### 3.3 Tiers

| tier | producer | consumer | status |
| --- | --- | --- | --- |
| T0 sealed | same-repo CI run, GitHub-attested | same-repo run, same pull request | built; fixed by `#13718` / `#13758` |
| T1 revalidated | owned Mac fleet under Glaeda admission | same-repo run, hosted revalidation | skeleton exists, never run (§4) |
| T2 base-anchored | T1 product of `main`'s tip | fork pull request | proposed here |

The tiers are ordered by how much of the identity the consumer controls: none of it in T2, its own
head in T0. That ordering is the trust boundary, and it is the same ordering
`ReusableStateConsumerTrust::{LowTrust, Trusted}` already encodes — `LowTrust` consumption is
read-only and requires a `Preferred` generation. T2 is the `LowTrust` arm with the additional
requirement that the *identity itself* contain no consumer-controlled term.

### 3.4 What an attacker can do under this proposal

Stated plainly, because a conservative answer is only worth something if its residue is named.

1. **Run their own test sources against base-built binaries.** This is the intended effect, not a
   side effect. `tests/`, `.github/` and `docs/` are outside `reaches_product()`, so a fork PR
   that touches only those adopts the base product and runs its own tests against it. The
   attacker already executes arbitrary code in a fork PR job today; fork PR jobs carry a
   read-only token, no secrets, and (per `manaflow-ai/cmux#13717`) no repository variables. No
   new authority is granted.
2. **Obtain a green compile check on code that was never compiled — if and only if
   `reaches_product()` under-approximates.** This is the one genuinely new risk. If some path
   actually reaches the product but sits inside an explicit exclusion prefix, an attacker can
   change it, adopt the base product, and pass. The mitigations are structural rather than
   incidental: `reaches_product()` defaults to `return True`, so unknown paths *do* invalidate
   reuse; the recipe projection is fail-closed on unclassified job keys and unknown step names;
   and the exclusion list already carries a documented correction
   (`workers/cmux-paste-text/main.m` is compiled into the app-host bundle despite `workers/`
   being excluded), which is evidence both that this has happened and that the discipline catches
   it. The fork tier does not create this risk. It widens who can trigger it from repository
   members to anyone, which is a real widening and should be weighed as such.
3. **Consume Actions minutes and artifact bandwidth** by opening fork pull requests whose product
   inputs match the base. Bounded by existing fork concurrency limits, and consuming is strictly
   cheaper than the compile it replaces.
4. **What they cannot do:** publish any artifact a trusted consumer will adopt; influence the base
   product's identity, which is the product of an already-merged commit on the base repository;
   cause any owned machine to fetch, materialize or execute fork-authored source; reach a secret,
   a repository variable, or the merge queue. A `merge_group` consumer's producer set is unchanged
   and excludes every fork event.

The load-bearing assumption is exactly one sentence: **`reaches_product()` must not
under-approximate.** Everything else in §3 is enforced by GitHub-attested facts. If that
assumption is not one CMUX wants to stake open-internet trust on, T2 should not ship, and the
honest fallback is that fork pull requests keep compiling.

## 4. First integration step

**Glaeda produces an admitted, revalidated compiled-product artifact on owned hardware, which
CMUX CI consumes as an accelerator and never as authority.**

This is not speculative. `.github/workflows/persistent-macos-compile.yml` on `upstream/main`
already pins `GLAEDA_REF` and runs Glaeda's `scripts/apple-build` as the compile step, and
`.github/workflows/ci-macos.yml` already carries the three consumer steps. CMUX's own
`docs/ci/mac-fleet.md` records that `vars.CI_PERSISTENT_MAC_COMPILE` is unset, so **the mechanism is built and has never
run**. The integration step is to name the artifact's identity, state its failure behavior, and
turn the variable on for one mini.

### 4.1 The artifact

`persistent-mac-compile-${request_id}`: a tar of `Build/Products`, `cmux-build.log`, and
`persistent-mac-metrics.json`. Retention one day. CMUX owns its contents;
`docs/CMUX_WORKLOAD_PROFILES.md` already forbids Glaeda from reconstructing CMUX build commands.

### 4.2 Its identity

The content identity of §2.1, **plus the provenance quad the caller passes and the runner
asserts**:

```
{ source_sha, source_tree, source_parent1, head_sha }
```

The producer refuses unless `HEAD == source_sha`, `HEAD^{tree} == source_tree`, the first parent
is `source_parent1`, the second parent is `head_sha`, there is no third parent, and the worktree
is clean. That quad is the §2.2 binding made explicit: byte identity and attestation anchor are
carried as separate, simultaneously-bound terms rather than one being required to equal the
other. **The 0/112 bug is not expressible against this identity**, because nothing here asserts
that the compiled revision and the attested head are the same revision.

### 4.3 Who seals

The mini, under Glaeda admission. Glaeda writes `glaeda.cache_key`, `glaeda.invocation_identity`,
`glaeda.generation` and `glaeda.reset_reasons` into `persistent-mac-metrics.json`, alongside the
CMUX-owned `classification` (`hot` / `partially-warm` / `cold-reset`) and the source, toolchain,
`Package.resolved` and submodule identities.

Sealing grants no authority. Per the universal invariants, a surviving warm worktree on a mini
proves nothing about source, ownership or result; the seal is evidence that a hosted job will
re-check.

### 4.4 Who revalidates

The required hosted `macOS compile admission` job — the check that already gates the pull request.
It re-asserts, from its own clean checkout: the checkout is not dirty; `HEAD` and `HEAD^{tree}`
equal the expected source; the producer's recorded source identity matches; the
`Package.resolved` digest and recursive submodule identity match; Xcode version, macOS SDK
version and SDK build match; the architecture is `arm64`; the classification is one of the three
valid values; and the Glaeda receipt carries a non-empty `cache_key`, `invocation_identity`,
`generation` and a well-formed `reset_reasons` list.

The owned mini is never a required runner. It shortens the hosted job from ~21.7 min to ~7 min,
which removes offered load from a pool at rho 0.86-1.25. That is the whole lever, and it is
CMUX's `docs/ci/mac-fleet.md`'s own conclusion.

### 4.5 What happens when validation fails

Nothing special, and that is the property worth protecting. Both the download and the revalidate
step carry `continue-on-error: true`, and every subsequent step is gated on
`steps.persistent-restore.outputs.hit != 'true'`, so a failed revalidation falls through to the
normal compile. A refused product costs one download and a few seconds. It never fails the pull
request, and it never becomes the answer.

One addition is required, and it is the gap of §2.3: **a refusal must be counted.** A fleet
producing products nobody can revalidate would otherwise be indistinguishable from a fleet nobody
has asked yet — a hit rate of zero rendered as an absence of evidence. Each hosted refusal is one
`ReusableStateMetrics::unresolved_identity_attempts` against the base-anchored generation, with a
`ReusableStateUnresolvedReason`, so a hitless run of them reaches
`InvestigateUnreachableIdentity` instead of `Observe`. Without that, the first integration step
can repeat the 0/112 failure silently on owned hardware.

### 4.6 Order

1. Turn on `vars.CI_PERSISTENT_MAC_COMPILE` for the single `#13491` canary mini. Measure the
   producer hit rate rather than assuming it; `--ready-only` adopts a producer only when its
   compile is already complete, so the rate is an empirical property of arrival timing.
2. Wire the refusal counter of §4.5 before widening, so the measurement in step 1 is readable.
3. Add the base-anchored producer of §3.2 — `main`'s tip, maintained continuously on idle minis
   rather than on demand. This is the artifact T2 consumes and it costs no additional hardware,
   because the minis are idle between pull-request compiles by construction.
4. Only then evaluate T2 against §3.4's residue. It is a policy decision about open-internet
   trust, not an engineering one.

## Boundary

This document states a design. It publishes no bytes, grants no consumption authority, changes no
workflow, and does not redefine any CMUX pass/fail semantics. Glaeda's reusable-state lifecycle
retains publication authority, CMUX retains the meaning of its own workloads, and the hosted
required check retains the verdict.
