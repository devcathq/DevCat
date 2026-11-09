// The browser versions (`vite --mode web` / `--mode hosted`): stand in for @tauri-apps/* so the
// same screens run in a phone browser. The phone-side commands (m_*) are answered here by
// WebHost (talks to the DevCat that served the page) or, in the hosted version, RelayHost
// (through the computer's relay); camera and notifications are stubs (the QR code is scanned
// with the phone's camera app, notifications come through ntfy).
import { CODE_LINK } from "../lib/env";
import { WebHost, carryPairing, deviceName, platform, saved as webSaved, setDeviceName } from "./client";
import type { RelayHost, RelaySaved } from "./relay";

type Handler = (e: { event: string; payload: unknown }) => void;
const handlers = new Map<string, Set<Handler>>();
function emit(event: string, payload: unknown) {
  handlers.get(event)?.forEach((h) => h({ event, payload }));
  if (event === "m-conn" && (payload as { state: string }).state === "offline") keepUp(backoff);
}

// The relay code and its WebAssembly are only in the hosted build (MODE is replaced at build
// time, so the computer's copy drops the import).
const HOSTED = import.meta.env.MODE === "hosted";
const hostP: Promise<WebHost | RelayHost> = HOSTED ? import("./relay").then((m) => new m.RelayHost(emit)) : Promise.resolve(new WebHost(emit));
let host: WebHost | RelayHost;
const saved = () => (HOSTED ? (host as RelayHost).saved() : webSaved());

function machine() {
  const s = saved();
  if (!s) return null;
  const relayed = HOSTED ? (s as RelaySaved) : null;
  return {
    id: s.hostId, name: s.hostName, addrs: relayed ? relayed.addrs : [location.host], os: s.os ?? "", appVersion: s.appVersion ?? "",
    access: s.access ?? "", addedAt: s.addedAt, lastSeen: Date.now(), manualAddrs: [], relay: relayed?.relay ?? null,
  };
}

// The app's Rust side keeps the connection up; here the page does. Phones drop the socket when
// the browser goes to the background, so reconnect when it's back, and retry while offline.
let retry: ReturnType<typeof setTimeout> | null = null;
let backoff = 1000;
function keepUp(delay: number) {
  if (retry) clearTimeout(retry);
  retry = setTimeout(async () => {
    retry = null;
    host ??= await hostP;
    if (document.hidden || !saved() || host.online()) return;
    try { await host.connect(); backoff = 1000; } catch { backoff = Math.min(backoff * 2, 30_000); keepUp(backoff); }
  }, delay);
}
document.addEventListener("visibilitychange", () => {
  if (document.hidden) carryPairing(); // before "Open in Safari" or another app takes the address
  else keepUp(0);
});
window.addEventListener("pageshow", () => keepUp(0));
// Back to a history entry from before pairing: put the pairing back in its address.
window.addEventListener("popstate", () => carryPairing());

export async function invoke<T>(cmd: string, args: Record<string, any> = {}): Promise<T> {
  host ??= await hostP;
  const r = await (async (): Promise<unknown> => {
    switch (cmd) {
      case "m_info": return { deviceId: saved()?.deviceId ?? "", deviceName: deviceName(), platform: platform(), version: "web", online: host.online() && saved() ? [saved()!.hostId] : [] };
      case "m_set_name": setDeviceName(String(args.name ?? "").trim() || deviceName()); return null;
      case "m_machines": { const m = machine(); return m ? [m] : []; }
      case "m_pair": {
        const link = String(args.link);
        // "code:<code>": typed on the page the computer served (the hosted page has no computer to ask)
        if (link.startsWith(CODE_LINK)) {
          if (HOSTED) throw "Pairing by code works on the page served by the computer. Scan the QR code here instead.";
          await (host as WebHost).pairCode(link.slice(CODE_LINK.length));
        } else await host.pair(link);
        return machine();
      }
      case "m_connect": return host.connect();
      case "m_call": return host.call(String(args.cmd), args.args ?? null);
      case "m_disconnect": host.close(); return null;
      case "m_network_changed": host.close(); keepUp(0); return null;
      case "m_forget": host.forget(); return [];
      case "m_set_addrs": { const m = machine(); return m ? [m] : []; }
      default: throw `Not available in the browser: ${cmd}`;
    }
  })();
  return r as T;
}

export type UnlistenFn = () => void;
export async function listen(event: string, h: Handler): Promise<UnlistenFn> {
  const set = handlers.get(event) ?? new Set<Handler>();
  set.add(h);
  handlers.set(event, set);
  return () => { set.delete(h); };
}

// plugin-notification: pages served over http can't show notifications.
export async function isPermissionGranted() { return false; }
export async function requestPermission() { return "denied"; }
export function sendNotification() {}

// plugin-barcode-scanner: not in the browser (the phone's camera app opens the link instead).
export enum Format { QRCode = "QR_CODE" }
export async function checkPermissions() { return "denied"; }
export async function requestPermissions() { return "denied"; }
export async function scan(): Promise<{ content: string }> { throw new Error("Use your phone's camera app to scan the code."); }

// plugin-opener
export async function openUrl(u: string) { window.open(u, "_blank", "noopener"); }

// plugin-deep-link: only the app receives devcat:// links.
export async function getCurrent(): Promise<string[] | null> { return null; }
export async function onOpenUrl(): Promise<() => void> { return () => {}; }
