// Review tab: the reviewed page shows in a native webview placed over the page area (Rust:
// src-tauri/src/review); the inspector injected into it adds notes. The toolbar, the notes sidebar
// and Finish are ordinary DevCat UI. The page is hidden whenever a DevCat overlay would be behind
// it (overlay.ts), and the tab never toasts: a toast over the page would hide it too.
import clsx from "clsx";
import { ArrowLeft, Ellipsis, MousePointerClick, RotateCw, X } from "lucide-react";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { useCallback, useEffect, useRef, useState, type KeyboardEvent as ReactKeyboardEvent } from "react";
import { Button, Checkbox, Dialog, ErrorBox, IconButton, Spinner, Textarea, Toolbar } from "../../components/ui";
import { api, errMsg } from "../../lib/api";
import type { ReviewChanged, ReviewDoc, ReviewNote, ReviewPage, ReviewRendered } from "../../lib/types";
import type { Tab } from "../../tabs/types";
import { setCloseGuard, useTabs } from "../../store/tabs";
import { useConnections } from "../../store/connections";
import { displayPath, isRemote } from "../../lib/remotePath";
import { useFonts } from "../appearance/fonts";
import { copyText } from "../claude/util";
import { useContextMenu } from "../connections/ContextMenu";
import { confirmDialog } from "../connections/dialogs";
import { useNotes } from "../notes/store";
import { useOverlayCovers } from "./overlay";
import { nextPlace, type Place } from "./place";
import { openReview } from "./store";

type ReviewTabSpec = Extract<Tab, { type: "review" }>;

interface State {
  id: string;
  doc: ReviewDoc;
  missing: number[];
  saved: boolean;
  url: string;
  inspectable: boolean;
}

const hostOf = (url: string) => { try { return new URL(url).host || url; } catch { return url; } };

