import tailwindcss from '@tailwindcss/vite'
import react from '@vitejs/plugin-react'
import { defineConfig } from 'vitest/config'

// Wingspan SPA. In dev the API is proxied to the local dev API server's bind
// address; when that server is not running the API client transparently falls
// back to the typed mock layer (src/api/mock.ts) so the UI stays demo-able.
// Inside the desktop app there is no proxy at all — the transport is Tauri IPC.
export default defineConfig({
  plugins: [react(), tailwindcss()],
  // Tauri's CLI owns the terminal; clearing it would eat the Rust output.
  clearScreen: false,
  envPrefix: ['VITE_', 'TAURI_ENV_'],
  server: {
    port: 5173,
    // The desktop shell points at a fixed port; silently moving is worse
    // than failing.
    strictPort: true,
    watch: { ignored: ['**/src-tauri/**'] },
    proxy: {
      '/api': {
        target: 'http://127.0.0.1:8787',
        changeOrigin: false,
        // SSE must not be buffered.
        configure: (proxy) => {
          proxy.on('error', () => {
            /* swallowed: the client falls back to the mock layer */
          })
        },
      },
    },
  },
  build: {
    outDir: 'dist',
    // The shipped webview is WebKitGTK; `tauri dev` builds a debug bundle.
    target: 'safari18',
    sourcemap: !!process.env.TAURI_ENV_DEBUG,
    chunkSizeWarningLimit: 1200,
    rollupOptions: {
      output: {
        // Keep the charting library in its own chunk so an app-code change
        // does not invalidate it in the browser cache.
        manualChunks: (id: string) =>
          id.includes('node_modules/echarts') || id.includes('node_modules/zrender')
            ? 'echarts'
            : undefined,
      },
    },
  },
  test: {
    environment: 'jsdom',
    setupFiles: ['./vitest.setup.ts'],
    css: false,
    include: ['src/**/*.test.{ts,tsx}'],
    restoreMocks: true,
  },
})
