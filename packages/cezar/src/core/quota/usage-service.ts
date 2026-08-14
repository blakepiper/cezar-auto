import type { AutoProvider } from '../runner-selection.ts';
import type { ProviderAccountRef, ProviderQuotaHealth, ProviderUsageSnapshot, ProviderUsageWindow } from './types.ts';

export interface ProviderUsageReading {
  health: ProviderQuotaHealth;
  source: string;
  windows: readonly ProviderUsageWindow[];
  error?: { code: string; message: string };
}

/** Adapter boundary: implementations must return only normalized, sanitized data. */
export interface ProviderUsageAdapter {
  provider: AutoProvider;
  read(account: ProviderAccountRef): Promise<ProviderUsageReading>;
}

/** Persistence is deliberately injected. The service never knows credential locations or raw payloads. */
export interface ProviderUsageSnapshotStore {
  load(): Promise<readonly ProviderUsageSnapshot[]>;
  save(snapshots: readonly ProviderUsageSnapshot[]): Promise<void>;
}

export interface ProviderUsageServiceOptions {
  adapters: readonly ProviderUsageAdapter[];
  cacheTtlMs: number;
  /** Refresh cached accounts in the background without tying refreshes to UI reads. */
  refreshIntervalMs?: number;
  store?: ProviderUsageSnapshotStore;
  now?: () => number;
}

type UsageListener = (snapshot: ProviderUsageSnapshot) => void;

function cacheKey(account: ProviderAccountRef): string {
  return `${account.provider}:${account.profileId}`;
}

const HEALTHS = new Set<ProviderQuotaHealth>([
  'available', 'soft_exhausted', 'hard_exhausted', 'auth_error', 'unavailable', 'unknown',
]);
const WINDOW_KINDS = new Set<ProviderUsageWindow['kind']>(['short', 'long', 'model', 'unknown']);
const SAFE_SOURCES = new Set(['claude-oauth', 'codex-app-server', 'cache', 'runtime', 'none', 'fake']);
const SAFE_ERROR_CODES = new Set([
  'auth_error', 'request_failed', 'invalid_response', 'adapter_unavailable', 'refresh_failed', 'provider_error',
]);
const GENERIC_ERROR_MESSAGE = 'Provider usage could not be refreshed.';
const ERROR_MESSAGES: Record<string, string> = {
  auth_error: 'Provider authentication is unavailable.',
  request_failed: 'Provider usage could not be refreshed.',
  invalid_response: 'Provider usage response was invalid.',
  adapter_unavailable: 'Usage is not available for this provider.',
  refresh_failed: 'Provider usage could not be refreshed.',
  provider_error: GENERIC_ERROR_MESSAGE,
};

/**
 * Adapters are an internal seam, but their input is still provider-controlled.
 * Normalize again here before a reading can enter the cache or an API response;
 * this keeps a malformed adapter/plugin from smuggling raw response text or a
 * credential into durable state.
 */
function sanitizeReading(provider: AutoProvider, value: unknown): ProviderUsageReading {
  if (typeof value !== 'object' || value === null) {
    return {
      health: 'unknown', source: provider, windows: [],
      error: { code: 'provider_error', message: GENERIC_ERROR_MESSAGE },
    };
  }
  const reading = value as Record<string, unknown>;
  const health = typeof reading.health === 'string' && HEALTHS.has(reading.health as ProviderQuotaHealth)
    ? reading.health as ProviderQuotaHealth
    : 'unknown';
  const source = typeof reading.source === 'string' && SAFE_SOURCES.has(reading.source)
    ? reading.source
    : provider;
  const windows: ProviderUsageWindow[] = [];
  if (Array.isArray(reading.windows)) {
    for (const value of reading.windows.slice(0, 8)) {
      if (typeof value !== 'object' || value === null) continue;
      const window = value as Record<string, unknown>;
      if (typeof window.kind !== 'string' || !WINDOW_KINDS.has(window.kind as ProviderUsageWindow['kind'])) continue;
      const usedPercent = window.usedPercent;
      if (usedPercent !== null && (typeof usedPercent !== 'number' || !Number.isFinite(usedPercent) || usedPercent < 0 || usedPercent > 100)) continue;
      const resetsAt = typeof window.resetsAt === 'string'
        && window.resetsAt.length <= 64
        && Number.isFinite(Date.parse(window.resetsAt))
        ? window.resetsAt
        : undefined;
      windows.push({
        kind: window.kind as ProviderUsageWindow['kind'],
        usedPercent: usedPercent as number | null,
        ...(resetsAt ? { resetsAt } : {}),
        ...(window.hardLimitReached === true ? { hardLimitReached: true } : {}),
      });
    }
  }
  const rawError = reading.error;
  let error: ProviderUsageReading['error'];
  if (typeof rawError === 'object' && rawError !== null) {
    const code = (rawError as Record<string, unknown>).code;
    const safeCode = typeof code === 'string' && SAFE_ERROR_CODES.has(code) ? code : 'provider_error';
    error = { code: safeCode, message: ERROR_MESSAGES[safeCode] ?? GENERIC_ERROR_MESSAGE };
  }
  return { health, source, windows, ...(error ? { error } : {}) };
}

function staleSnapshot(snapshot: ProviderUsageSnapshot, now: number, ttlMs: number): ProviderUsageSnapshot {
  return {
    ...snapshot,
    stale: Date.parse(snapshot.fetchedAt) + ttlMs <= now,
  };
}

