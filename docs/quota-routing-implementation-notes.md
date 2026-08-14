# Quota-aware automatic runner routing — implementation handoff

Status: implementation plan, no feature code landed yet  
Source specification: `Project Specification_ Quota-Aware Automatic Runner Routing for Cezar.md`  
Target providers for automatic routing: Claude Code and Codex

## Objective

Add an opt-in `auto` runner selection that resolves a concrete provider immediately before each
agent step. Claude is normally preferred, Codex is the fallback, and provider eligibility is
re-evaluated for every new auto-routed step. A fallback choice is not sticky: after Claude's reset
is confirmed, the next step returns to Claude automatically.

If no provider is eligible, preserve the task as queued, display why it is waiting, and wake it
when telemetry changes or a known reset passes. If a running auto-selected provider terminates on
a confirmed quota failure, continue the same workflow step in the same worktree with another
eligible provider before consuming the workflow's `onFail` budget.

Existing explicit runner behavior must remain unchanged when routing is disabled or a task/step is
pinned to `claude`, `codex`, `opencode`, or `pi`.

## Fork-specific conclusions

### `auto` is a selection policy, not a backend

`packages/cezar/src/core/agent-runner.ts` defines `RUNNER_IDS` as executable backends and
`runner-factory.ts` assumes every accepted value can construct an `AgentRunner`. Do not append
`auto` to `RUNNER_IDS`.

Introduce a separate authored-selection type:

```ts
type RunnerId = 'claude' | 'codex' | 'opencode' | 'pi';
type AutoProvider = 'claude' | 'codex';
type RunnerSelection = RunnerId | 'auto';
```

Use `RunnerSelection` only for project/task/workflow/CLI/composer policy. Continue using `RunnerId`
for runner construction, provider authentication, models, sessions, profiles, handoff commands,
and persisted execution affinity.

### Reuse the existing usage-limit lifecycle

The fork already contains a mature runtime-limit mechanism:

- `core/usage-limit.ts` extracts credible reset instants conservatively.
- `RunRecord.autoResumeAt` persists a known reset appointment.
- `RunManager.accountHolds()` publishes provider-account holds.
- `WorkspaceSemaphore.accountHolds()` unions holds across all project managers.
- `RunManager.reconcileAutoResumes()` rebuilds timers after restart.
- `RunManager.rescueStalledQueue()` prevents a silent queue wedge.

Quota routing must extend these guarantees rather than create a second scheduler. Preserve the
existing critical transition order:

1. publish provider/account exhaustion;
2. persist the blocked state;
3. release the provider reservation;
4. release the workspace task slot;
5. wake queues.

Publishing exhaustion after releasing capacity recreates the quota stampede already fixed by the
auto-resume work.

### Usage is provider-account scoped

The fork supports multiple Claude and Codex profiles through
`workspace/agent-profiles.ts`. Although automatic account switching is out of scope, the router
must query the account the current project would use for each candidate provider.

Internally key cache and hard-limit state by:

```ts
interface ProviderAccountRef {
  provider: AutoProvider;
  profileId: string;
}
```

Auto evaluates one resolved account per provider; it never searches other accounts for quota.
Reject a task-level `agentProfile` combined with `runner: auto` in MVP because the profile id is
provider-specific and becomes ambiguous if the other provider is selected.

### Codex has a structured quota protocol

The installed Codex app-server protocol exposes:

- `account/rateLimits/read`;
- `account/rateLimits/updated`;
- primary and secondary windows with `usedPercent` and reset timestamps;
- credit/spend-control state;
- structured `usageLimitExceeded` turn failures.

Use that protocol instead of reading `auth.json`, calling a private HTTP endpoint, or scraping
CLI prose. Add a narrow zod parser for only the fields Cezar consumes. Keep the adapter replaceable
because app-server protocol versions can evolve.

### Claude telemetry is the fragile adapter boundary

Claude Code currently provides no equivalent public app-server rate-limit request in this fork.
The reference implementations use Claude Code's local OAuth credential against
`https://api.anthropic.com/api/oauth/usage`.

