# Disposable VZ network-selector physical acceptance

This runbook is the operator-facing evidence sequence for issue #501 under parent #498. It prepares one exact, benign physical observation of the current Lima/VZ guest packet path before Glaeda chooses any hostile-CI enforcement backend.

It is deliberately observation-only on host networking. A successful run identifies a stable worker selector, or proves that the current VZ usernet path does not expose one that is safe enough for host-side enforcement.

This document grants no firewall authority and does not widen the accepted workload boundary.

The `SMOLRUNNER_*` acceptance token and `smolrunner-prepared-template.yaml` filename below are exact existing test/template identities and remain unchanged in this documentation lane.

## Current source model

The physical run must start from the checked-in current-main prepared-template contract rather than the retired M6 firewall assumptions.

The current repository baseline proves:

- `vmType: vz`;
- Apple-silicon `aarch64`;
- `plain: true`;
- no mounts;
- no explicit Lima networks;
- no port forwards;
- `ssh.localPort: 0`;
- `ssh.overVsock: true`;
- host resolver disabled;
- proxy-environment propagation disabled;
- no fixed `61922` control-port premise.

The pinned upstream source also narrows the expected data path:

1. Glaeda pins Lima `2.2.0`.
2. Lima VZ with no configured usernet starts an **in-process gvisor-tap-vsock** netstack from its VZ driver.
3. Lima `2.2.0` pins `gvisor-tap-vsock v0.8.9`.
4. That version installs guest TCP and UDP forwarders whose external sides use ordinary Go `net.Dial("tcp", ...)` and `net.Dial("udp", ...)` host sockets.

Therefore the working hypothesis is that guest egress may be attributed by macOS to the Lima hostagent process rather than to a dedicated per-VM host interface or process. This is a hypothesis to test, not enforcement authority.

## Operator gate

Running this acceptance creates and destroys a disposable VM and emits reviewed network probes from that VM. Obtain explicit operator approval for the exact Glaeda commit and this physical experiment before execution.

The eventual checked-in harness should require exactly:

```text
SMOLRUNNER_PHYSICAL_NETWORK_SELECTOR_ACCEPTANCE=vz-usernet-observe-v1
```

Ordinary CI must never set that token or execute the physical test.

## Host-network safety boundary

During this acceptance, do not:

- load or modify PF rules or anchors;
- change routes, interfaces, DNS configuration, sysctls, packet-filter state, or socket policy;
- install packet filters or network extensions;
- capture arbitrary packet payloads;
- dump unrelated host sockets or process environments;
- kill processes by name, UID, or broad process enumeration;
- reuse an existing Lima instance or state root;
- treat a PID, process name, address, elapsed time, or Lima instance name alone as destructive cleanup authority.

Any checked-in harness must use fixed absolute child programs and argv shapes. Every direct child environment starts empty and receives only explicit allowlisted values.

## Evidence boundary

Keep only bounded metadata required to choose a selector:

- exact Glaeda commit;
- prepared-template identity and pinned Lima version;
- exact fresh test namespace identity;
- retained Lima hostagent process identity needed to correlate this run;
- guest address family and source address relevant to each probe;
- destination class, never sensitive payload contents;
- host-side socket owner/process identity relevant to the test process only;
- host local/remote socket addresses and family;
- relevant host interface/route class when independently observable;
- whether the same evidence is shared with controller/control traffic;
- whether two fresh workers can be distinguished;
- whether the evidence survives stop/start and fresh-address allocation;
- final VM/process/state absence for the acceptance namespace.

Do not retain raw process environment, Keychain data, credentials, unrelated sockets, arbitrary packet data, private host paths, or full host-wide process/network dumps.

## 0. Freeze the exact input

Before creating a VM, record:

- exact Glaeda source commit;
- exact `examples/lima/smolrunner-prepared-template.yaml` identity;
- prepared-template manifest identity including Lima `2.2.0`;
- exact acceptance token value;
- the unique private test root / `LIMA_HOME` / instance namespace selected for this run.

Reconfirm that the ordinary repository regression `disposable_network_baseline_contract` passes for the same checkout.

If the prepared-template networking contract differs from the guarded baseline, stop and review the changed packet-path assumptions before continuing.

## 1. Establish the idle host baseline

Before guest traffic, create the fresh disposable VZ worker through the current prepared-template machinery and wait for the accepted ready state.

Record only observations scoped to the exact acceptance process/namespace:

- retained Lima hostagent process identity;
- its current TCP/UDP socket set relevant to this test;
- current guest IPv4 address and IPv6 availability;
- the host route/interface classes relevant to the guest path;
- evidence that SSH/control uses the current vsock boundary and has no fixed `61922` dependency.

Do not use a host-wide socket dump as the receipt. The observation must be bounded to the exact retained process or another independently proven selector candidate.

