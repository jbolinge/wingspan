import { describe, expect, it } from 'vitest'
import type { ChainExpiry, ChainQuote, ChainRow, OptionChainResponse, OptionContract } from '../api/types'
import type { ModelLeg } from '../store/appStore'
import { makeLeg, makeTrade } from '../test/factories'
import {
  MODEL_DRAG_TYPE,
  carriesModelDrag,
  readModelDrag,
  writeModelDrag,
  autoExpiry,
  centredScrollTop,
  chainMarks,
  contractKey,
  contractSpokenLabel,
  effectiveTradeIds,
  expiryKey,
  fmtContract,
  fmtNetCost,
  fmtSectionDate,
  fmtSignedQty,
  fmtStrike,
  heldStrikes,
  isLiveModelLeg,
  MINUS,
  modelContracts,
  modelContractsKey,
  modelNetCost,
  modelQtyByContract,
  overlayTradeName,
  parseExpiryKey,
  parseModelQty,
  parseTosCode,
  positionByContract,
  positionExpiryKeys,
  sectionTitle,
  stickySectionTitle,
  toModelContract,
  toModelLegs,
  valuationDateOf,
  visibleStrikes,
} from './model'

function contract(
  root: string,
  expiry: string,
  strike: number,
  right: 'call' | 'put',
): OptionContract {
  const yymmdd = expiry.slice(2).replaceAll('-', '')
  const cp = right === 'call' ? 'C' : 'P'
  return {
    underlying: 'SPX',
    root,
    expiry,
    settlement: root === 'SPX' ? 'am' : 'pm',
    strike,
    right,
    multiplier: 100,
    osi: `${root.padEnd(6, ' ')}${yymmdd}${cp}${String(strike * 1000).padStart(8, '0')}`,
    tos_code: `${root}${yymmdd}${cp}${strike}`,
  }
}

const OCT_7200P = contract('SPX', '2026-10-16', 7200, 'put')
const OCT_8200C = contract('SPX', '2026-10-16', 8200, 'call')
const OCTW_7700C = contract('SPXW', '2026-10-16', 7700, 'call')

function listing(expiry: string, root: string, dte = 10): ChainExpiry {
  return { expiry, root, dte, settlement: root === 'SPX' ? 'am' : 'pm', kind: 'monthly' }
}

function quote(c: OptionContract, mark: number | null): ChainQuote {
  return {
    contract: c,
    bid: null,
    ask: null,
    mark,
    last: null,
    iv: null,
    iv_hint: null,
    greeks: null,
    volume: null,
    open_interest: null,
    quote_as_of: '2026-08-31T16:00:00Z',
  }
}

function rows(strikes: number[]): ChainRow[] {
  return strikes.map((strike) => ({
    strike,
    call: quote(contract('SPX', '2026-10-16', strike, 'call'), 10),
    put: quote(contract('SPX', '2026-10-16', strike, 'put'), 10),
  }))
}

describe('keys', () => {
  it('keys a contract by its TOS code and an expiration by (expiry, root)', () => {
    expect(contractKey(OCT_7200P)).toBe('SPX261016P7200')
    expect(expiryKey(listing('2026-10-16', 'SPXW'))).toBe('2026-10-16|SPXW')
    expect(parseExpiryKey('2026-10-16|SPXW')).toEqual({ expiry: '2026-10-16', root: 'SPXW' })
    expect(parseExpiryKey('2026-10-16')).toBeNull()
    expect(parseExpiryKey('a|b|c')).toBeNull()
  })

  it('reads root, expiry, right and strike back out of a TOS code', () => {
    expect(parseTosCode('SPXW261016C7700')).toEqual({
      root: 'SPXW',
      expiry: '2026-10-16',
      right: 'call',
      strike: 7700,
    })
    expect(parseTosCode('SPX261016P7212.5')?.strike).toBe(7212.5)
    expect(parseTosCode('not a code')).toBeNull()
  })
})

