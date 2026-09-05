import { invoke } from '@tauri-apps/api/core'
import { screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { DiagnosticsResponse } from '../api/types'
import { renderWithClient, resetAppStore } from '../test/utils'
import { DiagnosticsCard } from './DiagnosticsCard'

function enterTauri(): void {
  ;(window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {}
}
function leaveTauri(): void {
  delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__
}

/** The cheap, static-only checklist — the card's first, unprobed load. */
const STATIC: DiagnosticsResponse = {
  rows: [
    { grade: 'ok', section: 'Config', name: 'wingspan.toml', detail: 'parsed cleanly' },
    { grade: 'warn', section: 'Config', name: 'trades root', detail: 'not set' },
    { grade: 'note', section: 'Providers', name: 'schwab', detail: 'not probed yet' },
  ],
}

/** What "Run checks" (`probe: true`) turns up — a live failure. */
const PROBED: DiagnosticsResponse = {
  rows: [
    STATIC.rows[0]!,
    STATIC.rows[1]!,
    { grade: 'fail', section: 'Providers', name: 'schwab', detail: 'connection refused' },
  ],
}

function mockDiagnostics(): void {
  vi.mocked(invoke).mockImplementation(async (cmd: string, args?: unknown) => {
    if (cmd === 'diagnostics') {
      const probe = (args as { request: { probe: boolean } }).request.probe
      return probe ? PROBED : STATIC
    }
    return null
  })
}

beforeEach(() => {
  resetAppStore()
  enterTauri()
  vi.mocked(invoke).mockReset()
  mockDiagnostics()
})

afterEach(() => {
  leaveTauri()
})

describe('DiagnosticsCard', () => {
  it('renders rows grouped by section, in the order Rust returned them, with the right badges', async () => {
    renderWithClient(<DiagnosticsCard />)

    const items = await screen.findAllByRole('listitem')
    expect(items).toHaveLength(3)
    expect(items[0]).toHaveTextContent('wingspan.toml')
    expect(items[0]).toHaveTextContent('ok')
    expect(items[1]).toHaveTextContent('trades root')
    expect(items[1]).toHaveTextContent('warn')
    expect(items[2]).toHaveTextContent('schwab')
    expect(screen.getByText('Config')).toBeInTheDocument()
    expect(screen.getByText('Providers')).toBeInTheDocument()
  })

  it('Run checks re-queries with probe: true', async () => {
    renderWithClient(<DiagnosticsCard />)
    await screen.findAllByRole('listitem')
    vi.mocked(invoke).mockClear()

    await userEvent.setup().click(screen.getByRole('button', { name: 'Run checks' }))

    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith('diagnostics', { request: { probe: true } }),
    )
  })

  it('renders the read-only note outside Tauri and calls nothing', () => {
    leaveTauri()
    renderWithClient(<DiagnosticsCard />)

    expect(
      screen.getByText(
        'Settings are editable in the Wingspan desktop app; this browser view is read-only.',
      ),
    ).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: 'Run checks' })).not.toBeInTheDocument()
    expect(invoke).not.toHaveBeenCalled()
  })
})
