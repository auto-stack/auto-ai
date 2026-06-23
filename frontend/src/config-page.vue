<script setup lang="ts">
import { ref, onMounted } from 'vue'

// Base URL for API calls — the origin of wherever this module was loaded from.
// This ensures fetch() hits the correct server (aaid :17654) even when the
// component is loaded remotely by auto-os-config (:17700).
const API_BASE = (() => {
  try {
    const url = new URL(import.meta.url)
    return `${url.protocol}//${url.host}`
  } catch {
    return 'http://127.0.0.1:17654'
  }
})()

interface ModelDef {
  id: string
  name: string
  tier: string
}

interface Provider {
  name: string
  kind: string
  base_url: string
  api_key_masked: string
  api_key: string
  key_env: string
  models: ModelDef[]
  max_concurrency: number
}

const listen_addr = ref('')
const idle_timeout_min = ref(10)
const log_level = ref('')
const default_provider = ref('')
const default_model = ref('')
const providers = ref<Provider[]>([])
const saveNote = ref('')
const saveOk = ref(false)

async function loadConfig() {
  try {
    const resp = await fetch(`${API_BASE}/v1/config/data`)
    const data = await resp.json()
    listen_addr.value = data.listen_addr || ''
    idle_timeout_min.value = data.idle_timeout_min || 10
    log_level.value = data.log_level || ''
    default_provider.value = data.default_provider || ''
    default_model.value = data.default_model || ''
    providers.value = (data.providers || []).map((p: any) => ({
      ...p,
      api_key: '',
    }))
  } catch (e) {
    console.error('load config failed', e)
  }
}

function addProvider() {
  providers.value.push({
    name: 'new-provider', kind: 'openai', base_url: '', api_key_masked: '',
    api_key: '', key_env: '', models: [], max_concurrency: 4,
  })
}

function removeProvider(i: number) {
  if (confirm(`Delete provider "${providers.value[i].name}"?`)) {
    providers.value.splice(i, 1)
  }
}

function addModel(i: number) {
  const id = prompt('Model ID:')
  if (id) providers.value[i].models.push({ id, name: id, tier: 'mid' })
}

function removeModel(i: number, j: number) {
  providers.value[i].models.splice(j, 1)
}

async function testProvider(i: number) {
  const p = providers.value[i]
  const model = p.models.length > 0 ? p.models[0].id : ''
  const div = document.getElementById(`test-${i}`)
  if (div) div.textContent = 'Testing...'
  try {
    const resp = await fetch(`${API_BASE}/v1/config/test`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ kind: p.kind, base_url: p.base_url, api_key: p.api_key || '', model }),
    })
    const data = await resp.json()
    if (div) {
      div.textContent = data.success ? `✓ Connected in ${data.latency_ms}ms` : `✗ ${data.error}`
      div.className = data.success ? 'test-ok' : 'test-fail'
    }
  } catch (e: any) {
    if (div) { div.textContent = `✗ ${e.message}`; div.className = 'test-fail' }
  }
}

async function saveConfig() {
  const body = {
    listen_addr: listen_addr.value,
    idle_timeout_min: idle_timeout_min.value,
    log_level: log_level.value,
    default_provider: default_provider.value,
    default_model: default_model.value,
    providers: providers.value.map(p => ({
      name: p.name, kind: p.kind, base_url: p.base_url,
      api_key: p.api_key || '', key_env: p.key_env || '',
      max_concurrency: p.max_concurrency || 4,
      models: (p.models || []).map(m => ({ id: m.id, name: m.name || m.id, tier: m.tier })),
    })),
  }
  try {
    const resp = await fetch(`${API_BASE}/v1/config/data`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body),
    })
    const data = await resp.json()
    saveOk.value = data.status === 'saved'
    saveNote.value = data.note || data.error || 'Unknown response'
  } catch (e: any) {
    saveOk.value = false
    saveNote.value = e.message
  }
}

onMounted(() => loadConfig())
</script>

