/** Top bar: provider truthfulness, the tape, staleness, reload and theme. */

import { useSnapshot, useReload } from '../api/queries'
import type { StreamStatus } from '../api/events'
import { fmtAge, fmtClock, fmtMoney, fmtNumber, pnlColorClass } from '../lib/format'
import { providerPill, snapshotAgeSeconds } from '../lib/provider'
import { useNow } from '../lib/useNow'
import { getMockMode } from '../api/client'
import { useAppStore } from '../store/appStore'
import { ClearTradesControl } from './ClearTradesControl'
import { TradesRootControl } from './TradesRootControl'
import { Pill, Spinner, Tooltip } from './ui'

const STREAM_LABEL: Record<StreamStatus, string> = {
  connecting: 'SSE connecting…',
  open: 'SSE live',
  closed: 'SSE offline — falling back to polling',
  mock: 'SSE simulated by the mock provider',
}

const STREAM_COLOR: Record<StreamStatus, string> = {
  connecting: 'var(--ws-warn)',
  open: 'var(--ws-pos)',
  closed: 'var(--ws-faint)',
  mock: 'var(--ws-info)',
}

export function StatusBar({ streamStatus }: { streamStatus: StreamStatus }) {
  const now = useNow()
  const snapshot = useSnapshot()
  const reload = useReload()
  const theme = useAppStore((s) => s.theme)
  const toggleTheme = useAppStore((s) => s.toggleTheme)

  const pill = providerPill(snapshot.data, now)
  const age = snapshotAgeSeconds(snapshot.data, now)
  const spot = snapshot.data?.spot ?? null
  const changePct =
    spot && spot.last - spot.change !== 0 ? (spot.change / (spot.last - spot.change)) * 100 : null
  const mockMode = getMockMode()

  const reasons =
    pill.reasons.length > 0 ? (
      <ul className="m-0 list-disc space-y-0.5 pl-4">
        {pill.reasons.map((reason) => (
          <li key={reason}>{reason}</li>
        ))}
      </ul>
    ) : (
      'All configured providers are healthy.'
    )

  return (
    <header className="flex shrink-0 flex-wrap items-center gap-x-4 gap-y-1 border-b border-line bg-surface px-3 py-1.5">
      <div className="flex items-center gap-2">
        <span className="text-[13px] font-bold tracking-[0.18em] text-fg-strong">WINGSPAN</span>
        <Tooltip
          content={
            <div className="space-y-1">
              <div className="font-semibold">
                spot: {pill.source} · options: {pill.source}
              </div>
              {reasons}
              <div className="text-muted">
                The pill always names the source actually used, never the one configured.
              </div>
            </div>
          }
        >
          <Pill tone={pill.tone} className="cursor-help">
            {pill.label}
          </Pill>
        </Tooltip>
        {pill.detail ? (
          <span className="max-w-[28rem] truncate text-[11px] text-muted" title={pill.detail}>
            ({pill.detail})
          </span>
        ) : null}
        {mockMode !== 'off' ? (
          <Pill
            tone="info"
            title={
              mockMode === 'forced'
                ? 'VITE_MOCK=1 — every response comes from src/api/mock.ts'
                : 'The API was unreachable, so the SPA fell back to src/api/mock.ts'
            }
          >
            mock {mockMode === 'fallback' ? 'fallback' : ''}
          </Pill>
        ) : null}
        <TradesRootControl />
        <ClearTradesControl />
      </div>

      <div className="flex items-baseline gap-2">
        <span className="ws-label">SPX</span>
        {spot ? (
          <>
            <span className="num text-[15px] font-semibold text-fg-strong">
              {fmtNumber(spot.last, 2)}
            </span>
            <span className={`num text-[12px] ${pnlColorClass(spot.change)}`}>
              {fmtMoney(spot.change, { dp: 2, signed: true, currency: false })}
              {changePct === null ? '' : ` (${changePct > 0 ? '+' : ''}${changePct.toFixed(2)}%)`}
            </span>
          </>
        ) : snapshot.isLoading ? (
          <Spinner label="" />
        ) : (
          <span className="text-muted">—</span>
        )}
      </div>

      <div className="ml-auto flex items-center gap-3">
        <Tooltip
          align="right"
          content={`Snapshot taken ${fmtClock(snapshot.data?.as_of)} local. Quote count: ${
            snapshot.data?.quote_count ?? 0
          }. Market ${snapshot.data?.market_open ? 'open' : 'closed'}.`}
        >
          <span className="cursor-help text-[11px] text-muted">
            as of <span className="num text-fg">{fmtClock(snapshot.data?.as_of)}</span>
            <span className="num ml-1 text-faint">({fmtAge(age)})</span>
          </span>
        </Tooltip>

        <Tooltip align="right" content={STREAM_LABEL[streamStatus]}>
          <span
            className="inline-block h-2 w-2 rounded-full"
            style={{ background: STREAM_COLOR[streamStatus] }}
            role="img"
            aria-label={STREAM_LABEL[streamStatus]}
          />
        </Tooltip>

        <button
          type="button"
          className="ws-btn"
          onClick={() => reload.mutate()}
          disabled={reload.isPending}
          title="Re-scan the trades directory and refresh every query"
        >
          {reload.isPending ? '↻ reloading…' : '↻ reload'}
        </button>

        <button
          type="button"
          className="ws-btn"
          onClick={toggleTheme}
          title={`Switch to the ${theme === 'dark' ? 'light' : 'dark'} theme`}
          aria-label={`Switch to the ${theme === 'dark' ? 'light' : 'dark'} theme`}
        >
          {theme === 'dark' ? '☾ dark' : '☀ light'}
        </button>
      </div>
    </header>
  )
}
