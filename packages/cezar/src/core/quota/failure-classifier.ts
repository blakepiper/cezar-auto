import { parseUsageLimit } from '../usage-limit.ts';

/**
 * Conservative fallback for terminal runner errors. A bare HTTP 429 is not a
 * subscription limit — providers also use it for transient request throttles.
 */
const QUOTA_EXHAUSTED_RE = /\b(?:usage|subscription)\s+limit\s+(?:has\s+been\s+)?(?:reached|exceeded)|\b(?:quota|credits)\s+(?:has\s+been\s+)?(?:reached|exceeded|exhausted)\b|\bout\s+of\s+(?:quota|credits)\b/i;

export type RunnerFailureKind = 'quota_exhausted' | 'unknown';

export function classifyRunnerFailure(message: string | undefined): RunnerFailureKind {
  if (!message) return 'unknown';
  // A reset-bearing limit is the strongest evidence. The fallback deliberately
  // requires an explicit exhaustion phrase, never a status code alone.
  if (parseUsageLimit(message) || QUOTA_EXHAUSTED_RE.test(message)) return 'quota_exhausted';
  return 'unknown';
}
