// Dev-only browser shims for @tauri-apps/* (vite --mode bridge): commands go to the Rust
// devbridge (`cargo run --example devbridge`); everything else is a harmless stub.
// `vite --mode demo` answers database / VPN / cluster / terminal commands with demo data first.
import { demoInvoke } from "./demo";

const URL_ = "http://127.0.0.1:1430/invoke";
const DEMO = import.meta.env.MODE === "demo";

function fallback(cmd: string): unknown {
  return /list|ids|sessions|projects|contexts|logs|traffic|history|queries/.test(cmd) ? [] : null;
}

export async function invoke<T>(cmd: string, args: Record<string, unknown> = {}): Promise<T> {
  if (DEMO) {
    const r = demoInvoke(cmd, args, emitLocal);
    if (r !== undefined) return r as T;
  }
  const res = await fetch(URL_, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ cmd, args }) });
  const body = await res.json();
  if ("ok" in body) return body.ok as T;
  if (typeof body.err === "string" && body.err.startsWith("devbridge: unsupported")) return fallback(cmd) as T;
  throw body.err;
}
export const convertFileSrc = (p: string) => p;
export class Channel<T> { onmessage: (m: T) => void = () => {}; }

// event: an in-page bus (the demo data emits pty / k8s events through it)
export type UnlistenFn = () => void;
type Handler = (e: { event: string; payload: unknown }) => void;
const handlers = new Map<string, Set<Handler>>();
function emitLocal(event: string, payload: unknown) {
  handlers.get(event)?.forEach((h) => h({ event, payload }));
}
// Backend events (terminal output, SFTP progress) queued by the devbridge, fetched ~10×/s.
let polling = false;
function pollEvents() {
  if (polling || DEMO) return;
  polling = true;
  const tick = async () => {
    try {
      const evs = await invoke<{ event: string; payload: unknown }[]>("__events");
      (evs ?? []).forEach((e) => emitLocal(e.event, e.payload));
    } catch { /* bridge not running */ }
    setTimeout(tick, 100);
  };
  void tick();
}

export async function listen(event: string, h: Handler): Promise<UnlistenFn> {
  if (/^(pty|sftp)-/.test(event)) pollEvents();
  const set = handlers.get(event) ?? new Set<Handler>();
  set.add(h);
  handlers.set(event, set);
  return () => { set.delete(h); };
}
export async function emit(event: string, payload?: unknown): Promise<void> { emitLocal(event, payload); }

// window
export enum UserAttentionType { Critical = 1, Informational = 2 }
export enum Effect { Sidebar = "sidebar", Acrylic = "acrylic" }
export enum EffectState { Active = "active" }
export function getCurrentWindow() {
  return { setTitle: async (t: string) => { document.title = t; }, setBadgeCount: async (n?: number) => { console.log("[bridge] badge", n ?? 0); }, requestUserAttention: async (t: number) => { console.log("[bridge] attention", t); }, label: "main", onFocusChanged: async () => () => {}, listen: async () => () => {}, setTheme: async () => {}, setEffects: async () => {}, clearEffects: async () => {}, setZoom: async (z: number) => { document.documentElement.style.zoom = String(z); } };
}
export const getCurrentWebviewWindow = getCurrentWindow;

// app
export async function getVersion() { return DEMO ? "0.3.6" : "0.0.0-bridge"; }

// path
export async function homeDir() { return "/Users"; }
export async function join(...p: string[]) { return p.join("/").replace(/\/+/g, "/"); }

// dialog: folder pick answers come from localStorage "devbridge.pick"
export async function open() { return localStorage.getItem("devbridge.pick"); }
export async function save() { return null; }
export async function ask() { return false; }
export async function confirm() { return false; }
export async function message() {}

// fs
export async function readTextFile() { return ""; }
export async function writeFile() {}
export async function writeTextFile() {}

// opener
export async function openUrl(u: string) { console.log("[bridge] openUrl", u); }
export async function openPath(p: string) { console.log("[bridge] openPath", p); }
export async function revealItemInDir(p: string) { console.log("[bridge] reveal", p); }
