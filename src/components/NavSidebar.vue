<script setup lang="ts">
import { useEngine } from "../composables/useEngine";
import type { ViewId } from "../types";



defineProps<{ current: ViewId }>();
defineEmits<{ select: [view: ViewId] }>();

const { state, discovered } = useEngine();

const items: { id: ViewId; label: string; icon: string }[] = [
  { id: "dashboard", label: "Dashboard", icon: "M3 12l9-9 9 9M5 10v10h5v-6h4v6h5V10" },
  { id: "pairing", label: "Pair devices", icon: "M13.8 10.2a4 4 0 010 5.6l-3 3a4 4 0 01-5.6-5.6l1.5-1.5m3.5-1.5a4 4 0 015.6-5.6l-3 3" },
  { id: "layout", label: "Monitor layout", icon: "M3 5h8v6H3zM13 5h8v10h-8zM3 13h8v6H3z" },
  { id: "settings", label: "Settings", icon: "M12 15a3 3 0 100-6 3 3 0 000 6zM19.4 15a1.7 1.7 0 00.3 1.8l.1.1a2 2 0 11-2.8 2.8l-.1-.1a1.7 1.7 0 00-2.9 1.2V21a2 2 0 01-4 0v-.1a1.7 1.7 0 00-2.9-1.2l-.1.1a2 2 0 11-2.8-2.8l.1-.1a1.7 1.7 0 00-1.2-2.9H3a2 2 0 010-4h.1a1.7 1.7 0 001.2-2.9l-.1-.1a2 2 0 112.8-2.8l.1.1a1.7 1.7 0 002.9-1.2V3a2 2 0 014 0v.1a1.7 1.7 0 002.9 1.2l.1-.1a2 2 0 112.8 2.8l-.1.1a1.7 1.7 0 001.2 2.9h.1a2 2 0 010 4h-.1a1.7 1.7 0 00-1.5 1z" },
  { id: "diagnostics", label: "Diagnostics", icon: "M3 12h4l3-8 4 16 3-8h4" },
];
</script>

<template>
  <nav class="flex w-56 shrink-0 flex-col border-r border-slate-200 bg-white px-3 py-4 dark:border-slate-800 dark:bg-slate-900">
    <div class="mb-6 flex items-center gap-2.5 px-2">
      <div class="flex h-8 w-8 items-center justify-center rounded-lg bg-gradient-to-br from-indigo-600 to-sky-500 text-white">
        <svg viewBox="0 0 24 24" class="h-5 w-5" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round">
          <rect x="2" y="5" width="8" height="6" rx="1" />
          <rect x="14" y="5" width="8" height="6" rx="1" />
          <path d="M10 8h4M12 15v4" />
        </svg>
      </div>
      <div>
        <div class="text-sm font-semibold leading-tight">MouseKey LAN</div>
        <div class="text-[11px] muted">v{{ state.snapshot?.me.version ?? "" }}</div>
      </div>
    </div>

    <button
      v-for="item in items"
      :key="item.id"
      class="mb-0.5 flex items-center gap-3 rounded-lg px-3 py-2 text-left text-sm font-medium transition-colors"
      :class="
        current === item.id
          ? 'bg-indigo-50 text-indigo-700 dark:bg-indigo-950 dark:text-indigo-300'
          : 'text-slate-600 hover:bg-slate-100 dark:text-slate-300 dark:hover:bg-slate-800'
      "
      @click="$emit('select', item.id)"
    >
      <svg viewBox="0 0 24 24" class="h-4.5 w-4.5" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
        <path :d="item.icon" />
      </svg>
      {{ item.label }}
      <span
        v-if="item.id === 'pairing' && discovered.length"
        class="ml-auto rounded-full bg-indigo-600 px-1.5 text-[11px] font-semibold text-white"
      >{{ discovered.length }}</span>
    </button>

    <div class="mt-auto space-y-1 px-2 text-[11px] muted">
      <div class="flex items-center gap-1.5">
        <span class="h-1.5 w-1.5 rounded-full" :class="state.snapshot?.captureRunning ? 'bg-emerald-500' : 'bg-rose-500'" />
        Input hooks {{ state.snapshot?.captureRunning ? "active" : "unavailable" }}
      </div>
      <div class="flex items-center gap-1.5">
        <span class="h-1.5 w-1.5 rounded-full" :class="state.snapshot?.discoveryRunning ? 'bg-emerald-500' : 'bg-slate-400'" />
        Discovery {{ state.snapshot?.discoveryRunning ? "on" : "off" }}
      </div>
    </div>
  </nav>
</template>
