// A room (src-tauri/src/rooms.rs): its files on the left as a tree (folders, whiteboards, the team
// memory in memory/), the selected one in the middle (a shared editor everyone types into, with
// agents' claims and suggestions marked and a preview for Markdown; a whiteboard; or a shared
// file), and on the right the thread, tasks, suggestions waiting for people, and the history of
// applied changes.
import Editor, { type OnMount } from "@monaco-editor/react";
import { open as openFile, save as saveFile } from "@tauri-apps/plugin-dialog";
import clsx from "clsx";
import { Brain, CheckSquare, Columns2, Copy, Download, Eye, FilePlus2, FileText, FolderInput, FolderUp, History, Lightbulb, MessagesSquare, NotebookPen, Pencil, PenTool, Plus, Settings, Sparkles, SquarePen, Trash2, Upload, Users } from "lucide-react";
import { Fragment, lazy, Suspense, useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { toast } from "sonner";
import { Badge, Button, Empty, IconButton, Spinner } from "../../components/ui";
import { errMsg } from "../../lib/api";
import { monaco } from "../../lib/monaco";
import { useFocusProject } from "../../store/connections";
import { useTabs } from "../../store/tabs";
import type { Tab } from "../../tabs/types";
import { useContextMenu, type MenuItem } from "../connections/ContextMenu";
import { confirmDialog, promptDialog } from "../connections/dialogs";
import { isFileDrag } from "../files/drop";
import { MarkdownDoc } from "../files/viewers/MarkdownDoc";
import { fromB64, label, rooms, type Claim, type FileInfo, type RoomBoard, type RoomView, type Suggestion } from "./api";
import { AddSkillDialog, copyToNotes, frontmatter, SaveMemoryDialog, ShareFromDevCatDialog } from "./Exchange";
import { FileTree, type TreeTarget } from "./FileTree";
import { bind, Replica, utf16 } from "./replica";
import { PeopleDialog, SettingsDialog, pathText } from "./RoomDialogs";
import { FileHistoryDialog } from "./FileHistory";
import { HistoryPanel, SuggestionsPanel, ThreadPanel } from "./RoomPanels";
import { shareFiles } from "./share";
import { startRooms, useRooms } from "./store";
import { TasksPanel } from "./TasksPanel";

// the drawing code is large: loaded when a whiteboard opens
const Board = lazy(() => import("./Board"));

type Panel = "thread" | "tasks" | "suggestions" | "history";
/** How a Markdown file shows: the editor, the editor and its preview, or the preview. */
type MdMode = "edit" | "split" | "preview";

const isMarkdown = (p: string) => /\.(md|markdown|mdx)$/i.test(p);
const base = (p: string) => p.split("/").pop() ?? p;
const parent = (p: string) => (p.includes("/") ? p.slice(0, p.lastIndexOf("/")) : "");

export default function RoomTab({ tab }: { tab: Tab & { type: "room" } }) {
  const room = tab.roomId;
  const replica = useMemo(() => new Replica(room), [room]);
  const [view, setView] = useState<RoomView | null>(null);
  const [board, setBoard] = useState<RoomBoard | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [file, setFile] = useState<string | null>(null);
  const [panel, setPanel] = useState<Panel>("thread");
  const [people, setPeople] = useState(false);
  const [settings, setSettings] = useState(false);
  const [sharing, setSharing] = useState(false);
  const [skill, setSkill] = useState<string | null>(null);
  const [memory, setMemory] = useState<string | null>(null);
  const [mdMode, setMdMode] = useState<MdMode>("split");
  // the file or folder whose sharing and editing history is shown
  const [historyOf, setHistoryOf] = useState<string | null>(null);
  const [reveal, setReveal] = useState<{ path: string; from: number; to: number; at: number } | null>(null);
  // files dragged over the room (from the system or DevCat's Files tree), to share them
  const [dragging, setDragging] = useState(false);
  const tick = useRooms((s) => s.ticks[room] ?? 0);
  const active = useTabs((s) => s.activeId === tab.id);
  const focus = useFocusProject();
  const menuCtx = useContextMenu();

  useEffect(() => replica.attach(), [replica]);
  useEffect(() => { startRooms(); }, []);
  const load = useCallback(async () => {
    try {
      // The document first: the editor must never show (and take typing into) an empty copy.
      await replica.pull();
      const [v, b] = await Promise.all([rooms.view(room), rooms.board(room)]);
      setView(v);
      setBoard(b);
      setError(null);
    } catch (e) {
      setError(errMsg(e));
    }
  }, [room, replica]);
  // Changes come in bursts (typing, an agent's run): refresh at most a few times a second.
  const timer = useRef<number | null>(null);
  useEffect(() => {
    if (timer.current) window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => void load(), tick === 0 ? 0 : 150);
    return () => { if (timer.current) window.clearTimeout(timer.current); };
  }, [tick, load]);
  // on screen: what named this user here is seen
  useEffect(() => { if (active) useRooms.getState().seen(room); }, [active, room, board?.messages.length]);

  const files = board?.files ?? [];
  useEffect(() => {
    if (file && !files.some((f) => f.path === file)) setFile(null);
    if (!file && files.length) setFile(files.find((f) => f.kind === "text")?.path ?? files[0].path);
  }, [files, file]);
  const current = files.find((f) => f.path === file) ?? null;
  const canEdit = !!view && (view.summary.role === "owner" || view.summary.role === "editor") && !view.summary.removed;
  const openSuggestions = (board?.suggestions ?? []).filter((s) => s.state === "open").length;
  const openTasks = (board?.tasks ?? []).filter((t) => t.state !== "done").length;

  useEffect(() => {
    void rooms.focus(room, file, false).catch(() => {});
  }, [room, file]);
  useEffect(() => () => void rooms.focus(room, null, false).catch(() => {}), [room]);

  if (error && !view) return <div className="p-6"><Empty title="This room can't be opened">{error}</Empty></div>;
  if (!view || !board) return <div className="flex h-full items-center justify-center"><Spinner /></div>;

  const run = async (f: () => Promise<unknown>, done?: string) => {
    try {
      await f();
      if (done) toast.success(done);
    } catch (e) {
      toast.error(errMsg(e));
    }
  };
  const ask = (title: string, labelText: string, value = "", placeholder?: string) => promptDialog({ title, label: labelText, defaultValue: value, placeholder, confirmLabel: "OK" });
  const newFile = async (folder = "") => {
    const name = await ask("New file", "Name (folders with /)", folder ? `${folder}/` : "", "notes/plan.md");
    if (!name) return;
    const path = name.trim().replace(/^\.?\/+/, "");
    await run(async () => { await rooms.fileNew(room, path); setFile(path); });
  };
  const newBoard = async (folder = "") => {
    const name = await ask("New whiteboard", "Name (folders with /)", folder ? `${folder}/` : "", "ideas/brainstorm");
    if (!name) return;
    const path = name.trim().replace(/^\.?\/+/, "");
    await run(async () => { await rooms.boardNew(room, path); setFile(path); });
  };
  const addFile = async (folder?: string) => {
    const picked = await openFile({ multiple: true, directory: false });
    const paths = typeof picked === "string" ? [picked] : picked ?? [];
    if (!paths.length) return;
    await run(async () => {
      const out = await rooms.pathsAdd(room, paths, folder);
      const ok = out.filter((s) => s.name);
      if (ok.length) setFile(ok[0].name);
      for (const s of out) if (s.note) toast.info(s.note, { duration: 10000 });
      if (ok.length) toast.success(ok.length === 1 ? `Shared ${ok[0].name}` : `Shared ${ok.length} files`);
    });
  };
  const addFolder = async (folder?: string) => {
    const picked = await openFile({ multiple: false, directory: true });
    if (typeof picked !== "string") return;
    await run(async () => {
      const out = await rooms.pathsAdd(room, [picked], folder);
      const ok = out.filter((s) => s.name);
      for (const s of out) if (s.note) toast.info(s.note, { duration: 10000 });
      toast.success(`Shared ${ok.length} file${ok.length === 1 ? "" : "s"} from ${base(picked.replace(/[\\/]+$/, ""))}`);
    });
  };
  const rename = async (path: string) => {
    const to = await ask(`Rename or move ${path}`, "New name (folders with /)", path);
    if (!to || to.trim() === path) return;
    await run(async () => {
      const moved = await rooms.rename(room, path, to.trim());
      const now = moved.find(([a]) => a === file);
      if (now) setFile(now[1]);
    });
  };
  const move = (from: string, toFolder: string) => {
    const to = toFolder ? `${toFolder}/${base(from)}` : base(from);
    if (to === from || toFolder === from || toFolder.startsWith(`${from}/`)) return;
    void run(async () => {
      const moved = await rooms.rename(room, from, to);
      const now = moved.find(([a]) => a === file);
      if (now) setFile(now[1]);
    });
  };
  const remove = async (t: TreeTarget) => {
    const n = t.folder ? files.filter((f) => f.path.startsWith(`${t.path}/`)).length : 1;
    if (!(await confirmDialog({ title: `Delete ${t.path}?`, message: t.folder ? `The folder and its ${n} file${n === 1 ? "" : "s"}, for everyone in the room.` : "For everyone in the room.", confirmLabel: "Delete", danger: true }))) return;
    await run(() => (t.folder ? rooms.folderDelete(room, t.path) : rooms.fileDelete(room, t.path)));
  };
  const saveOut = async (t: TreeTarget) => {
    if (t.folder) {
      const dir = await openFile({ directory: true, multiple: false, title: `Where to save ${t.path}` });
      if (typeof dir !== "string") return;
      const dest = `${dir.replace(/[\\/]+$/, "")}/${base(t.path)}`;
      await run(async () => toast.success(`Saved ${await rooms.folderSave(room, t.path, dest)} files to ${dest}`));
      return;
    }
    const dest = await saveFile({ defaultPath: t.file?.kind === "board" && !/\.excalidraw$/i.test(t.path) ? `${base(t.path)}.excalidraw` : base(t.path) });
    if (!dest) return;
    await run(() => rooms.fileSave(room, t.path, dest), "Saved");
  };
  const textOf = (path: string) => replica.text(path).toString();
  const skillFolder = (t: TreeTarget): string | null => {
    if (t.folder) return files.some((f) => f.path === `${t.path}/SKILL.md`) ? t.path : null;
    return base(t.path) === "SKILL.md" && parent(t.path) ? parent(t.path) : null;
  };
  const treeMenu = (t: TreeTarget): MenuItem[] => {
    const ic = (I: typeof Pencil) => <I className="size-3.5" />;
    const sk = skillFolder(t);
    const out: MenuItem[] = [];
    if (t.folder && canEdit) {
      out.push(
        { label: "New file here…", icon: ic(FilePlus2), onClick: () => void newFile(t.path) },
        { label: "New whiteboard here…", icon: ic(PenTool), onClick: () => void newBoard(t.path) },
        { label: "Share files here…", icon: ic(Upload), onClick: () => void addFile(t.path) },
        { label: "Share a folder here…", icon: ic(FolderUp), onClick: () => void addFolder(t.path) },
        "separator",
      );
    }
    if (!t.folder) out.push({ label: "Open", icon: ic(Eye), onClick: () => setFile(t.path) });
    out.push({ label: t.folder ? "Who shared and moved it…" : "Who shared and edited it…", icon: ic(History), onClick: () => setHistoryOf(t.path) });
    if (canEdit) out.push({ label: "Rename or move…", icon: ic(Pencil), onClick: () => void rename(t.path) });
    out.push("separator");
    if (sk) out.push({ label: "Add to my skills…", icon: ic(Sparkles), onClick: () => setSkill(sk) });
    if (!t.folder && t.file?.kind === "text") {
      if (isMarkdown(t.path)) out.push({ label: "Save to my memory…", icon: ic(Brain), onClick: () => setMemory(t.path) });
      out.push({ label: "Copy to my notes", icon: ic(NotebookPen), onClick: () => void copyToNotes(room, t.path, textOf(t.path), focus?.id ?? null) });
      out.push({ label: "Copy the text", icon: ic(Copy), onClick: () => void navigator.clipboard.writeText(textOf(t.path)).then(() => { toast.success("Copied"); void rooms.tookOut(room, t.path, "the clipboard").catch(() => {}); }) });
    }
    out.push({ label: t.folder ? "Save the folder to this computer…" : "Save to this computer…", icon: ic(Download), onClick: () => void saveOut(t) });
    if (canEdit) out.push("separator", { label: t.folder ? "Delete the folder" : "Delete", icon: ic(Trash2), danger: true, onClick: () => void remove(t) });
    return out.filter((x, i, a) => !(x === "separator" && (i === 0 || a[i - 1] === "separator" || i === a.length - 1)));
  };
  const newMenu = (e: React.MouseEvent) => menuCtx.show(e, [
    { label: "New file…", icon: <FilePlus2 className="size-3.5" />, onClick: () => void newFile() },
    { label: "New whiteboard…", icon: <PenTool className="size-3.5" />, onClick: () => void newBoard() },
  ]);
  const shareMenu = (e: React.MouseEvent) => menuCtx.show(e, [
    { label: "Files from this computer…", icon: <Upload className="size-3.5" />, onClick: () => void addFile() },
    { label: "A folder from this computer…", icon: <FolderUp className="size-3.5" />, onClick: () => void addFolder() },
    "separator",
    { label: "From DevCat: a note, memory, skill or changes…", icon: <FolderInput className="size-3.5" />, onClick: () => setSharing(true) },
  ]);
  const show = (s: Suggestion) => {
    setFile(s.path);
    if (s.from !== undefined && s.to !== undefined) setReveal({ path: s.path, from: s.from, to: s.to, at: Date.now() });
  };
  const here = view.members.filter((m) => !m.me && m.online && m.role !== "keeper");
  const md = current?.kind === "text" && isMarkdown(current.path);
  // who brought the open file into the room, and from where (the room's activity), else who made it
  const came = current ? [...board.activity].reverse().find((a) => a.path === current.path && (a.kind === "shared" || a.kind === "created")) : undefined;
  const origin = !current ? "" : came
    ? `${came.kind === "shared" ? "shared" : "made"} by ${label(came.by)}${came.from ? ` from ${came.from}` : ""}, ${new Date(came.at).toLocaleDateString()}`
    : `by ${label(current.createdBy)}, ${new Date(current.createdAt).toLocaleDateString()}`;

  return (
    <div className="relative flex h-full min-h-0 flex-col"
      onDragOver={(e) => { if (canEdit && isFileDrag(e.dataTransfer)) { e.preventDefault(); setDragging(true); } }}
      onDragLeave={(e) => { if (!e.currentTarget.contains(e.relatedTarget as Node)) setDragging(false); }}
      onDrop={(e) => {
        setDragging(false);
        if (!canEdit || !isFileDrag(e.dataTransfer)) return;
        e.preventDefault();
        void shareFiles(room, e.dataTransfer).then((names) => { if (names.length) setFile(names[0]); });
      }}>
      {dragging && (
        <div className="pointer-events-none absolute inset-3 z-20 flex items-center justify-center rounded-xl border-2 border-dashed border-accent/60 bg-app/70">
          <div className="flex max-w-sm flex-col items-center gap-2 rounded-lg border border-line bg-raised px-6 py-5 text-center text-[13px] text-fg shadow-lg">
            <Upload className="size-6 text-accent" />Drop files or folders to share them with the room
            <span className="text-[11px] text-muted">Everyone in it can open them, and agents read them. Drop on a folder in the tree to put them there, or on the thread to name them in your message.</span>
          </div>
        </div>
      )}
      <div className="flex h-9 shrink-0 items-center gap-2 border-b border-line bg-panel px-3">
        <Users className="size-4 text-accent" />
        <span className="font-semibold">{view.settings.name}</span>
        {view.summary.removed && <Badge tone="danger">You were removed: read only</Badge>}
        {view.summary.role === "viewer" && <Badge>Reads only</Badge>}
        {view.prefs.codingAgents && <Badge tone="info">Your coding agents may use it</Badge>}
        <div className="ml-3 flex items-center gap-1">
          {here.slice(0, 6).map((m) => (
            <span key={m.id} title={`${m.name}${m.file ? ` · in ${m.file}${m.typing ? ", typing" : ""}` : ""}${pathText(m) ? ` · ${pathText(m)}` : ""}`} className={clsx("flex size-6 items-center justify-center rounded-full bg-accent/20 text-[11px] font-semibold text-accent", m.file === file && "ring-2 ring-ok")}>
              {m.name.slice(0, 1).toUpperCase()}
            </span>
          ))}
          {here.length === 0 && <span className="text-[11px] text-faint">Nobody else here right now</span>}
        </div>
        <div className="ml-auto flex items-center gap-1">
          <Button size="sm" variant="ghost" onClick={() => setPeople(true)}><Users className="size-3.5" />{view.owner ? "People & invites" : "People"}</Button>
          <IconButton title="Room settings" onClick={() => setSettings(true)}><Settings className="size-3.5" /></IconButton>
        </div>
      </div>
      <div className="flex min-h-0 flex-1">
        <div className="flex w-60 shrink-0 flex-col border-r border-line bg-panel">
          <div className="flex h-8 items-center gap-1 px-2 text-[11px] font-semibold tracking-wide text-faint uppercase">
            Files
            {canEdit && (
              <span className="ml-auto flex gap-0.5">
                <IconButton title="New file or whiteboard" onClick={newMenu}><Plus className="size-3.5" /></IconButton>
                <IconButton title="Share: files, a folder, or a note, memory, skill or changes from DevCat" onClick={shareMenu}><Upload className="size-3.5" /></IconButton>
              </span>
            )}
          </div>
          <div className="min-h-0 flex-1 overflow-auto px-1">
            {files.length === 0
              ? <div className="px-2 py-3 text-[12px] text-muted">No files yet.{canEdit ? " Make one, share files or a folder from this computer, share a note, memory or skill from DevCat, or drop files here." : ""}</div>
              : <FileTree files={files} current={file} onOpen={setFile} members={view.members} claims={board.claims} canEdit={canEdit} menu={treeMenu} onMove={move}
                  onDropFiles={(dt, folder) => void shareFiles(room, dt, folder || undefined).then((names) => { if (names.length) setFile(names[0]); })} />}
          </div>
          {files.some((f) => f.path.startsWith("memory/")) && (
            <div className="border-t border-line px-3 py-1.5 text-[11px] text-muted"><Brain className="mr-1 inline size-3 text-accent" />memory/ is the room's team memory: every member's agent reads it.</div>
          )}
        </div>
        <div className="flex min-w-0 flex-1 flex-col">
          {current && (
            <div className="flex h-8 shrink-0 items-center gap-1 border-b border-line px-2">
              <span className="min-w-0 truncate text-[11.5px] text-muted" title={`${current.path} · ${origin}`}>
                <span className="text-fg">{current.path}</span> · {origin}
              </span>
              <button onClick={() => setHistoryOf(current.path)} title="Who shared, moved, took out and edited it"
                className="flex shrink-0 items-center gap-1 rounded px-1.5 py-0.5 text-[11.5px] text-muted hover:bg-hover hover:text-fg">
                <History className="size-3.5" />Who edited it
              </button>
              {md && (
                <span className="ml-auto flex shrink-0 items-center gap-0.5">
                  <PanelTab active={mdMode === "edit"} onClick={() => setMdMode("edit")} icon={<SquarePen className="size-3.5" />}>Edit</PanelTab>
                  <PanelTab active={mdMode === "split"} onClick={() => setMdMode("split")} icon={<Columns2 className="size-3.5" />}>Both</PanelTab>
                  <PanelTab active={mdMode === "preview"} onClick={() => setMdMode("preview")} icon={<Eye className="size-3.5" />}>Preview</PanelTab>
                </span>
              )}
            </div>
          )}
          <div className="flex min-h-0 flex-1">
            {!current ? (
              <Empty icon={<FileText className="size-6" />} title="Pick a file">Or talk in the thread: mention an agent (e.g. {view.handles[view.me]}) to bring it in.</Empty>
            ) : current.kind === "blob" ? (
              <BlobView room={room} file={current} />
            ) : current.kind === "board" ? (
              <Suspense fallback={<div className="flex h-full w-full items-center justify-center"><Spinner /></div>}>
                <Board key={current.path} replica={replica} path={current.path} readOnly={!canEdit} />
              </Suspense>
            ) : (
              <>
                {(!md || mdMode !== "preview") && (
                  <div className={clsx("min-w-0", md && mdMode === "split" ? "w-1/2 border-r border-line" : "flex-1")}>
                    <RoomEditor
                      key={current.path}
                      room={room}
                      replica={replica}
                      path={current.path}
                      readOnly={!canEdit}
                      claims={board.claims.filter((c) => c.path === current.path && c.winning)}
                      suggestions={board.suggestions.filter((s) => s.path === current.path && s.state === "open" && s.from !== undefined)}
                      reveal={reveal?.path === current.path ? reveal : null}
                    />
                  </div>
                )}
                {md && mdMode !== "edit" && <MarkdownPreview key={`p:${current.path}`} replica={replica} path={current.path} />}
              </>
            )}
          </div>
        </div>
        <div className="flex w-[360px] shrink-0 flex-col border-l border-line bg-panel">
          <div className="flex h-8 shrink-0 items-center gap-0.5 border-b border-line px-1">
            <PanelTab active={panel === "thread"} onClick={() => setPanel("thread")} icon={<MessagesSquare className="size-3.5" />}>Thread</PanelTab>
            <PanelTab active={panel === "tasks"} onClick={() => setPanel("tasks")} icon={<CheckSquare className="size-3.5" />}>
              Tasks{openTasks > 0 && <span className="ml-1 rounded-full bg-accent/20 px-1.5 text-[10px] text-accent">{openTasks}</span>}
            </PanelTab>
            <PanelTab active={panel === "suggestions"} onClick={() => setPanel("suggestions")} icon={<Lightbulb className="size-3.5" />}>
              Suggestions{openSuggestions > 0 && <span className="ml-1 rounded-full bg-warn/20 px-1.5 text-[10px] text-warn">{openSuggestions}</span>}
            </PanelTab>
            <PanelTab active={panel === "history"} onClick={() => setPanel("history")} icon={<History className="size-3.5" />}>History</PanelTab>
          </div>
          <div className="min-h-0 flex-1">
            {panel === "thread" && <ThreadPanel room={room} view={view} messages={board.messages} pins={board.pins} reactions={board.reactions} canShare={canEdit} canEdit={canEdit} files={files.map((f) => f.path)} onOpen={setFile} />}
            {panel === "tasks" && <TasksPanel room={room} view={view} tasks={board.tasks} canEdit={canEdit} />}
            {panel === "suggestions" && <SuggestionsPanel room={room} canEdit={canEdit} suggestions={board.suggestions} onShow={show} />}
            {panel === "history" && <HistoryPanel room={room} canEdit={canEdit} history={board.history} activity={board.activity} />}
          </div>
        </div>
      </div>
      <PeopleDialog open={people} onClose={() => setPeople(false)} room={room} view={view} />
      <SettingsDialog open={settings} onClose={() => setSettings(false)} room={room} view={view} tabId={tab.id} />
      <ShareFromDevCatDialog room={room} open={sharing} onClose={() => setSharing(false)} onShared={setFile} />
      {skill && <AddSkillDialog room={room} folder={skill} files={files} skillText={textOf(`${skill}/SKILL.md`)} onClose={() => setSkill(null)} />}
      {memory && <SaveMemoryDialog room={room} path={memory} text={textOf(memory)} onClose={() => setMemory(null)} />}
      {historyOf && <FileHistoryDialog room={room} path={historyOf} onClose={() => setHistoryOf(null)} />}
      {menuCtx.element}
    </div>
  );
}

