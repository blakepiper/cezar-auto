import { describe, expect, it } from 'vitest';
import { httpTestRequest, rustHttpBaseUrl } from './rust-server.testkit.ts';

/**
 * B11 scripted comparison run: the Rust server's behavior on a fixed set of guard/contract
 * assertions, exercised over a REAL TCP listener (not Hono's in-process `app.request()`) — a
 * stronger proof than the native `#[tokio::test]` route-family suites already in
 * `coducktor-server`, because it walks the actual HTTP parsing/handling path a real client uses.
 *
 * These are hand-picked from `origin-guard.test.ts`, `host-guard.test.ts`, `sse-headers.test.ts`,
 * `route-parity.test.ts` and `versioned-surface.test.ts` — the assertions that are genuinely
 * HTTP-request-shaped and therefore portable to an external-process harness. What is NOT here,
 * and why, is recorded in full in `.ai/specs/2026-08-15-rust-tui-refactor-plan.md`'s B11 entry:
 *
 * - `contract-parity.*.test.ts` (5 files) are compile-time TypeScript type-assertion files (their
 *   `it()` exists only to keep the file visible to the runner; the real check is `npm run
 *   typecheck`). There is no HTTP request to retarget — they check the Node route handlers'
 *   inferred TS types against hand-written zod schemas, which has no meaning for a Rust binary.
 * - `bc-route-inventory.test.ts` and two of `versioned-surface.test.ts`'s four tests read Hono's
 *   own in-process route table (`app.routes`) directly — never issue an HTTP request at all, so
 *   there is nothing here to point at an external server.
 * - The FULL `route-parity.test.ts`/`origin-guard.test.ts`/`host-guard.test.ts` suites build a
 *   fresh `createApp()` (and therefore a fresh repoRoot/store/CEZ_HOME) per test, which this
 *   harness's single long-lived external Rust process (one `--repo-root`, started once by
 *   `scripts/test-rust-server.mjs`) cannot give each test its own isolated copy of. Retargeting
 *   them fully needs a per-test spawn of the (already-built) Rust binary against that test's own
 *   temp repoRoot — a real harness capability, not a search-and-replace, and not built here. This
 *   file instead captures each suite's CORE guard/contract behavior against the one shared server
 *   and its `default` boot project, which is enough to prove the guards and headers hold for real
 *   over the wire — the per-test-isolation gap is named as follow-up in the plan doc.
 */
