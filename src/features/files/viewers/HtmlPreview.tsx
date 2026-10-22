// Rendered HTML in a sandboxed iframe (the built-in webview, no extra engine). Scripts are off
// unless the user turns them on; even then the frame has an opaque origin, so the page can't
// reach DevCat or the file system. Relative assets (css, images) aren't resolved: use
// "Open with default app" for the full page. A page that has scripts says so while they're off:
// pages built by JavaScript would otherwise just look empty.
import { ScrollText, X } from "lucide-react";
import { useMemo, useState } from "react";
import { Button, IconButton } from "../../../components/ui";

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

export function HtmlPreview({ html, scripts, setScripts, title }: { html: string; scripts: boolean; setScripts: (v: boolean) => void; title: string }) {
  const count = useMemo(() => countScripts(html), [html]);
  const [dismissed, setDismissed] = useState(false);
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
        title={title}
        srcDoc={html}
        sandbox={scripts ? "allow-scripts allow-popups allow-forms allow-modals" : "allow-popups"}
        className="min-h-0 w-full flex-1 border-0"
      />
    </div>
  );
}
