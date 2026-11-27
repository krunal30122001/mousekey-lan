<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { api, errorText } from "../api";
import ToggleRow from "../components/ToggleRow.vue";
import { toast, useEngine } from "../composables/useEngine";
import type { Settings } from "../types";

const { act } = useEngine();
const form = ref<Settings | null>(null);
const original = ref("");
const dirty = computed(() => form.value !== null && JSON.stringify(form.value) !== original.value);
const restartNeeded = ref(false);

async function load() {
  try {
    const s = await api.settings();
    form.value = s;
    original.value = JSON.stringify(s);
  } catch (e) {
    toast("error", errorText(e));
  }
}

async function save() {
  if (!form.value) return;
  const before = JSON.parse(original.value) as Settings;
  const after = form.value;
  if (await act(() => api.saveSettings(after), "Settings saved")) {
    restartNeeded.value ||= before.listenPort !== after.listenPort || before.discoveryEnabled !== after.discoveryEnabled;
    await load();
  }
}

// ── Run as administrator ──────────────────────────────────────────────────
const admin = ref<{ elevated: boolean; enabled: boolean } | null>(null);
const adminBusy = ref(false);

async function loadAdmin() {
  try {
    admin.value = await api.adminMode();
  } catch {
    admin.value = null;
  }
}

async function toggleAdmin() {
  if (!admin.value) return;
  const enable = !admin.value.enabled;
  if (enable && !confirm("MouseKey LAN will restart with administrator rights (Windows asks for permission). Continue?")) return;
  adminBusy.value = true;
  const ok = await act(
    () => api.setAdminMode(enable),
    enable ? "Restarting with administrator rights…" : "MouseKey LAN will start without administrator rights from now on",
  );
  adminBusy.value = false;
  if (ok) await loadAdmin();
}

// ── Lock-screen service ───────────────────────────────────────────────────
const lockSvc = ref<{ installed: boolean; running: boolean; ready: boolean } | null>(null);
const lockSvcLabel = computed(() => {
  const s = lockSvc.value;
  if (!s) return "";
  if (s.ready) return "ready";
  if (s.running) return admin.value?.elevated ? "starting…" : "needs Run as administrator";
  return s.installed ? "installed, not running" : "not installed";
});
const lockSvcBusy = ref(false);

async function loadLockSvc() {
  try {
    lockSvc.value = await api.lockService();
  } catch {
    lockSvc.value = null;
  }
}

async function toggleLockSvc() {
  if (!lockSvc.value) return;
  const enable = !lockSvc.value.installed;
  if (
    enable &&
    !confirm(
      "Windows will ask for administrator permission to install the lock-screen service.\n\n" +
      "It runs as SYSTEM and lets PCs paired with this one use its lock screen, including typing the password. " +
      "Only pair PCs you own. Continue?"
    )
  ) return;
  lockSvcBusy.value = true;
  const ok = await act(
    () => api.setLockService(enable),
    enable ? "Lock-screen service installed and running ✓" : "Lock-screen service removed",
  );
  lockSvcBusy.value = false;
  if (ok) await loadLockSvc();
}

onMounted(() => {
  void load();
  void loadAdmin();
  void loadLockSvc();
});
</script>