The Claude adapter must:

- resolve the selected `CLAUDE_CONFIG_DIR` profile;
- support the platform's actual Claude credential store, including macOS Keychain where needed;
- extract only access token and expiry into memory;
- use Node's built-in `fetch` with a bounded timeout;
- send the token only to Anthropic's fixed HTTPS origin;
- never log, cache, persist, emit, or include it in an error;
- return sanitized `auth_error` on 401/403;
- return `unknown` for endpoint, network, or schema failures;
- never use the Messages API fallback, because monitoring must not spend a model request.

## Verified integration map (2026-08-14)

This section is an implementation map of the current fork, not a proposed abstraction. It exists
to keep a change from updating the obvious new-run path while leaving a continuation, restart, or
second project context on the old concrete-runner path.

| Concern | Current source of truth | Change boundary |
| --- | --- | --- |
| Executable backend ids | `packages/cezar/src/core/agent-runner.ts` (`RUNNER_IDS`, `RunnerId`) | Keep concrete. Define `RunnerSelection` beside or above this seam; do not feed `auto` into `createRunner()`. |
| Authored task/config runner | `packages/cezar/src/config.ts`, `packages/cezar/src/workspace/config.ts`, `packages/contract/src/workspace.ts`, `packages/contract/src/workflows.ts`, `packages/cezar/src/workflows/types.ts` | Widen only these authored-selection schemas and validate Auto-specific incompatibilities at their request/config boundary. |
| Persisted run and step affinity | `packages/cezar/src/runs/store.ts` (`runRecordSchema`, `stepStateSchema`) | Keep `runner`/`backend` concrete and additive-safe. Persist requested selection separately; old records must still parse and normalize `claude-cli`. |
| New workflow execution | `packages/cezar/src/workflows/run.ts` (`execute`, `runAgentStep`) | Resolve/reserve at agent-step spawn, before model/profile environment resolution. Do not resolve once at `execute` and reuse it for later steps. |
| Follow-up/Continue execution | `packages/cezar/src/workflows/run.ts` (`continueRun`, `runContinuation`) | This is a distinct construction path. It needs the same resolver dependency and Auto validation, while a cross-provider continuation must start fresh rather than resume a foreign session. |
| Queue, quota hold, and restart recovery | `packages/cezar/src/workflows/run.ts` (`pump`, `recover`, `reconcileAutoResumes`, `rescueStalledQueue`, `accountHolds`) plus `packages/cezar/src/workspace/semaphore.ts` | Extend the existing liveness system; preserve its publish/persist/release/wake ordering. A quota park needs a durable queue/recovery representation rather than a new terminal status. |
| Existing limit parsing | `packages/cezar/src/core/usage-limit.ts` | Reuse only its conservative runtime-failure/reset evidence. Provider telemetry and routing policy stay in the new quota layer. |
| Per-provider account resolution | `packages/cezar/src/workspace/agent-profiles.ts` and `packages/cezar/src/core/agent-profiles.ts` | Resolve one selected profile per candidate provider. Do not infer a Codex profile from a Claude profile override. |
| Shared process wiring | `packages/cezar/src/index.ts` (headless and server boot) and `packages/cezar/src/server/project-context.ts` | Build one usage service/coordinator beside the shared `WorkspaceSemaphore`; inject it into the boot manager and lazily built project managers. |
| HTTP contract and routes | `packages/contract/src/*`, `packages/cezar/src/server/validators.ts`, `packages/cezar/src/server/server.ts` | Contract first; add routes through a chained workspace-level family. Do not add loose Hono route statements or project aliases for workspace data. |
| Cockpit live transport | `packages/cezar/src/server/ws.ts`, `packages/web/src/api/ws.ts`, `packages/web/src/api/global-events.tsx` | Add one demand-driven workspace topic. Local root owns its subscription; remote mode reconciles through HTTP/SSE. |

### Lifecycle invariants to preserve

