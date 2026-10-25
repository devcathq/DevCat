// File preview tab: toolbar (path, size, kind, actions) + a viewer per FilePreview kind,
// or git blame for text files inside a repository. Plain text files (code, config, …) open in an
// editor (.html also gets a rendered Preview); markdown, pdf, images, sheets, csv and docx stay preview-only.
import { Copy, ExternalLink, Files, FolderSearch, History, RefreshCw, UserSearch } from "lucide-react";
import { lazy, Suspense, useCallback, useEffect, useRef, useState } from "react";
import { Badge, ErrorBox, IconButton, Spinner, Toolbar } from "../../components/ui";
import { api } from "../../lib/api";
import type { FilePreview } from "../../lib/types";
import { useTabs } from "../../store/tabs";
import type { Tab } from "../../tabs/types";
import { BlameView } from "../git/BlameView";
import { openGitRepo } from "../git/open";
import { useRepoRoot } from "../git/repoOf";
import { FileIcon } from "./FileIcon";
import { copyFile, copyText, fmtSize, openExternally, relPath, revealPath, REVEAL_LABEL } from "./format";
import { CsvViewer, SheetViewer } from "./viewers/SheetViewer";
import { BinaryCard, DocxViewer, ImageViewer, MarkdownViewer } from "./viewers/simple";
import { TextEditor } from "./viewers/TextEditor";
import { isRemote } from "../../lib/remotePath";

const PdfViewer = lazy(() => import("./viewers/PdfViewer"));

const KIND_LABEL: Record<FilePreview["kind"], string> = {
  text: "Text", markdown: "Markdown", image: "Image", pdf: "PDF", sheet: "Spreadsheet", csv: "CSV", docx: "Word", binary: "Binary", too_large: "Too large",
};

