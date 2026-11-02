// OpenVPN / WireGuard editors: a structured form that generates the config, or the raw config text.
import Editor from "@monaco-editor/react";
import { AlertTriangle, FileUp, Info, Plus, Trash2 } from "lucide-react";
import { useEffect, useState } from "react";
import { Button, Checkbox, IconButton, Input, Select, Spinner } from "../../components/ui";
import { api, errMsg } from "../../lib/api";
import type { NetKind, NetProfile } from "../../lib/types";
import { PasswordInput, Row, Segmented } from "../connections/dialog/formBits";
import { PemField, pickTextFile, SectionTitle } from "./formBits";
import { emptyOvpnForm, generateOvpn, parseOvpn, type OvpnForm } from "./ovpnGen";
import { emptyWgForm, emptyWgPeer, FULL_TUNNEL, generateWg, isFullTunnel, parseWg, type WgForm, type WgPeer } from "./wgGen";

export type EditMode = "form" | "config";

interface VpnProps {
  p: NetProfile;
  set: (patch: Partial<NetProfile>) => void;
  text: string;
  setText: (t: string) => void;
  initialMode: EditMode;
  /** profile already saved (secrets live in the keychain) */
  saved: boolean;
}

/** Shared Form/Config switch. `parse` must succeed to enter the form view. */
function useModedForm<F>(text: string, setText: (t: string) => void, initialMode: EditMode, parse: (t: string) => { ok: true; form: F; notes: string[] } | { ok: false; reason: string }, gen: (f: F) => string, empty: () => F) {
  const [mode, setMode] = useState<EditMode>(initialMode);
  const [form, setForm] = useState<F | null>(() => {
    if (initialMode !== "form") return null;
    if (!text.trim() || text === gen(empty())) return empty();
    const r = parse(text);
    return r.ok ? r.form : null;
  });
  const [notes, setNotes] = useState<string[]>([]);
  const [blocked, setBlocked] = useState<string | null>(null);
  // Initial form view of a new profile: emit the generated text right away.
  useEffect(() => {
    if (mode === "form" && form && !text.trim()) setText(gen(form));
    if (initialMode === "form" && !form) setMode("config");
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  const switchTo = (m: EditMode) => {
    setBlocked(null);
    if (m === mode) return;
    if (m === "config") { setMode("config"); return; }
    if (!text.trim()) { const f = empty(); setForm(f); setNotes([]); setText(gen(f)); setMode("form"); return; }
    const r = parse(text);
    if (!r.ok) { setBlocked(r.reason); return; }
    setForm(r.form);
    setNotes(r.notes);
    setText(gen(r.form));
    setMode("form");
  };
  const update = (patch: Partial<F>) => {
    const next = { ...(form ?? empty()), ...patch };
    setForm(next);
    setText(gen(next));
  };
  return { mode, switchTo, form: form ?? empty(), update, notes, blocked };
}

/** Monaco editor for the config text + import + live parse preview. */
function ConfigText({ kind, text, setText }: { kind: NetKind; text: string; setText: (t: string) => void }) {
  const importFile = async () => {
    const f = await pickTextFile(
      kind === "openvpn" ? [{ name: "OpenVPN config", extensions: ["ovpn", "conf"] }] : [{ name: "WireGuard config", extensions: ["conf"] }],
      "Load configuration",
    );
    if (f) setText(f.text);
  };
  return (
    <div className="flex flex-col gap-2">
      <div className="flex items-center gap-2">
        <span className="text-[11px] text-faint">{kind === "openvpn" ? ".ovpn" : "WireGuard .conf"} — inline certificates and keys are fine; the file is stored with 0600 permissions.</span>
        <span className="flex-1" />
        <Button type="button" size="sm" onClick={importFile}><FileUp className="size-3" />Load file…</Button>
      </div>
      <div className="h-[380px] overflow-hidden rounded-md border border-line">
        <Editor
          language={kind === "openvpn" ? "shell" : "ini"}
          theme="devcat"
          value={text}
          onChange={(v) => setText(v ?? "")}
          options={{ minimap: { enabled: false }, fontSize: 12, wordWrap: "off", scrollBeyondLastLine: false, automaticLayout: true, lineNumbersMinChars: 3, tabSize: 2 }}
        />
      </div>
      <ParsePreview kind={kind} text={text} />
    </div>
  );
}

type Remote = { host?: string; port?: number | string; proto?: string };

function ParsePreview({ kind, text }: { kind: NetKind; text: string }) {
  const [res, setRes] = useState<Record<string, unknown> | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  useEffect(() => {
    if (!text.trim()) { setRes(null); setError(null); return; }
    let cancelled = false;
    const t = setTimeout(async () => {
      setLoading(true);
      try {
        const r = await api.netParse(kind, text);
        if (!cancelled) { setRes(r); setError(null); }
      } catch (e) {
        if (!cancelled) { setRes(null); setError(errMsg(e)); }
      } finally {
        if (!cancelled) setLoading(false);
      }
    }, 450);
    return () => { cancelled = true; clearTimeout(t); };
  }, [kind, text]);

  if (!text.trim()) return null;
  const summary = typeof res?.summary === "string" ? res.summary : null;
  const remotes = Array.isArray(res?.remotes) ? (res!.remotes as Remote[]) : [];
  const strs = (v: unknown) => (Array.isArray(v) ? v.filter((x) => x != null).map(String) : []);
  const peers = Array.isArray(res?.peers) ? (res!.peers as { endpoint?: string | null }[]) : [];
  const iface = (res?.interface ?? {}) as { address?: unknown; dns?: unknown; hasPrivateKey?: boolean };
  const endpoints = peers.map((x) => x.endpoint).filter((x): x is string => !!x);
  const warnings = strs(res?.warnings);
  const external = strs(res?.externalFiles);
  const address = strs(iface.address);
  const flags = [
    res?.needsUserPass === true && "asks for username / password",
    res?.hasInlineKey === true && "private key embedded",
    strs(res?.inlineBlocks).length > 0 && `inline: ${strs(res?.inlineBlocks).join(", ")}`,
    address.length > 0 && `address ${address.join(", ")}`,
    strs(iface.dns).length > 0 && `DNS ${strs(iface.dns).join(", ")}`,
    peers.length > 0 && `${peers.length} peer${peers.length === 1 ? "" : "s"}`,
    kind === "wireguard" && iface.hasPrivateKey === false && "no private key",
  ].filter(Boolean) as string[];

  return (
    <div className="rounded-md border border-line bg-panel px-3 py-2 text-[12px]">
      <div className="mb-1 flex items-center gap-2 text-[11px] font-semibold uppercase tracking-wider text-faint">
        Preview {loading && <Spinner className="size-3" />}
      </div>
      {error ? (
        <div className="text-[11px] text-faint">Preview unavailable: {error}</div>
      ) : res ? (
        <div className="flex flex-col gap-1">
          {summary && <div className="font-mono text-fg/90">{summary}</div>}
          {remotes.length > 0 && (
            <div className="flex flex-wrap gap-1">
              {remotes.map((r, i) => (
                <span key={i} className="rounded bg-hover px-1.5 font-mono text-[11px] text-muted">{r.proto ? `${r.proto} ` : ""}{r.host}{r.port ? `:${r.port}` : ""}</span>
              ))}
            </div>
          )}
          {endpoints.length > 0 && (
            <div className="flex flex-wrap gap-1">{endpoints.map((e) => <span key={e} className="rounded bg-hover px-1.5 font-mono text-[11px] text-muted">{e}</span>)}</div>
          )}
          {flags.length > 0 && <div className="text-[11px] text-muted">{flags.join(" · ")}</div>}
          {external.length > 0 && (
            <div className="flex items-start gap-1.5 text-[11px] text-warn">
              <AlertTriangle className="mt-px size-3 shrink-0" />
              <span>References external files ({external.join(", ")}). Embed them inline, or use absolute paths.</span>
            </div>
          )}
          {warnings.map((w) => (
            <div key={w} className="flex items-start gap-1.5 text-[11px] text-warn"><AlertTriangle className="mt-px size-3 shrink-0" />{w}</div>
          ))}
          {!summary && !remotes.length && !endpoints.length && !flags.length && !warnings.length && !external.length && <div className="text-[11px] text-faint">Nothing detected.</div>}
        </div>
      ) : (
        <div className="text-[11px] text-faint">…</div>
      )}
    </div>
  );
}

function Credentials({ p, set, saved, required }: { p: NetProfile; set: (patch: Partial<NetProfile>) => void; saved: boolean; required: boolean }) {
  return (
    <>
      <SectionTitle>Credentials</SectionTitle>
      <Row label="Username" hint={required ? undefined : "Only used when the config contains auth-user-pass."}>
        <Input value={p.vpnUser ?? ""} autoComplete="off" onChange={(e) => set({ vpnUser: e.target.value || null })} />
      </Row>
      <Row label="Password" hint="Stored in the OS keychain, never in the config file.">
        <PasswordInput value={p.vpnPassword ?? ""} onChange={(v) => set({ vpnPassword: v || (saved ? "" : null) })} />
      </Row>
    </>
  );
}

// ---------------------------------------------------------------- OpenVPN

const COMMON_CIPHERS = ["AES-256-GCM:AES-128-GCM:CHACHA20-POLY1305", "AES-256-GCM", "AES-128-GCM", "AES-256-CBC", "CHACHA20-POLY1305"];
const DIGESTS = ["SHA256", "SHA384", "SHA512", "SHA1"];

export function OpenVpnEditor({ p, set, text, setText, initialMode, saved }: VpnProps) {
  const m = useModedForm<OvpnForm>(text, setText, initialMode, parseOvpn, generateOvpn, emptyOvpnForm);
  const f = m.form;
  const needsUserPass = m.mode === "form" ? f.authUserPass : /^\s*auth-user-pass\b/m.test(text);
  return (
    <>
      <SectionTitle right={<ModeSwitchInline m={m} />}>Configuration</SectionTitle>
      <ModeHints m={m} />
      {m.mode === "config" ? (
        <ConfigText kind="openvpn" text={text} setText={setText} />
      ) : (
        <div className="flex flex-col gap-2.5">
          <Row label="Server">
            <div className="flex gap-1.5">
              <Input value={f.host} placeholder="vpn.example.com" onChange={(e) => m.update({ host: e.target.value.trim() })} />
              <Input className="w-20! shrink-0" inputMode="numeric" value={f.port || ""} placeholder="1194"
                onChange={(e) => { const n = parseInt(e.target.value, 10); m.update({ port: Number.isFinite(n) ? n : 0 }); }} />
            </div>
          </Row>
          <Row label="Protocol">
            <div className="flex items-center gap-3">
              <Segmented value={f.proto} onChange={(v) => m.update({ proto: v })} options={[{ value: "udp", label: "UDP" }, { value: "tcp", label: "TCP" }]} />
              <span className="text-xs text-muted">Device</span>
              <Segmented value={f.dev} onChange={(v) => m.update({ dev: v })} options={[{ value: "tun", label: "tun" }, { value: "tap", label: "tap" }]} />
            </div>
          </Row>
          <Row label="CA certificate"><PemField value={f.ca} placeholder="-----BEGIN CERTIFICATE-----" onChange={(v) => m.update({ ca: v })} /></Row>
          <Row label="Client certificate" hint="Optional when the server only uses username / password."><PemField value={f.cert} placeholder="-----BEGIN CERTIFICATE-----" onChange={(v) => m.update({ cert: v })} /></Row>
          <Row label="Client key"><PemField value={f.key} placeholder="-----BEGIN PRIVATE KEY-----" onChange={(v) => m.update({ key: v })} /></Row>
          <Row label="TLS key">
            <div className="flex flex-col gap-1.5">
              <div className="flex items-center gap-3">
                <Segmented value={f.tlsMode} onChange={(v) => m.update({ tlsMode: v })}
                  options={[{ value: "none", label: "None" }, { value: "tls-auth", label: "tls-auth" }, { value: "tls-crypt", label: "tls-crypt" }]} />
                {f.tlsMode === "tls-auth" && (
                  <>
                    <span className="text-xs text-muted">key-direction</span>
                    <Select className="w-20!" value={f.keyDirection} onChange={(e) => m.update({ keyDirection: e.target.value as OvpnForm["keyDirection"] })}>
                      <option value="1">1</option><option value="0">0</option><option value="">—</option>
                    </Select>
                  </>
                )}
              </div>
              {f.tlsMode !== "none" && <PemField value={f.tlsKey} placeholder="-----BEGIN OpenVPN Static key V1-----" onChange={(v) => m.update({ tlsKey: v })} />}
            </div>
          </Row>
          <Row label="Data ciphers">
            <Input list="ovpn-ciphers" className="font-mono text-xs!" value={f.cipher} placeholder="(OpenVPN default)" onChange={(e) => m.update({ cipher: e.target.value })} />
            <datalist id="ovpn-ciphers">{COMMON_CIPHERS.map((c) => <option key={c} value={c} />)}</datalist>
          </Row>
          <Row label="Auth digest">
            <div className="flex items-center gap-3">
              <Select className="w-32!" value={f.auth} onChange={(e) => m.update({ auth: e.target.value })}>
                <option value="">(default)</option>
                {f.auth && !DIGESTS.includes(f.auth) && <option value={f.auth}>{f.auth}</option>}
                {DIGESTS.map((d) => <option key={d} value={d}>{d}</option>)}
              </Select>
              <span className="text-xs text-muted">Compression</span>
              <Segmented value={f.compress} onChange={(v) => m.update({ compress: v })}
                options={[{ value: "none", label: "None" }, { value: "lz4", label: "lz4" }, { value: "lz4-v2", label: "lz4-v2" }]} />
            </div>
          </Row>
          <Row label="" hint="Adds auth-user-pass: the server asks for the username / password below.">
            <Checkbox label="Server requires username & password" checked={f.authUserPass} onChange={(v) => m.update({ authUserPass: v })} />
          </Row>
          <Row label="Extra directives" hint="One per line, copied verbatim (e.g. route-nopull, route 10.0.0.0 255.0.0.0).">
            <textarea
              spellCheck={false}
              rows={3}
              value={f.extra}
              onChange={(e) => m.update({ extra: e.target.value })}
              className="w-full resize-y rounded-md border border-line bg-app px-2 py-1.5 font-mono text-[11px] text-fg placeholder:text-faint focus:border-accent focus:outline-none"
              placeholder="route-nopull"
            />
          </Row>
        </div>
      )}
      <Credentials p={p} set={set} saved={saved} required={needsUserPass} />
      <Row label="openvpn arguments" hint="Optional extra command-line arguments, e.g. --mute-replay-warnings.">
        <Input className="font-mono text-xs!" value={p.options.ovpnArgs ?? ""} placeholder="(none)" onChange={(e) => setOption(p, set, "ovpnArgs", e.target.value || null)} />
      </Row>
      <Row label="" hint="For servers older than OpenVPN 2.4.5: DCO drops their packets, so the VPN connects but nothing gets through. DevCat turns it off by itself when OpenVPN reports that.">
        <Checkbox label="Turn off Data Channel Offload (DCO)" checked={p.options.disableDco === "true"} onChange={(v) => setOption(p, set, "disableDco", v ? "true" : null)} />
      </Row>
    </>
  );
}

// Form/Config switch (in the section title) + hints below it.
type Moded = { mode: EditMode; switchTo: (m: EditMode) => void; blocked: string | null; notes: string[] };
function ModeSwitchInline({ m }: { m: Moded }) {
  return <Segmented value={m.mode} onChange={m.switchTo} options={[{ value: "form", label: "Form" }, { value: "config", label: "Config file" }]} />;
}
function ModeHints({ m }: { m: Moded }) {
  if (!m.blocked && !(m.mode === "form" && m.notes.length)) return null;
  return (
    <div className="flex flex-col gap-0.5">
      {m.blocked && (
        <div className="flex items-start gap-1.5 text-[11px] text-warn">
          <AlertTriangle className="mt-px size-3 shrink-0" />
          <span>The form can't represent this config: {m.blocked} Keep editing the config file.</span>
        </div>
      )}
      {m.mode === "form" && m.notes.map((n) => <div key={n} className="flex items-start gap-1.5 text-[11px] text-faint"><Info className="mt-px size-3 shrink-0" />{n}</div>)}
    </div>
  );
}

// ---------------------------------------------------------------- WireGuard

function setOption(p: NetProfile, set: VpnProps["set"], key: string, value: string | null) {
  const options = { ...p.options };
  if (value === null) delete options[key]; else options[key] = value;
  set({ options });
}

export function WireGuardEditor({ p, set, text, setText, initialMode }: Omit<VpnProps, "saved">) {
  const m = useModedForm<WgForm>(text, setText, initialMode, parseWg, generateWg, emptyWgForm);
  const f = m.form;
  const setPeer = (i: number, patch: Partial<WgPeer>) => m.update({ peers: f.peers.map((x, j) => (j === i ? { ...x, ...patch } : x)) });
  return (
    <>
      <SectionTitle right={<ModeSwitchInline m={m} />}>Configuration</SectionTitle>
      <ModeHints m={m} />
      {m.mode === "config" ? (
        <ConfigText kind="wireguard" text={text} setText={setText} />
      ) : (
        <div className="flex flex-col gap-2.5">
          <div className="text-[11px] font-medium text-muted">[Interface]</div>
          <Row label="Private key" hint="Base64 key of this device. Stored in the config file (0600).">
            <PasswordInput value={f.privateKey} placeholder="yAnz5TF+lXXJte14tji3zlMNq+hd2rYUIgJBgB3fBmk=" onChange={(v) => m.update({ privateKey: v.trim() })} />
          </Row>
          <Row label="Address"><Input className="font-mono text-xs!" value={f.address} placeholder="10.8.0.2/32, fd00::2/128" onChange={(e) => m.update({ address: e.target.value })} /></Row>
          <Row label="DNS" hint="Optional — DNS servers to use while connected.">
            <Input className="font-mono text-xs!" value={f.dns} placeholder="1.1.1.1" onChange={(e) => m.update({ dns: e.target.value })} />
          </Row>
          <Row label="MTU"><Input className="w-24! font-mono text-xs!" inputMode="numeric" value={f.mtu} placeholder="auto" onChange={(e) => m.update({ mtu: e.target.value.replace(/\D/g, "") })} /></Row>
          {f.extra.length > 0 && <Row label="Other keys"><div className="font-mono text-[11px] text-faint">{f.extra.join("  ·  ")}</div></Row>}
          {f.peers.map((peer, i) => (
            <div key={i} className="flex flex-col gap-2.5 rounded-md border border-line/70 p-2.5">
              <div className="flex items-center text-[11px] font-medium text-muted">
                [Peer]{f.peers.length > 1 ? ` ${i + 1}` : ""}
                <span className="flex-1" />
                {f.peers.length > 1 && (
                  <IconButton type="button" title="Remove peer" onClick={() => m.update({ peers: f.peers.filter((_, j) => j !== i) })}><Trash2 className="size-3.5" /></IconButton>
                )}
              </div>
              <Row label="Public key"><Input className="font-mono text-xs!" value={peer.publicKey} placeholder="server public key" onChange={(e) => setPeer(i, { publicKey: e.target.value.trim() })} /></Row>
              <Row label="Preshared key"><PasswordInput value={peer.presharedKey} placeholder="(optional)" onChange={(v) => setPeer(i, { presharedKey: v.trim() })} /></Row>
              <Row label="Endpoint"><Input className="font-mono text-xs!" value={peer.endpoint} placeholder="vpn.example.com:51820" onChange={(e) => setPeer(i, { endpoint: e.target.value.trim() })} /></Row>
              <Row
                label="Allowed IPs"
                hint={isFullTunnel(peer.allowedIps)
                  ? "Full tunnel: all traffic goes through the VPN."
                  : "Split tunnel: only these networks go through the VPN (e.g. 10.0.0.0/8)."}
              >
                <div className="flex gap-1.5">
                  <Input className="font-mono text-xs!" value={peer.allowedIps} placeholder={FULL_TUNNEL} onChange={(e) => setPeer(i, { allowedIps: e.target.value })} />
                  {!isFullTunnel(peer.allowedIps) && <Button type="button" size="sm" className="h-7" onClick={() => setPeer(i, { allowedIps: FULL_TUNNEL })}>Full tunnel</Button>}
                </div>
              </Row>
              <Row label="Keepalive" hint="Seconds; 25 keeps NAT mappings alive. Empty = off.">
                <Input className="w-24! font-mono text-xs!" inputMode="numeric" value={peer.keepalive} placeholder="off" onChange={(e) => setPeer(i, { keepalive: e.target.value.replace(/\D/g, "") })} />
              </Row>
              {peer.extra.length > 0 && <Row label="Other keys"><div className="font-mono text-[11px] text-faint">{peer.extra.join("  ·  ")}</div></Row>}
            </div>
          ))}
          <div><Button type="button" size="sm" onClick={() => m.update({ peers: [...f.peers, emptyWgPeer()] })}><Plus className="size-3" />Add peer</Button></div>
        </div>
      )}
      <Row label="" hint="Off: ignore the DNS line so your system resolver stays as is.">
        <Checkbox label="Use the DNS servers from the config" checked={p.options.dns !== "false"} onChange={(v) => setOption(p, set, "dns", v ? null : "false")} />
      </Row>
    </>
  );
}
