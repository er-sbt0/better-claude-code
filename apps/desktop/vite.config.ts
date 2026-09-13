import tailwindcss from '@tailwindcss/vite'
import react from '@vitejs/plugin-react'
import { defineConfig } from 'vite'

// The Tauri CLI starts this dev server and points the webview at it, so the
// port is fixed and a fallback port would silently produce a blank window.
export default defineConfig({
  // Tailwind v4 needs no config file and no content globs: it scans the source
  // itself, and the theme lives in `src/styles.css` under `@theme`. There is
  // deliberately no `tailwind.config.js` — a second place to put a colour is a
  // second place for it to drift.
  plugins: [react(), tailwindcss()],
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
  // Tailwind v4's floor is Safari 16.4 — comfortably under this — but the
  // generated CSS uses `@property` and `color-mix()`, so a change here is a
  // change to what the theme may express.
  build: {
    target: 'safari18',
    sourcemap: true,
  },
})