/** A Markdown file as it reads, following everyone's typing. */
function MarkdownPreview({ replica, path }: { replica: Replica; path: string }) {
  const [text, setText] = useState(() => replica.text(path).toString());
  useEffect(() => {
    const up = () => setText(replica.text(path).toString());
    up();
    const unsub = replica.doc.subscribe(up);
    const off = replica.onChange(up);
    return () => { unsub(); off(); };
  }, [replica, path]);
  // a memory's or skill's frontmatter shows as its fields, not as Markdown
  const { fields, body } = frontmatter(text);
  const keys = Object.keys(fields);
  return (
    <div className="min-w-0 flex-1 overflow-auto px-6 py-4">
      {keys.length > 0 && (
        <div className="mb-3 grid grid-cols-[auto_1fr] gap-x-3 gap-y-0.5 rounded border border-line bg-raised px-3 py-2 text-[12px]">
          {keys.map((k) => <Fragment key={k}><span className="text-muted">{k}</span><span className="break-words text-fg">{fields[k]}</span></Fragment>)}
        </div>
      )}
      {body.trim() ? <MarkdownDoc text={body} path="" root={null} /> : keys.length === 0 && <div className="text-[12px] text-faint">Nothing to show yet.</div>}
    </div>
  );
}

function PanelTab({ active, onClick, icon, children }: { active: boolean; onClick: () => void; icon: ReactNode; children: ReactNode }) {
  return (
    <button onClick={onClick} className={clsx("flex h-6 items-center gap-1 rounded px-2 text-[12px]", active ? "bg-hover text-fg" : "text-muted hover:text-fg")}>
      {icon}{children}
    </button>
  );
}

