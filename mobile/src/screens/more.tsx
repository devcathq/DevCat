// More: this computer (address, access, keep awake), notifications setup, the activity log,
// help, renaming this phone and unpairing.
import { openUrl } from "@tauri-apps/plugin-opener";
import { Bell, ChevronRight, Coffee, Copy, HelpCircle, History, Laptop, LogOut, Pencil, Repeat, Server } from "lucide-react";
import { useEffect, useState, type ReactNode } from "react";
import { IS_WEB } from "../lib/env";
import { phone } from "../lib/rpc";
import { currentMachine, host, loadMachines, nav, toast, toastErr, useApp } from "../lib/store";
import type { AuditEntry, HostInfo, NotifySettings } from "../lib/types";
import { Badge, Btn, Card, Confirm, Empty, ErrorBox, Field, Input, Item, Loading, Note, Screen, Section, Toggle, TextArea, ago, useLoad } from "../ui";

const NTFY_IOS = "https://apps.apple.com/app/ntfy/id1625396347";
const NTFY_ANDROID = "https://play.google.com/store/apps/details?id=io.heckel.ntfy";

export function More() {
  const machine = useApp((s) => s.machines.find((m) => m.id === s.current));
  const info = useApp((s) => s.info);
  const awake = useLoad(() => host<{ on: boolean; until?: number | null }>("keep_awake_status"), [machine?.id]);
  const [unpair, setUnpair] = useState(false);
  const readonly = machine?.access === "readonly";
  const setAwake = async (on: boolean) => {
    try { await host("keep_awake_set", { on, minutes: null }); awake.reload(); } catch (e) { toastErr(e); }
  };
  const forget = async () => {
    if (!machine) return;
    await phone.forget(machine.id);
    const rest = await loadMachines();
    toast(`Unpaired from ${machine.name}. Also click Revoke on the computer to remove this phone there.`, "info");
    nav.reset(rest.length ? [{ s: "computers" }] : [{ s: "welcome" }]);
  };
  return (
    <Screen title="More" tab="more">
      {machine && (
        <Card onClick={() => nav.push({ s: "machine" })}>
          <div className="flex items-center gap-3">
            <Laptop className="size-6 text-muted" />
            <div className="min-w-0 flex-1">
              <div className="truncate font-semibold">{machine.name}</div>
              <div className="truncate text-[11.5px] text-muted">DevCat {machine.appVersion || "?"} · {machine.os || "computer"} · {machine.addrs[0] ?? ""}</div>
            </div>
            <Badge tone={readonly ? "default" : "ok"}>{readonly ? "Read-only" : "Full access"}</Badge>
          </div>
        </Card>
      )}
      <Item icon={<Bell className="size-5" />} title="Notifications" meta="Push when an agent finishes or needs you" onClick={() => nav.push({ s: "notify" })} chevron />
      <Item icon={<Coffee className="size-5" />} title="Keep computer awake" meta="So it stays reachable" right={<Toggle on={!!awake.data?.on} disabled={readonly || awake.loading} onChange={(v) => void setAwake(v)} />} />
      <Item icon={<History className="size-5" />} title="Activity log" meta="What this phone did on the computer" onClick={() => nav.push({ s: "activity" })} chevron />
      <Item icon={<HelpCircle className="size-5" />} title="Help & troubleshooting" onClick={() => nav.push({ s: "help" })} chevron />
      <Item icon={<Repeat className="size-5" />} title="Switch computer" onClick={() => nav.reset([{ s: "computers" }])} chevron />
      <Item icon={<Server className="size-5" />} title="Computer settings" meta="Addresses, name" onClick={() => nav.push({ s: "machine" })} chevron />
      <Section>This phone</Section>
      <PhoneName />
      {IS_WEB && <OtherBrowsers />}
      <div className="text-[11px] text-faint">DevCat Mobile {info?.version} · {info?.platform} · key {info?.deviceId.slice(0, 8)}…</div>
      <Btn variant="danger" block onClick={() => setUnpair(true)}><LogOut className="size-4" />Unpair this computer</Btn>
      <Confirm open={unpair} danger title={`Unpair ${machine?.name}?`} confirm="Unpair" onCancel={() => setUnpair(false)} onConfirm={() => { setUnpair(false); void forget(); }}>
        This phone forgets the computer. To pair again, scan a new code. On the computer, also click <b>Revoke</b> next to this phone.
      </Confirm>
    </Screen>
  );
}

/** Opened from the Home Screen icon (its own window, no address bar). */
const standalone = () => matchMedia("(display-mode: standalone)").matches || (navigator as { standalone?: boolean }).standalone === true;

