import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'

// Build ConfigPage as a standalone ESM bundle (no federation).
// The host (auto-os-config) loads it via dynamic import() from the real URL.
export default defineConfig({
  plugins: [vue()],
  build: {
    target: 'esnext',
    minify: true,
    lib: {
      entry: './src/config-page.vue',
      formats: ['es'],
      fileName: 'config-page',
    },
    rollupOptions: {
      external: [], // bundle everything (including Vue) for true independence
    },
    outDir: '../crates/auto-ai-daemon/frontend-dist',
    emptyOutDir: true,
  },
})
