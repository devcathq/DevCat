// New / edit connection dialog, driven by useUi().connectionDialog ({ id, kind, projectId }).
// Also hosts the global confirm/prompt dialogs (<DialogHost/>) and the Sync MCP / project code
// folders dialogs, since AppShell mounts this once.
import clsx from "clsx";
import { ArrowLeft, CheckCircle2, PlugZap, XCircle } from "lucide-react";
import { useEffect, useState } from "react";
import { toast } from "sonner";
import { Button, Dialog, ErrorBox, Spinner } from "../../components/ui";
import { KIND_META, KindIcon } from "../../components/ui/KindIcon";
import { api, errMsg } from "../../lib/api";
import type { ConnectionConfig, ConnectionKind } from "../../lib/types";
import { newConnection, useConnections } from "../../store/connections";
import { useUi } from "../../store/ui";
import {
  AdvancedSection, DockerSection, IdentitySection, KubernetesSection, RabbitSection, ServerSection, SqliteSection, SshSection, SshSessionSection,
  SslSection, SSH_KINDS, SSL_KINDS, suggestName, URL_KINDS, emptySsh,
} from "./dialog/sections";
import { DialogHost } from "./dialogs";
import { McpActivityDialog } from "./mcp/McpActivityDialog";
import { McpSyncDialog } from "./mcp/McpSyncDialog";
import { ProjectAgentsDialog } from "./mcp/ProjectAgentsDialog";

export default function ConnectionDialog() {
  const state = useUi((s) => s.connectionDialog);
  return (
    <>
      {state && <ConnectionDialogInner key={`${state.id ?? "new"}:${state.kind ?? ""}:${state.projectId ?? ""}`} id={state.id} kind={state.kind as ConnectionKind | undefined} projectId={state.projectId ?? (state.id ? null : useConnections.getState().focusProjectId)} />}
      <McpSyncDialog />
      <McpActivityDialog />
      <ProjectAgentsDialog />
      <DialogHost />
    </>
  );
}

const KINDS = Object.keys(KIND_META) as ConnectionKind[];

const KIND_BLURB: Record<ConnectionKind, string> = {
  postgres: "PostgreSQL, CockroachDB, Timescale, Supabase",
  mysql: "MySQL, MariaDB, PlanetScale, TiDB",
  sqlite: "Local database file",
  mssql: "Microsoft SQL Server, Azure SQL",
  mongodb: "MongoDB, Atlas, DocumentDB",
  redis: "Redis, Valkey, KeyDB",
  kubernetes: "Clusters from your kubeconfig",
  rabbitmq: "Management HTTP API",
  docker: "Containers, images, volumes: this computer or a remote engine",
  ssh: "Terminal, SFTP and agents on a server",
};

type Section = "general" | "ssl" | "ssh" | "advanced";
type TestState = { status: "idle" } | { status: "testing" } | { status: "ok"; message: string } | { status: "error"; message: string };

function freshConfig(kind: ConnectionKind, projectId: string | null): ConnectionConfig {
  const c = newConnection(kind, projectId);
  c.port = KIND_META[kind].defaultPort ?? null;
  if (kind === "sqlite" || kind === "kubernetes" || kind === "docker" || kind === "ssh") c.host = "";
  // Empty = 22, or the port of a ~/.ssh/config alias.
  if (kind === "ssh") c.port = null;
  if (kind === "redis") c.database = "0";
  if (kind === "rabbitmq") {
    c.database = "/";
    c.options = { managementPort: "15672", useTls: "false" };
  }
  return c;
}

