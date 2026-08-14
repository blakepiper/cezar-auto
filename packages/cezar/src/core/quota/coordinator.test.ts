import { describe, expect, it, vi } from 'vitest';
import { QuotaCoordinator } from './coordinator.ts';
import type { QuotaRoutingPolicy } from './router.ts';
import { ProviderUsageService, type ProviderUsageAdapter } from './usage-service.ts';

const policy: QuotaRoutingPolicy = {
  enabled: true,
  providerOrder: ['claude', 'codex'],
  unknownUsagePolicy: 'deny',
  providers: {
    claude: { enabled: true, stopNewWorkAtPercent: 90, longWindowStopAtPercent: 95, resumeBelowPercent: 80, maxConcurrent: 1 },
    codex: { enabled: true, stopNewWorkAtPercent: 90, longWindowStopAtPercent: 90, resumeBelowPercent: 80, maxConcurrent: 1 },
  },
};

const candidates = {
  claude: { account: { provider: 'claude' as const, profileId: 'default' }, available: true, authenticated: true },
  codex: { account: { provider: 'codex' as const, profileId: 'default' }, available: true, authenticated: true },
};

function service(): ProviderUsageService {
  const adapter = (provider: 'claude' | 'codex'): ProviderUsageAdapter => ({
    provider,
    read: vi.fn().mockResolvedValue({ health: 'available', source: 'fake', windows: [] }),
  });
  return new ProviderUsageService({ adapters: [adapter('claude'), adapter('codex')], cacheTtlMs: 30_000 });
}

describe('QuotaCoordinator', () => {
  it('atomically reserves provider capacity and wakes when a lease releases', async () => {
    const usage = service();
    const coordinator = new QuotaCoordinator(usage, () => policy);
    const wake = vi.fn();
    coordinator.onWake(wake);
    const [first, second] = await Promise.all([coordinator.acquire({ candidates }), coordinator.acquire({ candidates })]);
    expect(first).toMatchObject({ kind: 'selected', provider: 'claude' });
    expect(second).toMatchObject({ kind: 'selected', provider: 'codex' });
    if (first.kind === 'selected') first.lease.release();
    expect(wake).toHaveBeenCalled();
    coordinator.dispose();
    usage.dispose();
  });

  it('returns a wait decision when both providers are at capacity', async () => {
    const usage = service();
    const coordinator = new QuotaCoordinator(usage, () => policy);
    const first = await coordinator.acquire({ candidates });
    const second = await coordinator.acquire({ candidates });
    const third = await coordinator.acquire({ candidates });
    expect(first).toMatchObject({ kind: 'selected', provider: 'claude' });
    expect(second).toMatchObject({ kind: 'selected', provider: 'codex' });
    expect(third).toMatchObject({ kind: 'wait' });
    if (first.kind === 'selected') first.lease.release();
    if (second.kind === 'selected') second.lease.release();
    coordinator.dispose();
    usage.dispose();
  });

  it('wakes queues on a meaningful usage update', async () => {
    const usage = service();
    const coordinator = new QuotaCoordinator(usage, () => policy);
    const wake = vi.fn();
    coordinator.onWake(wake);
    await usage.refresh(candidates.claude.account);
    expect(wake).toHaveBeenCalledTimes(1);
    coordinator.dispose();
    usage.dispose();
  });

  it('retains a soft stop until a fresh reading proves recovery below the hysteresis threshold', async () => {
    const claude = vi.fn()
      .mockResolvedValueOnce({ health: 'available', source: 'fake', windows: [{ kind: 'short', usedPercent: 95 }] })
      .mockResolvedValueOnce({ health: 'available', source: 'fake', windows: [{ kind: 'short', usedPercent: 85 }] })
      .mockResolvedValueOnce({ health: 'available', source: 'fake', windows: [{ kind: 'short', usedPercent: 79 }] });
    const codex: ProviderUsageAdapter = { provider: 'codex', read: vi.fn().mockResolvedValue({ health: 'available', source: 'fake', windows: [] }) };
    const usage = new ProviderUsageService({
      adapters: [{ provider: 'claude', read: claude }, codex], cacheTtlMs: 30_000,
    });
    const coordinator = new QuotaCoordinator(usage, () => policy);
    expect(await coordinator.acquire({ candidates })).toMatchObject({ kind: 'selected', provider: 'codex' });
    await usage.refresh(candidates.claude.account, true);
    expect(await coordinator.acquire({ candidates })).toMatchObject({ kind: 'wait' });
    await usage.refresh(candidates.claude.account, true);
    expect(await coordinator.acquire({ candidates })).toMatchObject({ kind: 'selected', provider: 'claude' });
    coordinator.dispose();
    usage.dispose();
  });

  it('keeps a runner-reported quota failure out of routing until changed telemetry proves recovery', async () => {
    const claude: ProviderUsageAdapter = {
      provider: 'claude',
      read: vi.fn()
        .mockResolvedValueOnce({ health: 'available', source: 'fake', windows: [{ kind: 'short', usedPercent: 50 }] })
        .mockResolvedValueOnce({ health: 'available', source: 'fake', windows: [{ kind: 'short', usedPercent: 5 }] }),
    };
    const codex: ProviderUsageAdapter = {
      provider: 'codex', read: vi.fn().mockResolvedValue({ health: 'available', source: 'fake', windows: [] }),
    };
    const usage = new ProviderUsageService({ adapters: [claude, codex], cacheTtlMs: 30_000 });
    const coordinator = new QuotaCoordinator(usage, () => policy);

    const first = await coordinator.acquire({ candidates });
    expect(first).toMatchObject({ kind: 'selected', provider: 'claude' });
    if (first.kind === 'selected') first.lease.release();
    coordinator.reportQuotaExhausted(candidates.claude.account);

    const fallback = await coordinator.acquire({ candidates });
    expect(fallback).toMatchObject({ kind: 'selected', provider: 'codex' });
    if (fallback.kind === 'selected') fallback.lease.release();

    await usage.refresh(candidates.claude.account, true);
    expect(await coordinator.acquire({ candidates })).toMatchObject({ kind: 'selected', provider: 'claude' });
    coordinator.dispose();
    usage.dispose();
  });
});
