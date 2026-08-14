import { describe, expect, it } from 'vitest';
import { routeAutoStep, type ProviderRoutingState, type QuotaRoutingPolicy, type ProviderUsageSnapshot } from './router.ts';

const policy: QuotaRoutingPolicy = {
  enabled: true,
  providerOrder: ['claude', 'codex'],
  unknownUsagePolicy: 'allow',
  providers: {
    claude: { enabled: true, stopNewWorkAtPercent: 90, longWindowStopAtPercent: 95, resumeBelowPercent: 80, maxConcurrent: 1 },
    codex: { enabled: true, stopNewWorkAtPercent: 90, longWindowStopAtPercent: 90, resumeBelowPercent: 80, maxConcurrent: 1 },
  },
};

const usage = (overrides: Partial<ProviderUsageSnapshot> = {}): ProviderUsageSnapshot => ({
  provider: 'claude',
  profileId: 'default',
  fetchedAt: '2026-08-14T00:00:00.000Z',
  source: 'fake',
  health: 'available',
  stale: false,
  windows: [{ kind: 'short', usedPercent: 20 }],
  ...overrides,
});

const available = (overrides: Partial<ProviderRoutingState> = {}): ProviderRoutingState => ({
  available: true,
  authenticated: true,
  activeCount: 0,
  softExhausted: false,
  snapshot: usage(),
  ...overrides,
});

const input = (claude = available(), codex = available()) => ({
  policy,
  providers: { claude, codex },
});

describe('routeAutoStep', () => {
  it('prefers Claude whenever it is eligible', () => {
    expect(routeAutoStep(input())).toMatchObject({ kind: 'selected', provider: 'claude' });
  });

  it('falls back to Codex when Claude reaches its start threshold', () => {
    const decision = routeAutoStep(input(available({ snapshot: usage({ windows: [{ kind: 'short', usedPercent: 90 }] }) })));
    expect(decision).toMatchObject({ kind: 'selected', provider: 'codex' });
    expect(decision.softExhausted).toEqual(new Set(['claude']));
  });

  it('keeps a soft-stopped provider unavailable until it is below the resume threshold', () => {
    const held = routeAutoStep(input(available({
      softExhausted: true,
      snapshot: usage({ windows: [{ kind: 'short', usedPercent: 85 }] }),
    })));
    expect(held).toMatchObject({ kind: 'selected', provider: 'codex' });

    const recovered = routeAutoStep(input(available({
      softExhausted: true,
      snapshot: usage({ windows: [{ kind: 'short', usedPercent: 79 }] }),
    })));
    expect(recovered).toMatchObject({ kind: 'selected', provider: 'claude' });
  });

  it('honours concurrency, same-step failure exclusion, and unknown policy', () => {
    expect(routeAutoStep(input(available({ activeCount: 1 })))).toMatchObject({ kind: 'selected', provider: 'codex' });
    expect(routeAutoStep({ ...input(), attemptedProviders: new Set(['claude']) })).toMatchObject({ kind: 'selected', provider: 'codex' });
    expect(routeAutoStep({
      ...input(available({ snapshot: usage({ health: 'unknown', windows: [] }) })),
      policy: { ...policy, unknownUsagePolicy: 'deny' },
    })).toMatchObject({ kind: 'selected', provider: 'codex' });
  });

  it('does not conflate an authentication error with a quota failure', () => {
    const decision = routeAutoStep(input(available({ snapshot: usage({ health: 'auth_error', windows: [] }) })));
    expect(decision).toMatchObject({ kind: 'selected', provider: 'codex' });
    expect(decision.considered[0]).toEqual({ provider: 'claude', eligible: false, reason: 'auth_error' });
  });

  it('waits with the earliest credible reset when every provider is exhausted', () => {
    const decision = routeAutoStep(input(
      available({ snapshot: usage({ health: 'hard_exhausted', windows: [{ kind: 'short', usedPercent: 100, resetsAt: '2026-08-15T12:00:00.000Z', hardLimitReached: true }] }) }),
      available({ snapshot: usage({ health: 'soft_exhausted', windows: [{ kind: 'long', usedPercent: 95, resetsAt: '2026-08-15T11:00:00.000Z' }] }) }),
    ));
    expect(decision).toMatchObject({ kind: 'wait', retryAt: '2026-08-15T11:00:00.000Z' });
  });

  it('returns an explicit error while routing is disabled', () => {
    expect(routeAutoStep({ ...input(), policy: { ...policy, enabled: false } }))
      .toMatchObject({ kind: 'error', message: 'quota-aware routing is disabled' });
  });
});
