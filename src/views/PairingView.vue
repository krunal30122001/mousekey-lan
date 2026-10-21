<script setup lang="ts">
import { onMounted, ref } from "vue";
import { api, errorText } from "../api";
import StatusBadge from "../components/StatusBadge.vue";
import { toast, useEngine } from "../composables/useEngine";
import type { PairingInfo } from "../types";

const { state, discovered, paired, blocked, act, clearPending } = useEngine();
const info = ref<PairingInfo | null>(null);
const target = ref("");
const busy = ref(false);

onMounted(async () => {
  try {
    info.value = await api.pairingInfo();
  } catch (e) {
    toast("error", errorText(e));
  }
});

async function pairManual() {
  if (!target.value.trim()) return;
  busy.value = true;
  if (await act(() => api.pairWith(target.value.trim()))) target.value = "";
  busy.value = false;
}

async function copy(text: string) {
  try {
    await navigator.clipboard.writeText(text);
    toast("success", "Copied to clipboard", 2000);
  } catch {
    toast("error", "Could not access the clipboard");
  }
}
</script>

<template>
  <div class="space-y-6">
    <header>
      <h1 class="text-2xl font-semibold">Pair devices</h1>
      <p class="mt-1 text-sm muted">
        Pairing is mutual: the other PC's user must approve the request after checking that both screens show the same code.
      </p>
    </header>

    <div
      v-if="state.pending"
      class="flex flex-wrap items-center gap-4 rounded-xl border border-indigo-300 bg-indigo-50 p-4 dark:border-indigo-800 dark:bg-indigo-950"
    >
      <div class="flex-1 text-sm">
        Waiting for <strong>{{ state.pending.name }}</strong> to approve. Check that it shows this code:
      </div>
      <div class="font-mono text-2xl font-semibold tracking-widest selectable">{{ state.pending.code }}</div>
      <button class="btn btn-ghost" @click="clearPending">Hide</button>
    </div>

    <div class="grid gap-6 lg:grid-cols-5">
      <!-- Connect to another PC -->
      <section class="card space-y-5 lg:col-span-3">
        <div>
          <h2 class="font-semibold">Computers on this network</h2>
          <p class="text-xs muted">Found automatically on your LAN via mDNS.</p>
        </div>
        <ul v-if="discovered.length" class="divide-y divide-slate-100 dark:divide-slate-800">
          <li v-for="d in discovered" :key="d.id" class="flex items-center gap-3 py-3">
            <div class="min-w-0 flex-1">
              <div class="truncate font-medium">{{ d.name }}</div>
              <div class="font-mono text-xs muted">{{ d.addr }}</div>
            </div>
            <StatusBadge :status="d.status" />
            <button class="btn btn-primary" @click="act(() => api.pairDevice(d.id))">Pair</button>
            <button class="btn btn-ghost" title="Block this device" @click="act(() => api.setBlocked(d.id, true))">Block</button>
          </li>
        </ul>
        <div v-else class="rounded-lg border border-dashed border-slate-300 p-6 text-center text-sm muted dark:border-slate-700">
          No unpaired computers found yet. Make sure MouseKey LAN is running on the other PC and both are on the same network.
          If discovery is blocked (guest Wi-Fi, VPN), connect by IP address below.
        </div>

        <div class="border-t border-slate-100 pt-5 dark:border-slate-800">
          <label class="label" for="target">Connect by IP address or pairing link</label>
          <form class="mt-2 flex gap-2" @submit.prevent="pairManual">
            <input
              id="target"
              v-model="target"
              class="input font-mono"
              placeholder="192.168.1.20  or  mkl://192.168.1.20:47820?id=…"
              autocomplete="off"
              spellcheck="false"
            />
            <button class="btn btn-primary shrink-0" :disabled="busy || !target.trim()">Pair</button>
          </form>
          <p class="mt-2 text-xs muted">
            A pairing link (copied from the other PC) also verifies the device identity, which prevents connecting to an impostor.
          </p>
        </div>
      </section>

      <!-- This PC's pairing details -->
      <section class="card space-y-4 lg:col-span-2">
        <div>
          <h2 class="font-semibold">This PC</h2>
          <p class="text-xs muted">Scan or copy this link on the other computer.</p>
        </div>
        <template v-if="info">
          <div class="mx-auto w-48 overflow-hidden rounded-lg bg-white p-1" v-html="info.qrSvg" />
          <div>
            <div class="label">Pairing link</div>
            <div class="mt-1 flex gap-2">
              <code class="flex-1 truncate rounded-md bg-slate-100 px-2 py-1.5 text-xs selectable dark:bg-slate-800">{{ info.uri }}</code>
              <button class="btn btn-secondary text-xs" @click="copy(info.uri)">Copy</button>
            </div>
          </div>
          <div class="grid grid-cols-2 gap-3 text-xs">
            <div>
              <div class="label">IP address</div>
              <div class="mt-1 font-mono selectable">{{ info.addresses.join(", ") || "—" }}</div>
            </div>
            <div>
              <div class="label">Port</div>
              <div class="mt-1 font-mono">{{ info.port }}</div>
            </div>
            <div class="col-span-2">
              <div class="label">Key fingerprint</div>
              <div class="mt-1 font-mono selectable">{{ info.fingerprint }}</div>
            </div>
          </div>
        </template>
      </section>
    </div>

    <!-- Trusted and blocked devices -->
    <div class="grid gap-6 lg:grid-cols-2">
      <section class="card">
        <h2 class="font-semibold">Trusted devices</h2>
        <ul v-if="paired.length" class="mt-3 divide-y divide-slate-100 dark:divide-slate-800">
          <li v-for="d in paired" :key="d.id" class="flex items-center gap-3 py-2.5">
            <div class="min-w-0 flex-1">
              <div class="truncate text-sm font-medium">{{ d.name }}</div>
              <div class="truncate font-mono text-[11px] muted selectable">{{ d.fingerprint }}</div>
            </div>
            <button class="btn btn-ghost text-xs" @click="act(() => api.forget(d.id), `${d.name} removed`)">Revoke</button>
            <button class="btn btn-danger text-xs" @click="act(() => api.setBlocked(d.id, true), `${d.name} blocked`)">Block</button>
          </li>
        </ul>
        <p v-else class="mt-2 text-sm muted">No trusted devices.</p>
      </section>
      <section class="card">
        <h2 class="font-semibold">Blocked devices</h2>
        <ul v-if="blocked.length" class="mt-3 divide-y divide-slate-100 dark:divide-slate-800">
          <li v-for="d in blocked" :key="d.id" class="flex items-center gap-3 py-2.5">
            <div class="min-w-0 flex-1 truncate text-sm font-medium">{{ d.name }}</div>
            <button class="btn btn-secondary text-xs" @click="act(() => api.setBlocked(d.id, false), `${d.name} unblocked`)">
              Unblock
            </button>
          </li>
        </ul>
        <p v-else class="mt-2 text-sm muted">Blocked devices can never connect or send pairing requests.</p>
      </section>
    </div>
  </div>
</template>
