# Project Specification: Quota-Aware Automatic Runner Routing for Cezar

**Working feature name:** Cezar Auto Router  
**Repository basis:** Fork of `open-mercato/cezar`  
**Primary target providers:** Claude Code and OpenAI Codex  
**Implementation language:** TypeScript, consistent with the existing Cezar codebase  
**Status:** Implementation specification  
**Primary objective:** Allow Cezar to automatically route queued agent work between Claude Code and Codex according to subscription usage and provider availability, without requiring a human to monitor usage limits or manually switch runners.

---

# 1. Executive Summary

Cezar already provides most of the orchestration infrastructure required for unattended coding-agent execution. It can queue tasks, execute tasks in isolated Git worktrees, run autonomous workflows, invoke Claude Code or Codex through locally authenticated CLIs, run verification steps, and process multiple queued tasks without requiring the user to repeatedly type "next task." Cezar currently exposes Claude Code, Codex, and OpenCode through a common `AgentRunner` abstraction and supports runner selection at the configuration, task, and workflow-step levels.

The missing capability addressed by this project is **quota-aware runner selection**.

The desired behavior is:

1. Claude Code is normally the preferred provider.
2. Before starting each agent step, Cezar determines whether Claude has sufficient subscription quota available.
3. If Claude is available and below configured usage thresholds, the step runs with Claude.
4. If Claude is near or at its limit, Cezar automatically selects Codex.
5. When Claude's quota resets and becomes usable again, Cezar automatically returns to Claude for subsequent work.
6. Codex is also monitored. Cezar must avoid starting new work with Codex when its relevant usage window is near exhaustion.
7. If all configured providers are temporarily unavailable because of subscription limits, queued tasks remain queued rather than failing. Cezar resumes them automatically when a provider becomes eligible.
8. If a provider unexpectedly hits a quota limit during an agent step, Cezar may retry that step using another eligible provider in the same worktree.
9. Explicit user selection of `claude` or `codex` must continue to work and must not silently change behavior unless the user explicitly enables failover for pinned runners.
10. Existing Cezar behavior must remain backward compatible when quota-aware routing is not enabled.

The feature should be implemented as a **routing layer above `AgentRunner`**, rather than by embedding provider-selection logic inside the Claude or Codex runners.

---

# 2. Problem Statement

A developer using both Claude Code and Codex through subscription-backed CLI authentication currently has to perform several operational tasks manually:

- monitor Claude usage,
- monitor Codex usage,
- decide whether enough quota remains to start another task,
- switch the selected coding agent,
- recognize quota exhaustion,
- wait for provider reset windows,
- remember to return to the preferred provider after reset,
- restart interrupted work using another provider.

These decisions contain very little engineering judgment.

Given a sufficiently detailed backlog and strong automated verification, the human developer should manage the **work specification**, while Cezar manages the **execution resources**.

The target workflow is:

```text
Project specification
        |
        v
Cezar task queue
        |
        v
Quota-aware router
        |
   +----+----+
   |         |
Claude     Codex
   |         |
   +----+----+
        |
        v
Task worktree
        |
        v
Verification
        |
   +----+----+
   |         |
 PASS       FAIL
   |         |
 next      retry
 task      according
           to workflow
```

Provider quotas should behave similarly to any other constrained execution resource.

---

# 3. Existing Cezar Capabilities to Preserve

The implementation must preserve Cezar's existing design rather than replacing its task orchestration.

Current Cezar already provides:

- autonomous queued tasks,
- a global parallel execution limit,
- isolated Git worktrees,
- Claude Code support,
- Codex support,
- OpenCode support,
- workflow YAML,
- agent steps,
- shell verification steps,
- bounded `onFail` retry loops,
- per-task runner selection,
- per-workflow-step runner selection,
- a configurable default runner,
- normalized runner events,
- persisted run state,
- a browser cockpit,
- headless CLI operation.

Cezar's documented backend seam is `AgentRunner`, currently located at `packages/cezar/src/core/agent-runner.ts`. Claude Code and Codex already plug into that common abstraction.

Cezar also currently uses the locally authenticated Claude and Codex CLIs rather than requiring independent API billing credentials. That behavior is central to this project and must be preserved.

### Architectural implication

Do **not** redesign agent execution.

Add:

```text
Runner requested by task/workflow
              |
              v
        Runner Resolver
              |
        +-----+------+
        |            |
 explicit runner    auto
        |            |
        v            v
 AgentRunner      Quota Router
                     |
                     v
                 AgentRunner
```

The result of automatic routing should still be a normal existing `AgentRunner`.

---

# 4. Scope

## 4.1 MVP scope

Implement all of the following:

- quota monitoring for Claude Code,
- quota monitoring for Codex,
- normalized provider-usage representation,
- cached usage snapshots,
- configurable provider thresholds,
- configurable provider priority,
- new `auto` runner-selection mode,
- automatic provider selection at agent-step boundaries,
- automatic recovery after quota reset,
- quota-related failure detection,
- fallback to another provider after an actual quota failure,
- queue blocking when no provider is eligible,
- automatic queue wake-up after eligibility changes,
- cockpit display of provider usage,
- cockpit indication of automatically selected runner,
- CLI-readable usage state,
- JSON-readable usage state,
- audit events explaining routing decisions,
- unit tests,
- integration tests,
- mocked end-to-end tests,
- backward compatibility with current runner selection.

## 4.2 Explicitly out of scope for MVP

Do not implement:

- automatic switching among multiple Claude accounts,
- automatic switching among multiple ChatGPT accounts,
- purchasing API usage,
- API-key billing fallback,
- automatic model downgrading,
- OpenCode quota monitoring,
- killing a healthy running agent merely because its usage threshold was crossed,
- automatic merging of generated code,
- prediction of exact token cost of a future task,
- context-window percentage monitoring,
- automatic decomposition of large tasks,
- changing Cezar's worktree architecture,
- changing existing review-gate semantics,
- provider-specific account login flows.

Multi-account support may be added later by extending the same abstraction from `provider` to `provider account`.

---

# 5. Important Design Principle: Tasks, Not Chat Sessions

