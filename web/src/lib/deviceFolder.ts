/**
 * The File System Access API half of "On this device".
 *
 * `<input type="file" webkitdirectory>` works everywhere but forgets the
 * folder the moment the pick is over: re-syncing means asking the user to
 * find it again. Where `window.showDirectoryPicker` exists (Chromium today)
 * we keep the directory *handle* instead, so a re-sync is one click.
 *
 * The handle is held in module memory only — it dies with the page, and the
 * browser may still ask for permission again when it is used. Everything here
 * is feature-detected: Firefox and Safari simply never get a handle back and
 * fall through to re-picking with the input element.
 *
 * The types are declared locally rather than pulled from `lib.dom`: the
 * async-iterator half of the API is not in the `lib` list this project
 * compiles against, and a picker call is easier to keep honest when the shape
 * we rely on is written down.
 */

import type { DeviceEntry } from './deviceUpload'

interface FileHandleLike {
  readonly kind: 'file'
  readonly name: string
  getFile(): Promise<File>
}

interface DirectoryHandleLike {
  readonly kind: 'directory'
  readonly name: string
  values(): AsyncIterable<FileHandleLike | DirectoryHandleLike>
  queryPermission?(descriptor: { mode: 'read' }): Promise<PermissionState>
  requestPermission?(descriptor: { mode: 'read' }): Promise<PermissionState>
}

type DirectoryPicker = (options?: { mode?: 'read'; id?: string }) => Promise<DirectoryHandleLike>

/** Chromium-only today; never assume it is there. */
export function supportsDirectoryPicker(): boolean {
  if (typeof window === 'undefined') return false
  const maybe = (window as unknown as { showDirectoryPicker?: unknown }).showDirectoryPicker
  return typeof maybe === 'function'
}

function picker(): DirectoryPicker | null {
  if (!supportsDirectoryPicker()) return null
  return (window as unknown as { showDirectoryPicker: DirectoryPicker }).showDirectoryPicker
}

/** The folder the last device upload came from, while the page lives. */
let remembered: DirectoryHandleLike | null = null

/** The picked folder's name, or `null` when nothing is remembered. */
export function rememberedFolderName(): string | null {
  return remembered?.name ?? null
}

export function forgetFolder(): void {
  remembered = null
}

/** Test seam: pretend a directory handle was remembered. */
export function rememberFolderForTest(handle: unknown): void {
  remembered = handle as DirectoryHandleLike | null
}

/**
 * Walk a picked directory into upload entries. Directories are followed two
 * levels below the pick — as deep as the layout can be (`sub/Open Trades/x`)
 * and no deeper — and dot-directories are never opened at all.
 */
async function walk(
  dir: DirectoryHandleLike,
  prefix: string,
  depth: number,
  out: DeviceEntry[],
): Promise<void> {
  for await (const child of dir.values()) {
    if (child.name.startsWith('.')) continue
    const path = `${prefix}/${child.name}`
    if (child.kind === 'file') {
      out.push({ path, file: await child.getFile() })
    } else if (depth < 2) {
      await walk(child, path, depth + 1, out)
    }
  }
}

async function readable(handle: DirectoryHandleLike): Promise<boolean> {
  if (typeof handle.queryPermission !== 'function') return true
  const state = await handle.queryPermission({ mode: 'read' })
  if (state === 'granted') return true
  if (typeof handle.requestPermission !== 'function') return false
  return (await handle.requestPermission({ mode: 'read' })) === 'granted'
}

export interface DevicePick {
  folder: string
  entries: DeviceEntry[]
  /** True when the folder was remembered for one-click re-sync. */
  remembered: boolean
}

/**
 * Open the native directory picker and read it. Resolves to `null` when the
 * user dismissed the dialog — a cancel is not an error worth showing.
 */
export async function pickDirectory(): Promise<DevicePick | null> {
  const show = picker()
  if (show === null) return null
  let handle: DirectoryHandleLike
  try {
    handle = await show({ mode: 'read', id: 'wingspan-trades' })
  } catch (error) {
    if (error instanceof DOMException && error.name === 'AbortError') return null
    throw error
  }
  const entries: DeviceEntry[] = []
  await walk(handle, handle.name, 0, entries)
  remembered = handle
  return { folder: handle.name, entries, remembered: true }
}

/**
 * Re-read the remembered folder. `null` when there is nothing remembered or
 * the browser withdrew read permission — the caller then falls back to a
 * fresh pick.
 */
export async function rereadRememberedFolder(): Promise<DevicePick | null> {
  const handle = remembered
  if (handle === null) return null
  if (!(await readable(handle))) return null
  const entries: DeviceEntry[] = []
  await walk(handle, handle.name, 0, entries)
  return { folder: handle.name, entries, remembered: true }
}
