/**
 * The `Settings` card in Data health (desktop only, D16). Every writable
 * field is a typed group of `SettingsPatch` — Trades, Market data, Pricing,
 * Risk graph — plus the read-only paths `settings_read` reports alongside
 * them (the folder, the token/credentials files, the interpreter). Save
 * sends only the groups/fields the trader actually touched, so a comment or
 * an unrelated key in `wingspan.toml` survives untouched.
 *
 * Outside the desktop app there is no `settings_read`/`settings_update` —
 * no HTTP route exists and none is added — so every control renders
 * disabled, seeded from the already-fetched `useConfig()` read-only config
 * instead.
 */

import { useEffect, useId, useState, type ReactNode } from 'react'
import { ApiError } from '../api/client'
import {
  useConfig,
  usePickEnvFile,
  usePickPython,
  usePickTokenFile,
  useProbePython,
  useRevealFolder,
  useSettings,
  useUpdateSettings,
} from '../api/queries'
import type { ConfigResponse, SettingsPatch } from '../api/types'
import { isTauri } from '../lib/tauri'
import { useUiSignals } from '../lib/uiSignals'
import { Card } from './ui'

const READ_ONLY_NOTE =
  'Settings are editable in the Wingspan desktop app; this browser view is read-only.'

/* ---------- the editable shape, one bag of primitives per group ---------- */

interface TradesDraft {
  open_dir: string
  closed_dir: string
  statements_dir: string
  statement_timezone: string
  watch_debounce_ms: number
  watch_poll_secs: number
}
interface SchwabDraft {
  token_config_key: string
  app_key_env: string
  app_secret_env: string
  refresh: boolean
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
  yfinance: YfinanceDraft
  market: MarketDraft
  pricing: PricingDraft
  risk_graph: RiskGraphDraft
}

const EMPTY_DRAFT: Draft = {
  trades: {
    open_dir: '',
    closed_dir: '',
    statements_dir: '',
    statement_timezone: '',
    watch_debounce_ms: 0,
    watch_poll_secs: 0,
  },
  schwab: { token_config_key: '', app_key_env: '', app_secret_env: '', refresh: false },
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
    yfinance: { ...EMPTY_DRAFT.yfinance, ...settings.yfinance },
    market: { ...EMPTY_DRAFT.market, ...settings.market },
    pricing: { ...EMPTY_DRAFT.pricing, ...settings.pricing },
    risk_graph: { ...EMPTY_DRAFT.risk_graph, ...settings.risk_graph },
  }
}

/** Best-effort reading of the browser's read-only `useConfig()` shape, which
 *  is deliberately open (`ConfigResponse`) rather than `SettingsPatch`. */
function get(obj: unknown, ...path: string[]): unknown {
  let cur: unknown = obj
  for (const key of path) {
    if (typeof cur !== 'object' || cur === null) return undefined
    cur = (cur as Record<string, unknown>)[key]
  }
  return cur
}
const asStr = (v: unknown): string => (typeof v === 'string' ? v : v == null ? '' : String(v))
const asNum = (v: unknown): number => (typeof v === 'number' ? v : 0)
const asBool = (v: unknown): boolean => typeof v === 'boolean' && v

function draftFromConfig(config: ConfigResponse | undefined): Draft {
  return {
    trades: {
      open_dir: asStr(get(config, 'trades', 'open_dir')),
      closed_dir: asStr(get(config, 'trades', 'closed_dir')),
      statements_dir: asStr(get(config, 'trades', 'statements_dir')),
      statement_timezone: asStr(get(config, 'trades', 'statement_timezone')),
      watch_debounce_ms: asNum(get(config, 'trades', 'watch_debounce_ms')),
      watch_poll_secs: asNum(get(config, 'trades', 'watch_poll_secs')),
    },
    schwab: {
      token_config_key: asStr(get(config, 'schwab', 'token_config_key')),
      app_key_env: asStr(get(config, 'schwab', 'app_key_env')),
      app_secret_env: asStr(get(config, 'schwab', 'app_secret_env')),
      refresh: asBool(get(config, 'schwab', 'refresh')),
    },
    yfinance: { enabled: asBool(get(config, 'yfinance', 'enabled')) },
    market: {
      refresh_secs_open: asNum(get(config, 'market', 'refresh_secs_open')),
      refresh_secs_closed: asNum(get(config, 'market', 'refresh_secs_closed')),
      pause_when_hidden: asBool(get(config, 'market', 'pause_when_hidden')),
    },
    pricing: {
      risk_free_rate: asNum(get(config, 'pricing', 'risk_free_rate')),
      dividend_yield: asNum(get(config, 'pricing', 'dividend_yield')),
      weighted_vega_reference: asStr(get(config, 'pricing', 'weighted_vega', 'reference')),
      weighted_vega_exponent: asNum(get(config, 'pricing', 'weighted_vega', 'exponent')),
    },
    risk_graph: {
      points: asNum(get(config, 'risk_graph', 'points')),
      range_pct: asNum(get(config, 'risk_graph', 'range_pct')),
      tplus_preset: asStr(get(config, 'risk_graph', 'tplus_preset')),
    },
  }
}

