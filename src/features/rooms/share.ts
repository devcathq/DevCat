// Files dropped or pasted into a room: from DevCat's Files tree (their paths) or from the system
// (their bytes). Each is shared with the room under a free name, so everyone in it, and their
// agents, can open it.
import { toast } from "sonner";
import { errMsg } from "../../lib/api";
import { toBase64 } from "../files/drop";
import { DRAG_PATHS } from "../files/format";
import { rooms, type SharedFile } from "./api";

/** The most a room takes (crates/rooms MAX_BLOB). */
const MAX_BYTES = 25 * 1024 * 1024;

/** A pasted screenshot is called "image.png" by the system: give it a name people can tell apart. */
function pastedName(f: File, pasted: boolean) {
  if (!pasted || (f.name && f.name !== "image.png")) return f.name || "file";
  const d = new Date();
  const p = (n: number) => String(n).padStart(2, "0");
  const ext = f.type === "image/jpeg" ? "jpg" : (f.type.split("/")[1] ?? "png");
  return `pasted ${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}.${p(d.getMinutes())}.${p(d.getSeconds())}.${ext}`;
}

/** Share what was dropped (`DataTransfer`) or pasted (its files); returns their names in the room.
 *  The ones that can't be shared are reported and left out. */
export async function shareFiles(room: string, from: DataTransfer | File[]): Promise<string[]> {
  const shared: SharedFile[] = [];
  const fromTree = from instanceof DataTransfer ? from.getData(DRAG_PATHS) : "";
  if (fromTree) {
    let paths: string[] = [];
    try { paths = JSON.parse(fromTree) as string[]; } catch { return []; }
    for (const p of paths) {
      try { shared.push(await rooms.fileAdd(room, p)); } catch (e) { toast.error(errMsg(e)); }
    }
  } else {
    const pasted = !(from instanceof DataTransfer);
    for (const f of from instanceof DataTransfer ? [...from.files] : from) {
      const name = pastedName(f, pasted);
      if (f.size > MAX_BYTES) {
        toast.error(`${name} is over 25 MB: too large to share in a room.`);
        continue;
      }
      try {
        shared.push(await rooms.fileAddBytes(room, name, await toBase64(f)));
      } catch (e) {
        // a folder from the system has no bytes to read
        toast.error(`Couldn't share ${name}: ${errMsg(e)}`);
      }
    }
  }
  const out = shared.map((s) => s.name);
  if (out.length) toast.success(out.length === 1 ? `Shared ${out[0]} with the room` : `Shared ${out.length} files with the room`);
  // text too large to edit together went in as a plain file
  for (const s of shared) if (s.note) toast.info(s.note, { duration: 10000 });
  return out;
}
