// Focus mode for folders: which projects a folder belongs to. A folder used while a project is
// focused (its AI Workspace folder, or the folder of a chat, terminal, file or Git tab opened then)
// is that project's, and in focus the tabs of other folders are hidden, like other projects'
// connections. Kept in localStorage, so every window knows.
import { homeDir } from "@tauri-apps/api/path";
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

/** Write `folders` and show them. The file is read first by callers: another window may have
 *  changed it since this one last did. */
function save(folders: Record<string, string[]>) {
  try { localStorage.setItem(KEY, JSON.stringify(folders)); } catch { /* quota / private mode */ }
  useFolderProjects.setState({ folders });
}

/** The home folder (and the folders above it) never become a project's: a terminal opened there
 *  would claim everything under it. */
let home: string | null = null;
void homeDir().then((h) => { home = folderKey(h); }, () => {});
const tooWide = (key: string) => !key || (home !== null && (key === home || home.startsWith(`${key}/`)));

interface FolderProjectsState {
  /** Folder (see `folderKey`) → the projects it belongs to. */
  folders: Record<string, string[]>;
  add: (folder: string, projectId: string) => void;
  remove: (folder: string, projectId: string) => void;
}

export const useFolderProjects = create<FolderProjectsState>(() => ({
  folders: load(),
  add: (folder, projectId) => {
    const key = folderKey(folder);
    const folders = load();
    if (tooWide(key) || folders[key]?.includes(projectId)) return;
    save({ ...folders, [key]: [...(folders[key] ?? []), projectId] });
  },
  remove: (folder, projectId) => {
    const key = folderKey(folder);
    const folders = load();
    const ids = (folders[key] ?? []).filter((id) => id !== projectId);
    if (ids.length) folders[key] = ids;
    else delete folders[key];
    save(folders);
  },
}));

// another window changed them
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

// A tab opened while a project is focused makes its folder that project's: a new tab, or one
// already open (hidden) that is opened again, from the Git sidebar or a chat's session. Tabs of a
// connection go by its project instead.
useTabs.subscribe((s, prev) => {
  const focus = useConnections.getState().focusProjectId;
  if (!focus || !s.opened || s.opened === prev.opened) return;
  const t = s.tabs.find((x) => x.id === s.opened!.id);
  const folder = t && tabFolder(t);
  if (folder && !tabConnId(t)) useFolderProjects.getState().add(folder, focus);
});

// A deleted project's folders are forgotten.
useConnections.subscribe((s, prev) => {
  if (!prev.projects.length || s.projects === prev.projects) return;
  const gone = new Set(prev.projects.filter((p) => !s.projects.some((q) => q.id === p.id)).map((p) => p.id));
  if (!gone.size) return;
  const folders = load();
  for (const [f, ids] of Object.entries(folders)) {
    const kept = ids.filter((id) => !gone.has(id));
    if (kept.length) folders[f] = kept;
    else delete folders[f];
  }
  save(folders);
});
