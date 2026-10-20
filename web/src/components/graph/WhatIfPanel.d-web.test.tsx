/** The what-if dates itself on the server's day and says what its vol and
 *  baseline are measured against (D1 iii, D3, D5 iv). */

import { screen } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { useAppStore } from '../../store/appStore'
import { renderWithClient, resetAppStore } from '../../test/utils'
import { WHAT_IF_BASELINE_NOTE, WHAT_IF_VOL_NOTE, WhatIfPanel } from './WhatIfPanel'

beforeEach(() => {
  resetAppStore()
  vi.useFakeTimers({ toFake: ['Date'] })
  vi.setSystemTime(new Date(2026, 8, 1, 0, 30))
})
afterEach(() => vi.useRealTimers())

describe('WhatIfPanel', () => {
  it('defaults its date to the server day it is handed', () => {
    useAppStore.getState().patchWhatIf({ enabled: true })
    renderWithClient(<WhatIfPanel selected={['x']} spot={7000} baseline={null} today="2026-08-31" />)
    const input = screen.getByLabelText('What-if date') as HTMLInputElement
    expect(input.value).toBe('2026-08-31')
    expect(input.min).toBe('2026-08-31')
  })

  it('says its vol is on top of the graph shifts and what the baseline is', () => {
    useAppStore.getState().patchWhatIf({ enabled: true })
    renderWithClient(<WhatIfPanel selected={['x']} spot={7000} baseline={null} today="2026-08-31" />)
    expect(WHAT_IF_VOL_NOTE).toMatch(/on top of the graph’s vol shifts/)
    expect(WHAT_IF_BASELINE_NOTE).toMatch(/no vol shift/)
    expect(screen.getByText(new RegExp(WHAT_IF_VOL_NOTE.slice(0, 30)))).toBeInTheDocument()
    expect(screen.getByText('SPX pts')).toBeInTheDocument()
  })
})
