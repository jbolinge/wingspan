/**
 * The Model tab: the option chain with selectable expirations, ONE-style,
 * where a quantity typed into a Model cell becomes a modeled leg drawn on the
 * Positions tab's risk graph.
 *
 * Top to bottom: a toolbar (whose trades the Pos column counts, the strike
 * window, refresh, and the model's summary and actions), the expiration
 * strip, then the chain sections — stacked, scrolling inside the tab — with
 * the model ticket beside them on a wide window or above them on a narrow one.
 *
 * The chain opens on the money: the topmost section's spot line is scrolled
 * to the middle of the view when the tab is displayed, and again whenever a
 * different section becomes the topmost — never on a refetch or an edit,
 * which would move the rows under the trader's cursor.
 */

import { useQueryClient } from '@tanstack/react-query'
import { useCallback, useLayoutEffect, useMemo, useRef, useState } from 'react'
import { errorSlug } from '../../api/client'
import {
  CHAIN_KEYS,
  useChainExpirations,
  useOptionChains,
  useSnapshot,
} from '../../api/queries'
import type { OptionChainResponse, OptionContract, Trade } from '../../api/types'
import { tradesById } from '../../lib/aggregate'
import { formatExpiry } from '../../lib/dates'
import { fmtAge, fmtClock } from '../../lib/format'
import {
  autoExpiries,
  centredScrollTop,
  chainMarks,
  effectiveTradeIds,
  expiryKey,
  fmtNetCost,
  modelNetCost,
  modelQtyByContract,
  parseExpiryKey,
  positionByContract,
  positionExpiryKeys,
} from '../../lib/model'
import { sourceLabel } from '../../lib/provider'
import { useNow } from '../../lib/useNow'
import { MAX_MODEL_SECTIONS, useAppStore, type StrikeCount } from '../../store/appStore'
import { ErrorNote, Pill, Spinner, ToggleButton, Tooltip } from '../ui'
import { ChainSection } from './ChainTable'
import { ExpiryStrip } from './ExpiryStrip'
import { ModelTicket } from './ModelTicket'

const STRIKE_CHOICES: readonly StrikeCount[] = [16, 32, 64, null]

/**
 * Scroll `container` so section `key`'s spot row sits in its vertical middle;
 * false when that section has no spot row on screen (still loading, or no
 * strikes).
 *
 * Measured with bounding rects rather than `offsetTop`, whose reference for a
 * table row is the table, not the scroll container; and applied as the
 * container's own `scrollTop`, never `scrollIntoView`, which would scroll
 * every scrollable ancestor — the whole page — along with it.
 */
function centreSpotRow(container: HTMLElement, key: string): boolean {
  const row = Array.from(container.querySelectorAll<HTMLElement>('[data-testid="spot-row"]')).find(
    (el) => el.dataset.section === key,
  )
  if (!row) return false
  const rowRect = row.getBoundingClientRect()
  const rowTop = rowRect.top - container.getBoundingClientRect().top + container.scrollTop
  container.scrollTop = centredScrollTop(rowTop, rowRect.height, container.clientHeight)
  return true
}

/**
 * The age of the oldest chain on screen, ticking — its own component so the
 * once-a-second re-render stops here instead of repainting every chain row.
 */
function ChainAge({
  asOf,
  source,
  stale,
  degraded,
}: {
  asOf: string | null
  source: string | null
  stale: boolean
  degraded: string[]
}) {
  const now = useNow()
  if (asOf === null) return null
  const age = Math.max(0, (now.getTime() - new Date(asOf).getTime()) / 1000)
  return (
    <Tooltip
      content={
        <div className="space-y-1">
          <div className="font-semibold">
            chain from {source ?? 'unknown'} · fetched {fmtClock(asOf)} local
          </div>
          {stale ? (
            <div className="text-warn">
              Served from the in-memory cache: the last refetch failed.
            </div>
          ) : null}
          {degraded.length > 0 ? (
            <ul className="m-0 list-disc space-y-0.5 pl-4">
              {degraded.map((reason) => (
                <li key={reason}>{reason}</li>
              ))}
            </ul>
          ) : (
            <div>Every configured provider answered.</div>
          )}
          <div className="text-muted">IVs and greeks are Wingspan&apos;s own, solved off these quotes.</div>
        </div>
      }
    >
      <Pill tone={stale ? 'warn' : degraded.length > 0 ? 'info' : 'muted'} className="cursor-help">
        {stale ? 'stale · ' : ''}
        {source ?? 'chain'} · {fmtAge(age)}
      </Pill>
    </Tooltip>
  )
}

