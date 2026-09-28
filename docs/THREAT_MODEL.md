# Threat model

Glaeda automatically runs GitHub Actions jobs from operator-enrolled repositories on resources provisioned on an operator-owned Mac. Repository code is untrusted even when it comes from the operator's repository or a known open-source project.

The detailed product boundary and milestone sequence are in [Disposable autoscaling CI](DISPOSABLE_AUTOSCALING_CI.md).

## Assets to protect

- The Mac host, its operating system, user data, applications, and availability.
- GitHub App credentials, just-in-time runner configuration, workflow secrets, and unrelated credentials.
- Other workers, their jobs, state, and network traffic.
- Other devices and services on the host's private networks.
- The integrity of Glaeda's controller, durable state, capacity decisions, and public results.
- The operator's ability to understand recovery debt and stop automatic mutation.

## Adversary

Treat workflow steps, actions, checked-out source, build scripts, dependencies, test code, generated binaries, and nested containers as malicious. The adversary may try to escape, persist, steal credentials, reach the host or LAN, attack other systems, exhaust resources, confuse lifecycle observations, race cleanup, or induce controller crashes at any checkpoint.

The adversary is allowed to gain complete control of its disposable guest. Security must not depend on repository code cooperating with cleanup or preserving the runner process.

## Trusted computing base

The initial trusted computing base is macOS, Apple Virtualization Framework, Lima's controller, a pinned Ubuntu guest image, the pinned official GitHub Actions runner, GitHub Actions, and Glaeda's host controller. Their exact versions and configuration are managed inputs, and security updates require deliberate rollout.

A network-enforcement component belongs in this trusted computing base and has not been selected yet. [`DISPOSABLE_VZ_NETWORK_SELECTOR_ACCEPTANCE.md`](DISPOSABLE_VZ_NETWORK_SELECTOR_ACCEPTANCE.md) is the observation-only run that must first prove whether the current VZ usernet path exposes a worker selector stable enough for host-side enforcement. Until that component exists, the outbound half of the network policy below is a requirement rather than a control.

Glaeda does not independently re-prove every Linux, glibc, container-runtime, or network-stack semantic that a mature trusted component already owns. It verifies the configuration and lifecycle facts needed to use that component within this boundary.

## Required security invariants

A security requirement and a shipped control read identically in prose, and a reviewer deciding whether to enroll a repository cannot afford to confuse them. Every stated control below therefore carries an explicit enforcement status:

| Status | Meaning |
| --- | --- |
| `enforced` | Current `main` refuses the unsafe configuration at every cited point. |
| `partial` | Part of the statement is refused and the rest is not. The bullet says which part and cites the roadmap milestone that owns the remainder. |
| `required` | Glaeda requires this and does not yet enforce it. The bullet cites the roadmap milestone that owns the work. |

Citations name a repository path, optionally followed by `::` and the exact symbol or heading the claim rests on. `tests/threat_model_enforcement_bindings.rs` fails when a bullet omits its status, when a status is outside that closed vocabulary, when a cited path or symbol no longer exists, when an unenforced control names no owning milestone, or when an `enforced` control cites planned work instead of a control. That test proves the binding only; the owning contract tests prove the properties themselves.

Unless an operator explicitly selects a separately documented weaker backend:

- One potentially hostile job runs in one freshly provisioned virtual machine and the VM is destroyed after the job or any terminal failure. — **enforced** (`src/disposable_worker_reconciler.rs::vm_exists_before_clone_authorization`, `src/disposable_clone_runtime.rs::cleanup_destroy_not_observed`, `tests/disposable_worker_reconciler_contract.rs::terminal_cleanup_orders_vm_runner_and_capacity_release`)
- The job never executes in a Mac host namespace and receives no host filesystem mount, SSH agent, credential socket, host environment, dynamic port forward, or container-control socket. — **enforced** (`src/disposable_prepared_template.rs::fn validate_wire`, `src/disposable_template_runtime.rs::fn confirm_running_clone_target`, `tests/disposable_network_baseline_contract.rs::prepared_vz_template_keeps_the_network_selector_observation_baseline`)
- A GitHub runner is uniquely bound to the admitted attempt and cannot receive a second job. Its just-in-time registration is requested through the out-of-tree Scale Set bridge program, and no Glaeda code asserts that GitHub issued an ephemeral registration. — **partial** (`src/disposable_attempt_state.rs::fn record_assigned`, `src/disposable_runner_runtime.rs::runner_launch_name_mismatch`, `docs/ROADMAP.md::Milestone 3 — GitHub-native one-job execution`)
- Long-lived GitHub credentials stay on the Mac control plane. Ephemeral configuration is bounded, redacted, never public-journal data, and destroyed with the VM. — **enforced** (`src/github_scale_set_bridge.rs::fn load_keychain_private_key`, `src/github_scale_set_bridge.rs::MAX_JIT_CONFIG_BYTES`, `src/process.rs::fn redact`)
- The job has no inbound reachability. — **enforced** (`src/disposable_prepared_template.rs::isolation.port_forwards`, `tests/disposable_network_baseline_contract.rs::prepared_vz_template_keeps_the_network_selector_observation_baseline`)
- Outbound policy denies the host, private/LAN, link-local, metadata, controller, and peer-worker destinations while allowing the explicit internet access ordinary CI requires. — **required** (`docs/ROADMAP.md::Milestone 4 — hostile-work credential and network boundary`, `docs/DISPOSABLE_VZ_NETWORK_SELECTOR_ACCEPTANCE.md`)
- Admission reserves host-wide CPU, memory, disk, and concurrency capacity before provisioning. New capacity is advertised only when the exact Lima storage filesystem has room for the full hostile-worker disk ceiling plus the fixed host reserve, and that fact is rechecked before clone. VM and wall-time limits are hard ceilings. No work means no running worker VM. — **enforced** (`src/disposable_host_storage.rs::HOST_FREE_SPACE_RESERVE_BYTES`, `src/disposable_worker_coordinator.rs::fn advertised_capacity`, `src/disposable_worker_coordinator.rs::fn requires_host_storage`, `src/github_scale_set_delivery_consumer.rs::DISPOSABLE_JOB_MAX_MILLIS`, `tests/disposable_worker_reconciler_contract.rs::capacity_is_bounded_by_demand_global_workers_and_every_resource`)
- The guest runner user has no sudo or equivalent administrative authority. This is a build-time assertion about the pinned template, not a guest observation, and the runner integrity probe does not inspect the workload user's privileges. — **partial** (`src/disposable_prepared_template.rs::provisioning.workload_sudo`, `src/disposable_runner_runtime.rs::fn plan_launch`, `docs/ROADMAP.md::Milestone 2 — prepared disposable Linux worker`)
- Nested containers, when enabled, are rootless inside the guest and expose no host runtime socket. The disposable lane disables Lima's built-in containerd and has no other nested-container control; the rootless Podman modules belong to the trusted host-preparation lane. — **required** (`docs/ROADMAP.md::Milestone 4 — hostile-work credential and network boundary`)
- Every provision, registration, start, terminal observation, deregistration, destruction, and capacity-release transition is durably checkpointed or safely rediscovered after a crash. — **enforced** (`src/disposable_attempt_catalog.rs::fn validate_successor_of`, `src/unix_personal_worker_store/disposable_clone_transaction.rs`, `tests/disposable_worker_durable_reconciliation.rs::every_durable_checkpoint_reopens_without_duplicate_capacity_or_cleanup_loss`)
- Unknown ownership, conflicting identity, stale authority, partial cleanup, and ambiguous external mutation block reuse. Recovery prefers destroying the disposable worker over adopting uncertain state. — **enforced** (`src/disposable_worker_reconciler.rs::conflicting_vm_identity`, `src/disposable_clone_runtime.rs::cleanup_vm_control_host_conflict`, `tests/disposable_worker_durable_reconciliation.rs::stale_registration_is_bound_before_delete_and_recovery_never_uses_name_alone`)
- A job's result is not success until GitHub terminal evidence and Glaeda cleanup state are classified truthfully. Cleanup failure remains visible and retryable. — **enforced** (`src/disposable_attempt_state.rs::fn record_terminal`, `src/disposable_attempt_catalog.rs::fn validate`, `tests/disposable_attempt_state_contract.rs::runnerless_completion_requires_an_exact_prebound_job`)
- Bounded diagnostics exclude raw repository contents, environment dumps, secrets, credentials, and unrelated host data. — **enforced** (`src/disposable_service_failure_receipt.rs::fn from_static`, `src/process.rs::fn redact`, `tests/execution_receipt_schema_privacy.rs::untrusted_schema_errors_do_not_echo_attacker_controlled_field_names`)