## 2. Public IPv4 TCP probe

From the benign guest, open one bounded public IPv4 TCP connection to a reviewed test destination.

While it is live, freshly observe the host side and record:

- which host process owns the corresponding external socket;
- host local/remote addresses and family;
- relevant interface/route class;
- whether that ownership is unique to the worker or shared with Lima/controller operations.

Close the probe and prove the corresponding external socket disappears before moving to the next class.

## 3. Public UDP / DNS probe

Generate one reviewed public DNS lookup or other bounded UDP probe from the guest.

Record the same host-side ownership and route evidence as for TCP. DNS must not be assumed to follow the TCP selector merely because both succeed.

If the guest uses a local virtual DNS service, follow the request far enough to identify the external host socket or resolver path that actually leaves the Mac.

## 4. IPv6 probe

If both guest configuration and host routing support IPv6, repeat one TCP and one DNS/UDP observation over IPv6.

If IPv6 is unavailable, record that as an explicit capability result. Do not silently treat an IPv4-only receipt as IPv6 enforcement evidence.

## 5. Denied-destination path observations

Without changing host firewall state, issue bounded benign connection attempts from the guest toward representative destination classes required by M4:

- host loopback/control reachability;
- RFC1918 private address space;
- link-local address space;
- metadata-style destinations.

The goal is packet-path observation only. Record which selector candidate the attempt would traverse if enforcement existed, plus whether any current control mechanism bypasses that path.

A connection succeeding during this pre-enforcement experiment is not permission to run hostile workloads; record the observation and continue only within the benign fixture.

## 6. Two-worker distinguishability

Run this step only if the one-worker evidence shows that guest traffic collapses into a hostagent/process/UID identity that may be shared across workers or controller traffic.

Create the smallest safe second fresh disposable worker fixture and repeat one TCP or DNS observation concurrently.

Determine whether the two workers are distinguishable by an independently observable stable value such as:

- distinct process identity;
- distinct socket/source identity;
- distinct interface;
- distinct guest address/subnet with safe ownership binding;
- another immutable selector exposed by the current backend.

Do not invent a selector from a mutable name or transient PID/address alone. Any proposed value must have an ownership story that prevents a foreign successor from inheriting permissive policy.

## 7. Restart and address-reuse observation

For any promising selector, perform the smallest accepted stop/start or fresh-clone cycle needed to answer:

- whether the selector survives ordinary restart;
- whether a new address can replace the old one;
- whether a stale value can become associated with a different worker;
- whether policy could be installed before guest code and removed only after exact absence.

This step still performs no network enforcement.

## 8. Choose one explicit outcome

The receipt must conclude with exactly one of these outcomes.

### Host selector proven

Name the candidate selector and provide bounded evidence that it:

- covers guest public TCP;
- covers guest UDP/DNS;
- covers IPv6 when available;
- distinguishes workers where peer isolation requires it;
- excludes controller/control traffic;
- requires no fixed TCP escape hatch for vsock control;
- has an exact ownership/reuse story across restart and cleanup;
- can be observed independently before and after future enforcement.

Only after independent review of that receipt should #498 split an enforcement implementation.

### Host selector disproven

Record the concrete reason current VZ usernet cannot provide a sufficiently worker-specific selector. For example, if all guest TCP/UDP egress becomes native sockets of a shared Lima hostagent identity with no stronger per-worker boundary visible to macOS, say so explicitly.

Then evaluate a mature dedicated-network or guest-boundary enforcement backend in a separate issue. Do not force the retired UID-scoped PF design onto an unsuitable packet path.

## 9. Cleanup

After the final observation:

1. close all guest probes;
2. freshly prove the exact acceptance-owned mutation/process state is quiescent;
3. remove only the exact fresh disposable VM namespace using existing ownership checks;
4. prove the VM is absent;
5. prove acceptance-specific host processes/sockets are absent;
6. remove only the exact private acceptance state root after its identity is reconfirmed.

If quiescence, VM ownership, process identity, or cleanup authority is ambiguous, retain the exact recovery namespace and stop. Do not broaden deletion authority to make the test clean up.

## Completion receipt

The #501 completion comment should contain only:

- exact Glaeda commit accepted;
- prepared-template/Lima identity;
- acceptance namespace identity in bounded public form;
- TCP selector observation;
- UDP/DNS selector observation;
- IPv6 result;
- host/private/link-local/metadata path result;
- two-worker distinguishability result when required;
- restart/address-reuse result;
- explicit `Host selector proven` or `Host selector disproven` outcome;
- VM/process/state cleanup result;
- confirmation that host firewall/network configuration was never mutated.

The receipt is research authority for backend selection only. Hostile repository execution remains blocked until the selected enforcement backend, live observation gate, cleanup semantics, and hostile fixtures receive their own reviewed implementation slices.