/** Monaco's language for a file name (by extension), plain text otherwise. */
function languageOf(path: string): string {
  const ext = `.${path.split(".").pop()?.toLowerCase() ?? ""}`;
  return monaco.languages.getLanguages().find((l) => l.extensions?.includes(ext))?.id ?? "plaintext";
}

function RoomEditor({ room, replica, path, readOnly, claims, suggestions, reveal }: {
  room: string; replica: Replica; path: string; readOnly: boolean; claims: Claim[]; suggestions: Suggestion[];
  reveal: { from: number; to: number; at: number } | null;
}) {
  const editorRef = useRef<monaco.editor.IStandaloneCodeEditor | null>(null);
  const decorations = useRef<monaco.editor.IEditorDecorationsCollection | null>(null);
  const [mounted, setMounted] = useState<monaco.editor.IStandaloneCodeEditor | null>(null);
  const typing = useRef<number | null>(null);
  useEffect(() => () => { if (typing.current) window.clearTimeout(typing.current); }, []);
  // The binding lives with the effect (bound again if React re-runs it), not with onMount.
  useEffect(() => (mounted ? bind(monaco, mounted, replica, path) : undefined), [mounted, replica, path]);

  const onMount: OnMount = (editor) => {
    editorRef.current = editor;
    setMounted(editor);
    decorations.current = editor.createDecorationsCollection([]);
    // Tell the others who's typing where (cleared after a pause).
    editor.onDidChangeModelContent(() => {
      if (readOnly) return;
      if (!typing.current) void rooms.focus(room, path, true).catch(() => {});
      if (typing.current) window.clearTimeout(typing.current);
      typing.current = window.setTimeout(() => { typing.current = null; void rooms.focus(room, path, false).catch(() => {}); }, 2500);
    });
    paint();
  };

  const paint = useCallback(() => {
    const ed = editorRef.current;
    const model = ed?.getModel();
    if (!ed || !model || !decorations.current) return;
    const text = replica.text(path);
    const range = (from: number, to: number) => {
      const a = model.getPositionAt(utf16(text, from));
      const b = model.getPositionAt(utf16(text, to));
      return new monaco.Range(a.lineNumber, a.column, b.lineNumber, b.column);
    };
    decorations.current.set([
      ...claims.map((c) => ({
        range: range(c.from, c.to),
        options: { className: "room-claim", isWholeLine: false, hoverMessage: { value: `**${label(c.holder)}** is editing this${c.note ? `: ${c.note}` : ""}` }, overviewRuler: { color: "#8b5cf6", position: monaco.editor.OverviewRulerLane.Left } },
      })),
      ...suggestions.map((s) => ({
        range: range(s.from ?? 0, s.to ?? 0),
        options: { className: s.outdated ? "room-suggestion-stale" : "room-suggestion", hoverMessage: { value: `**${label(s.author)}** suggests:\n\n\`\`\`\n${s.replacement}\n\`\`\`${s.note ? `\n\n${s.note}` : ""}${s.outdated ? "\n\n_Out of date: the text changed since._" : ""}` }, overviewRuler: { color: "#f59e0b", position: monaco.editor.OverviewRulerLane.Right } },
      })),
    ]);
  }, [claims, suggestions, replica, path]);
  useEffect(() => { paint(); }, [paint]);
  useEffect(() => replica.onChange(paint), [replica, paint]);
  useEffect(() => {
    const ed = editorRef.current;
    const model = ed?.getModel();
    if (!reveal || !ed || !model) return;
    const text = replica.text(path);
    const a = model.getPositionAt(utf16(text, reveal.from));
    const b = model.getPositionAt(utf16(text, reveal.to));
    ed.revealRangeInCenter(new monaco.Range(a.lineNumber, a.column, b.lineNumber, b.column));
    ed.setSelection(new monaco.Range(a.lineNumber, a.column, b.lineNumber, b.column));
  }, [reveal, replica, path]);

  return (
    <Editor
      theme="devcat"
      path={`room://${room}/${path}`}
      defaultLanguage={languageOf(path)}
      defaultValue=""
      onMount={onMount}
      options={{ readOnly, minimap: { enabled: false }, fontSize: 13, wordWrap: "on", scrollBeyondLastLine: false, renderWhitespace: "selection", automaticLayout: true }}
    />
  );
}

