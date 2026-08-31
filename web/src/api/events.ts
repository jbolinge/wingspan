/**
 * Server-sent events (`GET /api/v1/events`).
 *
 * `snapshot` writes straight into the market-snapshot cache (no refetch round
 * trip), `trades_changed` invalidates everything derived from the trade files,
 * and `provider` patches the source/degraded fields so the status pill reacts
 * the instant a provider flips. In mock mode the same handlers are driven by a
 * simulated tape.
 */

import { useQueryClient } from '@tanstack/react-query'
import { useEffect, useState } from 'react'
import { API_BASE, isMockActive, onMockModeChange } from './client'
import { startMockEvents } from './mock'
import { queryKeys, TRADE_DEPENDENT_KEYS } from './queries'
import type { MarketSnapshot, ProviderEvent } from './types'

export type StreamStatus = 'connecting' | 'open' | 'closed' | 'mock'

export function useEventStream(): StreamStatus {
  const client = useQueryClient()
  const [sseStatus, setSseStatus] = useState<'connecting' | 'open' | 'closed'>('connecting')
  const [mock, setMock] = useState(isMockActive())
  const supported = typeof EventSource !== 'undefined'

  useEffect(() => onMockModeChange(() => setMock(isMockActive())), [])

  useEffect(() => {
    const applySnapshot = (snapshot: MarketSnapshot): void => {
      client.setQueryData(queryKeys.snapshot, snapshot)
    }

    const invalidateTrades = (): void => {
      for (const key of TRADE_DEPENDENT_KEYS) {
        void client.invalidateQueries({ queryKey: key })
      }
    }

    if (mock) {
      return startMockEvents((event, payload) => {
        if (event === 'snapshot') applySnapshot(payload as MarketSnapshot)
        else if (event === 'trades_changed') invalidateTrades()
      })
    }

    if (typeof EventSource === 'undefined') return undefined

    const source = new EventSource(`${API_BASE}/events`)

    const parse = <T,>(event: MessageEvent): T | null => {
      try {
        return JSON.parse(event.data as string) as T
      } catch {
        return null
      }
    }

    const onSnapshot = (event: MessageEvent): void => {
      const payload = parse<MarketSnapshot>(event)
      if (payload) applySnapshot(payload)
    }

    const onTradesChanged = (): void => invalidateTrades()

    const onProvider = (event: MessageEvent): void => {
      const payload = parse<ProviderEvent>(event)
      if (!payload) return
      client.setQueryData<MarketSnapshot>(queryKeys.snapshot, (previous) =>
        previous ? { ...previous, ...payload } : previous,
      )
    }

    source.addEventListener('open', () => setSseStatus('open'))
    source.addEventListener('snapshot', onSnapshot)
    source.addEventListener('trades_changed', onTradesChanged)
    source.addEventListener('provider', onProvider)
    source.addEventListener('error', () => {
      // EventSource reconnects on its own; reflect the gap in the UI meanwhile.
      setSseStatus(source.readyState === EventSource.CLOSED ? 'closed' : 'connecting')
    })

    return () => source.close()
  }, [client, mock])

  if (mock) return 'mock'
  return supported ? sseStatus : 'closed'
}