export function ModelTab({ trades }: { trades: Trade[] }) {
  const client = useQueryClient()
  const selected = useAppStore((s) => s.selected)
  const model = useAppStore((s) => s.model)
  const setTab = useAppStore((s) => s.setTab)
  const setModelQty = useAppStore((s) => s.setModelQty)
  const clearModel = useAppStore((s) => s.clearModel)
  const setModelStrikeCount = useAppStore((s) => s.setModelStrikeCount)
  const snapshot = useSnapshot()
  const listing = useChainExpirations()
  const [hint, setHint] = useState<string | null>(null)

  // `trades` is a fresh array on every poll; the Pos map is rebuilt only when
  // the quantities it would hold actually change, so the (memoised) chain
  // tables do not repaint on a poll that changed nothing.
  const ids = effectiveTradeIds(selected, trades)
  const positionsKey = JSON.stringify([...positionByContract(trades, ids)])
  const positions = useMemo(
    () => new Map<string, number>(JSON.parse(positionsKey) as [string, number][]),
    [positionsKey],
  )
  const modelQty = useMemo(() => modelQtyByContract(model.legs), [model.legs])

  const expirations = useMemo(() => listing.data?.expirations ?? [], [listing.data])
  const listed = useMemo(() => new Set(expirations.map(expiryKey)), [expirations])
  const auto = useMemo(
    () => autoExpiries(expirations, positions, model.legs, MAX_MODEL_SECTIONS),
    [expirations, positions, model.legs],
  )
  // A stored section that has since expired is not listed any more; once none
  // of the stored ones is, the automatic set takes over again.
  const stored = model.expiries.filter((key) => listed.has(key))
  const sectionKeys = stored.length > 0 ? stored : auto
  const sections = expirations.filter((e) => sectionKeys.includes(expiryKey(e)))

  // Every chain the tab needs: the sections on screen, plus the expiries the
  // modeled legs sit in, so the ticket has a mark for every leg.
  const chainKeys = [
    ...new Set([
      ...sections.map(expiryKey),
      ...model.legs.map((leg) => expiryKey(leg.contract)).filter((key) => listed.has(key)),
    ]),
  ]
  const chainQueries = useOptionChains(
    chainKeys.map((key) => parseExpiryKey(key)).filter((p): p is NonNullable<typeof p> => p !== null),
  )
  const queryByKey = new Map(chainKeys.map((key, index) => [key, chainQueries[index]]))
  const marks = chainMarks(chainQueries.map((query) => query.data))

  const heldKeys = useMemo(() => positionExpiryKeys(positions), [positions])
  const modeled = useMemo(() => new Set(model.legs.map((leg) => expiryKey(leg.contract))), [model.legs])

  const onToggle = useCallback(
    (key: string) => {
      const before = sectionKeys
      if (before.length === 1 && before[0] === key) {
        setHint('At least one expiration stays open — pick another first.')
        return
      }
      const store = useAppStore.getState()
      // The first click materialises the automatic set (and drops any
      // expired key), so the toggle acts on what is actually on screen.
      store.setModelExpiries(before)
      store.toggleModelExpiry(key)
      const after = useAppStore.getState().model.expiries
      const dropped = before.find((k) => k !== key && !after.includes(k))
      const parsed = dropped ? parseExpiryKey(dropped) : null
      setHint(
        parsed
          ? `At most ${MAX_MODEL_SECTIONS} expirations at once — ${formatExpiry(parsed.expiry)} ${parsed.root} made room.`
          : null,
      )
    },
    [sectionKeys],
  )

  const onSetQty = useCallback(
    (contract: OptionContract, qty: number) => setModelQty(contract, qty),
    [setModelQty],
  )
  const onShowAll = useCallback(() => setModelStrikeCount(null), [setModelStrikeCount])

  // Centring on spot. Once per topmost section (and strike window): the
  // guard is what keeps a snapshot refetch — new data, same section — or a
  // Model-cell edit from yanking the rows out from under the trader. A layout
  // effect, so the first frame the chain is painted is already centred.
  const scrollRef = useRef<HTMLDivElement>(null)
  const centredFor = useRef<string | null>(null)
  const onCentre = useCallback((key: string) => {
    if (scrollRef.current) centreSpotRow(scrollRef.current, key)
  }, [])
  const topKey = sections[0] ? expiryKey(sections[0]) : null
  const topRows = topKey === null ? 0 : (queryByKey.get(topKey)?.data?.rows.length ?? 0)
  const topReady = topRows > 0
  const centreMark = topKey === null ? null : `${topKey}#${String(model.strikeCount)}`
  useLayoutEffect(() => {
    if (!topReady || topKey === null || centreMark === null) return
    if (centredFor.current === centreMark) return
    const container = scrollRef.current
    if (container && centreSpotRow(container, topKey)) centredFor.current = centreMark
  }, [topReady, topKey, centreMark])

  const refresh = (): void => {
    for (const queryKey of CHAIN_KEYS) void client.invalidateQueries({ queryKey })
  }

  // The age pill reports the oldest chain on screen (the listing's own age
  // until one has loaded — it is cached far longer than any chain), and every
  // reason any of them, or the listing, was degraded.
  const loaded = sections
    .map((e) => queryByKey.get(expiryKey(e))?.data)
    .filter((chain): chain is OptionChainResponse => chain !== undefined)
  const oldest = loaded.reduce<string | null>(
    (at, chain) => (at === null || chain.as_of < at ? chain.as_of : at),
    null,
  ) ?? listing.data?.as_of ?? null
  const degraded = [...new Set([...(listing.data?.degraded ?? []), ...loaded.flatMap((c) => c.degraded)])]
  const stale = (listing.data?.stale ?? false) || loaded.some((chain) => chain.stale)
  const source = loaded[0]?.source ?? listing.data?.source ?? null

  const selectedCount = tradesById(trades, selected).length
  const net = modelNetCost(model.legs, marks)
  const legCount = model.legs.length
  const fetching = chainQueries.some((query) => query.isFetching) || listing.isFetching

  return (
    <div className="flex min-h-0 flex-1 flex-col gap-2">
      <div className="ws-card flex shrink-0 flex-wrap items-center gap-x-4 gap-y-1 px-2.5 py-1.5">
        <span className="text-[11px] text-muted">
          Position from:{' '}
          <button
            type="button"
            className="bg-transparent font-medium text-accent underline-offset-2 hover:underline"
            title="The Pos column counts these trades — change them on the Positions tab"
            onClick={() => setTab('positions')}
          >
            {trades.length === 0
              ? 'no open trades'
              : selectedCount > 0
                ? `${selectedCount} selected trade${selectedCount === 1 ? '' : 's'}`
                : `all ${trades.length} open trade${trades.length === 1 ? '' : 's'}`}
          </button>
          {/* The Pos column counts the whole book when nothing is selected —
              what the trader holds — but the graph then draws the model on
              its own, and the two must not be read as the same set. */}
          {selectedCount === 0 && legCount > 0 && model.include ? (
            <span> · the graph shows the model alone</span>
          ) : null}
        </span>

        <div role="group" aria-label="Strikes around spot" className="flex items-center gap-1">
          <span className="ws-label mr-0.5">Strikes</span>
          {STRIKE_CHOICES.map((count) => (
            <ToggleButton
              key={String(count)}
              pressed={model.strikeCount === count}
              onClick={() => setModelStrikeCount(count)}
              title={count === null ? 'Every listed strike' : `${count} strikes centred on spot`}
            >
              {count === null ? 'All' : count}
            </ToggleButton>
          ))}
        </div>

        <div className="flex items-center gap-1.5">
          <button
            type="button"
            className="ws-btn"
            onClick={refresh}
            title="Fetch the expirations and every chain on screen again"
          >
            ↻ refresh
          </button>
          <ChainAge asOf={oldest} source={source} stale={stale} degraded={degraded} />
          {fetching ? <Spinner label="" /> : null}
        </div>

        <div className="ml-auto flex items-center gap-2">
          <span className="text-[11px]">
            <span className="font-semibold text-warn">Model</span>
            <span className="num text-muted">
              {' '}
              · {legCount} leg{legCount === 1 ? '' : 's'}
              {legCount > 0 ? ` · ${fmtNetCost(net)}` : ''}
            </span>
            {legCount > 0 && !model.include ? (
              <span className="text-muted"> · hidden from the graph</span>
            ) : null}
          </span>
          <button
            type="button"
            className="ws-btn"
            onClick={() => setTab('positions')}
            title="Back to the risk graph, which draws the model on top of the selected trades — or alone, with none selected"
          >
            Show in Positions
          </button>
          <button
            type="button"
            className="ws-btn"
            disabled={legCount === 0}
            aria-label="Clear modeled trades"
            title="Remove every modeled leg (the chain layout stays)"
            onClick={clearModel}
          >
            Clear model
          </button>
        </div>
      </div>

      {listing.isLoading ? (
        <div className="ws-card px-3 py-4">
          <Spinner label="Loading expirations…" />
        </div>
      ) : listing.isError && !listing.data ? (
        <div className="ws-card">
          <ErrorNote error={listing.error} onRetry={() => void listing.refetch()} />
          {errorSlug(listing.error) === 'chain-unavailable' ? (
            <p className="m-0 px-3 pb-3 text-[12px] text-muted">
              The {sourceLabel(snapshot.data?.option_source)} source cannot list an option chain. Configure Schwab, tastytrade or yfinance under{' '}
              <button type="button" className="ws-btn" onClick={() => setTab('health')}>
                Data health → Settings
              </button>
              .
            </p>
          ) : null}
        </div>
      ) : (
        <>
          <div className="ws-card shrink-0">
            <ExpiryStrip
              expirations={expirations}
              selected={sectionKeys}
              held={heldKeys}
              modeled={modeled}
              onToggle={onToggle}
              hint={hint}
            />
          </div>

          <div className="flex min-h-0 flex-1 flex-col gap-2 min-[1400px]:grid min-[1400px]:grid-cols-[minmax(0,1fr)_18rem]">
            {/* Always there, empty state included, so the first quantity
                typed does not push the chain down under the cursor (and away
                from the cell Enter just moved focus to); the floor is the
                height of a one-leg ticket for the same reason. */}
            <div className="flex max-h-40 min-h-[6rem] shrink-0 flex-col min-[1400px]:order-2 min-[1400px]:max-h-none min-[1400px]:min-h-0">
              <ModelTicket legs={model.legs} marks={marks} include={model.include} />
            </div>
            <div
              ref={scrollRef}
              data-testid="chain-scroll"
              className="flex min-h-0 flex-1 flex-col gap-2 overflow-auto min-[1400px]:order-1"
            >
              {sections.length === 0 ? (
                <p className="m-0 px-2 py-4 text-center text-muted">No expirations are listed.</p>
              ) : (
                sections.map((listingEntry) => {
                  const key = expiryKey(listingEntry)
                  const query = queryByKey.get(key)
                  if (!query) return null
                  return (
                    <ChainSection
                      key={key}
                      sectionKey={key}
                      listing={listingEntry}
                      query={query}
                      positions={positions}
                      modelQty={modelQty}
                      strikeCount={model.strikeCount}
                      onSetQty={onSetQty}
                      onShowAll={onShowAll}
                      onHide={onToggle}
                      onCentre={onCentre}
                    />
                  )
                })
              )}
            </div>
          </div>
        </>
      )}
    </div>
  )
}
