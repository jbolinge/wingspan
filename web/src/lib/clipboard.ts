/**
 * Put one string on the system clipboard — the Model ticket's order lines,
 * pasted from there into thinkorswim.
 *
 * Inside the desktop app it is the `copy_text` command first: Rust owns the
 * X11 selection for as long as the app runs, with no clipboard plugin
 * permission to grant. That command speaks X11 only, so on a Wayland session
 * with no XWayland it fails — then, and only then, the webview's own
 * `navigator.clipboard` (GTK's clipboard, whichever backend GTK is on) gets a
 * turn before the failure is reported. In the browser — the devserver loop,
 * Playwright, vitest — it is `navigator.clipboard.writeText` alone. Either
 * way a failure rejects, so the caller can say "Couldn't copy: …" instead of
 * a button that silently did nothing.
 */

import { call } from '../api/client'
import type { CopyTextRequest } from '../api/types'
import { isTauri } from './tauri'

export async function copyText(text: string): Promise<void> {
  if (isTauri()) {
    const request: CopyTextRequest = { text }
    try {
      await call<null>('copy_text', { request })
      return
    } catch (error) {
      // The command's problem is the one worth reading if the webview cannot
      // help either; a webview without the API has nothing to add.
      if (!navigator.clipboard) throw error
      try {
        await navigator.clipboard.writeText(text)
        return
      } catch {
        throw error
      }
    }
  }
  // An insecure origin (a LAN address over plain http) has no clipboard API
  // at all; say so rather than throw a TypeError about `undefined`.
  if (typeof navigator === 'undefined' || !navigator.clipboard) {
    throw new Error('the clipboard is not available in this browser')
  }
  await navigator.clipboard.writeText(text)
}
