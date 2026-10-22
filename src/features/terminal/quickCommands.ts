// Quick commands (Xshell's quick command bar): buttons under terminals and agent chats that type
// saved text, optionally followed by Enter. Per computer (localStorage), shared by all windows.
import { create } from "zustand";

export interface QuickCommand {
  id: string;
  label: string;
  /** Typed as is; line breaks become Enter. */
  text: string;
  /** Press Enter after the text. */
  enter: boolean;
  /** null = every terminal; a connection id = only that SSH session's terminals. */
  scope: string | null;
}

const KEY = "devcat.quickCommands";

interface Saved { commands: QuickCommand[]; visible: boolean }

function load(): Saved {
  try {
    const raw = JSON.parse(localStorage.getItem(KEY) ?? "{}") as Partial<Saved>;
    const commands = Array.isArray(raw.commands)
      ? raw.commands.filter((c): c is QuickCommand => !!c && typeof c.id === "string" && typeof c.label === "string" && typeof c.text === "string")
      : [];
    return { commands, visible: raw.visible !== false };
  } catch {
    return { commands: [], visible: true };
  }
}

interface QuickState extends Saved {
  save: (c: QuickCommand) => void;
  remove: (id: string) => void;
  /** Move one place left (-1) or right (+1) among all commands. */
  move: (id: string, by: -1 | 1) => void;
  setVisible: (v: boolean) => void;
}

function persist(s: Saved) {
  try { localStorage.setItem(KEY, JSON.stringify({ commands: s.commands, visible: s.visible })); } catch { /* private mode */ }
}

export const useQuickCommands = create<QuickState>((set, get) => {
  const update = (patch: Partial<Saved>) => {
    set(patch);
    persist(get());
  };
  return {
    ...load(),
    save: (c) => {
      const list = get().commands;
      update({ commands: list.some((x) => x.id === c.id) ? list.map((x) => (x.id === c.id ? c : x)) : [...list, c] });
    },
    remove: (id) => update({ commands: get().commands.filter((c) => c.id !== id) }),
    move: (id, by) => {
      const list = [...get().commands];
      const i = list.findIndex((c) => c.id === id);
      const j = i + by;
      if (i < 0 || j < 0 || j >= list.length) return;
      [list[i], list[j]] = [list[j], list[i]];
      update({ commands: list });
    },
    setVisible: (visible) => update({ visible }),
  };
});

// Other windows' edits.
window.addEventListener("storage", (e) => { if (e.key === KEY) useQuickCommands.setState(load()); });

/** What a command types into the terminal. */
export const quickInput = (c: QuickCommand) => c.text.replace(/\r?\n/g, "\r") + (c.enter ? "\r" : "");

export const newQuickCommand = (scope: string | null = null): QuickCommand =>
  ({ id: crypto.randomUUID(), label: "", text: "", enter: true, scope });
