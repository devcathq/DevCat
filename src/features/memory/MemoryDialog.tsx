// Memory: one memory every coding agent shares (Claude Code, Codex, Gemini, Cursor, the DevCat
// Agent), served by `devcat mcp` (see src-tauri/src/memory). Two views: "Sync from Claude" copies
// Claude Code's own memory in and puts DevCat's MCP server in every agent ("Sync Claude memory to
// all agents"); "Memories" lists, edits and forgets what the agents saved; "Other computers" shares
// it with the user's other computers (OtherComputers.tsx). Desktop only.
import clsx from "clsx";
import { Brain, ChevronRight, Pencil, Plus, Trash2 } from "lucide-react";
import { useCallback, useEffect, useMemo, useState } from "react";
import { toast } from "sonner";
import { create } from "zustand";
import { Badge, Button, Checkbox, Dialog, ErrorBox, Field, Input, Select, Spinner, Textarea } from "../../components/ui";
import { api, errMsg } from "../../lib/api";
import type { ClaudeImportItem, ClaudeImportPlan, Memory, MemoryKind, MemoryPlace, PlaceMemories } from "../../lib/types";
import { Segmented } from "../connections/dialog/formBits";
import { OtherComputers, startMemoryPeers, useMemoryChanged } from "./OtherComputers";

const useOpen = create<{ open: boolean; view: View }>(() => ({ open: false, view: "claude" }));
type View = "claude" | "memories" | "computers";

export const openMemory = (view: View = "claude") => useOpen.setState({ open: true, view });

export function MemoryButton() {
  // pairing requests from other computers show in any window
  useEffect(() => startMemoryPeers(), []);
  return (
    <button onClick={() => openMemory()} title="Memory: one memory for all your coding agents" className="flex h-6 items-center rounded px-1.5 text-muted hover:bg-hover hover:text-fg">
      <Brain className="size-3.5" />
    </button>
  );
}

export function MemoryDialog() {
  const open = useOpen((s) => s.open);
  return open ? <MemoryView onClose={() => useOpen.setState({ open: false })} /> : null;
}

const KINDS: MemoryKind[] = ["user", "feedback", "project", "reference"];
const KIND_HINT: Record<MemoryKind, string> = {
  user: "Who you are and how you like to work",
  feedback: "A correction or confirmed approach, and why",
  project: "A fact, decision or constraint the code doesn't show",
  reference: "Where to find something",
};

const STATUS: Record<ClaudeImportItem["status"], { label: string; tone: "ok" | "info" | "warn" | "danger" | "default" }> = {
  new: { label: "New", tone: "ok" },
  changed: { label: "Changed", tone: "info" },
  conflict: { label: "Conflict", tone: "warn" },
  removed: { label: "Deleted in Claude", tone: "danger" },
  same: { label: "Up to date", tone: "default" },
};

const placeKey = (p: MemoryPlace) => (p.kind === "global" ? "global" : `${p.kind}:${p.id}`);

function MemoryView({ onClose }: { onClose: () => void }) {
  const view = useOpen((s) => s.view);
  const setView = (v: View) => useOpen.setState({ view: v });
  return (
    <Dialog
      open
      title={<span className="flex items-center gap-2"><Brain className="size-4" />Memory</span>}
      width={780}
      onClose={onClose}
      footer={view === "claude" ? undefined : <Button onClick={onClose}>Close</Button>}
    >
      <div className="flex flex-col gap-4 text-[13px]">
        <p className="text-fg/90">
          One memory for all your coding agents. Claude Code, Codex, Gemini, Cursor and the DevCat Agent read and save it through
          DevCat's MCP server, and each new session starts with a short brief of it. Switch models mid-task and the next one knows what
          the last one learned, with no other software to install.
        </p>
        <div><Segmented<View> value={view} onChange={setView} options={[{ value: "claude", label: "Sync from Claude" }, { value: "memories", label: "Memories" }, { value: "computers", label: "Other computers" }]} /></div>
        {view === "claude" ? <ClaudeSync onClose={onClose} onDone={() => setView("memories")} /> : view === "memories" ? <Memories /> : <OtherComputers />}
      </div>
    </Dialog>
  );
}

// ---------- Sync from Claude

