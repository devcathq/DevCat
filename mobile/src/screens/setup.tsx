// First launch and pairing: welcome guide, QR scan, paste link, approval wait, computer list.
import { Clipboard, Laptop, Monitor, Plus, QrCode, Smartphone } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { IS_HOSTED, IS_WEB, isPairLink } from "../lib/env";
import { readQr } from "../lib/qr";
import { errMsg, onPair, phone } from "../lib/rpc";
import { loadMachines, nav, selectMachine, toast, useApp } from "../lib/store";
import { appLink, parseLink } from "../web/client";
import { Badge, Btn, Card, ErrorBox, Field, Input, Note, Screen, Spinner, TextArea, ago } from "../ui";

const isPhone = () => /android|iphone|ipad/i.test(navigator.userAgent);

function StepRow({ n, title, meta }: { n: number; title: string; meta: string }) {
  return (
    <div className="flex items-center gap-3 rounded-xl bg-raised px-3 py-2.5">
      <span className="flex size-6 shrink-0 items-center justify-center rounded-full bg-accent/15 text-[12px] font-bold text-accent">{n}</span>
      <span className="min-w-0"><span className="block font-semibold">{title}</span><span className="block text-[11.5px] text-muted">{meta}</span></span>
    </div>
  );
}

/** Browser: pair from a photo of the QR code, right in this browser (no hand-over from the
 *  camera app's in-app browser, which doesn't keep the pairing). */
function WebScan() {
  const picker = useRef<HTMLInputElement>(null);
  const [busy, setBusy] = useState(false);
  const scan = async (file: File | undefined) => {
    if (!file) return;
    setBusy(true);
    try {
      const link = await readQr(file);
      if (!link) { toast("No QR code found. Fill the photo with the code and try again.", "err"); return; }
      if (link.trim().startsWith("devcat://")) { toast("That QR code is for the DevCat app. On the computer turn on Allow phone browsers in DevCat → Mobile, then try again.", "err"); return; }
      if (!isPairLink(link) || !parseLink(link)) { toast("That QR code isn't a DevCat pairing code.", "err"); return; }
      if (new URL(link).origin === location.origin) nav.reset([{ s: "weblanding", link }]);
      else location.href = link;
    } catch {
      toast("Couldn't read that photo. Try again.", "err");
    } finally {
      setBusy(false);
    }
  };
  return (
    <>
      <input ref={picker} type="file" accept="image/*" capture="environment" hidden onChange={(e) => { const f = e.target.files?.[0]; e.target.value = ""; void scan(f); }} />
      <Btn variant="primary" block disabled={busy} onClick={() => picker.current?.click()}>{busy ? <Spinner /> : <QrCode className="size-5" />}Scan QR code</Btn>
    </>
  );
}

export function Welcome() {
  return (
    <div className="flex h-full flex-col gap-3 px-5" style={{ paddingTop: "max(36px, env(safe-area-inset-top))", paddingBottom: "max(20px, env(safe-area-inset-bottom))" }}>
      <img src={`${import.meta.env.BASE_URL}icon.png`} alt="" className="size-14 rounded-2xl" />
      <div className="text-[24px] font-bold">DevCat Mobile</div>
      <div className="text-muted">Watch your AI agents and reach your databases, Redis, VPNs and clusters through DevCat on your computer.</div>
      <div className="pt-2 text-[10.5px] font-semibold tracking-wider text-faint uppercase">Set up in 3 steps</div>
      <StepRow n={1} title="Open DevCat on your computer" meta="Title bar → 📱 Mobile → turn on" />
      <StepRow n={2} title="Any network: Wi-Fi or mobile data" meta="through a relay (the default); with relays off, the same Wi-Fi" />
      <StepRow n={3} title={IS_WEB ? "Scan the QR code with your camera" : "Scan the QR code it shows"} meta={IS_WEB ? "it opens this page, ready to pair" : "then click Allow on the computer"} />
      <div className="flex-1" />
      {IS_WEB ? (
        <>
          <Note>This browser isn't paired yet. On the computer click <b>Show pairing code</b>, then take a photo of the QR code here.</Note>
          <WebScan />
        </>
      ) : (
        <>
          <Btn variant="primary" block onClick={() => nav.push({ s: "scan" })}><QrCode className="size-5" />Scan QR code</Btn>
          <Btn block onClick={() => nav.push({ s: "paste" })}><Clipboard className="size-4" />Paste pairing link</Btn>
        </>
      )}
      <button className="py-1 text-center text-[13px] font-semibold text-accent" onClick={() => nav.push({ s: "help" })}>Can't connect? Help</button>
    </div>
  );
}