The system must not reproduce the user's old interactive-chat workflow internally.

Cezar already treats tasks and workflow steps as execution units. Quota decisions should therefore occur around those units.

The router must **not** continuously switch providers merely because usage changes while an agent is working.

Correct behavior:

```text
Claude selected at 82%
        |
        v
Task begins
        |
Claude reaches 94% while working
        |
        v
Claude is allowed to finish
        |
        v
Next task evaluates quota again
        |
        v
Codex selected
```

Incorrect behavior:

```text
Claude reaches threshold
        |
        v
kill Claude process immediately
        |
        v
start Codex
```

The only exception is when the running provider itself terminates or becomes unusable because of a quota/rate-limit condition.

---

# 6. Functional Requirements

## FR-1: Introduce `auto` as a Runner Selection

Extend runner selection to support:

```text
claude
codex
opencode
auto
```

Existing configurations using explicit runners must continue working without modification.

`auto` means:

> Resolve the actual provider immediately before the corresponding agent step begins.

A task created with `runner: auto` must store both:

```text
requestedRunner = "auto"
resolvedRunner = "claude"
```

or:

```text
requestedRunner = "auto"
resolvedRunner = "codex"
```

These values must remain distinguishable throughout run persistence, API responses, UI rendering, and logs.

---

# 7. Runner Precedence

Preserve Cezar's existing runner-precedence model.

Conceptually:

1. workflow-step runner,
2. task runner,
3. project default runner.

If the resolved requested value is an explicit provider, use the explicit provider.

If the resolved requested value is:

```text
auto
```

invoke the quota router.

Example:

```yaml
steps:
  - id: implement
    runner: auto
    prompt: "{{task}}"

  - id: review
    runner: claude
    prompt: "Review the resulting implementation."
```

`implement` may run on Codex because Claude is exhausted.

`review` must still run on Claude because the workflow author explicitly pinned it.

---

# 8. Provider Usage Abstraction

Create a provider-neutral quota model.

Recommended conceptual types:

```ts
type UsageProvider = "claude" | "codex";

type UsageWindowKind =
  | "short"
  | "long"
  | "model"
  | "unknown";

interface ProviderUsageWindow {
  id: string;
  kind: UsageWindowKind;
  label: string;

  usedPercent: number | null;
  remainingPercent: number | null;

  resetsAt: string | null;

  hardLimitReached: boolean;

  metadata?: Record<string, unknown>;
}

type ProviderHealth =
  | "available"
  | "soft_exhausted"
  | "hard_exhausted"
  | "auth_error"
  | "unavailable"
  | "unknown";

interface ProviderUsageSnapshot {
  provider: UsageProvider;

  health: ProviderHealth;

  fetchedAt: string;
  source: string;

  stale: boolean;

  windows: ProviderUsageWindow[];

  error?: {
    code: string;
    message: string;
  };
}
```

Do not make routing logic depend directly on Claude-specific or Codex-specific JSON structures.

Provider adapters must normalize their responses into this representation.

---

# 9. Usage Provider Interface

Implement a small interface independent of `AgentRunner`.

Suggested abstraction:

```ts
interface UsageProviderAdapter {
  provider: UsageProvider;

  isAvailable(): Promise<boolean>;

  fetchUsage(options?: {
    force?: boolean;
  }): Promise<ProviderUsageSnapshot>;
}
```

Recommended services:

```text
UsageProviderAdapter
    |
    +-- ClaudeUsageAdapter
    |
    +-- CodexUsageAdapter
```

Above those:

```text
ProviderUsageService
```

Above that:

```text
QuotaRouter
```

Do not place HTTP/authentication parsing in `QuotaRouter`.

---

# 10. Usage Data Sources

## 10.1 Claude

The implementation may inspect the approaches used by existing MIT-licensed projects such as `clauth` and Claude Code Usage Monitor.

`clauth` currently exposes live Claude 5-hour and longer-window utilization and implements threshold-based automatic switching. It also exposes machine-readable status through a daemon/status interface.

Claude Code Usage Monitor currently retrieves authenticated Claude usage and can fall back to rate-limit information when its preferred usage source is unavailable.

Do not tightly couple Cezar to either project.

The preferred architecture is a native Cezar adapter with provider-specific behavior isolated behind `ClaudeUsageAdapter`.

### Claude adapter requirements

The adapter must:

- use the user's existing Claude Code authentication,
- never request a separate API key,
- never persist copied OAuth tokens,
- never expose credentials through Cezar's API,
- support expired-auth detection,
- distinguish telemetry failure from confirmed quota exhaustion,
- expose reset timestamps when available,
- tolerate missing long-window data,
- return `unknown` rather than inventing values.

If an authenticated endpoint changes or becomes unavailable, the adapter must fail gracefully.

Do not prevent all Cezar usage because a quota-monitoring endpoint broke.

---

# 11. Codex Usage Data

Implement the equivalent adapter:

```text
CodexUsageAdapter
```

Claude Code Usage Monitor demonstrates that Codex usage can be retrieved using the locally authenticated Codex installation rather than requiring separate API billing credentials.

Where practical, prefer structured rate-limit information exposed by the Codex tooling Cezar already interacts with.

The adapter must normalize whatever Codex calls its relevant usage windows into the provider-neutral window model.

Do not assume that future Codex subscription plans will always expose exactly the same number or names of windows.

---

# 12. Credential Handling

This feature deals with authentication material and must be implemented conservatively.

## Mandatory rules

1. Never write Claude or Codex access tokens into:
   - `runs.json`,
   - NDJSON event logs,
   - browser API payloads,
   - console logs,
   - error messages,
   - usage-cache files.

2. Do not copy provider credential files into `.ai/cezar/`.

3. Do not mutate authentication files directly unless absolutely necessary.

4. Prefer asking the installed provider CLI to refresh its own credentials if refresh is required.

5. Never send provider credentials to a Cezar-owned backend or third-party service.

6. Usage requests must go only to the corresponding provider's authenticated first-party service.

7. Reuse Cezar's existing secret-redaction infrastructure wherever possible.

8. Treat HTTP response bodies from authentication failures as potentially sensitive.

