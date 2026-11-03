// Download a file from an SSH host (an `ssh://…` path) to a folder on this computer, through the
// SFTP transfer the SFTP tab uses. Progress shows in a toast, so it keeps going if the tab closes.
import { listen } from "@tauri-apps/api/event";
import { downloadDir, join } from "@tauri-apps/api/path";
import { open } from "@tauri-apps/plugin-dialog";
import { toast } from "sonner";
import { api, errMsg } from "../../lib/api";
import { remoteParts } from "../../lib/remotePath";
import { confirmDialog } from "../connections/dialogs";
import { REVEAL_LABEL, basename, fmtSize, revealPath } from "./format";

const LAST_DIR_KEY = "devcat.download.dir";

export async function downloadRemoteFile(path: string): Promise<void> {
  const name = basename(remoteParts(path)?.path ?? path);
  let start: string | undefined;
  try { start = localStorage.getItem(LAST_DIR_KEY) ?? (await downloadDir()); } catch { /* no default */ }
  const dir = await open({ directory: true, multiple: false, defaultPath: start, title: `Download ${name} to…` });
  if (typeof dir !== "string") return;
  try { localStorage.setItem(LAST_DIR_KEY, dir); } catch { /* not remembered */ }

  const existing = await api.fsList(dir, true).catch(() => []);
  if (existing.some((e) => e.name === name)) {
    const ok = await confirmDialog({
      title: "Replace file?",
      message: <>{name} already exists in {basename(dir) || dir}. Replace it?</>,
      confirmLabel: "Replace",
      danger: true,
    });
    if (!ok) return;
  }

  const jobId = crypto.randomUUID();
  const t = toast.loading(`Downloading ${name}…`, {
    action: { label: "Cancel", onClick: () => void api.sftpCancel(jobId).catch(() => {}) },
  });
  const offs = [
    listen<{ jobId: string; bytes: number; bytesTotal: number }>("sftp-progress", (e) => {
      const p = e.payload;
      if (p.jobId !== jobId || !p.bytesTotal) return;
      toast.loading(`Downloading ${name}… ${Math.floor((p.bytes / p.bytesTotal) * 100)}%`, {
        id: t,
        description: `${fmtSize(p.bytes)} of ${fmtSize(p.bytesTotal)}`,
      });
    }),
    listen<{ jobId: string; error: string | null; cancelled: boolean }>("sftp-done", async (e) => {
      const p = e.payload;
      if (p.jobId !== jobId) return;
      offs.forEach((o) => void o.then((f) => f()));
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
    await api.sftpTransfer(jobId, [{ src: path, dstDir: dir }]);
  } catch (e) {
    offs.forEach((o) => void o.then((f) => f()));
    toast.error(errMsg(e), { id: t, description: undefined, action: undefined });
  }
}
