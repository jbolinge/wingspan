/**
 * The `Settings` card in Data health (desktop only, D16). Every writable
 * field is a typed group of `SettingsPatch` — Trades, Market data, Pricing,
 * Risk graph — plus the read-only paths `settings_read` reports alongside
 * them (the folder, the token/credentials files, the interpreter). Save
 * sends only the groups/fields the trader actually touched, so a comment or
 * an unrelated key in `wingspan.toml` survives untouched.
 *
 * Outside the desktop app there is no `settings_read`/`settings_update` —
 * no HTTP route exists and none is added — so the card renders the same
 * read-only note `DiagnosticsCard` does, and nothing else.
 */

import type { UseMutationResult } from '@tanstack/react-query'
import { useEffect, useId, useState, type ReactNode } from 'react'
import { ApiError } from '../api/client'
import {
  usePickEnvFile,
  usePickPython,
  usePickTastytradeEnvFile,
  usePickTokenFile,
  useProbePython,
  useRevealFolder,
  useSettings,
  useTradeSource,
  useUpdateSettings,
} from '../api/queries'
import type { PickedPath, SettingsPatch } from '../api/types'
import { isTauri } from '../lib/tauri'
import { useUiSignals } from '../lib/uiSignals'
import { Card } from './ui'

const READ_ONLY_NOTE =
  'Settings are editable in the Wingspan desktop app; this browser view is read-only.'

/* ---------- the editable shape, one bag of primitives per group ---------- */

interface TradesDraft {
  tab: string
  poll_secs: number
  timezone: string
}
interface SchwabDraft {
  token_config_key: string
  app_key_env: string
  app_secret_env: string
  refresh: boolean
}
interface TastytradeDraft {
  enabled: boolean
  client_id_env: string
  client_secret_env: string
  refresh_token_env: string
  sandbox: boolean
  delayed: boolean
  api_version: string
}
interface YfinanceDraft {
  enabled: boolean
}
interface MarketDraft {
  refresh_secs_open: number
  refresh_secs_closed: number
  pause_when_hidden: boolean
}
interface PricingDraft {
  risk_free_rate: number
  dividend_yield: number
  weighted_vega_reference: string
  weighted_vega_exponent: number
}
interface RiskGraphDraft {
  points: number
  range_pct: number
  tplus_preset: string
}
interface Draft {
  trades: TradesDraft
  schwab: SchwabDraft
  tastytrade: TastytradeDraft
  yfinance: YfinanceDraft
  market: MarketDraft
  pricing: PricingDraft
  risk_graph: RiskGraphDraft
}

const EMPTY_DRAFT: Draft = {
  trades: {
    tab: '',
    poll_secs: 0,
    timezone: '',
  },
  schwab: { token_config_key: '', app_key_env: '', app_secret_env: '', refresh: false },
  tastytrade: {
    enabled: false,
    client_id_env: '',
    client_secret_env: '',
    refresh_token_env: '',
    sandbox: false,
    delayed: false,
    api_version: '',
  },
  yfinance: { enabled: false },
  market: { refresh_secs_open: 0, refresh_secs_closed: 0, pause_when_hidden: false },
  pricing: {
    risk_free_rate: 0,
    dividend_yield: 0,
    weighted_vega_reference: '',
    weighted_vega_exponent: 0,
  },
  risk_graph: { points: 0, range_pct: 0, tplus_preset: '' },
}

function draftFromPatch(settings: SettingsPatch | undefined): Draft {
  if (!settings) return EMPTY_DRAFT
  return {
    trades: { ...EMPTY_DRAFT.trades, ...settings.trades },
    schwab: { ...EMPTY_DRAFT.schwab, ...settings.schwab },
    tastytrade: { ...EMPTY_DRAFT.tastytrade, ...settings.tastytrade },
    yfinance: { ...EMPTY_DRAFT.yfinance, ...settings.yfinance },
    market: { ...EMPTY_DRAFT.market, ...settings.market },
    pricing: { ...EMPTY_DRAFT.pricing, ...settings.pricing },
    risk_graph: { ...EMPTY_DRAFT.risk_graph, ...settings.risk_graph },
  }
}

/** Builds a patch with exactly the fields the trader touched — never a
 *  wholesale re-send of every group, which would clobber a comment or a
 *  field this form does not know about. */
