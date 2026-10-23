// Browser connection to the DevCat that served this page: an encrypted WebSocket session
// (crates/remote-proto/src/web.rs). The pairing secret comes from the QR link's fragment, which
// browsers never send over the network; afterwards a device secret kept in this browser.
import { b64url, b64urlDecode, browserKeys, randomBytes, type Half, type Mode } from "./crypto";

export interface Saved {
  hostId: string;
  hostName: string;
  deviceId: string;
  secret: string;
  addedAt: number;
  access?: string;
  os?: string;
  appVersion?: string;
}

type Emit = (event: string, payload: unknown) => void;
type HostMsg = { t: string; [k: string]: any };

const SAVED = "devcat.web.pairing";
const NAME = "devcat.web.name";
const enc = new TextEncoder();
const dec = new TextDecoder();

export function platform(): string {
  const ua = navigator.userAgent;
  return /iphone|ipad|ipod/i.test(ua) ? "ios" : /android/i.test(ua) ? "android" : "web";
}

export function deviceName(): string {
  return localStorage.getItem(NAME) || (platform() === "ios" ? "iPhone browser" : platform() === "android" ? "Android browser" : "Phone browser");
}

export function setDeviceName(n: string) {
  localStorage.setItem(NAME, n);
}

export function saved(): Saved | null {
  try { return JSON.parse(localStorage.getItem(SAVED) ?? "null"); } catch { return null; }
}

function save(s: Saved | null) {
  if (s) localStorage.setItem(SAVED, JSON.stringify(s)); else { localStorage.removeItem(SAVED); markUnpaired(); }
  carryPairing();
}

/** The computer answered that it doesn't know this browser's pairing (revoked, or its pairings
 *  were reset): keeping it would only fail again, so the browser forgets it and pairs anew. */
export class Refused {
  constructor(readonly reason: string) {}
}

// When this browser last dropped its pairing. A Home Screen icon keeps the address it was added
// with, pairing included, so without this the dropped pairing would come back on the next launch.
const UNPAIRED = "devcat.web.unpairedAt";

export function markUnpaired() {
  localStorage.setItem(UNPAIRED, String(Date.now()));
}

// Browsers don't share storage: not the in-app browser a QR code opens in, not Safari, not a
// Home Screen icon. So the address bar carries this browser's pairing too (in the fragment,
// which never goes over the network): open the same address anywhere and it's paired.
const CARRY = [SAVED, "devcat.hosted.pairing", "devcat.hosted.key"];
const CARRY_PARAM = "dc";

/** Put this browser's pairing in the address bar (or take it out when there's none). */
export function carryPairing() {
  const base = location.pathname + location.search;
  if (!pairedHostId()) {
    if (new URLSearchParams(location.hash.slice(1)).has(CARRY_PARAM)) history.replaceState(history.state, "", base);
    return;
  }
  const v: Record<string, string> = {};
  for (const k of CARRY) { const x = localStorage.getItem(k); if (x !== null) v[k] = x; }
  const hash = `#${CARRY_PARAM}=${b64url(enc.encode(JSON.stringify(v)))}`;
  if (location.hash !== hash) history.replaceState(history.state, "", base + hash);
}

/** When the pairing in a store (this browser's, or the carried one) was made; 0 for none. */
function pairedAt(get: (k: string) => string | null | undefined): number {
  let t = 0;
  for (const k of [SAVED, "devcat.hosted.pairing"]) {
    try { t = Math.max(t, Number(JSON.parse(get(k) ?? "null")?.addedAt) || 0); } catch { /* ignore */ }
  }
  return t;
}

/** Take the pairing the address bar carries (opened from another browser) into this one, unless
 *  this one has a newer pairing or dropped it since (a Home Screen icon keeps the address it was
 *  added with). */
export function adoptCarried() {
  const p = new URLSearchParams(location.hash.slice(1)).get(CARRY_PARAM);
  if (!p) return;
  try {
    const v = JSON.parse(dec.decode(b64urlDecode(p)));
    if (!CARRY.some((k) => typeof v[k] === "string")) return;
    const mine = Math.max(pairedAt((k) => localStorage.getItem(k)), Number(localStorage.getItem(UNPAIRED)) || 0);
    if (mine > pairedAt((k) => v[k])) return;
    for (const k of CARRY) if (typeof v[k] === "string") localStorage.setItem(k, v[k]); else localStorage.removeItem(k);
  } catch { /* not ours */ }
}

/** Pairing details from a QR link: `http://host/#v=1&id=…&code=…&s=…&name=…` (served by the
 *  computer; `s` is the browser key) or `https://…/#v=1&id=…&code=…&name=…&relay=…` (hosted). */
export interface LinkInfo { id: string; code: string; secret: string; name: string; addrs: string[]; relay: string | null }