export default function ReviewTab({ tab }: { tab: ReviewTabSpec }) {
  const [st, setSt] = useState<State | null>(null);
  const [startError, setStartError] = useState<string | null>(null);
  const [status, setStatus] = useState<string | null>(null);
  const [inspect, setInspectOn] = useState(true);
  const [finishing, setFinishing] = useState(false);
  const [area, setArea] = useState({ w: 0, h: 0 });
  const idRef = useRef<string | null>(null);

  // Start the review once; end it when the tab goes (its window may have ended it already).
  useEffect(() => {
    let gone = false;
    const target = tab.path ? { path: tab.path } : { url: tab.url ?? "" };
    api.reviewStart(target, useConnections.getState().focusProjectId).then(
      (info) => {
        if (gone) { void api.reviewClose(info.id).catch(() => {}); return; }
        idRef.current = info.id;
        useTabs.getState().update(tab.id, { title: info.title });
        setSt({ id: info.id, doc: info.doc, missing: [], saved: false, url: "", inspectable: true });
      },
      (e) => { if (!gone) setStartError(errMsg(e)); },
    );
    return () => {
      gone = true;
      const id = idRef.current;
      idRef.current = null;
      if (id) void api.reviewClose(id).catch(() => {});
    };
  }, [tab.id, tab.path, tab.url]);

  // The review's events come to this window only; each tab takes its own.
  useEffect(() => {
    const win = getCurrentWebviewWindow();
    const offs = [
      win.listen<ReviewChanged>("review://changed", ({ payload: p }) => {
        if (p.id === idRef.current) setSt((s) => s && { ...s, doc: p.doc, missing: p.missing, saved: p.saved });
      }),
      win.listen<ReviewPage>("review://page", ({ payload: p }) => {
        if (p.id !== idRef.current) return;
        setSt((s) => s && { ...s, url: p.url, inspectable: p.inspectable });
        if (p.inspectable) setInspectOn(true); // every page load starts the inspector on
      }),
    ];
    return () => offs.forEach((off) => void off.then((f) => f()).catch(() => {}));
  }, []);

  // Unsaved notes: DevCat asks before the tab closes or moves (the notes don't move).
  const dirty = !!st && st.doc.notes.length > 0 && !st.saved;
  useEffect(() => {
    useTabs.getState().update(tab.id, { dirty });
    if (!dirty) return;
    return setCloseGuard(tab.id, () => confirmDialog({
      title: "Unsaved review",
      message: <>The review in <b>{tab.title}</b> has notes that aren't saved to Notes. Discard them?</>,
      confirmLabel: "Discard & close",
      danger: true,
    }));
  }, [dirty, tab.id, tab.title]);

  const run = useCallback((p: Promise<unknown>) => { p.then(() => setStatus(null), (e) => setStatus(errMsg(e))); }, []);
  // An address on another site starts a review of its own: in this tab while it has no notes,
  // else in a new tab, so the notes stay where they are.
  const goTo = async (cur: State, url: string) => {
    if (await api.reviewNav(cur.id, { url })) return;
    openReview({ url });
    if (cur.doc.notes.length === 0) useTabs.getState().close(tab.id);
  };
  const toggleInspect = () => {
    if (!st) return;
    const on = !inspect;
    setInspectOn(on);
    run(api.reviewInspect(st.id, on));
  };

  const menu = useContextMenu();
  const moreMenu = (e: React.MouseEvent<HTMLButtonElement>) => {
    const r = e.currentTarget.getBoundingClientRect();
    menu.show({ clientX: r.left, clientY: r.bottom + 2 }, [
      {
        label: "Clear review browsing data",
        onClick: async () => {
          const ok = await confirmDialog({
            title: "Clear review browsing data",
            message: "Sign-ins, cookies and site data that review pages kept are removed. Pages open in reviews may need a reload.",
            confirmLabel: "Clear",
            danger: true,
          });
          if (!ok) return;
          api.reviewClearData().then(() => setStatus("Review browsing data cleared"), (err) => setStatus(errMsg(err)));
        },
      },
    ]);
  };

  return (
    <div className="flex h-full flex-col">
      <Toolbar>
        <IconButton title="Back" disabled={!st} onClick={() => st && run(api.reviewNav(st.id, "back"))}><ArrowLeft className="size-3.5" /></IconButton>
        <IconButton title="Reload" disabled={!st} onClick={() => st && run(api.reviewNav(st.id, "reload"))}><RotateCw className="size-3.5" /></IconButton>
        <UrlField st={st} fallback={tab.url ?? tab.path ?? ""} onGo={(url) => st && run(goTo(st, url))} />
        {status && <span className="max-w-64 truncate text-[11px] text-muted" title={status}>{status}</span>}
        {st && !st.inspectable && st.url && <span className="shrink-0 text-[11px] text-warn">Inspect is off on {hostOf(st.url)}</span>}
        {area.w > 0 && <span className="shrink-0 px-1 font-mono text-[11px] text-faint" title="Size of the page area">{area.w} × {area.h}</span>}
        <Button
          size="sm"
          variant={inspect && st?.inspectable !== false ? "default" : "ghost"}
          className={clsx(inspect && st?.inspectable !== false && "text-accent")}
          disabled={!st || !st.inspectable}
          title="Click elements to comment (I in the page)"
          onClick={toggleInspect}
        >
          <MousePointerClick className="size-3.5" /> Inspect
        </Button>
        <IconButton title="More" onClick={moreMenu}><Ellipsis className="size-3.5" /></IconButton>
        <Button size="sm" variant="primary" disabled={!st || !st.doc.notes.length} onClick={() => setFinishing(true)}>Finish review</Button>
      </Toolbar>
      {menu.element}
      <div className="flex min-h-0 flex-1">
        <PageArea tabId={tab.id} id={st?.id ?? null} error={startError} onSize={setArea} />
        <Sidebar st={st} onError={setStatus} />
      </div>
      {st && finishing && <FinishDialog id={st.id} onClose={() => setFinishing(false)} />}
    </div>
  );
}

