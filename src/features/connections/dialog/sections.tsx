// Form sections of the connection dialog. Each section edits a slice of ConnectionConfig.
import clsx from "clsx";
import { Check, FolderPlus, Plus, RefreshCw, Trash2 } from "lucide-react";
import { useEffect, useState } from "react";
import { Checkbox, IconButton, Input, Select, Spinner } from "../../../components/ui";
import { KIND_META } from "../../../components/ui/KindIcon";
import { api, errMsg } from "../../../lib/api";
import type { ConnectionConfig, ConnectionKind, SessionForward, SshConfig, SshConfigHost } from "../../../lib/types";
import { parseForwards } from "../../ssh/forwards";
import { useConnections } from "../../../store/connections";
import { createProject } from "../tree/projectActions";
import { NET_KIND_META } from "../../network/meta";
import { useNetProfiles } from "../../network/store";
import { FileInput, PasswordInput, Row, Segmented } from "./formBits";

export interface SectionProps {
  cfg: ConnectionConfig;
  set: (patch: Partial<ConnectionConfig>) => void;
  setOpt: (key: string, value: string | null) => void;
}

export const COLOR_PRESETS: { value: string | null; label: string }[] = [
  { value: null, label: "None" },
  { value: "#ef4444", label: "Production" },
  { value: "#f97316", label: "Orange" },
  { value: "#eab308", label: "Staging" },
  { value: "#22c55e", label: "Development" },
  { value: "#3b82f6", label: "Blue" },
  { value: "#a855f7", label: "Purple" },
  { value: "#64748b", label: "Gray" },
];

export const URL_KINDS: ConnectionKind[] = ["postgres", "mysql", "mongodb", "redis"];
export const SSH_KINDS: ConnectionKind[] = ["postgres", "mysql", "mssql", "mongodb", "redis"];
export const SSL_KINDS: ConnectionKind[] = ["postgres", "mysql", "mssql", "mongodb", "redis"];

const URL_PLACEHOLDER: Partial<Record<ConnectionKind, string>> = {
  postgres: "postgres://user:password@host:5432/dbname?sslmode=prefer",
  mysql: "mysql://user:password@host:3306/dbname",
  mongodb: "mongodb+srv://user:password@cluster0.example.net/?retryWrites=true",
  redis: "redis://:password@host:6379/0",
};

export function suggestName(cfg: ConnectionConfig): string {
  if (cfg.kind === "sqlite") return cfg.filePath?.split(/[\\/]/).pop() ?? "";
  if (cfg.kind === "kubernetes") return cfg.options.context || "Kubernetes";
  if (cfg.kind === "ssh") return cfg.host ? `${cfg.user ? `${cfg.user}@` : ""}${cfg.host}` : "";
  if (cfg.url) {
    try { return new URL(cfg.url).hostname; } catch { return ""; }
  }
  if (!cfg.host) return "";
  return cfg.database && cfg.kind !== "redis" && cfg.kind !== "rabbitmq" ? `${cfg.database}@${cfg.host}` : cfg.host;
}

const NEW_PROJECT = "\u0000new";

