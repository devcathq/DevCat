// A terminal tab: a login shell in a folder (local, or on an SSH host / saved SSH session), on its
// own, apart from any agent chat. Closing the tab ends the shell.
import { useRef, useState } from "react";
import { useTabs } from "../../store/tabs";
import type { Tab } from "../../tabs/types";
import { TerminalPane } from "../claude/TerminalPane";
import { QuickBar } from "./QuickBar";

type TerminalTabT = Extract<Tab, { type: "terminal" }>;

export default function TerminalTab({ tab }: { tab: TerminalTabT }) {
  const active = useTabs((s) => s.activeId === tab.id);
  const [pty, setPty] = useState<string | null>(null);
  // The tab shows the title the program sets (a shell's user@host:dir, Claude Code's task), like
  // a terminal app's tab; back to the session's name when there's none. Windows' console starts a
  // program's title as its own path (an SSH session's ssh.exe, a Docker shell's docker.exe), and
  // keeps it while nothing sets one (tmux doesn't, by default): that's no name either.
  const name = useRef(tab.title).current;
  const onTitle = (t: string) => {
    const own = !!tab.program && t.trim().toLowerCase().startsWith(tab.program.toLowerCase());
    useTabs.getState().update(tab.id, { title: (!own && !isProgramPath(t.trim()) && t.trim()) || name });
  };
  return (
    <div className="flex h-full flex-col bg-app">
      <div className="min-h-0 flex-1">
        <TerminalPane cwd={tab.cwd} program={tab.program ?? null} getArgs={tab.args ? async () => tab.args! : undefined} name={tab.program ? "docker" : tab.connId ? "ssh" : "shell"} visible={active} onPty={setPty} onTitle={onTitle} />
      </div>
      <QuickBar ptyId={pty} cwd={tab.cwd} />
    </div>
  );
}

/** A Windows console's default title: the program's path (`C:\Windows\System32\OpenSSH\ssh.EXE`),
 *  maybe after "Administrator: ". */
function isProgramPath(title: string): boolean {
  return /^(?:[^:\\]+: )?[A-Za-z]:\\[^:]*\.(?:exe|com|bat|cmd)$/i.test(title);
}