function ClaudeSync({ onClose, onDone }: { onClose: () => void; onDone: () => void }) {
  const [plan, setPlan] = useState<ClaudeImportPlan | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [chosen, setChosen] = useState<Map<string, boolean>>(new Map());
  const [auto, setAuto] = useState(true);
  const [rules, setRules] = useState(true);
  const [busy, setBusy] = useState(false);
  const [showSame, setShowSame] = useState(false);

  useEffect(() => {
    api.memoryClaudePreview().then((p) => {
      setPlan(p);
      // ticked to start with: what's new or changed (conflicts and removals wait for the user)
      setChosen(new Map(p.items.filter((i) => i.status === "new" || i.status === "changed").map((i) => [i.id, i.global])));
      if (p.synced) {
        setAuto(p.auto);
        setRules(p.rules);
      }
    }, (e) => setError(errMsg(e)));
  }, []);

  const actionable = useMemo(() => plan?.items.filter((i) => i.status !== "same") ?? [], [plan]);
  const same = useMemo(() => plan?.items.filter((i) => i.status === "same") ?? [], [plan]);

  const tick = (i: ClaudeImportItem, on: boolean) => setChosen((m) => {
    const next = new Map(m);
    if (on) next.set(i.id, m.get(i.id) ?? i.global); else next.delete(i.id);
    return next;
  });
  const moveTo = (i: ClaudeImportItem, global: boolean) => setChosen((m) => new Map(m).set(i.id, global));

  const sync = async () => {
    setBusy(true);
    try {
      const r = await api.memorySyncAll([...chosen].map(([id, global]) => ({ id, global })), auto, rules);
      const copied = [r.imported && `${r.imported} copied`, r.updated && `${r.updated} updated`, r.removed && `${r.removed} removed`].filter(Boolean).join(", ");
      toast.success(`Memory synced${copied ? `: ${copied}` : ""}`, {
        description: r.agents.length
          ? `${r.agents.join(", ")} share it from their next session.${r.rulesFiles.length ? ` Updated ${r.rulesFiles.join(", ")}.` : ""}`
          : "No coding agents found on this computer yet: run one once, then sync again.",
      });
      r.notes.forEach((n) => toast.warning(n));
      onDone();
    } catch (e) {
      setError(errMsg(e));
    } finally {
      setBusy(false);
    }
  };

  if (error) return <ErrorBox error={error} />;
  if (!plan) return <div className="flex justify-center py-6"><Spinner /></div>;

  return (
    <div className="flex flex-col gap-3">
      <p className="text-xs text-muted">
        Claude Code keeps its own memory per folder (<span className="font-mono">~/.claude/projects/…/memory</span>). Syncing copies it into the
        shared memory: a folder's memories go to its project, memories about you go to every project. Claude's files aren't changed.
      </p>
      {actionable.length === 0 ? (
        <div className="rounded-md border border-line p-3 text-xs text-muted">
          {plan.items.length ? "Claude's memory is already in the shared memory." : "Claude Code has no memories on this computer yet."}{" "}
          Syncing still puts DevCat's MCP server in your agents, so they all share the memory.
        </div>
      ) : (
        <div className="flex flex-col">
          <div className="flex items-center gap-2 pb-1.5 text-xs text-muted">
            <span className="flex-1">{actionable.length} to sync · {chosen.size} ticked</span>
            <button className="hover:text-fg" onClick={() => setChosen(new Map(actionable.map((i) => [i.id, chosen.get(i.id) ?? i.global])))}>Tick all</button>
            <span>·</span>
            <button className="hover:text-fg" onClick={() => setChosen(new Map())}>None</button>
          </div>
          <div className="max-h-[42vh] overflow-auto rounded-md border border-line">
            {actionable.map((i) => (
              <ImportRow key={i.id} item={i} ticked={chosen.has(i.id)} global={chosen.get(i.id) ?? i.global} onTick={(on) => tick(i, on)} onMove={(g) => moveTo(i, g)} />
            ))}
          </div>
        </div>
      )}
      {same.length > 0 && (
        <div className="text-xs text-muted">
          <button className="flex items-center gap-1 hover:text-fg" onClick={() => setShowSame((s) => !s)}>
            <ChevronRight className={clsx("size-3 transition-transform", showSame && "rotate-90")} />
            {same.length} already up to date
          </button>
          {showSame && <div className="selectable mt-1 pl-4 text-faint">{same.map((i) => i.name).join(", ")}</div>}
        </div>
      )}
      {plan.notes.map((n) => <div key={n} className="rounded-md border border-warn/30 bg-warn/10 p-2 text-xs">{n}</div>)}
      <div className="flex flex-col gap-1.5 border-t border-line pt-3">
        <Checkbox label="Keep in sync: copy new and changed Claude memories automatically" checked={auto} onChange={setAuto} />
        {plan.rulesFiles.length > 0 && (
          <Checkbox label={`Also tell agents about the memory in ${plan.rulesFiles.join(" and ")}`} checked={rules} onChange={setRules} />
        )}
        <span className="pl-5 text-[11px] text-faint">
          Syncing also adds DevCat's MCP server to Claude Code, Codex, Cursor and Gemini (as Sync MCP does). Start a new agent session to pick it up.
        </span>
      </div>
      <div className="flex justify-end gap-2 border-t border-line pt-3">
        <Button onClick={onClose}>Cancel</Button>
        <Button variant="primary" loading={busy} onClick={() => void sync()}>Sync Claude memory to all agents</Button>
      </div>
    </div>
  );
}