## Network policy

Ordinary CI needs DNS, HTTPS, HTTP, package registries, source hosts, and sometimes explicitly approved Git SSH or service endpoints. The default policy therefore is controlled outbound internet, not a blanket offline sandbox.

The enforcement point must stay outside repository authority. Read the statuses below before treating any clause as a control that exists today: the guest's reachability posture is enforced by the reviewed template contract, while every outbound deny rule is still a requirement owned by Milestone 4.

- The worker exposes no inbound path: the prepared template declares no Lima network, no port forward, and SSH over vsock only, so the guest holds no host-reachable listener. — **enforced** (`src/disposable_prepared_template.rs::fn validate_wire`, `tests/disposable_network_baseline_contract.rs::prepared_vz_template_keeps_the_network_selector_observation_baseline`)
- The worker inherits no host name resolution or proxy configuration and resolves through pinned public resolvers instead. — **enforced** (`src/disposable_prepared_template.rs::isolation.host_resolver`, `tests/disposable_network_baseline_contract.rs::prepared template must define host resolver policy`)
- Outbound policy denies host gateway addresses, RFC1918/private ranges, IPv6 private/link-local ranges, cloud metadata and link-local services, peer-worker networks, and controller endpoints, while allowing the explicit internet access ordinary CI requires. — **required** (`docs/ROADMAP.md::Milestone 4 — hostile-work credential and network boundary`, `docs/DISPOSABLE_VZ_NETWORK_SELECTOR_ACCEPTANCE.md`)
- Worker-to-worker access is denied. — **required** (`docs/ROADMAP.md::Milestone 4 — hostile-work credential and network boundary`)
- Project exceptions are explicit, scoped, observable, and do not grant generic LAN access. — **required** (`docs/ROADMAP.md::Milestone 4 — hostile-work credential and network boundary`)
- Connection, rate, and byte ceilings bound a compromised guest before broad unattended use. — **required** (`docs/ROADMAP.md::Milestone 4 — hostile-work credential and network boundary`)

The current template posture removes the plumbing that would make host and LAN access convenient. It is not a deny policy: default VZ usernet egress remains reachable from the guest. Do not enroll a repository on the assumption that a hostile job cannot reach the host's private network.

## Secrets and GitHub trust

Host credentials and unrelated repository credentials never enter the guest. A workflow may intentionally receive its own GitHub job token or configured repository secret; protecting the job from secrets deliberately granted by that workflow is outside Glaeda's boundary. Repository enrollment and GitHub policy should therefore avoid granting powerful secrets to unreviewed pull-request contexts.

## Failure behavior

When Glaeda cannot establish a required boundary or lifecycle fact, it does not start repository code. Once a job may have run, uncertainty triggers teardown and stale-runner cleanup rather than worker reuse. Automatic retries are bounded, preserve the same job and authority, use backoff and circuit breakers, and never exceed the host resource budget.

## Deferred high-assurance hardening

The existing R01 account, kernel, ELF, loader, filesystem, Podman, and cgroup evidence remains valuable for a future Linux/container backend and defense in depth. Completing that custom attestation graph is not required for the VM-first production path unless a concrete attack crosses the VM/control/network boundary above.

Also deferred are mutually hostile public multi-tenancy, protection from vulnerabilities in the trusted VM stack, arbitrary public-fork secret safety, cloud/Kubernetes autoscaling, deployment credentials, and automatic production deployment.

## Reporting vulnerabilities

Do not publish credential exposure or host-escape details in a public issue. Use GitHub's private security advisory flow once it is enabled for the repository.
