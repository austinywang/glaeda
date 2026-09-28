# External execution request synthetic loop

This directory is the repository/GitHub-only proof for Glaeda #1050. It exercises the external
semantic contract without assuming any workstation is reachable.

The caller fixture is intentionally CMUX-shaped only at the correlation layer:

```text
CMUX work object
  work ref: cmux:work:1050
  request ref: cmux:exec:1050:fixture-1
  exact Git source: teamleaderleo/glaeda + commit/tree
  useful operation: verify_focused
  requested capability: credentialless_project
        |
        v
glaeda-external-execution-request/v1
        |
        v
scripts/external_execution_request.py plan
        |
        v
glaeda-external-execution-receipt/v1
  state: planned
  resolved workload: verify-focused/v1
  authority: all false
```

The request contains no CMUX cwd, local workspace id, machine id, Cloud workspace id, surface,
terminal, focus or attention fields. Those stay with CMUX.

The `planned` result says the transport-independent adapter accepted the semantic request and
resolved the reviewed Glaeda workload. It says nothing about a current node, admission outcome,
physical attempt or terminal verification result.

Run the exact fixture round trip from the repository root:

```sh
python3 scripts/external_execution_request.py plan \
  < docs/experiments/external-execution-request/cmux-request.json \
  > /tmp/glaeda-external-result.json
cmp /tmp/glaeda-external-result.json \
  docs/experiments/external-execution-request/glaeda-result.json
```

The contract suite also covers unknown/forbidden field refusal, unsupported operation/capability
refusal, caller-ref/reuse non-authority, matching terminal receipt projection, ambiguity and replay
conflict:

```sh
python3 scripts/test-external-execution-request.py
```

A physical follow-up can feed the compiled request through the existing installed
`verify-focused/v1` owner. Its local source validation, admission, durable intent/receipt and
reconciliation rules remain authoritative.
