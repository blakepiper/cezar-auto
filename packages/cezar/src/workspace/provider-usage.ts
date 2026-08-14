import { readFile } from 'node:fs/promises';
import { z } from 'zod';
import { providerUsagePath } from '../paths.ts';
import { atomicWriteJsonSync } from './config.ts';
import type { ProviderUsageSnapshot } from '../core/quota/types.ts';
import type { ProviderUsageSnapshotStore } from '../core/quota/usage-service.ts';

const snapshotSchema = z.object({
  provider: z.enum(['claude', 'codex']),
  profileId: z.string().min(1).max(200),
  health: z.enum(['available', 'soft_exhausted', 'hard_exhausted', 'auth_error', 'unavailable', 'unknown']),
  fetchedAt: z.string().datetime(),
  source: z.string().max(100),
  stale: z.boolean(),
  windows: z.array(z.object({
    kind: z.enum(['short', 'long', 'model', 'unknown']),
    usedPercent: z.number().min(0).max(100).nullable(),
    resetsAt: z.string().datetime().optional(),
    hardLimitReached: z.boolean().optional(),
  })).max(8),
});

const snapshotFileSchema = z.object({ snapshots: z.array(snapshotSchema).max(32) });

/**
 * Small durable cache for provider usage. It deliberately drops adapter error
 * details: they are transient, potentially provider-controlled, and are not
 * necessary to restore an advisory stale snapshot after restart.
 */
export class FileProviderUsageSnapshotStore implements ProviderUsageSnapshotStore {
  constructor(private readonly path = providerUsagePath()) {}

  async load(): Promise<readonly ProviderUsageSnapshot[]> {
    try {
      const parsed = snapshotFileSchema.safeParse(JSON.parse(await readFile(this.path, 'utf8')));
      return parsed.success ? parsed.data.snapshots : [];
    } catch {
      return [];
    }
  }

  async save(snapshots: readonly ProviderUsageSnapshot[]): Promise<void> {
    const sanitized = snapshots.map(({ error: _error, ...snapshot }) => snapshot);
    const parsed = snapshotFileSchema.safeParse({ snapshots: sanitized });
    if (!parsed.success) return;
    atomicWriteJsonSync(this.path, parsed.data);
  }
}
