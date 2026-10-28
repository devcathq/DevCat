// Files and folders dropped or pasted into a room: from DevCat's Files tree (their paths) or from
// the system (their bytes; a folder is walked in the window). Each is shared with the room under a
// free name, in the folder it was dropped on, so everyone in it, and their agents, can open it.
import { toast } from "sonner";
import { errMsg } from "../../lib/api";
import { toBase64 } from "../files/drop";
import { DRAG_PATHS } from "../files/format";
import { rooms, type SharedFile } from "./api";

/** The most a room takes (crates/rooms MAX_BLOB). */
const MAX_BYTES = 25 * 1024 * 1024;
/** The most one dropped folder brings in (rooms.rs MAX_FOLDER_FILES). */
const MAX_FOLDER_FILES = 500;
/** Folders a shared folder leaves out (rooms.rs SKIP_DIRS). */
const SKIP = new Set([".git", ".hg", ".svn", "node_modules", "target", "__pycache__", ".venv", ".DS_Store", "Thumbs.db"]);

/** A pasted screenshot is called "image.png" by the system: give it a name people can tell apart. */
function pastedName(f: File, pasted: boolean) {
  if (!pasted || (f.name && f.name !== "image.png")) return f.name || "file";
  const d = new Date();
  const p = (n: number) => String(n).padStart(2, "0");
  const ext = f.type === "image/jpeg" ? "jpg" : (f.type.split("/")[1] ?? "png");
  return `pasted ${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}.${p(d.getMinutes())}.${p(d.getSeconds())}.${ext}`;
}

const join = (...parts: (string | undefined)[]) => parts.filter(Boolean).join("/");

/** A folder name in `into` no file of the room is in yet: `name`, or `name (2)`… */
function freeFolder(taken: string[], into: string | undefined, name: string): string {
  let candidate = name;
  for (let n = 2; ; n++) {
    const full = join(into, candidate);
    if (!taken.some((p) => p === full || p.startsWith(`${full}/`))) return candidate;
    candidate = `${name} (${n})`;
  }
}

/** The files in a dropped folder, with their path inside it. */
async function walk(dir: FileSystemDirectoryEntry): Promise<{ file: File; rel: string }[]> {
  const out: { file: File; rel: string }[] = [];
  const visit = async (d: FileSystemDirectoryEntry, prefix: string) => {
    const reader = d.createReader();
    // readEntries answers in batches until an empty one
    for (;;) {
      const batch = await new Promise<FileSystemEntry[]>((res, rej) => reader.readEntries(res, rej));
      if (!batch.length) break;
      for (const e of batch) {
        if (SKIP.has(e.name)) continue;
        if (e.isDirectory) await visit(e as FileSystemDirectoryEntry, `${prefix}${e.name}/`);
        else {
          out.push({ file: await new Promise<File>((res, rej) => (e as FileSystemFileEntry).file(res, rej)), rel: `${prefix}${e.name}` });
          if (out.length > MAX_FOLDER_FILES) throw new Error(`${dir.name} has more than ${MAX_FOLDER_FILES} files: share a smaller folder.`);
        }
      }
    }
  };
  await visit(dir, "");
  return out;
}

async function shareBytes(room: string, f: File, name: string, folder: string | undefined, from: string, shared: SharedFile[]) {
  if (f.size > MAX_BYTES) {
    toast.error(`${name} is over 25 MB: too large to share in a room.`);
    return;
  }
  try {
    shared.push(await rooms.fileAddBytes(room, name, await toBase64(f), folder, from));
  } catch (e) {
    toast.error(`Couldn't share ${name}: ${errMsg(e)}`);
  }
}

/** Share what was dropped (`DataTransfer`) or pasted (its files) into `into` (a folder of the room,
 *  none: the top); returns their names in the room. The ones that can't be shared are reported and
 *  left out. Call it from the drop handler itself: a dropped folder can only be read then. */
export async function shareFiles(room: string, from: DataTransfer | File[], into?: string): Promise<string[]> {
  const shared: SharedFile[] = [];
  const fromTree = from instanceof DataTransfer ? from.getData(DRAG_PATHS) : "";
  // a dropped folder's entry is only there during the drop event: take them all first
  const dropped = from instanceof DataTransfer
    ? [...from.items].filter((i) => i.kind === "file").map((i) => ({ file: i.getAsFile(), entry: i.webkitGetAsEntry() }))
    : from.map((f) => ({ file: f as File | null, entry: null as FileSystemEntry | null }));
  if (fromTree) {
    let paths: string[] = [];
    try { paths = JSON.parse(fromTree) as string[]; } catch { return []; }
    try {
      const out = await rooms.pathsAdd(room, paths, into, "DevCat's Files");
      shared.push(...out.filter((s) => s.name));
      for (const s of out) if (!s.name && s.note) toast.error(s.note);
    } catch (e) {
      toast.error(errMsg(e));
    }
  } else {
    const pasted = !(from instanceof DataTransfer);
    let taken: string[] | null = null;
    for (const { file, entry } of dropped) {
      if (entry?.isDirectory) {
        try {
          taken ??= (await rooms.board(room)).files.map((f) => f.path);
          const root = freeFolder(taken, into, entry.name);
          const inside = await walk(entry as FileSystemDirectoryEntry);
          for (const f of inside) {
            const dir = f.rel.includes("/") ? f.rel.slice(0, f.rel.lastIndexOf("/")) : undefined;
            await shareBytes(room, f.file, f.file.name, join(into, root, dir), "a dropped folder", shared);
          }
          taken.push(join(into, root));
        } catch (e) {
          toast.error(errMsg(e));
        }
        continue;
      }
      if (file) await shareBytes(room, file, pastedName(file, pasted), into, pasted ? "a paste" : "a dropped file", shared);
    }
  }
  const out = shared.map((s) => s.name);
  if (out.length) toast.success(out.length === 1 ? `Shared ${out[0]} with the room` : `Shared ${out.length} files with the room`);
  // text too large to edit together went in as a plain file
  for (const s of shared) if (s.note) toast.info(s.note, { duration: 10000 });
  return out;
}
