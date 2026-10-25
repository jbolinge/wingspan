/**
 * The trade setups panel, above the model ticket: one small button per
 * named setup (`lib/setups.ts` — the M200 CD and CDS tranches, the STT). A click
 * fetches the chains the setup's expiry slots need — under the query keys
 * and the strike scope the chain on screen uses, so an open chain is not
 * fetched twice — fits each leg to the chain by delta, and makes the fit
 * the model — replacing whatever was modeled — then opens the chain of the
 * first leg's expiration so the quantities are on screen. The ticket beside
 * it prices the legs, draws them on the risk graph and writes them as
 * thinkorswim orders. A leg fitted to the first or last strike of a windowed
 * chain may have been stopped by the window, not the delta: that slot is
 * fetched whole and fitted again before the fit is believed.
 *
 * What was fitted, and anything the plan would frown at (an expiration far
 * from its target, a leg's delta off, the net delta outside the bound), is
 * said under the buttons; the model is set either way, so the trader can
 * nudge a strike in the chain rather than start over.
 */

import { useQueryClient } from '@tanstack/react-query'
import { useEffect, useRef, useState } from 'react'
import { errorDetail } from '../../api/client'
import { optionChainQuery } from '../../api/queries'
import type { ChainExpiry, ChainScope, ChainUnderlying, OptionChainResponse } from '../../api/types'
import {
  SETUPS,
  fitLines,
  fitOpensSection,
  fitSetup,
  fmtNetDelta,
  pickSetupExpiries,
  type SetupFit,
  type TradeSetup,
} from '../../lib/setups'
import { useAppStore } from '../../store/appStore'

type Status =
  | { state: 'idle' }
  | { state: 'fitting'; setup: TradeSetup }
  | { state: 'done'; fit: SetupFit }
  | { state: 'failed'; setup: TradeSetup; reason: string }

export interface SetupsPanelProps {
  /** The expirations the chain on screen offers (under XSP, the PM ones). */
  expirations: readonly ChainExpiry[]
  underlying: ChainUnderlying
  /** The strike scope the chain on screen is fetched under (`all` while
   *  All strikes is pressed): the fit asks for the same, so the open chain
   *  is reused rather than fetched again at the other scope. */
  scope: ChainScope
  /** The listing has loaded; until then the buttons wait. */
  ready: boolean
}

export function SetupsPanel({ expirations, underlying, scope, ready }: SetupsPanelProps) {
  const client = useQueryClient()
  const setModelLegs = useAppStore((s) => s.setModelLegs)
  const setModelExpiry = useAppStore((s) => s.setModelExpiry)
  const [status, setStatus] = useState<Status>({ state: 'idle' })
  // A fit that lands after the panel is gone must not touch its state.
  const mounted = useRef(true)
  useEffect(() => {
    mounted.current = true
    return () => {
      mounted.current = false
    }
  }, [])

  const apply = async (setup: TradeSetup): Promise<void> => {
    const picked = pickSetupExpiries(setup, expirations)
    if (!picked.ok) {
      setStatus({ state: 'failed', setup, reason: picked.reason })
      return
    }
    setStatus({ state: 'fitting', setup })
    const { slots, warnings } = picked.picked
    const fetchChains = async (which: readonly string[], at: ChainScope): Promise<Record<string, OptionChainResponse>> => {
      const chains: Record<string, OptionChainResponse> = {}
      await Promise.all(
        which.map(async (slot) => {
          const expiry = slots[slot]
          if (expiry === undefined) return
          chains[slot] = await client.fetchQuery({
            ...optionChainQuery(expiry.expiry, expiry.root, at, underlying),
            // A chain fetched a moment ago (the one on screen, say) is the
            // one to fit on; a snapshot event invalidates it anyway, so it
            // is at most one poll old.
            staleTime: 60_000,
          })
        }),
      )
      return chains
    }
    let result: ReturnType<typeof fitSetup>
    try {
      const chains = await fetchChains(Object.keys(slots), scope)
      result = fitSetup(setup, slots, chains, warnings)
      if (scope !== 'all' && result.ok && result.fit.atEdge.length > 0) {
        const whole = await fetchChains(result.fit.atEdge, 'all')
        result = fitSetup(setup, slots, { ...chains, ...whole }, warnings)
      }
    } catch (error) {
      if (mounted.current) setStatus({ state: 'failed', setup, reason: errorDetail(error) })
      return
    }
    if (!result.ok) {
      if (mounted.current) setStatus({ state: 'failed', setup, reason: result.reason })
      return
    }
    // The store is what the click asked for: written even if the trader
    // has gone to another tab meanwhile. Only this panel's own words wait.
    setModelLegs(result.fit.legs.map((fitted) => ({ contract: fitted.quote.contract, qty: fitted.leg.qty })))
    const section = fitOpensSection(result.fit)
    if (section !== null) setModelExpiry(section)
    if (mounted.current) setStatus({ state: 'done', fit: result.fit })
  }

  const busy = status.state === 'fitting'
  return (
    <section aria-label="Trade setups" className="ws-card shrink-0 px-2.5 py-1.5">
      <div className="flex flex-wrap items-center gap-1.5">
        <h2 className="ws-label m-0 mr-0.5 font-semibold">Setups</h2>
        {SETUPS.map((setup) => (
          <button
            key={setup.id}
            type="button"
            className="ws-btn"
            title={`${setup.name}: ${setup.summary}`}
            disabled={!ready || busy}
            onClick={() => void apply(setup)}
          >
            {setup.label}
          </button>
        ))}
      </div>
      {/* One status per outcome, named by the setup, so the trader can tell
          which button the words belong to. Capped and scrolling, so a fit
          with every warning does not push the ticket under it off a short
          window. */}
      {status.state === 'fitting' ? (
        <p role="status" className="m-0 mt-1 text-[10px] text-muted">
          Fitting {status.setup.label} to the chain…
        </p>
      ) : status.state === 'failed' ? (
        <p role="status" className="m-0 mt-1 text-[10px] text-neg">
          {status.setup.label}: {status.reason}
        </p>
      ) : status.state === 'done' ? (
        <div role="status" className="num mt-1 max-h-24 space-y-0.5 overflow-y-auto text-[10px] text-muted">
          <div className="font-semibold text-fg">
            {status.fit.setup.label} · net Δ {fmtNetDelta(status.fit.netDelta)}
          </div>
          {/* Wrapped, not truncated: in the 18rem rail the strikes are the
              point of the line, and they come after the date. */}
          {fitLines(status.fit).map((line) => (
            <div key={line} className="break-words">
              {line}
            </div>
          ))}
          {status.fit.warnings.map((warning) => (
            <div key={warning} className="break-words text-warn">
              <span aria-hidden="true">⚠ </span>
              {warning}
            </div>
          ))}
        </div>
      ) : null}
    </section>
  )
}
