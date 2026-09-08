import { useEffect } from 'react'
import { useEventStream } from './api/events'
import { useTrades } from './api/queries'
import { ClosedTrades } from './components/ClosedTrades'
import { DataHealth } from './components/DataHealth'
import { PortfolioCards } from './components/PortfolioCards'
import { PositionsTable } from './components/PositionsTable'
import { RiskGraphPanel } from './components/graph/RiskGraphPanel'
import { StatusBar } from './components/StatusBar'
import { Card, ErrorNote, Pill, Spinner } from './components/ui'
import { pruneIds } from './lib/aggregate'
import { useMenuBridge } from './lib/menu'
import { applyTheme, useAppStore, type Tab } from './store/appStore'

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

  useEffect(() => applyTheme(theme), [theme])

  // A renamed or newly closed trade must not linger in the selection.
  const list = trades.data?.trades
  useEffect(() => {
    if (!list) return
    const pruned = pruneIds(list, selected)
    if (pruned.length !== selected.length) setSelected(pruned)
  }, [list, selected, setSelected])

  const openTrades = list ?? []
  const warningCount = trades.data?.warning_count ?? 0

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

      <main className="flex min-h-0 flex-1 flex-col gap-2 p-2">
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
                <PortfolioCards
                  trades={openTrades}
                  portfolioAll={trades.data?.portfolio_all}
                  selected={selected}
                />
                <Card
                  title="Open positions"
                  className="max-h-[42vh] shrink-0"
                  bodyClassName="min-h-0"
                >
                  <PositionsTable
                    trades={openTrades}
                    weightedVegaReference={trades.data?.portfolio_all.weighted_vega_reference}
                  />
                </Card>
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
