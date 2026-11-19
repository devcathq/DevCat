// Moving files between the computer and the phone, in pieces (fs_download / fs_upload, see
// src-tauri/src/remote/transfer.rs), and copying paths to paste into an agent chat.
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { IS_WEB } from "./env";
import { host, useApp } from "./store";

const PIECE = 1024 * 1024;
/** The browser keeps a download in memory until it's handed over as a file. */
const BROWSER_MAX = 512 * 1024 * 1024;

/** Browser version (and the dev bridge): files are saved by the browser, not the app. */
export const SAVES_IN_BROWSER = IS_WEB || import.meta.env.MODE === "bridge";

export type Progress = (done: number, total: number) => void;
export interface Stop { stopped: boolean }

function toB64(bytes: Uint8Array): string {
  let s = "";
  for (let i = 0; i < bytes.length; i += 0x8000) s += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(s);
}

const fromB64 = (d: string) => Uint8Array.from(atob(d), (c) => c.charCodeAt(0));

/** Save a file from the computer on the phone. Returns where it went, or null if cancelled. */
export async function download(path: string, name: string, size: number, progress: Progress, stop: Stop): Promise<string | null> {
  if (!SAVES_IN_BROWSER) {
    const id = useApp.getState().current;
    const un = await listen<{ path: string; done: number; total: number }>("m-transfer", (e) => {
      if (e.payload.path === path) progress(e.payload.done, e.payload.total);
    });
    try {
      return await invoke<string | null>("m_download", { id, path, name });
    } finally {
      un();
    }
  }
  if (size > BROWSER_MAX) throw `Too large to download in the browser (over ${BROWSER_MAX / 1024 / 1024} MB). Use the DevCat app.`;
  const parts: Uint8Array<ArrayBuffer>[] = [];
  let offset = 0;
  for (;;) {
    if (stop.stopped) return null;
    const r = await host<{ data: string; size: number; eof: boolean }>("fs_download", { path, offset, len: PIECE });
    const bytes = fromB64(r.data);
    parts.push(bytes);
    offset += bytes.length;
    progress(offset, r.size);
    if (r.eof || bytes.length === 0) break;
  }
  const url = URL.createObjectURL(new Blob(parts));
  const a = document.createElement("a");
  a.href = url;
  a.download = name;
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 60_000);
  return "your browser's downloads";
}

/** Send a file from the phone into `dir` on the computer. Throws `EXISTS: …` if it's there
 *  already and `overwrite` isn't set. Returns false if stopped. */
export async function upload(dir: string, file: File, overwrite: boolean, progress: Progress, stop: Stop): Promise<boolean> {
  let offset = 0;
  try {
    do {
      if (stop.stopped) {
        if (offset > 0) await host("fs_upload_cancel", { dir, name: file.name }).catch(() => {});
        return false;
      }
      const bytes = new Uint8Array(await file.slice(offset, offset + PIECE).arrayBuffer());
      const done = offset + bytes.length >= file.size;
      await host("fs_upload", { dir, name: file.name, offset, data: toB64(bytes), done, overwrite });
      offset += bytes.length;
      progress(offset, file.size);
    } while (offset < file.size);
    return true;
  } catch (e) {
    if (offset > 0) await host("fs_upload_cancel", { dir, name: file.name }).catch(() => {});
    throw e;
  }
}

/** Put text on the clipboard. The browser version is plain HTTP, where `navigator.clipboard`
 *  doesn't exist, so it falls back to a selected text box. */
export async function copyText(text: string): Promise<void> {
  if (window.isSecureContext && navigator.clipboard) {
    await navigator.clipboard.writeText(text);
    return;
  }
  const ta = document.createElement("textarea");
  ta.value = text;
  ta.setAttribute("readonly", "");
  ta.style.cssText = "position:fixed;top:0;left:0;opacity:0";
  document.body.appendChild(ta);
  ta.select();
  ta.setSelectionRange(0, text.length);
  const ok = document.execCommand("copy");
  ta.remove();
  if (!ok) throw "Couldn't copy. Long-press the path to select it instead.";
}

/** `path` relative to the project folder `root` (null when it's outside, or is the root). */
export function relativePath(path: string, root?: string | null): string | null {
  if (!root) return null;
  const r = root.replace(/[\\/]+$/, "");
  const s = r.includes("\\") && !r.includes("/") ? "\\" : "/";
  return path.startsWith(r + s) ? path.slice(r.length + 1) : null;
}

/** Kinds of image agents read; others (HEIC from an iPhone, …) are turned into JPEG first. */
const AGENT_IMAGES: Record<string, string> = { "image/png": "png", "image/jpeg": "jpg", "image/gif": "gif", "image/webp": "webp" };

export async function asAgentImage(file: Blob): Promise<{ blob: Blob; ext: string }> {
  const ext = AGENT_IMAGES[file.type];
  if (ext) return { blob: file, ext };
  const img = await createImageBitmap(file).catch(() => { throw "This image type can't be read. Try a screenshot or a JPEG/PNG."; });
  const c = document.createElement("canvas");
  c.width = img.width;
  c.height = img.height;
  c.getContext("2d")!.drawImage(img, 0, 0);
  const blob = await new Promise<Blob | null>((res) => c.toBlob(res, "image/jpeg", 0.9));
  if (!blob) throw "Couldn't convert the image.";
  return { blob, ext: "jpg" };
}

/** Send an image from the phone to a temp folder on the computer (paste_dir); returns its path
 *  there, to paste into an agent like Claude Code's own image paste. */
export async function uploadPasteImage(file: Blob): Promise<string> {
  const { blob, ext } = await asAgentImage(file);
  const dir = await host<string>("paste_dir");
  const name = `phone-${new Date().toISOString().replace(/[:.]/g, "-")}.${ext}`;
  await upload(dir, new File([blob], name, { type: blob.type }), false, () => {}, { stopped: false });
  const sep = dir.includes("\\") && !dir.includes("/") ? "\\" : "/";
  return dir.replace(/[\\/]+$/, "") + sep + name;
}
