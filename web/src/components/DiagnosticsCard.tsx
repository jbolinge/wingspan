/**
 * The `Diagnostics` card in Data health (desktop only, D16). Renders the
 * same checklist `wingspan-cli doctor` prints — one implementation, two
 * renderers — grouped by `section`, in the order Rust returns rows: that
 * order **is** the checklist, so nothing here re-sorts it.
 *
 * Outside the desktop app there is no `diagnostics` command — no HTTP route
 * exists and none is added — so the card renders the same read-only note
 * `SettingsCard` does, with nothing to probe.
 */

import { useState } from 'react'
import { ApiError } from '../api/client'
import {
  useDeleteDiscardedUploads,
  useDiagnostics,
  useOpenConfigFile,
  useOpenLogFolder,
} from '../api/queries'
import type { DiagnosticsGrade, DiagnosticsRow } from '../api/types'
import { isTauri } from '../lib/tauri'
import { Card, Pill, Spinner, type Tone } from './ui'

const READ_ONLY_NOTE =
  'Settings are editable in the Wingspan desktop app; this browser view is read-only.'

const GRADE_TONE: Record<DiagnosticsGrade, Tone | null> = {
  ok: 'pos',
  warn: 'warn',
  fail: 'neg',
  note: null,
}

function errorDetail(error: unknown): string {
  if (error instanceof ApiError) return error.problem?.detail ?? error.message
  return error instanceof Error ? error.message : String(error)
}

function groupBySection(rows: DiagnosticsRow[]): [string, DiagnosticsRow[]][] {
  const order: string[] = []
  const bySection = new Map<string, DiagnosticsRow[]>()
  for (const row of rows) {
    if (!bySection.has(row.section)) {
      bySection.set(row.section, [])
      order.push(row.section)
    }
    bySection.get(row.section)?.push(row)
  }
  return order.map((section) => [section, bySection.get(section) ?? []])
}

function Row({ row }: { row: DiagnosticsRow }) {
  const tone = GRADE_TONE[row.grade]
  return (
    <li className="flex flex-wrap items-baseline gap-2 border-t border-line/60 px-2 py-1.5 first:border-t-0">
      {tone ? <Pill tone={tone}>{row.grade}</Pill> : <span className="w-[3.5rem] shrink-0" />}
      <span className="text-[12px] font-medium text-fg-strong">{row.name}</span>
      <span className="text-[12px] text-muted">{row.detail}</span>
    </li>
  )
}

export function DiagnosticsCard() {
  const tauri = isTauri()
  const [probe, setProbe] = useState(false)
  const diagnostics = useDiagnostics(probe, tauri)
  const openLogFolder = useOpenLogFolder()
  const openConfigFile = useOpenConfigFile()
  const deleteUploads = useDeleteDiscardedUploads()

  const rows = diagnostics.data?.rows ?? []
  const discarded = diagnostics.data?.discarded_uploads ?? []
  const sections = groupBySection(rows)

  return (
    <Card
      title="Diagnostics"
      actions={
        tauri ? (
          <>
            <button
              type="button"
              className="ws-btn"
              disabled={diagnostics.isFetching}
              onClick={() => {
                // The first click flips the query key to `probe: true`, which
                // TanStack fetches on its own; a later click needs an
                // explicit refetch since the key no longer changes.
                if (probe) void diagnostics.refetch()
                else setProbe(true)
              }}
            >
              {diagnostics.isFetching ? 'Running…' : 'Run checks'}
            </button>
            <button type="button" className="ws-btn" onClick={() => openLogFolder.mutate()}>
              Reveal log folder
            </button>
            <button type="button" className="ws-btn" onClick={() => openConfigFile.mutate()}>
              Open config file
            </button>
          </>
        ) : null
      }
    >
      {!tauri ? (
        <p className="m-0 px-2.5 py-2 text-[11px] text-muted">{READ_ONLY_NOTE}</p>
      ) : diagnostics.isLoading ? (
        <div className="p-3">
          <Spinner />
        </div>
      ) : diagnostics.isError ? (
        <p role="alert" className="m-0 px-2.5 py-2 text-[12px] text-neg">
          {errorDetail(diagnostics.error)}
        </p>
      ) : (
        <div className="space-y-2 px-2.5 py-2">
          {sections.map(([section, sectionRows]) => (
            <div key={section}>
              <p className="ws-label m-0 mb-0.5">{section}</p>
              <ul className="m-0 list-none p-0">
                {sectionRows.map((row) => (
                  <Row key={`${row.section}-${row.name}`} row={row} />
                ))}
              </ul>
            </div>
          ))}
          {discarded.length > 0 ? (
            <div className="border-t border-line pt-2">
              <p className="ws-label m-0 mb-1">Discarded uploads</p>
              <ul className="m-0 list-none space-y-1 p-0">
                {discarded.map((path) => (
                  <li key={path} className="flex items-center gap-2">
                    <span className="num flex-1 truncate text-[11px] text-muted" title={path}>
                      {path}
                    </span>
                    <button
                      type="button"
                      className="ws-btn"
                      disabled={deleteUploads.isPending}
                      onClick={() => deleteUploads.mutate(path)}
                    >
                      Delete the old uploaded copy
                    </button>
                  </li>
                ))}
              </ul>
              {deleteUploads.isError ? (
                <p role="alert" className="m-0 mt-1 text-[11px] text-neg">
                  {errorDetail(deleteUploads.error)}
                </p>
              ) : null}
            </div>
          ) : null}
        </div>
      )}
    </Card>
  )
}