1. `RunRecord.runner` and `StepState.backend` identify an executable backend and may be consumed by
   model normalization, session resume, profile resolution, and the "open in CLI" command. They
   cannot become `auto`.
2. `RunManager` currently owns two agent-launch sites: normal workflow steps and `Continue`.
   Any injected coordinator, active-run state, lease cleanup, and emitted audit event must reach
   both sites.
3. `autoResumeAt` currently represents a *failed* explicit run that has a scheduled retry. Do not
   overload it silently for a queued Auto run without first updating every reader that assumes
   failed status, including store archival/read-state behavior and semaphore account holds.
4. `ProjectContexts` lazily creates a manager per registered root. A coordinator created inside a
   manager would make provider concurrency project-local and permit a multi-project quota
   stampede; it must be process-shared.
5. The queue watchdog is a liveness guarantee, not merely diagnostics. A parked Auto run must have
   at least one default-on wake source that survives restart, or the watchdog must deliberately
   rescue it.

### Phase-zero acceptance check

Before feature code begins, add focused characterization tests for the above boundaries: a normal
workflow launch, an explicit Continue, a recovery-created manager, and two project managers sharing
one semaphore. The first routing change should make those tests exercise the same injected fake
coordinator, proving that the dependency reaches every construction site.

## Target architecture

```text
profile + authentication resolution
              |
              v
ProviderUsageService
  - ClaudeUsageAdapter
  - CodexUsageAdapter
  - TTL / in-flight dedup
  - sanitized cache persistence
  - reset timers and change events
              |
              v
QuotaCoordinator
  - pure policy evaluation
  - serialized selection + reservation
  - provider concurrency counts
  - runtime hard-limit overrides
              |
              v
RunnerResolver
  explicit selection -> existing concrete runner
  auto selection     -> QuotaCoordinator lease
              |
              v
RunManager
  - agent-step dispatch
  - durable provider wait
  - same-step provider failover
              |
              v
existing AgentRunner implementations
```

Construct exactly one `ProviderUsageService` and one `QuotaCoordinator` per Cezar process. Share
them between the boot `RunManager` and every lazily built project context, like the existing
`WorkspaceSemaphore`.

## Data model

### Provider usage

Public contract schemas should live in `packages/contract`, with server-only policy state under a
small `packages/cezar/src/core/quota/` directory.

Required normalized fields:

```ts
type UsageWindowKind = 'short' | 'long' | 'model' | 'unknown';
type ProviderQuotaHealth =
  | 'available'
  | 'soft_exhausted'
  | 'hard_exhausted'
  | 'auth_error'
  | 'unavailable'
  | 'unknown';

interface ProviderUsageWindow {
  id: string;
  kind: UsageWindowKind;
  label: string;
  usedPercent: number | null;
  remainingPercent: number | null;
  resetsAt: string | null;
  hardLimitReached: boolean;
}

interface ProviderUsageSnapshot {
  provider: AutoProvider;
  profileId: string;
  health: ProviderQuotaHealth;
  fetchedAt: string;
  source: string;
  stale: boolean;
  windows: ProviderUsageWindow[];
  error?: { code: string; message: string };
}
```

Do not expose adapter metadata or raw provider payloads through the public contract.

### Workspace policy

Add top-level `quotaRouting` to `~/.cezar/config.json`:

```json
{
  "quotaRouting": {
    "enabled": false,
    "providerOrder": ["claude", "codex"],
    "refreshIntervalSeconds": 60,
    "cacheTtlSeconds": 30,
    "requestTimeoutSeconds": 8,
    "unknownUsagePolicy": "allow",
    "providers": {
      "claude": {
        "enabled": true,
        "stopNewWorkAtPercent": 90,
        "longWindowStopAtPercent": 95,
        "resumeBelowPercent": 80,
        "maxConcurrent": 1
      },
      "codex": {
        "enabled": true,
        "stopNewWorkAtPercent": 90,
        "longWindowStopAtPercent": 90,
        "resumeBelowPercent": 80,
        "maxConcurrent": 1
      }
    }
  }
}
```

