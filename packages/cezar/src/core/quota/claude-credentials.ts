import { execFile as execFileCallback } from 'node:child_process';
import { readFile as readFileCallback } from 'node:fs/promises';
import { promisify } from 'node:util';
import { z } from 'zod';
import {
  defaultAgentProfile,
  resolveStoredProfile,
} from '../../workspace/agent-profiles.ts';
import { loadAgentAccounts } from '../../workspace/agent-accounts.ts';
import type { ProviderAccountRef } from './types.ts';

/** The only credential material the usage adapter may receive, held in memory only. */
export interface ClaudeOAuthCredential {
  accessToken: string;
  /** ISO timestamp when known; expired credentials are never sent. */
  expiresAt?: string;
}

/** Platform-specific file/Keychain access belongs behind this narrow seam. */
export type ReadClaudeOAuthCredential = (account: ProviderAccountRef) => Promise<ClaudeOAuthCredential | undefined>;

const credentialFileSchema = z.object({
  claudeAiOauth: z.object({
    accessToken: z.string().min(1),
    // Claude Code writes this as epoch milliseconds. It is deliberately the
    // only metadata retained beside the in-memory token.
    expiresAt: z.number().finite().optional(),
  }),
});

export interface ClaudeCredentialReaderOptions {
  platform?: NodeJS.Platform;
  readFile?: (path: string, encoding: 'utf8') => Promise<string>;
  readKeychain?: () => Promise<string>;
  resolveProfilePath?: (profileId: string) => Promise<string>;
}

function parseCredential(raw: string): ClaudeOAuthCredential | undefined {
  try {
    const parsed = credentialFileSchema.safeParse(JSON.parse(raw));
    if (!parsed.success) return undefined;
    const { accessToken, expiresAt } = parsed.data.claudeAiOauth;
    return {
      accessToken,
      ...(expiresAt === undefined ? {} : { expiresAt: new Date(expiresAt).toISOString() }),
    };
  } catch {
    return undefined;
  }
}

/**
 * Read the installed Claude Code credential for one already-resolved account.
 * macOS keeps its JSON blob in Keychain; other platforms keep the same shape
 * in that profile's `.credentials.json`. No caller receives the raw blob.
 */
export function createInstalledClaudeCredentialReader(
  options: ClaudeCredentialReaderOptions = {},
): ReadClaudeOAuthCredential {
  const platform = options.platform ?? process.platform;
  const readFile = options.readFile ?? ((path: string, encoding: 'utf8') => readFileCallback(path, encoding));
  const readKeychain = options.readKeychain ?? (async () => {
    const { stdout } = await promisify(execFileCallback)(
      'security',
      ['find-generic-password', '-s', 'Claude Code-credentials', '-w'],
      { timeout: 5_000 },
    );
    return stdout;
  });
  const resolveProfilePath = options.resolveProfilePath ?? (async (profileId: string) => {
    const accounts = await loadAgentAccounts();
    const stored = accounts.accounts.find((candidate) =>
      candidate.id === profileId && candidate.provider === 'claude');
    return (stored ? resolveStoredProfile(stored) : defaultAgentProfile('claude')).path;
  });

  return async (account) => {
    if (account.provider !== 'claude') return undefined;
    if (platform === 'darwin') {
      try {
        return parseCredential(await readKeychain());
      } catch {
        return undefined;
      }
    }

    try {
      const profilePath = await resolveProfilePath(account.profileId);
      return parseCredential(await readFile(`${profilePath}/.credentials.json`, 'utf8'));
    } catch {
      return undefined;
    }
  };
}

/**
 * Creates the adapter-facing resolver without exposing credential metadata to
 * callers. Bad, blank, or expired values fail closed as an unavailable token.
 */
export function createClaudeAccessTokenResolver(
  readCredential: ReadClaudeOAuthCredential,
  now: () => number = Date.now,
): (account: ProviderAccountRef) => Promise<string | undefined> {
  return async (account) => {
    try {
      const credential = await readCredential(account);
      if (!credential?.accessToken.trim()) return undefined;
      if (credential.expiresAt !== undefined) {
        const expiresAt = Date.parse(credential.expiresAt);
        if (!Number.isFinite(expiresAt) || expiresAt <= now()) return undefined;
      }
      return credential.accessToken;
    } catch {
      return undefined;
    }
  };
}
