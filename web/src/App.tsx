import { useEffect, useRef } from 'react'
import { useEventStream } from './api/events'
import { useTrades, useWarnings } from './api/queries'
import { ClosedTrades } from './components/ClosedTrades'
import { DataHealth } from './components/DataHealth'
import { PortfolioCards } from './components/PortfolioCards'
import { PositionsTable } from './components/PositionsTable'
import { RiskGraphPanel } from './components/graph/RiskGraphPanel'
import { SplitHandle } from './components/SplitHandle'
import { StatusBar } from './components/StatusBar'
import { Card, ErrorNote, Pill, Spinner } from './components/ui'
import { pruneIds } from './lib/aggregate'
import { useMenuBridge } from './lib/menu'
import { partitionWarnings } from './lib/warnings'
import { applyTheme, useAppStore, type Tab } from './store/appStore'

/** The risk graph keeps at least this much (px) of the tab however far the
 *  open-positions table is dragged down. */
const GRAPH_MIN_HEIGHT = 260

const TABS: { id: Tab; label: string }[] = [
  { id: 'positions', label: 'Positions' },
  { id: 'closed', label: 'Closed' },
  { id: 'health', label: 'Data health' },
]

export default function App() {
  useMenuBridge()
  const streamStatus = useEventStream()
  const trades = useTrades()
  const theme = useAppStore((s) => s.theme)
  const tab = useAppStore((s) => s.tab)
  const setTab = useAppStore((s) => s.setTab)
  const selected = useAppStore((s) => s.selected)
  const setSelected = useAppStore((s) => s.setSelected)
  const positionsCollapsed = useAppStore((s) => s.positionsCollapsed)
  const setPositionsCollapsed = useAppStore((s) => s.setPositionsCollapsed)
  const positionsHeight = useAppStore((s) => s.positionsHeight)
  const setPositionsHeight = useAppStore((s) => s.setPositionsHeight)
  const graphMaximized = useAppStore((s) => s.graphMaximized)
  const setGraphMaximized = useAppStore((s) => s.setGraphMaximized)
  const dismissedWarnings = useAppStore((s) => s.dismissedWarnings)
  const warnings = useWarnings()
  const mainRef = useRef<HTMLElement>(null)
  const positionsRef = useRef<HTMLElement>(null)

  useEffect(() => applyTheme(theme), [theme])

  // Esc brings the dashboard back around a maximized graph. A modal on top
  // swallows Esc first (capture + stopPropagation), so this never closes
  // both at once.
  useEffect(() => {
    if (!graphMaximized) return undefined
    const onKeyDown = (event: KeyboardEvent): void => {
      if (event.key === 'Escape') setGraphMaximized(false)
    }
    document.addEventListener('keydown', onKeyDown)
    return () => document.removeEventListener('keydown', onKeyDown)
  }, [graphMaximized, setGraphMaximized])

  // A renamed or newly closed trade must not linger in the selection.
  const list = trades.data?.trades
  useEffect(() => {
    if (!list) return
    const pruned = pruneIds(list, selected)
    if (pruned.length !== selected.length) setSelected(pruned)
  }, [list, selected, setSelected])

  const openTrades = list ?? []
  // The badge counts what the Data health tab will actually list: the
  // server's warnings minus the ones the trader cleared. Until that list has
  // loaded the server's own count stands in.
  const warningCount = warnings.data
    ? partitionWarnings(warnings.data.warnings, dismissedWarnings).visible.length
    : (trades.data?.warning_count ?? 0)

  const resizePositions = (height: number): void => {
    // A main area without a laid-out height cannot bound anything.
    const room = mainRef.current?.clientHeight ?? 0
    setPositionsHeight(room > 0 ? Math.min(height, room - GRAPH_MIN_HEIGHT) : height)
  }

  return (
    <div className="flex h-full flex-col bg-bg text-fg">
      <StatusBar streamStatus={streamStatus} />

      <nav className="flex shrink-0 items-center gap-1 border-b border-line bg-surface px-2">
        {TABS.map((entry) => (
          <button
            key={entry.id}
            type="button"
            className={`border-b-2 bg-transparent px-3 py-1.5 text-[12px] ${
              tab === entry.id
                ? 'border-accent font-semibold text-fg-strong'
                : 'border-transparent text-muted hover:text-fg'
            }`}
            aria-current={tab === entry.id ? 'page' : undefined}
            onClick={() => setTab(entry.id)}
          >
            {entry.label}
            {entry.id === 'health' && warningCount > 0 ? (
              <Pill tone="warn" className="ml-1.5">
                {warningCount}
              </Pill>
            ) : null}
          </button>
        ))}
      </nav>

      <main ref={mainRef} className="flex min-h-0 flex-1 flex-col gap-2 p-2">
        {trades.isLoading ? (
          <div className="p-4">
            <Spinner label="Loading trades…" />
          </div>
        ) : trades.data || tab === 'health' ? (
          <>
            {/* A transient background-refetch failure keeps the last-known
                book on screen; a first-load failure (no data at all) blanks
                the positions and closed tabs, handled below. Data health is
                never blanked: the trades command answers 503 until some
                market source works, which is the ordinary state of a fresh
                install — and Settings, the one place a source is configured,
                lives on that tab. */}
            {trades.isError ? (
              <ErrorNote error={trades.error} onRetry={() => void trades.refetch()} />
            ) : null}
            {!trades.data ? (
              <DataHealth />
            ) : tab === 'positions' ? (
              <>
                {graphMaximized ? null : (
                  <>
                    <PortfolioCards
                      trades={openTrades}
                      portfolioAll={trades.data?.portfolio_all}
                      selected={selected}
                    />
                    <Card
                      title="Open positions"
                      className="shrink-0"
                      style={{
                        maxHeight: positionsCollapsed ? undefined : (positionsHeight ?? '42vh'),
                      }}
                      bodyClassName="flex min-h-0 flex-col"
                      cardRef={positionsRef}
                      collapsed={positionsCollapsed}
                      onToggleCollapse={() => setPositionsCollapsed(!positionsCollapsed)}
                      actions={
                        positionsCollapsed ? (
                          <span className="num text-[11px] text-muted">
                            {openTrades.length} open · {selected.length} selected
                          </span>
                        ) : undefined
                      }
                    >
                      <PositionsTable
                        trades={openTrades}
                        weightedVegaReference={trades.data?.portfolio_all.weighted_vega_reference}
                      />
                    </Card>
                    {positionsCollapsed ? null : (
                      <SplitHandle
                        label="Resize the open positions table"
                        value={positionsHeight}
                        measure={() => positionsRef.current?.offsetHeight ?? 0}
                        onResize={resizePositions}
                        onReset={() => setPositionsHeight(null)}
                      />
                    )}
                  </>
                )}
                <RiskGraphPanel trades={openTrades} />
              </>
            ) : tab === 'closed' ? (
              <ClosedTrades />
            ) : (
              <DataHealth />
            )}
          </>
        ) : trades.isError ? (
          <>
            <ErrorNote error={trades.error} onRetry={() => void trades.refetch()} />
            <p className="m-0 px-2 text-[12px] text-muted">
              No prices yet? A market-data source is set up under{' '}
              <button type="button" className="ws-btn" onClick={() => setTab('health')}>
                Data health → Settings
              </button>
              .
            </p>
          </>
        ) : null}
      </main>
    </div>
  )
}
