// Rooms: the sidebar list, a new room, joining one with an invite link, and the
// owner's answer to someone asking to join.
import clsx from "clsx";
import { DoorOpen, Plus, Users } from "lucide-react";
import { useEffect, useState } from "react";
import { toast } from "sonner";
import { Badge, Button, Dialog, Empty, ErrorBox, Field, Input } from "../../components/ui";
import { errMsg } from "../../lib/api";
import { rooms, type JoinRequest } from "./api";
import { myName, openRoom, setMyName, useRooms } from "./store";

type Mode = "list" | "new" | "join";

export function RoomsDialog() {
  const open = useRooms((s) => s.dialog);
  const show = useRooms((s) => s.show);
  const list = useRooms((s) => s.list);
  const startMode = useRooms((s) => s.dialogMode);
  const [mode, setMode] = useState<Mode>("list");
  const [error, setError] = useState<string | null>(null);
  useEffect(() => { if (open) { setMode(startMode); setError(null); } }, [open, startMode]);
  const close = () => show(false);
  return (
    <Dialog open={open} onClose={close} title="Rooms" width={560}>
      {mode === "list" && (
        <div className="space-y-3">
          <p className="text-[12px] text-muted">
            Share documents with other people, talk about them, and let each person's DevCat Agent join in. Everyone stays on their own network: DevCats connect to each other directly, or through the relay when they can't.
          </p>
          <div className="flex gap-2">
            <Button variant="primary" onClick={() => setMode("new")}><Plus className="size-3.5" />New room</Button>
            <Button onClick={() => setMode("join")}><DoorOpen className="size-3.5" />Join with an invite</Button>
          </div>
          {list.length === 0 ? (
            <Empty icon={<Users className="size-6" />} title="No rooms yet">Make one and invite people, or join with an invite someone sent you.</Empty>
          ) : (
            <div className="space-y-1.5">
              {list.map((r) => (
                <button key={r.id} onClick={() => { openRoom(r); close(); }} className="flex w-full items-center gap-3 rounded-md border border-line bg-raised px-3 py-2 text-left hover:bg-hover">
                  <Users className="size-4 shrink-0 text-accent" />
                  <div className="min-w-0 flex-1">
                    <div className="truncate font-medium">{r.name}</div>
                    <div className="text-[11px] text-muted">{r.members} member{r.members === 1 ? "" : "s"} · {r.online} here now</div>
                  </div>
                  {r.removed ? <Badge tone="danger">removed</Badge> : <Badge tone={r.role === "owner" ? "accent" : "default"}>{r.role}</Badge>}
                </button>
              ))}
            </div>
          )}
        </div>
      )}
      {mode === "new" && <NewRoom onBack={() => setMode("list")} onDone={close} setError={setError} />}
      {mode === "join" && <JoinRoom onBack={() => setMode("list")} onDone={close} setError={setError} />}
      {error && <div className="mt-3"><ErrorBox error={error} /></div>}
    </Dialog>
  );
}

