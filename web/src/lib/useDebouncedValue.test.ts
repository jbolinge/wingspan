import { act, renderHook } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { useDebouncedValue } from './useDebouncedValue'

describe('useDebouncedValue', () => {
  afterEach(() => {
    vi.useRealTimers()
  })

  it('returns the initial value immediately, with no delay on mount', () => {
    const { result } = renderHook(() => useDebouncedValue('first', 250))
    expect(result.current).toBe('first')
  })

  it('holds the last value until the input stops changing for the delay', () => {
    vi.useFakeTimers()
    const { result, rerender } = renderHook(({ value }) => useDebouncedValue(value, 250), {
      initialProps: { value: 0 },
    })

    // A burst of rapid changes (a slider mid-drag) — only the settled value
    // should ever be echoed out.
    for (let tick = 1; tick <= 5; tick += 1) {
      rerender({ value: tick })
      act(() => {
        vi.advanceTimersByTime(50)
      })
    }
    expect(result.current).toBe(0)

    act(() => {
      vi.advanceTimersByTime(250)
    })
    expect(result.current).toBe(5)
  })
})
