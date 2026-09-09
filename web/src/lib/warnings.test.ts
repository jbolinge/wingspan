import { describe, expect, it } from 'vitest'
import type { Warning } from '../api/types'
import { partitionWarnings, warningKey } from './warnings'

const skipped: Warning = {
  severity: 'info',
  code: 'statement_section_skipped',
  message: '2026-07-14-AccountStatement.csv: skipped section "Equities"',
  trade_id: null,
  contract: null,
}
const duplicate: Warning = {
  severity: 'warn',
  code: 'duplicate_execution_claim',
  message: 'order claims an execution already claimed',
  trade_id: 'lpta-2026-07-24',
  contract: 'SPX260918P6000',
}

describe('warningKey', () => {
  it('is stable for the same warning and distinct for a changed field', () => {
    expect(warningKey(skipped)).toBe(warningKey({ ...skipped }))
    expect(warningKey(skipped)).not.toBe(warningKey({ ...skipped, message: 'other' }))
    expect(warningKey(duplicate)).not.toBe(warningKey({ ...duplicate, trade_id: 'x' }))
    expect(warningKey(duplicate)).not.toBe(warningKey({ ...duplicate, severity: 'error' }))
  })

  it('keeps a null trade id apart from a null contract', () => {
    const a = warningKey({ ...duplicate, trade_id: null, contract: 'A' })
    const b = warningKey({ ...duplicate, trade_id: 'A', contract: null })
    expect(a).not.toBe(b)
  })
})

describe('partitionWarnings', () => {
  it('shows everything when nothing was cleared', () => {
    const { visible, hidden } = partitionWarnings([skipped, duplicate], [])
    expect(visible).toEqual([skipped, duplicate])
    expect(hidden).toEqual([])
  })

  it('hides exactly the cleared fingerprints, keeping order', () => {
    const { visible, hidden } = partitionWarnings([skipped, duplicate], [warningKey(skipped)])
    expect(visible).toEqual([duplicate])
    expect(hidden).toEqual([skipped])
  })

  it('ignores fingerprints of warnings that are no longer reported', () => {
    const { visible, hidden } = partitionWarnings([duplicate], ['stale-key', warningKey(skipped)])
    expect(visible).toEqual([duplicate])
    expect(hidden).toEqual([])
  })
})
