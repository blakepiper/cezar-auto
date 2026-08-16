import { existsSync, renameSync } from 'node:fs';
import { readFile } from 'node:fs/promises';
import { homedir } from 'node:os';
import { join } from 'node:path';
import { workspaceUiStateSchema } from '@open-mercato/cezar-contract';
import { cezarHomeDir, workspaceConfigPath } from '../paths.ts';
import { readUiState } from '../ui-state.ts';
import { loadWorkspaceConfig, mergeWriteWorkspaceConfig } from './config.ts';
import { mergeWriteWorkspaceUiState } from './ui-state.ts';

// Step 1.4 exported the global ui-state merge-write from here; step 2.7 moved
// it to its cleaner home (src/workspace/ui-state.ts, next to the read path the
// workspace routes share). Re-exported so existing importers keep working.
export { mergeWriteWorkspaceUiState } from './ui-state.ts';

/**
 * Workspace config migrations (spec 2026-07-20-multi-project-workspace,
 * "Migrations"). Deliberately tiny and **config-files-only** — run state
 * (`runs.json`, NDJSON) keeps the existing additive-zod convention and never
 * migrates. Rules, verbatim from the spec:
 *
 * - **idempotent** — every migration is safe to re-run after a crash mid-way;
 * - **additive** — never deletes or rewrites the user's per-repo files;
 * - **non-blocking** — a failing migration logs ONE warning and boot proceeds
 *   degraded with in-memory defaults; it is never a boot failure (the
 *   zero-config law "a read-only home degrades to a smaller cockpit" holds);
 * - **concurrency-safe** — every write takes the same read-modify-write +
 *   atomic-rename path as all workspace writes (`mergeWriteWorkspaceConfig`),
 *   and two processes racing the same idempotent step converge.
 */

export interface WorkspaceMigration {
  /** `schemaVersion` this migration produces. */
  to: number;
  /** Stable id for the warning line, e.g. `'001-workspace-config'`. */
  id: string;
  /** The migration body — MUST be idempotent (see module docs). */
  run(ctx: { home: string; bootRepoRoot: string | null }): Promise<void>;
}

/** Tolerant raw-JSON read: missing/unreadable/malformed/non-object → null. */
async function readRawObject(path: string): Promise<Record<string, unknown> | null> {
  try {
    return asObject(JSON.parse(await readFile(path, 'utf8')));
  } catch {
    return null;
  }
}

function asObject(value: unknown): Record<string, unknown> | null {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null;
}

/**
 * The boot repo's `.ai/coducktor/config.json` resource keys, read RAW (not through
 * `loadConfig`) so only values the user explicitly set are imported — a
 * defaulted value must not masquerade as a preference. Bounds mirror the
 * workspace `resources` schema; out-of-range values are simply not imported.
 */
async function readRepoResourceKeys(
  repoRoot: string,
): Promise<{ maxParallel?: number; memoryLimitMb?: number }> {
  const raw = await readRawObject(join(repoRoot, '.ai/coducktor', 'config.json'));
  const out: { maxParallel?: number; memoryLimitMb?: number } = {};
  const maxParallel = raw?.maxParallel;
  if (typeof maxParallel === 'number' && Number.isInteger(maxParallel) && maxParallel >= 1 && maxParallel <= 16) {
    out.maxParallel = maxParallel;
  }
  const memoryLimitMb = raw?.memoryLimitMb;
  if (
    typeof memoryLimitMb === 'number' &&
    Number.isInteger(memoryLimitMb) &&
    memoryLimitMb >= 0 &&
    memoryLimitMb <= 1_048_576
  ) {
    out.memoryLimitMb = memoryLimitMb;
  }
  return out;
}

