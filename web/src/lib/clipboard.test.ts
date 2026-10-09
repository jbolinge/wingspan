/**
 * `copyText` picks its transport: the `copy_text` command inside the desktop
 * app — the mock included, which has no clipboard to stand in for — and
 * `navigator.clipboard` in the browser. A failure is the caller's to show.
 */

import { invoke } from '@tauri-apps/api/core'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { ApiError, setMockMode } from '../api/client'
import { copyText } from './clipboard'

function enterTauri(): void {
  ;(window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {}
}

function leaveTauri(): void {
  delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__
}

/** jsdom has no `navigator.clipboard`; stand one in (or take it away). */
function stubClipboard(clipboard: Partial<Clipboard> | undefined): void {
  Object.defineProperty(navigator, 'clipboard', { value: clipboard, configurable: true })
}

const LINE = 'SELL -2 IRON CONDOR SPX 100 20 NOV 26 [AM] 8400/8500/7000/6900 CALL/PUT @22.20 LMT'

beforeEach(() => {
  vi.mocked(invoke).mockReset()
})

afterEach(() => {
  leaveTauri()
  stubClipboard(undefined)
  setMockMode('forced')
})

describe('copyText in the browser', () => {
  it('writes through navigator.clipboard and never invokes', async () => {
    const writeText = vi.fn(async () => {})
    stubClipboard({ writeText })
    await copyText(LINE)
    expect(writeText).toHaveBeenCalledWith(LINE)
    expect(invoke).not.toHaveBeenCalled()
  })

  it('passes a refusal on', async () => {
    stubClipboard({ writeText: vi.fn(async () => Promise.reject(new Error('Document is not focused.'))) })
    await expect(copyText(LINE)).rejects.toThrow('Document is not focused.')
  })

  // A plain-http LAN origin has no clipboard API at all.
  it('says so where there is no clipboard API', async () => {
    stubClipboard(undefined)
    await expect(copyText(LINE)).rejects.toThrow('the clipboard is not available in this browser')
  })
})

describe('copyText in the desktop app', () => {
  beforeEach(() => enterTauri())

  it('sends copy_text with the { request: { text } } envelope, not navigator.clipboard', async () => {
    setMockMode('off')
    vi.mocked(invoke).mockResolvedValue(null)
    const writeText = vi.fn(async () => {})
    stubClipboard({ writeText })
    await copyText(LINE)
    expect(invoke).toHaveBeenCalledWith('copy_text', { request: { text: LINE } })
    expect(writeText).not.toHaveBeenCalled()
  })

  // The mock answers the data commands; the clipboard is still the real one.
  it('still reaches Rust with the mock on', async () => {
    setMockMode('forced')
    vi.mocked(invoke).mockResolvedValue(null)
    await copyText(LINE)
    expect(invoke).toHaveBeenCalledWith('copy_text', { request: { text: LINE } })
  })

  // A Wayland session with no XWayland: Rust's X11 clipboard has nowhere to
  // go, the webview's (GTK's) still does.
  it('falls back to navigator.clipboard when the command fails', async () => {
    setMockMode('off')
    vi.mocked(invoke).mockRejectedValue({ status: 500, detail: 'no clipboard owner on this display' })
    const writeText = vi.fn(async () => {})
    stubClipboard({ writeText })
    await copyText(LINE)
    expect(invoke).toHaveBeenCalledWith('copy_text', { request: { text: LINE } })
    expect(writeText).toHaveBeenCalledWith(LINE)
  })

  it('rejects with the command’s problem when the webview cannot help either', async () => {
    setMockMode('off')
    vi.mocked(invoke).mockRejectedValue({ status: 500, detail: 'no clipboard owner on this display' })
    stubClipboard({ writeText: vi.fn(async () => Promise.reject(new Error('NotAllowedError'))) })
    const error = await copyText(LINE).catch((e: unknown) => e)
    expect(error).toBeInstanceOf(ApiError)
    expect((error as ApiError).message).toBe('no clipboard owner on this display')
  })

  it('rejects with the command’s problem when the webview has no clipboard API', async () => {
    setMockMode('off')
    vi.mocked(invoke).mockRejectedValue({ status: 500, detail: 'no clipboard owner on this display' })
    stubClipboard(undefined)
    const error = await copyText(LINE).catch((e: unknown) => e)
    expect(error).toBeInstanceOf(ApiError)
    expect((error as ApiError).message).toBe('no clipboard owner on this display')
  })
})
