// The room tab's dialogs: people (who's here, invites, roles) and settings (the room's, and this
// user's agent in it).
import clsx from "clsx";
import { Copy, Link2, Trash2, UserMinus } from "lucide-react";
import { useEffect, useState } from "react";
import { toast } from "sonner";
import { Badge, Button, Checkbox, Dialog, Field, Input, Select } from "../../components/ui";
import { api, errMsg } from "../../lib/api";
import type { AiProvider } from "../../lib/types";
import { useTabs } from "../../store/tabs";
import { confirmDialog } from "../connections/dialogs";
import { rooms, type MemberView, type Mentions, type Role, type RoomView } from "./api";
import { useRooms } from "./store";

const ROLE: Record<Role, string> = { owner: "Owner", editor: "Editor", viewer: "Reads only", keeper: "Keeper" };

export function pathText(m: MemberView): string | null {
  if (!m.path) return null;
  return m.path.direct ? `direct · ${m.path.rttMs} ms` : `through relay${m.path.relay ? ` ${m.path.relay.replace(/^https?:\/\//, "").split(".")[0]}` : ""} · ${m.path.rttMs} ms`;
}

const copy = (text: string) => void navigator.clipboard.writeText(text).then(() => toast.success("Copied"));

export function PeopleDialog({ open, onClose, room, view }: { open: boolean; onClose: () => void; room: string; view: RoomView }) {
  const [role, setRole] = useState<Role>("editor");
  const [link, setLink] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  useEffect(() => { if (!open) setLink(null); }, [open]);
  const invite = async () => {
    setBusy(true);
    try {
      setLink(await rooms.invite(room, role));
    } catch (e) {
      toast.error(errMsg(e));
    } finally {
      setBusy(false);
    }
  };
  const remove = async (m: MemberView) => {
    const ok = await confirmDialog({ title: `Remove ${m.name}?`, message: "They keep what they already have, but get nothing new: the room moves to a new key.", confirmLabel: "Remove", danger: true });
    if (!ok) return;
    try {
      await rooms.memberRemove(room, m.id);
    } catch (e) {
      toast.error(errMsg(e));
    }
  };
  const people = view.members.filter((m) => m.role !== "keeper");
  const keepers = view.members.filter((m) => m.role === "keeper");
  return (
    <Dialog open={open} onClose={onClose} title={`People in ${view.settings.name}`} width={580}>
      <div className="space-y-4">
        <div className="space-y-1.5">
          {people.map((m) => (
            <div key={m.id} className="flex items-center gap-3 rounded-md border border-line bg-raised px-3 py-2">
              <span className={clsx("size-2 shrink-0 rounded-full", m.online ? "bg-ok" : "bg-faint/40")} title={m.online ? "Here now" : "Away"} />
              <div className="min-w-0 flex-1">
                <div className="truncate font-medium">{m.name}{m.me && <span className="text-muted"> (you)</span>}</div>
                <div className="truncate text-[11px] text-muted">
                  {view.handles[m.id]}{m.online && m.file ? ` · in ${m.file}${m.typing ? ", typing" : ""}` : ""}{!m.me && pathText(m) ? ` · ${pathText(m)}` : ""}
                </div>
              </div>
              {view.owner && !m.me ? (
                <>
                  <Select className="w-32" value={m.role} onChange={(e) => void rooms.memberRole(room, m.id, e.target.value as Role).catch((er) => toast.error(errMsg(er)))}>
                    <option value="editor">Editor</option>
                    <option value="viewer">Reads only</option>
                  </Select>
                  <Button size="sm" variant="ghost" title="Remove from the room" onClick={() => void remove(m)}><UserMinus className="size-3.5" /></Button>
                </>
              ) : (
                <Badge tone={m.role === "owner" ? "accent" : "default"}>{ROLE[m.role]}</Badge>
              )}
            </div>
          ))}
          {keepers.map((k) => (
            <div key={k.id} className="flex items-center gap-3 rounded-md border border-dashed border-line px-3 py-2 text-[12px] text-muted">
              <span className="min-w-0 flex-1 truncate">{k.name}: keeps changes for whoever is away (can't read them) <span className="font-mono text-faint">{k.id.slice(0, 12)}…</span></span>
              {view.owner && <Button size="sm" variant="ghost" title="Remove the keeper" onClick={() => void remove(k)}><UserMinus className="size-3.5" /></Button>}
            </div>
          ))}
        </div>
        {view.owner && (
          <div className="space-y-2 border-t border-line pt-3">
            <div className="font-semibold">Invite someone</div>
            <div className="flex items-end gap-2">
              <Field label="As" className="w-40">
                <Select value={role} onChange={(e) => { setRole(e.target.value as Role); setLink(null); }}>
                  <option value="editor">Editor</option>
                  <option value="viewer">Reads only</option>
                </Select>
              </Field>
              <Button variant="primary" loading={busy} onClick={() => void invite()}><Link2 className="size-3.5" />Make an invite link</Button>
            </div>
            {link && (
              <div className="space-y-1.5">
                <div className="flex items-center gap-1.5">
                  <Input readOnly value={link} className="font-mono text-[11px]" onFocus={(e) => e.target.select()} />
                  <Button onClick={() => copy(link)}><Copy className="size-3.5" />Copy</Button>
                </div>
                <p className="text-[11px] text-muted">Send it to one person. It works once, for 24 hours, and only lets them <i>ask</i>: you let them in from here, with DevCat open.</p>
              </div>
            )}
            {view.invites.length > 0 && <div className="text-[11px] text-faint">{view.invites.length} invite{view.invites.length === 1 ? "" : "s"} not used yet.</div>}
          </div>
        )}
      </div>
    </Dialog>
  );
}

export function SettingsDialog({ open, onClose, room, view, tabId }: { open: boolean; onClose: () => void; room: string; view: RoomView; tabId: string }) {
  const [name, setName] = useState(view.settings.name);
  const [providers, setProviders] = useState<AiProvider[]>([]);
  const [keeper, setKeeper] = useState("");
  useEffect(() => {
    if (!open) return;
    setName(view.settings.name);
    void api.aiProvidersList().then((p) => setProviders(p.filter((x) => x.kind === "agent"))).catch(() => {});
  }, [open, view.settings.name]);
  const run = async (f: () => Promise<unknown>, done?: string) => {
    try {
      await f();
      if (done) toast.success(done);
    } catch (e) {
      toast.error(errMsg(e));
    }
  };
  const setPrefs = (p: Partial<RoomView["prefs"]>) => void run(() => rooms.prefs(room, { ...view.prefs, ...p }));
  const forget = async () => {
    const ok = await confirmDialog({
      title: `Leave ${view.settings.name} on this DevCat?`,
      message: view.owner ? "You own this room: the others keep their copies, but nobody can let new people in any more." : "Your copy is deleted here. The others keep the room; the owner can invite you again.",
      confirmLabel: "Leave",
      danger: true,
    });
    if (!ok) return;
    await run(async () => {
      await rooms.forget(room);
      useTabs.getState().close(tabId, true);
      await useRooms.getState().refresh();
      onClose();
    });
  };
  return (
    <Dialog open={open} onClose={onClose} title="Room settings" width={540}>
      <div className="space-y-4">
        {view.owner && (
          <div className="space-y-2">
            <div className="font-semibold">The room</div>
            <div className="flex items-end gap-2">
              <Field label="Name" className="flex-1"><Input value={name} onChange={(e) => setName(e.target.value)} /></Field>
              <Button disabled={!name.trim() || name === view.settings.name} onClick={() => void run(() => rooms.settings(room, { name }), "Renamed")}>Rename</Button>
            </div>
            <Checkbox
              label="Agents may edit directly inside a part they claimed (otherwise they only suggest, and a person accepts)"
              checked={view.settings.agentEdits}
              onChange={(v) => void run(() => rooms.settings(room, { agentEdits: v }))}
            />
            <div className="flex items-end gap-2">
              <Field label="Add a keeper" hint="Run `devcat keeper` on an always-on machine and paste its id. It keeps changes for whoever is away, sealed: it can't read them." className="flex-1">
                <Input value={keeper} onChange={(e) => setKeeper(e.target.value)} placeholder="64 hex characters" className="font-mono text-[11.5px]" />
              </Field>
              <Button disabled={!/^[0-9a-f]{64}$/i.test(keeper.trim())} onClick={() => void run(async () => { await rooms.keeperAdd(room, keeper.trim()); setKeeper(""); }, "Keeper added")}>Add</Button>
            </div>
          </div>
        )}
        <div className="space-y-2">
          <div className="font-semibold">Your agent here <span className="font-mono text-[12px] font-normal text-muted">{view.handles[view.me]}</span></div>
          <Field label="Who can wake it" hint="Mentioning it runs it with your AI connection, at your cost. Agents answering agents stop after a few rounds, and it runs at most 20 times an hour per room.">
            <Select value={view.prefs.mentions} onChange={(e) => setPrefs({ mentions: e.target.value as Mentions })}>
              <option value="anyone">Anyone in the room, and their agents</option>
              <option value="me">Only me</option>
              <option value="off">Nobody: keep my agent out of this room</option>
            </Select>
          </Field>
          <Field label="AI connection" hint={providers.length === 0 ? "Add a DevCat Agent connection first (AI → Connections)." : undefined}>
            <Select value={view.prefs.agentProvider ?? ""} onChange={(e) => setPrefs({ agentProvider: e.target.value || null })}>
              <option value="">The first DevCat Agent connection</option>
              {providers.map((p) => <option key={p.id} value={p.id}>{p.name}</option>)}
            </Select>
          </Field>
        </div>
        <div className="space-y-2">
          <div className="font-semibold">Your coding agents</div>
          <Checkbox
            label="Let my coding agents use this room: Claude Code, Codex, Cursor… (through DevCat's MCP server) and DevCat Agent chats can read it, write in the thread, suggest changes, and take and update tasks, as your agent"
            checked={!!view.prefs.codingAgents}
            onChange={(v) => setPrefs({ codingAgents: v })}
          />
          <p className="text-[11px] text-muted">
            Only you choose this, only for you; the others don't see it. Claude Code and Codex get the room tools after Sync MCP (Connections → the plug button), with DevCat open.
            What people write in the room reaches agents that can run commands on your computer: turn it on in rooms with people you trust. They can't copy your files into the room.
          </p>
        </div>
        <div className="flex items-center justify-between border-t border-line pt-3">
          <span className="text-[11px] text-faint">
            Your DevCat's id in rooms <span className="font-mono">{view.me.slice(0, 12)}…</span>{" "}
            <button className="text-accent hover:underline" title="For `devcat keeper --owner <id>`" onClick={() => copy(view.me)}>copy</button>
          </span>
          <Button variant="danger" onClick={() => void forget()}><Trash2 className="size-3.5" />Leave on this DevCat</Button>
        </div>
      </div>
    </Dialog>
  );
}
