/**
 * Shared plumbing for the WebDriver bundle smoke suite.
 *
 * Every spec here drives the *shipped* binary (`target/release/wingspan`,
 * built by `just bundle`) through `tauri-driver` in front of the system
 * `WebKitWebDriver` — the "external" provider. Nothing is linked into the
 * app: there is no `wdio` Cargo feature and no `tauri-plugin-wdio-webdriver`.
 *
 * ## Where the prices come from
 *
 * The shipped app has exactly one market source that works with no network
 * and no credentials: the synthetic `fixture` provider behind **"Load demo
 * book"** (`load_demo` → `providers::demo`). A launch configured the way this
 * suite configures one — no `[schwab] token_path`, `[yfinance] enabled =
 * false`, an empty `[market] state_dir` — has *no* spot source at all, so
 * `market-snapshot` fails and the positions panel renders its error instead
 * of a table. Every spec that asserts on rows therefore clicks "Load demo
 * book" first, exactly as a new user does on first run; the specs that assert
 * on which *source* is loaded (`persisted-source`) read the trade-source
 * control, which is fed by `trade_source` and needs no market data.
 *
 * `load_demo` resolves `fixtures/demo/legs.csv` **relative to the working
 * directory** ([`spawnTauriDriver`] sets that, and the whole child chain —
 * tauri-driver → WebKitWebDriver → the app — inherits it). So the demo book
 * the specs drive is a byte copy of `fixtures/demo` staged under
 * [`DEMO_CWD`]; the repository's own `fixtures/demo` is a tracked path and is
 * never written to.
 *
 * Nothing this suite produces lands anywhere in the repository — not even in
 * an ignored directory. [`RUN_DIR`] is under the system temp directory,
 * because `crates/wingspan-core/tests/redaction.rs` walks the whole checkout
 * and a run tree inside it would be scanned on every `cargo test`; the fix for
 * that is to put the artefacts somewhere else, never to widen its skip list.
 * The path is a fixed constant rather than a `mktemp` value precisely so that
 * the launcher process (`onPrepare`) and each worker process (a spec file, and
 * `beforeSession`) agree on where things are with no handoff.
 */

import { type ChildProcessByStdio, execFileSync, spawn } from 'node:child_process'
import { cpSync, existsSync, mkdirSync, rmSync, writeFileSync } from 'node:fs'
import { connect } from 'node:net'
import { homedir, tmpdir } from 'node:os'
import { dirname, join, resolve } from 'node:path'
import type { Readable } from 'node:stream'
import { fileURLToPath } from 'node:url'
import { browser } from '@wdio/globals'

const HERE = dirname(fileURLToPath(import.meta.url))
export const REPO_ROOT = resolve(HERE, '..', '..')

/** The shipped binary this whole suite drives — see `just bundle`. */
export const RELEASE_BINARY = join(REPO_ROOT, 'target/release/wingspan')

/** Everything this run writes, recreated per invocation — outside the
 *  checkout on purpose; see the module doc comment. */
export const RUN_DIR = join(tmpdir(), 'wingspan-wdio-smoke')

/** The working directory the app is launched in, and therefore where
 *  "Load demo book" finds its book — see the module doc comment. */
const DEMO_CWD = join(RUN_DIR, 'demo-cwd')
export const STAGED_DEMO_BOOK = join(DEMO_CWD, 'fixtures', 'demo')

/** `tauri.conf.json`'s `identifier` — where `dirs::data_dir()` puts the app's
 *  own tree under a redirected `XDG_DATA_HOME`. */
const APP_ID = 'com.jbolinge.wingspan'

/** Pin "today", so DTE/staleness read the same on every run. Deliberately the
 *  same day as `wingspan_app::demo::valuation_date()`, so clicking "Load demo
 *  book" (which pins the clock itself) does not move the calendar. */
const AS_OF = '2026-08-31'

export const TAURI_DRIVER_PORT = 4444
const NATIVE_DRIVER_PORT = 4445
const NATIVE_DRIVER = '/usr/bin/WebKitWebDriver'

interface XdgPaths {
  configHome: string
  dataHome: string
  /** `[market] state_dir` — nested under `dataHome` so one
   *  `check_redaction.py --root <dataHome>` scan covers it alongside the log
   *  file (`redaction.spec.ts`). */
  stateDir: string
  logDir: string
  configFile: string
  savedSourceFile: string
}

/** A pure function of the fixed [`RUN_DIR`] — callable from the launcher
 *  process and from any worker, always agreeing, with no IPC. */
export function xdgPaths(): XdgPaths {
  const configHome = join(RUN_DIR, 'xdg-config')
  const dataHome = join(RUN_DIR, 'xdg-data')
  const stateDir = join(dataHome, 'wingspan-state')
  return {
    configHome,
    dataHome,
    stateDir,
    logDir: join(dataHome, APP_ID, 'logs'),
    configFile: join(configHome, 'wingspan', 'wingspan.toml'),
    savedSourceFile: join(stateDir, 'trade_source.json'),
  }
}

