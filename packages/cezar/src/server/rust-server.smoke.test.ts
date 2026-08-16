import { describe, expect, it } from 'vitest';
import { httpTestRequest, rustHttpBaseUrl } from './rust-server.testkit.ts';

describe('Rust server HTTP harness', () => {
  const baseUrl = rustHttpBaseUrl();

  it.skipIf(!baseUrl)('serves the versioned health contract over a real listener', async () => {
    const response = await httpTestRequest(baseUrl!, '/api/v1/health');
    expect(response.status).toBe(200);
    expect(response.headers.get('access-control-allow-origin')).toBe('*');
    expect(await response.json()).toMatchObject({
      bootProject: 'default',
      capabilities: { followups: false },
    });
  });
});
