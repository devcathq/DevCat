// Dev only (`pnpm mobile:mock`): the mobile UI in a normal browser with made-up data and no
// computer, for designing screens and taking screenshots. Stands in for @tauri-apps/* like
// bridge.ts; commands the desktop demo knows (connections, VPNs, DevCat Agent chats) come from
// src/dev/demo.ts, the phone's own ones (agents, terminals, sessions, files) from here.
// `?screen=<route>` opens a screen straight away (see App.tsx `mockStart`). Nothing here is real.
import { demoInvoke } from "../../../src/dev/demo";

const MACHINE = "mock-macbook";
const NOW = Date.now();
const HOME = "/Users/ava";
const P = (n: string) => `${HOME}/Projects/${n}`;

type Handler = (e: { event: string; payload: unknown }) => void;
const handlers = new Map<string, Set<Handler>>();
function emit(event: string, payload: unknown) {
  handlers.get(event)?.forEach((h) => h({ event, payload }));
}
const fromComputer = (event: string, payload: unknown) => emit("m-event", { id: MACHINE, event, payload });

const terminals = [
  { id: "T1", cwd: P("storefront"), program: "claude", agent: true, origin: "desktop", startedAt: NOW - 40 * 60_000, cols: 60, rows: 30 },
  { id: "T2", cwd: P("api-gateway"), program: "claude", agent: true, origin: "phone:Pixel 9", startedAt: NOW - 12 * 60_000, cols: 60, rows: 30 },
  { id: "T3", cwd: P("ui"), program: "claude", agent: true, origin: "desktop", startedAt: NOW - 3 * 3_600_000, cols: 60, rows: 30 },
  { id: "T4", cwd: P("api-gateway"), program: "zsh", agent: false, origin: "desktop", startedAt: NOW - 5 * 3_600_000, cols: 60, rows: 30 },
];
const statuses = [
  { ptyId: "T1", cwd: P("storefront"), sessionId: "s1", status: "waiting", since: NOW - 60_000, message: "Claude needs your permission to use Bash" },
  { ptyId: "T2", cwd: P("api-gateway"), status: "working", since: NOW - 2 * 60_000 - 14_000 },
  { ptyId: "T3", cwd: P("ui"), status: "idle", since: NOW - 7 * 60_000, lastTurnMs: 192_000 },
];
const chats = [
  { sessionId: "demo-chat", cwd: P("storefront"), title: "Refunds in the last 30 days", providerId: "devcat-agent", providerName: "DevCat Agent", origin: "desktop", busy: false, waiting: null, since: NOW - 2 * 60_000 },
];

const HISTORY = Array.from({ length: 80 }, (_, i) => `  earlier output line ${i + 1}\r\n`).join("");
const SCREEN = [
  HISTORY,
  "\x1b[1m✻ Welcome to Claude Code!\x1b[0m\r\n\r\n",
  "\x1b[2m> \x1b[0mRun the checkout tests and fix what fails\r\n\r\n",
  "⏺ I'll run the test suite first.\r\n\r\n",
  "⏺ \x1b[1mBash\x1b[0m(npm test -- checkout)\r\n",
  "\r\n╭──────────────────────────────────────────╮\r\n",
  "│ Bash command                              │\r\n│   npm test -- checkout                    │\r\n│                                           │\r\n",
  "│ Do you want to proceed?                   │\r\n",
  "│ ❯ 1. Yes                                  │\r\n",
  "│   2. Yes, and don't ask again for npm     │\r\n",
  "│   3. No, and tell Claude what to do       │\r\n",
  "╰──────────────────────────────────────────╯\r\n",
].join("");

const projects = ["storefront", "api-gateway", "ui", "payments-worker"].map((n, i) => ({ cwd: P(n), sessionCount: 14 - i * 3, lastActive: new Date(NOW - i * 3_600_000).toISOString() }));

