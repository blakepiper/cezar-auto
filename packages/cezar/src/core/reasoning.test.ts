import { describe, expect, it } from 'vitest';
import { resolveReasoningEffort } from './reasoning.ts';

describe('resolveReasoningEffort', () => {
  it('honors an explicit level', () => {
    expect(resolveReasoningEffort('low', { task: 'debug auth', prompt: 'anything' })).toBe('low');
  });

  it('uses light reasoning for short verification work', () => {
    expect(resolveReasoningEffort('auto', { task: 'run the tests', prompt: 'verify the change' })).toBe('low');
  });

  it('raises the level for a complex debugging chunk', () => {
    expect(resolveReasoningEffort('auto', {
      task: 'Investigate the root cause of the authentication race condition and fix it',
      prompt: 'Debug the production outage and preserve concurrency guarantees.',
    })).toBe('xhigh');
  });

  it('uses a balanced default for ordinary implementation work', () => {
    expect(resolveReasoningEffort(undefined, { task: 'Add a button', prompt: 'Implement the UI change.' })).toBe('medium');
  });
});
