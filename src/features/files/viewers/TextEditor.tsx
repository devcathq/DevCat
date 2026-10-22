// Editable Monaco editor for kind "text" files (read-only with a reason banner when the backend
// says the file can't be edited). Save with ⌘S / Save button; conflicts with on-disk changes are
// detected via FilePreview.version and offer Overwrite / Reload. HTML files get a Preview / Source
// switch; the preview renders the editor's current (possibly unsaved) text.
import Editor, { type OnMount } from "@monaco-editor/react";
import clsx from "clsx";
import { AlertTriangle, Code, Eye, Lock, Save, Undo2 } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { Button } from "../../../components/ui";
import { api } from "../../../lib/api";
import type { FilePreview } from "../../../lib/types";
import { setCloseGuard, useTabs } from "../../../store/tabs";
import { confirmDialog } from "../../connections/dialogs";
import { basename } from "../format";
import { HtmlPreview, ScriptsButton, useHtmlScripts } from "./HtmlPreview";
import { WrapButton, useWrap } from "./simple";

type Ed = Parameters<OnMount>[0];
type Monaco = Parameters<OnMount>[1];

const CONFLICT = "CONFLICT:";

export function TextEditor({ preview, slot, tabId, hidden, reload, onDirty, onSaved }: {
  preview: FilePreview;
  slot: HTMLElement | null;
  tabId: string;
  /** blame is shown instead; stay mounted so edits survive */
  hidden: boolean;
  /** reload the preview from disk (the new one arrives via `preview`) */
  reload: () => void;
  onDirty: (dirty: boolean) => void;
  onSaved: () => void;
}) {
  const [wrap, setWrap] = useWrap();
  const [dirty, setDirty] = useState(false);
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [conflict, setConflict] = useState<string | null>(null);
  const edRef = useRef<Ed | null>(null);
  const monacoRef = useRef<Monaco | null>(null);
  /** version of the disk content the editor is based on */
  const baseVersion = useRef(preview.version ?? null);
  /** model alternative version id matching the saved / loaded content */
  const savedAlt = useRef(0);
  const dirtyRef = useRef(false);
  /** apply the next preview even when dirty (Revert / Reload after a conflict) */
  const forceNext = useRef(false);
  const readOnly = !preview.editable;
  const path = preview.path;
  const isHtml = /\.html?$/i.test(path);
  const [showPreview, setShowPreview] = useState(isHtml);
  const [scripts, setScripts] = useHtmlScripts();
  /** bumped when the editor's text is replaced from disk, so the preview re-reads it */
  const [textRev, setTextRev] = useState(0);
  const previewText = useMemo(
    () => (showPreview ? edRef.current?.getValue() ?? preview.text ?? "" : ""),
    [showPreview, textRev, preview.text], // eslint-disable-line react-hooks/exhaustive-deps
  );

  const markDirty = useCallback((d: boolean) => {
    if (dirtyRef.current === d) return;
    dirtyRef.current = d;
    setDirty(d);
    onDirty(d);
    useTabs.getState().update(tabId, { dirty: d });
  }, [onDirty, tabId]);

  const recompute = () => {
    const m = edRef.current?.getModel();
    if (m) markDirty(m.getAlternativeVersionId() !== savedAlt.current);
  };

  const applyEol = (ed: Ed, monaco: Monaco, eol: FilePreview["lineEnding"]) => {
    const m = ed.getModel();
    if (!m) return;
    m.setEOL(eol === "crlf" ? monaco.editor.EndOfLineSequence.CRLF : monaco.editor.EndOfLineSequence.LF);
    savedAlt.current = m.getAlternativeVersionId();
  };

  // New preview from disk (Refresh / focus / after save): take it unless the user has edits.
  useEffect(() => {
    const ed = edRef.current, monaco = monacoRef.current;
    if (!ed || !monaco) return;
    const force = forceNext.current;
    if (!force && (preview.version === baseVersion.current || dirtyRef.current)) return;
    forceNext.current = false;
    baseVersion.current = preview.version ?? null;
    const view = ed.saveViewState();
    ed.getModel()?.setValue(preview.text ?? "");
    applyEol(ed, monaco, preview.lineEnding);
    if (view) ed.restoreViewState(view);
    setTextRev((n) => n + 1);
    setConflict(null);
    setSaveError(null);
    markDirty(false);
  }, [preview, markDirty]);

  // Ask before closing the tab with unsaved edits.
  useEffect(() => {
    if (!dirty) return;
    return setCloseGuard(tabId, () => confirmDialog({
      title: "Unsaved changes",
      message: <>Discard your changes to <b>{basename(path)}</b>?</>,
      confirmLabel: "Discard & close",
      danger: true,
    }));
  }, [dirty, tabId, path]);
  useEffect(() => () => { useTabs.getState().update(tabId, { dirty: false }); }, [tabId]);

  const save = async (overwrite = false) => {
    const ed = edRef.current, m = ed?.getModel();
    if (!m || readOnly || saving) return;
    const content = m.getValue();
    const alt = m.getAlternativeVersionId();
    setSaving(true);
    try {
      baseVersion.current = await api.fsWriteText(path, content, overwrite ? null : baseVersion.current, preview.lineEnding ?? null);
      savedAlt.current = alt;
      setConflict(null);
      setSaveError(null);
      recompute();
      onSaved();
    } catch (e) {
      const msg = String(e);
      const i = msg.indexOf(CONFLICT);
      if (i >= 0) setConflict(msg.slice(i + CONFLICT.length).trim());
      else setSaveError(msg);
    } finally {
      setSaving(false);
    }
  };

  const revert = () => {
    forceNext.current = true;
    reload();
  };

  const onKeyDown = (e: KeyboardEvent) => {
    if ((e.metaKey || e.ctrlKey) && !e.altKey && e.key.toLowerCase() === "s") {
      e.preventDefault();
      e.stopPropagation();
      if (dirtyRef.current || conflict) void save();
    }
  };

  return (
    <div className={hidden ? "hidden" : "flex h-full min-h-0 flex-col"} onKeyDown={onKeyDown}>
      {slot && !hidden && createPortal(
        <>
          {readOnly
            ? <span className="inline-flex items-center gap-1 text-[11px] text-faint" title={preview.readonlyReason ?? undefined}><Lock className="size-3" />Read-only</span>
            : <>
              {dirty && <span className="text-[11px] text-warn">Modified</span>}
              {preview.lineEnding === "crlf" && <span className="font-mono text-[11px] text-faint" title="Line endings are kept as CRLF">CRLF</span>}
              <Button size="sm" variant="ghost" disabled={!dirty || saving} onClick={revert} title="Discard changes and reload from disk"><Undo2 className="size-3.5" />Revert</Button>
              <Button size="sm" variant={dirty ? "primary" : "default"} disabled={!dirty && !conflict} loading={saving} onClick={() => void save()} title="Save (⌘S)">
                {!saving && <Save className="size-3.5" />}Save
              </Button>
            </>}
          {/* after Save, so it doesn't shift when "Modified" appears */}
          {isHtml && (
            <div className="flex h-6 items-center rounded-md border border-line bg-app p-px text-[12px]">
              {([[true, "Preview", Eye], [false, "Source", Code]] as const).map(([v, label, I]) => (
                <button
                  key={label}
                  onClick={() => setShowPreview(v)}
                  className={clsx("inline-flex h-full items-center gap-1 rounded px-2", showPreview === v ? "bg-hover text-fg" : "text-muted hover:text-fg")}
                >
                  <I className="size-3" /> {label}
                </button>
              ))}
            </div>
          )}
          {showPreview ? <ScriptsButton on={scripts} set={setScripts} /> : <WrapButton wrap={wrap} setWrap={setWrap} />}
        </>,
        slot,
      )}
      {readOnly && preview.readonlyReason && <Banner>{preview.readonlyReason}</Banner>}
      {conflict && (
        <Banner tone="danger">
          <span className="min-w-0 flex-1 truncate" title={conflict}>{conflict} Saving would overwrite those changes.</span>
          <Button size="sm" variant="danger" onClick={() => void save(true)}>Overwrite</Button>
          <Button size="sm" onClick={revert}>Reload (discard my edits)</Button>
        </Banner>
      )}
      {!conflict && dirty && !!preview.version && preview.version !== baseVersion.current && (
        <Banner>
          <span className="min-w-0 flex-1 truncate">The file changed on disk while you were editing it.</span>
          <Button size="sm" onClick={revert}>Reload (discard my edits)</Button>
        </Banner>
      )}
      {saveError && (
        <Banner tone="danger">
          <span className="selectable min-w-0 flex-1 truncate" title={saveError}>Save failed: {saveError}</span>
          <Button size="sm" variant="ghost" onClick={() => setSaveError(null)}>Dismiss</Button>
        </Banner>
      )}
      {showPreview && <HtmlPreview html={previewText} scripts={scripts} setScripts={setScripts} title={basename(path)} />}
      {/* stays mounted while previewing so edits (and undo) survive */}
      <div className={showPreview ? "hidden" : "min-h-0 flex-1"}>
        <Editor
          theme="devcat"
          language={preview.language ?? "plaintext"}
          defaultValue={preview.text ?? ""}
          onMount={(ed, monaco) => {
            edRef.current = ed;
            monacoRef.current = monaco;
            applyEol(ed, monaco, preview.lineEnding);
          }}
          onChange={recompute}
          options={{
            readOnly,
            domReadOnly: readOnly,
            readOnlyMessage: preview.readonlyReason ? { value: preview.readonlyReason } : undefined,
            minimap: { enabled: false },
            fontSize: 12,
            scrollBeyondLastLine: false,
            automaticLayout: true,
            wordWrap: wrap ? "on" : "off",
            renderLineHighlight: readOnly ? "none" : "line",
            contextmenu: true,
            largeFileOptimizations: true,
          }}
        />
      </div>
    </div>
  );
}

function Banner({ children, tone = "warn" }: { children: ReactNode; tone?: "warn" | "danger" }) {
  return (
    <div className={tone === "danger"
      ? "flex shrink-0 items-center gap-2 border-b border-danger/25 bg-danger/10 px-3 py-1 text-[12px] text-danger"
      : "flex shrink-0 items-center gap-2 border-b border-warn/25 bg-warn/10 px-3 py-1 text-[12px] text-warn"}
    >
      <AlertTriangle className="size-3.5 shrink-0" /> {children}
    </div>
  );
}