function BlobView({ room, file }: { room: string; file: FileInfo }) {
  const [src, setSrc] = useState<string | null>(null);
  const [text, setText] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const hash = file.blob?.hash;
  const mime = file.blob?.mime ?? "";
  const image = mime.startsWith("image/") && mime !== "image/svg+xml";
  // text too large to edit together (rooms.rs `share`): read here, not edited
  const textual = mime === "text/plain";
  // By content, not by `file`: that object is new on every refresh of the room (anyone's typing or
  // presence), and fetching the image again each time made it blink.
  useEffect(() => {
    setSrc(null);
    setText(null);
    setError(null);
    if ((!image && !textual) || !hash) return;
    let alive = true;
    void rooms.blob(room, hash).then((b64) => {
      if (!alive) return;
      const bytes = fromB64(b64);
      if (textual) setText(new TextDecoder().decode(bytes));
      else setSrc(URL.createObjectURL(new Blob([bytes as BlobPart], { type: mime })));
    }).catch((e) => alive && setError(errMsg(e)));
    return () => { alive = false; };
  }, [room, hash, mime, image, textual]);
  useEffect(() => () => { if (src) URL.revokeObjectURL(src); }, [src]);
  const saveAs = async () => {
    if (!file.blob) return;
    const dest = await saveFile({ defaultPath: file.path.split("/").pop() });
    if (!dest) return;
    try {
      await rooms.blobSave(room, file.blob.hash, dest);
      toast.success("Saved");
    } catch (e) {
      toast.error(errMsg(e));
    }
  };
  const size = file.blob ? `${(file.blob.size / 1024).toFixed(file.blob.size < 10240 ? 1 : 0)} KB · ${file.blob.mime} · shared by ${label(file.createdBy)}` : "";
  if (textual && !error) {
    return (
      <div className="flex h-full min-h-0 flex-col">
        <div className="flex h-8 shrink-0 items-center gap-2 border-b border-line px-3 text-[11.5px] text-muted">
          <span className="truncate">{size} · too large to edit together (200,000 characters at most): everyone reads it here</span>
          <Button size="sm" variant="ghost" className="ml-auto" onClick={() => void saveAs()}><Download className="size-3.5" />Save to this computer</Button>
        </div>
        <div className="min-h-0 flex-1">
          {text === null ? (
            <div className="flex h-full items-center justify-center"><Spinner /></div>
          ) : (
            <Editor theme="devcat" path={`room-file://${room}/${file.path}`} defaultLanguage={languageOf(file.path)} value={text}
              options={{ readOnly: true, domReadOnly: true, minimap: { enabled: false }, fontSize: 13, wordWrap: "on", scrollBeyondLastLine: false, automaticLayout: true }} />
          )}
        </div>
      </div>
    );
  }
  return (
    <div className="flex h-full flex-col items-center justify-center gap-3 p-6">
      {image && !src && !error && <Spinner />}
      {src && <img src={src} alt={file.path} className="max-h-[70%] max-w-full rounded border border-line object-contain" />}
      {error && <div className="text-[12px] text-danger">{error}</div>}
      <div className="text-center text-[12px] text-muted">
        <div className="font-medium text-fg">{file.path}</div>
        {file.blob && <div>{size}</div>}
        {textual && <div className="mt-1">Too large to edit together (200,000 characters at most): save it to read it.</div>}
      </div>
      <Button onClick={() => void saveAs()}><Download className="size-3.5" />Save to this computer</Button>
    </div>
  );
}
