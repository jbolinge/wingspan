/**
 * The API client: one entry point, `call()`, over two transports.
 *
 * Inside the desktop app every request is a Tauri command (`invoke`); in the
 * browser — the Vite devserver and the Playwright runs behind it — the same
 * call becomes the `fetch` it has always been, against `/api/v1`. The mock
 * layer outranks both: `VITE_MOCK=1` forces it, and in dev the client latches
 * into it when the local dev API server is unreachable, so the SPA stays
 * usable. Errors surface as `ApiError` carrying the RFC 9457 problem document
 * whichever transport produced them.
 */

import { invoke } from '@tauri-apps/api/core'
import { isTauri } from '../lib/tauri'
import { MockNotFound, MockProblem, mockRequest } from './mock'
import type { ProblemJson } from './types'

export const API_BASE = '/api/v1'

export class ApiError extends Error {
  readonly status: number
  readonly problem: ProblemJson | null

  constructor(message: string, status: number, problem: ProblemJson | null) {
    super(message)
    this.name = 'ApiError'
    this.status = status
    this.problem = problem
  }
}

/**
 * The one sentence to show for a failed command: `problem+json`'s `detail`
 * when the transport gave us one, and whatever the throw carried otherwise.
 */
export function errorDetail(error: unknown): string {
  if (error instanceof ApiError) return error.problem?.detail ?? error.message
  return error instanceof Error ? error.message : String(error)
}

/**
 * The problem's slug — the last segment of `type`
 * (`https://wingspan.invalid/problems/<slug>`), or null when the throw carried
 * no problem document. The trade-source dialog keys its extra hint off it.
 */
export function errorSlug(error: unknown): string | null {
  if (!(error instanceof ApiError)) return null
  const type = error.problem?.type
  if (typeof type !== 'string') return null
  return type.split('/').pop() ?? null
}

type MockMode = 'off' | 'forced' | 'fallback'

function initialMode(): MockMode {
  const flag = import.meta.env.VITE_MOCK
  return flag === '1' || flag === 'true' ? 'forced' : 'off'
}

let mockMode: MockMode = initialMode()
const listeners = new Set<(mode: MockMode) => void>()

export function getMockMode(): MockMode {
  return mockMode
}

export function isMockActive(): boolean {
  return mockMode !== 'off'
}

/** Test seam — lets a test drive the client into (or out of) mock mode. */
export function setMockMode(mode: MockMode): void {
  if (mockMode === mode) return
  mockMode = mode
  for (const listener of listeners) listener(mode)
}

export function onMockModeChange(listener: (mode: MockMode) => void): () => void {
  listeners.add(listener)
  return () => listeners.delete(listener)
}

function serveMock<T>(path: string, init?: RequestInit): T {
  try {
    return mockRequest(path, init) as T
  } catch (error) {
    if (error instanceof MockProblem) {
      throw new ApiError(error.problem.detail ?? error.message, error.problem.status ?? 500, error.problem)
    }
    if (error instanceof MockNotFound) {
      throw new ApiError(error.message, 404, {
        title: 'Not Found',
        status: 404,
        detail: error.message,
      })
    }
    throw error
  }
}

async function readProblem(response: Response): Promise<ProblemJson | null> {
  try {
    const text = await response.text()
    if (!text) return null
    const parsed: unknown = JSON.parse(text)
    return typeof parsed === 'object' && parsed !== null ? (parsed as ProblemJson) : null
  } catch {
    return null
  }
}

/**
 * `path` is relative to `/api/v1`, e.g. `/trades` or `/portfolio?ids=a,b`.
 */
export async function apiFetch<T>(path: string, init?: RequestInit): Promise<T> {
  if (mockMode !== 'off') return serveMock<T>(path, init)

  let response: Response
  try {
    response = await fetch(`${API_BASE}${path}`, {
      ...init,
      headers: {
        Accept: 'application/json',
        ...(init?.body ? { 'Content-Type': 'application/json' } : {}),
        ...init?.headers,
      },
    })
  } catch (error) {
    // A request cancelled by an AbortSignal (a superseded risk-graph query,
    // an unmounted component) is not a network failure — let it propagate so
    // TanStack Query can quietly drop it instead of latching mock mode.
    if (error instanceof DOMException && error.name === 'AbortError') throw error
    // Only the browser dev flow has an unreachable API to fall back from;
    // inside Tauri there is no proxy and no devserver, so a failure is real.
    if (import.meta.env.DEV && !isTauri()) {
      setMockMode('fallback')
      return serveMock<T>(path, init)
    }
    throw new ApiError(
      error instanceof Error ? error.message : 'network error',
      0,
      null,
    )
  }

  // The Vite proxy answers 502/504 when nothing is listening on 8787.
  if (import.meta.env.DEV && !isTauri() && (response.status === 502 || response.status === 504)) {
    setMockMode('fallback')
    return serveMock<T>(path, init)
  }

  if (!response.ok) {
    const problem = await readProblem(response)
    throw new ApiError(
      problem?.detail ?? problem?.title ?? `${response.status} ${response.statusText}`,
      response.status,
      problem,
    )
  }

  if (response.status === 204) return undefined as T

  return (await response.json()) as T
}

/* ------------------------------------------------------------------ *
 * Commands
 * ------------------------------------------------------------------ */

/**
 * The HTTP shape of a command, for the browser transport and the mock — whose
 * router (`mock.ts`) still speaks paths and methods.
 */
