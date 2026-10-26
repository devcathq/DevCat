// "Other computers" in the Memory dialog: the same memory on your other computers (and servers
// running `devcat serve`), synced peer to peer (src-tauri/src/memory/sync). Pair with a one-time
// code shown on one computer and pasted on the other, which asks its user; then a list of the
// paired computers with their last sync, Sync now and Remove. Desktop only.
import { listen } from "@tauri-apps/api/event";
import clsx from "clsx";
import { ChevronRight, Copy, Monitor, RefreshCw, Trash2 } from "lucide-react";
import { useEffect, useState } from "react";
import { toast } from "sonner";
import { create } from "zustand";
import { Button, Dialog, ErrorBox, Input, Spinner } from "../../components/ui";
import { api, errMsg } from "../../lib/api";
import type { MemoryPairRequest, MemoryPeer, MemoryPeersStatus, MemorySyncActivity } from "../../lib/types";

const usePeers = create<{ status: MemoryPeersStatus | null; error: string | null; tick: number }>(() => ({ status: null, error: null, tick: 0 }));

const refresh = () => api.memoryPeers().then((status) => usePeers.setState({ status, error: null }), (e) => usePeers.setState({ error: errMsg(e) }));

let started = false;
/** Listen to memory sync events (once per window). */
export function startMemoryPeers() {
  if (started) return;
  started = true;
  void refresh();
  void listen<MemoryPeersStatus>("memory-sync", (e) => usePeers.setState({ status: e.payload, error: null }));
  void listen<MemoryPairRequest>("memory-sync-request", () => void refresh());
  void listen("memory-sync-resolved", () => void refresh());
  // memories arrived: the Memories view reloads
  void listen("memory-changed", () => usePeers.setState((s) => ({ tick: s.tick + 1 })));
}

/** Bumped whenever memories arrive from another computer. */
export const useMemoryChanged = () => usePeers((s) => s.tick);

function ago(ms: number) {
  const s = Math.max(0, Math.round((Date.now() - ms) / 1000));
  if (s < 60) return "just now";
  if (s < 3600) return `${Math.round(s / 60)} min ago`;
  if (s < 86400) return `${Math.round(s / 3600)} h ago`;
  return new Date(ms).toLocaleDateString();
}

const OS: Record<string, string> = { macos: "Mac", windows: "Windows", linux: "Linux" };

export function OtherComputers() {
  const status = usePeers((s) => s.status);
  const error = usePeers((s) => s.error);
  useEffect(() => {
    startMemoryPeers();
    void refresh();
  }, []);

  if (error) return <ErrorBox error={error} />;
  if (!status) return <div className="flex justify-center py-6"><Spinner /></div>;
  return (
    <div className="flex flex-col gap-3">
      <p className="text-xs text-muted">
        Share this memory with your other computers, or a server running <span className="font-mono">devcat serve</span>: what an agent learns on
        one, the agents on the others know. Computers connect to each other directly (or through the relay chosen in DevCat → Mobile), end-to-end
        encrypted, with no DevCat account or server. A project's memories go to the computers that have the project too (matched by its git remote).
      </p>
      {status.peers.length > 0 && (
        <div className="rounded-md border border-line">
          {status.peers.map((p) => <PeerRow key={p.id} peer={p} />)}
        </div>
      )}
      <Pairing ticket={status.ticket ?? null} first={status.peers.length === 0} />
      <Activity />
      <p className="text-[11px] text-faint">
        This computer is <b>{status.name}</b> to the others. When both computers changed the same memory, the newer change wins and the other is
        kept next to it as "… (conflict from …)". Secrets are removed before anything is shared.
      </p>
    </div>
  );
}

