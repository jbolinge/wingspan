/**
 * Round-3 review fix: on an expiry day the engine prices the default what-if
 * date (today) at now, the T+0 line (`time::what_if_instant`), so the panel
 * must not call that number the expiration's settlement.
 */

import { screen } from '@testing-library/react'
import { beforeEach, describe, expect, it } from 'vitest'
import { useAppStore } from '../../store/appStore'
import { renderWithClient, resetAppStore } from '../../test/utils'
import { WHAT_IF_EXPIRY_NOTE, WHAT_IF_TODAY_EXPIRY_NOTE, WhatIfPanel } from './WhatIfPanel'

beforeEach(() => resetAppStore())

describe('WhatIfPanel on an expiry day', () => {
  it('says today prices at T+0, not at settlement, when today is an expiry', () => {
    useAppStore.getState().patchWhatIf({ enabled: true, date: null })
    renderWithClient(
      <WhatIfPanel
        selected={['a']}
        spot={6200}
        baseline={null}
        today="2026-10-16"
        expiries={['2026-10-16', '2026-12-18']}
      />,
    )
    const note = screen.getByTestId('what-if-expiry-note')
    expect(note).toHaveTextContent(WHAT_IF_TODAY_EXPIRY_NOTE)
    expect(note).not.toHaveTextContent(/priced at that expiration’s settlement/)
  })

  it('still says a later expiry prices at its settlement', () => {
    useAppStore.getState().patchWhatIf({ enabled: true, date: '2026-12-18' })
    renderWithClient(
      <WhatIfPanel
        selected={['a']}
        spot={6200}
        baseline={null}
        today="2026-10-16"
        expiries={['2026-10-16', '2026-12-18']}
      />,
    )
    expect(screen.getByTestId('what-if-expiry-note')).toHaveTextContent(
      /Dec 18 2026 is an expiry: priced at that expiration’s settlement/,
    )
  })

  it('carries the today exception in the generic rule', () => {
    expect(WHAT_IF_EXPIRY_NOTE).toMatch(/Today is the exception: it prices now, the T\+0 line/)
  })
})