function ImportRow({ item, ticked, global, onTick, onMove }: { item: ClaudeImportItem; ticked: boolean; global: boolean; onTick: (on: boolean) => void; onMove: (global: boolean) => void }) {
  const st = STATUS[item.status];
  return (
    <div className="flex items-start gap-2 border-b border-line/60 px-2.5 py-2 last:border-b-0">
      <input type="checkbox" className="mt-0.5 accent-[var(--color-accent)]" checked={ticked} onChange={(e) => onTick(e.target.checked)} />
      <div className="flex min-w-0 flex-1 flex-col gap-0.5">
        <div className="flex items-center gap-1.5">
          <span className="truncate font-medium">{item.name}</span>
          {item.kind && <Badge>{item.kind}</Badge>}
          <Badge tone={st.tone}>{st.label}</Badge>
        </div>
        {item.description && <span className="text-xs text-muted">{item.description}</span>}
        <span className="font-mono text-[11px] text-faint">{item.folder}</span>
        {item.note && <span className="text-[11px] text-warn">{item.note}</span>}
      </div>
      {item.status !== "removed" && (
        <Select className="w-44 shrink-0" value={global ? "global" : "here"} disabled={!ticked} onChange={(e) => onMove(e.target.value === "global")}>
          <option value="here">{item.target}</option>
          <option value="global">Every project</option>
        </Select>
      )}
    </div>
  );
}

// ---------- Memories

type Editing = { place: MemoryPlace; memory?: Memory };