function PeerRow({ peer: p }: { peer: MemoryPeer }) {
  const [busy, setBusy] = useState(false);
  const [confirm, setConfirm] = useState(false);
  const syncNow = async () => {
    setBusy(true);
    try {
      const r = await api.memorySyncNow(p.id);
      const moved = r ? [r.received && `${r.received} received`, r.sent && `${r.sent} sent`, r.deleted && `${r.deleted} deleted`, r.conflicts && `${r.conflicts} conflict${r.conflicts === 1 ? "" : "s"} kept`].filter(Boolean).join(", ") : "";
      toast.success(`Synced with ${p.name}`, { description: moved || "Already up to date" });
    } catch (e) {
      toast.error(`Couldn't sync with ${p.name}`, { description: errMsg(e) });
    } finally {
      setBusy(false);
    }
  };
  const remove = async () => {
    try {
      usePeers.setState({ status: await api.memoryPeerRemove(p.id) });
    } catch (e) {
      toast.error(errMsg(e));
    }
  };
  return (
    <div className="flex items-center gap-2 border-b border-line/60 px-2.5 py-2 last:border-b-0">
      <Monitor className="size-4 shrink-0 text-muted" />
      <div className="flex min-w-0 flex-1 flex-col">
        <span className="truncate font-medium">{p.name} <span className="text-xs font-normal text-faint">{OS[p.os] ?? p.os}</span></span>
        <span className={clsx("truncate text-[11px]", p.lastError ? "text-warn" : "text-muted")}>
          {p.syncing ? "Syncing…" : p.lastError ? p.lastError : p.lastSync ? `Synced ${ago(p.lastSync)}${p.lastResult ? ` · last change: ${p.lastResult}` : ""}` : "Not synced yet"}
        </span>
      </div>
      {confirm ? (
        <>
          <Button size="sm" variant="danger" onClick={() => void remove()}>Remove</Button>
          <Button size="sm" onClick={() => setConfirm(false)}>Keep</Button>
        </>
      ) : (
        <>
          <Button size="sm" loading={busy || p.syncing} onClick={() => void syncNow()}><RefreshCw className="size-3" />Sync now</Button>
          <button title="Stop sharing memory with this computer (what it has stays there)" className="rounded p-1 text-muted hover:bg-hover hover:text-danger" onClick={() => setConfirm(true)}><Trash2 className="size-3" /></button>
        </>
      )}
    </div>
  );
}

function Pairing({ ticket, first }: { ticket: MemoryPeersStatus["ticket"]; first: boolean }) {
  const [starting, setStarting] = useState(false);
  const [pasting, setPasting] = useState(false);
  const [code, setCode] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const show = async () => {
    setError(null);
    setStarting(true);
    try {
      await api.memoryPairStart();
      await refresh();
    } catch (e) {
      setError(errMsg(e));
    } finally {
      setStarting(false);
    }
  };
  const join = async () => {
    setBusy(true);
    setError(null);
    try {
      const p = await api.memoryPairJoin(code.trim());
      toast.success(`Memory is shared with ${p.name}`, { description: "The first sync is running." });
      setCode("");
      setPasting(false);
      await refresh();
    } catch (e) {
      setError(errMsg(e));
    } finally {
      setBusy(false);
    }
  };
  const close = async () => {
    setPasting(false);
    setError(null);
    if (ticket) await api.memoryPairCancel().catch(() => {});
  };

  // the code disappears once it's used (or expires): back to the buttons
  const showing = !pasting && (!!ticket || starting);
  if (!showing && !pasting && !error) {
    return (
      <div className="flex flex-wrap items-center gap-2">
        <Button variant={first ? "primary" : "default"} onClick={() => void show()}>Pair a computer: show a code</Button>
        <Button onClick={() => setPasting(true)}>Paste a code from another computer</Button>
      </div>
    );
  }
  return (
    <div className="flex flex-col gap-2 rounded-md border border-line p-3">
      {error && <ErrorBox error={error} />}
      {showing && ticket && (
        <>
          <span className="text-xs">On the other computer open <b>Memory → Other computers → Paste a code</b> (or type <span className="font-mono">sync join &lt;code&gt;</span> in <span className="font-mono">devcat serve</span>) and paste:</span>
          <div className="flex gap-2">
            <Input readOnly className="flex-1 font-mono text-[11px]" value={ticket.ticket} onFocus={(e) => e.currentTarget.select()} />
            <Button onClick={() => void navigator.clipboard.writeText(ticket.ticket).then(() => toast.success("Copied"))}><Copy className="size-3.5" />Copy</Button>
          </div>
          <span className="text-[11px] text-faint">Works once, for {Math.max(1, Math.round((ticket.expiresAt - Date.now()) / 60000))} min. You'll be asked here before it's paired.</span>
        </>
      )}
      {showing && !ticket && <div className="flex justify-center py-2"><Spinner /></div>}
      {pasting && (
        <>
          <span className="text-xs">Paste the code shown on the other computer (<b>Memory → Other computers → Pair a computer</b>):</span>
          <Input autoFocus className="font-mono text-[11px]" placeholder="devcat://memory-sync?…" value={code} onChange={(e) => setCode(e.target.value)} />
          {busy && <span className="text-[11px] text-muted">Waiting for the other computer to allow it…</span>}
        </>
      )}
      <div className="flex justify-end gap-2">
        <Button onClick={() => void close()} disabled={busy}>{pasting ? "Cancel" : "Done"}</Button>
        {pasting && <Button variant="primary" loading={busy} disabled={!code.trim().startsWith("devcat://memory-sync")} onClick={() => void join()}>Pair</Button>}
      </div>
    </div>
  );
}

