import { existsSync, mkdirSync, rmSync, writeFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { afterAll, beforeAll, describe, expect, it } from 'vitest'

import { AgentBrowser, bootProjectId, readTestEnv } from './agent-browser'

/**
 * `/skills` (R6 Step 1.4) end-to-end against the shared dry-run environment.
 *
 * Reachability: fully reachable. The server discovers skills fresh on every GET, so the
 * suite seeds two real project skills into this worktree's `.ai/skills/` (removed in
 * afterAll) — the catalog renders them bold and first (#377) next to whatever global skills
 * the host machine genuinely has. Nothing here mutates state — team skills, the Refresh
 * button and the hosted-mode deep-link panel are retired (A15, decisions 5/7): local discovery
 * is a pure reader now.
 */

const artifactsDir = resolve(import.meta.dirname, '../../../.ai/qa/artifacts_e2e')
const sessionId = `e2e-settings-skills-${process.pid}`

const DESKTOP = { width: 1440, height: 900 }

const skillsDir = resolve(import.meta.dirname, '../../../.ai/skills')
const ALPHA = 'e2e-alpha-skill'
const BETA = 'e2e-beta-skill'

let browser: AgentBrowser
let baseUrl: string
let createdSkillsDir = false
let bootProject: string

/** A flat route target under this server's own project prefix (multi-project spec, step 3.2):
 *  every cockpit link is scoped, and every legacy flat URL redirects onto its scoped twin. */
const scoped = (path: string) => `/p/${bootProject}${path}`

beforeAll(async () => {
  baseUrl = readTestEnv().baseUrl
  bootProject = await bootProjectId(baseUrl)
  createdSkillsDir = !existsSync(skillsDir)
  mkdirSync(skillsDir, { recursive: true })
  writeFileSync(
    resolve(skillsDir, `${ALPHA}.md`),
    `---\nname: ${ALPHA}\ndescription: An e2e-seeded project skill\n---\n\n# Alpha skill\n\nDo the alpha thing.\n`,
    'utf8',
  )
  writeFileSync(
    resolve(skillsDir, `${BETA}.md`),
    `---\nname: ${BETA}\ndescription: The second seeded skill\n---\n\nDo the beta thing.\n`,
    'utf8',
  )
  browser = AgentBrowser.open(sessionId)
  browser.setViewport(DESKTOP.width, DESKTOP.height)
})

afterAll(() => {
  // Never leave test skills in a developer's catalog.
  rmSync(resolve(skillsDir, `${ALPHA}.md`), { force: true })
  rmSync(resolve(skillsDir, `${BETA}.md`), { force: true })
  if (createdSkillsDir) rmSync(skillsDir, { recursive: true, force: true })
  browser?.close()
})

const row = (name: string) => `[data-slot="skill-row"][data-skill="${name}"]`

describe('settings → skills against the live dry-run server', () => {
  it('the catalog renders the seeded project skills bold-first, and a click opens the markdown detail', () => {
    browser.goto(`${baseUrl}${scoped('/settings/skills')}`)
    browser.waitForFunction(`document.querySelector('${row(ALPHA)}') !== null`)

    // Seeded repo skills are project skills — tagged, emphasized, and ahead of every
    // global/team skill in the list (#377).
    expect(browser.count(`${row(ALPHA)}[data-project="true"]`)).toBe(1)
    expect(browser.count(`${row(BETA)}[data-project="true"]`)).toBe(1)
    const ordered = browser.evaluate(
      `(() => {
        const rows = [...document.querySelectorAll('[data-slot="skill-row"]')]
        const firstGlobal = rows.findIndex((r) => !r.hasAttribute('data-project'))
        const lastProject = rows.map((r) => r.hasAttribute('data-project')).lastIndexOf(true)
        return firstGlobal === -1 || lastProject < firstGlobal
      })()`,
    )
    expect(ordered).toBe(true)

    browser.click(row(ALPHA))
    browser.waitForFunction(
      `document.querySelector('[data-slot="skills-detail"] [data-slot="skill-detail"] h2')?.textContent === '${ALPHA}'`,
    )
    // The body rendered as MARKDOWN: the seeded `# Alpha skill` became a real heading.
    browser.waitForFunction(
      `[...document.querySelectorAll('[data-slot="skill-body"] h1')].some((h) => h.textContent === 'Alpha skill')`,
    )
    expect(browser.text('[data-slot="skill-body"]')).toContain('Do the alpha thing.')
    browser.screenshot(`${artifactsDir}/settings-skills.png`)
  })
})