export function parseLink(link: string): LinkInfo | null {
  const frag = link.includes("#") ? link.slice(link.indexOf("#") + 1) : link;
  const p = new URLSearchParams(frag);
  const id = p.get("id") ?? "", code = p.get("code") ?? "", secret = p.get("s") ?? "", relay = p.get("relay") || null;
  if (!/^[0-9a-f]{64}$/i.test(id) || !code || (!secret && !relay)) return null;
  // Only the computer serves the page over http: then its address is the page's.
  let host = "";
  try { host = link.startsWith("http://") ? new URL(link).host : ""; } catch { host = ""; }
  return { id, code, secret, relay, name: p.get("name") ?? "DevCat", addrs: [host, ...p.getAll("addr")].filter((a, i, all) => a && all.indexOf(a) === i) };
}

/** The same pairing for the DevCat Mobile app. */
export function appLink(l: LinkInfo): string {
  const q = new URLSearchParams({ v: "1", id: l.id, code: l.code, name: l.name });
  l.addrs.forEach((a) => q.append("addr", a));
  if (l.relay) q.append("relay", l.relay);
  return `devcat://pair?${q.toString()}`;
}

/** The computer this browser is paired with, either way (served page or hosted). */
export function pairedHostId(): string | null {
  for (const k of [SAVED, "devcat.hosted.pairing"]) {
    try { const s = JSON.parse(localStorage.getItem(k) ?? "null"); if (s?.hostId) return s.hostId; } catch { /* ignore */ }
  }
  return null;
}

class Conn {
  ws: WebSocket;
  keys: { send: Half; recv: Half } | null = null;
  inbox: (HostMsg | string)[] = [];
  waiters: ((m: HostMsg | string | null) => void)[] = [];
  closed = false;
  onSession: ((m: HostMsg) => void) | null = null;
  onClose: (() => void) | null = null;

  constructor(url: string) {
    this.ws = new WebSocket(url);
    this.ws.binaryType = "arraybuffer";
    this.ws.onmessage = (e) => {
      let m: HostMsg | string;
      if (typeof e.data === "string") m = e.data;
      else {
        const plain = this.keys?.recv.open(new Uint8Array(e.data as ArrayBuffer));
        if (!plain) { this.ws.close(); return; }
        m = JSON.parse(dec.decode(plain));
      }
      if (this.onSession && typeof m !== "string") this.onSession(m);
      else if (this.waiters.length) this.waiters.shift()!(m);
      else this.inbox.push(m);
    };
    this.ws.onclose = () => {
      this.closed = true;
      this.waiters.splice(0).forEach((w) => w(null));
      this.onClose?.();
    };
  }

  opened(): Promise<void> {
    return new Promise((res, rej) => {
      if (this.ws.readyState === WebSocket.OPEN) return res();
      this.ws.addEventListener("open", () => res(), { once: true });
      this.ws.addEventListener("error", () => rej("Couldn't reach DevCat on the computer. Same Wi-Fi? Is DevCat open with Remote access on?"), { once: true });
    });
  }

  next(timeoutMs: number): Promise<HostMsg | string | null> {
    if (this.inbox.length) return Promise.resolve(this.inbox.shift()!);
    if (this.closed) return Promise.resolve(null);
    return new Promise((res) => {
      const t = setTimeout(() => res(null), timeoutMs);
      this.waiters.push((m) => { clearTimeout(t); res(m); });
    });
  }

  send(m: unknown) {
    if (!this.keys || this.ws.readyState !== WebSocket.OPEN) throw "Disconnected from the computer";
    this.ws.send(this.keys.send.seal(enc.encode(JSON.stringify(m))) as Uint8Array<ArrayBuffer>);
  }
}

function denial(m: HostMsg | string | null): string {
  if (m === null) return "The computer closed the connection.";
  if (typeof m === "string") {
    try { const h = JSON.parse(m); if (h.reason) return h.reason; } catch { /* not JSON */ }
    return m;
  }
  return m.reason ?? "The computer refused the connection.";
}

