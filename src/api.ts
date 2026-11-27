import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { Diagnostics, EngineEvent, LayoutView, PairingInfo, Settings, Snapshot } from "./types";

/** Typed wrappers around the Rust commands in src-tauri/src/commands.rs. */
export const api = {
  snapshot: () => invoke<Snapshot>("get_snapshot"),
  pairWith: (target: string) => invoke<void>("pair_with", { target }),
  pairDevice: (deviceId: string) => invoke<void>("pair_device", { deviceId }),
  respondPairing: (requestId: number, approve: boolean, block = false) =>
    invoke<void>("respond_pairing", { requestId, approve, block }),
  disconnect: (deviceId: string) => invoke<void>("disconnect_device", { deviceId }),
  connect: (deviceId: string) => invoke<void>("connect_device", { deviceId }),
  forget: (deviceId: string) => invoke<void>("forget_device", { deviceId }),
  setBlocked: (deviceId: string, blocked: boolean) => invoke<void>("set_blocked", { deviceId, blocked }),
  returnControl: () => invoke<void>("return_control"),
  layout: () => invoke<LayoutView>("get_layout"),
  saveLayout: (positions: { id: string; x: number; y: number }[]) => invoke<void>("save_layout", { positions }),
  settings: () => invoke<Settings>("get_settings"),
  saveSettings: (settings: Settings) => invoke<Settings>("save_settings", { settings }),
  diagnostics: () => invoke<Diagnostics>("get_diagnostics"),
  pairingInfo: () => invoke<PairingInfo>("get_pairing_info"),
  openReceiveFolder: () => invoke<string>("open_receive_folder"),
  adminMode: () => invoke<{ elevated: boolean; enabled: boolean }>("get_admin_mode"),
  setAdminMode: (enabled: boolean) => invoke<void>("set_admin_mode", { enabled }),
  lockService: () => invoke<{ installed: boolean; running: boolean; ready: boolean }>("get_lock_service"),
  setLockService: (enabled: boolean) => invoke<void>("set_lock_service", { enabled }),
};

export function onEngineEvent(handler: (ev: EngineEvent) => void): Promise<UnlistenFn> {
  return listen<EngineEvent>("engine-event", (e) => handler(e.payload));
}

export function errorText(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  return JSON.stringify(e);
}
