import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { QuotaCoordinator } from '../core/quota/coordinator.ts';
import { RunStore } from '../runs/store.ts';
import { RunManager } from './run.ts';

type ResolutionHarness = {
  resolveRunnerSelection(selection: 'auto'): Promise<unknown>;
};

describe('RunManager auto runner resolution', () => {
  const roots: string[] = [];

  afterEach(() => {
    delete process.env.CEZ_DRY_RUN;
    for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true });
  });

  it('turns auto into a concrete backend and retains the coordinator lease', async () => {
    process.env.CEZ_DRY_RUN = '1';
    const release = vi.fn();
    const acquire = vi.fn().mockResolvedValue({
      kind: 'selected',
      provider: 'claude',
      lease: { provider: 'claude', profileId: 'default', release },
    });
    const root = mkdtempSync(join(tmpdir(), 'cez-auto-route-'));
    roots.push(root);
    const manager = new RunManager(RunStore.open(join(root, '.ai/coducktor')), root, {
      quotaCoordinator: { acquire } as unknown as QuotaCoordinator,
    });

    await expect((manager as unknown as ResolutionHarness).resolveRunnerSelection('auto'))
      .resolves.toMatchObject({ backend: 'claude', profileId: 'default', release });
    expect(acquire).toHaveBeenCalledWith(expect.objectContaining({
      candidates: expect.objectContaining({ claude: expect.objectContaining({ available: true }) }),
    }));
  });

  it('keeps a quota wait distinct from an ordinary runner-resolution failure', async () => {
    process.env.CEZ_DRY_RUN = '1';
    const acquire = vi.fn().mockResolvedValue({
      kind: 'wait',
      retryAt: '2026-08-14T18:00:00.000Z',
      considered: [],
      softExhausted: new Set(),
    });
    const root = mkdtempSync(join(tmpdir(), 'cez-auto-route-'));
    roots.push(root);
    const manager = new RunManager(RunStore.open(join(root, '.ai/coducktor')), root, {
      quotaCoordinator: { acquire } as unknown as QuotaCoordinator,
    });

    await expect((manager as unknown as ResolutionHarness).resolveRunnerSelection('auto'))
      .resolves.toMatchObject({ kind: 'quota-blocked', retryAt: '2026-08-14T18:00:00.000Z' });
  });
});