export function IdentitySection({ cfg, set }: SectionProps) {
  const projects = useConnections((s) => s.projects);
  const pickProject = async (value: string) => {
    if (value !== NEW_PROJECT) return set({ projectId: value || null });
    const p = await createProject();
    if (p) set({ projectId: p.id });
  };
  return (
    <>
      <Row label="Name">
        <Input autoFocus value={cfg.name} placeholder={suggestName(cfg) || "My connection"} onChange={(e) => set({ name: e.target.value })} />
      </Row>
      <Row label="Color" hint={cfg.color === "#ef4444" ? "Production — DevCat will highlight this connection in red." : undefined}>
        <div className="flex h-7 items-center gap-1.5">
          {COLOR_PRESETS.map((c) => (
            <button
              type="button"
              key={c.label}
              title={c.label}
              onClick={() => set({ color: c.value })}
              className={clsx(
                "flex size-5 items-center justify-center rounded-full border transition-transform hover:scale-110",
                c.value ? "border-transparent" : "border-line bg-app",
                (cfg.color ?? null) === c.value && "ring-2 ring-fg/70 ring-offset-1 ring-offset-panel",
              )}
              style={c.value ? { background: c.value } : undefined}
            >
              {!c.value && <span className="h-px w-3 rotate-45 bg-faint" />}
              {c.value && (cfg.color ?? null) === c.value && <Check className="size-3 text-white" />}
            </button>
          ))}
        </div>
      </Row>
      <Row label="Project">
        <div className="flex items-center gap-1.5">
          <Select value={cfg.projectId ?? ""} onChange={(e) => void pickProject(e.target.value)} className="min-w-0 flex-1">
            <option value="">No project</option>
            {projects.map((p) => <option key={p.id} value={p.id}>{p.name}</option>)}
            <option value={NEW_PROJECT}>New project…</option>
          </Select>
          <IconButton type="button" title="New project" onClick={() => void pickProject(NEW_PROJECT)}>
            <FolderPlus className="size-3.5" />
          </IconButton>
        </div>
      </Row>
    </>
  );
}

export function ServerSection({ cfg, set, useUrl, setUseUrl }: SectionProps & { useUrl: boolean; setUseUrl: (v: boolean) => void }) {
  const meta = KIND_META[cfg.kind];
  const canUrl = URL_KINDS.includes(cfg.kind);
  return (
    <>
      {canUrl && (
        <Row label="Connect with">
          <div><Segmented value={useUrl ? "url" : "host"} onChange={(v) => setUseUrl(v === "url")}
            options={[{ value: "host", label: "Host & port" }, { value: "url", label: "Connection URL" }]} /></div>
        </Row>
      )}
      {useUrl && canUrl ? (
        <Row label="URL" hint="Credentials in the URL are stored in the OS keychain together with the password.">
          <Input className="font-mono text-xs!" value={cfg.url ?? ""} placeholder={URL_PLACEHOLDER[cfg.kind]} onChange={(e) => set({ url: e.target.value })} />
        </Row>
      ) : (
        <>
          <Row label="Host">
            <div className="flex gap-1.5">
              <Input value={cfg.host} placeholder="localhost" onChange={(e) => set({ host: e.target.value })} />
              <Input
                className="w-24! shrink-0"
                inputMode="numeric"
                value={cfg.port ?? ""}
                placeholder={String(meta.defaultPort ?? "")}
                onChange={(e) => { const n = parseInt(e.target.value, 10); set({ port: Number.isFinite(n) ? n : null }); }}
              />
            </div>
          </Row>
          <Row label="User">
            <Input value={cfg.user} placeholder={cfg.kind === "redis" ? "default (optional)" : cfg.kind === "mssql" ? "sa" : cfg.kind === "postgres" ? "postgres" : cfg.kind === "mysql" ? "root" : ""} onChange={(e) => set({ user: e.target.value })} />
          </Row>
          <Row label="Password">
            <PasswordInput value={cfg.password ?? ""} onChange={(v) => set({ password: v })} />
          </Row>
          {cfg.kind === "redis" ? (
            <Row label="Database">
              <Select value={cfg.database ?? "0"} onChange={(e) => set({ database: e.target.value })} className="w-32!">
                {Array.from({ length: 16 }, (_, i) => <option key={i} value={String(i)}>db{i}</option>)}
              </Select>
            </Row>
          ) : (
            <Row label={cfg.kind === "mongodb" ? "Auth database" : "Database"} hint="Optional — leave empty to browse every database.">
              <Input value={cfg.database ?? ""} placeholder={cfg.kind === "mongodb" ? "admin" : cfg.kind === "postgres" ? "postgres" : ""} onChange={(e) => set({ database: e.target.value || null })} />
            </Row>
          )}
        </>
      )}
    </>
  );
}

export function SqliteSection({ cfg, set }: SectionProps) {
  return (
    <Row label="Database file" hint="Choose an existing file, or pick a location to create a new database.">
      <FileInput
        mode="open+save"
        value={cfg.filePath ?? ""}
        placeholder="/path/to/database.sqlite"
        onChange={(v) => set({ filePath: v || null })}
        filters={[{ name: "SQLite database", extensions: ["sqlite", "sqlite3", "db", "db3", "s3db"] }, { name: "All files", extensions: ["*"] }]}
      />
    </Row>
  );
}

