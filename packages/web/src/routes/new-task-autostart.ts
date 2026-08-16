/**
 * Deep-link prefill for `/new?skill=&ref=` — the React half of legacy's `handleDeepLink()`.
 *
 * The hosted-mode deep-link surface's unattended auto-start (`auto=1` + a matching
 * launch key posting the run immediately) is retired (A15, decision 5 — `launch-key.ts`
 * and `GET /api/v1/launch-key` are gone). What remains, matching legacy's honest degradation
 * path for every other case:
 *  - no `ref` (nor the `task` alias) → nothing to do beyond a plain composer;
 *  - `auto=1` → always the blocked path: prefill only, the user presses Start;
 *  - no `auto` → prefill + a toast;
 *  - the skill name is NOT validated client-side — the server starts the run and notes
 *    "skill not found … running with the plain prompt" (src/workflows/run.ts). Blocking an
 *    unknown skill here would break saved links that legacy honored.
 */

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