function files(dir: string) {
  const f = (name: string, isDir: boolean, size = 0, git: string | null = null) => ({ name, path: `${dir}/${name}`, isDir, size, modified: new Date(NOW - 3_600_000).toISOString(), git, ignored: false });
  return [f("src", true), f("tests", true), f("README.md", false, 2_310), f("package.json", false, 1_204, "modified"), f("tsconfig.json", false, 512)];
}

function phone(cmd: string, args: Record<string, unknown>): unknown {
  switch (cmd) {
    case "agents": return { terminals, agents: statuses, chats };
    case "host_info": return { id: MACHINE, name: "Ava's MacBook Pro", os: "macos", appVersion: "0.26.0", addrs: ["192.168.1.20:47199"] };
    case "keep_awake_status": return { on: true, until: null };
    case "keep_awake_set": return null;
    case "claude_projects": return projects;
    case "claude_sessions": return [
      { id: "s1", title: "Fix the failing checkout tests", messageCount: 38, updatedAt: new Date(NOW - 40 * 60_000).toISOString(), gitBranch: "fix/checkout", costUsd: 1.42 },
      { id: "s2", title: "Add Apple Pay to the payment sheet", messageCount: 112, updatedAt: new Date(NOW - 26 * 3_600_000).toISOString(), gitBranch: "feat/apple-pay", costUsd: 4.1 },
    ];
    case "claude_transcript": return [
      ...Array.from({ length: 6 }, (_, i) => [
        { type: "user", message: { role: "user", content: `Step ${i + 1}: look at the checkout flow and tell me what's slow` } },
        { type: "assistant", message: { role: "assistant", content: [{ type: "text", text: `Here's what I found in step ${i + 1}:\n\n- The **cart total** is computed twice per render\n- \`getShipping()\` calls the API without a cache\n- Images load at full size on mobile` }, { type: "tool_use", name: "Read", input: { file_path: "src/checkout/Cart.tsx" } }] } },
      ]).flat(),
      { type: "user", message: { role: "user", content: "Run the checkout tests and fix what fails" } },
      { type: "assistant", message: { role: "assistant", content: [{ type: "text", text: "I'll run the test suite first." }, { type: "tool_use", name: "Bash", input: { command: "npm test -- checkout" } }] } },
    ];
    case "home_dir": return HOME;
    case "fs_list": return files(String(args.dir ?? HOME));
    case "pty_attach": {
      const info = terminals.find((t) => t.id === args.id) ?? terminals[0];
      return { info, data: btoa(unescape(encodeURIComponent(info.program === "zsh" ? "ava@macbook api-gateway % " : SCREEN))) };
    }
    case "pty_detach": case "pty_write": case "pty_resize": return null;
    case "pty_open": {
      const t = { id: `T${terminals.length + 1}`, cwd: String(args.cwd ?? HOME), program: String(args.program ?? "zsh"), agent: args.program === "claude", origin: "phone:Mock phone", startedAt: Date.now(), cols: 60, rows: 30 };
      terminals.push(t);
      setTimeout(() => fromComputer("remote-pty-opened", t), 0);
      return t;
    }
    case "pty_close": {
      const i = terminals.findIndex((t) => t.id === args.id);
      if (i >= 0) terminals.splice(i, 1);
      return null;
    }
    case "notify_settings": return { onDone: true, onInput: true, minSeconds: 30, onlyWhenAway: true };
    case "notify_test": return null;
    case "notify_recent": return { boot: "mock", notes: [] };
    case "audit": return [{ at: NOW - 120_000, device: "", deviceName: "Pixel 9", action: "pty_open", detail: "~/Projects/api-gateway (claude)", ok: true }];
    case "agent_decide": case "agent_answer": case "agent_approve": case "agent_models": return cmd === "agent_models" ? [] : null;
    case "list_projects": return [
      { id: "p-shop", name: "Acme Shop", color: "#22c55e", order: 0, folders: [P("storefront"), P("ui")] },
      { id: "p-pay", name: "Payments", color: "#3b82f6", order: 1, folders: [P("api-gateway"), P("payments-worker")] },
      { id: "p-new", name: "interopera", color: "#8b5cf6", order: 2 },
      { id: "p-ui", name: "UI", color: "#eab308", order: 3 },
    ];
    case "agent_close": return null;
    case "agent_open": {
      if (args.ghost) return { ...(demoInvoke("agent_open", { ghostWindow: "mock" }, fromComputer) as object), sessionId: "mock-ghost" };
      // `mock-long`: the demo chat four times over, longer than the screen
      if (args.sessionId === "mock-long") {
        const r = demoInvoke("agent_open", args, fromComputer) as { events: { event: string }[] } & Record<string, unknown>;
        const body = r.events.filter((e) => e.event !== "meta" && e.event !== "title");
        return { ...r, sessionId: "mock-long", events: [r.events[0], ...body, ...body, ...body, ...body] };
      }
      // `mock-wait`: the demo chat stopped at the database dump, waiting for an OK
      if (args.sessionId !== "mock-wait") return undefined;
      const r = demoInvoke("agent_open", args, fromComputer) as { events: { event: string }[] } & Record<string, unknown>;
      const cut = r.events.findIndex((e) => e.event === "approval");
      return {
        ...r, sessionId: "mock-wait", busy: true, events: r.events.slice(0, cut),
        live: [{ type: "approval", request: { toolUseId: "t3", tool: "db_dump", kind: "exec", summary: "Dump shop-db / shop to ~/Downloads", input: { connection: "shop-db", database: "shop" } } }],
      };
    }
    default: return undefined;
  }
}

