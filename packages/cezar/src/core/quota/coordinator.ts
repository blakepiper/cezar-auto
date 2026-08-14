import type { AutoProvider } from '../runner-selection.ts';
import { routeAutoStep, type QuotaRoutingPolicy, type RoutingDecision } from './router.ts';
import type { ProviderAccountRef } from './types.ts';
import { ProviderUsageService } from './usage-service.ts';

export interface QuotaProviderCandidate {
  account: ProviderAccountRef;
  available: boolean;
  authenticated: boolean;
}

export interface QuotaAcquireInput {
  candidates: Readonly<Record<AutoProvider, QuotaProviderCandidate>>;
  attemptedProviders?: ReadonlySet<AutoProvider>;
}

export interface ProviderLease {
  provider: AutoProvider;
  profileId: string;
  release(): void;
}

export type QuotaAcquireResult = Exclude<RoutingDecision, { kind: 'selected' }> | {
  kind: 'selected';
  provider: AutoProvider;
  decision: RoutingDecision & { kind: 'selected' };
  lease: ProviderLease;
};

function accountKey(account: ProviderAccountRef): string {
  return `${account.provider}:${account.profileId}`;
}

/**
 * Serializes refresh → decision → reservation so two queued runs cannot both
 * observe the final provider slot. It is process-scoped; cross-process usage
 * remains advisory by design.
 */
export class QuotaCoordinator {
  readonly #activeCounts = new Map<string, number>();
  readonly #softExhausted = new Set<string>();
  readonly #wakeListeners = new Set<() => void>();
  #tail: Promise<void> = Promise.resolve();
  readonly #offUsage: () => void;

  constructor(
    private readonly usage: ProviderUsageService,
    private readonly policy: () => QuotaRoutingPolicy,
  ) {
    this.#offUsage = usage.onChange(() => this.#wake());
  }

  onWake(listener: () => void): () => void {
    this.#wakeListeners.add(listener);
    return () => this.#wakeListeners.delete(listener);
  }

  async acquire(input: QuotaAcquireInput): Promise<QuotaAcquireResult> {
    return this.#serialized(async () => {
      const policy = this.policy();
      const snapshots = await Promise.all((['claude', 'codex'] as const).map(async (provider) => {
        const candidate = input.candidates[provider];
        if (!candidate.available || !candidate.authenticated) return [provider, this.usage.get(candidate.account)] as const;
        return [provider, await this.usage.refresh(candidate.account)] as const;
      }));
      const usage = Object.fromEntries(snapshots) as Record<AutoProvider, ReturnType<ProviderUsageService['get']>>;
      const decision = routeAutoStep({
        policy,
        attemptedProviders: input.attemptedProviders,
        providers: {
          claude: this.#state(input.candidates.claude, usage.claude),
          codex: this.#state(input.candidates.codex, usage.codex),
        },
      });
      this.#recordHysteresis(input, decision);
      if (decision.kind !== 'selected') return decision;

      const candidate = input.candidates[decision.provider];
      const key = accountKey(candidate.account);
      this.#activeCounts.set(key, (this.#activeCounts.get(key) ?? 0) + 1);
      let released = false;
      return {
        kind: 'selected',
        provider: decision.provider,
        decision,
        lease: {
          provider: decision.provider,
          profileId: candidate.account.profileId,
          release: () => {
            if (released) return;
            released = true;
            const next = (this.#activeCounts.get(key) ?? 1) - 1;
            if (next <= 0) this.#activeCounts.delete(key);
            else this.#activeCounts.set(key, next);
            this.#wake();
          },
        },
      };
    });
  }

  dispose(): void {
    this.#offUsage();
    this.#wakeListeners.clear();
  }

  #state(candidate: QuotaProviderCandidate, snapshot: ReturnType<ProviderUsageService['get']>) {
    const key = accountKey(candidate.account);
    return {
      available: candidate.available,
      authenticated: candidate.authenticated,
      activeCount: this.#activeCounts.get(key) ?? 0,
      snapshot,
      softExhausted: this.#softExhausted.has(key),
    };
  }

  #recordHysteresis(input: QuotaAcquireInput, decision: RoutingDecision): void {
    for (const candidate of decision.considered) {
      const key = accountKey(input.candidates[candidate.provider].account);
      if (decision.softExhausted.has(candidate.provider)) this.#softExhausted.add(key);
      // Only a fresh eligible evaluation proves recovery. A stale snapshot,
      // full concurrency, or a same-step exclusion must not erase a previous
      // soft stop and let the next queue sweep flap back onto the provider.
      else if (candidate.eligible) this.#softExhausted.delete(key);
    }
  }

  #wake(): void {
    for (const listener of this.#wakeListeners) listener();
  }

  async #serialized<T>(operation: () => Promise<T>): Promise<T> {
    const result = this.#tail.then(operation, operation);
    this.#tail = result.then(() => undefined, () => undefined);
    return result;
  }
}
