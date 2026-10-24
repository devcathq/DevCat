// Focus mode for folders: which projects a folder belongs to. A folder used while a project is
// focused (its AI Workspace folder, or the folder of a chat, terminal, file or Git tab opened then)
// is that project's, and in focus the tabs of other folders are hidden, like other projects'
// connections. Kept in localStorage, so every window knows.
import { create } from "zustand";
import { tabConnId, tabFolder } from "../tabs/types";
import { useConnections } from "./connections";
import { useTabs } from "./tabs";

const KEY = "devcat.focus.folders";

/** A path to compare: forward slashes, no trailing slash, Windows paths in lower case. */
export function folderKey(path: string): string {
  const p = path.replace(/\\/g, "/").replace(/\/+$/, "");
  return /^[a-z]:\//i.test(p) || p.startsWith("//") ? p.toLowerCase() : p;
}

function load(): Record<string, string[]> {
  try {
    return JSON.parse(localStorage.getItem(KEY) ?? "{}") as Record<string, string[]>;
  } catch {
    return {};
  }
}

interface FolderProjectsState {
  /** Folder (see `folderKey`) → the projects it belongs to. */
  folders: Record<string, string[]>;
  add: (folder: string, projectId: string) => void;
}

export const useFolderProjects = create<FolderProjectsState>((set, get) => ({
  folders: load(),
  add: (folder, projectId) => {
    const key = folderKey(folder);
    const now = get().folders[key] ?? [];
    if (!key || now.includes(projectId)) return;
    const folders = { ...load(), ...get().folders, [key]: [...now, projectId] };
    try { localStorage.setItem(KEY, JSON.stringify(folders)); } catch { /* quota / private mode */ }
    set({ folders });
  },
}));

// another window added one
window.addEventListener("storage", (e) => {
  if (e.key === KEY) useFolderProjects.setState({ folders: load() });
});

/** Whether `path` (a folder, or a file in one) is in one of `projectId`'s folders. */
export function inProjectFolder(path: string, projectId: string, folders = useFolderProjects.getState().folders): boolean {
  const key = folderKey(path);
  return Object.entries(folders).some(([f, ids]) => ids.includes(projectId) && (key === f || key.startsWith(`${f}/`)));
}

/** Whether a tab in `path` shows under the window's focus (always without one, or without a path). */
export function folderInFocus(path: string | null | undefined): boolean {
  const focus = useConnections.getState().focusProjectId;
  return !focus || !path || inProjectFolder(path, focus);
}

// A tab opened while a project is focused makes its folder that project's (tabs aren't restored at
// start, so every new one was opened just now). Tabs of a connection go by its project instead.
useTabs.subscribe((s, prev) => {
  const focus = useConnections.getState().focusProjectId;
  if (!focus || s.tabs === prev.tabs) return;
  const before = new Set(prev.tabs.map((t) => t.id));
  for (const t of s.tabs) {
    const folder = tabFolder(t);
    if (!before.has(t.id) && folder && !tabConnId(t)) useFolderProjects.getState().add(folder, focus);
  }
});