/**
 * Migration 001 — `schemaVersion 0 → 1`, the "current version up" migration:
 *
 * 1. Create `~/.coducktor/config.json` with defaults if absent (the merge-write
 *    does this even when there is nothing to import).
 * 2. Booting inside a repo: import its `maxParallel`/`memoryLimitMb` into
 *    workspace `resources`, and its `appearance`/`notifications` ui-state
 *    keys into `~/.coducktor/ui-state.json`. Keys already set globally are NEVER
 *    overwritten — presence is checked against the RAW global file (before
 *    defaults are applied), which is exactly what makes a crash-interrupted
 *    re-run safe: the first pass writes the keys, the re-run sees them set.
 * 3. Every per-repo file is left untouched in place, so an older cezar run in
 *    the same repo keeps working off its local copies.
 *
 * Registering the boot repo is NOT part of the migration — the normal boot
 * flow owns registration (single owner, no divergent `addedAt`/`source`).
 */
const migration001: WorkspaceMigration = {
  to: 1,
  id: '001-workspace-config',
  async run({ bootRepoRoot }) {
    // Raw presence check BEFORE the first write: after any merge-write the
    // global file contains every defaulted key, so "already set globally"
    // must be decided against what the user's file held coming in.
    const rawGlobal = await readRawObject(workspaceConfigPath());
    const rawResources = asObject(rawGlobal?.resources);
    const imported = bootRepoRoot ? await readRepoResourceKeys(bootRepoRoot) : {};
    await mergeWriteWorkspaceConfig((config) => {
      if (imported.maxParallel !== undefined && rawResources?.maxParallel === undefined) {
        config.resources.maxParallel = imported.maxParallel;
      }
      if (imported.memoryLimitMb !== undefined && rawResources?.memoryLimitMb === undefined) {
        config.resources.memoryLimitMb = imported.memoryLimitMb;
      }
    });
    if (!bootRepoRoot) return;
    const repoUiState = await readUiState(bootRepoRoot);
    // The two prefs that went GLOBAL at step 3.5, read out of the per-repo bag and validated with
    // the DESTINATION's own field schemas. `notifications` is not named by the per-repo contract
    // at all (it left at 3.5), so it arrives through the open catchall as `unknown` — parsing it
    // here is what makes it assignable, and it is also the honest thing: `GET /workspace/ui-state`
    // promises these two keys' shapes, so a hand-edited value must not be promoted into the global
    // file unchecked. A value that does not parse is simply not imported — never a failed boot.
    const shape = workspaceUiStateSchema.shape;
    const importable: Record<string, unknown> = {};
    const appearance = shape.appearance.safeParse(repoUiState.appearance);
    if (appearance.success && appearance.data !== undefined) importable.appearance = appearance.data;
    const notifications = shape.notifications.safeParse(repoUiState.notifications);
    if (notifications.success && notifications.data !== undefined) {
      importable.notifications = notifications.data;
    }
    if (Object.keys(importable).length === 0) return; // nothing to import — don't create the file
    await mergeWriteWorkspaceUiState((state) => {
      for (const [key, value] of Object.entries(importable)) {
        if (state[key] === undefined) state[key] = value;
      }
    });
  },
};

/**
 * One on-disk state-dir rename. Idempotent and never destructive:
 *  - old absent → nothing to do (already migrated, or never existed);
 *  - both present → the NEW dir wins; warn about the stray old one but never
 *    delete it (deleting user data is not a migration's job);
 *  - old present, new absent → rename and log one line.
 * Failure degrades to a warning, never a failed boot (the migration framework's
 * non-blocking contract).
 */
async function migrateStateDir(oldPath: string, newPath: string, label: string): Promise<void> {
  const oldExists = existsSync(oldPath);
  const newExists = existsSync(newPath);
  if (!oldExists) return;
  if (newExists) {
    console.warn(
      `[cez] found both ${oldPath} and ${newPath} — using ${newPath}; ` +
        `remove ${oldPath} once you no longer need it`,
    );
    return;
  }
  try {
    renameSync(oldPath, newPath);
    console.log(`[cez] moved ${label} state from ${oldPath} to ${newPath}`);
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    console.warn(`[cez] could not move ${label} state from ${oldPath} to ${newPath} (${message})`);
  }
}

