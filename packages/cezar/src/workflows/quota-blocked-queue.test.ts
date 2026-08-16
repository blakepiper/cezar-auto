import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { QuotaCoordinator } from '../core/quota/coordinator.ts';
import { RunStore } from '../runs/store.ts';
import { RunManager } from './run.ts';
import type { WorkflowDef } from './types.ts';

const workflow: WorkflowDef = {
  name: 'quota-queue',
  source: 'built-in',
  steps: [
    { id: 'work', name: 'Work', prompt: '{{task}}' },
    { id: 'verify', name: 'Verify', command: 'true' },
  ],
};

type WakeableCoordinator = {
  acquire: ReturnType<typeof vi.fn>;
  reportQuotaExhausted: ReturnType<typeof vi.fn>;
  onWake(listener: () => void): () => void;
  wake(): void;
};

function coordinator(...results: unknown[]): WakeableCoordinator {
  let listener: (() => void) | undefined;
  return {
    acquire: vi.fn().mockImplementation(async () => results.shift()),
    reportQuotaExhausted: vi.fn(),
    onWake(next) {
      listener = next;
      return () => { listener = undefined; };
    },
    wake: () => listener?.(),
  };
}

const exhausted = { kind: 'wait', considered: [], softExhausted: new Set() };
const selected = {
  kind: 'selected',
  provider: 'claude',
  decision: { kind: 'selected', provider: 'claude', considered: [], softExhausted: new Set() },
  lease: { provider: 'claude', profileId: 'default', release: vi.fn() },
};

async function settles(store: RunStore, runId: string): Promise<void> {
  await expect.poll(() => store.getRun(runId)?.status, { timeout: 10_000 })
    .toMatch(/^(done|review)$/);
}

describe('quota-blocked workflow queue', () => {
  const roots: string[] = [];
  const managers: RunManager[] = [];

  afterEach(() => {
    delete process.env.CEZ_DRY_RUN;
    delete process.env.CEZ_CODEX_BIN;
    for (const manager of managers.splice(0)) manager.dispose();
    for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true });
  });

  it('keeps both-exhausted Auto work queued, then runs it after a provider refresh wake', async () => {
    process.env.CEZ_DRY_RUN = '1';
    const root = mkdtempSync(join(tmpdir(), 'cez-quota-queue-'));
    roots.push(root);
    const quota = coordinator(exhausted, selected);
    const store = RunStore.open(join(root, '.ai/coducktor'));
    const manager = new RunManager(store, root, { quotaCoordinator: quota as unknown as QuotaCoordinator });
    managers.push(manager);

    const run = manager.startRun(workflow, { task: 'mock:done after quota reset', runner: 'auto', worktree: false });
    await expect.poll(() => store.getRun(run.id)?.blockedReason?.type, { timeout: 10_000 }).toBe('provider_quota');
    expect(store.getRun(run.id)).toMatchObject({ status: 'queued', requestedRunner: 'auto' });
    expect(store.getRun(run.id)?.steps[0]?.status).toBe('pending');

    quota.wake();
    await settles(store, run.id);
    expect(store.getRun(run.id)?.steps[0]).toMatchObject({ backend: 'claude', status: 'done' });
  }, 15_000);

  it('retries a durable blocked checkpoint after restart using its requested Auto selection', async () => {
    process.env.CEZ_DRY_RUN = '1';
    const root = mkdtempSync(join(tmpdir(), 'cez-quota-recover-'));
    roots.push(root);
    const store = RunStore.open(join(root, '.ai/coducktor'));
    const firstQuota = coordinator(exhausted);
    const first = new RunManager(store, root, { quotaCoordinator: firstQuota as unknown as QuotaCoordinator });
    managers.push(first);
    const run = first.startRun(workflow, { task: 'mock:done after restart', runner: 'auto', worktree: false });
    await expect.poll(() => store.getRun(run.id)?.blockedReason?.type, { timeout: 10_000 }).toBe('provider_quota');
    first.dispose();
    managers.splice(managers.indexOf(first), 1);

    const secondQuota = coordinator(selected);
    const second = new RunManager(store, root, { quotaCoordinator: secondQuota as unknown as QuotaCoordinator });
    managers.push(second);
    await second.recover();

    await settles(store, run.id);
    expect(secondQuota.acquire).toHaveBeenCalled();
    expect(store.getRun(run.id)?.steps[0]).toMatchObject({ backend: 'claude', status: 'done' });
  }, 15_000);

  it('fails over an auto step from a runtime Claude quota failure to Codex without a new iteration', async () => {
    process.env.CEZ_DRY_RUN = '1';
    process.env.CEZ_CODEX_BIN = join(import.meta.dirname, '../core/__fixtures__/codex/mock-codex-app-server.mjs');
    const root = mkdtempSync(join(tmpdir(), 'cez-quota-failover-'));
    roots.push(root);
    const quota = coordinator(
      { ...selected, provider: 'claude', lease: { provider: 'claude', profileId: 'default', release: vi.fn() } },
      { ...selected, provider: 'codex', decision: { kind: 'selected', provider: 'codex', considered: [], softExhausted: new Set() }, lease: { provider: 'codex', profileId: 'default', release: vi.fn() } },
    );
    const store = RunStore.open(join(root, '.ai/coducktor'));
    const manager = new RunManager(store, root, { quotaCoordinator: quota as unknown as QuotaCoordinator });
    managers.push(manager);

    const run = manager.startRun(workflow, { task: 'mock:limit then complete', runner: 'auto', worktree: false });
    await expect.poll(() => store.getRun(run.id)?.steps[0]?.backend, { timeout: 10_000 }).toBe('codex');

    expect(quota.acquire).toHaveBeenCalledTimes(2);
    expect(quota.acquire.mock.calls[1]?.[0]).toMatchObject({
      attemptedProviders: new Set(['claude']), forceRefresh: true,
    });
    expect(quota.reportQuotaExhausted).toHaveBeenCalledWith({ provider: 'claude', profileId: 'default' });
    expect(store.getRun(run.id)?.steps[0]).toMatchObject({ backend: 'codex', iterations: 1, status: 'running' });
    manager.cancel(run.id);
  }, 15_000);
});
