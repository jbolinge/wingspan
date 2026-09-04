/**
 * Nothing this whole run wrote leaked an account id or an execution
 * reference — and the app actually wrote a log, so the scan means something.
 *
 * Runs last (`wdio.conf.ts`'s `specs` order), so `<XDG_DATA_HOME>` already
 * holds every other spec's logs, window geometry, webview data and market
 * state by the time this scans it. One `check_redaction.py --root
 * <XDG_DATA_HOME>` covers all of it: the log directory is
 * `<XDG_DATA_HOME>/com.jbolinge.wingspan/logs/` and `[market] state_dir` is
 * nested under the same tree for exactly this reason (`helpers.ts`'s
 * `xdgPaths`) — the same two regexes CI runs over the unpacked `.deb`.
 */
import { readdirSync, statSync } from 'node:fs'
import { join } from 'node:path'
import { expect } from '@wdio/globals'
import { checkRedaction, loadDemoBook, xdgPaths } from './helpers.ts'

describe('nothing this run wrote leaked an account id', () => {
  it('a real run is clean, and left a real log file', async () => {
    // A launch that gets all the way to priced positions, so the tracing file
    // layer has had every code path a normal session takes through it.
    await loadDemoBook()

    const xdg = xdgPaths()
    const logs = readdirSync(xdg.logDir).filter((name) => name.endsWith('.log'))
    expect(logs.length).toBeGreaterThan(0)
    const bytes = logs.reduce((sum, name) => sum + statSync(join(xdg.logDir, name)).size, 0)
    expect(bytes).toBeGreaterThan(0)

    const { code, output } = checkRedaction(xdg.dataHome)
    if (code !== 0) {
      throw new Error(`check_redaction.py --root ${xdg.dataHome} found something:\n${output}`)
    }
    expect(code).toBe(0)
  })
})