function Memories() {
  const [places, setPlaces] = useState<PlaceMemories[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [editing, setEditing] = useState<Editing | null>(null);
  const [filter, setFilter] = useState("");

  const load = useCallback(() => {
    api.memoryList().then(setPlaces, (e) => setError(errMsg(e)));
  }, []);
  // reload when memories arrive from another computer
  const arrived = useMemoryChanged();
  useEffect(load, [load, arrived]);

  const forget = async (m: Memory) => {
    try {
      await api.memoryDelete(m.place, m.name);
      load();
    } catch (e) {
      toast.error(errMsg(e));
    }
  };

  if (error) return <ErrorBox error={error} />;
  if (!places) return <div className="flex justify-center py-6"><Spinner /></div>;
  if (editing) {
    return <MemoryForm editing={editing} places={places} onCancel={() => setEditing(null)} onSaved={() => { setEditing(null); load(); }} />;
  }

  const q = filter.trim().toLowerCase();
  const matches = (m: Memory) => !q || [m.name, m.description, m.body, m.origin].some((s) => s.toLowerCase().includes(q));
  const total = places.reduce((n, p) => n + p.memories.length, 0);

  return (
    <div className="flex flex-col gap-3">
      <div className="flex items-center gap-2">
        <Input className="flex-1" placeholder={`Search ${total} memor${total === 1 ? "y" : "ies"}`} value={filter} onChange={(e) => setFilter(e.target.value)} />
        <Button onClick={() => setEditing({ place: { kind: "global" } })}><Plus className="size-3.5" />New memory</Button>
      </div>
      {total === 0 && (
        <div className="rounded-md border border-line p-3 text-xs text-muted">
          No memories yet. Agents save them as they work (ask one to "remember" something), or sync Claude Code's memory in.
        </div>
      )}
      <div className="flex max-h-[52vh] flex-col gap-3 overflow-auto">
        {places.map((p) => {
          const shown = p.memories.filter(matches);
          if (!shown.length && (q || p.place.kind !== "global" || !p.handoffs)) return null;
          return (
            <div key={placeKey(p.place)} className="flex flex-col">
              <div className="flex items-center gap-2 pb-1 text-xs font-medium">
                <span className="truncate">{p.label}</span>
                <span className="text-faint">{shown.length}</span>
                {p.handoffs > 0 && <Badge tone="accent">{p.handoffs} open handoff{p.handoffs === 1 ? "" : "s"}</Badge>}
              </div>
              <div className="rounded-md border border-line">
                {shown.map((m) => <MemoryRow key={m.name} memory={m} onEdit={() => setEditing({ place: m.place, memory: m })} onForget={() => void forget(m)} />)}
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
}

function MemoryRow({ memory: m, onEdit, onForget }: { memory: Memory; onEdit: () => void; onForget: () => void }) {
  const [open, setOpen] = useState(false);
  const [confirm, setConfirm] = useState(false);
  return (
    <div className="border-b border-line/60 px-2.5 py-1.5 last:border-b-0">
      <div className="flex items-center gap-1.5">
        <button className="flex min-w-0 flex-1 items-center gap-1.5 text-left" onClick={() => setOpen((o) => !o)}>
          <ChevronRight className={clsx("size-3 shrink-0 text-muted transition-transform", open && "rotate-90")} />
          <span className="truncate font-medium">{m.name}</span>
          <Badge>{m.kind}</Badge>
          <span className="truncate text-xs text-muted">{m.description}</span>
        </button>
        <span className="shrink-0 text-[11px] text-faint">{m.origin || "an agent"} · {m.updated.slice(0, 10)}</span>
        {confirm ? (
          <>
            <Button size="sm" variant="danger" onClick={onForget}>Forget</Button>
            <Button size="sm" onClick={() => setConfirm(false)}>Keep</Button>
          </>
        ) : (
          <>
            <button title="Edit" className="rounded p-1 text-muted hover:bg-hover hover:text-fg" onClick={onEdit}><Pencil className="size-3" /></button>
            <button title="Forget" className="rounded p-1 text-muted hover:bg-hover hover:text-danger" onClick={() => setConfirm(true)}><Trash2 className="size-3" /></button>
          </>
        )}
      </div>
      {open && <div className="selectable mt-1.5 whitespace-pre-wrap pl-4.5 font-mono text-[11.5px] text-fg/85">{m.body || "(empty)"}</div>}
    </div>
  );
}

function MemoryForm({ editing, places, onCancel, onSaved }: { editing: Editing; places: PlaceMemories[]; onCancel: () => void; onSaved: () => void }) {
  const m = editing.memory;
  const [name, setName] = useState(m?.name ?? "");
  const [description, setDescription] = useState(m?.description ?? "");
  const [kind, setKind] = useState<MemoryKind>(m?.kind ?? "user");
  const [body, setBody] = useState(m?.body ?? "");
  const [place, setPlace] = useState(placeKey(editing.place));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const save = async () => {
    const target = places.find((p) => placeKey(p.place) === place)?.place ?? { kind: "global" as const };
    setBusy(true);
    try {
      await api.memorySave({ place: target, name, description, kind, body, oldName: m && placeKey(m.place) === place ? m.name : undefined });
      if (m && placeKey(m.place) !== place) await api.memoryDelete(m.place, m.name);
      onSaved();
    } catch (e) {
      setError(errMsg(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="flex flex-col gap-3">
      {error && <ErrorBox error={error} />}
      <div className="grid grid-cols-2 gap-3">
        <Field label="Name"><Input value={name} placeholder="prefers-pnpm" onChange={(e) => setName(e.target.value)} autoFocus /></Field>
        <Field label="For">
          <Select value={place} onChange={(e) => setPlace(e.target.value)}>
            {places.map((p) => <option key={placeKey(p.place)} value={placeKey(p.place)}>{p.label}</option>)}
          </Select>
        </Field>
      </div>
      <Field label="Description" hint="One line: agents read it to decide whether the memory matters now">
        <Input value={description} onChange={(e) => setDescription(e.target.value)} />
      </Field>
      <Field label="Type" hint={KIND_HINT[kind]}>
        <Select value={kind} onChange={(e) => setKind(e.target.value as MemoryKind)}>
          {KINDS.map((k) => <option key={k} value={k}>{k}</option>)}
        </Select>
      </Field>
      <Field label="Memory">
        <Textarea rows={8} className="font-mono text-[12px]" value={body} onChange={(e) => setBody(e.target.value)} />
      </Field>
      <div className="flex justify-end gap-2">
        <Button onClick={onCancel}>Cancel</Button>
        <Button variant="primary" loading={busy} disabled={!name.trim()} onClick={() => void save()}>Save</Button>
      </div>
    </div>
  );
}
