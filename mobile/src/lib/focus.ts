// Project focus on the phone, like the desktop's: pick one of the computer's projects and the
// lists show only what belongs to it: its connections, and chats and terminals in its folders.
// A project's folders are its code folders (set on the computer) and, as on the desktop, the
// folders used while it was focused (`claim`: starting a chat or terminal there). The desktop
// keeps its own list in its window, so the phone keeps one too. Nothing changes on the computer.
import { create } from "zustand";
import type { Project } from "./types";
import { host, useApp } from "./store";

interface FocusState {
  projects: Project[];
  /** The focused project's id; null = all projects. */
  focus: string | null;
  /** Folders used on this phone while a project was focused: project id → folders. */
  claimed: Record<string, string[]>;
}

export const useFocus = create<FocusState>(() => ({ projects: [], focus: null, claimed: {} }));

const key = (machine: string) => `devcat.mobile.focus.${machine}`;
const claimsKey = (machine: string) => `devcat.mobile.focusFolders.${machine}`;

function loadClaims(machine: string): Record<string, string[]> {
  try { return JSON.parse(localStorage.getItem(claimsKey(machine)) ?? "{}") as Record<string, string[]>; } catch { return {}; }
}

/** The computer's projects, and the focus saved for it. */
export async function loadFocus() {
  const machine = useApp.getState().current;
  if (!machine) return;
  try {
    const projects = [...(await host<Project[]>("list_projects"))].sort((a, b) => a.order - b.order);
    if (useApp.getState().current !== machine) return;
    const saved = localStorage.getItem(key(machine));
    useFocus.setState({ projects, focus: projects.some((p) => p.id === saved) ? saved : null, claimed: loadClaims(machine) });
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
  return [...new Set([...(p.folders ?? []), ...(s.claimed[p.id] ?? [])].map(trim).filter(Boolean))];
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
  return focusFolders(s).some((f) => c === f || c.startsWith(`${f}/`) || c.startsWith(`${f}\\`));
}

/** `inFocus` that re-renders when the focus changes. */
export function useInFocus(): (cwd: string | null | undefined) => boolean {
  const s = useFocus();
  return (cwd) => inFocus(cwd, s);
}