9. UI payloads may include:
   - provider,
   - usage percentage,
   - reset time,
   - health,
   - timestamp.

10. UI payloads must not include:
   - bearer token,
   - refresh token,
   - raw auth headers,
   - entire credential objects.

---

# 13. Quota Routing Configuration

Quota-routing policy is primarily a **user/machine concern**, because Claude and Codex subscription quotas are shared across repositories.

Therefore the main routing policy should live in Cezar's global workspace configuration rather than only inside an individual project's `.ai/cezar/config.json`.

A project may still choose whether its default runner is `auto`.

Recommended global configuration:

```json
{
  "quotaRouting": {
    "enabled": true,

    "providerOrder": [
      "claude",
      "codex"
    ],

    "refreshIntervalSeconds": 60,
    "cacheTtlSeconds": 30,

    "unknownUsagePolicy": "allow",
    "allUnavailablePolicy": "wait",

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

Values above are defaults for implementation purposes, not assertions about ideal quota economics.

Make them configurable.

---

# 14. Backward Compatibility

This feature must be opt-in initially.

If the user does nothing:

```json
{
  "defaultRunner": "claude"
}
```

must continue behaving as it does in upstream Cezar.

Do not silently change existing installations to quota-aware routing.

Users enable the feature by either:

```json
{
  "defaultRunner": "auto"
}
```

or selecting `Auto` for a particular task/workflow.

If `auto` is requested while quota routing is disabled, return a clear validation error or prompt the user to enable routing.

Prefer validation rather than silently interpreting `auto` as Claude.

---

# 15. Threshold Semantics

Two threshold concepts are required.

## Soft exhaustion

A provider is considered `soft_exhausted` when a relevant usage window exceeds the configured threshold for **starting new work**.

Example:

```text
Claude 5h utilization = 91%
Configured stop threshold = 90%
```

Result:

```text
Do not assign another new auto-routed step to Claude.
```

Do not terminate an already running Claude process.

## Hard exhaustion

A provider is `hard_exhausted` when the provider reports an actual subscription/rate-limit condition or a relevant window reaches its hard limit.

Hard exhaustion makes the provider immediately ineligible for additional automatically routed work.

---

# 16. Hysteresis

Prevent provider thrashing.

Example failure mode:

```text
Claude = 89.8%
route Claude

next poll = 90.1%
route Codex

next poll = 89.9%
route Claude
```

Use separate stop and resume thresholds.

Example:

```text
stopNewWorkAtPercent = 90
resumeBelowPercent = 80
```

Once soft-exhausted, a provider remains soft-exhausted until:

- the relevant window resets, or
- utilization drops below `resumeBelowPercent`.

Reset detection should restore eligibility immediately when the new window is confirmed.

---

# 17. Routing Algorithm

Create a pure, deterministic routing function wherever possible.

Conceptual signature:

```ts
resolveAutoRunner({
  policy,
  installedProviders,
  runningCounts,
  usageSnapshots,
  attemptedProviders
}): RoutingDecision
```

Recommended result:

```ts
interface RoutingDecision {
  outcome: "selected" | "wait" | "error";

  provider?: UsageProvider;

  reason:
    | "preferred_available"
    | "preferred_soft_exhausted"
    | "preferred_hard_exhausted"
    | "provider_concurrency_limit"
    | "auth_error"
    | "usage_unknown_allowed"
    | "all_providers_unavailable"
    | "all_providers_exhausted"
    | "already_attempted";

  considered: Array<{
    provider: UsageProvider;
    eligible: boolean;
    reason: string;
  }>;

  retryAt?: string;
}
```

This function must have no network calls.

Network polling belongs in `ProviderUsageService`.

---

# 18. Default Routing Policy

For:

```json
{
  "providerOrder": ["claude", "codex"]
}
```

evaluate providers in that order.

For each provider:

1. Is the CLI installed?
2. Is the provider enabled?
3. Is authentication usable?
4. Has the provider already failed for this step?
5. Has its provider-specific concurrency ceiling been reached?
6. Is usage data fresh?
7. Is any relevant hard limit reached?
8. Is any configured soft threshold reached?
9. If usage is unknown, apply `unknownUsagePolicy`.

Choose the first eligible provider.

---

# 19. Unknown Telemetry Policy

Quota telemetry is not guaranteed to remain available forever.

Support:

```text
unknownUsagePolicy:
  allow
  deny
```

Default:

```text
allow
```

Reason:

Quota telemetry failure should not turn an otherwise functional Cezar installation into a dead queue.

With `allow`:

```text
Claude telemetry unavailable
Claude CLI authenticated and operational
        |
        v
Claude remains eligible
```

If Claude then actually returns a quota error, runtime failure handling marks it exhausted and routes subsequent work elsewhere.

With `deny`, users may choose more conservative behavior.

---

# 20. Refresh and Caching

Do not query provider usage before every internal event.

Implement a shared `ProviderUsageService` with:

- in-memory cache,
- configurable TTL,
- periodic refresh,
- force-refresh method,
- deduplicated concurrent refreshes.

Suggested defaults:

```text
periodic refresh: 60 seconds
routing freshness TTL: 30 seconds
request timeout: 5 to 10 seconds
```

If five queued tasks become eligible simultaneously, they should not trigger five identical provider usage calls.

Use a single in-flight promise or equivalent deduplication mechanism per provider.

---

# 21. Parallel Task Race Conditions

Cezar can execute multiple tasks concurrently.

Therefore this is invalid:

```text
Task A reads Claude = 89%
Task B reads Claude = 89%
Task C reads Claude = 89%

all three launch Claude
```

even though the user intended to stop starting new Claude work at 90%.

Quota percentage alone cannot predict exact consumption, but dispatch can be controlled.

Add:

```json
"maxConcurrent": 1
```

per provider.

Runner-selection and running-count reservation must occur atomically within Cezar's orchestration process.

Conceptually:

```text
acquire routing lock
    |
refresh if stale
    |
select provider
    |
reserve provider slot
    |
release routing lock
    |