function buildPatch(draft: Draft, dirty: ReadonlySet<string>): SettingsPatch {
  const patch: SettingsPatch = {}

  const trades: NonNullable<SettingsPatch['trades']> = {}
  if (dirty.has('trades.tab')) trades.tab = draft.trades.tab
  if (dirty.has('trades.poll_secs')) trades.poll_secs = draft.trades.poll_secs
  if (dirty.has('trades.timezone')) trades.timezone = draft.trades.timezone
  if (Object.keys(trades).length > 0) patch.trades = trades

  const schwab: NonNullable<SettingsPatch['schwab']> = {}
  if (dirty.has('schwab.token_config_key')) schwab.token_config_key = draft.schwab.token_config_key
  if (dirty.has('schwab.app_key_env')) schwab.app_key_env = draft.schwab.app_key_env
  if (dirty.has('schwab.app_secret_env')) schwab.app_secret_env = draft.schwab.app_secret_env
  if (dirty.has('schwab.refresh')) schwab.refresh = draft.schwab.refresh
  if (Object.keys(schwab).length > 0) patch.schwab = schwab

  const tastytrade: NonNullable<SettingsPatch['tastytrade']> = {}
  if (dirty.has('tastytrade.enabled')) tastytrade.enabled = draft.tastytrade.enabled
  if (dirty.has('tastytrade.client_id_env')) tastytrade.client_id_env = draft.tastytrade.client_id_env
  if (dirty.has('tastytrade.client_secret_env'))
    tastytrade.client_secret_env = draft.tastytrade.client_secret_env
  if (dirty.has('tastytrade.refresh_token_env'))
    tastytrade.refresh_token_env = draft.tastytrade.refresh_token_env
  if (dirty.has('tastytrade.sandbox')) tastytrade.sandbox = draft.tastytrade.sandbox
  if (dirty.has('tastytrade.delayed')) tastytrade.delayed = draft.tastytrade.delayed
  if (dirty.has('tastytrade.api_version')) tastytrade.api_version = draft.tastytrade.api_version
  if (Object.keys(tastytrade).length > 0) patch.tastytrade = tastytrade

  if (dirty.has('yfinance.enabled')) patch.yfinance = { enabled: draft.yfinance.enabled }

  const market: NonNullable<SettingsPatch['market']> = {}
  if (dirty.has('market.refresh_secs_open')) market.refresh_secs_open = draft.market.refresh_secs_open
  if (dirty.has('market.refresh_secs_closed'))
    market.refresh_secs_closed = draft.market.refresh_secs_closed
  if (dirty.has('market.pause_when_hidden')) market.pause_when_hidden = draft.market.pause_when_hidden
  if (Object.keys(market).length > 0) patch.market = market

  const pricing: NonNullable<SettingsPatch['pricing']> = {}
  if (dirty.has('pricing.risk_free_rate')) pricing.risk_free_rate = draft.pricing.risk_free_rate
  if (dirty.has('pricing.dividend_yield')) pricing.dividend_yield = draft.pricing.dividend_yield
  if (dirty.has('pricing.weighted_vega_reference'))
    pricing.weighted_vega_reference = draft.pricing.weighted_vega_reference
  if (dirty.has('pricing.weighted_vega_exponent'))
    pricing.weighted_vega_exponent = draft.pricing.weighted_vega_exponent
  if (Object.keys(pricing).length > 0) patch.pricing = pricing

  const riskGraph: NonNullable<SettingsPatch['risk_graph']> = {}
  if (dirty.has('risk_graph.points')) riskGraph.points = draft.risk_graph.points
  if (dirty.has('risk_graph.range_pct')) riskGraph.range_pct = draft.risk_graph.range_pct
  if (dirty.has('risk_graph.tplus_preset')) riskGraph.tplus_preset = draft.risk_graph.tplus_preset
  if (Object.keys(riskGraph).length > 0) patch.risk_graph = riskGraph

  return patch
}

/** The parent directory of a path, splitting on either `/` or `\`. */
function parentDir(path: string): string {
  const cut = path.search(/[\\/][^\\/]*$/)
  return cut < 0 ? path : path.slice(0, cut) || '/'
}

function errorDetail(error: unknown): string {
  if (error instanceof ApiError) return error.problem?.detail ?? error.message
  return error instanceof Error ? error.message : String(error)
}

/* ---------- small field primitives ---------- */

function TextField({
  label,
  value,
  onChange,
  width = '14rem',
}: {
  label: string
  value: string
  onChange: (value: string) => void
  width?: string
}) {
  return (
    <label className="flex items-center gap-1.5">
      <span className="ws-label w-32 shrink-0">{label}</span>
      <input
        type="text"
        className="ws-input"
        style={{ width }}
        value={value}
        onChange={(event) => onChange(event.target.value)}
      />
    </label>
  )
}

