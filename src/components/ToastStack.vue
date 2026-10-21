<script setup lang="ts">
import { dismissToast, useEngine } from "../composables/useEngine";

const { state } = useEngine();

const tone: Record<string, string> = {
  success: "border-emerald-300 bg-emerald-50 text-emerald-900 dark:border-emerald-800 dark:bg-emerald-950 dark:text-emerald-100",
  error: "border-rose-300 bg-rose-50 text-rose-900 dark:border-rose-800 dark:bg-rose-950 dark:text-rose-100",
  warn: "border-amber-300 bg-amber-50 text-amber-900 dark:border-amber-800 dark:bg-amber-950 dark:text-amber-100",
  info: "border-slate-300 bg-white text-slate-900 dark:border-slate-700 dark:bg-slate-900 dark:text-slate-100",
};
</script>

<template>
  <div class="pointer-events-none fixed right-4 bottom-4 z-50 flex w-96 max-w-[calc(100vw-2rem)] flex-col gap-2">
    <div
      v-for="t in state.toasts"
      :key="t.id"
      class="pointer-events-auto flex items-start gap-3 rounded-lg border px-4 py-3 text-sm shadow-lg"
      :class="tone[t.kind]"
      role="status"
    >
      <span class="flex-1 selectable">{{ t.text }}</span>
      <button class="text-current opacity-60 hover:opacity-100" aria-label="Dismiss" @click="dismissToast(t.id)">✕</button>
    </div>
  </div>
</template>
