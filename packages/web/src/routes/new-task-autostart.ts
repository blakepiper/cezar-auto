import type { CreateRunInput, RunnerSelection } from '@open-mercato/cezar-api-client'

import { BASELINE_SOURCE, buildCreateRunBody, type TaskSource } from './new-task-form'
import type { NewTaskParams } from './new-task-params'

/**
 * Bookmarklet auto-start (spec 011) — the React half of `handleDeepLink()` in web/app.js,
 * protected by BACKWARD_COMPATIBILITY.md: `/new?skill=&ref=&auto=1&key=` from a saved
 * bookmarklet must behave EXACTLY as it did on the legacy page.
 *
 * Link semantics, retaining the same guarded auto-start behavior as the legacy page:
 *  - no `ref` (nor the `task` alias) → nothing to do beyond a plain composer;
 *  - `auto=1` + the correct launch key → POST /api/runs immediately, unattended;
 *  - `auto=1` + a wrong/missing key (or an unreachable key endpoint) → BLOCKED: prefill
 *    only, the user presses Start;
 *  - no `auto` → prefill + a toast;
 *  - the skill name is NOT validated client-side — the server starts the run and notes
 *    "skill not found … running with the plain prompt" (src/workflows/run.ts). Blocking an
 *    unknown skill here would break saved bookmarklets that legacy honored.
 */

/** The unattended-start body. The runner stays absent when the resolved connected runner is the
 *  server default; a connected fallback is explicit so the server cannot resolve the request
 *  back to a disconnected default. */
export function bookmarkletRunBody(
  params: NewTaskParams,
  runner: RunnerSelection,
  defaultRunner: RunnerSelection,
): CreateRunInput {
  const source: TaskSource =
      params.skill !== ''
        ? { source: 'skill', ref: params.skill }
      : BASELINE_SOURCE
  return buildCreateRunBody({
    task: params.ref,
    source,
    model: '',
    runner,
    defaultRunner,
    variants: 1,
    images: [],
    autonomous: true,
    // Absent for every saved bookmarklet (legacy links have no `todo`); if an in-app link ever
    // arms auto, the inbox bookkeeping (#374) rides along here rather than being lost.
    todoId: params.todo,
  })
}

/** Why the composer is showing a prefilled form instead of a started run. */
export type DeepLinkNotice =
  | { kind: 'prefill' }
  | { kind: 'blocked' }
  | { kind: 'failed'; message: string }

/** The one toast the deep-link handling shows (legacy `alertBar` had exactly one line too).
 *  `unknownSkill` is the skill param when it matches nothing installed here — the honest case
 *  legacy papered over by embedding the name in the task text. */
export function deepLinkToast(
  notice: DeepLinkNotice,
  unknownSkill: string,
): { message: string; tone: 'default' | 'danger' } {
  if (notice.kind === 'failed') {
    return { message: `Auto-start failed: ${notice.message} — review and press Start`, tone: 'danger' }
  }
  if (notice.kind === 'blocked') {
    return { message: 'Auto-start blocked (bad key) — review and press Start', tone: 'danger' }
  }
  if (unknownSkill !== '') {
    return {
      message: `Unknown skill "${unknownSkill}" — prefilled with Baseline; review and press Start`,
      tone: 'danger',
    }
  }
  return { message: 'Prefilled from link — review and press Start', tone: 'default' }
}

/** Legacy's prefill for a skill the picker cannot select: the intent goes into the task text
 *  verbatim (`initFromQuery` wrote exactly this string) and quick-task resolves it from prose. */
export function unknownSkillPrefillText(skill: string, ref: string): string {
  return `Use the "${skill}" skill on: ${ref}`
}
