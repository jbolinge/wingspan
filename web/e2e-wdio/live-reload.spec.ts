/**
 * A change under the watched folder reaches the table with no manual reload.
 *
 * `TradesConfig::watch_debounce_ms` is 750ms, so a real filesystem round trip
 * plus the debounce plus a re-render must land well inside 2s — the number
 * the plan's live-reload requirement names.
 *
 * The book is the staged, writable copy of the synthetic demo book (see
 * `helpers.ts`: it is what "Load demo book" resolves to given the working
 * directory the driver is launched in). It has to be the demo book because it
 * is the only one the shipped binary can price with no network and no
 * credentials, and it has to be the *copy* because the repository's own
 * `fixtures/demo` is a tracked path this suite only ever reads.
 *
 * The edit is a duplicate of the trade file's own last order line — read back
 * out of the file, so it is guaranteed to still parse — which doubles the
 * strangle's quantity and so has to move the row.
 */
import { expect } from '@wdio/globals'
import { join } from 'node:path'
import { STAGED_DEMO_BOOK, appendDuplicateOrderLine, loadDemoBook, pollUntil, tradeRowText } from './helpers.ts'

const TRADE = 'Oct Strangle'
const TRADE_FILE = join(STAGED_DEMO_BOOK, 'Open Trades', 'Oct Strangle Aug 24 2026')

describe('editing a watched trade file', () => {
  it('updates the row with no manual reload', async () => {
    await loadDemoBook()

    const before = await tradeRowText(TRADE)
    expect(before).not.toBeNull()

    appendDuplicateOrderLine(TRADE_FILE)

    const after = await pollUntil(() => tradeRowText(TRADE), (text) => text !== before, 2_000)

    expect(after).not.toBeNull()
    expect(after).not.toEqual(before)
  })
})