Follow workspace config rules: optional/defaulted keys, `.catch`, `.passthrough()` at every object
level, and merge writes. Invalid hand-edited policy degrades to disabled defaults and never blocks
boot. The PUT API is strict and returns specific validation errors.

### Requested versus resolved runner

Keep `RunRecord.runner` and `StepState.backend` concrete because they own session affinity, model
mapping, account holds, active-provider display, and CLI resume.

Add optional fields:

```ts
RunRecord.requestedRunner?: RunnerSelection;
StepState.requestedRunner?: RunnerSelection;
RunRecord.blockedReason?: {
  type: 'provider_quota';
  providers: AutoProvider[];
  retryAt?: string;
};
```

New auto run before dispatch:

```text
requestedRunner = auto
runner = absent
```

After dispatch:

```text
requestedRunner = auto
runner = claude | codex
```

Each step also records its requested selection and actual backend. All fields must remain optional
for old `runs.json` compatibility.

### Durable workflow checkpoint

A run may hit quota waiting after earlier workflow steps or check retries have already completed.
Restarting that run from step zero can duplicate work and consume retry budgets incorrectly.

Persist a small optional checkpoint containing only what is needed to restart the workflow loop:

- next step id;
- bounded retry counts;
- attempted provider plus its recovery/reset generation;
- whether the failover continuation preamble is required;
- bounded, secret-redacted check failure context when a retry loop requires it.

Clear the checkpoint when the workflow completes or is explicitly restarted by a human. Ensure
the API either includes a deliberately sanitized schema for it or strips it before returning run
records; do not accidentally expose raw check output.

## Pure routing algorithm

Implement the decision as a synchronous, deterministic function. Network refreshes and writes do
not belong in it.

For every auto step, evaluate providers from the start of `providerOrder`. This is deliberately
non-sticky: a prior Codex selection does not change the next evaluation order.

For each provider:

1. routing policy enables it;
2. existing workspace provider settings do not disable it;
3. its CLI is installed;
4. the selected account is authenticated;
5. it has not already failed for this step in the current recovery generation;
6. its provider concurrency ceiling has room;
7. its snapshot is fresh enough or unknown policy is applied;
8. no hard limit is active;
9. short-window use is below the start threshold;
10. long/model-window use is below the long threshold.

Threshold behavior:

- stop when usage is `>=` the configured stop threshold;
- after soft exhaustion, remain exhausted until usage is `< resumeBelowPercent` or a confirmed new
  window/reset generation appears;
- if a known reset time has passed, force-refresh before choosing a provider;
- if Claude is recovered, choose it before Codex again;
- unknown telemetry defaults to allowed only when CLI/auth state is otherwise usable.

The result should include `selected`, `wait`, or `error`, considered-provider reasons, and the
earliest credible reset time. It must contain no credentials or raw response data.

## Usage service and coordinator

### `ProviderUsageService`

Responsibilities:

- adapter registry;
- cache per provider account;
- TTL and freshness calculation;
- one in-flight refresh promise per account;
- force refresh;
- previous/current snapshots for reset detection;
- sanitized atomic persistence at `~/.cezar/provider-usage.json`, mode `0600`;
- load persisted snapshots as stale;
- schedule the earliest known reset and refresh after it;
- publish only meaningful snapshot changes;
- dispose timers and persistent Codex app-server children.

A stale available snapshot may render in the UI but may not authorize new auto work when a fresh
check can reasonably be performed.

### `QuotaCoordinator`

Responsibilities:

- hold a mutex/promise chain around refresh, selection, and reservation;
- resolve the project's selected account for each candidate provider;
- call the pure router;
- reserve provider capacity atomically;
- expose a lease whose `release()` is idempotent;
- apply immediate hard-limit overrides from runtime failures;
- clear overrides only after a confirmed recovery generation;
- wake every project queue when eligibility or reservation capacity changes.

Reservations are process-wide. Cross-process usage cache is advisory; the MVP does not need an
inter-process concurrency lock.

## Run lifecycle integration

### Agent-step dispatch

