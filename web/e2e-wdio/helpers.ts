/**
 * Shared plumbing for the WebDriver bundle smoke suite.
 *
 * Every spec here drives the *shipped* binary (`target/release/wingspan`,
 * built by `just bundle`) through `tauri-driver` + the system
 * `WebKitWebDriver` — the "external" provider (`WINGSPAN-TAURI-PLAN.md` D1,
 * §14). Nothing is linked into the app: there is no `wdio` Cargo feature and
 * no `tauri-plugin-wdio-webdriver`.
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
 * on which *folder* is loaded (`persisted-folder`) read the trades-folder
 * control, which is fed by `trades_root` and needs no market data.
 *
 * `load_demo` resolves the book through `wingspan_app::demo::demo_root()`,
 * whose first candidate is `fixtures/demo` **relative to the working
 * directory** ([`spawnTauriDriver`] sets that, and the whole child chain —
 * tauri-driver → WebKitWebDriver → the app — inherits it). So the demo book
 * the specs drive is a byte copy of `fixtures/demo` staged under
 * [`DEMO_CWD`], which is what makes `live-reload.spec.ts` able to edit it: the
 * repository's own `fixtures/demo` is a tracked path and is never written to.
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
import { appendFileSync, cpSync, existsSync, mkdirSync, readFileSync, renameSync, rmSync, writeFileSync } from 'node:fs'
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
export const DEMO_CWD = join(RUN_DIR, 'demo-cwd')
export const STAGED_DEMO_BOOK = join(DEMO_CWD, 'fixtures', 'demo')

/** `tauri.conf.json`'s `identifier` — where `dirs::data_dir()` puts the app's
 *  own tree under a redirected `XDG_DATA_HOME`. */
export const APP_ID = 'com.jbolinge.wingspan'

/** Pin "today", so DTE/staleness read the same on every run. Deliberately the
 *  same day as `wingspan_app::demo::valuation_date()`, so clicking "Load demo
 *  book" (which pins the clock itself) does not move the calendar. */
export const AS_OF = '2026-08-31'

export const TAURI_DRIVER_PORT = 4444
export const NATIVE_DRIVER_PORT = 4445
export const NATIVE_DRIVER = '/usr/bin/WebKitWebDriver'

