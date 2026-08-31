/** Every control on the risk graph, in one scrollable column. */

import type { Greeks, IsoDate, RiskGraphResponse, Trade } from '../../api/types'
import { lineLabel, perTradeOverlayCurves } from '../../lib/chartOption'
import { formatExpiryShort } from '../../lib/dates'
import { fmtNumber, fmtRatioPct } from '../../lib/format'
import { chartPalette, perTradeColor, tplusColor } from '../../lib/palette'
import { yExtent } from '../../lib/readout'
import { useAppStore, type Theme } from '../../store/appStore'
import { NumberField, Slider, SwatchLine, ToggleButton, Tooltip } from '../ui'
import { DateEditor } from './DateEditor'
import { WhatIfPanel } from './WhatIfPanel'

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <section className="border-b border-line px-2 py-2 last:border-b-0">
      <h3 className="ws-label mb-1.5 font-semibold">{title}</h3>
      <div className="space-y-1.5">{children}</div>
    </section>
  )
}

const POINT_CHOICES = [121, 241, 481, 961]

export function GraphControls({
  data,
  trades,
  expiries,
  frontDte,
  spot,
  effectiveDates,
  selected,
  theme,
  baseline,
}: {
  data: RiskGraphResponse | undefined
  trades: Trade[]
  expiries: IsoDate[]
  frontDte: number | null
  spot: number | null
  effectiveDates: IsoDate[]
  selected: string[]
  theme: Theme
  /** Current P&L and greeks of the selection, for the what-if delta column. */
  baseline: { pnl: number; greeks: Greeks } | null
}) {
  const graph = useAppStore((s) => s.graph)
  const patchGraph = useAppStore((s) => s.patchGraph)
  const setExpiryVolShift = useAppStore((s) => s.setExpiryVolShift)
  const clearExpiryVolShifts = useAppStore((s) => s.clearExpiryVolShifts)
  const toggleLine = useAppStore((s) => s.toggleLine)
  const resetGraph = useAppStore((s) => s.resetGraph)
  const palette = chartPalette(theme)

  const lines = data?.lines ?? []
  const today = data?.as_of.slice(0, 10) ?? ''
  const visible = lines.filter((l) => !graph.hiddenLines.includes(l.date))
  const sortedLines = [...lines].sort((a, b) => a.date.localeCompare(b.date))
  const tplusLines = sortedLines.filter((l) => l.kind === 'tplus')
  const tplusRank = new Map(tplusLines.map((line, index) => [line.date, index]))

  const toggleYLock = (): void => {
    if (graph.yLock) {
      patchGraph({ yLock: false, yMin: null, yMax: null })
      return
    }
    const extent = yExtent(visible)
    patchGraph({ yLock: true, yMin: extent?.min ?? null, yMax: extent?.max ?? null })
  }

  return (
    <div className="flex h-full min-h-0 flex-col overflow-y-auto">
      <Section title="Price range">
        <div className="flex flex-wrap items-center gap-1">
          {(['auto', 'pct', 'abs'] as const).map((mode) => (
            <ToggleButton
              key={mode}
              pressed={graph.rangeMode === mode}
              onClick={() => patchGraph({ rangeMode: mode })}
              title={
                mode === 'auto'
                  ? 'Strikes ±5% unioned with spot ±10% — the server default'
                  : mode === 'pct'
                    ? 'A percentage band around spot'
                    : 'Absolute price bounds'
              }
            >
              {mode === 'pct' ? '%' : mode}
            </ToggleButton>
          ))}
        </div>
        {graph.rangeMode === 'pct' ? (
          <Slider
            label="±"
            value={graph.rangePct}
            min={0.02}
            max={0.5}
            step={0.01}
            onChange={(value) => patchGraph({ rangePct: value })}
            format={(value) => `${(value * 100).toFixed(0)}%`}
          />
        ) : null}
        {graph.rangeMode === 'abs' ? (
          <div className="flex flex-wrap gap-1">
            <NumberField
              label="min"
              value={graph.rangeMin}
              step={25}
              onCommit={(value) => patchGraph({ rangeMin: value })}
              placeholder={spot ? String(Math.round(spot * 0.9)) : ''}
            />
            <NumberField
              label="max"
              value={graph.rangeMax}
              step={25}
              onCommit={(value) => patchGraph({ rangeMax: value })}
              placeholder={spot ? String(Math.round(spot * 1.1)) : ''}
            />
          </div>
        ) : null}
        <div className="flex flex-wrap items-center gap-2">
          <NumberField
            label="spot override"
            value={graph.spotOverride}
            step={5}
            width="6rem"
            placeholder={spot ? String(Math.round(spot)) : 'live'}
            onCommit={(value) => patchGraph({ spotOverride: value })}
          />
          <label className="flex items-center gap-1">
            <span className="ws-label">points</span>
            <select
              className="ws-input"
              value={graph.points}
              aria-label="Curve resolution"
              onChange={(event) => patchGraph({ points: Number(event.target.value) })}
            >
              {POINT_CHOICES.map((choice) => (
                <option key={choice} value={choice}>
                  {choice}
                </option>
              ))}
            </select>
          </label>
        </div>
      </Section>

      <Section title="Y axis">
        <div className="flex flex-wrap items-center gap-1">
          <ToggleButton
            pressed={graph.yLock}
            onClick={toggleYLock}
            title="Freeze the vertical scale so the curves stop rescaling as the tape moves"
          >
            lock y-axis
          </ToggleButton>
          {graph.yLock ? (
            <span className="num text-[10px] text-muted">
              {fmtNumber(graph.yMin, 0)} … {fmtNumber(graph.yMax, 0)}
            </span>
          ) : null}
        </div>
      </Section>

      <Section title="Volatility">
        <Slider
          label="global"
          value={graph.volShift}
          min={-0.1}
          max={0.1}
          step={0.0025}
          onChange={(value) => patchGraph({ volShift: value })}
          format={(value) => `${value >= 0 ? '+' : ''}${(value * 100).toFixed(2)} vp`}
        />
        {expiries.map((expiry) => (
          <Slider
            key={expiry}
            label={formatExpiryShort(expiry)}
            value={graph.volShiftByExpiry[expiry] ?? 0}
            min={-0.1}
            max={0.1}
            step={0.0025}
            onChange={(value) => setExpiryVolShift(expiry, value)}
            format={(value) => `${value >= 0 ? '+' : ''}${(value * 100).toFixed(2)} vp`}
          />
        ))}
        <div className="flex items-center gap-1">
          <button type="button" className="ws-btn" onClick={clearExpiryVolShifts}>
            reset per-expiry
          </button>
          <Tooltip content="A vol point is 0.01 of decimal vol. Shifts are applied sticky-strike on top of the IVs Wingspan solved from the marks.">
            <span className="cursor-help text-[10px] text-muted">what is a vol point?</span>
          </Tooltip>
        </div>
      </Section>

      <Section title="Dates">
        <DateEditor expiries={expiries} frontDte={frontDte} effectiveDates={effectiveDates} />
      </Section>

      <Section title="Lines">
        <ul className="m-0 list-none space-y-0.5 p-0">
          {sortedLines.map((line) => {
              const color =
                line.kind === 't0'
                  ? palette.t0
                  : line.kind === 'tplus'
                    ? tplusColor(palette, tplusRank.get(line.date) ?? 0, tplusLines.length)
                    : palette.expiry
              const hidden = graph.hiddenLines.includes(line.date)
              return (
                <li key={line.date}>
                  <label className="flex items-center gap-1.5">
                    <input
                      type="checkbox"
                      checked={!hidden}
                      aria-label={`Show ${lineLabel(line, today)}`}
                      onChange={() => toggleLine(line.date)}
                    />
                    <SwatchLine color={color} dashed={line.kind === 'tplus'} />
                    <span className="num text-[11px]">{lineLabel(line, today)}</span>
                  </label>
                </li>
              )
          })}
          {lines.length === 0 ? <li className="text-[11px] text-muted">No curves yet.</li> : null}
        </ul>

        <div className="flex flex-wrap gap-1 pt-1">
          <ToggleButton
            pressed={graph.perTradeOverlay}
            onClick={() => patchGraph({ perTradeOverlay: !graph.perTradeOverlay })}
            title="Draw each selected trade's own T+0 curve underneath the aggregate"
          >
            per-trade overlay
          </ToggleButton>
          <ToggleButton
            pressed={graph.includeRealized}
            onClick={() => patchGraph({ includeRealized: !graph.includeRealized })}
            title="ONE-style: include realised P&L and fees so the T+0 line at spot equals the dashboard total P&L"
          >
            include realised
          </ToggleButton>
          <ToggleButton
            pressed={graph.showSigmaBands}
            onClick={() => patchGraph({ showSigmaBands: !graph.showSigmaBands })}
            title="±1σ and ±2σ price bands from the front ATM IV"
          >
            σ bands
          </ToggleButton>
          <ToggleButton
            pressed={graph.showBreakevens}
            onClick={() => patchGraph({ showBreakevens: !graph.showBreakevens })}
          >
            breakevens
          </ToggleButton>
          <ToggleButton
            pressed={graph.showSliceTable}
            onClick={() => patchGraph({ showSliceTable: !graph.showSliceTable })}
          >
            price slices
          </ToggleButton>
        </div>

        {graph.perTradeOverlay ? (
          <ul className="m-0 flex list-none flex-wrap gap-2 p-0 pt-1">
            {perTradeOverlayCurves(data).map((series, index) => (
              <li key={series.trade_id} className="flex items-center gap-1">
                <SwatchLine color={perTradeColor(palette, index)} />
                <span className="num text-[10px]">
                  {trades.find((t) => t.id === series.trade_id)?.name ?? series.trade_id}
                </span>
              </li>
            ))}
          </ul>
        ) : null}
      </Section>

      <Section title="What-if">
        <WhatIfPanel selected={selected} spot={spot} baseline={baseline} />
      </Section>

      <Section title="Reset">
        <button type="button" className="ws-btn" onClick={resetGraph}>
          restore graph defaults
        </button>
        {data ? (
          <p className="m-0 text-[10px] text-muted">
            Front ATM IV {fmtRatioPct(data.front_atm_iv, 2)} · ±1σ {fmtNumber(data.sigma_bands.one[0], 0)}–
            {fmtNumber(data.sigma_bands.one[1], 0)} · ±2σ {fmtNumber(data.sigma_bands.two[0], 0)}–
            {fmtNumber(data.sigma_bands.two[1], 0)}
          </p>
        ) : null}
      </Section>
    </div>
  )
}
