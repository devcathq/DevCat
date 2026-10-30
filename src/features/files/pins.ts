// Pinned files: kept at the top of the AI workspace's Files section to open again in one click.
// Pinned from the file list or a file's tab; saved on this computer (localStorage), shared by its
// windows. A workspace shows the pins inside its folder.
import { create } from "zustand";

export interface Pin {
  path: string;
  /** The folder the file was opened from (its tab's `root`), for opening it again the same way. */
  root: string | null;
}

const KEY = "devcat.files.pinned";

function load(): Pin[] {
  try {
    const v = JSON.parse(localStorage.getItem(KEY) ?? "[]");
    return Array.isArray(v) ? v.filter((p) => typeof p?.path === "string") : [];
  } catch {
    return [];
  }
}

export const usePins = create<{ pins: Pin[] }>(() => ({ pins: load() }));

function save(pins: Pin[]) {
  usePins.setState({ pins });
  try { localStorage.setItem(KEY, JSON.stringify(pins)); } catch { /* quota / private mode */ }
}

// another window pinned or unpinned something
if (typeof window !== "undefined") {
  window.addEventListener("storage", (e) => { if (e.key === KEY) usePins.setState({ pins: load() }); });
}

export const isPinned = (pins: Pin[], path: string) => pins.some((p) => p.path === path);

export function pin(path: string, root: string | null = null) {
  const pins = usePins.getState().pins;
  if (!isPinned(pins, path)) save([...pins, { path, root }]);
}

export function unpin(path: string) {
  save(usePins.getState().pins.filter((p) => p.path !== path));
}

export function togglePin(path: string, root: string | null = null) {
  if (isPinned(usePins.getState().pins, path)) unpin(path);
  else pin(path, root);
}

/** Forget pins of a deleted file or folder (and everything in it). */
export function unpinUnder(path: string) {
  const under = (p: string) => p === path || p.startsWith(`${path}/`) || p.startsWith(`${path}\\`);
  const pins = usePins.getState().pins;
  if (pins.some((p) => under(p.path))) save(pins.filter((p) => !under(p.path)));
}

/** Whether `path` is inside the folder `dir`. */
export function inFolder(path: string, dir: string) {
  const d = dir.replace(/[\\/]+$/, "");
  return path.startsWith(`${d}/`) || path.startsWith(`${d}\\`);
}
