/**
 * The Model tab: the option chain with selectable expirations, ONE-style,
 * where a quantity typed into a Model cell becomes a modeled leg drawn on the
 * risk graph — the Positions tab's, and this tab's own.
 *
 * Top to bottom: a toolbar (the Chain / Risk graph switch, in chain view the
 * chain's SPX | XSP underlying, whose trades the Pos column counts, the
 * strike window, refresh, and the model's summary and actions), then one of two views, with the model ticket beside it on a wide
 * window or above it on a narrow one:
 *
 * - **Chain** — the expiration strip and the chain, one expiration at a time,
 *   scrolling inside the tab. Picking another chip replaces the chain on
 *   screen; picking the open chip, or the chain's own ✕, closes it, and a
 *   closed chain ("No chain open") is a legal state the trader stays in until
 *   they pick another.
 * - **Risk graph** — `RiskGraphPanel`, the very component Positions mounts,
 *   with the same props: the same queries, the same cache, so flipping
 *   between building the model and seeing what it does is instant, and the
 *   trader never has to leave the tab to look. Its ⤢ maximize gives it the
 *   whole tab here too.
 *
 * The chain opens on the money: its spot line is scrolled to the middle of
 * the view when the tab is displayed, and again whenever a different section
 * opens or the chain comes back from the graph — never on a refetch or an
 * edit, which would move the rows under the trader's cursor.
 */

import { useQueryClient } from '@tanstack/react-query'
import { useCallback, useLayoutEffect, useMemo, useRef } from 'react'
import { errorSlug } from '../../api/client'
import {
  CHAIN_KEYS,
  useChainExpirations,
  useOptionChains,
  useSnapshot,
  type ChainRequestSection,
} from '../../api/queries'
import type { ChainScope, ChainUnderlying, OptionChainResponse, OptionContract, Trade } from '../../api/types'
import { pruneIds, tradesById } from '../../lib/aggregate'
import { fmtAge, fmtClock } from '../../lib/format'
import {
  autoExpiry,
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
  positionsOn,
  underlyingOf,
} from '../../lib/model'
import { sourceLabel } from '../../lib/provider'
import { useNow } from '../../lib/useNow'
import { AUTO_EXPIRY, useAppStore, type ModelView, type StrikeCount } from '../../store/appStore'
import { RiskGraphPanel } from '../graph/RiskGraphPanel'
import { ErrorNote, Pill, Spinner, ToggleButton, Tooltip } from '../ui'
import { ChainSection } from './ChainTable'
import { ExpiryStrip } from './ExpiryStrip'
import { ModelTicket } from './ModelTicket'

const STRIKE_CHOICES: readonly StrikeCount[] = [16, 32, 64, null]

const VIEWS: readonly { view: ModelView; label: string; title: string }[] = [
  { view: 'chain', label: 'Chain', title: 'Build the model on the option chain' },
  {
    view: 'graph',
    label: 'Risk graph',
    title: 'The risk graph: the model on top of the selected trades — or alone, with none selected',
  },
]

