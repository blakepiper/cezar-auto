import { z } from 'zod';
import {
  createClaudeAccessTokenResolver,
  createInstalledClaudeCredentialReader,
  type ReadClaudeOAuthCredential,
} from './claude-credentials.ts';
import type { ProviderUsageAdapter, ProviderUsageReading } from './usage-service.ts';
import type { ProviderAccountRef } from './types.ts';

export const CLAUDE_USAGE_URL = 'https://api.anthropic.com/api/oauth/usage';

const usageWindowSchema = z.object({
  utilization: z.number().finite().nullable().optional(),
  resets_at: z.string().nullable().optional(),
});

const claudeUsageSchema = z.object({
  five_hour: usageWindowSchema.nullable().optional(),
  seven_day: usageWindowSchema.nullable().optional(),
}).refine((value) => value.five_hour !== undefined || value.seven_day !== undefined);

export type ResolveClaudeAccessToken = (account: ProviderAccountRef) => Promise<string | undefined>;

export interface ClaudeUsageAdapterOptions {
  resolveAccessToken: ResolveClaudeAccessToken;
  fetch?: typeof fetch;
  timeoutMs?: number;
}

/** Options for the installed-Claude factory. Test seams stop before a token is exposed. */
export interface InstalledClaudeUsageAdapterOptions extends Omit<ClaudeUsageAdapterOptions, 'resolveAccessToken'> {
  readCredential?: ReadClaudeOAuthCredential;
  now?: () => number;
}

function window(kind: 'short' | 'long', value: z.infer<typeof usageWindowSchema>) {
  return {
    kind,
    usedPercent: value.utilization ?? null,
    ...(value.resets_at ? { resetsAt: value.resets_at } : {}),
    ...(value.utilization !== undefined && value.utilization !== null && value.utilization >= 100
      ? { hardLimitReached: true }
      : {}),
  } as const;
}

/** Converts the documented OAuth usage response into the narrow Cezar representation. */
export function normalizeClaudeUsage(raw: unknown): ProviderUsageReading {
  const parsed = claudeUsageSchema.safeParse(raw);
  if (!parsed.success) {
    return { health: 'unknown', source: 'claude-oauth', windows: [], error: { code: 'invalid_response', message: 'Claude usage response was invalid.' } };
  }
  const windows = [
    ...(parsed.data.five_hour ? [window('short', parsed.data.five_hour)] : []),
    ...(parsed.data.seven_day ? [window('long', parsed.data.seven_day)] : []),
  ];
  if (windows.some((item) => item.hardLimitReached)) return { health: 'hard_exhausted', source: 'claude-oauth', windows };
  return { health: 'available', source: 'claude-oauth', windows };
}

/**
 * Reads only the selected account's access token, holds it in local scope, and
 * sends it to Anthropic's fixed HTTPS origin. The credential resolver owns
 * platform-specific file/Keychain access and must never return it to callers.
 */
export class ClaudeUsageAdapter implements ProviderUsageAdapter {
  readonly provider = 'claude' as const;
  readonly #fetch: typeof fetch;
  readonly #timeoutMs: number;

  constructor(private readonly options: ClaudeUsageAdapterOptions) {
    this.#fetch = options.fetch ?? fetch;
    this.#timeoutMs = options.timeoutMs ?? 8_000;
  }

  async read(account: ProviderAccountRef): Promise<ProviderUsageReading> {
    const token = await this.options.resolveAccessToken(account).catch(() => undefined);
    if (!token) {
      return { health: 'auth_error', source: 'claude-oauth', windows: [], error: { code: 'auth_error', message: 'Claude authentication is unavailable.' } };
    }
    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), this.#timeoutMs);
    try {
      const response = await this.#fetch(CLAUDE_USAGE_URL, {
        headers: { authorization: `Bearer ${token}`, 'anthropic-beta': 'oauth-2025-04-20' },
        signal: controller.signal,
      });
      if (response.status === 401 || response.status === 403) {
        return { health: 'auth_error', source: 'claude-oauth', windows: [], error: { code: 'auth_error', message: 'Claude authentication was rejected.' } };
      }
      if (response.status === 429) {
        return { health: 'unknown', source: 'claude-oauth', windows: [], error: { code: 'rate_limited', message: 'Claude usage is temporarily rate limited.' } };
      }
      if (!response.ok) {
        return { health: 'unknown', source: 'claude-oauth', windows: [], error: { code: 'request_failed', message: 'Claude usage could not be refreshed.' } };
      }
      return normalizeClaudeUsage(await response.json());
    } catch {
      return { health: 'unknown', source: 'claude-oauth', windows: [], error: { code: 'request_failed', message: 'Claude usage could not be refreshed.' } };
    } finally {
      clearTimeout(timeout);
    }
  }
}

/**
 * Production adapter for the locally authenticated Claude Code installation.
 * The credential object is consumed immediately by the token resolver and is
 * never visible to the usage service, coordinator, or API layer.
 */
export function createInstalledClaudeUsageAdapter(
  options: InstalledClaudeUsageAdapterOptions = {},
): ClaudeUsageAdapter {
  const { readCredential, now, ...adapterOptions } = options;
  return new ClaudeUsageAdapter({
    ...adapterOptions,
    resolveAccessToken: createClaudeAccessTokenResolver(
      readCredential ?? createInstalledClaudeCredentialReader(),
      now,
    ),
  });
}