function commandToRequest(command: string, args?: Record<string, unknown>): [string, RequestInit] {
  const request = (args?.request ?? {}) as Record<string, unknown>
  const get: RequestInit = { method: 'GET' }
  const post = (body: unknown): RequestInit => ({ method: 'POST', body: JSON.stringify(body) })
  switch (command) {
    case 'market_snapshot':
      return ['/market/snapshot', get]
    case 'app_config':
      return ['/config', get]
    case 'reconciliation':
      return ['/reconciliation', get]
    case 'warnings':
      return ['/warnings', get]
    case 'trades':
      return ['/trades', get]
    case 'trades_closed':
      return ['/trades/closed', get]
    // Always the POST form: it carries `model_legs`, and one code path in the
    // browser beats choosing between it and `GET /portfolio?ids=` per call.
    case 'portfolio':
      return ['/portfolio', post({ ids: request.ids ?? [], model_legs: request.model_legs ?? [] })]
    case 'chain_expirations':
      return ['/chain/expirations', get]
    case 'option_chain':
      return ['/chain', post(request)]
    case 'set_model_contracts':
      return ['/model/contracts', { method: 'PUT', body: JSON.stringify(request) }]
    case 'model_orders':
      return ['/model/orders', post({ model_legs: request.model_legs ?? [] })]
    case 'risk_graph':
      return ['/risk-graph', post(request)]
    case 'what_if':
      return ['/what-if', post(request)]
    case 'reload':
      return ['/reload', post({})]
    case 'trade_source':
      return ['/trades/source', get]
    case 'probe_trade_source':
      return ['/trades/source/probe', post({ source: request.source })]
    case 'set_trade_source':
      return ['/trades/source', { method: 'PUT', body: JSON.stringify({ source: request.source }) }]
    case 'clear_trade_source':
      return ['/trades/source', { method: 'DELETE' }]
    default:
      throw new Error(`unknown command ${command}`)
  }
}

/**
 * Commands with no HTTP equivalent: the OS file choosers, the clipboard, and
 * the settings, diagnostics, demo-book and desktop-polish commands. None of
 * these have a route in `wingspan-devserver` or a case in `mock.ts` — they
 * only ever run inside the desktop app.
 */
const DESKTOP_ONLY = new Set([
  'pick_csv_file',
  'pick_python',
  'pick_env_file',
  'pick_token_file',
  'pick_tastytrade_env_file',
  'settings_read',
  'settings_update',
  'probe_python',
  'diagnostics',
  'load_demo',
  'leave_demo',
  'reveal_folder',
  'open_sheet',
  'open_log_folder',
  'open_config_file',
  'check_for_updates',
  // The browser writes the clipboard itself (`lib/clipboard.ts`); inside
  // the webview `navigator.clipboard` cannot be relied on, so Rust does.
  'copy_text',
])

/** Generation counter for `risk_graph`; never a field of the request. */
let riskGraphSeq = 0

/**
 * `invoke` has no `AbortSignal`, so the JS side stops waiting while Rust runs
 * the request to completion — intended: a superseded `risk_graph` returns its
 * answer to a caller that has already dropped it, rather than erroring.
 */
export function raceAbort<T>(promise: Promise<T>, signal?: AbortSignal): Promise<T> {
  if (!signal) return promise
  if (signal.aborted) {
    promise.catch(() => {})
    return Promise.reject(new DOMException('aborted', 'AbortError'))
  }
  let onAbort: (() => void) | null = null
  const aborted = new Promise<never>((_resolve, reject) => {
    onAbort = () => reject(new DOMException('aborted', 'AbortError'))
    signal.addEventListener('abort', onAbort, { once: true })
  })
  // Whichever of the two loses the race must not surface as an unhandled
  // rejection once nobody is listening any more.
  promise.catch(() => {})
  aborted.catch(() => {})
  return Promise.race([promise, aborted]).finally(() => {
    if (onAbort) signal.removeEventListener('abort', onAbort)
  })
}

/**
 * What a rejected `invoke` means. A command that failed gives back the same
 * problem+json document the HTTP transport sent; a refused (or misspelled,
 * or wrongly-typed) command gives back a bare string.
 */
function toApiError(raw: unknown): Error {
  if (raw instanceof DOMException) return raw
  if (raw instanceof Error && raw.name === 'AbortError') return raw
  if (typeof raw === 'object' && raw !== null) {
    const candidate = raw as Record<string, unknown>
    const problemShaped =
      ('status' in candidate && 'detail' in candidate) ||
      ('type' in candidate && 'title' in candidate)
    if (problemShaped) {
      const p = raw as ProblemJson
      return new ApiError(p.detail ?? p.title ?? `${p.status ?? 500}`, p.status ?? 500, p)
    }
  }
  return new ApiError(String(raw), 0, null)
}

function invokeCommand<T>(
  command: string,
  payload: Record<string, unknown> | undefined,
  signal?: AbortSignal,
): Promise<T> {
  return raceAbort(
    invoke<T>(command, payload).catch((raw: unknown) => {
      throw toApiError(raw)
    }),
    signal,
  )
}

/**
 * Run one command: a Tauri `invoke` in the desktop app, the equivalent HTTP
 * request in the browser, and the mock in front of both.
 */
export async function call<T>(
  command: string,
  args?: Record<string, unknown>,
  signal?: AbortSignal,
): Promise<T> {
  // These commands have neither a route nor a mock; they only ever run
  // inside the desktop app.
  if (DESKTOP_ONLY.has(command)) {
    if (!isTauri()) throw new Error(`${command} is only available in the desktop app`)
    return invokeCommand<T>(command, args, signal)
  }

  const [path, init] = commandToRequest(command, args)
  if (mockMode !== 'off') return serveMock<T>(path, init)

  if (isTauri()) {
    const payload = command === 'risk_graph' ? { ...args, seq: ++riskGraphSeq } : args
    return invokeCommand<T>(command, payload, signal)
  }

  return apiFetch<T>(path, { ...init, signal })
}
