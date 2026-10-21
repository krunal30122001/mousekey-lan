import { computed, reactive, readonly } from "vue";
import { api, errorText, onEngineEvent } from "../api";
import type { EngineEvent, Snapshot } from "../types";

export interface PairRequest {
  requestId: number;
  deviceId: string;
  name: string;
  addr: string;
  code: string;
}

export interface Transfer {
  id: number;
  peer: string;
  outgoing: boolean;
  kind: string;
  label: string;
  done: number;
  total: number;
  state: "active" | "done" | "failed";
}

export interface Toast {
  id: number;
  kind: "success" | "error" | "info" | "warn";
  text: string;
}

const state = reactive({
  snapshot: null as Snapshot | null,
  pairRequests: [] as PairRequest[],
  pending: null as { deviceId: string; name: string; code: string } | null,
  toasts: [] as Toast[],
  layoutRevision: 0,
  transfers: [] as Transfer[],
});

let toastId = 0;
let refreshTimer: number | undefined;
let started = false;

export function toast(kind: Toast["kind"], text: string, ms = 4500) {
  const id = ++toastId;
  state.toasts.push({ id, kind, text });
  window.setTimeout(() => dismissToast(id), ms);
}

export function dismissToast(id: number) {
  const i = state.toasts.findIndex((t) => t.id === id);
  if (i >= 0) state.toasts.splice(i, 1);
}

async function refresh() {
  try {
    state.snapshot = await api.snapshot();
  } catch (e) {
    console.error("snapshot failed", e);
  }
}

/** Coalesces bursts of `changed` events into one refresh. */
function scheduleRefresh() {
  if (refreshTimer !== undefined) return;
  refreshTimer = window.setTimeout(() => {
    refreshTimer = undefined;
    void refresh();
  }, 80);
}

function handle(ev: EngineEvent) {
  switch (ev.type) {
    case "changed":
    case "activeChanged":
      scheduleRefresh();
      if (ev.type === "changed") state.layoutRevision++;
      break;
    case "pairRequest":
      state.pairRequests.push({
        requestId: ev.requestId,
        deviceId: ev.deviceId,
        name: ev.name,
        addr: ev.addr,
        code: ev.code,
      });
      break;
    case "pairRequestClosed":
      state.pairRequests = state.pairRequests.filter((r) => r.requestId !== ev.requestId);
      break;
    case "pairPending":
      state.pending = { deviceId: ev.deviceId, name: ev.name, code: ev.code };
      break;
    case "pairResult":
      state.pending = null;
      toast(ev.success ? "success" : "error", ev.success ? ev.message : `Pairing failed: ${ev.message}`, 7000);
      scheduleRefresh();
      break;
    case "transfer": {
      const key = `${ev.outgoing ? "out" : "in"}-${ev.peer}-${ev.id}`;
      const t: Transfer = { ...ev, id: ev.id };
      const i = state.transfers.findIndex((x) => `${x.outgoing ? "out" : "in"}-${x.peer}-${x.id}` === key);
      if (i >= 0) state.transfers.splice(i, 1, t);
      else state.transfers.push(t);
      if (ev.state !== "active") {
        window.setTimeout(() => {
          const j = state.transfers.findIndex((x) => `${x.outgoing ? "out" : "in"}-${x.peer}-${x.id}` === key && x.state !== "active");
          if (j >= 0) state.transfers.splice(j, 1);
        }, 5000);
      }
      break;
    }
    case "notice":
      toast(ev.level === "warn" || ev.level === "blocked" ? "warn" : "info", ev.message, ev.level === "blocked" ? 20000 : 8000);
      break;
  }
}

/** Global engine store: snapshot + live events. Call `start()` once from App. */
export function useEngine() {
  async function start() {
    if (started) return;
    started = true;
    await onEngineEvent(handle);
    await refresh();
  }

  async function act(fn: () => Promise<unknown>, success?: string) {
    try {
      await fn();
      if (success) toast("success", success);
      await refresh();
      return true;
    } catch (e) {
      toast("error", errorText(e), 7000);
      return false;
    }
  }

  const devices = computed(() => state.snapshot?.devices ?? []);
  const paired = computed(() => devices.value.filter((d) => d.trusted && !d.blocked));
  const discovered = computed(() => devices.value.filter((d) => d.status === "discovered"));
  const blocked = computed(() => devices.value.filter((d) => d.blocked));
  const nameOf = (id: string | null | undefined) =>
    id ? (devices.value.find((d) => d.id === id)?.name ?? id) : "";

  return {
    state: readonly(state),
    start,
    refresh,
    act,
    devices,
    paired,
    discovered,
    blocked,
    nameOf,
    clearPending: () => (state.pending = null),
  };
}
