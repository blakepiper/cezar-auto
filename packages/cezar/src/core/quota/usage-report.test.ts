import { describe, expect, it, vi } from 'vitest';
import { formatUsageReport, readUsageReport } from './usage-report.ts';

const accounts = {
  claude: { provider: 'claude' as const, profileId: 'default' },
  codex: { provider: 'codex' as const, profileId: 'default' },
};

describe('usage report', () => {
  it('uses fresh reads on request and emits no credential material', async () => {
    const refresh = vi.fn(async ({ provider }: { provider: 'claude' | 'codex' }) => ({
      provider, profileId: 'default', health: 'available' as const, fetchedAt: '2026-08-14T00:00:00.000Z',
      source: 'fake', stale: false, windows: [{ kind: 'short' as const, usedPercent: 25 }],
    }));
    const report = await readUsageReport({ get: vi.fn(), refresh }, accounts, true);
    expect(refresh).toHaveBeenCalledTimes(2);
    expect(JSON.stringify(report)).not.toMatch(/token|secret|bearer/i);
    expect(formatUsageReport(report)).toContain('claude: available');
  });
});
