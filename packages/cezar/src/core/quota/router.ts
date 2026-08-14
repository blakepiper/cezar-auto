import type { AutoProvider } from '../runner-selection.ts';
import type { ProviderUsageSnapshot, ProviderUsageWindow } from './types.ts';

export type { ProviderQuotaHealth, ProviderUsageSnapshot, ProviderUsageWindow } from './types.ts';

export interface ProviderRoutingPolicy {
  enabled: boolean;
  stopNewWorkAtPercent: number;
  longWindowStopAtPercent: number;
  resumeBelowPercent: number;
  maxConcurrent: number;
}

export interface QuotaRoutingPolicy {
  enabled: boolean;
  providerOrder: readonly AutoProvider[];
  unknownUsagePolicy: 'allow' | 'deny';
  providers: Readonly<Record<AutoProvider, ProviderRoutingPolicy>>;
}

export interface ProviderRoutingState {
  available: boolean;
  authenticated: boolean;
  activeCount: number;
  snapshot?: ProviderUsageSnapshot;
  /** A provider soft-stopped by a prior observation stays stopped until a reset
   * or a usage value below `resumeBelowPercent` proves recovery. */
  softExhausted: boolean;
}

export type ProviderIneligibility =
  | 'disabled'
  | 'unavailable'
  | 'unauthenticated'
  | 'attempted'
  | 'concurrency_full'
  | 'hard_exhausted'
  | 'soft_exhausted'
  | 'auth_error'
  | 'unknown_usage';

export interface ConsideredProvider {
  provider: AutoProvider;
  eligible: boolean;
  reason?: ProviderIneligibility;
}

export type RoutingDecision =
  | {
    kind: 'selected';
    provider: AutoProvider;
    considered: ConsideredProvider[];
    /** Coordinator-owned next hysteresis state. */
    softExhausted: ReadonlySet<AutoProvider>;
  }
  | {
    kind: 'wait';
    considered: ConsideredProvider[];
    retryAt?: string;
    softExhausted: ReadonlySet<AutoProvider>;
  }
  | {
    kind: 'error';
    message: string;
    considered: ConsideredProvider[];
    softExhausted: ReadonlySet<AutoProvider>;
  };

export interface RouteAutoStepInput {
  policy: QuotaRoutingPolicy;
  providers: Readonly<Record<AutoProvider, ProviderRoutingState>>;
  /** Providers that already ended this same workflow step with a confirmed
   * quota failure. The coordinator clears this set only on a new recovery
   * generation. */
  attemptedProviders?: ReadonlySet<AutoProvider>;
}

function earliestReset(snapshot: ProviderUsageSnapshot | undefined): string | undefined {
  const times = (snapshot?.windows ?? [])
    .map((window) => window.resetsAt)
    .filter((value): value is string => value !== undefined && Number.isFinite(Date.parse(value)))
    .sort();
  return times[0];
}

function windowExhausted(window: ProviderUsageWindow, policy: ProviderRoutingPolicy): boolean {
  if (window.hardLimitReached) return true;
  if (window.usedPercent === null) return false;
  const threshold = window.kind === 'long' || window.kind === 'model'
    ? policy.longWindowStopAtPercent
    : policy.stopNewWorkAtPercent;
  return window.usedPercent >= threshold;
}

function recoveredBelowResume(snapshot: ProviderUsageSnapshot, resumeBelowPercent: number): boolean {
  const measured = snapshot.windows
    .map((window) => window.usedPercent)
    .filter((used): used is number => used !== null);
  return measured.length > 0 && measured.every((used) => used < resumeBelowPercent);
}

/**
 * Deterministically choose the first eligible automatic provider. This is
 * intentionally synchronous: refreshes, locks, reservations, and persistence
 * belong to the coordinator around this function.
 */
export function routeAutoStep(input: RouteAutoStepInput): RoutingDecision {
  const considered: ConsideredProvider[] = [];
  const softExhausted = new Set<AutoProvider>();
  const retryAt: string[] = [];
  const attempted = input.attemptedProviders ?? new Set<AutoProvider>();

  if (!input.policy.enabled) {
    return { kind: 'error', message: 'quota-aware routing is disabled', considered, softExhausted };
  }

  for (const provider of input.policy.providerOrder) {
    const settings = input.policy.providers[provider];
    const state = input.providers[provider];
    const snapshot = state.snapshot;
    const reset = earliestReset(snapshot);
    if (reset) retryAt.push(reset);

    let reason: ProviderIneligibility | undefined;
    if (!settings.enabled) reason = 'disabled';
    else if (!state.available) reason = 'unavailable';
    else if (!state.authenticated) reason = 'unauthenticated';
    else if (attempted.has(provider)) reason = 'attempted';
    else if (state.activeCount >= settings.maxConcurrent) reason = 'concurrency_full';
    else if (!snapshot || snapshot.stale || snapshot.health === 'unknown') {
      if (input.policy.unknownUsagePolicy === 'deny') reason = 'unknown_usage';
    } else if (snapshot.health === 'auth_error') {
      reason = 'auth_error';
    } else if (snapshot.health === 'unavailable') {
      reason = 'unavailable';
    } else if (snapshot.health === 'hard_exhausted' || snapshot.windows.some((window) => window.hardLimitReached)) {
      reason = 'hard_exhausted';
    } else {
      const exhausted = snapshot.health === 'soft_exhausted'
        || snapshot.windows.some((window) => windowExhausted(window, settings));
      const heldByHysteresis = state.softExhausted && !recoveredBelowResume(snapshot, settings.resumeBelowPercent);
      if (exhausted || heldByHysteresis) {
        softExhausted.add(provider);
        reason = 'soft_exhausted';
      }
    }

    if (reason) {
      considered.push({ provider, eligible: false, reason });
      continue;
    }
    considered.push({ provider, eligible: true });
    return { kind: 'selected', provider, considered, softExhausted };
  }

  retryAt.sort();
  return {
    kind: 'wait',
    considered,
    ...(retryAt[0] !== undefined ? { retryAt: retryAt[0] } : {}),
    softExhausted,
  };
}
