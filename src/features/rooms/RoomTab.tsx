// A room (src-tauri/src/rooms.rs): its files on the left, the selected one in a shared editor
// (everyone types into the same document; agents' claims and suggestions are marked in it), and
// on the right the thread, suggestions waiting for people, and the history of applied changes.
import Editor, { type OnMount } from "@monaco-editor/react";
import { open as openFile, save as saveFile } from "@tauri-apps/plugin-dialog";
import clsx from "clsx";
import { Bot, Download, FilePlus2, FileText, History, Image as ImageIcon, Lightbulb, MessagesSquare, Settings, Trash2, Upload, Users } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { toast } from "sonner";
import { Badge, Button, Empty, IconButton, Spinner } from "../../components/ui";
import { errMsg } from "../../lib/api";
import { monaco } from "../../lib/monaco";
import type { Tab } from "../../tabs/types";
import { confirmDialog, promptDialog } from "../connections/dialogs";
import { fromB64, label, rooms, type Claim, type FileInfo, type RoomBoard, type RoomView, type Suggestion } from "./api";
import { bind, Replica, utf16 } from "./replica";
import { PeopleDialog, SettingsDialog, pathText } from "./RoomDialogs";
import { HistoryPanel, SuggestionsPanel, ThreadPanel } from "./RoomPanels";
import { startRooms, useRooms } from "./store";

