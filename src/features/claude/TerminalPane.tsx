// xterm.js terminal bound to a backend PTY (iTerm2-like): DevCat palette, ⌘C/⌘V/⌘K,
// Option-as-Meta, clickable URLs and file paths, restart on Enter after the process exits.
// The PTY lives as long as the component (tabs stay mounted) and restarts when `startKey` changes.
import { openUrl } from "@tauri-apps/plugin-opener";
import { FitAddon } from "@xterm/addon-fit";
import { Unicode11Addon } from "@xterm/addon-unicode11";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { Terminal, type ILink } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { useEffect, useRef, useState } from "react";
import { ActivityDetector } from "./activity";
import { api, errMsg } from "../../lib/api";
import { ptyEventsReady, subscribePty } from "./pty";
import { openFile } from "./util";
import { glassOn, onPaletteChange, palette } from "../appearance/store";
import { onFontsChange, terminalFont, terminalZoomKey } from "../appearance/fonts";
import { terminalTheme } from "../appearance/themes";
import { enableGpu } from "../../lib/xtermGpu";
import { IS_MAC, terminalEditKey } from "../../lib/terminalKeys";
import { droppedPaths, isFileDrag, shellQuote } from "../files/drop";


const CODE_EXT = new Set(("ts tsx js jsx mjs cjs json md mdx css scss html rs go py rb java kt swift c h cpp hpp cs php sh zsh bash yml yaml toml " +
  "ini sql lua dart scala vue svelte txt lock xml gradle env conf proto graphql").split(" "));
