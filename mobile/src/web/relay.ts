// The hosted browser version (GitHub Pages): reaches DevCat on the computer from any network
// through its relay, with an iroh endpoint compiled to WebAssembly (mobile/web-iroh), and
// speaks the app's protocol (crates/remote-proto). This browser has its own key, kept here;
// the computer lists it under Paired phones like the app.
import init, { Node, type Pipe } from "devcat-web-iroh";
import { Refused, carryPairing, deviceName, markUnpaired, parseLink, platform } from "./client";

export interface RelaySaved {
  hostId: string;
  hostName: string;
  relay: string;
  /** The relay server the computer was last connected to (tried before iroh's DNS lookup). */
  home?: string | null;
  addrs: string[];
  deviceId: string;
  addedAt: number;
  access?: string;
  os?: string;
  appVersion?: string;
}

type Emit = (event: string, payload: unknown) => void;
type HostMsg = { t: string; [k: string]: any };

const SAVED = "devcat.hosted.pairing";
const KEY = "devcat.hosted.key";

export function relaySaved(): RelaySaved | null {
  try { return JSON.parse(localStorage.getItem(SAVED) ?? "null"); } catch { return null; }
}

function save(s: RelaySaved | null) {
  if (s) localStorage.setItem(SAVED, JSON.stringify(s)); else { localStorage.removeItem(SAVED); markUnpaired(); }
  carryPairing();
}

/** This browser's key (32 random bytes, hex), made once. */
function key(): string {
  let k = localStorage.getItem(KEY);
  if (!k || !/^[0-9a-f]{64}$/.test(k)) {
    k = Array.from(crypto.getRandomValues(new Uint8Array(32)), (b) => b.toString(16).padStart(2, "0")).join("");
    localStorage.setItem(KEY, k);
  }
  return k;
}

function timeout<T>(p: Promise<T>, ms: number, what: string): Promise<T> {
  return new Promise((res, rej) => {
    const t = setTimeout(() => rej(what), ms);
    p.then((v) => { clearTimeout(t); res(v); }, (e) => { clearTimeout(t); rej(e); });
  });
}

const errText = (e: unknown) => (typeof e === "string" ? e : e instanceof Error ? e.message : String(e));

function denial(m: HostMsg | null): string {
  if (m?.t === "denied") return m.reason ?? "The computer refused this browser.";
  return "The computer closed the connection.";
}

let wasm: Promise<unknown> | null = null;
const nodes = new Map<string, Promise<Node>>();

/** The endpoint for `relay` (one per relay; the WebAssembly loads on first use). */
function node(relay: string): Promise<Node> {
  let n = nodes.get(relay);
  if (!n) {
    const w = (wasm ??= init());
    n = w.then(() => Node.create(key(), relay));
    n.catch(() => nodes.delete(relay));
    nodes.set(relay, n);
  }
  return n;
}

export class RelayHost {
  private pipe: Pipe | null = null;
  private live = false;
  private connecting: Promise<HostMsg> | null = null;
  private pending = new Map<number, { res: (v: unknown) => void; rej: (e: unknown) => void }>();
  private nextId = 1;
  private ping: ReturnType<typeof setInterval> | null = null;
  private lastPong = 0;
  private emit: Emit;

  constructor(emit: Emit) {
    this.emit = emit;
  }

  online(): boolean {
    return !!this.pipe && this.live;
  }

  saved() {
    return relaySaved();
  }

  private async recv(pipe: Pipe, ms: number): Promise<HostMsg | null> {
    const t = await timeout(pipe.recv(), ms, "The computer didn't answer in time.");
    return t === null ? null : JSON.parse(t);
  }

  /** Connect and say hello. Resolves with the pipe and the first answer (after approval when pairing). */
  private async open(hostId: string, relay: string, home: string | null | undefined, hostName: string, pairCode?: string): Promise<{ pipe: Pipe; first: HostMsg }> {
    const n = await timeout(node(relay), 20000, "Couldn't start networking in this browser.");
    let pipe: Pipe;
    try {
      pipe = await timeout(n.open(hostId, relay, home ?? null), 25000, "timed out");
    } catch (e) {
      throw `Couldn't reach ${hostName}. Check that DevCat is open there with Remote access on and relays allowed. (${errText(e)})`;
    }
    await pipe.send(JSON.stringify({ t: "hello", name: deviceName(), platform: platform(), appVersion: "web", ...(pairCode ? { pairCode } : {}) }));
    let first = await this.recv(pipe, 20000);
    if (first?.t === "pairing") {
      this.emit("m-pair", { stage: "waiting", hostName: first.hostName ?? hostName });
      first = await this.recv(pipe, 150000).catch(() => { throw "Nobody approved this browser on the computer in time. Try again."; });
    }
    if (first?.t !== "welcome") {
      pipe.close();
      // Without a pairing code the computer only refuses a browser it doesn't know (any more).
      if (!pairCode && first?.t === "denied") throw new Refused(denial(first));
      throw denial(first);
    }
    return { pipe, first };
  }