/** The page's address; a file review shows the file's path inside its folder instead of the file server's URL. */
function UrlField({ st, fallback, onGo }: { st: State | null; fallback: string; onGo: (url: string) => void }) {
  const file = st?.doc.target.kind === "file" ? st.doc.target : null;
  const shown = file ? filePath(file.root, st!.url, file.rel) : st?.url || fallback;
  const [draft, setDraft] = useState(shown);
  useEffect(() => setDraft(shown), [shown]);
  return (
    <input
      value={draft}
      readOnly={!!file}
      spellCheck={false}
      autoCorrect="off"
      autoCapitalize="off"
      onChange={(e) => setDraft(e.target.value)}
      onKeyDown={(e) => {
        if (e.key === "Enter" && !e.nativeEvent.isComposing && !file && draft.trim()) onGo(draft.trim());
        if (e.key === "Escape") setDraft(shown);
      }}
      title={shown}
      className="h-6 min-w-0 flex-1 rounded-md border border-line bg-app px-2 font-mono text-xs text-fg focus:border-accent focus:outline-none"
    />
  );
}

/** `<root>/<page>` for a file review: the page from the served URL (`/f/<id>/<rel>`), else the reviewed file. */
function filePath(root: string, url: string, rel: string): string {
  let page = rel;
  try {
    const m = new URL(url).pathname.match(/^\/f\/[^/]+\/(.*)$/);
    if (m) page = decodeURIComponent(m[1]);
  } catch { /* not loaded yet */ }
  const path = `${root.replace(/[\\/]$/, "")}/${page}`;
  // a file on an SSH host: `MacMini: ~/site/index.html`, as the file tree names it
  return isRemote(root) ? displayPath(path) : path;
}

/**
 * The empty area the native page is placed over. Its rect (CSS px of this webview, times the UI
 * zoom = logical px of the window) goes to `reviewPlace`, at most once a frame, one call at a time
 * so a late "show" can't overtake a "hide".
 */
