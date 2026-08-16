/**
 * Transport seam for the B9 HTTP-suite reuse.
 *
 * The existing server tests use Hono's in-process `app.request()` for fast unit coverage. B9
 * adds a second target: when `DUCK_HTTP_BASE_URL` is set, the same request-shaped assertions can
 * use a real Rust listener instead. Keeping the target here means route-family suites do not
 * learn about child processes, ports, or Rust-specific startup details.
 */
import type { Hono } from 'hono';

export type HttpTestTarget = Hono | string;

export function rustHttpBaseUrl(): string | undefined {
  const value = process.env.DUCK_HTTP_BASE_URL?.trim();
  return value === '' ? undefined : value;
}

export function isExternalHttpTarget(target: HttpTestTarget): target is string {
  return typeof target === 'string';
}

/** Send an API request to either Hono or an externally launched Rust server. */
export async function httpTestRequest(
  target: HttpTestTarget,
  input: string,
  init?: RequestInit,
): Promise<Response> {
  const headers = new Headers(init?.headers);
  if (typeof target === 'string') {
    const url = new URL(input, target);
    if (!headers.has('host')) headers.set('host', url.host);
    return fetch(url, { ...init, headers });
  }
  if (!headers.has('host')) headers.set('host', '127.0.0.1:4321');
  return target.request(input, { ...init, headers });
}

/**
 * Resolve the test target selected by the B9 runner. Tests that need to exercise the Node app
 * keep passing their Hono instance; route-parity suites may use this helper after their fixture
 * setup has been made transport-neutral.
 */
export function selectedHttpTarget(app: Hono): HttpTestTarget {
  return rustHttpBaseUrl() ?? app;
}
