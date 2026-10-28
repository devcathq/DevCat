// A room whiteboard (Excalidraw). Its shapes live in the room's document as the root map
// `board:<path>`, shape id → the shape's JSON (crates/rooms doc.rs), in the window's own copy
// (replica.ts) like a text file's characters. A shape changed here is written when its version
// goes up; shapes from others are merged with Excalidraw's own reconcile (the newer version of a
// shape wins, and a shape being drawn here isn't taken away mid-stroke). Loaded on demand: the
// drawing code is large.
import { CaptureUpdateAction, Excalidraw, reconcileElements } from "@excalidraw/excalidraw";
import "@excalidraw/excalidraw/index.css";
import type { OrderedExcalidrawElement } from "@excalidraw/excalidraw/element/types";
import type { AppState, ExcalidrawImperativeAPI } from "@excalidraw/excalidraw/types";
import { useEffect, useRef, useState } from "react";
import { useIsDark } from "../appearance/store";
import type { Replica } from "./replica";

// fonts from the app (vite.config.ts), not Excalidraw's CDN
(window as unknown as { EXCALIDRAW_ASSET_PATH: string }).EXCALIDRAW_ASSET_PATH = "/excalidraw/";

type Shape = OrderedExcalidrawElement;
type Remote = Parameters<typeof reconcileElements>[1];

function read(replica: Replica, path: string): Shape[] {
  const map = replica.board(path);
  const out: Shape[] = [];
  for (const v of map.values()) {
    if (typeof v !== "string") continue;
    try { out.push(JSON.parse(v) as Shape); } catch { /* not a shape */ }
  }
  return out;
}

export default function Board({ replica, path, readOnly }: { replica: Replica; path: string; readOnly: boolean }) {
  const dark = useIsDark();
  const [api, setApi] = useState<ExcalidrawImperativeAPI | null>(null);
  // the version of each shape as the room has it, so only shapes changed here are written
  const known = useRef(new Map<string, number>());
  const [initial] = useState(() => {
    const shapes = read(replica, path);
    for (const s of shapes) known.current.set(s.id, s.version);
    return shapes;
  });

  // others' changes → the scene
  useEffect(() => {
    if (!api) return;
    const pull = () => {
      const remote = read(replica, path).filter((s) => (known.current.get(s.id) ?? -1) < s.version);
      if (!remote.length) return;
      for (const s of remote) known.current.set(s.id, s.version);
      const merged = reconcileElements(api.getSceneElementsIncludingDeleted() as Shape[], remote as unknown as Remote, api.getAppState() as AppState);
      api.updateScene({ elements: merged, captureUpdate: CaptureUpdateAction.NEVER });
    };
    pull();
    const unsub = replica.doc.subscribe(() => pull());
    const off = replica.onChange(pull);
    return () => { unsub(); off(); };
  }, [api, replica, path]);

  // changes here → the room (shapes whose version went up)
  const onChange = (elements: readonly Shape[]) => {
    if (readOnly) return;
    const map = replica.board(path);
    let changed = false;
    for (const el of elements) {
      if ((known.current.get(el.id) ?? -1) >= el.version) continue;
      known.current.set(el.id, el.version);
      map.set(el.id, JSON.stringify(el));
      changed = true;
    }
    if (changed) replica.doc.commit();
  };

  return (
    <div className="h-full w-full">
      <Excalidraw
        excalidrawAPI={setApi}
        initialData={{ elements: initial, appState: { viewBackgroundColor: dark ? "#1e1e1e" : "#ffffff" }, scrollToContent: true }}
        onChange={(els) => onChange(els as readonly Shape[])}
        theme={dark ? "dark" : "light"}
        viewModeEnabled={readOnly}
        UIOptions={{ tools: { image: false }, canvasActions: { loadScene: false, saveToActiveFile: false, toggleTheme: false } }}
      />
    </div>
  );
}
