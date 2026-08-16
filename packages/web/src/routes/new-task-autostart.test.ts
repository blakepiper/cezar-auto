import { describe, expect, it } from 'vitest'

import { deepLinkToast, unknownSkillPrefillText } from './new-task-autostart'

describe('deepLinkToast', () => {
  it('failed carries the server reason, as danger', () => {
    expect(deepLinkToast({ kind: 'failed', message: 'boom' }, '')).toEqual({
      message: 'Auto-start failed: boom — review and press Start',
      tone: 'danger',
    })
  })
  it('blocked names the bad key, as danger — and wins over an unknown skill', () => {
    expect(deepLinkToast({ kind: 'blocked' }, 'ghost')).toEqual({
      message: 'Auto-start blocked (bad key) — review and press Start',
      tone: 'danger',
    })
  })
  it('an unknown skill on a plain prefill is called out honestly', () => {
    expect(deepLinkToast({ kind: 'prefill' }, 'ghost')).toEqual({
      message: 'Unknown skill "ghost" — prefilled with Baseline; review and press Start',
      tone: 'danger',
    })
  })
  it('a plain prefill is a quiet nudge', () => {
    expect(deepLinkToast({ kind: 'prefill' }, '')).toEqual({
      message: 'Prefilled from link — review and press Start',
      tone: 'default',
    })
  })
})

it('unknownSkillPrefillText is byte-for-byte the legacy embedding', () => {
  expect(unknownSkillPrefillText('om-fix', 'https://x')).toBe('Use the "om-fix" skill on: https://x')
})