function equalMeaningful(a: ProviderUsageSnapshot | undefined, b: ProviderUsageSnapshot): boolean {
  if (!a) return false;
  return a.provider === b.provider
    && a.profileId === b.profileId
    && a.health === b.health
    && a.source === b.source
    && a.stale === b.stale
    && JSON.stringify(a.windows) === JSON.stringify(b.windows)
    && JSON.stringify(a.error) === JSON.stringify(b.error);
}

function earliestFutureReset(snapshot: ProviderUsageSnapshot, now: number): number | undefined {
  return snapshot.windows
    .map((window) => Date.parse(window.resetsAt ?? ''))
    .filter((time) => Number.isFinite(time) && time > now)
    .sort((a, b) => a - b)[0];
}

/**
 * Process-shared cache for provider usage. It serializes only each account's
 * refresh; the coordinator owns cross-account selection and reservations.
 */
export class ProviderUsageService {
  readonly #adapters = new Map<AutoProvider, ProviderUsageAdapter>();
  readonly #cache = new Map<string, ProviderUsageSnapshot>();
  readonly #inFlight = new Map<string, Promise<ProviderUsageSnapshot>>();
  readonly #listeners = new Set<UsageListener>();
  readonly #resetTimers = new Map<string, ReturnType<typeof setTimeout>>();
  readonly #refreshTimer?: ReturnType<typeof setInterval>;
  readonly #now: () => number;

  constructor(private readonly options: ProviderUsageServiceOptions) {
    this.#now = options.now ?? Date.now;
    for (const adapter of options.adapters) this.#adapters.set(adapter.provider, adapter);
    if (options.refreshIntervalMs !== undefined && options.refreshIntervalMs > 0) {
      this.#refreshTimer = setInterval(() => {
        for (const snapshot of this.#cache.values()) {
          void this.refresh({ provider: snapshot.provider, profileId: snapshot.profileId }, true);
        }
      }, options.refreshIntervalMs);
      this.#refreshTimer.unref?.();
    }
  }

  /** Load a prior sanitized cache. Restored values always begin stale. */
  async restore(): Promise<void> {
    if (!this.options.store) return;
    const snapshots = await this.options.store.load().catch(() => []);
    for (const snapshot of snapshots) {
      const reading = sanitizeReading(snapshot.provider, snapshot);
      this.#cache.set(cacheKey(snapshot), { ...snapshot, ...reading, stale: true });
    }
  }

  get(account: ProviderAccountRef): ProviderUsageSnapshot | undefined {
    const snapshot = this.#cache.get(cacheKey(account));
    return snapshot && staleSnapshot(snapshot, this.#now(), this.options.cacheTtlMs);
  }

  onChange(listener: UsageListener): () => void {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  }

  async refresh(account: ProviderAccountRef, force = false): Promise<ProviderUsageSnapshot> {
    const key = cacheKey(account);
    const cached = this.get(account);
    if (!force && cached && !cached.stale) return cached;
    const existing = this.#inFlight.get(key);
    if (existing) return existing;

    const refresh = this.#read(account).finally(() => this.#inFlight.delete(key));
    this.#inFlight.set(key, refresh);
    return refresh;
  }

  dispose(): void {
    if (this.#refreshTimer) clearInterval(this.#refreshTimer);
    for (const timer of this.#resetTimers.values()) clearTimeout(timer);
    this.#resetTimers.clear();
    this.#listeners.clear();
  }

  async #read(account: ProviderAccountRef): Promise<ProviderUsageSnapshot> {
    const adapter = this.#adapters.get(account.provider);
    let reading: ProviderUsageReading;
    if (!adapter) {
      reading = {
        health: 'unavailable', source: 'none', windows: [],
        error: { code: 'adapter_unavailable', message: 'Usage is not available for this provider.' },
      };
    } else {
      try {
        reading = sanitizeReading(account.provider, await adapter.read(account));
      } catch {
        reading = {
          health: 'unknown', source: adapter.provider, windows: [],
          error: { code: 'refresh_failed', message: 'Usage could not be refreshed.' },
        };
      }
    }

    const snapshot: ProviderUsageSnapshot = {
      ...account,
      ...reading,
      fetchedAt: new Date(this.#now()).toISOString(),
      stale: false,
    };
    const key = cacheKey(account);
    const prior = this.#cache.get(key);
    this.#cache.set(key, snapshot);
    this.#scheduleReset(account, snapshot);
    void this.#persist();
    if (!equalMeaningful(prior, snapshot)) {
      for (const listener of this.#listeners) listener(snapshot);
    }
    return snapshot;
  }

  #scheduleReset(account: ProviderAccountRef, snapshot: ProviderUsageSnapshot): void {
    const key = cacheKey(account);
    const previous = this.#resetTimers.get(key);
    if (previous) clearTimeout(previous);
    this.#resetTimers.delete(key);
    const resetAt = earliestFutureReset(snapshot, this.#now());
    if (resetAt === undefined) return;
    const timer = setTimeout(() => {
      this.#resetTimers.delete(key);
      void this.refresh(account, true);
    }, resetAt - this.#now());
    timer.unref?.();
    this.#resetTimers.set(key, timer);
  }

  async #persist(): Promise<void> {
    if (!this.options.store) return;
    await this.options.store.save([...this.#cache.values()]).catch(() => undefined);
  }
}
