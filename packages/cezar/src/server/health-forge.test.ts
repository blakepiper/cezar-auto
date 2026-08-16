import { execFileSync } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { RunStore } from '../runs/store.ts';
import type { RunManager } from '../workflows/run.ts';
import { createApp, type ServerDeps } from './server.ts';
import { apiRequest } from './loopback-request.testkit.ts';

/**
 * Forge seam + capabilities (cockpit-ui redesign spec): `/api/v1/health` gains
 * `forge` ADDITIVELY (the pre-forge fields are the protected contract,
 * BACKWARD_COMPATIBILITY.md §2). `capabilities` is exactly `{ followups }`
 * after A15 (decisions 5/7/8 retired the hosted/single-repo/automations and
 * spend-hiding flags — local mode is the only mode, spec §16a).
 */

interface HealthBody {
  version: string;
  repoRoot: string;
  repo: unknown;
  checks: unknown[];
  defaultRunner?: string;
  forge: { kind: string; available: boolean; reason?: string } | null;
  capabilities: {
    followups: boolean;
  };
}

describe('GET /api/v1/health — forge + capabilities', () => {
  let repoRoot: string;
  let store: RunStore;
  const savedFollowups = process.env.CEZ_FOLLOWUPS;
  const savedDryRun = process.env.CEZ_DRY_RUN;

  beforeEach(() => {
    repoRoot = mkdtempSync(join(tmpdir(), 'cez-health-'));
    store = RunStore.open(join(repoRoot, '.ai/coducktor'));
    // #471: the inbox is opt-in, so an ambient CEZ_FOLLOWUPS on the dev box
    // must not decide what these assertions see.
    delete process.env.CEZ_FOLLOWUPS;
    // Dry-run keeps the forge probe (and the claude check) off the network,
    // so the assertions are deterministic on any machine.
    process.env.CEZ_DRY_RUN = '1';
  });

  afterEach(() => {
    store.flush();
    rmSync(repoRoot, { recursive: true, force: true });
    if (savedFollowups === undefined) delete process.env.CEZ_FOLLOWUPS;
    else process.env.CEZ_FOLLOWUPS = savedFollowups;
    if (savedDryRun === undefined) delete process.env.CEZ_DRY_RUN;
    else process.env.CEZ_DRY_RUN = savedDryRun;
  });

  const makeApp = (over: Partial<ServerDeps> = {}) =>
    createApp({ repoRoot, store, manager: {} as RunManager, version: '0.0.0-test', ...over });

  const health = async (over: Partial<ServerDeps> = {}): Promise<HealthBody> => {
    const res = await apiRequest(makeApp(over), '/api/v1/health');
    expect(res.status).toBe(200);
    return (await res.json()) as HealthBody;
  };

  it('local mode: keeps every pre-forge field and adds forge:null + followups:false outside a repo', async () => {
    const body = await health();
    // Protected pre-existing shape — additive only.
    expect(body.version).toBe('0.0.0-test');
    expect(body.repoRoot).toBe(repoRoot);
    expect(body).toHaveProperty('repo');
    expect(Array.isArray(body.checks)).toBe(true);
    expect(body).toHaveProperty('defaultRunner');
    // New additive fields.
    expect(body.forge).toBeNull(); // tmp dir — not a git repo, no remote
    expect(body.capabilities).toEqual({
      followups: false,
    });
  });

  // getRepoInfo needs a resolvable HEAD — an empty commit is enough.
  const initRepo = (remote: string, remoteName = 'origin') => {
    execFileSync('git', ['init', '-q'], { cwd: repoRoot });
    execFileSync(
      'git',
      ['-c', 'user.email=t@test', '-c', 'user.name=t', 'commit', '--allow-empty', '-q', '-m', 'init'],
      { cwd: repoRoot },
    );
    execFileSync('git', ['remote', 'add', remoteName, remote], { cwd: repoRoot });
  };

  it('reports the GitHub forge for a repo with a github.com remote', async () => {
    initRepo('https://github.com/acme/demo.git');
    const body = await health();
    expect(body.forge).toEqual({ kind: 'github', available: true });
  });

  it('reports the GitHub forge for an SSH (scp-like) origin remote', async () => {
    initRepo('git@github.com:acme/demo.git');
    const body = await health();
    expect(body.forge).toEqual({ kind: 'github', available: true });
  });

  it('reports the GitHub forge when the only remote is not named origin', async () => {
    initRepo('git@github.com:acme/demo.git', 'github');
    const body = await health();
    expect(body.forge).toEqual({ kind: 'github', available: true });
  });

  it('reports forge:null for a non-GitHub remote', async () => {
    initRepo('git@gitlab.com:acme/demo.git');
    const body = await health();
    expect(body.forge).toBeNull();
  });

  // #471 — the inbox capability rides the same payload the UI already reads.
  it('reports followups:false by default — the global inbox is opt-in', async () => {
    expect((await health()).capabilities.followups).toBe(false);
  });

  it('reports followups:true with CEZ_FOLLOWUPS=1', async () => {
    process.env.CEZ_FOLLOWUPS = '1';
    expect((await health()).capabilities).toEqual({
      followups: true,
    });
  });
});

describe('POST /api/v1/runs/:id/open-in-cli — local-mode session checks', () => {
  let repoRoot: string;
  let store: RunStore;
  let runId: string;
  const savedDryRun = process.env.CEZ_DRY_RUN;

  beforeEach(() => {
    repoRoot = mkdtempSync(join(tmpdir(), 'cez-handoff-'));
    store = RunStore.open(join(repoRoot, '.ai/coducktor'));
    runId = store.createRun({ title: 't', workflow: 'quick-task', task: 'do it', steps: [] }).id;
    process.env.CEZ_DRY_RUN = '1';
  });

  afterEach(() => {
    store.flush();
    rmSync(repoRoot, { recursive: true, force: true });
    if (savedDryRun === undefined) delete process.env.CEZ_DRY_RUN;
    else process.env.CEZ_DRY_RUN = savedDryRun;
  });

  const post = (over: Partial<ServerDeps> = {}) =>
    apiRequest(
      createApp({ repoRoot, store, manager: {} as RunManager, version: '0.0.0-test', ...over }),
      `/api/v1/runs/${runId}/open-in-cli`,
      { method: 'POST' },
    );

  it('local mode reaches the normal session checks (409 — no session to resume)', async () => {
    const res = await post();
    expect(res.status).toBe(409);
    // steps: [] — no session to resume.
    expect(((await res.json()) as { error: string }).error).toContain('no agent session');
  });

  it('unknown runs still 404 first', async () => {
    const app = createApp({ repoRoot, store, manager: {} as RunManager, version: '0.0.0-test' });
    const res = await apiRequest(app, '/api/v1/runs/nope/open-in-cli', { method: 'POST' });
    expect(res.status).toBe(404);
  });
});