export function Scan() {
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const start = async () => {
    setError(null);
    setBusy(true);
    try {
      const bs = await import("@tauri-apps/plugin-barcode-scanner");
      let perm = await bs.checkPermissions();
      if (perm !== "granted") perm = await bs.requestPermissions();
      if (perm !== "granted") {
        setError("DevCat needs the camera to read the pairing code. Allow it in the phone's Settings, or paste the link instead.");
        return;
      }
      const r = await bs.scan({ windowed: false, formats: [bs.Format.QRCode] });
      if (!r?.content) return;
      if (!isPairLink(r.content)) {
        setError("That's not a DevCat pairing code. Scan the code in DevCat → Mobile on your computer.");
        return;
      }
      nav.replace({ s: "pairing", link: r.content });
    } catch (e) {
      const m = errMsg(e);
      setError(isPhone() ? m : "Scanning works in the phone app. Here, paste the pairing link instead.");
    } finally {
      setBusy(false);
    }
  };
  useEffect(() => { void start(); }, []);
  return (
    <Screen title="Scan pairing code">
      <div className="flex aspect-square items-center justify-center rounded-2xl bg-[radial-gradient(circle_at_50%_40%,#2a2f39,#0a0b0d_70%)]">
        {busy ? <Spinner /> : <div className="size-48 rounded-2xl border-[3px] border-accent" />}
      </div>
      <div className="text-center text-[13px] text-muted">Point the camera at the QR code in <b className="text-fg">DevCat → 📱 Mobile</b> on your computer.</div>
      {error && <ErrorBox error={error} />}
      <Btn block variant="primary" onClick={() => void start()} disabled={busy}><QrCode className="size-5" />Scan again</Btn>
      <Btn block onClick={() => nav.replace({ s: "paste" })}>Paste link instead</Btn>
    </Screen>
  );
}

export function Paste({ link: initial }: { link?: string }) {
  const [link, setLink] = useState(initial ?? "");
  const [addr, setAddr] = useState("");
  const valid = isPairLink(link);
  const fromClipboard = async () => {
    try { setLink((await navigator.clipboard.readText()).trim()); } catch { toast("Couldn't read the clipboard. Long-press the box and paste.", "err"); }
  };
  return (
    <Screen title="Paste pairing link">
      <Field label="Pairing link">
        <TextArea rows={4} className="font-mono text-[12px]" placeholder="devcat://pair?v=1&id=…" value={link} onChange={(e) => setLink(e.target.value)} />
      </Field>
      <Btn size="sm" onClick={() => void fromClipboard()}><Clipboard className="size-3.5" />Paste from clipboard</Btn>
      <Note>On the computer: DevCat → 📱 Mobile → <b>Copy link</b>, then send it to this phone (AirDrop, a note to yourself…). The link works once and expires after 5 minutes.</Note>
      <Field label="Computer's address (optional)" hint="Only if the computer is outside this network: its public host name or IP, e.g. devbox.example.com:47100">
        <Input placeholder="host:47100" value={addr} onChange={(e) => setAddr(e.target.value)} />
      </Field>
      <Btn variant="primary" block disabled={!valid} onClick={() => nav.replace({ s: "pairing", link: link.trim(), addr: addr.trim() || null })}>Continue</Btn>
    </Screen>
  );
}

