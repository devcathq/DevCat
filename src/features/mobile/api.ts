// DevCat Mobile: remote access for paired phones (src-tauri/src/remote) and agent
// notifications through ntfy (src-tauri/src/notify).
// Events: "remote-status" (RemoteStatus), "remote-pair-request" (PairRequest),
// "remote-pair-resolved" ({ requestId }), "remote-pty-opened" (PtyInfo), "agent-status" (AgentStatus).
import { invoke } from "@tauri-apps/api/core";

export interface RemoteSettings {
  enabled: boolean;
  hostName?: string | null;
  port: number;
  /** Host name or IP (optionally :port) for phones outside this network. */
  publicAddr?: string | null;
  /** Also serve DevCat Mobile to phone browsers (TCP, same port). */
  web: boolean;
  /** How phones on other networks get here: iroh's public relays, your own relay, or not at all. */
  relay: RelayChoice;
  relayUrl?: string | null;
}

export type RelayChoice = "public" | "custom" | "off";

export interface ClientInfo {
  deviceId: string;
  name: string;
  platform: string;
  access: Access;
  since: number;
  addr?: string | null;
  /** How the app's connection travels right now (none for browsers). */
  path?: ConnPath | null;
}

export interface ConnPath {
  direct: boolean;
  rttMs: number;
  relay?: string | null;
}

/** "direct · 35 ms" or "through relay aps1-1 · 140 ms". */
export function pathLabel(p: ConnPath): string {
  const where = p.direct ? "direct" : `through relay ${(p.relay ?? "").replace(/^https?:\/\//, "").split(".")[0] || ""}`.trim();
  return `${where} · ${p.rttMs} ms`;
}

export interface PairRequest {
  requestId: string;
  deviceId: string;
  name: string;
  platform: string;
  addr?: string | null;
}

export interface RemoteStatus {
  settings: RemoteSettings;
  running: boolean;
  error?: string | null;
  id?: string | null;
  hostName: string;
  addrs: string[];
  clients: ClientInfo[];
  pairingExpires?: number | null;
  pending: PairRequest[];
  webRunning: boolean;
  webError?: string | null;
  /** This build includes the browser version. */
  webAvailable: boolean;
  /** The relay this computer is connected to right now. */
  relayConnected?: string | null;
}

export type Access = "full" | "readonly";

export interface Device {
  id: string;
  name: string;
  platform: string;
  access: Access;
  pairedAt: number;
  lastSeen?: number | null;
  /** "app" | "web" (a phone browser) */
  kind: string;
}

export interface PairInfo {
  /** What the QR code holds: the browser link when browser access is on, else the app link. */
  link: string;
  appLink: string;
  /** Same-network browser link (http://this computer/…). */
  webLink?: string | null;
  /** Browser link that works on any network (the hosted page, through the relay). */
  hostedLink?: string | null;
  code: string;
  svg: string;
  expiresAt: number;
}

export interface AuditEntry {
  at: number;
  device: string;
  deviceName: string;
  action: string;
  detail?: string | null;
  ok: boolean;
}

export interface NotifySettings {
  enabled: boolean;
  server: string;
  topic: string;
  token?: string | null;
  onDone: boolean;
  onInput: boolean;
  minSeconds: number;
  onlyWhenAway: boolean;
}

export interface PtyInfo {
  id: string;
  cwd: string;
  program: string;
  agent: boolean;
  origin: string;
  startedAt: number;
}

export const mobileApi = {
  status: () => invoke<RemoteStatus>("remote_status"),
  save: (settings: RemoteSettings) => invoke<RemoteStatus>("remote_save", { settings }),
  /** addr: which of the status addresses the browser link uses (default the first). */
  pairStart: (addr?: string | null) => invoke<PairInfo>("remote_pair_start", { addr: addr ?? null }),
  pairCancel: () => invoke<void>("remote_pair_cancel"),
  /** access null = deny. */
  pairAnswer: (requestId: string, access: Access | null) => invoke<void>("remote_pair_answer", { requestId, access }),
  devices: () => invoke<Device[]>("remote_devices"),
  deviceUpdate: (id: string, patch: { access?: Access; name?: string }) =>
    invoke<Device[]>("remote_device_update", { id, access: patch.access ?? null, name: patch.name ?? null }),
  deviceRevoke: (id: string) => invoke<Device[]>("remote_device_revoke", { id }),
  audit: (limit = 300) => invoke<AuditEntry[]>("remote_audit", { limit }),
  resetIdentity: () => invoke<RemoteStatus>("remote_reset_identity"),
  /** QR code (SVG markup) for a short text. */
  qr: (text: string) => invoke<string>("remote_qr", { text }),

  notifySettings: () => invoke<NotifySettings>("notify_settings"),
  notifySave: (settings: NotifySettings) => invoke<NotifySettings>("notify_save", { settings }),
  notifyNewTopic: () => invoke<NotifySettings>("notify_new_topic"),
  notifyTest: () => invoke<void>("notify_test"),
};

/** The ntfy web/app link for a topic (the ntfy app opens it, or offers to subscribe). */
export function ntfyLink(s: NotifySettings): string {
  return `${s.server.replace(/\/+$/, "")}/${s.topic}`;
}

export const MOBILE_RELEASES_URL = "https://github.com/devcathq/DevCat/releases/latest";
export const NTFY_IOS_URL = "https://apps.apple.com/app/ntfy/id1625396347";
export const NTFY_ANDROID_URL = "https://play.google.com/store/apps/details?id=io.heckel.ntfy";
