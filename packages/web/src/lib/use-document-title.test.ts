import { renderHook } from '@testing-library/react'
import { beforeEach, describe, expect, it } from 'vitest'

import {
  documentTitleOf,
  type DocumentTitleParts,
  useDocumentTitle,
} from './use-document-title'

describe('documentTitleOf', () => {
  it.each([
    {
      name: 'project and page',
      projectName: 'Storefront',
      pageLabel: 'Tasks',
      expected: 'Storefront — Tasks · coducktor',
    },
    {
      name: 'project only',
      projectName: 'Storefront',
      pageLabel: null,
      expected: 'Storefront · coducktor',
    },
    {
      name: 'page only',
      projectName: null,
      pageLabel: 'Settings',
      expected: 'Settings · coducktor',
    },
    { name: 'neither part', projectName: null, pageLabel: null, expected: 'coducktor' },
    { name: 'empty project', projectName: '', pageLabel: 'Tasks', expected: 'Tasks · coducktor' },
    { name: 'blank parts', projectName: '  ', pageLabel: '\t', expected: 'coducktor' },
  ])('formats $name', ({ projectName, pageLabel, expected }) => {
    expect(documentTitleOf({ projectName, pageLabel })).toBe(expected)
  })
})

describe('useDocumentTitle', () => {
  beforeEach(() => {
    document.title = 'coducktor'
  })

  it('updates the existing writer when its truthful inputs change', () => {
    const initialProps: DocumentTitleParts = {
      projectName: 'Storefront',
      pageLabel: 'Tasks',
    }
    const { rerender } = renderHook(
      (parts: DocumentTitleParts) => useDocumentTitle(parts),
      { initialProps },
    )

    expect(document.title).toBe('Storefront — Tasks · coducktor')
    rerender({ projectName: 'Back office', pageLabel: null })
    expect(document.title).toBe('Back office · coducktor')
  })
})