describe('Rust server HTTP parity — origin/host guards, SSE headers, alias parity', () => {
  const baseUrl = rustHttpBaseUrl();

  async function request(path: string, init?: RequestInit): Promise<Response> {
    return httpTestRequest(baseUrl!, path, init);
  }

  // ---- origin-guard.test.ts: CSRF / DNS rebinding -------------------------------------------

  it.skipIf(!baseUrl)('rejects a cross-origin mutating request (blind CSRF) with 403', async () => {
    const res = await request('/api/v1/runs', {
      method: 'POST',
      headers: { 'content-type': 'application/json', origin: 'https://evil.tld' },
      body: JSON.stringify({ task: 'do the thing', steps: [{ id: 'work', prompt: '{{task}}' }] }),
    });
    expect(res.status).toBe(403);
    expect(((await res.json()) as { error: string }).error).toContain('cross-origin');
  });

  it.skipIf(!baseUrl)('rejects an opaque "null" Origin (sandboxed iframe) with 403', async () => {
    const res = await request('/api/v1/runs', {
      method: 'POST',
      headers: { 'content-type': 'application/json', origin: 'null' },
      body: JSON.stringify({ task: 'do the thing', steps: [{ id: 'work', prompt: '{{task}}' }] }),
    });
    expect(res.status).toBe(403);
  });

  it.skipIf(!baseUrl)('allows a same-origin mutating request through', async () => {
    const res = await request('/api/v1/runs', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({
        task: `rust-parity-same-origin-${Date.now()}`,
        steps: [{ id: 'work', prompt: '{{task}}' }],
      }),
    });
    expect(res.status).toBe(201);
  });

  // ---- host-guard.test.ts: DNS rebinding via a foreign Host header --------------------------
  //
  // A real client's Host header is set by the HTTP stack from the actual TCP destination, not by
  // application code — Node's `fetch` (Undici) proved this empirically here: setting a `host`
  // header on the request init does NOT change what is sent on the wire when the connection is a
  // real socket (confirmed independently with `curl -H "Host: attacker.example"` against a
  // manually-started server: the Rust guard correctly answers 403, matching Node's own
  // host-guard.test.ts assertion byte-for-byte — the drift-free part is real, just not provable
  // through THIS harness's HTTP client). So the "rebound Host is rejected" case — the guard's
  // whole reason to exist — genuinely cannot be driven through `fetch()` against an external
  // listener; it is provable only via a raw-socket client (curl, or a custom TCP writer), which is
  // out of scope for a vitest suite. Same limitation as host-guard.test.ts's own "missing Host
  // header" case (real HTTP/1.1 always sends one, `fetch` cannot omit it either). What CAN be
  // proven over `fetch` is the accept path: a genuinely different loopback authority (not a
  // spoofed header, an actually-different request URL) still gets through.
  it.skipIf(!baseUrl)('allows the loopback Host spellings through', async () => {
    const url = new URL(baseUrl!);
    for (const hostname of ['127.0.0.1', 'localhost']) {
      const res = await fetch(`http://${hostname}:${url.port}/api/v1/health`);
      expect(res.status, `hostname ${hostname}`).toBe(200);
    }
  });

  // ---- sse-headers.test.ts: anti-buffering contract ------------------------------------------

  it.skipIf(!baseUrl)('global /api/v1/events carries no-transform and X-Accel-Buffering: no', async () => {
    const res = await request('/api/v1/events');
    expect(res.status).toBe(200);
    expect(res.headers.get('content-type')).toContain('text/event-stream');
    expect(res.headers.get('cache-control')).toBe('no-cache, no-transform');
    expect(res.headers.get('x-accel-buffering')).toBe('no');
    await res.body?.cancel();
  });

  it.skipIf(!baseUrl)('per-run /api/v1/runs/:id/events carries the same contract', async () => {
    const create = await request('/api/v1/runs', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({
        task: `rust-parity-sse-${Date.now()}`,
        steps: [{ id: 'work', prompt: '{{task}}' }],
      }),
    });
    const { id } = (await create.json()) as { id: string };
    const res = await request(`/api/v1/runs/${id}/events`);
    expect(res.status).toBe(200);
    expect(res.headers.get('cache-control')).toBe('no-cache, no-transform');
    expect(res.headers.get('x-accel-buffering')).toBe('no');
    await res.body?.cancel();
  });

  // ---- route-parity.test.ts: unprefixed vs /api/v1/p/default alias --------------------------

  it.skipIf(!baseUrl)('answers the unprefixed and /api/v1/p/default spellings identically', async () => {
    const [unprefixed, scoped] = await Promise.all([
      request('/api/v1/workflows'),
      request('/api/v1/p/default/workflows'),
    ]);
    expect(unprefixed.status).toBe(scoped.status);
    expect(await unprefixed.json()).toEqual(await scoped.json());
  });

  // ---- versioned-surface.test.ts: unknown project + CORS-open health -----------------------

  it.skipIf(!baseUrl)('rejects an unknown project with 404', async () => {
    const res = await request('/api/v1/p/nope/agent-config');
    expect(res.status).toBe(404);
  });

  it.skipIf(!baseUrl)('serves health cross-origin — the discovery endpoint', async () => {
    const res = await request('/api/v1/health');
    expect(res.headers.get('access-control-allow-origin')).toBe('*');
  });
});