/**
 * Migration 002 — `schemaVersion 1 → 2`, the coducktor rename (decision 6,
 * spec §2.2.2 point 3): move the on-disk state dirs from their cezar-era names
 * to the coducktor ones.
 *
 * This must ALSO run before migration 001's config read/write on a pre-rename
 * install — the ordered loop alone would let 001 create a fresh
 * `.ai/coducktor` config and strand the user's real `.cezar` one — so
 * `runMigrations` calls `migrateStateDirs` at its very top. Registered here as
 * a normal migration too, so the framework's record bumps `schemaVersion` to 2
 * and re-running it is the same idempotent no-op (after the first rename both
 * dirs are settled).
 *
 * Run state never migrates in place — the whole-directory rename carries
 * runs.json, NDJSON logs, handoffs and worktrees wholesale, which is exactly
 * why the A15 accept ("a run started before the migration still loads") holds:
 * the data is moved, not rebuilt.
 */
const migration002: WorkspaceMigration = {
  to: 2,
  id: '002-coducktor-state-dirs',
  async run({ home, bootRepoRoot }) {
    await migrateStateDirs(home, bootRepoRoot);
  },
};

/** All known migrations. Kept in ascending `to` order; `runMigrations` sorts
 *  defensively anyway. Declared after both migrations so the references are
 *  live. */
export const WORKSPACE_MIGRATIONS: readonly WorkspaceMigration[] = [migration001, migration002];

/** The two state-dir renames migration 002 performs. Exported so `runMigrations`
 *  can fire it first AND so the migration test can drive it directly. */
export async function migrateStateDirs(home: string, bootRepoRoot: string | null): Promise<void> {
  // Per-user home: only meaningful when CEZ_HOME is NOT overriding the base —
  // an explicit CEZ_HOME *is* the location (tests/containers), so there is no
  // old spelling to move.
  if (!process.env.CEZ_HOME) {
    await migrateStateDir(join(homedir(), '.cezar'), join(homedir(), '.coducktor'), 'home');
  }
  if (bootRepoRoot) {
    await migrateStateDir(join(bootRepoRoot, '.ai', 'cezar'), join(bootRepoRoot, '.ai', 'coducktor'), 'repo');
  }
}

/**
 * Run every pending workspace migration — called at boot before anything else
 * touches `~/.coducktor`. Reads `schemaVersion` (absent file or key → 0, which
 * means "run everything" — safe because every migration is idempotent), runs
 * each migration with `to > current` in ascending order, and persists the new
 * `schemaVersion` after EACH one, so a crash resumes exactly where it left
 * off. A failing migration logs ONE warning and stops the chain (later
 * migrations may depend on earlier ones); boot proceeds degraded on in-memory
 * defaults. Never throws.
 *
 * `migrations` is injectable for tests only; production callers pass nothing.
 */
export async function runMigrations(
  opts: { bootRepoRoot: string | null },
  migrations: readonly WorkspaceMigration[] = WORKSPACE_MIGRATIONS,
): Promise<void> {
  const home = cezarHomeDir();
  // State-dir rename FIRST: on a pre-rename install, migration 001's config
  // write must land in the migrated home rather than create a fresh
  // `.coducktor` alongside the user's real `.cezar` config (which would strand
  // it — see migration 002's doc).
  await migrateStateDirs(home, opts.bootRepoRoot);
  const ordered = [...migrations].sort((a, b) => a.to - b.to);
  let current = (await loadWorkspaceConfig()).schemaVersion;
  for (const migration of ordered) {
    if (migration.to <= current) continue;
    try {
      await migration.run({ home, bootRepoRoot: opts.bootRepoRoot });
      const written = await mergeWriteWorkspaceConfig((config) => {
        config.schemaVersion = Math.max(config.schemaVersion, migration.to);
      });
      current = written.schemaVersion;
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      console.warn(
        `[cez] workspace migration ${migration.id} failed (${message}) — booting with in-memory defaults`,
      );
      return;
    }
  }
}
