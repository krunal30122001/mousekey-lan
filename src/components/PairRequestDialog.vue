<script setup lang="ts">
import { computed } from "vue";
import { api } from "../api";
import { useEngine } from "../composables/useEngine";

const { state, act } = useEngine();
const request = computed(() => state.pairRequests[0] ?? null);

function respond(approve: boolean, block = false) {
  const r = request.value;
  if (!r) return;
  void act(() => api.respondPairing(r.requestId, approve, block), approve ? undefined : block ? `${r.name} blocked` : undefined);
}
</script>

<template>
  <div v-if="request" class="fixed inset-0 z-40 flex items-center justify-center bg-slate-950/50 p-4 backdrop-blur-sm">
    <div class="card w-full max-w-md p-6 shadow-2xl" role="dialog" aria-modal="true" aria-labelledby="pair-title">
      <h2 id="pair-title" class="text-lg font-semibold">Pairing request</h2>
      <p class="mt-2 text-sm muted">
        <strong class="text-slate-900 dark:text-slate-100">{{ request.name }}</strong>
        ({{ request.addr }}) wants permission to control this PC with its keyboard and mouse.
      </p>
      <div class="mt-5 rounded-lg bg-slate-100 p-4 text-center dark:bg-slate-800">
        <div class="label">Verification code</div>
        <div class="mt-1 font-mono text-3xl font-semibold tracking-widest selectable">{{ request.code }}</div>
        <p class="mt-2 text-xs muted">Only approve if the other PC shows exactly the same code.</p>
      </div>
      <div class="mt-6 flex flex-wrap items-center justify-between gap-2">
        <button class="btn btn-danger" @click="respond(false, true)">Block device</button>
        <div class="flex gap-2">
          <button class="btn btn-secondary" @click="respond(false)">Reject</button>
          <button class="btn btn-primary" @click="respond(true)">Codes match, approve</button>
        </div>
      </div>
    </div>
  </div>
</template>