/** A number input that reports only numbers.
 *
 *  An emptied field is *unchanged*, not zero: `Number('')` is `0`, which is
 *  finite, so passing it through would mark the field dirty and Save would
 *  write it. A `points` of 0 breaks every risk-graph request and a
 *  `refresh_secs_open` of 0 asks for the fastest polling the app can do — and
 *  clearing a box before typing a new number is what everyone does. Half-typed
 *  input (`-`, `1e`) is `NaN` and ignored the same way. */
function NumField({
  label,
  value,
  onChange,
  step = 1,
  min,
}: {
  label: string
  value: number
  onChange: (value: number) => void
  step?: number
  min?: number
}) {
  return (
    <label className="flex items-center gap-1.5">
      <span className="ws-label w-32 shrink-0">{label}</span>
      <input
        type="number"
        className="ws-input num"
        style={{ width: '7rem' }}
        step={step}
        min={min}
        value={value}
        onChange={(event) => {
          const text = event.target.value.trim()
          if (text === '') return
          const parsed = Number(text)
          if (Number.isFinite(parsed)) onChange(parsed)
        }}
      />
    </label>
  )
}

function CheckField({
  label,
  checked,
  onChange,
}: {
  label: string
  checked: boolean
  onChange: (checked: boolean) => void
}) {
  return (
    <label className="flex items-center gap-1.5">
      <input
        type="checkbox"
        checked={checked}
        onChange={(event) => onChange(event.target.checked)}
      />
      <span className="text-[12px] text-fg">{label}</span>
    </label>
  )
}

function ReadOnlyPath({
  label,
  value,
  onBrowse,
  browseLabel = 'Browse…',
  extra,
}: {
  label: string
  value: string | null | undefined
  onBrowse?: () => void
  browseLabel?: string
  extra?: ReactNode
}) {
  return (
    <div className="flex flex-wrap items-center gap-1.5">
      <span className="ws-label w-32 shrink-0">{label}</span>
      <span className="num flex-1 truncate text-[12px] text-fg" title={value ?? undefined}>
        {value ?? '—'}
      </span>
      {onBrowse ? (
        <button type="button" className="ws-btn" onClick={onBrowse}>
          {browseLabel}
        </button>
      ) : null}
      {extra}
    </div>
  )
}

/**
 * What a picker did, when it did not do all of it.
 *
 * A picker writes `wingspan.toml` and then rebuilds the application on it,
 * and the two can disagree — a credentials file this process cannot read is
 * written happily and refused by the rebuild. The old settings stay in force,
 * so `settings_read` redraws the *old* path: without this line the chooser
 * would look like it silently did nothing.
 */
function PickerNote({ picker }: { picker: PickerResult }) {
  if (picker.isError) {
    return (
      <p role="alert" className="m-0 pl-[8.5rem] text-[11px] text-neg">
        {errorDetail(picker.error)}
      </p>
    )
  }
  const picked = picker.data
  if (!picked || picked.applied) return null
  return (
    <p role="alert" className="m-0 pl-[8.5rem] text-[11px] text-warn">
      saved to your settings file, but this session is still using the previous value —{' '}
      {picked.detail ?? 'the application could not be rebuilt'}
    </p>
  )
}

type PickerResult = UseMutationResult<PickedPath | null, Error, void>