export function Pairing({ link, addr }: { link: string; addr?: string | null }) {
  const info = useApp((s) => s.info);
  const [name, setName] = useState(info?.deviceName ?? "");
  const [stage, setStage] = useState<"ready" | "connecting" | "waiting" | "error">("ready");
  const [error, setError] = useState<string | null>(null);
  const hostName = (() => { try { return decodeURIComponent(/[?&#]name=([^&]*)/.exec(link)?.[1] ?? "").replace(/\+/g, " ") || "your computer"; } catch { return "your computer"; } })();
  const hostAddr = (() => {
    try {
      if (addr) return addr;
      if (/^https?:\/\//.test(link)) return new URL(link).host;
      return decodeURIComponent(/[?&]addr=([^&]*)/.exec(link)?.[1] ?? "");
    } catch { return ""; }
  })();
  useEffect(() => onPair((p) => { if (p.stage === "waiting") setStage("waiting"); }), []);

  const go = async () => {
    setError(null);
    setStage("connecting");
    try {
      if (name.trim() && name.trim() !== info?.deviceName) {
        await phone.setName(name.trim());
        useApp.setState({ info: info ? { ...info, deviceName: name.trim() } : info });
      }
      const m = await phone.pair(link, addr);
      await loadMachines();
      selectMachine(m.id);
      toast(`Paired with ${m.name}`, "ok");
      nav.reset([{ s: "computers" }, { s: "agents" }]);
    } catch (e) {
      setError(errMsg(e));
      setStage("error");
    }
  };

  return (
    <Screen title="Pairing">
      <div className="flex flex-col items-center gap-1 pt-6 pb-2">
        <Laptop className="size-12 text-muted" />
        <div className="text-[18px] font-bold">{hostName}</div>
        {hostAddr && <div className="font-mono text-[12px] text-muted">{hostAddr}</div>}
      </div>
      {stage === "waiting" ? (
        <Card tone="warn" className="text-center">
          <div className="font-semibold">⏳ Approve this phone on your computer</div>
          <div className="mt-1 text-[12px] text-muted">DevCat shows "Allow this phone to control DevCat?". Click <b className="text-fg">Allow</b>.</div>
        </Card>
      ) : stage === "connecting" ? (
        <div className="flex items-center justify-center gap-2 py-3 text-muted"><Spinner />Connecting…</div>
      ) : null}
      {error && <ErrorBox error={error} />}
      <Field label="This phone's name (shown on the computer)">
        <Input value={name} onChange={(e) => setName(e.target.value)} disabled={stage === "connecting" || stage === "waiting"} />
      </Field>
      <div className="flex-1" />
      {stage === "ready" || stage === "error" ? (
        <Btn variant="primary" block onClick={() => void go()}>{stage === "error" ? "Try again" : "Connect"}</Btn>
      ) : null}
      <Btn block onClick={() => nav.back()}>Cancel</Btn>
      {stage === "error" && <Note>Codes work once and expire after 5 minutes. If it keeps failing, click <b>New code</b> on the computer and scan again, or see <button className="font-semibold text-accent" onClick={() => nav.push({ s: "help" })}>Help</button>.</Note>}
    </Screen>
  );
}

export function Computers() {
  const machines = useApp((s) => s.machines);
  const conn = useApp((s) => s.conn);
  const current = useApp((s) => s.current);
  const terms = useApp((s) => s.terms);
  const agents = useApp((s) => s.agents);
  useEffect(() => {
    void loadMachines();
    machines.forEach((m) => { if (!conn[m.id] || conn[m.id].state === "offline") void phone.connect(m.id).catch(() => {}); });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  const open = (id: string) => { selectMachine(id); nav.push({ s: "agents" }); };
  return (
    <Screen title="Computers" back={false} right={IS_WEB ? undefined : <button aria-label="Pair another computer" className="flex size-9 items-center justify-center text-muted" onClick={() => nav.push({ s: "scan" })}><Plus className="size-5" /></button>}>
      {machines.map((m) => {
        const st = conn[m.id]?.state ?? "offline";
        const online = st === "online";
        const mine = m.id === current;
        const waiting = mine ? Object.values(agents).filter((a) => a.status === "waiting").length : 0;
        const working = mine ? Object.values(agents).filter((a) => a.status === "working").length : 0;
        return (
          <Card key={m.id} onClick={() => open(m.id)} className={online ? "" : "opacity-80"}>
            <div className="flex items-center gap-3">
              {m.os === "macos" || !m.os ? <Laptop className="size-6 text-muted" /> : <Monitor className="size-6 text-muted" />}
              <div className="min-w-0 flex-1">
                <div className="truncate font-semibold">{m.name}</div>
                <div className="truncate text-[11.5px] text-muted">{m.addrs[0] ?? "no address"}{m.appVersion ? ` · DevCat ${m.appVersion}` : ""}</div>
              </div>
              {st === "connecting" ? <Badge>connecting…</Badge> : online ? <Badge tone="ok">● Online</Badge> : <Badge>○ Offline</Badge>}
            </div>
            <div className="mt-2 text-[12px]">
              {online && mine ? (
                <>{waiting > 0 && <span className="text-warn">⚠ {waiting} agent{waiting === 1 ? "" : "s"} need{waiting === 1 ? "s" : ""} you · </span>}<span className="text-muted">{working} working · {terms.length} terminal{terms.length === 1 ? "" : "s"}</span></>
              ) : online ? <span className="text-muted">Tap to open</span> : (
                <span className="text-muted">{conn[m.id]?.error ? <span className="text-danger/90">{conn[m.id]?.error}</span> : m.lastSeen ? `Last seen ${ago(m.lastSeen)}` : "Not connected yet"}</span>
              )}
            </div>
          </Card>
        );
      })}
      <Note>A computer is <b>Offline</b> when DevCat isn't running there, Remote access is off, or this phone has no internet (with the computer's relays off: isn't on the same network). <button className="font-semibold text-accent" onClick={() => nav.push({ s: "help" })}>Help</button></Note>
      {!IS_WEB && <Btn block onClick={() => nav.push({ s: "scan" })}><Smartphone className="size-4" />Pair another computer</Btn>}
    </Screen>
  );
}

const PREFER_APP = "devcat.web.preferApp";

/** Browser only: the QR code opened this page. Pair here, or hand the pairing to the app. */
export function WebLanding({ link }: { link: string }) {
  const [always, setAlways] = useState(false);
  const info = parseLink(link);
  if (!info) return <Screen title="DevCat" back={false}><ErrorBox error="This link has no pairing details. Scan the QR code in DevCat → Mobile on the computer again." /></Screen>;
  const openApp = () => {
    if (always) localStorage.setItem(PREFER_APP, "1");
    location.href = appLink(info);
  };
  return (
    <div className="flex h-full flex-col gap-3 px-5" style={{ paddingTop: "max(36px, env(safe-area-inset-top))", paddingBottom: "max(20px, env(safe-area-inset-bottom))" }}>
      <img src={`${import.meta.env.BASE_URL}icon.png`} alt="" className="size-14 rounded-2xl" />
      <div className="text-[24px] font-bold">Pair with {info.name}</div>
      <div className="text-muted">Control DevCat on this computer from your phone: agents, databases, Redis, VPNs and clusters.</div>
      <Card>
        <div className="font-semibold">Have the DevCat app?</div>
        <div className="mt-1 text-[12.5px] text-muted">The app keeps the connection encrypted on any Wi-Fi and can notify you while open.</div>
        <Btn variant="primary" block className="mt-3" onClick={openApp}>Open in DevCat app</Btn>
        <label className="mt-2 flex items-center gap-2 text-[12.5px] text-muted"><input type="checkbox" checked={always} onChange={(e) => setAlways(e.target.checked)} />Always open pairing links in the app</label>
      </Card>
      <Card>
        <div className="font-semibold">Or use this browser</div>
        <div className="mt-1 text-[12.5px] text-muted">{IS_HOSTED
          ? "Nothing to install, works on any network. Encrypted end to end between this browser and the computer."
          : "Nothing to install. Traffic is encrypted with a key from the QR code; on public Wi-Fi prefer the app."}</div>
        <Btn block className="mt-3" onClick={() => nav.replace({ s: "pairing", link })}>Continue in browser</Btn>
      </Card>
      <div className="flex-1" />
      <div className="text-center text-[11.5px] text-faint">Don't have the app? It's on the DevCat releases page (Android) or built with Xcode (iPhone).</div>
    </div>
  );
}

/** The browser was told to hand pairing links to the app. */
export function prefersApp(): boolean {
  return localStorage.getItem(PREFER_APP) === "1";
}
