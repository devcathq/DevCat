// Agent bots: DevCat agents in Telegram and Discord. Add a bot (its token stays in the keychain), then
// link a chat to a folder: the chat gets notifications, Allow / Deny buttons and the agent's
// results, and can send requests and get files, as the link allows. Notification webhooks
// (Discord, Slack) are in Mobile → Notifications.
import { open as pickFolder } from "@tauri-apps/plugin-dialog";
import { openUrl } from "@tauri-apps/plugin-opener";
import clsx from "clsx";
import { Bot, Link2, MessageCircle, Send, Trash2, TriangleAlert } from "lucide-react";
import { useEffect, useState, type ReactNode } from "react";
import { toast } from "sonner";
import { Button, Checkbox, Dialog, Field, Input, Select, Spinner } from "../../components/ui";
import { errMsg } from "../../lib/api";
import { useWorkspace } from "../claude/sidebar/workspace";
import { loadProviders, openManageProviders, useProviders } from "../claude/providers";
import { DEFAULT_PERMS, integrationsApi, type BotKind, type BotStatus, type IntegrationsStatus, type Link, type Perms } from "./api";
import { closeIntegrations, setIntegrations, startIntegrations, useIntegrations } from "./store";

const KIND_LABEL: Record<BotKind, string> = { telegram: "Telegram", discord: "Discord" };

