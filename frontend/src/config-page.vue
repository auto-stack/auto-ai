<script setup lang="ts">
import { ref, reactive, onMounted } from 'vue'

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

interface TestState {
  status: 'idle' | 'testing' | 'ok' | 'fail'
  message: string
  latency?: number
}

const TIERS = ['min', 'lite', 'mid', 'pro', 'max'] as const

// Per-tier accent for the badge. Each gets its own hue so tiers are glanceable,
// but they stay muted so they don't fight the theme accent.
const TIER_COLORS: Record<string, string> = {
  min: '#9ca3af',
  lite: '#38bdf8',
  mid: '#6366f1',
  pro: '#a855f7',
  max: '#ec4899',
}

const listen_addr = ref('')
const idle_timeout_min = ref(10)
const log_level = ref('')
const default_provider = ref('')
const default_model = ref('')
const providers = ref<Provider[]>([])
const saveNote = ref('')
const saveOk = ref(false)
const saving = ref(false)
// per-provider test result, keyed by index
const testStates = reactive<Record<number, TestState>>({})

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
    delete testStates[i]
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
  testStates[i] = { status: 'testing', message: 'Connecting…' }
  try {
    const resp = await fetch(`${API_BASE}/v1/config/test`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ kind: p.kind, base_url: p.base_url, api_key: p.api_key || '', model }),
    })
    const data = await resp.json()
    if (data.success) {
      testStates[i] = { status: 'ok', message: `Connected`, latency: data.latency_ms }
    } else {
      testStates[i] = { status: 'fail', message: data.error || 'Connection failed' }
    }
  } catch (e: any) {
    testStates[i] = { status: 'fail', message: e.message }
  }
}

async function saveConfig() {
  saving.value = true
  saveNote.value = ''
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
    // reload to reflect server-normalized state (masked keys, defaults)
    if (saveOk.value) await loadConfig()
  } catch (e: any) {
    saveOk.value = false
    saveNote.value = e.message
  } finally {
    saving.value = false
  }
}

onMounted(() => loadConfig())
</script>

<template>
  <div class="aaid-config">
    <!-- Daemon settings (read-only) -->
    <section class="card">
      <header class="card-head">
        <h2>Daemon</h2>
        <span class="card-sub">Runtime configuration of the AI daemon process</span>
      </header>
      <dl class="kv">
        <div class="kv-row"><dt>Listen address</dt><dd class="mono">{{ listen_addr || '—' }}</dd></div>
        <div class="kv-row"><dt>Idle timeout</dt><dd>{{ idle_timeout_min }} min</dd></div>
        <div class="kv-row"><dt>Log level</dt><dd>{{ log_level || '—' }}</dd></div>
        <div class="kv-row"><dt>Default provider</dt><dd class="mono">{{ default_provider || '—' }}</dd></div>
        <div class="kv-row"><dt>Default model</dt><dd class="mono">{{ default_model || '—' }}</dd></div>
      </dl>
    </section>

    <!-- Providers -->
    <section class="card">
      <header class="card-head">
        <h2>LLM Providers</h2>
        <span class="card-sub">{{ providers.length }} configured</span>
      </header>

      <div v-for="(p, i) in providers" :key="i" class="provider-card">
        <div class="provider-head">
          <span class="provider-name">{{ p.name || 'unnamed' }}</span>
          <span class="badge">{{ p.kind }}</span>
          <span v-if="p.base_url" class="provider-url mono">{{ p.base_url }}</span>
          <span class="spacer"></span>
          <button class="btn-link danger" @click="removeProvider(i)">Delete</button>
        </div>

        <div class="grid">
          <div class="field">
            <label>Name</label>
            <input v-model="p.name" type="text" />
          </div>
          <div class="field">
            <label>Kind</label>
            <select v-model="p.kind">
              <option value="anthropic">anthropic</option>
              <option value="openai">openai</option>
            </select>
          </div>
          <div class="field span-2">
            <label>Base URL</label>
            <input v-model="p.base_url" type="text" placeholder="https://api.example.com/v1" />
          </div>
          <div class="field span-2">
            <label>API Key</label>
            <input v-model="p.api_key" type="password" :placeholder="p.api_key_masked || '(set via key_env)'" />
          </div>
          <div class="field">
            <label>Key Env Var</label>
            <input v-model="p.key_env" type="text" placeholder="PROVIDER_API_KEY" />
          </div>
          <div class="field">
            <label>Concurrency</label>
            <input v-model.number="p.max_concurrency" type="number" min="1" />
          </div>
        </div>

        <!-- Models -->
        <div class="models">
          <div class="models-head">
            <span class="models-title">Models <span class="muted">({{ p.models.length }})</span></span>
            <button class="btn-mini" @click="addModel(i)">+ Add</button>
          </div>
          <div v-if="p.models.length === 0" class="empty">No models configured.</div>
          <div v-for="(m, j) in p.models" :key="j" class="model-row">
            <input v-model="m.id" type="text" class="model-id mono" />
            <select v-model="m.tier" class="tier-select" :style="{ color: TIER_COLORS[m.tier] }">
              <option v-for="t in TIERS" :key="t" :value="t">{{ t }}</option>
            </select>
            <span class="tier-dot" :style="{ background: TIER_COLORS[m.tier] }"></span>
            <button class="btn-link" @click="removeModel(i, j)">Remove</button>
          </div>
        </div>

        <!-- Test connection -->
        <div class="test-row">
          <button class="btn-sm" :disabled="testStates[i]?.status === 'testing'" @click="testProvider(i)">
            {{ testStates[i]?.status === 'testing' ? 'Testing…' : 'Test Connection' }}
          </button>
          <span v-if="testStates[i] && testStates[i].status !== 'idle'" class="test-result" :class="testStates[i].status">
            <template v-if="testStates[i].status === 'ok'">✓ {{ testStates[i].message }} <span class="muted">({{ testStates[i].latency }}ms)</span></template>
            <template v-else-if="testStates[i].status === 'fail'">✗ {{ testStates[i].message }}</template>
            <template v-else>{{ testStates[i].message }}</template>
          </span>
        </div>
      </div>

      <button class="btn-sm ghost add-provider" @click="addProvider">+ Add Provider</button>
    </section>

    <!-- Save bar -->
    <div class="save-bar">
      <button class="btn btn-primary" :disabled="saving" @click="saveConfig">
        {{ saving ? 'Saving…' : 'Save Configuration' }}
      </button>
      <button class="btn" @click="loadConfig">Reload</button>
    </div>
    <div v-if="saveNote" class="save-note" :class="{ ok: saveOk, fail: !saveOk }">
      <span class="note-icon">{{ saveOk ? '✓' : '✗' }}</span>{{ saveNote }}
    </div>
  </div>
