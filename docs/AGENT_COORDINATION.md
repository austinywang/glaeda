# Agent coordination protocol

This document defines how multiple implementation agents coordinate work on Glaeda. The goal is to keep delegation observable, bounded, and recoverable. An agent must never disappear into an indefinite scheduled wait while the coordinator assumes progress is still happening.

## Core rule

Repository work is performed in the active work session and ends with an observable repository artifact or an explicit blocked result.

Scheduled tasks, reminders, recurring checks, and conditional watches are not an agent-to-agent signalling mechanism. Do not create them to wake another agent, poll another agent, wait for a review, or resume delegated implementation. Scheduling is reserved for a future user-facing reminder or monitoring request that the human operator explicitly asked for.

## No coordination locks

Do not create empty branches, refs, pull requests, commits, labels, or other repository objects merely to reserve review, diagnosis, repair, preflight, or coordination work.

- `claims/*` refs are deprecated and carry no ownership or authority.
- Review is non-exclusive. Any eligible implementation-independent agent may review an exact head without acquiring a slot.
- A short review-intent comment is allowed for visibility, but it is not a lock and must not stop another eligible reviewer from proceeding.
- The first complete valid exact-head verdict may satisfy the review gate. Later reviewers should stop when they have no materially new finding.
- Implementation and repair claims are ordinary repository-visible comments naming the exact base, canonical delivery branch, file fence, exclusions, checks, and completion signal.
- A canonical implementation or repair branch is the delivery surface for source changes, not a mutex or seat-reservation mechanism.
- Avoid source collisions by refreshing the canonical roster, open pull requests, current heads, and active file fences before mutation. When another worker is already editing the same fence, coordinate, narrow scope, or choose another lane.
- Do not report `no work` solely because another reviewer has expressed interest. Continue useful non-conflicting review, diagnosis, integration, or implementation work.

Independence requirements still apply: an author must not issue the independent final acceptance for final repair bytes they wrote.

## Required delegation contract

Before delegating implementation, record all of the following:

- exact repository and base commit SHA;
- issue or objective;
- files or subsystem owned by the agent;
- files and authority explicitly outside its scope;
- concrete deliverable, normally a branch, commit, pull request, review, merge, or issue comment;
- required checks;
- completion signal;
- bounded recovery rule when the signal does not appear.

A useful completion signal is externally observable and exact, for example:

- branch name plus head commit SHA;
- pull request number plus exact tested head SHA;
- review or issue comment that names the exact commit inspected;
- workflow run IDs and conclusions for that exact commit;
- merge commit SHA tied to the exact accepted pull-request head.

A private chat state, an agent saying it is waiting, or an unspecified future event is not a completion signal.

## No passive waiting

An implementation agent has only three valid terminal states:

1. **Completed:** publish the requested artifact and report its exact identity.
2. **Blocked:** report the exact missing dependency, current branch or commit, partial work, and the action required to unblock it.
3. **Failed:** report the exact failure and leave the repository in a reviewable or clean state.

`Waiting`, `scheduled`, `listening`, and `monitoring` are not terminal states for repository implementation.

The coordinator must continue independent work rather than pause for delegated agents. It may check the declared completion signal at most twice during the active coordination pass. When the artifact is still absent after the second check, classify the delegation as stalled and immediately take over, reassign, or reduce the scope. Do not create a scheduled task as a third check.

## Merge authority

Repository merge is an ordinary reversible repository action. A separate human merge approval is never required.

An agent may merge when:

- the pull request is tied to an exact current head;
- required checks for that exact head have passed;
- the complete final diff remains inside the declared file and authority fence;
- the applicable review requirement is satisfied;
- GitHub reports the candidate mergeable;
- the merge operation names the expected head SHA so head movement fails closed.

Routine low-risk changes may be self-reviewed and merged by their author. High-risk changes require an independent exact-head acceptance, and the acceptance reviewer does not author the final repair it accepts. After acceptance, any eligible agent may merge the accepted head.

Human approval can remain a prerequisite for effects beyond repository merge, such as credential configuration or rotation, operator-machine service installation or removal, paid-capacity changes, external publication or contact, release signing, destructive non-test data changes, or irreversible migrations without a proven recovery path.

After merge, update the canonical roster and dependent lanes immediately. Any head movement before merge expires prior acceptance.

## Dependency signalling

An agent may depend on another branch or pull request only when the dependency and signal are explicit. Use repository-visible signals:

- merged commit appears on `main`;
- named pull request reaches an exact reviewed head;
- named workflow succeeds on that exact head;
- named issue or review comment is posted.

The waiting agent must not remain open-ended. It should complete all independent work, report the dependency, and stop. The coordinator resumes or reassigns the dependent work after observing the signal.

## Recovery from a stalled agent

When an agent becomes unresponsive or leaves scheduled work behind:

1. Inspect its branch, pull request, comments, and workflow runs.
2. Record the last exact observable head and whether any partial work is usable.
3. Ignore or cancel scheduled coordination tasks; they carry no repository authority.
4. Refresh `main` and every relevant branch before continuing.
5. Take ownership or reassign from the last safe commit.
6. Avoid duplicate mutations and never assume an unseen agent action completed.
7. Run the normal checks and review the complete final diff before declaring recovery complete.

## Scheduling policy

A scheduled task is acceptable only when all of these are true:

- the human operator explicitly requested future or recurring delivery;
- the task observes an external condition or delivers a user-facing reminder;
- its cadence and timezone are explicit;
- its notification condition is explicit;
- its stop condition or continued value is clear;
- repository progress does not depend on another agent receiving it.

Never schedule a task merely because an agent cannot continue in the current response. Never use a scheduled task to substitute for a direct status report, explicit block, branch handoff, or immediate best-effort completion.

## Handoff template

Use this compact handoff for delegated repository work:

```text
Repository: owner/repo
Base: <exact SHA>
Objective: <one concrete outcome>
Owned scope: <files/subsystem>
Excluded scope: <authority and files>
Deliverable: <branch/PR/review/merge/comment>
Checks: <exact commands or workflows>
Completion signal: <observable artifact and exact identity>
Recovery: after two missing-signal checks, coordinator takes over or reassigns
```

## Anti-patterns

Do not:

- create empty claim branches or refs for review, diagnosis, repair, preflight, or coordination;
- treat a review-intent comment, assignee, label, callsign, or model name as an exclusive lock;
- stop useful work solely because another reviewer exists;
- add a human merge-approval gate after the accepted review and exact-head checks are complete;
- tell another agent to wait indefinitely for an unspecified signal;
- create reminders or condition watches to coordinate implementation agents;
- leave an agent in a listening state after its independent work is complete;
- rely on hidden chat state as proof of completion;
- keep polling without a fixed limit;
- block the coordinator when independent work remains;
- assume a scheduled task was received, executed, or understood;
- treat silence as success.

The repository is the coordination surface. Exact commits, pull requests, comments, merges, and workflow conclusions are the signals.