function call(cmd: string, args: Record<string, unknown>): unknown {
  const mine = phone(cmd, args);
  if (mine !== undefined) return mine;
  const demo = demoInvoke(cmd, args, fromComputer);
  if (demo !== undefined) return demo;
  throw `Not in the mock: ${cmd}`;
}

const machine = { id: MACHINE, name: "Ava's MacBook Pro", addrs: ["192.168.1.20:47199"], os: "macos", appVersion: "0.26.0", access: "full", addedAt: NOW - 86_400_000, lastSeen: NOW, manualAddrs: [], relay: "public" };

export async function invoke<T>(cmd: string, args: Record<string, unknown> = {}): Promise<T> {
  await new Promise((r) => setTimeout(r, 60));
  switch (cmd) {
    case "m_info": return { deviceId: "mock-phone", deviceName: "Mock phone", platform: "android", version: "mock", online: [MACHINE] } as T;
    case "m_machines": return [machine] as T;
    case "m_connect": setTimeout(() => emit("m-conn", { id: MACHINE, state: "online" }), 0); return { host: phone("host_info", {}), device: { access: "full" } } as T;
    case "m_call": return call(String(args.cmd), (args.args as Record<string, unknown>) ?? {}) as T;
    case "m_visible": case "m_disconnect": case "m_network_changed": case "m_set_name": return null as T;
    case "m_background": case "m_background_set": return { supported: true, on: !!args.on, batteryUnrestricted: false } as T;
    default: throw `Not in the mock: ${cmd}`;
  }
}

export type UnlistenFn = () => void;
export async function listen(event: string, h: Handler): Promise<UnlistenFn> {
  const set = handlers.get(event) ?? new Set<Handler>();
  set.add(h);
  handlers.set(event, set);
  if (event === "m-conn") setTimeout(() => h({ event, payload: { id: MACHINE, state: "online" } }), 0);
  return () => { set.delete(h); };
}

// plugin-notification
export async function isPermissionGranted() { return true; }
export async function requestPermission() { return "granted"; }
export function sendNotification(n: unknown) { console.log("[mock] notification", n); }

// plugin-barcode-scanner
export enum Format { QRCode = "QR_CODE" }
export async function checkPermissions() { return "granted"; }
export async function requestPermissions() { return "granted"; }
export async function scan(): Promise<{ content: string }> { throw new Error("No camera in the mock"); }

// plugin-opener
export async function openUrl(u: string) { console.log("[mock] openUrl", u); }

// plugin-deep-link
export async function getCurrent(): Promise<string[] | null> { return null; }
export async function onOpenUrl(): Promise<() => void> { return () => {}; }
