/** Data-health tab: warnings (with fix hints), reconciliation, provider
 *  diagnostics and the resolved config. Everything the trader needs to decide
 *  whether to trust the numbers on the other tabs. */

import { useMemo, useState } from 'react'
import {
  useConfig,
  useReconciliation,
  useSnapshot,
  useTradeSource,
  useWarnings,
} from '../api/queries'
import type { ReconciliationStatus, TradeSourceKind, Warning, WarningSeverity } from '../api/types'
import { fmtClock, fmtNumber } from '../lib/format'
import { providerPill, sourceLabel } from '../lib/provider'
import { useNow } from '../lib/useNow'
import { partitionWarnings, warningKey } from '../lib/warnings'
import { useAppStore } from '../store/appStore'
import { DiagnosticsCard } from './DiagnosticsCard'
import { SettingsCard } from './SettingsCard'
import { Card, EmptyState, ErrorNote, Pill, Spinner, ToggleButton, Tooltip } from './ui'

const SEVERITY_TONE: Record<WarningSeverity, 'neg' | 'warn' | 'info'> = {
  error: 'neg',
  warn: 'warn',
  info: 'info',
}

const STATUS_TONE: Record<ReconciliationStatus, 'pos' | 'neg' | 'warn' | 'info'> = {
  match: 'pos',
  qty_mismatch: 'neg',
  unassigned: 'warn',
  missing: 'neg',
}

const STATUS_HELP: Record<ReconciliationStatus, string> = {
  match: 'The statement quantity equals the sum across all trades.',
  qty_mismatch:
    'The account nets across trades. A mismatch usually means one trade closed a contract another trade still holds — check the trades that touch this contract.',
  unassigned:
    'The statement holds this contract but no trade file claims it. Reported, never netted into any trade.',
  missing:
    'A trade claims this contract but the latest statement does not list it — the position may have closed or expired.',
}

const SOURCE_LABEL: Record<TradeSourceKind, string> = {
  sheet: 'Google Sheet',
  csv: 'CSV file on this computer',
  config: 'from the config file',
  demo: 'demo book',
  empty: 'nothing imported',
  manual: 'manual entry',
}

/** Fix hints keyed by warning code; shown inline under the message. */
const FIX_HINTS: Record<string, string> = {
  duplicate_execution_claim:
    'Add an explicit hint comment to the duplicated order line in the trade file — `# ref=<execution ref>` or `# YYYY-MM-DD` — so the matcher can tell the copies apart. If the order never filled, delete the line.',
  unmatched_order:
    'No execution matched this order. Only a book assembled from TOS order files raises this; an imported Legs sheet or CSV never does.',
  date_outside_window:
    'The matched execution is not on the file-name date. That is expected when the file name is a non-trading day; if the wrong execution was picked, add `# YYYY-MM-DD` or `# ref=<execution ref>` to pin the order to its fill.',
  unassigned_account_position:
    'Create a trade file for the position, or ignore it — unassigned positions are reported but never netted into a trade.',
  closed_trade_has_open_legs:
    'The file is in “Closed Trades” but legs remain open. Either move it back to “Open Trades” or add the closing order.',
  expired_leg_unknown_settlement:
    'No settlement was recorded for an expired leg, so it was valued at intrinsic against the SPX level recorded for its expiry date (an estimate for an AM leg, or when the app was not running at that day’s close) or, failing that, the current SPX level — or at zero when neither is known. Record the settlement as a Closed row in the Legs sheet.',
  sheet_column_missing:
    'A required column is not in the header. Wingspan needs Trade, Side, Qty and OCC Symbol; add the column (or rename it to one of the accepted spellings) and import again.',
  sheet_column_absent:
    'An expected column is missing or entirely blank. The trades still load, without a cost basis or days-in-trade for the affected legs.',
  sheet_row_skipped:
    'A row could not be read — a blank required cell, an unparseable date, or a contract that is not an SPX or XSP option. Everything else imported normally.',
  sheet_leg_unpriced:
    'The Open Price cell was blank, so this leg contributes quantity but no cost basis.',
  sheet_close_price_missing:
    'The leg has a Closed date but no Close Price, so it was removed from the position without banking anything.',
  sheet_trade_instance_unverified:
    'Two trades share a name and the sheet carries neither a # nor a Key to tell them apart, so they were grouped as one. Add a # column to separate them.',
  sheet_pnl_disagreement:
    'The sheet’s own P/L for this leg differs from Wingspan’s by more than a cent. Wingspan’s number is computed from the prices and fees in the same row.',
  sheet_duplicate_column: 'Two header cells resolve to the same logical column; the first one won.',
  sheet_header_offset:
    'The header row is not the first row — Wingspan found it by scanning the first five records.',
  sheet_value_disagrees:
    'A cross-check column (Status or Expiry) disagrees with the OCC symbol or the dates. The OCC symbol wins.',
}

