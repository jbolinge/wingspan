/**
 * The once-per-process latch behind the first-run folder prompt.
 *
 * Lives outside the component so that "have we already decided?" survives
 * remounts (a tab switch must not re-prompt) while staying resettable from
 * vitest. Keeping it here also keeps `TradesRootControl.tsx` exporting
 * nothing but its component, which is what React Fast Refresh needs.
 */

let decided = false

/**
 * `true` exactly once per process — on the first successful `trades_root`
 * result. Every later call is `false`, so a refetch that merely repeats
 * `source === 'empty'` (a Clear confirmation closing, say) cannot pop the
 * folder dialog back open.
 */
export function claimFirstRunPrompt(): boolean {
  if (decided) return false
  decided = true
  return true
}

/** Test seam: let the next mount decide fresh whether to auto-open. */
export function resetFirstRunPrompt(): void {
  decided = false
}