// path(:line(:col)?)? preceded by start / whitespace / quote / bracket
const PATH_RE = /(^|[\s'"`(\[{<])((?:~|\.{1,2})?\/?(?:[\w.@+-]+\/)*[\w@+-][\w.@+-]*\.([A-Za-z][\w]{0,7}))(?::(\d+)(?::(\d+))?)?/g;

export interface TerminalPaneProps {
  cwd: string;
  /** null = the user's login shell. */
  program: string | null;
  /** AI connection to run instead of `program` (the backend resolves its command, env and API key). */
  providerId?: string | null;
  /** Args for each (re)start. */
  getArgs?: () => string[] | Promise<string[]>;
  /** Change to kill and restart the process. */
  startKey?: number;
  visible: boolean;
  /** Process name used in the exit line. */
  name: string;
  onPty?: (id: string | null) => void;
  /** Called before restarting after an exit (e.g. to pick the session to resume). */
  beforeRestart?: () => Promise<void> | void;
  /** Map Shift+Enter to ESC CR (newline in claude's prompt). */
  shiftEnterNewline?: boolean;
  /** Track whether the program is working (AI agent tabs): key in the activity store. */
  activityId?: string;
}

export function TerminalPane({ cwd, program, providerId, getArgs, startKey = 0, visible, name, onPty, beforeRestart, shiftEnterNewline, activityId }: TerminalPaneProps) {
  const host = useRef<HTMLDivElement>(null);
  const termRef = useRef<{ term: Terminal; fit: FitAddon } | null>(null);
  const ptyRef = useRef<string | null>(null);
  const exitedRef = useRef(false);
  const activity = useRef<ActivityDetector | null>(null);
  if (activityId && !activity.current) activity.current = new ActivityDetector(activityId);
  useEffect(() => () => activity.current?.dispose(), []);
  /** The next start was requested with Enter after an exit. */
  const enterRestart = useRef(false);
  const [restartNonce, setRestartNonce] = useNonce();
  // Latest callbacks without re-running effects.
  const cb = useRef({ getArgs, onPty, beforeRestart, cwd });
  cb.current = { getArgs, onPty, beforeRestart, cwd };

  // Colors follow the Appearance theme.
  useEffect(() => onPaletteChange((p, glass) => { if (termRef.current) termRef.current.term.options.theme = terminalTheme(p, glass); }), []);
  // Fonts follow the Appearance font settings.
  useEffect(() => onFontsChange((f) => {
    if (!termRef.current) return;
    Object.assign(termRef.current.term.options, terminalFont(f));
    try { termRef.current.fit.fit(); } catch { /* not attached yet */ }
  }), []);

  // Terminal instance lives for the component's lifetime.
  useEffect(() => {
    const term = new Terminal({
      ...terminalFont(),
      cursorBlink: true,
      cursorStyle: "block",
      scrollback: 10000,
      allowProposedApi: true,
      macOptionIsMeta: true,
      macOptionClickForcesSelection: true,
      rightClickSelectsWord: IS_MAC,
      drawBoldTextInBrightColors: false,
      allowTransparency: true,
      theme: terminalTheme(palette(), glassOn()),
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.loadAddon(new WebLinksAddon((_e, uri) => void openUrl(uri).catch(() => {})));
    const uni = new Unicode11Addon();
    term.loadAddon(uni);
    term.unicode.activeVersion = "11";
    term.open(host.current!);
    enableGpu(term);
    termRef.current = { term, fit };

    term.attachCustomKeyEventHandler((e) => {
      if (e.type !== "keydown") return true;
      // App shortcuts (⌥⇧⌘1/2/3 view switch) belong to the app.
      if (e.altKey && e.shiftKey && (e.metaKey || e.ctrlKey) && /^Digit\d$/.test(e.code)) return false;
      // ⌘/Ctrl + = / - / 0: terminal font size (every terminal; same setting as Appearance → Fonts).
      if (terminalZoomKey(e)) return false;
      if (terminalEditKey(term, e)) return false;
      // Shift+Enter → newline in claude's prompt (same as iTerm2 after /terminal-setup).
      if (shiftEnterNewline && e.key === "Enter" && e.shiftKey && !e.metaKey && !e.ctrlKey && !e.altKey) {
        e.preventDefault();
        if (ptyRef.current) void api.ptyWrite(ptyRef.current, "\x1b\r").catch(() => {});
        return false;
      }
      // Other ⌘ combos are app shortcuts (⌘W, ⌘T, …).
      if (IS_MAC && e.metaKey) return false;
      return true;
    });

    // File paths (src/foo.ts:12) → file preview tab.
    const links = term.registerLinkProvider({
      provideLinks(y, done) {
        const line = term.buffer.active.getLine(y - 1)?.translateToString(true) ?? "";
        const out: ILink[] = [];
        PATH_RE.lastIndex = 0;
        let m: RegExpExecArray | null;
        while ((m = PATH_RE.exec(line))) {
          const [, pre, path, ext] = m;
          if (!path.includes("/") && !CODE_EXT.has(ext.toLowerCase())) continue;
          const start = m.index + pre.length;
          const text = m[0].slice(pre.length);
          out.push({
            range: { start: { x: start + 1, y }, end: { x: start + text.length, y } },
            text,
            decorations: { underline: true, pointerCursor: true },
            activate: () => openFile(cb.current.cwd, path),
          });
        }
        done(out.length ? out : undefined);
      },
    });

    const title = term.onTitleChange((t) => activity.current?.title(t));
    const bell = term.onBell(() => activity.current?.bell());
    const input = term.onData((d) => {
      activity.current?.input();
      if (exitedRef.current) {
        if (d === "\r") {
          enterRestart.current = true;
          setRestartNonce();
        }
        return;
      }
      if (ptyRef.current) void api.ptyWrite(ptyRef.current, d).catch(() => {});
    });
    const resize = term.onResize(({ cols, rows }) => {
      activity.current?.resize();
      if (ptyRef.current) void api.ptyResize(ptyRef.current, cols, rows).catch(() => {});
    });
    const ro = new ResizeObserver(() => {
      if (host.current && host.current.offsetWidth > 0 && host.current.offsetHeight > 0) {
        try { fit.fit(); } catch { /* not attached yet */ }
      }
    });
    ro.observe(host.current!);
    return () => {
      ro.disconnect();
      links.dispose();
      input.dispose();
      title.dispose();
      bell.dispose();
      resize.dispose();
      term.dispose();
      termRef.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Process lifecycle: (re)start on startKey / restart after exit; close on unmount.
  useEffect(() => {
    const t = termRef.current;
    if (!t) return;
    let cancelled = false;
    let off: (() => void) | null = null;
    let myId: string | null = null;
    const isRestart = enterRestart.current;
    enterRestart.current = false;
    exitedRef.current = false;
    (async () => {
      if (isRestart) await cb.current.beforeRestart?.();
      await ptyEventsReady();
      if (cancelled) return;
      try { t.fit.fit(); } catch { /* hidden */ }
      try {
        const args = (await cb.current.getArgs?.()) ?? [];
        if (cancelled) return;
        const id = await api.ptyOpen(cb.current.cwd, program, args, t.term.cols || 80, t.term.rows || 24, providerId);
        if (cancelled) {
          void api.ptyClose(id).catch(() => {});
          return;
        }
        myId = id;
        ptyRef.current = id;
        cb.current.onPty?.(id);
        off = subscribePty(
          id,
          (bytes) => { t.term.write(bytes); activity.current?.output(bytes.length); },
          (code) => {
            if (ptyRef.current !== id) return;
            activity.current?.exit();
            ptyRef.current = null;
            exitedRef.current = true;
            cb.current.onPty?.(null);
            t.term.write(`\r\n\x1b[2m[${name} exited${code ? ` with code ${code}` : ""} — press Enter to restart]\x1b[0m\r\n`);
          },
        );
        if (visible) t.term.focus();
      } catch (e) {
        if (cancelled) return;
        exitedRef.current = true;
        t.term.write(`\r\n\x1b[31mCould not start ${name}: ${errMsg(e).replace(/\n/g, "\r\n")}\x1b[0m\r\n\x1b[2m[press Enter to retry]\x1b[0m\r\n`);
      }
    })();
    return () => {
      cancelled = true;
      off?.();
      if (myId) {
        void api.ptyClose(myId).catch(() => {});
        if (ptyRef.current === myId) {
          ptyRef.current = null;
          cb.current.onPty?.(null);
        }
      }
      // Separator between runs in the scrollback.
      if (termRef.current) termRef.current.term.write("\r\n");
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [startKey, restartNonce, program, providerId]);

  // Fit + focus when shown.
  useEffect(() => {
    const t = termRef.current;
    // Hidden terminals must not keep keyboard focus.
    if (!visible) return void t?.term.blur();
    requestAnimationFrame(() => {
      try { t?.fit.fit(); } catch { /* ignore */ }
      t?.term.focus();
    });
  }, [visible]);

  // Drop files on the terminal to type their paths (like iTerm2 / Windows Terminal): Claude Code
  // and other CLIs then read them, images included.
  const [dropping, setDropping] = useState(false);
  const onDrop = async (e: React.DragEvent) => {
    if (!isFileDrag(e.dataTransfer)) return;
    e.preventDefault();
    setDropping(false);
    const id = ptyRef.current;
    if (!id) return;
    try {
      const paths = await droppedPaths(e.dataTransfer, "terminal");
      if (paths.length) await api.ptyWrite(id, paths.map(shellQuote).join(" ") + " ");
      termRef.current?.term.focus();
    } catch (err) {
      termRef.current?.term.write(`\r\n\x1b[31mCouldn't drop the file: ${errMsg(err)}\x1b[0m\r\n`);
    }
  };

  return (
    <div className="relative h-full w-full bg-app px-2 pt-1.5 pb-1" onMouseDown={() => termRef.current?.term.focus()}
      onDragOver={(e) => { if (isFileDrag(e.dataTransfer)) { e.preventDefault(); e.dataTransfer.dropEffect = "copy"; setDropping(true); } }}
      onDragLeave={(e) => { if (!e.currentTarget.contains(e.relatedTarget as Node)) setDropping(false); }}
      onDrop={(e) => void onDrop(e)}>
      <div ref={host} className="h-full w-full" />
      {dropping && (
        <div className="pointer-events-none absolute inset-2 flex items-center justify-center rounded-lg border-2 border-dashed border-accent/60 bg-app/70 text-[13px] text-fg">
          Drop to type the file's path into the terminal
        </div>
      )}
    </div>
  );
}

function useNonce(): [number, () => void] {
  const [n, setN] = useState(0);
  return [n, () => setN((x) => x + 1)];
}