describe('the position the chain counts', () => {
  const strangle = makeTrade({
    id: 'strangle',
    legs: [makeLeg({ contract: OCT_7200P, qty: -1 }), makeLeg({ contract: OCT_8200C, qty: -1 })],
  })
  const hedge = makeTrade({ id: 'hedge', legs: [makeLeg({ contract: OCT_7200P, qty: 3 })] })

  it('is the selection, or every open trade when nothing is selected', () => {
    expect(effectiveTradeIds(['hedge'], [strangle, hedge])).toEqual(['hedge'])
    expect(effectiveTradeIds([], [strangle, hedge])).toEqual(['strangle', 'hedge'])
  })

  it('nets quantities per contract across the counted trades only', () => {
    const all = positionByContract([strangle, hedge], ['strangle', 'hedge'])
    expect(all.get('SPX261016P7200')).toBe(2)
    expect(all.get('SPX261016C8200')).toBe(-1)
    const one = positionByContract([strangle, hedge], ['strangle'])
    expect(one.get('SPX261016P7200')).toBe(-1)
  })

  it('drops a contract that nets to zero, so the Pos cell is blank', () => {
    const flat = makeTrade({ id: 'flat', legs: [makeLeg({ contract: OCT_7200P, qty: 1 })] })
    const net = positionByContract([strangle, flat], ['strangle', 'flat'])
    expect(net.has('SPX261016P7200')).toBe(false)
  })

  it('lists the expirations a position holds legs in', () => {
    const keys = positionExpiryKeys(positionByContract([strangle], ['strangle']))
    expect([...keys]).toEqual(['2026-10-16|SPX'])
  })
})

describe('model legs on the wire', () => {
  const legs: ModelLeg[] = [
    { contract: OCTW_7700C, qty: 2 },
    { contract: OCT_7200P, qty: -1 },
  ]

  it('strips the derived osi/tos_code from a contract', () => {
    expect(toModelContract(OCT_7200P)).toEqual({
      underlying: 'SPX',
      root: 'SPX',
      expiry: '2026-10-16',
      settlement: 'am',
      strike: 7200,
      right: 'put',
      multiplier: 100,
    })
  })

  it('sorts legs by contract, so typing order never changes the request', () => {
    const forward = toModelLegs(legs)
    const backward = toModelLegs([...legs].reverse())
    expect(forward).toEqual(backward)
    expect(forward.map((leg) => leg.qty)).toEqual([-1, 2])
    expect(forward[0]?.contract).not.toHaveProperty('tos_code')
  })

  it('lists each modeled contract once, with a key that ignores quantities', () => {
    expect(modelContracts(legs)).toHaveLength(2)
    expect(modelContractsKey(legs)).toBe('SPX261016P7200,SPXW261016C7700')
    expect(modelContractsKey([{ contract: OCT_7200P, qty: 5 }, { contract: OCTW_7700C, qty: -9 }])).toBe(
      modelContractsKey(legs),
    )
    expect(modelQtyByContract(legs).get('SPXW261016C7700')).toBe(2)
  })
})

describe('autoExpiry', () => {
  const expirations = [
    listing('2026-09-01', 'SPXW'),
    listing('2026-10-16', 'SPX'),
    listing('2026-10-16', 'SPXW'),
    listing('2026-11-20', 'SPX'),
  ]

  it('opens the first expiration holding a position or a modeled leg', () => {
    const positions = new Map([['SPX261120P7000', -2]])
    expect(autoExpiry(expirations, positions, [{ contract: OCTW_7700C, qty: 1 }])).toBe('2026-10-16|SPXW')
  })

  // The pick is recomputed whenever the book changes: a trade landing in a
  // nearer expiry must not move the chain away from the one being modeled.
  it('prefers an expiration the model has a leg in over a nearer held one', () => {
    const legs = [{ contract: contract('SPX', '2026-11-20', 7000, 'put'), qty: -1 }]
    expect(autoExpiry(expirations, new Map([['SPX261120C7800', 1]]), legs)).toBe('2026-11-20|SPX')
    // An October trade arrives: the model still sits in November.
    const positions = new Map([
      ['SPX261120C7800', 1],
      ['SPX261016P7200', -1],
    ])
    expect(autoExpiry(expirations, positions, legs)).toBe('2026-11-20|SPX')
    expect(autoExpiry(expirations, positions, [])).toBe('2026-10-16|SPX')
  })

  it('tells the two roots of a third Friday apart', () => {
    expect(autoExpiry(expirations, new Map([['SPX261016P7200', -1]]), [])).toBe('2026-10-16|SPX')
  })

  it('falls back to the nearest expiration when nothing is held', () => {
    expect(autoExpiry(expirations, new Map(), [])).toBe('2026-09-01|SPXW')
  })

  it('is null when nothing is listed', () => {
    expect(autoExpiry([], new Map(), [])).toBeNull()
  })
})

