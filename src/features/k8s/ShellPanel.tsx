import { FitAddon } from "@xterm/addon-fit";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { RotateCw } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { Button, Select } from "../../components/ui";
import { errMsg } from "../../lib/api";
import { events, eventsReady, k8s } from "./api";
import PodSwitcher from "./PodSwitcher";
import { glassOn, onPaletteChange, palette } from "../appearance/store";
import { onFontsChange, terminalFont, terminalZoomKey } from "../appearance/fonts";
import { terminalTheme } from "../appearance/themes";
import { enableGpu } from "../../lib/xtermGpu";
import { terminalEditKey } from "../../lib/terminalKeys";

export interface ShellTarget { namespace: string; pod: string; containers: string[]; container?: string | null; owner?: string; pods?: string[] }

const SHELL = ["/bin/sh", "-c", "command -v bash >/dev/null 2>&1 && exec bash || exec sh"];

function b64ToBytes(s: string): Uint8Array {
  const bin = atob(s);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

export default function ShellPanel({ connId, target, visible, onPodChange }: { connId: string; target: ShellTarget; visible: boolean; onPodChange?: (pod: string) => void }) {
  const host = useRef<HTMLDivElement>(null);
  const termRef = useRef<{ term: Terminal; fit: FitAddon } | null>(null);
  const sessionRef = useRef<string | null>(null);
  const [container, setContainer] = useState(target.container ?? target.containers[0] ?? "");
  const [state, setState] = useState<"connecting" | "open" | "closed">("connecting");
  const [nonce, setNonce] = useState(0);

  useEffect(() => onPaletteChange((p, glass) => { if (termRef.current) termRef.current.term.options.theme = terminalTheme(p, glass); }), []);
  useEffect(() => onFontsChange((f) => {
    if (!termRef.current) return;
    Object.assign(termRef.current.term.options, terminalFont(f));
    try { termRef.current.fit.fit(); } catch { /* not attached yet */ }
  }), []);

  // Terminal instance lives for the panel's lifetime.
  useEffect(() => {
    const term = new Terminal({
      ...terminalFont(),
      cursorBlink: true,
      scrollback: 10000,
      allowProposedApi: true,
      allowTransparency: true,
      theme: terminalTheme(palette(), glassOn()),
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(host.current!);
    enableGpu(term);
    termRef.current = { term, fit };
    // ⌘/Ctrl + = / - / 0 (font size), copy / paste / clear / select all: the same keys as the AI workspace terminals.
    term.attachCustomKeyEventHandler((e) => e.type !== "keydown" || !(terminalZoomKey(e) || terminalEditKey(term, e)));
    const input = term.onData((d) => {
      if (sessionRef.current) k8s.execInput(sessionRef.current, d).catch(() => {});
    });
    const resize = term.onResize(({ cols, rows }) => {
      if (sessionRef.current) k8s.execResize(sessionRef.current, cols, rows).catch(() => {});
    });
    const ro = new ResizeObserver(() => {
      if (host.current && host.current.offsetWidth > 0 && host.current.offsetHeight > 0) {
        try { fit.fit(); } catch { /* not attached yet */ }
      }
    });
    ro.observe(host.current!);
    return () => { ro.disconnect(); input.dispose(); resize.dispose(); term.dispose(); termRef.current = null; };
  }, []);

  // Exec session (restarts on container change / reconnect).
  useEffect(() => {
    const t = termRef.current;
    if (!t) return;
    let cancelled = false;
    let offs: (() => void)[] = [];
    setState("connecting");
    t.term.reset();
    t.term.write(`\x1b[2mConnecting to ${target.pod}${container ? ` (${container})` : ""}…\x1b[0m\r\n`);
    (async () => {
      await eventsReady();
      try { t.fit.fit(); } catch { /* hidden */ }
      try {
        const id = await k8s.execStart(connId, target.namespace, target.pod, container || null, SHELL, t.term.cols, t.term.rows);
        if (cancelled) { k8s.execStop(id).catch(() => {}); return; }
        sessionRef.current = id;
        setState("open");
        offs = [
          events.execOutput.on(id, (o) => t.term.write(b64ToBytes(o.data))),
          events.execExit.on(id, (e) => {
            sessionRef.current = null;
            setState("closed");
            const why = e.error ?? e.status;
            t.term.write(`\r\n\x1b[2m[session closed${why && why !== "Success" ? `: ${why}` : ""}]\x1b[0m\r\n`);
          }),
        ];
        t.term.focus();
      } catch (e) {
        if (cancelled) return;
        setState("closed");
        t.term.write(`\x1b[31m${errMsg(e).replace(/\n/g, "\r\n")}\x1b[0m\r\n`);
      }
    })();
    return () => {
      cancelled = true;
      offs.forEach((f) => f());
      if (sessionRef.current) k8s.execStop(sessionRef.current).catch(() => {});
      sessionRef.current = null;
    };
  }, [connId, target.namespace, target.pod, container, nonce]);

  // Re-fit when the dock tab becomes visible.
  useEffect(() => {
    if (!visible) return;
    const t = termRef.current;
    requestAnimationFrame(() => {
      try { t?.fit.fit(); } catch { /* ignore */ }
      t?.term.focus();
    });
  }, [visible]);

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex h-8 shrink-0 items-center gap-2 border-b border-line px-2 text-xs">
        {target.pods && target.pods.length > 0 && onPodChange && (
          <PodSwitcher owner={target.owner} pods={target.pods} pod={target.pod} onChange={onPodChange} />
        )}
        {target.containers.length > 1 && (
          <Select className="h-6 w-auto max-w-48 text-xs" value={container} onChange={(e) => setContainer(e.target.value)}>
            {target.containers.map((c) => <option key={c} value={c}>{c}</option>)}
          </Select>
        )}
        <span className={state === "open" ? "text-ok" : state === "connecting" ? "text-warn" : "text-muted"}>
          {state === "open" ? "● connected" : state === "connecting" ? "connecting…" : "disconnected"}
        </span>
        <div className="flex-1" />
        {state === "closed" && <Button size="sm" onClick={() => setNonce((n) => n + 1)}><RotateCw className="size-3" />Reconnect</Button>}
      </div>
      <div className="min-h-0 flex-1 bg-app p-1.5">
        <div ref={host} className="h-full w-full" />
      </div>
    </div>
  );
}
