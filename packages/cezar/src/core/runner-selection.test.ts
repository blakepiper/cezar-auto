import { describe, expect, it } from 'vitest';
import { RUNNER_IDS, isRunnerId } from './agent-runner.ts';
import { AUTO_PROVIDER_IDS, isAutoProvider } from './runner-selection.ts';

describe('runner selection domain', () => {
  it('keeps auto as a policy rather than a constructible backend', () => {
    expect(RUNNER_IDS).not.toContain('auto');
    expect(isRunnerId('auto')).toBe(false);
    expect(AUTO_PROVIDER_IDS).toEqual(['claude', 'codex']);
    expect(isAutoProvider('claude')).toBe(true);
    expect(isAutoProvider('codex')).toBe(true);
    expect(isAutoProvider('opencode')).toBe(false);
  });
});
