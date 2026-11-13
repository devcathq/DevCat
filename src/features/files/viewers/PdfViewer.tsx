// PDF preview with pdf.js: page canvases rendered lazily as they scroll into view.
import { getDocument, GlobalWorkerOptions, type PDFDocumentLoadingTask, type PDFDocumentProxy, type PDFPageProxy, type RenderTask } from "pdfjs-dist";
import workerUrl from "pdfjs-dist/build/pdf.worker.min.mjs?url";
import { MoveHorizontal, ZoomIn, ZoomOut } from "lucide-react";
import { memo, useCallback, useEffect, useRef, useState, type RefObject } from "react";
import { createPortal } from "react-dom";
import { ErrorBox, IconButton, Spinner } from "../../../components/ui";
import type { FilePreview } from "../../../lib/types";

GlobalWorkerOptions.workerSrc = workerUrl;

const GAP = 12;
const PAD = 16;

export default function PdfViewer({ preview, slot }: { preview: FilePreview; slot: HTMLElement | null }) {
  const [doc, setDoc] = useState<PDFDocumentProxy | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [base, setBase] = useState<{ w: number; h: number } | null>(null);
  const [scale, setScale] = useState<number | "fit">("fit");
  const [width, setWidth] = useState(0);
  const [current, setCurrent] = useState(1);
  const scrollRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    let live = true;
    let task: PDFDocumentLoadingTask | null = null;
    setDoc(null);
    setError(null);
    (async () => {
      // fetch() decodes the data URL natively — much faster than atob() for big files
      const buf = await (await fetch(`data:application/pdf;base64,${preview.dataBase64 ?? ""}`)).arrayBuffer();
      if (!live) return;
      task = getDocument({ data: new Uint8Array(buf) });
      const d = await task.promise;
      if (!live) return;
      const p1 = await d.getPage(1);
      const vp = p1.getViewport({ scale: 1 });
      setBase({ w: vp.width, h: vp.height });
      setDoc(d);
    })().catch((e) => live && setError(String(e?.message ?? e)));
    return () => { live = false; void task?.destroy(); };
  }, [preview]);

  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setWidth(el.clientWidth));
    ro.observe(el);
    return () => ro.disconnect();
  }, [doc]);

  const effScale = scale === "fit" ? (base && width ? Math.max(0.1, (width - PAD * 2 - 12) / base.w) : 1) : scale;
  const pageH = base ? base.h * effScale : 0;

  const onScroll = useCallback(() => {
    const el = scrollRef.current;
    if (!el || !pageH || !doc) return;
    const mid = el.scrollTop + el.clientHeight / 3;
    setCurrent(Math.min(doc.numPages, Math.max(1, Math.floor((mid - PAD) / (pageH + GAP)) + 1)));
  }, [pageH, doc]);

  const zoom = (f: number) => setScale(Math.min(6, Math.max(0.2, +(effScale * f).toFixed(3))));
  const goTo = (n: number) => {
    const el = scrollRef.current;
    if (!el || !doc) return;
    const p = Math.min(doc.numPages, Math.max(1, n));
    el.scrollTop = PAD + (p - 1) * (pageH + GAP);
  };

  if (error) return <ErrorBox error={`Couldn't render PDF: ${error}`} />;

  return (
    <div className="flex h-full min-h-0 flex-col">
      {slot && doc && createPortal(
        <>
          <span className="flex items-center gap-1 text-[11px] text-muted">
            <input
              key={current}
              defaultValue={current}
              onKeyDown={(e) => { if (e.key === "Enter") goTo(parseInt(e.currentTarget.value, 10) || 1); }}
              className="h-5 w-9 rounded border border-line bg-app text-center font-mono text-[11px] focus:border-accent focus:outline-none"
              aria-label="Page"
            />
            / {doc.numPages}
          </span>
          <IconButton title="Zoom out" onClick={() => zoom(1 / 1.2)}><ZoomOut className="size-3.5" /></IconButton>
          <span className="w-10 text-center font-mono text-[11px] text-muted">{Math.round(effScale * 100)}%</span>
          <IconButton title="Zoom in" onClick={() => zoom(1.2)}><ZoomIn className="size-3.5" /></IconButton>
          <IconButton title="Fit width" active={scale === "fit"} onClick={() => setScale("fit")}><MoveHorizontal className="size-3.5" /></IconButton>
        </>,
        slot,
      )}
      <div ref={scrollRef} onScroll={onScroll} className="min-h-0 flex-1 overflow-auto bg-app">
        {!doc || !base ? (
          <div className="flex h-full items-center justify-center"><Spinner /></div>
        ) : (
          <div className="flex w-max min-w-full flex-col items-center" style={{ padding: PAD, gap: GAP }}>
            {Array.from({ length: doc.numPages }, (_, i) => (
              <PdfPage key={i} doc={doc} n={i + 1} scale={effScale} estW={base.w * effScale} estH={pageH} root={scrollRef} />
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

const PdfPage = memo(function PdfPage({ doc, n, scale, estW, estH, root }: {
  doc: PDFDocumentProxy; n: number; scale: number; estW: number; estH: number; root: RefObject<HTMLDivElement | null>;
}) {
  const box = useRef<HTMLDivElement>(null);
  const canvas = useRef<HTMLCanvasElement>(null);
  const [visible, setVisible] = useState(false);
  const [size, setSize] = useState<{ w: number; h: number; s: number } | null>(null);
  const page = useRef<PDFPageProxy | null>(null);

  useEffect(() => {
    const el = box.current;
    if (!el) return;
    const io = new IntersectionObserver(([e]) => setVisible(e.isIntersecting), { root: root.current, rootMargin: "600px 0px" });
    io.observe(el);
    return () => io.disconnect();
  }, [root]);

  useEffect(() => {
    if (!visible) return;
    let task: RenderTask | null = null;
    let live = true;
    (async () => {
      page.current ??= await doc.getPage(n);
      if (!live) return;
      const p = page.current;
      const dpr = window.devicePixelRatio || 1;
      const vp = p.getViewport({ scale });
      const c = canvas.current;
      if (!c) return;
      c.width = Math.floor(vp.width * dpr);
      c.height = Math.floor(vp.height * dpr);
      c.style.width = `${Math.floor(vp.width)}px`;
      c.style.height = `${Math.floor(vp.height)}px`;
      setSize({ w: vp.width, h: vp.height, s: scale });
      task = p.render({ canvas: c, viewport: vp, transform: dpr !== 1 ? [dpr, 0, 0, dpr, 0, 0] : undefined });
      await task.promise;
    })().catch(() => { /* cancelled or failed page — leave blank */ });
    return () => { live = false; task?.cancel(); };
  }, [visible, doc, n, scale]);

  // Out of view (scrolled away, or its tab hidden): drop the page's bitmap. A page canvas at fit
  // width on a HiDPI screen is ~30 MB, so keeping every page drawn once grew a long PDF past 1 GB.
  useEffect(() => {
    if (visible) return;
    const c = canvas.current;
    if (c && c.width) { c.width = 0; c.height = 0; }
    page.current?.cleanup();
  }, [visible]);

  // Pages can differ in size: use the real one once known at this scale, else the first page's.
  const known = size?.s === scale ? size : null;
  const w = known?.w ?? estW;
  const h = known?.h ?? estH;
  return (
    <div ref={box} title={`Page ${n}`} className="relative shrink-0 bg-white shadow-lg shadow-black/40" style={{ width: w, height: h }}>
      <canvas ref={canvas} className="block" />
    </div>
  );
});
