/** True inside the Tauri webview. Phase 3 uses it only to hide the browser-only
 *  path input; the invoke transport arrives in Phase 5. */
export function isTauri(): boolean {
  return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window
}