function PageArea({ tabId, id, error, onSize }:
  { tabId: string; id: string | null; error: string | null; onSize: (s: { w: number; h: number }) => void }) {
  const ref = useRef<HTMLDivElement>(null);
  const active = useTabs((s) => s.activeId === tabId);
  const zoom = useFonts((s) => s.uiZoom);
  const covered = useOverlayCovers(ref, active && !!id);
  const [placed, setPlaced] = useState(false);
  const [size, setSize] = useState({ w: 0, h: 0 });
  const visible = active && !covered && size.w > 0 && size.h > 0;
  // the size the page sees (logical px); a hidden tab keeps showing its last one
  useEffect(() => {
    if (size.w > 0 && size.h > 0) onSize({ w: Math.round(size.w * zoom), h: Math.round(size.h * zoom) });
  }, [size, zoom, onSize]);

  const want = useRef<Place | null>(null);
  const sent = useRef<string>(""); // dedupe key of the last call; cleared when it failed, to retry
  const everSent = useRef(false); // a place went out (maybe failed): from then on every hide is sent
  const busy = useRef(false);
  const frame = useRef(0);

  const flush = useCallback(() => {
    frame.current = 0;
    const w = want.current;
    if (!w || busy.current) return;
    const key = JSON.stringify(w);
    if (key === sent.current) return;
    busy.current = true;
    sent.current = key;
    everSent.current = true;
    api.reviewPlace(w.id, w.x, w.y, w.w, w.h, w.visible).then(
      () => { if (w.visible) setPlaced(true); },
      () => { sent.current = ""; }, // the review ended (or the window is closing): nothing to show
    ).finally(() => {
      busy.current = false;
      if (want.current && JSON.stringify(want.current) !== sent.current) flush();
    });
  }, []);

  const measure = useCallback((now = false) => {
    const el = ref.current;
    if (!el || !id) return;
    const r = el.getBoundingClientRect();
    const vis = active && !covered && r.width > 0 && r.height > 0;
    // Nothing is created until the page can show; after that a hidden tab keeps its last place.
    want.current = nextPlace(want.current, vis ? { id, x: r.left * zoom, y: r.top * zoom, w: r.width * zoom, h: r.height * zoom } : null, everSent.current);
    if (!want.current) { if (frame.current) { cancelAnimationFrame(frame.current); frame.current = 0; } return; }
    if (now) { if (frame.current) cancelAnimationFrame(frame.current); flush(); }
    else if (!frame.current) frame.current = requestAnimationFrame(flush);
  }, [id, active, covered, zoom, flush]);

  // Hide right away (another tab or an overlay is showing); show and move at most once a frame.
  useEffect(() => { measure(!visible); }, [measure, visible]);

  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const ro = new ResizeObserver(() => {
      const r = el.getBoundingClientRect();
      setSize((s) => (s.w === r.width && s.h === r.height ? s : { w: r.width, h: r.height }));
      measure();
    });
    ro.observe(el);
    const onResize = () => measure();
    window.addEventListener("resize", onResize);
    return () => { ro.disconnect(); window.removeEventListener("resize", onResize); };
  }, [measure]);

  useEffect(() => () => { if (frame.current) cancelAnimationFrame(frame.current); }, []);

  return (
    <div ref={ref} className="relative min-w-0 flex-1 bg-app">
      {error ? (
        <ErrorBox error={error} />
      ) : !placed ? (
        <div className="flex h-full items-center justify-center"><Spinner /></div>
      ) : !visible ? (
        <div className="flex h-full items-center justify-center p-6 text-center text-xs text-muted">
          Page hidden while a DevCat menu or dialog is open
        </div>
      ) : null}
    </div>
  );
}

function Sidebar({ st, onError }: { st: State | null; onError: (e: string | null) => void }) {
  const notes = st?.doc.notes ?? [];
  return (
    <aside className="flex w-80 shrink-0 flex-col border-l border-line bg-panel">
      <div className="flex h-8 shrink-0 items-center border-b border-line px-3 text-xs font-semibold text-muted">
        Notes · {notes.length}
      </div>
      <div className="min-h-0 flex-1 overflow-auto" onMouseLeave={() => st && void api.reviewHighlight(st.id, null, false).catch(() => {})}>
        {!notes.length ? (
          <div className="p-4 text-xs text-muted">
            No notes yet. With Inspect on, click any element on the page to comment on it, like commenting on a PDF.
          </div>
        ) : (
          notes.map((n) => <NoteRow key={n.n} id={st!.id} note={n} missing={st!.missing.includes(n.n)} onError={onError} />)
        )}
      </div>
    </aside>
  );
}

