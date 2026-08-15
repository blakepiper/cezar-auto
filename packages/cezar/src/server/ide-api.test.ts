import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import type { Hono } from 'hono';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import { RunStore } from '../runs/store.ts';
import type { RunManager } from '../workflows/run.ts';
import { apiRequest } from './loopback-request.testkit.ts';
import { createApp } from './server.ts';

describe('project IDE API', () => {
  const savedCezHome = process.env.CEZ_HOME;
  const savedRemote = process.env.CEZ_REMOTE;
  let home: string;
  let root: string;
  let store: RunStore;
  let app: Hono;

  beforeEach(() => {
    home = mkdtempSync(join(tmpdir(), 'cez-ide-api-home-'));
    root = mkdtempSync(join(tmpdir(), 'cez-ide-api-root-'));
    process.env.CEZ_HOME = home;
    delete process.env.CEZ_REMOTE;
    mkdirSync(join(root, '.ai/cezar'), { recursive: true });
    writeFileSync(join(root, 'README.md'), '# cezar\n', 'utf8');
    mkdirSync(join(root, 'src'), { recursive: true });
    writeFileSync(join(root, 'src/index.ts'), 'export const answer = 42\n', 'utf8');
    store = RunStore.open(join(root, '.ai/cezar'));
    app = createApp({
      repoRoot: root,
      store,
      manager: {} as RunManager,
      version: '0.0.0-test',
    });
  });

  afterEach(() => {
    store.flush();
    rmSync(home, { recursive: true, force: true });
    rmSync(root, { recursive: true, force: true });
    if (savedCezHome === undefined) delete process.env.CEZ_HOME;
    else process.env.CEZ_HOME = savedCezHome;
    if (savedRemote === undefined) delete process.env.CEZ_REMOTE;
    else process.env.CEZ_REMOTE = savedRemote;
  });

  it('lists, reads, and saves through both project aliases', async () => {
    const tree = await apiRequest(app, '/api/v1/ide/tree');
    expect(tree.status).toBe(200);
    const treeBody = await tree.json() as { path: string; entries: unknown[] };
    expect(treeBody.path).toBe('');
    expect(treeBody.entries).toEqual(expect.arrayContaining([
      expect.objectContaining({ name: 'README.md', path: 'README.md', type: 'file' }),
      expect.objectContaining({ name: 'src', path: 'src', type: 'dir' }),
    ]));

    const read = await apiRequest(app, '/api/v1/p/default/ide/file?path=src%2Findex.ts');
    expect(read.status).toBe(200);
    expect(await read.json()).toMatchObject({ path: 'src/index.ts', content: 'export const answer = 42\n' });

    const save = await apiRequest(app, '/api/v1/p/default/ide/file', {
      method: 'PUT',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ path: 'src/index.ts', content: 'export const answer = 43\n' }),
    });
    expect(save.status).toBe(200);
    expect(await save.json()).toMatchObject({ content: 'export const answer = 43\n' });

    const unscoped = await apiRequest(app, '/api/v1/ide/file?path=src%2Findex.ts');
    expect(await unscoped.json()).toMatchObject({ content: 'export const answer = 43\n' });
  });

  it('rejects paths outside the project and malformed file bodies', async () => {
    const traversal = await apiRequest(app, '/api/v1/ide/file?path=..%2Fsecret.txt');
    expect(traversal.status).toBe(400);

    const malformed = await apiRequest(app, '/api/v1/ide/file', {
      method: 'PUT',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ path: 'src/index.ts' }),
    });
    expect(malformed.status).toBe(400);
  });
});
