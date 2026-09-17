/**
 * A CSV imported on this computer survives an app restart.
 *
 * The CSV is a staged copy of `fixtures/trades/legs.csv` — copied, never
 * linked, and never `~/workspace/example_trades`. It is imported through
 * `set_trade_source`, the command a successful "Pick a CSV file…" pick leads
 * to; see `helpers.ts`'s `setTradeSource` for why a click cannot stand in for
 * it here.
 *
 * The point of the test is what happens *after* that. `browser.reloadSession()`
 * is a second WebDriver session, so a second app process, against the same
 * `XDG_*_HOME` — and `wingspan.toml` still names the demo book's own CSV,
 * exactly as it did for the first launch. So the only thing that can explain
 * the staged CSV coming back on screen is `<state_dir>/trade_source.json`,
 * which the desktop honours ahead of the config file
 * (`SavedSourcePolicy::Honour`, the default).
 *
 * The assertions read the trade-source control rather than the positions
 * table: `trade_source` reports the path, the source and the trade counts it
 * actually read off disk, none of which needs a market snapshot — and the
 * shipped app has no market source for a CSV of one's own with no network
 * (see `helpers.ts`).
 */
import { browser, expect } from '@wdio/globals'
import {
  persistedSourceCsv,
  pollUntil,
  setTradeSource,
  stageLegsCsv,
  tradeSourceLabel,
  tradeSourceSummary,
  waitForApp,
} from './helpers.ts'

const STAGED = persistedSourceCsv()
const BASENAME = STAGED.split('/').pop() as string

/** Waits for the status bar to be naming `STAGED`, then returns its summary
 *  (`<path> — CSV file on this computer. N open / M closed trades. …`). */
async function summaryForStagedCsv(): Promise<string> {
  const label = await pollUntil(tradeSourceLabel, (value) => value === BASENAME, 30_000)
  expect(label).toEqual(BASENAME)
  return tradeSourceSummary()
}

function openTradeCount(summary: string): number {
  const match = /(\d+) open/.exec(summary)
  return match === null ? -1 : Number(match[1])
}

describe('a CSV imported on this computer', () => {
  before(() => {
    stageLegsCsv(STAGED)
  })

  it('is loaded, and still loaded after the app restarts', async () => {
    await waitForApp()
    // The configured book, not the staged one — so the assertions below
    // cannot be passing on a coincidence. `wdio.conf.ts` points `[trades]
    // csv_path` at `fixtures/demo/legs.csv`, whose basename this is.
    expect(await tradeSourceLabel()).toEqual('legs.csv')

    await setTradeSource(STAGED)

    const picked = await summaryForStagedCsv()
    expect(picked).toContain(STAGED)
    expect(picked).toContain('CSV file on this computer')
    expect(openTradeCount(picked)).toBeGreaterThan(0)

    // Quit and relaunch. Same XDG dirs, same wingspan.toml (still pointing at
    // the demo book's CSV) — only the persisted record can explain what
    // comes back.
    await browser.reloadSession()

    await waitForApp()
    const restored = await summaryForStagedCsv()
    expect(restored).toContain(STAGED)
    expect(restored).toContain('CSV file on this computer')
    expect(openTradeCount(restored)).toEqual(openTradeCount(picked))
  })
})