export function KubernetesSection({ cfg, set, setOpt }: SectionProps) {
  const [contexts, setContexts] = useState<string[] | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [nonce, setNonce] = useState(0);
  const path = cfg.filePath ?? "";

  useEffect(() => {
    let cancelled = false;
    const t = setTimeout(async () => {
      setLoading(true);
      setError(null);
      try {
        const list = await api.k8sContexts(path || null);
        if (!cancelled) setContexts(list);
      } catch (e) {
        if (!cancelled) { setContexts(null); setError(errMsg(e)); }
      } finally {
        if (!cancelled) setLoading(false);
      }
    }, 350);
    return () => { cancelled = true; clearTimeout(t); };
  }, [path, nonce]);

  const ctx = cfg.options.context ?? "";
  return (
    <>
      <Row label="Kubeconfig">
        <FileInput value={path} placeholder="~/.kube/config" onChange={(v) => set({ filePath: v || null })} />
      </Row>
      <Row label="Context" hint={error ? <span className="text-danger">{error}</span> : undefined}>
        <div className="flex items-center gap-1.5">
          <Select value={ctx} onChange={(e) => setOpt("context", e.target.value || null)}>
            <option value="">(current context)</option>
            {ctx && contexts && !contexts.includes(ctx) && <option value={ctx}>{ctx}</option>}
            {contexts?.map((c) => <option key={c} value={c}>{c}</option>)}
          </Select>
          {loading ? <span className="flex size-6 items-center justify-center"><Spinner className="size-3.5" /></span> : (
            <IconButton type="button" title="Reload contexts" onClick={() => setNonce((n) => n + 1)}><RefreshCw className="size-3.5" /></IconButton>
          )}
        </div>
      </Row>
      <Row label="Namespace">
        <Input value={cfg.options.namespace ?? ""} placeholder="default" onChange={(e) => setOpt("namespace", e.target.value || null)} />
      </Row>
    </>
  );
}

/** Docker: a context from the user's Docker config, or a host that overrides it. */
export function DockerSection({ cfg, set, setOpt }: SectionProps) {
  const [contexts, setContexts] = useState<{ name: string; endpoint: string; current: boolean }[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [nonce, setNonce] = useState(0);
  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    api.dockerContexts().then(
      (l) => { if (!cancelled) { setContexts(l); setError(null); } },
      (e) => { if (!cancelled) { setContexts(null); setError(errMsg(e)); } },
    ).finally(() => { if (!cancelled) setLoading(false); });
    return () => { cancelled = true; };
  }, [nonce]);
  const ctx = cfg.options.context ?? "";
  const current = contexts?.find((c) => c.current);
  return (
    <>
      <Row label="Context" hint={error ? <span className="text-danger">{error}</span> : cfg.host.trim() ? "Not used: the host below is." : "Docker Desktop, OrbStack, Colima… as `docker context ls` lists them."}>
        <div className="flex items-center gap-1.5">
          <Select value={ctx} onChange={(e) => setOpt("context", e.target.value || null)}>
            <option value="">(current context{current ? `: ${current.name}` : ""})</option>
            {ctx && contexts && !contexts.some((c) => c.name === ctx) && <option value={ctx}>{ctx}</option>}
            {contexts?.map((c) => <option key={c.name} value={c.name}>{c.name}{c.endpoint ? ` · ${c.endpoint}` : ""}</option>)}
          </Select>
          {loading ? <span className="flex size-6 items-center justify-center"><Spinner className="size-3.5" /></span> : (
            <IconButton type="button" title="Reload contexts" onClick={() => setNonce((n) => n + 1)}><RefreshCw className="size-3.5" /></IconButton>
          )}
        </div>
      </Row>
      <Row label="Host" hint="Optional, overrides the context: ssh://user@server (your ssh key, no password prompt), tcp://host:2376 or unix:///var/run/docker.sock.">
        <Input value={cfg.host} placeholder="(the context's engine)" onChange={(e) => set({ host: e.target.value })} />
      </Row>
    </>
  );
}