/** Builds a patch with exactly the fields the trader touched — never a
 *  wholesale re-send of every group, which would clobber a comment or a
 *  field this form does not know about. */
function buildPatch(draft: Draft, dirty: ReadonlySet<string>): SettingsPatch {
  const patch: SettingsPatch = {}

  const trades: NonNullable<SettingsPatch['trades']> = {}
  if (dirty.has('trades.open_dir')) trades.open_dir = draft.trades.open_dir
  if (dirty.has('trades.closed_dir')) trades.closed_dir = draft.trades.closed_dir
  if (dirty.has('trades.statements_dir')) trades.statements_dir = draft.trades.statements_dir
  if (dirty.has('trades.statement_timezone')) trades.statement_timezone = draft.trades.statement_timezone
  if (dirty.has('trades.watch_debounce_ms')) trades.watch_debounce_ms = draft.trades.watch_debounce_ms
  if (dirty.has('trades.watch_poll_secs')) trades.watch_poll_secs = draft.trades.watch_poll_secs
  if (Object.keys(trades).length > 0) patch.trades = trades

  const schwab: NonNullable<SettingsPatch['schwab']> = {}
  if (dirty.has('schwab.token_config_key')) schwab.token_config_key = draft.schwab.token_config_key
  if (dirty.has('schwab.app_key_env')) schwab.app_key_env = draft.schwab.app_key_env
  if (dirty.has('schwab.app_secret_env')) schwab.app_secret_env = draft.schwab.app_secret_env
  if (dirty.has('schwab.refresh')) schwab.refresh = draft.schwab.refresh
  if (Object.keys(schwab).length > 0) patch.schwab = schwab

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

function errorDetail(error: unknown): string {
  if (error instanceof ApiError) return error.problem?.detail ?? error.message
  return error instanceof Error ? error.message : String(error)
}

/* ---------- small field primitives, all disable-able ---------- */

function TextField({
  label,
  value,
  disabled,
  onChange,
  width = '14rem',
}: {
  label: string
  value: string
  disabled: boolean
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
        disabled={disabled}
        onChange={(event) => onChange(event.target.value)}
      />
    </label>
  )
}

function NumField({
  label,
  value,
  disabled,
  onChange,
  step = 1,
}: {
  label: string
  value: number
  disabled: boolean
  onChange: (value: number) => void
  step?: number
}) {
  return (
    <label className="flex items-center gap-1.5">
      <span className="ws-label w-32 shrink-0">{label}</span>
      <input
        type="number"
        className="ws-input num"
        style={{ width: '7rem' }}
        step={step}
        value={value}
        disabled={disabled}
        onChange={(event) => {
          const parsed = Number(event.target.value)
          if (Number.isFinite(parsed)) onChange(parsed)
        }}
      />
    </label>
  )
}

function CheckField({
  label,
  checked,
  disabled,
  onChange,
}: {
  label: string
  checked: boolean
  disabled: boolean
  onChange: (checked: boolean) => void
}) {
  return (
    <label className="flex items-center gap-1.5">
      <input
        type="checkbox"
        checked={checked}
        disabled={disabled}
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
  disabled,
}: {
  label: string
  value: string | null | undefined
  onBrowse?: () => void
  browseLabel?: string
  extra?: ReactNode
  disabled: boolean
}) {
  return (
    <div className="flex flex-wrap items-center gap-1.5">
      <span className="ws-label w-32 shrink-0">{label}</span>
      <span className="num flex-1 truncate text-[12px] text-fg" title={value ?? undefined}>
        {value ?? '—'}
      </span>
      {onBrowse ? (
        <button type="button" className="ws-btn" disabled={disabled} onClick={onBrowse}>
          {browseLabel}
        </button>
      ) : null}
      {extra}
    </div>
  )
}

export function SettingsCard() {
  const tauri = isTauri()
  const settings = useSettings()
  const config = useConfig(!tauri)
  const updateSettings = useUpdateSettings()
  const pickPython = usePickPython()
  const pickEnvFile = usePickEnvFile()
  const pickTokenFile = usePickTokenFile()
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
    if (tauri) {
      if (settings.data) setDraft(draftFromPatch(settings.data.settings))
    } else if (config.data) {
      setDraft(draftFromConfig(config.data))
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [tauri, settings.data, config.data])

  const disabled = !tauri
  const touch = (key: string): void => setDirty((prev) => new Set(prev).add(key))

  const paths = settings.data?.paths
  const tokenStatus = settings.data?.token_status ?? null
  const tradesRoot = tauri ? (paths?.trades_root ?? null) : (asStr(get(config.data, 'trades', 'root')) || null)

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

  return (
    <Card title="Settings">
      <div className="space-y-3 px-2.5 py-2 text-[12px]">
        {disabled ? <p className="m-0 text-[11px] text-muted">{READ_ONLY_NOTE}</p> : null}

        <fieldset className="m-0 space-y-1.5 border-0 p-0" disabled={disabled}>
          <legend className="ws-label mb-1 font-semibold text-fg-strong">Trades</legend>
          <ReadOnlyPath
            label="Folder"
            value={tradesRoot}
            disabled={disabled}
            onBrowse={() => useUiSignals.getState().emit('open-trades-dialog')}
            browseLabel="Choose…"
            extra={
              <button
                type="button"
                className="ws-btn"
                disabled={disabled || !tradesRoot}
                onClick={() => tradesRoot && revealFolder.mutate(tradesRoot)}
              >
                Reveal
              </button>
            }
          />
          <details id={advancedId}>
            <summary className="ws-label cursor-pointer select-none">Advanced</summary>
            <div className="mt-1.5 space-y-1.5 pl-1">
              <TextField
                label="Open dir"
                value={draft.trades.open_dir}
                disabled={disabled}
                onChange={(v) => {
                  setDraft((d) => ({ ...d, trades: { ...d.trades, open_dir: v } }))
                  touch('trades.open_dir')
                }}
              />
              <TextField
                label="Closed dir"
                value={draft.trades.closed_dir}
                disabled={disabled}
                onChange={(v) => {
                  setDraft((d) => ({ ...d, trades: { ...d.trades, closed_dir: v } }))
                  touch('trades.closed_dir')
                }}
              />
              <TextField
                label="Statements dir"
                value={draft.trades.statements_dir}
                disabled={disabled}
                onChange={(v) => {
                  setDraft((d) => ({ ...d, trades: { ...d.trades, statements_dir: v } }))
                  touch('trades.statements_dir')
                }}
              />
              <TextField
                label="Statement TZ"
                value={draft.trades.statement_timezone}
                disabled={disabled}
                onChange={(v) => {
                  setDraft((d) => ({ ...d, trades: { ...d.trades, statement_timezone: v } }))
                  touch('trades.statement_timezone')
                }}
              />
              <NumField
                label="Watch debounce ms"
                value={draft.trades.watch_debounce_ms}
                disabled={disabled}
                onChange={(v) => {
                  setDraft((d) => ({ ...d, trades: { ...d.trades, watch_debounce_ms: v } }))
                  touch('trades.watch_debounce_ms')
                }}
              />
              <NumField
                label="Watch poll secs"
                value={draft.trades.watch_poll_secs}
                disabled={disabled}
                onChange={(v) => {
                  setDraft((d) => ({ ...d, trades: { ...d.trades, watch_poll_secs: v } }))
                  touch('trades.watch_poll_secs')
                }}
              />
            </div>
          </details>
        </fieldset>

        <fieldset className="m-0 space-y-1.5 border-0 border-t border-line pt-2" disabled={disabled}>
          <legend className="ws-label mb-1 font-semibold text-fg-strong">Market data</legend>
          <ReadOnlyPath
            label="Token file"
            value={tauri ? paths?.token_path : asStr(get(config.data, 'schwab', 'token_path')) || null}
            disabled={disabled}
            onBrowse={() => pickTokenFile.mutate()}
          />
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
            value={tauri ? paths?.env_file : null}
            disabled={disabled}
            onBrowse={() => pickEnvFile.mutate()}
          />
          <TextField
            label="Token config key"
            value={draft.schwab.token_config_key}
            disabled={disabled}
            onChange={(v) => {
              setDraft((d) => ({ ...d, schwab: { ...d.schwab, token_config_key: v } }))
              touch('schwab.token_config_key')
            }}
          />
          <TextField
            label="App key env"
            value={draft.schwab.app_key_env}
            disabled={disabled}
            onChange={(v) => {
              setDraft((d) => ({ ...d, schwab: { ...d.schwab, app_key_env: v } }))
              touch('schwab.app_key_env')
            }}
          />
          <TextField
            label="App secret env"
            value={draft.schwab.app_secret_env}
            disabled={disabled}
            onChange={(v) => {
              setDraft((d) => ({ ...d, schwab: { ...d.schwab, app_secret_env: v } }))
              touch('schwab.app_secret_env')
            }}
          />
          <CheckField
            label="Let Wingspan refresh the Schwab access token itself"
            checked={draft.schwab.refresh}
            disabled={disabled}
            onChange={(v) => {
              setDraft((d) => ({ ...d, schwab: { ...d.schwab, refresh: v } }))
              touch('schwab.refresh')
            }}
          />
          <CheckField
            label="yfinance enabled"
            checked={draft.yfinance.enabled}
            disabled={disabled}
            onChange={(v) => {
              setDraft((d) => ({ ...d, yfinance: { enabled: v } }))
              touch('yfinance.enabled')
            }}
          />
          <ReadOnlyPath
            label="Interpreter"
            value={tauri ? paths?.python : asStr(get(config.data, 'yfinance', 'python')) || null}
            disabled={disabled}
            onBrowse={() => pickPython.mutate()}
            extra={
              <button
                type="button"
                className="ws-btn"
                disabled={disabled || probePython.isPending}
                onClick={() => probePython.mutate()}
              >
                {probePython.isPending ? 'Testing…' : 'Test'}
              </button>
            }
          />
          {probePython.data ? (
            <p className={`m-0 pl-[8.5rem] text-[11px] ${probePython.data.ok ? 'text-pos' : 'text-warn'}`}>
              {probePython.data.detail}
            </p>
          ) : probePython.isError ? (
            <p className="m-0 pl-[8.5rem] text-[11px] text-warn">{errorDetail(probePython.error)}</p>
          ) : null}
          <NumField
            label="Refresh secs (open)"
            value={draft.market.refresh_secs_open}
            disabled={disabled}
            onChange={(v) => {
              setDraft((d) => ({ ...d, market: { ...d.market, refresh_secs_open: v } }))
              touch('market.refresh_secs_open')
            }}
          />
          <NumField
            label="Refresh secs (closed)"
            value={draft.market.refresh_secs_closed}
            disabled={disabled}
            onChange={(v) => {
              setDraft((d) => ({ ...d, market: { ...d.market, refresh_secs_closed: v } }))
              touch('market.refresh_secs_closed')
            }}
          />
          <CheckField
            label="Pause price refreshes while the window is hidden"
            checked={draft.market.pause_when_hidden}
            disabled={disabled}
            onChange={(v) => {
              setDraft((d) => ({ ...d, market: { ...d.market, pause_when_hidden: v } }))
              touch('market.pause_when_hidden')
            }}
          />
        </fieldset>

        <fieldset className="m-0 space-y-1.5 border-0 border-t border-line pt-2" disabled={disabled}>
          <legend className="ws-label mb-1 font-semibold text-fg-strong">Pricing</legend>
          <NumField
            label="Risk-free rate"
            value={draft.pricing.risk_free_rate}
            step={0.001}
            disabled={disabled}
            onChange={(v) => {
              setDraft((d) => ({ ...d, pricing: { ...d.pricing, risk_free_rate: v } }))
              touch('pricing.risk_free_rate')
            }}
          />
          <NumField
            label="Dividend yield"
            value={draft.pricing.dividend_yield}
            step={0.001}
            disabled={disabled}
            onChange={(v) => {
              setDraft((d) => ({ ...d, pricing: { ...d.pricing, dividend_yield: v } }))
              touch('pricing.dividend_yield')
            }}
          />
          <TextField
            label="Weighted vega ref"
            value={draft.pricing.weighted_vega_reference}
            disabled={disabled}
            onChange={(v) => {
              setDraft((d) => ({ ...d, pricing: { ...d.pricing, weighted_vega_reference: v } }))
              touch('pricing.weighted_vega_reference')
            }}
          />
          <NumField
            label="Weighted vega exp"
            value={draft.pricing.weighted_vega_exponent}
            step={0.1}
            disabled={disabled}
            onChange={(v) => {
              setDraft((d) => ({ ...d, pricing: { ...d.pricing, weighted_vega_exponent: v } }))
              touch('pricing.weighted_vega_exponent')
            }}
          />
        </fieldset>

        <fieldset className="m-0 space-y-1.5 border-0 border-t border-line pt-2" disabled={disabled}>
          <legend className="ws-label mb-1 font-semibold text-fg-strong">Risk graph</legend>
          <NumField
            label="Points"
            value={draft.risk_graph.points}
            disabled={disabled}
            onChange={(v) => {
              setDraft((d) => ({ ...d, risk_graph: { ...d.risk_graph, points: v } }))
              touch('risk_graph.points')
            }}
          />
          <NumField
            label="Range %"
            value={draft.risk_graph.range_pct}
            step={0.01}
            disabled={disabled}
            onChange={(v) => {
              setDraft((d) => ({ ...d, risk_graph: { ...d.risk_graph, range_pct: v } }))
              touch('risk_graph.range_pct')
            }}
          />
          <TextField
            label="T+ preset"
            value={draft.risk_graph.tplus_preset}
            disabled={disabled}
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
            disabled={disabled || updateSettings.isPending || dirty.size === 0}
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