Refactor `RunManager.runAgentStep()` so concrete resolution occurs immediately before:

1. model normalization;
2. account environment resolution;
3. step backend/profile persistence;
4. runner construction;
5. process launch.

Suggested lease:

```ts
interface ResolvedRunnerLease {
  backend: AutoProvider;
  profileId: string;
  decision: RoutingDecision;
  release(): void;
}
```

Persist and emit `runner_route_decision` before the spawn. Release the lease in `finally` after
normal completion, error, cancellation, timeout, or a synchronous spawn failure.

Apply the same resolver rules in `runContinuation()`. It is a separate runner construction site
and must not be left on the old concrete-only path.

### Durable quota wait

When the router returns `wait`:

1. return the current agent step to `pending`;
2. persist the workflow checkpoint;
3. set `status: queued` and `blockedReason: provider_quota`;
4. preserve the branch and existing worktree;
5. remove the run from `active` without terminal cleanup;
6. release provider/workspace capacity;
7. leave the queue entry in place but ineligible until a quota wake occurs.

Add a dedicated `parkForProviderQuota()` transition. Do not call `dropActive()`: that function
currently schedules explicit-run auto-resume, enforces terminal retention, removes temp state, and
otherwise assumes a terminal transition.

On wake or restart:

- force-refresh the relevant accounts;
- acquire a provider reservation before clearing `blockedReason`;
- restore the recorded worktree rather than creating a new one;
- restore workflow position and retry state;
- continue the same step.

Wake sources:

- periodic refresh while auto work is pending;
- earliest known reset timer;
- provider reservation release;
- manual refresh;
- routing configuration change;
- authentication recovery;
- process restart.

Update the queue watchdog so a real quota reset timer is a legitimate future wake source, while a
blocked queue without any timer, refresh, active work, or reservation still gets rescued.

### Runtime quota failure and failover

Add an optional normalized kind to runner errors:

```ts
type RunnerFailureKind =
  | 'quota_exhausted'
  | 'rate_limited'
  | 'auth'
  | 'network'
  | 'process'
  | 'tool'
  | 'model'
  | 'cancelled'
  | 'unknown';
```

Classification priority:

1. structured provider protocol;
2. exact provider marker and reset extraction;
3. narrow provider-specific exit/error evidence;
4. conservative text fallback;
5. unknown.

For an auto-selected step with a confirmed quota failure:

1. report hard exhaustion to the coordinator before capacity is released;
2. record the attempted provider and recovery generation;
3. force-refresh that provider;
4. evaluate the next provider;
5. if selected, start a fresh session in the same worktree with the continuation preamble;
6. keep the same workflow step and iteration;
7. do not consume `onFail.retry`;
8. if none is eligible, enter the durable quota-wait transition.

Pinned runners keep current behavior and may use the existing `autoResumeAt` scheduler when their
error supplies a reset instant. They never silently change provider.

Cancellation always wins: interrupt the current runner, release its lease, clear routing wake
state for the task, and do not fail over.

## Authored surfaces and validation

Widen only runner-selection inputs:

- repo `defaultRunner`;
- workspace `agentDefaults.runner`;
- workflow step runner;
- start-run request;
- todo start;
- Continue/follow-up selection when the caller explicitly requests auto;
- `cezar run --runner`;
- composer/default runner controls.

Do not widen:

- provider status ids;
- backend checks;
- model catalogs;
- agent profile provider ids;
- `createRunner()`;
- resume/handoff backend ids.

Validation rules:

- `auto` while global quota routing is disabled returns a clear error;
- `auto` plus a model override is rejected;
- `auto` plus a task-level agent profile is rejected;
- explicit provider behavior is unchanged;
- project/workspace config APIs reject invalid policy combinations before persistence.

Planner, namer, and auxiliary LLM calls are out of MVP. Because the planner currently reads
`defaultRunner`, define an explicit concrete fallback for `defaultRunner: auto` (the first
configured auto provider) and document that planner calls are not quota-routed yet.

## HTTP, live state, and CLI

Add workspace-level routes to the existing chained provider family:

