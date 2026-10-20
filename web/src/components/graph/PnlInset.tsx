/**
 * P&L-only inset drawn inside grid 0's plot area, top-left. It takes no
 * pointer events (it sits over the curves it names) and is deliberately
 * narrow: the greeks now live in the strip below the chart, so this card
 * exists to name the visible lines and price them, not to duplicate the
 * strip's Δ/Θ/vega rows.
 */

import type { RiskGraphResponse, RiskLine } from '../../api/types'
import { fmtNumber, fmtPnl, pnlColorClass } from '../../lib/format'
import { chartPalette, tplusColor } from '../../lib/palette'
import { indexAt } from '../../lib/readout'
import { lineLabel } from '../../lib/chartOption'
import type { Theme } from '../../store/appStore'
import { SwatchLine } from '../ui'

export function PnlInset({
  data,
  lines,
  index,
  theme,
}: {
  data: Pick<RiskGraphResponse, 'prices' | 'spot'>
  lines: RiskLine[]
  index: number | null
  theme: Theme
}) {
  const palette = chartPalette(theme)
  // With no cursor the inset parks on spot, where the T+0 line equals the
  // dashboard's total P&L — so the two panels can be checked against each
  // other. Spot is a node of the grid itself (the server inserts it), so
  // this is the exact spot sample, not a neighbour. Both prices go through
  // `indexAt`, which refuses one that is off the fetched grid: panned far
  // enough, the coverage tile on screen need not hold spot at all, and an
  // edge sample dressed up as "At spot" would break that cross-check
  // silently.
  const effective = index ?? indexAt(data.prices, data.spot)
  const ordered = [...lines].sort((a, b) => a.date.localeCompare(b.date))
  const tplus = ordered.filter((l) => l.kind === 'tplus')
  const tplusRank = new Map(tplus.map((line, i) => [line.key, i]))

  return (
    <div
      data-testid="pnl-inset"
      className="pointer-events-none absolute left-[76px] top-2 z-10 rounded border border-line bg-panel/85 px-2 py-1 text-[11px] backdrop-blur-sm"
    >
      <div className="ws-label mb-0.5 font-semibold">
        {index !== null && effective !== null
          ? 'At cursor'
          : effective !== null
            ? 'At spot'
            : 'Outside the fetched range'}
        {effective !== null ? (
          <span className="num ml-2 text-[11px] text-fg-strong">
            {fmtNumber(data.prices[effective], 0)}
          </span>
        ) : null}
      </div>
      {ordered.map((line) => {
        const color =
          line.kind === 't0'
            ? palette.t0
            : line.kind === 'tplus'
              ? tplusColor(palette, tplusRank.get(line.key) ?? 0, tplus.length)
              : palette.expiry
        const pnl = effective === null ? null : (line.pnl[effective] ?? null)
        return (
          <div key={line.key} data-testid="pnl-inset-row" className="flex items-center gap-1">
            <SwatchLine color={color} dashed={line.kind === 'tplus'} />
            <span className="num">{lineLabel(line)}</span>
            <span className={`num ml-1 font-semibold ${pnlColorClass(pnl)}`}>
              {fmtPnl(pnl, 0)}
            </span>
          </div>
        )
      })}
    </div>
  )
}
