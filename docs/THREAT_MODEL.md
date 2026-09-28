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

The initial trusted computing base is macOS, Apple Virtualization Framework, Lima's controller, a pinned Ubuntu guest image, the pinned official GitHub Actions runner, the selected network-enforcement component, GitHub Actions, and Glaeda's host controller. Their exact versions and configuration are managed inputs, and security updates require deliberate rollout.

Glaeda does not independently re-prove every Linux, glibc, container-runtime, or network-stack semantic that a mature trusted component already owns. It verifies the configuration and lifecycle facts needed to use that component within this boundary.

## Required security invariants

Unless an operator explicitly selects a separately documented weaker backend:

- One potentially hostile job runs in one freshly provisioned virtual machine and the VM is destroyed after the job or any terminal failure.
- The job never executes in a Mac host namespace and receives no host filesystem mount, SSH agent, credential socket, host environment, dynamic port forward, or container-control socket.
- A GitHub runner is just-in-time/ephemeral, uniquely bound to the admitted attempt, and cannot receive a second job.
- Long-lived GitHub credentials stay on the Mac control plane. Ephemeral configuration is bounded, redacted, never public-journal data, and destroyed with the VM.
- The job has no inbound reachability. Outbound policy denies the host, private/LAN, link-local, metadata, controller, and peer-worker destinations while allowing the explicit internet access ordinary CI requires.
- Admission reserves host-wide CPU, memory, disk, and concurrency capacity before provisioning. New capacity is advertised only when the exact Lima storage filesystem has room for the full hostile-worker disk ceiling plus the fixed host reserve, and that fact is rechecked before clone. VM and wall-time limits are hard ceilings. No work means no running worker VM.
- The guest runner user has no sudo or equivalent administrative authority. Nested containers, when enabled, are rootless inside the guest and expose no host runtime socket.
- Every provision, registration, start, terminal observation, deregistration, destruction, and capacity-release transition is durably checkpointed or safely rediscovered after a crash.
- Unknown ownership, conflicting identity, stale authority, partial cleanup, and ambiguous external mutation block reuse. Recovery prefers destroying the disposable worker over adopting uncertain state.
- A job's result is not success until GitHub terminal evidence and Glaeda cleanup state are classified truthfully. Cleanup failure remains visible and retryable.
- Bounded diagnostics exclude raw repository contents, environment dumps, secrets, credentials, and unrelated host data.

## Network policy

Ordinary CI needs DNS, HTTPS, HTTP, package registries, source hosts, and sometimes explicitly approved Git SSH or service endpoints. The default policy therefore is controlled outbound internet, not a blanket offline sandbox.

The enforcement point is outside repository authority. It denies host gateway addresses, RFC1918/private ranges, IPv6 private/link-local ranges, cloud metadata and link-local services, peer-worker networks, and controller endpoints. Inbound and worker-to-worker access are denied. Project exceptions are explicit, scoped, observable, and do not grant generic LAN access. Connection, rate, and byte ceilings are hardening requirements before broad unattended use.

## Secrets and GitHub trust

Host credentials and unrelated repository credentials never enter the guest. A workflow may intentionally receive its own GitHub job token or configured repository secret; protecting the job from secrets deliberately granted by that workflow is outside Glaeda's boundary. Repository enrollment and GitHub policy should therefore avoid granting powerful secrets to unreviewed pull-request contexts.

## Failure behavior

When Glaeda cannot establish a required boundary or lifecycle fact, it does not start repository code. Once a job may have run, uncertainty triggers teardown and stale-runner cleanup rather than worker reuse. Automatic retries are bounded, preserve the same job and authority, use backoff and circuit breakers, and never exceed the host resource budget.

## Deferred high-assurance hardening

The existing R01 account, kernel, ELF, loader, filesystem, Podman, and cgroup evidence remains valuable for a future Linux/container backend and defense in depth. Completing that custom attestation graph is not required for the VM-first production path unless a concrete attack crosses the VM/control/network boundary above.

Also deferred are mutually hostile public multi-tenancy, protection from vulnerabilities in the trusted VM stack, arbitrary public-fork secret safety, cloud/Kubernetes autoscaling, deployment credentials, and automatic production deployment.

## Reporting vulnerabilities

Do not publish credential exposure or host-escape details in a public issue. Use GitHub's private security advisory flow once it is enabled for the repository.
