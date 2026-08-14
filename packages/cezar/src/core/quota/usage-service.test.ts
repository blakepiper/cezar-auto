import { afterEach, describe, expect, it, vi } from 'vitest';
import { ProviderUsageService, type ProviderUsageAdapter, type ProviderUsageSnapshotStore } from './usage-service.ts';

const account = { provider: 'claude' as const, profileId: 'default' };

function adapter(read = vi.fn().mockResolvedValue({
  health: 'available' as const,
  source: 'fake',
  windows: [{ kind: 'short' as const, usedPercent: 20 }],
})): ProviderUsageAdapter {
  return { provider: 'claude', read };
}

afterEach(() => vi.useRealTimers());

describe('ProviderUsageService', () => {
  it('caches fresh values and deduplicates concurrent account refreshes', async () => {
    const read = vi.fn().mockResolvedValue({ health: 'available' as const, source: 'fake', windows: [] });
    const service = new ProviderUsageService({ adapters: [adapter(read)], cacheTtlMs: 30_000 });
    const [first, second] = await Promise.all([service.refresh(account), service.refresh(account)]);
    expect(read).toHaveBeenCalledTimes(1);
    expect(first).toEqual(second);
    await service.refresh(account);
    expect(read).toHaveBeenCalledTimes(1);
    service.dispose();
  });

  it('marks aged cache entries stale and refreshes them', async () => {
    let now = Date.parse('2026-08-14T00:00:00.000Z');
    const read = vi.fn().mockResolvedValue({ health: 'available' as const, source: 'fake', windows: [] });
    const service = new ProviderUsageService({ adapters: [adapter(read)], cacheTtlMs: 30_000, now: () => now });
    await service.refresh(account);
    now += 30_000;
    expect(service.get(account)?.stale).toBe(true);
    await service.refresh(account);
    expect(read).toHaveBeenCalledTimes(2);
    service.dispose();
  });

  it('publishes only meaningful changes and persists sanitized snapshots', async () => {
    const save = vi.fn().mockResolvedValue(undefined);
    const store: ProviderUsageSnapshotStore = { load: vi.fn().mockResolvedValue([]), save };
    const service = new ProviderUsageService({ adapters: [adapter()], cacheTtlMs: 1, store });
    const changed = vi.fn();
    service.onChange(changed);
    await service.refresh(account);
    await service.refresh(account, true);
    await vi.waitFor(() => expect(save).toHaveBeenCalledTimes(2));
    expect(changed).toHaveBeenCalledTimes(1);
    expect(save.mock.calls[0]?.[0][0]).toMatchObject({ provider: 'claude', profileId: 'default', stale: false });
    service.dispose();
  });

  it('restores persisted snapshots as stale without trusting them for a fresh decision', async () => {
    const store: ProviderUsageSnapshotStore = {
      load: vi.fn().mockResolvedValue([{
        ...account, health: 'available', source: 'cache', fetchedAt: '2026-08-14T00:00:00.000Z', stale: false, windows: [],
      }]),
      save: vi.fn().mockResolvedValue(undefined),
    };
    const service = new ProviderUsageService({ adapters: [adapter()], cacheTtlMs: 30_000, store });
    await service.restore();
    expect(service.get(account)).toMatchObject({ stale: true, source: 'cache' });
    service.dispose();
  });

  it('sanitizes restored cache entries before exposing them', async () => {
    const token = 'restored-secret-value';
    const store: ProviderUsageSnapshotStore = {
      load: vi.fn().mockResolvedValue([{
        ...account, health: 'available', source: token, fetchedAt: '2026-08-14T00:00:00.000Z', stale: false,
        windows: [], error: { code: 'provider_error', message: token },
      }]),
      save: vi.fn().mockResolvedValue(undefined),
    };
    const service = new ProviderUsageService({ adapters: [adapter()], cacheTtlMs: 30_000, store });
    await service.restore();
    const snapshot = service.get(account);
    expect(snapshot).toMatchObject({ source: 'claude', stale: true, error: { code: 'provider_error' } });
    expect(JSON.stringify(snapshot)).not.toContain(token);
    service.dispose();
  });

  it('refreshes at the earliest provider reset and stops doing so after disposal', async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date('2026-08-14T00:00:00.000Z'));
    const read = vi.fn().mockResolvedValue({
      health: 'soft_exhausted' as const,
      source: 'fake',
      windows: [{ kind: 'short' as const, usedPercent: 95, resetsAt: '2026-08-14T00:00:01.000Z' }],
    });
    const service = new ProviderUsageService({ adapters: [adapter(read)], cacheTtlMs: 30_000 });
    await service.refresh(account);
    await vi.advanceTimersByTimeAsync(1_000);
    expect(read).toHaveBeenCalledTimes(2);
    service.dispose();
    await vi.advanceTimersByTimeAsync(10_000);
    expect(read).toHaveBeenCalledTimes(2);
  });

  it('sanitizes thrown adapter failures', async () => {
    const service = new ProviderUsageService({
      adapters: [adapter(vi.fn().mockRejectedValue(new Error('token top-secret-value rejected')))],
      cacheTtlMs: 30_000,
    });
    const snapshot = await service.refresh(account);
    expect(snapshot).toMatchObject({ health: 'unknown', error: { code: 'refresh_failed' } });
    expect(JSON.stringify(snapshot)).not.toContain('top-secret-value');
    service.dispose();
  });

  it('sanitizes malformed adapter values before they reach the cache or API layer', async () => {
    const token = 'bearer-secret-value';
    const service = new ProviderUsageService({
      adapters: [adapter(vi.fn().mockResolvedValue({
        health: 'available',
        source: token,
        windows: [
          { kind: 'short', usedPercent: 101, resetsAt: token },
          { kind: 'long', usedPercent: 42, resetsAt: '2026-08-14T01:00:00.000Z', hardLimitReached: true },
        ],
        error: { code: token, message: token },
      }))],
      cacheTtlMs: 30_000,
    });

    const snapshot = await service.refresh(account);
    expect(snapshot).toEqual(expect.objectContaining({
      health: 'available', source: 'claude',
      windows: [{ kind: 'long', usedPercent: 42, resetsAt: '2026-08-14T01:00:00.000Z', hardLimitReached: true }],
      error: { code: 'provider_error', message: 'Provider usage could not be refreshed.' },
    }));
    expect(JSON.stringify(snapshot)).not.toContain(token);
    service.dispose();
  });

  it('refreshes cached accounts periodically and stops after disposal', async () => {
    vi.useFakeTimers();
    const read = vi.fn().mockResolvedValue({ health: 'available' as const, source: 'fake', windows: [] });
    const service = new ProviderUsageService({
      adapters: [adapter(read)], cacheTtlMs: 30_000, refreshIntervalMs: 1_000,
    });
    await service.refresh(account);
    await vi.advanceTimersByTimeAsync(1_000);
    expect(read).toHaveBeenCalledTimes(2);
    service.dispose();
    await vi.advanceTimersByTimeAsync(5_000);
    expect(read).toHaveBeenCalledTimes(2);
  });
});
