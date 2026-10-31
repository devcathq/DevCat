// AI connections UI: the provider picker (sidebar header / tab toolbar) and the
// "Manage AI connections" dialog (add / edit / delete profiles, API key, test).
import clsx from "clsx";
import { Bot, Check, ChevronDown, Copy, Eye, EyeOff, KeyRound, Plus, Settings2, Trash2, X } from "lucide-react";
import { useEffect, useState, type MouseEvent } from "react";
import { toast } from "sonner";
import { Badge, Button, Dialog, Field, IconButton, Input, Select, Spinner } from "../../components/ui";
import { api, errMsg } from "../../lib/api";
import type { AgentSettings, AiDetect, AiEnvVar, AiKind, AiProvider } from "../../lib/types";
import { useContextMenu, type MenuItem } from "../connections/ContextMenu";
import { confirmDialog } from "../connections/dialogs";
import { copyText } from "../files/format";
import {
  blankProvider, closeManageProviders, deleteProvider, detectProviders, installState, joinArgs, KIND_ORDER, KINDS, loadProviders,
  openManageProviders, saveProvider, selectProvider, splitArgs, useProviders, useSelectedProvider, type InstallState,
  AGENT_PRESETS,
} from "./providers";

function StatusDot({ state, className }: { state: InstallState; className?: string }) {
  return (
    <span
      className={clsx("inline-block size-1.5 shrink-0 rounded-full", state === "installed" ? "bg-ok" : state === "missing" ? "bg-danger" : "bg-faint", className)}
      title={state === "installed" ? "Installed" : state === "missing" ? "Not installed" : "Install status unknown"}
    />
  );
}

/** Menu items to pick a provider (current one checked) + add / manage. */
export function providerMenu(currentId: string, onPick: (p: AiProvider) => void): MenuItem[] {
  const { providers, detect } = useProviders.getState();
  return [
    ...providers.map((p): MenuItem => {
      const st = installState(p, detect);
      return {
        label: `${p.name}${p.kind !== "claude" && p.name !== KINDS[p.kind].label ? ` (${KINDS[p.kind].label})` : ""}${st === "missing" ? " — not installed" : ""}`,
        icon: p.id === currentId ? <Check className="size-3.5 text-accent" /> : <StatusDot state={st} className="mx-1" />,
        onClick: () => onPick(p),
      };
    }),
    "separator",
    {
      label: "Add AI connection",
      icon: <Plus className="size-3.5" />,
      submenu: KIND_ORDER.filter((k) => k !== "claude").map((k): MenuItem => ({
        label: KINDS[k].label,
        icon: <StatusDot state={k === "custom" ? "unknown" : detect[k] ? (detect[k]!.path ? "installed" : "missing") : "unknown"} className="mx-1" />,
        onClick: () => openManageProviders("", k),
      })),
    },
    { label: "Manage AI connections…", icon: <Settings2 className="size-3.5" />, onClick: () => openManageProviders(currentId) },
  ];
}

/** Compact picker for the provider new chats use (sidebar header). */
export function ProviderPicker() {
  const selected = useSelectedProvider();
  const detect = useProviders((s) => s.detect);
  const menu = useContextMenu();
  useEffect(() => {
    void loadProviders();
    void detectProviders();
  }, []);
  const show = (e: MouseEvent) => {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    menu.show({ clientX: r.left, clientY: r.bottom + 2 }, providerMenu(selected.id, (p) => selectProvider(p.id)));
  };
  const st = installState(selected, detect);
  return (
    <>
      <button
        onClick={show}
        title={`AI connection for new chats: ${selected.name}${st === "missing" ? " (not installed)" : ""}`}
        className="flex h-6 min-w-0 max-w-40 items-center gap-1.5 rounded px-1.5 text-[11px] text-muted hover:bg-hover hover:text-fg"
      >
        <Bot className="size-3.5 shrink-0" />
        <span className="truncate">{selected.name}</span>
        <StatusDot state={st} />
        <ChevronDown className="size-3 shrink-0" />
      </button>
      {menu.element}
    </>
  );
}

