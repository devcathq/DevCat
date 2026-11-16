// Download a file from an SSH host (an `ssh://…` path) to a folder on this computer, through the
// SFTP transfer the SFTP tab uses. Progress shows in a toast, so it keeps going if the tab closes.
import { listen } from "@tauri-apps/api/event";
import { downloadDir, join } from "@tauri-apps/api/path";
import { open } from "@tauri-apps/plugin-dialog";
import { toast } from "sonner";
import { create } from "zustand";
import { api, errMsg } from "../../lib/api";
import { remoteParts } from "../../lib/remotePath";
import { confirmDialog } from "../connections/dialogs";
import { REVEAL_LABEL, basename, fmtSize, revealPath } from "./format";

const LAST_DIR_KEY = "devcat.download.dir";

/** Downloads running, by remote path: null while connecting (no bytes yet), else percent. The
 *  Download buttons show it, so a click visibly does something before the first bytes arrive. */
export const useDownloads = create<{ active: Record<string, number | null> }>(() => ({ active: {} }));

function setActive(path: string, pct: number | null | undefined) {
  useDownloads.setState((s) => {
    const active = { ...s.active };
    if (pct === undefined) delete active[path];
    else active[path] = pct;
    return { active };
  });
}

/** `size`: the file's size when a preview already knows it (then the host isn't asked first). */
export async function downloadRemoteFile(path: string, size?: number): Promise<void> {
  const name = basename(remoteParts(path)?.path ?? path);
  if (path in useDownloads.getState().active) {
    toast.message(`${name} is already downloading`);
    return;
  }
  let start: string | undefined;
  try { start = localStorage.getItem(LAST_DIR_KEY) ?? (await downloadDir()); } catch { /* no default */ }
  const dir = await open({ directory: true, multiple: false, defaultPath: start, title: `Download ${name} to…` });
  if (typeof dir !== "string") return;
  try { localStorage.setItem(LAST_DIR_KEY, dir); } catch { /* not remembered */ }

  const exists = await api.sftpLocalExists(dir, name).catch(() => false);
  if (exists) {
    const ok = await confirmDialog({
      title: "Replace file?",
      message: <>{name} already exists in {basename(dir) || dir}. Replace it?</>,
      confirmLabel: "Replace",
      danger: true,
    });
    if (!ok) return;
  }

  const jobId = crypto.randomUUID();
  setActive(path, null);
  // Until the first bytes: logging in to the host can take a few seconds.
  const t = toast.loading(`Downloading ${name}…`, {
    description: "Connecting…",
    action: { label: "Cancel", onClick: () => void api.sftpCancel(jobId).catch(() => {}) },
  });
  const offs = [
    listen<{ jobId: string; bytes: number; bytesTotal: number }>("sftp-progress", (e) => {
      const p = e.payload;
      if (p.jobId !== jobId || !p.bytesTotal || !p.bytes) return;
      const pct = Math.min(100, Math.floor((p.bytes / p.bytesTotal) * 100));
      setActive(path, pct);
      toast.loading(`Downloading ${name}… ${pct}%`, {
        id: t,
        description: `${fmtSize(p.bytes)} of ${fmtSize(p.bytesTotal)}`,
      });
    }),
    listen<{ jobId: string; error: string | null; cancelled: boolean }>("sftp-done", async (e) => {
      const p = e.payload;
      if (p.jobId !== jobId) return;
      offs.forEach((o) => void o.then((f) => f()));
      setActive(path, undefined);
      if (p.cancelled) toast.message(`Download of ${name} cancelled`, { id: t, description: undefined, action: undefined });
      else if (p.error) toast.error(`Download of ${name} failed: ${p.error}`, { id: t, description: undefined, action: undefined });
      else {
        const dst = await join(dir, name);
        toast.success(`Downloaded ${name}`, {
          id: t,
          description: dir,
          action: { label: REVEAL_LABEL, onClick: () => void revealPath(dst) },
        });
      }
    }),
  ];
  try {
    await Promise.all(offs);
    await api.sftpTransfer(jobId, [{ src: path, dstDir: dir, size }]);
  } catch (e) {
    offs.forEach((o) => void o.then((f) => f()));
    setActive(path, undefined);
    toast.error(errMsg(e), { id: t, description: undefined, action: undefined });
  }
}
