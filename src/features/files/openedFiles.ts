// Files opened with DevCat (double-click in Finder / Explorer, "Open With", dropped on the
// Dock icon, or an agent's `devcat_open_file`): the backend queues them and pokes a window.
// .ovpn profiles open the VPN import form (not one an agent opened: that's a file tab);
// anything else opens in an editor tab.
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { IS_MAIN_WINDOW } from "../../lib/windows";
import { useUi } from "../../store/ui";
import { openImportTab } from "../network/actions";
import { openFilePreview } from "./format";

interface OpenedFile {
  path: string;
  /** Its git repository, else its folder. */
  root: string | null;
  /** An agent opened it: a file tab whatever it is (a VPN config an agent wrote isn't offered for import). */
  agent?: boolean;
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
  const w = getCurrentWindow();
  await w.unminimize().catch(() => {});
  await w.setFocus().catch(() => {});
}

// Any window may take them (each file is handed out once); at launch the main window does.
void listen("opened-files", () => void drain()).catch(() => {});
if (IS_MAIN_WINDOW) void drain();
