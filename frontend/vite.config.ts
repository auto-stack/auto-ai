import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'

// Build ConfigPage as a standalone ESM bundle (no federation).
// The host (auto-os-config) loads it via dynamic import() from the real URL.
//
// `vue` is EXTERNAL: the bundle emits a bare `import 'vue'`, which the host's
// <script type="importmap"> resolves to the host's single Vue copy. Sharing one
// Vue runtime is what lets the component's reactivity (ref/onMounted/v-if) work
// when rendered inside the host — two separate Vue copies break reactivity.
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
      external: ['vue'],
    },
    outDir: '../crates/auto-ai-daemon/frontend-dist',
    emptyOutDir: true,
  },
})
