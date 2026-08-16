import { describe, expect, it } from 'vitest'

import { usageMetricVisibility } from './token-metrics'

describe('usageMetricVisibility', () => {
  it('is always visible — the hide-spend capability flags are retired (A15, decision 8)', () => {
    expect(usageMetricVisibility()).toEqual({ tokens: true, cost: true })
  })
})
