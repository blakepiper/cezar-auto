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

  it.skipIf(!baseUrl)('serves workspace registry and preference routes over a real listener', async () => {
    const projects = await request('/api/v1/projects');
    expect(projects.status).toBe(200);
    const projectBody = (await projects.json()) as { projects: unknown[] };
    expect(projectBody.projects.length).toBe(0);

    const config = await request('/api/v1/workspace/config');
    expect(config.status).toBe(200);
    const configBody = (await config.json()) as { resources: { maxParallel: number } };
    expect(configBody.resources.maxParallel).toBe(2);

    const updatedConfig = await request('/api/v1/workspace/config', {
      method: 'PUT',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ resources: { maxParallel: 4 } }),
    });
    expect(updatedConfig.status).toBe(200);
    const updatedConfigBody = (await updatedConfig.json()) as { resources: { maxParallel: number } };
    expect(updatedConfigBody.resources.maxParallel).toBe(4);

    const updatedUiState = await request('/api/v1/workspace/ui-state', {
      method: 'PUT',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ appearance: { density: 'compact' }, futurePreference: { enabled: true } }),
    });
    expect(updatedUiState.status).toBe(200);
    const updatedUiStateBody = (await updatedUiState.json()) as { futurePreference: { enabled: boolean } };
    expect(updatedUiStateBody.futurePreference.enabled).toBe(true);
  });

  it.skipIf(!baseUrl)('serves skills and workflows over a real listener', async () => {
    const skills = await request('/api/v1/skills');
    expect(skills.status).toBe(200);
    expect(Array.isArray(await skills.json())).toBe(true);

    const workflows = await request('/api/v1/workflows');
    expect(workflows.status).toBe(200);
    const workflowBody = (await workflows.json()) as { workflows: Array<{ name: string }> };
    expect(workflowBody.workflows[0]?.name).toBe('quick-task');

    const saved = await request('/api/v1/workflows', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({
        name: 'Rust Smoke',
        steps: [{ id: 'work', prompt: '{{task}}' }],
      }),
    });
    expect(saved.status).toBe(201);

    const parsed = await request('/api/v1/workflows/parse', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ yaml: 'name: parsed\nskills:\n  - guide\n' }),
    });
    expect(parsed.status).toBe(200);
    const parsedBody = (await parsed.json()) as { steps: Array<{ skill?: string }> };
    expect(parsedBody.steps[0]?.skill).toBe('guide');

    const removed = await request('/api/v1/workflows/Rust%20Smoke', { method: 'DELETE' });
    expect(removed.status).toBe(200);
    const missing = await request('/api/v1/workflows/Rust%20Smoke', { method: 'DELETE' });
    expect(missing.status).toBe(404);
  });

  it.skipIf(!baseUrl)('serves per-repo ui-state and gates the follow-up inbox', async () => {
    const updated = await request('/api/v1/ui-state', {
      method: 'PUT',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ appearance: { density: 'compact' }, futurePreference: { enabled: true } }),
    });
    expect(updated.status).toBe(200);
    const updatedBody = (await updated.json()) as { futurePreference: { enabled: boolean } };
    expect(updatedBody.futurePreference.enabled).toBe(true);

    const scoped = await request('/api/v1/p/default/ui-state');
    expect(scoped.status).toBe(200);
    const scopedBody = (await scoped.json()) as { appearance: { density: string } };
    expect(scopedBody.appearance.density).toBe('compact');

    const todos = await request('/api/v1/todos');
    expect(todos.status).toBe(200);
    expect(await todos.json()).toEqual([]);

    const dismissed = await request('/api/v1/todos/missing', { method: 'DELETE' });
    expect(dismissed.status).toBe(409);
    expect(((await dismissed.json()) as { error: string }).error).toContain('CEZ_FOLLOWUPS');

    const config = await request('/api/v1/config', {
      method: 'PUT',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ systemPrompt: '  Rust smoke config  ', defaultModels: { claude: 'opus' } }),
    });
    expect(config.status).toBe(200);
    const configBody = (await config.json()) as { systemPrompt: string; defaultModels: { claude: string } };
    expect(configBody.systemPrompt).toBe('Rust smoke config');
    expect(configBody.defaultModels.claude).toBe('opus');

    const scopedConfig = await request('/api/v1/p/default/config');
    expect(scopedConfig.status).toBe(200);
    expect(((await scopedConfig.json()) as { defaultModels: { claude: string } }).defaultModels.claude).toBe('opus');
  });
});
