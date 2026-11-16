// Project focus on the phone, like the desktop's: pick one of the computer's projects and the
// lists show only what belongs to it (chats and terminals in its code folders, its connections).
// Kept on the phone per computer; nothing changes on the computer.
import { create } from "zustand";
import type { Project } from "./types";
import { host, useApp } from "./store";

interface FocusState {
  projects: Project[];
  /** The focused project's id; null = all projects. */
  focus: string | null;
}

export const useFocus = create<FocusState>(() => ({ projects: [], focus: null }));

const key = (machine: string) => `devcat.mobile.focus.${machine}`;

/** The computer's projects, and the focus saved for it. */
export async function loadFocus() {
  const machine = useApp.getState().current;
  if (!machine) return;
  try {
    const projects = [...(await host<Project[]>("list_projects"))].sort((a, b) => a.order - b.order);
    if (useApp.getState().current !== machine) return;
    const saved = localStorage.getItem(key(machine));
    useFocus.setState({ projects, focus: projects.some((p) => p.id === saved) ? saved : null });
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

/** The focused project's code folders (none without focus). */
export function focusFolders(s: FocusState = useFocus.getState()): string[] {
  return (focusedProject(s)?.folders ?? []).map(trim).filter(Boolean);
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
