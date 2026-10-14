/**
 * The Model tab's pure helpers: how a chain row is keyed, which trades the
 * Pos column counts, how modeled legs become request fields, and which
 * strikes a section shows.
 *
 * Everything here is synchronous and side-effect free (the hook that keeps
 * the server's quote universe in step lives in `api/queries.ts`), so the
 * whole Model tab's arithmetic is unit-tested without rendering anything.
 */

import type {
  ChainExpiry,
  ChainRow,
  ChainUnderlying,
  IsoDate,
  OptionChainResponse,
  ModelContract,
  ModelLeg as ModelLegDto,
  OptionContract,
  Right,
  Trade,
} from '../api/types'
import type { ModelLeg } from '../store/appStore'
import { formatExpiry, parseIsoDate } from './dates'
import { fmtMoney } from './format'

/** U+2212: a real minus sign, the width of the `+` it pairs with in a mono
 *  column — a hyphen reads as a dash beside a `+2`. */
export const MINUS = '−'

/** A contract's identity everywhere in the Model tab: its TOS code. */
export function contractKey(contract: Pick<OptionContract, 'tos_code'>): string {
  return contract.tos_code
}

/** A chain section's identity: an expiration is the pair `(expiry, root)`,
 *  since a third Friday lists both `SPX`/AM and `SPXW`/PM.
 *
 *  The listing is SPX's, and XSP has no listing of its own: an XSP contract
 *  (root `XSP`, always PM) sits in the SPXW/PM section of its expiry — the
 *  one its chain is built from — so `XSP` reads as `SPXW` here, and the held
 *  and modeled marks on the expiry strip need no second vocabulary. */
export function expiryKey(expiration: Pick<ChainExpiry, 'expiry' | 'root'>): string {
  return `${expiration.expiry}|${expiration.root === XSP_ROOT ? 'SPXW' : expiration.root}`
}

/** The OPRA root of Mini-SPX options. */
const XSP_ROOT = 'XSP'

/** The index a contract is on — its `underlying`, read defensively. */
export function underlyingOf(contract: Pick<OptionContract, 'underlying'>): ChainUnderlying {
  return contract.underlying === 'XSP' ? 'XSP' : 'SPX'
}

export function parseExpiryKey(key: string): { expiry: IsoDate; root: string } | null {
  const [expiry, root, ...rest] = key.split('|')
  if (!expiry || !root || rest.length > 0) return null
  return { expiry, root }
}

/**
 * The trades the position side of the Model tab stands for: the selection,
 * or every open trade when nothing is selected — what the trader holds. (Not
 * what the risk graph draws then: with nothing selected it draws the model
 * alone.)
 */
export function effectiveTradeIds(selected: readonly string[], trades: readonly Trade[]): string[] {
  if (selected.length > 0) return [...selected]
  return trades.map((trade) => trade.id)
}

/** Net held quantity per contract (TOS code) across `ids`; zero nets are
 *  dropped, so a contract one trade is long and another short shows blank. */
export function positionByContract(trades: readonly Trade[], ids: readonly string[]): Map<string, number> {
  const wanted = new Set(ids)
  const out = new Map<string, number>()
  for (const trade of trades) {
    if (!wanted.has(trade.id)) continue
    for (const leg of trade.legs) {
      if (leg.qty === 0) continue
      const key = contractKey(leg.contract)
      out.set(key, (out.get(key) ?? 0) + leg.qty)
    }
  }
  for (const [key, qty] of out) if (qty === 0) out.delete(key)
  return out
}

/** The `trade_id` the server gives the model position's `per_trade` curve. */
export const MODEL_TRADE_ID = 'model'

/** What the per-trade overlay calls a curve: the trade's name, "Model" for
 *  the model position, the raw id for anything it cannot place. */
export function overlayTradeName(tradeId: string, trades: readonly Pick<Trade, 'id' | 'name'>[]): string {
  if (tradeId === MODEL_TRADE_ID) return 'Model'
  return trades.find((trade) => trade.id === tradeId)?.name ?? tradeId
}

/** The request shape of a contract: the frozen leg contract minus the derived
 *  `osi`/`tos_code`. */
export function toModelContract(contract: OptionContract): ModelContract {
  return {
    underlying: contract.underlying,
    root: contract.root,
    expiry: contract.expiry,
    settlement: contract.settlement,
    strike: contract.strike,
    right: contract.right,
    multiplier: contract.multiplier,
  }
}

function byKey(a: ModelLeg, b: ModelLeg): number {
  const ka = contractKey(a.contract)
  const kb = contractKey(b.contract)
  return ka < kb ? -1 : ka > kb ? 1 : 0
}

