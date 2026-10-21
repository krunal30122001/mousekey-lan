<script setup lang="ts">
import { api } from "../api";
import { useEngine } from "../composables/useEngine";

const { state, act } = useEngine();

function size(bytes: number) {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} KB`;
  if (bytes < 1024 ** 3) return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
  return `${(bytes / 1024 ** 3).toFixed(2)} GB`;
}

function pct(done: number, total: number) {
  return total > 0 ? Math.min(100, Math.round((done / total) * 100)) : 100;
}
</script>

<template>
  <div v-if="state.transfers.length" class="fixed bottom-4 left-60 z-30 w-80 space-y-2">
    <div v-for="t in state.transfers" :key="`${t.outgoing}-${t.peer}-${t.id}`" class="card p-3 shadow-lg">
      <div class="flex items-center justify-between gap-2 text-xs">
        <span class="truncate font-medium">
          {{ t.outgoing ? "↑" : "↓" }} {{ t.kind }}<template v-if="t.label && t.label !== t.peer">: {{ t.label }}</template>
        </span>
        <span
          class="shrink-0 font-medium"
          :class="t.state === 'failed' ? 'text-rose-600' : t.state === 'done' ? 'text-emerald-600' : 'muted'"
        >
          {{ t.state === "failed" ? "Failed" : t.state === "done" ? "Done" : `${pct(t.done, t.total)}%` }}
        </span>
      </div>
      <div class="mt-0.5 text-[11px] muted">
        {{ t.outgoing ? "to" : "from" }} {{ t.peer }} · {{ size(t.done) }} / {{ size(t.total) }}
      </div>
      <div class="mt-2 h-1.5 overflow-hidden rounded-full bg-slate-200 dark:bg-slate-700">
        <div
          class="h-full rounded-full transition-[width] duration-200"
          :class="t.state === 'failed' ? 'bg-rose-500' : t.state === 'done' ? 'bg-emerald-500' : 'bg-indigo-500'"
          :style="{ width: `${pct(t.done, t.total)}%` }"
        />
      </div>
      <button
        v-if="!t.outgoing && t.state === 'done' && t.kind === 'Dropped files'"
        class="mt-2 text-xs font-medium text-indigo-600 hover:underline dark:text-indigo-400"
        @click="act(api.openReceiveFolder)"
      >
        Open folder
      </button>
    </div>
  </div>
</template>
