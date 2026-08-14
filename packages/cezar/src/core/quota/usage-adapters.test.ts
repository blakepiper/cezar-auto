import { describe, expect, it, vi } from 'vitest';
import { CLAUDE_USAGE_URL, ClaudeUsageAdapter, normalizeClaudeUsage } from './claude-usage-adapter.ts';
import { CodexUsageAdapter, normalizeCodexRateLimits } from './codex-usage-adapter.ts';

const claude = { provider: 'claude' as const, profileId: 'work' };
const codex = { provider: 'codex' as const, profileId: 'work' };

describe('Claude usage adapter', () => {
  it('normalizes the five-hour and seven-day windows', () => {
    expect(normalizeClaudeUsage({
      five_hour: { utilization: 42, resets_at: '2026-08-14T12:00:00.000Z' },
      seven_day: { utilization: 80, resets_at: '2026-08-20T12:00:00.000Z' },
    })).toEqual({
      health: 'available', source: 'claude-oauth', windows: [
        { kind: 'short', usedPercent: 42, resetsAt: '2026-08-14T12:00:00.000Z' },
        { kind: 'long', usedPercent: 80, resetsAt: '2026-08-20T12:00:00.000Z' },
      ],
    });
  });

  it('uses only the fixed origin and never exposes the OAuth token in failures', async () => {
    const token = 'secret-access-token';
    const fetch = vi.fn().mockResolvedValue(new Response('nope', { status: 401 }));
    const adapter = new ClaudeUsageAdapter({ resolveAccessToken: vi.fn().mockResolvedValue(token), fetch });
    const result = await adapter.read(claude);
    expect(fetch).toHaveBeenCalledWith(CLAUDE_USAGE_URL, expect.objectContaining({ headers: expect.objectContaining({ authorization: `Bearer ${token}` }) }));
    expect(JSON.stringify(result)).not.toContain(token);
    expect(result).toMatchObject({ health: 'auth_error', error: { code: 'auth_error' } });
  });

  it('turns malformed and missing credentials into sanitized states', async () => {
    const missing = new ClaudeUsageAdapter({ resolveAccessToken: vi.fn().mockResolvedValue(undefined) });
    expect(await missing.read(claude)).toMatchObject({ health: 'auth_error' });
    const malformed = new ClaudeUsageAdapter({ resolveAccessToken: vi.fn().mockResolvedValue('x'), fetch: vi.fn().mockResolvedValue(new Response('{}')) });
    expect(await malformed.read(claude)).toMatchObject({ health: 'unknown', error: { code: 'invalid_response' } });
  });
});

describe('Codex usage adapter', () => {
  it('normalizes primary, secondary, and model windows from the app-server shape', () => {
    expect(normalizeCodexRateLimits({ rateLimits: {
      primary: { used_percent: 30, resets_at: 1_787_248_250 },
      secondary: { used_percent: 40, resets_at: 1_787_300_000 },
      individual_limit: { used_percent: 20, resets_at: 1_787_350_000 },
    } })).toMatchObject({
      health: 'available', source: 'codex-app-server', windows: [
        { kind: 'short', usedPercent: 30, resetsAt: '2026-08-20T17:50:50.000Z' },
        { kind: 'long', usedPercent: 40 },
        { kind: 'model', usedPercent: 20 },
      ],
    });
  });

  it('treats spend control as a hard exhaustion and sanitizes transport failure', async () => {
    expect(normalizeCodexRateLimits({ spend_control_reached: true })).toMatchObject({ health: 'hard_exhausted' });
    const adapter = new CodexUsageAdapter(vi.fn().mockRejectedValue(new Error('credential token secret')));
    const result = await adapter.read(codex);
    expect(result).toMatchObject({ health: 'unknown', error: { code: 'request_failed' } });
    expect(JSON.stringify(result)).not.toContain('credential token secret');
  });
});
