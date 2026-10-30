// DevCat Mobile: screen switch over the navigation stack, startup (paired computers, events),
// reconnecting when the app comes back to the foreground, and local notifications.
import { useEffect } from "react";
import { phone, startEvents } from "./lib/rpc";
import { loadMachines, nav, selectMachine, setLocalNotifier, startStateEvents, toast, useApp } from "./lib/store";
import type { Route } from "./lib/types";
import { Agents, NewChat, Sessions, Terminal, Transcript } from "./screens/agents";
import { AgentChat } from "./screens/agentchat";
import { Browse, Data, Query, RedisKey, Row, Rows } from "./screens/data";
import { Activity, Help, MachineSettings, More, Notify } from "./screens/more";
import { FileView, Files, FilesHome } from "./screens/files";
import { Cluster, NetLog, Network, Pod } from "./screens/ops";
import { IS_WEB, isPairLink } from "./lib/env";
import { Computers, Pairing, Paste, Scan, WebLanding, Welcome, prefersApp } from "./screens/setup";
import { adoptCarried, appLink, carryPairing, pairedHostId, parseLink } from "./web/client";
import { Loading, Toasts } from "./ui";

function screen(r: Route) {
  switch (r.s) {
    case "loading": return <Loading text="Starting…" />;
    case "welcome": return <Welcome />;
    case "weblanding": return <WebLanding link={r.link} />;
    case "scan": return <Scan />;
    case "paste": return <Paste link={r.link} />;
    case "pairing": return <Pairing link={r.link} addr={r.addr} />;
    case "computers": return <Computers />;
    case "agents": return <Agents />;
    case "terminal": return <Terminal id={r.id} />;
    case "newchat": return <NewChat cwd={r.cwd} />;
    case "sessions": return <Sessions cwd={r.cwd} />;
    case "transcript": return <Transcript cwd={r.cwd} sessionId={r.sessionId} title={r.title} />;
    case "agentchat": return <AgentChat cwd={r.cwd} providerId={r.providerId} sessionId={r.sessionId} first={r.first} />;
    case "data": return <Data />;
    case "browse": return <Browse key={r.path.join("\u0000")} connId={r.connId} path={r.path} title={r.title} />;
    case "rows": return <Rows connId={r.connId} table={r.table} />;
    case "row": return <Row connId={r.connId} table={r.table} row={r.row} pk={r.pk} />;
    case "query": return <Query connId={r.connId} database={r.database} schema={r.schema} sql={r.sql} />;
    case "rediskey": return <RedisKey connId={r.connId} database={r.database} keyName={r.key} />;
    case "network": return <Network />;
    case "netlog": return <NetLog id={r.id} name={r.name} />;
    case "cluster": return <Cluster />;
    case "pod": return <Pod connId={r.connId} namespace={r.namespace} name={r.name} />;
    case "more": return <More />;
    case "notify": return <Notify />;
    case "help": return <Help />;
    case "activity": return <Activity />;
    case "machine": return <MachineSettings />;
    case "fileshome": return <FilesHome />;
    case "files": return <Files key={r.dir} dir={r.dir} root={r.root} />;
    case "file": return <FileView key={r.path} path={r.path} root={r.root} />;
  }
}

/** Browser: a pairing link the QR code opened this page with (taken out of the address bar). */
function takeWebLink(): string | null {
  if (!IS_WEB) return null;
  const link = location.href;
  if (!isPairLink(link)) return null;
  history.replaceState(null, "", location.pathname);
  return link;
}

/** A link that opens a DevCat Agent chat (sent by the computer to a linked chat app, see
 *  integrations in src-tauri): `…#chat=<session>&host=<computer>&cwd=…&provider=…` in a browser,
 *  `devcat://chat?…` in the app. It pairs nothing: it opens the chat only on a computer this phone
 *  is already paired with. */
interface ChatLink { session: string; host: string; cwd: string; provider: string }