export function SettingsCard() {
  const tauri = isTauri()
  const settings = useSettings()
  const source = useTradeSource()
  const updateSettings = useUpdateSettings()
  const pickPython = usePickPython()
  const pickEnvFile = usePickEnvFile()
  const pickTokenFile = usePickTokenFile()
  const pickTastytradeEnvFile = usePickTastytradeEnvFile()
  const probePython = useProbePython()
  const revealFolder = useRevealFolder()

  const [draft, setDraft] = useState<Draft>(EMPTY_DRAFT)
  const [dirty, setDirty] = useState<Set<string>>(new Set())
  const [savedFile, setSavedFile] = useState<string | null>(null)
  const advancedId = useId()

  // Re-seed the draft whenever fresh data lands and nothing is unsaved —
  // a background refetch must never clobber an in-progress edit, but the
  // first load, and the refetch after a successful Save, both should land.
  useEffect(() => {
    if (dirty.size > 0) return
    if (settings.data) setDraft(draftFromPatch(settings.data.settings))
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [settings.data])

  const touch = (key: string): void => setDirty((prev) => new Set(prev).add(key))

  const paths = settings.data?.paths
  const tokenStatus = settings.data?.token_status ?? null
  const tastytradeStatus = settings.data?.tastytrade_status ?? null
  const sourceStatus = settings.data?.source_status ?? null
  const csvSourcePath = source.data?.kind === 'csv' ? source.data.path : null
  const isCsvSource = csvSourcePath !== null
  const configDir = settings.data?.file ? parentDir(settings.data.file) : null
  const revealTarget = csvSourcePath !== null ? parentDir(csvSourcePath) : configDir

  const save = (): void => {
    setSavedFile(null)
    const patch = buildPatch(draft, dirty)
    updateSettings.mutate(patch, {
      onSuccess: (data) => {
        setSavedFile(data.file)
        setDirty(new Set())
      },
    })
  }

  if (!tauri) {
    return (
      <Card title="Settings">
        <p className="m-0 px-2.5 py-2 text-[11px] text-muted">{READ_ONLY_NOTE}</p>
      </Card>
    )
  }

  return (
    <Card title="Settings">
      <div className="space-y-3 px-2.5 py-2 text-[12px]">
        <fieldset className="m-0 space-y-1.5 border-0 p-0">
          <legend className="ws-label mb-1 font-semibold text-fg-strong">Trades</legend>
          <ReadOnlyPath
            label="Source"
            value={paths?.source_label ?? null}
            onBrowse={() => useUiSignals.getState().emit('open-source-dialog')}
            browseLabel="Import trades…"
            extra={
              <button
                type="button"
                className="ws-btn"
                disabled={!revealTarget}
                title={
                  isCsvSource
                    ? undefined
                    : 'A sheet has no folder to reveal; this opens the folder holding wingspan.toml.'
                }
                onClick={() => revealTarget && revealFolder.mutate(revealTarget)}
              >
                Reveal
              </button>
            }
          />
          <p className="m-0 pl-[8.5rem] text-[11px] text-muted">
            {sourceStatus?.configured
              ? sourceStatus.host
                ? `Sheet host: ${sourceStatus.host}${
                    sourceStatus.tab ? ` · tab ${sourceStatus.tab}` : ''
                  }`
                : 'Source configured'
              : 'No trade source configured'}
          </p>
          <details id={advancedId}>
            <summary className="ws-label cursor-pointer select-none">Advanced</summary>
            <div className="mt-1.5 space-y-1.5 pl-1">
              <TextField
                label="Tab"
                value={draft.trades.tab}
                onChange={(v) => {
                  setDraft((d) => ({ ...d, trades: { ...d.trades, tab: v } }))
                  touch('trades.tab')
                }}
              />
              <NumField
                label="Poll secs"
                value={draft.trades.poll_secs}
                onChange={(v) => {
                  setDraft((d) => ({ ...d, trades: { ...d.trades, poll_secs: v } }))
                  touch('trades.poll_secs')
                }}
              />
              <TextField
                label="Timezone"
                value={draft.trades.timezone}
                onChange={(v) => {
                  setDraft((d) => ({ ...d, trades: { ...d.trades, timezone: v } }))
                  touch('trades.timezone')
                }}
              />
            </div>
          </details>
        </fieldset>

        <fieldset className="m-0 space-y-1.5 border-0 border-t border-line pt-2">
          <legend className="ws-label mb-1 font-semibold text-fg-strong">Market data</legend>
          <ReadOnlyPath
            label="Token file"
            value={paths?.token_path}
            onBrowse={() => pickTokenFile.mutate()}
          />
          <PickerNote picker={pickTokenFile} />
          {tokenStatus ? (
            tokenStatus.present && tokenStatus.world_readable ? (
              <p className="m-0 pl-[8.5rem] text-[11px] text-warn">
                the token file is readable by other users on this machine — its owner should
                tighten it; Wingspan never changes its permissions
              </p>
            ) : (
              <p className="m-0 pl-[8.5rem] text-[11px] text-muted">{tokenStatus.detail}</p>
            )
          ) : null}
          <ReadOnlyPath
            label="Credentials file"
            value={paths?.env_file}
            onBrowse={() => pickEnvFile.mutate()}
          />
          <PickerNote picker={pickEnvFile} />
          <TextField
            label="Token config key"
            value={draft.schwab.token_config_key}
            onChange={(v) => {
              setDraft((d) => ({ ...d, schwab: { ...d.schwab, token_config_key: v } }))
              touch('schwab.token_config_key')
            }}
          />
          <TextField
            label="App key env"
            value={draft.schwab.app_key_env}
            onChange={(v) => {
              setDraft((d) => ({ ...d, schwab: { ...d.schwab, app_key_env: v } }))
              touch('schwab.app_key_env')
            }}
          />
          <TextField
            label="App secret env"
            value={draft.schwab.app_secret_env}
            onChange={(v) => {
              setDraft((d) => ({ ...d, schwab: { ...d.schwab, app_secret_env: v } }))
              touch('schwab.app_secret_env')
            }}
          />
          <CheckField
            label="Let Wingspan refresh the Schwab access token itself"
            checked={draft.schwab.refresh}
            onChange={(v) => {
              setDraft((d) => ({ ...d, schwab: { ...d.schwab, refresh: v } }))
              touch('schwab.refresh')
            }}
          />
          <CheckField
            label="yfinance enabled"
            checked={draft.yfinance.enabled}
            onChange={(v) => {
              setDraft((d) => ({ ...d, yfinance: { enabled: v } }))
              touch('yfinance.enabled')
            }}
          />
          <ReadOnlyPath
            label="Interpreter"
            value={paths?.python}
            onBrowse={() => pickPython.mutate()}
            extra={
              <button
                type="button"
                className="ws-btn"
                disabled={probePython.isPending}
                onClick={() => probePython.mutate()}
              >
                {probePython.isPending ? 'Testing…' : 'Test'}
              </button>
            }
          />
          <PickerNote picker={pickPython} />
          {probePython.data ? (
            <p className={`m-0 pl-[8.5rem] text-[11px] ${probePython.data.ok ? 'text-pos' : 'text-warn'}`}>
              {probePython.data.detail}
            </p>
          ) : probePython.isError ? (
            <p className="m-0 pl-[8.5rem] text-[11px] text-warn">{errorDetail(probePython.error)}</p>
          ) : null}
          {/* tastytrade lives inside Market data beside Schwab and yfinance:
              this is the fieldset that holds the providers, and a top-level
              legend for one of the three would be the odd one out. Only
              variable *names* are editable here — the credentials file itself
              is a read-only path a Rust picker writes, and no control on this
              card ever accepts a secret value. */}
          <p className="ws-label mt-2 border-t border-line pt-2 font-semibold text-fg-strong">
            tastytrade
          </p>
          <CheckField
            label="tastytrade enabled"
            checked={draft.tastytrade.enabled}
            onChange={(v) => {
              setDraft((d) => ({ ...d, tastytrade: { ...d.tastytrade, enabled: v } }))
              touch('tastytrade.enabled')
            }}
          />
          <ReadOnlyPath
            label="Credentials file"
            value={paths?.tastytrade_env_file}
            onBrowse={() => pickTastytradeEnvFile.mutate()}
          />
          <PickerNote picker={pickTastytradeEnvFile} />
          {tastytradeStatus ? (
            <p
              className={`m-0 pl-[8.5rem] text-[11px] ${
                tastytradeStatus.authorized ? 'text-muted' : 'text-warn'
              }`}
            >
              {tastytradeStatus.detail}
            </p>
          ) : null}
          <TextField
            label="Client secret env"
            value={draft.tastytrade.client_secret_env}
            onChange={(v) => {
              setDraft((d) => ({ ...d, tastytrade: { ...d.tastytrade, client_secret_env: v } }))
              touch('tastytrade.client_secret_env')
            }}
          />
          <TextField
            label="Refresh token env"
            value={draft.tastytrade.refresh_token_env}
            onChange={(v) => {
              setDraft((d) => ({ ...d, tastytrade: { ...d.tastytrade, refresh_token_env: v } }))
              touch('tastytrade.refresh_token_env')
            }}
          />
          <TextField
            label="Client id env"
            value={draft.tastytrade.client_id_env}
            onChange={(v) => {
              setDraft((d) => ({ ...d, tastytrade: { ...d.tastytrade, client_id_env: v } }))
              touch('tastytrade.client_id_env')
            }}
          />
          <TextField
            label="API version"
            value={draft.tastytrade.api_version}
            onChange={(v) => {
              setDraft((d) => ({ ...d, tastytrade: { ...d.tastytrade, api_version: v } }))
              touch('tastytrade.api_version')
            }}
          />
          <CheckField
            label="Use the tastytrade sandbox (api.cert.tastyworks.com — no market data there)"
            checked={draft.tastytrade.sandbox}
            onChange={(v) => {
              setDraft((d) => ({ ...d, tastytrade: { ...d.tastytrade, sandbox: v } }))
              touch('tastytrade.sandbox')
            }}
          />
          <CheckField
            label="This tastytrade account gets delayed quotes"
            checked={draft.tastytrade.delayed}
            onChange={(v) => {
              setDraft((d) => ({ ...d, tastytrade: { ...d.tastytrade, delayed: v } }))
              touch('tastytrade.delayed')
            }}
          />
          <NumField
            label="Refresh secs (open)"
            min={1}
            value={draft.market.refresh_secs_open}
            onChange={(v) => {
              setDraft((d) => ({ ...d, market: { ...d.market, refresh_secs_open: v } }))
              touch('market.refresh_secs_open')
            }}
          />
          <NumField
            label="Refresh secs (closed)"
            min={1}
            value={draft.market.refresh_secs_closed}
            onChange={(v) => {
              setDraft((d) => ({ ...d, market: { ...d.market, refresh_secs_closed: v } }))
              touch('market.refresh_secs_closed')
            }}
          />
          <CheckField
            label="Pause price refreshes while the window is minimised or hidden"
            checked={draft.market.pause_when_hidden}
            onChange={(v) => {
              setDraft((d) => ({ ...d, market: { ...d.market, pause_when_hidden: v } }))
              touch('market.pause_when_hidden')
            }}
          />
        </fieldset>

        <fieldset className="m-0 space-y-1.5 border-0 border-t border-line pt-2">
          <legend className="ws-label mb-1 font-semibold text-fg-strong">Pricing</legend>
          <NumField
            label="Risk-free rate"
            value={draft.pricing.risk_free_rate}
            step={0.001}
            onChange={(v) => {
              setDraft((d) => ({ ...d, pricing: { ...d.pricing, risk_free_rate: v } }))
              touch('pricing.risk_free_rate')
            }}
          />
          <NumField
            label="Dividend yield"
            value={draft.pricing.dividend_yield}
            step={0.001}
            onChange={(v) => {
              setDraft((d) => ({ ...d, pricing: { ...d.pricing, dividend_yield: v } }))
              touch('pricing.dividend_yield')
            }}
          />
          <TextField
            label="Weighted vega ref"
            value={draft.pricing.weighted_vega_reference}
            onChange={(v) => {
              setDraft((d) => ({ ...d, pricing: { ...d.pricing, weighted_vega_reference: v } }))
              touch('pricing.weighted_vega_reference')
            }}
          />
          <NumField
            label="Weighted vega exp"
            value={draft.pricing.weighted_vega_exponent}
            step={0.1}
            onChange={(v) => {
              setDraft((d) => ({ ...d, pricing: { ...d.pricing, weighted_vega_exponent: v } }))
              touch('pricing.weighted_vega_exponent')
            }}
          />
        </fieldset>

        <fieldset className="m-0 space-y-1.5 border-0 border-t border-line pt-2">
          <legend className="ws-label mb-1 font-semibold text-fg-strong">Risk graph</legend>
          <NumField
            label="Points"
            min={2}
            value={draft.risk_graph.points}
            onChange={(v) => {
              setDraft((d) => ({ ...d, risk_graph: { ...d.risk_graph, points: v } }))
              touch('risk_graph.points')
            }}
          />
          <NumField
            label="Range %"
            value={draft.risk_graph.range_pct}
            step={0.01}
            min={0.01}
            onChange={(v) => {
              setDraft((d) => ({ ...d, risk_graph: { ...d.risk_graph, range_pct: v } }))
              touch('risk_graph.range_pct')
            }}
          />
          <TextField
            label="T+ preset"
            value={draft.risk_graph.tplus_preset}
            onChange={(v) => {
              setDraft((d) => ({ ...d, risk_graph: { ...d.risk_graph, tplus_preset: v } }))
              touch('risk_graph.tplus_preset')
            }}
          />
        </fieldset>

        <div className="flex items-center gap-2 border-t border-line pt-2">
          <button
            type="button"
            className="ws-btn"
            disabled={updateSettings.isPending || dirty.size === 0}
            onClick={save}
          >
            {updateSettings.isPending ? 'Saving…' : 'Save'}
          </button>
          {savedFile ? <span className="text-[11px] text-pos">✓ saved to {savedFile}</span> : null}
          {updateSettings.isError ? (
            <span role="alert" className="text-[11px] text-neg">
              {errorDetail(updateSettings.error)}
            </span>
          ) : null}
        </div>
      </div>
    </Card>
  )
}
