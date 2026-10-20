/**
 * Chart palette.
 *
 * Categorical colours come from the Okabe-Ito colour-blind-safe set; the T+x
 * ladder is a single hue ramp (sky blue -> reddish purple) so the lines read as
 * an ordered family rather than as unrelated categories, and stay ordered under
 * deuteranopia and protanopia.
 */

import type { Theme } from '../store/appStore'

export interface ChartPalette {
  background: string
  grid: string
  axis: string
  text: string
  muted: string
  zero: string
  spot: string
  expiry: string
  t0: string
  /** The one hue of the ghost family: the position without the model, drawn
   *  dashed under the combined curves. A warm grey, so it reads as "what was"
   *  against every live line and never collides with the cool axis grey. */
  position: string
  /** Endpoints of the T+x ramp. */
  tplusFrom: string
  tplusTo: string
  band1: string
  band2: string
  breakeven: string
  positive: string
  negative: string
  perTrade: string[]
  /** Greek-strip row hues; negative bars use the same hue at 55 % opacity. */
  delta: string
  gamma: string
  theta: string
  vega: string
  wvega: string
  /** Backlit column behind the cursor, across the strip rows. */
  cursorBand: string
  /** Background of the cursor value box. */
  cursorBox: string
}

const OKABE_ITO = [
  '#E69F00', // orange
  '#009E73', // bluish green
  '#CC79A7', // reddish purple
  '#56B4E9', // sky blue
  '#D55E00', // vermillion
  '#F0E442', // yellow
  '#0072B2', // blue
]

const DARK: ChartPalette = {
  background: 'transparent',
  grid: 'rgba(255,255,255,0.07)',
  axis: '#33465c',
  text: '#dde5ee',
  muted: '#8496aa',
  zero: '#8496aa',
  spot: '#e6a01a',
  expiry: '#f4f8fc',
  t0: '#56b4e9',
  position: '#c2b89f',
  tplusFrom: '#56b4e9',
  tplusTo: '#cc79a7',
  band1: 'rgba(86,180,233,0.10)',
  band2: 'rgba(86,180,233,0.05)',
  breakeven: '#35c39a',
  positive: '#35c39a',
  negative: '#f2705a',
  perTrade: OKABE_ITO,
  delta: '#7fb3ff',
  gamma: '#c29ffa',
  theta: '#f0a35e',
  vega: '#5fd0c0',
  wvega: '#d98cc0',
  cursorBand: 'rgba(255,255,255,0.10)',
  cursorBox: 'rgba(11,16,23,0.92)',
}

const LIGHT: ChartPalette = {
  background: 'transparent',
  grid: 'rgba(0,0,0,0.08)',
  axis: '#b3bfcd',
  text: '#1b2531',
  muted: '#5a6b7e',
  zero: '#5a6b7e',
  spot: '#a86c00',
  expiry: '#0a0f16',
  t0: '#0072b2',
  position: '#7d735f',
  tplusFrom: '#0072b2',
  tplusTo: '#a0527c',
  band1: 'rgba(0,114,178,0.10)',
  band2: 'rgba(0,114,178,0.05)',
  breakeven: '#068a68',
  positive: '#068a68',
  negative: '#cf4a2e',
  perTrade: OKABE_ITO,
  delta: '#2f6fd0',
  gamma: '#7a4fbf',
  theta: '#b06a12',
  vega: '#0e8f84',
  wvega: '#a84c85',
  cursorBand: 'rgba(0,0,0,0.07)',
  cursorBox: 'rgba(255,255,255,0.92)',
}

export function chartPalette(theme: Theme): ChartPalette {
  return theme === 'light' ? LIGHT : DARK
}

function hexToRgb(hex: string): [number, number, number] {
  const clean = hex.replace('#', '')
  const full =
    clean.length === 3
      ? clean
          .split('')
          .map((c) => c + c)
          .join('')
      : clean
  return [
    Number.parseInt(full.slice(0, 2), 16),
    Number.parseInt(full.slice(2, 4), 16),
    Number.parseInt(full.slice(4, 6), 16),
  ]
}

function toHex(value: number): string {
  return Math.max(0, Math.min(255, Math.round(value))).toString(16).padStart(2, '0')
}

/** Linear RGB interpolation; `t` is clamped to [0, 1]. */
export function mixHex(from: string, to: string, t: number): string {
  const clamped = Math.max(0, Math.min(1, t))
  const [r1, g1, b1] = hexToRgb(from)
  const [r2, g2, b2] = hexToRgb(to)
  return `#${toHex(r1 + (r2 - r1) * clamped)}${toHex(g1 + (g2 - g1) * clamped)}${toHex(
    b1 + (b2 - b1) * clamped,
  )}`
}

/** Colour of the i-th T+x line out of `total`, nearest-dated first. */
export function tplusColor(palette: ChartPalette, index: number, total: number): string {
  if (total <= 1) return palette.tplusFrom
  return mixHex(palette.tplusFrom, palette.tplusTo, index / (total - 1))
}

/** Dash pattern grades with the line's distance in time. */
export function tplusDash(index: number, total: number): number[] {
  const span = total <= 1 ? 0 : index / (total - 1)
  return [8 - 4 * span, 3 + 3 * span]
}

/**
 * How the `rank`-th expiration line (by date, 0 = the earliest — the
 * canonical `expiry` line every header readout describes) is drawn: the
 * earliest is the solid 3 px line the eye lands on first; every later one is
 * thinner and dashed, each with a pattern of its own, so several expirations
 * in one colour never read as one line. The inset and the line toggles draw
 * their swatches from the same style (`SwatchLine`'s `dash`/`width`).
 */
export interface ExpiryStyle {
  width: number
  /** ECharts / SVG dash pattern in px; null = solid. */
  dash: number[] | null
}

const LATER_EXPIRY_DASHES: readonly number[][] = [
  [12, 4],
  [12, 4, 2, 4],
  [6, 4],
  [2, 3],
]

export function expiryStyle(rank: number): ExpiryStyle {
  if (rank <= 0) return { width: 3, dash: null }
  const dash = LATER_EXPIRY_DASHES[(rank - 1) % LATER_EXPIRY_DASHES.length] ?? [6, 4]
  return { width: Math.max(1.8, 2.6 - 0.2 * (rank - 1)), dash: [...dash] }
}

/** Rank of every expiration line by date (keyed by `RiskLine.key`), counted
 *  over *all* the response's lines — not just the visible ones — so a line
 *  keeps its pattern when another is switched off. */
export function expiryRanks(lines: readonly { kind: string; key: string; date: string }[]): Map<string, number> {
  const expiries = lines.filter((line) => line.kind === 'expiry').sort((a, b) => a.date.localeCompare(b.date))
  return new Map(expiries.map((line, rank) => [line.key, rank]))
}

export function perTradeColor(palette: ChartPalette, index: number): string {
  return palette.perTrade[index % palette.perTrade.length] ?? palette.muted
}