describe('formatting', () => {
  it('writes strikes the TOS way', () => {
    expect(fmtStrike(7700)).toBe('7700')
    expect(fmtStrike(7712.5)).toBe('7712.5')
  })

  it('labels a contract for the ticket and for a screen reader', () => {
    expect(fmtContract(OCTW_7700C)).toBe('SPXW 16 OCT 26 7700 C')
    expect(contractSpokenLabel(OCTW_7700C)).toBe('SPXW 16 Oct 26 7700 call')
  })

  it('signs quantities with a real minus and leaves zero blank', () => {
    expect(fmtSignedQty(2)).toBe('+2')
    expect(fmtSignedQty(-1)).toBe(`${MINUS}1`)
    expect(fmtSignedQty(0)).toBe('')
    expect(fmtSignedQty(null)).toBe('')
  })

  it('titles a section with root, date, settlement and DTE', () => {
    expect(fmtSectionDate('2026-10-16')).toBe('16 Oct 2026')
    expect(sectionTitle(listing('2026-10-16', 'SPX', 46))).toBe('SPX · 16 Oct 2026 · AM · 46 DTE')
  })

  it('says debit or credit in words, matching the sign', () => {
    expect(fmtNetCost(1250)).toBe('net $1,250 debit')
    expect(fmtNetCost(-1250)).toBe('net -$1,250 credit')
    expect(fmtNetCost(0)).toBe('net $0')
    expect(fmtNetCost(null)).toBe('net —')
  })

  it('names the model curve in the per-trade overlay', () => {
    const trades = [makeTrade({ id: 'a', name: 'Alpha' })]
    expect(overlayTradeName('model', trades)).toBe('Model')
    expect(overlayTradeName('a', trades)).toBe('Alpha')
    expect(overlayTradeName('ghost', trades)).toBe('ghost')
  })
})

describe('parseModelQty', () => {
  it.each([
    ['', 0],
    ['  ', 0],
    ['+', 0],
    ['3', 3],
    ['+3', 3],
    ['-2', -2],
    [`${MINUS}2`, -2],
    [' - 2 ', -2],
    ['2.6', 2.6],
  ])('reads %j as %s', (text, expected) => {
    expect(parseModelQty(text)).toBe(expected)
  })

  it.each(['abc', '1e3', '--1', '1-', '1 0'])('refuses %j', (text) => {
    expect(parseModelQty(text)).toBeNull()
  })
})

describe('modelNetCost', () => {
  const legs: ModelLeg[] = [
    { contract: OCT_7200P, qty: -1 },
    { contract: OCTW_7700C, qty: 2 },
  ]

  it('is Σ qty × mark × multiplier: a debit is positive', () => {
    const marks = new Map<string, number | null>([
      ['SPX261016P7200', 42.5],
      ['SPXW261016C7700', 10.25],
    ])
    expect(modelNetCost(legs, marks)).toBe(-4250 + 2050)
  })

  it('is null while any leg has no mark rather than a partial sum', () => {
    expect(modelNetCost(legs, new Map([['SPX261016P7200', 42.5]]))).toBeNull()
    expect(
      modelNetCost(legs, new Map<string, number | null>([['SPX261016P7200', 42.5], ['SPXW261016C7700', null]])),
    ).toBeNull()
  })
})

describe('chainMarks and heldStrikes', () => {
  const chain = {
    rows: [
      { strike: 7200, call: quote(contract('SPX', '2026-10-16', 7200, 'call'), 600), put: quote(OCT_7200P, 12.5) },
      { strike: 7300, call: null, put: quote(contract('SPX', '2026-10-16', 7300, 'put'), null) },
    ],
  } as unknown as OptionChainResponse

  it('maps every quoted contract to its mark, null for an unpriceable one', () => {
    const marks = chainMarks([chain, undefined])
    expect(marks.get('SPX261016P7200')).toBe(12.5)
    expect(marks.get('SPX261016P7300')).toBeNull()
    expect(marks.has('SPX261016C7300')).toBe(false)
  })

  it('finds the strikes where either side is held or modeled', () => {
    expect(heldStrikes(chain.rows, new Map([['SPX261016P7200', -1]]), new Map())).toEqual([7200])
    expect(heldStrikes(chain.rows, new Map(), new Map([['SPX261016P7300', 1]]))).toEqual([7300])
    expect(heldStrikes(chain.rows, new Map(), new Map())).toEqual([])
  })
})