/** AMQP ports → the management API port DevCat actually talks to. */
const AMQP_TO_MGMT: Record<number, number> = { 5672: 15672, 5671: 15671 };

/**
 * Parse a pasted RabbitMQ URL (amqp://, amqps://, http(s)://) into connection fields.
 * AMQP URLs point at the messaging port, so the port is mapped to the management API port.
 */
export function parseRabbitUrl(text: string): Partial<ConnectionConfig> & { tls: boolean; basePath: string | null } | null {
  const m = text.trim().match(/^(amqps?|https?):\/\//i);
  if (!m) return null;
  let u: URL;
  try { u = new URL(text.trim().replace(/^amqps?:/i, "http:")); } catch { return null; }
  const scheme = m[1].toLowerCase();
  const amqp = scheme.startsWith("amqp");
  const tls = scheme === "amqps" || scheme === "https";
  const given = u.port ? parseInt(u.port, 10) : null;
  const port = amqp ? (given ? AMQP_TO_MGMT[given] ?? given : tls ? 15671 : 15672) : given ?? (tls ? 443 : 15672);
  const path = decodeURIComponent(u.pathname);
  // amqp://host/ and amqp://host/%2F both mean the default vhost "/"
  const vhost = amqp ? (path === "" || path === "/" || path === "//" ? "/" : path.slice(1)) : null;
  const basePath = !amqp && path && path !== "/" ? path.replace(/\/api\/?$/, "").replace(/\/$/, "") || null : null;
  return {
    host: u.hostname,
    port,
    user: decodeURIComponent(u.username),
    password: decodeURIComponent(u.password),
    ...(vhost ? { database: vhost } : {}),
    tls,
    basePath,
  };
}

/** SSH session (Xshell-style): how to log in, and where the terminal starts. */
export function SshSessionSection({ cfg, set, setOpt }: SectionProps) {
  const [hosts, setHosts] = useState<SshConfigHost[]>([]);
  useEffect(() => { api.netSshConfigHosts().then(setHosts).catch(() => {}); }, []);
  // Picking a ~/.ssh/config alias: ssh applies that entry (user, key, jump…) itself.
  const alias = hosts.find((h) => h.alias === cfg.host.trim());
  return (
    <>
      <Row label="Host" hint={alias ? <>Uses <span className="font-mono">{alias.alias}</span> from ~/.ssh/config ({alias.user ? `${alias.user}@` : ""}{alias.host}{alias.port && alias.port !== 22 ? `:${alias.port}` : ""}); fields below override it.</> : "A host name, an IP, or a Host alias from ~/.ssh/config."}>
        <div className="flex gap-1.5">
          <Input value={cfg.host} list="devcat-ssh-hosts" placeholder="server.example.com" onChange={(e) => set({ host: e.target.value })} />
          <datalist id="devcat-ssh-hosts">
            {hosts.map((h) => <option key={h.alias} value={h.alias}>{h.user ? `${h.user}@` : ""}{h.host}</option>)}
          </datalist>
          <Input
            className="w-24! shrink-0"
            inputMode="numeric"
            value={cfg.port ?? ""}
            placeholder={alias ? "" : "22"}
            onChange={(e) => { const n = parseInt(e.target.value, 10); set({ port: Number.isFinite(n) ? n : null }); }}
          />
        </div>
      </Row>
      <Row label="User">
        <Input value={cfg.user} placeholder={alias?.user ?? "root"} onChange={(e) => set({ user: e.target.value })} />
      </Row>
      <Row label="Password" hint="Optional. Kept in the OS keychain and typed into ssh's login prompt for you.">
        <PasswordInput value={cfg.password ?? ""} placeholder="(use a key or ssh-agent)" onChange={(v) => set({ password: v })} />
      </Row>
      <Row label="Private key" hint="Optional. Without one, ssh-agent and your ~/.ssh keys are tried.">
        <FileInput value={cfg.options.keyPath ?? ""} placeholder={alias?.identityFile ?? "~/.ssh/id_ed25519"} onChange={(v) => setOpt("keyPath", v || null)} />
      </Row>
      <Row label="Jump host" hint="Optional ProxyJump: user@bastion:22, or several separated by commas.">
        <Input value={cfg.options.jump ?? ""} placeholder="bastion.example.com" onChange={(e) => setOpt("jump", e.target.value || null)} />
      </Row>
      <div className="my-1 h-px bg-line" />
      <Row label="Start folder">
        <Input className="font-mono text-xs!" value={cfg.options.startDir ?? ""} placeholder="~" onChange={(e) => setOpt("startDir", e.target.value || null)} />
      </Row>
      <Row label="Run on login" hint="Optional. Runs before your shell starts, e.g. tmux new -A -s main. It can't change the shell's environment.">
        <Input className="font-mono text-xs!" value={cfg.options.startup ?? ""} placeholder="tmux new -A -s main" onChange={(e) => setOpt("startup", e.target.value || null)} />
      </Row>
      <div className="my-1 h-px bg-line" />
      <SessionForwards cfg={cfg} setOpt={setOpt} />
    </>
  );
}

const portNum = (v: string) => { const n = parseInt(v, 10); return Number.isFinite(n) && n > 0 && n <= 65535 ? n : 0; };

/** An SSH session's port forwards (options.forwards, JSON). Started from the sidebar. */
function SessionForwards({ cfg, setOpt }: Pick<SectionProps, "cfg" | "setOpt">) {
  const list = parseForwards(cfg.options.forwards);
  const save = (next: SessionForward[]) => setOpt("forwards", next.length ? JSON.stringify(next) : null);
  const edit = (i: number, patch: Partial<SessionForward>) => save(list.map((f, j) => (j === i ? { ...f, ...patch } : f)));
  const port = (i: number, key: "localPort" | "remotePort", v: number, placeholder: string, title: string) => (
    <Input className="w-14! shrink-0 px-1.5! font-mono text-xs!" inputMode="numeric" value={v || ""} placeholder={placeholder} title={title}
      onChange={(e) => edit(i, { [key]: portNum(e.target.value) })} />
  );
  const text = "shrink-0 text-[11px] text-muted";
  return (
    <Row
      label="Port forwards"
      hint="Start them from the session's menu in the sidebar. They use their own connection, so stopping them leaves terminals open."
    >
      <div className="flex flex-col gap-1.5">
        {list.map((f, i) => (
          <div key={i} className="flex items-center gap-1.5">
            <Select className="w-32! shrink-0" value={f.kind} onChange={(e) => edit(i, { kind: e.target.value as SessionForward["kind"] })}>
              <option value="local">Host → here</option>
              <option value="remote">Here → host</option>
              <option value="dynamic">SOCKS</option>
            </Select>
            {f.kind === "local" && (
              <>
                <Input className="w-0 min-w-24 flex-1 font-mono text-xs!" value={f.remoteHost} placeholder="localhost" title="Host or IP, as the SSH server sees it"
                  onChange={(e) => edit(i, { remoteHost: e.target.value.trim() })} />
                <span className={text}>:</span>
                {port(i, "remotePort", f.remotePort, "3000", "Port on the host's side")}
                <span className={text}>→ localhost:</span>
                {port(i, "localPort", f.localPort, "3000", "Port on this computer")}
              </>
            )}
            {f.kind === "remote" && (
              <>
                <span className={text}>localhost:</span>
                {port(i, "localPort", f.localPort, "3000", "Port on this computer")}
                <span className={text}>→ host's localhost:</span>
                {port(i, "remotePort", f.remotePort, "3000", "Port on the host")}
                <span className="flex-1" />
              </>
            )}
            {f.kind === "dynamic" && (
              <>
                <span className={text}>proxy on localhost:</span>
                {port(i, "localPort", f.localPort, "1080", "Port on this computer")}
                <span className="flex-1" />
              </>
            )}
            <IconButton type="button" title="Remove forward" onClick={() => save(list.filter((_, j) => j !== i))}><Trash2 className="size-3.5" /></IconButton>
          </div>
        ))}
        <button
          type="button"
          className="flex w-fit items-center gap-1 text-[12px] text-accent hover:underline"
          onClick={() => save([...list, { kind: "local", localPort: 0, remoteHost: "localhost", remotePort: 0 }])}
        >
          <Plus className="size-3.5" />Add forward{list.length ? "" : " (e.g. the host's port 3000 at localhost:3000 here)"}
        </button>
      </div>
    </Row>
  );
}

export function RabbitSection({ cfg, set, setOpt }: SectionProps) {
  const tls = cfg.options.useTls === "true";
  const applyUrl = (text: string) => {
    const p = parseRabbitUrl(text);
    if (!p) return false;
    const { tls: useTls, basePath, ...fields } = p;
    set({ ...fields, user: fields.user || cfg.user, password: fields.password || cfg.password });
    setOpt("managementPort", String(fields.port));
    setOpt("useTls", useTls ? "true" : "false");
    if (basePath) setOpt("basePath", basePath);
    return true;
  };
  const amqpPort = cfg.port != null && AMQP_TO_MGMT[cfg.port];
  const [mode, setMode] = useState<"host" | "url">(cfg.options.amqpUrl ? "url" : "host");
  const url = cfg.options.amqpUrl ?? "";
  const parsed = url ? parseRabbitUrl(url) : null;
  const onUrl = (text: string) => {
    setOpt("amqpUrl", text || null);
    applyUrl(text);
  };
  const modeRow = (
    <Row label="Connect with">
      <div>
        <Segmented
          value={mode}
          onChange={(v) => { setMode(v as "host" | "url"); if (v === "host") setOpt("amqpUrl", null); }}
          options={[{ value: "host", label: "Host & port" }, { value: "url", label: "AMQP URL" }]}
        />
      </div>
    </Row>
  );

  if (mode === "url") {
    const scheme = cfg.options.useTls === "true" ? "https" : "http";
    return (
      <>
        {modeRow}
        <Row label="URL" hint="amqp://, amqps:// or http(s):// — the same value you'd put in RABBITMQ_URL.">
          <Input autoFocus value={url} placeholder="amqp://guest:guest@localhost:5672/" onChange={(e) => onUrl(e.target.value)} />
        </Row>
        <Row label="">
          {url && !parsed ? (
            <span className="text-[12px] text-danger">Not a valid amqp:// or http:// URL.</span>
          ) : parsed ? (
            <div className="rounded-md border border-line bg-app px-3 py-2 text-[12px] leading-5 text-muted">
              DevCat connects to the RabbitMQ <b className="text-fg/90">management API</b> (not the AMQP port):
              <div className="mt-1 font-mono text-[11px] text-fg/90">
                {scheme}://{cfg.host}:{cfg.port}{cfg.options.basePath ?? ""}/api · user {cfg.user || "guest"} · vhost {cfg.database || "/"}
              </div>
              <div className="mt-1 text-[11px]">Needs the <span className="font-mono">rabbitmq_management</span> plugin. Switch to “Host &amp; port” to adjust.</div>
            </div>
          ) : null}
        </Row>
      </>
    );
  }

  return (
    <>
      {modeRow}
      <Row
        label="Management host"
        hint={amqpPort
          ? `${cfg.port} is the AMQP messaging port. DevCat uses the management API, usually on port ${amqpPort} (needs the rabbitmq_management plugin).`
          : "Tip: paste an amqp:// or http:// URL here to fill in everything."}
      >
        <div className="flex gap-1.5">
          <Input
            value={cfg.host}
            placeholder="localhost or amqp://user:pass@host:5672/"
            onPaste={(e) => { if (applyUrl(e.clipboardData.getData("text"))) e.preventDefault(); }}
            onChange={(e) => { if (!applyUrl(e.target.value)) set({ host: e.target.value }); }}
          />
          <Input
            className={clsx("w-24! shrink-0", amqpPort && "border-warn!")}
            inputMode="numeric"
            value={cfg.port ?? ""}
            placeholder="15672"
            onChange={(e) => {
              const n = parseInt(e.target.value, 10);
              set({ port: Number.isFinite(n) ? n : null });
              setOpt("managementPort", Number.isFinite(n) ? String(n) : null);
            }}
          />
        </div>
      </Row>
      <Row label="">
        <div className="flex items-center gap-4">
          <Checkbox label="Use HTTPS" checked={tls} onChange={(v) => { setOpt("useTls", v ? "true" : "false"); if (!v) setOpt("insecureTls", null); }} />
          {tls && (
            <span title="Accept self-signed or otherwise untrusted TLS certificates">
              <Checkbox label="Allow self-signed certificates" checked={cfg.options.insecureTls === "true"} onChange={(v) => setOpt("insecureTls", v ? "true" : null)} />
            </span>
          )}
        </div>
      </Row>
      <Row label="User" hint={!cfg.user ? "Empty = RabbitMQ's default guest / guest login." : undefined}>
        <Input value={cfg.user} placeholder="guest" onChange={(e) => set({ user: e.target.value })} />
      </Row>
      <Row label="Password"><PasswordInput value={cfg.password ?? ""} placeholder="guest" onChange={(v) => set({ password: v })} /></Row>
      <Row label="Virtual host"><Input value={cfg.database ?? ""} placeholder="/" onChange={(e) => set({ database: e.target.value || null })} /></Row>
      <Row label="Base path" hint="Only when the management UI is served behind a reverse-proxy prefix.">
        <Input value={cfg.options.basePath ?? ""} placeholder="/ (e.g. /rabbitmq)" onChange={(e) => setOpt("basePath", e.target.value || null)} />
      </Row>
    </>
  );
}

const SSL_MODES = [
  { value: "disable", label: "Disable", hint: "Plain, unencrypted connection." },
  { value: "prefer", label: "Prefer", hint: "Use TLS when the server supports it." },
  { value: "require", label: "Require", hint: "Always use TLS, don't verify the certificate." },
  { value: "verify-full", label: "Verify full", hint: "Use TLS and verify the certificate and host name." },
];

export function SslSection({ cfg, set }: SectionProps) {
  const simple = cfg.kind === "mongodb" || cfg.kind === "redis";
  const modes = simple ? SSL_MODES.filter((m) => m.value === "disable" || m.value === "require" || m.value === "verify-full") : SSL_MODES;
  const value = cfg.sslMode ?? (simple ? "disable" : "prefer");
  return (
    <>
      <Row label={simple ? "TLS" : "SSL mode"} hint={modes.find((m) => m.value === value)?.hint}>
        <Select value={value} onChange={(e) => set({ sslMode: e.target.value })} className="w-48">
          {modes.map((m) => <option key={m.value} value={m.value}>{m.label}</option>)}
        </Select>
      </Row>
      {cfg.kind === "mssql" && (
        <Row label=""><span className="text-[11px] text-faint">SQL Server maps Disable/Prefer to "Encrypt=false/optional" and Require to "TrustServerCertificate".</span></Row>
      )}
    </>
  );
}

export const emptySsh = (): SshConfig => ({ enabled: false, host: "", port: 22, user: "", auth: "password", keyPath: null, password: null });

export function SshSection({ cfg, set }: SectionProps) {
  const ssh = cfg.ssh ?? emptySsh();
  const up = (p: Partial<SshConfig>) => set({ ssh: { ...ssh, ...p } });
  return (
    <>
      <Row label="">
        <Checkbox label="Connect through an SSH tunnel" checked={ssh.enabled} onChange={(v) => up({ enabled: v })} />
      </Row>
      <fieldset disabled={!ssh.enabled} className={clsx("flex flex-col gap-2.5", !ssh.enabled && "opacity-50")}>
        <Row label="SSH host">
          <div className="flex gap-1.5">
            <Input value={ssh.host} placeholder="bastion.example.com" onChange={(e) => up({ host: e.target.value })} />
            <Input className="w-24! shrink-0" inputMode="numeric" value={ssh.port || ""} placeholder="22"
              onChange={(e) => { const n = parseInt(e.target.value, 10); up({ port: Number.isFinite(n) ? n : 22 }); }} />
          </div>
        </Row>
        <Row label="SSH user"><Input value={ssh.user} placeholder="ubuntu" onChange={(e) => up({ user: e.target.value })} /></Row>
        <Row label="Authentication">
          <div><Segmented value={ssh.auth} onChange={(v) => up({ auth: v })}
            options={[{ value: "password", label: "Password" }, { value: "key", label: "Private key" }]} /></div>
        </Row>
        {ssh.auth === "key" && (
          <Row label="Private key">
            <FileInput value={ssh.keyPath ?? ""} placeholder="~/.ssh/id_ed25519" onChange={(v) => up({ keyPath: v || null })} />
          </Row>
        )}
        <Row label={ssh.auth === "key" ? "Passphrase" : "Password"}>
          <PasswordInput value={ssh.password ?? ""} placeholder={ssh.auth === "key" ? "(optional)" : ""} onChange={(v) => up({ password: v || null })} />
        </Row>
      </fieldset>
    </>
  );
}

/** Managed option keys edited by dedicated fields; hidden from the raw editor. */
const MANAGED_OPTIONS = new Set(["context", "namespace", "managementPort", "useTls", "insecureTls", "basePath", "networkId", "keyPath", "jump", "startDir", "startup", "forwards"]);

/** VPN / SSH tunnel profile that is connected before this connection opens (options.networkId). */
function NetworkRow({ cfg, setOpt }: SectionProps) {
  const profiles = useNetProfiles();
  const id = cfg.options.networkId ?? "";
  const missing = !!id && !profiles.some((p) => p.id === id);
  return (
    <Row label="Network" hint={missing ? <span className="text-warn">The selected VPN / tunnel no longer exists.</span> : "Connected automatically before this connection opens. Manage profiles in the VPN & SSH tunnels sidebar."}>
      <Select value={id} onChange={(e) => setOpt("networkId", e.target.value || null)} className="w-72!">
        <option value="">None (direct)</option>
        {missing && <option value={id}>(missing profile)</option>}
        {profiles.map((p) => <option key={p.id} value={p.id}>{p.name} — {NET_KIND_META[p.kind].label}</option>)}
      </Select>
    </Row>
  );
}

export function AdvancedSection({ cfg, set, setOpt }: SectionProps) {
  const isDb = cfg.kind !== "kubernetes" && cfg.kind !== "rabbitmq" && cfg.kind !== "docker" && cfg.kind !== "ssh";
  const entries = Object.entries(cfg.options).filter(([k]) => !MANAGED_OPTIONS.has(k));
  const [newKey, setNewKey] = useState("");
  const addKey = () => {
    const k = newKey.trim();
    if (!k) return;
    setOpt(k, cfg.options[k] ?? "");
    setNewKey("");
  };
  return (
    <>
      {cfg.kind !== "ssh" && (
        <Row label="" hint="Blocks data edits, DDL and destructive actions from the UI for this connection.">
          <Checkbox label="Read-only connection" checked={cfg.readOnly} onChange={(v) => set({ readOnly: v })} />
        </Row>
      )}
      <NetworkRow cfg={cfg} set={set} setOpt={setOpt} />
      {isDb && (
        <Row label="Driver options" hint="Extra key/value options passed to the driver.">
          <div className="flex flex-col gap-1">
            {entries.map(([k, v]) => (
              <div key={k} className="flex gap-1.5">
                <Input value={k} readOnly className="w-40! shrink-0 font-mono text-xs! text-muted" />
                <Input value={v} className="font-mono text-xs!" onChange={(e) => setOpt(k, e.target.value)} />
                <IconButton type="button" title="Remove option" onClick={() => setOpt(k, null)}><Trash2 className="size-3.5" /></IconButton>
              </div>
            ))}
            <div className="flex gap-1.5">
              <Input value={newKey} placeholder="option name" className="w-40! shrink-0 font-mono text-xs!"
                onChange={(e) => setNewKey(e.target.value)}
                onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); addKey(); } }} />
              <IconButton type="button" title="Add option" onClick={addKey}><Plus className="size-3.5" /></IconButton>
            </div>
          </div>
        </Row>
      )}
    </>
  );
}