</template>

<style scoped>
/* All colors reference the host's theme variables (set on :root), so this page
   follows the sidebar accent picker automatically. Neutral surfaces use the
   host's --bg-card / --border / --text-* tokens. */
.aaid-config { max-width: 760px; }

.card {
  background: var(--bg-card);
  border: 1px solid var(--border);
  border-radius: var(--radius, 8px);
  padding: 20px 22px;
  margin-bottom: 16px;
}
.card-head {
  display: flex;
  align-items: baseline;
  gap: 10px;
  margin-bottom: 16px;
}
.card-head h2 {
  font-size: 15px;
  font-weight: 600;
  margin: 0;
}
.card-sub {
  font-size: 12px;
  color: var(--text-muted, #8a8a8a);
}

/* Read-only key/value list (replaces the old disabled inputs) */
.kv { display: flex; flex-direction: column; }
.kv-row {
  display: flex;
  align-items: baseline;
  padding: 7px 0;
  border-bottom: 1px solid var(--border);
}
.kv-row:last-child { border-bottom: none; }
.kv-row dt {
  width: 160px;
  font-size: 13px;
  color: var(--text-secondary, #616161);
  font-weight: 400;
}
.kv-row dd {
  font-size: 13px;
  color: var(--text-primary, #1a1a1a);
  flex: 1;
}
.mono { font-family: ui-monospace, 'SFMono-Regular', Menlo, Consolas, monospace; font-size: 12px; }

/* Provider card */
.provider-card {
  border: 1px solid var(--border);
  border-radius: var(--radius-sm, 6px);
  padding: 16px;
  margin-bottom: 12px;
  background: var(--bg-app, #fafafa);
}
.provider-head {
  display: flex;
  align-items: center;
  gap: 10px;
  margin-bottom: 14px;
  padding-bottom: 12px;
  border-bottom: 1px solid var(--border);
}
.provider-name { font-weight: 600; font-size: 14px; }
.provider-url { font-size: 11px; color: var(--text-muted, #8a8a8a); max-width: 220px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.spacer { flex: 1; }
.badge {
  font-size: 11px;
  padding: 2px 9px;
  border-radius: 10px;
  background: var(--accent-light, #eee);
  color: var(--accent, #0067c0);
  font-weight: 500;
}

/* Form grid */
.grid {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 10px 16px;
  margin-bottom: 14px;
}
.field { display: flex; flex-direction: column; gap: 4px; }
.field.span-2 { grid-column: span 2; }
.field label {
  font-size: 11px;
  color: var(--text-muted, #8a8a8a);
  font-weight: 500;
  text-transform: uppercase;
  letter-spacing: 0.03em;
}
.field input, .field select {
  padding: 7px 10px;
  border: 1px solid var(--border);
  border-radius: var(--radius-sm, 4px);
  font-size: 13px;
  background: var(--bg-input, #fff);
  outline: none;
  transition: border-color 0.15s, box-shadow 0.15s;
}
.field input:focus, .field select:focus {
  border-color: var(--accent, #0067c0);
  box-shadow: 0 0 0 3px var(--accent-light, rgba(0,103,192,0.15));
}

/* Models */
.models { margin-top: 4px; }
.models-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 8px;
}
.models-title { font-size: 12px; color: var(--text-secondary, #616161); font-weight: 600; text-transform: uppercase; letter-spacing: 0.03em; }
.muted { color: var(--text-muted, #8a8a8a); }
.empty { font-size: 12px; color: var(--text-muted, #8a8a8a); padding: 8px 0; }
.model-row {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 5px 0;
}
.model-id {
  flex: 1;
  padding: 5px 9px;
  border: 1px solid var(--border);
  border-radius: var(--radius-sm, 4px);
  font-size: 12px;
  outline: none;
  background: var(--bg-input, #fff);
  transition: border-color 0.15s, box-shadow 0.15s;
}
.model-id:focus { border-color: var(--accent); box-shadow: 0 0 0 3px var(--accent-light); }
.tier-select {
  padding: 5px 8px;
  border: 1px solid var(--border);
  border-radius: var(--radius-sm, 4px);
  font-size: 12px;
  font-weight: 600;
  text-transform: capitalize;
  background: var(--bg-input, #fff);
  outline: none;
  width: 80px;
}
.tier-dot { width: 8px; height: 8px; border-radius: 50%; flex-shrink: 0; }

/* Test connection */
.test-row { display: flex; align-items: center; gap: 12px; margin-top: 12px; padding-top: 12px; border-top: 1px solid var(--border); }
.test-result { font-size: 12px; }
.test-result.ok { color: var(--success, #107c10); }
.test-result.fail { color: var(--danger, #c42b1c); }
.test-result.testing { color: var(--text-muted, #8a8a8a); }

/* Buttons */
.btn { padding: 8px 18px; border: 1px solid var(--border); border-radius: var(--radius-sm, 4px); background: var(--bg-card, #fff); cursor: pointer; font-size: 13px; font-weight: 500; transition: background 0.15s, border-color 0.15s; }
.btn:hover { background: var(--bg-hover, #ededed); }
.btn:disabled { opacity: 0.6; cursor: not-allowed; }
.btn-primary {
  background: var(--accent, #0067c0);
  color: var(--accent-foreground, #fff);
  border-color: var(--accent, #0067c0);
}
.btn-primary:hover { background: var(--accent-hover, #0078d4); }
.btn-primary:disabled { background: var(--accent, #0067c0); border-color: var(--accent, #0067c0); }
.btn-sm { padding: 6px 12px; font-size: 12px; border: 1px solid var(--border); border-radius: var(--radius-sm, 4px); background: var(--bg-card, #fff); cursor: pointer; font-weight: 500; transition: background 0.15s, border-color 0.15s; }
.btn-sm:hover:not(:disabled) { background: var(--bg-hover, #ededed); border-color: var(--accent, #0067c0); color: var(--accent); }
.btn-sm:disabled { opacity: 0.6; cursor: not-allowed; }
.btn-sm.ghost { border-style: dashed; background: transparent; }
.btn-mini { padding: 3px 9px; font-size: 11px; border: 1px solid var(--border); border-radius: var(--radius-sm, 4px); background: transparent; cursor: pointer; color: var(--text-secondary, #616161); }
.btn-mini:hover { border-color: var(--accent); color: var(--accent); }
.btn-link { background: none; border: none; cursor: pointer; font-size: 12px; color: var(--text-secondary, #616161); padding: 2px 4px; }
.btn-link:hover { color: var(--accent); }
.btn-link.danger:hover { color: var(--danger, #c42b1c); }
.add-provider { margin-top: 4px; }

/* Save bar */
.save-bar { display: flex; gap: 10px; margin-top: 4px; }
.save-note {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 10px 14px;
  border-radius: var(--radius-sm, 4px);
  margin-top: 10px;
  font-size: 13px;
  border: 1px solid;
}
.save-note .note-icon { font-weight: 700; }
.save-note.ok { background: var(--accent-light, #e8f5e9); color: var(--accent, #2e7d32); border-color: var(--accent-light); }
.save-note.fail { background: rgba(196, 43, 28, 0.08); color: var(--danger, #c42b1c); border-color: rgba(196, 43, 28, 0.2); }
</style>
