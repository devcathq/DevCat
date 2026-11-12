// Rendered HTML in a sandboxed iframe (the built-in webview, no extra engine). Scripts are off
// unless the user turns them on; even then the frame has an opaque origin, so the page can't
// reach DevCat or the file system. Relative assets (css, images, scripts) can't load in a srcdoc
// frame, so `inlineLocalAssets` swaps them for data: URLs first (local pages only). A page that has scripts says so while they're off:
// pages built by JavaScript would otherwise just look empty. Links open the file in a tab or the URL
// in the browser (`previewDoc`); `#` links scroll the page.
import { openUrl } from "@tauri-apps/plugin-opener";
import { ScrollText, X } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { toast } from "sonner";
import { Button, IconButton } from "../../../components/ui";
import { api } from "../../../lib/api";
import { resolvePath } from "../../claude/util";
import { dirname, openFilePreview } from "../format";
import { inlineAssets, type Load } from "./htmlAssets";

const SCRIPTS_KEY = "devcat.preview.htmlScripts";

export function useHtmlScripts(): [boolean, (v: boolean) => void] {
  const [on, setOn] = useState(() => localStorage.getItem(SCRIPTS_KEY) === "1");
  return [on, (v) => { setOn(v); localStorage.setItem(SCRIPTS_KEY, v ? "1" : "0"); }];
}

export function ScriptsButton({ on, set }: { on: boolean; set: (v: boolean) => void }) {
  return (
    <IconButton title={on ? "Scripts run in the preview (click to disable)" : "Run the page's scripts in the preview"} active={on} onClick={() => set(!on)}>
      <ScrollText className="size-3.5" />
    </IconButton>
  );
}

/** How many `<script>` elements the page has. */
export function countScripts(html: string): number {
  return html.match(/<script\b/gi)?.length ?? 0;
}

/**
 * A srcdoc frame resolves relative URLs against the *parent's* address (http://tauri.localhost/),
 * so `<a href="#why">` would navigate the frame to DevCat's own page (blank, scripts off) instead
 * of scrolling. `<base href="about:srcdoc">` makes `#why` a same-document jump. A page with its own
 * `<base href>` keeps it.
 *
 * Links to anything else (`../index.html`, `https://…`) can't load in the frame: it would go blank.
 * A small script of ours catches those clicks and hands the raw `href` to DevCat (`linkTarget`),
 * which opens the file in a tab or the URL in the browser. The frame always may run scripts for it,
 * so with the page's scripts off a CSP that only allows our nonce blocks the rest (`<script>`,
 * `onclick=…`, `javascript:` links). Our script keeps its token in a closure and removes itself
 * before the page's own scripts run, and it acts only on real clicks, so a page with scripts on
 * can't make DevCat open things by itself.
 */
export function previewDoc(html: string, { scripts, token }: { scripts: boolean; token: string }): string {
  const nonce = token.replace(/[^\w-]/g, "");
  const head =
    (scripts ? "" : `<meta http-equiv="Content-Security-Policy" content="script-src 'nonce-${nonce}'; object-src 'none'">`) +
    (/<base\b[^>]*\bhref\s*=/i.test(html) ? "" : '<base href="about:srcdoc">') +
    `<script nonce="${nonce}">(function(){var t=${JSON.stringify(token)},p=parent;document.currentScript.remove();` +
    `addEventListener("click",function(e){if(!e.isTrusted||e.defaultPrevented||e.button!==0)return;` +
    `var a=e.target&&e.target.closest?e.target.closest("a[href]"):null;if(!a)return;var h=(a.getAttribute("href")||"").trim();` +
    `if(!h||h.charAt(0)==="#"||/^javascript:/i.test(h))return;e.preventDefault();p.postMessage({devcatPreview:t,href:h},"*");});})();</script>`;
  // Right after the doctype (or first): before anything that could run, and a tag before the
  // doctype would put the page in quirks mode. The parser moves these into <head> by itself.
  const dt = html.match(/<!doctype\b[^>]*>/i);
  if (!dt || dt.index === undefined || /<script\b/i.test(html.slice(0, dt.index))) return head + html;
  const end = dt.index + dt[0].length;
  return html.slice(0, end) + head + html.slice(end);
}