function NewRoom({ onBack, onDone, setError }: { onBack: () => void; onDone: () => void; setError: (e: string | null) => void }) {
  const [name, setName] = useState("");
  const [me, setMe] = useState(myName());
  const [busy, setBusy] = useState(false);
  const create = async () => {
    setBusy(true);
    setError(null);
    try {
      setMyName(me);
      const id = await rooms.create(name, me);
      await useRooms.getState().refresh();
      openRoom({ id, name: name.trim() });
      onDone();
    } catch (e) {
      setError(errMsg(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="space-y-3">
      <Field label="Room name"><Input autoFocus value={name} onChange={(e) => setName(e.target.value)} placeholder="Q4 spec" /></Field>
      <Field label="Your name" hint="What the others see (and your agent's handle: @yourname-agent)."><Input value={me} onChange={(e) => setMe(e.target.value)} placeholder="Arif" /></Field>
      <div className="flex justify-end gap-2">
        <Button onClick={onBack}>Back</Button>
        <Button variant="primary" loading={busy} disabled={!name.trim() || !me.trim()} onClick={() => void create()}>Make the room</Button>
      </div>
    </div>
  );
}

function JoinRoom({ onBack, onDone, setError }: { onBack: () => void; onDone: () => void; setError: (e: string | null) => void }) {
  const [link, setLink] = useState("");
  const [me, setMe] = useState(myName());
  const [busy, setBusy] = useState(false);
  const join = async () => {
    setBusy(true);
    setError(null);
    try {
      setMyName(me);
      const id = await rooms.join(link.trim(), me);
      await useRooms.getState().refresh();
      const r = useRooms.getState().list.find((x) => x.id === id);
      openRoom({ id, name: r?.name ?? "Room" });
      toast.success(`You're in ${r?.name ?? "the room"}.`);
      onDone();
    } catch (e) {
      setError(errMsg(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="space-y-3">
      <Field label="Invite link" hint="It starts with devcat://room. The room's owner lets you in from their DevCat (they need it open)."><Input autoFocus value={link} onChange={(e) => setLink(e.target.value)} placeholder="devcat://room?v=1&room=…" className="font-mono text-[11.5px]" /></Field>
      <Field label="Your name"><Input value={me} onChange={(e) => setMe(e.target.value)} placeholder="Akbar" /></Field>
      {busy && <div className="text-[12px] text-muted">Asking the owner… they see your request in their DevCat.</div>}
      <div className="flex justify-end gap-2">
        <Button onClick={onBack} disabled={busy}>Back</Button>
        <Button variant="primary" loading={busy} disabled={!link.trim().startsWith("devcat://room") || !me.trim()} onClick={() => void join()}>Ask to join</Button>
      </div>
    </div>
  );
}

/** Someone asks to join one of this user's rooms. */
export function RoomJoinPrompt() {
  const pending = useRooms((s) => s.pending);
  const req: JoinRequest | undefined = pending[0];
  const [busy, setBusy] = useState(false);
  if (!req) return null;
  const answer = async (role: "editor" | "viewer" | null) => {
    setBusy(true);
    try {
      await rooms.answer(req.requestId, role);
    } catch (e) {
      toast.error(errMsg(e));
    } finally {
      useRooms.setState((s) => ({ pending: s.pending.filter((p) => p.requestId !== req.requestId) }));
      setBusy(false);
    }
  };
  return (
    <Dialog
      open
      onClose={() => void answer(null)}
      title="Someone wants to join a room"
      width={460}
      footer={
        <>
          <Button onClick={() => void answer(null)} disabled={busy}>Refuse</Button>
          {req.role !== "viewer" && <Button onClick={() => void answer("viewer")} disabled={busy}>Let in to read only</Button>}
          <Button variant="primary" onClick={() => void answer(req.role === "viewer" ? "viewer" : "editor")} disabled={busy}>Let in{req.role === "viewer" ? " to read" : ""}</Button>
        </>
      }
    >
      <div className="space-y-2 text-[13px]">
        <p><b>{req.name}</b> wants to join <b>{req.roomName}</b> {req.role === "viewer" ? "to read" : "as an editor"}, with the invite you made.</p>
        <p className="font-mono text-[11px] break-all text-faint">{req.member}</p>
        <p className="text-[12px] text-muted">Only let in people you invited. They get the room's documents and thread; their agent can join the discussion.</p>
      </div>
    </Dialog>
  );
}

/** Sidebar: this user's rooms, a new room, and joining one with an invite. */
export function RoomsPanel() {
  const list = useRooms((s) => s.list);
  const agents = useRooms((s) => s.agents);
  const mentions = useRooms((s) => s.mentions);
  useEffect(() => { void useRooms.getState().refresh(false).catch(() => {}); }, []);
  return (
    <div className="flex h-full flex-col">
      {list.length === 0 ? (
        <Empty icon={<Users className="size-6" />} title="No rooms yet">
          Share documents with other people, talk about them, and let each person's DevCat Agent join in.
          <div className="mt-3 flex flex-col gap-2">
            <Button variant="primary" onClick={() => useRooms.getState().show(true, "new")}><Plus className="size-3.5" />New room</Button>
            <Button onClick={() => useRooms.getState().show(true, "join")}><DoorOpen className="size-3.5" />Join with an invite</Button>
          </div>
        </Empty>
      ) : (
        <div className="min-h-0 flex-1 overflow-y-auto py-0.5">
          {list.map((r) => (
            <button key={r.id} onClick={() => openRoom(r)} className="flex w-full items-center gap-2 px-3 py-1.5 text-left hover:bg-hover">
              <Users className={clsx("size-3.5 shrink-0", r.removed ? "text-faint" : "text-accent")} />
              <span className="min-w-0 flex-1">
                <span className="block truncate text-[13px]">{r.name}</span>
                <span className="block truncate text-[11px] text-faint">{r.members} member{r.members === 1 ? "" : "s"} · {r.online} here now</span>
              </span>
              {agents[r.id] && <span className="size-1.5 shrink-0 animate-pulse rounded-full bg-accent" title="Your agent is working here" />}
              {(mentions[r.id] ?? 0) > 0 && <span className="shrink-0 rounded-full bg-accent px-1.5 text-[10px] font-semibold text-app" title="Messages naming you">@{mentions[r.id]}</span>}
              {r.removed ? <Badge tone="danger">removed</Badge> : r.role === "owner" && <Badge tone="accent">owner</Badge>}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