function WarningItem({ warning }: { warning: Warning }) {
  const hint = FIX_HINTS[warning.code]
  return (
    <li className="border-t border-line/60 px-2 py-1.5 first:border-t-0">
      <div className="flex flex-wrap items-baseline gap-2">
        <Pill tone={SEVERITY_TONE[warning.severity]}>{warning.severity}</Pill>
        <span className="num text-[11px] text-muted">{warning.code}</span>
        {warning.trade_id ? (
          <span className="num text-[11px] text-accent">{warning.trade_id}</span>
        ) : null}
        {warning.contract ? (
          <span className="num text-[11px] text-faint">{warning.contract}</span>
        ) : null}
      </div>
      <p className="m-0 mt-0.5 text-[12px] text-fg">{warning.message}</p>
      {hint ? <p className="m-0 mt-0.5 text-[11px] text-warn">Fix: {hint}</p> : null}
    </li>
  )
}

function TradeSourceCard() {
  const query = useTradeSource()
  const data = query.data
  return (
    <Card title="Trade source">
      {query.isLoading ? (
        <div className="p-3">
          <Spinner />
        </div>
      ) : query.isError ? (
        <ErrorNote error={query.error} onRetry={() => void query.refetch()} />
      ) : (
        <dl className="m-0 grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 px-2.5 py-2 text-[12px]">
          <dt className="ws-label">Source</dt>
          <dd className="num m-0 break-all text-fg">{data?.label ?? '—'}</dd>
          <dt className="ws-label">Kind</dt>
          <dd className="num m-0 text-fg">{data ? SOURCE_LABEL[data.kind] : '—'}</dd>
          <dt className="ws-label">Tab</dt>
          <dd className="num m-0 text-fg">{data?.tab ?? '—'}</dd>
          <dt className="ws-label">Path</dt>
          <dd className="num m-0 break-all text-fg">{data?.path ?? '—'}</dd>
          <dt className="ws-label">Open/Closed</dt>
          <dd className="num m-0 text-fg">
            {data?.open_trades ?? 0} / {data?.closed_trades ?? 0}
          </dd>
          <dt className="ws-label">Legs</dt>
          <dd className="num m-0 text-fg">{data?.legs ?? 0}</dd>
          <dt className="ws-label">Rows skipped</dt>
          <dd className="num m-0 text-fg">{data?.rows_skipped ?? 0}</dd>
          <dt className="ws-label">Fetched</dt>
          <dd className="num m-0 text-fg">{fmtClock(data?.fetched_at)}</dd>
          <dt className="ws-label">Stale</dt>
          <dd className="num m-0 text-fg">{data?.stale ? 'yes' : 'no'}</dd>
          <dt className="ws-label">Auto refresh</dt>
          <dd className="num m-0 text-fg">
            {data?.auto_refresh_secs ? `${data.auto_refresh_secs} seconds` : 'manual reload only'}
          </dd>
          {data?.detail ? (
            <>
              <dt className="ws-label">Detail</dt>
              <dd className="m-0 text-warn">{data.detail}</dd>
            </>
          ) : null}
        </dl>
      )}
    </Card>
  )
}