```text
GET  /api/v1/providers/usage
POST /api/v1/providers/usage/refresh
```

The response contains normalized sanitized snapshots, policy summary, and an advisory next-runner
preview. Update contract schemas first, then middleware validation, route chain, typed client,
contract parity, typed-body tests, route inventory, and `BACKWARD_COMPATIBILITY.md`.

Live UI behavior:

- add a trusted-only `provider-usage` WebSocket topic;
- subscribe once at the application root in local mode;
- start UI-driven periodic refresh only on topic subscriber `0 -> 1`, stop on `1 -> 0`;
- routing-required refresh/wake timers continue without a browser;
- hosted mode uses the workspace SSE reconciliation path because it intentionally opens no browser
  WebSocket;
- publish only when the sanitized snapshot changes.

Add CLI commands/options using the existing `node:util` `parseArgs` entrypoint:

```text
cezar usage
cezar usage --json
cezar usage --refresh
cezar run --runner auto "task"
```

JSON mode prints only JSON to stdout.

## Cockpit plan

Place global policy and usage under Settings -> Resources, near the existing usage-limit
auto-resume control.

Controls:

- routing enabled;
- provider order;
- short/long stop thresholds;
- resume threshold;
- provider max concurrency;
- unknown telemetry policy;
- refresh usage.

Display:

- short and long/model windows;
- available, soft exhausted, hard exhausted, auth error, unknown;
- stale telemetry;
- reset instants;
- current advisory provider.

Composer:

- offer `Auto (quota-aware)` only when routing is enabled;
- hide/disable the model picker for Auto;
- hide provider-specific account selection for Auto;
- show `Would currently use Claude/Codex` as advisory only.

Run surfaces:

- show `Auto -> Claude` or `Auto -> Codex`;
- use `StepState.backend` for the active provider;
- show routing/failover events in the transcript;
- show queued quota reason and next reset;
- provide a refresh action, not a silent threshold bypass.

Update attention, grouping, sorting, and read-state helpers so quota-blocked work remains in
progress and is not presented as a failed outcome or user-attention notification.

## Audit events

Persist sanitized run events where relevant:

- `runner_route_decision`;
- `runner_failover_started`;
- `runner_failover_completed`;
- `task_waiting_for_provider`;
- `task_provider_wait_resolved`.

Provider usage refresh/change notifications are workspace-level service events, not duplicated
into every run log. Never attach raw provider responses, headers, auth paths, or tokens.

## Implementation phases and commit boundaries

### 0. Architecture map

- Add this document and confirm every construction/transition site.
- No behavior change.

### 1. Runner selection and contracts

- Introduce `RunnerSelection` without widening concrete provider/backend ids.
- Add normalized quota and workspace policy schemas.
- Add schema/config validation tests.

### 2. Pure router

- Implement priority, thresholds, hysteresis, unknown policy, concurrency, attempted-provider
  exclusion, and recovery generations.
- Pure unit tests only.

### 3. Usage service with fake adapters

- Implement cache, TTL, dedup, stale persistence, reset scheduling, and change events.
- Keep provider acquisition fake at this stage.

### 4. Claude adapter

- Implement credential source isolation, fixed-origin fetch, sanitized errors, and fixture parsers.
- No live request in CI.

### 5. Codex adapter

- Implement app-server rate-limit read/update handling and structured usage-limit errors.
- Use a fake app-server in tests.

### 6. Shared coordinator

- Add selection lock, reservations, runtime hard overrides, queue wake subscription, and boot/headless
  dependency wiring.
- Update every `RunManager` and `ProjectContexts` construction site together.

### 7. Persist requested/resolved selection

- Add optional run/step fields and API parity.
- Allow `auto` through authored config/task/workflow/CLI surfaces with validation, but do not yet
  route real work.

### 8. Step dispatch routing

- Resolve/reserve immediately before agent spawn in both new-workflow and Continue paths.
- Prove Claude preferred, Codex fallback, and non-sticky return to Claude after reset.

### 9. Durable blocked queue