/**
 * The environment every launch of the app runs under.
 *
 * `tauri-driver` only *adds* two `TAURI_*_AUTOMATION` variables to what it
 * was given before spawning the native driver, and `WebKitWebDriver` does the
 * same when it spawns the application, so this is what the shipped binary
 * boots with for every session any spec opens.
 *
 * `XDG_CONFIG_HOME`/`XDG_DATA_HOME` are what keep the smoke off the
 * developer's real config, state, logs and window geometry (`dirs::`
 * honours both). `DBUS_SESSION_BUS_ADDRESS` must name a socket that does
 * **not** exist: with no portal running, a reachable session bus makes the
 * app hang in an xdg-desktop-portal call. `just smoke` sets that one (and the
 * two `WEBKIT_` ones) for the wdio process itself; repeating them here means a
 * bare `npm run e2e:wdio` behaves identically.
 */
function launchEnv(): NodeJS.ProcessEnv {
  const xdg = xdgPaths()
  return {
    ...process.env,
    WINGSPAN_AS_OF: AS_OF,
    DBUS_SESSION_BUS_ADDRESS: 'unix:path=/nonexistent/wingspan-no-portal',
    WEBKIT_DISABLE_COMPOSITING_MODE: '1',
    WEBKIT_DISABLE_DMABUF_RENDERER: '1',
    XDG_CONFIG_HOME: xdg.configHome,
    XDG_DATA_HOME: xdg.dataHome,
  }
}

function tomlString(value: string): string {
  return `"${value.replace(/\\/g, '\\\\').replace(/"/g, '\\"')}"`
}

/** Writes `<XDG_CONFIG_HOME>/wingspan/wingspan.toml` pointing `[trades]
 *  csv_path` at `csvPath`. The desktop reads `Layers::user_only()`, so this
 *  file and `WINGSPAN_*` are the only configuration it sees. */
export function writeConfig(csvPath: string): void {
  const xdg = xdgPaths()
  mkdirSync(dirname(xdg.configFile), { recursive: true })
  mkdirSync(xdg.stateDir, { recursive: true })
  writeFileSync(
    xdg.configFile,
    `[trades]\ncsv_path = ${tomlString(csvPath)}\n\n` +
      `[market]\nstate_dir = ${tomlString(xdg.stateDir)}\n\n` +
      `[yfinance]\nenabled = false\n`,
    'utf8',
  )
}

/** Forgets whichever source an earlier spec persisted, so `[trades]
 *  csv_path` alone decides the book — `SavedSourcePolicy::Honour` (the
 *  desktop's default) prefers the saved record over the config file. Also
 *  removes the fetched-bytes cache, belt-and-braces, so a stale cache file
 *  from an earlier spec cannot stand in for a source that was just cleared. */
export function clearSavedSource(): void {
  const xdg = xdgPaths()
  rmSync(xdg.savedSourceFile, { force: true })
  rmSync(join(xdg.stateDir, 'trade_source_cache.csv'), { force: true })
  rmSync(join(xdg.stateDir, 'trade_source_cache.json'), { force: true })
}

/** A fresh, writable copy of the synthetic demo book at [`STAGED_DEMO_BOOK`],
 *  which is what "Load demo book" resolves to given [`DEMO_CWD`] as the
 *  working directory. Copied, never linked — and the repository's own
 *  `fixtures/demo` is only ever read. */
export function stageDemoBook(): string {
  rmSync(DEMO_CWD, { recursive: true, force: true })
  mkdirSync(dirname(STAGED_DEMO_BOOK), { recursive: true })
  cpSync(join(REPO_ROOT, 'fixtures/demo'), STAGED_DEMO_BOOK, { recursive: true, dereference: true })
  return STAGED_DEMO_BOOK
}

/** A fresh copy of `fixtures/trades/legs.csv` at `dest`. Copied, never
 *  linked, and never sourced from `~/workspace/example_trades`. Mirrors
 *  `web/e2e/helpers.ts`. */
export function stageLegsCsv(dest: string): string {
  mkdirSync(dirname(dest), { recursive: true })
  cpSync(join(REPO_ROOT, 'fixtures/trades/legs.csv'), dest, { dereference: true })
  return dest
}

/** Runs `scripts/check_redaction.py --root <root>`, returning its exit code
 *  (0 = clean) without throwing — the caller asserts. */