describe('visibleStrikes', () => {
  const all = rows([7600, 7625, 7650, 7675, 7700, 7725, 7750, 7775, 7800, 7825, 7850, 7875])

  it('shows every row for "All", or for a chain no longer than the window', () => {
    expect(visibleStrikes(all, 7740, null)).toHaveLength(12)
    expect(visibleStrikes(all, 7740, 16)).toHaveLength(12)
  })

  it('balances the window around spot: half below, half at or above', () => {
    const shown = visibleStrikes(all, 7740, 4).map((row) => row.strike)
    expect(shown).toEqual([7700, 7725, 7750, 7775])
  })

  it('shifts the window inward at either end of the chain', () => {
    expect(visibleStrikes(all, 7500, 4).map((row) => row.strike)).toEqual([7600, 7625, 7650, 7675])
    expect(visibleStrikes(all, 9000, 4).map((row) => row.strike)).toEqual([7800, 7825, 7850, 7875])
  })

  it('keeps a pinned strike outside the window, in strike order', () => {
    const shown = visibleStrikes(all, 7740, 4, [7600, 7875]).map((row) => row.strike)
    expect(shown).toEqual([7600, 7700, 7725, 7750, 7775, 7875])
  })
})

describe('the server\'s valuation date and expired legs', () => {
  it('reads the UTC date off a snapshot instant, whatever the offset it is written in', () => {
    expect(valuationDateOf('2026-08-31T16:00:00Z')).toBe('2026-08-31')
    // 21:30 in New York is already tomorrow in UTC — the server's date too.
    expect(valuationDateOf('2026-09-30T21:30:00-04:00')).toBe('2026-10-01')
    expect(valuationDateOf(null)).toBeNull()
    expect(valuationDateOf(undefined)).toBeNull()
    expect(valuationDateOf('not a date')).toBeNull()
  })

  it('counts a leg live through its expiry day and expired the day after', () => {
    const leg = { contract: contract('SPXW', '2026-10-01', 7700, 'put') }
    expect(isLiveModelLeg(leg, '2026-09-30')).toBe(true)
    expect(isLiveModelLeg(leg, '2026-10-01')).toBe(true)
    expect(isLiveModelLeg(leg, '2026-10-02')).toBe(false)
  })
})

describe('stickySectionTitle', () => {
  it('is the section in the short form the pinned header row carries', () => {
    expect(stickySectionTitle({ root: 'SPXW', expiry: '2026-11-20', settlement: 'pm', dte: 81 })).toBe(
      'SPXW · 20 NOV 26 · PM · 81d',
    )
  })
})

describe('centredScrollTop', () => {
  it('puts the row in the middle of the viewport', () => {
    // 800px down, 20px tall, in a 400px viewport: its middle (810) lands on
    // the viewport's (scrollTop + 200).
    expect(centredScrollTop(800, 20, 400)).toBe(610)
  })

  it('never scrolls above the top of the content', () => {
    expect(centredScrollTop(50, 20, 400)).toBe(0)
  })

  it('rounds to a whole pixel', () => {
    expect(centredScrollTop(800, 21, 400)).toBe(611)
  })
})

describe('the Model-cell drag payload', () => {
  /** jsdom has no DataTransfer; the four methods the helpers touch suffice. */
  function dataTransfer(): DataTransfer {
    const data = new Map<string, string>()
    return {
      effectAllowed: 'uninitialized',
      dropEffect: 'none',
      get types() {
        return Array.from(data.keys())
      },
      setData: (type: string, value: string) => void data.set(type, value),
      getData: (type: string) => data.get(type) ?? '',
    } as unknown as DataTransfer
  }

  it('round-trips the source contract and its quantity as a move', () => {
    const dt = dataTransfer()
    writeModelDrag(dt, 'SPX261016P7500', -1)
    expect(dt.effectAllowed).toBe('move')
    expect(carriesModelDrag(dt)).toBe(true)
    expect(readModelDrag(dt)).toEqual({ from: 'SPX261016P7500', qty: -1 })
  })

  it('is not fooled by other drags or a mangled payload', () => {
    expect(carriesModelDrag(null)).toBe(false)
    expect(readModelDrag(null)).toBeNull()
    const text = dataTransfer()
    text.setData('text/plain', '-1')
    expect(carriesModelDrag(text)).toBe(false)
    expect(readModelDrag(text)).toBeNull()
    const broken = dataTransfer()
    broken.setData(MODEL_DRAG_TYPE, '{"from": 7, "qty": "x"}')
    expect(readModelDrag(broken)).toBeNull()
    const garbage = dataTransfer()
    garbage.setData(MODEL_DRAG_TYPE, 'not json')
    expect(readModelDrag(garbage)).toBeNull()
    const zero = dataTransfer()
    zero.setData(MODEL_DRAG_TYPE, '{"from": "SPX261016P7500", "qty": 0}')
    expect(readModelDrag(zero)).toBeNull()
  })
})