launch agent
```

Release reservation when the agent step terminates.

Users can raise `maxConcurrent` if desired.

---

# 22. Queue Behavior When All Providers Are Exhausted

Do not fail a queued task because all subscription providers are temporarily exhausted.

Introduce a waiting condition.

Possible internal representation:

```ts
blockedReason: {
  type: "provider_quota";
  providers: ["claude", "codex"];
  retryAt: "2026-08-14T15:00:00Z";
}
```

The task remains logically queued.

Suggested UI state:

```text
Queued
Waiting for provider quota
Claude resets in 38m
Codex weekly limit reached
```

This should not require introducing an externally visible terminal task state unless the existing Cezar state machine makes that necessary.

Prefer:

```text
status = queued
blockedReason = provider_quota
```

over adding a wholly separate top-level status.

---

# 23. Automatic Queue Wake-Up

The scheduler must reconsider blocked tasks when:

- periodic usage refresh completes,
- a provider reset time passes,
- a running provider slot becomes free,
- the user forces a quota refresh,
- routing configuration changes,
- provider authentication becomes healthy,
- Cezar restarts.

Do not depend only on a fixed polling interval when a known `resetAt` value exists.

Schedule a wake-up around the earliest known relevant reset time.

Still perform an actual usage refresh before launching work.

---

# 24. Provider Failure Classification

Extend runner errors so orchestration can distinguish quota exhaustion from unrelated failure.

Suggested abstraction:

```ts
type RunnerFailureKind =
  | "quota_exhausted"
  | "rate_limited"
  | "auth"
  | "network"
  | "process"
  | "tool"
  | "model"
  | "cancelled"
  | "unknown";
```

The Claude and Codex runners should normalize structured provider events where possible.

Text matching may be used as a fallback only.

Do not classify every HTTP 429 as subscription exhaustion without considering the available provider-specific context.

---

# 25. Automatic Mid-Task Recovery

Do not proactively migrate a healthy running session.

However, if a provider process terminates because quota is actually exhausted, the same workflow step may be retried using another provider.

Example:

```text
Task T17
worktree: cez/T17

Claude starts implementation
        |
edits three files
        |
Claude returns quota exhaustion
        |
        v
mark Claude temporarily ineligible
        |
refresh usage
        |
select Codex
        |
        v
Codex starts in SAME WORKTREE
```

Codex receives a generated continuation preamble:

```text
A previous coding-agent attempt was interrupted because its provider
became unavailable.

Continue the current workflow step.

Before editing:
1. Inspect the current worktree.
2. Inspect git status and git diff.
3. Determine what the previous attempt already completed.
4. Preserve correct existing work.
5. Complete the original task and acceptance criteria.
6. Run the required verification.

Do not restart the implementation blindly.
```

Append the original workflow-step prompt afterward.

---

# 26. Failover Loop Prevention

Track attempted providers per execution attempt.

Example:

```ts
attemptedProviders = new Set<UsageProvider>();
```

Sequence:

```text
Claude
  |
quota failure
  |
Codex
  |
quota failure
  |
WAIT
```

Do not produce:

```text
Claude -> Codex -> Claude -> Codex -> ...
```

A provider becomes eligible for that step again only after:

- an appropriate reset/recovery,
- a human retry,
- or a new workflow attempt according to explicit retry semantics.

---

# 27. Explicit Runner Failure Semantics

Pinned runner behavior must remain predictable.

For:

```yaml
runner: claude
```

default behavior:

```text
Claude quota exhausted
        |
        v
step fails/waits according to existing semantics
```

Do **not** silently execute Codex.

Optionally support later:

```yaml
runner: claude
allowProviderFailover: true
```

but this is not required for MVP.

Automatic cross-provider routing belongs primarily to:

```yaml
runner: auto
```

---

# 28. Model Compatibility

Provider failover becomes ambiguous when the workflow specifies a provider-specific model.

Example:

```yaml
runner: auto
model: opus
```

`opus` does not identify a Codex model.

MVP rule:

If an agent step uses `runner: auto`, its `model` must either:

1. be omitted, or
2. use a future provider-neutral model policy.

For MVP, reject `runner: auto` plus a provider-specific model override with a clear validation message.

Do not silently map model names across providers.

---

# 29. Planner and Auxiliary LLM Calls

MVP quota routing applies to normal workflow agent steps.

It does not need to route:

- task naming,
- planner calls,
- metadata generation,
- other auxiliary LLM features.

Those may be migrated to the same infrastructure later.

Keep MVP boundaries explicit.

---

# 30. Persistence

Persist enough quota state for continuity across restarts, but never persist credentials.

Recommended location:

```text
~/.cezar/provider-usage.json
```

Example:

```json
{
  "schemaVersion": 1,
  "providers": {
    "claude": {
      "health": "soft_exhausted",
      "fetchedAt": "2026-08-14T14:20:00Z",
      "windows": [
        {
          "id": "short",
          "kind": "short",
          "label": "5h",
          "usedPercent": 92,
          "resetsAt": "2026-08-14T15:00:00Z"
        }
      ]
    }
  }
}
```

Persisted data is advisory.

On process start:

1. load cached snapshot,
2. mark it stale,
3. display it if useful,
4. perform a background refresh,
5. do not launch auto-routed work based exclusively on stale "available" data if a fresh check can reasonably be made.

Use atomic file writes.

---

# 31. Routing Audit Events

Every automatic provider decision must be inspectable.

Emit normalized events such as:

```json
{
  "type": "runner_route_decision",
  "requestedRunner": "auto",
  "resolvedRunner": "codex",
  "reason": "preferred_soft_exhausted",
  "preferredProvider": "claude",
  "timestamp": "...",
  "usage": {
    "claude": {
      "shortWindowUsedPercent": 92,
      "snapshotAgeMs": 1200
    }
  }
}
```

Do not include credentials or raw provider responses.

Also emit:

```text
provider_usage_refreshed
provider_became_exhausted
provider_became_available
runner_failover_started
runner_failover_completed
task_waiting_for_provider
task_provider_wait_resolved
```

These events should integrate into Cezar's existing run/event architecture where practical.

---

# 32. CLI Requirements

Add:

```bash
cezar usage
```

Example human-readable output:

```text
Provider  Status            Short window       Long window
Claude    soft exhausted    92%  reset 38m     51%
Codex     available         44%                 63%