/**
 * `model_legs` for a request. Sorted by contract so the query key depends on
 * *what* is modeled, not on the order the trader typed it in — re-entering
 * the same model must hit the same cached curves.
 */
export function toModelLegs(legs: readonly ModelLeg[]): ModelLegDto[] {
  return [...legs]
    .filter((leg) => leg.qty !== 0)
    .sort(byKey)
    .map((leg) => ({ contract: toModelContract(leg.contract), qty: leg.qty }))
}

/** The distinct modeled contracts, for `set_model_contracts`, in key order. */
export function modelContracts(legs: readonly ModelLeg[]): ModelContract[] {
  const seen = new Set<string>()
  const out: ModelContract[] = []
  for (const leg of [...legs].sort(byKey)) {
    const key = contractKey(leg.contract)
    if (seen.has(key)) continue
    seen.add(key)
    out.push(toModelContract(leg.contract))
  }
  return out
}

/**
 * The server's valuation date, read off a snapshot's `as_of`: the UTC date of
 * the instant its clock stamped. That is the date the server refuses an
 * expired modeled leg against (the book's valuation date is the clock's UTC
 * date — or the demo's pinned one, which this follows too, where the
 * browser's own calendar would be a month out). Null for an unreadable
 * instant.
 */
export function valuationDateOf(asOf: string | null | undefined): IsoDate | null {
  if (!asOf) return null
  const at = new Date(asOf)
  return Number.isNaN(at.getTime()) ? null : at.toISOString().slice(0, 10)
}

/**
 * Whether a modeled leg can still be priced on `valuationDate`: its expiry is
 * that day or later. An expired one would make the server refuse the whole
 * request (422 `out-of-range`), taking the risk graph down with it. The rule
 * never drops a live leg: the UTC date is never behind New York's, and on the
 * evening it runs ahead, every contract dated the New York day has already
 * settled.
 */
export function isLiveModelLeg(leg: Pick<ModelLeg, 'contract'>, valuationDate: IsoDate): boolean {
  return leg.contract.expiry >= valuationDate
}

/** A stable string for a set of contracts — what the universe sync compares. */
export function modelContractsKey(legs: readonly ModelLeg[]): string {
  return [...new Set(legs.map((leg) => contractKey(leg.contract)))].sort().join(',')
}

/**
 * Root, expiry, right and strike back out of a TOS code
 * (`SPXW261016C7700`). The Pos column only has codes to go on once trades are
 * folded into a map; null for anything that is not that shape.
 */
export function parseTosCode(
  code: string,
): { root: string; expiry: IsoDate; right: Right; strike: number } | null {
  const match = /^([A-Z]+)(\d{2})(\d{2})(\d{2})([CP])(\d+(?:\.\d+)?)$/.exec(code)
  if (!match) return null
  const [, root, yy, mm, dd, cp, strike] = match
  if (!root || !yy || !mm || !dd || !cp || !strike) return null
  return { root, expiry: `20${yy}-${mm}-${dd}`, right: cp === 'C' ? 'call' : 'put', strike: Number(strike) }
}

/**
 * The legs of a position map on one product: an `XSP…` code is XSP, any
 * other (or an unreadable one) SPX — the root decides, as `underlying_of_root`
 * does on the server.
 */
export function positionsOn(
  positions: ReadonlyMap<string, number>,
  underlying: ChainUnderlying,
): Map<string, number> {
  const out = new Map<string, number>()
  for (const [code, qty] of positions) {
    const on: ChainUnderlying = parseTosCode(code)?.root === XSP_ROOT ? 'XSP' : 'SPX'
    if (on === underlying) out.set(code, qty)
  }
  return out
}

/** The `expiry|root` keys a position map holds legs in. */
export function positionExpiryKeys(positions: ReadonlyMap<string, number>): Set<string> {
  const out = new Set<string>()
  for (const code of positions.keys()) {
    const parsed = parseTosCode(code)
    if (parsed) out.add(expiryKey(parsed))
  }
  return out
}

/**
 * The section shown before the trader picks one: the first listed expiration
 * the model has a leg in, else the first holding a position, else the first
 * listed one, else null (nothing is listed).
 *
 * Modeled legs come first because the pick is recomputed whenever the book
 * changes: with positions first, a trade added in a nearer expiry (a sheet
 * refresh) would swap the chain the trader is modeling in for another one,
 * with no click. The model only changes under the trader's own hand.
 */
export function autoExpiry(
  expirations: readonly ChainExpiry[],
  positions: ReadonlyMap<string, number>,
  legs: readonly ModelLeg[],
): string | null {
  const keys = expirations.map(expiryKey)
  const modeled = new Set(legs.map((leg) => expiryKey(leg.contract)))
  const held = positionExpiryKeys(positions)
  return keys.find((key) => modeled.has(key)) ?? keys.find((key) => held.has(key)) ?? keys[0] ?? null
}

