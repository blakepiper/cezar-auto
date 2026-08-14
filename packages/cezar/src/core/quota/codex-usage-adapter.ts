import { z } from 'zod';
import type { ChildProcessWithoutNullStreams } from 'node:child_process';
import { readNdjson } from '../ndjson.ts';
import {
  CodexAppServerRpc,
  endCodexAppServer,
  resolveCodexExecutable,
  spawnCodexAppServer,
  type CodexAppServerMessage,
} from '../codex-app-server-transport.ts';
import { resolveProfileEnvForRoot } from '../../workspace/agent-profiles.ts';
import type { ProviderUsageAdapter, ProviderUsageReading } from './usage-service.ts';
import type { ProviderAccountRef } from './types.ts';

const rateLimitWindowSchema = z.object({
  used_percent: z.number().finite().nullable().optional(),
  usedPercent: z.number().finite().nullable().optional(),
  resets_at: z.number().finite().nullable().optional(),
  resetsAt: z.number().finite().nullable().optional(),
});

const rateLimitsSchema = z.object({
  primary: rateLimitWindowSchema.nullable().optional(),
  secondary: rateLimitWindowSchema.nullable().optional(),
  individual_limit: rateLimitWindowSchema.nullable().optional(),
  individualLimit: rateLimitWindowSchema.nullable().optional(),
  spend_control_reached: z.boolean().nullable().optional(),
  spendControlReached: z.boolean().nullable().optional(),
  rate_limit_reached_type: z.string().nullable().optional(),
  rateLimitReachedType: z.string().nullable().optional(),
});

// Test the wrapper first: a non-strict direct object would otherwise strip its
// `rateLimits` key and make the direct branch incorrectly win the union.
const codexResponseSchema = z.union([z.object({ rateLimits: rateLimitsSchema }), rateLimitsSchema]);

export type ReadCodexRateLimits = (account: ProviderAccountRef) => Promise<unknown>;

export interface InstalledCodexUsageAdapterOptions {
  /** Any accessible project root; account identity comes from CODEX_HOME. */
  cwd: string;
  bin?: string;
  timeoutMs?: number;
  /** Fixture seam; production uses the app-server request below. */
  readRateLimits?: ReadCodexRateLimits;
}

export interface CodexRateLimitReadOptions {
  cwd: string;
  bin?: string;
  env?: Record<string, string>;
  timeoutMs?: number;
  spawn?: (bin: string, cwd: string, env?: Record<string, string>) => ChildProcessWithoutNullStreams;
}

/**
 * One bounded authenticated app-server read. The long-lived usage service
 * caches the answer, so this process is never spawned per UI render.
 */
export async function readCodexRateLimitsFromAppServer(options: CodexRateLimitReadOptions): Promise<unknown> {
  const child = (options.spawn ?? spawnCodexAppServer)(resolveCodexExecutable(options.bin), options.cwd, options.env);
  const rpc = new CodexAppServerRpc(child);
  const reader = (async () => {
    try {
      for await (const line of readNdjson(child.stdout)) {
        const message = JSON.parse(line) as CodexAppServerMessage;
        rpc.dispatchResponse(message);
      }
    } catch (error) {
      rpc.rejectPending(error instanceof Error ? error.message : 'Codex usage reader failed');
    }
  })();
  let timeout: NodeJS.Timeout | undefined;
  const deadline = new Promise<never>((_, reject) => {
    timeout = setTimeout(() => {
      rpc.rejectPending('Codex usage read timed out');
      reject(new Error('Codex usage read timed out'));
    }, options.timeoutMs ?? 8_000);
    timeout.unref?.();
  });
  const exited = new Promise<never>((_, reject) => {
    child.once('error', () => reject(new Error('Codex usage reader failed')));
    child.once('exit', (code) => {
      const error = new Error(`Codex usage reader exited (${code ?? 'unknown'})`);
      rpc.rejectPending(error.message);
      reject(error);
    });
  });
  try {
    return await Promise.race([
      (async () => {
        await rpc.initialize();
        return rpc.request('account/rateLimits/read', {});
      })(),
      deadline,
      exited,
    ]);
  } finally {
    if (timeout) clearTimeout(timeout);
    endCodexAppServer(child);
    void reader.catch(() => undefined);
  }
}

function isoTime(value: number | null | undefined): string | undefined {
  if (value === null || value === undefined || !Number.isFinite(value)) return undefined;
  return new Date(value < 100_000_000_000 ? value * 1_000 : value).toISOString();
}

function normalizeWindow(kind: 'short' | 'long' | 'model', value: z.infer<typeof rateLimitWindowSchema>) {
  const usedPercent = value.used_percent ?? value.usedPercent ?? null;
  const resetsAt = isoTime(value.resets_at ?? value.resetsAt);
  return { kind, usedPercent, ...(resetsAt ? { resetsAt } : {}) } as const;
}

/** Normalizes the app-server's primary/secondary/model rate-limit windows. */
export function normalizeCodexRateLimits(raw: unknown): ProviderUsageReading {
  const parsed = codexResponseSchema.safeParse(raw);
  if (!parsed.success) {
    return { health: 'unknown', source: 'codex-app-server', windows: [], error: { code: 'invalid_response', message: 'Codex usage response was invalid.' } };
  }
  const limits = 'rateLimits' in parsed.data ? parsed.data.rateLimits : parsed.data;
  const hard = limits.spend_control_reached === true || limits.spendControlReached === true
    || Boolean(limits.rate_limit_reached_type ?? limits.rateLimitReachedType);
  const windows = [
    ...(limits.primary ? [normalizeWindow('short', limits.primary)] : []),
    ...(limits.secondary ? [normalizeWindow('long', limits.secondary)] : []),
    ...(limits.individual_limit ? [normalizeWindow('model', limits.individual_limit)] : []),
    ...(limits.individualLimit ? [normalizeWindow('model', limits.individualLimit)] : []),
  ].map((item) => hard ? { ...item, hardLimitReached: true } : item);
  return { health: hard ? 'hard_exhausted' : 'available', source: 'codex-app-server', windows };
}

/** App-server transport is injected so lifecycle ownership stays outside the adapter. */
export class CodexUsageAdapter implements ProviderUsageAdapter {
  readonly provider = 'codex' as const;

  constructor(private readonly readRateLimits: ReadCodexRateLimits) {}

  async read(account: ProviderAccountRef): Promise<ProviderUsageReading> {
    try {
      return normalizeCodexRateLimits(await this.readRateLimits(account));
    } catch {
      return { health: 'unknown', source: 'codex-app-server', windows: [], error: { code: 'request_failed', message: 'Codex usage could not be refreshed.' } };
    }
  }
}

/** Build the production Codex adapter with the selected profile's CODEX_HOME. */
export function createInstalledCodexUsageAdapter(
  options: InstalledCodexUsageAdapterOptions,
): CodexUsageAdapter {
  if (options.readRateLimits) return new CodexUsageAdapter(options.readRateLimits);
  return new CodexUsageAdapter(async (account) => {
    const { env } = await resolveProfileEnvForRoot(undefined, 'codex', account.profileId);
    return readCodexRateLimitsFromAppServer({
      cwd: options.cwd,
      bin: options.bin,
      timeoutMs: options.timeoutMs,
      env,
    });
  });
}
