import { describe, expect, it } from 'vitest';
import { httpTestRequest, rustHttpBaseUrl } from './rust-server.testkit.ts';

describe('Rust server HTTP harness', () => {
  const baseUrl = rustHttpBaseUrl();

  async function request(path: string, init?: RequestInit): Promise<Response> {
    return httpTestRequest(baseUrl!, path, init);
  }

  it.skipIf(!baseUrl)('serves the versioned health contract over a real listener', async () => {
    const response = await httpTestRequest(baseUrl!, '/api/v1/health');
    expect(response.status).toBe(200);
    expect(response.headers.get('access-control-allow-origin')).toBe('*');
    expect(await response.json()).toMatchObject({
      bootProject: 'default',
      capabilities: { followups: false },
    });
  });

  it.skipIf(!baseUrl)('serves the runs lifecycle family over a real listener', async () => {
    const task = `rust-server-smoke-${Date.now()}`;
    const create = await request('/api/v1/runs', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ task, steps: [{ id: 'work', prompt: '{{task}}' }] }),
    });
    expect(create.status).toBe(201);
    const created = (await create.json()) as { id: string; task: string };
    expect(created.task).toBe(task);

    const listed = await request('/api/v1/runs');
    expect(listed.status).toBe(200);
    expect(((await listed.json()) as Array<{ id: string }>).some((run) => run.id === created.id)).toBe(true);

    const patched = await request(`/api/v1/runs/${created.id}`, {
      method: 'PATCH',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ title: 'Rust smoke title' }),
    });
    expect(patched.status).toBe(200);
    expect((await patched.json()) as { titleSummary: string }).toMatchObject({
      titleSummary: 'Rust smoke title',
    });

    const read = await request(`/api/v1/runs/${created.id}/read`, { method: 'POST' });
    expect(read.status).toBe(200);
    const archived = await request(`/api/v1/runs/${created.id}/archive`, { method: 'POST' });
    expect(archived.status).toBe(200);
    expect((await archived.json()) as { archived: boolean }).toMatchObject({ archived: true });

    const deleted = await request(`/api/v1/runs/${created.id}`, { method: 'DELETE' });
    expect(deleted.status).toBe(200);
    expect(await deleted.json()).toEqual({ deleted: true });
  });
});
