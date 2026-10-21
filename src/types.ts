// Mirrors the serde types in crates/mkl-core/src/views.rs and src-tauri/src/commands.rs.

export interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}

export type DeviceStatus = "online" | "connecting" | "offline" | "discovered" | "blocked";

export interface MeView {
  id: string;
  name: string;
  port: number;
  addresses: string[];
  fingerprint: string;
  pairUri: string;
  screens: Rect[];
  version: string;
}

export interface DeviceView {
  id: string;
  name: string;
  status: DeviceStatus;
  trusted: boolean;
  blocked: boolean;
  paused: boolean;
  addr: string | null;
  lastSeen: number | null;
  pairedAt: number | null;
  rttMs: number | null;
  screens: Rect[];
  remoteLocked: boolean;
  fingerprint: string | null;
  controlling: boolean;
  controllingUs: boolean;
}

export interface Snapshot {
  me: MeView;
  devices: DeviceView[];
  activeTarget: string | null;
  controlledBy: string | null;
  locked: boolean;
  captureRunning: boolean;
  discoveryRunning: boolean;
}

export interface Settings {
  deviceName: string;
  listenPort: number;
  autostart: boolean;
  closeToTray: boolean;
  autoReconnect: boolean;
  discoveryEnabled: boolean;
  acceptPairRequests: boolean;
  edgeSwitching: boolean;
  blockCorners: boolean;
  cornerSize: number;
  switchDelayMs: number;
  returnHotkey: boolean;
  localInputReclaims: boolean;
  shareClipboard: boolean;
  shareFiles: boolean;
  maxTransferMb: number;
}

export interface LayoutDevice {
  id: string;
  name: string;
  isLocal: boolean;
  online: boolean;
  x: number;
  y: number;
  screens: Rect[];
}

export interface LayoutView {
  revision: number;
  devices: LayoutDevice[];
}

export interface PeerDiag {
  id: string;
  name: string;
  addr: string;
  rttMs: number | null;
  quality: string;
  connectedSecs: number;
  sent: number;
  received: number;
  dropped: number;
  remoteLocked: boolean;
}

export interface Diagnostics {
  deviceId: string;
  uptimeSecs: number;
  listenPort: number;
  captureRunning: boolean;
  discoveryRunning: boolean;
  locked: boolean;
  hookEvents: number;
  hookAvgUs: number;
  hookMaxUs: number;
  forwarded: number;
  injected: number;
  injectErrors: number;
  unauthorized: number;
  switches: number;
  reconnects: number;
  lastReconnectMs: number | null;
  keysHeldByRemote: number;
  screens: Rect[];
  peers: PeerDiag[];
  process: { cpuPercent: number; workingSetMb: number };
  logPath: string;
  os: string;
}

export interface PairingInfo {
  uri: string;
  qrSvg: string;
  addresses: string[];
  port: number;
  fingerprint: string;
  name: string;
}

export type EngineEvent =
  | { type: "changed" }
  | { type: "pairRequest"; requestId: number; deviceId: string; name: string; addr: string; code: string }
  | { type: "pairRequestClosed"; requestId: number }
  | { type: "pairPending"; deviceId: string; name: string; code: string }
  | { type: "pairResult"; deviceId: string | null; name: string; success: boolean; message: string }
  | { type: "activeChanged"; active: string | null }
  | { type: "notice"; level: string; message: string }
  | {
      type: "transfer";
      id: number;
      peer: string;
      outgoing: boolean;
      kind: string;
      label: string;
      done: number;
      total: number;
      state: "active" | "done" | "failed";
    };

export type ViewId = "dashboard" | "pairing" | "layout" | "settings" | "diagnostics";
