// Markdown / image / docx / binary previews (editable text lives in TextEditor.tsx).
import Editor from "@monaco-editor/react";
import clsx from "clsx";
import { Code, Download, Eye, ExternalLink, FileQuestion, Maximize, Minimize, WrapText, ZoomIn, ZoomOut } from "lucide-react";
import { useEffect, useMemo, useState, type CSSProperties } from "react";
import { createPortal } from "react-dom";
import { Button, IconButton } from "../../../components/ui";
import type { FilePreview } from "../../../lib/types";
import { fmtSize, openExternally } from "../format";
import { downloadRemoteFile } from "../download";
import { MarkdownDoc } from "./MarkdownDoc";
import { TruncatedBanner } from "./SheetViewer";
import { isRemote } from "../../../lib/remotePath";

type ViewerProps = { preview: FilePreview; slot: HTMLElement | null; root: string | null };

const CHECKER: CSSProperties = {
  backgroundColor: "var(--color-hover)",
  backgroundImage: "linear-gradient(45deg,var(--color-raised) 25%,transparent 25%),linear-gradient(-45deg,var(--color-raised) 25%,transparent 25%),linear-gradient(45deg,transparent 75%,var(--color-raised) 75%),linear-gradient(-45deg,transparent 75%,var(--color-raised) 75%)",
  backgroundSize: "16px 16px",
  backgroundPosition: "0 0,0 8px,8px -8px,-8px 0",
};

const WRAP_KEY = "devcat.preview.wrap";

function Code_({ text, language, wrap }: { text: string; language: string; wrap: boolean }) {
  return (
    <Editor
      theme="devcat"
      language={language}
      value={text}
      options={{
        readOnly: true,
        domReadOnly: true,
        minimap: { enabled: false },
        fontSize: 12,
        scrollBeyondLastLine: false,
        automaticLayout: true,
        wordWrap: wrap ? "on" : "off",
        renderLineHighlight: "none",
        contextmenu: true,
        largeFileOptimizations: true,
      }}
    />
  );
}

export function useWrap(): [boolean, (v: boolean) => void] {
  const [wrap, setWrap] = useState(() => localStorage.getItem(WRAP_KEY) === "1");
  return [wrap, (v) => { setWrap(v); localStorage.setItem(WRAP_KEY, v ? "1" : "0"); }];
}

export function WrapButton({ wrap, setWrap }: { wrap: boolean; setWrap: (v: boolean) => void }) {
  return <IconButton title={wrap ? "Disable word wrap" : "Word wrap"} active={wrap} onClick={() => setWrap(!wrap)}><WrapText className="size-3.5" /></IconButton>;
}

export function MarkdownViewer({ preview, slot, root }: ViewerProps) {
  const [source, setSource] = useState(false);
  const [wrap, setWrap] = useWrap();
  return (
    <div className="flex h-full min-h-0 flex-col">
      {slot && createPortal(
        <>
          <div className="flex h-6 items-center rounded-md border border-line bg-app p-px text-[12px]">
            {([[false, "Preview", Eye], [true, "Source", Code]] as const).map(([v, label, I]) => (
              <button
                key={label}
                onClick={() => setSource(v)}
                className={clsx("inline-flex h-full items-center gap-1 rounded px-2", source === v ? "bg-hover text-fg" : "text-muted hover:text-fg")}
              >
                <I className="size-3" /> {label}
              </button>
            ))}
          </div>
          {source && <WrapButton wrap={wrap} setWrap={setWrap} />}
        </>,
        slot,
      )}
      {preview.truncated && <TruncatedBanner text="The file is large; only its beginning is shown." />}
      <div className="min-h-0 flex-1">
        {source
          ? <Code_ text={preview.text ?? ""} language="markdown" wrap={wrap} />
          : <MarkdownDoc text={preview.text ?? ""} path={preview.path} root={root} />}
      </div>
    </div>
  );
}

