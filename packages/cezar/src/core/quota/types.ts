import type { AutoProvider } from '../runner-selection.ts';

export type ProviderQuotaHealth =
  | 'available'
  | 'soft_exhausted'
  | 'hard_exhausted'
  | 'auth_error'
  | 'unavailable'
  | 'unknown';

export interface ProviderAccountRef {
  provider: AutoProvider;
  profileId: string;
}

export interface ProviderUsageWindow {
  kind: 'short' | 'long' | 'model' | 'unknown';
  usedPercent: number | null;
  resetsAt?: string;
  hardLimitReached?: boolean;
}

/** Sanitized normalized usage state. Credentials and adapter payloads never enter this shape. */
export interface ProviderUsageSnapshot extends ProviderAccountRef {
  health: ProviderQuotaHealth;
  fetchedAt: string;
  source: string;
  stale: boolean;
  windows: readonly ProviderUsageWindow[];
  error?: { code: string; message: string };
}