function ProviderDiagnostics() {
  const now = useNow(1000)
  const snapshot = useSnapshot()
  const pill = providerPill(snapshot.data, now)
  const data = snapshot.data
  return (
    <Card title="Provider diagnostics">
      <div className="space-y-1 px-2 py-2 text-[12px]">
        <div className="flex flex-wrap items-center gap-2">
          <Pill tone={pill.tone}>{pill.label}</Pill>
          {data ? (
            <span className="text-muted">
              spot <span className="num text-fg">{sourceLabel(data.spot_source)}</span> · options{' '}
              <span className="num text-fg">{sourceLabel(data.option_source)}</span> · quotes{' '}
              <span className="num text-fg">{data.quote_count}</span> · market{' '}
              <span className="num text-fg">{data.market_open ? 'open' : 'closed'}</span> · as of{' '}
              <span className="num text-fg">{fmtClock(data.as_of)}</span>
            </span>
          ) : null}
        </div>
        {data?.spot ? (
          <div className="text-muted">
            {data.spot.symbol} <span className="num text-fg">{fmtNumber(data.spot.last, 2)}</span>{' '}
            <span className="num">{fmtNumber(data.spot.change, 2)}</span> at{' '}
            <span className="num">{fmtClock(data.spot.as_of)}</span>
          </div>
        ) : null}
        {pill.reasons.length > 0 ? (
          <ul className="m-0 list-disc space-y-0.5 pl-5 text-warn">
            {pill.reasons.map((reason) => (
              <li key={reason}>{reason}</li>
            ))}
          </ul>
        ) : (
          <p className="m-0 text-pos">All configured providers healthy.</p>
        )}
      </div>
    </Card>
  )
}

