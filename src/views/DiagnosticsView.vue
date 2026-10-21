<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref } from "vue";
import { api } from "../api";
import { toast } from "../composables/useEngine";
import type { Diagnostics } from "../types";

const diag = ref<Diagnostics | null>(null);
let timer: number | undefined;

async function refresh() {
  try {
    diag.value = await api.diagnostics();
  } catch (e) {
    console.error(e);
  }
}

function duration(secs: number) {
  const h = Math.floor(secs / 3600);
  const m = Math.floor((secs % 3600) / 60);
  const s = secs % 60;
  return h ? `${h}h ${m}m` : m ? `${m}m ${s}s` : `${s}s`;
}

const qualityTone: Record<string, string> = {
  excellent: "text-emerald-600 dark:text-emerald-400",
  good: "text-emerald-600 dark:text-emerald-400",
  fair: "text-amber-600 dark:text-amber-400",
  poor: "text-rose-600 dark:text-rose-400",
  measuring: "muted",
};

async function copyReport() {
  if (!diag.value) return;
  try {
    await navigator.clipboard.writeText(JSON.stringify(diag.value, null, 2));
    toast("success", "Diagnostics copied", 2000);
  } catch {
    toast("error", "Could not access the clipboard");
  }
}

onMounted(() => {
  void refresh();
  timer = window.setInterval(refresh, 2000);
});
onBeforeUnmount(() => window.clearInterval(timer));
</script>

