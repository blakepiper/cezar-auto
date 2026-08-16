import { QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { MemoryRouter, useLocation } from 'react-router'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { createQueryClient } from '@/api/query-client'
import type { ProjectListEntry } from '@open-mercato/cezar-api-client'
import { AddProjectDialog } from '@/components/add-project-dialog'

/**
 * The add-project dialog (multi-project spec, "Add project" / step 4.2). A15 (decision 8)
 * retired the folder browser with `GET /api/v1/fs/browse` — the dialog now takes the absolute
 * path as typed text and registers it with `POST /api/v1/projects`.
 *
 * Driven through a stubbed `fetch` rather than a mocked client: the request the dialog actually
 * puts on the wire (the POST body) is half of what this step is, and a mocked client would
 * assert the dialog's intent instead of its behavior.
 */

const fetchMock = vi.fn<typeof fetch>()

beforeEach(() => {
  vi.stubGlobal('fetch', fetchMock)
})

afterEach(() => {
  cleanup()
  fetchMock.mockReset()
  vi.unstubAllGlobals()
})

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } })
}

function project(over: Partial<ProjectListEntry> = {}): ProjectListEntry {
  return {
    id: 'cezar',
    name: 'cezar',
    root: '/home/me/Projects/cezar',
    addedAt: '2026-07-01T00:00:00.000Z',
    lastOpenedAt: '2026-07-20T12:00:00.000Z',
    source: 'local',
    status: 'ok',
    ...over,
  }
}

const posted: { root: string }[] = []

function serve({ projects = [], register }: {
  projects?: ProjectListEntry[]
  /** What `POST /api/v1/projects` answers. Receives the posted root. */
  register?: (root: string) => Response
} = {}): void {
  posted.length = 0
  fetchMock.mockImplementation(async (input, init) => {
    const url = new URL(String(input), 'http://localhost')
    if (url.pathname === '/api/v1/projects' && init?.method === 'POST') {
      const root = (JSON.parse(String(init.body)) as { root: string }).root
      posted.push({ root })
      return register ? register(root) : json({ project: project({ id: 'added', root }) })
    }
    if (url.pathname === '/api/v1/projects') return json({ projects, bootProject: 'cezar', projectsDir: '~/cezar/projects' })
    return json({ error: `unexpected ${String(init?.method ?? 'GET')} ${url.pathname}` }, 404)
  })
}

/** Makes the post-registration navigation assertable. */
function LocationProbe() {
  return <span data-testid="location">{useLocation().pathname}</span>
}

function renderDialog() {
  const onOpenChange = vi.fn()
  render(
    <QueryClientProvider client={createQueryClient()}>
      <MemoryRouter initialEntries={['/p/cezar/']}>
        <AddProjectDialog open onOpenChange={onOpenChange} />
        <LocationProbe />
      </MemoryRouter>
    </QueryClientProvider>,
  )
  return { onOpenChange }
}

const input = () => screen.getByRole('textbox', { name: 'Project folder' }) as HTMLInputElement
const addButton = () => document.querySelector('[data-slot="add-project-confirm"]') as HTMLButtonElement

describe('AddProjectDialog', () => {
  it('registers the typed absolute path and navigates to its scope', async () => {
    serve({ register: (root) => json({ project: project({ id: 'notes', name: 'notes', root }) }) })
    renderDialog()

    // Empty path — nothing to register yet.
    expect(addButton().disabled).toBe(true)
    fireEvent.change(input(), { target: { value: '/home/me/Projects/notes' } })
    expect(addButton().disabled).toBe(false)
    fireEvent.click(addButton())

    await waitFor(() => expect(screen.getByTestId('location').textContent).toBe('/p/notes/'))
    expect(posted).toEqual([{ root: '/home/me/Projects/notes' }])
  })

  it('trims the typed path before registering', async () => {
    serve()
    renderDialog()
    fireEvent.change(input(), { target: { value: '  /home/me/Projects/cezar  ' } })
    fireEvent.click(addButton())
    await waitFor(() => expect(posted).toEqual([{ root: '/home/me/Projects/cezar' }]))
  })

  it('accepts a NON-GIT folder — cezar degrades in it, so the dialog must not invent a restriction', async () => {
    serve({ register: (root) => json({ project: project({ id: 'notes', name: 'notes', root, status: 'not-git' }) }) })
    renderDialog()
    fireEvent.change(input(), { target: { value: '/home/me/Projects/notes' } })
    fireEvent.click(addButton())
    await waitFor(() => expect(screen.getByTestId('location').textContent).toBe('/p/notes/'))
  })

  it('navigates to the existing entry when the server answers 409 (already registered)', async () => {
    serve({
      projects: [project({ id: 'cezar', root: '/home/me/Projects/cezar' })],
      // The registry dedupes by realpath and answers the EXISTING entry — not a dead end.
      register: (root) =>
        json({ project: project({ id: 'cezar', root }), error: 'already registered as cezar' }, 409),
    })
    renderDialog()
    fireEvent.change(input(), { target: { value: '/home/me/Projects/cezar' } })
    fireEvent.click(addButton())
    await waitFor(() => expect(screen.getByTestId('location').textContent).toBe('/p/cezar/'))
    expect(document.querySelector('[data-slot="add-project-error"]')).toBeNull()
  })

  it('shows a register refusal verbatim and stays put', async () => {
    serve({
      register: () => json({ error: 'not a project folder: ~ is your home directory or a cezar task worktree' }, 400),
    })
    const { onOpenChange } = renderDialog()
    fireEvent.change(input(), { target: { value: '~' } })
    fireEvent.click(addButton())
    await waitFor(() =>
      expect(document.querySelector('[data-slot="add-project-error"]')?.textContent).toBe(
        'not a project folder: ~ is your home directory or a cezar task worktree',
      ),
    )
    expect(screen.getByTestId('location').textContent).toBe('/p/cezar/')
    expect(onOpenChange).not.toHaveBeenCalled()
    // Server messages quote unbreakable paths — they must wrap, not widen the dialog column.
    expect(document.querySelector('[data-slot="add-project-error"]')?.className).toContain('break-words')
  })

  it('clears a stale register error as soon as the path changes again', async () => {
    serve({
      register: () => json({ error: 'not writable: /opt/checkouts' }, 400),
    })
    renderDialog()
    fireEvent.change(input(), { target: { value: '/opt/checkouts' } })
    fireEvent.click(addButton())
    await waitFor(() => expect(document.querySelector('[data-slot="add-project-error"]')).not.toBeNull())

    // The error names a path that is no longer in the field — leaving it up would be a lie.
    fireEvent.change(input(), { target: { value: '/home/me/Projects/cezar' } })
    await waitFor(() => expect(document.querySelector('[data-slot="add-project-error"]')).toBeNull())
  })
})