- Add blocked reason, workflow checkpoint, quota park transition, reset wake, and restart recovery.
- Prove both-exhausted stays queued and resumes automatically.

### 10. Runtime failover

- Normalize failure kinds and retry the same step/worktree on another provider.
- Preserve `onFail` budget and prevent provider loops.

### 11. API and CLI

- Add usage/refresh routes, typed client, route inventory, and `cezar usage` commands.

### 12. Cockpit

- Add policy controls, usage visibility, Auto picker, resolved provider, stale state, and blocked UI.

### 13. Security, resilience, and documentation

- Complete secret audits, malformed/timeout/restart/concurrency tests, user guide, and developer
  adapter documentation.

### 14. Final regression

- Run all repository gates and inspect the final diff/state for credentials or unrelated changes.

Suggested commit subjects:

```text
docs: map quota-routing integration points
refactor: separate runner selections from executable backends
feat: add quota-routing contracts and workspace policy
feat: implement deterministic quota routing
feat: add provider usage service
feat: read Claude subscription usage safely
feat: read Codex rate limits through app-server
feat: add shared quota coordinator and reservations
feat: persist requested and resolved runners
feat: route auto workflow steps by quota
feat: park quota-blocked workflows durably
feat: fail over auto steps after quota exhaustion
feat: add provider usage API and CLI
feat: add quota routing cockpit controls
test: harden quota routing lifecycle and security
docs: document quota-aware routing
```

## Required tests

### Pure router

- preferred provider available;
- preferred soft/hard exhausted;
- missing, disabled, auth error, and unknown provider;
- fallback available;
- all exhausted;
- unknown allow/deny;
- provider concurrency full;
- attempted provider excluded;
- reset generation clears exclusion;
- hysteresis prevents flapping;
- every new step reevaluates Claude before Codex.

### Usage service

- cache hit and stale cache;
- force refresh;
- concurrent refresh deduplication;
- timeout/malformed/auth/network failure;
- sanitized atomic persistence;
- restart from stale persisted state;
- earliest-reset scheduling;
- change-only events;
- service disposal.

### Adapters

- Claude five-hour and seven-day normalization;
- missing partial windows;
- credential expiry and fixed-origin enforcement;
- macOS/file credential sources through injected fakes;
- Codex primary/secondary normalization;
- Codex rate-limit update merge;
- structured Codex `usageLimitExceeded`;
- no raw error body or token in outputs.

### RunManager integration

- Claude selected below threshold;
- Codex selected when Claude is over threshold;
- next task returns to Claude after a confirmed reset;
- both exhausted remains queued;
- known reset wakes the queue;
- manual refresh wakes the queue;
- restart preserves blocked task and workflow position;
- runtime Claude quota failure continues on Codex in the same worktree;
- both providers fail once without a loop;
- provider failover does not consume `onFail`;
- cancellation releases reservations and does not fail over;
- explicit Claude never silently uses Codex;
- multi-project coordinator and account scoping;
- both `ActiveRun`/Continue construction paths carry the routing dependencies.

### HTTP/UI/security

- request/response contract parity in both directions;
- new route reaches `AppType` and typed client;
- route inventory and versioned surface;
- Auto picker enabled/disabled behavior;
- model/profile controls under Auto;
- stale usage and refresh;
- resolved provider and blocked reason display;
- token redaction from cache, logs, events, and API;
- untrusted WebSocket cannot read provider usage;
- provider credential files remain unchanged.

Tests must use fake adapters and fake clocks and must never consume real provider quota.

For lifecycle regression tests, follow the repository rule: temporarily stash/remove the source
fix, run the new test to prove it fails against the old behavior, then restore the fix.

## Final verification gates

```bash
npm run typecheck
npm test
npm run test:unit
npm run build
npm run test:package
npm run test:e2e
```

The key delivery boundary is the durable blocked-queue phase. Do not proceed to runtime failover or
UI polish until auto selection, provider reservation, waiting, wake-up, restart recovery, and
explicit-runner compatibility are all proven together.
