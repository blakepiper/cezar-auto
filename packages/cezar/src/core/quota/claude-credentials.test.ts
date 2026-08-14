import { describe, expect, it, vi } from 'vitest';
import { createClaudeAccessTokenResolver } from './claude-credentials.ts';

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