export function checkRedaction(root: string): { code: number; output: string } {
  try {
    const output = execFileSync('python3', ['scripts/check_redaction.py', '--root', root], {
      cwd: REPO_ROOT,
      encoding: 'utf8',
    })
    return { code: 0, output }
  } catch (error) {
    const err = error as { status?: number; stdout?: string; stderr?: string }
    return { code: err.status ?? 1, output: `${err.stdout ?? ''}${err.stderr ?? ''}` }
  }
}

/** Where `persisted-source.spec.ts` stages its copy of `fixtures/trades/legs.csv`. */
export function persistedSourceCsv(): string {
  return join(RUN_DIR, 'persisted-source', 'persisted-legs.csv')
}

// --------------------------------------------------------------- the app

/** Polls `check` every `intervalMs` until `isDone`, returning the last value
 *  either way — so a failure reports what the UI actually said rather than a
 *  bare wdio timeout. */
export async function pollUntil<T>(
  check: () => Promise<T>,
  isDone: (value: T) => boolean,
  timeoutMs: number,
  intervalMs = 100,
): Promise<T> {
  const deadline = Date.now() + timeoutMs
  let last = await check()
  while (!isDone(last) && Date.now() < deadline) {
    await new Promise((r) => setTimeout(r, intervalMs))
    last = await check()
  }
  return last
}

/** The status bar's trade-source button — present as soon as the SPA has
 *  mounted and `trade_source` has answered, market data or not. */
const SOURCE_BUTTON = 'button[aria-label^="Trade source: "]'

/** What the trade-source button shows while its first query is in flight. */
const PENDING_LABEL = '\u2026'

/** Resolves once the SPA has mounted *and* `trade_source` has answered — the
 *  button renders `…` in between, and a spec that read it then would be
 *  asserting on a spinner. */
export async function waitForApp(): Promise<void> {
  await browser.waitUntil(
    async () => {
      const label = await tradeSourceLabel()
      return label !== null && label !== PENDING_LABEL
    },
    {
      timeout: 40_000,
      timeoutMsg: 'the window never rendered a settled trade-source control',
    },
  )
}

/** The basename the trade-source button is showing, or `null` before it
 *  renders; [`PENDING_LABEL`] while the first query is in flight. */
export async function tradeSourceLabel(): Promise<string | null> {
  return browser.execute((selector: string) => {
    const button = document.querySelector(selector)
    const label = button?.getAttribute('aria-label') ?? null
    return label === null ? null : label.replace(/^Trade source: /, '')
  }, SOURCE_BUTTON)
}

/** The trade-source button's `title`: the full path, the source sentence and
 *  the open/closed counts — everything `trade_source` reports, with no market
 *  data needed. */
export async function tradeSourceSummary(): Promise<string> {
  return browser.execute((selector: string) => {
    return document.querySelector(selector)?.getAttribute('title') ?? ''
  }, SOURCE_BUTTON)
}

/** The trade names the positions table is showing, in one round trip. */
export async function tradeNames(): Promise<string[]> {
  return browser.execute(() =>
    Array.from(document.querySelectorAll('input[aria-label^="Select "]')).map((el) =>
      (el.getAttribute('aria-label') ?? '').replace(/^Select /, ''),
    ),
  )
}

/** The text of the positions row for `name`, or `null` if it is not there. */
export async function tradeRowText(name: string): Promise<string | null> {
  return browser.execute((label: string) => {
    const checkbox = document.querySelector(`input[aria-label="Select ${label}"]`)
    return checkbox?.closest('tr')?.textContent ?? null
  }, name)
}

interface InvokeOutcome {
  done: boolean
  ok: boolean
  error: string | null
}

/**
 * Calls one Tauri command through the same bridge `@tauri-apps/api`'s
 * `invoke` uses (`window.__TAURI_INTERNALS__`, injected regardless of
 * `withGlobalTauri`).
 *
 * Kicked off in one `execute()` and polled for in another rather than run
 * through `executeAsync`: WebdriverIO's async-script bridge is deprecated in
 * v9, and a promise returned from a plain `execute()` serialises as an empty
 * object rather than being awaited.
 */
async function invokeCommand(command: string, args?: Record<string, unknown>): Promise<void> {
  await browser.execute(
    (cmd: string, payload: Record<string, unknown> | null) => {
      const scope = window as unknown as {
        __TAURI_INTERNALS__: { invoke: (c: string, a?: Record<string, unknown>) => Promise<unknown> }
        __wingspanSmokeInvoke?: { done: boolean; ok: boolean; error: string | null }
      }
      scope.__wingspanSmokeInvoke = { done: false, ok: false, error: null }
      scope.__TAURI_INTERNALS__
        .invoke(cmd, payload ?? undefined)
        .then(() => {
          scope.__wingspanSmokeInvoke = { done: true, ok: true, error: null }
        })
        .catch((error: unknown) => {
          scope.__wingspanSmokeInvoke = { done: true, ok: false, error: String(error) }
        })
    },
    command,
    args ?? null,
  )
  const outcome = await pollUntil<InvokeOutcome>(
    () =>
      browser.execute(
        () =>
          (window as unknown as { __wingspanSmokeInvoke?: InvokeOutcome }).__wingspanSmokeInvoke ?? {
            done: false,
            ok: false,
            error: null,
          },
      ),
    (value) => value.done,
    30_000,
  )
  if (!outcome.done) throw new Error(`${command} never answered`)
  if (!outcome.ok) throw new Error(`${command} failed: ${outcome.error ?? 'no reason given'}`)
}