export default function FilePreviewTab({ tab }: { tab: Extract<Tab, { type: "file" }> }) {
  const [preview, setPreview] = useState<FilePreview | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [slot, setSlot] = useState<HTMLDivElement | null>(null);
  const [reload, setReload] = useState(0);
  const repo = useRepoRoot(tab.path);
  const dirty = useRef(false);
  const loadSeq = useRef(0);
  /** The file's change stamp when it was last loaded (null: it wasn't there; undefined: not known). */
  const loadedStamp = useRef<string | null | undefined>(undefined);
  const remote = isRemote(tab.path);

  const load = useCallback(async () => {
    // only the latest load applies (e.g. one started before a save must not undo it)
    const seq = ++loadSeq.current;
    setLoading(true);
    // taken before reading, so a change made meanwhile still shows as one
    const stamp = remote ? undefined : await api.fsStamp(tab.path).catch(() => undefined);
    try {
      const p = await api.fsPreview(tab.path);
      if (seq !== loadSeq.current) return;
      setPreview(p);
      setError(null);
    } catch (e) {
      if (seq === loadSeq.current) setError(String(e));
    } finally {
      if (seq === loadSeq.current) {
        setLoading(false);
        loadedStamp.current = stamp;
      }
    }
  }, [tab.path, remote]);

  useEffect(() => { void load(); }, [load]);

  // An agent often names a file before it has written it, and rewrites an image or a PDF while it's
  // open: while this tab shows, look every second, and load the file again once a change has
  // settled (the same stamp twice). Text waits for the window's focus (below): it may be edited here.
  const active = useTabs((s) => s.activeId === tab.id);
  const watch = active && !remote && preview?.kind !== "text";
  useEffect(() => {
    if (!watch) return;
    let pending: string | null | undefined;
    const tick = window.setInterval(() => {
      if (document.hidden || loadedStamp.current === undefined) return;
      void api.fsStamp(tab.path).then((now) => {
        if (now === loadedStamp.current) pending = undefined;
        else if (now === pending) {
          pending = undefined;
          void load();
        } else pending = now;
      }, () => {});
    }, 1000);
    return () => window.clearInterval(tick);
  }, [watch, load, tab.path]);

  // Pick up changes made outside DevCat when the window regains focus (text files without edits).
  const isText = preview?.kind === "text";
  useEffect(() => {
    if (!isText) return;
    const onFocus = () => { if (!dirty.current && useTabs.getState().activeId === tab.id) void load(); };
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, [isText, load, tab.id]);
  const onDirty = useCallback((d: boolean) => { dirty.current = d; }, []);

  const shown = relPath(tab.path, tab.root);
  const kind = preview?.kind;
  const canBlame = !!repo && !isRemote(tab.path) && (kind === "text" || kind === "markdown");
  const blame = !!tab.blame && repo !== null && (!preview || canBlame);
  const setBlame = (v: boolean) => useTabs.getState().update(tab.id, { blame: v } as never);

  return (
    <div className="flex h-full min-h-0 flex-col bg-app">
      <Toolbar className="gap-2">
        <FileIcon name={tab.path} />
        {/* rtl keeps the file name visible when a long path gets truncated */}
        <span className="selectable min-w-0 truncate font-mono text-[12px] text-fg/90" title={tab.path} dir="rtl">
          <bdi>{shown}</bdi>
        </span>
        {preview && <span className="shrink-0 text-[11px] text-faint">{fmtSize(preview.size)}</span>}
        {kind && <Badge tone={kind === "binary" || kind === "too_large" ? "warn" : "default"}>{KIND_LABEL[kind] ?? kind}</Badge>}
        {preview?.language && kind === "text" && <span className="shrink-0 font-mono text-[11px] text-faint">{preview.language}</span>}
        <div className="flex-1" />
        <div ref={setSlot} className="flex shrink-0 items-center gap-1.5" />
        <div className="mx-0.5 h-4 w-px shrink-0 bg-line" />
        {canBlame && (
          <>
            <IconButton title={blame ? "Hide blame" : "Blame"} active={blame} onClick={() => setBlame(!blame)}><UserSearch className="size-3.5" /></IconButton>
            <IconButton title="File history" onClick={() => openGitRepo(repo, { path: tab.path })}><History className="size-3.5" /></IconButton>
          </>
        )}
        <IconButton title="Refresh" onClick={() => { void load(); setReload((n) => n + 1); }}><RefreshCw className={loading ? "size-3.5 animate-spin" : "size-3.5"} /></IconButton>
        <IconButton title="Copy path" onClick={() => void copyText(tab.path)}><Copy className="size-3.5" /></IconButton>
        {!isRemote(tab.path) && (
          <>
            <IconButton title="Copy file (paste in Slack, Explorer, Finder)" onClick={() => void copyFile(tab.path)}><Files className="size-3.5" /></IconButton>
            <IconButton title="Open with default app" onClick={() => void openExternally(tab.path)}><ExternalLink className="size-3.5" /></IconButton>
            <IconButton title={REVEAL_LABEL} onClick={() => void revealPath(tab.path)}><FolderSearch className="size-3.5" /></IconButton>
          </>
        )}
      </Toolbar>
      {/* a failed refresh keeps the last content (and any unsaved edits) on screen */}
      {error && preview && <div className="selectable shrink-0 truncate border-b border-danger/25 bg-danger/10 px-3 py-1 text-[12px] text-danger" title={error}>{error}</div>}
      <div className="flex min-h-0 flex-1 flex-col">
        {error && !preview ? (
          <ErrorBox error={error} />
        ) : !preview ? (
          <div className="flex h-full items-center justify-center"><Spinner /></div>
        ) : (
          <>
            {/* the editor stays mounted under blame so edits survive toggling it */}
            {preview.kind === "text" && (
              <TextEditor
                key={tab.path}
                preview={preview}
                slot={slot}
                tabId={tab.id}
                hidden={blame}
                reload={() => void load()}
                onDirty={onDirty}
                onSaved={() => { void load(); setReload((n) => n + 1); }}
              />
            )}
            {blame ? (
              repo ? <BlameView key={tab.path} path={tab.path} repo={repo} slot={slot} reload={reload} /> : <div className="flex h-full items-center justify-center"><Spinner /></div>
            ) : preview.kind !== "text" && (
              <Viewer key={preview.kind} preview={preview} slot={slot} root={tab.root ?? null} />
            )}
          </>
        )}
      </div>
    </div>
  );
}

function Viewer({ preview, slot, root }: { preview: FilePreview; slot: HTMLElement | null; root: string | null }) {
  const props = { preview, slot, root };
  switch (preview.kind) {
    case "markdown": return <MarkdownViewer {...props} />;
    case "image": return <ImageViewer {...props} />;
    case "pdf": return <Suspense fallback={<div className="flex h-full items-center justify-center"><Spinner /></div>}><PdfViewer preview={preview} slot={slot} /></Suspense>;
    case "sheet": return <SheetViewer preview={preview} slot={slot} />;
    case "csv": return <CsvViewer preview={preview} slot={slot} />;
    case "docx": return <DocxViewer {...props} />;
    default: return <BinaryCard {...props} />;
  }
}