// ---------------------------------------------------------------- manage dialog

interface Form {
  id: string;
  name: string;
  kind: AiKind;
  command: string;
  args: string;
  env: AiEnvVar[];
  apiKeyEnv: string;
  /** New key typed by the user ("" = unchanged). */
  apiKey: string;
  hasApiKey: boolean;
  /** Show the key input although a key is stored. */
  replaceKey: boolean;
  clearKey: boolean;
  builtin: boolean;
  /** DevCat Agent: the model API. */
  agent: AgentSettings;
}

const toForm = (p: AiProvider): Form => ({
  id: p.id, name: p.name, kind: p.kind, command: p.command ?? "", args: joinArgs(p.args), env: p.env.map((e) => ({ ...e })),
  apiKeyEnv: p.apiKeyEnv ?? "", apiKey: "", hasApiKey: !!p.hasApiKey, replaceKey: false, clearKey: false, builtin: !!p.builtin,
  agent: { ...(p.agent ?? AGENT_PRESETS[0].settings) },
});

const fromForm = (f: Form): AiProvider => ({
  id: f.id, name: f.name.trim(), kind: f.kind, command: f.command.trim() || null, args: splitArgs(f.args),
  // Only what the form shows for this type: a variable name typed under another type stays hidden
  // (and would be used, or refused for DevCat Agent, #91).
  env: f.kind === "agent" ? [] : f.env.filter((e) => e.key.trim()),
  apiKeyEnv: KINDS[f.kind].keyEnvEditable ? f.apiKeyEnv.trim() || null : null,
  apiKey: f.clearKey ? "" : f.apiKey.trim() ? f.apiKey.trim() : undefined,
  agent: f.kind === "agent" ? { ...f.agent, baseUrl: f.agent.baseUrl.trim(), model: f.agent.model.trim() } : null,
});

/** The models an API offers (reloaded when the API, the connection or the typed key changes). */
function useModelList(value: AgentSettings, providerId: string, apiKey: string) {
  const [state, setState] = useState<{ models: string[] | null; loading: boolean; error: string | null }>({ models: null, loading: false, error: null });
  const [nonce, setNonce] = useState(0);
  useEffect(() => {
    if (!/^https?:\/\/./.test(value.baseUrl.trim())) return setState({ models: null, loading: false, error: null });
    let live = true;
    setState((s) => ({ ...s, loading: true, error: null }));
    const t = setTimeout(() => {
      api.agentModels({ ...value, model: "" }, providerId || null, apiKey.trim() || null)
        .then((list) => list.map((m) => m.id))
        .then((models) => live && setState({ models, loading: false, error: models.length ? null : "The provider returned no models." }))
        .catch((e) => live && setState({ models: null, loading: false, error: errMsg(e) }));
    }, 500);
    return () => { live = false; clearTimeout(t); };
  }, [value.protocol, value.baseUrl, providerId, apiKey, nonce]);
  return { ...state, reload: () => setNonce((n) => n + 1) };
}

/** The harness's default (`MAX_STEPS`). */
const DEFAULT_MAX_STEPS = 60;

/** DevCat Agent: which model API to use, with presets for the common ones. The model itself is
 *  picked in the chat; here it only gets a default once the provider's list is known. */
