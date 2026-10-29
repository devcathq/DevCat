// Who shared, moved, took out and edited a room file, and the room's activity: all from the room's
// own document, which every member's DevCat keeps (peer to peer: no server keeps a log). Who
// edited comes from the document's history itself (each change belongs to a member's DevCat);
// the rest is recorded as it happens.
import { ArrowRightLeft, Download, FilePlus2, Pencil, Share2, Trash2 } from "lucide-react";
import { useEffect, useState, type ReactNode } from "react";
import { Dialog, Empty, Spinner } from "../../components/ui";
import { errMsg } from "../../lib/api";
import { label, rooms, type Activity, type FileHistory } from "./api";

const when = (ms: number) => (ms > 0 ? new Date(ms).toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" }) : "at an unknown time");
const clock = (ms: number) => new Date(ms).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });

const ICON: Record<Activity["kind"], ReactNode> = {
  shared: <Share2 className="size-3.5 text-accent" />,
  created: <FilePlus2 className="size-3.5 text-accent" />,
  moved: <ArrowRightLeft className="size-3.5 text-muted" />,
  deleted: <Trash2 className="size-3.5 text-danger" />,
  tookOut: <Download className="size-3.5 text-warn" />,
};

/** What someone did, as a sentence; `withPath`: name the file (the room's feed). */
export function activityText(a: Activity, withPath: boolean): string {
  const what = withPath ? `${a.path}${a.files ? ` (${a.files} file${a.files === 1 ? "" : "s"})` : ""}` : a.files ? `the folder (${a.files} file${a.files === 1 ? "" : "s"})` : "it";
  switch (a.kind) {
    case "shared": return `shared ${what}${a.from ? ` from ${a.from}` : ""}`;
    case "created": return `made ${what}`;
    case "moved": return `moved ${withPath ? what : "it"} to ${a.to ?? "?"}`;
    case "deleted": return `deleted ${what}`;
    case "tookOut": return `took ${what} out to ${a.to ?? "their computer"}`;
  }
}

export function ActivityRow({ a, withPath }: { a: Activity; withPath: boolean }) {
  return (
    <div className="flex items-start gap-2 py-1 text-[12px]">
      <span className="mt-0.5 shrink-0">{ICON[a.kind]}</span>
      <span className="min-w-0 flex-1 break-words"><span className="font-medium text-fg">{label(a.by)}</span> <span className="text-muted">{activityText(a, withPath)}</span></span>
      <span className="shrink-0 text-[11px] text-faint">{when(a.at)}</span>
    </div>
  );
}

/** The room's activity, newest first. */
export function ActivityFeed({ activity }: { activity: Activity[] }) {
  if (!activity.length) return <div className="py-6 text-center text-[12px] text-muted">Who shares, makes, moves, deletes or takes out files shows up here, for everyone in the room.</div>;
  return <div className="divide-y divide-line/50">{[...activity].reverse().map((a) => <ActivityRow key={a.id} a={a} withPath />)}</div>;
}

export function FileHistoryDialog({ room, path, onClose }: { room: string; path: string; onClose: () => void }) {
  const [h, setH] = useState<FileHistory | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => { void rooms.fileHistory(room, path).then(setH).catch((e) => setError(errMsg(e))); }, [room, path]);
  return (
    <Dialog open onClose={onClose} title={`Who shared and edited ${path.split("/").pop()}`} width={600}>
      {error ? <Empty title="Couldn't read its history">{error}</Empty> : !h ? <div className="flex justify-center p-6"><Spinner /></div> : (
        <div className="space-y-4">
          {h.names.length > 1 && <div className="text-[11.5px] text-muted">Before: {h.names.slice(1).join(" ← ")}</div>}
          <div>
            <div className="mb-1 text-[11px] font-semibold tracking-wide text-faint uppercase">Shared, moved, taken out</div>
            {h.activity.length === 0
              ? <div className="text-[12px] text-muted">Nothing recorded (it was shared before DevCat kept this).</div>
              : [...h.activity].reverse().map((a) => <ActivityRow key={a.id} a={a} withPath={a.path !== path} />)}
          </div>
          <div>
            <div className="mb-1 text-[11px] font-semibold tracking-wide text-faint uppercase">Edited by</div>
            {h.edits.length === 0
              ? <div className="text-[12px] text-muted">No edits.</div>
              : [...h.edits].reverse().map((e, i) => (
                <div key={i} className="flex items-center gap-2 py-1 text-[12px]">
                  <Pencil className="size-3.5 shrink-0 text-muted" />
                  <span className="min-w-0 flex-1 truncate font-medium text-fg">{e.who}</span>
                  <span className="shrink-0 font-mono text-[11px]"><span className="text-ok">+{e.inserted}</span> <span className="text-danger">−{e.deleted}</span></span>
                  <span className="w-44 shrink-0 text-right text-[11px] text-faint">{when(e.from)}{e.to - e.from > 60_000 ? `–${clock(e.to)}` : ""}</span>
                </div>
              ))}
          </div>
          <p className="text-[11px] text-faint">
            From the room's own document, which every member's DevCat keeps: no server holds a log. Edits come from its history (each change is made by a member's DevCat); what was taken out is what members' DevCats recorded when they did it.
          </p>
        </div>
      )}
    </Dialog>
  );
}
