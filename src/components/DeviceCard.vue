<script setup lang="ts">
import { computed } from "vue";
import { api } from "../api";
import { useEngine } from "../composables/useEngine";
import type { DeviceView } from "../types";
import StatusBadge from "./StatusBadge.vue";

const props = defineProps<{ device: DeviceView }>();
const { act } = useEngine();

const resolution = computed(() =>
  props.device.screens.length
    ? props.device.screens.map((s) => `${s.w}×${s.h}`).join(" + ")
    : "—",
);

const lastSeen = computed(() => {
  if (!props.device.lastSeen) return "never";
  return new Date(props.device.lastSeen * 1000).toLocaleString();
});

function forget() {
  if (confirm(`Forget ${props.device.name}? It will need to pair again before it can connect.`)) {
    void act(() => api.forget(props.device.id), `${props.device.name} forgotten`);
  }
}
</script>

<template>
  <div
    class="card flex flex-col gap-4 transition-shadow"
    :class="{
      'ring-2 ring-indigo-500': device.controlling,
      'ring-2 ring-emerald-500': device.controllingUs,
    }"
  >
    <div class="flex items-start justify-between gap-3">
      <div class="min-w-0">
        <div class="truncate font-semibold" :title="device.name">{{ device.name }}</div>
        <div class="mt-0.5 truncate font-mono text-xs muted selectable">{{ device.addr ?? device.id }}</div>
      </div>
      <StatusBadge :status="device.status" :paused="device.paused" />
    </div>

    <div v-if="device.controlling" class="rounded-md bg-indigo-50 px-3 py-1.5 text-xs font-medium text-indigo-700 dark:bg-indigo-950 dark:text-indigo-300">
      Your keyboard and mouse are controlling this PC
    </div>
    <div v-else-if="device.controllingUs" class="rounded-md bg-emerald-50 px-3 py-1.5 text-xs font-medium text-emerald-700 dark:bg-emerald-950 dark:text-emerald-300">
      This PC is being controlled by {{ device.name }}
    </div>
    <div v-if="device.remoteLocked" class="rounded-md bg-amber-50 px-3 py-1.5 text-xs text-amber-800 dark:bg-amber-950 dark:text-amber-300">
      Locked: unlock it with its own keyboard (Windows blocks remote input on the lock screen)
    </div>

    <dl class="grid grid-cols-2 gap-x-4 gap-y-2 text-xs">
      <div>
        <dt class="muted">Latency</dt>
        <dd class="font-medium">{{ device.rttMs != null ? `${device.rttMs.toFixed(1)} ms` : "—" }}</dd>
      </div>
      <div>
        <dt class="muted">Displays</dt>
        <dd class="font-medium">{{ resolution }}</dd>
      </div>
      <div class="col-span-2">
        <dt class="muted">Last seen</dt>
        <dd class="font-medium">{{ device.status === "online" ? "now" : lastSeen }}</dd>
      </div>
    </dl>

    <div class="mt-auto flex flex-wrap gap-2 border-t border-slate-100 pt-3 dark:border-slate-800">
      <button v-if="device.status === 'online'" class="btn btn-secondary" @click="act(() => api.disconnect(device.id))">
        Disconnect
      </button>
      <button v-else class="btn btn-secondary" @click="act(() => api.connect(device.id))">Connect</button>
      <button class="btn btn-danger ml-auto" @click="forget">Forget</button>
    </div>
  </div>
</template>
