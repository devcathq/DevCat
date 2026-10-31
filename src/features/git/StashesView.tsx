// Stashes: list (message, date) + the selected stash's files and diffs (a stash is a commit).
import clsx from "clsx";
import { Archive, ArchiveRestore, Copy, PackageOpen, RefreshCw, Trash2 } from "lucide-react";
import { useCallback, useEffect, useState, type MouseEvent } from "react";
import { Group, Panel, Separator } from "react-resizable-panels";
import { Button, Empty, ErrorBox, IconButton, Spinner, Spin } from "../../components/ui";
import { api, errMsg } from "../../lib/api";
import type { GitStash } from "../../lib/types";
import { useContextMenu, type MenuItem } from "../connections/ContextMenu";
import { copyText } from "../files/format";
import { CommitDetail } from "./CommitDetail";
import type { Dialogs } from "./dialogs";
import { gitAction, useRepo } from "./store";
import { fmtUnix, relUnix } from "./util";

/** Ask for a message (+ untracked) and stash the working-tree changes. */
export async function stashChanges(repo: string, dlg: Dialogs) {
  const r = await dlg.prompt({
    title: "Stash changes",
    label: "Message (optional)",
    allowEmpty: true,
    placeholder: "WIP",
    checkbox: { label: "Include untracked files", initial: true },
    ok: "Stash",
  });
  if (r) await gitAction(repo, "Stash", () => api.gitStashSave(repo, r.value || null, r.checked), { done: "Changes stashed" });
}

export async function applyStash(repo: string, s: GitStash, pop: boolean) {
  await gitAction(repo, `${pop ? "Pop" : "Apply"} ${s.name}`, () => api.gitStashApply(repo, s.index, pop));
}

export async function dropStash(repo: string, dlg: Dialogs, s: GitStash) {
  const ok = await dlg.confirm({
    title: "Drop stash?",
    body: <>Delete <code className="font-mono">{s.name}</code> “{s.message}”? Its changes are lost.</>,
    ok: "Drop",
    danger: true,
  });
  if (ok) await gitAction(repo, `Drop ${s.name}`, () => api.gitStashDrop(repo, s.index), { done: `Dropped ${s.name}` });
}

export function StashesView({ repo, dlg, active }: { repo: string; dlg: Dialogs; active: boolean }) {
  const { tick } = useRepo(repo);
  const [stashes, setStashes] = useState<GitStash[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [selected, setSelected] = useState<string | null>(null);
  const menu = useContextMenu();

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const list = await api.gitStashList(repo);
      setStashes(list);
      setError(null);
      setSelected((cur) => (cur && list.some((s) => s.sha === cur) ? cur : list[0]?.sha ?? null));
    } catch (e) {
      setError(errMsg(e));
    } finally {
      setLoading(false);
    }
  }, [repo]);

  // Every repo refresh (after stash / apply / drop, or polling) re-reads the list while visible.
  useEffect(() => { if (active) void load(); }, [active, tick, load]);

  const sel = stashes?.find((s) => s.sha === selected) ?? null;

  const showMenu = (e: MouseEvent, s: GitStash) => {
    setSelected(s.sha);
    const items: MenuItem[] = [
      { label: "Apply", icon: <ArchiveRestore className="size-3.5" />, onClick: () => void applyStash(repo, s, false) },
      { label: "Pop (apply and drop)", icon: <PackageOpen className="size-3.5" />, onClick: () => void applyStash(repo, s, true) },
      "separator",
      { label: "Copy name", icon: <Copy className="size-3.5" />, onClick: () => void copyText(s.name) },
      { label: "Drop…", icon: <Trash2 className="size-3.5" />, danger: true, onClick: () => void dropStash(repo, dlg, s) },
    ];
    menu.show(e, items);
  };

  if (error && !stashes) return <ErrorBox error={error} />;
  if (!stashes) return <div className="flex h-full items-center justify-center"><Spinner /></div>;

  return (
    <>
      <Group orientation="horizontal" className="h-full">
        <Panel defaultSize="28%" minSize="16%">
          <div className="flex h-full min-h-0 flex-col bg-panel">
            <div className="flex h-9 shrink-0 items-center gap-1 border-b border-line px-2">
              <Button size="sm" onClick={() => void stashChanges(repo, dlg)}><Archive className="size-3.5" />Stash changes…</Button>
              <div className="flex-1" />
              <IconButton title="Refresh" onClick={() => void load()}><Spin icon={RefreshCw} spinning={loading} className="size-3.5" /></IconButton>
            </div>
            <div className="min-h-0 flex-1 overflow-y-auto py-1">
              {stashes.length === 0 && <div className="px-3 py-6 text-center text-xs text-muted">No stashes</div>}
              {stashes.map((s) => (
                <div
                  key={s.sha}
                  className={clsx("cursor-default px-3 py-1.5", s.sha === selected ? "bg-accent/15" : "hover:bg-hover/60")}
                  onClick={() => setSelected(s.sha)}
                  onContextMenu={(e) => showMenu(e, s)}
                >
                  <div className="truncate text-[12px] text-fg/90" title={s.message}>{s.message}</div>
                  <div className="flex items-center gap-2 text-[11px] text-faint">
                    <span className="font-mono">{s.name}</span>
                    <span className="flex-1" />
                    <span title={fmtUnix(s.time)}>{relUnix(s.time)}</span>
                  </div>
                </div>
              ))}
            </div>
          </div>
        </Panel>
        <Separator className="w-px bg-line hover:bg-accent data-[separator=active]:bg-accent" />
        <Panel minSize="40%">
          {sel ? (
            <CommitDetail
              key={sel.sha}
              repo={repo}
              sha={sel.sha}
              header={
                <div className="mb-2 flex items-center gap-1.5">
                  <span className="font-mono text-[11px] text-muted">{sel.name}</span>
                  <div className="flex-1" />
                  <Button size="sm" onClick={() => void applyStash(repo, sel, false)}><ArchiveRestore className="size-3.5" />Apply</Button>
                  <Button size="sm" onClick={() => void applyStash(repo, sel, true)}><PackageOpen className="size-3.5" />Pop</Button>
                  <Button size="sm" variant="danger" onClick={() => void dropStash(repo, dlg, sel)}><Trash2 className="size-3.5" />Drop</Button>
                </div>
              }
            />
          ) : (
            <Empty icon={<Archive className="size-8" />} title="No stash selected">Stash your working-tree changes to set them aside and bring them back later.</Empty>
          )}
        </Panel>
      </Group>
      {menu.element}
    </>
  );
}
