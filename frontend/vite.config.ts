import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'
import type { Plugin } from 'vite'

// Build ConfigPage as a standalone ESM bundle (no federation).
// The host (auto-os-config) loads it via dynamic import() from the real URL.
//
// `vue` is EXTERNAL: the bundle emits a bare `import 'vue'`, which the host's
// <script type="importmap"> resolves to the host's single Vue copy. Sharing one
// Vue runtime is what lets the component's reactivity (ref/onMounted/v-if) work
// when rendered inside the host — two separate Vue copies break reactivity.
//
// CSS is INLINED into the JS bundle (injected as a <style> tag at runtime)
// because the remote is loaded via bare import(), so the host never loads a
// separate style.css — it would be orphaned and the component would render
// unstyled. (Latent bug once Vue was externalized.)

/** Inline all CSS emitted by the build into the JS as a runtime <style> injection.
 *  Collect CSS during `transform`, suppress the default CSS output, then inject
 *  it at the top of every JS chunk in `renderChunk`. */
function cssInjectedByJs(): Plugin {
  const cssChunks: string[] = []
  return {
    name: 'css-injected-by-js',
    apply: 'build',
    transform(code, id) {
      if (id.endsWith('.css')) {
        cssChunks.push(code)
        return { code: '', map: null }
      }
      return null
    },
    renderChunk(code) {
      if (cssChunks.length === 0) return null
      const css = cssChunks.join('\n')
      const escaped = JSON.stringify(css)
      const injector = `try{var s=document.createElement('style');s.textContent=${escaped};document.head.appendChild(s);}catch(e){}\n`
      return { code: injector + code, map: null }
    },
  }
}

export default defineConfig({
  plugins: [vue(), cssInjectedByJs()],
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