<template>
  <div class="aaid-config">
    <!-- Daemon settings (read-only display) -->
    <div class="card">
      <h2>Daemon Settings</h2>
      <div class="field"><label>Listen Address</label><input :value="listen_addr" readonly></div>
      <div class="field"><label>Idle Timeout (min)</label><input :value="idle_timeout_min" readonly></div>
      <div class="field"><label>Log Level</label><input :value="log_level" readonly></div>
      <div class="field"><label>Default Provider</label><input :value="default_provider" readonly></div>
      <div class="field"><label>Default Model</label><input :value="default_model" readonly></div>
    </div>

    <!-- Providers -->
    <div class="card">
      <h2>LLM Providers</h2>
      <div v-for="(p, i) in providers" :key="i" class="provider-card">
        <div class="provider-header">
          <span class="provider-name">{{ p.name }}</span>
          <span class="provider-kind">{{ p.kind }}</span>
        </div>
        <div class="field"><label>Name</label><input v-model="p.name"></div>
        <div class="field"><label>Kind</label>
          <select v-model="p.kind">
            <option value="anthropic">anthropic</option>
            <option value="openai">openai</option>
          </select>
        </div>
        <div class="field"><label>Base URL</label><input v-model="p.base_url"></div>
        <div class="field"><label>API Key</label>
          <input type="password" v-model="p.api_key" :placeholder="p.api_key_masked || '(set via key_env)'">
        </div>
        <div class="field"><label>Key Env Var</label><input v-model="p.key_env"></div>
        <div class="field"><label>Max Concurrency</label><input type="number" v-model.number="p.max_concurrency"></div>

        <table>
          <thead><tr><th>Model ID</th><th>Tier</th><th></th></tr></thead>
          <tbody>
            <tr v-for="(m, j) in p.models" :key="j">
              <td><input v-model="m.id" class="model-input"></td>
              <td>
                <select v-model="m.tier" class="tier-select">
                  <option v-for="t in ['min','lite','mid','pro','max']" :key="t" :value="t">{{ t }}</option>
                </select>
              </td>
              <td><button class="btn-mini" @click="removeModel(i, j)">×</button></td>
            </tr>
          </tbody>
        </table>
        <button class="btn-sm" @click="addModel(i)">+ Add Model</button>

        <div :id="`test-${i}`" class="test-result"></div>

        <div class="provider-actions">
          <button class="btn-sm" @click="testProvider(i)">Test Connection</button>
          <button class="btn-sm danger" @click="removeProvider(i)">Delete</button>
        </div>
      </div>
      <button class="btn-sm" @click="addProvider">+ Add Provider</button>
    </div>

    <div class="actions">
      <button class="btn btn-primary" @click="saveConfig">Save Configuration</button>
      <button class="btn" @click="loadConfig">Reload</button>
    </div>
    <div v-if="saveNote" class="save-note" :class="{ ok: saveOk, fail: !saveOk }">
      {{ saveOk ? '✓' : '✗' }} {{ saveNote }}
    </div>
  </div>
</template>

<style scoped>
.aaid-config { max-width: 700px; }
.card { background: var(--bg-card); border: 1px solid var(--border); border-radius: 8px; padding: 20px; margin-bottom: 16px; }
.card h2 { font-size: 16px; margin-bottom: 12px; border-bottom: 1px solid #eee; padding-bottom: 8px; }
.field { display: flex; align-items: center; margin-bottom: 10px; gap: 12px; }
.field label { width: 140px; font-size: 13px; color: #555; flex-shrink: 0; }
.field input, .field select { flex: 1; padding: 6px 10px; border: 1px solid #ccc; border-radius: 4px; font-size: 13px; }
.field input:read-only { background: #f9f9f9; color: #999; }
table { width: 100%; border-collapse: collapse; margin: 8px 0; }
th, td { text-align: left; padding: 4px 8px; border-bottom: 1px solid #eee; font-size: 12px; }
.model-input { border: none; font-size: 12px; }
.provider-card { border: 1px solid #e0e0e0; border-radius: 8px; padding: 16px; margin-bottom: 12px; }
.provider-header { display: flex; align-items: center; gap: 10px; margin-bottom: 12px; }
.provider-name { font-weight: 600; }
.provider-kind { font-size: 11px; padding: 2px 8px; border-radius: 10px; background: #e8e8e8; }
.provider-actions { margin-top: 10px; display: flex; gap: 8px; }
.btn-sm { padding: 4px 10px; font-size: 12px; border: 1px solid #ddd; border-radius: 4px; background: #fff; cursor: pointer; }
.btn-sm:hover { background: #f0f0f0; }
.btn-sm.danger { color: #c42b1c; }
.btn-mini { padding: 0 6px; border: 1px solid #ddd; border-radius: 3px; background: #fff; cursor: pointer; font-size: 12px; }
.tier-select { font-size: 12px; }
.actions { display: flex; gap: 8px; margin-top: 12px; }
.btn { padding: 8px 16px; border: 1px solid #ccc; border-radius: 4px; cursor: pointer; font-size: 13px; }
.btn-primary { background: #0067c0; color: white; border-color: #0067c0; }
.save-note { padding: 8px 12px; border-radius: 4px; margin-top: 8px; font-size: 13px; }
.save-note.ok { background: #d4edda; color: #155724; }
.save-note.fail { background: #f8d7da; color: #721c24; }
.test-result { padding: 6px 10px; border-radius: 4px; margin-top: 6px; font-size: 12px; }
.test-ok { background: #e8f5e9; color: #2e7d32; }
.test-fail { background: #ffebee; color: #c62828; }
</style>