/**
 * Imports a CSV the way the trade-source dialog's "Pick a CSV file…" button
 * does.
 *
 * Not a click: inside the desktop app "Pick a CSV file…" opens the *native*
 * OS chooser (`TradeSourceDialog.tsx`), a window that lives outside the
 * webview and that WebDriver cannot touch. So the spec calls the one command
 * a successful pick leads to, and asserts on everything downstream of it
 * (`state::set_source` → `saved_source::write` → the reload). This is the
 * frozen request envelope `set_trade_source` takes.
 */
export async function setTradeSource(path: string): Promise<void> {
  await invokeCommand('set_trade_source', { request: { source: { kind: 'csv', path } } })
}

/**
 * Clicks through the real first-run path to synthetic prices: the status
 * bar's trade-source button opens `TradeSourceDialog`, whose "Load demo
 * book" button calls `load_demo`. See the module doc comment for why every
 * spec that asserts on the positions table needs this.
 */
export async function loadDemoBook(): Promise<void> {
  await waitForApp()
  const opener = await browser.$(SOURCE_BUTTON)
  await opener.waitForClickable({ timeout: 20_000 })
  await opener.click()

  const demoButton = await browser.$('button=Load demo book')
  await demoButton.waitForClickable({ timeout: 20_000 })
  await demoButton.click()

  // The dialog reports the load and closes itself after a beat; the table is
  // what the specs actually need, so wait for that.
  await browser.waitUntil(async () => (await tradeNames()).length > 0, {
    timeout: 40_000,
    timeoutMsg: 'the demo book never produced a positions table',
  })
}

// ------------------------------------------------------------ the driver

function resolveTauriDriver(): string {
  const cargoBin = join(homedir(), '.cargo', 'bin', 'tauri-driver')
  return existsSync(cargoBin) ? cargoBin : 'tauri-driver'
}

/** Waits for something to accept a TCP connection on `127.0.0.1:port`. */
async function waitForPort(port: number, timeoutMs: number): Promise<void> {
  const deadline = Date.now() + timeoutMs
  for (;;) {
    const ok = await new Promise<boolean>((resolvePromise) => {
      const socket = connect({ host: '127.0.0.1', port }, () => {
        socket.end()
        resolvePromise(true)
      })
      socket.on('error', () => resolvePromise(false))
    })
    if (ok) return
    if (Date.now() > deadline) throw new Error(`tauri-driver never opened port ${port} in ${timeoutMs}ms`)
    await new Promise((r) => setTimeout(r, 200))
  }
}

export type DriverProcess = ChildProcessByStdio<null, Readable, Readable>

/**
 * The **external** provider: `tauri-driver` in front of the system
 * `WebKitWebDriver`, spawned once for the whole run.
 *
 * Spawned here rather than by `@wdio/tauri-service` (a devDependency,
 * deliberately absent from `services`) because that service has no
 * per-launch environment option — see `docs/configuration.md`: `appArgs`
 * exists, `env` does not — and every launch here needs the redirected
 * `XDG_*_HOME`, the pinned clock and the dead D-Bus address of [`launchEnv`],
 * plus [`DEMO_CWD`] as its working directory.
 */
export async function spawnTauriDriver(): Promise<DriverProcess> {
  const child = spawn(
    resolveTauriDriver(),
    [
      `--port=${TAURI_DRIVER_PORT}`,
      `--native-port=${NATIVE_DRIVER_PORT}`,
      `--native-driver=${NATIVE_DRIVER}`,
    ],
    { cwd: DEMO_CWD, env: launchEnv(), stdio: ['ignore', 'pipe', 'pipe'] },
  )
  const chunks: Buffer[] = []
  child.stdout.on('data', (chunk: Buffer) => chunks.push(chunk))
  child.stderr.on('data', (chunk: Buffer) => chunks.push(chunk))
  child.on('exit', (code, signal) => {
    if (code !== null && code !== 0) {
      // eslint-disable-next-line no-console
      console.error(
        `tauri-driver exited early (code ${code}, signal ${signal}):\n${Buffer.concat(chunks).toString('utf8')}`,
      )
    }
  })
  await waitForPort(TAURI_DRIVER_PORT, 20_000)
  return child
}
