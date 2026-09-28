# Adaptive verification compiler

This slice implements the inner optimization loop from #547 as a pure deterministic reducer.

## Authority boundary

`adaptive_verification_compiler` consumes bounded verification observations and controlled-experiment results. It emits recommendations and evidence only. Artifact publication, cache mutation, execution admission, verification semantics, and workflow edits remain in their existing owners.

The reducer uses a closed candidate vocabulary:

- `reuse_exact_compiled_product`
- `retain_local_immutable_artifact`
- `reuse_dependency_generation`
- `bake_prepared_tool`
- `split_consumer_artifact`
- `run_test_without_rebuild`
- `move_static_guard_earlier`
- `parallelize_independent_checks`
- `isolate_flaky_or_hanging_suite`
- `skip_irrelevant_platform_lane`

The reducer cannot invent a new workflow mutation.

## Observation receipt

Each observation names one workload/profile and carries:

- stage and elapsed milliseconds;
- a bounded comparable-sample count when the producer supplies an aggregate median/summary;
- cold/warm/reuse class;
- bytes read, written, and transferred when measured;
- artifact-transfer backend when observed (for example a local broker, R2, or GitHub artifact transport);
- exact source and toolchain identity when available;
- semantic validation result;
- resource profile;
- optional immutable artifact, tool, suite, consumer, independent-guard, and platform-lane identity;
- exact validity inputs already established by the producer.

Supported stages cover checkout/materialization, dependency resolution, compile, link, test execution, artifact packaging, artifact transfer, restore, setup/tool installation, cleanup, and static guards.

The reducer accepts at most 512 observations and 64 experiment records per workload/profile. Mixed scopes and duplicate identifiers fail closed. The output receipt retains both bounded evidence sets in deterministic order, so the typed JSON is stable when equivalent inputs arrive in a different order.

## Exact validity contract

Reusable-work candidates are promotable only when their required validity inputs are explicit.

| candidate | required exact inputs |
| --- | --- |
| reuse exact compiled product / run test without rebuild | source tree, toolchain, SDK, architecture, build configuration, compiler flags, product schema |
| retain local immutable artifact | artifact identity, product schema |
| reuse dependency generation | lockfile, toolchain, architecture |
| bake prepared tool | tool identity, toolchain, architecture |
| split consumer artifact | artifact identity, product schema, consumer contract |

Missing inputs keep the recommendation in `observed` / advisory state and omit an exact fingerprint. Exact candidates receive a deterministic SHA-256 fingerprint over the sorted validity inputs.

## Utility

Every candidate reports the same cost fields:

`avoided work - restore/transfer overhead - publication overhead - invalidation/reset cost`

Repeated work and overhead are aggregated per run before the representative median is computed, so six repeated consumers contribute six transfers instead of one median transfer. The receipt also reports retained bytes, observed reuse-hit frequency for the candidate subject, and an explicit list of unavailable cost inputs. Missing measurements therefore remain visible in the receipt.

## Promotion lifecycle

The typed lifecycle is:

`observed -> candidate -> experimenting -> accepted -> preferred -> demoted -> retired`

Rules in this first slice:

- reusable candidates with incomplete validity remain `observed`;
- an exact candidate with zero applicable trials is `candidate`;
- a controlled experiment gains lifecycle authority only after it is bound to the exact deterministic candidate ID emitted by a prior receipt;
- unbound/class-only/subject-only experiments remain retained advisory evidence and cannot promote or demote any candidate;
- one successful exactly bound controlled trial is `experimenting`;
- two compatible successful controlled trials whose gain exceeds stated noise become `accepted`;
- three become `preferred`;
- a semantic disagreement, candidate failure/reset, or controlled regression/noise result produces `demoted`;
- retirement is an explicit terminal operator action preserving evidence.

Performance evidence never substitutes for semantic validation.

### Composition with #21 reusable-state lifecycle

The lifecycle above is the **optimizer-plan lifecycle**. It decides whether Glaeda has enough
evidence to recommend a bounded execution change. It carries advisory authority only.

Reusable candidates map into the shared `reusable_state_lifecycle` classes:

| optimizer candidate | #21 reusable-state class |
| --- | --- |
| reuse exact compiled product | `immutable_compiled_product` |
| retain local immutable artifact | `immutable_compiled_product` |
| split consumer artifact | `immutable_compiled_product` |
| run test without rebuild | `immutable_compiled_product` |
| reuse dependency generation | `prepared_dependency_generation` |
| bake prepared tool | `prepared_dependency_generation` |