function NoteRow({ id, note, missing, onError }: { id: string; note: ReviewNote; missing: boolean; onError: (e: string | null) => void }) {
  const [editing, setEditing] = useState<string | null>(null);
  const quiet = (p: Promise<unknown>) => void p.catch(() => {});
  const save = () => {
    const text = editing?.trim();
    setEditing(null);
    if (text && text !== note.comment) api.reviewEdit(id, note.n, text).then(() => onError(null), (e) => onError(errMsg(e)));
  };
  const keys = (e: ReactKeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) { e.preventDefault(); save(); }
    if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); setEditing(null); }
  };
  return (
    <div
      className="group flex gap-2 border-b border-line px-3 py-2 hover:bg-hover/50"
      onMouseEnter={() => quiet(api.reviewHighlight(id, note.n, false))}
      onClick={() => editing === null && quiet(api.reviewHighlight(id, note.n, true))}
      onDoubleClick={() => setEditing(note.comment)}
      title={`${note.page} · ${note.width}px wide`}
    >
      <span className="flex size-5 shrink-0 items-center justify-center rounded-full bg-accent text-[11px] font-semibold text-accent-fg">{note.n}</span>
      <div className="min-w-0 flex-1">
        <div className="truncate font-mono text-[11px] text-muted" title={note.selector}>{note.selector}</div>
        {editing !== null ? (
          <Textarea
            autoFocus
            value={editing}
            rows={3}
            className="mt-1 font-sans text-[13px]"
            onChange={(e) => setEditing(e.target.value)}
            onKeyDown={keys}
            onBlur={() => setEditing(null)}
            onClick={(e) => e.stopPropagation()}
            onDoubleClick={(e) => e.stopPropagation()}
          />
        ) : (
          <div className="whitespace-pre-wrap break-words text-[13px]">{note.comment}</div>
        )}
        {missing && <div className="mt-0.5 text-[11px] text-warn">not found on this page</div>}
      </div>
      <IconButton
        title="Remove note"
        className="opacity-0 group-hover:opacity-100"
        onClick={(e) => { e.stopPropagation(); api.reviewRemove(id, note.n).then(() => onError(null), (err) => onError(errMsg(err))); }}
      >
        <X className="size-3.5" />
      </IconButton>
    </div>
  );
}

/** The review as text, exactly what Save stores (the backend refuses to save text that changed since). */
function FinishDialog({ id, onClose }: { id: string; onClose: () => void }) {
  const [styles, setStyles] = useState(false);
  const [shown, setShown] = useState<ReviewRendered | null>(null);
  const [line, setLine] = useState<{ text: string; tone: "ok" | "warn" | "danger" } | null>(null);
  const [saving, setSaving] = useState(false);

  const render = useCallback(async (withStyles: boolean) => {
    try { setShown(await api.reviewRender(id, withStyles)); } catch (e) { setShown(null); setLine({ text: errMsg(e), tone: "danger" }); }
  }, [id]);
  useEffect(() => { void render(styles); }, [render, styles]);

  const save = async () => {
    if (!shown) return;
    setSaving(true);
    try {
      await api.reviewFinish(id, styles, shown.rev);
      void useNotes.getState().load();
      setLine({ text: "Saved to Notes", tone: "ok" });
    } catch (e) {
      const msg = errMsg(e);
      if (/review changed/i.test(msg)) {
        await render(styles);
        setLine({ text: "New notes arrived — check the text again", tone: "warn" });
      } else setLine({ text: msg, tone: "danger" });
    } finally {
      setSaving(false);
    }
  };

  return (
    <Dialog
      open
      onClose={onClose}
      title="Finish review"
      width={760}
      footer={
        <>
          {line && (
            <span className={clsx("mr-auto text-xs", line.tone === "ok" ? "text-ok" : line.tone === "warn" ? "text-warn" : "text-danger")}>{line.text}</span>
          )}
          <Button disabled={!shown} onClick={() => shown && void copyText(shown.text).then(() => setLine({ text: "Copied", tone: "ok" }))}>Copy review</Button>
          <Button variant="primary" loading={saving} disabled={!shown} onClick={() => void save()}>Save</Button>
        </>
      }
    >
      <div className="mb-2 flex items-center gap-3">
        <Checkbox label="Include styles" checked={styles} onChange={(v) => { setLine(null); setStyles(v); }} />
        {shown && <span className="text-xs text-faint">≈{Math.ceil(shown.text.length / 4)} tokens</span>}
      </div>
      {shown ? (
        <pre className="selectable max-h-[60vh] overflow-auto whitespace-pre-wrap break-words rounded-md border border-line bg-app p-3 font-mono text-xs">{shown.text}</pre>
      ) : (
        <div className="flex h-24 items-center justify-center"><Spinner /></div>
      )}
      <div className="mt-2 text-[11px] text-faint">
        Saved as one note in this project's Notes. Finishing again updates the same note.
      </div>
    </Dialog>
  );
}
