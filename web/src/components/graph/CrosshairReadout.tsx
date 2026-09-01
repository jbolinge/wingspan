/** Per-line P&L / Δ / Θ under the cursor. */

import type { RiskGraphResponse, RiskLine } from '../../api/types'
import { todayIso } from '../../lib/dates'
import { fmtNumber, fmtPnl, pnlColorClass } from '../../lib/format'
import { chartPalette, tplusColor } from '../../lib/palette'
import { nearestPriceIndex, readoutAt } from '../../lib/readout'
import { lineLabel } from '../../lib/chartOption'
import type { Theme } from '../../store/appStore'
import { InfoTip, SwatchLine } from '../ui'

export function CrosshairReadout({
  data,
  lines,
  index,
  theme,
}: {
  data: RiskGraphResponse
  lines: RiskLine[]
  index: number | null
  theme: Theme
}) {
  const palette = chartPalette(theme)
  // With no cursor the readout parks on spot, where the T+0 line equals the
  // dashboard's total P&L — so the two panels can be checked against each other.
  const effective = index ?? nearestPriceIndex(data.prices, data.spot)
  // `lines` is the visible set: the readout must list the curves that are
  // actually plotted, and measure Θ against the next *visible* line.
  const readout = readoutAt({ prices: data.prices, lines }, effective)
  if (!readout) return null
  // Local calendar day, not the server's UTC as_of sliced to a date — see
  // GraphControls.
  const today = todayIso()
  const ordered = [...lines].sort((a, b) => a.date.localeCompare(b.date))
  const tplus = ordered.filter((l) => l.kind === 'tplus')
  const tplusRank = new Map(tplus.map((line, index) => [line.key, index]))

  return (
    <div className="ws-card">
      <header className="flex items-center justify-between border-b border-line px-2 py-1">
        <h3 className="ws-label m-0 font-semibold">
          {index === null ? 'At spot' : 'At cursor'}
          <span className="num ml-2 text-[12px] text-fg-strong">
            {fmtNumber(readout.price, 0)}
          </span>
        </h3>
        <span className="text-[10px] text-muted">
          Δ $/pt · Θ $/day
          <InfoTip content="Δ is the slope of that line at the cursor, in dollars per index point. Θ is the drop to the next line out, in dollars per calendar day — both read straight off the plotted curves." />
        </span>
      </header>
      <table className="w-full border-collapse text-[11px]">
        <thead>
          <tr className="text-muted">
            <th className="px-2 py-0.5 text-left font-medium">Line</th>
            <th className="px-2 py-0.5 text-right font-medium">P&amp;L</th>
            <th className="px-2 py-0.5 text-right font-medium">Δ</th>
            <th className="px-2 py-0.5 text-right font-medium">Θ</th>
          </tr>
        </thead>
        <tbody>
          {readout.rows.map((row) => {
            const color =
              row.kind === 't0'
                ? palette.t0
                : row.kind === 'tplus'
                  ? tplusColor(palette, tplusRank.get(row.key) ?? 0, tplus.length)
                  : palette.expiry
            const line = ordered.find((l) => l.key === row.key)
            return (
              <tr key={row.key} className="border-t border-line/50">
                <td className="px-2 py-0.5 whitespace-nowrap">
                  <SwatchLine color={color} dashed={row.kind === 'tplus'} />
                  <span className="num ml-1">{line ? lineLabel(line, today) : row.date}</span>
                </td>
                <td className={`num px-2 py-0.5 text-right font-semibold ${pnlColorClass(row.pnl)}`}>
                  {fmtPnl(row.pnl, 0)}
                </td>
                <td className="num px-2 py-0.5 text-right">{fmtNumber(row.delta, 1)}</td>
                <td className={`num px-2 py-0.5 text-right ${pnlColorClass(row.theta)}`}>
                  {row.theta === null ? '—' : fmtNumber(row.theta, 0)}
                </td>
              </tr>
            )
          })}
        </tbody>
      </table>
    </div>
  )
}
