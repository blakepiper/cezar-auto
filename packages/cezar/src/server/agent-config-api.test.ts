import { mkdirSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import type { Hono } from 'hono';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { RunStore } from '../runs/store.ts';
import type { RunManager } from '../workflows/run.ts';
import { apiRequest } from './loopback-request.testkit.ts';
import { createApp } from './server.ts';

/**
 * `GET/PUT /api/v1/agent-config` (spec #404). The contract under test: files are
 * addressed by catalog id (unknown → 404) and the load-bearing security property —
 * a repo-LOCAL file whose hooks would otherwise be a remote code-execution
 * primitive — is editable like any other. Writes are a local-machine capability
 * and local is the only mode (A15, decision 5 retires hosted mode).
 */
describe('the agent-config API', () => {
  let repoRoot: string;
  let store: RunStore;
  let app: Hono;

  beforeEach(() => {
    repoRoot = mkdtempSync(join(tmpdir(), 'cez-agentcfg-'));
    mkdirSync(join(repoRoot, '.ai/coducktor'), { recursive: true });
    store = RunStore.open(join(repoRoot, '.ai/coducktor'));
    app = createApp({
      repoRoot,
      store,
      manager: {} as RunManager,
      version: '0.0.0-test',
    });
  });
  afterEach(() => {
    store.flush();
    rmSync(repoRoot, { recursive: true, force: true });
  });

  const put = (id: string, body: unknown) =>
    apiRequest(app, `/api/v1/agent-config/${id}`, {
      method: 'PUT',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(body),
    });

  it('GET lists the catalog with editable:true locally', async () => {
    const res = await apiRequest(app, '/api/v1/agent-config');
    expect(res.status).toBe(200);
    const body = (await res.json()) as {
      editable: boolean;
      files: unknown[];
      userMcp: unknown;
    };
    expect(body.editable).toBe(true);
    expect(body.files.length).toBeGreaterThan(10);
    expect(body.userMcp).not.toBeNull();
  });

  it('GET :id → 404 for an unknown id', async () => {
    expect((await apiRequest(app, '/api/v1/agent-config/nope')).status).toBe(404);
  });

  it('GET :id reads an absent file as exists:false', async () => {
    const res = await apiRequest(app, '/api/v1/agent-config/claude.project.settings');
    expect(res.status).toBe(200);
    expect(await res.json()).toMatchObject({ exists: false, version: null });
  });

  it('PUT creates a file, then a correct-version PUT updates it', async () => {
    const created = await put('claude.project.settings', {
      content: '{"a":1}',
      version: null,
    });
    expect(created.status).toBe(200);
    const { version } = (await created.json()) as { version: string };
    expect(readFileSync(join(repoRoot, '.claude', 'settings.json'), 'utf8')).toBe('{"a":1}');
    const updated = await put('claude.project.settings', {
      content: '{"a":2}',
      version,
    });
    expect(updated.status).toBe(200);
  });

  it('PUT rejects invalid JSON with 400', async () => {
    expect((await put('claude.project.settings', { content: '{bad', version: null })).status).toBe(400);
  });

  it('PUT rejects a stale version with 409', async () => {
    await put('claude.project.settings', { content: '{"a":1}', version: null });
    expect(
      (
        await put('claude.project.settings', {
          content: '{"a":2}',
          version: null,
        })
      ).status,
    ).toBe(409);
  });

  it('PUT :id → 404 for an unknown id', async () => {
    expect((await put('nope', { content: 'x', version: null })).status).toBe(404);
  });
});
