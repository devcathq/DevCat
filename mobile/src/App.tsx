// DevCat Mobile: screen switch over the navigation stack, startup (paired computers, events),
// reconnecting when the app comes back to the foreground, and local notifications.
import { useEffect } from "react";
import { phone, startEvents } from "./lib/rpc";
import { loadMachines, nav, selectMachine, setLocalNotifier, startStateEvents, useApp } from "./lib/store";
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

/** App: a devcat://pair link it was opened with (from the web page's "Open in DevCat app"). */
async function watchDeepLinks() {
  if (IS_WEB) return;
  try {
    const dl = await import("@tauri-apps/plugin-deep-link");
    const go = (urls: string[] | null | undefined) => {
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
  const webLink = takeWebLink();
  if (IS_WEB) carryPairing();
  const [info, machines] = await Promise.all([phone.info(), loadMachines()]);
  useApp.setState({ info });
  if (webLink) {
    const l = parseLink(webLink);
    if (l && pairedHostId() === l.id) {
      // Already paired with this computer in this browser: just open it.
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
