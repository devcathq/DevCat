import { openTab } from "../../store/tabs";
import { basename } from "../files/format";
import { remoteHost, isRemote, sessionIdOf } from "../../lib/remotePath";

/** Open a new terminal tab in `cwd` (each call opens another one). */
export function openTerminal(cwd: string) {
  const where = isRemote(cwd) ? `${remoteHost(cwd)}: ${basename(cwd)}` : basename(cwd);
  openTab({ type: "terminal", cwd, connId: sessionIdOf(cwd) ?? undefined }, where ? `Terminal · ${where}` : "Terminal");
}
