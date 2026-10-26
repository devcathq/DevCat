// Files dropped on the app: from DevCat's own Files tree (their paths) or from the system (the
// web view gets their bytes, not their path: they're saved under the app's data folder first).
import { api } from "../../lib/api";
import { DRAG_PATHS } from "./format";

/** The drag carries files we can take. */
export const isFileDrag = (dt: DataTransfer | null) => !!dt && [...dt.types].some((t) => t === "Files" || t === DRAG_PATHS);

/** A file's bytes, base64 (what the backend commands take). */
export function toBase64(f: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const r = new FileReader();
    r.onload = () => resolve(String(r.result).replace(/^data:[^,]*,/, ""));
    r.onerror = () => reject(r.error);
    r.readAsDataURL(f);
  });
}

/** Paths of the dropped files (system files are saved under `<data>/agent-attachments/<bucket>`). */
export async function droppedPaths(dt: DataTransfer, bucket: string): Promise<string[]> {
  const fromTree = dt.getData(DRAG_PATHS);
  if (fromTree) {
    try { return JSON.parse(fromTree) as string[]; } catch { return []; }
  }
  const out: string[] = [];
  for (const f of [...dt.files]) out.push(await api.agentAttach(bucket, f.name || "dropped", await toBase64(f)));
  return out;
}

/** A path as typed into a shell / CLI prompt: quoted when it has spaces. */
export const shellQuote = (p: string) => (/[\s"'&()]/.test(p) ? `"${p.replace(/"/g, '\\"')}"` : p);
