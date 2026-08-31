/**
 * Thin fetch client for `/api/v1`.
 *
 * Errors surface as `ApiError` carrying the RFC 9457 problem document when the
 * server sent one. In dev, if the API is unreachable (no `wingspan serve`
 * behind the Vite proxy) the client latches into mock mode so the SPA stays
 * usable; `VITE_MOCK=1` forces it from the start.
 */

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
        // A multipart body carries its own boundary in the Content-Type the
        // browser writes: setting one by hand would corrupt every part.
        ...(init?.body && !(init.body instanceof FormData)
          ? { 'Content-Type': 'application/json' }
          : {}),
        ...init?.headers,
      },
    })
  } catch (error) {
    if (import.meta.env.DEV) {
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
  if (import.meta.env.DEV && (response.status === 502 || response.status === 504)) {
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

  // A misconfigured deployment can answer /api with the SPA's index.html.
  // Fail loudly instead of throwing an opaque JSON parse error.
  const contentType = response.headers.get('content-type') ?? ''
  if (!contentType.includes('json')) {
    throw new ApiError(
      `expected JSON from ${API_BASE}${path} but got "${contentType || 'no content-type'}"`,
      response.status,
      null,
    )
  }
  return (await response.json()) as T
}

export function apiGet<T>(path: string): Promise<T> {
  return apiFetch<T>(path, { method: 'GET' })
}

export function apiPost<T>(path: string, body: unknown): Promise<T> {
  return apiFetch<T>(path, { method: 'POST', body: JSON.stringify(body) })
}

export function apiPut<T>(path: string, body: unknown): Promise<T> {
  return apiFetch<T>(path, { method: 'PUT', body: JSON.stringify(body) })
}

export function apiDelete<T>(path: string): Promise<T> {
  return apiFetch<T>(path, { method: 'DELETE' })
}

/**
 * `multipart/form-data` POST — the browser sets the boundary itself, so no
 * `Content-Type` goes near it. Used by the device upload.
 */
export function apiUpload<T>(path: string, form: FormData): Promise<T> {
  return apiFetch<T>(path, { method: 'POST', body: form })
}