const UNDERLYINGS: readonly { underlying: ChainUnderlying; label: string; title: string }[] = [
  { underlying: 'SPX', label: 'SPX', title: 'Build the model on SPX options' },
  {
    underlying: 'XSP',
    label: 'XSP',
    title:
      'Build the model on XSP (Mini-SPX) options: SPX spot, strikes and prices divided by 10, PM-settled. An XSP contract is one tenth of an SPX contract.',
  },
]

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
  const moveModelLeg = useAppStore((s) => s.moveModelLeg)
  const stepModelQty = useAppStore((s) => s.stepModelQty)
  const clearModel = useAppStore((s) => s.clearModel)
  const setModelStrikeCount = useAppStore((s) => s.setModelStrikeCount)
  const setModelExpiry = useAppStore((s) => s.setModelExpiry)
  const setModelView = useAppStore((s) => s.setModelView)
  const setModelUnderlying = useAppStore((s) => s.setModelUnderlying)
  const graphMaximized = useAppStore((s) => s.graphMaximized)
  const setGraphMaximized = useAppStore((s) => s.setGraphMaximized)
  const snapshot = useSnapshot()
  const listing = useChainExpirations()

  // `trades` is a fresh array on every poll; the Pos map is rebuilt only when
  // the quantities it would hold actually change, so the (memoised) chain
  // tables do not repaint on a poll that changed nothing.
  const ids = effectiveTradeIds(selected, trades)
  // What the ticket's Position rows price: the ticked trades that still exist,
  // and nothing when none are ticked (the graph then draws the model alone).
  const selectedIds = pruneIds(trades, selected)
  const positionsKey = JSON.stringify([...positionByContract(trades, ids)])
  const positions = useMemo(
    () => new Map<string, number>(JSON.parse(positionsKey) as [string, number][]),
    [positionsKey],
  )
  const modelQty = useMemo(() => modelQtyByContract(model.legs), [model.legs])

  // The whole SPX listing, which every chain request is keyed by — and what
  // the chain on screen offers: XSP is always PM-settled, so under XSP the
  // AM monthlies (`SPX`/AM) are not offered; every PM expiration is.
  const fullListing = useMemo(() => listing.data?.expirations ?? [], [listing.data])
  const underlying = model.underlying
  const expirations = useMemo(
    () => (underlying === 'XSP' ? fullListing.filter((e) => e.settlement === 'pm') : fullListing),
    [fullListing, underlying],
  )
  const listed = useMemo(() => new Set(expirations.map(expiryKey)), [expirations])
  const fullListed = useMemo(() => new Set(fullListing.map(expiryKey)), [fullListing])
  // The chips' dots and the automatic pick count only the product on
  // screen: an XSP hedge must not mark (or open) the SPXW section the SPX
  // chain would show with an empty Pos, nor an SPXW holding the XSP one.
  const chainPositions = useMemo(() => positionsOn(positions, underlying), [positions, underlying])
  const chainLegs = useMemo(
    () => model.legs.filter((leg) => underlyingOf(leg.contract) === underlying),
    [model.legs, underlying],
  )
  const auto = useMemo(
    () => autoExpiry(expirations, chainPositions, chainLegs),
    [expirations, chainPositions, chainLegs],
  )
  // `null` is the trader's own "no chain open"; the automatic pick opens
  // until they choose, and again once the chosen one has expired off the
  // listing.
  // Under XSP a chosen AM monthly (`SPX`/AM) is not offered; the same date's
  // PM section (`SPXW`, listed on every third Friday) stands in for it, so
  // the switch keeps the trader on the expiry they were looking at.
  const pmTwin = (key: string): string | null => {
    const parsed = parseExpiryKey(key)
    if (parsed === null || parsed.root !== 'SPX') return null
    const twin = expiryKey({ expiry: parsed.expiry, root: 'SPXW' })
    return listed.has(twin) ? twin : null
  }
  const openKey =
    model.expiry === null
      ? null
      : model.expiry !== AUTO_EXPIRY && listed.has(model.expiry)
        ? model.expiry
        : (model.expiry !== AUTO_EXPIRY && underlying === 'XSP' ? pmTwin(model.expiry) : null) ?? auto
  const section = expirations.find((e) => expiryKey(e) === openKey) ?? null
  const showChain = model.view === 'chain'

  // Every chain the tab needs: the one on screen, plus the expiries the
  // modeled legs sit in, so the ticket has a mark for every leg. In graph
  // view the open section is not on screen, and observing it anyway would
  // refetch it on every snapshot (a 0DTE SPXW chain is ~16 tastytrade
  // requests) for nothing; its cached data makes the way back instant.
  const screenKey = section && showChain ? expiryKey(section) : null
  // A leg is priced from the chain of its own underlying — an XSP hedge
  // keeps its marks while the chain on screen is SPX — so a request is the
  // triple (underlying, expiry, root).
  const wanted: { key: string; section: string; underlying: ChainUnderlying }[] = []
  const wantedKeys = new Set<string>()
  const want = (section: string, under: ChainUnderlying): void => {
    const key = `${under}|${section}`
    if (wantedKeys.has(key)) return
    wantedKeys.add(key)
    wanted.push({ key, section, underlying: under })
  }
  if (screenKey !== null) want(screenKey, underlying)
  for (const leg of model.legs) {
    const key = expiryKey(leg.contract)
    if (fullListed.has(key)) want(key, underlyingOf(leg.contract))
  }
  // Only the chain on screen, and only under All, asks for every listed
  // strike: a per-symbol provider stops at the window around spot otherwise,
  // which is plenty for 16 · 32 · 64 but leaves a 3000 tail put with no row.
  // A chain wanted for the ticket's marks keeps to the window — the server
  // stretches it to hold every modeled strike.
  const chainSections: ChainRequestSection[] = wanted.flatMap(({ section: key, underlying: under }) => {
    const parsed = parseExpiryKey(key)
    if (parsed === null) return []
    const onScreen = key === screenKey && under === underlying
    const scope: ChainScope = onScreen && model.strikeCount === null ? 'all' : 'near'
    return [{ ...parsed, scope, underlying: under }]
  })
  const chainQueries = useOptionChains(chainSections)
  const queryByKey = new Map(wanted.map(({ key }, index) => [key, chainQueries[index]]))
  const sectionQuery = section ? queryByKey.get(`${underlying}|${expiryKey(section)}`) : undefined
  const marks = chainMarks(chainQueries.map((query) => query.data))

  const heldKeys = useMemo(() => positionExpiryKeys(chainPositions), [chainPositions])
  const modeled = useMemo(() => new Set(chainLegs.map((leg) => expiryKey(leg.contract))), [chainLegs])

  // A chip is a switch for the chain on screen: the open one closes, any
  // other replaces it.
  const onToggle = useCallback(
    (key: string) => setModelExpiry(key === openKey ? null : key),
    [openKey, setModelExpiry],
  )
  const onHide = useCallback(() => setModelExpiry(null), [setModelExpiry])

  const onSetQty = useCallback(
    (contract: OptionContract, qty: number) => setModelQty(contract, qty),
    [setModelQty],
  )
  const onStepQty = useCallback(
    (contract: OptionContract, delta: 1 | -1) => stepModelQty(contract, delta),
    [stepModelQty],
  )
  const onMoveQty = useCallback(
    (from: string, to: OptionContract) => moveModelLeg(from, to),
    [moveModelLeg],
  )
  const onShowAll = useCallback(() => setModelStrikeCount(null), [setModelStrikeCount])

  // Centring on spot. Once per section (and strike window): the guard is
  // what keeps a snapshot refetch — new data, same section — or a Model-cell
  // edit from yanking the rows out from under the trader. A layout effect, so
  // the first frame the chain is painted is already centred.
  const scrollRef = useRef<HTMLDivElement>(null)
  const centredFor = useRef<string | null>(null)
  const onCentre = useCallback((key: string) => {
    if (scrollRef.current) centreSpotRow(scrollRef.current, key)
  }, [])
  // In graph view there is no chain on screen to centre — the scroll
  // container is not even mounted — so it counts as closed.
  const sectionKey = section && showChain ? expiryKey(section) : null
  // Placeholder rows (the windowed chain, while All's whole one loads) keep
  // the scroll where it was; the whole chain, with its rows above and below,
  // is centred when it lands.
  const rowCount = sectionQuery?.data?.rows.length ?? 0
  const ready = rowCount > 0 && sectionQuery?.isPlaceholderData !== true
  const centreMark =
    sectionKey === null ? null : `${underlying}|${sectionKey}#${String(model.strikeCount)}`
  useLayoutEffect(() => {
    // Closing the chain (or switching to the graph) forgets the last
    // centring, so the chain that comes back — even the same expiration —
    // opens on the money again.
    if (centreMark === null) centredFor.current = null
    if (!ready || sectionKey === null || centreMark === null) return
    if (centredFor.current === centreMark) return
    const container = scrollRef.current
    if (container && centreSpotRow(container, sectionKey)) centredFor.current = centreMark
  }, [ready, sectionKey, centreMark])

  const refresh = (): void => {
    for (const queryKey of CHAIN_KEYS) void client.invalidateQueries({ queryKey })
  }

  // The age pill reports the chain on screen (the listing's own age until it
  // has loaded — it is cached far longer than any chain), and every reason
  // it, or the listing, was degraded.
  const loaded = [sectionQuery?.data].filter(
    (chain): chain is OptionChainResponse => chain !== undefined,
  )
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

  // The ticket rail: always there, empty state included, so the first
  // quantity typed does not push the chain down under the cursor (and away
  // from the cell Enter just moved focus to); the floor is the height of a
  // one-leg ticket for the same reason.
  const ticket = (
    <div className="flex max-h-52 min-h-[6rem] shrink-0 flex-col min-[1400px]:order-2 min-[1400px]:max-h-none min-[1400px]:min-h-0">
      <ModelTicket legs={model.legs} marks={marks} include={model.include} selectedIds={selectedIds} />
    </div>
  )

  // Maximized, the graph has the whole tab — no toolbar, no ticket — exactly
  // as Positions drops its cards and table around it (Esc, handled in App,
  // brings them back). Only in graph view: a chain on screen is not the
  // graph the trader maximized.
  if (graphMaximized && !showChain) return <RiskGraphPanel trades={trades} />

  return (
    <div className="flex min-h-0 flex-1 flex-col gap-2">
      <div className="ws-card flex shrink-0 flex-wrap items-center gap-x-4 gap-y-1 px-2.5 py-1.5">
        {/* The tab's primary switch, so it leads the toolbar. */}
        <div role="group" aria-label="Model view" className="flex items-center gap-1">
          {VIEWS.map(({ view, label, title }) => (
            <ToggleButton
              key={view}
              pressed={model.view === view}
              onClick={() => {
                setModelView(view)
                // A maximize left over from Positions (the flag is global)
                // would otherwise swallow the toolbar the moment the graph
                // view opens; from here the graph opens beside its ticket,
                // and the panel's own ⤢ maximizes it.
                if (graphMaximized) setGraphMaximized(false)
              }}
              title={title}
            >
              {label}
            </ToggleButton>
          ))}
        </div>

        {/* Which index the chain is built on, right after the view switch.
            A property of the chain, not of the model (every leg keeps its
            own underlying), so like Strikes it is labelled and shown in
            chain view only. */}
        {showChain ? (
          <div role="group" aria-label="Chain underlying" className="flex items-center gap-1">
            <span className="ws-label mr-0.5">Chain on</span>
            {UNDERLYINGS.map(({ underlying: choice, label, title }) => (
              <ToggleButton
                key={choice}
                pressed={underlying === choice}
                onClick={() => setModelUnderlying(choice)}
                title={title}
              >
                {label}
              </ToggleButton>
            ))}
          </div>
        ) : null}

        <span className="text-[11px] text-muted">
          Position from:{' '}
          <button
            type="button"
            className="bg-transparent font-medium text-accent underline-offset-2 hover:underline"
            title={
              showChain
                ? 'The Pos column counts these trades — change them on the Positions tab'
                : 'The risk graph draws these trades (with the model) — change them on the Positions tab'
            }
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

        {/* The strike window shapes the chain alone; the graph has its own
            range controls. */}
        {showChain ? (
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
        ) : null}

        <div className="flex items-center gap-1.5">
          <button
            type="button"
            className="ws-btn"
            onClick={refresh}
            title={
              showChain
                ? 'Fetch the expirations and the chain on screen again'
                : "Fetch the expirations and the chains the ticket's marks come from again"
            }
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
            disabled={legCount === 0}
            aria-label="Clear modeled trades"
            title="Remove every modeled leg (the chain layout stays)"
            onClick={clearModel}
          >
            Clear model
          </button>
        </div>
      </div>

      {showChain ? (
        listing.isLoading ? (
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
                selected={openKey}
                held={heldKeys}
                modeled={modeled}
                onToggle={onToggle}
                displayRoot={underlying === 'XSP' ? 'XSP' : undefined}
              />
            </div>

            <div className="flex min-h-0 flex-1 flex-col gap-2 min-[1400px]:grid min-[1400px]:grid-cols-[minmax(0,1fr)_18rem]">
              {ticket}
              <div
                ref={scrollRef}
                data-testid="chain-scroll"
                className="flex min-h-0 flex-1 flex-col gap-2 overflow-auto min-[1400px]:order-1"
              >
                {section === null || !sectionQuery ? (
                  <p className="m-0 px-2 py-4 text-center text-muted">
                    {expirations.length === 0
                      ? 'No expirations are listed.'
                      : 'No chain open — pick an expiration above.'}
                  </p>
                ) : (
                  <ChainSection
                    key={`${underlying}|${expiryKey(section)}`}
                    underlying={underlying}
                    sectionKey={expiryKey(section)}
                    listing={section}
                    query={sectionQuery}
                    positions={positions}
                    modelQty={modelQty}
                    strikeCount={model.strikeCount}
                    onSetQty={onSetQty}
                    onStepQty={onStepQty}
                    onMoveQty={onMoveQty}
                    onShowAll={onShowAll}
                    onHide={onHide}
                    onCentre={onCentre}
                  />
                )}
              </div>
            </div>
          </>
        )
      ) : (
        // The graph needs no chain listing — a source that cannot list one
        // still draws the selected trades — so the listing's loading and
        // error states belong to the chain view alone. The ticket keeps its
        // rail beside the graph: its greeks are the numbers behind the curves.
        <div className="flex min-h-0 flex-1 flex-col gap-2 min-[1400px]:grid min-[1400px]:grid-cols-[minmax(0,1fr)_18rem]">
          {ticket}
          <div className="flex min-h-0 flex-1 flex-col overflow-auto min-[1400px]:order-1">
            <RiskGraphPanel trades={trades} />
          </div>
        </div>
      )}
    </div>
  )
}
