import react from '@vitejs/plugin-react'
import { defineConfig } from 'vite'

// The Tauri CLI starts this dev server and points the webview at it, so the
// port is fixed and a fallback port would silently produce a blank window.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: {
      // Rust rebuilds are the Tauri CLI's job; watching them here would
      // restart the dev server on every cargo write.
      ignored: ['**/src-tauri/**'],
    },
  },
  // Ubuntu-first (§4): the weakest of the three webviews sets the target.
  // WebKitGTK 2.50 is what we develop against; don't ship syntax it lacks.
  build: {
    target: 'safari18',
    sourcemap: true,
  },
})
