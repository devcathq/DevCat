// "📱 Mobile": remote access for the DevCat mobile app (pairing QR, paired phones, port and
// public address), phone notifications for agents (ntfy), the activity log and the setup help.
// Everything here is local to this computer: no DevCat server, no account.
import { openUrl } from "@tauri-apps/plugin-opener";
import clsx from "clsx";
import { Bell, Copy, ExternalLink, Globe, History, LifeBuoy, RefreshCw, Smartphone, Trash2 } from "lucide-react";
import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { toast } from "sonner";
import { Button, Checkbox, Dialog, Field, Input, Select, Spinner } from "../../components/ui";
import { errMsg } from "../../lib/api";
import {
  MOBILE_RELEASES_URL, NTFY_ANDROID_URL, NTFY_IOS_URL, mobileApi, ntfyLink, pathLabel,
  type Access, type AuditEntry, type Device, type NotifySettings, type PairInfo, type RelayChoice, type RemoteSettings,
} from "./api";
import { useMobile, type MobileTab } from "./store";

const TABS: { id: MobileTab; label: string; icon: ReactNode }[] = [
  { id: "remote", label: "Remote access", icon: <Smartphone className="size-3.5" /> },
  { id: "notify", label: "Notifications", icon: <Bell className="size-3.5" /> },
  { id: "activity", label: "Activity", icon: <History className="size-3.5" /> },
  { id: "help", label: "Help", icon: <LifeBuoy className="size-3.5" /> },
];

async function copy(text: string, what: string) {
  try {
    await navigator.clipboard.writeText(text);
    toast.success(`${what} copied`);
  } catch (e) {
    toast.error(errMsg(e));
  }
}

function ago(ms: number): string {
  const s = Math.max(0, Math.round((Date.now() - ms) / 1000));
  if (s < 60) return "just now";
  if (s < 3600) return `${Math.floor(s / 60)} min ago`;
  if (s < 86400) return `${Math.floor(s / 3600)} h ago`;
  return `${Math.floor(s / 86400)} days ago`;
}

function Toggle({ on, onChange, disabled }: { on: boolean; onChange: (v: boolean) => void; disabled?: boolean }) {
  return (
    <button
      role="switch"
      aria-checked={on}
      disabled={disabled}
      onClick={() => onChange(!on)}
      className={clsx("relative h-5 w-9 shrink-0 rounded-full border transition-colors disabled:opacity-50", on ? "border-ok bg-ok" : "border-line bg-hover")}
    >
      <span className={clsx("absolute top-0.5 size-3.5 rounded-full transition-all", on ? "left-[18px] bg-white" : "left-0.5 bg-muted")} />
    </button>
  );
}

function Steps({ children }: { children: ReactNode }) {
  return <ol className="list-decimal space-y-1.5 pl-5 text-[12.5px] text-muted marker:text-accent [&_b]:text-fg">{children}</ol>;
}

function Section({ title, children, right }: { title: string; children: ReactNode; right?: ReactNode }) {
  return (
    <div className="space-y-2">
      <div className="flex items-center gap-2 text-[11px] font-semibold tracking-wider text-faint uppercase">
        {title}<span className="h-px flex-1 bg-line" />{right}
      </div>
      {children}
    </div>
  );
}

function Countdown({ until }: { until: number }) {
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(t);
  }, []);
  const s = Math.max(0, Math.round((until - now) / 1000));
  return <b className="tabular-nums">{Math.floor(s / 60)}:{String(s % 60).padStart(2, "0")}</b>;
}

// ------------------------------------------------------------------ remote access

