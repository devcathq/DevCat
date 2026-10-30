// Multiple app windows. Each window is its own JS context (own tabs, own project focus);
// the backend and localStorage are shared. Secondary windows are opened with
// `?focus=<projectId>&tab=<tab spec json>`, read once here at startup.
import { emit, listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { toast } from "sonner";
import type { TabSpec } from "../tabs/types";
import { api, errMsg } from "./api";

function currentLabel(): string {
  try {
    return getCurrentWindow().label;
  } catch {
    return "main"; // not running inside Tauri
  }
}

export const WINDOW_LABEL = currentLabel();
/** The window from tauri.conf.json; it owns the persisted (localStorage) per-window state. */
export const IS_MAIN_WINDOW = WINDOW_LABEL === "main";
/** Per-window storage: localStorage in the main window (survives restarts), sessionStorage elsewhere. */
export const windowStorage: Storage = IS_MAIN_WINDOW ? localStorage : sessionStorage;

export const IS_MAC = navigator.platform.toLowerCase().includes("mac");
export const NEW_WINDOW_SHORTCUT = IS_MAC ? "⇧⌘N" : "Ctrl+Shift+N";

// Launch parameters, consumed once: drop them from the URL so a reload doesn't repeat them.
const params = new URLSearchParams(location.search);
export const launchFocus = params.get("focus");
let launchTabRaw = params.get("tab");
if (location.search) history.replaceState(null, "", location.pathname + location.hash);

/** A tab to move / copy into another window. */
export type TabTransfer = TabSpec & { title: string };

/** The tab this window was opened with (once), if any. */
export function takeLaunchTab(): TabTransfer | null {
  const raw = launchTabRaw;
  launchTabRaw = null;
  if (!raw) return null;
  try {
    const t = JSON.parse(raw) as TabTransfer;
    return t && typeof t === "object" && typeof t.type === "string" ? t : null;
  } catch {
    return null;
  }
}

/** Whether this window was opened for a ghost chat (it then shows only that chat). */
export function isGhostWindow(): boolean {
  if (!launchTabRaw) return false;
  try {
    const t = JSON.parse(launchTabRaw) as { type?: string; ghost?: boolean };
    return t.type === "claude" && t.ghost === true;
  } catch {
    return false;
  }
}

/** Open another app window, optionally focused on a project and/or with a tab open. */
export async function openNewWindow(projectId?: string | null, tab?: TabTransfer | null): Promise<boolean> {
  try {
    await api.openWindow(projectId, tab as Record<string, unknown> | null);
    return true;
  } catch (e) {
    toast.error(`Could not open a window: ${errMsg(e)}`);
    return false;
  }
}

// ---- cross-window notifications (other windows reload shared data)

const SYNC_EVENT = "devcat://window-sync";

export type SyncMessage =
  | { kind: "connections" }
  | { kind: "notes" }
  | { kind: "bookmarks" }
  | { kind: "conn-open"; id: string; open: boolean };

/** Tell the other windows that shared data changed. */
export function broadcast(msg: SyncMessage) {
  emit(SYNC_EVENT, { ...msg, from: WINDOW_LABEL }).catch(() => {});
}

/** Receive messages from other windows (not this one's own). */
export function onBroadcast(handler: (msg: SyncMessage) => void): () => void {
  let unlisten: (() => void) | null = null;
  let disposed = false;
  listen<SyncMessage & { from: string }>(SYNC_EVENT, (e) => {
    if (e.payload.from !== WINDOW_LABEL) handler(e.payload);
  })
    .then((u) => (disposed ? u() : (unlisten = u)))
    .catch(() => {});
  return () => {
    disposed = true;
    unlisten?.();
  };
}