A `preferred` optimizer candidate therefore means **hand this exact candidate to the owning
reusable-state family**. It does not publish bytes or grant consumption authority. A real generation
still begins under #21's reviewed publisher boundary and must progress through:

`candidate -> validated -> observed_consumers -> preferred -> demoted -> retired`

using `ReusableStateIdentityContract`, `ReusableStateMetrics`, semantic mismatch accounting,
reset/invalidation limits, and the shared retention/eviction policy.

The optimizer's utility is an experiment/candidate estimate. Once a generation exists, #21's
cumulative generation utility is authoritative for retain/demote/stop-publishing decisions. This
keeps discovery history separate from generation validity/retention history.

Repository-side classes such as guard parallelization, suite isolation, moving a static guard, and
platform-lane skipping remain recommendations. The reducer never edits workflow YAML.

## cmux proving case

The production reducer contains no cmux names or rules. The unit fixture feeds cmux-like observations through the same generic paths.

The evidence packet comes from the cmux work referenced by #547:

- #6134 reports focused test jobs dominated by repeated full-app compilation while test bodies are small. The fixture rediscovers `run_test_without_rebuild`.
- #13095 reports 90 serial guard steps taking 386 seconds, including 104 seconds of full-history checkout, and documents independent guard parallelization. The fixture rediscovers `parallelize_independent_checks`.
- #13325 records compile admission feeding six app-host shards and recent app-host products around 829 MB, with repeated GitHub artifact downloads when the R2 broker is absent. The fixture includes six consumers, both GitHub-artifact and R2 transfer identities, and exact compiled-product/local immutable-artifact candidates.
- #13325 also calls out Release as a separate full-build consumer. The fixture carries a distinct exact Release validity set (Release configuration, compiler flags, and product schema), producing a second compiled-product reuse candidate without any cmux-specific reducer branch.
- The same evidence family contains repeated hanging/retry cases and irrelevant macOS-lane work. Generic timeout and platform-relevance observations rediscover isolation and lane-skip recommendations.

### Real controlled validation retained as experimenting

cmux #13091 reports a fixed-ref, fixed-commit fresh-VM experiment on GitHub-hosted macOS with Xcode 26.6:

| arm | DerivedData / SPM | build | SwiftCompile lines |
| --- | --- | ---: | ---: |
| seed | miss / miss | 2,481 s | 13,545 |
| exact restore | exact hit / exact hit | 392 s | 2 |

That is direct evidence for exact reusable preparation. The fixture records it as one controlled compatible trial, so the lifecycle stays `experimenting` until another compatible trial satisfies the promotion rule.

### Measured rejection

cmux #12985 proposed exact reuse of bundled resource/helper products through an input/output
fingerprint. Its private Airbench campaign produced matched samples on the same commit/toolchain:

| workload | main | candidate | samples |
| --- | ---: | ---: | ---: |
| no-op warm build | 11.2 s median | 66.2 s median | 4 / 4 |
| one-file warm edit | 39.2 s | 94.9 s | 4 / 4 |

The skip path hashed all 5,901 files in the Ghostty worktree, adding roughly 55–56 seconds. The
fixture retains the four-sample no-op observation as one bounded aggregate and records the controlled
11.2 s → 66.2 s comparison. The exact-product candidate is therefore `demoted`.

The separate #13095/#13201 layer-format measurement remains useful discovery evidence:

- aggregate: 432,884,327 bytes;
- four split layers: 432,926,479 bytes;
- delta: +42,152 bytes, about 0.01%;
- one layer pack/validation: 4.252 s;
- one layer restore/validation: 2.762 s.

Those figures establish that splitting alone is size-neutral. They do not form an aggregate-vs-layer
end-to-end timing comparison, so the `split_consumer_artifact` candidate remains `candidate` /
experiment-required until selective-consumer transfer savings are measured.

## Output

`AdaptiveVerificationCompilerReceipt::render_json` emits the typed receipt, including the retained bounded observation and experiment evidence. `render_human` explains the same candidates, evidence observations, validity status, utility, trial count, lifecycle, and next action.

The tests also cover:

- generic cmux rediscovery without repository-specific production rules, including a separate Release build and both R2/GitHub transfer paths;
- fail-closed experiment applicability: an unbound trial has zero promotion authority across multiple same-class candidates;
- deterministic receipt output under reordered equivalent observations;
- advisory reuse when exact validity is incomplete;
- promotion after multiple compatible controlled trials;
- demotion after a measured regression;
- the real cmux exact-restore experiment remaining in the experimenting state;
- the measured #12985 exact-product reuse regression, while the split-only format stays experiment-required;
- identical typed-source JSON and human rendering;
- duplicate and mixed-scope evidence refusal.