function RemoteTab() {
  const status = useMobile((s) => s.status);
  const [draft, setDraft] = useState<RemoteSettings | null>(null);
  const [busy, setBusy] = useState(false);
  const [pair, setPair] = useState<PairInfo | null>(null);
  const [devices, setDevices] = useState<Device[]>([]);
  const [qrAddr, setQrAddr] = useState<string | null>(null);

  const loadDevices = useCallback(() => mobileApi.devices().then(setDevices).catch(() => {}), []);
  useEffect(() => { void loadDevices(); }, [loadDevices, status?.clients.length]);
  useEffect(() => { if (status && !draft) setDraft(status.settings); }, [status, draft]);
  // The code was used (a phone paired) or cancelled: hide it. Only on that change, so a status
  // update that arrives just before the new code can't hide it.
  const hadCode = useRef(false);
  useEffect(() => {
    const has = !!status?.pairingExpires;
    if (hadCode.current && !has) setPair(null);
    hadCode.current = has;
  }, [status?.pairingExpires]);
  useEffect(() => {
    if (!pair) return;
    const t = setTimeout(() => setPair(null), Math.max(0, pair.expiresAt - Date.now()));
    return () => clearTimeout(t);
  }, [pair]);

  if (!status || !draft) return <Spinner />;
  const s = status.settings;
  const dirty = draft.port !== s.port || (draft.hostName ?? "") !== (s.hostName ?? "") || (draft.publicAddr ?? "") !== (s.publicAddr ?? "")
    || draft.relay !== s.relay || (draft.relayUrl ?? "") !== (s.relayUrl ?? "");
  const relayOn = s.relay !== "off";

  const save = async (next: RemoteSettings) => {
    setBusy(true);
    try {
      const st = await mobileApi.save(next);
      setDraft(st.settings);
      if (st.settings.enabled && st.running && !s.enabled) {
        setPair(await mobileApi.pairStart(qrAddr));
      } else if (st.settings.enabled && pair && (st.settings.web !== s.web || st.settings.relay !== s.relay || st.settings.relayUrl !== s.relayUrl)) {
        setPair(await mobileApi.pairStart(qrAddr));
      }
      if (!st.settings.enabled) setPair(null);
    } catch (e) {
      toast.error(errMsg(e));
    } finally {
      setBusy(false);
    }
  };
  const newCode = async (addr: string | null = qrAddr) => {
    try { setPair(await mobileApi.pairStart(addr)); } catch (e) { toast.error(errMsg(e)); }
  };
  const browser = !!pair?.webLink || !!pair?.hostedLink;
  const anywhere = !!pair?.hostedLink || (!pair && relayOn);
  const connected = (id: string) => status.clients.some((c) => c.deviceId === id);
  const pathOf = (id: string) => status.clients.find((c) => c.deviceId === id && c.path)?.path;

  return (
    <div className="space-y-5">
      <div className="flex items-center gap-3">
        <Toggle on={s.enabled} disabled={busy} onChange={(on) => void save({ ...s, enabled: on })} />
        <div className="min-w-0 flex-1">
          <div className="font-semibold">{s.enabled ? (status.running ? "On" : "Starting…") : "Off"}</div>
          <div className="truncate text-[12px] text-muted">
            {s.enabled && status.running
              ? <>Phones can reach this computer at <span className="font-mono text-fg">{status.addrs.join(", ") || `port ${s.port}`}</span>{relayOn && <> and from any network through the relay</>}</>
              : "Turn on to control DevCat on this computer from your phone."}
          </div>
          {s.enabled && status.running && relayOn && (
            <div className="truncate text-[11.5px]">
              {status.relayConnected
                ? <span className="text-ok">● Relay connected <span className="font-mono text-muted">{status.relayConnected.replace(/^https?:\/\//, "").replace(/\/$/, "")}</span></span>
                : <span className="text-warn">○ Connecting to the relay… (needs internet)</span>}
            </div>
          )}
        </div>
        {busy && <Spinner />}
      </div>
      {status.error && <div className="rounded-md border border-danger/40 bg-danger/10 px-3 py-2 text-[12px] text-danger">{status.error}</div>}

      {s.enabled && status.running && (
        <Section title="Pair a phone">
          <div className="flex gap-4">
            <div className="flex size-[168px] shrink-0 items-center justify-center overflow-hidden rounded-lg bg-white">
              {pair ? <div className="size-full [&>svg]:size-full" dangerouslySetInnerHTML={{ __html: pair.svg }} /> : (
                <Button variant="primary" onClick={() => void newCode()}>Show pairing code</Button>
              )}
            </div>
            <div className="min-w-0 flex-1 space-y-3">
              {browser || (!pair && s.web && status.webRunning) ? (
                <Steps>
                  {anywhere ? <li>Any network works: Wi-Fi or mobile data</li> : <li>Join <b>the same Wi-Fi</b> as this computer, or set a public address below</li>}
                  <li>Point the phone's <b>camera</b> at this code and open the link{pair && <>. It works once and expires in <Countdown until={pair.expiresAt} /></>}</li>
                  <li>DevCat opens in the phone's browser: tap <b>Open in DevCat app</b> if you have the app (<button className="text-accent hover:underline" onClick={() => void openUrl(MOBILE_RELEASES_URL)}>get it</button>), or <b>Continue in browser</b></li>
                  <li>Click <b>Allow</b> when DevCat asks here</li>
                </Steps>
              ) : (
                <Steps>
                  <li>Install <b>DevCat Mobile</b> on your phone (<button className="text-accent hover:underline" onClick={() => void openUrl(MOBILE_RELEASES_URL)}>Android APK · iPhone</button>)</li>
                  {anywhere ? <li>Any network works: Wi-Fi or mobile data</li> : <li>Join <b>the same Wi-Fi</b> as this computer, or set a public address below</li>}
                  <li>In the app tap <b>Scan QR code</b> and scan this code{pair && <>. It works once and expires in <Countdown until={pair.expiresAt} /></>}</li>
                  <li>Click <b>Allow</b> when DevCat asks here</li>
                </Steps>
              )}
              {pair && (
                <div className="flex flex-wrap items-center gap-2">
                  <Button size="sm" onClick={() => void copy(pair.link, "Pairing link")}><Copy className="size-3" />Copy link</Button>
                  {browser && <Button size="sm" variant="ghost" onClick={() => void copy(pair.appLink, "App link")} title="For pasting into the app">Copy app link</Button>}
                  {pair.hostedLink && pair.webLink && <Button size="sm" variant="ghost" onClick={() => void copy(pair.webLink!, "Same-Wi-Fi link")} title="Opens the page served by this computer: faster, but only on the same network">Copy same-Wi-Fi link</Button>}
                  <Button size="sm" onClick={() => void newCode()}><RefreshCw className="size-3" />New code</Button>
                  <span className="text-[11px] text-faint">Code <span className="font-mono text-muted">{pair.code}</span></span>
                </div>
              )}
              {pair && browser && !pair.hostedLink && status.addrs.length > 1 && (
                <label className="flex items-center gap-2 text-[11.5px] text-muted">
                  Address in the code
                  <Select className="w-56" value={qrAddr ?? status.addrs[0]} onChange={(e) => { setQrAddr(e.target.value); void newCode(e.target.value); }}>
                    {status.addrs.map((a) => <option key={a} value={a}>{a}</option>)}
                  </Select>
                </label>
              )}
            </div>
          </div>
        </Section>
      )}

      <Section title="Paired phones">
        {devices.length === 0 ? (
          <div className="text-[12px] text-faint">No phone paired yet.</div>
        ) : (
          <div className="space-y-1.5">
            {devices.map((d) => (
              <div key={d.id} className="flex items-center gap-3 rounded-md border border-line bg-raised px-3 py-2">
                {d.kind !== "app" ? <Globe className="size-4 shrink-0 text-muted" /> : <Smartphone className="size-4 shrink-0 text-muted" />}
                <div className="min-w-0 flex-1">
                  <div className="truncate font-medium">{d.name}</div>
                  <div className="truncate text-[11px] text-muted">
                    {d.kind !== "app" ? "browser" : "app"} · {d.platform || "phone"} · paired {new Date(d.pairedAt).toLocaleDateString()} ·{" "}
                    {connected(d.id) ? (
                      <span className="text-ok" title={pathOf(d.id)?.direct === false ? "The phone and this computer couldn't reach each other directly, so the traffic (end-to-end encrypted) goes through the relay. DevCat keeps trying to switch to direct." : undefined}>
                        ● connected now{pathOf(d.id) ? ` · ${pathLabel(pathOf(d.id)!)}` : ""}
                      </span>
                    ) : d.lastSeen ? `last seen ${ago(d.lastSeen)}` : "never connected"}
                  </div>
                </div>
                <Select
                  className="w-32"
                  value={d.access}
                  onChange={(e) => void mobileApi.deviceUpdate(d.id, { access: e.target.value as Access }).then(setDevices).catch((er) => toast.error(errMsg(er)))}
                  title="Read-only phones can browse and run SELECT-style queries, but can't change data, VPNs, clusters or terminals."
                >
                  <option value="full">Full access</option>
                  <option value="readonly">Read-only</option>
                </Select>
                <Button size="sm" variant="danger" onClick={() => void mobileApi.deviceRevoke(d.id).then(setDevices).catch((er) => toast.error(errMsg(er)))}>Revoke</Button>
              </div>
            ))}
          </div>
        )}
      </Section>

      <Section title="Connection settings">
        <div className="flex items-start gap-3">
          <Toggle on={s.web} disabled={busy} onChange={(on) => void save({ ...s, web: on })} />
          <div className="min-w-0 flex-1 text-[12.5px]">
            <div className="font-medium">Allow phone browsers</div>
            <div className="text-[11.5px] text-muted">
              The QR code opens DevCat in the phone's browser, so no app is needed. Its traffic is encrypted with a key from the code, but the page itself
              comes over plain HTTP: on Wi-Fi you don't trust, use the app. Off: only the app can pair.
            </div>
            {s.web && status.webError && <div className="mt-1 text-[11.5px] text-danger">{status.webError}</div>}
            {s.web && !status.webAvailable && <div className="mt-1 text-[11.5px] text-warn">This build of DevCat doesn't include the browser version; the QR code is for the app.</div>}
          </div>
        </div>
        <div className="grid grid-cols-2 gap-3">
          <Field label="Name phones see" hint="Default: this computer's name">
            <Input value={draft.hostName ?? ""} placeholder={status.hostName} onChange={(e) => setDraft({ ...draft, hostName: e.target.value })} />
          </Field>
          <Field label="Port" hint="UDP for the app, TCP for browsers">
            <Input type="number" min={1024} max={65535} value={draft.port} onChange={(e) => setDraft({ ...draft, port: Number(e.target.value) || 0 })} />
          </Field>
          <Field className="col-span-2" label="Reach from other networks"
            hint={draft.relay === "public"
              ? "Phones on mobile data or another Wi-Fi connect through iroh's free public relays (like RustDesk's public servers), switching to a direct path when they can. Relays only pass along end-to-end encrypted traffic; they can't read it."
              : draft.relay === "custom"
                ? "Your own iroh-relay server (one small program or Docker container on any server with a public address). Nothing goes through third parties."
                : "Only the same network, or the public address below."}>
            <div className="flex gap-2">
              <Select className="w-64" value={draft.relay} onChange={(e) => setDraft({ ...draft, relay: e.target.value as RelayChoice })}>
                <option value="public">Public relays (recommended)</option>
                <option value="custom">My own relay</option>
                <option value="off">Off: same network only</option>
              </Select>
              {draft.relay === "custom" && <Input className="flex-1 font-mono" value={draft.relayUrl ?? ""} placeholder="https://relay.example.com" onChange={(e) => setDraft({ ...draft, relayUrl: e.target.value })} />}
            </div>
          </Field>
          <Field className="col-span-2" label="Public address (optional)" hint={`Without a relay: a host name or IP that reaches this computer. Your router must forward port ${draft.port} (UDP for the app, TCP for browsers) to it.`}>
            <Input value={draft.publicAddr ?? ""} placeholder="e.g. devbox.example.com or 203.0.113.40" onChange={(e) => setDraft({ ...draft, publicAddr: e.target.value })} />
          </Field>
        </div>
        {dirty && (
          <div className="flex justify-end gap-2">
            <Button variant="ghost" onClick={() => setDraft(s)}>Discard</Button>
            <Button variant="primary" loading={busy} onClick={() => void save({ ...draft, enabled: s.enabled })}>Save</Button>
          </div>
        )}
      </Section>

      <Section title="Identity">
        <div className="flex items-center gap-3 text-[12px] text-muted">
          <span className="min-w-0 flex-1 truncate">This computer's key: <span className="font-mono">{status.id ? `${status.id.slice(0, 8)}…${status.id.slice(-8)}` : "created when turned on"}</span></span>
          <Button size="sm" variant="ghost" title="New key: every paired phone must pair again" onClick={() => void mobileApi.resetIdentity().then(() => { void loadDevices(); setPair(null); toast.success("New identity: pair your phones again"); }).catch((e) => toast.error(errMsg(e)))}>
            <Trash2 className="size-3" />Reset and unpair all
          </Button>
        </div>
      </Section>
    </div>
  );
}

// ------------------------------------------------------------------ notifications

function NotifyTab() {
  const [s, setS] = useState<NotifySettings | null>(null);
  const [qr, setQr] = useState<string | null>(null);
  const [testing, setTesting] = useState(false);
  useEffect(() => { mobileApi.notifySettings().then(setS).catch((e) => toast.error(errMsg(e))); }, []);
  const link = s ? ntfyLink(s) : "";
  useEffect(() => { if (link) mobileApi.qr(link).then(setQr).catch(() => setQr(null)); }, [link]);
  if (!s) return <Spinner />;

  const save = async (next: NotifySettings) => {
    setS(next);
    try { setS(await mobileApi.notifySave(next)); } catch (e) { toast.error(errMsg(e)); }
  };
  const test = async () => {
    setTesting(true);
    try {
      await mobileApi.notifyTest();
      toast.success("Test sent. It should arrive on your phone in a few seconds.");
    } catch (e) {
      toast.error(errMsg(e));
    } finally {
      setTesting(false);
    }
  };

  return (
    <div className="space-y-5">
      <div className="flex items-center gap-3">
        <Toggle on={s.enabled} onChange={(on) => void save({ ...s, enabled: on })} />
        <div className="flex-1">
          <div className="font-semibold">Notify my phone when an agent finishes or needs me</div>
          <div className="text-[12px] text-muted">Works even when the DevCat app on the phone is closed. Uses the free <b className="text-fg">ntfy</b> app; no account needed.</div>
        </div>
      </div>

      <Section title="Set up (once)">
        <div className="flex gap-4">
          <div className="flex size-[150px] shrink-0 items-center justify-center overflow-hidden rounded-lg bg-white">
            {qr ? <div className="size-full [&>svg]:size-full" dangerouslySetInnerHTML={{ __html: qr }} /> : <Spinner />}
          </div>
          <div className="min-w-0 flex-1 space-y-2">
            <Steps>
              <li>Install <b>ntfy</b>: <button className="text-accent hover:underline" onClick={() => void openUrl(NTFY_IOS_URL)}>App Store</button> · <button className="text-accent hover:underline" onClick={() => void openUrl(NTFY_ANDROID_URL)}>Google Play</button></li>
              <li>In ntfy tap <b>+</b> and subscribe to the topic <span className="font-mono text-fg">{s.topic}</span>{s.server !== "https://ntfy.sh" && <> on server <span className="font-mono text-fg">{s.server}</span></>}. Or scan this code with the camera.</li>
              <li><b>Send a test</b> and check that it arrives</li>
            </Steps>
            <div className="flex flex-wrap gap-2">
              <Button size="sm" onClick={() => void copy(s.topic, "Topic")}><Copy className="size-3" />Copy topic</Button>
              <Button size="sm" variant="primary" loading={testing} onClick={() => void test()}>Send a test</Button>
            </div>
          </div>
        </div>
        <div className="text-[11px] text-faint">The topic name is the only key: anyone who knows it can read these notifications. They only contain the project name and the status, never code or data.</div>
      </Section>

      <Section title="Notify me when">
        <div className="flex flex-col items-start gap-2">
          <Checkbox label="An agent finished" checked={s.onDone} onChange={(v) => void save({ ...s, onDone: v })} />
          <label className="ml-6 flex items-center gap-2 text-[12px] text-muted">
            only if it ran at least
            <Input type="number" min={0} className="w-16" value={s.minSeconds} onChange={(e) => setS({ ...s, minSeconds: Math.max(0, Number(e.target.value) || 0) })} onBlur={() => void save(s)} />
            seconds
          </label>
          <Checkbox label="An agent needs input (permission prompts, questions)" checked={s.onInput} onChange={(v) => void save({ ...s, onInput: v })} />
          <Checkbox label="Only when I'm away (not while DevCat is the active app)" checked={s.onlyWhenAway} onChange={(v) => void save({ ...s, onlyWhenAway: v })} />
        </div>
      </Section>

      <Section title="Server">
        <div className="grid grid-cols-2 gap-3">
          <Field label="ntfy server" hint="Self-hosted ntfy works too">
            <Input value={s.server} onChange={(e) => setS({ ...s, server: e.target.value })} onBlur={() => void save(s)} />
          </Field>
          <Field label="Access token (optional)" hint="For a protected self-hosted server">
            <Input type="password" value={s.token ?? ""} onChange={(e) => setS({ ...s, token: e.target.value })} onBlur={() => void save(s)} />
          </Field>
        </div>
        <Button size="sm" variant="ghost" onClick={() => void mobileApi.notifyNewTopic().then(setS).catch((e) => toast.error(errMsg(e)))}>
          <RefreshCw className="size-3" />New private topic (subscribe again on the phone)
        </Button>
        <div className="text-[11px] text-faint">Agent status comes from Claude Code hooks DevCat adds to the terminals it starts (macOS and Linux). Agents started before turning this on report after their next restart.</div>
      </Section>
    </div>
  );
}

// ------------------------------------------------------------------ activity

function ActivityTab() {
  const [rows, setRows] = useState<AuditEntry[] | null>(null);
  const load = useCallback(() => mobileApi.audit(300).then(setRows).catch((e) => toast.error(errMsg(e))), []);
  useEffect(() => { void load(); }, [load]);
  if (!rows) return <Spinner />;
  return (
    <div className="space-y-2">
      <div className="flex items-center text-[12px] text-muted">
        <span className="flex-1">Everything a phone changed or read on this computer, newest first. Keystrokes typed into terminals aren't logged.</span>
        <Button size="sm" variant="ghost" onClick={() => void load()}><RefreshCw className="size-3" />Refresh</Button>
      </div>
      {rows.length === 0 ? <div className="py-6 text-center text-[12px] text-faint">Nothing yet.</div> : (
        <div className="divide-y divide-line rounded-md border border-line">
          {rows.map((r, i) => (
            <div key={i} className="flex items-start gap-3 px-3 py-1.5 text-[12px]">
              <span className={r.ok ? "text-ok" : "text-danger"}>{r.ok ? "✓" : "✕"}</span>
              <div className="min-w-0 flex-1">
                <span className="font-mono text-fg">{r.action}</span>
                {r.detail && <span className="ml-2 break-all text-muted">{r.detail}</span>}
              </div>
              <span className="shrink-0 text-faint">{r.deviceName} · {new Date(r.at).toLocaleString()}</span>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

// ------------------------------------------------------------------ help

function HelpTab() {
  const Item = ({ q, children }: { q: string; children: ReactNode }) => (
    <div className="rounded-md border border-line bg-raised px-3 py-2.5">
      <div className="mb-1 font-semibold">{q}</div>
      <div className="space-y-1 text-[12.5px] text-muted [&_b]:text-fg">{children}</div>
    </div>
  );
  return (
    <div className="space-y-2.5">
      <Item q="How does it work?">
        <p>The phone talks to DevCat on this computer, encrypted end to end with keys made during pairing. There's no DevCat server or account. Your connections, passwords and VPNs stay here; the phone only sends requests and shows the answers.</p>
        <p>On the same network it connects <b>directly</b>. From elsewhere it goes through a <b>relay</b> (iroh's public relays, or your own), which only passes along encrypted traffic, and switches to a direct path when the networks allow it.</p>
      </Item>
      <Item q="The phone says the computer is offline">
        <ul className="list-disc space-y-0.5 pl-4">
          <li>DevCat is running here and <b>Remote access</b> is on</li>
          <li>With relays on (Connection settings), both need internet; otherwise both on the <b>same Wi-Fi</b> (guest networks often keep devices apart)</li>
          <li>The QR code holds one of this computer's addresses; if it has several (Wi-Fi and Ethernet), pick the one the phone can reach under <b>Address in the code</b></li>
          <li>This computer isn't asleep: turn on <b>Keep awake</b> (☕ in the title bar)</li>
          <li>macOS firewall: allow DevCat to accept incoming connections if it asks</li>
          <li>VPN on the phone: some VPNs block the local network; turn it off or allow LAN access</li>
        </ul>
      </Item>
      <Item q="Reaching this computer from outside (4G, another network)">
        <p>Leave <b>Reach from other networks</b> on <b>Public relays</b> (the default) and pair: it works from anywhere, nothing to set up. Prefer no third party? Run your own <button className="text-accent hover:underline" onClick={() => void openUrl("https://github.com/n0-computer/iroh/tree/main/iroh-relay")}>iroh-relay</button> and choose <b>My own relay</b>.</p>
        <p>Or, without relays, a public address: on your router forward the <b>port</b> (Remote access → Connection settings; UDP for the app, TCP for browsers) to this computer, and enter your public IP or host name as <b>Public address</b>. Then pair again (or add the address in the app). Only paired phones get in; everyone else is refused.</p>
      </Item>
      <Item q="App or browser?">
        <p>The QR code opens DevCat in the phone's <b>browser</b>: nothing to install, works on any phone. Traffic is encrypted with a key from the code, but the page itself comes over plain HTTP, so someone who controls the Wi-Fi could tamper with it. The <b>app</b> doesn't have that gap and scans codes itself; on the landing page tap <b>Open in DevCat app</b> to pair the app instead. Turn <b>Allow phone browsers</b> off to require the app.</p>
      </Item>
      <Item q="Installing the app">
        <p><b>Android</b>: download the APK from the GitHub release and open it (allow "install unknown apps" once). <b>iPhone</b>: install from TestFlight when available, or build it from the repository with Xcode (see <span className="font-mono">mobile/README.md</span>).</p>
      </Item>
      <Item q="What can a phone do?">
        <p><b>Full access</b>: everything in the app: agents and terminals, queries and edits, Redis keys, VPNs, Kubernetes actions. <b>Read-only</b>: browse, read rows and keys, and run queries that only read. Change it any time under Paired phones; <b>Revoke</b> cuts a phone off immediately.</p>
      </Item>
      <Item q="Lost a phone?">
        <p>Click <b>Revoke</b> next to it. To be extra safe, use <b>Reset and unpair all</b>: this computer gets a new key and every phone must pair again.</p>
      </Item>
    </div>
  );
}

// ------------------------------------------------------------------ dialog

export function MobileDialog() {
  const { open, tab, show, close } = useMobile();
  return (
    <Dialog open={open} onClose={close} title={<span className="flex items-center gap-2"><Smartphone className="size-4" />Mobile</span>} width={760}>
      <div className="-mt-1 mb-4 flex gap-1 border-b border-line">
        {TABS.map((t) => (
          <button
            key={t.id}
            onClick={() => show(t.id)}
            className={clsx("-mb-px flex items-center gap-1.5 border-b-2 px-3 py-2 text-[12.5px]", tab === t.id ? "border-accent text-fg" : "border-transparent text-muted hover:text-fg")}
          >
            {t.icon}{t.label}
          </button>
        ))}
        <button className="ml-auto flex items-center gap-1 px-2 text-[11px] text-faint hover:text-muted" onClick={() => void openUrl(MOBILE_RELEASES_URL)}>
          Get the app<ExternalLink className="size-3" />
        </button>
      </div>
      {tab === "remote" ? <RemoteTab /> : tab === "notify" ? <NotifyTab /> : tab === "activity" ? <ActivityTab /> : <HelpTab />}
    </Dialog>
  );
}
