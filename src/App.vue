<script setup lang="ts">
import { onMounted, ref } from "vue";
import NavSidebar from "./components/NavSidebar.vue";
import PairRequestDialog from "./components/PairRequestDialog.vue";
import ToastStack from "./components/ToastStack.vue";
import TransferPanel from "./components/TransferPanel.vue";
import { useEngine } from "./composables/useEngine";
import type { ViewId } from "./types";
import DashboardView from "./views/DashboardView.vue";
import DiagnosticsView from "./views/DiagnosticsView.vue";
import LayoutView from "./views/LayoutView.vue";
import PairingView from "./views/PairingView.vue";
import SettingsView from "./views/SettingsView.vue";

const { state, start } = useEngine();
const view = ref<ViewId>("dashboard");

onMounted(start);
</script>

<template>
  <div class="flex h-full">
    <NavSidebar :current="view" @select="view = $event" />
    <main class="min-w-0 flex-1 overflow-y-auto">
      <div v-if="!state.snapshot" class="flex h-full items-center justify-center text-sm muted">Starting…</div>
      <div v-else class="mx-auto max-w-6xl px-8 py-7">
        <DashboardView v-if="view === 'dashboard'" @navigate="view = $event" />
        <PairingView v-else-if="view === 'pairing'" />
        <LayoutView v-else-if="view === 'layout'" />
        <SettingsView v-else-if="view === 'settings'" />
        <DiagnosticsView v-else />
      </div>
    </main>
    <PairRequestDialog />
    <TransferPanel />
    <ToastStack />
  </div>
</template>
