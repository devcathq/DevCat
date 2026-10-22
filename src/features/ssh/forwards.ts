// Port forwards of saved SSH sessions: saved in the session (options.forwards), started and stopped
// from the sidebar. The backend runs them (src-tauri/src/claude/ssh_forward.rs) and reports each
// session's state with the `ssh-forward` event.
import { listen } from "@tauri-apps/api/event";
import { toast } from "sonner";
import { create } from "zustand";
import { api, errMsg } from "../../lib/api";
import type { ConnectionConfig, ForwardStatus, SessionForward } from "../../lib/types";

/** The session's saved forwards (bad JSON = none). */
export function parseForwards(json: string | null | undefined): SessionForward[] {
  if (!json) return [];
  try {
    const list = JSON.parse(json) as unknown;
    return Array.isArray(list) ? (list as SessionForward[]) : [];
  } catch {
    return [];
  }
}

export const useForwards = create<{ byConn: Record<string, ForwardStatus> }>(() => ({ byConn: {} }));

const put = (s: ForwardStatus) => useForwards.setState((cur) => ({ byConn: { ...cur.byConn, [s.connId]: s } }));

let started = false;
/** Follow the backend's forward states (once per window). */
export function initForwards() {
  if (started) return;
  started = true;
  listen<ForwardStatus>("ssh-forward", (e) => {
    const s = e.payload;
    put(s);
    if (s.state === "error" && s.error) toast.error(s.error);
  }).catch(() => {});
  api.sshForwardList().then((list) => list.forEach(put)).catch(() => {});
}

/** Is any of the session's forwards starting or running? */
export const forwardsActive = (s: ForwardStatus | undefined) => s?.state === "starting" || s?.state === "running";

export async function startForwards(c: ConnectionConfig) {
  try {
    put(await api.sshForwardStart(c.id));
  } catch (e) {
    toast.error(errMsg(e));
  }
}

export function stopForwards(c: ConnectionConfig) {
  api.sshForwardStop(c.id).catch((e) => toast.error(errMsg(e)));
}