<template>
  <div v-if="diag" class="space-y-6">
    <header class="flex flex-wrap items-end justify-between gap-4">
      <div>
        <h1 class="text-2xl font-semibold">Connection diagnostics</h1>
        <p class="mt-1 text-sm muted">Live values, refreshed every 2 seconds while this page is open.</p>
      </div>
      <button class="btn btn-secondary" @click="copyReport">Copy report</button>
    </header>

    <div class="grid gap-4 sm:grid-cols-2 xl:grid-cols-4">
      <div class="card p-4">
        <div class="label">Hook latency</div>
        <div class="mt-1 text-2xl font-semibold">{{ diag.hookAvgUs.toFixed(1) }} µs</div>
        <div class="text-xs muted">avg · max {{ diag.hookMaxUs.toFixed(0) }} µs · {{ diag.hookEvents.toLocaleString() }} events</div>
      </div>
      <div class="card p-4">
        <div class="label">App resources</div>
        <div class="mt-1 text-2xl font-semibold">{{ diag.process.cpuPercent.toFixed(1) }}% CPU</div>
        <div class="text-xs muted">{{ diag.process.workingSetMb.toFixed(1) }} MB memory</div>
      </div>
      <div class="card p-4">
        <div class="label">Input</div>
        <div class="mt-1 text-2xl font-semibold">{{ diag.forwarded.toLocaleString() }}</div>
        <div class="text-xs muted">sent · {{ diag.injected.toLocaleString() }} received · {{ diag.switches }} switches</div>
      </div>
      <div class="card p-4">
        <div class="label">Reconnects</div>
        <div class="mt-1 text-2xl font-semibold">{{ diag.reconnects }}</div>
        <div class="text-xs muted">
          {{ diag.lastReconnectMs != null ? `last took ${(diag.lastReconnectMs / 1000).toFixed(1)} s` : "none since start" }}
        </div>
      </div>
    </div>

    <section class="card overflow-x-auto p-0">
      <table class="w-full text-sm">
        <thead class="border-b border-slate-200 text-left dark:border-slate-800">
          <tr class="label">
            <th class="px-5 py-3 font-semibold">Device</th>
            <th class="px-3 py-3 font-semibold">Address</th>
            <th class="px-3 py-3 font-semibold">Round trip</th>
            <th class="px-3 py-3 font-semibold">Quality</th>
            <th class="px-3 py-3 font-semibold">Connected</th>
            <th class="px-3 py-3 font-semibold">Msgs out / in</th>
            <th class="px-5 py-3 font-semibold">Dropped</th>
          </tr>
        </thead>
        <tbody class="divide-y divide-slate-100 dark:divide-slate-800">
          <tr v-for="p in diag.peers" :key="p.id">
            <td class="px-5 py-3 font-medium">
              {{ p.name }}
              <span v-if="p.remoteLocked" class="ml-1 text-xs text-amber-600">(locked)</span>
            </td>
            <td class="px-3 py-3 font-mono text-xs selectable">{{ p.addr }}</td>
            <td class="px-3 py-3">{{ p.rttMs != null ? `${p.rttMs.toFixed(2)} ms` : "—" }}</td>
            <td class="px-3 py-3 font-medium capitalize" :class="qualityTone[p.quality]">{{ p.quality }}</td>
            <td class="px-3 py-3">{{ duration(p.connectedSecs) }}</td>
            <td class="px-3 py-3 tabular-nums">{{ p.sent.toLocaleString() }} / {{ p.received.toLocaleString() }}</td>
            <td class="px-5 py-3 tabular-nums">{{ p.dropped }}</td>
          </tr>
          <tr v-if="!diag.peers.length">
            <td colspan="7" class="px-5 py-6 text-center muted">No active connections.</td>
          </tr>
        </tbody>
      </table>
    </section>

    <div class="grid gap-6 lg:grid-cols-2">
      <section class="card">
        <h2 class="font-semibold">This PC</h2>
        <dl class="mt-3 grid grid-cols-[auto_1fr] gap-x-6 gap-y-2 text-sm">
          <dt class="muted">Device ID</dt><dd class="font-mono selectable">{{ diag.deviceId }}</dd>
          <dt class="muted">Listening port</dt><dd>TCP {{ diag.listenPort }}</dd>
          <dt class="muted">Input hooks</dt><dd>{{ diag.captureRunning ? "Active" : "Not running" }}</dd>
          <dt class="muted">LAN discovery</dt><dd>{{ diag.discoveryRunning ? "Active" : "Off" }}</dd>
          <dt class="muted">Session</dt><dd>{{ diag.locked ? "Locked" : "Unlocked" }}</dd>
          <dt class="muted">Displays</dt>
          <dd>{{ diag.screens.map((s) => `${s.w}×${s.h} @ (${s.x}, ${s.y})`).join(", ") }}</dd>
          <dt class="muted">Keys held by remote</dt><dd>{{ diag.keysHeldByRemote }}</dd>
          <dt class="muted">Refused input events</dt><dd>{{ diag.unauthorized }}</dd>
          <dt class="muted">Injection errors</dt><dd>{{ diag.injectErrors }}</dd>
          <dt class="muted">Uptime</dt><dd>{{ duration(diag.uptimeSecs) }}</dd>
          <dt class="muted">Platform</dt><dd>{{ diag.os }}</dd>
          <dt class="muted">Log file</dt><dd class="font-mono text-xs break-all selectable">{{ diag.logPath }}</dd>
        </dl>
      </section>
      <section class="card text-sm">
        <h2 class="font-semibold">Troubleshooting</h2>
        <ul class="mt-3 list-disc space-y-2 pl-5 muted">
          <li><strong>Other PC not found:</strong> both PCs must be on the same network; guest Wi-Fi and some routers block mDNS. Pair by IP address instead.</li>
          <li><strong>Connection fails:</strong> allow MouseKey LAN through Windows Firewall (the installer does this) and make sure TCP {{ diag.listenPort }} isn't blocked by other security software.</li>
          <li><strong>Can't type into some windows:</strong> Windows blocks input into apps running as administrator unless MouseKey LAN also runs as administrator.</li>
          <li><strong>Locked PC:</strong> Windows doesn't allow remote input on the lock screen. Unlock with that PC's keyboard; control resumes automatically.</li>
          <li><strong>Injection errors rising:</strong> the target desktop is a secure desktop (UAC prompt, Ctrl+Alt+Del screen) or the session is locked.</li>
          <li><strong>High round trip (&gt; 40 ms):</strong> prefer wired Ethernet or 5 GHz Wi-Fi, and avoid VPNs between the PCs.</li>
        </ul>
      </section>
    </div>
  </div>
</template>