Auto routing:
Claude -> Codex

Next runner:
Codex
```

Add:

```bash
cezar usage --json
```

Return machine-readable normalized snapshots.

Also support:

```bash
cezar usage --refresh
```

And:

```bash
cezar run --runner auto "implement task X"
```

Use the existing CLI conventions rather than inventing an unrelated argument parser.

---

# 33. Cockpit UI Requirements

Add quota visibility without turning Cezar into a billing dashboard.

## Settings > Agents or Resources

Display:

```text
Quota-aware routing          Enabled

Preference
1. Claude
2. Codex

Claude
5h     92%    resets in 38m
7d     51%
Status: Paused for new work

Codex
5h     44%
week   63%
Status: Available
```

Controls:

- Enable quota-aware routing
- Provider priority
- Stop threshold
- Long-window stop threshold
- Resume threshold
- Maximum concurrent tasks
- Unknown telemetry policy
- Refresh now

---

# 34. New Task Runner Picker

Add:

```text
Auto
Claude
Codex
OpenCode
```

Recommended display label:

```text
Auto (quota-aware)
```

If selected, optionally show:

```text
Would currently use: Codex
Claude is at configured usage threshold
```

This preview is advisory. The actual selection still occurs when execution begins.

---

# 35. Task Display

An auto-routed run must show both policy and execution provider.

Example:

```text
Runner
Auto -> Codex
```

or an Auto badge alongside the Codex icon.

For a workflow where multiple steps resolve differently:

```text
Implement       Claude
Verify          shell
Repair          Codex
```

The run history must accurately reflect what actually happened.

---

# 36. Waiting UI

If no provider is eligible:

```text
Queued
Waiting for provider quota

Claude
92%, resumes after reset at 3:00 PM

Codex
weekly threshold reached, resets Sunday
```

Provide:

```text
Refresh usage
```

Do not provide a button that silently ignores configured limits unless explicitly labeled as an override.

---

# 37. HTTP/API Surface

Follow existing Cezar API conventions.

Recommended conceptual endpoints:

```text
GET  /api/usage
POST /api/usage/refresh
```

If global settings already have a consolidated endpoint, extend that endpoint rather than creating redundant settings APIs.

The usage endpoint should return only normalized sanitized state.

Example:

```json
{
  "routingEnabled": true,
  "providerOrder": ["claude", "codex"],
  "providers": {
    "claude": {
      "health": "soft_exhausted",
      "fetchedAt": "...",
      "stale": false,
      "windows": []
    },
    "codex": {
      "health": "available",
      "fetchedAt": "...",
      "stale": false,
      "windows": []
    }
  }
}
```

---

# 38. Recommended Module Structure

Codex must inspect the current fork before creating files because upstream paths may evolve.

Do not blindly assume this layout exists.

A reasonable target structure is:

```text
packages/cezar/src/core/
  agent-runner.ts
  runner-resolver.ts

  usage/
    types.ts
    provider-usage-service.ts
    quota-router.ts
    claude-usage-adapter.ts
    codex-usage-adapter.ts
    usage-cache.ts
    failure-classifier.ts
```

If the existing repository has established patterns for services, configuration, APIs, or filesystem persistence, follow those patterns instead.

`AgentRunner` itself should remain focused on agent execution.

---

# 39. Runner Resolver

Introduce or extend a single runner resolution point.

Conceptually:

```ts
async function resolveRunnerForStep(
  requestedRunner: RunnerName,
  context: RunnerResolutionContext
): Promise<ResolvedRunner>
```

For explicit providers:

```ts
if (requestedRunner !== "auto") {
  return getRunner(requestedRunner);
}
```

For auto:

```ts
const decision = await quotaRouter.selectProvider(context);

if (decision.outcome === "selected") {
  return getRunner(decision.provider);
}

if (decision.outcome === "wait") {
  throw new ProviderWaitSignal(decision);
}

throw new RunnerResolutionError(...);
```

Do not scatter `if claude usage > 90` checks throughout scheduler code.

---

# 40. Provider Usage Service

Responsibilities:

- own provider adapters,
- periodically poll providers,
- cache results,
- deduplicate refresh requests,
- persist sanitized state,
- calculate freshness,
- publish usage-change events,
- expose current snapshots,
- expose force refresh.

It must **not** decide which provider should run a task.

That belongs to `QuotaRouter`.

---

# 41. Quota Router

Responsibilities:

- read routing policy,
- inspect normalized usage,
- inspect installed provider availability,
- inspect provider concurrency,
- respect attempted-provider exclusions,
- produce a deterministic decision.

It must **not**:

- read auth files,
- perform HTTP requests,
- launch runners,
- modify tasks,
- write UI state.

This separation is important for testability.

---

# 42. Failure Classifier

Provider runners should produce normalized failure semantics.

Where structured provider protocol events exist, use those first.

Fallback:

```text
structured signal
    |
    no
    v
exit status + known error metadata
    |
    insufficient
    v
conservative text classification
```

Never mark a provider exhausted from a broad substring such as `"limit"` alone.

Classification should include test fixtures representing actual observed provider outputs.

---

# 43. Rate-Limit Failure Flow

For an auto-routed step:

```text
Claude selected
        |
        v
Claude runner starts
        |
        v
quota failure detected
        |
        v
runner returns normalized quota failure
        |
        v
ProviderUsageService force-refreshes Claude
        |
        v
Claude marked temporarily unavailable
        |
        v
QuotaRouter invoked with attemptedProviders={claude}
        |
        +----------------+
        |                |
     Codex eligible    none eligible
        |                |
        v                v
 retry same step      task waits
 same worktree        for quota
```

The step retry must not create a new worktree.

---

# 44. Interaction With Workflow `onFail`

Provider failover is infrastructure recovery, not normal workflow failure.

Therefore:

```text
Claude quota failure
-> Codex infrastructure failover
```

should occur before consuming the workflow's ordinary:

```yaml
onFail:
  retry:
  max:
```

budget.

Only after all eligible providers have been attempted should the step be considered failed for workflow purposes.

This prevents provider quota exhaustion from consuming application-level repair attempts.

---

# 45. Interaction With Cancellation

If the user cancels a task:

- cancel the active runner,
- release any provider concurrency reservation,
- do not invoke failover,
- do not automatically restart it after quota reset.

Cancellation always wins.

---

# 46. Interaction With Cezar Restart

After restart:

1. recover queued tasks using existing Cezar semantics,
2. load cached provider usage as stale,
3. initialize provider adapters,
4. refresh usage,
5. recompute provider health,
6. re-evaluate quota-blocked tasks,
7. resume queue draining.

Do not lose a task merely because it was waiting for provider quota when Cezar stopped.

---

# 47. Dry-Run and Test Infrastructure

Cezar already has a dry-run mode for exercising the cockpit without real agent calls.

Extend testing so quota routing can also operate entirely without real Claude or Codex accounts.

Create deterministic fake adapters.

Example fixtures:

```json
{
  "claude": {
    "health": "soft_exhausted",
    "windows": [
      {
        "kind": "short",
        "usedPercent": 94,
        "resetsAt": "2026-08-14T16:00:00Z"
      }
    ]
  },
  "codex": {
    "health": "available",
    "windows": [
      {
        "kind": "short",
        "usedPercent": 25
      }
    ]
  }
}
```

Tests must never consume real subscription quota.

---

# 48. Required Unit Tests

At minimum:

## QuotaRouter

- preferred provider available,
- preferred provider soft exhausted,
- preferred provider hard exhausted,
- preferred provider auth error,
- preferred provider missing,
- fallback available,
- all providers exhausted,
- all providers unknown with policy `allow`,
- all providers unknown with policy `deny`,
- concurrency threshold reached,
- already-attempted provider excluded,
- reset returns provider to eligibility,
- hysteresis prevents flapping.

## Usage service

- cache hit,
- stale cache,
- force refresh,
- concurrent refresh deduplication,
- timeout,
- malformed provider payload,
- auth failure,
- network failure,
- sanitized persistence,
- restart from cached state.

## Failure classification

- Claude quota exhaustion,
- Claude auth error,
- Claude generic execution failure,
- Codex quota exhaustion,
- Codex auth error,
- Codex network error,
- unknown errors remain unknown.

---

# 49. Required Integration Tests

Test:

### Scenario A: Claude preferred

```text
Claude 20%
Codex 20%

Task runner=auto

Expected:
Claude
```

### Scenario B: Claude threshold reached

```text
Claude 92%
Codex 30%

Expected:
Codex
```

### Scenario C: Claude reset

```text
Task 1 -> Codex because Claude exhausted

Claude usage refresh shows new window at 5%

Task 2

Expected:
Claude
```

### Scenario D: Both exhausted

```text
Claude exhausted
Codex exhausted

Expected:
task remains queued
blockedReason=provider_quota
```

### Scenario E: One resets

```text
Codex reset detected

Expected:
queued task automatically starts on Codex
```

### Scenario F: Runtime Claude failure

```text
Claude selected
Claude mock edits worktree
Claude mock returns quota_exhausted
Codex available