async function run(f: () => Promise<IntegrationsStatus>, ok?: string) {
  try {
    setIntegrations(await f());
    if (ok) toast.success(ok);
    return true;
  } catch (e) {
    toast.error(errMsg(e));
    return false;
  }
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

/** What a link may do, as checkboxes (with what each one means). */
function PermsEditor({ perms, onChange }: { perms: Perms; onChange: (p: Perms) => void }) {
  const set = (k: keyof Perms) => (v: boolean) => onChange({ ...perms, [k]: v });
  return (
    <div className="grid grid-cols-2 gap-x-4 gap-y-1.5">
      <Checkbox label="Notify: finished / needs you" checked={perms.notify} onChange={set("notify")} />
      <Checkbox label="Allow / Deny, and answer questions" checked={perms.approve} onChange={set("approve")} />
      <Checkbox label="Send new requests to the agent" checked={perms.requests} onChange={set("requests")} />
      <Checkbox label="Edit files without asking (commands still ask)" checked={perms.autoEdit} onChange={set("autoEdit")} />
      <Checkbox label="Get files from the folder (/files, /file)" checked={perms.files} onChange={set("files")} />
      <Checkbox label="Use DevCat connections (their data goes to the chat app)" checked={perms.connections} onChange={set("connections")} />
    </div>
  );
}

function BotSetup({ kind }: { kind: BotKind }) {
  const link = (url: string, label: string) => <button className="text-accent hover:underline" onClick={() => void openUrl(url)}>{label}</button>;
  return kind === "telegram" ? (
    <ol className="list-decimal space-y-1 pl-5 text-[12px] text-muted marker:text-accent [&_b]:text-fg">
      <li>In Telegram open {link("https://t.me/BotFather", "@BotFather")} and send <b>/newbot</b>; pick a name.</li>
      <li>Copy the token it gives you (like <span className="font-mono">123456:ABC…</span>) and paste it here.</li>
      <li>Use this bot for this DevCat only: another program reading its messages stops it here.</li>
    </ol>
  ) : (
    <ol className="list-decimal space-y-1 pl-5 text-[12px] text-muted marker:text-accent [&_b]:text-fg">
      <li>In the {link("https://discord.com/developers/applications", "Discord Developer Portal")} create an application, open <b>Bot</b> and <b>Reset Token</b>; paste the token here.</li>
      <li>To use it in a server: turn on <b>Message Content Intent</b> (same page), then invite it (OAuth2 → URL Generator: scope <b>bot</b>, permissions <b>Send Messages</b>, <b>Attach Files</b>, <b>Read Message History</b>). Direct messages need neither.</li>
    </ol>
  );
}

function AddBot({ onDone }: { onDone: () => void }) {
  const [kind, setKind] = useState<BotKind>("telegram");
  const [token, setToken] = useState("");
  const [busy, setBusy] = useState(false);
  const add = async () => {
    setBusy(true);
    const ok = await run(() => integrationsApi.botAdd(kind, token), `${KIND_LABEL[kind]} bot added`);
    setBusy(false);
    if (ok) { setToken(""); onDone(); }
  };
  return (
    <div className="space-y-3 rounded-md border border-line bg-raised/40 p-3">
      <div className="flex gap-2">
        {(["telegram", "discord"] as BotKind[]).map((k) => (
          <button key={k} onClick={() => setKind(k)} className={clsx("rounded-md border px-3 py-1 text-[12.5px]", kind === k ? "border-accent bg-accent/10 text-fg" : "border-line text-muted hover:text-fg")}>{KIND_LABEL[k]}</button>
        ))}
      </div>
      <BotSetup kind={kind} />
      <div className="flex gap-2">
        <Input type="password" className="flex-1" placeholder="Bot token" value={token} onChange={(e) => setToken(e.target.value)} onKeyDown={(e) => e.key === "Enter" && token.trim() && void add()} />
        <Button variant="primary" loading={busy} disabled={!token.trim()} onClick={() => void add()}>Add bot</Button>
        <Button variant="ghost" onClick={onDone}>Cancel</Button>
      </div>
      <div className="text-[11px] text-faint">The token is checked with {KIND_LABEL[kind]}, then kept in this computer's keychain.</div>
    </div>
  );
}

function BotRow({ b }: { b: BotStatus }) {
  return (
    <div className="flex items-center gap-3 rounded-md border border-line px-3 py-2">
      <span className={clsx("size-2 shrink-0 rounded-full", b.running ? "bg-ok" : b.enabled ? "bg-danger" : "bg-faint")} />
      <div className="min-w-0 flex-1">
        <div className="text-[13px] font-medium">{b.name} <span className="text-[11px] font-normal text-muted">{KIND_LABEL[b.kind]}</span></div>
        {b.error && <div className="text-[11.5px] text-danger">{b.error}</div>}
        {b.warning && <div className="flex items-start gap-1 text-[11.5px] text-warn"><TriangleAlert className="mt-0.5 size-3 shrink-0" />{b.warning}</div>}
        {!b.error && !b.warning && <div className="text-[11.5px] text-muted">{b.running ? "Listening" : b.enabled ? "Starting…" : "Off"}</div>}
      </div>
      <Button size="sm" variant="ghost" onClick={() => void run(() => integrationsApi.botEnable(b.id, !b.enabled))}>{b.enabled ? "Turn off" : "Turn on"}</Button>
      <Button size="sm" variant="ghost" title="Remove the bot (its linked chats are unlinked)" onClick={() => void run(() => integrationsApi.botRemove(b.id), "Bot removed")}><Trash2 className="size-3.5" /></Button>
    </div>
  );
}

function Countdown({ until }: { until: number }) {
  const [now, setNow] = useState(Date.now());
  useEffect(() => { const t = setInterval(() => setNow(Date.now()), 1000); return () => clearInterval(t); }, []);
  const s = Math.max(0, Math.round((until - now) / 1000));
  return <span className="tabular-nums">{Math.floor(s / 60)}:{String(s % 60).padStart(2, "0")}</span>;
}

function LinkChat({ status }: { status: IntegrationsStatus }) {
  const providers = useProviders((s) => s.providers).filter((p) => p.kind === "agent");
  const cwd = useWorkspace((s) => s.cwd);
  const [botId, setBotId] = useState(status.bots[0]?.id ?? "");
  const [folder, setFolder] = useState(cwd ?? "");
  const [providerId, setProviderId] = useState(providers[0]?.id ?? "");
  const [perms, setPerms] = useState<Perms>(DEFAULT_PERMS);
  useEffect(() => { void loadProviders(); }, []);
  useEffect(() => { if (!providerId && providers[0]) setProviderId(providers[0].id); }, [providers, providerId]);
  const code = status.code;
  const bot = status.bots.find((b) => b.id === (code?.botId ?? botId));

  if (code) {
    return (
      <div className="space-y-2 rounded-md border border-accent/40 bg-accent/5 p-3">
        <div className="text-[13px]">In {bot ? `${KIND_LABEL[bot.kind]}, send this to ${bot.name}` : "the chat, send this to the bot"} (a direct message, or in the group / channel to link):</div>
        <div className="flex items-center gap-3">
          <span className="rounded-md bg-app px-3 py-1.5 font-mono text-[18px] font-semibold tracking-wider select-all">/link {code.code}</span>
          <Button size="sm" onClick={() => void navigator.clipboard.writeText(`/link ${code.code}`).then(() => toast.success("Copied"))}>Copy</Button>
          <span className="text-[12px] text-muted">expires in <Countdown until={code.expiresMs} /></span>
          <Button size="sm" variant="ghost" className="ml-auto" onClick={() => void run(integrationsApi.linkCancelCode)}>Cancel</Button>
        </div>
        <div className="text-[11.5px] text-muted">Then DevCat asks you here to allow that chat. Only the person who sends the code can use it afterwards.</div>
      </div>
    );
  }
  if (!status.bots.length) return <div className="text-[12px] text-muted">Add a bot first.</div>;
  return (
    <div className="space-y-3 rounded-md border border-line p-3">
      <div className="grid grid-cols-[1fr_1fr] gap-3">
        <Field label="Bot">
          <Select value={botId} onChange={(e) => setBotId(e.target.value)}>
            {status.bots.map((b) => <option key={b.id} value={b.id}>{b.name} ({KIND_LABEL[b.kind]})</option>)}
          </Select>
        </Field>
        <Field label="DevCat Agent connection" hint="Runs the requests from the chat">
          {providers.length ? (
            <Select value={providerId} onChange={(e) => setProviderId(e.target.value)}>
              {providers.map((p) => <option key={p.id} value={p.id}>{p.name}</option>)}
            </Select>
          ) : (
            <Button size="sm" onClick={() => openManageProviders("", "agent")}>Add a DevCat Agent connection</Button>
          )}
        </Field>
      </div>
      <Field label="Folder" hint="Where the agent works: its project, rules and memory. On this computer.">
        <div className="flex gap-2">
          <Input className="flex-1 font-mono" value={folder} onChange={(e) => setFolder(e.target.value)} placeholder="/path/to/project" />
          <Button onClick={() => void pickFolder({ directory: true, defaultPath: folder || undefined }).then((p) => typeof p === "string" && setFolder(p))}>Choose…</Button>
        </div>
      </Field>
      <PermsEditor perms={perms} onChange={setPerms} />
      <div className="flex justify-end">
        <Button variant="primary" disabled={!botId || !folder.trim() || !providerId} onClick={() => void run(() => integrationsApi.linkCode(botId, folder, providerId, perms))}>
          <Link2 className="size-3.5" />Get a link code
        </Button>
      </div>
    </div>
  );
}

function LinkRow({ l, bot }: { l: Link; bot?: BotStatus }) {
  const providers = useProviders((s) => s.providers).filter((p) => p.kind === "agent");
  const [perms, setPerms] = useState(l.perms);
  const save = (p: Perms, providerId: string | null = null) => {
    setPerms(p);
    void run(() => integrationsApi.linkUpdate(l.id, p, providerId));
  };
  return (
    <div className="space-y-2 rounded-md border border-line p-3">
      <div className="flex items-center gap-2">
        <MessageCircle className="size-4 text-muted" />
        <div className="min-w-0 flex-1">
          <div className="truncate text-[13px] font-medium">{l.chatName} <span className="text-[11px] font-normal text-muted">· {bot ? `${bot.name} (${KIND_LABEL[bot.kind]})` : "bot removed"}</span></div>
          <div className="truncate font-mono text-[11px] text-muted" title={l.folder}>{l.folder}</div>
          <div className="text-[11px] text-faint">Listens to {l.users.map((u) => u.name).join(", ")}</div>
        </div>
        <Select className="w-40" value={l.providerId} onChange={(e) => save(perms, e.target.value)} title="DevCat Agent connection">
          {providers.map((p) => <option key={p.id} value={p.id}>{p.name}</option>)}
          {!providers.some((p) => p.id === l.providerId) && <option value={l.providerId}>(missing)</option>}
        </Select>
        <Button size="sm" variant="ghost" title="Send a test message" onClick={() => void integrationsApi.linkTest(l.id).then(() => toast.success("Sent"), (e) => toast.error(errMsg(e)))}><Send className="size-3.5" /></Button>
        <Button size="sm" variant="ghost" title="Unlink this chat" onClick={() => void run(() => integrationsApi.linkRemove(l.id), "Unlinked")}><Trash2 className="size-3.5" /></Button>
      </div>
      <PermsEditor perms={perms} onChange={(p) => save(p)} />
    </div>
  );
}

export function IntegrationsDialog() {
  const open = useIntegrations((s) => s.open);
  const status = useIntegrations((s) => s.status);
  const [adding, setAdding] = useState(false);
  useEffect(() => { if (open) { startIntegrations(); void loadProviders(); } }, [open]);
  if (!open) return null;
  return (
    <Dialog open onClose={closeIntegrations} width={760} title={<span className="flex items-center gap-2"><Bot className="size-4" />Agent bots</span>}>
      {!status ? <Spinner /> : (
        <div className="space-y-5">
          <p className="text-[12.5px] text-muted">
            Work with your agents from Telegram or Discord: get told when they finish, Allow / Deny what they ask, send new requests, and get their answers and files. The bots run from this computer while DevCat is open (no server). Notification-only webhooks (Discord, Slack) are in Mobile → Notifications.
          </p>
          <Section title="Bots" right={!adding && <Button size="sm" variant="ghost" onClick={() => setAdding(true)}><Bot className="size-3.5" />Add a bot</Button>}>
            {adding && <AddBot onDone={() => setAdding(false)} />}
            {status.bots.map((b) => <BotRow key={b.id} b={b} />)}
            {!status.bots.length && !adding && <div className="text-[12px] text-muted">No bots yet. <button className="text-accent hover:underline" onClick={() => setAdding(true)}>Add a Telegram or Discord bot</button>.</div>}
          </Section>
          <Section title="Link a chat">
            <LinkChat status={status} />
          </Section>
          <Section title={`Linked chats${status.links.length ? ` (${status.links.length})` : ""}`}>
            {status.links.map((l) => <LinkRow key={l.id} l={l} bot={status.bots.find((b) => b.id === l.botId)} />)}
            {!status.links.length && <div className="text-[12px] text-muted">None yet.</div>}
          </Section>
          <div className="text-[11px] text-faint">
            What a chat gets leaves this computer: the agent's answers, file names and, if allowed, files and query results. Only the people who linked a chat are listened to there. Dropping a database is never confirmed from a chat app, and ghost chats are never sent.
          </div>
        </div>
      )}
    </Dialog>
  );
}
