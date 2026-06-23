import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'
import federation from '@originjs/vite-plugin-federation'

export default defineConfig({
  plugins: [
    vue(),
    federation({
      name: 'aaid-config',
      exposes: {
        ConfigPage: './src/config-page.vue',
      },
      shared: ['vue'],
    }),
  ],
  build: {
    target: 'esnext',
    minify: true,
    // Output to a directory aaid serves as static files.
    outDir: '../crates/auto-ai-daemon/frontend-dist',
    emptyOutDir: true,
  },
})
