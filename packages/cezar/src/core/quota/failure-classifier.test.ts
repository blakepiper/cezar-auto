import { describe, expect, it } from 'vitest';
import { classifyRunnerFailure } from './failure-classifier.ts';

describe('classifyRunnerFailure', () => {
  it.each([
    'Claude AI usage limit reached|1786723200',
    'Your subscription usage limit has been reached.',
    'quota exceeded — try another provider',
  ])('recognizes confirmed quota exhaustion: %s', (message) => {
    expect(classifyRunnerFailure(message)).toBe('quota_exhausted');
  });

  it('does not confuse a bare rate-limit response with subscription exhaustion', () => {
    expect(classifyRunnerFailure('429 rate_limit_error')).toBe('unknown');
  });
});
