/** Price-slice table: P&L per line at fixed offsets from spot and at each
 *  breakeven, for reading exact numbers off the graph. */

import type { RiskGraphResponse, RiskLine } from '../../api/types'
import { lineLabel } from '../../lib/chartOption'
import { fmtNumber, fmtPnl, pnlColorClass } from '../../lib/format'
import { buildSlices } from '../../lib/readout'

export function SliceTable({ data, lines }: { data: RiskGraphResponse; lines: RiskLine[] }) {
  const ordered = [...lines].sort((a, b) => a.date.localeCompare(b.date))
  const rows = buildSlices(data, ordered)
  const today = data.as_of.slice(0, 10)
  if (rows.length === 0 || ordered.length === 0) return null
  return (
    <div className="ws-card overflow-x-auto">
      <header className="border-b border-line px-2 py-1">
        <h3 className="ws-label m-0 font-semibold">Price slices</h3>
      </header>
      <table className="w-full border-collapse text-[11px]">
        <thead>
          <tr className="text-muted">
            <th className="px-2 py-0.5 text-left font-medium">Price</th>
            <th className="px-2 py-0.5 text-left font-medium">vs spot</th>
            {ordered.map((line) => (
              <th key={line.key} className="px-2 py-0.5 text-right font-medium whitespace-nowrap">
                {lineLabel(line, today)}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {rows.map((row) => (
            <tr key={`${row.price}-${row.label}`} className="border-t border-line/50">
              <td className="num px-2 py-0.5 text-fg-strong">{fmtNumber(row.price, 0)}</td>
              <td className="num px-2 py-0.5 text-muted">{row.label}</td>
              {row.values.map((value, index) => (
                <td
                  key={ordered[index]?.key ?? index}
                  className={`num px-2 py-0.5 text-right ${pnlColorClass(value)}`}
                >
                  {fmtPnl(value, 0)}
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  )
}
