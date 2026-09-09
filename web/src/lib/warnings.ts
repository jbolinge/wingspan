/** Client-side clearing of data-health warnings.
 *
 *  The server re-derives every warning from the trade files on each load, so
 *  "clear" cannot be a server call: it is a remembered fingerprint of the
 *  warning, and a warning whose fingerprint is remembered stays hidden until
 *  the trader restores it — through reloads, relaunches and folder re-picks
 *  alike. The fingerprint is the whole warning, so a changed message (a
 *  different statement, a different quantity) surfaces again as new. */

import type { Warning } from '../api/types'

/** ASCII unit separator: cannot occur in a code, an id or a CSV-derived message. */
const SEP = String.fromCharCode(31)

export function warningKey(warning: Warning): string {
  return [
    warning.severity,
    warning.code,
    warning.trade_id ?? '',
    warning.contract ?? '',
    warning.message,
  ].join(SEP)
}

/** Splits the server's list into what is shown and what the trader cleared. */
export function partitionWarnings(
  warnings: readonly Warning[],
  dismissed: readonly string[],
): { visible: Warning[]; hidden: Warning[] } {
  if (dismissed.length === 0) return { visible: [...warnings], hidden: [] }
  const cleared = new Set(dismissed)
  const visible: Warning[] = []
  const hidden: Warning[] = []
  for (const warning of warnings) (cleared.has(warningKey(warning)) ? hidden : visible).push(warning)
  return { visible, hidden }
}