function parseChatLink(link: string): ChatLink | null {
  const i = link.search(/[#?]chat=/);
  if (i < 0) return null;
  const q = new URLSearchParams(link.slice(i + 1));
  const [session, host, cwd, provider] = ["chat", "host", "cwd", "provider"].map((k) => q.get(k) ?? "");
  return session && host && cwd && provider ? { session, host, cwd, provider } : null;
}

function chatLinkUrl(c: ChatLink): string {
  const q = new URLSearchParams({ chat: c.session, host: c.host, cwd: c.cwd, provider: c.provider });
  return `devcat://chat?${q.toString()}`;
}

/** Browser: a chat link this page was opened with (taken out of the address bar). */
function takeWebChatLink(): ChatLink | null {
  if (!IS_WEB) return null;
  const c = parseChatLink(location.href);
  if (c) history.replaceState(null, "", location.pathname);
  return c;
}

/** Open a linked chat, if this phone is paired with its computer. */
async function openChatLink(c: ChatLink): Promise<boolean> {
  const machines = useApp.getState().machines.length ? useApp.getState().machines : await loadMachines();
  if (!machines.some((m) => m.id === c.host)) {
    toast("That chat is on a computer this phone isn't paired with. Pair it in DevCat → Mobile pairing first.", "err");
    return false;
  }
  selectMachine(c.host);
  nav.reset([{ s: "computers" }, { s: "agents" }, { s: "agentchat", cwd: c.cwd, providerId: c.provider, sessionId: c.session }]);
  return true;
}

/** App: a devcat://pair link it was opened with (from the web page's "Open in DevCat app"), or a
 *  devcat://chat link. */
async function watchDeepLinks() {
  if (IS_WEB) return;
  try {
    const dl = await import("@tauri-apps/plugin-deep-link");
    const go = (urls: string[] | null | undefined) => {
      const chat = urls?.map((u) => (u.startsWith("devcat://chat") ? parseChatLink(u) : null)).find(Boolean);
      if (chat) return void openChatLink(chat);
      const u = urls?.find(isPairLink);
      if (u) nav.push({ s: "pairing", link: u });
    };
    go(await dl.getCurrent());
    await dl.onOpenUrl(go);
  } catch { /* not available (browser bridge, desktop dev) */ }
}

async function boot() {
  await startEvents();
  startStateEvents();
  if (IS_WEB) adoptCarried();
  const chatLink = takeWebChatLink();
  const webLink = takeWebLink();
  if (IS_WEB) carryPairing();
  const [info, machines] = await Promise.all([phone.info(), loadMachines()]);
  useApp.setState({ info });
  if (webLink) {
    const l = parseLink(webLink);
    // Already paired with this computer in this browser: just open it, unless the computer no
    // longer knows that pairing (then it's forgotten, and this new code pairs again).
    if (l && pairedHostId() === l.id && (await phone.connect(l.id).then(() => true, () => pairedHostId() === l.id))) {
      // open it below
    } else if (l && prefersApp()) {
      location.href = appLink(l);
      nav.reset([{ s: "weblanding", link: webLink }]);
      return;
    } else {
      nav.reset([{ s: "weblanding", link: webLink }]);
      return;
    }
  }
  void watchDeepLinks();
  if (chatLink) {
    // paired in this browser: open it here; else the app may be (it has its own pairings)
    if (machines.some((m) => m.id === chatLink.host)) {
      await openChatLink(chatLink);
      return;
    }
    if (prefersApp()) {
      location.href = chatLinkUrl(chatLink);
    } else {
      toast("That chat is on a computer this browser isn't paired with. Open the link on a phone paired with it, or pair this one in DevCat → Mobile pairing.", "err");
    }
  }
  if (machines.length === 0) {
    nav.reset([{ s: "welcome" }]);
    return;
  }
  const last = useApp.getState().current;
  const pick = machines.find((m) => m.id === last) ?? machines[0];
  selectMachine(pick.id);
  nav.reset([{ s: "computers" }, { s: "agents" }]);
}

export function App() {
  const stack = useApp((s) => s.stack);
  useEffect(() => {
    void boot();
    void import("@tauri-apps/plugin-notification").then(async (n) => {
      setLocalNotifier((title, body) => {
        void n.isPermissionGranted().then((ok) => (ok ? n.sendNotification({ title, body }) : undefined));
      });
      if (!(await n.isPermissionGranted())) await n.requestPermission();
    }).catch(() => {});
    // Back from the background or a network change: start fresh connections.
    let hiddenAt = 0;
    const onVis = () => {
      if (document.hidden) { hiddenAt = Date.now(); return; }
      if (Date.now() - hiddenAt > 20_000) void phone.networkChanged();
    };
    const onOnline = () => void phone.networkChanged();
    document.addEventListener("visibilitychange", onVis);
    window.addEventListener("online", onOnline);
    return () => { document.removeEventListener("visibilitychange", onVis); window.removeEventListener("online", onOnline); };
  }, []);
  const r = stack[stack.length - 1];
  return (
    <>
      <div className="h-full" key={stack.length + r.s}>{screen(r)}</div>
      <Toasts />
    </>
  );
}