function AgentFields({ value, onChange, providerId, apiKey }: { value: AgentSettings; onChange: (a: AgentSettings) => void; providerId: string; apiKey: string }) {
  const list = useModelList(value, providerId, apiKey);
  const preset = AGENT_PRESETS.find((p) => p.settings.baseUrl === value.baseUrl && p.settings.protocol === value.protocol);
  // Default model: keep the current one if the provider has it, else the preset's, else the newest.
  useEffect(() => {
    const models = list.models;
    if (!models?.length || models.includes(value.model)) return;
    const wanted = preset?.settings.model;
    onChange({ ...value, model: wanted && models.includes(wanted) ? wanted : models[0] });
  }, [list.models]);
  const manual = !list.loading && !list.models?.length && !!list.error;
  return (
    <>
      <div className="grid grid-cols-2 gap-3">
        <Field label="Provider" hint={preset?.keyHint ?? "Any OpenAI- or Anthropic-compatible API."}>
          <Select value={preset?.label ?? ""} onChange={(e) => {
            const p = AGENT_PRESETS.find((x) => x.label === e.target.value);
            if (p) onChange({ ...p.settings, maxSteps: value.maxSteps });
          }}>
            {!preset && <option value="">Custom</option>}
            {AGENT_PRESETS.map((p) => <option key={p.label} value={p.label}>{p.label}</option>)}
          </Select>
        </Field>
        <Field label="API style">
          <Select value={value.protocol} onChange={(e) => onChange({ ...value, protocol: e.target.value as AgentSettings["protocol"] })}>
            <option value="openai-chat">OpenAI-compatible (chat completions)</option>
            <option value="openai-responses">OpenAI Responses API</option>
            <option value="anthropic">Anthropic (messages)</option>
          </Select>
        </Field>
      </div>
      <Field label="API address">
        <Input className="font-mono text-xs" value={value.baseUrl} placeholder="https://api.example.com/v1" onChange={(e) => onChange({ ...value, baseUrl: e.target.value })} />
      </Field>
      {manual && (
        <Field label="Model" hint="This provider didn't list its models: type the one to use.">
          <Input className="font-mono text-xs" value={value.model} placeholder="model name" onChange={(e) => onChange({ ...value, model: e.target.value })} />
        </Field>
      )}
      <div className={clsx("-mt-1 flex items-center gap-1.5 text-[11px]", list.error && !manual ? "text-warn" : "text-faint")}>
        {list.loading ? <><Spinner className="size-3" />Checking the provider…</>
          : list.models?.length ? <><Check className="size-3 text-ok" />Connected: {list.models.length} models. Default <code className="text-muted">{value.model}</code>; switch models any time in the chat.</>
          : list.error ? <>Couldn't reach the provider ({list.error}).<button className="text-accent hover:underline" onClick={list.reload}>Try again</button></>
          : null}
      </div>
      <Field label="Steps per turn" hint={`How many steps (model calls) the agent takes on one message before it pauses and asks you to continue. 0 = no limit. Default ${DEFAULT_MAX_STEPS}.`}>
        <Input className="w-28 font-mono text-xs" type="number" min={0} step={10} value={value.maxSteps ?? DEFAULT_MAX_STEPS} onChange={(e) => {
          const n = Math.floor(Number(e.target.value));
          onChange({ ...value, maxSteps: e.target.value === "" || !Number.isFinite(n) || n < 0 ? null : n });
        }} />
      </Field>
    </>
  );
}

