// The quick command bar under a terminal or agent chat (Xshell-style): click a button to type its
// text. Right-click a button to edit, move or delete it. On an SSH host it also opens SFTP.
import clsx from "clsx";
import { ArrowDownUp, ArrowLeft, ArrowRight, Pencil, Plus, Trash2, X, Zap } from "lucide-react";
import { useMemo, useState } from "react";
import { toast } from "sonner";
import { Button, Checkbox, Dialog, Field, IconButton, Input, Select, Textarea } from "../../components/ui";
import { api, errMsg } from "../../lib/api";
import { isRemote, sessionIdOf } from "../../lib/remotePath";
import { useConnections } from "../../store/connections";
import { useContextMenu } from "../connections/ContextMenu";
import { openSftp } from "../ssh/open";
import { newQuickCommand, quickInput, useQuickCommands, type QuickCommand } from "./quickCommands";

export function QuickBar({ ptyId, cwd }: { ptyId: string | null; cwd: string }) {
  const { commands, visible, remove, move, setVisible } = useQuickCommands();
  const sessionId = sessionIdOf(cwd);
  const shown = commands.filter((c) => c.scope === null || c.scope === sessionId);
  const [editing, setEditing] = useState<QuickCommand | null>(null);
  const menu = useContextMenu();
  if (!visible) return null;

  const run = (c: QuickCommand) => {
    if (!ptyId) return void toast.error("The terminal isn't running: press Enter in it to start it.");
    api.ptyWrite(ptyId, quickInput(c)).catch((e) => toast.error(errMsg(e)));
  };
  const itemMenu = (e: React.MouseEvent, c: QuickCommand) => {
    const i = commands.indexOf(c);
    menu.show(e, [
      { label: "Edit…", icon: <Pencil className="size-3.5" />, onClick: () => setEditing(c) },
      { label: "Move left", icon: <ArrowLeft className="size-3.5" />, disabled: i <= 0, onClick: () => move(c.id, -1) },
      { label: "Move right", icon: <ArrowRight className="size-3.5" />, disabled: i >= commands.length - 1, onClick: () => move(c.id, 1) },
      "separator",
      { label: "Delete", icon: <Trash2 className="size-3.5" />, danger: true, onClick: () => remove(c.id) },
    ]);
  };

  return (
    <div className="flex h-8 shrink-0 items-center gap-1 border-t border-line bg-panel px-2">
      <Zap className="size-3.5 shrink-0 text-faint" aria-label="Quick commands" />
      <div className="flex min-w-0 flex-1 items-center gap-1 overflow-x-auto">
        {shown.map((c) => (
          <button
            key={c.id}
            type="button"
            title={`${c.text}${c.enter ? " ⏎" : ""}${c.scope ? " (this session only)" : ""}\nRight-click to edit`}
            onClick={() => run(c)}
            onContextMenu={(e) => itemMenu(e, c)}
            className={clsx(
              "h-6 max-w-48 shrink-0 truncate rounded border border-line bg-raised px-2 text-[11px] hover:border-accent/60 hover:bg-hover",
              c.scope && "border-accent/40",
            )}
          >
            {c.label || c.text}
          </button>
        ))}
        <button
          type="button"
          onClick={() => setEditing(newQuickCommand())}
          className="flex h-6 shrink-0 items-center gap-1 rounded px-1.5 text-[11px] text-muted hover:bg-hover hover:text-fg"
          title="Add a button that types a command"
        >
          <Plus className="size-3.5" />{shown.length ? "" : "Quick command"}
        </button>
      </div>
      {isRemote(cwd) && (
        <Button size="sm" variant="ghost" title="Browse and copy files on this host" onClick={() => openSftp(cwd)}>
          <ArrowDownUp className="size-3.5" />SFTP
        </Button>
      )}
      <IconButton title="Hide the quick command bar (show it again from the tab's menu)" onClick={() => setVisible(false)}>
        <X className="size-3.5" />
      </IconButton>
      {menu.element}
      {editing && <QuickCommandDialog command={editing} sessionId={sessionId} onClose={() => setEditing(null)} />}
    </div>
  );
}

function QuickCommandDialog({ command, sessionId, onClose }: { command: QuickCommand; sessionId: string | null; onClose: () => void }) {
  const save = useQuickCommands((s) => s.save);
  const isNew = !useQuickCommands.getState().commands.some((c) => c.id === command.id);
  const [c, setC] = useState(command);
  const connections = useConnections((s) => s.connections);
  const sessions = useMemo(() => connections.filter((x) => x.kind === "ssh"), [connections]);
  const submit = () => {
    if (!c.text.trim()) return void toast.error("Enter the text to type");
    save({ ...c, label: c.label.trim() });
    onClose();
  };
  return (
    <Dialog
      open
      onClose={onClose}
      title={isNew ? "New quick command" : "Edit quick command"}
      width={480}
      footer={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button variant="primary" onClick={submit}>{isNew ? "Add" : "Save"}</Button>
        </>
      }
    >
      <form className="flex flex-col gap-3" onSubmit={(e) => { e.preventDefault(); submit(); }}>
        <Field label="Label">
          <Input autoFocus value={c.label} placeholder={c.text.split("\n")[0] || "Deploy logs"} onChange={(e) => setC({ ...c, label: e.target.value })} />
        </Field>
        <Field label="Text to type" hint="Each line break is typed as Enter.">
          <Textarea
            rows={3}
            className="font-mono text-xs!"
            value={c.text}
            placeholder="tail -f /var/log/app.log"
            onChange={(e) => setC({ ...c, text: e.target.value })}
            onKeyDown={(e) => { if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) { e.preventDefault(); submit(); } }}
          />
        </Field>
        <Checkbox label="Press Enter after the text" checked={c.enter} onChange={(enter) => setC({ ...c, enter })} />
        <Field label="Show in">
          <Select value={c.scope ?? ""} onChange={(e) => setC({ ...c, scope: e.target.value || null })}>
            <option value="">Every terminal and agent chat</option>
            {sessions.map((s) => <option key={s.id} value={s.id}>{s.name} only{s.id === sessionId ? " (this one)" : ""}</option>)}
          </Select>
        </Field>
        <button type="submit" hidden />
      </form>
    </Dialog>
  );
}