function ReconciliationTable() {
  const query = useReconciliation()
  const data = query.data
  const [onlyProblems, setOnlyProblems] = useState(true)
  if (query.isLoading) {
    return (
      <Card title="Reconciliation">
        <div className="p-3">
          <Spinner />
        </div>
      </Card>
    )
  }
  if (query.isError) {
    return (
      <Card title="Reconciliation">
        <ErrorNote error={query.error} onRetry={() => void query.refetch()} />
      </Card>
    )
  }
  const rows = data?.rows ?? []
  const problems = rows.filter((row) => row.status !== 'match')
  const shown = onlyProblems ? problems : rows
  const statementAsOf = data?.statement_as_of ?? null
  const title = statementAsOf === null ? 'Reconciliation' : `Reconciliation — statement ${statementAsOf}`
  return (
    <Card
      title={title}
      actions={
        <>
          <span className="text-[11px] text-muted">
            {rows.length - problems.length}/{rows.length} match
          </span>
          <ToggleButton pressed={onlyProblems} onClick={() => setOnlyProblems(!onlyProblems)}>
            problems only
          </ToggleButton>
        </>
      }
      bodyClassName="overflow-auto"
    >
      <p className="m-0 px-2 py-1 text-[11px] text-muted">{data?.note}</p>
      {shown.length === 0 ? (
        rows.length === 0 && data?.source_note ? (
          <EmptyState>{data.source_note}</EmptyState>
        ) : (
          <EmptyState>Every contract reconciles against the latest statement.</EmptyState>
        )
      ) : (
        <table className="w-full border-collapse text-[12px]">
          <thead>
            <tr className="text-left text-muted">
              <th className="px-2 py-1 font-medium">Contract</th>
              <th className="px-2 py-1 text-right font-medium">Statement qty</th>
              <th className="px-2 py-1 text-right font-medium">Trades qty</th>
              <th className="px-2 py-1 text-right font-medium">Δ</th>
              <th className="px-2 py-1 font-medium">Status</th>
            </tr>
          </thead>
          <tbody>
            {shown.map((row) => (
              <tr key={row.tos_code} className="border-t border-line/60">
                <td className="num px-2 py-0.5">{row.tos_code}</td>
                <td className="num px-2 py-0.5 text-right">{row.statement_qty}</td>
                <td className="num px-2 py-0.5 text-right">{row.trades_qty}</td>
                <td className="num px-2 py-0.5 text-right text-muted">
                  {row.statement_qty - row.trades_qty}
                </td>
                <td className="px-2 py-0.5">
                  <Tooltip content={STATUS_HELP[row.status]}>
                    <Pill tone={STATUS_TONE[row.status]}>{row.status.replace('_', ' ')}</Pill>
                  </Tooltip>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </Card>
  )
}

function ResolvedConfig() {
  const query = useConfig()
  const [open, setOpen] = useState(false)
  return (
    <Card
      title="Resolved config"
      actions={
        <ToggleButton pressed={open} onClick={() => setOpen(!open)}>
          {open ? 'hide' : 'show'}
        </ToggleButton>
      }
    >
      {open ? (
        query.isLoading ? (
          <div className="p-3">
            <Spinner />
          </div>
        ) : query.isError ? (
          <ErrorNote error={query.error} onRetry={() => void query.refetch()} />
        ) : (
          <pre className="num m-0 max-h-96 overflow-auto px-2 py-2 text-[11px] leading-snug text-fg">
            {JSON.stringify(query.data, null, 2)}
          </pre>
        )
      ) : (
        <p className="m-0 px-2 py-2 text-[11px] text-muted">
          Secrets are redacted before this view is built; the account number never appears here.
        </p>
      )}
    </Card>
  )
}

export function DataHealth() {
  const query = useWarnings()
  const [severity, setSeverity] = useState<WarningSeverity | 'all'>('all')
  const collapsed = useAppStore((s) => s.warningsCollapsed)
  const setCollapsed = useAppStore((s) => s.setWarningsCollapsed)
  const dismissed = useAppStore((s) => s.dismissedWarnings)
  const dismissWarnings = useAppStore((s) => s.dismissWarnings)
  const restoreWarnings = useAppStore((s) => s.restoreWarnings)
  const reported = query.data?.warnings
  // "Clear" hides; the server keeps reporting the warning and `hidden` is
  // how many of its current list the trader has cleared, so the restore
  // button says what it would bring back.
  const { visible: warnings, hidden } = useMemo(
    () => partitionWarnings(reported ?? [], dismissed),
    [reported, dismissed],
  )
  const shown = severity === 'all' ? warnings : warnings.filter((w) => w.severity === severity)
  const counts = {
    error: warnings.filter((w) => w.severity === 'error').length,
    warn: warnings.filter((w) => w.severity === 'warn').length,
    info: warnings.filter((w) => w.severity === 'info').length,
  }

  return (
    <div className="flex min-h-0 flex-1 flex-col gap-2 overflow-y-auto">
      <Card
        title="Warnings"
        className="shrink-0"
        collapsed={collapsed}
        onToggleCollapse={() => setCollapsed(!collapsed)}
        actions={
          <>
            {(['all', 'error', 'warn', 'info'] as const).map((level) => (
              <ToggleButton
                key={level}
                pressed={severity === level}
                onClick={() => setSeverity(level)}
              >
                {level}
                {level === 'all' ? ` ${warnings.length}` : ` ${counts[level]}`}
              </ToggleButton>
            ))}
            <span aria-hidden="true" className="mx-1 h-4 border-l border-line" />
            <button
              type="button"
              className="ws-btn"
              disabled={shown.length === 0}
              title="Hide the listed warnings until you restore them; the files are not touched"
              onClick={() => dismissWarnings(shown.map(warningKey))}
            >
              clear {shown.length}
            </button>
            {hidden.length > 0 ? (
              <button
                type="button"
                className="ws-btn"
                title="Show every cleared warning again"
                onClick={restoreWarnings}
              >
                restore {hidden.length}
              </button>
            ) : null}
          </>
        }
        bodyClassName="overflow-auto"
      >
        {query.isLoading ? (
          <div className="p-3">
            <Spinner />
          </div>
        ) : query.isError ? (
          <ErrorNote error={query.error} onRetry={() => void query.refetch()} />
        ) : shown.length === 0 ? (
          <EmptyState>
            {hidden.length > 0
              ? `Nothing left to show — ${hidden.length} cleared.`
              : 'Nothing to report — every row of the Legs table parsed cleanly.'}
          </EmptyState>
        ) : (
          <ul className="m-0 list-none p-0" data-testid="warnings-list">
            {shown.map((warning) => (
              <WarningItem key={warningKey(warning)} warning={warning} />
            ))}
          </ul>
        )}
      </Card>

      <div className="grid grid-cols-1 items-start gap-2 xl:grid-cols-2">
        <div className="space-y-2">
          <TradeSourceCard />
          <ReconciliationTable />
        </div>
        <div className="space-y-2">
          <ProviderDiagnostics />
          <ResolvedConfig />
          <SettingsCard />
          <DiagnosticsCard />
        </div>
      </div>
    </div>
  )
}
