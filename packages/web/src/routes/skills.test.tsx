import { QueryClientProvider } from '@tanstack/react-query'
import { act, cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import { afterEach, describe, expect, it, vi } from 'vitest'

import { queryKeys, workspaceQueryKeys } from '@/api/queries'
import { createQueryClient } from '@/api/query-client'
import type { Skill, WorkflowsResponse } from '@open-mercato/cezar-api-client'
import { Toaster, resetToasts } from '@/components/ui/toaster'
import { AppRoutes } from '@/routes'

/**
 * `/skills` (R6 Step 1.4): the catalog + detail against fixture payloads and the #377
 * ordering/bold rendering. A pure local-discovery reader since A15 (decision 7) — the import
 * panel, the update banner and the deep-link "Run from GitHub" panel are retired, and
 * so are the tests that pinned them. The pure rules themselves are pinned in lib/skills.test.ts — this file asserts
 * the SURFACE honors them.
 */

// ---- fixtures --------------------------------------------------------------------------------

const skill = (over: Partial<Skill> & Pick<Skill, 'name' | 'source'>): Skill => ({
  body: `# ${over.name}\n\nBody of ${over.name}.`,
  path: `.ai/skills/${over.name}.md`,
  ...over,
})

// Deliberately listed global-first: the SECTION must reorder project-first (#377).
const SKILLS: Skill[] = [
  skill({
    name: 'zebra-global',
    source: 'global',
    path: '/home/u/.agents/skills/zebra-global/SKILL.md',
    description: 'A global skill',
  }),
  skill({ name: 'om-fix', source: 'ai', description: 'Fix an issue end to end' }),
  skill({ name: 'om-review', source: 'cezar', path: '.ai/coducktor/skills/om-review.md' }),
]

const WORKFLOWS: WorkflowsResponse = {
  workflows: [
    {
      name: 'fix-and-verify',
      source: 'file',
      steps: [{ id: 'fix', name: 'Fix', skill: 'om-fix' }],
    },
  ],
  issues: [],
}

let requests: Array<{ method: string; url: string; body?: unknown }> = []

function serve({
  skills = SKILLS,
  workspaceUiState = {},
}: {
  skills?: Skill[]
  workspaceUiState?: Record<string, unknown>
} = {}) {
  requests = []
  // The selection lives in the GLOBAL ui-state (`/api/v1/workspace/ui-state`), whose PUT answers
  // the MERGED state; the stub must merge and return rather than answer `{}`.
  let global: Record<string, unknown> = { ...workspaceUiState }
  const json = (payload: unknown) =>
    new Response(JSON.stringify(payload), { status: 200, headers: { 'content-type': 'application/json' } })
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input)
      const method = init?.method ?? 'GET'
      const body = init?.body ? JSON.parse(String(init.body)) : undefined
      requests.push({ method, url, body })
      if (url === '/api/v1/skills' && method === 'GET') return json(skills)
      if (url === '/api/v1/workflows') return json(WORKFLOWS)
      if (url === '/api/v1/ui-state') return json({}) // per-repo prefs — unused by the page
      if (url === '/api/v1/workspace/ui-state' && method === 'GET') return json(global)
      if (url === '/api/v1/workspace/ui-state' && method === 'PUT') {
        global = { ...global, ...(body as Record<string, unknown>) }
        return json(global)
      }
      return new Promise<never>(() => {})
    }),
  )
}

/** Seeds the step-3.2 route gates — boot id (legacy redirect) + registry (known-check) — so a
 *  flat entry URL lands scoped immediately. The boot project mounts UNSCOPED, so the exact
 *  `/api/v1/*` paths this file's fetch stub matches stay byte-identical. */
function gateSeededClient() {
  const client = createQueryClient()
  client.setQueryData(queryKeys.health, { bootProject: 'boot' })
  client.setQueryData(workspaceQueryKeys.projects, {
    projects: [],
    bootProject: 'boot',
    projectsDir: '~/cezar/projects',
  })
  return client
}

function renderAt(entry: string) {
  const client = gateSeededClient()
  render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={[entry]}>
        <AppRoutes />
        <Toaster />
      </MemoryRouter>
    </QueryClientProvider>,
  )
  return client
}

const rowNames = () =>
  [...document.querySelectorAll('[data-slot="skill-row"]')].map((el) => el.getAttribute('data-skill'))

const detail = () => document.querySelector('[data-slot="skills-detail"] [data-slot="skill-detail"]')

afterEach(() => {
  act(() => resetToasts())
  cleanup()
  vi.unstubAllGlobals()
})