function Activity() {
  const [open, setOpen] = useState(false);
  const [items, setItems] = useState<MemorySyncActivity[] | null>(null);
  const status = usePeers((s) => s.status);
  useEffect(() => {
    if (open) api.memorySyncActivity(12).then(setItems, () => setItems([]));
  }, [open, status]);
  return (
    <div className="text-xs text-muted">
      <button className="flex items-center gap-1 hover:text-fg" onClick={() => setOpen((o) => !o)}>
        <ChevronRight className={clsx("size-3 transition-transform", open && "rotate-90")} />
        Activity
      </button>
      {open && (
        <div className="selectable mt-1 flex flex-col gap-0.5 pl-4">
          {items?.length === 0 && <span className="text-faint">Nothing yet</span>}
          {items?.map((e, i) => (
            <span key={i} className={clsx(!e.ok && "text-warn")}>
              <span className="text-faint">{new Date(e.at).toLocaleString()}</span> · {e.deviceName}: {e.action}{e.detail ? ` (${e.detail})` : ""}
            </span>
          ))}
        </div>
      )}
    </div>
  );
}

/** "Share memory with this computer?": shown in every window when a computer with this one's code asks. */
export function MemoryPairPrompt() {
  const req = usePeers((s) => s.status?.pending[0] ?? null);
  useEffect(() => startMemoryPeers(), []);
  if (!req) return null;
  const answer = async (allow: boolean) => {
    try {
      await api.memoryPairAnswer(req.requestId, allow);
      if (allow) toast.success(`Memory is shared with ${req.name}`);
    } catch (e) {
      toast.error(errMsg(e));
    }
    void refresh();
  };
  return (
    <Dialog
      open
      onClose={() => void answer(false)}
      title="Share your agents' memory with this computer?"
      width={460}
      footer={<><Button variant="ghost" onClick={() => void answer(false)}>Deny</Button><Button variant="primary" onClick={() => void answer(true)}>Allow</Button></>}
    >
      <div className="space-y-3 text-[13px]">
        <div className="flex items-center gap-3 rounded-md border border-line bg-raised px-3 py-2.5">
          <Monitor className="size-5 text-muted" />
          <div className="min-w-0">
            <div className="font-semibold">{req.name}</div>
            <div className="truncate text-[11px] text-muted">{OS[req.os] ?? req.os} · key <span className="font-mono">{req.peerId.slice(0, 6)}…{req.peerId.slice(-6)}</span></div>
          </div>
        </div>
        <p className="text-[12px] text-muted">
          It used this computer's memory sync code. Once allowed, both computers keep the same memories and handoffs, and can change them.
          Your connections, passwords and files aren't shared.
        </p>
        <p className="text-[11px] text-faint">Didn't paste a code on another computer just now? Click Deny.</p>
      </div>
    </Dialog>
  );
}
