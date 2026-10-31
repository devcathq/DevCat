// The window's own copy of a room's document (Loro, the same CRDT as the backend's), kept in step
// with the backend's copy by exchanging updates. Typing goes into this copy and its updates go to
// the backend, which merges and passes them on; changes from others come back as updates. Both
// copies merge whatever arrives in whatever order, so no edit overwrites another.
import { LoroDoc, UndoManager, type LoroMap, type LoroText } from "loro-crdt/base64";
import type * as Monaco from "monaco-editor";
import { toast } from "sonner";
import { errMsg } from "../../lib/api";
import { fromB64, rooms, toB64 } from "./api";

/** A path inside a root container's name: Loro refuses "/" there (a file in a folder), so "/" is
 *  written %2F and "%" %25, as the backend does (crates/rooms doc.rs `root_part`). */
export const rootPart = (path: string) => path.replace(/%/g, "%25").replace(/\//g, "%2F");

export class Replica {
  readonly doc = new LoroDoc();
  private pushing: Promise<void> = Promise.resolve();
  private pulling: Promise<void> | null = null;
  private again = false;
  private listeners = new Set<() => void>();

  constructor(readonly room: string) {
    // each change says when it was made, a minute apart at most: the file's edit history
    // (crates/rooms doc.rs `edits`, CHANGE_MERGE_SECS)
    this.doc.setRecordTimestamp(true);
    this.doc.setChangeMergeInterval(60);
  }

  /** Start sending this copy's changes to the backend; returns the stop (for an effect). */
  attach(): () => void {
    return this.doc.subscribeLocalUpdates((bytes) => this.push(bytes));
  }

  private push(bytes: Uint8Array) {
    const update = toB64(bytes);
    this.pushing = this.pushing
      .then(() => rooms.docImport(this.room, update, this.doc.peerIdStr))
      .catch((e) => { toast.error(`Your change didn't reach the room: ${errMsg(e)}`); void this.pull(); });
  }

  /** Fetch what this copy lacks (calls made meanwhile run once more after it). */
  pull(): Promise<void> {
    if (this.pulling) {
      this.again = true;
      return this.pulling;
    }
    this.pulling = (async () => {
      do {
        this.again = false;
        const { updates } = await rooms.docUpdates(this.room, toB64(this.doc.oplogVersion().encode()));
        const bytes = fromB64(updates);
        if (bytes.length > 0) {
          this.doc.import(bytes);
          this.listeners.forEach((f) => f());
        }
      } while (this.again);
    })().finally(() => { this.pulling = null; });
    return this.pulling;
  }

  text(path: string): LoroText {
    return this.doc.getText(`file:${rootPart(path)}`);
  }

  /** A whiteboard's shapes (shape id → its JSON): the backend's root map `board:<path>`. */
  board(path: string): LoroMap {
    return this.doc.getMap(`board:${rootPart(path)}`);
  }

  onChange(f: () => void): () => void {
    this.listeners.add(f);
    return () => { this.listeners.delete(f); };
  }

}

/** Make the model show `next` with the smallest edit (keeps cursors and selections in place). */
function reconcile(model: Monaco.editor.ITextModel, next: string) {
  const cur = model.getValue();
  if (cur === next) return;
  let start = 0;
  while (start < cur.length && start < next.length && cur.charCodeAt(start) === next.charCodeAt(start)) start++;
  let endCur = cur.length;
  let endNext = next.length;
  while (endCur > start && endNext > start && cur.charCodeAt(endCur - 1) === next.charCodeAt(endNext - 1)) {
    endCur--;
    endNext--;
  }
  const a = model.getPositionAt(start);
  const b = model.getPositionAt(endCur);
  model.applyEdits([{ range: { startLineNumber: a.lineNumber, startColumn: a.column, endLineNumber: b.lineNumber, endColumn: b.column }, text: next.slice(start, endNext), forceMoveMarkers: false }]);
}

let binds = 0;

/**
 * Bind a Monaco editor to a room file: typing becomes CRDT inserts and deletes (positions are
 * UTF-16 on both sides), others' changes show up as minimal edits, and Ctrl/Cmd+Z undoes only
 * your own changes (Loro's undo, which steps around everyone else's).
 */
export function bind(monaco: typeof Monaco, editor: Monaco.editor.IStandaloneCodeEditor, replica: Replica, path: string): () => void {
  const model = editor.getModel();
  if (!model) return () => {};
  const text = replica.text(path);
  // The editor's line breaks must be the document's, or positions drift by one per line: a new
  // model on Windows writes "\r\n", so a "\n" document got "\r\n" in the editor and every
  // keystroke landed past the end of the text (Loro: "Index out of bound"), and nothing synced.
  const content = text.toString();
  model.setEOL(content.includes("\r\n") ? monaco.editor.EndOfLineSequence.CRLF : monaco.editor.EndOfLineSequence.LF);
  reconcile(model, content);
  const undo = new UndoManager(replica.doc, { mergeInterval: 800, maxUndoSteps: 200 });
  let fromRemote = false;

  const typed = model.onDidChangeContent((e) => {
    if (fromRemote) return;
    // Offsets refer to the text before this event: apply from the end backwards.
    const changes = [...e.changes].sort((x, y) => y.rangeOffset - x.rangeOffset);
    for (const c of changes) {
      if (c.rangeLength > 0) text.delete(c.rangeOffset, c.rangeLength);
      if (c.text) text.insert(c.rangeOffset, c.text);
    }
    replica.doc.commit();
  });
  const sync = () => {
    const next = text.toString();
    if (model.getValue() === next) return;
    fromRemote = true;
    try {
      reconcile(model, next);
    } finally {
      fromRemote = false;
    }
  };
  const unsub = replica.doc.subscribe(() => sync());
  const offPull = replica.onChange(sync);
  const n = ++binds;
  const actions = [
    editor.addAction({ id: `room-undo-${n}`, label: "Undo (your changes)", keybindings: [monaco.KeyMod.CtrlCmd | monaco.KeyCode.KeyZ], run: () => { undo.undo(); sync(); } }),
    editor.addAction({ id: `room-redo-${n}`, label: "Redo (your changes)", keybindings: [monaco.KeyMod.CtrlCmd | monaco.KeyMod.Shift | monaco.KeyCode.KeyZ, monaco.KeyMod.CtrlCmd | monaco.KeyCode.KeyY], run: () => { undo.redo(); sync(); } }),
  ];
  return () => {
    actions.forEach((a) => a.dispose());
    typed.dispose();
    unsub();
    offPull();
    undo.free();
  };
}

/** Characters (what the backend counts in) → UTF-16 offset in this copy of the file. */
export function utf16(text: LoroText, chars: number): number {
  const v = text.convertPos(chars, "unicode", "utf16");
  return typeof v === "number" ? v : text.length;
}
