// Dev only (`pnpm mobile:bridge`): browser shims for @tauri-apps/* so the mobile UI runs in a
// normal browser against the phone-side bridge (mobile/src-tauri/examples/bridge.rs). Commands
// and events are the real ones; camera and notifications are stubs.
const BASE = (import.meta.env.VITE_MOBILE_BRIDGE as string | undefined) ?? "http://127.0.0.1:1443";

export async function invoke<T>(cmd: string, args: Record<string, unknown> = {}): Promise<T> {
  const res = await fetch(`${BASE}/invoke`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ cmd, args }) });
  const body = await res.json();
  if ("ok" in body) return body.ok as T;
  throw body.err;
}

type Handler = (e: { event: string; payload: unknown }) => void;
const handlers = new Map<string, Set<Handler>>();
let source: EventSource | null = null;
function connect() {
  if (source) return;
  source = new EventSource(`${BASE}/events`);
  source.onmessage = (m) => {
    const { event, payload } = JSON.parse(m.data);
    handlers.get(event)?.forEach((h) => h({ event, payload }));
  };
}
export type UnlistenFn = () => void;
export async function listen(event: string, h: Handler): Promise<UnlistenFn> {
  connect();
  const set = handlers.get(event) ?? new Set<Handler>();
  set.add(h);
  handlers.set(event, set);
  return () => { set.delete(h); };
}

// plugin-notification
export async function isPermissionGranted() { return false; }
export async function requestPermission() { return "denied"; }
export async function onAction(_cb: unknown) { return { unregister: async () => {} }; }
export function sendNotification(n: unknown) { console.log("[bridge] notification", n); }

// plugin-barcode-scanner
export enum Format { QRCode = "QR_CODE" }
export async function checkPermissions() { return "granted"; }
export async function requestPermissions() { return "granted"; }
export async function scan(): Promise<{ content: string }> {
  const link = localStorage.getItem("bridge.scan");
  if (link) return { content: link };
  throw new Error("No camera in the browser bridge");
}

// plugin-opener
export async function openUrl(u: string) { console.log("[bridge] openUrl", u); }

// plugin-deep-link: only the app receives devcat:// links.
export async function getCurrent(): Promise<string[] | null> { return null; }
export async function onOpenUrl(): Promise<() => void> { return () => {}; }
