# Intelligent automatic routing and usage-aware recovery

Status: implementation specification and delivery plan

Audience: product, contract, core, client, runner, and TUI implementers

Initial runners: Claude Code, Codex, and OpenCode

State scope: per-user routing state under `~/.coducktor/`; per-run decisions in repository run state

## 1. Outcome

Coducktor's `auto` choices must become a trustworthy fire-and-forget policy. For every agent step
authored with automatic settings, Coducktor will:

1. choose an eligible runner and account without crossing configured usage reserves;
2. let the chosen runner resolve a concrete model and reasoning effort by its own defaults;
3. start promptly from cached state rather than waiting on quota probes;
4. explain and persist the decision;
5. detect a real usage-limit failure, fail over safely, and avoid spending workflow retry budget;
6. wait durably when no candidate is usable; and
7. resume promptly after capacity returns, including after Coducktor restarts.

Settings will expose the same sanitized usage and limit data used by the router. A user must be able
to see what is known, when it was observed, where it came from, and when Coducktor is operating from
incomplete information.

This is not a single opaque AI call. It is a deterministic, testable routing system composed of
usage adapters, eligibility policy, scoring, a reservation coordinator, decision explanations, and
a durable recovery loop.

**Deliberately not part of this system**: a task profiler and a model capability registry — see
[§8](#8-task-intelligence-deliberately-not-built) for why. `auto` ranks candidates on usage health,
configured priority, and connectivity; it does not read task content to infer complexity, and it
does not maintain a per-model capability catalog. Model and reasoning selection under `auto`
delegate to the chosen runner's own defaults.

## 2. Product principles

### 2.1 Fast on the critical path

Submission and step dispatch must never synchronously call a provider usage endpoint, start a
monitor-only model request, or wait for a catalog refresh. Provider status, catalogs, and usage
snapshots refresh in the background. Routing consumes an immutable cache snapshot.

Warm routing policy evaluation must take less than 10 ms at p95 in a release build. The additional
submit-to-runner-spawn latency attributable to automatic routing must remain below 50 ms at p95.

### 2.2 Honest about incomplete telemetry

The three initial runners do not expose equivalent quota interfaces. Coducktor must distinguish:

- authoritative provider limit data;
- limit data observed through a supported local runner interface;
- a limit inferred from a confirmed runtime failure;
- Coducktor-recorded token and cost consumption; and
- unknown usage.

Missing data is state, not zero usage. The UI must never render an unknown limit as an empty or
fully available progress bar.

### 2.3 Quality before cleverness

Routing is deterministic scoring over cached data, not an extra LLM call to decide which LLM to
call. Such a call adds latency, spends the resource being scheduled, creates a new failure mode
when accounts are exhausted, and makes decisions harder to reproduce. This guardrail is why a task
profiler is out of scope ([§8](#8-task-intelligence-deliberately-not-built)): the moment routing
tries to understand task content, it invites exactly this failure mode.

### 2.4 Explicit choices remain explicit

Automatic routing applies only where the effective authored selection is `auto`. Explicit runner,
model, reasoning, or account choices retain their current meaning. An explicit runner must not be
silently changed because another runner has more capacity.

The independent picker choices are:

| Picker | `auto` meaning |
| --- | --- |
| Runner | Choose a runner and eligible account for this agent step. |
| Model | Choose a concrete model after the runner is known. |
| Reasoning | Choose a supported effort after the model is known. |

A user may combine them. For example, `runner=codex`, `model=auto`, and `reasoning=high` pins Codex,
lets Coducktor choose a Codex model, and requires high effort. If a requested combination is not
supported, submission fails with a useful conflict rather than silently weakening the request.

### 2.5 Provider failure is not workflow failure

A confirmed usage-limit failure is an execution-resource event. It does not consume a workflow
step's `onFail` retry allowance and does not immediately mark the task failed. Coducktor first tries
another eligible route or parks the task until a reset.

### 2.6 No new network surface

Coducktor continues to rely on installed local agent CLIs and their supported local protocols. It
does not read raw OAuth tokens, call private provider endpoints, add a Coducktor service, or open a
listening socket. OpenCode's existing short-lived local `serve` process remains owned by the
OpenCode runner.

## 3. Scope

### 3.1 Required in the first release

- Claude Code, Codex, and OpenCode as automatic runner candidates.
- Multiple configured Claude and Codex profiles as opt-in automatic account candidates.
- The default OpenCode installation/profile as an automatic candidate.
- Background, cached usage collection from supported local interfaces.
- Runtime limit detection for every initial runner.
- Process-wide reservations and per-route concurrency limits.
- Same-step failover and durable wait/resume behavior.
- A Settings usage view with manual refresh and clear freshness/provenance labels.
- `duck usage` and `duck usage --json` backed by the same engine response.
- Durable, sanitized routing decisions and usage snapshots.
- Decision explanations in the task transcript and task details.
- Zero-configuration degradation when a CLI, account, catalog, or telemetry source is missing.

### 3.2 Not in the first release

- A task profiler or a model capability registry, in this or any later release — see
  [§8](#8-task-intelligence-deliberately-not-built).
- Pi automatic routing.
- Direct calls to provider billing or private subscription endpoints.
- Automatically buying credits or enabling paid overage.
- Exact prediction of the tokens a task will consume.
- Switching a healthy runner in the middle of a turn merely because a threshold was crossed.
- Treating OpenCode's many upstream credentials as independently selectable Coducktor profiles.
- An LLM-based routing classifier.
- Cross-process reservations. Two separately running Coducktor processes remain advisory to one
  another; provider-side limits and runtime failure handling are the final backstop.

## 4. Current baseline and gaps

The current Rust application already has useful pieces:

- `RunnerSelection::Auto` in `coducktor-contract`;
- Claude, Codex, OpenCode, and pi runner implementations;
- Claude and Codex account profiles and per-project account selection;
- host model discovery for Codex and OpenCode;
- model-advertised Codex reasoning efforts;
- normalized per-run token and cost events;
- durable `autoResumeAt`, account holds, and reconciliation;
- quota configuration shapes and normalized usage contracts; and
- Auto runner selection and Settings controls for auto-resume.

The first quota-aware slice is now connected:

- `workspace_usage` probes Codex's local app-server and OpenCode Go's authenticated usage endpoint
  through bounded cached requests; Claude and OpenCode upstreams without a supported quota
  interface remain unknown rather than zero;
- `duck usage`, `duck usage --json`, and Settings → Resources render that shared sanitized view; and
- when the Composer selects the `Auto` runner, it ranks known available headroom above unknown capacity
  instead of always allowing the legacy Claude-first order to win; and
- runner Auto retains the ranked connected-provider list, emits its choices into the run event
  stream, and retries the unchanged opening prompt on the next provider after a classified quota,
  authentication, capacity, or startup failure; and
- the routing decision itself — every candidate considered, its eligibility, its reason, and which
  one was selected — is built once per run creation and persisted on the run's first step
  (`StepState::routingDecision`), with a one-line summary announced into the transcript. This is
  §10.4's explanation, scoped to the single resolve-at-creation decision the engine actually makes
  today; it does not yet cover step-level re-resolution or same-step failover attempts.

The broader coordinator work remains incomplete:

- runner Auto is resolved once at run creation rather than at each automatic step;
- model Auto and reasoning Auto omit overrides and delegate to the runner;
- the TUI renders runner Auto through Claude's model picker;
- current recovery retries the same persisted runner/account;
- live Claude session observations and OpenCode local consumption have not yet been harvested; and
- reservations, concurrency caps, route-specific model policy, step/Continue re-routing, and
  durable cross-restart failover still need the shared coordinator described below.

A task profiler and model capability registry are not gaps to close — they are deliberately not
being built. See [§8](#8-task-intelligence-deliberately-not-built).

Repository history contains a prior TypeScript quota service, policy router, coordinator,
provider-failure classifier, sanitized snapshot store, blocked queue, and settings UI. The Rust
implementation should port the proven invariants and tests, not the deleted server/browser
architecture.

## 5. Provider-interface reality

Provider adapters are capability-specific. A source may contribute connection state, quota
windows, consumption history, model metadata, or only runtime failures.

### 5.1 Codex

The installed Codex app-server schema exposes structured `account/rateLimits/read`, sparse
`account/rateLimits/updated` notifications, reset windows, limit IDs, and structured
`usageLimitExceeded` errors. Its model catalog also advertises supported reasoning efforts.

Codex is therefore the reference implementation for proactive quota routing:

- query each eligible `CODEX_HOME` profile through a bounded local app-server session;
- normalize the returned windows and reset instants;
- merge sparse notifications into the last full snapshot;
- consume runtime `usageLimitExceeded` as authoritative exhaustion; and
- never persist raw app-server payloads.

### 5.2 Claude Code

Claude Code documents five-hour and seven-day `rate_limits` fields in status-line input after the
first API response in a subscriber session, and its limit errors contain reset times. It does not
currently expose a documented cold-start, non-interactive quota command suitable for Coducktor.

The Claude adapter therefore has two levels:

1. harvest structured `rate_limits` fields from real Coducktor-owned Claude sessions if a phase-zero
   characterization proves this can be injected into print mode without editing user settings;
2. always recognize confirmed plan-limit failures and their reset time from the runner's bounded,
   sanitized error classification.

Coducktor must not issue a paid prompt merely to obtain a quota reading. Until a real session has
provided telemetry, the account limit is `unknown`. `/usage` is interactive and must not be scraped.

### 5.3 OpenCode

OpenCode is both a runner and a multiprovider client. Its local server exposes configured and
connected providers and models. `opencode stats` exposes historical local token/cost consumption,
but neither interface provides a universal quota-window API for every possible upstream provider.

OpenCode Go is the concrete exception: when the existing `opencode-go` credential is present in
OpenCode's XDG data directory, Coducktor calls its authenticated usage endpoint with an eight-second
default bound and normalizes the returned rolling, weekly, and monthly percentages and reset times.
The credential and raw response never enter the usage contract or durable state. Failed auth,
network, oversized, or malformed responses degrade to unknown rather than zero.

For the first release:

- OpenCode is a full routing candidate only when a concrete `provider/model` can be selected;
- model capabilities come from the OpenCode catalog directly;
- connection state comes from the existing local OpenCode probe/server;
- Coducktor-recorded usage and, where a stable parseable interface exists, OpenCode local usage are
  displayed as consumption, not mislabeled as remaining quota;
- provider-auth and rate-limit failures are normalized at runtime;
- a confirmed limit holds the route key conservatively until a known reset or a successful probe/run;
- absent upstream quota data remains `unknown` and receives a configurable score penalty; and
- OpenCode's default model with no concrete identity is allowed only as a lower-confidence fallback.

An OpenCode route is keyed by runner, OpenCode profile, and upstream provider/model family. It must
not be assumed to share quota with the same user's Claude Code or Codex account because Coducktor
cannot safely prove credential identity.

### 5.4 Verified primary references

- Claude Code status-line fields and update behavior:
  <https://code.claude.com/docs/en/statusline>
- Claude Code usage-limit/error distinctions:
  <https://code.claude.com/docs/en/errors>
- OpenCode local server and provider interfaces:
  <https://dev.opencode.ai/docs/server/>
- OpenCode CLI `stats`, auth, and model commands:
  <https://dev.opencode.ai/docs/cli/>
- OpenCode's multiprovider model:
  <https://opencode.ai/docs/providers>
- Codex app-server behavior is verified from the installed CLI's generated JSON schema during
  implementation; fixtures must record the supported protocol rather than linking to an unstable
  generated artifact.

## 6. Terminology and identities

The implementation must keep authored policy separate from executable identity.

```text
RunnerSelection        auto | claude | codex | opencode | pi
Runner                 claude | codex | opencode | pi
AccountKey             Runner + profile ID
RouteKey               AccountKey + concrete model + optional upstream provider
Candidate              one executable runner/account/model/reasoning combination
RoutingDecision        selected candidate or a durable wait decision
ProviderUsageSnapshot  sanitized evidence about one account/route's limits
RouteLease             process-wide reservation held while a runner is active
RecoveryGeneration     one bounded set of attempts between evidence changes
```

`RunRecord.runner` and `StepState.backend` remain concrete. `requestedRunner` preserves `auto`.
The selected profile, model, reasoning, route key, and decision explanation are recorded per step.

## 7. User experience

### 7.1 New task

The runner, model, and reasoning pickers retain Auto. When runner is Auto, the summary reads:

```text
Auto · Chooses runner and account for each step
```

Model and reasoning Auto delegate to the chosen runner's own defaults; the picker does not claim to
choose them, and it must not show Claude's model catalog while runner Auto is selected. Instead it
shows an automatic-policy explanation and an optional preview computed from the current cache:

```text
Likely route: Codex · work
Claude personal is reserved at 91% weekly usage
```

The preview is advisory. Dispatch re-evaluates the route because usage or concurrency may change.

### 7.2 Run transcript and details

Every automatic decision emits one compact normalized event:

```text
Auto routing · selected Codex — also considered: Claude (reserved quota)
```

The collapsed row shows the outcome. Expanded details show every considered candidate and its
sanitized reason: `selected`, `considered`, `disabled`, `not_installed`, `disconnected`,
`auth_error`, `reserved_quota`, `hard_exhausted`, or `unknown_usage`. No credential paths, raw
provider payloads, or private error text appear.

When waiting:

```text
Waiting for provider capacity · next known reset 14:00
```

When failing over:

```text
Claude personal reached its five-hour limit · continuing this step with OpenCode
```

### 7.3 Settings: Provider usage and Auto routing

Add a `Provider usage & Auto routing` section under Settings → Resources. It contains:

```text
AUTO ROUTING                                      Enabled
Routing health                                   5/6 candidates ready
Last background refresh                          18s ago

CLAUDE CODE
  Personal     Connected   5 hour  72%   resets 14:00
                           7 day   91%   resets Monday   Reserved
  Work         Connected   Limits unknown; no session observation yet

CODEX
  Personal     Connected   Primary 34%   resets 13:21
                           Weekly  62%   resets Sunday

OPENCODE
  Default      Connected   Quota unavailable from configured upstreams
                           Coducktor-recorded: 1.2M tokens / $8.40 this month
```

Each row includes:

- connection and authentication health;
- Auto eligibility toggle;
- priority or preference weight;
- quota windows with used percentage and reset time when known;
- `Available`, `Reserved`, `Exhausted`, `Unknown`, `Auth error`, or `Unavailable` health;
- source and observation time in details;
- Coducktor-recorded consumption, clearly scoped and labeled;
- the last sanitized error and suggested remediation; and
- a non-blocking `Refresh now` action.

OpenCode expands into concrete upstream routes. Its configured default route is eligible when the
user enables OpenCode for Auto; every additional paid or remote `provider/model` route requires an
explicit Auto-eligibility toggle. This prevents model discovery from silently turning every API key
in an OpenCode installation into spendable automatic capacity.

Settings also exposes policy controls with safe defaults:

- enable intelligent Auto routing;
- preserve this percentage of short and long windows;
- behavior when usage is unknown: `allow with penalty` or `exclude`;
- maximum concurrent sessions per account;
- quality preference: `Economy`, `Balanced`, or `Best available`;
- automatic failover on confirmed quota failure;
- automatic resume when capacity returns; and
- maximum automatic route attempts per recovery generation.

Advanced model-family rules remain in configuration in the first release. The TUI need not become
a general scoring-rule editor.

### 7.4 Headless usage

Restore:

```text
duck usage
duck usage --json
duck usage --refresh
```

`--refresh` requests bounded background probes and waits only for those probes, not for a model
call. It exits nonzero only when the command itself cannot produce a response; unknown provider
usage is valid output.

## 8. Task intelligence (deliberately not built)

Earlier drafts of this spec called for two additional subsystems: a deterministic task profiler
(inferring complexity/risk/breadth from prompt text and signals to pick a quality tier) and a
versioned model capability registry (a cross-provider quality/speed/context scale keyed by model
family). Both are struck from this plan, not deferred:

- **Task profiler** — task classification from lexical signals is inherently fuzzy. A mediocre
  scorer is worse than none: it makes `auto`'s choices feel arbitrary in exactly the cases where a
  user is watching closely enough to notice, and there is no evidence yet that quality-tier
  mis-selection is a real problem worth this complexity. If a concrete complaint ever justifies
  revisiting this, it should start from real usage data produced by the simpler router this spec
  now describes, not from a design written in advance of any evidence.
- **Model capability registry** — mechanical and lower-risk than the profiler, but still not
  worth the maintenance cost (a versioned per-model-family catalog that needs updating as new
  models ship) against what it would buy: today's reactive failover already turns an unsupported
  capability into a same-step retry on the next candidate. A registry would only upgrade that from
  reactive to proactive.

Model and reasoning selection under `auto` therefore delegate to the chosen runner's own defaults
for the foreseeable future, and every reason code this router produces (§10) is one an eligibility
or usage check can actually justify — never one that implies the router understood the task.

## 9. Usage and quota model

Extend the existing workspace usage contract rather than exposing runner wire types:

```rust
struct ProviderUsageSnapshot {
    runner: Runner,
    profile_id: String,
    upstream_provider: Option<String>,
    health: ProviderUsageHealth,
    confidence: UsageConfidence,
    fetched_at: String,
    source: UsageSource,
    stale: bool,
    windows: Vec<ProviderUsageWindow>,
    consumption: Option<UsageAggregate>,
    error: Option<ProviderUsageError>,
}

enum UsageConfidence { Authoritative, Observed, Inferred, Unknown }

struct ProviderUsageWindow {
    id: Option<String>,
    kind: ProviderUsageWindowKind,
    used_percent: Option<f64>,
    resets_at: Option<String>,
    hard_limit_reached: Option<bool>,
}
```

`UsageAggregate` states its scope (`Coducktor only`, `OpenCode local history`, or provider account),
period, input/output/reasoning/cache tokens, and cost only when reported. Unknown costs remain
absent; Coducktor does not fabricate cross-provider cost conversions.

### 9.1 Health derivation

- `Available`: fresh evidence and below every configured stop threshold.
- `Reserved`: at/above a soft threshold but below a confirmed hard limit.
- `Hard exhausted`: structured provider evidence or a conservatively classified runtime failure.
- `Auth error`: profile cannot authenticate.
- `Unavailable`: executable/protocol unavailable.
- `Unknown`: no trustworthy current limit evidence.

Use hysteresis. An account crossing a stop threshold remains reserved until every measured window
falls below its configured resume threshold or a reset produces fresh evidence. This prevents
flapping around 90%.

### 9.2 Persistence

Persist sanitized snapshots in `~/.coducktor/provider-usage.json` using the repository's atomic
`0600` read-modify-write rules. Preserve unknown top-level and per-entry keys. Parse entries with
per-entry salvage. Leave a corrupt file in place, warn once, and boot with an empty stale cache.

Restored snapshots always begin stale. They may explain prior holds but do not become authoritative
until refreshed or corroborated by a not-yet-expired runtime reset.

Never persist credentials, raw provider responses, full stderr/stdout, authorization headers,
account email addresses unless already user-authored as a profile label, or hashes derived from
secrets.

## 10. Candidate construction and scoring

### 10.1 Candidate set

Build candidates immediately before each automatic agent step:

1. enumerate enabled Claude profiles selected for Auto;
2. enumerate enabled Codex profiles selected for Auto;
3. add the default OpenCode profile when enabled;
4. enumerate concrete compatible models for each runner, limited to explicitly eligible OpenCode
   upstream routes;
5. enumerate only supported reasoning levels; and
6. create a runner-native-default candidate only when model settings are locked or no reliable
   concrete model is available.

Additional Claude/Codex profiles are not automatically eligible merely because they exist. The
user must opt them into Auto so Coducktor does not unexpectedly consume a work or paid account.
The currently selected project profile is eligible by default.

### 10.2 Hard eligibility filters

Reject a candidate before scoring when:

- its runner or profile is disabled, missing, disconnected, or unauthenticated;
- its model conflicts with the runner;
- a fresh hard limit or durable runtime hold applies;
- its per-account concurrent lease limit is full;
- it was already attempted in this recovery generation;
- the user's unknown-usage policy excludes it;
- it violates a pinned picker value; or
- session continuation requires an unavailable foreign session and fresh handoff is forbidden by
  the current operation.

### 10.3 Stable score

Score remaining candidates using bounded integer components:

| Component | Range | Purpose |
| --- | ---: | --- |
| Quota headroom | 0..250 | Prefer measured remaining capacity and protect long windows. |
| Continuity | 0..150 | Prefer safely resumable sessions and avoid needless provider changes. |
| User preference | 0..100 | Apply account/runner priority and Economy/Balanced/Best policy. |
| Cost efficiency | 0..75 | Prefer lower relative usage when quality is equivalent. |
| Freshness/confidence | 0..75 | Reward reliable current telemetry and model metadata. |
| Congestion penalty | -100..0 | Spread work away from saturated account slots. |
| Unknown-usage penalty | -150..0 | Allow but demote unknown quota under the default policy. |

Tie-breaking is stable: configured runner priority, configured account priority, model family,
reasoning effort, then lexical route key. The same inputs always produce the same decision.

The current implementation (`quota_aware_routing_decision` in
`crates/coducktor-client/src/in_process.rs`) scores a narrower set — health rank, headroom,
configured priority, configured provider order — sufficient for today's runner-only, resolve-at-
creation decision. Extend it in place as the remaining components above become real (continuity
once same-step failover exists, cost/freshness once consumption data is tracked); do not build a
parallel scorer.

### 10.4 Explanation — implemented

The scorer returns structured reasons, not prose assembled by the runner:

```rust
struct RoutingDecision {
    selected: Option<RouteSelection>,
    considered: Vec<ConsideredCandidate>,
    retry_at: Option<String>,
    generation: u64,
}
```

This is implemented as described: `coducktor_contract::RoutingDecision` (no `task_profile` field —
see §8), built by `finalize_routing_decision`/`connectivity_routing_decision`/
`quota_aware_routing_decision` in `crates/coducktor-client/src/in_process.rs`, persisted on the
run's first step by `RunManager::create_run`, and announced into the transcript by
`routing_decision_note` in `crates/coducktor-core/src/workflows/run/mod.rs`. `considered` is
bounded to the three routing candidates (Claude, Codex, OpenCode) today, matching "a bounded top
set of rejected candidates."

## 11. Runtime architecture

```text
installed CLIs and live runner events
                 |
                 v
         Usage adapters
     Claude  Codex  OpenCode
                 |
                 v
   ProviderUsageService (process-wide)
   cache, persistence, refresh, reset wakeups
                 |
                 +------------------+
                 |                  |
                 v                  v
          Settings usage    AutoRoutingCoordinator (process-wide)
                          eligibility, scoring, serialized reservations
                                    |
                                    v
                          RunManager step dispatch
                                    |
                                    v
                     existing SessionFactory and runners
```

### 11.1 Ownership

Construct one usage service and one routing coordinator per Coducktor process. Share them across
the boot project and every lazily created project manager. A per-project coordinator would permit
multiple projects to stampede the last account capacity.

The coordinator owns only routing leases and policy state. `RunManager` remains the owner of run
lifecycle, durable queue state, worktrees, and workflow transitions.

### 11.2 Layering

- `coducktor-contract`: serde-compatible request, response, persisted decision, usage, and event
  shapes.
- `coducktor-core`: pure scoring, quota reconciliation, failure-independent state transitions, and
  sanitized state persistence.
- `coducktor-client`: local CLI usage adapters, process-wide cache/coordinator wiring, model
  catalog integration, and Engine methods.
- `coducktor-runners`: provider protocol parsing, structured usage observations, and structured
  failure classification only.
- `coducktor-tui`: composer Auto UX, Settings usage rendering, refresh actions, and decision event
  presentation through `Engine`.

Screens never read provider files, spawn CLIs, or inspect runner wire payloads.

## 12. Dispatch lifecycle

For each agent step:

1. Resolve authored runner precedence: continuation override, workflow step, task, project default.
2. If explicit, preserve current concrete behavior and validate pinned model/reasoning.
3. If Auto, build the candidate set from cached catalogs/status/usage.
4. Serialize `evaluate → select → reserve` in the process-wide coordinator.
5. If selected, persist the decision and concrete step affinity before spawning.
6. Resolve the account environment, model, and reasoning from the selected candidate.
7. Open the existing runner session and hold the route lease until the session settles.
8. Feed runner usage observations back into the usage service.
9. Release the route lease on every completion, failure, cancellation, panic boundary, and spawn
   error.
10. Wake parked queues after a lease release or meaningful usage/status change.

Runner Auto is therefore resolved per agent step, not once when a run is created. A later workflow
step may return to a preferred runner after its quota recovers.

## 13. Failure classification and failover

### 13.1 Failure classes

Runners return a provider-neutral failure class alongside the sanitized message:

```rust
enum AgentFailureClass {
    UsageLimit { reset_at: Option<String>, scope: Option<String> },
    Authentication,
    TransientThrottle,
    ProviderUnavailable,
    ModelUnavailable,
    ContextLimit,
    UserOrToolFailure,
    Unknown,
}
```

Prefer structured protocol codes. Bounded compatibility patterns may recognize documented Claude
and OpenCode messages. Patterns must distinguish subscription exhaustion from temporary server
throttling, context limits, billing/API-key limits, and general 429s. Raw provider text never
becomes a routing decision without a recognized, tested signature.

### 13.2 Same-step failover

On confirmed `UsageLimit` for an Auto step:

1. publish the route hold before releasing its lease;
2. persist the attempt, reset time, and recovery generation;
3. close the failed session and release capacity;
4. re-evaluate the same step excluding routes already attempted in the generation;
5. resume the existing provider session only when runner and profile are unchanged and the session
   is valid there;
6. otherwise start a fresh session in the same worktree with a deterministic handoff; and
7. do not increment workflow `onFail` counters.

The deterministic cross-provider handoff contains the original task, current workflow-step prompt,
completed-step summaries, current Git status/diff stat, changed-file names, verification outcomes,
the last bounded assistant response, and an instruction to inspect existing work before editing.
It does not attempt to translate or copy a foreign provider session.

Default maximum same-generation automatic route attempts is three. The attempt list is route-based,
so another account on the same runner may be tried. A successful turn clears the generation.

Unconfirmed failures follow existing workflow semantics. Coducktor must not bounce among providers
on arbitrary errors.

## 14. Durable wait and intelligent auto-resume

### 14.1 One recovery system

Replace the narrow “failed run with `autoResumeAt`” interpretation with a general durable capacity
wait while retaining compatibility with existing records.

Add an optional routing wait object:

```rust
struct RoutingWait {
    reason: RoutingWaitReason,
    generation: u64,
    attempted_routes: Vec<String>,
    blocked_routes: Vec<BlockedRoute>,
    retry_at: Option<String>,
    created_at: String,
    last_checked_at: String,
    attempts: u32,
}
```

Old failed records with `autoResumeAt` are read as legacy usage-limit waits. The writer emits only
the new shape after migration. The migration is additive, idempotent, non-blocking, and never
deletes old data.

A run waiting only for capacity remains nonterminal and visibly waiting; it is not presented as a
completed failure. A pinned explicit run retains the current opt-in same-route auto-resume behavior.

### 14.2 Wake sources

The wait scheduler has no timer as its source of truth. It reconciles durable records on:

- process startup;
- a known reset deadline;
- a usage snapshot change;
- provider connection/authentication change;
- an account eligibility/settings change;
- a route lease release;
- a model catalog change; and
- a bounded periodic safety sweep.

Only one wake for a run/generation may transition it from waiting to starting. Queue and coordinator
locks must make this atomic within the process.

### 14.3 Recovery rules

- Before `retry_at`, do not probe or repeatedly enqueue a route held by a known deadline.
- At a deadline, mark the prior snapshot stale and refresh in the background; a route may be tried
  when fresh evidence shows recovery or policy allows unknown post-reset state.
- Clear a runtime exhaustion hold only after a reset boundary, a fresh available snapshot, or a
  successful real runner turn on that route.
- Re-run task profiling because the step may now be a continuation/retry, but preserve explicit
  picker constraints.
- Prefer safe same-session continuity, then the highest-scoring fresh candidate.
- Cap automatic recovery attempts at 12 across generations, preserving the current safety bound.
- Apply exponential safety backoff only when no reset is known: 30 s, 60 s, 2 min, 5 min, then 10
  min capped. A meaningful evidence change resets the backoff.
- Never busy-loop on unknown telemetry.
- Cancellation, archival, manual runner selection, or task completion clears the wait.

### 14.4 Restart behavior

At startup, migrations run before engine construction. The manager rebuilds route holds and waiting
queue entries from durable runs, restores sanitized usage snapshots as stale, creates the shared
coordinator, and performs one reconciliation pass. Future deadlines remain parked. Due runs are
woken through the normal queue, never executed directly inside migration or startup recovery.

## 15. Configuration

Extend `~/.coducktor/config.json` under the existing `quotaRouting` key. Preserve unknown keys.

```json
{
  "quotaRouting": {
    "qualityPreference": "balanced",
    "unknownUsagePolicy": "allow_with_penalty",
    "maxAutoAttemptsPerGeneration": 3,
    "refreshIntervalSeconds": 60,
    "cacheTtlSeconds": 30,
    "requestTimeoutSeconds": 8,
    "providers": {
      "claude": {
        "enabled": true,
        "priority": 100,
        "stopNewWorkAtPercent": 90,
        "longWindowStopAtPercent": 90,
        "resumeBelowPercent": 80,
        "maxConcurrentPerAccount": 1
      },
      "codex": {
        "enabled": true,
        "priority": 95,
        "stopNewWorkAtPercent": 90,
        "longWindowStopAtPercent": 90,
        "resumeBelowPercent": 80,
        "maxConcurrentPerAccount": 1
      },
      "opencode": {
        "enabled": true,
        "priority": 80,
        "stopNewWorkAtPercent": 90,
        "longWindowStopAtPercent": 90,
        "resumeBelowPercent": 80,
        "maxConcurrentPerAccount": 1
      }
    },
    "accounts": {
      "default:claude": { "autoEligible": true, "priority": 100 },
      "work-claude": { "autoEligible": false, "priority": 80 }
    },
    "routes": {
      "opencode:default:anthropic/claude-sonnet-x": {
        "autoEligible": true,
        "priority": 90
      },
      "opencode:default:openai/gpt-x": {
        "autoEligible": false,
        "priority": 80
      }
    }
  }
}
```

Defaults:

- intelligent routing disabled until telemetry and dispatch integration ship together;
- when enabled through the TUI, selected default Claude, Codex, and OpenCode profiles become
  eligible;
- additional profiles require explicit opt-in;
- OpenCode's configured default model becomes its initial eligible route; other discovered remote
  models require explicit opt-in before Auto may spend against them;
- unknown usage is allowed with a penalty so zero-configuration systems continue to work;
- automatic failover and resume are enabled with intelligent routing; and
- explicit runner behavior is unchanged.

Repo configuration may choose default picker values but must not override machine-wide account
eligibility or quota thresholds. Account and quota policy remains per-user workspace state.

## 16. Contract and state changes

### 16.1 Contract

- Extend `QuotaProvider` with OpenCode or replace quota-only provider fields with the existing
  `Runner` where serde compatibility permits.
- Expand `ProviderUsageSnapshot` with confidence, upstream provider, consumption, and stable window
  IDs.
- Expand `WorkspaceUsageResponse` with overall refresh state and policy health.
- Add routing-decision and routing-wait shapes to runs and normalized events.
- Add concrete selected reasoning to `StepState` if not already persisted there.
- Add Auto eligibility and priority patches for account settings.
- Keep old optional fields readable and default missing additions.

### 16.2 Persisted run affinity

For every automatic step persist:

- requested runner/model/reasoning intent;
- concrete runner, profile, model, and reasoning;
- route key and recovery generation;
- bounded decision reason codes;
- session ID only for the owning runner/profile;
- usage-limit classification and reset time; and
- failover/wait attempts.

The current command, state directory, environment, marker, and branch spellings remain the only
writer vocabulary. Existing marker and task-branch reader compatibility regexes are untouched.

## 17. Security, privacy, and safety

- Use argument arrays and bounded stdout/stderr for every probe.
- Apply the existing curated child environment per runner/profile.
- Never inherit probe or runner output into the user's terminal.
- Use only documented local CLI commands/protocols or real session observations.
- Never inspect or copy raw auth tokens to learn whether two accounts are identical.
- Sanitize error codes/messages before cache, event, or UI storage.
- Cap provider windows, candidates, decision reasons, payload bytes, and probe time.
- Do not automatically opt paid secondary profiles into routing.
- Do not select an API-billed route merely because subscription quota is exhausted unless the user
  explicitly enabled that profile/route.
- A routing failure must not delete worktrees, discard edits, or reset branches.
- Cross-provider handoff starts from the existing filesystem and validates Git state first.

## 18. Observability

Add counters and bounded timing measurements available to tests and debug logs:

- routing evaluations and duration;
- selections by runner/profile/model tier/reasoning;
- exclusion reasons;
- cache age and refresh outcomes by adapter;
- quota holds, wait duration, and wake source;
- same-step failovers and outcomes;
- false-positive overrides, where a held route later succeeds before reset;
- recovery generations and terminal exhaustion; and
- decision-preview versus dispatch-decision changes.

Production logs use profile IDs/labels and normalized reason codes, never raw credentials or
provider bodies. The task transcript carries user-relevant decisions; detailed debug telemetry
stays out of normal transcript output.

## 19. Testing strategy

### 19.1 Pure policy tests

Table-drive quota thresholds, hysteresis, unknown policies, scoring, stable ties, attempted-route
exclusion, and recovery backoff. Include property tests for:

- the same inputs always select the same candidate;
- an ineligible candidate is never selected;
- increasing only a candidate's used percentage cannot improve its score;
- explicit constraints are never relaxed; and
- every wait either has a wake source or a terminal explanation.

### 19.2 Adapter fixtures

- Generate and commit minimal sanitized Codex app-server fixtures for full reads, sparse updates,
  model limit IDs, reset credits, and `usageLimitExceeded`.
- Characterize Claude status-line availability in the exact print/session transport Coducktor uses.
  Test five-hour/seven-day absence, partial windows, malformed values, and documented limit errors.
- Characterize OpenCode provider/model responses, local usage output if parsed, and representative
  auth/quota failures from the mock runner. Do not accept an unversioned prose parser without
  golden fixtures and a safe unknown fallback.
- Fuzz or property-test every provider-controlled parser with size bounds.

### 19.3 Coordinator and concurrency tests

- Two projects share one final account slot and only one obtains it.
- A quota failure is published before lease release, preventing a queue stampede.
- Every error/cancel/panic path releases exactly one lease.
- Stale telemetry does not erase a runtime hard hold.
- A snapshot/reset/status/settings change wakes parked queues once.
- Unknown telemetry neither blocks startup nor creates a busy loop.

### 19.4 Run lifecycle tests

- Auto resolves separately for consecutive workflow steps.
- Explicit step runner overrides task Auto.
- Model and reasoning Auto resolve concretely and are persisted.
- Confirmed quota failure retries the same step without consuming `onFail`.
- Cross-runner failover starts fresh in the same worktree with a bounded handoff.
- Same runner/profile recovery resumes the session.
- All routes exhausted creates a durable wait, not a terminal failure.
- Restart before and after a reset reconstructs the same safe state.
- Manual cancellation/archive/override clears waits and reservations.
- Legacy `autoResumeAt` records migrate and reconcile correctly.

### 19.5 TUI and headless tests

- Auto runner does not display a Claude-specific model catalog.
- Settings distinguishes limits, consumption, stale data, and unknown data.
- Refresh is asynchronous and keeps the UI responsive.
- Narrow terminal layouts retain account, health, and reset information.
- Transcript events explain selection, failover, waiting, and resume.
- `duck usage --json` round-trips through contract fixtures.
- Snapshots are reviewed rather than blindly accepted.

### 19.6 Final gate

Run:

```text
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
cargo tree --workspace
```

Record real terminal behavior in `docs/tui/terminals.md`; headless output is not evidence for the
interactive Settings screen.

## 20. Delivery plan

Each phase lands independently on `main`, preserves zero-configuration startup, and keeps explicit
runner behavior working.

### Phase 0 — characterize local provider seams

Goal: eliminate protocol guesses before state or UI depends on them.

1. Add test-only probes/fixtures for the installed Codex generated app-server schema.
2. Prove whether Claude status-line `rate_limits` can be harvested in Coducktor's print transport
   through ephemeral settings without modifying user files. If not, record Claude proactive quota
   as unavailable and rely on runtime observations.
3. Inspect OpenCode's versioned local server schema for provider/model metadata and determine
   whether `stats` has a stable machine-readable form. Treat prose-only output as unsupported.
4. Enumerate structured and sanitized failure signals for all three runners.
5. Write characterization tests before implementing adapters.

Exit criteria: every first-release adapter capability is classified as supported, observed-only,
or unavailable; no production parser depends on an unbounded human-readable screen.

### Phase 1 — contract, config, and persistence foundations

Likely files:

- `crates/coducktor-contract/src/workspace.rs`
- `crates/coducktor-contract/src/runs.rs`
- `crates/coducktor-contract/src/events.rs`
- `crates/coducktor-core/src/workspace/config.rs`
- `crates/coducktor-core/src/workspace/migrations.rs`
- new core sanitized usage store module

Work:

1. Add OpenCode and the normalized confidence/consumption shapes.
2. Add routing intent, decision, attempt, and wait shapes as optional fields.
3. Expand quota/account policy parsing while preserving unknown keys.
4. Add the atomic `0600` sanitized snapshot store and legacy-run migration.
5. Add contract fixtures and malformed-state salvage tests.

Exit criteria: all new state round-trips, old fixtures still load, corrupt optional state never
blocks startup, and no behavior changes yet.

### Phase 2 — usage service and Settings visibility

Likely files:

- new `crates/coducktor-client/src/usage/` modules
- runner protocol parsers for structured observations
- `crates/coducktor-client/src/in_process.rs`
- `crates/coducktor-client/src/engine.rs`
- `crates/coducktor-tui/src/headless.rs`
- `crates/coducktor-tui/src/screens/settings/`

Work:

1. Implement the process-wide TTL cache, in-flight refresh deduplication, persistence, and events.
2. Implement Codex, Claude-observation, and OpenCode adapters according to Phase 0.
3. Feed live session usage and limit observations back from runners.
4. Implement `workspace_usage`, refresh, `duck usage`, and the Settings usage view.
5. Keep quota policy configuration separate from the Composer's Auto runner choice until the
   coordinator ships.

Exit criteria: users can inspect honest usage/limit state without enabling Auto, refreshes do not
block the TUI, and provider failures degrade to unknown/unavailable.

### Phase 3 — shared coordinator and automatic dispatch

Likely files:

- new core pure scorer/policy modules
- new client process-wide coordinator module
- `crates/coducktor-core/src/workflows/run/mod.rs`
- `crates/coducktor-runners/src/session_factory.rs`
- startup/project manager wiring in client/TUI

Work:

1. Implement hard eligibility, stable scoring, explanations, and route leases.
2. Inject one coordinator into every project manager.
3. Move Auto resolution to each agent-step dispatch site, including Continue.
4. Resolve account, model, and reasoning together and persist concrete affinity before spawn.
5. Emit normalized decision events.
6. Enable the Settings toggle only after all dispatch paths use the coordinator.

Exit criteria: Auto intelligently selects among all three runners and opted-in accounts; explicit
routes are unchanged; multi-project concurrency cannot overbook an in-process route.

### Phase 4 — quota failover and durable intelligent resume

Likely files:

- `crates/coducktor-core/src/workflows/run/quota.rs`
- `crates/coducktor-core/src/workflows/run/auto_resume.rs`
- `crates/coducktor-core/src/workflows/run/mod.rs`
- runner failure parsers
- transcript projection/widgets

Work:

1. Introduce structured runner failure classes.
2. Publish runtime holds before lease release.
3. Add bounded same-step route failover and deterministic cross-provider handoff.
4. Generalize legacy auto-resume into durable routing waits and recovery generations.
5. Wire every wake source and startup reconciliation.
6. Render failover, waiting, and resume decisions in the task transcript.

Exit criteria: confirmed quota failures neither lose work nor consume workflow failure retries;
exhausted tasks wait durably and resume once; restart tests cover every wait boundary.

### Phase 5 — calibration and rollout

1. Run mocked workload matrices spanning task kinds, quota states, accounts, and catalogs.
2. Review routing explanations for surprising or wasteful selections.
3. Calibrate thresholds and scores without changing hard safety floors.
4. Test real terminals and each locally installed runner with non-destructive tasks.
5. Document configuration and the limits of OpenCode/Claude telemetry.
6. Turn intelligent Auto on only for users who explicitly enable it; consider a later default-on
   change after observed false-routing and recovery rates meet the success criteria.

## 21. Acceptance scenarios

The feature is complete only when all scenarios pass. (Two scenarios from earlier drafts —
choosing a model/effort tier by task content — are gone along with the task profiler; see §8.)

1. Claude at 92% of a protected weekly window is reserved; Codex or OpenCode is selected.
2. Codex is hard exhausted and Claude telemetry is unknown; policy `allow_with_penalty` may choose
   Claude and records that uncertainty.
3. OpenCode has no quota API but is connected with a capable concrete model; it remains an eligible,
   lower-confidence fallback and is never shown as 0% used.
4. Two projects simultaneously request the last Codex account slot; one starts and one waits or
   takes another route.
5. Claude hits a documented plan limit mid-step; the hold is visible before the next queue sweep,
   and the step continues with the best untried candidate in the same worktree.
6. Every route is exhausted; the task remains visibly waiting with the earliest known reset.
7. Coducktor restarts while waiting; it reconstructs the hold and resumes once without duplicate
   execution.
8. A user cancels a waiting task; no future refresh or timer restarts it.
9. Explicit Claude + Opus + High never routes to Codex/OpenCode and never changes effort.
10. An opted-out work account is never selected even when every opted-in account is exhausted.
11. Settings and `duck usage --json` agree on values, freshness, provenance, and unknown state.
12. Missing CLIs, malformed cache, unavailable credentials, and offline operation still allow the
   application to start with reduced capabilities.

## 22. Success measures

Collect locally and expose only aggregated debug metrics unless the user asks for details:

- warm routing p95 under 10 ms;
- added warm dispatch p95 under 50 ms;
- no model call made solely for routing or usage refresh;
- no workflow retry consumed by a confirmed quota failure;
- no duplicate automatic resume for a run/generation;
- no route selected below a hard task capability floor;
- every automatic selection has at least one user-readable reason;
- every parked task has a durable wake source;
- false-positive quota classifications are rare and visible in tests/debug data; and
- unknown telemetry is accurately labeled in every UI and CLI representation.

## 23. Implementation guardrails

- Do not append `Auto` to executable runner IDs or feed it to `SessionFactory`.
- Do not resolve Auto once at run creation and reuse it across workflow steps.
- Do not couple provider probing to screen rendering or task submission.
- Do not call private provider quota endpoints or handle raw OAuth credentials.
- Do not conflate OpenCode local token statistics with upstream quota remaining.
- Do not use arbitrary failure text as proof of quota exhaustion.
- Do not overload the old `autoResumeAt` semantics without migrating all readers.
- Do not create one coordinator per repository.
- Do not release a failed route lease before publishing its exhaustion hold.
- Do not resume a session from a different runner or account profile.
- Do not silently lower explicit model/reasoning choices.
- Do not accept snapshot changes without reviewing their semantic routing effect.
