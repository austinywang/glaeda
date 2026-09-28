# CMUX workload profile synthetic plan

This fixture proves Glaeda can accept a caller-neutral CMUX semantic workload request while leaving CMUX semantics in the CMUX repository.

`request.json` freezes synthetic exact source identity plus `cmux.ci.guard@1` and the `cold` state class. `plan.json` resolves only the fixed `cmux-repository-profile/v1` adapter and grants no execution, placement, resource-override, or redispatch authority.

The OIDs are deliberately synthetic; this fixture exercises the request contract without claiming that a CMUX checkout or physical worker ran.

Regenerate and compare:

```sh
python3 scripts/cmux_workload_request.py plan \
  < docs/experiments/cmux-workload-profile/request.json \
  > /tmp/cmux-workload-plan.json
cmp /tmp/cmux-workload-plan.json \
  docs/experiments/cmux-workload-profile/plan.json
```
