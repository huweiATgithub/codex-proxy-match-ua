<script setup>
import { computed, onMounted, ref } from "vue";
import { BaseButton, BaseInput, BaseTable, defineTableColumns } from "@codex-proxy/ui";

const dates = new Intl.DateTimeFormat(undefined, { dateStyle: "short", timeStyle: "medium" });
const columns = defineTableColumns([
  { key: "rejectedAt", label: "Rejected at", kind: "datetime", size: "xl" },
  { key: "clientKeyName", label: "Client key", size: "lg", emptyText: "Name unavailable" },
  { key: "accountName", label: "Upstream account", size: "lg", emptyText: "Name unavailable" },
  { key: "platforms", label: "Client → key OS", size: "lg",
    format: (_, row) => (row.clientPlatform || "Unknown") + " → " + (row.effectivePlatform || "Unknown") },
  { key: "requestId", label: "Request ID", kind: "mono", size: "xl" },
]);
const input = ref("");
const requestId = ref("");
const pages = ref([null]);
const rows = ref([]);
const nextCursor = ref(null);
const expanded = ref(null);
const loading = ref(false);
const error = ref("");
let sequence = 0;
const count = computed(() => rows.value.length + (rows.value.length === 1 ? " rejection" : " rejections"));
const emptyMessage = computed(() => requestId.value ? "No rejections match this request ID." : "No rejections recorded.");

function uaSample(value, count) {
  if (count === 0) return "Missing";
  if (count > 1) return count + " headers · first value: " + (value || "(empty)");
  return value || "(empty)";
}

async function load() {
  const current = ++sequence;
  const query = new URLSearchParams();
  if (requestId.value) query.set("requestId", requestId.value);
  if (pages.value.at(-1)) query.set("before", pages.value.at(-1));
  loading.value = true;
  error.value = "";
  expanded.value = null;
  nextCursor.value = null;
  try {
    const response = await window.codexProxyPlugin.request({ method: "GET", path: "api/rejections", query: query.toString() });
    const data = JSON.parse(new TextDecoder().decode(response.body));
    if (response.status !== 200) throw new Error(data.error || "Could not load rejection records.");
    if (current === sequence) {
      rows.value = data.items;
      nextCursor.value = data.nextCursor;
    }
  } catch (cause) {
    if (current === sequence) {
      rows.value = [];
      error.value = (cause.message || "Could not load rejection records.") + " Use Refresh to retry.";
    }
  } finally {
    if (current === sequence) loading.value = false;
  }
}

function search() {
  requestId.value = input.value.trim();
  pages.value = [null];
  load();
}

function previous() {
  pages.value.pop();
  load();
}

function next() {
  pages.value.push(nextCursor.value);
  load();
}

onMounted(load);
</script>

<template>
  <main>
    <form role="search" @submit.prevent="search">
      <label for="request-id">Request ID</label>
      <BaseInput id="request-id" v-model="input" class="request-input" size="sm" type="search"
        placeholder="Exact gateway request ID" autocomplete="off" spellcheck="false" />
      <BaseButton type="submit" size="sm" variant="primary">Search</BaseButton>
      <BaseButton class="refresh" size="sm" @click="load">Refresh</BaseButton>
    </form>
    <section class="results" aria-label="Rejected requests">
      <BaseTable :columns="columns" :rows="rows" row-key="rowKey" :loading="loading"
        :expanded-row-keys="expanded ? [expanded] : []" density="compact" show-header-when-empty>
        <template #rejectedAt="{ row }">
          <button class="toggle" type="button" :aria-label="'Details for ' + row.requestId"
            :aria-expanded="expanded === row.rowKey" @click="expanded = expanded === row.rowKey ? null : row.rowKey">
            <span class="chevron" aria-hidden="true"></span>
            <time :datetime="new Date(row.rejectedAt * 1000).toISOString()">{{ dates.format(row.rejectedAt * 1000) }}</time>
          </button>
        </template>
        <template #expanded="{ row }">
          <div class="details">
            <p>{{ row.message }}</p>
            <dl>
              <dt>Client User-Agent</dt><dd><code>{{ uaSample(row.clientUa, row.clientUaCount) }}</code></dd>
              <dt>Effective key User-Agent</dt><dd><code>{{ uaSample(row.effectiveUa, row.effectiveUaCount) }}</code></dd>
              <dt>Request ID</dt><dd><code>{{ row.requestId }}</code></dd>
              <dt>Model</dt><dd>{{ row.model || "Unknown" }}</dd>
              <dt>Upstream transport</dt><dd>{{ row.transport.toUpperCase() }}</dd>
              <dt>Attempt index</dt><dd>{{ row.attemptIndex }}</dd>
            </dl>
          </div>
        </template>
        <template #empty>
          <p v-if="error" role="alert">{{ error }}</p>
          <p v-else role="status">{{ emptyMessage }}</p>
        </template>
      </BaseTable>
    </section>
    <footer>
      <p v-if="rows.length && !loading" role="status">{{ count }} · Page {{ pages.length }}</p>
      <nav v-if="pages.length > 1 || nextCursor" aria-label="Result pages">
        <BaseButton size="sm" :disabled="loading || pages.length === 1" @click="previous">Previous</BaseButton>
        <BaseButton size="sm" :disabled="loading || !nextCursor" @click="next">Next</BaseButton>
      </nav>
    </footer>
  </main>
</template>

<style>
:root { color: var(--cp-color-text); font: 14px/1.5 var(--cp-font-family, Inter, system-ui, sans-serif); }
body { margin: 0; }
main { display: flex; flex-direction: column; gap: .75rem; height: var(--plugin-viewport-height, var(--cp-plugin-viewport-height, 100vh)); min-height: 16rem; }
form, footer, nav { display: flex; align-items: center; gap: .5rem; }
form { flex-wrap: wrap; }
label { font-weight: 600; }
.request-input { width: min(22rem, 100%); }
.refresh, nav { margin-inline-start: auto; }
.results { flex: 1; min-width: 0; min-height: 7rem; overflow: hidden; }
footer, dt { color: var(--cp-color-text-secondary); }
p, dl, dd { margin: 0; }
.toggle { display: inline-flex; align-items: center; gap: .6rem; cursor: pointer; color: inherit; font: inherit; }
.toggle:hover time { text-decoration: underline; }
.toggle:focus-visible { outline: 2px solid var(--cp-color-primary); outline-offset: 2px; }
.chevron { width: .45rem; height: .45rem; border: solid currentColor; border-width: 1.5px 1.5px 0 0; transform: rotate(45deg); }
.toggle[aria-expanded="true"] .chevron { transform: rotate(135deg); }
time { font-variant-numeric: tabular-nums; }
.details { width: min(100%, 100vw); padding: .75rem 1rem 1rem; overflow-wrap: anywhere; }
.details p { margin-bottom: .6rem; font-weight: 600; }
dl { display: grid; grid-template-columns: max-content minmax(0, 1fr); gap: .3rem 1rem; }
dd code { white-space: pre-wrap; }
[role="alert"] { color: var(--cp-color-error-text); }
@media (max-width: 640px) {
  form label { flex-basis: 100%; }
  .request-input { flex: 1 1 9rem; min-width: 0; }
  dl { grid-template-columns: 1fr; gap: 0; }
  dd { margin-bottom: .4rem; }
}
</style>