function ConnectionDialogInner({ id, kind, projectId }: { id: string | null; kind?: ConnectionKind; projectId: string | null }) {
  const close = useUi((s) => s.closeConnectionDialog);
  const editing = !!id;
  const validKind = kind && KINDS.includes(kind) ? kind : undefined;

  const [cfg, setCfg] = useState<ConnectionConfig | null>(() => (!editing && validKind ? freshConfig(validKind, projectId) : null));
  const [loadError, setLoadError] = useState<string | null>(null);
  const [useUrl, setUseUrl] = useState(false);
  const [section, setSection] = useState<Section>("general");
  const [test, setTest] = useState<TestState>({ status: "idle" });
  const [saving, setSaving] = useState(false);
  const [nameError, setNameError] = useState(false);

  useEffect(() => {
    if (!id) return;
    let cancelled = false;
    api.getConnection(id)
      .then((c) => {
        if (cancelled) return;
        setCfg({ ...c, options: { ...(c.options ?? {}) } });
        setUseUrl(!!c.url);
      })
      .catch((e) => !cancelled && setLoadError(errMsg(e)));
    return () => { cancelled = true; };
  }, [id]);

  const set = (patch: Partial<ConnectionConfig>) => {
    setCfg((c) => (c ? { ...c, ...patch } : c));
    setTest({ status: "idle" });
    if ("name" in patch) setNameError(false);
  };
  const setOpt = (key: string, value: string | null) => {
    setCfg((c) => {
      if (!c) return c;
      const options = { ...c.options };
      if (value === null) delete options[key]; else options[key] = value;
      return { ...c, options };
    });
    setTest({ status: "idle" });
  };

  const pickKind = (k: ConnectionKind) => {
    setCfg((c) => freshConfig(k, c?.projectId ?? projectId));
    setUseUrl(false);
    setSection("general");
    setTest({ status: "idle" });
  };

  /** Normalize the form state into what the backend expects. */
  const finalize = (c: ConnectionConfig): ConnectionConfig => {
    const out: ConnectionConfig = { ...c, options: { ...c.options } };
    out.name = c.name.trim() || suggestName(c);
    if (!URL_KINDS.includes(c.kind) || !useUrl) out.url = null;
    else out.url = c.url?.trim() || null;
    if (!SSH_KINDS.includes(c.kind)) out.ssh = null;
    else if (out.ssh && !out.ssh.enabled && !out.ssh.host) out.ssh = null;
    if (!SSL_KINDS.includes(c.kind)) out.sslMode = null;
    if (c.kind === "rabbitmq") {
      // Placeholders say guest/guest — make an empty login actually mean that.
      if (!c.user.trim()) {
        out.user = "guest";
        if (!c.password) out.password = "guest";
      }
      // Remember the URL for the "AMQP URL" view, minus the password (that goes to the keychain).
      if (out.options.amqpUrl) out.options.amqpUrl = out.options.amqpUrl.trim().replace(/^([a-z]+:\/\/[^:@/]*):[^@/]*@/i, "$1@");
      out.port = c.port ?? 15672;
      out.options.managementPort = String(out.port);
      if (!out.database) out.database = "/";
    }
    if (c.kind === "kubernetes") out.host = "";
    if (c.kind === "docker") out.host = c.host.trim();
    if (c.kind === "ssh") {
      out.host = c.host.trim();
      out.user = c.user.trim();
      out.readOnly = false;
      // Empty fields mean "not set" (ssh / ~/.ssh/config defaults apply).
      for (const k of ["keyPath", "jump", "startDir", "startup"]) {
        const v = out.options[k]?.trim();
        if (v) out.options[k] = v; else delete out.options[k];
      }
    }
    out.group = null;
    out.projectId = c.projectId || null;
    return out;
  };

  const runTest = async () => {
    if (!cfg) return;
    setTest({ status: "testing" });
    try {
      const v = await api.testConnection(finalize(cfg));
      setTest({ status: "ok", message: v || "Connected" });
    } catch (e) {
      setTest({ status: "error", message: errMsg(e) });
    }
  };

  const submit = async () => {
    if (!cfg || saving) return;
    const out = finalize(cfg);
    if (!out.name) {
      setNameError(true);
      setSection("general");
      return;
    }
    if (out.kind === "sqlite" && !out.filePath) {
      toast.error("Choose a database file");
      return;
    }
    setSaving(true);
    try {
      const saved = await useConnections.getState().save(out);
      toast.success(editing ? `Saved ${saved.name}` : `Added ${saved.name}`);
      close();
    } catch (e) {
      toast.error(`Could not save connection: ${errMsg(e)}`);
    } finally {
      setSaving(false);
    }
  };

  // ---- step 1: kind picker
  if (!editing && !cfg) {
    return (
      <Dialog open title="New connection" width={640} onClose={close}>
        <div className="grid grid-cols-2 gap-2">
          {KINDS.map((k, i) => (
            <button
              key={k}
              autoFocus={i === 0}
              onClick={() => pickKind(k)}
              className="flex items-center gap-3 rounded-lg border border-line bg-app px-3 py-2.5 text-left hover:border-accent/60 hover:bg-raised focus-visible:border-accent"
            >
              <span className="flex size-9 shrink-0 items-center justify-center rounded-md bg-raised">
                <KindIcon kind={k} className="size-5" />
              </span>
              <span className="min-w-0">
                <span className="block text-[13px] font-medium">{KIND_META[k].label}</span>
                <span className="block truncate text-[11px] text-muted">{KIND_BLURB[k]}</span>
              </span>
            </button>
          ))}
        </div>
      </Dialog>
    );
  }

  // ---- loading existing profile
  if (!cfg) {
    return (
      <Dialog open title="Edit connection" width={640} onClose={close}>
        {loadError ? <ErrorBox error={loadError} /> : <div className="flex h-40 items-center justify-center"><Spinner /></div>}
      </Dialog>
    );
  }

  const sections: { id: Section; label: string }[] = [
    { id: "general", label: "General" },
    ...(SSL_KINDS.includes(cfg.kind) && !(useUrl && URL_KINDS.includes(cfg.kind)) ? [{ id: "ssl" as const, label: "SSL / TLS" }] : []),
    ...(SSH_KINDS.includes(cfg.kind) ? [{ id: "ssh" as const, label: cfg.ssh?.enabled ? "SSH ●" : "SSH" }] : []),
    { id: "advanced", label: "Advanced" },
  ];
  const activeSection = sections.some((s) => s.id === section) ? section : "general";
  const sp = { cfg, set, setOpt };

  const title = (
    <span className="flex items-center gap-2">
      {!editing && !validKind && (
        <button type="button" title="Choose another type" onClick={() => setCfg(null)} className="-ml-1 rounded p-0.5 text-muted hover:bg-hover hover:text-fg">
          <ArrowLeft className="size-4" />
        </button>
      )}
      <KindIcon kind={cfg.kind} />
      {editing ? `Edit ${cfg.name || "connection"}` : `New ${KIND_META[cfg.kind].label} connection`}
    </span>
  );

  return (
    <Dialog
      open
      title={title}
      width={640}
      onClose={close}
      footer={
        <>
          <Button type="button" onClick={runTest} loading={test.status === "testing"} className="mr-auto">
            {test.status !== "testing" && <PlugZap className="size-3.5" />}Test connection
          </Button>
          <Button type="button" onClick={close}>Cancel</Button>
          <Button type="submit" form="devcat-conn-form" variant="primary" loading={saving}>{editing ? "Save" : "Add connection"}</Button>
        </>
      }
    >
      <form
        id="devcat-conn-form"
        autoComplete="off"
        onSubmit={(e) => { e.preventDefault(); submit(); }}
        className="flex min-h-[340px] flex-col"
      >
        <div className="-mx-4 -mt-4 mb-4 flex gap-0.5 border-b border-line px-4">
          {sections.map((s) => (
            <button
              type="button"
              key={s.id}
              onClick={() => setSection(s.id)}
              className={clsx(
                "-mb-px border-b-2 px-3 py-2 text-xs font-medium",
                activeSection === s.id ? "border-accent text-fg" : "border-transparent text-muted hover:text-fg",
              )}
            >
              {s.label}
            </button>
          ))}
        </div>

        <div className="flex flex-col gap-2.5">
          {activeSection === "general" && (
            <>
              <IdentitySection {...sp} />
              {nameError && <div className="pl-[144px] text-[11px] text-danger">Name is required.</div>}
              <div className="my-1 h-px bg-line" />
              {cfg.kind === "sqlite" ? <SqliteSection {...sp} />
                : cfg.kind === "kubernetes" ? <KubernetesSection {...sp} />
                : cfg.kind === "rabbitmq" ? <RabbitSection {...sp} />
                : cfg.kind === "docker" ? <DockerSection {...sp} />
                : cfg.kind === "ssh" ? <SshSessionSection {...sp} />
                : <ServerSection {...sp} useUrl={useUrl} setUseUrl={(v) => { setUseUrl(v); setTest({ status: "idle" }); }} />}
            </>
          )}
          {activeSection === "ssl" && <SslSection {...sp} />}
          {activeSection === "ssh" && <SshSection {...sp} cfg={{ ...cfg, ssh: cfg.ssh ?? emptySsh() }} />}
          {activeSection === "advanced" && <AdvancedSection {...sp} />}
        </div>

        <div className="mt-auto pt-4">
          {test.status === "ok" && (
            <div className="selectable flex items-start gap-2 rounded-md border border-ok/30 bg-ok/10 px-3 py-2 text-xs text-ok">
              <CheckCircle2 className="mt-px size-3.5 shrink-0" />
              <span className="min-w-0 break-words"><b className="font-semibold">Connected.</b> {test.message}</span>
            </div>
          )}
          {test.status === "error" && (
            <div className="selectable flex max-h-32 items-start gap-2 overflow-auto rounded-md border border-danger/30 bg-danger/10 px-3 py-2 font-mono text-xs text-danger">
              <XCircle className="mt-px size-3.5 shrink-0" />
              <span className="min-w-0 whitespace-pre-wrap break-words">{test.message}</span>
            </div>
          )}
        </div>
        <button type="submit" hidden />
      </form>
    </Dialog>
  );
}
