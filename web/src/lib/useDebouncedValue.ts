import { useEffect, useState } from 'react'

/**
 * Echoes `value` after it has held still for `delayMs` — the first render
 * returns `value` immediately (no delay on initial mount/selection), so only
 * rapid-fire changes (a slider mid-drag) are held back.
 */
export function useDebouncedValue<T>(value: T, delayMs: number): T {
  const [debounced, setDebounced] = useState(value)
  useEffect(() => {
    const timer = setTimeout(() => setDebounced(value), delayMs)
    return () => clearTimeout(timer)
  }, [value, delayMs])
  return debounced
}
