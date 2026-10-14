/** Expanded trade row: netted legs, quarantined orders and warnings. */

import type { Leg, Trade, Warning } from '../api/types'
import { formatExpiry } from '../lib/dates'
import { fmtLegGreek, fmtPrice, fmtQty, fmtRatioPct } from '../lib/format'
import { COLUMN_UNITS, GREEK_UNITS, XSP_EXPOSURE_NOTE } from './units'
import { InfoTip, Pill, Tooltip } from './ui'

function contractLabel(leg: Leg): string {
  const c = leg.contract
  return `${c.root} ${formatExpiry(c.expiry)} ${c.strike} ${c.right.toUpperCase()}`
}

const GREEK_KEYS = ['delta', 'gamma', 'theta', 'vega'] as const

const SEVERITY_TONE = { error: 'neg', warn: 'warn', info: 'info' } as const

export function WarningRow({ warning }: { warning: Warning }) {
  return (
    <li className="flex items-start gap-2 py-0.5">
      <Pill tone={SEVERITY_TONE[warning.severity]}>{warning.severity}</Pill>
      <span className="num shrink-0 text-[11px] text-muted">{warning.code}</span>
      <span className="text-[11px] text-fg">{warning.message}</span>
      {warning.contract ? (
        <span className="num shrink-0 text-[11px] text-faint">{warning.contract}</span>
      ) : null}
    </li>
  )
}

function LegsTable({ legs }: { legs: Leg[] }) {
  if (legs.length === 0) {
    return <p className="px-2 py-1 text-muted">No open legs — every contract in this trade is closed or expired.</p>
  }
  return (
    <table className="w-full border-collapse text-[11px]">
      <thead>
        <tr className="text-left text-muted">
          <th className="px-2 py-1 font-medium">Qty</th>
          <th className="px-2 py-1 font-medium">Contract</th>
          <th className="px-2 py-1 text-right font-medium">
            <Tooltip content={COLUMN_UNITS.dte} align="center">
              <span className="cursor-help">DTE</span>
            </Tooltip>
          </th>
          <th className="px-2 py-1 text-right font-medium">Avg cost</th>
          <th className="px-2 py-1 text-right font-medium">Mark</th>
          <th className="px-2 py-1 text-right font-medium">Bid</th>
          <th className="px-2 py-1 text-right font-medium">Ask</th>
          <th className="px-2 py-1 text-right font-medium">
            IV
            <InfoTip content={COLUMN_UNITS.iv} />
          </th>
          <th className="px-2 py-1 text-right font-medium">
            Δ<InfoTip content={`Per contract. ${GREEK_UNITS.delta}`} />
          </th>
          <th className="px-2 py-1 text-right font-medium">Γ</th>
          <th className="px-2 py-1 text-right font-medium">Θ</th>
          <th className="px-2 py-1 text-right font-medium">V</th>
          <th className="px-2 py-1 font-medium">Quote</th>
        </tr>
      </thead>
      <tbody>
        {legs.map((leg) => (
          <tr key={leg.contract.osi} className="border-t border-line/60">
            <td className={`num px-2 py-0.5 ${leg.qty < 0 ? 'text-neg' : 'text-pos'}`}>
              {fmtQty(leg.qty)}
            </td>
            <td className="px-2 py-0.5">
              <Tooltip content={`OSI ${leg.contract.osi} · TOS ${leg.contract.tos_code} · settlement ${leg.contract.settlement.toUpperCase()} · multiplier ${leg.contract.multiplier}`}>
                <span className="num cursor-help text-fg">{contractLabel(leg)}</span>
              </Tooltip>
            </td>
            <td className="num px-2 py-0.5 text-right">{leg.dte}</td>
            <td className="num px-2 py-0.5 text-right">{fmtPrice(leg.avg_cost)}</td>
            <td className="num px-2 py-0.5 text-right text-fg-strong">{fmtPrice(leg.mark)}</td>
            <td className="num px-2 py-0.5 text-right text-muted">{fmtPrice(leg.bid)}</td>
            <td className="num px-2 py-0.5 text-right text-muted">{fmtPrice(leg.ask)}</td>
            <td className="num px-2 py-0.5 text-right">{fmtRatioPct(leg.iv, 1)}</td>
            {GREEK_KEYS.map((key) => {
              const xsp = leg.contract.underlying === 'XSP'
              return (
                <td
                  key={key}
                  className={`num px-2 py-0.5 text-right${xsp ? ' cursor-help' : ''}`}
                  title={xsp ? XSP_EXPOSURE_NOTE : undefined}
                  data-xsp-exposure={xsp ? '' : undefined}
                >
                  {fmtLegGreek(key, leg.greeks[key], xsp)}
                </td>
              )
            })}
            <td className="px-2 py-0.5 text-muted">{leg.quote_source ?? '—'}</td>
          </tr>
        ))}
      </tbody>
    </table>
  )
}

const QUARANTINE_HINT =
  'These orders duplicate executions already claimed by another trade, so they are excluded from legs, cash flow and reconciliation. ' +
  'To fix: add an explicit hint comment to the order line in the trade file — `# ref=9000000000001` (the execution ref) or `# 2026-08-28` (the fill date) — so the matcher can tell the copies apart. ' +
  'If the order never actually filled, delete the line.'

export function TradeDetail({ trade }: { trade: Trade }) {
  return (
    <div className="border-t border-line bg-surface-2/60 px-2 py-2">
      <div className="mb-1 flex items-center gap-2">
        <span className="ws-label">Netted legs</span>
        <span className="text-[11px] text-muted">{trade.structure}</span>
      </div>
      <LegsTable legs={trade.legs} />

      {trade.quarantined.length > 0 ? (
        <div className="mt-2">
          <div className="mb-1 flex items-center gap-2">
            <Pill tone="neg">quarantined</Pill>
            <span className="ws-label">{trade.quarantined.length} orders excluded</span>
            <InfoTip content={QUARANTINE_HINT} />
          </div>
          <ul className="m-0 list-none space-y-0.5 p-0">
            {trade.quarantined.map((order) => (
              <li key={order.raw} className="rounded border border-neg/30 bg-neg/5 px-2 py-1">
                <code className="num block text-[11px] break-all text-fg">{order.raw}</code>
                <span className="text-[10px] text-muted">
                  {order.reason}
                  {order.claimed_by ? ` — already claimed by ${order.claimed_by}` : ''}
                </span>
              </li>
            ))}
          </ul>
          <p className="mt-1 mb-0 text-[10px] text-muted">{QUARANTINE_HINT}</p>
        </div>
      ) : null}

      {trade.warnings.length > 0 ? (
        <div className="mt-2">
          <span className="ws-label">Warnings</span>
          <ul className="m-0 list-none p-0">
            {trade.warnings.map((warning) => (
              <WarningRow key={`${warning.code}-${warning.message}`} warning={warning} />
            ))}
          </ul>
        </div>
      ) : null}
    </div>
  )
}
