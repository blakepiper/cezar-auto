import type { RunnerId } from './agent-runner.ts';

/**
 * A requested runner may be a concrete executable backend or the quota-aware
 * selection policy. `auto` deliberately does not belong in `RUNNER_IDS`:
 * runner construction, sessions, models, and persisted backend affinity all
 * require a concrete backend.
 */
export type RunnerSelection = RunnerId | 'auto';

export const AUTO_PROVIDER_IDS = ['claude', 'codex'] as const;
export type AutoProvider = (typeof AUTO_PROVIDER_IDS)[number];

export function isAutoProvider(value: string): value is AutoProvider {
  return (AUTO_PROVIDER_IDS as readonly string[]).includes(value);
}

