// Files opened with DevCat (double-click in Finder / Explorer, "Open With", dropped on the
// Dock icon, or an agent's `devcat_open_file`): the backend queues them and pokes a window.
// .ovpn profiles open the VPN import form (not one an agent opened: that's a file tab);
// anything else opens in an editor tab. Queries an agent opens (`db_open_query`) arrive the same
// way ("opened-queries") and open in a query tab, running there once when the agent asked for it.
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { IS_MAIN_WINDOW } from "../../lib/windows";
import { useUi } from "../../store/ui";
import { openTab } from "../../store/tabs";
import { useConnections } from "../../store/connections";
import { openImportTab } from "../network/actions";
import { openFilePreview } from "./format";

interface OpenedFile {
  path: string;
  /** Its git repository, else its folder. */
  root: string | null;
  /** An agent opened it: a file tab whatever it is (a VPN config an agent wrote isn't offered for import). */
  agent?: boolean;
}

interface OpenedQuery {
  connId: string;
  database: string | null;
  sql: string;
  title: string | null;
  savedQueryId: string | null;
  /** Run it once the tab opens: on the connection's session, or on a read-only one. */
  run: "full" | "read-only" | null;
}

async function bringToFront() {
  const w = getCurrentWindow();
  await w.unminimize().catch(() => {});
  await w.setFocus().catch(() => {});
}

async function drainQueries() {
  const qs = (await invoke<OpenedQuery[] | null>("take_opened_queries").catch(() => null)) ?? [];
  if (!qs.length) return;
  for (const q of qs) {
    const name = useConnections.getState().byId(q.connId)?.name ?? "?";
    // always a new tab (no key): a saved query's open tab may hold edits, and this one may run
    openTab(
      { type: "query", connId: q.connId, database: q.database, schema: null, initialSql: q.sql, savedQueryId: q.savedQueryId ?? undefined, autoRun: q.run ?? undefined },
      q.title ?? `Query – ${name}`,
    );
  }
  await bringToFront();
}

async function drain() {
  const files = (await invoke<OpenedFile[] | null>("take_opened_files").catch(() => null)) ?? [];
  if (!files.length) return;
  const vpn = files.filter((f) => !f.agent && /\.ovpn$/i.test(f.path));
  if (vpn.length) {
    const ui = useUi.getState();
    ui.setSidebar("network");
    ui.setSidebarCollapsed(false);
    vpn.forEach((f) => openImportTab(f.path));
  }
  files.filter((f) => !vpn.includes(f)).forEach((f) => openFilePreview(f.path, f.root));
  await bringToFront();
}

// Any window may take them (each file is handed out once); at launch the main window does.
void listen("opened-files", () => void drain()).catch(() => {});
void listen("opened-queries", () => void drainQueries()).catch(() => {});
if (IS_MAIN_WINDOW) { void drain(); void drainQueries(); }