/** What a clicked link in the preview of `file` opens: a URL in the browser, or a file (fragment and query dropped). */
export function linkTarget(file: string, href: string): { url: string } | { path: string } | null {
  if (/^(https?:|mailto:)/i.test(href)) return { url: href };
  if (href.startsWith("//")) return { url: `https:${href}` };
  const local = href.replace(/^file:\/\//i, "");
  if (/^[a-z][a-z0-9+.-]+:/i.test(local) && !/^[A-Za-z]:[\\/]/.test(local)) return null; // another scheme
  let rel = local.replace(/[#?].*$/, "");
  if (!rel) return null;
  try { rel = decodeURIComponent(rel); } catch { /* keep it as written */ }
  return { path: normalizePath(resolvePath(dirname(file), rel)) };
}

/** Collapse `.` and `..` segments, so `sessions/../index.html` is the same tab as `index.html`. */
function normalizePath(p: string): string {
  const m = p.match(/^([a-z][a-z0-9+.-]*:\/\/[^/]*|[A-Za-z]:)?([\s\S]*)$/i)!;
  const lead = m[1] ?? "", rest = m[2];
  const sep = rest.includes("\\") && !rest.includes("/") ? "\\" : "/";
  const abs = /^[\\/]/.test(rest);
  const out: string[] = [];
  for (const seg of rest.split(/[\\/]+/)) {
    if (!seg || seg === ".") continue;
    if (seg === ".." && out.length && out[out.length - 1] !== "..") out.pop();
    else if (seg !== ".." || !abs) out.push(seg);
  }
  return lead + (abs ? sep : "") + out.join(sep);
}

/** `inlineAssets` for the page `file` (a local path) shown from the tab's folder `root`. */
/** `inlineAssets` for the local page `file` shown from the tab's folder `root`; the files are read in batches (`fs_html_assets`). */
function inlineLocalAssets(html: string, file: string, root: string | null, scripts: boolean): Promise<string> {
  if (/^[a-z][a-z0-9+.-]+:\/\//i.test(file)) return Promise.resolve(html); // ssh:// and the like
  // a page with its own <base href> resolves its references elsewhere
  if (/<base\b[^>]*\bhref\s*=/i.test(html)) return Promise.resolve(html);
  type Batch = { refs: string[]; waiters: ((u: string | null) => void)[] };
  let batch: Batch | null = null;
  const load: Load = (ref) => new Promise((resolve) => {
    if (!batch) {
      const b: Batch = { refs: [], waiters: [] };
      batch = b;
      queueMicrotask(() => {
        batch = null;
        api.fsHtmlAssets(file, root, b.refs)
          .then((urls) => b.waiters.forEach((w, k) => w(urls[k] ?? null)))
          .catch(() => b.waiters.forEach((w) => w(null)));
      });
    }
    (batch as Batch).refs.push(ref);
    (batch as Batch).waiters.push(resolve);
  });
  return inlineAssets(html, scripts, load);
}

export function HtmlPreview({ html, scripts, setScripts, title, path, root }: {
  html: string;
  scripts: boolean;
  setScripts: (v: boolean) => void;
  title: string;
  /** the file shown, for its relative links */
  path: string;
  root: string | null;
}) {
  const count = useMemo(() => countScripts(html), [html]);
  // the page with its local css / images / scripts inlined; the raw page until they're read
  const [full, setFull] = useState<{ html: string; scripts: boolean; out: string } | null>(null);
  useEffect(() => {
    let live = true;
    inlineLocalAssets(html, path, root, scripts)
      .then((out) => { if (live) setFull({ html, scripts, out }); })
      .catch(() => { if (live) setFull({ html, scripts, out: html }); });
    return () => { live = false; };
  }, [html, path, root, scripts]);
  const ready = full && full.html === html && full.scripts === scripts ? full.out : null;
  const token = useMemo(() => crypto.randomUUID(), [ready, scripts]); // eslint-disable-line react-hooks/exhaustive-deps
  const doc = useMemo(() => (ready === null ? "" : previewDoc(ready, { scripts, token })), [ready, scripts, token]);
  const [dismissed, setDismissed] = useState(false);
  const frame = useRef<HTMLIFrameElement>(null);
  useEffect(() => {
    const onMessage = (e: MessageEvent) => {
      const d = e.data as { devcatPreview?: unknown; href?: unknown } | null;
      if (!frame.current || e.source !== frame.current.contentWindow || d?.devcatPreview !== token || typeof d.href !== "string") return;
      const to = linkTarget(path, d.href);
      if (!to) return;
      if ("url" in to) void openUrl(to.url).catch((err) => toast.error(String(err)));
      else openFilePreview(to.path, root);
    };
    window.addEventListener("message", onMessage);
    return () => window.removeEventListener("message", onMessage);
  }, [token, path, root]);
  return (
    <div className="flex min-h-0 flex-1 flex-col bg-white">
      {!scripts && count > 0 && !dismissed && (
        <div className="flex shrink-0 items-center gap-2 border-b border-line bg-panel px-3 py-1 text-[12px] text-muted">
          <ScrollText className="size-3.5 shrink-0 text-accent" />
          <span className="min-w-0 flex-1 truncate">
            This page uses JavaScript ({count} script{count === 1 ? "" : "s"}) and may look empty or broken without it. Scripts run sandboxed: they can't reach DevCat or your files.
          </span>
          <Button size="sm" variant="primary" onClick={() => setScripts(true)}>Run scripts</Button>
          <IconButton title="Hide this message" onClick={() => setDismissed(true)}><X className="size-3.5" /></IconButton>
        </div>
      )}
      <iframe
        // re-create the frame when the sandbox changes (browsers don't apply it to a loaded page)
        key={scripts ? "js" : "static"}
        ref={frame}
        title={title}
        srcDoc={doc}
        // scripts are always allowed for our link script; with the page's off, previewDoc's CSP blocks them
        sandbox={scripts ? "allow-scripts allow-popups allow-forms allow-modals" : "allow-scripts allow-popups"}
        className="min-h-0 w-full flex-1 border-0"
      />
    </div>
  );
}