describe('the catalog list', () => {
  it('renders project-first with bold project rows and source tags (#377)', async () => {
    serve()
    renderAt('/skills')

    await waitFor(() => expect(rowNames()).toEqual(['om-fix', 'om-review', 'zebra-global']))
    const rows = [...document.querySelectorAll('[data-slot="skill-row"]')]
    expect(rows[0]?.getAttribute('data-project')).toBe('true')
    expect(rows[1]?.getAttribute('data-project')).toBe('true')
    expect(rows[2]?.hasAttribute('data-project')).toBe(false)
    // The tag says where each skill comes from.
    expect(rows[0]?.querySelector('[data-slot="skill-source"]')?.textContent).toBe('ai')
    expect(rows[2]?.querySelector('[data-slot="skill-source"]')?.textContent).toBe('global')
  })

  it('the first skill is the default selection: detail shows markdown body, path, used-by', async () => {
    serve()
    renderAt('/skills')

    await waitFor(() => expect(detail()).not.toBeNull())
    const pane = detail()!
    expect(pane.querySelector('h2')?.textContent).toBe('om-fix')
    expect(pane.querySelector('[data-slot="skill-path"]')?.textContent).toContain('.ai/skills/om-fix.md')
    // `# om-fix` became a real heading — the body renders as markdown, not a <pre> dump.
    await waitFor(() =>
      expect(pane.querySelector('[data-slot="skill-body"] h1')?.textContent).toBe('om-fix'),
    )
    expect(pane.querySelector('[data-slot="skill-used-by"]')?.textContent).toContain('fix-and-verify › Fix')
  })

  it('clicking a row selects it via the URL and swaps the detail', async () => {
    serve()
    renderAt('/skills')
    await waitFor(() => expect(rowNames()).toHaveLength(3))

    fireEvent.click(document.querySelector('[data-slot="skill-row"][data-skill="om-review"]')!)
    await waitFor(() => expect(detail()?.querySelector('h2')?.textContent).toBe('om-review'))
    expect(
      document
        .querySelector('[data-slot="skill-row"][data-skill="om-review"]')
        ?.getAttribute('aria-current'),
    ).toBe('page')
    // An unreferenced skill says so instead of showing an empty section.
    expect(detail()?.querySelector('[data-slot="skill-used-by"]')?.textContent).toContain(
      'Not referenced by any workflow yet',
    )
  })

  it('the filter narrows the rows and never drops the selection state', async () => {
    serve()
    renderAt('/skills')
    await waitFor(() => expect(rowNames()).toHaveLength(3))

    fireEvent.change(document.querySelector('[data-slot="skills-filter"]')!, {
      target: { value: 'review' },
    })
    expect(rowNames()).toEqual(['om-review'])
  })

  it('an empty catalog explains where skills come from', async () => {
    serve({ skills: [] })
    renderAt('/skills')

    // #374: the hint must mention every project discovery dir, not just `.ai/skills/`.
    await waitFor(() => {
      const text = document.querySelector('[data-slot="skill-rows"]')?.textContent ?? ''
      expect(text).toContain('.ai/skills/')
      expect(text).toContain('.ai/coducktor/skills/')
      expect(text).toContain('.agents/skills/')
    })
    // No skills → the detail pane says there is nothing to pick, rather than inventing one.
    await waitFor(() =>
      expect(document.querySelector('[data-slot="skills-detail"]')?.textContent).toContain(
        'No skill selected',
      ),
    )
  })
})

describe('selection and scroll survive (#384)', () => {
  it('the selected skill and the row container stay put across a re-render', async () => {
    serve()
    renderAt('/skills?skill=om-review')
    await waitFor(() => expect(rowNames()).toHaveLength(3))

    const rowsBefore = document.querySelector('[data-slot="skill-rows"]')!
    rowsBefore.scrollTop = 120

    // A filter round-trip re-renders the rows without remounting the scroll container.
    fireEvent.change(document.querySelector('[data-slot="skills-filter"]')!, {
      target: { value: 'om' },
    })
    expect(rowNames()).toEqual(['om-fix', 'om-review'])

    const rowsAfter = document.querySelector('[data-slot="skill-rows"]')!
    expect(rowsAfter).toBe(rowsBefore)
    expect(rowsAfter.scrollTop).toBe(120)
    expect(
      document
        .querySelector('[data-slot="skill-row"][data-skill="om-review"]')
        ?.getAttribute('aria-current'),
    ).toBe('page')
    expect(detail()?.querySelector('h2')?.textContent).toBe('om-review')
  })

  it('a URL selection that no longer exists falls back to the first skill, never crashes', async () => {
    serve({ skills: SKILLS.filter((s) => s.name !== 'om-review') })
    renderAt('/skills?skill=om-review')
    await waitFor(() => expect(detail()?.querySelector('h2')?.textContent).toBe('om-fix'))
  })
})