  /** Start the session on an admitted connection. */
  private attach(pipe: Pipe, hostId: string, welcome: HostMsg) {
    this.pipe = pipe;
    this.live = true;
    const s = relaySaved();
    if (s) save({ ...s, hostName: welcome.host?.name ?? s.hostName, access: welcome.device?.access, os: welcome.host?.os, appVersion: welcome.host?.appVersion, addrs: welcome.host?.addrs ?? s.addrs, relay: welcome.host?.relay ?? s.relay, home: welcome.host?.home ?? s.home });
    const closed = () => {
      if (this.pipe !== pipe) return;
      this.pipe = null;
      this.live = false;
      this.pending.forEach((p) => p.rej("Disconnected from the computer"));
      this.pending.clear();
      if (this.ping) clearInterval(this.ping);
      this.emit("m-conn", { id: hostId, state: "offline", error: null });
    };
    void (async () => {
      try {
        for (;;) {
          const t = await pipe.recv();
          if (t === null) break;
          const m: HostMsg = JSON.parse(t);
          if (m.t === "result") {
            const p = this.pending.get(m.id);
            this.pending.delete(m.id);
            if (m.err !== undefined) p?.rej(m.err); else p?.res(m.ok ?? null);
          } else if (m.t === "event") this.emit("m-event", { id: hostId, event: m.event, payload: m.payload });
          else if (m.t === "pong") this.lastPong = Date.now();
        }
      } catch { /* closed */ }
      closed();
    })();
    this.lastPong = Date.now();
    if (this.ping) clearInterval(this.ping);
    this.ping = setInterval(() => {
      if (Date.now() - this.lastPong > 45000) { pipe.close(); closed(); return; }
      pipe.send(JSON.stringify({ t: "ping" })).catch(() => {});
    }, 15000);
    this.emit("m-conn", { id: hostId, state: "online", error: null });
  }

  /** Pair with the link from the QR code. */
  async pair(link: string): Promise<RelaySaved> {
    const l = parseLink(link);
    if (!l?.relay) throw "This link has no DevCat pairing details. Scan the QR code in DevCat → Mobile on the computer.";
    this.close();
    this.emit("m-pair", { stage: "connecting", hostName: l.name });
    const { pipe, first } = await this.open(l.id, l.relay, l.home, l.name, l.code);
    const n = await node(l.relay);
    save({ hostId: l.id, hostName: first.host?.name ?? l.name, relay: first.host?.relay ?? l.relay, home: first.host?.home ?? l.home, addrs: l.addrs, deviceId: n.id(), addedAt: Date.now() });
    this.attach(pipe, l.id, first);
    this.emit("m-pair", { stage: "done", hostName: first.host?.name ?? l.name });
    return relaySaved()!;
  }

  /** Connected (reconnecting when needed). */
  async connect(): Promise<HostMsg> {
    const s = relaySaved();
    if (!s) throw "This browser isn't paired. Scan the QR code in DevCat → Mobile on the computer.";
    if (this.online()) return { t: "welcome" };
    if (!this.connecting) {
      this.emit("m-conn", { id: s.hostId, state: "connecting", error: null });
      this.connecting = this.open(s.hostId, s.relay, s.home, s.hostName)
        .then(({ pipe, first }) => { this.attach(pipe, s.hostId, first); return first; })
        .catch((e) => {
          if (e instanceof Refused) {
            save(null);
            this.emit("m-conn", { id: s.hostId, state: "offline", error: e.reason, unpaired: true });
            throw e.reason;
          }
          this.emit("m-conn", { id: s.hostId, state: "offline", error: errText(e) });
          throw e;
        })
        .finally(() => { this.connecting = null; });
    }
    return this.connecting;
  }

  async call(cmd: string, args: unknown): Promise<unknown> {
    await this.connect();
    const id = this.nextId++;
    return new Promise((res, rej) => {
      this.pending.set(id, { res, rej });
      this.pipe!.send(JSON.stringify({ t: "call", id, cmd, args: args ?? null })).catch((e: unknown) => { this.pending.delete(id); rej(errText(e)); });
    });
  }

  close() {
    const p = this.pipe;
    this.pipe = null;
    this.live = false;
    p?.close();
  }

  forget() {
    this.close();
    save(null);
  }
}
