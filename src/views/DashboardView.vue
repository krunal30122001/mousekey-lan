<script setup lang="ts">
import { computed } from "vue";
import { api } from "../api";
import DeviceCard from "../components/DeviceCard.vue";
import { useEngine } from "../composables/useEngine";

const emit = defineEmits<{ navigate: [view: "pairing" | "layout"] }>();
const { state, paired, nameOf, act } = useEngine();

const me = computed(() => state.snapshot?.me);
const online = computed(() => paired.value.filter((d) => d.status === "online").length);
const activeName = computed(() => nameOf(state.snapshot?.activeTarget));
const controllerName = computed(() => nameOf(state.snapshot?.controlledBy));
</script>

<template>
  <div class="space-y-6">
    <header class="flex flex-wrap items-end justify-between gap-4">
      <div>
        <h1 class="text-2xl font-semibold">Dashboard</h1>
        <p class="mt-1 text-sm muted">
          {{ online }} of {{ paired.length }} paired {{ paired.length === 1 ? "computer" : "computers" }} online
        </p>
      </div>
      <button class="btn btn-primary" @click="emit('navigate', 'pairing')">+ Pair a computer</button>
    </header>

    <div
      v-if="state.snapshot && !state.snapshot.captureRunning"
      class="rounded-lg border border-rose-300 bg-rose-50 p-4 text-sm text-rose-900 dark:border-rose-800 dark:bg-rose-950 dark:text-rose-200"
    >
      Keyboard and mouse capture could not start on this PC, so this PC can be controlled by others but cannot control them.
      See Diagnostics and the log file for details.
    </div>

    <!-- This PC -->
    <section class="card flex flex-wrap items-center gap-6">
      <div class="flex h-14 w-14 items-center justify-center rounded-xl bg-gradient-to-br from-indigo-600 to-sky-500 text-white">
        <svg viewBox="0 0 24 24" class="h-7 w-7" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round">
          <rect x="3" y="4" width="18" height="12" rx="2" />
          <path d="M8 20h8M12 16v4" />
        </svg>
      </div>
      <div class="min-w-0 flex-1">
        <div class="label">This PC</div>
        <div class="truncate text-lg font-semibold">{{ me?.name }}</div>
        <div class="mt-0.5 font-mono text-xs muted selectable">
          {{ me?.addresses.join(", ") || "no network address" }} · port {{ me?.port }}
        </div>
      </div>
      <div class="min-w-56 rounded-lg bg-slate-100 px-4 py-3 text-sm dark:bg-slate-800">
        <template v-if="state.snapshot?.activeTarget">
          <div class="label">Controlling</div>
          <div class="font-semibold text-indigo-600 dark:text-indigo-400">{{ activeName }}</div>
          <button class="mt-1 text-xs font-medium text-indigo-600 hover:underline dark:text-indigo-400" @click="act(api.returnControl)">
            Return control here (Ctrl+Alt+Home)
          </button>
        </template>
        <template v-else-if="state.snapshot?.controlledBy">
          <div class="label">Controlled by</div>
          <div class="font-semibold text-emerald-600 dark:text-emerald-400">{{ controllerName }}</div>
          <div class="text-xs muted">Move this PC's own mouse to take control back.</div>
        </template>
        <template v-else>
          <div class="label">Active device</div>
          <div class="font-semibold">This PC</div>
          <div class="text-xs muted">Push the cursor past a screen edge to switch.</div>
        </template>
      </div>
    </section>

    <!-- Paired computers -->
    <section>
      <div class="mb-3 flex items-center justify-between">
        <h2 class="text-sm font-semibold">Paired computers</h2>
        <button v-if="paired.length" class="btn btn-ghost text-xs" @click="emit('navigate', 'layout')">Arrange monitors →</button>
      </div>
      <div v-if="paired.length" class="grid gap-4 sm:grid-cols-2 xl:grid-cols-3">
        <DeviceCard v-for="d in paired" :key="d.id" :device="d" />
      </div>
      <div v-else class="card flex flex-col items-center gap-3 py-12 text-center">
        <div class="text-base font-semibold">No computers paired yet</div>
        <p class="max-w-md text-sm muted">
          Install MouseKey LAN on another PC on the same Wi-Fi or network, then pair it here. Both PCs must approve the
          connection.
        </p>
        <button class="btn btn-primary mt-2" @click="emit('navigate', 'pairing')">Pair a computer</button>
      </div>
    </section>
  </div>
</template>
