import type { ProviderAccountRef } from './types.ts';

/** The only credential material the usage adapter may receive, held in memory only. */
export interface ClaudeOAuthCredential {
  accessToken: string;
  /** ISO timestamp when known; expired credentials are never sent. */
  expiresAt?: string;
}

/** Platform-specific file/Keychain access belongs behind this narrow seam. */
export type ReadClaudeOAuthCredential = (account: ProviderAccountRef) => Promise<ClaudeOAuthCredential | undefined>;

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

