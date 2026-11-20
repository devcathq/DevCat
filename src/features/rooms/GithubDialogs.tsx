// A room's GitHub repository (src-tauri/src/rooms_github.rs): its settings (this user's own; the
// token goes to the keychain), the confirmation of every post an agent wants to make there (the
// exact text, shown before anything is published), and this user's own "Create issue" from a
// pinned decision or a task.
import { open as pickFolder } from "@tauri-apps/plugin-dialog";
import { CircleDot, FolderGit2 } from "lucide-react";
import { useEffect, useState } from "react";
import { toast } from "sonner";
import { Badge, Button, Checkbox, Dialog, Field, Input, Textarea } from "../../components/ui";
import { errMsg } from "../../lib/api";
import { rooms, type GithubCheck, type GithubPrefs, type RoomView } from "./api";
import { useRooms } from "./store";

const NONE: GithubPrefs = { repo: "", site: "", read: false, write: false };
/** The room tab reads its view again (the settings show what was saved). */
const reload = (room: string) => useRooms.setState((s) => ({ ticks: { ...s.ticks, [room]: (s.ticks[room] ?? 0) + 1 } }));
const where = (repo: string, site: string) => `${repo}${site.trim() ? ` on ${site.trim().replace(/^https?:\/\//, "").split("/")[0]}` : ""}`;

/** Room settings → GitHub. */
export function GithubSettings({ room, view, open }: { room: string; view: RoomView; open: boolean }) {
  const g = view.prefs.github ?? NONE;
  const [repo, setRepo] = useState(g.repo);
  const [site, setSite] = useState(g.site);
  const [token, setToken] = useState("");
  const [busy, setBusy] = useState(false);
  const [check, setCheck] = useState<GithubCheck | null>(null);
  useEffect(() => {
    if (!open) return;
    setRepo(g.repo);
    setSite(g.site);
    setToken("");
    setCheck(null);
    if (g.repo && view.githubToken) void rooms.githubCheck(room).then(setCheck).catch(() => {});
  }, [open, room]);
  const changed = repo.trim() !== g.repo || site.trim() !== g.site || token.trim() !== "";
  const save = async () => {
    setBusy(true);
    try {
      const c = await rooms.githubSave(room, repo.trim(), site.trim(), token.trim() || undefined);
      setCheck(c);
      setToken("");
      reload(room);
      toast.success(repo.trim() ? `GitHub: ${repo.trim()}` : "GitHub taken off the room");
    } catch (e) {
      toast.error(errMsg(e));
    } finally {
      setBusy(false);
    }
  };
  const fromFolder = async () => {
    const dir = await pickFolder({ directory: true, multiple: false });
    if (typeof dir !== "string") return;
    try {
      const r = await rooms.githubFromFolder(dir);
      setRepo(r.repo);
      setSite(r.site);
    } catch (e) {
      toast.error(errMsg(e));
    }
  };
  const setSwitch = (p: Partial<GithubPrefs>) => void rooms.prefs(room, { ...view.prefs, github: { ...g, ...p } }).then(() => reload(room)).catch((e) => toast.error(errMsg(e)));
  const remove = () => {
    setRepo("");
    setSite("");
    void rooms.githubSave(room, "", "").then(() => { setCheck(null); reload(room); toast.success("GitHub taken off the room"); }).catch((e) => toast.error(errMsg(e)));
  };
  const ready = !!g.repo && view.githubToken;
  return (
    <div className="space-y-2">
      <div className="font-semibold">GitHub</div>
      <p className="text-[11px] text-muted">
        Your agents here (the room's agent and your coding agents) can read the repository's issues and discussions, and create issues from what the room decided. Only you set this, only for you: the token stays in your keychain, never in the room.
      </p>
      <div className="flex items-end gap-2">
        <Field label="Repository" className="flex-1"><Input value={repo} onChange={(e) => setRepo(e.target.value)} placeholder="owner/name" className="font-mono text-[12px]" /></Field>
        <Button title="Read it from a folder's origin remote" onClick={() => void fromFolder()}><FolderGit2 className="size-3.5" />From a folder…</Button>
      </div>
      <Field label="GitHub Enterprise Server (empty for github.com)">
        <Input value={site} onChange={(e) => setSite(e.target.value)} placeholder="github.acme.com" className="font-mono text-[12px]" />
      </Field>
      <Field label={view.githubToken ? "Token (saved; type a new one to replace it)" : "Token"} hint="A fine-grained token for this repository: Issues and Discussions, read (and write for Write). A new address needs its token again.">
        <Input type="password" value={token} onChange={(e) => setToken(e.target.value)} placeholder={view.githubToken ? "••••••••" : "github_pat_…"} className="font-mono text-[12px]" />
      </Field>
      <div className="flex items-center justify-end gap-2">
        {check && <span className="mr-auto text-[11px] text-muted">{check.repo}: {check.private ? "private" : "public"}{check.discussions ? ", discussions on" : ", no discussions"}{check.canWrite ? "" : ", the token can't create issues"}</span>}
        {g.repo && <Button disabled={busy} onClick={remove}>Remove</Button>}
        <Button variant="primary" loading={busy} disabled={!changed || (!!repo.trim() && !/^[\w.-]+\/[\w.-]+$/.test(repo.trim()))} onClick={() => void save()}>Save</Button>
      </div>
      <Checkbox label="Read: agents may read issues and discussions" checked={g.read} onChange={(v) => setSwitch({ read: v })} />
      {g.read && (
        <p className={check?.private ? "text-[11px] text-warn" : "text-[11px] text-muted"}>
          {check?.private ? "This repository is private: " : ""}what your agent reads there can show up in the thread, and everyone in the room sees it.
        </p>
      )}
      <Checkbox label="Write: agents may create issues and write in discussions (you confirm each one here first)" checked={g.write} onChange={(v) => setSwitch({ write: v })} />
      {(g.read || g.write) && !ready && <p className="text-[11px] text-warn">Save a repository and a token first: until then your agents get no GitHub tools.</p>}
    </div>
  );
}

const ACTION = { issue: "Create an issue", comment: "Comment in a discussion", discussion: "Start a discussion" } as const;

/** An agent wants to post on GitHub: show exactly what, and post only on this user's OK. */
export function GithubConfirmPrompt() {
  const pending = useRooms((s) => s.github);
  const p = pending[0];
  const [busy, setBusy] = useState(false);
  if (!p) return null;
  const answer = async (ok: boolean) => {
    setBusy(true);
    try {
      await rooms.githubAnswer(p.id, ok);
    } catch (e) {
      toast.error(errMsg(e));
    } finally {
      useRooms.setState((s) => ({ github: s.github.filter((x) => x.id !== p.id) }));
      setBusy(false);
    }
  };
  return (
    <Dialog
      open
      onClose={() => void answer(false)}
      title={<span className="inline-flex items-center gap-2"><CircleDot className="size-4" />{ACTION[p.action]} on GitHub?</span>}
      width={640}
      footer={
        <>
          <Button onClick={() => void answer(false)} disabled={busy}>Don't post</Button>
          <Button variant="primary" onClick={() => void answer(true)} disabled={busy}>Post on GitHub</Button>
        </>
      }
    >
      <div className="space-y-2.5 text-[13px]">
        <p><b>{p.by}</b>, in the room <b>{p.roomName}</b>, wants to post this in <span className="font-mono">{p.repo}</span> on {p.host}{p.target ? <>, {p.target}</> : null}.</p>
        {p.title != null && <div><div className="text-xs text-muted">Title</div><div className="selectable rounded-md border border-line bg-raised px-2.5 py-1.5 font-medium">{p.title}</div></div>}
        {p.labels.length > 0 && <div className="flex flex-wrap gap-1">{p.labels.map((l) => <Badge key={l}>{l}</Badge>)}</div>}
        <div>
          <div className="text-xs text-muted">{p.action === "comment" ? "Comment" : "Body"} (Markdown)</div>
          <pre className="selectable max-h-[45vh] overflow-auto whitespace-pre-wrap break-words rounded-md border border-line bg-raised px-2.5 py-1.5 font-mono text-[12px]">{p.body || "(empty)"}</pre>
        </div>
        {p.redacted && <p className="text-[12px] text-warn">Secret-looking text in it was hidden before you see it.</p>}
        <p className="text-[12px] text-muted">The text comes from the room, where other people write. Once posted it's public to everyone who can see the repository, and DevCat can't take it back. The link goes in the room's thread.</p>
      </div>
    </Dialog>
  );
}

/** An issue to start from: the text's first line as its title, the text (and notes) and where it's from as its body. */
export function issueDraft(text: string, from: string, notes?: string | null): { title: string; body: string } {
  const first = text.trim().split("\n")[0].replace(/^#+\s*/, "").trim();
  const title = first.length > 120 ? `${first.slice(0, 117)}…` : first;
  return { title, body: `${text.trim()}${notes?.trim() ? `\n\n${notes.trim()}` : ""}\n\n${from}` };
}

/** Whether this user can create issues from the room now (repository, token, Write on). */
export const canCreateIssues = (view: RoomView) => !view.summary.removed && !!view.prefs.github?.repo && !!view.prefs.github?.write && view.githubToken;

/** This user's own issue from the room (a pinned decision, a task): what they see here is what's posted. */
export function GithubIssueDialog({ room, view, draft, onClose }: { room: string; view: RoomView; draft: { title: string; body: string } | null; onClose: () => void }) {
  const [title, setTitle] = useState("");
  const [body, setBody] = useState("");
  const [labels, setLabels] = useState<string[]>([]);
  const [known, setKnown] = useState<string[]>([]);
  const [busy, setBusy] = useState(false);
  const g = view.prefs.github ?? NONE;
  useEffect(() => {
    if (!draft) return;
    setTitle(draft.title);
    setBody(draft.body);
    setLabels([]);
    void rooms.githubLabels(room).then(setKnown).catch(() => setKnown([]));
  }, [draft, room]);
  const create = async () => {
    setBusy(true);
    try {
      const url = await rooms.githubIssue(room, title, body, labels);
      toast.success("Issue created, and linked in the thread", { description: url });
      onClose();
    } catch (e) {
      toast.error(errMsg(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Dialog
      open={!!draft}
      onClose={onClose}
      title={`Create an issue in ${where(g.repo, g.site)}`}
      width={620}
      footer={<><Button onClick={onClose}>Cancel</Button><Button variant="primary" loading={busy} disabled={!title.trim()} onClick={() => void create()}>Create the issue</Button></>}
    >
      <div className="space-y-2.5">
        <Field label="Title"><Input value={title} onChange={(e) => setTitle(e.target.value)} /></Field>
        <Field label="Body (Markdown)"><Textarea rows={12} value={body} onChange={(e) => setBody(e.target.value)} /></Field>
        {known.length > 0 && (
          <div className="flex flex-wrap gap-1">
            {known.map((l) => (
              <button key={l} onClick={() => setLabels((x) => (x.includes(l) ? x.filter((y) => y !== l) : [...x, l]))}>
                <Badge tone={labels.includes(l) ? "accent" : "default"}>{l}</Badge>
              </button>
            ))}
          </div>
        )}
        <p className="text-[11px] text-muted">It's posted on GitHub exactly as written here, as you (the room's token). The link goes in the room's thread.</p>
      </div>
    </Dialog>
  );
}