/**
 * The `scrollTop` that puts a row `rowHeight` tall, `rowTop` from the top of
 * the scroll content, in the vertical middle of a viewport `viewportHeight`
 * tall. Never negative: a row nearer the top than half a viewport simply
 * leaves the content at its start. (The far end needs no clamp — the browser
 * clamps a `scrollTop` past the end itself.)
 */
export function centredScrollTop(rowTop: number, rowHeight: number, viewportHeight: number): number {
  return Math.max(0, Math.round(rowTop - (viewportHeight - rowHeight) / 2))
}

/** A strike as TOS writes it: no grouping, decimals only when there are some. */
export function fmtStrike(strike: number): string {
  if (!Number.isFinite(strike)) return '—'
  return String(Number(strike.toFixed(2)))
}

/** `SPXW 16 OCT 26 7700 C` — the ticket's contract label. */
export function fmtContract(contract: Pick<OptionContract, 'root' | 'expiry' | 'strike' | 'right'>): string {
  return `${contract.root} ${formatExpiry(contract.expiry)} ${fmtStrike(contract.strike)} ${
    contract.right === 'call' ? 'C' : 'P'
  }`
}

const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec']

/** `SPXW 16 Oct 26 7700 call` — what a screen reader hears for a Model cell. */
export function contractSpokenLabel(
  contract: Pick<OptionContract, 'root' | 'expiry' | 'strike' | 'right'>,
): string {
  const d = parseIsoDate(contract.expiry)
  const month = MONTHS[d.getUTCMonth()] ?? '???'
  const yy = String(d.getUTCFullYear()).slice(2)
  return `${contract.root} ${d.getUTCDate()} ${month} ${yy} ${fmtStrike(contract.strike)} ${contract.right}`
}

/** `+2` / `−1` / blank for zero: the Pos and Model columns' quantity form. */
export function fmtSignedQty(qty: number | null | undefined): string {
  if (qty === null || qty === undefined || !Number.isFinite(qty) || qty === 0) return ''
  return qty > 0 ? `+${qty}` : `${MINUS}${Math.abs(qty)}`
}

/**
 * What the trader typed into a Model cell, as a number: blank is 0, a leading
 * `+`, `-` or `−` is a sign, anything else unreadable is null (the cell then
 * reverts rather than guessing). Fractions are kept here; the store rounds.
 */
export function parseModelQty(text: string): number | null {
  // A space after the sign (`- 2`) is a typing habit; one between digits is
  // not a number.
  const trimmed = text.trim().replace(MINUS, '-').replace(/^([+-])\s+/, '$1')
  if (trimmed === '' || trimmed === '+' || trimmed === '-') return 0
  if (!/^[+-]?(\d+(\.\d*)?|\.\d+)$/.test(trimmed)) return null
  const value = Number(trimmed)
  return Number.isFinite(value) ? value : null
}

/**
 * The model's net premium in dollars: Σ qty × mark × multiplier, so a debit
 * (buying) is positive and a credit negative. Null while any leg has no mark
 * — a partial sum would be a number that is simply wrong.
 */
export function modelNetCost(
  legs: readonly ModelLeg[],
  marksByTosCode: ReadonlyMap<string, number | null>,
): number | null {
  let total = 0
  for (const leg of legs) {
    const mark = marksByTosCode.get(contractKey(leg.contract))
    if (mark === null || mark === undefined || !Number.isFinite(mark)) return null
    total += leg.qty * mark * leg.contract.multiplier
  }
  return Math.round(total * 100) / 100
}

/**
 * The rows a section shows: `count` strikes balanced around spot (half below
 * it, half at or above), shifted inward at either end of the chain, plus any
 * `pinned` strike outside that window — the strikes holding a position or a
 * modeled leg, which must never be hidden by the window. `count: null` (or a
 * chain no longer than `count`) is every row.
 */
export function visibleStrikes(
  rows: readonly ChainRow[],
  spot: number,
  count: number | null,
  pinned: Iterable<number> = [],
): ChainRow[] {
  if (count === null || rows.length <= count) return [...rows]
  let atm = rows.findIndex((row) => row.strike >= spot)
  if (atm === -1) atm = rows.length
  const start = Math.max(0, Math.min(rows.length - count, atm - Math.floor(count / 2)))
  const keep = new Set(pinned)
  return rows.filter((row, index) => (index >= start && index < start + count) || keep.has(row.strike))
}

