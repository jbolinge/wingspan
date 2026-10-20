/** Editor for the risk-graph date list.
 *
 *  The wire format is the frozen one — `today`, `+Nd`, `expiry:YYYY-MM-DD` —
 *  so an arbitrary calendar date the trader picks is converted to an offset
 *  rather than smuggled through as a new shape. */

import { useState } from 'react'
import type { IsoDate, RiskLine } from '../../api/types'
import { dateSpecDate, dateSpecLabel, dateToSpec } from '../../lib/dateSpec'
import { daysBetween, formatDate, formatExpiryShort, todayIso } from '../../lib/dates'
import { effectiveDateSpecs, evenLadder, presetDates } from '../../lib/graphRequest'
import { useAppStore } from '../../store/appStore'

export function DateEditor({
  expiries,
  frontDte,
  lines,
  today: serverToday,
}: {
  /** The live expiries, front first. */
  expiries: IsoDate[]
  frontDte: number | null
  /** The lines the server drew — what the preset in use actually is. */
  lines: readonly RiskLine[]
  /** The server's date (the T+0 line's); the browser's only until a
   *  response has said. */
  today?: IsoDate | null
}) {
  const dates = useAppStore((s) => s.graph.dates)
  const patchGraph = useAppStore((s) => s.patchGraph)
  const [draft, setDraft] = useState('')
  const today = serverToday ?? todayIso()
  const front = expiries[0] ?? null
  const specs = dates ?? []

  const setDates = (next: string[] | null): void => patchGraph({ dates: next })

  const addSpec = (spec: string): void => {
    // Materialising the server preset rebuilds it from the server's own line
    // keys — `expiry` stays an expiration line (`expiry:<date>`), priced at
    // settlement, and `t+N` stays N days out. Rebuilt from calendar dates
    // against the browser's day, the default expiration line came back as a
    // T+N line priced at the current clock time on expiry day — a PM leg
    // keeping hours of time value and the kink at the strike gone.
    const base = dates ?? effectiveDateSpecs(lines)
    if (base.includes(spec)) return
    setDates([...base, spec])
  }

  const addFromDate = (value: string): void => {
    if (!value || daysBetween(today, value) < 0) return
    addSpec(dateToSpec(value, expiries, today))
  }

  return (
    <div className="space-y-1">
      <div className="flex flex-wrap items-center gap-1">
        <button
          type="button"
          className="ws-btn"
          aria-pressed={dates === null}
          onClick={() => setDates(null)}
          title="T+0, quarter, half, three-quarter and DTE-1 of the front expiry, plus the expiration line"
        >
          ONE preset
        </button>
        <button
          type="button"
          className="ws-btn"
          onClick={() => setDates(presetDates(frontDte, front))}
          title="Materialise the preset so individual dates can be edited"
        >
          edit preset
        </button>
        <button
          type="button"
          className="ws-btn"
          onClick={() => setDates(evenLadder(frontDte, 5, front))}
          title="Five evenly spaced T+x lines out to the front expiry, plus its expiration line"
        >
          even ×5
        </button>
        <button
          type="button"
          className="ws-btn"
          onClick={() => setDates(['today', ...expiries.map((e) => `expiry:${e}`)])}
          title="T+0 plus every expiry in the selected set"
        >
          all expiries
        </button>
      </div>

      <ul className="m-0 flex flex-wrap gap-1 p-0">
        {specs.map((spec) => (
          <li key={spec} className="flex items-center gap-1 rounded border border-line bg-surface-2 px-1.5 py-px">
            <span className="num text-[11px]">{dateSpecLabel(spec)}</span>
            <span className="text-[10px] text-faint">{formatDate(dateSpecDate(spec, today))}</span>
            <button
              type="button"
              className="bg-transparent text-muted hover:text-neg"
              aria-label={`Remove ${dateSpecLabel(spec)}`}
              onClick={() => setDates(specs.filter((s) => s !== spec))}
            >
              ×
            </button>
          </li>
        ))}
        {specs.length === 0 ? (
          <li className="text-[11px] text-muted">
            Server preset in use ({lines.length} lines). “edit preset” to change it.
          </li>
        ) : null}
      </ul>

      <div className="flex flex-wrap items-center gap-1">
        <input
          className="ws-input"
          type="date"
          value={draft}
          aria-label="Add a date"
          min={today}
          onChange={(event) => setDraft(event.target.value)}
        />
        <button
          type="button"
          className="ws-btn"
          disabled={!draft}
          onClick={() => {
            addFromDate(draft)
            setDraft('')
          }}
        >
          add date
        </button>
        {expiries.map((expiry) => (
          <button
            key={expiry}
            type="button"
            className="ws-btn"
            onClick={() => addSpec(`expiry:${expiry}`)}
            title={`Add the ${formatDate(expiry)} expiration line`}
          >
            + EXP {formatExpiryShort(expiry)}
          </button>
        ))}
      </div>
    </div>
  )
}
