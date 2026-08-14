import { describe, expect, it, vi } from 'vitest';
import {
  createClaudeAccessTokenResolver,
  createInstalledClaudeCredentialReader,
} from './claude-credentials.ts';

const account = { provider: 'claude' as const, profileId: 'work' };

describe('createClaudeAccessTokenResolver', () => {
  it('returns only a non-expired access token', async () => {
    const resolver = createClaudeAccessTokenResolver(
      vi.fn().mockResolvedValue({ accessToken: 'access-token', expiresAt: '2026-08-15T00:00:00.000Z' }),
      () => Date.parse('2026-08-14T00:00:00.000Z'),
    );
    await expect(resolver(account)).resolves.toBe('access-token');
  });

  it('fails closed for expired, malformed, blank, and unreadable credentials', async () => {
    const now = () => Date.parse('2026-08-14T00:00:00.000Z');
    await expect(createClaudeAccessTokenResolver(vi.fn().mockResolvedValue({ accessToken: 'x', expiresAt: '2026-08-13T00:00:00.000Z' }), now)(account)).resolves.toBeUndefined();
    await expect(createClaudeAccessTokenResolver(vi.fn().mockResolvedValue({ accessToken: 'x', expiresAt: 'bad' }), now)(account)).resolves.toBeUndefined();
    await expect(createClaudeAccessTokenResolver(vi.fn().mockResolvedValue({ accessToken: '  ' }), now)(account)).resolves.toBeUndefined();
    await expect(createClaudeAccessTokenResolver(vi.fn().mockRejectedValue(new Error('keychain failed')), now)(account)).resolves.toBeUndefined();
  });
});

describe('createInstalledClaudeCredentialReader', () => {
  const credentialJson = JSON.stringify({
    claudeAiOauth: { accessToken: 'access-token', expiresAt: 1_785_168_000_000 },
  });

  it('reads only the OAuth fields from the selected non-macOS profile', async () => {
    const readFile = vi.fn().mockResolvedValue(credentialJson);
    const reader = createInstalledClaudeCredentialReader({
      platform: 'linux',
      readFile,
      resolveProfilePath: vi.fn().mockResolvedValue('/profiles/work'),
    });

    await expect(reader(account)).resolves.toEqual({
      accessToken: 'access-token',
      expiresAt: '2026-07-27T16:00:00.000Z',
    });
    expect(readFile).toHaveBeenCalledWith('/profiles/work/.credentials.json', 'utf8');
  });

  it('uses the macOS Keychain reader and fails closed for malformed secrets', async () => {
    const readKeychain = vi.fn().mockResolvedValue(credentialJson);
    const reader = createInstalledClaudeCredentialReader({
      platform: 'darwin',
      readKeychain,
      readFile: vi.fn(),
    });
    await expect(reader(account)).resolves.toMatchObject({ accessToken: 'access-token' });
    expect(readKeychain).toHaveBeenCalledTimes(1);

    const malformed = createInstalledClaudeCredentialReader({
      platform: 'darwin',
      readKeychain: vi.fn().mockResolvedValue('{not-json'),
    });
    await expect(malformed(account)).resolves.toBeUndefined();

    const denied = createInstalledClaudeCredentialReader({
      platform: 'darwin',
      readKeychain: vi.fn().mockRejectedValue(new Error('access denied')),
    });
    await expect(denied(account)).resolves.toBeUndefined();
  });
});
