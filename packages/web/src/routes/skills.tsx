import { SparklesIcon, TriangleAlertIcon } from 'lucide-react'
import { useState } from 'react'
import { useSearchParams } from 'react-router'

import { Link } from '@/lib/project-router'

import { useSkills, useWorkflows } from '@/api/queries'
import type { Skill } from '@open-mercato/cezar-api-client'
import { CenteredState } from '@/components/centered-state'
import { SkillDetailBody, SkillSourceTag } from '@/components/skill-detail'
import { SkillEmptyHint } from '@/components/skill-empty-hint'
import { Input } from '@/components/ui/input'
import { filterSkills, isProjectSkill, orderSkills, skillUsedBy } from '@/lib/skills'
import { cn } from '@/lib/utils'

/**
 * `/skills` — the skills catalog as its own top-level surface: catalog + detail. `/settings/skills`
 * redirects here (routes.tsx) so pasted links keep working. Skills are playbooks agents follow,
 * not a knob — so this is a page, not a settings section.
 *
 * A pure local-discovery reader (A15, decision 7 — spec §16a.1): remote team skills, the import
 * panel and the Refresh button are retired along with `skills-remote.ts`/`skills-update.ts`.
 * `~/.agents/skills/` is already a supported discovery location and needs no clone, cache or
 * network. The retired hosted-mode "Run from GitHub" deep-link panel is retired with that
 * whole surface (decision 5) — the GitHub tab's "Hand this to the agent" card replaces it.
 *
 * The two standing feedback items stay built in:
 *  - #377 project-first and bold: the list renders through `orderSkills`/`filterSkills`, the
 *    same pure module every picker uses;
 *  - #384 stable scroll/selection: selection lives in the URL (`?skill=<name>`), the rows are
 *    keyed React elements inside one persistent scroll container — a refresh re-renders rows
 *    in place instead of rebuilding the pane, so neither the selection nor the scroll
 *    position can be lost (the legacy innerHTML rebuild lost both).
 */

export function SkillsRoute() {
  return (
    <div data-route="skills" className="flex min-h-full flex-col">
      {/* Desktop header — below `md` the shell's top bar already says "Skills". */}
      <header className="sticky top-0 z-10 hidden h-14 shrink-0 items-center gap-3 border-b border-border bg-background px-5 md:flex">
        <h1 className="text-base font-semibold">Skills</h1>
        <p className="text-[13px] text-muted-foreground">Markdown playbooks agents can follow.</p>
      </header>
      <SkillsCatalog />
    </div>
  )
}

function SkillsCatalog() {
  const skillsQuery = useSkills()
  const workflowsQuery = useWorkflows()
  const [searchParams] = useSearchParams()
  const [query, setQuery] = useState('')

  if (skillsQuery.isError) {
    return (
      <CenteredState
        icon={<TriangleAlertIcon />}
        tone="danger"
        heading="h2"
        title="Could not load skills"
        subtitle={skillsQuery.error.message}
      />
    )
  }

  const skills = orderSkills(skillsQuery.data ?? [])
  const param = searchParams.get('skill')
  // Explicit choice if it still exists, else the first skill — a vanished selection degrades,
  // it never crashes.
  const selection =
    param !== null && skills.some((skill) => skill.name === param) ? param : (skills[0]?.name ?? null)
  const selected = skills.find((skill) => skill.name === selection) ?? null
  const shown = filterSkills(skills, query)

  return (
    <div data-slot="skills-section" className="flex min-h-full flex-1 items-stretch">
      {/* List pane. Below md it IS the page until a selection is in the URL — the GitHub
          tab's two-surfaces-one-URL rule. */}
      <section
        data-slot="skills-list"
        className={cn(
          'w-full flex-col border-border md:flex md:w-[320px] md:shrink-0 md:border-r',
          // Pin the pane below the sticky h-14 header so the ROWS scroll inside it (the #384
          // stable-scroll surface) — `var(--spacing)*14` tracks the density token.
          'md:sticky md:top-14 md:max-h-[calc(100dvh-(var(--spacing)*14))]',
          param === null ? 'flex' : 'hidden md:flex',
        )}
      >
        <div className="flex shrink-0 items-center gap-2 p-3 pb-2">
          <Input
            data-slot="skills-filter"
            placeholder="Filter skills…"
            aria-label="Filter skills"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            className="h-8 text-[13px]"
          />
        </div>

        <ul data-slot="skill-rows" className="min-h-0 flex-1 overflow-y-auto px-2 pb-2">
          {skillsQuery.isPending ? (
            <li className="px-2.5 py-2 text-[13px] text-soft-foreground">Loading…</li>
          ) : shown.length > 0 ? (
            shown.map((skill) => <SkillRow key={skill.path} skill={skill} active={selection === skill.name} />)
          ) : (
            <li className="px-2.5 py-2 text-xs leading-relaxed text-soft-foreground">
              {skills.length > 0 ? '(no skills match)' : <SkillEmptyHint />}
            </li>
          )}
        </ul>
      </section>

      {/* Detail pane. Hidden below md until the URL carries a selection. */}
      <section
        data-slot="skills-detail"
        className={cn('min-w-0 flex-1 flex-col', param === null ? 'hidden md:flex' : 'flex')}
      >
        <div className="min-w-0 flex-1 px-4 py-4 md:px-7 md:py-5">
          <Link
            to="/skills"
            data-slot="skills-back"
            className="mb-3 inline-flex items-center gap-1.5 text-xs font-medium text-muted-foreground hover:text-foreground md:hidden"
          >
            Back to the list
          </Link>

          {selected ? (
            <SkillDetailBody
              skill={selected}
              usedBy={skillUsedBy(workflowsQuery.data?.workflows ?? [], selected.name)}
            />
          ) : skillsQuery.isPending ? null : (
            <CenteredState
              icon={<SparklesIcon />}
              tone="neutral"
              heading="h2"
              title="No skill selected"
              subtitle="Pick a skill from the catalog."
            />
          )}
        </div>
      </section>
    </div>
  )
}

function SkillRow({ skill, active }: { skill: Skill; active: boolean }) {
  const project = isProjectSkill(skill)
  return (
    <li>
      <Link
        to={`/skills?skill=${encodeURIComponent(skill.name)}`}
        data-slot="skill-row"
        data-skill={skill.name}
        data-project={project ? 'true' : undefined}
        aria-current={active ? 'page' : undefined}
        className={cn(
          'flex flex-col gap-0.5 rounded-md px-2.5 py-2 transition-colors hover:bg-muted',
          active && 'bg-muted',
        )}
      >
        <span className="flex min-w-0 items-center gap-2">
          <SparklesIcon
            aria-hidden="true"
            className={cn('size-3.5 shrink-0', project ? 'text-violet' : 'text-soft-foreground')}
          />
          {/* Project skills read bold (#377) — the visual half of the ordering rule. */}
          <span
            className={cn(
              'min-w-0 truncate font-mono text-[13px]',
              project ? 'font-semibold text-foreground' : 'font-medium text-muted-foreground',
            )}
          >
            {skill.name}
          </span>
          <SkillSourceTag source={skill.source} className="ml-auto" />
        </span>
        {skill.description ? (
          <span className="line-clamp-2 pl-[22px] text-xs text-soft-foreground">{skill.description}</span>
        ) : null}
      </Link>
    </li>
  )
}