type Panel = "thread" | "suggestions" | "history";

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
  const [reveal, setReveal] = useState<{ path: string; from: number; to: number; at: number } | null>(null);
  const tick = useRooms((s) => s.ticks[room] ?? 0);

  useEffect(() => () => replica.dispose(), [replica]);
  useEffect(() => startRooms(), []);
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

  const files = board?.files ?? [];
  useEffect(() => {
    if (file && !files.some((f) => f.path === file)) setFile(null);
    if (!file && files.length) setFile(files.find((f) => f.kind === "text")?.path ?? files[0].path);
  }, [files, file]);
  const current = files.find((f) => f.path === file) ?? null;
  const canEdit = !!view && (view.summary.role === "owner" || view.summary.role === "editor") && !view.summary.removed;
  const openSuggestions = (board?.suggestions ?? []).filter((s) => s.state === "open").length;

  useEffect(() => {
    void rooms.focus(room, file, false).catch(() => {});
  }, [room, file]);
  useEffect(() => () => void rooms.focus(room, null, false).catch(() => {}), [room]);

  if (error && !view) return <div className="p-6"><Empty title="This room can't be opened">{error}</Empty></div>;
  if (!view || !board) return <div className="flex h-full items-center justify-center"><Spinner /></div>;

  const newFile = async () => {
    const path = await promptDialog({ title: "New file", label: "Name (folders with /)", placeholder: "notes/plan.md" });
    if (!path) return;
    try {
      await rooms.fileNew(room, path);
      setFile(path.trim().replace(/^\.?\/+/, ""));
    } catch (e) {
      toast.error(errMsg(e));
    }
  };
  const addFile = async () => {
    const picked = await openFile({ multiple: false, directory: false });
    if (typeof picked !== "string") return;
    try {
      const name = await rooms.fileAdd(room, picked);
      setFile(name);
      toast.success(`Shared ${name}`);
    } catch (e) {
      toast.error(errMsg(e));
    }
  };
  const deleteFile = async (f: FileInfo) => {
    if (!(await confirmDialog({ title: `Delete ${f.path}?`, message: "For everyone in the room.", confirmLabel: "Delete", danger: true }))) return;
    try {
      await rooms.fileDelete(room, f.path);
    } catch (e) {
      toast.error(errMsg(e));
    }
  };
  const show = (s: Suggestion) => {
    setFile(s.path);
    if (s.from !== undefined && s.to !== undefined) setReveal({ path: s.path, from: s.from, to: s.to, at: Date.now() });
  };
  const here = view.members.filter((m) => !m.me && m.online && m.role !== "keeper");

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex h-9 shrink-0 items-center gap-2 border-b border-line bg-panel px-3">
        <Users className="size-4 text-accent" />
        <span className="font-semibold">{view.settings.name}</span>
        {view.summary.removed && <Badge tone="danger">You were removed: read only</Badge>}
        {view.summary.role === "viewer" && <Badge>Reads only</Badge>}
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
        <div className="flex w-56 shrink-0 flex-col border-r border-line bg-panel">
          <div className="flex h-8 items-center gap-1 px-2 text-[11px] font-semibold tracking-wide text-faint uppercase">
            Files
            {canEdit && (
              <span className="ml-auto flex gap-0.5">
                <IconButton title="New file" onClick={() => void newFile()}><FilePlus2 className="size-3.5" /></IconButton>
                <IconButton title="Share a file from this computer" onClick={() => void addFile()}><Upload className="size-3.5" /></IconButton>
              </span>
            )}
          </div>
          <div className="min-h-0 flex-1 overflow-auto px-1 pb-2">
            {files.length === 0 && <div className="px-2 py-3 text-[12px] text-muted">No files yet.{canEdit ? " Make one, or share one from this computer." : ""}</div>}
            {files.map((f) => {
              const viewers = view.members.filter((m) => !m.me && m.online && m.file === f.path);
              const claimed = board.claims.some((c) => c.path === f.path && c.winning);
              return (
                <div key={f.path} className={clsx("group flex items-center gap-1.5 rounded px-2 py-1 text-[12.5px]", f.path === file ? "bg-accent/15 text-fg" : "text-muted hover:bg-hover hover:text-fg")}>
                  <button className="flex min-w-0 flex-1 items-center gap-1.5 text-left" onClick={() => setFile(f.path)}>
                    {f.kind === "blob" ? <ImageIcon className="size-3.5 shrink-0" /> : <FileText className="size-3.5 shrink-0" />}
                    <span className="truncate">{f.path}</span>
                  </button>
                  {claimed && <Bot className="size-3 shrink-0 text-accent" aria-label="An agent is editing part of it" />}
                  {viewers.length > 0 && <span className="text-[10px] text-ok" title={viewers.map((v) => v.name).join(", ")}>{viewers.length}</span>}
                  {canEdit && <button className="hidden text-faint group-hover:block hover:text-danger" title="Delete" onClick={() => void deleteFile(f)}><Trash2 className="size-3" /></button>}
                </div>
              );
            })}
          </div>
        </div>
        <div className="min-w-0 flex-1">
          {!current ? (
            <Empty icon={<FileText className="size-6" />} title="Pick a file">Or talk in the thread: mention an agent (e.g. {view.handles[view.me]}) to bring it in.</Empty>
          ) : current.kind === "blob" ? (
            <BlobView room={room} file={current} />
          ) : (
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
          )}
        </div>
        <div className="flex w-[360px] shrink-0 flex-col border-l border-line bg-panel">
          <div className="flex h-8 shrink-0 items-center gap-0.5 border-b border-line px-1">
            <PanelTab active={panel === "thread"} onClick={() => setPanel("thread")} icon={<MessagesSquare className="size-3.5" />}>Thread</PanelTab>
            <PanelTab active={panel === "suggestions"} onClick={() => setPanel("suggestions")} icon={<Lightbulb className="size-3.5" />}>
              Suggestions{openSuggestions > 0 && <span className="ml-1 rounded-full bg-warn/20 px-1.5 text-[10px] text-warn">{openSuggestions}</span>}
            </PanelTab>
            <PanelTab active={panel === "history"} onClick={() => setPanel("history")} icon={<History className="size-3.5" />}>History</PanelTab>
          </div>
          <div className="min-h-0 flex-1">
            {panel === "thread" && <ThreadPanel room={room} view={view} messages={board.messages} />}
            {panel === "suggestions" && <SuggestionsPanel room={room} canEdit={canEdit} suggestions={board.suggestions} onShow={show} />}
            {panel === "history" && <HistoryPanel room={room} canEdit={canEdit} history={board.history} />}
          </div>
        </div>
      </div>
      <PeopleDialog open={people} onClose={() => setPeople(false)} room={room} view={view} />
      <SettingsDialog open={settings} onClose={() => setSettings(false)} room={room} view={view} tabId={tab.id} />
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
  const unbind = useRef<(() => void) | null>(null);
  const typing = useRef<number | null>(null);
  useEffect(() => () => { unbind.current?.(); if (typing.current) window.clearTimeout(typing.current); }, []);

  const onMount: OnMount = (editor, m) => {
    editorRef.current = editor;
    unbind.current = bind(m, editor, replica, path);
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
  useEffect(() => paint(), [paint]);
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
  const [error, setError] = useState<string | null>(null);
  const image = file.blob?.mime.startsWith("image/") && file.blob.mime !== "image/svg+xml";
  useEffect(() => {
    setSrc(null);
    setError(null);
    if (!image || !file.blob) return;
    let alive = true;
    void rooms.blob(room, file.blob.hash).then((b64) => {
      if (!alive) return;
      const bytes = fromB64(b64);
      setSrc(URL.createObjectURL(new Blob([bytes as BlobPart], { type: file.blob!.mime })));
    }).catch((e) => alive && setError(errMsg(e)));
    return () => { alive = false; };
  }, [room, file, image]);
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
  return (
    <div className="flex h-full flex-col items-center justify-center gap-3 p-6">
      {image && !src && !error && <Spinner />}
      {src && <img src={src} alt={file.path} className="max-h-[70%] max-w-full rounded border border-line object-contain" />}
      {error && <div className="text-[12px] text-danger">{error}</div>}
      <div className="text-center text-[12px] text-muted">
        <div className="font-medium text-fg">{file.path}</div>
        {file.blob && <div>{(file.blob.size / 1024).toFixed(file.blob.size < 10240 ? 1 : 0)} KB · {file.blob.mime} · shared by {label(file.createdBy)}</div>}
      </div>
      <Button onClick={() => void saveAs()}><Download className="size-3.5" />Save to this computer</Button>
    </div>
  );
}
