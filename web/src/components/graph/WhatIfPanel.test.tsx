/**
 * `whatIf.price` is documented as `null` = follow spot (store/appStore.ts).
 * Clearing the price field must be able to get back there, not land on 0.
 */

import { fireEvent, screen } from '@testing-library/react'
import { beforeEach, describe, expect, it } from 'vitest'
import { useAppStore } from '../../store/appStore'
import { renderWithClient, resetAppStore } from '../../test/utils'
import { WhatIfPanel } from './WhatIfPanel'

beforeEach(() => resetAppStore())

function renderPanel() {
  return renderWithClient(
    <WhatIfPanel selected={['oct-strangle-2026-08-24']} spot={7750} baseline={null} />,
  )
}

describe('WhatIfPanel price field', () => {
  it('commits null ("follow spot") when the field is cleared, not Number(\'\') === 0', () => {
    useAppStore.getState().patchWhatIf({ enabled: true, price: 7800 })
    renderPanel()

    const input = screen.getByLabelText('What-if price value') as HTMLInputElement
    expect(input.value).toBe('7800')

    fireEvent.change(input, { target: { value: '' } })
    expect(useAppStore.getState().graph.whatIf.price).toBe(7800) // not committed until blur
    fireEvent.blur(input)

    expect(useAppStore.getState().graph.whatIf.price).toBeNull()
    expect(input.value).toBe('7750') // falls back to spot, not 0
  })

  it('still commits a typed number normally', () => {
    useAppStore.getState().patchWhatIf({ enabled: true, price: 7800 })
    renderPanel()

    const input = screen.getByLabelText('What-if price value') as HTMLInputElement
    fireEvent.change(input, { target: { value: '7900' } })
    fireEvent.blur(input)

    expect(useAppStore.getState().graph.whatIf.price).toBe(7900)
  })
})
