import { mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { providerUsagePath } from '../paths.ts';
import { FileProviderUsageSnapshotStore } from './provider-usage.ts';

describe('provider usage snapshot store', () => {
  const original = process.env.CEZ_HOME;
  let home: string;

  beforeEach(() => {
    home = mkdtempSync(join(tmpdir(), 'cez-usage-'));
    process.env.CEZ_HOME = home;
  });
  afterEach(() => {
    if (original === undefined) delete process.env.CEZ_HOME;
    else process.env.CEZ_HOME = original;
    rmSync(home, { recursive: true, force: true });
  });

  it('writes only sanitized snapshots atomically with private permissions', async () => {
    const store = new FileProviderUsageSnapshotStore();
    await store.save([{
      provider: 'claude', profileId: 'default', health: 'available', fetchedAt: '2026-08-14T00:00:00.000Z',
      source: 'claude-oauth', stale: false, windows: [{ kind: 'short', usedPercent: 20 }],
      error: { code: 'provider_error', message: 'token secret-value must not persist' },
    }]);
    expect(statSync(providerUsagePath()).mode & 0o777).toBe(0o600);
    expect(readFileSync(providerUsagePath(), 'utf8')).not.toContain('secret-value');
    expect(await store.load()).toEqual([{
      provider: 'claude', profileId: 'default', health: 'available', fetchedAt: '2026-08-14T00:00:00.000Z',
      source: 'claude-oauth', stale: false, windows: [{ kind: 'short', usedPercent: 20 }],
    }]);
  });

  it('fails open on malformed cache data', async () => {
    const store = new FileProviderUsageSnapshotStore();
    writeFileSync(providerUsagePath(), '{bad json', 'utf8');
    // The cache is advisory; a corrupt file cannot block startup or routing.
    expect(await store.load()).toEqual([]);
  });
});
