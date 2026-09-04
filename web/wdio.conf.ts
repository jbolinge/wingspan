/**
 * The WebDriver bundle smoke suite (P7-BUNDLE-SMOKE / `WINGSPAN-TAURI-PLAN.md`
 * §14, D1): drives the *shipped* binary (`target/release/wingspan`, from
 * `just bundle`) from outside, through the **external** provider —
 * `tauri-driver` + the system `WebKitWebDriver`. Nothing is linked into the
 * app: there is no `wdio` Cargo feature and no `tauri-plugin-wdio-webdriver`
 * (D1), so nothing here could use one.
 *
 * `@wdio/tauri-service` is a devDependency (pinned per the plan) but is
 * deliberately **not** in `services`: even its `external` mode has no way to
 * hand the spawned app an environment, and every launch here needs redirected
 * `XDG_*_HOME` (so the smoke never touches a developer's real config, state,
 * logs or window geometry), a pinned clock, a working directory, and a D-Bus
 * address that points at nothing. `helpers.ts`'s `spawnTauriDriver` does that
 * job instead — the fallback the task's instructions sanction — and the child
 * chain (tauri-driver → WebKitWebDriver → the app) carries all of it down
 * with no extra plumbing, because neither hop replaces the environment or the
 * working directory it was given.
 *
 * One session per spec file, each a fresh launch of the app;
 * `browser.reloadSession()` inside a spec (`persisted-folder.spec.ts`) buys a
 * second one the same way, against the same `XDG_*_HOME`, which is what makes
 * "it is still loaded after a restart" mean anything.
 */

import { existsSync, mkdirSync, rmSync } from 'node:fs'
import { join } from 'node:path'
import {
  type DriverProcess,
  REPO_ROOT,
  RELEASE_BINARY,
  RUN_DIR,
  TAURI_DRIVER_PORT,
  clearSavedRoot,
  spawnTauriDriver,
  stageDemoBook,
  writeConfig,
  xdgPaths,
} from './e2e-wdio/helpers.ts'

/** What `[trades] root` points at for every spec's first launch. The repo's
 *  own demo book, read-only — the *editable* copy the "Load demo book"
 *  button resolves to is staged separately (`helpers.ts`). */
const DEMO_ROOT = join(REPO_ROOT, 'fixtures/demo')

let driver: DriverProcess | undefined

/** Spec files already given their starting state in this worker process.
 *  `beforeSession` also fires for `browser.reloadSession()`, and re-running
 *  the setup there would wipe the persisted record `persisted-folder.spec.ts`
 *  restarts the app to prove. */
const configured = new Set<string>()

export const config: WebdriverIO.Config = {
  runner: 'local',
  specs: [
    './e2e-wdio/window.spec.ts',
    './e2e-wdio/no-mock.spec.ts',
    './e2e-wdio/persisted-folder.spec.ts',
    './e2e-wdio/live-reload.spec.ts',
    // Last on purpose: it scans the XDG_DATA_HOME every spec above has been
    // writing logs and state into.
    './e2e-wdio/redaction.spec.ts',
  ],
  maxInstances: 1,

  // tauri-driver, spawned by `onPrepare` below.
  hostname: '127.0.0.1',
  port: TAURI_DRIVER_PORT,
  path: '/',
  protocol: 'http',

  capabilities: [
    {
      // tauri-driver reads `tauri:options` itself and rewrites it into the
      // `webkitgtk:browserOptions` WebKitWebDriver understands, so no
      // `browserName` is wanted here.
      'tauri:options': { application: RELEASE_BINARY },
    } as WebdriverIO.Capabilities,
  ],

  logLevel: 'warn',
  waitforTimeout: 20_000,
  connectionRetryTimeout: 120_000,
  connectionRetryCount: 2,

  framework: 'mocha',
  reporters: ['spec'],
  mochaOpts: {
    ui: 'bdd',
    timeout: 180_000,
  },

  onPrepare: async function onPrepare() {
    if (!existsSync(RELEASE_BINARY)) {
      throw new Error(
        `${RELEASE_BINARY} does not exist — run \`just bundle\` first. This suite drives the ` +
          'shipped release binary, not a debug build (a debug build without `custom-protocol` ' +
          'loads http://localhost:5173 and every spec sees a blank window).',
      )
    }
    rmSync(RUN_DIR, { recursive: true, force: true })
    const xdg = xdgPaths()
    mkdirSync(xdg.configHome, { recursive: true })
    mkdirSync(xdg.stateDir, { recursive: true })
    stageDemoBook()
    driver = await spawnTauriDriver()
  },

  onComplete: async function onComplete() {
    driver?.kill()
  },

  beforeSession: async function beforeSession(_config, _capabilities, specs) {
    const spec = specs[0] ?? ''
    if (configured.has(spec)) return
    configured.add(spec)
    clearSavedRoot()
    writeConfig(DEMO_ROOT)
  },
}
