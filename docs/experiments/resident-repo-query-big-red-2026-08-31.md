# Resident repository query: Big Red controlled result

Status: `repo-query/v1` merged through PR #977 as `f5f67bd`. The measurements are performance
observations, not source-validity, publication, merge, or result-reuse authority.

## Landed slice

`glaeda-repo-query` runs one fixed request against an explicit canonical checkout and complete
base, head, and expected head-tree object IDs. It returns:

- canonical repository, object format, base, head, head tree, and merge base;
- ancestry, commit count, bounded per-file numstat, and complete-patch digest;
- the patch itself only when it fits the requested ceiling;
- optional literal exact-tree grep, bounded tree blobs, bounded path history, and object facts;
- completeness/omission status plus profile, request, Git-process, byte, and timing identity.

The process boundary clears ambient Git configuration, credentials, hooks, lazy fetch, replacement
objects, external diff/text conversion, pager, submodule recursion, and protocol access. It
reobserves checkout, Git-directory, and origin identity before returning. It accepts no refs,
arbitrary Git arguments, shell, network, fetch, checkout, mutation, publication, retention, or
result-reuse authority. The complete response is capped at 128 KiB. Any individual Git stream over
the fixed 1 MiB capture boundary fails the query with a bounded error; very large patches therefore
do not yet return a streaming digest or typed omission.

## Complete review-evidence loop

The Big Red dogfood asked one real next-action question of exact candidate
`697cae359a4389877adbbe9a8225e70ada1ab9f0`, tree
`75429d618ebbd125045c39bd4213993408193acf`: inspect topology/diff, find timed-executor use, read the
complete CLI and CLI-test blobs, inspect `src/process.rs` history, and classify known and missing
objects.

| arm | samples | median | worker-visible/result bytes | outer calls | result |
| --- | ---: | ---: | ---: | ---: | --- |
| GitHub baseline | 3 | 4444 ms | 75,713 after optimistic projection; 227,827 transported | 5 | lacked equivalent missing-object size evidence |
| Glaeda resident | 5 | 39.008 ms internal; 40 ms wrapper | 21,452 | 1 | complete for the registered next action |

The landed path was about 114 times faster, removed four remote calls, and was 71.7% smaller than
the optimistic GitHub projection. Its five internal samples were 38.670, 38.962, 39.008, 50.207,
and 64.603 ms; median maximum RSS was 8,268 KiB. The request used 28 bounded local Git processes
and consumed 95,720 Git stdout bytes without exposing those intermediate results to the worker.
CPU time and disk I/O were not retained for this run. The profile adds no cache or write path;
missing objects, capture-bound overflow, or identity drift fail closed to another evidence source.

Profile generation:
`sha256:f575e0e3cd40e54ca4f868f99777e40386a2fe909cb91362f777e8881302ef65`.
Request digest:
`sha256:f0a870a5be725b08496f19e0863180a9ff42ef475ff369d356030e63f69b716d`.

## Direct-local calibration

An independently implemented narrower candidate reproduced the exact PR #974 review facts under a
four-arm rotating serial control: two warmups plus ten measured samples per arm, exact equality of
object coordinates, ancestry, commit count, changed-file facts, aggregate numstat, and normalized
patch hunks.

| arm | median | p90 | model-visible bytes | logical outer calls |
| --- | ---: | ---: | ---: | ---: |
| naive remote | 3431.93 ms | 3760.88 ms | 51,201 | 4 |
| optimized remote | 936.95 ms | 987.56 ms | 4,679 | 1 |
| direct local | 10.51 ms | 11.64 ms | 4,679 | 6 |
| discarded narrow Glaeda candidate | 16.58 ms | 18.60 ms | 5,534 | 1 |

This calibration is not attributed to the broader landed binary. It preserves the important
negative control: a typed wrapper did not make Git faster or smaller. The discarded candidate was
1.58 times slower and 18.27% larger than a perfectly composed local projection. The product win is
one stable, bounded, identity-bearing call that prevents repeated remote reads and procedural
rediscovery.

Calibration source `b37a40482c09980b65b351bff79480ebefa680e1`, tree
`9958ff360fe13ca89c32fc56b26acbb2ee6cd863`; [path-free raw
receipt](resident-repo-query-narrow-control-receipt.json) digest
`sha256:33d75f3408faf0fa235a2969729631b0547d1381184e365cfd6b366c7d9755e3`.

## Ambient-context deletion

Full-loop validation found that named verification profiles inherited the caller's process umask.
Under ambient `0002`, fixtures became group-writable and safety checks failed; under `0022` they
passed. Verification now binds child file creation to `0022` and records that value in plan and
receipt documents. Agents no longer need to know or reproduce this machine fact.

`AGENTS.md` and the README also delegate the exact required phase argv to
`./scripts/verify required --plan-json`; they no longer carry a duplicated eight-command recipe.
The executable named profile remains the single current procedure.