Expected:
Codex retries same workflow step
same worktree
previous changes visible
attemptedProviders contains Claude
```

### Scenario G: Both fail during same step

Expected:

```text
Claude attempted
Codex attempted
task waits or fails according to whether reset time is known
no infinite retry
```

### Scenario H: Explicit runner

```yaml
runner: claude
```

Claude exhausted.

Expected:

```text
Codex is not silently substituted.
```

---

# 50. Required UI Tests

Test at minimum:

- Auto appears in runner selector when enabled,
- provider bars render,
- stale telemetry is visually identified,
- usage refresh updates values,
- resolved provider appears on task,
- blocked queued task displays quota reason,
- next reset displays correctly,
- configuration updates persist,
- credentials never appear in browser payloads,
- `Auto -> Claude` changes to `Auto -> Codex` as mocked usage changes.

---

# 51. Security Tests

Add tests proving:

- fake bearer tokens are redacted from logs,
- raw credential objects never enter API responses,
- usage cache contains no token strings,
- raw provider error body is sanitized,
- remote cockpit receives only normalized usage state,
- provider authentication files are not modified by ordinary usage polling.

---

# 52. Observability

Add useful structured debug logs such as:

```text
[quota] refreshed claude: available, short=81%, long=42%
[quota] refreshed codex: available, short=37%, long=65%
[router] task abc requested auto
[router] selected claude: preferred_available
```

When switching:

```text
[router] skipped claude: short threshold 90%, observed 92%
[router] selected codex: fallback_available
```

When blocked:

```text
[router] no provider eligible
[router] next known reset: 2026-08-14T19:00:00Z
```

Never log authentication tokens.

---

# 53. Usage Endpoint Fragility

Treat quota telemetry as a potentially unstable integration.

Provider adapters must be independently replaceable.

A future change should be able to replace:

```text
ClaudeUsageAdapterV1
```

with:

```text
ClaudeUsageAdapterV2
```

without changing:

- task scheduler,
- QuotaRouter,
- UI schemas,
- workflow semantics.

Claude Code Usage Monitor currently demonstrates both authenticated usage retrieval and fallback behavior when its preferred usage endpoint is unavailable.

This separation is therefore a hard architectural requirement, not optional cleanup.

---

# 54. Reference Implementations and Licensing

Two existing implementations are useful references.

## `clauth`

Useful concepts:

- live Claude quota monitoring,
- threshold switching,
- long-window gates,
- hysteresis/fallback behavior,
- headless operation,
- machine-readable status,
- handling stale telemetry,
- authentication failure quarantine.

The project is MIT licensed.

## Claude Code Usage Monitor

Useful concepts:

- Claude authenticated usage retrieval,
- Codex authenticated usage retrieval,
- local CLI credential discovery,
- provider-specific usage parsing,
- reset countdowns,
- safe handling of local authentication,
- fallback rate-limit data.

The project is also MIT licensed.

### Implementation rule

Codex may study these repositories for behavior and algorithms.

If code is copied rather than independently reimplemented, preserve all license and attribution obligations.

Prefer adapting concepts into Cezar's TypeScript architecture rather than mechanically porting large sections from unrelated applications.

---

# 55. Configuration Migration Requirements

Any schema changes must be backward compatible.

Existing:

```json
{
  "defaultRunner": "claude"
}
```

must remain valid.

New:

```json
{
  "defaultRunner": "auto"
}
```

must become valid.

Missing `quotaRouting` configuration must resolve to safe defaults.

Invalid quota settings should produce specific validation errors.

Examples:

```text
stopNewWorkAtPercent must be between 0 and 100
resumeBelowPercent must be lower than stopNewWorkAtPercent
providerOrder contains unsupported provider
no enabled providers configured
```

Use Cezar's existing Zod validation conventions. Cezar's current stack documents Zod at system boundaries.

---

# 56. Recommended Defaults

For first implementation:

```text
Feature enabled automatically: no
Default project runner: unchanged
Auto preference: Claude, then Codex
Unknown usage: allow
Refresh: 60 seconds
Cache TTL for dispatch: 30 seconds
Stop new work: 90%
Long-window stop: 95%
Resume: 80%
Provider max concurrency: 1
Quota failover during auto steps: yes
Failover for explicitly pinned runner: no
```

These must remain configurable.

---

# 57. Acceptance Criteria

The feature is complete when all conditions below are satisfied.

## AC-1

Given:

```text
Claude usage below threshold
Codex usage below threshold
provider order = Claude, Codex
runner = auto
```

Cezar launches Claude.

## AC-2

Given:

```text
Claude usage above threshold
Codex below threshold
runner = auto
```

Cezar launches Codex without human action.

## AC-3

After Claude's usage window resets, Cezar automatically considers Claude eligible again and the next auto-routed task selects Claude.

## AC-4

If both providers are exhausted, the task remains queued and visibly indicates that it is waiting for provider quota.

## AC-5

When an eligible provider becomes available, the queue resumes automatically.

## AC-6

An explicit:

```yaml
runner: claude
```

does not silently run Codex.

## AC-7

An auto-routed Claude step that terminates because of confirmed quota exhaustion can automatically continue using Codex in the same worktree.

## AC-8

A provider is never switched merely because it crosses a soft threshold while a healthy step is already running.

## AC-9

No OAuth token or credential is persisted into Cezar run state, event state, quota cache, or UI payloads.

## AC-10

All tests run without consuming real Claude or Codex subscription quota.

## AC-11

Existing Cezar tests continue to pass.

## AC-12

The following commands pass:

```bash
npm run typecheck
npm test
npm run test:unit
npm run build
```

Also run relevant package and end-to-end tests according to the repository's current contribution requirements.

## AC-13

A fresh fork with quota routing disabled behaves identically to upstream Cezar for existing runner-selection flows.

---

# 58. Implementation Plan

Codex should execute this project incrementally.

Do not attempt the entire implementation in one change.

Each phase should leave the repository compiling and preferably passing tests.

---

## Task 0: Repository Reconnaissance

Before editing:

1. Read:
   - `AGENTS.md`
   - `SDLC.md`
   - `CODE_REVIEW.md`
   - relevant package READMEs
   - `packages/cezar/src/core/agent-runner.ts`
2. Locate:
   - runner type definitions,
   - runner factory/resolver,
   - Claude runner,
   - Codex runner,
   - project config schema,
   - global workspace config schema,
   - task persistence schema,
   - queue scheduler,
   - workflow executor,
   - API route definitions,
   - web runner picker,
   - global settings UI,
   - event schema,
   - dry-run implementation.
3. Search for all exhaustive checks of:
   ```text
   claude
   codex
   opencode
   ```
4. Identify every location that must understand the new `auto` value.
5. Record the architecture mapping in:
   ```text
   docs/quota-routing-implementation-notes.md
   ```
6. Do not begin feature implementation until this mapping exists.

**Completion criterion:** architecture map committed with no behavioral change.

---

## Task 1: Normalized Usage Domain Model

Implement:

- `UsageProvider`,
- `ProviderUsageWindow`,
- `ProviderUsageSnapshot`,
- `ProviderHealth`,
- routing configuration schema,
- routing decision schema.

Add pure validation tests.

Do not perform network calls yet.

**Completion criterion:** types compile and schema tests pass.

---

## Task 2: QuotaRouter Pure Logic

Implement the deterministic router with mocked snapshots.

Cover:

- priority,
- thresholds,
- hysteresis,
- unknown policy,
- hard exhaustion,
- concurrency,
- attempted-provider exclusion,
- all-unavailable behavior.

No real provider code.

**Completion criterion:** comprehensive router unit tests pass.

---

## Task 3: ProviderUsageService

Implement:

- adapter registration,
- cache,
- TTL,
- force refresh,
- deduplication,
- periodic refresh,
- sanitized persistence,
- stale recovery.

Use fake adapters only initially.

**Completion criterion:** service tests pass completely offline.

---

## Task 4: Claude Usage Adapter

Implement Claude usage acquisition.

Requirements:

- local Claude authentication,
- no separate API key,
- no credential persistence,
- sanitized errors,
- normalized windows,
- authentication failure detection,
- telemetry unavailable state,
- reset extraction,
- deterministic parser tests using fixtures.

Study `clauth` and Claude Code Usage Monitor if useful.

Do not make routing changes in this task.

**Completion criterion:** adapter tests pass using fixtures; optional manually gated live test may be documented but must not run in CI.

---

## Task 5: Codex Usage Adapter

Implement equivalent Codex behavior.

Requirements mirror Task 4.

Where Cezar's existing Codex app-server integration already exposes useful structured rate-limit data, reuse that architecture where practical rather than creating duplicate plumbing.

**Completion criterion:** deterministic fixture tests pass without real quota consumption.

---

## Task 6: Add `auto` Runner Type

Extend:

- config schemas,
- API schemas,
- task schemas,
- workflow schemas,
- CLI parsing,
- UI types.

Do not yet alter dispatch behavior beyond resolving/validating the new value.

Ensure existing runner values remain unchanged.

**Completion criterion:** `auto` survives config/task/workflow serialization and all existing runner tests remain green.

---

## Task 7: Integrate Router at Agent-Step Dispatch

Add the runner resolver above `AgentRunner`.

For `auto`:

1. request fresh-enough usage,
2. execute QuotaRouter,
3. reserve provider concurrency slot,
4. persist routing decision,
5. instantiate selected existing runner,
6. launch step,
7. release slot after termination.

**Completion criterion:** mocked integration tests prove Claude/Codex selection.

---

## Task 8: Quota-Blocked Queue Behavior

Implement:

```text
queued + blockedReason=provider_quota
```

Add:

- known next reset,
- automatic wake-up,
- refresh-trigger wake-up,
- restart recovery.

Do not mark quota waiting as task failure.

**Completion criterion:** both-exhausted integration test stays queued and later starts automatically.

---

## Task 9: Runtime Quota Failure Failover

Implement normalized runner failure classification.

For auto-routed steps only:

1. classify actual quota failure,
2. refresh provider state,
3. exclude failed provider,
4. find fallback,
5. retry same step in same worktree,
6. inject continuation preamble,
7. preserve workflow retry budget.

Prevent loops.

**Completion criterion:** simulated Claude quota failure transparently resumes on Codex.

---

## Task 10: CLI Usage Commands

Implement:

```bash
cezar usage
cezar usage --json
cezar usage --refresh
```

Update CLI help.

**Completion criterion:** commands function against fake adapters and produce stable JSON suitable for scripts.

---

## Task 11: Cockpit UI

Implement:

- usage display,
- Auto runner picker,
- current resolution preview,
- resolved-runner display,
- blocked quota state,
- routing configuration,
- manual refresh,
- stale telemetry state.

Do not expose credentials.

**Completion criterion:** UI unit/e2e tests pass.

---

## Task 12: Security and Resilience

Add explicit tests for:

- token redaction,
- malformed endpoint responses,
- auth expiration,
- provider timeout,
- partial telemetry,
- stale cache,
- provider endpoint outage,
- Cezar restart,
- concurrent routing calls.

Audit all persisted files for credential leakage.

**Completion criterion:** security/resilience test suite passes.

---

## Task 13: Documentation

Document:

### User guide

```text
Enabling Auto routing
Viewing quota
Changing provider order
Changing thresholds
Understanding waiting tasks
Manual provider pinning
Troubleshooting stale usage
```

### Developer guide

Document:

```text
UsageProviderAdapter
ProviderUsageService
QuotaRouter
failure classification
routing events
testing with fake providers
```

Include a warning that quota-data availability depends on provider tooling and may change independently of Cezar.

**Completion criterion:** feature is understandable without reading source code.

---

## Task 14: Final Regression and Cleanup

Run the complete applicable repository test suite.

At minimum:

```bash
npm run typecheck
npm test
npm run test:unit
npm run build
```

Also run the current package and E2E gates if supported in the environment.

Inspect:

```bash
git diff
git status
```

Confirm:

- no auth files,
- no tokens,
- no generated credentials,
- no debug fixtures containing real account data,
- no accidental breaking changes.

Produce a final implementation report containing:

```text
Files changed
Architecture
Tests run
Known limitations
Manual verification steps
Future improvements
```

---

# 59. Commit Strategy

Prefer one logical commit per implementation task.

Example:

```text
docs: map quota-routing integration points
feat: add normalized provider usage model
feat: implement deterministic quota router
feat: add provider usage service
feat: add Claude usage adapter
feat: add Codex usage adapter
feat: support auto runner selection
feat: route auto steps by provider quota
feat: pause queue when provider quota unavailable
feat: fail over auto steps after quota exhaustion
feat: add usage CLI
feat: add quota routing cockpit UI
test: harden quota routing security and resilience
docs: document quota-aware auto routing
```

Do not mix unrelated cleanup into these commits unless necessary.

---

# 60. Codex Working Instructions

When implementing this specification:

1. Treat the checked-out Cezar fork as the source of truth.
2. Do not assume this document's suggested new file paths must be followed exactly.
3. Follow existing repository conventions when they conflict with suggested organization.
4. Preserve backward compatibility.
5. Do not redesign unrelated Cezar systems.
6. Prefer small interfaces and pure policy functions.
7. Keep provider-specific parsing isolated.
8. Do not duplicate runner implementations.
9. Do not introduce a second task scheduler.
10. Do not add a database.
11. Do not require API keys.
12. Do not make tests consume subscription quota.
13. Do not expose credentials.
14. Run relevant tests after every task.
15. Fix failures before continuing.
16. Keep commits logically scoped.
17. If provider telemetry is unavailable, degrade gracefully.
18. If a source format is uncertain, implement a fixture-backed adapter boundary rather than allowing uncertainty to leak into the router.
19. Prefer structured provider signals over text matching.
20. Do not silently switch an explicitly pinned provider.

---

# 61. Definition of Success

A developer should eventually be able to configure:

```json
{
  "defaultRunner": "auto"
}
```

queue a large set of autonomous Cezar tasks, and leave the machine unattended.

During execution:

```text
Claude available
    |
    v
Claude receives work
    |
Claude approaches configured quota threshold
    |
    v
new work automatically goes to Codex
    |
Claude resets
    |
    v
new work automatically returns to Claude
```

If both providers are temporarily exhausted:

```text
tasks remain safely queued
        |
        v
Cezar waits
        |
provider becomes eligible
        |
        v
queue resumes automatically
```

If Claude unexpectedly exhausts its quota during an auto-routed task:

```text
partial work remains in worktree
        |
        v
Codex inspects existing state
        |
        v
Codex continues the same workflow step
```

The user should no longer need to:

```text
watch quota bars
switch Claude -> Codex manually
switch Codex -> Claude manually
type "next task"
remember provider reset times
restart tasks solely because a subscription window was exhausted
```

The human's role becomes specification, review, and intervention on genuinely ambiguous engineering decisions rather than supervision of agent availability.