export function AiConnectionsDialog() {
  const manage = useProviders((s) => s.manage);
  const providers = useProviders((s) => s.providers);
  const detect = useProviders((s) => s.detect);
  const selectedId = useProviders((s) => s.selectedId);
  const [form, setForm] = useState<Form | null>(null);
  const [saving, setSaving] = useState(false);
  const [showKey, setShowKey] = useState(false);
  const [test, setTest] = useState<{ running: boolean; result?: AiDetect; error?: string } | null>(null);

  // Open: pick the requested profile (or start a new one).
  useEffect(() => {
    if (!manage) return;
    void loadProviders(true);
    void detectProviders(true);
    const list = useProviders.getState().providers;
    const p = manage.newKind ? blankProvider(manage.newKind) : list.find((x) => x.id === manage.editId) ?? list[0];
    setForm(p ? toForm(p) : null);
    setTest(null);
    setShowKey(false);
  }, [manage]);

  if (!manage) return null;
  const set = (p: Partial<Form>) => setForm((f) => (f ? { ...f, ...p } : f));
  const pick = (p: AiProvider) => { setForm(toForm(p)); setTest(null); setShowKey(false); };
  const kind = form ? KINDS[form.kind] : null;
  const keyVar = form ? form.apiKeyEnv.trim() || kind?.keyEnv || "" : "";

  const save = async () => {
    if (!form) return;
    setSaving(true);
    try {
      const saved = await saveProvider(fromForm(form));
      // A newly added connection is what the user wants to chat with next.
      if (!form.id) selectProvider(saved.id);
      setForm(toForm(saved));
      toast.success(`Saved ${saved.name}`);
    } catch (e) {
      toast.error(errMsg(e));
    } finally {
      setSaving(false);
    }
  };

  const remove = async () => {
    if (!form?.id) return;
    const ok = await confirmDialog({
      title: form.builtin ? "Reset Claude Code" : "Delete AI connection",
      message: form.builtin
        ? <>Reset <b>{form.name}</b> to its defaults? Its arguments, environment and stored API key are removed.</>
        : <>Delete <b>{form.name}</b> and its stored API key? Open chats keep running until they're restarted.</>,
      confirmLabel: form.builtin ? "Reset" : "Delete",
      danger: true,
    });
    if (!ok) return;
    try {
      await deleteProvider(form.id);
      const list = useProviders.getState().providers;
      setForm(toForm(list.find((p) => p.id === form.id) ?? list[0]));
    } catch (e) {
      toast.error(errMsg(e));
    }
  };

  const runTest = async () => {
    if (!form) return;
    setTest({ running: true });
    try {
      const [r] = await api.aiProviderDetect({ ...fromForm(form), apiKey: undefined });
      setTest({ running: false, result: r });
    } catch (e) {
      setTest({ running: false, error: errMsg(e) });
    }
  };

  return (
    <Dialog
      open
      onClose={closeManageProviders}
      title={<span className="flex items-center gap-2"><Bot className="size-4" />AI connections</span>}
      width={780}
      footer={
        <>
          {form?.id && (
            <Button variant="ghost" className="mr-auto text-danger" onClick={() => void remove()}>
              <Trash2 className="size-3.5" />{form.builtin ? "Reset to defaults" : "Delete"}
            </Button>
          )}
          <Button variant="ghost" onClick={closeManageProviders}>Close</Button>
          {form && form.id && form.id !== selectedId && (
            <Button onClick={() => selectProvider(form.id)} title="Use this connection for new chats">Use for new chats</Button>
          )}
          <Button variant="primary" loading={saving} disabled={!form} onClick={() => void save()}>{form?.id ? "Save" : "Add connection"}</Button>
        </>
      }
    >
      <div className="flex min-h-[420px] gap-4">
        <div className="flex w-52 shrink-0 flex-col gap-1 border-r border-line pr-3">
          {providers.map((p) => (
            <button
              key={p.id}
              onClick={() => pick(p)}
              className={clsx("flex items-center gap-2 rounded px-2 py-1.5 text-left", form?.id === p.id ? "bg-hover text-fg" : "text-muted hover:bg-hover/60 hover:text-fg")}
            >
              <StatusDot state={installState(p, detect)} />
              <div className="min-w-0 flex-1">
                <div className="truncate text-[13px]">{p.name}</div>
                <div className="truncate text-[11px] text-faint">{KINDS[p.kind].label}{p.id === selectedId ? " · default" : ""}</div>
              </div>
              {p.hasApiKey && <KeyRound className="size-3 shrink-0 text-faint" aria-label="API key stored" />}
            </button>
          ))}
          {form && !form.id && (
            <div className="flex items-center gap-2 rounded bg-hover px-2 py-1.5 text-[13px] text-fg"><Plus className="size-3.5" />New connection</div>
          )}
          <Select
            className="mt-2 h-7 text-xs"
            value=""
            onChange={(e) => { if (e.target.value) pick(blankProvider(e.target.value as AiKind)); }}
          >
            <option value="">+ Add connection…</option>
            {KIND_ORDER.filter((k) => k !== "claude").map((k) => <option key={k} value={k}>{KINDS[k].label}</option>)}
          </Select>
        </div>
        {form && kind && (
          <div className="flex min-w-0 flex-1 flex-col gap-3">
            <div className="grid grid-cols-2 gap-3">
              <Field label="Type">
                <Select value={form.kind} disabled={form.builtin} onChange={(e) => {
                  const k = e.target.value as AiKind;
                  // the key's variable belongs to the type: the new one starts from its own default
                  set({ kind: k, name: form.name === kind.label || !form.name.trim() ? KINDS[k].label : form.name, apiKeyEnv: "" });
                }}>
                  {KIND_ORDER.map((k) => <option key={k} value={k}>{KINDS[k].label}</option>)}
                </Select>
              </Field>
              <Field label="Name"><Input value={form.name} placeholder={kind.label} onChange={(e) => set({ name: e.target.value })} /></Field>
            </div>
            {form.kind === "agent" ? <AgentFields value={form.agent} onChange={(agent) => set({ agent })} providerId={form.id} apiKey={form.clearKey ? "" : form.apiKey} /> : <>
            <Field label="Command" hint={form.kind === "custom" ? "Program to run: a name on PATH or an absolute path." : `Leave empty to use ${kind.program} from your PATH.`}>
              <Input className="font-mono text-xs" value={form.command} placeholder={kind.program || "my-agent"} onChange={(e) => set({ command: e.target.value })} />
            </Field>
            <Field label="Arguments" hint={form.kind === "claude" ? "Added before the workspace's own options (session, model, permission mode)." : "Passed on every start, e.g. --model gpt-5."}>
              <Input className="font-mono text-xs" value={form.args} placeholder="--flag value" onChange={(e) => set({ args: e.target.value })} />
            </Field>
            </>}

            <div className="flex flex-col gap-1">
              <span className="text-xs text-muted">API key</span>
              <div className="flex items-center gap-2">
                {form.hasApiKey && !form.clearKey && !form.replaceKey ? (
                  <>
                    <Badge tone="ok"><KeyRound className="mr-1 size-3" />Stored in keychain</Badge>
                    <Button size="sm" variant="ghost" onClick={() => set({ replaceKey: true })}>Replace</Button>
                    <Button size="sm" variant="ghost" className="text-danger" onClick={() => set({ clearKey: true })}><X className="size-3" />Remove</Button>
                  </>
                ) : form.clearKey ? (
                  <>
                    <span className="text-[12px] text-warn">The stored key is removed when you save.</span>
                    <Button size="sm" variant="ghost" onClick={() => set({ clearKey: false })}>Undo</Button>
                  </>
                ) : (
                  <>
                    <Input
                      type={showKey ? "text" : "password"}
                      className="flex-1 font-mono text-xs"
                      autoComplete="off"
                      value={form.apiKey}
                      placeholder={form.hasApiKey ? "New key (replaces the stored one)" : "Optional"}
                      onChange={(e) => set({ apiKey: e.target.value })}
                    />
                    <IconButton title={showKey ? "Hide" : "Show"} onClick={() => setShowKey(!showKey)}>
                      {showKey ? <EyeOff className="size-3.5" /> : <Eye className="size-3.5" />}
                    </IconButton>
                    {form.hasApiKey && <Button size="sm" variant="ghost" onClick={() => set({ apiKey: "", replaceKey: false })}>Cancel</Button>}
                  </>
                )}
              </div>
              {form.kind === "agent" ? (
                <span className="text-[11px] text-faint">Sent only to the API address above. Not needed for Ollama or LM Studio.</span>
              ) : <div className="flex items-center gap-2 text-[11px] text-faint">
                <span>Passed as</span>
                {kind.keyEnvEditable ? (
                  <Input className="h-6 w-48 font-mono text-[11px]" value={form.apiKeyEnv} placeholder={kind.keyEnv ?? "VARIABLE_NAME"} onChange={(e) => set({ apiKeyEnv: e.target.value })} />
                ) : (
                  <code className="text-muted">{keyVar}</code>
                )}
                <span className="min-w-0 truncate">· Without a key: {kind.login}</span>
              </div>}
            </div>

            {form.kind !== "agent" && <div className="flex flex-col gap-1">
              <div className="flex items-center justify-between">
                <span className="text-xs text-muted">Environment variables</span>
                <Button size="sm" variant="ghost" onClick={() => set({ env: [...form.env, { key: "", value: "" }] })}><Plus className="size-3" />Add variable</Button>
              </div>
              {form.env.length === 0 && <span className="text-[11px] text-faint">None. Stored in plain text: put secrets in the API key field.</span>}
              {form.env.map((e, i) => (
                <div key={i} className="flex items-center gap-1.5">
                  <Input className="w-48 font-mono text-xs" value={e.key} placeholder="NAME"
                    onChange={(ev) => set({ env: form.env.map((x, j) => (j === i ? { ...x, key: ev.target.value } : x)) })} />
                  <Input className="flex-1 font-mono text-xs" value={e.value} placeholder="value"
                    onChange={(ev) => set({ env: form.env.map((x, j) => (j === i ? { ...x, value: ev.target.value } : x)) })} />
                  <IconButton title="Remove variable" onClick={() => set({ env: form.env.filter((_, j) => j !== i) })}><X className="size-3.5" /></IconButton>
                </div>
              ))}
            </div>}

            {form.kind === "agent" ? (
              <p className="mt-auto rounded-md border border-line bg-app p-2.5 text-[12px] text-muted">
                Built into DevCat: nothing to install. Chats show the agent's steps, ask before it edits files or runs commands, and work with any model that supports tool calls.
              </p>
            ) : <div className="mt-auto rounded-md border border-line bg-app p-2.5 text-[12px]">
              <div className="flex items-center gap-2">
                <span className="font-medium">Status</span>
                <TestResult form={form} test={test} detect={detect[form.kind]} />
                <Button size="sm" className="ml-auto" loading={test?.running} onClick={() => void runTest()}>Test</Button>
              </div>
            </div>}
          </div>
        )}
      </div>
    </Dialog>
  );
}

