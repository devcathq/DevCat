// Claude sidebar state: current workspace folder + recent folders (persisted in localStorage).
// Recent folders: the workspace folders picked here, and the folders of the chat, terminal and
// SFTP tabs the user switches to (local and ssh://), newest first. localStorage is shared by all
// windows, so each change starts from the stored list and the others follow its `storage` event.
import { open } from "@tauri-apps/plugin-dialog";
import { create } from "zustand";
import { localStart } from "../../../lib/remotePath";
import { useTabs } from "../../../store/tabs";
import type { Tab } from "../../../tabs/types";

const KEY_CWD = "devcat.claude.cwd";
/** Folder per focused project: { [projectId]: cwd }. */
const KEY_CWD_BY_PROJECT = "devcat.claude.cwdByProject";
const KEY_RECENT = "devcat.claude.recent";
const KEY_IGNORED = "devcat.claude.showIgnored";
const KEY_SPLIT = "devcat.claude.sessionsOpen";
const MAX_RECENT = 10;

function load<T>(key: string, fallback: T): T {
  try {
    const raw = localStorage.getItem(key);
    return raw == null ? fallback : (JSON.parse(raw) as T);
  } catch {
    return fallback;
  }
}
function save(key: string, v: unknown) {
  try { localStorage.setItem(key, JSON.stringify(v)); } catch { /* quota / private mode */ }
}

interface WorkspaceState {
  cwd: string | null;
  /** Focused project the folder belongs to (null = no focus: the global folder). */
  project: string | null;
  recent: string[];
  showIgnored: boolean;
  sessionsOpen: boolean;
  setCwd: (cwd: string | null) => void;
  /** Switch to the folder remembered for a focused project (null = the global one). */
  setProject: (projectId: string | null) => void;
  /** Put a folder first in the recent list (without opening it). */
  noteRecent: (cwd: string) => void;
  forgetRecent: (cwd: string) => void;
  setShowIgnored: (v: boolean) => void;
  setSessionsOpen: (v: boolean) => void;
}

export const useWorkspace = create<WorkspaceState>((set, get) => ({
  cwd: load<string | null>(KEY_CWD, null),
  project: null,
  recent: load<string[]>(KEY_RECENT, []),
  showIgnored: load(KEY_IGNORED, false),
  sessionsOpen: load(KEY_SPLIT, true),
  setCwd: (cwd) => {
    const project = get().project;
    if (project) save(KEY_CWD_BY_PROJECT, { ...load<Record<string, string>>(KEY_CWD_BY_PROJECT, {}), [project]: cwd ?? undefined });
    else save(KEY_CWD, cwd);
    set({ cwd });
    if (cwd) get().noteRecent(cwd);
  },
  noteRecent: (cwd) => {
    const stored = load<string[]>(KEY_RECENT, get().recent);
    if (stored[0] === cwd && get().recent[0] === cwd) return;
    const recent = [cwd, ...stored.filter((r) => r !== cwd)].slice(0, MAX_RECENT);
    save(KEY_RECENT, recent);
    set({ recent });
  },
  setProject: (project) => {
    if (project === get().project) return;
    const cwd = project ? load<Record<string, string>>(KEY_CWD_BY_PROJECT, {})[project] ?? null : load<string | null>(KEY_CWD, null);
    set({ project, cwd });
  },
  forgetRecent: (cwd) => {
    const recent = load<string[]>(KEY_RECENT, get().recent).filter((r) => r !== cwd);
    save(KEY_RECENT, recent);
    set({ recent });
  },
  setShowIgnored: (showIgnored) => { save(KEY_IGNORED, showIgnored); set({ showIgnored }); },
  setSessionsOpen: (sessionsOpen) => { save(KEY_SPLIT, sessionsOpen); set({ sessionsOpen }); },
}));

/** The folder a tab is "in" for the recent list: chats (not ghost ones), shells and SFTP. */
function recentFolder(t: Tab | undefined): string | null {
  if (!t) return null;
  if (t.type === "claude") return t.ghost ? null : t.cwd || null;
  if (t.type === "terminal") return t.program ? null : t.cwd || null; // not a container's shell
  if (t.type === "sftp") return t.cwd || null;
  return null;
}
useTabs.subscribe((s, prev) => {
  if (s.activeId === prev.activeId) return;
  const dir = recentFolder(s.tabs.find((t) => t.id === s.activeId));
  if (dir) useWorkspace.getState().noteRecent(dir);
});
// another window changed the list
window.addEventListener("storage", (e) => {
  if (e.key === KEY_RECENT) useWorkspace.setState({ recent: load<string[]>(KEY_RECENT, []) });
});

/** Ask for a folder with the native picker; sets it as the workspace. */
export async function pickFolder(): Promise<string | null> {
  const dir = await open({ directory: true, multiple: false, defaultPath: localStart(useWorkspace.getState().cwd) });
  if (typeof dir !== "string") return null;
  useWorkspace.getState().setCwd(dir);
  return dir;
}
