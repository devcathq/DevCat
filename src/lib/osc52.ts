// OSC 52 (set the clipboard): how programs in a terminal copy text. Claude Code, tmux and vim use
// it, and it's the only way for a program on an SSH host, which can't reach this computer's
// clipboard otherwise. Only writes are honoured: a `?` query (read the clipboard) is ignored, so a
// program or a remote host can't read what's on this computer's clipboard.
import type { IDisposable, Terminal } from "@xterm/xterm";

/** The text an OSC 52 sequence asks to copy, or null to ignore it (a read query, a clear, bad base64).
 *  `data` is what follows `52;`: `<selections>;<base64>` (`c;aGk=`; the selections may be empty). */
export function osc52Text(data: string): string | null {
  const semi = data.indexOf(";");
  if (semi < 0) return null;
  const b64 = data.slice(semi + 1).replace(/\s+/g, "");
  if (!b64 || !/^[A-Za-z0-9+/]+={0,2}$/.test(b64)) return null;
  let bin: string;
  try { bin = atob(b64); } catch { return null; }
  const bytes = Uint8Array.from(bin, (c) => c.charCodeAt(0));
  try {
    return new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  } catch {
    return null;
  }
}

/** Handle OSC 52 in `term`: `write` gets the text while `allowed()` (asked per sequence, so the setting applies live). */
export function handleOsc52(term: Terminal, allowed: () => boolean, write: (text: string) => void): IDisposable {
  return term.parser.registerOscHandler(52, (data) => {
    if (allowed()) {
      const text = osc52Text(data);
      if (text !== null) write(text);
    }
    // handled either way: nothing else should see it
    return true;
  });
}
