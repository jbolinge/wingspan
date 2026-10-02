import { useCallback, useSyncExternalStore } from 'react'

/**
 * Whether a CSS media query matches, kept current as the window resizes.
 *
 * For the rare layout CSS alone cannot express — a table that changes its
 * orientation, not just its size, where rendering both and hiding one would
 * put two same-named tables in front of a test and of anything walking the
 * DOM. Everything else stays a Tailwind breakpoint; a query written here must
 * be the same one the breakpoint compiles to, so the two flip together.
 */
export function useMediaQuery(query: string): boolean {
  const subscribe = useCallback(
    (onChange: () => void) => {
      const list = window.matchMedia(query)
      list.addEventListener('change', onChange)
      return () => list.removeEventListener('change', onChange)
    },
    [query],
  )
  return useSyncExternalStore(
    subscribe,
    () => window.matchMedia(query).matches,
    () => false,
  )
}

/** Tailwind's `min-[1400px]:` — where the Model tab's ticket becomes a rail. */
export const RAIL_QUERY = '(width >= 1400px)'