/** The address carries this browser's pairing (web/client.ts carryPairing). */
function OtherBrowsers() {
  const installed = standalone();
  const ios = /iphone|ipad|ipod/i.test(navigator.userAgent);
  const copy = async () => {
    try { await navigator.clipboard.writeText(location.href); toast("Copied. Open it in another browser on this phone.", "ok"); }
    catch { toast(installed ? "Couldn't copy here. Open DevCat in Safari to copy the link." : "Copy the address from the address bar instead.", "info"); }
  };
  return (
    <>
      {installed ? (
        <Note>Installed on the Home Screen: DevCat opens in its own window and stays paired.</Note>
      ) : (
        <Note>
          <b>Install it as an app:</b> {ios ? <>in Safari tap <b>Share</b> → <b>Add to Home Screen</b> (from the QR code's preview, first tap <b>Open in Safari</b>)</> : <>in the browser menu tap <b>Add to Home screen</b> / <b>Install app</b></>}.
          It opens full-screen with its own App Switcher card and stays paired.
        </Note>
      )}
      <Note>This page's address carries the pairing, so it also works in another browser or as a bookmark. Keep it private, like a password.</Note>
      <Btn block onClick={() => void copy()}><Copy className="size-4" />Copy link for another browser</Btn>
    </>
  );
}

function PhoneName() {
  const info = useApp((s) => s.info);
  const [name, setName] = useState(info?.deviceName ?? "");
  useEffect(() => { setName(info?.deviceName ?? ""); }, [info?.deviceName]);
  const dirty = name.trim() && name.trim() !== info?.deviceName;
  const save = async () => {
    try {
      await phone.setName(name.trim());
      useApp.setState({ info: info ? { ...info, deviceName: name.trim() } : info });
      toast("Saved. Computers see the new name next time this phone connects.", "ok");
    } catch (e) { toastErr(e); }
  };
  return (
    <div className="flex items-end gap-2">
      <Field label="Name shown on computers"><Input value={name} onChange={(e) => setName(e.target.value)} /></Field>
      {dirty && <Btn variant="primary" onClick={() => void save()}><Pencil className="size-4" />Save</Btn>}
    </div>
  );
}

export function MachineSettings() {
  const m = currentMachine();
  const hostInfo = useLoad(() => host<HostInfo>("host_info"), [m?.id]);
  const path = useApp((s) => (m ? s.conn[m.id]?.path : null));
  const [addrs, setAddrs] = useState((m?.manualAddrs ?? []).join("\n"));
  if (!m) return <Screen title="Computer"><Empty title="No computer selected" /></Screen>;
  const save = async () => {
    try {
      await phone.setAddrs(m.id, addrs.split(/[\s,]+/).filter(Boolean));
      await loadMachines();
      toast("Saved", "ok");
    } catch (e) { toastErr(e); }
  };
  return (
    <Screen title={m.name} sub="Computer settings">
      <Section>Addresses this phone tries</Section>
      {m.addrs.map((a) => <div key={a} className="font-mono text-[12.5px]">{a}{m.manualAddrs.includes(a) ? <span className="text-faint"> · yours</span> : ""}</div>)}
      {hostInfo.data && <div className="text-[11.5px] text-muted">The computer reports: {hostInfo.data.addrs.join(", ") || "none"}</div>}
      <Field label="Your own addresses (tried first)" hint="One per line, host:port. Use this for a public address, e.g. devbox.example.com:47100">
        <TextArea rows={3} className="font-mono text-[12.5px]" value={addrs} onChange={(e) => setAddrs(e.target.value)} />
      </Field>
      <Btn variant="primary" block onClick={() => void save()}>Save addresses</Btn>
      <Section>Details</Section>
      <div className="grid grid-cols-[110px_1fr] gap-y-1 text-[12.5px]">
        <span className="text-muted">Paired</span><span>{new Date(m.addedAt).toLocaleString()}</span>
        <span className="text-muted">Last seen</span><span>{m.lastSeen ? ago(m.lastSeen) : "—"}</span>
        <span className="text-muted">Access</span><span>{m.access === "readonly" ? "Read-only" : "Full"} (change on the computer)</span>
        {path && <><span className="text-muted">Connection</span><span>{path.direct ? "Direct" : <>Through the relay <span className="font-mono text-[11.5px]">{(path.relay ?? "").replace(/^https?:\/\//, "").replace(/\/$/, "")}</span></>} · {path.rttMs} ms</span></>}
        <span className="text-muted">Other networks</span><span className="break-all">{!m.relay ? "Off (same network only)" : m.relay === "public" ? "Through public relays" : <>Through <span className="font-mono text-[11.5px]">{m.relay}</span></>}</span>
        <span className="text-muted">Key</span><span className="font-mono text-[11px] break-all">{m.id}</span>
      </div>
    </Screen>
  );
}

function Step({ n, title, meta, right }: { n: number; title: ReactNode; meta?: ReactNode; right?: ReactNode }) {
  return (
    <div className="flex items-center gap-3 rounded-xl bg-raised px-3 py-2.5">
      <span className="flex size-6 shrink-0 items-center justify-center rounded-full bg-accent/15 text-[12px] font-bold text-accent">{n}</span>
      <span className="min-w-0 flex-1"><span className="block font-semibold">{title}</span>{meta && <span className="block truncate text-[11.5px] text-muted">{meta}</span>}</span>
      {right}
    </div>
  );
}

export function Notify() {
  const s = useLoad(() => host<NotifySettings>("notify_settings"), []);
  const [v, setV] = useState<NotifySettings | null>(null);
  const [tested, setTested] = useState<string | null>(null);
  const readonly = currentMachine()?.access === "readonly";
  const isIos = /iphone|ipad/i.test(navigator.userAgent);
  useEffect(() => { if (s.data) setV(s.data); }, [s.data]);
  if (!v) return <Screen title="Notifications">{s.error ? <ErrorBox error={s.error} /> : <Loading />}</Screen>;
  const save = async (next: NotifySettings) => {
    setV(next);
    try { setV(await host<NotifySettings>("notify_save", { settings: next })); } catch (e) { toastErr(e); }
  };
  const topicUrl = `${v.server.replace(/\/+$/, "")}/${v.topic}`;
  const subscribe = () => {
    const host_ = v.server.replace(/^https?:\/\//, "").replace(/\/+$/, "");
    // The ntfy app handles ntfy:// links; the web page offers to open the app otherwise.
    void openUrl(`ntfy://${host_}/${v.topic}`).catch(() => openUrl(topicUrl));
  };
  const test = async () => {
    try { await host("notify_test"); setTested(new Date().toLocaleTimeString()); } catch (e) { toastErr(e); }
  };
  return (
    <Screen title="Notifications">
      <div className="text-[13px] text-muted">Get a push when an agent finishes or needs you, even when this app is closed. DevCat on the computer sends it through the free <b className="text-fg">ntfy</b> app. No account needed.</div>
      <Item title="Notify this phone" meta={v.enabled ? "On" : "Off"} right={<Toggle on={v.enabled} disabled={readonly} onChange={(on) => void save({ ...v, enabled: on })} />} />
      <Section>Set up (once)</Section>
      <Step n={1} title="Install ntfy" meta="free, open source" right={<Btn size="sm" onClick={() => void openUrl(isIos ? NTFY_IOS : NTFY_ANDROID)}>{isIos ? "App Store" : "Google Play"}</Btn>} />
      <Step n={2} title="Subscribe to your private topic" meta={<span className="font-mono">{v.topic}</span>} right={<Btn size="sm" variant="primary" onClick={subscribe}>Subscribe</Btn>} />
      <Step n={3} title="Send a test" meta={tested ? <span className="text-ok">✓ Sent {tested}. Did it arrive?</span> : "Check that it arrives"} right={<Btn size="sm" disabled={readonly} onClick={() => void test()}>Test</Btn>} />
      {!isIos && <Note>Android: in ntfy, allow notifications and turn off battery optimization for ntfy so pushes arrive right away.</Note>}
      <Section>Notify me when</Section>
      <Item title="An agent finished" meta={`after running at least ${v.minSeconds} s`} right={<Toggle on={v.onDone} disabled={readonly} onChange={(x) => void save({ ...v, onDone: x })} />} />
      <Item title="An agent needs input" meta="permission prompts, questions" right={<Toggle on={v.onInput} disabled={readonly} onChange={(x) => void save({ ...v, onInput: x })} />} />
      <Item title="Only when I'm away" meta="not while DevCat is the active app on the computer" right={<Toggle on={v.onlyWhenAway} disabled={readonly} onChange={(x) => void save({ ...v, onlyWhenAway: x })} />} />
      <Note>Notifications contain only the project name and status, never code or data. The topic name is the key: keep it private. Server: <b>{v.server}</b> (change it on the computer).</Note>
    </Screen>
  );
}

export function Activity() {
  const log = useLoad(() => host<AuditEntry[]>("audit", { limit: 200 }), []);
  return (
    <Screen title="Activity log" sub="Kept on the computer">
      {log.loading ? <Loading /> : log.error ? <ErrorBox error={log.error} onRetry={log.reload} /> : (log.data ?? []).length === 0 ? <Empty title="Nothing yet" /> : log.data!.map((e, i) => (
        <Item key={i} icon={<span className={e.ok ? "text-ok" : "text-danger"}>{e.ok ? "✓" : "✕"}</span>} title={<span className="font-mono text-[12.5px]">{e.action}</span>}
          meta={`${e.detail ? `${e.detail} · ` : ""}${ago(e.at)}`} />
      ))}
      <Note>Every change made from a phone is logged on the computer (DevCat → Mobile → Activity). Keystrokes typed into terminals are not logged.</Note>
    </Screen>
  );
}

function Q({ q, children }: { q: string; children: ReactNode }) {
  const [open, setOpen] = useState(false);
  return (
    <Card onClick={() => setOpen(!open)}>
      <div className="flex items-center gap-2"><b className="flex-1">{q}</b><ChevronRight className={`size-4 text-faint transition-transform ${open ? "rotate-90" : ""}`} /></div>
      {open && <div className="mt-2 space-y-1.5 text-[12.5px] text-muted [&_b]:text-fg">{children}</div>}
    </Card>
  );
}

export function Help() {
  return (
    <Screen title="Help & troubleshooting">
      <Q q="How does it work?">
        <p>This app talks to DevCat on your computer, encrypted end to end with keys made when you paired. There's no DevCat server or account. Connections, passwords and VPNs stay on the computer; the phone sends requests and shows the answers.</p>
        <p>On the same network it connects <b>directly</b>. Elsewhere it goes through the computer's <b>relay</b>, which only passes along encrypted traffic, and switches to a direct path when the networks allow it.</p>
      </Q>
      <Q q="Setting up the computer">
        <p>1. Open DevCat on the computer. 2. Click the <b>📱</b> icon in the title bar (next to ☕). 3. Turn <b>Remote access</b> on. 4. Click <b>Show pairing code</b> and scan it here. 5. Click <b>Allow</b> on the computer.</p>
      </Q>
      <Q q="The computer shows Offline">
        <ul className="list-disc space-y-1 pl-4">
          <li>DevCat is open on the computer and Remote access is <b>on</b></li>
          <li>This phone has internet. If the computer's relays are off (DevCat → Mobile → Reach from other networks), both must be on the <b>same Wi-Fi</b>; guest networks often keep devices apart</li>
          <li>The computer isn't asleep: turn on <b>Keep awake</b> (More tab, or ☕ on the computer)</li>
          <li>macOS asked whether DevCat may accept incoming connections: click <b>Allow</b> (System Settings → Network → Firewall)</li>
          <li>A VPN on this phone can block the local network: pause it or allow LAN access</li>
          <li>iPhone: allow <b>Local Network</b> for DevCat in Settings → Privacy & Security → Local Network</li>
        </ul>
      </Q>
      <Q q="Reaching the computer from outside (4G, another network)">
        <p>Works out of the box: DevCat → Mobile → <b>Reach from other networks</b> is on <b>Public relays</b> by default. If it was off when you paired, turn it on and pair again.</p>
        <p>Without relays the computer needs a public address. On your router, forward the <b>UDP port</b> shown in DevCat → Mobile (47100 by default) to the computer, and enter your public IP or host name as <b>Public address</b> there. Then add it here under More → Computer settings → Your own addresses. Only paired phones get in.</p>
      </Q>
      <Q q="Pairing fails">
        <p>The code works <b>once</b> and expires after <b>5 minutes</b>. On the computer click <b>New code</b> and scan again. Five wrong tries cancel the code.</p>
      </Q>
      <Q q="Notifications don't arrive">
        <p>Install <b>ntfy</b>, subscribe to the topic shown under More → Notifications, and send a test. On Android, turn off battery optimization for ntfy. Notifications are skipped while you're using DevCat on the computer if "Only when I'm away" is on. They need Claude Code agents started from DevCat (terminals opened before this was set up report after a restart).</p>
      </Q>
      <Q q="Full access vs read-only">
        <p><b>Full access</b>: agents and terminals, queries and edits, Redis keys, VPNs, Kubernetes actions. <b>Read-only</b>: browse and run queries that only read. Change it on the computer under Paired phones.</p>
      </Q>
      <Q q="Lost this phone?">
        <p>On the computer, click <b>Revoke</b> next to it in DevCat → Mobile. It's cut off immediately.</p>
      </Q>
    </Screen>
  );
}
