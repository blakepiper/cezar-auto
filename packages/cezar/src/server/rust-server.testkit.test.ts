import { describe, expect, it } from 'vitest';
import { httpTestRequest, selectedHttpTarget } from './rust-server.testkit.ts';

describe('B9 HTTP-suite transport seam', () => {
  it('keeps the default target in-process', async () => {
    const app = {
      request: async (input: string, init?: RequestInit) =>
        new Response(JSON.stringify({ input, hasHost: new Headers(init?.headers).has('host') }), {
          headers: { 'content-type': 'application/json' },
        }),
    } as never;
    const response = await httpTestRequest(selectedHttpTarget(app), '/api/v1/health');
    expect(await response.json()).toEqual({ input: '/api/v1/health', hasHost: true });
  });

  it('accepts an externally selected base URL without changing request assertions', async () => {
    const previous = process.env.DUCK_HTTP_BASE_URL;
    process.env.DUCK_HTTP_BASE_URL = 'http://127.0.0.1:49999';
    try {
      const target = selectedHttpTarget({ request: (() => Promise.reject(new Error('unused'))) } as never);
      expect(target).toBe('http://127.0.0.1:49999');
    } finally {
      if (previous === undefined) delete process.env.DUCK_HTTP_BASE_URL;
      else process.env.DUCK_HTTP_BASE_URL = previous;
    }
  });
});
