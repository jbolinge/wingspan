/**
 * Reading a trades folder that lives on the *viewing* device.
 *
 * The browser hands us a flat list of files with a path relative to the folder
 * the user picked (`File.webkitRelativePath`, or the walk of a File System
 * Access directory handle). These pure functions mirror the server's upload
 * filter from `docs/API.md` — the three folder names matched
 * case-insensitively, the same one-level descent as discovery — so the user
 * sees what will be sent *before* anything leaves the machine, and the parts
 * we do send carry exactly the relative paths the contract asks for.
 *
 * Nothing here touches the network or the DOM; `TradesDeviceTab` renders it.
 */

/** The three folders discovery looks for, in the canonical casing. */
export const TRADE_DIR_NAMES = ['Open Trades', 'Closed Trades', 'Daily Trade Statement'] as const

/** Server caps (docs/API.md). Exceeding one is a 413 naming the cap. */
export const MAX_FILE_BYTES = 4 * 1024 * 1024
export const MAX_FILES = 400
export const MAX_TOTAL_BYTES = 32 * 1024 * 1024

export type DeviceFileKind = 'trade' | 'statement'

/** Why a picked file will not be uploaded. */
export type SkipReason = 'dotfile' | 'outside-layout' | 'too-large'

/** One file as the browser handed it over, path *including* the picked folder. */
export interface DeviceEntry {
  /** e.g. `Trades/Open Trades/M1 July 14 2026`. */
  path: string
  file: File
}

export interface KeptFile {
  /** Path relative to the picked folder — this is the multipart filename. */
  relativePath: string
  kind: DeviceFileKind
  bytes: number
  file: File
}

export interface SkippedFile {
  /** Path relative to the picked folder when there is one, else the raw path. */
  relativePath: string
  reason: SkipReason
  bytes: number
}

export interface DeviceSelection {
  /** The picked folder's own name, or `''` when the pick was empty. */
  folder: string
  kept: KeptFile[]
  skipped: SkippedFile[]
  tradeFiles: number
  statements: number
  /** Total bytes of the kept files only. */
  bytes: number
  /** A server cap the kept set already busts — a 413 waiting to happen. */
  overCap: 'files' | 'bytes' | null
}

/**
 * Split `Trades/Open Trades/M1` into the picked folder and the rest.
 * A bare filename has no folder segment and can never be in the layout.
 */
export function stripPickedFolder(path: string): { folder: string; relativePath: string } | null {
  const parts = path.split('/')
  if (parts.length < 2) return null
  const folder = parts[0] as string
  const relativePath = parts.slice(1).join('/')
  if (folder === '' || relativePath === '') return null
  return { folder, relativePath }
}

/**
 * The server's keep-rule: the file sits directly in one of the three folders,
 * either at the root of the pick (`Open Trades/x`) or one level down
 * (`2026/Open Trades/x`). Anything else is `null` — skipped, never uploaded.
 */
export function layoutKind(relativePath: string): DeviceFileKind | null {
  const parts = relativePath.split('/')
  if (parts.length < 2 || parts.length > 3) return null
  if (parts.some((part) => part === '')) return null
  const dir = (parts[parts.length - 2] as string).toLowerCase()
  const match = TRADE_DIR_NAMES.find((name) => name.toLowerCase() === dir)
  if (match === undefined) return null
  return match === 'Daily Trade Statement' ? 'statement' : 'trade'
}

function isDotted(relativePath: string): boolean {
  return relativePath.split('/').some((part) => part.startsWith('.'))
}

/** `FileList` (or any array of files) → entries keyed by their picked path. */
export function entriesFromFileList(files: ArrayLike<File>): DeviceEntry[] {
  const out: DeviceEntry[] = []
  for (let i = 0; i < files.length; i += 1) {
    const file = files[i] as File
    out.push({ path: file.webkitRelativePath || file.name, file })
  }
  return out
}

/**
 * Decide, for every picked file, whether it is part of the snapshot. Skip
 * reasons are checked in the order a person would explain them: hidden files
 * first, then "not part of the trades layout", then the per-file size cap.
 */
export function buildSelection(entries: readonly DeviceEntry[]): DeviceSelection {
  const kept: KeptFile[] = []
  const skipped: SkippedFile[] = []
  let folder = ''
  let bytes = 0

  for (const entry of entries) {
    const split = stripPickedFolder(entry.path)
    const relativePath = split?.relativePath ?? entry.path
    const bytesOf = entry.file.size
    if (folder === '' && split !== null) folder = split.folder

    if (split === null || isDotted(relativePath)) {
      const reason: SkipReason = split !== null && isDotted(relativePath) ? 'dotfile' : 'outside-layout'
      skipped.push({ relativePath, reason, bytes: bytesOf })
      continue
    }
    const kind = layoutKind(relativePath)
    if (kind === null) {
      skipped.push({ relativePath, reason: 'outside-layout', bytes: bytesOf })
      continue
    }
    if (bytesOf > MAX_FILE_BYTES) {
      skipped.push({ relativePath, reason: 'too-large', bytes: bytesOf })
      continue
    }
    kept.push({ relativePath, kind, bytes: bytesOf, file: entry.file })
    bytes += bytesOf
  }

  return {
    folder,
    kept,
    skipped,
    tradeFiles: kept.filter((file) => file.kind === 'trade').length,
    statements: kept.filter((file) => file.kind === 'statement').length,
    bytes,
    overCap: kept.length > MAX_FILES ? 'files' : bytes > MAX_TOTAL_BYTES ? 'bytes' : null,
  }
}

/**
 * The multipart body of `POST /api/v1/trades/upload`: one `file` part per kept
 * file, the part's filename being its path relative to the picked folder.
 */
export function buildUploadForm(selection: DeviceSelection): FormData {
  const form = new FormData()
  for (const file of selection.kept) form.append('file', file.file, file.relativePath)
  return form
}

/** Byte counts in the same units the caps are written in. */
export function fmtBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return '—'
  if (bytes < 1024) return `${Math.round(bytes)} B`
  const kib = bytes / 1024
  if (kib < 1024) return `${kib.toFixed(kib < 10 ? 1 : 0)} KiB`
  return `${(kib / 1024).toFixed(1)} MiB`
}

/** One line for the summary card: what would be uploaded, in plain words. */
export function selectionSentence(selection: DeviceSelection): string {
  const files = `${selection.tradeFiles} trade file${selection.tradeFiles === 1 ? '' : 's'}`
  const statements = `${selection.statements} statement${selection.statements === 1 ? '' : 's'}`
  return `${files} / ${statements}, ${selection.skipped.length} skipped, ${fmtBytes(selection.bytes)}`
}

/** Why the pre-flight refuses to send — `null` when it is good to go. */
export function capMessage(selection: DeviceSelection): string | null {
  if (selection.overCap === 'files') {
    return `${selection.kept.length} files — the server accepts at most ${MAX_FILES} per upload.`
  }
  if (selection.overCap === 'bytes') {
    return `${fmtBytes(selection.bytes)} — the server accepts at most ${fmtBytes(MAX_TOTAL_BYTES)} per upload.`
  }
  return null
}
