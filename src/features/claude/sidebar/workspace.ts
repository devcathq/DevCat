// Claude sidebar state: current workspace folder + recent folders (persisted in localStorage).
import { open } from "@tauri-apps/plugin-dialog";
import { create } from "zustand";
import { localStart } from "../../../lib/remotePath";

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
    const recent = cwd ? [cwd, ...get().recent.filter((r) => r !== cwd)].slice(0, MAX_RECENT) : get().recent;
    const project = get().project;
    if (project) save(KEY_CWD_BY_PROJECT, { ...load<Record<string, string>>(KEY_CWD_BY_PROJECT, {}), [project]: cwd ?? undefined });
    else save(KEY_CWD, cwd);
    save(KEY_RECENT, recent);
    set({ cwd, recent });
  },
  setProject: (project) => {
    if (project === get().project) return;
    const cwd = project ? load<Record<string, string>>(KEY_CWD_BY_PROJECT, {})[project] ?? null : load<string | null>(KEY_CWD, null);
    set({ project, cwd });
  },
  forgetRecent: (cwd) => {
    const recent = get().recent.filter((r) => r !== cwd);
    save(KEY_RECENT, recent);
    set({ recent });
  },
  setShowIgnored: (showIgnored) => { save(KEY_IGNORED, showIgnored); set({ showIgnored }); },
  setSessionsOpen: (sessionsOpen) => { save(KEY_SPLIT, sessionsOpen); set({ sessionsOpen }); },
}));

/** Ask for a folder with the native picker; sets it as the workspace. */
export async function pickFolder(): Promise<string | null> {
  const dir = await open({ directory: true, multiple: false, defaultPath: localStart(useWorkspace.getState().cwd) });
  if (typeof dir !== "string") return null;
  useWorkspace.getState().setCwd(dir);
  return dir;
}
