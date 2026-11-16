// Project focus on the phone, like the desktop's: pick one of the computer's projects and the
// lists show only what belongs to it: its connections, and chats and terminals in its folders.
// A folder is a project's when it's one of its code folders (set on the computer), one the desktop
// used with it (its window's list, shared as `focus_folders`), one used with it on this phone
// (`claim`: a chat or terminal started there under focus), or when it has the project's name (a
// folder under the home folder called "DevCat" for the project "devcat"). Nothing changes on the
// computer.
import { create } from "zustand";
import type { Project } from "./types";
import { host, useApp } from "./store";

interface FocusState {
  projects: Project[];
  /** The focused project's id; null = all projects. */
  focus: string | null;
  /** Folders used on this phone while a project was focused: project id → folders. */
  claimed: Record<string, string[]>;
  /** The desktop's list: folder → project ids. */
  desktop: Record<string, string[]>;
  /** The computer's home folder (names above it don't count). */
  home: string | null;
}

export const useFocus = create<FocusState>(() => ({ projects: [], focus: null, claimed: {}, desktop: {}, home: null }));

const key = (machine: string) => `devcat.mobile.focus.${machine}`;
const claimsKey = (machine: string) => `devcat.mobile.focusFolders.${machine}`;
/** The last list of the computer's projects (and its folders), shown while it isn't connected. */
const cacheKey = (machine: string) => `devcat.mobile.focusCache.${machine}`;

/** What was known about a computer's projects the last time it was connected. */
export function showCached(machine: string) {
  try {
    const c = JSON.parse(localStorage.getItem(cacheKey(machine)) ?? "null") as { projects: Project[]; desktop: Record<string, string[]>; home: string | null } | null;
    if (!c) return;
    const saved = localStorage.getItem(key(machine));
    useFocus.setState({ ...c, focus: c.projects.some((p) => p.id === saved) ? saved : null, claimed: loadClaims(machine) });
  } catch { /* nothing cached */ }
}

function loadClaims(machine: string): Record<string, string[]> {
  try { return JSON.parse(localStorage.getItem(claimsKey(machine)) ?? "{}") as Record<string, string[]>; } catch { return {}; }
}

/** The computer's projects, and the focus saved for it. */
export async function loadFocus() {
  const machine = useApp.getState().current;
  if (!machine) return;
  try {
    const [list, desktop, home] = await Promise.all([
      host<Project[]>("list_projects"),
      // an older computer doesn't share its list
      host<Record<string, string[]>>("focus_folders").catch(() => ({})),
      host<string | null>("home_dir").catch(() => null),
    ]);
    const projects = [...list].sort((a, b) => a.order - b.order);
    if (useApp.getState().current !== machine) return;
    const saved = localStorage.getItem(key(machine));
    useFocus.setState({ projects, focus: projects.some((p) => p.id === saved) ? saved : null, claimed: loadClaims(machine), desktop, home });
    try { localStorage.setItem(cacheKey(machine), JSON.stringify({ projects, desktop, home })); } catch { /* quota */ }
  } catch { /* offline or an older computer: no focus */ }
}

export function setFocus(id: string | null) {
  const machine = useApp.getState().current;
  if (machine) {
    if (id) localStorage.setItem(key(machine), id);
    else localStorage.removeItem(key(machine));
  }
  useFocus.setState({ focus: id });
}

export function focusedProject(s: FocusState = useFocus.getState()): Project | null {
  return s.projects.find((p) => p.id === s.focus) ?? null;
}

const trim = (p: string) => p.replace(/[\\/]+$/, "");

/** The focused project's folders: its code folders, then the ones used with it here (none
 *  without focus). */
export function focusFolders(s: FocusState = useFocus.getState()): string[] {
  const p = focusedProject(s);
  if (!p) return [];
  const desktop = Object.entries(s.desktop).filter(([, ids]) => ids.includes(p.id)).map(([f]) => f);
  return [...new Set([...(p.folders ?? []), ...desktop, ...(s.claimed[p.id] ?? [])].map(trim).filter(Boolean))];
}

const norm = (name: string) => name.toLowerCase().replace(/[^a-z0-9]/g, "");

/** A folder has the project's name: it, or a folder it's in, under the home folder. */
function namedLike(cwd: string, project: Project, home: string | null): boolean {
  const want = norm(project.name);
  if (!want) return false;
  const c = trim(cwd).replace(/\\/g, "/");
  const h = home ? trim(home).replace(/\\/g, "/") : null;
  // without the home folder: skip the first two parts (/Users/<name>, /home/<name>)
  const rest = h && (c === h || c.startsWith(`${h}/`)) ? c.slice(h.length) : h ? "" : c.split("/").slice(3).join("/");
  return rest.split("/").some((part) => part && norm(part) === want);
}

/** A chat or terminal starts in `cwd` while a project is focused: the folder is that project's
 *  from now on (on this phone), as on the desktop. The home folder stays nobody's. */
export function claim(cwd: string, home?: string | null) {
  const s = useFocus.getState();
  const machine = useApp.getState().current;
  const c = trim(cwd);
  if (!s.focus || !machine || !c || (home && trim(home) === c) || inFocus(c, s)) return;
  const claimed = { ...loadClaims(machine) };
  claimed[s.focus] = [...(claimed[s.focus] ?? []), c];
  localStorage.setItem(claimsKey(machine), JSON.stringify(claimed));
  useFocus.setState({ claimed });
}

/** Whether a folder is shown under the focus: inside one of the project's folders (or no focus). */
export function inFocus(cwd: string | null | undefined, s: FocusState = useFocus.getState()): boolean {
  if (!s.focus) return true;
  const c = trim(cwd ?? "");
  if (!c) return false;
  const p = focusedProject(s);
  return focusFolders(s).some((f) => c === f || c.startsWith(`${f}/`) || c.startsWith(`${f}\\`)) || (!!p && namedLike(c, p, s.home));
}

/** `inFocus` that re-renders when the focus changes. */
export function useInFocus(): (cwd: string | null | undefined) => boolean {
  const s = useFocus();
  return (cwd) => inFocus(cwd, s);
}