/** `16 Oct 2026` — a section title's date. */
export function fmtSectionDate(date: IsoDate): string {
  const d = parseIsoDate(date)
  return `${d.getUTCDate()} ${MONTHS[d.getUTCMonth()] ?? '???'} ${d.getUTCFullYear()}`
}

/** `SPX · 16 Oct 2026 · AM · 46 DTE` — a chain section's card title. */
export function sectionTitle(listing: Pick<ChainExpiry, 'expiry' | 'root' | 'settlement' | 'dte'>): string {
  return `${listing.root} · ${fmtSectionDate(listing.expiry)} · ${listing.settlement.toUpperCase()} · ${listing.dte} DTE`
}

/** `SPX · 16 OCT 26 · AM · 46d` — a section in the short form its pinned
 *  header row carries, once the card title has scrolled away. */
export function stickySectionTitle(
  listing: Pick<ChainExpiry, 'expiry' | 'root' | 'settlement' | 'dte'>,
): string {
  return `${listing.root} · ${formatExpiry(listing.expiry)} · ${listing.settlement.toUpperCase()} · ${listing.dte}d`
}

/** Modeled quantity per contract (TOS code). */
export function modelQtyByContract(legs: readonly ModelLeg[]): Map<string, number> {
  return new Map(legs.map((leg) => [contractKey(leg.contract), leg.qty]))
}

/** Every mark the loaded chains carry, by TOS code — the ticket's prices. A
 *  quote with no mark maps to null, which is not the same as "not loaded". */
export function chainMarks(chains: readonly (OptionChainResponse | undefined)[]): Map<string, number | null> {
  const out = new Map<string, number | null>()
  for (const chain of chains) {
    if (!chain) continue
    for (const row of chain.rows) {
      for (const quote of [row.call, row.put]) {
        if (quote) out.set(contractKey(quote.contract), quote.mark)
      }
    }
  }
  return out
}

/** The strikes in `rows` where either side is held or modeled — the ones a
 *  strike window must keep on screen. */
export function heldStrikes(
  rows: readonly ChainRow[],
  positions: ReadonlyMap<string, number>,
  modelQty: ReadonlyMap<string, number>,
): number[] {
  const out: number[] = []
  for (const row of rows) {
    const held = [row.call, row.put].some((quote) => {
      if (!quote) return false
      const key = contractKey(quote.contract)
      return positions.has(key) || modelQty.has(key)
    })
    if (held) out.push(row.strike)
  }
  return out
}

/** `net $1,250 debit` / `net -$1,250 credit` — the sign and the word agree;
 *  `net —` while a mark is missing. */
export function fmtNetCost(net: number | null): string {
  if (net === null) return 'net —'
  if (net === 0) return 'net $0'
  return `net ${fmtMoney(net, { dp: 0 })} ${net > 0 ? 'debit' : 'credit'}`
}

// ---------------------------------------------------------------- drag & drop

/**
 * The MIME type a Model cell puts on the drag when its quantity is dragged.
 *
 * The gesture is the browser's own text drag — select the `-1`, drag it to
 * another strike — so the cell rides along on the `dragstart` the browser
 * fires anyway and adds this type beside `text/plain`. A drop target that
 * finds it knows the drag is a modeled quantity and *moves* it: the source
 * cell empties. Without it (text dragged in from elsewhere) the browser's
 * default — pasting the text — stands.
 */
export const MODEL_DRAG_TYPE = 'application/x-wingspan-model-qty'

export interface ModelDrag {
  /** `tos_code` of the contract the quantity came from. */
  from: string
  qty: number
}

/** Puts a modeled quantity on the drag. */
export function writeModelDrag(dt: DataTransfer, from: string, qty: number): void {
  dt.setData(MODEL_DRAG_TYPE, JSON.stringify({ from, qty }))
  dt.effectAllowed = 'move'
}

/** Whether a drag passing over a cell carries a modeled quantity. The data
 *  itself is unreadable until `drop`, but the type list is not. */
export function carriesModelDrag(dt: DataTransfer | null): boolean {
  return dt !== null && Array.from(dt.types).includes(MODEL_DRAG_TYPE)
}

/** The modeled quantity a drop carries, or null for any other drag. */
export function readModelDrag(dt: DataTransfer | null): ModelDrag | null {
  if (!carriesModelDrag(dt) || dt === null) return null
  try {
    const parsed: unknown = JSON.parse(dt.getData(MODEL_DRAG_TYPE))
    if (typeof parsed !== 'object' || parsed === null) return null
    const { from, qty } = parsed as { from?: unknown; qty?: unknown }
    if (typeof from !== 'string' || typeof qty !== 'number' || !Number.isFinite(qty) || qty === 0) {
      return null
    }
    return { from, qty }
  } catch {
    return null
  }
}
