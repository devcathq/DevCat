// Between a room and the rest of DevCat. In: share a note, a memory, a skill or a repository's
// changes with a room (from the room, or "Share to room…" in Notes and Memory). Out: add a skill
// from a room to your skills, save a room file to your memory, copy one to your notes. Nothing
// comes out on its own: each person looks at what will be written on their computer, and
// confirms it.
import { homeDir } from "@tauri-apps/api/path";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import clsx from "clsx";
import { AlertTriangle, Brain, GitCompare, NotebookPen, Sparkles, Users } from "lucide-react";
import { useEffect, useMemo, useState, type ReactNode } from "react";
import { toast } from "sonner";
import { create } from "zustand";
import { Badge, Button, Dialog, Empty, Field, Input, Select, Spinner } from "../../components/ui";
import { api, errMsg } from "../../lib/api";
import type { Memory, MemoryKind, MemoryPlace, Note } from "../../lib/types";
import { useConnections, useFocusProject } from "../../store/connections";
import { confirmDialog } from "../connections/dialogs";
import { noteTitle, useNotes } from "../notes/store";
import { rooms, type FileInfo, type SkillInfo } from "./api";
import { openRoom, useRooms } from "./store";

// ---------------------------------------------------------------- formats

/** `key: value` pairs of a `---` frontmatter block (nested `metadata:` keys too), and the body. */
export function frontmatter(text: string): { fields: Record<string, string>; body: string } {
  const m = /^---\r?\n([\s\S]*?)\r?\n---\r?\n?/.exec(text);
  if (!m) return { fields: {}, body: text };
  const fields: Record<string, string> = {};
  for (const line of m[1].split(/\r?\n/)) {
    const i = line.indexOf(":");
    if (i < 0) continue;
    const v = line.slice(i + 1).trim().replace(/^["']|["']$/g, "");
    if (v) fields[line.slice(0, i).trim()] = v;
  }
  return { fields, body: text.slice(m[0].length).replace(/^\s*\n/, "") };
}

/** A memory as a file, in the frontmatter Claude Code's memory uses. */
export function memoryFile(m: { name: string; description: string; kind: string; body: string }): string {
  return `---\nname: ${m.name}\ndescription: ${m.description.replace(/\n/g, " ")}\nmetadata:\n  type: ${m.kind}\n---\n\n${m.body.trim()}\n`;
}

const fileName = (s: string) => s.replace(/[\\/:*?"<>|]+/g, "-").replace(/\s+/g, " ").trim().slice(0, 80) || "note";

// ---------------------------------------------------------- share a text

/** `from`: what it was, for the room's activity ("a note", "a memory"). */
interface ShareTextReq { name: string; text: string; folder?: string; what: string; from: string }

/** "Share to room…" from elsewhere in DevCat (Notes, Memory): pick a room, share the text there. */
export const useShareToRoom = create<{ req: ShareTextReq | null; share: (r: ShareTextReq | null) => void }>((set) => ({
  req: null,
  share: (req) => set({ req }),
}));

export function ShareToRoomDialog() {
  const req = useShareToRoom((s) => s.req);
  const list = useRooms((s) => s.list);
  const [room, setRoom] = useState("");
  const [busy, setBusy] = useState(false);
  const editable = list.filter((r) => !r.removed && (r.role === "owner" || r.role === "editor"));
  useEffect(() => {
    if (!req) return;
    void useRooms.getState().refresh().catch(() => {});
  }, [req]);
  useEffect(() => { if (!editable.some((r) => r.id === room)) setRoom(editable[0]?.id ?? ""); }, [editable, room]);
  if (!req) return null;
  const close = () => useShareToRoom.getState().share(null);
  const go = async () => {
    setBusy(true);
    try {
      const { name, note } = await rooms.shareText(room, req.name, req.text, req.folder, req.from);
      const r = list.find((x) => x.id === room);
      toast.success(`Shared ${name} with ${r?.name ?? "the room"}`, r ? { action: { label: "Open", onClick: () => openRoom(r) } } : undefined);
      if (note) toast.info(note, { duration: 10000 });
      close();
    } catch (e) {
      toast.error(errMsg(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Dialog open onClose={close} title={`Share ${req.what} with a room`} width={480}
      footer={<><Button onClick={close}>Cancel</Button><Button variant="primary" loading={busy} disabled={!room} onClick={() => void go()}>Share</Button></>}>
      {editable.length === 0 ? (
        <Empty icon={<Users className="size-6" />} title="No room to share with">Make a room, or join one, where you can edit (Rooms in the left bar).</Empty>
      ) : (
        <div className="space-y-3">
          <Field label="Room"><Select value={room} onChange={(e) => setRoom(e.target.value)}>{editable.map((r) => <option key={r.id} value={r.id}>{r.name}</option>)}</Select></Field>
          <p className="text-[12px] text-muted">
            It becomes <span className="font-mono text-fg">{req.folder ? `${req.folder}/` : ""}{req.name}</span> in the room: everyone in it can read and edit it, and their agents read it.
          </p>
        </div>
      )}
    </Dialog>
  );
}

// --------------------------------------------------- share from the room

type Kind = "note" | "memory" | "skill" | "changes";

/** The room's "Share from DevCat…": a note, a memory, a skill or a repository's changes. */
export function ShareFromDevCatDialog({ room, open, onClose, onShared }: { room: string; open: boolean; onClose: () => void; onShared: (path: string) => void }) {
  const [kind, setKind] = useState<Kind>("note");
  const notes = useNotes((s) => s.notes);
  const projects = useConnections((s) => s.projects);
  const [memories, setMemories] = useState<Memory[] | null>(null);
  const [skills, setSkills] = useState<SkillInfo[] | null>(null);
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    if (!open) return;
    void useNotes.getState().load().catch(() => {});
    void api.memoryList().then((p) => setMemories(p.flatMap((x) => x.memories))).catch(() => setMemories([]));
    const folders = [...new Set(projects.flatMap((p) => p.folders ?? []))];
    void rooms.skillsList(folders).then(setSkills).catch(() => setSkills([]));
  }, [open, projects]);
  const done = (name: string, note: string | null) => {
    toast.success(`Shared ${name}`);
    if (note) toast.info(note, { duration: 10000 });
    onShared(name);
    onClose();
  };
  const run = async (f: () => Promise<void>) => {
    setBusy(true);
    try { await f(); } catch (e) { toast.error(errMsg(e)); } finally { setBusy(false); }
  };
  const shareNote = (n: Note) => run(async () => {
    const r = await rooms.shareText(room, `${fileName(noteTitle(n.body))}.md`, n.body, "notes", "a note");
    done(r.name, r.note);
  });
  const shareMemory = (m: Memory) => run(async () => {
    const r = await rooms.shareText(room, `${m.name}.md`, memoryFile(m), "memory", "a memory");
    done(r.name, r.note);
  });
  const shareSkill = (s: SkillInfo) => run(async () => {
    const out = await rooms.pathsAdd(room, [s.dir], "skills", "a skill");
    for (const x of out) if (!x.name && x.note) toast.error(x.note);
    const first = out.find((x) => x.name.endsWith("/SKILL.md")) ?? out.find((x) => x.name);
    toast.success(`Shared the skill ${s.name} (${out.filter((x) => x.name).length} files)`);
    if (first) onShared(first.name);
    onClose();
  });
  const shareChanges = () => run(async () => {
    const dir = await openDialog({ directory: true, multiple: false, title: "The repository whose changes to share" });
    if (typeof dir !== "string") return;
    const [unstaged, staged] = await Promise.all([api.gitDiffText(dir, false), api.gitDiffText(dir, true)]);
    const diff = [staged, unstaged].filter((d) => d.trim()).join("\n");
    if (!diff.trim()) {
      toast.info("That repository has no changes to share.");
      return;
    }
    const repo = dir.split(/[\\/]/).filter(Boolean).pop() ?? "repo";
    const r = await rooms.shareText(room, `${fileName(repo)} ${new Date().toISOString().slice(0, 10)}.diff`, diff, "changes", "repository changes");
    done(r.name, r.note);
  });

  const tab = (k: Kind, icon: ReactNode, text: string) => (
    <button onClick={() => setKind(k)} className={clsx("flex items-center gap-1.5 rounded px-2.5 py-1 text-[12px]", kind === k ? "bg-hover text-fg" : "text-muted hover:text-fg")}>{icon}{text}</button>
  );
  const item = (key: string, title: string, sub: string, onClick: () => void, badge?: string) => (
    <button key={key} disabled={busy} onClick={onClick} className="flex w-full items-center gap-2 rounded-md border border-line bg-raised px-3 py-2 text-left hover:bg-hover disabled:opacity-60">
      <div className="min-w-0 flex-1">
        <div className="truncate text-[13px] font-medium">{title}</div>
        {sub && <div className="truncate text-[11px] text-muted">{sub}</div>}
      </div>
      {badge && <Badge>{badge}</Badge>}
    </button>
  );
  const projectName = (id?: string | null) => projects.find((p) => p.id === id)?.name;
  return (
    <Dialog open={open} onClose={onClose} title="Share from DevCat" width={560}>
      <div className="space-y-3">
        <div className="flex gap-1 border-b border-line pb-2">
          {tab("note", <NotebookPen className="size-3.5" />, "Note")}
          {tab("memory", <Brain className="size-3.5" />, "Memory")}
          {tab("skill", <Sparkles className="size-3.5" />, "Skill")}
          {tab("changes", <GitCompare className="size-3.5" />, "Repository changes")}
        </div>
        <div className="max-h-[50vh] space-y-1.5 overflow-auto">
          {kind === "note" && (notes.length === 0 ? <Empty title="No notes yet" /> : notes.map((n) => item(n.id, noteTitle(n.body) || "Untitled", projectName(n.projectId) ?? "General", () => void shareNote(n))))}
          {kind === "memory" && (memories === null ? <Spinner /> : memories.length === 0 ? <Empty title="No memories yet" /> : memories.map((m) => item(`${JSON.stringify(m.place)}:${m.name}`, m.name, m.description, () => void shareMemory(m), m.kind)))}
          {kind === "skill" && (skills === null ? <Spinner /> : skills.length === 0
            ? <Empty title="No skills found">Skills are folders with a SKILL.md, in ~/.claude/skills, ~/.agents/skills, ~/.codex/skills, or the same folders in a project's code folders.</Empty>
            : skills.map((s) => item(s.dir, s.name, s.description || s.dir, () => void shareSkill(s), s.from === "~" ? "yours" : s.from.split(/[\\/]/).pop())))}
          {kind === "changes" && (
            <div className="space-y-2 text-[12px] text-muted">
              <p>Shares what you changed in a Git repository (staged and not), as a .diff file in <span className="font-mono">changes/</span>, so others and their agents can review it.</p>
              <Button variant="primary" loading={busy} onClick={() => void shareChanges()}><GitCompare className="size-3.5" />Pick a repository…</Button>
            </div>
          )}
        </div>
        <p className="text-[11px] text-faint">
          {kind === "memory" ? "Memories go to memory/, the room's team memory: every member's agent reads it." : kind === "skill" ? "Skills go to skills/: others can add them to their own skills from there." : kind === "note" ? "Notes go to notes/." : ""}
        </p>
      </div>
    </Dialog>
  );
}

// ------------------------------------------------------------- take out

/** Where a skill from a room can go on this computer. */
function useSkillPlaces(): { value: string; label: string }[] {
  const [home, setHome] = useState<string | null>(null);
  const projects = useConnections((s) => s.projects);
  useEffect(() => { void homeDir().then((h) => setHome(h.replace(/[\\/]+$/, ""))).catch(() => setHome(null)); }, []);
  return useMemo(() => {
    const out: { value: string; label: string }[] = [];
    if (home) {
      out.push({ value: `${home}/.claude/skills`, label: "Mine, every project: ~/.claude/skills (Claude Code, DevCat Agent)" });
      out.push({ value: `${home}/.agents/skills`, label: "Mine, every project: ~/.agents/skills" });
      out.push({ value: `${home}/.codex/skills`, label: "Mine, every project: ~/.codex/skills (Codex)" });
    }
    for (const p of projects) for (const f of p.folders ?? []) out.push({ value: `${f.replace(/[\\/]+$/, "")}/.claude/skills`, label: `${p.name}: ${f}/.claude/skills (whoever works in it)` });
    return out;
  }, [home, projects]);
}

/** Add a skill from the room (a folder with a SKILL.md) to the skills on this computer. */
export function AddSkillDialog({ room, folder, files, skillText, onClose }: { room: string; folder: string; files: FileInfo[]; skillText: string; onClose: () => void }) {
  const places = useSkillPlaces();
  const [place, setPlace] = useState("");
  const [busy, setBusy] = useState(false);
  useEffect(() => { if (!place && places.length) setPlace(places[0].value); }, [places, place]);
  const name = folder.split("/").pop()!.replace(/ \(\d+\)$/, "");
  const inside = files.filter((f) => f.path.startsWith(`${folder}/`));
  const { fields, body } = frontmatter(skillText);
  const add = async () => {
    setBusy(true);
    const dest = `${place}/${name}`;
    try {
      let n: number;
      try {
        n = await rooms.folderSave(room, folder, dest, false, "their skills");
      } catch (e) {
        if (!/already exists/.test(errMsg(e))) throw e;
        if (!(await confirmDialog({ title: `Replace your skill ${name}?`, message: `${dest} already has files: the room's files of the same name replace them (others stay).`, confirmLabel: "Replace", danger: true }))) return;
        n = await rooms.folderSave(room, folder, dest, true, "their skills");
      }
      toast.success(`Added the skill ${name} (${n} files) to ${place}`);
      onClose();
    } catch (e) {
      toast.error(errMsg(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Dialog open onClose={onClose} title={`Add the skill ${fields.name ?? name} to your skills`} width={620}
      footer={<><Button onClick={onClose}>Cancel</Button><Button variant="primary" loading={busy} disabled={!place} onClick={() => void add()}><Sparkles className="size-3.5" />Add it</Button></>}>
      <div className="space-y-3">
        <div className="flex gap-2 rounded-md border border-warn/40 bg-warn/10 p-2.5 text-[12px] text-fg">
          <AlertTriangle className="mt-0.5 size-4 shrink-0 text-warn" />
          <span>A skill is instructions your coding agents follow, with their access to your computer (commands, files). Read it before adding it, and only add skills from people you trust.</span>
        </div>
        {fields.description && <p className="text-[12px] text-muted">{fields.description}</p>}
        <Field label="Where">
          <Select value={place} onChange={(e) => setPlace(e.target.value)}>{places.map((p) => <option key={p.value} value={p.value}>{p.label}</option>)}</Select>
        </Field>
        <div className="text-[11px] text-muted">{inside.length} file{inside.length === 1 ? "" : "s"} go to <span className="font-mono text-fg">{place}/{name}</span>: {inside.map((f) => f.path.slice(folder.length + 1)).join(", ")}</div>
        <pre className="max-h-64 overflow-auto rounded border border-line bg-app p-2 text-[11.5px] whitespace-pre-wrap">{body || skillText}</pre>
      </div>
    </Dialog>
  );
}

/** Save a room file (Markdown, maybe with memory frontmatter) as one of your memories. */
export function SaveMemoryDialog({ room, path, text, onClose }: { room: string; path: string; text: string; onClose: () => void }) {
  const projects = useConnections((s) => s.projects);
  const focus = useFocusProject();
  const { fields, body } = frontmatter(text);
  const [name, setName] = useState(fields.name ?? path.split("/").pop()!.replace(/\.md$/i, ""));
  const [description, setDescription] = useState(fields.description ?? body.split("\n").find((l) => l.trim())?.replace(/^#+\s*/, "").slice(0, 150) ?? "");
  const [kind, setKind] = useState<MemoryKind>((["user", "feedback", "project", "reference"].includes(fields.type) ? fields.type : "project") as MemoryKind);
  const [place, setPlace] = useState(focus ? `project:${focus.id}` : "global");
  const [busy, setBusy] = useState(false);
  const save = async () => {
    setBusy(true);
    try {
      const p: MemoryPlace = place === "global" ? { kind: "global" } : { kind: "project", id: place.slice("project:".length) };
      await api.memorySave({ place: p, name: name.trim(), description: description.trim(), kind, body });
      void rooms.tookOut(room, path, "their memory").catch(() => {});
      toast.success(`Saved ${name} to your memory: your coding agents read it from now on`);
      onClose();
    } catch (e) {
      toast.error(errMsg(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Dialog open onClose={onClose} title="Save to your memory" width={600}
      footer={<><Button onClick={onClose}>Cancel</Button><Button variant="primary" loading={busy} disabled={!name.trim() || !description.trim()} onClick={() => void save()}><Brain className="size-3.5" />Save</Button></>}>
      <div className="space-y-3">
        <p className="text-[12px] text-muted">Your coding agents (Claude Code, Codex, DevCat Agent…) read your memory. Check it says what you want them to follow.</p>
        <div className="grid grid-cols-2 gap-2">
          <Field label="Name"><Input value={name} onChange={(e) => setName(e.target.value)} /></Field>
          <Field label="Kind">
            <Select value={kind} onChange={(e) => setKind(e.target.value as MemoryKind)}>
              <option value="project">Project</option><option value="reference">Reference</option><option value="feedback">Feedback</option><option value="user">User</option>
            </Select>
          </Field>
        </div>
        <Field label="Description"><Input value={description} onChange={(e) => setDescription(e.target.value)} /></Field>
        <Field label="For">
          <Select value={place} onChange={(e) => setPlace(e.target.value)}>
            <option value="global">Every project</option>
            {projects.map((p) => <option key={p.id} value={`project:${p.id}`}>{p.name}</option>)}
          </Select>
        </Field>
        <pre className="max-h-56 overflow-auto rounded border border-line bg-app p-2 text-[11.5px] whitespace-pre-wrap">{body}</pre>
      </div>
    </Dialog>
  );
}

/** Copy a room file into your notes (the focused project's, or General). */
export async function copyToNotes(room: string, path: string, text: string, projectId: string | null) {
  try {
    await useNotes.getState().save({ id: "", projectId, body: text, pinned: false, createdAt: 0, updatedAt: 0 });
    void rooms.tookOut(room, path, "their notes").catch(() => {});
    toast.success("Copied to your notes");
  } catch (e) {
    toast.error(errMsg(e));
  }
}