<template>
  <div v-if="form" class="max-w-3xl space-y-6">
    <header class="flex flex-wrap items-end justify-between gap-4">
      <div>
        <h1 class="text-2xl font-semibold">Settings</h1>
        <p class="mt-1 text-sm muted">Changes apply immediately unless marked otherwise.</p>
      </div>
      <div class="flex gap-2">
        <button class="btn btn-secondary" :disabled="!dirty" @click="load">Discard</button>
        <button class="btn btn-primary" :disabled="!dirty" @click="save">Save changes</button>
      </div>
    </header>

    <div
      v-if="restartNeeded"
      class="rounded-lg border border-amber-300 bg-amber-50 p-3 text-sm text-amber-900 dark:border-amber-800 dark:bg-amber-950 dark:text-amber-200"
    >
      Restart MouseKey LAN (tray icon → Quit, then start it again) for network changes to take effect.
    </div>

    <section class="card">
      <h2 class="font-semibold">General</h2>
      <div class="mt-3">
        <label class="label" for="name">Device name</label>
        <input id="name" v-model="form.deviceName" class="input mt-1 max-w-sm" maxlength="48" />
        <p class="mt-1 text-xs muted">Shown to other computers. Takes effect on their next connection.</p>
      </div>
      <div class="mt-3 divide-y divide-slate-100 dark:divide-slate-800">
        <ToggleRow v-model="form.autostart" label="Start automatically when I sign in" description="Runs in the tray and reconnects to trusted devices." />
        <ToggleRow v-model="form.closeToTray" label="Keep running when the window is closed" description="Close hides the window; quit from the tray icon." />
      </div>
    </section>

    <section class="card">
      <h2 class="font-semibold">Input &amp; switching</h2>
      <div class="mt-1 divide-y divide-slate-100 dark:divide-slate-800">
        <ToggleRow v-model="form.edgeSwitching" label="Switch computers at screen edges" description="Push the cursor past an edge that touches another computer in the layout." />
        <ToggleRow v-model="form.blockCorners" label="Ignore screen corners" description="Prevents accidental switches when reaching for Start, close buttons or hot corners." :disabled="!form.edgeSwitching" />
        <ToggleRow v-model="form.returnHotkey" label="Ctrl+Alt+Home returns control to this PC" description="Recommended. Always works while this PC is controlling another." />
        <ToggleRow v-model="form.localInputReclaims" label="Local mouse takes control back" description="Moving or clicking this PC's own mouse while another PC controls it hands control back here. Typing on this PC's keyboard never does." />
      </div>
      <div class="mt-3 grid gap-4 sm:grid-cols-2">
        <div>
          <label class="label" for="corner">Corner size (px)</label>
          <input id="corner" v-model.number="form.cornerSize" type="number" min="0" max="200" class="input mt-1" :disabled="!form.blockCorners" />
        </div>
        <div>
          <label class="label" for="delay">Edge switch delay (ms)</label>
          <input id="delay" v-model.number="form.switchDelayMs" type="number" min="0" max="2000" step="50" class="input mt-1" />
          <p class="mt-1 text-xs muted">0 = switch instantly. Increase if you switch by accident.</p>
        </div>
      </div>
    </section>

    <section class="card">
      <div class="flex items-center justify-between gap-4">
        <h2 class="font-semibold">Clipboard &amp; files</h2>
        <button class="btn btn-secondary text-xs" @click="act(api.openReceiveFolder)">Open received files</button>
      </div>
      <div class="mt-1 divide-y divide-slate-100 dark:divide-slate-800">
        <ToggleRow
          v-model="form.shareClipboard"
          label="Share clipboard (text and images)"
          description="Copy on one PC, paste on another. Sent to connected PCs as soon as you copy."
        />
        <ToggleRow
          v-model="form.shareFiles"
          label="Share files"
          description="Copy files in Explorer, move to the other PC and press Ctrl+V. Or drag files across the screen edge and release: they're saved to Downloads\MouseKey LAN there."
        />
      </div>
      <div class="mt-3">
        <label class="label" for="maxmb">Largest transfer (MB)</label>
        <input id="maxmb" v-model.number="form.maxTransferMb" type="number" min="1" max="100000" class="input mt-1 max-w-40" :disabled="!form.shareFiles" />
        <p class="mt-1 text-xs muted">Larger copies and drops are refused. Applies to both sending and receiving.</p>
      </div>
    </section>

    <section class="card">
      <h2 class="font-semibold">Network</h2>
      <div class="mt-1 divide-y divide-slate-100 dark:divide-slate-800">
        <ToggleRow v-model="form.autoReconnect" label="Reconnect automatically" description="Re-establish connections to trusted devices after network drops, sleep or restart." />
        <ToggleRow v-model="form.discoveryEnabled" label="LAN discovery (mDNS)" description="Advertise this PC and find others automatically. Requires restart." />
      </div>
      <div class="mt-3">
        <label class="label" for="port">Listening port</label>
        <input id="port" v-model.number="form.listenPort" type="number" min="1024" max="65535" class="input mt-1 max-w-40" />
        <p class="mt-1 text-xs muted">TCP. Default 47820. Requires restart. The installer adds a Windows Firewall rule for the app.</p>
      </div>
    </section>

    <section class="card">
      <h2 class="font-semibold">Security</h2>

      <!-- Run as administrator -->
      <div v-if="admin" class="mt-3 flex flex-wrap items-start justify-between gap-4 rounded-lg bg-slate-50 p-4 dark:bg-slate-800/60">
        <div class="max-w-lg">
          <div class="text-sm font-medium">
            Run as administrator
            <span
              class="ml-2 rounded px-1.5 py-0.5 text-[11px] font-semibold"
              :class="admin.elevated ? 'bg-emerald-100 text-emerald-700 dark:bg-emerald-950 dark:text-emerald-300' : 'bg-slate-200 text-slate-600 dark:bg-slate-700 dark:text-slate-300'"
            >
              {{ admin.elevated ? "running as admin now" : "running normally" }}
            </span>
          </div>
          <p class="mt-1 text-xs muted">
            Needed to type and click in windows that run as administrator on this PC (Task Manager, installers, admin
            terminals). Windows blocks remote input into them otherwise. Turn this on for the PC you control remotely.
          </p>
        </div>
        <button class="btn shrink-0" :class="admin.enabled ? 'btn-secondary' : 'btn-primary'" :disabled="adminBusy" @click="toggleAdmin">
          {{ admin.enabled ? "Turn off" : "Turn on" }}
        </button>
      </div>

      <!-- Lock-screen service -->
      <div v-if="lockSvc !== null" class="mt-3 flex flex-wrap items-start justify-between gap-4 rounded-lg bg-slate-50 p-4 dark:bg-slate-800/60">
        <div class="max-w-lg">
          <div class="text-sm font-medium">
            Lock-screen control
            <span
              class="ml-2 rounded px-1.5 py-0.5 text-[11px] font-semibold"
              :class="lockSvc.ready
                ? 'bg-emerald-100 text-emerald-700 dark:bg-emerald-950 dark:text-emerald-300'
                : lockSvc.installed
                  ? 'bg-amber-100 text-amber-700 dark:bg-amber-950 dark:text-amber-200'
                  : 'bg-slate-200 text-slate-600 dark:bg-slate-700 dark:text-slate-300'"
            >
              {{ lockSvcLabel }}
            </span>
          </div>
          <p class="mt-1 text-xs muted">
            Lets the PC controlling this one use its lock screen and type the password, through a small Windows
            service that runs as SYSTEM. Install it on each PC you want to unlock from another PC; it also needs
            <strong>Run as administrator</strong> (above). Works while you're signed in and the PC is locked, not after
            signing out. Ctrl+Alt+Del can't be sent.
          </p>
        </div>
        <button
          class="btn shrink-0"
          :class="lockSvc.installed ? 'btn-secondary' : 'btn-primary'"
          :disabled="lockSvcBusy"
          @click="toggleLockSvc"
        >
          <span v-if="lockSvcBusy">Working…</span>
          <span v-else>{{ lockSvc.installed ? 'Uninstall' : 'Install service' }}</span>
        </button>
      </div>

      <div class="mt-1 divide-y divide-slate-100 dark:divide-slate-800">
        <ToggleRow v-model="form.acceptPairRequests" label="Allow pairing requests" description="When off, unknown computers are rejected without asking. Already-trusted devices still connect." />
      </div>
      <ul class="mt-3 list-disc space-y-1 pl-5 text-xs muted">
        <li>All traffic is end-to-end encrypted and authenticated (Noise XX: X25519, ChaCha20-Poly1305, BLAKE2s).</li>
        <li>Only devices you approved can send input to this PC, and only while they hold control.</li>
        <li>This PC's private key is protected with Windows DPAPI. Keystrokes are never logged or stored.</li>
        <li>The Windows lock screen can be controlled with the lock-screen service (see above). The sign-in screen after signing out and UAC prompts can't.</li>
      </ul>
    </section>
  </div>
</template>