function TestResult({ form, test, detect }: { form: Form; test: { running: boolean; result?: AiDetect; error?: string } | null; detect?: AiDetect }) {
  if (test?.running) return <Spinner className="size-3.5" />;
  if (test?.error) return <span className="truncate text-danger">{test.error}</span>;
  // Before a test: the startup detection is accurate for profiles using the default binary.
  const r = test?.result ?? (!form.command.trim() && form.kind !== "custom" ? detect : undefined);
  if (!r) return <span className="text-faint">Press Test to check the command.</span>;
  if (r.path)
    return (
      <span className="min-w-0 truncate text-ok" title={r.path}>
        Found{r.version ? ` · ${r.version}` : ""} <span className="font-mono text-faint">{r.path}</span>
      </span>
    );
  const hint = r.installHint ?? KINDS[form.kind].install;
  return (
    <span className="flex min-w-0 items-center gap-2">
      <span className="shrink-0 text-danger">{r.error ?? "Not found"}</span>
      {hint && (
        <button className="selectable flex min-w-0 items-center gap-1 rounded bg-panel px-1.5 py-0.5 font-mono text-[11px] text-fg/90 hover:bg-hover"
          title="Copy install command" onClick={() => void copyText(hint, "Install command copied")}>
          <span className="truncate">{hint}</span><Copy className="size-3 shrink-0 text-faint" />
        </button>
      )}
    </span>
  );
}