export interface XdgPaths {
  configHome: string
  dataHome: string
  /** `[market] state_dir` — nested under `dataHome` so one
   *  `check_redaction.py --root <dataHome>` scan covers it alongside the log
   *  file (`redaction.spec.ts`). */
  stateDir: string
  logDir: string
  configFile: string
  savedRootFile: string
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
    savedRootFile: join(stateDir, 'trades_root.json'),
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
export function launchEnv(): NodeJS.ProcessEnv {
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

/** Writes `<XDG_CONFIG_HOME>/wingspan/wingspan.toml` pointing `[trades] root`
 *  at `root`. The desktop reads `Layers::user_only()`, so this file and
 *  `WINGSPAN_*` are the only configuration it sees. */
export function writeConfig(root: string): void {
  const xdg = xdgPaths()
  mkdirSync(dirname(xdg.configFile), { recursive: true })
  mkdirSync(xdg.stateDir, { recursive: true })
  writeFileSync(
    xdg.configFile,
    `[trades]\nroot = ${tomlString(root)}\n\n` +
      `[market]\nstate_dir = ${tomlString(xdg.stateDir)}\n\n` +
      `[yfinance]\nenabled = false\n`,
    'utf8',
  )
}

/** Forgets whichever folder an earlier spec persisted, so `[trades] root`
 *  alone decides the book — `SavedRootPolicy::Honour` (the desktop's default)
 *  prefers the saved record over the config file. */
export function clearSavedRoot(): void {
  rmSync(xdgPaths().savedRootFile, { force: true })
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

/** A fresh copy of `fixtures/trades` at `dest`, with `Open Trades/M1 July 14
 *  2026` renamed so the loaded book is tellable from the real fixtures
 *  directory. Never symlinked, and never sourced from
 *  `~/workspace/example_trades`. Mirrors `web/e2e/helpers.ts`. */
export function stageTradesFolder(dest: string, label = 'Staged'): string {
  rmSync(dest, { recursive: true, force: true })
  mkdirSync(dest, { recursive: true })
  cpSync(join(REPO_ROOT, 'fixtures/trades'), dest, { recursive: true, dereference: true })
  const openDir = join(dest, 'Open Trades')
  renameSync(join(openDir, 'M1 July 14 2026'), join(openDir, `${label} July 14 2026`))
  return dest
}

/** Appends a copy of a trade file's own last order line, doubling that
 *  order's quantity. The line is read back out of the file, so it is
 *  guaranteed to still parse as a thinkorswim order. */
export function appendDuplicateOrderLine(file: string): void {
  const lines = readFileSync(file, 'utf8').split('\n').filter((line) => line.trim().length > 0)
  appendFileSync(file, `${lines[lines.length - 1]}\n`, 'utf8')
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

/** Where `persisted-folder.spec.ts` stages its copy of `fixtures/trades`. */
export function persistedFolderTradesDir(): string {
  return join(RUN_DIR, 'persisted-folder-trades')
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

/** The status bar's trades-folder button — present as soon as the SPA has
 *  mounted and `trades_root` has answered, market data or not. */
const ROOT_BUTTON = 'button[aria-label^="Trades folder: "]'

/** What the trades-folder button shows while its first query is in flight. */
const PENDING_LABEL = '\u2026'

/** Resolves once the SPA has mounted *and* `trades_root` has answered — the
 *  button renders `…` in between, and a spec that read it then would be
 *  asserting on a spinner. */
export async function waitForApp(): Promise<void> {
  await browser.waitUntil(
    async () => {
      const label = await tradesFolderLabel()
      return label !== null && label !== PENDING_LABEL
    },
    {
      timeout: 40_000,
      timeoutMsg: 'the window never rendered a settled trades-folder control',
    },
  )
}

/** The basename the trades-folder button is showing, or `null` before it
 *  renders; [`PENDING_LABEL`] while the first query is in flight. */
export async function tradesFolderLabel(): Promise<string | null> {
  return browser.execute((selector: string) => {
    const button = document.querySelector(selector)
    const label = button?.getAttribute('aria-label') ?? null
    return label === null ? null : label.replace(/^Trades folder: /, '')
  }, ROOT_BUTTON)
}

/** The trades-folder button's `title`: the full path, the source sentence and
 *  the open/closed counts — everything `trades_root` reports, with no market
 *  data needed. */
export async function tradesFolderSummary(): Promise<string> {
  return browser.execute((selector: string) => {
    return document.querySelector(selector)?.getAttribute('title') ?? ''
  }, ROOT_BUTTON)
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
export async function invokeCommand(command: string, args?: Record<string, unknown>): Promise<void> {
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
 * Picks a folder the way the trades dialog's "Load & analyze" button does.
 *
 * Not a click: inside the desktop app "Choose folder…" opens the *native* OS
 * chooser (`TradesFolderDialog.tsx`), a window that lives outside the webview
 * and that WebDriver cannot touch, and the plain path textbox next to it only
 * renders when `isTauri()` is false — never in this binary. So the spec calls
 * the one command that click would have reached, and asserts on everything
 * downstream of it (`state::set_root` → `saved_root::write` → the reload).
 */
export async function setTradesRoot(root: string): Promise<void> {
  await invokeCommand('set_trades_root', { request: { root } })
}

/**
 * Clicks through the real first-run path to synthetic prices: the status
 * bar's trades-folder button opens `TradesFolderDialog`, whose "Load demo
 * book" button calls `load_demo`. See the module doc comment for why every
 * spec that asserts on the positions table needs this.
 */
export async function loadDemoBook(): Promise<void> {
  await waitForApp()
  const opener = await browser.$(ROOT_BUTTON)
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
 * Spawned here rather than by `@wdio/tauri-service` (a devDependency, pinned
 * per the plan, but deliberately absent from `services`) because that service
 * has no per-launch environment option — see `docs/configuration.md`:
 * `appArgs` exists, `env` does not — and every launch here needs the
 * redirected `XDG_*_HOME`, the pinned clock and the dead D-Bus address of
 * [`launchEnv`], plus [`DEMO_CWD`] as its working directory. The task's own
 * instructions name this as the sanctioned fallback.
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
