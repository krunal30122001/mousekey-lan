<script setup lang="ts">
import { computed } from "vue";
import type { DeviceStatus } from "../types";

const props = defineProps<{ status: DeviceStatus; paused?: boolean }>();

const look = computed(() => {
  if (props.paused) return { text: "Paused", dot: "bg-amber-400", cls: "text-amber-700 dark:text-amber-300" };
  switch (props.status) {
    case "online":
      return { text: "Online", dot: "bg-emerald-500", cls: "text-emerald-700 dark:text-emerald-300" };
    case "connecting":
      return { text: "Connecting", dot: "bg-sky-400 animate-pulse", cls: "text-sky-700 dark:text-sky-300" };
    case "discovered":
      return { text: "Not paired", dot: "bg-indigo-400", cls: "text-indigo-700 dark:text-indigo-300" };
    case "blocked":
      return { text: "Blocked", dot: "bg-rose-500", cls: "text-rose-700 dark:text-rose-300" };
    default:
      return { text: "Offline", dot: "bg-slate-400", cls: "text-slate-500 dark:text-slate-400" };
  }
});
</script>

<template>
  <span class="inline-flex items-center gap-1.5 text-xs font-medium" :class="look.cls">
    <span class="h-2 w-2 rounded-full" :class="look.dot" />
    {{ look.text }}
  </span>
</template>
