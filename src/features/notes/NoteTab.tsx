// One note: Markdown editor with a rendered Preview, saved automatically as you type.
// Toolbar: project (or General), pin, Edit / Preview, save state, delete.
import Editor, { type OnMount } from "@monaco-editor/react";
import clsx from "clsx";
import { Check, Code, Eye, Pin, PinOff, Trash2, TriangleAlert, Users } from "lucide-react";
import { useShareToRoom } from "../rooms/Exchange";
import { useCallback, useEffect, useRef, useState } from "react";
import { toast } from "sonner";
import { Button, IconButton, Spinner, Toolbar } from "../../components/ui";
import { errMsg } from "../../lib/api";
import { useConnections } from "../../store/connections";
import { useTabs } from "../../store/tabs";
import type { Tab } from "../../tabs/types";
import { confirmDialog } from "../connections/dialogs";
import { MarkdownDoc } from "../files/viewers/MarkdownDoc";
import MiniSelect from "../grid/MiniSelect";
import { relativeTime } from "../history/historyActions";
import { noteTitle, useNotes } from "./store";

type Ed = Parameters<OnMount>[0];
const MODE_KEY = "devcat.notes.mode";
const SAVE_DELAY = 500;

export default function NoteTab({ tab }: { tab: Extract<Tab, { type: "note" }> }) {
  const id = tab.noteId;
  const note = useNotes((s) => s.notes.find((n) => n.id === id));
  const loaded = useNotes((s) => s.loaded);
  const projects = useConnections((s) => s.projects);
  const [mode, setModeState] = useState<"edit" | "preview">(() => (localStorage.getItem(MODE_KEY) === "preview" && note?.body.trim() ? "preview" : "edit"));
  const [draft, setDraft] = useState(note?.body ?? "");
  const [status, setStatus] = useState<"idle" | "saving" | "saved" | "error">("idle");
  const [saveError, setSaveError] = useState<string | null>(null);
  const edRef = useRef<Ed | null>(null);
  /** text typed but not yet sent */
  const pending = useRef<string | null>(null);
  const inFlight = useRef(0);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const setMode = (m: "edit" | "preview") => {
    setModeState(m);
    localStorage.setItem(MODE_KEY, m);
    if (m === "edit") requestAnimationFrame(() => edRef.current?.focus());
  };

  const flush = useCallback(async () => {
    if (timer.current) { clearTimeout(timer.current); timer.current = null; }
    const body = pending.current;
    if (body === null) return;
    pending.current = null;
    const cur = useNotes.getState().notes.find((n) => n.id === id);
    if (!cur) return;
    inFlight.current++;
    setStatus("saving");
    try {
      await useNotes.getState().save({ ...cur, body });
      setSaveError(null);
      if (pending.current === null && inFlight.current === 1) setStatus("saved");
    } catch (e) {
      pending.current ??= body; // retried on the next edit / ⌘S
      setSaveError(errMsg(e));
      setStatus("error");
    } finally {
      inFlight.current--;
    }
  }, [id]);

  const onChange = (v: string | undefined) => {
    const text = v ?? "";
    setDraft(text);
    pending.current = text;
    setStatus("saving");
    if (timer.current) clearTimeout(timer.current);
    timer.current = setTimeout(() => void flush(), SAVE_DELAY);
  };

  // Changed in another window: take it unless we have unsent edits.
  const body = note?.body;
  useEffect(() => {
    if (body === undefined || pending.current !== null || inFlight.current > 0) return;
    const ed = edRef.current;
    if (ed && ed.getValue() !== body) {
      const view = ed.saveViewState();
      ed.setValue(body);
      if (view) ed.restoreViewState(view);
    }
    setDraft(body);
  }, [body]);

  // Tab title follows the note's first line.
  const title = noteTitle(draft);
  useEffect(() => { useTabs.getState().update(tab.id, { title }); }, [title, tab.id]);

  // Unmounting (tab closed): send what's left. Empty notes are removed by the store.
  useEffect(() => () => { void flush(); }, [flush]);

  const update = async (patch: { projectId?: string | null; pinned?: boolean }) => {
    await flush();
    const cur = useNotes.getState().notes.find((n) => n.id === id);
    if (!cur) return;
    try { await useNotes.getState().save({ ...cur, ...patch }); } catch (e) { toast.error("Could not update the note", { description: errMsg(e) }); }
  };

  const remove = async () => {
    const ok = !draft.trim() || await confirmDialog({
      title: "Delete note?",
      message: <>"<b>{title}</b>" will be deleted. This can't be undone.</>,
      confirmLabel: "Delete",
      danger: true,
    });
    if (!ok) return;
    pending.current = null;
    if (timer.current) clearTimeout(timer.current);
    try {
      await useNotes.getState().remove(id);
      useTabs.getState().close(tab.id, true);
    } catch (e) {
      toast.error("Could not delete the note", { description: errMsg(e) });
    }
  };

  const onKeyDown = (e: React.KeyboardEvent) => {
    if ((e.metaKey || e.ctrlKey) && !e.altKey && e.key.toLowerCase() === "s") { e.preventDefault(); void flush(); }
    if ((e.metaKey || e.ctrlKey) && e.shiftKey && e.key.toLowerCase() === "v") { e.preventDefault(); setMode(mode === "edit" ? "preview" : "edit"); }
  };

  if (!note) {
    return (
      <div className="flex h-full flex-col items-center justify-center gap-3 text-sm text-muted">
        {loaded ? <>This note was deleted.<Button size="sm" onClick={() => useTabs.getState().close(tab.id, true)}>Close tab</Button></> : <Spinner />}
      </div>
    );
  }

  const project = projects.find((p) => p.id === note.projectId);
  return (
    <div className="flex h-full min-h-0 flex-col bg-app" onKeyDown={onKeyDown}>
      <Toolbar className="gap-2">
        <span className="size-2 shrink-0 rounded-full" style={{ background: project?.color ?? "var(--color-faint)" }} />
        <MiniSelect
          value={project ? project.id : ""}
          onChange={(e) => void update({ projectId: e.target.value || null })}
          title="Project this note belongs to (General notes show in every project)"
          className="max-w-48"
        >
          <option value="">General</option>
          {projects.map((p) => <option key={p.id} value={p.id}>{p.name}</option>)}
        </MiniSelect>
        <IconButton title={note.pinned ? "Unpin" : "Pin to the top"} active={note.pinned} onClick={() => void update({ pinned: !note.pinned })}>
          {note.pinned ? <PinOff className="size-3.5" /> : <Pin className="size-3.5" />}
        </IconButton>
        <div className="flex-1" />
        <SaveState status={status} error={saveError} updatedAt={note.updatedAt} />
        <div className="flex h-6 items-center rounded-md border border-line bg-app p-px text-[12px]" title="Toggle with ⇧⌘V">
          {([["edit", "Edit", Code], ["preview", "Preview", Eye]] as const).map(([v, label, I]) => (
            <button
              key={v}
              onClick={() => setMode(v)}
              className={clsx("inline-flex h-full items-center gap-1 rounded px-2", mode === v ? "bg-hover text-fg" : "text-muted hover:text-fg")}
            >
              <I className="size-3" /> {label}
            </button>
          ))}
        </div>
        <IconButton title="Share with a room" onClick={() => useShareToRoom.getState().share({ name: `${(title || "note").replace(/[\\/:*?"<>|]+/g, "-").slice(0, 80)}.md`, text: draft, folder: "notes", what: "this note" })}><Users className="size-3.5" /></IconButton>
        <IconButton title="Delete note" onClick={() => void remove()}><Trash2 className="size-3.5" /></IconButton>
      </Toolbar>
      {mode === "preview" && (
        <div className="min-h-0 flex-1 overflow-auto">
          {draft.trim()
            ? <MarkdownDoc text={draft} path="" root={null} />
            : <div className="p-8 text-sm text-faint italic">Empty note. Switch to Edit to write something.</div>}
        </div>
      )}
      {/* stays mounted in Preview so the cursor and undo history survive */}
      <div className={mode === "preview" ? "hidden" : "min-h-0 flex-1"}>
        <Editor
          theme="devcat"
          language="markdown"
          defaultValue={note.body}
          onMount={(ed) => { edRef.current = ed; if (!note.body) ed.focus(); }}
          onChange={onChange}
          options={{
            minimap: { enabled: false },
            fontSize: 13,
            lineNumbers: "off",
            glyphMargin: false,
            folding: false,
            lineDecorationsWidth: 12,
            wordWrap: "on",
            scrollBeyondLastLine: false,
            automaticLayout: true,
            renderLineHighlight: "none",
            quickSuggestions: false,
            wordBasedSuggestions: "off",
            padding: { top: 12, bottom: 12 },
            placeholder: "# Title\n\nWrite in Markdown…",
          }}
        />
      </div>
    </div>
  );
}

function SaveState({ status, error, updatedAt }: { status: string; error: string | null; updatedAt: number }) {
  if (status === "saving") return <span className="text-[11px] text-faint">Saving…</span>;
  if (status === "error") return <span className="inline-flex items-center gap-1 text-[11px] text-danger" title={error ?? undefined}><TriangleAlert className="size-3" />Not saved</span>;
  if (status === "saved") return <span className="inline-flex items-center gap-1 text-[11px] text-faint"><Check className="size-3" />Saved</span>;
  return <span className="text-[11px] text-faint" title={new Date(updatedAt).toLocaleString()}>Edited {relativeTime(new Date(updatedAt).toISOString())}</span>;
}
