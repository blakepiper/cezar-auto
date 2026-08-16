export interface UsageMetricVisibility {
  tokens: boolean
  cost: boolean
}

/**
 * Token counts and cost always render (A15, decision 8 — Tier 3's spend-hiding flags and their
 * `capabilities.*` mirrors are retired: hiding
 * spend only ever made sense for someone looking at a shared/demo instance that no longer
 * exists). Kept as a function, not inlined at each call site, so the ~6 call sites that used to
 * read this from `/api/health` don't each need their own edit — this is the one place that used
 * to interpret the capability, and it still is.
 */
export function usageMetricVisibility(): UsageMetricVisibility {
  return { tokens: true, cost: true }
}