export class WebHost {
  private conn: Conn | null = null;
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
    return !!this.conn && !this.conn.closed && !!this.conn.onSession;
  }

  /** Open + challenge + encrypted hello. Resolves with the first session message. */
  private async handshake(mode: Mode, secret: Uint8Array, deviceId: string | null): Promise<{ conn: Conn; first: HostMsg }> {
    const conn = new Conn(`${location.protocol === "https:" ? "wss" : "ws"}://${location.host}/ws`);
    await conn.opened();
    const nonce = randomBytes(16);
    conn.ws.send(JSON.stringify({ t: "open", device: deviceId, nonce: b64url(nonce) }));
    const ch = await conn.next(15000);
    let challenge: { t: string; nonce?: string; reason?: string };
    try { challenge = typeof ch === "string" ? JSON.parse(ch) : { t: "?" }; } catch { challenge = { t: "?" }; }
    if (challenge.t !== "challenge" || !challenge.nonce) {
      conn.ws.close();
      // Asked with a device id, the only answer before the challenge is "not paired (any more)".
      if (deviceId && challenge.t === "denied") throw new Refused(denial(ch));
      throw denial(ch);
    }
    conn.keys = browserKeys(secret, nonce, b64urlDecode(challenge.nonce), mode);
    conn.send({ t: "hello", name: deviceName(), platform: platform(), appVersion: "web" });
    const first = await conn.next(mode === "pair" ? 20000 : 15000);
    if (!first || typeof first === "string" || first.t === "denied") {
      conn.ws.close();
      throw denial(first);
    }
    return { conn, first };
  }

  /** Start the session on an admitted connection (after `welcome`). */
  private attach(conn: Conn, hostId: string, welcome: HostMsg) {
    this.conn = conn;
    const s = saved();
    if (s) save({ ...s, hostName: welcome.host?.name ?? s.hostName, access: welcome.device?.access, os: welcome.host?.os, appVersion: welcome.host?.appVersion });
    conn.onSession = (m) => {
      if (m.t === "result") {
        const p = this.pending.get(m.id);
        this.pending.delete(m.id);
        if (m.err !== undefined) p?.rej(m.err); else p?.res(m.ok ?? null);
      } else if (m.t === "event") this.emit("m-event", { id: hostId, event: m.event, payload: m.payload });
      else if (m.t === "pong") this.lastPong = Date.now();
    };
    conn.onClose = () => {
      if (this.conn === conn) this.conn = null;
      this.pending.forEach((p) => p.rej("Disconnected from the computer"));
      this.pending.clear();
      if (this.ping) clearInterval(this.ping);
      this.emit("m-conn", { id: hostId, state: "offline", error: null });
    };
    this.lastPong = Date.now();
    if (this.ping) clearInterval(this.ping);
    this.ping = setInterval(() => {
      if (Date.now() - this.lastPong > 45000) { conn.ws.close(); return; }
      try { conn.send({ t: "ping" }); } catch { /* closing */ }
    }, 15000);
    this.emit("m-conn", { id: hostId, state: "online", error: null });
  }

  /** Pair with the link from the QR code; returns what's saved for this browser. */
  async pair(link: string): Promise<Saved> {
    const l = parseLink(link);
    if (!l) throw "This link has no DevCat pairing details. Scan the QR code in DevCat → Mobile on the computer.";
    this.emit("m-pair", { stage: "connecting", hostName: l.name });
    let { conn, first } = await this.handshake("pair", b64urlDecode(l.secret), null);
    if (first.t === "pairing") {
      this.emit("m-pair", { stage: "waiting", hostName: first.hostName ?? l.name });
      const m = await conn.next(150000);
      if (!m || typeof m === "string" || m.t !== "paired") { conn.ws.close(); throw m === null ? "Nobody approved this phone on the computer in time. Try again." : denial(m); }
      first = m;
    }
    if (first.t !== "paired") { conn.ws.close(); throw denial(first); }
    const s: Saved = { hostId: l.id, hostName: l.name, deviceId: first.deviceId, secret: first.secret, addedAt: Date.now() };
    save(s);
    const welcome = await conn.next(10000);
    if (!welcome || typeof welcome === "string" || welcome.t !== "welcome") { conn.ws.close(); throw denial(welcome); }
    this.attach(conn, l.id, welcome);
    this.emit("m-pair", { stage: "done", hostName: welcome.host?.name ?? l.name });
    return saved()!;
  }

  /** Connected (reconnecting with this browser's device secret when needed). */
  async connect(): Promise<HostMsg> {
    const s = saved();
    if (!s) throw "This browser isn't paired. Scan the QR code in DevCat → Mobile on the computer.";
    if (this.online()) return { t: "welcome" };
    if (!this.connecting) {
      this.emit("m-conn", { id: s.hostId, state: "connecting", error: null });
      this.connecting = this.handshake("device", b64urlDecode(s.secret), s.deviceId)
        .then(({ conn, first }) => {
          if (first.t !== "welcome") { conn.ws.close(); throw denial(first); }
          this.attach(conn, s.hostId, first);
          return first;
        })
        .catch((e) => {
          if (e instanceof Refused) {
            save(null);
            this.emit("m-conn", { id: s.hostId, state: "offline", error: e.reason, unpaired: true });
            throw e.reason;
          }
          this.emit("m-conn", { id: s.hostId, state: "offline", error: typeof e === "string" ? e : String(e) });
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
      try { this.conn!.send({ t: "call", id, cmd, args: args ?? null }); } catch (e) { this.pending.delete(id); rej(e); }
    });
  }

  close() {
    this.conn?.ws.close();
    this.conn = null;
  }

  forget() {
    this.close();
    save(null);
  }
}