export function ImageViewer({ preview, slot }: ViewerProps) {
  const [fit, setFit] = useState(true);
  const [zoom, setZoom] = useState(1);
  const [dims, setDims] = useState<{ w: number; h: number } | null>(null);
  const src = useMemo(() => `data:${preview.mime ?? "image/png"};base64,${preview.dataBase64 ?? ""}`, [preview]);
  useEffect(() => { setDims(null); }, [src]);
  const step = (dir: 1 | -1) => { setFit(false); setZoom((z) => Math.min(16, Math.max(0.05, +(dir > 0 ? z * 1.25 : z / 1.25).toFixed(3)))); };

  return (
    <div className="flex h-full min-h-0 flex-col">
      {slot && createPortal(
        <>
          {dims && <span className="px-1 font-mono text-[11px] text-faint">{dims.w} × {dims.h}</span>}
          <IconButton title="Zoom out" onClick={() => step(-1)}><ZoomOut className="size-3.5" /></IconButton>
          <span className="w-10 text-center font-mono text-[11px] text-muted">{fit ? "fit" : `${Math.round(zoom * 100)}%`}</span>
          <IconButton title="Zoom in" onClick={() => step(1)}><ZoomIn className="size-3.5" /></IconButton>
          <IconButton title={fit ? "Actual size" : "Fit to window"} onClick={() => { setFit(!fit); setZoom(1); }}>
            {fit ? <Maximize className="size-3.5" /> : <Minimize className="size-3.5" />}
          </IconButton>
        </>,
        slot,
      )}
      <div
        className="min-h-0 flex-1 overflow-auto"
        onWheel={(e) => { if (e.ctrlKey || e.metaKey) { e.preventDefault(); step(e.deltaY < 0 ? 1 : -1); } }}
      >
        <div className={clsx("flex min-h-full min-w-full p-4", fit ? "h-full items-center justify-center" : "w-max items-start")}>
          <img
            src={src}
            alt={preview.path}
            draggable={false}
            onLoad={(e) => setDims({ w: e.currentTarget.naturalWidth, h: e.currentTarget.naturalHeight })}
            className={clsx("m-auto block shadow-lg", fit && "max-h-full max-w-full object-contain")}
            style={fit ? CHECKER : { ...CHECKER, width: dims ? dims.w * zoom : undefined, maxWidth: "none", imageRendering: zoom >= 3 ? "pixelated" : undefined }}
          />
        </div>
      </div>
    </div>
  );
}

export function DocxViewer({ preview }: ViewerProps) {
  const paragraphs = useMemo(() => (preview.text ?? "").split(/\n{1,}/), [preview.text]);
  return (
    <div className="flex h-full min-h-0 flex-col">
      {preview.truncated && <TruncatedBanner text="The document is long; only its beginning is shown." />}
      <div className="min-h-0 flex-1 overflow-auto bg-app py-8">
        <div className="selectable mx-auto max-w-[760px] rounded-sm border border-line bg-raised px-14 py-12 font-serif text-[14px] leading-relaxed text-fg/90 shadow-xl">
          {paragraphs.every((p) => !p.trim())
            ? <div className="text-center text-faint italic">No text content</div>
            : paragraphs.map((p, i) => (p.trim() ? <p key={i} className="mb-3 whitespace-pre-wrap">{p}</p> : null))}
        </div>
      </div>
    </div>
  );
}

export function BinaryCard({ preview }: ViewerProps) {
  const tooLarge = preview.kind === "too_large";
  return (
    <div className="flex h-full items-center justify-center p-6">
      <div className="flex w-80 flex-col items-center gap-2 rounded-lg border border-line bg-panel p-6 text-center">
        <FileQuestion className="size-9 text-faint" />
        <div className="font-medium">{tooLarge ? "File too large to preview" : "No preview available"}</div>
        <div className="text-xs text-muted">
          {fmtSize(preview.size)}{preview.mime ? ` · ${preview.mime}` : ""}
        </div>
        {isRemote(preview.path)
          ? <Button className="mt-2" onClick={() => void downloadRemoteFile(preview.path)}><Download className="size-3.5" /> Download</Button>
          : <Button className="mt-2" onClick={() => void openExternally(preview.path)}><ExternalLink className="size-3.5" /> Open externally</Button>}
      </div>
    </div>
  );
}
