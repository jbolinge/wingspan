import { invoke } from '@tauri-apps/api/core'
import { fireEvent, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { SettingsRead } from '../api/types'
import { renderWithClient, resetAppStore } from '../test/utils'
import { SettingsCard } from './SettingsCard'

function enterTauri(): void {
  ;(window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {}
}
function leaveTauri(): void {
  delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__
}

const FAKE_SETTINGS: SettingsRead = {
  file: '/home/trader/.config/wingspan/wingspan.toml',
  settings: {
    trades: {
      open_dir: 'Open Trades',
      closed_dir: 'Closed Trades',
      statements_dir: 'Daily Trade Statement',
      statement_timezone: 'America/Chicago',
      watch_debounce_ms: 750,
      watch_poll_secs: 0,
    },
    schwab: {
      token_config_key: 'paths.token_file',
      app_key_env: 'SCHWAB_APP_KEY',
      app_secret_env: 'SCHWAB_APP_SECRET',
      refresh: true,
    },
    yfinance: { enabled: true },
    market: { refresh_secs_open: 15, refresh_secs_closed: 300, pause_when_hidden: false },
    pricing: {
      risk_free_rate: 0.045,
      dividend_yield: 0.012,
      weighted_vega_reference: 'front_expiry',
      weighted_vega_exponent: 1,
    },
    risk_graph: { points: 241, range_pct: 0.12, tplus_preset: 'quarters' },
  },
  paths: {
    trades_root: '/home/trader/Trades',
    token_path: '/home/trader/.config/wingspan/schwab_token.json',
    env_file: '/home/trader/.config/wingspan/.env',
    python: '/usr/bin/python3',
  },
  token_status: { present: true, world_readable: false, detail: 'permissions look fine (0600)' },
}

/** Every command resolves per the base fixture unless `cmd` is in `rejects`,
 *  in which case `invoke` rejects with that value (a problem document or a
 *  bare string, matching the two shapes `toApiError` handles). */
function mockSettings(rejects: Partial<Record<string, unknown>> = {}): void {
  vi.mocked(invoke).mockImplementation(async (cmd: string) => {
    if (cmd in rejects) throw rejects[cmd]
    if (cmd === 'settings_read') return FAKE_SETTINGS
    if (cmd === 'settings_update') return { file: FAKE_SETTINGS.file }
    return null
  })
}

beforeEach(() => {
  resetAppStore()
  enterTauri()
  vi.mocked(invoke).mockReset()
  mockSettings()
})

afterEach(() => {
  leaveTauri()
})

describe('SettingsCard', () => {
  it('renders the four groups', async () => {
    renderWithClient(<SettingsCard />)

    await screen.findByText('Trades')
    expect(screen.getByText('Market data')).toBeInTheDocument()
    expect(screen.getByText('Pricing')).toBeInTheDocument()
    expect(screen.getByText('Risk graph')).toBeInTheDocument()
  })

  it('a changed field produces a patch containing only that field', async () => {
    renderWithClient(<SettingsCard />)
    const rateInput = await screen.findByLabelText('Risk-free rate')
    await waitFor(() => expect(rateInput).toHaveValue(0.045))

    fireEvent.change(rateInput, { target: { value: '0.05' } })
    await userEvent.setup().click(screen.getByRole('button', { name: 'Save' }))

    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith('settings_update', {
        request: { pricing: { risk_free_rate: 0.05 } },
      }),
    )
  })

  it('shows the saved file path on success', async () => {
    renderWithClient(<SettingsCard />)
    const rateInput = await screen.findByLabelText('Risk-free rate')
    fireEvent.change(rateInput, { target: { value: '0.05' } })

    await userEvent.setup().click(screen.getByRole('button', { name: 'Save' }))

    expect(
      await screen.findByText((content) => content.includes(FAKE_SETTINGS.file as string)),
    ).toBeInTheDocument()
  })

  it('shows the server sentence as an alert on a failed save', async () => {
    mockSettings({
      settings_update: { status: 422, title: 'Unprocessable Entity', detail: 'bad risk-free rate' },
    })
    renderWithClient(<SettingsCard />)
    const rateInput = await screen.findByLabelText('Risk-free rate')
    fireEvent.change(rateInput, { target: { value: '0.05' } })

    await userEvent.setup().click(screen.getByRole('button', { name: 'Save' }))

    const alert = await screen.findByRole('alert')
    expect(alert).toHaveTextContent('bad risk-free rate')
  })

  it('renders the read-only note and nothing else outside Tauri', () => {
    leaveTauri()
    renderWithClient(<SettingsCard />)

    expect(
      screen.getByText(
        'Settings are editable in the Wingspan desktop app; this browser view is read-only.',
      ),
    ).toBeInTheDocument()
    expect(screen.queryByLabelText('Risk-free rate')).not.toBeInTheDocument()
    expect(screen.queryByRole('button', { name: 'Save' })).not.toBeInTheDocument()
    expect(invoke).not.toHaveBeenCalled()
  })

  it('clearing a number field is unchanged, not zero', async () => {
    renderWithClient(<SettingsCard />)
    const points = await screen.findByLabelText('Points')
    await waitFor(() => expect(points).toHaveValue(241))

    // Clearing the box before typing is what everyone does; `Number('')` is 0,
    // and a `points` of 0 makes every risk-graph request answer TooFewPoints.
    fireEvent.change(points, { target: { value: '' } })
    // A second field, changed for real, so Save has something to send.
    fireEvent.change(await screen.findByLabelText('Risk-free rate'), { target: { value: '0.05' } })
    await userEvent.setup().click(screen.getByRole('button', { name: 'Save' }))

    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith('settings_update', {
        request: { pricing: { risk_free_rate: 0.05 } },
      }),
    )
    const patches = vi
      .mocked(invoke)
      .mock.calls.filter(([cmd]) => cmd === 'settings_update')
      .map(([, args]) => args)
    expect(JSON.stringify(patches)).not.toContain('points')
  })

  it('says so when a picker saved the path but could not apply it', async () => {
    // `pick_env_file` writes `wingspan.toml` and then rebuilds the app on it;
    // a credentials file the process cannot read is written happily and
    // refused by the rebuild. `settings_read` then redraws the *old* path, so
    // without this line the chooser looks like it silently did nothing.
    vi.mocked(invoke).mockImplementation(async (cmd: string) => {
      if (cmd === 'settings_read') return FAKE_SETTINGS
      if (cmd === 'pick_env_file')
        return {
          path: '/home/trader/secrets/.env',
          applied: false,
          detail: 'the settings file could not be re-read: permission denied',
        }
      return null
    })
    renderWithClient(<SettingsCard />)
    await screen.findByText('Market data')

    const browse = screen.getAllByRole('button', { name: 'Browse…' })
    await userEvent.setup().click(browse[1] as HTMLElement)

    const alert = await screen.findByRole('alert')
    expect(alert).toHaveTextContent('still using the previous value')
    expect(alert).toHaveTextContent('permission denied')
  })

  it('a picker that applied cleanly says nothing at all', async () => {
    vi.mocked(invoke).mockImplementation(async (cmd: string) => {
      if (cmd === 'settings_read') return FAKE_SETTINGS
      if (cmd === 'pick_env_file')
        return { path: '/home/trader/secrets/.env', applied: true, detail: null }
      return null
    })
    renderWithClient(<SettingsCard />)
    await screen.findByText('Market data')

    const browse = screen.getAllByRole('button', { name: 'Browse…' })
    await userEvent.setup().click(browse[1] as HTMLElement)

    await waitFor(() => expect(invoke).toHaveBeenCalledWith('pick_env_file', undefined))
    expect(screen.queryByRole('alert')).not.toBeInTheDocument()
  })

  it('a failed python probe renders a warning and never blocks Save', async () => {
    mockSettings({ probe_python: 'python3 not found on PATH' })
    renderWithClient(<SettingsCard />)
    const rateInput = await screen.findByLabelText('Risk-free rate')
    fireEvent.change(rateInput, { target: { value: '0.05' } })

    await userEvent.setup().click(screen.getByRole('button', { name: 'Test' }))

    expect(await screen.findByText('python3 not found on PATH')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Save' })).toBeEnabled()
  })
})
