/** True inside the Tauri webview. Gates the folder/token pickers, the demo
 *  book toggle, the settings/diagnostics cards, and the native menu bridge —
 *  none of which exist in the browser devserver harness. */
export function isTauri(): boolean {
  return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window
}
