/**
 * A folder picked on this computer survives an app restart.
 *
 * The folder is a staged copy of `fixtures/trades` — copied, never linked,
 * and never `~/workspace/example_trades`. It is picked through
 * `set_trades_root`, the command `TradesFolderDialog`'s "Load & analyze"
 * button calls; see `helpers.ts`'s `setTradesRoot` for why a click cannot
 * stand in for it here.
 *
 * The point of the test is what happens *after* that. `browser.reloadSession()`
 * is a second WebDriver session, so a second app process, against the same
 * `XDG_*_HOME` — and `wingspan.toml` still says `[trades] root =
 * <repo>/fixtures/demo`, exactly as it did for the first launch. So the only
 * thing that can put the staged folder back on screen is
 * `<state_dir>/trades_root.json`, which `SavedRootPolicy::Honour` (the
 * desktop's default) reads ahead of the config file.
 *
 * The assertions read the trades-folder control rather than the positions
 * table: `trades_root` reports the path, the source and the trade counts it
 * actually read off the disk, none of which needs a market snapshot — and the
 * shipped app has no market source for a folder of one's own with no network
 * (see `helpers.ts`).
 */
import { browser, expect } from '@wdio/globals'
import {
  persistedFolderTradesDir,
  pollUntil,
  setTradesRoot,
  stageTradesFolder,
  tradesFolderLabel,
  tradesFolderSummary,
  waitForApp,
} from './helpers.ts'

const LABEL = 'Persisted'
const STAGED = persistedFolderTradesDir()
const BASENAME = STAGED.split('/').pop() as string

/** Waits for the status bar to be naming `STAGED`, then returns its summary
 *  (`<path> — folder on this computer. N open / M closed trades. …`). */
async function summaryForStagedFolder(): Promise<string> {
  const label = await pollUntil(tradesFolderLabel, (value) => value === BASENAME, 30_000)
  expect(label).toEqual(BASENAME)
  return tradesFolderSummary()
}

function openTradeCount(summary: string): number {
  const match = /(\d+) open/.exec(summary)
  return match === null ? -1 : Number(match[1])
}

describe('a folder picked on this computer', () => {
  before(() => {
    stageTradesFolder(STAGED, LABEL)
  })

  it('is loaded, and still loaded after the app restarts', async () => {
    await waitForApp()
    // The configured book, not the staged one — so the assertions below
    // cannot be passing on a coincidence.
    expect(await tradesFolderLabel()).toEqual('demo')

    await setTradesRoot(STAGED)

    const picked = await summaryForStagedFolder()
    expect(picked).toContain(STAGED)
    expect(picked).toContain('folder on this computer')
    expect(openTradeCount(picked)).toBeGreaterThan(0)

    // Quit and relaunch. Same XDG dirs, same wingspan.toml (still pointing at
    // the demo book) — only the persisted record can explain what comes back.
    await browser.reloadSession()

    await waitForApp()
    const restored = await summaryForStagedFolder()
    expect(restored).toContain(STAGED)
    expect(restored).toContain('folder on this computer')
    expect(openTradeCount(restored)).toEqual(openTradeCount(picked))
  })
})
